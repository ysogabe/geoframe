//! EXIF の読み取り（撮影時刻・GPS・向き）。
//!
//! 現行 Python の `util.extract_exif`（Pillow の `getexif()`）と**同じ結果**になるよう、必要なタグだけを
//! 自前で読む（外部の EXIF crate を使わないのは、Pillow の読み方 — ASCII の末尾の NUL を **1 つだけ**
//! 取る、同じタグは後勝ち、`36867 or 36868 or 306` の連鎖で空文字だけが次へ落ちる — を境界まで
//! 合わせるため）。基準は `tests/fixtures/exif-python.json`（Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_exif_fixture.py` は P5b で削除した。再生成はできない）。
//!
//! TIFF の構造（IFD0 → Exif IFD・GPS IFD）を読むだけで、範囲外・循環・巨大な個数は**黙って捨てる**
//! （Pillow も壊れたタグは警告して飛ばす）。EXIF が読めないことは画像が読めないことではないので、
//! 失敗は「見つからない」として返す。
//!
//! Python と意図して違える点（`docs/rust-migration/server.md` §0・§12）:
//! - GPS が壊れている（成分が 3 つ未満・型が違う）写真: Python は例外で**写真ごと索引しない**
//!   （`index_local` が「読み込み失敗」で飛ばし、毎周期やり直す）。Rust は**位置なし**で索引する。
//! - 分母 0 の有理数: Python は NaN を保存する（SQLite では NULL で、`loc_source='exif'` だけが残る）。
//!   Rust は位置なしにする。
//! - 全角など ASCII 以外の十進数字で書かれた時刻: Python は元の文字のまま `taken_at` に入れる。
//!   Rust は ASCII の数字に直す（`taken_at` は `YYYY-MM-DD HH:MM`。inventory.md §6.1）。

use std::borrow::Cow;

/// 写真から読む位置と撮影時刻（Python の `extract_exif` の返り値）。
#[derive(Debug, Clone, PartialEq)]
pub struct PhotoMeta {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// `YYYY-MM-DD HH:MM`。読めないときは空文字（Python と同じ）。
    pub taken_at: String,
}

/// EXIF から読んだ内容。向きは画像の変換に使う（`PhotoMeta` には入れない）。
#[derive(Debug, Clone, PartialEq)]
pub struct ExifInfo {
    pub meta: PhotoMeta,
    /// 画像に当てる向き（1〜8）。EXIF の Orientation で、無いときは XMP の `tiff:Orientation`
    /// （[`effective_orientation`]）。どちらも無い・範囲外なら 1。
    pub orientation: u8,
}

impl ExifInfo {
    pub fn empty() -> Self {
        Self {
            meta: PhotoMeta {
                lat: None,
                lon: None,
                taken_at: String::new(),
            },
            orientation: 1,
        }
    }
}

// --- Pillow の `ExifTags` ---
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132; // 306（IFD0）
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_XMP: u16 = 700;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003; // 36867
const TAG_DATETIME_DIGITIZED: u16 = 0x9004; // 36868
const GPS_LAT_REF: u16 = 1;
const GPS_LAT: u16 = 2;
const GPS_LON_REF: u16 = 3;
const GPS_LON: u16 = 4;

// TIFF のデータ型
const T_BYTE: u16 = 1;
const T_ASCII: u16 = 2;
const T_SHORT: u16 = 3;
const T_LONG: u16 = 4;
const T_RATIONAL: u16 = 5;
const T_SBYTE: u16 = 6;
const T_SSHORT: u16 = 8;
const T_SLONG: u16 = 9;
const T_SRATIONAL: u16 = 10;

/// 1 つの IFD の中のタグ数の上限（壊れたファイルで巨大な個数を読まない。通常は数十）。
const MAX_ENTRIES: usize = 2048;

struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    /// 値そのもの（4 バイト以内ならエントリ内、そうでなければオフセット先）のバイト列の位置と長さ。
    start: usize,
    len: usize,
}

