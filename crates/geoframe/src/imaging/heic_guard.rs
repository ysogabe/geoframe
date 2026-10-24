//! `zune-heic` に渡す前の、**ファイルが宣言する長さ・寸法**の検査。
//!
//! `zune-heic` は、ファイルの中の宣言値（`mdat` のサイズ、EXIF の extent 長、SPS の寸法、`ispe` の寸法）で
//! 先にメモリを確保してから読み始める。小さなファイルがそれらを偽ると、確保に失敗して**プロセスごと
//! `SIGABRT` で落ちる**（Rust の確保失敗は `panic` ではなく abort で、`catch_unwind` で捕まえられない）。
//! 750 バイトの合成 HEIC の EXIF の長さを 1 GiB にするだけで再現した。そこで、デコーダに渡す前に
//! 宣言値がファイルの実際の大きさに収まるかを自前で確かめ、収まらないものは拒否する。
//!
//! 見る箇所（`zune-heic` 0.5.0 の確保の経路を読んで洗ったもの）:
//!
//! | 確保 | 宣言値 | 検査 |
//! |---|---|---|
//! | `mdat` 本体（`vec![0; size]`、上限 128 MiB） | トップレベルのボックスのサイズ | ボックスがファイルに収まる |
//! | EXIF（`vec![0; length]`） | `iloc` の extent 長 | extent が（`mdat` でなく）ファイル・`idat` に収まる |
//! | alpha などの補助面（`vec![0; w*h]`） | `ispe` の寸法 | 画素数が上限以下（デコード時のみ） |
//! | フレーム・タイルの面（`RawFrame`・`BlockMap`） | SPS の寸法（hvcC と、画像データの中の SPS） | 画素数が上限以下（デコード時のみ） |
//!
//! `ipco` の子ボックス（`colr`・`hvcC`…）の中身は `zune-heic` が自分で 10 MiB に制限する。`iinf`・`iloc`・
//! `iref`・`ipma` の個数は 137 に制限される。いずれも確保の大きさが小さく収まるので、ここでは見ない。
//!
//! 検査の結果は固定の語（[`super::error::HeicFailure::class`]）だけで返す。値は含めない。

use super::heic::{be16, be32, boxes, item_types};

/// `iinf`・`iloc` などの個数の上限（`zune-heic` と同じ）。
const MAX_ITEMS: u32 = 137;

/// 検査する。`decode` が真なら、画素数にかかわる検査（`ispe`・SPS）も行う（ヘッダだけ読むときは不要）。
///
/// 失敗は `class`（固定の語）。`zune-heic` が自分で拒否する不正な入力は、ここでは通してよい。
pub(super) fn check(bytes: &[u8], max_pixels: u64, decode: bool) -> Result<(), &'static str> {
    let top = strict_boxes(bytes)?;
    let Some(meta) = top.iter().find(|b| &b.kind == b"meta") else {
        return Ok(()); // `zune-heic` が「meta が無い」と拒否する
    };
    let Some(meta) = meta.body.get(4..) else {
        return Ok(());
    };
    let children = boxes(meta);
    let find = |kind: &[u8; 4]| children.iter().find(|(k, _)| k == kind).map(|(_, b)| *b);
    let idat = find(b"idat");

    let iloc = match find(b"iloc") {
        Some(body) => parse_iloc(body)?,
        None => Vec::new(),
    };
    // 範囲外の extent があれば、EXIF の長さ（や画像データ）が偽の長さで確保される
    let mut spans: Vec<(u32, Vec<&[u8]>)> = Vec::new();
    for item in &iloc {
        let mut slices = Vec::new();
        for e in &item.extents {
            let start = item.base.checked_add(e.offset).ok_or("extent")?;
            let end = start.checked_add(e.length).ok_or("extent")?;
            let source = match item.construction_method {
                0 => Some(bytes),
                1 => idat,
                _ => None, // `zune-heic` は 2 以上を使うときに拒否する
            };
            let Some(source) = source else { continue };
            let (start, end) = (
                usize::try_from(start).map_err(|_| "extent")?,
                usize::try_from(end).map_err(|_| "extent")?,
            );
            slices.push(source.get(start..end).ok_or("extent")?);
        }
        spans.push((item.id, slices));
    }
    if !decode {
        return Ok(());
    }

    // ispe（補助面の確保）と hvcC の SPS（フレームの確保）
    let ipco = find(b"iprp")
        .map(boxes)
        .and_then(|v| v.into_iter().find(|(k, _)| k == b"ipco"))
        .map(|(_, b)| boxes(b))
        .unwrap_or_default();
    for (kind, body) in &ipco {
        match kind {
            b"ispe" => {
                if let (Some(w), Some(h)) = (be32(body, 4), be32(body, 8)) {
                    too_many(w, h, max_pixels)?;
                }
            }
            b"hvcC" => {
                let mut result = Ok(());
                hvcc_nals(body, |nal| {
                    if result.is_ok() {
                        result = check_nal(nal, max_pixels);
                    }
                });
                result?;
            }
            _ => {}
        }
    }
    // 画像データの中の SPS（デコーダは hvcC のあとに読んだ SPS で寸法を置き換える）
    let types = find(b"iinf").map(item_types).unwrap_or_default();
    for (id, slices) in &spans {
        if !types.iter().any(|(i, t)| i == id && t == b"hvc1") {
            continue;
        }
        let mut result = Ok(());
        for_each_nal(slices, |nal| {
            if result.is_ok() {
                result = check_nal(nal, max_pixels);
            }
        });
        result?;
    }
    Ok(())
}

