//! 管理操作（状態の集計・STATUS の追加キー・手動の再試行・Provider 停止の解除）の試験。合成の DB・fake handler だけ。
//!
//! - 読み取り: AI の表が無い旧 DB でも 0 件／出力に対象・入力・token・モデル・URL・パスを含めない
//! - STATUS の追加キー: あるときだけ足す（AI の表も誤りも停止も無い DB には 1 つも足さない）
//! - 再試行: failed だけを対象に handler の再検査で振り分け／成功済みは戻さない／検査できないものは保持して件数だけ
//! - 停止の解除: 用途指定は今の設定に当たる停止だけ（設定が不正なら何もしない）／`--all` は全部

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Map, Value};

use super::jobs::{self, JobKey, JobKind, JobState, Recheck};
use super::ops::{
    self, ConfigState, PauseTarget, ReleaseError, RetryReport, RetryTarget, TaskPause,
};
use super::pause::{self, PauseReason, PauseScope};
use super::settings::{AiConfig, AiConfigError, TaskKind};
use super::tests_settings::SAMPLE;
use super::tests_worker::{Fake, key};
use super::waits::{self, WaitKey, WaitReason, WaitScope};
use super::worker::{Clock, Handler};
use crate::schema;
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};

const T0: i64 = 1_792_972_800 + 3_600;

struct Db {
    _dir: TempDir,
    path: std::path::PathBuf,
    writer: Writer,
    config: AiConfig,
}

fn ai_config(dir: &TempDir, json: Option<&str>) -> AiConfig {
    let mut vars: HashMap<String, String> = HashMap::new();
    if let Some(json) = json {
        let path = dir.path().join("ai.json");
        std::fs::write(&path, json).unwrap();
        vars.insert("GEOFRAME_AI_CONFIG".into(), path.to_str().unwrap().into());
    }
    AiConfig::load(&|k| vars.get(k).cloned())
}

fn db(tag: &str, json: Option<&str>) -> Db {
    let dir = TempDir::new(tag);
    let path = dir.path().join("catalog.db");
    let writer = Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
    writer.call(schema::migrate).unwrap();
    writer.call(super::migrate).unwrap();
    let config = ai_config(&dir, json);
    Db {
        _dir: dir,
        path,
        writer,
        config,
    }
}

impl Db {
    fn clock(&self) -> Clock {
        Arc::new(|| T0)
    }

    /// `failed` のジョブを作る（terminal・カウンタつき）。
    fn failed(&self, kind: JobKind, target: &str, code: &str) {
        let (k, t, code) = (key(kind, target), target.to_string(), code.to_string());
        self.writer
            .call(move |c| {
                jobs::enqueue(c, &k, T0)?;
                c.execute(
                    "UPDATE ai_jobs SET state='failed', attempts=5, terminal=1, last_error_code=?2
                     WHERE target_key=?1",
                    rusqlite::params![t, code],
                )
            })
            .unwrap();
    }

    fn state(&self, kind: JobKind, target: &str) -> (String, u32, i64) {
        let (k, t) = (kind.as_str(), target.to_string());
        self.writer
            .call(move |c| {
                c.query_row(
                    "SELECT state, attempts, terminal FROM ai_jobs WHERE kind=?1 AND target_key=?2",
                    rusqlite::params![k, t],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
            })
            .unwrap()
    }

    fn read(&self) -> Connection {
        crate::db::open_read_only(&self.path).unwrap()
    }
}

fn handlers(list: &[&Arc<Fake>]) -> Vec<Arc<dyn Handler>> {
    list.iter()
        .map(|h| Arc::clone(*h) as Arc<dyn Handler>)
        .collect()
}

// ---------------------------------------------------------------- 読み取り

#[test]
fn the_report_of_an_old_database_without_ai_tables_is_empty_and_reads_only() {
    let dir = TempDir::new("ops-old");
    let (path, _keep) = crate::testutil::synthetic_catalog(dir.path(), 3);
    let ro = crate::db::open_read_only(&path).unwrap();
    let config = ai_config(&dir, None);
    let report = ops::report(&ro, &config).unwrap();
    assert_eq!(report.config, ConfigState::Unset);
    assert!(report.jobs.is_empty() && report.reasons.is_empty() && report.pauses.is_empty());
    // 用途は全部、理由つきで「使えない」
    assert!(
        report
            .tasks
            .iter()
            .all(|t| t.gate.is_err() && t.pause == TaskPause::None)
    );
    // 追加のキーは 1 つも足さない（Python 版と同じ応答のまま）
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &ro,
        &config,
        &waits::Identities::from_config(&config),
        T0,
    );
    assert!(out.is_empty(), "{out:?}");
    // 表は作られていない
    assert!(!super::tests::table_exists(&ro, "ai_jobs"));
    assert!(!super::tests::table_exists(&ro, "ai_worker_runs"));
}

