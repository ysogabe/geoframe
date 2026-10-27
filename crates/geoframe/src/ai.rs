//! P4: AI の実行基盤。永続ジョブ（[`jobs`]）、設定（[`settings`]）、クラウド送信の予算（[`budget`]）、
//! Provider の一時停止（[`pause`]）、共有の一時待機（[`waits`]）、起動 ID と停止開始の記録（[`runs`]）、背景ワーカー
//! （[`worker`]）、管理操作（[`ops`]。CLI と STATUS）。T4.2 で AI 専用の HTTP（[`http`]・[`dest`]・[`gate`]・[`lanes`]）、
//! Discovery（[`discovery`]）、Decision（[`decision`]・[`image_input`]）、LAN の共通部分（[`provider`]）を足した。
//! T4.6a は [`cloud`] に Vertex の明示認証・固定宛先・用途別通信を足した。T4.6b は [`location_clue`] の写真別 Decision を本番登録する。
//! 設計は `docs/rust-migration/p4-design.md` §3・§4。
//!
//! T4.3 で空間別の検索索引（[`space`]・[`index`]・[`activation`]・[`search`]）を足した。旧選別の安全化は
//! [`crate::selection`]（旧表だけを読むので、AI の表が無い旧 DB でも動く）。
//!
//! 追加の表（`ai_jobs`・`ai_request_usage`・`ai_provider_pauses`・`ai_provider_waits`・`ai_worker_runs`・
//! `ai_embedding_spaces`・`ai_embedding_vectors`・`ai_location_clues`）は Python と同じスキーマの再現
//! （[`crate::schema::migrate`]）には入れず、取り込みが有効な起動で [`migrate`] が足す。`INGEST=0`
//! （読み取り専用）の起動では作らず、表が無い旧 DB は `db::REQUIRED_SCHEMA` を満たしたまま読める。

use rusqlite::{Connection, TransactionBehavior};

pub mod activation;
pub mod budget;
pub mod cloud;
pub mod decision;
pub mod dest;
pub mod discovery;
pub mod embedding;
pub mod gate;
pub mod http;
pub mod image_embedding;
pub mod image_input;
pub mod index;
pub mod jobs;
pub mod lanes;
pub mod location_clue;
pub mod location_suggest;
pub mod ops;
pub mod pause;
pub mod provider;
pub mod runs;
pub mod search;
pub mod settings;
pub mod space;
pub mod waits;
pub mod worker;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_http;
#[cfg(test)]
mod tests_index;
#[cfg(test)]
mod tests_ops;
#[cfg(test)]
mod tests_provider;
#[cfg(test)]
mod tests_settings;
#[cfg(test)]
mod tests_store;
#[cfg(test)]
mod tests_worker;

/// まだ無い AI の表・索引・列の名前（読み取りだけ。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    pending_active(conn, true, true)
}

/// 有効な用途の追加表だけを計画する（書き込みなし）。
pub fn pending_active(
    conn: &Connection,
    clue: bool,
    suggest: bool,
) -> rusqlite::Result<Vec<String>> {
    let mut missing = jobs::pending(conn)?;
    missing.extend(missing_tables(conn, budget::TABLES)?);
    missing.extend(missing_tables(conn, pause::TABLES)?);
    missing.extend(missing_tables(conn, runs::TABLES)?);
    missing.extend(missing_tables(conn, waits::TABLES)?);
    missing.extend(index::pending(conn)?);
    if clue {
        missing.extend(missing_tables(conn, location_clue::TABLES)?);
    }
    if suggest {
        missing.extend(missing_tables(conn, location_suggest::TABLES)?);
    }
    Ok(missing)
}

/// AI の表を全部足す（無いものだけ。既に最新なら何も書かない）。作った名前を返す。
/// 書き込み用の接続で呼ぶ。他の表・行には触れない。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    migrate_active(conn, true, true)
}

/// 本番起動は location_clue / location_suggest のそれぞれ有効な用途の結果表だけを作る。
pub fn migrate_active(
    conn: &mut Connection,
    location_clue_enabled: bool,
    location_suggest_enabled: bool,
) -> rusqlite::Result<Vec<String>> {
    let mut created = jobs::migrate(conn)?;
    created.extend(create_tables(conn, budget::TABLES, budget::SCHEMA)?);
    created.extend(create_tables(conn, pause::TABLES, pause::SCHEMA)?);
    created.extend(create_tables(conn, runs::TABLES, runs::SCHEMA)?);
    created.extend(create_tables(conn, waits::TABLES, waits::SCHEMA)?);
    created.extend(index::migrate(conn)?);
    if location_clue_enabled {
        created.extend(create_tables(
            conn,
            location_clue::TABLES,
            location_clue::SCHEMA,
        )?);
    }
    if location_suggest_enabled {
        created.extend(create_tables(
            conn,
            location_suggest::TABLES,
            location_suggest::SCHEMA,
        )?);
    }
    Ok(created)
}

fn missing_tables(conn: &Connection, tables: &[&str]) -> rusqlite::Result<Vec<String>> {
    let mut missing = Vec::new();
    for table in tables {
        if !jobs::object_exists(conn, "table", table)? {
            missing.push((*table).to_string());
        }
    }
    Ok(missing)
}

/// `schema`（`IF NOT EXISTS` の表だけ）を、`tables` のどれかが無いときに 1 つのトランザクションで実行する。
fn create_tables(
    conn: &mut Connection,
    tables: &[&str],
    schema: &str,
) -> rusqlite::Result<Vec<String>> {
    let missing = missing_tables(conn, tables)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(schema)?;
    tx.commit()?;
    Ok(missing)
}
