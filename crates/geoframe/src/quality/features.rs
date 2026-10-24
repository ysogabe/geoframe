//! 1 枚の画像の特徴量（SHA-256・pHash・鮮明度・露出）。現行 `quality.py` の `analyze_path` に当たる。
//!
//! **同じ入力なら同じ結果**（乱数・時刻・並列の順序に依らない）。Python と画素まで一致させることはしない
//! （デコーダ・縮小の実装が違う）が、pHash は**同じ作り方**（グレー → 中央を正方形に切って 32x32 へ Lanczos →
//! DCT の左上 8x8 → 直流を除く 63 個の中央値より大きいか）にして、Python が保存した値と数ビットの差に収まるように
//! している（server.md §16.2 の計測）。特徴量の行を作った算法は行に残らないので、Python が作った行と Rust が
//! 作った行が混ざって比べられても、類似の判定（距離 12 以下）が崩れないための設計。

use std::f64::consts::PI;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::imaging::{ImageError, Luma, decode_luma, target_dims};

/// pHash の縮小サイズ。
const PHASH_SIZE: usize = 32;
/// DCT で使う左上の次数。
const PHASH_LOW: usize = 8;
/// 鮮明度・露出を測る画像の長辺。
const METRIC_PX: u32 = 512;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub sharpness: f64,
    pub luminance: f64,
    pub exposure_score: f64,
    pub black_clip: f64,
    pub white_clip: f64,
    pub contrast: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    /// 読んだファイルのバイト列の SHA-256（16 進、小文字）。`display` が指すファイルそのもの。
    pub sha256: String,
    pub phash: u64,
    pub metrics: Metrics,
}

