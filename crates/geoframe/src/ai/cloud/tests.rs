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
    std::fs::write(path, serde_json::to_vec(&json!({"client_email":"synthetic@example.invalid", "private_key":pem(), "token_uri":"http://untrusted.invalid/never"})).unwrap()).unwrap();
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
    assert_eq!(body["contents"][0]["parts"][1]["text"], "Image 17");
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

#[test]
fn sent_timeout_is_attempt_and_whole_deadline_includes_auth_and_refresh() {
    for slow_auth in [true, false] {
        let (_dir, e) = fixture(20);
        let auth = stub(move |s, _| {
            if slow_auth {
                std::thread::sleep(Duration::from_millis(160));
            }
            respond(s, "200 OK", "", &token_body())
        });
        let api = stub(|s, _| {
            std::thread::sleep(Duration::from_millis(160));
            respond(s, "200 OK", "", &good_reply())
        });
        let dns = FakeDns::new();
        let c = client(
            &e,
            transport(&api, "api.test", &dns),
            transport(&auth, "oauth.test", &dns),
        );
        let start = std::time::Instant::now();
        let result = call(
            &e,
            &c,
            &request(),
            &StopFlag::new(),
            &Deadline::after(Duration::from_millis(80)),
        )
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_millis(150));
        if slow_auth {
            assert!(matches!(result, RunError::ProviderWait(_)));
            assert_eq!(usage(&e), 0);
            assert_eq!(api.requests.lock().unwrap().len(), 0);
        } else {
            assert_eq!(result, RunError::Attempt(ErrorCode::Timeout));
            assert_eq!(usage(&e), 1);
        }
    }
    // first inference + refresh + retry share one deadline, rather than restarting it per HTTP request.
    let (_dir, e) = fixture(20);
    let n = Arc::new(AtomicUsize::new(0));
    let count = n.clone();
    let api = stub(move |s, _| {
        std::thread::sleep(Duration::from_millis(65));
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            respond(s, "401 Unauthorized", "", b"")
        } else {
            respond(s, "200 OK", "", &good_reply())
        }
    });
    let auth = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let c = client(
        &e,
        transport(&api, "api.test", &dns),
        transport(&auth, "oauth.test", &dns),
    );
    let start = std::time::Instant::now();
    let result = call(
        &e,
        &c,
        &request(),
        &StopFlag::new(),
        &Deadline::after(Duration::from_millis(110)),
    )
    .unwrap_err();
    assert_eq!(result, RunError::Attempt(ErrorCode::Timeout));
    assert!(start.elapsed() < Duration::from_millis(180));
    assert_eq!(usage(&e), 2);
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
    use std::io::Write;
    for wire in [true, false] {
        let api = stub(move |s, _| {
            if wire {
                let head =
                    b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n";
                if s.write_all(head).is_err() {
                    return;
                }
                let chunk = crate::testutil::gzip::padded(b"", 0).repeat(512);
                for _ in 0..8192 {
                    if s.write_all(&chunk).is_err() {
                        return;
                    }
                }
            } else {
                respond(
                    s,
                    "200 OK",
                    "Content-Encoding: gzip\r\n",
                    &crate::testutil::gzip::padded(b"", (1 << 20) + 1),
                );
            }
        });
        let dns = FakeDns::new();
        let t = transport(&api, "api.test", &dns);
        let e = t
            .post(
                "/generate",
                b"{}",
                Some("synthetic-token"),
                &deadline(),
                &StopFlag::new(),
            )
            .unwrap_err();
        assert_eq!(e.error, HttpError::ResponseTooLarge);
        assert!(e.started);
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
