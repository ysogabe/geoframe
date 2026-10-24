//! HEIC（純 Rust の `zune-heic`）のデコードと、失敗したときの**構造の分類**。
//!
//! `zune-heic` は irot/imir/clap を適用して返す（向きは当てない。EXIF の Orientation も使わない）。
//! グリッド（iPhone の 512x512 タイル）は並列に復号する。HDR のゲインマップ・深度・アルファの補助画像は使わず、
//! 主画像だけを取り出す。
//!
//! # 構造の分類（[`HeicStructure`]）
//!
//! 実機の HEIC（Drive の原本）が読めないとき、**原因を切り分けられるように**、ファイルの*構造の種類*だけを
//! ログに出す: ブランド、主画像の種類（単一・グリッド・オーバーレイ…）、コーデック、ビット深度・クロマ形式・
//! HEVC のプロファイル、色の記述（ICC か nclx）、補助画像の種類（アルファ・深度・ゲインマップ）、項目の数。
//! **ファイル名・EXIF の値・寸法・座標は出さない**。4 文字の種別コードは英数字だけに限る。

use std::fmt;

use zune_heic::zune_core::bytestream::ZCursor;
use zune_heic::zune_core::options::DecoderOptions;
use zune_heic::{HeicErrors, HeifDecoder};

use super::error::{HeicFailure, ImageError};
use super::heic_guard;

/// `mdat` の上限。既定の 16 MiB は大きい HEIC（約 48MP は約 28 MB）を拒否する。ファイルはまるごとメモリに読む。
const MAX_MDAT: usize = 128 << 20;

/// デコード結果（向き補正・クロップは済み）。
pub struct HeicImage {
    pub width: u32,
    pub height: u32,
    /// 1（グレー）か 3（RGB）の成分のインターリーブ。アルファは捨てる。
    pub channels: u8,
    pub pixels: Vec<u8>,
    pub icc: Option<Vec<u8>>,
}

/// ヘッダだけ読んだ結果（寸法の上限の判定、EXIF の取り出し）。
pub struct HeicHeader {
    pub width: u32,
    pub height: u32,
    /// EXIF の TIFF 構造（`Exif\0\0` が付いていれば取り除いたもの）
    pub exif: Option<Vec<u8>>,
}

fn options() -> DecoderOptions {
    DecoderOptions::default().set_hevc_max_mdat_size(MAX_MDAT)
}

fn fail(err: &HeicErrors, bytes: &[u8]) -> ImageError {
    failure(classify(err), bytes)
}

fn failure(class: &'static str, bytes: &[u8]) -> ImageError {
    ImageError::Heic(Box::new(HeicFailure {
        class,
        structure: HeicStructure::probe(bytes),
    }))
}

/// デコーダに渡す前の、宣言された長さ・寸法の検査（[`heic_guard`]）。`zune-heic` は宣言値で先に確保するので、
/// 偽った小さなファイルでプロセスが落ちる。
fn guard(bytes: &[u8], max_pixels: u64, decode: bool) -> Result<(), ImageError> {
    heic_guard::check(bytes, max_pixels, decode).map_err(|class| failure(class, bytes))
}

/// ヘッダ（寸法と EXIF）だけ読む。デコードはしない。
pub fn read_header(bytes: &[u8]) -> Result<HeicHeader, ImageError> {
    guard(bytes, super::MAX_INPUT_PIXELS, false)?;
    let mut dec = HeifDecoder::new_with_options(ZCursor::new(bytes), options());
    dec.decode_headers().map_err(|e| fail(&e, bytes))?;
    let (w, h) = (dec.width(), dec.height());
    let (Some(w), Some(h)) = (w, h) else {
        return Err(ImageError::Decode {
            format: "heic",
            class: super::error::DecodeClass::Dimensions,
        });
    };
    let exif = dec
        .exif_data()
        .map(|d| super::exif::strip_exif_prefix(d).to_vec());
    Ok(HeicHeader {
        width: u32::try_from(w).unwrap_or(u32::MAX),
        height: u32::try_from(h).unwrap_or(u32::MAX),
        exif,
    })
}

