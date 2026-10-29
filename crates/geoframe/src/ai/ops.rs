//! AI ジョブの管理操作（P4 / T4.1c。CLI の `geoframe ai-jobs` と、`/api/admin/status` の AI の集計）。
//!
//! # 読み取り
//!
//! [`report`]・[`insert_admin_status`] は**読み取りだけ**（読み取り専用の接続でよい）。AI の表が無い旧 DB
//! （`INGEST=0` の旧 Python の DB）でも 0 件として動く。出すのは**件数と固定の理由コード・用途名だけ**で、
//! 対象のキー・入力の hash・token・起動 ID・モデル名・URL・パス・写真の値は出さない。
//!
//! # 書き込み（サービス停止中だけ）
//!
//! [`retry`]・[`release_pause`] は writer を通す。呼ぶ側（CLI）が `GEOFRAME_INGEST=1`・検証環境のガード・
//! `.ingest.lock`（[`crate::ingest::start`]）を済ませてから使う。稼働中の `serve` はロックを持っているので、
//! その間は始まらない（稼働中拒否）。どちらも外部へは送らない。
//!
//! - `retry`: `failed` のジョブだけを対象に、**登録済みの handler** が現在の入力・既存の結果を再検査して
//!   `Requeue`／`Obsolete`／`Satisfied` に振り分ける（[`jobs::retry_failed`]）。成功済みの結果は戻さない。
//!   **検査できないもの**は戻さず `failed` のまま保持し、件数だけを報告する: handler が未登録の種類
//!   （いまの配布バイナリは全て。実 handler は T4.2・T4.4・T4.6 が登録する）と、設定で今は使えない用途
//!   （無効・クラウド未許可など）。
//! - `release_pause`: Provider・用途の停止（[`pause`]）の明示解除。用途を指定する解除は**今の設定の識別**に
//!   当たる停止だけ、`--all` は全部。設定が不正・未指定のときに、用途指定の解除は何もしない。

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use super::index;
use super::jobs::{self, ErrorCode, JobKind, JobState, RetryScope};
use super::pause::{self, PauseInfo};
use super::settings::{AiConfig, AiConfigError, Disabled, TaskKind};
use super::waits;
use super::worker::{Clock, Handler, task_for};
use crate::writer::{WriteError, Writer};

/// 設定の状態（固定のコードだけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigState {
    /// `GEOFRAME_AI_CONFIG` が無い（新しい外部 AI 呼び出しは無効）。
    Unset,
    Invalid(AiConfigError),
    Ready,
}

/// 用途が停止（[`pause`]）にかかっているか。設定で使える用途だけを評価する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPause {
    /// かかっていない（設定で使えない用途も、ここでは評価しないのでこれ）。
    None,
    /// Provider 全体または用途の停止にかかっている（範囲と固定の理由コードだけ）。
    Paused(pause::Paused),
    /// 停止の表を読めず、確認できない（送信側は読めないとき送らない）。
    Unknown,
}

/// 用途ごとの状態（設定で使えるか・停止中か・停止中の待ちの件数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskReport {
    pub task: TaskKind,
    /// `Err` は設定で使えない理由。
    pub gate: Result<(), Disabled>,
    pub pause: TaskPause,
    /// 停止中のとき、その用途の待ちのジョブ（`pending`＋`deferred`。`location_geocode` は `location_suggest`
    /// に数える）の件数。停止中でないときは `Some(0)`。ジョブの件数を集計できなかったときは `None`（不明。
    /// 0 件とは区別する。ジョブの表が無い旧 DB は集計できているので `Some(0)`）。
    pub waiting: Option<i64>,
}

/// `ai-jobs status` と STATUS が使う、用途の停止の判定（送信側 [`pause::status`] と同じ）。設定で使えない用途は
/// 評価せず [`TaskPause::None`]。停止の表を読めなければ [`TaskPause::Unknown`]（送信側は送らない）。
pub fn task_pause(conn: &Connection, ai: &AiConfig, task: TaskKind) -> TaskPause {
    match (ai.gate(task, true), ai.settings()) {
        (Ok(_), Some(settings)) => match pause::status(conn, settings, task) {
            Ok(None) => TaskPause::None,
            Ok(Some(paused)) => TaskPause::Paused(paused),
            Err(_) => TaskPause::Unknown,
        },
        _ => TaskPause::None,
    }
}

