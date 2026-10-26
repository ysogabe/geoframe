//! 有効な検索空間の読み取りと線形のコサイン検索（P4 / T4.3。設計は p4-design.md §6.1・§6.2・§7）。
//!
//! # 読み取り
//!
//! [`load_active_index`] は `settings.ai_active_space`・空間の識別・今の対象・ベクトルを**同じ読み取り
//! トランザクション**で読む。並行する切替・保存の前後が混ざらない（ポインタと本体が同じ世代）。ポインタが無い・
//! 未登録・表が無い旧 DB は `index: None`（旧結果参照。新しい空間なし）。
//!
//! 索引に入るのは、今の対象（[`super::index`] の規則）のうち、**その空間に**有効なベクトルがある `content_sha256`。
//! 別の空間・旧 `photo_embeddings` / `photo_ai_features` のベクトルは読まない（混在 0）。同じ `content_sha256` の
//! 写真は 1 つの項目にまとめ、`photo_ids`（昇順。先頭が代表）に全て持つ。BLOB は長さ・有限数・ノルム・空間の
//! 識別で検査し、壊れた行は**読み飛ばして数える**（`status.corrupt`。全体の失敗やパニックにしない）。壊れた行は
//! `pending` に数えて再生成の対象になる（[`super::index::store_prepared`] が置き換える）。検査は公開・進捗と
//! 同じ 1 つの判定（[`super::index`] の `scan`）なので、`status` と索引の中身は食い違わない。
//!
//! # 検索
//!
//! 検索語のベクトルは [`QueryVector`] で、**同じ識別**（`provider_space_id`・前処理・版を含む全項目）が要る。
//! 違う識別・違う次元は [`SearchError::IdentityMismatch`]。計算は全項目を線形に走るコサイン類似度（現在の規模
//! 6,500 枚 × 768 次元で足りる想定。ANN・外部ベクトル DB は使わない）。`none` の空間でも向きだけを比べる
//! （読み込み時に単位ベクトルへ直す）。並びは類似度の降順、同点は代表の写真 ID の昇順。
//!
//! このモジュールは**型付きの境界**で、E5/E6 の HTTP との接続は T4.4。

use std::fmt;

use rusqlite::Connection;

use super::index::{IndexStatus, status_in};
use super::space::{self, SpaceIdentity, VectorError};

/// 索引の 1 項目（`content_sha256` ごと）。値は `Debug` に出さない。
#[derive(Clone)]
pub struct IndexEntry {
    photo_ids: Vec<i64>,
    input_sha256: String,
    unit: Vec<f32>,
}

impl fmt::Debug for IndexEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexEntry")
            .field("photos", &self.photo_ids.len())
            .finish_non_exhaustive()
    }
}

impl IndexEntry {
    /// 代表の写真 ID（同じ内容の写真のうち最小）。
    pub fn photo_id(&self) -> i64 {
        self.photo_ids[0]
    }

    /// 同じ内容の写真すべて（昇順）。
    pub fn photo_ids(&self) -> &[i64] {
        &self.photo_ids
    }

    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }
}

/// 読み込んだ有効な索引。
pub struct ActiveIndex {
    identity: SpaceIdentity,
    entries: Vec<IndexEntry>,
}

impl fmt::Debug for ActiveIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveIndex")
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

/// [`load_active_index`] の結果。`index` が `None` なら旧結果参照（新しい空間なし）。
#[derive(Debug)]
pub struct Loaded {
    pub status: IndexStatus,
    pub index: Option<ActiveIndex>,
}

/// 検索語のベクトル（索引と同じ識別のもの）。
#[derive(Clone)]
pub struct QueryVector {
    identity: SpaceIdentity,
    unit: Vec<f32>,
}

impl fmt::Debug for QueryVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryVector").finish_non_exhaustive()
    }
}

impl QueryVector {
    /// Provider から得た検索語のベクトルを、`identity` で検査する（次元・有限数・ゼロでないノルム）。
    pub fn new(identity: &SpaceIdentity, values: &[f32]) -> Result<Self, VectorError> {
        Ok(Self {
            identity: identity.clone(),
            unit: space::unit(identity, values)?,
        })
    }

    pub fn identity(&self) -> &SpaceIdentity {
        &self.identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    /// 検索語のベクトルの識別が索引と違う（混ぜて比較しない）。
    IdentityMismatch,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("検索語のベクトルの空間が索引と違います")
    }
}

impl std::error::Error for SearchError {}

/// 検索結果の 1 件。
#[derive(Clone, PartialEq)]
pub struct Hit {
    pub photo_id: i64,
    pub photo_ids: Vec<i64>,
    pub input_sha256: String,
    /// コサイン類似度（-1〜1）。
    pub score: f32,
}

impl fmt::Debug for Hit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hit")
            .field("score", &self.score)
            .finish_non_exhaustive()
    }
}

impl ActiveIndex {
    pub fn identity(&self) -> &SpaceIdentity {
        &self.identity
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 項目の索引ベクトル（単位ベクトル）と `query` の類似度の高い順に、上位 `limit` 件。
    pub fn search(&self, query: &QueryVector, limit: usize) -> Result<Vec<Hit>, SearchError> {
        if query.identity != self.identity {
            return Err(SearchError::IdentityMismatch);
        }
        let mut scored: Vec<(f32, &IndexEntry)> = self
            .entries
            .iter()
            .map(|e| (space::dot(&e.unit, &query.unit), e))
            .collect();
        scored.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.photo_id().cmp(&b.1.photo_id()))
        });
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            .map(|(score, e)| Hit {
                photo_id: e.photo_id(),
                photo_ids: e.photo_ids.clone(),
                input_sha256: e.input_sha256.clone(),
                score,
            })
            .collect())
    }
}

/// 有効な検索空間を読む。ポインタ・識別・対象・ベクトル・状態を 1 つの読み取りトランザクションで読む。
/// 表が無い旧 DB でも書き込みなしで動く（`index: None`）。
pub fn load_active_index(conn: &Connection) -> rusqlite::Result<Loaded> {
    let tx = conn.unchecked_transaction()?;
    let loaded = load_in(&tx)?;
    tx.commit()?;
    Ok(loaded)
}

/// [`load_active_index`] の本体（呼び出し側の読み取りトランザクションの中で動く）。公開・進捗と同じ判定
/// ([`super::index::scan`]) の結果から作るので、`status` の件数と索引の中身が食い違わない。
pub(super) fn load_in(tx: &Connection) -> rusqlite::Result<Loaded> {
    let (status, identity, states) = status_in(tx, true)?;
    let Some(identity) = identity else {
        return Ok(Loaded {
            status,
            index: None,
        });
    };
    let entries = states
        .into_iter()
        .filter_map(|state| {
            let unit = state.unit?;
            Some(IndexEntry {
                input_sha256: state.members[0].input_sha256.clone(),
                photo_ids: state.members.iter().map(|m| m.photo_id).collect(),
                unit,
            })
        })
        .collect();
    Ok(Loaded {
        status,
        index: Some(ActiveIndex { identity, entries }),
    })
}
