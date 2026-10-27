//! Synthetic files/DB and loopback only, through production factory and real worker core.
use super::*;
use crate::ai::embedding::tests::{config_json, models, reply};
use crate::ai::embedding::{self, MODEL};
use crate::ai::http::Deadline;
use crate::ai::jobs::{Completion, Failure};
use crate::ai::provider::LanClient;
use crate::ai::settings::TaskKind;
use crate::ai::tests_http::{Captured, Stub, respond, stub};
use crate::ai::tests_worker::{Env, T0, env_with};
use crate::ai::worker::{self, Core};
use crate::stop::StopFlag;
use base64::Engine;
use serde_json::{Value, json};
use std::fs;
use std::sync::mpsc;
use std::time::Duration;

type Hook = Arc<dyn Fn(&Captured) + Send + Sync>;
pub(in crate::ai) struct Lab {
    pub(in crate::ai) stub: Stub,
    state: Arc<Mutex<State>>,
}
struct State {
    available: bool,
    lease: Value,
    status: u16,
    post: Value,
    models: Value,
    hook: Option<Hook>,
}
impl Lab {
    pub(in crate::ai) fn new() -> Self {
        let state = Arc::new(Mutex::new(State {
            available: true,
            lease: json!(0),
            status: 200,
            post: reply(),
            models: models(),
            hook: None,
        }));
        let shared = state.clone();
        let stub = stub(move |stream, req| {
            let hook = shared.lock().unwrap().hook.clone();
            if let Some(h) = hook {
                h(req);
            }
            let st = shared.lock().unwrap();
            let line = &req.request_line;
            let (status, body) = if line.starts_with("GET /llm/catalog ") {
                (
                    200,
                    json!({"services":[{"id":"embed","available":st.available,
                    "url":"http://192.0.2.1:9/trap","local_url":"http://192.0.2.1:9/trap",
                    "apis":[{"method":"POST","path":"/v1/embeddings"},{"method":"GET","path":"/v1/models"}]}]}),
                )
            } else if line.starts_with("GET /llm/services/embed ") {
                (
                    200,
                    json!({"id":"embed","state":"running","available":st.available,"lease_count":st.lease,
                    "endpoint":"192.0.2.1:9"}),
                )
            } else if line.starts_with("GET /llm-embed/v1/models ") {
                (200, st.models.clone())
            } else if line.starts_with("POST /llm-embed/v1/embeddings ") {
                (st.status, st.post.clone())
            } else {
                (404, json!({}))
            };
            drop(st);
            respond(
                stream,
                &format!("{status} X"),
                "Retry-After: 60\r\n",
                &serde_json::to_vec(&body).unwrap(),
            );
        });
        Self { stub, state }
    }
    pub(in crate::ai) fn env(&self, tag: &str) -> Env {
        let e = env_with(
            tag,
            &config_json(&format!("http://{}", self.stub.addr)),
            true,
        );
        sql(&e, "DELETE FROM photos");
        fs::create_dir_all(e.config.data_dir.join("derivatives")).unwrap();
        e
    }
    pub(in crate::ai) fn posts(&self) -> Vec<Captured> {
        self.stub
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.request_line.starts_with("POST "))
            .cloned()
            .collect()
    }
    fn hook(&self, f: impl Fn(&Captured) + Send + Sync + 'static) {
        self.state.lock().unwrap().hook = Some(Arc::new(f));
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
fn pointer(e: &Env) -> Option<String> {
    use rusqlite::OptionalExtension;
    e.writer()
        .call(|c| {
            c.query_row(
                "SELECT value FROM settings WHERE key='ai_active_space'",
                [],
                |r| r.get(0),
            )
            .optional()
        })
        .unwrap()
}
fn handlers(e: &Env) -> Vec<Arc<dyn Handler>> {
    worker::production_handlers_for_test(&e.config, LanClient::new(&e.config.ai))
}
fn cycle(e: &Env) {
    Core::new(&e.deps(handlers(e), &StopFlag::new(), "embed-test")).cycle();
}
fn jpeg(seed: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let pixels = (0..32 * 24 * 3)
        .map(|i| seed.wrapping_add((i % 17) as u8))
        .collect::<Vec<_>>();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(&pixels, 32, 24, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}
fn photo(e: &Env, id: i64, bytes: &[u8]) -> String {
    let rel = format!("derivatives/synthetic-{id}.jpg");
    fs::write(e.config.data_dir.join(&rel), bytes).unwrap();
    let hash = sha(bytes);
    let h = hash.clone();
    e.writer().call(move|c|{
        c.execute("INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,hidden,favorite,rotation)
            VALUES(?1,'local','synthetic',?2,'m',1,'2026-01-02 03:04',1,1,0)",params![id,rel])?;
        c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error,analyzed_at)
            VALUES(?1,?2,'m',1,?3,'','synthetic')",params![id,rel,h])?;Ok(())
    }).unwrap();
    hash
}
fn dump(e: &Env, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let q = format!("SELECT * FROM {table} ORDER BY 1");
    e.writer()
        .call(move |c| {
            let mut stmt = c.prepare(&q)?;
            let columns = stmt.column_count();
            stmt.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())?
                .collect()
        })
        .unwrap()
}
fn prepare(e: &Env) -> (Arc<dyn Handler>, Claimed) {
    let h = handlers(e).into_iter().next().unwrap();
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    for action in h.reconcile_cleanup(&read, T0).unwrap() {
        e.writer()
            .call(move |c| {
                let tx = c.transaction()?;
                action(&tx)?;
                tx.commit()
            })
            .unwrap();
    }
    let keys = h.reconcile(&read, T0).unwrap();
    let job = e
        .writer()
        .call(move |c| {
            jobs::enqueue_batch(c, &keys, T0)?;
            jobs::claim_next(c, T0, "run", "token", &[JobKind::ImageEmbedding])
        })
        .unwrap()
        .unwrap();
    (h, job)
}
#[test]
fn factory_to_reconcile_claim_image_atomic_store_and_publish_restart_sends_zero() {
    let lab = Lab::new();
    let e = lab.env("embed-e2e");
    let raw = jpeg(17);
    let mut source = raw[..2].to_vec();
    let marker = b"Exif\0\0synthetic-private-gps";
    source.extend([0xff, 0xe1]);
    source.extend(((marker.len() + 2) as u16).to_be_bytes());
    source.extend(marker);
    source.extend(&raw[2..]);
    source.extend(b"synthetic-private-tail");
    photo(&e, 1, &source);
    let before = dump(&e, "photos");
    let quality = dump(&e, "photo_quality_features");
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        1
    );
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 1);
    assert_eq!(pointer(&e).as_deref(), Some("eg2-test-v1"));
    assert_eq!(dump(&e, "photos"), before);
    assert_eq!(dump(&e, "photo_quality_features"), quality);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_embeddings"), 0);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM photo_ai_features"), 0);
    cycle(&e);
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    let posts = lab.posts();
    let body: Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["input"].as_array().unwrap().len(), 1);
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["encoding_format"], "float");
    let encoded = body["input"][0]["image"]
        .as_str()
        .unwrap()
        .strip_prefix("data:image/jpeg;base64,")
        .unwrap();
    let sent = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_ne!(sent, source);
    assert!(!sent.windows(4).any(|w| w == b"Exif"));
    assert!(!sent.windows(17).any(|w| w == b"synthetic-private"));
    assert!(!sent.windows(11).any(|w| w == b"ICC_PROFILE"));
    let identity = embedding::configured_identity(&e.config.ai, TaskKind::ImageEmbedding).unwrap();
    let sent_hash = sha(&sent);
    e.writer()
        .call(move |c| {
            let (stored, blob): (String, Vec<u8>) = c.query_row(
                "SELECT sent_sha256,vector FROM ai_embedding_vectors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(stored, sent_hash);
            let values = super::super::space::decode(&identity, &blob).unwrap();
            assert!((values[0] - 0.6).abs() < 1e-6);
            Ok(())
        })
        .unwrap();
}
#[test]
fn missing_or_corrupt_success_regenerates_only_that_vector_and_reuses_valid_success() {
    let lab = Lab::new();
    let e = lab.env("embed-repair");
    photo(&e, 1, &jpeg(1));
    photo(&e, 2, &jpeg(2));
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    sql(
        &e,
        "UPDATE ai_embedding_vectors SET vector=zeroblob(dimension*4) WHERE input_sha256=(SELECT content_sha256 FROM photo_quality_features WHERE photo_id=1)",
    );
    cycle(&e);
    assert_eq!(lab.posts().len(), 3);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        2
    );
    sql(
        &e,
        "DELETE FROM ai_embedding_vectors WHERE input_sha256=(SELECT content_sha256 FROM photo_quality_features WHERE photo_id=2)",
    );
    cycle(&e);
    assert_eq!(lab.posts().len(), 4);
    cycle(&e);
    assert_eq!(lab.posts().len(), 4);
    // Lost job rows can reuse independently verified vectors without a send.
    sql(&e, "DELETE FROM ai_jobs");
    cycle(&e);
    assert_eq!(lab.posts().len(), 4);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        2
    );
}
#[test]
fn discovery_unavailable_lease_and_429_defer_then_resume() {
    for mode in [0, 1, 2] {
        let lab = Lab::new();
        let e = lab.env("embed-wait");
        photo(&e, 1, &jpeg(2));
        photo(&e, 2, &jpeg(3));
        {
            let mut s = lab.state.lock().unwrap();
            match mode {
                0 => s.available = false,
                1 => s.lease = json!(1),
                _ => s.status = 429,
            };
        }
        cycle(&e);
        assert_eq!(n(&e, "SELECT SUM(attempts) FROM ai_jobs"), 0);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='deferred'"),
            1
        );
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_provider_waits"), 1);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_provider_pauses"), 0);
        assert_eq!(lab.posts().len(), if mode == 2 { 1 } else { 0 });
        // New worker sees the persisted wait and sends nothing.
        cycle(&e);
        assert_eq!(lab.posts().len(), if mode == 2 { 1 } else { 0 });
        {
            let mut s = lab.state.lock().unwrap();
            s.available = true;
            s.lease = json!(0);
            s.status = 200;
        }
        e.advance(61);
        cycle(&e);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
            2
        );
    }
}
#[test]
fn unauthorized_pauses_provider_and_model_or_capability_pauses_task() {
    for mode in [0, 1, 2] {
        let lab = Lab::new();
        let e = lab.env("embed-pause");
        photo(&e, 1, &jpeg(4));
        {
            let mut s = lab.state.lock().unwrap();
            match mode {
                0 => s.status = 401,
                1 => s.post["model"] = json!("wrong"),
                _ => s.models["data"][0]["modalities"] = json!(["text"]),
            };
        }
        cycle(&e);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_provider_pauses"), 1);
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM ai_provider_pauses WHERE scope='provider'"
            ),
            if mode == 0 { 1 } else { 0 }
        );
        assert_eq!(n(&e, "SELECT SUM(attempts) FROM ai_jobs"), 0);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 0);
        let posts = lab.posts().len();
        e.advance(3600);
        cycle(&e);
        assert_eq!(lab.posts().len(), posts);
        // Explicit release allows recovery; neither scope is silently cleared.
        {
            let mut s = lab.state.lock().unwrap();
            s.status = 200;
            s.post = reply();
            s.models = models();
        }
        sql(&e, "DELETE FROM ai_provider_pauses");
        e.advance(61);
        cycle(&e);
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
            1
        );
    }
}
#[test]
fn one_bad_image_hits_terminal_limit_other_images_continue_and_retry_adds_difference() {
    let lab = Lab::new();
    let e = lab.env("embed-repeat");
    photo(&e, 1, &jpeg(5));
    lab.state.lock().unwrap().status = 500;
    for _ in 0..8 {
        cycle(&e);
        e.advance(3601);
    }
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND terminal=1"
        ),
        1
    );
    let posts = lab.posts().len();
    cycle(&e);
    assert_eq!(lab.posts().len(), posts);
    lab.state.lock().unwrap().status = 200;
    photo(&e, 2, &jpeg(6));
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        1
    );
    // Below 95%, old pointer is preserved, then a manual retry adds just the failed SHA.
    assert!(pointer(&e).is_none());
    let h = handlers(&e).remove(0);
    e.writer()
        .call(move |c| {
            jobs::retry_failed(c, T0 + 50000, jobs::RetryScope::All, |c, k| h.recheck(c, k))
        })
        .unwrap();
    e.advance(50001);
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        2
    );
    assert_eq!(pointer(&e).as_deref(), Some("eg2-test-v1"));
    let posts = lab.posts().len();
    cycle(&e);
    assert_eq!(lab.posts().len(), posts);
}
#[test]
fn old_pointer_remains_pending_all_failed_never_publishes_and_99_of_100_does() {
    let lab = Lab::new();
    let e = lab.env("embed-coverage");
    sql(&e, "INSERT INTO settings VALUES('ai_active_space','old')");
    let identity = embedding::configured_identity(&e.config.ai, TaskKind::ImageEmbedding).unwrap();
    for i in 1..=100 {
        let hash = photo(&e, i, &jpeg(i as u8));
        if i < 100 {
            let identity = identity.clone();
            e.writer()
                .call(move |c| {
                    index::register_space(c, &identity, T0).unwrap();
                    index::store_vector(c, &identity, &hash, &"a".repeat(64), &vec![1.0; 768], T0)
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        }
    }
    lab.state.lock().unwrap().available = false;
    cycle(&e);
    assert_eq!(pointer(&e).as_deref(), Some("old"));
    // Terminal current failed member excludes exactly one SHA, reusing 99 valid vectors.
    sql(
        &e,
        "UPDATE ai_jobs SET state='failed',terminal=1,last_error_code='input_invalid' WHERE target_key LIKE '%'||char(31)||(SELECT content_sha256 FROM photo_quality_features WHERE photo_id=100)",
    );
    e.advance(61);
    lab.state.lock().unwrap().available = true;
    cycle(&e);
    assert_eq!(pointer(&e).as_deref(), Some("eg2-test-v1"));
    assert_eq!(lab.posts().len(), 0);
    let lab = Lab::new();
    let e = lab.env("embed-all-fail");
    photo(&e, 1, b"invalid synthetic jpeg");
    sql(&e, "INSERT INTO settings VALUES('ai_active_space','old')");
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='failed'"),
        1
    );
    assert_eq!(pointer(&e).as_deref(), Some("old"));
    assert_eq!(lab.posts().len(), 0);
}
#[test]
fn generation_changes_after_discovery_prevent_send_and_after_post_prevent_commit() {
    for after_post in [false, true] {
        for mutation in [
            "UPDATE photos SET rotation=90 WHERE id=1",
            "UPDATE photos SET md5='new' WHERE id=1",
            "UPDATE photos SET mtime=2 WHERE id=1",
            "UPDATE photos SET display='derivatives/other.jpg' WHERE id=1",
            "DELETE FROM photos WHERE id=1",
        ] {
            let lab = Lab::new();
            let e = lab.env("embed-generation-race");
            photo(&e, 1, &jpeg(7));
            let writer = e.writer();
            let mutation = mutation.to_string();
            lab.hook(move |r| {
                if r.request_line.starts_with(if after_post {
                    "POST "
                } else {
                    "GET /llm-embed/v1/models "
                }) {
                    let q = mutation.clone();
                    writer.call(move |c| c.execute_batch(&q)).unwrap();
                }
            });
            cycle(&e);
            assert_eq!(lab.posts().len(), usize::from(after_post));
            assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 0);
            assert_eq!(
                n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
                0
            );
        }
    }
}
#[test]
fn actual_file_change_and_deletion_after_discovery_or_post_are_rejected() {
    for after_post in [false, true] {
        for delete in [false, true] {
            let lab = Lab::new();
            let e = lab.env("embed-file-race");
            photo(&e, 1, &jpeg(8));
            let path = e.config.data_dir.join("derivatives/synthetic-1.jpg");
            lab.hook(move |r| {
                if r.request_line.starts_with(if after_post {
                    "POST "
                } else {
                    "GET /llm-embed/v1/models "
                }) {
                    if delete {
                        let _ = fs::remove_file(&path);
                    } else {
                        fs::write(&path, jpeg(9)).unwrap();
                    }
                }
            });
            cycle(&e);
            assert_eq!(lab.posts().len(), usize::from(after_post));
            assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 0);
        }
    }
}
#[test]
fn stop_after_discovery_sends_zero_but_sent_result_can_complete_and_save_is_not_overwritten() {
    for before_post in [true, false] {
        let lab = Lab::new();
        let e = lab.env("embed-stop");
        photo(&e, 1, &jpeg(10));
        let stop = StopFlag::new();
        let signal = stop.clone();
        let writer = e.writer();
        lab.hook(move |r| {
            if r.request_line.starts_with(if before_post {
                "GET /llm-embed/v1/models "
            } else {
                "POST "
            }) {
                writer
                    .call(|c| {
                        c.execute(
                            "UPDATE photos SET lat=1,lon=2,hidden=0,favorite=0 WHERE id=1",
                            [],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                signal.stop();
            }
        });
        Core::new(&e.deps(handlers(&e), &stop, "embed-stop")).cycle();
        assert_eq!(lab.posts().len(), usize::from(!before_post));
        assert_eq!(
            n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
            i64::from(!before_post)
        );
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM photos WHERE lat=1 AND lon=2 AND hidden=0 AND favorite=0"
            ),
            1
        );
    }
}
#[test]
fn completion_checks_token_and_db_generation_and_rolls_back_vector_and_job_together() {
    for stale_token in [false, true] {
        let lab = Lab::new();
        let e = lab.env("embed-cas");
        photo(&e, 1, &jpeg(11));
        let (h, job) = prepare(&e);
        let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
        let writer = e.writer();
        let stop = StopFlag::new();
        let clock = e.clock();
        let ctx = RunCtx::for_test(
            &read,
            &e.config.ai,
            TaskKind::ImageEmbedding,
            &stop,
            &writer,
            &clock,
        );
        let action = h.run(&ctx, &job).unwrap();
        if stale_token {
            sql(&e, "UPDATE ai_jobs SET claim_token='replacement'");
        } else {
            sql(&e, "UPDATE photos SET rotation=180 WHERE id=1");
        }
        let result = e
            .writer()
            .call(move |c| jobs::complete(c, &job, T0, action))
            .unwrap();
        assert_eq!(
            result,
            if stale_token {
                Completion::Rejected
            } else {
                Completion::Obsolete
            }
        );
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 0);
    }
    let lab = Lab::new();
    let e = lab.env("embed-atomic");
    photo(&e, 1, &jpeg(12));
    sql(
        &e,
        "CREATE TRIGGER reject_success BEFORE UPDATE OF state ON ai_jobs WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'synthetic'); END;",
    );
    cycle(&e);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 0);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        0
    );
}
#[tokio::test]
async fn http_wait_holds_neither_writer_nor_image_lock_and_query_can_run_in_parallel() {
    let lab = Lab::new();
    let e = lab.env("embed-parallel");
    photo(&e, 1, &jpeg(13));
    let lan = LanClient::new(&e.config.ai).unwrap();
    let (entered, wait) = mpsc::channel();
    let (release, hold) = mpsc::channel();
    let hold = Arc::new(Mutex::new(hold));
    lab.hook(move |r| {
        if r.request_line.starts_with("POST ") && r.body.windows(7).any(|s| s == b"\"image\"") {
            entered.send(()).unwrap();
            hold.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    });
    let h = worker::production_handlers_for_test(&e.config, Some(lan.clone()));
    let deps = e.deps(h, &StopFlag::new(), "parallel");
    let join = std::thread::spawn(move || Core::new(&deps).cycle());
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    e.writer()
        .call(|c| {
            c.execute("INSERT INTO settings VALUES('writer-available','1')", [])?;
            Ok(())
        })
        .unwrap();
    {
        let _lock = imaging::lock_derivatives();
    }
    let client = EmbeddingClient::new(&e.config.ai, lan.clone(), TaskKind::QueryEmbedding).unwrap();
    let result = lan
        .http()
        .run_interactive(&StopFlag::new(), move |s| {
            client.query(&s, "合成", &Deadline::interactive(), || Ok(()))
        })
        .await
        .unwrap();
    release.send(()).unwrap();
    join.join().unwrap();
    assert!(result.is_ok());
    assert_eq!(lab.posts().len(), 2);
}
#[test]
fn generation_churn_preserves_terminal_and_interruption_history_other_spaces_are_untouched() {
    let lab = Lab::new();
    let e = lab.env("embed-history");
    photo(&e, 1, &jpeg(14));
    let (_, job) = prepare(&e);
    e.writer()
        .call(move |c| {
            jobs::record_failure(c, &job, T0, ErrorCode::InputInvalid, Failure::Permanent)
        })
        .unwrap();
    sql(&e,"UPDATE ai_jobs SET attempts=5,interruption_count=3; UPDATE photos SET rotation=90;
        INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,created_at,updated_at)
        VALUES('image_embedding','other-space'||char(31)||'abc','other','other','pending',0,0)");
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND terminal=1 AND attempts=5 AND interruption_count=3"
        ),
        1
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE target_key LIKE 'other-space%' AND state='pending'"
        ),
        1
    );
}
#[test]
fn cleanup_failure_cannot_fall_through_to_enqueue_and_disabled_factory_sends_nothing() {
    let lab = Lab::new();
    let e = lab.env("embed-cleanup-fail");
    photo(&e, 1, &jpeg(15));
    sql(
        &e,
        "CREATE TRIGGER no_space BEFORE INSERT ON ai_embedding_spaces BEGIN SELECT RAISE(ABORT,'synthetic'); END;",
    );
    cycle(&e);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 0);
    assert_eq!(lab.posts().len(), 0);
    for json in [
        "{}".to_string(),
        config_json(&format!("http://{}", lab.stub.addr)).replace(embedding::CONTRACT, "unknown"),
        config_json(&format!("http://{}", lab.stub.addr)).replace(embedding::REVISION, "unknown"),
    ] {
        let e = env_with("embed-disabled", &json, true);
        assert!(handlers(&e).is_empty());
    }
    let e = lab.env("embed-ingest-off");
    let mut config = (*e.config).clone();
    config.ingest = false;
    assert!(worker::production_handlers_for_test(&config, LanClient::new(&config.ai)).is_empty());
}
#[test]
fn new_duplicate_generation_reuses_verified_vector_without_send() {
    let lab = Lab::new();
    let e = lab.env("embed-duplicate");
    let bytes = jpeg(16);
    photo(&e, 1, &bytes);
    cycle(&e);
    photo(&e, 2, &bytes);
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 1);
    // Changed bytes get a new SHA/job and preserve the old stored result.
    let bytes = jpeg(18);
    fs::write(
        e.config.data_dir.join("derivatives/synthetic-2.jpg"),
        &bytes,
    )
    .unwrap();
    let hash = sha(&bytes);
    e.writer()
        .call(move |c| {
            c.execute(
                "UPDATE photo_quality_features SET content_sha256=?1 WHERE photo_id=2",
                [hash],
            )?;
            Ok(())
        })
        .unwrap();
    cycle(&e);
    assert_eq!(lab.posts().len(), 2);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 2);
}

