//! 画像処理の失敗。
//!
//! **文面にパス・ファイル名・画像の内容を含めない**（inventory.md §6.3）。含められるのは定型文、
//! 失敗の種類を表す固定の語、`std::io::ErrorKind`（パスを持たない）、寸法、HEIC の構造の分類だけ。
//! ログにはこの `Display` をそのまま出してよい。

use std::fmt;
use std::io;

use super::heic::HeicStructure;

/// 画像のデコードの失敗の種類（固定の語）。デコーダが返す文字列は使わない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeClass {
    /// 途中で切れている（Pillow は既定でエラーにする）
    Truncated,
    /// 壊れている（ヘッダ・ハフマン表・量子化表・チャンクなど）
    Corrupt,
    /// 未対応の方式（算術符号化・ロスレス JPEG・未対応の PNG など）
    Unsupported,
    /// 寸法が不正（0、または上限を超える）
    Dimensions,
}

impl DecodeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Truncated => "truncated",
            Self::Corrupt => "corrupt",
            Self::Unsupported => "unsupported",
            Self::Dimensions => "dimensions",
        }
    }
}

#[derive(Debug)]
pub enum ImageError {
    /// ファイルの読み書き。`ErrorKind` だけを持つ。
    Io(io::ErrorKind),
    /// 内容が JPEG・PNG・TIFF・HEIC のどれでもない。
    NotAnImage,
    /// ファイルが大きすぎてメモリに載せない。
    FileTooLarge,
    /// 画素数が上限を超える（Pillow の `DecompressionBombError` と同じ考え方）。
    TooManyPixels { width: u32, height: u32 },
    /// デコードの失敗。`format` は "jpeg" などの固定の語。
    Decode {
        format: &'static str,
        class: DecodeClass,
    },
    /// HEIC のデコードの失敗。構造の分類を持つ（ファイル名・内容は持たない）。
    Heic(Box<HeicFailure>),
    /// JPEG のエンコードの失敗。
    Encode,
    /// 手動回転が 0・90・180・270 の倍数でない。
    BadRotation,
    /// DATA_DIR の外へ出る、または名前として不正（[`crate::datadir::PathError`]）。
    Path(crate::datadir::PathError),
}

/// HEIC の失敗: エラーの種類（固定の語）とファイルの構造。
#[derive(Debug)]
pub struct HeicFailure {
    /// `HeicErrors` の種類を畳んだ固定の語（"grid"、"hevc-bitstream"、"missing-box" など）
    pub class: &'static str,
    pub structure: HeicStructure,
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "ファイルの入出力に失敗しました（{kind}）"),
            Self::NotAnImage => f.write_str("画像として読めません（形式が不明）"),
            Self::FileTooLarge => f.write_str("ファイルが大きすぎます"),
            Self::TooManyPixels { width, height } => {
                write!(f, "画素数が上限を超えています（{width}x{height}）")
            }
            Self::Decode { format, class } => {
                write!(f, "{format}をデコードできません（{}）", class.as_str())
            }
            Self::Heic(h) => write!(
                f,
                "heicをデコードできません（{}）構造: {}",
                h.class, h.structure
            ),
            Self::Encode => f.write_str("JPEGを書き出せません"),
            Self::BadRotation => f.write_str("回転の角度が不正です"),
            Self::Path(e) => write!(f, "パスが不正です（{e}）"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind())
    }
}

impl From<crate::datadir::PathError> for ImageError {
    fn from(e: crate::datadir::PathError) -> Self {
        Self::Path(e)
    }
}
