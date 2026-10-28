use super::*;
use crate::{
    ai::{
        cloud::tests::{pem, token_body, transport, write_key},
        tests_http::{FakeDns, respond, stub},
    },
    testutil::TempDir,
};
use serde_json::{Value, json};
use std::sync::{
    Barrier,
    atomic::{AtomicU64, Ordering},
};

#[test]
fn token_validation_expiry_margin_and_clock_rollback() {
    let t = parse_token(&token_body(), 1000).unwrap();
    assert!(t.valid_at(1000));
    assert!(t.valid_at(4539));
    assert!(!t.valid_at(4540));
    assert!(!t.valid_at(999));
    for (value, kind, expiry) in [
        ("", "Bearer", 3600),
        ("x\r\ny", "Bearer", 3600),
        ("a b", "Bearer", 3600),
        ("x", "Other", 3600),
        ("x", "Bearer", 0),
        ("x", "Bearer", 60),
        ("x", "Bearer", 3601),
    ] {
        assert!(
            parse_token(
                &serde_json::to_vec(
                    &json!({"access_token":value,"token_type":kind,"expires_in":expiry})
                )
                .unwrap(),
                1000
            )
            .is_none()
        );
    }
    assert!(parse_token(&serde_json::to_vec(&json!({"access_token":"a".repeat(MAX_TOKEN_BYTES+1),"token_type":"Bearer","expires_in":3600})).unwrap(),1000).is_none());
    for expiry in [json!(-1), json!(1.2), json!("3600"), Value::Null] {
        assert!(
            parse_token(
                &serde_json::to_vec(
                    &json!({"access_token":"x","token_type":"Bearer","expires_in":expiry})
                )
                .unwrap(),
                1000
            )
            .is_none()
        );
    }
    assert!(!format!("{t:?}").contains("synthetic-token"));
}

