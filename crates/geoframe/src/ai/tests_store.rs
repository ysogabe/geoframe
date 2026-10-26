//! 追加表（送信予算 `ai_request_usage`・Provider 停止 `ai_provider_pauses`）の試験。合成の DB・偽の時計
//! （`now` の UNIX 秒）だけを使い、外部通信はしない。
//!
//! - スキーマ: 追加だけで既存の表・行・`ai_jobs` を変えない／冪等／旧 DB を読み取り専用で読める
//! - 予約: UTC 日・周期窓・上限 0・競合・再起動・設定変更でも履歴を消さない・失敗しても巻き戻さない
//! - 対話的な予約: writer が塞がっていれば期限で戻り、期限切れの予約は実行されない
//! - 停止: 理由コードの保持・再起動で解除されない・関係する設定変更だけで効かなくなる・ジョブに触れない

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::budget::*;
use super::jobs;
use super::pause::{self, PauseReason, PauseScope};
use super::settings::TaskKind;
use super::tests::{dump, table_exists};
use super::tests_settings::{parse, sample, variant};
use crate::schema;
use crate::testutil::TempDir;
use crate::writer::{OpenMode, WriteError, Writer};

/// 2026-10-26T00:00:00Z（UTC 日の始まり）。
const D0: i64 = 1_792_972_800;
const DAY: i64 = 86_400;
const AI_TABLES: [&str; 4] = [
    "ai_jobs",
    "idx_ai_jobs_runnable",
    "ai_request_usage",
    "ai_provider_pauses",
];

struct Db {
    _dir: TempDir,
    path: std::path::PathBuf,
    writer: Writer,
}

fn db(tag: &str) -> Db {
    let dir = TempDir::new(tag);
    let path = dir.path().join("catalog.db");
    let writer = Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
    writer.call(schema::migrate).unwrap();
    writer.call(super::migrate).unwrap();
    Db {
        _dir: dir,
        path,
        writer,
    }
}

impl Db {
    fn reserve(&self, task: TaskKind, b: SendBudget, now: i64) -> Reservation {
        self.writer
            .call(move |c| reserve(c, task, &b, now))
            .unwrap()
    }

    fn usage(&self, task: TaskKind, now: i64, cycle_secs: u32) -> Usage {
        self.writer
            .call(move |c| usage(c, task, now, cycle_secs))
            .unwrap()
    }

    /// 書き込み用の接続を開き直す（再起動の代わり。古い writer は閉じる）。
    fn restart(self) -> Db {
        let Db { _dir, path, writer } = self;
        drop(writer);
        let writer = Writer::open(&path, OpenMode::MustExist).unwrap();
        Db { _dir, path, writer }
    }
}

fn budget(daily: u32, cycle: u32, cycle_secs: u32) -> SendBudget {
    SendBudget {
        daily,
        cycle,
        cycle_secs,
    }
}

const JUDGE: TaskKind = TaskKind::PhotoJudge;

// ---------------------------------------------------------------- スキーマ

/// 既存の（AI 以外の）表・索引の定義。AI の追加分は除く。
fn master_without_ai(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT type||':'||name||':'||COALESCE(sql,'') FROM sqlite_master
             WHERE name NOT LIKE '%ai_jobs%' AND name NOT LIKE '%ai_request_usage%'
               AND name NOT LIKE '%ai_provider_pauses%' ORDER BY rowid",
        )
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn expected_ai_objects() -> Vec<String> {
    sorted(AI_TABLES.iter().map(|s| s.to_string()).collect())
}

