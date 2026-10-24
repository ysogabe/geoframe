//! 画像処理の技術PoC（T0.4）。
//!
//! 現行Pythonの`make_derivative`（`app/geoframe/util.py`）と同じ規則を、候補の部品を切り替えながら
//! 再現する: デコード → EXIF Orientation（と手動回転）で向き補正 → 長辺`max_px`へ縮小（小さい画像は
//! 拡大しない）→ JPEG出力。出力JPEGにEXIF（GPSを含む）は残さない。

use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};

use image::metadata::Orientation;
use image::{DynamicImage, RgbImage};

#[cfg(feature = "heic")]
pub mod heic;
pub mod heic_pure;
pub mod subprocess;

/// JPEGのデコーダ。`image`は内部でzune-jpegを使うので、速度差はラッパーの分だけになる見込み
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JpegDecoderKind {
    /// `image::codecs::jpeg::JpegDecoder`
    Image,
    /// `zune-jpeg`を直接
    Zune,
}

/// 縮小の実装とフィルタ
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resizer {
    Image(image::imageops::FilterType),
    Fir(fast_image_resize::FilterType),
}

/// JPEGエンコーダ
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderKind {
    /// `image::codecs::jpeg::JpegEncoder`
    Image,
    /// `jpeg-encoder`、クロマ4:2:0（Pillowの既定と同じ）
    Je420,
    /// `jpeg-encoder`、4:2:0、ハフマン表を最適化
    Je420Opt,
    /// `jpeg-encoder`、4:4:4（サブサンプリングなし）
    Je444,
}

/// 入力のICCプロファイルの扱い
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IccPolicy {
    /// 捨てる（現行Python）。広色域の画像は色が薄く見える
    Drop,
    /// 出力JPEGへそのまま埋め込む
    Keep,
    /// 画素をsRGBへ変換し、プロファイルは付けない
    Srgb,
}

/// 縮小と向き補正の順序
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// 向き補正→縮小（現行Pythonと同じ）
    OrientFirst,
    /// 縮小→向き補正（回転・転置を小さい画像で行う）
    ResizeFirst,
}

/// EXIF Orientationの読み取り元（JPEGのみ。HEICはlibheifが変換を適用済みなので使わない）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrientationSource {
    /// デコーダが返すもの（`image`の`orientation()`、zune直接ならEXIFチャンクを`image`の関数で解釈）
    Decoder,
    /// `kamadak-exif`でファイル全体から読む
    Kamadak,
}

/// HEICの読み方
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeicBackend {
    /// `libheif-rs`（featureの`heic`が要る）
    Libheif,
    /// 外部コマンド（`heif-convert`/`heif-dec`）でJPEGへ変換させ、それを読む
    Command(String),
    /// 純Rustの`zune-heic`（feature `heic-zune`）。向き・グリッド・ICC抽出に対応
    Zune,
    /// 純Rustの`heic-rs`（feature `heic-rs`）。ICCは取り出さない
    HeicRs,
    /// 純Rustの`heif-oxide`（feature `heic-oxide`）。出力は常にsRGB（ICCは取り出さない）
    Oxide,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub decoder: JpegDecoderKind,
    pub resizer: Resizer,
    pub encoder: EncoderKind,
    pub quality: u8,
    pub icc: IccPolicy,
    pub order: Order,
    pub orientation: OrientationSource,
    pub heic: HeicBackend,
    /// 長辺の上限（現行の`DERIV_PX`）
    pub max_px: u32,
    /// 手動回転（時計回り、0/90/180/270）。EXIFの向きを当てた後に回す
    pub rotation: u16,
    /// これを超える画素数の入力は拒否する（デコードの前に寸法で判定）
    pub max_input_pixels: u64,
    /// HEICのデコードに使うスレッド数（`None`ならライブラリの既定）
    pub heic_threads: Option<u32>,
}

