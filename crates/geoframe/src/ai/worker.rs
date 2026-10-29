//! AI の背景ワーカー（P4 / T4.1c。設計は p4-design.md §4）。
//!
//! 処理の std スレッドが 1 本（ほかに、SIGTERM の直後に停止開始を書くだけの小さなスレッドが 1 本）、既存の writer（[`crate::writer::Writer`]）と `.ingest.lock`
//! （[`crate::ingest::LockHold`]）を共有して動く。T4.6b/c2 で location_clue / location_suggest / location_geocode を本番登録した。
//! T4.4a で明示契約の local image_embedding を登録した。T4.5c で photo_judge（任意の Gemini 比較）を、用途の許可・クラウド接続・場面計算が揃うときだけ登録する（揃わなければ未登録で claim しない。fake の Provider・任意宛先の設定は置かない）。
//!
//! # 起動の条件
//!
//! `GEOFRAME_INGEST=1` かつ `GEOFRAME_AI_ENABLED=1` かつ AI の設定が不正でない（[`should_start`]）。
//! 起動するスレッドは 2 種類で、どちらも Provider や外部 I/O を使わない順に独立している:
//! - `ai-scenes`: 同一場面の局所計算（[`super::groups::runtime`]）。保存済みの画像 vector だけを使い、
//!   Provider の設定・用途の有無に関係なく動く。
//! - `ai-worker`: 有効な背景用途（Provider 経由のジョブ）の周期。背景用途が 0 件なら周期は何もしない。
//!
//! `query_embedding` は利用者を待たせる対話の用途で、永続キューの背景ジョブではない。`INGEST=0` では
//! 何も起動せず、AI の表を作らず、ジョブの回収も Provider 停止の整理もしない（読み取り専用）。
//!
//! 起動時の準備（[`startup`]。ロックを取ったあと・`INGEST=1` のときだけ、ワーカーが無効
//! （`GEOFRAME_AI_ENABLED=0` を含む）でも）:
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
use super::provider::LanClient;
use super::runs;
use super::settings::{AiConfig, Disabled, Gate, TaskKind};
use super::waits::{self, Wait, WaitKey, WaitReason, WaitScope};
use crate::config::Config;
use crate::datadir::DataDir;
use crate::ingest::LockHold;
use crate::stop::{StopFlag, join_within};
use crate::writer::{WriteError, Writer};

/// 時計（UTC の Unix 秒）。本番は [`jobs::unix_now`]、試験は偽の時計。
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// [`Handler::run`] が返す、結果の保存。writer の接続・トランザクションの中で呼ばれ（[`jobs::complete`]）、
/// **現在の入力の検査**（写真の世代・候補集合・処理版・手動 SAVE の有無）と結果の保存をして
/// [`InputCheck`] を返す。外部 I/O・長い処理をしない。
pub type Persist = Box<dyn FnOnce(&Connection) -> rusqlite::Result<InputCheck> + Send>;

/// Local writer operation for publication; contains no network or image work.
pub type Maintenance = Box<dyn FnOnce(&mut Connection) -> rusqlite::Result<()> + Send>;

/// 1 回の reconcile が writer を塞ぐ長さを区切る件数（1 トランザクションあたり）。
const ENQUEUE_CHUNK: usize = 200;
/// 背景の送信予算の予約が writer を待つ上限。過ぎたら送らず後回しにする。
const BUDGET_WAIT: Duration = Duration::from_secs(5);

