//! 試験用の Drive のスタブ。ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）で、
//! トークンの交換（JWT の署名・claims を検証する）・`files.list`（ページ送り）・`files.get?alt=media` を
//! 返す。応答はすべて合成。故障の注入（状態コード・`Retry-After`）と、受けた要求の記録ができる。
//!
//! 応答は 1 要求ごとに `Connection: close`（クライアントの接続の使い回しを考えずに済む）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

/// 受けた要求（記録用）。`Authorization` の値は記録しない（`auth_ok` だけ）。
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: String,
    /// `Authorization: Bearer` が、発行したトークンと一致した。
    pub auth_ok: bool,
}

impl Request {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_media(&self) -> bool {
        self.param("alt") == Some("media")
    }
}

/// 故障の注入: 要求を見て、`Some((状態コード, 本文, 追加ヘッダ))` なら本来の応答の代わりに返す。
pub type Fault = Box<dyn FnMut(&Request) -> Option<(u16, String, Vec<(String, String)>)> + Send>;

struct State {
    public_key: Vec<u8>,
    email: String,
    token_url: String,
    folder_name: String,
    folder_id: String,
    items: Vec<Value>,
    page_size: usize,
    media: HashMap<String, Vec<u8>>,
    requests: Vec<Request>,
    tokens_issued: u32,
    /// このトークンの番号以下は失効した（401 を返す）。
    revoked_through: u32,
    expires_in: u64,
    faults: Vec<Fault>,
}

pub struct StubDrive {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StubDrive {
    /// `public_key` は署名の検証に使う公開鍵（PKCS#1 の DER）、`email` は assertion の `iss`。
    pub fn start(public_key: Vec<u8>, email: &str, folder_name: &str, folder_id: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            public_key,
            email: email.to_string(),
            token_url: format!("http://{addr}/token"),
            folder_name: folder_name.to_string(),
            folder_id: folder_id.to_string(),
            items: Vec::new(),
            page_size: 2,
            media: HashMap::new(),
            requests: Vec::new(),
            tokens_issued: 0,
            revoked_through: 0,
            expires_in: 3599,
            faults: Vec::new(),
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
                        // 1 つの接続の失敗でサーバーを止めない
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

    pub fn set_items(&self, items: Vec<Value>) {
        self.state().items = items;
    }

    pub fn set_media(&self, id: &str, bytes: Vec<u8>) {
        self.state().media.insert(id.to_string(), bytes);
    }

    pub fn set_page_size(&self, size: usize) {
        self.state().page_size = size;
    }

    pub fn set_expires_in(&self, seconds: u64) {
        self.state().expires_in = seconds;
    }

    pub fn add_fault(&self, fault: Fault) {
        self.state().faults.push(fault);
    }

    pub fn clear_faults(&self) {
        self.state().faults.clear();
    }

    /// 発行済みのトークンをすべて失効させる（次の API 要求は 401）。
    pub fn revoke_tokens(&self) {
        let mut state = self.state();
        state.revoked_through = state.tokens_issued;
    }

    pub fn tokens_issued(&self) -> u32 {
        self.state().tokens_issued
    }

    pub fn requests(&self) -> Vec<Request> {
        self.state().requests.clone()
    }

    pub fn clear_requests(&self) {
        self.state().requests.clear();
    }

    pub fn list_requests(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| {
                r.path == "/drive/v3/files"
                    && r.param("q").is_some_and(|q| q.contains("in parents"))
            })
            .collect()
    }

    pub fn media_requests(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(Request::is_media)
            .collect()
    }
}

impl Drop for StubDrive {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // accept を起こす
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 合成の写真 1 件（`files.list` の要素）。
pub fn item(id: &str, name: &str, md5: Option<&str>, mime: &str, meta: Option<Value>) -> Value {
    let mut v = json!({"id": id, "name": name, "mimeType": mime});
    if let Some(md5) = md5 {
        v["md5Checksum"] = json!(md5);
    }
    if let Some(meta) = meta {
        v["imageMediaMetadata"] = meta;
    }
    v
}

fn read_request(
    stream: &mut TcpStream,
) -> Option<(String, String, HashMap<String, String>, String)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some((
        method,
        target,
        headers,
        String::from_utf8_lossy(&body).into_owned(),
    ))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(String, String)],
) -> std::io::Result<()> {
    // `X-No-Length` を付けると Content-Length を送らない（接続を閉じて本文の終わりを知らせる）。
    // `X-Slow-Body: <ミリ秒>` を付けると、ヘッダを送ってからその時間待って本文を送る（本文の受信の時間切れ）
    let slow_ms: Option<u64> = extra
        .iter()
        .find(|(k, _)| k == "X-Slow-Body")
        .and_then(|(_, v)| v.parse().ok());
    // `X-Close-Before-Headers` を付けると、何も書かずに接続を閉じる（応答のヘッダーの前の切断）。
    // `X-Claim-Length: <バイト数>` を付けると、実際の本文より大きな Content-Length を送って本文を途中で切る
    // （ヘッダーを受け取ったあとの切断）
    if extra.iter().any(|(k, _)| k == "X-Close-Before-Headers") {
        return Ok(());
    }
    let claim_length: Option<usize> = extra
        .iter()
        .find(|(k, _)| k == "X-Claim-Length")
        .and_then(|(_, v)| v.parse().ok());
    let no_length = extra.iter().any(|(k, _)| k == "X-No-Length");
    let mut head =
        format!("HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\n");
    if !no_length {
        head.push_str(&format!(
            "Content-Length: {}\r\n",
            claim_length.unwrap_or(body.len())
        ));
    }
    for (k, v) in extra
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "X-No-Length" | "X-Slow-Body" | "X-Claim-Length"))
    {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(ms) = slow_ms {
        stream.flush()?;
        std::thread::sleep(Duration::from_millis(ms));
    }
    stream.write_all(body)?;
    stream.flush()
}

