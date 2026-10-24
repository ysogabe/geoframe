//! 同期サイクル（現行 `catalog.py` の `worker`／`cycle`。棚卸し B1）の骨組み。
//!
//! # 構成
//!
//! サイクルは**段（[`Stage`]）を順に実行**する。段の中身は T2.3〜T2.6 で入れる（T2.3 で
//! `index_local`・`local_derivatives`、T2.4 で Drive の 2 段、T2.5 で `infer`・`enrich` が入った。残りは何もしない
//! [`Placeholder`]）。順序は現行の
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
//! 構造的に起きない。`StageCtx::read` は実 DB の**メモリ上の写し**で（[`simulation_copy`]）、先の段が
//! 自分の計画を写しに適用するので、後の段は「先の段が済んだあと」の状態で件数を数える。
//! ファイルへの書き込み・ネットワークの副作用（原本のダウンロード、derivative の生成、キャッシュの更新）は、各段が [`StageCtx::is_dry_run`] を見て避ける（`Cache` は凍結される）。
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
    /// 読み取り用の接続（サイクルの間だけ有効）。実行では読み取り専用の接続。**`--dry-run` では実 DB の
    /// メモリ上の写し**（[`simulation_copy`]）で、先の段が自分の計画を写しに適用して、後の段に
    /// 「その段が済んだあとの状態」を見せる（実 DB は変わらない）。dry-run の段が写しに書くのは
    /// このためだけで、実 DB への書き込みは [`StageCtx::writer`] を通る。
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

/// 写しを作る時間の上限。本番相当の DB（約 85 MiB）で 0.3 秒ほどなので、その 100 倍。超えたら dry-run を
/// 失敗（終了コード 1）にする。読み取りトランザクションでスナップショットを固定するので（下記）、
/// 超えるのは写しの元（実 DB）が長く排他ロックされているような異常なときだけ。
pub const COPY_DEADLINE: Duration = Duration::from_secs(30);

/// 写しの 1 回の `step` で進めるページ数（4 KiB で約 4 MiB）。
const COPY_PAGES_PER_STEP: i32 = 1024;
/// ロックで進めなかったときの待ち。ロックの解けを待つだけなので短くてよい（期限内なら何度でも）。
const COPY_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// `--dry-run` 用の、実 DB のメモリ上の写し。`read` から SQLite のバックアップ API で作る（実 DB は
/// 読むだけ）。写しは書ける接続で、段が自分の計画を適用して後続の段に見せるために使う。
///
/// **やり直しが起きないようにする**: バックアップは、元が途中で更新されると最初からやり直す。更新の
/// 多い DB では終わらなくなる。そこで、写しの間だけ `read` で読み取りトランザクションを持ち、
/// スナップショットを固定する（WAL なら書き手を止めず、書き手の更新は写しに入らない。ロールバック
/// ジャーナルの DB では、写しの間（約 0.3 秒）だけ書き手が待つ）。さらに [`COPY_DEADLINE`] の期限を
/// 付け、ロックで進めない間は短い間隔で繰り返し、期限を超えたらエラーにする（`busy_timeout` ぶんまでは
/// 超えうる）。
pub fn simulation_copy(read: &Connection) -> rusqlite::Result<Connection> {
    simulation_copy_within(read, COPY_DEADLINE)
}

/// [`simulation_copy`] の期限を指定できる形（試験用に分けた）。
pub(crate) fn simulation_copy_within(
    read: &Connection,
    deadline: Duration,
) -> rusqlite::Result<Connection> {
    let start = Instant::now();
    let timed_out = || {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some(format!(
                "DB が使用中で、{}秒以内にメモリ上へ写せませんでした",
                deadline.as_secs_f32()
            )),
        )
    };
    // スナップショットを固定する。`BEGIN` だけでは最初の読み取りまで固定されないので、1 回読む
    loop {
        let snapshot = read
            .execute_batch("BEGIN")
            .and_then(|()| read.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(())));
        match snapshot {
            Ok(()) => break,
            Err(e) => {
                let _ = read.execute_batch("ROLLBACK");
                if !is_busy(&e) {
                    return Err(e);
                }
                if start.elapsed() >= deadline {
                    return Err(timed_out());
                }
                std::thread::sleep(COPY_RETRY_INTERVAL);
            }
        }
    }
    let result = copy_snapshot(read, start, deadline).map_err(|e| e.unwrap_or_else(timed_out));
    let _ = read.execute_batch("ROLLBACK");
    result
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// `Err(None)` は期限切れ。
fn copy_snapshot(
    read: &Connection,
    start: Instant,
    deadline: Duration,
) -> Result<Connection, Option<rusqlite::Error>> {
    use rusqlite::backup::{Backup, StepResult};
    let mut copy = Connection::open_in_memory()?;
    {
        let backup = Backup::new(read, &mut copy)?;
        loop {
            match backup.step(COPY_PAGES_PER_STEP)? {
                StepResult::Done => break,
                StepResult::More => {}
                // 元か先がロックされている（rusqlite は他の結果も `#[non_exhaustive]` で持つ）
                _ => std::thread::sleep(COPY_RETRY_INTERVAL),
            }
            if start.elapsed() >= deadline {
                return Err(None);
            }
        }
    }
    Ok(copy)
}

/// 段の一覧（[`STAGE_NAMES`] の順）。T2.6 で、残りの `Placeholder`（`quality`）を中身の入った段に置き換える
/// （T2.3 で `index_local`・`local_derivatives`、T2.4 で `index_drive`・`fetch_derivatives`、T2.5 で
/// `infer`・`enrich` が入った）。
pub fn default_stages() -> Vec<Box<dyn Stage>> {
    // Drive の 2 つの段は、トークン・フォルダ ID を共有する（現行は 1 サイクルで `svc` を作って両方に渡す）
    default_stages_with(Arc::new(crate::drive::DriveShared::new()))
}

