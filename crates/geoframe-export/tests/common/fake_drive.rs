//! 試験用の偽の Drive の HTTP サーバー（Drive API v3 の、このサービスが使う操作とトークンの入口だけ。本物の Google には触れない）。
//!
//! 試験のプロセスの中で、127.0.0.1 の空きポートに立てる（`std` の `TcpListener`。接続ごとに 1 スレッド）。状態は偽の rclone と同じ
//! 形で、`Env::fake_dir` の中のファイルに持つ: `drive.json`（偽の Drive の模型。`fake-rclone/model.rs`）、`ctl.json`（挙動の差し替え）、
//! `calls.log`（呼ばれた操作の記録）。要求ごとにロックして読み書きするので、`Env::drive()`・`Env::save()`・`Env::add_rule()`・
//! `Hook::mutate_after`・`Env::ops()` などが、偽の rclone と同じように使える。CLI を子プロセスで動かす試験には `--drive-endpoint`
//! （[`FakeDrive::url`]）で向ける。
//!
//! ## 答える要求（Drive v3 の形で答える）
//!
//! | 要求 | 操作名（`op`） | 備考 |
//! |---|---|---|
//! | `POST /token` | `token` | `grant_type=refresh_token` と、架空の資格情報（[`FAKE_CLIENT_ID`] など）の照合 |
//! | `GET /drive/v3/files?q=…` | `list` | `q` は `'<ID>' in parents and trashed=false` だけ（ほかは 400。名前で絞る退行を捕まえる）。`pageSize`・`pageToken` |
//! | `GET /drive/v3/files/generateIds` | `generate_ids` | 払い出した ID だけが、作成に使える |
//! | `GET /drive/v3/files/<ID>` | `get` | ゴミ箱の中でも返る（`trashed: true`） |
//! | `GET /drive/v3/files/<ID>?alt=media` | `read` | ファイルの中身 |
//! | `POST /drive/v3/files` | `create_folder` | 重複した ID は 409 |
//! | `PATCH /drive/v3/files/<ID>` | `move`（`name`・`addParents`・`removeParents`）／`trash`（`{"trashed":true}`） | それ以外の本文のキー・`trashed:false` は 400 |
//! | `POST /upload/drive/v3/files?uploadType=multipart` | `upload`（`phase: multipart`） | 重複した ID は 409 |
//! | `POST /upload/drive/v3/files?uploadType=resumable` | `upload`（`phase: init`） | セッションの URL を `Location` で返す |
//! | `PUT /upload/drive/v3/files?upload_id=…` | `upload`（`phase: chunk`・`status`） | 308 と `Range`。`Content-Range: bytes */N` は位置の問い合わせ |
//!
//! **完全な削除（`DELETE`）・`emptyTrash`・中身の更新（`PATCH /upload/…`）は 405**（操作名は `unsupported`）。呼ばれたことは
//! calls.log に残るので、試験が「呼んでいない」を確かめられる。
//!
//! ## calls.log
//!
//! 1 要求 1 行（`kind: "http"`）。`op` は中立の操作名（`list`・`read`・`create_folder`・`upload`・`move`・`trash` と、読むだけの
//! `get`・`generate_ids`・`token`）。resumable の 1 回の送信は複数の要求になるので、`upload` が複数行になる（`phase` で見分ける。
//! 規則の「何回目」は、条件に合う要求を数える）。ほかに `method`・`route`・`subject`（対象のノードの Drive 上のパス。模型から引く）・
//! `q`（一覧のとき）・`has_page_token`・`authorization`（`missing`・`invalid`・`ok`。トークンの値は書かない）・`injected`（規則の効果の種類）。
//!
//! ## 故障注入（`Env::add_rule(op, skip, times, effect)`）
//!
//! `op` に当たる要求のうち、最初の `skip` 回は通常どおり、次の `times` 回に効果を掛ける。`contains` は `subject`・`phase:<名前>` の
//! 部分一致（[`FakeDrive::add_rule_contains`]）。効果は、この下の関数で作る: [`status`]・[`rate_limit_403`]・[`too_many_requests`]・
//! [`unauthorized`]・[`invalid_grant`]・[`drop_connection`]・[`do_then_drop`]・[`cut_response`]・[`redirect`]・[`repeat_page_token`]・
//! [`bad_location`]・[`cut_after`]・[`lose_session`]。`drive.rs` の [`fail`](super::drive::fail)（500）・
//! [`do_then_fail`](super::drive::do_then_fail)（済ませて 500）・[`sleep_ms`](super::drive::sleep_ms)・
//! [`garbage`](super::drive::garbage)（200 で壊れた本文）も同じ意味で使える。
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use geoframe_export::drive::auth::Credentials;
use geoframe_export::drive::http::Endpoints;
use serde_json::{Map, Value, json};

use super::drive::Env;
use super::drive::model::{self, Ctl, Drive, Kind, Node, ROOT, md5_hex};

pub const FAKE_CLIENT_ID: &str = "fake-client-id.apps.example.invalid";
pub const FAKE_CLIENT_SECRET: &str = "FAKE-CLIENT-SECRET-0001";
pub const FAKE_REFRESH_TOKEN: &str = "FAKE-REFRESH-TOKEN-0001";
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const PERSIST_UNIT: usize = 256 * 1024;
const MAX_BODY: usize = 64 << 20;

