//! 純RustのHEICデコーダ候補（`zune-heic`・`heic-rs`・`heif-oxide`）。
//!
//! いずれもirot/imir/clapをデコード時に適用するので、向きは当てない。`zune-heic`だけICCを取り出せる。

use crate::{Decoded, Options};

#[cfg(not(feature = "heic-zune"))]
pub fn zune(_: &[u8], _: &Options) -> Result<Decoded, String> {
    Err("このビルドにはfeature `heic-zune`が含まれていません".into())
}

#[cfg(not(feature = "heic-rs"))]
pub fn heic_rs(_: &[u8], _: &Options) -> Result<Decoded, String> {
    Err("このビルドにはfeature `heic-rs`が含まれていません".into())
}

#[cfg(not(feature = "heic-oxide"))]
pub fn oxide(_: &[u8], _: &Options) -> Result<Decoded, String> {
    Err("このビルドにはfeature `heic-oxide`が含まれていません".into())
}

#[cfg(any(feature = "heic-zune", feature = "heic-rs", feature = "heic-oxide"))]
fn check(w: usize, h: usize, limit: u64) -> Result<(), String> {
    if (w as u64) * (h as u64) > limit {
        Err(format!("入力が大きすぎます（{w}x{h}）"))
    } else {
        Ok(())
    }
}

#[cfg(feature = "heic-zune")]
pub fn zune(bytes: &[u8], opts: &Options) -> Result<Decoded, String> {
    use image::RgbImage;
    use image::metadata::Orientation;
    use zune_heic::HeifDecoder;
    use zune_heic::zune_core::bytestream::ZCursor;
    use zune_heic::zune_core::options::DecoderOptions;

    // 既定のmdat上限（16 MiB）は、ノイズの多い48MPのHEIC（約28 MB）を拒否する。上限は明示する
    let mut zopts = DecoderOptions::default().set_hevc_max_mdat_size(128 << 20);
    if let Some(n) = opts.heic_threads {
        zopts = zopts.set_num_threads(u8::try_from(n).unwrap_or(u8::MAX));
    }
    let mut dec = HeifDecoder::new_with_options(ZCursor::new(bytes), zopts);
    dec.decode_headers()
        .map_err(|e| format!("HEICを開けません: {e:?}"))?;
    let (w, h) = (
        dec.width().ok_or("幅を読めません")?,
        dec.height().ok_or("高さを読めません")?,
    );
    check(w, h, opts.max_input_pixels)?;
    let pixels = dec
        .decode()
        .map_err(|e| format!("HEICをデコードできません: {e:?}"))?;
    let (w, h) = (
        dec.width().ok_or("幅を読めません")?,
        dec.height().ok_or("高さを読めません")?,
    );
    let comps = dec
        .colorspace()
        .ok_or("色空間を読めません")?
        .num_components();
    let rgb = match comps {
        3 => pixels,
        // アルファ付き・グレーは3チャンネルへ落とす
        4 => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        1 => pixels.iter().flat_map(|&g| [g, g, g]).collect(),
        2 => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        n => return Err(format!("未対応のチャンネル数: {n}")),
    };
    let icc = dec.icc_data().cloned().filter(|d| !d.is_empty());
    let rgb = RgbImage::from_raw(w as u32, h as u32, rgb).ok_or("バッファ長が合いません")?;
    Ok(Decoded {
        rgb,
        icc,
        orientation: Orientation::NoTransforms,
    })
}

#[cfg(feature = "heic-rs")]
pub fn heic_rs(bytes: &[u8], opts: &Options) -> Result<Decoded, String> {
    use image::RgbImage;
    use image::metadata::Orientation;
    let options = heic_rs::DecodeOptions {
        layout: heic_rs::PixelLayout::Rgb8,
        max_pixels: Some(opts.max_input_pixels),
        decode_alpha: false,
        threads: opts.heic_threads.map(|n| n as usize),
        ..heic_rs::DecodeOptions::default()
    };
    let img =
        heic_rs::decode(bytes, &options).map_err(|e| format!("HEICをデコードできません: {e}"))?;
    check(
        img.width as usize,
        img.height as usize,
        opts.max_input_pixels,
    )?;
    let rgb =
        RgbImage::from_raw(img.width, img.height, img.data).ok_or("バッファ長が合いません")?;
    Ok(Decoded {
        rgb,
        icc: None,
        orientation: Orientation::NoTransforms,
    })
}

#[cfg(feature = "heic-oxide")]
pub fn oxide(bytes: &[u8], opts: &Options) -> Result<Decoded, String> {
    use image::RgbImage;
    use image::metadata::Orientation;
    let img =
        heif_oxide::decode_bytes(bytes).map_err(|e| format!("HEICをデコードできません: {e}"))?;
    check(
        img.width as usize,
        img.height as usize,
        opts.max_input_pixels,
    )?;
    let rgba = img.to_rgba8();
    let rgb: Vec<u8> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let rgb = RgbImage::from_raw(img.width, img.height, rgb).ok_or("バッファ長が合いません")?;
    Ok(Decoded {
        rgb,
        icc: None,
        orientation: Orientation::NoTransforms,
    })
}
