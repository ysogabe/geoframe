//! Provider・用途の一時停止 `ai_provider_pauses`（P4 / T4.1b の足場。設計は p4-design.md §4.3 の表）。
//!
//! 認証 token を 1 回更新しても続く 401、403、モデル 404、Provider 全体の契約不一致は、ジョブごとの失敗では
//! ない。停止を**理由コードつきで DB に残し**、解除されるまで新しい要求を出さない（[`status`]）。再起動では
//! 解除されない。全ジョブを `failed` にはしない（この表は `ai_jobs` に触れない）。
//!
//! # 停止の単位と、解除される設定変更
//!
//! 停止は [`PauseKey`]（範囲＋識別 hash）で覚える。識別 hash は**その理由に関係する設定だけ**から作る
//! （[`super::settings::AiSettings::provider_pause_key`]・[`task_pause_key`](super::settings::AiSettings::task_pause_key)）:
//! Provider の停止は接続先・認証（LAN の入口、クラウドの認証ファイル・project・region）、用途の停止は
//! 用途・モデル・その接続先。タイムアウト・別の用途の許可や上限・`include_hidden` を変えても hash は変わらず、
//! 停止は解除されない。関係する設定が変わると hash が変わり、その停止は現在の設定に当たらなくなる
//! （効かない）。残った古い行は [`release_unmatched`] が消す。管理 CLI からの明示の解除は
//! [`release`]・[`release_all`]（CLI 自体は T4.1c）。認証の更新・検証の通信はここに無い。
//!
//! # 秘匿
//!
//! 保存するのは固定の理由コードと用途名と時刻と hash だけ。URL・パス・モデル名・応答の本文は保存しない。
//! [`PauseKey`] の `Debug` は hash を出さない。

use std::fmt;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::settings::{AiSettings, TaskKind};

pub const TABLES: &[&str] = &["ai_provider_pauses"];

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_provider_pauses(
  scope TEXT NOT NULL CHECK(scope IN ('provider','task')),
  subject TEXT NOT NULL,
  task TEXT NOT NULL CHECK(task IN
    ('image_embedding','query_embedding','photo_judge','location_clue','location_suggest')),
  reason TEXT NOT NULL CHECK(reason IN
    ('unauthorized','forbidden','model_not_found','contract_mismatch')),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(scope, subject)
) WITHOUT ROWID;";

/// 停止の理由（固定の列挙だけを保存する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// 認証 token を 1 回更新しても 401。
    Unauthorized,
    Forbidden,
    /// モデル（または用途のサービス）が 404。
    ModelNotFound,
    /// Provider 全体の契約不一致（版・能力・応答の形）。
    ContractMismatch,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::ModelNotFound => "model_not_found",
            Self::ContractMismatch => "contract_mismatch",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Unauthorized,
            Self::Forbidden,
            Self::ModelNotFound,
            Self::ContractMismatch,
        ]
        .into_iter()
        .find(|r| r.as_str() == text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseScope {
    /// 同じ接続先・認証を使う全用途。
    Provider,
    /// 1 つの用途（モデルを含む）。
    Task,
}

impl PauseScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Task => "task",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Provider, Self::Task]
            .into_iter()
            .find(|s| s.as_str() == text)
    }
}

/// 停止の識別（範囲＋識別 hash）。`AiSettings` が作る。hash は `Debug` に出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct PauseKey {
    scope: PauseScope,
    subject: String,
}

impl PauseKey {
    pub(super) fn new(scope: PauseScope, subject: String) -> Self {
        Self { scope, subject }
    }

    pub fn scope(&self) -> PauseScope {
        self.scope
    }
}

impl fmt::Debug for PauseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PauseKey")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// 有効な停止（[`status`] の結果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paused {
    pub scope: PauseScope,
    pub reason: PauseReason,
}

/// 状態表示用の 1 行（識別 hash は含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseInfo {
    pub scope: PauseScope,
    pub task: TaskKind,
    pub reason: PauseReason,
    pub created_at: i64,
}

