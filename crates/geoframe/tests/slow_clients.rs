//! 遅い・読まないクライアントが、静的配信の枠・接続を握り続けて他のルートまで止めないこと
//! （T1.3 レビュー指摘。docs/rust-migration/server.md §9.3）。合成データだけを使う。

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TempDir, build_empty_catalog, request};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::limits::{ConnLimits, LimitedListener};
use geoframe::server::{AppState, router};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::timeout;

const LOCALHOST: &[(&str, &str)] = &[("Host", "localhost")];

/// ループバックの送受信バッファ（合わせて数 MB）より十分大きい。穴あきファイルなので実際の容量は使わない。
const BIG: u64 = 128 * 1024 * 1024;

struct Fixture {
    _tmp: TempDir,
    _writer: rusqlite::Connection,
    data: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new("slow");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("photos")).unwrap();
        let (_, writer) = build_empty_catalog(&data, "");
        let file = std::fs::File::create(data.join("photos/big.bin")).unwrap();
        file.set_len(BIG).unwrap();
        Self {
            _tmp: tmp,
            _writer: writer,
            data,
        }
    }

    fn state(&self, addr: std::net::SocketAddr, slots: usize) -> AppState {
        let catalog = Catalog::open(&self.data.join("catalog.db")).unwrap();
        let config = Config::load(
            |_| None,
            &Overrides {
                listen: Some(addr),
                data_dir: Some(self.data.clone()),
            },
        )
        .unwrap();
        let mut state = common::test_state(Arc::new(config), catalog);
        state.static_slots = Arc::new(Semaphore::new(slots));
        state
    }

    /// 素の `axum::serve`（接続の上限なし）。同時配信の枠だけを確かめる。
    async fn serve(&self, slots: usize) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(self.state(addr, slots));
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    /// 接続の上限・停滞・無通信の打ち切り（`LimitedListener`）を通して起動する。
    async fn serve_limited(&self, slots: usize, limits: ConnLimits) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(self.state(addr, slots));
        let listener = LimitedListener::new(listener, limits);
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }
}

/// 大きなファイルを要求し、応答のヘッダが来たところで読むのをやめる（枠を握ったままにする）。
async fn hold(addr: std::net::SocketAddr) -> TcpStream {
    hold_with_head(addr).await.0
}

/// [`hold`] に加えて、ヘッダと同じ読み取りで受け取ってしまった本文のバイト数と、`Content-Length` を返す。
async fn hold_with_head(addr: std::net::SocketAddr) -> (TcpStream, u64, u64) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /photos/big.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 512];
    let head_end = loop {
        if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
            .expect("ヘッダが来ない")
            .unwrap();
        assert_ne!(n, 0, "ヘッダの前に閉じた");
        seen.extend_from_slice(&chunk[..n]);
    };
    assert!(seen.starts_with(b"HTTP/1.1 200"), "{:?}", &seen[..16]);
    let head = String::from_utf8_lossy(&seen[..head_end]).to_lowercase();
    let length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .expect("Content-Length")
        .trim()
        .parse()
        .unwrap();
    (stream, (seen.len() - head_end) as u64, length)
}

/// 時間内に答えが返ること（返らなければテスト失敗）。
async fn quick(addr: std::net::SocketAddr, method: &str, target: &str) -> common::Response {
    timeout(
        Duration::from_secs(3),
        request(addr, method, target, LOCALHOST),
    )
    .await
    .unwrap_or_else(|_| panic!("{method} {target} が時間内に返らない"))
}