#[test]
fn the_config_states_are_fixed_codes_and_an_invalid_config_adds_only_a_code() {
    let dir = TempDir::new("ops-config");
    assert_eq!(
        ops::config_state(&ai_config(&dir, Some(SAMPLE))),
        ConfigState::Ready
    );
    let bad = ai_config(&dir, Some("{\"version\":1,\"nope\":1}"));
    assert_eq!(
        ops::config_state(&bad),
        ConfigState::Invalid(AiConfigError::UnknownKey)
    );
    let d = db("ops-config-status", Some("{"));
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0,
    );
    assert_eq!(
        out.get("ai_config_invalid").unwrap(),
        &serde_json::json!({"code": "syntax", "disabled_tasks": 5})
    );
    // ジョブも停止も無いので、ほかのキーは足さない
    assert_eq!(out.len(), 1);
}

#[test]
fn the_admin_keys_carry_counts_and_fixed_reasons_only() {
    let d = db("ops-admin", Some(SAMPLE));
    let secret = |kind| JobKey::new(kind, "SECRET-TARGET", "SECRET-HASH", "SECRET-PROC");
    d.writer
        .call(move |c| {
            for (kind, state, code) in [
                (JobKind::ImageEmbedding, "pending", ""),
                (JobKind::PhotoJudge, "deferred", "clock_rewound"),
                (JobKind::LocationClue, "failed", "interrupted"),
            ] {
                let mut k = secret(kind);
                k.target_key = format!("SECRET-TARGET-{}", kind.as_str());
                jobs::enqueue(c, &k, T0)?;
                c.execute(
                    "UPDATE ai_jobs SET state=?2, last_error_code=?3 WHERE kind=?1",
                    rusqlite::params![kind.as_str(), state, code],
                )?;
            }
            // 壊れた理由の値は other にまとめ、そのまま出さない
            let mut k = secret(JobKind::LocationSuggest);
            k.target_key = "SECRET-X".into();
            jobs::enqueue(c, &k, T0)?;
            c.execute(
                "UPDATE ai_jobs SET state='deferred', last_error_code='https://secret.example/path'
                 WHERE kind='location_suggest'",
                [],
            )?;
            let settings = super::tests_settings::sample();
            let key = settings
                .provider_pause_key(TaskKind::ImageEmbedding)
                .unwrap();
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &key,
                PauseReason::Unauthorized,
                T0,
            )?;
            Ok(())
        })
        .unwrap();
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0,
    );
    let jobs_json = out.get("ai_jobs").unwrap();
    assert_eq!(jobs_json["total"], 4);
    assert_eq!(jobs_json["by_state"]["deferred"], 2);
    assert_eq!(jobs_json["by_kind"]["photo_judge"]["deferred"], 1);
    assert_eq!(jobs_json["reasons"]["deferred"]["clock_rewound"], 1);
    assert_eq!(jobs_json["reasons"]["deferred"]["other"], 1);
    assert_eq!(jobs_json["reasons"]["failed"]["interrupted"], 1);
    assert_eq!(
        out.get("ai_pauses").unwrap(),
        &serde_json::json!([{"task": "image_embedding", "scope": "provider", "reason": "unauthorized"}])
    );
    let text = Value::Object(out).to_string();
    for needle in [
        "SECRET",
        "secret.example",
        "192.0.2.10",
        "example-embed",
        "credentials",
    ] {
        assert!(!text.contains(needle), "{needle}: {text}");
    }
}