/// 架空の資格情報（偽のサーバーが受け付ける値）。
pub fn credentials() -> Credentials {
    Credentials::from_values(FAKE_CLIENT_ID, FAKE_CLIENT_SECRET, FAKE_REFRESH_TOKEN).unwrap()
}

/// 資格情報のファイル（`{dir}/credentials.json`。0600）を、架空の値で書く。
pub fn write_credentials(dir: &Path) -> PathBuf {
    write_credentials_with(dir, FAKE_REFRESH_TOKEN)
}

/// 更新トークンだけを替えた資格情報のファイル（受け付けられない値を渡して `invalid_grant` を作る）。
pub fn write_credentials_with(dir: &Path, refresh_token: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("dir");
    let path = dir.join("credentials.json");
    let body = json!({
        "client_id": FAKE_CLIENT_ID,
        "client_secret": FAKE_CLIENT_SECRET,
        "refresh_token": refresh_token,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).expect("credentials");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    path
}

// ---- 効果（`Env::add_rule` に渡す） ----

/// 操作は行わず、この状態コードで答える。
pub fn status(code: u16) -> Value {
    json!({"kind": "status", "status": code})
}

/// 流量制限の 403（`reason: rateLimitExceeded`）。操作は行わない。
pub fn rate_limit_403() -> Value {
    json!({"kind": "status", "status": 403, "reason": "rateLimitExceeded"})
}

/// 403 で、`error.errors[]` の理由が `reasons` の順に並ぶ（流量制限の理由が 2 件目以降にあっても再試行されることの確認用）。
pub fn forbidden_with_reasons(reasons: &[&str]) -> Value {
    json!({"kind": "status", "status": 403, "reasons": reasons})
}

/// 状態コードと本文（そのまま）で答える。操作は行わない。
pub fn status_with_body(code: u16, body: &str) -> Value {
    json!({"kind": "status", "status": code, "raw_body": body})
}

/// 429 と `Retry-After`（秒）。操作は行わない。
pub fn too_many_requests(retry_after_secs: u64) -> Value {
    json!({"kind": "status", "status": 429, "reason": "rateLimitExceeded", "retry_after": retry_after_secs})
}

/// 401（トークンが無効）。操作は行わない。
pub fn unauthorized() -> Value {
    json!({"kind": "status", "status": 401, "reason": "authError"})
}

/// トークンの入口の `400 {"error":"invalid_grant"}`。
pub fn invalid_grant() -> Value {
    json!({"kind": "status", "status": 400, "oauth_error": "invalid_grant"})
}

/// 操作は行わず、応答を返さずに接続を切る。
pub fn drop_connection() -> Value {
    json!({"kind": "drop"})
}

/// 操作を済ませてから、応答を返さずに接続を切る（応答を失った）。
pub fn do_then_drop() -> Value {
    json!({"kind": "do_then_drop"})
}

/// 操作を済ませ、本文の半分まで送って接続を切る（読み取りの途中の切断）。
pub fn cut_response() -> Value {
    json!({"kind": "cut_response"})
}

/// 操作は行わず、3xx で別の場所へ向ける。
pub fn redirect(code: u16, location: &str) -> Value {
    json!({"kind": "redirect", "status": code, "location": location})
}

/// 一覧が、いつも同じ `nextPageToken` を返す。
pub fn repeat_page_token() -> Value {
    json!({"kind": "repeat_token"})
}

/// resumable のセッションの `Location` を、別のところへ向ける（セッションは作る）。
pub fn bad_location(location: &str) -> Value {
    json!({"kind": "bad_location", "location": location})
}

/// チャンクの `bytes` バイトまで受けたところで接続を切る（受け取り済みとして残るのは、256KiB の倍数まで）。
pub fn cut_after(bytes: usize) -> Value {
    json!({"kind": "cut_after", "bytes": bytes})
}

/// セッションを捨てて 404 を返す（セッションの喪失）。
pub fn lose_session() -> Value {
    json!({"kind": "lose_session"})
}

// ---- サーバー ----

struct Session {
    id: String,
    parent: String,
    name: String,
    total: usize,
    received: Vec<u8>,
    done: Option<String>,
}

struct Issued {
    value: String,
    expires_at: Instant,
    revoked: bool,
}

struct Inner {
    sessions: HashMap<String, Session>,
    session_seq: u64,
    tokens: Vec<Issued>,
    token_seq: u64,
    lifetime_secs: u64,
}

struct Shared {
    dir: PathBuf,
    addr: SocketAddr,
    inner: Mutex<Inner>,
    stop: AtomicBool,
    connections: AtomicUsize,
}

pub struct FakeDrive {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeDrive {
    /// `env` の偽の Drive（`env.fake_dir`）を、HTTP で見せる。
    pub fn start(env: &Env) -> Self {
        Self::start_in(&env.fake_dir)
    }

    pub fn start_in(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).expect("fake dir");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Shared {
            dir: dir.to_owned(),
            addr,
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                session_seq: 0,
                tokens: Vec::new(),
                token_seq: 0,
                lifetime_secs: 3599,
            }),
            stop: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
        });
        let s = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                s.connections.fetch_add(1, Ordering::SeqCst);
                let s2 = Arc::clone(&s);
                std::thread::spawn(move || serve_connection(&s2, stream));
            }
        });
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// `http://127.0.0.1:<port>`（`--drive-endpoint` に渡す形）。
    pub fn url(&self) -> String {
        format!("http://{}", self.shared.addr)
    }

    pub fn endpoints(&self) -> Endpoints {
        Endpoints::loopback(&self.url()).expect("loopback")
    }

    /// 受けた接続の数。
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// 払い出したアクセストークンを、全部無効にする（次の要求は 401 になる）。
    pub fn revoke_tokens(&self) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        for t in &mut inner.tokens {
            t.revoked = true;
        }
    }

    /// 次から払い出すトークンの `expires_in`（秒）。
    pub fn set_token_lifetime(&self, secs: u64) {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lifetime_secs = secs;
    }

    /// 払い出したトークンの数。
    pub fn tokens_issued(&self) -> usize {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tokens
            .len()
    }

    /// 条件（`subject` か `phase:<名前>` の部分一致）つきの規則を足す。
    pub fn add_rule_contains(
        &self,
        op: &str,
        contains: &str,
        skip: u64,
        times: u64,
        effect: Value,
    ) {
        let mut c = Ctl::load(&self.shared.dir);
        c.rules.push(model::Rule {
            op: op.to_owned(),
            contains: Some(contains.to_owned()),
            skip,
            times,
            seen: 0,
            effect,
        });
        c.save(&self.shared.dir);
    }

    /// 払い出した ID の中身（ファイルの本体）。アップロードしたファイルは、ここに置く。
    pub fn blob(&self, id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.shared.dir.join("blobs").join(id)).ok()
    }

    /// calls.log の行のうち、HTTP の要求だけ（順番どおり）。
    pub fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.shared.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["kind"] == "http")
            .collect()
    }
}

