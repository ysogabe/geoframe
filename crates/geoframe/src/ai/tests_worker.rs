//! AI の背景ワーカー（起動の準備・1 周期・停止・回収）の試験。合成の DB・偽の時計・**このテストだけの fake handler**
//! だけを使い、外部通信はしない（fake は本番の経路に無い）。
//!
//! - 正常な完了・再起動（冪等）・未登録の種類は claim も成功化もされない・使えない用途は飛ばして他が進む
//! - 古い token の応答・入力が変わった応答は保存されない
//! - 停止: 停止後は claim も送信も 0・待機中の有効な応答は確定・join 期限後は戻して token 失効・ロックが残る
//! - 停止の共通期限（writer が忙しくても越えない）
//! - 回収: 停止開始が記録された起動は数えず、`SIGKILL` 相当は 3 回で `failed`
//! - handler 待機中も DB の読み書きができる／panic で全 AI が止まらない／ログに秘匿情報を出さない

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::budget::Reservation;
use super::jobs::{self, Claimed, ErrorCode, InputCheck, JobKey, JobKind, JobState, Recheck};
use super::pause::{self, PauseReason};
use super::runs;
use super::settings::TaskKind;
use super::tests_settings::{SAMPLE, variant};
use super::worker::*;
use crate::config::{Config, Overrides};
use crate::guard::Guard;
use crate::ingest::{self, Ingest, IngestError};
use crate::stop::StopFlag;
use crate::testutil::TempDir;
use crate::writer::Writer;

/// 2026-10-26T01:00:00Z（UTC 日の途中）。
pub(super) const T0: i64 = 1_792_972_800 + 3_600;

// ---------------------------------------------------------------- 準備

pub(super) struct Env {
    _dir: TempDir,
    pub(super) config: Arc<Config>,
    ingest: Option<Ingest>,
    clock: Arc<AtomicI64>,
}

pub(super) fn env(tag: &str) -> Env {
    env_with(tag, SAMPLE, true)
}

pub(super) fn env_with(tag: &str, ai_json: &str, ingest_on: bool) -> Env {
    let dir = TempDir::new(tag);
    let ai_path = dir.path().join("ai.json");
    std::fs::write(&ai_path, ai_json).unwrap();
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert(
        "GEOFRAME_AI_CONFIG".into(),
        ai_path.to_str().unwrap().into(),
    );
    if ingest_on {
        vars.insert("GEOFRAME_INGEST".into(), "1".into());
    }
    let overrides = Overrides {
        listen: None,
        data_dir: Some(dir.path().join("data")),
    };
    let config = Arc::new(Config::load(|k| vars.get(k).cloned(), &overrides).unwrap());
    let ingest = ingest::start_with(&config, &Guard::skip()).unwrap();
    // 結果の保存先の合成の表（本物の結果表は後続のタスク）
    ingest
        .writer
        .call(|c| {
            c.execute_batch(
                "CREATE TABLE ai_test_results(target TEXT NOT NULL);
                 INSERT INTO photos(source,drive_id,name,lat,lon) VALUES ('drive','a','n1',1.0,2.0);",
            )
        })
        .unwrap();
    Env {
        _dir: dir,
        config,
        ingest: Some(ingest),
        clock: Arc::new(AtomicI64::new(T0)),
    }
}

impl Env {
    pub(super) fn writer(&self) -> Writer {
        self.ingest.as_ref().unwrap().writer.clone()
    }

    pub(super) fn clock(&self) -> Clock {
        let c = Arc::clone(&self.clock);
        Arc::new(move || c.load(Ordering::SeqCst))
    }

    fn set_clock(&self, now: i64) {
        self.clock.store(now, Ordering::SeqCst);
    }

    pub(super) fn advance(&self, secs: i64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    pub(super) fn deps(
        &self,
        handlers: Vec<Arc<dyn Handler>>,
        stop: &StopFlag,
        run_id: &str,
    ) -> WorkerDeps {
        WorkerDeps {
            config: Arc::clone(&self.config),
            writer: self.writer(),
            stop: stop.clone(),
            lock: self.ingest.as_ref().unwrap().lock_hold(),
            handlers,
            clock: self.clock(),
            poll: Duration::from_millis(20),
            run_id: run_id.to_string(),
        }
    }

    /// 1 周期だけ同期的に動かす。
    pub(super) fn cycle(&self, handlers: &[Arc<Fake>], stop: &StopFlag) {
        let handlers = handlers
            .iter()
            .map(|h| Arc::clone(h) as Arc<dyn Handler>)
            .collect();
        Core::new(&self.deps(handlers, stop, "run-test")).cycle();
    }

    pub(super) fn row(&self, kind: JobKind, target: &str) -> Option<Row> {
        let (k, t) = (kind.as_str(), target.to_string());
        self.writer()
            .call(move |c| {
                c.query_row(
                    "SELECT state, attempts, interruption_count, last_error_code,
                            claim_token IS NOT NULL, next_attempt_at
                     FROM ai_jobs WHERE kind=?1 AND target_key=?2",
                    rusqlite::params![k, t],
                    |r| {
                        Ok(Row {
                            state: JobState::parse(&r.get::<_, String>(0)?).unwrap(),
                            attempts: r.get(1)?,
                            interruptions: r.get(2)?,
                            code: r.get(3)?,
                            has_token: r.get(4)?,
                            next: r.get(5)?,
                        })
                    },
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })
            })
            .unwrap()
    }

    pub(super) fn state(&self, kind: JobKind, target: &str) -> JobState {
        self.row(kind, target).expect("ジョブがある").state
    }

    pub(super) fn results(&self) -> i64 {
        self.writer()
            .call(|c| c.query_row("SELECT COUNT(*) FROM ai_test_results", [], |r| r.get(0)))
            .unwrap()
    }