#[test]
fn changed_configuration_rejects_an_old_handler_without_a_send_or_migration_delta() {
    let lab = Lab::new();
    let e = lab.env("embed-config-old");
    photo(&e, 1, &jpeg(19));
    let (h, job) = prepare(&e);
    let settings = config_json(&format!("http://{}", lab.stub.addr))
        .replace(embedding::REVISION, "unknown-revision");
    let newer = env_with("embed-config-new", &settings, true);
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let writer = e.writer();
    let clock = e.clock();
    let stop = StopFlag::new();
    let ctx = RunCtx::for_test(
        &read,
        &newer.config.ai,
        TaskKind::ImageEmbedding,
        &stop,
        &writer,
        &clock,
    );
    assert!(matches!(h.run(&ctx, &job), Err(RunError::PolicyDisabled)));
    assert_eq!(lab.posts().len(), 0);
    assert!(
        crate::ai::pending_active(&read, false, false, false)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::ingest::plan(&e.config, &read)
            .unwrap()
            .schema
            .missing_objects
            .is_empty()
    );
}
#[test]
fn pause_written_during_discovery_is_checked_before_inference() {
    let lab = Lab::new();
    let e = lab.env("embed-last-pause");
    photo(&e, 1, &jpeg(20));
    let writer = e.writer();
    let config = e.config.clone();
    lab.hook(move |r| {
        if r.request_line.starts_with("GET /llm-embed/v1/models ") {
            let key = config
                .ai
                .settings()
                .unwrap()
                .task_pause_key(TaskKind::ImageEmbedding)
                .unwrap();
            writer
                .call(move |c| {
                    crate::ai::pause::pause(
                        c,
                        TaskKind::ImageEmbedding,
                        &key,
                        crate::ai::pause::PauseReason::ContractMismatch,
                        T0,
                    )
                })
                .unwrap();
        }
    });
    cycle(&e);
    assert_eq!(lab.posts().len(), 0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='deferred' AND attempts=0"
        ),
        1
    );
}
#[test]
fn lease_started_during_post_is_reprobed_without_spending_failure_attempts() {
    let lab = Lab::new();
    let e = lab.env("embed-lease-race");
    photo(&e, 1, &jpeg(21));
    let state = lab.state.clone();
    lab.hook(move |r| {
        if r.request_line.starts_with("POST ") {
            let mut s = state.lock().unwrap();
            s.status = 502;
            s.lease = json!(1);
            s.available = false;
        }
    });
    cycle(&e);
    assert_eq!(lab.posts().len(), 1);
    assert_eq!(n(&e, "SELECT SUM(attempts) FROM ai_jobs"), 0);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='deferred'"),
        1
    );
    {
        let mut s = lab.state.lock().unwrap();
        s.hook = None;
        s.lease = json!(0);
        s.available = true;
        s.status = 200;
    }
    e.advance(61);
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        1
    );
}
#[test]
fn service_wait_and_task_pause_allow_another_purpose_to_continue() {
    use crate::ai::tests_worker::{Fake, key};
    for pause in [false, true] {
        let lab = Lab::new();
        let mut config: Value =
            serde_json::from_str(&config_json(&format!("http://{}", lab.stub.addr))).unwrap();
        config["tasks"]["location_clue"] = json!({"provider":"local","model":"example-decision"});
        let e = env_with("embed-other-purpose", &config.to_string(), true);
        sql(&e, "DELETE FROM photos");
        fs::create_dir_all(e.config.data_dir.join("derivatives")).unwrap();
        photo(&e, 1, &jpeg(22));
        if pause {
            lab.state.lock().unwrap().models["data"][0]["modalities"] = json!(["text"]);
        } else {
            lab.state.lock().unwrap().available = false;
        }
        let fake = Fake::new(JobKind::LocationClue, |ctx, job| {
            ctx.before_send()?;
            Ok(crate::ai::tests_worker::persist_result(
                job.key.target_key.clone(),
                InputCheck::Current,
            ))
        });
        let other = key(JobKind::LocationClue, "synthetic-other");
        e.enqueue(&other);
        let mut handlers = handlers(&e);
        handlers.retain(|h| h.kind() != JobKind::LocationClue);
        handlers.push(fake.clone());
        Core::new(&e.deps(handlers, &StopFlag::new(), "other-purpose")).cycle();
        assert_eq!(fake.runs(), 1);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_test_results"), 1);
        assert_eq!(lab.posts().len(), 0);
    }
}

