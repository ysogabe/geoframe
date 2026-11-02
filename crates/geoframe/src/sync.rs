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
use crate::ops::{Board, OpKind, Outcome, Request, RunKind, outcome_of, stages_json};
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
    simulation_copy_stepping(read, deadline, &mut || {})
}

/// [`simulation_copy_within`] に、バックアップの 1 step が終わって続きがあるたびに呼ぶ `after_step` を足した形
/// （試験が、写しの途中で元の DB を更新させるために使う。本番は何もしない閉包を渡す）。
fn simulation_copy_stepping(
    read: &Connection,
    deadline: Duration,
    after_step: &mut dyn FnMut(),
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
    let result =
        copy_snapshot(read, start, deadline, after_step).map_err(|e| e.unwrap_or_else(timed_out));
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
    after_step: &mut dyn FnMut(),
) -> Result<Connection, Option<rusqlite::Error>> {
    use rusqlite::backup::{Backup, StepResult};
    let mut copy = Connection::open_in_memory()?;
    {
        let backup = Backup::new(read, &mut copy)?;
        loop {
            match backup.step(COPY_PAGES_PER_STEP)? {
                StepResult::Done => break,
                StepResult::More => after_step(),
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
    stages(
        Arc::new(crate::drive::DriveShared::new()),
        crate::drive::Allow::Yes,
    )
}

/// 運用コンソールの「保留の解除」が走らせる段の一覧: `index_drive` の 1 段だけで、承認した行の集合（`ids`）に限って
/// 消す（[`crate::drive::Allow::Only`]）。`drive` は `serve` の定期実行と同じもの（保留の記憶・トークンを共有する）。
pub fn index_drive_stage_only(
    drive: Arc<crate::drive::DriveShared>,
    ids: crate::drive::HeldIds,
) -> Vec<Box<dyn Stage>> {
    vec![Box::new(crate::drive::IndexDrive::allowing_only(
        drive, ids,
    ))]
}

/// [`default_stages`] の、Drive の状態（通信先・再試行）を指定できる形（試験用）。
pub(crate) fn default_stages_with(drive: Arc<crate::drive::DriveShared>) -> Vec<Box<dyn Stage>> {
    stages(drive, crate::drive::Allow::No)
}

fn stages(
    drive: Arc<crate::drive::DriveShared>,
    allow: crate::drive::Allow,
) -> Vec<Box<dyn Stage>> {
    // `enrich` は応答の壊れた座標を覚えるので、プロセスの間 1 つを使い回す（`stages` を呼ぶたびに作る）
    STAGE_NAMES
        .iter()
        .map(|name| match *name {
            "index_local" => Box::new(crate::local::IndexLocal) as Box<dyn Stage>,
            "infer" => Box::new(crate::geo::Infer),
            "enrich" => Box::new(crate::geo::Enrich::new()),
            "index_drive" => Box::new(crate::drive::IndexDrive::with_allow(
                Arc::clone(&drive),
                allow.clone(),
            )),
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

/// 要求の待ちを見る間隔の上限。停止は `StopFlag` の `Condvar` で即座に起きるので、これは運用の要求の検出の遅れの上限
/// （設計書の方針 1）。
const REQUEST_POLL: Duration = Duration::from_millis(100);

/// 同期サイクルのスレッド（現行の `worker`）。起動直後に 1 回、以降 `poll` ごとの定期サイクルに加えて、運用コンソールの
/// 要求（`board`。[`crate::ops`]）を同じスレッドで直列に処理する。`stop` で止まる（待ちの最中ならすぐ、サイクルの最中なら
/// 今の段が終わってから）。
///
/// - 要求は定期サイクルより先に取り出す。定期サイクルの実行中に来た要求は、サイクルの直後に走る。
/// - 手動の同期（[`OpKind::Sync`]）のあとは、次の定期サイクルを `poll` 後にする（続けて 2 回回さない）。品質解析・保留の
///   解除は周期を変えない。
/// - `drive` は `stages` の `index_drive` と同じもの（保留の記憶・トークンを共有する）。保留の解除の要求は、その記憶
///   （`drive.mass_delete_board()`）の印と照らしてから `index_drive` だけを走らせる。
/// - DB を開けない・`run_cycle` 自身のパニックでも、要求は `failed` で記録し、スレッドは続ける。
#[allow(clippy::too_many_arguments)]
pub fn spawn_worker(
    config: Arc<Config>,
    writer: Writer,
    cache: Arc<Cache>,
    stages: Vec<Box<dyn Stage>>,
    stop: StopFlag,
    poll: Duration,
    lock: crate::ingest::LockHold,
    board: Arc<Board>,
    drive: Arc<crate::drive::DriveShared>,
) -> JoinHandle<()> {
    spawn_worker_impl(
        config,
        writer,
        cache,
        stages,
        stop,
        poll,
        lock,
        board,
        drive,
        #[cfg(test)]
        None,
    )
}

/// 保留の解除で走らせる段を作る閉包（試験で差し替える）。
#[cfg(test)]
type ReleaseStages = Arc<
    dyn Fn(Arc<crate::drive::DriveShared>, crate::drive::HeldIds) -> Vec<Box<dyn Stage>>
        + Send
        + Sync,
>;

#[allow(clippy::too_many_arguments)]
fn spawn_worker_impl(
    config: Arc<Config>,
    writer: Writer,
    cache: Arc<Cache>,
    stages: Vec<Box<dyn Stage>>,
    stop: StopFlag,
    poll: Duration,
    lock: crate::ingest::LockHold,
    board: Arc<Board>,
    drive: Arc<crate::drive::DriveShared>,
    #[cfg(test)] release_stages: Option<ReleaseStages>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("sync-worker".into())
        .spawn(move || {
            // スレッドが終わるまで `.ingest.lock` を手放さない（呼び出し側が先に終わっても）
            let _lock = lock;
            let http = crate::net::agent();
            let env = WorkerEnv {
                config: &config,
                writer: &writer,
                cache: &cache,
                http: &http,
                stop: &stop,
                #[cfg(test)]
                release_stages: release_stages.clone(),
            };
            let mut next_cycle_at = Instant::now(); // 起動直後に 1 回
            loop {
                if stop.is_stopped() {
                    break;
                }
                if let Some(request) = board.take_queued() {
                    run_request(&env, &board, &stages, &drive, &request);
                    if request.kind == OpKind::Sync {
                        next_cycle_at = Instant::now() + poll;
                        board.schedule_next_cycle(poll);
                    }
                    continue;
                }
                let now = Instant::now();
                if now >= next_cycle_at {
                    board.start_cycle();
                    let guard = board.cycle_guard(poll);
                    match one_cycle(&env, &stages) {
                        Some(report) => guard.finish_cycle(&report),
                        // 段の結果を得られなかった（`Drop` が失敗として終える）
                        None => drop(guard),
                    }
                    next_cycle_at = Instant::now() + poll;
                    continue;
                }
                if stop.wait((next_cycle_at - now).min(REQUEST_POLL)) {
                    break;
                }
            }
        })
        .expect("同期サイクルのスレッドを起動できません")
}

/// ワーカーが段を走らせるのに使うもの（サイクルの間じゅう変わらない）。
struct WorkerEnv<'a> {
    config: &'a Config,
    writer: &'a Writer,
    cache: &'a Cache,
    http: &'a ureq::Agent,
    stop: &'a StopFlag,
    /// 試験で、保留の解除の段を差し替える（本番は `None` 相当で `index_drive_stage_only`）。
    #[cfg(test)]
    release_stages: Option<ReleaseStages>,
}

impl WorkerEnv<'_> {
    /// 保留の解除で走らせる段（`index_drive` だけ）。
    fn release_stages(
        &self,
        drive: &Arc<crate::drive::DriveShared>,
        ids: crate::drive::HeldIds,
    ) -> Vec<Box<dyn Stage>> {
        #[cfg(test)]
        if let Some(make) = &self.release_stages {
            return make(Arc::clone(drive), ids);
        }
        index_drive_stage_only(Arc::clone(drive), ids)
    }
}

/// 取り出した運用の要求を 1 件処理して、`board` に結果を残す（どの経路でも `running` を外す: [`Board::guard`]）。
fn run_request(
    env: &WorkerEnv<'_>,
    board: &Board,
    stages: &[Box<dyn Stage>],
    drive: &Arc<crate::drive::DriveShared>,
    request: &Request,
) {
    let guard = board.guard(request.id);
    let report = match request.kind {
        OpKind::Sync => one_cycle(env, stages),
        OpKind::QualityScan => one_cycle(env, &[Box::new(crate::quality::Scan)]),
        OpKind::DriveRelease => {
            // 実行の直前に、待っているあいだに保留が変わっていないか確かめる（印と集合が違えば実行しない）。
            // 印か集合が無い要求は承認が無いのと同じなので実行しない
            let current = drive.mass_delete_board().current();
            let approved = match (&request.token, &request.ids, &current) {
                (Some(token), Some(ids), Some(hold)) => {
                    (*token == crate::ops::token_of(hold)).then(|| ids.clone())
                }
                _ => None,
            };
            let Some(ids) = approved else {
                tracing::warn!("[ops] 保留の解除: 承認したあとで保留が変わったので実行しません");
                guard.finish(Outcome::HoldChanged, Vec::new(), None);
                return;
            };
            one_cycle(env, &env.release_stages(drive, ids))
        }
        // AI 系は POST の中で処理し、同期ワーカーの待ちには入らない
        OpKind::AiRetry | OpKind::AiReleasePause => None,
    };
    match report {
        Some(report) => guard.finish(
            outcome_of(RunKind::Op(request.kind), &report),
            stages_json(&report),
            None,
        ),
        None => guard.finish(Outcome::Failed, Vec::new(), None),
    }
}

/// サイクル 1 回分（`stages` を順に）。接続を開けない・`run_cycle` 自身のパニックは `None`（次の周期・要求に持ち越す。
/// スレッドを終わらせない）。
fn one_cycle(env: &WorkerEnv<'_>, stages: &[Box<dyn Stage>]) -> Option<CycleReport> {
    let read = match crate::db::open_read_only(&env.config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!("[sync] DB を読み取り用に開けません（次の周期に持ち越します）: {e}");
            return None;
        }
    };
    let ctx = StageCtx {
        mode: Mode::Apply,
        config: env.config,
        read: &read,
        writer: Some(env.writer),
        cache: env.cache,
        http: env.http,
        stop: env.stop,
    };
    // 段ごとの隔離の外側（`run_cycle` 自身）がパニックしても、スレッドは続ける
    match catch_unwind(AssertUnwindSafe(|| run_cycle(stages, &ctx))) {
        Ok(report) => Some(report),
        Err(_) => {
            tracing::error!("[sync] サイクルがパニックしました（次の周期に持ち越します）");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::ops::EnqueueError;
    use crate::stop::join_within;
    use crate::testutil::TempDir;
    use crate::writer::OpenMode;
    use std::cell::Cell;
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
            Arc::new(Board::with_system_clock(stop.clone())),
            Arc::new(crate::drive::DriveShared::new()),
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
            Arc::new(Board::with_system_clock(stop.clone())),
            Arc::new(crate::drive::DriveShared::new()),
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

    // ---- 運用コンソールの要求（`ops::Board`）を処理するワーカー ----

    /// 進みを自分で決める時計（`ops::Board` に渡す）。時刻の文字列は「基準からの秒数」。
    struct TestClock {
        base: Instant,
        elapsed: Mutex<Duration>,
    }

    impl TestClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
            })
        }

        fn advance(&self, by: Duration) {
            *self.elapsed.lock().unwrap() += by;
        }

        fn stamp(&self, after: Duration) -> String {
            crate::ops::Clock::timestamp(self, after)
        }
    }

    impl crate::ops::Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + *self.elapsed.lock().unwrap()
        }

        fn timestamp(&self, after: Duration) -> String {
            format!("t+{}s", (*self.elapsed.lock().unwrap() + after).as_secs())
        }
    }

    /// 動き出したことを知らせ、放してもらう（または停止の合図が来る）まで止まる段。
    struct Gate {
        started: Mutex<std::sync::mpsc::Sender<()>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        runs: Arc<AtomicUsize>,
    }

    /// `Gate` を操作する側。
    struct GateHandle {
        started: std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::Sender<()>,
        runs: Arc<AtomicUsize>,
    }

    impl GateHandle {
        /// 段が動き出すまで待つ。
        fn wait_started(&self) {
            self.started
                .recv_timeout(Duration::from_secs(10))
                .expect("段が動き出さない");
        }

        /// 止まっている段を 1 つ放す。
        fn release(&self) {
            self.release.send(()).unwrap();
        }

        fn runs(&self) -> usize {
            self.runs.load(Ordering::SeqCst)
        }
    }

    fn gate() -> (Box<dyn Stage>, GateHandle) {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let runs = Arc::new(AtomicUsize::new(0));
        (
            Box::new(Gate {
                started: Mutex::new(started_tx),
                release: Mutex::new(release_rx),
                runs: Arc::clone(&runs),
            }),
            GateHandle {
                started: started_rx,
                release: release_tx,
                runs,
            },
        )
    }

    impl Stage for Gate {
        fn name(&self) -> &'static str {
            "gate"
        }

        fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let _ = self.started.lock().unwrap().send(());
            let release = self.release.lock().unwrap();
            // 停止の合図が来たら戻る（本物の長い段も項目の合間に `ctx.stop` を見る）
            let deadline = Instant::now() + Duration::from_secs(10);
            while release.try_recv().is_err() && Instant::now() < deadline {
                if ctx.stop.wait(Duration::from_millis(5)) {
                    break;
                }
            }
            Ok(StageOutcome::Done(Counts(vec![("n", 1)])))
        }
    }

    /// 回数だけ数えて終わる段。
    struct Tick(Arc<AtomicUsize>);

    impl Stage for Tick {
        fn name(&self) -> &'static str {
            "tick"
        }

        fn run(&self, _: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(StageOutcome::Done(Counts::default()))
        }
    }

    /// `run_cycle` の外側でパニックする段（`name()` は段ごとの隔離の外で呼ばれる）。
    struct BadName;

    impl Stage for BadName {
        fn name(&self) -> &'static str {
            panic!("secret-name");
        }

        fn run(&self, _: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
            Ok(StageOutcome::Done(Counts::default()))
        }
    }

    struct Fixed(&'static str, fn() -> Result<StageOutcome, StageError>);

    impl Stage for Fixed {
        fn name(&self) -> &'static str {
            self.0
        }

        fn run(&self, _: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
            (self.1)()
        }
    }

    /// 要求を処理するワーカーと、その掲示板・時計・保留の記憶。
    struct Ops {
        s: Setup,
        board: Arc<Board>,
        clock: Arc<TestClock>,
        stop: StopFlag,
        mass_delete: Arc<crate::drive::MassDeleteBoard>,
        handle: Option<JoinHandle<()>>,
    }

    fn start_ops(tag: &str, stages: Vec<Box<dyn Stage>>, poll: Duration) -> Ops {
        start_ops_with(tag, stages, poll, |_| None)
    }

    /// 保留の解除で走らせる段を差し替えて始める（閉包は停止の合図を受け取る）。
    fn start_ops_with(
        tag: &str,
        stages: Vec<Box<dyn Stage>>,
        poll: Duration,
        release_stages: impl FnOnce(&StopFlag) -> Option<ReleaseStages>,
    ) -> Ops {
        let s = setup(tag);
        let clock = TestClock::new();
        let stop = StopFlag::new();
        let board = Arc::new(Board::new(stop.clone(), clock.clone()));
        let drive = Arc::new(crate::drive::DriveShared::new());
        let mass_delete = drive.mass_delete_board();
        let release_stages = release_stages(&stop);
        let handle = spawn_worker_impl(
            Arc::clone(&s.config),
            s.writer.clone(),
            Arc::clone(&s.cache),
            stages,
            stop.clone(),
            poll,
            crate::ingest::LockHold::for_test(s._dir.path()),
            Arc::clone(&board),
            Arc::clone(&drive),
            release_stages,
        );
        Ops {
            s,
            board,
            clock,
            stop,
            mass_delete,
            handle: Some(handle),
        }
    }

    impl Ops {
        /// 条件が成り立つまで待つ（最大 10 秒）。
        fn wait_for(&self, what: &str, cond: impl Fn(&crate::ops::Snapshot) -> bool) {
            for _ in 0..1000 {
                if cond(&self.board.snapshot()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("待っても成り立たない: {what}");
        }

        /// 履歴が `n` 件になり、実行中が空になるまで待つ。
        fn wait_history(&self, n: usize) {
            self.wait_for("履歴", |snap| {
                snap.history.len() >= n && snap.running.is_none()
            });
        }

        /// 最初の定期サイクル（起動直後の 1 回）が終わるまで待つ。
        fn wait_first_cycle(&self) {
            self.wait_for("最初のサイクル", |snap| snap.last_cycle.is_some());
        }

        fn latest(&self) -> crate::ops::Record {
            self.board.snapshot().history[0].clone()
        }

        /// 停止して、スレッドが終わるのを待つ（時間内に終わらなければ失敗）。
        fn stop_and_join(&mut self) {
            self.stop.stop();
            let handle = self.handle.take().unwrap();
            let started = Instant::now();
            assert!(join_within(handle, Duration::from_secs(5)), "止まらない");
            assert!(started.elapsed() < Duration::from_secs(2), "停止が遅い");
        }
    }

    impl Drop for Ops {
        fn drop(&mut self) {
            self.stop.stop();
            if let Some(handle) = self.handle.take() {
                let _ = join_within(handle, Duration::from_secs(5));
            }
        }
    }

    const LONG: Duration = Duration::from_secs(3600);

    /// 退けたデータベース（接続は開いたまま。読み取り用の接続だけが開けなくなる）。
    fn move_db(config: &Config, away: bool) {
        let db = config.db_path();
        let moved = db.with_extension("db.away");
        if away {
            std::fs::rename(&db, &moved).unwrap();
        } else {
            std::fs::rename(&moved, &db).unwrap();
        }
    }

    #[test]
    fn a_request_arriving_during_a_cycle_waits_and_runs_right_after_it() {
        let (stage, gate) = gate();
        let ops = start_ops("ops-during-cycle", vec![stage], LONG);
        gate.wait_started(); // 起動直後の定期サイクルが止まっている
        assert_eq!(ops.board.snapshot().running.unwrap().kind, RunKind::Cycle);
        let queued = ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        // 受け付けても、サイクルが終わるまでは走らない
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(gate.runs(), 1);
        assert_eq!(ops.board.snapshot().queued.unwrap().id, queued.id);
        gate.release(); // サイクルが終わる → 直後に要求が走る
        gate.wait_started();
        let snap = ops.board.snapshot();
        let running = snap.running.unwrap();
        assert_eq!(
            (running.id, running.kind),
            (queued.id, RunKind::Op(OpKind::Sync))
        );
        assert!(snap.queued.is_none());
        assert_eq!(snap.last_cycle.unwrap().outcome, Outcome::Done);
        gate.release();
        ops.wait_history(1);
        assert_eq!(ops.latest().outcome, Outcome::Done);
        assert_eq!(gate.runs(), 2);
    }

    #[test]
    fn the_same_kind_is_busy_and_another_kind_waits_one_at_a_time() {
        let (stage, gate) = gate();
        let ops = start_ops("ops-busy", vec![stage], LONG);
        gate.wait_started();
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        // 待ちは 1 つだけ（同じ種類も別の種類も入らない）
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::Busy)
        );
        assert_eq!(
            ops.board.enqueue(OpKind::QualityScan, None, None),
            Err(EnqueueError::Busy)
        );
        gate.release(); // サイクルが終わり、同期が走り出す
        gate.wait_started();
        // 実行中の同期と同じ種類は入らない。別の種類は 1 つ待てて、2 つ目は入らない
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::Busy)
        );
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        assert_eq!(
            ops.board.enqueue(OpKind::DriveRelease, None, None),
            Err(EnqueueError::Busy)
        );
        gate.release(); // 同期が終わり、品質解析（本物の `quality::Scan`）が走る
        ops.wait_history(2);
        let snap = ops.board.snapshot();
        assert_eq!(snap.history[0].kind, RunKind::Op(OpKind::QualityScan));
        assert_eq!(snap.history[0].outcome, Outcome::Done);
        assert_eq!(snap.history[1].kind, RunKind::Op(OpKind::Sync));
        // 品質解析の段は `quality` の 1 段だけ
        assert_eq!(
            snap.history[0]
                .stages
                .iter()
                .map(|st| st.name)
                .collect::<Vec<_>>(),
            ["quality"]
        );
        assert_eq!(gate.runs(), 2, "品質解析は定期の段を走らせない");
    }

    #[test]
    fn a_failing_db_open_records_failed_and_the_worker_moves_on() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ops = start_ops("ops-db-broken", vec![Box::new(Tick(ticks.clone()))], LONG);
        ops.wait_first_cycle();
        move_db(&ops.s.config, true);
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(1);
        let record = ops.latest();
        assert_eq!(record.outcome, Outcome::Failed);
        assert!(record.stages.is_empty());
        assert!(ops.board.snapshot().running.is_none());
        assert_eq!(ticks.load(Ordering::SeqCst), 1, "段は走っていない");
        // 失敗した同期も最短の間隔の基準になる
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
        // 次の要求へ進める（直したあとなら通る）
        move_db(&ops.s.config, false);
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.wait_history(2);
        assert_eq!(ops.latest().outcome, Outcome::Done);
        // 手動の同期の周期の置き直し: 失敗しても次の周期は poll 後
        assert_eq!(
            ops.board.snapshot().next_cycle_at,
            Some(ops.clock.stamp(LONG))
        );
    }

    #[test]
    fn a_failing_db_open_also_ends_the_periodic_cycle_as_failed() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ops = start_ops(
            "ops-db-broken-cycle",
            vec![Box::new(Tick(ticks.clone()))],
            Duration::from_millis(30),
        );
        ops.wait_first_cycle();
        move_db(&ops.s.config, true);
        ops.wait_for("失敗したサイクル", |snap| {
            snap.last_cycle
                .as_ref()
                .is_some_and(|c| c.outcome == Outcome::Failed)
        });
        assert!(ops.board.snapshot().history.is_empty());
        move_db(&ops.s.config, false);
        ops.wait_for("直ったサイクル", |snap| {
            snap.last_cycle
                .as_ref()
                .is_some_and(|c| c.outcome == Outcome::Done)
        });
    }

    #[test]
    fn a_panic_inside_run_cycle_is_failed_and_a_stage_panic_is_partial() {
        let ops = start_ops(
            "ops-panic",
            vec![
                Box::new(Fixed("fails", || Err(StageError::fixed("secret-failure")))),
                Box::new(Fixed("boom", || panic!("secret-panic"))),
                Box::new(Fixed("fine", || Ok(StageOutcome::Done(Counts::default())))),
            ],
            LONG,
        );
        ops.wait_first_cycle();
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(1);
        let record = ops.latest();
        assert_eq!(record.outcome, Outcome::Partial);
        let results: Vec<_> = record
            .stages
            .iter()
            .map(|st| (st.name, st.result))
            .collect();
        assert_eq!(
            results,
            [("fails", "failed"), ("boom", "panicked"), ("fine", "done")]
        );
        let json = ops.board.snapshot().to_json().to_string();
        assert!(!json.contains("secret"), "失敗の文面は出さない: {json}");

        // `run_cycle` 自身のパニックは、段の結果を得られないので失敗。スレッドは続く
        let ops = start_ops("ops-panic-outer", vec![Box::new(BadName)], LONG);
        ops.wait_first_cycle();
        assert_eq!(
            ops.board.snapshot().last_cycle.unwrap().outcome,
            Outcome::Failed
        );
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(1);
        let record = ops.latest();
        assert_eq!(record.outcome, Outcome::Failed);
        assert!(record.stages.is_empty());
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.wait_history(2);
        assert_eq!(ops.latest().outcome, Outcome::Done);
    }

    #[test]
    fn stopping_while_waiting_returns_quickly_and_refuses_new_requests() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let mut ops = start_ops("ops-stop-idle", vec![Box::new(Tick(ticks.clone()))], LONG);
        ops.wait_first_cycle();
        std::thread::sleep(Duration::from_millis(150)); // 待ちに入っている
        ops.stop_and_join();
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::Stopping)
        );
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stopping_while_running_returns_and_the_waiting_request_does_not_start() {
        let (stage, gate) = gate();
        let mut ops = start_ops("ops-stop-running", vec![stage], LONG);
        gate.wait_started();
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        gate.release();
        gate.wait_started(); // 同期が走っている
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.stop_and_join(); // 段は停止の合図で戻る
        let snap = ops.board.snapshot();
        assert_eq!(snap.history.len(), 1, "待っていた要求は走らない");
        assert_eq!(snap.history[0].kind, RunKind::Op(OpKind::Sync));
        assert!(snap.running.is_none());
        assert_eq!(snap.queued.unwrap().kind, OpKind::QualityScan);
        assert_eq!(
            ops.board.enqueue(OpKind::AiRetry, None, None),
            Err(EnqueueError::Stopping)
        );
    }

    #[test]
    fn drive_reads_are_too_soon_for_60_seconds_by_the_boards_clock() {
        let ops = start_ops("ops-too-soon", vec![Box::new(Tick(Arc::default()))], LONG);
        ops.wait_first_cycle();
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(1);
        // 1. 同期のすぐあとの同期 2. 保留の解除（Drive を読む要求は共通） 3. 品質解析は対象外
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
        assert_eq!(
            ops.board
                .enqueue(OpKind::DriveRelease, Some("t".into()), None),
            Err(EnqueueError::TooSoon)
        );
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.wait_history(2);
        ops.clock.advance(Duration::from_secs(59));
        assert_eq!(
            ops.board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
        ops.clock.advance(Duration::from_secs(1));
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(3);
    }

    fn hold(drive_rows: u64) -> Option<(u64, crate::drive::HoldReason, u64)> {
        Some((3, crate::drive::HoldReason::Ratio, drive_rows))
    }

    fn token_of_current(ops: &Ops) -> String {
        crate::ops::token_of(&ops.mass_delete.current().unwrap())
    }

    #[test]
    fn a_drive_release_whose_hold_changed_while_waiting_does_not_run() {
        use crate::drive::HeldIds;
        let (stage, gate) = gate();
        let ops = start_ops("ops-hold-changed", vec![stage], LONG);
        gate.wait_started(); // 定期サイクルが止まっているあいだに要求を入れる
        let ids = HeldIds::new(vec![1, 2, 3]);
        ops.mass_delete
            .record_with_ids(hold(100), ids.clone(), "t0");
        let token = token_of_current(&ops);
        ops.board
            .enqueue(OpKind::DriveRelease, Some(token.clone()), Some(ids.clone()))
            .unwrap();
        // 待っているあいだに、保留が変わる
        ops.mass_delete
            .record_with_ids(hold(101), ids.clone(), "t1");
        gate.release();
        ops.wait_history(1);
        let record = ops.latest();
        assert_eq!(record.outcome, Outcome::HoldChanged);
        assert!(record.stages.is_empty(), "段は走っていない");
        assert_eq!(gate.runs(), 1);
        assert_eq!(ops.mass_delete.current().unwrap().checked_at, "t1");
        // 保留が消えていても同じ
        ops.clock.advance(Duration::from_secs(61));
        ops.mass_delete
            .record_with_ids(None, HeldIds::default(), "t2");
        ops.board
            .enqueue(OpKind::DriveRelease, Some(token), Some(ids))
            .unwrap();
        ops.wait_history(2);
        assert_eq!(ops.latest().outcome, Outcome::HoldChanged);
        // 印や集合の無い要求は承認が無いのと同じ
        ops.clock.advance(Duration::from_secs(61));
        ops.board.enqueue(OpKind::DriveRelease, None, None).unwrap();
        ops.wait_history(3);
        assert_eq!(ops.latest().outcome, Outcome::HoldChanged);
    }

    #[test]
    fn a_drive_release_with_the_same_hold_runs_only_index_drive_and_maps_skipped() {
        use crate::drive::HeldIds;
        let (stage, gate) = gate();
        let ops = start_ops("ops-release-skipped", vec![stage], LONG);
        gate.wait_started();
        let ids = HeldIds::new(vec![5, 7]);
        ops.mass_delete
            .record_with_ids(hold(100), ids.clone(), "t0");
        ops.board
            .enqueue(
                OpKind::DriveRelease,
                Some(token_of_current(&ops)),
                Some(ids),
            )
            .unwrap();
        gate.release();
        ops.wait_history(1);
        // 鍵が無いので `index_drive` は通信せずに省略される → skipped（`Skipped` の対応）
        let record = ops.latest();
        assert_eq!(record.outcome, Outcome::Skipped);
        assert_eq!(
            record
                .stages
                .iter()
                .map(|st| (st.name, st.result))
                .collect::<Vec<_>>(),
            [("index_drive", "skipped")]
        );
        assert_eq!(gate.runs(), 1, "定期の段は走らない");
        // 保留は消さない（削除していない）
        assert!(ops.mass_delete.current().is_some());
    }

    /// 保留の解除で走らせる段が `index_drive` の代わりに `stage` になるようにして、承認済みの要求を 1 件流し、
    /// 要求の履歴の 1 件目を返す。`before` は段を作る直前（要求を取り出したあと）に呼ばれる。
    fn run_release_with(
        tag: &str,
        before: fn(&StopFlag),
        behavior: fn() -> Result<StageOutcome, StageError>,
    ) -> (Ops, crate::ops::Record, GateHandle) {
        use crate::drive::HeldIds;
        let (gate_stage, gate) = gate();
        let ops = start_ops_with(tag, vec![gate_stage], LONG, |stop| {
            let stop = stop.clone();
            Some(Arc::new(move |_, _| {
                before(&stop);
                vec![Box::new(Fixed("index_drive", behavior)) as Box<dyn Stage>]
            }))
        });
        gate.wait_started();
        let ids = HeldIds::new(vec![5, 7]);
        ops.mass_delete
            .record_with_ids(hold(100), ids.clone(), "t0");
        ops.board
            .enqueue(
                OpKind::DriveRelease,
                Some(token_of_current(&ops)),
                Some(ids),
            )
            .unwrap();
        gate.release();
        ops.wait_history(1);
        let record = ops.latest();
        (ops, record, gate)
    }

    fn release_stage_results(record: &crate::ops::Record) -> Vec<(&'static str, &'static str)> {
        record
            .stages
            .iter()
            .map(|st| (st.name, st.result))
            .collect()
    }

    #[test]
    fn a_drive_release_stopped_before_the_stage_is_skipped_and_leaves_running() {
        // 要求を取り出したあと、段が走る前に停止の合図が来た（`NotRun`）
        let (ops, record, gate) = run_release_with(
            "ops-release-not-run",
            |stop| stop.stop(),
            || panic!("停止後なので段は走らない"),
        );
        assert_eq!(record.outcome, Outcome::Skipped);
        assert_eq!(release_stage_results(&record), [("index_drive", "not_run")]);
        assert!(ops.board.snapshot().running.is_none());
        assert_eq!(gate.runs(), 1, "定期の段は走らない");
    }

    #[test]
    fn a_failing_drive_release_stage_is_failed_and_leaves_running() {
        let (ops, record, _gate) = run_release_with(
            "ops-release-failed",
            |_| {},
            || Err(StageError::fixed("secret-failure")),
        );
        assert_eq!(record.outcome, Outcome::Failed);
        assert_eq!(release_stage_results(&record), [("index_drive", "failed")]);
        assert!(ops.board.snapshot().running.is_none());
        let json = ops.board.snapshot().to_json().to_string();
        assert!(!json.contains("secret"), "失敗の文面は出さない: {json}");
        // 保留は消さない
        assert!(ops.mass_delete.current().is_some());
    }

    #[test]
    fn a_panicking_drive_release_stage_is_failed_and_the_worker_goes_on() {
        let (ops, record, _gate) =
            run_release_with("ops-release-panicked", |_| {}, || panic!("secret-panic"));
        assert_eq!(record.outcome, Outcome::Failed);
        assert_eq!(
            release_stage_results(&record),
            [("index_drive", "panicked")]
        );
        assert!(ops.board.snapshot().running.is_none());
        let json = ops.board.snapshot().to_json().to_string();
        assert!(!json.contains("secret"), "パニックの文面は出さない: {json}");
        // スレッドは続く: 次の要求を処理できる
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.wait_history(2);
        assert_eq!(ops.latest().outcome, Outcome::Done);
    }

    #[test]
    fn a_manual_sync_pushes_the_next_cycle_out_by_one_poll_but_other_requests_do_not() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ops = start_ops("ops-next-cycle", vec![Box::new(Tick(ticks.clone()))], LONG);
        ops.wait_first_cycle();
        assert_eq!(
            ops.board.snapshot().next_cycle_at,
            Some(ops.clock.stamp(LONG))
        );
        ops.clock.advance(Duration::from_secs(100));
        // 品質解析は周期を変えない
        ops.board.enqueue(OpKind::QualityScan, None, None).unwrap();
        ops.wait_history(1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            ops.board.snapshot().next_cycle_at,
            Some("t+3600s".to_string())
        );
        // 手動の同期は、今から poll 後にする
        ops.board.enqueue(OpKind::Sync, None, None).unwrap();
        ops.wait_history(2);
        ops.wait_for("周期の置き直し", |snap| {
            snap.next_cycle_at.as_deref() == Some("t+3700s")
        });
        // 続けて定期サイクルは走らない（起動直後の 1 回 + 同期の 1 回）
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
    }

    /// `counter` が `above` を超えるまで待つ（最大 10 秒）。
    fn wait_until_grows(counter: &AtomicUsize, above: usize) {
        for _ in 0..1000 {
            if counter.load(Ordering::SeqCst) > above {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("書き手の更新が進まない");
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
        // 書き手が終えた更新の回数（写しの開始から完了までに増えたことを確かめる）
        let updates = Arc::new(AtomicUsize::new(0));
        let writer = {
            let (path, stop, updates) = (path.clone(), Arc::clone(&stop), Arc::clone(&updates));
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
                    updates.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        // 書き手が動き出すまで待つ
        wait_until_grows(&updates, 0);
        let read = crate::db::open_read_only(&path).unwrap();
        let before: i64 = read
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        // 写しの途中（step の合間）ごとに、書き手が新しく更新を終えるのを待つ。これで「写しの最中に更新が
        // 続いていた」ことが、書き手と写しの速さによらず決まる
        let steps = Cell::new(0usize);
        let updates_at_start = updates.load(Ordering::SeqCst);
        let started = Instant::now();
        let copy = simulation_copy_stepping(&read, Duration::from_secs(20), &mut || {
            steps.set(steps.get() + 1);
            wait_until_grows(&updates, updates.load(Ordering::SeqCst));
        });
        let took = started.elapsed();
        let updates_at_end = updates.load(Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        let copy = copy.expect("更新され続けても完了する");
        assert!(
            steps.get() >= 2,
            "写しが 1 step で終わった: {}",
            steps.get()
        );
        assert!(
            updates_at_end >= updates_at_start + steps.get(),
            "写しのあいだに更新が続いていない: {updates_at_start} → {updates_at_end}（{} step）",
            steps.get()
        );
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