    pub(super) fn enqueue(&self, key: &JobKey) {
        let key = key.clone();
        let now = self.clock.load(Ordering::SeqCst);
        self.writer()
            .call(move |c| jobs::enqueue(c, &key, now))
            .unwrap();
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Row {
    pub(super) state: JobState,
    pub(super) attempts: u32,
    pub(super) interruptions: u32,
    pub(super) code: String,
    pub(super) has_token: bool,
    pub(super) next: i64,
}

pub(super) fn key(kind: JobKind, target: &str) -> JobKey {
    JobKey::new(kind, target, "in-1", "p1")
}

pub(super) fn persist_result(target: String, check: InputCheck) -> Persist {
    Box::new(move |conn: &Connection| {
        conn.execute("INSERT INTO ai_test_results(target) VALUES (?1)", [&target])?;
        Ok(check)
    })
}

type Behavior = dyn Fn(&RunCtx<'_>, &Claimed) -> Result<Persist, RunError> + Send + Sync;

/// このテストだけの用途別処理: reconcile は決まったキーを返し、`run` は注入した振る舞い。
pub(super) struct Fake {
    kind: JobKind,
    keys: Mutex<Vec<JobKey>>,
    behavior: Box<Behavior>,
    runs: AtomicUsize,
    pub(super) verdicts: Mutex<HashMap<String, Recheck>>,
}

impl Fake {
    pub(super) fn new(
        kind: JobKind,
        behavior: impl Fn(&RunCtx<'_>, &Claimed) -> Result<Persist, RunError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            kind,
            keys: Mutex::new(Vec::new()),
            behavior: Box::new(behavior),
            runs: AtomicUsize::new(0),
            verdicts: Mutex::new(HashMap::new()),
        })
    }

    /// 結果を保存して成功する fake。
    pub(super) fn ok(kind: JobKind) -> Arc<Self> {
        Self::new(kind, |_, job| {
            Ok(persist_result(
                job.key.target_key.clone(),
                InputCheck::Current,
            ))
        })
    }

    fn with_keys(self: Arc<Self>, keys: &[JobKey]) -> Arc<Self> {
        *self.keys.lock().unwrap() = keys.to_vec();
        self
    }

    pub(super) fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

impl Handler for Fake {
    fn kind(&self) -> JobKind {
        self.kind
    }

    fn reconcile(&self, _read: &Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        Ok(self.keys.lock().unwrap().clone())
    }

    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        (self.behavior)(ctx, job)
    }

    fn recheck(&self, _conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        Ok(self
            .verdicts
            .lock()
            .unwrap()
            .get(&key.target_key)
            .copied()
            .unwrap_or(Recheck::Requeue))
    }
}

fn dyn_handlers(list: &[Arc<Fake>]) -> Vec<Arc<dyn Handler>> {
    list.iter()
        .map(|h| Arc::clone(h) as Arc<dyn Handler>)
        .collect()
}

// ---------------------------------------------------------------- 起動の条件・登録

#[test]
fn the_shipped_binary_registers_only_location_clue_and_a_worker_starts_only_with_ingest_and_a_background_task()
 {
    // 配布バイナリは location_clue のみ（fake・本番のスタブは置かない）
    let e = env("production-registration");
    let handlers =
        production_handlers_for_test(&e.config, crate::ai::provider::LanClient::new(&e.config.ai));
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].kind(), JobKind::LocationClue);

    // 取り込み有効 ＋ AI 有効 ＋ 有効な背景用途あり
    assert!(should_start(&env("start-ok").config));
    // INGEST=0 は何も起動しない
    assert!(!should_start(
        &env_with("start-no-ingest", SAMPLE, false).config
    ));
    // 設定ファイルが無い（未指定）／AI 無効／有効な用途が query_embedding だけ → 起動しない
    let only_query = r#"{"version":1,"local_base_url":"http://192.0.2.10:8100",
      "embedding_space":{"space_id":"s","model_id":"m","revision":"r","image_preprocess":"a",
        "text_preprocess":"b","dimension":4,"normalization":"l2"},
      "tasks":{"query_embedding":{"provider":"local","model":"m","service_id":"svc"}}}"#;
    let e = env_with("start-query-only", only_query, true);
    assert_eq!(e.config.ai.enabled_tasks(true), [TaskKind::QueryEmbedding]);
    assert!(background_tasks(&e.config.ai, true).is_empty());
    assert!(should_start(&e.config));
    assert!(!should_start(&env_with("start-bad", "{", true).config));
    let mut vars = HashMap::new();
    vars.insert("GEOFRAME_INGEST".to_string(), "1".to_string());
    let unset = Config::load(|k| vars.get(k).cloned(), &Overrides::default()).unwrap();
    assert!(should_start(&unset));
    // 旧設定で AI を無効にすると、設定が有効でも起動しない
    let dir = TempDir::new("start-off");
    let path = dir.path().join("ai.json");
    std::fs::write(&path, SAMPLE).unwrap();
    let mut vars = HashMap::new();
    vars.insert("GEOFRAME_INGEST".to_string(), "1".to_string());
    vars.insert("GEOFRAME_AI_ENABLED".to_string(), "0".to_string());
    vars.insert(
        "GEOFRAME_AI_CONFIG".to_string(),
        path.to_str().unwrap().into(),
    );
    let off = Config::load(|k| vars.get(k).cloned(), &Overrides::default()).unwrap();
    assert!(!should_start(&off));
}

// ---------------------------------------------------------------- 完了・再起動・未登録

#[test]
fn a_registered_kind_is_reconciled_run_and_completed_and_a_restart_repeats_nothing() {
    let e = env("complete");
    let stop = StopFlag::new();
    let clue = Fake::ok(JobKind::LocationClue).with_keys(&[
        key(JobKind::LocationClue, "a"),
        key(JobKind::LocationClue, "b"),
    ]);
    e.cycle(&[Arc::clone(&clue)], &stop);
    for t in ["a", "b"] {
        let row = e.row(JobKind::LocationClue, t).unwrap();
        assert_eq!(row.state, JobState::Succeeded, "{t}");
        assert!(!row.has_token && row.code.is_empty());
    }
    assert_eq!((clue.runs(), e.results()), (2, 2));

    // 再起動（新しいワーカー・新しい run_id）。成功済みは再実行・再課金されない
    let stop2 = StopFlag::new();
    Core::new(&e.deps(
        dyn_handlers(&[Arc::clone(&clue)]),
        &stop2,
        "run-after-restart",
    ))
    .cycle();
    assert_eq!((clue.runs(), e.results()), (2, 2));
    // 新しい入力は別のジョブ（古い世代は積み直されない）
    clue.keys
        .lock()
        .unwrap()
        .push(JobKey::new(JobKind::LocationClue, "a", "in-2", "p1"));
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert_eq!((clue.runs(), e.results()), (3, 3));
}

#[test]
fn an_unregistered_kind_is_never_claimed_or_marked_successful() {
    let e = env("unregistered");
    // 登録は location_clue だけ。photo_judge（SAMPLE では有効なクラウド用途）のジョブは登録が無い
    e.enqueue(&key(JobKind::PhotoJudge, "j"));
    e.enqueue(&key(JobKind::ImageEmbedding, "i"));
    let clue = Fake::ok(JobKind::LocationClue).with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Succeeded);
    for (kind, t) in [(JobKind::PhotoJudge, "j"), (JobKind::ImageEmbedding, "i")] {
        let row = e.row(kind, t).unwrap();
        assert_eq!(row.state, JobState::Pending);
        assert_eq!((row.attempts, row.interruptions), (0, 0));
        assert!(!row.has_token);
    }
}

