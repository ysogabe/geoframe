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
//! - メタデータなし・構造が最後まで正しいこと（下の「JPEG の検査」「PNG・WebP の検査」）。
//!
//! # JPEG の検査
//!
//! 先頭の SOS で打ち切らず、**EOI まで全てのマーカーを順に読む**（`jpeg_scan`）。
//!
//! - 許す: SOF0/1/2（8bit・1 か 3 成分。ベースライン・拡張シーケンシャル・プログレッシブ）、DHT・DQT・DRI・SOS・EOI と、
//!   **形が固定の** APP0（`JFIF\0` 1.x の 14 バイトちょうど。サムネイルなし）・APP14（`Adobe` 12 バイトちょうど）。
//!   どのセグメントも、ペイロードが宣言どおりの長さで過不足なく読み切れること（DQT・DHT・SOF・SOS・DRI は中身の構造まで
//!   見る。余分なバイトに任意データを隠せない）。
//! - 断る: APP1（EXIF・XMP）・**APP2（ICC プロファイルは任意のタグを持てるので、中身を見ずに断る。入力は sRGB の
//!   derivative を前提とする）**・APP3〜13・APP15・COM・JPGn・JFXX や JFIF のサムネイル（`HasMetadata`）、
//!   算術符号・ロスレス・階層・12bit・4 成分、DNL、予約マーカー、SOF の後の APP0/APP14、SOI の重複（`Malformed`）。
//! - 画素データ（エントロピー符号化部分）は、バイトスタッフィング（`FF 00`）・リスタート（`FF D0〜D7`。DRI が有効で
//!   番号が 0 から順になっていること）・マーカー前の詰め物 `FF` を読み分けて**正しく読み飛ばす**。SOS を複数持つ
//!   プログレッシブも同じ。SOS のあとを無条件に飛ばしたり EOI を探したりはしない。
//! - **EOI が無い（SOS の後で切れた）ものは断る。EOI の後ろにデータがあるものも断る**（末尾に足されたメタデータ・
//!   別ファイルを送らない。MPF などを後ろに持つ写真は、ここへ来る前に derivative で作り直されている前提）。
//! - 構造を通ったあと、大きさ・画素数の上限を**先に**適用し、そのあとで `zune-jpeg` の厳格モードで**全体をデコードして
//!   みる**（出力は 1.6MP × 3 バイト以下。捨てる）。ハフマン符号の壊れ・表の欠落・寸法に足りない画素データを見る。
//!   ベースライン／シーケンシャルは**末尾の EOI を外して**読ませる（EOI が付け足された切れた走査を、デコーダーが
//!   ゼロで埋めて成功させない）。限界: プログレッシブは次のマーカーとして EOI を読むので EOI 付きで読ませ、
//!   「走査の途中で切れて EOI だけ付け足された」ものを見抜けない（EOI が無い切れ方は構造検査が断る）。
//!
//! 対象外（既知の限界）: 画素データそのもの・ハフマン表のシンボルに隠された任意のバイト列（ステガノグラフィ）、
//! 最後の走査の、全 MCU の後ろから EOI までに足されたバイト（ハフマン符号を自前で数えないと見分けられない）。
//!
//! # PNG・WebP の検査
//!
//! PNG は IHDR で始まり IEND で終わり、**IEND の後ろにデータが無い**こと。許すチャンクは IHDR・PLTE・IDAT・IEND・tRNS・
//! gAMA・cHRM・sRGB・sBIT・bKGD・pHYs だけで、iCCP・tIME・eXIf・テキスト・未知のチャンクは断る。WebP は RIFF の
//! 長さがファイルの長さと一致し、チャンクは VP8X・VP8・VP8L・ALPH だけ（ICCP・EXIF・XMP・未知は断る）。
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
        let mut progressive = false;
        let (format, width, height) = if bytes.starts_with(&[0xFF, 0xD8]) {
            let (w, h, p) = jpeg_scan(&bytes)?;
            progressive = p;
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
        // 上限を通ってから（デコードの資源は 1.6MP × 3 バイトまで）、画素データの符号を読み切れるか確かめる
        if format == ImageFormat::Jpeg {
            jpeg_decodes(&bytes, width, height, progressive)?;
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

/// 成分の数の上限（4 成分の CMYK・YCCK は断る）。
const JPEG_MAX_COMPONENTS: usize = 3;

/// JPEG を SOI の次から EOI まで読み、寸法（幅・高さ）とプログレッシブかを返す。メタデータ・構造の異常・EOI の欠落・EOI の後ろのデータは断る。
/// 設計は冒頭の「JPEG の検査」。
fn jpeg_scan(data: &[u8]) -> Result<(u32, u32, bool), ImageError> {
    use ImageError::{HasMetadata, Malformed};
    let mut i = 2;
    let mut size = None;
    let mut progressive = false;
    let mut ids = [0u8; JPEG_MAX_COMPONENTS];
    let mut ncomp = 0;
    let mut covered = [false; JPEG_MAX_COMPONENTS];
    let mut seen_app0 = false;
    let mut seen_app14 = false;
    let mut scans = 0u32;
    let mut restart_interval = 0u16;
    loop {
        // マーカーの前の詰め物 0xFF は許す
        if data.get(i) != Some(&0xFF) {
            return Err(Malformed);
        }
        while data.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        let marker = *data.get(i + 1).ok_or(Malformed)?;
        i += 2;
        match marker {
            // EOI: 全ての成分が走査されていて、ファイルの終わりでなければならない
            0xD9 => {
                if scans == 0 || !covered[..ncomp].iter().all(|c| *c) || i != data.len() {
                    return Err(Malformed);
                }
                let (width, height) = size.ok_or(Malformed)?;
                return Ok((width, height, progressive));
            }
            // 0x00・TEM・予約・RSTn・SOI・DNL・EXP は、ここ（セグメントの境）には来ない
            0x00 | 0x01 | 0x02..=0xBF | 0xD0..=0xD8 | 0xDC | 0xDF => return Err(Malformed),
            _ => {}
        }
        let head = data.get(i..i + 2).ok_or(Malformed)?;
        let len = be16(head) as usize;
        if len < 2 || i + len > data.len() {
            return Err(Malformed);
        }
        let payload = &data[i + 2..i + len];
        i += len;
        match marker {
            // JFIF: 形が固定（1.x・単位 0〜2・サムネイルなし）。JFXX・他の APP0 は断る。形が正しくても、
            // フレームの後・2 つ目は置き場所の異常
            0xE0 => {
                let ok = payload.len() == 14
                    && payload.starts_with(b"JFIF\0")
                    && payload[5] == 1
                    && payload[7] <= 2
                    && payload[12] == 0
                    && payload[13] == 0;
                if !ok {
                    return Err(HasMetadata);
                }
                if size.is_some() || seen_app0 {
                    return Err(Malformed);
                }
                seen_app0 = true;
            }
            // Adobe: "Adobe" + 版・フラグ・変換（12 バイトちょうど）
            0xEE => {
                if payload.len() != 12 || !payload.starts_with(b"Adobe") || payload[11] > 2 {
                    return Err(HasMetadata);
                }
                if size.is_some() || seen_app14 {
                    return Err(Malformed);
                }
                seen_app14 = true;
            }
            // APP1（EXIF・XMP）・APP2（ICC）・APP3〜13（IPTC ほか）・APP15・JPGn・COM
            0xE1..=0xED | 0xEF | 0xF0..=0xFE => return Err(HasMetadata),
            // SOF0（ベースライン）・SOF1（拡張シーケンシャル）・SOF2（プログレッシブ）
            0xC0..=0xC2 => {
                if size.is_some() || payload.len() < 6 || payload[0] != 8 {
                    return Err(Malformed);
                }
                let (height, width, nf) = (be16(&payload[1..]), be16(&payload[3..]), payload[5]);
                if height == 0 || width == 0 || !matches!(nf, 1 | 3) {
                    return Err(Malformed);
                }
                let nf = usize::from(nf);
                if payload.len() != 6 + 3 * nf {
                    return Err(Malformed);
                }
                for (k, c) in payload[6..].as_chunks::<3>().0.iter().enumerate() {
                    let (h, v) = (c[1] >> 4, c[1] & 15);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || c[2] > 3 {
                        return Err(Malformed);
                    }
                    if ids[..k].contains(&c[0]) {
                        return Err(Malformed);
                    }
                    ids[k] = c[0];
                }
                ncomp = nf;
                progressive = marker == 0xC2;
                size = Some((width, height));
            }
            // DHT: 表の列（クラス・番号・16 個の個数・その合計のシンボル）が過不足なく読み切れる
            0xC4 => {
                let mut rest = payload;
                if rest.is_empty() {
                    return Err(Malformed);
                }
                while !rest.is_empty() {
                    if rest.len() < 17 || rest[0] >> 4 > 1 || rest[0] & 15 > 3 {
                        return Err(Malformed);
                    }
                    let symbols: usize = rest[1..17].iter().map(|&n| usize::from(n)).sum();
                    if symbols > 256 || rest.len() < 17 + symbols {
                        return Err(Malformed);
                    }
                    rest = &rest[17 + symbols..];
                }
            }
            // DQT: 精度・番号・64 個の値の列が過不足なく読み切れる
            0xDB => {
                let mut rest = payload;
                if rest.is_empty() {
                    return Err(Malformed);
                }
                while !rest.is_empty() {
                    let (precision, id) = (rest[0] >> 4, rest[0] & 15);
                    if precision > 1 || id > 3 {
                        return Err(Malformed);
                    }
                    let need = 1 + 64 * (usize::from(precision) + 1);
                    if rest.len() < need {
                        return Err(Malformed);
                    }
                    rest = &rest[need..];
                }
            }
            0xDD => {
                if payload.len() != 2 {
                    return Err(Malformed);
                }
                restart_interval = be16(payload) as u16;
            }
            0xDA => {
                if size.is_none() {
                    return Err(Malformed);
                }
                jpeg_check_sos(payload, &ids[..ncomp], progressive, &mut covered)?;
                scans += 1;
                i = jpeg_skip_entropy(data, i, restart_interval)?;
            }
            // 算術符号（DAC・SOF9〜11・13〜15）・ロスレス（SOF3）・階層（SOF5〜7）・JPG（C8）
            _ => return Err(Malformed),
        }
    }
}

/// SOS のヘッダー（成分の選択・ハフマン表の番号・スペクトルの範囲・近似の桁）。
fn jpeg_check_sos(
    payload: &[u8],
    ids: &[u8],
    progressive: bool,
    covered: &mut [bool; JPEG_MAX_COMPONENTS],
) -> Result<(), ImageError> {
    let ns = usize::from(*payload.first().ok_or(ImageError::Malformed)?);
    if !(1..=ids.len()).contains(&ns) || payload.len() != 1 + 2 * ns + 3 {
        return Err(ImageError::Malformed);
    }
    let mut picked = [false; JPEG_MAX_COMPONENTS];
    for c in payload[1..1 + 2 * ns].as_chunks::<2>().0 {
        let k = ids
            .iter()
            .position(|&id| id == c[0])
            .ok_or(ImageError::Malformed)?;
        if picked[k] || c[1] >> 4 > 3 || c[1] & 15 > 3 {
            return Err(ImageError::Malformed);
        }
        picked[k] = true;
    }
    let tail = &payload[1 + 2 * ns..];
    let (ss, se, ah, al) = (tail[0], tail[1], tail[2] >> 4, tail[2] & 15);
    let ok = if progressive {
        ss <= se && se <= 63 && (ss != 0 || se == 0) && (ss == 0 || ns == 1) && ah <= 13 && al <= 13
    } else {
        ss == 0 && se == 63 && ah == 0 && al == 0
    };
    if !ok {
        return Err(ImageError::Malformed);
    }
    for (all, one) in covered.iter_mut().zip(picked) {
        *all |= one;
    }
    Ok(())
}

/// SOS ヘッダーの直後（`start`）から、エントロピー符号化部分を読み飛ばして次のマーカーの `0xFF` の位置を返す。
/// `FF 00`（スタッフィング）・`FF D0〜D7`（リスタート。DRI が有効で番号が 0 から順）・`FF FF`（詰め物）を読み分ける。
/// ファイルの終わりまでマーカーが来なければ（切れている）断る。
fn jpeg_skip_entropy(
    data: &[u8],
    start: usize,
    restart_interval: u16,
) -> Result<usize, ImageError> {
    let mut j = start;
    let mut next_restart = 0u8;
    let mut has_data = false;
    loop {
        let p = data[j..]
            .iter()
            .position(|&b| b == 0xFF)
            .ok_or(ImageError::Malformed)?;
        has_data |= p > 0;
        j += p;
        match *data.get(j + 1).ok_or(ImageError::Malformed)? {
            0x00 => {
                has_data = true;
                j += 2;
            }
            m @ 0xD0..=0xD7 => {
                if restart_interval == 0 || m != 0xD0 + next_restart {
                    return Err(ImageError::Malformed);
                }
                next_restart = (next_restart + 1) & 7;
                j += 2;
            }
            0xFF => j += 1,
            _ => break,
        }
    }
    if has_data {
        Ok(j)
    } else {
        Err(ImageError::Malformed)
    }
}

/// 構造検査を通った JPEG を、`zune-jpeg` の厳格モードで最後までデコードできるか（結果は捨てる）。
/// 呼ぶ前に大きさ・画素数の上限を通っていること（資源は 1.6MP × 3 バイトまで）。
fn jpeg_decodes(
    bytes: &[u8],
    width: u32,
    height: u32,
    progressive: bool,
) -> Result<(), ImageError> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::options::DecoderOptions;

    // ベースライン／シーケンシャルは EOI を外して読ませる: 画素データが足りなければデコーダーは EOI の手前で
    // 入力の終わりに当たり（厳格モードでは失敗）、EOI が付け足された切れた走査を「ゼロで埋めて成功」にさせない。
    // プログレッシブは次のマーカーとして EOI を読むので、そのまま読ませる
    let input = if progressive {
        bytes
    } else {
        &bytes[..bytes.len() - 2]
    };
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(width as usize)
        .set_max_height(height as usize);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(input), options);
    let pixels = decoder.decode().map_err(|_| ImageError::Malformed)?;
    match decoder.dimensions() {
        Some((w, h)) if w as u32 == width && h as u32 == height && !pixels.is_empty() => Ok(()),
        _ => Err(ImageError::Malformed),
    }
}

