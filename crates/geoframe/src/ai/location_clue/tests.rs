//! 合成 DB・画像と loopback Discovery / Decision だけ。配布用 handler factory を通す。
use super::*;
use crate::ai::jobs::{Completion, Failure, JobState};
use crate::ai::ops::{self, RetryTarget};
use crate::ai::settings::TaskKind;
use crate::ai::tests_http::{Captured, Stub, respond, stub};
use crate::ai::tests_settings::variant;
use crate::ai::tests_worker::{Env, T0, env_with};
use crate::ai::worker::{self, Core};
use crate::stop::StopFlag;
use base64::Engine;
use serde_json::json;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

type Hook = Arc<dyn Fn(&str) + Send + Sync>;
struct State {
    available: bool,
    lease: u32,
    status: u16,
    reply: serde_json::Value,
    hook: Option<Hook>,
}
struct Lab {
    stub: Stub,
    state: Arc<Mutex<State>>,
}
impl Lab {
    fn new() -> Self {
        let state = Arc::new(Mutex::new(State {
            available: true,
            lease: 0,
            status: 200,
            reply: json!({"model":"example-decision","usage":{},"answers":{"clue":{"noul":0.8},"kind":{"choice":"street"}}}),
            hook: None,
        }));
        let shared = state.clone();
        let stub = stub(move |stream, req| {
            let hook = shared.lock().unwrap().hook.clone();
            if let Some(h) = hook {
                h(&req.request_line);
            }
            let s = shared.lock().unwrap();
            let value = if req.request_line.starts_with("GET /llm/catalog ") {
                json!({"services":[{"id":"decision","available":s.available,"apis":[{"method":"POST","path":"/v1/systemone"},{"method":"GET","path":"/health"}]}]})
            } else if req.request_line.starts_with("GET /llm/services/decision ") {
                json!({"id":"decision","state":"running","available":s.available,"lease_count":s.lease})
            } else if req.request_line.starts_with("GET /llm-decision/health ") {
                json!({"status":"ready","model":"example-decision","modalities":["image"]})
            } else {
                return respond(
                    stream,
                    &format!("{} X", s.status),
                    "Retry-After: 60\r\n",
                    &serde_json::to_vec(&s.reply).unwrap(),
                );
            };
            respond(stream, "200 OK", "", &serde_json::to_vec(&value).unwrap());
        });
        Self { stub, state }
    }
    fn env(&self, tag: &str) -> Env {
        let settings = variant(
            "http://192.0.2.10:8100",
            &format!("http://{}", self.stub.addr),
        );
        let e = env_with(tag, &settings, true);
        e.writer()
            .call(|c| c.execute_batch("DELETE FROM photos"))
            .unwrap();
        fs::create_dir_all(e.config.data_dir.join("derivatives")).unwrap();
        fs::create_dir_all(e.config.data_dir.join("photos")).unwrap();
        e
    }
    fn posts(&self) -> Vec<Captured> {
        self.stub
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.request_line.starts_with("POST "))
            .cloned()
            .collect()
    }
}
fn handlers(e: &Env) -> Vec<Arc<dyn Handler>> {
    worker::production_handlers_for_test(&e.config, LanClient::new(&e.config.ai))
}
fn cycle(e: &Env) {
    cycle_stop(e, &StopFlag::new());
}
fn cycle_stop(e: &Env, stop: &StopFlag) {
    Core::new(&e.deps(handlers(e), stop, "clue-test")).cycle();
}
fn data(e: &Env) -> DataDir {
    DataDir::open(&e.config.data_dir).unwrap()
}
fn jpeg() -> Vec<u8> {
    let mut v = Vec::new();
    jpeg_encoder::Encoder::new(&mut v, 90)
        .encode(&[110; 32 * 24 * 3], 32, 24, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    v
}
fn add(e: &Env, id: i64, rel: &str, bytes: Option<&[u8]>, quality: u8) {
    if let Some(bytes) = bytes {
        fs::write(e.config.data_dir.join(rel), bytes).unwrap();
    }
    let (rel, hash) = (rel.to_string(), bytes.map(sha));
    e.writer().call(move |c| {
        c.execute("INSERT INTO photos(id,source,name,display,drive_id,md5,mtime,taken_at,hidden,favorite,rotation)
             VALUES(?1,'local','synthetic',?2,?3,NULL,NULL,'2026-01-02 03:04',1,1,0)",params![id,rel,format!("fake{id}")])?;
        if quality>0 {
            c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error,analyzed_at)
                VALUES(?1,?2,NULL,NULL,?3,?4,'2026-01-01T00:00:00Z')",
                params![id,rel,if quality==1 {hash} else {None},if quality==2 {"synthetic failure with private data"} else {""}])?;
        }
        Ok(())
    }).unwrap();
}
fn sql(e: &Env, sql: &str) {
    let sql = sql.to_string();
    e.writer().call(move |c| c.execute_batch(&sql)).unwrap();
}
fn n(e: &Env, sql: &str) -> i64 {
    let sql = sql.to_string();
    e.writer()
        .call(move |c| c.query_row(&sql, [], |r| r.get(0)))
        .unwrap()
}
fn targets(e: &Env) -> Vec<Target> {
    load_targets(
        &crate::db::open_read_only(&e.config.db_path()).unwrap(),
        &data(e),
        "example-decision",
    )
    .unwrap()
}
fn dump_primary(e: &Env) -> Vec<Vec<Value>> {
    e.writer()
        .call(|c| {
            let mut stmt = c.prepare("SELECT * FROM photos ORDER BY id")?;
            let count = stmt.column_count();
            stmt.query_map([], |r| (0..count).map(|i| r.get(i)).collect())?
                .collect()
        })
        .unwrap()
}

