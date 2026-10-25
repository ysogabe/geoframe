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
use crate::config::{Config, DEFAULT_TILE_UA, Overrides};
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
        gzip: false,
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

/// 本文の上限は、gzip の本文では圧縮後でなく**展開後**の大きさに効く（圧縮後は数 KB でも、展開後が 1 MiB を超えれば
/// 失敗）。上限ちょうどは読める。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_body_limit_counts_the_decompressed_size() {
    let item = "[{\"geometry\":{\"coordinates\":[1.5,2.5]},\"properties\":{\"title\":\"t\"}}]";
    let max = MAX_BODY_BYTES as usize;
    let at_limit = Reply::gzip_padded(item, max - item.len());
    let over_limit = Reply::gzip_padded(item, max - item.len() + 1);
    assert!(
        over_limit.body.len() < 20_000,
        "圧縮後は小さい本文で試す: {}",
        over_limit.body.len()
    );

    // 地理院: 超えたら失敗として Nominatim へ回る。ちょうど上限なら読めて、Nominatim へは回らない
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(over_limit.clone()));
    rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
    let (status, body) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    assert_eq!(rig.stub.count(Upstream::Osm), 1);
    rig.stub.set(Upstream::Gsi, Some(at_limit.clone()));
    let (_, body) = rig.get_json("/api/admin/geocode?q=cd").await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(rig.stub.count(Upstream::Osm), 1);

    // Nominatim: 超えたら空でキャッシュしない。ちょうど上限なら読める（空の配列はキャッシュする）
    let rig = Rig::start().await;
    rig.stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    rig.stub.set(Upstream::Osm, Some(over_limit));
    let (status, body) = rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    assert!(rig.cache.snapshot().is_empty());
    rig.stub
        .set(Upstream::Osm, Some(Reply::gzip_padded("[]", max - 2)));
    let _ = at_limit;
    rig.get_json("/api/admin/geocode?q=ab").await;
    assert_eq!(
        rig.cache.snapshot().len(),
        1,
        "上限ちょうどの本文は読めて、結果をキャッシュする"
    );
}

/// 検索語は前後の空白を落としたあと 200 文字まで。超えたら上流へ問い合わせずに空を返す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_over_the_length_cap_never_reaches_upstream() {
    let rig = Rig::start().await;
    rig.stub.set(
        Upstream::Gsi,
        Some(Reply::json(
            r#"[{"geometry":{"coordinates":[1.5,2.5]},"properties":{"title":"t"}}]"#,
        )),
    );
    rig.stub.set(Upstream::Osm, Some(Reply::json("[]")));
    // 上限ちょうど（空白は数えない）は検索する
    let at = "a".repeat(MAX_QUERY_LEN);
    let (_, body) = rig
        .get_json(&format!("/api/admin/geocode?q=%20{at}%20"))
        .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(rig.stub.count(Upstream::Gsi), 1);
    // 1 文字超えたら、どちらの上流にも触れない
    let over = "a".repeat(MAX_QUERY_LEN + 1);
    let (status, body) = rig.get_json(&format!("/api/admin/geocode?q={over}")).await;
    assert_eq!((status, body), (200, json!({"ok": true, "items": []})));
    assert_eq!(rig.stub.count(Upstream::Gsi), 1);
    assert_eq!(rig.stub.count(Upstream::Osm), 0);
    // 数えるのはコードポイント（バイトではない）
    assert_eq!(
        clean_query(&"あ".repeat(MAX_QUERY_LEN)).map(str::len),
        Some(MAX_QUERY_LEN * 3)
    );
    assert_eq!(clean_query(&"あ".repeat(MAX_QUERY_LEN + 1)), None);
}