fn type_size(typ: u16) -> Option<usize> {
    match typ {
        T_BYTE | T_ASCII | T_SBYTE | 7 => Some(1),
        T_SHORT | T_SSHORT => Some(2),
        T_LONG | T_SLONG | 11 => Some(4),
        T_RATIONAL | T_SRATIONAL | 12 => Some(8),
        _ => None,
    }
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let little = match data.get(..4)? {
            b"II*\0" => true,
            b"MM\0*" => false,
            _ => return None,
        };
        Some(Self { data, little })
    }

    fn u16_at(&self, off: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(off..off.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(off..off.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn ifd0_offset(&self) -> Option<usize> {
        usize::try_from(self.u32_at(4)?).ok()
    }

    /// IFD のエントリを読む。値が範囲外のエントリは飛ばす（Pillow は警告して飛ばす）。
    fn entries(&self, ifd: usize) -> Vec<Entry> {
        let Some(n) = self.u16_at(ifd) else {
            return Vec::new();
        };
        let n = usize::from(n).min(MAX_ENTRIES);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let base = ifd + 2 + 12 * i;
            let (Some(tag), Some(typ), Some(count)) = (
                self.u16_at(base),
                self.u16_at(base + 2),
                self.u32_at(base + 4),
            ) else {
                break; // 以降はデータの外
            };
            let Some(size) = type_size(typ) else { continue };
            let Some(len) = size.checked_mul(count as usize) else {
                continue;
            };
            let start = if len <= 4 {
                base + 8
            } else {
                let Some(off) = self.u32_at(base + 8).and_then(|o| usize::try_from(o).ok()) else {
                    continue;
                };
                off
            };
            let Some(end) = start.checked_add(len) else {
                continue;
            };
            if end > self.data.len() {
                continue;
            }
            out.push(Entry {
                tag,
                typ,
                count,
                start,
                len,
            });
        }
        out
    }

    fn bytes(&self, e: &Entry) -> &'a [u8] {
        &self.data[e.start..e.start + e.len]
    }

    /// 整数として読める型の `idx` 番目の値。
    fn uint(&self, e: &Entry, idx: usize) -> Option<u32> {
        if idx >= e.count as usize {
            return None;
        }
        match e.typ {
            T_BYTE => self.data.get(e.start + idx).map(|&b| u32::from(b)),
            T_SHORT => self.u16_at(e.start + 2 * idx).map(u32::from),
            T_LONG => self.u32_at(e.start + 4 * idx),
            _ => None,
        }
    }

    /// 数値として読める型の `idx` 番目の値を `f64` で（Python の `float(v)`。有理数は割り算。分母 0 は NaN）。
    fn number(&self, e: &Entry, idx: usize) -> Option<f64> {
        if idx >= e.count as usize {
            return None;
        }
        match e.typ {
            T_BYTE | T_SHORT | T_LONG => self.uint(e, idx).map(f64::from),
            T_SBYTE => self.data.get(e.start + idx).map(|&b| f64::from(b as i8)),
            T_SSHORT => self.u16_at(e.start + 2 * idx).map(|v| f64::from(v as i16)),
            T_SLONG => self.u32_at(e.start + 4 * idx).map(|v| f64::from(v as i32)),
            T_RATIONAL => {
                let n = self.u32_at(e.start + 8 * idx)?;
                let d = self.u32_at(e.start + 8 * idx + 4)?;
                Some(ratio(f64::from(n), f64::from(d)))
            }
            T_SRATIONAL => {
                let n = self.u32_at(e.start + 8 * idx)? as i32;
                let d = self.u32_at(e.start + 8 * idx + 4)? as i32;
                Some(ratio(f64::from(n), f64::from(d)))
            }
            _ => None,
        }
    }
}

fn ratio(n: f64, d: f64) -> f64 {
    if d == 0.0 { f64::NAN } else { n / d }
}