#[test]
fn production_path_succeeds_atomically_reuses_after_restart_and_preserves_primary_data() {
    let lab = Lab::new();
    let e = lab.env("clue-production");
    let original = jpeg();
    add(&e, 1, "derivatives/test.jpg", Some(&original), 1);
    let before = dump_primary(&e);
    // HTTP 応答待ちでも別 writer 操作・画像 lock が使える。リクエストは画像ロックを保持しない。
    let writer = e.writer();
    lab.state.lock().unwrap().hook = Some(Arc::new(move |line| {
        if line.starts_with("POST ") {
            let _lock = imaging::lock_derivatives();
            writer
                .call(|c| {
                    c.execute(
                        "INSERT OR REPLACE INTO settings VALUES('synthetic','1')",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
    }));
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        1
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_location_clues WHERE source_sha256 IS NOT NULL AND error=''"
        ),
        1
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE clue=0.8 AND kind='street' AND error=''"
        ),
        1
    );
    assert_eq!(dump_primary(&e), before);
    assert_eq!(
        fs::read(e.config.data_dir.join("derivatives/test.jpg")).unwrap(),
        original
    );
    assert_eq!(
        targets(&e)[0].readiness,
        Readiness::Ready(Clue::new(0.8, "street").unwrap())
    );
    // Core と production handler/client を毎回作り直しても再送しない。
    cycle(&e);
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    // Python が互換行を変更したら、その値を信用せず Rust の検証済み snapshot を復元する。
    sql(
        &e,
        "UPDATE photo_loc_clues SET clue=0.99,kind='landmark',error='foreign text'",
    );
    assert_eq!(targets(&e)[0].readiness, Readiness::Pending);
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE clue=0.8 AND kind='street' AND error=''"
        ),
        1
    );
    let req: serde_json::Value = serde_json::from_slice(&lab.posts()[0].body).unwrap();
    let sent = base64::engine::general_purpose::STANDARD
        .decode(
            req["images"][0]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/jpeg;base64,")
                .unwrap(),
        )
        .unwrap();
    let rgb = imaging::prepare_decision_rgb(&original, false).unwrap();
    let expected = PreparedImage::encode_jpeg_rgb(
        &rgb.pixels,
        rgb.width as u16,
        rgb.height as u16,
        88,
        ImageProfile::Decision,
    )
    .unwrap();
    assert_eq!(req["images"][0], expected.data_url());
    assert_ne!(sent, original);
    assert!(!req.to_string().contains("test.jpg"));
    assert!(!format!("{:?}", targets(&e)[0].key).contains(&targets(&e)[0].key.input_hash));
    assert_eq!(
        format!("{:?}", Clue::new(0.8, "street").unwrap()),
        "Clue { .. }"
    );
}

#[test]
fn targeting_matches_python_and_fallback_raw_png_and_hidden_are_supported() {
    use image::ImageEncoder;
    let lab = Lab::new();
    let e = lab.env("clue-targets");
    add(&e, 1, "photos/raw.jpg", Some(&jpeg()), 1);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[100; 8 * 12], 8, 12, image::ExtendedColorType::L8)
        .unwrap();
    add(&e, 2, "photos/raw.png", Some(&png), 1);
    add(&e, 3, "", None, 0);
    fs::write(e.config.data_dir.join("derivatives/fake3.jpg"), jpeg()).unwrap();
    add(&e, 4, "photos/saved.jpg", Some(&jpeg()), 1);
    sql(&e, "UPDATE photos SET lat=1,lon=2 WHERE id=4");
    add(&e, 5, "photos/bad-time.jpg", Some(&jpeg()), 1);
    sql(
        &e,
        "UPDATE photos SET taken_at='2026-02-30 00:00' WHERE id=5",
    );
    add(&e, 6, "photos/no-time.jpg", Some(&jpeg()), 1);
    sql(&e, "UPDATE photos SET taken_at=NULL WHERE id=6");
    add(&e, 7, "photos/earlier.jpg", Some(&jpeg()), 1);
    sql(
        &e,
        "UPDATE photos SET taken_at='2025-01-01 00:00' WHERE id=7",
    );
    assert_eq!(
        targets(&e).iter().map(|t| t.photo_id).collect::<Vec<_>>(),
        [7, 1, 2, 3]
    );
    cycle(&e);
    assert_eq!(lab.posts().len(), 4);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM photo_loc_clues WHERE error=''"),
        4
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE photo_id=3 AND display='derivatives/fake3.jpg'"
        ),
        1
    );
    // fallback は品質の行が無くても、実際に読んだ bytes の SHA を再検査する。
    cycle(&e);
    assert_eq!(lab.posts().len(), 4);
    assert!(matches!(targets(&e)[3].readiness, Readiness::Ready(_)));
}

#[test]
fn question_fixture_and_thresholds_match_the_existing_python_contract() {
    let image = crate::ai::image_input::fixtures::image(4, 4);
    let request = DecisionRequest::new("m", DECISION_STATE, questions(), vec![image]).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&request.to_json().unwrap()).unwrap();
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/location-clue-python.json"
    ))
    .unwrap();
    assert_eq!(body["state"], fixture["state"]);
    assert_eq!(body["questions"], fixture["questions"]);
    assert!(!Clue::new(0.1999, "street").unwrap().passes());
    assert!(Clue::new(0.2, "street").unwrap().passes());
    assert!(Clue::new(0.0, "landmark").unwrap().passes());
    for score in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
        assert!(Clue::new(score, "landmark").is_none());
    }
    assert!(Clue::new(0.9, "unknown").is_none());
}

#[test]
fn legacy_results_are_not_trusted_and_versions_and_sqlite_types_affect_the_key() {
    let lab = Lab::new();
    let e = lab.env("clue-legacy");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    sql(
        &e,
        "INSERT INTO photo_loc_clues VALUES(1,'photos/a.jpg',NULL,NULL,0.8,'street','example-decision','2026-01-01T00:00:00Z','')",
    );
    assert_eq!(targets(&e)[0].readiness, Readiness::Pending);
    let initial = targets(&e).remove(0).key;
    assert_ne!(initial.processor_version, processor_version("other"));
    assert_ne!(
        processor_version("example-decision"),
        sha(format!("example-decision\0changed-questions\0{PREPROCESS_VERSION}").as_bytes())
    );
    sql(&e, "UPDATE photos SET md5='' WHERE id=1");
    assert_ne!(targets(&e)[0].key.input_hash, initial.input_hash);
    sql(&e, "UPDATE photos SET md5=NULL WHERE id=1");
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    // URL / 別用途の許可だけの変更は結果版に入らず、成功の再送 0。
    let key = targets(&e)[0].key.clone();
    assert_eq!(key.processor_version, initial.processor_version);
    assert_eq!(initial.input_hash, key.input_hash);
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    assert_eq!(
        load_targets(&read, &data(&e), "other-model").unwrap()[0].readiness,
        Readiness::Pending
    );
    // 以前の質問版の成功は保持しつつ、現在の質問版を新しい job で判定する。
    sql(
        &e,
        "UPDATE ai_jobs SET processor_version='prior-questions'; UPDATE ai_location_clues SET processor_version='prior-questions'",
    );
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        2
    );
}

