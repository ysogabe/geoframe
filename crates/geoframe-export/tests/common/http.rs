//! T6 の試験の共通部品: 生の TCP で話す最小の HTTP クライアント（接続を細かく操作するため。`Connection: close` で EOF まで読む）。
//! 相手は、試験が 127.0.0.1 の空きポートで起こした `serve` だけ。
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

#[derive(Debug)]
pub struct Resp {
    pub status: u16,
    /// 小文字にした名前と値。
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("本文が JSON でない: {e}: {:?}", self.body))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// `{"error": ...}` のコード。
    pub fn error(&self) -> String {
        self.json()["error"].as_str().unwrap_or("").to_owned()
    }
}

pub fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s
}

/// 送信元のアドレスを `source`（127.0.0.0/8 の中の、別のアドレス。例: `127.0.0.2`）にして接続する。許可リストの中と外の送信元を、
/// 1 つのホストで作るため（`std` は、接続の前に送信元を決められない）。IPv4 だけ。
pub fn connect_from(addr: SocketAddr, source: &str) -> TcpStream {
    use std::net::Ipv4Addr;
    use std::os::fd::FromRawFd;
    let SocketAddr::V4(dst) = addr else {
        panic!("IPv4 だけ")
    };
    let src: Ipv4Addr = source.parse().expect("送信元");
    let sockaddr = |ip: Ipv4Addr, port: u16| libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(ip.octets()),
        },
        sin_zero: [0; 8],
    };
    let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: 自分で作った TCP ソケットに、構造体のポインタと大きさを渡すだけ。成功したら、fd の持ち主は `TcpStream` に移る。
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0, "socket");
        let s = sockaddr(src, 0);
        let d = sockaddr(*dst.ip(), dst.port());
        let bound = libc::bind(fd, (&raw const s).cast(), len);
        let connected = bound == 0 && libc::connect(fd, (&raw const d).cast(), len) == 0;
        if !connected {
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            panic!("{source} から接続できません: {e}");
        }
        let stream = TcpStream::from_raw_fd(fd);
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
    }
}

/// 応答を EOF まで読んで解釈する。
pub fn read_response(stream: &mut TcpStream) -> Resp {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            // 相手が応答のあとにリセットで閉じることがある（読めた分で解釈する）
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset && !raw.is_empty() => break,
            Err(e) => panic!(
                "応答を読めません: {e} （読めた分: {:?}）",
                String::from_utf8_lossy(&raw)
            ),
        }
    }
    parse_response(&raw)
}

pub fn parse_response(raw: &[u8]) -> Resp {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("応答の形が違う: {:?}", String::from_utf8_lossy(raw)));
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("状態行が違う: {head:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect::<Vec<_>>();
    let body_raw = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.contains("chunked"));
    let body = if chunked {
        String::from_utf8_lossy(&dechunk(body_raw)).into_owned()
    } else {
        String::from_utf8_lossy(body_raw).into_owned()
    };
    Resp {
        status,
        headers,
        body,
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = raw.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&raw[..eol])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
        .unwrap_or(0);
        raw = &raw[eol + 2..];
        if size == 0 || raw.len() < size {
            return out;
        }
        out.extend_from_slice(&raw[..size]);
        raw = raw.get(size + 2..).unwrap_or(&[]);
    }
}

/// 1 つの要求を送って、応答を読む。`headers` に `Host`・`Connection: close` は足される。
pub fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Resp {
    let mut stream = connect(addr);
    send_request(&mut stream, method, path, headers, body);
    read_response(&mut stream)
}

pub fn send_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (n, v) in headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).expect("write head");
    if let Some(b) = body {
        stream.write_all(b).expect("write body");
    }
    stream.flush().expect("flush");
}

/// 認証つき（`Authorization: Bearer <token>`）の GET。
pub fn get(addr: SocketAddr, path: &str, token: &str) -> Resp {
    let auth = format!("Bearer {token}");
    http_request(addr, "GET", path, &[("Authorization", &auth)], None)
}

/// 認証つきの `POST /v1/exports`（`Content-Type: application/json`）。
pub fn post_json(addr: SocketAddr, token: &str, body: &Value) -> Resp {
    post_raw(addr, token, &serde_json::to_vec(body).unwrap())
}

pub fn post_raw(addr: SocketAddr, token: &str, body: &[u8]) -> Resp {
    let auth = format!("Bearer {token}");
    http_request(
        addr,
        "POST",
        "/v1/exports",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
        ],
        Some(body),
    )
}
