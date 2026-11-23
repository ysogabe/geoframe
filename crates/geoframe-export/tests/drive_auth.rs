//! 資格情報の読み込みと、アクセストークンの取得・保持の試験（`geoframe_export::drive::auth`）。相手は偽の Drive の HTTP サーバー
//! （`tests/common/fake_drive.rs`）。値は全部、架空。本物の Google には一切つながない。
//!
//! 見るもの: ファイルの検査（権限・種類・大きさ・キー・リンク）、`invalid_grant` が `drive_auth_invalid` になること（再試行しない）、
//! 一時的な失敗は再試行すること、トークンはメモリだけで資格情報のファイルを書かないこと（更新時刻・中身・ディレクトリが変わらない）、
//! 期限の 60 秒前に取り直すこと。

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use common::drive::*;
use common::fake_drive as fk;
use geoframe_export::drive::api::{DriveApi, Limit};
use geoframe_export::drive::auth::{Credentials, CredentialsError};
use geoframe_export::drive::http::{ApiError, Retry};
use geoframe_export::error::ErrorCode;
use geoframe_export::remote::RemoteError;
use serde_json::json;

fn fast() -> Retry {
    Retry {
        max_retries: 3,
        base: Duration::from_millis(5),
        cap: Duration::from_millis(100),
    }
}

fn api(fake: &fk::FakeDrive, cred: &std::path::Path) -> DriveApi {
    DriveApi::new(
        Credentials::load(cred).expect("credentials"),
        fake.endpoints(),
    )
    .with_retry(fast())
}

// ---- ファイルの検査 ----

fn valid_body() -> String {
    json!({"client_id": "i", "client_secret": "s", "refresh_token": "r"}).to_string()
}

