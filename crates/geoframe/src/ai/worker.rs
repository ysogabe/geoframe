//! AI の背景ワーカー（P4 / T4.1c。設計は p4-design.md §4）。
//!
//! 処理の std スレッドが 1 本（ほかに、SIGTERM の直後に停止開始を書くだけの小さなスレッドが 1 本）、既存の writer（[`crate::writer::Writer`]）と `.ingest.lock`
//! （[`crate::ingest::LockHold`]）を共有して動く。**この版は用途別の実装を持たない**: 実際の AI 処理は
//! [`Handler`] を実装して [`production_handlers`] に登録する後続のタスク（T4.2 の Decision・T4.4 の画像
//! Embedding・T4.6 の位置候補）が足す。いまの配布バイナリの登録は空で、ワーカーは起動時の回収と停止の記録だけを
//! 行い、ジョブを 1 件も claim しない（fake の Provider・スタブの設定は置かない）。
//!
//! # 起動の条件
//!
//! `GEOFRAME_INGEST=1` かつ `GEOFRAME_AI_ENABLED=1` かつ有効な背景用途が 1 つ以上（[`should_start`]）。
//! `query_embedding` は利用者を待たせる対話の用途で、永続キューの背景ジョブではない。`INGEST=0` では
//! 何も起動せず、AI の表を作らず、ジョブの回収も Provider 停止の整理もしない（読み取り専用）。
//!
//! 起動時の準備（[`startup`]。ロックを取ったあと・`INGEST=1` のときだけ、ワーカーが無効でも）:
//! 1. 前の起動の `running` を回収する（[`super::runs::startup`]）。停止開始が記録された起動のものは数えず
//!    `deferred/shutdown`、それ以外（`SIGKILL`・クラッシュ）は `interruption_count` を加算して 3 回で `failed`。
//! 2. AI の設定が**有効**なときだけ、関係する設定が変わって効かなくなった Provider 停止を整理する
//!    （[`super::pause::release_unmatched`]）。設定が未指定・不正のときは何も消さない
//!    （現在の設定の識別が分からない状態で、既存の停止を一括で消さないため）。
//!
//! # 1 周期
//!
//! 起動時と周期（`GEOFRAME_AI_POLL_S`。最小 30 秒）ごとに:
//! 1. **reconcile**: 使える用途の handler ごとに、現在の写真・結果から導いたジョブのキー
//!    （[`Handler::reconcile`]）を積む（[`super::jobs::enqueue_batch`]。冪等で、別世代は `obsolete`）。
//! 2. **処理**: 使える登録済みの種類だけで [`super::jobs::claim_next`] し、writer の外で [`Handler::run`]、
//!    writer で確定（[`super::jobs::complete`]）または失敗の記録、を空になるまで繰り返す。
//!
//! 使えない種類は claim の対象から外すだけで、他の種類は進む: 未登録の種類・設定で無効（[`Disabled`]）・
//! Provider／用途の停止中・Provider 側の待ち（利用不可・429・予算切れ）・1 周期の上限に達した用途。
//! 未登録の種類のジョブは claim も成功化もされず `pending`/`deferred` のまま残る。
//!
//! # 所有・停止
//!
//! ジョブの claim は起動 ID とランダムな token を持ち、確定・失敗の記録は両方が合う `running` にだけ効く。
//! **SIGTERM を受けると共有の [`StopFlag`] が立つ**（[`crate::server::serve_until`]）。ワーカーは claim の前と
//! 外部へ送る直前（[`RunCtx::before_send`]）に確かめ、止まっていれば claim も送信もしない。待っている最中に
//! 届いた有効な応答は、ワーカーの join の間は token が有効なので確定される。停止の記録（停止開始・
//! 最後の戻し）は共通期限つきで writer へ頼み、writer が忙しくても期限を越えて待たない（[`AiWorker::shutdown`]）。
//! join の期限までに終わらない handler はスレッドごと残り（`LockHold` を持ったまま）、最後の戻しで
//! `deferred/shutdown`・token 失効となって、遅れて届いた応答は保存されない。
//!
//! # 秘匿
//!
//! ログとエラーは固定のコード・種類・件数だけ。対象キー・入力 hash・token・起動 ID・モデル・URL・パス・
//! 例外の本文は出さない。

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::budget::{self, Reservation};
use super::jobs::{
    self, Claimed, Completion, ErrorCode, Failure, InputCheck, JobKey, JobKind, MAX_DEFER_SECS,
    MIN_DEFER_SECS, Recheck,
};
use super::pause::{self, PauseReason};
use super::runs;
use super::settings::{AiConfig, Disabled, Gate, TaskKind};
use crate::config::Config;
use crate::ingest::LockHold;
use crate::stop::{StopFlag, join_within};
use crate::writer::{WriteError, Writer};

