//! 結合テスト用の地理院・Nominatim のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）。
//!
//! E4（`GET /api/admin/geocode`）は、検索語が 2 文字以上だと本物の地理院・Nominatim へ通信する。結合テストが
//! 外へ出ないよう、`common::app_state` などの既定の状態はこのスタブ（[`GeocodeStub::deny`]）に向ける。
//! 応答はすべて合成の値。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use geoframe::geocode::{Endpoints, Geocoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upstream {
    Gsi,
    Osm,
}

#[derive(Default)]
struct State {
    gsi: Option<String>,
    osm: Option<String>,
    /// 受けた要求（`gsi` か `osm` と、`?` より後ろのクエリ文字列）。
    seen: Vec<(Upstream, String)>,
    /// 応答を決めていない上流への要求（`strict` でないときだけ記録する）。
    unexpected: Vec<String>,
}

pub struct GeocodeStub {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GeocodeStub {
    /// 上流ごとに `set` で決めた応答を返す。決めていない上流への要求は 500 を返して記録する
    /// （[`GeocodeStub::assert_no_unexpected`] で検査する）。
    pub fn start() -> Self {
        Self::spawn(false)
    }

    /// どんな要求も受け付けない。要求が 1 つでも届いたら**プロセスごと落とす**（試験の既定の接続先。
    /// 外へ出る代わりに、意図しない通信をした試験が必ず失敗する）。
    fn strict() -> Self {
        Self::spawn(true)
    }

    fn spawn(strict: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stopping) = (Arc::clone(&state), Arc::clone(&stopping));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        if strict {
                            eprintln!(
                                "geocode stub: 想定外の地名検索の通信が届いた。結合テストの既定の接続先は何も受け付けない。\
                                 通信が要る試験は GeocodeStub::start() を作って with_geocoder で向ける"
                            );
                            std::process::abort();
                        }
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || {
                            let _ = handle(stream, &state);
                        });
                    }
                }
            })
        };
        Self {
            addr,
            state,
            stopping,
            thread: Some(thread),
        }
    }

    /// この試験のプロセスで共有する、何も受け付けないスタブの `Geocoder`（既定）。
    pub fn deny() -> Arc<Geocoder> {
        static DENY: OnceLock<(GeocodeStub, Arc<Geocoder>)> = OnceLock::new();
        let (_, geocoder) = DENY.get_or_init(|| {
            let stub = GeocodeStub::strict();
            let geocoder = stub.geocoder();
            (stub, geocoder)
        });
        Arc::clone(geocoder)
    }

    /// このスタブへ向けた `Geocoder`（実時計）。
    pub fn geocoder(&self) -> Arc<Geocoder> {
        let endpoints = Endpoints::loopback(self.addr).expect("ループバック");
        Arc::new(Geocoder::with_endpoints(endpoints))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 上流が返す本文（200 の JSON）。
    pub fn set(&self, upstream: Upstream, body: &str) {
        let mut state = self.state();
        match upstream {
            Upstream::Gsi => state.gsi = Some(body.to_string()),
            Upstream::Osm => state.osm = Some(body.to_string()),
        }
    }

    pub fn seen(&self) -> Vec<(Upstream, String)> {
        self.state().seen.clone()
    }

    pub fn assert_no_unexpected(&self) {
        let unexpected = self.state().unexpected.clone();
        assert!(unexpected.is_empty(), "想定外の要求: {unexpected:?}");
    }
}

impl Drop for GeocodeStub {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(mut stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
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
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let upstream = match path {
        "/address-search/AddressSearch" => Upstream::Gsi,
        "/search" => Upstream::Osm,
        _ => return reply(&mut stream, 404, ""),
    };
    let body = {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        st.seen.push((upstream, query.to_string()));
        let body = match upstream {
            Upstream::Gsi => st.gsi.clone(),
            Upstream::Osm => st.osm.clone(),
        };
        if body.is_none() {
            st.unexpected.push(format!("{upstream:?}"));
        }
        body
    };
    match body {
        Some(body) => reply(&mut stream, 200, &body),
        None => reply(&mut stream, 500, ""),
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
