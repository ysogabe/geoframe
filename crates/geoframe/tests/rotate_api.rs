//! 回転 API（E14 `POST /api/admin/rotate`）の結合テスト（T3.2）。合成データだけを使う。
//!
//! 現行 Python の `admin_rotate` に、合成の derivative（JPEG）を置いて同じ要求を順に流した結果
//! （Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_rotate_fixture.py` は P5b で削除した。再生成はできない）と、状態コード・応答の JSON・`photos` の変化・
//! データ置き場のファイルの増減（画像は寸法・四隅の色・EXIF の有無）が一致することを確かめる。意図した差は
//! フィクスチャの `known_differences` に載るケースだけで、下の `known` が期待値を持つ。

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::{TempDir, build_admin_catalog, post_raw, spawn_with_env};
use geoframe::db::Catalog;
use geoframe::writer::{OpenMode, Writer};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

const JSON_UTF8: &str = "application/json; charset=utf-8";
const ROTATE: &str = "/api/admin/rotate";

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rotate-python.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

// ---- 合成の画像（Python の `rotate_fixture.draw` と同じ絵） ----

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 160, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const YELLOW: [u8; 3] = [255, 255, 0];
const COLORS: [(&str, [u8; 3]); 4] = [
    ("red", RED),
    ("green", GREEN),
    ("blue", BLUE),
    ("yellow", YELLOW),
];
const GRAYS: [u8; 4] = [40, 110, 180, 250];
const MARK: usize = 10;

/// 向き 6 と GPS（`GPSLatitudeRef`）を持つ最小の EXIF の APP1 セグメント。
fn exif_segment() -> Vec<u8> {
    // TIFF（ビッグエンディアン）: IFD0 = Orientation(6)・GPS IFD へのポインタ、GPS IFD = GPSLatitudeRef('N')
    let mut tiff: Vec<u8> = b"MM\0*\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&[0, 2]); // エントリ 2 つ
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]); // Orientation SHORT 6
    tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 38]); // GPS IFD へ（オフセット 38）
    tiff.extend_from_slice(&[0, 0, 0, 0]); // 次の IFD なし
    assert_eq!(tiff.len(), 38);
    tiff.extend_from_slice(&[0, 1]); // GPS IFD のエントリ 1 つ
    tiff.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]); // GPSLatitudeRef ASCII "N"
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(&payload);
    segment
}

/// 名前の画像（`app/tests/rotate_fixture.py` の `IMAGES`）を JPEG（品質 90）にする。
pub fn draw(name: &str, images: &Value) -> Vec<u8> {
    let spec = images[name].as_array().unwrap();
    let gray = spec[0] == "L";
    let (w, h) = (
        usize::try_from(spec[1].as_u64().unwrap()).unwrap(),
        usize::try_from(spec[2].as_u64().unwrap()).unwrap(),
    );
    let with_exif = spec[3].as_bool().unwrap();
    let channels = if gray { 1 } else { 3 };
    let mut pixels = vec![0u8; w * h * channels];
    for y in 0..h {
        for x in 0..w {
            let quadrant = usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2);
            let p = &mut pixels[(y * w + x) * channels..][..channels];
            if gray {
                p[0] = GRAYS[quadrant];
            } else {
                p.copy_from_slice(&COLORS[quadrant].1);
            }
        }
    }
    let mut out = Vec::new();
    let color = if gray {
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
    if with_exif {
        // SOI の直後に APP1 を差し込む
        out.splice(2..2, exif_segment());
    }
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

/// ファイルの記録（`rotate_fixture.describe` と同じ形）。
pub fn describe(path: &Path) -> Value {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_symlink() {
        return json!({"kind": "symlink"});
    }
    if meta.is_dir() {
        return json!({"kind": "dir"});
    }
    let data = fs::read(path).unwrap();
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
        "corners": [at(MARK, MARK), at(w - 1 - MARK, MARK), at(MARK, h - 1 - MARK), at(w - 1 - MARK, h - 1 - MARK)],
        "exif": data.windows(6).any(|w| w == b"Exif\0\0"),
    })
}

/// データ置き場のファイル（`catalog.db*` を除く）→ 記録。リンクの先のディレクトリには降りない。
pub fn listing(root: &Path) -> BTreeMap<String, Value> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Value>) {
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
            out.insert(rel, describe(&path));
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
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
    /// 試験が DB を直接いじる・見るための接続（WAL を保つ）。
    db: Connection,
    addr: std::net::SocketAddr,
    images: Value,
}

