//! 合成した小さな画像だけで、向き補正・縮小規則・EXIFを残さないことを確かめる（写真は使わない）。

use std::io::Cursor;

use exif::experimental::Writer;
use exif::{Field, In, Rational, Tag, Value};
use image::{Rgb, RgbImage};

use super::*;

/// 左半分が赤、右半分が青の画像（向きの確認用）
fn two_tone(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 {
            Rgb([255, 0, 0])
        } else {
            Rgb([0, 0, 255])
        }
    })
}

fn jpeg_plain(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100)
        .encode_image(img)
        .unwrap();
    out
}

/// GPSと機種名を含むEXIF（APP1）を作る
fn exif_app1(orientation: u16) -> Vec<u8> {
    let fields = [
        Field {
            tag: Tag::Orientation,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![orientation]),
        },
        Field {
            tag: Tag::Make,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"SynthMaker".to_vec()]),
        },
        Field {
            tag: Tag::GPSLatitudeRef,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"N".to_vec()]),
        },
        Field {
            tag: Tag::GPSLatitude,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![
                Rational { num: 10, denom: 1 },
                Rational { num: 20, denom: 1 },
                Rational { num: 30, denom: 1 },
            ]),
        },
    ];
    let mut w = Writer::new();
    for f in &fields {
        w.push_field(f);
    }
    let mut tiff = Cursor::new(Vec::new());
    w.write(&mut tiff, false).unwrap();
    let tiff = tiff.into_inner();
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

/// SOIの直後へAPP1を差し込む
fn with_exif(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&exif_app1(orientation));
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn has_exif(bytes: &[u8]) -> bool {
    bytes.windows(6).any(|w| w == b"Exif\0\0")
        || exif::Reader::new()
            .read_from_container(&mut Cursor::new(bytes))
            .is_ok()
}

fn has_icc(bytes: &[u8]) -> bool {
    bytes.windows(12).any(|w| w == b"ICC_PROFILE\0")
}

fn px(bytes: &[u8], x: u32, y: u32) -> [u8; 3] {
    image::load_from_memory(bytes)
        .unwrap()
        .to_rgb8()
        .get_pixel(x, y)
        .0
}

fn reddish(p: [u8; 3]) -> bool {
    p[0] > 180 && p[2] < 80
}

fn bluish(p: [u8; 3]) -> bool {
    p[2] > 180 && p[0] < 80
}

/// 部品の全組み合わせ
fn all_options() -> Vec<Options> {
    let mut v = Vec::new();
    for decoder in [JpegDecoderKind::Image, JpegDecoderKind::Zune] {
        for resizer in [
            Resizer::Image(image::imageops::FilterType::Lanczos3),
            Resizer::Image(image::imageops::FilterType::Triangle),
            Resizer::Fir(fast_image_resize::FilterType::CatmullRom),
            Resizer::Fir(fast_image_resize::FilterType::Bilinear),
        ] {
            // Je420Optは含めない: jpeg-encoder 0.7.1の「4:2:0＋ハフマン最適化」の出力を
            // zune-jpeg 0.5.15（imageのデコーダ）が誤って読む（Pillowは正しく読む）ため、画素を検証する
            // テストの対象外にする。EXIFを残さないことだけは別に確かめる（下のテスト）
            for encoder in [EncoderKind::Image, EncoderKind::Je420, EncoderKind::Je444] {
                for order in [Order::OrientFirst, Order::ResizeFirst] {
                    for orientation in [OrientationSource::Decoder, OrientationSource::Kamadak] {
                        v.push(Options {
                            decoder,
                            resizer,
                            encoder,
                            order,
                            orientation,
                            ..Options::default()
                        });
                    }
                }
            }
        }
    }
    v
}