/// 時計（UTC の Unix 秒）。本番は [`jobs::unix_now`]、試験は偽の時計。
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// [`Handler::run`] が返す、結果の保存。writer の接続・トランザクションの中で呼ばれ（[`jobs::complete`]）、
/// **現在の入力の検査**（写真の世代・候補集合・処理版・手動 SAVE の有無）と結果の保存をして
/// [`InputCheck`] を返す。外部 I/O・長い処理をしない。
pub type Persist = Box<dyn FnOnce(&Connection) -> rusqlite::Result<InputCheck> + Send>;

/// 1 回の reconcile が writer を塞ぐ長さを区切る件数（1 トランザクションあたり）。
const ENQUEUE_CHUNK: usize = 200;
/// 背景の送信予算の予約が writer を待つ上限。過ぎたら送らず後回しにする。
const BUDGET_WAIT: Duration = Duration::from_secs(5);

/// 用途別の処理（後続のタスクが実装して [`production_handlers`] に登録する）。`kind` ごとに 1 つ。
///
/// - `reconcile`: 現在の写真・有効な結果から、**まだ結果の無い**対象のキーを導く。現在の結果表に
///   適合する答えがあるものはキーにしない（job 行の有無だけで再計算を決めない）。`input_hash`・
///   `processor_version` は結果を左右する入力・処理版だけから作り、URL・タイムアウト・許可は入れない。
/// - `run`: claim したジョブを writer の外で実行する。写真・入力は [`RunCtx::read`] で読み、
///   **外部へ送る直前に必ず [`RunCtx::before_send`]**（停止・ポリシー・Provider 停止・送信予算を再評価する）。
///   送る直前に DB の行・入力世代・hidden を読み直し、古ければ [`RunError::InputChanged`]。
/// - `recheck`: 手動の再試行（`geoframe ai-jobs retry`）で、`failed` のジョブを現在の入力・既存の結果に
///   照らして振り分ける。外部へは送らない。
pub trait Handler: Send + Sync {
    fn kind(&self) -> JobKind;
    fn reconcile(&self, read: &Connection, now: i64) -> rusqlite::Result<Vec<JobKey>>;
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError>;
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck>;
}

/// 配布バイナリに登録する handler。T4.2・T4.4・T4.6 がここへ足す。**いまは空**（実 AI 処理も fake も無い）。
pub fn production_handlers() -> Vec<Arc<dyn Handler>> {
    Vec::new()
}

/// ジョブの種類を使う設定上の用途。`location_geocode` は LLM の routing を持たず、位置候補（`location_suggest`）の
/// 後続なので、その用途の有効・無効・停止・上限に従う。
pub fn task_for(kind: JobKind) -> TaskKind {
    match kind {
        JobKind::ImageEmbedding => TaskKind::ImageEmbedding,
        JobKind::LocationClue => TaskKind::LocationClue,
        JobKind::LocationSuggest | JobKind::LocationGeocode => TaskKind::LocationSuggest,
        JobKind::PhotoJudge => TaskKind::PhotoJudge,
    }
}

/// 背景ジョブの用途（`query_embedding` は対話用で含めない）。
pub fn background_tasks(ai: &AiConfig, has_writer: bool) -> Vec<TaskKind> {
    ai.enabled_tasks(has_writer)
        .into_iter()
        .filter(|t| *t != TaskKind::QueryEmbedding)
        .collect()
}

