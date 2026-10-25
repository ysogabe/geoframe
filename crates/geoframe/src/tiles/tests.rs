//! 地図タイルのプロキシ（E11）と掃除（B4）の試験。
//!
//! - フィクスチャ（`tests/fixtures/tile-python.json`。`tools/gen_tile_fixture.py` が現行 Python に流して作る）と
//!   同じ手順を、同じ応答を返すループバックのスタブ・同じ偽の時計で流して、状態コード・ヘッダ・本文・上流への要求・
//!   キャッシュのファイルが一致すること。掃除も、偽のファイル群で残るファイルが一致すること。
//! - Python と意図して違える点（本文の上限、gzip、壊れた/リンクのキャッシュ、同時数、掃除のスレッドなど）。
//!
//! 実時間の `sleep` で進み具合を推測しない。待つ試験は、条件が満たされるまで期限つきで待つ。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::stub::{Reply, StubUpstream, hex_decode, hex_encode};
use super::*;
use crate::config::Overrides;
use crate::db::Catalog;
use crate::server::{AppState, router};
use crate::testutil::{TempDir, capture, synthetic_catalog};

const FIXTURE: &str = include_str!("../../tests/fixtures/tile-python.json");
const START: Duration = Duration::from_secs(1000);

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    hex_encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

// ---------------------------------------------------------------- 部品

/// 偽の時計。
struct TestClock(Mutex<Duration>);

