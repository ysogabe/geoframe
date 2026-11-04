//! 同梱の境界データの形式の定義と、読み込み・検証。判定のロジックは持たない（分類器は別のタスク）。
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

fn sha256_hex(data: &[u8]) -> String {
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

/// 輪（閉じた点列。先頭と末尾が同じ点）。座標は `[経度, 緯度]`。
pub type Ring = Vec<[f64; 2]>;

/// 多角形。`rings[0]` が外側、残りが穴。
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub rings: Vec<Ring>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub id: String,
    /// 地域だけ。`PolygonSet::countries` の添字。
    pub country: Option<usize>,
    pub polygons: Vec<Polygon>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PolygonSet {
    pub countries: Vec<Feature>,
    pub regions: Vec<Feature>,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Result<u8, DataError> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| DataError::new("ポリゴンが途中で終わっている"))?;
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
        let remaining = self.data.len() - self.pos;
        if n.checked_mul(min_bytes_each)
            .is_none_or(|need| need > remaining)
        {
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

    fn take(&mut self, n: usize) -> Result<&'a [u8], DataError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or_else(|| DataError::new("ポリゴンが途中で終わっている"))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
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
    let mut c = Cursor { data: raw, pos: 0 };
    if c.take(MAGIC.len())? != MAGIC {
        return Err(DataError::new("ポリゴンの magic が違う"));
    }
    let mut features_budget = (limits.countries.features, limits.regions.features);
    let n_countries = c.count_within(3, &mut features_budget.0)?;
    let n_regions = c.count_within(3, &mut features_budget.1)?;
    let (mut px, mut py) = (0i64, 0i64);
    let mut layers: [Vec<Feature>; 2] = [
        Vec::with_capacity(n_countries),
        Vec::with_capacity(n_regions),
    ];
    for (layer, is_region, n, lim) in [
        (0usize, false, n_countries, &limits.countries),
        (1, true, n_regions, &limits.regions),
    ] {
        let (mut polygons_left, mut rings_left, mut vertices_left) =
            (lim.polygons, lim.rings, lim.vertices);
        for _ in 0..n {
            let id_len = c.count(1)?;
            if id_len == 0 || id_len > MAX_ID_LEN {
                return Err(DataError::new("ID の長さが不正"));
            }
            let id = std::str::from_utf8(c.take(id_len)?)
                .ok()
                .filter(|s| valid_id(s))
                .ok_or_else(|| DataError::new("ID に使えない文字がある"))?
                .to_owned();
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
            let mut polygons = Vec::with_capacity(n_polygons);
            for _ in 0..n_polygons {
                let n_rings = c.count_within(1, &mut rings_left)?;
                if n_rings == 0 {
                    return Err(DataError::new("輪が無い多角形"));
                }
                let mut rings = Vec::with_capacity(n_rings);
                for _ in 0..n_rings {
                    let n_points = c.count_within(2, &mut vertices_left)?;
                    if n_points < 4 {
                        return Err(DataError::new("輪の点が 4 未満"));
                    }
                    let mut ring = Vec::with_capacity(n_points);
                    for _ in 0..n_points {
                        px = px.wrapping_add(unzigzag(c.varint()?));
                        py = py.wrapping_add(unzigzag(c.varint()?));
                        let (x, y) = (f64::from_bits(px as u64), f64::from_bits(py as u64));
                        if !x.is_finite() || !y.is_finite() {
                            return Err(DataError::new("有限でない座標"));
                        }
                        ring.push([x, y]);
                    }
                    if ring.first() != ring.last() {
                        return Err(DataError::new("輪が閉じていない"));
                    }
                    rings.push(ring);
                }
                polygons.push(Polygon { rings });
            }
            layers[layer].push(Feature {
                id,
                country,
                polygons,
            });
        }
    }
    if c.pos != raw.len() {
        return Err(DataError::new("ポリゴンの末尾に余りがある"));
    }
    let [countries, regions] = layers;
    Ok(PolygonSet { countries, regions })
}

/// 埋め込んだポリゴンを展開・復号し、manifest の件数と名称表の ID に一致することを確かめて返す。
///
/// 数十 MB の一時メモリを使う。呼び手（分類段）がサイクルの間だけ持ち、終わったら手放す想定で、ここでは保持しない。
pub fn load_polygons() -> Result<PolygonSet, DataError> {
    let m = manifest()?;
    let a = &m.artifacts.polygons;
    let raw = gunzip(POLYGONS_GZ, a.raw_bytes, a.max_raw_bytes)?;
    let set = decode_polygons(&raw, &DecodeLimits::from_manifest(m))?;
    drop(raw);
    check_polygon_set(&set, m, names()?)?;
    Ok(set)
}

fn layer_counts(features: &[Feature]) -> LayerCounts {
    let polygons = features.iter().map(|f| f.polygons.len()).sum();
    let rings = features
        .iter()
        .flat_map(|f| &f.polygons)
        .map(|p| p.rings.len())
        .sum();
    let vertices = features
        .iter()
        .flat_map(|f| &f.polygons)
        .flat_map(|p| &p.rings)
        .map(Vec::len)
        .sum();
    LayerCounts {
        features: features.len(),
        polygons,
        rings,
        holes: rings - polygons,
        vertices,
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
    for (f, p) in set.countries.iter().zip(&names.countries) {
        if f.id != p.id {
            return Err(DataError::new("ポリゴンと名称表で国の並びが違う"));
        }
    }
    for (f, p) in set.regions.iter().zip(&names.regions) {
        let parent = f.country.map(|i| set.countries[i].id.as_str());
        if f.id != p.id || parent != p.country.as_deref() {
            return Err(DataError::new(
                "ポリゴンと名称表で地域の並びまたは国参照が違う",
            ));
        }
    }
    for f in set.countries.iter().chain(&set.regions) {
        for ring in f.polygons.iter().flat_map(|p| &p.rings) {
            for w in ring.windows(2) {
                let ((x1, _), (x2, _)) = ((w[0][0], w[0][1]), (w[1][0], w[1][1]));
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
