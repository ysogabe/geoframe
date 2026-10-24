//! 同期サイクル（現行 `catalog.py` の `worker`／`cycle`。棚卸し B1）の骨組み。
//!
//! # 構成
//!
//! サイクルは**段（[`Stage`]）を順に実行**する。段の中身は T2.3〜T2.6 で入れる（T2.3 で
//! `index_local`・`local_derivatives` が入った。残りは何もしない [`Placeholder`]）。順序は現行の
//! `cycle()` と同じ:
//!
//! `index_local` → `index_drive` → `infer` → `enrich` → `local_derivatives` → `fetch_derivatives` → `quality`
//!
//! - **段ごとに失敗を隔離する**。段が `Err` を返しても、パニックしても、残りの段と次の周期へ進む
//!   （現行は段ごとの `except` で握る）。ログには段の名前と失敗の種類だけを出す（例外文字列には
//!   ファイル名・座標が入りうる。[`StageError`]）。
//! - 起動直後に 1 回、以降 `GEOFRAME_DRIVE_POLL_MIN` 分ごと（[`spawn_worker`]）。専用の std スレッドで
//!   動き、HTTP サーバーの起動を待たせない。
//! - **件数だけをログに出す**（段ごとの [`Counts`] と、サイクル末尾の `表示可能／取得待ち／全`）。
//!
//! # `--dry-run`
//!
//! [`Mode::DryRun`] では**書き込み接続を段に渡さない**（[`StageCtx::writer`] が `None`）。DB への書き込みは
//! 構造的に起きない。ファイルへの書き込み・ネットワークの副作用（原本のダウンロード、derivative の
//! 生成、キャッシュの更新）は、各段が [`StageCtx::is_dry_run`] を見て避ける（`Cache` は凍結される）。
//! 段は dry-run では「何をするか」を件数にして返す。P2 の受入（取り込み済みの本番コピーに対して
//! 変更がほぼ 0 であること）は、これで確かめる。
//!
//! # 段を書くときの約束
//!
//! - DB は [`StageCtx::read`]（読み取り専用の接続）で読み、書くのは [`StageCtx::writer`] 経由だけ。
//! - 時刻を扱うときは naive な日時（`NaiveDateTime`）で差分を取る（inventory.md §6.1）。
//! - ログに写真の内容・ファイル名・座標・地名・Drive の ID・例外文字列を出さない（inventory.md §6.3）。
//!   出してよいのは件数・状態名・写真 ID・HTTP ステータス。
//! - 長い段（ダウンロードの繰り返しなど）は、項目の合間に [`StageCtx::stop`] を見て早めに戻る。

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::cache::Cache;
use crate::config::Config;
use crate::stop::StopFlag;
use crate::writer::{WriteError, Writer};

/// 段の名前（サイクルの順）。
pub const STAGE_NAMES: [&str; 7] = [
    "index_local",
    "index_drive",
    "infer",
    "enrich",
    "local_derivatives",
    "fetch_derivatives",
    "quality",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 実際に書く。
    Apply,
    /// 書かずに、何をするかを件数だけ出す。
    DryRun,
}

/// 段に渡すもの。
pub struct StageCtx<'a> {
    pub mode: Mode,
    pub config: &'a Config,
    /// 読み取り専用の接続（サイクルの間だけ有効）。
    pub read: &'a Connection,
    /// 書き込み接続。`Mode::DryRun` では `None`。
    pub writer: Option<&'a Writer>,
    pub cache: &'a Cache,
    pub http: &'a ureq::Agent,
    pub stop: &'a StopFlag,
}

impl StageCtx<'_> {
    pub fn is_dry_run(&self) -> bool {
        self.mode == Mode::DryRun
    }

    /// 書き込み接続。dry-run では使えない（段が書こうとしたら、この `Err` でそれと分かる）。
    pub fn writer(&self) -> Result<&Writer, StageError> {
        self.writer
            .ok_or_else(|| StageError::fixed("dry-run では書き込めません"))
    }
}