#[test]
fn a_kind_whose_policy_is_disabled_is_neither_reconciled_nor_claimed() {
    let e = env("policy");
    // SAMPLE の location_suggest はクラウドで allow_cloud が無いので使えない（location_geocode も同じ用途）
    assert!(e.config.ai.gate(TaskKind::LocationSuggest, true).is_err());
    e.enqueue(&key(JobKind::LocationSuggest, "pre"));
    e.enqueue(&key(JobKind::LocationGeocode, "pre"));
    let suggest =
        Fake::ok(JobKind::LocationSuggest).with_keys(&[key(JobKind::LocationSuggest, "x")]);
    let geocode =
        Fake::ok(JobKind::LocationGeocode).with_keys(&[key(JobKind::LocationGeocode, "x")]);
    e.cycle(
        &[Arc::clone(&suggest), Arc::clone(&geocode)],
        &StopFlag::new(),
    );
    assert_eq!((suggest.runs(), geocode.runs()), (0, 0));
    // 積まれない（無効の理由は状態の集計に出る）。事前のジョブはそのまま
    assert!(e.row(JobKind::LocationSuggest, "x").is_none());
    assert_eq!(e.state(JobKind::LocationSuggest, "pre"), JobState::Pending);
    assert_eq!(e.state(JobKind::LocationGeocode, "pre"), JobState::Pending);
}

// ---------------------------------------------------------------- 阻害の分離

#[test]
fn an_unavailable_provider_defers_its_kind_while_other_kinds_progress() {
    let e = env("unavailable");
    let down = Arc::new(AtomicUsize::new(1));
    let flag = Arc::clone(&down);
    let clue = Fake::new(JobKind::LocationClue, move |_, job| {
        if flag.load(Ordering::SeqCst) == 1 {
            Err(RunError::Unavailable)
        } else {
            Ok(persist_result(
                job.key.target_key.clone(),
                InputCheck::Current,
            ))
        }
    })
    .with_keys(&[
        key(JobKind::LocationClue, "c1"),
        key(JobKind::LocationClue, "c2"),
    ]);
    let embed = Fake::ok(JobKind::ImageEmbedding).with_keys(&[
        key(JobKind::ImageEmbedding, "e1"),
        key(JobKind::ImageEmbedding, "e2"),
    ]);
    // 同じ LAN の入口の別の用途。location_clue の「利用不可」で image_embedding は止まらない
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    let c1 = e.row(JobKind::LocationClue, "c1").unwrap();
    assert_eq!(c1.state, JobState::Deferred);
    assert_eq!(c1.code, "unavailable");
    assert_eq!(c1.attempts, 0, "後回しは失敗に数えない");
    assert_eq!(c1.next, T0 + 60);
    // Provider 側の待ちが明けるまで、同じ用途は次のジョブも claim しない（連打しない）
    assert_eq!(clue.runs(), 1);
    assert_eq!(e.state(JobKind::LocationClue, "c2"), JobState::Pending);
    assert_eq!(e.state(JobKind::ImageEmbedding, "e1"), JobState::Succeeded);
    assert_eq!(e.state(JobKind::ImageEmbedding, "e2"), JobState::Succeeded);

    // 復帰: 時間が経ち、Provider が使えるようになれば再開する
    down.store(0, Ordering::SeqCst);
    e.advance(61);
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "c1"), JobState::Succeeded);
    assert_eq!(e.state(JobKind::LocationClue, "c2"), JobState::Succeeded);
}

#[test]
fn a_rate_limit_waits_for_the_clamped_retry_after() {
    let e = env("ratelimit");
    let clue = Fake::new(JobKind::LocationClue, |_, _| {
        Err(RunError::RateLimited {
            retry_after_secs: Some(5),
        })
    })
    .with_keys(&[key(JobKind::LocationClue, "c1")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    let row = e.row(JobKind::LocationClue, "c1").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.attempts),
        (JobState::Deferred, "rate_limited", 0)
    );
    assert_eq!(row.next, T0 + 60, "Retry-After は 60 秒以上に収める");
}

#[test]
fn a_task_pause_is_recorded_skips_only_that_kind_and_survives_a_restart_until_released() {
    let e = env("task-pause");
    let clue = Fake::new(JobKind::LocationClue, |_, _| {
        Err(RunError::TaskPause(PauseReason::ModelNotFound))
    })
    .with_keys(&[
        key(JobKind::LocationClue, "c1"),
        key(JobKind::LocationClue, "c2"),
    ]);
    let embed = Fake::ok(JobKind::ImageEmbedding).with_keys(&[key(JobKind::ImageEmbedding, "e1")]);
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    // 停止は DB に残り、ジョブは失敗に数えず後回し。他の用途は進む
    let pauses = e.writer().call(|c| pause::list(c)).unwrap();
    assert_eq!(pauses.len(), 1);
    assert_eq!(
        (pauses[0].task, pauses[0].reason),
        (TaskKind::LocationClue, PauseReason::ModelNotFound)
    );
    let c1 = e.row(JobKind::LocationClue, "c1").unwrap();
    assert_eq!(
        (c1.state, c1.attempts, c1.code.as_str()),
        (JobState::Deferred, 0, "provider_paused")
    );
    assert_eq!(e.state(JobKind::ImageEmbedding, "e1"), JobState::Succeeded);
    assert_eq!(clue.runs(), 1);

    // 時間が経っても、再起動（新しいワーカー）しても、解除されるまで新しい要求を出さない
    e.advance(86_400);
    Core::new(&e.deps(
        dyn_handlers(&[Arc::clone(&clue)]),
        &StopFlag::new(),
        "restarted",
    ))
    .cycle();
    assert_eq!(clue.runs(), 1);
    assert_eq!(e.state(JobKind::LocationClue, "c2"), JobState::Pending);

    // 明示の解除のあと再開する
    let released = super::ops::release_pause(
        &e.writer(),
        &e.config.ai,
        super::ops::PauseTarget::Task(TaskKind::LocationClue),
    )
    .unwrap();
    assert_eq!(released, 1);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(clue.runs(), 2, "解除されたので要求を出す");
}

