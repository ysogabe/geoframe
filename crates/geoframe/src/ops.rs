//! 運用コンソールの掲示板（`Board`）。同期ワーカーの 1 本のスレッドが処理する運用の要求（同期・品質解析の一括・
//! Drive の大量削除の保留の解除）の待ち・実行中・履歴を、メモリの中だけで持つ。`MassDeleteBoard` と同じ作り
//! （`Mutex` で守る。DB には書かない。再起動で消える）。
//!
//! - **直列**: 要求は 1 つだけ待てる。同じ種類が実行中のあいだは受け付けない。定期サイクルの実行中に来た要求は
//!   待ち、サイクルが終わった直後に取り出される。
//! - **Drive を読む要求**（同期・保留の解除）は、前回の終了から [`DRIVE_READ_MIN_INTERVAL`] は受け付けない
//!   （Drive と地理院を叩き続けない）。基準の時刻は履歴とは別に持つので、履歴から押し出されても効く。
//! - **応答に環境の情報を出さない**: 段の失敗の文面（SQLite のメッセージ・パス）と保留した行の ID は、
//!   JSON にも `Debug` にも出さない。`Board` が持つ `running`・`queued` の表示は種類と時刻だけで、印（`token`）と ID は
//!   ワーカーが取り出す [`Request`] の中にだけある。

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};

use crate::ai::ops::RetryReport;
use crate::drive::{HeldIds, MassDeleteHold};
use crate::stop::StopFlag;
use crate::sync::{CycleReport, StageResult};

/// Drive を読む要求（同期・保留の解除）の最短の間隔（前回の終了から）。
pub const DRIVE_READ_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// 履歴に残す件数（新しいほう）。
pub const HISTORY_LEN: usize = 10;

/// 運用の要求の種類。中身は持たない（印や ID は [`Request`] の中だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// 同期をいま回す（定期と同じ段）。
    Sync,
    /// 品質解析の一括。
    QualityScan,
    /// Drive の大量削除の保留の解除。
    DriveRelease,
    /// AI ジョブの再試行（PR ②。同期ワーカーでは処理しない）。
    AiRetry,
    /// AI の停止の解除（PR ②。同上）。
    AiReleasePause,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::QualityScan => "quality_scan",
            Self::DriveRelease => "drive_release",
            Self::AiRetry => "ai_retry",
            Self::AiReleasePause => "ai_release_pause",
        }
    }

    /// Drive の一覧を読む要求か（最短の間隔の対象）。
    pub fn reads_drive(self) -> bool {
        matches!(self, Self::Sync | Self::DriveRelease)
    }
}

/// 実行中・履歴の記録の種類: 運用の要求か、定期サイクル（`"cycle"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Cycle,
    Op(OpKind),
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cycle => "cycle",
            Self::Op(kind) => kind.as_str(),
        }
    }
}

/// 要求の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 失敗が無かった。
    Done,
    /// 失敗した段があった（他の段は走った）。
    Partial,
    /// DB を開けない・`run_cycle` 自身のパニックなど、段の結果を得られなかった。
    Failed,
    /// 段が省略された（鍵なし・認証失敗・停止）。
    Skipped,
    /// 保留の解除で、承認していない行が消えようとした、または取り直した一覧が空だった（保留に戻した）。
    Exceeded,
    /// 実行の直前に、保留が承認したものと違っていた（実行しなかった）。
    HoldChanged,
    /// AI ジョブの再試行（PR ②）。
    Retried,
    /// AI の停止の解除（PR ②）。
    Released,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Exceeded => "exceeded",
            Self::HoldChanged => "hold_changed",
            Self::Retried => "retried",
            Self::Released => "released",
        }
    }
}

/// ワーカーが取り出して処理する要求。**印と ID の集合はここにだけある**（`Board` の表示には出ない）。
/// `Debug` は ID の列を出さない（[`HeldIds`] の `Debug` は件数だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub kind: OpKind,
    pub requested_at: String,
    /// 保留の解除（[`OpKind::DriveRelease`]）のとき、画面が見た保留の印。
    pub token: Option<String>,
    /// 同じく、承認した行の ID の集合。
    pub ids: Option<HeldIds>,
}

/// 受け付けた要求（POST の応答に使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub id: u64,
    pub kind: OpKind,
    pub requested_at: String,
}

/// 受け付けられなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueError {
    /// すでに待ちがある、または同じ種類が実行中。
    Busy,
    /// 停止の合図が来ている。
    Stopping,
    /// Drive を読む要求の前回の終了から [`DRIVE_READ_MIN_INTERVAL`] 経っていない。
    TooSoon,
}

/// 段 1 つぶんの結果（JSON 用）。段の失敗の文面は持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageJson {
    pub name: &'static str,
    /// `done`・`skipped`・`failed`・`panicked`・`not_run`。
    pub result: &'static str,
    /// `skipped` の理由（段が返す固定文）。
    pub reason: Option<&'static str>,
    pub counts: Vec<(&'static str, u64)>,
}

