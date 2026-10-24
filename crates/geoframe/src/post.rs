//! `POST` の入口（P3 / T3.1）: ルーティング、CSRF 対策、本文の検査、エラー応答の対応。現行
//! `handlers.py` の `do_POST`・`_csrf_ok` と**同じ順序・同じ状態コード**（棚卸し §1.2）。
//!
//! # 順序
//!
//! 1. （[`crate::server`] の入口）未実装のメソッド（`OPTIONS` を含む）は 501、Host 検査（403）。
//! 2. ルートが無ければ 403（[`route_of`]。`/api/blog/` 配下も含め、未知の POST は一律 403＝現行の
//!    `send_error(403)`。`GET` の未知の `/api/blog/…` だけが 404 で、POST は違う）。
//! 3. `Content-Type: application/json` 必須（違えば 403）。
//! 4. `Origin` があれば `Host` と同じ（違えば 403。無い・空なら通す）。
//! 5. `Content-Length` 必須（無い・数でなければ 400）。
//! 6. 上限 [`MAX_POST_BYTES`] 超は**本文を読まずに** 413 を返して接続を閉じる。
//! 7. JSON として解釈できなければ 400、オブジェクトでなければ 400。
//! 8. 各ルートの処理。
//!
//! 5・6 は、巨大・不正な `Content-Length`（`u64` を超える・数字でない・重複して値が違う・`+`付きなど）を
//! axum/hyper が**この関数に届く前に**断るので、その入力は 3〜6 の順序・状態コード
//! どおりにならない（431・400 で接続が閉じる）。Python が書く入力を断る向きの差だけで、書き込みは起きない。
//! server.md §17.1.1、試験は `tests/update_api.rs` の `content_length_boundaries_at_the_http_layer`。
//!
//! CORS ヘッダは一切返さない。`OPTIONS` は 501 のまま（プリフライトを失敗させる。CSRF 対策の一部）。
//!
//! # エラー応答
//!
//! [`ApiError`] → `BadRequest` 400、`NotFound` 404、`Busy` 409（旅行ブログの書き出し中。T3.6）、
//! `Unavailable` 503（書き込みが無効。Python に無い）、`Internal` 500（定型文だけ。詳細は**ログにだけ**
//! 出し、応答には出さない）。本文は `{"ok": false, "error": "…"}`。
//!
//! # 書き込みの共有
//!
//! 更新は [`AppState::writer`]（取り込みが有効なときだけある、同期サイクルと共有の writer）を通す。
//! 無ければ書かずに 503。writer の呼び出しはブロックするので、`spawn_blocking` に包む。

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::{Map, Value};

use crate::server::{AppState, error_response, json_response};
use crate::{update, webpath};

/// 本文の上限（現行の `MAX_POST_BYTES`）。
pub const MAX_POST_BYTES: u64 = 64 * 1024;

/// 受け付ける POST のルート（現行の `_post_routes`）。T3.1 で動くのは `SetLoc`・`SetFlag` だけで、
/// 残りは CSRF 対策までを現行と同じに通し、処理は後のタスクで入れる（それまでは 503）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    SetLoc,
    SetFlag,
    Rotate,
    BlogSave,
    BlogExport,
}

impl Route {
    /// アクセスログに出すテンプレート（実際のパスの代わり）。
    pub fn label(self) -> &'static str {
        match self {
            Self::SetLoc => "/api/admin/setloc",
            Self::SetFlag => "/api/admin/setflag",
            Self::Rotate => "/api/admin/rotate",
            Self::BlogSave => "/api/blog/save",
            Self::BlogExport => "/api/blog/export",
        }
    }
}

/// リクエストのパスから POST のルートを選ぶ。現行は `_norm_path` した結果の**完全一致**なので、
/// `/api/admin/setloc/`・`//api/admin/setloc`・`/api/admin/%73etloc`・クエリ付きも当たる。
/// 当たらなければ `None`（呼び出し側が 403）。
pub fn route_of(raw_path: &str) -> Option<Route> {
    let normalized = webpath::normalize(raw_path)?;
    match normalized.path.as_str() {
        "/api/admin/setloc" => Some(Route::SetLoc),
        "/api/admin/setflag" => Some(Route::SetFlag),
        "/api/admin/rotate" => Some(Route::Rotate),
        "/api/blog/save" => Some(Route::BlogSave),
        "/api/blog/export" => Some(Route::BlogExport),
        _ => None,
    }
}

/// 更新ハンドラのエラー。応答の状態コードと文面は [`ApiError::into_response`]。
#[derive(Debug)]
pub enum ApiError {
    /// 入力が不正（400）。文面は利用者向けで、入力の値は `id` 以外入れない。
    BadRequest(String),
    /// 対象が無い（404）。
    NotFound(String),
    /// 実行中（409）。旅行ブログの書き出し（T3.6）が使う。
    #[allow(dead_code)]
    Busy(String),
    /// このタスクではまだ無いルート（T3.2 回転、T3.6 旅行ブログの保存・書き出し）。GET の未実装と同じ 503。
    NotYetImplemented,
    /// 書き込みが無効（`GEOFRAME_INGEST=0`、またはガードで取り込みを断った）ので何も書かなかった（503）。
    Unavailable,
    /// それ以外（500）。応答は定型文。ここの文字列は**ログにだけ**出す（SQLite のメッセージは表名・列名
    /// までで値を含まない `WriteError` の `Display`。ファイル名・座標・地名を入れない）。
    Internal(String),
}

