//! AI ワーカーの起動 ID と停止開始の記録 `ai_worker_runs`（P4 / T4.1c。設計は p4-design.md §4.3 の終わり）。
//!
//! `ai_jobs.run_id` が指す「起動」ごとに 1 行持つ。**停止開始**（`stop_requested_at`）は、SIGTERM を受けた直後に
//! その起動が書く印で、次の起動が `running` のまま残ったジョブを回収するときに、
//!
//! - 停止開始が記録された起動 → 正常な停止の途中で最終の戻し（[`super::jobs::release_for_shutdown`]）が
//!   書けなかっただけ。`deferred/shutdown` に戻し、**中断としては数えない**。
//! - 記録の無い起動 → `SIGKILL`・電源断・クラッシュ。`interruption_count` を加算し、3 回で `failed`
//!   （[`super::jobs::reclaim_interrupted`]）。
//!
//! と区別する。回収は**ロックを取ったあと**（`.ingest.lock` を持つ起動だけが呼ぶ。同じ DB を使う別の起動が
//! 生きている間は呼ばれない）に 1 回だけ、1 つのトランザクションで行う。済んだ起動の行は消す（表は小さいまま）。
//!
//! 表は `INGEST=1` の起動の [`super::migrate`] だけが作る。`INGEST=0` では作らず、回収もしない。

use rusqlite::{Connection, TransactionBehavior, params};

use super::jobs::{self, Reclaimed};

pub const TABLES: &[&str] = &["ai_worker_runs"];

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_worker_runs(
  run_id TEXT PRIMARY KEY,
  started_at INTEGER NOT NULL,
  stop_requested_at INTEGER
) WITHOUT ROWID;";

/// [`startup`] の結果。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StartupReport {
    /// 中断として数えた（`interruption_count` を加算した）もの。
    pub interrupted: Reclaimed,
    /// 停止開始が記録されていたので、数えずに `deferred/shutdown` に戻したもの。
    pub planned: usize,
}

/// 起動の最初（ロックを取ったあと）に呼ぶ。前の起動の `running` を回収し、この起動の行を作り、
/// 済んだ起動の行を消す。1 つの `IMMEDIATE` トランザクション。
pub fn startup(conn: &mut Connection, run_id: &str, now: i64) -> rusqlite::Result<StartupReport> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 先に、停止開始が記録された起動の running を数えずに戻す（残りが予期しない中断）
    let planned = tx.execute(
        "UPDATE ai_jobs SET state='deferred', next_attempt_at=?2, claim_token=NULL,
                last_error_code='shutdown', updated_at=?2
         WHERE state='running' AND run_id IS NOT ?1
           AND run_id IN (SELECT run_id FROM ai_worker_runs WHERE stop_requested_at IS NOT NULL)",
        params![run_id, now],
    )?;
    let interrupted = jobs::reclaim_interrupted_in(&tx, run_id, now)?;
    // 回収が済んだので、前の起動の行は要らない（この起動の行は、停止の印が先に書かれていても残す）
    tx.execute("DELETE FROM ai_worker_runs WHERE run_id<>?1", [run_id])?;
    tx.execute(
        "INSERT INTO ai_worker_runs(run_id, started_at) VALUES (?1,?2)
         ON CONFLICT(run_id) DO NOTHING",
        params![run_id, now],
    )?;
    tx.commit()?;
    Ok(StartupReport {
        interrupted,
        planned,
    })
}

/// 停止開始を記録する（SIGTERM の直後。最初の 1 回の時刻を保つ）。起動の行がまだ無くても作る。
pub fn mark_stop_requested(conn: &Connection, run_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO ai_worker_runs(run_id, started_at, stop_requested_at) VALUES (?1,?2,?2)
         ON CONFLICT(run_id) DO UPDATE SET stop_requested_at=COALESCE(stop_requested_at, ?2)",
        params![run_id, now],
    )?;
    Ok(())
}