/// ワーカーを起動するか（上の「起動の条件」）。
pub fn should_start(config: &Config) -> bool {
    config.ingest && config.ai.legacy().enabled && !background_tasks(&config.ai, true).is_empty()
}

// ---------------------------------------------------------------- 実行の文脈・エラー

/// [`Handler::run`] が失敗したときの固定の分類（例外の本文・URL・応答は持たない）。ワーカーがジョブの状態・
/// 待ち時間・Provider 停止へ対応付ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// 停止中（送っていない）。ジョブは `deferred/shutdown` に戻る。失敗に数えない。
    Stopping,
    /// 送る前に入力が古いと分かった。ジョブは `obsolete`（新しい入力は別のジョブ）。
    InputChanged,
    /// 設定・許可が今は送信を許さない（[`Disabled`]。後続の handler は `Disabled` をこれに対応付ける）。
    /// 失敗に数えず後回し。
    PolicyDisabled,
    /// Provider・用途が停止中（[`pause`]）。失敗に数えず後回し。
    Paused,
    /// 送信予算の予約が通らなかった（[`RunCtx::before_send`] が返す）。失敗に数えず後回し。時計の巻き戻しも
    /// 固定の理由（`clock_rewound`）として残り、予算をリセットする救済は無い。
    Budget(Reservation),
    /// Provider の停止・GPU lease・到達不能（写真の不良ではない）。失敗に数えず後回し。その用途を一定時間（60 秒）送らない。
    Unavailable,
    /// 429。`retry_after_secs` を 60〜3600 秒に収めて後回し。その用途をその間送らない。
    RateLimited { retry_after_secs: Option<u32> },
    /// 失敗 1 回（応答構造の不正・クラウドへの送信後の時間切れ／5xx など課金され得る試行）。5 回で `failed`。
    Attempt(ErrorCode),
    /// Provider は使えるのに、同じジョブだけが反復して失敗する（5xx・時間切れ）。
    Repeated(ErrorCode),
    /// そのジョブだけの恒久的な失敗（入力サイズ超過・不正）。即 `failed`。後続のジョブは進む。
    Permanent(ErrorCode),
    /// 認証・契約の不一致などで、Provider 全体（同じ接続先・認証の全用途）を停止する。ジョブは失敗に数えない。
    ProviderPause(PauseReason),
    /// モデル 404 などで、この用途だけを停止する。ジョブは失敗に数えない。
    TaskPause(PauseReason),
}

/// [`Handler::run`] に渡す文脈。接続情報は [`RunCtx::before_send`] が通ったときだけ渡される。
pub struct RunCtx<'a> {
    read: &'a Connection,
    config: &'a AiConfig,
    task: TaskKind,
    stop: &'a StopFlag,
    writer: &'a Writer,
    clock: &'a Clock,
}

impl<'a> RunCtx<'a> {
    /// 読み取り専用の接続（写真・結果・入力世代の読み取り）。トランザクションを長く持たない。
    pub fn read(&self) -> &Connection {
        self.read
    }

    pub fn task(&self) -> TaskKind {
        self.task
    }

    /// 停止の合図が立っているか。長い準備（画像の縮小など）の合間に見て、早く戻る。
    pub fn is_stopping(&self) -> bool {
        self.stop.is_stopped()
    }

    /// 外部へ送る**直前**に呼ぶ（送信のたびに）。順に確かめ、全部通ったときだけ接続情報を返す:
    /// 1. 停止の合図（[`RunError::Stopping`]）
    /// 2. 今の設定の送信可否（[`RunError::PolicyDisabled`]）
    /// 3. Provider・用途の停止（[`RunError::Paused`]）
    /// 4. クラウドの用途は送信予算の予約（[`RunError::Budget`]。writer を [`BUDGET_WAIT`] まで待つ）。
    ///    予約は失敗・中断しても巻き戻らない。
    pub fn before_send(&self) -> Result<Gate<'a>, RunError> {
        if self.stop.is_stopped() {
            return Err(RunError::Stopping);
        }
        let gate = self
            .config
            .gate(self.task, true)
            .map_err(|_: Disabled| RunError::PolicyDisabled)?;
        if let Some(settings) = self.config.settings() {
            match pause::status(self.read, settings, self.task) {
                Ok(None) => {}
                // 読めないときは送らない（停止中かもしれない）
                Ok(Some(_)) | Err(_) => return Err(RunError::Paused),
            }
        }
        if let Some(send_budget) = gate.budget {
            let task = self.task;
            let clock = Arc::clone(self.clock);
            let deadline = Instant::now() + BUDGET_WAIT;
            // 時刻は writer の依頼の中で取る（順番待ちのあと）
            let reservation = match self.writer.call_within(deadline, move |conn| {
                budget::reserve(conn, task, &send_budget, clock())
            }) {
                Ok(reservation) => reservation,
                Err(WriteError::Expired) => Reservation::Busy,
                Err(_) => Reservation::Failed,
            };
            if !reservation.is_granted() {
                return Err(RunError::Budget(reservation));
            }
        }
        Ok(gate)
    }
}