/// ファイルのバイト列から特徴量を作る。
pub fn analyze_bytes(bytes: &[u8]) -> Result<Features, ImageError> {
    let sha256 = sha256_hex(bytes);
    let gray = decode_luma(bytes)?;
    Ok(Features {
        sha256,
        phash: perceptual_hash(&gray)?,
        metrics: image_metrics(&gray)?,
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// `src` の `crop`（左・上・幅・高さ。小数可）を `to` へ Lanczos3 で縮める（Pillow の `resize(box=)`）。
fn resize_luma(
    src: &Luma,
    crop: Option<(f64, f64, f64, f64)>,
    to: (u32, u32),
) -> Result<Vec<u8>, ImageError> {
    if crop.is_none() && (src.width, src.height) == to {
        return Ok(src.data.clone());
    }
    let image = ImageRef::new(src.width, src.height, &src.data, PixelType::U8)
        .map_err(|_| ImageError::Encode)?;
    let mut dst = Image::new(to.0, to.1, PixelType::U8);
    let mut options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    if let Some((left, top, width, height)) = crop {
        options = options.crop(left, top, width, height);
    }
    Resizer::new()
        .resize(&image, &mut dst, &options)
        .map_err(|_| ImageError::Encode)?;
    Ok(dst.into_vec())
}

/// `ImageOps.fit(image, (32, 32), LANCZOS)` の切り出し枠（中央、縦横比 1:1）。
fn fit_box(width: u32, height: u32) -> (f64, f64, f64, f64) {
    let (w, h) = (f64::from(width), f64::from(height));
    // 出力の縦横比は 1.0。live_size_ratio = w / h
    let ratio = w / h;
    let (crop_w, crop_h) = if ratio == 1.0 {
        (w, h)
    } else if ratio >= 1.0 {
        (h, h)
    } else {
        (w, w / 1.0)
    };
    ((w - crop_w) * 0.5, (h - crop_h) * 0.5, crop_w, crop_h)
}

fn cos_table() -> [[f64; PHASH_SIZE]; PHASH_LOW] {
    let mut table = [[0.0; PHASH_SIZE]; PHASH_LOW];
    for (u, row) in table.iter_mut().enumerate() {
        for (x, value) in row.iter_mut().enumerate() {
            *value = (PI * (2 * x + 1) as f64 * u as f64 / (2 * PHASH_SIZE) as f64).cos();
        }
    }
    table
}

/// 32x32 の DCT の左上 8x8 から作る 64 ビットの pHash（先頭の係数が最上位ビット）。
pub fn perceptual_hash(gray: &Luma) -> Result<u64, ImageError> {
    let crop = fit_box(gray.width, gray.height);
    let pixels = resize_luma(gray, Some(crop), (PHASH_SIZE as u32, PHASH_SIZE as u32))?;
    let cos = cos_table();
    // 横方向（x）の DCT を左上 8 列ぶんだけ
    let mut horizontal = [[0.0_f64; PHASH_LOW]; PHASH_SIZE];
    for (y, out) in horizontal.iter_mut().enumerate() {
        let row = &pixels[y * PHASH_SIZE..(y + 1) * PHASH_SIZE];
        for (u, value) in out.iter_mut().enumerate() {
            *value = row
                .iter()
                .zip(&cos[u])
                .fold(0.0, |sum, (&p, &c)| sum + f64::from(p) * c);
        }
    }
    let mut coeffs = [0.0_f64; PHASH_LOW * PHASH_LOW];
    for v in 0..PHASH_LOW {
        for u in 0..PHASH_LOW {
            coeffs[v * PHASH_LOW + u] =
                (0..PHASH_SIZE).fold(0.0, |sum, y| sum + horizontal[y][u] * cos[v][y]);
        }
    }
    // 直流（先頭）を除く 63 個の中央値
    let mut rest = coeffs[1..].to_vec();
    rest.sort_by(f64::total_cmp);
    let median = rest[rest.len() / 2];
    Ok(coeffs
        .iter()
        .fold(0_u64, |acc, &c| (acc << 1) | u64::from(c > median)))
}

/// ヒストグラムの累積が `total * fraction` に達する最初の階調。
fn percentile(histogram: &[u64; 256], fraction: f64, total: u64) -> u8 {
    let target = total as f64 * fraction;
    let mut current = 0_u64;
    for (index, &count) in histogram.iter().enumerate() {
        current += count;
        if current as f64 >= target {
            return index as u8;
        }
    }
    255
}

/// `FIND_EDGES`（3x3、中央 8・周り -1、0〜255 に収める）。外周 1 画素は元の値のまま（Pillow と同じ）。
fn find_edges(gray: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = gray.to_vec();
    if width < 3 || height < 3 {
        return out;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |dx: usize, dy: usize| i32::from(gray[(y + dy - 1) * width + (x + dx - 1)]);
            let sum = at(0, 0)
                + at(1, 0)
                + at(2, 0)
                + at(0, 1)
                + at(2, 1)
                + at(0, 2)
                + at(1, 2)
                + at(2, 2);
            out[y * width + x] = (8 * at(1, 1) - sum).clamp(0, 255) as u8;
        }
    }
    out
}

/// 鮮明度・輝度・露出・白飛び・黒つぶれ・コントラスト（現行の `image_metrics`）。
pub fn image_metrics(gray: &Luma) -> Result<Metrics, ImageError> {
    let (width, height) = (gray.width, gray.height);
    let work;
    let (data, w, h): (&[u8], usize, usize) = if width <= METRIC_PX && height <= METRIC_PX {
        (&gray.data, width as usize, height as usize)
    } else {
        let to = target_dims(width, height, METRIC_PX);
        work = resize_luma(gray, None, to)?;
        (&work, to.0 as usize, to.1 as usize)
    };
    let mut histogram = [0_u64; 256];
    for &p in data {
        histogram[usize::from(p)] += 1;
    }
    let total = data.len().max(1) as u64;
    let weighted: u64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &c)| i as u64 * c)
        .sum();
    let luminance = weighted as f64 / total as f64;
    let black_clip = histogram[..8].iter().sum::<u64>() as f64 / total as f64;
    let white_clip = histogram[248..].iter().sum::<u64>() as f64 / total as f64;
    let contrast = f64::from(percentile(&histogram, 0.95, total))
        - f64::from(percentile(&histogram, 0.05, total));

    let edge = find_edges(data, w, h);
    // 外周 4 画素を落とす（8 画素より大きいときだけ）
    let (edge, ew, eh) = if w > 8 && h > 8 {
        let mut cropped = Vec::with_capacity((w - 8) * (h - 8));
        for y in 4..h - 4 {
            cropped.extend_from_slice(&edge[y * w + 4..y * w + w - 4]);
        }
        (cropped, w - 8, h - 8)
    } else {
        (edge, w, h)
    };
    // `ImageStat.Stat(edge).var[0]`: (sum2 - sum^2 / count) / count
    let count = (ew * eh) as u64;
    let (mut sum, mut sum2) = (0_u64, 0_u64);
    for &p in &edge {
        sum += u64::from(p);
        sum2 += u64::from(p) * u64::from(p);
    }
    let variance = if count == 0 {
        0.0
    } else {
        (sum2 as f64 - (sum as f64).powi(2) / count as f64) / count as f64
    };
    let sharpness = variance.ln_1p();

    let center_score = 100.0 * (-((luminance - 128.0) / 100.0).powi(2)).exp();
    let clip_penalty = ((black_clip + white_clip) * 260.0).min(55.0);
    let low_contrast_penalty = (45.0 - contrast).max(0.0) * 0.45;
    let exposure_score = (center_score - clip_penalty - low_contrast_penalty).clamp(0.0, 100.0);
    Ok(Metrics {
        sharpness,
        luminance,
        exposure_score,
        black_clip,
        white_clip,
        contrast,
    })
}
