//! 明るさとシャープネス（design.md 4.3）。旧 manifest の値（Pillow で計算していた）に合わせた定義を、Rust で作り直す。
//!
//! 入力は、JPEG をデコードした**縮小前のグレー（8 ビット 1 チャンネル）**。
//!
//! - `mean_brightness` … 縮小前のグレー全体の平均。小数 2 桁。
//! - `sharpness_laplacian_var` … **グレーにしてから**長辺 1024 以下に縮小し、3×3 の 4 近傍ラプラシアン
//!   （`[0,1,0; 1,-4,1; 0,1,0]`、scale 1、offset 128、0〜255 に丸め、縁の 1 画素は元の値）を掛けた応答の母分散。小数 2 桁。
//!
//! RGB を先に縮小してからグレーにする順序は、旧値との差が 100 倍ほど大きかったので採らない（実測。design.md 4.3 の
//! 初稿の「縮小 → グレー」とは逆）。
//!
//! 縮小は Pillow 11.1.0 の `Image.thumbnail((1024, 1024))`（BICUBIC、`reducing_gap=2.0`）の 8 ビット 1 チャンネルの
//! 経路を、`libImaging/Resample.c` と同じ手順で移植したもの（係数の範囲の決め方、22 ビットの固定小数点、横→縦の 2 パス、
//! パスごとの丸めと 0〜255 への丸め）。`fast_image_resize`（CatmullRom）も試したが、旧値との差が大きかったので使わない。
//!
//! `thumbnail` の `reducing_gap=2.0` による整数倍の縮小も移植している（[`reduce`]）。縮小の比（`元の大きさ / 縮小後 / 2`）が
//! 整数で 2 以上になる軸（長辺が 4096px 以上）では、先に `Image.reduce` と同じ箱形の平均で縮め、そのあとの畳み込みには
//! 縮めた後の画像と、整数倍で割った小数のままの範囲（box）を渡す。表示用 JPEG の長辺は 2560px なので、通常は入らない。

/// 縮小後の長辺の上限（`thumbnail((1024, 1024))`）。
pub const THUMBNAIL_MAX: u32 = 1024;

/// 旧 manifest の値（Pillow で計算していた）との差の許容幅。旧 manifest の値と今の表示用 JPEG が両方ある写真での実測
/// （手で回す `tests/old_values.rs`。写真と値はリポジトリに入れない）の分布から決めた。差の主な原因は、JPEG のデコーダ
/// （zune-jpeg と libjpeg-turbo）の IDCT・色変換の丸めの違い（グレーにした画素のごく一部が ±1〜2 ずれる）で、
/// 縮小と畳み込みは Pillow と画素単位で一致する（`tests/measure_fixtures.rs`）。
///
/// 明るさは絶対差。旧値・新値とも小数 2 桁に丸めているので、丸めの 1 桁分より少し広くとる。
pub const BRIGHTNESS_TOLERANCE: f64 = 0.02;
/// シャープネスは、絶対差が [`SHARPNESS_ABS_TOLERANCE`]（小数 2 桁の丸めの範囲）＋値の [`SHARPNESS_REL_TOLERANCE`] 倍以内。
/// 絶対差は小数 2 桁の丸めの範囲、相対差は実測の最大の 2 倍ほど（デコーダの差が、ラプラシアンで増えた分）。
pub const SHARPNESS_ABS_TOLERANCE: f64 = 0.05;
pub const SHARPNESS_REL_TOLERANCE: f64 = 3e-3;

/// 明るさが許容幅に収まるか。
pub fn brightness_within_tolerance(new: f64, old: f64) -> bool {
    (new - old).abs() <= BRIGHTNESS_TOLERANCE
}

/// シャープネスが許容幅に収まるか。
pub fn sharpness_within_tolerance(new: f64, old: f64) -> bool {
    (new - old).abs() <= SHARPNESS_ABS_TOLERANCE + SHARPNESS_REL_TOLERANCE * old.abs()
}

/// Pillow の 8 ビットの畳み込みの固定小数点のビット数（`PRECISION_BITS = 32 - 8 - 2`）。
const PRECISION_BITS: u32 = 22;

