//! `POST` の入口（P3 / T3.1）: ルーティング、CSRF 対策、本文の検査、エラー応答の対応。現行
//! `handlers.py` の `do_POST`・`_csrf_ok` と**同じ順序・同じ状態コード**（棚卸し §1.2）。
//!
//! # 順序
//!
//! 1. （[`crate::server`] の入口）未実装のメソッド（`OPTIONS` を含む）は 501、Host 検査（403）。
//! 2. ルートが無ければ 403（[`route_of`]。`/api/blog/` 配下も含め、未知の POST は一律 403＝現行の
//!    `send_error(403)`。`GET` の未知の `/api/blog/…` だけが 404 で、POST は違う）。
//! 3. `Content-Type: application/json` 必須（違えば 403）。
//! 4. `Origin` があれば `Host` と同じ（違えば 403。無い・空なら通す）。3・4 の値は現行と同じく**生バイトを
//!    Latin-1 として**読む（[`crate::csrf`]）。現行が例外にする `Origin`（角括弧の中が IPv6 でない等）は、現行は
//!    応答を返さず接続を閉じるが、Rust は 403。`Origin` の前後の空白・タブは hyper が RFC 9110 の OWS として
//!    取り除く（現行は末尾を残して 403 にする。既知の差。server.md §17.1.2）。
//! 5. `Content-Length` 必須（無い・数でなければ 400）。
//! 6. 上限 [`MAX_POST_BYTES`] 超は**本文を読まずに** 413 を返して接続を閉じる。
//! 7. JSON として解釈できなければ 400、オブジェクトでなければ 400。
//! 8. 各ルートの処理。
//!
//! 5・6 は、巨大・不正な `Content-Length`（`u64` を超える・数字でない・重複して値が違う・`+`付きなど）を
//! axum/hyper が**この関数に届く前に**断るので、その入力は 3〜6 の順序・状態コード
//! どおりにならない（431・400 で接続が閉じる）。これは Python が書く入力を断る向きの差で、書き込みは起きない。
//! 逆向き（Python が断るのに Rust が書く）は 2 つ見つかり、どちらも書く前に断る:
//! - hyper が通す先頭の `0` の長い表記（`0…037`）を、Python の `int()` は 4300 桁を超えると断る
//!   （400）。[`content_length`] が同じ上限で 400 にする。
//! - hyper の上限は現行 `http.server` より緩く、1 行 65536 バイト超・ヘッダ 100 個以上（現行は 431）、
//!   要求行 65536 バイト超（現行は 414）を通す。[`head_limits`] が書く前に断る。
//!
//! server.md §17.1.1、試験は `tests/update_api.rs` の `content_length_boundaries_at_the_http_layer`。
//!
//! CORS ヘッダは一切返さない。`OPTIONS` は 501 のまま（プリフライトを失敗させる。CSRF 対策の一部）。
//!
//! # エラー応答
//!
//! [`ApiError`] → `BadRequest` 400、`NotFound` 404、`Busy` 409（他の処理と重なった回転（T3.2）、旅行ブログの書き出し中（T3.6））、
//! `Unavailable` 503（書き込みが無効。Python に無い）、`Internal` 500（定型文だけ。詳細は**ログにだけ**
//! 出し、応答には出さない）。本文は `{"ok": false, "error": "…"}`。
//!
//! # 書き込みの共有
//!
//! 更新は [`AppState::writer`]（取り込みが有効なときだけある、同期サイクルと共有の writer）を通す。
//! 無ければ書かずに 503。writer の呼び出しはブロックするので、`spawn_blocking` に包む。

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::Response;
use serde_json::{Map, Value};

use crate::server::{AppState, error_response, json_response};
use crate::{blog_write, csrf, rotate, update, webpath};

/// 本文の上限（現行の `MAX_POST_BYTES`）。
pub const MAX_POST_BYTES: u64 = 64 * 1024;

/// 受け付ける POST のルート（現行の `_post_routes`）。
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
    /// 他の処理と重なった・実行中（409）。回転（T3.2）と、旅行ブログの書き出し（T3.6）が使う。
    Busy(String),
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

