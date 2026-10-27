//! All external I/O is synthetic loopback. Names, photographs, identifiers and key are fixtures.
use super::super::*;
use crate::ai::jobs;
use crate::ai::{
    cloud::CloudClients,
    ops::{self, RetryTarget},
    provider::LanClient,
    tests_http::{Stub, respond, stub},
    tests_worker::{Env, env_with},
    worker::{self, Core, Handler},
};
use crate::{
    cache::Cache,
    geocode::{
        Endpoints, Geocoder, Outcome,
        stub::{SharedClock, TestClock},
    },
    stop::StopFlag,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

type Hook = Arc<dyn Fn(&str) + Send + Sync>;
struct State {
    available: bool,
    loading: bool,
    cloud_status: u16,
    geocode_status: u16,
    once_401: bool,
    hook: Option<Hook>,
    answer: Value,
}
struct Lab {
    _key: crate::testutil::TempDir,
    env: Env,
    _decision: Stub,
    cloud: Stub,
    geo: Stub,
    state: Arc<Mutex<State>>,
    clients: CloudClients,
    cache: Arc<Cache>,
    geocoder: Arc<Geocoder>,
    clock: Arc<TestClock>,
}
impl Lab {
    fn new() -> Self {
        Self::configured(|_| {})
    }
    fn configured(change: impl FnOnce(&mut Value)) -> Self {
        let key = crate::testutil::TempDir::new("suggest-runtime-key");
        let path = key.path().join("synthetic.json");
        std::fs::write(&path,serde_json::to_vec(&json!({"client_email":"synthetic@example.invalid","private_key":format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",include_str!("../../../../tests/fixtures/jwt-test-pkcs8.b64").trim())})).unwrap()).unwrap();
        let state = Arc::new(Mutex::new(State {
            available: true,
            loading: false,
            cloud_status: 200,
            geocode_status: 200,
            once_401: false,
            hook: None,
            answer: json!({"country_code":"ZZ","place_name":"合成広場","geocode_query":"合成広場, 合成市","precision":"landmark","confidence":"medium","evidence":["合成 evidence canary"],"photos":[{"index":1,"place_name":"合成塔","geocode_query":"合成塔","precision":"landmark","confidence":"high"}]}),
        }));
        let s = state.clone();
        let decision = stub(move |stream, req| {
            invoke(&s, "decision");
            let s = s.lock().unwrap();
            let v = if req.request_line.starts_with("GET /llm/catalog ") {
                json!({"services":[{"id":"decision","available":s.available,"apis":[{"method":"POST","path":"/v1/systemone"},{"method":"GET","path":"/health"}]}]})
            } else if req.request_line.starts_with("GET /llm/services/decision ") {
                json!({"id":"decision","state":"running","available":s.available,"lease_count":0})
            } else if req.request_line.starts_with("GET /llm-decision/health ") {
                json!({"status":if s.loading {"loading"} else {"ready"},"model":"example-decision","modalities":["image"]})
            } else {
                json!({"model":"example-decision","usage":{},"answers":{"clue":{"noul":0.8},"kind":{"choice":"street"}}})
            };
            respond(stream, "200 OK", "", &serde_json::to_vec(&v).unwrap());
        });
        let s = state.clone();
        let cloud = stub(move |stream, req| {
            let token = req.request_line.starts_with("POST /token ");
            invoke(&s, if token { "token" } else { "cloud" });
            let mut s = s.lock().unwrap();
            if token {
                return respond(stream,"200 OK","",br#"{"access_token":"synthetic-token","token_type":"Bearer","expires_in":3600}"#);
            }
            if s.once_401 {
                s.once_401 = false;
                return respond(stream, "401 X", "", b"");
            }
            let v = json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":s.answer.to_string()}]}}]});
            respond(
                stream,
                &format!("{} X", s.cloud_status),
                "",
                &serde_json::to_vec(&v).unwrap(),
            );
        });
        let s = state.clone();
        let geo = stub(move |stream, req| {
            invoke(&s, "geocode");
            let status = s.lock().unwrap().geocode_status;
            // Empty primary day query, city fallback; no GSI request is expected from AI.
            let v = if req.request_line.contains("%2C")
                || req.request_line.contains("address-search")
            {
                json!([])
            } else {
                json!([{"lat":"12.5","lon":"34.5","display_name":"合成地点"}])
            };
            respond(
                stream,
                &format!("{status} X"),
                "",
                &serde_json::to_vec(&v).unwrap(),
            );
        });
        let mut doc: Value = serde_json::from_str(crate::ai::tests_settings::SAMPLE).unwrap();
        doc["local_base_url"] = json!(format!("http://{}", decision.addr));
        doc["tasks"]["location_suggest"]["allow_cloud"] = json!(true);
        doc["tasks"]["location_suggest"]["daily_request_limit"] = json!(100);
        doc["tasks"]["location_suggest"]["credential_path"] = json!(path);
        change(&mut doc);
        let env = env_with("suggest-runtime", &doc.to_string(), true);
        sql(&env, "DELETE FROM photos");
        std::fs::create_dir_all(env.config.data_dir.join("photos")).unwrap();
        let clients = CloudClients::loopback(&env.config.ai, cloud.addr, cloud.addr);
        let clock = TestClock::new(Duration::ZERO);
        let geocoder = Arc::new(
            Geocoder::with_parts(
                Endpoints::loopback(geo.addr).unwrap(),
                Box::new(SharedClock(clock.clone())),
                8,
            )
            .with_user_agent(&env.config.tile_ua),
        );
        Self {
            _key: key,
            env,
            _decision: decision,
            cloud,
            geo,
            state,
            clients,
            cache: Arc::new(Cache::memory_only()),
            geocoder,
            clock,
        }
    }
    fn handlers(&self) -> Vec<Arc<dyn Handler>> {
        worker::production_handlers(
            &self.env.config,
            LanClient::new(&self.env.config.ai),
            Some(&self.clients),
            self.geocoder.clone(),
            self.cache.clone(),
        )
    }
    fn cycle(&self) {
        self.cycle_stop(&StopFlag::new());
    }
    fn cycle_stop(&self, stop: &StopFlag) {
        Core::new(&self.env.deps(self.handlers(), stop, "suggest-runtime")).cycle();
    }
    fn ready(&self) {
        self.cycle();
        assert_eq!(
            n(
                &self.env,
                "SELECT COUNT(*) FROM ai_location_clues WHERE error=''"
            ),
            n(&self.env, "SELECT COUNT(*) FROM photos")
        );
    }
    fn paid(&self) {
        self.ready();
        self.cycle();
        assert_eq!(
            n(
                &self.env,
                "SELECT COUNT(*) FROM ai_location_suggest_results"
            ),
            1
        );
    }
    fn done(&self) {
        self.paid();
        self.cycle();
    }
    fn posts(&self) -> usize {
        self.cloud
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.request_line.starts_with("POST /token "))
            .count()
    }
    fn tokens(&self) -> usize {
        self.cloud
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.request_line.starts_with("POST /token "))
            .count()
    }
    fn hook(&self, h: impl Fn(&str) + Send + Sync + 'static) {
        self.state.lock().unwrap().hook = Some(Arc::new(h));
    }
    fn listing(&self) -> Value {
        let read = crate::db::open_read_only(&self.env.config.db_path()).unwrap();
        let ctx = crate::admin::AdminContext::new(&self.env.config, &self.env.config.data_dir);
        serde_json::to_value(
            crate::admin::photos(&read, &ctx, 1, 100, crate::admin::Filter::All).unwrap(),
        )
        .unwrap()
    }
}
fn invoke(state: &Mutex<State>, event: &str) {
    let hook = state.lock().unwrap().hook.clone();
    if let Some(h) = hook {
        h(event);
    }
}
fn sql(e: &Env, s: &str) {
    let s = s.to_string();
    e.writer().call(move |c| c.execute_batch(&s)).unwrap();
}
fn n(e: &Env, s: &str) -> i64 {
    let s = s.to_string();
    e.writer()
        .call(move |c| c.query_row(&s, [], |r| r.get(0)))
        .unwrap()
}
fn add(e: &Env, id: i64, day: u32) {
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 88)
        .encode(
            &[id as u8; 32 * 24 * 3],
            32,
            24,
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    let rel = format!("photos/synthetic-{id}.jpg");
    std::fs::write(e.config.data_dir.join(&rel), &bytes).unwrap();
    e.writer().call(move|c| {
        c.execute("INSERT INTO photos(id,source,name,display,taken_at,hidden,favorite,rotation) VALUES(?1,'local','synthetic',?2,?3,0,1,0)",params![id,rel,format!("2026-01-{day:02} 03:04")])?;
        c.execute("INSERT INTO photo_quality_features(photo_id,display,content_sha256,analyzed_at,error) VALUES(?1,?2,?3,'synthetic','')",params![id,rel,sha(&bytes)])?;
        Ok(())
    }).unwrap();
}
fn primary(e: &Env) -> Vec<Vec<rusqlite::types::Value>> {
    e.writer()
        .call(|c| {
            let mut q = c.prepare("SELECT * FROM photos ORDER BY id")?;
            let columns = q.column_count();
            q.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())?
                .collect()
        })
        .unwrap()
}
fn save(e: &Env, id: i64) {
    crate::update::set_loc(
        json!({"id":id,"lat":12.5,"lon":34.5}).as_object().unwrap(),
        Some(&e.writer()),
    )
    .unwrap();
}

#[test]
fn factory_paid_commit_separate_geocode_candidates_and_human_save_preserve_primary() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    add(&l.env, 2, 2);
    let before = primary(&l.env);
    l.paid();
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 0);
    assert!(l.geo.requests.lock().unwrap().is_empty());
    l.cycle();
    assert_eq!(primary(&l.env), before);
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE scope='day' AND precision='city'"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE scope='photo' AND precision='landmark'"
        ),
        1
    );
    assert_eq!(l.posts(), 1);
    assert_eq!(l.tokens(), 1);
    assert!(
        l.geo
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.request_line.starts_with("GET /search?")
                && r.header("user-agent") == Some(l.env.config.tile_ua.as_str()))
    );
    assert!(
        l.listing()["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["suggest"]["lat"] == 12.5)
    );
    save(&l.env, 1);
    l.cycle();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photos WHERE id=1 AND loc_source='manual'"
        ),
        1
    );
    assert_eq!(l.posts(), 1, "manual subset reuses paid answer");
}

