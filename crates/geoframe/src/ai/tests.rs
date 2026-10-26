//! 永続ジョブの試験。合成の DB・偽の時計（`now` の整数）だけを使い、外部通信はしない。
//!
//! - スキーマ: 追加だけで既存の表・行を変えない／冪等／旧 DB を読み取り専用で読める
//! - enqueue の冪等・世代の無効化
//! - claim・token・起動 ID、旧い応答の拒否
//! - 結果の保存と `succeeded` の原子性（入力が古ければ結果 0・`obsolete`、保存の失敗で成功だけ残らない）
//! - 後回し・失敗 5 回・反復失敗・中断 3 回・起動回収・手動の再試行

use rusqlite::{Connection, params};

use super::jobs::*;
use crate::schema;
use crate::testutil::TempDir;
use crate::writer::{OpenMode, WriteError, Writer};

const T0: i64 = 1_000_000;

fn key(kind: JobKind, target: &str, input: &str) -> JobKey {
    JobKey::new(kind, target, input, "proc-1")
}

fn embed(target: &str, input: &str) -> JobKey {
    key(JobKind::ImageEmbedding, target, input)
}

/// 写真の表（Python と同じスキーマ）と `ai_jobs` を持つ DB と、その writer。
struct Setup {
    _dir: TempDir,
    writer: Writer,
    path: std::path::PathBuf,
}

fn setup(tag: &str) -> Setup {
    let dir = TempDir::new(tag);
    let path = dir.path().join("catalog.db");
    let writer = Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
    writer.call(schema::migrate).unwrap();
    writer.call(migrate).unwrap();
    // 結果の保存先の合成の表（本物の結果表は後続のタスク）
    writer
        .call(|c| {
            c.execute_batch(
                "CREATE TABLE ai_test_results(job_id INTEGER NOT NULL, photo_id INTEGER NOT NULL)",
            )
        })
        .unwrap();
    Setup {
        _dir: dir,
        writer,
        path,
    }
}

impl Setup {
    fn enqueue(&self, k: &JobKey, now: i64) -> Enqueued {
        let k = k.clone();
        self.writer.call(move |c| enqueue(c, &k, now)).unwrap()
    }

    fn claim(&self, now: i64, run_id: &str, token: &str) -> Option<Claimed> {
        let (run_id, token) = (run_id.to_string(), token.to_string());
        self.writer
            .call(move |c| claim_next(c, now, &run_id, &token, &JobKind::ALL))
            .unwrap()
    }

    fn fail(&self, claimed: &Claimed, now: i64, failure: Failure) -> Option<JobState> {
        let claimed = claimed.clone();
        self.writer
            .call(move |c| record_failure(c, &claimed, now, ErrorCode::Timeout, failure))
            .unwrap()
    }

    fn sql<T: rusqlite::types::FromSql + Send + 'static>(&self, sql: &'static str) -> T {
        self.writer
            .call(move |c| c.query_row(sql, [], |r| r.get(0)))
            .unwrap()
    }

    fn row(&self, id: i64) -> Row {
        self.writer
            .call(move |c| {
                c.query_row(
                    "SELECT state, attempts, defer_count, interruption_count, next_attempt_at,
                            last_error_code, claim_token IS NOT NULL
                     FROM ai_jobs WHERE id=?1",
                    [id],
                    |r| {
                        Ok(Row {
                            state: JobState::parse(&r.get::<_, String>(0)?).unwrap(),
                            attempts: r.get(1)?,
                            defer_count: r.get(2)?,
                            interruptions: r.get(3)?,
                            next: r.get(4)?,
                            code: r.get(5)?,
                            has_token: r.get(6)?,
                        })
                    },
                )
            })
            .unwrap()
    }

    fn results(&self) -> i64 {
        self.sql("SELECT COUNT(*) FROM ai_test_results")
    }
}

#[derive(Debug, PartialEq)]
struct Row {
    state: JobState,
    attempts: u32,
    defer_count: u32,
    interruptions: u32,
    next: i64,
    code: String,
    has_token: bool,
}

// ---------------------------------------------------------------- スキーマ

