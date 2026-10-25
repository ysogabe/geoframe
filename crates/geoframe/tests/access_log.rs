//! リクエストログに、パス・クエリ・接続元などが出ないこと（AC7）。
//!
//! 単独のテストファイルにしてある。購読者の切り替え（`set_default` はスレッドごと、
//! `set_global_default` は1プロセス1回）が、並行して走る他のテストと干渉しないよう、プロセスを
//! 分けた。

mod common;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geoframe::config::{Config, LogLevel, Overrides};
use geoframe::db::{Catalog, REQUIRED_SCHEMA};
use geoframe::server::router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 書き込み先を共有する `MakeWriter`。
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

async fn get(addr: std::net::SocketAddr, target: &str) -> String {
    get_with_host(addr, target, "localhost").await
}

async fn get_with_host(addr: std::net::SocketAddr, target: &str, host: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    raw
}

#[tokio::test]
async fn access_log_has_only_method_route_status_and_time() {
    let captured = Captured::default();
    let subscriber = geoframe::logging::subscriber(LogLevel::Trace, captured.clone());
    // current_thread のテストなので、サーバーのタスクも同じスレッドで動き、このガードが効く
    let _guard = tracing::subscriber::set_default(subscriber);

    // 合成の DB（必要な列だけ）
    let dir = std::env::temp_dir().join(format!("geoframe-test-accesslog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("catalog.db");
    let writer = rusqlite::Connection::open(&path).unwrap();
    for (table, columns) in REQUIRED_SCHEMA {
        writer
            .execute_batch(&format!("CREATE TABLE {table} ({})", columns.join(", ")))
            .unwrap();
    }

    let config = Config::load(
        |_| None,
        &Overrides {
            data_dir: Some(dir.clone()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let state = common::test_state(Arc::new(config), Catalog::open(&path).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    get(addr, "/healthz?lat=12.345678&lon=98.765432").await;
    get(addr, "/derivatives/IMG_secret-name.jpg?q=1AbCdEfDriveId").await;
    // 引数付きのルート（静的ファイル）でも、ログに出るのは実際のパスではなくテンプレート。
    // derivatives のファイル名には Drive ID が入る（T1.1 レビューからの申し送り）
    get(addr, "/photos/IMG_other-secret.jpg").await;
    get(addr, "/blog_exports/trip-secret/index.html").await;
    get(addr, "/static/js/secret-script.js").await;
    get(addr, "/vendor/secret-vendor.css").await;
    get(addr, "/derivatives/.dl_1AbCdEfDriveId").await; // ドット始まり（403）
    get(addr, "/derivatives/%2e%2e/secret-dir/x.jpg").await; // トラバーサル
    get(addr, "/admin?token=abcSecretToken").await; // ページ
    get(addr, "/nope-secret-path?q=x").await; // ホワイトリスト外
    // 地名検索。`q` は 1 文字なので通信しない（このテストは外へ出ない）。ほかの名前の値はログに出ない
    get(addr, "/api/admin/geocode?secret-query=1&q=x").await;
    // 更新ルートへの GET は現行でも静的のホワイトリストに当たらず 403（ルートのテンプレートは付かない）
    get(addr, "/api/admin/secret-route").await;
    // 管理画面の読み取り API。クエリの値（検索語・不正な値）はログに出ない
    get(addr, "/api/admin/photos?filter=secret-filter&page=2").await;
    get(addr, "/api/admin/photos?page=secret-page").await; // 400
    get(addr, "/api/admin/status?x=secret-status").await;
    // 旅行ブログの読み取り API。trip_id・未知のパスの名前はログに出ない（旅程の中身も読まない）
    get(addr, "/api/blog/trips?secret-blog-query=1").await;
    get(addr, "/api/blog/trip?trip_id=secret-trip-id").await; // 404
    get(addr, "/api/blog/trip?trip_id=Secret_Bad").await; // 400
    get(addr, "/api/blog/draft?trip_id=secret-draft-id").await; // 404
    get(addr, "/api/blog/secret-blog-route").await; // 404
    // Host が許可されない要求。拒否の理由は出るが、Host の値は出ない
    get_with_host(addr, "/derivatives/a.jpg", "attacker.invalid:8080").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(log.contains("GET /healthz 200"), "{log}");
    for expected in [
        "GET /derivatives/{*path} 404",
        "GET /derivatives/{*path} 403",
        "GET /photos/{*path} 404",
        "GET /blog_exports/{*path} 404",
        "GET /static/{*path} 404",
        "GET /vendor/{*path} 404",
        "GET /admin 200",
        "GET (unmatched) 403",
        "GET /api/admin/geocode 200",
        "GET /api/admin/photos 200",
        "GET /api/admin/photos 400",
        "GET /api/admin/status 200",
        "GET /api/blog/trips 200",
        "GET /api/blog/trip 404",
        "GET /api/blog/trip 400",
        "GET /api/blog/draft 404",
        "GET /api/blog/{*path} 404",
    ] {
        assert!(log.contains(expected), "{expected} が無い:\n{log}");
    }
    assert!(log.contains("Host ヘッダが許可されていない"), "{log}");
    // 依存クレート（axum の「connection … accepted」など）のログは出ない
    assert!(!log.contains("accepted"), "{log}");
    for secret in [
        "12.345678",
        "98.765432",
        "secret-name",
        "DriveId",
        "127.0.0.1",
        "localhost",
        "other-secret",
        "trip-secret",
        "secret-script",
        "secret-vendor",
        "secret-dir",
        "abcSecretToken",
        "nope-secret",
        "secret-query",
        "secret-route",
        "secret-filter",
        "secret-page",
        "secret-status",
        "secret-blog",
        "secret-trip-id",
        "Secret_Bad",
        "secret-draft-id",
        "attacker",
        "a.jpg",
        "x.jpg",
        ".dl_",
    ] {
        assert!(!log.contains(secret), "ログに {secret} が出ている:\n{log}");
    }
}

/// 本文が UTF-8 でない応答（PNG）も読む `get`。
async fn get_bytes(addr: std::net::SocketAddr, target: &str) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
}

/// 地図タイル: ログに出るのはメソッド・ルートのテンプレート・状態コード・時間だけ。タイルの座標（z/x/y）・層は出ない。
/// 上流はループバックのスタブ。
#[tokio::test]
async fn tile_requests_log_only_the_route_template_and_never_the_coordinates() {
    let captured = Captured::default();
    let subscriber = geoframe::logging::subscriber(LogLevel::Trace, captured.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = std::env::temp_dir().join(format!(
        "geoframe-test-accesslog-tiles-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("catalog.db");
    let writer = rusqlite::Connection::open(&path).unwrap();
    for (table, columns) in REQUIRED_SCHEMA {
        writer
            .execute_batch(&format!("CREATE TABLE {table} ({})", columns.join(", ")))
            .unwrap();
    }
    let config = Config::load(
        |_| None,
        &Overrides {
            data_dir: Some(dir.clone()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let stub = common::TileStub::start();
    let state = common::with_tile_endpoints(
        common::test_state(Arc::new(config), Catalog::open(&path).unwrap()),
        stub.endpoints(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    get_bytes(addr, "/tiles/osm/13/4321/2345.png").await; // 取得（miss）
    get_bytes(addr, "/tiles/osm/13/4321/2345.png").await; // hit
    get_bytes(addr, "/tiles/gsi/7/66/33.png?secret-query=1").await;
    get_bytes(addr, "/tiles/osm/13/99999/2345.png").await; // 404（範囲外）
    stub.set_status(Some(503));
    get_bytes(addr, "/tiles/gsi/9/257/258.png").await; // 上流の失敗
    tokio::time::sleep(Duration::from_millis(100)).await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(log.matches("GET /tiles/{*path} 200").count() >= 4, "{log}");
    assert!(log.contains("GET /tiles/{*path} 404"), "{log}");
    for secret in [
        "4321",
        "2345",
        "99999",
        "257",
        "258",
        "secret-query",
        "osm/",
        "gsi/",
        ".png",
        "127.0.0.1",
    ] {
        assert!(!log.contains(secret), "ログに {secret} が出ている:\n{log}");
    }
}

async fn post(addr: std::net::SocketAddr, target: &str, extra: &str, body: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 {extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    raw
}

/// 更新 API（POST）のログ: 出るのはメソッド・ルート・状態コード・時間と、内部エラーの種類だけ。
/// 座標・id・Origin の値・本文の中身は出ない。500 の応答にも例外の文字列は載らない。
#[tokio::test]
async fn post_logs_only_method_route_status_and_the_kind_of_internal_error() {
    let captured = Captured::default();
    let subscriber = geoframe::logging::subscriber(LogLevel::Trace, captured.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = std::env::temp_dir().join(format!(
        "geoframe-test-accesslog-post-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("catalog.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    for (table, columns) in REQUIRED_SCHEMA {
        conn.execute_batch(&format!("CREATE TABLE {table} ({})", columns.join(", ")))
            .unwrap();
    }
    conn.execute_batch("INSERT INTO photos(id, name) VALUES(4, 'secret-name.jpg')")
        .unwrap();
    let writer =
        geoframe::writer::Writer::open(&path, geoframe::writer::OpenMode::MustExist).unwrap();
    let catalog = Catalog::open(&path).unwrap().with_writer(writer);
    let config = Config::load(
        |_| None,
        &Overrides {
            data_dir: Some(dir.clone()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let state = common::test_state(Arc::new(config), catalog);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    let setloc = r#"{"id": 4, "lat": 12.345678, "lon": 98.765432}"#;
    // 成功（200）
    let ok = post(addr, "/api/admin/setloc?q=secret-query", "", setloc).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    // CSRF 対策で断る（403）・ルートが無い（403）
    let origin = post(
        addr,
        "/api/admin/setloc",
        "Origin: http://attacker.invalid\r\n",
        setloc,
    )
    .await;
    assert!(origin.starts_with("HTTP/1.1 403"), "{origin}");
    let none = post(addr, "/api/admin/secret-route", "", setloc).await;
    assert!(none.starts_with("HTTP/1.1 403"), "{none}");
    // 入力の違反（400）
    let bad = post(
        addr,
        "/api/admin/setloc",
        "",
        r#"{"id": 4, "lat": 99.5, "lon": 1}"#,
    )
    .await;
    assert!(bad.starts_with("HTTP/1.1 400"), "{bad}");
    // 書き込みが失敗する（表が消えた）→ 500。応答は定型文だけ
    conn.execute_batch("DROP TABLE photos").unwrap();
    let broken = post(
        addr,
        "/api/admin/setflag",
        "",
        r#"{"id": 4, "favorite": true}"#,
    )
    .await;
    assert!(broken.starts_with("HTTP/1.1 500"), "{broken}");
    assert!(broken.contains("内部エラーが発生しました"), "{broken}");
    assert!(!broken.contains("no such table"), "{broken}");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);

    for expected in [
        "POST /api/admin/setloc 200",
        "POST /api/admin/setloc 403",
        "POST (unmatched) 403",
        "POST /api/admin/setloc 400",
        "POST /api/admin/setflag 500",
        "[api] /api/admin/setflag 内部エラー",
    ] {
        assert!(log.contains(expected), "{expected} が無い:\n{log}");
    }
    for secret in [
        "12.345678",
        "98.765432",
        "secret-name",
        "secret-query",
        "secret-route",
        "attacker",
        "99.5",
        "127.0.0.1",
        "localhost",
    ] {
        assert!(!log.contains(secret), "ログに {secret} が出ている:\n{log}");
    }
}