#[test]
fn missing_corrupt_quality_failed_and_pending_have_distinct_readiness() {
    let lab = Lab::new();
    let e = lab.env("clue-quality");
    add(&e, 1, "photos/missing.jpg", None, 0);
    add(&e, 2, "photos/corrupt.jpg", Some(b"broken image"), 0);
    add(&e, 3, "photos/quality-failed.jpg", Some(&jpeg()), 2);
    add(&e, 4, "photos/quality-pending.jpg", Some(&jpeg()), 0);
    add(&e, 5, "", None, 0);
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='failed'"),
        4
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='input_invalid' AND clue IS NULL AND kind=''"
        ),
        4
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE photo_id=5 AND display=''"
        ),
        1
    );
    assert_eq!(
        targets(&e)
            .iter()
            .map(|t| t.readiness.clone())
            .collect::<Vec<_>>(),
        [
            Readiness::Failed,
            Readiness::Failed,
            Readiness::Failed,
            Readiness::Pending,
            Readiness::Failed
        ]
    );
    assert_eq!(
        n(&e, "SELECT attempts FROM ai_jobs WHERE target_key='4'"),
        0
    );
    let hash = sha(&jpeg());
    e.writer().call(move |c| {
        c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error,analyzed_at)
            VALUES(4,'photos/quality-pending.jpg',NULL,NULL,?1,'','2026-01-01T00:00:00Z')",[hash])?;
        Ok(())
    }).unwrap();
    e.advance(30);
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    assert!(matches!(targets(&e)[3].readiness, Readiness::Ready(_)));
}

#[test]
fn unavailable_lease_429_and_paused_never_project_errors() {
    for mode in ["unavailable", "lease", "429", "pause"] {
        let lab = Lab::new();
        let e = lab.env(&format!("clue-{mode}"));
        add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
        {
            let mut s = lab.state.lock().unwrap();
            match mode {
                "unavailable" => s.available = false,
                "lease" => s.lease = 1,
                "429" => s.status = 429,
                _ => s.status = 403,
            }
        }
        cycle(&e);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0, "{mode}");
        assert_eq!(n(&e, "SELECT attempts FROM ai_jobs"), 0, "{mode}");
        assert_eq!(targets(&e)[0].readiness, Readiness::Pending);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='deferred'"),
            1
        );
    }
}

#[test]
fn unanswerable_and_bad_responses_fail_at_five_then_retry_invalidates_only_the_failed_projection() {
    let lab = Lab::new();
    let e = lab.env("clue-five");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    lab.state.lock().unwrap().reply["answers"]["clue"] = json!({"error":"secret provider answer"});
    for attempt in 1..=5 {
        cycle(&e);
        assert_eq!(n(&e, "SELECT attempts FROM ai_jobs"), attempt);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM photo_loc_clues"),
            i64::from(attempt == 5)
        );
        e.advance(1000);
    }
    assert_eq!(targets(&e)[0].readiness, Readiness::Failed);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='bad_response'"
        ),
        1
    );
    let before = lab.posts().len();
    let report = ops::retry(
        &e.writer(),
        &e.config.ai,
        &handlers(&e),
        RetryTarget::Kind(JobKind::LocationClue),
        &e.clock(),
    )
    .unwrap();
    assert_eq!(report.requeued, 1);
    assert_eq!(lab.posts().len(), before);
    assert_eq!(targets(&e)[0].readiness, Readiness::Pending);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0);
    lab.state.lock().unwrap().reply["answers"]["clue"] = json!({"noul":0.8});
    cycle(&e);
    assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
    assert_eq!(n(&e, "SELECT attempts FROM ai_jobs"), 0);
    // succeeded は retry が消さない。
    assert_eq!(
        ops::retry(
            &e.writer(),
            &e.config.ai,
            &handlers(&e),
            RetryTarget::All,
            &e.clock()
        )
        .unwrap()
        .requeued,
        0
    );
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM photo_loc_clues WHERE error=''"),
        1
    );
}

#[test]
fn permanent_projection_and_failure_history_survive_a_b_a_and_unknown_codes_are_redacted() {
    let lab = Lab::new();
    let e = lab.env("clue-terminal");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    lab.state.lock().unwrap().status = 422;
    cycle(&e);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND terminal=1 AND attempts=0"
        ),
        1
    );
    let a = targets(&e)[0].key.clone();
    sql(&e, "UPDATE photos SET rotation=90");
    cycle(&e);
    sql(&e, "UPDATE photos SET rotation=0");
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    assert_eq!(targets(&e)[0].key, a);
    assert_eq!(targets(&e)[0].readiness, Readiness::Failed);
    sql(
        &e,
        "DELETE FROM photo_loc_clues; DELETE FROM ai_location_clues; UPDATE ai_jobs SET last_error_code='private unknown error' WHERE state='failed'",
    );
    cycle(&e);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='other' AND clue IS NULL AND kind=''"
        ),
        1
    );
    assert_eq!(lab.posts().len(), 2);
}

#[test]
fn startup_interruption_limit_is_repaired_idempotently_and_old_failure_tokens_cannot_write() {
    let lab = Lab::new();
    let e = lab.env("clue-interrupted");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let key = targets(&e)[0].key.clone();
    let k = key.clone();
    let claimed = e
        .writer()
        .call(move |c| {
            jobs::enqueue(c, &k, T0)?;
            let job =
                jobs::claim_next(c, T0, "old-run", "old-token", &[JobKind::LocationClue])?.unwrap();
            c.execute("UPDATE ai_jobs SET interruption_count=2", [])?;
            jobs::reclaim_interrupted(c, "new-run", T0 + 1)?;
            Ok(job)
        })
        .unwrap();
    assert_eq!(n(&e, "SELECT interruption_count FROM ai_jobs"), 3);
    cycle(&e);
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='interrupted'"
        ),
        1
    );
    let handler = handlers(&e).remove(0);
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let projection = handler.projection(&read, &key, T0).unwrap().unwrap();
    e.writer()
        .call(move |c| {
            assert_eq!(
                jobs::record_failure_with(
                    c,
                    &claimed,
                    T0 + 2,
                    ErrorCode::InputInvalid,
                    Failure::Permanent,
                    projection
                )?,
                None
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='interrupted'"
        ),
        1
    );
}

#[test]
fn discovery_and_reply_waits_recheck_updates_rotation_display_sha_delete_and_manual_save() {
    for phase in ["health", "POST"] {
        for mutation in [
            "UPDATE photos SET mtime=1",
            "UPDATE photos SET rotation=90",
            "UPDATE photos SET display='photos/new.jpg'",
            "UPDATE photo_quality_features SET content_sha256=printf('%064d',1)",
            "DELETE FROM photos",
            "UPDATE photos SET lat=1,lon=2,loc_source='manual'",
        ] {
            let lab = Lab::new();
            let e = lab.env("clue-races");
            add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
            let writer = e.writer();
            let mutation_copy = mutation.to_string();
            lab.state.lock().unwrap().hook = Some(Arc::new(move |line| {
                if (phase == "health" && line.starts_with("GET /llm-decision/health "))
                    || (phase == "POST" && line.starts_with("POST "))
                {
                    let mutation = mutation_copy.clone();
                    writer.call(move |c| c.execute_batch(&mutation)).unwrap();
                }
            }));
            cycle(&e);
            assert_eq!(
                n(&e, "SELECT COUNT(*) FROM photo_loc_clues"),
                0,
                "{phase:?} {mutation}"
            );
            assert_eq!(
                n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='obsolete'"),
                1,
                "{mutation}"
            );
            assert_eq!(
                lab.posts().len(),
                usize::from(phase == "POST"),
                "{mutation}"
            );
        }
    }
}

