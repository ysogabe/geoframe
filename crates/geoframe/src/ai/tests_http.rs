//! AI 専用 HTTP（[`super::http`]・[`super::dest`]・[`super::lanes`]）の試験。ループバックのスタブサーバー・偽の DNS・
//! 合成の本文だけを使い、外へは出ない。
//!
//! - 固定のヘッダー（`X-Client-Name`・Host）・`Origin`／認証なし・リダイレクトを追わない（Location へ接続 0）・
//!   環境のプロキシを使わない（子プロセスで `HTTP_PROXY` を与えて確認）
//! - 接続先の固定: 公開アドレスの混在・過多・空・rebinding を全体拒否（接続 0）、Host と TLS の SNI は元の host のまま
//! - 要求 8 MiB（+1 で受信 0・DNS 0）、応答 1 MiB（+1・展開後・圧縮前の上限）、遅い DNS／ヘッダー／本文の全体期限
//! - 背景と対話の枠、対話の blocking 境界（枠が使用中なら待たず Busy）、停止後は送信 0

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::dest::{self, Lookup};
use super::http::{
    AiHttp, Deadline, Endpoint, HttpError, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, ServiceId,
};
use super::lanes::Lane;
use crate::net::Failure;
use crate::stop::StopFlag;

// ---------------------------------------------------------------- 偽の DNS・スタブサーバー

/// 名前→アドレスの表。呼び出し回数と同時に走った数を数える。
pub(super) struct FakeDns {
    table: Mutex<HashMap<String, Vec<Vec<SocketAddr>>>>,
    pub calls: AtomicUsize,
    delay: Mutex<Duration>,
    running: AtomicUsize,
    pub max_running: AtomicUsize,
    /// 名前解決の最中に呼ぶ処理（停止の合図を立てるなど。解決結果を返す直前）。
    on_lookup: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl FakeDns {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            table: Mutex::new(HashMap::new()),
            calls: AtomicUsize::new(0),
            delay: Mutex::new(Duration::ZERO),
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(0),
            on_lookup: Mutex::new(None),
        })
    }

    /// 名前解決の最中（結果を返す直前）に `f` を呼ぶ。
    pub fn on_lookup(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_lookup.lock().unwrap() = Some(Arc::new(f));
    }

    /// `host` の解決結果を差し込む。呼び出しごとに先頭から順に使い、最後の結果は繰り返す。
    pub fn script(&self, host: &str, answers: Vec<Vec<SocketAddr>>) {
        self.table.lock().unwrap().insert(host.to_string(), answers);
    }

    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }
}

impl Lookup for Arc<FakeDns> {
    fn lookup(&self, host: &str, _port: u16) -> std::io::Result<Vec<SocketAddr>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
        let delay = *self.delay.lock().unwrap();
        std::thread::sleep(delay);
        let hook = self.on_lookup.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
        self.running.fetch_sub(1, Ordering::SeqCst);
        let table = self.table.lock().unwrap();
        match table.get(host) {
            Some(answers) if !answers.is_empty() => Ok(answers[n.min(answers.len() - 1)].clone()),
            _ => Err(std::io::Error::other("nxdomain")),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Captured {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body_len: usize,
}

impl Captured {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn Fn(&mut TcpStream, &Captured) + Send + Sync;

/// ループバックの HTTP スタブ。接続数と受けた要求を数える。
pub(super) struct Stub {
    pub addr: SocketAddr,
    pub connections: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<Captured>>>,
    /// 受け取ったバイト数（要求の一部でも届いていれば 0 でなくなる）。
    pub bytes: Arc<AtomicUsize>,
}

pub(super) fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

pub(super) fn stub(handler: impl Fn(&mut TcpStream, &Captured) + Send + Sync + 'static) -> Stub {
    stub_with_first_bytes(|| {}, handler)
}

/// 最初のバイトが届いた瞬間（要求を読み切る前）に `first_bytes` を呼ぶスタブ。
pub(super) fn stub_with_first_bytes(
    first_bytes: impl Fn() + Send + Sync + 'static,
    handler: impl Fn(&mut TcpStream, &Captured) + Send + Sync + 'static,
) -> Stub {
    let first_bytes: Arc<dyn Fn() + Send + Sync> = Arc::new(first_bytes);
    let bytes = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<Handler> = Arc::new(handler);
    {
        let (connections, requests) = (Arc::clone(&connections), Arc::clone(&requests));
        let bytes = Arc::clone(&bytes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                connections.fetch_add(1, Ordering::SeqCst);
                let (handler, requests) = (Arc::clone(&handler), Arc::clone(&requests));
                let (bytes, first_bytes) = (Arc::clone(&bytes), Arc::clone(&first_bytes));
                std::thread::spawn(move || {
                    let Some(captured) = read_request(&mut stream, &bytes, &*first_bytes) else {
                        return;
                    };
                    requests.lock().unwrap().push(captured.clone());
                    handler(&mut stream, &captured);
                });
            }
        });
    }
    Stub {
        addr,
        connections,
        requests,
        bytes,
    }
}

fn read_request(
    stream: &mut TcpStream,
    total: &AtomicUsize,
    first_bytes: &(dyn Fn() + Send + Sync),
) -> Option<Captured> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        if total.fetch_add(n, Ordering::SeqCst) == 0 {
            first_bytes();
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut have = buf.len() - head_end;
    while have < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        total.fetch_add(n, Ordering::SeqCst);
        have += n;
    }
    Some(Captured {
        request_line,
        headers,
        body_len: length,
    })
}

