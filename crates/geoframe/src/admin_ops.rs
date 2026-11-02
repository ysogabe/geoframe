//! 管理画面の「運用」タブの API（docs/admin-ops/design.md の PR ①）。
//!
//! - `GET /api/admin/ops`: 同期ワーカーの掲示板（[`crate::ops::Board`]）と大量削除の保留の記憶
//!   （[`crate::drive::MassDeleteBoard`]）の写しを返す。**DB は読まない**（3 秒ごとの読み直しが DB を引かない）。
//!   `INGEST=0`（掲示板が無い）でも 200 で、`ingest: false` と空の項目だけを返す。
//! - `POST /api/admin/ops/sync`・`/quality-scan`・`/drive-release`: 要求を掲示板に 1 つ入れるだけで、実行は同期ワーカーの
//!   1 本のスレッドが順に行う（応答は受付の `request`。進み具合は `GET` で読む）。
//! - `POST /api/admin/ops/ai-retry`・`/ai-release-pause`: 同期ワーカーの待ち行列には入れず、POST の中（`spawn_blocking`）で
//!   CLI（`geoframe ai-jobs retry`・`release-pause`）と同じ関数（[`crate::ai::ops`]）を writer 経由で呼び、結果を応答と履歴に
//!   残す。再試行の handler は背景ワーカーとは別のインスタンス、同時に動くのは 1 つだけ（[`AiOps`]）。外部へは送らない。
//!
//! # 安全面
//!
//! - **`token`（印）は認可ではなく、古い画面の防止である。** 認証の無い LAN では、`GET /api/admin/ops` を読める相手は誰でも
//!   `token` を得て `drive-release` を POST できる。印が守るのは「保留が変わる前に見た件数で解除してしまう」ことだけで、相手の
//!   正当性は ADR 0008 の前提（家庭 LAN・外部公開時は Cloudflare Access）に任せる。`setflag`・`rotate`・選別の `confirm` と
//!   同じ境界だが、`drive-release` は管理 API で最も破壊的な書き込みなので、ここに明記しておく。
//! - 応答・ログに保留した行の ID・パス・SQLite の文面を出さない。エラーの本文は固定の符号だけ。
//! - 判定の順序は `update::set_flag` と同じ: 本文の形 400 → 掲示板が無ければ 503 → 状態による 409。
//!
//! 運用固有の失敗は `{"ok": false, "error": "<符号>"}`（409 `busy`・`too_soon`・`no_hold`・`not_releasable`・`hold_changed`）。
//! 共通の入口（CSRF 403・413・400・500）は `post.rs` のまま。

use std::sync::{Arc, Mutex, TryLockError};

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::ai::jobs::JobKind;
use crate::ai::ops::{PauseTarget, ReleaseError, RetryTarget};
use crate::ai::settings::TaskKind;
use crate::ai::worker::{Clock, Handler};
use crate::config::Config;
use crate::drive::{HoldReason, MassDeleteBoard};
use crate::ops::{self, Board, EnqueueError, OpKind, Outcome, Queued, Report};
use crate::post::ApiError;
use crate::server::{AppState, error_response, json_response};
use crate::writer::Writer;

/// 応答・エラーの符号（固定。値は入れない）。
const BAD_PARAMS: &str = "bad_params";
const BUSY: &str = "busy";
const TOO_SOON: &str = "too_soon";
const NO_HOLD: &str = "no_hold";
const NOT_RELEASABLE: &str = "not_releasable";
const HOLD_CHANGED: &str = "hold_changed";
const CONFIG_UNAVAILABLE: &str = "config_unavailable";

// ---------------------------------------------------------------- GET

/// `GET /api/admin/ops`。掲示板と保留の記憶のスナップショットだけで組み立てる（DB は読まない）。
pub async fn response(state: &AppState) -> Response {
    let body = build(state);
    match serde_json::to_vec(&body) {
        Ok(bytes) => json_response(StatusCode::OK, bytes),
        Err(e) => {
            tracing::error!("運用の状態を JSON にできません: {e}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "内部エラーが発生しました",
            )
        }
    }
}

