//! EXIF（撮影時刻・GPS）の読み取りを、Python の `util.extract_exif`・`norm_time` の結果と突き合わせる。
//!
//! 基準は `tools/gen_exif_fixture.py` が作る `tests/fixtures/exif-python.json`（合成の JPEG のバイト列と、
//! Pillow を通した結果）。Python と**意図して違える点**（`docs/rust-migration/server.md` §0・§12）は
//! `rust_differs` に列挙し、それ以外は完全一致を求める。

use base64::Engine;
use geoframe::imaging::{ImageError, exif_info_from_bytes, norm_time};
use serde_json::Value;

fn doc() -> Value {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/exif-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Python の結果から、Rust が意図して違える点の期待値（`lat`, `lon`, `taken_at`）。
/// `None` は「Python と同じ」。
fn rust_differs(name: &str, python_taken: &str) -> Option<(Option<f64>, Option<f64>, String)> {
    match name {
        // 壊れた GPS: Python は例外（写真ごと索引しない）か NaN。Rust は位置なしで時刻は残す
        "two_components" | "lat_is_short" | "denominator_zero" | "denominator_zero_seconds" => {
            Some((None, None, "2024-05-06 07:08".into()))
        }
        _ => {
            let _ = python_taken;
            None
        }
    }
}

#[test]
fn exifの読み取りはpythonと一致する() {
    let doc = doc();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 40, "ケースが少なすぎる");
    let mut compared = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(case["jpeg_base64"].as_str().unwrap())
            .unwrap();
        let python = &case["python"];
        let rust = exif_info_from_bytes(&bytes);

        if python.get("error").is_some() {
            match name {
                // 壊れた GPS は Rust では位置なしで成功する（上の `rust_differs`）
                "two_components" | "lat_is_short" => {
                    let info = rust.unwrap_or_else(|e| panic!("{name}: {e}"));
                    let want = rust_differs(name, "").unwrap();
                    assert_eq!(
                        (info.meta.lat, info.meta.lon, info.meta.taken_at),
                        want,
                        "{name}"
                    );
                }
                // 画像として読めない: どちらも失敗
                "not_an_image" => assert!(matches!(rust, Err(ImageError::NotAnImage)), "{name}"),
                other => panic!("未知の Python 例外のケース: {other}"),
            }
            compared += 1;
            continue;
        }
        let info = rust.unwrap_or_else(|e| panic!("{name}: {e}"));
        let taken = python["taken_at"].as_str().unwrap();
        let want = rust_differs(name, taken).unwrap_or_else(|| {
            let f = |v: &Value| v.as_f64();
            (f(&python["lat"]), f(&python["lon"]), taken.to_string())
        });
        // NaN は JSON では文字列 "NaN"（`as_f64` が `None` になる）。Rust は位置なしに揃える
        assert_eq!(
            (info.meta.lat, info.meta.lon, info.meta.taken_at.clone()),
            want,
            "{name}"
        );
        compared += 1;
    }
    assert_eq!(compared, cases.len());
}

#[test]
fn norm_timeはpythonと一致する_ただし全角などの数字はasciiにする() {
    let doc = doc();
    let table = doc["norm_time"].as_array().unwrap();
    assert!(table.len() >= 10);
    for row in table {
        let input = row["input"].as_str().unwrap_or("");
        let python = row["python"].as_str().unwrap();
        let rust = norm_time(input);
        // Python は元の文字のまま、Rust は ASCII の数字（数字以外は同じ）
        let want: String = python
            .chars()
            .map(|c| match c {
                '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
                '٠'..='٩' => char::from(b'0' + (c as u32 - '٠' as u32) as u8),
                c => c,
            })
            .collect();
        assert_eq!(rust, want, "{input:?}");
        if input.is_ascii() {
            assert_eq!(rust, python, "{input:?}");
        }
    }
}