/// `lab.test` が `addr` のループバックを指す LAN の入口の `AiHttp`。
pub(super) fn lab_http(addr: SocketAddr, dns: &Arc<FakeDns>) -> AiHttp {
    dns.script("lab.test", vec![vec![addr]]);
    AiHttp::with_lookup(
        &format!("http://lab.test:{}", addr.port()),
        Arc::new(Arc::clone(dns)),
    )
    .unwrap()
}

fn session(http: &AiHttp) -> super::http::Session {
    http.session(Lane::Background, Duration::ZERO, &StopFlag::new())
        .unwrap()
}

fn catalog_get(http: &AiHttp, limit: Duration) -> Result<super::http::Reply, HttpError> {
    session(http).get(Endpoint::Catalog, &Deadline::after(limit))
}

// ---------------------------------------------------------------- 固定のヘッダー・リダイレクト・プロキシ

#[test]
fn requests_carry_only_the_fixed_headers_and_the_original_host() {
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let id = ServiceId::parse("example-svc").unwrap();
    let s = session(&http);
    let d = Deadline::after(Duration::from_secs(5));
    s.get(Endpoint::Catalog, &d).unwrap();
    s.get(Endpoint::Service(&id), &d).unwrap();
    s.get(Endpoint::DecisionHealth, &d).unwrap();
    s.post_json(Endpoint::DecisionSystemOne, b"{}", &d).unwrap();
    let requests = server.requests.lock().unwrap();
    let lines: Vec<&str> = requests.iter().map(|r| r.request_line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "GET /llm/catalog HTTP/1.1",
            "GET /llm/services/example-svc HTTP/1.1",
            "GET /llm-decision/health HTTP/1.1",
            "POST /llm-decision/v1/systemone HTTP/1.1",
        ]
    );
    for r in requests.iter() {
        // URL を IP に書き換えず、Host は設定の名前のまま
        assert_eq!(
            r.header("host"),
            Some(format!("lab.test:{}", server.addr.port()).as_str())
        );
        assert_eq!(r.header("x-client-name"), Some("geoframe"));
        for forbidden in [
            "origin",
            "authorization",
            "cookie",
            "proxy-authorization",
            "referer",
        ] {
            assert!(r.header(forbidden).is_none(), "{forbidden}");
        }
        assert!(r.header("user-agent").unwrap().starts_with("geoframe/"));
    }
    assert_eq!(requests[3].header("content-type"), Some("application/json"));
}

