//! derivative（表示用の長辺 2560px の JPEG）の生成。現行 Python の `util.make_derivative` に当たる。
//!
//! # 手順（Python と同じ）
//!
//! 1. 読む: JPEG・PNG・TIFF・HEIC（内容で判別。拡張子は見ない）。
//! 2. EXIF の向きを当てる → 手動回転（`rotation`、時計回り）を当てる。HEIC は向きをデコーダが適用済みなので
//!    EXIF の Orientation は使わない（二重に回る）。
//! 3. 長辺を 2560px に収める（拡大しない。丸めは Pillow の `thumbnail` と同じ [`target_dims`]）。
//! 4. RGB か L（グレー）以外は RGB にする。
//! 5. JPEG（品質 85、4:2:0）。**EXIF（GPS を含む）は出力に残さない**（エンコーダに渡さない）。
//! 6. `.part` に書いて `rename`（読み手に書きかけを見せない）。失敗・パニックでも自分の `.part` を残さない。
//!    `.part` は排他的な新規作成（`O_EXCL`＋`O_NOFOLLOW`）で、既存のリンクを辿らない。
//!
//! # Python と違う点
//!
//! - ICC: Python は捨てる（広色域の写真は色が薄くなる）。Rust は **sRGB へ変換**して付けない。意図した変更。
//! - 縮小の順序: 縮小してから向きを当てる（結果は同じで速く、メモリも減る。image-poc.md §3.2）。
//! - 縮小のフィルタは CatmullRom（Pillow の既定 BICUBIC と同じ系統）。画素は一致しない（PSNR 約 48 dB）。
//! - 途中で切れた JPEG・PNG は失敗にする（Pillow と同じ。`image` は部分的な画像で成功するので使わない）。
//! - 16 ビットのグレー PNG は 8 ビットへ正しく縮める（Pillow は `I;16` を RGB にするとき飽和する）。
//! - 手動回転は 0・90・180・270 だけ（管理画面もその値しか作らない）。

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use image::metadata::Orientation;
use image::{DynamicImage, GrayImage, RgbImage};

use super::error::{DecodeClass, ImageError};
use super::exif::{self, Format};
use super::heic;
use crate::datadir::{DataDir, PathError, check_drive_id};

/// 長辺の上限（現行の `DERIV_PX`）。
pub const DERIV_PX: u32 = 2560;
/// JPEG の品質（Python の `quality=85`）。
pub const JPEG_QUALITY: u8 = 85;
/// 旅行ブログの書き出しの長辺（現行の `EXPORT_MAX_PX`）と JPEG の品質（`quality=88`）。
pub const EXPORT_PX: u32 = 1800;
pub const EXPORT_QUALITY: u8 = 88;
/// 入力の画素数の上限。Pillow は 2 倍（約 1.79 億画素）を超えると `DecompressionBombError`。
pub const MAX_INPUT_PIXELS: u64 = 178_956_970;
/// 入力ファイルの大きさの上限（まるごとメモリに読む）。
pub const MAX_INPUT_BYTES: u64 = 512 << 20;

/// ICC の扱いの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccOutcome {
    /// ICC が無い（または RGB でない）。
    None,
    /// sRGB へ変換した。
    ToSrgb,
    /// 読めない・壊れている・RGB 用でないので、変換せずに捨てた。
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivativeInfo {
    pub format: Format,
    pub input: (u32, u32),
    pub output: (u32, u32),
    /// 当てた向き（1〜8）。EXIF の Orientation で、タグが無ければ XMP の `tiff:Orientation`。HEIC は常に 1。
    pub exif_orientation: u8,
    /// グレースケール（L）のまま出した。
    pub gray: bool,
    pub icc: IccOutcome,
    /// 書き出した JPEG のバイト数。
    pub bytes: usize,
}

/// 1 枚分の生成結果（まだファイルにはしていない）。
pub struct Rendered {
    pub jpeg: Vec<u8>,
    pub info: DerivativeInfo,
}

/// derivative のファイル名。回したものは名前を変える（`/derivatives/` は内容不変として
/// ブラウザにキャッシュさせているので、同じ URL のまま中身を変えると古い向きが残る）。
///
/// `rotation` は**素の値**で名前にする（Python の `f"{id}.r{rotation}.jpg"` と同じ。0 は付けない）。
/// `drive_id` はファイル名に使える文字だけ（区切りを含められない）。
pub fn derivative_name(drive_id: &str, rotation: i32) -> Result<String, PathError> {
    check_drive_id(drive_id)?;
    Ok(if rotation == 0 {
        format!("{drive_id}.jpg")
    } else {
        format!("{drive_id}.r{rotation}.jpg")
    })
}

// ---------------------------------------------------------------------------
// 画素

/// 8 ビットの画素。`channels` は 1（L）か 3（RGB）。
struct Raster {
    width: u32,
    height: u32,
    channels: u8,
    data: Vec<u8>,
}

impl Raster {
    fn into_dynamic(self) -> Option<DynamicImage> {
        match self.channels {
            1 => GrayImage::from_raw(self.width, self.height, self.data)
                .map(DynamicImage::ImageLuma8),
            _ => {
                RgbImage::from_raw(self.width, self.height, self.data).map(DynamicImage::ImageRgb8)
            }
        }
    }

