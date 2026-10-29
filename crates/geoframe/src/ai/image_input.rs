//! Provider へ送れる画像の「準備済み入力」（P4 / T4.2。設計は p4-design.md §5「入力画像」）。
//!
//! [`PreparedImage`] は、**検証済みの RGB 画素列から、このモジュールが新しく作った JPEG だけ**を持つ。
//! 圧縮済みの JPEG・PNG・WebP・dataURL・ファイルのパス・外部 URL を取り込む入口は無い（`from_bytes` のようなものは
//! 公開にも内部にも置かない。試験用にも増やさない）。送信側（[`super::decision`]）は `PreparedImage` しか受け取らない
//! ので、元画像のバイト列（EXIF・GPS・XMP・コメント・PNG のチャンク・WebP の拡張領域・EOI の後ろの追加データなど）が
//! Provider へ出る道は構造的に無い。
//!
//! # 作り方（唯一の入口）
//!
//! [`PreparedImage::encode_jpeg_rgb`]。呼び出し側（後続の handler）が既存の imaging でデコード・向きの適用・縮小を済ませ、
//! 得た RGB（`R,G,B,R,G,B,…` の `width × height × 3` バイト）を渡す。
//!
//! - **エンコードの前に**、寸法が 0 でないこと・長辺が用途の上限以下（埋め込み 768px・Decision 1536px）・画素数が
//!   1,600,000 以下・RGB の長さが `width × height × 3` ちょうどであることを確かめる（掛け算は桁あふれしない型で行う）。
//!   合わなければ何も確保せず断る（黙って切らない・欠けた画素を補わない）。
//! - エンコーダーは毎回新しく作り、ベースラインの JPEG を作る（`jpeg-encoder`。EXIF・ICC・コメントは出さない。JFIF の
//!   APP0 は形が固定でサムネイルを持たない）。出力が 8,000,000 バイトを超えれば断る。
//! - 入力の画素そのもの（に埋められた文字・模様）は対象外（ステガノグラフィは検出しない）。
//!
//! 圧縮データ（元の JPEG・PNG・WebP など）の読み取りは、この型には無い。元画像のデコードに失敗した・画素が欠けて
//! いた場合の扱いは、デコードする既存の imaging の責務で、ここへは成功して得た RGB だけが来る。
//!
//! 参考: 旧版はバイト列を取り込み、独自の JPEG/PNG/WebP の解析で EXIF 等を探していたが、非画素の追加データ
//! （EOI 直前の文字列・PNG の不正長チャンク・WebP の VP8X 拡張）や画素の欠損（CRC の合う `IDAT=[0]` の 1×1 PNG、
//! `VP8X` だけの WebP、走査の途中で切れ EOI を付け直したプログレッシブ）を通した。入力のバイト列を保持しない
//! 作りに変えることで、解析の巨大化と見逃しの両方をなくした。
//!
//! ```
//! use geoframe::ai::image_input::{ImageProfile, PreparedImage};
//!
//! let rgb = vec![128u8; 16 * 8 * 3];
//! let image = PreparedImage::encode_jpeg_rgb(&rgb, 16, 8, 85, ImageProfile::Decision).unwrap();
//! assert_eq!(image.dimensions(), (16, 8));
//! assert!(image.data_url().starts_with("data:image/jpeg;base64,"));
//! ```
//!
//! 圧縮済みのバイト列を包む入口と、private なフィールドを直接組み立てる書き方は、コンパイルできない:
//!
//! ```compile_fail,E0599
//! use geoframe::ai::image_input::{ImageProfile, PreparedImage};
//! let _ = PreparedImage::from_bytes(vec![0xFF, 0xD8], ImageProfile::Decision);
//! ```
//!
//! ```compile_fail,E0599
//! use geoframe::ai::image_input::PreparedImage;
//! let _ = PreparedImage::from_data_url("data:image/jpeg;base64,AAAA".to_string());
//! ```
//!
//! ```compile_fail,E0451
//! use geoframe::ai::image_input::PreparedImage;
//! let _ = PreparedImage { width: 1, height: 1, jpeg: vec![0xFF, 0xD8] };
//! ```

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
    /// 寸法が 0、または RGB の長さが `width × height × 3` と合わない（欠けた・余分な画素）。
    Malformed,
    /// 長辺が用途の上限を超える。
    TooLarge,
    /// 画素数が 1,600,000 を超える。
    TooManyPixels,
    /// 作った JPEG が 8,000,000 バイトを超える。
    TooManyBytes,
}