/// 用途ごとの状態を読む（読み取りだけ）。`counts` は [`jobs::status_counts`] で、集計できなかったときは `None`。
fn task_reports(
    conn: &Connection,
    ai: &AiConfig,
    counts: Option<&[(JobKind, JobState, i64)]>,
) -> Vec<TaskReport> {
    TaskKind::ALL
        .into_iter()
        .map(|task| {
            let gate = ai.gate(task, true).map(|_| ());
            let pause = task_pause(conn, ai, task);
            let waiting = if matches!(pause, TaskPause::Paused(_)) {
                counts.map(|counts| {
                    counts
                        .iter()
                        .filter(|(kind, state, _)| {
                            task_for(*kind) == task
                                && matches!(state, JobState::Pending | JobState::Deferred)
                        })
                        .map(|(_, _, n)| n)
                        .sum()
                })
            } else {
                Some(0)
            };
            TaskReport {
                task,
                gate,
                pause,
                waiting,
            }
        })
        .collect()
}

/// 状態の集計（件数と固定の理由だけ）。
#[derive(Debug, Clone)]
pub struct Report {
    pub config: ConfigState,
    /// 用途ごとの今の使えるか・停止にかかっているか（`writer` がある起動として評価）。
    pub tasks: Vec<TaskReport>,
    /// 種類・状態ごとの件数。
    pub jobs: Vec<(JobKind, JobState, i64)>,
    /// 後回し・失敗・待機中のジョブの理由コードごとの件数（状態、固定の理由コード）。
    pub reasons: Vec<(JobState, &'static str, i64)>,
    pub pauses: Vec<PauseInfo>,
    /// まだ有効な共有の一時待機（今の設定の用途に結び直したもの。[`waits::view`]）。
    pub waits: waits::WaitView,
    /// 検索索引（空間別ベクトル）の状態（件数だけ。空間の表が無い旧 DB でも読める）。
    pub index: index::IndexStatus,
    /// 公開前の空間（設定が指す画像 Embedding の空間で、まだ検索に使われていないもの）の進み具合。設定が無い・
    /// 空間が未登録・既に有効なときは `None`（CLI は `GEOFRAME_AI_CONFIG` を付けて実行したときだけ分かる）。
    pub building: Option<index::Building>,
    pub location_holds: Vec<(&'static str, i64)>,
}

/// 設定の状態。
pub fn config_state(ai: &AiConfig) -> ConfigState {
    match ai.invalid() {
        Some(code) => ConfigState::Invalid(code),
        None if ai.settings().is_some() => ConfigState::Ready,
        None => ConfigState::Unset,
    }
}

/// 状態を読む（読み取りだけ。表が無ければ 0 件）。
pub fn report(conn: &Connection, ai: &AiConfig) -> rusqlite::Result<Report> {
    report_at(conn, ai, jobs::unix_now())
}

/// [`report`] の、待ちの期限を見る時刻（UTC の Unix 秒）を指定できる形。
pub fn report_at(conn: &Connection, ai: &AiConfig, now: i64) -> rusqlite::Result<Report> {
    let index = index::status(conn)?;
    let active = match &index.active {
        index::ActiveState::Active(space_id) => Some(space_id.as_str()),
        _ => None,
    };
    let building = index::building_in(
        conn,
        super::embedding::configured_identity(ai, TaskKind::ImageEmbedding).as_ref(),
        active,
    )?;
    let job_counts = jobs::status_counts(conn)?;
    Ok(Report {
        config: config_state(ai),
        tasks: task_reports(conn, ai, Some(&job_counts)),
        jobs: job_counts,
        reasons: reason_counts(conn)?,
        pauses: pause::list(conn)?,
        waits: waits::view(conn, now, &waits::Identities::from_config(ai))?,
        index,
        building,
        location_holds: super::location_suggest::hold_counts(conn)?,
    })
}

/// 待機・後回し・失敗のジョブの理由コードごとの件数。保存された文字列は固定の [`ErrorCode`] だけを通す
/// （壊れた値は `other` にまとめ、そのまま出さない）。
fn reason_counts(conn: &Connection) -> rusqlite::Result<Vec<(JobState, &'static str, i64)>> {
    if !jobs::object_exists(conn, "table", "ai_jobs")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT state, last_error_code, COUNT(*) FROM ai_jobs
         WHERE state IN ('pending','deferred','failed') AND last_error_code<>''
         GROUP BY state, last_error_code ORDER BY 1, 2",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut merged: BTreeMap<(&'static str, &'static str), i64> = BTreeMap::new();
    for row in rows {
        let (state, code, count) = row?;
        let Some(state) = JobState::parse(&state) else {
            continue;
        };
        let code = ErrorCode::parse(&code).map_or("other", ErrorCode::as_str);
        *merged.entry((state.as_str(), code)).or_insert(0) += count;
    }
    Ok(merged
        .into_iter()
        .filter_map(|((state, code), count)| JobState::parse(state).map(|s| (s, code, count)))
        .collect())
}

/// `GET /api/admin/status` へ足す AI のキー（Rust だけが返す追加フィールド。既存のキーの意味・値は変えない）。
/// 足すものは**あるときだけ**（AI の表も設定の誤りも停止も無い DB・Python 版の応答には 1 つも足さない）:
///
/// - `ai_config_invalid`: `{"code": 固定コード, "disabled_tasks": 無効になった用途の数}`（設定が不正のとき）。
/// - `ai_jobs`: `{"total", "by_state": {状態: 件数}, "by_kind": {種類: {状態: 件数}}, "reasons": {状態: {理由: 件数}}}`。
///   理由は固定のコード（`clock_rewound`・`budget_exhausted`・`provider_paused`・`interrupted` など）だけ。
/// - `ai_pauses`: `[{"task", "scope", "reason"}]`（Provider・用途の停止。解除は CLI）。
/// - `ai_task_blocks`: `[{"task", "scope", "reason", "waiting"}]`（設定で使える用途のうち、Provider・用途の停止にかかって
///   送られていないもの。`ai_pauses` が停止を起こした用途だけなのに対し、同じ停止にかかる**全用途**。`scope` は
///   `provider`／`task`、`waiting` はその用途の待ち＝`pending`＋`deferred` の件数。
///   ジョブの件数を集計できなかったときは `null`＝不明で、停止の情報（`task`・`scope`・`reason`〕は残す）。停止にかかる用途が無ければ
///   足さない。
/// - `ai_waits`: `{"items": [{"task", "scope": "entrance"|"service", "reason", "until": "YYYY-MM-DDTHH:MM:SSZ"}],
///   "unidentified": 件数}`（まだ有効な共有の一時待機。`items` は今の設定の用途に結び直したもので、`unidentified` は
///   設定が使えず結び直せない件数。どちらも空ならキーを足さない。`unidentified` は 0 なら出さない。接続先・モデル・
///   サービス ID・ハッシュ・Provider の自由文は返さない。期限は UTC）。
///
/// 読み取りの失敗は STATUS 全体を壊さない（足さないだけ）。
pub fn insert_admin_status(
    out: &mut Map<String, Value>,
    conn: &Connection,
    ai: &AiConfig,
    wait_ids: &waits::Identities,
    now: i64,
) {
    if let Ok(holds) = super::location_suggest::hold_counts(conn)
        && !holds.is_empty()
    {
        out.insert(
            "ai_location_holds".into(),
            json!(holds.into_iter().collect::<BTreeMap<_, _>>()),
        );
    }
    if let Some(code) = ai.invalid() {
        out.insert(
            "ai_config_invalid".into(),
            json!({"code": code.code(), "disabled_tasks": TaskKind::ALL.len()}),
        );
    }
    let counts = jobs::status_counts(conn);
    if let Ok(counts) = counts.as_deref()
        && !counts.is_empty()
    {
        let mut by_state: BTreeMap<&str, i64> = BTreeMap::new();
        let mut by_kind: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
        let mut total = 0;
        for &(kind, state, count) in counts {
            *by_state.entry(state.as_str()).or_insert(0) += count;
            by_kind
                .entry(kind.as_str())
                .or_default()
                .insert(state.as_str(), count);
            total += count;
        }
        let mut reasons: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
        for (state, code, count) in reason_counts(conn).unwrap_or_default() {
            reasons
                .entry(state.as_str())
                .or_default()
                .insert(code, count);
        }
        out.insert(
            "ai_jobs".into(),
            json!({"total": total, "by_state": by_state, "by_kind": by_kind, "reasons": reasons}),
        );
    }
    if let Ok(pauses) = pause::list(conn)
        && !pauses.is_empty()
    {
        let list: Vec<Value> = pauses
            .iter()
            .map(|p| {
                json!({"task": p.task.as_str(), "scope": p.scope.as_str(), "reason": p.reason.as_str()})
            })
            .collect();
        out.insert("ai_pauses".into(), Value::Array(list));
    }
    // 停止にかかる全用途（待ちの件数が集計できないときは null＝不明）
    let blocks: Vec<Value> = task_reports(conn, ai, counts.as_deref().ok())
        .into_iter()
        .filter_map(|t| match t.pause {
            TaskPause::Paused(p) => Some(json!({
                "task": t.task.as_str(), "scope": p.scope.as_str(),
                "reason": p.reason.as_str(), "waiting": t.waiting
            })),
            _ => None,
        })
        .collect();
    if !blocks.is_empty() {
        out.insert("ai_task_blocks".into(), Value::Array(blocks));
    }
    if let Ok(view) = waits::view(conn, now, wait_ids)
        && !view.is_empty()
    {
        let items: Vec<Value> = view
            .waits
            .iter()
            .map(|w| {
                json!({"task": w.task.as_str(), "scope": w.scope.as_str(),
                       "reason": w.reason.as_str(), "until": waits::utc_iso(w.until)})
            })
            .collect();
        let mut value = Map::new();
        value.insert("items".into(), Value::Array(items));
        if view.unidentified > 0 {
            value.insert("unidentified".into(), json!(view.unidentified));
        }
        out.insert("ai_waits".into(), Value::Object(value));
    }
}

// ---------------------------------------------------------------- 手動の再試行

/// [`retry`] の対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryTarget {
    All,
    Kind(JobKind),
}

/// [`retry`] の集計（件数だけ）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetryReport {
    pub requeued: usize,
    pub obsolete: usize,
    pub satisfied: usize,
    /// handler が未登録の種類の `failed`（検査できないので戻さず保持）。
    pub unsupported: usize,
    /// 設定で今は使えない用途の `failed`（保持）。
    pub policy_disabled: usize,
}