// ---------------------------------------------------------------- 手動の再試行

#[test]
fn retry_sorts_only_failed_jobs_through_the_handler_and_never_touches_succeeded_ones() {
    let d = db("ops-retry", Some(SAMPLE));
    let clue = Fake::ok(JobKind::LocationClue);
    clue.verdicts.lock().unwrap().extend([
        ("again".to_string(), Recheck::Requeue),
        ("moved".to_string(), Recheck::Obsolete),
        ("done".to_string(), Recheck::Satisfied),
    ]);
    for (t, code) in [
        ("again", "bad_response"),
        ("moved", "bad_response"),
        ("done", "interrupted"),
    ] {
        d.failed(JobKind::LocationClue, t, code);
    }
    // 成功済み・待機中は対象外
    d.writer
        .call(|c| {
            jobs::enqueue(c, &key(JobKind::LocationClue, "ok"), T0)?;
            c.execute(
                "UPDATE ai_jobs SET state='succeeded' WHERE target_key='ok'",
                [],
            )?;
            jobs::enqueue(c, &key(JobKind::LocationClue, "waiting"), T0)
        })
        .unwrap();

    let report = ops::retry(
        &d.writer,
        &d.config,
        &handlers(&[&clue]),
        RetryTarget::Kind(JobKind::LocationClue),
        &d.clock(),
    )
    .unwrap();
    assert_eq!(
        report,
        RetryReport {
            requeued: 1,
            obsolete: 1,
            satisfied: 1,
            unsupported: 0,
            policy_disabled: 0
        }
    );
    // 再実行待ちは回数・履歴をリセット。無効・成功済みは結果を再計算しない
    assert_eq!(
        d.state(JobKind::LocationClue, "again"),
        ("pending".into(), 0, 0)
    );
    assert_eq!(d.state(JobKind::LocationClue, "moved").0, "obsolete");
    assert_eq!(d.state(JobKind::LocationClue, "done").0, "succeeded");
    assert_eq!(d.state(JobKind::LocationClue, "ok").0, "succeeded");
    assert_eq!(d.state(JobKind::LocationClue, "waiting").0, "pending");
    // 2 回目は対象が無い
    let again = ops::retry(
        &d.writer,
        &d.config,
        &handlers(&[&clue]),
        RetryTarget::All,
        &d.clock(),
    )
    .unwrap();
    assert_eq!(again.requeued + again.obsolete + again.satisfied, 0);
}

#[test]
fn retry_keeps_failed_jobs_it_cannot_check_and_reports_only_counts() {
    let d = db("ops-retry-keep", Some(SAMPLE));
    // photo_judge は handler が未登録。location_suggest は登録済みだが設定で使えない（クラウド未許可）
    d.failed(JobKind::PhotoJudge, "j1", "bad_response");
    d.failed(JobKind::PhotoJudge, "j2", "interrupted");
    d.failed(JobKind::LocationSuggest, "s1", "bad_response");
    d.failed(JobKind::LocationClue, "c1", "bad_response");
    let suggest = Fake::ok(JobKind::LocationSuggest);
    let clue = Fake::ok(JobKind::LocationClue);
    let report = ops::retry(
        &d.writer,
        &d.config,
        &handlers(&[&suggest, &clue]),
        RetryTarget::All,
        &d.clock(),
    )
    .unwrap();
    assert_eq!(
        report,
        RetryReport {
            requeued: 1,
            obsolete: 0,
            satisfied: 0,
            unsupported: 2,
            policy_disabled: 1
        }
    );
    // 検査できないものは failed のまま（カウンタも履歴もそのまま。安易に Requeue しない）
    for (kind, t) in [
        (JobKind::PhotoJudge, "j1"),
        (JobKind::PhotoJudge, "j2"),
        (JobKind::LocationSuggest, "s1"),
    ] {
        assert_eq!(d.state(kind, t), ("failed".into(), 5, 1), "{t}");
    }
    assert_eq!(d.state(JobKind::LocationClue, "c1").0, "pending");

    // 設定が未指定でも、どの failed も戻さない（env が違う CLI で実行しても壊さない）
    let d = db("ops-retry-unset", None);
    d.failed(JobKind::LocationClue, "c1", "bad_response");
    let clue = Fake::ok(JobKind::LocationClue);
    let report = ops::retry(
        &d.writer,
        &d.config,
        &handlers(&[&clue]),
        RetryTarget::All,
        &d.clock(),
    )
    .unwrap();
    assert_eq!((report.requeued, report.policy_disabled), (0, 1));
    assert_eq!(d.state(JobKind::LocationClue, "c1").0, "failed");
}

