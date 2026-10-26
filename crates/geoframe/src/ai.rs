//! P4: AI の実行基盤。永続ジョブ（[`jobs`]）、設定（[`settings`]）、クラウド送信の予算（[`budget`]）、
//! Provider の一時停止（[`pause`]）。HTTP・ワーカー・Provider は後続のタスク。
//! 設計は `docs/rust-migration/p4-design.md` §3・§4。
//!
//! 追加の表（`ai_jobs`・`ai_request_usage`・`ai_provider_pauses`）は Python と同じスキーマの再現
//! （[`crate::schema::migrate`]）には入れず、取り込みが有効な起動で [`migrate`] が足す。`INGEST=0`
//! （読み取り専用）の起動では作らず、表が無い旧 DB は `db::REQUIRED_SCHEMA` を満たしたまま読める。

use rusqlite::{Connection, TransactionBehavior};

pub mod budget;
pub mod jobs;
pub mod pause;
pub mod settings;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_settings;
#[cfg(test)]
mod tests_store;

/// まだ無い AI の表・索引・列の名前（読み取りだけ。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut missing = jobs::pending(conn)?;
    missing.extend(missing_tables(conn, budget::TABLES)?);
    missing.extend(missing_tables(conn, pause::TABLES)?);
    Ok(missing)
}

/// AI の表を全部足す（無いものだけ。既に最新なら何も書かない）。作った名前を返す。
/// 書き込み用の接続で呼ぶ。他の表・行には触れない。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let mut created = jobs::migrate(conn)?;
    created.extend(create_tables(conn, budget::TABLES, budget::SCHEMA)?);
    created.extend(create_tables(conn, pause::TABLES, pause::SCHEMA)?);
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
