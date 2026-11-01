//! 静的ファイルの配信（`/static/`・`/vendor/` は埋め込み、`/derivatives/`・`/photos/`・
//! `/blog_exports/` はデータ置き場から）と、Python の `send_error` と同じ形のエラーページ。
//!
//! 何を配ってよいかの判定（ホワイトリスト・ドット始まり）は [`crate::webpath`] が済ませており、
//! ここは**実体の解決と応答**だけを受け持つ。データ置き場の実体は、リンクを辿って解決したうえで
//! その領域のディレクトリの内側にあることを確かめてから開く（外を指すリンクは 403）。
//! 仕様と Python との差は docs/rust-migration/server.md §9。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use tokio::io::AsyncReadExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::embedded;
use crate::webpath::{Area, Target};

/// ファイルを流すときの1回の読み取りの大きさ。
const CHUNK: usize = 64 * 1024;

/// 拡張子から `Content-Type` を決める。Python の `SimpleHTTPRequestHandler`（`mimetypes`）と同じく
/// テキストにも `charset` は付けない。表は固定で、機体の `/etc/mime.types` に左右されない。
/// 表に無いものは `application/octet-stream`。
pub fn content_type(name: &str) -> &'static str {
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => return "application/octet-stream",
    };
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "geojson" => "application/geo+json",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "zip" => "application/zip",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Python の `BaseHTTPRequestHandler.send_error` と同じ HTML のエラーページ（本文の形は同じで、
/// ステータス行の理由句だけ hyper が決める）。`Content-Type` は `text/html;charset=utf-8`。
/// `message` と `explain` の末尾に `.` が付くのも Python のテンプレートどおり。
pub fn error_page(status: StatusCode, message: &str, explain: &str) -> Response {
    let code = status.as_u16();
    let body = format!(
        "<!DOCTYPE HTML>\n\
         <html lang=\"en\">\n\
         \x20   <head>\n\
         \x20       <meta charset=\"utf-8\">\n\
         \x20       <title>Error response</title>\n\
         \x20   </head>\n\
         \x20   <body>\n\
         \x20       <h1>Error response</h1>\n\
         \x20       <p>Error code: {code}</p>\n\
         \x20       <p>Message: {}.</p>\n\
         \x20       <p>Error code explanation: {code} - {}.</p>\n\
         \x20   </body>\n\
         </html>\n",
        html_escape(message),
        html_escape(explain),
    );
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html;charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    response
}

/// 400（Python の `send_error(400)`）。`Host` ヘッダが複数ある要求に返す。
pub fn bad_request() -> Response {
    error_page(
        StatusCode::BAD_REQUEST,
        "Bad Request",
        "Bad request syntax or unsupported method",
    )
}

/// 403（Python の `send_error(403)`）。
pub fn forbidden() -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        "Forbidden",
        "Request forbidden -- authorization will not help",
    )
}

/// 404（Python の静的配信の `send_error(404, "File not found")`）。
pub fn not_found() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        "File not found",
        "Nothing matches the given URI",
    )
}

/// 501（Python が未実装のメソッドに返すもの。`OPTIONS` もここ＝プリフライトは必ず失敗し、
/// CORS ヘッダは一切付かない。CSRF 対策の一部）。
pub fn unsupported_method(method: &Method) -> Response {
    error_page(
        StatusCode::NOT_IMPLEMENTED,
        &format!("Unsupported method ('{method}')"),
        "Server does not support this operation",
    )
}

/// Host 検査で拒否したときの 403（本文は現行と同じ文面）。
pub fn forbidden_host() -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        "Forbidden host",
        "許可されていないHostヘッダです。LAN内の他端末からホスト名で開く場合は、\
         環境変数 GEOFRAME_ALLOWED_HOSTS に追加してください。",
    )
}

