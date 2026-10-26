//! クラウドへの送信の予算 `ai_request_usage`（P4 / T4.1b。設計は p4-design.md §4.3 の終わり）。
//!
//! 課金され得る要求の**件数**の上限（金額の保証ではない）。外部へ送る直前に writer の短いトランザクションで
//! 枠を予約し（[`reserve`]）、予約できたときだけ送る。失敗・時間切れ・中断・再試行をしても消費は巻き戻さない
//! （戻す関数は無い）。履歴は用途と時間窓だけをキーにするので、再起動・設定の変更（上限・モデル・許可）でも
//! 消えない。設定のハッシュはここに入れない。
//!
//! # 窓
//!
//! - **UTC 日**: `now` を 86400 秒で切った日の始まり（UNIX 秒）。
//! - **周期窓**: `now` を `cycle_secs` 秒で切った窓の始まり（UNIX 秒の固定窓）。`cycle_secs` は AI ワーカーの
//!   周期（`GEOFRAME_AI_POLL_S`。検索語は 60 秒）。窓の長さを設定で変えると、同じ始まりの行は合算される
//!   （保守的）。古い周期窓の行は予約のたびに掃除する。UTC 日の行は残す（監査）。
//!
//! 両方の上限を 1 つのトランザクションで読み、**両方とも残りがあるときだけ**両方を 1 増やす。どちらかが
//! 尽きていれば何も書かない（日の枠だけ減って周期窓が拒否する、を作らない）。上限 0 は DB に触れず拒否。
//!
//! # 対話的な予約
//!
//! 検索語のクラウド Embedding など利用者を待たせる予約は [`reserve_interactive`]。writer の順番待ちを含め
//! 期限（既定 [`INTERACTIVE_RESERVE_WAIT`] = 200ms）で戻り、期限を過ぎたキュー内の予約は実行されない
//! （[`Writer::call_within`]）。`Granted` 以外では**送らない**。期限切れで戻ったあとに予約が確定していても、
//! 待つ側は去っているので送信許可は戻らない（枠は消費済みのまま。保守的）。writer が無い（`INGEST=0`）ときは
//! 予約できないので、クラウドへは送らない（[`Reservation::NoWriter`]）。背景の予約は
//! `writer.call(|conn| budget::reserve(conn, ..))`。

use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::settings::TaskKind;
use crate::writer::{WriteError, Writer};

pub const TABLES: &[&str] = &["ai_request_usage"];

/// 対話的な予約が writer を待つ上限。
pub const INTERACTIVE_RESERVE_WAIT: Duration = Duration::from_millis(200);

const DAY_SECS: i64 = 86_400;
/// 周期窓の行を残す期間（秒）。周期は最長 1 日（`GEOFRAME_AI_POLL_S` の上限）なので 2 日あれば足りる。
const CYCLE_ROW_KEEP_SECS: i64 = 2 * DAY_SECS;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_request_usage(
  task TEXT NOT NULL CHECK(task IN
    ('image_embedding','query_embedding','photo_judge','location_clue','location_suggest')),
  scope TEXT NOT NULL CHECK(scope IN ('day','cycle')),
  window_start INTEGER NOT NULL,
  count INTEGER NOT NULL DEFAULT 0 CHECK(count>=0),
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(task, scope, window_start)
) WITHOUT ROWID;";

/// 1 つの用途の送信の上限。クラウドの用途だけが持つ（[`super::settings::AiConfig::gate`] が作る）。
/// 上限がどちらも必須なので、「上限なし」は表せない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendBudget {
    /// 1 UTC 日の送信数。
    pub daily: u32,
    /// 1 周期窓の送信数。
    pub cycle: u32,
    /// 周期窓の長さ（秒。0 は 1 として扱う）。
    pub cycle_secs: u32,
}

/// 予約の結果。`Granted` のときだけ送ってよい。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reservation {
    Granted,
    /// 今日の上限に達している（上限 0 を含む）。
    DailyExhausted,
    /// この周期窓の上限に達している（上限 0 を含む）。
    CycleExhausted,
    /// writer の順番待ちが期限を過ぎた（対話的な予約だけ）。送らない。
    Busy,
    /// writer が無い（`INGEST=0`）。予算を確かめられないので送らない。
    NoWriter,
    /// DB の失敗。送らない。
    Failed,
}

