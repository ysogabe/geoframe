//! P4: AI の実行基盤。永続ジョブ（[`jobs`]）、設定（[`settings`]）、クラウド送信の予算（[`budget`]）、
//! Provider の一時停止（[`pause`]）、共有の一時待機（[`waits`]）、起動 ID と停止開始の記録（[`runs`]）、背景ワーカー
//! （[`worker`]）、管理操作（[`ops`]。CLI と STATUS）。T4.2 で AI 専用の HTTP（[`http`]・[`dest`]・[`lanes`]）、
//! Discovery（[`discovery`]）、Decision（[`decision`]・[`image_input`]）、LAN の共通部分（[`provider`]）を足した。
//! 用途別の handler の登録は後続のタスク。
//! 設計は `docs/rust-migration/p4-design.md` §3・§4。
//!
//! 追加の表（`ai_jobs`・`ai_request_usage`・`ai_provider_pauses`・`ai_provider_waits`・`ai_worker_runs`）は Python と同じスキーマの再現
//! （[`crate::schema::migrate`]）には入れず、取り込みが有効な起動で [`migrate`] が足す。`INGEST=0`
//! （読み取り専用）の起動では作らず、表が無い旧 DB は `db::REQUIRED_SCHEMA` を満たしたまま読める。

use rusqlite::{Connection, TransactionBehavior};

pub mod budget;
pub mod decision;
pub mod dest;
pub mod discovery;
pub mod http;
pub mod image_input;
pub mod jobs;
pub mod lanes;
pub mod ops;
pub mod pause;
pub mod provider;
pub mod runs;
pub mod settings;
pub mod waits;
pub mod worker;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_http;
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
    let mut missing = jobs::pending(conn)?;
    missing.extend(missing_tables(conn, budget::TABLES)?);
    missing.extend(missing_tables(conn, pause::TABLES)?);
    missing.extend(missing_tables(conn, runs::TABLES)?);
    missing.extend(missing_tables(conn, waits::TABLES)?);
    Ok(missing)
}

/// AI の表を全部足す（無いものだけ。既に最新なら何も書かない）。作った名前を返す。
/// 書き込み用の接続で呼ぶ。他の表・行には触れない。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let mut created = jobs::migrate(conn)?;
    created.extend(create_tables(conn, budget::TABLES, budget::SCHEMA)?);
    created.extend(create_tables(conn, pause::TABLES, pause::SCHEMA)?);
    created.extend(create_tables(conn, runs::TABLES, runs::SCHEMA)?);
    created.extend(create_tables(conn, waits::TABLES, waits::SCHEMA)?);
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