fn write(env: &Env, name: &str, body: &[u8], mode: u32) -> PathBuf {
    let dir = env.root.path().join("cred");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn a_valid_file_with_owner_only_permissions_is_read() {
    let env = Env::new("auth-ok");
    for mode in [0o600, 0o400] {
        let p = write(&env, "ok.json", valid_body().as_bytes(), mode);
        assert!(Credentials::load(&p).is_ok(), "{mode:o}");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn permissions_for_the_group_or_others_are_refused() {
    let env = Env::new("auth-mode");
    for mode in [0o640, 0o604, 0o644, 0o660, 0o606, 0o777, 0o610] {
        let p = write(&env, "m.json", valid_body().as_bytes(), mode);
        assert_eq!(
            Credentials::load(&p).unwrap_err(),
            CredentialsError::TooPermissive,
            "{mode:o}"
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn links_directories_and_missing_files_are_refused() {
    let env = Env::new("auth-kind");
    let real = write(&env, "real.json", valid_body().as_bytes(), 0o600);
    let link = real.with_file_name("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(
        Credentials::load(&link),
        Err(CredentialsError::Unreadable(_))
    ));
    assert_eq!(
        Credentials::load(real.parent().unwrap()).unwrap_err(),
        CredentialsError::NotRegularFile
    );
    assert!(matches!(
        Credentials::load(&real.with_file_name("none.json")),
        Err(CredentialsError::Unreadable(std::io::ErrorKind::NotFound))
    ));
    // FIFO は、開いても読みにいかない（止まらない）
    let fifo = real.with_file_name("fifo");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(
        Credentials::load(&fifo).unwrap_err(),
        CredentialsError::NotRegularFile
    );
}

#[test]
fn the_size_limit_is_16_kib() {
    let env = Env::new("auth-size");
    let pad = |n: usize| {
        let mut b = valid_body().into_bytes();
        b.resize(n, b' ');
        b
    };
    let ok = write(&env, "ok.json", &pad(16 * 1024), 0o600);
    assert!(Credentials::load(&ok).is_ok());
    let big = write(&env, "big.json", &pad(16 * 1024 + 1), 0o600);
    assert_eq!(
        Credentials::load(&big).unwrap_err(),
        CredentialsError::TooLarge
    );
}

#[test]
fn only_the_three_keys_are_accepted() {
    let env = Env::new("auth-keys");
    let cases: [(&str, CredentialsError); 8] = [
        (
            r#"{"client_id":"i","client_secret":"s","refresh_token":"r","token_uri":"https://x"}"#,
            CredentialsError::BadKeys("unknown_field"),
        ),
        (
            r#"{"client_id":"i","client_id":"i","client_secret":"s","refresh_token":"r"}"#,
            CredentialsError::BadKeys("duplicate_field"),
        ),
        (
            r#"{"client_id":"i","client_secret":"s"}"#,
            CredentialsError::MissingKey("refresh_token"),
        ),
        (
            r#"{"client_id":"","client_secret":"s","refresh_token":"r"}"#,
            CredentialsError::BadValue("client_id"),
        ),
        (
            r#"{"client_id":"i","client_secret":"s","refresh_token":null}"#,
            CredentialsError::BadValue("refresh_token"),
        ),
        (r#"["client_id"]"#, CredentialsError::NotJson),
        (r#"not json"#, CredentialsError::NotJson),
        (
            r#"{"client_id":"i","client_secret":"s","refresh_token":"a\u0000b"}"#,
            CredentialsError::BadValue("refresh_token"),
        ),
    ];
    for (body, want) in cases {
        let p = write(&env, "k.json", body.as_bytes(), 0o600);
        assert_eq!(Credentials::load(&p).unwrap_err(), want, "{body}");
    }
    // 読めなかったときの表示に、値が出ない
    let p = write(
        &env,
        "k.json",
        br#"{"client_id":"LEAKY-ID","client_secret":"LEAKY-SECRET","refresh_token":"LEAKY-TOKEN","x":1}"#,
        0o600,
    );
    let text = format!(
        "{} {:?}",
        Credentials::load(&p).unwrap_err(),
        Credentials::load(&p).unwrap_err()
    );
    assert!(!text.contains("LEAKY"), "{text}");
    let c = Credentials::from_values("LEAKY-ID", "LEAKY-SECRET", "LEAKY-TOKEN").unwrap();
    assert!(!format!("{c:?}").contains("LEAKY"));
}

// ---- トークン ----

#[test]
fn one_token_is_fetched_and_reused_without_writing_the_credentials_file() {
    let env = Env::new("auth-reuse");
    let fake = fk::FakeDrive::start(&env);
    let dir = env.root.path().join("cred");
    let cred = fk::write_credentials(&dir);
    // 更新時刻・中身・ディレクトリの中身を、操作の前後で比べる
    let past = std::time::SystemTime::now() - Duration::from_secs(7 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&cred)
        .unwrap()
        .set_modified(past)
        .unwrap();
    let before = (
        std::fs::metadata(&cred).unwrap().modified().unwrap(),
        std::fs::read(&cred).unwrap(),
        std::fs::read_dir(&dir).unwrap().count(),
    );
    let api = api(&fake, &cred);
    let root = env.drive().resolve(&["yamareco"]).unwrap();
    for _ in 0..3 {
        api.list_children(&root, api.deadline(Limit::Query))
            .unwrap();
    }
    assert_eq!(env.op_count("token"), 1);
    assert_eq!(fake.tokens_issued(), 1);
    // トークンの更新（無効にされた → 取り直し）のあとも、ファイルは書かれない
    fake.revoke_tokens();
    api.list_children(&root, api.deadline(Limit::Query))
        .unwrap();
    assert_eq!(fake.tokens_issued(), 2);
    let after = (
        std::fs::metadata(&cred).unwrap().modified().unwrap(),
        std::fs::read(&cred).unwrap(),
        std::fs::read_dir(&dir).unwrap().count(),
    );
    assert_eq!(before, after);
    assert_eq!(
        std::fs::metadata(&cred).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_token_close_to_its_expiry_is_fetched_again() {
    let env = Env::new("auth-expiry");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    let root = env.drive().resolve(&["yamareco"]).unwrap();
    // 寿命が 60 秒以下のトークンは、使うたびに取り直す（期限の 60 秒前に取り直す）
    fake.set_token_lifetime(30);
    for n in 1..=3 {
        api.list_children(&root, api.deadline(Limit::Query))
            .unwrap();
        assert_eq!(fake.tokens_issued(), n);
    }
    // 寿命が十分あれば、取り直さない
    fake.set_token_lifetime(3599);
    api.list_children(&root, api.deadline(Limit::Query))
        .unwrap();
    assert_eq!(fake.tokens_issued(), 4);
    api.list_children(&root, api.deadline(Limit::Query))
        .unwrap();
    assert_eq!(fake.tokens_issued(), 4);
}

#[test]
fn invalid_grant_is_drive_auth_invalid_and_is_not_retried() {
    let env = Env::new("auth-grant");
    let fake = fk::FakeDrive::start(&env);
    // 更新トークンが違う → 偽のサーバーが invalid_grant を返す
    let cred = fk::write_credentials_with(&env.root.path().join("cred"), "REVOKED-REFRESH-TOKEN");
    let api = api(&fake, &cred);
    let root = env.drive().resolve(&["yamareco"]).unwrap();

    let e = api
        .check_credentials(api.deadline(Limit::Query))
        .unwrap_err();
    assert_eq!(e, ApiError::AuthInvalid);
    assert_eq!(env.op_count("token"), 1, "再試行しない");
    // 照会でも書き込みでも、ジョブのエラーコードは drive_auth_invalid
    let remote = RemoteError::from(e);
    assert_eq!(remote.into_query_error().code, ErrorCode::DriveAuthInvalid);
    assert_eq!(remote.into_op_error().code, ErrorCode::DriveAuthInvalid);
    assert_eq!(
        api.list_children(&root, api.deadline(Limit::Query)),
        Err(ApiError::AuthInvalid)
    );
    assert_eq!(
        api.create_folder(&root, "x", api.deadline(Limit::Query)),
        Err(ApiError::AuthInvalid)
    );
    // API には 1 度も出ていない
    assert!(env.ops().iter().all(|o| o == "token"), "{:?}", env.ops());
    assert_eq!(ErrorCode::DriveAuthInvalid.as_str(), "drive_auth_invalid");
}

#[test]
fn invalid_grant_in_the_middle_of_a_session_surfaces_on_the_next_refresh() {
    let env = Env::new("auth-grant-mid");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    let root = env.drive().resolve(&["yamareco"]).unwrap();
    api.list_children(&root, api.deadline(Limit::Query))
        .unwrap();
    // 認可が取り消された: トークンが無効になり、取り直しが invalid_grant になる
    fake.revoke_tokens();
    env.add_rule("token", 0, 1, fk::invalid_grant());
    assert_eq!(
        api.list_children(&root, api.deadline(Limit::Query)),
        Err(ApiError::AuthInvalid)
    );
    assert_eq!(env.op_count("token"), 2);
}

#[test]
fn invalid_client_is_an_auth_failure_and_other_4xx_are_not() {
    let env = Env::new("auth-client");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    let dl = || api.deadline(Limit::Query);
    env.add_rule(
        "token",
        0,
        1,
        json!({"kind": "status", "status": 401, "oauth_error": "invalid_client"}),
    );
    assert_eq!(api.check_credentials(dl()), Err(ApiError::AuthInvalid));
    env.add_rule(
        "token",
        0,
        1,
        json!({"kind": "status", "status": 400, "oauth_error": "unauthorized_client"}),
    );
    assert_eq!(api.check_credentials(dl()), Err(ApiError::AuthInvalid));
    // こちらの組み立ての誤りか Google 側の不調と言えるものは、認可の失効にしない（drive_unavailable）。再試行もしない
    let before = env.op_count("token");
    env.add_rule(
        "token",
        0,
        1,
        json!({"kind": "status", "status": 400, "oauth_error": "invalid_request"}),
    );
    let e = api.check_credentials(dl()).unwrap_err();
    assert_eq!(e, ApiError::Status(400));
    assert_eq!(env.op_count("token"), before + 1);
    assert_eq!(
        RemoteError::from(e).into_query_error().code,
        ErrorCode::DriveUnavailable
    );
    // 本文が OAuth のエラーでない 403 も同じ
    env.add_rule("token", 0, 1, fk::status(403));
    assert_eq!(api.check_credentials(dl()), Err(ApiError::Status(403)));
}

#[test]
fn transient_token_failures_are_retried() {
    let env = Env::new("auth-transient");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    for effect in [
        fk::status(503),
        fk::too_many_requests(0),
        fk::drop_connection(),
    ] {
        let env2 = Env::new("auth-transient-each");
        let fake2 = fk::FakeDrive::start(&env2);
        let api2 = self::api(&fake2, &cred);
        env2.add_rule("token", 0, 2, effect.clone());
        api2.check_credentials(api2.deadline(Limit::Query)).unwrap();
        assert_eq!(env2.op_count("token"), 3, "{effect}");
    }
    // 使い切ったら失敗（認可の失効とは言わない）
    env.add_rule("token", 0, 1000, fk::status(503));
    let e = api
        .check_credentials(api.deadline(Limit::Query))
        .unwrap_err();
    assert_eq!(e, ApiError::Status(503));
    assert_eq!(env.op_count("token"), 4);
    assert_eq!(
        RemoteError::from(e).into_query_error().code,
        ErrorCode::DriveUnavailable
    );
    let _ = fake;
}

#[test]
fn the_token_request_is_a_urlencoded_form_and_never_carries_an_authorization_header() {
    let env = Env::new("auth-form");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    api.check_credentials(api.deadline(Limit::Query)).unwrap();
    let t = &fake.requests()[0];
    assert_eq!(
        (
            t["op"].as_str(),
            t["method"].as_str(),
            t["authorization"].as_str()
        ),
        (Some("token"), Some("POST"), Some("missing"))
    );
    // 偽のサーバーは、grant_type・client_id・client_secret・refresh_token が架空の値と一致したときだけトークンを返す
    assert_eq!(fake.tokens_issued(), 1);
}

#[test]
fn a_bad_token_response_is_a_bad_response() {
    let env = Env::new("auth-bad-token");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = api(&fake, &cred);
    env.add_rule("token", 0, 1, garbage("{\"access_token\":\"\"}"));
    assert_eq!(
        api.check_credentials(api.deadline(Limit::Query)),
        Err(ApiError::BadResponse)
    );
    env.add_rule("token", 0, 1, garbage("not json"));
    assert_eq!(
        api.check_credentials(api.deadline(Limit::Query)),
        Err(ApiError::BadResponse)
    );
}