/// ヘッダの最初の値の生バイト（現行の `self.headers.get`）。現行は Latin-1 の `str` として読むので、
/// 文字にせず**バイトのまま**判定に渡す（[`crate::csrf`]）。
fn first_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&[u8]> {
    headers.get(name).map(HeaderValue::as_bytes)
}

/// 現行の `_csrf_ok` の判定（[`csrf::verdict`]）を、リクエストのヘッダに掛ける。
///
/// 差分試験（tests/csrf_python.rs）がこの関数を直接呼ぶ。
pub fn csrf_verdict(headers: &HeaderMap) -> csrf::Verdict {
    csrf::verdict(
        first_header(headers, header::CONTENT_TYPE),
        first_header(headers, header::ORIGIN),
        first_header(headers, header::HOST),
    )
}

/// 現行の `_csrf_ok`。通らなければそのまま返す応答（403）。
fn csrf_check(headers: &HeaderMap) -> Result<(), Box<Response>> {
    match csrf_verdict(headers) {
        csrf::Verdict::Allowed => Ok(()),
        csrf::Verdict::ContentType => Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            "Content-Type: application/json が必要です",
        ))),
        csrf::Verdict::CrossOrigin => Err(Box::new(error_response(
            StatusCode::FORBIDDEN,
            "クロスオリジンは拒否されます",
        ))),
    }
}

/// Python の `int()` が文字列を数に直すときの桁数の上限（`sys.int_info.default_max_str_digits`。3.11 以降
/// と、3.10.7・3.9.14・3.8.14・3.7.14 の既定）。符号・前後の空白・`_` は数えず、先頭の `0` は数える
/// （`"0"*4301`＝`ValueError`、`"0"*4298 + "37"`＝通る）。超えると `ValueError` で、現行の `do_POST` は 400。
const PYTHON_INT_MAX_DIGITS: usize = 4300;

/// `Content-Length`。無い・数でない（hyper が弾く形以外では負数）なら `None`。
///
/// hyper は先頭の `0` の長い表記（`0…037`）を通すが、現行は `int()` の桁数の上限で 400 にする。書かずに
/// 400 にしないと、Python が断る入力を Rust が書いてしまうので、ここで同じ上限を適用する。
fn content_length(headers: &HeaderMap) -> Option<u64> {
    let value = first_header(headers, header::CONTENT_LENGTH)?;
    let value = std::str::from_utf8(value).ok()?.trim();
    if value.len() > PYTHON_INT_MAX_DIGITS {
        return None;
    }
    value.parse().ok()
}

/// 現行 `http.server` が 1 行（要求行・ヘッダ 1 行。改行を含む）に許す長さ（`_MAXLINE`）。超えると要求行は 414、
/// ヘッダは 431 で、`do_POST` に届かない。
const PYTHON_MAX_LINE: usize = 65536;
/// 現行 `http.client.parse_headers` が許すヘッダ行の数（`_MAXHEADERS` = 100 で、ヘッダの終わりの空行も数える
/// ので、ヘッダは 99 個まで。100 個以上は 431）。
const PYTHON_MAX_HEADERS: usize = 99;

