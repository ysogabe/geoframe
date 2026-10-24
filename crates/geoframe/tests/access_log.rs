//! リクエストログに、パス・クエリ・接続元などが出ないこと（AC7）。
//!
//! 単独のテストファイルにしてある。購読者の切り替え（`set_default` はスレッドごと、
//! `set_global_default` は1プロセス1回）が、並行して走る他のテストと干渉しないよう、プロセスを
//! 分けた。

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geoframe::config::{Config, LogLevel, Overrides};
use geoframe::db::{Catalog, REQUIRED_SCHEMA};
use geoframe::server::{AppState, router};
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

    let config = Config::load(|_| None, &Overrides::default()).unwrap();
    let state = AppState {
        config: Arc::new(config),
        catalog: Catalog::open(&path).unwrap(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    get(addr, "/healthz?lat=12.345678&lon=98.765432").await;
    get(addr, "/derivatives/IMG_secret-name.jpg?q=1AbCdEfDriveId").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(log.contains("GET /healthz 200"), "{log}");
    assert!(log.contains("GET (unmatched) 404"), "{log}");
    // 依存クレート（axum の「connection … accepted」など）のログは出ない
    assert!(!log.contains("accepted"), "{log}");
    for secret in [
        "12.345678",
        "98.765432",
        "secret-name",
        "DriveId",
        "127.0.0.1",
        "localhost",
    ] {
        assert!(!log.contains(secret), "ログに {secret} が出ている:\n{log}");
    }
}