// 既定値はPoCの比較の基準（現行Pythonと同じ順序、libheif）で、**推奨構成とは違う**。
// 推奨は`order: ResizeFirst`・`heic: Zune`（docs/rust-migration/image-poc.mdの§1・§6）。
// P2で本体へ移すときに、既定値を推奨に合わせる。
impl Default for Options {
    fn default() -> Self {
        Self {
            decoder: JpegDecoderKind::Image,
            resizer: Resizer::Fir(fast_image_resize::FilterType::CatmullRom),
            encoder: EncoderKind::Je420,
            quality: 85,
            icc: IccPolicy::Srgb,
            order: Order::OrientFirst,
            orientation: OrientationSource::Decoder,
            heic: HeicBackend::Libheif,
            max_px: 2560,
            rotation: 0,
            max_input_pixels: 200_000_000,
            heic_threads: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Jpeg,
    Heic,
}

/// 段階ごとの時間
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub decode: Duration,
    pub orient: Duration,
    pub resize: Duration,
    pub icc: Duration,
    pub encode: Duration,
    pub total: Duration,
}

#[derive(Debug)]
pub struct Output {
    pub jpeg: Vec<u8>,
    pub kind: InputKind,
    pub in_dims: (u32, u32),
    pub out_dims: (u32, u32),
    /// EXIFから読んだ向き（1〜8）。HEICでは常に1（libheifが適用済み）
    pub exif_orientation: u8,
    pub icc_in: bool,
    pub timings: Timings,
}

/// デコード結果。向きは未補正（JPEG）か適用済み（HEIC）
pub struct Decoded {
    pub rgb: RgbImage,
    pub icc: Option<Vec<u8>>,
    /// まだ適用していない向き
    pub orientation: Orientation,
}

pub fn sniff(bytes: &[u8]) -> Option<InputKind> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return Some(InputKind::Jpeg);
    }
    // ISO BMFF: size(4) "ftyp" brand(4)
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        if [
            &b"heic"[..],
            b"heix",
            b"hevc",
            b"hevx",
            b"heim",
            b"heis",
            b"mif1",
            b"msf1",
        ]
        .contains(&brand)
        {
            return Some(InputKind::Heic);
        }
    }
    None
}

fn orientation_number(o: Orientation) -> u8 {
    match o {
        Orientation::NoTransforms => 1,
        Orientation::FlipHorizontal => 2,
        Orientation::Rotate180 => 3,
        Orientation::FlipVertical => 4,
        Orientation::Rotate90FlipH => 5,
        Orientation::Rotate90 => 6,
        Orientation::Rotate270FlipH => 7,
        Orientation::Rotate270 => 8,
    }
}

fn check_pixels(w: u32, h: u32, limit: u64) -> Result<(), String> {
    if u64::from(w) * u64::from(h) > limit {
        Err(format!("入力が大きすぎます（{w}x{h}）"))
    } else {
        Ok(())
    }
}

/// kamadak-exifでJPEGファイル全体からOrientationを読む。無ければ1
fn kamadak_orientation(bytes: &[u8]) -> Orientation {
    exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .and_then(|v| u8::try_from(v).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms)
}

pub fn decode_jpeg(bytes: &[u8], opts: &Options) -> Result<Decoded, String> {
    use image::ImageDecoder;
    match opts.decoder {
        JpegDecoderKind::Image => {
            let mut dec = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))
                .map_err(|e| format!("JPEGを開けません: {e}"))?;
            let (w, h) = dec.dimensions();
            check_pixels(w, h, opts.max_input_pixels)?;
            let icc = dec.icc_profile().map_err(|e| e.to_string())?;
            let orientation = match opts.orientation {
                OrientationSource::Decoder => dec.orientation().map_err(|e| e.to_string())?,
                OrientationSource::Kamadak => kamadak_orientation(bytes),
            };
            let rgb = DynamicImage::from_decoder(dec)
                .map_err(|e| format!("JPEGをデコードできません: {e}"))?
                .into_rgb8();
            Ok(Decoded {
                rgb,
                icc,
                orientation,
            })
        }
        JpegDecoderKind::Zune => {
            use zune_core::bytestream::ZCursor;
            use zune_core::colorspace::ColorSpace;
            use zune_core::options::DecoderOptions;
            let zopts = DecoderOptions::default()
                .jpeg_set_out_colorspace(ColorSpace::RGB)
                .set_strict_mode(false);
            let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), zopts);
            dec.decode_headers()
                .map_err(|e| format!("JPEGを開けません: {e:?}"))?;
            let (w, h) = dec.dimensions().ok_or("寸法を読めません")?;
            check_pixels(w as u32, h as u32, opts.max_input_pixels)?;
            let icc = dec.icc_profile();
            let orientation = match opts.orientation {
                OrientationSource::Decoder => dec
                    .exif()
                    .and_then(|e| Orientation::from_exif_chunk(e))
                    .unwrap_or(Orientation::NoTransforms),
                OrientationSource::Kamadak => kamadak_orientation(bytes),
            };
            let pixels = dec
                .decode()
                .map_err(|e| format!("JPEGをデコードできません: {e:?}"))?;
            let rgb = RgbImage::from_raw(w as u32, h as u32, pixels)
                .ok_or("デコード結果のバッファ長が合いません")?;
            Ok(Decoded {
                rgb,
                icc,
                orientation,
            })
        }
    }
}