#[test]
fn stale_persist_and_stale_failure_projection_do_not_overwrite_manual_gps_or_new_generation() {
    for failure in [false, true] {
        let lab = Lab::new();
        let e = lab.env("clue-cas");
        add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
        let key = targets(&e)[0].key.clone();
        let k = key.clone();
        let claimed = e
            .writer()
            .call(move |c| {
                jobs::enqueue(c, &k, T0)?;
                Ok(jobs::claim_next(c, T0, "r", "t", &[JobKind::LocationClue])?.unwrap())
            })
            .unwrap();
        let h = handlers(&e).remove(0);
        let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
        let p = if failure {
            h.projection(&read, &key, T0).unwrap().unwrap()
        } else {
            let input = Input::load(&read, 1).unwrap().unwrap();
            persist(
                input,
                key,
                Stored {
                    source_sha: Some(sha(&jpeg())),
                    clue: Some(0.8),
                    kind: "street".into(),
                    model: "example-decision".into(),
                    at: waits::utc_iso(T0),
                    error: String::new(),
                },
                "example-decision".into(),
            )
        };
        sql(&e, "UPDATE photos SET lat=1,lon=2,loc_source='manual'");
        e.writer()
            .call(move |c| {
                if failure {
                    assert_eq!(
                        jobs::record_failure_with(
                            c,
                            &claimed,
                            T0,
                            ErrorCode::InputInvalid,
                            Failure::Permanent,
                            p
                        )?,
                        Some(JobState::Obsolete)
                    );
                } else {
                    assert_eq!(jobs::complete(c, &claimed, T0, p)?, Completion::Obsolete);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0);
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM photos WHERE lat=1 AND loc_source='manual'"
            ),
            1
        );
    }
}

#[test]
fn source_replacement_during_discovery_or_response_is_never_sent_or_saved() {
    for phase in ["health", "POST"] {
        let lab = Lab::new();
        let e = lab.env("clue-bytes-race");
        add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
        let path = e.config.data_dir.join("photos/a.jpg");
        let phase_copy = phase.to_string();
        lab.state.lock().unwrap().hook = Some(Arc::new(move |line| {
            if (phase_copy == "health" && line.starts_with("GET /llm-decision/health "))
                || (phase_copy == "POST" && line.starts_with("POST "))
            {
                fs::write(&path, b"replaced while waiting").unwrap();
            }
        }));
        cycle(&e);
        assert_eq!(lab.posts().len(), usize::from(phase == "POST"));
        assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0);
    }
}

#[test]
fn stop_before_send_sends_zero_and_an_inflight_valid_response_can_commit() {
    for inflight in [false, true] {
        let lab = Lab::new();
        let e = lab.env("clue-stop");
        add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
        let stop = StopFlag::new();
        let s = stop.clone();
        lab.state.lock().unwrap().hook = Some(Arc::new(move |line| {
            if (inflight && line.starts_with("POST "))
                || (!inflight && line.starts_with("GET /llm-decision/health "))
            {
                s.stop();
            }
        }));
        cycle_stop(&e, &stop);
        assert_eq!(lab.posts().len(), usize::from(inflight));
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM photo_loc_clues"),
            i64::from(inflight)
        );
    }
}

#[test]
fn unsafe_paths_links_fifo_and_oversized_inputs_fail_without_posting() {
    use std::os::unix::fs::symlink;
    let lab = Lab::new();
    let e = lab.env("clue-unsafe");
    add(&e, 1, "../outside.jpg", None, 0);
    add(&e, 2, "/etc/passwd", None, 0);
    fs::write(e.config.data_dir.join("photos/real.jpg"), jpeg()).unwrap();
    symlink(
        e.config.data_dir.join("photos/real.jpg"),
        e.config.data_dir.join("photos/link.jpg"),
    )
    .unwrap();
    add(&e, 3, "photos/link.jpg", None, 0);
    symlink(
        e.config.data_dir.parent().unwrap(),
        e.config.data_dir.join("photos/outside-dir"),
    )
    .unwrap();
    add(&e, 4, "photos/outside-dir/ai.json", None, 0);
    symlink(
        e.config.data_dir.join("photos"),
        e.config.data_dir.join("photos/inside-dir"),
    )
    .unwrap();
    add(&e, 5, "photos/inside-dir/real.jpg", None, 0);
    let fifo = e.config.data_dir.join("photos/fifo.jpg");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    add(&e, 6, "photos/fifo.jpg", None, 0);
    let big = e.config.data_dir.join("photos/big.jpg");
    fs::File::create(big)
        .unwrap()
        .set_len(imaging::MAX_INPUT_BYTES + 1)
        .unwrap();
    add(&e, 7, "photos/big.jpg", None, 0);
    add(&e, 8, "photos", None, 0);
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='input_invalid'"
        ),
        7
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM photo_loc_clues WHERE error='input_too_large'"
        ),
        1
    );
}

