//! 国・地域別の場所表示のための、同梱の境界データ（Natural Earth 1:10m の Admin0 日本視点版と Admin1）。
//! 設計は docs/international-places/design.md の「境界データと分類器」。
//!
//! - [`data`]: 同梱データの形式の定義と、読み込み・検証（段①の T1）。点の内外判定・索引・同期段は後のタスクで足す。
//!
//! 通常のビルドは境界データを取得・生成しない。`crates/geoframe/data/regions/` にコミットされた成果物を
//! `include_bytes!` で埋め込み、manifest の SHA256 と一致するかは `cargo test` で検査する。
//! 更新は `tools/regions/` の明示的なコマンドだけで行う。

pub mod data;

#[cfg(test)]
mod tests;
