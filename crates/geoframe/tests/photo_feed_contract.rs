//! `GET /api/v1/photos` の応答の形（契約）の検査。
//!
//! - Rust の型（`geoframe::feed::Photo`）と、現行 Python が出す JSON の両方を、同じ JSON Schema
//!   （docs/architecture/photo-feed.schema.json）で検査する。
//! - Python 出力のフィクスチャを型で読み書きし直して、キー・値・null が変わらないことを確かめる
//!   （T1.5 の全件比較の下ごしらえ）。
//!
//! Python 側の検査は app/tests/test_photo_feed_contract.py。契約を変えるときは両方と文書を直す。
//! miharashi の読み方は photo_feed_miharashi.rs。

mod common;

use common::{CONTRACT_FIELDS, python_fixture, sample_photos, schema_json, validator, violations};
use geoframe::feed::{Photo, QualityKind, is_fetched_at, is_time};
use serde_json::{Value, json};

fn as_array(v: &Value) -> &Vec<Value> {
    v.as_array().expect("配列")
}

#[test]
fn schema_requires_exactly_the_contract_fields() {
    let schema = schema_json();
    let mut required: Vec<&str> = schema["$defs"]["photo"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort_unstable();
    let mut expected = CONTRACT_FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(required, expected);
    // 契約外のフィールドは許す（品質解析やAIの内部値）
    assert_eq!(
        schema["$defs"]["photo"]["additionalProperties"],
        Value::Bool(true)
    );
}

#[test]
fn python_output_satisfies_the_schema() {
    let fixture = python_fixture();
    assert!(!as_array(&fixture).is_empty());
    assert_eq!(violations(&validator(), &fixture), Vec::<String>::new());
}

#[test]
fn serialized_types_satisfy_the_schema() {
    let json = serde_json::to_value(sample_photos()).unwrap();
    assert_eq!(violations(&validator(), &json), Vec::<String>::new());
}

#[test]
fn python_output_roundtrips_through_the_type_unchanged() {
    let fixture = python_fixture();
    let photos: Vec<Photo> = serde_json::from_value(fixture.clone()).expect("型で読める");
    assert_eq!(photos.len(), as_array(&fixture).len());
    let again = serde_json::to_value(&photos).unwrap();
    // キーの集合・値・null の出し方・整数と小数の区別まで同じ（Value の比較）
    assert_eq!(again, fixture);
    // 文字列に直しても読み直せる（浮動小数の往復を含む）
    let text = serde_json::to_string(&photos).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), fixture);
}

#[test]
fn fixture_exercises_null_and_value_variants() {
    // フィクスチャが形のバリエーションを含んでいること（往復テストが形骸化しないように）
    let photos: Vec<Photo> = serde_json::from_value(python_fixture()).unwrap();
    assert!(photos.iter().any(|p| p.elev.is_none()));
    assert!(photos.iter().any(|p| p.elev.is_some()));
    assert!(photos.iter().any(|p| p.quality_kind == QualityKind::None));
    assert!(
        photos
            .iter()
            .any(|p| p.quality_kind == QualityKind::Similar)
    );
    assert!(photos.iter().any(|p| p.quality_kind == QualityKind::Exact));
    assert!(photos.iter().any(|p| p.quality_group.is_some()));
    assert!(
        photos
            .iter()
            .any(|p| !p.ai_group.is_empty() && p.ai_local_score.is_none())
    );
    assert!(photos.iter().any(|p| p.time.is_empty()));
    assert!(photos.iter().any(|p| p.fetched_at.is_empty()));
    assert!(photos.iter().any(|p| p.lat.fract() == 0.0), "整数値の小数");
    // 並びは撮影日時の昇順で、時刻不明が最後
    let times: Vec<&str> = photos.iter().map(|p| p.time.as_str()).collect();
    assert_eq!(times.last(), Some(&""));
}

