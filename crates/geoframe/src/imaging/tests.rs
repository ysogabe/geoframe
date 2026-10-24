//! 合成した小さな画像だけで、向き・縮小・EXIF を残さないこと・ICC・失敗の扱いを確かめる（写真は使わない）。
//! HEIC は `tests/fixtures/synthetic_*.heic`（pillow-heif で作った 64x48 の合成画像。写真ではない）。

use std::path::PathBuf;

use image::ImageEncoder;

use super::*;
use crate::datadir::DataDir;
use crate::testutil::TempDir;

// ---------------------------------------------------------------------------
// 合成画像の部品

/// 左半分が赤、右半分が青の RGB 画素
fn two_tone(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..h {
        for x in 0..w {
            v.extend_from_slice(if x < w / 2 {
                &[255, 0, 0]
            } else {
                &[0, 0, 255]
            });
        }
    }
    v
}

fn jpeg_encode(
    data: &[u8],
    w: u32,
    h: u32,
    color: jpeg_encoder::ColorType,
    icc: Option<&[u8]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, 100);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
    if let Some(icc) = icc {
        enc.add_icc_profile(icc).unwrap();
    }
    enc.encode(data, w as u16, h as u16, color).unwrap();
    out
}

fn jpeg_rgb(w: u32, h: u32) -> Vec<u8> {
    jpeg_encode(&two_tone(w, h), w, h, jpeg_encoder::ColorType::Rgb, None)
}

// --- TIFF（EXIF）の組み立て。ビッグエンディアン ---

enum V {
    Short(u16),
    Ascii(&'static str),
    Rationals(Vec<(u32, u32)>),
    Long(u32),
}

/// IFD0（とその下の GPS IFD）を持つ TIFF 構造。`gps` が空でなければ GPS IFD を付ける。
fn tiff(ifd0: Vec<(u16, V)>, gps: Vec<(u16, V)>) -> Vec<u8> {
    fn encode(v: &V) -> (u16, u32, Vec<u8>) {
        match v {
            V::Short(n) => (3, 1, n.to_be_bytes().to_vec()),
            V::Long(n) => (4, 1, n.to_be_bytes().to_vec()),
            V::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len() as u32, b)
            }
            V::Rationals(rs) => (
                5,
                rs.len() as u32,
                rs.iter()
                    .flat_map(|(n, d)| [n.to_be_bytes(), d.to_be_bytes()].concat())
                    .collect(),
            ),
        }
    }
    let mut ifd0 = ifd0;
    if !gps.is_empty() {
        ifd0.push((0x8825, V::Long(0)));
    }
    let ifd0_size = 2 + 12 * ifd0.len() + 4;
    let gps_off = 8 + ifd0_size;
    let gps_size = if gps.is_empty() {
        0
    } else {
        2 + 12 * gps.len() + 4
    };
    let mut data_off = gps_off + gps_size;
    let mut data = Vec::new();
    let mut ifd_bytes = |entries: &[(u16, V)], gps_ptr: usize| -> Vec<u8> {
        let mut out = (entries.len() as u16).to_be_bytes().to_vec();
        for (tag, v) in entries {
            let (typ, count, mut bytes) = if *tag == 0x8825 {
                (4, 1, (gps_ptr as u32).to_be_bytes().to_vec())
            } else {
                encode(v)
            };
            out.extend_from_slice(&tag.to_be_bytes());
            out.extend_from_slice(&typ.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            if bytes.len() <= 4 {
                bytes.resize(4, 0);
                out.extend_from_slice(&bytes);
            } else {
                out.extend_from_slice(&(data_off as u32).to_be_bytes());
                data_off += bytes.len();
                data.extend_from_slice(&bytes);
            }
        }
        out.extend_from_slice(&0u32.to_be_bytes());
        out
    };
    let mut out = b"MM\0*\0\0\0\x08".to_vec();
    out.extend(ifd_bytes(&ifd0, gps_off));
    if !gps.is_empty() {
        out.extend(ifd_bytes(&gps, 0));
    }
    out.extend(data);
    out
}

/// 向きと、出力に残ってはいけない機種名・GPS を持つ EXIF
fn exif_tiff(orientation: u16) -> Vec<u8> {
    tiff(
        vec![
            (0x0112, V::Short(orientation)),
            (0x010F, V::Ascii("SynthMaker")),
        ],
        vec![
            (1, V::Ascii("N")),
            (2, V::Rationals(vec![(10, 1), (20, 1), (30, 1)])),
            (3, V::Ascii("E")),
            (4, V::Rationals(vec![(100, 1), (40, 1), (50, 1)])),
        ],
    )
}

fn with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut seg = b"Exif\0\0".to_vec();
    seg.extend_from_slice(tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_encode(data: &[u8], w: u32, h: u32, color: image::ExtendedColorType) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(data, w, h, color)
        .unwrap();
    out
}

/// IHDR の直後に `eXIf` チャンクを差し込む
fn png_with_exif(png: &[u8], tiff: &[u8]) -> Vec<u8> {
    let ihdr_end = 8 + 12 + 13;
    let mut chunk = (tiff.len() as u32).to_be_bytes().to_vec();
    let mut body = b"eXIf".to_vec();
    body.extend_from_slice(tiff);
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    let mut out = png[..ihdr_end].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[ihdr_end..]);
    out
}

