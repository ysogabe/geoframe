//! `manifest.json` の組み立て（design.md 2.8 の旧 manifest の形、6.3）。
//!
//! 旧 manifest のキーをすべて同じ順で出し、6.3 のキーを足す。同じ入力からは同じバイト列になる（時刻やジョブ ID を
//! 入れない。AC11）。`lat`・`lon`・`elev` は一覧 API の値をそのまま出し（丸めない）、無いときはキーを省かずに `null` を書く。

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::date::{Date, Minute, Period};
use crate::folder::FolderName;
use crate::listing::Exclusion;
use crate::request::Destination;

/// `generator`。`geoframe-export <版>`（公開済みのフォルダが、このサービスが書いたものかを確かめるのにも使う）。
pub const GENERATOR: &str = concat!("geoframe-export ", env!("CARGO_PKG_VERSION"));
/// `source`（旧 manifest の `GEOFRAME /api/v1/photos` から変わる。対象がフィードではなくなったため）。
pub const SOURCE: &str = "GEOFRAME /api/admin/export/photos";
/// `scope`（非表示を除く全写真）。
pub const SCOPE: &str = "all_visible";

/// 書き出した写真 1 枚（manifest の `photos` の 1 行）。
#[derive(Debug, Clone, PartialEq)]
pub struct PhotoEntry {
    pub id: i64,
    /// 日付のサブフォルダを含む相対パス（単日は `HHMM00_<id>.jpg`、期間は `YYYY-MM-DD/HHMM00_<id>.jpg`）。
    pub filename: String,
    pub title: String,
    pub time: Minute,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub width: u32,
    pub height: u32,
    /// 配信されたバイト数（そのまま書いたので、作業領域のファイルの大きさと同じ）。
    pub file_size: u64,
    pub mean_brightness: f64,
    pub sharpness_laplacian_var: f64,
    pub inferred: bool,
    pub elev: Option<f64>,
}

/// `HHMM00_<id>.jpg`（カタログの撮影時刻は分までなので、秒は常に `00`）。
pub fn file_name(time: Minute, id: i64) -> String {
    format!("{:02}{:02}00_{id}.jpg", time.hour, time.minute)
}

/// 作業領域・Drive での相対パス。期間のときは日付のサブフォルダに置く。
pub fn relative_path(period: &Period, time: Minute, id: i64) -> String {
    if period.is_single_day() {
        file_name(time, id)
    } else {
        format!("{}/{}", time.date, file_name(time, id))
    }
}

/// manifest に必要な、写真以外の入力。
#[derive(Debug, Clone, Copy)]
pub struct ManifestInput<'a> {
    pub destination: Destination,
    pub folder: &'a FolderName,
    pub period: &'a Period,
    /// 期間内の非表示でない写真の数（候補の数）。
    pub catalog_count: usize,
    /// 仕様上の除外（`not_derivative`・`has_metadata`）。
    pub excluded: &'a [Exclusion],
    pub undated_in_catalog: u64,
    pub invalid_time_in_catalog: u64,
    /// 書き出した写真（`time`・`id` の昇順）。
    pub photos: &'a [PhotoEntry],
}

fn notes(period: &Period) -> Value {
    let mut n = Map::new();
    let mut put = |k: &str, v: &str| {
        n.insert(k.to_owned(), Value::String(v.to_owned()));
    };
    put(
        "scope",
        "対象は期間内に撮った非表示でない全写真。重複・類似で非推奨の写真と、位置の無い写真も含む。表示用 JPEG の形でないもの(EXIF 付きの原本など)と、JPEG にメタデータの区画があるものは外し、excluded に理由を書いた",
    );
    put(
        "taken_at",
        if period.is_single_day() {
            "カタログは分までしか持たないためファイル名の秒は 00。ローカル時刻のままで補正しない"
        } else {
            "カタログは分までしか持たないためファイル名の秒は 00。ローカル時刻のままで補正しない。ファイルは日付ごとのサブフォルダに入れた"
        },
    );
    put(
        "mean_brightness",
        "グレースケール(縮小前) 0-255 の平均。Pillow の L 変換と同じ式。旧 manifest の値と JPEG のデコーダの違いで小数の下の桁がずれうる",
    );
    put(
        "sharpness_laplacian_var",
        "グレースケールにしてから最大1024px に縮小(Pillow の thumbnail 相当、BICUBIC)し、3x3(4近傍) Laplacian 応答(オフセット128、0-255 に丸め)の分散。旧 manifest の値とわずかにずれうる",
    );
    put(
        "spot",
        "名所の一覧を用意していないため全件空欄(nearest_spot_distance_m は null)",
    );
    put(
        "gps_inferred",
        "カタログの inferred フラグ。true は写真自体に GPS が無く推定された位置。lat/lon が null の写真は位置なし(gps_inferred は false)",
    );
    put("elev", "カタログの値。null は標高なし");
    if !period.is_single_day() {
        put(
            "days",
            "日付ごとの枚数・撮影時刻の範囲・id 一覧。写真本体の一覧は photos(filename に日付フォルダを含む)",
        );
    }
    Value::Object(n)
}