impl Drop for FakeDrive {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // accept を起こす
        let _ = TcpStream::connect_timeout(&self.shared.addr, Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 接続を受けて数えるだけの待ち受け（別のホストを指す `Location` に、接続が来ないことの確認用）。
pub struct Sink {
    pub addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sink {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (Arc::clone(&connections), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                if stream.is_ok() {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        Self {
            addr,
            connections,
            stop,
            thread: Some(thread),
        }
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---- HTTP の読み書き ----

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn q(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn pct_decode(s: &str, plus_is_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_pairs(s: &str, plus_is_space: bool) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (pct_decode(k, plus_is_space), pct_decode(v, plus_is_space))
        })
        .collect()
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16384];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n", 0) {
            break i;
        }
        if buf.len() > 65536 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let start = lines.next()?;
    let mut parts = start.split(' ');
    let method = parts.next()?.to_owned();
    let target = parts.next()?;
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), parse_pairs(q, false)),
        None => (target.to_owned(), Vec::new()),
    };
    let want: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if want > MAX_BODY {
        return None;
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < want {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            // 本文の途中で切れた（クライアントが切った）
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(want);
    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, v: &Value) -> Self {
        Self {
            status,
            headers: vec![(
                "Content-Type".into(),
                "application/json; charset=UTF-8".into(),
            )],
            body: serde_json::to_vec(v).unwrap(),
        }
    }

    fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/octet-stream".into())],
            body,
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }

    /// Drive のエラー応答の形（`error.errors[].reason`）。
    fn error(status: u16, reason: &str) -> Self {
        Self::json(
            status,
            &json!({"error": {"code": status, "message": "fake drive error",
                "errors": [{"message": "fake drive error", "domain": "global", "reason": reason}]}}),
        )
    }

    fn head(&self, declared_len: usize) -> String {
        let reason = match self.status {
            200 => "OK",
            201 => "Created",
            308 => "Resume Incomplete",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "X",
        };
        let mut h = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (k, v) in &self.headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        h.push_str(&format!(
            "Content-Length: {declared_len}\r\nConnection: close\r\n\r\n"
        ));
        h
    }
}

enum Outcome {
    Respond(Response),
    /// 応答を返さずに接続を切る。
    Drop,
    /// 本文の半分まで送って切る。
    Cut(Response),
}

fn serve_connection(shared: &Shared, mut stream: TcpStream) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    // 第 1 段（ロックを持つ）: 分類・記録・効果の選択
    let decision = decide(shared, &req);
    if decision.effect_kind() == "sleep" {
        let ms = decision
            .effect
            .as_ref()
            .and_then(|e| e["ms"].as_u64())
            .unwrap_or(0);
        std::thread::sleep(Duration::from_millis(ms));
    }
    // 第 2 段（ロックを持つ）: 操作と応答
    let outcome = execute(shared, &req, decision);
    match outcome {
        Outcome::Respond(r) => {
            let _ = stream.write_all(r.head(r.body.len()).as_bytes());
            let _ = stream.write_all(&r.body);
            let _ = stream.flush();
        }
        Outcome::Cut(r) => {
            let _ = stream.write_all(r.head(r.body.len()).as_bytes());
            let _ = stream.write_all(&r.body[..r.body.len() / 2]);
            let _ = stream.flush();
        }
        Outcome::Drop => {}
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

// ---- 分類と効果の選択 ----

struct Decision {
    op: &'static str,
    route: &'static str,
    effect: Option<Value>,
}

impl Decision {
    fn effect_kind(&self) -> &str {
        self.effect
            .as_ref()
            .and_then(|e| e["kind"].as_str())
            .unwrap_or("")
    }
}

fn lock_inner(shared: &Shared) -> std::sync::MutexGuard<'_, Inner> {
    shared.inner.lock().unwrap_or_else(|e| e.into_inner())
}