impl StageJson {
    pub fn of(name: &'static str, result: &StageResult) -> Self {
        let (result, reason, counts) = match result {
            StageResult::Done(counts) => ("done", None, counts.0.clone()),
            StageResult::Skipped(reason) => ("skipped", Some(*reason), Vec::new()),
            // 失敗の文面（SQLite のメッセージ・パス）は出さない。ログには別に出ている
            StageResult::Failed(_) => ("failed", None, Vec::new()),
            StageResult::Panicked => ("panicked", None, Vec::new()),
            StageResult::NotRun => ("not_run", None, Vec::new()),
        };
        Self {
            name,
            result,
            reason,
            counts,
        }
    }

    fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("result".into(), json!(self.result));
        if let Some(reason) = self.reason {
            out.insert("reason".into(), json!(reason));
        }
        if !self.counts.is_empty() {
            let counts: Map<String, Value> = self
                .counts
                .iter()
                .map(|(name, n)| ((*name).to_string(), json!(n)))
                .collect();
            out.insert("counts".into(), Value::Object(counts));
        }
        Value::Object(out)
    }
}

/// `CycleReport` の段を JSON 用にする。
pub fn stages_json(report: &CycleReport) -> Vec<StageJson> {
    report
        .stages
        .iter()
        .map(|s| StageJson::of(s.name, &s.result))
        .collect()
}

fn count_of(result: &StageResult, name: &str) -> u64 {
    match result {
        StageResult::Done(counts) => counts
            .0
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, v)| *v),
        _ => 0,
    }
}

/// サイクルの結果から要求の結果を決める。
///
/// - [`OpKind::DriveRelease`] は唯一の段から決める: `Skipped`・`NotRun` → `skipped`、`Failed`・`Panicked` → `failed`、
///   `Done` で `deletes_withheld` か `mass_delete_withheld` が **0 より大きい** → `exceeded`（`counts()` は 0 でも鍵を
///   入れるので、鍵の有無では決めない）、それ以外 → `done`。
/// - それ以外は段が複数: 失敗・パニックが 1 つでもあれば `partial`、全部 `NotRun` なら `skipped`、それ以外は `done`。
///
/// DB を開けない・`run_cycle` 自身のパニックは、ここに来る前にワーカーが `failed` で終える。
pub fn outcome_of(kind: RunKind, report: &CycleReport) -> Outcome {
    if kind == RunKind::Op(OpKind::DriveRelease) {
        return match report.stages.first().map(|s| &s.result) {
            None | Some(StageResult::Failed(_) | StageResult::Panicked) => Outcome::Failed,
            Some(StageResult::Skipped(_) | StageResult::NotRun) => Outcome::Skipped,
            Some(done @ StageResult::Done(_)) => {
                if count_of(done, "deletes_withheld") > 0
                    || count_of(done, "mass_delete_withheld") > 0
                {
                    Outcome::Exceeded
                } else {
                    Outcome::Done
                }
            }
        };
    }
    if report.failed() > 0 {
        Outcome::Partial
    } else if !report.stages.is_empty()
        && report
            .stages
            .iter()
            .all(|s| matches!(s.result, StageResult::NotRun))
    {
        Outcome::Skipped
    } else {
        Outcome::Done
    }
}

/// AI 系の要求の結果（PR ② で中身を使う。いまは型だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Retry(RetryReport),
    Release { released: usize },
}

impl Report {
    fn to_json(self) -> Value {
        match self {
            Self::Retry(r) => json!({
                "requeued": r.requeued,
                "obsolete": r.obsolete,
                "satisfied": r.satisfied,
                "unsupported": r.unsupported,
                "policy_disabled": r.policy_disabled,
            }),
            Self::Release { released } => json!({ "released": released }),
        }
    }
}

/// 終わった実行の記録（履歴・直近のサイクル）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// 要求の番号（定期サイクルは 0）。
    pub id: u64,
    pub kind: RunKind,
    /// 要求を受け付けた時刻（定期サイクルは `None`）。
    pub requested_at: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub outcome: Outcome,
    pub stages: Vec<StageJson>,
    pub report: Option<Report>,
}

impl Record {
    fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        out.insert("kind".into(), json!(self.kind.as_str()));
        if let Some(requested_at) = &self.requested_at {
            out.insert("requested_at".into(), json!(requested_at));
        }
        out.insert("started_at".into(), json!(self.started_at));
        out.insert("finished_at".into(), json!(self.finished_at));
        out.insert("outcome".into(), json!(self.outcome.as_str()));
        out.insert(
            "stages".into(),
            Value::Array(self.stages.iter().map(StageJson::to_json).collect()),
        );
        if let Some(report) = self.report {
            out.insert("report".into(), report.to_json());
        }
        Value::Object(out)
    }
}

/// 時計。本番は実時間、試験は進みを自分で決める偽の時計。
pub trait Clock: Send + Sync {
    /// 単調な時刻（最短の間隔の判定に使う）。
    fn now(&self) -> Instant;
    /// 今から `after` 後の現地時刻（`YYYY-MM-DD HH:MM:SS`。`sysinfo::local_timestamp()` と同じ形）。
    fn timestamp(&self, after: Duration) -> String;
}

/// 実時間の時計。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn timestamp(&self, after: Duration) -> String {
        crate::sysinfo::local_timestamp_at(SystemTime::now() + after)
    }
}