#[test]
fn geocode_failure_and_restart_never_repeat_cloud_and_retry_resets_only_geocode() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.paid();
    l.state.lock().unwrap().geocode_status = 503;
    for _ in 0..5 {
        l.env.advance(3600);
        l.cycle();
    }
    assert_eq!(l.posts(), 1);
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_geocode' AND state='failed' AND attempts=5"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_runs WHERE status='done'"
        ),
        1
    );
    let report = ops::retry(
        &l.env.writer(),
        &l.env.config.ai,
        &l.handlers(),
        RetryTarget::Kind(JobKind::LocationGeocode),
        &l.env.clock(),
    )
    .unwrap();
    assert_eq!(report.requeued, 1);
    l.state.lock().unwrap().geocode_status = 200;
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 1);
}

#[test]
fn stopped_loading_or_unavailable_decision_after_saved_clue_has_zero_token_and_image_post() {
    for loading in [false, true] {
        let l = Lab::new();
        add(&l.env, 1, 2);
        l.ready();
        {
            let mut s = l.state.lock().unwrap();
            s.loading = loading;
            s.available = loading;
        }
        l.cycle();
        assert_eq!(l.tokens(), 0);
        assert_eq!(l.posts(), 0);
        assert_eq!(
            n(
                &l.env,
                "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_suggest' AND last_error_code='decision_wait' AND attempts=0"
            ),
            1
        );
        assert_eq!(n(&l.env, "SELECT COUNT(*) FROM ai_provider_pauses"), 0);
    }
}

