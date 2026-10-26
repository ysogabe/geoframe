//! Provider へ送れる画像の「準備済み入力」（P4 / T4.2。設計は p4-design.md §5「入力画像」）。
//!
//! [`PreparedImage`] は、**この検査を通ったバイト列だけ**を持つ。元画像・ファイルのパス・外部 URL は受けず、
//! メタデータ（EXIF・GPS・XMP・テキスト・コメント）を持つ画像は断る。送信側（[`super::decision`]）は
//! `PreparedImage` しか受け取らないので、元画像を検査なしで送る道は無い。
//!
//! # 検査する制約
//!
//! - 形式は JPEG・PNG・WebP（先頭のバイトで判定。dataURL の MIME は検出した形式から作る）。
//! - 1 枚 8,000,000 バイト以下、**1.6 メガピクセル（1,600,000 画素）以下**、長辺は用途の上限以下
//!   （埋め込み用 768px 系・Decision 1536px）。
//! - メタデータなし: JPEG は APP1（EXIF・XMP）・APP13（IPTC）・COM・未知の APPn を、PNG は `eXIf`・`tEXt`・`zTXt`・`iTXt`
//!   を、WebP は `EXIF`・`XMP ` を含まないこと。
//!
//! # 範囲（T4.2 と T4.6）
//!
//! この版は**検査の境界と、安全な入力を作る入口**（[`PreparedImage::encode_jpeg_rgb`]。`jpeg-encoder` は EXIF を出さない）
//! までを持つ。写真の読み取り・縮小・向きの適用・再エンコードの実処理は、用途（Decision は T4.6、埋め込みは T4.4）が
//! 既存の derivative（EXIF を捨てた JPEG）から行う。

use base64::Engine as _;

/// 1 枚のバイト数の上限（Decision の上流の上限と同じ）。
pub const MAX_IMAGE_BYTES: usize = 8_000_000;
/// 1 枚の画素数の上限（モデルが読む 1.6 メガピクセル）。
pub const MAX_PIXELS: u64 = 1_600_000;

/// 用途ごとの長辺の上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProfile {
    /// 画像・検索語の Embedding 用（768px 系）。
    Embedding,
    /// Decision・位置候補（文字が読める大きさ）。
    Decision,
}

impl ImageProfile {
    pub fn max_long_side(self) -> u32 {
        match self {
            Self::Embedding => 768,
            Self::Decision => 1536,
        }
    }
}

/// 準備済み入力を作れなかった理由（固定の列挙。画像の中身は持たない）。どれも同じ入力では何度やっても
/// 同じ結果になる恒久の失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    Empty,
    /// JPEG・PNG・WebP のどれでもない。
    UnknownFormat,
    /// 形式として読めない（寸法が取れない・切れている）。
    Malformed,
    TooManyBytes,
    TooLarge,
    TooManyPixels,
    /// EXIF・GPS・XMP・テキストなどを含む。
    HasMetadata,
}

impl ImageError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::UnknownFormat => "unknown_format",
            Self::Malformed => "malformed",
            Self::TooManyBytes => "too_many_bytes",
            Self::TooLarge => "too_large",
            Self::TooManyPixels => "too_many_pixels",
            Self::HasMetadata => "has_metadata",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }
}

/// 検査済みの画像。バイト列は `Debug` に出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedImage {
    format: ImageFormat,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for PreparedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedImage")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl PreparedImage {
    /// 検査して取り込む。メタデータを持つ画像・制約を超える画像は断る（黙って削らない）。
    pub fn from_bytes(bytes: Vec<u8>, profile: ImageProfile) -> Result<Self, ImageError> {
        if bytes.is_empty() {
            return Err(ImageError::Empty);
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(ImageError::TooManyBytes);
        }
        let (format, width, height) = if bytes.starts_with(&[0xFF, 0xD8]) {
            let (w, h) = jpeg_size(&bytes)?;
            (ImageFormat::Jpeg, w, h)
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            let (w, h) = png_size(&bytes)?;
            (ImageFormat::Png, w, h)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            let (w, h) = webp_size(&bytes)?;
            (ImageFormat::Webp, w, h)
        } else {
            return Err(ImageError::UnknownFormat);
        };
        if width == 0 || height == 0 {
            return Err(ImageError::Malformed);
        }
        if width.max(height) > profile.max_long_side() {
            return Err(ImageError::TooLarge);
        }
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(ImageError::TooManyPixels);
        }
        Ok(Self {
            format,
            width,
            height,
            bytes,
        })
    }

    /// RGB の画素列から、メタデータのない JPEG を作って検査する（安全な入力を作る入口。縮小・向きの適用は呼び出し側）。
    pub fn encode_jpeg_rgb(
        rgb: &[u8],
        width: u16,
        height: u16,
        quality: u8,
        profile: ImageProfile,
    ) -> Result<Self, ImageError> {
        if width == 0 || height == 0 || rgb.len() != usize::from(width) * usize::from(height) * 3 {
            return Err(ImageError::Malformed);
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100))
            .encode(rgb, width, height, jpeg_encoder::ColorType::Rgb)
            .map_err(|_| ImageError::Malformed)?;
        Self::from_bytes(out, profile)
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// base64 にした長さ（dataURL の接頭辞を除く）。
    pub fn base64_len(&self) -> usize {
        self.bytes.len().div_ceil(3) * 4
    }

    /// `data:<mime>;base64,<...>`。外部 URL・ファイルのパスは作れない。
    pub fn data_url(&self) -> String {
        let mut url = format!("data:{};base64,", self.format.mime());
        base64::engine::general_purpose::STANDARD.encode_string(&self.bytes, &mut url);
        url
    }
}