fn build(state: &AppState) -> Value {
    let Some(board) = &state.ops else {
        return json!({
            "ok": true,
            "ingest": false,
            "worker": null,
            "hold": null,
            "ai": null,
            "history": [],
        });
    };
    let snapshot = board.snapshot();
    json!({
        "ok": true,
        "ingest": true,
        "poll_minutes": poll_minutes(state.config.drive_poll()),
        "worker": snapshot.worker_json(),
        "hold": state
            .mass_delete
            .as_ref()
            .and_then(|b| b.current())
            .map(|hold| ops::hold_json(&hold)),
        // 再試行できる種類（再試行用の handlers に登録のあるもの。起動時に固定。JobKind の定義順）
        "ai": { "retry_kinds": retry_kinds(&state.ai_retry_handlers) },
        "history": snapshot.history_json(),
    })
}

/// 再試行用の handlers に登録のある種類の名前（`JobKind` の定義順、重複なし）。
fn retry_kinds(handlers: &[Arc<dyn Handler>]) -> Vec<&'static str> {
    JobKind::ALL
        .into_iter()
        .filter(|kind| handlers.iter().any(|h| h.kind() == *kind))
        .map(JobKind::as_str)
        .collect()
}

/// 同期の間隔（分）。割り切れるときは整数で出す（`10`）。
fn poll_minutes(poll: std::time::Duration) -> Value {
    let minutes = poll.as_secs_f64() / 60.0;
    if minutes.fract() == 0.0 && minutes < u64::MAX as f64 {
        json!(minutes as u64)
    } else {
        json!(minutes)
    }
}

// ---------------------------------------------------------------- POST

fn bad_request() -> ApiError {
    ApiError::BadRequest(BAD_PARAMS.into())
}

/// 掲示板が無い（`INGEST=0`）なら 503。
fn board_or_unavailable(ops: Option<&Arc<Board>>) -> Result<&Arc<Board>, ApiError> {
    ops.ok_or(ApiError::Unavailable)
}

/// `enqueue` の失敗を `ApiError` にする。停止の合図のあとは 503。
fn enqueue_error(e: EnqueueError) -> ApiError {
    match e {
        EnqueueError::Busy => ApiError::Busy(BUSY.into()),
        EnqueueError::TooSoon => ApiError::Busy(TOO_SOON.into()),
        EnqueueError::Stopping => ApiError::Unavailable,
    }
}

/// 受け付けた要求の応答の `request`。
fn request_json(queued: &Queued) -> Value {
    json!({
        "id": queued.id,
        "kind": queued.kind.as_str(),
        "state": "queued",
    })
}

/// `POST /api/admin/ops/sync`。定期サイクルと同じ段を 1 回、いま回す要求。
pub fn sync(ops: Option<&Arc<Board>>) -> Result<Value, ApiError> {
    enqueue_plain(ops, OpKind::Sync)
}

/// `POST /api/admin/ops/quality-scan`。品質解析の一括（`geoframe quality-scan` と同じ）の要求。
pub fn quality_scan(ops: Option<&Arc<Board>>) -> Result<Value, ApiError> {
    enqueue_plain(ops, OpKind::QualityScan)
}

fn enqueue_plain(ops: Option<&Arc<Board>>, kind: OpKind) -> Result<Value, ApiError> {
    let board = board_or_unavailable(ops)?;
    let queued = board.enqueue(kind, None, None).map_err(enqueue_error)?;
    Ok(json!({ "ok": true, "request": request_json(&queued) }))
}

/// 本文の `token`: 16 桁の小文字 hex。
fn parse_token(payload: &Map<String, Value>) -> Result<String, ApiError> {
    payload
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| t.len() == 16 && t.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
        .map(str::to_owned)
        .ok_or_else(bad_request)
}

