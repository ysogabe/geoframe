//! 結合テスト用の地図タイルの上流（OSM・地理院）のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。
//! 外へ出ない）。
//!
//! E11（`GET /tiles/…`）はキャッシュに無いタイルを本物の OSM・地理院へ取りに行く。結合テストが外へ出ないよう、
//! `common::test_state` などの既定の状態は、何も受け付けないスタブ（[`TileStub::deny_endpoints`]）に向ける。
//! 応答はすべて合成の PNG。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use geoframe::tiles::TileEndpoints;

/// 合成の PNG（先頭の 8 バイトだけが本物。現行と同じく、上流の応答はマジックだけを検査する）。
pub const SAMPLE_PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic tile";

pub struct TileStub {
    addr: SocketAddr,
    /// `Some(status)` ならその状態コードを空の本文で返す。`None` は 200 と [`SAMPLE_PNG`]。
    status: Arc<Mutex<Option<u16>>>,
    seen: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TileStub {
    pub fn start() -> Self {
        Self::spawn(false)
    }

    fn spawn(strict: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let status = Arc::new(Mutex::new(None));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = {
            let (status, seen, stopping) = (
                Arc::clone(&status),
                Arc::clone(&seen),
                Arc::clone(&stopping),
            );
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        if strict {
                            eprintln!(
                                "tile stub: 想定外の地図タイルの通信が届いた。結合テストの既定の接続先は何も受け付けない。\
                                 通信が要る試験は TileStub::start() を作って with_tiles で向ける"
                            );
                            std::process::abort();
                        }
                        let (status, seen) = (Arc::clone(&status), Arc::clone(&seen));
                        std::thread::spawn(move || {
                            let _ = handle(stream, &status, &seen);
                        });
                    }
                }
            })
        };
        Self {
            addr,
            status,
            seen,
            stopping,
            thread: Some(thread),
        }
    }

    /// 何も受け付けない接続先（試験の既定）。要求が 1 つでも届いたら**プロセスごと落とす**。
    pub fn deny_endpoints() -> TileEndpoints {
        static DENY: OnceLock<(TileStub, TileEndpoints)> = OnceLock::new();
        let (_, endpoints) = DENY.get_or_init(|| {
            let stub = TileStub::spawn(true);
            let endpoints = stub.endpoints();
            (stub, endpoints)
        });
        endpoints.clone()
    }

    pub fn endpoints(&self) -> TileEndpoints {
        TileEndpoints::loopback(self.addr).expect("ループバック")
    }

    /// 以後の要求に返す状態コード（`None` で 200 + PNG）。
    pub fn set_status(&self, status: Option<u16>) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// 受けた要求のパス（`/osm/3/1/2.png` の形）。
    pub fn seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for TileStub {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(
    mut stream: TcpStream,
    status: &Mutex<Option<u16>>,
    seen: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(path);
    let code = *status.lock().unwrap_or_else(PoisonError::into_inner);
    let (code, body) = match code {
        Some(code) => (code, &b""[..]),
        None => (200, SAMPLE_PNG),
    };
    write!(
        stream,
        "HTTP/1.1 {code} X\r\nContent-Type: image/png\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}
