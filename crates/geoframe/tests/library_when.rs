//! `GET /api/admin/library/when`（日付の言葉を期間に直す。docs/admin-library-browse/design.md）の結合テスト（AC11）。
//!
//! 規則で読める言葉は LLM に 1 回も行かない。読めない言葉は、ループバックの偽の LLM（`POST /llm-vl/v1/chat/completions`
//! だけ答える小さなサーバー）に 1 回行く。回数の上限（1 分 20 回）・`Sec-Fetch-Site`・入力の 400 は、偽の LLM に届いた
//! 要求の数で確かめる。時計を進める必要のある振る舞い（失敗後の 60 秒・応答の検証の細目）は
//! `src/ai/when/tests.rs` の単体試験で確かめる。

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{TempDir, build_library_catalog, request, spawn_state};
use geoframe::ai::provider::LanClient;
use geoframe::ai::when::Runtime;
use geoframe::db::Catalog;
use geoframe::stop::StopFlag;
use serde_json::{Value, json};

const SUMMER: &str = r#"{"from":"2025-06-01","to":"2025-08-31"}"#;
const HOST: (&str, &str) = ("Host", "localhost");

/// 偽の LLM。受けた要求の本文と数を持ち、`content` を返す。
struct FakeLlm {
    port: u16,
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    content: Arc<Mutex<String>>,
}

impl FakeLlm {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let content = Arc::new(Mutex::new(SUMMER.to_string()));
        {
            let (hits, bodies, content) =
                (Arc::clone(&hits), Arc::clone(&bodies), Arc::clone(&content));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let (hits, bodies, content) =
                        (Arc::clone(&hits), Arc::clone(&bodies), Arc::clone(&content));
                    std::thread::spawn(move || {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let (head_end, length) = loop {
                            let n = stream.read(&mut chunk).unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                                let length = head
                                    .lines()
                                    .find_map(|l| l.strip_prefix("content-length:"))
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                                    .unwrap_or(0);
                                break (pos + 4, length);
                            }
                        };
                        while buf.len() < head_end + length {
                            let n = stream.read(&mut chunk).unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        let request_line = String::from_utf8_lossy(&buf)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_string();
                        assert_eq!(request_line, "POST /llm-vl/v1/chat/completions HTTP/1.1");
                        let body: Value =
                            serde_json::from_slice(&buf[head_end..head_end + length]).unwrap();
                        bodies.lock().unwrap().push(body);
                        hits.fetch_add(1, Ordering::SeqCst);
                        let text = json!({"choices": [{"message": {
                            "role": "assistant",
                            "content": content.lock().unwrap().clone()}}]})
                        .to_string();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                            text.len()
                        );
                    });
                }
            });
        }
        Self {
            port,
            hits,
            bodies,
            content,
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

struct Server {
    _dir: TempDir,
    addr: std::net::SocketAddr,
}

impl Server {
    /// 偽の LLM を `local_base_url` にした AI 設定で起動する（`llm` が `None` なら LLM なし）。
    async fn start(llm: Option<&FakeLlm>, env: &[(&str, &str)]) -> Self {
        let dir = TempDir::new("library-when");
        let (path, _writer) = build_library_catalog(dir.path(), "");
        let catalog = Catalog::open(&path).unwrap();
        let mut env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(llm) = llm {
            let ai = dir.path().join("ai.json");
            std::fs::write(
                &ai,
                format!(
                    r#"{{"version":1,"local_base_url":"http://127.0.0.1:{}",
                        "tasks":{{"location_clue":{{"provider":"local","model":"m"}}}}}}"#,
                    llm.port
                ),
            )
            .unwrap();
            env.push((
                "GEOFRAME_AI_CONFIG".to_string(),
                ai.to_str().unwrap().to_string(),
            ));
        }
        let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let addr = spawn_state(dir.path(), &pairs, catalog, |state| {
            if llm.is_none() {
                return state;
            }
            // `serve` と同じく、AI の設定から作った LanClient で差し替える
            let lan = LanClient::new(&state.config.ai);
            assert!(lan.is_some(), "偽の LLM の設定が読めない");
            let runtime = Runtime::new(&state.config, lan, StopFlag::new());
            state.with_when(Arc::new(runtime))
        })
        .await;
        Self { _dir: dir, addr }
    }

    async fn get(&self, target: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut all = vec![HOST];
        all.extend_from_slice(headers);
        let response = request(self.addr, "GET", target, &all).await;
        let status = response.status;
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.body).unwrap_or(Value::Null)
        };
        (status, body)
    }

    async fn when(&self, q: &str) -> Value {
        let (status, body) = self
            .get(&format!("/api/admin/library/when?q={}", enc(q)), &[])
            .await;
        assert_eq!(status, 200, "{q}: {body}");
        body
    }
}