fn manual_rotation(deg: u16) -> Orientation {
    match deg % 360 {
        90 => Orientation::Rotate90,
        180 => Orientation::Rotate180,
        270 => Orientation::Rotate270,
        _ => Orientation::NoTransforms,
    }
}

fn apply(img: RgbImage, o: Orientation) -> RgbImage {
    if o == Orientation::NoTransforms {
        return img;
    }
    let mut d = DynamicImage::ImageRgb8(img);
    d.apply_orientation(o);
    d.into_rgb8()
}

/// 長辺を`max_px`に収めた寸法。小さい画像は拡大しない。
///
/// Pillowの`Image.thumbnail`（`(max_px, max_px)`を渡したとき）と同じ丸め: 縮める側の辺は、
/// 切り捨てと切り上げのうち縦横比の誤差が小さい方を採る（四捨五入と1px違うことがある）。
pub fn target_dims(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    if w <= max_px && h <= max_px {
        return (w, h);
    }
    let aspect = f64::from(w) / f64::from(h);
    let m = f64::from(max_px);
    // key: 候補nの縦横比の誤差
    let pick = |number: f64, key: &dyn Fn(f64) -> f64| -> u32 {
        let (f, c) = (number.floor(), number.ceil());
        (if key(f) <= key(c) { f } else { c }).max(1.0) as u32
    };
    if aspect >= 1.0 {
        // 横が長い（またはmax_px四方より横長）: 横をmax_pxにし、縦を決める
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

pub fn resize(img: RgbImage, dims: (u32, u32), resizer: Resizer) -> Result<RgbImage, String> {
    if dims == img.dimensions() {
        return Ok(img);
    }
    match resizer {
        Resizer::Image(filter) => Ok(image::imageops::resize(&img, dims.0, dims.1, filter)),
        Resizer::Fir(filter) => {
            use fast_image_resize::images::Image;
            use fast_image_resize::{PixelType, ResizeAlg, ResizeOptions, Resizer as Fir};
            let (w, h) = img.dimensions();
            let src = Image::from_vec_u8(w, h, img.into_raw(), PixelType::U8x3)
                .map_err(|e| e.to_string())?;
            let mut dst = Image::new(dims.0, dims.1, PixelType::U8x3);
            let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter));
            Fir::new()
                .resize(&src, &mut dst, &options)
                .map_err(|e| e.to_string())?;
            RgbImage::from_raw(dims.0, dims.1, dst.into_vec()).ok_or_else(|| "縮小結果".into())
        }
    }
}

/// 画素をsRGBへ変換する。失敗したときは`img`を変えない
fn to_srgb(img: &mut RgbImage, icc: &[u8]) -> Result<(), String> {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    let src = ColorProfile::new_from_slice(icc).map_err(|e| format!("ICCを読めません: {e:?}"))?;
    let dst = ColorProfile::new_srgb();
    let transform = src
        .create_transform_8bit(Layout::Rgb, &dst, Layout::Rgb, TransformOptions::default())
        .map_err(|e| format!("ICC変換を作れません: {e:?}"))?;
    let mut converted = vec![0u8; img.as_raw().len()];
    transform
        .transform(img.as_raw(), &mut converted)
        .map_err(|e| format!("ICC変換に失敗: {e:?}"))?;
    img.copy_from_slice(&converted);
    Ok(())
}