#[test]
fn a_provider_pause_stops_every_task_that_shares_the_destination_but_not_a_cloud_task() {
    let e = env("provider-pause");
    let clue = Fake::new(JobKind::LocationClue, |_, _| {
        Err(RunError::ProviderPause(PauseReason::Unauthorized))
    })
    .with_keys(&[key(JobKind::LocationClue, "c1")]);
    let embed = Fake::ok(JobKind::ImageEmbedding).with_keys(&[key(JobKind::ImageEmbedding, "e1")]);
    // photo_judge は SAMPLE ではクラウド（別の接続先・認証）で、LAN の停止には当たらない
    let judge = Fake::ok(JobKind::PhotoJudge).with_keys(&[key(JobKind::PhotoJudge, "j1")]);
    // 1 周期目で location_clue が Provider を止める。image_embedding は同じ周期でまだ claim 前なら止まる
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &StopFlag::new(),
    );
    assert_eq!(e.state(JobKind::LocationClue, "c1"), JobState::Deferred);
    assert_eq!(e.state(JobKind::PhotoJudge, "j1"), JobState::Succeeded);
    // 2 周期目: LAN の用途は全て止まったまま（image_embedding の新しいジョブも走らない）
    embed
        .keys
        .lock()
        .unwrap()
        .push(key(JobKind::ImageEmbedding, "e2"));
    e.advance(3_600);
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &StopFlag::new(),
    );
    assert_eq!(embed.runs(), 0);
    assert_eq!(e.state(JobKind::ImageEmbedding, "e2"), JobState::Pending);
    let reasons: Vec<_> = e
        .writer()
        .call(|c| pause::list(c))
        .unwrap()
        .into_iter()
        .map(|p| (p.scope, p.reason))
        .collect();
    assert_eq!(
        reasons,
        [(pause::PauseScope::Provider, PauseReason::Unauthorized)]
    );
}

#[test]
fn a_panicking_handler_is_counted_against_that_job_only_and_ends_in_failed() {
    let e = env("panic");
    let boom = Fake::new(JobKind::LocationClue, |_, _| panic!("SECRET-PANIC-MESSAGE"))
        .with_keys(&[key(JobKind::LocationClue, "boom")]);
    let embed =
        Fake::ok(JobKind::ImageEmbedding).with_keys(&[key(JobKind::ImageEmbedding, "fine")]);
    for round in 1..=5 {
        e.cycle(&[Arc::clone(&boom), Arc::clone(&embed)], &StopFlag::new());
        let row = e.row(JobKind::LocationClue, "boom").unwrap();
        assert_eq!(row.attempts, round, "panic は失敗として数える");
        assert_eq!(row.code, "panic");
        e.advance(1_000);
    }
    let row = e.row(JobKind::LocationClue, "boom").unwrap();
    assert_eq!(
        row.state,
        JobState::Failed,
        "5 回で failed。全 AI を塞ぐ無限ループにならない"
    );
    assert_eq!(boom.runs(), 5);
    // 別の用途は panic の影響を受けずに完了している
    assert_eq!(
        e.state(JobKind::ImageEmbedding, "fine"),
        JobState::Succeeded
    );
    // failed になったあとは claim されない
    e.cycle(&[Arc::clone(&boom)], &StopFlag::new());
    assert_eq!(boom.runs(), 5);
}

// ---------------------------------------------------------------- 入力・token の保護

#[test]
fn a_response_for_a_changed_input_is_discarded_and_the_job_becomes_obsolete() {
    let e = env("cas");
    let clue = Fake::new(JobKind::LocationClue, |_, job| {
        // 現在の入力の検査が「変わっている」と答える（写真が更新された）。persist が書いた結果は捨てられる
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Changed,
        ))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Obsolete);
    assert_eq!(e.results(), 0);

    // 送信前に入力が古いと分かった場合も obsolete（送らない）
    let e = env("cas2");
    let clue = Fake::new(JobKind::LocationClue, |_, _| Err(RunError::InputChanged))
        .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Obsolete);
}

#[test]
fn a_stale_token_response_after_the_input_was_replaced_is_rejected() {
    let e = env("stale");
    let writer = e.writer();
    let new_key = JobKey::new(JobKind::LocationClue, "c", "in-2", "p1");
    let replaced = std::sync::atomic::AtomicBool::new(false);
    let clue = Fake::new(JobKind::LocationClue, move |_, job| {
        // 最初の応答を待つ間に、同じ対象の入力が更新されて新しい世代のジョブができる（古い世代は supersede で無効）
        if !replaced.swap(true, Ordering::SeqCst) {
            let k = new_key.clone();
            writer
                .call(move |c| {
                    jobs::enqueue(c, &k, T0)?;
                    jobs::supersede(c, &k, T0)
                })
                .unwrap();
        }
        let label = format!("{}:{}", job.key.target_key, job.key.input_hash);
        Ok(persist_result(label, InputCheck::Current))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    // 古い世代（in-1）の応答は保存されず、古いジョブは無効のまま。新しい世代（in-2）だけが保存される
    let saved: Vec<String> = e
        .writer()
        .call(|c| {
            let mut s = c.prepare("SELECT target FROM ai_test_results ORDER BY rowid")?;
            s.query_map([], |r| r.get(0))?.collect()
        })
        .unwrap();
    assert_eq!(saved, ["c:in-2"]);
    let states: Vec<(String, String)> = e
        .writer()
        .call(|c| {
            let mut s = c.prepare("SELECT input_hash, state FROM ai_jobs ORDER BY input_hash")?;
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })
        .unwrap();
    assert_eq!(
        states,
        [
            ("in-1".into(), "obsolete".into()),
            ("in-2".into(), "succeeded".into())
        ]
    );
}

// ---------------------------------------------------------------- 停止

#[test]
fn after_the_stop_signal_nothing_is_claimed_and_nothing_is_sent() {
    let e = env("stop-before");
    let stop = StopFlag::new();
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&sent);
    let clue = Fake::new(JobKind::LocationClue, move |ctx, job| {
        ctx.before_send()?;
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.enqueue(&key(JobKind::LocationClue, "c"));
    stop.stop();
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert_eq!(
        (clue.runs(), sent.load(Ordering::SeqCst)),
        (0, 0),
        "claim も送信も 0"
    );
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!((row.state, row.has_token), (JobState::Pending, false));
}

#[test]
fn a_stop_between_claim_and_send_returns_the_job_without_sending_or_counting_a_failure() {
    let e = env("stop-mid");
    let stop = StopFlag::new();
    let (sent, trigger) = (Arc::new(AtomicUsize::new(0)), stop.clone());
    let counter = Arc::clone(&sent);
    let clue = Fake::new(JobKind::LocationClue, move |ctx, job| {
        // claim 済み・準備中に SIGTERM が来た。送る直前の確認で止まる
        trigger.stop();
        ctx.before_send()?;
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[
        key(JobKind::LocationClue, "c"),
        key(JobKind::LocationClue, "d"),
    ]);
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.attempts, row.interruptions),
        (JobState::Deferred, 0, 0)
    );
    assert_eq!(row.code, "shutdown");
    assert!(!row.has_token);
    // 次のジョブは claim されない
    assert_eq!(clue.runs(), 1);
    assert_eq!(e.state(JobKind::LocationClue, "d"), JobState::Pending);
}

#[test]
fn a_valid_response_that_arrives_after_the_stop_signal_is_still_committed() {
    let e = env("stop-valid");
    let stop = StopFlag::new();
    let trigger = stop.clone();
    // 送信は済んでいて（有償）、応答を待つ間に SIGTERM。join の間は token が有効なので確定される
    let clue = Fake::new(JobKind::LocationClue, move |_, job| {
        trigger.stop();
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Succeeded);
    assert_eq!(e.results(), 1);
}

/// 予算の予約の順番待ちのあいだ（writer への依頼を出してから実行されるまで）に停止の合図が来ても、許可は返らない。
/// 時刻は writer の依頼の中で取る（`reserve` の引数）ので、その時計の呼び出しで合図を立てて、依頼の実行中に
/// 来た合図を同期的に再現する。取れた予約は戻さない。
#[test]
fn a_stop_that_arrives_while_the_budget_reservation_waits_denies_the_send_but_keeps_the_reservation()
 {
    let e = env("stop-during-budget");
    let stop = StopFlag::new();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let clock: Clock = {
        let (armed, stop) = (Arc::clone(&armed), stop.clone());
        Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                stop.stop(); // 予約の依頼が実行される瞬間に SIGTERM
            }
            T0
        })
    };
    let (sent, outcome) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let judge = {
        let (sent, outcome, armed) = (Arc::clone(&sent), Arc::clone(&outcome), Arc::clone(&armed));
        Fake::new(JobKind::PhotoJudge, move |ctx, _| {
            armed.store(true, Ordering::SeqCst);
            let result = ctx.before_send().map(|_| ());
            *outcome.lock().unwrap() = Some(result);
            result?;
            sent.fetch_add(1, Ordering::SeqCst);
            Err(RunError::Unavailable)
        })
        .with_keys(&[key(JobKind::PhotoJudge, "j1")])
    };
    let mut deps = e.deps(dyn_handlers(&[Arc::clone(&judge)]), &stop, "run-test");
    deps.clock = clock;
    Core::new(&deps).cycle();
    assert_eq!(
        *outcome.lock().unwrap(),
        Some(Err(RunError::Stopping)),
        "予約が取れたあとでも、停止していれば許可しない"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    let row = e.row(JobKind::PhotoJudge, "j1").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.attempts, row.has_token),
        (JobState::Deferred, "shutdown", 0, false)
    );
    // 取れた予約は戻さない（送ったものとして数える側）
    let used: i64 = e
        .writer()
        .call(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(count),0) FROM ai_request_usage WHERE scope='day'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(used, 1);
}