    fn from_dynamic(img: DynamicImage) -> Self {
        // L はそのまま。それ以外（RGB・アルファ付き・16 ビット）は RGB にする（Python の `convert("RGB")`）
        match img {
            DynamicImage::ImageLuma8(g) => Self {
                width: g.width(),
                height: g.height(),
                channels: 1,
                data: g.into_raw(),
            },
            other => {
                let rgb = other.into_rgb8();
                Self {
                    width: rgb.width(),
                    height: rgb.height(),
                    channels: 3,
                    data: rgb.into_raw(),
                }
            }
        }
    }
}

struct Decoded {
    raster: Raster,
    icc: Option<Vec<u8>>,
    /// まだ当てていない EXIF の向き（HEIC は 1）。
    orientation: u8,
}

fn check_pixels(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::Decode {
            format: "image",
            class: DecodeClass::Dimensions,
        });
    }
    if u64::from(width) * u64::from(height) > MAX_INPUT_PIXELS {
        return Err(ImageError::TooManyPixels { width, height });
    }
    Ok(())
}

fn jpeg_class(e: &zune_jpeg::errors::DecodeErrors) -> DecodeClass {
    use zune_jpeg::errors::DecodeErrors as E;
    match e {
        E::ExhaustedData => DecodeClass::Truncated,
        E::Unsupported(_) => DecodeClass::Unsupported,
        E::ZeroError | E::LargeDimensions(_) => DecodeClass::Dimensions,
        _ => DecodeClass::Corrupt,
    }
}

/// JPEG のデコードの失敗。`Structure` は「データは足りているがマーカーの並びが厳密でない」失敗で、
/// 寛容なモードで読み直してよい。それ以外（データが足りない・壊れている）は読み直さない。
enum JpegFailure {
    Structure,
    Final(ImageError),
}

/// 厳格モードの失敗のうち、**マーカーの並びだけが理由**のものか（読み直してよい）。
///
/// `zune-jpeg`の厳格モードのエラーは文字列で区別するしかない。許可リスト方式にして、
/// データが足りない・壊れている失敗（`Code length N  more than bits left M`＝ハフマン復号中にビットが尽きた、
/// `Premature end of buffer`、`ExhaustedData`、入出力）は**読み直さない**: 寛容なモードはそれを部分的な画像
/// （下がグレーで埋まったもの）で成功させてしまう。許可するのは次の3つ（`zune-jpeg` 0.5.15）:
/// - `[strict-mode]: Extra bytes between headers`（ヘッダの間の余分なバイト）
/// - `Marker .. found where not expected`／`Unexpected marker .. between scans`（予期しない位置のマーカー）
/// - `Unexpected marker .. at restart boundary`（リスタートの境のマーカー）
///
/// libjpegはどれも警告だけで読む。許可リストが広すぎないことは、全ての切断点で失敗になるテストが見る。
fn is_marker_error(e: &zune_jpeg::errors::DecodeErrors) -> bool {
    use zune_jpeg::errors::DecodeErrors as E;
    let msg: &str = match e {
        E::Format(m) | E::MCUError(m) => m,
        E::FormatStatic(m) => m,
        _ => return false,
    };
    msg.starts_with("[strict-mode]") || msg.contains("arker")
}

fn decode_jpeg_once(bytes: &[u8], strict: bool) -> Result<Decoded, JpegFailure> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::errors::DecodeErrors;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let fail = |e: &DecodeErrors| {
        if strict && is_marker_error(e) {
            JpegFailure::Structure
        } else {
            JpegFailure::Final(ImageError::Decode {
                format: "jpeg",
                class: jpeg_class(e),
            })
        }
    };
    // 幅・高さの既定の上限（16384）は外す。大きさは画素数（MAX_INPUT_PIXELS）で見る
    let options = DecoderOptions::default()
        .set_strict_mode(strict)
        .set_max_width(65535)
        .set_max_height(65535);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    dec.decode_headers().map_err(|e| fail(&e))?;
    let (w, h) = dec
        .dimensions()
        .ok_or(JpegFailure::Final(ImageError::Decode {
            format: "jpeg",
            class: DecodeClass::Dimensions,
        }))?;
    let (w, h) = (w as u32, h as u32);
    check_pixels(w, h).map_err(JpegFailure::Final)?;
    // グレーは 1 成分のまま、それ以外（YCbCr・CMYK・YCCK）は RGB に変換して受け取る
    let out = match dec.input_colorspace() {
        Some(ColorSpace::Luma) => ColorSpace::Luma,
        _ => ColorSpace::RGB,
    };
    let options = dec.options().jpeg_set_out_colorspace(out);
    dec.set_options(options);
    let icc = dec.icc_profile();
    let data = dec.decode().map_err(|e| fail(&e))?;
    let channels = if out == ColorSpace::Luma { 1 } else { 3 };
    if data.len() != w as usize * h as usize * usize::from(channels) {
        return Err(JpegFailure::Final(ImageError::Decode {
            format: "jpeg",
            class: DecodeClass::Corrupt,
        }));
    }
    Ok(Decoded {
        raster: Raster {
            width: w,
            height: h,
            channels,
            data,
        },
        icc,
        orientation: 1,
    })
}