/// 同じタグが複数あれば後のものが勝つ（Pillow は辞書に入れ直す）。
fn find(entries: &[Entry], tag: u16) -> Option<Entry> {
    entries.iter().rev().find(|e| e.tag == tag).copied()
}

/// Pillow の ASCII の読み方: 末尾の NUL を**1 つだけ**取り、latin-1 として文字にする。
fn ascii_text(tiff: &Tiff, e: &Entry) -> String {
    let b = tiff.bytes(e);
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    b.iter().map(|&c| char::from(c)).collect()
}

/// 時刻の候補 1 つ（Python の `ex.get(tag)`）。`None` は偽値で次の候補へ落ちる（`a or b or c`）。
/// ASCII 以外の型は Pillow が数値などを返すので、真なら文字列としては読めない（`norm_time` は空を返す）。
fn time_candidate(tiff: &Tiff, entries: &[Entry], tag: u16) -> Option<String> {
    let e = find(entries, tag)?;
    if e.typ != T_ASCII {
        return python_truthy(tiff, &e).then(String::new);
    }
    let text = ascii_text(tiff, &e);
    if text.is_empty() { None } else { Some(text) }
}

/// ASCII 以外の型の値が Python で真か。Pillow の読み方: BYTE・UNDEFINED は `bytes`（空でなければ真。
/// 1 バイトの 0 も真）、ほかの型は個数 0 が空のタプル（偽）・1 が数値（0 のとき偽。有理数は 0/1 が偽、
/// 分母 0 は NaN で真）・2 以上がタプル（真）。
fn python_truthy(tiff: &Tiff, e: &Entry) -> bool {
    match (e.typ, e.count) {
        (T_BYTE | 7, n) => n > 0,
        (_, 0) => false,
        (_, 1) => {
            let value = match e.typ {
                11 => tiff.u32_at(e.start).map(|v| f64::from(f32::from_bits(v))),
                12 => tiff
                    .data
                    .get(e.start..e.start + 8)
                    .and_then(|b| <[u8; 8]>::try_from(b).ok())
                    .map(|b| {
                        f64::from_bits(if tiff.little {
                            u64::from_le_bytes(b)
                        } else {
                            u64::from_be_bytes(b)
                        })
                    }),
                _ => tiff.number(e, 0),
            };
            value != Some(0.0)
        }
        _ => true,
    }
}

/// GPS の度分秒を十進にする（Python の `dms_to_deg`）。成分は先頭の 3 つを使う。
/// 数値の型の 2 つ以下・1 つだけのタグは読めない（Python は IndexError・TypeError）ので `None`。
fn dms_to_deg(tiff: &Tiff, e: &Entry, negative: bool) -> Option<f64> {
    if e.count < 3 {
        return None;
    }
    let d = tiff.number(e, 0)?;
    let m = tiff.number(e, 1)?;
    let s = tiff.number(e, 2)?;
    let deg = d + m / 60.0 + s / 3600.0;
    Some(if negative { -deg } else { deg })
}

/// Python の `round(x, 6)`（10 進の正確な丸め。偶数丸め）と同じ値。
fn round6(x: f64) -> f64 {
    // Rust の固定小数点の書式は、2 進の厳密な値を 10 進で偶数丸めする（Python の round と同じ）
    format!("{x:.6}").parse().unwrap_or(x)
}

/// 参照（`N`/`S`/`E`/`W`）が負の側か。Python は lat・lon のどちらも `ref in ("S", "W")` で判定する
/// （緯度の参照が `W` でも負になる）。無いときの既定は `N`/`E`（どちらも正）。
fn is_negative_ref(tiff: &Tiff, gps: &[Entry], tag: u16) -> bool {
    find(gps, tag)
        .is_some_and(|e| e.typ == T_ASCII && matches!(ascii_text(tiff, &e).as_str(), "S" | "W"))
}