impl ImageError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::TooLarge => "too_large",
            Self::TooManyPixels => "too_many_pixels",
            Self::TooManyBytes => "too_many_bytes",
        }
    }
}

/// このモジュールが RGB から作った JPEG。バイト列・画素・dataURL は `Debug` に出さない（寸法だけ）。
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedImage {
    width: u32,
    height: u32,
    jpeg: Vec<u8>,
}

impl std::fmt::Debug for PreparedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl PreparedImage {
    /// RGB の画素列（`width × height × 3` バイト）から、メタデータのないベースラインの JPEG を新しく作る。
    /// 制約は**エンコードの前**に確かめる。`quality` は 1〜100 に丸める。
    pub fn encode_jpeg_rgb(
        rgb: &[u8],
        width: u16,
        height: u16,
        quality: u8,
        profile: ImageProfile,
    ) -> Result<Self, ImageError> {
        let (w, h) = (u32::from(width), u32::from(height));
        if w == 0 || h == 0 {
            return Err(ImageError::Malformed);
        }
        if w.max(h) > profile.max_long_side() {
            return Err(ImageError::TooLarge);
        }
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_PIXELS {
            return Err(ImageError::TooManyPixels);
        }
        // pixels ≤ 1.6M なので × 3 は桁あふれしない
        if rgb.len() as u64 != pixels * 3 {
            return Err(ImageError::Malformed);
        }
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, quality.clamp(1, 100))
            .encode(rgb, width, height, jpeg_encoder::ColorType::Rgb)
            .map_err(|_| ImageError::Malformed)?;
        if jpeg.len() > MAX_IMAGE_BYTES {
            return Err(ImageError::TooManyBytes);
        }
        Ok(Self {
            width: w,
            height: h,
            jpeg,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 作った JPEG のバイト数。
    pub fn len(&self) -> usize {
        self.jpeg.len()
    }

    /// 実送信 JPEG の来歴用。圧縮済み入力を取り込む入口は増やさない。
    pub fn sha256(&self) -> String {
        ring::digest::digest(&ring::digest::SHA256, &self.jpeg)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.jpeg.is_empty()
    }

    /// base64 にした長さ（dataURL の接頭辞を除く）。
    pub fn base64_len(&self) -> usize {
        self.jpeg.len().div_ceil(3) * 4
    }

    /// クラウドの inlineData 用。入力は増やさず、この型が作った JPEG だけを返す。
    pub(crate) fn jpeg_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.jpeg)
    }