/// 段が返す件数（`(名前, 件数)`）。名前は `&'static str` なので、値（ファイル名など）を入れられない。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts(pub Vec<(&'static str, u64)>);

impl Counts {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (name, n)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{name}={n}")?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageOutcome {
    Done(Counts),
    /// 条件を満たさないので何もしなかった（例: Drive の鍵が無い）。理由は定型文。
    Skipped(&'static str),
}

/// 段の失敗。**ログに出してよい文面だけ**を持つ: 定型文か、DB・I/O の種類（SQLite のメッセージは
/// 表名・列名まで、`io::ErrorKind` はパスを含まない）。
#[derive(Debug, PartialEq, Eq)]
pub struct StageError(String);

impl StageError {
    pub fn fixed(message: &'static str) -> Self {
        Self(message.to_string())
    }

    /// 組み立てた文面から作る。**呼ぶ側が、パス・ファイル名・座標・例外の文字列を含めない**こと
    /// （`PathError`・`io::ErrorKind` など、値を持たない型の `Display` だけを渡す）。
    pub fn text(message: String) -> Self {
        Self(message)
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StageError {}

impl From<WriteError> for StageError {
    fn from(e: WriteError) -> Self {
        Self(e.to_string())
    }
}

impl From<rusqlite::Error> for StageError {
    fn from(e: rusqlite::Error) -> Self {
        Self(format!("DB の読み取りに失敗しました: {e}"))
    }
}

impl From<std::io::Error> for StageError {
    fn from(e: std::io::Error) -> Self {
        // `io::Error` の Display はパスを含まないが、OS のメッセージは種類だけにする
        Self(format!("入出力に失敗しました: {}", e.kind()))
    }
}

pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError>;
}

/// 中身が入る前の段。何もせず、件数なしで終わる。
pub struct Placeholder(pub &'static str);

impl Stage for Placeholder {
    fn name(&self) -> &'static str {
        self.0
    }

    fn run(&self, _ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        Ok(StageOutcome::Done(Counts::default()))
    }
}

/// 段の一覧（[`STAGE_NAMES`] の順）。T2.4〜T2.6 で、残りの `Placeholder` を中身の入った段に置き換えていく
/// （T2.3 で `index_local`・`local_derivatives` が入った）。
pub fn default_stages() -> Vec<Box<dyn Stage>> {
    STAGE_NAMES
        .iter()
        .map(|name| match *name {
            "index_local" => Box::new(crate::local::IndexLocal) as Box<dyn Stage>,
            "local_derivatives" => Box::new(crate::local::LocalDerivatives),
            _ => Box::new(Placeholder(name)),
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageResult {
    Done(Counts),
    Skipped(&'static str),
    Failed(String),
    Panicked,
    /// 停止の合図で実行しなかった。
    NotRun,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StageReport {
    pub name: &'static str,
    pub result: StageResult,
}

/// 表示可能・取得待ち・全件（現行のサイクル末尾のログ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhotoCounts {
    pub ready: i64,
    pub pending: i64,
    pub total: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CycleReport {
    pub stages: Vec<StageReport>,
    pub photos: Option<PhotoCounts>,
}

impl CycleReport {
    /// 失敗した（`Err` かパニック）段の数。
    pub fn failed(&self) -> usize {
        self.stages
            .iter()
            .filter(|s| matches!(s.result, StageResult::Failed(_) | StageResult::Panicked))
            .count()
    }
}

fn photo_counts(conn: &Connection) -> rusqlite::Result<PhotoCounts> {
    let one = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
    Ok(PhotoCounts {
        ready: one("SELECT COUNT(*) FROM photos WHERE display IS NOT NULL AND lat IS NOT NULL")?,
        pending: one("SELECT COUNT(*) FROM photos WHERE lat IS NOT NULL AND display IS NULL")?,
        total: one("SELECT COUNT(*) FROM photos")?,
    })
}

/// サイクルを 1 回実行する。段の失敗・パニックは隔離して次の段へ進む。
pub fn run_cycle(stages: &[Box<dyn Stage>], ctx: &StageCtx<'_>) -> CycleReport {
    let started = Instant::now();
    let mut reports = Vec::with_capacity(stages.len());
    for stage in stages {
        let name = stage.name();
        if ctx.stop.is_stopped() {
            reports.push(StageReport {
                name,
                result: StageResult::NotRun,
            });
            continue;
        }
        let result = match catch_unwind(AssertUnwindSafe(|| stage.run(ctx))) {
            Ok(Ok(StageOutcome::Done(counts))) => {
                if counts.is_empty() {
                    tracing::debug!("[sync] 段 {name}: 完了");
                } else {
                    tracing::info!("[sync] 段 {name}: 完了 {counts}");
                }
                StageResult::Done(counts)
            }
            Ok(Ok(StageOutcome::Skipped(reason))) => {
                tracing::info!("[sync] 段 {name}: 省略（{reason}）");
                StageResult::Skipped(reason)
            }
            Ok(Err(e)) => {
                tracing::warn!("[sync] 段 {name}: 失敗（次の段へ進みます）: {e}");
                StageResult::Failed(e.to_string())
            }
            // パニックのメッセージ（値が入りうる）は出さない。場所は panic hook が出す
            Err(_) => {
                tracing::error!("[sync] 段 {name}: パニック（次の段へ進みます）");
                StageResult::Panicked
            }
        };
        reports.push(StageReport { name, result });
    }
    let photos = match photo_counts(ctx.read) {
        Ok(counts) => {
            tracing::info!(
                "[sync] サイクル完了: 表示可能 {} / 取得待ち {} / 全{}（{} ms{}）",
                counts.ready,
                counts.pending,
                counts.total,
                started.elapsed().as_millis(),
                if ctx.is_dry_run() { "、dry-run" } else { "" }
            );
            Some(counts)
        }
        Err(e) => {
            tracing::warn!("[sync] 件数を数えられません: {e}");
            None
        }
    };
    CycleReport {
        stages: reports,
        photos,
    }
}

/// 同期サイクルのスレッド（現行の `worker`）。起動直後に 1 回、以降 `poll` ごと。`stop` で止まる
/// （待ちの最中ならすぐ、サイクルの最中なら今の段が終わってから）。
pub fn spawn_worker(
    config: Arc<Config>,
    writer: Writer,
    cache: Arc<Cache>,
    stages: Vec<Box<dyn Stage>>,
    stop: StopFlag,
    poll: Duration,
    lock: crate::ingest::LockHold,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("sync-worker".into())
        .spawn(move || {
            // スレッドが終わるまで `.ingest.lock` を手放さない（呼び出し側が先に終わっても）
            let _lock = lock;
            let http = crate::net::agent();
            loop {
                one_cycle(&config, &writer, &cache, &stages, &http, &stop);
                if stop.wait(poll) {
                    break;
                }
            }
        })
        .expect("同期サイクルのスレッドを起動できません")
}

/// サイクル 1 回分。接続を開けないなどの失敗も次の周期に持ち越す（スレッドを終わらせない）。
fn one_cycle(
    config: &Config,
    writer: &Writer,
    cache: &Cache,
    stages: &[Box<dyn Stage>],
    http: &ureq::Agent,
    stop: &StopFlag,
) {
    let read = match crate::db::open_read_only(&config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!("[sync] DB を読み取り用に開けません（次の周期に持ち越します）: {e}");
            return;
        }
    };
    let ctx = StageCtx {
        mode: Mode::Apply,
        config,
        read: &read,
        writer: Some(writer),
        cache,
        http,
        stop,
    };
    // 段ごとの隔離の外側（`run_cycle` 自身）がパニックしても、スレッドは続ける
    if catch_unwind(AssertUnwindSafe(|| run_cycle(stages, &ctx))).is_err() {
        tracing::error!("[sync] サイクルがパニックしました（次の周期に持ち越します）");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::testutil::TempDir;
    use crate::writer::OpenMode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Setup {
        _dir: TempDir,
        config: Arc<Config>,
        writer: Writer,
        cache: Arc<Cache>,
    }

    fn setup(tag: &str) -> Setup {
        let dir = TempDir::new(tag);
        let overrides = Overrides {
            listen: None,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let config = Arc::new(Config::load(|_| None, &overrides).unwrap());
        let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
        writer.call(crate::schema::migrate).unwrap();
        let cache = Arc::new(Cache::load(dir.path()));
        Setup {
            _dir: dir,
            config,
            writer,
            cache,
        }
    }

    /// 呼ばれた順を記録し、指定どおりに成功・失敗・パニックする段。
    struct Probe {
        name: &'static str,
        behavior: fn(&StageCtx<'_>) -> Result<StageOutcome, StageError>,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Stage for Probe {
        fn name(&self) -> &'static str {
            self.name
        }
        fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
            self.log.lock().unwrap().push(self.name);
            (self.behavior)(ctx)
        }
    }

    fn probe(
        name: &'static str,
        log: &Arc<Mutex<Vec<&'static str>>>,
        behavior: fn(&StageCtx<'_>) -> Result<StageOutcome, StageError>,
    ) -> Box<dyn Stage> {
        Box::new(Probe {
            name,
            behavior,
            log: Arc::clone(log),
        })
    }

    fn with_ctx<T>(s: &Setup, mode: Mode, f: impl FnOnce(&StageCtx<'_>) -> T) -> T {
        let read = crate::db::open_read_only(&s.config.db_path()).unwrap();
        let http = crate::net::agent();
        let stop = StopFlag::new();
        let ctx = StageCtx {
            mode,
            config: &s.config,
            read: &read,
            writer: (mode == Mode::Apply).then_some(&s.writer),
            cache: &s.cache,
            http: &http,
            stop: &stop,
        };
        f(&ctx)
    }

    #[test]
    fn default_stages_follow_the_python_order() {
        let names: Vec<_> = default_stages().iter().map(|s| s.name()).collect();
        assert_eq!(names, STAGE_NAMES);
        assert_eq!(
            STAGE_NAMES,
            [
                "index_local",
                "index_drive",
                "infer",
                "enrich",
                "local_derivatives",
                "fetch_derivatives",
                "quality"
            ]
        );
    }

    #[test]
    fn an_empty_cycle_reports_counts_only() {
        let s = setup("sync-empty");
        s.writer
            .call(|c| {
                c.execute_batch(
                    "INSERT INTO photos(source,drive_id,name,display,lat,lon) VALUES
                       ('drive','a','n','derivatives/a.jpg',1,2),
                       ('drive','b','n',NULL,1,2),
                       ('drive','c','n',NULL,NULL,NULL)",
                )
            })
            .unwrap();
        let report = with_ctx(&s, Mode::Apply, |ctx| run_cycle(&default_stages(), ctx));
        assert_eq!(report.failed(), 0);
        assert_eq!(report.stages.len(), 7);
        assert_eq!(
            report.photos,
            Some(PhotoCounts {
                ready: 1,
                pending: 1,
                total: 3
            })
        );
    }

    #[test]
    fn a_failing_or_panicking_stage_does_not_stop_the_next_ones() {
        let s = setup("sync-isolation");
        let log = Arc::new(Mutex::new(Vec::new()));
        let stages = vec![
            probe("a", &log, |_| Err(StageError::fixed("ダミーの失敗"))),
            probe("b", &log, |_| panic!("secret-value")),
            probe("c", &log, |_| Ok(StageOutcome::Skipped("鍵がありません"))),
            probe("d", &log, |_| {
                Ok(StageOutcome::Done(Counts(vec![("added", 3)])))
            }),
        ];
        let report = with_ctx(&s, Mode::Apply, |ctx| run_cycle(&stages, ctx));
        assert_eq!(*log.lock().unwrap(), ["a", "b", "c", "d"]);
        assert_eq!(report.failed(), 2);
        assert_eq!(
            report.stages[0].result,
            StageResult::Failed("ダミーの失敗".into())
        );
        assert_eq!(report.stages[1].result, StageResult::Panicked);
        assert_eq!(
            report.stages[2].result,
            StageResult::Skipped("鍵がありません")
        );
        assert_eq!(
            report.stages[3].result,
            StageResult::Done(Counts(vec![("added", 3)]))
        );
    }

    #[test]
    fn dry_run_gives_stages_no_writer() {
        let s = setup("sync-dry");
        let log = Arc::new(Mutex::new(Vec::new()));
        let stages = vec![probe("w", &log, |ctx| {
            assert!(ctx.is_dry_run());
            // 書こうとすると、それと分かる失敗になる
            ctx.writer()?
                .call(|c| c.execute("DELETE FROM photos", []))?;
            Ok(StageOutcome::Done(Counts::default()))
        })];
        let report = with_ctx(&s, Mode::DryRun, |ctx| run_cycle(&stages, ctx));
        assert_eq!(report.failed(), 1);
        assert_eq!(
            report.stages[0].result,
            StageResult::Failed("dry-run では書き込めません".into())
        );
    }

    #[test]
    fn stop_flag_skips_the_remaining_stages() {
        let s = setup("sync-stop");
        let log = Arc::new(Mutex::new(Vec::new()));
        let stages = vec![
            probe("a", &log, |ctx| {
                ctx.stop.stop();
                Ok(StageOutcome::Done(Counts::default()))
            }),
            probe("b", &log, |_| Ok(StageOutcome::Done(Counts::default()))),
        ];
        let report = with_ctx(&s, Mode::Apply, |ctx| run_cycle(&stages, ctx));
        assert_eq!(*log.lock().unwrap(), ["a"]);
        assert_eq!(report.stages[1].result, StageResult::NotRun);
        assert_eq!(report.failed(), 0);
    }

    #[test]
    fn worker_runs_once_at_start_then_every_period_and_stops() {
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        struct Counter;
        impl Stage for Counter {
            fn name(&self) -> &'static str {
                "counter"
            }
            fn run(&self, _: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
                RUNS.fetch_add(1, Ordering::SeqCst);
                Ok(StageOutcome::Done(Counts::default()))
            }
        }
        let s = setup("sync-worker");
        let stop = StopFlag::new();
        let handle = spawn_worker(
            Arc::clone(&s.config),
            s.writer.clone(),
            Arc::clone(&s.cache),
            vec![Box::new(Counter)],
            stop.clone(),
            Duration::from_millis(40),
            crate::ingest::LockHold::for_test(s._dir.path()),
        );
        for _ in 0..300 {
            if RUNS.load(Ordering::SeqCst) >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let before_stop = RUNS.load(Ordering::SeqCst);
        assert!(before_stop >= 3, "周期ごとに繰り返す: {before_stop}");
        stop.stop();
        handle.join().unwrap();
        let after = RUNS.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(RUNS.load(Ordering::SeqCst), after, "停止後は動かない");
    }

    #[test]
    fn worker_survives_a_stage_panic_and_keeps_cycling() {
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        struct Boom;
        impl Stage for Boom {
            fn name(&self) -> &'static str {
                "boom"
            }
            fn run(&self, _: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
                RUNS.fetch_add(1, Ordering::SeqCst);
                panic!("secret-value");
            }
        }
        let s = setup("sync-worker-panic");
        let stop = StopFlag::new();
        let handle = spawn_worker(
            Arc::clone(&s.config),
            s.writer.clone(),
            Arc::clone(&s.cache),
            vec![Box::new(Boom)],
            stop.clone(),
            Duration::from_millis(20),
            crate::ingest::LockHold::for_test(s._dir.path()),
        );
        for _ in 0..300 {
            if RUNS.load(Ordering::SeqCst) >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(RUNS.load(Ordering::SeqCst) >= 3);
        stop.stop();
        handle.join().unwrap();
    }

    #[test]
    fn counts_display_is_names_and_numbers_only() {
        assert_eq!(
            Counts(vec![("added", 2), ("removed", 0)]).to_string(),
            "added=2 removed=0"
        );
    }

    #[test]
    fn errors_keep_only_loggable_text() {
        let e = StageError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "/secret/path/file.jpg",
        ));
        assert!(!e.to_string().contains("secret"), "{e}");
    }
}