pub(super) fn dump(conn: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    for table in schema::TABLES {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| {
                        r.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format!("{v:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|v| format!("{table}:{}", v.join("|")))
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

fn master(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT type||':'||name||':'||COALESCE(sql,'') FROM sqlite_master WHERE name NOT LIKE '%ai_jobs%' AND name NOT LIKE 'sqlite_autoindex_ai_jobs%' ORDER BY rowid")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn migrate_adds_only_ai_jobs_and_is_idempotent() {
    let dir = TempDir::new("ai-migrate");
    let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO photos(source,drive_id,name,lat,lon,place,favorite,hidden,rotation,loc_source)
           VALUES ('drive','a','n1',1.0,2.0,'p',1,1,90,'manual');
         INSERT INTO photo_ai_features(photo_id,display,content_sha256,embedding_model,
           embedding_dim,embedding,analyzed_at) VALUES (1,'d','sha','m',1,x'00','t');
         INSERT INTO photo_embeddings VALUES ('sha','m',1,x'00','t');
         INSERT INTO photo_loc_runs VALUES ('r','2024-01-01','m','v','[]','[]','done',1,'{}','','t');",
    )
    .unwrap();
    assert!(
        !table_exists(&conn, "ai_jobs"),
        "Python と同じスキーマの再現には ai_jobs を入れない"
    );
    let before_rows = dump(&conn);
    let before_master = master(&conn);

    // dry-run 相当は何があるか教える
    assert_eq!(pending(&conn).unwrap(), ["ai_jobs", "idx_ai_jobs_runnable"]);
    assert_eq!(
        migrate(&mut conn).unwrap(),
        ["ai_jobs", "idx_ai_jobs_runnable"]
    );
    assert!(pending(&conn).unwrap().is_empty());
    // 追加だけ: 既存の表の行・定義は 1 つも変わらない（写真・旧 AI・手動の列）
    assert_eq!(dump(&conn), before_rows);
    assert_eq!(master(&conn), before_master);

    // 2 回目は何も書かない
    let changes = conn.total_changes();
    let version: i64 = conn
        .query_row("PRAGMA schema_version", [], |r| r.get(0))
        .unwrap();
    assert!(migrate(&mut conn).unwrap().is_empty());
    assert_eq!(conn.total_changes(), changes);
    assert_eq!(
        conn.query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        version
    );
    // schema::migrate を毎起動やっても ai_jobs を壊さず、何も変えない
    assert!(schema::migrate(&mut conn).unwrap().is_noop());
    assert_eq!(dump(&conn), before_rows);
}

pub(super) fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

/// INGEST=0 の旧 DB（ai_jobs が無い）は、必須スキーマを満たしたまま読み取り専用で読め、件数は空。
#[test]
fn an_old_database_without_ai_jobs_is_readable_read_only() {
    let dir = TempDir::new("ai-old-db");
    let (path, _keep) = crate::testutil::synthetic_catalog(dir.path(), 3);
    let ro = crate::db::open_read_only(&path).unwrap();
    crate::db::check_schema(&ro).unwrap();
    assert!(status_counts(&ro).unwrap().is_empty());
    assert_eq!(pending(&ro).unwrap().len(), 2);
    // 読み取り専用の接続は ai_jobs を作れない（作らない）
    assert!(!table_exists(&ro, "ai_jobs"));
}

#[test]
fn the_table_rejects_unknown_kinds_and_states() {
    let s = setup("ai-check");
    let bad_kind = s.writer.call(|c| {
        c.execute(
            "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,created_at,updated_at)
             VALUES ('query_embedding','t','h','p',0,0)",
            [],
        )
    });
    assert!(matches!(bad_kind, Err(WriteError::Query(_))));
    for kind in JobKind::ALL {
        assert_eq!(s.enqueue(&key(kind, "t", "h"), T0), Enqueued::Created);
    }
    let bad_state = s
        .writer
        .call(|c| c.execute("UPDATE ai_jobs SET state='weird'", []));
    assert!(matches!(bad_state, Err(WriteError::Query(_))));
}

// ---------------------------------------------------------------- enqueue

#[test]
fn enqueue_is_idempotent_and_never_resets_a_finished_job() {
    let s = setup("ai-enqueue");
    let k = embed("sha1", "gen1");
    assert_eq!(s.enqueue(&k, T0), Enqueued::Created);
    assert_eq!(s.enqueue(&k, T0 + 1), Enqueued::Existing(JobState::Pending));
    assert_eq!(s.sql::<i64>("SELECT COUNT(*) FROM ai_jobs"), 1);

    // 成功済み: 何度積んでも変わらない（再課金しない）
    let c = s.claim(T0, "run", "tok").unwrap();
    let done = s
        .writer
        .call({
            let c = c.clone();
            move |conn| complete(conn, &c, T0 + 5, |_| Ok(InputCheck::Current))
        })
        .unwrap();
    assert_eq!(done, Completion::Succeeded);
    let after = s.row(c.id);
    assert_eq!(
        s.enqueue(&k, T0 + 100),
        Enqueued::Existing(JobState::Succeeded)
    );
    assert_eq!(s.row(c.id), after);

    // failed も勝手には戻さない（手動の再試行だけ）
    let k2 = embed("sha2", "gen1");
    s.enqueue(&k2, T0);
    let c2 = s.claim(T0, "run", "tok2").unwrap();
    s.fail(&c2, T0, Failure::Permanent);
    assert_eq!(
        s.enqueue(&k2, T0 + 100),
        Enqueued::Existing(JobState::Failed)
    );
    assert_eq!(s.row(c2.id).state, JobState::Failed);

    // 種類・対象・入力世代・処理版のどれかが違えば別のジョブ
    assert_eq!(s.enqueue(&embed("sha1", "gen2"), T0), Enqueued::Created);
    assert_eq!(
        s.enqueue(&key(JobKind::PhotoJudge, "sha1", "gen1"), T0),
        Enqueued::Created
    );
    assert_eq!(
        s.enqueue(
            &JobKey::new(JobKind::ImageEmbedding, "sha1", "gen1", "proc-2"),
            T0
        ),
        Enqueued::Created
    );
}

#[test]
fn a_new_input_generation_obsoletes_the_old_jobs_but_not_results() {
    let s = setup("ai-supersede");
    let old = embed("photo", "gen1");
    let ok = embed("photo", "gen0");
    s.enqueue(&ok, T0);
    let done = s.claim(T0, "run", "t0").unwrap();
    s.writer
        .call({
            let done = done.clone();
            move |c| complete(c, &done, T0, |_| Ok(InputCheck::Current))
        })
        .unwrap();
    s.enqueue(&old, T0);
    let running = s.claim(T0, "run", "t1").unwrap();
    s.enqueue(&embed("photo", "gen1b"), T0); // pending（別世代）

    let current = embed("photo", "gen2");
    s.enqueue(&current, T0);
    let n = s
        .writer
        .call({
            let current = current.clone();
            move |c| supersede(c, &current, T0 + 1)
        })
        .unwrap();
    assert_eq!(n, 2); // running の gen1 と pending の gen1b。succeeded の gen0 は結果なので残る
    assert_eq!(s.row(done.id).state, JobState::Succeeded);
    assert_eq!(s.row(running.id).state, JobState::Obsolete);
    assert!(!s.row(running.id).has_token);
    // 現世代と、別の対象は変えない
    assert_eq!(
        s.sql::<i64>("SELECT COUNT(*) FROM ai_jobs WHERE state='pending'"),
        1
    );

    // 無効になった実行中ジョブの遅い応答は保存されない
    let late = s
        .writer
        .call({
            let running = running.clone();
            move |c| {
                complete(c, &running, T0 + 2, |c| {
                    c.execute("INSERT INTO ai_test_results VALUES (1,1)", [])?;
                    Ok(InputCheck::Current)
                })
            }
        })
        .unwrap();
    assert_eq!(late, Completion::Rejected);
    assert_eq!(s.results(), 0);

    // 入力が戻ったら（同じキーの enqueue）obsolete から pending に戻る
    assert_eq!(s.enqueue(&old, T0 + 10), Enqueued::Revived);
    assert_eq!(s.row(running.id).state, JobState::Pending);
    assert_eq!(s.row(running.id).attempts, 0);
}

// ---------------------------------------------------------------- 入力の往復と失敗の履歴

impl Setup {
    fn supersede(&self, keep: &JobKey, now: i64) -> usize {
        let keep = keep.clone();
        self.writer.call(move |c| supersede(c, &keep, now)).unwrap()
    }

    /// 入力が `to` に変わった（`from` の世代は無効）ことを、本番と同じ順（supersede→enqueue）で表す。
    fn switch_input(&self, to: &JobKey, now: i64) -> Enqueued {
        self.supersede(to, now);
        self.enqueue(to, now)
    }

    /// 何度 claim しても取れる全てのジョブの対象（A の再実行を確かめるための総当たり）。
    fn drain_targets(&self, now: i64) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut n = 0;
        while let Some(c) = self.claim(now, "drain", &format!("drain-{n}")) {
            out.push((c.key.target_key.clone(), c.key.input_hash.clone()));
            n += 1;
        }
        out
    }

    fn terminal(&self, id: i64) -> bool {
        self.writer
            .call(move |c| {
                c.query_row("SELECT terminal FROM ai_jobs WHERE id=?1", [id], |r| {
                    r.get(0)
                })
            })
            .unwrap()
    }
}

/// 通常の失敗 5 回・恒久失敗・反復失敗・中断 3 回のどれで `failed` になっても、入力を A→B→A と往復して
/// 同じキーを `enqueue` し直しても、カウンタも `failed` も戻らず、claim もできない。
#[test]
fn a_failed_key_stays_failed_across_input_round_trips() {
    type Drive = fn(&Setup, &JobKey) -> i64;
    // 戻り値: failed になったジョブの id
    fn by_attempts(s: &Setup, k: &JobKey) -> i64 {
        s.enqueue(k, T0);
        let mut now = T0;
        loop {
            let c = s.claim(now, "run", &format!("a{now}")).unwrap();
            if s.fail(&c, now, Failure::Attempt) == Some(JobState::Failed) {
                return c.id;
            }
            now = s.row(c.id).next;
        }
    }
    fn by_permanent(s: &Setup, k: &JobKey) -> i64 {
        s.enqueue(k, T0);
        let c = s.claim(T0, "run", "p").unwrap();
        assert_eq!(s.fail(&c, T0, Failure::Permanent), Some(JobState::Failed));
        c.id
    }
    fn by_repeated(s: &Setup, k: &JobKey) -> i64 {
        s.enqueue(k, T0);
        let mut now = T0;
        loop {
            let c = s.claim(now, "run", &format!("r{now}")).unwrap();
            if s.fail(&c, now, Failure::Repeated) == Some(JobState::Failed) {
                return c.id;
            }
            now = s.row(c.id).next;
        }
    }
    fn by_interruptions(s: &Setup, k: &JobKey) -> i64 {
        s.enqueue(k, T0);
        let mut id = 0;
        for n in 1..=3u32 {
            let c = s
                .claim(
                    T0 + 100 * i64::from(n),
                    &format!("run-{n}"),
                    &format!("i{n}"),
                )
                .unwrap();
            id = c.id;
            let next = format!("run-{}", n + 1);
            s.writer
                .call(move |conn| reclaim_interrupted(conn, &next, T0 + 100 * i64::from(n) + 50))
                .unwrap();
        }
        assert_eq!(s.row(id).state, JobState::Failed);
        id
    }
    let modes: [(&str, Drive); 4] = [
        ("attempts", by_attempts),
        ("permanent", by_permanent),
        ("repeated", by_repeated),
        ("interruptions", by_interruptions),
    ];
    for (name, drive) in modes {
        let s = setup(&format!("ai-roundtrip-{name}"));
        let a = embed("photo", "A");
        let b = embed("photo", "B");
        let id = drive(&s, &a);
        let failed = s.row(id);
        assert_eq!(failed.state, JobState::Failed, "{name}");
        assert!(s.terminal(id), "{name}");

        for round in 0..3 {
            let now = T0 + 10_000 + round * 10;
            // A→B（A は failed → obsolete。B は新しく pending）
            let to_b = s.switch_input(&b, now);
            assert_eq!(
                to_b,
                if round == 0 {
                    Enqueued::Created
                } else {
                    Enqueued::Revived // 前の往復で B は obsolete になっている
                }
            );
            assert_eq!(s.row(id).state, JobState::Obsolete, "{name}");
            // B→A: failed の履歴は消えず、failed に戻る（Revived でも Created でもない）
            assert_eq!(
                s.switch_input(&a, now + 1),
                Enqueued::Existing(JobState::Failed),
                "{name}"
            );
            let back = s.row(id);
            assert_eq!(back.state, JobState::Failed, "{name}");
            assert_eq!(
                (
                    back.attempts,
                    back.defer_count,
                    back.interruptions,
                    back.next,
                    back.code.as_str()
                ),
                (
                    failed.attempts,
                    failed.defer_count,
                    failed.interruptions,
                    failed.next,
                    failed.code.as_str()
                ),
                "{name}: カウンタ・理由は往復で変わらない"
            );
            assert!(!back.has_token);
        }
        // 実際に取れない: 十分先の時刻まで総当たりしても A は一度も claim されない（B だけ）
        let ran = s.drain_targets(T0 + 10_000_000);
        assert!(ran.iter().all(|(_, input)| input != "A"), "{name}: {ran:?}");
        assert_eq!(s.sql::<i64>("SELECT COUNT(*) FROM ai_jobs"), 2, "{name}");

        // 明示の手動再試行だけがカウンタを戻して再び走らせる
        let r = s
            .writer
            .call(move |c| {
                retry_failed(c, T0 + 20_000_000, RetryScope::Id(id), |_, _| {
                    Ok(Recheck::Requeue)
                })
            })
            .unwrap();
        assert_eq!(r.requeued, 1, "{name}");
        let requeued = s.row(id);
        assert_eq!(
            (
                requeued.state,
                requeued.attempts,
                requeued.defer_count,
                requeued.interruptions
            ),
            (JobState::Pending, 0, 0, 0),
            "{name}"
        );
        assert!(!s.terminal(id), "{name}");
        let c = s.claim(T0 + 20_000_000, "run-x", "tx").unwrap();
        assert_eq!((c.id, c.attempts), (id, 0), "{name}");
    }
}

/// 手動の再検査が `Obsolete` と判断した `failed` も、入力が戻って `enqueue` されただけでは再び走らない。
#[test]
fn retry_obsolete_does_not_release_a_failed_key_on_enqueue() {
    let s = setup("ai-retry-obsolete");
    let a = embed("photo", "A");
    s.enqueue(&a, T0);
    let c = s.claim(T0, "run", "t").unwrap();
    s.fail(&c, T0, Failure::Permanent);
    let r = s
        .writer
        .call(|conn| retry_failed(conn, T0 + 1, RetryScope::All, |_, _| Ok(Recheck::Obsolete)))
        .unwrap();
    assert_eq!(r.obsolete, 1);
    assert_eq!(s.row(c.id).state, JobState::Obsolete);

    assert_eq!(s.enqueue(&a, T0 + 2), Enqueued::Existing(JobState::Failed));
    assert_eq!(s.row(c.id).state, JobState::Failed);
    assert!(s.drain_targets(T0 + 10_000_000).is_empty());
    // 失敗として手動の再試行の対象に戻っている
    let r = s
        .writer
        .call(|conn| retry_failed(conn, T0 + 3, RetryScope::All, |_, _| Ok(Recheck::Requeue)))
        .unwrap();
    assert_eq!(r.requeued, 1);
}

/// 成功済みのキーは入力が往復しても再送しない（結果は変わらず、新しい行も増えない）。
#[test]
fn a_succeeded_key_is_not_resent_after_input_round_trips() {
    let s = setup("ai-roundtrip-success");
    let (a, b) = (embed("photo", "A"), embed("photo", "B"));
    s.enqueue(&a, T0);
    let c = s.claim(T0, "run", "t").unwrap();
    let done = s
        .writer
        .call({
            let c = c.clone();
            move |conn| complete(conn, &c, T0, |_| Ok(InputCheck::Current))
        })
        .unwrap();
    assert_eq!(done, Completion::Succeeded);
    let before = s.row(c.id);
    s.switch_input(&b, T0 + 1);
    assert_eq!(
        s.switch_input(&a, T0 + 2),
        Enqueued::Existing(JobState::Succeeded)
    );
    assert_eq!(s.row(c.id), before);
    // 戻った A が現世代なので B は無効。成功済みの A は走らず、何も取れない
    assert!(s.drain_targets(T0 + 1_000_000).is_empty());
}

/// 失敗に至っていない途中のカウンタ・待ち時刻は、入力の往復で消えず、早まらない。
#[test]
fn partial_counters_and_backoff_survive_input_round_trips() {
    let s = setup("ai-roundtrip-partial");
    let (a, b) = (embed("photo", "A"), embed("photo", "B"));
    s.enqueue(&a, T0);
    // 通常の失敗 2 回（attempts=2・120 秒のバックオフ）
    let mut now = T0;
    let mut id = 0;
    for n in 1..=2 {
        let c = s.claim(now, "run", &format!("a{n}")).unwrap();
        id = c.id;
        s.fail(&c, now, Failure::Attempt);
        now = s.row(id).next;
    }
    // さらに 1 回、反復失敗（defer_count=1）と、1 回の中断
    let c = s.claim(now, "run", "d").unwrap();
    s.fail(&c, now, Failure::Repeated);
    now = s.row(id).next;
    let c = s.claim(now, "run-old", "i").unwrap();
    assert_eq!(c.id, id);
    s.writer
        .call(move |conn| reclaim_interrupted(conn, "run-new", now + 5))
        .unwrap();
    let before = s.row(id);
    assert_eq!(
        (
            before.state,
            before.attempts,
            before.defer_count,
            before.interruptions
        ),
        (JobState::Pending, 2, 1, 1)
    );
    let wait_until = now + 5;
    assert_eq!(before.next, wait_until);

    // 往復（待ち時刻より前のうちに）
    s.switch_input(&b, now + 6);
    assert_eq!(s.row(id).state, JobState::Obsolete);
    assert_eq!(s.switch_input(&a, now + 7), Enqueued::Revived);
    let after = s.row(id);
    assert_eq!(after.state, JobState::Pending);
    assert_eq!(
        (
            after.attempts,
            after.defer_count,
            after.interruptions,
            after.next,
            after.code.as_str()
        ),
        (2, 1, 1, before.next, before.code.as_str())
    );
    assert!(!s.terminal(id));

    // 待ち時刻までは取れない。残りの失敗で上限に達する（往復で 5 回分を取り戻していない）
    assert!(s.claim(before.next - 1, "run-new", "early").is_none());
    let mut now = before.next;
    for expected in [JobState::Pending, JobState::Pending, JobState::Failed] {
        let c = s.claim(now, "run-new", &format!("z{now}")).unwrap();
        assert_eq!(c.id, id);
        assert_eq!(s.fail(&c, now, Failure::Attempt), Some(expected));
        now = s.row(id).next;
    }
    assert_eq!(s.row(id).attempts, 5);
}

/// 中断の上限も往復で戻らない: 2 回中断された A が往復しても、3 回目の中断で `failed`。
#[test]
fn the_interruption_limit_is_not_reset_by_input_round_trips() {
    let s = setup("ai-roundtrip-interrupt");
    let (a, b) = (embed("photo", "A"), embed("photo", "B"));
    s.enqueue(&a, T0);
    let mut id = 0;
    for n in 1..=2u32 {
        let c = s.claim(T0, &format!("run-{n}"), &format!("t{n}")).unwrap();
        id = c.id;
        let next = format!("run-{}", n + 1);
        s.writer
            .call(move |conn| reclaim_interrupted(conn, &next, T0))
            .unwrap();
    }
    s.switch_input(&b, T0 + 1);
    assert_eq!(s.switch_input(&a, T0 + 2), Enqueued::Revived);
    assert_eq!(s.row(id).interruptions, 2);
    // B も同じく取られうるので、A を指定して取る
    let mut taken = None;
    while let Some(c) = s.claim(T0 + 10, "run-3", &format!("t3-{}", taken.is_some())) {
        if c.id == id {
            taken = Some(c);
            break;
        }
    }
    assert!(taken.is_some());
    let r = s
        .writer
        .call(|conn| reclaim_interrupted(conn, "run-4", T0 + 20))
        .unwrap();
    assert_eq!(r.failed, 1);
    assert_eq!(s.row(id).state, JobState::Failed);
}

/// 旧い `ai_jobs`（`terminal` 列が無い）に列を足し、失敗の履歴を可能な範囲で復元する。冪等で、
/// 他の表・行は変えない。
#[test]
fn migrate_adds_the_terminal_column_to_an_older_ai_jobs_table() {
    let dir = TempDir::new("ai-migrate-old-table");
    let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
    schema::migrate(&mut conn).unwrap();
    conn.execute_batch(
        "CREATE TABLE ai_jobs(
           id INTEGER PRIMARY KEY,
           kind TEXT NOT NULL CHECK(kind IN
             ('image_embedding','location_clue','location_suggest','location_geocode','photo_judge')),
           target_key TEXT NOT NULL, input_hash TEXT NOT NULL, processor_version TEXT NOT NULL,
           state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN
             ('pending','running','deferred','succeeded','failed','obsolete')),
           attempts INTEGER NOT NULL DEFAULT 0, defer_count INTEGER NOT NULL DEFAULT 0,
           interruption_count INTEGER NOT NULL DEFAULT 0, next_attempt_at INTEGER NOT NULL DEFAULT 0,
           run_id TEXT, claim_token TEXT, last_error_code TEXT NOT NULL DEFAULT '',
           created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
           UNIQUE(kind, target_key, input_hash, processor_version));
         CREATE INDEX idx_ai_jobs_runnable ON ai_jobs(state, next_attempt_at, id);
         INSERT INTO ai_jobs(id,kind,target_key,input_hash,processor_version,state,attempts,
                             interruption_count,last_error_code,created_at,updated_at) VALUES
           (1,'image_embedding','p','failed','v','failed',5,0,'timeout',1,1),
           (2,'image_embedding','p','obs-attempts','v','obsolete',5,0,'timeout',1,1),
           (3,'image_embedding','p','obs-interrupted','v','obsolete',0,3,'interrupted',1,1),
           (4,'image_embedding','p','obs-permanent','v','obsolete',0,0,'input_invalid',1,1),
           (5,'image_embedding','p','obs-plain','v','obsolete',2,0,'timeout',1,1),
           (6,'image_embedding','p','pending','v','pending',1,0,'timeout',1,1),
           (7,'image_embedding','p','done','v','succeeded',0,0,'',1,1);",
    )
    .unwrap();
    let photos_before = dump(&conn);
    assert_eq!(pending(&conn).unwrap(), ["ai_jobs.terminal"]);
    assert_eq!(migrate(&mut conn).unwrap(), ["ai_jobs.terminal"]);
    assert!(pending(&conn).unwrap().is_empty());
    assert_eq!(dump(&conn), photos_before);

    let terminals: Vec<i64> = conn
        .prepare("SELECT terminal FROM ai_jobs ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(terminals, [1, 1, 1, 1, 0, 0, 0]);
    let counters: i64 = conn
        .query_row(
            "SELECT SUM(attempts)+SUM(interruption_count) FROM ai_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(counters, 5 + 5 + 3 + 2 + 1);

    // 冪等: 2 回目は何も書かない
    let changes = conn.total_changes();
    assert!(migrate(&mut conn).unwrap().is_empty());
    assert_eq!(conn.total_changes(), changes);

    // 復元した履歴は効く: 旧い obsolete の失敗キーを enqueue しても failed に戻り、plain は pending
    assert_eq!(
        enqueue(
            &mut conn,
            &JobKey::new(JobKind::ImageEmbedding, "p", "obs-attempts", "v"),
            T0
        )
        .unwrap(),
        Enqueued::Existing(JobState::Failed)
    );
    assert_eq!(
        enqueue(
            &mut conn,
            &JobKey::new(JobKind::ImageEmbedding, "p", "obs-plain", "v"),
            T0
        )
        .unwrap(),
        Enqueued::Revived
    );
}

// ---------------------------------------------------------------- claim

#[test]
fn claim_takes_one_runnable_job_in_order_with_a_token() {
    let s = setup("ai-claim");
    s.enqueue(&embed("a", "1"), T0 + 10); // まだ先
    s.enqueue(&embed("b", "1"), T0);
    s.enqueue(&key(JobKind::PhotoJudge, "c", "1"), T0);
    // 種類の絞り込み・空
    let none = s
        .writer
        .call(|c| claim_next(c, T0, "run", "x", &[]))
        .unwrap();
    assert!(none.is_none());
    let judge = s
        .writer
        .call(|c| claim_next(c, T0, "run", "j", &[JobKind::PhotoJudge]))
        .unwrap()
        .unwrap();
    assert_eq!(judge.key.kind, JobKind::PhotoJudge);
    let first = s.claim(T0, "run", "t1").unwrap();
    assert_eq!(first.key.target_key, "b");
    // 先の時刻のものと実行中のものは取れない
    assert!(s.claim(T0, "run", "t2").is_none());
    let late = s.claim(T0 + 10, "run", "t3").unwrap();
    assert_eq!(late.key.target_key, "a");
    assert_eq!(s.row(first.id).state, JobState::Running);
    assert!(s.row(first.id).has_token);
    assert_eq!(
        s.sql::<i64>("SELECT COUNT(*) FROM ai_jobs WHERE state='running'"),
        3
    );
}

#[test]
fn ids_are_random_and_debug_hides_the_internal_values() {
    let a = random_id().unwrap();
    let b = random_id().unwrap();
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
    assert_ne!(a, "0".repeat(32));

    let s = setup("ai-debug");
    let k = embed("secret-target", "secret-input");
    s.enqueue(&k, T0);
    let c = s.claim(T0, "run-secret", "token-secret").unwrap();
    for text in [format!("{k:?}"), format!("{c:?}")] {
        for hidden in [
            "secret-target",
            "secret-input",
            "token-secret",
            "run-secret",
            "proc-1",
        ] {
            assert!(!text.contains(hidden), "{text}");
        }
    }
}

// ---------------------------------------------------------------- 結果の確定

/// 合成の写真 1 枚。結果の確定時に今の写真の世代（md5）を見る。
fn add_photo(s: &Setup, md5: &'static str) -> i64 {
    s.writer
        .call(move |c| {
            c.execute(
                "INSERT INTO photos(source,drive_id,name,md5) VALUES ('drive','d','n',?1)",
                [md5],
            )?;
            Ok(c.last_insert_rowid())
        })
        .unwrap()
}

/// 現在の写真の世代が `expected` のときだけ結果を保存する、後続の handler と同じ形の確定。
fn complete_for_photo(
    s: &Setup,
    claimed: &Claimed,
    photo_id: i64,
    expected_md5: &'static str,
) -> Completion {
    let claimed = claimed.clone();
    s.writer
        .call(move |c| {
            complete(c, &claimed, T0 + 1, |conn| {
                // 書いてから検査しても、Changed なら savepoint ごと捨てられる
                conn.execute(
                    "INSERT INTO ai_test_results VALUES (?1,?2)",
                    params![claimed.id, photo_id],
                )?;
                let md5: Option<String> = conn
                    .query_row("SELECT md5 FROM photos WHERE id=?1", [photo_id], |r| {
                        r.get(0)
                    })
                    .ok()
                    .flatten();
                Ok(if md5.as_deref() == Some(expected_md5) {
                    InputCheck::Current
                } else {
                    InputCheck::Changed
                })
            })
        })
        .unwrap()
}

#[test]
fn a_current_result_and_succeeded_are_committed_together() {
    let s = setup("ai-complete-ok");
    let photo = add_photo(&s, "m1");
    s.enqueue(&embed("p", "m1"), T0);
    let c = s.claim(T0, "run", "tok").unwrap();
    assert_eq!(
        complete_for_photo(&s, &c, photo, "m1"),
        Completion::Succeeded
    );
    assert_eq!(s.results(), 1);
    let row = s.row(c.id);
    assert_eq!(row.state, JobState::Succeeded);
    assert!(!row.has_token);
    // 確定済みに対する 2 回目（重複した応答）は保存しない
    assert_eq!(
        complete_for_photo(&s, &c, photo, "m1"),
        Completion::Rejected
    );
    assert_eq!(s.results(), 1);
}

#[test]
fn a_stale_source_saves_nothing_and_obsoletes_the_job() {
    let s = setup("ai-complete-stale");
    let photo = add_photo(&s, "m1");
    // job 行の input_hash は古いまま（m1）。現在の photos が m2 に変わった
    s.enqueue(&embed("p", "m1"), T0);
    let c = s.claim(T0, "run", "tok").unwrap();
    s.writer
        .call(move |conn| conn.execute("UPDATE photos SET md5='m2' WHERE id=?1", [photo]))
        .unwrap();
    assert_eq!(
        complete_for_photo(&s, &c, photo, "m1"),
        Completion::Obsolete
    );
    assert_eq!(s.results(), 0, "callback が書いた行も残らない");
    assert_eq!(s.row(c.id).state, JobState::Obsolete);
    assert!(!s.row(c.id).has_token);
    // 削除された写真も同じ
    s.enqueue(&embed("p", "m2"), T0);
    let c2 = s.claim(T0, "run", "tok2").unwrap();
    s.writer
        .call(move |conn| conn.execute("DELETE FROM photos WHERE id=?1", [photo]))
        .unwrap();
    assert_eq!(
        complete_for_photo(&s, &c2, photo, "m2"),
        Completion::Obsolete
    );
    assert_eq!(s.results(), 0);
}

#[test]
fn a_failing_save_leaves_neither_rows_nor_a_lone_success() {
    let s = setup("ai-complete-err");
    s.enqueue(&embed("p", "1"), T0);
    let c = s.claim(T0, "run", "tok").unwrap();

    // 保存が書いたあとにエラー
    let err = s.writer.call({
        let c = c.clone();
        move |conn| {
            complete(conn, &c, T0, |conn| {
                conn.execute("INSERT INTO ai_test_results VALUES (1,1)", [])?;
                conn.execute("INSERT INTO no_such_table VALUES (1)", [])?;
                Ok(InputCheck::Current)
            })
        }
    });
    assert!(matches!(err, Err(WriteError::Query(_))));
    assert_eq!(s.results(), 0);
    assert_eq!(s.row(c.id).state, JobState::Running);

    // 保存がパニック（writer が受け止めて後始末する）
    let panicked = s.writer.call({
        let c = c.clone();
        move |conn| {
            complete(conn, &c, T0, |conn| {
                conn.execute("INSERT INTO ai_test_results VALUES (1,1)", [])?;
                panic!("boom");
            })
        }
    });
    assert!(matches!(panicked, Err(WriteError::Task(_))));
    assert_eq!(s.results(), 0);
    assert_eq!(s.row(c.id).state, JobState::Running);

    // 呼び出し側は失敗を記録できる（token は生きている）
    assert_eq!(s.fail(&c, T0, Failure::Attempt), Some(JobState::Pending));
    assert_eq!(s.results(), 0);
}

#[test]
fn a_wrong_or_revoked_claim_is_rejected_without_running_the_callback() {
    let s = setup("ai-token");
    s.enqueue(&embed("p", "1"), T0);
    let c = s.claim(T0, "run-a", "tok-a").unwrap();

    // token・run_id のどちらが違っても拒否（偽の claim を作るには別の claim を借りる）
    s.enqueue(&embed("q", "1"), T0);
    let other = s.claim(T0, "run-a", "tok-b").unwrap();
    let forged = Claimed {
        id: c.id,
        ..other.clone()
    };
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ran.clone();
    let rejected = s
        .writer
        .call(move |conn| {
            complete(conn, &forged, T0, |_| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(InputCheck::Current)
            })
        })
        .unwrap();
    assert_eq!(rejected, Completion::Rejected);
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(s.row(c.id).state, JobState::Running);

    // 再起動: 別の起動が回収し、同じジョブを新しい token で取り直す。旧い応答は保存されない
    let r = s
        .writer
        .call(|conn| reclaim_interrupted(conn, "run-b", T0 + 1))
        .unwrap();
    assert_eq!(r.requeued, 2);
    let again = s.claim(T0 + 1, "run-b", "tok-c").unwrap();
    assert_eq!(again.id, c.id);
    assert_eq!(complete_for_photo(&s, &c, 1, "m"), Completion::Rejected);
    assert_eq!(s.fail(&c, T0 + 2, Failure::Permanent), None);
    assert_eq!(s.row(c.id).state, JobState::Running);
    // 新しい claim は確定できる
    assert_eq!(
        s.writer
            .call(move |conn| complete(conn, &again, T0 + 3, |_| Ok(InputCheck::Current)))
            .unwrap(),
        Completion::Succeeded
    );
}

#[test]
fn a_job_found_stale_before_sending_is_obsoleted_only_by_its_owner() {
    let s = setup("ai-mark-obsolete");
    s.enqueue(&embed("p", "1"), T0);
    let c = s.claim(T0, "run", "tok").unwrap();
    let stranger = Claimed {
        id: c.id,
        ..{
            s.enqueue(&embed("q", "1"), T0);
            s.claim(T0, "run", "tok2").unwrap()
        }
    };
    assert!(
        !s.writer
            .call(move |conn| mark_obsolete(conn, &stranger, T0))
            .unwrap()
    );
    assert!(
        s.writer
            .call({
                let c = c.clone();
                move |conn| mark_obsolete(conn, &c, T0)
            })
            .unwrap()
    );
    assert_eq!(s.row(c.id).state, JobState::Obsolete);
}

// ---------------------------------------------------------------- 失敗・後回し

#[test]
fn defer_does_not_count_as_a_failure_and_clamps_the_wait() {
    let s = setup("ai-defer");
    s.enqueue(&embed("p", "1"), T0);
    for (retry_after, wait) in [
        (None, 60),
        (Some(5), 60),
        (Some(120), 120),
        (Some(999_999), 3600),
    ] {
        let now = T0 + 10_000;
        let c = s.claim(now, "run", "tok").unwrap();
        let state = s.fail(
            &c,
            now,
            Failure::Defer {
                retry_after_secs: retry_after,
            },
        );
        assert_eq!(state, Some(JobState::Deferred));
        let row = s.row(c.id);
        assert_eq!((row.attempts, row.defer_count), (0, 0));
        assert_eq!(row.next, now + wait);
        assert_eq!(row.code, "timeout");
        assert!(!row.has_token);
        // 待ち時間のあいだは取れず、過ぎたら取れる
        assert!(s.claim(now + wait - 1, "run", "t").is_none());
        s.writer
            .call(move |conn| conn.execute("UPDATE ai_jobs SET next_attempt_at=?1", [now]))
            .unwrap();
    }
}

#[test]
fn ordinary_failures_back_off_and_stop_at_five() {
    let s = setup("ai-attempts");
    s.enqueue(&embed("p", "1"), T0);
    let mut now = T0;
    let mut waits = Vec::new();
    for n in 1..=5u32 {
        let c = s.claim(now, "run", &format!("t{n}")).unwrap();
        assert_eq!(c.attempts, n - 1);
        let state = s.fail(&c, now, Failure::Attempt).unwrap();
        let row = s.row(c.id);
        assert_eq!(row.attempts, n);
        if n < 5 {
            assert_eq!(state, JobState::Pending);
            waits.push(row.next - now);
            // バックオフの間は取れない
            assert!(s.claim(row.next - 1, "run", "x").is_none());
            now = row.next;
        } else {
            assert_eq!(state, JobState::Failed);
        }
    }
    assert_eq!(waits, [60, 120, 240, 480]);
    // failed は二度と取れない
    assert!(s.claim(now + 1_000_000, "run", "x").is_none());
    assert_eq!(backoff_secs(5), 480);
}

#[test]
fn repeated_failures_of_one_job_start_counting_at_the_third() {
    let s = setup("ai-repeated");
    s.enqueue(&embed("p", "1"), T0);
    let mut now = T0;
    // 1・2 回目: defer_count だけ。attempts は増えない
    // 3 回目以降: attempts にも数え、5 回で failed（= 全体で 7 回目）
    let mut expected = Vec::new();
    for n in 1..=7u32 {
        let c = s.claim(now, "run", &format!("t{n}")).unwrap();
        let state = s.fail(&c, now, Failure::Repeated).unwrap();
        let row = s.row(c.id);
        expected.push((n, state, row.defer_count, row.attempts));
        if state == JobState::Failed {
            break;
        }
        now = row.next;
    }
    use JobState::{Deferred, Failed};
    assert_eq!(
        expected,
        [
            (1, Deferred, 1, 0),
            (2, Deferred, 2, 0),
            (3, Deferred, 3, 1),
            (4, Deferred, 4, 2),
            (5, Deferred, 5, 3),
            (6, Deferred, 6, 4),
            (7, Failed, 7, 5),
        ]
    );
}

#[test]
fn a_permanent_failure_fails_only_that_job() {
    let s = setup("ai-permanent");
    s.enqueue(&embed("bad", "1"), T0);
    s.enqueue(&embed("good", "1"), T0);
    let bad = s.claim(T0, "run", "t1").unwrap();
    assert_eq!(s.fail(&bad, T0, Failure::Permanent), Some(JobState::Failed));
    assert_eq!(s.row(bad.id).attempts, 0);
    let good = s.claim(T0, "run", "t2").unwrap();
    assert_eq!(good.key.target_key, "good");
}

// ---------------------------------------------------------------- 起動回収・停止

#[test]
fn interrupted_jobs_are_requeued_then_isolated_after_three() {
    let s = setup("ai-interrupt");
    s.enqueue(&embed("p", "1"), T0);
    let mut now = T0;
    let mut id = 0;
    for n in 1..=3u32 {
        // 起動 n が取って、実行中に強制終了 → 起動 n+1 が回収する
        let c = s.claim(now, &format!("run-{n}"), &format!("t{n}")).unwrap();
        id = c.id;
        assert_eq!(c.attempts, 0, "中断は失敗に数えない");
        now += 100;
        let next_run = format!("run-{}", n + 1);
        let r = s
            .writer
            .call(move |conn| reclaim_interrupted(conn, &next_run, now))
            .unwrap();
        let row = s.row(id);
        assert_eq!(row.interruptions, n);
        assert_eq!((row.attempts, row.code.as_str()), (0, "interrupted"));
        assert!(!row.has_token);
        if n < 3 {
            assert_eq!(
                r,
                Reclaimed {
                    requeued: 1,
                    failed: 0
                }
            );
            assert_eq!(row.state, JobState::Pending);
        } else {
            assert_eq!(
                r,
                Reclaimed {
                    requeued: 0,
                    failed: 1
                }
            );
            assert_eq!(row.state, JobState::Failed);
        }
    }
    // 隔離されたのはそのジョブだけ。failed は取れず、ほかのジョブは取れる
    assert!(s.claim(now + 1_000_000, "run-4", "t9").is_none());
    s.enqueue(&embed("other", "1"), now);
    assert_ne!(s.claim(now, "run-4", "t10").unwrap().id, id);
}

#[test]
fn reclaim_leaves_the_current_runs_jobs_alone() {
    let s = setup("ai-reclaim-own");
    s.enqueue(&embed("p", "1"), T0);
    let c = s.claim(T0, "run-now", "tok").unwrap();
    let r = s
        .writer
        .call(|conn| reclaim_interrupted(conn, "run-now", T0 + 1))
        .unwrap();
    assert_eq!(r, Reclaimed::default());
    assert_eq!(s.row(c.id).state, JobState::Running);
    assert!(s.row(c.id).has_token);
}

#[test]
fn shutdown_returns_running_jobs_without_counting_and_revokes_the_token() {
    let s = setup("ai-shutdown");
    s.enqueue(&embed("p", "1"), T0);
    let c = s.claim(T0, "run", "tok").unwrap();
    let n = s
        .writer
        .call(|conn| release_for_shutdown(conn, "run", T0 + 5))
        .unwrap();
    assert_eq!(n, 1);
    let row = s.row(c.id);
    assert_eq!(row.state, JobState::Deferred);
    assert_eq!(
        (row.attempts, row.defer_count, row.interruptions),
        (0, 0, 0)
    );
    assert_eq!(row.code, "shutdown");
    assert!(!row.has_token);
    // 停止後に届いた応答は保存されない
    assert_eq!(complete_for_photo(&s, &c, 1, "m"), Completion::Rejected);
    assert_eq!(s.results(), 0);
    // 次の起動で取り直せる
    assert!(s.claim(T0 + 5, "run-next", "t2").is_some());
}

// ---------------------------------------------------------------- 手動の再試行

#[test]
fn manual_retry_touches_only_failed_jobs_after_rechecking() {
    let s = setup("ai-retry");
    // 取る順 = 積んだ順。requeue・stale・satisfied は failed に、done は succeeded に、waiting は pending のまま
    for name in ["requeue", "stale", "satisfied", "done"] {
        s.enqueue(&key(JobKind::ImageEmbedding, name, "1"), T0);
    }
    for n in 0..4 {
        let c = s.claim(T0, "run", &format!("t{n}")).unwrap();
        if c.key.target_key == "done" {
            s.writer
                .call(move |conn| complete(conn, &c, T0, |_| Ok(InputCheck::Current)))
                .unwrap();
        } else {
            s.fail(&c, T0, Failure::Permanent);
        }
    }
    s.enqueue(&key(JobKind::ImageEmbedding, "waiting", "1"), T0);
    s.writer
        .call(|conn| {
            conn.execute(
                "UPDATE ai_jobs SET attempts=5, defer_count=2, interruption_count=1
                 WHERE state='failed'",
                [],
            )
        })
        .unwrap();
    let done_id = s.sql::<i64>("SELECT id FROM ai_jobs WHERE target_key='done'");
    let waiting_id = s.sql::<i64>("SELECT id FROM ai_jobs WHERE target_key='waiting'");
    let (done_before, waiting_before) = (s.row(done_id), s.row(waiting_id));

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let retried = s
        .writer
        .call(move |conn| {
            retry_failed(conn, T0 + 50, RetryScope::All, |_, k| {
                log.lock().unwrap().push(k.target_key.clone());
                Ok(match k.target_key.as_str() {
                    "requeue" => Recheck::Requeue,
                    "stale" => Recheck::Obsolete,
                    _ => Recheck::Satisfied,
                })
            })
        })
        .unwrap();
    assert_eq!(
        retried,
        Retried {
            requeued: 1,
            obsolete: 1,
            satisfied: 1
        }
    );
    // 検査したのは failed だけ。成功済み・待機中は対象外で、そのまま
    assert_eq!(*seen.lock().unwrap(), ["requeue", "stale", "satisfied"]);
    assert_eq!(s.row(done_id), done_before);
    assert_eq!(s.row(waiting_id), waiting_before);

    let requeued = s.sql::<i64>("SELECT id FROM ai_jobs WHERE target_key='requeue'");
    let row = s.row(requeued);
    assert_eq!(row.state, JobState::Pending);
    assert_eq!(
        (row.attempts, row.defer_count, row.interruptions),
        (0, 0, 0)
    );
    assert_eq!(row.next, T0 + 50);
    assert_eq!(row.code, "");
    assert_eq!(
        s.sql::<String>("SELECT state FROM ai_jobs WHERE target_key='stale'"),
        "obsolete"
    );
    assert_eq!(
        s.sql::<String>("SELECT state FROM ai_jobs WHERE target_key='satisfied'"),
        "succeeded"
    );
}

#[test]
fn manual_retry_scopes_by_kind_and_id() {
    let s = setup("ai-retry-scope");
    for (kind, name) in [
        (JobKind::ImageEmbedding, "a"),
        (JobKind::PhotoJudge, "b"),
        (JobKind::PhotoJudge, "c"),
    ] {
        s.enqueue(&key(kind, name, "1"), T0);
        let c = s.claim(T0, "run", name).unwrap();
        s.fail(&c, T0, Failure::Permanent);
    }
    let r = s
        .writer
        .call(|conn| {
            retry_failed(conn, T0, RetryScope::Kind(JobKind::PhotoJudge), |_, _| {
                Ok(Recheck::Requeue)
            })
        })
        .unwrap();
    assert_eq!(r.requeued, 2);
    assert_eq!(
        s.sql::<i64>("SELECT COUNT(*) FROM ai_jobs WHERE state='failed'"),
        1
    );
    let id = s.sql::<i64>("SELECT id FROM ai_jobs WHERE state='failed'");
    let r = s
        .writer
        .call(move |conn| retry_failed(conn, T0, RetryScope::Id(id), |_, _| Ok(Recheck::Requeue)))
        .unwrap();
    assert_eq!(r.requeued, 1);
    // 検査が失敗したら何も変えない（全体がロールバックされる）
    let c = s.claim(T0, "run", "z").unwrap();
    s.fail(&c, T0, Failure::Permanent);
    let err = s.writer.call(|conn| {
        retry_failed(conn, T0, RetryScope::All, |_, _| {
            Err(rusqlite::Error::InvalidQuery)
        })
    });
    assert!(err.is_err());
    assert_eq!(s.row(c.id).state, JobState::Failed);
}

// ---------------------------------------------------------------- 集計・保存される値

#[test]
fn counts_by_kind_and_state_and_only_fixed_codes_are_stored() {
    let s = setup("ai-counts");
    s.enqueue(&embed("a", "1"), T0);
    s.enqueue(&embed("b", "1"), T0);
    s.enqueue(&key(JobKind::PhotoJudge, "c", "1"), T0);
    let c = s.claim(T0, "run", "t").unwrap();
    s.fail(&c, T0, Failure::Attempt);
    let counts = s.writer.call(|conn| status_counts(conn)).unwrap();
    assert_eq!(
        counts,
        [
            (JobKind::ImageEmbedding, JobState::Pending, 2),
            (JobKind::PhotoJudge, JobState::Pending, 1),
        ]
    );
    // 保存される失敗の理由は固定の語だけ（対象・入力・例外本文は入らない）
    for code in [
        ErrorCode::Unavailable,
        ErrorCode::RateLimited,
        ErrorCode::Timeout,
        ErrorCode::ServerError,
        ErrorCode::BadResponse,
        ErrorCode::InputTooLarge,
        ErrorCode::InputInvalid,
        ErrorCode::PolicyDisabled,
        ErrorCode::Shutdown,
        ErrorCode::Interrupted,
        ErrorCode::Panic,
        ErrorCode::Internal,
    ] {
        assert!(
            code.as_str()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        );
    }
    assert_eq!(s.row(c.id).code, "timeout");
    // 読み取り専用の接続（STATUS）からも読める
    let ro = crate::db::open_read_only(&s.path).unwrap();
    assert_eq!(status_counts(&ro).unwrap().len(), 2);
}

/// 起動 → 実行中に強制終了 → 次の起動で回収・取り直し → 確定、までを実際の DB ファイルの開き直しで通す。
#[test]
fn a_job_survives_a_restart_and_is_finished_exactly_once() {
    let dir = TempDir::new("ai-restart");
    let path = dir.path().join("catalog.db");
    let k = embed("p", "1");
    {
        let w = Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
        w.call(schema::migrate).unwrap();
        w.call(migrate).unwrap();
        w.call({
            let k = k.clone();
            move |c| enqueue(c, &k, T0)
        })
        .unwrap();
        let first = w
            .call(|c| claim_next(c, T0, "run-1", "tok-1", &JobKind::ALL))
            .unwrap();
        assert!(first.is_some());
        // 書き込み接続を落とす（= 強制終了。running のまま残る）
    }
    let w = Writer::open(&path, OpenMode::MustExist).unwrap();
    w.call(migrate).unwrap(); // 起動のたびの migrate は何もしない
    // 再 enqueue しても増えない（running のまま）
    assert_eq!(
        w.call({
            let k = k.clone();
            move |c| enqueue(c, &k, T0 + 1)
        })
        .unwrap(),
        Enqueued::Existing(JobState::Running)
    );
    assert_eq!(
        w.call(|c| reclaim_interrupted(c, "run-2", T0 + 2)).unwrap(),
        Reclaimed {
            requeued: 1,
            failed: 0
        }
    );
    let c = w
        .call(|c| claim_next(c, T0 + 2, "run-2", "tok-2", &JobKind::ALL))
        .unwrap()
        .unwrap();
    assert_eq!(c.key, k);
    assert_eq!(
        w.call({
            let c = c.clone();
            move |conn| complete(conn, &c, T0 + 3, |_| Ok(InputCheck::Current))
        })
        .unwrap(),
        Completion::Succeeded
    );
    assert_eq!(
        w.call(move |conn| complete(conn, &c, T0 + 4, |_| Ok(InputCheck::Current)))
            .unwrap(),
        Completion::Rejected
    );
    let n: i64 = w
        .call(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(n, 1);
}
