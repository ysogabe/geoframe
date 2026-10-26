//! Provider の共有の一時待機 `ai_provider_waits`（P4 / T4.2。設計は p4-design.md §4.3 の 1・2 行目）。
//!
//! 到達不能・GPU lease・429・「まだ使えない」は、写真の不良でもジョブの失敗でもない**一時的な待ち**。待ちの単位は
//! 次の 2 つで、型で区別する（[`WaitScope`]）:
//!
//! - **入口全体**（[`WaitScope::Entrance`]）: 同じ Provider（接続先・認証が同じ。
//!   [`super::settings::AiSettings::provider_fingerprint`]）を使う**全用途**を止める根拠がある。到達不能、
//!   lease が 1 件以上または数えられない（GPU の評価中）、429 など。
//! - **サービス単体**（[`WaitScope::Service`]）: そのサービスを使う用途だけを止める。サービスが running でない、
//!   health が loading、`available=false` など。同じ入口の別サービスは巻き込まない。
//!
//! 別の Provider（別の接続先・クラウド）は、どちらの待ちにも巻き込まない。
//!
//! # 永続停止との違い
//!
//! 認証の失敗（401・403）・モデル 404・契約不一致は [`super::pause`] の**永続停止**で、管理者の操作か設定の変更まで
//! 続く。ここの待ちは `until`（UTC の Unix 秒。60〜3600 秒先）が過ぎれば自然に消え、再起動をまたいで残る
//! （再起動の直後に Provider へ連打しない）。待ちは成功済みの結果にも `ai_jobs` の他の行にも触れず、設定の変更や
//! 待ちだけで過去の成功結果を再送することはない。
//!
//! # 秘匿
//!
//! 保存するのはハッシュ化した識別（URL・サービス ID・モデル名そのものは入れない）・固定の理由・時刻だけ。

use std::collections::HashMap;

use rusqlite::{Connection, params};

use super::jobs::{MAX_DEFER_SECS, MIN_DEFER_SECS};

pub const TABLES: &[&str] = &["ai_provider_waits"];

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_provider_waits(
  provider TEXT NOT NULL,
  service TEXT NOT NULL,
  reason TEXT NOT NULL CHECK(reason IN ('unreachable','leased','not_ready','rate_limited','busy')),
  until INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(provider, service)
) WITHOUT ROWID;";

/// `service` 列で、入口全体の待ちを表す値。
const ENTRANCE: &str = "";

/// 待ちが及ぶ範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitScope {
    /// 同じ Provider の全用途。
    Entrance,
    /// そのサービスを使う用途だけ。
    Service,
}

/// 待ちの理由（固定の列挙だけを保存・表示する。Provider の自由文の `reason` は使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// DNS・接続・TLS・時間切れ・切断で到達できない。
    Unreachable,
    /// lease が 1 件以上、または lease を変更中で数えられない。
    Leased,
    /// サービスが running でない・health が loading・`available=false`。
    NotReady,
    /// 429。
    RateLimited,
    /// 入口が一時的に混んでいる（503）・こちらの枠が使用中。
    Busy,
}

impl WaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Leased => "leased",
            Self::NotReady => "not_ready",
            Self::RateLimited => "rate_limited",
            Self::Busy => "busy",
        }
    }
}

/// [`super::worker::RunError::ProviderWait`] が運ぶ待ち。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    pub scope: WaitScope,
    pub reason: WaitReason,
    /// `Retry-After`。無ければ 60 秒。どちらも 60〜3600 秒に収める。
    pub retry_after_secs: Option<u32>,
}

impl Wait {
    pub fn new(scope: WaitScope, reason: WaitReason) -> Self {
        Self {
            scope,
            reason,
            retry_after_secs: None,
        }
    }

    /// 待つ秒数（60〜3600 秒）。
    pub fn secs(&self) -> u32 {
        self.retry_after_secs
            .unwrap_or(MIN_DEFER_SECS)
            .clamp(MIN_DEFER_SECS, MAX_DEFER_SECS)
    }
}

/// 待ちの識別。`service` が `None` の行は入口全体。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WaitKey {
    pub provider: String,
    pub service: Option<String>,
}

/// 待ちを記録する（入口全体なら `service=None`）。同じキーの既存の待ちより短くはしない（長い 429 の待ちを、
/// あとの 60 秒の待ちで縮めない）。期限の過ぎた行はここで掃除する。
pub fn set(
    conn: &mut Connection,
    key: &WaitKey,
    reason: WaitReason,
    until: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM ai_provider_waits WHERE until<=?1", [now])?;
    tx.execute(
        "INSERT INTO ai_provider_waits(provider, service, reason, until, updated_at)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(provider, service) DO UPDATE SET
           reason=excluded.reason, until=excluded.until, updated_at=excluded.updated_at
         WHERE excluded.until>=ai_provider_waits.until",
        params![
            key.provider,
            key.service.as_deref().unwrap_or(ENTRANCE),
            reason.as_str(),
            until,
            now
        ],
    )?;
    tx.commit()
}

/// `now` にまだ有効な待ち（キー→期限）。表が無い旧 DB は空。
pub fn load(conn: &Connection, now: i64) -> rusqlite::Result<HashMap<WaitKey, i64>> {
    if !super::jobs::object_exists(conn, "table", "ai_provider_waits")? {
        return Ok(HashMap::new());
    }
    let mut stmt =
        conn.prepare("SELECT provider, service, until FROM ai_provider_waits WHERE until>?1")?;
    let rows = stmt.query_map([now], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (provider, service, until) = row?;
        let service = (service != ENTRANCE).then_some(service);
        out.insert(WaitKey { provider, service }, until);
    }
    Ok(out)
}

/// 用途の待ちを判定する材料: 入口全体の待ちと、サービス単体の待ちのどちらか有効なものがあるか。
pub fn blocks(
    waits: &HashMap<WaitKey, i64>,
    provider: &str,
    service: Option<&str>,
    now: i64,
) -> bool {
    let live = |key: WaitKey| waits.get(&key).is_some_and(|until| *until > now);
    live(WaitKey {
        provider: provider.to_string(),
        service: None,
    }) || service.is_some_and(|s| {
        live(WaitKey {
            provider: provider.to_string(),
            service: Some(s.to_string()),
        })
    })
}