#[test]
fn failure_projection_and_job_transition_roll_back_together_on_sql_error() {
    let lab = Lab::new();
    let e = lab.env("clue-atomic-failure");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let key = targets(&e)[0].key.clone();
    let k = key.clone();
    let claimed = e
        .writer()
        .call(move |c| {
            jobs::enqueue(c, &k, T0)?;
            Ok(jobs::claim_next(c, T0, "r", "t", &[JobKind::LocationClue])?.unwrap())
        })
        .unwrap();
    sql(
        &e,
        "CREATE TRIGGER reject_clue BEFORE INSERT ON photo_loc_clues BEGIN SELECT RAISE(ABORT,'private SQL value'); END",
    );
    let h = handlers(&e).remove(0);
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let project = h.projection(&read, &key, T0).unwrap().unwrap();
    e.writer()
        .call(move |c| {
            assert!(
                jobs::record_failure_with(
                    c,
                    &claimed,
                    T0,
                    ErrorCode::InputInvalid,
                    Failure::Permanent,
                    project
                )
                .is_err()
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='running' AND attempts=0"
        ),
        1
    );
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_location_clues"), 0);
}

#[test]
fn image_bytes_are_read_before_releasing_lock_and_no_sql_transaction_is_held_during_http() {
    let lab = Lab::new();
    let e = lab.env("clue-boundaries");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let (tx, rx) = mpsc::channel();
    let rx = Arc::new(Mutex::new(rx));
    let sender = tx.clone();
    let writer = e.writer();
    lab.state.lock().unwrap().hook = Some(Arc::new(move |line| {
        if line.starts_with("POST ") {
            let _image = imaging::lock_derivatives();
            writer
                .call(|c| {
                    assert!(c.is_autocommit());
                    c.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
                })
                .unwrap();
            sender.send(()).unwrap();
        }
    }));
    cycle(&e);
    rx.lock()
        .unwrap()
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(lab.posts().len(), 1);
}

#[test]
fn polling_discovers_later_photos_invalidates_ineligible_jobs_and_reuses_a_b_a_success() {
    let lab = Lab::new();
    let e = lab.env("clue-reconcile");
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    cycle(&e);
    let a = targets(&e)[0].key.clone();
    sql(&e, "UPDATE photos SET rotation=90");
    cycle(&e);
    sql(&e, "UPDATE photos SET rotation=0");
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    assert_eq!(targets(&e)[0].key, a);
    assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
    // 新しい pending を作ってから削除/SAVE/無効時刻になった対象は周期で obsolete にする。
    for mutation in [
        "DELETE FROM photos WHERE id=2",
        "UPDATE photos SET lat=1 WHERE id=2",
        "UPDATE photos SET taken_at='bad' WHERE id=2",
    ] {
        sql(
            &e,
            "DELETE FROM photos WHERE id=2; DELETE FROM photo_quality_features WHERE photo_id=2",
        );
        add(&e, 2, "photos/b.jpg", Some(&jpeg()), 1);
        sql(
            &e,
            "UPDATE photos SET mtime=COALESCE((SELECT MAX(id) FROM ai_jobs),0) WHERE id=2",
        );
        let key = targets(&e).remove(1).key;
        e.writer()
            .call(move |c| {
                jobs::enqueue(c, &key, T0)?;
                Ok(())
            })
            .unwrap();
        sql(&e, mutation);
        cycle(&e);
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM ai_jobs WHERE target_key='2' AND state IN ('pending','running','deferred','failed')"
            ),
            0
        );
    }
}

#[test]
fn config_scope_and_cloud_permissions_do_not_change_success_identity_and_disabled_factory_is_empty()
{
    let lab = Lab::new();
    let e = lab.env("clue-config-identity");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    cycle(&e);
    let old = targets(&e)[0].key.clone();
    let base = fs::read_to_string(e.config.data_dir.parent().unwrap().join("ai.json")).unwrap();
    let settings_path = e.config.data_dir.parent().unwrap().join("changed.json");
    let changed = base
        .replace(&format!("http://{}", lab.stub.addr), "http://127.0.0.1:9")
        .replace("\"allow_cloud\": true", "\"allow_cloud\": false");
    fs::write(&settings_path, changed).unwrap();
    let mut config = (*e.config).clone();
    config.ai = crate::ai::settings::AiConfig::load(&|k| {
        (k == "GEOFRAME_AI_CONFIG").then(|| settings_path.to_str().unwrap().to_string())
    });
    let h = worker::production_handlers_for_test(&config, LanClient::new(&config.ai));
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    assert_eq!(h[0].reconcile(&read, T0).unwrap()[0], old);
    assert!(matches!(
        load_targets(&read, &data(&e), "example-decision").unwrap()[0].readiness,
        Readiness::Ready(_)
    ));
    let mut deps = e.deps(h, &StopFlag::new(), "changed-config");
    deps.config = Arc::new(config.clone());
    Core::new(&deps).cycle();
    assert_eq!(lab.posts().len(), 1);
    config.ingest = false;
    assert!(worker::production_handlers_for_test(&config, LanClient::new(&config.ai)).is_empty());
    for json in ["{}", r#"{"version":1,"tasks":{}}"#] {
        let disabled = env_with("clue-no-schema", json, true);
        assert!(handlers(&disabled).is_empty());
        assert_eq!(
            n(
                &disabled,
                "SELECT COUNT(*) FROM sqlite_master WHERE name='ai_location_clues'"
            ),
            0
        );
        let conn = crate::db::open_read_only(&disabled.config.db_path()).unwrap();
        assert!(
            !crate::ingest::plan(&disabled.config, &conn)
                .unwrap()
                .schema
                .missing_objects
                .iter()
                .any(|n| n == "ai_location_clues")
        );
    }
}

#[test]
fn ingest_zero_old_database_remains_unchanged_and_no_metadata_is_required() {
    let dir = crate::testutil::TempDir::new("clue-old-db");
    let (path, _keep) = crate::testutil::synthetic_catalog(dir.path(), 2);
    let before = fs::read(&path).unwrap();
    let read = crate::db::open_read_only(&path).unwrap();
    crate::db::check_schema(&read).unwrap();
    load_targets(
        &read,
        &DataDir::open(dir.path()).unwrap(),
        "example-decision",
    )
    .unwrap();
    assert!(
        crate::ai::pending(&read)
            .unwrap()
            .contains(&"ai_location_clues".to_string())
    );
    assert!(!jobs::object_exists(&read, "table", "ai_location_clues").unwrap());
    drop(read);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn bytes_are_read_under_derivative_lock_and_replacement_before_read_is_detected() {
    let lab = Lab::new();
    let e = lab.env("clue-read-lock");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    std::thread::scope(|scope| {
        let lock = imaging::lock_derivatives();
        let runner = scope.spawn(|| cycle(&e));
        assert!(imaging::lock_waiters::wait_until_waiting(
            runner.thread().id(),
            || false,
            Duration::from_secs(3)
        ));
        fs::write(
            e.config.data_dir.join("photos/a.jpg"),
            b"replaced before acquiring image lock",
        )
        .unwrap();
        drop(lock);
        runner.join().unwrap();
    });
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0);
}

#[test]
fn fallback_bytes_define_new_generations_and_failed_history_survives_missing_present_missing() {
    let lab = Lab::new();
    let e = lab.env("clue-fallback-generations");
    add(&e, 1, "", None, 0);
    cycle(&e);
    let missing = targets(&e)[0].key.clone();
    assert_eq!(targets(&e)[0].readiness, Readiness::Failed);
    let path = e.config.data_dir.join("derivatives/fake1.jpg");
    fs::write(&path, jpeg()).unwrap();
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    let ready = targets(&e)[0].key.clone();
    assert_ne!(ready, missing);
    assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
    fs::write(&path, imaging::rotate_jpeg(&jpeg(), 90).unwrap().jpeg).unwrap();
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    fs::write(&path, jpeg()).unwrap();
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    assert_eq!(targets(&e)[0].key, ready);
    assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
    fs::remove_file(path).unwrap();
    cycle(&e);
    assert_eq!(targets(&e)[0].key, missing);
    assert_eq!(targets(&e)[0].readiness, Readiness::Failed);
    assert_eq!(lab.posts().len(), 2);
}