fn mime_of(n: &Node) -> &'static str {
    match n.kind {
        Kind::Dir => FOLDER_MIME,
        Kind::Shortcut => model::SHORTCUT_MIME,
        Kind::Gdoc => model::GDOC_MIME,
        Kind::Form => model::FORM_MIME,
        Kind::File if n.name.ends_with(".jpg") => "image/jpeg",
        Kind::File if n.name.ends_with(".json") => "application/json",
        Kind::File => "application/octet-stream",
    }
}

/// 経路から、操作名・経路の名前・対象（Drive 上のパス）を決める。
fn classify(
    req: &Request,
    drive: &Drive,
    inner: &Inner,
) -> (&'static str, &'static str, String, Option<&'static str>) {
    let path_of = |id: &str| drive.path_of(id).unwrap_or_default();
    let rest = req.path.strip_prefix("/drive/v3/files");
    let upload = req.path.strip_prefix("/upload/drive/v3/files");
    match (req.method.as_str(), rest, upload) {
        ("POST", None, None) if req.path == "/token" => ("token", "token", String::new(), None),
        ("GET", Some(""), _) => {
            let parent = req
                .q("q")
                .and_then(parse_list_q)
                .map(|p| path_of_or_root(drive, &p))
                .unwrap_or_default();
            ("list", "files.list", parent, None)
        }
        ("GET", Some("/generateIds"), _) => {
            ("generate_ids", "files.generateIds", String::new(), None)
        }
        ("GET", Some(r), _) if r.starts_with('/') => {
            let id = &r[1..];
            if req.q("alt") == Some("media") {
                ("read", "files.get.media", path_of(id), None)
            } else {
                ("get", "files.get", path_of(id), None)
            }
        }
        ("POST", Some(""), _) => {
            let body = req.json().unwrap_or(Value::Null);
            let parent = body["parents"][0].as_str().unwrap_or_default();
            let name = body["name"].as_str().unwrap_or_default();
            let subject = format!("{}/{name}", path_of_or_root(drive, parent));
            if body["mimeType"] == FOLDER_MIME {
                ("create_folder", "files.create", subject, None)
            } else {
                ("unsupported", "files.create", subject, None)
            }
        }
        ("PATCH", Some(r), _) if r.starts_with('/') => {
            let id = &r[1..];
            let body = req.json().unwrap_or(Value::Null);
            let op = if body.get("trashed").is_some() {
                "trash"
            } else {
                "move"
            };
            (op, "files.update", path_of(id), None)
        }
        ("POST", _, Some("")) => {
            let body = req.json();
            // multipart の本文は JSON ではない。resumable の開始は JSON
            let (parent, name, phase) = match req.q("uploadType") {
                Some("resumable") => {
                    let b = body.unwrap_or(Value::Null);
                    (
                        b["parents"][0].as_str().unwrap_or_default().to_owned(),
                        b["name"].as_str().unwrap_or_default().to_owned(),
                        "init",
                    )
                }
                _ => match parse_multipart(req) {
                    Some((m, _)) => (
                        m["parents"][0].as_str().unwrap_or_default().to_owned(),
                        m["name"].as_str().unwrap_or_default().to_owned(),
                        "multipart",
                    ),
                    None => (String::new(), String::new(), "multipart"),
                },
            };
            let subject = format!("{}/{name}", path_of_or_root(drive, &parent));
            ("upload", "upload.create", subject, Some(phase))
        }
        ("PUT", _, Some("")) => {
            let sess = req.q("upload_id").and_then(|u| inner.sessions.get(u));
            let subject = sess
                .map(|s| format!("{}/{}", path_of_or_root(drive, &s.parent), s.name))
                .unwrap_or_default();
            let phase = if req
                .header("content-range")
                .is_some_and(|r| r.starts_with("bytes */"))
            {
                "status"
            } else {
                "chunk"
            };
            ("upload", "upload.session", subject, Some(phase))
        }
        _ => ("unsupported", "unsupported", String::new(), None),
    }
}

fn path_of_or_root(drive: &Drive, id: &str) -> String {
    if id == ROOT {
        String::new()
    } else {
        drive.path_of(id).unwrap_or_default()
    }
}

/// `'<ID>' in parents and trashed=false` だけを受け付ける。
fn parse_list_q(q: &str) -> Option<String> {
    let rest = q.strip_prefix('\'')?;
    let (id, tail) = rest.split_once('\'')?;
    (tail == " in parents and trashed=false" && !id.is_empty() && !id.contains('\\'))
        .then(|| id.to_owned())
}

