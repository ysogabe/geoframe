//! 試験用の地理院・Nominatim のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）で、
//! 地理院の地名検索（`/address-search/AddressSearch`）と Nominatim（`/search`）を 1 つのサーバーで受ける。
//! 応答はすべて合成。上流ごとに「次に返す応答」を決め、受けた要求は種類・クエリ文字列・User-Agent で記録する。
//!
//! 偽の時計（[`TestClock`]）と組み合わせる: 応答の `took` だけ時計を進めてから返し、要求を受けた時刻を記録する
//! ので、1 秒 1 件の判定が実時間に依存しない。要求ごとに別のスレッドで答える（同時の要求を試験できる）。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{Clock, Endpoints, Geocoder};

/// 偽の時計。`sleep` は待たずに時計を進め、引数を記録する。
pub struct TestClock {
    now: Mutex<Duration>,
    sleeps: Mutex<Vec<Duration>>,
}

impl TestClock {
    pub fn new(start: Duration) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(start),
            sleeps: Mutex::new(Vec::new()),
        })
    }

    pub fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }

    pub fn take_sleeps(&self) -> Vec<Duration> {
        std::mem::take(&mut *self.sleeps.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// `Geocoder` に渡す時計（`Arc<TestClock>` を共有する）。
pub struct SharedClock(pub Arc<TestClock>);

impl Clock for SharedClock {
    fn now(&self) -> Duration {
        self.0.now()
    }

    fn sleep(&self, duration: Duration) {
        self.0
            .sleeps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(duration);
        self.0.advance(duration);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Upstream {
    Gsi,
    Osm,
}

impl Upstream {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gsi => "gsi",
            Self::Osm => "osm",
        }
    }
}

/// スタブの応答。
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// 何も返さずに接続を閉じる（応答のヘッダーの前の切断）。
    pub hangup: bool,
    /// 返す前に偽の時計を進める（通信にかかった時間）。
    pub took: Duration,
    /// `body` は gzip で、`Content-Encoding: gzip` を付ける。
    pub gzip: bool,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            body: body.as_bytes().to_vec(),
            hangup: false,
            took: Duration::ZERO,
            gzip: false,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
            hangup: false,
            took: Duration::ZERO,
            gzip: false,
        }
    }

    /// 展開すると `head` の後ろに空白が `pad` 個続く JSON になる gzip の本文（圧縮後はごく小さい）。
    pub fn gzip_padded(head: &str, pad: usize) -> Self {
        Self {
            body: crate::testutil::gzip::padded(head.as_bytes(), pad),
            gzip: true,
            ..Self::status(200)
        }
    }

    pub fn hangup() -> Self {
        Self {
            hangup: true,
            ..Self::status(0)
        }
    }
}

/// 受けた要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub upstream: Upstream,
    /// `?` より後ろ（URL のクエリ文字列。エンコードしたまま）。
    pub query: String,
    pub user_agent: String,
    /// 受けた時刻（偽の時計）。
    pub at: Duration,
}

type Hook = Arc<dyn Fn(Upstream) + Send + Sync>;

struct State {
    gsi: Option<Reply>,
    osm: Option<Reply>,
    seen: Vec<Seen>,
    hook: Option<Hook>,
}

pub struct StubUpstreams {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StubUpstreams {
    pub fn start(clock: Arc<TestClock>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            gsi: None,
            osm: None,
            seen: Vec::new(),
            hook: None,
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
                        let (state, clock) = (Arc::clone(&state), Arc::clone(&clock));
                        std::thread::spawn(move || {
                            let _ = handle(stream, &state, &clock);
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

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 上流が次に返す応答。`None` にすると、要求が来たとき 500 を返す（試験が失敗するよう記録は残る）。
    pub fn set(&self, upstream: Upstream, reply: Option<Reply>) {
        let mut state = self.state();
        match upstream {
            Upstream::Gsi => state.gsi = reply,
            Upstream::Osm => state.osm = reply,
        }
    }

    /// 要求を受けたあと、応答を返す前に呼ぶ関数（応答を止めたり、時刻を記録したりする）。
    pub fn set_hook(&self, hook: impl Fn(Upstream) + Send + Sync + 'static) {
        self.state().hook = Some(Arc::new(hook));
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    pub fn take_seen(&self) -> Vec<Seen> {
        std::mem::take(&mut self.state().seen)
    }

    pub fn count(&self, upstream: Upstream) -> usize {
        self.state()
            .seen
            .iter()
            .filter(|s| s.upstream == upstream)
            .count()
    }
}

impl Drop for StubUpstreams {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
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

fn handle(mut stream: TcpStream, state: &Mutex<State>, clock: &TestClock) -> std::io::Result<()> {
    let Some(head) = read_head(&mut stream) else {
        return Ok(());
    };
    let mut lines = head.lines();
    let target = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_string();
    let user_agent = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let upstream = match path {
        "/address-search/AddressSearch" => Upstream::Gsi,
        "/search" => Upstream::Osm,
        _ => return write_reply(&mut stream, &Reply::status(404)),
    };
    let (reply, hook) = {
        let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
        st.seen.push(Seen {
            upstream,
            query: query.to_string(),
            user_agent,
            at: clock.now(),
        });
        let reply = match upstream {
            Upstream::Gsi => st.gsi.clone(),
            Upstream::Osm => st.osm.clone(),
        };
        (reply, st.hook.clone())
    };
    if let Some(hook) = hook {
        hook(upstream);
    }
    let reply = reply.unwrap_or_else(|| Reply::status(500));
    clock.advance(reply.took);
    write_reply(&mut stream, &reply)
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    if reply.hangup {
        return Ok(());
    }
    let head = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nConnection: close\r\n{}Content-Length: {}\r\n\r\n",
        reply.status,
        if reply.gzip {
            "Content-Encoding: gzip\r\n"
        } else {
            ""
        },
        reply.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

/// 何も受け付けない接続先の `Geocoder`（試験の既定。`AppState::new` は本番の接続先なので、検索語が 2 文字以上の
/// E4 を叩く試験が本物の地理院・Nominatim へ出ないよう、`with_geocoder` でこれに差し替える）。要求が 1 つでも
/// 届いたら**プロセスごと落とす**（通信が要る試験は [`StubUpstreams`] を作って向ける）。
pub fn deny_geocoder() -> Arc<Geocoder> {
    static DENY: std::sync::OnceLock<(SocketAddr, Arc<Geocoder>)> = std::sync::OnceLock::new();
    let (_, geocoder) = DENY.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                eprintln!(
                    "geocode stub: 想定外の地名検索の通信が届いた（既定の接続先は何も受け付けない）"
                );
                std::process::abort();
            }
        });
        let geocoder = Geocoder::with_endpoints(Endpoints::loopback(addr).unwrap());
        (addr, Arc::new(geocoder))
    });
    Arc::clone(geocoder)
}
