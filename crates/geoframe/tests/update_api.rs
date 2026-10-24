//! 更新 API（E12 `POST /api/admin/setloc`・E13 `POST /api/admin/setflag`）と POST の入口（ルーティング・
//! CSRF 対策・エラー応答）の結合テスト（T3.1）。合成データだけを使う。
//!
//! 現行 Python に同じ要求を順に流した結果（`tools/gen_update_fixture.py` が書き出すフィクスチャ）と、
//! 状態コード・応答の JSON・`photos` の変化が一致することを確かめる。意図した差は
//! フィクスチャの `known_differences` に載るケースだけで、下の `KNOWN` が期待値を持つ。

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use common::{TempDir, build_admin_catalog, get, post_raw, request, spawn_with_env};
use geoframe::db::Catalog;
use geoframe::writer::{OpenMode, Writer};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const JSON_UTF8: &str = "application/json; charset=utf-8";

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/update-api-python.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

struct Server {
    dir: TempDir,
    /// 試験が DB を直接いじる・見るための接続（WAL を保つ）。
    db: Connection,
    addr: std::net::SocketAddr,
}

impl Server {
    /// 管理画面 API と同じ合成 DB。`with_writer` が偽なら、取り込みが無効のときと同じ（writer が無い）。
    async fn start(with_writer: bool) -> Self {
        let dir = TempDir::new("update-api");
        let db = build_admin_catalog(dir.path());
        let path = dir.path().join("catalog.db");
        let mut catalog = Catalog::open(&path).unwrap();
        if with_writer {
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let addr = spawn_with_env(dir.path(), &[], catalog).await;
        Self { dir, db, addr }
    }

    fn snapshot(&self) -> BTreeMap<i64, Map<String, Value>> {
        let conn = Connection::open_with_flags(
            self.dir.path().join("catalog.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut stmt = conn.prepare("SELECT * FROM photos ORDER BY id").unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt.query([]).unwrap();
        let mut out = BTreeMap::new();
        while let Some(row) = rows.next().unwrap() {
            let mut map = Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                    rusqlite::types::ValueRef::Real(f) => Value::from(f),
                    rusqlite::types::ValueRef::Text(t) => {
                        Value::from(String::from_utf8(t.to_vec()).unwrap())
                    }
                    rusqlite::types::ValueRef::Blob(_) => Value::from("<blob>"),
                };
                map.insert(name.clone(), value);
            }
            out.insert(map["id"].as_i64().unwrap(), map);
        }
        out
    }

    /// JSON を CSRF 対策の条件を満たして POST する。
    async fn post_json(&self, target: &str, body: &Value) -> common::Response {
        let body = body.to_string();
        post_raw(
            self.addr,
            target,
            &[
                ("Host".into(), "localhost".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body.as_bytes(),
        )
        .await
    }
}

/// 変わった行の、変わった列だけ（フィクスチャの `changes` と同じ形）。
fn diff(
    before: &BTreeMap<i64, Map<String, Value>>,
    after: &BTreeMap<i64, Map<String, Value>>,
) -> Value {
    let mut out = Map::new();
    for (id, row) in after {
        match before.get(id) {
            None => {
                out.insert(id.to_string(), json!({"_new": row}));
            }
            Some(old) => {
                let changed: Map<String, Value> = row
                    .iter()
                    .filter(|(k, v)| old.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !changed.is_empty() {
                    out.insert(id.to_string(), Value::Object(changed));
                }
            }
        }
    }
    for id in before.keys().filter(|id| !after.contains_key(id)) {
        out.insert(id.to_string(), json!({"_deleted": true}));
    }
    Value::Object(out)
}

/// Python と応答が違うケースの、Rust の期待値: (状態コード, `error` の文面, `photos` の変化)。
fn known(name: &str) -> (u16, &'static str, Value) {
    match name {
        // JSON の NaN・Infinity は Rust の解析が受け付けない（400 のまま、文面だけ違う）
        "setloc_nan_lat" | "setloc_infinity_lon" => (400, "JSONとして解釈できません", json!({})),
        // i64 を超える id は存在しない id（Python は SQLite に渡せず 500）
        "setloc_id_beyond_i64" | "setflag_id_beyond_i64" => {
            (404, "id=9223372036854775808 は存在しません", json!({}))
        }
        // パスの \ は / に読み替えない（Python は読み替えて当たる）
        "route_backslash" => (403, "", json!({})),
        // HTTP 層が Content-Length を先に断る（本文は定型 JSON でない。書き込みは起きない）。
        // 状態コードと接続の扱いは `content_length_boundaries_at_the_http_layer`
        "http_cl_u64_max" => (431, "", json!({})),
        "http_te_chunked_with_cl" => (400, "Content-Length が必要です", json!({})),
        name if name.starts_with("http_cl_") => (400, "", json!({})),
        other => panic!("{other} は known_differences に無い"),
    }
}

#[tokio::test]
async fn every_request_matches_python_status_body_and_row_changes() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    // 取りこぼしが無いよう規模を固定する（ケースを足したら更新する）
    assert!(cases.len() >= 130, "{}", cases.len());
    let known_names: Vec<&str> = fixture["known_differences"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let server = Server::start(true).await;
    let mut seen_known = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let headers: Vec<(String, String)> = case["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_string(),
                    h[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let body: Vec<u8> = {
            let hex = case["body_hex"].as_str().unwrap();
            (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        };
        let before = server.snapshot();
        let response = post_raw(
            server.addr,
            case["target"].as_str().unwrap(),
            &headers,
            &body,
        )
        .await;
        let after = server.snapshot();
        let changes = diff(&before, &after);

        if known_names.contains(&name) {
            seen_known += 1;
            let (status, error, expected_changes) = known(name);
            assert_eq!(response.status, status, "{name}");
            if !error.is_empty() {
                assert_eq!(response.json()["error"], error, "{name}");
            }
            assert_eq!(changes, expected_changes, "{name}");
            // Python は違う（だから known）。差が消えたら、この一覧から外す
            let py_same = case["status"].as_u64().unwrap() as u16 == response.status
                && case["body"] == response.json_or_null()
                && case["changes"] == changes;
            assert!(
                !py_same,
                "{name} は Python と同じになった。known_differences から外す"
            );
            continue;
        }
        assert_eq!(
            response.status,
            case["status"].as_u64().unwrap() as u16,
            "{name}: {}",
            String::from_utf8_lossy(&response.body)
        );
        if !case["body"].is_null() {
            assert_eq!(response.json(), case["body"], "{name}");
            assert_eq!(response.header("content-type"), Some(JSON_UTF8), "{name}");
            assert_eq!(response.header("cache-control"), Some("no-store"), "{name}");
        }
        assert_eq!(changes, case["changes"], "{name}: photos の変化");
        assert!(!response.headers.contains("access-control-"), "{name}");
    }
    assert_eq!(seen_known, known_names.len());
}

trait JsonOrNull {
    fn json_or_null(&self) -> Value;
}

impl JsonOrNull for common::Response {
    fn json_or_null(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

// ---- 取り込みが無効（writer が無い）ときの 503 ----

#[tokio::test]
async fn without_a_writer_valid_updates_are_503_and_write_nothing() {
    let server = Server::start(false).await;
    let before = server.snapshot();
    let valid = [
        (
            "/api/admin/setloc",
            json!({"id": 4, "lat": 35.5, "lon": 139.5}),
        ),
        (
            "/api/admin/setflag",
            json!({"id": 2, "favorite": true, "hidden": true}),
        ),
    ];
    for (target, body) in &valid {
        let response = server.post_json(target, body).await;
        assert_eq!(response.status, 503, "{target}");
        let json = response.json();
        assert_eq!(json["ok"], false);
        assert!(json["error"].as_str().unwrap().contains("書き込みが無効"));
        // 定型文だけ。値・パス・環境変数名を出さない
        let text = response.text();
        for secret in ["35.5", "139.5", "catalog", "GEOFRAME", "/"] {
            assert!(!text.contains(secret), "{secret}: {text}");
        }
        assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    }
    assert_eq!(server.snapshot(), before, "何も書かない");
    // 入力の違反と CSRF 対策は、書き込みの有無によらず Python と同じ（400・403）
    let response = server
        .post_json("/api/admin/setloc", &json!({"id": 4, "lat": 99, "lon": 0}))
        .await;
    assert_eq!(response.status, 400);
    let response = server
        .post_json("/api/admin/setflag", &json!({"id": 4}))
        .await;
    assert_eq!(response.status, 400);
    let response = server
        .post_json(
            "/api/admin/setflag",
            &json!({"id": 99999, "favorite": true}),
        )
        .await;
    assert_eq!(
        response.status, 503,
        "存在の確認は書き込みの側（writer が無ければ確かめられない）"
    );
    let response = post_raw(
        server.addr,
        "/api/admin/setflag",
        &[
            ("Host".into(), "localhost".into()),
            ("Content-Length".into(), "2".into()),
            ("Connection".into(), "close".into()),
        ],
        b"{}",
    )
    .await;
    assert_eq!(response.status, 403);
    // 読み取りは動く
    assert_eq!(get(server.addr, "/api/v1/photos").await.status, 200);
}

// ---- 入口の順序・メソッド ----

#[tokio::test]
async fn host_is_checked_before_the_route_and_the_csrf_checks() {
    let server = Server::start(true).await;
    let body = json!({"id": 4, "favorite": true}).to_string();
    let post = |target: &'static str, host: &'static str| {
        let body = body.clone();
        let addr = server.addr;
        async move {
            post_raw(
                addr,
                target,
                &[
                    ("Host".into(), host.into()),
                    ("Content-Type".into(), "application/json".into()),
                    ("Content-Length".into(), body.len().to_string()),
                    ("Connection".into(), "close".into()),
                ],
                body.as_bytes(),
            )
            .await
        }
    };
    // 許可されない Host は、ルートの有無・本文より先に 403（本文は Host の文面）
    for target in ["/api/admin/setflag", "/api/admin/nope", "/"] {
        let response = post(target, "attacker.example").await;
        assert_eq!(response.status, 403, "{target}");
        assert!(response.text().contains("Forbidden host"), "{target}");
    }
    assert_eq!(server.snapshot()[&4]["favorite"], json!(0));
    // Host が複数あれば 400
    let response = post_raw(
        server.addr,
        "/api/admin/setflag",
        &[
            ("Host".into(), "localhost".into()),
            ("Host".into(), "attacker.example".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ],
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status, 400);
    assert_eq!(server.snapshot()[&4]["favorite"], json!(0));
    // 許可された Host なら通る
    let response = post("/api/admin/setflag", "localhost").await;
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn origin_is_compared_with_the_host_header_including_the_port() {
    let server = Server::start(true).await;
    let send = |host: &'static str, origin: &'static str| {
        let addr = server.addr;
        async move {
            let body = br#"{"id": 4, "favorite": true}"#;
            post_raw(
                addr,
                "/api/admin/setflag",
                &[
                    ("Host".into(), host.into()),
                    ("Origin".into(), origin.into()),
                    ("Content-Type".into(), "application/json".into()),
                    ("Content-Length".into(), body.len().to_string()),
                    ("Connection".into(), "close".into()),
                ],
                body,
            )
            .await
        }
    };
    // 既定の待ち受けポートは 8080。Host にポートがあるなら Origin にも同じポートが要る
    assert_eq!(
        send("localhost:8080", "http://localhost:8080").await.status,
        200
    );
    assert_eq!(send("localhost:8080", "http://localhost").await.status, 403);
    assert_eq!(send("localhost", "http://localhost:8080").await.status, 403);
    // 拒否の文面
    let response = send("localhost", "http://attacker.example").await;
    assert_eq!(response.json()["error"], "クロスオリジンは拒否されます");
}

#[tokio::test]
async fn options_stays_501_and_no_response_carries_cors_headers() {
    let server = Server::start(true).await;
    for target in ["/api/admin/setloc", "/api/admin/setflag", "/api/blog/save"] {
        let response = request(
            server.addr,
            "OPTIONS",
            target,
            &[
                ("Host", "localhost"),
                ("Origin", "http://attacker.example"),
                ("Access-Control-Request-Method", "POST"),
                ("Access-Control-Request-Headers", "content-type"),
            ],
        )
        .await;
        assert_eq!(response.status, 501, "{target}");
        assert!(!response.headers.contains("access-control-"), "{target}");
        assert!(!response.headers.contains("allow:"), "{target}");
    }
    // GET・HEAD の /api/admin/ 配下は今まで通り（P1 で決めたもの）。更新ルートへの GET は未実装の 503、
    // HEAD は 403
    let response = get(server.addr, "/api/admin/setloc").await;
    assert_eq!(response.status, 503);
    let response = request(
        server.addr,
        "HEAD",
        "/api/admin/setloc",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(response.status, 403);
    let response = request(
        server.addr,
        "HEAD",
        "/api/admin/status",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(response.status, 403);
}

#[tokio::test]
async fn a_post_without_content_length_is_400_even_when_chunked() {
    let server = Server::start(true).await;
    let response = post_raw(
        server.addr,
        "/api/admin/setflag",
        &[
            ("Host".into(), "localhost".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Transfer-Encoding".into(), "chunked".into()),
            ("Connection".into(), "close".into()),
        ],
        b"1b\r\n{\"id\": 4, \"favorite\": true}\r\n0\r\n\r\n",
    )
    .await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json()["error"], "Content-Length が必要です");
    assert_eq!(server.snapshot()[&4]["favorite"], json!(0));
}

/// 本文を読まずに 413 を返し、接続を閉じる（未読の本文を次の要求として解釈させない）。
#[tokio::test]
async fn an_oversized_body_gets_413_without_being_read_and_the_connection_closes() {
    let server = Server::start(true).await;
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    // keep-alive の接続で、本文を送らずに 70000 バイトと宣言する
    stream
        .write_all(
            b"POST /api/admin/setflag HTTP/1.1\r\nHost: localhost\r\n\
              Content-Type: application/json\r\nContent-Length: 70000\r\n\r\n",
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    // 本文が届くのを待たずに応答が来て、サーバーの側から閉じる
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw)).await;
    assert!(read.is_ok(), "413 のあと接続が閉じない");
    let text = String::from_utf8_lossy(&raw).to_lowercase();
    assert!(text.starts_with("http/1.1 413"), "{text}");
    assert!(text.contains("connection: close"), "{text}");
    assert!(text.contains("上限65536バイト"), "{text}");
    // 閉じたあとに要求を足しても応答は来ない
    let _ = stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await;
    let mut more = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut more)).await;
    assert!(more.is_empty(), "{}", String::from_utf8_lossy(&more));
}

// ---- 500 の本文に例外を出さない ----

#[tokio::test]
async fn an_internal_error_returns_the_fixed_message_and_nothing_from_the_exception() {
    let server = Server::start(true).await;
    // 書き込みが失敗する状況にする（表が無い）。SQLite のメッセージには表名が入る
    server
        .db
        .execute_batch("ALTER TABLE photos RENAME TO photos_moved")
        .unwrap();
    for (target, body) in [
        (
            "/api/admin/setloc",
            json!({"id": 4, "lat": 12.345678, "lon": 98.765432}),
        ),
        ("/api/admin/setflag", json!({"id": 4, "favorite": true})),
    ] {
        let response = server.post_json(target, &body).await;
        assert_eq!(response.status, 500, "{target}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "内部エラーが発生しました"}),
            "{target}"
        );
        for leaked in [
            "photos",
            "no such table",
            "sqlite",
            "12.345678",
            "catalog.db",
        ] {
            assert!(!response.text().to_lowercase().contains(leaked), "{leaked}");
        }
    }
}

// ---- 並行 ----

/// 同時に来た更新が、それぞれ正しく書かれる（writer の単一接続を通る）。
#[tokio::test]
async fn concurrent_updates_all_land() {
    let server = Server::start(true).await;
    let mut tasks = Vec::new();
    for id in 1..=30i64 {
        let addr = server.addr;
        tasks.push(tokio::spawn(async move {
            let body =
                json!({"id": id, "lat": 10.0 + id as f64 / 100.0, "lon": 20.0, "hidden": true})
                    .to_string();
            // setloc と setflag を同じ行へ
            let headers = vec![
                ("Host".to_string(), "localhost".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Content-Length".to_string(), body.len().to_string()),
                ("Connection".to_string(), "close".to_string()),
            ];
            let a = post_raw(addr, "/api/admin/setloc", &headers, body.as_bytes()).await;
            let b = post_raw(addr, "/api/admin/setflag", &headers, body.as_bytes()).await;
            (a.status, b.status)
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), (200, 200));
    }
    let snapshot = server.snapshot();
    for id in 1..=30i64 {
        let row = &snapshot[&id];
        assert_eq!(row["loc_source"], "manual", "{id}");
        assert_eq!(row["lat"], json!(10.0 + id as f64 / 100.0), "{id}");
        assert_eq!(row["hidden"], json!(1), "{id}");
    }
}

// ---- HTTP 層が先に拒否する Content-Length（server.md §17.1） ----

/// 1 回の接続の結果。
#[derive(Debug)]
struct Probe {
    /// 状態コード（応答が無ければ 0）
    status: u16,
    headers: String,
    body: Vec<u8>,
    /// 応答のあと、同じ接続にもう 1 つ要求を足しても応答が来ない（サーバーが閉じた）
    closed: bool,
}

/// 受け取った分から、ヘッダの終わりと本文までの 1 つの応答を取り出す。足りなければ `None`。
fn parse_response(raw: &[u8]) -> Option<(u16, String, Vec<u8>, usize)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let (status_line, headers) = head.split_once("\r\n").unwrap_or((head, ""));
    let headers = headers.to_lowercase();
    let length: usize = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .map_or(0, |v| v.trim().parse().unwrap());
    let end = split + 4 + length;
    if raw.len() < end {
        return None;
    }
    let status = status_line.split(' ').nth(1).unwrap().parse().unwrap();
    Some((status, headers, raw[split + 4..end].to_vec(), end))
}

/// `raw`（要求のバイト列。本文は付けても付けなくてもよい）を実際の TCP 接続で送り、最初の応答を読む。
/// そのあと同じ接続に `GET /healthz` を足して、応答が来れば接続は保たれ、EOF・リセットなら閉じられている。
/// 時間には頼らない（応答もリセットも EOF も即座に来る）。止まった試験は 20 秒の上限でハングではなく失敗にする。
async fn probe(addr: std::net::SocketAddr, raw: &[u8]) -> Probe {
    async fn read_one(stream: &mut tokio::net::TcpStream, acc: &mut Vec<u8>) -> bool {
        let mut chunk = [0u8; 8192];
        loop {
            if parse_response(acc).is_some() {
                return true;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return false,
                Ok(n) => acc.extend_from_slice(&chunk[..n]),
            }
        }
    }
    let run = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let _ = stream.write_all(raw).await;
        let mut acc = Vec::new();
        if !read_one(&mut stream, &mut acc).await {
            return Probe {
                status: 0,
                headers: String::new(),
                body: Vec::new(),
                closed: true,
            };
        }
        let (status, headers, body, used) = parse_response(&acc).unwrap();
        acc.drain(..used);
        let _ = stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await;
        let closed = !read_one(&mut stream, &mut acc).await;
        Probe {
            status,
            headers,
            body,
            closed,
        }
    };
    tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("応答もリセットも来ないまま止まった")
}

/// HTTP 層（axum/hyper）が `Content-Length` を先に検査して断る境界と、hyper が通すが Python が断る境界
/// （`int()` の 4300 桁、`http.server` の 1 行 65536 バイト・ヘッダ 99 個）。前者は `post.rs` に届く前なので、
/// server.md §17.1 の表の順序・状態コードどおりにはならないが、Python との差は**拒否の向きだけ**（Python が
/// 書く入力を Rust が断る）。後者は `post.rs` が書く前に断り、Python と同じ状態コードになる（Rust が書いて
/// Python が断る入力は、この表に無い）。要求と Python の結果は、フィクスチャ（`http_*` ほか）から読む。
/// 値は固定した crates（hyper 1.x）の振る舞いで、上げたときはここが落ちて気づく。
#[tokio::test]
async fn content_length_boundaries_at_the_http_layer() {
    // (ケース名, Python の状態, Rust の状態, Rust が応答のあと接続を閉じる, Rust が書く)
    const TABLE: &[(&str, u16, u16, bool, bool)] = &[
        // 上限の境目: ちょうど 65536 は通り、65537 は本文を読まずに 413 と接続の切断
        ("body_exactly_limit_ok", 200, 200, false, true),
        ("length_declared_too_big", 413, 413, true, false),
        // 巨大・不正な値は hyper が先に断る（u64 の最大値は 431、それを超えると 400）
        ("http_cl_u64_max", 413, 431, true, false),
        ("http_cl_u64_max_plus1", 413, 400, true, false),
        ("http_cl_not_digits", 400, 400, true, false),
        ("http_cl_not_digits_text_plain", 403, 400, true, false),
        ("http_cl_negative", 400, 400, true, false),
        ("http_cl_empty", 400, 400, true, false),
        // Python の int() だけが読む形（Python は書く）
        ("http_cl_plus_sign", 200, 400, true, false),
        ("http_cl_underscore", 200, 400, true, false),
        ("http_cl_list_same", 400, 400, true, false),
        // 重複した Content-Length: 同じ値だけ通る
        ("http_cl_dup_same", 200, 200, false, true),
        ("http_cl_dup_first_ok_second_big", 200, 400, true, false),
        ("http_cl_dup_first_big_second_ok", 413, 400, true, false),
        ("http_cl_dup_second_over_limit", 200, 400, true, false),
        // Python の int() が読む桁数の上限（4300 桁。符号・空白・_ は数えず、先頭の 0 は数える）。
        // hyper は先頭の 0 の長い表記を通すので、post.rs が 4301 桁以上を 400 にする（書かない）
        ("http_cl_zeros_4300", 200, 200, false, true),
        ("http_cl_zeros_4301", 400, 400, false, false),
        ("http_cl_zeros_4302", 400, 400, false, false),
        // 桁数の検査は 413 より前（4301 桁なら、値が上限超過でも 413 でなく 400）。本文を送らない宣言だけの
        // 要求は、400 でも本文が来ないので接続を閉じる
        ("http_cl_zeros_4300_over_limit", 413, 413, true, false),
        ("http_cl_zeros_4301_over_limit", 400, 400, true, false),
        // 前後の空白・タブは hyper が取り除く。桁数には数えない
        ("http_cl_spaces_zeros_4300", 200, 200, false, true),
        ("http_cl_spaces_zeros_4301", 400, 400, false, false),
        ("http_cl_tab_padded", 200, 200, false, true),
        ("http_cl_plus_zeros_4301", 400, 400, true, false),
        // 重複: hyper は最初の 1 つだけを残す（数値が同じなら通す）。Python も最初の値だけを使う
        ("http_cl_dup_first_zeros_4301", 400, 400, false, false),
        ("http_cl_dup_second_zeros_4301", 200, 200, false, true),
        // 現行 http.server が行の読み込みで断る大きさ（1 行 65536 バイト・ヘッダ 99 個まで。要求行は 414、
        // ヘッダは 431）。hyper の既定はもっと緩いので post.rs が断る。境界ちょうどは通る
        ("http_hdr_line_at_limit", 200, 200, false, true),
        ("http_hdr_line_over_limit", 431, 431, true, false),
        ("http_hdr_count_at_limit", 200, 200, false, true),
        ("http_hdr_count_over_limit", 431, 431, true, false),
        ("http_line_at_limit", 200, 200, false, true),
        ("http_line_over_limit", 414, 414, true, false),
        // Transfer-Encoding: chunked は、Content-Length が無い扱いで 400
        ("http_te_chunked_only", 400, 400, false, false),
        ("http_te_chunked_with_cl", 400, 400, true, false),
    ];
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    let server = Server::start(true).await;
    for (name, py_status, status, closed, writes) in TABLE {
        let case = cases
            .iter()
            .find(|c| c["name"] == *name)
            .unwrap_or_else(|| panic!("{name} がフィクスチャに無い"));
        assert_eq!(case["status"], *py_status, "{name}: Python の結果");
        // フィクスチャの要求をそのまま送る。ただし接続を保てるか見るので Connection: close は keep-alive に替える
        let mut raw =
            format!("POST {} HTTP/1.1\r\n", case["target"].as_str().unwrap()).into_bytes();
        for h in case["headers"].as_array().unwrap() {
            let (k, v) = (h[0].as_str().unwrap(), h[1].as_str().unwrap());
            // 行の数を変えないよう、外さずに keep-alive に替える（ヘッダの個数の境界を試すため）
            let v = if k.eq_ignore_ascii_case("connection") {
                "keep-alive"
            } else {
                v
            };
            raw.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        raw.extend_from_slice(b"\r\n");
        let hex = case["body_hex"].as_str().unwrap();
        raw.extend(
            (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()),
        );

        let before = server.snapshot();
        let p = probe(server.addr, &raw).await;
        let wrote = server.snapshot() != before;
        let context = format!("{name}: {} {}", p.status, String::from_utf8_lossy(&p.body));
        assert_eq!(p.status, *status, "{context}");
        assert_eq!(p.closed, *closed, "接続の切断 {context}");
        assert_eq!(wrote, *writes, "書き込み {context}");
        assert!(!p.headers.contains("access-control-"), "{name}");
    }
}

/// `id` が `i64` を超える要求は、SQLite の rowid の最大（`i64::MAX`）の行があっても、その行を更新しない。
#[tokio::test]
async fn an_id_beyond_i64_never_updates_the_row_with_the_largest_rowid() {
    let server = Server::start(true).await;
    server
        .db
        .execute(
            "INSERT INTO photos(id, source, name, lat, lon, loc_source, favorite, hidden)
             VALUES(9223372036854775807, 'drive', 'max', 35.0, 139.0, 'exif', 0, 0)",
            [],
        )
        .unwrap();
    let before = server.snapshot();
    for (target, body) in [
        (
            "/api/admin/setloc",
            r#"{"id": 9223372036854775808, "lat": 1, "lon": 2}"#,
        ),
        (
            "/api/admin/setflag",
            r#"{"id": 9223372036854775808, "favorite": true, "hidden": true}"#,
        ),
        (
            "/api/admin/setflag",
            r#"{"id": 18446744073709551615, "favorite": true}"#,
        ),
    ] {
        let response = server
            .post_json(target, &serde_json::from_str(body).unwrap())
            .await;
        assert_eq!(response.status, 404, "{target} {body}");
        assert_eq!(server.snapshot(), before, "{target} {body}");
    }
    // 実在する最大の id そのものは更新できる
    let response = server
        .post_json(
            "/api/admin/setflag",
            &json!({"id": 9223372036854775807i64, "favorite": true}),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(server.snapshot()[&i64::MAX]["favorite"], json!(1));
}