// ---------------------------------------------------------------- 形式ごとの寸法・メタデータ

fn be16(b: &[u8]) -> u32 {
    u32::from(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn jpeg_size(data: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut i = 2;
    let mut size = None;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            return Err(ImageError::Malformed);
        }
        let marker = data[i + 1];
        i += 2;
        match marker {
            // 詰め物の 0xFF
            0xFF => {
                i -= 1;
                continue;
            }
            // 長さを持たない（TEM・RSTn・SOI）
            0x01 | 0xD0..=0xD8 => continue,
            // EOI: SOS の前に終わった
            0xD9 => return Err(ImageError::Malformed),
            _ => {}
        }
        if i + 2 > data.len() {
            return Err(ImageError::Malformed);
        }
        let len = be16(&data[i..]) as usize;
        if len < 2 || i + len > data.len() {
            return Err(ImageError::Malformed);
        }
        let payload = &data[i + 2..i + len];
        match marker {
            // JFIF・ICC プロファイル・Adobe は許す。EXIF/XMP（APP1）・IPTC（APP13）・コメント・その他の APPn は断る
            0xE0 | 0xE2 | 0xEE => {}
            0xE1 | 0xE3..=0xED | 0xEF | 0xFE => return Err(ImageError::HasMetadata),
            // SOFn（DHT=C4・JPG=C8・DAC=CC を除く）
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if payload.len() < 6 {
                    return Err(ImageError::Malformed);
                }
                size = Some((be16(&payload[3..]), be16(&payload[1..])));
            }
            // SOS: ここから先は画素データ
            0xDA => return size.ok_or(ImageError::Malformed),
            _ => {}
        }
        i += len;
    }
    Err(ImageError::Malformed)
}

fn png_size(data: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut i = 8;
    let mut size = None;
    let mut ended = false;
    while i + 8 <= data.len() {
        let len = be32(&data[i..]) as usize;
        let kind = &data[i + 4..i + 8];
        let end = i
            .checked_add(12)
            .and_then(|n| n.checked_add(len))
            .ok_or(ImageError::Malformed)?;
        if end > data.len() {
            return Err(ImageError::Malformed);
        }
        match kind {
            b"IHDR" => {
                if len != 13 || size.is_some() {
                    return Err(ImageError::Malformed);
                }
                size = Some((be32(&data[i + 8..]), be32(&data[i + 12..])));
            }
            b"eXIf" | b"tEXt" | b"zTXt" | b"iTXt" => return Err(ImageError::HasMetadata),
            b"IEND" => {
                ended = true;
                break;
            }
            _ => {}
        }
        i = end;
    }
    match (size, ended) {
        (Some(size), true) => Ok(size),
        _ => Err(ImageError::Malformed),
    }
}