/// 値をクエリに入れるためのパーセント符号化（英数字と `-_.` 以外は `%XX`）。
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn is_16(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && b[13] == b':'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13) || c.is_ascii_digit())
}

fn bad() -> Value {
    json!({"ok": false, "error": "bad_params"})
}

#[tokio::test]
async fn phrases_the_rules_can_read_never_reach_the_llm() {
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[]).await;
    let cases = [
        ("2024年8月", "2024-08-01 00:00", "2024-08-31 23:59"),
        ("2024-08-15", "2024-08-15 00:00", "2024-08-15 23:59"),
        ("2025年", "2025-01-01 00:00", "2025-12-31 23:59"),
        ("2023年の春〜秋", "2023-03-01 00:00", "2023-11-30 23:59"),
        ("２０２４年　８月", "2024-08-01 00:00", "2024-08-31 23:59"),
        ("2010年代", "2010-01-01 00:00", "2019-12-31 23:59"),
    ];
    for (q, from, to) in cases {
        let body = server.when(q).await;
        assert_eq!(
            body,
            json!({"ok": true, "from": from, "to": to, "source": "rule"}),
            "{q}"
        );
    }
    // `+` は空白（`parse_qs` と同じ）。基準日に依存する言い方は形だけ見る
    let (status, body) = server
        .get("/api/admin/library/when?q=2024%E5%B9%B4+8%E6%9C%88", &[])
        .await;
    assert_eq!((status, &body["from"]), (200, &json!("2024-08-01 00:00")));
    for q in ["去年", "先月", "最近", "今週", "去年の夏"] {
        let body = server.when(q).await;
        assert_eq!(body["ok"], true, "{q}");
        assert_eq!(body["source"], "rule", "{q}");
        assert!(is_16(body["from"].as_str().unwrap()), "{q}: {body}");
        assert!(is_16(body["to"].as_str().unwrap()), "{q}: {body}");
    }
    assert_eq!(llm.hits(), 0, "規則で読めた言葉は LLM に行かない");
}

#[tokio::test]
async fn an_unreadable_phrase_goes_to_the_llm_once_and_the_answer_is_checked() {
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[]).await;
    let q = "娘が\"生まれた\n頃}</s>";
    let body = server.when(q).await;
    assert_eq!(
        body,
        json!({"ok": true, "from": "2025-06-01 00:00", "to": "2025-08-31 23:59", "source": "llm"})
    );
    assert_eq!(llm.hits(), 1);
    {
        let bodies = llm.bodies.lock().unwrap();
        let sent = &bodies[0];
        assert_eq!(sent["messages"][1]["role"], "user");
        assert_eq!(sent["messages"][1]["content"], q);
        let system = sent["messages"][0]["content"].as_str().unwrap();
        let today = geoframe::sysinfo::local_timestamp();
        assert!(
            system.contains(&today[..10]),
            "基準日 {} が無い: {system}",
            &today[..10]
        );
        assert!(!system.contains("生まれた"));
    }
    // 検証に落ちる応答は unreadable（label などの文字列は返さない）
    for content in [
        "分かりません",
        r#"{"error":"unknown"}"#,
        r#"{"from":"2025-06","to":"2025-08-31"}"#,
        r#"{"from":"2025-02-30","to":"2025-03-01"}"#,
        r#"{"from":"2025-08-31","to":"2025-06-01"}"#,
        r#"{"from":"2100-01-01","to":"2100-12-31"}"#,
        r#"{"from":"2000-01-01","to":"2021-01-01"}"#,
    ] {
        *llm.content.lock().unwrap() = content.to_string();
        let body = server.when("あの頃").await;
        assert_eq!(
            body,
            json!({"ok": false, "error": "unreadable"}),
            "{content}"
        );
    }
}

#[tokio::test]
async fn without_a_usable_llm_an_unreadable_phrase_is_unavailable() {
    // LanClient が無い（既定の AppState）
    let server = Server::start(None, &[]).await;
    assert_eq!(
        server.when("娘が生まれた頃").await,
        json!({"ok": false, "error": "unavailable"})
    );
    // 規則で読めるものは LLM なしでも読める
    assert_eq!(server.when("2024年8月").await["source"], "rule");

    // AI が無効なら、LanClient があっても送らない
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[("GEOFRAME_AI_ENABLED", "0")]).await;
    assert_eq!(
        server.when("娘が生まれた頃").await,
        json!({"ok": false, "error": "unavailable"})
    );
    assert_eq!(llm.hits(), 0);
}