/// Pillow の `convert("L")`: `L = (R*19595 + G*38470 + B*7471 + 0x8000) >> 16`（ITU-R 601。切り上げ付きの固定小数点）。
pub fn rgb_to_gray(rgb: &[u8]) -> Vec<u8> {
    let (pixels, _) = rgb.as_chunks::<3>();
    pixels
        .iter()
        .map(|p| {
            let v = u32::from(p[0]) * 19_595
                + u32::from(p[1]) * 38_470
                + u32::from(p[2]) * 7_471
                + 0x8000;
            (v >> 16) as u8
        })
        .collect()
}

/// 小数 2 桁に丸める（Python の `round(x, 2)` と同じく、2 進数の値そのものを丸める）。
pub fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// 縮小前のグレー全体の平均（丸める前）。
pub fn mean(gray: &[u8]) -> f64 {
    if gray.is_empty() {
        return 0.0;
    }
    let sum: u64 = gray.iter().map(|&v| u64::from(v)).sum();
    sum as f64 / gray.len() as f64
}

/// 母分散（`ImageStat` と同じ式: `Σv² / n − mean²`。和は整数で持つ）。
pub fn population_variance(gray: &[u8]) -> f64 {
    if gray.is_empty() {
        return 0.0;
    }
    let n = gray.len() as f64;
    let sum: u64 = gray.iter().map(|&v| u64::from(v)).sum();
    let sum2: u64 = gray.iter().map(|&v| u64::from(v) * u64::from(v)).sum();
    let mean = sum as f64 / n;
    sum2 as f64 / n - mean * mean
}

/// `thumbnail((max, max))` の出力の大きさ（Pillow 11.1.0 の `preserve_aspect_ratio`）。縮小しないときは `None`。
pub fn thumbnail_size(width: u32, height: u32, max: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || (max >= width && max >= height) {
        return None;
    }
    let (x, y) = (f64::from(max), f64::from(max));
    let aspect = f64::from(width) / f64::from(height);
    // max(min(floor(number), ceil(number), key=key), 1)。同点なら先に書いた floor
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> u32 {
        let (lo, hi) = (number.floor(), number.ceil());
        let pick = if key(hi) < key(lo) { hi } else { lo };
        pick.max(1.0) as u32
    };
    let size = if x / y >= aspect {
        (round_aspect(y * aspect, &|n| (aspect - n / y).abs()), max)
    } else {
        (
            max,
            round_aspect(x / aspect, &|n| {
                if n == 0.0 {
                    0.0
                } else {
                    (aspect - x / n).abs()
                }
            }),
        )
    };
    (size != (width, height)).then_some(size)
}

