//! 試験用の上流（OSM・地理院）のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）で、
//! 層ごとのパス（`/osm/<z>/<x>/<y>.png`・`/gsi/…`）を 1 つのサーバーで受ける。応答はすべて合成。
//!
//! 「次に返す応答」を順に並べておき（リダイレクトでは複数回来る）、受けた要求は、パスと `Host` 以外のヘッダで
//! 記録する。要求ごとに別のスレッドで答える（同時の要求を試験できる）。`hook` は要求を記録した直後、応答の前に
//! 呼ばれ、ブロックして応答を止められる（同時に開いている数の試験）。

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use super::{Layer, TileEndpoints};

/// スタブの応答。
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// 何も返さずに接続を閉じる。
    pub hangup: bool,
}

impl Reply {
    pub fn png(body: &[u8]) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "image/png".into())],
            body: body.to_vec(),
            hangup: false,
        }
    }

    pub fn hangup() -> Self {
        Self {
            hangup: true,
            ..Self::status(0)
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            hangup: false,
        }
    }

    /// `Content-Encoding: gzip` の本文（展開すると PNG のマジック + `pad` 個の空白。圧縮後はごく小さい）。
    pub fn gzip_png(pad: usize) -> Self {
        let decoded = super::PNG_MAGIC;
        Self {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "image/png".into()),
                ("Content-Encoding".into(), "gzip".into()),
            ],
            body: crate::testutil::gzip::padded(decoded, pad),
            hangup: false,
        }
    }

    /// フィクスチャの応答（`body_hex`・`body_pad`・`headers`・`hangup`）。
    pub fn from_json(v: &Value) -> Self {
        if v.get("hangup").and_then(Value::as_bool) == Some(true) {
            return Self::hangup();
        }
        let body = match v.get("body_pad").and_then(Value::as_u64) {
            Some(pad) => {
                let mut body = super::PNG_MAGIC.to_vec();
                body.resize(body.len() + pad as usize, 0);
                body
            }
            None => hex_decode(v["body_hex"].as_str().unwrap_or("")),
        };
        let headers = v["headers"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            status: v["status"].as_u64().unwrap() as u16,
            headers,
            body,
            hangup: false,
        }
    }
}

pub fn hex_decode(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 受けた要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub path: String,
    /// 小文字の名前 → 値（`host` は除く）。
    pub headers: BTreeMap<String, String>,
}

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct State {
    replies: VecDeque<Reply>,
    fallback: Option<Reply>,
    seen: Vec<Seen>,
    unscripted: Vec<String>,
    hook: Option<Hook>,
    /// 層ごとの、今応答を待っている数と、その最大。
    in_flight: [usize; 2],
    max_in_flight: [usize; 2],
}

pub struct StubUpstream {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StubUpstream {
    pub fn start() -> Self {
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

    pub fn endpoints(&self) -> TileEndpoints {
        TileEndpoints::stub(&format!("http://{}", self.addr))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 次に返す応答を並べ直す（受けた要求の記録は残す）。
    pub fn script(&self, replies: Vec<Reply>) {
        self.state().replies = replies.into();
    }

    /// 並べた応答がなくなったあとの要求にも、同じ応答を返し続ける（既定は 500 で、想定外の要求として記録する）。
    pub fn always(&self, reply: Reply) {
        self.state().fallback = Some(reply);
    }

    pub fn take_seen(&self) -> Vec<Seen> {
        std::mem::take(&mut self.state().seen)
    }

    pub fn unscripted(&self) -> Vec<String> {
        self.state().unscripted.clone()
    }

    pub fn unused_replies(&self) -> usize {
        self.state().replies.len()
    }

    pub fn set_hook(&self, hook: Option<Hook>) {
        self.state().hook = hook;
    }

    /// 層ごとの、同時に応答を待っていた数の最大。
    pub fn max_in_flight(&self, layer: Layer) -> usize {
        self.state().max_in_flight[layer.index()]
    }

    pub fn in_flight(&self, layer: Layer) -> usize {
        self.state().in_flight[layer.index()]
    }
}

impl Drop for StubUpstream {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn layer_of(path: &str) -> Option<Layer> {
    let first = path.trim_start_matches('/').split('/').next()?;
    Layer::parse(first)
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
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let headers: BTreeMap<String, String> = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .filter(|(k, _)| k != "host")
        .collect();
    let layer = layer_of(&path);
    let (reply, hook) = {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        st.seen.push(Seen {
            path: path.clone(),
            headers,
        });
        if let Some(layer) = layer {
            let i = layer.index();
            st.in_flight[i] += 1;
            st.max_in_flight[i] = st.max_in_flight[i].max(st.in_flight[i]);
        }
        let reply = st.replies.pop_front().or_else(|| st.fallback.clone());
        if reply.is_none() {
            st.unscripted.push(path.clone());
        }
        (reply, st.hook.clone())
    };
    if let Some(hook) = hook {
        hook(&path);
    }
    let result = answer(&mut stream, reply.unwrap_or_else(|| Reply::status(500)));
    if let Some(layer) = layer {
        state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_flight[layer.index()] -= 1;
    }
    result
}

fn answer(stream: &mut TcpStream, reply: Reply) -> std::io::Result<()> {
    if reply.hangup {
        return Ok(());
    }
    let mut head = format!("HTTP/1.1 {} X\r\n", reply.status);
    let mut has_length = false;
    for (name, value) in &reply.headers {
        has_length |= name.eq_ignore_ascii_case("content-length");
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !has_length {
        head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

/// 何も受け付けない接続先（試験の既定）。`AppState::new` は本番の接続先なので、`/tiles/…` を叩く試験が本物の OSM・
/// 地理院へ出ないよう、`with_tiles` でこれに向けた `TileProxy` に差し替える。要求が 1 つでも届いたら**プロセスごと
/// 落とす**（通信が要る試験は [`StubUpstream`] を作って向ける）。
pub fn deny_endpoints() -> TileEndpoints {
    static DENY: std::sync::OnceLock<SocketAddr> = std::sync::OnceLock::new();
    let addr = DENY.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                eprintln!(
                    "tile stub: 想定外の地図タイルの通信が届いた（既定の接続先は何も受け付けない）"
                );
                std::process::abort();
            }
        });
        addr
    });
    TileEndpoints::loopback(*addr).unwrap()
}