    /// `data:image/jpeg;base64,<自分が作った JPEG>`。外部 URL・ファイルのパス・他の形式は作れない。
    pub fn data_url(&self) -> String {
        let mut url = String::from("data:image/jpeg;base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&self.jpeg, &mut url);
        url
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! 合成画像（試験だけ）。実写真・実メタデータは使わない。準備済み入力は必ず [`PreparedImage::encode_jpeg_rgb`] で作る。

    use super::{ImageProfile, PreparedImage};

    /// 滑らかな RGB。
    pub fn rgb(width: u16, height: u16) -> Vec<u8> {
        (0..usize::from(width) * usize::from(height) * 3)
            .map(|i| (i % 251) as u8)
            .collect()
    }

    /// 画素が乱れた RGB（圧縮しにくい）。
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

    /// Decision 用の小さな合成画像。
    pub fn image(width: u16, height: u16) -> PreparedImage {
        PreparedImage::encode_jpeg_rgb(
            &rgb(width, height),
            width,
            height,
            82,
            ImageProfile::Decision,
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    const D: ImageProfile = ImageProfile::Decision;

    /// dataURL から JPEG のバイト列を取り出す。
    fn jpeg_of(image: &PreparedImage) -> Vec<u8> {
        let url = image.data_url();
        let b64 = url.strip_prefix("data:image/jpeg;base64,").unwrap();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap()
    }

    /// 出力の JPEG のマーカーを SOI から EOI まで順にたどり、現れたマーカーの列を返す（走査の中身は読み飛ばす）。
    fn markers(jpeg: &[u8]) -> Vec<u8> {
        assert_eq!(&jpeg[..2], [0xFF, 0xD8]);
        let (mut i, mut out) = (2, Vec::new());
        loop {
            assert_eq!(jpeg[i], 0xFF, "マーカーの位置 {i}");
            let m = jpeg[i + 1];
            out.push(m);
            if m == 0xD9 {
                assert_eq!(i + 2, jpeg.len(), "EOI の後ろにデータがある");
                return out;
            }
            let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
            i += 2 + len;
            if m == 0xDA {
                // 走査: FF 00 以外の FF（マーカー）まで読み飛ばす
                while !(jpeg[i] == 0xFF && jpeg[i + 1] != 0x00) {
                    i += 1;
                }
            }
        }
    }

    /// 厳格モードでデコードして RGB を得る（寸法・全画素の確認用）。
    fn decode(jpeg: &[u8]) -> (usize, usize, Vec<u8>) {
        use zune_jpeg::JpegDecoder;
        use zune_jpeg::zune_core::bytestream::ZCursor;
        use zune_jpeg::zune_core::options::DecoderOptions;
        let mut d = JpegDecoder::new_with_options(
            ZCursor::new(jpeg),
            DecoderOptions::default().set_strict_mode(true),
        );
        let pixels = d.decode().unwrap();
        let (w, h) = d.dimensions().unwrap();
        (w, h, pixels)
    }

    #[test]
    fn the_encoder_makes_a_clean_decodable_jpeg_with_the_same_size_and_pixels() {
        let (w, h) = (64u16, 48u16);
        let src = rgb(w, h);
        let image = PreparedImage::encode_jpeg_rgb(&src, w, h, 90, D).unwrap();
        assert_eq!(image.dimensions(), (64, 48));
        let jpeg = jpeg_of(&image);
        assert_eq!(jpeg.len(), image.len());
        assert!(!image.is_empty());
        assert_eq!(
            image.data_url().len(),
            "data:image/jpeg;base64,".len() + image.base64_len()
        );
        // 全体を最後までデコードでき、寸法が合い、画素が元に近い（高品質の再現。全画素の健全性）
        let (dw, dh, pixels) = decode(&jpeg);
        assert_eq!((dw, dh), (64, 48));
        assert_eq!(pixels.len(), src.len());
        let mean_err = pixels
            .iter()
            .zip(&src)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum::<u64>() as f64
            / src.len() as f64;
        assert!(mean_err < 12.0, "mean abs error {mean_err}");
        // メタデータなし: 現れるマーカーは JFIF の APP0・DQT・SOF0（ベースライン）・DHT・SOS・EOI だけ
        for m in markers(&jpeg) {
            assert!(
                matches!(m, 0xE0 | 0xDB | 0xC0 | 0xC4 | 0xDA | 0xD9),
                "{m:x}"
            );
        }
        let app0 = jpeg.windows(2).position(|w| w == [0xFF, 0xE0]).unwrap();
        assert_eq!(&jpeg[app0 + 4..app0 + 9], b"JFIF\0");
        assert_eq!(u16::from_be_bytes([jpeg[app0 + 2], jpeg[app0 + 3]]), 16);
    }

    #[test]
    fn nothing_but_the_generated_jpeg_is_in_the_output() {
        // 圧縮コンテナに足されうる追加データ（旧版が通した 3 種）の文字列は、画素の入力には存在せず、
        // 出力にも現れない。入力は画素列だけで、元の圧縮データを受ける型・引数がそもそも無い
        let marker: &[u8] = b"GPS=35.0,139.0 hidden";
        for (w, h) in [(64u16, 64u16), (1, 1), (33, 17)] {
            let image = PreparedImage::encode_jpeg_rgb(&rgb(w, h), w, h, 85, D).unwrap();
            let jpeg = jpeg_of(&image);
            assert!(jpeg.windows(marker.len()).all(|x| x != marker));
            for key in [
                &b"Exif"[..],
                b"http://ns.adobe.com",
                b"ICC_PROFILE",
                b"gAMA",
                b"VP8X",
            ] {
                assert!(jpeg.windows(key.len()).all(|x| x != key), "{key:?}");
            }
            assert_eq!(markers(&jpeg).last(), Some(&0xD9));
        }
        // 同じ画素からは同じ JPEG（毎回新しいエンコーダーで、再使用する状態が出力に残らない）
        let a = PreparedImage::encode_jpeg_rgb(&rgb(40, 30), 40, 30, 85, D).unwrap();
        let b = PreparedImage::encode_jpeg_rgb(&rgb(40, 30), 40, 30, 85, D).unwrap();
        assert_eq!(a, b);
        // 品質は 1〜100 に丸める（0 でも 255 でも作れる）
        for q in [0u8, 255] {
            assert!(PreparedImage::encode_jpeg_rgb(&rgb(8, 8), 8, 8, q, D).is_ok());
        }
    }

    #[test]
    fn invalid_shapes_are_refused_before_encoding() {
        // 寸法 0
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], 0, 10, 85, D).unwrap_err(),
            ImageError::Malformed
        );
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], 10, 0, 85, D).unwrap_err(),
            ImageError::Malformed
        );
        // RGB の長さが合わない（欠けた画素・余分な画素・チャンネル数の取り違え）
        let src = rgb(40, 30);
        for bad in [
            &src[..src.len() - 1],
            &src[..src.len() - 3],
            &src[..3],
            &[][..],
        ] {
            assert_eq!(
                PreparedImage::encode_jpeg_rgb(bad, 40, 30, 85, D).unwrap_err(),
                ImageError::Malformed
            );
        }
        let mut longer = src.clone();
        longer.push(0);
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&longer, 40, 30, 85, D).unwrap_err(),
            ImageError::Malformed
        );
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&src, 41, 30, 85, D).unwrap_err(),
            ImageError::Malformed
        );
        // 長辺（Decision 1536・Embedding 768）。上限ちょうどは可
        assert!(
            PreparedImage::encode_jpeg_rgb(&rgb(1536, 8), 1536, 8, 85, D).is_ok(),
            "1536 は可"
        );
        // 長さが合わない入力でも、上限の違反が先に分かる（巨大な確保をしない）
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], 1537, 8, 85, D).unwrap_err(),
            ImageError::TooLarge
        );
        assert!(
            PreparedImage::encode_jpeg_rgb(&rgb(768, 4), 768, 4, 85, ImageProfile::Embedding)
                .is_ok()
        );
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], 769, 4, 85, ImageProfile::Embedding).unwrap_err(),
            ImageError::TooLarge
        );
        // 1.6MP: 1536×1042 = 1,600,512 は超える。1536×1041 = 1,598,976 は収まる
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], 1536, 1042, 85, D).unwrap_err(),
            ImageError::TooManyPixels
        );
        assert_eq!(
            PreparedImage::encode_jpeg_rgb(&[], u16::MAX, u16::MAX, 85, D).unwrap_err(),
            ImageError::TooLarge
        );
        // 画素数の上限ちょうど近く（1536×1041）は、長さが正しければ作れる
        let big = PreparedImage::encode_jpeg_rgb(&rgb(1536, 1041), 1536, 1041, 82, D).unwrap();
        assert_eq!(big.dimensions(), (1536, 1041));
        assert!(big.len() <= MAX_IMAGE_BYTES);
    }

    /// 実際の derivative 生成（`imaging::render`）の出力を、既存のデコーダーで RGB にして新規 JPEG を作れる。
    /// 後続の handler の使い方（原本／derivative のデコード→RGB→この入口）の確認。圧縮バイト列はそのまま包まない。
    #[test]
    fn the_real_derivative_pipeline_output_can_be_re_encoded_from_its_rgb() {
        let mut src = Vec::new();
        jpeg_encoder::Encoder::new(&mut src, 80)
            .encode(&noise_rgb(200, 150), 200, 150, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let rendered = crate::imaging::render(&src, 0).unwrap();
        let (w, h, rgb_pixels) = decode(&rendered.jpeg);
        assert_eq!((w, h), (200, 150));
        let image = PreparedImage::encode_jpeg_rgb(&rgb_pixels, 200, 150, 82, D).unwrap();
        assert_eq!(image.dimensions(), (200, 150));
        // 送る JPEG は、render が作った JPEG のバイト列とは別物（新しく作り直している）
        assert_ne!(jpeg_of(&image), rendered.jpeg);
        let (rw, rh, _) = decode(&jpeg_of(&image));
        assert_eq!((rw, rh), (200, 150));
    }
}