fn gps(tiff: &Tiff, gps_entries: &[Entry]) -> (Option<f64>, Option<f64>) {
    // Python: `if gps and 2 in gps and 4 in gps`
    let (Some(lat_e), Some(lon_e)) = (find(gps_entries, GPS_LAT), find(gps_entries, GPS_LON))
    else {
        return (None, None);
    };
    let lat = dms_to_deg(
        tiff,
        &lat_e,
        is_negative_ref(tiff, gps_entries, GPS_LAT_REF),
    );
    let lon = dms_to_deg(
        tiff,
        &lon_e,
        is_negative_ref(tiff, gps_entries, GPS_LON_REF),
    );
    match (lat, lon) {
        (Some(la), Some(lo)) if la.is_finite() && lo.is_finite() => {
            (Some(round6(la)), Some(round6(lo)))
        }
        // 壊れた GPS（成分が足りない・型が違う・分母 0）は位置なし。Python は例外か NaN
        _ => (None, None),
    }
}

/// IFD0 の Orientation タグ。**タグがあれば `Some`**（値が範囲外・読めない型なら 1）。無ければ `None`。
fn orientation_tag(tiff: &Tiff, ifd0: &[Entry]) -> Option<u8> {
    let e = find(ifd0, TAG_ORIENTATION)?;
    Some(
        tiff.uint(&e, 0)
            .and_then(|v| u8::try_from(v).ok())
            .filter(|v| (1..=8).contains(v))
            .unwrap_or(1),
    )
}

/// TIFF 構造の IFD0 に Orientation タグがあるか、あればその値（範囲外は 1）。
pub fn tiff_orientation(data: &[u8]) -> Option<u8> {
    let tiff = Tiff::new(data)?;
    let ifd0 = tiff.entries(tiff.ifd0_offset()?);
    orientation_tag(&tiff, &ifd0)
}

/// 画像に当てる EXIF の向き（1〜8。Pillow の `getexif()` + `exif_transpose` と同じ）。
///
/// 1. IFD0 に Orientation タグがあれば、**値が範囲外でも**それを使う（XMP は見ない）。
/// 2. 無いときだけ、XMP の `tiff:Orientation` を使う（Pillow の `getexif` の補完）。JPEG は APP1 の XMP
///    （複数あれば最後）、PNG は `XML:com.adobe.xmp` のテキスト（`tEXt`・圧縮しない `iTXt`。`zTXt`・
///    圧縮した `iTXt` は読まない）、TIFF はタグ 700。HEIC の XMP は読まない（pillow-heif が開くときに
///    XMP の向きを取り除くので、Pillow も使わない）。
///
/// `tiff` は [`exif_tiff`] で取り出した EXIF の TIFF 構造。
pub fn effective_orientation(format: Format, bytes: &[u8], tiff: Option<&[u8]>) -> u8 {
    if let Some(o) = tiff.and_then(tiff_orientation) {
        return o;
    }
    let xmp = match format {
        Format::Jpeg => jpeg_xmp(bytes),
        Format::Png => png_xmp(bytes),
        Format::Tiff => tiff_xmp(bytes),
        Format::Heic => None,
    };
    xmp.and_then(xmp_orientation)
        .filter(|v| (1..=8).contains(v))
        .unwrap_or(1)
}

/// XMP の最初の `tiff:Orientation="d` か `tiff:Orientation>d`（Pillow の正規表現
/// `tiff:Orientation(="|>)([0-9])` の最初の一致。1 桁だけ読むので `"16"` は 1）。
fn xmp_orientation(xmp: &[u8]) -> Option<u8> {
    const KEY: &[u8] = b"tiff:Orientation";
    let mut from = 0;
    while let Some(i) = xmp[from..].windows(KEY.len()).position(|w| w == KEY) {
        let rest = &xmp[from + i + KEY.len()..];
        let digit = if rest.starts_with(b"=\"") {
            rest.get(2)
        } else if rest.starts_with(b">") {
            rest.get(1)
        } else {
            None
        };
        if let Some(d) = digit.filter(|d| d.is_ascii_digit()) {
            return Some(d - b'0');
        }
        from += i + 1;
    }
    None
}