/// デコードする。`max_pixels` を超える寸法はデコードの前に断る。
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<HeicImage, ImageError> {
    guard(bytes, max_pixels, true)?;
    let mut dec = HeifDecoder::new_with_options(ZCursor::new(bytes), options());
    dec.decode_headers().map_err(|e| fail(&e, bytes))?;
    if let (Some(w), Some(h)) = (dec.width(), dec.height()) {
        let (w, h) = (
            u32::try_from(w).unwrap_or(u32::MAX),
            u32::try_from(h).unwrap_or(u32::MAX),
        );
        if u64::from(w) * u64::from(h) > max_pixels {
            return Err(ImageError::TooManyPixels {
                width: w,
                height: h,
            });
        }
    }
    let pixels = dec.decode().map_err(|e| fail(&e, bytes))?;
    let (Some(w), Some(h), Some(cs)) = (dec.width(), dec.height(), dec.colorspace()) else {
        return Err(ImageError::Decode {
            format: "heic",
            class: super::error::DecodeClass::Corrupt,
        });
    };
    let unsupported = || ImageError::Decode {
        format: "heic",
        class: super::error::DecodeClass::Unsupported,
    };
    let (channels, pixels) = match cs.num_components() {
        3 => (3, pixels),
        // アルファは捨てる（Python の `convert("RGB")` と同じ。透明部分の下の色が出る）
        4 => (
            3,
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect(),
        ),
        1 => (1, pixels),
        // グレー + アルファ: Python は `LA` を RGB へ変換する
        2 => (
            3,
            pixels
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0]])
                .collect(),
        ),
        _ => return Err(unsupported()),
    };
    let (w, h) = (
        u32::try_from(w).map_err(|_| unsupported())?,
        u32::try_from(h).map_err(|_| unsupported())?,
    );
    let icc = dec.icc_data().filter(|d| !d.is_empty()).cloned();
    Ok(HeicImage {
        width: w,
        height: h,
        channels,
        pixels,
        icc,
    })
}

/// `HeicErrors` の種類を、ログに出せる固定の語に畳む（メッセージは出さない）。
fn classify(err: &HeicErrors) -> &'static str {
    match err {
        HeicErrors::Io(_) => "io",
        HeicErrors::UnexpectedEof { .. } => "truncated",
        HeicErrors::UnsupportedVersion { .. } => "unsupported-version",
        HeicErrors::PayloadTooShort { .. } => "box-too-short",
        HeicErrors::WouldUnderflow { .. } => "inconsistent",
        HeicErrors::NalErrors(_) => "hevc-bitstream",
        HeicErrors::HeaderError(_) => "box-header",
        HeicErrors::TooSmallOutput(..) => "buffer",
        HeicErrors::ParseError { box_type, msg } => {
            // ボックスの種別で畳む。メッセージは使わない（値を含みうる）
            match box_type.as_bytes() {
                b"grid" | b"iovl" => "grid",
                b"hvcC" | b"hvc1" => "hevc-config",
                b"iloc" | b"iinf" | b"infe" | b"pitm" | b"ipma" | b"ipco" | b"iprp" | b"iref" => {
                    "item-structure"
                }
                b"colr" | b"pixi" | b"clap" | b"irot" | b"imir" | b"ispe" => "image-property",
                _ => classify_message(msg),
            }
        }
        HeicErrors::Generic { msg } => classify_message(msg),
    }
}

/// メッセージの*語*から種類を決める。メッセージ自体は返さない。
fn classify_message(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.contains("grid") || m.contains("tile") {
        "grid"
    } else if m.contains("unsupported") || m.contains("not supported") {
        "unsupported"
    } else if m.contains("exceed") || m.contains("limit") || m.contains("too large") {
        "limit"
    } else if m.contains("not found")
        || m.contains("missing")
        || m.starts_with("no ")
        || m.contains(" no ")
    {
        "missing-box"
    } else if m.contains("overflow") || m.contains("zero") {
        "dimensions"
    } else if m.contains("decode failure") {
        "decode-failure"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------------------
// 構造の分類

/// ファイルの構造の種類（値は含まない）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeicStructure {
    /// `ftyp` のメジャーブランド（英数字 4 文字。読めなければ空）
    pub brand: String,
    /// 主画像の項目の種類: `single`（hvc1/av01 など）・`grid`・`overlay`・`identity`・`other`・`unknown`
    pub primary: &'static str,
    /// コーデック: `hevc`・`av1`・`other`・`unknown`
    pub codec: &'static str,
    /// HEVC のプロファイル（1=Main、2=Main 10、3=Main Still Picture、4=RExt…）
    pub hevc_profile: Option<u8>,
    pub bit_depth: Option<u8>,
    /// クロマ形式（0=モノクロ、1=4:2:0、2=4:2:2、3=4:4:4）
    pub chroma: Option<u8>,
    /// 色の記述: `prof`（ICC）・`nclx`・両方・`none`
    pub colr: &'static str,
    /// 補助画像: アルファ・深度・ゲインマップ・その他
    pub aux_alpha: bool,
    pub aux_depth: bool,
    pub aux_gainmap: bool,
    pub aux_other: bool,
    /// 項目の数（`iinf`）
    pub items: usize,
}

impl fmt::Display for HeicStructure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "brand={} primary={} codec={}",
            if self.brand.is_empty() {
                "-"
            } else {
                &self.brand
            },
            self.primary,
            self.codec
        )?;
        if let Some(p) = self.hevc_profile {
            write!(f, " profile={p}")?;
        }
        if let Some(d) = self.bit_depth {
            write!(f, " bit_depth={d}")?;
        }
        if let Some(c) = self.chroma {
            write!(f, " chroma={c}")?;
        }
        write!(f, " colr={}", self.colr)?;
        let aux: Vec<&str> = [
            (self.aux_alpha, "alpha"),
            (self.aux_depth, "depth"),
            (self.aux_gainmap, "gainmap"),
            (self.aux_other, "other"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| *n)
        .collect();
        write!(
            f,
            " aux={}",
            if aux.is_empty() {
                "-".into()
            } else {
                aux.join(",")
            }
        )?;
        write!(f, " items={}", self.items)
    }
}