/// 用途別の処理（後続のタスクが実装して [`production_handlers`] に登録する）。`kind` ごとに 1 つ。
///
/// - `reconcile`: 現在の写真・有効な結果から対象のキーを導く。現在の結果表に
///   適合する答えは再利用する（成功キーを渡しても enqueue は再実行しない）。`input_hash`・
///   `processor_version` は結果を左右する入力・処理版だけから作り、URL・タイムアウト・許可は入れない。
/// - `run`: claim したジョブを writer の外で実行する。写真・入力は [`RunCtx::read`] で読み、
///   **外部へ送る直前に必ず [`RunCtx::before_send`]**（停止・ポリシー・Provider 停止・送信予算を再評価する）。
///   送る直前に DB の行・入力世代・hidden を読み直し、古ければ [`RunError::InputChanged`]。
/// - `recheck`: 手動の再試行（`geoframe ai-jobs retry`）で、`failed` のジョブを現在の入力・既存の結果に
///   照らして振り分ける。外部へは送らない。
pub trait Handler: Send + Sync {
    fn kind(&self) -> JobKind;
    fn claim_scope(&self) -> jobs::ClaimScope {
        jobs::ClaimScope {
            kind: self.kind(),
            target_prefix: None,
            processor_version: None,
        }
    }
    fn reconcile(&self, read: &Connection, now: i64) -> rusqlite::Result<Vec<JobKey>>;
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError>;
    /// retry のファイル由来検査は writer の外で準備する。通信はしない。
    fn prepare_retry(&self) -> rusqlite::Result<()> {
        Ok(())
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck>;
    /// 対象から外れた写真等の旧ジョブを失効する短い writer 操作（現世代を操作内でも再検査）。
    fn reconcile_cleanup(&self, _read: &Connection, _now: i64) -> rusqlite::Result<Vec<Persist>> {
        Ok(Vec::new())
    }
    /// Local writer maintenance after reconciliation and every completion/failure.
    fn maintenance(&self, _read: &Connection, _now: i64) -> rusqlite::Result<Option<Maintenance>> {
        Ok(None)
    }
    /// DB 外で投影の準備をし、短い writer transaction 内で現世代・terminal 状態を照合する。
    /// terminal failure と起動回収後の修復で共有する。外部 I/O はこの準備側だけで行う。
    fn projection(
        &self,
        _read: &Connection,
        _key: &JobKey,
        _now: i64,
    ) -> rusqlite::Result<Option<Persist>> {
        Ok(None)
    }
}

/// プロセスで一度作った依存を注入する。GETとAIのGeocoder/cacheを分割しない。
pub fn production_handlers(
    config: &Config,
    lan: Option<LanClient>,
    cloud: Option<&super::cloud::CloudClients>,
    geocoder: Arc<crate::geocode::Geocoder>,
    cache: Arc<crate::cache::Cache>,
) -> Vec<Arc<dyn Handler>> {
    if !config.ingest {
        return Vec::new();
    }
    let Ok(data) = DataDir::open(&config.data_dir) else {
        return Vec::new();
    };
    let mut handlers = Vec::new();
    if let Some(client) = lan.clone().and_then(|l| {
        super::embedding::EmbeddingClient::new(&config.ai, l, TaskKind::ImageEmbedding)
    }) {
        handlers.push(super::image_embedding::handler(data.clone(), client));
    }

    if let (Ok(gate), Some(lan)) = (config.ai.gate(TaskKind::LocationClue, true), lan.clone()) {
        handlers.push(super::location_clue::handler(
            data.clone(),
            lan,
            gate.settings.model.clone(),
        ));
    }
    // 任意の Gemini 比較。用途の許可・クラウド接続・場面計算が揃ったときだけ登録される（無ければ claim されない）。
    handlers.extend(super::scene_judge::handler(config, data.clone(), cloud));
    handlers.extend(super::location_suggest::handlers(
        config, data, lan, cloud, geocoder, cache,
    ));
    handlers
}

#[cfg(test)]
pub(super) fn production_handlers_for_test(
    config: &Config,
    lan: Option<LanClient>,
) -> Vec<Arc<dyn Handler>> {
    let cloud = super::cloud::CloudClients::new(&config.ai).ok();
    production_handlers(
        config,
        lan,
        cloud.as_ref(),
        Arc::new(crate::geocode::Geocoder::production(&config.tile_ua)),
        Arc::new(crate::cache::Cache::memory_only()),
    )
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
    super::groups::runtime::enabled(config)
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
    /// 現在の品質特徴量の解析待ち。その写真だけ後回し、他の写真は進める。
    InputPending,
    /// 依存サービスだけの待ち。Cloud Provider の認証停止へ波及させない。
    DependencyWait(ErrorCode),
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
    /// 同じ Provider を使う用途で**共有する一時待機**（到達不能・GPU lease・429・サービスが未準備）。失敗に数えず後回し
    /// （ジョブの `next_attempt_at` も遅らせる）。範囲は [`WaitScope`]: 入口全体なら同じ Provider の他の用途も claim の前に
    /// 飛ばし、サービス単体ならそのサービスを使う用途だけ。別の Provider は巻き込まない。DB に残り、再起動後も効く
    /// （[`super::waits`]）。永続の停止（[`RunError::ProviderPause`]）とは別。
    ProviderWait(Wait),
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
    #[cfg(test)]
    pub(super) fn for_test(
        read: &'a Connection,
        config: &'a AiConfig,
        task: TaskKind,
        stop: &'a StopFlag,
        writer: &'a Writer,
        clock: &'a Clock,
    ) -> Self {
        Self {
            read,
            config,
            task,
            stop,
            writer,
            clock,
        }
    }

    pub(super) fn config(&self) -> &AiConfig {
        self.config
    }

    /// 読み取り専用の接続（写真・結果・入力世代の読み取り）。トランザクションを長く持たない。
    pub fn read(&self) -> &Connection {
        self.read
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    pub fn task(&self) -> TaskKind {
        self.task
    }

    /// 停止の合図が立っているか。長い準備（画像の縮小など）の合間に見て、早く戻る。
    pub fn is_stopping(&self) -> bool {
        self.stop.is_stopped()
    }

    /// 停止の合図（外部 I/O の枠を取る・待つときに渡す）。
    pub fn stop(&self) -> &StopFlag {
        self.stop
    }

    /// 外部へ送る**直前**に呼ぶ（送信のたびに）。順に確かめ、全部通ったときだけ接続情報を返す:
    /// 1. 停止の合図（[`RunError::Stopping`]）
    /// 2. 今の設定の送信可否（[`RunError::PolicyDisabled`]）
    /// 3. Provider・用途の停止（[`RunError::Paused`]）
    /// 4. クラウドの用途は送信予算の予約（[`RunError::Budget`]。writer を [`BUDGET_WAIT`] まで待つ）。
    ///    予約は失敗・中断しても巻き戻らない。
    /// 5. 許可を返す直前にもう一度、停止の合図（予約の順番待ちのあいだに来た合図も尊重する）。
    pub fn before_send(&self) -> Result<Gate<'a>, RunError> {
        self.check(true, None)
    }

    /// cloud の送信予約は、writer の既定待ち上限と用途全体期限の短い方まで。
    pub(super) fn before_send_until(
        &self,
        deadline: &super::http::Deadline,
    ) -> Result<Gate<'a>, RunError> {
        self.check(true, Some(deadline))
    }