#[test]
fn a_requeued_job_is_runnable_again_and_the_count_reset_is_the_only_way_back() {
    let d = db("ops-retry-run", Some(SAMPLE));
    d.failed(JobKind::LocationClue, "c1", "bad_response");
    // enqueue（reconcile）は failed を戻さない
    let outcome = d
        .writer
        .call(|c| jobs::enqueue(c, &key(JobKind::LocationClue, "c1"), T0 + 5))
        .unwrap();
    assert_eq!(outcome, jobs::Enqueued::Existing(JobState::Failed));
    let clue = Fake::ok(JobKind::LocationClue);
    ops::retry(
        &d.writer,
        &d.config,
        &handlers(&[&clue]),
        RetryTarget::All,
        &d.clock(),
    )
    .unwrap();
    let claimed = d
        .writer
        .call(|c| jobs::claim_next(c, T0 + 10, "r", "t", &[JobKind::LocationClue]))
        .unwrap();
    assert!(claimed.is_some());
}

// ---------------------------------------------------------------- 停止の解除

#[test]
fn release_pause_by_task_removes_only_pauses_matching_the_current_config() {
    let d = db("ops-release", Some(SAMPLE));
    let settings = super::tests_settings::sample();
    let other = super::tests_settings::parse(&super::tests_settings::variant(
        "192.0.2.10:8100",
        "192.0.2.77:8100",
    ))
    .unwrap();
    let (cur_p, cur_t) = (
        settings
            .provider_pause_key(TaskKind::ImageEmbedding)
            .unwrap(),
        settings.task_pause_key(TaskKind::ImageEmbedding).unwrap(),
    );
    let stale = other.task_pause_key(TaskKind::ImageEmbedding).unwrap();
    let judge = settings.provider_pause_key(TaskKind::PhotoJudge).unwrap();
    d.writer
        .call(move |c| {
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &cur_p,
                PauseReason::Unauthorized,
                T0,
            )?;
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &cur_t,
                PauseReason::ModelNotFound,
                T0,
            )?;
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &stale,
                PauseReason::Forbidden,
                T0,
            )?;
            pause::pause(c, TaskKind::PhotoJudge, &judge, PauseReason::Forbidden, T0)?;
            Ok(())
        })
        .unwrap();
    let removed = ops::release_pause(
        &d.writer,
        &d.config,
        PauseTarget::Task(TaskKind::ImageEmbedding),
    )
    .unwrap();
    assert_eq!(removed, 2, "Provider の停止と用途の停止");
    let left = d.writer.call(|c| pause::list(c)).unwrap();
    assert_eq!(
        left.len(),
        2,
        "別の設定の停止と、別の Provider の停止は残る"
    );
    // --all は全部（設定に当たらないものも）
    assert_eq!(
        ops::release_pause(&d.writer, &d.config, PauseTarget::All).unwrap(),
        2
    );
    assert!(d.writer.call(|c| pause::list(c)).unwrap().is_empty());
}