// ---------------------------------------------------------------- 起動時の準備

/// 起動時の準備（上の「起動の条件」の下の 1・2）の結果。
#[derive(Clone, PartialEq, Eq)]
pub struct Started {
    /// この起動の ID。ワーカーが claim に付ける（ログ・出力・`Debug` には出さない）。
    pub run_id: String,
}

impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started").finish_non_exhaustive()
    }
}

/// 起動の準備。**ロックを取ったあと**（[`crate::ingest::start`] のあと）に 1 回だけ呼ぶ。失敗したら
/// ワーカーを起動しない（`None`）。件数だけをログに出す。
pub fn startup(config: &Config, writer: &Writer, clock: &Clock) -> Option<Started> {
    let run_id = match jobs::random_id() {
        Ok(id) => id,
        Err(_) => {
            tracing::error!("[ai] 起動 ID を作れないので AI の回収・ワーカーを始めません");
            return None;
        }
    };
    let now = clock();
    let id = run_id.clone();
    match writer.call(move |conn| runs::startup(conn, &id, now)) {
        Ok(report) => {
            let n = report.interrupted;
            if report.planned + n.requeued + n.failed > 0 {
                tracing::info!(
                    "[ai] 前の起動のジョブを回収しました: 停止中だったもの {} / 中断 {} / 中断の上限で失敗 {}",
                    report.planned,
                    n.requeued,
                    n.failed
                );
            }
        }
        Err(e) => {
            tracing::error!("[ai] 前の起動のジョブを回収できません（AI を始めません）: {e}");
            return None;
        }
    }
    if let Some(settings) = config.ai.settings() {
        let keys: Vec<_> = TaskKind::ALL
            .into_iter()
            .flat_map(|t| [settings.provider_pause_key(t), settings.task_pause_key(t)])
            .flatten()
            .collect();
        match writer.call(move |conn| pause::release_unmatched(conn, &keys)) {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                "[ai] 関係する設定が変わって効かなくなった Provider 停止を {n} 件整理しました"
            ),
            Err(e) => tracing::warn!("[ai] Provider 停止の整理に失敗しました: {e}"),
        }
    }
    Some(Started { run_id })
}

// ---------------------------------------------------------------- ワーカー

/// ワーカーに渡すもの。
pub struct WorkerDeps {
    pub config: Arc<Config>,
    pub writer: Writer,
    pub stop: StopFlag,
    /// ワーカーのスレッドが終わるまで `.ingest.lock` を手放さない。
    pub lock: LockHold,
    pub handlers: Vec<Arc<dyn Handler>>,
    pub clock: Clock,
    /// 周期（試験は短くする。本番は `config.ai.legacy().poll()`）。
    pub poll: Duration,
    pub run_id: String,
}

/// 周期の処理本体（スレッドの外から 1 周期ずつ動かせる。試験用）。
pub(super) struct Core {
    config: Arc<Config>,
    writer: Writer,
    stop: StopFlag,
    handlers: Vec<Arc<dyn Handler>>,
    clock: Clock,
    run_id: String,
    /// Provider 側の都合（利用不可・429・予算切れ）で、この時刻（Unix 秒）まで送らない用途。
    blocked: HashMap<TaskKind, i64>,
}