/// `geocode_osm:` のキャッシュは件数に上限があり、満杯のあとの新しい検索語は結果を返すが保存しない。
/// 既に入っている語は当たり続け、`geo:` など、ほかのキーには触れない。
#[test]
fn the_geocode_cache_is_capped_without_touching_other_keys() {
    let clock = TestClock::new(START);
    let stub = StubUpstreams::start(Arc::clone(&clock));
    let mut geocoder = Geocoder::with_parts(
        Endpoints::stub(&stub.base_url()),
        Box::new(SharedClock(Arc::clone(&clock))),
        MAX_IN_FLIGHT,
    );
    geocoder.cache_cap = 2;
    let cache = Cache::memory_only();
    cache.put("geo:10.000,20.000", json!({"place": "テスト"}));
    stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    stub.set(
        Upstream::Osm,
        Some(Reply::json(
            r#"[{"lat":"1.5","lon":"2.5","display_name":"t"}]"#,
        )),
    );
    for q in ["aa", "bb"] {
        assert_eq!(geocoder.search(&cache, q).len(), 1);
    }
    assert_eq!(cache.len(), 3);
    // 満杯: 結果は返すが、保存しない（毎回問い合わせる）
    assert_eq!(geocoder.search(&cache, "cc").len(), 1);
    assert_eq!(geocoder.search(&cache, "cc").len(), 1);
    assert_eq!(cache.len(), 3);
    assert_eq!(stub.count(Upstream::Osm), 4);
    // 入っている語は当たる（問い合わせない）。ほかの名前空間は残る
    assert_eq!(geocoder.search(&cache, "aa").len(), 1);
    assert_eq!(stub.count(Upstream::Osm), 4);
    assert!(cache.get("geo:10.000,20.000").is_some());
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

/// 実時計（`SystemClock`）でも、続けた Nominatim の 2 回の要求が間隔以上あく。スタブが**要求を受けた実際の時刻**
/// の差を見る（`search` の呼び出しの前後を測らない: 1 回目の通信が終わってから測り始めるまでの遅れや、遅い環境で
/// の揺れに左右されない）。間隔は 1 回目の通信が**終わった**時刻から数えるので、受信の差はそれ以上になる。
/// 上限は見ない（遅れて長くなっても落ちない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_clock_keeps_the_interval_between_received_requests() {
    let clock = TestClock::new(START);
    let stub = StubUpstreams::start(clock);
    stub.set(Upstream::Gsi, Some(Reply::json("[]")));
    stub.set(Upstream::Osm, Some(Reply::json("[]")));
    let received = Arc::new(Mutex::new(Vec::new()));
    {
        let received = Arc::clone(&received);
        stub.set_hook(move |upstream| {
            if upstream == Upstream::Osm {
                received
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(std::time::Instant::now());
            }
        });
    }
    let interval = Duration::from_millis(300);
    let mut geocoder = Geocoder::production(DEFAULT_TILE_UA);
    geocoder.endpoints = Endpoints::stub(&stub.base_url());
    geocoder.min_interval = interval;
    let cache = Cache::memory_only();
    let (geocoder, cache) = (Arc::new(geocoder), Arc::new(cache));
    tokio::task::spawn_blocking(move || {
        geocoder.search(&cache, "ab");
        geocoder.search(&cache, "cd");
    })
    .await
    .unwrap();
    let received = received.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(received.len(), 2);
    let gap = received[1].duration_since(received[0]);
    assert!(gap >= interval, "{gap:?}");
}

/// 本番のコードは、地名検索の通信先を差し替える入口（`Endpoints::loopback`・`Geocoder::with_endpoints`・
/// `AppState::with_geocoder`）と、地図タイルの通信先を差し替える入口（`TileEndpoints::loopback`・
/// `TileProxy::with_endpoints`・`AppState::with_tiles`）を呼ばない。設定・環境変数・コマンドラインから通信先を変えられないことの確認
/// （入口は結合テストのために `pub` だが、本番のバイナリからは使われない）。
///
/// 入口を定義するファイル（`geocode.rs`・`server.rs`）も含め、すべてのファイルを調べる。外すのは、
/// コメントの行・入口の**定義の行**（`fn <入口>(`）・`#[cfg(test)]` の付いた項目（試験のビルドにしかない）・
/// 試験のファイル（`tests.rs`・`stub.rs`。どちらも `cfg(test)` のモジュール）だけ。
#[test]
fn production_code_never_redirects_the_geocoder() {
    // 地名検索（`Endpoints`・`with_geocoder`）と地図タイル（`TileEndpoints`・`with_tiles`）の入口。`Endpoints::…` は
    // `TileEndpoints::…` にも当たる
    const ENTRIES: [(&str, &str); 5] = [
        ("Endpoints::loopback", "loopback"),
        ("Endpoints::stub", "stub"),
        ("with_endpoints(", "with_endpoints"),
        ("with_geocoder(", "with_geocoder"),
        ("with_tiles(", "with_tiles"),
    ];

    /// `#[cfg(test)]` の付いた項目（同じ字下げの `}` か、`;`・`,` で終わる行まで）と、コメントの行を除く。
    fn production_lines(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut skipping_from: Option<usize> = None;
        for line in text.lines() {
            let indent = line.len() - line.trim_start().len();
            let trimmed = line.trim();
            if let Some(attr_indent) = skipping_from {
                // 属性の次の行から、項目の終わりまで
                if indent == attr_indent
                    && (trimmed.starts_with('}')
                        || trimmed.ends_with(';')
                        || trimmed.ends_with(','))
                {
                    skipping_from = None;
                }
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                skipping_from = Some(indent);
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            out.push(line);
        }
        out
    }

    /// 入口の定義の行（`pub fn with_geocoder(` など）か。呼び出しは含まない。
    fn defines(line: &str, name: &str) -> bool {
        line.find("fn ")
            .is_some_and(|at| line[at + 3..].trim_start().starts_with(name))
    }

    /// 本番の行から、入口の使用を探す。
    fn uses(text: &str) -> Vec<&'static str> {
        let lines = production_lines(text);
        ENTRIES
            .iter()
            .filter(|(needle, name)| {
                lines
                    .iter()
                    .any(|line| line.contains(needle) && !defines(line, name))
            })
            .map(|(needle, _)| *needle)
            .collect()
    }

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    // 走査そのものの確認: 本番の行に入口の呼び出しを足せば見つかり、定義・コメント・`cfg(test)` の中では見つからない
    let injected = "fn run() {\n    let g = Geocoder::with_endpoints(Endpoints::loopback(a));\n}\n";
    assert_eq!(uses(injected), ["Endpoints::loopback", "with_endpoints("]);
    let allowed = "\
        pub fn with_geocoder(mut self) -> Self {\n    self\n}\n\
        // Endpoints::loopback で向ける\n\
        #[cfg(test)]\n\
        mod tests {\n    fn f() { let _ = Endpoints::stub(b); }\n}\n\
        impl X {\n    #[cfg(test)]\n    pub(crate) fn stub(b: &str) -> Self {\n        Self::at(b)\n    }\n}\n";
    assert!(uses(allowed).is_empty(), "{:?}", uses(allowed));
    // 試験の項目のあとに本番の呼び出しがあれば、見つかる
    let after = "#[cfg(test)]\nmod tests;\nfn run() {\n    with_geocoder(g);\n}\n";
    assert_eq!(uses(after), ["with_geocoder("]);
    // 地図タイルの入口も同じ
    let tiles = "fn run() {\n    let p = TileProxy::with_endpoints(c, d, TileEndpoints::loopback(a));\n    s.with_tiles(p);\n}\n";
    assert_eq!(
        uses(tiles),
        ["Endpoints::loopback", "with_endpoints(", "with_tiles("]
    );

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    assert!(files.len() > 20);
    let mut users = Vec::new();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // 試験のファイル（`#[cfg(test)] mod tests;`・`#[cfg(test)] pub(crate) mod stub;` で入るモジュール）
        if ["tests.rs", "stub.rs"].contains(&name.as_str()) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for needle in uses(&text) {
            users.push(format!("{name}: {needle}"));
        }
    }
    assert!(users.is_empty(), "{users:?}");
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
    )
    .with_user_agent("GeoFrame-Test/1 (secret-contact.test)");
    let cache = Cache::memory_only();
    let secrets = [
        "秘密の検索語",
        "秘密の地名",
        "31.4159",
        "27.1828",
        "GeoFrame-Test",
        "secret-contact",
    ];
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

