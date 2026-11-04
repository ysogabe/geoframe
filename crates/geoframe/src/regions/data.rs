//! 同梱の境界データの形式の定義と、読み込み・検証。判定のロジックは持たない（[`super::index`]）。
//!
//! # 成果物（`crates/geoframe/data/regions/`）
//!
//! |ファイル|内容|
//! |---|---|
//! |`manifest.json`|元データの URL・版・SHA256、変換器の版、成果物の SHA256・件数・サイズ上限|
//! |`polygons.bin.gz`|国・地域のポリゴン本体（gzip。下の形式）|
//! |`names.json.gz`|ID→名称表（gzip の JSON）。読み取り API と検索の別名に使う小さな表|
//! |`SOURCES.md`|出典と利用条件の説明（埋め込まない）|
//!
//! # `polygons.bin`（gzip を展開した後。整数はすべてリトルエンディアンの LEB128 可変長整数）
//!
//! ```text
//! magic    "GFREGN01"（8 バイト。末尾 2 文字が形式の版）
//! n_countries, n_regions
//! 国 n_countries 件、続けて地域 n_regions 件。どちらも ID の昇順（names.json と同じ並び）:
//!   id_len, id（UTF-8）
//!   地域だけ: country（国の添字。0 起点で国の並びの位置）
//!   n_polygons
//!   多角形ごと: n_rings（先頭が外側の輪、残りが穴）
//!     輪ごと: n_points（閉じるための末尾の点を含む。元データのまま）
//!       点ごと: dx, dy
//! ```
//!
//! 座標は IEEE 754 倍精度のビット列を符号付き 64 ビット整数とみなし、直前の点（ファイル全体で連続。先頭は 0）との
//! 差を取ってジグザグ変換した値が `dx`・`dy`（x=経度, y=緯度）。丸めも簡略化もしないので、復号した値は
//! 元の shapefile とビット単位で一致する。
//!
//! 実行時は形式の版・件数・展開後のサイズ（manifest の上限と一致）を確認する。全データのハッシュは取らない
//! （manifest との一致は [`verify_artifacts`] を `cargo test` と更新時に回す）。
//!
//! 復号は manifest の件数（国・地域の件数、多角形・輪・頂点の総数）を上限として受け取り（[`DecodeLimits`]）、
//! `Vec` を確保する前に、宣言された個数が残りの上限と残りのバイト数の両方に収まることを確かめる。
//! 復号は gzip を展開しながら小さな窓で読み（[`decode_polygons_from`]）、展開後の全体を 1 つのバッファに持たない。
//! 結果は入れ子の `Vec` ではなく、座標 1 本とオフセット表の平らな配列（[`Layer`]）。
//! ID は長さ 1〜64・文字種（ASCII 英数字と `:_-`）を、多角形は 1 つ以上の輪を持つことを、読んだ時点で確かめる。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use serde::Deserialize;

/// 形式の版。`polygons.bin` の magic と `names.json`・manifest の `format` に一致すること。
pub const FORMAT_VERSION: u32 = 1;
/// `polygons.bin` の先頭 8 バイト。
pub const MAGIC: &[u8; 8] = b"GFREGN01";
/// 日本の国 ID。
pub const JAPAN_ID: &str = "ne:JPN";

const MANIFEST_JSON: &str = include_str!("../../data/regions/manifest.json");
static POLYGONS_GZ: &[u8] = include_bytes!("../../data/regions/polygons.bin.gz");
static NAMES_GZ: &[u8] = include_bytes!("../../data/regions/names.json.gz");

/// 同梱データの読み込み・検証の失敗。メッセージは固定の理由だけで、データの中身を含めない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError(pub String);

impl DataError {
    fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "境界データ: {}", self.0)
    }
}

impl std::error::Error for DataError {}

// ---------------------------------------------------------------- manifest

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub source: Source,
    pub converter: Converter,
    pub counts: Counts,
    pub artifacts: Artifacts,
    pub max_total_bytes: u64,
    pub dataset_sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub name: String,
    pub data_version: String,
    pub license: String,
    pub terms_url: String,
    pub git_repo: String,
    pub git_tag: String,
    pub git_commit: String,
    pub layers: HashMap<String, SourceLayer>,
}

#[derive(Debug, Deserialize)]
pub struct SourceLayer {
    pub stem: String,
    pub files: HashMap<String, SourceFile>,
}