#[derive(Debug)]
struct Running {
    id: u64,
    kind: RunKind,
    requested_at: Option<String>,
    started_at: String,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    queued: Option<Request>,
    next_id: u64,
    history: VecDeque<Record>,
    last_cycle: Option<Record>,
    next_cycle_at: Option<String>,
    /// Drive を読む要求（同期・保留の解除）が最後に終わった時刻。履歴とは別（履歴から押し出されても効く）。
    last_drive_read_finished: Option<Instant>,
}

/// 運用コンソールの掲示板。`Mutex` は 1 つだけ。
pub struct Board {
    inner: Mutex<Inner>,
    stop: StopFlag,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Board {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Board(..)")
    }
}

impl Board {
    /// `stop` は停止の合図（来ていれば [`EnqueueError::Stopping`]）。`clock` は試験で差し替える。
    pub fn new(stop: StopFlag, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                ..Inner::default()
            }),
            stop,
            clock,
        }
    }

    /// 実時間の時計の掲示板。
    pub fn with_system_clock(stop: StopFlag) -> Self {
        Self::new(stop, Arc::new(SystemClock))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // ガード（`Drop`）がパニックの巻き戻しの中でも呼ばれるので、毒は無視する
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 要求を 1 つ入れる。同じロックの中で、次をこの順に確かめる: 停止の合図が無い（[`EnqueueError::Stopping`]）、
    /// 待ちが空で同じ種類が実行中でない（[`EnqueueError::Busy`]）、Drive を読む要求なら前回の終了から
    /// [`DRIVE_READ_MIN_INTERVAL`] 以上（[`EnqueueError::TooSoon`]）。
    pub fn enqueue(
        &self,
        kind: OpKind,
        token: Option<String>,
        ids: Option<HeldIds>,
    ) -> Result<Queued, EnqueueError> {
        let mut inner = self.lock();
        if self.stop.is_stopped() {
            return Err(EnqueueError::Stopping);
        }
        if inner.queued.is_some()
            || inner
                .running
                .as_ref()
                .is_some_and(|r| r.kind == RunKind::Op(kind))
        {
            return Err(EnqueueError::Busy);
        }
        if kind.reads_drive()
            && inner.last_drive_read_finished.is_some_and(|at| {
                self.clock.now().saturating_duration_since(at) < DRIVE_READ_MIN_INTERVAL
            })
        {
            return Err(EnqueueError::TooSoon);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        let requested_at = self.clock.timestamp(Duration::ZERO);
        inner.queued = Some(Request {
            id,
            kind,
            requested_at: requested_at.clone(),
            token,
            ids,
        });
        Ok(Queued {
            id,
            kind,
            requested_at,
        })
    }

    /// 待ちの要求を取り出して実行中にする（同じロックの中。取り出した直後に同じ種類を受け付けない）。
    /// 実行中のもの（定期サイクルを含む）があるときは取り出さない。
    pub fn take_queued(&self) -> Option<Request> {
        let mut inner = self.lock();
        if inner.running.is_some() {
            return None;
        }
        let request = inner.queued.take()?;
        inner.running = Some(Running {
            id: request.id,
            kind: RunKind::Op(request.kind),
            requested_at: Some(request.requested_at.clone()),
            started_at: self.clock.timestamp(Duration::ZERO),
        });
        Some(request)
    }

    /// 要求の記録を終えて実行中を外す。履歴は新しい順に [`HISTORY_LEN`] 件。`Sync`・`DriveRelease` なら
    /// 最短の間隔の基準を更新する。`id` が実行中のものと違えば何もしない（二重に呼んでも害が無い）。
    pub fn finish(
        &self,
        id: u64,
        outcome: Outcome,
        stages: Vec<StageJson>,
        report: Option<Report>,
    ) {
        let mut inner = self.lock();
        let Some(running) = inner
            .running
            .take_if(|r| r.id == id && r.kind != RunKind::Cycle)
        else {
            return;
        };
        if let RunKind::Op(kind) = running.kind
            && kind.reads_drive()
        {
            inner.last_drive_read_finished = Some(self.clock.now());
        }
        inner.history.push_front(Record {
            id: running.id,
            kind: running.kind,
            requested_at: running.requested_at,
            started_at: running.started_at,
            finished_at: self.clock.timestamp(Duration::ZERO),
            outcome,
            stages,
            report,
        });
        inner.history.truncate(HISTORY_LEN);
    }

    /// 定期サイクルを始める（実行中にし、次の予定を消す）。
    pub fn start_cycle(&self) {
        let mut inner = self.lock();
        inner.running = Some(Running {
            id: 0,
            kind: RunKind::Cycle,
            requested_at: None,
            started_at: self.clock.timestamp(Duration::ZERO),
        });
        inner.next_cycle_at = None;
    }

    /// 定期サイクルを終える（直近の記録にし、`next_in` 後を次の予定にする）。
    pub fn finish_cycle(&self, report: &CycleReport, next_in: Duration) {
        let outcome = outcome_of(RunKind::Cycle, report);
        self.finish_cycle_with(outcome, stages_json(report), next_in);
    }

    /// 段の結果を得られなかった定期サイクル（DB を開けない・`run_cycle` 自身のパニック）を終える。
    pub fn finish_cycle_failed(&self, next_in: Duration) {
        self.finish_cycle_with(Outcome::Failed, Vec::new(), next_in);
    }

    fn finish_cycle_with(&self, outcome: Outcome, stages: Vec<StageJson>, next_in: Duration) {
        let mut inner = self.lock();
        let started_at = inner
            .running
            .take_if(|r| r.kind == RunKind::Cycle)
            .map_or_else(|| self.clock.timestamp(Duration::ZERO), |r| r.started_at);
        inner.last_cycle = Some(Record {
            id: 0,
            kind: RunKind::Cycle,
            requested_at: None,
            started_at,
            finished_at: self.clock.timestamp(Duration::ZERO),
            outcome,
            stages,
            report: None,
        });
        inner.next_cycle_at = Some(self.clock.timestamp(next_in));
    }

    /// 次の定期サイクルの予定を `after` 後に置き直す（手動の同期のあと）。
    pub fn schedule_next_cycle(&self, after: Duration) {
        self.lock().next_cycle_at = Some(self.clock.timestamp(after));
    }

    /// 要求の記録を `Drop` で必ず終えるガード（パニックの巻き戻しでも `running` が残らない）。
    pub fn guard(&self, id: u64) -> RunGuard<'_> {
        RunGuard {
            board: self,
            id,
            cycle: false,
            next_in: Duration::ZERO,
            done: false,
        }
    }

    /// 定期サイクルの記録を `Drop` で必ず終えるガード（`next_in` は次の予定までの間隔）。
    pub fn cycle_guard(&self, next_in: Duration) -> RunGuard<'_> {
        RunGuard {
            board: self,
            id: 0,
            cycle: true,
            next_in,
            done: false,
        }
    }

    /// GET 用の複製（ロックを短く持つ）。
    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        Snapshot {
            running: inner.running.as_ref().map(|r| RunningView {
                id: r.id,
                kind: r.kind,
                started_at: r.started_at.clone(),
            }),
            queued: inner.queued.as_ref().map(|q| Queued {
                id: q.id,
                kind: q.kind,
                requested_at: q.requested_at.clone(),
            }),
            next_cycle_at: inner.next_cycle_at.clone(),
            last_cycle: inner.last_cycle.clone(),
            history: inner.history.iter().cloned().collect(),
        }
    }
}