#[test]
fn release_pause_by_task_does_nothing_when_the_config_cannot_identify_the_pause() {
    for (tag, json) in [("ops-release-bad", Some("{")), ("ops-release-unset", None)] {
        let d = db(tag, json);
        let settings = super::tests_settings::sample();
        let key = settings
            .provider_pause_key(TaskKind::ImageEmbedding)
            .unwrap();
        d.writer
            .call(move |c| {
                pause::pause(
                    c,
                    TaskKind::ImageEmbedding,
                    &key,
                    PauseReason::Unauthorized,
                    T0,
                )
            })
            .unwrap();
        assert_eq!(
            ops::release_pause(
                &d.writer,
                &d.config,
                PauseTarget::Task(TaskKind::ImageEmbedding)
            ),
            Err(ReleaseError::ConfigUnavailable)
        );
        assert_eq!(d.writer.call(|c| pause::list(c)).unwrap().len(), 1, "{tag}");
        // 明示した --all だけが、設定が不正でも消す
        assert_eq!(
            ops::release_pause(&d.writer, &d.config, PauseTarget::All),
            Ok(1)
        );
    }
    // 設定にない用途の解除も何も消さない
    let d = db(
        "ops-release-none",
        Some(&super::tests_settings::variant(
            r#""location_clue": {"provider": "local", "model": "example-decision"},"#,
            "",
        )),
    );
    assert_eq!(
        ops::release_pause(
            &d.writer,
            &d.config,
            PauseTarget::Task(TaskKind::LocationClue)
        ),
        Err(ReleaseError::ConfigUnavailable)
    );
}

// ---------------------------------------------------------------- 共有の一時待機の表示

fn wait_key(provider: String, service: Option<String>) -> WaitKey {
    WaitKey { provider, service }
}

/// 待ちを `until` まで DB に書く。
fn put_wait(d: &Db, key: WaitKey, reason: WaitReason, until: i64) {
    d.writer
        .call(move |c| waits::set(c, &key, reason, until, T0))
        .unwrap();
}

/// 有効な共有の待ちは、用途・範囲・固定の理由・UTC の期限だけを出す。入口全体は同じ Provider の全用途、
/// サービス単体はそのサービスの用途だけ。古い設定の待ち・期限切れは出ない。
#[test]
fn live_shared_waits_are_shown_per_task_with_scope_reason_and_utc_deadline() {
    let d = db("ops-waits", Some(SAMPLE));
    let s = super::tests_settings::sample();
    let local = s.provider_fingerprint(TaskKind::LocationClue).unwrap();
    let cloud = s.provider_fingerprint(TaskKind::PhotoJudge).unwrap();
    // 入口全体（LAN）: 画像・文の Embedding と clue の 3 用途
    put_wait(
        &d,
        wait_key(local.clone(), None),
        WaitReason::Leased,
        T0 + 600,
    );
    // サービス単体（LAN の Embedding サービス）: 画像・文の Embedding だけ（clue の decision は別）
    put_wait(
        &d,
        wait_key(
            local,
            Some(s.service_key(TaskKind::ImageEmbedding).unwrap()),
        ),
        WaitReason::NotReady,
        T0 + 1_200,
    );
    // クラウドのモデル単位
    put_wait(
        &d,
        wait_key(cloud, Some(s.service_key(TaskKind::PhotoJudge).unwrap())),
        WaitReason::RateLimited,
        T0 + 3_600,
    );
    // 古い設定の待ち（今の設定のどれにも当たらない）と、期限切れの待ち
    put_wait(
        &d,
        wait_key("f".repeat(64), None),
        WaitReason::Unreachable,
        T0 + 900,
    );
    put_wait(
        &d,
        wait_key(
            s.provider_fingerprint(TaskKind::LocationSuggest).unwrap(),
            None,
        ),
        WaitReason::Busy,
        T0 + 30,
    );
    let report = ops::report_at(&d.read(), &d.config, T0 + 60).unwrap();
    let shown: Vec<(&str, &str, &str, i64)> = report
        .waits
        .waits
        .iter()
        .map(|w| {
            (
                w.task.as_str(),
                w.scope.as_str(),
                w.reason.as_str(),
                w.until - T0,
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("image_embedding", "entrance", "leased", 600),
            ("image_embedding", "service", "not_ready", 1_200),
            ("query_embedding", "entrance", "leased", 600),
            ("query_embedding", "service", "not_ready", 1_200),
            ("photo_judge", "service", "rate_limited", 3_600),
            ("location_clue", "entrance", "leased", 600),
        ]
    );
    assert_eq!(report.waits.unidentified, 0);

    // STATUS の JSON: 固定の語だけ（ハッシュ・接続先・サービス ID・モデルは出ない）
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0 + 60,
    );
    let items = out.get("ai_waits").unwrap()["items"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(items.len(), 6);
    assert_eq!(
        items[0],
        serde_json::json!({"task": "image_embedding", "scope": "entrance",
                           "reason": "leased", "until": waits::utc_iso(T0 + 600)})
    );
    assert!(out.get("ai_waits").unwrap().get("unidentified").is_none());
    let text = serde_json::Value::Object(out).to_string();
    for secret in [
        "192.0.2.10",
        "example-embed",
        "example-judge",
        "example-project",
        &"f".repeat(64),
    ] {
        assert!(!text.contains(secret), "{secret}\n{text}");
    }
    assert!(!text.contains(&s.provider_fingerprint(TaskKind::LocationClue).unwrap()));

    // 再起動のあと（新しい接続・設定の読み直し）も同じ表示
    let again = db_reopen(&d);
    let report2 = ops::report_at(&again.read(), &again.config, T0 + 60).unwrap();
    assert_eq!(report2.waits, report.waits);

    // 期限が来れば（行が残っていても）出ない。最後の期限（T0+3600）ちょうどでも出ない
    let late = ops::report_at(&d.read(), &d.config, T0 + 3_600).unwrap();
    assert!(late.waits.is_empty(), "{:?}", late.waits);
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0 + 3_600,
    );
    assert!(!out.contains_key("ai_waits"));
}

