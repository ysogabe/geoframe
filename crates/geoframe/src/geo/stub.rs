//! 試験用の地理院のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）で、
//! 逆ジオコーダ・標高 API・`muni.js` の 3 つを返す。応答はすべて合成（座標・地名は架空）。
//! 応答の中身と故障は、座標を受けて返す関数で差し替える。受けた要求は種類と座標で記録する。
//!
//! 応答は 1 要求ごとに `Connection: close`。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// スタブの応答。
#[derive(Debug, Clone)]
pub enum Reply {
    /// 200 と JSON の本文。
    Json(String),
    /// 200 と、JSON とは限らない本文。
    Text(String),
    /// 本文なしの状態コード。
    Status(u16),
    /// 何も返さずに接続を閉じる（応答のヘッダーの前の切断）。
    Hangup,
    /// ヘッダーを返してから、本文の途中で接続を閉じる。
    Truncated,
    /// チャンク長が数でない `Transfer-Encoding: chunked` の本文（`ureq` の `Protocol`）。
    BadChunk,
    /// 途中で終わる gzip の本文を `Content-Encoding: gzip` で送る（`ureq` の `Decompress`）。
    TruncatedGzip,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self::Json(body.to_string())
    }
}

/// 受けた要求の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rgeo,
    Elev,
    Muni,
}

type Responder = Box<dyn Fn(f64, f64) -> Reply + Send>;

struct State {
    rgeo: Responder,
    elev: Responder,
    muni: Reply,
    requests: Vec<(Kind, f64, f64)>,
}

pub struct StubGsi {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StubGsi {
    /// 逆ジオ・標高は空の JSON（`{}`）、`muni.js` は 404 を返す状態で始める。
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            rgeo: Box::new(|_, _| Reply::json("{}")),
            elev: Box::new(|_, _| Reply::json("{}")),
            muni: Reply::Status(404),
            requests: Vec::new(),
        }));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = Arc::clone(&state);
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let _ = handle(stream, &state);
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

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn set_rgeo(&self, f: impl Fn(f64, f64) -> Reply + Send + 'static) {
        self.state().rgeo = Box::new(f);
    }

    pub fn set_elev(&self, f: impl Fn(f64, f64) -> Reply + Send + 'static) {
        self.state().elev = Box::new(f);
    }

    pub fn set_muni(&self, reply: Reply) {
        self.state().muni = reply;
    }

    /// 受けた要求（種類・緯度・経度の順）。`muni.js` の座標は 0。
    pub fn requests(&self) -> Vec<(Kind, f64, f64)> {
        self.state().requests.clone()
    }

    pub fn count(&self, kind: Kind) -> usize {
        self.state()
            .requests
            .iter()
            .filter(|(k, _, _)| *k == kind)
            .count()
    }

    pub fn clear_requests(&self) {
        self.state().requests.clear();
    }
}

impl Drop for StubGsi {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // accept を起こす
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&buf[..pos]).into_owned());
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn query_number(query: &str, key: &str) -> Option<f64> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

fn handle(mut stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    let Some(head) = read_head(&mut stream) else {
        return Ok(());
    };
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let reply = {
        let mut st = state.lock().unwrap();
        if path.ends_with("/muni.js") {
            st.requests.push((Kind::Muni, 0.0, 0.0));
            st.muni.clone()
        } else {
            let (Some(lat), Some(lon)) = (query_number(query, "lat"), query_number(query, "lon"))
            else {
                return respond(&mut stream, &Reply::Status(400));
            };
            if path.ends_with("/LonLatToAddress") {
                st.requests.push((Kind::Rgeo, lat, lon));
                (st.rgeo)(lat, lon)
            } else if path.ends_with("/getelevation.php") {
                st.requests.push((Kind::Elev, lat, lon));
                (st.elev)(lat, lon)
            } else {
                Reply::Status(404)
            }
        }
    };
    respond(&mut stream, &reply)
}

fn respond(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    match reply {
        Reply::BadChunk => {
            return stream.write_all(
                b"HTTP/1.1 200 X\r\nContent-Type: application/json\r\nConnection: close\r\n\
                  Transfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n0\r\n\r\n",
            );
        }
        Reply::TruncatedGzip => {
            // gzip のヘッダー 10 バイト＋最終でない stored ブロックの頭と 4 バイトだけ（ブロックの本体が足りない）
            let gzip: &[u8] = &[
                0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0x03, 0x00, 0x10, 0x00, 0xef, 0xff, b'a', b'b',
                b'c', b'd',
            ];
            let head = format!(
                "HTTP/1.1 200 X\r\nContent-Type: application/json\r\nConnection: close\r\n\
                 Content-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                gzip.len()
            );
            stream.write_all(head.as_bytes())?;
            return stream.write_all(gzip);
        }
        _ => {}
    }
    let (status, content_type, body, claim): (u16, &str, &str, Option<usize>) = match reply {
        Reply::Hangup => return Ok(()),
        Reply::Json(body) => (200, "application/json", body, None),
        Reply::Text(body) => (200, "text/plain", body, None),
        Reply::Status(status) => (*status, "text/plain", "", None),
        Reply::Truncated => (200, "application/json", "{\"resu", Some(1000)),
        Reply::BadChunk | Reply::TruncatedGzip => unreachable!("上で返した"),
    };
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        claim.unwrap_or(body.len())
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