impl Reservation {
    pub fn is_granted(self) -> bool {
        self == Self::Granted
    }
}

/// 現在の件数（状態表示・試験用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub day: u32,
    pub cycle: u32,
}

fn day_start(now: i64) -> i64 {
    now - now.rem_euclid(DAY_SECS)
}

fn cycle_start(now: i64, cycle_secs: u32) -> i64 {
    let secs = i64::from(cycle_secs.max(1));
    now - now.rem_euclid(secs)
}

fn count_of(
    conn: &Connection,
    task: TaskKind,
    scope: &str,
    window_start: i64,
) -> rusqlite::Result<u32> {
    let count: Option<i64> = conn
        .query_row(
            "SELECT count FROM ai_request_usage WHERE task=?1 AND scope=?2 AND window_start=?3",
            params![task.as_str(), scope, window_start],
            |row| row.get(0),
        )
        .optional()?;
    Ok(u32::try_from(count.unwrap_or(0)).unwrap_or(u32::MAX))
}

/// `now`（UTC の UNIX 秒）の時点の件数。
pub fn usage(
    conn: &Connection,
    task: TaskKind,
    now: i64,
    cycle_secs: u32,
) -> rusqlite::Result<Usage> {
    Ok(Usage {
        day: count_of(conn, task, "day", day_start(now))?,
        cycle: count_of(conn, task, "cycle", cycle_start(now, cycle_secs))?,
    })
}

/// 送信の枠を 1 つ予約する（writer の接続で。1 回の短い `IMMEDIATE` トランザクション）。
/// 予約できれば日・周期窓の両方を 1 増やして確定し、できなければ何も書かない。
pub fn reserve(
    conn: &mut Connection,
    task: TaskKind,
    budget: &SendBudget,
    now: i64,
) -> rusqlite::Result<Reservation> {
    // 上限 0 は、DB を見るまでもなく送れない（未設定・0 の扱いは設定の層でも同じ）
    if budget.daily == 0 {
        return Ok(Reservation::DailyExhausted);
    }
    if budget.cycle == 0 {
        return Ok(Reservation::CycleExhausted);
    }
    let day = day_start(now);
    let cycle = cycle_start(now, budget.cycle_secs);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if count_of(&tx, task, "day", day)? >= budget.daily {
        return Ok(Reservation::DailyExhausted);
    }
    if count_of(&tx, task, "cycle", cycle)? >= budget.cycle {
        return Ok(Reservation::CycleExhausted);
    }
    for (scope, start) in [("day", day), ("cycle", cycle)] {
        tx.execute(
            "INSERT INTO ai_request_usage(task, scope, window_start, count, updated_at)
             VALUES (?1,?2,?3,1,?4)
             ON CONFLICT(task, scope, window_start)
             DO UPDATE SET count=count+1, updated_at=excluded.updated_at",
            params![task.as_str(), scope, start, now],
        )?;
    }
    tx.execute(
        "DELETE FROM ai_request_usage WHERE scope='cycle' AND window_start<?1",
        [now - CYCLE_ROW_KEEP_SECS],
    )?;
    tx.commit()?;
    Ok(Reservation::Granted)
}

/// 利用者を待たせる予約。`writer` が無ければ [`Reservation::NoWriter`]。順番待ちを含めて `deadline` までに
/// 始まらなければ [`Reservation::Busy`]（その予約は実行されない）。`Granted` 以外では送らない。
pub fn reserve_interactive(
    writer: Option<&Writer>,
    task: TaskKind,
    budget: SendBudget,
    now: i64,
    deadline: Instant,
) -> Reservation {
    let Some(writer) = writer else {
        return Reservation::NoWriter;
    };
    match writer.call_within(deadline, move |conn| reserve(conn, task, &budget, now)) {
        Ok(reservation) => reservation,
        Err(WriteError::Expired) => Reservation::Busy,
        Err(_) => Reservation::Failed,
    }
}