#[test]
fn reverting_to_an_older_generation_cannot_revive_a_lower_failure_budget() {
    let lab = Lab::new();
    let e = lab.env("embed-revert-history");
    photo(&e, 1, &jpeg(23));
    lab.state.lock().unwrap().status = 500;
    cycle(&e);
    e.advance(3601);
    sql(&e, "UPDATE photos SET rotation=90");
    cycle(&e);
    sql(
        &e,
        "UPDATE ai_jobs SET terminal=1,state='failed',attempts=5 WHERE state='deferred'",
    );
    let posts = lab.posts().len();
    // This key already exists as obsolete with a lower attempt count; it must inherit
    // the terminal current lineage before enqueue has any chance to revive it.
    sql(&e, "UPDATE photos SET rotation=0");
    e.advance(3601);
    cycle(&e);
    assert_eq!(lab.posts().len(), posts);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND terminal=1 AND attempts=5"
        ),
        1
    );
    // Explicit retry clears the current lineage, and a later generation may then run.
    let h = handlers(&e).remove(0);
    let now = e.clock()();
    e.writer()
        .call(move |c| jobs::retry_failed(c, now, jobs::RetryScope::All, |c, k| h.recheck(c, k)))
        .unwrap();
    sql(&e, "UPDATE photos SET rotation=90");
    e.advance(3601);
    lab.state.lock().unwrap().status = 200;
    cycle(&e);
    assert_eq!(lab.posts().len(), posts + 1);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_embedding_vectors"), 1);
}

