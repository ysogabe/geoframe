//! 表示クライアント miharashi が `GET /api/v1/photos` をどう読むかの再現と、互換の検査。
//!
//! miharashi の crate には依存しない（別々にビルド・リリースするため）。読み方は
//! miharashi の `crates/miharashi-app/src/photos.rs`（`Catalog::list_geoframe`）と
//! `album.rs`（`eligible`・`parse_time`）を写したもの。miharashi 側の読み方が変わったら、
//! ここと docs/architecture/photo-feed-contract.md を直す。
//! 写した時点の miharashi のコミットは 1bc73a6。
//!
//! 写し元の要点:
//! - 応答は配列。配列でなければ失敗。
//! - `file` が文字列でない要素は黙って飛ばす。ほかのフィールドは欠けても失敗しない。
//! - 文字列は `as_str().unwrap_or("")`、`lat`/`lon` は `as_f64()`、`trip` は `as_i64()`。
//! - `fav` と `quality_recommended` は `== true`（`1` や `"true"` は false 扱い）。
//! - `id` は数値・文字列ならそれを文字列にし、無ければ `file` で代用する。
//! - `time`・`fetched_at` は先頭16文字を chrono で `YYYY-MM-DD HH:MM` か `YYYY-MM-DDTHH:MM` と
//!   読み、読めなければ先頭10文字の日付だけ（12:00 扱い）で読む。暦（月の日数・閏年）も見る。

mod common;

use chrono::{NaiveDate, NaiveDateTime};
use common::{
    CONTRACT_FIELDS, TempDir, build_catalog, build_empty_catalog, photos_from, python_fixture,
    sample_photos,
};
use geoframe::feed::{Photo, is_time};
use serde_json::{Value, json};

/// miharashi の `Meta` と `Entry` のうち、GEOFRAME の応答から読む部分。
#[derive(Debug, Clone, PartialEq)]
struct Read {
    file: String,
    /// 表示の場所名: `"{pref} {title}"` の前後を除いたもの
    place: String,
    taken: String,
    id: String,
    lat: Option<f64>,
    lon: Option<f64>,
    pref: String,
    city: String,
    fav: bool,
    fetched_at: String,
    trip: Option<i64>,
}

/// miharashi がフィールドごとに期待する JSON 型（欠けたとき使う既定値は `fallback`）。
/// ここに無いフィールドは読まない。
const READS: [(&str, &str, &str); 12] = [
    ("file", "string", "要素ごと飛ばす"),
    ("id", "integer|string", "file で代用"),
    ("time", "string", "空文字"),
    ("lat", "number", "None"),
    ("lon", "number", "None"),
    ("pref", "string", "空文字"),
    ("city", "string", "空文字"),
    ("title", "string", "空文字"),
    ("fav", "boolean", "false"),
    ("fetched_at", "string", "空文字"),
    ("quality_kind", "string", "空文字"),
    ("quality_recommended", "boolean", "false"),
];

fn text(p: &Value, k: &str) -> String {
    p[k].as_str().unwrap_or("").to_string()
}

/// `album::eligible`
fn eligible(quality_kind: &str, quality_recommended: bool, fav: bool) -> bool {
    quality_kind != "similar" || quality_recommended || fav
}

/// `Catalog::list_geoframe` の本体（HTTP を除く）。`Err` は「配列でない」。
fn read_feed(list: &Value) -> Result<Vec<Read>, String> {
    let arr = list.as_array().ok_or("unexpected /api/v1/photos")?;
    let mut out = Vec::new();
    for p in arr {
        let Some(file) = p["file"].as_str() else {
            continue;
        };
        let fav = p["fav"] == true;
        if !eligible(
            p["quality_kind"].as_str().unwrap_or(""),
            p["quality_recommended"] == true,
            fav,
        ) {
            continue;
        }
        let place = format!("{} {}", text(p, "pref"), text(p, "title"));
        let id = match &p["id"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => file.to_string(),
        };
        out.push(Read {
            file: file.to_string(),
            place: place.trim().to_string(),
            taken: text(p, "time"),
            id,
            lat: p["lat"].as_f64(),
            lon: p["lon"].as_f64(),
            pref: text(p, "pref"),
            city: text(p, "city"),
            fav,
            fetched_at: text(p, "fetched_at"),
            trip: p["trip"].as_i64(),
        });
    }
    Ok(out)
}