#[test]
fn missing_clue_setting_holds_new_cloud_but_saved_answer_geocodes_without_client_or_key() {
    let l = Lab::configured(|d| {
        d["tasks"].as_object_mut().unwrap().remove("location_clue");
    });
    add(&l.env, 1, 2);
    l.cycle();
    assert_eq!(l.posts(), 0);
    assert_eq!(l.tokens(), 0);
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.paid();
    std::fs::remove_file(l._key.path().join("synthetic.json")).unwrap();
    let handlers = worker::production_handlers(
        &l.env.config,
        None,
        None,
        l.geocoder.clone(),
        l.cache.clone(),
    );
    Core::new(&l.env.deps(handlers, &StopFlag::new(), "restart-no-cloud")).cycle();
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 1);
    assert_eq!(l.posts(), 1);
}

#[test]
fn token_wait_revalidates_database_hidden_save_source_and_decision_without_cloud_pause() {
    for action in ["rotation", "hidden", "save", "source", "decision"] {
        let l = Lab::new();
        add(&l.env, 1, 2);
        l.ready();
        let writer = l.env.writer();
        let path = l.env.config.data_dir.join("photos/synthetic-1.jpg");
        let state = l.state.clone();
        l.hook(move |event| {
            if event == "token" {
                match action {
                    "rotation" => {
                        writer
                            .call(|c| c.execute_batch("UPDATE photos SET rotation=90 WHERE id=1"))
                            .unwrap();
                    }
                    "hidden" => {
                        writer
                            .call(|c| c.execute_batch("UPDATE photos SET hidden=1 WHERE id=1"))
                            .unwrap();
                    }
                    "save" => {
                        writer
                            .call(|c| {
                                c.execute_batch(
                                    "UPDATE photos SET lat=1,lon=2,loc_source='manual' WHERE id=1",
                                )
                            })
                            .unwrap();
                    }
                    "source" => {
                        std::fs::write(&path, b"synthetic replacement").unwrap();
                    }
                    _ => {
                        state.lock().unwrap().available = false;
                    }
                }
            }
        });
        l.cycle();
        assert_eq!(l.tokens(), 1);
        assert_eq!(l.posts(), 0, "{action}");
        assert_eq!(n(&l.env, "SELECT COUNT(*) FROM ai_provider_pauses"), 0);
    }
}

