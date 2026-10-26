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
//!
//! # 管理表示（`ai-jobs status`・`/api/admin/status`・管理画面 STATUS）
//!
//! [`view`] が、**まだ有効な**待ち（`until` が今より先）を、**今の設定の用途**に結び直して返す。保存された識別
//! （Provider・サービスのハッシュ）は用途名に引き直すだけで、ハッシュも接続先・サービス ID・モデル名・Provider の
//! 自由文の `reason` も表示に出さない。出すのは用途名・範囲（`entrance`/`service`）・固定の理由・期限（UTC）だけ。
//!
//! - 入口全体の待ちは、同じ Provider の識別を持つ**全ての用途**の行になる。サービス単体の待ちは、そのサービスの識別を
//!   持つ用途だけ。今の設定に当たらない待ち（古い設定の残り）は、何も止めていないので**出さない**。
//! - 設定が未指定・不正で用途に結び直せないときは、有効な待ちの**件数だけ**（`unidentified`）を返す。
//! - 期限が過ぎた待ちは出ない（行が残っていても数えない）。再起動の前後で同じ表示になる（DB の行から読み直す）。
//! - 期限は UTC の ISO 8601（`YYYY-MM-DDTHH:MM:SSZ`、秒まで）。

use std::collections::HashMap;

use rusqlite::{Connection, params};

use super::jobs::{MAX_DEFER_SECS, MIN_DEFER_SECS};
use super::settings::{AiConfig, TaskKind};

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

impl WaitScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entrance => "entrance",
            Self::Service => "service",
        }
    }
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

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Unreachable,
            Self::Leased,
            Self::NotReady,
            Self::RateLimited,
            Self::Busy,
        ]
        .into_iter()
        .find(|r| r.as_str() == text)
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

// ---------------------------------------------------------------- 管理表示

/// 今の設定での、用途ごとの待ちの識別（メモリの中だけ。表示には出さない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identities {
    /// 設定が使えて、用途に結び直せるか（未指定・不正なら `false`）。
    identifiable: bool,
    tasks: Vec<(TaskKind, String, String)>,
}

impl Identities {
    /// 今の設定から作る。未指定・不正なら結び直せない。
    pub fn from_config(ai: &AiConfig) -> Self {
        let Some(settings) = ai.settings() else {
            return Self {
                identifiable: false,
                tasks: Vec::new(),
            };
        };
        let tasks = TaskKind::ALL
            .into_iter()
            .filter_map(|task| {
                Some((
                    task,
                    settings.provider_fingerprint(task)?,
                    settings.service_key(task)?,
                ))
            })
            .collect();
        Self {
            identifiable: true,
            tasks,
        }
    }
}

/// 表示する有効な待ち 1 件（用途ごと）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveWait {
    pub task: TaskKind,
    pub scope: WaitScope,
    pub reason: WaitReason,
    /// UTC の Unix 秒。
    pub until: i64,
}

/// 表示用の待ちの集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WaitView {
    pub waits: Vec<ActiveWait>,
    /// 設定が使えず用途に結び直せない、有効な待ちの件数（値は出さない）。
    pub unidentified: usize,
}

impl WaitView {
    pub fn is_empty(&self) -> bool {
        self.waits.is_empty() && self.unidentified == 0
    }
}

/// `now` にまだ有効な待ちを、今の設定の用途に結び直して返す（読み取りだけ。表が無い旧 DB は空）。
pub fn view(conn: &Connection, now: i64, ids: &Identities) -> rusqlite::Result<WaitView> {
    let mut out = WaitView::default();
    if !super::jobs::object_exists(conn, "table", "ai_provider_waits")? {
        return Ok(out);
    }
    let mut stmt = conn
        .prepare("SELECT provider, service, reason, until FROM ai_provider_waits WHERE until>?1")?;
    let rows = stmt.query_map([now], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (provider, service, reason, until) = row?;
        let Some(reason) = WaitReason::parse(&reason) else {
            continue;
        };
        if !ids.identifiable {
            out.unidentified += 1;
            continue;
        }
        for (task, task_provider, task_service) in &ids.tasks {
            if *task_provider != provider {
                continue;
            }
            let scope = if service == ENTRANCE {
                WaitScope::Entrance
            } else if *task_service == service {
                WaitScope::Service
            } else {
                continue;
            };
            out.waits.push(ActiveWait {
                task: *task,
                scope,
                reason,
                until,
            });
        }
    }
    // 用途の順、同じ用途では入口全体が先
    out.waits
        .sort_by_key(|w| (w.task, w.scope == WaitScope::Service, w.until));
    Ok(out)
}

/// UTC の Unix 秒を `YYYY-MM-DDTHH:MM:SSZ` にする（先発グレゴリオ暦。Howard Hinnant の `civil_from_days`）。
pub fn utc_iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc_iso;

    #[test]
    fn utc_dates_are_formatted_as_iso_8601() {
        assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_iso(1_782_432_000 + 3_661), "2026-06-26T01:01:01Z");
        assert_eq!(utc_iso(-1), "1969-12-31T23:59:59Z");
    }
}