impl Core {
    pub(super) fn new(deps: &WorkerDeps) -> Self {
        Self {
            config: Arc::clone(&deps.config),
            writer: deps.writer.clone(),
            stop: deps.stop.clone(),
            handlers: deps.handlers.clone(),
            clock: Arc::clone(&deps.clock),
            run_id: deps.run_id.clone(),
            blocked: HashMap::new(),
        }
    }

    /// 1 周期: reconcile のあと、使える種類のジョブが無くなる（または停止・上限）まで処理する。
    pub(super) fn cycle(&mut self) {
        if self.handlers.is_empty() || self.stop.is_stopped() {
            return;
        }
        let read = match crate::db::open_read_only(&self.config.db_path()) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!("[ai] DB を読み取り用に開けません（次の周期に持ち越します）: {e}");
                return;
            }
        };
        self.reconcile(&read);
        let mut processed: HashMap<TaskKind, u32> = HashMap::new();
        loop {
            // claim の直前に毎回確かめる（止まったら新しい claim をしない）
            if self.stop.is_stopped() {
                return;
            }
            let kinds = self.usable_kinds(&read, &processed);
            if kinds.is_empty() {
                return;
            }
            let (run_id, now) = (self.run_id.clone(), (self.clock)());
            let token = match jobs::random_id() {
                Ok(token) => token,
                Err(_) => {
                    tracing::error!("[ai] claim の token を作れません（次の周期に持ち越します）");
                    return;
                }
            };
            let claimed = match self
                .writer
                .call(move |conn| jobs::claim_next(conn, now, &run_id, &token, &kinds))
            {
                Ok(Some(claimed)) => claimed,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("[ai] ジョブを claim できません（次の周期に持ち越します）: {e}");
                    return;
                }
            };
            let task = task_for(claimed.key.kind);
            *processed.entry(task).or_insert(0) += 1;
            self.process(&read, claimed, task);
        }
    }

    /// 使える登録済みの種類（上の「1 周期」の「使えない種類」を除いたもの）。
    fn usable_kinds(&self, read: &Connection, processed: &HashMap<TaskKind, u32>) -> Vec<JobKind> {
        let now = (self.clock)();
        self.handlers
            .iter()
            .map(|h| h.kind())
            .filter(|kind| {
                let task = task_for(*kind);
                let Ok(gate) = self.config.ai.gate(task, true) else {
                    return false;
                };
                if processed.get(&task).copied().unwrap_or(0) >= gate.cycle_limit {
                    return false;
                }
                if self.blocked.get(&task).is_some_and(|until| *until > now) {
                    return false;
                }
                match self.config.ai.settings() {
                    Some(settings) => matches!(pause::status(read, settings, task), Ok(None)),
                    None => false,
                }
            })
            .collect()
    }

    fn handler(&self, kind: JobKind) -> Option<Arc<dyn Handler>> {
        self.handlers.iter().find(|h| h.kind() == kind).cloned()
    }

    fn reconcile(&self, read: &Connection) {
        let now = (self.clock)();
        for handler in &self.handlers {
            if self.stop.is_stopped() {
                return;
            }
            let kind = handler.kind();
            // 設定で許可されない用途は積まない（無効の理由は状態の集計に出る）
            if self.config.ai.gate(task_for(kind), true).is_err() {
                continue;
            }
            let keys = match catch_unwind(AssertUnwindSafe(|| handler.reconcile(read, now))) {
                Ok(Ok(keys)) => keys,
                Ok(Err(e)) => {
                    tracing::warn!("[ai] {} の reconcile に失敗しました: {e}", kind.as_str());
                    continue;
                }
                Err(_) => {
                    tracing::error!("[ai] {} の reconcile がパニックしました", kind.as_str());
                    continue;
                }
            };
            let keys: Vec<JobKey> = keys.into_iter().filter(|k| k.kind == kind).collect();
            let (mut created, mut revived) = (0, 0);
            for chunk in keys.chunks(ENQUEUE_CHUNK) {
                if self.stop.is_stopped() {
                    break;
                }
                let (chunk, clock) = (chunk.to_vec(), Arc::clone(&self.clock));
                match self
                    .writer
                    .call(move |conn| jobs::enqueue_batch(conn, &chunk, clock()))
                {
                    Ok(done) => {
                        created += done.created;
                        revived += done.revived;
                    }
                    Err(e) => {
                        tracing::warn!("[ai] ジョブを積めません: {e}");
                        break;
                    }
                }
            }
            if created + revived > 0 {
                tracing::info!(
                    "[ai] {} のジョブを積みました: 新規 {created} / 復活 {revived}",
                    kind.as_str()
                );
            }
        }
    }

    /// claim した 1 件を処理する。必ず確定・失敗の記録・後回しのどれかへ進める。
    fn process(&mut self, read: &Connection, claimed: Claimed, task: TaskKind) {
        // claim の直後に止まったときは、実行せず戻す
        if self.stop.is_stopped() {
            self.record(
                &claimed,
                ErrorCode::Shutdown,
                Failure::Defer {
                    retry_after_secs: None,
                },
            );
            return;
        }
        let Some(handler) = self.handler(claimed.key.kind) else {
            // claim の対象は登録済みの種類だけ。ここには来ないが、来ても成功化せず戻す
            self.record(
                &claimed,
                ErrorCode::Internal,
                Failure::Defer {
                    retry_after_secs: None,
                },
            );
            return;
        };
        let outcome = {
            let ctx = RunCtx {
                read,
                config: &self.config.ai,
                task,
                stop: &self.stop,
                writer: &self.writer,
                clock: &self.clock,
            };
            catch_unwind(AssertUnwindSafe(|| handler.run(&ctx, &claimed)))
        };
        match outcome {
            Ok(Ok(persist)) => self.complete(&claimed, persist),
            Ok(Err(error)) => self.apply_error(&claimed, task, error),
            Err(_) => {
                // panic は当該ジョブの失敗（5 回で failed）。同じジョブが全 AI を塞ぐ無限ループにしない
                tracing::error!(
                    "[ai] {} の処理がパニックしました",
                    claimed.key.kind.as_str()
                );
                self.record(&claimed, ErrorCode::Panic, Failure::Attempt);
            }
        }
    }

    fn complete(&self, claimed: &Claimed, persist: Persist) {
        let (c, clock) = (claimed.clone(), Arc::clone(&self.clock));
        match self
            .writer
            .call(move |conn| jobs::complete(conn, &c, clock(), persist))
        {
            Ok(Completion::Succeeded) => {}
            Ok(Completion::Obsolete) => tracing::info!(
                "[ai] {} のジョブは入力が変わっていたので結果を捨てました",
                claimed.key.kind.as_str()
            ),
            Ok(Completion::Rejected) => tracing::info!(
                "[ai] {} のジョブの応答は無効になった claim のものなので保存しません",
                claimed.key.kind.as_str()
            ),
            Err(e) => {
                tracing::warn!("[ai] 結果を確定できません: {e}");
                // 結果の保存の失敗は当該ジョブの失敗（成功だけが残ることは無い）
                self.record(claimed, ErrorCode::Internal, Failure::Attempt);
            }
        }
    }

    fn record(&self, claimed: &Claimed, code: ErrorCode, failure: Failure) {
        let (c, clock) = (claimed.clone(), Arc::clone(&self.clock));
        if let Err(e) = self
            .writer
            .call(move |conn| jobs::record_failure(conn, &c, clock(), code, failure))
        {
            // running のまま残り、次の起動の回収が数える
            tracing::warn!("[ai] 失敗を記録できません: {e}");
        }
    }

    /// [`RunError`] をジョブの状態・Provider 側の待ち・停止へ対応付ける。
    fn apply_error(&mut self, claimed: &Claimed, task: TaskKind, error: RunError) {
        let now = (self.clock)();
        let defer = |code, secs: Option<u32>| {
            (
                code,
                Failure::Defer {
                    retry_after_secs: secs,
                },
            )
        };
        let (code, failure, block) = match error {
            RunError::Stopping => {
                let (code, f) = defer(ErrorCode::Shutdown, None);
                (code, f, None)
            }
            RunError::InputChanged => {
                let (c, clock) = (claimed.clone(), Arc::clone(&self.clock));
                if let Err(e) = self
                    .writer
                    .call(move |conn| jobs::mark_obsolete(conn, &c, clock()))
                {
                    tracing::warn!("[ai] ジョブを無効にできません: {e}");
                }
                return;
            }
            RunError::PolicyDisabled => {
                let (code, f) = defer(ErrorCode::PolicyDisabled, None);
                (code, f, None)
            }
            RunError::Paused => {
                let (code, f) = defer(ErrorCode::ProviderPaused, None);
                (code, f, None)
            }
            RunError::Budget(reservation) => {
                let (code, secs) = budget_wait(reservation, self.cycle_secs(task));
                let (code, f) = defer(code, Some(secs));
                (code, f, Some(i64::from(secs)))
            }
            RunError::Unavailable => {
                let (code, f) = defer(ErrorCode::Unavailable, None);
                (code, f, Some(i64::from(MIN_DEFER_SECS)))
            }
            RunError::RateLimited { retry_after_secs } => {
                let secs = retry_after_secs
                    .unwrap_or(MIN_DEFER_SECS)
                    .clamp(MIN_DEFER_SECS, MAX_DEFER_SECS);
                let (code, f) = defer(ErrorCode::RateLimited, Some(secs));
                (code, f, Some(i64::from(secs)))
            }
            RunError::Attempt(code) => (code, Failure::Attempt, None),
            RunError::Repeated(code) => (code, Failure::Repeated, None),
            RunError::Permanent(code) => (code, Failure::Permanent, None),
            RunError::ProviderPause(reason) | RunError::TaskPause(reason) => {
                let provider = matches!(error, RunError::ProviderPause(_));
                self.pause_and_defer(claimed, task, reason, provider);
                return;
            }
        };
        if let Some(secs) = block {
            self.blocked.insert(task, now + secs);
        }
        self.record(claimed, code, failure);
    }

    fn cycle_secs(&self, task: TaskKind) -> u32 {
        self.config.ai.legacy().cycle(task).1
    }

    /// 停止を DB に残し、ジョブは失敗に数えず後回しにする（1 回の依頼）。
    fn pause_and_defer(
        &self,
        claimed: &Claimed,
        task: TaskKind,
        reason: PauseReason,
        provider: bool,
    ) {
        let key = self.config.ai.settings().and_then(|s| {
            if provider {
                s.provider_pause_key(task)
            } else {
                s.task_pause_key(task)
            }
        });
        let (c, clock) = (claimed.clone(), Arc::clone(&self.clock));
        let result = self.writer.call(move |conn| {
            let now = clock();
            if let Some(key) = &key {
                pause::pause(conn, task, key, reason, now)?;
            }
            jobs::record_failure(
                conn,
                &c,
                now,
                ErrorCode::ProviderPaused,
                Failure::Defer {
                    retry_after_secs: None,
                },
            )
        });
        match result {
            Ok(_) => tracing::warn!(
                "[ai] {} を停止しました（理由: {}）。設定の修正か geoframe ai-jobs release-pause で解除します",
                task.as_str(),
                reason.as_str()
            ),
            Err(e) => tracing::warn!("[ai] 停止を記録できません: {e}"),
        }
    }
}