#[test]
fn changed_failure_projection_rolls_back_its_writes_and_success_projection_is_atomic_on_error() {
    for failure in [true, false] {
        let lab = Lab::new();
        let e = lab.env("clue-savepoint");
        add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
        let key = targets(&e)[0].key.clone();
        e.writer()
            .call(move |c| {
                jobs::enqueue(c, &key, T0)?;
                let claimed = jobs::claim_next(c, T0, "r", "t", &[JobKind::LocationClue])?.unwrap();
                if failure {
                    let outcome = jobs::record_failure_with(
                        c,
                        &claimed,
                        T0,
                        ErrorCode::InputInvalid,
                        Failure::Permanent,
                        |tx| {
                            tx.execute("INSERT INTO ai_test_results VALUES('synthetic')", [])?;
                            Ok(InputCheck::Changed)
                        },
                    )?;
                    assert_eq!(outcome, Some(JobState::Obsolete));
                } else {
                    let outcome = jobs::complete(c, &claimed, T0, |tx| {
                        tx.execute("INSERT INTO ai_test_results VALUES('synthetic')", [])?;
                        Err(rusqlite::Error::InvalidQuery)
                    });
                    assert!(outcome.is_err());
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_test_results"), 0);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
            0
        );
    }
}

#[derive(Clone)]
struct Logs(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[test]
fn production_sql_and_image_failures_log_only_fixed_reasons_and_never_private_values() {
    let lab = Lab::new();
    let e = lab.env("clue-private-logs");
    add(
        &e,
        1,
        "photos/PRIVATE-source-GPS-name.jpg",
        Some(&jpeg()),
        1,
    );
    sql(
        &e,
        "CREATE TRIGGER reject_clue BEFORE INSERT ON photo_loc_clues BEGIN SELECT RAISE(ABORT,'PRIVATE-SQL-value-35.0-139.0-location'); END",
    );
    let logs = Logs(Arc::new(Mutex::new(Vec::new())));
    let captured = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || captured.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || cycle(&e));
    let log = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("結果を確定できません"));
    for private in [
        "PRIVATE",
        "example-decision",
        "写真を1枚見て",
        "この写真には",
        "0.8",
        &sha(&jpeg()),
        e.config.data_dir.to_str().unwrap(),
    ] {
        assert!(!log.contains(private), "{log}");
    }
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_location_clues"), 0);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_loc_clues"), 0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='pending' AND last_error_code='internal' AND attempts=1"
        ),
        1
    );
}

#[test]
fn retry_prepares_fallback_sha_outside_writer_and_handles_unprojected_interruptions_and_changed_bytes()
 {
    for unprojected in [true, false] {
        let lab = Lab::new();
        let e = lab.env("clue-fallback-retry");
        add(&e, 1, "", None, 0);
        let path = e.config.data_dir.join("derivatives/fake1.jpg");
        fs::write(&path, jpeg()).unwrap();
        if unprojected {
            let key = targets(&e)[0].key.clone();
            e.writer()
                .call(move |c| {
                    jobs::enqueue(c, &key, T0)?;
                    jobs::claim_next(c, T0, "old", "t", &[JobKind::LocationClue])?.unwrap();
                    c.execute("UPDATE ai_jobs SET interruption_count=2", [])?;
                    jobs::reclaim_interrupted(c, "new", T0 + 1)?;
                    Ok(())
                })
                .unwrap();
        } else {
            lab.state.lock().unwrap().status = 422;
            cycle(&e);
            fs::write(&path, imaging::rotate_jpeg(&jpeg(), 90).unwrap().jpeg).unwrap();
        }
        let before = lab.posts().len();
        let report = ops::retry(
            &e.writer(),
            &e.config.ai,
            &handlers(&e),
            RetryTarget::All,
            &e.clock(),
        )
        .unwrap();
        assert_eq!(lab.posts().len(), before);
        if unprojected {
            assert_eq!(report.requeued, 1);
        } else {
            assert_eq!(report.obsolete, 1);
        }
        lab.state.lock().unwrap().status = 200;
        cycle(&e);
        assert_eq!(lab.posts().len(), before + 1);
        assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
    }
}

// ---- STATUS の `db.loc.clue_pending`（R9）----