// Regressions adapted from the independent review reproductions. Every cleanup action
// is executed in a writer transaction, matching production's bounded batches.
fn cleanup(e: &Env, h: &Arc<dyn Handler>, now: i64) {
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    for action in h.reconcile_cleanup(&read, now).unwrap() {
        e.writer()
            .call(move |c| {
                let tx = c.transaction()?;
                action(&tx)?;
                tx.commit()
            })
            .unwrap();
    }
}

#[test]
fn old_success_cannot_mask_current_terminal_after_temporary_absence() {
    let lab = Lab::new();
    let e = lab.env("lineage-old-success");
    let bytes = jpeg(26);
    photo(&e, 1, &bytes);
    cycle(&e);
    photo(&e, 2, &bytes);
    e.advance(61);
    cycle(&e);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded'"),
        2
    );
    assert_eq!(lab.posts().len(), 1);
    sql(&e, "DELETE FROM ai_embedding_vectors");
    lab.state.lock().unwrap().status = 422;
    e.advance(61);
    cycle(&e);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE id=2 AND state='failed' AND terminal=1"
        ),
        1
    );
    sql(&e, "UPDATE photos SET md5='temporarily-stale'");
    e.advance(61);
    cycle(&e);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE id=2 AND state='obsolete' AND terminal=1"
        ),
        1
    );
    sql(&e, "UPDATE photos SET md5='m'");
    e.advance(61);
    let h = handlers(&e).remove(0);
    cleanup(&e, &h, e.clock()());
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE id=2 AND state='failed' AND terminal=1 AND last_error_code='input_invalid'"
        ),
        1
    );
    // A new duplicate generation must inherit B, never the older success A.
    photo(&e, 3, &bytes);
    cleanup(&e, &h, e.clock()());
    cycle(&e);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE id=3 AND state='failed' AND terminal=1 AND last_error_code='input_invalid'"
        ),
        1
    );
    assert_eq!(lab.posts().len(), 2);
}