fn failed_count(writer: &Writer, kind: JobKind) -> Result<usize, WriteError> {
    writer.call(move |conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND kind=?1",
            [kind.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| usize::try_from(n).unwrap_or(0))
    })
}

/// `failed` のジョブだけを再検査して振り分ける（上の「書き込み」）。外部へは送らない。
pub fn retry(
    writer: &Writer,
    ai: &AiConfig,
    handlers: &[Arc<dyn Handler>],
    target: RetryTarget,
    clock: &Clock,
) -> Result<RetryReport, WriteError> {
    let kinds: Vec<JobKind> = match target {
        RetryTarget::All => JobKind::ALL.to_vec(),
        RetryTarget::Kind(kind) => vec![kind],
    };
    let mut report = RetryReport::default();
    for kind in kinds {
        let Some(handler) = handlers.iter().find(|h| h.kind() == kind) else {
            report.unsupported += failed_count(writer, kind)?;
            continue;
        };
        if ai.gate(task_for(kind), true).is_err() {
            report.policy_disabled += failed_count(writer, kind)?;
            continue;
        }
        handler.prepare_retry().map_err(WriteError::Query)?;
        let (handler, clock) = (Arc::clone(handler), Arc::clone(clock));
        let done = writer.call(move |conn| {
            jobs::retry_failed(conn, clock(), RetryScope::Kind(kind), |c, key| {
                handler.recheck(c, key)
            })
        })?;
        report.requeued += done.requeued;
        report.obsolete += done.obsolete;
        report.satisfied += done.satisfied;
    }
    Ok(report)
}