fn handle(mut stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    let Some((method, target, headers, body)) = read_request(&mut stream) else {
        return Ok(());
    };
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.clone(), Vec::new()),
    };
    let mut st = state.lock().unwrap();
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| t.strip_prefix("stub-token-"))
        .and_then(|n| n.parse::<u32>().ok());
    let auth_ok = bearer.is_some_and(|n| n >= 1 && n <= st.tokens_issued && n > st.revoked_through);
    let request = Request {
        method,
        path: path.clone(),
        query,
        body,
        auth_ok,
    };
    st.requests.push(request.clone());
    let mut injected = None;
    for fault in st.faults.iter_mut() {
        if let Some(response) = fault(&request) {
            injected = Some(response);
            break;
        }
    }
    if let Some((status, body, extra)) = injected {
        drop(st);
        return respond(
            &mut stream,
            status,
            "application/json",
            body.as_bytes(),
            &extra,
        );
    }

    if path == "/token" && request.method == "POST" {
        let assertion = request
            .body
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == "assertion")
            .map(|(_, v)| percent_decode(v));
        let grant_ok = request
            .body
            .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer");
        let claims = assertion
            .as_deref()
            .and_then(|a| crate::jwt::verify(a, &st.public_key).ok());
        let valid = grant_ok
            && claims.as_ref().is_some_and(|c| {
                c["iss"] == st.email.as_str()
                    && c["scope"] == super::SCOPE
                    && c["aud"] == st.token_url.as_str()
                    && c["exp"]
                        .as_u64()
                        .zip(c["iat"].as_u64())
                        .is_some_and(|(e, i)| e == i + 3600)
            });
        if !valid {
            drop(st);
            return respond(
                &mut stream,
                400,
                "application/json",
                br#"{"error":"invalid_grant"}"#,
                &[],
            );
        }
        st.tokens_issued += 1;
        let body = json!({
            "access_token": format!("stub-token-{}", st.tokens_issued),
            "expires_in": st.expires_in,
            "token_type": "Bearer",
        })
        .to_string();
        drop(st);
        return respond(&mut stream, 200, "application/json", body.as_bytes(), &[]);
    }

    if !auth_ok {
        drop(st);
        return respond(
            &mut stream,
            401,
            "application/json",
            br#"{"error":{"code":401}}"#,
            &[],
        );
    }

    if path == "/drive/v3/files" && request.method == "GET" {
        let q = request.param("q").unwrap_or("").to_string();
        // フォルダの名前の検索
        if q.starts_with("name='") {
            let found = q.contains(&format!("name='{}'", st.folder_name));
            let files = if found {
                json!([{ "id": st.folder_id }])
            } else {
                json!([])
            };
            drop(st);
            return respond(
                &mut stream,
                200,
                "application/json",
                json!({ "files": files }).to_string().as_bytes(),
                &[],
            );
        }
        // 一覧
        let parent = format!("'{}' in parents and trashed=false", st.folder_id);
        let items: Vec<Value> = if q == parent {
            st.items.clone()
        } else {
            Vec::new()
        };
        let start: usize = request
            .param("pageToken")
            .and_then(|t| t.strip_prefix("page-"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let end = (start + st.page_size).min(items.len());
        let mut page = json!({ "files": items[start.min(items.len())..end] });
        if end < items.len() {
            page["nextPageToken"] = json!(format!("page-{end}"));
        }
        drop(st);
        return respond(
            &mut stream,
            200,
            "application/json",
            page.to_string().as_bytes(),
            &[],
        );
    }

    if let Some(id) = path.strip_prefix("/drive/v3/files/")
        && request.is_media()
    {
        let bytes = st.media.get(id).cloned();
        drop(st);
        return match bytes {
            Some(bytes) => respond(&mut stream, 200, "application/octet-stream", &bytes, &[]),
            None => respond(
                &mut stream,
                404,
                "application/json",
                br#"{"error":{"code":404}}"#,
                &[],
            ),
        };
    }
    drop(st);
    respond(&mut stream, 404, "application/json", b"{}", &[])
}