/// 非圧縮 RGB 8 ビットの TIFF（IFD0 に向きと時刻、Exif の GPS）。ビッグエンディアンの最小構成。
fn tiff_image(w: u32, h: u32, orientation: u16) -> Vec<u8> {
    let pixels = two_tone(w, h);
    let entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (256, 4, 1, w.to_be_bytes().to_vec()),
        (257, 4, 1, h.to_be_bytes().to_vec()),
        (
            258,
            3,
            3,
            [8u16.to_be_bytes(), 8u16.to_be_bytes(), 8u16.to_be_bytes()].concat(),
        ),
        (259, 3, 1, 1u16.to_be_bytes().to_vec()),
        (262, 3, 1, 2u16.to_be_bytes().to_vec()),
        (273, 4, 1, Vec::new()), // StripOffsets（後で埋める）
        (274, 3, 1, orientation.to_be_bytes().to_vec()),
        (277, 3, 1, 3u16.to_be_bytes().to_vec()),
        (278, 4, 1, h.to_be_bytes().to_vec()),
        (279, 4, 1, (pixels.len() as u32).to_be_bytes().to_vec()),
    ];
    let ifd_size = 2 + 12 * entries.len() + 4;
    let extra_off = 8 + ifd_size; // BitsPerSample の 6 バイト
    let pixel_off = extra_off + 6;
    let mut out = b"MM\0*\0\0\0\x08".to_vec();
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (tag, typ, count, value) in &entries {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&typ.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        match *tag {
            258 => out.extend_from_slice(&(extra_off as u32).to_be_bytes()),
            273 => out.extend_from_slice(&(pixel_off as u32).to_be_bytes()),
            _ => {
                let mut v = value.clone();
                if *typ == 3 && *count == 1 {
                    v.resize(4, 0); // SHORT は左詰め
                }
                out.extend_from_slice(&v);
            }
        }
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&[0, 8, 0, 8, 0, 8]);
    out.extend_from_slice(&pixels);
    out
}

// --- 出力の検査 ---

fn decode_out(jpeg: &[u8]) -> (u32, u32, usize, Vec<u8>) {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let mut dec = JpegDecoder::new(ZCursor::new(jpeg));
    let px = dec.decode().unwrap();
    let (w, h) = dec.dimensions().unwrap();
    let comps = px.len() / (w * h);
    (w as u32, h as u32, comps, px)
}

/// SOF の成分数（1 ならグレースケール）
fn jpeg_components(jpeg: &[u8]) -> u8 {
    let mut pos = 2;
    while pos + 4 <= jpeg.len() {
        assert_eq!(jpeg[pos], 0xFF);
        let marker = jpeg[pos + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return jpeg[pos + 9];
        }
        pos += 2 + len;
    }
    panic!("SOF が無い");
}

/// SOF のマーカー（バイト列の先頭からの位置）
fn sof_position(jpeg: &[u8]) -> usize {
    let mut pos = 2;
    loop {
        let marker = jpeg[pos + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return pos;
        }
        pos += 2 + len;
    }
}

fn px(jpeg: &[u8], x: u32, y: u32) -> [u8; 3] {
    let (w, _, comps, data) = decode_out(jpeg);
    assert_eq!(comps, 3);
    let i = ((y * w + x) * 3) as usize;
    [data[i], data[i + 1], data[i + 2]]
}

fn reddish(p: [u8; 3]) -> bool {
    p[0] > 170 && p[2] < 90
}

fn bluish(p: [u8; 3]) -> bool {
    p[2] > 170 && p[0] < 90
}

/// 出力に EXIF（`Exif\0\0`）も、機種名も、ICC も無い
fn assert_clean(jpeg: &[u8]) {
    assert!(
        !jpeg.windows(6).any(|w| w == b"Exif\0\0"),
        "EXIF が残っている"
    );
    assert!(
        !jpeg.windows(10).any(|w| w == b"SynthMaker"),
        "機種名が残っている"
    );
    assert!(
        !jpeg.windows(12).any(|w| w == b"ICC_PROFILE\0"),
        "ICC が残っている"
    );
    // GPS の IFD ポインタ（0x8825）を持つ TIFF も無い
    assert!(!jpeg.windows(4).any(|w| w == b"MM\0*" || w == b"II*\0"));
}