#[test]
fn optional_internal_values_serialize_as_null_not_omitted() {
    let json = serde_json::to_value(&sample_photos()[0]).unwrap();
    for key in [
        "elev",
        "quality_score",
        "quality_group",
        "quality_rank",
        "ai_rank",
        "ai_local_score",
    ] {
        assert_eq!(json.get(key), Some(&Value::Null), "{key}");
    }
    // Python は REAL を小数で出す。整数値でも `35.0`
    let text = serde_json::to_string(&sample_photos()[0]).unwrap();
    assert!(text.contains(r#""lat":35.0,"lon":139.0"#), "{text}");
}

#[test]
fn missing_contract_field_fails_deserialization_and_schema() {
    let validator = validator();
    let fixture = python_fixture();
    for field in CONTRACT_FIELDS {
        let mut item = as_array(&fixture)[1].clone();
        item.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Photo>(item.clone()).is_err(),
            "{field} が無いのに読めた"
        );
        assert!(
            !violations(&validator, &json!([item])).is_empty(),
            "{field} が無いのに Schema を満たした"
        );
    }
}

#[test]
fn missing_internal_field_fails_unless_it_is_nullable() {
    // 内部値は契約外だが、Python と同じ形で出す約束なので、欠けた入力は読み込みで気づく。
    // ただし `Option` の内部値は serde の既定どおり、欠けたら `None`（null と同じ）になる。
    // Schema は内部値を要求しない（additionalProperties で許すだけ）
    let nullable = [
        "elev",
        "quality_score",
        "quality_group",
        "quality_rank",
        "ai_rank",
        "ai_local_score",
    ];
    let validator = validator();
    let fixture = python_fixture();
    for field in as_array(&fixture)[1].as_object().unwrap().keys() {
        if CONTRACT_FIELDS.contains(&field.as_str()) {
            continue;
        }
        let mut item = as_array(&fixture)[1].clone();
        item.as_object_mut().unwrap().remove(field);
        let read = serde_json::from_value::<Photo>(item.clone());
        assert_eq!(read.is_ok(), nullable.contains(&field.as_str()), "{field}");
        assert_eq!(
            violations(&validator, &json!([item])),
            Vec::<String>::new(),
            "{field}"
        );
    }
}

#[test]
fn unknown_extra_fields_are_allowed() {
    let mut item = as_array(&python_fixture())[0].clone();
    item["future_field"] = json!({"x": 1});
    assert!(serde_json::from_value::<Photo>(item.clone()).is_ok());
    assert_eq!(
        violations(&validator(), &json!([item])),
        Vec::<String>::new()
    );
}

#[test]
fn schema_and_type_reject_wrong_json_types_and_formats() {
    let validator = validator();
    let fixture = python_fixture();
    let base = as_array(&fixture)[1].clone();
    let broken: Vec<(&str, Value)> = vec![
        ("id", json!("2")),
        ("id", json!(2.5)),
        ("id", json!(true)),
        ("file", json!("/derivatives/p02.jpg")),
        ("file", json!(null)),
        ("time", json!("2025-06-07")),
        ("time", json!("2025-06-07T11:00")),
        ("time", json!(null)),
        ("lat", json!("35.0")),
        ("lat", json!(91.0)),
        ("lon", json!(-180.5)),
        ("pref", json!(null)),
        ("city", json!(1)),
        ("title", json!("")),
        ("fav", json!(1)),
        ("fav", json!("true")),
        ("fetched_at", json!("2025-06-08 09:30:00")),
        ("fetched_at", json!(null)),
        ("quality_kind", json!("other")),
        ("quality_kind", json!(null)),
        ("quality_recommended", json!(0)),
        ("trip", json!(-1)),
        ("trip", json!(0.5)),
        ("trip", json!("0")),
    ];
    for (field, value) in broken {
        let mut item = base.clone();
        item[field] = value.clone();
        assert!(
            !violations(&validator, &json!([item.clone()])).is_empty(),
            "{field}={value} が Schema を通った"
        );
        // 型で読めるかは、Schema の書式（正規表現・範囲）まで見るものと見ないものがある。
        // JSON 型の違いは型の読み込みでも失敗する
        let json_type_wrong = matches!(
            field,
            "id" | "fav" | "quality_recommended" | "pref" | "city"
        ) || matches!(
            (field, &value),
            ("lat", Value::String(_)) | ("trip", Value::String(_))
        ) || matches!(field, "quality_kind" | "time" | "fetched_at" | "file" if value.is_null());
        if json_type_wrong {
            assert!(
                serde_json::from_value::<Photo>(item).is_err(),
                "{field}={value} が型で読めた"
            );
        }
    }
    // 配列でない応答
    assert!(!violations(&validator, &json!({"photos": []})).is_empty());
}

#[test]
fn format_helpers_agree_with_the_schema_patterns() {
    let validator = validator();
    let base = as_array(&python_fixture())[1].clone();
    let times = [
        "",
        "2025-06-07 11:00",
        "2025-12-31 23:59",
        "2025-06-07",
        "2025-06-07T11:00",
        "2025-06-07 11:00:00",
        "2025-13-07 11:00",
        "2025-00-07 11:00",
        "2025-06-32 11:00",
        "2025-06-07 24:00",
        "2025-06-07 11:60",
        "2025-06-07 11:6",
        " 2025-06-07 11:00",
        "2025-06-07 11:00\n",
    ];
    for s in times {
        let mut item = base.clone();
        item["time"] = json!(s);
        let ok = violations(&validator, &json!([item])).is_empty();
        assert_eq!(is_time(s), ok, "time {s:?}");
    }
    let fetched = [
        "",
        "2025-06-08T09:30:00",
        "2025-06-08 09:30:00",
        "2025-06-08T09:30",
        "2025-06-08T09:30:60",
        "2025-06-08T24:00:00",
        "2025-06-08T09:30:00Z",
        "2025-06-08T09:30:00.123",
        "2025-06-08T09:30:00\n",
    ];
    for s in fetched {
        let mut item = base.clone();
        item["fetched_at"] = json!(s);
        let ok = violations(&validator, &json!([item])).is_empty();
        assert_eq!(is_fetched_at(s), ok, "fetched_at {s:?}");
    }
}

#[test]
fn fixture_values_use_the_documented_formats() {
    for p in serde_json::from_value::<Vec<Photo>>(python_fixture()).unwrap() {
        assert!(is_time(&p.time), "{}", p.time);
        assert!(is_fetched_at(&p.fetched_at), "{}", p.fetched_at);
        assert!(!p.title.is_empty());
        assert!(p.trip >= 0);
    }
}
