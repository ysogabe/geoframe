//! 契約テスト（photo_feed_*.rs）の共通部品。合成データだけを使う。

// テストファイルごとに使う部品が違うので、使われない関数があっても警告しない
#![allow(dead_code)]

use std::path::PathBuf;

use geoframe::feed::{Photo, QualityKind};
use serde_json::Value;

/// 契約のフィールド（docs/architecture/photo-feed-contract.md の表）。
pub const CONTRACT_FIELDS: [&str; 13] = [
    "id",
    "file",
    "time",
    "lat",
    "lon",
    "pref",
    "city",
    "title",
    "fav",
    "fetched_at",
    "quality_kind",
    "quality_recommended",
    "trip",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: PathBuf) -> Value {
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// docs/architecture/photo-feed.schema.json
pub fn schema_json() -> Value {
    read_json(repo_root().join("docs/architecture/photo-feed.schema.json"))
}

/// 現行 Python の `api_photos()` が合成DBに対して出した JSON（再生成は
/// `python3 tools/gen_photo_feed_fixture.py`）。
pub fn python_fixture() -> Value {
    read_json(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/photo-feed-python.json"),
    )
}

pub fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema_json()).expect("Schema が不正")
}

/// Schema の違反の説明（空なら適合）。
pub fn violations(validator: &jsonschema::Validator, instance: &Value) -> Vec<String> {
    validator
        .iter_errors(instance)
        .map(|e| format!("{} ({})", e, e.instance_path()))
        .collect()
}

/// 内部値を全部埋めた合成の写真と、グループに入らない合成の写真。
pub fn sample_photos() -> Vec<Photo> {
    let plain = Photo {
        id: 1,
        fetched_at: String::new(),
        fav: false,
        file: "derivatives/s01.jpg".into(),
        title: "IMG_0001".into(),
        lat: 35.0,
        lon: 139.0,
        time: "2025-06-01 10:00".into(),
        place: String::new(),
        pref: String::new(),
        city: String::new(),
        elev: None,
        inferred: false,
        quality_score: None,
        quality_group: None,
        quality_kind: QualityKind::None,
        quality_rank: None,
        quality_recommended: false,
        quality_role: String::new(),
        ai_group: String::new(),
        ai_status: String::new(),
        ai_same_scene: false,
        ai_rank: None,
        ai_recommended: false,
        ai_local_score: None,
        trip: 0,
    };
    let grouped = Photo {
        id: 2,
        fetched_at: "2025-06-02T09:30:00".into(),
        fav: true,
        file: "derivatives/s02.jpg".into(),
        title: "見本市 サンプル町".into(),
        lat: 35.2,
        lon: 139.2,
        time: "2025-06-07 11:00".into(),
        place: "サンプル町".into(),
        pref: "架空県".into(),
        city: "見本市".into(),
        elev: Some(12.5),
        inferred: true,
        quality_score: Some(0.9),
        quality_group: Some(1),
        quality_kind: QualityKind::Similar,
        quality_rank: Some(1),
        quality_recommended: true,
        quality_role: "recommended".into(),
        ai_group: "ai-1".into(),
        ai_status: "judged".into(),
        ai_same_scene: true,
        ai_rank: Some(1),
        ai_recommended: true,
        ai_local_score: Some(0.8),
        trip: 1,
    };
    let exact_unknown_time = Photo {
        id: 3,
        file: "derivatives/s03.jpg".into(),
        time: String::new(),
        quality_kind: QualityKind::Exact,
        quality_role: "alternate".into(),
        quality_group: Some(2),
        quality_rank: Some(2),
        quality_score: Some(0.7),
        trip: 2,
        ..plain.clone()
    };
    vec![plain, grouped, exact_unknown_time]
}