/// `geoframe sync --allow-mass-delete` の 1 回だけ使う段の一覧: `index_drive` が大量削除の保留を外す。
/// **`serve` の定期実行は [`default_stages`] で、常に保留する**（環境変数・設定では外せない）。
pub fn default_stages_allowing_mass_delete() -> Vec<Box<dyn Stage>> {
    stages(Arc::new(crate::drive::DriveShared::new()), true)
}

/// [`default_stages`] の、Drive の状態（通信先・再試行）を指定できる形（試験用）。
pub(crate) fn default_stages_with(drive: Arc<crate::drive::DriveShared>) -> Vec<Box<dyn Stage>> {
    stages(drive, false)
}

fn stages(drive: Arc<crate::drive::DriveShared>, allow_mass_delete: bool) -> Vec<Box<dyn Stage>> {
    // `enrich` は応答の壊れた座標を覚えるので、プロセスの間 1 つを使い回す（`stages` を呼ぶたびに作る）
    STAGE_NAMES
        .iter()
        .map(|name| match *name {
            "index_local" => Box::new(crate::local::IndexLocal) as Box<dyn Stage>,
            "infer" => Box::new(crate::geo::Infer),
            "enrich" => Box::new(crate::geo::Enrich::new()),
            "index_drive" if allow_mass_delete => Box::new(
                crate::drive::IndexDrive::allowing_mass_delete(Arc::clone(&drive)),
            ),
            "index_drive" => Box::new(crate::drive::IndexDrive::new(Arc::clone(&drive))),
            "local_derivatives" => Box::new(crate::local::LocalDerivatives::new()),
            "fetch_derivatives" => {
                Box::new(crate::drive::FetchDerivatives::new(Arc::clone(&drive)))
            }
            "quality" => Box::new(crate::quality::Quality::new()),
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
                    // 位置は `exif`（推定の取り消しの対象にならない）。国外なので、地名の段は通信しない
                    "INSERT INTO photos(source,drive_id,name,display,lat,lon,loc_source) VALUES
                       ('drive','a','n','derivatives/a.jpg',1,2,'exif'),
                       ('drive','b','n',NULL,1,2,'exif'),
                       ('drive','c','n',NULL,NULL,NULL,NULL)",
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

    /// 別の接続が更新し続けても、写しは完了する（スナップショットを固定するのでやり直しが起きない）。
    /// 写しは固定した時点の内容で、整合している。
    #[test]
    fn simulation_copy_finishes_while_a_wal_db_is_updated_continuously() {
        let dir = TempDir::new("sync-copy-wal");
        let path = dir.path().join("catalog.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB)")
                .unwrap();
            // 1 回の step（1024 ページ）では収まらない大きさ（約 12 MiB）
            let tx = conn.unchecked_transaction().unwrap();
            for _ in 0..3000 {
                tx.execute("INSERT INTO t(v) VALUES (zeroblob(4000))", [])
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (path, stop) = (path.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                let conn = Connection::open(&path).unwrap();
                conn.busy_timeout(Duration::from_secs(5)).unwrap();
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    // 元の接続が step の合間に見るたびに変わっている状態を作り続ける
                    conn.execute("INSERT INTO t(v) VALUES (zeroblob(100))", [])
                        .unwrap();
                    conn.execute(
                        "UPDATE t SET v = zeroblob(4000) WHERE id = ?1",
                        [n % 3000 + 1],
                    )
                    .unwrap();
                    n += 1;
                }
                n
            })
        };
        let read = crate::db::open_read_only(&path).unwrap();
        let before: i64 = read
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        let started = Instant::now();
        let copy = simulation_copy_within(&read, Duration::from_secs(20));
        let took = started.elapsed();
        stop.store(true, Ordering::SeqCst);
        let written = writer.join().unwrap();
        let copy = copy.expect("更新され続けても完了する");
        assert!(written > 0, "書き手が動いていなかった");
        assert!(took < Duration::from_secs(20), "{took:?}");
        let rows: i64 = copy
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert!(rows >= before, "{rows} < {before}");
        let check: String = copy
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        // 写しのあと、元の接続は読み取りトランザクションを持ち越さない（次は最新が見える）
        assert!(read.is_autocommit());
        let latest: i64 = read
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert!(latest >= rows);
    }

    /// 元が排他ロックされたままなら、期限でエラーにする（終わらずに待ち続けない）。
    #[test]
    fn simulation_copy_fails_at_the_deadline_when_the_source_stays_locked() {
        let dir = TempDir::new("sync-copy-locked");
        let path = dir.path().join("catalog.db");
        let other = Connection::open(&path).unwrap();
        other
            .execute_batch("CREATE TABLE t(id INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        let read = crate::db::open_read_only(&path).unwrap();
        read.busy_timeout(Duration::from_millis(50)).unwrap();
        // ジャーナルが DELETE のまま、書き手が排他ロックを持つ
        other.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let started = Instant::now();
        let err = simulation_copy_within(&read, Duration::from_millis(200)).unwrap_err();
        let took = started.elapsed();
        assert!(is_busy(&err), "{err}");
        assert!(
            err.to_string().contains("メモリ上へ写せませんでした"),
            "{err}"
        );
        // 期限＋busy_timeout 1 回ぶん程度で返る
        assert!(took < Duration::from_secs(5), "{took:?}");
        // 失敗しても元の接続はトランザクションを持ち越さず、ロックが解ければ写せる
        assert!(read.is_autocommit());
        other.execute_batch("COMMIT").unwrap();
        let copy = simulation_copy_within(&read, Duration::from_secs(5)).unwrap();
        let n: i64 = copy
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
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