#[test]
fn refresh_401_revalidates_input_and_consumes_reserved_posts_without_refund() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    l.state.lock().unwrap().once_401 = true;
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = count.clone();
    let writer = l.env.writer();
    l.hook(move |event| {
        if event == "token" && c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            writer
                .call(|c| c.execute_batch("UPDATE photos SET hidden=1 WHERE id=1"))
                .unwrap();
        }
    });
    l.cycle();
    assert_eq!(l.tokens(), 2);
    assert_eq!(l.posts(), 1);
    assert_eq!(
        n(
            &l.env,
            "SELECT count FROM ai_request_usage WHERE task='location_suggest' AND scope='day'"
        ),
        2
    );
    assert_eq!(
        n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_results"),
        0
    );
}

#[test]
fn reply_and_geocode_wait_changes_reject_candidate_projection_and_paid_resend() {
    for phase in ["cloud", "geocode"] {
        let l = Lab::new();
        add(&l.env, 1, 2);
        l.ready();
        if phase == "geocode" {
            l.cycle();
        }
        let writer = l.env.writer();
        l.hook(move |event| {
            if event == phase {
                writer
                    .call(|c| c.execute_batch("UPDATE photos SET rotation=90 WHERE id=1"))
                    .unwrap();
            }
        });
        l.cycle();
        assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 0);
        assert_eq!(l.posts(), 1);
        assert_eq!(
            n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_results"),
            i64::from(phase == "geocode")
        );
    }
}

#[test]
fn new_candidate_visibility_invalidates_on_db_or_file_change_and_preserves_legacy_rows() {
    for change in [
        "rotation", "hidden", "time", "source", "addition", "delete", "legacy",
    ] {
        let l = Lab::new();
        add(&l.env, 1, 2);
        l.done();
        match change {
            "rotation" => sql(&l.env, "UPDATE photos SET rotation=90 WHERE id=1"),
            "hidden" => sql(&l.env, "UPDATE photos SET hidden=1 WHERE id=1"),
            "time" => sql(
                &l.env,
                "UPDATE photos SET taken_at='2026-02-01 03:04' WHERE id=1",
            ),
            "source" => std::fs::write(
                l.env.config.data_dir.join("photos/synthetic-1.jpg"),
                b"synthetic changed",
            )
            .unwrap(),
            "addition" => add(&l.env, 2, 2),
            "delete" => {
                std::fs::remove_file(l.env.config.data_dir.join("photos/synthetic-1.jpg")).unwrap();
            }
            _ => sql(
                &l.env,
                "UPDATE photo_loc_suggestions SET place_name='legacy canary' WHERE photo_id=1",
            ),
        }
        let listing = l.listing();
        let p = &listing["items"][0];
        assert_eq!(!p["suggest"].is_null(), change == "legacy", "{change}");
        assert_eq!(l.posts(), 1);
    }
}

#[test]
fn shared_geocoder_cache_ua_rate_and_typed_empty_failure_stop_have_negative_controls() {
    let l = Lab::new();
    let stop = StopFlag::new();
    assert!(
        matches!(l.geocoder.nominatim_checked(&l.cache,"合成甲","zz",&stop),Outcome::Success(v) if v.len()==1)
    );
    assert_eq!(
        l.geocoder.nominatim_search(&l.cache, "合成甲", "zz").len(),
        1
    );
    assert_eq!(
        l.geo.requests.lock().unwrap().len(),
        1,
        "legacy GET path hits same AI cache"
    );
    assert!(matches!(
        l.geocoder
            .nominatim_checked(&l.cache, "合成乙", "zz", &stop),
        Outcome::Success(_)
    ));
    assert_eq!(l.clock.take_sleeps(), vec![Duration::from_secs(1)]);
    l.state.lock().unwrap().geocode_status = 503;
    assert!(matches!(
        l.geocoder
            .nominatim_checked(&l.cache, "合成失敗", "", &stop),
        Outcome::Transient
    ));
    l.state.lock().unwrap().geocode_status = 200;
    assert!(
        matches!(l.geocoder.nominatim_checked(&l.cache,"合成空, 合成空","",&stop),Outcome::Success(v) if v.is_empty())
    );
    let before = l.geo.requests.lock().unwrap().len();
    stop.stop();
    assert!(matches!(
        l.geocoder
            .nominatim_checked(&l.cache, "合成未送信", "", &stop),
        Outcome::Stopping
    ));
    assert_eq!(l.geo.requests.lock().unwrap().len(), before);
}

