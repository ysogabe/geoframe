//! 4.3（明るさ・シャープネス）。期待値は Pillow 11.1.0 で計算して固定してある
//! （`tests/fixtures/measure/gen_expected.py` が `expected.json` と JPEG を作る。試験の実行時に Python は使わない）。
//!
//! - **縮小前の生のグレー**（合成。パターンは gen_expected.py と同じ整数演算）: Pillow の `thumbnail`・`Kernel` と
//!   **画素単位で一致**する（縮小後の大きさ、縮小後と応答のバイト列の CRC、小数 2 桁の値の文字列まで）。
//! - **JPEG**（Pillow が作り、Pillow がデコードして値を求めたもの）: デコーダの違い（zune-jpeg と libjpeg-turbo）の分だけずれる。
//!   許容幅は 581 枚の実測（旧 manifest の値との差）から決めた `measure::*_TOLERANCE`。

use flate2::Crc;
use geoframe_export::jpeg::{self, Inspection};
use geoframe_export::measure;
use serde_json::Value;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/measure");

fn expected() -> Value {
    serde_json::from_slice(&std::fs::read(format!("{DIR}/expected.json")).expect("expected.json"))
        .expect("JSON")
}

fn crc(bytes: &[u8]) -> u64 {
    let mut c = Crc::new();
    c.update(bytes);
    u64::from(c.sum())
}

fn grad(x: usize, y: usize, w: usize, h: usize) -> usize {
    ((x * 255 / (w - 1).max(1)) * 3 / 4 + (y * 64) / (h - 1).max(1)).min(255)
}

/// gen_expected.py の `pattern`。
fn pattern(kind: &str, w: usize, h: usize) -> Vec<u8> {
    match kind {
        "flat" => vec![77; w * h],
        "noise" => {
            let mut s: u64 = 12345;
            (0..w * h)
                .map(|_| {
                    s = (s * 1_103_515_245 + 12_345) & 0x7FFF_FFFF;
                    ((s >> 16) & 255) as u8
                })
                .collect()
        }
        _ => (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| match kind {
                "gradient" => grad(x, y, w, h) as u8,
                "stripes" => {
                    if (x / 2) % 2 == 1 {
                        255
                    } else {
                        0
                    }
                }
                "checker" => {
                    if (x ^ y) & 1 == 1 {
                        255
                    } else {
                        0
                    }
                }
                "mixed" => {
                    if y >= h / 2 {
                        if (x / 3) % 2 == 1 { 255 } else { 20 }
                    } else {
                        grad(x, y, w, h) as u8
                    }
                }
                other => panic!("{other}"),
            })
            .collect(),
    }
}

fn s(v: &Value) -> &str {
    v.as_str().expect("文字列")
}

fn n(v: &Value) -> usize {
    v.as_u64().expect("整数") as usize
}

#[test]
fn the_expected_values_were_made_with_pillow_11_1_0() {
    assert_eq!(s(&expected()["pillow"]), "11.1.0");
}

#[test]
fn raw_buffers_match_pillow_pixel_for_pixel() {
    let expected = expected();
    let cases = expected["raw"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let (kind, w, h) = (s(&case["kind"]), n(&case["width"]), n(&case["height"]));
        let label = format!("{kind} {w}x{h}");
        let gray = pattern(kind, w, h);
        // 縮小後の大きさ（縮小しないときは元の大きさ）
        let want = (n(&case["thumb"][0]), n(&case["thumb"][1]));
        let (tw, th) = measure::thumbnail_size(w as u32, h as u32, measure::THUMBNAIL_MAX)
            .map_or((w, h), |(a, b)| (a as usize, b as usize));
        assert_eq!((tw, th), want, "{label}: 縮小後の大きさ");
        // 縮小後のバイト列、応答のバイト列
        let small = if (tw, th) == (w, h) {
            gray.clone()
        } else {
            measure::resize_bicubic(&gray, w, h, tw, th)
        };
        assert_eq!(
            crc(&small),
            case["thumb_crc32"].as_u64().unwrap(),
            "{label}: 縮小"
        );
        let lap = measure::laplacian_response(&small, tw, th);
        assert_eq!(
            crc(&lap),
            case["lap_crc32"].as_u64().unwrap(),
            "{label}: ラプラシアン"
        );
        // 小数 2 桁の値（文字列で完全一致）
        let m = measure::measure(&gray, w, h);
        assert_eq!(
            format!("{:.2}", m.mean_brightness),
            s(&case["mean_brightness"]),
            "{label}: 明るさ"
        );
        assert_eq!(
            format!("{:.2}", m.sharpness_laplacian_var),
            s(&case["sharpness_laplacian_var"]),
            "{label}: シャープネス"
        );
    }
}