/// TIFF のタグ 700（XMP）。バイト列の型だけ（Pillow は文字列の型に正規表現のバイト列を当てて失敗する）。
fn tiff_xmp(data: &[u8]) -> Option<&[u8]> {
    let tiff = Tiff::new(data)?;
    let ifd0 = tiff.entries(tiff.ifd0_offset()?);
    let e = find(&ifd0, TAG_XMP)?;
    matches!(e.typ, T_BYTE | 7).then(|| tiff.bytes(&e))
}

/// PNG の `XML:com.adobe.xmp` のテキスト。同じキーが複数あれば後のものが勝つ（Pillow は辞書に入れ直す）。
fn png_xmp(bytes: &[u8]) -> Option<&[u8]> {
    const KEY: &[u8] = b"XML:com.adobe.xmp";
    let mut found = None;
    let mut pos = 8usize;
    while let Some(head) = bytes.get(pos..pos + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind = &head[4..8];
        let Some(body) = bytes.get(pos + 8..pos + 8 + len) else {
            break;
        };
        match kind {
            b"tEXt" => {
                if let Some(value) = body.strip_prefix(KEY).and_then(|r| r.strip_prefix(&[0])) {
                    found = Some(value);
                }
            }
            b"iTXt" => {
                // キー NUL 圧縮フラグ 圧縮方式 言語 NUL 翻訳キーワード NUL テキスト
                if let Some(rest) = body.strip_prefix(KEY).and_then(|r| r.strip_prefix(&[0]))
                    && let [0, _, rest @ ..] = rest
                    && let Some(rest) = rest.splitn(3, |&b| b == 0).nth(2)
                {
                    found = Some(rest);
                }
            }
            b"IEND" => break,
            _ => {}
        }
        pos = match pos.checked_add(len).and_then(|p| p.checked_add(12)) {
            Some(p) => p,
            None => break,
        };
    }
    found
}

/// TIFF 構造のバイト列（`II*\0` か `MM\0*` で始まる）から読む。壊れていれば空の結果。
pub fn parse_tiff(data: &[u8]) -> ExifInfo {
    let Some(tiff) = Tiff::new(data) else {
        return ExifInfo::empty();
    };
    let Some(ifd0_off) = tiff.ifd0_offset() else {
        return ExifInfo::empty();
    };
    let ifd0 = tiff.entries(ifd0_off);
    let sub = |tag: u16| -> Vec<Entry> {
        find(&ifd0, tag)
            .and_then(|e| tiff.uint(&e, 0))
            .and_then(|o| usize::try_from(o).ok())
            .map(|o| tiff.entries(o))
            .unwrap_or_default()
    };
    let exif = sub(TAG_EXIF_IFD);
    let gps_entries = sub(TAG_GPS_IFD);

    // Python: `dt = ex.get(36867) or ex.get(36868) or exif.get(306)`
    let dt = time_candidate(&tiff, &exif, TAG_DATETIME_ORIGINAL)
        .or_else(|| time_candidate(&tiff, &exif, TAG_DATETIME_DIGITIZED))
        .or_else(|| time_candidate(&tiff, &ifd0, TAG_DATETIME));
    let (lat, lon) = gps(&tiff, &gps_entries);
    let orientation = orientation_tag(&tiff, &ifd0).unwrap_or(1);
    ExifInfo {
        meta: PhotoMeta {
            lat,
            lon,
            taken_at: norm_time(dt.as_deref().unwrap_or("")),
        },
        orientation,
    }
}

// --- 時刻の正規化 ---

/// 十進数字（Unicode の Nd）の 0 の位置。Python の正規表現 `\d` は Nd をすべて通すので同じ範囲を読む
/// （`unicodedata` の 15.1 から生成した 68 の連続 10 字）。
const DIGIT_ZEROS: [u32; 68] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0,
    0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950,
    0x1FBF0,
];