fn webp_size(data: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut i = 12;
    let mut size = None;
    while i + 8 <= data.len() {
        let kind = &data[i..i + 4];
        let len = le32(&data[i + 4..]) as usize;
        let start = i + 8;
        let end = start.checked_add(len).ok_or(ImageError::Malformed)?;
        if end > data.len() {
            return Err(ImageError::Malformed);
        }
        let payload = &data[start..end];
        match kind {
            b"EXIF" | b"XMP " => return Err(ImageError::HasMetadata),
            b"VP8X" => {
                if payload.len() < 10 {
                    return Err(ImageError::Malformed);
                }
                // 0x08: EXIF、0x04: XMP、0x02: アニメーション（扱わない）
                if payload[0] & 0x0C != 0 {
                    return Err(ImageError::HasMetadata);
                }
                if payload[0] & 0x02 != 0 {
                    return Err(ImageError::Malformed);
                }
                let w = 1 + u32::from_le_bytes([payload[4], payload[5], payload[6], 0]);
                let h = 1 + u32::from_le_bytes([payload[7], payload[8], payload[9], 0]);
                size.get_or_insert((w, h));
            }
            b"VP8 " => {
                if payload.len() < 10 || payload[3..6] != [0x9D, 0x01, 0x2A] {
                    return Err(ImageError::Malformed);
                }
                let w = u32::from(u16::from_le_bytes([payload[6], payload[7]]) & 0x3FFF);
                let h = u32::from(u16::from_le_bytes([payload[8], payload[9]]) & 0x3FFF);
                size.get_or_insert((w, h));
            }
            b"VP8L" => {
                if payload.len() < 5 || payload[0] != 0x2F {
                    return Err(ImageError::Malformed);
                }
                let bits = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
                size.get_or_insert(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1));
            }
            _ => {}
        }
        // チャンクは偶数長に詰められる
        i = end + (len & 1);
    }
    size.ok_or(ImageError::Malformed)
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! 合成画像（試験だけ）。実写真・実メタデータは使わない。

    pub fn rgb(width: u16, height: u16) -> Vec<u8> {
        (0..usize::from(width) * usize::from(height) * 3)
            .map(|i| (i % 251) as u8)
            .collect()
    }

    pub fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 80)
            .encode(
                &rgb(width, height),
                width,
                height,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        out
    }

    /// SOI の直後に、指定のマーカーのセグメントを差し込む。
    pub fn jpeg_with_segment(width: u16, height: u16, marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut data = jpeg(width, height);
        let mut seg = vec![0xFF, marker];
        seg.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(payload);
        data.splice(2..2, seg);
        data
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]); // CRC は見ない
        out
    }

    pub fn png(width: u32, height: u32, extra: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        out.extend(chunk(b"IHDR", &ihdr));
        if let Some((kind, data)) = extra {
            out.extend(chunk(kind, data));
        }
        out.extend(chunk(b"IDAT", &[0]));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    fn riff(chunks: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(chunks);
        out
    }

    fn webp_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// 損失ありの WebP（VP8）。`flags` を指定すると VP8X を前に付ける。
    pub fn webp_lossy(width: u16, height: u16, vp8x_flags: Option<u8>) -> Vec<u8> {
        let mut vp8 = vec![0, 0, 0, 0x9D, 0x01, 0x2A];
        vp8.extend_from_slice(&width.to_le_bytes());
        vp8.extend_from_slice(&height.to_le_bytes());
        let mut chunks = Vec::new();
        if let Some(flags) = vp8x_flags {
            let mut x = vec![flags, 0, 0, 0];
            x.extend_from_slice(&(u32::from(width) - 1).to_le_bytes()[..3]);
            x.extend_from_slice(&(u32::from(height) - 1).to_le_bytes()[..3]);
            chunks.extend(webp_chunk(b"VP8X", &x));
        }
        chunks.extend(webp_chunk(b"VP8 ", &vp8));
        riff(&chunks)
    }

    pub fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
        let bits = (width - 1) | ((height - 1) << 14);
        let mut data = vec![0x2F];
        data.extend_from_slice(&bits.to_le_bytes());
        riff(&webp_chunk(b"VP8L", &data))
    }

    pub fn webp_with_exif(width: u16, height: u16) -> Vec<u8> {
        let mut chunks = webp_chunk(b"EXIF", b"Exif\0\0II*\0");
        let lossy = webp_lossy(width, height, None);
        chunks.extend_from_slice(&lossy[12..]);
        riff(&chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    const D: ImageProfile = ImageProfile::Decision;

    #[test]
    fn jpeg_png_webp_are_accepted_and_become_data_urls() {
        let jpeg = PreparedImage::from_bytes(jpeg(64, 48), D).unwrap();
        assert_eq!(
            (jpeg.format(), jpeg.dimensions()),
            (ImageFormat::Jpeg, (64, 48))
        );
        assert!(jpeg.data_url().starts_with("data:image/jpeg;base64,"));
        assert_eq!(
            jpeg.data_url().len(),
            "data:image/jpeg;base64,".len() + jpeg.base64_len()
        );
        let png = PreparedImage::from_bytes(png(100, 50, None), D).unwrap();
        assert_eq!(
            (png.format(), png.dimensions()),
            (ImageFormat::Png, (100, 50))
        );
        assert!(png.data_url().starts_with("data:image/png;base64,"));
        for webp in [
            webp_lossy(30, 20, None),
            webp_lossy(30, 20, Some(0)),
            webp_lossless(30, 20),
        ] {
            let image = PreparedImage::from_bytes(webp, D).unwrap();
            assert_eq!(
                (image.format(), image.dimensions()),
                (ImageFormat::Webp, (30, 20))
            );
            assert!(image.data_url().starts_with("data:image/webp;base64,"));
        }
    }

    #[test]
    fn metadata_bearing_images_are_refused() {
        // JPEG: EXIF（APP1）・XMP（APP1）・IPTC（APP13）・コメント
        for (marker, payload) in [
            (0xE1, b"Exif\0\0".as_slice()),
            (0xE1, b"http://ns.adobe.com/xap/1.0/\0".as_slice()),
            (0xED, b"Photoshop 3.0\0".as_slice()),
            (0xFE, b"hello".as_slice()),
            (0xE5, b"x".as_slice()),
        ] {
            assert_eq!(
                PreparedImage::from_bytes(jpeg_with_segment(32, 32, marker, payload), D)
                    .unwrap_err(),
                ImageError::HasMetadata,
                "{marker:x}"
            );
        }
        // JFIF・ICC は許す
        assert!(
            PreparedImage::from_bytes(jpeg_with_segment(32, 32, 0xE2, b"ICC_PROFILE\0"), D).is_ok()
        );
        for kind in [b"eXIf", b"tEXt", b"zTXt", b"iTXt"] {
            assert_eq!(
                PreparedImage::from_bytes(png(10, 10, Some((kind, b"x"))), D).unwrap_err(),
                ImageError::HasMetadata
            );
        }
        assert_eq!(
            PreparedImage::from_bytes(webp_lossy(10, 10, Some(0x08)), D).unwrap_err(),
            ImageError::HasMetadata
        );
        assert_eq!(
            PreparedImage::from_bytes(webp_lossy(10, 10, Some(0x04)), D).unwrap_err(),
            ImageError::HasMetadata
        );
        assert_eq!(
            PreparedImage::from_bytes(webp_with_exif(10, 10), D).unwrap_err(),
            ImageError::HasMetadata
        );
    }

    #[test]
    fn size_pixel_and_byte_limits() {
        // 長辺 1536 ちょうどは可、1537 は不可（Decision）
        assert!(PreparedImage::from_bytes(png(1536, 1041, None), D).is_ok());
        assert_eq!(
            PreparedImage::from_bytes(png(1537, 100, None), D).unwrap_err(),
            ImageError::TooLarge
        );
        // 1.6MP 以下: 1536×1042 = 1,600,512 は超える。1536×1041 = 1,598,976 は収まる
        assert_eq!(
            PreparedImage::from_bytes(png(1536, 1042, None), D).unwrap_err(),
            ImageError::TooManyPixels
        );
        // 埋め込みは長辺 768
        assert!(PreparedImage::from_bytes(png(768, 768, None), ImageProfile::Embedding).is_ok());
        assert_eq!(
            PreparedImage::from_bytes(png(769, 10, None), ImageProfile::Embedding).unwrap_err(),
            ImageError::TooLarge
        );
        // 1 枚 8,000,000 バイト: 形式を見る前にバイト数で断る
        let mut big = png(10, 10, None);
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(
            PreparedImage::from_bytes(big, D).unwrap_err(),
            ImageError::TooManyBytes
        );
        assert_eq!(
            PreparedImage::from_bytes(Vec::new(), D).unwrap_err(),
            ImageError::Empty
        );
    }

    #[test]
    fn garbage_and_foreign_formats_are_refused() {
        for bad in [
            b"GIF89a....".to_vec(),
            b"<svg/>".to_vec(),
            b"http://example.invalid/a.jpg".to_vec(),
            b"/etc/passwd".to_vec(),
        ] {
            assert_eq!(
                PreparedImage::from_bytes(bad, D).unwrap_err(),
                ImageError::UnknownFormat
            );
        }
        let mut cut = jpeg(32, 32);
        cut.truncate(20);
        assert_eq!(
            PreparedImage::from_bytes(cut, D).unwrap_err(),
            ImageError::Malformed
        );
        let mut png_cut = png(10, 10, None);
        png_cut.truncate(30);
        assert_eq!(
            PreparedImage::from_bytes(png_cut, D).unwrap_err(),
            ImageError::Malformed
        );
    }

    #[test]
    fn the_encoder_entry_makes_clean_checked_jpegs() {
        let image = PreparedImage::encode_jpeg_rgb(&rgb(40, 30), 40, 30, 85, D).unwrap();
        assert_eq!(image.dimensions(), (40, 30));
        // 画素数の合わない入力は作れない
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&rgb(40, 30), 41, 30, 85, D).unwrap_err(),
            ImageError::Malformed
        );
        // 用途の上限を越える大きさは、エンコード後の検査で断られる
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&rgb(800, 10), 800, 10, 85, ImageProfile::Embedding)
                .unwrap_err(),
            ImageError::TooLarge
        );
    }
}