/// JPEG をデコードする。
///
/// **途中で切れた JPEG は失敗にする**（Pillow は既定でエラー。`image` の `JpegDecoder` は寛容なモードで
/// 部分的な画像を成功として返すので使わない）ため、まず `zune-jpeg` の厳格モードで読む。ただし厳格モードは
/// マーカーの並びにも厳しく（例: ヘッダの間の余分なバイト）、libjpeg が警告だけで読む写真まで落とす。
/// そこで、**マーカーの並びだけが理由の失敗は、寛容なモードで読み直す**。データの不足・壊れは読み直さない。
fn decode_jpeg(bytes: &[u8]) -> Result<Decoded, ImageError> {
    let decoded = decode_jpeg_with_retry(bytes)?;
    // 厳格モードのデコーダも、最後の MCU 行の途中で切れた JPEG を成功させてしまう（検証: 切断点の約 2%）。
    // 画像データのあとに EOI が無ければ、切れたファイルとして失敗にする。Pillow は EOI の無い JPEG も
    // 読むが、切れたファイルと見分けられないので、見えないまま下がグレーのderivativeになるよりよい
    let has_eoi = super::exif::jpeg_scan_data(bytes)
        .is_none_or(|scan| scan.windows(2).any(|w| w == [0xFF, 0xD9]));
    if has_eoi {
        Ok(decoded)
    } else {
        Err(ImageError::Decode {
            format: "jpeg",
            class: DecodeClass::Truncated,
        })
    }
}

fn decode_jpeg_with_retry(bytes: &[u8]) -> Result<Decoded, ImageError> {
    match decode_jpeg_once(bytes, true) {
        Ok(d) => Ok(d),
        Err(JpegFailure::Final(e)) => Err(e),
        Err(JpegFailure::Structure) => decode_jpeg_once(bytes, false).map_err(|f| match f {
            JpegFailure::Final(e) => e,
            JpegFailure::Structure => ImageError::Decode {
                format: "jpeg",
                class: DecodeClass::Corrupt,
            },
        }),
    }
}

fn image_err(format: &'static str, e: &image::ImageError) -> ImageError {
    use image::ImageError as E;
    let class = match e {
        E::Unsupported(_) => DecodeClass::Unsupported,
        E::Limits(_) => DecodeClass::Dimensions,
        E::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => DecodeClass::Truncated,
        _ => DecodeClass::Corrupt,
    };
    ImageError::Decode { format, class }
}

fn decode_with_image(format: Format, bytes: &[u8]) -> Result<Decoded, ImageError> {
    use image::ImageDecoder;
    let name = if format == Format::Png { "png" } else { "tiff" };
    let cursor = Cursor::new(bytes);
    // 寸法と ICC を先に取る（デコーダは `from_decoder` に渡すと消費される）
    let (img, icc) = if format == Format::Png {
        let mut dec =
            image::codecs::png::PngDecoder::new(cursor).map_err(|e| image_err(name, &e))?;
        let (w, h) = dec.dimensions();
        check_pixels(w, h)?;
        let icc = dec.icc_profile().ok().flatten();
        (
            DynamicImage::from_decoder(dec).map_err(|e| image_err(name, &e))?,
            icc,
        )
    } else {
        let mut dec =
            image::codecs::tiff::TiffDecoder::new(cursor).map_err(|e| image_err(name, &e))?;
        let (w, h) = dec.dimensions();
        check_pixels(w, h)?;
        let icc = dec.icc_profile().ok().flatten();
        (
            DynamicImage::from_decoder(dec).map_err(|e| image_err(name, &e))?,
            icc,
        )
    };
    Ok(Decoded {
        raster: Raster::from_dynamic(img),
        icc,
        orientation: 1,
    })
}

// ---------------------------------------------------------------------------
// 向き・縮小・ICC・エンコード

fn orientation_of(n: u8) -> Orientation {
    Orientation::from_exif(n).unwrap_or(Orientation::NoTransforms)
}

fn manual_orientation(deg: u16) -> Orientation {
    match deg {
        90 => Orientation::Rotate90,
        180 => Orientation::Rotate180,
        270 => Orientation::Rotate270,
        _ => Orientation::NoTransforms,
    }
}