/// Unicode の十進数字なら 0〜9 の値。
fn digit_value(c: char) -> Option<u8> {
    if c.is_ascii_digit() {
        return Some(c as u8 - b'0');
    }
    let cp = c as u32;
    if cp < 0x660 {
        return None;
    }
    DIGIT_ZEROS
        .iter()
        .find(|&&z| (z..z + 10).contains(&cp))
        .map(|&z| (cp - z) as u8)
}

/// `'YYYY:MM:DD HH:MM:SS'` などを `'YYYY-MM-DD HH:MM'` に正規化する（Python の `norm_time`）。
/// 先頭から `\d{4}[:-]\d{2}[:-]\d{2}[ T]\d{2}:\d{2}` に合うときだけ（暦の正しさは見ない）。合わなければ空文字。
/// 数字は ASCII に直す（Python は全角などをそのまま返す。モジュールの説明を参照）。
pub fn norm_time(s: &str) -> String {
    let chars: Vec<char> = s.chars().take(16).collect();
    if chars.len() < 16 {
        return String::new();
    }
    let d = |i: usize| digit_value(chars[i]);
    let is = |i: usize, set: &[char]| set.contains(&chars[i]);
    // 0..4 年 / 4 区切り / 5..7 月 / 7 区切り / 8..10 日 / 10 区切り / 11..13 時 / 13 ':' / 14..16 分
    let digit_positions = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15];
    if digit_positions.iter().any(|&i| d(i).is_none())
        || !is(4, &[':', '-'])
        || !is(7, &[':', '-'])
        || !is(10, &[' ', 'T'])
        || !is(13, &[':'])
    {
        return String::new();
    }
    let g = |i: usize| char::from(b'0' + d(i).unwrap_or(0));
    format!(
        "{}{}{}{}-{}{}-{}{} {}{}:{}{}",
        g(0),
        g(1),
        g(2),
        g(3),
        g(5),
        g(6),
        g(8),
        g(9),
        g(11),
        g(12),
        g(14),
        g(15)
    )
}

// --- コンテナから EXIF の TIFF を取り出す ---

/// 画像の種類（内容で決める。拡張子は見ない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Tiff,
    Heic,
}

impl Format {
    /// BigTIFF（`II+\0`・`MM\0+`）か。これは対応しない（[`super::ImageError::BigTiff`]）。
    pub fn is_bigtiff(bytes: &[u8]) -> bool {
        bytes.starts_with(b"II+\0") || bytes.starts_with(b"MM\0+")
    }

    /// 先頭のバイトから種類を決める（Pillow も内容で決める）。
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xFF, 0xD8]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
            Some(Self::Tiff)
        } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && is_heif_brand(&bytes[8..12]) {
            Some(Self::Heic)
        } else {
            None
        }
    }
}

/// HEIC（HEVC）のブランド。AVIF などは対象外。
fn is_heif_brand(brand: &[u8]) -> bool {
    [
        &b"heic"[..],
        b"heix",
        b"hevc",
        b"hevx",
        b"heim",
        b"heis",
        b"mif1",
        b"msf1",
    ]
    .contains(&brand)
}

/// JPEG の SOS より前のセグメント（マーカー、本体）を順に返す。長さを持たないマーカーは飛ばし、
/// 切れていれば（または EOI・SOS に着いたら）そこで終わる。
fn jpeg_segments(bytes: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut pos = 2;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let next = (|| {
            loop {
                // マーカー: 0xFF の連続のあとに種別
                while *bytes.get(pos)? != 0xFF {
                    pos += 1;
                }
                while *bytes.get(pos)? == 0xFF {
                    pos += 1;
                }
                let marker = *bytes.get(pos)?;
                pos += 1;
                match marker {
                    0xD8 | 0x01 | 0xD0..=0xD7 => continue, // 長さを持たない
                    0xD9 | 0xDA => return None, // EOI / SOS（画像データの前までしか探さない）
                    _ => {}
                }
                let len = usize::from(u16::from_be_bytes(
                    bytes.get(pos..pos + 2)?.try_into().ok()?,
                ));
                let body = bytes.get(pos + 2..pos + len)?;
                pos += len;
                return Some((marker, body));
            }
        })();
        done = next.is_none();
        next
    })
}

