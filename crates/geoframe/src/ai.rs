//! P4: AI の実行基盤。いまは永続ジョブ（[`jobs`]）だけ。HTTP・設定・ワーカー・Provider は後続のタスク。
//! 設計は `docs/rust-migration/p4-design.md` §4。

pub mod jobs;

#[cfg(test)]
mod tests;
