//! 向き（EXIF の Orientation・XMP の `tiff:Orientation`）を、Python の `make_derivative` の結果と突き合わせる。
//!
//! 基準は `tests/fixtures/orientation-python.json`（Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_orientation_fixture.py` は P5b で削除した。再生成はできない。合成の画像と、
//! Pillow を通した出力の寸法・四隅の色）。Python と**意図して違える点**（`docs/rust-migration/server.md` §0・§12）は
//! `rust_differs` に列挙し、それ以外は完全一致を求める。

use base64::Engine;
use geoframe::imaging::render;
use serde_json::Value;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;

const MARK: usize = 20;
const COLORS: [(&str, [i32; 3]); 5] = [
    ("red", [255, 0, 0]),
    ("green", [0, 160, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("white", [255, 255, 255]),
];

fn doc() -> Value {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/orientation-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// 出力の寸法と、四隅の目印の色の名前（Python のフィクスチャ生成と同じ位置・同じ決め方）
fn measure(jpeg: &[u8]) -> ((usize, usize), Vec<&'static str>) {
    let mut dec = JpegDecoder::new(ZCursor::new(jpeg));
    let px = dec.decode().unwrap();
    let (w, h) = dec.dimensions().unwrap();
    assert_eq!(px.len(), w * h * 3);
    let names = [
        (MARK / 2, MARK / 2),
        (w - 1 - MARK / 2, MARK / 2),
        (MARK / 2, h - 1 - MARK / 2),
        (w - 1 - MARK / 2, h - 1 - MARK / 2),
    ]
    .iter()
    .map(|&(x, y)| {
        let p = &px[(y * w + x) * 3..][..3];
        COLORS
            .iter()
            .min_by_key(|(_, c)| (0..3).map(|i| (c[i] - i32::from(p[i])).pow(2)).sum::<i32>())
            .unwrap()
            .0
    })
    .collect();
    ((w, h), names)
}

/// Rust が Python と違える点: 圧縮した XMP（`zTXt`・圧縮した `iTXt`）は展開しないので、向きを当てない。
fn rust_differs(name: &str) -> bool {
    matches!(name, "png_xmp_itxt_compressed_6" | "png_xmp_ztxt_6")
}

#[test]
fn 向きはpythonと一致する() {
    let doc = doc();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 50, "ケースが少なすぎる");
    let mut compared = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(case["data_base64"].as_str().unwrap())
            .unwrap();
        let rotation = i32::try_from(case["rotation"].as_i64().unwrap()).unwrap();
        let python_size: Vec<usize> = case["python"]["size"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| usize::try_from(v.as_u64().unwrap()).unwrap())
            .collect();
        let python_corners: Vec<&str> = case["python"]["corners"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        let out = render(&bytes, rotation).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ((w, h), corners) = measure(&out.jpeg);
        if rust_differs(name) {
            // 向きを当てない（元のまま）。Python は回している
            assert_ne!(python_size, vec![140, 90], "{name}: Python が回していない");
            assert_eq!((w, h), (140, 90), "{name}");
            assert_eq!(corners, ["red", "green", "blue", "yellow"], "{name}");
        } else {
            assert_eq!(vec![w, h], python_size, "{name}: 寸法");
            assert_eq!(corners, python_corners, "{name}: 四隅の色");
        }
        compared += 1;
    }
    assert_eq!(compared, cases.len());
}

#[test]
fn xmpだけに向きがある140x90のjpegは90x140になる() {
    // レビューの再現: XMP の向き 6 だけの JPEG は、Python は 90x140、Rust も同じになる
    let doc = doc();
    let case = doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "jpeg_xmp_attr_6")
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(case["data_base64"].as_str().unwrap())
        .unwrap();
    let out = render(&bytes, 0).unwrap();
    assert_eq!(out.info.output, (90, 140));
    assert_eq!(out.info.exif_orientation, 6);
}
