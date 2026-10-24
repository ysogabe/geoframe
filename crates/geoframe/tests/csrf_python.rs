//! POST の CSRF 対策の判定（Content-Type・Origin・Host）を、現行 Python の判定と突き合わせる差分試験（T3.1）。
//! フィクスチャは `tools/gen_csrf_fixture.py`（`app/tests/csrf_fixture.py`）が、現行の `_csrf_ok`・`_host_ok` を
//! 実際のヘッダ解析（生バイトを Latin-1 にして `email` のパーサへ）の上で動かして作る。
//!
//! 方針（管理者）: **Python が断るものを Rust が通すことは 1 件も許さない**。Rust だけが断るのは、HTTP 層
//! （hyper/httparse）がヘッダの値の制御文字を先に 400 にするものだけ（`hyper_accepts`）。
//!
//! - 判定の関数（`post::csrf_verdict`・`hostcheck::host_allowed`）は、hyper が値を渡すまでの変換（両端の
//!   空白・タブを取り除く、制御文字なら要求ごと 400）を試験の側で再現して呼ぶ。`Origin` の末尾の空白は
//!   hyper が取り除くので、生のバイト列を見る走査器（`OriginTrailingSpace`）の印を渡す。
//! - 実際の TCP でも、現行 Python のサーバーに生バイトで送った結果（`http`）と状態コード・書き込みの有無を比べる。

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use common::{TempDir, build_admin_catalog, spawn_with_env};
use geoframe::csrf::{self, Verdict};
use geoframe::db::Catalog;
use geoframe::hostcheck;
use geoframe::post;
use geoframe::rawhead::OriginTrailingSpace;
use geoframe::writer::{OpenMode, Writer};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csrf-python.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// フィクスチャの値（Latin-1 の文字列。1 文字＝1 バイト）を生のバイト列にする。
fn raw_bytes(value: &str) -> Vec<u8> {
    value
        .chars()
        .map(|c| u8::try_from(u32::from(c)).expect("Latin-1 の範囲"))
        .collect()
}

/// httparse が値に許すバイト（タブ、0x20〜0x7e、0x80 以上）。これ以外を含む値は、要求ごと 400。
fn hyper_accepts(raw: &[u8]) -> bool {
    raw.iter()
        .all(|&b| b == b'\t' || (0x20..=0x7e).contains(&b) || b >= 0x80)
}

/// httparse が値の両端から取り除くもの（空白・タブ）。
fn ows_trim(raw: &[u8]) -> &[u8] {
    let is_ows = |b: &u8| matches!(b, b' ' | b'\t');
    let start = raw.iter().position(|b| !is_ows(b)).unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|b| !is_ows(b))
        .map_or(start, |i| i + 1);
    &raw[start..end]
}

/// hyper が判定に渡す値。要求ごと断るもの（`None`）か、両端を取り除いた値。
fn through_hyper(raw: &str) -> Option<Vec<u8>> {
    let bytes = raw_bytes(raw);
    hyper_accepts(&bytes).then(|| ows_trim(&bytes).to_vec())
}

/// 現行の判定（フィクスチャ）を `Verdict` に直す。例外（`error`）は現行が応答を返さず接続を閉じるので、
/// Rust の 403（`CrossOrigin`）と対応させる。
fn expected(py: &str) -> Verdict {
    match py {
        "ok" => Verdict::Allowed,
        "content_type" => Verdict::ContentType,
        "origin" | "error" => Verdict::CrossOrigin,
        other => panic!("{other}"),
    }
}

#[derive(Default)]
struct Tally {
    total: usize,
    python_allows: usize,
    /// Python が通し、Rust は HTTP 層が先に 400 にした件数
    http_layer_only: usize,
    /// HTTP 層が断った値に含まれていた、httparse が許さないバイト（0x00〜0x08・0x0a〜0x1f・0x7f だけのはず）
    rejected_bytes: BTreeSet<u8>,
    /// Python が断り、Rust は 400（HTTP 層）で断った
    refused_by_both: usize,
}