fn decide(shared: &Shared, req: &Request) -> Decision {
    let inner = lock_inner(shared);
    let drive = Drive::load(&shared.dir);
    let (op, route, subject, phase) = classify(req, &drive, &inner);
    let mut ctl = Ctl::load(&shared.dir);
    let mut effect: Option<Value> = None;
    let haystack = match phase {
        Some(p) => format!("{subject} phase:{p}"),
        None => subject.clone(),
    };
    for rule in &mut ctl.rules {
        let hit = rule.op == op
            && rule
                .contains
                .as_ref()
                .is_none_or(|c| haystack.contains(c.as_str()));
        if !hit {
            continue;
        }
        rule.seen += 1;
        if effect.is_none()
            && rule.seen > rule.skip
            && rule.seen <= rule.skip.saturating_add(rule.times)
        {
            effect = Some(rule.effect.clone());
        }
    }
    ctl.save(&shared.dir);

    let auth = match bearer(req) {
        None => "missing",
        Some(t) => {
            if token_valid(&inner, t) {
                "ok"
            } else {
                "invalid"
            }
        }
    };
    let mut line = json!({
        "kind": "http", "op": op, "method": req.method, "route": route, "subject": subject,
        "authorization": auth,
        "injected": effect.as_ref().map(|e| e["kind"].clone()),
    });
    if let Some(p) = phase {
        line["phase"] = json!(p);
    }
    if op == "list" {
        line["q"] = json!(req.q("q"));
        line["has_page_token"] = json!(req.q("pageToken").is_some());
        line["page_size"] = json!(req.q("pageSize"));
    }
    if matches!(
        op,
        "move" | "trash" | "get" | "read" | "create_folder" | "upload" | "list"
    ) {
        line["fields"] = json!(req.q("fields"));
    }
    // 作る ID（本文のメタデータの `id`。作成の再送が、同じ ID で送られていることを確かめる）
    let new_id = match (req.q("uploadType"), op) {
        (Some("multipart"), _) => {
            parse_multipart(req).and_then(|(m, _)| m["id"].as_str().map(str::to_owned))
        }
        (_, "create_folder" | "upload") => {
            req.json().and_then(|b| b["id"].as_str().map(str::to_owned))
        }
        _ => None,
    };
    if let Some(id) = new_id {
        line["new_id"] = json!(id);
    }
    if let Some(cr) = req.header("content-range") {
        line["content_range"] = json!(cr);
    }
    if req.method != "GET" && req.method != "POST" && req.method != "PATCH" && req.method != "PUT" {
        line["method_unsupported"] = json!(true);
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(shared.dir.join("calls.log"))
        .expect("calls.log");
    writeln!(log, "{line}").expect("log");
    Decision { op, route, effect }
}

fn bearer(req: &Request) -> Option<&str> {
    req.header("authorization")?.strip_prefix("Bearer ")
}

fn token_valid(inner: &Inner, token: &str) -> bool {
    inner
        .tokens
        .iter()
        .any(|t| t.value == token && !t.revoked && Instant::now() < t.expires_at)
}

// ---- 実行 ----

fn execute(shared: &Shared, req: &Request, decision: Decision) -> Outcome {
    let kind = decision.effect_kind().to_owned();
    let effect = decision.effect.clone().unwrap_or(Value::Null);
    let before = |kind: &str| match kind {
        "status" => {
            let code = effect["status"].as_u64().unwrap_or(500) as u16;
            let mut r = if let Some(oauth) = effect["oauth_error"].as_str() {
                Response::json(code, &json!({"error": oauth, "error_description": "fake"}))
            } else if let Some(raw) = effect["raw_body"].as_str() {
                let mut r = Response::bytes(code, raw.as_bytes().to_vec());
                r.headers = vec![("Content-Type".into(), "application/json".into())];
                r
            } else if let Some(reasons) = effect["reasons"].as_array() {
                let errors: Vec<Value> = reasons
                    .iter()
                    .map(
                        |r| json!({"message": "fake drive error", "domain": "global", "reason": r}),
                    )
                    .collect();
                Response::json(
                    code,
                    &json!({"error": {"code": code, "message": "fake drive error", "errors": errors}}),
                )
            } else {
                Response::error(code, effect["reason"].as_str().unwrap_or("backendError"))
            };
            if let Some(s) = effect["retry_after"].as_u64() {
                r = r.header("Retry-After", &s.to_string());
            }
            Some(Outcome::Respond(r))
        }
        "fail" => Some(Outcome::Respond(Response::error(500, "backendError"))),
        "drop" => Some(Outcome::Drop),
        "garbage" => Some(Outcome::Respond(Response::bytes(
            200,
            effect["stdout"].as_str().unwrap_or("").as_bytes().to_vec(),
        ))),
        "redirect" => {
            let code = effect["status"].as_u64().unwrap_or(302) as u16;
            Some(Outcome::Respond(Response::empty(code).header(
                "Location",
                effect["location"].as_str().unwrap_or("/"),
            )))
        }
        _ => None,
    };
    if let Some(o) = before(&kind) {
        return o;
    }

    let mut inner = lock_inner(shared);
    // トークンの入口と、セッションの URL（`upload_id` が認可する）以外は、Authorization を確かめる
    let needs_auth = !(decision.op == "token" || decision.route == "upload.session");
    if needs_auth && !bearer(req).is_some_and(|t| token_valid(&inner, t)) {
        return Outcome::Respond(Response::error(401, "authError"));
    }
    let mut drive = Drive::load(&shared.dir);
    let mut response = match decision.route {
        "token" => handle_token(shared, &mut inner, req),
        "files.list" => handle_list(&drive, req, &kind),
        "files.generateIds" => handle_generate_ids(shared, &mut drive, req),
        "files.get" | "files.get.media" => {
            handle_get(shared, &drive, req, decision.route == "files.get.media")
        }
        "files.create" => handle_create_folder(shared, &mut drive, req),
        "files.update" => handle_update(shared, &mut drive, req),
        "upload.create" => {
            handle_upload_create(shared, &mut inner, &mut drive, req, &effect, &kind)
        }
        "upload.session" => handle_session(shared, &mut inner, &mut drive, req, &effect, &kind),
        _ => {
            // DELETE・emptyTrash・中身の更新・知らない経路
            if req.path.starts_with("/upload/")
                || req.method == "DELETE"
                || req.path.contains("emptyTrash")
            {
                Response::error(405, "methodNotAllowed")
            } else {
                Response::error(404, "notFound")
            }
        }
    };
    // 模型への書き戻しは、各ハンドラが `save` する
    let _ = &mut drive;
    match kind.as_str() {
        "do_then_drop" | "cut_after" => Outcome::Drop,
        "do_then_fail" => Outcome::Respond(Response::error(500, "backendError")),
        "cut_response" => Outcome::Cut(std::mem::replace(&mut response, Response::empty(200))),
        _ => Outcome::Respond(response),
    }
}

fn fields_of(req: &Request, in_files: bool) -> Option<HashSet<String>> {
    let f = req.q("fields")?;
    let inner = if in_files {
        let start = f.find("files(")? + "files(".len();
        let end = f[start..].find(')')? + start;
        &f[start..end]
    } else {
        f
    };
    Some(
        inner
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// Drive の `files` リソースの形（`int64` の `size` は文字列）。
fn item_json(n: &Node, drive: &Drive, fields: &Option<HashSet<String>>) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), json!("drive#file"));
    m.insert("id".into(), json!(n.id));
    m.insert("name".into(), json!(n.name));
    m.insert("mimeType".into(), json!(mime_of(n)));
    m.insert("trashed".into(), json!(drive.is_trashed(&n.id)));
    m.insert("parents".into(), json!([n.parent]));
    if n.kind == Kind::File {
        m.insert("size".into(), json!(n.size.to_string()));
        m.insert("md5Checksum".into(), json!(n.effective_md5()));
    }
    if let Some(f) = fields {
        m.retain(|k, _| f.contains(k));
    }
    Value::Object(m)
}

