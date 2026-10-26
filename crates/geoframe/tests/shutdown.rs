//! 停止（SIGTERM）の順序と共通期限（T4.1c）。合成データだけを使い、SIGTERM は `serve_until` の合図の差し替えで
//! 再現する（試験プロセスへ本物のシグナルは送らない）。
//!
//! - 合図を受けた**直後に**共有の `StopFlag` が立つ（HTTP の終了待ちより前。同期・AI のスレッドが止まり始める）
//! - 終わらない HTTP（読まないクライアントの大きな配信）は、共通期限の 3/4 で打ち切られる
//! - bind に失敗したときはすぐエラーを返す（合図は立てない。呼び出し側が続けてスレッドを止める）

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TempDir, build_empty_catalog};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::server::serve_until;
use geoframe::stop::StopFlag;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const BIG: u64 = 128 * 1024 * 1024;

fn free_addr() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn state_at(tmp: &TempDir, addr: std::net::SocketAddr) -> geoframe::server::AppState {
    let data = tmp.path().join("data");
    std::fs::create_dir_all(data.join("photos")).unwrap();
    let (_, writer) = build_empty_catalog(&data, "");
    drop(writer);
    let file = std::fs::File::create(data.join("photos/big.bin")).unwrap();
    file.set_len(BIG).unwrap();
    let catalog = Catalog::open(&data.join("catalog.db")).unwrap();
    let config = Config::load(
        |_| None,
        &Overrides {
            listen: Some(addr),
            data_dir: Some(data),
        },
    )
    .unwrap();
    common::test_state(Arc::new(config), catalog)
}

/// 大きなファイルを要求し、ヘッダが来たところで読むのをやめる（配信を握ったままにする）。
async fn hold(addr: std::net::SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /photos/big.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 512];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        seen.extend_from_slice(&chunk[..n]);
    }
    stream
}

async fn connect_when_up(addr: std::net::SocketAddr) -> TcpStream {
    for _ in 0..200 {
        if let Ok(stream) = TcpStream::connect(addr).await {
            drop(stream);
            return hold(addr).await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("サーバーが起動しない");
}

#[tokio::test]
async fn the_signal_sets_the_shared_stop_at_once_and_a_stuck_http_drain_is_cut_at_the_limit() {
    let tmp = TempDir::new("shutdown-http");
    let addr = free_addr();
    let state = state_at(&tmp, addr);
    let stop = StopFlag::with_budget(Duration::from_millis(1_200));
    let (signal, wait) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve_until(state, stop.clone(), async move {
        let _ = wait.await;
    }));
    let held = connect_when_up(addr).await;
    assert!(!stop.is_stopped());

    signal.send(()).unwrap();
    let began = Instant::now();
    // HTTP の終了待ち（握られた配信）が続いていても、停止の合図はすぐ立つ
    while !stop.is_stopped() {
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "合図が立たない"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!server.is_finished(), "配信中の接続を待っている");
    // 共通期限（1.2 秒）の 3/4 = 0.9 秒で打ち切られる
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("HTTP の終了待ちが期限で打ち切られない")
        .unwrap();
    assert!(result.is_ok());
    let waited = began.elapsed();
    assert!(waited >= Duration::from_millis(600), "{waited:?}");
    assert!(waited < Duration::from_millis(2_500), "{waited:?}");
    // 起点は合図の時刻で、HTTP の終了を待ってから数え始めていない
    let deadline = stop.deadline().unwrap();
    assert!(deadline <= began + Duration::from_millis(1_200 + 100));
    drop(held);
}

#[tokio::test]
async fn an_idle_server_stops_immediately_after_the_signal() {
    let tmp = TempDir::new("shutdown-idle");
    let addr = free_addr();
    let state = state_at(&tmp, addr);
    let stop = StopFlag::with_budget(Duration::from_secs(30));
    let (signal, wait) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve_until(state, stop.clone(), async move {
        let _ = wait.await;
    }));
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    signal.send(()).unwrap();
    let began = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("停止しない")
        .unwrap();
    assert!(result.is_ok());
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(stop.is_stopped());
}

#[tokio::test]
async fn a_bind_failure_returns_the_error_without_flagging_the_stop() {
    let tmp = TempDir::new("shutdown-bind");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let state = state_at(&tmp, taken.local_addr().unwrap());
    let stop = StopFlag::new();
    let result = serve_until(state, stop.clone(), std::future::pending::<()>()).await;
    assert!(result.is_err());
    // 呼び出し側（cli）が続けて stop() を呼んでスレッドを止める
    assert!(!stop.is_stopped());
    drop(taken);
}
