//! HTTP サーバー。ルートは `GET /healthz`（プロセスが動いていて DB が読める）と
//! `GET /api/v1/photos`（写真フィード。契約は docs/architecture/photo-feed-contract.md）。
//!
//! 公開するルートが増えても、次の方針は変えない。
//!
//! - ハンドラは [`AppState`] の [`Catalog`] 経由でだけ DB を読む（tokio の worker で SQLite を
//!   直接実行しない）。
//! - 応答の本文にエラーの文字列・パス・件数などの環境の情報を載せない（理由はログにだけ出す）。
//! - リクエストログは [`access_log`] が一手に引き受け、出す項目は
//!   メソッド・ルートのテンプレート・状態コード・所要時間だけ（[`crate::logging`]）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use serde::Serialize;

use crate::config::Config;
use crate::db::Catalog;
use crate::photos;

/// `/healthz` が DB の確認を待つ上限。超えたら不健康（503）として答える。`/healthz` 専用の接続の
/// `busy_timeout`（2 秒、`db.rs`）より長く、API の読み取りとは別の接続を使うので、API が混んで
/// いても DB が正常なら間に合う。
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// ハンドラが共有する状態。複製は安い。
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub catalog: Catalog,
}

#[derive(Serialize)]
struct Health {
    ok: bool,
}

/// プロセスが動いていて、DB が読める（`photos` を1行読める）なら 200、そうでなければ 503。
/// 本文は `{"ok":true|false}` だけで、理由・件数・パスは載せない（理由はログに出す）。
async fn healthz(State(state): State<AppState>) -> Response {
    let ok = match tokio::time::timeout(HEALTH_TIMEOUT, state.catalog.check_readable()).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!("ヘルス確認: {e}");
            false
        }
        Err(_) => {
            tracing::warn!(
                "ヘルス確認: DB の応答が {} 秒以内に返りません",
                HEALTH_TIMEOUT.as_secs()
            );
            false
        }
    };
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Health { ok }),
    )
        .into_response()
}

/// `GET /api/v1/photos`。Python の `_send_json` と同じ `Content-Type` と `Cache-Control`
/// （`json.dumps(ensure_ascii=False)` と同じく、日本語はエスケープしない UTF-8 のまま）。
/// 読めない・順番待ちが `QUEUE_TIMEOUT` を超えたときは 503 と定型の本文で、理由はログにだけ出す。
async fn api_photos(State(state): State<AppState>) -> Response {
    match state.catalog.read(photos::list_photos).await {
        Ok(list) => match serde_json::to_vec(&list) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(e) => {
                tracing::error!("写真一覧を JSON にできません: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        },
        Err(e) => {
            tracing::warn!("写真一覧を読めません: {e}");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "写真一覧を読み取れません。しばらくしてから再試行してください",
            )
        }
    }
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

/// `{"ok":false,"error":"..."}`。本文は定型文だけで、環境の情報（SQLite のエラー・パス）を載せない。
fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "ok": false, "error": message });
    json_response(status, body.to_string().into_bytes())
}

/// リクエストログ。出す項目は**メソッド・ルートのテンプレート・状態コード・所要時間（ミリ秒）**
/// だけ。実際のパスやクエリ（ファイル名・Drive ID・検索語が入りうる）は出さず、どのルートにも
/// 当たらなかったリクエストは `(unmatched)` と出す。`/healthz` の成功は頻繁に呼ばれるので debug。
async fn access_log(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let ms = started.elapsed().as_millis();
    let route = route.as_deref().unwrap_or("(unmatched)");
    if status >= 500 {
        tracing::warn!("{method} {route} {status} {ms}ms");
    } else if route == "/healthz" {
        tracing::debug!("{method} {route} {status} {ms}ms");
    } else {
        tracing::info!("{method} {route} {status} {ms}ms");
    }
    response
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/photos", get(api_photos))
        // 既定のフォールバックには layer が掛からないので、明示して 404 もログに載せる
        .fallback(not_found)
        .layer(middleware::from_fn(access_log))
        .with_state(state)
}

/// SIGINT / SIGTERM（systemd の停止）を待つ。
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM を登録できません");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

pub async fn serve(state: AppState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(state.config.listen).await?;
    tracing::info!("{} で待ち受けます", listener.local_addr()?);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("停止しました");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::testutil::{TempDir, synthetic_catalog};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn state(catalog: Catalog) -> AppState {
        let config = Config::load(|_| None, &Overrides::default()).unwrap();
        AppState {
            config: Arc::new(config),
            catalog,
        }
    }

    /// 127.0.0.1 の空きポートで router を起動し、(状態行, ヘッダ, 本文) を返す GET を用意する。
    async fn spawn(state: AppState) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        addr
    }

    async fn get(addr: std::net::SocketAddr, target: &str) -> (String, String, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let (status, headers) = head.split_once("\r\n").unwrap();
        (status.to_string(), headers.to_lowercase(), body.to_string())
    }

    #[tokio::test]
    async fn healthz_is_ok_when_the_db_is_readable() {
        let dir = TempDir::new("health-ok");
        let (path, _writer) = synthetic_catalog(dir.path(), 3);
        let addr = spawn(state(Catalog::open(&path).unwrap())).await;

        let (status, headers, body) = get(addr, "/healthz").await;
        assert!(status.starts_with("HTTP/1.1 200"), "{status}");
        assert!(headers.contains("cache-control: no-store"), "{headers}");
        // 件数などは出さない
        assert_eq!(body, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn healthz_is_503_when_the_db_cannot_be_read() {
        let dir = TempDir::new("health-ng");
        let (path, writer) = synthetic_catalog(dir.path(), 3);
        let catalog = Catalog::open(&path).unwrap();
        let addr = spawn(state(catalog)).await;
        // 起動後に DB の中身が壊れた・入れ替わった状況
        writer.execute_batch("DROP TABLE photos").unwrap();

        let (status, _, body) = get(addr, "/healthz").await;
        assert!(status.starts_with("HTTP/1.1 503"), "{status}");
        assert_eq!(body, r#"{"ok":false}"#);
        assert!(!body.contains("photos"), "{body}");
    }

    #[tokio::test]
    async fn unknown_paths_are_404() {
        let dir = TempDir::new("health-404");
        let (path, _writer) = synthetic_catalog(dir.path(), 0);
        let addr = spawn(state(Catalog::open(&path).unwrap())).await;
        let (status, _, _) = get(addr, "/nope").await;
        assert!(status.starts_with("HTTP/1.1 404"), "{status}");
    }
}
