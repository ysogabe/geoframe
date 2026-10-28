//! 合成鍵/RGB/JSON と loopback だけ。配布 binary に差替入口は無い。
use super::*;
use crate::ai::{
    gate::GateConnector,
    http::HttpError,
    image_input::fixtures,
    jobs::{InputCheck, JobKind, JobState},
    settings::CloudTarget,
    tests_http::{FakeDns, Stub, respond, stub},
    tests_worker::{Env, Fake, T0, env_with, key, persist_result},
    worker::Clock as JobClock,
};
use crate::{stop::StopFlag, testutil::TempDir};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use ureq::unversioned::transport::{Connector, DefaultConnector};

pub(super) fn pem() -> String {
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        include_str!("../../../tests/fixtures/jwt-test-pkcs8.b64").trim()
    )
}
pub(super) fn write_key(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, serde_json::to_vec(&json!({"client_email":"synthetic@example.invalid", "private_key":pem(), "token_uri":"http://untrusted.invalid/never"})).unwrap()).unwrap();
    // AI の鍵は所有者だけが読める権限でなければ使われない。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
pub(super) fn token_body() -> Vec<u8> {
    serde_json::to_vec(
        &json!({"access_token":"synthetic-token", "token_type":"Bearer", "expires_in":3600}),
    )
    .unwrap()
}
fn good_reply() -> Vec<u8> {
    serde_json::to_vec(&json!({"candidates":[{"finishReason":"STOP", "content":{"parts":[{"text":"{\"marker\":\"synthetic-reply\"}"}]}}]})).unwrap()
}

const SYNTHETIC_SIGNATURE: &str = "synthetic-thought-signature-never-persist";

fn envelope(parts: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"candidates":[{
        "finishReason":"STOP", "content":{"parts":parts}
    }]}))
    .unwrap()
}

fn signed_reply() -> Vec<u8> {
    envelope(json!([
        {"text":"{\"marker\":"},
        {"text":"\"synthetic-reply\"}"},
        {"text":"", "thoughtSignature":SYNTHETIC_SIGNATURE}
    ]))
}

pub(super) fn transport(server: &Stub, host: &str, dns: &Arc<FakeDns>) -> Transport {
    dns.script(host, vec![vec![server.addr]]);
    Transport::stub(
        host,
        server.addr.port(),
        Arc::new(dns.clone()),
        DefaultConnector::new().chain(GateConnector),
    )
}
fn config(path: &Path, limit: u32, allow: bool) -> String {
    serde_json::to_string(&json!({"version":1,"tasks":{
        "photo_judge":{"provider":"cloud","model":"synthetic-model","allow_cloud":allow,"daily_request_limit":limit,"credential_path":path,"project":"synthetic-project","region":"us"},
        "location_suggest":{"provider":"cloud","model":"synthetic-suggest","allow_cloud":true,"daily_request_limit":20,"credential_path":path,"project":"synthetic-project","region":"us"}
    }})).unwrap()
}
fn request() -> GenerateRequest {
    GenerateRequest::photo_judge(
        "synthetic-prompt",
        &json!({"type":"OBJECT"}),
        &[(1, fixtures::image(32, 24))],
    )
    .unwrap()
}
fn usage(e: &Env) -> u32 {
    e.writer().call(|c| c.query_row("SELECT COALESCE(SUM(count),0) FROM ai_request_usage WHERE task='photo_judge' AND scope='day'", [], |r| r.get(0))).unwrap()
}
fn call(
    e: &Env,
    client: &CloudClient,
    request: &GenerateRequest,
    stop: &StopFlag,
    deadline: &Deadline,
) -> Result<JsonReply, RunError> {
    let read = crate::db::open_read_only(&e.config.db_path()).unwrap();
    let clock: JobClock = Arc::new(|| T0);
    let writer = e.writer();
    let ctx = RunCtx::for_test(
        &read,
        &e.config.ai,
        TaskKind::PhotoJudge,
        stop,
        &writer,
        &clock,
    );
    client.run_until(&ctx, request, deadline)
}
fn client(e: &Env, api: Transport, token: Transport) -> CloudClient {
    let mut c = CloudClients::new(&e.config.ai)
        .unwrap()
        .client(TaskKind::PhotoJudge)
        .unwrap();
    let p = Arc::get_mut(&mut c.provider).unwrap();
    p.api = api;
    p.token = token;
    p.clock = Arc::new(|| Some(T0 as u64));
    c
}
fn fixture(limit: u32) -> (TempDir, Env) {
    let dir = TempDir::new("cloud-key");
    let path = dir.path().join("synthetic-key.json");
    write_key(&path);
    let e = env_with("cloud-db", &config(&path, limit, true), true);
    (dir, e)
}
fn deadline() -> Deadline {
    Deadline::after(Duration::from_secs(3))
}

#[test]
fn host_path_all_location_forms_and_injection_rejection() {
    let mut c = CloudTarget {
        credential_path: "/synthetic/key".into(),
        project: "synthetic-project".into(),
        region: "us".into(),
    };
    for (region, host) in [
        ("us", "aiplatform.us.rep.googleapis.com"),
        ("eu", "aiplatform.eu.rep.googleapis.com"),
        ("global", "aiplatform.googleapis.com"),
        (
            "asia-northeast1",
            "asia-northeast1-aiplatform.googleapis.com",
        ),
    ] {
        c.region = region.into();
        let t = Target::generate(&c, "model-1.2_x").unwrap();
        assert_eq!(t.host, host);
        assert_eq!(
            t.path,
            format!(
                "/v1/projects/synthetic-project/locations/{region}/publishers/google/models/model-1.2_x:generateContent"
            )
        );
    }
    for bad in [
        "", "-x", "x-", "..", "a..b", "a/b", "x?y", "x#y", "x@y", "x%2f", "x\ny", "x:y", "日本",
        " a", "a\\b",
    ] {
        c.region = "us".into();
        c.project = "synthetic-project".into();
        assert!(Target::generate(&c, bad).is_none());
        c.region = bad.into();
        assert!(Target::generate(&c, "model").is_none());
        c.region = "us".into();
        c.project = bad.into();
        assert!(Target::generate(&c, "model").is_none());
    }
    c.project = "a".repeat(64);
    assert!(Target::generate(&c, "model").is_none());
    c.project = "p".into();
    c.region = "a".repeat(33);
    assert!(Target::generate(&c, "model").is_none());
    c.region = "us".into();
    assert!(Target::generate(&c, &"a".repeat(65)).is_none());
}