/// 英数字だけの 4 文字にする（そうでなければ空）。ファイルの内容を文字列にして出さないため。
fn fourcc(b: &[u8]) -> String {
    if b.len() == 4 && b.iter().all(|c| c.is_ascii_alphanumeric()) {
        b.iter().map(|&c| char::from(c)).collect()
    } else {
        String::new()
    }
}

/// ボックスの一覧（`(種別, 本体)`）。壊れていれば手前で止める。
pub(super) fn boxes(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let kind = [data[4], data[5], data[6], data[7]];
        let (header, total) = match size {
            0 => (8, data.len()), // 末尾まで
            1 => {
                if data.len() < 16 {
                    break;
                }
                let big = u64::from_be_bytes(data[8..16].try_into().unwrap_or([0; 8]));
                (16, usize::try_from(big).unwrap_or(usize::MAX))
            }
            n => (8, n),
        };
        if total < header || total > data.len() {
            break;
        }
        out.push((kind, &data[header..total]));
        data = &data[total..];
    }
    out
}

pub(super) fn be16(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(o..o + 2)?.try_into().ok()?,
    )))
}

pub(super) fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// `iinf` の本体から、項目の ID と種類（`infe` の version 2・3）。壊れていれば手前で止める。
pub(super) fn item_types(iinf: &[u8]) -> Vec<(u32, [u8; 4])> {
    let mut out = Vec::new();
    let version = iinf.first().copied().unwrap_or(0);
    let start = if version == 0 { 6 } else { 8 };
    for (k, infe) in iinf.get(start..).map(boxes).unwrap_or_default() {
        if &k != b"infe" {
            continue;
        }
        let v = infe.first().copied().unwrap_or(0);
        let (id, type_at) = match v {
            2 => (be16(infe, 4), 4 + 2 + 2),
            3 => (be32(infe, 4), 4 + 4 + 2),
            _ => continue,
        };
        if let (Some(id), Some(t)) = (id, infe.get(type_at..type_at + 4)) {
            out.push((id, [t[0], t[1], t[2], t[3]]));
        }
    }
    out
}

