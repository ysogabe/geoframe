//! 旅行ブログの保存と書き出し（E15 `POST /api/blog/save`・E16 `POST /api/blog/export`。T3.6）の結合テスト。
//! 合成データだけを使う。
//!
//! 現行 Python の `save_blog_draft`・`export_blog` に、合成の旅程・合成DB・合成の画像を置いて同じ要求を順に流した
//! 結果（`tools/gen_blog_write_fixture.py` が書き出すフィクスチャ）と、状態コード・応答の JSON・データ置き場のファイルの
//! 増減が一致することを確かめる。保存された下書きと書き出しのテキストは**全文（バイト列）**で比べ、画像は寸法・
//! 成分数・EXIF と ICC の有無・四隅の色で比べる。意図した差は、フィクスチャの `known_differences` に載るケースだけ。
//!
//! 書き出しはプロセスで 1 本だけ（実行中なら 409）なので、このファイルの試験は 1 本ずつ動かす（[`serial`]）。

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use common::{TempDir, build_admin_catalog, get, post_raw, spawn_with_env};
use geoframe::db::Catalog;
use geoframe::writer::{OpenMode, Writer};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

const JSON_UTF8: &str = "application/json; charset=utf-8";
const SAVE: &str = "/api/blog/save";
const EXPORT: &str = "/api/blog/export";

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SERIAL.lock().await
}

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blog-write-python.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

// ---- 合成の画像（Python の `blog_write_fixture.draw` と同じ絵） ----

const COLORS: [(&str, [u8; 3]); 4] = [
    ("red", [255, 0, 0]),
    ("green", [0, 160, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
];
const GRAYS: [u8; 4] = [40, 110, 180, 250];
const MARK: usize = 10;

fn fake_icc() -> Vec<u8> {
    let mut icc = b"synthetic-icc-profile-".to_vec();
    icc.extend(0u8..64);
    icc
}

/// 向き `orient` と、必要なら GPS（`GPSLatitudeRef`）を持つ最小の EXIF の APP1 セグメント。
fn exif_segment(orient: u16, gps: bool) -> Vec<u8> {
    let mut tiff: Vec<u8> = b"MM\0*\0\0\0\x08".to_vec();
    let entries: u16 = if gps { 2 } else { 1 };
    tiff.extend_from_slice(&entries.to_be_bytes());
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1]);
    tiff.extend_from_slice(&orient.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    if gps {
        tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 38]);
    }
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    if gps {
        assert_eq!(tiff.len(), 38);
        tiff.extend_from_slice(&[0, 1]);
        tiff.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
        tiff.extend_from_slice(&[0, 0, 0, 0]);
    }
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    segment(0xE1, &payload)
}

fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, marker];
    out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn icc_segment() -> Vec<u8> {
    let mut payload = b"ICC_PROFILE\0".to_vec();
    payload.extend_from_slice(&[1, 1]);
    payload.extend_from_slice(&fake_icc());
    segment(0xE2, &payload)
}

fn draw(name: &str, images: &Value) -> Vec<u8> {
    let spec = &images[name];
    let mode = spec["mode"].as_str().unwrap();
    let (w, h) = (
        usize::try_from(spec["size"][0].as_u64().unwrap()).unwrap(),
        usize::try_from(spec["size"][1].as_u64().unwrap()).unwrap(),
    );
    let channels = match mode {
        "L" => 1,
        "RGB" => 3,
        "RGBA" => 4,
        other => panic!("{other}"),
    };
    let mut pixels = vec![0u8; w * h * channels];
    for y in 0..h {
        for x in 0..w {
            let quadrant = usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2);
            let p = &mut pixels[(y * w + x) * channels..][..channels];
            match channels {
                1 => p[0] = GRAYS[quadrant],
                3 => p.copy_from_slice(&COLORS[quadrant].1),
                _ => {
                    p[..3].copy_from_slice(&COLORS[quadrant].1);
                    p[3] = 128;
                }
            }
        }
    }
    let (width, height) = (u32::try_from(w).unwrap(), u32::try_from(h).unwrap());
    if spec["fmt"] == "PNG" {
        let image = match channels {
            4 => image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(width, height, pixels).unwrap(),
            ),
            _ => image::DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(width, height, pixels).unwrap(),
            ),
        };
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        return out.into_inner();
    }
    let mut out = Vec::new();
    let color = if channels == 1 {
        jpeg_encoder::ColorType::Luma
    } else {
        jpeg_encoder::ColorType::Rgb
    };
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(
            &pixels,
            u16::try_from(w).unwrap(),
            u16::try_from(h).unwrap(),
            color,
        )
        .unwrap();
    // SOI の直後に EXIF → ICC の順で差し込む
    let mut extra = Vec::new();
    if let Some(orient) = spec.get("orient").and_then(Value::as_u64) {
        extra.extend(exif_segment(
            u16::try_from(orient).unwrap(),
            spec.get("gps").and_then(Value::as_bool).unwrap_or(false),
        ));
    }
    if spec.get("icc").and_then(Value::as_bool).unwrap_or(false) {
        extra.extend(icc_segment());
    }
    out.splice(2..2, extra);
    out
}