#[test]
fn reduce_matches_pillow_pixel_for_pixel() {
    let expected = expected();
    let cases = expected["reduce"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let (kind, w, h) = (s(&case["kind"]), n(&case["width"]), n(&case["height"]));
        let (fx, fy) = (n(&case["factor"][0]), n(&case["factor"][1]));
        let label = format!("{kind} {w}x{h} / {fx}x{fy}");
        let (out, ow, oh) = measure::reduce(&pattern(kind, w, h), w, h, fx, fy);
        assert_eq!(
            (ow, oh),
            (n(&case["size"][0]), n(&case["size"][1])),
            "{label}: 大きさ"
        );
        assert_eq!(crc(&out), case["crc32"].as_u64().unwrap(), "{label}");
    }
}

#[test]
fn the_reduce_boundary_is_covered() {
    // 長辺 4095 は縮小の比が 4 未満で reduce が入らず、4096 から入る（Pillow の `int(元 / 先 / 2.0) > 1`）。
    // 境目の画像が、期待値の中にある
    let expected = expected();
    let has = |w: u64, h: u64| {
        expected["raw"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["width"] == w && c["height"] == h)
    };
    assert!(has(4095, 128) && has(4096, 128) && has(8192, 128));
}

#[test]
fn jpeg_fixtures_match_pillow_within_the_tolerance() {
    let expected = expected();
    let cases = expected["jpeg"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = s(&case["name"]);
        let bytes = std::fs::read(format!("{DIR}/{name}.jpg")).expect("fixture");
        assert_eq!(bytes.len(), n(&case["bytes"]), "{name}");
        // Pillow が作った JPEG は、メタデータの区画を持たない（JFIF だけ）
        let Ok(Inspection::Clean(header)) = jpeg::inspect(&bytes, 60_000_000) else {
            panic!("{name}: 区画の検査に通らない");
        };
        assert_eq!(
            (header.width as usize, header.height as usize),
            (n(&case["width"]), n(&case["height"])),
            "{name}"
        );
        let gray = jpeg::decode_gray(&bytes, header).expect("デコード");
        assert_eq!(gray.data.len(), gray.width * gray.height);
        let m = measure::measure(&gray.data, gray.width, gray.height);
        let (b, sh) = (
            s(&case["mean_brightness"]).parse::<f64>().unwrap(),
            s(&case["sharpness_laplacian_var"]).parse::<f64>().unwrap(),
        );
        println!(
            "{name}: 明るさ {} / Pillow {b}、シャープネス {} / Pillow {sh}",
            m.mean_brightness, m.sharpness_laplacian_var
        );
        assert!(
            measure::brightness_within_tolerance(m.mean_brightness, b),
            "{name}: 明るさ {} / Pillow {b}",
            m.mean_brightness
        );
        assert!(
            measure::sharpness_within_tolerance(m.sharpness_laplacian_var, sh),
            "{name}: シャープネス {} / Pillow {sh}",
            m.sharpness_laplacian_var
        );
    }
}

#[test]
fn reduce_with_a_huge_factor_does_not_overflow() {
    // 区画が 4096×4096 でも桁あふれしない（Pillow の u32 の範囲を超える係数。サムネイルの経路では起きない）
    let (w, h) = (4096usize, 4096usize);
    let src = vec![200u8; w * h];
    let (out, ow, oh) = measure::reduce(&src, w, h, 4096, 4096);
    assert_eq!((ow, oh), (1, 1));
    assert!((i32::from(out[0]) - 200).abs() <= 1, "{}", out[0]);
}