/// JPEG の最初の `Exif\0` で始まる APP1 の中身（`Exif\0\0` の後ろ）。SOS までの区間だけ見る。
fn jpeg_exif(bytes: &[u8]) -> Option<&[u8]> {
    // `Exif\0\0`（6 バイト）を取る
    jpeg_segments(bytes)
        .find(|(marker, body)| *marker == 0xE1 && body.starts_with(b"Exif\0"))
        .and_then(|(_, body)| body.get(6..))
}

/// JPEG の XMP（`http://ns.adobe.com/xap/1.0/\0` で始まる APP1 の、名前空間の後ろ）。Pillow は
/// 見つけるたびに上書きするので、**最後**のものが勝つ。拡張 XMP（`.../xmp/extension/`）は読まない。
fn jpeg_xmp(bytes: &[u8]) -> Option<&[u8]> {
    const NS: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    jpeg_segments(bytes)
        .filter(|(marker, body)| *marker == 0xE1 && body.starts_with(NS))
        .last()
        .map(|(_, body)| &body[NS.len()..])
}

/// JPEG の最初の SOS のあと（エントロピー符号化されたデータの先頭）から末尾まで。SOS が見つからなければ `None`。
/// 画像データの中の `FF` は `FF00` かリスタートマーカーなので、このあとの `FFD9` は EOI。
pub fn jpeg_scan_data(bytes: &[u8]) -> Option<&[u8]> {
    let mut pos = 2;
    loop {
        while *bytes.get(pos)? != 0xFF {
            pos += 1;
        }
        while *bytes.get(pos)? == 0xFF {
            pos += 1;
        }
        let marker = *bytes.get(pos)?;
        pos += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes(
            bytes.get(pos..pos + 2)?.try_into().ok()?,
        ));
        let end = pos.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        if marker == 0xDA {
            return bytes.get(end..);
        }
        pos = end;
    }
}

/// PNG の `eXIf` チャンクの中身（TIFF 構造）。
fn png_exif(bytes: &[u8]) -> Option<&[u8]> {
    let mut pos = 8;
    loop {
        let len = usize::try_from(u32::from_be_bytes(
            bytes.get(pos..pos + 4)?.try_into().ok()?,
        ))
        .ok()?;
        let kind = bytes.get(pos + 4..pos + 8)?;
        let body = bytes.get(pos + 8..pos + 8 + len)?;
        match kind {
            b"eXIf" => return Some(body),
            b"IEND" => return None,
            _ => {}
        }
        pos = pos.checked_add(len)?.checked_add(12)?; // 長さ + 種別 + 本体 + CRC
    }
}

/// 画像のバイト列から EXIF の TIFF 構造を取り出す。HEIC は `zune-heic` が取り出したものを渡す
/// （`heic_exif`。ファイルの構造の読み方はそちらに任せる）。
pub fn exif_tiff<'a>(
    format: Format,
    bytes: &'a [u8],
    heic_exif: Option<&'a [u8]>,
) -> Option<Cow<'a, [u8]>> {
    match format {
        Format::Jpeg => jpeg_exif(bytes).map(Cow::Borrowed),
        Format::Png => png_exif(bytes).map(Cow::Borrowed),
        Format::Tiff => Some(Cow::Borrowed(bytes)),
        Format::Heic => heic_exif.map(Cow::Borrowed),
    }
}

/// `Exif\0\0` が付いていれば取り除く（HEIC の Exif ブロックの形の揺れを吸収する）。
pub fn strip_exif_prefix(data: &[u8]) -> &[u8] {
    data.strip_prefix(b"Exif\0\0").unwrap_or(data)
}