#[test]
fn same_second_roundtrip_and_absence_preserve_all_terminal_and_wait_history() {
    for mode in ["attempt", "permanent", "interruption", "defer"] {
        let lab = Lab::new();
        let e = lab.env(&format!("lineage-{mode}"));
        photo(&e, 1, &jpeg(25));
        let h = handlers(&e).remove(0);
        cleanup(&e, &h, T0); // A, id 1
        sql(&e, "UPDATE photos SET rotation=90");
        cleanup(&e, &h, T0); // B, id 2
        sql(&e, "UPDATE photos SET rotation=0");
        cleanup(&e, &h, T0); // A again
        e.writer()
            .call(move |c| {
                let job =
                    jobs::claim_next_scoped(c, T0, "run", "token", &[h.claim_scope()])?.unwrap();
                assert_eq!(job.id, 1);
                match mode {
                    "permanent" => {
                        jobs::record_failure(
                            c,
                            &job,
                            T0,
                            ErrorCode::InputInvalid,
                            Failure::Permanent,
                        )?;
                    }
                    "attempt" => {
                        c.execute("UPDATE ai_jobs SET attempts=4,defer_count=6 WHERE id=1", [])?;
                        jobs::record_failure(c, &job, T0, ErrorCode::Timeout, Failure::Attempt)?;
                    }
                    "interruption" => {
                        c.execute(
                            "UPDATE ai_jobs SET interruption_count=2,defer_count=6 WHERE id=1",
                            [],
                        )?;
                        jobs::reclaim_interrupted(c, "restart", T0)?;
                    }
                    _ => {
                        jobs::record_failure(
                            c,
                            &job,
                            T0,
                            ErrorCode::Timeout,
                            Failure::Defer {
                                retry_after_secs: Some(300),
                            },
                        )?;
                    }
                }
                Ok(())
            })
            .unwrap();
        // Capture the authoritative history before absence. Timestamps and generated IDs
        // are deliberately misleading, and a fresh handler simulates process restart.
        let before=e.writer().call(|c|c.query_row("SELECT attempts,defer_count,interruption_count,terminal,next_attempt_at,last_error_code FROM ai_jobs WHERE id=1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,bool>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)))).unwrap();
        let h = handlers(&e).remove(0);
        sql(&e, "UPDATE photos SET md5='absent'");
        cleanup(&e, &h, T0);
        sql(&e, "UPDATE photos SET md5='m'");
        cleanup(&e, &h, T0);
        let after=e.writer().call(|c|c.query_row("SELECT attempts,defer_count,interruption_count,terminal,next_attempt_at,last_error_code FROM ai_jobs WHERE id=1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,bool>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)))).unwrap();
        assert_eq!(before, after, "{mode}");
        // Another entirely new generation inherits every field, even after absence.
        sql(&e, "UPDATE photos SET rotation=180");
        cleanup(&e, &h, T0);
        let inherited=e.writer().call(|c|c.query_row("SELECT attempts,defer_count,interruption_count,terminal,next_attempt_at,last_error_code FROM ai_jobs WHERE id=3",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,bool>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)))).unwrap();
        assert_eq!(before, inherited, "{mode}");
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM ai_jobs WHERE id=3 AND state='failed'"
            ),
            i64::from(mode != "defer")
        );
        assert!(lab.posts().is_empty());
    }
}