/// 送信予算の予約が通らなかったときの、固定の理由と待つ秒数（60〜3600 秒）。
fn budget_wait(reservation: Reservation, cycle_secs: u32) -> (ErrorCode, u32) {
    let clamp = |s: u32| s.clamp(MIN_DEFER_SECS, MAX_DEFER_SECS);
    match reservation {
        Reservation::DailyExhausted => (ErrorCode::BudgetExhausted, MAX_DEFER_SECS),
        Reservation::CycleExhausted => (ErrorCode::BudgetExhausted, clamp(cycle_secs)),
        Reservation::ClockRewound => (ErrorCode::ClockRewound, clamp(600)),
        // 順番待ちの超過・writer なし・DB の失敗: 予算は確かめられなかった（送らない）
        Reservation::Busy | Reservation::NoWriter | Reservation::Failed | Reservation::Granted => {
            (ErrorCode::Internal, MIN_DEFER_SECS)
        }
    }
}

/// 起動したワーカー（スレッド 2 本: 周期の処理と、停止開始の記録）。
pub struct AiWorker {
    thread: JoinHandle<()>,
    /// 停止開始を記録するスレッド（SIGTERM の直後に 1 回書いて終わる）。
    marker: JoinHandle<()>,
    run_id: String,
}

/// ワーカーを起動する。呼ぶ前に [`startup`] を済ませること。
pub fn spawn(deps: WorkerDeps) -> AiWorker {
    let run_id = deps.run_id.clone();
    // 停止開始の記録: SIGTERM の直後に書く（handler で待っているワーカーのスレッドには頼れない）。
    // 共通期限つきで、writer が忙しくても期限を越えて待たない
    let marker = {
        let (writer, stop, clock, id) = (
            deps.writer.clone(),
            deps.stop.clone(),
            Arc::clone(&deps.clock),
            deps.run_id.clone(),
        );
        let lock = deps.lock.clone();
        std::thread::Builder::new()
            .name("ai-stop-mark".into())
            .spawn(move || {
                let _lock = lock;
                while !stop.wait(Duration::from_secs(3600)) {}
                let deadline = stop.deadline().unwrap_or_else(Instant::now);
                match writer.call_within(deadline, move |conn| {
                    runs::mark_stop_requested(conn, &id, clock())
                }) {
                    Ok(()) => {}
                    Err(e) => tracing::warn!("[ai] 停止開始を記録できません: {e}"),
                }
            })
            .expect("AI の停止記録のスレッドを起動できません")
    };
    let thread = std::thread::Builder::new()
        .name("ai-worker".into())
        .spawn(move || {
            // スレッドが終わるまで `.ingest.lock` を手放さない（呼び出し側が先に終わっても）
            let _lock = deps.lock.clone();
            let poll = deps.poll;
            let stop = deps.stop.clone();
            let mut core = Core::new(&deps);
            loop {
                if catch_unwind(AssertUnwindSafe(|| core.cycle())).is_err() {
                    tracing::error!("[ai] 周期の処理がパニックしました（次の周期に持ち越します）");
                }
                if stop.wait(poll) {
                    break;
                }
            }
        })
        .expect("AI ワーカーのスレッドを起動できません");
    AiWorker {
        thread,
        marker,
        run_id,
    }
}