/// `POST /api/admin/ops/drive-release`。画面が見た保留（`token`）の行の集合に限って、大量削除の保留を解除する要求。
///
/// 判定の順序: `token` の形（400）→ 掲示板が無ければ 503 → 停止の合図のあとは 503 → 保留が無い（409 `no_hold`）→ 一覧が空のための保留
/// （409 `not_releasable`）→ 印の不一致（409 `hold_changed`）→ 受付（409 `busy`・`too_soon`、503）。実行の直前にもワーカーが
/// 印を照らし直し、削除は承認した行の集合に限られる（`Allow::Only`）。
pub fn drive_release(
    payload: &Map<String, Value>,
    ops: Option<&Arc<Board>>,
    mass_delete: Option<&Arc<MassDeleteBoard>>,
) -> Result<Value, ApiError> {
    let token = parse_token(payload)?;
    let board = board_or_unavailable(ops)?;
    // 停止の合図のあとは、保留の判定（409）より先に 503
    if board.is_stopping() {
        return Err(ApiError::Unavailable);
    }
    let hold = mass_delete
        .and_then(|b| b.current())
        .ok_or_else(|| ApiError::Busy(NO_HOLD.into()))?;
    if hold.reason != HoldReason::Ratio {
        return Err(ApiError::Busy(NOT_RELEASABLE.into()));
    }
    if ops::token_of(&hold) != token {
        return Err(ApiError::Busy(HOLD_CHANGED.into()));
    }
    let queued = board
        .enqueue(OpKind::DriveRelease, Some(token), Some(hold.ids.clone()))
        .map_err(enqueue_error)?;
    // 固定の語と件数だけ（ID・パスは出さない）
    tracing::warn!(
        "大量削除の解除を受け付けた: 承認 {} 件、理由 {}",
        hold.held,
        hold.reason.as_str()
    );
    Ok(json!({
        "ok": true,
        "request": request_json(&queued),
        "approved": hold.held,
    }))
}

// ---------------------------------------------------------------- AI の操作（POST の中で行う）

/// AI の操作（再試行・停止の解除）に要る部品。`AppState` から複製して `spawn_blocking` の閉包へ渡す。
pub struct AiOps {
    ops: Option<Arc<Board>>,
    writer: Option<Writer>,
    config: Arc<Config>,
    handlers: Arc<Vec<Arc<dyn Handler>>>,
    clock: Clock,
    lock: Arc<Mutex<()>>,
}

impl AiOps {
    pub fn of(state: &AppState) -> Self {
        Self {
            ops: state.ops.clone(),
            writer: state.writer(),
            config: Arc::clone(&state.config),
            handlers: Arc::clone(&state.ai_retry_handlers),
            clock: Arc::clone(&state.ai_clock),
            lock: Arc::clone(&state.ai_ops_lock),
        }
    }

    /// 掲示板・writer が無ければ 503（`INGEST=0`）、停止の合図のあとも 503。
    fn ready(&self) -> Result<(&Arc<Board>, &Writer), ApiError> {
        let board = board_or_unavailable(self.ops.as_ref())?;
        let writer = self.writer.as_ref().ok_or(ApiError::Unavailable)?;
        if board.is_stopping() {
            return Err(ApiError::Unavailable);
        }
        Ok((board, writer))
    }

    /// 同時に 1 つだけ。鎖は**この関数の中（`spawn_blocking` の閉包）で**取る。async の側で取ると、接続が切れて Future が
    /// 落ちたときに鎖だけが外れ、書き込みは走り続けてしまうため（取れなければ 409 `busy`）。
    fn exclusive<T>(&self, run: impl FnOnce() -> T) -> Result<T, ApiError> {
        let _guard = match self.lock.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(ApiError::Busy(BUSY.into())),
            // 鎖は何も守っていない（順序だけ）。毒は無視する
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        Ok(run())
    }
}