#[test]
fn explicit_retry_resets_old_lineage_history_before_existing_and_new_generations() {
    for tracked in [true, false] {
        let lab = Lab::new();
        let e = lab.env("lineage-retry");
        photo(&e, 1, &jpeg(27));
        let (h, job) = prepare(&e);
        e.writer()
            .call(move |c| {
                jobs::record_failure(c, &job, T0, ErrorCode::InputInvalid, Failure::Permanent)
            })
            .unwrap();
        sql(&e, "UPDATE photos SET rotation=90");
        cleanup(&e, &h, T0);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE terminal=1"), 2);
        if !tracked {
            sql(&e, "DELETE FROM ai_image_embedding_lineage");
        }
        let retry = h.clone();
        e.writer()
            .call(move |c| {
                jobs::retry_failed(c, T0, jobs::RetryScope::All, |c, k| retry.recheck(c, k))
            })
            .unwrap();
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM ai_jobs WHERE terminal=1 OR attempts!=0 OR defer_count!=0 OR interruption_count!=0 OR last_error_code!=''"
            ),
            0
        );
        sql(&e, "UPDATE photos SET md5='absent'");
        cleanup(&e, &h, T0);
        sql(&e, "UPDATE photos SET md5='m',rotation=0");
        cleanup(&e, &h, T0);
        assert_eq!(
            n(
                &e,
                "SELECT COUNT(*) FROM ai_jobs WHERE id=1 AND state='pending' AND terminal=0"
            ),
            1
        );
        sql(&e, "UPDATE photos SET rotation=180");
        cleanup(&e, &h, T0);
        cycle(&e);
        assert_eq!(lab.posts().len(), 1);
        assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs WHERE terminal=1"), 0);
    }
}

