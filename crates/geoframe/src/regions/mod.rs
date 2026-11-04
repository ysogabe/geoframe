//! 国・地域別の場所表示のための、同梱の境界データ（Natural Earth 1:10m の Admin0 日本視点版と Admin1）。
//! 設計は docs/international-places/design.md の「境界データと分類器」。
//!
//! - [`data`] — 同梱データの形式の定義と、読み込み・検証。座標は平らな配列（[`data::Layer`]）。
//! - [`index`] — 境界の索引（[`RegionIndex`]。呼び出し側が明示的に作り、使い終えたら手放す）と、判定手順 1〜4。
//! - [`search`] — 分類版（[`classifier_version`]）と検索用文字列（[`search_text`]）。ポリゴンを展開せずに得られる。
//!
//! - [`table`] — 分類の保存先 `photo_regions` 表（`available`・`pending`・`ensure_tables`）と、読み取り側が使う有効な分類の SQL 片。
//! - [`stage`] — 同期段 `regions`（候補選択・判定・適用・掃除）。
//!
//! 通常のビルドは境界データを取得・生成しない。`crates/geoframe/data/regions/` にコミットされた成果物を
//! `include_bytes!` で埋め込み、manifest の SHA256 と一致するかは `cargo test` で検査する。
//! 更新は `tools/regions/` の明示的なコマンドだけで行う。

pub mod data;
pub mod index;
pub mod search;
pub mod stage;
pub mod table;

pub use index::{Classification, InvalidCoordinate, Method, RegionIndex};
pub use search::{classifier_version, search_text};
pub use stage::Regions;
pub use table::{
    SCHEMA, TABLES, available, ensure_tables, join, pending, pending_photos, valid_coordinates,
};

#[cfg(test)]
mod classify_tests;
#[cfg(test)]
mod measure_tests;
#[cfg(test)]
mod stage_tests;
#[cfg(test)]
mod tests;
