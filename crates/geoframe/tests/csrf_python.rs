//! POST の CSRF 対策の判定（Content-Type・Origin・Host）を、現行 Python の判定と突き合わせる差分試験（T3.1）。
//! フィクスチャ（Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_csrf_fixture.py` は P5b で削除した。再生成はできない）は、Python 版の `_csrf_ok`・`_host_ok` を
//! 実際のヘッダ解析（生バイトを Latin-1 にして `email` のパーサへ）の上で動かして作る。
//!
//! 方針（管理者）: **Python が断るものを Rust が通すことは、既知の差の 1 つを除いて許さない**。
//! 既知の差は、`Origin` の値の末尾の空白・タブ（hyper が RFC 9110 の OWS として取り除く。Python は空白を
//! 残して 403）だけ（`is_known_ows_difference`）。Rust だけが断るのは、HTTP 層（hyper/httparse）がヘッダの
//! 値の制御文字を先に 400 にするものだけ（`hyper_accepts`）。
//!
//! - 判定の関数（`post::csrf_verdict`・`hostcheck::host_allowed`）は、hyper が値を渡すまでの変換（両端の
//!   空白・タブを取り除く、制御文字なら要求ごと 400）を試験の側で再現して呼ぶ。
//! - 実際の TCP でも、現行 Python のサーバーに生バイトで送った結果（`http`）と状態コード・書き込みの有無を比べる。

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use common::{TempDir, build_admin_catalog, spawn_with_env};
use geoframe::csrf::{self, Verdict};
use geoframe::db::Catalog;
use geoframe::hostcheck;
use geoframe::post;
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
    /// 既知の差: Python が断り、Rust は通した（`Origin` の末尾の空白・タブ。OWS の除去）
    known_ows_difference: usize,
}

/// フィクスチャの中で、既知の差に当たる件数（固定する）。
const KNOWN_OWS_DIFFERENCES_IN_ORIGIN: usize = 70;
const KNOWN_OWS_DIFFERENCES_IN_MIXED: usize = 3;

/// 既知の差の条件（狭く）: `Origin` の値の**末尾**が空白かタブ（hyper が OWS として取り除くもの）で、
/// Python が `Origin` の判定（403）か例外（接続を閉じる）で断り、Rust は通した。先頭の空白・タブは
/// Python も取り除く（`email` のパーサが `lstrip`、`urlsplit` も先頭の空白を除く）ので差にならない。
fn is_known_ows_difference(origin: Option<&str>, python: &str, rust: Verdict) -> bool {
    rust == Verdict::Allowed
        && matches!(python, "origin" | "error")
        && origin.is_some_and(|o| o.ends_with([' ', '\t']))
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
    let verdict = post::csrf_verdict(&map);
    if is_known_ows_difference(headers[1], python, verdict) {
        // 原因が空白・タブの除去だけであることの確認: hyper が取り除く前の生の値を同じ判定に渡せば、
        // Python と同じになる（`netloc` や角括弧の読み違いが、末尾の空白と重なって紛れ込まない）
        let raw = headers.map(|h| h.map(raw_bytes));
        let untrimmed = csrf::verdict(raw[0].as_deref(), raw[1].as_deref(), raw[2].as_deref());
        assert_eq!(untrimmed, expected(python), "{headers:?}");
        tally.known_ows_difference += 1;
        return;
    }
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
    // `Content-Type` に既知の差は無い（許す向きは 0 件）
    assert_eq!(tally.known_ows_difference, 0);
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
    // 既知の差（末尾の空白・タブ）。減ったら Rust か Python の読み方が変わった、増えたら新しい差が入った
    assert_eq!(tally.known_ows_difference, KNOWN_OWS_DIFFERENCES_IN_ORIGIN);
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
    assert_eq!(tally.known_ows_difference, KNOWN_OWS_DIFFERENCES_IN_MIXED);
}