/// `target`: 文字列 `"all"` か、`{key: "<名前>"}` の 1 つだけを持つオブジェクト（名前は `parse` が知るものだけ）。
/// `Ok(None)` が全部、`Ok(Some(x))` が指定。
fn parse_target<T>(
    payload: &Map<String, Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, ApiError> {
    match payload.get("target") {
        Some(Value::String(all)) if all == "all" => Ok(None),
        Some(Value::Object(object)) if object.len() == 1 => object
            .get(key)
            .and_then(Value::as_str)
            .and_then(parse)
            .map(Some)
            .ok_or_else(bad_request),
        _ => Err(bad_request()),
    }
}

/// `POST /api/admin/ops/ai-retry`。`failed` の AI ジョブを再検査して振り分ける（`geoframe ai-jobs retry` と同じ）。
///
/// 判定の順序（`update::set_flag` と同じ）: 本文の形 400 → 掲示板・writer が無い／停止の合図のあと 503 → 別の AI の操作が
/// 実行中 409 `busy` → 実行。途中の種類で失敗したときは 500（定型文）で、済んだ種類の件数は履歴の `report` に残す。
pub fn ai_retry(payload: &Map<String, Value>, ai: &AiOps) -> Result<Value, ApiError> {
    let target = match parse_target(payload, "kind", JobKind::parse)? {
        None => RetryTarget::All,
        Some(kind) => RetryTarget::Kind(kind),
    };
    let (board, writer) = ai.ready()?;
    ai.exclusive(|| {
        let started_at = board.timestamp_now();
        match crate::ai::ops::retry(writer, &ai.config.ai, &ai.handlers, target, &ai.clock) {
            Ok(report) => {
                board.record_direct(
                    OpKind::AiRetry,
                    started_at,
                    Outcome::Retried,
                    Some(Report::Retry(report)),
                );
                // 固定の語と件数だけ
                tracing::warn!(
                    "AI の再試行: 戻した {} / 無効 {} / 成功済み {} / 処理が未登録 {} / 設定で使えない {}",
                    report.requeued,
                    report.obsolete,
                    report.satisfied,
                    report.unsupported,
                    report.policy_disabled
                );
                Ok(json!({ "ok": true, "report": Report::Retry(report).to_json() }))
            }
            Err((report, cause)) => {
                board.record_direct(
                    OpKind::AiRetry,
                    started_at,
                    Outcome::Failed,
                    Some(Report::Retry(report)),
                );
                tracing::warn!(
                    "AI の再試行が途中で失敗した（済んだ分: 戻した {} / 無効 {} / 成功済み {}）",
                    report.requeued,
                    report.obsolete,
                    report.satisfied
                );
                // 原因はログにだけ（SQLite のメッセージまでで値を含まない）。応答は固定の文
                tracing::error!("AI の再試行の失敗の原因: {cause}");
                Err(ApiError::Internal("AI の再試行に失敗しました".into()))
            }
        }
    })?
}

/// `POST /api/admin/ops/ai-release-pause`。Provider・用途の停止を明示解除する（`geoframe ai-jobs release-pause` と同じ）。
///
/// 判定の順序は [`ai_retry`] と同じ。設定が未指定・不正・その用途が未設定で今の識別が分からないときは、何も消さず 409
/// `config_unavailable`（全部の解除 `"all"` は設定によらない）。
pub fn ai_release_pause(payload: &Map<String, Value>, ai: &AiOps) -> Result<Value, ApiError> {
    let target = match parse_target(payload, "task", TaskKind::parse)? {
        None => PauseTarget::All,
        Some(task) => PauseTarget::Task(task),
    };
    let (board, writer) = ai.ready()?;
    ai.exclusive(|| {
        let started_at = board.timestamp_now();
        match crate::ai::ops::release_pause(writer, &ai.config.ai, target) {
            Ok(released) => {
                board.record_direct(
                    OpKind::AiReleasePause,
                    started_at,
                    Outcome::Released,
                    Some(Report::Release { released }),
                );
                tracing::warn!("AI の停止を解除した: {released} 件");
                Ok(json!({ "ok": true, "released": released }))
            }
            // 何も消していない拒否。他の 409 と同じく履歴には残さない
            Err(ReleaseError::ConfigUnavailable) => Err(ApiError::Busy(CONFIG_UNAVAILABLE.into())),
            Err(ReleaseError::Write(cause)) => {
                board.record_direct(OpKind::AiReleasePause, started_at, Outcome::Failed, None);
                tracing::warn!("AI の停止の解除に失敗した");
                tracing::error!("AI の停止の解除の失敗の原因: {cause}");
                Err(ApiError::Internal("AI の停止の解除に失敗しました".into()))
            }
        }
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn the_token_must_be_16_lowercase_hex_digits() {
        assert_eq!(
            parse_token(&payload(json!({"token": "0123456789abcdef"}))).unwrap(),
            "0123456789abcdef"
        );
        for bad in [
            json!({}),
            json!({"token": null}),
            json!({"token": 1234567890123456_u64}),
            json!({"token": "0123456789ABCDEF"}),
            json!({"token": "0123456789abcde"}),
            json!({"token": "0123456789abcdef0"}),
            json!({"token": "0123456789abcdeg"}),
            json!({"token": ""}),
        ] {
            assert!(
                matches!(parse_token(&payload(bad.clone())), Err(ApiError::BadRequest(m)) if m == BAD_PARAMS),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_poll_interval_is_an_integer_when_it_divides_evenly() {
        assert_eq!(poll_minutes(std::time::Duration::from_secs(600)), json!(10));
        assert_eq!(poll_minutes(std::time::Duration::from_secs(90)), json!(1.5));
    }
}