fn too_many(w: u32, h: u32, max_pixels: u64) -> Result<(), &'static str> {
    if u64::from(w) * u64::from(h) > max_pixels {
        Err("dimensions")
    } else {
        Ok(())
    }
}

/// SPS の NAL（2 バイトのヘッダ付き）なら寸法を検査する。
fn check_nal(nal: &[u8], max_pixels: u64) -> Result<(), &'static str> {
    if nal.len() < 3 || (nal[0] >> 1) & 0x3f != 33 {
        return Ok(());
    }
    match sps_dims(&nal[2..]) {
        Some((w, h)) => too_many(w, h, max_pixels),
        None => Ok(()), // 読めない SPS は `zune-heic` が拒否する
    }
}

// ---------------------------------------------------------------------------
// ボックス

struct Bx<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// トップレベルのボックスの一覧。**宣言のサイズが残りに収まらないものは拒否**する（`mdat` の確保の大きさ）。
/// 末尾の 8 バイト未満の余りは無視する。
fn strict_boxes(mut data: &[u8]) -> Result<Vec<Bx<'_>>, &'static str> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let kind = [data[4], data[5], data[6], data[7]];
        let mut header = 8usize;
        let total = match size {
            0 => data.len(), // 末尾まで
            1 => {
                let big = data.get(8..16).ok_or("box-header")?;
                header = 16;
                usize::try_from(u64::from_be_bytes(
                    big.try_into().map_err(|_| "box-header")?,
                ))
                .map_err(|_| "truncated")?
            }
            n => n as usize,
        };
        if &kind == b"uuid" {
            header += 16;
        }
        if total < header {
            return Err("box-header");
        }
        if total > data.len() {
            return Err("truncated");
        }
        out.push(Bx {
            kind,
            body: &data[header..total],
        });
        data = &data[total..];
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// iloc

struct IlocItem {
    id: u32,
    construction_method: u8,
    base: u64,
    extents: Vec<IlocExtent>,
}

struct IlocExtent {
    offset: u64,
    length: u64,
    /// `iloc` の本体の中での、長さのフィールドの位置と幅（テストが値を差し替える）
    #[cfg_attr(not(test), allow(dead_code))]
    length_field: (usize, usize),
}