#[test]
fn migrate_adds_only_the_ai_tables_and_is_idempotent() {
    let dir = TempDir::new("store-migrate");
    let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO photos(source,drive_id,name,lat,lon,place,favorite,hidden,rotation,loc_source)
           VALUES ('drive','a','n1',1.0,2.0,'p',1,1,90,'manual');
         INSERT INTO photo_loc_runs VALUES ('r','2024-01-01','m','v','[]','[]','done',1,'{}','','t');",
    )
    .unwrap();
    let before_rows = dump(&conn);
    let before_master = master_without_ai(&conn);

    assert_eq!(
        sorted(super::pending(&conn).unwrap()),
        expected_ai_objects()
    );
    assert_eq!(
        sorted(super::migrate(&mut conn).unwrap()),
        expected_ai_objects()
    );
    assert!(super::pending(&conn).unwrap().is_empty());
    // 追加だけ: 既存の表の行・定義は 1 つも変わらない（Python と同じスキーマの再現にも入れない）
    assert_eq!(dump(&conn), before_rows);
    assert_eq!(master_without_ai(&conn), before_master);

    // 2 回目は何も書かない
    let changes = conn.total_changes();
    let version: i64 = conn
        .query_row("PRAGMA schema_version", [], |r| r.get(0))
        .unwrap();
    assert!(super::migrate(&mut conn).unwrap().is_empty());
    assert_eq!(conn.total_changes(), changes);
    assert_eq!(
        conn.query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        version
    );
    assert!(schema::migrate(&mut conn).unwrap().is_noop());
    assert_eq!(dump(&conn), before_rows);
}