fn handle_token(shared: &Shared, inner: &mut Inner, req: &Request) -> Response {
    let _ = shared;
    let form = parse_pairs(&String::from_utf8_lossy(&req.body), true);
    let get = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("grant_type") != Some("refresh_token") {
        return Response::json(400, &json!({"error": "unsupported_grant_type"}));
    }
    if get("client_id") != Some(FAKE_CLIENT_ID) || get("client_secret") != Some(FAKE_CLIENT_SECRET)
    {
        return Response::json(
            401,
            &json!({"error": "invalid_client", "error_description": "fake"}),
        );
    }
    if get("refresh_token") != Some(FAKE_REFRESH_TOKEN) {
        return Response::json(
            400,
            &json!({"error": "invalid_grant", "error_description": "fake"}),
        );
    }
    inner.token_seq += 1;
    let value = format!("fake-access-token-{:04}", inner.token_seq);
    inner.tokens.push(Issued {
        value: value.clone(),
        expires_at: Instant::now() + Duration::from_secs(inner.lifetime_secs),
        revoked: false,
    });
    Response::json(
        200,
        &json!({"access_token": value, "expires_in": inner.lifetime_secs, "token_type": "Bearer"}),
    )
}

fn handle_list(drive: &Drive, req: &Request, kind: &str) -> Response {
    let Some(parent) = req.q("q").and_then(parse_list_q) else {
        // 名前で絞る `q` など、このサービスが送らない形
        return Response::error(400, "invalidQuery");
    };
    if parent != ROOT && drive.node(&parent).is_none() {
        return Response::error(404, "notFound");
    }
    let page_size: usize = req
        .q("pageSize")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let offset: usize = match req.q("pageToken") {
        None => 0,
        Some(t) => match t.strip_prefix("pt").and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None if t == "stuck" => 0,
            None => return Response::error(400, "invalidPageToken"),
        },
    };
    let all: Vec<&Node> = drive
        .children(&parent)
        .into_iter()
        .filter(|n| !drive.is_trashed(&n.id))
        .collect();
    let fields = fields_of(req, true);
    let page: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(page_size)
        .map(|n| item_json(n, drive, &fields))
        .collect();
    let mut body = json!({"kind": "drive#fileList", "files": page});
    if offset + page_size < all.len() {
        body["nextPageToken"] = json!(format!("pt{}", offset + page_size));
    }
    if kind == "repeat_token" {
        body["nextPageToken"] = json!("stuck");
    }
    Response::json(200, &body)
}

fn handle_generate_ids(shared: &Shared, drive: &mut Drive, req: &Request) -> Response {
    let count: usize = req.q("count").and_then(|v| v.parse().ok()).unwrap_or(10);
    if !(1..=1000).contains(&count) || req.q("space") != Some("drive") {
        return Response::error(400, "invalid");
    }
    let ids: Vec<String> = (0..count).map(|_| drive.new_id()).collect();
    drive.save(&shared.dir);
    Response::json(
        200,
        &json!({"kind": "drive#generatedIds", "space": "drive", "ids": ids}),
    )
}

fn handle_get(shared: &Shared, drive: &Drive, req: &Request, media: bool) -> Response {
    let id = req.path.rsplit('/').next().unwrap_or_default();
    let Some(n) = drive.node(id) else {
        return Response::error(404, "notFound");
    };
    if media {
        if n.kind != Kind::File {
            return Response::error(403, "fileNotDownloadable");
        }
        let body = std::fs::read(shared.dir.join("blobs").join(&n.id)).unwrap_or_else(|_| {
            n.content
                .clone()
                .unwrap_or_else(|| "x".repeat(n.size as usize))
                .into_bytes()
        });
        return Response::bytes(200, body);
    }
    Response::json(200, &item_json(n, drive, &fields_of(req, false)))
}

