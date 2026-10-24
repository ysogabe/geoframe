//! 画像処理（P2 / T2.2）: derivative の生成、EXIF（撮影時刻・GPS）、HEIC。
//!
//! 取り込みの段（T2.3 のローカル索引・`local_derivatives`、T2.4 の Drive・`fetch_derivatives`）が呼ぶ
//! **同期 API**。CPU を食うので、同期サイクルの std スレッドから呼ぶ（tokio の worker では呼ばない）。
//! 詳細は `docs/rust-migration/server.md` §12。
//!
//! | 関数 | Python | 内容 |
//! |---|---|---|
//! | [`derivative_name`] | `util.derivative_name` | 回したものは別名 |
//! | [`store_derivative`]・[`make_derivative`] | `util.make_derivative` | 原本 → 長辺 2560px の JPEG（`.part` 経由） |
//! | [`extract_exif`] | `util.extract_exif` | 撮影時刻・GPS |
//! | [`norm_time`] | `util.norm_time` | `YYYY-MM-DD HH:MM` |
//! | [`is_supported_ext`] | `config.EXTS` | 取り込む拡張子 |

mod derivative;
mod error;
pub mod exif;
pub mod heic;
mod heic_guard;
pub mod probe;

use std::path::Path;

pub use derivative::{
    DERIV_PX, DerivativeInfo, IccOutcome, JPEG_QUALITY, MAX_INPUT_BYTES, MAX_INPUT_PIXELS,
    Rendered, Stored, derivative_name, make_derivative, render, store_derivative, target_dims,
};
pub use error::{DecodeClass, HeicFailure, ImageError};
pub use exif::{ExifInfo, Format, PhotoMeta, norm_time};

/// 取り込む拡張子（小文字、ドット付き）。Python の `EXTS` は HEIC を `pillow_heif` があるときだけ含める
/// （本番は入っている）。Rust は HEIC を内蔵しているので常に含める。
pub const EXTS: [&str; 7] = [".jpg", ".jpeg", ".png", ".tif", ".tiff", ".heic", ".heif"];

/// 拡張子が取り込み対象か（Python の `p.suffix.lower() in EXTS`）。
pub fn is_supported_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&format!(".{}", e.to_ascii_lowercase()).as_str()))
}

/// ブラウザがそのまま表示できる拡張子か（`local_derivatives` が原本を表示に使う判定。Python の
/// `src.suffix.lower() in (".jpg", ".jpeg", ".png")`）。
pub fn is_browser_displayable_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
}

/// 画像ファイルの撮影時刻・GPS（Python の `extract_exif(path)`）。ヘッダだけ読む（デコードしない）。
///
/// 画像として読めない（JPEG・PNG・TIFF・HEIC のどれでもない）ときは [`ImageError::NotAnImage`]。
/// Python は `Image.open` の失敗で例外にし、`index_local` は「読み込み失敗」としてその写真を飛ばす。
/// EXIF が無い・壊れているのは失敗ではなく、位置なし・時刻なしの結果（Python と同じ）。
pub fn extract_exif(path: &Path) -> Result<PhotoMeta, ImageError> {
    Ok(extract_exif_info(path)?.meta)
}

/// [`extract_exif`] に向きも付けたもの。
pub fn extract_exif_info(path: &Path) -> Result<ExifInfo, ImageError> {
    let bytes = read_for_exif(path)?;
    exif_info_from_bytes(&bytes)
}

/// バイト列から読む。
pub fn exif_info_from_bytes(bytes: &[u8]) -> Result<ExifInfo, ImageError> {
    let format = Format::sniff(bytes).ok_or(ImageError::NotAnImage)?;
    let heic_exif;
    let heic_ref = if format == Format::Heic {
        heic_exif = heic::read_header(bytes)?.exif;
        heic_exif.as_deref()
    } else {
        None
    };
    Ok(exif::exif_tiff(format, bytes, heic_ref)
        .map(|tiff| exif::parse_tiff(&tiff))
        .unwrap_or_else(ExifInfo::empty))
}

/// JPEG の EXIF は画像データ（SOS）の前にあるので、先頭だけ読めばよい。
const JPEG_HEAD_BYTES: u64 = 4 << 20;

/// 画像の先頭から EXIF を読むためのバイト列。JPEG は先頭の一部、PNG・TIFF・HEIC は（EXIF の位置が
/// ファイルのどこにありうるので）全体を読む。
fn read_for_exif(path: &Path) -> Result<Vec<u8>, ImageError> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    let mut head = Vec::new();
    let mut reader = file.take(JPEG_HEAD_BYTES);
    reader.read_to_end(&mut head)?;
    if Format::sniff(&head) == Some(Format::Jpeg) {
        return Ok(head);
    }
    let mut file = reader.into_inner();
    (&mut file)
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut head)?;
    if head.len() as u64 > MAX_INPUT_BYTES {
        return Err(ImageError::FileTooLarge);
    }
    Ok(head)
}

#[cfg(test)]
mod tests;