#[derive(Debug, Deserialize)]
pub struct SourceFile {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
pub struct Converter {
    pub script: String,
    pub version: String,
    pub pyshp: String,
}

#[derive(Debug, Deserialize)]
pub struct Counts {
    pub countries: LayerCounts,
    pub regions: LayerCounts,
    pub excluded_admin1: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct LayerCounts {
    pub features: usize,
    pub polygons: usize,
    pub rings: usize,
    pub holes: usize,
    pub vertices: usize,
}

#[derive(Debug, Deserialize)]
pub struct Artifacts {
    pub polygons: Artifact,
    pub names: Artifact,
}

#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub raw_bytes: u64,
    pub raw_sha256: String,
    pub max_bytes: u64,
    pub max_raw_bytes: u64,
}

/// 埋め込んだ manifest。壊れていれば（`cargo test` で検出される）エラー。
pub fn manifest() -> Result<&'static Manifest, DataError> {
    static CELL: OnceLock<Result<Manifest, DataError>> = OnceLock::new();
    CELL.get_or_init(|| {
        let m: Manifest = serde_json::from_str(MANIFEST_JSON)
            .map_err(|_| DataError::new("manifest を読めない"))?;
        if m.format != FORMAT_VERSION {
            return Err(DataError::new("manifest の形式の版が違う"));
        }
        Ok(m)
    })
    .as_ref()
    .map_err(Clone::clone)
}

// ---------------------------------------------------------------- 展開

/// gzip を展開する。展開後が `max_raw` 、または `expected_raw` を超える・満たさないものは拒否する（展開爆弾の防止）。
fn gunzip(gz: &[u8], expected_raw: u64, max_raw: u64) -> Result<Vec<u8>, DataError> {
    if expected_raw > max_raw {
        return Err(DataError::new("展開後のサイズが上限を超える"));
    }
    let mut out = Vec::with_capacity(expected_raw as usize);
    // 上限 +1 バイトまで読み、超えたら拒否する
    GzDecoder::new(gz)
        .take(max_raw + 1)
        .read_to_end(&mut out)
        .map_err(|_| DataError::new("gzip を展開できない"))?;
    if out.len() as u64 != expected_raw {
        return Err(DataError::new("展開後のサイズが manifest と違う"));
    }
    Ok(out)
}

pub(super) fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    let mut s = String::with_capacity(64);
    for b in digest.as_ref() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 埋め込んだ成果物が manifest の SHA256・サイズ・上限と一致するか。全データをハッシュするので、実行のたびには呼ばない