impl TestClock {
    fn new(start: Duration) -> Arc<Self> {
        Arc::new(Self(Mutex::new(start)))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

struct SharedClock(Arc<TestClock>);

impl Clock for SharedClock {
    fn now(&self) -> Duration {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn config(dir: &Path, env: &[(&str, &str)]) -> Config {
    Config::load(
        |key| {
            env.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        },
        &Overrides {
            data_dir: Some(dir.to_path_buf()),
            ..Overrides::default()
        },
    )
    .unwrap()
}

/// スタブの上流・偽の時計・ルーターを持つ試験の環境。取り込みは無効（書き込み用の接続を持たない）。
struct Harness {
    dir: TempDir,
    stub: StubUpstream,
    clock: Arc<TestClock>,
    proxy: Arc<TileProxy>,
    addr: SocketAddr,
    state: AppState,
    _writer: rusqlite::Connection,
}

impl Harness {
    async fn new(env: &[(&str, &str)]) -> Self {
        Self::with_timeout(env, None).await
    }

    async fn with_timeout(env: &[(&str, &str)], timeout: Option<Duration>) -> Self {
        let dir = TempDir::new("tiles");
        let (db, writer) = synthetic_catalog(dir.path(), 0);
        let mut env = env.to_vec();
        if !env.iter().any(|(k, _)| *k == "GEOFRAME_TILE_UA") {
            env.push(("GEOFRAME_TILE_UA", "GeoFrame-Test/1.0 (synthetic contact)"));
        }
        let config = config(dir.path(), &env);
        let stub = StubUpstream::start();
        let clock = TestClock::new(START);
        let mut proxy = TileProxy::with_parts(
            &config,
            dir.path(),
            stub.endpoints(),
            Box::new(SharedClock(Arc::clone(&clock))),
        );
        if let Some(timeout) = timeout {
            proxy.timeout = timeout;
        }
        let proxy = Arc::new(proxy);
        let state = AppState::new(Arc::new(config), Catalog::open(&db).unwrap())
            .with_geocoder(crate::geocode::stub::deny_geocoder())
            .with_tiles(Arc::clone(&proxy));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            dir,
            stub,
            clock,
            proxy,
            addr,
            state,
            _writer: writer,
        }
    }

    fn cache_root(&self) -> PathBuf {
        self.dir.path().join("tile_cache")
    }

    fn tile_file(&self, path: &str) -> PathBuf {
        self.cache_root().join(path)
    }

    async fn send(&self, method: &str, target: &str) -> Raw {
        send(self.addr, method, target).await
    }
}

struct Raw {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Raw {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

async fn send(addr: SocketAddr, method: &str, target: &str) -> Raw {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("ヘッダの終わり");
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    Raw {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

/// `dir` の下のファイルを、相対パス → SHA-256 にする。
fn listing(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_symlink() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                out.insert(rel, "symlink".to_string());
            } else if kind.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                // 並行して消されたファイルは、無かったことにする
                if let Ok(bytes) = fs::read(&path) {
                    out.insert(rel, sha256(&bytes));
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn sample_png() -> Vec<u8> {
    let mut png = PNG_MAGIC.to_vec();
    png.extend_from_slice(b"synthetic tile body");
    png
}

fn png_with(pad: usize) -> Vec<u8> {
    let mut png = PNG_MAGIC.to_vec();
    png.resize(PNG_MAGIC.len() + pad, 0);
    png
}

// ---------------------------------------------------------------- フィクスチャ（Python との一致）

#[test]
fn python_fixture_constants_match() {
    let doc = fixture();
    let c = &doc["constants"];
    // 透明 PNG はバイト列まで一致する
    assert_eq!(
        hex_encode(TRANSPARENT_TILE),
        c["blank_png_hex"].as_str().unwrap()
    );
    assert_eq!(
        sha256(TRANSPARENT_TILE),
        c["blank_png_sha256"].as_str().unwrap()
    );
    assert_eq!(
        crate::config::DEFAULT_TILE_UA,
        c["default_ua"].as_str().unwrap()
    );
    assert_eq!(
        DEFAULT_FETCH_PER_MIN,
        c["default_per_min"].as_i64().unwrap()
    );
    assert_eq!(
        crate::config::DEFAULT_TILE_CACHE_MAX_MB,
        c["default_cache_max_mb"].as_i64().unwrap()
    );
    assert_eq!(UPSTREAM_TIMEOUT.as_secs(), c["timeout"].as_u64().unwrap());
    assert_eq!(WINDOW.as_secs_f64(), c["window_seconds"].as_f64().unwrap());
    assert_eq!(
        PRUNE_INTERVAL.as_secs(),
        c["prune_interval"].as_u64().unwrap()
    );
    for layer in Layer::ALL {
        assert_eq!(
            u64::from(layer.max_zoom()),
            c["max_zoom"][layer.name()].as_u64().unwrap()
        );
        assert_eq!(
            layer.concurrency() as u64,
            c["concurrency"][layer.name()].as_u64().unwrap()
        );
    }
    // 本番の URL
    let production = TileEndpoints::production();
    for u in c["urls"].as_array().unwrap() {
        let tile = Tile {
            layer: Layer::parse(u["layer"].as_str().unwrap()).unwrap(),
            z: u["z"].as_u64().unwrap() as u32,
            x: u["x"].as_u64().unwrap(),
            y: u["y"].as_u64().unwrap(),
        };
        assert_eq!(production.url(tile), u["url"].as_str().unwrap());
    }
}

/// フィクスチャの 1 手順を流して、Python の記録と比べる。
async fn run_step(h: &Harness, case: &str, index: usize, step: &Value) {
    let ctx = format!("{case}[{index}] {} {}", step["method"], step["target"]);
    for seed in step["seed"].as_array().into_iter().flatten() {
        let path = h.tile_file(seed["path"].as_str().unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, hex_decode(seed["body_hex"].as_str().unwrap())).unwrap();
    }
    if let Some(advance) = step["advance"].as_f64() {
        h.clock.advance(Duration::from_secs_f64(advance));
    }
    h.stub.script(
        step["replies"]
            .as_array()
            .into_iter()
            .flatten()
            .map(Reply::from_json)
            .collect(),
    );
    h.stub.take_seen();
    let response = h
        .send(
            step["method"].as_str().unwrap(),
            step["target"].as_str().unwrap(),
        )
        .await;

    assert_eq!(
        response.status as u64,
        step["status"].as_u64().unwrap(),
        "{ctx}"
    );
    // ヘッダ: Python の応答のヘッダ（`Date`・`Server` を除く）と、名前も値も一致する。`connection: close` は
    // Python の `send_error` が付けるもので、本文の扱い（HTTP/1.0 の接続）に由来する
    let mut expected: BTreeMap<String, String> = step["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    expected.remove("connection");
    let mut actual = response.headers.clone();
    actual.remove("date");
    actual.remove("connection");
    assert_eq!(actual, expected, "{ctx}");
    // HEAD は本文なしで、Content-Length だけが GET と同じ
    assert_eq!(
        response.body.len() as u64,
        step["body_len"].as_u64().unwrap(),
        "{ctx}"
    );
    if step["method"] == "GET" {
        assert_eq!(
            sha256(&response.body),
            step["body_sha256"].as_str().unwrap(),
            "{ctx}"
        );
    } else {
        assert!(response.body.is_empty(), "{ctx}");
    }
    if let Some(text) = step["body_text"].as_str()
        && step["method"] == "GET"
    {
        assert_eq!(std::str::from_utf8(&response.body).unwrap(), text, "{ctx}");
    }

    // 上流への要求: 順序・パス・User-Agent・Accept・Accept-Encoding が一致し、余分なヘッダを送らない
    let seen = h.stub.take_seen();
    let expected_requests = step["requests"].as_array().unwrap();
    assert_eq!(seen.len(), expected_requests.len(), "{ctx}: {seen:?}");
    for (got, want) in seen.iter().zip(expected_requests) {
        assert_eq!(got.path, want["path"].as_str().unwrap(), "{ctx}");
        for name in ["user-agent", "accept", "accept-encoding"] {
            assert_eq!(
                got.headers.get(name).map(String::as_str),
                want["headers"][name].as_str(),
                "{ctx}: {name}"
            );
        }
        // Python の urllib は `Connection: close` も付ける（接続を使い回さない印）。それ以外は同じ名前だけ
        let mut names: Vec<_> = got.headers.keys().cloned().collect();
        names.retain(|n| n != "connection");
        let mut want_names: Vec<_> = want["headers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        want_names.retain(|n| n != "connection");
        assert_eq!(names, want_names, "{ctx}: 上流へ送るヘッダの名前");
    }
    assert_eq!(
        h.stub.unused_replies() as u64,
        step["unused_replies"].as_u64().unwrap(),
        "{ctx}: 使われなかった応答"
    );
    assert!(h.stub.unscripted().is_empty(), "{ctx}: 想定外の要求");

    // キャッシュのファイル（一時ファイルが残っていないことも含む）
    let want: BTreeMap<String, String> = step["cache"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    assert_eq!(listing(&h.cache_root()), want, "{ctx}: キャッシュ");
}

#[tokio::test(flavor = "multi_thread")]
async fn python_fixture_cases_match() {
    let doc = fixture();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 18);
    let mut steps = 0;
    for case in cases {
        let per_min = case["per_min"].as_i64().unwrap_or(240).to_string();
        let h = Harness::new(&[("GEOFRAME_TILE_FETCH_PER_MIN", &per_min)]).await;
        // 「ここは Python と同じ」を、取り込み無効（書き込み用の接続なし）のまま確かめる
        assert!(h.state.writer().is_none());
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            run_step(&h, case["name"].as_str().unwrap(), i, step).await;
            steps += 1;
        }
    }
    assert!(steps >= 170, "{steps}");
}

fn make_files(root: &Path, files: &[Value]) {
    for f in files {
        let path = root.join(f["path"].as_str().unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 開いたままの手で mtime まで決める（掃除のスレッドが動いていて、書いた直後に消されても失敗しない）
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&vec![0u8; f["size"].as_u64().unwrap() as usize])
            .unwrap();
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(f["mtime"].as_u64().unwrap());
        file.set_modified(mtime).unwrap();
    }
}

fn pngs_under(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = listing(root)
        .into_keys()
        .filter(|p| p.ends_with(".png"))
        .collect();
    out.sort();
    out
}

#[test]
fn python_fixture_prune_cases_match() {
    let doc = fixture();
    for case in doc["prune_cases"].as_array().unwrap() {
        let dir = TempDir::new("prune-fixture");
        let root = dir.path().join("tile_cache");
        fs::create_dir_all(&root).unwrap();
        make_files(&root, case["files"].as_array().unwrap());
        let report = prune_dir(&root, case["max_mb"].as_i64().unwrap(), &|| false).unwrap();
        let want: Vec<String> = case["survivors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(pngs_under(&root), want, "{}", case["name"]);
        assert_eq!(
            report.files,
            case["files"].as_array().unwrap().len(),
            "{}",
            case["name"]
        );
        assert_eq!(
            report.deleted,
            report.files - want.len(),
            "{}",
            case["name"]
        );
    }
}

// ---------------------------------------------------------------- パスの形

#[test]
fn parse_path_follows_the_python_regex_and_range_checks() {
    let t = |layer, z, x, y| Tile { layer, z, x, y };
    assert_eq!(
        parse_path("/tiles/osm/3/1/2.png"),
        Some(t(Layer::Osm, 3, 1, 2))
    );
    assert_eq!(
        parse_path("/tiles/gsi/018/0/0.png"),
        None,
        "ズームは 2 桁まで"
    );
    assert_eq!(
        parse_path("/tiles/gsi/08/0007/00.png"),
        Some(t(Layer::Gsi, 8, 7, 0))
    );
    // 範囲とズーム
    assert_eq!(parse_path("/tiles/osm/3/8/0.png"), None);
    assert_eq!(parse_path("/tiles/osm/20/0/0.png"), None);
    assert!(parse_path("/tiles/osm/19/524287/524287.png").is_some());
    assert_eq!(parse_path("/tiles/gsi/19/0/0.png"), None);
    // 桁あふれ・大きすぎるズームでもパニックしない（2^z が u64 を超える z = 64〜99 を含む）
    for z in 0..=99 {
        for layer in ["osm", "gsi"] {
            let _ = parse_path(&format!("/tiles/{layer}/{z:02}/0/0.png"));
            let _ = parse_path(&format!("/tiles/{layer}/{z}/99999999999999999999999/0.png"));
        }
    }
    assert_eq!(
        parse_path(&format!("/tiles/osm/3/0/{}.png", "9".repeat(5000))),
        None
    );
    // 桁数の上限は Python の `int()` と同じ（先頭ゼロも数える。4300 桁は通り、4301 桁は 404）
    let zeros = |n: usize| "0".repeat(n);
    let seven = |n: usize| format!("{}7", "0".repeat(n - 1));
    assert_eq!(
        parse_path(&format!("/tiles/osm/3/{}/{}.png", zeros(4300), seven(4300))),
        Some(t(Layer::Osm, 3, 0, 7))
    );
    assert_eq!(
        parse_path(&format!("/tiles/osm/3/{}/0.png", zeros(4301))),
        None
    );
    assert_eq!(
        parse_path(&format!("/tiles/osm/3/0/{}.png", zeros(4301))),
        None
    );
    assert_eq!(
        parse_path(&format!("/tiles/osm/3/{}/0.png", seven(4301))),
        None
    );
    for bad in [
        "/tiles/osm/1/0/0.png/",
        "/tiles/osm/1/0/0.PNG",
        "/tiles/OSM/1/0/0.png",
        "/tiles/osm/1/0/0.png\n",
        "/tiles/osm/1/0/ 0.png",
        "/tiles/osm/+1/0/0.png",
        "/tiles/osm/١/0/0.png",
        "/tiles/osm/1/0/٠.png",
        "tiles/osm/1/0/0.png",
        "/Tiles/osm/1/0/0.png",
    ] {
        assert_eq!(parse_path(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------------- 流量の窓

#[test]
fn the_window_allows_per_min_and_slides_with_a_fake_clock() {
    let clock = TestClock::new(START);
    let window = FetchWindow::new(240, Box::new(SharedClock(Arc::clone(&clock))));
    // 240 枚ちょうどまで通り、241 枚目から止まる
    assert!((0..240).all(|_| window.allow()));
    assert!(!window.allow());
    assert!(!window.allow());
    // 59.999 秒では動かず、60 秒ちょうどで全部の記録が窓から出る
    clock.advance(Duration::from_millis(59_999));
    assert!(!window.allow());
    clock.advance(Duration::from_millis(1));
    assert!((0..240).all(|_| window.allow()));
    assert!(!window.allow());
}

#[test]
fn the_window_never_lets_more_than_the_limit_through_concurrently() {
    let clock = TestClock::new(START);
    let window = Arc::new(FetchWindow::new(
        240,
        Box::new(SharedClock(Arc::clone(&clock))),
    ));
    let allowed: usize = (0..8)
        .map(|_| {
            let window = Arc::clone(&window);
            std::thread::spawn(move || (0..500).filter(|_| window.allow()).count())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .sum();
    assert_eq!(allowed, 240);
}

#[test]
fn a_negative_or_zero_limit_never_allows() {
    let clock = TestClock::new(START);
    for per_min in [0, -1, i64::MIN] {
        let window = FetchWindow::new(per_min, Box::new(SharedClock(Arc::clone(&clock))));
        assert!(!window.allow(), "{per_min}");
    }
    let huge = FetchWindow::new(i64::MAX, Box::new(SharedClock(clock)));
    assert!((0..1000).all(|_| huge.allow()));
}

// ---------------------------------------------------------------- Python と違える点

#[tokio::test(flavor = "multi_thread")]
async fn tiles_work_without_a_writer_and_do_not_touch_the_database() {
    let h = Harness::new(&[]).await;
    assert!(h.state.writer().is_none());
    h.stub.always(Reply::png(&sample_png()));
    let before = fs::read(h.dir.path().join("catalog.db")).unwrap();
    let r = h.send("GET", "/tiles/osm/3/1/2.png").await;
    assert_eq!((r.status, r.header("x-tile-cache")), (200, "miss"));
    assert!(h.tile_file("osm/3/1/2.png").is_file());
    let r = h.send("GET", "/tiles/osm/3/1/2.png").await;
    assert_eq!(r.header("x-tile-cache"), "hit");
    // DB のファイルは変わらない（書き込み用の接続が無く、タイルは DB に触れない）
    assert_eq!(fs::read(h.dir.path().join("catalog.db")).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_upstream_body_is_capped_after_decompression() {
    let h = Harness::new(&[]).await;
    let max = MAX_TILE_BYTES as usize;
    // ちょうど上限（マジックを含む）は通り、1 バイト超えると失敗（保存しない）
    h.stub
        .script(vec![Reply::png(&png_with(max - PNG_MAGIC.len()))]);
    let r = h.send("GET", "/tiles/osm/5/1/1.png").await;
    assert_eq!((r.header("x-tile-cache"), r.body.len()), ("miss", max));
    h.stub
        .script(vec![Reply::png(&png_with(max - PNG_MAGIC.len() + 1))]);
    let r = h.send("GET", "/tiles/osm/5/1/2.png").await;
    assert_eq!(r.header("x-tile-cache"), "error");
    assert_eq!(r.body, TRANSPARENT_TILE);
    assert!(!h.tile_file("osm/5/1/2.png").exists());
    // gzip: 圧縮後は小さくても、展開後が上限を超えれば失敗する
    let bomb = Reply::gzip_png(max * 4);
    assert!(bomb.body.len() < 100_000, "{}", bomb.body.len());
    h.stub.script(vec![bomb]);
    let r = h.send("GET", "/tiles/osm/5/1/3.png").await;
    assert_eq!(r.header("x-tile-cache"), "error");
    assert!(!h.tile_file("osm/5/1/3.png").exists());
    // 上限に収まる gzip の PNG は、展開して受ける（Python は展開せずマジックで断る。サーバーは gzip を返さない）
    h.stub.script(vec![Reply::gzip_png(100)]);
    let r = h.send("GET", "/tiles/osm/5/1/4.png").await;
    assert_eq!(r.header("x-tile-cache"), "miss");
    let mut expected = PNG_MAGIC.to_vec();
    expected.extend_from_slice(&[b' '; 100]);
    assert_eq!(r.body, expected);
    assert!(h.stub.unscripted().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn cache_files_that_are_not_good_png_files_are_refetched_not_served() {
    let h = Harness::new(&[]).await;
    h.stub.always(Reply::png(&sample_png()));
    let root = h.cache_root();
    for (i, content) in [&b""[..], b"<html>captive portal</html>", &PNG_MAGIC[..7]]
        .into_iter()
        .enumerate()
    {
        let y = i + 1;
        fs::create_dir_all(root.join("osm/6/1")).unwrap();
        fs::write(root.join(format!("osm/6/1/{y}.png")), content).unwrap();
        let r = h.send("GET", &format!("/tiles/osm/6/1/{y}.png")).await;
        assert_eq!(r.header("x-tile-cache"), "miss", "{content:?}");
        assert_eq!(r.body, sample_png());
        // 取り直した中身で置き換わっている
        assert_eq!(
            fs::read(root.join(format!("osm/6/1/{y}.png"))).unwrap(),
            sample_png()
        );
    }
    // 上限を超える大きさのファイル（疎なファイル）
    let big = root.join("osm/6/1/4.png");
    let f = fs::File::create(&big).unwrap();
    f.set_len(MAX_TILE_BYTES + 1).unwrap();
    drop(f);
    let r = h.send("GET", "/tiles/osm/6/1/4.png").await;
    assert_eq!(r.header("x-tile-cache"), "miss");
    assert_eq!(fs::read(&big).unwrap(), sample_png());
    // ディレクトリ（`y.png` という名前）は読めず、書けもしない（失敗は握って、取得した本文は返す）
    fs::create_dir_all(root.join("osm/6/1/5.png")).unwrap();
    let r = h.send("GET", "/tiles/osm/6/1/5.png").await;
    assert_eq!((r.status, r.header("x-tile-cache")), (200, "miss"));
    assert_eq!(r.body, sample_png());
    assert!(root.join("osm/6/1/5.png").is_dir());
    assert!(
        listing(&root).keys().all(|p| !p.ends_with(".part")),
        "{:?}",
        listing(&root)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn symlinks_in_the_cache_are_not_followed_for_reading_or_writing() {
    let h = Harness::new(&[]).await;
    h.stub.always(Reply::png(&sample_png()));
    let outside = TempDir::new("tiles-outside");
    let root = h.cache_root();

    // (1) ファイルがリンク: 読まず、リンクそのものを置き換える。リンクの先は書き換えない
    let victim = outside.path().join("victim.png");
    fs::write(&victim, png_with(10)).unwrap();
    fs::create_dir_all(root.join("osm/7/1")).unwrap();
    symlink(&victim, root.join("osm/7/1/1.png")).unwrap();
    let r = h.send("GET", "/tiles/osm/7/1/1.png").await;
    assert_eq!(r.header("x-tile-cache"), "miss");
    assert_eq!(
        fs::read(&victim).unwrap(),
        png_with(10),
        "リンクの先は変わらない"
    );
    assert!(
        !fs::symlink_metadata(root.join("osm/7/1/1.png"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(root.join("osm/7/1/1.png")).unwrap(), sample_png());
    // 切れたリンクも同じ
    symlink(outside.path().join("none.png"), root.join("osm/7/1/2.png")).unwrap();
    let r = h.send("GET", "/tiles/osm/7/1/2.png").await;
    assert_eq!(r.header("x-tile-cache"), "miss");
    assert!(!outside.path().join("none.png").exists());

    // (2) ディレクトリがリンク: そこへは書かず、読みもしない（取得した本文は返す。保存はしない）
    let elsewhere = outside.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join("1")).unwrap();
    fs::write(elsewhere.join("1/1.png"), png_with(20)).unwrap();
    symlink(&elsewhere, root.join("osm/8")).unwrap();
    for _ in 0..2 {
        let r = h.send("GET", "/tiles/osm/8/1/1.png").await;
        assert_eq!(
            (r.header("x-tile-cache"), r.body.as_slice()),
            ("miss", sample_png().as_slice())
        );
    }
    assert_eq!(fs::read(elsewhere.join("1/1.png")).unwrap(), png_with(20));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
    assert_eq!(fs::read_dir(elsewhere.join("1")).unwrap().count(), 1);

    // (3) 層のディレクトリがリンクでも同じ
    symlink(&elsewhere, root.join("gsi")).unwrap();
    let r = h.send("GET", "/tiles/gsi/1/0/0.png").await;
    assert_eq!(r.header("x-tile-cache"), "miss");
    assert!(!elsewhere.join("1/0").exists());
    fs::remove_file(root.join("gsi")).unwrap();

    // (4) tile_cache 自体がリンク
    let moved = outside.path().join("moved_cache");
    fs::rename(&root, &moved).unwrap();
    symlink(&moved, &root).unwrap();
    let before = listing(&moved);
    let r = h.send("GET", "/tiles/osm/7/1/1.png").await;
    assert_eq!(
        r.header("x-tile-cache"),
        "miss",
        "リンクの tile_cache のヒットは読まない"
    );
    assert_eq!(listing(&moved), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fifo_in_the_cache_does_not_block_a_request() {
    let h = Harness::new(&[]).await;
    h.stub.always(Reply::png(&sample_png()));
    fs::create_dir_all(h.tile_file("osm/9/1")).unwrap();
    let fifo = h.tile_file("osm/9/1/1.png");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        h.send("GET", "/tiles/osm/9/1/1.png"),
    )
    .await
    .expect("FIFO を開いて固まった");
    assert_eq!(r.header("x-tile-cache"), "miss");
    assert_eq!(
        fs::read(&fifo).unwrap(),
        sample_png(),
        "通常のファイルに置き換わる"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn simultaneous_requests_for_the_same_tile_leave_one_whole_file() {
    let h = Harness::new(&[]).await;
    let body = png_with(200_000);
    h.stub.always(Reply::png(&body));
    let tasks: Vec<_> = (0..12)
        .map(|_| {
            let addr = h.addr;
            tokio::spawn(async move { send(addr, "GET", "/tiles/gsi/9/4/4.png").await })
        })
        .collect();
    for task in tasks {
        let r = task.await.unwrap();
        assert_eq!(r.status, 200);
        assert!(["hit", "miss"].contains(&r.header("x-tile-cache")));
        assert_eq!(r.body, body, "どの応答も完全な本文");
    }
    // 残るのは完全な 1 ファイルだけ。書きかけの一時ファイルは残らない
    let files = listing(&h.cache_root());
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(fs::read(h.tile_file("gsi/9/4/4.png")).unwrap(), body);
}

/// 同時に開く上流の接続の上限（OSM 2・地理院 4）。スタブが応答を止めて、同時に待っている数を数える。
async fn check_upstream_limit(layer: Layer, expected: usize) {
    let h = Harness::new(&[]).await;
    let (arrived_tx, arrived_rx) = mpsc::channel::<String>();
    let release = Arc::new((Mutex::new(0usize), Condvar::new()));
    {
        let arrived_tx = Mutex::new(arrived_tx);
        let release = Arc::clone(&release);
        h.stub.set_hook(Some(Arc::new(move |path| {
            let _ = arrived_tx.lock().unwrap().send(path.to_string());
            let (permits, cv) = &*release;
            let mut permits = permits.lock().unwrap();
            while *permits == 0 {
                permits = cv.wait(permits).unwrap();
            }
            *permits -= 1;
        })));
    }
    h.stub.always(Reply::png(&sample_png()));
    let total = expected * 3;
    let tasks: Vec<_> = (0..total)
        .map(|i| {
            let addr = h.addr;
            let target = format!("/tiles/{}/12/{i}/0.png", layer.name());
            tokio::spawn(async move { send(addr, "GET", &target).await })
        })
        .collect();
    let slots = Arc::clone(&h.proxy.upstream_slots[layer.index()]);
    // 上限ぶんの接続が上流に届いて止まる。枠はちょうど使い切られ、それ以上は上流に届かない
    for _ in 0..expected {
        arrived_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("上流に届かない");
    }
    assert_eq!(slots.available_permits(), 0);
    assert_eq!(h.stub.in_flight(layer), expected);
    // 1 つずつ放す。放すたびに、待っていた次の 1 つが上流に届く
    let mut done = 0;
    while done < total {
        {
            let (permits, cv) = &*release;
            *permits.lock().unwrap() += 1;
            cv.notify_one();
        }
        done += 1;
        if done + expected <= total {
            arrived_rx
                .recv_timeout(Duration::from_secs(20))
                .expect("次の要求が上流に届かない");
        }
    }
    for task in tasks {
        let r = task.await.unwrap();
        assert_eq!((r.status, r.header("x-tile-cache")), (200, "miss"));
    }
    assert_eq!(h.stub.max_in_flight(layer), expected);
    assert_eq!(slots.available_permits(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_concurrency_is_two_for_osm() {
    check_upstream_limit(Layer::Osm, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_concurrency_is_four_for_gsi() {
    check_upstream_limit(Layer::Gsi, 4).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_upstream_times_out_into_an_error_tile() {
    let h = Harness::with_timeout(&[], Some(Duration::from_millis(300))).await;
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let release = Arc::clone(&release);
        h.stub.set_hook(Some(Arc::new(move |_| {
            let (open, cv) = &*release;
            let mut open = open.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        })));
    }
    h.stub.always(Reply::png(&sample_png()));
    let r = h.send("GET", "/tiles/osm/3/1/1.png").await;
    assert_eq!((r.status, r.header("x-tile-cache")), (200, "error"));
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(!h.tile_file("osm/3/1/1.png").exists());
    let (open, cv) = &*release;
    *open.lock().unwrap() = true;
    cv.notify_all();
}

/// 条件が満たされるまで、期限つきで待つ（進み具合を実時間の経過で推測しない）。
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "待ちきれない: {what}");
        std::thread::yield_now();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_and_the_cache_write_finish_even_if_the_client_disconnects() {
    let h = Harness::new(&[]).await;
    let (arrived_tx, arrived_rx) = mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let arrived_tx = Mutex::new(arrived_tx);
        let release = Arc::clone(&release);
        h.stub.set_hook(Some(Arc::new(move |_| {
            let _ = arrived_tx.lock().unwrap().send(());
            let (open, cv) = &*release;
            let mut open = open.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        })));
    }
    h.stub.always(Reply::png(&sample_png()));
    let mut stream = tokio::net::TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(b"GET /tiles/osm/3/2/2.png HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    arrived_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    drop(stream); // 上流の応答を待つ間にクライアントが切れる
    {
        let (open, cv) = &*release;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
    let file = h.tile_file("osm/3/2/2.png");
    wait_until("キャッシュのファイル", || file.is_file());
    assert_eq!(fs::read(&file).unwrap(), sample_png());
    // 枠も戻っている
    wait_until("上流の枠", || {
        h.proxy.upstream_slots[Layer::Osm.index()].available_permits() == 2
    });
}

// ---------------------------------------------------------------- 失敗のログ（AC7）

#[test]
fn logs_never_contain_coordinates_urls_or_the_user_agent() {
    let dir = TempDir::new("tiles-logs");
    let stub = StubUpstream::start();
    let clock = TestClock::new(START);
    let cfg = config(
        dir.path(),
        &[(
            "GEOFRAME_TILE_UA",
            "GeoFrame-Test/1.0 (private-contact-name)",
        )],
    );
    let proxy = TileProxy::with_parts(
        &cfg,
        dir.path(),
        stub.endpoints(),
        Box::new(SharedClock(clock)),
    );
    let slots = Arc::clone(&proxy.upstream_slots[Layer::Osm.index()]);
    let tile = Tile {
        layer: Layer::Osm,
        z: 17,
        x: 123_457,
        y: 654_321,
    };
    let secrets = [
        "123457",
        "654321",
        "private-contact-name",
        "/osm/",
        "127.0.0.1",
        "tile_cache",
    ];
    let fetch = |reply: Reply| {
        stub.script(vec![reply]);
        let permit = Arc::clone(&slots).try_acquire_owned().unwrap();
        proxy.fetch_blocking(tile, permit)
    };
    let logs = capture::logs_of(|| {
        for reply in [
            Reply::status(500),
            Reply::status(429),
            Reply::status(403),
            Reply::hangup(),
            Reply::png(b"<html>secret 123457</html>"),
        ] {
            assert_eq!(fetch(reply).1, CacheStatus::Error);
        }
        // 成功しても、保存に失敗しても出さない
        assert_eq!(fetch(Reply::png(&sample_png())).1, CacheStatus::Miss);
        fs::remove_dir_all(&proxy.cache.root).unwrap();
        fs::write(&proxy.cache.root, b"not a directory").unwrap();
        assert_eq!(fetch(Reply::png(&sample_png())).1, CacheStatus::Miss);
    });
    assert!(logs.contains("上流取得失敗（osm）: http 500"), "{logs}");
    assert!(
        logs.contains("http 429") && logs.contains("http 403"),
        "{logs}"
    );
    assert!(logs.contains("bad_response"), "{logs}");
    assert_eq!(logs.matches("上流取得失敗（osm）").count(), 5, "{logs}");
    assert!(logs.contains("キャッシュ書込失敗"), "{logs}");
    for secret in secrets {
        assert!(
            !logs.contains(secret),
            "{secret:?} がログに出ている:\n{logs}"
        );
    }
}

#[test]
fn the_throttle_log_has_only_the_limit() {
    let clock = TestClock::new(START);
    let window = FetchWindow::new(2, Box::new(SharedClock(Arc::clone(&clock))));
    let logs = capture::logs_of(|| {
        assert!(window.allow() && window.allow());
        assert!(!window.allow());
        assert!(!window.allow());
        clock.advance(WINDOW);
        assert!(window.allow());
    });
    // 開始と解除が 1 回ずつ（連続して断っても 1 行）
    assert_eq!(logs.matches("抑制 開始").count(), 1, "{logs}");
    assert_eq!(logs.matches("抑制 解除").count(), 1, "{logs}");
}

// ---------------------------------------------------------------- 掃除（B4）

fn put(root: &Path, rel: &str, size: usize, mtime_secs: u64) {
    make_files(
        root,
        &[serde_json::json!({"path": rel, "size": size, "mtime": mtime_secs})],
    );
}

#[test]
fn prune_does_not_follow_or_delete_symlinks_and_ignores_other_files() {
    let dir = TempDir::new("prune-links");
    let root = dir.path().join("tile_cache");
    let outside = TempDir::new("prune-outside");
    fs::create_dir_all(&root).unwrap();
    let mb = 1024 * 1024;
    put(&root, "osm/3/1/1.png", mb, 100);
    put(&root, "osm/3/1/2.png", mb, 200);
    // 外の大きなファイルへのリンク（ファイル、ディレクトリ、層）
    put(outside.path(), "big.png", 4 * mb, 50);
    symlink(outside.path().join("big.png"), root.join("osm/3/1/3.png")).unwrap();
    put(outside.path(), "dir/1/1.png", 4 * mb, 50);
    symlink(outside.path().join("dir"), root.join("osm/4")).unwrap();
    symlink(outside.path().join("dir"), root.join("gsi")).unwrap();
    // x の階層がリンク（先に大きな PNG と古い書きかけがある）
    put(outside.path(), "xdir/7.png", 4 * mb, 50);
    put(outside.path(), "xdir/8.1234.00ff.part", 10, 50);
    symlink(outside.path().join("xdir"), root.join("osm/3/2")).unwrap();
    // 形の合わないファイル（foreign）は数えず、消さない
    put(&root, "osm/3/1/notes.txt", mb, 10);
    put(&root, "osm/3/1/01.png", mb, 10);
    put(&root, "osm/3/readme.png", mb, 10);
    put(&root, "other/1/1/1.png", mb, 10);

    let report = prune_dir(&root, 1, &|| false).unwrap();
    // 数えたのは 2 枚だけ（2 MB > 1 MB）。古い 1 枚を消すと 1 MB ≤ 0.9 × 1 MB にならないので、両方消える
    assert_eq!((report.files, report.deleted), (2, 2), "{report:?}");
    for kept in [
        "osm/3/1/notes.txt",
        "osm/3/1/01.png",
        "osm/3/readme.png",
        "other/1/1/1.png",
    ] {
        assert!(root.join(kept).is_file(), "{kept}");
    }
    assert!(
        fs::symlink_metadata(root.join("osm/3/1/3.png"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(root.join("osm/4"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(root.join("gsi"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(outside.path().join("big.png")).unwrap().len(),
        4 * mb as u64
    );
    assert!(outside.path().join("dir/1/1.png").is_file());
    assert!(
        fs::symlink_metadata(root.join("osm/3/2"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(outside.path().join("xdir/7.png").is_file());
    assert!(outside.path().join("xdir/8.1234.00ff.part").is_file());

    // tile_cache 自体がリンクなら、何もせずエラー
    let link = dir.path().join("linked");
    symlink(&root, &link).unwrap();
    put(&root, "osm/5/1/1.png", 2 * mb, 1);
    assert!(prune_dir(&link, 1, &|| false).is_err());
    assert!(root.join("osm/5/1/1.png").is_file());
    // 無ければ何もしない
    assert_eq!(
        prune_dir(&dir.path().join("none"), 1, &|| false).unwrap(),
        PruneReport::default()
    );
}

#[test]
fn prune_removes_only_stale_part_files() {
    let dir = TempDir::new("prune-parts");
    let root = dir.path().join("tile_cache");
    put(&root, "osm/3/1/1.png", 10, 100);
    put(&root, "osm/3/1/2.1234.00ff.part", 10, 100); // 古い（1970 年）
    let fresh = root.join("osm/3/1/3.1234.00ff.part");
    fs::write(&fresh, b"x").unwrap(); // 今
    let report = prune_dir(&root, 16384, &|| false).unwrap();
    assert_eq!((report.deleted, report.stale_parts), (0, 1), "{report:?}");
    assert!(root.join("osm/3/1/1.png").is_file());
    assert!(!root.join("osm/3/1/2.1234.00ff.part").exists());
    assert!(fresh.is_file());
}

#[test]
fn prune_stops_when_asked_and_keeps_the_rest() {
    let dir = TempDir::new("prune-stop");
    let root = dir.path().join("tile_cache");
    for i in 0..10 {
        put(&root, &format!("osm/3/1/{i}.png"), 200_000, 100 + i as u64);
    }
    let calls = std::cell::Cell::new(0);
    let report = prune_dir(&root, 1, &|| {
        calls.set(calls.get() + 1);
        calls.get() > 2
    })
    .unwrap();
    assert!(report.interrupted, "{report:?}");
    assert!(report.deleted < 10);
    // 消し残しが古い順の接頭辞になっている（新しい方は残る）
    let left = pngs_under(&root);
    assert_eq!(left.len(), 10 - report.deleted);
}

/// 古い一時ファイル 2 個と、上限を超える `.png` 3 枚（`max_mb=0` なら止まらなければ全部消える）。
fn stop_fixture(root: &Path) {
    for i in 0..3 {
        put(root, &format!("osm/3/1/{i}.png"), 1000, 100 + i as u64);
    }
    for i in 0..2 {
        put(root, &format!("osm/3/1/{i}.1234.00ff.part"), 10, 100);
    }
}

fn remaining(root: &Path) -> (usize, usize) {
    let all = listing(root);
    let parts = all.keys().filter(|p| p.ends_with(".part")).count();
    (all.len() - parts, parts)
}

#[test]
fn prune_that_is_stopped_while_scanning_deletes_nothing() {
    let dir = TempDir::new("prune-stop-scan");
    let root = dir.path().join("tile_cache");
    stop_fixture(&root);
    // 走査の `should_stop` は、ディレクトリで 1 回 + 5 件で 1 回ずつ（計 6 回）。k 回目で止まる合図を、
    // 走査のどこで立てても、集めた古い一時ファイルも `.png` も 1 件も消さずに戻る
    for k in 1..=6 {
        let calls = std::cell::Cell::new(0);
        let report = prune_dir(&root, 0, &|| {
            calls.set(calls.get() + 1);
            calls.get() >= k
        })
        .unwrap();
        assert!(report.interrupted, "k={k} {report:?}");
        assert_eq!((report.deleted, report.stale_parts), (0, 0), "k={k}");
        assert_eq!(remaining(&root), (5 - 2, 2), "k={k}");
    }
    // 合図が無ければ、同じ並びで全部消える
    let report = prune_dir(&root, 0, &|| false).unwrap();
    assert!(!report.interrupted);
    assert_eq!((report.deleted, report.stale_parts), (3, 2), "{report:?}");
}

#[test]
fn prune_that_is_stopped_while_removing_stale_parts_stops_in_the_middle() {
    let dir = TempDir::new("prune-stop-parts");
    let root = dir.path().join("tile_cache");
    stop_fixture(&root);
    // 古い一時ファイルが 1 個消えたのを見たら止まる合図（走査中は立たない）
    let report = prune_dir(&root, 0, &|| remaining(&root).1 < 2).unwrap();
    assert!(report.interrupted, "{report:?}");
    assert_eq!((report.deleted, report.stale_parts), (0, 1), "{report:?}");
    assert_eq!(remaining(&root), (3, 1));
}

#[test]
fn prune_runs_safely_beside_requests_and_never_leaves_a_broken_file() {
    let dir = TempDir::new("prune-race");
    let (db, _w) = synthetic_catalog(dir.path(), 0);
    let _ = db;
    let stub = StubUpstream::start();
    stub.always(Reply::png(&png_with(5000)));
    let cfg = config(dir.path(), &[("GEOFRAME_TILE_CACHE_MAX_MB", "0")]);
    let proxy = Arc::new(TileProxy::with_parts(
        &cfg,
        dir.path(),
        stub.endpoints(),
        Box::new(SystemClock {
            origin: Instant::now(),
        }),
    ));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = StopFlag::new();
    let pruner = {
        let proxy = Arc::clone(&proxy);
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut runs = 0;
            while !stop.is_stopped() {
                proxy.prune(&|| false);
                runs += 1;
            }
            runs
        })
    };
    let results = rt.block_on(async {
        let tasks: Vec<_> = (0..40)
            .map(|i| {
                let proxy = Arc::clone(&proxy);
                tokio::spawn(async move {
                    proxy
                        .fetch(Tile {
                            layer: Layer::Osm,
                            z: 10,
                            x: i % 4,
                            y: 0,
                        })
                        .await
                })
            })
            .collect();
        let mut out = Vec::new();
        for task in tasks {
            out.push(task.await.unwrap());
        }
        out
    });
    stop.stop();
    assert!(pruner.join().unwrap() > 0);
    for (body, status) in results {
        // 掃除に消されても、読めたものは完全な本文で、取り直せば miss。壊れた本文や error は返らない
        assert!(
            matches!(status, CacheStatus::Hit | CacheStatus::Miss),
            "{status:?}"
        );
        assert_eq!(body, png_with(5000));
    }
    assert!(stub.unscripted().is_empty());
    for (path, _) in listing(&dir.path().join("tile_cache")) {
        assert!(path.ends_with(".png"), "書きかけが残っている: {path}");
    }
}

#[test]
fn the_pruner_thread_runs_at_start_and_periodically_and_stops_on_the_flag() {
    let dir = TempDir::new("pruner-thread");
    let (db, _w) = synthetic_catalog(dir.path(), 0);
    let _ = db;
    let cfg = config(dir.path(), &[("GEOFRAME_TILE_CACHE_MAX_MB", "1")]);
    let proxy = Arc::new(TileProxy::with_parts(
        &cfg,
        dir.path(),
        StubUpstream::start().endpoints(),
        Box::new(SystemClock {
            origin: Instant::now(),
        }),
    ));
    let root = dir.path().join("tile_cache");
    let mb = 1024 * 1024;
    put(&root, "osm/3/1/1.png", mb, 100);
    put(&root, "osm/3/1/2.png", mb, 200);
    let stop = StopFlag::new();
    // 間隔を 1 時間にすると、最初の 1 回だけ（起動時）が実時間で見える
    let pruner = spawn_pruner(Arc::clone(&proxy), stop.clone(), PRUNE_INTERVAL);
    wait_until("起動時の掃除", || pngs_under(&root).is_empty());
    stop.stop();
    let started = Instant::now();
    pruner.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "停止の合図で止まる"
    );

    // 短い間隔なら、あとから増えた分も消える
    let stop = StopFlag::new();
    let pruner = spawn_pruner(Arc::clone(&proxy), stop.clone(), Duration::from_millis(10));
    for round in 0..3 {
        put(&root, &format!("osm/4/1/{round}.png"), 2 * mb, 100);
        wait_until("定期の掃除", || pngs_under(&root).is_empty());
    }
    stop.stop();
    pruner.join().unwrap();
    // 停止の合図が先に出ていれば、走査を始めない
    let stop = StopFlag::new();
    stop.stop();
    put(&root, "osm/5/1/1.png", 2 * mb, 100);
    spawn_pruner(proxy, stop, PRUNE_INTERVAL).join().unwrap();
    assert!(root.join("osm/5/1/1.png").is_file());
}

#[test]
fn prune_does_nothing_for_a_missing_or_linked_cache_but_does_not_panic() {
    let dir = TempDir::new("prune-missing");
    let (db, _w) = synthetic_catalog(dir.path(), 0);
    let _ = db;
    let cfg = config(dir.path(), &[]);
    let proxy = TileProxy::with_parts(
        &cfg,
        dir.path(),
        StubUpstream::start().endpoints(),
        Box::new(SystemClock {
            origin: Instant::now(),
        }),
    );
    assert_eq!(proxy.prune(&|| false), PruneReport::default());
    let target = TempDir::new("prune-link-target");
    symlink(target.path(), dir.path().join("tile_cache")).unwrap();
    let logs = capture::logs_of(|| {
        assert_eq!(proxy.prune(&|| false), PruneReport::default());
    });
    assert!(logs.contains("キャッシュ整理を飛ばしました"), "{logs}");
    assert!(!logs.contains(dir.path().to_str().unwrap()), "{logs}");
}
