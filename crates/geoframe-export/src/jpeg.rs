//! 表示用 JPEG の区画の検査と、デコード（design.md 4.1 の手順 5）。
//!
//! 表示用 JPEG はメタデータ（EXIF・GPS・XMP・IPTC など）を落としてあるはずなので、**許す区画だけ**で組み立てられて
//! いるかを、デコードの前にバイト列を直接たどって確かめる。作り方が変わって EXIF 付きの画像が配られたときに、
//! 外へ出さずに気づくため（`has_metadata`）。
//!
//! 許すもの:
//! - SOI・EOI
//! - APP0 の JFIF（サムネイル無しの 14 バイトの本体だけ）
//! - APP2 の ICC プロファイル（`ICC_PROFILE\0`）
//! - 画像の区画: SOF・DHT・DAC・DQT・DRI・DNL・SOS と、その後ろの符号化データ（RSTn を含む）
//!
//! 許さないもの（`has_metadata`）: 上以外の APPn（APP1 の Exif/XMP、APP2 の MPF、APP13 の IPTC、APP11 など）、COM、
//! JPGn、EOI の後ろのデータ。
//!
//! 構造が JPEG として成り立たないもの（SOI が無い・区画の長さが残りを超える・区画の間に余計なバイトがある・
//! EOI が無い・SOF か SOS が無い）は、メタデータの有無を問わず「壊れている」（`decode_failed`）。
//!
//! 寸法は SOF のヘッダから読み、画素数の上限はデコードの前に確かめる（上限を超える画像は、確保もデコードもしない）。

use crate::measure::rgb_to_gray;

/// SOF が持つ寸法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
}

impl Header {
    pub fn pixels(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// 区画の検査の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inspection {
    /// 許す区画だけでできている。
    Clean(Header),
    /// 許さない区画か、EOI の後ろのデータがある。
    HasMetadata,
}

/// 検査の失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectError {
    /// JPEG として成り立たない（理由は固定の短い説明）。
    Malformed(&'static str),
    /// 画素数が上限を超える（デコードの前に止める）。
    TooManyPixels,
}

fn be16(b: &[u8]) -> usize {
    usize::from(b[0]) << 8 | usize::from(b[1])
}

/// `ICC_PROFILE\0`＋順番 2 バイトを持つ APP2。
fn is_icc(payload: &[u8]) -> bool {
    payload.len() >= 14 && payload.starts_with(b"ICC_PROFILE\0")
}

/// サムネイル無しの JFIF の APP0（`JFIF\0`＋版 2＋単位 1＋密度 4＋サムネイルの幅・高さ 0・0）。
fn is_plain_jfif(payload: &[u8]) -> bool {
    payload.len() == 14 && payload.starts_with(b"JFIF\0") && payload[12] == 0 && payload[13] == 0
}

/// 画素の上限（`max_pixels`）を掛けて、区画を検査する。`has_metadata` の写真は、画素数が大きくても
/// 外へ出さないだけで済むので失敗にしない（デコードしないため）。
pub fn inspect(bytes: &[u8], max_pixels: u64) -> Result<Inspection, InspectError> {
    let bad = InspectError::Malformed;
    let len = bytes.len();
    if len < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err(bad("JPEG の先頭（SOI）がありません"));
    }
    let mut pos = 2;
    let mut header: Option<Header> = None;
    let mut scans = 0u32;
    let mut metadata = false;
    loop {
        // 区画の間は、マーカー（0xFF の並びのあとの 0 以外のバイト）から始まる
        if pos >= len {
            return Err(bad("JPEG の終わり（EOI）がありません"));
        }
        if bytes[pos] != 0xFF {
            return Err(bad("JPEG の区画の間に余計なバイトがあります"));
        }
        while pos < len && bytes[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = bytes.get(pos) else {
            return Err(bad("JPEG の終わり（EOI）がありません"));
        };
        pos += 1;
        match marker {
            0xD9 => {
                if header.is_none() || scans == 0 {
                    return Err(bad("JPEG に画像の区画（SOF・SOS）がありません"));
                }
                if pos < len {
                    metadata = true; // EOI の後ろにデータがある
                }
                break;
            }
            // 区画の外の SOI・RSTn・TEM・0x00・予約のマーカー
            0x00..=0xBF | 0xD0..=0xD8 => return Err(bad("JPEG の区画の並びが正しくありません")),
            _ => {}
        }
        // ここからは長さを持つ区画
        if pos + 2 > len {
            return Err(bad("JPEG の区画の長さが読めません"));
        }
        let seg_len = be16(&bytes[pos..pos + 2]);
        if seg_len < 2 || pos + seg_len > len {
            return Err(bad("JPEG の区画の長さが残りのバイト数を超えています"));
        }
        let payload = &bytes[pos + 2..pos + seg_len];
        pos += seg_len;
        match marker {
            0xE0 => metadata |= !is_plain_jfif(payload),
            0xE2 => metadata |= !is_icc(payload),
            // 他の APPn、COM（0xFE）、JPGn（0xF0〜0xFD）、階層の DHP・EXP（0xDE・0xDF）、予約の JPG（0xC8）
            0xE1 | 0xE3..=0xEF | 0xF0..=0xFE | 0xDE | 0xDF | 0xC8 => metadata = true,
            // SOF（0xC0〜0xCF のうち DHT・JPG・DAC 以外）
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                if header.is_some() {
                    return Err(bad("JPEG に SOF が複数あります"));
                }
                if payload.len() < 6 {
                    return Err(bad("JPEG の SOF が短すぎます"));
                }
                let height = be16(&payload[1..3]) as u32;
                let width = be16(&payload[3..5]) as u32;
                if width == 0 || height == 0 {
                    return Err(bad("JPEG の寸法が 0 です"));
                }
                header = Some(Header { width, height });
            }
            0xDA => {
                if header.is_none() {
                    return Err(bad("JPEG の SOS が SOF より前にあります"));
                }
                scans += 1;
                // 符号化データを読み飛ばす: 次の「0xFF のあとが 0x00・RSTn・0xFF 以外」まで
                let mut i = pos;
                loop {
                    if i >= len {
                        return Err(bad("JPEG の終わり（EOI）がありません"));
                    }
                    if bytes[i] != 0xFF {
                        i += 1;
                        continue;
                    }
                    let mut j = i + 1;
                    while j < len && bytes[j] == 0xFF {
                        j += 1;
                    }
                    match bytes.get(j) {
                        None => return Err(bad("JPEG の終わり（EOI）がありません")),
                        Some(0x00 | 0xD0..=0xD7) => i = j + 1,
                        Some(_) => break,
                    }
                }
                pos = i;
            }
            // DQT・DHT・DRI・DAC・DNL
            0xDB | 0xC4 | 0xDD | 0xCC | 0xDC => {}
            _ => metadata = true,
        }
    }
    if metadata {
        return Ok(Inspection::HasMetadata);
    }
    let header = header.ok_or(bad("JPEG に SOF がありません"))?;
    if header.pixels() > max_pixels {
        return Err(InspectError::TooManyPixels);
    }
    Ok(Inspection::Clean(header))
}