/// （`cargo test` と、データの更新を確かめるときに使う）。
pub fn verify_artifacts() -> Result<(), DataError> {
    let m = manifest()?;
    for (art, gz) in [
        (&m.artifacts.polygons, POLYGONS_GZ),
        (&m.artifacts.names, NAMES_GZ),
    ] {
        if gz.len() as u64 != art.bytes || sha256_hex(gz) != art.sha256 {
            return Err(DataError::new(
                "埋め込んだ成果物が manifest の SHA256 と一致しない",
            ));
        }
        if art.bytes > art.max_bytes {
            return Err(DataError::new("成果物が manifest のサイズ上限を超える"));
        }
        let raw = gunzip(gz, art.raw_bytes, art.max_raw_bytes)?;
        if sha256_hex(&raw) != art.raw_sha256 {
            return Err(DataError::new("展開後の SHA256 が manifest と一致しない"));
        }
    }
    if m.artifacts.polygons.bytes + m.artifacts.names.bytes > m.max_total_bytes {
        return Err(DataError::new(
            "成果物の合計が manifest のサイズ上限を超える",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- 名称表

/// 国または地域の 1 件。
#[derive(Debug, Clone, Deserialize)]
pub struct Place {
    pub id: String,
    /// 地域だけ。所属する国の ID。
    #[serde(default)]
    pub country: Option<String>,
    /// 表示名（日本語→英語→原表記の順に補完済み。空にならない）。
    pub name: String,
    /// 検索用の表記（日本語・英語・原表記・別名。表示名を含む。重複なし）。
    pub search: Vec<String>,
    /// 国だけ。ISO 3166-1 alpha-2（得られる場合）。
    #[serde(default)]
    pub iso2: Option<String>,
    /// 地域だけ。ISO 3166-2（得られる場合）。
    #[serde(default)]
    pub iso: Option<String>,
    /// 日本の地域だけ。写真の `pref` 列の表記（47 都道府県）。
    #[serde(default)]
    pub pref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawNames {
    format: u32,
    countries: Vec<Place>,
    regions: Vec<Place>,
}

/// ID→名称表。読み取り API が使う小さな表で、ポリゴンの展開は要らない。
#[derive(Debug)]
pub struct NameTable {
    pub countries: Vec<Place>,
    pub regions: Vec<Place>,
    country_index: HashMap<String, usize>,
    region_index: HashMap<String, usize>,
    pref_index: HashMap<String, usize>,
}

impl NameTable {
    pub fn country(&self, id: &str) -> Option<&Place> {
        self.country_index.get(id).map(|&i| &self.countries[i])
    }

    pub fn region(&self, id: &str) -> Option<&Place> {
        self.region_index.get(id).map(|&i| &self.regions[i])
    }

    /// 写真の `pref` 列の表記（例「東京都」）から、日本の地域を引く。
    pub fn region_of_pref(&self, pref: &str) -> Option<&Place> {
        self.pref_index.get(pref).map(|&i| &self.regions[i])
    }

    /// 日本の都道府県（47 件。JIS の順ではなく ID の順）。
    pub fn prefectures(&self) -> impl Iterator<Item = &Place> {
        self.regions.iter().filter(|r| r.pref.is_some())
    }
}

/// ID の最大の長さ（バイト）。
pub const MAX_ID_LEN: usize = 64;

/// ID の文字種と長さ（管理 API の入力検査と同じ）。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

fn build_names(raw: RawNames, m: &Manifest) -> Result<NameTable, DataError> {
    if raw.format != FORMAT_VERSION {
        return Err(DataError::new("名称表の形式の版が違う"));
    }
    if raw.countries.len() != m.counts.countries.features
        || raw.regions.len() != m.counts.regions.features
    {
        return Err(DataError::new("名称表の件数が manifest と違う"));
    }
    let mut country_index = HashMap::new();
    for (i, c) in raw.countries.iter().enumerate() {
        check_place(c)?;
        if c.country.is_some() || c.pref.is_some() {
            return Err(DataError::new("国に地域の項目がある"));
        }
        if country_index.insert(c.id.clone(), i).is_some() {
            return Err(DataError::new("国の ID が重複している"));
        }
    }
    let mut region_index = HashMap::new();
    let mut pref_index = HashMap::new();
    for (i, r) in raw.regions.iter().enumerate() {
        check_place(r)?;
        match &r.country {
            Some(c) if country_index.contains_key(c) => {}
            _ => return Err(DataError::new("地域の国が名称表に無い")),
        }
        if region_index.insert(r.id.clone(), i).is_some() {
            return Err(DataError::new("地域の ID が重複している"));
        }
        if let Some(pref) = &r.pref
            && (r.country.as_deref() != Some(JAPAN_ID)
                || pref_index.insert(pref.clone(), i).is_some())
        {
            return Err(DataError::new("都道府県の対応が不正"));
        }
    }
    Ok(NameTable {
        countries: raw.countries,
        regions: raw.regions,
        country_index,
        region_index,
        pref_index,
    })
}

fn check_place(p: &Place) -> Result<(), DataError> {
    if !valid_id(&p.id) || !p.id.starts_with("ne:") {
        return Err(DataError::new("使えない ID がある"));
    }
    if p.name.trim().is_empty() || p.search.iter().any(|s| s.trim().is_empty()) {
        return Err(DataError::new("空の名称がある"));
    }
    if !p.search.contains(&p.name) {
        return Err(DataError::new("検索用の表記に表示名が無い"));
    }
    let unique: HashSet<&String> = p.search.iter().collect();
    if unique.len() != p.search.len() {
        return Err(DataError::new("検索用の表記が重複している"));
    }
    Ok(())
}

/// 埋め込んだ名称表（初回に展開・検証して保持する。小さいので常駐してよい）。
pub fn names() -> Result<&'static NameTable, DataError> {
    static CELL: OnceLock<Result<NameTable, DataError>> = OnceLock::new();
    CELL.get_or_init(|| {
        let m = manifest()?;
        let a = &m.artifacts.names;
        let raw = gunzip(NAMES_GZ, a.raw_bytes, a.max_raw_bytes)?;
        let parsed: RawNames =
            serde_json::from_slice(&raw).map_err(|_| DataError::new("名称表を読めない"))?;
        build_names(parsed, m)
    })
    .as_ref()
    .map_err(Clone::clone)
}

// ---------------------------------------------------------------- ポリゴン

/// 1 層（国または地域）の地物を、平らな配列にまとめたもの。
///
/// 入れ子の `Vec` にすると、輪・多角形ごとの確保の見出しだけで数十 MB になるので、座標は 1 本の配列に並べ、
/// 地物・多角形・輪の区切りは **先頭に 0 を置いた終端のオフセット表**（長さ = 件数 + 1）で持つ。
///
/// ```text
/// 地物 f の多角形  = feature_polys[f] .. feature_polys[f+1]   （多角形の通し番号）
/// 多角形 p の輪    = poly_rings[p]    .. poly_rings[p+1]      （輪の通し番号。先頭が外側、残りが穴）
/// 輪 r の点        = ring_pts[r]      .. ring_pts[r+1]        （pts の添字。閉じるための末尾の点を含む）
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    ids: Vec<String>,
    /// 地域だけ。所属する国の添字（国の層での位置）。国の層では空。
    parents: Vec<u32>,
    feature_polys: Vec<u32>,
    poly_rings: Vec<u32>,
    ring_pts: Vec<u32>,
    /// `[経度, 緯度]`。
    pts: Vec<[f64; 2]>,
}

/// 層の確保の見積もり（再確保を避けるため）。
#[derive(Debug, Clone, Copy, Default)]
struct LayerCapacity {
    features: usize,
    polygons: usize,
    rings: usize,
    points: usize,
}

impl Layer {
    /// 見積もりの分を確保する。オフセット表は `u32` なので、各件数が `u32` に収まること、`+ 1`（先頭の 0）の加算が
    /// あふれないこと、確保に失敗しないこと（`try_reserve_exact`）を確かめ、どれかが外れたら `Err`（パニック・中断をしない）。
    fn try_with_capacity(c: LayerCapacity, regions: bool) -> Result<Self, DataError> {
        fn reserved<T>(n: usize, extra: usize) -> Result<Vec<T>, DataError> {
            let n = n
                .checked_add(extra)
                .filter(|&n| n <= u32::MAX as usize)
                .ok_or_else(|| DataError::new("件数が大きすぎる"))?;
            let mut v = Vec::new();
            v.try_reserve_exact(n)
                .map_err(|_| DataError::new("確保できない大きさの件数"))?;
            Ok(v)
        }
        let mut feature_polys = reserved(c.features, 1)?;
        feature_polys.push(0);
        let mut poly_rings = reserved(c.polygons, 1)?;
        poly_rings.push(0);
        let mut ring_pts = reserved(c.rings, 1)?;
        ring_pts.push(0);
        Ok(Self {
            ids: reserved(c.features, 0)?,
            parents: if regions {
                reserved(c.features, 0)?
            } else {
                Vec::new()
            },
            feature_polys,
            poly_rings,
            ring_pts,
            pts: reserved(c.points, 0)?,
        })
    }

    /// 空の層。試験の合成データが `begin_feature` などで組み立てる。
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self::try_with_capacity(LayerCapacity::default(), false).expect("空の確保")
    }

    /// 地物の数。
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn id(&self, feature: usize) -> &str {
        &self.ids[feature]
    }

    /// 地域の所属する国（国の層での添字）。国の層では `None`。
    pub fn parent(&self, feature: usize) -> Option<usize> {
        self.parents.get(feature).map(|&p| p as usize)
    }

    pub fn polygons_len(&self) -> usize {
        self.poly_rings.len() - 1
    }

    pub fn rings_len(&self) -> usize {
        self.ring_pts.len() - 1
    }

    pub fn points_len(&self) -> usize {
        self.pts.len()
    }

    /// 地物 `feature` の多角形の通し番号の範囲。
    pub fn polygon_range(&self, feature: usize) -> std::ops::Range<usize> {
        self.feature_polys[feature] as usize..self.feature_polys[feature + 1] as usize
    }

    /// 多角形 `polygon` の輪の通し番号の範囲（先頭が外側、残りが穴）。
    pub fn ring_range(&self, polygon: usize) -> std::ops::Range<usize> {
        self.poly_rings[polygon] as usize..self.poly_rings[polygon + 1] as usize
    }

    /// 輪 `ring` の点列（閉じるための末尾の点を含む）。
    pub fn ring(&self, ring: usize) -> &[[f64; 2]] {
        &self.pts[self.ring_pts[ring] as usize..self.ring_pts[ring + 1] as usize]
    }

    /// 全部の輪（点列）。
    pub fn rings(&self) -> impl Iterator<Item = &[[f64; 2]]> {
        (0..self.rings_len()).map(|r| self.ring(r))
    }

    /// 地物 `feature` の全部の輪。
    pub fn feature_rings(&self, feature: usize) -> impl Iterator<Item = &[[f64; 2]]> {
        self.polygon_range(feature)
            .flat_map(|p| self.ring_range(p))
            .map(|r| self.ring(r))
    }

    // ---- 組み立て（復号と、試験用の合成データ）

    fn offset(n: usize) -> Result<u32, DataError> {
        u32::try_from(n).map_err(|_| DataError::new("頂点の数が大きすぎる"))
    }

    /// 地物を始める。`parent` は地域の層だけ（国の層の添字）。
    pub(super) fn begin_feature(&mut self, id: String, parent: Option<usize>) {
        self.ids.push(id);
        if let Some(p) = parent {
            self.parents.push(p as u32);
        }
    }

    pub(super) fn push_point(&mut self, x: f64, y: f64) {
        self.pts.push([x, y]);
    }

    /// 今の輪を閉じる。4 点以上で、先頭と末尾が同じこと。
    pub(super) fn end_ring(&mut self) -> Result<(), DataError> {
        let start = *self.ring_pts.last().expect("先頭の 0") as usize;
        let ring = &self.pts[start..];
        if ring.len() < 4 {
            return Err(DataError::new("輪の点が 4 未満"));
        }
        if ring.first() != ring.last() {
            return Err(DataError::new("輪が閉じていない"));
        }
        self.ring_pts.push(Self::offset(self.pts.len())?);
        Ok(())
    }

    /// 今の多角形を閉じる。輪が 1 つ以上あること。
    pub(super) fn end_polygon(&mut self) -> Result<(), DataError> {
        let before = *self.poly_rings.last().expect("先頭の 0") as usize;
        if self.rings_len() == before {
            return Err(DataError::new("輪が無い多角形"));
        }
        self.poly_rings.push(Self::offset(self.rings_len())?);
        Ok(())
    }

    /// 今の地物を閉じる。多角形が 1 つ以上あること。
    pub(super) fn end_feature(&mut self) -> Result<(), DataError> {
        let before = *self.feature_polys.last().expect("先頭の 0") as usize;
        if self.polygons_len() == before {
            return Err(DataError::new("多角形が無い地物"));
        }
        self.feature_polys.push(Self::offset(self.polygons_len())?);
        Ok(())
    }

    /// 余分な確保を返す（復号の見積もりが上限いっぱいのとき）。
    fn shrink(&mut self) {
        self.pts.shrink_to_fit();
        self.ring_pts.shrink_to_fit();
        self.poly_rings.shrink_to_fit();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolygonSet {
    pub countries: Layer,
    pub regions: Layer,
}

/// 展開済みのバイト列を、一定の大きさの窓で読む（全体を持たない）。`total` は読むはずの全体のバイト数で、
/// 個数が残りのバイト数に収まるかの検査に使う。
struct Cursor<R: Read> {
    src: R,
    buf: Vec<u8>,
    pos: usize,
    filled: usize,
    /// `buf` の先頭より前に読み終えたバイト数。
    base: u64,
    total: u64,
}

const WINDOW: usize = 64 * 1024;

impl<R: Read> Cursor<R> {
    fn new(src: R, total: u64) -> Self {
        Self {
            src,
            buf: vec![0; WINDOW],
            pos: 0,
            filled: 0,
            base: 0,
            total,
        }
    }

    /// 窓を読み直す。もう読めなければ `false`。
    fn fill(&mut self) -> Result<bool, DataError> {
        self.base += self.filled as u64;
        self.pos = 0;
        self.filled = 0;
        loop {
            match self.src.read(&mut self.buf) {
                Ok(n) => {
                    self.filled = n;
                    return Ok(n > 0);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(DataError::new("gzip を展開できない")),
            }
        }
    }

    fn position(&self) -> u64 {
        self.base + self.pos as u64
    }

    fn remaining(&self) -> u64 {
        self.total.saturating_sub(self.position())
    }

    fn byte(&mut self) -> Result<u8, DataError> {
        if self.pos == self.filled && !self.fill()? {
            return Err(DataError::new("ポリゴンが途中で終わっている"));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64, DataError> {
        let mut n: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift >= 64 || (shift == 63 && b > 1) {
                return Err(DataError::new("可変長整数が大きすぎる"));
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
    }

    /// 個数。残りのバイト数より多い個数は、確保する前に拒否する（壊れたデータでの巨大な確保を避ける）。
    fn count(&mut self, min_bytes_each: usize) -> Result<usize, DataError> {
        let n = usize::try_from(self.varint()?).map_err(|_| DataError::new("個数が大きすぎる"))?;
        let need = (n as u64).checked_mul(min_bytes_each as u64);
        if need.is_none_or(|need| need > self.remaining()) {
            return Err(DataError::new("個数が残りのデータより多い"));
        }
        Ok(n)
    }

    /// 個数。`count` の検査に加えて、残りの上限 `budget`（manifest の件数から使った分を引いたもの）を超えるものも拒否する。
    fn count_within(
        &mut self,
        min_bytes_each: usize,
        budget: &mut usize,
    ) -> Result<usize, DataError> {
        let n = self.count(min_bytes_each)?;
        if n > *budget {
            return Err(DataError::new("個数が manifest の件数を超える"));
        }
        *budget -= n;
        Ok(n)
    }

    /// `n` バイトを読む（呼び手が `n` を小さく抑えてから呼ぶ。ID と magic だけ）。
    fn take(&mut self, n: usize) -> Result<Vec<u8>, DataError> {
        if n as u64 > self.remaining() {
            return Err(DataError::new("ポリゴンが途中で終わっている"));
        }
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if self.pos == self.filled && !self.fill()? {
                return Err(DataError::new("ポリゴンが途中で終わっている"));
            }
            let k = (n - out.len()).min(self.filled - self.pos);
            out.extend_from_slice(&self.buf[self.pos..self.pos + k]);
            self.pos += k;
        }
        Ok(out)
    }

    /// 全部読み終えたこと（`total` ちょうどで、その先が無いこと）を確かめる。gzip の末尾の CRC もここで検査される。
    fn finish(&mut self) -> Result<(), DataError> {
        if self.pos != self.filled || self.fill()? {
            return Err(DataError::new("ポリゴンの末尾に余りがある"));
        }
        if self.position() != self.total {
            return Err(DataError::new("展開後のサイズが manifest と違う"));
        }
        Ok(())
    }
}

fn unzigzag(z: u64) -> i64 {
    ((z >> 1) as i64) ^ -((z & 1) as i64)
}

/// 復号で受け入れる件数の上限。壊れたデータが入力長には収まる巨大な個数を宣言しても、確保の前に拒否するための値で、
/// 通常は manifest の件数（[`DecodeLimits::from_manifest`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub countries: LayerLimits,
    pub regions: LayerLimits,
}

/// 層（国または地域）ごとの上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerLimits {
    pub features: usize,
    pub polygons: usize,
    pub rings: usize,
    pub vertices: usize,
}

impl From<&LayerCounts> for LayerLimits {
    fn from(c: &LayerCounts) -> Self {
        Self {
            features: c.features,
            polygons: c.polygons,
            rings: c.rings,
            vertices: c.vertices,
        }
    }
}

impl DecodeLimits {
    pub fn from_manifest(m: &Manifest) -> Self {
        Self {
            countries: (&m.counts.countries).into(),
            regions: (&m.counts.regions).into(),
        }
    }
}

/// 展開済みの `polygons.bin` を復号する。件数は `limits`（通常は manifest の件数）を超えるものを、確保の前に拒否する。
/// 座標が有限なこと、輪が 4 点以上で閉じていること、ID の長さと文字種、多角形が輪を持つことまで確かめる。
pub fn decode_polygons(raw: &[u8], limits: &DecodeLimits) -> Result<PolygonSet, DataError> {
    decode_polygons_from(raw, raw.len() as u64, limits)
}

/// [`decode_polygons`] の流し込み版。`src` は展開済みの `polygons.bin` を `total` バイト返す読み手で、
/// 展開後の全体を 1 つのバッファに持たずに、小さな窓で読みながら復号する。
pub fn decode_polygons_from<R: Read>(
    src: R,
    total: u64,
    limits: &DecodeLimits,
) -> Result<PolygonSet, DataError> {
    let mut c = Cursor::new(src, total);
    if c.take(MAGIC.len())? != MAGIC {
        return Err(DataError::new("ポリゴンの magic が違う"));
    }
    let mut features_budget = (limits.countries.features, limits.regions.features);
    let n_countries = c.count_within(3, &mut features_budget.0)?;
    let n_regions = c.count_within(3, &mut features_budget.1)?;
    let (mut px, mut py) = (0i64, 0i64);
    let mut layers: Vec<Layer> = Vec::with_capacity(2);
    for (is_region, n, lim) in [
        (false, n_countries, &limits.countries),
        (true, n_regions, &limits.regions),
    ] {
        // 件数の上限（manifest）と、入力に収まる最大の件数（多角形・輪は 1 バイト、点は 2 バイト以上）の小さい方で見積もる
        let cap = |limit: usize, min_bytes: u64| {
            limit.min(usize::try_from(total / min_bytes).unwrap_or(usize::MAX))
        };
        // オフセット表（`u32`）に表せない件数の上限は、確保の前に拒否する
        if [lim.features, lim.polygons, lim.rings, lim.vertices]
            .iter()
            .any(|&n| n >= u32::MAX as usize)
        {
            return Err(DataError::new("件数の上限が大きすぎる"));
        }
        let mut layer = Layer::try_with_capacity(
            LayerCapacity {
                features: n,
                polygons: cap(lim.polygons, 1),
                rings: cap(lim.rings, 1),
                points: cap(lim.vertices, 2),
            },
            is_region,
        )?;
        let (mut polygons_left, mut rings_left, mut vertices_left) =
            (lim.polygons, lim.rings, lim.vertices);
        for _ in 0..n {
            let id_len = c.count(1)?;
            if id_len == 0 || id_len > MAX_ID_LEN {
                return Err(DataError::new("ID の長さが不正"));
            }
            let id = String::from_utf8(c.take(id_len)?)
                .ok()
                .filter(|s| valid_id(s))
                .ok_or_else(|| DataError::new("ID に使えない文字がある"))?;
            let country = if is_region {
                let i = usize::try_from(c.varint()?)
                    .map_err(|_| DataError::new("国の添字が大きすぎる"))?;
                if i >= n_countries {
                    return Err(DataError::new("国の添字が範囲外"));
                }
                Some(i)
            } else {
                None
            };
            let n_polygons = c.count_within(1, &mut polygons_left)?;
            if n_polygons == 0 {
                return Err(DataError::new("多角形が無い地物"));
            }
            layer.begin_feature(id, country);
            for _ in 0..n_polygons {
                let n_rings = c.count_within(1, &mut rings_left)?;
                if n_rings == 0 {
                    return Err(DataError::new("輪が無い多角形"));
                }
                for _ in 0..n_rings {
                    let n_points = c.count_within(2, &mut vertices_left)?;
                    if n_points < 4 {
                        return Err(DataError::new("輪の点が 4 未満"));
                    }
                    for _ in 0..n_points {
                        px = px.wrapping_add(unzigzag(c.varint()?));
                        py = py.wrapping_add(unzigzag(c.varint()?));
                        let (x, y) = (f64::from_bits(px as u64), f64::from_bits(py as u64));
                        if !x.is_finite() || !y.is_finite() {
                            return Err(DataError::new("有限でない座標"));
                        }
                        layer.push_point(x, y);
                    }
                    layer.end_ring()?;
                }
                layer.end_polygon()?;
            }
            layer.end_feature()?;
        }
        layer.shrink();
        layers.push(layer);
    }
    c.finish()?;
    let regions = layers.pop().expect("2 層");
    let countries = layers.pop().expect("2 層");
    Ok(PolygonSet { countries, regions })
}

/// 埋め込んだポリゴンを展開・復号し、manifest の件数と名称表の ID に一致することを確かめて返す。
///
/// gzip を展開しながら小さな窓で復号するので、展開後の全体（約 23MB）は持たない。常駐するのは座標の配列（約 30MB）と
/// オフセット表だけ。呼び手（分類段）がサイクルの間だけ持ち、終わったら手放す想定で、ここでは保持しない。
pub fn load_polygons() -> Result<PolygonSet, DataError> {
    #[cfg(test)]
    POLYGON_LOADS.with(|c| c.set(c.get() + 1));
    let m = manifest()?;
    let a = &m.artifacts.polygons;
    if a.raw_bytes > a.max_raw_bytes {
        return Err(DataError::new("展開後のサイズが上限を超える"));
    }
    // 上限 +1 バイトまで読み、超えたら拒否する（`finish` が余りとして検出する）
    let src = GzDecoder::new(POLYGONS_GZ).take(a.max_raw_bytes + 1);
    let set = decode_polygons_from(src, a.raw_bytes, &DecodeLimits::from_manifest(m))?;
    check_polygon_set(&set, m, names()?)?;
    Ok(set)
}

#[cfg(test)]
thread_local! {
    /// 試験用: このスレッドでポリゴンを展開した回数（読み取り専用の経路が展開しないことの確認）。
    pub(super) static POLYGON_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn layer_counts(layer: &Layer) -> LayerCounts {
    LayerCounts {
        features: layer.len(),
        polygons: layer.polygons_len(),
        rings: layer.rings_len(),
        holes: layer.rings_len() - layer.polygons_len(),
        vertices: layer.points_len(),
    }
}

fn same_counts(a: &LayerCounts, b: &LayerCounts) -> bool {
    (a.features, a.polygons, a.rings, a.holes, a.vertices)
        == (b.features, b.polygons, b.rings, b.holes, b.vertices)
}

/// 件数・ID の並び・国参照・座標の範囲と日付変更線をまたぐ辺の無いことを確かめる。
pub fn check_polygon_set(
    set: &PolygonSet,
    m: &Manifest,
    names: &NameTable,
) -> Result<(), DataError> {
    if !same_counts(&layer_counts(&set.countries), &m.counts.countries)
        || !same_counts(&layer_counts(&set.regions), &m.counts.regions)
    {
        return Err(DataError::new("ポリゴンの件数が manifest と違う"));
    }
    if set.countries.len() != names.countries.len() || set.regions.len() != names.regions.len() {
        return Err(DataError::new("ポリゴンと名称表の件数が違う"));
    }
    for (id, p) in set.countries.ids().iter().zip(&names.countries) {
        if *id != p.id {
            return Err(DataError::new("ポリゴンと名称表で国の並びが違う"));
        }
    }
    for (i, p) in names.regions.iter().enumerate() {
        let parent = set.regions.parent(i).map(|c| set.countries.id(c));
        if set.regions.id(i) != p.id || parent != p.country.as_deref() {
            return Err(DataError::new(
                "ポリゴンと名称表で地域の並びまたは国参照が違う",
            ));
        }
    }
    for ring in set.countries.rings().chain(set.regions.rings()) {
        for w in ring.windows(2) {
            let (x1, x2) = (w[0][0], w[1][0]);
            // 図の端（経度 ±180）に沿う辺（南極の縁）は許し、東西が反対側の点を直接つなぐ辺は拒否する
            if (x2 - x1).abs() > 180.0 && !(x1.abs() == 180.0 && x2.abs() == 180.0) {
                return Err(DataError::new("経度 ±180 をまたぐ辺がある"));
            }
        }
        if ring
            .iter()
            .any(|p| !(-180.0..=180.0).contains(&p[0]) || !(-90.0..=90.0).contains(&p[1]))
        {
            return Err(DataError::new("範囲外の座標がある"));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod testing {
    //! 試験が埋め込みデータの生のバイト列を使うための窓口。
    pub fn polygons_gz() -> &'static [u8] {
        super::POLYGONS_GZ
    }
    pub fn names_gz() -> &'static [u8] {
        super::NAMES_GZ
    }
    pub fn manifest_json() -> &'static str {
        super::MANIFEST_JSON
    }
    pub fn gunzip(gz: &[u8], expected: u64, max: u64) -> Result<Vec<u8>, super::DataError> {
        super::gunzip(gz, expected, max)
    }
    pub fn sha256_hex(data: &[u8]) -> String {
        super::sha256_hex(data)
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn caps(n: usize) -> LayerCapacity {
        LayerCapacity {
            features: n,
            polygons: n,
            rings: n,
            points: n,
        }
    }

    #[test]
    fn capacity_that_overflows_or_cannot_be_represented_is_an_error() {
        // `+ 1` があふれる
        assert!(Layer::try_with_capacity(caps(usize::MAX), true).is_err());
        // オフセット表（u32）に表せない
        assert!(Layer::try_with_capacity(caps(u32::MAX as usize), false).is_err());
        assert!(Layer::try_with_capacity(caps(u32::MAX as usize + 1), false).is_err());
        // 普通の大きさは通る
        assert!(Layer::try_with_capacity(caps(1000), true).is_ok());
    }

    #[test]
    fn the_public_decoder_rejects_limits_that_cannot_be_offsets_before_allocating() {
        let huge = LayerLimits {
            features: 1,
            polygons: usize::MAX,
            rings: usize::MAX,
            vertices: usize::MAX,
        };
        let limits = DecodeLimits {
            countries: huge,
            regions: huge,
        };
        // 国 1 件・地域 0 件・ID `ne:X`・多角形 1・輪 1・点 4 の途中で終わるデータ
        let mut raw = MAGIC.to_vec();
        raw.extend_from_slice(&[1, 0, 4]);
        raw.extend_from_slice(b"ne:X");
        // 入力の長さを偽って大きく申告する（`total` から確保量を見積もる経路）
        for total in [raw.len() as u64, u64::MAX / 4] {
            let e = decode_polygons_from(&raw[..], total, &limits).expect_err("拒否されるはず");
            assert!(e.0.contains("大きすぎる"), "{}", e.0);
        }
        // 上限が u32 に収まる大きさでも、入力の長さを超える確保はしない
        let ok_limits = DecodeLimits {
            countries: LayerLimits {
                features: 1,
                polygons: 1,
                rings: 1,
                vertices: 4,
            },
            regions: LayerLimits {
                features: 0,
                polygons: 0,
                rings: 0,
                vertices: 0,
            },
        };
        assert!(decode_polygons_from(&raw[..], raw.len() as u64, &ok_limits).is_err());
    }
}