#[test]
fn a_t4_1a_database_gets_only_the_new_tables_and_keeps_its_jobs() {
    let dir = TempDir::new("store-from-4-1a");
    let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
    schema::migrate(&mut conn).unwrap();
    // T4.1a の状態: ai_jobs だけがある（中に失敗・成功・進行中のジョブ）
    jobs::migrate(&mut conn).unwrap();
    for (n, kind) in [
        (1, jobs::JobKind::ImageEmbedding),
        (2, jobs::JobKind::PhotoJudge),
    ] {
        let key = jobs::JobKey::new(kind, format!("t{n}"), "in", "p");
        jobs::enqueue(&mut conn, &key, D0).unwrap();
    }
    let claimed = jobs::claim_next(&conn, D0, "run", "tok", &jobs::JobKind::ALL)
        .unwrap()
        .unwrap();
    jobs::record_failure(
        &mut conn,
        &claimed,
        D0,
        jobs::ErrorCode::Timeout,
        jobs::Failure::Permanent,
    )
    .unwrap();
    let jobs_before: Vec<String> = conn
        .prepare("SELECT * FROM ai_jobs ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            (0..15)
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|v| v.join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(jobs_before.len(), 2);

    assert_eq!(
        sorted(super::pending(&conn).unwrap()),
        ["ai_provider_pauses", "ai_request_usage"]
    );
    assert_eq!(
        sorted(super::migrate(&mut conn).unwrap()),
        ["ai_provider_pauses", "ai_request_usage"]
    );
    let jobs_after: Vec<String> = conn
        .prepare("SELECT * FROM ai_jobs ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            (0..15)
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|v| v.join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(jobs_after, jobs_before);
    assert!(super::migrate(&mut conn).unwrap().is_empty());
}

/// INGEST=0 の旧 DB（AI の表が無い）は、必須スキーマを満たしたまま読み取り専用で読め、表は作られない。
#[test]
fn an_old_database_without_the_ai_tables_is_readable_read_only() {
    let dir = TempDir::new("store-old-db");
    let (path, _keep) = crate::testutil::synthetic_catalog(dir.path(), 3);
    let ro = crate::db::open_read_only(&path).unwrap();
    crate::db::check_schema(&ro).unwrap();
    assert_eq!(sorted(super::pending(&ro).unwrap()), expected_ai_objects());
    for name in ["ai_jobs", "ai_request_usage", "ai_provider_pauses"] {
        assert!(!table_exists(&ro, name), "{name}");
    }
    // 停止の読み取りは表が無ければ停止なし（読み取り専用でも失敗しない）
    assert_eq!(
        pause::active(&ro, &[sample().provider_pause_key(JUDGE).unwrap()]).unwrap(),
        None
    );
    assert_eq!(pause::status(&ro, &sample(), JUDGE).unwrap(), None);
    assert!(pause::list(&ro).unwrap().is_empty());
}

#[test]
fn the_new_tables_reject_unknown_tasks_scopes_and_reasons() {
    let d = db("store-checks");
    let attempt = |sql: &'static str| d.writer.call(move |c| c.execute_batch(sql));
    assert!(matches!(
        attempt("INSERT INTO ai_request_usage VALUES ('geocode','day',0,1,0)"),
        Err(WriteError::Query(_))
    ));
    assert!(matches!(
        attempt("INSERT INTO ai_request_usage VALUES ('photo_judge','week',0,1,0)"),
        Err(WriteError::Query(_))
    ));
    assert!(matches!(
        attempt("INSERT INTO ai_request_usage VALUES ('photo_judge','day',0,-1,0)"),
        Err(WriteError::Query(_))
    ));
    assert!(matches!(
        attempt("INSERT INTO ai_provider_pauses VALUES ('provider','h','photo_judge','timeout',0)"),
        Err(WriteError::Query(_))
    ));
    assert!(matches!(
        attempt("INSERT INTO ai_provider_pauses VALUES ('global','h','photo_judge','forbidden',0)"),
        Err(WriteError::Query(_))
    ));
    attempt("INSERT INTO ai_request_usage VALUES ('photo_judge','day',0,1,0)").unwrap();
}

// ---------------------------------------------------------------- 予約

#[test]
fn the_daily_limit_is_exact_and_resets_only_at_the_next_utc_day() {
    let d = db("budget-daily");
    let b = budget(3, 100, 60);
    // 同じ UTC 日に 3 件まで
    for n in 0..3 {
        assert_eq!(
            d.reserve(JUDGE, b, D0 + 10 + n),
            Reservation::Granted,
            "{n}"
        );
    }
    assert_eq!(d.reserve(JUDGE, b, D0 + 3_600), Reservation::DailyExhausted);
    // 日の最後の秒でも同じ日。巻き戻しの無い履歴
    assert_eq!(
        d.reserve(JUDGE, b, D0 + DAY - 1),
        Reservation::DailyExhausted
    );
    // 日を跨ぐと別の日の枠（前の日の件数は残る）
    assert_eq!(d.reserve(JUDGE, b, D0 + DAY), Reservation::Granted);
    assert_eq!(d.usage(JUDGE, D0 + DAY, 60).day, 1);
    assert_eq!(d.usage(JUDGE, D0 + DAY - 1, 60).day, 3);
    // 用途ごとに別の枠
    assert_eq!(
        d.reserve(TaskKind::LocationSuggest, b, D0 + 10),
        Reservation::Granted
    );
    assert_eq!(d.usage(JUDGE, D0 + 10, 60).day, 3);
}

#[test]
fn the_cycle_window_limits_and_a_denied_reservation_consumes_nothing() {
    let d = db("budget-cycle");
    let b = budget(100, 2, 60);
    assert_eq!(d.reserve(JUDGE, b, D0), Reservation::Granted);
    assert_eq!(d.reserve(JUDGE, b, D0 + 59), Reservation::Granted);
    assert_eq!(d.reserve(JUDGE, b, D0 + 30), Reservation::CycleExhausted);
    // 拒否では日の枠も周期窓も増えない
    assert_eq!(d.usage(JUDGE, D0, 60), Usage { day: 2, cycle: 2 });
    // 次の窓は新しい枠（日の件数は積み上がる）
    assert_eq!(d.reserve(JUDGE, b, D0 + 60), Reservation::Granted);
    assert_eq!(d.usage(JUDGE, D0 + 60, 60), Usage { day: 3, cycle: 1 });

    // 日の枠が尽きたときの拒否は、周期窓を消費しない
    let tight = budget(3, 10, 60);
    assert_eq!(
        d.reserve(JUDGE, tight, D0 + 120),
        Reservation::DailyExhausted
    );
    assert_eq!(d.usage(JUDGE, D0 + 120, 60).cycle, 0);
}

#[test]
fn a_zero_limit_denies_without_touching_the_database() {
    let d = db("budget-zero");
    assert_eq!(
        d.reserve(JUDGE, budget(0, 5, 60), D0),
        Reservation::DailyExhausted
    );
    assert_eq!(
        d.reserve(JUDGE, budget(5, 0, 60), D0),
        Reservation::CycleExhausted
    );
    let rows: i64 = d
        .writer
        .call(|c| c.query_row("SELECT COUNT(*) FROM ai_request_usage", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(rows, 0);
    // 窓の長さ 0 は 1 秒として扱う（0 での除算・剰余にならない）
    assert_eq!(d.reserve(JUDGE, budget(5, 5, 0), D0), Reservation::Granted);
}

#[test]
fn concurrent_reservations_never_exceed_the_limit() {
    let d = db("budget-race");
    let b = budget(10, 10, 60);
    let granted = Arc::new(Mutex::new(0u32));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let writer = d.writer.clone();
            let granted = Arc::clone(&granted);
            thread::spawn(move || {
                for _ in 0..5 {
                    let r = writer.call(move |c| reserve(c, JUDGE, &b, D0 + 5)).unwrap();
                    if r.is_granted() {
                        *granted.lock().unwrap() += 1;
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*granted.lock().unwrap(), 10);
    assert_eq!(d.usage(JUDGE, D0 + 5, 60), Usage { day: 10, cycle: 10 });
}

#[test]
fn the_history_survives_a_restart_and_a_settings_change() {
    let d = db("budget-restart");
    let b = budget(3, 100, 60);
    for n in 0..3 {
        assert_eq!(d.reserve(JUDGE, b, D0 + n), Reservation::Granted);
    }
    // 再起動（接続を開き直す）しても上限は維持される
    let d = d.restart();
    assert_eq!(d.reserve(JUDGE, b, D0 + 100), Reservation::DailyExhausted);
    assert_eq!(d.usage(JUDGE, D0 + 100, 60).day, 3);
    // 設定の変更で上限を上げても、使った分は残る（履歴をリセットしない）
    assert_eq!(
        d.reserve(JUDGE, budget(5, 100, 60), D0 + 101),
        Reservation::Granted
    );
    assert_eq!(
        d.reserve(JUDGE, budget(5, 100, 60), D0 + 102),
        Reservation::Granted
    );
    assert_eq!(
        d.reserve(JUDGE, budget(5, 100, 60), D0 + 103),
        Reservation::DailyExhausted
    );
    // 下げれば、すでに使った分で直ちに止まる。窓の長さを変えても日の件数は同じ
    assert_eq!(
        d.reserve(JUDGE, budget(2, 100, 30), D0 + 104),
        Reservation::DailyExhausted
    );
    assert_eq!(d.usage(JUDGE, D0 + 104, 30).day, 5);
}

#[test]
fn failures_retries_and_interruptions_never_give_the_budget_back() {
    let d = db("budget-no-rollback");
    let b = budget(2, 10, 60);
    assert_eq!(d.reserve(JUDGE, b, D0), Reservation::Granted);
    // 予約のあとで同じ書き込みの依頼が失敗しても、予約は確定したまま（`reserve` が自分で確定する）
    let err = d
        .writer
        .call(move |c| -> rusqlite::Result<()> {
            assert_eq!(reserve(c, JUDGE, &b, D0 + 1)?, Reservation::Granted);
            Err(rusqlite::Error::InvalidQuery)
        })
        .unwrap_err();
    assert!(matches!(err, WriteError::Query(_)));
    assert_eq!(d.usage(JUDGE, D0 + 1, 60).day, 2);
    // ジョブの失敗・再試行・中断の回収は、送信の件数に触れない
    let key = jobs::JobKey::new(jobs::JobKind::PhotoJudge, "g", "in", "p");
    d.writer.call(move |c| jobs::enqueue(c, &key, D0)).unwrap();
    let claimed = d
        .writer
        .call(|c| jobs::claim_next(c, D0, "run-1", "tok", &jobs::JobKind::ALL))
        .unwrap()
        .unwrap();
    d.writer
        .call(move |c| {
            jobs::record_failure(
                c,
                &claimed,
                D0,
                jobs::ErrorCode::Timeout,
                jobs::Failure::Attempt,
            )
        })
        .unwrap();
    d.writer
        .call(|c| jobs::reclaim_interrupted(c, "run-2", D0 + 500))
        .unwrap();
    assert_eq!(d.usage(JUDGE, D0 + 2, 60).day, 2);
    assert_eq!(d.reserve(JUDGE, b, D0 + 3), Reservation::DailyExhausted);
    // 古い周期窓の行は掃除されるが、UTC 日の行は残る
    let later = D0 + 3 * DAY;
    assert_eq!(
        d.reserve(JUDGE, budget(5, 5, 60), later),
        Reservation::Granted
    );
    let (days, cycles): (i64, i64) = d
        .writer
        .call(|c| {
            c.query_row(
                "SELECT SUM(scope='day'), SUM(scope='cycle') FROM ai_request_usage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!((days, cycles), (2, 1));
}

// ---------------------------------------------------------------- 対話的な予約

/// writer の依頼を 1 つ、解放の合図が来るまで止めておく。止まったことを確かめてから返す。
fn block_writer(writer: &Writer) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let writer = writer.clone();
    let handle = thread::spawn(move || {
        writer
            .call(move |_| {
                started_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(())
            })
            .unwrap();
    });
    started_rx.recv().unwrap();
    (release_tx, handle)
}

#[test]
fn an_interactive_reservation_returns_busy_when_the_writer_is_blocked_and_never_runs_late() {
    let d = db("budget-busy");
    let b = budget(5, 5, 60);
    let (release, blocker) = block_writer(&d.writer);

    let started = Instant::now();
    let outcome = reserve_interactive(
        Some(&d.writer),
        JUDGE,
        b,
        D0,
        started + Duration::from_millis(100),
    );
    let waited = started.elapsed();
    assert_eq!(outcome, Reservation::Busy);
    assert!(!outcome.is_granted());
    assert!(waited >= Duration::from_millis(100), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");

    // writer が戻っても、期限を過ぎてキューに残った予約は実行されない（送信許可は復活しない）
    release.send(()).unwrap();
    blocker.join().unwrap();
    d.writer.call(|c| c.execute_batch("SELECT 1")).unwrap();
    assert_eq!(d.usage(JUDGE, D0, 60), Usage::default());
    // 空いていれば同じ呼び出しは通る
    assert_eq!(
        reserve_interactive(
            Some(&d.writer),
            JUDGE,
            b,
            D0,
            Instant::now() + INTERACTIVE_RESERVE_WAIT
        ),
        Reservation::Granted
    );
    assert_eq!(d.usage(JUDGE, D0, 60), Usage { day: 1, cycle: 1 });
}

#[test]
fn a_reservation_that_had_already_started_stays_consumed_after_the_waiter_left() {
    let d = db("budget-late-commit");
    let b = budget(5, 5, 60);
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    // 予約より先に始まって居座る依頼は作れないので、予約の中身を遅くする（同じ writer 経路の `call_within`）
    let writer = d.writer.clone();
    let waiter = thread::spawn(move || {
        writer.call_within(Instant::now() + Duration::from_millis(100), move |c| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
            reserve(c, JUDGE, &b, D0)
        })
    });
    started_rx.recv().unwrap();
    // 待つ側は期限で去る
    assert!(matches!(waiter.join().unwrap(), Err(WriteError::Expired)));
    // 始まっていた予約は最後まで走って確定する: 枠は消費済み（保守的）で、巻き戻らない
    release_tx.send(()).unwrap();
    d.writer.call(|c| c.execute_batch("SELECT 1")).unwrap();
    assert_eq!(d.usage(JUDGE, D0, 60), Usage { day: 1, cycle: 1 });
}

#[test]
fn without_a_writer_or_with_a_passed_deadline_nothing_is_granted() {
    let d = db("budget-no-writer");
    let b = budget(5, 5, 60);
    // writer が無い（INGEST=0）: 予算を確かめられないので送らない
    assert_eq!(
        reserve_interactive(
            None,
            JUDGE,
            b,
            D0,
            Instant::now() + INTERACTIVE_RESERVE_WAIT
        ),
        Reservation::NoWriter
    );
    // 期限がすでに過ぎている依頼は、writer が空いていても実行されない
    assert_eq!(
        reserve_interactive(Some(&d.writer), JUDGE, b, D0, Instant::now()),
        Reservation::Busy
    );
    assert_eq!(d.usage(JUDGE, D0, 60), Usage::default());
    // 表が無いなど DB の失敗は Failed（送らない）
    let bare = TempDir::new("budget-no-table");
    let w = Writer::open(&bare.path().join("bare.db"), OpenMode::CreateIfMissing).unwrap();
    let outcome = reserve_interactive(
        Some(&w),
        JUDGE,
        b,
        D0,
        Instant::now() + INTERACTIVE_RESERVE_WAIT,
    );
    assert_eq!(outcome, Reservation::Failed);
}

// ---------------------------------------------------------------- Provider の停止

const PAUSE_TASK: TaskKind = TaskKind::PhotoJudge;

impl Db {
    fn pause(&self, s: &super::settings::AiSettings, reason: PauseReason, now: i64) -> bool {
        let key = s.provider_pause_key(PAUSE_TASK).unwrap();
        self.writer
            .call(move |c| pause::pause(c, PAUSE_TASK, &key, reason, now))
            .unwrap()
    }

    fn paused(&self, s: &super::settings::AiSettings, task: TaskKind) -> Option<pause::Paused> {
        let s = s.clone();
        self.writer
            .call(move |c| pause::status(c, &s, task))
            .unwrap()
    }
}

#[test]
fn a_pause_keeps_a_fixed_reason_survives_restart_and_is_released_explicitly() {
    let d = db("pause-basic");
    let s = sample();
    assert_eq!(d.paused(&s, PAUSE_TASK), None);
    assert!(d.pause(&s, PauseReason::Unauthorized, D0));
    // 同じ停止は上書きしない（最初の理由と時刻を保つ）
    assert!(!d.pause(&s, PauseReason::Forbidden, D0 + 10));
    let paused = d.paused(&s, PAUSE_TASK).unwrap();
    assert_eq!(
        (paused.scope, paused.reason),
        (PauseScope::Provider, PauseReason::Unauthorized)
    );
    // 同じ接続先・認証の別の用途も止まる（クラウドの location_suggest は同じ認証・project）
    assert_eq!(
        d.paused(&s, TaskKind::LocationSuggest).unwrap().reason,
        PauseReason::Unauthorized
    );
    // 別の接続先（LAN）の用途は止まらない
    assert_eq!(d.paused(&s, TaskKind::ImageEmbedding), None);

    // 再起動だけでは解除されない
    let d = d.restart();
    assert_eq!(
        d.paused(&s, PAUSE_TASK).unwrap().reason,
        PauseReason::Unauthorized
    );
    let listed = d.writer.call(|c| pause::list(c)).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].task, listed[0].created_at), (PAUSE_TASK, D0));

    // 管理 CLI の明示の解除（足場）
    let key = s.provider_pause_key(PAUSE_TASK).unwrap();
    assert!(d.writer.call(move |c| pause::release(c, &key)).unwrap());
    assert_eq!(d.paused(&s, PAUSE_TASK), None);
    // 全部の解除
    assert!(d.pause(&s, PauseReason::ContractMismatch, D0 + 20));
    assert_eq!(d.writer.call(pause::release_all).unwrap(), 1);
    assert!(d.writer.call(|c| pause::list(c)).unwrap().is_empty());
}

#[test]
fn a_model_pause_is_per_task_and_the_model_change_clears_it() {
    let d = db("pause-model");
    let s = sample();
    let key = s.task_pause_key(PAUSE_TASK).unwrap();
    assert!(
        d.writer
            .call(move |c| pause::pause(c, PAUSE_TASK, &key, PauseReason::ModelNotFound, D0))
            .unwrap()
    );
    let p = d.paused(&s, PAUSE_TASK).unwrap();
    assert_eq!(
        (p.scope, p.reason),
        (PauseScope::Task, PauseReason::ModelNotFound)
    );
    // 同じ接続先の別の用途は止まらない
    assert_eq!(d.paused(&s, TaskKind::LocationSuggest), None);
    // モデルを直すと、その停止は効かなくなる（認証は同じ）
    let fixed = parse(&variant(
        r#""model": "example-judge""#,
        r#""model": "example-judge-2""#,
    ))
    .unwrap();
    assert_eq!(d.paused(&fixed, PAUSE_TASK), None);
}

#[test]
fn unrelated_setting_changes_do_not_release_a_pause_but_destination_changes_do() {
    let d = db("pause-config");
    let s = sample();
    assert!(d.pause(&s, PauseReason::Forbidden, D0));
    let current = |s: &super::settings::AiSettings| {
        TaskKind::ALL
            .into_iter()
            .flat_map(|t| [s.provider_pause_key(t), s.task_pause_key(t)])
            .flatten()
            .collect::<Vec<_>>()
    };
    // 関係しない変更（別の用途の許可・上限・include_hidden・別の用途のモデル・許可の取り消し・上限の変更）
    for (from, to) in [
        (r#""allow_cloud": true"#, r#""allow_cloud": false"#),
        (
            r#""daily_request_limit": 20"#,
            r#""daily_request_limit": 3"#,
        ),
        (
            r#""allow_cloud": true,"#,
            r#""allow_cloud": true, "include_hidden": true,"#,
        ),
        (
            r#""model": "example-suggest""#,
            r#""model": "example-suggest-2""#,
        ),
        (
            r#""model": "example-embed""#,
            r#""model": "example-embed-2""#,
        ),
    ] {
        let v = parse(&variant(from, to)).unwrap();
        assert!(d.paused(&v, PAUSE_TASK).is_some(), "{from}");
        // 整理しても消えない
        let keys = current(&v);
        assert_eq!(
            d.writer
                .call(move |c| pause::release_unmatched(c, &keys))
                .unwrap(),
            0,
            "{from}"
        );
    }
    // 認証・接続先が変わると効かなくなり、整理で消える（同じ認証を使う別の用途も含めて project を変える）
    let moved =
        parse(&super::tests_settings::SAMPLE.replace("example-project", "example-project-b"))
            .unwrap();
    assert_eq!(d.paused(&moved, PAUSE_TASK), None);
    let keys = current(&moved);
    assert_eq!(
        d.writer
            .call(move |c| pause::release_unmatched(c, &keys))
            .unwrap(),
        1
    );
    assert!(d.writer.call(|c| pause::list(c)).unwrap().is_empty());
}

#[test]
fn pausing_a_provider_never_touches_the_jobs() {
    let d = db("pause-jobs");
    let key = jobs::JobKey::new(jobs::JobKind::PhotoJudge, "g1", "in", "p");
    let other = jobs::JobKey::new(jobs::JobKind::ImageEmbedding, "p1", "in", "p");
    for k in [key.clone(), other.clone()] {
        d.writer.call(move |c| jobs::enqueue(c, &k, D0)).unwrap();
    }
    let claimed = d
        .writer
        .call(|c| jobs::claim_next(c, D0, "run", "tok", &[jobs::JobKind::PhotoJudge]))
        .unwrap()
        .unwrap();
    let before = d
        .writer
        .call(|c| c.query_row("SELECT group_concat(state||':'||attempts||':'||defer_count, ','), COUNT(*) FROM ai_jobs", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))))
        .unwrap();
    let s = sample();
    assert!(d.pause(&s, PauseReason::Unauthorized, D0 + 1));
    assert!(!d.pause(&s, PauseReason::ContractMismatch, D0 + 2));
    let after = d
        .writer
        .call(|c| c.query_row("SELECT group_concat(state||':'||attempts||':'||defer_count, ','), COUNT(*) FROM ai_jobs", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))))
        .unwrap();
    assert_eq!(after, before);
    // 停止中も、実行中のジョブの結果の確定・ほかのジョブの claim は通常どおり（停止の判断は呼び出し側）
    let done = d
        .writer
        .call(move |c| jobs::complete(c, &claimed, D0 + 3, |_| Ok(jobs::InputCheck::Current)))
        .unwrap();
    assert_eq!(done, jobs::Completion::Succeeded);
    assert!(
        d.writer
            .call(|c| jobs::claim_next(c, D0 + 4, "run", "tok2", &jobs::JobKind::ALL))
            .unwrap()
            .is_some()
    );
    // 一覧に出るのは固定の情報だけ
    let listed = d.writer.call(|c| pause::list(c)).unwrap();
    assert_eq!(listed.len(), 1);
    let text = format!("{listed:?} {:?}", s.provider_pause_key(PAUSE_TASK));
    assert!(!text.contains("example-project"), "{text}");
    // 保存された行にも、設定の値は入らない
    let stored: String = d
        .writer
        .call(|c| {
            c.query_row(
                "SELECT scope||task||reason FROM ai_provider_pauses",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(stored, "providerphoto_judgeunauthorized");
}