/// 同時に配れるファイルの数（[`crate::limits::STATIC_FILE_SLOTS`]）に空きが無いときの 503
/// （Python の `http.server` の 503 の文面）。本文は定型で、環境の情報は載せない。
pub fn busy() -> Response {
    let mut response = error_page(
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable",
        "The server cannot process the request due to a high load",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

pub(crate) fn not_modified() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    response
}

/// 200 の応答の共通部。HEAD は本文なし（`Content-Length` は GET と同じ）。
pub(crate) fn file_response(
    method: &Method,
    content_type: &str,
    len: u64,
    body: Body,
    modified: Option<SystemTime>,
    etag: Option<&str>,
) -> Response {
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        body
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("Content-Type は固定の表の値"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Some(modified) = modified {
        let date = httpdate::fmt_http_date(modified);
        headers.insert(
            header::LAST_MODIFIED,
            HeaderValue::from_str(&date).expect("HTTP 日付は ASCII"),
        );
    }
    if let Some(etag) = etag {
        headers.insert(
            header::ETAG,
            HeaderValue::from_str(etag).expect("ETag は ASCII"),
        );
    }
    response
}

fn epoch_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `If-Modified-Since` による 304 の判定（データ置き場のファイル用）。Python と同じ規則:
/// `If-None-Match` があれば無視（こちらは ETag を出さないので、付いていれば新規取得）、
/// 読めない日付も無視、秒未満は切り捨てて「更新が日付以前なら 304」。
fn not_modified_since(headers: &HeaderMap, modified: SystemTime) -> bool {
    if headers.contains_key(header::IF_NONE_MATCH) {
        return false;
    }
    let Some(since) = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| httpdate::parse_http_date(v).ok())
    else {
        return false;
    };
    epoch_secs(modified) <= epoch_secs(since)
}

/// `If-None-Match` が ETag に当たるか（埋め込みファイル用）。`*`、カンマ区切り、弱い比較（`W/`）に対応。
pub(crate) fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}

