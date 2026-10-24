//! JPEG の EOI の欠けや切れ方を、Python（Pillow）の`make_derivative`の結果と突き合わせる。
//!
//! 基準は `tools/gen_jpeg_eoi_fixture.py` が作る `tests/fixtures/jpeg-eoi-python.json`。Rust が意図して違える点
//! （`docs/rust-migration/server.md` §0・§12.8）は下の表に列挙し、それ以外は Python と同じ結果を求める。
//!
//! - Python が読むが Rust は失敗にする: EOI が無く、その後ろにゼロ埋めやゴミが続くファイル。Pillow は
//!   ゴミまで画像データとして読み、足りない分を補う。Rust は、切れたファイルと見分けられないので失敗にする。
//! - Python は失敗するが Rust は読む: SOI の直後のゴミ（Pillow は先頭の3バイトが`FFD8FF`でないと開けない。
//!   Rust は libjpeg 相当の許容のため、マーカーの並びだけの失敗は読み直す）。
//! - EOI だけが欠けてファイルがそこで終わる、または全ての切断点で切ったものは、どちらも失敗する
//!   （Pillow 12.3.0 で確認。「PillowはEOIの無いJPEGを読む」とは言えない）。

use base64::Engine;
use geoframe::imaging::render;
use serde_json::Value;

fn doc() -> Value {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/jpeg-eoi-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn decode(s: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_str().unwrap())
        .unwrap()
}

/// Rust が Python と違う結果にするケース: `(名前, Rust の結果が ok か)`
const RUST_DIFFERS: [(&str, bool); 3] = [
    ("no_eoi_then_zero_padding", false),
    ("no_eoi_then_garbage", false),
    ("garbage_right_after_soi", true),
];

#[test]
fn eoiの扱いはpythonと一致する_違える点を除く() {
    let doc = doc();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let python_ok = case["python"] == "ok";
        let rust_ok = render(&decode(&case["jpeg_base64"]), 0).is_ok();
        let want = RUST_DIFFERS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(python_ok, |&(_, ok)| ok);
        assert_eq!(rust_ok, want, "{name}（Python: {}）", case["python"]);
        if RUST_DIFFERS.iter().any(|(n, _)| *n == name) {
            assert_ne!(rust_ok, python_ok, "{name}: 表の差が Python と同じになった");
        }
    }
    // 表に載せたケースがフィクスチャに全部ある
    for (name, _) in RUST_DIFFERS {
        assert!(cases.iter().any(|c| c["name"] == name), "{name}");
    }
}

#[test]
fn 全ての切断点でpythonもrustも読めない() {
    let doc = doc();
    let jpeg = decode(&doc["sweep"]["jpeg_base64"]);
    let python_ok: Vec<usize> = doc["sweep"]["python_ok_cuts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| usize::try_from(v.as_u64().unwrap()).unwrap())
        .collect();
    let sos = jpeg.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
    for n in sos..jpeg.len() {
        let rust_ok = render(&jpeg[..n], 0).is_ok();
        // Pillow が読めない切断点は Rust も読めない。Pillow が読める切断点があれば、そこは差として記録される
        assert!(
            !rust_ok || python_ok.contains(&n),
            "切断点 {n} で Rust だけが読めた"
        );
    }
    // 基準そのもの: Pillow 12.3.0 では、どの切断点も読めない
    assert!(
        python_ok.is_empty(),
        "Pillow が読める切断点がある: {python_ok:?}"
    );
}
