//! 表示用 JPEG の取得・区画の検査・作業領域・manifest（design.md 4.1 の手順 5〜7、6.3、8 章、10 章の AC3・AC4・AC5b・
//! 「一部の失敗」・「上限」）。偽の写真サービス（127.0.0.1）だけに接続する。合成の JPEG は jpeg-encoder で作る。

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::time::{Duration, Instant};

use common::{FakeSite, Reply, TempDir, closed_port_base};
use geoframe_export::date::Period;
use geoframe_export::error::{ErrorCode, ExportError};
use geoframe_export::image::{
    ImageClient, Limits, PrepareError, Prepared, prepare, prepare_with_clock,
};
use geoframe_export::jpeg::{self, InspectError, Inspection};
use geoframe_export::listing::{ExcludeReason, Selection, select};
use geoframe_export::measure;
use geoframe_export::request::Destination;
use geoframe_export::staging::Workspace;
use serde_json::{Value, json};

// ---- 合成の JPEG と、その加工 ----

fn rgb_pattern(w: u16, h: u16, seed: u8) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(usize::from(w) * usize::from(h) * 3);
    for y in 0..h {
        for x in 0..w {
            rgb.extend([
                (x as u8).wrapping_mul(3).wrapping_add(seed),
                (y as u8).wrapping_mul(5),
                (x as u8 ^ y as u8).wrapping_add(seed),
            ]);
        }
    }
    rgb
}

fn jpeg_bytes(w: u16, h: u16, seed: u8) -> Vec<u8> {
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85)
        .encode(&rgb_pattern(w, h, seed), w, h, jpeg_encoder::ColorType::Rgb)
        .expect("encode");
    out
}

/// SOI の直後に、`FF <marker> <長さ> <本体>` の区画を足す。
fn with_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, marker]);
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
    out.extend(&jpeg[2..]);
    out
}

fn with_trailing(jpeg: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut out = jpeg.to_vec();
    out.extend(tail);
    out
}

// ---- 一覧の応答と偽の写真サービス ----

fn period1() -> Period {
    Period::parse(Some("2030-03-10"), None).unwrap()
}

fn period2() -> Period {
    Period::parse(Some("2030-03-10"), Some("2030-03-11")).unwrap()
}

fn row(id: i64, time: &str) -> Value {
    json!({"id": id, "file": format!("derivatives/p{id}.jpg"), "title": format!("題{id}"),
           "lat": 12.345678901234567, "lon": 98.76543210987654, "time": time, "elev": 3.0, "inferred": false})
}

fn selection(period: Period, photos: Vec<Value>) -> Selection {
    let response = json!({"from": period.from().to_string(), "to": period.to().to_string(),
                          "photos": photos, "undated": 4, "invalid_time": 5, "truncated": false});
    select(&response, &period).expect("一覧の検査に通る")
}

fn route(id: i64, reply: Reply) -> (String, Reply) {
    (format!("/derivatives/p{id}.jpg"), reply)
}

fn jpeg_route(id: i64, bytes: &[u8]) -> (String, Reply) {
    route(id, Reply::jpeg(bytes.to_vec()))
}

fn client(site: &FakeSite) -> ImageClient {
    ImageClient::new(&site.base)
}

fn limits(f: impl FnOnce(&mut Limits)) -> Limits {
    let mut l = Limits::default();
    f(&mut l);
    l
}

fn code<T: std::fmt::Debug, E: std::fmt::Debug + Into<ExportError>>(r: Result<T, E>) -> ErrorCode {
    r.expect_err("失敗するはず").into().code
}

fn code_of<T: std::fmt::Debug, E: std::fmt::Debug + Into<ExportError>>(
    name: &str,
    r: Result<T, E>,
) -> ErrorCode {
    r.expect_err(&format!("{name}: 失敗するはず")).into().code
}

fn read_manifest(ws: &Workspace) -> Value {
    serde_json::from_slice(&std::fs::read(ws.path().join("manifest.json")).unwrap()).unwrap()
}