#[test]
fn failed_cleanup_never_bypasses_legacy_attempt_limit_through_generic_enqueue() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    let run = run_id_for("example-suggest", &[1]);
    l.env.writer().call(move |c| {
        c.execute("INSERT INTO photo_loc_runs(run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,attempts,answer_json,error,analyzed_at) VALUES(?1,'2026-01-02','example-suggest','loc-v2','[1]','[1]','error',5,'','bad_response','synthetic')", [run])?;
        c.execute_batch("CREATE TRIGGER synthetic_cleanup_failure BEFORE DELETE ON ai_location_suggest_holds BEGIN SELECT RAISE(ABORT,'synthetic cleanup failure'); END;")
    }).unwrap();
    assert_eq!(
        n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_holds"),
        1
    );

    l.cycle();
    assert_eq!(l.posts(), 0);
    assert_eq!(l.tokens(), 0);
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_suggest'"
        ),
        0
    );
    assert_eq!(n(&l.env, "SELECT attempts FROM photo_loc_runs"), 5);
    assert_eq!(
        n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_holds"),
        1
    );

    sql(&l.env, "DROP TRIGGER synthetic_cleanup_failure");
    l.cycle();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_suggest' AND state='failed' AND attempts=5 AND terminal=1"
        ),
        1
    );
    assert_eq!(l.posts(), 0);
    assert_eq!(l.tokens(), 0);
}

#[test]
fn legacy_unknown_paid_answer_holds_with_fixed_status_counts_and_zero_cloud() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    let run = run_id_for("example-suggest", &[1]);
    l.env.writer().call(move|c|{c.execute("INSERT INTO photo_loc_runs(run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,attempts,answer_json,error,analyzed_at) VALUES(?1,'2026-01-02','example-suggest','loc-v2','[1]','[1]','done',2,'secret legacy canary','secret error canary','synthetic')",[run])?;Ok(())}).unwrap();
    let logs = crate::testutil::capture::logs_of(|| l.cycle());
    assert_eq!(l.posts(), 0);
    assert_eq!(l.tokens(), 0);
    let read = crate::db::open_read_only(&l.env.config.db_path()).unwrap();
    assert_eq!(hold_counts(&read).unwrap(), vec![("legacy_unverified", 1)]);
    let ctx = crate::admin::AdminContext::new(&l.env.config, &l.env.config.data_dir);
    let status = crate::admin::status(&read, &ctx).unwrap();
    assert_eq!(status["ai_location_holds"]["legacy_unverified"], 1);
    assert!(!logs.contains("canary"));
    assert!(!logs.contains("2026-01-02"));
}

#[test]
fn disabled_cloud_zero_limit_and_missing_ai_leave_factory_no_cloud_side_effects() {
    for mode in [0, 1, 2] {
        let l = Lab::configured(|d| match mode {
            0 => d["tasks"]["location_suggest"]["allow_cloud"] = json!(false),
            1 => d["tasks"]["location_suggest"]["daily_request_limit"] = json!(0),
            _ => {
                d["tasks"]
                    .as_object_mut()
                    .unwrap()
                    .remove("location_suggest");
            }
        });
        add(&l.env, 1, 2);
        l.cycle();
        l.cycle();
        assert_eq!(l.posts(), 0);
        assert_eq!(l.tokens(), 0);
        assert_eq!(
            n(
                &l.env,
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='ai_location_candidates'"
            ),
            0
        );
    }
}

#[test]
fn cloud_401_success_reserves_two_and_stop_after_paid_response_still_commits() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    l.state.lock().unwrap().once_401 = true;
    l.cycle();
    assert_eq!(l.posts(), 2);
    assert_eq!(l.tokens(), 2);
    assert_eq!(
        n(
            &l.env,
            "SELECT count FROM ai_request_usage WHERE task='location_suggest' AND scope='day'"
        ),
        2
    );
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    let stop = StopFlag::new();
    let s = stop.clone();
    l.hook(move |event| {
        if event == "cloud" {
            s.stop();
        }
    });
    l.cycle_stop(&stop);
    assert_eq!(
        n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_results"),
        1
    );
    assert!(l.geo.requests.lock().unwrap().is_empty());
}