/// `iloc` の本体（`zune-heic` と同じ配置）。読めなければ `item-structure`。
fn parse_iloc(body: &[u8]) -> Result<Vec<IlocItem>, &'static str> {
    let bad = || "item-structure";
    let version = *body.first().ok_or_else(bad)?;
    let (b1, b2) = (*body.get(4).ok_or_else(bad)?, *body.get(5).ok_or_else(bad)?);
    let (offset_size, length_size) = (usize::from(b1 >> 4), usize::from(b1 & 0x0f));
    let base_size = usize::from(b2 >> 4);
    let index_size = if version >= 1 {
        usize::from(b2 & 0x0f)
    } else {
        0
    };
    let mut pos = 6usize;

    let sized = |pos: &mut usize, size: usize| -> Result<u64, &'static str> {
        let v = match size {
            0 => 0,
            4 => u64::from(be32(body, *pos).ok_or_else(bad)?),
            8 => u64::from_be_bytes(
                body.get(*pos..*pos + 8)
                    .ok_or_else(bad)?
                    .try_into()
                    .map_err(|_| bad())?,
            ),
            _ => return Err(bad()), // `zune-heic` も 0・4・8 だけ
        };
        *pos += size;
        Ok(v)
    };

    let count = if version < 2 {
        let c = be16(body, pos).ok_or_else(bad)?;
        pos += 2;
        c
    } else {
        let c = be32(body, pos).ok_or_else(bad)?;
        pos += 4;
        c
    };
    if count > MAX_ITEMS {
        return Err(bad());
    }
    let mut items = Vec::new();
    for _ in 0..count {
        let id = if version < 2 {
            let v = be16(body, pos).ok_or_else(bad)?;
            pos += 2;
            v
        } else {
            let v = be32(body, pos).ok_or_else(bad)?;
            pos += 4;
            v
        };
        let construction_method = if version >= 1 {
            let v = (be16(body, pos).ok_or_else(bad)? & 0x0f) as u8;
            pos += 2;
            v
        } else {
            0
        };
        pos += 2; // data_reference_index
        let base = sized(&mut pos, base_size)?;
        let extent_count = be16(body, pos).ok_or_else(bad)?;
        pos += 2;
        let mut extents = Vec::new();
        for _ in 0..extent_count {
            if version >= 1 && index_size > 0 {
                sized(&mut pos, index_size)?;
            }
            let offset = sized(&mut pos, offset_size)?;
            let length_field = (pos, length_size);
            let length = sized(&mut pos, length_size)?;
            extents.push(IlocExtent {
                offset,
                length,
                length_field,
            });
        }
        items.push(IlocItem {
            id,
            construction_method,
            base,
            extents,
        });
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// HEVC の SPS

/// hvcC（`HEVCDecoderConfigurationRecord`）の NAL の配列を順に渡す（NAL は 2 バイトのヘッダ付き）。
fn hvcc_nals(payload: &[u8], mut f: impl FnMut(&[u8])) {
    // 22 バイトの固定部のあとに、配列の数と配列
    let Some(&arrays) = payload.get(22) else {
        return;
    };
    let mut pos = 23usize;
    for _ in 0..arrays {
        let Some(count) = payload.get(pos + 1..pos + 3) else {
            return;
        };
        let count = u16::from_be_bytes([count[0], count[1]]);
        pos += 3;
        for _ in 0..count {
            let Some(len) = payload.get(pos..pos + 2) else {
                return;
            };
            let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
            pos += 2;
            let Some(nal) = payload.get(pos..pos + len) else {
                return;
            };
            f(nal);
            pos += len;
        }
    }
}

/// 画像データ（extent の並び）の NAL を順に渡す。枠の見分け方は `zune-heic` と同じ（最初の extent で決める）。
/// 壊れていれば、そこで止める（`zune-heic` が拒否する）。
fn for_each_nal(extents: &[&[u8]], mut f: impl FnMut(&[u8])) {
    let Some(first) = extents.first() else { return };
    let annex_b = !is_length_prefixed(first)
        && (first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]));
    for extent in extents {
        if annex_b {
            let starts: Vec<usize> = extent
                .windows(3)
                .enumerate()
                .filter(|(_, w)| *w == [0, 0, 1])
                .map(|(i, _)| i + 3)
                .collect();
            for (n, &start) in starts.iter().enumerate() {
                let end = starts.get(n + 1).map_or(extent.len(), |&next| next - 3);
                f(&extent[start..end]);
            }
        } else {
            let mut rest = *extent;
            while let Some((len, tail)) = rest.split_first_chunk::<4>() {
                let len = u32::from_be_bytes(*len) as usize;
                if len > tail.len() {
                    return;
                }
                f(&tail[..len]);
                rest = &tail[len..];
            }
        }
    }
}