/// 同じ DB と設定ファイルを新しく開き直す（再起動に当たる）。
fn db_reopen(d: &Db) -> Db {
    let dir = TempDir::new("ops-waits-reopen");
    let config = ai_config(&dir, Some(SAMPLE));
    Db {
        _dir: dir,
        path: d.path.clone(),
        writer: Writer::open(&d.path, OpenMode::CreateIfMissing).unwrap(),
        config,
    }
}

/// 設定が不正・未指定で用途に結び直せないときは、件数だけ（値は出さない）。待ちが無ければキーを足さない。
#[test]
fn unidentifiable_waits_are_only_counted() {
    let d = db("ops-waits-bad-config", Some(SAMPLE));
    let s = super::tests_settings::sample();
    put_wait(
        &d,
        wait_key(
            s.provider_fingerprint(TaskKind::LocationClue).unwrap(),
            None,
        ),
        WaitReason::Unreachable,
        T0 + 600,
    );
    put_wait(
        &d,
        wait_key("a".repeat(64), Some("b".repeat(64))),
        WaitReason::Busy,
        T0 + 600,
    );
    let broken = db("ops-waits-bad-config2", Some("{"));
    for config in [
        &broken.config,
        &ai_config(&TempDir::new("ops-waits-unset"), None),
    ] {
        let ids = waits::Identities::from_config(config);
        let mut out = Map::new();
        ops::insert_admin_status(&mut out, &d.read(), config, &ids, T0 + 60);
        assert_eq!(
            out.get("ai_waits").unwrap(),
            &serde_json::json!({"items": [], "unidentified": 2})
        );
        let text = serde_json::Value::Object(out).to_string();
        assert!(!text.contains("aaaa") && !text.contains("bbbb"), "{text}");
        let report = ops::report_at(&d.read(), config, T0 + 60).unwrap();
        assert_eq!(
            (report.waits.waits.len(), report.waits.unidentified),
            (0, 2)
        );
    }
    // 表の無い旧 DB・待ちの無い DB には何も足さない
    let empty = db("ops-waits-none", Some(SAMPLE));
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &empty.read(),
        &empty.config,
        &waits::Identities::from_config(&empty.config),
        T0,
    );
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(WaitScope::Entrance.as_str(), "entrance");
}

// ---------------------------------------------------------------- 停止にかかる全用途

/// `location_suggest` もクラウド許可と日の上限つきで使える設定（`photo_judge` と同じ認証・project・region を共有する）。
fn suggest_allowed() -> String {
    let json = SAMPLE.replacen(
        "\"model\": \"example-suggest\",",
        "\"model\": \"example-suggest\", \"allow_cloud\": true, \"daily_request_limit\": 5,",
        1,
    );
    assert_ne!(json, SAMPLE);
    json
}