/// デコードの失敗（理由は持たない。呼び出し側が固定の説明に直す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

/// デコードした、縮小前のグレー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// JPEG をデコードしてグレー（Pillow の `convert("L")` 相当）にする。`header` は [`inspect`] が読んだ寸法で、
/// デコーダが報告する寸法と食い違えば失敗にする。グレーの JPEG は 1 チャンネルのまま、それ以外は RGB で受け取る。
/// 厳格なモードで読む。区画の並びと EOI の有無は [`inspect`] が先に確かめている（EOI が無い＝途中で切れたものは
/// そこで失敗する）。EOI があって走査データが足りない JPEG は、libjpeg（Pillow）と同じく、デコーダが成功させる。
pub fn decode_gray(bytes: &[u8], header: Header) -> Result<Gray, DecodeError> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(65_535)
        .set_max_height(65_535);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    dec.decode_headers().map_err(|_| DecodeError)?;
    let (w, h) = dec.dimensions().ok_or(DecodeError)?;
    if (w as u32, h as u32) != (header.width, header.height) {
        return Err(DecodeError);
    }
    let out = match dec.input_colorspace() {
        Some(ColorSpace::Luma) => ColorSpace::Luma,
        _ => ColorSpace::RGB,
    };
    let options = dec.options().jpeg_set_out_colorspace(out);
    dec.set_options(options);
    let data = dec.decode().map_err(|_| DecodeError)?;
    let pixels = w * h;
    let data = if out == ColorSpace::Luma {
        if data.len() != pixels {
            return Err(DecodeError);
        }
        data
    } else {
        if data.len() != pixels * 3 {
            return Err(DecodeError);
        }
        rgb_to_gray(&data)
    };
    Ok(Gray {
        width: w,
        height: h,
        data,
    })
}