/// 現行が `do_POST` の前（行の読み込み）で断る大きさの要求を、`POST` が書く前に断る。hyper の既定の上限
/// （head 全体 約 400 KiB・ヘッダ 100 個）は現行より緩く、現行が 414・431 にする要求を Rust が書いてしまうため。
/// 見えるのは hyper が整えたあとの値なので、現行の長さは次のように見積もる（現行以上の長さにはならず、
/// 現行より**厳しいことはあっても緩くはならない**向きの見積もり。ただし値の前後の空白と、hyper が捨てた
/// 重複する `Content-Length` は見えず、その分だけは通る。server.md §17.1.1）。
/// 要求行は `POST ` + 対象 + ` HTTP/1.1\r\n`、ヘッダは `名前: 値\r\n`。
pub fn head_limits(uri: &Uri, headers: &HeaderMap) -> Option<Response> {
    let target = uri.path_and_query().map_or(1, |pq| pq.as_str().len());
    let too_large = |status, message: &str| {
        // 現行は閉じる（HTTP/1.0）。未読の本文が残るので、接続を再利用させない
        let mut response = error_response(status, message);
        response
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
        Some(response)
    };
    if "POST ".len() + target + " HTTP/1.1\r\n".len() > PYTHON_MAX_LINE {
        return too_large(StatusCode::URI_TOO_LONG, "要求行が長すぎます");
    }
    if headers.len() > PYTHON_MAX_HEADERS
        || headers
            .iter()
            .any(|(name, value)| name.as_str().len() + 2 + value.len() + 2 > PYTHON_MAX_LINE)
    {
        return too_large(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "ヘッダが大きすぎます",
        );
    }
    None
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
    let data_dir = state.data_root.as_ref().clone();
    let blog = std::sync::Arc::clone(&state.blog);
    let result = tokio::task::spawn_blocking(move || match route {
        Route::SetLoc => update::set_loc(&payload, writer.as_ref()),
        Route::SetFlag => update::set_flag(&payload, writer.as_ref()),
        Route::Rotate => rotate::rotate(&payload, writer.as_ref(), &data_dir),
        Route::BlogSave => blog_write::save(&payload, &blog, &data_dir, writer.as_ref()),
        Route::BlogExport => blog_write::export(&payload, &blog, &data_dir, writer.as_ref()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn with_length(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    /// Python の `int()` の桁数の上限（4300 桁）。先頭の `0` も数える。
    #[test]
    fn content_length_follows_pythons_int_digit_limit() {
        let padded = |digits: usize| format!("{}37", "0".repeat(digits - 2));
        assert_eq!(content_length(&with_length(&padded(4300))), Some(37));
        assert_eq!(content_length(&with_length(&padded(4301))), None);
        assert_eq!(content_length(&with_length(&padded(5000))), None);
        assert_eq!(content_length(&with_length("37")), Some(37));
        assert_eq!(content_length(&with_length("abc")), None);
        assert_eq!(content_length(&HeaderMap::new()), None);
    }

    /// 4301 桁は、値が上限を超えていても 413 でなく 400（桁数の検査が先）。
    #[test]
    fn too_many_digits_is_400_even_when_the_value_is_over_the_limit() {
        let over = format!("{}{}", "0".repeat(4301 - 5), MAX_POST_BYTES + 1);
        let response = *pre_body_check(&{
            let mut h = with_length(&over);
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            h
        })
        .unwrap_err();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn head_limits_match_the_line_and_header_limits_of_http_server() {
        let ok: Uri = "/api/admin/setloc".parse().unwrap();
        assert!(head_limits(&ok, &HeaderMap::new()).is_none());
        // ヘッダ 99 個は通り、100 個は 431
        let mut headers = HeaderMap::new();
        for i in 0..99 {
            let name = header::HeaderName::from_bytes(format!("x-{i}").as_bytes()).unwrap();
            headers.insert(name, HeaderValue::from_static("1"));
        }
        assert!(head_limits(&ok, &headers).is_none());
        headers.insert("x-99", HeaderValue::from_static("1"));
        let response = head_limits(&ok, &headers).unwrap();
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
        assert_eq!(response.headers()[header::CONNECTION], "close");
        // 1 行（`名前: 値\r\n`）は 65536 バイトまで
        let line = |value: usize| {
            let mut h = HeaderMap::new();
            h.insert("x-a", HeaderValue::from_str(&"a".repeat(value)).unwrap());
            h
        };
        let room = PYTHON_MAX_LINE - "x-a: ".len() - 2;
        assert!(head_limits(&ok, &line(room)).is_none());
        assert!(head_limits(&ok, &line(room + 1)).is_some());
        // 要求行（`POST 対象 HTTP/1.1\r\n`）も 65536 バイトまで
        let room = PYTHON_MAX_LINE - "POST ".len() - " HTTP/1.1\r\n".len() - "/?".len();
        let target = |n: usize| format!("/?{}", "a".repeat(n)).parse::<Uri>().unwrap();
        assert!(head_limits(&target(room), &HeaderMap::new()).is_none());
        let response = head_limits(&target(room + 1), &HeaderMap::new()).unwrap();
        assert_eq!(response.status(), StatusCode::URI_TOO_LONG);
    }
}