// ---------------------------------------------------------------- User-Agent（`GEOFRAME_TILE_UA`と共用）

/// 本番の組み立て（`AppState::new`）は、設定の `GEOFRAME_TILE_UA` を地名検索にも渡す。未設定なら地図タイルと同じ既定。
#[test]
fn the_app_state_hands_the_configured_tile_user_agent_to_the_geocoder() {
    let dir = TempDir::new("geocode-ua");
    let (path, _writer) = synthetic_catalog(dir.path(), 0);
    let state_with = |env: &'static [(&str, &str)]| {
        let config = Config::load(
            |k| {
                env.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| (*v).to_string())
            },
            &Overrides::default(),
        )
        .unwrap();
        AppState::new(Arc::new(config), Catalog::open(&path).unwrap())
    };
    let state = state_with(&[("GEOFRAME_TILE_UA", " GeoFrame-Test/1 (synthetic contact) ")]);
    assert_eq!(
        state.geocoder.user_agent(),
        "GeoFrame-Test/1 (synthetic contact)"
    );
    assert_eq!(state_with(&[]).geocoder.user_agent(), DEFAULT_TILE_UA);
    assert_eq!(
        state_with(&[("GEOFRAME_TILE_UA", "")])
            .geocoder
            .user_agent(),
        DEFAULT_TILE_UA
    );
}

/// 地理院にも Nominatim にも、設定した User-Agent が届く。通信先を替える入口は既定（`GeoFrame/0.2`）のまま。
#[test]
fn the_configured_user_agent_reaches_both_upstreams() {
    let ua = "GeoFrame-Test/1 (synthetic contact)";
    let clock = TestClock::new(START);
    let stub = StubUpstreams::start(Arc::clone(&clock));
    let parts = || {
        Geocoder::with_parts(
            Endpoints::stub(&stub.base_url()),
            Box::new(SharedClock(Arc::clone(&clock))),
            MAX_IN_FLIGHT,
        )
    };
    let search = |geocoder: &Geocoder| {
        stub.set(Upstream::Gsi, Some(Reply::json("[]")));
        stub.set(Upstream::Osm, Some(Reply::json("[]")));
        geocoder.search(&Cache::memory_only(), "ab");
        stub.take_seen()
            .into_iter()
            .map(|s| (s.upstream, s.user_agent))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        search(&parts()),
        [
            (Upstream::Gsi, USER_AGENT.into()),
            (Upstream::Osm, USER_AGENT.into())
        ]
    );
    assert_eq!(
        search(&parts().with_user_agent(ua)),
        [(Upstream::Gsi, ua.into()), (Upstream::Osm, ua.into())]
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