fn png_size(data: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut i = 8;
    let mut size = None;
    let mut ended = false;
    let mut idat = false;
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
        // IHDR が最初
        if size.is_none() && kind != b"IHDR" {
            return Err(ImageError::Malformed);
        }
        match kind {
            b"IHDR" => {
                if len != 13 || size.is_some() {
                    return Err(ImageError::Malformed);
                }
                size = Some((be32(&data[i + 8..]), be32(&data[i + 12..])));
            }
            b"IDAT" => idat = true,
            b"IEND" => {
                // IEND の後ろにデータがあれば断る（別ファイル・メタデータを足されている）
                ended = end == data.len() && len == 0;
                break;
            }
            b"PLTE" | b"tRNS" | b"gAMA" | b"cHRM" | b"sRGB" | b"sBIT" | b"bKGD" | b"pHYs" => {}
            // eXIf・テキスト・iCCP・tIME・未知のチャンクなど
            _ => return Err(ImageError::HasMetadata),
        }
        i = end;
    }
    match (size, ended && idat) {
        (Some(size), true) => Ok(size),
        _ => Err(ImageError::Malformed),
    }
}

fn webp_size(data: &[u8]) -> Result<(u32, u32), ImageError> {
    // RIFF の長さがファイルの長さと合う（後ろにデータが無い）
    if le32(&data[4..]) as usize + 8 != data.len() {
        return Err(ImageError::Malformed);
    }
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
            b"VP8X" => {
                if payload.len() < 10 {
                    return Err(ImageError::Malformed);
                }
                // 0x20: ICC、0x08: EXIF、0x04: XMP、0x02: アニメーション（扱わない）
                if payload[0] & 0x2C != 0 {
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
            b"ALPH" => {}
            // EXIF・XMP・ICCP・未知のチャンク
            _ => return Err(ImageError::HasMetadata),
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

    /// 画素が乱れた画像（`FF 00` のスタッフィングが出る）。
    pub fn noise_rgb(width: u16, height: u16) -> Vec<u8> {
        let mut x: u32 = 0x1234_5678;
        (0..usize::from(width) * usize::from(height) * 3)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    /// 乱れた画素を、指定の設定でエンコードした JPEG。
    pub fn jpeg_with(
        width: u16,
        height: u16,
        configure: impl FnOnce(&mut jpeg_encoder::Encoder<&mut Vec<u8>>),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
        configure(&mut encoder);
        encoder
            .encode(
                &noise_rgb(width, height),
                width,
                height,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        out
    }

    /// マーカー＋長さ＋ペイロードのセグメント。
    pub fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut seg = vec![0xFF, marker];
        seg.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(payload);
        seg
    }

    /// `at` の位置に差し込む。
    pub fn splice(mut data: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
        data.splice(at..at, bytes.iter().copied());
        data
    }

    /// 末尾の EOI の直前。
    pub fn before_eoi(data: Vec<u8>, bytes: &[u8]) -> Vec<u8> {
        let at = data.len() - 2;
        assert_eq!(&data[at..], [0xFF, 0xD9]);
        splice(data, at, bytes)
    }

    /// `FF <marker>` の位置（マーカー。エントロピー部分の `FF` は必ず `00` が続くので、`DA` などは取り違えない）。
    pub fn marker_positions(data: &[u8], marker: u8) -> Vec<usize> {
        (2..data.len() - 1)
            .filter(|&i| data[i] == 0xFF && data[i + 1] == marker)
            .collect()
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
            // ICC プロファイル（任意のタグを持てるので中身を見ずに断る）
            (0xE2, b"ICC_PROFILE\0\x01\x01".as_slice()),
            // JPGn
            (0xF5, b"x".as_slice()),
        ] {
            assert_eq!(
                PreparedImage::from_bytes(jpeg_with_segment(32, 32, marker, payload), D)
                    .unwrap_err(),
                ImageError::HasMetadata,
                "{marker:x}"
            );
        }
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

    // ---------------------------------------------------------------- JPEG を最後まで読む

    fn refused(bytes: Vec<u8>) -> ImageError {
        PreparedImage::from_bytes(bytes, D).unwrap_err()
    }

    /// 旧実装は最初の SOS で成功を返したので、画素データのあとのメタデータを通した。
    #[test]
    fn metadata_after_the_first_scan_is_refused() {
        let base = jpeg(32, 32);
        assert!(PreparedImage::from_bytes(base.clone(), D).is_ok());
        for (marker, payload) in [
            (0xE1, b"Exif\0\0II*\0".as_slice()),
            (0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>".as_slice()),
            (0xED, b"Photoshop 3.0\0".as_slice()),
            (0xFE, b"secret".as_slice()),
            (0xE0, b"JFXX\0\x10abc".as_slice()),
            (0xE2, b"ICC_PROFILE\0\x01\x01".as_slice()),
        ] {
            assert_eq!(
                refused(before_eoi(base.clone(), &segment(marker, payload))),
                ImageError::HasMetadata,
                "{marker:x}"
            );
        }
        // プログレッシブ: 2 つ目以降の走査の前・間にも入れられない
        let progressive = jpeg_with(48, 48, |e| e.set_progressive(true));
        let scans = marker_positions(&progressive, 0xDA);
        assert!(scans.len() >= 3, "{scans:?}");
        assert!(PreparedImage::from_bytes(progressive.clone(), D).is_ok());
        for at in &scans[1..] {
            assert_eq!(
                refused(splice(progressive.clone(), *at, &segment(0xFE, b"hidden"))),
                ImageError::HasMetadata
            );
        }
    }

    #[test]
    fn every_truncation_and_any_trailing_data_is_refused() {
        let base = jpeg_with(16, 16, |_| {});
        for cut in 2..base.len() {
            assert!(
                PreparedImage::from_bytes(base[..cut].to_vec(), D).is_err(),
                "cut={cut}"
            );
        }
        // SOS のヘッダーの直後で切れたもの（旧実装は成功した）
        let sos = marker_positions(&base, 0xDA)[0];
        let header_end = sos + 2 + usize::from(u16::from_be_bytes([base[sos + 2], base[sos + 3]]));
        assert_eq!(refused(base[..header_end].to_vec()), ImageError::Malformed);
        // 画素データの途中で切れて EOI が無い
        assert_eq!(
            refused(base[..header_end + 8].to_vec()),
            ImageError::Malformed
        );
        // EOI の後ろ
        for tail in [&[0u8][..], &[0xFF, 0xD9], &base, b"hidden".as_slice()] {
            let mut bytes = base.clone();
            bytes.extend_from_slice(tail);
            assert_eq!(refused(bytes), ImageError::Malformed);
        }
        // 走査のない JPEG（SOI・EOI だけ）
        assert_eq!(refused(vec![0xFF, 0xD8, 0xFF, 0xD9]), ImageError::Malformed);
    }

    /// 正当な JPEG の部品（スタッフィング・リスタート・詰め物・プログレッシブ・グレー）は誤って断らない。
    #[test]
    fn valid_entropy_coded_structure_is_not_misread() {
        let plain = jpeg_with(40, 40, |_| {});
        assert!(
            !marker_positions(&plain, 0x00).is_empty()
                || plain.windows(2).any(|w| w == [0xFF, 0x00])
        );
        assert!(PreparedImage::from_bytes(plain.clone(), D).is_ok());
        // 詰め物の 0xFF（マーカーの前）
        assert!(PreparedImage::from_bytes(before_eoi(plain.clone(), &[0xFF, 0xFF]), D).is_ok());
        // リスタート（FF D0〜D7 が 0 から順に 8 で一巡する）
        let restart = jpeg_with(40, 40, |e| e.set_restart_interval(1));
        assert!(restart.windows(2).any(|w| w == [0xFF, 0xD7]));
        assert_eq!(
            PreparedImage::from_bytes(restart.clone(), D)
                .unwrap()
                .dimensions(),
            (40, 40)
        );
        // DRI の無い走査にリスタートが来たら断る
        let sos = marker_positions(&plain, 0xDA)[0];
        let at = sos + 2 + usize::from(u16::from_be_bytes([plain[sos + 2], plain[sos + 3]]));
        assert_eq!(
            refused(splice(plain.clone(), at + 3, &[0xFF, 0xD0])),
            ImageError::Malformed
        );
        // 番号の順が違うリスタートも断る
        let first = restart.windows(2).position(|w| w == [0xFF, 0xD0]).unwrap();
        let mut skipped = restart.clone();
        skipped[first + 1] = 0xD2;
        assert_eq!(refused(skipped), ImageError::Malformed);
        // プログレッシブ（SOS が複数）・リスタート付きプログレッシブ
        for bytes in [
            jpeg_with(48, 48, |e| e.set_progressive(true)),
            jpeg_with(48, 48, |e| {
                e.set_progressive(true);
                e.set_restart_interval(2);
            }),
        ] {
            assert!(marker_positions(&bytes, 0xDA).len() >= 2);
            assert_eq!(
                PreparedImage::from_bytes(bytes, D).unwrap().dimensions(),
                (48, 48)
            );
        }
        // グレー（1 成分）
        let mut gray = Vec::new();
        jpeg_encoder::Encoder::new(&mut gray, 80)
            .encode(
                &noise_rgb(24, 24)[..24 * 24],
                24,
                24,
                jpeg_encoder::ColorType::Luma,
            )
            .unwrap();
        assert!(PreparedImage::from_bytes(gray, D).is_ok());
    }

    /// 許すマーカー（APP0・APP14・DRI・DQT・DHT）も、ペイロードが宣言と違えば（任意データを隠せるので）断る。
    #[test]
    fn allowed_markers_must_have_their_exact_shape() {
        let jfif = |extra: &[u8], thumb: (u8, u8)| {
            let mut p = b"JFIF\0\x01\x01\x00\x00\x01\x00\x01".to_vec();
            p.extend_from_slice(&[thumb.0, thumb.1]);
            p.extend_from_slice(extra);
            p
        };
        let base = jpeg(32, 32);
        // 先頭の（正当な）JFIF の前に差し込んで、差し込んだものだけを検査する
        let with_first = |seg: Vec<u8>| splice(base.clone(), 2, &seg);
        // JFXX・サムネイル付き JFIF・余分なバイトのある JFIF・版の違う JFIF
        for p in [
            b"JFXX\0\x10abc".to_vec(),
            jfif(&[1, 2, 3], (1, 1)),
            jfif(b"extra", (0, 0)),
            b"AVI1\0\0\0\0\0\0\0\0\0\0".to_vec(),
            {
                let mut v = jfif(&[], (0, 0));
                v[5] = 2;
                v
            },
        ] {
            assert_eq!(
                refused(with_first(segment(0xE0, &p))),
                ImageError::HasMetadata,
                "{p:?}"
            );
        }
        // 正しい JFIF（先頭）に、2 つ目の APP0 を足すと断る
        assert!(
            PreparedImage::from_bytes(with_first(segment(0xE0, &jfif(&[], (0, 0)))), D).is_err()
        );
        // Adobe: 12 バイトちょうどだけ
        let adobe = |extra: &[u8]| {
            let mut p = b"Adobe\0\x64\0\0\0\0\x01".to_vec();
            p.extend_from_slice(extra);
            p
        };
        assert!(PreparedImage::from_bytes(with_first(segment(0xEE, &adobe(&[]))), D).is_ok());
        assert_eq!(
            refused(with_first(segment(0xEE, &adobe(b"payload")))),
            ImageError::HasMetadata
        );
        assert_eq!(
            refused(with_first(segment(0xEE, b"NotAdobe\0\0\0\0"))),
            ImageError::HasMetadata
        );
        // DRI・DQT・DHT の余分なバイト
        assert_eq!(
            refused(with_first(segment(0xDD, &[0, 0, 1, 2]))),
            ImageError::Malformed
        );
        let mut dqt = vec![0u8];
        dqt.extend_from_slice(&[8u8; 64]);
        assert!(PreparedImage::from_bytes(with_first(segment(0xDB, &dqt)), D).is_ok());
        dqt.push(0x55);
        assert_eq!(
            refused(with_first(segment(0xDB, &dqt))),
            ImageError::Malformed
        );
        let mut dht = vec![0x00u8, 1];
        dht.extend_from_slice(&[0; 15]);
        dht.push(0); // シンボル 1 個
        assert!(PreparedImage::from_bytes(with_first(segment(0xC4, &dht)), D).is_ok());
        dht.extend_from_slice(b"hidden");
        assert_eq!(
            refused(with_first(segment(0xC4, &dht))),
            ImageError::Malformed
        );
        // 予約マーカー・TEM・SOI の重複・DNL・SOF の後の APP0
        for marker in [
            0x02u8, 0x7F, 0xBF, 0x01, 0xD8, 0xDC, 0xDF, 0xCC, 0xC8, 0xC3, 0xC9,
        ] {
            assert!(
                PreparedImage::from_bytes(with_first(segment(marker, b"xx")), D).is_err(),
                "{marker:x}"
            );
        }
        let sof = marker_positions(&base, 0xC0)[0];
        assert_eq!(
            refused(splice(
                base.clone(),
                sof,
                &segment(0xE0, &jfif(&[], (0, 0)))
            )),
            ImageError::Malformed
        );
    }

    /// 大きさ・画素数の上限は、デコードの前に適用される（デコードできない中身でも上限のエラーになる）。
    #[test]
    fn size_limits_apply_before_decoding() {
        let mut big = jpeg(32, 32);
        let sof = marker_positions(&big, 0xC0)[0];
        // 1500×1500（2.25MP）。画素データは 32×32 のまま＝デコードすれば失敗するが、その前に上限で断る
        big[sof + 5..sof + 7].copy_from_slice(&1500u16.to_be_bytes());
        big[sof + 7..sof + 9].copy_from_slice(&1500u16.to_be_bytes());
        assert_eq!(refused(big.clone()), ImageError::TooManyPixels);
        // 長辺の上限（Embedding の 768）も先
        big[sof + 5..sof + 7].copy_from_slice(&800u16.to_be_bytes());
        big[sof + 7..sof + 9].copy_from_slice(&700u16.to_be_bytes());
        assert_eq!(
            PreparedImage::from_bytes(big.clone(), ImageProfile::Embedding).unwrap_err(),
            ImageError::TooLarge
        );
        // 上限内で画素データが寸法に足りなければ、デコードの検証で断る
        big[sof + 5..sof + 7].copy_from_slice(&64u16.to_be_bytes());
        big[sof + 7..sof + 9].copy_from_slice(&64u16.to_be_bytes());
        assert_eq!(refused(big), ImageError::Malformed);
    }

    /// 構造は正しいが、画素データの符号が壊れているものはデコードの検証で断る。
    #[test]
    fn broken_entropy_data_is_refused_by_the_decode_check() {
        let base = jpeg_with(48, 48, |_| {});
        let sos = marker_positions(&base, 0xDA)[0];
        let start = sos + 2 + usize::from(u16::from_be_bytes([base[sos + 2], base[sos + 3]]));
        // 画素データの途中までに切り、EOI を付け足す（構造だけでは正しく見える）
        let mut cut = base[..start + (base.len() - start) / 3].to_vec();
        while cut.last() == Some(&0xFF) {
            cut.pop();
        }
        cut.extend_from_slice(&[0xFF, 0xD9]);
        assert!(PreparedImage::from_bytes(cut, D).is_err());
    }

    /// 正当な JPEG を誤って断らない: 大きさ（MCU に満たない端・1 画素）・標本化・リスタート・プログレッシブ・
    /// 滑らかな画素と乱れた画素の組み合わせ。
    #[test]
    fn valid_jpegs_of_many_shapes_are_all_accepted() {
        use jpeg_encoder::SamplingFactor as S;
        let sizes = [
            (1u16, 1u16),
            (7, 9),
            (8, 8),
            (15, 17),
            (16, 16),
            (33, 31),
            (100, 3),
            (3, 100),
            (257, 129),
        ];
        let mut checked = 0;
        for (w, h) in sizes {
            for smooth in [false, true] {
                for sampling in [S::R_4_4_4, S::F_2_2, S::F_2_1, S::F_1_2] {
                    for (progressive, restart) in [(false, 0u16), (false, 3), (true, 0), (true, 2)]
                    {
                        let rgb = if smooth { rgb(w, h) } else { noise_rgb(w, h) };
                        let mut out = Vec::new();
                        let mut e = jpeg_encoder::Encoder::new(&mut out, 85);
                        e.set_sampling_factor(sampling);
                        e.set_progressive(progressive);
                        if restart > 0 {
                            e.set_restart_interval(restart);
                        }
                        e.encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
                        let image = PreparedImage::from_bytes(out, D);
                        assert!(
                            image.is_ok(),
                            "{w}x{h} smooth={smooth} progressive={progressive} restart={restart}: {image:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 9 * 2 * 4 * 4);
    }

    /// 実際の derivative 生成（`imaging::render`）の出力が、そのまま準備済み入力として通る（EXIF・ICC つきの入力からでも、
    /// 出力には何も付かない）。T4.6 が既存の derivative から Decision の入力を作る前提の契約。
    #[test]
    fn the_real_derivative_pipeline_output_is_accepted() {
        let plain = jpeg_with(200, 150, |_| {});
        // EXIF（APP1）と ICC（APP2）を持つ入力
        let mut with_meta = plain.clone();
        for seg in [
            segment(0xE1, b"Exif\0\0II*\0"),
            segment(0xE2, b"ICC_PROFILE\0\x01\x01not-a-real-profile"),
        ] {
            with_meta = splice(with_meta, 2, &seg);
        }
        for input in [plain, with_meta] {
            let out = crate::imaging::render(&input, 0).unwrap();
            let image = PreparedImage::from_bytes(out.jpeg, D).unwrap();
            assert_eq!(image.format(), ImageFormat::Jpeg);
            assert_eq!(image.dimensions(), (200, 150));
        }
    }
}