impl From<crate::writer::WriteError> for ApiError {
    fn from(e: crate::writer::WriteError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl ApiError {
    fn into_response(self, route: Route) -> Response {
        match self {
            Self::BadRequest(m) => error_response(StatusCode::BAD_REQUEST, &m),
            Self::NotFound(m) => error_response(StatusCode::NOT_FOUND, &m),
            Self::Busy(m) => error_response(StatusCode::CONFLICT, &m),
            Self::NotYetImplemented => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "この機能は Rust 版ではまだ使えません",
            ),
            Self::Unavailable => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "書き込みが無効なので更新できません（この Rust 版は取り込みが無効です）",
            ),
            Self::Internal(detail) => {
                tracing::error!("[api] {} 内部エラー: {detail}", route.label());
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        }
    }
}

/// ヘッダの最初の値（現行の `self.headers.get`）。UTF-8 でなければ置き換え文字にする。
fn first_header(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

/// Python の `urllib.parse.urlparse(url).netloc`。`//` で始まる部分の、次の `/`・`?`・`#` までの文字列。
/// `scheme:` の前置きは、Python と同じ規則（先頭が英字で、`:` までが英数字と `+-.`）で読み飛ばす。
/// 先頭の制御文字・空白と、タブ・改行は Python が取り除く。角括弧が合わない（Python は `ValueError`）ときは
/// `None`（拒否する）。
fn netloc(url: &str) -> Option<String> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    if let Some(i) = rest.find(':')
        && i > 0
        && rest.starts_with(|c: char| c.is_ascii_alphabetic())
        && rest[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        rest = &rest[i + 1..];
    }
    let Some(after) = rest.strip_prefix("//") else {
        return Some(String::new());
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    if netloc.contains('[') != netloc.contains(']') {
        return None;
    }
    Some(netloc.to_owned())
}

/// 現行の `_csrf_ok`。通らなければそのまま返す応答（403）。
fn csrf_check(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let content_type = first_header(headers, header::CONTENT_TYPE).unwrap_or_default();
    let content_type = content_type.split(';').next().unwrap_or("").trim();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            "Content-Type: application/json が必要です",
        )));
    }
    if let Some(origin) = first_header(headers, header::ORIGIN)
        && !origin.is_empty()
    {
        let host = first_header(headers, header::HOST).unwrap_or_default();
        if netloc(&origin).as_deref() != Some(host.trim()) {
            return Err(Box::new(error_response(
                StatusCode::FORBIDDEN,
                "クロスオリジンは拒否されます",
            )));
        }
    }
    Ok(())
}

/// `Content-Length`。無い・数でない（hyper が弾く形以外では負数）なら `None`。
fn content_length(headers: &HeaderMap) -> Option<u64> {
    first_header(headers, header::CONTENT_LENGTH)?
        .trim()
        .parse()
        .ok()
}

/// 本文を読む前の検査（Content-Type・Origin・Content-Length・サイズ）。通れば本文の長さ。
fn pre_body_check(headers: &HeaderMap) -> Result<u64, Box<Response>> {
    csrf_check(headers)?;
    let Some(length) = content_length(headers) else {
        return Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "Content-Length が必要です",
        )));
    };
    if length > MAX_POST_BYTES {
        // 本文を読まずに返すので、未読のデータが残る。接続を再利用させない（`Connection: close`）
        let mut response = error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("本文が大きすぎます（上限{MAX_POST_BYTES}バイト）"),
        );
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
        return Err(Box::new(response));
    }
    Ok(length)
}

/// `POST` を処理する。`route` は [`route_of`] が選んだもの（入口の検査で、ルートが無い POST は 403 済み）。
pub async fn handle(state: &AppState, route: Route, headers: &HeaderMap, body: Body) -> Response {
    let length = match pre_body_check(headers) {
        Ok(length) => length,
        Err(response) => return *response,
    };
    let bytes = match axum::body::to_bytes(body, length as usize).await {
        Ok(bytes) => bytes,
        // 宣言より短いまま切れた・読めなかった。現行は読み切れず接続が切れる。JSON にならないので 400
        Err(_) => return json_syntax_error(),
    };
    let payload: Map<String, Value> = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => map,
        Ok(_) => {
            return error_response(StatusCode::BAD_REQUEST, "JSONオブジェクトを送ってください");
        }
        Err(_) => return json_syntax_error(),
    };
    let writer = state.writer();
    let result = tokio::task::spawn_blocking(move || match route {
        Route::SetLoc => update::set_loc(&payload, writer.as_ref()),
        Route::SetFlag => update::set_flag(&payload, writer.as_ref()),
        // 後のタスクで入れる（T3.2 回転、T3.6 旅行ブログの保存・書き出し）
        Route::Rotate | Route::BlogSave | Route::BlogExport => Err(ApiError::NotYetImplemented),
    })
    .await
    .unwrap_or_else(|_| Err(ApiError::Internal("更新の処理が異常終了しました".into())));
    match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(e) => {
                ApiError::Internal(format!("応答を JSON にできません: {e}")).into_response(route)
            }
        },
        Err(e) => e.into_response(route),
    }
}

fn json_syntax_error() -> Response {
    error_response(StatusCode::BAD_REQUEST, "JSONとして解釈できません")
}