/// `album::parse_time`。chrono で解析するので、月ごとの日数・閏年まで見る
/// （`2025-02-31 10:00` は読めず、日付だけにも落ちない）。
fn parse_time(s: &str) -> Option<NaiveDateTime> {
    let s = s.get(..16).unwrap_or(s);
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(12, 0, 0)
        })
}

/// 期待値の組み立て（`parse_time` の結果との比較用）。
fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(h, mi, 0)
}

/// フィクスチャのうち miharashi が一覧に出す写真の ID（`album::eligible` の結果）。
const FIXTURE_SHOWN: [&str; 7] = ["1", "2", "4", "6", "8", "9", "10"];

#[test]
fn reads_the_python_fixture() {
    check_fixture_reads(&python_fixture());
}

/// エンドポイントの実出力（HTTP 越し）を miharashi の読み方で読む。Python のフィクスチャと同じ
/// 結果になる。
#[tokio::test]
async fn reads_the_live_endpoint_like_the_python_fixture() {
    let dir = TempDir::new("miharashi-live");
    let (path, _writer) = build_catalog(dir.path(), "");
    let response = photos_from(&path).await;
    assert_eq!(response.status, 200);
    let live = response.json();
    check_fixture_reads(&live);
    assert_eq!(
        read_feed(&live).unwrap(),
        read_feed(&python_fixture()).unwrap()
    );
}

/// `taken_at` が NULL の行は、契約どおり空文字（時刻不明）で最後に出るので、miharashi は
/// 時刻なしの写真として読める（Python の `time: null` は文字列でなく、`unwrap_or("")` で
/// 空になるだけだった）。
#[tokio::test]
async fn reads_a_null_taken_at_as_an_unknown_time() {
    let dir = TempDir::new("miharashi-null");
    let sql = "INSERT INTO photos(id,source,display,name,taken_at,lat,lon)
               VALUES(1,'local','derivatives/a.jpg','IMG_0001.jpg','2025-06-01 10:00',35.0,139.0);
               INSERT INTO photos(id,source,display,name,taken_at,lat,lon)
               VALUES(2,'local','derivatives/b.jpg','IMG_0002.jpg',NULL,35.1,139.1);";
    let (path, _writer) = build_empty_catalog(dir.path(), sql);
    let live = photos_from(&path).await.json();
    let reads = read_feed(&live).unwrap();
    let ids: Vec<&str> = reads.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["1", "2"]);
    assert_eq!(reads[0].trip, Some(0));
    assert_eq!(reads[1].trip, Some(1));
    assert_eq!(reads[1].taken, "");
    assert_eq!(parse_time(&reads[1].taken), None);
    assert_eq!(live[1]["time"], "");
}

fn check_fixture_reads(list: &Value) {
    let reads = read_feed(list).unwrap();
    let ids: Vec<&str> = reads.iter().map(|r| r.id.as_str()).collect();
    // id=3 は similar の推奨外でお気に入りでもないので出さない
    assert_eq!(ids, FIXTURE_SHOWN);
    for r in &reads {
        assert!(r.lat.is_some() && r.lon.is_some(), "{r:?}");
        assert!(r.trip.is_some(), "{r:?}");
        assert!(r.file.starts_with("derivatives/"), "{r:?}");
        assert!(!r.place.is_empty(), "{r:?}");
        assert!(
            r.taken.is_empty() || parse_time(&r.taken).is_some(),
            "{r:?}"
        );
        assert!(
            r.fetched_at.is_empty() || parse_time(&r.fetched_at).is_some(),
            "{r:?}"
        );
    }
    // 値の読み取りを1件で確かめる（id=2: お気に入り・住所あり・取り込み日時あり）
    let r = reads.iter().find(|r| r.id == "2").unwrap();
    assert_eq!(r.place, "架空県 見本市 サンプル町");
    assert_eq!((r.pref.as_str(), r.city.as_str()), ("架空県", "見本市"));
    assert!(r.fav);
    assert_eq!(r.trip, Some(0));
    assert_eq!(parse_time(&r.fetched_at), at(2025, 6, 2, 9, 30));
    assert_eq!(parse_time(&r.taken), at(2025, 6, 1, 12, 0));
    // 住所なし（pref 空）の場所名はタイトルだけ。時刻不明は読めないまま空
    let r = reads.iter().find(|r| r.id == "1").unwrap();
    assert_eq!(r.place, "IMG_0001");
    let r = reads.iter().find(|r| r.id == "10").unwrap();
    assert_eq!(r.taken, "");
    assert_eq!(parse_time(&r.taken), None);
}

