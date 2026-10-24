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
//! 6. `.part` に書いて `rename`（読み手に書きかけを見せない）。失敗・パニックでも `.part` を残さない。
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
    /// 当てた EXIF の向き（1〜8。HEIC は常に 1）。
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

fn decode_jpeg(bytes: &[u8]) -> Result<Decoded, ImageError> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let err = |e: &zune_jpeg::errors::DecodeErrors| ImageError::Decode {
        format: "jpeg",
        class: jpeg_class(e),
    };
    // 幅・高さの既定の上限（16384）は外す。大きさは画素数（MAX_INPUT_PIXELS）で見る
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(65535)
        .set_max_height(65535);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    dec.decode_headers().map_err(|e| err(&e))?;
    let (w, h) = dec.dimensions().ok_or(ImageError::Decode {
        format: "jpeg",
        class: DecodeClass::Dimensions,
    })?;
    let (w, h) = (w as u32, h as u32);
    check_pixels(w, h)?;
    // グレーは 1 成分のまま、それ以外（YCbCr・CMYK・YCCK）は RGB に変換して受け取る
    let out = match dec.input_colorspace() {
        Some(ColorSpace::Luma) => ColorSpace::Luma,
        _ => ColorSpace::RGB,
    };
    let options = dec.options().jpeg_set_out_colorspace(out);
    dec.set_options(options);
    let icc = dec.icc_profile();
    let data = dec.decode().map_err(|e| err(&e))?;
    let channels = if out == ColorSpace::Luma { 1 } else { 3 };
    if data.len() != w as usize * h as usize * usize::from(channels) {
        return Err(ImageError::Decode {
            format: "jpeg",
            class: DecodeClass::Corrupt,
        });
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
    use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
    let w = u16::try_from(r.width).map_err(|_| ImageError::Encode)?;
    let h = u16::try_from(r.height).map_err(|_| ImageError::Encode)?;
    let mut out = Vec::with_capacity(r.data.len() / 8);
    let mut enc = Encoder::new(&mut out, JPEG_QUALITY);
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

/// バイト列から derivative の JPEG を作る。`rotation` は時計回りの角度（0・90・180・270。負・360 超は剰余）。
pub fn render(bytes: &[u8], rotation: i32) -> Result<Rendered, ImageError> {
    let rot = rotation.rem_euclid(360);
    if rot % 90 != 0 {
        return Err(ImageError::BadRotation);
    }
    let manual = manual_orientation(rot as u16);

    let format = Format::sniff(bytes).ok_or(ImageError::NotAnImage)?;
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
    let input = (decoded.raster.width, decoded.raster.height);
    // EXIF の向き（HEIC はデコーダが適用済みなので 1 のまま）
    if format != Format::Heic
        && let Some(tiff) = exif::exif_tiff(format, bytes, None)
    {
        decoded.orientation = exif::parse_tiff(&tiff).orientation;
    }
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

/// ファイルを読む（大きさの上限つき）。
fn read_limited(path: &Path) -> Result<Vec<u8>, ImageError> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    Ok(bytes)
}

/// 書きかけの `.part`。`commit` されないまま落とされたら（失敗・パニック）消す。
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

/// `src`（原本）から `dest` に derivative を書く。`.part` を経由し、失敗しても `.part` を残さない
/// （原本は呼び出し側が消す。EXIF 付きの原本を残さないのは呼び出し側の `finally` の役目）。
///
/// `dest` は[`DataDir::derivative_dest`]で検査した絶対パスを渡すこと。親ディレクトリは作る。
pub fn make_derivative(
    src: &Path,
    dest: &Path,
    rotation: i32,
) -> Result<DerivativeInfo, ImageError> {
    let bytes = read_limited(src)?;
    let rendered = render(&bytes, rotation)?;
    drop(bytes);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // Python は `dest.with_suffix(".part")`（最後の拡張子を置き換える。`x.r90.jpg` → `x.r90.part`）
    let mut part = PartFile {
        path: dest.with_extension("part"),
        committed: false,
    };
    {
        let mut file = fs::File::create(&part.path)?;
        file.write_all(&rendered.jpeg)?;
        // rename の前にディスクへ（電源断で空のファイルが derivative として残らないように）
        file.sync_all()?;
    }
    fs::rename(&part.path, dest)?;
    part.committed = true;
    Ok(rendered.info)
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
