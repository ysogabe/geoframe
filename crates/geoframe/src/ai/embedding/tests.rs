use super::*;
use crate::ai::http::{AiHttp, HttpError};
use crate::ai::lanes::Lane;
use crate::ai::tests_http::{FakeDns, lab_http, respond, stub};
use crate::ai::tests_worker::env_with;
use crate::stop::StopFlag;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) fn config_json(base: &str) -> String {
    json!({"version":1,"local_base_url":base,"embedding_contract":CONTRACT,
        "embedding_space":{"space_id":"eg2-test-v1","model_id":MODEL,"revision":REVISION,
            "image_preprocess":IMAGE_PREPROCESS,"text_preprocess":TEXT_PREPROCESS,
            "dimension":DIMENSION,"normalization":"l2"},
        "tasks":{"image_embedding":{"provider":"local","model":MODEL,"service_id":SERVICE_ID},
            "query_embedding":{"provider":"local","model":MODEL,"service_id":SERVICE_ID}}})
    .to_string()
}
pub(crate) fn models() -> Value {
    json!({"object":"list","data":[{"id":MODEL,"object":"model","owned_by":"local",
        "modalities":["text","image"],"dimensions":768}]})
}
pub(crate) fn reply() -> Value {
    let mut v = vec![0.0; DIMENSION as usize];
    v[0] = 3.0;
    v[1] = 4.0;
    json!({"object":"list","model":MODEL,"usage":{"prompt_tokens":280,"total_tokens":280},
        "data":[{"object":"embedding","index":0,"embedding":v}]})
}
fn identity() -> SpaceIdentity {
    SpaceIdentity::new(
        "eg2-test-v1",
        PROVIDER_SPACE,
        MODEL,
        REVISION,
        IMAGE_PREPROCESS,
        TEXT_PREPROCESS,
        DIMENSION,
        Normalization::L2,
    )
    .unwrap()
}
#[test]
fn only_explicit_known_contract_and_exact_metadata_enable_the_adapter() {
    let good = config_json("http://127.0.0.1:1");
    let e = env_with("embed-contract", &good, true);
    for task in [TaskKind::ImageEmbedding, TaskKind::QueryEmbedding] {
        assert_eq!(configured_identity(&e.config.ai, task), Some(identity()));
    }
    // No inference or network. Explicit contract with a mismatched declaration is disabled.
    for (a, b) in [
        (CONTRACT, "unknown-v1"),
        (REVISION, "r2"),
        (IMAGE_PREPROCESS, "image-v2"),
        (TEXT_PREPROCESS, "text-v2"),
        (MODEL, "embeddinggemma-2-q8_0"),
        ("\"embed\"", "\"other\""),
        ("\"dimension\":768", "\"dimension\":767"),
        ("\"l2\"", "\"none\""),
    ] {
        let bad = good.replace(a, b);
        let e = env_with("embed-contract-bad", &bad, true);
        assert!(configured_identity(&e.config.ai, TaskKind::ImageEmbedding).is_none());
        assert!(configured_identity(&e.config.ai, TaskKind::QueryEmbedding).is_none());
    }
    let mut v: Value = serde_json::from_str(&good).unwrap();
    v.as_object_mut().unwrap().remove("embedding_contract");
    let e = env_with("embed-contract-unset", &v.to_string(), true);
    assert!(configured_identity(&e.config.ai, TaskKind::ImageEmbedding).is_none());
}
#[test]
fn settings_reject_unknown_and_duplicate_contract_without_printing_values() {
    use crate::ai::settings::{AiConfigError, AiSettings};
    let good = config_json("http://127.0.0.1:1");
    assert_eq!(
        AiSettings::parse(good.replace(CONTRACT, "secret-wrong").as_bytes()),
        Err(AiConfigError::BadValue)
    );
    let dup = good.replacen("{", &format!("{{\"embedding_contract\":\"{CONTRACT}\","), 1);
    assert_eq!(
        AiSettings::parse(dup.as_bytes()),
        Err(AiConfigError::DuplicateKey)
    );
    let unknown = good.replacen("{", "{\"secret-unknown\":true,", 1);
    assert_eq!(
        AiSettings::parse(unknown.as_bytes()),
        Err(AiConfigError::UnknownKey)
    );
    assert!(!format!("{:?}", AiSettings::parse(unknown.as_bytes())).contains("secret-unknown"));
}
#[test]
fn models_require_both_modalities_exact_model_and_dimension_without_duplicates() {
    let good = models();
    assert_eq!(check_models(&serde_json::to_vec(&good).unwrap()), Ok(()));
    for pointer in [
        "/data/0/id",
        "/data/0/modalities",
        "/data/0/dimensions",
        "/object",
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = json!(null);
        assert_eq!(
            check_models(&serde_json::to_vec(&bad).unwrap()),
            Err(ProviderError::contract(false))
        );
    }
    for mode in [json!(["text"]), json!(["image"]), json!(["image", 1])] {
        let mut bad = good.clone();
        bad["data"][0]["modalities"] = mode;
        assert!(check_models(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = good.clone();
    bad["data"]
        .as_array_mut()
        .unwrap()
        .push(good["data"][0].clone());
    assert!(check_models(&serde_json::to_vec(&bad).unwrap()).is_err());
}
#[test]
fn envelope_maps_indices_and_renormalizes_bf16_vectors() {
    let mut doc = reply();
    let vector = doc["data"][0].clone();
    let mut v = vector.clone();
    v["index"] = json!(1);
    v["embedding"][0] = json!(-3.0);
    doc["data"] = json!([v, vector]);
    let out = parse_reply(&serde_json::to_vec(&doc).unwrap(), &identity(), 2).unwrap();
    assert!((out[0][0] - 0.6).abs() < 1e-6);
    assert!((out[0][1] - 0.8).abs() < 1e-6);
    assert!((out[1][0] + 0.6).abs() < 1e-6);
    for row in out {
        assert!((row.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() - 1.0).abs() < 1e-6);
    }
}
#[test]
fn bad_model_indices_vectors_and_envelopes_are_rejected() {
    let good = reply();
    for (pointer, value) in [
        ("/data/0/index", json!(-1)),
        ("/data/0/index", json!(1)),
        ("/data/0/index", json!(0.5)),
        ("/data/0/embedding", json!([1.0])),
        ("/data/0/embedding", json!(vec![0.0; 768])),
        ("/data/0/embedding/0", json!(1e300)),
        ("/data/0/embedding/0", json!(null)),
        ("/data/0/embedding/0", json!("NaN")),
        ("/object", json!(null)),
        ("/usage", json!(null)),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            parse_reply(&serde_json::to_vec(&bad).unwrap(), &identity(), 1),
            Err(ProviderError::BadResponse)
        );
    }
    let mut bad = good.clone();
    bad["model"] = json!("embeddinggemma-2-q8_0");
    assert_eq!(
        parse_reply(&serde_json::to_vec(&bad).unwrap(), &identity(), 1),
        Err(ProviderError::contract(false))
    );
    let mut bad = good.clone();
    bad["data"]
        .as_array_mut()
        .unwrap()
        .push(good["data"][0].clone());
    assert!(parse_reply(&serde_json::to_vec(&bad).unwrap(), &identity(), 2).is_err());
    for bytes in [b"NaN".as_slice(), b"Infinity", b"{", b"[]"] {
        assert!(parse_reply(bytes, &identity(), 1).is_err());
    }
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut values = vec![1.0; 768];
        values[0] = v;
        assert!(QueryVector::new(&identity(), &values).is_err());
        assert!(
            crate::ai::index::PreparedVector::new(
                &identity(),
                &"a".repeat(64),
                &"b".repeat(64),
                &values
            )
            .is_err()
        );
    }
}
#[test]
fn four_mib_and_batch_limits_are_checked_before_dns_or_write() {
    let server = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let dns = FakeDns::new();
    let http = lab_http(server.addr, &dns);
    let stop = StopFlag::new();
    let session = http
        .session(Lane::Background, Duration::ZERO, &stop)
        .unwrap();
    assert_eq!(
        session.post_json(
            Endpoint::Embeddings,
            &vec![b' '; MAX_REQUEST_BYTES + 1],
            &Deadline::background(60)
        ),
        Err(HttpError::RequestTooLarge)
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.bytes.load(Ordering::SeqCst), 0);
    assert!(encode_inputs(vec![json!("a"); 9]).is_err());
    assert!(encode_inputs(vec![]).is_err());
    assert!(encode_inputs(vec![json!("a"); 8]).is_ok());
    assert!(encode_inputs(vec![json!("x".repeat(MAX_REQUEST_BYTES))]).is_err());
}
#[tokio::test]
async fn query_uses_prefix_fixed_endpoint_and_shared_interactive_lane() {
    let lab = crate::ai::image_embedding::tests::Lab::new();
    let e = lab.env("query-contract");
    let lan = LanClient::new(&e.config.ai).unwrap();
    let client = EmbeddingClient::new(&e.config.ai, lan.clone(), TaskKind::QueryEmbedding).unwrap();
    let stop = StopFlag::new();
    let result = lan
        .http()
        .run_interactive(&stop, move |session| {
            client.query(
                &session,
                "合成の検索語",
                &Deadline::interactive(),
                || Ok(()),
            )
        })
        .await
        .unwrap();
    assert!(result.is_ok());
    let posts = lab.posts();
    assert_eq!(posts.len(), 1);
    let body: Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(
        body["input"],
        json!([format!("{QUERY_PREFIX}合成の検索語")])
    );
    assert_eq!(body["encoding_format"], "float");
    assert_eq!(body["model"], MODEL);
    let _slot = lan
        .http()
        .session(Lane::Interactive, Duration::ZERO, &stop)
        .unwrap();
    assert_eq!(
        lan.http().run_interactive(&stop, |_| ()).await,
        Err(HttpError::Busy)
    );
}
#[test]
fn after_discovery_caller_can_deny_query_and_stopping_denies_first_write() {
    let lab = crate::ai::image_embedding::tests::Lab::new();
    let e = lab.env("query-final-gate");
    let lan = LanClient::new(&e.config.ai).unwrap();
    let client = EmbeddingClient::new(&e.config.ai, lan.clone(), TaskKind::QueryEmbedding).unwrap();
    let stop = StopFlag::new();
    let session = lan
        .http()
        .session(Lane::Interactive, Duration::ZERO, &stop)
        .unwrap();
    assert!(
        client
            .query(&session, "合成", &Deadline::interactive(), || Err(
                ProviderError::Stopping
            ))
            .is_err()
    );
    assert_eq!(lab.posts().len(), 0);
    // Reuse the HTTP gate's stop-at-first-write guarantee for this new endpoint too.
    let dns = FakeDns::new();
    let hook_stop = stop.clone();
    let http = AiHttp::with_lookup_and_connect_hook(
        &format!("http://{}", lab.stub.addr),
        Arc::new(dns.clone()),
        Arc::new(move || hook_stop.stop()),
    )
    .unwrap();
    dns.script("127.0.0.1", vec![vec![lab.stub.addr]]);
    let s = http
        .session(Lane::Background, Duration::ZERO, &stop)
        .unwrap();
    assert_eq!(
        s.post_json(Endpoint::Embeddings, b"{}", &Deadline::background(60)),
        Err(HttpError::Stopping)
    );
    assert_eq!(lab.posts().len(), 0);
}

/// POST waits until a subsequent catalog request releases it. Thus receiving the POST,
/// its timeout and the reprobe order are synchronized, with no server sleep/race.
#[test]
fn sent_post_timeout_reserves_reprobe_and_reaches_terminal_but_real_lease_waits() {
    use crate::ai::jobs::{self, Failure, JobKey, JobKind};
    use crate::ai::waits::{WaitReason, WaitScope};
    use crate::ai::worker::RunError;
    use std::sync::{Condvar, Mutex};
    for mode in ["available", "lease", "not_ready", "unreachable", "stopping"] {
        let stop = StopFlag::new();
        let post_stop = stop.clone();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let pending = release.clone();
        let server = stub(move |stream, req| {
            let line = &req.request_line;
            if line.starts_with("POST ") {
                if mode == "stopping" {
                    post_stop.stop();
                }
                let (lock, cv) = &*pending;
                let done = lock.lock().unwrap();
                // Upper bound only prevents a fixture leak when the negative control
                // deliberately removes the reprobe reservation.
                let _guard = cv
                    .wait_timeout_while(done, Duration::from_secs(5), |done| !*done)
                    .unwrap();
                return;
            }
            let (lock, cv) = &*pending;
            *lock.lock().unwrap() = true;
            cv.notify_all();
            if mode == "unreachable" {
                return;
            }
            let body = if line.starts_with("GET /llm/catalog ") {
                json!({"services":[{"id":"embed","available":true,"apis":[{"method":"GET","path":"/v1/models"},{"method":"POST","path":"/v1/embeddings"}]}]})
            } else if line.starts_with("GET /llm/services/embed ") {
                json!({"id":"embed","state":if mode == "not_ready" {"stopped"} else {"running"},"available":true,"lease_count":if mode == "lease" {1} else {0}})
            } else {
                models()
            };
            respond(stream, "200 OK", "", &serde_json::to_vec(&body).unwrap());
        });
        let e = env_with(
            "embedding-reserved-timeout",
            &config_json(&format!("http://{}", server.addr)),
            true,
        );
        let lan = LanClient::new(&e.config.ai).unwrap();
        let client =
            EmbeddingClient::new(&e.config.ai, lan.clone(), TaskKind::ImageEmbedding).unwrap();
        let session = lan
            .http()
            .session(Lane::Background, Duration::ZERO, &stop)
            .unwrap();
        let result = client.send(
            &session,
            b"{}",
            &Deadline::after(Duration::from_millis(900)),
        );
        let requests = server.requests.lock().unwrap();
        assert!(requests[0].request_line.starts_with("POST "));
        if mode == "stopping" {
            assert_eq!(result, Err(ProviderError::Stopping));
            assert_eq!(requests.len(), 1);
        } else if mode == "unreachable" {
            assert_eq!(
                result,
                Err(ProviderError::wait(
                    WaitScope::Entrance,
                    WaitReason::Unreachable
                ))
            );
            assert_eq!(requests.len(), 2);
        } else if mode == "not_ready" {
            assert_eq!(
                result,
                Err(ProviderError::wait(
                    WaitScope::Service,
                    WaitReason::NotReady
                ))
            );
            assert_eq!(requests.len(), 3);
        } else if mode == "lease" {
            assert_eq!(
                result,
                Err(ProviderError::wait(WaitScope::Entrance, WaitReason::Leased))
            );
            assert_eq!(requests.len(), 3);
        } else {
            assert_eq!(result, Err(ProviderError::Transient(ErrorCode::Timeout)));
            let error = result.unwrap_err();
            assert_eq!(
                error.into_run_error(false),
                RunError::Repeated(ErrorCode::Timeout)
            );
            assert_eq!(requests.len(), 4);
            // Actual bounded queue transition using fake Unix time: the repeated timeout
            // now advances attempts and reaches failed rather than blocking the entrance.
            e.writer().call(move|c| {
                let key=JobKey::new(JobKind::ImageEmbedding,"timeout-fixture","input","processor");
                jobs::enqueue(c,&key,1)?;
                c.execute("UPDATE ai_jobs SET attempts=4,defer_count=2 WHERE target_key='timeout-fixture'",[])?;
                let job=jobs::claim_next(c,1,"run","token",&[JobKind::ImageEmbedding])?.unwrap();
                assert_eq!(jobs::record_failure(c,&job,1,ErrorCode::Timeout,Failure::Repeated)?,Some(jobs::JobState::Failed));
                Ok(())
            }).unwrap();
        }
        if mode != "stopping" {
            assert!(requests[1].request_line.starts_with("GET /llm/catalog "));
        }
        // Release even when the fixture returns early, so no background POST survives.
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

#[test]
fn timeout_before_first_write_is_wait_and_dns_or_connect_stop_stays_stopping() {
    use crate::ai::waits::{WaitReason, WaitScope};
    for stop_at in ["none", "dns", "connect"] {
        let server = stub(|stream, _| respond(stream, "200 OK", "", b"{}"));
        let dns = FakeDns::new();
        dns.script("lab.test", vec![vec![server.addr]]);
        let stop = StopFlag::new();
        let hook_stop = stop.clone();
        if stop_at == "dns" {
            dns.on_lookup(move || hook_stop.stop());
        }
        // DNS timeout gives the gate a strict non-write result. The callback runs on the
        // resolver thread; no global clock or sleeps in the test thread are needed.
        if stop_at == "none" {
            dns.set_delay(Duration::from_millis(150));
        }
        let e = env_with(
            "embedding-before-write",
            &config_json("http://lab.test:8123"),
            true,
        );
        let lan = LanClient::new(&e.config.ai).unwrap();
        let client = EmbeddingClient::new(&e.config.ai, lan, TaskKind::ImageEmbedding).unwrap();
        let on_connect = stop.clone();
        let http = AiHttp::with_lookup_and_connect_hook(
            "http://lab.test:8123",
            Arc::new(dns),
            Arc::new(move || {
                if stop_at == "connect" {
                    on_connect.stop();
                }
            }),
        )
        .unwrap();
        let session = http
            .session(Lane::Background, Duration::ZERO, &stop)
            .unwrap();
        let result = client.send(&session, b"{}", &Deadline::after(Duration::from_millis(90)));
        if stop_at == "none" {
            assert_eq!(
                result,
                Err(ProviderError::wait(
                    WaitScope::Entrance,
                    WaitReason::Unreachable
                ))
            );
        } else {
            assert_eq!(result, Err(ProviderError::Stopping));
        }
        assert!(server.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn reserved_deadline_is_nested_and_never_restarts_query_or_request_budget() {
    use std::time::Instant;
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn now() -> Instant {
        *START.get_or_init(Instant::now) + Duration::from_secs(TICK.load(Ordering::SeqCst))
    }
    let whole = Deadline::with_clock(Duration::from_secs(15), now);
    TICK.store(4, Ordering::SeqCst);
    let post = whole.reserving(DISCOVERY_TIMEOUT);
    assert_eq!(post.remaining(), Some(Duration::from_secs(9)));
    TICK.store(13, Ordering::SeqCst);
    assert_eq!(post.remaining(), None);
    assert_eq!(
        whole.capped(DISCOVERY_TIMEOUT).remaining(),
        Some(Duration::from_secs(2))
    );
    TICK.store(15, Ordering::SeqCst);
    assert_eq!(whole.remaining(), None);
    assert_eq!(whole.capped(DISCOVERY_TIMEOUT).remaining(), None);
}
