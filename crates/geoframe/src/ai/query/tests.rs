use super::*;
use crate::ai::embedding::tests::{config_json, models};
use crate::ai::http::Endpoint;
use crate::ai::settings::Normalization;
use crate::ai::tests_http::{Captured, Stub, respond, stub};
use crate::ai::{activation, embedding, index, pause, search_view, space, waits};
use crate::config::Overrides;
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};
use rusqlite::Connection;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

const SQL: &str = include_str!("../../../tests/fixtures/ai-search-catalog.sql");
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/ai-search-python.json"
    ))
    .unwrap()
}

struct Env {
    dir: TempDir,
    config: Arc<Config>,
    catalog: Catalog,
    conn: Connection,
    runtime: Arc<Runtime>,
    lan: Option<LanClient>,
}
impl Env {
    fn new(base: Option<&str>, vars: &[(&str, &str)], active: bool) -> Self {
        let dir = TempDir::new("query-http");
        let path = dir.path().join("catalog.db");
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(SQL).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        let mut env = std::collections::HashMap::<String, String>::new();
        env.insert("GEOFRAME_AI_SEARCH_PER_MIN".into(), "1000".into());
        if let Some(base) = base {
            let p = dir.path().join("ai.json");
            std::fs::write(&p, config_json(base)).unwrap();
            env.insert("GEOFRAME_AI_CONFIG".into(), p.to_str().unwrap().into());
        }
        for (k, v) in vars {
            env.insert(k.to_string(), v.to_string());
        }
        let config = Arc::new(
            Config::load(
                |k| env.get(k).cloned(),
                &Overrides {
                    data_dir: Some(dir.path().into()),
                    ..Overrides::default()
                },
            )
            .unwrap(),
        );
        if active {
            let identity =
                embedding::configured_identity(&config.ai, TaskKind::QueryEmbedding).unwrap();
            activate(&mut conn, &identity);
        }
        let catalog = Catalog::open(&path).unwrap();
        let lan = LanClient::new(&config.ai);
        let runtime = Arc::new(Runtime::new(&config, lan.clone(), StopFlag::new(), None));
        Self {
            dir,
            config,
            catalog,
            conn,
            runtime,
            lan,
        }
    }
    async fn search(&self, q: &str) -> Result<Value, Error> {
        self.runtime
            .search(Arc::clone(&self.config), self.catalog.clone(), Some(q))
            .await
    }
    fn writer(&mut self) -> Writer {
        let writer =
            Writer::open(&self.dir.path().join("catalog.db"), OpenMode::MustExist).unwrap();
        self.catalog = self.catalog.clone().with_writer(writer.clone());
        writer
    }
}
fn activate(conn: &mut Connection, identity: &space::SpaceIdentity) {
    crate::ai::migrate(conn).unwrap();
    index::register_space(conn, identity, 1).unwrap();
    let rows: Vec<(String, Vec<u8>)> = {
        let mut s = conn
            .prepare("SELECT content_sha256,embedding FROM photo_ai_features ORDER BY photo_id")
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    for (sha, blob) in rows {
        let mut v = vec![0.0; 768];
        for (i, b) in blob.as_chunks::<4>().0.iter().enumerate() {
            v[i] = f32::from_le_bytes(*b);
        }
        index::store_vector(conn, identity, &sha, &sha, &v, 1).unwrap();
    }
    let snapshot = activation::snapshot_targets(conn, identity).unwrap();
    assert!(
        activation::try_activate_default(conn, &snapshot, identity)
            .unwrap()
            .published()
    );
}
fn standard(stream: &mut std::net::TcpStream, r: &Captured) {
    let path = r.request_line.split_whitespace().nth(1).unwrap();
    let body = match path {
        "/llm/catalog" => {
            json!({"services":[{"id":"embed","available":true,"apis":[{"method":"GET","path":"/v1/models"},{"method":"POST","path":"/v1/embeddings"}]}]})
        }
        "/llm/services/embed" => {
            json!({"id":"embed","state":"running","available":true,"lease_count":0})
        }
        "/llm-embed/v1/models" => models(),
        "/llm-embed/v1/embeddings" => {
            let mut v = vec![0.; 768];
            v[0] = 1.;
            json!({"model":embedding::MODEL,"object":"list","usage":{},"data":[{"index":0,"embedding":v}]})
        }
        _ => panic!("unexpected synthetic endpoint"),
    };
    respond(stream, "200 OK", "", &serde_json::to_vec(&body).unwrap());
}
fn base(stub: &Stub) -> String {
    format!("http://{}", stub.addr)
}
fn posts(stub: &Stub) -> usize {
    stub.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.request_line.starts_with("POST "))
        .count()
}