fn photo(p: &PhotoEntry) -> Value {
    // キーの順は旧 manifest と同じ（serde_json の preserve_order）
    json!({
        "id": p.id,
        "filename": p.filename,
        "title": p.title,
        "taken_at": p.time.to_string(),
        "lat": p.lat,
        "lon": p.lon,
        "width": p.width,
        "height": p.height,
        "file_size": p.file_size,
        "mean_brightness": p.mean_brightness,
        "sharpness_laplacian_var": p.sharpness_laplacian_var,
        "spot": "",
        "nearest_spot_distance_m": null,
        "gps_inferred": p.inferred,
        "elev": p.elev,
    })
}

/// 日ごとの `count`・`first`・`last`・`ids`（写真のある日だけ、日付の昇順）。
fn days(photos: &[PhotoEntry]) -> Value {
    let mut by_day: BTreeMap<Date, Vec<&PhotoEntry>> = BTreeMap::new();
    for p in photos {
        by_day.entry(p.time.date).or_default().push(p);
    }
    let mut out = Map::new();
    for (date, list) in by_day {
        let first = list.first().map(|p| p.time.to_string());
        let last = list.last().map(|p| p.time.to_string());
        out.insert(
            date.to_string(),
            json!({
                "count": list.len(),
                "first": first,
                "last": last,
                "ids": list.iter().map(|p| p.id).collect::<Vec<_>>(),
            }),
        );
    }
    Value::Object(out)
}

/// manifest の JSON。
pub fn build(input: &ManifestInput<'_>) -> Value {
    let period = input.period;
    let date = if period.is_single_day() {
        period.from().to_string()
    } else {
        format!("{}/{}", period.from(), period.to())
    };
    let excluded: Vec<Value> = input
        .excluded
        .iter()
        .map(|e| json!({"id": e.id, "reason": e.reason.as_str()}))
        .collect();
    let mut m = Map::new();
    // 旧 manifest の順（date, source, count, catalog_count, notes, days, photos）に、6.3 のキーを差し込む
    m.insert("generator".into(), json!(GENERATOR));
    m.insert("date".into(), json!(date));
    m.insert("destination".into(), json!(input.destination.as_str()));
    m.insert("folder".into(), json!(input.folder.as_str()));
    m.insert("scope".into(), json!(SCOPE));
    m.insert("source".into(), json!(SOURCE));
    m.insert("count".into(), json!(input.photos.len()));
    m.insert("catalog_count".into(), json!(input.catalog_count));
    m.insert("excluded".into(), Value::Array(excluded));
    m.insert("undated_in_catalog".into(), json!(input.undated_in_catalog));
    m.insert(
        "invalid_time_in_catalog".into(),
        json!(input.invalid_time_in_catalog),
    );
    m.insert("notes".into(), notes(period));
    if !period.is_single_day() {
        m.insert("days".into(), days(input.photos));
    }
    m.insert(
        "photos".into(),
        Value::Array(input.photos.iter().map(photo).collect()),
    );
    Value::Object(m)
}

/// 書き出すバイト列（整形した JSON と末尾の改行）。同じ値からは同じバイト列になる。
pub fn to_bytes(manifest: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}