fn label(px: &[u8]) -> String {
    let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
    if (r - g).abs() < 12 && (g - b).abs() < 12 {
        let nearest = GRAYS
            .iter()
            .min_by_key(|level| (i32::from(**level) - g).abs())
            .unwrap();
        return format!("g{nearest}");
    }
    COLORS
        .iter()
        .min_by_key(|(_, c)| {
            (0..3)
                .map(|i| (i32::from(c[i]) - i32::from(px[i])).pow(2))
                .sum::<i32>()
        })
        .unwrap()
        .0
        .to_string()
}

/// JPEG の成分数（SOF マーカー）。JPEG でなければ 0。
fn jpeg_components(data: &[u8]) -> u64 {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return 0;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if matches!(marker, 0xC0..=0xC2) {
            return u64::from(data[i + 9]);
        }
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            i += 2;
            continue;
        }
        if marker == 0xFF {
            i += 1;
            continue;
        }
        i += 2 + usize::from(u16::from_be_bytes([data[i + 2], data[i + 3]]));
    }
    0
}

const TEXT_SUFFIXES: [&str; 4] = [".json", ".md", ".html", ".txt"];

/// ファイルの記録（`blog_write_fixture.describe` と同じ形）。
fn describe(path: &Path) -> Value {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_symlink() {
        return json!({"kind": "symlink"});
    }
    if meta.is_dir() {
        return json!({"kind": "dir"});
    }
    let data = fs::read(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy();
    if TEXT_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return json!({"kind": "text", "text": String::from_utf8(data).unwrap()});
    }
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(&data), options);
    let Ok(px) = dec.decode() else {
        return json!({"kind": "other", "bytes": data.len()});
    };
    let (w, h) = dec.dimensions().unwrap();
    assert_eq!(px.len(), w * h * 3);
    let at = |x: usize, y: usize| label(&px[(y * w + x) * 3..][..3]);
    json!({
        "kind": "image",
        "size": [w, h],
        "components": jpeg_components(&data),
        "corners": [at(MARK, MARK), at(w - 1 - MARK, MARK), at(MARK, h - 1 - MARK), at(w - 1 - MARK, h - 1 - MARK)],
        "exif": data.windows(6).any(|w| w == b"Exif\0\0"),
        "icc": data.windows(11).any(|w| w == b"ICC_PROFILE"),
    })
}

/// データ置き場のファイルの記録。画像のデコードは重いので、ファイルの大きさと更新時刻が変わったものだけ記録し直す。
#[derive(Default)]
struct Lister {
    cache: BTreeMap<String, (Stamp, Value)>,
}

/// ファイルの大きさ・更新時刻（秒・ナノ秒）・モード。
type Stamp = (u64, i64, i64, u32);

impl Lister {
    fn listing(&mut self, root: &Path) -> BTreeMap<String, Value> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if rel.starts_with("catalog.db") {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).unwrap();
                out.push((rel, path.clone()));
                if meta.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut paths = Vec::new();
        walk(root, root, &mut paths);
        let mut out = BTreeMap::new();
        for (rel, path) in paths {
            let meta = fs::symlink_metadata(&path).unwrap();
            let stamp: Stamp = (meta.len(), meta.mtime(), meta.mtime_nsec(), meta.mode());
            let value = match self.cache.get(&rel) {
                Some((old, value)) if *old == stamp && !meta.is_dir() => value.clone(),
                _ => describe(&path),
            };
            self.cache.insert(rel.clone(), (stamp, value.clone()));
            out.insert(rel, value);
        }
        out
    }
}