/// `HEAD` が 200 になるまで待つ（枠が戻るまで）。
async fn wait_until_served(addr: std::net::SocketAddr, within: Duration) {
    let started = Instant::now();
    loop {
        if quick(addr, "HEAD", "/photos/big.bin").await.status == 200 {
            return;
        }
        assert!(started.elapsed() < within, "枠が戻らない");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn slow_readers_cannot_exhaust_the_static_slots_or_block_other_routes() {
    let fixture = Fixture::new();
    let slots = 8;
    let addr = fixture.serve(slots).await;

    let mut holders = Vec::new();
    for _ in 0..slots {
        holders.push(hold(addr).await);
    }

    // 枠が埋まったら、待たずに定型の 503（ファイルを開かない）。HEAD も同じ
    for method in ["GET", "HEAD"] {
        let r = quick(addr, method, "/photos/big.bin").await;
        assert_eq!(r.status, 503, "{method}");
        assert_eq!(r.header("retry-after"), Some("5"));
        if method == "GET" {
            assert!(r.text().contains("Service Unavailable"), "{}", r.text());
            assert!(!r.text().contains("big.bin") && !r.text().contains("photos"));
        }
    }
    // 枠を使わない応答（ヘルス・写真API・埋め込み・無いファイル）は止まらない
    assert_eq!(quick(addr, "GET", "/healthz").await.status, 200);
    assert_eq!(quick(addr, "GET", "/api/v1/photos").await.status, 200);
    assert_eq!(quick(addr, "GET", "/static/admin.css").await.status, 200);
    assert_eq!(quick(addr, "GET", "/").await.status, 200);
    assert_eq!(quick(addr, "GET", "/photos/none.jpg").await.status, 404);

    // 握っていた接続を閉じれば枠が戻る
    drop(holders);
    wait_until_served(addr, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_stalled_download_is_cut_and_gives_its_slot_back() {
    let fixture = Fixture::new();
    let limits = ConnLimits {
        max_connections: 64,
        stall_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_secs(30),
    };
    let slots = 2;
    let addr = fixture.serve_limited(slots, limits).await;

    let mut holders = Vec::new();
    for _ in 0..slots {
        holders.push(hold(addr).await);
    }
    assert_eq!(quick(addr, "GET", "/photos/big.bin").await.status, 503);
    assert_eq!(quick(addr, "GET", "/healthz").await.status, 200);

    // 読まないまま停滞の上限を超えると、サーバーが切って枠が戻る
    wait_until_served(addr, Duration::from_secs(10)).await;
    // 枠が1つ戻った時点では、もう1本はまだ切られていないことがある。その状態で読み始めると受信バッファが
    // 空いて送信が進み、停滞でなくなって最後まで送られてしまう。どちらも切られるまで（停滞の上限の倍）待つ
    tokio::time::sleep(limits.stall_timeout * 2).await;

    // 切られた側は、バッファに残っていた分を読み終えたところで終わる（128MiB を最後まで送られない）
    for mut stream in holders {
        let mut total = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        let ended = timeout(Duration::from_secs(10), async {
            loop {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => total += n as u64,
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "切られていない");
        assert!(total < BIG, "最後まで送られた: {total}");
    }
}

/// 少しずつでも読み続ける相手は、全体で停滞の上限より長くかかっても切られない。
///
/// これは TCP を通した統合の確認で、期限の細かい動作は見ない（読み取りの間隔と TCP のバッファ・
/// ウィンドウ更新・スケジューリングで、送信側の進捗が何 ms で起きるかは保証できないため）。
/// 停滞の上限（2 秒）を読み取りの間隔（50ms）の 40 倍にして、負荷で揺れても「進捗が上限内に起きる」ことを
/// 余裕で満たす。タイマーの厳密な動作（上限ちょうどで切れる・進捗で測り直す）は `limits.rs` の
/// 仮想時間の単体試験が見る。読み取りの回数では判定しない。
#[tokio::test]
async fn a_slow_but_steady_reader_is_not_cut() {
    let fixture = Fixture::new();
    let limits = ConnLimits {
        max_connections: 64,
        stall_timeout: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(2),
    };
    let addr = fixture.serve_limited(4, limits).await;

    let (mut stream, mut total, length) = hold_with_head(addr).await;
    assert_eq!(length, BIG);
    let mut chunk = vec![0u8; 256 * 1024];
    let started = Instant::now();
    // 停滞の上限を超える時間（1.5 倍）、読み取りの間に待ちを入れながら読み続ける
    while started.elapsed() < limits.stall_timeout * 3 / 2 {
        let n = timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .expect("読めない")
            .unwrap();
        assert_ne!(n, 0, "読んでいる途中で切られた");
        total += n as u64;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(total < length, "遅い読み取りの間に全量が届いた: {total}");

    // 切られていれば、受信バッファの分を読んだあとに EOF になって全量には届かない。
    // 残りを速く読み切り、最初に受け取った分も含めて Content-Length ぴったりになること
    let rest = timeout(Duration::from_secs(60), async {
        while total < length {
            let n = stream.read(&mut chunk).await.unwrap();
            assert_ne!(
                n, 0,
                "残りを読む途中で切られた（受信済み {total} / {length}）"
            );
            total += n as u64;
        }
    })
    .await;
    assert!(
        rest.is_ok(),
        "残りを読み切れない（受信済み {total} / {length}）"
    );
    assert_eq!(total, length);
}

#[tokio::test]
async fn idle_connections_are_cut_and_the_connection_cap_holds() {
    let fixture = Fixture::new();
    let limits = ConnLimits {
        max_connections: 2,
        stall_timeout: Duration::from_secs(30),
        idle_timeout: Duration::from_millis(600),
    };
    let addr = fixture.serve_limited(4, limits).await;

    // 何も送らない接続で上限（2）を埋める
    let mut idle = Vec::new();
    for _ in 0..2 {
        idle.push(TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 枠が空くまで accept されない（応答が返らない）
    let early = timeout(
        Duration::from_millis(250),
        request(addr, "GET", "/healthz", LOCALHOST),
    )
    .await;
    assert!(early.is_err(), "上限を超えて受け付けた");

    // 無通信の上限で切られて枠が空き、受け付けられる
    let r = timeout(
        Duration::from_secs(5),
        request(addr, "GET", "/healthz", LOCALHOST),
    )
    .await
    .expect("枠が空かない");
    assert_eq!(r.status, 200);
    for mut stream in idle {
        let mut buf = [0u8; 1];
        let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .expect("切られていない");
        assert!(matches!(n, Ok(0) | Err(_)));
    }
}