impl AiWorker {
    /// 停止する（呼ぶ前に `stop` の合図を出しておくこと）。スレッドの終了を `join_cap` と共通期限の残りのうち
    /// 短いほどまで待ち（この間 token は有効で、有効な応答は確定される）、そのあと**最後の戻し**
    /// （[`jobs::release_for_shutdown`]）を共通期限つきで writer へ頼む。writer が忙しくても期限を越えて待たない。
    /// 終わらなかったスレッドは残り、`LockHold` をプロセスの終了まで持つ（2 つ目の書き手を作らない）。
    /// スレッドが終わったかを返す。
    pub fn shutdown(
        self,
        writer: &Writer,
        stop: &StopFlag,
        join_cap: Duration,
        clock: &Clock,
    ) -> bool {
        let joined = join_within(self.thread, stop.phase_limit(join_cap));
        // 停止開始の記録（共通期限つきの依頼なので、遅くとも期限で終わる）。プロセスの終了で書き損じない
        join_within(self.marker, stop.phase_limit(join_cap));
        if !joined {
            tracing::warn!(
                "[ai] ワーカーのスレッドが時間内に止まりませんでした（実行中のジョブは戻してそのまま終了します）"
            );
        }
        let deadline = stop.deadline().unwrap_or_else(|| Instant::now() + join_cap);
        let (run_id, clock) = (self.run_id, Arc::clone(clock));
        match writer.call_within(deadline, move |conn| {
            jobs::release_for_shutdown(conn, &run_id, clock())
        }) {
            Ok(0) => {}
            Ok(n) => tracing::info!("[ai] 実行中だったジョブ {n} 件を停止として戻しました"),
            Err(e) => {
                tracing::warn!("[ai] 実行中のジョブを戻せません（次の起動の回収に任せます）: {e}")
            }
        }
        joined
    }
}