/// writer への claim の依頼が順番待ちのあいだに停止の合図が来たら、依頼が実行されても pending→running に
/// ならず、token も作られない。呼び出し側の事前確認は通っている（合図は依頼を出す瞬間に立てる）。
#[test]
fn a_stop_that_arrives_while_the_claim_request_waits_for_the_writer_claims_nothing() {
    let e = env("stop-during-claim");
    let stop = StopFlag::new();
    let clue = Fake::ok(JobKind::LocationClue);
    e.enqueue(&key(JobKind::LocationClue, "c"));
    // claim の観測: running への遷移を DB のトリガーに記録する（停止の後に遷移したかを DB 側から確かめる）
    e.writer()
        .call(|c| {
            c.execute_batch(
                "CREATE TABLE claim_observer(n INTEGER);
                 CREATE TRIGGER observe_claim AFTER UPDATE OF state ON ai_jobs
                   WHEN NEW.state='running' BEGIN INSERT INTO claim_observer VALUES (1); END;",
            )
        })
        .unwrap();
    let mut deps = e.deps(dyn_handlers(&[Arc::clone(&clue)]), &stop, "run-test");
    // 最初の writer 呼び出し（reconcile の結果が空なので claim）の依頼を出す瞬間に合図を立てる
    let trigger = stop.clone();
    deps.writer = e.writer().interrupt_next_call(move || trigger.stop());
    assert!(!stop.is_stopped(), "事前確認の時点では合図は無い");
    Core::new(&deps).cycle();
    assert!(stop.is_stopped());
    assert_eq!(clue.runs(), 0, "claim されたジョブは無い");
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.attempts, row.has_token),
        (JobState::Pending, 0, false)
    );
    let transitions: i64 = e
        .writer()
        .call(|c| c.query_row("SELECT COUNT(*) FROM claim_observer", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(transitions, 0, "停止後に pending→running の遷移は 0");
}

#[test]
fn spawn_and_shutdown_run_the_loop_record_the_stop_and_release_nothing_when_idle() {
    let e = env("spawn");
    let stop = StopFlag::with_budget(Duration::from_secs(5));
    let started = startup(&e.config, &e.writer(), &e.clock()).unwrap();
    let clue = Fake::ok(JobKind::LocationClue).with_keys(&[key(JobKind::LocationClue, "c")]);
    let worker = spawn(e.deps(dyn_handlers(&[Arc::clone(&clue)]), &stop, &started.run_id));
    let deadline = Instant::now() + Duration::from_secs(5);
    while e.row(JobKind::LocationClue, "c").map(|r| r.state) != Some(JobState::Succeeded) {
        assert!(Instant::now() < deadline, "ワーカーがジョブを処理しない");
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.stop();
    assert!(worker.shutdown(&e.writer(), &stop, Duration::from_secs(2), &e.clock()));
    // 停止開始が SIGTERM の直後に記録されている（記録は別スレッドなので、少し待つ）
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let marked: Option<i64> = e
            .writer()
            .call(|c| {
                c.query_row("SELECT stop_requested_at FROM ai_worker_runs", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        if marked.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "停止開始が記録されない");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(e.results(), 1);
}

#[test]
fn a_stuck_handler_keeps_the_lock_and_its_late_response_is_not_saved() {
    let mut e = env("stuck");
    let stop = StopFlag::with_budget(Duration::from_secs(5));
    let started = startup(&e.config, &e.writer(), &e.clock()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let clue = Fake::new(JobKind::LocationClue, move |_, job| {
        entered_tx.lock().unwrap().send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv(); // I/O で固まっているつもり
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    let worker = spawn(e.deps(dyn_handlers(&[Arc::clone(&clue)]), &stop, &started.run_id));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Running);

    stop.stop();
    let began = Instant::now();
    // join は 150ms だけ待つ。終わらないので false で戻り、実行中のジョブは deferred/shutdown になる
    assert!(!worker.shutdown(&e.writer(), &stop, Duration::from_millis(150), &e.clock()));
    assert!(began.elapsed() < Duration::from_secs(2));
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.has_token),
        (JobState::Deferred, "shutdown", false)
    );
    assert_eq!(
        (row.attempts, row.interruptions),
        (0, 0),
        "失敗にも中断にも数えない"
    );

    // スレッドが残っている間は、Ingest を捨てても `.ingest.lock` が外れず、別の書き手は始まらない
    let config = Arc::clone(&e.config);
    drop(e.ingest.take());
    assert!(matches!(
        ingest::start_with(&config, &Guard::skip()),
        Err(IngestError::Locked)
    ));
    // 遅れて届いた応答は保存されない（token が失効している）。スレッドが終わればロックが外れる
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let reopened = loop {
        match ingest::start_with(&config, &Guard::skip()) {
            Ok(ingest) => break ingest,
            Err(IngestError::Locked) => {
                assert!(
                    Instant::now() < deadline,
                    "ワーカーが終わってもロックが外れない"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    };
    let (results, state): (i64, String) = reopened
        .writer
        .call(|c| {
            Ok((
                c.query_row("SELECT COUNT(*) FROM ai_test_results", [], |r| r.get(0))?,
                c.query_row("SELECT state FROM ai_jobs WHERE target_key='c'", [], |r| {
                    r.get(0)
                })?,
            ))
        })
        .unwrap();
    assert_eq!((results, state.as_str()), (0, "deferred"));
}

#[test]
fn the_shutdown_never_waits_past_the_common_deadline_even_when_the_writer_is_busy() {
    let e = env("busy-writer");
    let stop = StopFlag::with_budget(Duration::from_millis(400));
    let started = startup(&e.config, &e.writer(), &e.clock()).unwrap();
    let clue = Fake::ok(JobKind::LocationClue);
    let worker = spawn(e.deps(dyn_handlers(&[clue]), &stop, &started.run_id));
    std::thread::sleep(Duration::from_millis(100));
    // writer を長い依頼で塞ぐ（他の書き込みが詰まっている状況）
    let (block_tx, block_rx) = mpsc::channel::<()>();
    let (busy_tx, busy_rx) = mpsc::channel::<()>();
    let blocker = {
        let writer = e.writer();
        std::thread::spawn(move || {
            writer
                .call(move |_| {
                    busy_tx.send(()).unwrap();
                    let _ = block_rx.recv();
                    Ok(())
                })
                .unwrap();
        })
    };
    busy_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // ワーカーが次の周期で、塞がった writer への claim に入って待つ
    std::thread::sleep(Duration::from_millis(100));
    stop.stop();
    let began = Instant::now();
    // join の上限は 10 秒だが、共通期限（400ms）の残りまでしか待たない。停止の記録も最後の戻しも期限つき
    let joined = worker.shutdown(&e.writer(), &stop, Duration::from_secs(10), &e.clock());
    let waited = began.elapsed();
    assert!(waited < Duration::from_millis(1_500), "{waited:?}");
    assert!(
        !joined,
        "writer の順番待ちで塞がったワーカーは期限までに終わらない"
    );
    // 期限を過ぎた依頼は、writer が空いたあとも実行されない（停止開始は書かれない）
    block_tx.send(()).unwrap();
    blocker.join().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let marked: Option<i64> = e
        .writer()
        .call(|c| {
            c.query_row("SELECT stop_requested_at FROM ai_worker_runs", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(marked, None);
}

/// 回帰（T4.1c 修正3）: 停止の記録・最後の戻しの SQL が、別の接続に書き込みを握られて writer スレッドで
/// 待っている。ワーカーも `Ingest` も（`Writer` の複製も）drop されたあとも、その SQL が終わって接続が閉じる
/// まで `.ingest.lock` は外れない。writer スレッドがロックを持つので、依頼を出したスレッドの寿命には依らない。
/// （書き込み中の SQL の開始を同期で捉える版は `ingest` の試験。ここは本番の配線 = worker の `deps.writer` が
/// `Ingest::start` の writer であることを、停止の流れ全体で確かめる。）
#[test]
fn the_lock_survives_the_worker_and_ingest_while_the_stop_sql_waits_for_the_database() {
    let mut e = env("stop-sql-lock");
    let stop = StopFlag::with_budget(Duration::from_millis(400));
    let started = startup(&e.config, &e.writer(), &e.clock()).unwrap();
    let worker = spawn(e.deps(Vec::new(), &stop, &started.run_id));
    let blocker = Connection::open(e.config.db_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

    stop.stop();
    let began = Instant::now();
    let joined = worker.shutdown(&e.writer(), &stop, Duration::from_secs(10), &e.clock());
    assert!(joined, "handler が無いワーカーのスレッドは終わる");
    assert!(began.elapsed() < Duration::from_millis(1_500));

    let lock_path = e.config.data_dir.join(".ingest.lock");
    let lock_is_free = || {
        let f = std::fs::File::options()
            .write(true)
            .open(&lock_path)
            .unwrap();
        f.try_lock().is_ok()
    };
    // 外側（ワーカー・Ingest・Writer の複製）を全て手放す
    drop(e.ingest.take());
    assert!(!lock_is_free(), "SQL が待っているのにロックが外れた");

    blocker.execute_batch("ROLLBACK").unwrap();
    drop(blocker);
    let limit = Instant::now() + Duration::from_secs(20);
    while !lock_is_free() {
        assert!(Instant::now() < limit, "ロックが外れない");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------- 起動の回収

#[test]
fn a_run_with_a_recorded_stop_is_reclaimed_without_counting_an_interruption() {
    let e = env("reclaim-planned");
    let writer = e.writer();
    // 前の起動: claim 済みのまま、停止開始が記録され、最後の戻しは書けなかった
    let first = startup(&e.config, &writer, &e.clock()).unwrap();
    e.enqueue(&key(JobKind::LocationClue, "c"));
    let id = first.run_id.clone();
    let claimed = writer
        .call(move |c| jobs::claim_next(c, T0, &id, "tok", &[JobKind::LocationClue]))
        .unwrap();
    assert!(claimed.is_some());
    let id = first.run_id.clone();
    writer
        .call(move |c| runs::mark_stop_requested(c, &id, T0 + 1))
        .unwrap();

    e.set_clock(T0 + 100);
    let second = startup(&e.config, &writer, &e.clock()).unwrap();
    assert_ne!(first.run_id, second.run_id);
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.has_token),
        (JobState::Deferred, "shutdown", false)
    );
    assert_eq!(
        (row.attempts, row.interruptions),
        (0, 0),
        "予定された停止は中断として数えない"
    );
    // 済んだ起動の行は消え、この起動の行だけが残る
    let rows: Vec<String> = writer
        .call(|c| {
            let mut s = c.prepare("SELECT run_id FROM ai_worker_runs")?;
            s.query_map([], |r| r.get(0))?.collect()
        })
        .unwrap();
    assert_eq!(rows, [second.run_id]);
}

#[test]
fn startup_reclaims_running_jobs_even_when_ai_is_disabled_but_never_starts_the_worker() {
    let e = env("reclaim-ai-off");
    let writer = e.writer();
    // AI が有効だった前の起動が、claim 済みのまま落ちた（SIGKILL・クラッシュ）
    let first = startup(&e.config, &writer, &e.clock()).unwrap();
    e.enqueue(&key(JobKind::LocationClue, "c"));
    let id = first.run_id.clone();
    writer
        .call(move |c| jobs::claim_next(c, T0, &id, "tok", &[JobKind::LocationClue]))
        .unwrap()
        .expect("claim できる");
    // 同じデータで AI を無効に戻して起動する
    let mut vars = HashMap::new();
    vars.insert("GEOFRAME_INGEST".to_string(), "1".to_string());
    vars.insert("GEOFRAME_AI_ENABLED".to_string(), "0".to_string());
    let off = Config::load(
        |k| vars.get(k).cloned(),
        &Overrides {
            listen: None,
            data_dir: Some(e.config.data_dir.clone()),
        },
    )
    .unwrap();
    assert!(!should_start(&off));
    e.set_clock(T0 + 100);
    let second = startup(&off, &writer, &e.clock()).expect("無効でも回収は行う");
    assert_ne!(first.run_id, second.run_id);
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.interruptions, row.has_token),
        (JobState::Pending, 1, false),
        "running が残り続けない（中断として数える）"
    );
}

#[test]
fn three_unplanned_interruptions_make_the_job_failed_and_isolate_it() {
    let e = env("reclaim-sigkill");
    let writer = e.writer();
    e.enqueue(&key(JobKind::LocationClue, "c"));
    for round in 1..=3u32 {
        let started = startup(&e.config, &writer, &e.clock()).unwrap();
        // 停止開始の記録なしで running のまま落ちる（SIGKILL・クラッシュ・電源断）
        let (id, now) = (started.run_id.clone(), e.clock.load(Ordering::SeqCst));
        let claimed = writer
            .call(move |c| jobs::claim_next(c, now, &id, "tok", &[JobKind::LocationClue]))
            .unwrap()
            .expect("claim できる");
        assert_eq!(claimed.key.target_key, "c", "round {round}");
        e.advance(100);
    }
    // 4 回目の起動で 3 回目の中断を数える → failed/interrupted
    let started = startup(&e.config, &writer, &e.clock()).unwrap();
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.interruptions, row.code.as_str()),
        (JobState::Failed, 3, "interrupted")
    );
    // 隔離されたので、同じジョブが全 AI を塞がない。別のジョブは claim される
    e.enqueue(&key(JobKind::LocationClue, "other"));
    let id = started.run_id;
    let next = writer
        .call(move |c| jobs::claim_next(c, T0 + 1_000, &id, "tok", &[JobKind::LocationClue]))
        .unwrap()
        .expect("別のジョブ");
    assert_eq!(next.key.target_key, "other");
}

#[test]
fn startup_releases_only_pauses_that_no_longer_match_a_valid_config_and_never_with_a_bad_one() {
    let e = env("pause-cleanup");
    let writer = e.writer();
    // 今の設定に当たる停止と、接続先が変わる前の設定の停止
    let current = e.config.ai.settings().unwrap().clone();
    let old = super::tests_settings::parse(&variant("192.0.2.10:8100", "192.0.2.99:8100")).unwrap();
    let keys = |s: &super::settings::AiSettings| {
        (
            s.provider_pause_key(TaskKind::ImageEmbedding).unwrap(),
            s.task_pause_key(TaskKind::ImageEmbedding).unwrap(),
        )
    };
    let (cur_provider, _) = keys(&current);
    let (old_provider, old_task) = keys(&old);
    writer
        .call(move |c| {
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &cur_provider,
                PauseReason::Unauthorized,
                T0,
            )?;
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &old_provider,
                PauseReason::Forbidden,
                T0,
            )?;
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &old_task,
                PauseReason::ModelNotFound,
                T0,
            )
        })
        .unwrap();
    assert_eq!(writer.call(|c| pause::list(c)).unwrap().len(), 3);
    startup(&e.config, &writer, &e.clock()).unwrap();
    let left = writer.call(|c| pause::list(c)).unwrap();
    assert_eq!(left.len(), 1, "効かなくなった 2 件だけを整理する");
    assert_eq!(left[0].reason, PauseReason::Unauthorized);

    // 設定が不正・未指定のときは、現在の識別が分からないので何も消さない
    for (tag, json) in [("pause-bad-config", "{"), ("pause-empty-config", "")] {
        let bad = env_with(tag, json, true);
        let w = bad.writer();
        let (old_provider, _) = keys(&old);
        w.call(move |c| {
            pause::pause(
                c,
                TaskKind::ImageEmbedding,
                &old_provider,
                PauseReason::Forbidden,
                T0,
            )
        })
        .unwrap();
        assert!(bad.config.ai.invalid().is_some());
        startup(&bad.config, &w, &bad.clock()).unwrap();
        assert_eq!(w.call(|c| pause::list(c)).unwrap().len(), 1, "{tag}");
    }
    let mut vars = HashMap::new();
    vars.insert("GEOFRAME_INGEST".to_string(), "1".to_string());
    let unset = Arc::new(Config::load(|k| vars.get(k).cloned(), &Overrides::default()).unwrap());
    let w = e.writer();
    startup(&unset, &w, &e.clock()).unwrap();
    assert_eq!(
        w.call(|c| pause::list(c)).unwrap().len(),
        1,
        "未指定でも消さない"
    );
}

// ---------------------------------------------------------------- 送信前の確認・予算

#[test]
fn the_cloud_budget_is_reserved_before_each_send_and_a_rewound_clock_is_a_fixed_reason() {
    let e = env("budget");
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&sends);
    let judge = Fake::new(JobKind::PhotoJudge, move |ctx, job| {
        ctx.before_send()?; // クラウドの用途は送信予算を予約する
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[
        key(JobKind::PhotoJudge, "j1"),
        key(JobKind::PhotoJudge, "j2"),
        key(JobKind::PhotoJudge, "j3"),
    ]);
    // 1 周期の上限は 2（既定）。3 件目は次の周期へ
    e.cycle(&[Arc::clone(&judge)], &StopFlag::new());
    assert_eq!(sends.load(Ordering::SeqCst), 2);
    assert_eq!(e.state(JobKind::PhotoJudge, "j3"), JobState::Pending);
    // 次の周期（同じ窓の中）: 周期の予算が尽きている → 送らず、失敗に数えず後回し
    e.advance(5);
    e.cycle(&[Arc::clone(&judge)], &StopFlag::new());
    assert_eq!(sends.load(Ordering::SeqCst), 2, "予算が尽きたら送らない");
    let row = e.row(JobKind::PhotoJudge, "j3").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.attempts),
        (JobState::Deferred, "budget_exhausted", 0)
    );
    // 窓が明ければ送る
    e.advance(120);
    e.cycle(&[Arc::clone(&judge)], &StopFlag::new());
    assert_eq!(sends.load(Ordering::SeqCst), 3);
    assert_eq!(e.state(JobKind::PhotoJudge, "j3"), JobState::Succeeded);

    // 時計が履歴より前に戻ると、送らず固定の理由 clock_rewound で後回し（予算のリセットはしない）
    judge
        .keys
        .lock()
        .unwrap()
        .push(key(JobKind::PhotoJudge, "j4"));
    e.set_clock(T0 - 500);
    e.cycle(&[Arc::clone(&judge)], &StopFlag::new());
    assert_eq!(sends.load(Ordering::SeqCst), 3);
    let row = e.row(JobKind::PhotoJudge, "j4").unwrap();
    assert_eq!(
        (row.state, row.code.as_str()),
        (JobState::Deferred, "clock_rewound")
    );
    assert_eq!(row.attempts, 0);
}

#[test]
fn before_send_sees_a_pause_recorded_while_the_job_was_running_and_does_not_send() {
    let e = env("before-send");
    let writer = e.writer();
    let pause_key = e
        .config
        .ai
        .settings()
        .unwrap()
        .task_pause_key(TaskKind::LocationClue)
        .unwrap();
    let seen: Arc<Mutex<Vec<RunError>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let clue = Fake::new(JobKind::LocationClue, move |ctx, _| {
        // 準備中に別の経路が用途の停止を記録した。送る直前の確認が拾う
        let key = pause_key.clone();
        writer
            .call(move |c| {
                pause::pause(c, TaskKind::LocationClue, &key, PauseReason::Forbidden, T0)
            })
            .unwrap();
        let error = ctx.before_send().expect_err("停止中は送れない");
        log.lock().unwrap().push(error);
        Err(error)
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(seen.lock().unwrap().as_slice(), [RunError::Paused]);
    let row = e.row(JobKind::LocationClue, "c").unwrap();
    assert_eq!(
        (row.state, row.code.as_str(), row.attempts),
        (JobState::Deferred, "provider_paused", 0)
    );
    assert!(Reservation::Granted.is_granted());
}

// ---------------------------------------------------------------- 待機中の独立・秘匿

#[test]
fn db_reads_and_writes_keep_working_while_a_handler_waits_for_a_slow_provider() {
    let e = env("slow");
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let clue = Fake::new(JobKind::LocationClue, move |_, job| {
        entered_tx.lock().unwrap().send(()).unwrap();
        let _ = release_rx.lock().unwrap().recv();
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
    .with_keys(&[key(JobKind::LocationClue, "c")]);
    let (config, writer, clock) = (Arc::clone(&e.config), e.writer(), e.clock());
    let lock = e.ingest.as_ref().unwrap().lock_hold();
    let handlers = dyn_handlers(&[Arc::clone(&clue)]);
    let cycle = std::thread::spawn(move || {
        let deps = WorkerDeps {
            config,
            writer,
            stop: StopFlag::new(),
            lock,
            handlers,
            clock,
            poll: Duration::from_millis(20),
            run_id: "slow-run".into(),
        };
        Core::new(&deps).cycle();
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // handler は応答待ち。その間に writer への書き込み（位置の手動確定に相当）も、読み取りも、すぐ終わる
    let began = Instant::now();
    e.writer()
        .call(|c| {
            c.execute(
                "UPDATE photos SET lat=3.0, lon=4.0, loc_source='manual' WHERE id=1",
                [],
            )
        })
        .unwrap();
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let (lat, running): (f64, i64) = read
        .query_row(
            "SELECT (SELECT lat FROM photos WHERE id=1), (SELECT COUNT(*) FROM ai_jobs WHERE state='running')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((lat, running), (3.0, 1));
    assert!(began.elapsed() < Duration::from_secs(2));
    release_tx.send(()).unwrap();
    cycle.join().unwrap();
    assert_eq!(e.state(JobKind::LocationClue, "c"), JobState::Succeeded);
}

/// ログの書き込み先（共有バッファ）。
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn logs_never_contain_targets_hashes_tokens_run_ids_models_urls_or_panic_messages() {
    let e = env("secret-logs");
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let secret = |kind| JobKey::new(kind, "SECRET-TARGET-7", "SECRET-HASH-9", "SECRET-PROC-3");
    let clue = Fake::new(JobKind::LocationClue, |_, _| panic!("SECRET-PANIC-MESSAGE"))
        .with_keys(&[secret(JobKind::LocationClue)]);
    let embed = Fake::new(JobKind::ImageEmbedding, |_, _| {
        Err(RunError::TaskPause(PauseReason::Forbidden))
    })
    .with_keys(&[secret(JobKind::ImageEmbedding)]);
    let judge = Fake::new(JobKind::PhotoJudge, |_, job| {
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Changed,
        ))
    })
    .with_keys(&[secret(JobKind::PhotoJudge)]);
    tracing::subscriber::with_default(subscriber, || {
        e.cycle(&[clue, embed, judge], &StopFlag::new());
        let _ = startup(&e.config, &e.writer(), &e.clock());
    });
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!log.is_empty(), "ログは出ている");
    for needle in [
        "SECRET",
        "192.0.2.10",
        "example-embed",
        "example-judge",
        "example-decision",
        "credentials.json",
        "example-project",
        "run-test",
    ] {
        assert!(!log.contains(needle), "{needle} がログに出ている:\n{log}");
    }
}

#[test]
fn debug_output_of_the_job_views_hides_targets_tokens_and_ids() {
    let e = env("debug");
    let claimed = e
        .writer()
        .call(|c| {
            jobs::enqueue(
                c,
                &JobKey::new(JobKind::LocationClue, "SECRET-T", "SECRET-H", "p"),
                T0,
            )?;
            jobs::claim_next(
                c,
                T0,
                "SECRET-RUN",
                "SECRET-TOKEN",
                &[JobKind::LocationClue],
            )
        })
        .unwrap()
        .unwrap();
    let shown = format!(
        "{claimed:?} {:?} {:?}",
        claimed.key,
        RunError::Budget(Reservation::ClockRewound)
    );
    assert!(!shown.contains("SECRET"), "{shown}");
    let started = Started {
        run_id: "SECRET-RUN-ID".into(),
    };
    assert!(!format!("{started:?}").contains("SECRET"));
    assert_eq!(ErrorCode::ClockRewound.as_str(), "clock_rewound");
}