#[test]
fn pending_hidden_failed_rank_six_and_saved_photo_number_are_runtime_contracts() {
    let l = Lab::new();
    for id in 1..=9 {
        add(&l.env, id, 2);
    }
    l.ready();
    sql(&l.env,"UPDATE photos SET hidden=1 WHERE id=9;
        DELETE FROM ai_location_clues WHERE photo_id=9; DELETE FROM photo_loc_clues WHERE photo_id=9;
        UPDATE ai_jobs SET state='pending' WHERE kind='location_clue' AND target_key='9';
        UPDATE ai_location_clues SET kind='landmark',clue=0.9 WHERE photo_id=3;
        UPDATE photo_loc_clues SET kind='landmark',clue=0.9 WHERE photo_id=3;
        UPDATE ai_location_clues SET kind='landmark',clue=0.1 WHERE photo_id=8;
        UPDATE photo_loc_clues SET kind='landmark',clue=0.1 WHERE photo_id=8;
        UPDATE ai_location_clues SET kind='',clue=NULL,error='input_invalid' WHERE photo_id=7;
        UPDATE photo_loc_clues SET kind='',clue=NULL,error='input_invalid' WHERE photo_id=7;
        UPDATE ai_jobs SET state='failed',terminal=1 WHERE kind='location_clue' AND target_key='7';");
    l.cycle();
    assert_eq!(l.posts(), 1);
    let sent: String = l
        .env
        .writer()
        .call(|c| {
            c.query_row(
                "SELECT sent_ids_json FROM photo_loc_runs WHERE status='done'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<i64>>(&sent).unwrap(),
        vec![3, 8, 1, 2, 4, 5]
    );
    // Change ranking after the paid receipt: saved index1 still identifies photo3.
    sql(
        &l.env,
        "UPDATE ai_location_clues SET clue=0.95 WHERE photo_id=1; UPDATE photo_loc_clues SET clue=0.95 WHERE photo_id=1;",
    );
    l.cycle();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE photo_id=3 AND scope='photo'"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE photo_id=7 AND scope='day'"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE photo_id=9"
        ),
        0
    );
    assert_eq!(l.posts(), 1);
}

#[test]
fn pending_day_does_not_block_another_day_and_all_failed_never_sends() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    add(&l.env, 2, 3);
    l.ready();
    sql(&l.env,"DELETE FROM ai_location_clues WHERE photo_id=1; DELETE FROM photo_loc_clues WHERE photo_id=1;
        UPDATE ai_jobs SET state='deferred',next_attempt_at=9999999999 WHERE kind='location_clue' AND target_key='1';");
    l.cycle();
    assert_eq!(l.posts(), 1);
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_runs WHERE day='2026-01-03'"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT count FROM ai_location_suggest_holds WHERE reason='pending'"
        ),
        1
    );
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.ready();
    sql(
        &l.env,
        "UPDATE ai_location_clues SET kind='',clue=NULL,error='input_invalid';
        UPDATE photo_loc_clues SET kind='',clue=NULL,error='input_invalid';
        UPDATE ai_jobs SET state='failed',terminal=1 WHERE kind='location_clue';",
    );
    l.cycle();
    assert_eq!(l.posts(), 0);
    assert_eq!(l.tokens(), 0);
    assert_eq!(
        n(
            &l.env,
            "SELECT count FROM ai_location_suggest_holds WHERE reason='no_images'"
        ),
        1
    );
}

#[test]
fn oversized_day_is_terminal_before_token_and_other_days_continue_without_changing_selection() {
    let l = Lab::new();
    let rgb = crate::ai::image_input::fixtures::noise_rgb(1536, 1024);
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 98)
        .encode(&rgb, 1536, 1024, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    for id in 1..=6 {
        add(&l.env, id, 2);
        std::fs::write(
            l.env
                .config
                .data_dir
                .join(format!("photos/synthetic-{id}.jpg")),
            &bytes,
        )
        .unwrap();
        let hash = sha(&bytes);
        l.env
            .writer()
            .call(move |c| {
                c.execute(
                    "UPDATE photo_quality_features SET content_sha256=?1 WHERE photo_id=?2",
                    params![hash, id],
                )?;
                Ok(())
            })
            .unwrap();
    }
    add(&l.env, 7, 3);
    l.ready();
    for _ in 0..3 {
        l.cycle();
    }
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_suggest' AND state='failed' AND last_error_code='input_too_large'"
        ),
        1
    );
    assert_eq!(l.posts(), 1);
    assert_eq!(l.tokens(), 1);
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_runs WHERE status='done' AND day='2026-01-03'"
        ),
        1
    );
}

#[test]
fn factory_to_actual_candidate_http_api_and_manual_save() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.done();
    let before = primary(&l.env);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let state = crate::server::AppState::new(
        l.env.config.clone(),
        crate::db::Catalog::open(&l.env.config.db_path())
            .unwrap()
            .with_writer(l.env.writer()),
    )
    .with_cache(l.cache.clone())
    .with_geocoder(l.geocoder.clone());
    let (addr, server) = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let listener =
            crate::limits::LimitedListener::new(listener, crate::limits::ConnLimits::default());
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::server::router(state))
                .await
                .unwrap();
        });
        (addr, server)
    });
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().proxy(None).build());
    let mut response = agent
        .get(format!("http://{addr}/api/admin/photos"))
        .header("Host", "localhost")
        .call()
        .unwrap();
    let value: Value = serde_json::from_slice(
        &crate::net::read_body_limited(response.body_mut(), 1 << 20).unwrap(),
    )
    .unwrap();
    let suggest = &value["items"][0]["suggest"];
    assert_eq!(suggest["scope"], "photo");
    assert_eq!(suggest["precision"], "landmark");
    assert_eq!(suggest["lat"], 12.5);
    assert_eq!(primary(&l.env), before);
    let body =
        serde_json::to_vec(&json!({"id":1,"lat":suggest["lat"],"lon":suggest["lon"]})).unwrap();
    let mut response = agent
        .post(format!("http://{addr}/api/admin/setloc"))
        .header("Host", "localhost")
        .header("Origin", "http://localhost")
        .header("Content-Type", "application/json")
        .send(&body)
        .unwrap();
    let value: Value = serde_json::from_slice(
        &crate::net::read_body_limited(response.body_mut(), 1 << 20).unwrap(),
    )
    .unwrap();
    assert_eq!(value["loc_source"], "manual");
    assert_eq!(l.posts(), 1);
    server.abort();
    runtime.shutdown_timeout(Duration::from_secs(1));
}

