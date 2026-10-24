//! 地名検索の試験。
//!
//! - `python_fixture_*`: 現行 Python の `handlers.Handler` に流した結果（`tests/fixtures/geocode-python.json`。
//!   `tools/gen_geocode_fixture.py`）と、同じ要求・同じ応答のスタブ・同じ偽の時計で比べる。状態コード・応答・
//!   上流への要求の順とクエリ・User-Agent・待ち・キャッシュの全体。
//! - それ以外は、Python との意図した差（本文の上限、同時の処理数、ログ）と、同時の要求で Nominatim が 1 秒 1 件を
//!   超えないこと、取り込みの有効・無効での `.cache.json` の扱い。
//!
//! 外部へは通信しない（ループバックのスタブだけ）。時計は偽で、実時間の待ちに頼らない。座標・地名・検索語は合成の値。

use std::fs;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::stub::{Reply, SharedClock, StubUpstreams, TestClock, Upstream};
use super::*;
use crate::cache::{Cache, SaveOutcome};
use crate::config::{Config, Overrides};
use crate::db::Catalog;
use crate::server::{AppState, router};
use crate::testutil::{TempDir, capture, synthetic_catalog};

const START: Duration = Duration::from_secs(100);

fn fixture() -> Value {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/geocode-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn secs(v: &Value) -> Duration {
    Duration::from_secs_f64(v.as_f64().unwrap())
}

/// 偽の時計・スタブ・キャッシュ・地名検索・HTTP サーバー（`router`）の一式。
struct Rig {
    clock: Arc<TestClock>,
    stub: StubUpstreams,
    cache: Arc<Cache>,
    geocoder: Arc<Geocoder>,
    addr: SocketAddr,
    _dir: TempDir,
    _writer: Mutex<rusqlite::Connection>,
}

impl Rig {
    async fn start() -> Self {
        Self::start_with(MAX_IN_FLIGHT, Cache::memory_only()).await
    }

    async fn start_with(in_flight: usize, cache: Cache) -> Self {
        let clock = TestClock::new(START);
        let stub = StubUpstreams::start(Arc::clone(&clock));
        let cache = Arc::new(cache);
        let geocoder = Arc::new(Geocoder::with_parts(
            Endpoints::stub(&stub.base_url()),
            Box::new(SharedClock(Arc::clone(&clock))),
            in_flight,
        ));
        let dir = TempDir::new("geocode");
        let (path, writer) = synthetic_catalog(dir.path(), 0);
        let config = Config::load(|_| None, &Overrides::default()).unwrap();
        let state = AppState::new(Arc::new(config), Catalog::open(&path).unwrap())
            .with_cache(Arc::clone(&cache))
            .with_geocoder(Arc::clone(&geocoder));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        Self {
            clock,
            stub,
            cache,
            geocoder,
            addr,
            _dir: dir,
            _writer: Mutex::new(writer),
        }
    }

    fn put_cache(&self, initial: &Value) {
        for (key, value) in initial.as_object().unwrap() {
            self.cache.put(key, value.clone());
        }
    }

    async fn request(&self, method: &str, target: &str) -> (u16, Vec<u8>) {
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        stream
            .write_all(
                format!(
                    "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, raw[split + 4..].to_vec())
    }

    async fn get(&self, target: &str) -> (u16, Vec<u8>) {
        self.request("GET", target).await
    }

    async fn get_json(&self, target: &str) -> (u16, Value) {
        let (status, body) = self.get(target).await;
        (status, serde_json::from_slice(&body).unwrap())
    }
}

fn reply_of(spec: &Value) -> Reply {
    if spec.get("hangup").is_some() {
        return Reply::hangup();
    }
    let body = match spec.get("body_hex") {
        Some(hex) => {
            let hex = hex.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect()
        }
        None => spec["body"].as_str().unwrap_or("").as_bytes().to_vec(),
    };
    Reply {
        status: spec["status"].as_u64().unwrap() as u16,
        body,
        hangup: false,
        took: spec.get("took").map_or(Duration::ZERO, secs),
    }
}

fn approx_eq(a: &[Duration], b: &[Duration]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x.as_secs_f64() - y.as_secs_f64()).abs() < 1e-9)
}

// ---------------------------------------------------------------- Python との突き合わせ

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_fixture_requests_responses_waits_and_cache_match() {
    let doc = fixture();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 30, "{}", cases.len());
    let mut steps_total = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let rig = Rig::start().await;
        if let Some(initial) = case.get("initial_cache") {
            rig.put_cache(initial);
        }
        for (n, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            steps_total += 1;
            let label = format!("{name} #{n}");
            for (key, upstream) in [("gsi", Upstream::Gsi), ("osm", Upstream::Osm)] {
                rig.stub.set(upstream, step.get(key).map(reply_of));
            }
            rig.stub.take_seen();
            rig.clock.take_sleeps();
            if let Some(advance) = step.get("advance") {
                rig.clock.advance(secs(advance));
            }
            if let Some(target) = step.get("get") {
                let (status, body) = rig.get_json(target.as_str().unwrap()).await;
                assert_eq!(status as u64, step["status"].as_u64().unwrap(), "{label}");
                assert_eq!(body, step["body"], "{label}");
            } else {
                let spec = &step["nominatim"];
                let (geocoder, cache) = (Arc::clone(&rig.geocoder), Arc::clone(&rig.cache));
                let (q, cc) = (
                    spec["q"].as_str().unwrap().to_string(),
                    spec["countrycode"].as_str().unwrap().to_string(),
                );
                let items =
                    tokio::task::spawn_blocking(move || geocoder.nominatim_search(&cache, &q, &cc))
                        .await
                        .unwrap();
                assert_eq!(Value::Array(items), step["items"], "{label}");
            }
            let seen: Vec<Value> = rig
                .stub
                .take_seen()
                .into_iter()
                .map(|s| {
                    json!({"upstream": s.upstream.name(), "query": s.query,
                           "user_agent": s.user_agent})
                })
                .collect();
            let expected: Vec<Value> = step["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    assert_eq!(r["timeout"].as_u64().unwrap(), TIMEOUT.as_secs(), "{label}");
                    json!({"upstream": r["upstream"], "query": r["query"],
                           "user_agent": r["user_agent"]})
                })
                .collect();
            assert_eq!(seen, expected, "{label}: 上流への要求");
            let sleeps = rig.clock.take_sleeps();
            let expected_sleeps: Vec<Duration> = step["sleeps"]
                .as_array()
                .unwrap()
                .iter()
                .map(secs)
                .collect();
            assert!(
                approx_eq(&sleeps, &expected_sleeps),
                "{label}: 待ち {sleeps:?} / {expected_sleeps:?}"
            );
            assert_eq!(
                Value::Object(rig.cache.snapshot()),
                step["cache"],
                "{label}: キャッシュ"
            );
        }
    }
    assert!(steps_total > 150, "{steps_total}");
}