/// 4 バイトの長さ付きの NAL だけで過不足なく並んでいるか（`zune-heic` の判定と同じ）。
fn is_length_prefixed(data: &[u8]) -> bool {
    let mut rest = data;
    while !rest.is_empty() {
        let Some((len, tail)) = rest.split_first_chunk::<4>() else {
            return false;
        };
        let len = u32::from_be_bytes(*len) as usize;
        if len < 2 || len > tail.len() || tail[0] & 0x80 != 0 {
            return false;
        }
        rest = &tail[len..];
    }
    true
}

/// SPS（NAL のヘッダの後ろ）の `pic_width_in_luma_samples`・`pic_height_in_luma_samples`。
/// 読めなければ `None`。
fn sps_dims(payload: &[u8]) -> Option<(u32, u32)> {
    // エミュレーション防止バイト（00 00 03 の 03）を除く
    let mut rbsp = Vec::with_capacity(payload.len().min(256));
    let mut zeros = 0;
    for &b in payload.iter().take(256) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    let mut r = Bits {
        data: &rbsp,
        pos: 0,
    };
    r.skip(4)?; // sps_video_parameter_set_id
    let sub_layers = r.read(3)? as usize; // sps_max_sub_layers_minus1
    if sub_layers > 6 {
        return None;
    }
    r.skip(1)?; // temporal_id_nesting
    // profile_tier_level: 一般のプロファイル 88 ビット + レベル 8 ビット
    r.skip(96)?;
    let mut present = [(false, false); 6];
    for p in present.iter_mut().take(sub_layers) {
        *p = (r.read(1)? == 1, r.read(1)? == 1);
    }
    if sub_layers > 0 {
        r.skip(2 * (8 - sub_layers))?;
    }
    for &(profile, level) in present.iter().take(sub_layers) {
        if profile {
            r.skip(88)?;
        }
        if level {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    if r.ue()? == 3 {
        r.skip(1)?; // separate_colour_plane_flag
    }
    let w = r.ue()?;
    let h = r.ue()?;
    Some((w, h))
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let b = *self.data.get(self.pos / 8)?;
        let v = (b >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(v))
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.pos = self.pos.checked_add(n)?;
        (self.pos <= self.data.len() * 8).then_some(())
    }

    /// 指数ゴロム（符号なし）
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.read(zeros)?;
        ((1u64 << zeros) - 1 + u64::from(rest)).try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::{MAX_INPUT_PIXELS, heic};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    const FIXTURES: [&str; 3] = [
        "synthetic_o1.heic",
        "synthetic_o6.heic",
        "synthetic_p3.heic",
    ];

    /// `bytes` の中での `part` の位置（`part` は `bytes` の部分スライス）
    fn pos_of(bytes: &[u8], part: &[u8]) -> usize {
        part.as_ptr() as usize - bytes.as_ptr() as usize
    }

    /// `meta` の子ボックスの本体
    fn meta_child<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
        let top = strict_boxes(bytes).unwrap();
        let meta = top.iter().find(|b| &b.kind == b"meta").unwrap();
        boxes(&meta.body[4..])
            .into_iter()
            .find(|(k, _)| k == kind)
            .unwrap()
            .1
    }

    fn put(bytes: &mut [u8], at: usize, width: usize, value: u64) {
        bytes[at..at + width].copy_from_slice(&value.to_be_bytes()[8 - width..]);
    }

    /// 全部の extent の長さを `value` に書き換えた HEIC（項目 `only` だけを書き換えるなら Some）
    fn with_extent_length(name: &str, only: Option<u32>, value: u64) -> Vec<u8> {
        let mut bytes = fixture(name);
        let body = meta_child(&bytes, b"iloc");
        let base = pos_of(&bytes, body);
        let mut writes = Vec::new();
        for item in parse_iloc(body).unwrap() {
            if only.is_some_and(|id| id != item.id) {
                continue;
            }
            for e in item.extents {
                let (at, width) = e.length_field;
                assert!(width >= 4, "fixture の length_size が 4 未満");
                writes.push((base + at, width));
            }
        }
        assert!(!writes.is_empty());
        for (at, width) in writes {
            put(&mut bytes, at, width, value);
        }
        bytes
    }

    fn exif_item_id(bytes: &[u8]) -> u32 {
        item_types(meta_child(bytes, b"iinf"))
            .into_iter()
            .find(|(_, t)| t == b"Exif")
            .unwrap()
            .0
    }

    // --- 合成の SPS ---

    struct W(Vec<bool>);

    impl W {
        fn bits(&mut self, v: u64, n: usize) {
            for i in (0..n).rev() {
                self.0.push(i < 64 && (v >> i) & 1 == 1);
            }
        }
        fn ue(&mut self, v: u32) {
            let x = u64::from(v) + 1;
            let n = 64 - x.leading_zeros() as usize;
            self.bits(0, n - 1);
            self.bits(x, n);
        }
        fn bytes(&self) -> Vec<u8> {
            self.0
                .chunks(8)
                .map(|c| {
                    c.iter()
                        .enumerate()
                        .fold(0u8, |a, (i, &b)| a | (u8::from(b) << (7 - i)))
                })
                .collect()
        }
    }

    /// SPS の RBSP（NAL のヘッダなし）。`sub_layers` は `sps_max_sub_layers_minus1`
    fn sps_rbsp(w: u32, h: u32, sub_layers: usize, chroma: u32) -> Vec<u8> {
        let mut b = W(Vec::new());
        b.bits(0, 4);
        b.bits(sub_layers as u64, 3);
        b.bits(1, 1);
        b.bits(0, 96);
        for i in 0..sub_layers {
            b.bits(u64::from(i % 2 == 0), 1); // profile あり（0 番目と 2 番目…）
            b.bits(1, 1); // level あり
        }
        if sub_layers > 0 {
            b.bits(0, 2 * (8 - sub_layers));
        }
        for i in 0..sub_layers {
            if i % 2 == 0 {
                b.bits(0, 88);
            }
            b.bits(0, 8);
        }
        b.ue(0);
        b.ue(chroma);
        if chroma == 3 {
            b.bits(0, 1);
        }
        b.ue(w);
        b.ue(h);
        b.bits(1, 1); // 後ろに続くものの代わり
        b.bytes()
    }

    /// エミュレーション防止バイトを入れる
    fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                out.push(3);
                zeros = 0;
            }
            out.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        out
    }

    fn sps_nal(w: u32, h: u32) -> Vec<u8> {
        let mut nal = vec![33 << 1, 1];
        nal.extend(escape(&sps_rbsp(w, h, 0, 1)));
        nal
    }

    // --- テスト ---

    #[test]
    fn 実際の合成heicは検査を通る() {
        for name in FIXTURES {
            let bytes = fixture(name);
            assert_eq!(check(&bytes, MAX_INPUT_PIXELS, true), Ok(()), "{name}");
            assert!(heic::read_header(&bytes).is_ok(), "{name}");
            assert!(heic::decode(&bytes, MAX_INPUT_PIXELS).is_ok(), "{name}");
        }
    }

    #[test]
    fn 実際のhvccのspsから寸法を読める() {
        for name in FIXTURES {
            let bytes = fixture(name);
            let ipco = boxes(
                boxes(meta_child(&bytes, b"iprp"))
                    .iter()
                    .find(|(k, _)| k == b"ipco")
                    .unwrap()
                    .1,
            );
            let (_, hvcc) = ipco.iter().find(|(k, _)| k == b"hvcC").unwrap();
            let mut dims = Vec::new();
            hvcc_nals(hvcc, |nal| {
                if (nal[0] >> 1) & 0x3f == 33 {
                    dims.push(sps_dims(&nal[2..]));
                }
            });
            // 64x48 の画像。符号化の寸法は CTB（64）の倍数に切り上がり、切り落とし（conformance window）で 48 になる
            assert_eq!(dims, vec![Some((64, 64))], "{name}");
        }
    }

    #[test]
    fn exifのextent長を偽ると検査で断る_確保に進まない() {
        // 1 GiB・4 GiB 近く。検査が漏れると 1 GiB の確保で abort する（テストのプロセスごと落ちる）
        for value in [1u64 << 30, u64::from(u32::MAX), 1 << 20] {
            for name in FIXTURES {
                let base = fixture(name);
                let exif = exif_item_id(&base);
                let bytes = with_extent_length(name, Some(exif), value);
                assert_eq!(
                    check(&bytes, MAX_INPUT_PIXELS, false),
                    Err("extent"),
                    "{name} {value}"
                );
                let err = heic::read_header(&bytes).err().unwrap();
                assert!(err.to_string().contains("extent"), "{err}");
                let err = heic::decode(&bytes, MAX_INPUT_PIXELS).err().unwrap();
                assert!(err.to_string().contains("extent"), "{err}");
            }
        }
    }

    #[test]
    fn どの項目のextentでも範囲外は断る() {
        for name in FIXTURES {
            let bytes = with_extent_length(name, None, 1 << 30);
            assert_eq!(check(&bytes, MAX_INPUT_PIXELS, false), Err("extent"));
        }
    }

    #[test]
    fn extentの位置が範囲外でも断る() {
        // 長さは正しいまま、オフセットだけファイルの外へ
        let mut bytes = fixture("synthetic_o1.heic");
        let body = meta_child(&bytes, b"iloc");
        let base = pos_of(&bytes, body);
        let items = parse_iloc(body).unwrap();
        let offset_size = match body[4] >> 4 {
            0 => panic!("offset_size 0"),
            n => usize::from(n),
        };
        // length_field の直前が offset
        let (at, _) = items[0].extents[0].length_field;
        put(
            &mut bytes,
            base + at - offset_size,
            offset_size,
            u64::MAX >> (64 - 8 * offset_size),
        );
        assert_eq!(check(&bytes, MAX_INPUT_PIXELS, false), Err("extent"));
    }

    #[test]
    fn mdatの宣言サイズがファイルを超えるなら断る() {
        for name in FIXTURES {
            let mut bytes = fixture(name);
            let at = {
                let top = strict_boxes(&bytes).unwrap();
                let mdat = top.iter().find(|b| &b.kind == b"mdat").unwrap();
                pos_of(&bytes, mdat.body) - 8
            };
            // 128 MiB（デコーダの上限ちょうど）を宣言する
            put(&mut bytes, at, 4, 128 << 20);
            assert_eq!(check(&bytes, MAX_INPUT_PIXELS, false), Err("truncated"));
            let err = heic::read_header(&bytes).err().unwrap();
            assert!(err.to_string().contains("truncated"), "{err}");
            assert!(heic::decode(&bytes, MAX_INPUT_PIXELS).is_err());
        }
    }

    #[test]
    fn ispeの寸法が画素数の上限を超えるならデコード前に断る() {
        let mut bytes = fixture("synthetic_o1.heic");
        let ipco = boxes(meta_child(&bytes, b"iprp"))
            .into_iter()
            .find(|(k, _)| k == b"ipco")
            .unwrap()
            .1;
        let ispe = boxes(ipco)
            .into_iter()
            .find(|(k, _)| k == b"ispe")
            .unwrap()
            .1;
        let at = pos_of(&bytes, ispe) + 4;
        put(&mut bytes, at, 4, 16_000);
        put(&mut bytes, at + 4, 4, 16_000);
        assert_eq!(check(&bytes, MAX_INPUT_PIXELS, true), Err("dimensions"));
        // ヘッダだけ読むときは見ない（EXIF は読める）
        assert_eq!(check(&bytes, MAX_INPUT_PIXELS, false), Ok(()));
        assert!(heic::decode(&bytes, MAX_INPUT_PIXELS).is_err());
    }

    #[test]
    fn spsの寸法を読む() {
        for (w, h) in [
            (64, 48),
            (1, 1),
            (4032, 3024),
            (65_535, 65_535),
            (16_384, 8),
        ] {
            for sub_layers in [0, 1, 2, 3, 6] {
                for chroma in [0, 1, 3] {
                    let rbsp = sps_rbsp(w, h, sub_layers, chroma);
                    assert_eq!(
                        sps_dims(&rbsp),
                        Some((w, h)),
                        "{w}x{h} {sub_layers} {chroma}"
                    );
                    assert_eq!(sps_dims(&escape(&rbsp)), Some((w, h)), "escaped");
                }
            }
        }
        // 途中で切れていれば読めない
        let rbsp = sps_rbsp(64, 48, 0, 1);
        assert_eq!(sps_dims(&rbsp[..8]), None);
        assert_eq!(sps_dims(&[]), None);
        assert_eq!(sps_dims(&[0xFF; 40]), None);
    }

    fn hvcc_with(nal: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 22];
        p.push(1); // 配列の数
        p.extend([33, 0, 1]); // SPS が 1 つ
        p.extend((nal.len() as u16).to_be_bytes());
        p.extend(nal);
        p
    }

    #[test]
    fn hvccの配列の中の大きすぎるspsを断る() {
        let big = hvcc_with(&sps_nal(60_000, 60_000));
        let mut result = Ok(());
        hvcc_nals(&big, |n| {
            result = result.and(check_nal(n, MAX_INPUT_PIXELS))
        });
        assert_eq!(result, Err("dimensions"));
        let small = hvcc_with(&sps_nal(4032, 3024));
        let mut result = Ok(());
        hvcc_nals(&small, |n| {
            result = result.and(check_nal(n, MAX_INPUT_PIXELS))
        });
        assert_eq!(result, Ok(()));
        // 壊れた配列は止まるだけ
        hvcc_nals(&big[..30], |_| panic!("読めないはず"));
        hvcc_nals(&[], |_| panic!("空"));
    }

    #[test]
    fn 画像データの中のspsも見る_長さ付きとannexb() {
        let sps = sps_nal(60_000, 60_000);
        let other = vec![34 << 1, 1, 0xAA, 0xBB]; // PPS
        let mut lp = Vec::new();
        for nal in [&other, &sps, &other] {
            lp.extend((nal.len() as u32).to_be_bytes());
            lp.extend(nal.iter());
        }
        let mut annex = Vec::new();
        for nal in [&other, &sps, &other] {
            annex.extend([0, 0, 0, 1]);
            annex.extend(nal.iter());
        }
        for data in [lp, annex] {
            let mut types = Vec::new();
            let mut result = Ok(());
            for_each_nal(&[&data], |n| {
                types.push((n[0] >> 1) & 0x3f);
                result = result.and(check_nal(n, MAX_INPUT_PIXELS));
            });
            assert_eq!(types, vec![34, 33, 34]);
            assert_eq!(result, Err("dimensions"));
        }
        // 長さが残りを超える並びは止まる（デコーダが拒否する）
        let mut bad = vec![0, 0, 0xFF, 0xFF];
        bad.extend(&sps);
        for_each_nal(&[&bad], |_| panic!("読めないはず"));
    }

    #[test]
    fn 壊れた入力で検査が落ちない() {
        let base = fixture("synthetic_o1.heic");
        // 1 バイトずつ切ったもの・各バイトを 0xFF にしたもの。どれも止まらず結果を返す
        for n in 0..base.len() {
            let _ = check(&base[..n], MAX_INPUT_PIXELS, true);
        }
        for i in 0..base.len() {
            let mut b = base.clone();
            b[i] = 0xFF;
            let _ = check(&b, MAX_INPUT_PIXELS, true);
        }
    }
}