#[test]
fn cleanup_and_supersede_protect_other_processors_and_spaces_in_all_states_and_races() {
    let lab = Lab::new();
    let e = lab.env("processor-boundary");
    photo(&e, 1, &jpeg(28));
    let h = handlers(&e).remove(0);
    cleanup(&e, &h, T0);
    sql(&e,"INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,claim_token,created_at,updated_at)
        SELECT kind,target_key,'other-'||st,s,st,'other-token',0,0 FROM ai_jobs
        CROSS JOIN (SELECT 'pending' st UNION ALL SELECT 'running' UNION ALL SELECT 'deferred' UNION ALL SELECT 'failed')
        CROSS JOIN (SELECT 'foreign-processor' s)
        WHERE id=1;");
    sql(&e,"INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,claim_token,created_at,updated_at)
        SELECT kind,'other-space'||char(31)||'sha',input_hash,processor_version,state,claim_token,0,0
        FROM ai_jobs WHERE processor_version='foreign-processor';");
    sql(&e,"INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,claim_token,created_at,updated_at)
        SELECT 'image_embedding','other-space'||char(31)||'sha','same-'||st,processor_version,st,'space-token',0,0
        FROM ai_jobs CROSS JOIN (SELECT 'pending' st UNION ALL SELECT 'running' UNION ALL SELECT 'deferred' UNION ALL SELECT 'failed') WHERE id=1;");
    let before=e.writer().call(|c| {
        c.prepare("SELECT id,target_key,input_hash,processor_version,state,claim_token FROM ai_jobs WHERE id!=1 ORDER BY id")?
        .query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()
    }).unwrap();
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let actions = h.reconcile_cleanup(&read, T0).unwrap();
    // Change generation after planning. The writer must use the new generation and limit
    // both cleanup and supersede to this processor.
    sql(&e, "UPDATE photos SET rotation=90");
    for action in actions {
        e.writer()
            .call(move |c| {
                let tx = c.transaction()?;
                action(&tx)?;
                tx.commit()
            })
            .unwrap();
    }
    sql(&e, "UPDATE photos SET md5='absent'");
    cleanup(&e, &h, T0);
    let after=e.writer().call(|c| {
        c.prepare("SELECT id,target_key,input_hash,processor_version,state,claim_token FROM ai_jobs WHERE processor_version='foreign-processor' OR target_key LIKE 'other-space%' ORDER BY id")?
        .query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()
    }).unwrap();
    assert_eq!(before, after);
    assert!(h.reconcile(&read, T0).unwrap().is_empty()); // generic supersede cannot re-enter
    let scope = h.claim_scope();
    assert!(
        e.writer()
            .call(move |c| jobs::claim_next_scoped(c, T0, "run", "claim", &[scope]))
            .unwrap()
            .is_none()
    );
}

fn synthetic_targets(e: &Env, count: usize) {
    sql(e,&format!("WITH RECURSIVE nums(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM nums WHERE i<{count})
        INSERT INTO photos(id,source,name,display,md5,mtime,rotation)
        SELECT i,'local','synthetic','derivatives/'||i||'.jpg','m',1,0 FROM nums;
        INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error,analyzed_at)
        SELECT id,display,md5,mtime,printf('%064x',id),'','synthetic' FROM photos;"));
}

#[test]
fn cleanup_chunk_failure_rolls_back_only_that_chunk_and_restart_repairs_lineage() {
    let lab = Lab::new();
    let e = lab.env("cleanup-partial");
    synthetic_targets(&e, 450);
    sql(&e,"CREATE TRIGGER fail_middle BEFORE INSERT ON ai_jobs WHEN (SELECT COUNT(*) FROM ai_jobs)>=225
        BEGIN SELECT RAISE(ABORT,'synthetic-middle-chunk'); END;");
    struct GuardedCleanup(Arc<dyn Handler>);
    impl Handler for GuardedCleanup {
        fn kind(&self) -> JobKind {
            self.0.kind()
        }
        fn claim_scope(&self) -> jobs::ClaimScope {
            self.0.claim_scope()
        }
        fn reconcile(&self, _: &Connection, _: i64) -> rusqlite::Result<Vec<JobKey>> {
            panic!("generic reconcile must be skipped after a middle cleanup chunk fails")
        }
        fn reconcile_cleanup(&self, c: &Connection, now: i64) -> rusqlite::Result<Vec<Persist>> {
            self.0.reconcile_cleanup(c, now)
        }
        fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
            self.0.run(ctx, job)
        }
        fn recheck(&self, c: &Connection, k: &JobKey) -> rusqlite::Result<Recheck> {
            self.0.recheck(c, k)
        }
    }
    let h = Arc::new(GuardedCleanup(handlers(&e).remove(0)));
    Core::new(&e.deps(vec![h], &StopFlag::new(), "middle-cleanup-failure")).cycle();
    // register + 199 targets committed in chunk 1. The whole second chunk rolled back,
    // and no generic enqueue could bypass the failed cleanup to create remaining rows.
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 199);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_image_embedding_lineage"),
        199
    );
    assert_eq!(lab.posts().len(), 0);
    sql(
        &e,
        "DROP TRIGGER fail_middle;
        UPDATE ai_jobs SET state='failed',terminal=1,attempts=5,defer_count=7,
            interruption_count=3,last_error_code='timeout',next_attempt_at=9999999999;",
    );
    let h = handlers(&e).remove(0);
    cleanup(&e, &h, T0);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 450);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_image_embedding_lineage"),
        450
    );
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE terminal=1 AND attempts=5 AND defer_count=7 AND interruption_count=3 AND next_attempt_at=9999999999 AND last_error_code='timeout'"
        ),
        199
    );
}

#[test]
fn stop_between_cleanup_chunks_preserves_committed_history_and_next_cycle_repairs() {
    // Stop from SQLite's COMMIT trace of the actual worker's first cleanup chunk.
    // No timing race and no fabricated manual chunk execution.
    static STOP: Mutex<Option<StopFlag>> = Mutex::new(None);
    fn trace(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event
            && sql == "COMMIT"
            && let Some(stop) = STOP.lock().unwrap().as_ref()
        {
            stop.stop();
        }
    }
    let lab = Lab::new();
    let e = lab.env("cleanup-stop");
    synthetic_targets(&e, 450);
    let stop = StopFlag::new();
    *STOP.lock().unwrap() = Some(stop.clone());
    e.writer()
        .call(|c| {
            c.trace_v2(
                rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
                Some(trace),
            );
            Ok(())
        })
        .unwrap();
    Core::new(&e.deps(handlers(&e), &stop, "stopped-cleanup")).cycle();
    e.writer()
        .call(|c| {
            c.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
            Ok(())
        })
        .unwrap();
    *STOP.lock().unwrap() = None;
    assert!(stop.is_stopped());
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 199);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='pending' AND claim_token IS NULL"
        ),
        199
    );
    assert!(lab.posts().is_empty());
    let h = handlers(&e).remove(0);
    cleanup(&e, &h, T0);
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 450);
    assert_eq!(
        n(&e, "SELECT COUNT(*) FROM ai_image_embedding_lineage"),
        450
    );
}