impl Server {
    async fn start(with_writer: bool) -> Self {
        let dir = TempDir::new("rotate-api");
        let db = build_admin_catalog(dir.path());
        let path = dir.path().join("catalog.db");
        let mut catalog = Catalog::open(&path).unwrap();
        if with_writer {
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let addr = spawn_with_env(dir.path(), &[], catalog).await;
        Self {
            dir,
            db,
            addr,
            images: fixture()["images"].clone(),
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
                "garbage" => fs::write(&path, b"not an image").unwrap(),
                "dir" => fs::create_dir(&path).unwrap(),
                "symlink" => symlink(op["to"].as_str().unwrap(), &path).unwrap(),
                other => panic!("{other}"),
            }
        }
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

    async fn post_body(&self, target: &str, body: &[u8]) -> common::Response {
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
    Value::Object(out)
}

const NO_DISPLAY: &str = "表示用の画像がまだありません";
const BAD_DELTA: &str = "delta は 90 / 180 / 270 / -90 で指定してください";

/// Python と応答が違うケースの、Rust の期待値: (状態コード, `error` の文面)。どれもファイルと DB は変わらない。
fn known(name: &str) -> (u16, &'static str) {
    match name {
        "id_beyond_i64" => (404, "id=9223372036854775808 は存在しません"),
        "symlinked_display" | "display_outside_derivatives" => (400, NO_DISPLAY),
        "delta_float_90" | "delta_float_exponent" => (400, BAD_DELTA),
        other => panic!("{other} は known_differences に無い"),
    }
}

#[tokio::test]
async fn every_request_matches_python_status_body_row_changes_and_files() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    // 取りこぼしが無いよう規模を固定する（ケースを足したら更新する）
    assert_eq!(cases.len(), 57);
    let known_names: Vec<&str> = fixture["known_differences"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let server = Server::start(true).await;
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
        let (db_before, files_before) = (server.snapshot(), listing(server.dir.path()));
        let response = post_raw(
            server.addr,
            case["target"].as_str().unwrap(),
            &headers,
            &body,
        )
        .await;
        let changes = diff(&db_before, &server.snapshot());
        let files = files_diff(&files_before, &listing(server.dir.path()));

        if known_names.contains(&name) {
            seen_known += 1;
            let (status, error) = known(name);
            assert_eq!(response.status, status, "{name}");
            assert_eq!(response.json()["error"], error, "{name}");
            assert_eq!(changes, json!({}), "{name}: DB は変わらない");
            assert_eq!(
                files,
                json!({"added": {}, "removed": [], "changed": {}}),
                "{name}: ファイルは変わらない"
            );
            // Python は違う（だから known）。差が消えたら、この一覧から外す
            let py_same = case["status"].as_u64().unwrap() as u16 == response.status
                && case["body"] == response.json()
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
        assert_eq!(files, case["files"], "{name}: ファイルの増減");
        assert!(!response.headers.contains("access-control-"), "{name}");
    }
    assert_eq!(seen_known, known_names.len());
    // どのケースも `.part` を残さない
    let leftovers: Vec<String> = listing(server.dir.path())
        .into_keys()
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ---- 取り込みが無効（writer が無い）ときの 503 ----

#[tokio::test]
async fn without_a_writer_a_valid_rotation_is_503_before_any_file_work() {
    let server = Server::start(false).await;
    server.apply_setup(&fixture()["initial"]);
    let (db_before, files_before) = (server.snapshot(), listing(server.dir.path()));

    let response = server.post_body(ROTATE, br#"{"id": 2, "delta": 90}"#).await;
    assert_eq!(response.status, 503);
    let json = response.json();
    assert_eq!(json["ok"], false);
    assert!(json["error"].as_str().unwrap().contains("書き込みが無効"));
    let text = response.text();
    for secret in ["drive-2", "derivatives", "GEOFRAME", ".jpg", "catalog"] {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
    assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    // 存在の確認も writer の側なので、存在しない id も 503（setloc・setflag と同じ）
    let response = server
        .post_body(ROTATE, br#"{"id": 999, "delta": 90}"#)
        .await;
    assert_eq!(response.status, 503);
    // 何も書かず、ファイルも作らない（`.part` も新しい名前も無い）
    assert_eq!(server.snapshot(), db_before);
    assert_eq!(listing(server.dir.path()), files_before);

    // 入力の違反（400）は書き込みの有無によらず Python と同じ
    for (body, error) in [
        (&br#"{"id": 2, "delta": 45}"#[..], BAD_DELTA),
        (&br#"{"id": 0, "delta": 90}"#[..], "id が範囲外です"),
        (&br#"{"delta": 90}"#[..], "id は整数で指定してください"),
    ] {
        let response = server.post_body(ROTATE, body).await;
        assert_eq!(response.status, 400);
        assert_eq!(response.json()["error"], error);
    }
    assert_eq!(listing(server.dir.path()), files_before);
    // 読み取りは動く
    assert_eq!(common::get(server.addr, "/api/v1/photos").await.status, 200);
}

/// 回転の要求どうしが同時に来ても、1 つずつ順に反映される（90° を 4 本 → 元の向き・元の名前）。
#[tokio::test]
async fn simultaneous_rotations_of_one_photo_are_applied_one_after_another() {
    let server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let before = listing(server.dir.path());
    let addr = server.addr;
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            tokio::spawn(async move {
                let body = br#"{"id": 2, "delta": 90}"#;
                post_raw(
                    addr,
                    ROTATE,
                    &[
                        ("Host".into(), "localhost".into()),
                        ("Content-Type".into(), "application/json".into()),
                        ("Content-Length".into(), body.len().to_string()),
                        ("Connection".into(), "close".into()),
                    ],
                    body,
                )
                .await
            })
        })
        .collect();
    let mut responses = Vec::new();
    for task in tasks {
        responses.push(task.await.unwrap());
    }
    let mut rotations: Vec<u64> = responses
        .iter()
        .map(|r| {
            assert_eq!(r.status, 200, "{}", r.text());
            r.json()["rotation"].as_u64().unwrap()
        })
        .collect();
    rotations.sort_unstable();
    assert_eq!(rotations, [0, 90, 180, 270]);
    // 最後は元の名前・元の向きで、ファイルは 1 つだけ（孤児が残らない）
    let after = listing(server.dir.path());
    let image = &after["derivatives/drive-2.jpg"];
    assert_eq!(image["size"], json!([160, 96]));
    assert_eq!(image["corners"], json!(["red", "green", "blue", "yellow"]));
    let derivatives_of_2: Vec<&String> = after
        .keys()
        .filter(|name| name.starts_with("derivatives/drive-2"))
        .collect();
    assert_eq!(derivatives_of_2, ["derivatives/drive-2.jpg"]);
    assert!(!before.contains_key("derivatives/drive-2.jpg"));
    assert_eq!(server.snapshot()[&2]["rotation"], json!(0));
    assert_eq!(
        server.snapshot()[&2]["display"],
        json!("derivatives/drive-2.jpg")
    );
}

/// 棚卸し S6: 回したあと、表示のURL（E23）は新しい名前で200、古い名前は404、写真フィード（E1）の`file`も
/// 新しい名前になる（miharashi 側のキャッシュ回避）。
#[tokio::test]
async fn after_a_rotation_the_new_url_is_served_and_the_feed_names_it() {
    let server = Server::start(true).await;
    server.apply_setup(&fixture()["initial"]);
    let file_of = |feed: &Value, id: i64| {
        feed.as_array()
            .expect("写真一覧は配列")
            .iter()
            .find(|p| p["id"] == id)
            .map(|p| p["file"].as_str().unwrap().to_string())
    };
    let feed = common::get(server.addr, "/api/v1/photos").await.json();
    assert_eq!(file_of(&feed, 2).as_deref(), Some("derivatives/p02.jpg"));
    assert_eq!(
        common::get(server.addr, "/derivatives/p02.jpg")
            .await
            .status,
        200
    );

    let response = server.post_body(ROTATE, br#"{"id": 2, "delta": 90}"#).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["thumb"], "/derivatives/drive-2.r90.jpg");

    let image = common::get(server.addr, "/derivatives/drive-2.r90.jpg").await;
    assert_eq!(image.status, 200);
    assert_eq!(image.header("content-type"), Some("image/jpeg"));
    assert_eq!(
        image.body,
        fs::read(server.dir.path().join("derivatives/drive-2.r90.jpg")).unwrap()
    );
    assert_eq!(
        common::get(server.addr, "/derivatives/p02.jpg")
            .await
            .status,
        404
    );
    let feed = common::get(server.addr, "/api/v1/photos").await.json();
    assert_eq!(
        file_of(&feed, 2).as_deref(),
        Some("derivatives/drive-2.r90.jpg")
    );
}