    /// Discovery などの**読み取り**の前に呼ぶ。[`RunCtx::before_send`] と同じ確認のうち、クラウドの送信枠の予約
    /// （手順 4）だけをしない。推論の要求を出す前の確認で送信枠を使わないための区別。
    pub fn before_probe(&self) -> Result<Gate<'a>, RunError> {
        self.check(false, None)
    }

    /// 重い準備（画像のデコードなど）の前の足切り。[`RunCtx::before_probe`] の確認に加えて、クラウドの送信枠が
    /// **残っているか**を予約せずに読む。送る直前の予約（[`RunCtx::before_send`]）の代わりにはならない。
    /// 枠が無ければ、予約と同じ理由（日・周期）の [`RunError::Budget`]。
    pub fn budget_left(&self) -> Result<(), RunError> {
        let gate = self.before_probe()?;
        let Some(b) = gate.budget else {
            return Ok(());
        };
        let usage = budget::usage(self.read, self.task, (self.clock)(), b.cycle_secs)
            .map_err(|_| RunError::Budget(Reservation::Failed))?;
        if b.daily == 0 || usage.day >= b.daily {
            return Err(RunError::Budget(Reservation::DailyExhausted));
        }
        if b.cycle == 0 || usage.cycle >= b.cycle {
            return Err(RunError::Budget(Reservation::CycleExhausted));
        }
        Ok(())
    }

    pub(super) fn dependency_allowed(&self, task: TaskKind) -> bool {
        self.config.gate(task, true).is_ok()
            && self
                .config
                .settings()
                .is_some_and(|s| matches!(pause::status(self.read, s, task), Ok(None)))
    }

    fn check(
        &self,
        reserve: bool,
        whole: Option<&super::http::Deadline>,
    ) -> Result<Gate<'a>, RunError> {
        if self.stop.is_stopped() {
            return Err(RunError::Stopping);
        }
        let expired = || whole.is_some_and(|d| d.remaining().is_none());
        let timeout =
            || RunError::ProviderWait(Wait::new(WaitScope::Entrance, WaitReason::Unreachable));
        if expired() {
            return Err(timeout());
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
        if let Some(send_budget) = gate.budget.filter(|_| reserve) {
            let task = self.task;
            let clock = Arc::clone(self.clock);
            let deadline = whole.map_or_else(
                || Instant::now() + BUDGET_WAIT,
                |d| d.wait_until(BUDGET_WAIT),
            );
            // 時刻は writer の依頼の中で取る（順番待ちのあと）
            let reservation = match self.writer.call_within(deadline, move |conn| {
                budget::reserve(conn, task, &send_budget, clock())
            }) {
                Ok(reservation) => reservation,
                Err(WriteError::Expired) if expired() => return Err(timeout()),
                Err(WriteError::Expired) => Reservation::Busy,
                Err(_) => Reservation::Failed,
            };
            if !reservation.is_granted() {
                return Err(RunError::Budget(reservation));
            }
        }
        // 許可を確定する直前にもう一度。予算の順番待ち（writer が塞がっている間）に停止の合図が来たら送らない。
        // 取れた予約は戻さない（送ったものとして数える側に倒す）
        if self.stop.is_stopped() {
            return Err(RunError::Stopping);
        }
        if expired() {
            return Err(timeout());
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
        Err(_) => {
            tracing::error!("[ai] 前の起動のジョブを回収できません（AI を始めません）");
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
            Err(_) => tracing::warn!("[ai] Provider 停止の整理に失敗しました"),
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
    /// Provider 側の都合（利用不可・429・予算切れ）で、この時刻（Unix 秒）まで送らない用途（この起動の中だけ）。
    blocked: HashMap<TaskKind, i64>,
    /// 共有の一時待機（DB から周期の最初に読み、新しい待ちは記録と同時に足す。再起動後も残る）。
    waits: HashMap<WaitKey, i64>,
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
            waits: HashMap::new(),
        }
    }

    /// 1 周期: reconcile のあと、使える種類のジョブが無くなる（または停止・上限）まで処理する。
    pub(super) fn cycle(&mut self) {
        if self.handlers.is_empty() || self.stop.is_stopped() {
            return;
        }
        let read = match crate::db::open_read_only(&self.config.db_path()) {
            Ok(conn) => conn,
            Err(_) => {
                tracing::warn!("[ai] DB を読み取り用に開けません（次の周期に持ち越します）");
                return;
            }
        };
        // 共有の一時待機（再起動をまたいで残る）を読み直す。読めなければ空のまま進む（待ちは各 handler の確認が補う）
        self.waits = waits::load(&read, (self.clock)()).unwrap_or_default();
        self.reconcile(&read);
        self.maintain(&read);
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
            let scopes: Vec<_> = kinds
                .iter()
                .filter_map(|k| self.handler(*k))
                .map(|h| h.claim_scope())
                .collect();
            let (run_id, now, stop) = (self.run_id.clone(), (self.clock)(), self.stop.clone());
            // writer の順番待ちのあいだに来た合図も尊重する: 依頼の中、claim の直前にもう一度確かめ、
            // 止まっていれば token も作らず状態も変えない（呼び出し側の確認だけでは、待ちの間の合図を取りこぼす）
            let claimed = match self.writer.call(move |conn| {
                if stop.is_stopped() {
                    return Ok(None);
                }
                let token = jobs::random_id()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                jobs::claim_next_scoped(conn, now, &run_id, &token, &scopes)
            }) {
                Ok(Some(claimed)) => claimed,
                Ok(None) => return,
                Err(_) => {
                    tracing::warn!("[ai] ジョブを claim できません（次の周期に持ち越します）");
                    return;
                }
            };
            let task = task_for(claimed.key.kind);
            *processed.entry(task).or_insert(0) += 1;
            self.process(&read, claimed, task);
            self.maintain(&read);
        }
    }

    fn maintain(&self, read: &Connection) {
        for handler in &self.handlers {
            match handler.maintenance(read, (self.clock)()) {
                Ok(Some(action)) => {
                    if self.writer.call(action).is_err() {
                        tracing::warn!("[ai] 索引の保守に失敗しました");
                    }
                }
                Ok(None) => {}
                Err(_) => tracing::warn!("[ai] 索引の保守を準備できません"),
            }
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
                if self.wait_blocks(task, now) {
                    return false;
                }
                match self.config.ai.settings() {
                    Some(settings) => matches!(pause::status(read, settings, task), Ok(None)),
                    None => false,
                }
            })
            .collect()
    }

    /// 共有の一時待機が用途を止めているか（入口全体の待ち、または用途が使うサービスの待ち）。
    fn wait_blocks(&self, task: TaskKind, now: i64) -> bool {
        let Some(settings) = self.config.ai.settings() else {
            return false;
        };
        let Some(provider) = settings.provider_fingerprint(task) else {
            return false;
        };
        let service = settings.service_key(task);
        waits::blocks(&self.waits, &provider, service.as_deref(), now)
    }

    fn handler(&self, kind: JobKind) -> Option<Arc<dyn Handler>> {
        self.handlers.iter().find(|h| h.kind() == kind).cloned()
    }

    fn reconcile(&self, read: &Connection) {
        let now = (self.clock)();
        'handlers: for handler in &self.handlers {
            if self.stop.is_stopped() {
                return;
            }
            let kind = handler.kind();
            // 設定で許可されない用途は積まない（無効の理由は状態の集計に出る）
            if self.config.ai.gate(task_for(kind), true).is_err() {
                continue;
            }
            let cleanup = match handler.reconcile_cleanup(read, now) {
                Ok(actions) => actions,
                Err(_) => {
                    tracing::warn!("[ai] 失効検査に失敗しました");
                    continue;
                }
            };
            let mut cleanup = cleanup.into_iter();
            loop {
                let chunk: Vec<_> = cleanup.by_ref().take(ENQUEUE_CHUNK).collect();
                if self.stop.is_stopped() {
                    return;
                }
                if chunk.is_empty() {
                    break;
                }
                if self
                    .writer
                    .call(move |conn| {
                        let tx = conn.transaction()?;
                        for action in chunk {
                            action(&tx)?;
                        }
                        tx.commit()
                    })
                    .is_err()
                {
                    tracing::warn!("[ai] 失効を記録できません");
                    // cleanup が担う試行履歴の継承等を汎用 enqueue で迂回しない。
                    continue 'handlers;
                }
            }
            let keys = match catch_unwind(AssertUnwindSafe(|| handler.reconcile(read, now))) {
                Ok(Ok(keys)) => keys,
                Ok(Err(_)) => {
                    tracing::warn!("[ai] {} の reconcile に失敗しました", kind.as_str());
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
                let projections = match chunk
                    .iter()
                    .map(|key| handler.projection(read, key, now))
                    .collect::<rusqlite::Result<Vec<_>>>()
                {
                    Ok(items) => items,
                    Err(_) => {
                        tracing::warn!("[ai] 投影の準備に失敗しました");
                        break;
                    }
                };
                let (chunk, clock) = (chunk.to_vec(), Arc::clone(&self.clock));
                match self.writer.call(move |conn| {
                    let done = jobs::enqueue_batch(conn, &chunk, clock())?;
                    // 起動回収で failed になったもの・未投影も次周期で冪等に修復する。
                    let tx = conn.transaction()?;
                    for project in projections.into_iter().flatten() {
                        project(&tx)?;
                    }
                    tx.commit()?;
                    Ok(done)
                }) {
                    Ok(done) => {
                        created += done.created;
                        revived += done.revived;
                    }
                    Err(_) => {
                        tracing::warn!("[ai] ジョブを積めません");
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
            Err(_) => {
                tracing::warn!("[ai] 結果を確定できません");
                // 結果の保存の失敗は当該ジョブの失敗（成功だけが残ることは無い）
                self.record(claimed, ErrorCode::Internal, Failure::Attempt);
            }
        }
    }

    fn record(&self, claimed: &Claimed, code: ErrorCode, failure: Failure) {
        let (c, clock) = (claimed.clone(), Arc::clone(&self.clock));
        let project = (|| {
            let read = crate::db::open_read_only(&self.config.db_path()).map_err(|_| ())?;
            self.handler(claimed.key.kind)
                .map(|h| h.projection(&read, &claimed.key, clock()))
                .transpose()
                .map(|v| v.flatten())
                .map_err(|_| ())
        })();
        let Ok(project) = project else {
            tracing::warn!("[ai] 失敗投影の準備に失敗しました");
            return;
        };
        if self
            .writer
            .call(move |conn| {
                jobs::record_failure_with(conn, &c, clock(), code, failure, |tx| match project {
                    Some(p) => p(tx),
                    None => Ok(InputCheck::Current),
                })
            })
            .is_err()
        {
            // running のまま残り、次の起動の回収が数える。値・SQL エラー本文は出さない。
            tracing::warn!("[ai] 失敗を記録できません");
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
                if self
                    .writer
                    .call(move |conn| jobs::mark_obsolete(conn, &c, clock()))
                    .is_err()
                {
                    tracing::warn!("[ai] ジョブを無効にできません");
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
            RunError::InputPending => {
                let (code, f) = defer(ErrorCode::InputPending, None);
                (code, f, None)
            }
            RunError::DependencyWait(code) => {
                let (code, f) = defer(code, None);
                (code, f, None)
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
            RunError::ProviderWait(wait) => {
                self.wait_and_defer(claimed, task, wait);
                return;
            }
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

    /// 共有の一時待機を DB に残し（再起動後も効く）、ジョブは失敗に数えず、`next_attempt_at` を待ちの終わりまで遅らせる
    /// （1 回の依頼）。範囲が入口全体なら、同じ Provider の他の用途も次の claim から飛ばされる。
    fn wait_and_defer(&mut self, claimed: &Claimed, task: TaskKind, wait: Wait) {
        let secs = wait.secs();
        let now = (self.clock)();
        let until = now + i64::from(secs);
        let key = self.config.ai.settings().and_then(|s| {
            let provider = s.provider_fingerprint(task)?;
            let service = match wait.scope {
                WaitScope::Entrance => None,
                WaitScope::Service => Some(s.service_key(task)?),
            };
            Some(WaitKey { provider, service })
        });
        let code = if wait.reason == WaitReason::RateLimited {
            ErrorCode::RateLimited
        } else {
            ErrorCode::Unavailable
        };
        if let Some(key) = &key {
            let entry = self.waits.entry(key.clone()).or_insert(until);
            *entry = (*entry).max(until);
        } else {
            // 設定から識別が作れないときは、この起動の中の用途の待ちにする
            self.blocked.insert(task, until);
        }
        let (c, clock, reason) = (claimed.clone(), Arc::clone(&self.clock), wait.reason);
        let result = self.writer.call(move |conn| {
            let now = clock();
            if let Some(key) = &key {
                waits::set(conn, key, reason, until, now)?;
            }
            jobs::record_failure(
                conn,
                &c,
                now,
                code,
                Failure::Defer {
                    retry_after_secs: Some(secs),
                },
            )
        });
        if result.is_err() {
            // ジョブは running のまま残り、次の起動の回収が数える。待ちはこの起動の中では効く
            tracing::warn!("[ai] 待機を記録できません");
        }
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
            Err(_) => tracing::warn!("[ai] 停止を記録できません"),
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
    local: Option<JoinHandle<()>>,
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
                    Err(_) => tracing::warn!("[ai] 停止開始を記録できません"),
                }
            })
            .expect("AI の停止記録のスレッドを起動できません")
    };
    let local =
        super::groups::runtime::Runtime::production(Arc::clone(&deps.config)).map(|runtime| {
            let (config, writer, stop, lock, poll) = (
                Arc::clone(&deps.config),
                deps.writer.clone(),
                deps.stop.clone(),
                deps.lock.clone(),
                deps.poll,
            );
            std::thread::Builder::new()
                .name("ai-scenes".into())
                .spawn(move || {
                    let _lock = lock;
                    loop {
                        if stop.is_stopped() {
                            break;
                        }
                        if catch_unwind(AssertUnwindSafe(|| {
                            if let Ok(read) = crate::db::open_read_only(&config.db_path()) {
                                match runtime.cycle(&read, &writer, &stop) {
                                    Ok(_) => {}
                                    // 書き込みの順番待ちが期限を過ぎただけ（混雑）。次の周期にやり直す
                                    Err(super::groups::Error::WriterBusy) => {
                                        tracing::warn!("[ai] scene_writer_busy")
                                    }
                                    // 場面の表が想定外の列を持つ（移行できない）。配信は続け、周期だけを止める
                                    Err(super::groups::Error::SchemaMismatch) => {
                                        tracing::warn!("[ai] scene_schema_mismatch")
                                    }
                                    Err(_) => tracing::warn!("[ai] scene_cycle_failed"),
                                }
                            }
                        }))
                        .is_err()
                        {
                            tracing::warn!("[ai] scene_cycle_failed");
                        }
                        if stop.wait(poll) {
                            break;
                        }
                    }
                })
                .expect("scene_worker_start_failed")
        });
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
        local,
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
    /// 期限つきの依頼が writer で始まっていれば、writer スレッド自身も `LockHold` を持つ（`Writer::open_held`）ので、
    /// この呼び出しや `Ingest` が先に終わっても、その SQL と接続の close が済むまでロックは外れない。
    /// スレッドが終わったかを返す。
    pub fn shutdown(
        self,
        writer: &Writer,
        stop: &StopFlag,
        join_cap: Duration,
        clock: &Clock,
    ) -> bool {
        let joined = join_within(self.thread, stop.phase_limit(join_cap));
        let local_joined = self
            .local
            .is_none_or(|local| join_within(local, stop.phase_limit(join_cap)));
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
            Err(_) => {
                tracing::warn!("[ai] 実行中のジョブを戻せません（次の起動の回収に任せます）")
            }
        }
        joined && local_joined
    }
}