/// 1 件の判定を突き合わせる。`headers`: (Content-Type, Origin, Host) の生の値（Latin-1。無ければ `None`）。
fn compare(headers: [Option<&str>; 3], python: &str, tally: &mut Tally) {
    use axum::http::{HeaderMap, HeaderValue, header};
    tally.total += 1;
    if python == "ok" {
        tally.python_allows += 1;
    }
    let mut map = HeaderMap::new();
    let mut rejected = false;
    let mut origin_trailing_space = false;
    for (name, raw) in [header::CONTENT_TYPE, header::ORIGIN, header::HOST]
        .into_iter()
        .zip(headers)
    {
        let Some(raw) = raw else { continue };
        match through_hyper(raw) {
            Some(value) => {
                map.insert(name.clone(), HeaderValue::from_bytes(&value).unwrap());
            }
            None => rejected = true,
        }
        if name == header::ORIGIN {
            let mut line = b"Origin:".to_vec();
            line.extend(raw_bytes(raw));
            line.extend(b"\r\n");
            origin_trailing_space = OriginTrailingSpace::default().feed(&line);
        }
    }
    if rejected {
        // HTTP 層が 400 にする。Python が断るものなら両方断る。通すものだけが「Rust だけが断る」
        for raw in headers.into_iter().flatten() {
            tally
                .rejected_bytes
                .extend(raw_bytes(raw).into_iter().filter(|&b| !hyper_accepts(&[b])));
        }
        if python == "ok" {
            tally.http_layer_only += 1;
        } else {
            tally.refused_by_both += 1;
        }
        return;
    }
    let verdict = post::csrf_verdict(&map, origin_trailing_space);
    assert_eq!(verdict, expected(python), "Python は {python}: {headers:?}");
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn opt(v: &Value) -> Option<&str> {
    v.as_str()
}

#[test]
fn nothing_python_refuses_is_allowed_by_the_content_type_check() {
    let fixture = fixture();
    let cases = fixture["content_type"].as_array().unwrap();
    assert!(cases.len() >= 600, "{}", cases.len());
    let mut tally = Tally::default();
    for case in cases {
        let headers = [Some(s(&case[0])), None, None];
        compare(headers, s(&case[1]), &mut tally);
    }
    report("content_type", &tally);
    // NBSP などを前後に付けた値が、実際に Python では通る側にある（試験が空になっていない）
    assert!(tally.python_allows >= 80, "{}", tally.python_allows);
}

#[test]
fn nothing_python_refuses_is_allowed_by_the_origin_check() {
    let fixture = fixture();
    let cases = fixture["origin"].as_array().unwrap();
    assert!(cases.len() >= 6000, "{}", cases.len());
    let mut tally = Tally::default();
    for case in cases {
        let headers = [Some("application/json"), opt(&case[0]), opt(&case[1])];
        compare(headers, s(&case[2]), &mut tally);
    }
    report("origin", &tally);
    assert!(tally.python_allows >= 2000, "{}", tally.python_allows);
}

#[test]
fn nothing_python_refuses_is_allowed_when_the_headers_are_combined() {
    let fixture = fixture();
    let cases = fixture["mixed"].as_array().unwrap();
    assert!(cases.len() >= 200, "{}", cases.len());
    let mut tally = Tally::default();
    for case in cases {
        let headers = [opt(&case[0]), opt(&case[1]), opt(&case[2])];
        compare(headers, s(&case[3]), &mut tally);
    }
    report("mixed", &tally);
}

fn report(name: &str, tally: &Tally) {
    eprintln!(
        "{name}: {} 件（Python が通す {}、HTTP 層が先に 400 にして Python が通すもの {}、両方が断る 400 {}）。\
         HTTP 層が断る値のバイト: {:02x?}",
        tally.total,
        tally.python_allows,
        tally.http_layer_only,
        tally.refused_by_both,
        tally.rejected_bytes
    );
    // Rust だけが断るのは制御文字を含む値だけ（hyper が将来 0x80 以上などを断るようになったら気づく）
    assert!(
        tally.rejected_bytes.iter().all(|&b| b < 0x20 || b == 0x7f),
        "{:02x?}",
        tally.rejected_bytes
    );
}

/// 現行の `_host_ok`（フィクスチャの `host`）と `hostcheck::host_allowed`。Host は Latin-1 として読む。
#[test]
fn the_host_check_agrees_with_python_for_every_host() {
    let fixture = fixture();
    let cases = fixture["host"].as_array().unwrap();
    assert!(cases.len() >= 1500, "{}", cases.len());
    let names: BTreeSet<String> = ["localhost".to_string()].into();
    let mut http_layer_only = 0;
    let mut python_allows = 0;
    for case in cases {
        let python = s(&case[1]);
        if python == "ok" {
            python_allows += 1;
        }
        let allowed = match opt(&case[0]) {
            None => true,
            Some(raw) => match through_hyper(raw) {
                None => {
                    // 制御文字: HTTP 層が先に 400
                    if python == "ok" {
                        http_layer_only += 1;
                    }
                    continue;
                }
                Some(value) => hostcheck::host_allowed(Some(&csrf::latin1(&value)), 8080, &names),
            },
        };
        assert_eq!(allowed, python == "ok", "Python は {python}: {:?}", case[0]);
    }
    eprintln!(
        "host: {} 件（Python が通す {python_allows}、HTTP 層が先に 400 にして Python が通すもの {http_layer_only}）",
        cases.len()
    );
    assert!(python_allows >= 100, "{python_allows}");
}

// ---- 実際の TCP ----

/// Python と状態コードが違うケースの、Rust の状態コード。どれも**拒否の向き**で、Rust は書かない:
/// - 値の制御文字・折り返し（obs-fold）と、ヘッダとして読めない行を HTTP 層（hyper）が先に 400 にする。Python は値を読んで、
///   通す（200。書く）ものと、断る（403）ものがある。
/// - 末尾に空白がある `Origin` の行を、同じ接続の以降の要求にも掛ける（rawhead.rs）。
///
/// それ以外のケースは、状態コードも書き込みの有無も Python と同じ。
const DIFFERENT_STATUS: &[(&str, u16)] = &[
    ("ct_vt_suffix", 400),
    ("ct_ff_prefix", 400),
    ("ct_fs_prefix", 400),
    ("ct_us_suffix", 400),
    ("fold_content_type", 400),
    ("fold_origin", 400),
    ("fold_origin_bad", 400),
    ("origin_nul_prefix_is_bad_header", 400),
    ("origin_vt_suffix", 400),
    // ヘッダとして読めない行（名前とコロンの間の空白）。現行はそこでヘッダの読み込みを打ち切り、後ろの
    // `Origin` などが消えるので通して書く。hyper は 400
    ("bad_line_hides_content_type", 400),
    ("bad_line_hides_cross_origin", 400),
    ("bad_line_hides_host_mismatch", 400),
    // 2 つ目の Origin に付いていても、現行は最初の Origin しか見ないが、Rust は断る
    ("origin_duplicate_second_trailing_space", 403),
];

struct Server {
    dir: TempDir,
    addr: std::net::SocketAddr,
}

impl Server {
    async fn start() -> Self {
        let dir = TempDir::new("csrf-python");
        let _db = build_admin_catalog(dir.path());
        let path = dir.path().join("catalog.db");
        let catalog = Catalog::open(&path)
            .unwrap()
            .with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        let addr = spawn_with_env(dir.path(), &[], catalog).await;
        Self { dir, addr }
    }

    /// id 4 の座標（要求は毎回違う値を書くので、変わったかで書き込みを見分ける）。
    fn position(&self) -> (f64, f64) {
        let conn = Connection::open_with_flags(
            self.dir.path().join("catalog.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row("SELECT lat, lon FROM photos WHERE id = 4", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    }
}

/// 生の要求を送り、状態コード（応答が無く接続が閉じたときは 0）を返す。
async fn send(addr: std::net::SocketAddr, case: &Value) -> u16 {
    let body = s(&case["body"]).as_bytes();
    let mut raw = format!("POST {} HTTP/1.1\r\n", s(&case["target"])).into_bytes();
    for header in case["headers"].as_array().unwrap() {
        raw.extend(s(&header[0]).chars().map(|c| c as u8));
        raw.extend(b": ");
        raw.extend(raw_bytes(s(&header[1])));
        raw.extend(b"\r\n");
    }
    raw.extend(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    // Content-Length の後ろに送るもの
    for header in case["after"].as_array().unwrap() {
        raw.extend(s(&header[0]).chars().map(|c| c as u8));
        raw.extend(b": ");
        raw.extend(raw_bytes(s(&header[1])));
        raw.extend(b"\r\n");
    }
    raw.extend(b"Connection: close\r\n\r\n");
    raw.extend(body);
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _ = stream.write_all(&raw).await;
    let mut response = Vec::new();
    let read = async {
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => response.extend_from_slice(&chunk[..n]),
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), read)
        .await
        .expect("応答もリセットも来ないまま止まった");
    let head = String::from_utf8_lossy(&response);
    head.split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

#[tokio::test]
async fn real_requests_get_the_same_status_and_never_write_what_python_refuses() {
    let fixture = fixture();
    let cases = fixture["http"].as_array().unwrap();
    assert!(cases.len() >= 84, "{}", cases.len());
    let different: BTreeMap<&str, u16> = DIFFERENT_STATUS.iter().copied().collect();
    let server = Server::start().await;
    let mut seen = 0;
    for case in cases {
        let name = s(&case["name"]);
        let py_status = u16::try_from(case["status"].as_u64().unwrap()).unwrap();
        let py_wrote = case["wrote"].as_bool().unwrap();
        let before = server.position();
        let status = send(server.addr, case).await;
        let wrote = server.position() != before;
        // 絶対に、Python が書かないものを書かない
        assert!(
            !wrote || py_wrote,
            "{name}: Python は書かない（{py_status}）のに Rust が書いた（{status}）"
        );
        if let Some(&expected) = different.get(name) {
            seen += 1;
            assert_ne!(py_status, expected, "{name}: もう同じ。表から外す");
            assert_eq!((status, wrote), (expected, false), "{name}");
            continue;
        }
        assert_eq!(
            wrote, py_wrote,
            "{name}: 書き込み（Python {py_status}、Rust {status}）"
        );
        if py_status == 0 {
            // Python は例外で応答を返さず接続を閉じる。Rust は 403 の定型 JSON
            assert_eq!(status, 403, "{name}");
        } else {
            assert_eq!(status, py_status, "{name}");
        }
    }
    assert_eq!(
        seen,
        different.len(),
        "DIFFERENT_STATUS に載せた名前がフィクスチャに無い"
    );
}

// ---- 末尾に空白がある Origin（rawhead.rs） ----

/// 1 つの応答（状態コードと本文）を読む。`Content-Length` の分だけ読む。
async fn read_response(stream: &mut tokio::net::TcpStream) -> (u16, Vec<u8>) {
    let mut acc = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(split) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&acc[..split]).to_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map_or(0, |v| v.trim().parse().unwrap());
            if acc.len() >= split + 4 + length {
                let status = head.split(' ').nth(1).unwrap().parse().unwrap();
                return (status, acc[split + 4..split + 4 + length].to_vec());
            }
        }
        let n = stream.read(&mut chunk).await.expect("読み取り");
        assert!(n > 0, "応答の途中で閉じた");
        acc.extend_from_slice(&chunk[..n]);
    }
}

fn post(origin: Option<&str>, k: u32) -> Vec<u8> {
    let body = format!(r#"{{"id": 4, "lat": 40.{k:02}, "lon": 41.{k:02}}}"#);
    let mut head = String::from(
        "POST /api/admin/setloc HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n",
    );
    if let Some(origin) = origin {
        head.push_str(&format!("Origin:{origin}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    head.into_bytes()
}

/// 末尾に空白がある `Origin` を受け取った接続は、以降の `Origin` 付きの POST も断る（rawhead.rs の方針。
/// 先読みされた次の要求と取り違えて通してしまわないための、断る側の差）。`Origin` の無い POST と、別の
/// 接続は影響を受けない。
#[tokio::test]
async fn a_connection_that_sent_a_trailing_space_origin_refuses_later_origins_only() {
    let server = Server::start().await;
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    // 1. 末尾に空白（現行も 403）
    stream
        .write_all(&post(Some(" http://localhost "), 1))
        .await
        .unwrap();
    let (status, body) = read_response(&mut stream).await;
    assert_eq!(status, 403);
    assert!(String::from_utf8_lossy(&body).contains("クロスオリジン"));
    // 2. 同じ接続の、きれいな Origin（現行は 200）。Rust は断る（差。書かない）
    let before = server.position();
    stream
        .write_all(&post(Some(" http://localhost"), 2))
        .await
        .unwrap();
    assert_eq!(read_response(&mut stream).await.0, 403);
    assert_eq!(server.position(), before);
    // 3. Origin の無い POST は通る
    stream.write_all(&post(None, 3)).await.unwrap();
    assert_eq!(read_response(&mut stream).await.0, 200);
    assert_ne!(server.position(), before);
    // 4. 別の接続は影響を受けない
    let mut fresh = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let before = server.position();
    fresh
        .write_all(&post(Some(" http://localhost"), 4))
        .await
        .unwrap();
    assert_eq!(read_response(&mut fresh).await.0, 200);
    assert_ne!(server.position(), before);
    // 5. 末尾がタブだけなら、現行は通すので印は付かない
    let mut tabbed = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let before = server.position();
    tabbed
        .write_all(&post(Some(" http://localhost\t"), 5))
        .await
        .unwrap();
    assert_eq!(read_response(&mut tabbed).await.0, 200);
    tabbed
        .write_all(&post(Some(" http://localhost"), 6))
        .await
        .unwrap();
    assert_eq!(read_response(&mut tabbed).await.0, 200);
    assert_ne!(server.position(), before);
}

/// ヘッダの行が複数の読み取りにまたがっても（空白と改行の間で切れても）印が付く。
#[tokio::test]
async fn a_trailing_space_split_across_reads_is_still_seen() {
    let server = Server::start().await;
    let request = post(Some(" http://localhost "), 7);
    let space = request
        .windows(b"localhost \r\n".len())
        .position(|w| w == b"localhost \r\n")
        .unwrap()
        + b"localhost".len();
    // 空白の前・空白と CR の間・CR と LF の間・LF の後で切る
    for cut in [space, space + 1, space + 2, space + 3] {
        let before = server.position();
        let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        stream.set_nodelay(true).unwrap();
        stream.write_all(&request[..cut]).await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        stream.write_all(&request[cut..]).await.unwrap();
        assert_eq!(read_response(&mut stream).await.0, 403, "cut={cut}");
        assert_eq!(server.position(), before, "cut={cut}");
    }
}