#[test]
fn reads_the_serialized_types_the_same_way() {
    let photos = sample_photos();
    let json = serde_json::to_value(&photos).unwrap();
    let reads = read_feed(&json).unwrap();
    // 型の出力は miharashi が読む形になる。all のうち similar の推奨外は無いので全件
    assert_eq!(reads.len(), photos.len());
    for (r, p) in reads.iter().zip(&photos) {
        assert_eq!(r.id, p.id.to_string());
        assert_eq!(r.file, p.file);
        assert_eq!(r.lat, Some(p.lat));
        assert_eq!(r.lon, Some(p.lon));
        assert_eq!(r.fav, p.fav);
        assert_eq!(r.trip, Some(p.trip));
        assert_eq!(r.taken, p.time);
        assert_eq!(r.fetched_at, p.fetched_at);
        assert_eq!(
            (r.pref.as_str(), r.city.as_str()),
            (p.pref.as_str(), p.city.as_str())
        );
    }
}

#[test]
fn contract_fields_cover_what_miharashi_reads_and_nothing_else_is_needed() {
    // miharashi が読むのは契約のフィールドだけ（trip は READS の外で as_i64 で読む）
    for (name, _, _) in READS {
        assert!(CONTRACT_FIELDS.contains(&name), "{name} は契約に無い");
    }
    assert!(CONTRACT_FIELDS.contains(&"trip"));
    // 契約外の内部値を全部消しても、読み取り結果は変わらない
    let full = python_fixture();
    let mut stripped = full.clone();
    for item in stripped.as_array_mut().unwrap() {
        item.as_object_mut()
            .unwrap()
            .retain(|k, _| CONTRACT_FIELDS.contains(&k.as_str()));
    }
    assert_eq!(read_feed(&stripped).unwrap(), read_feed(&full).unwrap());
    // 型で読み書きし直しても同じ
    let photos: Vec<Photo> = serde_json::from_value(full.clone()).unwrap();
    let again = serde_json::to_value(photos).unwrap();
    assert_eq!(read_feed(&again).unwrap(), read_feed(&full).unwrap());
}

#[test]
fn json_types_miharashi_expects_match_the_schema() {
    let schema = common::schema_json();
    let props = &schema["$defs"]["photo"]["properties"];
    for (name, expected, _) in READS {
        let ty = props[name]["type"].as_str();
        let schema_ty = match (name, ty) {
            ("quality_kind", None) => "string", // enum（"" / exact / similar）
            (_, Some(t)) => t,
            (_, None) => panic!("{name} の type が Schema に無い"),
        };
        // id は miharashi が数値と文字列の両方を受けるが、契約は整数
        assert!(
            expected.split('|').any(|e| e == schema_ty),
            "{name}: miharashi は {expected}、Schema は {schema_ty}"
        );
    }
}