/// 縦横が入れ替わる向きか。
fn swaps_axes(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// 長辺を `max_px` に収めた寸法。小さい画像は拡大しない。
///
/// Pillow の `Image.thumbnail`（`(max_px, max_px)` を渡したとき）と同じ丸め: 縮める側と反対の辺は、
/// 切り捨てと切り上げのうち縦横比の誤差が小さい方を採る（四捨五入と 1px 違うことがある。同じ誤差なら切り捨て）。
pub fn target_dims(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    if w <= max_px && h <= max_px {
        return (w, h);
    }
    let aspect = f64::from(w) / f64::from(h);
    let m = f64::from(max_px);
    let pick = |number: f64, key: &dyn Fn(f64) -> f64| -> u32 {
        let (f, c) = (number.floor(), number.ceil());
        (if key(f) <= key(c) { f } else { c }).max(1.0) as u32
    };
    if aspect >= 1.0 {
        // 横長（または正方形）: 横を max_px にして縦を決める
        let y = pick(m / aspect, &|n| {
            if n == 0.0 {
                0.0
            } else {
                (aspect - m / n).abs()
            }
        });
        (max_px, y)
    } else {
        let x = pick(m * aspect, &|n| (aspect - n / m).abs());
        (x, max_px)
    }
}

fn resize(r: Raster, to: (u32, u32)) -> Result<Raster, ImageError> {
    use fast_image_resize::images::Image;
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    if (r.width, r.height) == to {
        return Ok(r);
    }
    let ty = if r.channels == 1 {
        PixelType::U8
    } else {
        PixelType::U8x3
    };
    let src = Image::from_vec_u8(r.width, r.height, r.data, ty).map_err(|_| ImageError::Encode)?;
    let mut dst = Image::new(to.0, to.1, ty);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    Resizer::new()
        .resize(&src, &mut dst, &options)
        .map_err(|_| ImageError::Encode)?;
    Ok(Raster {
        width: to.0,
        height: to.1,
        channels: r.channels,
        data: dst.into_vec(),
    })
}

fn orient(r: Raster, o: Orientation) -> Result<Raster, ImageError> {
    if o == Orientation::NoTransforms {
        return Ok(r);
    }
    // バッファ長はデコード・縮小で検査済みなので、`into_dynamic` が `None` を返すことは起きない
    let mut img = r.into_dynamic().ok_or(ImageError::Encode)?;
    img.apply_orientation(o);
    Ok(Raster::from_dynamic(img))
}

/// 画素を sRGB へ変換する（RGB のみ）。読めない・壊れている・RGB 用でない ICC は変えずに `false`。
fn to_srgb(r: &mut Raster, icc: &[u8]) -> bool {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    let Ok(src) = ColorProfile::new_from_slice(icc) else {
        return false;
    };
    let dst = ColorProfile::new_srgb();
    let Ok(transform) =
        src.create_transform_8bit(Layout::Rgb, &dst, Layout::Rgb, TransformOptions::default())
    else {
        return false;
    };
    let mut out = vec![0u8; r.data.len()];
    if transform.transform(&r.data, &mut out).is_err() {
        return false;
    }
    r.data = out;
    true
}

fn encode_jpeg(r: &Raster) -> Result<Vec<u8>, ImageError> {
    encode_jpeg_with(r, JPEG_QUALITY)
}

fn encode_jpeg_with(r: &Raster, quality: u8) -> Result<Vec<u8>, ImageError> {
    use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
    let w = u16::try_from(r.width).map_err(|_| ImageError::Encode)?;
    let h = u16::try_from(r.height).map_err(|_| ImageError::Encode)?;
    let mut out = Vec::with_capacity(r.data.len() / 8);
    let mut enc = Encoder::new(&mut out, quality);
    // 4:2:0（Pillow の既定と同じ）。ハフマン表の最適化は付けない（zune-jpeg が誤読する。image-poc.md §3.2）
    enc.set_sampling_factor(SamplingFactor::F_2_2);
    let color = if r.channels == 1 {
        ColorType::Luma
    } else {
        ColorType::Rgb
    };
    enc.encode(&r.data, w, h, color)
        .map_err(|_| ImageError::Encode)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// 公開 API

/// 内容で形式を判別してデコードする。`orientation` には EXIF の向き（EXIF に無ければ XMP の向き。Pillow の
/// `getexif` と同じ）を入れる。HEIC はデコーダが適用済みなので 1 のまま。
fn decode_any(bytes: &[u8]) -> Result<(Format, Decoded), ImageError> {
    let format = Format::sniff(bytes).ok_or_else(|| not_an_image(bytes))?;
    let mut decoded = match format {
        Format::Jpeg => decode_jpeg(bytes)?,
        Format::Png | Format::Tiff => decode_with_image(format, bytes)?,
        Format::Heic => {
            let img = heic::decode(bytes, MAX_INPUT_PIXELS)?;
            Decoded {
                raster: Raster {
                    width: img.width,
                    height: img.height,
                    channels: img.channels,
                    data: img.pixels,
                },
                icc: img.icc,
                orientation: 1,
            }
        }
    };
    if format != Format::Heic {
        let tiff = exif::exif_tiff(format, bytes, None);
        decoded.orientation = exif::effective_orientation(format, bytes, tiff.as_deref());
    }
    Ok((format, decoded))
}

/// 向きを当てた 8 ビットのグレー画像（品質解析の入力）。
pub struct Luma {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// 品質解析用にデコードする。Python の `ImageOps.exif_transpose(image).convert("L")` に当たる:
/// EXIF の向きを当て、RGB は Pillow と同じ整数式 `(R*19595 + G*38470 + B*7471 + 0x8000) >> 16` でグレーにする
/// （`image` crate の `to_luma8` は係数が違う）。ICC・アルファは使わない。
pub fn decode_luma(bytes: &[u8]) -> Result<Luma, ImageError> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let (_, decoded) = decode_any(bytes)?;
    let raster = decoded.raster;
    let gray = if raster.channels == 1 {
        raster
    } else {
        let data = raster
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| {
                ((u32::from(p[0]) * 19595
                    + u32::from(p[1]) * 38470
                    + u32::from(p[2]) * 7471
                    + 0x8000)
                    >> 16) as u8
            })
            .collect();
        Raster {
            width: raster.width,
            height: raster.height,
            channels: 1,
            data,
        }
    };
    let oriented = orient(gray, orientation_of(decoded.orientation))?;
    Ok(Luma {
        width: oriented.width,
        height: oriented.height,
        data: oriented.data,
    })
}

/// バイト列から derivative の JPEG を作る。`rotation` は時計回りの角度（0・90・180・270。負・360 超は剰余）。
pub fn render(bytes: &[u8], rotation: i32) -> Result<Rendered, ImageError> {
    // ファイルから読む経路は [`read_limited`] が見るが、バイト列を直接渡す呼び出しにも同じ上限をかける
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let rot = rotation.rem_euclid(360);
    if rot % 90 != 0 {
        return Err(ImageError::BadRotation);
    }
    let manual = manual_orientation(rot as u16);

    let (format, decoded) = decode_any(bytes)?;
    let input = (decoded.raster.width, decoded.raster.height);
    let exif_o = orientation_of(decoded.orientation);

    // 向きを当てたあとの寸法で長辺を収める。縮小してから向きを当てる（同じ結果で速い）
    let swapped = swaps_axes(exif_o) ^ swaps_axes(manual);
    let (ow, oh) = if swapped { (input.1, input.0) } else { input };
    let out_dims = target_dims(ow, oh, DERIV_PX);
    let pre = if swapped {
        (out_dims.1, out_dims.0)
    } else {
        out_dims
    };
    let mut raster = resize(decoded.raster, pre)?;
    raster = orient(orient(raster, exif_o)?, manual)?;

    let mut icc = IccOutcome::None;
    if let Some(profile) = decoded.icc
        && raster.channels == 3
    {
        icc = if to_srgb(&mut raster, &profile) {
            IccOutcome::ToSrgb
        } else {
            IccOutcome::Dropped
        };
    }
    let jpeg = encode_jpeg(&raster)?;
    let info = DerivativeInfo {
        format,
        input,
        output: (raster.width, raster.height),
        exif_orientation: decoded.orientation,
        gray: raster.channels == 1,
        icc,
        bytes: jpeg.len(),
    };
    Ok(Rendered { jpeg, info })
}

/// 既存の derivative を時計回りに `delta` 度回した JPEG にする（管理画面の回転。Python の
/// `im.rotate(-delta, expand=True).save(.., "JPEG", quality=85)`）。
///
/// [`render`] との違い: **EXIF の向きを当てない**（Pillow の `rotate` は当てない）、**縮小しない**（derivative は
/// 作ったときに 2560px に収まっている）、**ICC を変換せず付けない**（Pillow は付けない）。グレーはグレーのまま。
/// 出力に EXIF（GPS を含む）は付かない。`delta` は 90 の倍数（負・360 超は剰余）。
pub fn rotate_jpeg(bytes: &[u8], delta: i32) -> Result<Rendered, ImageError> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let rot = delta.rem_euclid(360);
    if rot % 90 != 0 {
        return Err(ImageError::BadRotation);
    }
    let (format, decoded) = decode_any(bytes)?;
    let input = (decoded.raster.width, decoded.raster.height);
    let raster = orient(decoded.raster, manual_orientation(rot as u16))?;
    let jpeg = encode_jpeg(&raster)?;
    let info = DerivativeInfo {
        format,
        input,
        output: (raster.width, raster.height),
        exif_orientation: decoded.orientation,
        gray: raster.channels == 1,
        icc: IccOutcome::None,
        bytes: jpeg.len(),
    };
    Ok(Rendered { jpeg, info })
}