fn bicubic(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// 1 次元の係数（Pillow の `precompute_coeffs` と `normalize_coeffs_8bpc`）。`in0..in1` は入力の中で縮める範囲（小数）。
/// 返すのは、1 出力画素あたりの係数の最大の個数 `ksize`、各画素の `(xmin, 個数)`、固定小数点の係数。
fn coefficients(
    in_size: usize,
    in0: f64,
    in1: f64,
    out_size: usize,
) -> (usize, Vec<(usize, usize)>, Vec<i32>) {
    let support_base = 2.0;
    let scale = (in1 - in0) / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = support_base * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut kk = vec![0i32; out_size * ksize];
    let mut weights = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = in0 + (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        // C の `(int)(v + 0.5)`（0 方向への切り捨て）。負なら 0 に丸められる
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = (((center + support + 0.5) as i64).max(0) as usize).min(in_size);
        let n = xmax.saturating_sub(xmin);
        let mut ww = 0.0;
        for (x, w) in weights.iter_mut().enumerate().take(n) {
            *w = bicubic((x as f64 + xmin as f64 - center + 0.5) * ss);
            ww += *w;
        }
        for (x, w) in weights.iter().enumerate().take(n) {
            let w = if ww != 0.0 { w / ww } else { *w };
            let scaled = w * f64::from(1u32 << PRECISION_BITS);
            // C の `(int)(±0.5 + v * 2^22)`（0 方向への切り捨て）
            kk[xx * ksize + x] = if w < 0.0 {
                (-0.5 + scaled) as i32
            } else {
                (0.5 + scaled) as i32
            };
        }
        bounds.push((xmin, n));
    }
    (ksize, bounds, kk)
}

fn clip8(v: i64) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Pillow の `ImagingResample`（BICUBIC、8 ビット 1 チャンネル）: 大きさが変わる方向だけ、横→縦の順に畳み込む。
/// `box_` は入力の中で縮める範囲 `(x0, y0, x1, y1)`（小数）。
fn resample(
    src: &[u8],
    width: usize,
    height: usize,
    out_w: usize,
    out_h: usize,
    box_: (f64, f64, f64, f64),
) -> Vec<u8> {
    debug_assert_eq!(src.len(), width * height);
    let (bx0, by0, bx1, by1) = box_;
    // Pillow: `xsize != imIn->xsize || box[0] || box[2] != xsize`（縦も同じ）
    let need_horizontal = out_w != width || bx0 != 0.0 || bx1 != out_w as f64;
    let need_vertical = out_h != height || by0 != 0.0 || by1 != out_h as f64;
    let mut cur: Vec<u8>;
    let mut cur_ref: &[u8] = src;
    if need_horizontal {
        let (ksize, bounds, kk) = coefficients(width, bx0, bx1, out_w);
        let mut out = vec![0u8; out_w * height];
        for y in 0..height {
            let row = &src[y * width..(y + 1) * width];
            for (xx, &(xmin, n)) in bounds.iter().enumerate() {
                let k = &kk[xx * ksize..xx * ksize + n];
                let mut ss = 1i64 << (PRECISION_BITS - 1);
                for (x, &kv) in k.iter().enumerate() {
                    ss += i64::from(row[xmin + x]) * i64::from(kv);
                }
                out[y * out_w + xx] = clip8(ss);
            }
        }
        cur = out;
        cur_ref = &cur;
    }
    if need_vertical {
        let (ksize, bounds, kk) = coefficients(height, by0, by1, out_h);
        let mut out = vec![0u8; out_w * out_h];
        for (yy, &(ymin, n)) in bounds.iter().enumerate() {
            let k = &kk[yy * ksize..yy * ksize + n];
            for xx in 0..out_w {
                let mut ss = 1i64 << (PRECISION_BITS - 1);
                for (y, &kv) in k.iter().enumerate() {
                    ss += i64::from(cur_ref[(ymin + y) * out_w + xx]) * i64::from(kv);
                }
                out[yy * out_w + xx] = clip8(ss);
            }
        }
        cur = out;
        cur_ref = &cur;
    }
    cur_ref.to_vec()
}

/// Pillow の `division_UINT32(divider, 8)`（`libImaging/Reduce.c`）: 平均を出す掛け算の係数。`2^32 / (256 * divider)` を
/// 単精度の浮動小数で割り、切り捨てる。
fn reduce_multiplier(divider: u32) -> u32 {
    let max_dividend = (1u32 << 8) * divider;
    let max_int = (1u32 << 30) as f32 * 4.0;
    (max_int / max_dividend as f32) as u32
}

/// Pillow の `Image.reduce((factor_x, factor_y))`（8 ビット 1 チャンネル、`libImaging/Reduce.c`）: `factor_x × factor_y`
/// の区画ごとの平均（`(合計 + 個数/2) * multiplier >> 24`。右端・下端の区画は端数の個数で平均する）。大きさは切り上げ。
pub fn reduce(
    src: &[u8],
    width: usize,
    height: usize,
    factor_x: usize,
    factor_y: usize,
) -> (Vec<u8>, usize, usize) {
    debug_assert_eq!(src.len(), width * height);
    let (out_w, out_h) = (width.div_ceil(factor_x), height.div_ceil(factor_y));
    let mut out = vec![0u8; out_w * out_h];
    for oy in 0..out_h {
        let y0 = oy * factor_y;
        let y1 = (y0 + factor_y).min(height);
        for ox in 0..out_w {
            let x0 = ox * factor_x;
            let x1 = (x0 + factor_x).min(width);
            let count = ((x1 - x0) * (y1 - y0)) as u32;
            let mut sum = count / 2;
            for y in y0..y1 {
                sum += src[y * width + x0..y * width + x1]
                    .iter()
                    .map(|&v| u32::from(v))
                    .sum::<u32>();
            }
            out[oy * out_w + ox] =
                ((u64::from(sum) * u64::from(reduce_multiplier(count))) >> 24) as u8;
        }
    }
    (out, out_w, out_h)
}

/// Pillow の `Image.resize(size, BICUBIC, box=None, reducing_gap=2.0)`（`thumbnail` が内部で呼ぶ形）の 8 ビット 1 チャンネル版。
/// 軸ごとの縮小の比 `int(元 / 先 / 2.0)` が 2 以上の軸があれば、先に [`reduce`] で縮め（比は 1 以下に丸められた軸では 1）、
/// 残りの畳み込みには、縮めた後の画像と `元の範囲 / 整数倍` の小数の範囲を渡す。
pub fn resize_bicubic(
    src: &[u8],
    width: usize,
    height: usize,
    out_w: usize,
    out_h: usize,
) -> Vec<u8> {
    const REDUCING_GAP: f64 = 2.0;
    let factor =
        |from: usize, to: usize| ((from as f64 / to as f64 / REDUCING_GAP) as usize).max(1);
    let (factor_x, factor_y) = (factor(width, out_w), factor(height, out_h));
    if factor_x > 1 || factor_y > 1 {
        // `_get_safe_box` は、全体の範囲を渡したとき全体に収まる（support を足しても画像の外は切られる）
        let (reduced, rw, rh) = reduce(src, width, height, factor_x, factor_y);
        // Pillow の C 側は box を単精度（`float box[4]`）で受け取る。割り算の結果を単精度に丸めてから使う
        let to_f32 = |v: f64| f64::from(v as f32);
        let box_ = (
            0.0,
            0.0,
            to_f32(width as f64 / factor_x as f64),
            to_f32(height as f64 / factor_y as f64),
        );
        return resample(&reduced, rw, rh, out_w, out_h, box_);
    }
    resample(
        src,
        width,
        height,
        out_w,
        out_h,
        (0.0, 0.0, width as f64, height as f64),
    )
}

/// 3×3 の 4 近傍ラプラシアン（scale 1、offset 128、0〜255 に丸め）。縁の 1 画素は元の値（Pillow の
/// `ImageFilter.Kernel`）。幅か高さが 3 未満なら、そのまま写す。
pub fn laplacian_response(gray: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = gray.to_vec();
    if width < 3 || height < 3 {
        return out;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let i = y * width + x;
            let sum = i32::from(gray[i - width])
                + i32::from(gray[i + width])
                + i32::from(gray[i - 1])
                + i32::from(gray[i + 1])
                - 4 * i32::from(gray[i]);
            out[i] = (128 + sum).clamp(0, 255) as u8;
        }
    }
    out
}