#[test]
fn scale_6500_reports_bounded_writer_chunks_and_bulk_failure_queries() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SQL: AtomicUsize = AtomicUsize::new(0);
    static FAIL: AtomicUsize = AtomicUsize::new(0);
    fn trace(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event {
            SQL.fetch_add(1, Ordering::SeqCst);
            if sql.starts_with("SELECT target_key,input_hash,last_error_code") {
                FAIL.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let lab = Lab::new();
    let e = lab.env("cleanup-scale-6500");
    synthetic_targets(&e, 6500);
    let h = handlers(&e).remove(0);
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let mut actions = h.reconcile_cleanup(&read, T0).unwrap().into_iter();
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;
    let mut chunks = 0;
    // Measure without SQL tracing overhead. Actual worker groups at most 200 actions.
    loop {
        let chunk: Vec<_> = actions.by_ref().take(200).collect();
        if chunk.is_empty() {
            break;
        }
        let elapsed = e
            .writer()
            .call(move |c| {
                let start = std::time::Instant::now();
                let tx = c.transaction()?;
                for action in chunk {
                    action(&tx)?;
                }
                tx.commit()?;
                Ok(start.elapsed())
            })
            .unwrap();
        total += elapsed;
        max = max.max(elapsed);
        chunks += 1;
    }
    assert_eq!(n(&e, "SELECT COUNT(*) FROM ai_jobs"), 6500);
    assert_eq!(chunks, 33);
    // While genuinely missing vectors remain, maintenance performs a single read query
    // and submits no writer closure. At completion, scan classifies all failed SHAs in 1 SQL.
    assert!(h.maintenance(&read, T0).unwrap().is_none());
    sql(
        &e,
        "UPDATE ai_jobs SET state='failed',terminal=1,last_error_code='input_invalid'",
    );
    let action = h.maintenance(&read, T0).unwrap().unwrap();
    let elapsed = e
        .writer()
        .call(move |c| {
            c.trace_v2(
                rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
                Some(trace),
            );
            SQL.store(0, Ordering::SeqCst);
            FAIL.store(0, Ordering::SeqCst);
            let start = std::time::Instant::now();
            action(c)?;
            let elapsed = start.elapsed();
            c.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
            Ok(elapsed)
        })
        .unwrap();
    let full_sql = SQL.load(Ordering::SeqCst);
    let failed_sql = FAIL.load(Ordering::SeqCst);
    assert_eq!(failed_sql, 1);
    assert_eq!(pointer(&e), None); // all failed never publishes
    eprintln!(
        "6500 synthetic targets, debug={}: cleanup {} writer chunks total {:?}, max {:?}; all-failed activation writer {:?}, SQL {}, failed SELECTs {}; pending maintenance writer closures 0",
        cfg!(debug_assertions),
        chunks,
        total,
        max,
        elapsed,
        full_sql,
        failed_sql
    );
}

#[test]
fn lineage_migration_reports_one_additive_table_and_preserves_existing_jobs() {
    let lab = Lab::new();
    let e = lab.env("lineage-migration");
    photo(&e, 1, &jpeg(29));
    let (_, job) = prepare(&e);
    e.writer()
        .call(move |c| {
            jobs::record_failure(c, &job, T0, ErrorCode::InputInvalid, Failure::Permanent)
        })
        .unwrap();
    let before = dump(&e, "ai_jobs");
    sql(&e, "DROP TABLE ai_image_embedding_lineage");
    e.writer()
        .call(|c| {
            assert_eq!(
                crate::ai::pending_active(c, false, false, false)?,
                vec!["ai_image_embedding_lineage"]
            );
            assert_eq!(jobs::migrate(c)?, vec!["ai_image_embedding_lineage"]);
            let changes = c.total_changes();
            assert!(jobs::migrate(c)?.is_empty());
            assert_eq!(c.total_changes(), changes);
            Ok(())
        })
        .unwrap();
    assert_eq!(before, dump(&e, "ai_jobs"));
    let h = handlers(&e).remove(0);
    cleanup(&e, &h, T0);
    assert_eq!(
        n(
            &e,
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND terminal=1"
        ),
        1
    );
    assert!(lab.posts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_worker_connects_images_local_cycles_e5_e6_and_local_advances_during_slow_provider()
 {
    use crate::ai::{groups, query, search_view};
    use std::time::Instant;
    let lab = Lab::new();
    let e = lab.env("scene-production-e2e");
    photo(&e, 1, &jpeg(41));
    photo(&e, 2, &jpeg(42));
    sql(
        &e,
        "UPDATE photos SET lat=1,lon=2,hidden=0,favorite=0; UPDATE photo_quality_features SET sharpness=5,luminance=128,exposure_score=90,black_clip=0,white_clip=0,contrast=100",
    );
    let lan = LanClient::new(&e.config.ai).unwrap();
    let stop = StopFlag::new();
    let started = worker::startup(&e.config, &e.writer(), &e.clock()).unwrap();
    let handlers = worker::production_handlers_for_test(&e.config, Some(lan.clone()));
    assert_eq!(handlers.len(), 1);
    let background = worker::spawn(e.deps(handlers, &stop, &started.run_id));
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if search_view::status(&read, &e.config).unwrap()["pipeline"]["scenes"]["local_ranked"] == 1
        {
            break;
        }
        assert!(Instant::now() < until, "production scene cycle timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(lab.posts().len(), 2);
    let catalog = crate::db::Catalog::open(&e.config.db_path())
        .unwrap()
        .with_writer(e.writer());
    let rt = Arc::new(query::Runtime::new(
        &e.config,
        Some(lan.clone()),
        stop.clone(),
        None,
    ));
    let view = rt
        .search(e.config.clone(), catalog.clone(), Some("q="))
        .await
        .unwrap();
    assert_eq!(view["groups"].as_array().unwrap().len(), 1);
    assert_eq!(view["groups"][0]["kind"], "scene");
    assert_eq!(lab.posts().len(), 2); // Empty search makes no probe or POST.
    let semantic = rt
        .search(e.config.clone(), catalog.clone(), Some("q=synthetic"))
        .await
        .unwrap();
    assert_eq!(semantic["mode"], "semantic");
    assert_eq!(semantic["groups"][0]["photos"].as_array().unwrap().len(), 2);
    assert_eq!(lab.posts().len(), 3);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Arc::new(Mutex::new(release_rx));
    lab.hook(move |r| {
        if r.request_line.starts_with("POST ") {
            entered_tx.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
    });
    photo(&e, 3, &jpeg(43));
    sql(
        &e,
        "UPDATE photos SET taken_at='2026-01-03 03:04' WHERE id=3",
    );
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    sql(&e, "UPDATE photos SET favorite=1 WHERE id=1");
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let prepared = groups::runtime::Prepared::read(&read, &e.config).unwrap();
        let tx = read.unchecked_transaction().unwrap();
        let current = prepared.load_in(&tx).unwrap();
        tx.commit().unwrap();
        if current
            .groups()
            .iter()
            .any(|g| g.members().iter().any(|m| m.photo_id == 1 && m.favorite))
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "local worker was blocked by provider"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(crate::photos::list_photos(&read).unwrap().len(), 2); // third has no location
    assert_eq!(
        search_view::status(&read, &e.config).unwrap()["pipeline"]["scenes"]["local_ranked"],
        1
    );
    stop.stop();
    release_tx.send(()).unwrap();
    assert!(background.shutdown(&e.writer(), &stop, Duration::from_secs(2), &e.clock()));
}