#[test]
fn python_fixture_constants_match() {
    let doc = fixture();
    let c = &doc["constants"];
    assert_eq!(c["user_agent"], USER_AGENT);
    assert_eq!(c["timeout"].as_u64().unwrap(), TIMEOUT.as_secs());
    assert_eq!(c["min_len"].as_u64().unwrap() as usize, MIN_LEN);
    assert_eq!(c["max_results"].as_u64().unwrap() as usize, MAX_RESULTS);
    assert_eq!(secs(&c["min_interval"]), NOMINATIM_MIN_INTERVAL);
    // 本番の通信先は Python の config.py と同じ（接頭辞と、クエリの作り方）
    let production = Endpoints::production();
    assert_eq!(
        production.gsi,
        "https://msearch.gsi.go.jp/address-search/AddressSearch?q={q}"
    );
    assert_eq!(
        production.nominatim,
        "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2&limit={limit}&accept-language=ja"
    );
}

#[test]
fn python_fixture_small_functions_match() {
    use crate::pyquery::{py_float, py_quote, py_strip};
    let doc = fixture();
    let floats = doc["float_samples"].as_array().unwrap();
    assert!(floats.len() > 60);
    for sample in floats {
        let s = sample["s"].as_str().unwrap();
        let got = py_float(s);
        match sample["f"].as_str() {
            None => assert_eq!(got, None, "{s:?}"),
            Some("nan") => assert!(got.is_some_and(f64::is_nan), "{s:?}"),
            Some(expected) => assert_eq!(got, Some(expected.parse::<f64>().unwrap()), "{s:?}"),
        }
    }
    for sample in doc["quote_samples"].as_array().unwrap() {
        let s = sample["s"].as_str().unwrap();
        assert_eq!(py_quote(s), sample["q"].as_str().unwrap(), "{s:?}");
    }
    // `str.isspace` が真のコードポイントは、前後から落とす。それ以外は落とさない（全コードポイント）
    let spaces: Vec<u32> = doc["space_code_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    for code in 0..=0x10FFFFu32 {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        let text = format!("{c}x{c}");
        let stripped = py_strip(&text);
        if spaces.binary_search(&code).is_ok() {
            assert_eq!(stripped, "x", "U+{code:04X}");
        } else {
            assert_eq!(stripped, text, "U+{code:04X}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_gets_other_than_the_reads_are_403_like_python() {
    let doc = fixture();
    let rig = Rig::start().await;
    let gets = doc["admin_gets"].as_array().unwrap();
    assert!(gets.len() >= 15);
    for entry in gets {
        let target = entry["target"].as_str().unwrap();
        let (status, _) = rig.get(target).await;
        assert_eq!(status as u64, entry["status"].as_u64().unwrap(), "{target}");
    }
    // 上流には触れない
    assert!(rig.stub.seen().is_empty());
    // HEAD は現行でも静的のホワイトリストに当たらず 403
    let (status, _) = rig.request("HEAD", "/api/admin/geocode?q=ab").await;
    assert_eq!(status, 403);
    assert!(rig.stub.seen().is_empty());
}

// ---------------------------------------------------------------- Python との意図した差

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_over_the_limit_is_a_failure() {
    let rig = Rig::start().await;
    // 上限（1 MiB）を超える合法な JSON。地理院が超えたら失敗として Nominatim へ回り、Nominatim が超えたら
    // 空でキャッシュしない
    let big = format!("[{}]", " ".repeat(MAX_BODY_BYTES as usize + 1));
    rig.stub.set(Upstream::Gsi, Some(Reply::json(&big)));
    rig.stub.set(Upstream::Osm, Some(Reply::json(&big)));
    let (status, body) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    assert_eq!(rig.stub.count(Upstream::Gsi), 1);
    assert_eq!(rig.stub.count(Upstream::Osm), 1);
    assert!(rig.cache.snapshot().is_empty());

    // ちょうど上限に収まる地理院の応答は読める
    let mut ok = String::from(
        "[{\"geometry\":{\"coordinates\":[1.5,2.5]},\"properties\":{\"title\":\"t\"}}",
    );
    ok.push_str(&" ".repeat(MAX_BODY_BYTES as usize - ok.len() - 1));
    ok.push(']');
    assert_eq!(ok.len() as u64, MAX_BODY_BYTES);
    rig.stub.set(Upstream::Gsi, Some(Reply::json(&ok)));
    let (_, body) = rig.get_json("/api/admin/geocode?q=cd").await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numbers_json_cannot_hold_make_the_whole_body_invalid() {
    // Python は `NaN`・`Infinity`・`1e400` を読む（非有限の座標は Nominatim では範囲外で飛ばし、地理院では通す）。
    // Rust は本文全体が不正な JSON になり、失敗と同じ（server.md §18）
    for body in [
        r#"[{"geometry":{"coordinates":[NaN,1.5]}}]"#,
        r#"[{"geometry":{"coordinates":[Infinity,1.5]}}]"#,
        r#"[{"geometry":{"coordinates":[1e400,1.5]}}]"#,
    ] {
        let rig = Rig::start().await;
        rig.stub.set(Upstream::Gsi, Some(Reply::json(body)));
        rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
        let (status, got) = rig.get_json("/api/admin/geocode?q=ab").await;
        assert_eq!(
            (status, got),
            (200, json!({"ok": true, "items": []})),
            "{body}"
        );
        assert_eq!(rig.stub.count(Upstream::Osm), 1, "{body}");
    }
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1","lon":"2","display_name":"前"},{"lat":NaN,"lon":"2","display_name":"NaN"}]"#,
        )),
    );
    let (_, got) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!(got["items"], json!([]));
    assert!(rig.cache.snapshot().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_display_name_that_is_not_a_string_falls_back_to_the_query() {
    // Python は `str(display_name)`（`0`・`[]` などの偽の値は検索語）。Rust は文字列だけ（server.md §18）
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1","lon":"2","display_name":12},{"lat":"3","lon":"4","display_name":["x"]},
                {"lat":"5","lon":"6","display_name":"  名前  "}]"#,
        )),
    );
    let (_, got) = rig.get_json("/api/admin/geocode?q=ab").await;
    let titles: Vec<&str> = got["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["ab", "ab", "名前"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_searches_never_exceed_one_nominatim_request_per_second() {
    const N: usize = 6;
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    let mut osm = Reply::json("[]");
    osm.took = Duration::from_millis(250);
    rig.stub.set(Upstream::Osm, Some(osm));

    let handles: Vec<_> = (0..N)
        .map(|i| {
            let (geocoder, cache) = (Arc::clone(&rig.geocoder), Arc::clone(&rig.cache));
            std::thread::spawn(move || geocoder.search(&cache, &format!("語{i}")))
        })
        .collect();
    for handle in handles {
        assert!(handle.join().unwrap().is_empty());
    }
    let seen = rig.stub.seen();
    let osm_at: Vec<Duration> = seen
        .iter()
        .filter(|s| s.upstream == Upstream::Osm)
        .map(|s| s.at)
        .collect();
    assert_eq!(osm_at.len(), N);
    assert_eq!(seen.len(), 2 * N, "地理院は毎回引く");
    // 前の通信が終わってから（受けた時刻＋0.25 秒）1 秒以上あける。最初は待たない
    let mut sorted = osm_at.clone();
    sorted.sort();
    assert_eq!(sorted[0], START);
    for pair in sorted.windows(2) {
        assert!(
            pair[1] - pair[0] >= Duration::from_millis(1250),
            "{sorted:?}"
        );
    }
    let sleeps = rig.clock.take_sleeps();
    assert_eq!(sleeps.len(), N - 1);
    assert!(
        sleeps.iter().all(|s| *s == Duration::from_secs(1)),
        "{sleeps:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_searches_through_http_are_paced_too() {
    const N: usize = 5;
    let rig = Arc::new(Rig::start().await);
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
    let mut tasks = Vec::new();
    for i in 0..N {
        let rig = Arc::clone(&rig);
        tasks.push(tokio::spawn(async move {
            rig.get_json(&format!("/api/admin/geocode?q=word{i}")).await
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    }
    let mut osm_at: Vec<Duration> = rig
        .stub
        .seen()
        .iter()
        .filter(|s| s.upstream == Upstream::Osm)
        .map(|s| s.at)
        .collect();
    osm_at.sort();
    assert_eq!(osm_at.len(), N);
    for pair in osm_at.windows(2) {
        assert!(pair[1] - pair[0] >= Duration::from_secs(1), "{osm_at:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_word_searched_at_once_asks_nominatim_once() {
    const N: usize = 5;
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1.5","lon":"2.5","display_name":"合成の地名"}]"#,
        )),
    );
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let (geocoder, cache) = (Arc::clone(&rig.geocoder), Arc::clone(&rig.cache));
            std::thread::spawn(move || geocoder.search(&cache, "同じ語"))
        })
        .collect();
    let results: Vec<Vec<Value>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().all(|r| r == &results[0] && r.len() == 1));
    assert_eq!(
        rig.stub.count(Upstream::Osm),
        1,
        "待っている間にキャッシュに入った"
    );
    assert_eq!(rig.stub.count(Upstream::Gsi), N);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_number_of_searches_in_flight_is_capped() {
    let rig = Arc::new(Rig::start_with(1, Cache::memory_only()).await);
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
    // 最初の要求の地理院の応答を止める
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let arrived_tx = Mutex::new(arrived_tx);
    let gated = AtomicUsize::new(0);
    let gated = Arc::new(gated);
    {
        let gated = Arc::clone(&gated);
        rig.stub.set_hook(move |upstream| {
            if upstream == Upstream::Gsi && gated.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = arrived_tx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(());
                let _ = release_rx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(Duration::from_secs(20));
            }
        });
    }
    let first = {
        let rig = Arc::clone(&rig);
        tokio::spawn(async move { rig.get_json("/api/admin/geocode?q=ab").await })
    };
    tokio::task::spawn_blocking(move || arrived_rx.recv_timeout(Duration::from_secs(20)))
        .await
        .unwrap()
        .unwrap();
    // 空きが無い間の要求は待たせずに断る。上流には触れない。2 文字未満は空きを使わないので答える
    let (status, body) = rig.get_json("/api/admin/geocode?q=cd").await;
    assert_eq!(status, 503);
    assert_eq!(body["ok"], false);
    assert!(!body.to_string().contains("cd"), "{body}");
    assert_eq!(rig.stub.count(Upstream::Gsi), 1);
    let (status, body) = rig.get_json("/api/admin/geocode?q=a").await;
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    release_tx.send(()).unwrap();
    let (status, body) = first.await.unwrap();
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    // 終われば空きが戻る
    let (status, _) = rig.get_json("/api/admin/geocode?q=ef").await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cap_slot_is_released_when_the_client_goes_away() {
    let rig = Rig::start_with(1, Cache::memory_only()).await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
    let permit = rig.geocoder.try_reserve().unwrap();
    assert!(rig.geocoder.try_reserve().is_none());
    drop(permit);
    assert!(rig.geocoder.try_reserve().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_clock_waits_at_least_the_interval() {
    // 実時間の下限だけを見る（遅れて長くなっても落ちない）
    let clock = TestClock::new(START);
    let stub = StubUpstreams::start(clock);
    stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    stub.set(Upstream::Osm, Some(Reply::json("[]")));
    let mut geocoder = Geocoder::production();
    geocoder.endpoints = Endpoints::stub(&stub.base_url());
    geocoder.min_interval = Duration::from_millis(300);
    let cache = Cache::memory_only();
    let (geocoder, cache) = (Arc::new(geocoder), Arc::new(cache));
    tokio::task::spawn_blocking(move || {
        geocoder.search(&cache, "ab");
        let before = std::time::Instant::now();
        geocoder.search(&cache, "cd");
        assert!(
            before.elapsed() >= Duration::from_millis(250),
            "{:?}",
            before.elapsed()
        );
    })
    .await
    .unwrap();
}

// ---------------------------------------------------------------- ログ（AC7）

#[test]
fn failure_logs_never_contain_the_query_titles_or_coordinates() {
    let clock = TestClock::new(START);
    let stub = StubUpstreams::start(Arc::clone(&clock));
    let geocoder = Geocoder::with_parts(
        Endpoints::stub(&stub.base_url()),
        Box::new(SharedClock(Arc::clone(&clock))),
        MAX_IN_FLIGHT,
    );
    let cache = Cache::memory_only();
    let secrets = ["秘密の検索語", "秘密の地名", "31.4159", "27.1828"];
    let poisoned = r#"[{"geometry":{"coordinates":[27.1828,31.4159]},"properties":{"title":"秘密の地名"}},"text"]"#;
    let logs = capture::logs_of(|| {
        for (gsi, osm) in [
            (Reply::status(500), Reply::status(429)),
            (Reply::hangup(), Reply::hangup()),
            (
                Reply::json("not json 秘密の地名"),
                Reply::json("{\"error\":\"秘密の地名\"}"),
            ),
            (Reply::json(poisoned), Reply::json("[]")),
        ] {
            stub.set(Upstream::Gsi, Some(gsi));
            stub.set(Upstream::Osm, Some(osm));
            geocoder.search(&cache, secrets[0]);
            clock.advance(Duration::from_secs(5));
        }
        // 成功しても出さない
        stub.set(
            Upstream::Gsi,
            Some(Reply::json(
                r#"[{"geometry":{"coordinates":[27.1828,31.4159]},"properties":{"title":"秘密の地名"}}]"#,
            )),
        );
        let items = geocoder.search(&cache, secrets[0]);
        assert_eq!(items.len(), 1);
    });
    assert!(logs.contains("地名検索失敗（地理院）"), "{logs}");
    assert!(logs.contains("地名検索失敗（Nominatim）"), "{logs}");
    // 出してよいのは失敗の種類と状態コードだけ
    assert!(
        logs.contains("http 500") && logs.contains("http 429"),
        "{logs}"
    );
    assert!(
        logs.contains("disconnected") && logs.contains("bad_response"),
        "{logs}"
    );
    for secret in secrets {
        assert!(!logs.contains(secret), "{secret}: {logs}");
    }
    assert!(
        !logs.contains("127.0.0.1") && !logs.contains("http://"),
        "{logs}"
    );
}

// ---------------------------------------------------------------- .cache.json（取り込みの有効・無効）

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_ingest_off_the_cache_stays_in_memory_and_the_file_is_untouched() {
    let dir = TempDir::new("geocode-cache-off");
    let original = r#"{"geo:1.000,2.000": {"place": "合成"}}"#;
    fs::write(dir.path().join(".cache.json"), original).unwrap();
    let rig = Rig::start_with(MAX_IN_FLIGHT, Cache::load_memory_only(dir.path())).await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1.5","lon":"2.5","display_name":"合成の地名"}]"#,
        )),
    );
    let (_, first) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    // 同じ語はメモリのキャッシュから（Nominatim は 1 回だけ）
    let (_, second) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!(second, first);
    assert_eq!(rig.stub.count(Upstream::Osm), 1);
    assert!(rig.cache.get("geocode_osm:ab").is_some());
    // 既にあったキーも読めて、ファイルには何も書かない
    assert!(rig.cache.get("geo:1.000,2.000").is_some());
    assert_eq!(rig.cache.save_now(), SaveOutcome::Clean);
    assert_eq!(
        fs::read_to_string(dir.path().join(".cache.json")).unwrap(),
        original
    );
    assert!(!dir.path().join(".cache.json.tmp").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_ingest_on_the_cache_is_shared_with_the_sync_cycle_and_saved() {
    let dir = TempDir::new("geocode-cache-on");
    let rig = Rig::start_with(MAX_IN_FLIGHT, Cache::load(dir.path())).await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1.5","lon":"2.5","display_name":"合成の地名"}]"#,
        )),
    );
    // 同期サイクルの enrich が `geo:` を書き続けている間に検索しても、互いの更新を失わない
    let writer = {
        let cache = Arc::clone(&rig.cache);
        std::thread::spawn(move || {
            for i in 0..200 {
                cache.put(&format!("geo:{i}.000,0.000"), json!({"place": "合成"}));
                if i % 50 == 0 {
                    cache.save_now();
                }
            }
        })
    };
    let (status, body) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!(status, 200);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    writer.join().unwrap();
    assert_eq!(rig.cache.save_now(), SaveOutcome::Saved);
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".cache.json")).unwrap()).unwrap();
    let map = saved.as_object().unwrap();
    assert_eq!(map.len(), 201, "geo: 200 件と geocode_osm: 1 件");
    assert_eq!(
        map["geocode_osm:ab"],
        json!([{"title": "合成の地名", "lat": 1.5, "lon": 2.5, "source": "osm"}])
    );
    // 再起動後の読み込みで当たる（保存したものは起動時の旧キーの破棄で消えない）
    let reloaded = Cache::load(dir.path());
    assert_eq!(reloaded.purge_legacy(), 0);
    assert!(reloaded.get("geocode_osm:ab").is_some());
}