pub fn encode_jpeg(img: &RgbImage, icc: Option<&[u8]>, opts: &Options) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity((w as usize * h as usize) / 6);
    match opts.encoder {
        EncoderKind::Image => {
            use image::ImageEncoder;
            let mut enc =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, opts.quality);
            if let Some(icc) = icc {
                enc.set_icc_profile(icc.to_vec())
                    .map_err(|e| format!("{e}"))?;
            }
            enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())?;
        }
        EncoderKind::Je420 | EncoderKind::Je420Opt | EncoderKind::Je444 => {
            use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
            let mut enc = Encoder::new(&mut out, opts.quality);
            enc.set_sampling_factor(if opts.encoder == EncoderKind::Je444 {
                SamplingFactor::F_1_1
            } else {
                SamplingFactor::F_2_2
            });
            enc.set_optimized_huffman_tables(opts.encoder == EncoderKind::Je420Opt);
            if let Some(icc) = icc {
                enc.add_icc_profile(icc).map_err(|e| e.to_string())?;
            }
            let w16 = u16::try_from(w).map_err(|_| "幅が65535を超えます")?;
            let h16 = u16::try_from(h).map_err(|_| "高さが65535を超えます")?;
            enc.encode(img.as_raw(), w16, h16, ColorType::Rgb)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

/// 1枚を処理する。`path`は外部コマンドでHEICを読むときだけ使う（無ければバイト列から一時ファイルを作る）
pub fn process(bytes: &[u8], path: Option<&Path>, opts: &Options) -> Result<Output, String> {
    let t0 = Instant::now();
    let kind = sniff(bytes).ok_or("JPEGでもHEICでもありません")?;
    let mut timings = Timings::default();

    let t = Instant::now();
    let decoded = match kind {
        InputKind::Jpeg => decode_jpeg(bytes, opts)?,
        InputKind::Heic => match &opts.heic {
            #[cfg(feature = "heic")]
            HeicBackend::Libheif => heic::decode(bytes, opts.max_input_pixels, opts.heic_threads)?,
            #[cfg(not(feature = "heic"))]
            HeicBackend::Libheif => {
                return Err("このビルドにはHEIC（feature `heic`）が含まれていません".into());
            }
            HeicBackend::Command(cmd) => subprocess::decode(cmd, bytes, path, opts)?,
            HeicBackend::Zune => heic_pure::zune(bytes, opts)?,
            HeicBackend::HeicRs => heic_pure::heic_rs(bytes, opts)?,
            HeicBackend::Oxide => heic_pure::oxide(bytes, opts)?,
        },
    };
    timings.decode = t.elapsed();

    let in_dims = decoded.rgb.dimensions();
    let exif_orientation = orientation_number(decoded.orientation);
    let icc_in = decoded.icc.is_some();
    let icc = decoded.icc;

    // EXIFの向きを当て、続けて手動回転を当てる（Pythonと同じ）
    let orientation = decoded.orientation;
    let manual = manual_rotation(opts.rotation);
    let apply_all = |img: RgbImage| apply(apply(img, orientation), manual);
    let swapped = |o: Orientation| {
        matches!(
            o,
            Orientation::Rotate90
                | Orientation::Rotate270
                | Orientation::Rotate90FlipH
                | Orientation::Rotate270FlipH
        )
    };
    let swaps = swapped(orientation) ^ swapped(manual);

    let mut img = decoded.rgb;
    let (ow, oh) = if swaps {
        (in_dims.1, in_dims.0)
    } else {
        in_dims
    };
    let dims = target_dims(ow, oh, opts.max_px);
    match opts.order {
        Order::OrientFirst => {
            let t = Instant::now();
            img = apply_all(img);
            timings.orient = t.elapsed();
            let t = Instant::now();
            img = resize(img, dims, opts.resizer)?;
            timings.resize = t.elapsed();
        }
        Order::ResizeFirst => {
            let pre = if swaps { (dims.1, dims.0) } else { dims };
            let t = Instant::now();
            img = resize(img, pre, opts.resizer)?;
            timings.resize = t.elapsed();
            let t = Instant::now();
            img = apply_all(img);
            timings.orient = t.elapsed();
        }
    }

    let mut out_icc: Option<Vec<u8>> = None;
    if let Some(profile) = icc {
        match opts.icc {
            IccPolicy::Drop => {}
            IccPolicy::Keep => out_icc = Some(profile),
            IccPolicy::Srgb => {
                let t = Instant::now();
                // 読めない・壊れている・RGBでない（グレーなど）ICCは、警告なしで捨てて続ける（`Drop`と同じ）。
                // 画像の内容は変えず、失敗の中身（プロファイルの値）もログへ出さない
                let _ = to_srgb(&mut img, &profile);
                timings.icc = t.elapsed();
            }
        }
    }

    let t = Instant::now();
    let jpeg = encode_jpeg(&img, out_icc.as_deref(), opts)?;
    timings.encode = t.elapsed();
    timings.total = t0.elapsed();

    Ok(Output {
        jpeg,
        kind,
        in_dims,
        out_dims: img.dimensions(),
        exif_orientation,
        icc_in,
        timings,
    })
}

#[cfg(test)]
mod tests;