// ---------------------------------------------------------------- Provider 停止の解除

/// [`release_pause`] の対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseTarget {
    /// 全ての停止（設定が不正でも、明示した操作として消す）。
    All,
    /// 今の設定でその用途に当たる Provider の停止と用途の停止。
    Task(TaskKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseError {
    /// 設定が未指定・不正・その用途が未設定で、今の識別が分からない（何も消さない）。
    ConfigUnavailable,
    Write,
}

/// Provider・用途の停止を明示解除する。消した件数を返す。
pub fn release_pause(
    writer: &Writer,
    ai: &AiConfig,
    target: PauseTarget,
) -> Result<usize, ReleaseError> {
    match target {
        PauseTarget::All => writer
            .call(pause::release_all)
            .map_err(|_| ReleaseError::Write),
        PauseTarget::Task(task) => {
            let settings = ai.settings().ok_or(ReleaseError::ConfigUnavailable)?;
            let keys: Vec<_> = [
                settings.provider_pause_key(task),
                settings.task_pause_key(task),
            ]
            .into_iter()
            .flatten()
            .collect();
            if keys.is_empty() {
                return Err(ReleaseError::ConfigUnavailable);
            }
            writer
                .call(move |conn| {
                    let mut removed = 0;
                    for key in &keys {
                        if pause::release(conn, key)? {
                            removed += 1;
                        }
                    }
                    Ok(removed)
                })
                .map_err(|_| ReleaseError::Write)
        }
    }
}