#[test]
fn orientationに応じて寸法と向きが変わる() {
    let src = two_tone(60, 40);
    for opts in all_options() {
        for (o, dims) in [
            (1u16, (60, 40)),
            (3, (60, 40)),
            (6, (40, 60)),
            (8, (40, 60)),
        ] {
            let out = process(&with_exif(&jpeg_plain(&src), o), None, &opts).unwrap();
            assert_eq!(out.in_dims, (60, 40), "{opts:?}");
            assert_eq!(out.out_dims, dims, "o={o} {opts:?}");
            assert_eq!(out.exif_orientation, o as u8);
            let (w, h) = dims;
            match o {
                // 無変換: 左が赤
                1 => {
                    assert!(
                        reddish(px(&out.jpeg, 5, h / 2)) && bluish(px(&out.jpeg, w - 5, h / 2))
                    );
                }
                // 180度: 左が青
                3 => {
                    assert!(
                        bluish(px(&out.jpeg, 5, h / 2)) && reddish(px(&out.jpeg, w - 5, h / 2))
                    );
                }
                // 時計回り90度: 元の左（赤）が上へ
                6 => {
                    assert!(
                        reddish(px(&out.jpeg, w / 2, 5)) && bluish(px(&out.jpeg, w / 2, h - 5))
                    );
                }
                // 反時計回り90度: 元の左（赤）が下へ
                8 => {
                    assert!(
                        bluish(px(&out.jpeg, w / 2, 5)) && reddish(px(&out.jpeg, w / 2, h - 5))
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn 手動回転はexifの向きの後に当たる() {
    let src = two_tone(60, 40);
    // Orientation 3（180度）の後に時計回り90度 = 元から見て時計回り270度（反時計回り90度）
    let opts = Options {
        rotation: 90,
        ..Options::default()
    };
    let out = process(&with_exif(&jpeg_plain(&src), 3), None, &opts).unwrap();
    assert_eq!(out.out_dims, (40, 60));
    assert!(bluish(px(&out.jpeg, 20, 5)) && reddish(px(&out.jpeg, 20, 55)));
}

#[test]
fn 長辺は2560に収まり小さい画像は拡大しない() {
    assert_eq!(target_dims(4032, 3024, 2560), (2560, 1920));
    assert_eq!(target_dims(3024, 4032, 2560), (1920, 2560));
    assert_eq!(target_dims(2600, 100, 2560), (2560, 98));
    assert_eq!(target_dims(5000, 1, 2560), (2560, 1));
    assert_eq!(target_dims(2560, 1000, 2560), (2560, 1000));
    assert_eq!(target_dims(300, 200, 2560), (300, 200));

    let big = jpeg_plain(&two_tone(2600, 100));
    let out = process(&big, None, &Options::default()).unwrap();
    assert_eq!(out.out_dims, (2560, 98));

    let small = jpeg_plain(&two_tone(50, 40));
    let out = process(&small, None, &Options::default()).unwrap();
    assert_eq!(out.out_dims, (50, 40));
}

#[test]
fn 縮小は向きを当てた後の寸法で行う() {
    // 縦長にして2560へ収める。resize-firstでも結果の寸法は同じ
    let src = jpeg_plain(&two_tone(2600, 100));
    for order in [Order::OrientFirst, Order::ResizeFirst] {
        let opts = Options {
            order,
            ..Options::default()
        };
        let out = process(&with_exif(&src, 6), None, &opts).unwrap();
        assert_eq!(out.out_dims, (98, 2560), "{order:?}");
    }
}

#[test]
fn 出力にexifとgpsを残さない() {
    let src = with_exif(&jpeg_plain(&two_tone(60, 40)), 6);
    assert!(has_exif(&src), "入力にはEXIFがあるはず");
    let mut all = all_options();
    all.push(Options {
        encoder: EncoderKind::Je420Opt,
        ..Options::default()
    });
    for opts in all {
        for icc in [IccPolicy::Drop, IccPolicy::Keep, IccPolicy::Srgb] {
            let opts = Options {
                icc,
                ..opts.clone()
            };
            let out = process(&src, None, &opts).unwrap();
            assert!(!has_exif(&out.jpeg), "{opts:?}");
            // 機種名・座標の文字列やバイト列も残っていない
            assert!(!out.jpeg.windows(10).any(|w| w == b"SynthMaker"));
        }
    }
}

#[test]
fn iccの扱い() {
    use moxcms::ColorProfile;
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    // 広色域で飽和した緑（P3の(0,255,0)はsRGBの外）
    let green = RgbImage::from_pixel(32, 32, Rgb([0, 255, 0]));
    let mut src = Vec::new();
    {
        use image::ImageEncoder;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut src, 100);
        enc.set_icc_profile(p3.clone()).unwrap();
        enc.write_image(green.as_raw(), 32, 32, image::ExtendedColorType::Rgb8)
            .unwrap();
    }
    for decoder in [JpegDecoderKind::Image, JpegDecoderKind::Zune] {
        for encoder in [EncoderKind::Image, EncoderKind::Je420] {
            let base = Options {
                decoder,
                encoder,
                ..Options::default()
            };
            let drop = process(
                &src,
                None,
                &Options {
                    icc: IccPolicy::Drop,
                    ..base.clone()
                },
            )
            .unwrap();
            assert!(drop.icc_in && !has_icc(&drop.jpeg));
            // 捨てると画素値はそのまま（色は薄く見える）
            assert!(px(&drop.jpeg, 16, 16)[1] > 240);

            let keep = process(
                &src,
                None,
                &Options {
                    icc: IccPolicy::Keep,
                    ..base.clone()
                },
            )
            .unwrap();
            assert!(has_icc(&keep.jpeg));

            let srgb = process(
                &src,
                None,
                &Options {
                    icc: IccPolicy::Srgb,
                    ..base.clone()
                },
            )
            .unwrap();
            assert!(!has_icc(&srgb.jpeg));
            // P3の純緑はsRGBでは範囲外なので、sRGBへ変換すると赤・青成分が0にクリップされ、緑は飽和のまま
            let c = px(&srgb.jpeg, 16, 16);
            assert!(c[1] > 240 && c[0] < 40 && c[2] < 60, "{c:?}");
        }
    }
    // P3の中間色（(128,128,128)の灰色は不変、(200,100,100)はsRGBで彩度が上がる）
    let mid = RgbImage::from_pixel(32, 32, Rgb([200, 100, 100]));
    let mut src2 = Vec::new();
    {
        use image::ImageEncoder;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut src2, 100);
        enc.set_icc_profile(p3).unwrap();
        enc.write_image(mid.as_raw(), 32, 32, image::ExtendedColorType::Rgb8)
            .unwrap();
    }
    let drop = px(
        &process(
            &src2,
            None,
            &Options {
                icc: IccPolicy::Drop,
                ..Options::default()
            },
        )
        .unwrap()
        .jpeg,
        16,
        16,
    );
    let srgb = px(
        &process(
            &src2,
            None,
            &Options {
                icc: IccPolicy::Srgb,
                ..Options::default()
            },
        )
        .unwrap()
        .jpeg,
        16,
        16,
    );
    assert!(
        srgb[0] > drop[0],
        "sRGBへ変換すると赤が強くなる: {drop:?} → {srgb:?}"
    );
}

#[test]
fn 壊れた入力と巨大な入力は拒否する() {
    assert!(process(b"not an image", None, &Options::default()).is_err());
    assert!(process(&[0xFF, 0xD8, 0xFF, 0xD9], None, &Options::default()).is_err());
    let opts = Options {
        max_input_pixels: 100,
        ..Options::default()
    };
    for decoder in [JpegDecoderKind::Image, JpegDecoderKind::Zune] {
        let err = process(
            &jpeg_plain(&two_tone(20, 20)),
            None,
            &Options {
                decoder,
                ..opts.clone()
            },
        )
        .unwrap_err();
        assert!(err.contains("大きすぎ"), "{err}");
    }
}

#[test]
fn heicの判定() {
    let mut ftyp = vec![0, 0, 0, 24];
    ftyp.extend_from_slice(b"ftypheic");
    ftyp.extend_from_slice(&[0; 12]);
    assert_eq!(sniff(&ftyp), Some(InputKind::Heic));
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF]), Some(InputKind::Jpeg));
    assert_eq!(sniff(b"GIF89a......"), None);
}
