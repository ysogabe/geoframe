//! クラウドへの送信の予算 `ai_request_usage`（P4 / T4.1b。設計は p4-design.md §4.3 の終わり）。
//!
//! 課金され得る要求の**件数**の上限（金額の保証ではない）。外部へ送る直前に writer の短いトランザクションで
//! 枠を予約し（[`reserve`]）、予約できたときだけ送る。失敗・時間切れ・中断・再試行をしても消費は巻き戻さない
//! （戻す関数は無い）。履歴は用途と時間窓だけをキーにするので、再起動・設定の変更（上限・モデル・許可）でも
//! 消えない。設定のハッシュはここに入れない。
//!
//! # 窓
//!
//! - **UTC 日**: `now` を 86400 秒で切った日の始まり（UNIX 秒）をキーにした固定窓。
//! - **周期窓**: 直近 `cycle_secs` 秒の**ローリング窓**。`cycle_secs` は AI ワーカーの周期
//!   （`GEOFRAME_AI_POLL_S`。検索語は 60 秒）。予約ごとに「予約した時刻（秒）」をキーに件数を足し、
//!   `now - cycle_secs` より後の件数を合算する。履歴のキーに窓の長さを含めないので、周期を設定で変えても
//!   （長くしても短くしても）窓の中の消費は同じ数だけ数えられ、枠は復活しない。以前の固定窓の行
//!   （`window_start` が窓の始まり）も、最後に足した時刻 `updated_at` の予約として数える（保守的。移行は不要）。
//!   周期窓の行は 2 日を過ぎたら予約のたびに掃除する（周期は最長 [`MAX_CYCLE_SECS`] に丸める）。
//!   UTC 日の行は消さない（監査。時計の高水位の根拠でもある）。
//!
//! # 時計の巻き戻し
//!
//! 履歴の `updated_at` の最大値（**高水位**。全用途の全行。UTC 日の行は消さないので掃除で下がらない）より
//! `now` が小さいときは、何も書かずに [`Reservation::ClockRewound`] で拒否する。時計を戻して古い窓や
//! 掃除済みの行を作り直し、使った枠を復活させることを防ぐ。再起動をまたいでも DB の履歴から求める。
//! 秒単位の小さな後戻りでも拒否するが、`Granted` 以外は送らないので、次の周期に再試行すればよい。
//!
//! 両方の上限を 1 つのトランザクションで読み、**両方とも残りがあるときだけ**両方を 1 増やす。どちらかが
//! 尽きていれば何も書かない（日の枠だけ減って周期窓が拒否する、を作らない）。上限 0 は DB に触れず拒否。
//!
//! # 対話的な予約
//!
//! 検索語のクラウド Embedding など利用者を待たせる予約は [`reserve_interactive`]。writer の順番待ちを含め
//! 期限（既定 [`INTERACTIVE_RESERVE_WAIT`] = 200ms）で戻り、期限を過ぎたキュー内の予約は実行されない
//! （[`Writer::call_within`]）。`Granted` 以外では**送らない**。期限切れで戻ったあとに予約が確定していても、
//! 待つ側は去っているので送信許可は戻らない（枠は消費済みのまま。保守的）。返信が溜まっていても、受け取ったのが
//! 期限のあとなら同じく `Busy`（[`Writer::call_within`]）。writer が無い（`INGEST=0`）ときは
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
/// 周期窓の長さの上限（秒）。設定の層（`GEOFRAME_AI_POLL_S`）が丸める上限と同じ。
pub const MAX_CYCLE_SECS: u32 = 86_400;
/// 周期窓の行を残す期間（秒）。窓は最長 [`MAX_CYCLE_SECS`]（1 日）なので 2 日あれば足りる。
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
    /// 周期窓の長さ（秒。0 は 1、[`MAX_CYCLE_SECS`] 超はその値として扱う）。
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
    /// 時計が履歴の最後の予約より前に戻っている（[`Reservation`] の上の「時計の巻き戻し」）。送らない。
    ClockRewound,
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

fn cycle_window(cycle_secs: u32) -> i64 {
    i64::from(cycle_secs.clamp(1, MAX_CYCLE_SECS))
}

fn day_count(conn: &Connection, task: TaskKind, day: i64) -> rusqlite::Result<u32> {
    let count: Option<i64> = conn
        .query_row(
            "SELECT count FROM ai_request_usage WHERE task=?1 AND scope='day' AND window_start=?2",
            params![task.as_str(), day],
            |row| row.get(0),
        )
        .optional()?;
    Ok(u32::try_from(count.unwrap_or(0)).unwrap_or(u32::MAX))
}

/// `now` から遡って `cycle_secs` 秒より新しい（`updated_at > now - cycle_secs`）周期窓の件数の合計。
/// `now` より後の行（時計が戻った状態の表示）も数える。
fn cycle_count(
    conn: &Connection,
    task: TaskKind,
    now: i64,
    cycle_secs: u32,
) -> rusqlite::Result<u32> {
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(count), 0) FROM ai_request_usage
         WHERE task=?1 AND scope='cycle' AND updated_at>?2",
        params![task.as_str(), now.saturating_sub(cycle_window(cycle_secs))],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(total).unwrap_or(u32::MAX))
}

/// 履歴の高水位（全用途・全行の `updated_at` の最大。履歴が無ければ `None`）。
fn high_water(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT MAX(updated_at) FROM ai_request_usage", [], |row| {
        row.get(0)
    })
}

/// `now`（UTC の UNIX 秒）の時点の件数。
pub fn usage(
    conn: &Connection,
    task: TaskKind,
    now: i64,
    cycle_secs: u32,
) -> rusqlite::Result<Usage> {
    Ok(Usage {
        day: day_count(conn, task, day_start(now))?,
        cycle: cycle_count(conn, task, now, cycle_secs)?,
    })
}

/// 送信の枠を 1 つ予約する（writer の接続で。1 回の短い `IMMEDIATE` トランザクション）。
/// 予約できれば日・周期窓の両方を 1 増やして確定し、できなければ何も書かない。
///
/// `now` は writer の依頼の**中で**（順番待ちのあとに）取ること。待つ前に取った時刻だと、あとから取った別の
/// 依頼が先に確定したときに高水位より前になり、正しい時計でも [`Reservation::ClockRewound`] で拒否される
/// （送らない側に倒れるだけで過剰な送信にはならない）。
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
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if high_water(&tx)?.is_some_and(|latest| now < latest) {
        return Ok(Reservation::ClockRewound);
    }
    if day_count(&tx, task, day)? >= budget.daily {
        return Ok(Reservation::DailyExhausted);
    }
    if cycle_count(&tx, task, now, budget.cycle_secs)? >= budget.cycle {
        return Ok(Reservation::CycleExhausted);
    }
    // 周期窓は予約した時刻（秒）をキーにする（窓の長さをキーに含めない）。UTC 日は日の始まり
    for (scope, start) in [("day", day), ("cycle", now)] {
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

/// 利用者を待たせる予約。`now` は呼び出しの直前に取る（上の [`reserve`] の注意。順番待ちが短い対話用なので
/// 実際の拒否は稀）。`writer` が無ければ [`Reservation::NoWriter`]。順番待ちを含めて `deadline` までに
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