/// 旅行ブログの書き出し用の JPEG にする（現行 `travel_blog._export_image`）。Pillow の
/// `exif_transpose` → `thumbnail((1800, 1800), LANCZOS)` → `convert("RGB")` → `save(.., "JPEG", quality=88)`
/// に当たる。[`render`] との違い:
///
/// - 長辺 [`EXPORT_PX`]・品質 [`EXPORT_QUALITY`]。
/// - **ICC を付けない・変換もしない**（Pillow は `icc_profile` を渡さないので落とすだけ。広色域の原本は色が
///   薄くなる。現行と同じ）。EXIF（GPS を含む）も付けない。
/// - **グレーも RGB（3 成分）で出す**（Pillow の `convert("RGB")`。derivative はグレーのまま）。
/// - ハフマン表の最適化は付けない（Pillow は `optimize=True`。`jpeg-encoder` の 4:2:0 との組み合わせを
///   `zune-jpeg` が誤読するため。ファイルが数 % 大きくなる）。
///
/// 入力の上限（ファイルの大きさ・画素数・HEIC の守り）は [`render`] と同じ経路を通る。
pub fn render_export(bytes: &[u8]) -> Result<Rendered, ImageError> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let (format, decoded) = decode_any(bytes)?;
    let input = (decoded.raster.width, decoded.raster.height);
    let exif_o = orientation_of(decoded.orientation);
    let swapped = swaps_axes(exif_o);
    let (ow, oh) = if swapped { (input.1, input.0) } else { input };
    let out_dims = target_dims(ow, oh, EXPORT_PX);
    let pre = if swapped {
        (out_dims.1, out_dims.0)
    } else {
        out_dims
    };
    let mut raster = orient(resize(decoded.raster, pre)?, exif_o)?;
    if raster.channels == 1 {
        raster = Raster {
            width: raster.width,
            height: raster.height,
            channels: 3,
            data: raster.data.iter().flat_map(|&v| [v, v, v]).collect(),
        };
    }
    let jpeg = encode_jpeg_with(&raster, EXPORT_QUALITY)?;
    let info = DerivativeInfo {
        format,
        input,
        output: (raster.width, raster.height),
        exif_orientation: decoded.orientation,
        gray: false,
        icc: IccOutcome::None,
        bytes: jpeg.len(),
    };
    Ok(Rendered { jpeg, info })
}