#[test]
fn geocode_old_token_and_transaction_rollback_preserve_paid_answer_and_primary() {
    for old_token in [false, true] {
        let l = Lab::new();
        add(&l.env, 1, 2);
        l.paid();
        let before = primary(&l.env);
        let handler = l
            .handlers()
            .into_iter()
            .find(|h| h.kind() == JobKind::LocationGeocode)
            .unwrap();
        let read = crate::db::open_read_only(&l.env.config.db_path()).unwrap();
        let key = handler
            .reconcile(&read, crate::ai::tests_worker::T0)
            .unwrap()
            .remove(0);
        let claimed = l
            .env
            .writer()
            .call(move |c| {
                jobs::enqueue(c, &key, crate::ai::tests_worker::T0)?;
                Ok(jobs::claim_next(
                    c,
                    crate::ai::tests_worker::T0,
                    "test-run",
                    "old-token",
                    &[JobKind::LocationGeocode],
                )?
                .unwrap())
            })
            .unwrap();
        let (writer, clock, stop) = (l.env.writer(), l.env.clock(), StopFlag::new());
        let ctx = worker::RunCtx::for_test(
            &read,
            &l.env.config.ai,
            crate::ai::settings::TaskKind::LocationSuggest,
            &stop,
            &writer,
            &clock,
        );
        let persist = handler.run(&ctx, &claimed).unwrap();
        if old_token {
            sql(
                &l.env,
                "UPDATE ai_jobs SET claim_token='new-token' WHERE kind='location_geocode'",
            );
        } else {
            sql(
                &l.env,
                "CREATE TRIGGER synthetic_reject BEFORE INSERT ON photo_loc_suggestions BEGIN SELECT RAISE(ABORT,'private canary SQL'); END;",
            );
        }
        let done =
            writer.call(move |c| jobs::complete(c, &claimed, crate::ai::tests_worker::T0, persist));
        if old_token {
            assert_eq!(done.unwrap(), jobs::Completion::Rejected);
        } else {
            assert!(done.is_err());
        }
        assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 0);
        assert_eq!(n(&l.env, "SELECT COUNT(*) FROM ai_location_candidates"), 0);
        assert_eq!(
            n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_results"),
            1
        );
        assert_eq!(l.posts(), 1);
        assert_eq!(primary(&l.env), before);
    }
}

#[test]
fn succeeded_geocode_repairs_missing_projection_without_http_and_keeps_python_rewrite() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.done();
    let geocode_calls = l.geo.requests.lock().unwrap().len();
    sql(&l.env, "DELETE FROM photo_loc_suggestions");
    l.cycle();
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 1);
    assert_eq!(l.geo.requests.lock().unwrap().len(), geocode_calls);
    assert_eq!(l.posts(), 1);
    sql(
        &l.env,
        "UPDATE photo_loc_suggestions SET place_name='synthetic Python rewrite'",
    );
    l.cycle();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE place_name='synthetic Python rewrite'"
        ),
        1
    );
}

#[test]
fn photo_no_geocode_hit_falls_back_to_day_precision_and_empty_success_is_final() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.state.lock().unwrap().answer["photos"][0]["geocode_query"] = json!("");
    l.done();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE scope='photo' AND precision='city' AND lat=12.5"
        ),
        1
    );

    let l = Lab::new();
    add(&l.env, 1, 2);
    l.paid();
    let empty = stub(|stream, _| respond(stream, "200 OK", "", b"[]"));
    let geocoder = Arc::new(Geocoder::with_parts(
        Endpoints::loopback(empty.addr).unwrap(),
        Box::new(SharedClock(l.clock.clone())),
        8,
    ));
    let handlers =
        worker::production_handlers(&l.env.config, None, None, geocoder.clone(), l.cache.clone());
    Core::new(&l.env.deps(handlers, &StopFlag::new(), "empty-result")).cycle();
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE lat IS NULL AND lon IS NULL"
        ),
        1
    );
    assert_eq!(
        n(
            &l.env,
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_geocode' AND state='succeeded'"
        ),
        1
    );
    let calls = empty.requests.lock().unwrap().len();
    assert!(
        matches!(geocoder.nominatim_checked(&l.cache,"合成塔","ZZ",&StopFlag::new()),Outcome::Success(v) if v.is_empty())
    );
    assert_eq!(empty.requests.lock().unwrap().len(), calls);
    assert_eq!(l.posts(), 1);
}