fn clue_pending_with(e: &Env, config: &crate::config::Config) -> i64 {
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let ctx = crate::admin::AdminContext::new(config, &config.data_dir);
    let status = crate::admin::status(&read, &ctx).unwrap();
    status["db"]["loc"]["clue_pending"].as_i64().unwrap()
}
fn clue_pending(e: &Env) -> i64 {
    clue_pending_with(e, &e.config)
}
/// 旧 Python が今の画像に対して判定済みとした行（`add` の写真は md5・mtime が NULL）。
fn legacy_clue(e: &Env, id: i64, rel: &str) {
    let rel = rel.to_string();
    e.writer()
        .call(move |c| {
            c.execute(
                "INSERT INTO photo_loc_clues(photo_id,display,source_md5,source_mtime,clue,kind,model,analyzed_at,error)
                 VALUES(?1,?2,NULL,NULL,0.9,'street','python-model','2026-01-01T00:00:00Z','')",
                params![id, rel],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn status_clue_pending_counts_rust_waits_even_when_python_rows_are_fresh() {
    let lab = Lab::new();
    let e = lab.env("clue-status");
    // 対象の無い DB
    assert_eq!(clue_pending(&e), 0);
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    add(&e, 2, "photos/bad.jpg", Some(b"broken image"), 0);
    add(&e, 3, "", None, 0);
    fs::write(e.config.data_dir.join("derivatives/fake3.jpg"), jpeg()).unwrap();
    for (id, rel) in [
        (1, "photos/a.jpg"),
        (2, "photos/bad.jpg"),
        (3, "derivatives/fake3.jpg"),
    ] {
        legacy_clue(&e, id, rel);
    }
    // 旧 Python の行だけで数えると 0（Rust を判定者としない設定）。Rust の保存結果は 1 件も無い
    let mut python_only = (*e.config).clone();
    python_only.ingest = false;
    assert_eq!(clue_pending_with(&e, &python_only), 0);
    assert_eq!(clue_pending(&e), 3);
    // 判定を保存したら 0（恒久失敗は判定済み）
    cycle(&e);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_location_clues"), 3);
    assert_eq!(clue_pending(&e), 0);
    // 品質の解析待ちの写真は待ちに数える。解析が済んで保存されれば 0 に戻る
    add(&e, 4, "photos/pending.jpg", Some(&jpeg()), 0);
    legacy_clue(&e, 4, "photos/pending.jpg");
    cycle(&e);
    assert_eq!(clue_pending(&e), 1);
    let hash = sha(&jpeg());
    e.writer().call(move |c| {
        c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error,analyzed_at)
            VALUES(4,'photos/pending.jpg',NULL,NULL,?1,'','2026-01-01T00:00:00Z')",[hash])?;
        Ok(())
    }).unwrap();
    assert_eq!(clue_pending(&e), 1);
    e.advance(30);
    cycle(&e);
    assert_eq!(clue_pending(&e), 0);
}

#[test]
fn status_clue_pending_follows_the_decision_model_and_falls_back_to_python_rows_when_rust_does_not_judge()
 {
    let lab = Lab::new();
    let e = lab.env("clue-status-model");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    add(&e, 2, "photos/b.jpg", Some(&jpeg()), 1);
    legacy_clue(&e, 1, "photos/a.jpg");
    cycle(&e);
    assert_eq!(clue_pending(&e), 0);
    // Decision のモデルが変わると、保存済みの結果は今の世代ではなく、全件が待ちに戻る
    let base = fs::read_to_string(e.config.data_dir.parent().unwrap().join("ai.json")).unwrap();
    let path = e.config.data_dir.parent().unwrap().join("other-model.json");
    fs::write(&path, base.replace("example-decision", "other-decision")).unwrap();
    let mut config = (*e.config).clone();
    config.ai = crate::ai::settings::AiConfig::load(&|k| {
        (k == "GEOFRAME_AI_CONFIG").then(|| path.to_str().unwrap().to_string())
    });
    assert_eq!(clue_pending_with(&e, &config), 2);
    // Rust が判定しない設定（用途なし・取り込み無効）では、旧 Python の行の世代一致で数える従来の値
    sql(&e, "DELETE FROM photo_loc_clues WHERE photo_id=2");
    let mut off = (*e.config).clone();
    off.ingest = false;
    assert_eq!(clue_pending_with(&e, &off), 1);
    let unset = env_with("clue-status-unset", "{}", true);
    sql(&unset, "DELETE FROM photos");
    fs::create_dir_all(unset.config.data_dir.join("photos")).unwrap();
    add(&unset, 1, "photos/a.jpg", Some(&jpeg()), 1);
    add(&unset, 2, "photos/b.jpg", Some(&jpeg()), 1);
    legacy_clue(&unset, 1, "photos/a.jpg");
    assert_eq!(clue_pending(&unset), 1);
}

// ---- STATUS の `db.loc.clue_runnable`・`clue_pause`（timer 用。今 Decision が動けば進む待ち）----

fn loc_status_with(e: &Env, ctx: &crate::admin::AdminContext) -> serde_json::Value {
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    crate::admin::status(&read, ctx).unwrap()["db"]["loc"].clone()
}
/// (clue_pending, clue_runnable, clue_pause)
fn waits(e: &Env) -> (i64, i64, String) {
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let loc = loc_status_with(e, &ctx);
    (
        loc["clue_pending"].as_i64().unwrap(),
        loc["clue_runnable"].as_i64().unwrap(),
        loc["clue_pause"].as_str().unwrap().to_string(),
    )
}
fn insert_job(e: &Env, key: &JobKey, state: &str, next_attempt_at: i64) {
    let (target, hash, version, state) = (
        key.target_key.clone(),
        key.input_hash.clone(),
        key.processor_version.clone(),
        state.to_string(),
    );
    e.writer()
        .call(move |c| {
            c.execute(
                "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,next_attempt_at,created_at,updated_at)
                 VALUES('location_clue',?1,?2,?3,?4,?5,0,0)",
                params![target, hash, version, state, next_attempt_at],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn status_clue_runnable_counts_only_waits_decision_can_advance() {
    let lab = Lab::new();
    let e = lab.env("clue-runnable");
    assert_eq!(waits(&e), (0, 0, String::new()));
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    // ジョブがまだ無い待ちは、動けば進む
    assert_eq!(waits(&e), (1, 1, String::new()));
    // 品質の解析待ち（画像を送らず後回しになる）だけなら進められない
    add(&e, 2, "photos/q.jpg", Some(&jpeg()), 0);
    assert_eq!(waits(&e), (2, 1, String::new()));
    sql(&e, "DELETE FROM photos WHERE id=1");
    assert_eq!(waits(&e), (1, 0, String::new()));
    // 品質の解析に失敗した写真も、Decision を使わずに恒久失敗になる
    add(&e, 3, "photos/f.jpg", Some(&jpeg()), 2);
    assert_eq!(waits(&e), (2, 0, String::new()));
}

#[test]
fn status_clue_runnable_ignores_deferrals_before_their_due_time_and_failed_history() {
    let lab = Lab::new();
    let e = lab.env("clue-runnable-defer");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let key = targets(&e)[0].key.clone();
    let now = crate::ai::jobs::unix_now();
    // 期限が先の後回しだけ
    insert_job(&e, &key, "deferred", now + 3600);
    assert_eq!(waits(&e), (1, 0, String::new()));
    // 期限が来れば動く
    sql(
        &e,
        &format!("UPDATE ai_jobs SET next_attempt_at={}", now - 1),
    );
    assert_eq!(waits(&e), (1, 1, String::new()));
    // 実行中も動いている扱い
    sql(&e, "UPDATE ai_jobs SET state='running', next_attempt_at=0");
    assert_eq!(waits(&e), (1, 1, String::new()));
    // 失敗の履歴だけ（明示の再試行まで動かない）
    sql(&e, "UPDATE ai_jobs SET state='failed'");
    assert_eq!(waits(&e), (1, 0, String::new()));
}

#[test]
fn status_clue_runnable_is_zero_while_the_provider_is_paused_and_follows_release() {
    let lab = Lab::new();
    let e = lab.env("clue-runnable-pause");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    add(&e, 2, "", None, 0);
    fs::write(e.config.data_dir.join("derivatives/fake2.jpg"), jpeg()).unwrap();
    assert_eq!(waits(&e), (2, 2, String::new()));
    let settings = e.config.ai.settings().unwrap();
    let provider = settings.provider_pause_key(TaskKind::LocationClue).unwrap();
    e.writer()
        .call(move |c| {
            crate::ai::pause::pause(
                c,
                TaskKind::LocationClue,
                &provider,
                crate::ai::pause::PauseReason::Unauthorized,
                1,
            )
        })
        .unwrap();
    // 総待ちは保つ。動けないので runnable は 0 で、理由コードだけ出す
    assert_eq!(waits(&e), (2, 0, "unauthorized".to_string()));
    sql(&e, "DELETE FROM ai_provider_pauses");
    assert_eq!(waits(&e), (2, 2, String::new()));
    // 用途の停止も同じ
    let task = settings.task_pause_key(TaskKind::LocationClue).unwrap();
    e.writer()
        .call(move |c| {
            crate::ai::pause::pause(
                c,
                TaskKind::LocationClue,
                &task,
                crate::ai::pause::PauseReason::ModelNotFound,
                1,
            )
        })
        .unwrap();
    assert_eq!(waits(&e), (2, 0, "model_not_found".to_string()));
}

/// 古い世代 A の解析日時を B より前にして、「最新の 1 行」が B になるようにする（互換行も合わせる）。
fn age_stored_clues(e: &Env, at: &str) {
    let at = at.to_string();
    e.writer()
        .call(move |c| {
            c.execute("UPDATE ai_location_clues SET analyzed_at=?1", params![at])?;
            c.execute("UPDATE photo_loc_clues SET analyzed_at=?1", params![at])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn status_fallback_wait_clears_after_the_worker_reuses_an_older_generation_a_b_a() {
    // 差し替え（実 bytes の SHA が変わる）と回転（DB の世代だけが変わる）の 2 通り
    for rotate in [false, true] {
        let lab = Lab::new();
        let e = lab.env("clue-status-aba");
        add(&e, 1, "", None, 0);
        let path = e.config.data_dir.join("derivatives/fake1.jpg");
        fs::write(&path, jpeg()).unwrap();
        // A: 成功。解析日時を古くして、あとの B が最新の行になるようにする
        cycle(&e);
        assert_eq!(lab.posts().len(), 1);
        age_stored_clues(&e, "2026-01-01T00:00:00Z");
        assert_eq!(waits(&e), (0, 0, String::new()));
        // B: 別の世代で成功
        if rotate {
            sql(&e, "UPDATE photos SET rotation=90");
        } else {
            fs::write(&path, imaging::rotate_jpeg(&jpeg(), 90).unwrap().jpeg).unwrap();
        }
        cycle(&e);
        assert_eq!(lab.posts().len(), 2);
        assert_eq!(waits(&e), (0, 0, String::new()));
        // A へ戻す。ワーカーが保存済みの A を再利用して互換行へ投影し直す（送信は増えない）が、A の日時は古いまま
        if rotate {
            sql(&e, "UPDATE photos SET rotation=0");
        } else {
            fs::write(&path, jpeg()).unwrap();
        }
        // ワーカーが観測するまで、ファイルだけの変更は見えない（B のまま）
        if !rotate {
            assert_eq!(waits(&e), (0, 0, String::new()));
        }
        cycle(&e);
        assert_eq!(lab.posts().len(), 2);
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM photo_loc_clues WHERE analyzed_at='2026-01-01T00:00:00Z'"
            ),
            1
        );
        assert!(matches!(targets(&e)[0].readiness, Readiness::Ready(_)));
        assert_eq!(waits(&e), (0, 0, String::new()));
    }
}

#[test]
fn status_clue_runnable_is_absent_when_rust_does_not_judge() {
    let lab = Lab::new();
    let e = lab.env("clue-runnable-off");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let mut off = (*e.config).clone();
    off.ingest = false;
    let ctx = crate::admin::AdminContext::new(&off, &off.data_dir);
    let loc = loc_status_with(&e, &ctx);
    assert!(loc.get("clue_runnable").is_none() && loc.get("clue_pause").is_none());
    assert_eq!(loc["clue_pending"], 1);
}

#[test]
fn status_never_opens_image_files_and_judges_fallback_photos_from_stored_rows() {
    let lab = Lab::new();
    let e = lab.env("clue-status-no-read");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    add(&e, 2, "", None, 0);
    add(&e, 3, "", None, 0);
    fs::write(e.config.data_dir.join("derivatives/fake2.jpg"), jpeg()).unwrap();
    // fake3 の derivative は無い（読めない fallback）
    let before = source_reads();
    assert_eq!(waits(&e), (3, 3, String::new()));
    cycle(&e);
    let after_cycle = source_reads();
    assert!(after_cycle > before);
    // 保存済み・恒久失敗の fallback も含め、待ちは 0。STATUS は 1 回もファイルを読まない
    assert_eq!(waits(&e), (0, 0, String::new()));
    assert_eq!(source_reads(), after_cycle);
    // ワーカーが観測するまで、ファイルだけの変更は見えない。観測して新しい世代のジョブが積まれたら待ちに戻る
    fs::write(e.config.data_dir.join("derivatives/fake2.jpg"), {
        let mut v = Vec::new();
        jpeg_encoder::Encoder::new(&mut v, 80)
            .encode(&[30; 32 * 24 * 3], 32, 24, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        v
    })
    .unwrap();
    assert_eq!(waits(&e), (0, 0, String::new()));
    let changed = Input::load(&crate::db::open_read_only(&e.config.db_path()).unwrap(), 2)
        .unwrap()
        .unwrap()
        .observe(&data(&e))
        .key("example-decision");
    insert_job(&e, &changed, "pending", 0);
    assert_eq!(waits(&e), (1, 1, String::new()));
    assert_eq!(source_reads(), after_cycle + 1); // 試験自身の observe の 1 回だけ
}

#[test]
fn status_clue_waits_are_cached_per_model_and_recomputed_after_the_ttl() {
    let lab = Lab::new();
    let e = lab.env("clue-status-cache");
    add(&e, 1, "photos/a.jpg", Some(&jpeg()), 1);
    let cached = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let fresh = crate::admin::AdminContext::new(&e.config, &e.config.data_dir)
        .with_clue_ttl(Duration::ZERO);
    assert_eq!(loc_status_with(&e, &cached)["clue_pending"], 1);
    add(&e, 2, "photos/b.jpg", Some(&jpeg()), 1);
    // TTL の間は前の値。TTL を過ぎた（0）文脈は数え直す
    assert_eq!(loc_status_with(&e, &cached)["clue_pending"], 1);
    assert_eq!(loc_status_with(&e, &fresh)["clue_pending"], 2);
    // 停止はキャッシュに入れず、毎回読む
    let provider = e
        .config
        .ai
        .settings()
        .unwrap()
        .provider_pause_key(TaskKind::LocationClue)
        .unwrap();
    e.writer()
        .call(move |c| {
            crate::ai::pause::pause(
                c,
                TaskKind::LocationClue,
                &provider,
                crate::ai::pause::PauseReason::Forbidden,
                1,
            )
        })
        .unwrap();
    let loc = loc_status_with(&e, &cached);
    assert_eq!(
        (loc["clue_pending"].as_i64(), loc["clue_runnable"].as_i64()),
        (Some(1), Some(0))
    );
    assert_eq!(loc["clue_pause"], "forbidden");
}