#[test]
fn cache_separates_credential_scope_audience_and_refreshes_before_expiry() {
    let dir = TempDir::new("cloud-cache");
    let p = dir.path().join("key");
    let q = dir.path().join("other-key");
    write_key(&p);
    write_key(&q);
    let server = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let http = transport(&server, "oauth.test", &dns);
    let auth = Auth::default();
    let stop = StopFlag::new();
    let deadline = Deadline::after(Duration::from_secs(3));
    let now = AtomicU64::new(1000);
    let clock = || Some(now.load(Ordering::SeqCst));
    let a = auth.token(&p, &http, &deadline, &stop, &clock).unwrap();
    let b = auth.token(&p, &http, &deadline, &stop, &clock).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    auth.token(&q, &http, &deadline, &stop, &clock).unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    {
        let cache = auth.cache.lock().unwrap();
        assert!(
            cache
                .get(&CacheKey {
                    credential: p.clone(),
                    scope: "drive.readonly",
                    audience: TOKEN_URL
                })
                .is_none()
        );
        assert!(
            cache
                .get(&CacheKey {
                    credential: p.clone(),
                    scope: SCOPE,
                    audience: "https://other.invalid"
                })
                .is_none()
        );
    }
    now.store(4540, Ordering::SeqCst);
    let fresh = auth.token(&p, &http, &deadline, &stop, &clock).unwrap();
    assert!(!Arc::ptr_eq(&a, &fresh));
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[test]
fn old_401_on_another_thread_cannot_remove_new_token() {
    let auth = Arc::new(Auth::default());
    let path = std::path::PathBuf::from("/synthetic/key");
    let key = Auth::key(&path);
    let old = Arc::new(parse_token(&token_body(), 1000).unwrap());
    let fresh = Arc::new(parse_token(&token_body(), 2000).unwrap());
    auth.cache.lock().unwrap().insert(key.clone(), old.clone());
    let barrier = Arc::new(Barrier::new(2));
    let worker = {
        let (auth, path, old, barrier) = (auth.clone(), path.clone(), old.clone(), barrier.clone());
        std::thread::spawn(move || {
            barrier.wait();
            auth.invalidate(&path, &old);
        })
    };
    auth.cache
        .lock()
        .unwrap()
        .insert(key.clone(), fresh.clone());
    barrier.wait();
    worker.join().unwrap();
    assert!(Arc::ptr_eq(
        auth.cache.lock().unwrap().get(&key).unwrap(),
        &fresh
    ));
    auth.invalidate(&path, &fresh);
    assert!(auth.cache.lock().unwrap().is_empty());
}

#[test]
fn exchange_io_does_not_hold_cache_mutex() {
    let dir = TempDir::new("cloud-cache-io");
    let path = dir.path().join("key");
    write_key(&path);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (a, b) = (entered.clone(), release.clone());
    let server = stub(move |s, _| {
        a.wait();
        b.wait();
        respond(s, "200 OK", "", &token_body())
    });
    let dns = FakeDns::new();
    let http = transport(&server, "oauth.test", &dns);
    let auth = Arc::new(Auth::default());
    let worker = {
        let auth = auth.clone();
        std::thread::spawn(move || {
            auth.token(
                &path,
                &http,
                &Deadline::after(Duration::from_secs(3)),
                &StopFlag::new(),
                &|| Some(1000),
            )
        })
    };
    entered.wait();
    assert!(auth.cache.try_lock().is_ok());
    release.wait();
    worker.join().unwrap().unwrap();
}

#[test]
fn missing_huge_fifo_json_and_bad_pem_are_fixed_auth_failures_without_dns() {
    let dir = TempDir::new("cloud-bad-key");
    let server = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let http = transport(&server, "oauth.test", &dns);
    for case in ["missing", "huge", "fifo", "json", "pem"] {
        let p = dir.path().join(case);
        match case {
            "huge" => std::fs::write(&p, vec![b' '; (64 << 10) + 1]).unwrap(),
            "fifo" => {
                assert!(
                    std::process::Command::new("mkfifo")
                        .arg(&p)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            "json" => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(&p, b"{synthetic-secret").unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            "pem" => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(
                    &p,
                    serde_json::to_vec(
                        &json!({"client_email":"synthetic-secret","private_key":"synthetic-secret"}),
                    )
                    .unwrap(),
                )
                .unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            _ => {}
        }
        let error = Auth::default()
            .token(
                &p,
                &http,
                &Deadline::after(Duration::from_secs(1)),
                &StopFlag::new(),
                &|| Some(1000),
            )
            .unwrap_err();
        assert_eq!(error, unauthorized());
        assert!(!format!("{error:?}").contains("synthetic"));
    }
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    // The shared signer still verifies the AI-specific assertion.
    let signer = Rs256Signer::from_pem(&pem()).unwrap();
    assert!(
        crate::jwt::verify(
            &signer
                .sign(&GoogleAssertion::new(
                    "synthetic@example.invalid",
                    SCOPE,
                    TOKEN_URL,
                    1000
                ))
                .unwrap(),
            signer.public_key_der()
        )
        .is_ok()
    );
}

#[test]
fn a_loosely_permitted_key_is_refused_before_any_dns_or_exchange() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("cloud-loose-key");
    let p = dir.path().join("key");
    write_key(&p);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    let server = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let http = transport(&server, "oauth.test", &dns);
    let error = Auth::default()
        .token(
            &p,
            &http,
            &Deadline::after(Duration::from_secs(1)),
            &StopFlag::new(),
            &|| Some(1000),
        )
        .unwrap_err();
    assert_eq!(error, unauthorized());
    assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn a_refused_key_logs_one_fixed_warning_without_path_and_keeps_the_pause_reason() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("cloud-loose-key-log");
    let p = dir.path().join("secret-key-name");
    write_key(&p);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    let server = stub(|s, _| respond(s, "200 OK", "", &token_body()));
    let dns = FakeDns::new();
    let http = transport(&server, "oauth.test", &dns);
    let auth = Auth::default();
    let logs = crate::testutil::capture::logs_of(|| {
        for _ in 0..3 {
            let error = auth
                .token(
                    &p,
                    &http,
                    &Deadline::after(Duration::from_secs(1)),
                    &StopFlag::new(),
                    &|| Some(1000),
                )
                .unwrap_err();
            assert_eq!(error, unauthorized());
        }
    });
    assert_eq!(logs.matches("権限または所有者が安全でない").count(), 1);
    assert!(logs.contains("release-pause"));
    assert!(!logs.contains("secret-key-name") && !logs.contains(dir.path().to_str().unwrap()));
    // 権限以外の失敗（壊れた JSON など）では、この警告を出さない。
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&p, "{").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let logs = crate::testutil::capture::logs_of(|| {
        assert_eq!(
            Auth::default()
                .token(
                    &p,
                    &http,
                    &Deadline::after(Duration::from_secs(1)),
                    &StopFlag::new(),
                    &|| Some(1000),
                )
                .unwrap_err(),
            unauthorized()
        );
    });
    assert!(!logs.contains("安全でない"));
}