#[tokio::test]
async fn python_fixture_recommended_and_semantic_all_card_and_group_fields() {
    let stub = stub(standard);
    let e = Env::new(Some(&base(&stub)), &[], true);
    for case in fixture()["cases"].as_array().unwrap() {
        let q = format!(
            "q={}&groups={}&include_hidden={}",
            pyquery::py_quote(case["query"].as_str().unwrap()),
            case["groups"],
            case["hidden"]
        );
        assert_eq!(e.search(&q).await.unwrap(), case["result"]);
    }
    assert_eq!(posts(&stub), 3); // distinct normalized semantic queries; empty queries never send
}
#[tokio::test]
async fn legacy_empty_query_matches_python_without_settings_provider_or_new_tables() {
    let e = Env::new(None, &[], false);
    let before = e.conn.total_changes();
    for case in fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["query"] == "")
    {
        let q = format!(
            "groups={}&include_hidden={}",
            case["groups"], case["hidden"]
        );
        assert_eq!(e.search(&q).await.unwrap(), case["result"]);
    }
    let mut actual = search_view::status(&e.conn, &e.config).unwrap();
    actual.as_object_mut().unwrap().remove("pipeline");
    let mut expected = fixture()["status"].clone();
    expected.as_object_mut().unwrap().remove("pipeline");
    assert_eq!(actual, expected);
    assert_eq!(e.conn.total_changes(), before);
    assert_eq!(e.search("q=x").await, Err(Error::Unavailable));
    e.conn
        .execute_batch("DELETE FROM photo_ai_features")
        .unwrap();
    assert_eq!(e.search("").await.unwrap()["mode"], "empty");
    assert_eq!(e.search("q=x").await, Err(Error::Unavailable)); // explicit P4 difference from Python's early empty
}
#[tokio::test]
async fn parser_unicode_groups_duplicates_query_limit_and_fixed_errors() {
    let e = Env::new(None, &[], false);
    for q in [
        "groups=+１_２",
        "groups=&groups=2",
        "groups=999999999999999999999",
        "groups=-１",
        "groups=1&groups=bad",
    ] {
        assert!(e.search(q).await.is_ok(), "{q}");
    }
    for q in [
        "groups=bad",
        "groups=1.5",
        "groups=1__2",
        "groups=%1c1",
        "groups=1&groups=2&groups=bad&q="
            .replace("groups=1&", "")
            .as_str(),
    ] {
        if q.starts_with("groups=2") {
            continue;
        }
        assert_eq!(e.search(q).await, Err(Error::BadParams));
    }
    assert_eq!(
        e.search(&format!("q={}", "猫".repeat(301))).await,
        Err(Error::BadParams)
    );
    assert_eq!(
        e.search(&format!("q={}", "猫".repeat(300))).await,
        Err(Error::Unavailable)
    );
    let v = e
        .search("q=%1c%1f%20%E3%80%80&include_hidden=TRUE")
        .await
        .unwrap();
    assert_eq!(v["query"], "");
    assert_eq!(v["include_hidden"], false);
    assert_eq!(
        pyquery::py_casefold("Straße İ Σς Ꭰꭰ ﬃ"),
        "strasse i\u{307} σσ ᎠᎠ ffi"
    );
}
#[tokio::test]
async fn monotonic_rate_boundary_counts_empty_hits_and_invalid_query_after_disabled_order() {
    let mut e = Env::new(None, &[("GEOFRAME_AI_SEARCH_PER_MIN", "2")], false);
    let time = Arc::new(AtomicU64::new(0));
    let t = Arc::clone(&time);
    Arc::get_mut(&mut e.runtime).unwrap().now =
        Arc::new(move || Duration::from_millis(t.load(Ordering::SeqCst)));
    assert!(e.search("").await.is_ok());
    assert_eq!(
        e.search(&format!("q={}", "x".repeat(301))).await,
        Err(Error::BadParams)
    );
    assert_eq!(e.search("").await, Err(Error::RateLimited));
    time.store(59999, Ordering::SeqCst);
    assert_eq!(e.search("").await, Err(Error::RateLimited));
    time.store(60000, Ordering::SeqCst);
    assert!(e.search("").await.is_ok());
    let e = Env::new(None, &[("GEOFRAME_AI_ENABLED", "0")], false);
    assert_eq!(e.search("groups=bad").await, Err(Error::BadParams));
    assert_eq!(e.search("q=x").await, Err(Error::Disabled));
    assert!(e.runtime.memory.lock().unwrap().window.is_empty());
}
#[tokio::test]
async fn lru64_casefold_hit_stopped_provider_and_space_isolation() {
    let stub = stub(standard);
    let e = Env::new(Some(&base(&stub)), &[], true);
    e.search("q=Stra%C3%9Fe").await.unwrap();
    let requests = stub.requests.lock().unwrap().len();
    e.search("q=STRASSE").await.unwrap();
    assert_eq!(stub.requests.lock().unwrap().len(), requests);
    for i in 0..64 {
        e.search(&format!("q=synthetic-{i}")).await.unwrap();
    }
    assert_eq!(e.runtime.memory.lock().unwrap().vectors.len(), 64);
    assert_eq!(posts(&stub), 65);
    e.search("q=Stra%C3%9Fe").await.unwrap();
    assert_eq!(posts(&stub), 66);
    e.runtime.stop.stop();
    e.search("q=STRASSE").await.unwrap();
    assert_eq!(posts(&stub), 66);
    assert_eq!(e.search("q=new").await, Err(Error::Unavailable));
    e.conn
        .execute(
            "UPDATE settings SET value='other' WHERE key='ai_active_space'",
            [],
        )
        .unwrap();
    assert_eq!(e.search("q=STRASSE").await, Err(Error::Unavailable));
    assert_eq!(posts(&stub), 66);
}
#[tokio::test]
async fn busy_lane_concurrent_misses_and_background_lane_are_independent() {
    let stub = stub(standard);
    let e = Env::new(Some(&base(&stub)), &[], true);
    let bg = e
        .lan
        .as_ref()
        .unwrap()
        .http()
        .session(Lane::Background, Duration::ZERO, &e.runtime.stop)
        .unwrap();
    assert!(e.search("q=x").await.is_ok());
    let ia = e
        .lan
        .as_ref()
        .unwrap()
        .http()
        .session(Lane::Interactive, Duration::ZERO, &e.runtime.stop)
        .unwrap();
    assert_eq!(e.search("q=miss").await, Err(Error::Unavailable));
    assert!(e.search("q=x").await.is_ok());
    drop(ia);
    drop(bg);
    let before = posts(&stub);
    let (a, b) = tokio::join!(e.search("q=concurrent"), e.search("q=concurrent"));
    assert!(a.is_ok() || b.is_ok());
    assert_eq!(posts(&stub), before + 1);
}
#[tokio::test]
async fn writer_busy_does_not_block_lan_and_ingest0_has_no_db_writes() {
    let stub = stub(standard);
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    let before = e.conn.total_changes();
    e.search("q=readonly").await.unwrap();
    assert_eq!(e.conn.total_changes(), before);
    let writer = e.writer();
    let (entered, started) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        writer
            .call(move |_| {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), e.search("q=busy-writer")).await;
    release.send(()).unwrap();
    thread.join().unwrap();
    assert!(result.unwrap().is_ok());
}
#[tokio::test]
async fn query_wait_and_pause_shared_with_background_and_persisted_when_writer_available() {
    for (status, error) in [
        (
            "429 Too Many Requests",
            ProviderError::wait(waits::WaitScope::Entrance, waits::WaitReason::RateLimited),
        ),
        (
            "401 Unauthorized",
            ProviderError::Pause(
                pause::PauseScope::Provider,
                pause::PauseReason::Unauthorized,
            ),
        ),
    ] {
        let stub = stub(move |s, r| {
            if r.request_line.starts_with("POST ") {
                respond(s, status, "Retry-After: 120\r\n", b"")
            } else {
                standard(s, r)
            }
        });
        let mut e = Env::new(Some(&base(&stub)), &[], true);
        e.writer();
        assert_eq!(e.search("q=failed").await, Err(Error::Unavailable));
        let requests = stub.requests.lock().unwrap().len();
        assert_eq!(e.search("q=blocked").await, Err(Error::Unavailable));
        assert_eq!(stub.requests.lock().unwrap().len(), requests);
        assert!(
            e.lan
                .as_ref()
                .unwrap()
                .check_shared(
                    &e.config.ai,
                    TaskKind::ImageEmbedding,
                    crate::ai::jobs::unix_now()
                )
                .is_err()
        );
        match error {
            ProviderError::Wait(_) => assert!(
                !waits::load(&e.conn, crate::ai::jobs::unix_now())
                    .unwrap()
                    .is_empty()
            ),
            _ => assert!(
                pause::status(
                    &e.conn,
                    e.config.ai.settings().unwrap(),
                    TaskKind::ImageEmbedding
                )
                .unwrap()
                .is_some()
            ),
        }
        assert!(e.runtime.memory.lock().unwrap().vectors.is_empty());
        assert!(e.search("").await.is_ok());
    }
}
#[tokio::test]
async fn after_discovery_policy_recheck_blocks_post_and_cache_hit_ignores_pause() {
    let db_path = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let path = Arc::clone(&db_path);
    let stub = stub(move |s, r| {
        if r.request_line.contains("/v1/models ") {
            let c = Connection::open(path.lock().unwrap().as_ref().unwrap()).unwrap();
            c.execute(
                "UPDATE settings SET value='switched' WHERE key='ai_active_space'",
                [],
            )
            .unwrap();
        }
        standard(s, r)
    });
    let e = Env::new(Some(&base(&stub)), &[], true);
    *db_path.lock().unwrap() = Some(e.dir.path().join("catalog.db"));
    assert_eq!(e.search("q=never-send").await, Err(Error::Unavailable));
    assert_eq!(posts(&stub), 0);
}
#[tokio::test]
async fn generation_and_flags_read_after_network_do_not_reuse_old_rows_or_judgment() {
    let path = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let p = Arc::clone(&path);
    let stub = stub(move |s, r| {
        if r.request_line.starts_with("POST ") {
            let c = Connection::open(p.lock().unwrap().as_ref().unwrap()).unwrap();
            c.execute_batch("UPDATE photos SET md5='changed' WHERE id=1; DELETE FROM photos WHERE id=2; UPDATE photos SET hidden=1 WHERE id=3; UPDATE photos SET favorite=1,lat=1.0,lon=2.0,loc_source='manual' WHERE id=6; UPDATE photo_ai_features SET content_sha256='stale' WHERE photo_id=8;").unwrap();
        }
        standard(s, r)
    });
    let e = Env::new(Some(&base(&stub)), &[], true);
    *path.lock().unwrap() = Some(e.dir.path().join("catalog.db"));
    let result = e.search("q=synthetic").await.unwrap();
    let photos: Vec<_> = result["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["photos"].as_array().unwrap())
        .collect();
    let ids: Vec<_> = photos.iter().map(|p| p["id"].as_i64().unwrap()).collect();
    for id in [1, 2, 3, 4, 7] {
        assert!(!ids.contains(&id));
    }
    let p = photos.iter().find(|p| p["id"] == 6).unwrap();
    assert_eq!(p["lat"], 1.0);
    assert_eq!(p["favorite"], true);
    let p = photos.iter().find(|p| p["id"] == 8).unwrap();
    assert_eq!(p["keep_score"], Value::Null);
    assert_eq!(p["subject"], "");
    assert_eq!(
        e.conn
            .query_row("SELECT loc_source FROM photos WHERE id=6", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "manual"
    );
}
#[tokio::test]
async fn active_switch_during_post_rejects_result_without_old_vector_fallback() {
    let path = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let p = Arc::clone(&path);
    let stub = stub(move |s, r| {
        if r.request_line.starts_with("POST ") {
            let c = Connection::open(p.lock().unwrap().as_ref().unwrap()).unwrap();
            c.execute(
                "UPDATE settings SET value='different' WHERE key='ai_active_space'",
                [],
            )
            .unwrap();
        }
        standard(s, r)
    });
    let e = Env::new(Some(&base(&stub)), &[], true);
    *path.lock().unwrap() = Some(e.dir.path().join("catalog.db"));
    assert_eq!(e.search("q=x").await, Err(Error::Unavailable));
    assert!(e.search("").await.is_ok());
}
#[test]
fn status_counts_actual_index_corrupt_pending_and_selection_without_raw_reasons_or_space() {
    let mut e = Env::new(Some("http://127.0.0.1:1"), &[], true);
    let status = search_view::status(&e.conn, &e.config).unwrap();
    assert_eq!(status["photos"], 8);
    assert_eq!(status["judged"], 2);
    e.conn
        .execute(
            "UPDATE ai_embedding_vectors SET vector=zeroblob(3072) WHERE input_sha256=?1",
            [format!("{:064x}", 1)],
        )
        .unwrap();
    e.conn
        .execute(
            "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
            [format!("{:064x}", 2)],
        )
        .unwrap();
    let status = search_view::status(&e.conn, &e.config).unwrap();
    assert_eq!(status["photos"], 6);
    assert_eq!(status["pipeline"]["corrupt"], 1);
    assert_eq!(status["pipeline"]["pending"], 2);
    assert!(!status.to_string().contains("eg2-test-v1"));
    let identity = embedding::configured_identity(&e.config.ai, TaskKind::QueryEmbedding).unwrap();
    let key = index::scan(&e.conn, Some(&identity), false)
        .unwrap()
        .into_iter()
        .find(|s| s.input_sha256() == format!("{:064x}", 2))
        .unwrap()
        .job_key(&identity);
    crate::ai::jobs::enqueue(&mut e.conn, &key, 1).unwrap();
    e.conn.execute("UPDATE ai_jobs SET state='failed',terminal=1,last_error_code='synthetic-private-reason' WHERE target_key=?1",[&key.target_key]).unwrap();
    let failed = search_view::status(&e.conn, &e.config).unwrap();
    assert_eq!(failed["pipeline"]["failed_excluded"], 1);
    assert_eq!(failed["pipeline"]["pending"], 1);
    assert_eq!(failed["pipeline"]["failed_reasons"], json!({"other":1}));
    assert!(!failed.to_string().contains("synthetic-private-reason"));
    e.conn
        .execute("UPDATE photos SET mtime=2 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        search_view::status(&e.conn, &e.config).unwrap()["judged"],
        1
    );
    activation::deactivate(&mut e.conn).unwrap();
    assert_eq!(
        search_view::status(&e.conn, &e.config).unwrap()["pipeline"]["index_state"],
        "unset"
    );
}
#[test]
fn identity_full_metadata_isolation_and_visual_does_not_mix_legacy_vectors() {
    let mut e = Env::new(Some("http://127.0.0.1:1"), &[], true);
    let identity = embedding::configured_identity(&e.config.ai, TaskKind::QueryEmbedding).unwrap();
    let other = space::SpaceIdentity::new(
        "different",
        "provider",
        "model",
        "rev",
        "image",
        "text",
        768,
        Normalization::L2,
    )
    .unwrap();
    let query = QueryVector::new(&other, &vec![1.; 768]).unwrap();
    assert_eq!(
        search_view::search(
            &e.conn,
            &e.config,
            "x",
            12,
            false,
            Some(&query),
            &Deadline::interactive()
        )
        .unwrap(),
        Err(Error::Unavailable)
    );
    // Make the two legacy vectors close but the active vectors orthogonal: visual count must be two.
    let mut v = vec![0.; 768];
    v[2] = 1.;
    e.conn
        .execute(
            "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
            [format!("{:064x}", 2)],
        )
        .unwrap();
    index::store_vector(
        &mut e.conn,
        &identity,
        &format!("{:064x}", 2),
        &format!("{:064x}", 2),
        &v,
        2,
    )
    .unwrap();
    let result = search_view::search(
        &e.conn,
        &e.config,
        "",
        12,
        false,
        None,
        &Deadline::interactive(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        result["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["group_id"] == "scene-a")
            .unwrap()["visual_cluster_count"],
        2
    );
}

#[tokio::test]
async fn one_deadline_covers_post_timeout_reprobe_and_does_not_cache_failure() {
    let stub = stub(|s, r| {
        if r.request_line.starts_with("POST ") {
            std::thread::sleep(Duration::from_millis(500));
        }
        standard(s, r)
    });
    let e = Env::new(Some(&base(&stub)), &[], true);
    let started = Instant::now();
    let result = e
        .runtime
        .search_until(
            Arc::clone(&e.config),
            e.catalog.clone(),
            "timeout".into(),
            12,
            false,
            Deadline::after(Duration::from_millis(240)),
        )
        .await;
    assert_eq!(result, Err(Error::Unavailable));
    assert!(started.elapsed() < Duration::from_millis(450));
    assert_eq!(posts(&stub), 1);
    assert_eq!(
        stub.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.request_line.contains("/v1/models "))
            .count(),
        2
    );
    assert!(e.runtime.memory.lock().unwrap().vectors.is_empty());
}

#[tokio::test]
async fn cancelled_outer_future_keeps_lane_and_lock_until_blocking_request_finishes() {
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = Arc::clone(&gate);
    let stub = stub(move |s, r| {
        if r.request_line.starts_with("POST ") {
            entered.send(()).unwrap();
            let (m, c) = &*release;
            let mut ready = m.lock().unwrap();
            while !*ready {
                ready = c.wait(ready).unwrap();
            }
        }
        standard(s, r)
    });
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    let lock = LockHold::for_test(e.dir.path());
    Arc::get_mut(&mut e.runtime).unwrap()._lock = Some(lock.clone());
    let (finished, done) = tokio::sync::oneshot::channel();
    *e.runtime.finished.lock().unwrap() = Some(finished);
    let (rt, catalog, config) = (
        Arc::clone(&e.runtime),
        e.catalog.clone(),
        Arc::clone(&e.config),
    );
    let handle = tokio::spawn(async move { rt.search(config, catalog, Some("q=held")).await });
    started.recv().await.unwrap();
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    assert_eq!(e.search("q=another").await, Err(Error::Unavailable));
    assert!(lock.holders() >= 2);
    // A queued background operation is not needed: an already executing background slot is independent.
    let bg = e
        .lan
        .as_ref()
        .unwrap()
        .http()
        .session(Lane::Background, Duration::ZERO, &e.runtime.stop)
        .unwrap();
    drop(bg);
    let (m, c) = &*gate;
    *m.lock().unwrap() = true;
    c.notify_all();
    done.await.unwrap();
    assert_eq!(posts(&stub), 1);
    assert!(e.search("q=held").await.is_ok());
}

#[tokio::test]
async fn database_queue_timeout_does_not_restart_deadline_or_send_later() {
    let stub = stub(standard);
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    e.catalog = Catalog::open_with_queue_timeout(
        &e.dir.path().join("catalog.db"),
        1,
        Duration::from_secs(1),
    )
    .unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let catalog = e.catalog.clone();
    let holding = tokio::spawn(async move {
        catalog
            .read(move |_| {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap()
    });
    started.await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        e.runtime.search_until(
            Arc::clone(&e.config),
            e.catalog.clone(),
            "queued".into(),
            12,
            false,
            Deadline::after(Duration::from_millis(50)),
        ),
    )
    .await;
    release.send(()).unwrap();
    holding.await.unwrap();
    assert!(result.is_err());
    assert_eq!(stub.requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn policy_disabled_invalid_unknown_contract_cloud_and_ai0_send_nothing() {
    let stub = stub(standard);
    for variant in ["invalid", "unknown", "cloud", "disabled"] {
        let mut e = Env::new(Some(&base(&stub)), &[], true);
        let p = e.dir.path().join("ai.json");
        let mut doc: Value = serde_json::from_str(&config_json(&base(&stub))).unwrap();
        match variant {
            "invalid" => std::fs::write(&p, b"{").unwrap(),
            "unknown" => {
                doc["embedding_space"]["revision"] = json!("unknown");
                std::fs::write(&p, doc.to_string()).unwrap();
            }
            "cloud" => {
                doc["tasks"]["query_embedding"] = json!({"provider":"cloud","model":"synthetic-cloud","allow_cloud":true,"daily_request_limit":100});
                std::fs::write(&p, doc.to_string()).unwrap();
            }
            _ => {}
        }
        e.config = Arc::new(
            Config::load(
                |k| match k {
                    "GEOFRAME_AI_CONFIG" => Some(p.to_str().unwrap().into()),
                    "GEOFRAME_AI_ENABLED" if variant == "disabled" => Some("0".into()),
                    _ => None,
                },
                &Overrides {
                    data_dir: Some(e.dir.path().into()),
                    ..Overrides::default()
                },
            )
            .unwrap(),
        );
        e.runtime = Arc::new(Runtime::new(
            &e.config,
            LanClient::new(&e.config.ai),
            StopFlag::new(),
            None,
        ));
        let before = e.conn.total_changes();
        assert!(matches!(
            e.search("q=never-send").await,
            Err(Error::Disabled | Error::Unavailable)
        ));
        assert_eq!(e.conn.total_changes(), before);
    }
    assert!(stub.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cache_hit_rate_accounting_and_paused_hit_skip_all_discovery() {
    let stub = stub(standard);
    let e = Env::new(
        Some(&base(&stub)),
        &[("GEOFRAME_AI_SEARCH_PER_MIN", "2")],
        true,
    );
    e.search("q=x").await.unwrap();
    let key = e
        .config
        .ai
        .settings()
        .unwrap()
        .provider_pause_key(TaskKind::QueryEmbedding)
        .unwrap();
    let mut c = Connection::open(e.dir.path().join("catalog.db")).unwrap();
    pause::pause(
        &mut c,
        TaskKind::QueryEmbedding,
        &key,
        pause::PauseReason::Unauthorized,
        1,
    )
    .unwrap();
    let before = stub.requests.lock().unwrap().len();
    assert!(e.search("q=X").await.is_ok());
    assert_eq!(stub.requests.lock().unwrap().len(), before);
    assert_eq!(e.search("q=x").await, Err(Error::RateLimited));
}

#[tokio::test]
async fn shared_wait_expiry_and_readonly_pause_without_writer_are_effective() {
    let stub = stub(standard);
    let e = Env::new(Some(&base(&stub)), &[], true);
    let now = crate::ai::jobs::unix_now();
    let lan = e.lan.as_ref().unwrap();
    lan.record_shared(
        &e.config.ai,
        TaskKind::QueryEmbedding,
        ProviderError::wait(waits::WaitScope::Entrance, waits::WaitReason::Leased),
        now,
    );
    assert!(
        lan.check_shared(&e.config.ai, TaskKind::ImageEmbedding, now + 59)
            .is_err()
    );
    assert!(
        lan.check_shared(&e.config.ai, TaskKind::ImageEmbedding, now + 60)
            .is_ok()
    );
    assert_eq!(e.search("q=wait").await, Err(Error::Unavailable));
    assert!(stub.requests.lock().unwrap().is_empty());
    assert!(
        e.conn
            .query_row("SELECT COUNT(*) FROM ai_provider_waits", [], |r| r
                .get::<_, i64>(0))
            .unwrap()
            == 0
    );
    assert!(e.search("").await.is_ok());
}

#[tokio::test]
async fn synthetic_6500x768_http_query_performance() {
    let stub = stub(standard);
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    let identity = embedding::configured_identity(&e.config.ai, TaskKind::QueryEmbedding).unwrap();
    let mut values = vec![0.0f32; 768];
    values[0] = 0.6;
    values[1] = 0.8;
    let blob: Vec<u8> = values.into_iter().flat_map(f32::to_le_bytes).collect();
    let tx = e.conn.transaction().unwrap();
    tx.execute_batch("DELETE FROM photo_ai_group_members;DELETE FROM photo_ai_groups;DELETE FROM photo_ai_features;DELETE FROM photo_quality;DELETE FROM photo_quality_features;DELETE FROM photos;DELETE FROM ai_embedding_vectors;").unwrap();
    {
        let mut photo=tx.prepare("INSERT INTO photos(id,source,name,display,md5,mtime,lat,lon) VALUES(?1,'local','synthetic','derivatives/synthetic.jpg','m',1,35,139)").unwrap();
        let mut feature=tx.prepare("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,analyzed_at) VALUES(?1,'derivatives/synthetic.jpg','m',1,?2,'fixture')").unwrap();
        let mut vector=tx.prepare("INSERT INTO ai_embedding_vectors(space_id,input_sha256,sent_sha256,dimension,vector,created_at) VALUES(?1,?2,?2,768,?3,1)").unwrap();
        for id in 1..=6500 {
            let sha = format!("{id:064x}");
            photo.execute([id]).unwrap();
            feature.execute(rusqlite::params![id, sha]).unwrap();
            vector
                .execute(rusqlite::params![identity.space_id(), sha, blob])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    fn rss() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }
    let addr = server(&e).await;
    let before = rss();
    let started = Instant::now();
    let (status, _, body) = request(
        addr,
        "GET",
        "/api/ai/search?q=synthetic-performance",
        "",
        "",
    )
    .await;
    assert_eq!(status, 200);
    let result: Value = serde_json::from_slice(&body).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(result["searched_photos"], 6500);
    assert_eq!(result["groups"].as_array().unwrap().len(), 12);
    assert!(elapsed < Duration::from_secs(15));
    let query = QueryVector::new(&identity, &{
        let mut v = vec![0.; 768];
        v[0] = 1.;
        v
    })
    .unwrap();
    let after_http = rss();
    static TX_STARTED: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    static TX_MICROS: AtomicU64 = AtomicU64::new(0);
    fn trace(event: rusqlite::trace::TraceEvent<'_>) {
        let rusqlite::trace::TraceEvent::Stmt(_, sql) = event else {
            return;
        };
        if sql.starts_with("BEGIN") {
            *TX_STARTED.lock().unwrap() = Some(Instant::now());
        }
        if sql == "COMMIT"
            && let Some(start) = TX_STARTED.lock().unwrap().take()
        {
            TX_MICROS.store(start.elapsed().as_micros() as u64, Ordering::SeqCst);
        }
    }
    e.conn.trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
        Some(trace),
    );
    let start = Instant::now();
    let direct = search_view::search(
        &e.conn,
        &e.config,
        "synthetic",
        12,
        false,
        Some(&query),
        &Deadline::interactive(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(direct["searched_photos"], 6500);
    eprintln!(
        "T44B_SYNTHETIC_6500 http_ms={} view_ms={} db_snapshot_us={} rss_before_kib={} rss_after_http_kib={} rss_after_view_kib={}",
        elapsed.as_millis(),
        start.elapsed().as_millis(),
        TX_MICROS.load(Ordering::SeqCst),
        before,
        after_http,
        rss()
    );
}

async fn server(e: &Env) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = crate::server::AppState::new(Arc::clone(&e.config), e.catalog.clone())
        .with_ai_search(Arc::clone(&e.runtime));
    tokio::spawn(async move {
        axum::serve(listener, crate::server::router(state))
            .await
            .unwrap()
    });
    addr
}
async fn request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    extra: &str,
    body: &str,
) -> (u16, String, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let text = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).await.unwrap();
    let mut data = Vec::new();
    stream.read_to_end(&mut data).await.unwrap();
    let i = data.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(data[..i].to_vec()).unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        head,
        data[i + 4..].to_vec(),
    )
}
#[tokio::test]
async fn http_exact_routes_head_guards_errors_headers_and_python_cards() {
    let e = Env::new(None, &[], false);
    let addr = server(&e).await;
    let (status, headers, body) = request(addr, "GET", "/api/ai/search", "", "").await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        fixture()["cases"][0]["result"]
    );
    assert!(headers.contains("application/json; charset=utf-8"));
    assert!(headers.contains("cache-control: no-store"));
    assert_eq!(request(addr, "GET", "/api/ai/status", "", "").await.0, 200);
    for path in ["/api/ai/status", "/api/ai/search"] {
        let head = request(addr, "HEAD", path, "", "").await;
        assert_eq!(head.0, 403);
        assert!(head.2.is_empty());
        assert_eq!(request(addr, "POST", path, "", "").await.0, 403);
    }
    for path in ["/api/ai/statusX", "/api/ai/search/", "/api/ai/unknown"] {
        assert_eq!(request(addr, "GET", path, "", "").await.0, 503);
    }
    assert_eq!(
        request(addr, "GET", "/%61pi/ai/status", "", "").await.0,
        403
    );
    assert_eq!(request(addr, "GET", "//api/ai/status", "", "").await.0, 200);
    assert_eq!(
        request(addr, "GET", "/api/ai/search?groups=bad", "", "")
            .await
            .0,
        400
    );
    assert_eq!(
        request(
            addr,
            "GET",
            "/api/ai/search?q=synthetic-private-query",
            "",
            ""
        )
        .await
        .0,
        503
    );
    assert_eq!(
        request(addr, "OPTIONS", "/api/ai/status", "", "").await.0,
        501
    );
    let (status, _, body) = request(
        addr,
        "GET",
        "/api/ai/search?q=synthetic-private-query",
        "Origin: http://evil.example\r\n",
        "",
    )
    .await;
    assert_eq!(status, 503);
    assert!(!String::from_utf8_lossy(&body).contains("synthetic-private-query"));
}
#[tokio::test]
async fn background_http_held_query_feed_status_and_setloc_respond_independently() {
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release = Arc::clone(&gate);
    let stub = stub(move |s, r| {
        if r.body == b"{\"background\":true}" {
            entered.send(()).unwrap();
            let (m, c) = &*release;
            let mut ready = m.lock().unwrap();
            while !*ready {
                ready = c.wait(ready).unwrap();
            }
        }
        standard(s, r)
    });
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    e.writer();
    let addr = server(&e).await;
    let bg = e
        .lan
        .as_ref()
        .unwrap()
        .http()
        .session(Lane::Background, Duration::ZERO, &e.runtime.stop)
        .unwrap();
    let holding = std::thread::spawn(move || {
        bg.post_json(
            Endpoint::Embeddings,
            b"{\"background\":true}",
            &Deadline::background(60),
        )
        .unwrap()
    });
    started.recv().await.unwrap();
    let all = tokio::time::timeout(Duration::from_secs(2), async {
        assert_eq!(
            request(addr, "GET", "/api/ai/search?q=independent", "", "")
                .await
                .0,
            200
        );
        for path in ["/api/v1/photos", "/api/ai/status", "/api/admin/status"] {
            assert_eq!(request(addr, "GET", path, "", "").await.0, 200);
        }
        let csrf = "Content-Type: application/json\r\nX-Requested-With: XMLHttpRequest\r\n";
        assert_eq!(
            request(
                addr,
                "POST",
                "/api/admin/setloc",
                csrf,
                "{\"id\":1,\"lat\":1,\"lon\":2}"
            )
            .await
            .0,
            200
        );
    })
    .await;
    let (m, c) = &*gate;
    *m.lock().unwrap() = true;
    c.notify_all();
    holding.join().unwrap();
    all.unwrap();
}

#[tokio::test]
async fn error_recording_busy_writer_is_bounded_and_keeps_shared_memory_wait() {
    let stub = stub(|s, r| {
        if r.request_line.starts_with("POST ") {
            respond(s, "429 Too Many Requests", "", b"")
        } else {
            standard(s, r)
        }
    });
    let mut e = Env::new(Some(&base(&stub)), &[], true);
    let writer = e.writer();
    let (entered, started) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let holding = std::thread::spawn(move || {
        writer
            .call(move |_| {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let elapsed = Instant::now();
    let result = e.search("q=writer-error").await;
    let blocked = e.search("q=shared-wait").await;
    release.send(()).unwrap();
    holding.join().unwrap();
    assert_eq!(result, Err(Error::Unavailable));
    assert_eq!(blocked, Err(Error::Unavailable));
    assert!(elapsed.elapsed() < Duration::from_secs(1));
    assert_eq!(posts(&stub), 1);
    assert_eq!(
        e.conn
            .query_row("SELECT COUNT(*) FROM ai_provider_waits", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        e.lan
            .as_ref()
            .unwrap()
            .check_shared(
                &e.config.ai,
                TaskKind::ImageEmbedding,
                crate::ai::jobs::unix_now()
            )
            .is_err()
    );
}

#[tokio::test]
async fn http_rate_429_disabled_503_empty_mode_and_fixed_error_bodies() {
    for (vars, wanted) in [
        (vec![("GEOFRAME_AI_SEARCH_PER_MIN", "0")], 429),
        (vec![("GEOFRAME_AI_ENABLED", "0")], 503),
    ] {
        let e = Env::new(None, &vars, false);
        let addr = server(&e).await;
        let (status, headers, body) = request(
            addr,
            "GET",
            "/api/ai/search?q=synthetic-private-query",
            "",
            "",
        )
        .await;
        assert_eq!(status, wanted);
        assert!(headers.contains("cache-control: no-store"));
        assert!(!String::from_utf8_lossy(&body).contains("synthetic-private-query"));
        assert_eq!(
            request(addr, "GET", "/api/ai/search?groups=bad", "", "")
                .await
                .0,
            400
        );
    }
    let e = Env::new(None, &[], false);
    e.conn.execute("DELETE FROM photo_ai_features", []).unwrap();
    let addr = server(&e).await;
    let (status, _, body) = request(addr, "GET", "/api/ai/search", "", "").await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"ok":true,"query":"","mode":"empty","groups":[],"searched_photos":0})
    );
    assert_eq!(
        request(addr, "GET", "http://evil.example/api/ai/status", "", "")
            .await
            .0,
        403
    );
    assert_eq!(
        request(addr, "GET", "/api/ai/status", "Host: evil.example\r\n", "")
            .await
            .0,
        400
    );
}

#[test]
fn http_logs_contain_only_route_status_timing_not_query_keys_vectors_or_errors() {
    let logs = crate::testutil::capture::logs_of(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let e = Env::new(None, &[], false);
            let addr = server(&e).await;
            assert_eq!(
                request(
                    addr,
                    "GET",
                    "/api/ai/search?q=synthetic-private-query",
                    "",
                    ""
                )
                .await
                .0,
                503
            );
            assert_eq!(request(addr, "GET", "/api/ai/status", "", "").await.0, 200);
        });
    });
    assert!(logs.contains("/api/ai/search"));
    assert!(!logs.contains("synthetic-private-query"));
    assert!(!logs.contains("gemini-embedding-2"));
    assert!(!logs.contains("derivatives/"));
}