/// derivative のファイルを作る・差し替える・消す処理の直列化。回転（管理画面）と取得（`fetch_derivatives`）が
/// 同じ写真の derivative を同時に書き換えて、DB の `display` と実在するファイルが食い違わないようにする。
/// ファイルを書くところからその `display` を DB に書くところまで（または、読んだ `display` を DB で
/// 確かめて消すところまで）を、このロックを持ったまま行う。ロックの中で待つのは writer だけで、
/// writer の依頼はこのロックを取らない（デッドロックしない）。
pub fn lock_derivatives() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[cfg(test)]
    let _waiting = lock_waiters::Registered::new();
    let _counted = WaitingCount::new();
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// [`lock_derivatives`] を待っているスレッドの数（待ち始めから取れるまで）。
static WAITING_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct WaitingCount;

impl WaitingCount {
    fn new() -> Self {
        WAITING_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for WaitingCount {
    fn drop(&mut self) {
        WAITING_COUNT.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 試験用の観測: [`lock_derivatives`] で今ロックを待っているスレッドの数。結合試験（別のクレートとして
/// ビルドされるので、`cfg(test)` の [`lock_waiters`] は見えない）が、要求の処理が「ロックの前まで来て待っている」
/// ことを、実時間を待たずに合図で確かめるのに使う。本番の処理は使わない。
#[doc(hidden)]
pub fn lock_derivatives_waiting() -> usize {
    WAITING_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// 試験用: [`lock_derivatives`] の中でロックを待っているスレッドの記録。他のスレッドがロックを持っている間に
/// 「このスレッドはもうロックの前まで来て、待っている」ことを、実時間を待たずに確かめる。
#[cfg(test)]
pub(crate) mod lock_waiters {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::thread::ThreadId;
    use std::time::{Duration, Instant};

    static WAITING: Mutex<Option<HashSet<ThreadId>>> = Mutex::new(None);

    fn with<R>(f: impl FnOnce(&mut HashSet<ThreadId>) -> R) -> R {
        let mut guard = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.get_or_insert_with(HashSet::new))
    }

    /// 生きている間、呼んだスレッドを「待っている」に入れる（ロックを取れたら外れる）。
    pub(super) struct Registered(ThreadId);

    impl Registered {
        pub(super) fn new() -> Self {
            let id = std::thread::current().id();
            with(|set| set.insert(id));
            Self(id)
        }
    }

    impl Drop for Registered {
        fn drop(&mut self) {
            with(|set| set.remove(&self.0));
        }
    }

    /// `thread` が [`super::lock_derivatives`] でロックを待つ状態になるまで待つ。`gave_up()` が真になった
    /// （そのスレッドが待たずに終わった）か `limit` を過ぎたら false。**待つのは来るはずの出来事だけ**で、
    /// 来ないことを時間で推し量りはしない（`limit` は止まったときに試験を終わらせるための上限）。
    pub(crate) fn wait_until_waiting(
        thread: ThreadId,
        gave_up: impl Fn() -> bool,
        limit: Duration,
    ) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if with(|set| set.contains(&thread)) {
                return true;
            }
            if gave_up() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }
}

/// 種類を決められなかったバイト列の失敗: BigTIFF は専用のエラー、それ以外は画像でない。
pub(super) fn not_an_image(bytes: &[u8]) -> ImageError {
    if Format::is_bigtiff(bytes) {
        ImageError::BigTiff
    } else {
        ImageError::NotAnImage
    }
}

/// ファイルを開いて、大きさの上限（[`MAX_INPUT_BYTES`]）を超えていないことまで確かめる。中身は読まない。
/// 読む前の検査なので、書き出しの `dry_run`（読まない）と実際の読み込み（[`read_opened`]）で同じ答えになる。
pub fn open_limited(path: &Path) -> Result<(fs::File, u64), ImageError> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    Ok((file, len))
}

/// [`open_limited`] で開いたファイルを、大きさの上限つきで読み切る。
pub fn read_opened(file: fs::File, len: u64) -> Result<Vec<u8>, ImageError> {
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    Ok(bytes)
}

/// ファイルを読む（大きさの上限つき）。`img-probe` も使う。
pub fn read_limited(path: &Path) -> Result<Vec<u8>, ImageError> {
    let (file, len) = open_limited(path)?;
    read_opened(file, len)
}

/// 書きかけの `.part`。`commit` されないまま落とされたら（失敗・パニック）消す。**自分が作ったものだけ**を持つ
/// （[`create_part`] が新規作成に成功してから作る。既にあったファイル・リンクは消さない）。
struct PartFile {
    path: PathBuf,
    committed: bool,
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// `.part` の名前の候補を作る。`<dest の名前>.<pid>.<乱数>.part`（末尾は `.part` のまま）。
/// 名前を推測できないので、`DATA_DIR` に置かれたリンクに当たらない（当たっても [`create_part`] が拒否する）。
fn part_name(dest: &Path) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut random = [0u8; 8];
    // 乱数が取れない環境では 0 のまま（pid と、衝突時の作り直しに任せる）
    let _ = SystemRandom::new().fill(&mut random);
    let stem = dest
        .file_name()
        .map_or_else(|| "derivative".into(), |n| n.to_string_lossy().into_owned());
    format!(
        "{stem}.{}.{:016x}.part",
        std::process::id(),
        u64::from_be_bytes(random)
    )
}

/// 作り直す回数の上限（乱数の衝突、または同じ名前を置かれ続ける場合）。
const PART_ATTEMPTS: usize = 8;

/// `dest` と同じディレクトリに `.part` を**新規作成**して開く。
///
/// `O_CREAT|O_EXCL`（`create_new`）と `O_NOFOLLOW` で作るので、同じ名前のファイル・シンボリックリンク
/// （切れたリンクも）が既にあれば開けず（`AlreadyExists`）、リンクの先を書き換えない。名前を変えて
/// [`PART_ATTEMPTS`] 回まで作り直す。それでも作れなければ失敗にする。
fn create_part(
    dest: &Path,
    mut next_name: impl FnMut() -> String,
) -> Result<(fs::File, PartFile), ImageError> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let mut last = std::io::ErrorKind::AlreadyExists;
    for _ in 0..PART_ATTEMPTS {
        let path = dir.join(next_name());
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => {
                return Ok((
                    file,
                    PartFile {
                        path,
                        committed: false,
                    },
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e.kind(),
            Err(e) => return Err(e.into()),
        }
    }
    Err(ImageError::Io(last))
}

/// `src`（原本）から `dest` に derivative を書く。`.part` を経由し、失敗しても自分の `.part` を残さない
/// （原本は呼び出し側が消す。EXIF 付きの原本を残さないのは呼び出し側の `finally` の役目）。
///
/// `dest` は[`DataDir::derivative_dest`]で検査した絶対パスを渡すこと。親ディレクトリは作る。
/// `.part` は新規作成だけで（[`create_part`]）既存のリンクを辿らず、最後の `rename` は `dest` が
/// リンクでもそのリンク自体を置き換える（先は書き換えない）。
pub fn make_derivative(
    src: &Path,
    dest: &Path,
    rotation: i32,
) -> Result<DerivativeInfo, ImageError> {
    let bytes = read_limited(src)?;
    let rendered = render(&bytes, rotation)?;
    drop(bytes);
    write_atomic(dest, &rendered.jpeg)?;
    Ok(rendered.info)
}

/// `dest` に `jpeg` を書く。`.part` を排他的に新規作成して書き、`fsync` してから `rename` する（読み手に
/// 書きかけを見せない）。失敗しても自分の `.part` は残さず、`dest` は元のまま。親ディレクトリは作る。
pub fn write_atomic(dest: &Path, jpeg: &[u8]) -> Result<(), ImageError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let (mut file, mut part) = create_part(dest, || part_name(dest))?;
    file.write_all(jpeg)?;
    // rename の前にディスクへ（電源断で空のファイルが derivative として残らないように）
    file.sync_all()?;
    drop(file);
    fs::rename(&part.path, dest)?;
    part.committed = true;
    Ok(())
}

/// 取り込みの結果: DB の `display` に入れる相対パスと生成の内容。
#[derive(Debug)]
pub struct Stored {
    /// `derivatives/<名前>`（DATA_DIR からの相対パス）
    pub display: String,
    pub info: DerivativeInfo,
}

/// `DATA_DIR/derivatives/<name>` に derivative を作る。`name` は [`derivative_name`] などで作った名前。
/// 保存先は[`DataDir`]が検査する（名前の規則・`derivatives/` がリンクでないこと）。
pub fn store_derivative(
    data: &DataDir,
    src: &Path,
    name: &str,
    rotation: i32,
) -> Result<Stored, ImageError> {
    let (dest, display) = data.derivative_dest(name)?;
    let info = make_derivative(src, &dest, rotation)?;
    Ok(Stored { display, info })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::testutil::TempDir;

    fn jpeg_8x8() -> Vec<u8> {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(&[128; 8 * 8 * 3], 8, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn 大きすぎる入力は読まずに断る() {
        let tmp = TempDir::new("read-limit");
        let big = tmp.path().join("big.jpg");
        // 疎なファイル（中身は読まれない）
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_INPUT_BYTES + 1)
            .unwrap();
        assert!(matches!(read_limited(&big), Err(ImageError::FileTooLarge)));
        // ちょうど上限なら読む（画像ではないので別の失敗になる）
        let ok = tmp.path().join("ok.bin");
        fs::File::create(&ok).unwrap().set_len(1 << 20).unwrap();
        assert_eq!(read_limited(&ok).unwrap().len(), 1 << 20);
        // バイト列を直接渡す経路にも同じ上限がかかる
        let bytes = vec![0u8; usize::try_from(MAX_INPUT_BYTES).unwrap() + 1];
        assert!(matches!(render(&bytes, 0), Err(ImageError::FileTooLarge)));
        // `img-probe`（--out なし）も読む前に断る
        let args = crate::imaging::probe::ProbeArgs {
            files: vec![big],
            jobs: 1,
            ..Default::default()
        };
        let _ = crate::imaging::probe::run(&args);
    }

    #[test]
    fn partの名前は_partで終わり毎回変わる() {
        let dest = Path::new("/x/derivatives/abc.r90.jpg");
        let a = part_name(dest);
        assert!(a.starts_with("abc.r90.jpg.") && a.ends_with(".part"), "{a}");
        assert_ne!(a, part_name(dest));
        assert!(!a.contains('/'));
    }

    #[test]
    fn 既存のリンクには作らず別の名前で作り直す() {
        let tmp = TempDir::new("part-link");
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"keep me").unwrap();
        let dir = tmp.path().join("derivatives");
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("safe.jpg");
        // 生きているリンクと、切れたリンク（先がまだ無い）を置く
        symlink(&outside, dir.join("first.part")).unwrap();
        symlink(
            tmp.path().join("created-by-attacker"),
            dir.join("second.part"),
        )
        .unwrap();
        let mut candidates = ["first.part", "second.part", "third.part"].into_iter();
        let (mut file, part) =
            create_part(&dest, || candidates.next().unwrap().to_owned()).unwrap();
        file.write_all(b"jpeg").unwrap();
        drop(file);
        assert_eq!(part.path, dir.join("third.part"));
        // リンクの先は書き換わらず、切れたリンクの先も作られていない
        assert_eq!(fs::read(&outside).unwrap(), b"keep me");
        assert!(!tmp.path().join("created-by-attacker").exists());
        drop(part); // 未確定なので自分の `third.part` だけ消える
        assert_eq!(names(&dir), ["first.part", "second.part"]);
        assert!(
            fs::symlink_metadata(dir.join("first.part"))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn 名前がすべて塞がれていれば失敗にしてほかを消さない() {
        let tmp = TempDir::new("part-blocked");
        let dir = tmp.path().join("derivatives");
        fs::create_dir(&dir).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"keep me").unwrap();
        symlink(&outside, dir.join("same.part")).unwrap();
        let dest = dir.join("safe.jpg");
        let err = create_part(&dest, || "same.part".to_owned()).err().unwrap();
        assert!(matches!(
            err,
            ImageError::Io(std::io::ErrorKind::AlreadyExists)
        ));
        assert_eq!(fs::read(&outside).unwrap(), b"keep me");
        assert_eq!(names(&dir), ["same.part"]);
    }

    #[test]
    fn 従来の名前に置かれたリンクがあっても外を書き換えない() {
        let tmp = TempDir::new("part-legacy");
        let src = tmp.path().join("src.jpg");
        fs::write(&src, jpeg_8x8()).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"keep me").unwrap();
        let dir = tmp.path().join("derivatives");
        fs::create_dir(&dir).unwrap();
        // Python と同じ名前（`dest.with_extension("part")`）に外向きのリンク
        symlink(&outside, dir.join("safe.part")).unwrap();
        let dest = dir.join("safe.jpg");
        make_derivative(&src, &dest, 0).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"keep me");
        // 出力は通常のファイルで、リンクではない
        let meta = fs::symlink_metadata(&dest).unwrap();
        assert!(meta.is_file() && !meta.is_symlink());
        // リンクは自分のものではないので触らない
        assert_eq!(names(&dir), ["safe.jpg", "safe.part"]);
    }

    #[test]
    fn 出力先がリンクならリンク自体を置き換え先は書き換えない() {
        let tmp = TempDir::new("part-dest-link");
        let src = tmp.path().join("src.jpg");
        fs::write(&src, jpeg_8x8()).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"keep me").unwrap();
        let dir = tmp.path().join("derivatives");
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("safe.jpg");
        symlink(&outside, &dest).unwrap();
        make_derivative(&src, &dest, 0).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"keep me");
        let meta = fs::symlink_metadata(&dest).unwrap();
        assert!(meta.is_file() && !meta.is_symlink());
        assert_eq!(names(&dir), ["safe.jpg"]);
    }
}
