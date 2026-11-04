//! 分類版（`classifier_version`）と、地域の検索用文字列（`search_text`）。どちらもポリゴンを展開せずに得られる
//! （manifest と、小さな名称表だけを使う）。読み取り専用の起動でも呼べる。

use std::sync::OnceLock;

use super::data::{self, DataError};

/// 分類のアルゴリズム（判定手順 1〜4・境界上は内部・格子の細かさに依らない意味）の版。判定の意味が変わるときに上げる。
pub const ALGORITHM_VERSION: u32 = 1;

/// `search_text` の作り方（含める表記・正規化・連結）の版。作り方を変えるときに上げる。
pub const SEARCH_TEXT_VERSION: u32 = 1;

/// 分類版。境界データ（manifest の `dataset_sha256` と、ポリゴン・名称表それぞれの展開後の SHA256）、形式の版、
/// 分類アルゴリズムの版、`search_text` の作り方の版を、固定の並びで SHA256 にかけたもの。名称表を直しても変わる。
/// 実行時に全データをハッシュしない（manifest の値を使う。manifest と成果物の一致は `cargo test` が確かめる）。
pub fn classifier_version() -> Result<&'static str, DataError> {
    static CELL: OnceLock<Result<String, DataError>> = OnceLock::new();
    CELL.get_or_init(|| {
        let m = data::manifest()?;
        let input = format!(
            "geoframe-regions\nformat={}\nalgorithm={}\nsearch_text={}\ndataset={}\npolygons={}\nnames={}\n",
            m.format,
            ALGORITHM_VERSION,
            SEARCH_TEXT_VERSION,
            m.dataset_sha256,
            m.artifacts.polygons.raw_sha256,
            m.artifacts.names.raw_sha256,
        );
        let hex = data::sha256_hex(input.as_bytes());
        Ok(format!("gf{ALGORITHM_VERSION}-{}", &hex[..24]))
    })
    .as_ref()
    .map(String::as_str)
    .map_err(Clone::clone)
}

/// 国と（あれば）地域の検索用文字列。国・地域の日本語・英語・原表記・別名を、国→地域の順に重複なしで 1 つにつなぐ。
///
/// 正規化は、既存の文字検索（`admin_library` の `q`。SQLite の `LIKE '%語%'`。ASCII の大文字小文字を区別しない）に合わせる。
/// 語は空白で区切られて渡る（空白を含まない）ので、各表記の前後の空白を除き、内部の連続する空白を 1 つにして、
/// 表記どうしを半角空白でつなぐ。大文字小文字・かな・全半角は変えない（`LIKE` も変えない）。
/// 地域が国に属さない、または ID が名称表に無いときは `None`。
pub fn search_text(country_id: &str, region_id: Option<&str>) -> Option<String> {
    let names = data::names().ok()?;
    let country = names.country(country_id)?;
    let region = match region_id {
        Some(id) => {
            let r = names.region(id)?;
            if r.country.as_deref() != Some(country_id) {
                return None;
            }
            Some(r)
        }
        None => None,
    };
    let mut seen: Vec<String> = Vec::new();
    for s in country
        .search
        .iter()
        .chain(region.into_iter().flat_map(|r| r.search.iter()))
    {
        let norm = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if !norm.is_empty() && !seen.contains(&norm) {
            seen.push(norm);
        }
    }
    Some(seen.join(" "))
}