#[test]
fn service_ids_cannot_inject_paths() {
    for bad in [
        "",
        "../x",
        "a/b",
        "a b",
        ".hidden",
        "a..b",
        "x?y",
        "x#y",
        "日本",
        &"a".repeat(65),
    ] {
        assert!(ServiceId::parse(bad).is_none(), "{bad:?}");
    }
    for ok in ["decision", "example-embed-svc", "a.b_c-1", &"a".repeat(64)] {
        assert!(ServiceId::parse(ok).is_some(), "{ok:?}");
    }
}

#[test]
fn redirects_are_never_followed_and_nothing_reaches_the_target() {
    let target = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let location = format!("Location: http://{}/stolen\r\n", target.addr);
    let origin = stub(move |stream, _| respond(stream, "302 Found", &location, b""));
    let dns = FakeDns::new();
    let http = lab_http(origin.addr, &dns);
    assert_eq!(
        catalog_get(&http, Duration::from_secs(5)).unwrap_err(),
        HttpError::Redirect(302)
    );
    assert_eq!(
        session(&http)
            .post_json(
                Endpoint::DecisionSystemOne,
                b"{\"secret\":1}",
                &Deadline::after(Duration::from_secs(5))
            )
            .unwrap_err(),
        HttpError::Redirect(302)
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(target.connections.load(Ordering::SeqCst), 0);
    // すべての 3xx が同じ扱い
    for code in [
        "301 Moved",
        "303 See Other",
        "307 Temporary Redirect",
        "308 Permanent Redirect",
    ] {
        let origin = stub(move |stream, _| {
            respond(stream, code, "Location: http://10.255.255.1/x\r\n", b"")
        });
        let http = lab_http(origin.addr, &dns);
        assert!(matches!(
            catalog_get(&http, Duration::from_secs(5)).unwrap_err(),
            HttpError::Redirect(_)
        ));
    }
}

/// 子プロセスで環境のプロキシを与えて確認する（`unsafe` な環境変数の書き換えをテストの中でしない）。
#[test]
fn environment_proxies_are_never_used() {
    if std::env::var("GEOFRAME_TEST_PROXY_CHILD").is_ok() {
        return proxy_child();
    }
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    {
        let hits = Arc::clone(&hits);
        std::thread::spawn(move || {
            for _ in proxy.incoming() {
                hits.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let url = format!("http://{proxy_addr}");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ai::tests_http::environment_proxies_are_never_used",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("GEOFRAME_TEST_PROXY_CHILD", server.addr.to_string())
        .env("PROXY_PROBE", proxy_addr.to_string())
        .env("HTTP_PROXY", &url)
        .env("http_proxy", &url)
        .env("HTTPS_PROXY", &url)
        .env("https_proxy", &url)
        .env("ALL_PROXY", &url)
        .env("all_proxy", &url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // 子の AI 専用 agent の要求は、プロキシを通らず直接スタブへ届いた。対照として、一般の net::agent は
    // 環境のプロキシへ接続する（この試験が意味を持つことの確認）
    let seen = server.requests.lock().unwrap().len();
    assert_eq!(seen, 1);
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "対照: 一般の agent はプロキシを見る"
    );
}

fn proxy_child() {
    let server: SocketAddr = std::env::var("GEOFRAME_TEST_PROXY_CHILD")
        .unwrap()
        .parse()
        .unwrap();
    let dns = FakeDns::new();
    let http = lab_http(server, &dns);
    // 名前で解決できる入口: プロキシを使えば、プロキシへ CONNECT 風の要求が行く
    let reply = catalog_get(&http, Duration::from_secs(5)).unwrap();
    assert_eq!(reply.status, 200);
    // 対照: 一般の agent は環境のプロキシへ接続しようとする（名前は解決しない）
    let control = crate::net::agent()
        .get(&format!("http://lab.test:{}/llm/catalog", server.port()))
        .call();
    assert!(control.is_err() || control.unwrap().status().as_u16() != 200);
}

// ---------------------------------------------------------------- 接続先の固定

fn sa(ip: &str, port: u16) -> SocketAddr {
    SocketAddr::new(ip.parse().unwrap(), port)
}

#[test]
fn a_public_address_anywhere_in_the_answer_denies_everything() {
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let port = server.addr.port();
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let cases: Vec<(&str, Vec<SocketAddr>)> = vec![
        (
            "public after private",
            vec![server.addr, sa("203.0.113.9", port)],
        ),
        (
            "public before private",
            vec![sa("203.0.113.9", port), server.addr],
        ),
        ("public ipv6", vec![server.addr, sa("2001:db8::1", port)]),
        ("mapped public", vec![sa("::ffff:8.8.8.8", port)]),
        ("link-local", vec![sa("169.254.1.1", port)]),
        (
            "too many",
            (0..17).map(|i| sa(&format!("10.0.0.{i}"), port)).collect(),
        ),
    ];
    for (name, answer) in cases {
        dns.script("lab.test", vec![answer]);
        assert_eq!(
            catalog_get(&http, Duration::from_secs(5)).unwrap_err(),
            HttpError::DestinationDenied,
            "{name}"
        );
    }
    // 解決結果が空・名前が無い
    dns.script("lab.test", vec![vec![]]);
    assert_eq!(
        catalog_get(&http, Duration::from_secs(5)).unwrap_err(),
        HttpError::Net(Failure::Dns)
    );
    dns.script("other.test", vec![vec![server.addr]]);
    std::thread::sleep(Duration::from_millis(100));
    // どの場合も、スタブへの接続は 0
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn every_connection_resolves_again_and_a_rebound_name_is_refused_without_sending() {
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    // 1 回目は private、2 回目は公開アドレス（rebinding）
    dns.script(
        "lab.test",
        vec![
            vec![server.addr],
            vec![sa("203.0.113.9", server.addr.port())],
        ],
    );
    let s = session(&http);
    let d = Deadline::after(Duration::from_secs(5));
    assert_eq!(s.get(Endpoint::Catalog, &d).unwrap().status, 200);
    // 接続を再利用できても、毎回検査してから使う（公開に変わったら送らない）
    assert_eq!(
        s.get(Endpoint::Catalog, &d).unwrap_err(),
        HttpError::DestinationDenied
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_eq!(dns.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn ip_literal_targets_are_vetted_without_dns() {
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = AiHttp::with_lookup(
        &format!("http://{}", server.addr),
        Arc::new(Arc::clone(&dns)),
    )
    .unwrap();
    assert_eq!(
        catalog_get(&http, Duration::from_secs(5)).unwrap().status,
        200
    );
    let public =
        AiHttp::with_lookup("http://203.0.113.9:8100", Arc::new(Arc::clone(&dns))).unwrap();
    assert_eq!(
        catalog_get(&public, Duration::from_secs(5)).unwrap_err(),
        HttpError::DestinationDenied
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    // 設定に使えない URL は作れない
    for bad in [
        "ftp://10.0.0.1",
        "http://user@10.0.0.1",
        "http://10.0.0.1/path",
        "http://10.0.0.1/?q=1",
        "10.0.0.1",
        "",
    ] {
        assert_eq!(
            AiHttp::with_lookup(bad, Arc::new(Arc::clone(&dns))).unwrap_err(),
            HttpError::BadTarget,
            "{bad:?}"
        );
    }
}

/// HTTPS の入口では、TLS の SNI も元の名前のまま（IP に書き換えない）。ハンドシェイクの最初のバイト列に
/// 名前が平文で入る。証明書は検証されず接続は失敗する（それでよい）。
#[test]
fn the_tls_server_name_stays_the_configured_host() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hello = Arc::new(Mutex::new(Vec::new()));
    {
        let hello = Arc::clone(&hello);
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                if let Ok(n) = stream.read(&mut buf) {
                    *hello.lock().unwrap() = buf[..n].to_vec();
                }
            }
        });
    }
    let dns = FakeDns::new();
    dns.script("lab.test", vec![vec![addr]]);
    let http = AiHttp::with_lookup(
        &format!("https://lab.test:{}", addr.port()),
        Arc::new(Arc::clone(&dns)),
    )
    .unwrap();
    let err = catalog_get(&http, Duration::from_secs(5)).unwrap_err();
    assert!(
        matches!(err, HttpError::Net(_) | HttpError::Timeout),
        "{err:?}"
    );
    let hello = hello.lock().unwrap();
    assert!(hello.windows(8).any(|w| w == b"lab.test"), "SNI に元の名前");
    assert!(!hello.windows(9).any(|w| w == b"127.0.0.1"));
}

// ---------------------------------------------------------------- 大きさの上限

#[test]
fn request_limit_is_checked_before_any_dns_or_connection() {
    let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let s = session(&http);
    let d = Deadline::after(Duration::from_secs(30));
    let over = vec![b' '; MAX_REQUEST_BYTES + 1];
    assert_eq!(
        s.post_json(Endpoint::DecisionSystemOne, &over, &d)
            .unwrap_err(),
        HttpError::RequestTooLarge
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    // ちょうど 8 MiB は送れる
    let exact = vec![b' '; MAX_REQUEST_BYTES];
    assert_eq!(
        s.post_json(Endpoint::DecisionSystemOne, &exact, &d)
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        server.requests.lock().unwrap()[0].body_len,
        MAX_REQUEST_BYTES
    );
}

fn serve_gzip(body: Vec<u8>) -> Stub {
    stub(move |stream, _| {
        respond(stream, "200 OK", "Content-Encoding: gzip\r\n", &body);
    })
}

#[test]
fn response_limit_applies_to_plain_decoded_and_wire_bytes() {
    let limit = MAX_RESPONSE_BYTES as usize;
    let dns = FakeDns::new();
    let get = |server: &Stub| catalog_get(&lab_http(server.addr, &dns), Duration::from_secs(10));
    // 1 MiB ちょうど
    let exact = stub(move |s, _| respond(s, "200 OK", "", &vec![b' '; limit]));
    assert_eq!(get(&exact).unwrap().body.len(), limit);
    // +1
    let over = stub(move |s, _| respond(s, "200 OK", "", &vec![b' '; limit + 1]));
    assert_eq!(get(&over).unwrap_err(), HttpError::ResponseTooLarge);
    // 圧縮後は小さいが、展開後に 1 MiB を超える
    let bomb = serve_gzip(crate::testutil::gzip::padded(b"", limit + 1));
    assert_eq!(get(&bomb).unwrap_err(), HttpError::ResponseTooLarge);
    let ok_gzip = serve_gzip(crate::testutil::gzip::padded(b"{}", limit - 2));
    assert_eq!(get(&ok_gzip).unwrap().body.len(), limit);
    // 展開後が空の gzip を終わりなく流し続ける: 圧縮前の上限で止まり、期限まで読み続けない
    let member = crate::testutil::gzip::padded(b"", 0);
    let chunk = member.repeat(512);
    let endless = stub(move |s, _| {
        let head = "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n";
        if s.write_all(head.as_bytes()).is_err() {
            return;
        }
        for _ in 0..8192 {
            if s.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    let started = Instant::now();
    assert_eq!(get(&endless).unwrap_err(), HttpError::ResponseTooLarge);
    assert!(started.elapsed() < Duration::from_secs(8));
    // 2xx 以外は本文を読まない（巨大でも受け取らない）
    let error_body = stub(move |s, _| {
        respond(
            s,
            "503 Service Unavailable",
            "Retry-After: 120\r\n",
            &vec![b'x'; limit * 3],
        )
    });
    let reply = get(&error_body).unwrap();
    assert_eq!(
        (reply.status, reply.retry_after_secs, reply.body.len()),
        (503, Some(120), 0)
    );
}

#[test]
fn retry_after_is_read_only_as_seconds() {
    let dns = FakeDns::new();
    for (header, expected) in [
        ("Retry-After: 90\r\n", Some(90)),
        ("Retry-After: Wed, 21 Oct 2026 07:28:00 GMT\r\n", None),
        ("Retry-After: -5\r\n", None),
        ("Retry-After: 99999999999999\r\n", None),
        ("", None),
    ] {
        let server = stub(move |s, _| respond(s, "429 Too Many Requests", header, b""));
        let reply = catalog_get(&lab_http(server.addr, &dns), Duration::from_secs(5)).unwrap();
        assert_eq!(reply.status, 429);
        assert_eq!(reply.retry_after_secs, expected, "{header}");
    }
}

// ---------------------------------------------------------------- 全体期限（DNS・ヘッダー・本文）

#[test]
fn deadlines_cover_dns_headers_and_body() {
    let dns = FakeDns::new();
    // 遅いヘッダー
    let slow_head = stub(|s, _| {
        std::thread::sleep(Duration::from_secs(3));
        respond(s, "200 OK", "", b"{}");
    });
    let started = Instant::now();
    assert_eq!(
        catalog_get(&lab_http(slow_head.addr, &dns), Duration::from_millis(400)).unwrap_err(),
        HttpError::Timeout
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // 本文を少しずつ流し続ける（ヘッダーは来る）: 読み切りまで含めた全体期限で止まる
    let trickle = stub(|s, _| {
        let _ =
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n");
        for _ in 0..100 {
            if s.write_all(b"x").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let started = Instant::now();
    let err = catalog_get(&lab_http(trickle.addr, &dns), Duration::from_millis(500)).unwrap_err();
    assert!(
        matches!(err, HttpError::Timeout | HttpError::Net(Failure::Timeout)),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    // 期限を使い切っていれば、DNS もしない
    let before = dns.calls.load(Ordering::SeqCst);
    let past = Deadline::after(Duration::ZERO);
    let http = lab_http(slow_head.addr, &dns);
    let calls = dns.calls.load(Ordering::SeqCst);
    assert_eq!(
        session(&http).get(Endpoint::Catalog, &past).unwrap_err(),
        HttpError::Timeout
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), calls);
    let _ = before;
}

#[test]
fn slow_dns_is_bounded_by_the_deadline_and_by_a_cap_on_helper_threads() {
    let server = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    dns.set_delay(Duration::from_secs(2));
    let started = Instant::now();
    // 要求の期限（300ms）を越えて DNS を待たない
    assert_eq!(
        catalog_get(&http, Duration::from_millis(300)).unwrap_err(),
        HttpError::Timeout
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    // 遅い DNS が溜まっても、同時に走る補助スレッドは上限まで（超えた要求は即座に時間切れ）
    for _ in 0..12 {
        let _ = catalog_get(&http, Duration::from_millis(100));
    }
    assert!(
        dns.max_running.load(Ordering::SeqCst) <= dest::MAX_INFLIGHT_LOOKUPS,
        "{}",
        dns.max_running.load(Ordering::SeqCst)
    );
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- 枠・blocking 境界・停止

#[test]
fn a_stopped_flag_sends_nothing() {
    let server = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let stop = StopFlag::new();
    let s = http
        .session(Lane::Background, Duration::ZERO, &stop)
        .unwrap();
    stop.stop();
    assert_eq!(
        s.get(Endpoint::Catalog, &Deadline::after(Duration::from_secs(5)))
            .unwrap_err(),
        HttpError::Stopping
    );
    assert_eq!(
        http.session(Lane::Background, Duration::ZERO, &stop)
            .unwrap_err(),
        HttpError::Stopping
    );
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- 停止: 要求を書き始める境界

/// 停止の合図を立てるクロージャー（DNS・スタブの差し込みから呼ぶ）。
fn stopper(stop: &StopFlag) -> impl Fn() + Send + Sync + 'static {
    let stop = stop.clone();
    move || stop.stop()
}

fn started_session(http: &AiHttp, stop: &StopFlag) -> super::http::Session {
    http.session(Lane::Background, Duration::ZERO, stop)
        .unwrap()
}

/// 名前解決の最中に停止が来て、解決は（ループバックを）返す: 接続も要求も出ない。
/// 旧実装（`send` の入口だけで確認）は、このあと接続して要求を送った。
#[test]
fn a_stop_during_name_resolution_connects_and_sends_nothing() {
    let server = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let stop = StopFlag::new();
    let session = started_session(&http, &stop);
    dns.on_lookup(stopper(&stop));
    assert_eq!(
        session
            .get(Endpoint::Catalog, &Deadline::after(Duration::from_secs(5)))
            .unwrap_err(),
        HttpError::Stopping
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    assert_eq!(server.bytes.load(Ordering::SeqCst), 0);
    // POST も同じ（本文があっても出ない）
    drop(session);
    let stop = StopFlag::new();
    dns.on_lookup(stopper(&stop));
    let session = started_session(&http, &stop);
    assert_eq!(
        session
            .post_json(
                Endpoint::DecisionSystemOne,
                b"{\"x\":1}",
                &Deadline::after(Duration::from_secs(5))
            )
            .unwrap_err(),
        HttpError::Stopping
    );
    assert_eq!(
        (
            server.connections.load(Ordering::SeqCst),
            server.bytes.load(Ordering::SeqCst)
        ),
        (0, 0)
    );
}

/// 接続（TCP の確立）を終えたあと、要求を書き始める前に停止が来た: 接続は張られているが、要求のバイトは 0。
/// 旧実装は DNS の前しか確認しなかったので、ここで要求を送って 200 を受けた。
#[test]
fn a_stop_after_the_connection_is_made_writes_no_request_bytes() {
    let server = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    dns.script("lab.test", vec![vec![server.addr]]);
    let stop = StopFlag::new();
    let http = AiHttp::with_lookup_and_connect_hook(
        &format!("http://lab.test:{}", server.addr.port()),
        Arc::new(Arc::clone(&dns)),
        Arc::new(stopper(&stop)),
    )
    .unwrap();
    let session = started_session(&http, &stop);
    assert_eq!(
        session
            .get(Endpoint::Catalog, &Deadline::after(Duration::from_secs(5)))
            .unwrap_err(),
        HttpError::Stopping
    );
    assert!(stop.is_stopped());
    // 接続を張ったことは受け側に見えるが、HTTP のバイトは 1 つも届かない
    let started = Instant::now();
    while server.connections.load(Ordering::SeqCst) == 0
        && started.elapsed() < Duration::from_secs(2)
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    assert_eq!(server.bytes.load(Ordering::SeqCst), 0);
    assert!(server.requests.lock().unwrap().is_empty());
    // POST（本文あり）も同じ
    let stop = StopFlag::new();
    let http = AiHttp::with_lookup_and_connect_hook(
        &format!("http://lab.test:{}", server.addr.port()),
        Arc::new(Arc::clone(&dns)),
        Arc::new(stopper(&stop)),
    )
    .unwrap();
    assert_eq!(
        started_session(&http, &stop)
            .post_json(
                Endpoint::DecisionSystemOne,
                &vec![b'x'; 100_000],
                &Deadline::after(Duration::from_secs(5))
            )
            .unwrap_err(),
        HttpError::Stopping
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.bytes.load(Ordering::SeqCst), 0);
}

/// 要求を書き始めたあとに停止が来ても、残りの本文の送信・応答の受信・結果の確定は止めない。
#[test]
fn a_stop_after_the_first_byte_does_not_cut_the_exchange() {
    let stop = StopFlag::new();
    let server = stub_with_first_bytes(stopper(&stop), |s, req| {
        // 本文は全部届いてから応答する
        assert_eq!(req.body_len, 3 << 20);
        respond(s, "200 OK", "", b"{\"ok\":true}");
    });
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let session = started_session(&http, &stop);
    let reply = session
        .post_json(
            Endpoint::DecisionSystemOne,
            &vec![b'x'; 3 << 20],
            &Deadline::after(Duration::from_secs(20)),
        )
        .unwrap();
    // 最初のバイトが届いた時点で停止の合図が立っている
    assert!(stop.is_stopped());
    assert_eq!(
        (reply.status, reply.body.as_slice()),
        (200, b"{\"ok\":true}".as_slice())
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(server.bytes.load(Ordering::SeqCst) >= 3 << 20);
}

/// 接続プールの再利用でも、要求を書く前の確認が効く。keep-alive のスタブで 2 回続けて成功し（接続は 1 本）、
/// 3 回目は停止で、スタブの受信は増えない。
#[test]
fn a_pooled_connection_is_gated_per_request_and_still_reused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    {
        let (connections, served) = (Arc::clone(&connections), Arc::clone(&served));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                connections.fetch_add(1, Ordering::SeqCst);
                let served = Arc::clone(&served);
                std::thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            match stream.read(&mut chunk) {
                                Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                                _ => return,
                            }
                        }
                        buf.clear();
                        served.fetch_add(1, Ordering::SeqCst);
                        let body = b"{}";
                        let head =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                        if stream.write_all(head.as_bytes()).is_err()
                            || stream.write_all(body).is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
    }
    let dns = FakeDns::new();
    dns.script("lab.test", vec![vec![addr]]);
    let http = AiHttp::with_lookup(
        &format!("http://lab.test:{}", addr.port()),
        Arc::new(Arc::clone(&dns)),
    )
    .unwrap();
    let stop = StopFlag::new();
    for _ in 0..2 {
        let reply = started_session(&http, &stop)
            .get(Endpoint::Catalog, &Deadline::after(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(reply.status, 200);
    }
    assert_eq!(
        (
            connections.load(Ordering::SeqCst),
            served.load(Ordering::SeqCst)
        ),
        (1, 2)
    );
    // 3 回目: 名前解決の最中に停止
    dns.on_lookup(stopper(&stop));
    assert_eq!(
        started_session(&http, &stop)
            .get(Endpoint::Catalog, &Deadline::after(Duration::from_secs(5)))
            .unwrap_err(),
        HttpError::Stopping
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_runs_beside_a_slow_background_request_and_a_busy_slot_is_refused_at_once() {
    let release = Arc::new(Mutex::new(false));
    let gate = Arc::clone(&release);
    let server = stub(move |s, req| {
        if req.request_line.contains("/llm-decision/health") {
            // 遅い背景の要求
            while !*gate.lock().unwrap() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        respond(s, "200 OK", "", b"{}");
    });
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let stop = StopFlag::new();
    // 背景の要求が遅いまま
    let background = {
        let (http, stop) = (http.clone(), stop.clone());
        std::thread::spawn(move || {
            let s = http
                .session(Lane::Background, Duration::ZERO, &stop)
                .unwrap();
            s.get(
                Endpoint::DecisionHealth,
                &Deadline::after(Duration::from_secs(20)),
            )
            .unwrap()
            .status
        })
    };
    while server.requests.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // 対話の枠は別。背景を待たずに動く（tokio の worker は塞がない: 結果は blocking スレッドから）
    let started = Instant::now();
    let status = http
        .run_interactive(&stop, |s| {
            s.get(Endpoint::Catalog, &Deadline::interactive())
                .unwrap()
                .status
        })
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert!(started.elapsed() < Duration::from_secs(5));
    // 背景の枠も使用中なので、2 つ目の背景は取れない
    assert_eq!(
        http.session(Lane::Background, Duration::ZERO, &stop)
            .unwrap_err(),
        HttpError::Busy
    );
    // 対話の枠が使用中のあいだ、次の対話は待たずに Busy（定型 503 に当たる）。tokio の worker は塞がらない
    let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let (http, stop) = (http.clone(), stop.clone());
        tokio::spawn(async move {
            http.run_interactive(&stop, move |_session| {
                entered_tx.send(()).unwrap();
                hold_rx.recv().unwrap();
            })
            .await
        })
    };
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    let started = Instant::now();
    let busy = http.run_interactive(&stop, |_| ()).await;
    assert_eq!(busy.unwrap_err(), HttpError::Busy);
    assert!(started.elapsed() < Duration::from_millis(200));
    // 枠が使用中でも、tokio の worker 上の別の仕事は進む
    tokio::time::sleep(Duration::from_millis(20)).await;
    hold_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    // 枠は返っている
    assert!(http.run_interactive(&stop, |_| ()).await.is_ok());
    *release.lock().unwrap() = true;
    assert_eq!(background.join().unwrap(), 200);
    // 停止後の対話は、枠も blocking スレッドも使わず Stopping
    stop.stop();
    assert_eq!(
        http.run_interactive(&stop, |_| ()).await.unwrap_err(),
        HttpError::Stopping
    );
}