impl Db {
    /// 指定の状態のジョブを積む。
    fn job(&self, kind: JobKind, target: &str, state: &str) {
        let (k, t, state) = (key(kind, target), target.to_string(), state.to_string());
        self.writer
            .call(move |c| {
                jobs::enqueue(c, &k, T0)?;
                c.execute(
                    "UPDATE ai_jobs SET state=?2 WHERE target_key=?1",
                    rusqlite::params![t, state],
                )
            })
            .unwrap();
    }

    fn pause_with(&self, task: TaskKind, provider_scope: bool, reason: PauseReason) {
        let settings = self.config.settings().unwrap().clone();
        self.writer
            .call(move |c| {
                let key = if provider_scope {
                    settings.provider_pause_key(task)
                } else {
                    settings.task_pause_key(task)
                }
                .unwrap();
                pause::pause(c, task, &key, reason, T0).map(|_| ())
            })
            .unwrap();
    }
}

fn task_report(report: &ops::Report, task: TaskKind) -> ops::TaskReport {
    *report.tasks.iter().find(|t| t.task == task).unwrap()
}

#[test]
fn a_provider_pause_marks_every_task_behind_it_and_counts_their_waiting_jobs() {
    let d = db("ops-blocks-provider", Some(&suggest_allowed()));
    d.job(JobKind::PhotoJudge, "j1", "pending");
    d.job(JobKind::PhotoJudge, "j2", "deferred");
    d.job(JobKind::PhotoJudge, "j3", "succeeded");
    d.job(JobKind::LocationSuggest, "s1", "pending");
    // location_geocode は location_suggest の待ちに数える。failed・running は待ちではない
    d.job(JobKind::LocationGeocode, "g1", "deferred");
    d.job(JobKind::LocationSuggest, "s2", "failed");
    d.job(JobKind::LocationSuggest, "s3", "running");
    // 別の Provider（LAN）の用途は止まらない
    d.job(JobKind::LocationClue, "c1", "pending");
    // 停止を起こしたのは location_suggest だけ。photo_judge も同じ認証・project・region なので止まる
    d.pause_with(TaskKind::LocationSuggest, true, PauseReason::Forbidden);

    let report = ops::report_at(&d.read(), &d.config, T0 + 60).unwrap();
    let paused = TaskPause::Paused(pause::Paused {
        scope: PauseScope::Provider,
        reason: PauseReason::Forbidden,
    });
    for (task, waiting) in [(TaskKind::PhotoJudge, 2), (TaskKind::LocationSuggest, 2)] {
        let t = task_report(&report, task);
        assert_eq!(
            (t.gate, t.pause, t.waiting),
            (Ok(()), paused, waiting),
            "{task:?}"
        );
    }
    for task in [
        TaskKind::ImageEmbedding,
        TaskKind::QueryEmbedding,
        TaskKind::LocationClue,
    ] {
        let t = task_report(&report, task);
        assert_eq!((t.pause, t.waiting), (TaskPause::None, 0), "{task:?}");
    }
    // 既存の一覧は停止を起こした用途だけ
    assert_eq!(report.pauses.len(), 1);

    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0 + 60,
    );
    assert_eq!(
        out.get("ai_task_blocks").unwrap(),
        &serde_json::json!([
            {"task": "photo_judge", "scope": "provider", "reason": "forbidden", "waiting": 2},
            {"task": "location_suggest", "scope": "provider", "reason": "forbidden", "waiting": 2},
        ])
    );
    assert_eq!(
        out.get("ai_pauses").unwrap(),
        &serde_json::json!([{"task": "location_suggest", "scope": "provider", "reason": "forbidden"}])
    );
    // 出すのは用途名・範囲・固定の理由・件数だけ
    let text = Value::Object(out).to_string();
    for secret in [
        "credentials.json",
        "example-project",
        "example-region1",
        "192.0.2.10",
        "example-suggest",
        "example-judge",
        "SECRET",
    ] {
        assert!(!text.contains(secret), "{secret}\n{text}");
    }
    let debug = format!("{report:?}");
    for secret in ["credentials.json", "example-project", "192.0.2.10"] {
        assert!(!debug.contains(secret), "{secret}\n{debug}");
    }
}