fn files_diff(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> Value {
    let added: Map<String, Value> = after
        .iter()
        .filter(|(k, _)| !before.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let removed: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let changed: Map<String, Value> = after
        .iter()
        .filter(|(k, v)| before.get(*k).is_some_and(|old| old != *v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    json!({"added": added, "removed": removed, "changed": changed})
}

// ---- サーバー ----

struct Server {
    dir: TempDir,
    _plans: TempDir,
    /// 試験が DB を直接いじる接続（WAL を保つ）。
    db: Connection,
    addr: std::net::SocketAddr,
    images: Value,
    lister: Lister,
}

impl Server {
    async fn start(with_writer: bool) -> Self {
        let fixture = fixture();
        let dir = TempDir::new("blog-write-api");
        let plans = TempDir::new("blog-write-plans");
        for (name, content) in fixture["plans"].as_object().unwrap() {
            fs::write(plans.path().join(name), content.as_str().unwrap()).unwrap();
        }
        let db = build_admin_catalog(dir.path());
        let path = dir.path().join("catalog.db");
        let mut catalog = Catalog::open(&path).unwrap();
        if with_writer {
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let plans_dir = plans.path().to_string_lossy().into_owned();
        let addr = spawn_with_env(
            dir.path(),
            &[("GEOFRAME_BLOG_PLANS_DIR", plans_dir.as_str())],
            catalog,
        )
        .await;
        Self {
            dir,
            _plans: plans,
            db,
            addr,
            images: fixture["images"].clone(),
            lister: Lister::default(),
        }
    }

    fn apply_setup(&self, ops: &Value) {
        for op in ops.as_array().unwrap() {
            if op["op"] == "sql" {
                self.db.execute_batch(op["sql"].as_str().unwrap()).unwrap();
                continue;
            }
            let path = self.dir.path().join(op["path"].as_str().unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            match op["op"].as_str().unwrap() {
                "image" => {
                    fs::write(&path, draw(op["image"].as_str().unwrap(), &self.images)).unwrap()
                }
                "text" => fs::write(&path, op["text"].as_str().unwrap()).unwrap(),
                "garbage" => fs::write(&path, b"not an image").unwrap(),
                "dir" => fs::create_dir(&path).unwrap(),
                "symlink" => symlink(op["to"].as_str().unwrap(), &path).unwrap(),
                other => panic!("{other}"),
            }
        }
    }

    fn listing(&mut self) -> BTreeMap<String, Value> {
        let root = self.dir.path().to_path_buf();
        self.lister.listing(&root)
    }

    async fn post(&self, target: &str, body: &[u8]) -> common::Response {
        post_raw(
            self.addr,
            target,
            &[
                ("Host".into(), "localhost".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body,
        )
        .await
    }
}

const NO_CHANGE: &str = r#"{"added": {}, "removed": [], "changed": {}}"#;

fn no_change() -> Value {
    serde_json::from_str(NO_CHANGE).unwrap()
}

/// Python と応答が違うケースの、Rust の期待値: (状態コード, `error` の文面)。どれもファイルは変わらない。
fn known(name: &str) -> (u16, &'static str) {
    match name {
        "export_photo_id_beyond_i64" => (400, "写真#99999999999999999999の表示用画像がありません"),
        "export_photo_id_superscript" => (400, "写真#²の表示用画像がありません"),
        other => panic!("{other} は known_differences に無い"),
    }
}

#[tokio::test]
async fn every_request_matches_python_status_body_and_files() {
    let _serial = serial().await;
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    // 取りこぼしが無いよう規模を固定する（ケースを足したら更新する）
    assert_eq!(cases.len(), 93);
    let known_names: Vec<&str> = fixture["known_differences"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut server = Server::start(true).await;
    server.apply_setup(&fixture["initial"]);
    let mut seen_known = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        server.apply_setup(&case["setup"]);
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
        let files_before = server.listing();
        let response = post_raw(
            server.addr,
            case["target"].as_str().unwrap(),
            &headers,
            &body,
        )
        .await;
        let files = files_diff(&files_before, &server.listing());

        if known_names.contains(&name) {
            seen_known += 1;
            let (status, error) = known(name);
            assert_eq!(response.status, status, "{name}");
            assert_eq!(response.json()["error"], error, "{name}");
            assert_eq!(files, no_change(), "{name}: ファイルは変わらない");
            // Python は違う（だから known）。差が消えたら、この一覧から外す
            let py_same = case["status"].as_u64().unwrap() as u16 == response.status
                && case["body"] == response.json();
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
        assert_eq!(files, case["files"], "{name}: ファイルの増減");
        assert!(!response.headers.contains("access-control-"), "{name}");
    }
    assert_eq!(seen_known, known_names.len());
    // どのケースも `.part` や作業領域（ドット始まり）を残さない
    let leftovers: Vec<String> = server
        .listing()
        .into_keys()
        .filter(|name| {
            name.ends_with(".part")
                || name.starts_with("blog_exports/.")
                || name.split('/').any(|part| part.starts_with(".previous"))
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ---- 取り込みが無効（writer が無い）ときの 503 ----

#[tokio::test]
async fn without_a_writer_save_and_export_are_503_and_nothing_is_created() {
    let _serial = serial().await;
    let mut server = Server::start(false).await;
    server.apply_setup(&fixture()["initial"]);
    let before = server.listing();
    let manifest = json!({"trip_id": "sample-trip-a", "status": "draft", "chapters": []});
    for (target, extra) in [(SAVE, json!({})), (EXPORT, json!({}))] {
        let mut payload = json!({"trip_id": "sample-trip-a", "manifest": manifest});
        payload.as_object_mut().unwrap().extend(
            extra
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let response = server.post(target, payload.to_string().as_bytes()).await;
        assert_eq!(response.status, 503, "{target}");
        let json = response.json();
        assert_eq!(json["ok"], false);
        assert!(json["error"].as_str().unwrap().contains("書き込みが無効"));
        let text = response.text();
        for secret in ["sample-trip", "travel_blog", "blog_exports", "GEOFRAME"] {
            assert!(!text.contains(secret), "{secret}: {text}");
        }
        assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    }
    // 何も作らない（`travel_blog/`・`blog_exports/` も）
    assert_eq!(server.listing(), before);
    assert!(!server.dir.path().join("travel_blog").exists());
    assert!(!server.dir.path().join("blog_exports").exists());

    // dry_run は何も書かないので答える（Python と同じ）
    let dry = json!({"trip_id": "sample-trip-a", "manifest": manifest, "dry_run": true});
    let response = server.post(EXPORT, dry.to_string().as_bytes()).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({"ok": true, "dry_run": true, "trip_id": "sample-trip-a", "images": 0})
    );
    // 入力の違反（400・404）は書き込みの有無によらず Python と同じ
    for (target, body, status, error) in [
        (
            SAVE,
            json!({"trip_id": "sample-trip-a", "manifest": {"status": "x"}}),
            400,
            "trip_idが不正です",
        ),
        (
            SAVE,
            json!({"trip_id": "no-such-trip", "manifest": manifest}),
            404,
            "旅程が見つかりません",
        ),
        (
            EXPORT,
            json!({"trip_id": "sample-trip-a", "manifest": []}),
            400,
            "manifestはJSONオブジェクトにしてください",
        ),
    ] {
        let response = server.post(target, body.to_string().as_bytes()).await;
        assert_eq!(response.status, status, "{target}");
        assert_eq!(response.json()["error"], error);
    }
    assert_eq!(server.listing(), before);
    // 読み取り（E10）は動く
    let response = get(server.addr, "/api/blog/draft?trip_id=sample-trip-a").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["source"], "generated");
}

/// 保存した下書きは、E10 がそのまま返し、旅程の一覧の `has_draft` にも反映される。
#[tokio::test]
async fn a_saved_draft_is_read_back_by_the_draft_endpoint() {
    let _serial = serial().await;
    let server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let manifest = json!({
        "trip_id": "sample-trip-a", "status": "published", "title": "題", "subtitle": "", "summary": "",
        "extra": [1, 2.5], "chapters": [{"id": "d1", "date": "2025-06-06", "title": "章", "location_label": "", "body": "本文",
            "images": [{"photo_id": 12, "alt": "", "caption": "c"}]}],
    });
    let response = server
        .post(
            SAVE,
            json!({"trip_id": "sample-trip-a", "manifest": manifest})
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    let draft = get(server.addr, "/api/blog/draft?trip_id=sample-trip-a").await;
    assert_eq!(draft.status, 200);
    let body = draft.json();
    assert_eq!(body["source"], "saved");
    assert_eq!(body["manifest"]["status"], "draft");
    assert_eq!(body["manifest"]["extra"], json!([1, 2.5]));
    assert_eq!(
        body["manifest"]["chapters"][0]["images"][0]["photo_id"],
        "12"
    );
    assert_eq!(
        body["manifest"]["chapters"][0]["images"][0]["alt"],
        "旅の写真"
    );
    let trips = get(server.addr, "/api/blog/trips").await.json();
    let item = trips["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["trip_id"] == "sample-trip-a")
        .unwrap();
    assert_eq!(
        (item["has_draft"].clone(), item["has_export"].clone()),
        (json!(true), json!(false))
    );
    // 書き出すと has_export も真になり、`url` の先が配信される
    let response = server
        .post(
            EXPORT,
            json!({"trip_id": "sample-trip-a", "manifest": body["manifest"]})
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        response.json(),
        json!({"ok": true, "trip_id": "sample-trip-a", "images": 1, "path": "blog_exports/sample-trip-a",
               "url": "/blog_exports/sample-trip-a/index.html"})
    );
}

/// 作業領域・`.previous` は、書き出しの途中でも HTTP から読めない（ドット始まりは配信しない）。
#[tokio::test]
async fn the_export_url_is_served_and_the_work_area_is_not() {
    let _serial = serial().await;
    let mut server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let manifest = json!({"trip_id": "sample-trip-a", "status": "draft", "title": "配信の試験",
        "chapters": [{"id": "d1", "images": [{"photo_id": "4"}]}]});
    let response = server
        .post(
            EXPORT,
            json!({"trip_id": "sample-trip-a", "manifest": manifest})
                .to_string()
                .as_bytes(),
        )
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    let page = get(server.addr, "/blog_exports/sample-trip-a/index.html").await;
    assert_eq!(page.status, 200);
    assert!(page.text().contains("配信の試験"));
    let image = get(
        server.addr,
        "/blog_exports/sample-trip-a/images/photo-4.jpg",
    )
    .await;
    assert_eq!(image.status, 200);
    assert_eq!(&image.body[..2], &[0xFF, 0xD8]);
    // 隠れた作業領域・退避先
    let export_dir = server.dir.path().join("blog_exports");
    fs::create_dir_all(export_dir.join(".sample-trip-a-rs0123456789abcdef")).unwrap();
    fs::write(
        export_dir.join(".sample-trip-a-rs0123456789abcdef/index.html"),
        "x",
    )
    .unwrap();
    for path in [
        "/blog_exports/.sample-trip-a-rs0123456789abcdef/index.html",
        "/blog_exports/.sample-trip-a.previous/index.html",
    ] {
        assert_eq!(get(server.addr, path).await.status, 403, "{path}");
    }
    let _ = server.listing();
}

/// 同時に来た 2 本の書き出しは、1 本が通り、もう 1 本は 409（または順に通る）。壊れた書き出しは残らない。
#[tokio::test]
async fn simultaneous_exports_never_leave_a_broken_export() {
    let _serial = serial().await;
    let mut server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let addr = server.addr;
    let body = json!({"trip_id": "sample-trip-a", "manifest": {
        "trip_id": "sample-trip-a", "status": "draft", "title": "同時",
        "chapters": [{"id": "d1", "images": [{"photo_id": "1"}, {"photo_id": "2"}, {"photo_id": "4"}]}]}})
    .to_string();
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let body = body.clone();
            tokio::spawn(async move {
                post_raw(
                    addr,
                    EXPORT,
                    &[
                        ("Host".into(), "localhost".into()),
                        ("Content-Type".into(), "application/json".into()),
                        ("Content-Length".into(), body.len().to_string()),
                        ("Connection".into(), "close".into()),
                    ],
                    body.as_bytes(),
                )
                .await
                .status
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    assert!(
        statuses.iter().all(|s| matches!(*s, 200 | 409)),
        "{statuses:?}"
    );
    assert!(statuses.contains(&200), "{statuses:?}");
    let files = server.listing();
    let names: Vec<&String> = files
        .keys()
        .filter(|k| k.starts_with("blog_exports/"))
        .collect();
    assert_eq!(
        names,
        [
            "blog_exports/sample-trip-a",
            "blog_exports/sample-trip-a/blog-manifest.json",
            "blog_exports/sample-trip-a/images",
            "blog_exports/sample-trip-a/images/photo-1.jpg",
            "blog_exports/sample-trip-a/images/photo-2.jpg",
            "blog_exports/sample-trip-a/images/photo-4.jpg",
            "blog_exports/sample-trip-a/index.html",
            "blog_exports/sample-trip-a/index.md",
        ]
    );
}

/// HEIC の元も derivative と同じデコーダで読める。EXIF・GPS・向きの情報は出力に残らない。
#[tokio::test]
async fn a_heic_source_is_decoded_and_exported_without_metadata() {
    let _serial = serial().await;
    let mut server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let bytes = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic_o6.heic"),
    )
    .unwrap();
    fs::create_dir_all(server.dir.path().join("photos")).unwrap();
    fs::write(server.dir.path().join("photos/o24.heic"), &bytes).unwrap();
    server
        .db
        .execute_batch("UPDATE photos SET display='photos/o24.heic' WHERE id=24")
        .unwrap();
    let expected = geoframe::imaging::render(&bytes, 0).unwrap().info.output;
    let body = json!({"trip_id": "sample-trip-a", "manifest": {
        "trip_id": "sample-trip-a", "status": "draft",
        "chapters": [{"id": "d1", "images": [{"photo_id": "24"}]}]}});
    let response = server.post(EXPORT, body.to_string().as_bytes()).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let files = server.listing();
    let image = &files["blog_exports/sample-trip-a/images/photo-24.jpg"];
    assert_eq!(image["kind"], "image");
    assert_eq!(
        image["size"],
        json!([expected.0, expected.1]),
        "向きを当てた寸法"
    );
    assert_eq!(
        (image["exif"].clone(), image["icc"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(image["components"], 3);
}

/// 巨大な画像（画素数の上限超え・ファイルの大きさの上限超え）は、メモリを確保する前に断る（500・何も残さない）。
#[tokio::test]
async fn oversized_sources_are_refused_before_decoding() {
    let _serial = serial().await;
    let mut server = Server::start(true).await;
    let fixture = fixture();
    server.apply_setup(&fixture["initial"]);
    let before = server.listing();
    // 小さな JPEG の SOF の寸法だけを 30000x30000（9 億画素）に書き換える
    let mut jpeg = draw("small", &fixture["images"]);
    let sof = jpeg
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("SOF0");
    jpeg[sof + 5..sof + 9].copy_from_slice(&[0x75, 0x30, 0x75, 0x30]);
    fs::write(server.dir.path().join("derivatives/huge-pixels.jpg"), jpeg).unwrap();
    // 512 MiB を 1 バイト超える（疎なファイル。読まれない）
    fs::File::create(server.dir.path().join("derivatives/huge-file.jpg"))
        .unwrap()
        .set_len(512 * 1024 * 1024 + 1)
        .unwrap();
    server
        .db
        .execute_batch(
            "UPDATE photos SET display='derivatives/huge-pixels.jpg' WHERE id=25;
             UPDATE photos SET display='derivatives/huge-file.jpg' WHERE id=26;",
        )
        .unwrap();
    let after_setup = server.listing();
    for id in ["25", "26"] {
        let body = json!({"trip_id": "sample-trip-a", "manifest": {
            "trip_id": "sample-trip-a", "status": "draft",
            "chapters": [{"id": "d1", "images": [{"photo_id": "1"}, {"photo_id": id}]}]}});
        let response = server.post(EXPORT, body.to_string().as_bytes()).await;
        assert_eq!(response.status, 500, "{id}");
        assert_eq!(response.json()["error"], "内部エラーが発生しました");
        // 書き出しの置き場（空の`blog_exports/`）のほかは何も残さない
        let mut now = server.listing();
        now.remove("blog_exports");
        assert_eq!(now, after_setup, "{id}: 何も残さない");
    }
    assert!(after_setup.len() > before.len());
    assert!(
        !server
            .dir
            .path()
            .join("blog_exports")
            .join("sample-trip-a")
            .exists()
    );
}

/// 回転・取得と同じロック（`imaging::lock_derivatives`）を握る別スレッド。`release` で手放す。
struct Hold {
    release: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

fn hold_derivatives() -> Hold {
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let _guard = geoframe::imaging::lock_derivatives();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    held_rx.recv().unwrap();
    Hold { release, thread }
}

impl Hold {
    fn release(self) {
        let _ = self.release.send(());
        self.thread.join().unwrap();
    }
}

/// 書き出しのように応答まで長くかかる `POST` は、無通信の期限で接続を切られない（`serve` が付ける接続の情報の
/// `ConnBusy`）。期限を 0.4 秒にして、書き出しを 1.5 秒止めておく（回転・取得と同じロックを試験が握る）。
/// 比較として、接続の情報を付けない起動では同じ要求が切られる（試験が効いていることの確認）。
#[tokio::test]
async fn a_long_export_is_not_cut_by_the_idle_timeout() {
    use geoframe::config::{Config, Overrides};
    use geoframe::limits::{ConnLimits, LimitedListener};
    use geoframe::server::{ConnInfo, router};
    use std::sync::Arc;
    use std::time::Duration;

    let _serial = serial().await;
    let fixture = fixture();
    let plans = TempDir::new("blog-write-idle-plans");
    for (name, content) in fixture["plans"].as_object().unwrap() {
        fs::write(plans.path().join(name), content.as_str().unwrap()).unwrap();
    }
    let dir = TempDir::new("blog-write-idle");
    let db = build_admin_catalog(dir.path());
    fs::create_dir_all(dir.path().join("derivatives")).unwrap();
    fs::write(
        dir.path().join("derivatives/p01.jpg"),
        draw("small", &fixture["images"]),
    )
    .unwrap();
    let path = dir.path().join("catalog.db");
    let catalog = Catalog::open(&path)
        .unwrap()
        .with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
    let plans_dir = plans.path().to_string_lossy().into_owned();
    let env = [("GEOFRAME_BLOG_PLANS_DIR".to_string(), plans_dir)];
    let config = Config::load(
        |k| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()),
        &Overrides {
            data_dir: Some(dir.path().to_path_buf()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let limits = ConnLimits {
        idle_timeout: Duration::from_millis(400),
        ..ConnLimits::default()
    };
    let app = router(common::test_state(Arc::new(config), catalog));

    // 本番の起動（接続の情報つき）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let with_info = listener.local_addr().unwrap();
    let service = app
        .clone()
        .into_make_service_with_connect_info::<ConnInfo>();
    let listener = LimitedListener::new(listener, limits);
    tokio::spawn(async move { axum::serve(listener, service).await });
    // 接続の情報を付けない起動（比較用）
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let without_info = listener.local_addr().unwrap();
    let listener = LimitedListener::new(listener, limits);
    tokio::spawn(async move { axum::serve(listener, app).await });

    let body = json!({"trip_id": "sample-trip-a", "manifest": {
        "trip_id": "sample-trip-a", "status": "draft",
        "chapters": [{"id": "d1", "images": [{"photo_id": "1"}]}]}})
    .to_string();
    let send = |addr: std::net::SocketAddr| {
        let body = body.clone();
        tokio::spawn(async move {
            post_raw(
                addr,
                EXPORT,
                &[
                    ("Host".into(), "localhost".into()),
                    ("Content-Type".into(), "application/json".into()),
                    ("Content-Length".into(), body.len().to_string()),
                    ("Connection".into(), "close".into()),
                ],
                body.as_bytes(),
            )
            .await
        })
    };
    // 比較: 接続の情報が無い起動では、処理を止めている間に無通信の期限で切られる
    let hold = hold_derivatives();
    let cut = send(without_info);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    hold.release();
    assert!(
        cut.await.is_err(),
        "比較: 接続の情報が無いと、無通信の期限で切られる"
    );
    // 切られても書き出し自体は終わる（応答を受けた者がいないだけ）。済むのを待つ（来るはずの出来事だけを待つ）
    let index = dir.path().join("blog_exports/sample-trip-a/index.html");
    for _ in 0..2000 {
        if index.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(index.is_file());
    fs::remove_dir_all(dir.path().join("blog_exports")).unwrap();

    // 本番の起動: 同じ止め方でも、最後まで答える
    let hold = hold_derivatives();
    let kept = send(with_info);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    hold.release();
    let kept = kept.await.expect("接続の情報つきは最後まで答える");
    assert_eq!(kept.status, 200, "{}", kept.text());
    assert!(index.is_file());
    drop(db);
}