#[test]
fn saved_answer_with_missing_clue_setting_and_auth_still_geocodes_without_cloud() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.paid();
    let doc = std::fs::read(l.env.config.data_dir.parent().unwrap().join("ai.json")).unwrap();
    let mut doc: Value = serde_json::from_slice(&doc).unwrap();
    doc["tasks"]
        .as_object_mut()
        .unwrap()
        .remove("location_clue");
    let path = l._key.path().join("no-clue.json");
    std::fs::write(&path, doc.to_string()).unwrap();
    std::fs::remove_file(l._key.path().join("synthetic.json")).unwrap();
    let config = Arc::new(
        crate::config::Config::load(
            |key| match key {
                "GEOFRAME_AI_CONFIG" => Some(path.to_string_lossy().into()),
                "GEOFRAME_INGEST" => Some("1".into()),
                _ => None,
            },
            &crate::config::Overrides {
                data_dir: Some(l.env.config.data_dir.clone()),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let handlers =
        worker::production_handlers(&config, None, None, l.geocoder.clone(), l.cache.clone());
    let mut deps = l
        .env
        .deps(handlers, &StopFlag::new(), "without-clue-setting");
    deps.config = config;
    Core::new(&deps).cycle();
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 1);
    assert_eq!(l.posts(), 1);
    assert_eq!(l.tokens(), 1);
}

#[test]
fn geocode_final_writer_cas_rejects_changes_after_http_and_before_commit() {
    let l = Lab::new();
    add(&l.env, 1, 2);
    l.paid();
    let handler = l
        .handlers()
        .into_iter()
        .find(|h| h.kind() == JobKind::LocationGeocode)
        .unwrap();
    let read = crate::db::open_read_only(&l.env.config.db_path()).unwrap();
    let key = handler
        .reconcile(&read, crate::ai::tests_worker::T0)
        .unwrap()
        .remove(0);
    let claimed = l
        .env
        .writer()
        .call(move |c| {
            jobs::enqueue(c, &key, crate::ai::tests_worker::T0)?;
            Ok(jobs::claim_next(
                c,
                crate::ai::tests_worker::T0,
                "test-run",
                "test-token",
                &[JobKind::LocationGeocode],
            )?
            .unwrap())
        })
        .unwrap();
    let (writer, clock, stop) = (l.env.writer(), l.env.clock(), StopFlag::new());
    let ctx = worker::RunCtx::for_test(
        &read,
        &l.env.config.ai,
        crate::ai::settings::TaskKind::LocationSuggest,
        &stop,
        &writer,
        &clock,
    );
    let persist = handler.run(&ctx, &claimed).unwrap();
    sql(&l.env, "UPDATE photos SET rotation=90 WHERE id=1");
    assert_eq!(
        writer
            .call(move |c| jobs::complete(c, &claimed, crate::ai::tests_worker::T0, persist))
            .unwrap(),
        jobs::Completion::Obsolete
    );
    assert_eq!(n(&l.env, "SELECT COUNT(*) FROM photo_loc_suggestions"), 0);
    assert_eq!(
        n(&l.env, "SELECT COUNT(*) FROM ai_location_suggest_results"),
        1
    );
    assert_eq!(l.posts(), 1);
}

#[test]
fn python_eleven_column_paid_run_reusable_and_apply_run_match_rust_subset_projection() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let l = Lab::new();
    add(&l.env, 1, 2);
    add(&l.env, 2, 2);
    l.done();
    save(&l.env, 1);
    l.cycle();
    let read = crate::db::open_read_only(&l.env.config.db_path()).unwrap();
    let run = read
        .query_row(
            "SELECT * FROM photo_loc_runs WHERE status='done'",
            [],
            |r| {
                let mut map = serde_json::Map::new();
                for (i, name) in r.as_ref().column_names().iter().enumerate() {
                    map.insert(
                        (*name).into(),
                        if *name == "attempts" {
                            json!(r.get::<_, i64>(i)?)
                        } else {
                            json!(r.get::<_, String>(i)?)
                        },
                    );
                }
                Ok(Value::Object(map))
            },
        )
        .unwrap();
    let row:String=read.query_row("SELECT json_array(photo_id,run_id,scope,place_name,day_place_name,geocode_query,precision,confidence,evidence_json,lat,lon,geocode_title,model,analyzed_at) FROM photo_loc_suggestions WHERE photo_id=2",[],|r|r.get(0)).unwrap();
    let input = json!({"model":"example-suggest","run":run,"subset":[2],"hits":{"合成市":[{"title":"合成地点","lat":12.5,"lon":34.5,"source":"osm"}]}});
    let mut child = Command::new("python3")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/gen_location_suggest_fixture.py"),
        )
        .arg("--apply-roundtrip")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "synthetic Python apply roundtrip");
    let python: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        python,
        json!([serde_json::from_str::<Value>(&row).unwrap()])
    );
    assert_eq!(l.posts(), 1);
}