#[test]
fn a_task_pause_marks_only_that_task_and_a_task_not_usable_by_settings_is_not_listed() {
    let d = db("ops-blocks-task", Some(&suggest_allowed()));
    d.job(JobKind::LocationClue, "c1", "deferred");
    d.pause_with(TaskKind::LocationClue, false, PauseReason::ModelNotFound);
    let report = ops::report_at(&d.read(), &d.config, T0).unwrap();
    let t = task_report(&report, TaskKind::LocationClue);
    assert_eq!(
        (t.pause, t.waiting),
        (
            TaskPause::Paused(pause::Paused {
                scope: PauseScope::Task,
                reason: PauseReason::ModelNotFound
            }),
            1
        )
    );
    assert_eq!(
        report
            .tasks
            .iter()
            .filter(|t| t.pause != TaskPause::None)
            .count(),
        1
    );
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0,
    );
    assert_eq!(
        out.get("ai_task_blocks").unwrap(),
        &serde_json::json!([
            {"task": "location_clue", "scope": "task", "reason": "model_not_found", "waiting": 1}
        ])
    );

    // SAMPLE の location_suggest は allow_cloud が無く設定で使えない。同じ認証の photo_judge が止まっても、
    // 使えない用途は「停止中」にせず（使えない理由のまま）、ai_task_blocks にも載せない
    let sample = db("ops-blocks-disabled", Some(SAMPLE));
    sample.pause_with(TaskKind::PhotoJudge, true, PauseReason::Unauthorized);
    let report = ops::report_at(&sample.read(), &sample.config, T0).unwrap();
    let suggest = task_report(&report, TaskKind::LocationSuggest);
    assert!(suggest.gate.is_err());
    assert_eq!(suggest.pause, TaskPause::None);
    assert!(matches!(
        task_report(&report, TaskKind::PhotoJudge).pause,
        TaskPause::Paused(_)
    ));
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &sample.read(),
        &sample.config,
        &waits::Identities::from_config(&sample.config),
        T0,
    );
    let blocks = out.get("ai_task_blocks").unwrap().as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["task"], "photo_judge");
}

#[test]
fn without_a_pause_nothing_is_marked_and_no_key_is_added() {
    let d = db("ops-blocks-none", Some(&suggest_allowed()));
    d.job(JobKind::PhotoJudge, "j1", "pending");
    let report = ops::report_at(&d.read(), &d.config, T0).unwrap();
    assert!(
        report
            .tasks
            .iter()
            .all(|t| t.pause == TaskPause::None && t.waiting == 0)
    );
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0,
    );
    assert!(out.get("ai_task_blocks").is_none(), "{out:?}");
    assert!(out.get("ai_pauses").is_none());
}

#[test]
fn a_pause_lookup_that_fails_is_reported_as_unknown_and_not_as_a_block() {
    let d = db("ops-blocks-unknown", Some(&suggest_allowed()));
    // 一覧（scope・task・reason・created_at）は読めるが、識別（subject）で引けない表
    d.writer
        .call(|c| {
            c.execute_batch(
                "DROP TABLE ai_provider_pauses;
                 CREATE TABLE ai_provider_pauses(scope TEXT, task TEXT, reason TEXT, created_at INTEGER);",
            )
        })
        .unwrap();
    let report = ops::report_at(&d.read(), &d.config, T0).unwrap();
    for task in [TaskKind::PhotoJudge, TaskKind::LocationSuggest] {
        let t = task_report(&report, task);
        assert_eq!(
            (t.gate, t.pause, t.waiting),
            (Ok(()), TaskPause::Unknown, 0)
        );
    }
    // 確認できないものは「停止中」として出さない（送信側は読めないときも送らない）
    let mut out = Map::new();
    ops::insert_admin_status(
        &mut out,
        &d.read(),
        &d.config,
        &waits::Identities::from_config(&d.config),
        T0,
    );
    assert!(out.get("ai_task_blocks").is_none(), "{out:?}");
}