impl HeicStructure {
    /// バイト列から構造を読む。壊れていても止まらず、分かった分だけ埋める。
    pub fn probe(bytes: &[u8]) -> Self {
        let mut s = Self {
            primary: "unknown",
            codec: "unknown",
            colr: "none",
            ..Self::default()
        };
        let top = boxes(bytes);
        if let Some((_, body)) = top.iter().find(|(k, _)| k == b"ftyp") {
            s.brand = body.get(..4).map(fourcc).unwrap_or_default();
        }
        // `meta` は FullBox（先頭 4 バイトがバージョンとフラグ）
        let Some((_, meta)) = top.iter().find(|(k, _)| k == b"meta") else {
            return s;
        };
        let Some(meta) = meta.get(4..) else { return s };
        let children = boxes(meta);

        let primary_id = children
            .iter()
            .find(|(k, _)| k == b"pitm")
            .and_then(|(_, b)| match b.first() {
                Some(0) => be16(b, 4),
                Some(_) => be32(b, 4),
                None => None,
            });

        // iinf: 項目の ID と種類
        let item_types = children
            .iter()
            .find(|(k, _)| k == b"iinf")
            .map(|(_, iinf)| item_types(iinf))
            .unwrap_or_default();
        s.items = item_types.len();
        if let Some(pid) = primary_id
            && let Some((_, t)) = item_types.iter().find(|(id, _)| *id == pid)
        {
            s.primary = match t {
                b"grid" => "grid",
                b"iovl" => "overlay",
                b"iden" => "identity",
                b"hvc1" | b"hev1" | b"av01" | b"jpeg" | b"avc1" | b"vvc1" => "single",
                _ => "other",
            };
        }

        // iprp/ipco: 性質。最初に見つかった hvcC/av1C をコーデックの代表にする（グリッドはタイルのもの）
        let ipco = children
            .iter()
            .find(|(k, _)| k == b"iprp")
            .map(|(_, b)| boxes(b))
            .and_then(|v| v.into_iter().find(|(k, _)| k == b"ipco"))
            .map(|(_, b)| boxes(b))
            .unwrap_or_default();
        let (mut prof, mut nclx) = (false, false);
        for (kind, body) in &ipco {
            match kind {
                b"hvcC" if s.codec == "unknown" => {
                    s.codec = "hevc";
                    s.hevc_profile = body.get(1).map(|b| b & 0x1f);
                    s.chroma = body.get(16).map(|b| b & 0x03);
                    s.bit_depth = body.get(17).map(|b| (b & 0x07) + 8);
                }
                b"av1C" if s.codec == "unknown" => s.codec = "av1",
                b"colr" => match body.get(..4) {
                    Some(b"prof" | b"rICC") => prof = true,
                    Some(b"nclx" | b"nclc") => nclx = true,
                    _ => {}
                },
                b"auxC" => {
                    // FullBox + NUL 終端の URN。語から種類を決め、文字列は出さない
                    let urn = body.get(4..).unwrap_or_default().to_ascii_lowercase();
                    let has = |w: &[u8]| urn.windows(w.len()).any(|x| x == w);
                    if has(b"alpha") {
                        s.aux_alpha = true;
                    } else if has(b"depth") {
                        s.aux_depth = true;
                    } else if has(b"gainmap") || has(b"hdrgain") {
                        s.aux_gainmap = true;
                    } else {
                        s.aux_other = true;
                    }
                }
                _ => {}
            }
        }
        if s.codec == "unknown" {
            // hvcC が無くても、項目の種類から分かることがある
            if item_types.iter().any(|(_, t)| t == b"hvc1" || t == b"hev1") {
                s.codec = "hevc";
            } else if item_types.iter().any(|(_, t)| t == b"av01") {
                s.codec = "av1";
            } else if !item_types.is_empty() {
                s.codec = "other";
            }
        }
        s.colr = match (prof, nclx) {
            (true, true) => "prof+nclx",
            (true, false) => "prof",
            (false, true) => "nclx",
            _ => "none",
        };
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn 単一画像の構造を読む() {
        let s = HeicStructure::probe(&fixture("synthetic_o1.heic"));
        assert_eq!(s.brand, "heic");
        assert_eq!(s.primary, "single");
        assert_eq!(s.codec, "hevc");
        assert_eq!(s.bit_depth, Some(8));
        assert_eq!(s.chroma, Some(1));
        // pillow-heif は nclx の色の記述を付ける（ICC は付けない）
        assert_eq!(s.colr, "nclx");
        assert_eq!(s.items, 2); // 主画像と EXIF
        assert!(!s.aux_alpha && !s.aux_depth && !s.aux_gainmap);
        let text = s.to_string();
        assert!(
            text.starts_with("brand=heic primary=single codec=hevc"),
            "{text}"
        );
    }

    #[test]
    fn iccありの構造() {
        let s = HeicStructure::probe(&fixture("synthetic_p3.heic"));
        assert_eq!(s.colr, "prof");
    }

    #[test]
    fn 壊れた入力でも止まらず分類を返す() {
        for bytes in [
            &b""[..],
            b"\0\0\0\x18ftypheic",
            b"not heic at all, just some text that is long enough",
            &[0xFF; 64],
        ] {
            let s = HeicStructure::probe(bytes);
            assert_eq!(s.primary, "unknown");
            assert_eq!(s.codec, "unknown");
        }
        // 途中で切れていても、読めた分だけ
        let mut cut = fixture("synthetic_o1.heic");
        cut.truncate(cut.len() / 2);
        let _ = HeicStructure::probe(&cut);
    }

    #[test]
    fn 種別コードは英数字のときだけ出す() {
        assert_eq!(fourcc(b"heic"), "heic");
        assert_eq!(fourcc(b"he\nc"), "");
        assert_eq!(fourcc(b"a/..c"), "");
        assert_eq!(fourcc(&[0xFF, 0xFE, 0xFD, 0xFC]), "");
    }

    #[test]
    fn エラーの分類は固定の語になる() {
        let e = HeicErrors::Generic {
            msg: "Grid item has no data".into(),
        };
        assert_eq!(classify(&e), "grid");
        let e = HeicErrors::Generic {
            msg: "MDAT 999999 size exceeds HEIC max configured size 1".into(),
        };
        assert_eq!(classify(&e), "limit");
        let e = HeicErrors::Generic {
            msg: "/home/user/secret.heic is odd".into(),
        };
        assert_eq!(classify(&e), "other");
    }
}
