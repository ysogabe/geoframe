//! 試験の共通部品: 偽の写真サービス（127.0.0.1 の最小の HTTP サーバー）、一時ディレクトリ、fixture の読み込み。
//! 本物の写真サービス（127.0.0.1:8080）には接続しない。
#![allow(dead_code)] // 試験ファイルごとに使う部品が違う

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geoframe_export::settings::FeedBase;

pub const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/export_photos.json"
);

/// T1 が写真サービス側の試験で出した応答の例（fixture）。
pub fn fixture_json() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(FIXTURE).expect("fixture")).expect("fixture の JSON")
}

pub enum Body {
    Bytes(Vec<u8>),
    /// `len` バイトの同じ値（大きな応答を、メモリに持たずに流す）。
    Filler {
        len: u64,
        byte: u8,
    },
}

pub enum Reply {
    /// 応答の先頭（ステータス行とヘッダー。最後の空行まで）と本文。
    Raw { head: String, body: Body },
    /// 要求を読んだあと、`Duration` のあいだ何も返さない。
    Stall(Duration),
}

impl Reply {
    pub fn json(body: Vec<u8>) -> Self {
        Self::Raw {
            head: format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            body: Body::Bytes(body),
        }
    }

    pub fn status(code: u16) -> Self {
        Self::Raw {
            head: format!("HTTP/1.1 {code} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"),
            body: Body::Bytes(b"{}".to_vec()),
        }
    }

    pub fn redirect(code: u16, location: &str) -> Self {
        Self::Raw {
            head: format!(
                "HTTP/1.1 {code} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            body: Body::Bytes(Vec::new()),
        }
    }
}

/// 127.0.0.1 の空きポートで待ち受け、接続のたびに同じ応答を返す。接続数と要求の 1 行目を数える。
pub struct FakeFeed {
    pub base: FeedBase,
    pub addr: String,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeFeed {
    pub fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(reply);
        {
            let (connections, requests) = (Arc::clone(&connections), Arc::clone(&requests));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    connections.fetch_add(1, Ordering::SeqCst);
                    let (reply, requests) = (Arc::clone(&reply), Arc::clone(&requests));
                    std::thread::spawn(move || serve(stream, &reply, &requests));
                }
            });
        }
        Self {
            base: FeedBase::parse(&format!("http://{addr}")).expect("base"),
            addr,
            connections,
            requests,
        }
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// 受けた要求の 1 行目（`GET /path?query HTTP/1.1`）。
    pub fn request_lines(&self) -> Vec<String> {
        self.requests.lock().expect("lock").clone()
    }
}

fn serve(mut stream: TcpStream, reply: &Reply, requests: &Mutex<Vec<String>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let line = String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    requests.lock().expect("lock").push(line);
    match reply {
        Reply::Stall(d) => std::thread::sleep(*d),
        Reply::Raw { head, body } => {
            if stream.write_all(head.as_bytes()).is_err() {
                return;
            }
            match body {
                Body::Bytes(b) => {
                    let _ = stream.write_all(b);
                }
                Body::Filler { len, byte } => {
                    let chunk = vec![*byte; 64 * 1024];
                    let mut left = *len;
                    while left > 0 {
                        let n = left.min(chunk.len() as u64) as usize;
                        if stream.write_all(&chunk[..n]).is_err() {
                            return;
                        }
                        left -= n as u64;
                    }
                }
            }
        }
    }
}

/// 待ち受けのない 127.0.0.1 のポート（接続拒否の試験用）。
pub fn closed_port_base() -> FeedBase {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    FeedBase::parse(&format!("http://127.0.0.1:{port}")).expect("base")
}

/// `CARGO_TARGET_TMPDIR` の下の一時ディレクトリ（0700）。作ったものだけを、落ちるときに消す。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("tmp dir");
        set_mode(&path, 0o700);
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

pub const MARK_ID: &str = "CLIENT-ID-MARKER-7f3a";
pub const MARK_SECRET: &str = "CLIENT-SECRET-MARKER-91bc";
pub const MARK_TOKEN: &str = "TOKEN-MARKER-5d20";

/// 目印の値を入れた rclone 設定（`remote` のセクションに専用の client_id・client_secret）。
pub fn good_config(remote: &str) -> String {
    format!(
        "[{remote}]\ntype = drive\nclient_id = {MARK_ID}\nclient_secret = {MARK_SECRET}\nscope = drive\ntoken = {{\"access_token\":\"{MARK_TOKEN}\"}}\n"
    )
}

/// `<dir>/rclone/rclone.conf`（親は 0700、ファイルは 0600）を作ってパスを返す。
pub fn write_config(dir: &Path, content: &[u8]) -> PathBuf {
    let parent = dir.join("rclone");
    std::fs::create_dir_all(&parent).expect("mkdir");
    set_mode(&parent, 0o700);
    let path = parent.join("rclone.conf");
    std::fs::write(&path, content).expect("write");
    set_mode(&path, 0o600);
    path
}

/// 目印の値が文字列に出ていないことを確かめる。
pub fn assert_no_secrets(text: &str) {
    for mark in [MARK_ID, MARK_SECRET, MARK_TOKEN] {
        assert!(!text.contains(mark), "設定の値が出ています: {text}");
    }
    assert!(!text.contains("access_token"), "{text}");
}