/// [`Board::guard`]・[`Board::cycle_guard`] のガード。`finish` を呼ばずに落ちたら `failed` で終える。
pub struct RunGuard<'a> {
    board: &'a Board,
    id: u64,
    cycle: bool,
    next_in: Duration,
    done: bool,
}

impl RunGuard<'_> {
    /// 要求の記録を終える。
    pub fn finish(mut self, outcome: Outcome, stages: Vec<StageJson>, report: Option<Report>) {
        self.done = true;
        self.board.finish(self.id, outcome, stages, report);
    }

    /// 定期サイクルの記録を終える（次の予定は [`Board::cycle_guard`] で渡した間隔）。
    pub fn finish_cycle(mut self, report: &CycleReport) {
        self.done = true;
        self.board.finish_cycle(report, self.next_in);
    }
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if self.cycle {
            self.board.finish_cycle_failed(self.next_in);
        } else {
            self.board
                .finish(self.id, Outcome::Failed, Vec::new(), None);
        }
    }
}

/// 実行中の表示（種類と開始時刻だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningView {
    pub id: u64,
    pub kind: RunKind,
    pub started_at: String,
}

/// GET 用の複製。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub running: Option<RunningView>,
    pub queued: Option<Queued>,
    pub next_cycle_at: Option<String>,
    pub last_cycle: Option<Record>,
    /// 新しい順。
    pub history: Vec<Record>,
}

impl Snapshot {
    /// `GET /api/admin/ops` の `worker`。
    pub fn worker_json(&self) -> Value {
        json!({
            "running": self.running.as_ref().map(|r| json!({
                "id": r.id,
                "kind": r.kind.as_str(),
                "started_at": r.started_at,
            })),
            "queued": self.queued.as_ref().map(|q| json!({
                "id": q.id,
                "kind": q.kind.as_str(),
                "requested_at": q.requested_at,
            })),
            "next_cycle_at": self.next_cycle_at,
            "last_cycle": self.last_cycle.as_ref().map(Record::to_json),
        })
    }

    /// `GET /api/admin/ops` の `history`（新しい順）。
    pub fn history_json(&self) -> Value {
        Value::Array(self.history.iter().map(Record::to_json).collect())
    }

    /// `{"worker": …, "history": […]}`。
    pub fn to_json(&self) -> Value {
        json!({ "worker": self.worker_json(), "history": self.history_json() })
    }
}