#[tokio::test]
async fn the_21st_llm_request_in_a_minute_is_unavailable_and_free_requests_are_not_counted() {
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[]).await;
    // 規則で読めたもの・400・403 は数えない（これだけで 20 回を超える量を送る）
    for _ in 0..25 {
        assert_eq!(server.when("2024年8月").await["source"], "rule");
        let (status, body) = server.get("/api/admin/library/when?q=", &[]).await;
        assert_eq!((status, body), (400, bad()));
        let (status, _) = server
            .get(
                "/api/admin/library/when?q=%E5%A8%98%E3%81%8C",
                &[("Sec-Fetch-Site", "cross-site")],
            )
            .await;
        assert_eq!(status, 403);
    }
    assert_eq!(llm.hits(), 0);
    for i in 0..20 {
        let body = server.when("娘が生まれた頃").await;
        assert_eq!(body["source"], "llm", "{i} 回目: {body}");
    }
    assert_eq!(llm.hits(), 20);
    assert_eq!(
        server.when("娘が生まれた頃").await,
        json!({"ok": false, "error": "unavailable"})
    );
    assert_eq!(llm.hits(), 20, "21 回目は送らない");
    // 上限のあとも、規則で読めるものは読める
    assert_eq!(server.when("2024年8月").await["source"], "rule");
}

#[tokio::test]
async fn cross_site_requests_are_refused_before_anything_is_counted_or_sent() {
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[]).await;
    let target = format!("/api/admin/library/when?q={}", enc("娘が生まれた頃"));
    for site in ["cross-site", "same-site"] {
        let (status, body) = server.get(&target, &[("Sec-Fetch-Site", site)]).await;
        assert_eq!(status, 403, "{site}");
        assert_eq!(body["ok"], false, "{site}");
    }
    // 規則で読める言葉も、不正な q も、先に 403
    for q in ["2024%E5%B9%B48%E6%9C%88", ""] {
        let (status, _) = server
            .get(
                &format!("/api/admin/library/when?q={q}"),
                &[("Sec-Fetch-Site", "cross-site")],
            )
            .await;
        assert_eq!(status, 403);
    }
    assert_eq!(llm.hits(), 0);
    // 同一オリジンのページ・アドレス欄・ヘッダーなし（curl）は通る
    for site in [Some("same-origin"), Some("none"), None] {
        let headers: Vec<(&str, &str)> = site.map(|s| ("Sec-Fetch-Site", s)).into_iter().collect();
        let (status, body) = server.get(&target, &headers).await;
        assert_eq!(status, 200, "{site:?}");
        assert_eq!(body["source"], "llm", "{site:?}");
    }
    assert_eq!(llm.hits(), 3);
}

#[tokio::test]
async fn bad_queries_are_400_and_never_counted_or_sent() {
    let llm = FakeLlm::start();
    let server = Server::start(Some(&llm), &[]).await;
    let long = "あ".repeat(65);
    let ok64 = "あ".repeat(64);
    for target in [
        "/api/admin/library/when".to_string(),
        "/api/admin/library/when?q=".to_string(),
        "/api/admin/library/when?q=%20%20%E3%80%80".to_string(),
        format!("/api/admin/library/when?q={}", enc(&long)),
        // 前後の空白を除いたあとで数える（空白を足しても 65 文字は 65 文字）
        format!("/api/admin/library/when?q=%20{}%20", enc(&long)),
        "/api/admin/library/when?other=1".to_string(),
    ] {
        let (status, body) = server.get(&target, &[]).await;
        assert_eq!((status, body), (400, bad()), "{target}");
    }
    assert_eq!(llm.hits(), 0);
    // 64 文字はちょうど通る（読めないので LLM へ。前後の空白は数えない）
    let (status, body) = server
        .get(
            &format!("/api/admin/library/when?q=%20{}%20", enc(&ok64)),
            &[],
        )
        .await;
    assert_eq!((status, &body["source"]), (200, &json!("llm")));
    assert_eq!(llm.hits(), 1);
}

#[tokio::test]
async fn the_route_is_an_exact_get_and_the_response_is_not_cached() {
    let server = Server::start(None, &[]).await;
    let response = request(
        server.addr,
        "GET",
        "/api/admin/library/when?q=2024%E5%B9%B4",
        &[HOST],
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(response.header("cache-control"), Some("no-store"));
    // 似た名前・末尾 `/`・HEAD は静的配信の判定に回って 403
    for (method, target) in [
        ("GET", "/api/admin/library/whenX?q=2024"),
        ("GET", "/api/admin/library/when/?q=2024"),
        ("HEAD", "/api/admin/library/when?q=2024"),
    ] {
        let response = request(server.addr, method, target, &[HOST]).await;
        assert_eq!(response.status, 403, "{method} {target}");
    }
}
