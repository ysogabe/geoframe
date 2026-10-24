//! `libheif-rs`でHEICを読む（システムのlibheifへ動的リンク。featureの`heic`）。
//!
//! libheifはirot（回転）・imir（反転）・clap（切り抜き）をデコード時に適用する。EXIFのOrientationを
//! さらに当てると二重に回るので、HEICでは向きを当てない（`Decoded::orientation`は常に無変換）。

use image::RgbImage;
use image::metadata::Orientation;
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

use crate::Decoded;

pub fn decode(bytes: &[u8], max_pixels: u64, threads: Option<u32>) -> Result<Decoded, String> {
    let lib = LibHeif::new();
    let mut ctx =
        HeifContext::read_from_bytes(bytes).map_err(|e| format!("HEICを開けません: {e}"))?;
    if let Some(n) = threads {
        ctx.set_max_decoding_threads(n);
    }
    let handle = ctx
        .primary_image_handle()
        .map_err(|e| format!("主画像を取得できません: {e}"))?;
    let (w, h) = (handle.width(), handle.height());
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(format!("入力が大きすぎます（{w}x{h}）"));
    }
    let icc = handle
        .color_profile_raw()
        .map(|p| p.data)
        .filter(|d| !d.is_empty());
    // 8bit RGBへ。10bit・HDRの画像もlibheifが8bitへ落とす（ゲインマップは使わない）
    let img = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
        .map_err(|e| format!("HEICをデコードできません: {e}"))?;
    let plane = img
        .planes()
        .interleaved
        .ok_or("インターリーブのプレーンがありません")?;
    let (pw, ph) = (plane.width as usize, plane.height as usize);
    let mut buf = Vec::with_capacity(pw * ph * 3);
    for row in 0..ph {
        let start = row * plane.stride;
        buf.extend_from_slice(&plane.data[start..start + pw * 3]);
    }
    let rgb = RgbImage::from_raw(pw as u32, ph as u32, buf).ok_or("バッファ長が合いません")?;
    Ok(Decoded {
        rgb,
        icc,
        orientation: Orientation::NoTransforms,
    })
}