fn run(
    site: &FakeSite,
    selection: &Selection,
    tmp: &TempDir,
) -> (Workspace, Result<Prepared, PrepareError>) {
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let result = prepare(&client(site), selection, Destination::Yamareco, &ws, None);
    (ws, result)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                walk(base, &e.path(), out);
            } else {
                out.push(
                    e.path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

// ---- AC4: 書き出し、manifest ----

#[test]
fn a_single_day_is_written_byte_for_byte_with_a_complete_manifest() {
    let tmp = TempDir::new("single");
    let (a, b, c) = (
        jpeg_bytes(64, 48, 1),
        jpeg_bytes(80, 60, 2),
        jpeg_bytes(48, 64, 3),
    );
    let mut rows = vec![
        row(3, "2030-03-10 12:00"),
        row(1, "2030-03-10 09:11"),
        row(2, "2030-03-10 09:11"),
    ];
    // 2: 位置なし・標高なし、3: 推定の位置・標高なし
    rows[2]["lat"] = Value::Null;
    rows[2]["lon"] = Value::Null;
    rows[2]["elev"] = Value::Null;
    rows[0]["inferred"] = json!(true);
    rows[0].as_object_mut().unwrap().remove("elev");
    let site = FakeSite::start(vec![
        jpeg_route(1, &a),
        jpeg_route(2, &b),
        jpeg_route(3, &c),
    ]);
    let sel = selection(period1(), rows);
    let (ws, r) = run(&site, &sel, &tmp);
    let prepared = r.unwrap();

    // 配信されたバイト列がそのまま作業領域に書かれる（再エンコードしない）。名前は HHMM00_<id>.jpg
    assert_eq!(
        files_in(ws.path()),
        [
            "091100_1.jpg",
            "091100_2.jpg",
            "120000_3.jpg",
            "manifest.json"
        ]
    );
    for (name, bytes) in [
        ("091100_1.jpg", &a),
        ("091100_2.jpg", &b),
        ("120000_3.jpg", &c),
    ] {
        assert_eq!(
            &std::fs::read(ws.path().join(name)).unwrap(),
            bytes,
            "{name}"
        );
    }
    // 結果の数と、公開用の一覧（manifest が最後）
    assert_eq!(prepared.folder.as_str(), "2030-03-10");
    assert_eq!((prepared.catalog_count, prepared.count), (3, 3));
    assert!(prepared.excluded.is_empty());
    assert_eq!(prepared.photo_bytes, (a.len() + b.len() + c.len()) as u64);
    assert_eq!(
        (
            prepared.undated_in_catalog,
            prepared.invalid_time_in_catalog
        ),
        (4, 5)
    );
    let paths: Vec<&str> = prepared.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "091100_1.jpg",
            "091100_2.jpg",
            "120000_3.jpg",
            "manifest.json"
        ]
    );
    for f in &prepared.files {
        assert_eq!(
            f.size,
            std::fs::metadata(ws.path().join(&f.path)).unwrap().len(),
            "{}",
            f.path
        );
    }
    assert_eq!(prepared.files.last().unwrap().size, prepared.manifest_bytes);

    let m = read_manifest(&ws);
    assert!(m.get("days").is_none());
    assert_eq!(m["date"], "2030-03-10");
    assert_eq!(
        (m["count"].as_u64(), m["catalog_count"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(m["scope"], "all_visible");
    assert_eq!(m["source"], "GEOFRAME /api/admin/export/photos");
    assert_eq!(m["destination"], "yamareco");
    assert_eq!(m["folder"], "2030-03-10");
    assert_eq!(m["generator"], geoframe_export::manifest::GENERATOR);
    assert!(
        m["generator"]
            .as_str()
            .unwrap()
            .starts_with("geoframe-export ")
    );
    assert_eq!(m["excluded"], json!([]));
    assert_eq!(
        (
            m["undated_in_catalog"].as_u64(),
            m["invalid_time_in_catalog"].as_u64()
        ),
        (Some(4), Some(5))
    );

    let photos = m["photos"].as_array().unwrap();
    // 時刻・id の昇順
    let ids: Vec<i64> = photos.iter().map(|p| p["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, [1, 2, 3]);
    // 全部のキーが、決まった順で出る（キーを省かない）
    let keys: Vec<&str> = photos[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "filename",
            "title",
            "taken_at",
            "lat",
            "lon",
            "width",
            "height",
            "file_size",
            "mean_brightness",
            "sharpness_laplacian_var",
            "spot",
            "nearest_spot_distance_m",
            "gps_inferred",
            "elev",
        ]
    );
    for p in photos {
        assert_eq!(p["spot"], "");
        assert!(p["nearest_spot_distance_m"].is_null());
        assert!(p.as_object().unwrap().contains_key("elev"));
    }
    // 1: 位置あり・標高あり・推定でない。座標は丸めずにそのまま
    let p1 = &photos[0];
    assert_eq!(p1["filename"], "091100_1.jpg");
    assert_eq!(p1["title"], "題1");
    assert_eq!(p1["taken_at"], "2030-03-10 09:11");
    assert_eq!(p1["lat"].as_f64(), Some(12.345678901234567));
    assert_eq!(p1["lon"].as_f64(), Some(98.76543210987654));
    assert_eq!(p1["elev"].as_f64(), Some(3.0));
    assert_eq!(p1["gps_inferred"], false);
    assert_eq!(
        (p1["width"].as_u64(), p1["height"].as_u64()),
        (Some(64), Some(48))
    );
    assert_eq!(p1["file_size"].as_u64(), Some(a.len() as u64));
    // 2: 位置なし。lat/lon は null、gps_inferred は false、elev は null（キーは出る）
    let p2 = &photos[1];
    assert!(p2["lat"].is_null() && p2["lon"].is_null() && p2["elev"].is_null());
    assert_eq!(p2["gps_inferred"], false);
    // 3: 推定の位置。標高の欠けは null
    let p3 = &photos[2];
    assert_eq!(p3["gps_inferred"], true);
    assert!(p3["elev"].is_null());
    assert_eq!(p3["filename"], "120000_3.jpg");
    // 明るさ・シャープネスはデコードした画像の値（小数 2 桁）
    let header = match jpeg::inspect(&a, 60_000_000).unwrap() {
        Inspection::Clean(h) => h,
        Inspection::HasMetadata => panic!(),
    };
    let gray = jpeg::decode_gray(&a, header).unwrap();
    let want = measure::measure(&gray.data, gray.width, gray.height);
    assert_eq!(p1["mean_brightness"].as_f64(), Some(want.mean_brightness));
    assert_eq!(
        p1["sharpness_laplacian_var"].as_f64(),
        Some(want.sharpness_laplacian_var)
    );
}

#[test]
fn a_period_uses_date_folders_and_days() {
    let tmp = TempDir::new("period");
    let imgs: Vec<Vec<u8>> = (1..=4)
        .map(|i| jpeg_bytes(40 + i as u16, 30, i as u8))
        .collect();
    let rows = vec![
        row(4, "2030-03-11 23:59"),
        row(1, "2030-03-10 00:00"),
        row(3, "2030-03-11 08:05"),
        row(2, "2030-03-10 09:11"),
    ];
    let site = FakeSite::start(
        (1..=4)
            .map(|i| jpeg_route(i, &imgs[i as usize - 1]))
            .collect(),
    );
    let sel = selection(period2(), rows);
    let (ws, r) = run(&site, &sel, &tmp);
    let prepared = r.unwrap();
    assert_eq!(prepared.folder.as_str(), "2030-03-10-11");
    assert_eq!(
        files_in(ws.path()),
        [
            "2030-03-10/000000_1.jpg",
            "2030-03-10/091100_2.jpg",
            "2030-03-11/080500_3.jpg",
            "2030-03-11/235900_4.jpg",
            "manifest.json"
        ]
    );
    for d in ["2030-03-10", "2030-03-11"] {
        let mode = std::fs::metadata(ws.path().join(d))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{d}");
    }
    let m = read_manifest(&ws);
    assert_eq!(m["date"], "2030-03-10/2030-03-11");
    assert_eq!(m["folder"], "2030-03-10-11");
    assert_eq!(m["photos"][0]["filename"], "2030-03-10/000000_1.jpg");
    assert_eq!(m["photos"][3]["filename"], "2030-03-11/235900_4.jpg");
    assert_eq!(
        m["days"],
        json!({
            "2030-03-10": {"count": 2, "first": "2030-03-10 00:00", "last": "2030-03-10 09:11", "ids": [1, 2]},
            "2030-03-11": {"count": 2, "first": "2030-03-11 08:05", "last": "2030-03-11 23:59", "ids": [3, 4]},
        })
    );
    // キーの順: days は notes と photos の間
    let keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "generator",
            "date",
            "destination",
            "folder",
            "scope",
            "source",
            "count",
            "catalog_count",
            "excluded",
            "undated_in_catalog",
            "invalid_time_in_catalog",
            "notes",
            "days",
            "photos"
        ]
    );
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 旧 manifest の各キーが、新しい manifest に同じ相対順で、同じ型で入っていること。
fn assert_keeps_old_shape(old: &Value, new: &Value, at: &str) {
    let (o, n) = (old.as_object().unwrap(), new.as_object().unwrap());
    let new_keys: Vec<&String> = n.keys().collect();
    let mut last = None;
    for k in o.keys() {
        let pos = new_keys
            .iter()
            .position(|x| *x == k)
            .unwrap_or_else(|| panic!("{at}.{k} が無い"));
        assert!(last.is_none_or(|l| pos > l), "{at}.{k} の順が違う");
        last = Some(pos);
        assert_eq!(json_type(&o[k]), json_type(&n[k]), "{at}.{k} の型");
    }
}

#[test]
fn the_manifest_keeps_every_key_of_the_old_manifest() {
    let site = FakeSite::start(vec![
        jpeg_route(101, &jpeg_bytes(64, 48, 1)),
        jpeg_route(102, &jpeg_bytes(64, 48, 2)),
    ]);
    for (fixture, period) in [
        ("old_manifest_single.json", period1()),
        ("old_manifest_period.json", period2()),
    ] {
        let old: Value = serde_json::from_slice(
            &std::fs::read(format!(
                "{}/tests/fixtures/{fixture}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        )
        .unwrap();
        let mut rows = vec![row(101, "2030-03-10 09:11")];
        rows[0]["inferred"] = json!(true);
        if !period.is_single_day() {
            rows.push(row(102, "2030-03-11 10:00"));
            rows[1]["title"] = json!("");
        }
        let sel = selection(period, rows);
        let dir = TempDir::new(&format!("oldshape-{fixture}"));
        let ws = Workspace::create(dir.path(), "job1").unwrap();
        prepare(&client(&site), &sel, Destination::TravelJournal, &ws, None).unwrap();
        let new = read_manifest(&ws);
        assert_keeps_old_shape(&old, &new, "manifest");
        assert_keeps_old_shape(&old["notes"], &new["notes"], "notes");
        assert_keeps_old_shape(&old["photos"][0], &new["photos"][0], "photos[0]");
        // 旧 manifest のキーと、足したキーだけ
        let mut want: Vec<&str> = old
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        want.extend([
            "generator",
            "destination",
            "folder",
            "excluded",
            "undated_in_catalog",
            "invalid_time_in_catalog",
            "scope",
        ]);
        want.sort_unstable();
        let mut got: Vec<&str> = new
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        got.sort_unstable();
        assert_eq!(got, want, "{fixture}");
        assert_eq!(new["destination"], "travel-journal");
        // 写真 1 枚のキーは、旧 manifest と同じ集合・同じ順
        let old_keys: Vec<&String> = old["photos"][0].as_object().unwrap().keys().collect();
        let new_keys: Vec<&String> = new["photos"][0].as_object().unwrap().keys().collect();
        assert_eq!(old_keys, new_keys, "{fixture}");
        if !period.is_single_day() {
            assert_keeps_old_shape(
                &old["days"]["2030-03-10"],
                &new["days"]["2030-03-10"],
                "days[..]",
            );
            let old_keys: Vec<&String> = old["days"]["2030-03-10"]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            let new_keys: Vec<&String> = new["days"]["2030-03-10"]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            assert_eq!(old_keys, new_keys);
            assert_eq!(new["photos"][1]["title"], "");
        }
    }
}

#[test]
fn the_same_input_gives_the_same_manifest_bytes() {
    let imgs = [jpeg_bytes(64, 48, 1), jpeg_bytes(64, 48, 2)];
    let site = FakeSite::start(vec![jpeg_route(1, &imgs[0]), jpeg_route(2, &imgs[1])]);
    let sel = selection(
        period1(),
        vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")],
    );
    let mut outputs = Vec::new();
    for name in ["det-a", "det-b"] {
        let tmp = TempDir::new(name);
        let (ws, r) = run(&site, &sel, &tmp);
        r.unwrap();
        outputs.push(std::fs::read(ws.path().join("manifest.json")).unwrap());
    }
    assert_eq!(outputs[0], outputs[1]);
    // 時刻やジョブ ID は入らない
    let text = String::from_utf8(outputs.remove(0)).unwrap();
    assert!(!text.contains("job1"));
}

// ---- AC3: 除外 ----

#[test]
fn files_with_metadata_sections_are_excluded_and_the_export_continues() {
    let tmp = TempDir::new("metadata");
    let base = jpeg_bytes(64, 48, 1);
    // ICC プロファイルは許す
    let icc = {
        let mut out = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut out, 85);
        enc.add_icc_profile(&[7u8; 100]).unwrap();
        enc.encode(
            &rgb_pattern(64, 48, 2),
            64,
            48,
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
        out
    };
    assert!(icc.windows(12).any(|w| w == b"ICC_PROFILE\0"));
    let cases: Vec<(i64, &str, Vec<u8>, bool)> = vec![
        (1, "clean", base.clone(), true),
        (
            2,
            "exif",
            with_segment(&base, 0xE1, b"Exif\0\0MM\0*"),
            false,
        ),
        (
            3,
            "xmp",
            with_segment(&base, 0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>"),
            false,
        ),
        (4, "comment", with_segment(&base, 0xFE, b"hello"), false),
        (
            5,
            "mpf",
            with_segment(&base, 0xE2, b"MPF\0\x4d\x4d\0*"),
            false,
        ),
        (
            6,
            "iptc",
            with_segment(&base, 0xED, b"Photoshop 3.0\0"),
            false,
        ),
        (7, "app11", with_segment(&base, 0xEB, b"JP"), false),
        (
            8,
            "trailing",
            with_trailing(&base, b"EXIF-after-EOI"),
            false,
        ),
        (9, "icc", icc.clone(), true),
        (10, "jfxx", with_segment(&base, 0xE0, b"JFXX\0\x10"), false),
        (
            11,
            "jfif-thumbnail",
            with_segment(&base, 0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\x01\x01\xff\0\0"),
            false,
        ),
        (12, "newline", with_trailing(&base, b"\n"), false),
    ];
    let routes: Vec<_> = cases
        .iter()
        .map(|(id, _, b, _)| jpeg_route(*id, b))
        .collect();
    let rows: Vec<Value> = cases
        .iter()
        .map(|(id, ..)| row(*id, &format!("2030-03-10 {:02}:00", id)))
        .collect();
    let site = FakeSite::start(routes);
    let sel = selection(period1(), rows);
    let (ws, r) = run(&site, &sel, &tmp);
    let prepared = r.unwrap();

    let kept: Vec<i64> = cases.iter().filter(|c| c.3).map(|c| c.0).collect();
    assert_eq!(kept, [1, 9]);
    assert_eq!(prepared.catalog_count, 12);
    assert_eq!(prepared.count, 2);
    let excluded: Vec<(i64, &str)> = prepared
        .excluded
        .iter()
        .map(|e| (e.id, e.reason.as_str()))
        .collect();
    let want: Vec<(i64, &str)> = cases
        .iter()
        .filter(|c| !c.3)
        .map(|c| (c.0, "has_metadata"))
        .collect();
    assert_eq!(excluded, want);
    // 外した写真は書かれない。manifest に理由付きで出る
    assert_eq!(
        files_in(ws.path()),
        ["010000_1.jpg", "090000_9.jpg", "manifest.json"]
    );
    let m = read_manifest(&ws);
    assert_eq!(m["count"], 2);
    assert_eq!(m["catalog_count"], 12);
    assert_eq!(m["excluded"][0], json!({"id": 2, "reason": "has_metadata"}));
    assert_eq!(m["excluded"].as_array().unwrap().len(), 10);
    // count = catalog_count − 除外
    assert_eq!(
        prepared.count + prepared.excluded.len(),
        prepared.catalog_count
    );
}

#[test]
fn non_derivative_files_are_excluded_without_being_fetched() {
    let tmp = TempDir::new("notderiv");
    let good = jpeg_bytes(64, 48, 1);
    let mut rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
    rows[1]["file"] = json!("local/IMG_0001.jpg");
    let site = FakeSite::start(vec![jpeg_route(1, &good)]);
    let sel = selection(period1(), rows);
    let (ws, r) = run(&site, &sel, &tmp);
    let prepared = r.unwrap();
    assert_eq!(
        prepared
            .excluded
            .iter()
            .map(|e| (e.id, e.reason))
            .collect::<Vec<_>>(),
        [(2, ExcludeReason::NotDerivative)]
    );
    assert_eq!((prepared.catalog_count, prepared.count), (2, 1));
    // 取りに行かない（原本のパスにも、他のパスにも要求を出さない）
    assert_eq!(site.request_lines(), ["GET /derivatives/p1.jpg HTTP/1.1"]);
    assert_eq!(
        read_manifest(&ws)["excluded"][0],
        json!({"id": 2, "reason": "not_derivative"})
    );
}

#[test]
fn nothing_to_export_when_everything_is_excluded() {
    // 全部が not_derivative: 画像を 1 枚も取らない。失敗の値に、候補の数と除外の理由が全部入っている
    let tmp = TempDir::new("nothing-a");
    let mut rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
    for r in &mut rows {
        r["file"] = json!("local/x.jpg");
    }
    let site = FakeSite::start(vec![]);
    let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
    let err = r.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NothingToExport);
    assert_eq!(err.catalog_count, 2);
    assert_eq!(
        reasons(&err),
        [
            (1, ExcludeReason::NotDerivative),
            (2, ExcludeReason::NotDerivative)
        ]
    );
    assert_eq!(
        (err.undated_in_catalog, err.invalid_time_in_catalog),
        (4, 5)
    );
    assert!(site.request_lines().is_empty());
    assert_eq!(files_in(ws.path()), Vec::<String>::new());

    // not_derivative と has_metadata だけ: manifest も作らない。除外の理由は候補の順で全部残る
    let tmp = TempDir::new("nothing-b");
    let with_exif = with_segment(&jpeg_bytes(64, 48, 1), 0xE1, b"Exif\0\0");
    let mut rows = vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 10:00"),
        row(3, "2030-03-10 11:00"),
    ];
    rows[1]["file"] = json!("local/x.jpg");
    let site = FakeSite::start(vec![jpeg_route(1, &with_exif), jpeg_route(3, &with_exif)]);
    let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
    let err = r.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NothingToExport);
    assert_eq!(err.catalog_count, 3);
    assert_eq!(
        reasons(&err),
        [
            (1, ExcludeReason::HasMetadata),
            (2, ExcludeReason::NotDerivative),
            (3, ExcludeReason::HasMetadata)
        ]
    );
    assert_eq!(files_in(ws.path()), Vec::<String>::new());
}

fn reasons(e: &PrepareError) -> Vec<(i64, ExcludeReason)> {
    e.excluded.iter().map(|x| (x.id, x.reason)).collect()
}

#[test]
fn other_failures_carry_the_counts_known_so_far() {
    // 1 枚目は原本のパス（not_derivative）、2 枚目は区画の検査で外れ、3 枚目が壊れている
    let tmp = TempDir::new("partial-counts");
    let with_exif = with_segment(&jpeg_bytes(64, 48, 1), 0xE1, b"Exif\0\0");
    let mut rows = vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 10:00"),
        row(3, "2030-03-10 11:00"),
        row(4, "2030-03-10 12:00"),
    ];
    rows[0]["file"] = json!("local/x.jpg");
    let site = FakeSite::start(vec![
        jpeg_route(2, &with_exif),
        jpeg_route(3, b"not a jpeg"),
        jpeg_route(4, &jpeg_bytes(64, 48, 2)),
    ]);
    let (_ws, r) = run(&site, &selection(period1(), rows), &tmp);
    let err = r.unwrap_err();
    assert_eq!(err.code(), ErrorCode::DecodeFailed);
    assert_eq!(err.catalog_count, 4);
    assert_eq!(
        reasons(&err),
        [
            (1, ExcludeReason::NotDerivative),
            (2, ExcludeReason::HasMetadata)
        ]
    );
    // 4 枚目は取りに行かない
    assert_eq!(site.request_lines().len(), 2);
}

#[test]
fn a_pending_derivative_fails_before_any_fetch() {
    let tmp = TempDir::new("pending");
    let mut rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
    rows[1]["file"] = Value::Null;
    let site = FakeSite::start(vec![jpeg_route(1, &jpeg_bytes(64, 48, 1))]);
    let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
    assert_eq!(code(r), ErrorCode::DerivativePending);
    assert!(site.request_lines().is_empty());
    assert_eq!(files_in(ws.path()), Vec::<String>::new());
}

#[test]
fn an_empty_candidate_list_is_not_prepared() {
    // 候補 0 枚は呼び出し側（T5）が成功として終える。ここに来たら食い違い
    let tmp = TempDir::new("empty");
    let site = FakeSite::start(vec![]);
    let (_ws, r) = run(&site, &selection(period1(), vec![]), &tmp);
    assert_eq!(code(r), ErrorCode::Internal);
    assert!(site.request_lines().is_empty());
}

// ---- AC5b と「一部の失敗」: 1 枚の取得・デコードの失敗 ----

#[test]
fn one_failed_fetch_fails_the_whole_export() {
    let good = jpeg_bytes(64, 48, 1);
    let cases: Vec<(&str, Reply)> = vec![
        ("404", Reply::status(404)),
        ("500", Reply::status(500)),
        (
            "cut off",
            Reply::Raw {
                head: "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n"
                    .into(),
                body: common::Body::Bytes(good[..50].to_vec()),
            },
        ),
    ];
    for (name, reply) in cases {
        let tmp = TempDir::new(&format!("fetchfail-{}", name.replace(' ', "-")));
        let site = FakeSite::start(vec![
            jpeg_route(1, &good),
            route(2, reply),
            jpeg_route(3, &good),
        ]);
        let rows = vec![
            row(1, "2030-03-10 09:00"),
            row(2, "2030-03-10 10:00"),
            row(3, "2030-03-10 11:00"),
        ];
        let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
        assert_eq!(code(r), ErrorCode::FetchFailed, "{name}");
        // 公開用の manifest は作られない（途中までの写真は作業領域に残るが、呼び出し側が消す）
        assert!(!ws.path().join("manifest.json").exists(), "{name}");
    }
}

#[test]
fn a_refused_connection_is_fetch_failed() {
    let tmp = TempDir::new("refused");
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    let c = ImageClient::new(&closed_port_base());
    assert_eq!(
        code(prepare(&c, &sel, Destination::Yamareco, &ws, None)),
        ErrorCode::FetchFailed
    );
}

#[test]
fn bad_jpegs_fail_the_whole_export_as_decode_failed() {
    // 走査データが大きい画像（切れた画像の、ヘッダの途中ではなく画像データの途中で切る）
    let good = jpeg_bytes(200, 150, 1);
    let truncated = good[..good.len() - 40].to_vec(); // EOI が無い（転送の途中で切れた）
    let no_eoi = good[..good.len() - 2].to_vec();
    let in_headers = good[..30].to_vec();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("garbage", b"not a jpeg at all".to_vec()),
        ("html", b"<html><body>error</body></html>".to_vec()),
        ("empty", Vec::new()),
        ("png", b"\x89PNG\r\n\x1a\n....".to_vec()),
        ("truncated", truncated),
        ("no eoi", no_eoi),
        ("cut in headers", in_headers),
        ("soi only", vec![0xFF, 0xD8]),
        ("soi eoi", vec![0xFF, 0xD9]),
        ("soi eoi 2", vec![0xFF, 0xD8, 0xFF, 0xD9]),
    ];
    for (name, bad) in cases {
        let tmp = TempDir::new(&format!("decodefail-{}", name.replace(' ', "-")));
        let site = FakeSite::start(vec![jpeg_route(1, &good), jpeg_route(2, &bad)]);
        let rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
        let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
        assert_eq!(code_of(name, r), ErrorCode::DecodeFailed, "{name}");
        assert!(!ws.path().join("manifest.json").exists(), "{name}");
        assert!(
            !ws.path().join("100000_2.jpg").exists(),
            "{name}: 壊れた画像は書かない"
        );
    }
}

/// SOI の後ろで、最初に現れる `FF <marker>` の区画の本体の位置（`nth` 番目）。
fn payload_at(jpeg: &[u8], marker: u8, nth: usize) -> usize {
    let (mut pos, mut seen) = (2, 0);
    loop {
        assert_eq!(jpeg[pos], 0xFF);
        let len = usize::from(jpeg[pos + 2]) << 8 | usize::from(jpeg[pos + 3]);
        if jpeg[pos + 1] == marker {
            if seen == nth {
                return pos + 4;
            }
            seen += 1;
        }
        assert_ne!(jpeg[pos + 1], 0xDA, "区画 {marker:#x} が見つかりません");
        pos += 2 + len;
    }
}

#[test]
fn jpegs_that_pass_the_section_check_but_not_the_decoder_fail_as_decode_failed() {
    // 区画の検査（並びと長さ）は通るが、デコーダが拒む画像。表の中身・成分・走査データを壊す
    let good = jpeg_bytes(200, 150, 1);
    let dht = payload_at(&good, 0xC4, 0);
    let sof = payload_at(&good, 0xC0, 0);
    let dqt = payload_at(&good, 0xDB, 0);
    let sos = payload_at(&good, 0xDA, 0);
    let scan = sos - 2 + (usize::from(good[sos - 2]) << 8 | usize::from(good[sos - 1]));
    let patch = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut j = good.clone();
        f(&mut j);
        j
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        // ハフマン表: 符号の長さごとの個数が、全部 255（256 個を超える）／符号が足りなくなる並び
        (
            "huffman table counts too large",
            patch(&|j| j[dht + 1..dht + 17].fill(0xFF)),
        ),
        (
            "huffman table oversubscribed",
            patch(&|j| j[dht + 1..dht + 4].fill(2)),
        ),
        // SOF: 精度 12 ビット、成分 0 個、標本化の比が 0
        ("sof precision 12", patch(&|j| j[sof] = 12)),
        ("sof no components", patch(&|j| j[sof + 5] = 0)),
        ("sof sampling factor 0", patch(&|j| j[sof + 7] = 0)),
        // DQT: 精度と番号の組が不正
        ("dqt bad id", patch(&|j| j[dqt] = 0x1F)),
        // SOS: 使うハフマン表が定義されていない
        ("sos undefined tables", patch(&|j| j[sos + 2] = 0x77)),
        // 走査データ: ハフマン符号として成り立たないビット列（FF 00 の詰め物が続く）
        (
            "invalid huffman codes in the scan",
            patch(&|j| {
                for (i, b) in j[scan + 10..scan + 50].iter_mut().enumerate() {
                    *b = if i % 2 == 0 { 0xFF } else { 0x00 };
                }
            }),
        ),
    ];
    for (name, bad) in cases {
        // 区画の検査は通る（止めるのはデコーダ）
        let Ok(Inspection::Clean(header)) = jpeg::inspect(&bad, 60_000_000) else {
            panic!("{name}: 区画の検査で止まってしまう");
        };
        assert!(jpeg::decode_gray(&bad, header).is_err(), "{name}");

        let tmp = TempDir::new(&format!("decoder-{}", name.replace(' ', "-")));
        let site = FakeSite::start(vec![jpeg_route(1, &good), jpeg_route(2, &bad)]);
        let rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
        let (ws, r) = run(&site, &selection(period1(), rows), &tmp);
        let err = r.expect_err(name);
        assert_eq!(err.code(), ErrorCode::DecodeFailed, "{name}");
        // 2 枚目まで取りに行き、2 枚目で止まる。manifest は作らない。壊れた画像は書かない
        assert_eq!(site.request_lines().len(), 2, "{name}");
        assert!(!ws.path().join("manifest.json").exists(), "{name}");
        assert!(!ws.path().join("100000_2.jpg").exists(), "{name}");
    }
}

#[test]
fn structure_errors_win_over_metadata() {
    // 区画の長さが残りを超える APP1 は、メタデータの除外ではなく壊れたファイル
    let good = jpeg_bytes(64, 48, 1);
    let mut broken = good[..2].to_vec();
    broken.extend([0xFF, 0xE1, 0xFF, 0xFF, 1, 2, 3]);
    assert_eq!(
        jpeg::inspect(&broken, 60_000_000).unwrap_err(),
        InspectError::Malformed("JPEG の区画の長さが残りのバイト数を超えています")
    );
    // 区画の間に余計なバイトがある
    let mut junk = good[..2].to_vec();
    junk.extend([0x00, 0x11]);
    junk.extend(&good[2..]);
    assert!(matches!(
        jpeg::inspect(&junk, 60_000_000),
        Err(InspectError::Malformed(_))
    ));
}

// ---- 上限 ----

#[test]
fn the_image_size_limit_counts_bytes_read_and_allows_the_exact_size() {
    let good = jpeg_bytes(64, 48, 1);
    let len = good.len() as u64;
    for (max, ok) in [(len, true), (len - 1, false), (len + 1, true)] {
        let tmp = TempDir::new(&format!("maxbytes-{max}"));
        let site = FakeSite::start(vec![jpeg_route(1, &good)]);
        let ws = Workspace::create(tmp.path(), "job1").unwrap();
        let c = ImageClient::with_limits(&site.base, limits(|l| l.max_image_bytes = max));
        let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
        let r = prepare(&c, &sel, Destination::Yamareco, &ws, None);
        if ok {
            r.unwrap();
        } else {
            assert_eq!(code(r), ErrorCode::FetchFailed, "{max}");
        }
    }
}

#[test]
fn a_lying_content_length_does_not_get_past_the_size_limit() {
    // Content-Length は小さく申告して、本文は大きい（読んだバイト数で数える）
    let tmp = TempDir::new("lying");
    let big = vec![0xAB; 5000];
    let site = FakeSite::start(vec![route(
        1,
        Reply::Raw {
            head: "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into(),
            body: common::Body::Bytes(big),
        },
    )]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let c = ImageClient::with_limits(&site.base, limits(|l| l.max_image_bytes = 1000));
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    assert_eq!(
        code(prepare(&c, &sel, Destination::Yamareco, &ws, None)),
        ErrorCode::FetchFailed
    );
}

#[test]
fn the_pixel_limit_is_checked_from_the_header_before_decoding() {
    let good = jpeg_bytes(64, 48, 1); // 3,072 画素
    for (max, ok) in [(3072, true), (3071, false), (100, false)] {
        let tmp = TempDir::new(&format!("maxpix-{max}"));
        let site = FakeSite::start(vec![jpeg_route(1, &good)]);
        let ws = Workspace::create(tmp.path(), "job1").unwrap();
        let c = ImageClient::with_limits(&site.base, limits(|l| l.max_pixels = max));
        let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
        let r = prepare(&c, &sel, Destination::Yamareco, &ws, None);
        if ok {
            r.unwrap();
        } else {
            assert_eq!(code(r), ErrorCode::FetchFailed, "{max}");
        }
    }
    // SOF だけが巨大（本体は 64×48 のまま）でも、デコードせずに画素数で断る。本番の上限は 6,000 万
    let sof = good
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("SOF");
    let patch = |w: u16, h: u16| {
        let mut b = good.clone();
        b[sof + 5..sof + 7].copy_from_slice(&h.to_be_bytes());
        b[sof + 7..sof + 9].copy_from_slice(&w.to_be_bytes());
        b
    };
    assert_eq!(
        jpeg::inspect(&patch(8001, 7500), 60_000_000).unwrap_err(),
        InspectError::TooManyPixels
    );
    assert_eq!(
        jpeg::inspect(&patch(65_535, 65_535), 60_000_000).unwrap_err(),
        InspectError::TooManyPixels
    );
    // ちょうど上限は通る（検査の段階）
    assert!(matches!(
        jpeg::inspect(&patch(6000, 10_000), 60_000_000),
        Ok(Inspection::Clean(_))
    ));
    // 寸法が 0 は壊れたファイル
    assert!(matches!(
        jpeg::inspect(&patch(0, 48), 60_000_000),
        Err(InspectError::Malformed(_))
    ));
    // メタデータのある写真は、画素数が大きくても外すだけ（デコードしない）
    let big_with_exif = with_segment(&patch(65_535, 65_535), 0xE1, b"Exif\0\0");
    assert_eq!(
        jpeg::inspect(&big_with_exif, 60_000_000).unwrap(),
        Inspection::HasMetadata
    );
    // 巨大な画素数の画像を prepare に渡しても、デコードに入らずに失敗する
    let tmp = TempDir::new("hugepix");
    let site = FakeSite::start(vec![jpeg_route(1, &patch(65_535, 65_535))]);
    let (_ws, r) = run(
        &site,
        &selection(period1(), vec![row(1, "2030-03-10 09:00")]),
        &tmp,
    );
    assert_eq!(code(r), ErrorCode::FetchFailed);
}

#[test]
fn the_total_size_limit_of_a_job_counts_fetched_bytes() {
    let imgs: Vec<Vec<u8>> = (1..=3)
        .map(|i| jpeg_bytes(60 + 4 * i as u16, 48, i as u8))
        .collect();
    let total: u64 = imgs.iter().map(|b| b.len() as u64).sum();
    for (max, ok) in [(total, true), (total - 1, false)] {
        let tmp = TempDir::new(&format!("jobtotal-{max}"));
        let site = FakeSite::start(
            (1..=3)
                .map(|i| jpeg_route(i, &imgs[i as usize - 1]))
                .collect(),
        );
        let ws = Workspace::create(tmp.path(), "job1").unwrap();
        let c = ImageClient::with_limits(&site.base, limits(|l| l.max_job_bytes = max));
        let rows = (1..=3)
            .map(|i| row(i, &format!("2030-03-10 0{i}:00")))
            .collect();
        let r = prepare(
            &c,
            &selection(period1(), rows),
            Destination::Yamareco,
            &ws,
            None,
        );
        if ok {
            assert_eq!(r.unwrap().photo_bytes, total);
        } else {
            assert_eq!(code(r), ErrorCode::FetchFailed);
        }
    }
}

#[test]
fn the_job_total_is_enforced_before_reading_not_after() {
    let good = jpeg_bytes(64, 48, 1);
    let len = good.len() as u64;
    let site = FakeSite::start(vec![jpeg_route(1, &good)]);
    let c = client(&site);
    let t = Duration::from_secs(10);
    // 残りがちょうど（1 枚の大きさ）なら読める。1 バイト足りなければ失敗、0 でも失敗
    assert_eq!(c.fetch("derivatives/p1.jpg", t, len).unwrap(), good);
    assert_eq!(c.fetch("derivatives/p1.jpg", t, len + 1).unwrap(), good);
    for remaining in [len - 1, 1, 0] {
        let e = c.fetch("derivatives/p1.jpg", t, remaining).unwrap_err();
        assert_eq!(e.code, ErrorCode::FetchFailed, "{remaining}");
        assert!(
            e.detail.contains("1 ジョブの上限"),
            "{remaining}: {}",
            e.detail
        );
    }
    // 1 枚の上限のほうが小さいときは、1 枚の上限の失敗として返す
    let c = ImageClient::with_limits(&site.base, limits(|l| l.max_image_bytes = len - 1));
    let e = c.fetch("derivatives/p1.jpg", t, u64::MAX).unwrap_err();
    assert!(e.detail.contains("1 枚の上限"), "{}", e.detail);
}

#[test]
fn the_fetch_stops_reading_just_past_the_remaining_job_bytes() {
    // 本文は 48MB。残りが 1,000 バイトなら、超えると分かったところで読むのをやめる（上限を大きく超えて読まない）。
    // 相手が読むのをやめると、偽のサーバーの書き込みはそこで止まる（ソケットのバッファの分だけ先に書ける）
    const BODY: u64 = 48 * 1024 * 1024;
    let site = FakeSite::start(vec![route(
        1,
        Reply::Raw {
            head: "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nConnection: close\r\n\r\n".into(),
            body: common::Body::Filler {
                len: BODY,
                byte: 0xAB,
            },
        },
    )]);
    let c = ImageClient::with_limits(&site.base, limits(|l| l.max_image_bytes = BODY));
    let e = c
        .fetch("derivatives/p1.jpg", Duration::from_secs(10), 1000)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::FetchFailed);
    assert!(e.detail.contains("1 ジョブの上限"), "{}", e.detail);
    std::thread::sleep(Duration::from_millis(300));
    let sent = site.body_bytes_sent();
    assert!(
        sent < BODY / 2,
        "読むのをやめていない（サーバーが {sent} バイト書けた）"
    );
}

#[test]
fn files_excluded_for_metadata_still_count_toward_the_job_total() {
    // 1 枚目は区画の検査で外れるが、読んだバイトは合計に数える（合計は「取った画像」に掛ける）
    let excluded_one = with_segment(&jpeg_bytes(64, 48, 1), 0xE1, b"Exif\0\0");
    let kept = jpeg_bytes(80, 60, 2);
    let total = (excluded_one.len() + kept.len()) as u64;
    for (max, ok) in [(total, true), (total - 1, false)] {
        let tmp = TempDir::new(&format!("excluded-count-{max}"));
        let site = FakeSite::start(vec![jpeg_route(1, &excluded_one), jpeg_route(2, &kept)]);
        let ws = Workspace::create(tmp.path(), "job1").unwrap();
        let c = ImageClient::with_limits(&site.base, limits(|l| l.max_job_bytes = max));
        let rows = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 10:00")];
        let r = prepare(
            &c,
            &selection(period1(), rows),
            Destination::Yamareco,
            &ws,
            None,
        );
        if ok {
            assert_eq!(r.unwrap().photo_bytes, kept.len() as u64);
        } else {
            let err = r.unwrap_err();
            assert_eq!(err.code(), ErrorCode::FetchFailed, "{max}");
            // 外した 1 枚目までは分かっている
            assert_eq!(reasons(&err), [(1, ExcludeReason::HasMetadata)]);
        }
    }
}

#[test]
fn production_limits_are_the_documented_ones() {
    let l = Limits::default();
    assert_eq!(l.max_image_bytes, 30 * 1024 * 1024);
    assert_eq!(l.max_pixels, 60_000_000);
    assert_eq!(l.image_timeout, Duration::from_secs(60));
    assert_eq!(l.max_job_bytes, 2 * 1024 * 1024 * 1024);
}

#[test]
fn a_silent_server_hits_the_image_timeout() {
    let tmp = TempDir::new("imgtimeout");
    let site = FakeSite::start(vec![route(1, Reply::Stall(Duration::from_secs(3)))]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let timeout = Duration::from_millis(400);
    let c = ImageClient::with_limits(&site.base, limits(|l| l.image_timeout = timeout));
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    let started = Instant::now();
    assert_eq!(
        code(prepare(&c, &sel, Destination::Yamareco, &ws, None)),
        ErrorCode::FetchFailed
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= timeout && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[test]
fn a_body_that_stalls_midway_hits_the_image_timeout() {
    let tmp = TempDir::new("imgstall");
    let good = jpeg_bytes(64, 48, 1);
    let site = FakeSite::start(vec![route(
        1,
        Reply::Partial {
            head: "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n".into(),
            prefix: good[..100].to_vec(),
            hold: Duration::from_secs(3),
        },
    )]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let timeout = Duration::from_millis(500);
    let c = ImageClient::with_limits(&site.base, limits(|l| l.image_timeout = timeout));
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    let started = Instant::now();
    assert_eq!(
        code(prepare(&c, &sel, Destination::Yamareco, &ws, None)),
        ErrorCode::FetchFailed
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= timeout && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[test]
fn the_job_deadline_stops_the_export_with_timeout() {
    let good = jpeg_bytes(64, 48, 1);
    // 期限が過ぎていれば、1 枚も取らない
    let tmp = TempDir::new("deadline-past");
    let site = FakeSite::start(vec![jpeg_route(1, &good)]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    let r = prepare(
        &client(&site),
        &sel,
        Destination::Yamareco,
        &ws,
        Some(Instant::now()),
    );
    assert_eq!(code(r), ErrorCode::Timeout);
    assert!(site.request_lines().is_empty());
    // 取得の途中で期限が来たら、1 枚の期限（60 秒）を待たずに timeout
    let tmp = TempDir::new("deadline-mid");
    let site = FakeSite::start(vec![route(1, Reply::Stall(Duration::from_secs(3)))]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_millis(400);
    let r = prepare(
        &client(&site),
        &sel,
        Destination::Yamareco,
        &ws,
        Some(deadline),
    );
    assert_eq!(code(r), ErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// 呼ぶたびに 1 時間ずつ進む時計（期限の確かめを、時間に頼らずに数える）。`n` 回目の呼び出しは `base + n 時間`。
fn ticking_clock(base: Instant) -> impl Fn() -> Instant {
    let calls = std::cell::Cell::new(0u32);
    move || {
        calls.set(calls.get() + 1);
        base + Duration::from_secs(3600 * u64::from(calls.get()))
    }
}

#[test]
fn the_deadline_is_checked_after_decoding_and_after_writing_the_manifest() {
    // 1 枚だけの書き出しで、期限の確かめは 4 回: 取得の前（1 回目）、デコードと計測の後（2 回目）、結果を決める前
    // （3 回目。全部除外のような早く終わる結果もここを通る）、manifest の後（4 回目）。
    // 期限を n 回目の時刻ちょうどにすると、n 回目で timeout になる
    let good = jpeg_bytes(64, 48, 1);
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    for (hours, written) in [
        (1u64, vec![]),
        (2, vec![]),
        (3, vec!["090000_1.jpg"]),
        (4, vec!["090000_1.jpg", "manifest.json"]),
    ] {
        let tmp = TempDir::new(&format!("deadline-clock-{hours}"));
        let site = FakeSite::start(vec![jpeg_route(1, &good)]);
        let ws = Workspace::create(tmp.path(), "job1").unwrap();
        let base = Instant::now();
        let r = prepare_with_clock(
            &client(&site),
            &sel,
            Destination::Yamareco,
            &ws,
            Some(base + Duration::from_secs(3600 * hours)),
            &ticking_clock(base),
        );
        let err = r.expect_err(&format!("{hours} 時間目に期限"));
        assert_eq!(err.code(), ErrorCode::Timeout, "{hours}");
        // 1 回目: 1 枚も取らない。2 回目: 取ったが書かない。3 回目: manifest を書かない。4 回目: 書いた後で、成功を返さない
        assert_eq!(
            site.request_lines().len(),
            usize::from(hours > 1),
            "{hours}"
        );
        assert_eq!(files_in(ws.path()), written, "{hours}");
    }
    // 期限が 4 回の確かめより後ならそのまま成功する
    let tmp = TempDir::new("deadline-clock-ok");
    let site = FakeSite::start(vec![jpeg_route(1, &good)]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let base = Instant::now();
    prepare_with_clock(
        &client(&site),
        &sel,
        Destination::Yamareco,
        &ws,
        Some(base + Duration::from_secs(3600 * 4 + 1)),
        &ticking_clock(base),
    )
    .unwrap();
}

#[test]
fn redirects_are_not_followed() {
    let good = jpeg_bytes(64, 48, 1);
    for status in [301, 302, 303, 307, 308] {
        let tmp = TempDir::new(&format!("redirect-{status}"));
        let target = FakeSite::start(vec![jpeg_route(1, &good)]);
        let location = format!("http://{}/derivatives/p1.jpg", target.addr);
        let site = FakeSite::start(vec![route(1, Reply::redirect(status, &location))]);
        let (_ws, r) = run(
            &site,
            &selection(period1(), vec![row(1, "2030-03-10 09:00")]),
            &tmp,
        );
        assert_eq!(code(r), ErrorCode::FetchFailed, "{status}");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(site.connections(), 1, "{status}");
        assert_eq!(
            target.connections(),
            0,
            "{status}: リダイレクトの行き先に接続した"
        );
    }
}

#[test]
fn a_gzip_response_is_decoded_and_the_limit_counts_decoded_bytes() {
    use std::io::Write;
    let gzip = |data: &[u8]| {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(data).unwrap();
        gz.finish().unwrap()
    };
    let serve = |body: Vec<u8>| {
        FakeSite::start(vec![route(
            1,
            Reply::Raw {
                head: format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ),
                body: common::Body::Bytes(body),
            },
        )])
    };
    // 展開したバイト列を書き、file_size もそのバイト列の大きさ
    let good = jpeg_bytes(64, 48, 1);
    let tmp = TempDir::new("gzip-ok");
    let (ws, r) = run(
        &serve(gzip(&good)),
        &selection(period1(), vec![row(1, "2030-03-10 09:00")]),
        &tmp,
    );
    r.unwrap();
    assert_eq!(std::fs::read(ws.path().join("090000_1.jpg")).unwrap(), good);
    assert_eq!(
        read_manifest(&ws)["photos"][0]["file_size"].as_u64(),
        Some(good.len() as u64)
    );
    // 小さく圧縮された大きな本文（展開爆弾）は、展開後の大きさで上限に掛かる
    let tmp = TempDir::new("gzip-bomb");
    let site = serve(gzip(&vec![0u8; 1_000_000]));
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let c = ImageClient::with_limits(&site.base, limits(|l| l.max_image_bytes = 10_000));
    let sel = selection(period1(), vec![row(1, "2030-03-10 09:00")]);
    assert_eq!(
        code(prepare(&c, &sel, Destination::Yamareco, &ws, None)),
        ErrorCode::FetchFailed
    );
}

#[test]
fn the_request_is_a_plain_get_of_the_derivative_path() {
    let tmp = TempDir::new("reqline");
    let site = FakeSite::start(vec![jpeg_route(7, &jpeg_bytes(64, 48, 1))]);
    let (_ws, r) = run(
        &site,
        &selection(period1(), vec![row(7, "2030-03-10 09:00")]),
        &tmp,
    );
    r.unwrap();
    assert_eq!(site.request_lines(), ["GET /derivatives/p7.jpg HTTP/1.1"]);
}

// ---- 作業領域 ----

#[test]
fn the_workspace_is_private_and_new_and_removed_when_dropped() {
    let tmp = TempDir::new("workspace");
    let path = {
        let ws = Workspace::create(tmp.path(), "job-1_a").unwrap();
        let mode = std::fs::metadata(ws.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        ws.write_new("091100_1.jpg", b"x").unwrap();
        let file_mode = std::fs::metadata(ws.path().join("091100_1.jpg"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        // 既にあるファイルは書き換えない
        assert_eq!(
            code(ws.write_new("091100_1.jpg", b"y")),
            ErrorCode::Internal
        );
        assert_eq!(std::fs::read(ws.path().join("091100_1.jpg")).unwrap(), b"x");
        // 同じ作業領域は 2 度作れない
        assert_eq!(
            code(Workspace::create(tmp.path(), "job-1_a")),
            ErrorCode::Internal
        );
        // 決まった形のパスだけ
        for bad in [
            "../escape.jpg",
            "a.jpg",
            "/abs/091100_1.jpg",
            "2030-03-10/../x.jpg",
            "x/091100_1.jpg",
        ] {
            assert_eq!(code(ws.write_new(bad, b"z")), ErrorCode::Internal, "{bad}");
        }
        ws.path().to_path_buf()
    };
    assert!(!path.exists(), "手放したら消える");
    for bad in ["", "..", "a/b", "a b", &"x".repeat(65)] {
        assert_eq!(
            code(Workspace::create(tmp.path(), bad)),
            ErrorCode::Internal,
            "{bad:?}"
        );
    }
}

#[test]
fn links_are_never_followed_when_writing() {
    let tmp = TempDir::new("links");
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, b"keep").unwrap();
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    // 書き先がリンク（O_EXCL|O_NOFOLLOW）
    symlink(&victim, ws.path().join("091100_1.jpg")).unwrap();
    assert_eq!(
        code(ws.write_new("091100_1.jpg", b"overwritten")),
        ErrorCode::Internal
    );
    // 日付のフォルダがリンク
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, ws.path().join("2030-03-10")).unwrap();
    assert_eq!(
        code(ws.write_new("2030-03-10/091100_1.jpg", b"x")),
        ErrorCode::Internal
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    // 作業領域を消しても、リンクの先は消えない
    drop(ws);
    assert!(victim.exists() && elsewhere.exists());
}

#[test]
fn an_early_result_is_not_returned_after_the_deadline() {
    // 全部が not_derivative（画像を 1 枚も取らない早く終わる結果）でも、期限を過ぎていれば nothing_to_export ではなく timeout
    let tmp = TempDir::new("deadline-early");
    let mut rows = vec![row(1, "2030-03-10 09:00")];
    rows[0]["file"] = json!("local/x.jpg");
    let site = FakeSite::start(vec![]);
    let ws = Workspace::create(tmp.path(), "job1").unwrap();
    let base = Instant::now();
    let r = prepare_with_clock(
        &client(&site),
        &selection(period1(), rows),
        Destination::Yamareco,
        &ws,
        Some(base + Duration::from_secs(3600)),
        &ticking_clock(base),
    );
    assert_eq!(r.unwrap_err().code(), ErrorCode::Timeout);
    assert!(site.request_lines().is_empty());
}

#[test]
fn a_gzip_body_larger_on_the_wire_fits_the_remaining_job_bytes() {
    // 圧縮すると少し大きくなる本文でも、展開後の大きさが残り以内なら読める（ureq の上限は展開前に効くので、残りに縛らない）
    use std::io::Write;
    let good = jpeg_bytes(64, 48, 7);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
    gz.write_all(&good).unwrap();
    let body = gz.finish().unwrap();
    assert!(body.len() > good.len());
    let site = FakeSite::start(vec![route(
        1,
        Reply::Raw {
            head: format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            body: common::Body::Bytes(body),
        },
    )]);
    let got = client(&site)
        .fetch(
            "derivatives/p1.jpg",
            Duration::from_secs(10),
            good.len() as u64,
        )
        .unwrap();
    assert_eq!(got, good);
}