/// ファイルを `CHUNK` ずつ流す本文。読み取りに失敗したらそこで打ち切る（接続が閉じる）。
/// 同時配信の枠（`permit`）は本文が最後まで流れるか捨てられる（切断・停滞の打ち切り・`HEAD`）まで持つ。
pub(crate) fn file_body(file: tokio::fs::File, permit: OwnedSemaphorePermit) -> Body {
    let stream = futures_util::stream::unfold(Some((file, permit)), |state| async move {
        let (mut file, permit) = state?;
        let mut buf = vec![0u8; CHUNK];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((
                    Ok::<Bytes, std::io::Error>(Bytes::from(buf)),
                    Some((file, permit)),
                ))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    Body::from_stream(stream)
}

/// 埋め込みの `/static/`・`/vendor/` のファイルを返す。
/// `Last-Modified` は出さず、内容のハッシュの ETag で再検証させる（`no-cache` と組み合わせる。
/// ビルド時刻・チェックアウト時刻に依存する日付で古い JS が 304 になる事故を避ける）。
pub fn serve_embedded(
    target: &Target<'_>,
    trailing_slash: bool,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let dir = match target.area {
        Area::Static => "static",
        Area::Vendor => "vendor",
        _ => return not_found(),
    };
    let key = format!("{dir}/{}", target.rel.join("/"));
    serve_embedded_key(&key, trailing_slash, method, headers)
}

/// `static/admin.html` のようなキーで埋め込みのファイルを返す（ページの読み替えにも使う）。
pub fn serve_embedded_key(
    key: &str,
    trailing_slash: bool,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let Some(file) = embedded::get(key) else {
        // ディレクトリそのものは一覧を出さない（403）。無ければ404
        return if embedded::is_dir(key) {
            forbidden()
        } else {
            not_found()
        };
    };
    if trailing_slash {
        return not_found(); // ファイルに `/` を付けた要求（Python と同じ）
    }
    if etag_matches(headers, file.etag) {
        let mut response = not_modified();
        response
            .headers_mut()
            .insert(header::ETAG, HeaderValue::from_static(file.etag));
        return response;
    }
    file_response(
        method,
        content_type(key),
        file.data.len() as u64,
        Body::from(file.data),
        None,
        Some(file.etag),
    )
}

/// 領域の中のファイルを解決できなかった理由（[`resolve_area_file`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denied {
    /// 無い・解決できない・通常のファイルでない（404）
    NotFound,
    /// 領域の外を指すリンク、またはディレクトリ（403）
    Forbidden,
}

impl Denied {
    pub(crate) fn into_response(self) -> Response {
        match self {
            Self::NotFound => not_found(),
            Self::Forbidden => forbidden(),
        }
    }
}

/// データ置き場の領域（`derivatives/`・`photos/`・`blog_exports/`・`thumbs/`）の中のファイルを、リンクを辿って
/// 解決する。**ファイルを開く前の検査**で、`serve_data` と `/thumb/` が共有する（読むだけで、ディレクトリは作らない）。
///
/// 1. 領域のディレクトリ（`<data_root>/<名前>`）とファイルの両方を**リンクを辿って解決**する。
///    `data_root` は起動時に解決済み。領域のディレクトリ自体がリンクでもよい（運用者の設定。
///    別のディスクへ置く場合）が、**中のリンク**は解決後も領域のディレクトリの内側でなければ
///    [`Denied::Forbidden`]（`derivatives/x.jpg -> ../catalog.db` のような、データ置き場の中でも領域の外を指すものを含む）。
/// 2. ディレクトリは `Forbidden`（一覧は出さない）、通常のファイル以外・末尾 `/` 付きは `NotFound`。
///
/// 戻り値は解決後の絶対パス。
pub(crate) async fn resolve_area_file(
    data_root: &Path,
    area: Area,
    rel: &[&str],
    trailing_slash: bool,
) -> Result<PathBuf, Denied> {
    let Some(dir_name) = area.data_dir_name() else {
        return Err(Denied::NotFound);
    };
    let area_dir = data_root.join(dir_name);
    let mut path: PathBuf = area_dir.clone();
    path.extend(rel);

    // 解決できない（無い・循環・権限）は Python と同じく 404
    let Ok(area_real) = tokio::fs::canonicalize(&area_dir).await else {
        return Err(Denied::NotFound);
    };
    let Ok(real) = tokio::fs::canonicalize(&path).await else {
        return Err(Denied::NotFound);
    };
    if !real.starts_with(&area_real) {
        tracing::warn!(
            "{} の領域の外を指すリンクを拒否しました（ファイル名は記録しません）",
            area.route()
        );
        return Err(Denied::Forbidden);
    }
    let Ok(meta) = tokio::fs::metadata(&real).await else {
        return Err(Denied::NotFound);
    };
    if meta.is_dir() {
        return Err(Denied::Forbidden);
    }
    if !meta.is_file() || trailing_slash {
        return Err(Denied::NotFound);
    }
    Ok(real)
}

/// データ置き場の `derivatives/`・`photos/`・`blog_exports/` のファイルを返す。
///
/// 手順:
/// 1. 領域とファイルを解決する（[`resolve_area_file`]。外を指すリンクは 403、ディレクトリは 403、通常のファイル以外は 404）。
/// 2. 同時配信の枠（`slots`）を取る。空きが無ければ待たずに 503（ファイル記述子を握る数の上限）。
/// 3. 解決後のパスを開き、開いたハンドルのサイズと更新時刻で `Content-Length`・`Last-Modified`・304 を決める。
///    枠は応答の本文が捨てられるまで持つ（304 と `HEAD` はすぐ返す）。
pub async fn serve_data(
    data_root: &Path,
    slots: &Arc<Semaphore>,
    target: &Target<'_>,
    trailing_slash: bool,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let real = match resolve_area_file(data_root, target.area, &target.rel, trailing_slash).await {
        Ok(real) => real,
        Err(denied) => return denied.into_response(),
    };
    let Ok(permit) = Arc::clone(slots).try_acquire_owned() else {
        return busy();
    };
    let Ok(file) = tokio::fs::File::open(&real).await else {
        return not_found();
    };
    // 開いたハンドルの値を使う（stat と open のあいだの入れ替えで食い違わない）
    let Ok(meta) = file.metadata().await else {
        return not_found();
    };
    let modified = meta.modified().ok();
    if let Some(modified) = modified
        && not_modified_since(headers, modified)
    {
        return not_modified();
    }
    let name = target.rel.last().copied().unwrap_or("");
    file_response(
        method,
        content_type(name),
        meta.len(),
        file_body(file, permit),
        modified,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_are_fixed_and_without_charset() {
        for (name, expected) in [
            ("a.jpg", "image/jpeg"),
            ("a.JPEG", "image/jpeg"),
            ("a.png", "image/png"),
            ("a.heic", "image/heic"),
            ("a.webp", "image/webp"),
            ("admin.html", "text/html"),
            ("a.css", "text/css"),
            ("admin.js", "text/javascript"),
            ("a.json", "application/json"),
            ("a.woff2", "font/woff2"),
            ("README.md", "text/markdown"),
            ("favicon.ico", "image/vnd.microsoft.icon"),
            ("noext", "application/octet-stream"),
            (".hidden", "application/octet-stream"),
            ("a.unknown", "application/octet-stream"),
            ("a.tar.gz", "application/octet-stream"),
            ("trail.", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }

    #[test]
    fn error_pages_match_the_python_template() {
        let body = |r: Response| async move {
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let text = String::from_utf8(rt.block_on(body(forbidden())).to_vec()).unwrap();
        assert_eq!(
            text,
            "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        \
             <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        \
             <p>Error code: 403</p>\n        <p>Message: Forbidden.</p>\n        \
             <p>Error code explanation: 403 - Request forbidden -- authorization will not help.</p>\n    \
             </body>\n</html>\n"
        );
        // 501 のメッセージにはメソッド名が入る（エスケープされる）
        let method = Method::from_bytes(b"A<B").unwrap_or(Method::OPTIONS);
        let text =
            String::from_utf8(rt.block_on(body(unsupported_method(&method))).to_vec()).unwrap();
        assert!(text.contains("Unsupported method ('"), "{text}");
        assert!(!text.contains("<B"), "{text}");
    }

    #[test]
    fn etag_matching_handles_lists_weak_tags_and_star() {
        let mut headers = HeaderMap::new();
        assert!(!etag_matches(&headers, "\"a\""));
        headers.insert(header::IF_NONE_MATCH, "\"b\", W/\"a\"".parse().unwrap());
        assert!(etag_matches(&headers, "\"a\""));
        assert!(!etag_matches(&headers, "\"c\""));
        headers.insert(header::IF_NONE_MATCH, "*".parse().unwrap());
        assert!(etag_matches(&headers, "\"anything\""));
    }

    #[test]
    fn if_modified_since_follows_python() {
        let modified = UNIX_EPOCH
            + std::time::Duration::from_secs(1_750_000_000)
            + std::time::Duration::from_millis(900);
        let mut headers = HeaderMap::new();
        // ヘッダ無し・読めない日付は 200
        assert!(!not_modified_since(&headers, modified));
        headers.insert(header::IF_MODIFIED_SINCE, "garbage".parse().unwrap());
        assert!(!not_modified_since(&headers, modified));
        // 同じ秒（秒未満は切り捨て）と新しい日付は 304、古い日付は 200
        let at =
            |secs: u64| httpdate::fmt_http_date(UNIX_EPOCH + std::time::Duration::from_secs(secs));
        headers.insert(
            header::IF_MODIFIED_SINCE,
            at(1_750_000_000).parse().unwrap(),
        );
        assert!(not_modified_since(&headers, modified));
        headers.insert(
            header::IF_MODIFIED_SINCE,
            at(1_750_000_100).parse().unwrap(),
        );
        assert!(not_modified_since(&headers, modified));
        headers.insert(
            header::IF_MODIFIED_SINCE,
            at(1_749_999_999).parse().unwrap(),
        );
        assert!(!not_modified_since(&headers, modified));
        // If-None-Match があれば日付は見ない
        headers.insert(
            header::IF_MODIFIED_SINCE,
            at(1_750_000_100).parse().unwrap(),
        );
        headers.insert(header::IF_NONE_MATCH, "\"x\"".parse().unwrap());
        assert!(!not_modified_since(&headers, modified));
    }
}
