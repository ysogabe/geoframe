//! Drive API の操作のログ（docs/note-photo-export/drive-api.md 6 章）: 出してよいのは、操作名・状態コード・再試行の回数・Drive のエラーの
//! `reason`（短い固定の語）だけ。資格情報・アクセストークン・ページトークン・セッションの URL・ファイル名・応答の本文は出さない。
//!
//! `tracing` の呼び出し箇所は、受け口の無いスレッドが先に通ると「関心なし」を覚えられて、あとから付けた受け口に届かないことがある。
//! ほかの試験とスレッドを共有しないよう、この試験だけで 1 つのファイルにしている（試験は 1 つだけ）。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::drive::*;
use common::fake_drive as fk;
use geoframe_export::drive::api::{DriveApi, Limit};
use geoframe_export::drive::auth::Credentials;
use geoframe_export::drive::http::Retry;

/// 1 件のイベントのフィールド（名前と、文字列にした値）。
type Event = Vec<(String, String)>;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Event>>>);

struct Fields<'a>(&'a mut Vec<(String, String)>);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.push((field.name().to_owned(), value.to_string()));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.push((field.name().to_owned(), value.to_string()));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Vec::new();
        event.record(&mut Fields(&mut fields));
        self.0.lock().unwrap().push(fields);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

const SECRET_NAME: &str = "SECRET-PHOTO-NAME-0001.jpg";
const SECRET_FOLDER: &str = "SECRET-FOLDER-NAME-0002";

#[test]
fn the_logs_carry_no_secrets_names_urls_or_bodies() {
    let env = Env::new("api-log");
    let fake = fk::FakeDrive::start(&env);
    let cred = fk::write_credentials(&env.root.path().join("cred"));
    let api = DriveApi::new(Credentials::load(&cred).unwrap(), fake.endpoints())
        .with_retry(Retry {
            max_retries: 3,
            base: Duration::from_millis(5),
            cap: Duration::from_millis(50),
        })
        .with_sizes(2, 100 * 1024, 256 * 1024);
    let d = env.drive();
    let (work, yamareco) = (
        d.resolve(&["geoframe-export-work"]).unwrap(),
        d.resolve(&["yamareco"]).unwrap(),
    );
    let body: Vec<u8> = (0..600_000).map(|i| (i % 251) as u8).collect();
    let local = env.root.path().join("local.bin");
    std::fs::write(&local, &body).unwrap();
    let md5 = model::md5_hex(&body);
    let small = env.root.path().join("small.bin");
    std::fs::write(&small, b"small").unwrap();
    let small_md5 = model::md5_hex(b"small");
    // 失敗・再試行の経路を全部通す
    env.add_rule("list", 0, 1, fk::rate_limit_403());
    env.add_rule("list", 2, 1, fk::too_many_requests(0));
    env.add_rule("list", 3, 1, fk::unauthorized());
    env.add_rule("list", 4, 1, fk::drop_connection());
    env.add_rule("list", 5, 1, fk::status(503));
    env.add_rule("create_folder", 0, 1, fk::do_then_drop());
    env.add_rule("move", 0, 1, fk::do_then_drop());
    env.add_rule("trash", 0, 1, fk::do_then_drop());
    fake.add_rule_contains("upload", "phase:chunk", 1, 1, fk::cut_after(300_000));
    fake.add_rule_contains("upload", "phase:chunk", 3, 1, fk::lose_session());

    let capture = Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        let dl = || api.deadline(Limit::Query);
        let up = || api.deadline(Limit::Upload);
        let folder = api.create_folder(&work, SECRET_FOLDER, dl()).unwrap();
        for i in 0..5 {
            let mut d = env.drive();
            d.new_node(
                &folder,
                &format!("{i}-{SECRET_NAME}"),
                model::Kind::File,
                1,
                None,
                None,
            );
            env.save(&d);
        }
        let listed = api.list_children(&folder, dl()).unwrap();
        assert_eq!(listed.len(), 5);
        for _ in 0..3 {
            let _ = api.list_children(&folder, dl());
        }
        api.upload(&folder, SECRET_NAME, &local, &md5, body.len() as u64, up())
            .unwrap();
        let _ = api.upload(
            &folder,
            &format!("s-{SECRET_NAME}"),
            &small,
            &small_md5,
            5,
            up(),
        );
        api.move_to(&folder, &work, &yamareco, SECRET_FOLDER, dl())
            .unwrap();
        api.trash(&folder, dl()).unwrap();
        // 再試行を使い切る（失敗のログ）。断られる（404・403）経路も
        env.add_rule("get", 0, 1000, fk::status(503));
        assert!(api.get(&folder, dl()).is_err());
        env.add_rule("list", 0, 1, fk::status(403));
        assert!(api.list_children(&yamareco, dl()).is_err());
        // 認可の失効
        fake.revoke_tokens();
        env.add_rule("token", 0, 1, fk::invalid_grant());
        assert!(api.list_children(&yamareco, dl()).is_err());
    });

    let events = capture.0.lock().unwrap().clone();
    assert!(
        events.len() >= 8,
        "再試行・失敗の経路のログが出ていません: {events:?}"
    );
    let text = events
        .iter()
        .flat_map(|e| e.iter().map(|(k, v)| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join("\n");
    let forbidden = [
        // 資格情報・トークン
        fk::FAKE_CLIENT_ID,
        fk::FAKE_CLIENT_SECRET,
        fk::FAKE_REFRESH_TOKEN,
        "fake-access-token",
        "Bearer",
        "Authorization",
        "refresh_token",
        // ページトークン・セッションの URL・URL そのもの
        "pageToken",
        "pt2",
        "upload_id",
        "sess-",
        "http://",
        "127.0.0.1",
        "uploadType",
        // ファイル名・応答の本文
        SECRET_NAME,
        SECRET_FOLDER,
        "fake drive error",
        "invalid_grant",
        "error_description",
        // 手元のパス
        env.root.path().to_str().unwrap(),
    ];
    for f in forbidden {
        assert!(!text.contains(f), "ログに {f:?} が出ています:\n{text}");
    }
    // 出ているのは、操作名・状態コード・再試行の回数・reason・失敗の種類だけ
    let names: std::collections::HashSet<&str> = events
        .iter()
        .flat_map(|e| e.iter().map(|(k, _)| k.as_str()))
        .collect();
    for k in &names {
        assert!(
            [
                "message", "op", "status", "reason", "retry", "retries", "failure", "pages",
                "entries", "restart"
            ]
            .contains(k),
            "想定していないフィールド {k}"
        );
    }
    assert!(
        events
            .iter()
            .any(|e| e.iter().any(|(k, v)| k == "op" && v == "list"))
    );
    assert!(
        events
            .iter()
            .any(|e| e.iter().any(|(k, v)| k == "status" && v == "429"))
    );
    assert!(events.iter().any(|e| {
        e.iter()
            .any(|(k, v)| k == "reason" && v == "rateLimitExceeded")
    }));
    assert!(events.iter().any(|e| e.iter().any(|(k, _)| k == "retry")));
}