/// 1 枚の写真の計算結果（小数 2 桁に丸めたもの）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measures {
    pub mean_brightness: f64,
    pub sharpness_laplacian_var: f64,
}

/// 縮小後のグレー（`thumbnail` を掛けたもの）のシャープネス（丸める前）。
pub fn sharpness(gray: &[u8], width: usize, height: usize) -> f64 {
    match thumbnail_size(width as u32, height as u32, THUMBNAIL_MAX) {
        Some((tw, th)) => {
            let (tw, th) = (tw as usize, th as usize);
            let small = resize_bicubic(gray, width, height, tw, th);
            population_variance(&laplacian_response(&small, tw, th))
        }
        None => population_variance(&laplacian_response(gray, width, height)),
    }
}

/// 縮小前のグレー（`width × height`、1 画素 1 バイト）から、明るさとシャープネスを求める。
pub fn measure(gray: &[u8], width: usize, height: usize) -> Measures {
    Measures {
        mean_brightness: round2(mean(gray)),
        sharpness_laplacian_var: round2(sharpness(gray, width, height)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_conversion_uses_the_pillow_fixed_point_formula() {
        assert_eq!(rgb_to_gray(&[0, 0, 0, 255, 255, 255]), [0, 255]);
        // 純色: 19595・38470・7471 の係数（和は 65536）
        assert_eq!(
            rgb_to_gray(&[255, 0, 0, 0, 255, 0, 0, 0, 255]),
            [76, 150, 29]
        );
        // 切り上げ項（0x8000）で丸める: (1*19595 + 0x8000) >> 16 = 0、(2*19595+..) >> 16 = 1
        assert_eq!(rgb_to_gray(&[1, 0, 0, 2, 0, 0]), [0, 1]);
    }

    #[test]
    fn thumbnail_sizes_follow_preserve_aspect_ratio() {
        // 縮小しない（両方が 1024 以下）
        assert_eq!(thumbnail_size(1024, 1024, 1024), None);
        assert_eq!(thumbnail_size(800, 600, 1024), None);
        // 縮小
        assert_eq!(thumbnail_size(2560, 1920, 1024), Some((1024, 768)));
        assert_eq!(thumbnail_size(1920, 2560, 1024), Some((768, 1024)));
        assert_eq!(thumbnail_size(2560, 2560, 1024), Some((1024, 1024)));
        // 縦横比に合わせて floor・ceil のうち近い方を選ぶ（2560×1707 → 1024×683、2560×1703 → 1024×681）
        assert_eq!(thumbnail_size(2560, 1707, 1024), Some((1024, 683)));
        // 極端な縦横比でも 1 以上
        assert_eq!(thumbnail_size(60_000, 1, 1024), Some((1024, 1)));
        assert_eq!(thumbnail_size(1, 60_000, 1024), Some((1, 1024)));
        // 片方だけが 1024 を超える
        assert_eq!(thumbnail_size(1100, 100, 1024), Some((1024, 93)));
    }

    #[test]
    fn a_flat_image_stays_flat_through_the_resize() {
        let src = vec![77u8; 2560 * 1920];
        let out = resize_bicubic(&src, 2560, 1920, 1024, 768);
        assert!(out.iter().all(|&v| v == 77));
    }

    #[test]
    fn resize_changes_only_the_axes_that_change() {
        let src: Vec<u8> = (0..100 * 10).map(|i| (i % 251) as u8).collect();
        // 大きさが同じなら、そのまま
        assert_eq!(resize_bicubic(&src, 100, 10, 100, 10), src);
        // 横だけ縮める: 高さは保たれる
        let out = resize_bicubic(&src, 100, 10, 50, 10);
        assert_eq!(out.len(), 50 * 10);
    }

    #[test]
    fn laplacian_keeps_the_border_and_clamps() {
        // 平坦: 内側は 128、縁は元の値
        let flat = vec![10u8; 5 * 5];
        let r = laplacian_response(&flat, 5, 5);
        assert_eq!(r[0], 10);
        assert_eq!(r[12], 128);
        // 中央だけ明るい: 4*255 を引いて 0 に丸める。隣は 128 + 255 = 255 に丸める
        let mut spot = vec![0u8; 5 * 5];
        spot[12] = 255;
        let r = laplacian_response(&spot, 5, 5);
        assert_eq!(r[12], 0);
        assert_eq!(r[11], 255);
        assert_eq!(r[7], 255);
        assert_eq!(r[6], 128);
        // 3 未満なら写す
        assert_eq!(laplacian_response(&[1, 2, 3, 4], 2, 2), [1, 2, 3, 4]);
    }

    #[test]
    fn variance_is_the_population_variance() {
        assert_eq!(population_variance(&[0, 255]), 16256.25);
        assert_eq!(population_variance(&[7; 10]), 0.0);
        assert_eq!(mean(&[0, 255]), 127.5);
    }

    #[test]
    fn rounding_is_two_decimals() {
        assert_eq!(round2(70.214), 70.21);
        assert_eq!(round2(70.216), 70.22);
        assert_eq!(round2(2000.29), 2000.29);
        assert_eq!(round2(0.0), 0.0);
    }
}
