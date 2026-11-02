//! 管理画面の「運用」タブの API（docs/admin-ops/design.md の PR ①）。
//!
//! - `GET /api/admin/ops`: 同期ワーカーの掲示板（[`crate::ops::Board`]）と大量削除の保留の記憶
//!   （[`crate::drive::MassDeleteBoard`]）の写しを返す。**DB は読まない**（3 秒ごとの読み直しが DB を引かない）。
//!   `INGEST=0`（掲示板が無い）でも 200 で、`ingest: false` と空の項目だけを返す。
//! - `POST /api/admin/ops/sync`・`/quality-scan`・`/drive-release`: 要求を掲示板に 1 つ入れるだけで、実行は同期ワーカーの
//!   1 本のスレッドが順に行う（応答は受付の `request`。進み具合は `GET` で読む）。
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

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::drive::{HoldReason, MassDeleteBoard};
use crate::ops::{self, Board, EnqueueError, OpKind, Queued};
use crate::post::ApiError;
use crate::server::{AppState, error_response, json_response};

/// 応答・エラーの符号（固定。値は入れない）。
const BAD_PARAMS: &str = "bad_params";
const BUSY: &str = "busy";
const TOO_SOON: &str = "too_soon";
const NO_HOLD: &str = "no_hold";
const NOT_RELEASABLE: &str = "not_releasable";
const HOLD_CHANGED: &str = "hold_changed";

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
        // 再試行できる種類（PR ② で埋める）
        "ai": { "retry_kinds": [] },
        "history": snapshot.history_json(),
    })
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
/// 判定の順序: `token` の形（400）→ 掲示板が無ければ 503 → 保留が無い（409 `no_hold`）→ 一覧が空のための保留
/// （409 `not_releasable`）→ 印の不一致（409 `hold_changed`）→ 受付（409 `busy`・`too_soon`、503）。実行の直前にもワーカーが
/// 印を照らし直し、削除は承認した行の集合に限られる（`Allow::Only`）。
pub fn drive_release(
    payload: &Map<String, Value>,
    ops: Option<&Arc<Board>>,
    mass_delete: Option<&Arc<MassDeleteBoard>>,
) -> Result<Value, ApiError> {
    let token = parse_token(payload)?;
    let board = board_or_unavailable(ops)?;
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