/// 保留の印。`hex(sha256("{held}\t{reason}\t{drive_rows}\t{since}\n{id1}\n{id2}…")[..8])`（選別タブの
/// `curation::group_token` と同じ作り。認可ではない）。保留した行の ID の集合が 1 件でも違えば別の印になる。
pub fn token_of(hold: &MassDeleteHold) -> String {
    let mut text = format!(
        "{}\t{}\t{}\t{}",
        hold.held,
        hold.reason.as_str(),
        hold.drive_rows,
        hold.since
    );
    for id in hold.ids.as_slice() {
        text.push('\n');
        text.push_str(&id.to_string());
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `GET /api/admin/ops` の `hold`。ID は出さない。`releasable` は `ratio` の保留だけ（一覧が空のための保留は
/// 画面から解除しない）。
pub fn hold_json(hold: &MassDeleteHold) -> Value {
    json!({
        "held": hold.held,
        "reason": hold.reason.as_str(),
        "drive_rows": hold.drive_rows,
        "since": hold.since,
        "checked_at": hold.checked_at,
        "token": token_of(hold),
        "releasable": hold.reason == crate::drive::HoldReason::Ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::HoldReason;
    use crate::sync::{Counts, StageReport};

    /// 進みを自分で決める時計。時刻の文字列は「基準からの秒数」。
    struct FakeClock {
        base: Instant,
        elapsed: Mutex<Duration>,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
            })
        }

        fn advance(&self, by: Duration) {
            *self.elapsed.lock().unwrap() += by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.base + *self.elapsed.lock().unwrap()
        }

        fn timestamp(&self, after: Duration) -> String {
            format!("t+{}s", (*self.elapsed.lock().unwrap() + after).as_secs())
        }
    }

    fn board() -> (Board, Arc<FakeClock>, StopFlag) {
        let clock = FakeClock::new();
        let stop = StopFlag::new();
        let board = Board::new(stop.clone(), clock.clone());
        (board, clock, stop)
    }

    fn done_report(names: &[&'static str]) -> CycleReport {
        CycleReport {
            stages: names
                .iter()
                .map(|name| StageReport {
                    name,
                    result: StageResult::Done(Counts(vec![("n", 1)])),
                })
                .collect(),
            photos: None,
        }
    }

    fn run_to_end(board: &Board, kind: OpKind) -> u64 {
        let queued = board.enqueue(kind, None, None).unwrap();
        let req = board.take_queued().unwrap();
        assert_eq!(req.id, queued.id);
        board.finish(req.id, Outcome::Done, Vec::new(), None);
        req.id
    }

    #[test]
    fn enqueue_accepts_one_waiting_request_and_numbers_them() {
        let (board, _clock, _stop) = board();
        let first = board.enqueue(OpKind::QualityScan, None, None).unwrap();
        assert_eq!((first.id, first.kind), (1, OpKind::QualityScan));
        // 待ちがあるあいだは種類を問わず Busy
        assert_eq!(
            board.enqueue(OpKind::QualityScan, None, None),
            Err(EnqueueError::Busy)
        );
        assert_eq!(
            board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::Busy)
        );
        let snap = board.snapshot();
        assert_eq!(snap.queued.map(|q| q.id), Some(1));
        assert!(snap.running.is_none());
    }

    #[test]
    fn a_taken_request_blocks_its_own_kind_but_lets_another_wait() {
        let (board, _clock, _stop) = board();
        board.enqueue(OpKind::QualityScan, None, None).unwrap();
        let req = board.take_queued().unwrap();
        assert_eq!(req.kind, OpKind::QualityScan);
        // 取り出した直後に同じ種類は受け付けない
        assert_eq!(
            board.enqueue(OpKind::QualityScan, None, None),
            Err(EnqueueError::Busy)
        );
        // 別の種類は 1 つ待てて、2 つ目は Busy
        board.enqueue(OpKind::Sync, None, None).unwrap();
        assert_eq!(
            board.enqueue(OpKind::DriveRelease, None, None),
            Err(EnqueueError::Busy)
        );
        // 実行中は次を取り出さない
        assert!(board.take_queued().is_none());
        board.finish(req.id, Outcome::Done, Vec::new(), None);
        assert_eq!(board.take_queued().map(|r| r.kind), Some(OpKind::Sync));
    }

    #[test]
    fn a_request_arriving_during_a_cycle_waits_and_runs_after_it() {
        let (board, _clock, _stop) = board();
        board.start_cycle();
        board.enqueue(OpKind::Sync, None, None).unwrap();
        assert!(board.take_queued().is_none());
        board.finish_cycle(&done_report(&["index_local"]), Duration::from_secs(600));
        assert_eq!(board.take_queued().map(|r| r.kind), Some(OpKind::Sync));
    }

    #[test]
    fn a_stop_signal_refuses_everything() {
        let (board, _clock, stop) = board();
        stop.stop();
        for kind in [OpKind::Sync, OpKind::QualityScan, OpKind::DriveRelease] {
            assert_eq!(board.enqueue(kind, None, None), Err(EnqueueError::Stopping));
        }
    }

    #[test]
    fn drive_reads_wait_60_seconds_from_the_last_one() {
        let (board, clock, _stop) = board();
        // 初めてなら受け付ける
        run_to_end(&board, OpKind::Sync);
        // 60 秒未満は拒否（Sync も DriveRelease も）。Drive を読まない品質解析は影響されない
        clock.advance(Duration::from_secs(59));
        assert_eq!(
            board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
        assert_eq!(
            board.enqueue(OpKind::DriveRelease, None, None),
            Err(EnqueueError::TooSoon)
        );
        board.enqueue(OpKind::QualityScan, None, None).unwrap();
        board.take_queued().unwrap();
        board.finish(2, Outcome::Done, Vec::new(), None);
        // 60 秒ちょうどで受け付ける
        clock.advance(Duration::from_secs(1));
        board.enqueue(OpKind::DriveRelease, None, None).unwrap();
        board.take_queued().unwrap();
        board.finish(3, Outcome::Done, Vec::new(), None);
        // 解除の終了も基準になる（交互に送って一覧を短い間隔で取れない）
        clock.advance(Duration::from_secs(30));
        assert_eq!(
            board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
    }

    #[test]
    fn the_interval_survives_the_history_pushing_the_record_out() {
        let (board, clock, _stop) = board();
        run_to_end(&board, OpKind::Sync);
        // Drive を読まない要求で履歴を押し出す
        for _ in 0..HISTORY_LEN + 2 {
            run_to_end(&board, OpKind::QualityScan);
        }
        let snap = board.snapshot();
        assert_eq!(snap.history.len(), HISTORY_LEN);
        assert!(
            snap.history
                .iter()
                .all(|r| r.kind != RunKind::Op(OpKind::Sync))
        );
        clock.advance(Duration::from_secs(10));
        assert_eq!(
            board.enqueue(OpKind::Sync, None, None),
            Err(EnqueueError::TooSoon)
        );
        clock.advance(Duration::from_secs(50));
        board.enqueue(OpKind::Sync, None, None).unwrap();
    }

    #[test]
    fn a_failed_or_dropped_drive_read_also_counts() {
        let (board, clock, _stop) = board();
        board.enqueue(OpKind::DriveRelease, None, None).unwrap();
        let req = board.take_queued().unwrap();
        {
            // 記録を終えずに落ちた（外側のパニックなど）
            let _guard = board.guard(req.id);
        }
        let snap = board.snapshot();
        assert!(snap.running.is_none());
        assert_eq!(snap.history[0].outcome, Outcome::Failed);
        clock.advance(Duration::from_secs(5));
        assert_eq!(
            board.enqueue(OpKind::DriveRelease, None, None),
            Err(EnqueueError::TooSoon)
        );
    }

    #[test]
    fn the_guard_finishes_during_a_panic_and_a_cycle_guard_clears_running() {
        let (board, _clock, _stop) = board();
        board.enqueue(OpKind::QualityScan, None, None).unwrap();
        let req = board.take_queued().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = board.guard(req.id);
            panic!("段の外側");
        }));
        assert!(result.is_err());
        let snap = board.snapshot();
        assert!(snap.running.is_none());
        assert_eq!(snap.history[0].outcome, Outcome::Failed);
        // 定期サイクルも同じ
        board.start_cycle();
        drop(board.cycle_guard(Duration::from_secs(600)));
        let snap = board.snapshot();
        assert!(snap.running.is_none());
        let last = snap.last_cycle.unwrap();
        assert_eq!((last.kind, last.outcome), (RunKind::Cycle, Outcome::Failed));
        assert!(snap.next_cycle_at.is_some());
        // 正しく終えたガードは Drop で二重に記録しない
        board.enqueue(OpKind::QualityScan, None, None).unwrap();
        let req = board.take_queued().unwrap();
        board.guard(req.id).finish(Outcome::Done, Vec::new(), None);
        assert_eq!(board.snapshot().history.len(), 2);
        assert_eq!(board.snapshot().history[0].outcome, Outcome::Done);
    }

    #[test]
    fn finish_twice_or_for_another_id_changes_nothing() {
        let (board, _clock, _stop) = board();
        board.enqueue(OpKind::QualityScan, None, None).unwrap();
        let req = board.take_queued().unwrap();
        board.finish(req.id + 5, Outcome::Done, Vec::new(), None);
        assert!(board.snapshot().running.is_some());
        board.finish(req.id, Outcome::Done, Vec::new(), None);
        board.finish(req.id, Outcome::Failed, Vec::new(), None);
        let snap = board.snapshot();
        assert_eq!(snap.history.len(), 1);
        assert_eq!(snap.history[0].outcome, Outcome::Done);
    }

    #[test]
    fn history_keeps_the_newest_ten_newest_first() {
        let (board, _clock, _stop) = board();
        for _ in 0..13 {
            run_to_end(&board, OpKind::QualityScan);
        }
        let ids: Vec<u64> = board.snapshot().history.iter().map(|r| r.id).collect();
        assert_eq!(ids, (4..=13).rev().collect::<Vec<_>>());
    }

    #[test]
    fn the_cycle_record_and_the_next_schedule_follow_the_cycle() {
        let (board, clock, _stop) = board();
        board.start_cycle();
        let snap = board.snapshot();
        assert_eq!(snap.running.map(|r| r.kind), Some(RunKind::Cycle));
        assert!(snap.next_cycle_at.is_none());
        clock.advance(Duration::from_secs(3));
        board.finish_cycle(&done_report(&["index_local"]), Duration::from_secs(600));
        let snap = board.snapshot();
        assert!(snap.running.is_none());
        let last = snap.last_cycle.unwrap();
        assert_eq!(
            (last.started_at.as_str(), last.finished_at.as_str()),
            ("t+0s", "t+3s")
        );
        assert_eq!(snap.next_cycle_at.as_deref(), Some("t+603s"));
        // 手動の同期のあとは、予定を置き直す
        board.schedule_next_cycle(Duration::from_secs(600));
        assert_eq!(board.snapshot().next_cycle_at.as_deref(), Some("t+603s"));
        // 定期サイクルは履歴に入らない
        assert!(board.snapshot().history.is_empty());
    }

    fn hold_with(ids: Vec<i64>) -> MassDeleteHold {
        let board = crate::drive::MassDeleteBoard::default();
        board.record_with_ids(
            Some((ids.len() as u64, HoldReason::Ratio, 500)),
            HeldIds::new(ids),
            "2026-11-01 10:00:00",
        );
        board.current().unwrap()
    }

    #[test]
    fn the_token_depends_on_the_four_fields_and_the_set_of_ids() {
        let base = hold_with(vec![11, 12, 13]);
        assert_eq!(token_of(&base), token_of(&hold_with(vec![13, 11, 12])));
        let token = token_of(&base);
        assert_eq!(token.len(), 16);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let mut variants = Vec::new();
        let mut v = base.clone();
        v.held += 1;
        variants.push(v);
        let mut v = base.clone();
        v.reason = HoldReason::EmptyListing;
        variants.push(v);
        let mut v = base.clone();
        v.drive_rows += 1;
        variants.push(v);
        let mut v = base.clone();
        v.since = "2026-11-01 10:00:01".into();
        variants.push(v);
        // ID の集合が 1 つ違う（件数は同じ）
        let mut v = base.clone();
        v.ids = HeldIds::new(vec![11, 12, 14]);
        variants.push(v);
        for variant in variants {
            assert_ne!(token_of(&variant), token);
        }
        // `checked_at` は印に入らない（保留が続くあいだ変わる）
        let mut v = base.clone();
        v.checked_at = "2026-11-01 10:10:00".into();
        assert_eq!(token_of(&v), token);
    }

    #[test]
    fn debug_output_never_lists_ids() {
        let ids: Vec<i64> = (900_001..900_121).collect();
        let hold = hold_with(ids.clone());
        let shown = format!("{hold:?}");
        assert!(shown.contains("HeldIds(len=120)"), "{shown}");
        assert!(
            !shown.contains("900001") && !shown.contains("900120"),
            "{shown}"
        );
        let (board, _clock, _stop) = board();
        board
            .enqueue(
                OpKind::DriveRelease,
                Some(token_of(&hold)),
                Some(HeldIds::new(ids)),
            )
            .unwrap();
        let req = board.take_queued().unwrap();
        let shown = format!("{req:?}");
        assert!(shown.contains("HeldIds(len=120)"), "{shown}");
        assert!(
            !shown.contains("900001") && !shown.contains("900120"),
            "{shown}"
        );
        assert!(!format!("{:?}", HeldIds::new(vec![900_005])).contains("900005"));
    }

    #[test]
    fn json_has_neither_ids_nor_paths_nor_sqlite_messages() {
        let (board, _clock, _stop) = board();
        let hold = hold_with((900_001..900_121).collect());
        board
            .enqueue(
                OpKind::DriveRelease,
                Some(token_of(&hold)),
                Some(hold.ids.clone()),
            )
            .unwrap();
        let queued_text = board.snapshot().to_json().to_string();
        assert!(!queued_text.contains("900001"), "{queued_text}");
        assert!(!queued_text.contains(&token_of(&hold)), "{queued_text}");
        let req = board.take_queued().unwrap();
        let report = CycleReport {
            stages: vec![StageReport {
                name: "index_drive",
                result: StageResult::Failed(
                    "DB の読み取りに失敗しました: no such table: photos (/srv/geoframe/data/catalog.db)"
                        .into(),
                ),
            }],
            photos: None,
        };
        board.finish(
            req.id,
            outcome_of(RunKind::Op(OpKind::DriveRelease), &report),
            stages_json(&report),
            None,
        );
        let text = board.snapshot().to_json().to_string();
        for forbidden in [
            "900001",
            "900120",
            "catalog.db",
            "/srv",
            "no such table",
            "photos",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}: {text}");
        }
        assert!(text.contains(r#""result":"failed""#), "{text}");
        let hold_text = hold_json(&hold).to_string();
        assert!(!hold_text.contains("900001"), "{hold_text}");
        assert_eq!(hold_json(&hold)["token"], token_of(&hold));
        assert_eq!(hold_json(&hold)["releasable"], true);
        let mut empty = hold;
        empty.reason = HoldReason::EmptyListing;
        assert_eq!(hold_json(&empty)["releasable"], false);
    }

    #[test]
    fn json_shape_of_a_finished_request_and_the_worker() {
        let (board, _clock, _stop) = board();
        board.enqueue(OpKind::Sync, None, None).unwrap();
        let queued = board.snapshot().worker_json();
        assert_eq!(queued["queued"]["kind"], "sync");
        assert_eq!(queued["running"], Value::Null);
        let req = board.take_queued().unwrap();
        let report = CycleReport {
            stages: vec![
                StageReport {
                    name: "index_local",
                    result: StageResult::Done(Counts(vec![("indexed", 3)])),
                },
                StageReport {
                    name: "index_drive",
                    result: StageResult::Skipped("Drive の鍵が無いので飛ばします"),
                },
            ],
            photos: None,
        };
        board.finish(
            req.id,
            outcome_of(RunKind::Op(OpKind::Sync), &report),
            stages_json(&report),
            Some(Report::Release { released: 2 }),
        );
        let json = board.snapshot().to_json();
        let record = &json["history"][0];
        assert_eq!(record["id"], 1);
        assert_eq!(record["outcome"], "done");
        assert_eq!(record["stages"][0]["counts"]["indexed"], 3);
        assert_eq!(record["stages"][1]["result"], "skipped");
        assert_eq!(
            record["stages"][1]["reason"],
            "Drive の鍵が無いので飛ばします"
        );
        assert_eq!(record["report"]["released"], 2);
        assert_eq!(json["worker"]["queued"], Value::Null);
    }

    fn one_stage(result: StageResult) -> CycleReport {
        CycleReport {
            stages: vec![StageReport {
                name: "index_drive",
                result,
            }],
            photos: None,
        }
    }

    #[test]
    fn drive_release_outcome_follows_the_table() {
        let kind = RunKind::Op(OpKind::DriveRelease);
        let counts = |pairs: Vec<(&'static str, u64)>| StageResult::Done(Counts(pairs));
        let cases: Vec<(StageResult, Outcome)> = vec![
            (StageResult::Skipped("鍵なし"), Outcome::Skipped),
            (StageResult::NotRun, Outcome::Skipped),
            (StageResult::Failed("x".into()), Outcome::Failed),
            (StageResult::Panicked, Outcome::Failed),
            // `counts()` は 0 でも鍵を入れる: 0 は done
            (
                counts(vec![
                    ("deletes_withheld", 0),
                    ("mass_delete_withheld", 0),
                    ("approved", 5),
                    ("deleted", 5),
                ]),
                Outcome::Done,
            ),
            (counts(vec![("mass_delete_withheld", 3)]), Outcome::Exceeded),
            (counts(vec![("deletes_withheld", 3)]), Outcome::Exceeded),
        ];
        for (result, expected) in cases {
            let shown = format!("{result:?}");
            assert_eq!(outcome_of(kind, &one_stage(result)), expected, "{shown}");
        }
        // 段が無い（想定外）は失敗
        let empty = CycleReport {
            stages: Vec::new(),
            photos: None,
        };
        assert_eq!(outcome_of(kind, &empty), Outcome::Failed);
    }

    #[test]
    fn multi_stage_outcome_is_partial_skipped_or_done() {
        let stage = |name, result| StageReport { name, result };
        let report = |stages| CycleReport {
            stages,
            photos: None,
        };
        let done = || StageResult::Done(Counts::default());
        for kind in [
            RunKind::Op(OpKind::Sync),
            RunKind::Op(OpKind::QualityScan),
            RunKind::Cycle,
        ] {
            assert_eq!(
                outcome_of(kind, &report(vec![stage("a", done()), stage("b", done())])),
                Outcome::Done
            );
            // 失敗・パニックが 1 つでもあれば partial
            assert_eq!(
                outcome_of(
                    kind,
                    &report(vec![
                        stage("a", done()),
                        stage("b", StageResult::Failed("x".into()))
                    ])
                ),
                Outcome::Partial
            );
            assert_eq!(
                outcome_of(
                    kind,
                    &report(vec![stage("a", StageResult::Panicked), stage("b", done())])
                ),
                Outcome::Partial
            );
            // 省略は失敗ではない
            assert_eq!(
                outcome_of(
                    kind,
                    &report(vec![
                        stage("a", done()),
                        stage("b", StageResult::Skipped("鍵なし"))
                    ])
                ),
                Outcome::Done
            );
            // 全部 NotRun（停止）は skipped。一部だけなら done
            assert_eq!(
                outcome_of(
                    kind,
                    &report(vec![
                        stage("a", StageResult::NotRun),
                        stage("b", StageResult::NotRun)
                    ])
                ),
                Outcome::Skipped
            );
            assert_eq!(
                outcome_of(
                    kind,
                    &report(vec![stage("a", done()), stage("b", StageResult::NotRun)])
                ),
                Outcome::Done
            );
        }
    }

    #[test]
    fn kinds_and_outcomes_have_stable_names() {
        let kinds = [
            (OpKind::Sync, "sync"),
            (OpKind::QualityScan, "quality_scan"),
            (OpKind::DriveRelease, "drive_release"),
            (OpKind::AiRetry, "ai_retry"),
            (OpKind::AiReleasePause, "ai_release_pause"),
        ];
        for (kind, name) in kinds {
            assert_eq!(kind.as_str(), name);
        }
        assert_eq!(RunKind::Cycle.as_str(), "cycle");
        let outcomes = [
            (Outcome::Done, "done"),
            (Outcome::Partial, "partial"),
            (Outcome::Failed, "failed"),
            (Outcome::Skipped, "skipped"),
            (Outcome::Exceeded, "exceeded"),
            (Outcome::HoldChanged, "hold_changed"),
            (Outcome::Retried, "retried"),
            (Outcome::Released, "released"),
        ];
        for (outcome, name) in outcomes {
            assert_eq!(outcome.as_str(), name);
        }
    }
}