#[test]
fn miharashi_quirks_that_the_contract_must_keep_in_mind() {
    // 配列でなければ失敗
    assert!(read_feed(&json!({"photos": []})).is_err());
    // file が文字列でない要素は飛ばす。ほかの欠けは既定値になるだけ（miharashi 自身のモックは
    // id・fav・city・fetched_at・quality_* を省いている）
    let minimal = json!([
        {"file": "derivatives/a.png", "title": "T", "pref": "P", "time": "2026-01-01 00:00",
         "lat": 35.0, "lon": 139.0, "trip": 0},
        {"title": "ファイルなし"},
        {"file": 7}
    ]);
    let reads = read_feed(&minimal).unwrap();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].id, "derivatives/a.png"); // id が無ければ file
    assert!(!reads[0].fav);
    assert_eq!(reads[0].place, "P T");
    // 真偽値は本物の true だけが true。0/1 や文字列はお気に入りにならない
    for fav in [json!(1), json!("true"), json!(null)] {
        let item = json!([{"file": "derivatives/a.jpg", "fav": fav}]);
        assert!(!read_feed(&item).unwrap()[0].fav, "{fav}");
    }
    // quality_recommended が 1 だと similar は推奨外として落ちる
    let item =
        json!([{"file": "derivatives/a.jpg", "quality_kind": "similar", "quality_recommended": 1}]);
    assert!(read_feed(&item).unwrap().is_empty());
    // lat/lon は数値だけ。文字列だと None（契約は数値で必ずある）
    let item = json!([{"file": "derivatives/a.jpg", "lat": "35.0", "lon": 139}]);
    let r = &read_feed(&item).unwrap()[0];
    assert_eq!((r.lat, r.lon), (None, Some(139.0)));
    // id は文字列も受けるが、契約は整数
    let item = json!([{"file": "derivatives/a.jpg", "id": "abc"}]);
    assert_eq!(read_feed(&item).unwrap()[0].id, "abc");
    // 時刻は T 区切りも、日付だけも読む。契約の書式（空白区切り・秒なしと T 区切り・秒あり）は読める範囲
    assert_eq!(parse_time("2025-06-07 11:00"), at(2025, 6, 7, 11, 0));
    assert_eq!(parse_time("2025-06-08T09:30:00"), at(2025, 6, 8, 9, 30));
    assert_eq!(parse_time("2025-06-08"), at(2025, 6, 8, 12, 0));
    assert_eq!(parse_time("2025-06-07 25:00"), at(2025, 6, 7, 12, 0)); // 時刻が壊れても日付は読む
    assert_eq!(parse_time(""), None);
    assert_eq!(parse_time("unknown"), None);
}

#[test]
fn miharashi_checks_the_calendar_which_the_schema_pattern_does_not() {
    // 存在しない日付は読めない（日付だけにも落ちない）。平年の2月29日も同じ
    for ng in [
        "2025-02-31 10:00",
        "2025-02-30 10:00",
        "2025-02-29 10:00",
        "2025-04-31 10:00",
        "2025-06-31T10:00",
        "2025-02-31",
        "2100-02-29 10:00", // 100で割れ400で割れない年は平年
    ] {
        assert_eq!(parse_time(ng), None, "{ng:?}");
    }
    // 閏年の2月29日と各月の末日は読める
    assert_eq!(parse_time("2024-02-29 10:00"), at(2024, 2, 29, 10, 0));
    assert_eq!(parse_time("2000-02-29 10:00"), at(2000, 2, 29, 10, 0));
    assert_eq!(parse_time("2025-04-30 23:59"), at(2025, 4, 30, 23, 59));
    assert_eq!(parse_time("2025-12-31T00:00:00"), at(2025, 12, 31, 0, 0));
    // 契約の書式（Schema の pattern と is_time）は月1〜12・日1〜31までしか見ないので、
    // miharashi が読めない日付を通す。書く側（Python の取り込み）が実在の日付を出す前提
    for s in ["2025-02-31 10:00", "2025-02-29 10:00"] {
        assert!(is_time(s), "{s:?}");
    }
    let validator = common::validator();
    let mut item = python_fixture()[1].clone();
    item["time"] = json!("2025-02-31 10:00");
    assert!(common::violations(&validator, &json!([item])).is_empty());
}