fn report(name: &str, tally: &Tally) {
    eprintln!(
        "{name}: {} 件（Python が通す {}、HTTP 層が先に 400 にして Python が通すもの {}、両方が断る 400 {}、\
         既知の差 {}）。HTTP 層が断る値のバイト: {:02x?}",
        tally.total,
        tally.python_allows,
        tally.http_layer_only,
        tally.refused_by_both,
        tally.known_ows_difference,
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
///
/// それ以外のケースは、状態コードも書き込みの有無も Python と同じ。許す向きの差は `KNOWN_OWS_DIFFERENCES` の
/// 1 つだけ。
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
];

/// 既知の差（許す向き）: 最初の `Origin` の値の末尾が空白・タブ（hyper が RFC 9110 の OWS として取り除く）で、
/// Python は空白を残して 403 にする（例外のものは応答なしで接続を閉じる）のに、Rust は取り除いて通して 200 で書く。
/// 先頭の空白・タブは Python も取り除くので差にならない。守りは変わらない（server.md §17.1.2）。
const KNOWN_OWS_DIFFERENCES: &[&str] = &[
    "origin_trailing_space",
    "origin_trailing_spaces",
    "origin_trailing_tab_space",
    "origin_trailing_space_tab",
    "origin_leading_and_trailing_space",
    "origin_trailing_space_bracket",
    "origin_trailing_space_ip",
    "origin_trailing_space_empty_netloc",
    "origin_name_uppercase_trailing_space",
    "origin_duplicate_first_trailing_space",
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
    tokio::time::timeout(Duration::from_secs(15), read)
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
    let ows: BTreeSet<&str> = KNOWN_OWS_DIFFERENCES.iter().copied().collect();
    let server = Server::start().await;
    let mut seen = 0;
    let mut seen_ows = 0;
    for case in cases {
        let name = s(&case["name"]);
        let py_status = u16::try_from(case["status"].as_u64().unwrap()).unwrap();
        let py_wrote = case["wrote"].as_bool().unwrap();
        let before = server.position();
        let status = send(server.addr, case).await;
        let wrote = server.position() != before;
        if ows.contains(name) {
            // 既知の差: Python は断る（403か、例外で応答なし）のに、Rust は取り除いて通す
            seen_ows += 1;
            assert!(
                origin_ends_with_ows(case),
                "{name}: 最初の Origin の末尾が空白・タブでない"
            );
            // 原因が空白・タブの除去だけであること: 取り除く前の生の値の判定は、Python の 403（例外）と同じ
            assert_eq!(untrimmed_verdict(case), Verdict::CrossOrigin, "{name}");
            assert!(
                !py_wrote && matches!(py_status, 0 | 403),
                "{name}: もう同じ。表から外す"
            );
            assert_eq!((status, wrote), (200, true), "{name}");
            continue;
        }
        // 絶対に、既知の差以外で、Python が書かないものを書かない
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
    assert_eq!(
        seen_ows,
        ows.len(),
        "KNOWN_OWS_DIFFERENCES に載せた名前がフィクスチャに無い"
    );
}

/// 要求の最初の `Origin` の値（Latin-1）の末尾が、空白かタブか。
fn origin_ends_with_ows(case: &Value) -> bool {
    case["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| s(&h[0]).eq_ignore_ascii_case("origin"))
        .is_some_and(|h| s(&h[1]).ends_with([' ', '\t']))
}

/// 要求の最初の `name` ヘッダの生の値（Latin-1 を 1 バイト＝1 文字で戻したもの）。
fn first_raw_header(case: &Value, name: &str) -> Option<Vec<u8>> {
    case["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| s(&h[0]).eq_ignore_ascii_case(name))
        .map(|h| raw_bytes(s(&h[1])))
}

/// hyper が両端を取り除く前の生の値で、`csrf::verdict` を呼んだ結果。
fn untrimmed_verdict(case: &Value) -> Verdict {
    csrf::verdict(
        first_raw_header(case, "content-type").as_deref(),
        first_raw_header(case, "origin").as_deref(),
        first_raw_header(case, "host").as_deref(),
    )
}

// ---- 末尾に空白がある Origin（既知の差） ----

/// 1 つの応答の状態コードを読む（`Connection: close` で送るので、閉じるまで読む）。
async fn post_status(addr: std::net::SocketAddr, origin: Option<&str>, k: u32) -> u16 {
    let body = format!(r#"{{"id": 4, "lat": 40.{k:02}, "lon": 41.{k:02}}}"#);
    let mut head = String::from(
        "POST /api/admin/setloc HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Connection: close\r\n",
    );
    if let Some(origin) = origin {
        head.push_str(&format!("Origin:{origin}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    // 閉じられないまま止まった試験を、ハングではなく失敗にする
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut response))
        .await
        .expect("応答が閉じられない")
        .unwrap();
    String::from_utf8_lossy(&response)
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// `Origin` の値の前後の空白・タブは hyper が取り除くので、Rust は通す（Python は末尾の空白を残して 403）。
/// 差が消えたり（Rust が 403 にする）、広がったり（空白以外が付いても通す）したら落ちる。
#[tokio::test]
async fn origin_surrounding_ows_is_trimmed_by_hyper_so_rust_allows_what_python_refuses() {
    let server = Server::start().await;
    // (Origin の行の `Origin:` の後ろ, 期待する状態コード)
    let cases: &[(&str, u16)] = &[
        (" http://localhost ", 200),   // Python は 403
        (" http://localhost   ", 200), // Python は 403
        (" http://localhost\t ", 200), // Python は 403
        (" http://localhost \t", 200), // Python は 403
        ("\thttp://localhost\t", 200), // Python も 200（タブは urlparse が捨てる）
        (" http://localhost", 200),    // Python も 200
        (" http://localhost/ ", 200),  // Python も 200
        // OWS 以外が付けば、Python と同じく断る
        (" http://localhost\x0c", 400), // 制御文字は HTTP 層が 400（Python は 403）
        (" http://localhost.", 403),
        (" http://localhost:80 ", 403),
        (" http://other.test ", 403),
    ];
    for (k, (origin, expected)) in cases.iter().enumerate() {
        let before = server.position();
        let status = post_status(server.addr, Some(origin), u32::try_from(k).unwrap() + 1).await;
        assert_eq!(status, *expected, "{origin:?}");
        assert_eq!(server.position() != before, *expected == 200, "{origin:?}");
    }
}

/// 同じ接続の後続の要求は、前の要求の `Origin` に引きずられない（接続に印を残さない）。
#[tokio::test]
async fn a_later_request_on_the_same_connection_is_judged_on_its_own_origin() {
    let server = Server::start().await;
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    for (k, origin) in [
        " http://localhost ",
        " http://other.test",
        " http://localhost",
    ]
    .into_iter()
    .enumerate()
    {
        let body = format!(r#"{{"id": 4, "lat": 42.{k:02}, "lon": 43.{k:02}}}"#);
        let request = format!(
            "POST /api/admin/setloc HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Origin:{origin}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let (status, _) = tokio::time::timeout(Duration::from_secs(15), read_response(&mut stream))
            .await
            .expect("応答が返らない");
        assert_eq!(status, if k == 1 { 403 } else { 200 }, "{origin:?}");
    }
}

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