/// `generateIds` で払い出した ID か（`fid` と 4 桁以上の数字で、`next` 以下）。
fn issued(drive: &Drive, id: &str) -> bool {
    id.strip_prefix("fid")
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|n| n >= 1 && n <= drive.next)
}

fn new_node(id: &str, parent: &str, name: &str, kind: Kind, size: u64) -> Node {
    Node {
        id: id.to_owned(),
        parent: parent.to_owned(),
        name: name.to_owned(),
        kind,
        size,
        content: None,
        md5: None,
        target: None,
        trashed: false,
    }
}

fn parent_ok(drive: &Drive, parent: &str) -> bool {
    parent == ROOT || drive.node(parent).is_some_and(|n| n.kind == Kind::Dir)
}

fn handle_create_folder(shared: &Shared, drive: &mut Drive, req: &Request) -> Response {
    let Some(b) = req.json() else {
        return Response::error(400, "badRequest");
    };
    let (Some(id), Some(name), Some(parent)) = (
        b["id"].as_str(),
        b["name"].as_str(),
        b["parents"][0].as_str(),
    ) else {
        return Response::error(400, "badRequest");
    };
    if b["mimeType"] != FOLDER_MIME || b["parents"].as_array().map(Vec::len) != Some(1) {
        return Response::error(400, "badRequest");
    }
    if !issued(drive, id) {
        return Response::error(400, "invalidId");
    }
    if drive.node(id).is_some() {
        return Response::error(409, "duplicate");
    }
    if !parent_ok(drive, parent) {
        return Response::error(404, "notFound");
    }
    drive.push_node(new_node(id, parent, name, Kind::Dir, 0));
    drive.save(&shared.dir);
    let n = drive.node(id).unwrap();
    Response::json(200, &item_json(n, drive, &fields_of(req, false)))
}

fn handle_update(shared: &Shared, drive: &mut Drive, req: &Request) -> Response {
    let id = req.path.rsplit('/').next().unwrap_or_default().to_owned();
    if drive.node(&id).is_none() {
        return Response::error(404, "notFound");
    }
    let Some(Value::Object(b)) = req.json() else {
        return Response::error(400, "badRequest");
    };
    if b.keys().any(|k| k != "name" && k != "trashed") {
        // 中身・メタデータの更新は、このサービスは送らない
        return Response::error(400, "badRequest");
    }
    if let Some(t) = b.get("trashed") {
        if *t != json!(true) || b.len() != 1 {
            return Response::error(400, "badRequest");
        }
        if let Some(n) = drive.node_mut_pub(&id) {
            n.trashed = true;
        }
        drive.trashed.push(id.clone());
    } else {
        let add = req.q("addParents");
        let remove = req.q("removeParents");
        if let Some(remove) = remove {
            // 親でないものを外す要求は、通さない（再送の取り違えを捕まえる）
            if drive.node(&id).is_some_and(|n| n.parent != remove) {
                return Response::error(404, "notFound");
            }
        }
        if let Some(add) = add
            && !parent_ok(drive, add)
        {
            return Response::error(404, "notFound");
        }
        if let Some(n) = drive.node_mut_pub(&id) {
            if let Some(add) = add {
                n.parent = add.to_owned();
            }
            if let Some(name) = b.get("name").and_then(Value::as_str) {
                n.name = name.to_owned();
            }
        }
    }
    drive.save(&shared.dir);
    Response::json(
        200,
        &item_json(drive.node(&id).unwrap(), drive, &fields_of(req, false)),
    )
}

/// multipart/related の本文を、メタデータ（JSON）と中身に分ける。
fn parse_multipart(req: &Request) -> Option<(Value, Vec<u8>)> {
    let ct = req.header("content-type")?;
    let boundary = ct
        .split("boundary=")
        .nth(1)?
        .trim()
        .trim_matches('"')
        .to_owned();
    let delim = format!("--{boundary}").into_bytes();
    let body = &req.body;
    let mut pos = find(body, &delim, 0)? + delim.len();
    let mut parts: Vec<Vec<u8>> = Vec::new();
    loop {
        if body[pos..].starts_with(b"--") {
            break;
        }
        // 見出し
        let head_end = find(body, b"\r\n\r\n", pos)?;
        let start = head_end + 4;
        let end = find(body, &[b"\r\n".as_slice(), &delim].concat(), start)?;
        parts.push(body[start..end].to_vec());
        pos = end + 2 + delim.len();
    }
    if parts.len() != 2 {
        return None;
    }
    let meta: Value = serde_json::from_slice(&parts[0]).ok()?;
    Some((meta, parts.remove(1)))
}

fn finish_file(
    shared: &Shared,
    drive: &mut Drive,
    req: &Request,
    id: &str,
    parent: &str,
    name: &str,
    content: &[u8],
) -> Response {
    if drive.node(id).is_some() {
        return Response::error(409, "duplicate");
    }
    let mut n = new_node(id, parent, name, Kind::File, content.len() as u64);
    n.md5 = Some(md5_hex(content));
    if content.len() <= 64 * 1024 {
        n.content = String::from_utf8(content.to_vec()).ok();
    }
    drive.push_node(n);
    drive.save(&shared.dir);
    let blobs = shared.dir.join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs");
    std::fs::write(blobs.join(id), content).expect("blob");
    Response::json(
        200,
        &item_json(drive.node(id).unwrap(), drive, &fields_of(req, false)),
    )
}