/// 停止を記録する。既に同じ `key` の停止があれば**何も変えない**（最初の理由と時刻を保つ。`false`）。
/// 新しく記録したら `true`。`task` は停止を起こした用途（表示用）。
pub fn pause(
    conn: &mut Connection,
    task: TaskKind,
    key: &PauseKey,
    reason: PauseReason,
    now: i64,
) -> rusqlite::Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO ai_provider_pauses(scope, subject, task, reason, created_at)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(scope, subject) DO NOTHING",
        params![
            key.scope.as_str(),
            key.subject,
            task.as_str(),
            reason.as_str(),
            now
        ],
    )?;
    Ok(inserted == 1)
}

/// `keys` のどれかが停止中か（読み取りだけ。表が無い旧 DB は停止なし）。
pub fn active(conn: &Connection, keys: &[PauseKey]) -> rusqlite::Result<Option<Paused>> {
    if !super::jobs::object_exists(conn, "table", "ai_provider_pauses")? {
        return Ok(None);
    }
    for key in keys {
        let row: Option<String> = conn
            .query_row(
                "SELECT reason FROM ai_provider_pauses WHERE scope=?1 AND subject=?2",
                params![key.scope.as_str(), key.subject],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(reason) = row.as_deref().and_then(PauseReason::parse) {
            return Ok(Some(Paused {
                scope: key.scope,
                reason,
            }));
        }
    }
    Ok(None)
}

/// 現在の設定での、`task` の停止（Provider の停止・用途の停止のどちらか）。用途が設定に無ければ `None`。
pub fn status(
    conn: &Connection,
    settings: &AiSettings,
    task: TaskKind,
) -> rusqlite::Result<Option<Paused>> {
    let keys: Vec<PauseKey> = [
        settings.provider_pause_key(task),
        settings.task_pause_key(task),
    ]
    .into_iter()
    .flatten()
    .collect();
    active(conn, &keys)
}

/// 明示の解除（管理 CLI 用）。あれば消して `true`。
pub fn release(conn: &mut Connection, key: &PauseKey) -> rusqlite::Result<bool> {
    let removed = conn.execute(
        "DELETE FROM ai_provider_pauses WHERE scope=?1 AND subject=?2",
        params![key.scope.as_str(), key.subject],
    )?;
    Ok(removed == 1)
}

/// 全ての停止を明示的に解除する（管理 CLI 用）。消した件数を返す。
pub fn release_all(conn: &mut Connection) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM ai_provider_pauses", [])
}

/// 現在の設定の識別（`current`）のどれにも当たらない停止を消す: 関係する設定が変わって効かなくなった行の
/// 整理。無関係の設定変更・再起動では何も消えない。消した件数を返す。
pub fn release_unmatched(conn: &mut Connection, current: &[PauseKey]) -> rusqlite::Result<usize> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT scope, subject FROM ai_provider_pauses")?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut removed = 0;
    for (scope, subject) in rows {
        let matched = current
            .iter()
            .any(|k| k.scope.as_str() == scope && k.subject == subject);
        if !matched {
            removed += tx.execute(
                "DELETE FROM ai_provider_pauses WHERE scope=?1 AND subject=?2",
                params![scope, subject],
            )?;
        }
    }
    tx.commit()?;
    Ok(removed)
}

/// 停止の一覧（状態表示用。古い順）。
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<PauseInfo>> {
    if !super::jobs::object_exists(conn, "table", "ai_provider_pauses")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT scope, task, reason, created_at FROM ai_provider_pauses
         ORDER BY created_at, scope, task",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (scope, task, reason, created_at) = row?;
        if let (Some(scope), Some(task), Some(reason)) = (
            PauseScope::parse(&scope),
            TaskKind::parse(&task),
            PauseReason::parse(&reason),
        ) {
            out.push(PauseInfo {
                scope,
                task,
                reason,
                created_at,
            });
        }
    }
    Ok(out)
}