#[test]
fn typed_request_and_envelope_keep_generated_jpeg_and_redact_debug() {
    let image = fixtures::image(32, 24);
    let r = GenerateRequest::location_suggest(
        "synthetic-prompt",
        &json!({"type":"OBJECT"}),
        &[(17, image.clone())],
    )
    .unwrap();
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][2]["inlineData"]["data"],
        image.jpeg_base64()
    );
    assert_eq!(
        body["contents"][0]["parts"][2]["inlineData"]["mimeType"],
        "image/jpeg"
    );
    assert_eq!(body["contents"][0]["parts"][1]["text"], "写真17");
    assert!(!format!("{r:?}").contains("synthetic-prompt"));
    for (prompt, schema, images) in [
        ("", json!({}), vec![(1, image.clone())]),
        ("p", json!([]), vec![(1, image.clone())]),
        ("p", json!({}), vec![]),
        ("p", json!({}), vec![(0, image.clone())]),
        ("p", json!({}), vec![(1, image.clone()), (1, image.clone())]),
    ] {
        assert_eq!(
            GenerateRequest::photo_judge(prompt, &schema, &images).unwrap_err(),
            RequestError::Invalid
        );
    }
    assert_eq!(
        GenerateRequest::photo_judge(&"x".repeat(8 << 20), &json!({}), &[(1, image.clone())])
            .unwrap_err(),
        RequestError::TooLarge
    );
    // JSON escaping makes the *completed* request too big even if the input estimate fits.
    assert_eq!(
        GenerateRequest::photo_judge(
            &format!("x{}", "\n".repeat((8 << 20) / 2)),
            &json!({}),
            &[(1, image)]
        )
        .unwrap_err(),
        RequestError::TooLarge
    );
    let reply = request::parse_reply(&good_reply()).unwrap();
    assert!(reply.json_text().contains("synthetic-reply"));
    assert!(!format!("{reply:?}").contains("synthetic-reply"));
    for body in [b"{}".to_vec(),b"not-json".to_vec(),serde_json::to_vec(&json!({"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[{"text":"{}"}]}}]})).unwrap(),serde_json::to_vec(&json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"fileData":{"fileUri":"https://untrusted.invalid"}}]}}]})).unwrap()] { assert!(request::parse_reply(&body).is_none()); }
}

fn assert_python_schema_envelope(task: TaskKind) {
    // Generated from AST only by tools/gen_ai_cloud_schema_fixture.py --check.
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ai-cloud-schemas-python.json"
    ))
    .unwrap();
    let schema = &fixture["schemas"][task.as_str()];
    assert_eq!(schema["type"], "object");
    let images = [(1, fixtures::image(32, 24))];
    let r = match task {
        TaskKind::PhotoJudge => GenerateRequest::photo_judge("synthetic-prompt", schema, &images),
        TaskKind::LocationSuggest => {
            GenerateRequest::location_suggest("synthetic-prompt", schema, &images)
        }
        _ => unreachable!(),
    }
    .unwrap();
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    let config = &body["generationConfig"];
    assert_eq!(config["responseJsonSchema"], *schema);
    assert!(config.get("responseSchema").is_none());
    assert_eq!(config["temperature"], json!(0.1));
    assert_eq!(config["responseMimeType"], "application/json");
    assert_eq!(r.task, task);
}

#[test]
fn photo_judge_preserves_python_json_schema_and_generation_config() {
    assert_python_schema_envelope(TaskKind::PhotoJudge);
}

#[test]
fn location_suggest_preserves_python_json_schema_and_generation_config() {
    assert_python_schema_envelope(TaskKind::LocationSuggest);
}

#[test]
fn reply_accepts_only_text_with_optional_string_signature_and_discards_it() {
    for text in ["{}", "[]"] {
        for parts in [
            json!([{"text":text}]),
            json!([{"text":text,"thoughtSignature":SYNTHETIC_SIGNATURE}]),
            json!([{"text":&text[..1]}, {"text":&text[1..]},
                {"text":"","thoughtSignature":SYNTHETIC_SIGNATURE}]),
        ] {
            let reply = request::parse_reply(&envelope(parts)).unwrap();
            assert_eq!(reply.json_text(), text);
            assert!(!reply.json_text().contains(SYNTHETIC_SIGNATURE));
            assert!(!format!("{reply:?}").contains(SYNTHETIC_SIGNATURE));
        }
    }
}

#[test]
fn reply_rejects_other_payloads_invalid_signature_json_and_finish_reason() {
    for (name, value) in [
        ("functionCall", json!({"name":"synthetic"})),
        ("fileData", json!({"fileUri":"https://untrusted.invalid"})),
        (
            "inlineData",
            json!({"mimeType":"image/jpeg","data":"synthetic"}),
        ),
        ("url", json!("https://untrusted.invalid")),
        ("thought", json!(true)),
        ("unknownPayload", json!({})),
    ] {
        // Reject the payload both alone and alongside otherwise valid signed text.
        for with_text in [false, true] {
            let mut part = json!({"thoughtSignature":SYNTHETIC_SIGNATURE});
            if with_text {
                part["text"] = json!("{}");
            }
            part[name] = value.clone();
            assert!(request::parse_reply(&envelope(json!([part]))).is_none());
        }
    }
    for signature in [Value::Null, json!(1), json!(true), json!([]), json!({})] {
        assert!(
            request::parse_reply(&envelope(json!([
                {"text":"{}","thoughtSignature":signature}
            ])))
            .is_none()
        );
    }
    for text in ["", "{", "null", "1", "true", "\"text\""] {
        assert!(
            request::parse_reply(&envelope(json!([
                {"text":text,"thoughtSignature":SYNTHETIC_SIGNATURE}
            ])))
            .is_none()
        );
    }
    for parts in [
        json!([]),
        json!([{}]),
        json!([{"text":1}]),
        json!([{"thoughtSignature":SYNTHETIC_SIGNATURE}]),
    ] {
        assert!(request::parse_reply(&envelope(parts)).is_none());
    }
    for finish in [json!("MAX_TOKENS"), json!("SAFETY"), json!(null)] {
        let mut body: Value = serde_json::from_slice(&signed_reply()).unwrap();
        body["candidates"][0]["finishReason"] = finish;
        assert!(request::parse_reply(&serde_json::to_vec(&body).unwrap()).is_none());
    }
    let mut body: Value = serde_json::from_slice(&signed_reply()).unwrap();
    let candidate = body["candidates"][0].clone();
    body["candidates"].as_array_mut().unwrap().push(candidate);
    assert!(request::parse_reply(&serde_json::to_vec(&body).unwrap()).is_none());
    assert!(request::parse_reply(&vec![b' '; (1 << 20) + 1]).is_none());
}

#[test]
fn signed_200_succeeds_once_with_budget_one_and_never_persists_signature() {
    let (_dir, e) = fixture(1);
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let api = stub(|s, _| respond(s, "200 OK", "", &signed_reply()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let handler = Fake::new(JobKind::PhotoJudge, move |ctx, _| {
        let reply = c.run(ctx, &request())?;
        assert_eq!(reply.json_text(), "{\"marker\":\"synthetic-reply\"}");
        assert!(!format!("{reply:?}").contains(SYNTHETIC_SIGNATURE));
        // Synthetic downstream saving: no production handler or result schema is added.
        Ok(persist_result(
            reply.json_text().into(),
            InputCheck::Current,
        ))
    });
    e.enqueue(&key(JobKind::PhotoJudge, "signed"));
    let logs = crate::testutil::capture::logs_of(|| {
        e.cycle(std::slice::from_ref(&handler), &StopFlag::new());
        e.cycle(std::slice::from_ref(&handler), &StopFlag::new());
    });
    assert!(!logs.contains(SYNTHETIC_SIGNATURE));
    assert_eq!(e.state(JobKind::PhotoJudge, "signed"), JobState::Succeeded);
    assert_eq!(usage(&e), 1);
    assert_eq!(api.requests.lock().unwrap().len(), 1);
    assert_eq!(auth.requests.lock().unwrap().len(), 1);
    assert_eq!(e.results(), 1);
    let saved: String = e
        .writer()
        .call(|c| c.query_row("SELECT target FROM ai_test_results", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(saved, "{\"marker\":\"synthetic-reply\"}");
    assert!(!saved.contains(SYNTHETIC_SIGNATURE));
}

#[test]
fn success_one_reservation_token_zero_cache_reused_headers_and_jpeg_match() {
    let (_dir, e) = fixture(20);
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    for _ in 0..2 {
        assert!(call(&e, &c, &request(), &StopFlag::new(), &deadline()).is_ok());
    }
    assert_eq!(usage(&e), 2);
    assert_eq!(auth.requests.lock().unwrap().len(), 1);
    let requests = api.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for r in &*requests {
        assert_eq!(r.header("authorization"), Some("Bearer synthetic-token"));
        assert_eq!(
            r.header("host"),
            Some(format!("api.test:{}", api.addr.port()).as_str())
        );
        assert_eq!(r.header("origin"), None);
        assert_eq!(r.header("x-client-name"), None);
        assert!(
            r.request_line
                .ends_with("/publishers/google/models/synthetic-model:generateContent HTTP/1.1")
        );
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(
            body["contents"][0]["parts"][2]["inlineData"]["data"],
            fixtures::image(32, 24).jpeg_base64()
        );
    }
    let tokens = auth.requests.lock().unwrap();
    assert_eq!(tokens[0].header("authorization"), None);
    assert_eq!(tokens[0].header("origin"), None);
    assert_eq!(
        tokens[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    let body = std::str::from_utf8(&tokens[0].body).unwrap();
    let assertion = body.split("&assertion=").nth(1).unwrap();
    let signer = crate::jwt::Rs256Signer::from_pem(&pem()).unwrap();
    let claims = crate::jwt::verify(assertion, signer.public_key_der()).unwrap();
    assert_eq!(claims["scope"], target::SCOPE);
    assert_eq!(claims["aud"], target::TOKEN_URL);
    assert_eq!(claims["iat"], T0);
    assert_eq!(claims["exp"], T0 + 3600);
}

#[test]
fn once_only_401_refresh_and_each_inference_consumes_its_own_reservation() {
    for (limit, always401, expected_api, expected_usage, expected) in [
        (20, false, 2, 2, None),
        (
            20,
            true,
            2,
            2,
            Some(RunError::ProviderPause(
                super::super::pause::PauseReason::Unauthorized,
            )),
        ),
        (
            1,
            false,
            1,
            1,
            Some(RunError::Budget(
                super::super::budget::Reservation::DailyExhausted,
            )),
        ),
    ] {
        let (_dir, e) = fixture(limit);
        let n = Arc::new(AtomicUsize::new(0));
        let count = n.clone();
        let api = stub(move |s, _| {
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            if first || always401 {
                respond(s, "401 Unauthorized", "", b"");
            } else {
                respond(s, "200 OK", "", &good_reply());
            }
        });
        let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        let result = call(&e, &c, &request(), &StopFlag::new(), &deadline());
        assert_eq!(result.err(), expected);
        assert_eq!(n.load(Ordering::SeqCst), expected_api);
        assert_eq!(usage(&e), expected_usage);
        assert_eq!(auth.requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn policy_stop_invalid_and_oversize_are_checked_before_any_key_dns_or_http() {
    let dir = TempDir::new("cloud-disabled");
    let path = dir.path().join("missing-key");
    for (allow, limit) in [(false, 20), (true, 0), (true, 20)] {
        let e = env_with("cloud-disabled-db", &config(&path, limit, allow), true);
        let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
        let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        let stop = StopFlag::new();
        if allow && limit > 0 {
            stop.stop();
        }
        assert_eq!(
            call(&e, &c, &request(), &stop, &deadline()).err(),
            Some(if stop.is_stopped() {
                RunError::Stopping
            } else {
                RunError::PolicyDisabled
            })
        );
        let mut oversized = request();
        oversized.body = vec![0; (8 << 20) + 1];
        assert_eq!(
            call(&e, &c, &oversized, &StopFlag::new(), &deadline()).err(),
            Some(RunError::Permanent(ErrorCode::InputTooLarge))
        );
        let mut invalid = request();
        invalid.task = TaskKind::LocationSuggest;
        assert_eq!(
            call(&e, &c, &invalid, &StopFlag::new(), &deadline()).err(),
            Some(RunError::Permanent(ErrorCode::InputInvalid))
        );
        assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
        assert_eq!(usage(&e), 0);
        assert_eq!(
            auth.requests.lock().unwrap().len() + api.requests.lock().unwrap().len(),
            0
        );
    }
    let e = env_with("cloud-no-writer", &config(&path, 20, true), false);
    assert_eq!(
        e.config.ai.gate(TaskKind::PhotoJudge, false).unwrap_err(),
        super::super::settings::Disabled::NoWriter
    );
}

#[test]
fn auth_failure_is_never_a_billable_attempt_and_never_sends_inference() {
    for (status,body,expected) in [
        (400,b"".to_vec(),RunError::ProviderPause(super::super::pause::PauseReason::Unauthorized)),
        (403,b"".to_vec(),RunError::ProviderPause(super::super::pause::PauseReason::Forbidden)),
        (500,b"".to_vec(),ProviderError::wait(super::super::waits::WaitScope::Entrance,super::super::waits::WaitReason::Unreachable).into_run_error(false)),
        (200,b"{\"access_token\":\"secret\\r\\nBad: bad\",\"token_type\":\"Bearer\",\"expires_in\":3600}".to_vec(),RunError::ProviderPause(super::super::pause::PauseReason::Unauthorized))
    ] {
        let (_dir,e)=fixture(20); let api=stub(|s,_|respond(s,"200 OK","",&good_reply()));
        let auth=stub(move|s,_|respond(s,&format!("{status} X"),"",&body));let dns=FakeDns::new();
        let c=client(&e,transport(&api,"api.test",&dns),transport(&auth,"oauth.test",&dns));
        assert_eq!(call(&e,&c,&request(),&StopFlag::new(),&deadline()).unwrap_err(),expected);
        assert_eq!(api.requests.lock().unwrap().len(),0);assert_eq!(usage(&e),0);
    }
}

#[test]
fn inference_statuses_and_unreadable_or_large_envelope_are_classified() {
    use super::super::{
        pause::PauseReason,
        waits::{Wait, WaitReason, WaitScope},
    };
    for (status, headers, body, expected) in [
        (
            401,
            "",
            vec![],
            RunError::ProviderPause(PauseReason::Unauthorized),
        ),
        (
            403,
            "",
            vec![],
            RunError::ProviderPause(PauseReason::Forbidden),
        ),
        (
            404,
            "",
            vec![],
            RunError::TaskPause(PauseReason::ModelNotFound),
        ),
        (
            429,
            "Retry-After: 7\r\n",
            vec![],
            RunError::ProviderWait(Wait {
                scope: WaitScope::Entrance,
                reason: WaitReason::RateLimited,
                retry_after_secs: Some(7),
            }),
        ),
        (500, "", vec![], RunError::Attempt(ErrorCode::ServerError)),
        (
            400,
            "",
            vec![],
            RunError::Permanent(ErrorCode::InputInvalid),
        ),
        (
            413,
            "",
            vec![],
            RunError::Permanent(ErrorCode::InputTooLarge),
        ),
        (
            200,
            "",
            b"not-json".to_vec(),
            RunError::Attempt(ErrorCode::BadResponse),
        ),
        (
            200,
            "",
            vec![b' '; (1 << 20) + 1],
            RunError::Attempt(ErrorCode::BadResponse),
        ),
    ] {
        let (_dir, e) = fixture(20);
        let api = stub(move |s, _| respond(s, &format!("{status} X"), headers, &body));
        let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        assert_eq!(
            call(&e, &c, &request(), &StopFlag::new(), &deadline()).unwrap_err(),
            expected
        );
        assert_eq!(usage(&e), if status == 401 { 2 } else { 1 });
    }
}

#[test]
fn redirects_at_both_endpoints_never_follow_or_forward_secrets() {
    for token_redirect in [true, false] {
        let trap = stub(|s, _| respond(s, "200 OK", "", b"{}"));
        let loc = format!("Location: http://{}/trap\r\n", trap.addr);
        let (_dir, e) = fixture(20);
        let l = loc.clone();
        let api = stub(move |s, _| respond(s, "302 Found", &l, b""));
        let auth = stub(move |s, _| {
            if token_redirect {
                respond(s, "302 Found", &loc, b"")
            } else {
                respond(s, "200 OK", "", &token_body())
            }
        });
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        assert_eq!(
            call(&e, &c, &request(), &StopFlag::new(), &deadline()).unwrap_err(),
            contract()
        );
        assert_eq!(trap.connections.load(Ordering::SeqCst), 0);
        assert_eq!(usage(&e), if token_redirect { 0 } else { 1 });
    }
}

#[test]
fn unsent_dns_and_connect_failures_wait_with_zero_attempts_but_keep_the_reservation() {
    for dns_failure in [true, false] {
        let (_dir, e) = fixture(20);
        let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
        let dns = FakeDns::new();
        let api = if dns_failure {
            Transport::stub(
                "absent.test",
                80,
                Arc::new(dns.clone()),
                DefaultConnector::new().chain(GateConnector),
            )
        } else {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            drop(listener);
            dns.script("absent.test", vec![vec![addr]]);
            Transport::stub(
                "absent.test",
                addr.port(),
                Arc::new(dns.clone()),
                DefaultConnector::new().chain(GateConnector),
            )
        };
        let c = client(&e, api, transport(&auth, "oauth.test", &dns));
        let handler = Fake::new(JobKind::PhotoJudge, move |ctx, job| {
            c.run(ctx, &request())?;
            Ok(persist_result(
                job.key.target_key.clone(),
                InputCheck::Current,
            ))
        });
        e.enqueue(&key(JobKind::PhotoJudge, "t"));
        e.cycle(&[handler], &StopFlag::new());
        let row = e.row(JobKind::PhotoJudge, "t").unwrap();
        assert_eq!(row.attempts, 0);
        assert_eq!(row.state, JobState::Deferred);
        assert_eq!(usage(&e), 1);
    }
}

// Manual time advances only at scripted I/O boundaries, on the calling thread.
// RSA/DB/CPU speed therefore cannot decide which request has started.
thread_local! {
    static IO_NOW: std::cell::Cell<std::time::Instant> = std::cell::Cell::new(std::time::Instant::now());
}
fn io_now() -> std::time::Instant {
    IO_NOW.with(std::cell::Cell::get)
}
fn advance_io(ms: u64) {
    IO_NOW.with(|t| t.set(t.get() + Duration::from_millis(ms)));
}

#[derive(Debug)]
struct Exchange {
    host: &'static str,
    budget_ms: u64,
    work_ms: u64,
    before_write: bool,
    status: &'static str,
}
#[derive(Debug, Clone)]
struct ScriptConnector(Arc<Mutex<std::collections::VecDeque<Exchange>>>);
impl Connector for ScriptConnector {
    type Out = Box<dyn ureq::unversioned::transport::Transport>;
    fn connect(
        &self,
        d: &ureq::unversioned::transport::ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let step = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request");
        assert_eq!(d.uri.host(), Some(step.host));
        // Assert exactly what production passes to ureq, before any fake I/O.
        assert_eq!(
            d.config.timeouts().global,
            Some(Duration::from_millis(step.budget_ms))
        );
        if step.before_write {
            advance_io(step.budget_ms);
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        Ok(Some(Box::new(ScriptIo {
            buffers: ureq::unversioned::transport::LazyBuffers::new(16384, 16384),
            step: Some(step),
            wrote: false,
        })))
    }
}
#[derive(Debug)]
struct ScriptIo {
    buffers: ureq::unversioned::transport::LazyBuffers,
    step: Option<Exchange>,
    wrote: bool,
}
impl ureq::unversioned::transport::Transport for ScriptIo {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        &mut self.buffers
    }
    fn transmit_output(
        &mut self,
        amount: usize,
        _: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        self.wrote |= amount > 0;
        Ok(())
    }
    fn await_input(
        &mut self,
        _: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        use ureq::unversioned::transport::Buffers;
        assert!(
            self.wrote,
            "timeout/response must follow the real write gate"
        );
        let Some(step) = self.step.take() else {
            return Ok(false);
        };
        advance_io(step.work_ms.min(step.budget_ms));
        if step.work_ms > step.budget_ms {
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        let body = if step.host == "oauth.test" {
            token_body()
        } else {
            good_reply()
        };
        let head = format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            step.status,
            body.len()
        );
        let response = [head.as_bytes(), &body].concat();
        self.buffers.input_append_buf()[..response.len()].copy_from_slice(&response);
        self.buffers.input_appended(response.len());
        Ok(true)
    }
    fn is_open(&mut self) -> bool {
        false
    }
}
fn exchange(host: &'static str, budget_ms: u64, work_ms: u64, status: &'static str) -> Exchange {
    Exchange {
        host,
        budget_ms,
        work_ms,
        before_write: false,
        status,
    }
}
fn scripted_client(e: &Env, steps: Vec<Exchange>) -> (CloudClient, ScriptConnector) {
    let script = ScriptConnector(Arc::new(Mutex::new(steps.into())));
    let dns = FakeDns::new();
    let transport = |host| {
        dns.script(host, vec![vec!["127.0.0.1:80".parse().unwrap()]]);
        Transport::stub(
            host,
            80,
            Arc::new(dns.clone()),
            script.clone().chain(GateConnector),
        )
    };
    let c = client(e, transport("api.test"), transport("oauth.test"));
    (c, script)
}
fn unreachable() -> RunError {
    use crate::ai::waits::{Wait, WaitReason, WaitScope};
    RunError::ProviderWait(Wait::new(WaitScope::Entrance, WaitReason::Unreachable))
}

#[test]
fn sent_timeout_is_attempt_and_whole_deadline_includes_auth_and_refresh() {
    let auth = |budget, work| exchange("oauth.test", budget, work, "200 OK");
    let api = |budget, work, status| exchange("api.test", budget, work, status);
    let mut unsent = api(800, 900, "200 OK");
    unsent.before_write = true;
    for (steps, expected, reservations) in [
        (vec![auth(1000, 1100)], unreachable(), 0),
        (vec![auth(1000, 200), unsent], unreachable(), 1),
        (
            vec![auth(1000, 200), api(800, 900, "200 OK")],
            RunError::Attempt(ErrorCode::Timeout),
            1,
        ),
        // A sent 401 does not make a subsequent *token* timeout an Attempt.
        (
            vec![
                auth(1000, 200),
                api(800, 200, "401 Unauthorized"),
                auth(600, 700),
            ],
            unreachable(),
            1,
        ),
        // Refresh returns exactly at the deadline: no second reservation or write.
        (
            vec![
                auth(1000, 200),
                api(800, 200, "401 Unauthorized"),
                auth(600, 600),
            ],
            unreachable(),
            1,
        ),
        // All four requests use one shrinking deadline; the retry really starts.
        (
            vec![
                auth(1000, 200),
                api(800, 200, "401 Unauthorized"),
                auth(600, 200),
                api(400, 500, "200 OK"),
            ],
            RunError::Attempt(ErrorCode::Timeout),
            2,
        ),
    ] {
        let (_dir, e) = fixture(20);
        let (c, script) = scripted_client(&e, steps);
        let result = call(
            &e,
            &c,
            &request(),
            &StopFlag::new(),
            &Deadline::with_clock(Duration::from_secs(1), io_now),
        );
        assert_eq!(result.unwrap_err(), expected);
        assert!(script.0.lock().unwrap().is_empty());
        assert_eq!(usage(&e), reservations);
    }
}

#[test]
fn writer_reservation_wait_uses_the_remaining_whole_deadline() {
    use std::sync::mpsc;
    let (_dir, e) = fixture(20);
    let (c, _) = scripted_client(&e, vec![exchange("oauth.test", 3000, 0, "200 OK")]);
    let stop = StopFlag::new();
    // Fill the real auth cache before imposing a short real-time deadline.
    c.provider
        .auth
        .token(
            &c.provider.cloud.credential_path,
            &c.provider.token,
            &Deadline::with_clock(Duration::from_secs(3), io_now),
            &stop,
            &*c.provider.clock,
        )
        .unwrap();
    let (release, held) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    std::thread::scope(|scope| {
        let writer = e.writer();
        let blocker = scope.spawn(move || {
            writer
                .call(move |_| {
                    entered.send(()).unwrap();
                    let _ = held.recv_timeout(Duration::from_secs(3));
                    Ok(())
                })
                .unwrap()
        });
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = std::time::Instant::now();
        let result = call(
            &e,
            &c,
            &request(),
            &stop,
            &Deadline::after(Duration::from_millis(200)),
        );
        let elapsed = started.elapsed();
        let _ = release.send(());
        blocker.join().unwrap();
        assert_eq!(result.unwrap_err(), unreachable());
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    });
    // Flushing the writer proves the expired queued reservation never runs later.
    assert_eq!(usage(&e), 0);
}

#[derive(Debug)]
struct InspectConnector(Arc<AtomicUsize>);
impl Connector for InspectConnector {
    type Out = Box<dyn ureq::unversioned::transport::Transport>;
    fn connect(
        &self,
        d: &ureq::unversioned::transport::ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(d.uri.host(), Some(target::TOKEN_HOST));
        assert_eq!(d.uri.scheme_str(), Some("https"));
        assert!(
            d.addrs
                .iter()
                .all(|a| a.port() == 443 && super::super::dest::is_public_ip(a.ip()))
        );
        assert!(d.config.proxy().is_none());
        Err(ureq::Error::ConnectionFailed) // inspect only; never open an external socket
    }
}

#[test]
fn public_only_dns_vets_every_result_and_pins_original_host_and_port() {
    use super::super::dest::is_public_ip;
    let sa = |s: &str| SocketAddr::new(s.parse().unwrap(), 443);
    for ok in [
        "8.8.8.8",
        "142.250.1.1",
        "2607:f8b0:4004::1",
        "2001:4860:4860::8888",
    ] {
        assert!(is_public_ip(ok.parse().unwrap()), "{ok}");
    }
    let bad = [
        "0.0.0.0",
        "10.0.0.1",
        "127.0.0.1",
        "100.64.0.1",
        "169.254.1.1",
        "172.16.0.1",
        "192.168.0.1",
        "192.0.0.9",
        "192.0.2.1",
        "192.31.196.1",
        "192.52.193.1",
        "192.175.48.1",
        "2620:4f:8000::1",
        "192.88.99.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:8.8.8.8",
        "64:ff9b::808:808",
        "100::1",
        "fc00::1",
        "fe80::1",
        "fec0::1",
        "ff00::1",
        "2001:db8::1",
        "2001::1",
        "2001:20::1",
        "2002:808:808::1",
        "3fff::1",
    ];
    let dns = FakeDns::new();
    let connects = Arc::new(AtomicUsize::new(0));
    let t = Transport::public_with(
        target::TOKEN_HOST,
        Arc::new(dns.clone()),
        InspectConnector(connects.clone()),
    );
    for ip in bad {
        assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        for addresses in [vec![sa("8.8.8.8"), sa(ip)], vec![sa(ip), sa("8.8.8.8")]] {
            dns.script(target::TOKEN_HOST, vec![addresses]);
            let e = t
                .post("/token", b"x", None, &deadline(), &StopFlag::new())
                .unwrap_err();
            assert_eq!(e.error, HttpError::DestinationDenied);
            assert!(!e.started);
        }
    }
    dns.script(target::TOKEN_HOST, vec![vec![sa("8.8.8.8"); 17]]);
    assert_eq!(
        t.post("/token", b"x", None, &deadline(), &StopFlag::new())
            .unwrap_err()
            .error,
        HttpError::DestinationDenied
    );
    dns.script(target::TOKEN_HOST, vec![vec![]]);
    assert!(
        !t.post("/token", b"x", None, &deadline(), &StopFlag::new())
            .unwrap_err()
            .started
    );
    assert_eq!(connects.load(Ordering::SeqCst), 0);
    dns.script(
        target::TOKEN_HOST,
        vec![vec![sa("8.8.8.8"), sa("2607:f8b0:4004::1")]],
    );
    let e = t
        .post("/token", b"x", None, &deadline(), &StopFlag::new())
        .unwrap_err();
    assert_eq!(e.error, HttpError::Net(crate::net::Failure::Connect));
    assert!(!e.started);
    assert_eq!(connects.load(Ordering::SeqCst), 1);
    // A test transport cannot even resolve a mismatched authority.
    let mismatched = super::super::http::build_agent(
        InspectConnector(connects.clone()),
        super::super::dest::PinResolver::public(target::TOKEN_HOST, Arc::new(dns.clone())),
    );
    let _armed = super::super::gate::arm(&StopFlag::new());
    assert!(
        super::super::dest::denied(
            &mismatched
                .post("https://untrusted.invalid/token")
                .send(b"x")
                .unwrap_err()
        )
        .is_some()
    );
    assert_eq!(connects.load(Ordering::SeqCst), 1);
}

#[test]
fn token_and_api_stop_after_dns_and_after_connect_write_no_http_bytes() {
    for token_phase in [true, false] {
        for after_connect in [true, false] {
            let (_dir, e) = fixture(20);
            let stop = StopFlag::new();
            let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
            let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
            let dns = FakeDns::new();
            let halted = if token_phase { &auth } else { &api };
            let host = if token_phase {
                "oauth.test"
            } else {
                "api.test"
            };
            dns.script(host, vec![vec![halted.addr]]);
            let transport = if after_connect {
                let signal = stop.clone();
                Transport::stub(
                    host,
                    halted.addr.port(),
                    Arc::new(dns.clone()),
                    DefaultConnector::new()
                        .chain(super::super::gate::HookConnector(Arc::new(move || {
                            signal.stop()
                        })))
                        .chain(GateConnector),
                )
            } else {
                let signal = stop.clone();
                dns.on_lookup(move || signal.stop());
                Transport::stub(
                    host,
                    halted.addr.port(),
                    Arc::new(dns.clone()),
                    DefaultConnector::new().chain(GateConnector),
                )
            };
            // For API DNS test use a different lookup for auth, so stop is triggered at the right phase.
            let other_dns = FakeDns::new();
            let (a, t) = if token_phase {
                (self::transport(&api, "api.test", &other_dns), transport)
            } else {
                (transport, self::transport(&auth, "oauth.test", &other_dns))
            };
            let c = client(&e, a, t);
            assert_eq!(
                call(&e, &c, &request(), &stop, &deadline()).unwrap_err(),
                RunError::Stopping
            );
            assert_eq!(halted.bytes.load(Ordering::SeqCst), 0);
            assert_eq!(usage(&e), if token_phase { 0 } else { 1 });
        }
    }
}

#[test]
fn stop_after_first_write_preserves_successful_reply_and_writer_is_free_during_io() {
    let (_dir, e) = fixture(20);
    let stop = StopFlag::new();
    let signal = stop.clone();
    let writer = e.writer();
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let api = stub(move |s, _| {
        signal.stop();
        // The HTTP operation cannot own the writer or a transaction while waiting.
        writer
            .call_within(
                std::time::Instant::now() + Duration::from_millis(150),
                |c| c.execute("INSERT INTO ai_test_results VALUES ('other-operation')", []),
            )
            .unwrap();
        respond(s, "200 OK", "", &good_reply());
    });
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    assert!(call(&e, &c, &request(), &stop, &deadline()).is_ok());
    assert!(stop.is_stopped());
    assert_eq!(e.results(), 1);
    assert_eq!(usage(&e), 1);
}

#[test]
fn same_provider_tasks_share_lanes_and_interactive_does_not_wait_for_background_token() {
    let (_dir, e) = fixture(20);
    let clients = CloudClients::new(&e.config.ai).unwrap();
    let a = clients.client(TaskKind::PhotoJudge).unwrap();
    let b = clients.client(TaskKind::LocationSuggest).unwrap();
    assert!(Arc::ptr_eq(&a.provider, &b.provider));
    assert!(Arc::ptr_eq(&a.provider.lanes, &b.provider.lanes));
    let stop = StopFlag::new();
    let held = a
        .provider
        .lanes
        .try_acquire(Lane::Background, &stop)
        .unwrap();
    let interactive = b
        .provider
        .lanes
        .try_acquire(Lane::Interactive, &stop)
        .unwrap();
    assert!(
        b.provider
            .lanes
            .try_acquire(Lane::Background, &stop)
            .is_err()
    );
    drop(interactive);
    drop(held);
    // Real token exchange is held in the background lane, while interactive can take its independent slot.
    let entered = Arc::new(std::sync::Barrier::new(2));
    let released = Arc::new(std::sync::Barrier::new(2));
    let (x, y) = (entered.clone(), released.clone());
    let auth = stub(move |s, _| {
        x.wait();
        y.wait();
        respond(s, "200 OK", "", &token_body())
    });
    let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let provider = c.provider.clone();
    std::thread::scope(|s| {
        let handle = s.spawn(|| call(&e, &c, &request(), &stop, &deadline()));
        entered.wait();
        let _interactive = provider
            .lanes
            .try_acquire(Lane::Interactive, &stop)
            .unwrap();
        released.wait();
        handle.join().unwrap().unwrap();
    });
    assert_eq!(usage(&e), 1);
}

#[test]
fn lane_wait_and_slow_dns_stay_inside_the_overall_deadline() {
    let (_dir, e) = fixture(20);
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let stop = StopFlag::new();
    let held = c
        .provider
        .lanes
        .try_acquire(Lane::Background, &stop)
        .unwrap();
    let start = std::time::Instant::now();
    assert!(matches!(
        call(
            &e,
            &c,
            &request(),
            &stop,
            &Deadline::after(Duration::from_millis(60))
        ),
        Err(RunError::ProviderWait(_))
    ));
    assert!(start.elapsed() < Duration::from_millis(150));
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    drop(held);
    dns.set_delay(Duration::from_millis(250));
    let start = std::time::Instant::now();
    assert!(matches!(
        call(
            &e,
            &c,
            &request(),
            &stop,
            &Deadline::after(Duration::from_millis(70))
        ),
        Err(RunError::ProviderWait(_))
    ));
    assert!(start.elapsed() < Duration::from_millis(170));
    assert_eq!(usage(&e), 0);
    assert_eq!(auth.bytes.load(Ordering::SeqCst), 0);
}

#[test]
fn tls_sni_keeps_original_name_and_tls_failure_is_unsent() {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hello = Arc::new(Mutex::new(Vec::new()));
    let capture = hello.clone();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = [0; 4096];
        let n = stream.read(&mut bytes).unwrap();
        *capture.lock().unwrap() = bytes[..n].to_vec();
    });
    let dns = FakeDns::new();
    dns.script(target::TOKEN_HOST, vec![vec![addr]]);
    // cfg(test) loopback resolver, TLS and HTTPS still real. No production override.
    let t = Transport::tls_stub(target::TOKEN_HOST, addr.port(), Arc::new(dns.clone()));
    let error = t
        .post(
            "/token",
            b"synthetic-assertion",
            None,
            &deadline(),
            &StopFlag::new(),
        )
        .unwrap_err();
    assert!(!error.started);
    thread.join().unwrap();
    let hello = hello.lock().unwrap();
    assert!(
        hello
            .windows(target::TOKEN_HOST.len())
            .any(|w| w == target::TOKEN_HOST.as_bytes())
    );
    assert!(!hello.windows(9).any(|w| w == b"127.0.0.1"));
    assert!(!hello.windows(19).any(|w| w == b"synthetic-assertion"));
}

#[test]
fn wire_and_decompressed_response_limits_apply_to_cloud() {
    use crate::ai::http::MAX_RESPONSE_BYTES;
    use crate::testutil::gzip;
    let wire_limit = crate::net::wire_limit(MAX_RESPONSE_BYTES) as usize;
    // 34 legal members with a large extra field, rather than millions of empty
    // members. Each contains one valid unknown subfield (RFC 1952 §2.3.1.1).
    let member = gzip::with_extra(b"", u16::MAX as usize);
    let wire_body = member.repeat(wire_limit / member.len() + 1);
    assert!(wire_body.len() > wire_limit);
    let expanded_body = gzip::padded(b"", MAX_RESPONSE_BYTES as usize + 1);
    assert!(expanded_body.len() < wire_limit);
    for (body, expected) in [
        // Positive controls: valid padding, and the exact decompressed boundary.
        (member, Ok(Vec::new())),
        (
            gzip::padded(b"", MAX_RESPONSE_BYTES as usize),
            Ok(vec![b' '; MAX_RESPONSE_BYTES as usize]),
        ),
        (wire_body, Err(HttpError::ResponseTooLarge)),
        (expanded_body, Err(HttpError::ResponseTooLarge)),
    ] {
        let api = stub(move |s, _| respond(s, "200 OK", "Content-Encoding: gzip\r\n", &body));
        let dns = FakeDns::new();
        let t = transport(&api, "api.test", &dns);
        match (
            t.post(
                "/generate",
                b"{}",
                Some("synthetic-token"),
                &deadline(),
                &StopFlag::new(),
            ),
            expected,
        ) {
            (Ok(reply), Ok(body)) => assert_eq!(reply.body, body),
            (Err(error), Err(expected)) => {
                assert_eq!(error.error, expected);
                assert!(error.started);
            }
            (actual, _) => panic!("unexpected response kind: {actual:?}"),
        }
    }
}

#[test]
fn logs_debug_and_fixed_errors_do_not_expose_any_sensitive_input_or_output() {
    let (_dir, e) = fixture(20);
    let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let mut debug = String::new();
    let logs = crate::testutil::capture::logs_of(|| {
        let r = call(&e, &c, &request(), &StopFlag::new(), &deadline()).unwrap();
        debug = format!("{c:?} {:?} {r:?}", request());
    });
    let all = format!("{logs}{debug}");
    for marker in [
        "synthetic-token",
        "synthetic-prompt",
        "synthetic-reply",
        "synthetic-model",
        "synthetic@example.invalid",
        "api.test",
        "oauth.test",
        "https://",
        "synthetic-key.json",
        &pem(),
        &fixtures::image(32, 24).jpeg_base64(),
    ] {
        assert!(!all.contains(marker));
    }
}

#[test]
fn cloud_token_and_inference_ignore_all_environment_proxies() {
    const CHILD: &str = "GEOFRAME_TEST_CLOUD_PROXY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let (_dir, e) = fixture(20);
        let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
        let api = stub(|s, _| respond(s, "200 OK", "", &good_reply()));
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        assert!(call(&e, &c, &request(), &StopFlag::new(), &deadline()).is_ok());
        assert_eq!(auth.requests.lock().unwrap().len(), 1);
        assert_eq!(api.requests.lock().unwrap().len(), 1);
        return;
    }
    let proxy = stub(|s, _| respond(s, "502 Bad Gateway", "", b""));
    let url = format!("http://{}", proxy.addr);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "ai::cloud::tests::cloud_token_and_inference_ignore_all_environment_proxies",
            "--test-threads=1",
        ])
        .env(CHILD, "1");
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        child.env(k, &url);
    }
    let output = child
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn sent_server_failures_stop_at_five_attempts_and_do_not_retry_internally() {
    let (_dir, e) = fixture(20);
    let api = stub(|s, _| respond(s, "503 Service Unavailable", "", b""));
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let handler = Fake::new(JobKind::PhotoJudge, move |ctx, job| {
        c.run(ctx, &request())?;
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    });
    e.enqueue(&key(JobKind::PhotoJudge, "t"));
    for i in 1..=5 {
        e.cycle(std::slice::from_ref(&handler), &StopFlag::new());
        let row = e.row(JobKind::PhotoJudge, "t").unwrap();
        assert_eq!(row.attempts, i);
        assert_eq!(api.requests.lock().unwrap().len(), i as usize);
        if i < 5 {
            assert_eq!(row.state, JobState::Pending);
        } else {
            assert_eq!(row.state, JobState::Failed);
        }
        e.advance(4000);
    }
    e.cycle(&[handler], &StopFlag::new());
    assert_eq!(api.requests.lock().unwrap().len(), 5);
    assert_eq!(usage(&e), 5);
    assert_eq!(auth.requests.lock().unwrap().len(), 1);
}