fn handle_upload_create(
    shared: &Shared,
    inner: &mut Inner,
    drive: &mut Drive,
    req: &Request,
    effect: &Value,
    kind: &str,
) -> Response {
    match req.q("uploadType") {
        Some("multipart") => {
            let Some((meta, content)) = parse_multipart(req) else {
                return Response::error(400, "badRequest");
            };
            let (Some(id), Some(name), Some(parent)) = (
                meta["id"].as_str(),
                meta["name"].as_str(),
                meta["parents"][0].as_str(),
            ) else {
                return Response::error(400, "badRequest");
            };
            if !issued(drive, id) {
                return Response::error(400, "invalidId");
            }
            if !parent_ok(drive, parent) {
                return Response::error(404, "notFound");
            }
            finish_file(shared, drive, req, id, parent, name, &content)
        }
        Some("resumable") => {
            let Some(meta) = req.json() else {
                return Response::error(400, "badRequest");
            };
            let (Some(id), Some(name), Some(parent)) = (
                meta["id"].as_str(),
                meta["name"].as_str(),
                meta["parents"][0].as_str(),
            ) else {
                return Response::error(400, "badRequest");
            };
            let Some(total) = req
                .header("x-upload-content-length")
                .and_then(|v| v.parse::<usize>().ok())
            else {
                return Response::error(400, "badRequest");
            };
            if !issued(drive, id) {
                return Response::error(400, "invalidId");
            }
            if !parent_ok(drive, parent) {
                return Response::error(404, "notFound");
            }
            if drive.node(id).is_some() {
                return Response::error(409, "duplicate");
            }
            inner.session_seq += 1;
            let sid = format!("sess-{:04}", inner.session_seq);
            inner.sessions.insert(
                sid.clone(),
                Session {
                    id: id.to_owned(),
                    parent: parent.to_owned(),
                    name: name.to_owned(),
                    total,
                    received: Vec::new(),
                    done: None,
                },
            );
            let location = if kind == "bad_location" {
                effect["location"].as_str().unwrap_or_default().to_owned()
            } else {
                format!(
                    "http://{}/upload/drive/v3/files?uploadType=resumable&upload_id={sid}",
                    shared.addr
                )
            };
            Response::empty(200).header("Location", &location)
        }
        // 既存のファイルの中身を更新する呼び出し（`PATCH`）は、別の経路で 405 になる
        _ => Response::error(400, "badRequest"),
    }
}

/// `bytes a-b/total` を `(a, b, total)` に、`bytes */total` を `None` と total にする。
fn parse_content_range(v: &str) -> Option<(Option<(usize, usize)>, usize)> {
    let rest = v.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let total = total.parse().ok()?;
    if range == "*" {
        return Some((None, total));
    }
    let (a, b) = range.split_once('-')?;
    Some((Some((a.parse().ok()?, b.parse().ok()?)), total))
}

fn resume_incomplete(received: usize) -> Response {
    let r = Response::empty(308);
    if received == 0 {
        r
    } else {
        r.header("Range", &format!("bytes=0-{}", received - 1))
    }
}

fn handle_session(
    shared: &Shared,
    inner: &mut Inner,
    drive: &mut Drive,
    req: &Request,
    effect: &Value,
    kind: &str,
) -> Response {
    let Some(sid) = req.q("upload_id").map(str::to_owned) else {
        return Response::error(404, "notFound");
    };
    if kind == "lose_session" {
        inner.sessions.remove(&sid);
        return Response::error(404, "notFound");
    }
    let Some(sess) = inner.sessions.get_mut(&sid) else {
        return Response::error(404, "notFound");
    };
    let Some((range, total)) = req.header("content-range").and_then(parse_content_range) else {
        return Response::error(400, "badRequest");
    };
    if total != sess.total {
        return Response::error(400, "badRequest");
    }
    if let Some(done) = &sess.done {
        // 済んだセッションへの問い合わせ（位置の問い合わせ・再送）には、できたファイルを返す
        let n = drive.node(done).unwrap();
        return Response::json(200, &item_json(n, drive, &fields_of(req, false)));
    }
    let Some((a, b)) = range else {
        return resume_incomplete(sess.received.len());
    };
    if a != sess.received.len() || b + 1 < a || b + 1 - a != req.body.len() || b + 1 > sess.total {
        return Response::error(400, "badRequest");
    }
    if kind == "cut_after" {
        let n = effect["bytes"].as_u64().unwrap_or(0) as usize;
        let keep = (n.min(req.body.len()) / PERSIST_UNIT) * PERSIST_UNIT;
        sess.received.extend_from_slice(&req.body[..keep]);
        // 接続を切る（`execute` が、この効果のときは応答を捨てて切る）
        return Response::error(500, "backendError");
    }
    sess.received.extend_from_slice(&req.body);
    if sess.received.len() < sess.total {
        return resume_incomplete(sess.received.len());
    }
    let (id, parent, name, content) = (
        sess.id.clone(),
        sess.parent.clone(),
        sess.name.clone(),
        std::mem::take(&mut sess.received),
    );
    let response = finish_file(shared, drive, req, &id, &parent, &name, &content);
    if response.status == 200
        && let Some(s) = inner.sessions.get_mut(&sid)
    {
        s.done = Some(id);
    }
    response
}