fn render_ok(bytes: &[u8], rotation: i32) -> Rendered {
    render(bytes, rotation).unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

// ---------------------------------------------------------------------------
// 向きと回転

#[test]
fn exifの向きに応じて寸法と向きが変わる() {
    let base = jpeg_rgb(60, 40);
    for (o, dims) in [
        (1u16, (60, 40)),
        (3, (60, 40)),
        (6, (40, 60)),
        (8, (40, 60)),
    ] {
        let out = render_ok(&with_exif(&base, &exif_tiff(o)), 0);
        assert_eq!(out.info.input, (60, 40));
        assert_eq!(out.info.output, dims, "o={o}");
        assert_eq!(out.info.exif_orientation, o as u8);
        let (w, h) = dims;
        match o {
            1 => assert!(reddish(px(&out.jpeg, 5, h / 2)) && bluish(px(&out.jpeg, w - 5, h / 2))),
            3 => assert!(bluish(px(&out.jpeg, 5, h / 2)) && reddish(px(&out.jpeg, w - 5, h / 2))),
            // 時計回り 90 度: 元の左（赤）が上へ
            6 => assert!(reddish(px(&out.jpeg, w / 2, 5)) && bluish(px(&out.jpeg, w / 2, h - 5))),
            // 反時計回り 90 度: 元の左（赤）が下へ
            8 => assert!(bluish(px(&out.jpeg, w / 2, 5)) && reddish(px(&out.jpeg, w / 2, h - 5))),
            _ => unreachable!(),
        }
        assert_clean(&out.jpeg);
    }
}

#[test]
fn 反転を伴う向き2_4_5_7も当たる() {
    let base = jpeg_rgb(60, 40);
    // 2: 左右反転 / 4: 上下反転（左右は変わらない） / 5: 転置 / 7: 反転転置
    let cases: [(u16, (u32, u32)); 4] =
        [(2, (60, 40)), (4, (60, 40)), (5, (40, 60)), (7, (40, 60))];
    for (o, dims) in cases {
        let out = render_ok(&with_exif(&base, &exif_tiff(o)), 0);
        assert_eq!(out.info.output, dims, "o={o}");
        let (w, h) = dims;
        match o {
            2 => assert!(bluish(px(&out.jpeg, 5, h / 2)) && reddish(px(&out.jpeg, w - 5, h / 2))),
            4 => assert!(reddish(px(&out.jpeg, 5, h / 2)) && bluish(px(&out.jpeg, w - 5, h / 2))),
            // 転置: 元の左（赤）が上へ
            5 => assert!(reddish(px(&out.jpeg, w / 2, 5)) && bluish(px(&out.jpeg, w / 2, h - 5))),
            // 反転転置: 元の左（赤）が下へ
            7 => assert!(bluish(px(&out.jpeg, w / 2, 5)) && reddish(px(&out.jpeg, w / 2, h - 5))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn 範囲外の向きは当てない() {
    let base = jpeg_rgb(60, 40);
    for o in [0u16, 9, 255] {
        let out = render_ok(&with_exif(&base, &exif_tiff(o)), 0);
        assert_eq!(out.info.output, (60, 40), "o={o}");
        assert_eq!(out.info.exif_orientation, 1);
    }
}

#[test]
fn 手動回転はexifの向きのあとに当たる() {
    let base = jpeg_rgb(60, 40);
    // 向き 3（180 度）のあとに時計回り 90 度 = 元から見て反時計回り 90 度
    let out = render_ok(&with_exif(&base, &exif_tiff(3)), 90);
    assert_eq!(out.info.output, (40, 60));
    assert!(bluish(px(&out.jpeg, 20, 5)) && reddish(px(&out.jpeg, 20, 55)));
    // 向きなし + 270 度
    let out = render_ok(&base, 270);
    assert_eq!(out.info.output, (40, 60));
    assert!(bluish(px(&out.jpeg, 20, 5)) && reddish(px(&out.jpeg, 20, 55)));
    // 360・-90 は剰余
    assert_eq!(render_ok(&base, 360).info.output, (60, 40));
    assert_eq!(render_ok(&base, -90).info.output, (40, 60));
    // 90 の倍数でない角度は断る
    assert!(matches!(render(&base, 45), Err(ImageError::BadRotation)));
}

// ---------------------------------------------------------------------------
// 縮小

#[test]
fn 長辺は2560に収まり小さい画像は拡大しない() {
    assert_eq!(target_dims(4032, 3024, DERIV_PX), (2560, 1920));
    assert_eq!(target_dims(2560, 1000, DERIV_PX), (2560, 1000));
    assert_eq!(target_dims(300, 200, DERIV_PX), (300, 200));

    let out = render_ok(&jpeg_rgb(2600, 100), 0);
    assert_eq!(out.info.output, (2560, 98));
    let (w, h, _, _) = decode_out(&out.jpeg);
    assert_eq!((w, h), (2560, 98));
    let out = render_ok(&jpeg_rgb(50, 40), 0);
    assert_eq!(out.info.output, (50, 40));
}

#[test]
fn 縮小は向きを当てたあとの寸法で行う() {
    let src = jpeg_rgb(2600, 100);
    let out = render_ok(&with_exif(&src, &exif_tiff(6)), 0);
    assert_eq!(out.info.output, (98, 2560));
    // 手動回転でも同じ
    let out = render_ok(&src, 90);
    assert_eq!(out.info.output, (98, 2560));
    // 向き 6 + 手動 90 で 180 度回るので、縦横は元のまま
    let out = render_ok(&with_exif(&src, &exif_tiff(6)), 90);
    assert_eq!(out.info.output, (2560, 98));
}

#[test]
fn 縮小の寸法はpillowのthumbnailと一致する() {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/thumbnail-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 100);
    for c in cases {
        let i = |v: &serde_json::Value, k: usize| v[k].as_u64().unwrap() as u32;
        let (w, h) = (i(&c["input"], 0), i(&c["input"], 1));
        let want = (i(&c["python"], 0), i(&c["python"], 1));
        assert_eq!(target_dims(w, h, DERIV_PX), want, "{w}x{h}");
    }
}

// ---------------------------------------------------------------------------
// 形式

#[test]
fn グレースケールは1成分のまま出す() {
    let gray: Vec<u8> = (0..32 * 24).map(|i| (i % 256) as u8).collect();
    let jpeg = jpeg_encode(&gray, 32, 24, jpeg_encoder::ColorType::Luma, None);
    let out = render_ok(&jpeg, 0);
    assert!(out.info.gray);
    assert_eq!(jpeg_components(&out.jpeg), 1);

    let png = png_encode(&gray, 32, 24, image::ExtendedColorType::L8);
    let out = render_ok(&png, 0);
    assert!(out.info.gray);
    assert_eq!(out.info.format, Format::Png);
    assert_eq!(jpeg_components(&out.jpeg), 1);
}

#[test]
fn アルファ付きとパレットのpngはrgbにする() {
    let rgba: Vec<u8> = (0..16 * 16)
        .flat_map(|i| [200, (i % 256) as u8, 30, 128])
        .collect();
    let png = png_encode(&rgba, 16, 16, image::ExtendedColorType::Rgba8);
    let out = render_ok(&png, 0);
    assert!(!out.info.gray);
    assert_eq!(jpeg_components(&out.jpeg), 3);
    // 16 ビットも通る
    let v16: Vec<u8> = (0..8 * 8 * 3)
        .flat_map(|i| (((i * 257) % 65536) as u16).to_ne_bytes())
        .collect();
    let png16 = png_encode(&v16, 8, 8, image::ExtendedColorType::Rgb16);
    assert_eq!(render_ok(&png16, 0).info.output, (8, 8));
}

#[test]
fn pngのexifの向きも当たる() {
    let png = png_encode(&two_tone(60, 40), 60, 40, image::ExtendedColorType::Rgb8);
    let out = render_ok(&png_with_exif(&png, &exif_tiff(6)), 0);
    assert_eq!(out.info.output, (40, 60));
    assert!(reddish(px(&out.jpeg, 20, 5)) && bluish(px(&out.jpeg, 20, 55)));
    assert_clean(&out.jpeg);
}

#[test]
fn tiffは読めて向きも当たる() {
    let out = render_ok(&tiff_image(60, 40, 1), 0);
    assert_eq!(out.info.format, Format::Tiff);
    assert_eq!(out.info.output, (60, 40));
    assert!(reddish(px(&out.jpeg, 5, 20)) && bluish(px(&out.jpeg, 55, 20)));
    let out = render_ok(&tiff_image(60, 40, 6), 0);
    assert_eq!(out.info.output, (40, 60));
    assert!(reddish(px(&out.jpeg, 20, 5)) && bluish(px(&out.jpeg, 20, 55)));
}

// ---------------------------------------------------------------------------
// EXIF を残さない（JPEG・PNG・TIFF・HEIC のどの入力でも）

#[test]
fn 出力にexifとgpsを残さない() {
    let jpeg = with_exif(&jpeg_rgb(60, 40), &exif_tiff(6));
    assert!(
        jpeg.windows(10).any(|w| w == b"SynthMaker"),
        "入力には機種名があるはず"
    );
    assert_clean(&render_ok(&jpeg, 0).jpeg);
    assert_clean(&render_ok(&jpeg, 90).jpeg);
    let png = png_with_exif(
        &png_encode(&two_tone(30, 20), 30, 20, image::ExtendedColorType::Rgb8),
        &exif_tiff(1),
    );
    assert_clean(&render_ok(&png, 0).jpeg);
    for name in [
        "synthetic_o1.heic",
        "synthetic_o6.heic",
        "synthetic_p3.heic",
    ] {
        let heic = fixture(name);
        assert!(
            heic.windows(10).any(|w| w == b"SynthMaker"),
            "{name} には機種名があるはず"
        );
        assert_clean(&render_ok(&heic, 0).jpeg);
    }
}

// ---------------------------------------------------------------------------
// ICC

fn p3_profile() -> Vec<u8> {
    moxcms::ColorProfile::new_display_p3().encode().unwrap()
}

#[test]
fn 広色域のiccはsrgbへ変換して付けない() {
    // P3 の純緑は sRGB の外: 変換すると赤・青が 0 側へクリップされ、緑は飽和のまま
    let green: Vec<u8> = (0..32 * 32).flat_map(|_| [0u8, 255, 0]).collect();
    let src = jpeg_encode(
        &green,
        32,
        32,
        jpeg_encoder::ColorType::Rgb,
        Some(&p3_profile()),
    );
    let out = render_ok(&src, 0);
    assert_eq!(out.info.icc, IccOutcome::ToSrgb);
    let c = px(&out.jpeg, 16, 16);
    assert!(c[1] > 240 && c[0] < 40 && c[2] < 60, "{c:?}");
    assert_clean(&out.jpeg);

    // P3 の中間色は sRGB では彩度が上がる（赤が強くなる）
    let mid: Vec<u8> = (0..32 * 32).flat_map(|_| [200u8, 100, 100]).collect();
    let with_icc = jpeg_encode(
        &mid,
        32,
        32,
        jpeg_encoder::ColorType::Rgb,
        Some(&p3_profile()),
    );
    let without = jpeg_encode(&mid, 32, 32, jpeg_encoder::ColorType::Rgb, None);
    let converted = px(&render_ok(&with_icc, 0).jpeg, 16, 16);
    let dropped = px(&render_ok(&without, 0).jpeg, 16, 16);
    assert!(converted[0] > dropped[0], "{dropped:?} → {converted:?}");
}

#[test]
fn 変換できないiccは捨てて画像は処理を続ける() {
    let gray = moxcms::ColorProfile::new_gray_with_gamma(2.2)
        .encode()
        .unwrap();
    let broken = vec![0x42u8; 200];
    let data: Vec<u8> = (0..32 * 32).flat_map(|_| [200u8, 100, 100]).collect();
    let plain = render_ok(
        &jpeg_encode(&data, 32, 32, jpeg_encoder::ColorType::Rgb, None),
        0,
    );
    for (name, profile) in [("gray", gray), ("broken", broken)] {
        let src = jpeg_encode(&data, 32, 32, jpeg_encoder::ColorType::Rgb, Some(&profile));
        let out =
            render(&src, 0).unwrap_or_else(|e| panic!("{name}: ICC の失敗で画像ごと失敗した: {e}"));
        assert_eq!(out.info.icc, IccOutcome::Dropped, "{name}");
        // ICC なしと同じ出力になる
        assert_eq!(out.jpeg, plain.jpeg, "{name}");
        assert_clean(&out.jpeg);
    }
}

// ---------------------------------------------------------------------------
// 失敗

#[test]
fn 壊れた入力と巨大な入力は拒否する() {
    assert!(matches!(
        render(b"not an image", 0),
        Err(ImageError::NotAnImage)
    ));
    assert!(matches!(render(b"", 0), Err(ImageError::NotAnImage)));
    assert!(render(&[0xFF, 0xD8, 0xFF, 0xD9], 0).is_err());
    // 宣言の寸法が上限を超える JPEG は、デコードの前に断る（65535x65535 の SOF に書き換える）
    let mut huge = jpeg_rgb(16, 16);
    let sof = sof_position(&huge);
    huge[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    assert!(matches!(
        render(&huge, 0),
        Err(ImageError::TooManyPixels { .. })
    ));
}

#[test]
fn 途中で切れたファイルは失敗にする() {
    // 切れた JPEG は、`image` なら部分的な画像で成功してしまう。Pillow と同じく失敗にする
    let data: Vec<u8> = (0..200 * 150)
        .flat_map(|i| [(i % 251) as u8, (i % 241) as u8, (i % 239) as u8])
        .collect();
    let jpeg = jpeg_encode(&data, 200, 150, jpeg_encoder::ColorType::Rgb, None);
    for keep in [jpeg.len() / 4, jpeg.len() / 2, jpeg.len() - 200] {
        let err = render(&jpeg[..keep], 0)
            .err()
            .unwrap_or_else(|| panic!("keep={keep} が成功した"));
        assert!(
            matches!(err, ImageError::Decode { format: "jpeg", .. }),
            "{err}"
        );
    }
    let png = png_encode(&data, 200, 150, image::ExtendedColorType::Rgb8);
    for keep in [png.len() / 2, png.len() - 20] {
        assert!(render(&png[..keep], 0).is_err(), "png keep={keep}");
    }
    let heic = fixture("synthetic_o1.heic");
    let err = render(&heic[..heic.len() / 2], 0).err().unwrap();
    assert!(matches!(err, ImageError::Heic(_)), "{err}");
}

#[test]
fn エラーの文面にパスや内容が入らない() {
    let tmp = TempDir::new("imaging-err");
    let secret = tmp.path().join("ひみつの写真-12.34,56.78.jpg");
    std::fs::write(&secret, b"GPS 12.34 56.78 not a jpeg").unwrap();
    let err = make_derivative(&secret, &tmp.path().join("out.jpg"), 0).unwrap_err();
    let text = format!("{err} / {err:?}");
    assert!(
        !text.contains("ひみつ")
            && !text.contains("12.34")
            && !text.contains(tmp.path().to_str().unwrap()),
        "{text}"
    );
    // 無いファイル
    let err =
        make_derivative(&tmp.path().join("無い.jpg"), &tmp.path().join("o.jpg"), 0).unwrap_err();
    let text = format!("{err}");
    assert!(
        !text.contains("無い") && text.contains("not found"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// ファイルへの書き込み

fn dir_entries(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn partを経由して書き失敗しても残さない() {
    let tmp = TempDir::new("imaging-part");
    let src = tmp.path().join("src.jpg");
    let out_dir = tmp.path().join("derivatives");
    let dest = out_dir.join("abc.r90.jpg");
    std::fs::write(&src, with_exif(&jpeg_rgb(60, 40), &exif_tiff(1))).unwrap();

    let info = make_derivative(&src, &dest, 90).unwrap();
    assert_eq!(info.output, (40, 60));
    // 親ディレクトリは作られ、`.part` は残らない
    assert_eq!(dir_entries(&out_dir), ["abc.r90.jpg"]);
    assert_clean(&std::fs::read(&dest).unwrap());

    // 既存の出力は置き換わる
    let before = std::fs::read(&dest).unwrap();
    make_derivative(&src, &dest, 0).unwrap();
    assert_ne!(std::fs::read(&dest).unwrap(), before);
    assert_eq!(dir_entries(&out_dir), ["abc.r90.jpg"]);

    // 失敗（画像でない原本）: 出力も `.part` も作らない。既存の出力は壊さない
    let bad = tmp.path().join("bad.jpg");
    std::fs::write(&bad, b"nope").unwrap();
    let ok_bytes = std::fs::read(&dest).unwrap();
    assert!(make_derivative(&bad, &dest, 0).is_err());
    assert_eq!(dir_entries(&out_dir), ["abc.r90.jpg"]);
    assert_eq!(std::fs::read(&dest).unwrap(), ok_bytes);
}

#[test]
fn 書き込みが失敗しても_partを残さない() {
    let tmp = TempDir::new("imaging-part-fail");
    let src = tmp.path().join("src.jpg");
    std::fs::write(&src, jpeg_rgb(20, 20)).unwrap();
    // 出力先がディレクトリ: rename が失敗する。`.part` を残さない
    let dest = tmp.path().join("dest.jpg");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("keep"), b"x").unwrap();
    assert!(make_derivative(&src, &dest, 0).is_err());
    assert_eq!(dir_entries(tmp.path()), ["dest.jpg", "src.jpg"]);
}

#[test]
fn store_derivativeは検査した場所に書きdisplayを返す() {
    let tmp = TempDir::new("imaging-store");
    let data = DataDir::open(tmp.path()).unwrap();
    let src = tmp.path().join("tmp.bin");
    std::fs::write(&src, jpeg_rgb(30, 20)).unwrap();
    let name = derivative_name("1AbC_dEf-9", 90).unwrap();
    assert_eq!(name, "1AbC_dEf-9.r90.jpg");
    let stored = store_derivative(&data, &src, &name, 90).unwrap();
    assert_eq!(stored.display, "derivatives/1AbC_dEf-9.r90.jpg");
    assert!(tmp.path().join(&stored.display).is_file());
    // 名前に区切りが入っていたら書かない
    assert!(matches!(
        store_derivative(&data, &src, "../escape.jpg", 0),
        Err(ImageError::Path(_))
    ));
    assert!(!tmp.path().parent().unwrap().join("escape.jpg").exists());
}

#[test]
fn derivativeの名前は回転で変わりidの区切りを許さない() {
    assert_eq!(derivative_name("abc", 0).unwrap(), "abc.jpg");
    assert_eq!(derivative_name("abc", 90).unwrap(), "abc.r90.jpg");
    assert_eq!(derivative_name("abc", 270).unwrap(), "abc.r270.jpg");
    for bad in ["", "a/b", "../x", "a.jpg", "a b"] {
        assert!(derivative_name(bad, 0).is_err(), "{bad:?}");
    }
}

#[test]
fn 取り込む拡張子はpythonのextsと同じ() {
    for ok in [
        "a.jpg", "a.JPG", "a.jpeg", "a.png", "a.tif", "a.TIFF", "a.heic", "a.HEIF", "x/y.Jpeg",
    ] {
        assert!(is_supported_ext(&PathBuf::from(ok)), "{ok}");
    }
    for ng in [
        "a",
        "a.gif",
        "a.webp",
        "a.jpg.bak",
        "a.avif",
        ".jpg",
        "a.heic.part",
    ] {
        assert!(!is_supported_ext(&PathBuf::from(ng)), "{ng}");
    }
    assert!(is_browser_displayable_ext(&PathBuf::from("a.PNG")));
    assert!(!is_browser_displayable_ext(&PathBuf::from("a.heic")));
    assert!(!is_browser_displayable_ext(&PathBuf::from("a.tif")));
}

// ---------------------------------------------------------------------------
// HEIC

#[test]
fn heicはirotを一度だけ適用しexifを残さない() {
    let o1 = render_ok(&fixture("synthetic_o1.heic"), 0);
    assert_eq!(o1.info.format, Format::Heic);
    assert_eq!(o1.info.input, (64, 48));
    assert_eq!(o1.info.output, (64, 48));
    // 左が赤・右が青
    assert!(reddish(px(&o1.jpeg, 5, 24)) && bluish(px(&o1.jpeg, 59, 24)));

    // 向き 6: デコーダが irot を適用して 48x64 になる。EXIF の向きはさらに当てない
    let o6 = render_ok(&fixture("synthetic_o6.heic"), 0);
    assert_eq!(o6.info.input, (48, 64));
    assert_eq!(o6.info.output, (48, 64));
    assert_eq!(o6.info.exif_orientation, 1);
    assert!(reddish(px(&o6.jpeg, 24, 5)) && bluish(px(&o6.jpeg, 24, 59)));

    // 手動回転は HEIC にも当たる
    let r = render_ok(&fixture("synthetic_o1.heic"), 90);
    assert_eq!(r.info.output, (48, 64));
}

#[test]
fn heicのiccはsrgbへ変換する() {
    let out = render_ok(&fixture("synthetic_p3.heic"), 0);
    assert_eq!(out.info.icc, IccOutcome::ToSrgb);
    assert_clean(&out.jpeg);
    let plain = render_ok(&fixture("synthetic_o1.heic"), 0);
    assert_eq!(plain.info.icc, IccOutcome::None);
}

#[test]
fn heicの失敗は構造の分類を含みファイルの内容を含まない() {
    let heic = fixture("synthetic_o1.heic");
    let err = render(&heic[..heic.len() / 2], 0).err().unwrap();
    let text = err.to_string();
    assert!(
        text.contains("brand=heic") && text.contains("primary=") && text.contains("codec="),
        "{text}"
    );
    assert!(!text.contains("SynthMaker"), "{text}");
    // ブランドが ftyp だけで中身が空のファイルでも、分類は返る（落ちない）
    let mut stub = vec![0, 0, 0, 24];
    stub.extend_from_slice(b"ftypheic");
    stub.extend_from_slice(&[0; 12]);
    let err = render(&stub, 0).err().unwrap();
    assert!(matches!(err, ImageError::Heic(_)), "{err}");
}

#[test]
fn heicのexifを読める() {
    // 緯度だけ（経度が無い）のため位置は無い（Python も同じ）。向きは EXIF の値（irot と同じ 1 か 6）
    let o1 = exif_info_from_bytes(&fixture("synthetic_o1.heic")).unwrap();
    assert_eq!(o1.meta.lat, None);
    assert_eq!(o1.meta.lon, None);
    assert_eq!(o1.meta.taken_at, "");
    let o6 = exif_info_from_bytes(&fixture("synthetic_o6.heic")).unwrap();
    assert_eq!(o6.orientation, 6);
}

// ---------------------------------------------------------------------------
// EXIF（境界の基準は tests/exif_python.rs が Python と突き合わせる）

#[test]
fn exifの値と向きを読む() {
    let t = tiff(
        vec![(0x0112, V::Short(8))],
        vec![
            (1, V::Ascii("S")),
            (2, V::Rationals(vec![(12, 1), (30, 1), (0, 1)])),
            (3, V::Ascii("W")),
            (4, V::Rationals(vec![(78, 1), (45, 1), (0, 1)])),
        ],
    );
    let info = exif_info_from_bytes(&with_exif(&jpeg_rgb(8, 8), &t)).unwrap();
    assert_eq!(info.orientation, 8);
    assert_eq!(info.meta.lat, Some(-12.5));
    assert_eq!(info.meta.lon, Some(-78.75));
}

#[test]
fn exifが壊れていても画像は読める_落ちない() {
    let base = with_exif(&jpeg_rgb(16, 16), &exif_tiff(6));
    let exif_end = base
        .windows(2)
        .position(|w| w == [0xFF, 0xDB])
        .unwrap_or(base.len());
    // EXIF の区間の全てのバイトを順に壊す／全ての長さに切る: パニックしない。画像として読める限り成功する
    let mut seed = 0x1234_5678u32;
    for i in 4..exif_end.min(base.len()) {
        let mut broken = base.clone();
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        broken[i] = (seed >> 24) as u8;
        let _ = exif_info_from_bytes(&broken);
        let _ = render(&broken, 0);
    }
    for keep in 0..exif_end.min(base.len()) {
        let _ = exif_info_from_bytes(&base[..keep]);
    }
    // 向きを指すオフセットが範囲外
    let mut t = exif_tiff(6);
    t[4..8].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    let info = exif_info_from_bytes(&with_exif(&jpeg_rgb(8, 8), &t)).unwrap();
    assert_eq!(info.orientation, 1);
    assert_eq!(info.meta, ExifInfo::empty().meta);
}

#[test]
fn 画像でないファイルのexifは失敗にする() {
    let tmp = TempDir::new("imaging-exif");
    let p = tmp.path().join("a.jpg");
    std::fs::write(&p, b"hello").unwrap();
    assert!(matches!(extract_exif(&p), Err(ImageError::NotAnImage)));
    // EXIF の無い画像は失敗ではなく空の結果
    std::fs::write(&p, jpeg_rgb(8, 8)).unwrap();
    let meta = extract_exif(&p).unwrap();
    assert_eq!(meta, ExifInfo::empty().meta);
    // PNG の eXIf
    let png = png_with_exif(
        &png_encode(&two_tone(8, 8), 8, 8, image::ExtendedColorType::Rgb8),
        &tiff(
            vec![],
            vec![
                (2, V::Rationals(vec![(1, 1), (0, 1), (0, 1)])),
                (4, V::Rationals(vec![(2, 1), (0, 1), (0, 1)])),
            ],
        ),
    );
    std::fs::write(&p, png).unwrap();
    let meta = extract_exif(&p).unwrap();
    assert_eq!((meta.lat, meta.lon), (Some(1.0), Some(2.0)));
    // TIFF 本体の IFD0 から
    std::fs::write(&p, tiff_image(8, 8, 3)).unwrap();
    assert_eq!(extract_exif_info(&p).unwrap().orientation, 3);
}

#[test]
fn norm_timeは暦を見ず全角などの数字をasciiにする() {
    assert_eq!(norm_time("2024:05:06 07:08:09"), "2024-05-06 07:08");
    assert_eq!(norm_time("0000:00:00 00:00:00"), "0000-00-00 00:00");
    assert_eq!(norm_time("2024:13:45 25:61:00"), "2024-13-45 25:61");
    assert_eq!(norm_time(""), "");
    assert_eq!(norm_time("2024:05:06"), "");
    assert_eq!(norm_time(" 2024:05:06 07:08:09"), "");
    assert_eq!(
        norm_time("２０２４:０５:０６ ０７:０８:０９"),
        "2024-05-06 07:08"
    );
    assert_eq!(norm_time("٢٠٢٤:٠٥:٠٦ ٠٧:٠٨:٠٩"), "2024-05-06 07:08");
    // 数字でない文字は通さない（上付き数字・ローマ数字・漢数字）
    assert_eq!(norm_time("²⁰²⁴:05:06 07:08"), "");
    assert_eq!(norm_time("二〇二四:05:06 07:08"), "");
}

#[test]
fn libjpegが許す壊れ方のjpegは読める() {
    // Pillow（libjpeg）は、末尾のゴミ・マーカーの間のゴミ（APP0 の後ろなど）を警告だけで読む。
    // 厳格モードのままだと、これらの写真の derivative が永久にできなくなる。
    // ここで使う「SOI の直後のゴミ」は Pillow は JPEG と識別できず失敗するが、Rust は読む
    // （Python との突き合わせは tests/jpeg_eoi_python.rs）。
    // EOI が欠けてファイルがそこで終わるものは Pillow も失敗する。EOI の位置にゴミが続くものは、Pillow は読むが
    // 切れたファイルと見分けられないので Rust は失敗にする（tests/jpeg_eoi_python.rs、server.md §0）
    let data: Vec<u8> = (0..120 * 90)
        .flat_map(|i| [(i % 251) as u8, (i % 241) as u8, (i % 239) as u8])
        .collect();
    let jpeg = jpeg_encode(&data, 120, 90, jpeg_encoder::ColorType::Rgb, None);
    let trailing = [jpeg.clone(), vec![0u8; 64], b"trailer".to_vec()].concat();
    let mut between = jpeg[..2].to_vec();
    between.extend_from_slice(&[0, 0, 0, 0]);
    between.extend_from_slice(&jpeg[2..]);
    for (name, bytes) in [("末尾のゴミ", trailing), ("マーカーの間のゴミ", between)] {
        let out = render(&bytes, 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(out.info.output, (120, 90), "{name}");
    }
    // EOI だけが欠けた JPEG は失敗（切れたファイルとデータの上では見分けられない）
    let err = render(&jpeg[..jpeg.len() - 2], 0).err().unwrap();
    assert!(
        matches!(
            err,
            ImageError::Decode {
                class: DecodeClass::Truncated,
                ..
            }
        ),
        "{err}"
    );
}

/// 擬似乱数のノイズ（圧縮されにくく、データ部が十分に長くなる）
fn noise_rgb(w: u32, h: u32) -> Vec<u8> {
    let mut x = 0x2545_F491u32;
    (0..w * h * 3)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn どこで切れたjpegも失敗にする() {
    // 切断点を全て回す。1つでも成功すると、部分的な画像（下がグレーで埋まったもの）が「取得済み」になる。
    // 厳格モードのデコーダは、最後のMCU行の途中で切れていても成功させてしまう（末尾の約2%）ので、
    // 画像データのあとにEOIがあることも見ている（`jpeg_has_eoi`）
    for (w, h, subsampled) in [(64u32, 48u32, false), (70, 50, true)] {
        let data = noise_rgb(w, h);
        let mut out = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut out, 90);
        enc.set_sampling_factor(if subsampled {
            jpeg_encoder::SamplingFactor::F_2_2
        } else {
            jpeg_encoder::SamplingFactor::F_1_1
        });
        enc.encode(&data, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let scan_start = out.windows(2).position(|b| b == [0xFF, 0xDA]).unwrap();
        assert!(
            out.len() - scan_start > 3000,
            "データ部が短すぎる: {}",
            out.len() - scan_start
        );
        let mut accepted = Vec::new();
        for keep in scan_start + 14..out.len() {
            if render(&out[..keep], 0).is_ok() {
                accepted.push(keep);
            }
        }
        assert!(
            accepted.is_empty(),
            "切れたJPEGが成功した（{w}x{h}）: {} 箇所、最初は {:?}",
            accepted.len(),
            accepted.first()
        );
    }
}

#[test]
fn どこで切れたpngとtiffも失敗にする() {
    let data = noise_rgb(48, 40);
    let png = png_encode(&data, 48, 40, image::ExtendedColorType::Rgb8);
    // IEND（12 バイト）の手前まで切ると失敗。IEND だけが欠けたものは、画素は揃っているので成功してよい
    for keep in 8..png.len() - 12 {
        assert!(
            render(&png[..keep], 0).is_err(),
            "png keep={keep}/{}",
            png.len()
        );
    }
    let tif = tiff_image(48, 40, 1);
    for keep in 0..tif.len() {
        if render(&tif[..keep], 0).is_ok() {
            panic!("tiff keep={keep}/{} が成功した", tif.len());
        }
    }
}

#[test]
fn どこで切れたheicも失敗にする() {
    for name in [
        "synthetic_o1.heic",
        "synthetic_o6.heic",
        "synthetic_p3.heic",
    ] {
        let heic = fixture(name);
        for keep in 0..heic.len() {
            assert!(
                render(&heic[..keep], 0).is_err(),
                "{name} keep={keep}/{}",
                heic.len()
            );
        }
    }
}

#[test]
fn bigtiffは専用のエラーで断る() {
    // 合成: BigTIFF のヘッダ（`II+\0` / `MM\0+`、バイトサイズ 8、0、IFD0 のオフセット）だけ。Pillow は開けるが非対応
    for header in [
        &b"II+\0\x08\0\0\0\x10\0\0\0\0\0\0\0"[..],
        b"MM\0+\0\x08\0\0\0\0\0\0\0\0\0\x10",
    ] {
        assert!(matches!(render(header, 0), Err(ImageError::BigTiff)));
        assert!(matches!(
            exif_info_from_bytes(header),
            Err(ImageError::BigTiff)
        ));
        assert!(!Format::sniff(header).is_some());
    }
    // 通常の TIFF（`*`）は従来どおり
    assert!(render(&tiff_image(20, 10, 1), 0).is_ok());
    // 文面は定型で、画像でないものとは区別できる
    assert_eq!(
        ImageError::BigTiff.to_string(),
        "BigTIFFには対応していません"
    );
    assert!(matches!(
        render(b"PK\x03\x04", 0),
        Err(ImageError::NotAnImage)
    ));
}
