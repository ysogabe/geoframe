//! Drive の単体テスト。Drive は合成のスタブ（`stub.rs`。ループバックの HTTP）だけを使い、実際の
//! 通信はしない。鍵は `tests/fixtures/jwt-test-*.b64` の使い捨ての RSA 鍵、写真は合成のバイト列
//! （EXIF は `tests/fixtures/exif-python.json` の合成 JPEG）。Python の `index_drive` との列ごとの
//! 突き合わせは `drive_python` のテスト（`tests/fixtures/drive-index-python.json`）。

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::stub::{StubDrive, item};
use super::*;
use crate::backoff::FailureMemo;
use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::jwt::Rs256Signer;
use crate::sync::{Counts, Mode, Stage, StageCtx, StageOutcome, StageResult};
use crate::testutil::{TempDir, capture};
use crate::writer::{OpenMode, Writer};
use std::sync::atomic::Ordering;

const EMAIL: &str = "sa@example.invalid";
const FOLDER_NAME: &str = "Frame Test";
const FOLDER_ID: &str = "folder_id_0000000000001";

pub(super) fn pem() -> String {
    let b64 = include_str!("../../tests/fixtures/jwt-test-pkcs8.b64").trim();
    let lines: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    )
}

fn fixture_jpeg(name: &str) -> Vec<u8> {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/exif-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let case = doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(case["jpeg_base64"].as_str().unwrap())
        .unwrap()
}

fn heic() -> Vec<u8> {
    fs::read(format!(
        "{}/tests/fixtures/synthetic_o1.heic",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn png() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(12, 8, image::Rgb([200, 30, 30]))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn tiff() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(12, 8, image::Rgb([30, 200, 30]))
        .write_to(&mut out, image::ImageFormat::Tiff)
        .unwrap();
    out.into_inner()
}

/// 画像として読めないバイト列。
fn broken() -> Vec<u8> {
    b"this is not an image".to_vec()
}

const FAST_RETRY: Retry = Retry {
    max_retries: 3,
    base: Duration::from_millis(1),
    cap: Duration::from_millis(5),
};

struct Env {
    dir: TempDir,
    config: Arc<Config>,
    writer: Writer,
    cache: Cache,
    stub: StubDrive,
    shared: Arc<DriveShared>,
    key: PathBuf,
}

fn env(tag: &str) -> Env {
    env_with(tag, &[])
}

fn env_with(tag: &str, extra: &[(&str, &str)]) -> Env {
    let dir = TempDir::new(tag);
    let key = dir.path().join("drive_key.json");
    fs::write(
        &key,
        json!({"client_email": EMAIL, "private_key": pem()}).to_string(),
    )
    .unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let mut vars: HashMap<String, String> = HashMap::from([
        ("GEOFRAME_DRIVE_KEY".into(), key.display().to_string()),
        ("GEOFRAME_DRIVE_FOLDER".into(), FOLDER_NAME.into()),
    ]);
    for (k, v) in extra {
        vars.insert(k.to_string(), v.to_string());
    }
    let overrides = Overrides {
        listen: None,
        data_dir: Some(dir.path().to_path_buf()),
    };
    let config = Arc::new(Config::load(|k| vars.get(k).cloned(), &overrides).unwrap());
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    let public = Rs256Signer::from_pem(&pem())
        .unwrap()
        .public_key_der()
        .to_vec();
    let stub = StubDrive::start(public, EMAIL, FOLDER_NAME, FOLDER_ID);
    let shared = Arc::new(DriveShared::with(
        Endpoints::stub(&stub.base_url()),
        FAST_RETRY,
    ));
    let cache = Cache::load(dir.path());
    Env {
        dir,
        config,
        writer,
        cache,
        stub,
        shared,
        key,
    }
}

/// 1 行ぶんの列（比較用）。
type Row = Vec<(String, rusqlite::types::Value)>;

impl Env {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn index_stage(&self) -> IndexDrive {
        IndexDrive::new(Arc::clone(&self.shared))
    }

    fn fetch_stage(&self) -> FetchDerivatives {
        FetchDerivatives::new(Arc::clone(&self.shared))
    }

    fn read_for(&self, mode: Mode) -> Connection {
        let real = crate::db::open_read_only(&self.config.db_path()).unwrap();
        match mode {
            Mode::Apply => real,
            Mode::DryRun => crate::sync::simulation_copy(&real).unwrap(),
        }
    }

    fn run_with(&self, stage: &dyn Stage, mode: Mode, stop: &StopFlag) -> StageResult {
        let read = self.read_for(mode);
        self.run_on(stage, mode, &read, stop)
    }

    fn run_on(
        &self,
        stage: &dyn Stage,
        mode: Mode,
        read: &Connection,
        stop: &StopFlag,
    ) -> StageResult {
        self.run_on_http(stage, mode, read, stop, &crate::net::agent())
    }

    /// 通信の設定（時間切れなど）を差し替えて実行する。
    fn run_with_http(&self, stage: &dyn Stage, http: &ureq::Agent) -> StageResult {
        let read = self.read_for(Mode::Apply);
        self.run_on_http(stage, Mode::Apply, &read, &StopFlag::new(), http)
    }

    fn run_on_http(
        &self,
        stage: &dyn Stage,
        mode: Mode,
        read: &Connection,
        stop: &StopFlag,
        http: &ureq::Agent,
    ) -> StageResult {
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read,
            writer: (mode == Mode::Apply).then_some(&self.writer),
            cache: &self.cache,
            http,
            stop,
        };
        match stage.run(&ctx) {
            Ok(StageOutcome::Done(counts)) => StageResult::Done(counts),
            Ok(StageOutcome::Skipped(reason)) => StageResult::Skipped(reason),
            Err(e) => StageResult::Failed(e.to_string()),
        }
    }

    fn run(&self, stage: &dyn Stage) -> StageResult {
        self.run_with(stage, Mode::Apply, &StopFlag::new())
    }

    fn done(&self, stage: &dyn Stage) -> Counts {
        match self.run(stage) {
            StageResult::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn index(&self) -> Counts {
        self.done(&self.index_stage())
    }

    fn rows(&self) -> Vec<Row> {
        let conn = crate::db::open_read_only(&self.config.db_path()).unwrap();
        let mut stmt = conn.prepare("SELECT * FROM photos ORDER BY id").unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        stmt.query_map([], |row| {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| Ok((n.clone(), row.get::<_, rusqlite::types::Value>(i)?)))
                .collect()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn sql(&self, sql: &'static str) {
        self.writer.call(move |c| c.execute_batch(sql)).unwrap();
    }

    fn listing(&self, items: Vec<Value>) {
        self.stub.set_items(items);
    }

    fn tmp_leftovers(&self) -> Vec<String> {
        let dir = self.root().join("tmp");
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}

fn get(counts: &Counts, name: &str) -> u64 {
    counts
        .0
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |(_, v)| *v)
}

fn col<'a>(row: &'a Row, name: &str) -> &'a rusqlite::types::Value {
    &row.iter().find(|(n, _)| n == name).unwrap().1
}

fn text(row: &Row, name: &str) -> Option<String> {
    match col(row, name) {
        rusqlite::types::Value::Text(s) => Some(s.clone()),
        rusqlite::types::Value::Null => None,
        other => panic!("{name}: {other:?}"),
    }
}

fn jpeg(id: &str, name: &str, md5: &str) -> Value {
    item(id, name, Some(md5), "image/jpeg", None)
}

fn located(id: &str, name: &str, md5: &str, time: &str, lat: f64, lon: f64) -> Value {
    item(
        id,
        name,
        Some(md5),
        "image/jpeg",
        Some(json!({"time": time, "location": {"latitude": lat, "longitude": lon}})),
    )
}

// ---------------------------------------------------------------------------
// 小さな関数

#[test]
fn the_folder_setting_is_an_id_only_when_python_would_say_so() {
    assert!(looks_like_folder_id("folder_id_0000000000001"));
    assert!(looks_like_folder_id(&"A-_z9".repeat(4)));
    assert!(!looks_like_folder_id("GeoFrame"));
    assert!(!looks_like_folder_id("0123456789012345678")); // 19 字
    assert!(looks_like_folder_id("01234567890123456789")); // 20 字
    assert!(!looks_like_folder_id("folder id 0000000000001")); // 空白
    assert!(!looks_like_folder_id("フォルダ1234567890123456789"));
}

#[test]
fn query_literals_escape_quotes_and_backslashes() {
    assert_eq!(escape_query_literal("GeoFrame"), "GeoFrame");
    assert_eq!(escape_query_literal("it's"), "it\\'s");
    assert_eq!(escape_query_literal("a\\b'c"), "a\\\\b\\'c");
}

#[test]
fn retry_waits_double_and_are_capped() {
    let retry = Retry::production();
    let secs = |attempt, after| retry.wait_for(attempt, after).as_secs();
    assert_eq!(
        (0..5).map(|a| secs(a, None)).collect::<Vec<_>>(),
        [1, 2, 4, 8, 16]
    );
    assert_eq!(secs(10, None), 30);
    // Retry-After があればそれ（上限まで）
    assert_eq!(secs(0, Some(Duration::from_secs(7))), 7);
    assert_eq!(secs(0, Some(Duration::from_secs(600))), 30);
}

#[test]
fn errors_never_carry_secrets_or_urls() {
    for e in [
        DriveError::Key(KeyError::Malformed),
        DriveError::Key(KeyError::Unreadable(io::ErrorKind::NotFound)),
        DriveError::Jwt(JwtError::Key),
        DriveError::Net(Failure::Tls),
        DriveError::Net(Failure::Disconnected),
        DriveError::Status(503),
        DriveError::TokenRejected(400),
        DriveError::BadResponse,
        DriveError::TooLarge,
        DriveError::Io(io::ErrorKind::TimedOut),
    ] {
        let s = e.to_string();
        assert!(
            !s.contains("http") && !s.contains("Bearer") && !s.contains("PRIVATE"),
            "{s}"
        );
    }
    assert!(DriveError::Status(429).is_outage());
    assert!(DriveError::Status(503).is_outage());
    assert!(DriveError::Status(401).is_outage());
    assert!(!DriveError::Status(404).is_outage());
    assert!(!DriveError::Status(403).is_outage());
    assert!(DriveError::Net(Failure::Dns).is_outage());
    // 接続・応答の待ちの時間切れと流量制限は障害（写真のせいではない）。権限の 403 と本文の時間切れは写真の失敗
    assert!(DriveError::Net(Failure::Timeout).is_outage());
    assert!(DriveError::Net(Failure::Tls).is_outage());
    assert!(DriveError::Net(Failure::Disconnected).is_outage());
    assert!(!DriveError::Net(Failure::Other).is_outage());
    assert!(DriveError::Net(Failure::ReasonUnreadable).is_outage());
    assert!(DriveError::RateLimited(403).is_outage());
    assert!(!DriveError::BodyTimeout.is_outage());
    assert!(!DriveError::RateLimited(403).is_auth());
    assert!(DriveError::TokenRejected(400).is_auth());
    assert!(!DriveError::Status(503).is_auth());
}

// ---------------------------------------------------------------------------
// 認証

#[test]
fn without_a_key_both_stages_are_skipped_and_nothing_is_sent() {
    let e = env("drive-nokey");
    fs::remove_file(&e.key).unwrap();
    assert_eq!(e.run(&e.index_stage()), StageResult::Skipped(index::NO_KEY));
    assert_eq!(e.run(&e.fetch_stage()), StageResult::Skipped(index::NO_KEY));
    assert!(e.stub.requests().is_empty());
    assert!(e.rows().is_empty());
}

#[test]
fn a_broken_key_skips_both_stages_without_sending_anything() {
    let e = env("drive-badkey");
    for contents in [
        "not json",
        "{}",
        r#"{"client_email":"a","private_key":"x"}"#,
    ] {
        fs::write(&e.key, contents).unwrap();
        e.sql("DELETE FROM photos");
        assert_eq!(
            e.run(&e.index_stage()),
            StageResult::Skipped(index::AUTH_FAILED),
            "{contents}"
        );
        // 同じサイクルの fetch も飛ばす（現行は `svc` が作れず両方飛ばす）
        assert_eq!(
            e.run(&e.fetch_stage()),
            StageResult::Skipped(index::AUTH_FAILED)
        );
    }
    assert!(e.stub.requests().is_empty());
}

#[test]
fn the_token_exchange_sends_a_signed_assertion_and_the_token_is_shared_by_both_stages() {
    let e = env("drive-token");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.stub.set_media("f1", fixture_jpeg("typical"));
    e.index();
    e.done(&e.fetch_stage());
    // スタブが assertion の署名・iss・scope・aud・exp を検証して発行する。両方の段で 1 回だけ
    assert_eq!(e.stub.tokens_issued(), 1);
    let requests = e.stub.requests();
    assert!(requests.iter().filter(|r| r.path == "/token").count() == 1);
    assert!(
        requests
            .iter()
            .filter(|r| r.path != "/token")
            .all(|r| r.auth_ok)
    );
    assert!(e.stub.media_requests().len() == 1);
}

#[test]
fn a_rejected_assertion_skips_as_an_auth_error_but_an_outage_fails() {
    // 署名を受け付けない（400）→ 認証エラー（飛ばす）
    let e = env("drive-reject");
    e.stub.add_fault(Box::new(|r| {
        (r.path == "/token").then(|| (400, r#"{"error":"invalid_grant"}"#.to_string(), vec![]))
    }));
    assert_eq!(
        e.run(&e.index_stage()),
        StageResult::Skipped(index::AUTH_FAILED)
    );
    assert_eq!(
        e.run(&e.fetch_stage()),
        StageResult::Skipped(index::AUTH_FAILED)
    );

    // Google 側の不調（500 が続く）→ 認証の失敗ではないので、段の失敗
    let e = env("drive-outage");
    e.stub.add_fault(Box::new(|r| {
        (r.path == "/token").then(|| (500, "{}".to_string(), vec![]))
    }));
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("500"), "{message}");
    assert!(!message.contains("http"), "{message}");
    // 再試行された（最初の 1 回 + 3 回）
    assert_eq!(
        e.stub
            .requests()
            .iter()
            .filter(|r| r.path == "/token")
            .count(),
        4
    );
}

#[test]
fn an_expiring_token_is_renewed_and_a_revoked_one_is_replaced_once() {
    let e = env("drive-renew");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    // 期限の 60 秒前を切る寿命: 使うたびに取り直す（鍵の確認・フォルダ・一覧のページごと）
    e.stub.set_expires_in(30);
    e.index();
    let first = e.stub.tokens_issued();
    assert!(first >= 3, "{first}");
    e.index();
    assert!(e.stub.tokens_issued() > first);

    // 長い寿命: 使い回す
    let e = env("drive-reuse");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.index();
    e.index();
    assert_eq!(e.stub.tokens_issued(), 1);

    // 失効（401）→ 取り直して 1 回だけやり直す
    e.stub.revoke_tokens();
    e.index();
    assert_eq!(e.stub.tokens_issued(), 2);
    let unauthorized = e
        .stub
        .requests()
        .iter()
        .filter(|r| r.path != "/token" && !r.auth_ok)
        .count();
    assert!(unauthorized >= 1);
}

#[test]
fn the_key_is_never_followed_into_a_directory_or_a_huge_file() {
    let e = env("drive-keykinds");
    fs::remove_file(&e.key).unwrap();
    fs::create_dir(&e.key).unwrap();
    assert_eq!(
        e.run(&e.index_stage()),
        StageResult::Skipped(index::AUTH_FAILED)
    );
    fs::remove_dir(&e.key).unwrap();
    // FIFO を開くと書き手が来るまで止まる。開く前に種類を見て断る
    let made = std::process::Command::new("mkfifo").arg(&e.key).status();
    assert!(made.is_ok_and(|s| s.success()), "mkfifo を実行できません");
    assert_eq!(
        e.run(&e.index_stage()),
        StageResult::Skipped(index::AUTH_FAILED)
    );
    fs::remove_file(&e.key).unwrap();
    fs::File::create(&e.key)
        .unwrap()
        .set_len(MAX_KEY_BYTES + 1)
        .unwrap();
    assert_eq!(
        e.run(&e.index_stage()),
        StageResult::Skipped(index::AUTH_FAILED)
    );
    // 権限が緩くても読む（警告するだけ）
    fs::write(
        &e.key,
        json!({"client_email": EMAIL, "private_key": pem()}).to_string(),
    )
    .unwrap();
    fs::set_permissions(&e.key, fs::Permissions::from_mode(0o644)).unwrap();
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    assert_eq!(get(&e.index(), "added"), 1);
    assert!(e.shared.permission_warned.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// 索引

#[test]
fn index_adds_rows_in_listing_order_with_the_python_columns() {
    let e = env("drive-add");
    e.listing(vec![
        located("f3", "c.jpg", "m3", "2024:05:06 07:08:09", 35.5, 139.25),
        jpeg("f1", "a.jpg", "m1"),
        // 取り込まない MIME（フォルダ・動画・GIF）
        item(
            "d1",
            "sub",
            None,
            "application/vnd.google-apps.folder",
            None,
        ),
        item("v1", "m.mp4", Some("mv"), "video/mp4", None),
        item("g1", "g.gif", Some("mg"), "image/gif", None),
        item(
            "f2",
            "b.heic",
            Some("m2"),
            "image/heic",
            Some(json!({"time": "2023-01-02T03:04:05"})),
        ),
        // 緯度だけ（現行は lat と lon を独立に取る）
        item(
            "f4",
            "d.jpg",
            None,
            "image/jpeg",
            Some(json!({"location": {"latitude": 1.5}})),
        ),
    ]);
    let counts = e.index();
    assert_eq!(get(&counts, "remote"), 4);
    assert_eq!(get(&counts, "pages"), 4); // 7 件 / ページ 2 件
    assert_eq!(get(&counts, "added"), 4);
    assert_eq!(get(&counts, "updated"), 0);
    assert_eq!(get(&counts, "removed"), 0);

    let rows = e.rows();
    let ids: Vec<_> = rows.iter().map(|r| text(r, "drive_id").unwrap()).collect();
    assert_eq!(ids, ["f3", "f1", "f2", "f4"]); // 一覧の順に id が振られる
    let r = &rows[0];
    assert_eq!(text(r, "source").as_deref(), Some("drive"));
    assert_eq!(text(r, "name").as_deref(), Some("c.jpg"));
    assert_eq!(text(r, "md5").as_deref(), Some("m3"));
    assert_eq!(text(r, "taken_at").as_deref(), Some("2024-05-06 07:08"));
    assert_eq!(col(r, "lat"), &rusqlite::types::Value::Real(35.5));
    assert_eq!(col(r, "lon"), &rusqlite::types::Value::Real(139.25));
    assert_eq!(text(r, "loc_source").as_deref(), Some("drive_meta"));
    let indexed = text(r, "indexed_at").unwrap();
    assert_eq!(indexed.len(), 19);
    assert_eq!(&indexed[10..11], "T");
    assert!(text(r, "path").is_none() && text(r, "display").is_none());
    // 位置の無い写真
    assert_eq!(text(&rows[1], "taken_at").as_deref(), Some("")); // NULL にしない
    assert!(text(&rows[1], "loc_source").is_none());
    assert_eq!(col(&rows[1], "lat"), &rusqlite::types::Value::Null);
    // 時刻は `T` 区切りでも正規化する
    assert_eq!(
        text(&rows[2], "taken_at").as_deref(),
        Some("2023-01-02 03:04")
    );
    // md5 が無い写真は md5 が NULL
    assert!(text(&rows[3], "md5").is_none());
    // 緯度だけ: lat あり・lon NULL・loc_source は drive_meta
    assert_eq!(col(&rows[3], "lat"), &rusqlite::types::Value::Real(1.5));
    assert_eq!(col(&rows[3], "lon"), &rusqlite::types::Value::Null);
    assert_eq!(text(&rows[3], "loc_source").as_deref(), Some("drive_meta"));
}

#[test]
fn index_asks_drive_exactly_what_python_asks() {
    let e = env("drive-query");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.index();
    let requests = e.stub.requests();
    // フォルダ名の解決（pageSize=5）
    let lookup = requests
        .iter()
        .find(|r| r.param("q").is_some_and(|q| q.starts_with("name=")))
        .unwrap();
    assert_eq!(
        lookup.param("q"),
        Some(
            "name='Frame Test' and mimeType='application/vnd.google-apps.folder' and trashed=false"
        )
    );
    assert_eq!(lookup.param("fields"), Some("files(id)"));
    assert_eq!(lookup.param("pageSize"), Some("5"));
    let list = e.stub.list_requests();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].param("q"),
        Some("'folder_id_0000000000001' in parents and trashed=false")
    );
    assert_eq!(
        list[0].param("fields"),
        Some("nextPageToken,files(id,name,md5Checksum,mimeType,imageMediaMetadata(time,location))")
    );
    assert_eq!(list[0].param("pageSize"), Some("1000"));
    assert!(list[0].param("pageToken").is_none());
    // 解決したフォルダ ID は覚える（2 回目は名前を引かない）
    e.stub.clear_requests();
    e.index();
    assert!(
        e.stub
            .requests()
            .iter()
            .all(|r| r.param("q").is_none_or(|q| !q.starts_with("name=")))
    );

    // 設定が ID なら、名前の検索をしない
    let e = env_with("drive-idfolder", &[("GEOFRAME_DRIVE_FOLDER", FOLDER_ID)]);
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.index();
    assert!(
        e.stub
            .requests()
            .iter()
            .all(|r| r.param("q").is_none_or(|q| !q.starts_with("name=")))
    );
}

#[test]
fn a_missing_folder_is_an_error_and_a_quote_in_the_name_is_escaped() {
    let e = env_with("drive-nofolder", &[("GEOFRAME_DRIVE_FOLDER", "Nope")]);
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("フォルダ"), "{message}");
    assert!(!message.contains("Nope"), "フォルダ名を出さない: {message}");
    let e = env_with("drive-quote", &[("GEOFRAME_DRIVE_FOLDER", "it's")]);
    let _ = e.run(&e.index_stage());
    let q = e
        .stub
        .requests()
        .iter()
        .find_map(|r| r.param("q").map(str::to_owned))
        .unwrap();
    assert!(q.starts_with("name='it\\'s' and"), "{q}");
}

#[test]
fn the_second_run_changes_nothing() {
    let e = env("drive-idem");
    e.listing(vec![
        located("f1", "a.jpg", "m1", "2024:05:06 07:08:09", 35.0, 139.0),
        jpeg("f2", "b.jpg", "m2"),
    ]);
    e.index();
    let before = e.rows();
    let counts = e.index();
    assert_eq!(
        get(&counts, "added") + get(&counts, "updated") + get(&counts, "removed"),
        0
    );
    assert_eq!(get(&counts, "unchanged"), 2);
    // `indexed_at` も含めて、行が書き換えられていない
    assert_eq!(e.rows(), before);
    // 名前だけが変わっても（md5 が同じなら）何もしない（現行と同じ）
    e.listing(vec![
        located(
            "f1",
            "renamed.jpg",
            "m1",
            "2024:05:06 07:08:09",
            35.0,
            139.0,
        ),
        jpeg("f2", "b.jpg", "m2"),
    ]);
    e.index();
    assert_eq!(e.rows(), before);
}

/// 手動の位置のほかに、お気に入り・回転・表示・地名・推定の痕跡を持つ行を作る（`loc_source` は引数）。
fn seed_located_row(e: &Env, loc_source: &str) {
    e.listing(vec![located(
        "f1",
        "a.jpg",
        "m1",
        "2024:05:06 07:08:09",
        35.0,
        139.0,
    )]);
    e.index();
    e.sql(Box::leak(
        format!(
            "UPDATE photos SET loc_source='{loc_source}', lat=1.0, lon=2.0, favorite=1, hidden=1, rotation=90,
             place='東京', elev=12.5, display='derivatives/f1.r90.jpg', fetched_at='2024-01-01T00:00:00',
             inferred_from=7, infer_dt_s=60, title='題名', muni_cd='13101', pref='東京都', city='千代田区',
             indexed_at='2000-01-01T00:00:00'"
        )
        .into_boxed_str(),
    ));
    fs::create_dir_all(e.root().join("derivatives")).unwrap();
    fs::write(e.root().join("derivatives/f1.r90.jpg"), b"x").unwrap();
}

#[test]
fn a_changed_md5_updates_the_row_and_resets_the_derived_columns() {
    let e = env("drive-update");
    // 手動でない位置（推定）は、現行と同じく原本の値（ここでは位置なし）で上書きされる
    seed_located_row(&e, "inferred");
    e.listing(vec![jpeg("f1", "a2.jpg", "m1-new")]);
    let counts = e.index();
    assert_eq!(get(&counts, "updated"), 1);
    assert_eq!(get(&counts, "location_kept"), 0);
    let rows = e.rows();
    let r = &rows[0];
    assert_eq!(text(r, "name").as_deref(), Some("a2.jpg"));
    assert_eq!(text(r, "md5").as_deref(), Some("m1-new"));
    assert_eq!(col(r, "lat"), &rusqlite::types::Value::Null);
    assert_eq!(col(r, "lon"), &rusqlite::types::Value::Null);
    assert!(text(r, "loc_source").is_none());
    assert_eq!(text(r, "taken_at").as_deref(), Some(""));
    // 戻るもの
    for name in ["place", "elev", "display", "fetched_at"] {
        assert_eq!(col(r, name), &rusqlite::types::Value::Null, "{name}");
    }
    assert_eq!(col(r, "rotation"), &rusqlite::types::Value::Integer(0));
    assert_ne!(
        text(r, "indexed_at").as_deref(),
        Some("2000-01-01T00:00:00")
    );
    // 残るもの
    assert_eq!(col(r, "favorite"), &rusqlite::types::Value::Integer(1));
    assert_eq!(col(r, "hidden"), &rusqlite::types::Value::Integer(1));
    assert_eq!(col(r, "inferred_from"), &rusqlite::types::Value::Integer(7));
    assert_eq!(col(r, "infer_dt_s"), &rusqlite::types::Value::Integer(60));
    assert_eq!(text(r, "title").as_deref(), Some("題名"));
    assert_eq!(text(r, "muni_cd").as_deref(), Some("13101"));
    // 更新では古い derivative のファイルは消さない（現行と同じ。次の取得が上書きする）
    assert!(e.root().join("derivatives/f1.r90.jpg").exists());
    // md5 の NULL 同士は「変わらない」
    e.listing(vec![item("n1", "n.jpg", None, "image/jpeg", None)]);
    e.index();
    let before = e.rows();
    let counts = e.index();
    assert_eq!(get(&counts, "updated") + get(&counts, "added"), 0);
    assert_eq!(e.rows(), before);
}

/// T3.7 ①: 手動の位置（`loc_source='manual'`）は、md5 が変わっても `lat lon loc_source place elev` を書き換えない。
/// 撮影時刻・名前・md5・表示・取得時刻・回転・indexed_at は今までどおり更新される。
#[test]
fn a_changed_md5_keeps_a_manual_location_and_its_place_names() {
    let e = env("drive-update-manual");
    seed_located_row(&e, "manual");
    // 原本は位置つきで、撮影時刻も変わる（手動の位置は原本の位置に置き換わらない）
    e.listing(vec![located(
        "f1",
        "a2.jpg",
        "m1-new",
        "2025:01:02 03:04:05",
        35.5,
        139.5,
    )]);
    let counts = e.index();
    assert_eq!(get(&counts, "updated"), 1);
    assert_eq!(get(&counts, "location_kept"), 1);
    let rows = e.rows();
    let r = &rows[0];
    assert_eq!(col(r, "lat"), &rusqlite::types::Value::Real(1.0));
    assert_eq!(col(r, "lon"), &rusqlite::types::Value::Real(2.0));
    assert_eq!(text(r, "loc_source").as_deref(), Some("manual"));
    assert_eq!(text(r, "place").as_deref(), Some("東京"));
    assert_eq!(col(r, "elev"), &rusqlite::types::Value::Real(12.5));
    assert_eq!(text(r, "muni_cd").as_deref(), Some("13101"));
    assert_eq!(text(r, "pref").as_deref(), Some("東京都"));
    assert_eq!(text(r, "city").as_deref(), Some("千代田区"));
    assert_eq!(col(r, "inferred_from"), &rusqlite::types::Value::Integer(7));
    assert_eq!(col(r, "infer_dt_s"), &rusqlite::types::Value::Integer(60));
    // それ以外の更新は今までどおり
    assert_eq!(text(r, "name").as_deref(), Some("a2.jpg"));
    assert_eq!(text(r, "md5").as_deref(), Some("m1-new"));
    assert_eq!(text(r, "taken_at").as_deref(), Some("2025-01-02 03:04"));
    for name in ["display", "fetched_at"] {
        assert_eq!(col(r, name), &rusqlite::types::Value::Null, "{name}");
    }
    assert_eq!(col(r, "rotation"), &rusqlite::types::Value::Integer(0));
    assert_ne!(
        text(r, "indexed_at").as_deref(),
        Some("2000-01-01T00:00:00")
    );
    assert_eq!(col(r, "favorite"), &rusqlite::types::Value::Integer(1));
    assert_eq!(col(r, "hidden"), &rusqlite::types::Value::Integer(1));
    assert_eq!(text(r, "title").as_deref(), Some("題名"));
}

/// 手動の行と手動でない行が同じ更新に混ざっても、行ごとに規則が決まる。`--dry-run`（写しに同じ書き込みを
/// 当てる）も同じ結果と件数になる。
#[test]
fn dry_run_counts_and_applies_the_same_rule_for_manual_rows() {
    let e = env("drive-update-manual-dry");
    e.listing(vec![
        located("m", "m.jpg", "m1", "2024:05:06 07:08:09", 35.0, 139.0),
        located("n", "n.jpg", "n1", "2024:05:06 07:09:09", 35.1, 139.1),
    ]);
    e.index();
    e.sql("UPDATE photos SET loc_source='manual', lat=1.0, lon=2.0 WHERE drive_id='m'");
    e.listing(vec![
        located("m", "m.jpg", "m2", "2024:05:06 07:08:09", 35.0, 139.0),
        located("n", "n.jpg", "n2", "2024:05:06 07:09:09", 36.0, 140.0),
    ]);
    let before = e.rows();
    let sim = e.read_for(Mode::DryRun);
    let result = e.run_on(&e.index_stage(), Mode::DryRun, &sim, &StopFlag::new());
    let StageResult::Done(counts) = result else {
        panic!("{result:?}");
    };
    assert_eq!(get(&counts, "updated"), 2);
    assert_eq!(get(&counts, "location_kept"), 1);
    // 後続の段に見せる写しにも同じ規則が当たっている
    let sim_lat = |id: &str| -> f64 {
        sim.query_row("SELECT lat FROM photos WHERE drive_id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!((sim_lat("m"), sim_lat("n")), (1.0, 36.0));
    // dry-run は実 DB を変えない
    assert_eq!(e.rows(), before);
    // 実行すると、手動の行だけ位置が残る
    let counts = e.index();
    assert_eq!(
        (get(&counts, "updated"), get(&counts, "location_kept")),
        (2, 1)
    );
    let m = row_of(&e, "m").unwrap();
    assert_eq!(col(&m, "lat"), &rusqlite::types::Value::Real(1.0));
    assert_eq!(text(&m, "loc_source").as_deref(), Some("manual"));
    let n = row_of(&e, "n").unwrap();
    assert_eq!(col(&n, "lat"), &rusqlite::types::Value::Real(36.0));
    assert_eq!(text(&n, "loc_source").as_deref(), Some("drive_meta"));
}

fn api_set_loc(writer: &Writer, id: i64, lat: f64, lon: f64) {
    let payload = json!({"id": id, "lat": lat, "lon": lon});
    crate::update::set_loc(payload.as_object().unwrap(), Some(writer)).unwrap();
}

fn api_set_flag(writer: &Writer, id: i64, favorite: bool, hidden: bool) {
    let payload = json!({"id": id, "favorite": favorite, "hidden": hidden});
    crate::update::set_flag(payload.as_object().unwrap(), Some(writer)).unwrap();
}

/// `drive_id` の行の id。
fn id_of(e: &Env, drive_id: &str) -> i64 {
    let row = e
        .rows()
        .into_iter()
        .find(|r| text(r, "drive_id").as_deref() == Some(drive_id))
        .unwrap_or_else(|| panic!("行が無い: {drive_id}"));
    match col(&row, "id") {
        rusqlite::types::Value::Integer(n) => *n,
        other => panic!("{other:?}"),
    }
}

fn row_of(e: &Env, drive_id: &str) -> Option<Row> {
    e.rows()
        .into_iter()
        .find(|r| text(r, "drive_id").as_deref() == Some(drive_id))
}

fn int(row: &Row, name: &str) -> i64 {
    match col(row, name) {
        rusqlite::types::Value::Integer(n) => *n,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn index_drive_plan_survives_the_update_api_including_a_location_set_after_the_plan() {
    let mut e = env("drive-update-api");
    e.listing(vec![
        located("f1", "a.jpg", "m1", "2024:05:06 07:08:09", 35.0, 139.0), // md5 が変わる（Update）
        located("f2", "b.jpg", "m2", "2024:05:06 07:09:09", 35.1, 139.1), // 変わらない
        located("f3", "c.jpg", "m3", "2024:05:06 07:10:09", 35.2, 139.2), // 消える（Delete）
    ]);
    e.index();
    let (id1, id2, id3) = (id_of(&e, "f1"), id_of(&e, "f2"), id_of(&e, "f3"));
    e.listing(vec![
        located("f1", "a.jpg", "m1-new", "2024:05:06 07:08:09", 35.0, 139.0),
        located("f2", "b.jpg", "m2", "2024:05:06 07:09:09", 35.1, 139.1),
    ]);
    // 一覧を読んで計画を作ったあと、書く直前に更新 API が入る
    let api = e.writer.clone();
    e.writer = api.clone().interrupt_next_call(move || {
        api_set_flag(&api, id1, true, true);
        api_set_loc(&api, id1, 43.0, 141.0);
        api_set_loc(&api, id2, 48.0, 2.0);
        api_set_flag(&api, id2, true, false);
        api_set_flag(&api, id3, true, true);
    });
    let counts = e.index();
    assert_eq!((get(&counts, "updated"), get(&counts, "removed")), (1, 1));

    // 変わらない行は、手動の位置もお気に入りも残る
    let r2 = row_of(&e, "f2").unwrap();
    assert_eq!(text(&r2, "loc_source").as_deref(), Some("manual"));
    assert_eq!(col(&r2, "lat"), &rusqlite::types::Value::Real(48.0));
    assert_eq!((int(&r2, "favorite"), int(&r2, "hidden")), (1, 0));
    // md5 が変わった行: お気に入り・除外は残る。位置は、計画のあとに更新 API が手動で直したものが残る
    // （T3.7 ①。書く時点で `manual` なら位置を書かない。計画の時点では手動でなかったので `location_kept` には数えない）
    let r1 = row_of(&e, "f1").unwrap();
    assert_eq!((int(&r1, "favorite"), int(&r1, "hidden")), (1, 1));
    assert_eq!(text(&r1, "loc_source").as_deref(), Some("manual"));
    assert_eq!(col(&r1, "lat"), &rusqlite::types::Value::Real(43.0));
    assert_eq!(col(&r1, "lon"), &rusqlite::types::Value::Real(141.0));
    assert_eq!(get(&counts, "location_kept"), 0);
    assert_eq!(text(&r1, "md5").as_deref(), Some("m1-new"));
    // 一覧から消えた行は、お気に入りごと消える（§2.3 ②。T3.7）
    assert!(row_of(&e, "f3").is_none());
}

#[test]
fn fetch_keeps_what_the_update_api_wrote_while_an_original_is_downloading() {
    let mut e = env("drive-fetch-update-api");
    e.listing(vec![
        located("j1", "a.jpg", "m1", "2024:05:01 10:00:00", 35.0, 139.0),
        located("j2", "b.jpg", "m2", "2024:05:02 10:00:00", 35.1, 139.1),
    ]);
    e.stub.set_media("j1", fixture_jpeg("typical"));
    e.stub.set_media("j2", fixture_jpeg("typical"));
    e.index();
    let (id1, id2) = (id_of(&e, "j1"), id_of(&e, "j2"));
    // 取得の段が最初の書き込み（j1 の display）を積む直前に、更新 API が j1・j2 を直す
    let api = e.writer.clone();
    e.writer = api.clone().interrupt_next_call(move || {
        api_set_loc(&api, id1, 43.0, 141.0);
        api_set_flag(&api, id1, true, false);
        api_set_loc(&api, id2, 48.0, 2.0);
        api_set_flag(&api, id2, false, true);
    });
    let counts = e.done(&e.fetch_stage());
    assert_eq!(get(&counts, "fetched"), 2);
    for (drive_id, lat, flags) in [("j1", 43.0, (1, 0)), ("j2", 48.0, (0, 1))] {
        let r = row_of(&e, drive_id).unwrap();
        assert_eq!(
            text(&r, "loc_source").as_deref(),
            Some("manual"),
            "{drive_id}"
        );
        assert_eq!(
            col(&r, "lat"),
            &rusqlite::types::Value::Real(lat),
            "{drive_id}"
        );
        assert_eq!(
            (int(&r, "favorite"), int(&r, "hidden")),
            flags,
            "{drive_id}"
        );
        assert!(text(&r, "display").is_some(), "{drive_id}");
    }
}

#[test]
fn vanished_files_are_deleted_with_their_derivatives_but_only_safe_names() {
    let e = env("drive-delete");
    e.listing(vec![
        jpeg("f1", "a.jpg", "m1"),
        jpeg("f2", "b.jpg", "m2"),
        jpeg("f3", "c.jpg", "m3"),
    ]);
    e.index();
    fs::create_dir_all(e.root().join("derivatives")).unwrap();
    fs::write(e.root().join("derivatives/f1.jpg"), b"x").unwrap();
    fs::write(e.root().join("keep.txt"), b"y").unwrap();
    e.sql("UPDATE photos SET display='derivatives/f1.jpg' WHERE drive_id='f1'");
    // 危ない display（DATA_DIR の中の別の場所を指す）は、行は消すがファイルは消さない
    e.sql("UPDATE photos SET display='derivatives/../keep.txt' WHERE drive_id='f2'");
    // ローカルの行は触らない
    e.sql("INSERT INTO photos(source, path, name) VALUES('local','photos/x.jpg','x.jpg')");
    e.listing(vec![jpeg("f3", "c.jpg", "m3")]);
    let counts = e.index();
    assert_eq!(get(&counts, "removed"), 2);
    assert_eq!(get(&counts, "derivative_files_removed"), 1);
    assert_eq!(get(&counts, "derivative_files_unsafe"), 1);
    assert!(!e.root().join("derivatives/f1.jpg").exists());
    assert!(e.root().join("keep.txt").exists());
    let ids: Vec<_> = e.rows().iter().map(|r| text(r, "drive_id")).collect();
    assert_eq!(ids, [Some("f3".to_string()), None]);
}

#[test]
fn a_failed_listing_deletes_and_adds_nothing() {
    let e = env("drive-partial");
    e.listing(vec![
        jpeg("f1", "a.jpg", "m1"),
        jpeg("f2", "b.jpg", "m2"),
        jpeg("f3", "c.jpg", "m3"),
    ]);
    e.index();
    let before = e.rows();
    // 1 ページ目は取れて、2 ページ目が 5xx で終わらない。f1 が消え、f9 が増える一覧だったとしても
    e.listing(vec![
        jpeg("f9", "z.jpg", "m9"),
        jpeg("f8", "y.jpg", "m8"),
        jpeg("f3", "c.jpg", "m3"),
    ]);
    e.stub.add_fault(Box::new(|r| {
        (r.param("pageToken") == Some("page-2")).then(|| (500, "{}".to_string(), vec![]))
    }));
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("500"), "{message}");
    assert_eq!(e.rows(), before);
    // dry-run でも同じ（失敗する）
    assert!(matches!(
        e.run_with(&e.index_stage(), Mode::DryRun, &StopFlag::new()),
        StageResult::Failed(_)
    ));
    // 直れば、そのまま取り込める
    e.stub.clear_faults();
    let counts = e.index();
    assert_eq!((get(&counts, "added"), get(&counts, "removed")), (2, 2));
}

#[test]
fn a_malformed_page_is_a_failure_and_never_a_partial_listing() {
    for page in [
        json!({"files": "nope"}),
        json!({"files": [{"mimeType": "image/jpeg", "name": "x.jpg"}]}), // id が無い
        json!({"files": [], "nextPageToken": 5}),
    ] {
        let e = env("drive-badpage");
        e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
        e.index();
        let before = e.rows();
        let body = page.to_string();
        e.stub.add_fault(Box::new(move |r| {
            r.param("fields")
                .is_some_and(|f| f.starts_with("nextPageToken"))
                .then(|| (200, body.clone(), vec![]))
        }));
        assert!(
            matches!(e.run(&e.index_stage()), StageResult::Failed(_)),
            "{page}"
        );
        assert_eq!(e.rows(), before, "{page}");
    }
    // 同じページ送りのトークンが続く（止まらない）
    let e = env("drive-loop");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.stub.add_fault(Box::new(|r| {
        r.param("fields")
            .is_some_and(|f| f.starts_with("nextPageToken"))
            .then(|| {
                (
                    200,
                    r#"{"files":[],"nextPageToken":"same"}"#.to_string(),
                    vec![],
                )
            })
    }));
    assert!(matches!(e.run(&e.index_stage()), StageResult::Failed(_)));
    assert!(e.rows().is_empty());
}

#[test]
fn rate_limits_are_retried_with_the_server_pace_and_permission_errors_are_not() {
    let e = env("drive-429");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    let mut left = 2;
    e.stub.add_fault(Box::new(move |r| {
        (r.param("fields")
            .is_some_and(|f| f.starts_with("nextPageToken"))
            && left > 0)
            .then(|| {
                left -= 1;
                (
                    429,
                    "{}".to_string(),
                    vec![("Retry-After".to_string(), "0".to_string())],
                )
            })
    }));
    assert_eq!(get(&e.index(), "added"), 1);
    assert_eq!(e.stub.list_requests().len(), 3);

    // 流量制限の 403 は再試行する。権限の 403 は再試行しない
    let e = env("drive-403");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    let mut left = 1;
    e.stub.add_fault(Box::new(move |r| {
        (r.param("fields")
            .is_some_and(|f| f.starts_with("nextPageToken"))
            && left > 0)
            .then(|| {
                left -= 1;
                (
                    403,
                    r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#.to_string(),
                    vec![],
                )
            })
    }));
    assert_eq!(get(&e.index(), "added"), 1);
    let e = env("drive-403b");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.stub.add_fault(Box::new(|r| {
        r.param("fields")
            .is_some_and(|f| f.starts_with("nextPageToken"))
            .then(|| {
                (
                    403,
                    r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#.to_string(),
                    vec![],
                )
            })
    }));
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("403"));
    assert_eq!(e.stub.list_requests().len(), 1);

    // 429 が続いて回数を使い切ったら失敗（429 を含む文面）
    let e = env("drive-429b");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.stub.add_fault(Box::new(|r| {
        r.param("fields")
            .is_some_and(|f| f.starts_with("nextPageToken"))
            .then(|| (429, "{}".to_string(), vec![]))
    }));
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("429"));
    assert_eq!(e.stub.list_requests().len(), 4);
}

#[test]
fn an_empty_listing_never_deletes_the_rows_even_though_python_would() {
    let e = env("drive-empty");
    e.listing(vec![jpeg("f1", "a.jpg", "m1"), jpeg("f2", "b.jpg", "m2")]);
    e.index();
    e.sql("UPDATE photos SET favorite=1");
    let before = e.rows();
    e.listing(vec![]);
    let counts = e.index();
    assert_eq!(get(&counts, "removed"), 0);
    assert_eq!(get(&counts, "deletes_withheld"), 2);
    assert_eq!(e.rows(), before);
    // フォルダに画像以外しかないのも空の一覧
    e.listing(vec![item(
        "d1",
        "sub",
        None,
        "application/vnd.google-apps.folder",
        None,
    )]);
    assert_eq!(get(&e.index(), "deletes_withheld"), 2);
    // 1 件でも返れば、通常どおり消える
    e.listing(vec![jpeg("f2", "b.jpg", "m2")]);
    let counts = e.index();
    assert_eq!(
        (get(&counts, "removed"), get(&counts, "deletes_withheld")),
        (1, 0)
    );
}

#[test]
fn a_duplicated_id_keeps_the_first_position_and_the_last_value() {
    let e = env("drive-dup");
    e.listing(vec![
        jpeg("f1", "first.jpg", "m1"),
        jpeg("f2", "b.jpg", "m2"),
        jpeg("f1", "second.jpg", "m1b"),
    ]);
    let counts = e.index();
    assert_eq!(get(&counts, "remote"), 2);
    let rows = e.rows();
    assert_eq!(text(&rows[0], "drive_id").as_deref(), Some("f1"));
    assert_eq!(text(&rows[0], "name").as_deref(), Some("second.jpg"));
    assert_eq!(text(&rows[0], "md5").as_deref(), Some("m1b"));
}

#[test]
fn a_file_without_a_name_is_not_indexed_but_its_row_is_kept() {
    let e = env("drive-noname");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.index();
    e.listing(vec![
        json!({"id": "f1", "mimeType": "image/jpeg", "md5Checksum": "m1b"}),
        jpeg("f2", "b.jpg", "m2"),
    ]);
    let counts = e.index();
    assert_eq!(get(&counts, "malformed"), 1);
    assert_eq!(get(&counts, "removed"), 0);
    let rows = e.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(text(&rows[0], "md5").as_deref(), Some("m1")); // 更新されない
}

#[test]
fn rows_without_a_drive_id_and_foreign_rows_follow_the_python_rules() {
    let e = env("drive-orphan");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    e.index();
    // `source='drive'` で drive_id が NULL の行は、現行の `not in remote` で消える
    e.sql("INSERT INTO photos(source, name) VALUES('drive','orphan.jpg')");
    e.sql("INSERT INTO photos(source, path, name) VALUES('local','photos/x.jpg','x.jpg')");
    let counts = e.index();
    assert_eq!(get(&counts, "removed"), 1);
    assert_eq!(e.rows().len(), 2);
}

#[test]
fn the_stop_flag_stops_before_anything_is_written() {
    let e = env("drive-stop");
    e.listing(vec![jpeg("f1", "a.jpg", "m1")]);
    let stop = StopFlag::new();
    stop.stop();
    let result = e.run_with(&e.index_stage(), Mode::Apply, &stop);
    assert!(matches!(result, StageResult::Skipped(_)), "{result:?}");
    assert!(e.rows().is_empty());
    // 書き込みの直前に来た合図も、行を書かずに終わる
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let read = e.read_for(Mode::Apply);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::Apply,
        config: &e.config,
        read: &read,
        writer: Some(&e.writer),
        cache: &e.cache,
        http: &http,
        stop: &stop,
    };
    let result = e
        .index_stage()
        .run_with(&ctx, move || counter.fetch_add(1, Ordering::SeqCst) >= 1)
        .unwrap();
    assert!(matches!(result, StageOutcome::Skipped(_)));
    assert!(e.rows().is_empty());
}

#[test]
fn dry_run_reads_the_listing_but_writes_nothing_and_shows_the_after_state_to_later_stages() {
    let e = env("drive-dry");
    e.listing(vec![
        located("f1", "a.jpg", "m1", "2024:05:06 07:08:09", 35.0, 139.0),
        jpeg("f2", "b.jpg", "m2"),
    ]);
    e.index();
    e.listing(vec![
        jpeg("f2", "b.jpg", "m2-new"),
        jpeg("f3", "c.jpg", "m3"),
    ]);
    let rows = e.rows();
    let dir_before = dir_names(&e);
    // 1 つの写しを両方の段が共有する（cli.rs の `sync --dry-run` と同じ）
    let sim = e.read_for(Mode::DryRun);
    let stop = StopFlag::new();
    let StageResult::Done(counts) = e.run_on(&e.index_stage(), Mode::DryRun, &sim, &stop) else {
        panic!("完了のはず");
    };
    assert_eq!(
        (
            get(&counts, "added"),
            get(&counts, "updated"),
            get(&counts, "removed")
        ),
        (1, 1, 1)
    );
    let media_before = e.stub.media_requests().len();
    let StageResult::Done(fetch) = e.run_on(&e.fetch_stage(), Mode::DryRun, &sim, &stop) else {
        panic!("完了のはず");
    };
    // 写しの上では f2（更新）と f3（追加）が取得待ち。ダウンロードはしない
    assert_eq!((get(&fetch, "pending"), get(&fetch, "planned")), (2, 2));
    assert_eq!(e.stub.media_requests().len(), media_before);
    // 実 DB は変わらない
    assert_eq!(e.rows(), rows);
    assert_eq!(dir_names(&e), dir_before);
    assert!(e.tmp_leftovers().is_empty());
    // 実行と同じ件数
    let applied = e.index();
    assert_eq!(
        (
            get(&applied, "added"),
            get(&applied, "updated"),
            get(&applied, "removed")
        ),
        (1, 1, 1)
    );
}

// ---------------------------------------------------------------------------
// 取得

/// データ置き場の直下の名前（DB と WAL を除く）。
fn dir_names(e: &Env) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(e.root())
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with("catalog.db"))
        .collect();
    names.sort();
    names
}

fn derivative_files(e: &Env) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(e.root().join("derivatives"))
        .map(|rd| {
            rd.map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn fetch_makes_derivatives_for_every_format_and_leaves_no_original() {
    let e = env("drive-fetch");
    e.listing(vec![
        located("j1", "a.jpg", "m1", "2024:05:01 10:00:00", 35.0, 139.0),
        item(
            "p1",
            "b.png",
            Some("m2"),
            "image/png",
            Some(json!({"time": "2024:05:02 10:00:00"})),
        ),
        item(
            "t1",
            "c.tif",
            Some("m3"),
            "image/tiff",
            Some(json!({"time": "2024:05:03 10:00:00"})),
        ),
        item(
            "h1",
            "d.heic",
            Some("m4"),
            "image/heic",
            Some(json!({"time": "2024:05:04 10:00:00"})),
        ),
    ]);
    e.stub.set_media("j1", fixture_jpeg("typical"));
    e.stub.set_media("p1", png());
    e.stub.set_media("t1", tiff());
    e.stub.set_media("h1", heic());
    e.index();
    let counts = e.done(&e.fetch_stage());
    assert_eq!(get(&counts, "pending"), 4);
    assert_eq!(get(&counts, "attempted"), 4);
    assert_eq!(get(&counts, "fetched"), 4);
    for kind in ["ok_jpeg", "ok_png", "ok_tiff", "ok_heic"] {
        assert_eq!(get(&counts, kind), 1, "{kind}");
    }
    assert_eq!(
        derivative_files(&e),
        ["h1.jpg", "j1.jpg", "p1.jpg", "t1.jpg"]
    );
    assert!(e.tmp_leftovers().is_empty(), "{:?}", e.tmp_leftovers());
    for row in e.rows() {
        let id = text(&row, "drive_id").unwrap();
        assert_eq!(text(&row, "display"), Some(format!("derivatives/{id}.jpg")));
        let fetched = text(&row, "fetched_at").unwrap();
        assert_eq!(fetched.len(), 19);
        assert_eq!(&fetched[10..11], "T");
    }
    // 取得したものは次のサイクルで対象にならない（ダウンロードもしない）
    let media = e.stub.media_requests().len();
    let counts = e.done(&e.fetch_stage());
    assert_eq!((get(&counts, "pending"), get(&counts, "attempted")), (0, 0));
    assert_eq!(e.stub.media_requests().len(), media);
    // 出力にはEXIF（GPS・機種名）が残らない
    for name in derivative_files(&e) {
        let bytes = fs::read(e.root().join("derivatives").join(name)).unwrap();
        assert!(!bytes.windows(4).any(|w| w == b"Exif"));
    }
    // 取得の順は `taken_at` の昇順（位置の有無で絞らない）
    let order: Vec<_> = e
        .stub
        .media_requests()
        .iter()
        .map(|r| r.path.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(order, ["j1", "p1", "t1", "h1"]);
}

#[test]
fn originals_are_downloaded_into_tmp_and_removed_even_when_conversion_fails() {
    let e = env("drive-failclean");
    e.listing(vec![jpeg("b1", "a.jpg", "m1"), jpeg("b2", "b.jpg", "m2")]);
    e.stub.set_media("b1", broken());
    e.stub
        .set_media("b2", fixture_jpeg("typical")[..60].to_vec()); // 途中で切れた JPEG
    e.index();
    let counts = e.done(&e.fetch_stage());
    assert_eq!(get(&counts, "fetched"), 0);
    assert_eq!(get(&counts, "fail_not_image"), 1);
    assert_eq!(
        get(&counts, "fail_decode_jpeg") + get(&counts, "fail_not_image"),
        2
    );
    assert!(e.tmp_leftovers().is_empty());
    assert!(derivative_files(&e).is_empty());
    for row in e.rows() {
        assert!(text(&row, "display").is_none());
    }
}

#[test]
fn the_original_guard_removes_the_file_on_success_error_and_panic() {
    let dir = TempDir::new("drive-guard");
    let make = |name: &str| {
        let path = dir.path().join(name);
        let (mut file, guard) = fetch::create_original(&path).unwrap();
        std::io::Write::write_all(&mut file, b"secret").unwrap();
        assert!(path.exists());
        (path, guard)
    };
    let (path, guard) = make("ok");
    drop(guard);
    assert!(!path.exists());
    let path = {
        let result = std::panic::catch_unwind(|| {
            let (path, _guard) = make("panic");
            std::panic::panic_any(path);
        });
        *result.unwrap_err().downcast::<PathBuf>().unwrap()
    };
    assert!(!path.exists());
    // 権限は 0600
    let (path, guard) = make("mode");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(guard);
}

#[test]
fn a_planted_link_in_tmp_is_not_followed_not_removed_and_blocks_only_that_photo() {
    let e = env("drive-link");
    e.listing(vec![jpeg("l1", "a.jpg", "m1"), jpeg("l2", "b.jpg", "m2")]);
    e.stub.set_media("l1", fixture_jpeg("typical"));
    e.stub.set_media("l2", fixture_jpeg("typical"));
    e.index();
    let outside = TempDir::new("drive-link-target");
    let target = outside.path().join("victim");
    fs::write(&target, b"important").unwrap();
    fs::create_dir_all(e.root().join("tmp")).unwrap();
    symlink(&target, e.root().join("tmp/.dl_l1")).unwrap();
    let counts = e.done(&e.fetch_stage());
    assert_eq!(get(&counts, "fail_io"), 1);
    assert_eq!(get(&counts, "fetched"), 1);
    assert_eq!(fs::read(&target).unwrap(), b"important");
    // リンク自体も消されない（起動時の掃除の仕事）
    assert!(
        fs::symlink_metadata(e.root().join("tmp/.dl_l1"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // 通信もしていない（リンクの手前で止まる）
    let fetched: Vec<_> = e
        .stub
        .media_requests()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(fetched, ["/drive/v3/files/l2"]);
    // 切れたリンクでも同じ
    fs::remove_file(e.root().join("tmp/.dl_l1")).unwrap();
    symlink(outside.path().join("nothing"), e.root().join("tmp/.dl_l1")).unwrap();
    e.sql("UPDATE photos SET display=NULL WHERE drive_id='l1'");
    e.done(&e.fetch_stage());
    assert!(!outside.path().join("nothing").exists());
}

#[test]
fn photos_that_keep_failing_do_not_block_the_ones_behind_them() {
    // 受入条件②。1 サイクル 2 枚。先頭の 3 枚（枠より多い）は恒久的に失敗する
    let e = env_with("drive-block", &[("GEOFRAME_FETCH_PER_CYCLE", "2")]);
    let mut items = Vec::new();
    for (i, id) in ["x1", "x2", "x3"].iter().enumerate() {
        items.push(located(
            id,
            &format!("{id}.jpg"),
            id,
            &format!("2020:01:0{} 10:00:00", i + 1),
            35.0,
            139.0,
        ));
        e.stub.set_media(id, broken());
    }
    for (i, id) in ["g1", "g2", "g3"].iter().enumerate() {
        items.push(located(
            id,
            &format!("{id}.jpg"),
            id,
            &format!("2024:01:0{} 10:00:00", i + 1),
            35.0,
            139.0,
        ));
        e.stub.set_media(id, fixture_jpeg("typical"));
    }
    e.listing(items);
    e.index();
    let fetch = e.fetch_stage();
    let displayed = || {
        let mut ids: Vec<String> = e
            .rows()
            .iter()
            .filter(|r| text(r, "display").is_some())
            .map(|r| text(r, "drive_id").unwrap())
            .collect();
        ids.sort();
        ids
    };

    // 1 回目: 先頭の 2 枚（x1・x2）を試して失敗
    let counts = e.done(&fetch);
    assert_eq!(
        (
            get(&counts, "attempted"),
            get(&counts, "fetched"),
            get(&counts, "fail_not_image")
        ),
        (2, 0, 2)
    );
    assert!(displayed().is_empty());
    // 2 回目: x1・x2 は飛ばし、x3 を試して失敗、良い写真 g1 を 1 枚取得
    let counts = e.done(&fetch);
    assert_eq!(get(&counts, "skipped_backoff"), 2);
    assert_eq!((get(&counts, "attempted"), get(&counts, "fetched")), (2, 1));
    assert_eq!(displayed(), ["g1"]);
    // 3 回目: 失敗中の 3 枚を飛ばして、残りの 2 枚
    let counts = e.done(&fetch);
    assert_eq!(get(&counts, "skipped_backoff"), 3);
    assert_eq!((get(&counts, "attempted"), get(&counts, "fetched")), (2, 2));
    assert_eq!(displayed(), ["g1", "g2", "g3"]);
    // 4 回目: 失敗中の 3 枚だけが残り、何も試さない
    let counts = e.done(&fetch);
    assert_eq!((get(&counts, "attempted"), get(&counts, "pending")), (0, 3));
    assert_eq!(get(&counts, "skipped_backoff"), 3);

    // 同じ状況を「記録なし」（現行の LIMIT のSQLと同じ）で繰り返すと、良い写真は永遠に取れない
    let e2 = env_with("drive-block2", &[("GEOFRAME_FETCH_PER_CYCLE", "2")]);
    let mut items = Vec::new();
    for (i, id) in ["x1", "x2", "x3", "g1"].iter().enumerate() {
        items.push(located(
            id,
            &format!("{id}.jpg"),
            id,
            &format!("2020:01:0{} 10:00:00", i + 1),
            35.0,
            139.0,
        ));
        e2.stub.set_media(
            id,
            if id.starts_with('x') {
                broken()
            } else {
                fixture_jpeg("typical")
            },
        );
    }
    e2.listing(items);
    e2.index();
    // 待ち時間 0 の記録 = 毎回すべてを試す
    let no_memory = FetchDerivatives::with_memo(
        Arc::clone(&e2.shared),
        FailureMemo::with_policy(Duration::ZERO, Duration::ZERO, 16),
    );
    for _ in 0..3 {
        let counts = e2.done(&no_memory);
        assert_eq!(get(&counts, "fetched"), 0);
    }
}

#[test]
fn a_failed_photo_is_tried_again_when_its_original_changes_or_the_wait_is_over() {
    let e = env("drive-retry");
    e.listing(vec![jpeg("r1", "a.jpg", "m1")]);
    e.stub.set_media("r1", broken());
    e.index();
    let fetch = e.fetch_stage();
    assert_eq!(get(&e.done(&fetch), "fail_not_image"), 1);
    assert_eq!(get(&e.done(&fetch), "skipped_backoff"), 1);
    // Drive で原本が差し替わった（md5 が変わる）→ すぐ試す
    e.stub.set_media("r1", fixture_jpeg("typical"));
    e.listing(vec![jpeg("r1", "a.jpg", "m1-new")]);
    e.index();
    let counts = e.done(&fetch);
    assert_eq!(
        (get(&counts, "fetched"), get(&counts, "skipped_backoff")),
        (1, 0)
    );

    // 待ち時間が過ぎれば、同じ原本でも試す（待ち時間 0 の方針）
    let e = env("drive-retry2");
    e.listing(vec![jpeg("r1", "a.jpg", "m1")]);
    e.stub.set_media("r1", broken());
    e.index();
    let fetch = FetchDerivatives::with_memo(
        Arc::clone(&e.shared),
        FailureMemo::with_policy(Duration::ZERO, Duration::ZERO, 16),
    );
    assert_eq!(get(&e.done(&fetch), "fail_not_image"), 1);
    e.stub.set_media("r1", fixture_jpeg("typical"));
    assert_eq!(get(&e.done(&fetch), "fetched"), 1);
}

#[test]
fn an_outage_stops_the_stage_without_blaming_the_photos() {
    let e = env("drive-fetchoutage");
    e.listing(vec![jpeg("o1", "a.jpg", "m1"), jpeg("o2", "b.jpg", "m2")]);
    e.stub.set_media("o1", fixture_jpeg("typical"));
    e.stub.set_media("o2", fixture_jpeg("typical"));
    e.index();
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| (503, "{}".to_string(), vec![]))
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    assert!(
        message.contains("503") && message.contains("attempted=1"),
        "{message}"
    );
    // 最初の 1 枚で止まった（2 枚目は試していない）。写真の失敗として覚えない
    assert_eq!(e.stub.media_requests().len(), 4);
    assert!(e.tmp_leftovers().is_empty());
    e.stub.clear_faults();
    let counts = e.done(&fetch);
    assert_eq!(
        (get(&counts, "fetched"), get(&counts, "skipped_backoff")),
        (2, 0)
    );
}

#[test]
fn a_missing_file_is_that_photos_failure_and_the_rest_continue() {
    let e = env("drive-404");
    e.listing(vec![
        located("m1", "a.jpg", "m1", "2024:01:01 10:00:00", 1.0, 2.0),
        located("m2", "b.jpg", "m2", "2024:01:02 10:00:00", 1.0, 2.0),
    ]);
    e.stub.set_media("m2", fixture_jpeg("typical")); // m1 は Drive から消えた（404）
    e.index();
    let counts = e.done(&e.fetch_stage());
    assert_eq!(
        (get(&counts, "fail_download"), get(&counts, "fetched")),
        (1, 1)
    );
}

#[test]
fn rotation_and_unsafe_ids_are_handled() {
    let e = env("drive-rot");
    e.listing(vec![jpeg("q1", "a.jpg", "m1"), jpeg("q2", "b.jpg", "m2")]);
    e.stub.set_media("q1", fixture_jpeg("typical"));
    e.stub.set_media("q2", fixture_jpeg("typical"));
    e.index();
    e.sql("UPDATE photos SET rotation=90 WHERE drive_id='q1'");
    // 区切りを含む ID（DB が壊れている・書き換えられた）は、通信もファイルの作成もしない
    e.sql("UPDATE photos SET drive_id='../escape' WHERE drive_id='q2'");
    let counts = e.done(&e.fetch_stage());
    assert_eq!(
        (get(&counts, "fetched"), get(&counts, "fail_other")),
        (1, 1)
    );
    let rows = e.rows();
    assert_eq!(
        text(&rows[0], "display").as_deref(),
        Some("derivatives/q1.r90.jpg")
    );
    assert!(text(&rows[1], "display").is_none());
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(!e.root().join("escape").exists());
    assert_eq!(derivative_files(&e), ["q1.r90.jpg"]);
}

#[test]
fn the_per_cycle_limit_and_a_zero_limit_are_respected() {
    let e = env_with("drive-limit", &[("GEOFRAME_FETCH_PER_CYCLE", "2")]);
    let items: Vec<Value> = (1..=5)
        .map(|i| {
            located(
                &format!("z{i}"),
                "n.jpg",
                &format!("m{i}"),
                &format!("2024:01:0{i} 10:00:00"),
                1.0,
                2.0,
            )
        })
        .collect();
    for i in 1..=5 {
        e.stub.set_media(&format!("z{i}"), fixture_jpeg("typical"));
    }
    e.listing(items);
    e.index();
    let fetch = e.fetch_stage();
    assert_eq!(get(&e.done(&fetch), "fetched"), 2);
    assert_eq!(get(&e.done(&fetch), "fetched"), 2);
    assert_eq!(get(&e.done(&fetch), "fetched"), 1);
    assert_eq!(get(&e.done(&fetch), "pending"), 0);

    let e = env_with("drive-limit0", &[("GEOFRAME_FETCH_PER_CYCLE", "0")]);
    e.listing(vec![jpeg("z1", "a.jpg", "m1")]);
    e.index();
    let counts = e.done(&e.fetch_stage());
    assert_eq!((get(&counts, "pending"), get(&counts, "attempted")), (1, 0));
    assert!(e.stub.media_requests().is_empty());
}

#[test]
fn download_respects_the_size_limit_before_and_while_reading() {
    let e = env("drive-big");
    e.stub.set_media("big", vec![7u8; 10_000]);
    e.listing(vec![]);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let session = Session::new(&e.shared, &http, &stop, &e.key);
    let mut out = Vec::new();
    assert_eq!(session.download("big", &mut out, 10_000).unwrap(), 10_000);
    assert_eq!(out.len(), 10_000);
    let mut out = Vec::new();
    assert_eq!(
        session.download("big", &mut out, 9_999),
        Err(DriveError::TooLarge)
    );
    assert!(out.is_empty(), "上限を超える本文は書かない");
    // Content-Length が無い応答（チャンク）でも、読みながら上限で止まる
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                200,
                "x".repeat(5000),
                vec![("X-No-Length".to_string(), "1".to_string())],
            )
        })
    }));
    let mut out = Vec::new();
    assert_eq!(
        session.download("big", &mut out, 100),
        Err(DriveError::TooLarge)
    );
    assert!(out.len() <= 100);
}

#[test]
fn log_lines_never_contain_names_keys_tokens_or_urls() {
    // 段の結果の文面（Counts・StageError）に、スタブが返した名前・鍵・トークンが入らないこと
    let e = env("drive-nolog");
    e.listing(vec![located(
        "secretid0001",
        "Private-Holiday-Name.jpg",
        "m1",
        "2024:05:06 07:08:09",
        35.123456,
        139.654321,
    )]);
    e.stub.set_media("secretid0001", broken());
    let mut seen = Vec::new();
    seen.push(format!("{:?}", e.run(&e.index_stage())));
    seen.push(format!("{:?}", e.run(&e.fetch_stage())));
    e.stub.add_fault(Box::new(|r| {
        r.path
            .ends_with("/files")
            .then(|| (500, "{}".to_string(), vec![]))
    }));
    seen.push(format!("{:?}", e.run(&e.index_stage())));
    for text in seen {
        for needle in [
            "Private-Holiday",
            "35.12",
            "139.65",
            "stub-token",
            "PRIVATE",
            "http",
            "secretid0001",
            FOLDER_NAME,
        ] {
            assert!(!text.contains(needle), "{needle} in {text}");
        }
    }
}

// ---------------------------------------------------------------------------
// Python との突き合わせ（tests/fixtures/drive-index-python.json）

const SENTINEL: &str = "2000-01-01T00:00:00";
const KEPT_FETCHED_AT: &str = "2024-01-01T00:00:00";

/// `Row` を、フィクスチャと同じ形（`indexed_at`・`fetched_at` は値を捨てる）の JSON にする。
fn normalized(rows: &[Row]) -> Value {
    use rusqlite::types::Value as Cell;
    Value::Array(
        rows.iter()
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (name, cell) in row {
                    let value = match cell {
                        Cell::Null => Value::Null,
                        Cell::Integer(i) => json!(i),
                        Cell::Real(f) => json!(f),
                        Cell::Text(s) => json!(s),
                        Cell::Blob(_) => panic!("{name}: blob"),
                    };
                    match name.as_str() {
                        "indexed_at" => {
                            map.insert("touched".into(), json!(value != json!(SENTINEL)));
                        }
                        "fetched_at" => {
                            map.insert(
                                "fetched".into(),
                                json!(!value.is_null() && value != json!(KEPT_FETCHED_AT)),
                            );
                            map.insert(
                                "fetched_kept".into(),
                                json!(value == json!(KEPT_FETCHED_AT)),
                            );
                        }
                        _ => {
                            map.insert(name.clone(), value);
                        }
                    }
                }
                Value::Object(map)
            })
            .collect(),
    )
}

/// JPEG の寸法（SOF から）。
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        let len = usize::from(bytes[i + 2]) << 8 | usize::from(bytes[i + 3]);
        if matches!(marker, 0xC0..=0xC3) {
            let h = u32::from(bytes[i + 5]) << 8 | u32::from(bytes[i + 6]);
            let w = u32::from(bytes[i + 7]) << 8 | u32::from(bytes[i + 8]);
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

fn derivative_sizes(e: &Env) -> Value {
    let mut map = serde_json::Map::new();
    let mut names = derivative_files(e);
    names.sort();
    for name in names {
        let bytes = fs::read(e.root().join("derivatives").join(&name)).unwrap();
        if bytes.len() > 20 && name.ends_with(".jpg") {
            let (w, h) = jpeg_size(&bytes).unwrap();
            map.insert(name, json!([w, h]));
        }
    }
    Value::Object(map)
}

fn listing_requests(e: &Env) -> Vec<Value> {
    e.stub
        .requests()
        .iter()
        .filter(|r| r.path == "/drive/v3/files" && !r.is_media())
        .map(|r| {
            json!({
                "q": r.param("q"),
                "fields": r.param("fields"),
                "pageSize": r.param("pageSize").map(|s| s.parse::<i64>().unwrap()),
                "pageToken": r.param("pageToken"),
            })
        })
        .collect()
}

fn media_ids(e: &Env, from: usize) -> Vec<String> {
    e.stub.media_requests()[from..]
        .iter()
        .map(|r| r.path.rsplit('/').next().unwrap().to_string())
        .collect()
}

impl Env {
    fn set_fetch_limit(&mut self, limit: u32) {
        let mut config = (*self.config).clone();
        config.fetch_per_cycle = limit;
        self.config = Arc::new(config);
    }
}

/// T3.7 ①（ユーザーの決定。server.md §0）による、Python との**唯一の**差の分類。フィクスチャの `idA` は
/// `changes` の段階で手動の位置（`1.0, 2.0, 'manual', place='p', elev=3.5`）を持ったまま md5 が変わる。
/// Python はその位置を原本の値（位置なし）で上書きして `lat lon loc_source place elev` が NULL になるが、
/// Rust は手動の値を残す。この 5 列以外（`inferred_from` `infer_dt_s` `muni_cd` `pref` `city` `favorite` `hidden`
/// `title` や、`name md5 taken_at display fetched_at rotation indexed_at`）は Python と一致のまま比べる。
/// 差であること（Python の値が Rust の値と違うこと）も確かめ、分類が空振りしていないようにする。
fn python_rows_with_the_manual_location_decision(py_rows: &Value) -> Value {
    let mut rows = py_rows.clone();
    let mut classified = 0;
    for row in rows.as_array_mut().unwrap() {
        if row["drive_id"] != "idA" {
            continue;
        }
        for (column, kept) in [
            ("lat", json!(1.0)),
            ("lon", json!(2.0)),
            ("loc_source", json!("manual")),
            ("place", json!("p")),
            ("elev", json!(3.5)),
        ] {
            assert_ne!(
                row[column], kept,
                "{column}: Python と同じ値なら決定による差ではない"
            );
            row[column] = kept;
        }
        classified += 1;
    }
    assert_eq!(classified, 1, "idA の行が無い");
    rows
}

#[test]
fn matches_the_python_fixture() {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/drive-index-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["folder"]["name"], FOLDER_NAME);
    assert_eq!(doc["folder"]["id"], FOLDER_ID);
    assert_eq!(doc["page_size"], 2);
    let mut e = env("drive-python");
    for (id, b64) in doc["media"].as_object().unwrap() {
        e.stub.set_media(
            id,
            base64::engine::general_purpose::STANDARD
                .decode(b64.as_str().unwrap())
                .unwrap(),
        );
    }
    let index = e.index_stage();
    let fetch = e.fetch_stage();
    let marker_files = [
        "derivatives/idA.r90.jpg",
        "derivatives/idB.jpg",
        "derivatives/idE.jpg",
    ];
    let mut rows_before_emptied = Value::Null;
    let mut final_python_displays = Value::Null;
    let mut rust_fetch_cycles = Vec::new();
    // `changes` から先は、idA の手動の位置が Python との差（決定による差）になる
    let mut manual_decision = false;

    for phase in doc["phases"].as_array().unwrap() {
        let name = phase["name"].as_str().unwrap();
        let py = &phase["python"];
        manual_decision |= name == "changes";
        // この段階の Python の行（決定による差だけを反映したもの）
        let py_rows = || {
            if manual_decision && py["rows"].as_array().is_some_and(|r| !r.is_empty()) {
                python_rows_with_the_manual_location_decision(&py["rows"])
            } else {
                py["rows"].clone()
            }
        };
        e.sql("UPDATE photos SET indexed_at='2000-01-01T00:00:00'");
        for op in phase["ops"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            match op["op"].as_str().unwrap() {
                "sql" => {
                    let sql: &'static str =
                        Box::leak(op["sql"].as_str().unwrap().to_string().into_boxed_str());
                    e.sql(sql);
                }
                "mkfile" => {
                    let path = e.root().join(op["path"].as_str().unwrap());
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, b"derivative").unwrap();
                }
                other => panic!("{other}"),
            }
        }
        e.sql("UPDATE photos SET indexed_at='2000-01-01T00:00:00' WHERE indexed_at IS NULL");
        e.stub.clear_requests();
        e.stub.clear_faults();
        let rows_now;
        match phase["kind"].as_str().unwrap() {
            "index" => {
                e.listing(phase["remote"].as_array().unwrap().clone());
                let before = normalized(&e.rows());
                if let Some(page) = phase.get("fail_at_page").and_then(Value::as_u64) {
                    let token = format!("page-{}", (page - 1) * 2);
                    e.stub.add_fault(Box::new(move |r| {
                        (r.param("pageToken") == Some(token.as_str()))
                            .then(|| (500, "{}".to_string(), vec![]))
                    }));
                }
                let result = e.run(&index);
                let mut requests = listing_requests(&e);
                let py_requests = py["requests"].as_array().unwrap();
                if py["raised"] == json!(true) {
                    // Python は例外で DB に触れない。Rust は再試行ぶん要求が多いので、Python の要求の数まで比べる
                    assert!(
                        matches!(result, StageResult::Failed(_)),
                        "{name}: {result:?}"
                    );
                    requests.truncate(py_requests.len());
                    assert_eq!(normalized(&e.rows()), before, "{name}");
                } else {
                    let StageResult::Done(counts) = result else {
                        panic!("{name}: {result:?}");
                    };
                    let py_counts: Vec<u64> = py["counts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap())
                        .collect();
                    assert_eq!(get(&counts, "remote"), py_counts[0], "{name}");
                    assert_eq!(get(&counts, "added"), py_counts[1], "{name}");
                    if name == "emptied" {
                        // 意図した差: Python は一覧が空だと全行を消す。Rust は消さない
                        assert_eq!(get(&counts, "removed"), 0);
                        assert_eq!(get(&counts, "deletes_withheld"), py_counts[2]);
                    } else {
                        assert_eq!(get(&counts, "removed"), py_counts[2], "{name}");
                    }
                }
                assert_eq!(
                    Value::Array(requests),
                    py["requests"],
                    "{name}: Drive への問い合わせ"
                );
                rows_now = normalized(&e.rows());
                if name == "emptied" {
                    assert_eq!(rows_now, rows_before_emptied, "{name}: 消さない");
                } else {
                    assert_eq!(rows_now, py_rows(), "{name}: photos の全列");
                }
                rows_before_emptied = rows_now.clone();
            }
            "fetch" => {
                e.set_fetch_limit(phase["limit"].as_u64().unwrap() as u32);
                let media_before = e.stub.media_requests().len();
                e.done(&fetch);
                let attempts = media_ids(&e, media_before);
                let py_attempts: Vec<String> = py["attempts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
                let first = rust_fetch_cycles.is_empty();
                if first {
                    // 最初のサイクルは Python と同じ写真を同じ順に試し、同じ行・同じ寸法になる
                    assert_eq!(attempts, py_attempts, "{name}");
                    assert_eq!(normalized(&e.rows()), py_rows(), "{name}");
                    assert_eq!(derivative_sizes(&e), py["sizes"], "{name}");
                } else if name.contains("limit1") {
                    // Python は失敗する idF が毎回枠を占め、後ろの写真を取得できない。Rust は idF を飛ばして先へ進む
                    assert_eq!(py_attempts, ["idF"], "{name}: Python は idF から動けない");
                    assert!(
                        !attempts.contains(&"idF".to_string()),
                        "{name}: {attempts:?}"
                    );
                    assert_eq!(attempts.len(), 1, "{name}");
                }
                assert!(e.tmp_leftovers().is_empty(), "{name}");
                rust_fetch_cycles.push(attempts);
                final_python_displays = py["rows"].clone();
                rows_now = normalized(&e.rows());
                rows_before_emptied = rows_now.clone();
            }
            other => panic!("{other}"),
        }
        let _ = rows_now;
        let exists: serde_json::Map<String, Value> = marker_files
            .iter()
            .map(|p| (p.to_string(), json!(e.root().join(p).exists())))
            .collect();
        if name != "emptied" {
            // 消える行の derivative（idE）は一緒に消え、残る行のものは残る。取得の段階では、
            // Rust の取得の進み方が違うのでファイルの有無は index の段階だけで比べる
            if phase["kind"] == "index" {
                assert_eq!(Value::Object(exists), py["exists"], "{name}");
            }
        }
    }

    // 十分なサイクルを回せば、Rust も Python の最終状態と同じ写真が取得済みになる
    // （取得できないのは idF・idG だけ）。Python の最後のサイクルのあとの表示と比べる
    let displayed = |rows: &Value| -> Vec<String> {
        let mut ids: Vec<String> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| !r["display"].is_null() && r["source"] == "drive")
            .map(|r| r["drive_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    // `emptied` は行を消さないので、`rows_before_emptied` は最後の取得のあとの Rust の状態
    assert_eq!(
        displayed(&rows_before_emptied),
        displayed(&final_python_displays)
    );
    assert_eq!(rust_fetch_cycles.len(), 4);
}

#[test]
fn only_the_original_download_gets_the_long_timeout() {
    // agent 全体の時間切れを 150 ms にして、応答を 500 ms 遅らせる。一覧は時間切れ、原本のダウンロードは
    // 要求ごとの設定（15 分）で最後まで読める
    let e = env("drive-timeout");
    e.listing(vec![jpeg("t1", "a.jpg", "m1")]);
    e.stub.set_media("t1", vec![1u8; 100]);
    let slow = |name: &'static str| -> stub::Fault {
        Box::new(move |r| {
            let hit = if name == "media" {
                r.is_media()
            } else {
                r.param("fields")
                    .is_some_and(|f| f.starts_with("nextPageToken"))
            };
            if hit {
                std::thread::sleep(Duration::from_millis(500));
            }
            None
        })
    };
    let http = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(150)))
            .http_status_as_error(false)
            .build(),
    );
    let stop = StopFlag::new();
    let session = Session::new(&e.shared, &http, &stop, &e.key);
    session.token().unwrap();
    e.stub.add_fault(slow("list"));
    assert_eq!(
        session.list_folder(FOLDER_ID).unwrap_err(),
        DriveError::Net(Failure::Timeout)
    );
    e.stub.clear_faults();
    e.stub.add_fault(slow("media"));
    let mut out = Vec::new();
    assert_eq!(session.download("t1", &mut out, 1000).unwrap(), 100);
}

#[test]
fn a_401_that_survives_a_new_token_stops_the_stage_without_blaming_the_photos() {
    // トークンを取り直しても断られる（サービスアカウント側の問題）。写真の失敗として記録しない
    let e = env("drive-401");
    e.listing(vec![jpeg("u1", "a.jpg", "m1"), jpeg("u2", "b.jpg", "m2")]);
    e.stub.set_media("u1", fixture_jpeg("typical"));
    e.stub.set_media("u2", fixture_jpeg("typical"));
    e.index();
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| (401, "{}".to_string(), vec![]))
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    assert!(
        message.contains("401") && message.contains("attempted=1"),
        "{message}"
    );
    // 取り直しは 1 回だけ（最初の要求 + 取り直したあとの要求の 2 回で止まる）
    assert_eq!(e.stub.media_requests().len(), 2);
    assert_eq!(e.stub.tokens_issued(), 2);
    // 写真の失敗として覚えていない: 直れば、待たずに取得できる
    e.stub.clear_faults();
    let counts = e.done(&fetch);
    assert_eq!(
        (get(&counts, "fetched"), get(&counts, "skipped_backoff")),
        (2, 0)
    );
}

// ---------------------------------------------------------------------------
// 通信の障害と写真の失敗の区別（レビュー指摘）

/// 取得待ちの 2 枚を作り、索引まで済ませる。
fn two_pending(tag: &str) -> Env {
    let e = env(tag);
    e.listing(vec![jpeg("o1", "a.jpg", "m1"), jpeg("o2", "b.jpg", "m2")]);
    e.stub.set_media("o1", fixture_jpeg("typical"));
    e.stub.set_media("o2", fixture_jpeg("typical"));
    e.index();
    e
}

/// 障害が直った次のサイクルで、2 枚とも待たずに取得される。
fn recovers_without_waiting(e: &Env, fetch: &FetchDerivatives) {
    e.stub.clear_faults();
    let counts = e.done(fetch);
    assert_eq!(
        (
            get(&counts, "fetched"),
            get(&counts, "skipped_backoff"),
            get(&counts, "pending")
        ),
        (2, 0, 2),
        "{counts}"
    );
}

#[test]
fn a_rate_limit_that_outlasts_the_retries_stops_the_stage_without_blaming_the_photos() {
    let e = two_pending("drive-ratelimit-fetch");
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                403,
                r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#.to_string(),
                vec![],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    // 流量制限だと分かる文面で、最初の 1 枚で止まった（2 枚目は試さない）
    assert!(
        message.contains("流量制限") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 4); // 最初 + 再試行 3 回
    assert!(e.tmp_leftovers().is_empty());
    recovers_without_waiting(&e, &fetch);
}

#[test]
fn a_permission_403_is_still_that_photos_failure() {
    let e = two_pending("drive-403-photo");
    e.stub.add_fault(Box::new(|r| {
        (r.is_media() && r.path.ends_with("/o1")).then(|| {
            (
                403,
                r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#.to_string(),
                vec![],
            )
        })
    }));
    let counts = e.done(&e.fetch_stage());
    assert_eq!(
        (get(&counts, "fetched"), get(&counts, "fail_download")),
        (1, 1)
    );
}

#[test]
fn a_timeout_waiting_for_the_response_stops_the_stage_without_blaming_the_photos() {
    let e = two_pending("drive-timeout-head");
    let http = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_recv_response(Some(Duration::from_millis(150)))
            .http_status_as_error(false)
            .build(),
    );
    e.stub.add_fault(Box::new(|r| {
        if r.is_media() {
            std::thread::sleep(Duration::from_millis(500));
        }
        None
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run_with_http(&fetch, &http) else {
        panic!("失敗のはず");
    };
    assert!(
        message.contains("timeout") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    recovers_without_waiting(&e, &fetch);
}

#[test]
fn a_body_that_takes_too_long_is_that_photos_failure_and_the_rest_continue() {
    // スタブは 1 本のスレッドなので、遅い本文を送っている間（450 ms）は次の要求が待たされる。上限（300 ms）は
    // その待ちを含めても 2 枚目が間に合う長さにしてある
    let mut e = two_pending("drive-timeout-body");
    e.shared = Arc::new(
        DriveShared::with(Endpoints::stub(&e.stub.base_url()), FAST_RETRY)
            .with_download_timeout(Duration::from_millis(300)),
    );
    e.stub.add_fault(Box::new(|r| {
        (r.is_media() && r.path.ends_with("/o1")).then(|| {
            (
                200,
                "x".repeat(10),
                vec![("X-Slow-Body".to_string(), "450".to_string())],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let counts = e.done(&fetch);
    assert_eq!(
        (
            get(&counts, "attempted"),
            get(&counts, "fetched"),
            get(&counts, "fail_slow_download")
        ),
        (2, 1, 1),
        "{counts}"
    );
    assert!(e.tmp_leftovers().is_empty());
    // その写真は次のサイクルでは飛ばされる（失敗が続く写真が枠を塞がない仕組みの対象）
    e.stub.clear_faults();
    let counts = e.done(&fetch);
    assert_eq!(
        (get(&counts, "skipped_backoff"), get(&counts, "attempted")),
        (1, 0)
    );
}

/// 本文の読み出しの失敗は、`read_error` の表どおりに分類される。
#[test]
fn body_read_errors_follow_the_table() {
    for kind in [
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::NotConnected,
    ] {
        let error = read_error(&io::Error::from(kind));
        assert_eq!(error, DriveError::Net(Failure::Disconnected), "{kind:?}");
        assert!(error.is_outage(), "{kind:?}");
    }
    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
        let error = read_error(&io::Error::from(kind));
        assert_eq!(error, DriveError::BodyTimeout, "{kind:?}");
        assert!(!error.is_outage(), "{kind:?}");
    }
    // `ureq` が包んで返す時間切れ（全体の上限）も、原本の本文では写真の失敗
    let wrapped = ureq::Error::Timeout(ureq::Timeout::Global).into_io();
    assert_eq!(read_error(&wrapped), DriveError::BodyTimeout);
    assert_eq!(
        read_error(&io::Error::from(io::ErrorKind::PermissionDenied)),
        DriveError::Io(io::ErrorKind::PermissionDenied)
    );
}

#[test]
fn a_disconnect_before_the_response_headers_stops_the_stage_without_blaming_the_photos() {
    let e = two_pending("drive-disconnect-head");
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                200,
                String::new(),
                vec![("X-Close-Before-Headers".to_string(), "1".to_string())],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    // 最初の 1 枚で止まった（2 枚目は試さない）。再試行もしない（通信の障害は次のサイクルに任せる）
    assert!(
        message.contains("disconnected") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    // 写真の失敗として覚えていないので、復旧した次のサイクルで 2 枚とも待たずに取得される
    recovers_without_waiting(&e, &fetch);
}

#[test]
fn a_disconnect_while_exchanging_the_token_is_an_outage_not_an_auth_error() {
    let e = env("drive-disconnect-token");
    e.stub.add_fault(Box::new(|r| {
        (r.path == "/token").then(|| {
            (
                200,
                String::new(),
                vec![("X-Close-Before-Headers".to_string(), "1".to_string())],
            )
        })
    }));
    // 認証の失敗（両方の段を飛ばす）ではなく、段の失敗
    let StageResult::Failed(message) = e.run(&e.index_stage()) else {
        panic!("失敗のはず");
    };
    assert!(message.contains("disconnected"), "{message}");
    assert!(!e.shared.auth_failed());
}

#[test]
fn a_disconnect_in_the_middle_of_an_original_stops_the_stage_without_blaming_the_photos() {
    let e = two_pending("drive-disconnect-body");
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                200,
                "x".repeat(10),
                vec![("X-Claim-Length".to_string(), "1000".to_string())],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    assert!(
        message.contains("disconnected") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    recovers_without_waiting(&e, &fetch);
}

#[test]
fn a_403_whose_reason_body_is_cut_off_is_an_outage_not_a_permission_error() {
    let e = two_pending("drive-403-cut");
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                403,
                r#"{"error":{"errors":[{"rea"#.to_string(),
                vec![("X-Claim-Length".to_string(), "500".to_string())],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    // 理由が読めなかった 403 を権限の不足（写真の失敗）にしない。再試行もしない
    assert!(
        message.contains("disconnected") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    recovers_without_waiting(&e, &fetch);
}

/// 403 の理由の本文が壊れていて読めない（`ureq` が `Protocol`・`Decompress` を返す）ときも、障害として段を
/// 止める。ヘッダーの `Protocol`（写真の失敗）とは別の扱い（理由は §14.3 の表）。
fn assert_unreadable_reason_stops_the_stage(tag: &str, flag: &str) {
    let e = two_pending(tag);
    let flag = flag.to_string();
    e.stub.add_fault(Box::new(move |r| {
        r.is_media()
            .then(|| (403, String::new(), vec![(flag.clone(), "1".to_string())]))
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    assert!(
        message.contains("reason_unreadable") && message.contains("attempted=1"),
        "{message}"
    );
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    recovers_without_waiting(&e, &fetch);
}

/// 本文の上限は、圧縮後でなく**展開後**の大きさに効く（圧縮後は数十 KB でも、展開後が上限を超えれば断る）。
/// 上限ちょうどは通り、1 バイト超えたら断る。
#[test]
fn size_limits_apply_to_the_decompressed_body() {
    let e = env("drive-gzip-limits");
    let probe = |status: u16, head: &str, pad: usize| {
        let head = head.to_string();
        e.stub.clear_faults();
        e.stub.add_fault(Box::new(move |_| {
            Some((
                status,
                head.clone(),
                vec![("X-Gzip-Pad".to_string(), pad.to_string())],
            ))
        }));
        crate::net::agent()
            .get(&format!("{}/probe", e.stub.base_url()))
            .call()
            .unwrap()
    };
    // 一覧・トークンの JSON（read_json）
    let json_max = MAX_JSON_BYTES as usize;
    assert_eq!(read_json(probe(200, "{}", json_max - 2)), Ok(json!({})));
    assert_eq!(
        read_json(probe(200, "{}", json_max - 1)),
        Err(DriveError::TooLarge)
    );
    // 403 の理由の本文（先頭に流量制限の語があるので、全部読めれば true。超えたら読み捨てて false）
    let reason = "rateLimitExceeded";
    let reason_max = 16 << 10;
    assert_eq!(
        is_rate_limit_body(&mut probe(403, reason, reason_max - reason.len())),
        Ok(true)
    );
    assert_eq!(
        is_rate_limit_body(&mut probe(403, reason, reason_max - reason.len() + 1)),
        Ok(false)
    );
}

/// 原本のダウンロードも展開後の大きさで数える（圧縮後の大きさを見る Content-Length の事前検査は、
/// 展開する本文では効かない）。展開後が上限を大きく超える本文は、書き出す前に止まる。
#[test]
fn download_counts_the_decompressed_size() {
    let e = env("drive-gzip-download");
    e.listing(vec![]);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let session = Session::new(&e.shared, &http, &stop, &e.key);
    let serve = |pad: usize| {
        e.stub.clear_faults();
        e.stub.add_fault(Box::new(move |r| {
            r.is_media().then(|| {
                (
                    200,
                    String::new(),
                    vec![("X-Gzip-Pad".to_string(), pad.to_string())],
                )
            })
        }));
    };
    serve(10_000);
    let mut out = Vec::new();
    assert_eq!(session.download("big", &mut out, 10_000), Ok(10_000));
    assert_eq!(out.len(), 10_000);
    serve(10_001);
    let mut out = Vec::new();
    assert_eq!(
        session.download("big", &mut out, 10_000),
        Err(DriveError::TooLarge)
    );
    // 超える前の分を書いてから `TooLarge` になりうる（分割して返る応答）ので、書いた量は上限以内。
    // 全体を捨てる責任は呼び出し側（`fetch_one` が原本を必ず消す。途中で切れる本文の試験が確かめている）
    assert!(out.len() <= 10_000, "上限を超えて書かない");
    // 展開後の本文が複数回に分かれて返る場合は、上限までの分だけが書かれて止まる（書き込みが部分的になる例）
    let chunk = COPY_CHUNK;
    serve(4 * chunk);
    let mut out = Vec::new();
    assert_eq!(
        session.download("big", &mut out, 2 * chunk as u64),
        Err(DriveError::TooLarge)
    );
    assert!(
        !out.is_empty() && out.len() <= 2 * chunk,
        "途中まで書いてから止まる: {}",
        out.len()
    );
    // 圧縮後は小さくても、展開後が 64 MiB の本文（上限は 1000 バイト）
    serve(64 << 20);
    let mut out = Vec::new();
    assert_eq!(
        session.download("big", &mut out, 1000),
        Err(DriveError::TooLarge)
    );
    assert!(out.len() <= 1000);
}

/// 展開後が空になる gzip（空のメンバーの繰り返し）は、展開後の数えでは止まらない。圧縮前の上限
/// （`net::wire_limit`）で止まり、原本は `TooLarge`（その写真の失敗。障害ではない）、小さな JSON・理由の本文は
/// `TooLarge`・読み捨て扱いになる。上限の内側の空メンバー（正当な範囲）は通る。
#[test]
fn empty_gzip_members_are_stopped_by_the_wire_limit() {
    let e = env("drive-gzip-empty-members");
    e.listing(vec![]);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let session = Session::new(&e.shared, &http, &stop, &e.key);
    let serve = |members: usize| {
        e.stub.clear_faults();
        e.stub.add_fault(Box::new(move |r| {
            (r.is_media() || r.path.ends_with("/probe")).then(|| {
                (
                    200,
                    String::new(),
                    vec![("X-Gzip-Empty-Members".to_string(), members.to_string())],
                )
            })
        }));
    };
    let member = crate::testutil::gzip::padded(b"", 0).len();
    let limit = 1000u64;
    let wire = crate::net::wire_limit(limit) as usize;
    // 圧縮前の上限の内側: 展開後は空なので 0 バイトで終わる
    serve(wire / member - 1);
    let mut out = Vec::new();
    assert_eq!(session.download("big", &mut out, limit), Ok(0));
    // 圧縮前の上限を超える: 展開後は空のままだが止まる
    serve(wire / member + 2);
    let mut out = Vec::new();
    let error = session.download("big", &mut out, limit).unwrap_err();
    assert_eq!(error, DriveError::TooLarge);
    assert!(!error.is_outage());
    assert!(out.is_empty());
    // 小さな JSON（read_json）も同じ
    let json_wire = crate::net::wire_limit(MAX_JSON_BYTES) as usize;
    serve(json_wire / member + 2);
    let response = crate::net::agent()
        .get(&format!("{}/probe", e.stub.base_url()))
        .call()
        .unwrap();
    assert_eq!(read_json(response), Err(DriveError::TooLarge));
}

/// スタブの壊れた本文が、実際に `ureq` の `Protocol`・`Decompress` を返すこと（この 2 つが理由の本文の
/// 取りこぼしだった）と、`reason_body_failure` の表。
#[test]
fn reason_body_failures_follow_the_table() {
    let e = env("drive-reason-table");
    for flag in ["X-Bad-Chunk", "X-Truncated-Gzip"] {
        let header = flag.to_string();
        e.stub.add_fault(Box::new(move |_| {
            Some((403, String::new(), vec![(header.clone(), "1".to_string())]))
        }));
        let url = format!("{}/probe", e.stub.base_url());
        let mut response = crate::net::agent().get(&url).call().unwrap();
        let error = response
            .body_mut()
            .with_config()
            .limit(16 << 10)
            .read_to_vec()
            .unwrap_err();
        match (flag, &error) {
            ("X-Bad-Chunk", ureq::Error::Protocol(_)) => {}
            ("X-Truncated-Gzip", ureq::Error::Decompress("gzip", _)) => {}
            _ => panic!("想定外のエラー: {flag}"),
        }
        // ヘッダーの前なら写真の失敗（`Other`）になる種類でも、理由の本文では障害
        assert_eq!(Failure::from(&error), Failure::Other);
        assert_eq!(reason_body_failure(&error), Failure::ReasonUnreadable);
        e.stub.clear_faults();
    }
    let io = |kind| ureq::Error::Io(io::Error::from(kind));
    for (error, expected) in [
        (io(io::ErrorKind::UnexpectedEof), Failure::Disconnected),
        (io(io::ErrorKind::ConnectionReset), Failure::Disconnected),
        (io(io::ErrorKind::TimedOut), Failure::Timeout),
        (
            ureq::Error::Timeout(ureq::Timeout::Global),
            Failure::Timeout,
        ),
        (ureq::Error::Tls("x"), Failure::Tls),
        (
            ureq::Error::Decompress("gzip", io::Error::from(io::ErrorKind::UnexpectedEof)),
            Failure::ReasonUnreadable,
        ),
        (ureq::Error::BadUri("x".into()), Failure::ReasonUnreadable),
        (ureq::Error::TooManyRedirects, Failure::ReasonUnreadable),
    ] {
        let failure = reason_body_failure(&error);
        assert_eq!(failure, expected);
        assert!(failure.is_outage());
    }
    assert!(DriveError::Net(Failure::ReasonUnreadable).is_outage());
}

#[test]
fn a_403_whose_reason_body_is_malformed_chunked_is_an_outage() {
    assert_unreadable_reason_stops_the_stage("drive-403-bad-chunk", "X-Bad-Chunk");
}

#[test]
fn a_403_whose_reason_body_is_a_truncated_gzip_is_an_outage() {
    assert_unreadable_reason_stops_the_stage("drive-403-bad-gzip", "X-Truncated-Gzip");
}

#[test]
fn a_403_whose_reason_body_times_out_is_an_outage_and_not_a_slow_download() {
    let mut e = two_pending("drive-403-slow");
    e.shared = Arc::new(
        DriveShared::with(Endpoints::stub(&e.stub.base_url()), FAST_RETRY)
            .with_download_timeout(Duration::from_millis(300)),
    );
    e.stub.add_fault(Box::new(|r| {
        r.is_media().then(|| {
            (
                403,
                r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#.to_string(),
                vec![("X-Slow-Body".to_string(), "700".to_string())],
            )
        })
    }));
    let fetch = e.fetch_stage();
    let StageResult::Failed(message) = e.run(&fetch) else {
        panic!("失敗のはず");
    };
    // 原本の本文の時間切れ（`fail_slow_download`・写真の失敗）ではなく、通信の障害として止まる
    assert!(
        message.contains("timeout") && message.contains("attempted=1"),
        "{message}"
    );
    assert!(!message.contains("fail_slow_download"), "{message}");
    assert_eq!(e.stub.media_requests().len(), 1);
    assert!(e.tmp_leftovers().is_empty());
    // スタブは 1 本のスレッドで、遅い本文を送り終える（700 ms）まで次の要求を受けられない
    std::thread::sleep(Duration::from_millis(800));
    recovers_without_waiting(&e, &fetch);
}

// ---------------------------------------------------------------------------
// 大量削除の保留

/// DB に Drive の行 `d1..=dN`（md5 は `m`）を作る。派生物の `display` も付けて、ファイルを置く。
fn seed_drive_rows(e: &Env, rows: usize) {
    fs::create_dir_all(e.root().join("derivatives")).unwrap();
    e.writer
        .call(move |conn| {
            for i in 1..=rows {
                conn.execute(
                    "INSERT INTO photos(source, drive_id, name, md5, display, favorite)
                     VALUES('drive', ?1, 'n', 'm', ?2, 1)",
                    rusqlite::params![format!("d{i}"), format!("derivatives/d{i}.jpg")],
                )?;
            }
            Ok(())
        })
        .unwrap();
    for i in 1..=rows {
        fs::write(e.root().join(format!("derivatives/d{i}.jpg")), b"x").unwrap();
    }
}

/// 一覧に `d1..=dKEEP` だけを返す（md5 は DB と同じなので、更新も追加もない）。
fn list_first(e: &Env, keep: usize) {
    e.stub.set_page_size(1000);
    e.listing(
        (1..=keep)
            .map(|i| jpeg(&format!("d{i}"), "n", "m"))
            .collect(),
    );
}

fn row_count(e: &Env) -> usize {
    e.rows().len()
}

#[test]
fn a_listing_that_would_delete_most_rows_holds_the_deletes_back() {
    // 100 件から 1 件だけ見える一覧（0 件ではないので、空の一覧の保護は効かない）
    let e = env("drive-mass-1");
    seed_drive_rows(&e, 100);
    list_first(&e, 1);
    let before = e.rows();
    let counts = e.index();
    assert_eq!(
        (
            get(&counts, "removed"),
            get(&counts, "mass_delete_withheld"),
            get(&counts, "deletes_withheld")
        ),
        (0, 99, 0)
    );
    assert_eq!(e.rows(), before);
    assert!(e.root().join("derivatives/d50.jpg").exists());
    // 保留しても、追加と更新は通常どおり
    e.listing(vec![jpeg("d1", "n", "m"), jpeg("new1", "n", "m9")]);
    let counts = e.index();
    assert_eq!(
        (
            get(&counts, "added"),
            get(&counts, "removed"),
            get(&counts, "mass_delete_withheld")
        ),
        (1, 0, 99)
    );
    assert_eq!(row_count(&e), 101);
}

#[test]
fn the_mass_delete_threshold_is_more_than_50_rows_and_more_than_20_percent() {
    // (Drive の行, 一覧に残す件数, 消える件数が保留されるか)
    for (rows, keep, withheld) in [
        (100, 50, false),  // 消す 50 件: 件数が「超える」でない
        (100, 49, true),   // 消す 51 件で 51%
        (300, 240, false), // 消す 60 件: ちょうど 20%（超えない）
        (300, 239, true),  // 消す 61 件: 20% を超える
        (400, 349, false), // 消す 51 件（12.75%）: 割合が小さい
        (60, 5, true),     // 消す 55 件（92%）
        (50, 1, false),    // 消す 49 件: 件数が小さい
    ] {
        let e = env("drive-mass-edge");
        seed_drive_rows(&e, rows);
        list_first(&e, keep);
        let counts = e.index();
        let doomed = rows - keep;
        if withheld {
            assert_eq!(
                (
                    get(&counts, "removed"),
                    get(&counts, "mass_delete_withheld")
                ),
                (0, doomed as u64),
                "{rows} 件から {keep} 件"
            );
            assert_eq!(row_count(&e), rows);
        } else {
            assert_eq!(
                (
                    get(&counts, "removed"),
                    get(&counts, "mass_delete_withheld")
                ),
                (doomed as u64, 0),
                "{rows} 件から {keep} 件"
            );
            assert_eq!(row_count(&e), keep);
        }
    }
}

#[test]
fn the_explicit_flag_lifts_the_mass_delete_and_the_empty_listing_holds() {
    let e = env("drive-mass-flag");
    seed_drive_rows(&e, 100);
    list_first(&e, 1);
    let allowing = IndexDrive::allowing_mass_delete(Arc::clone(&e.shared));
    let counts = e.done(&allowing);
    assert_eq!(
        (
            get(&counts, "removed"),
            get(&counts, "mass_delete_withheld"),
            get(&counts, "derivative_files_removed")
        ),
        (99, 0, 99)
    );
    assert_eq!(row_count(&e), 1);
    assert!(!e.root().join("derivatives/d50.jpg").exists());
    assert!(e.root().join("derivatives/d1.jpg").exists());
    // 0 件の一覧の保留も外れる（一度きりの、意図した全削除）
    e.listing(vec![]);
    assert_eq!(get(&e.done(&allowing), "removed"), 1);
    assert_eq!(row_count(&e), 0);
}

#[test]
fn dry_run_reports_the_hold_and_what_the_flag_would_do_without_writing() {
    let e = env("drive-mass-dry");
    seed_drive_rows(&e, 100);
    list_first(&e, 1);
    let before = e.rows();
    let stop = StopFlag::new();
    let StageResult::Done(held) = e.run_with(&e.index_stage(), Mode::DryRun, &stop) else {
        panic!("完了のはず");
    };
    assert_eq!(
        (get(&held, "removed"), get(&held, "mass_delete_withheld")),
        (0, 99)
    );
    let allowing = IndexDrive::allowing_mass_delete(Arc::clone(&e.shared));
    let StageResult::Done(lifted) = e.run_with(&allowing, Mode::DryRun, &stop) else {
        panic!("完了のはず");
    };
    assert_eq!(
        (
            get(&lifted, "removed"),
            get(&lifted, "mass_delete_withheld")
        ),
        (99, 0)
    );
    // どちらでも実 DB と派生物は変わらない
    assert_eq!(e.rows(), before);
    assert!(e.root().join("derivatives/d99.jpg").exists());
}

// ---------------------------------------------------------------------------
// 大量削除の保留の記憶（管理画面の `sync.mass_delete_hold`。T3.7）

#[test]
fn the_board_shows_a_ratio_hold_and_clears_when_the_listing_recovers() {
    let e = env("drive-board-ratio");
    let board = e.shared.mass_delete_board();
    seed_drive_rows(&e, 100);
    // まだサイクルが回っていない
    assert_eq!(board.current(), None);
    // 100 件から 1 件だけ見える一覧: 99 件を保留
    list_first(&e, 1);
    e.index();
    let hold = board.current().expect("保留が記録される");
    assert_eq!(
        (hold.held, hold.reason, hold.drive_rows),
        (99, HoldReason::Ratio, 100)
    );
    assert_eq!(hold.reason.as_str(), "ratio");
    assert_eq!(hold.since.len(), 19);
    // 一覧が元に戻れば（保留が要らなくなれば）消える
    list_first(&e, 100);
    e.index();
    assert_eq!(board.current(), None);
}

#[test]
fn the_board_shows_an_empty_listing_hold_and_the_flag_cycle_clears_it() {
    let e = env("drive-board-empty");
    let board = e.shared.mass_delete_board();
    seed_drive_rows(&e, 3);
    e.listing(vec![]);
    e.index();
    let hold = board.current().expect("空の一覧の保留が記録される");
    assert_eq!(
        (hold.held, hold.reason, hold.drive_rows),
        (3, HoldReason::EmptyListing, 3)
    );
    assert_eq!(hold.reason.as_str(), "empty_listing");
    // `--allow-mass-delete` のサイクルは保留せずに消すので、記憶も消える（同じ `serve` の中で解除した場合）
    let allowing = IndexDrive::allowing_mass_delete(Arc::clone(&e.shared));
    assert_eq!(get(&e.done(&allowing), "removed"), 3);
    assert_eq!(board.current(), None);
}

#[test]
fn the_board_is_untouched_by_dry_run_and_by_cycles_that_learned_nothing() {
    let e = env("drive-board-keep");
    let board = e.shared.mass_delete_board();
    seed_drive_rows(&e, 100);
    list_first(&e, 1);
    // dry-run は記録しない
    let stop = StopFlag::new();
    let StageResult::Done(_) = e.run_with(&e.index_stage(), Mode::DryRun, &stop) else {
        panic!("完了のはず");
    };
    assert_eq!(board.current(), None);
    e.index();
    let held = board.current().expect("保留");
    // 一覧の取得に失敗したサイクルは何も確かめていないので、記憶はそのまま
    e.stub.add_fault(Box::new(|r| {
        (r.path == "/drive/v3/files" && !r.is_media()).then(|| (500, "{}".to_string(), vec![]))
    }));
    assert!(matches!(e.run(&e.index_stage()), StageResult::Failed(_)));
    assert_eq!(board.current(), Some(held.clone()));
    e.stub.clear_faults();
    // 鍵が無い（Drive を使わない）サイクルも同じ
    fs::remove_file(&e.key).unwrap();
    assert!(matches!(e.run(&e.index_stage()), StageResult::Skipped(_)));
    assert_eq!(board.current(), Some(held));
}

#[test]
fn the_board_keeps_since_while_the_hold_continues() {
    let board = MassDeleteBoard::default();
    board.record(Some((99, HoldReason::Ratio, 100)), "2026-10-25 10:00:00");
    board.record(Some((98, HoldReason::Ratio, 100)), "2026-10-25 10:10:00");
    let hold = board.current().unwrap();
    assert_eq!(hold.held, 98);
    assert_eq!(hold.since, "2026-10-25 10:00:00");
    assert_eq!(hold.checked_at, "2026-10-25 10:10:00");
    // 保留が外れて、また付いたら新しい `since`
    board.record(None, "2026-10-25 10:20:00");
    assert_eq!(board.current(), None);
    board.record(
        Some((60, HoldReason::EmptyListing, 60)),
        "2026-10-25 10:30:00",
    );
    assert_eq!(board.current().unwrap().since, "2026-10-25 10:30:00");
}

// ---------------------------------------------------------------------------
// 原本の削除の失敗

#[test]
fn a_failed_removal_of_the_original_is_reported_for_success_and_for_failure() {
    // 取得中（スタブが応答する間）に tmp/ を書き込み不可にして、削除を失敗させる。ファイルの削除は
    // ディレクトリの書き込み権限が要るので、root 以外で動く試験でだけ成り立つ（CI・この機体は一般ユーザー）
    for (tag, media, fetched, failed_kind) in [
        ("ok", fixture_jpeg("typical"), 1, "ok_jpeg"),
        ("broken", broken(), 0, "fail_not_image"),
    ] {
        let e = env("drive-rmfail");
        e.listing(vec![jpeg("r1", "a.jpg", "m1")]);
        e.stub.set_media("r1", media);
        e.index();
        let tmp = e.root().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let locked = tmp.clone();
        e.stub.add_fault(Box::new(move |r| {
            if r.is_media() {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
            }
            None
        }));
        let counts = e.done(&e.fetch_stage());
        // 後始末（TempDir の削除が通るように、権限を戻す）
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(get(&counts, "original_remove_failed"), 1, "{tag}: {counts}");
        assert_eq!(get(&counts, "fetched"), fetched, "{tag}: {counts}");
        assert_eq!(get(&counts, failed_kind), 1, "{tag}: {counts}");
        // 消せなかった原本は残っている（次の起動時の掃除の対象）
        assert_eq!(e.tmp_leftovers(), [".dl_r1"], "{tag}");
        // 文面にはパスもファイル名もない
        let text = counts.to_string();
        assert!(!text.contains("r1") && !text.contains(".dl_"), "{text}");
    }
}

#[test]
fn the_original_is_removed_explicitly_and_the_drop_only_covers_what_is_left() {
    let dir = TempDir::new("drive-explicit");
    let path = dir.path().join("o");
    let (mut file, mut guard) = fetch::create_original(&path).unwrap();
    std::io::Write::write_all(&mut file, b"secret").unwrap();
    assert!(guard.remove().is_ok());
    assert!(!path.exists());
    // 既に無くても成功（二重に呼んでも落ちない）
    assert!(guard.remove().is_ok());
    // 消したあとに同じ名前で作られたファイルは、Drop が消さない（別の持ち主のもの）
    fs::write(&path, b"someone else's").unwrap();
    drop(guard);
    assert!(path.exists());
}

// ---------------------------------------------------------------------------
// 実際のログ出力の検査

#[test]
fn derived_debug_output_hides_names_times_and_locations() {
    let file = RemoteFile {
        id: "secretid0001".into(),
        name: Some("Private-Holiday-Name.jpg".into()),
        md5: Some("m1".into()),
        time: Some("2024:05:06 07:08:09".into()),
        lat: Some(35.123456),
        lon: Some(139.654321),
    };
    let listing = Listing {
        files: vec![file.clone()],
        pages: 1,
    };
    let entry = index::Entry {
        drive_id: "secretid0001".into(),
        name: "Private-Holiday-Name.jpg".into(),
        md5: Some("m1".into()),
        taken_at: "2024-05-06T07:08:09".into(),
        lat: Some(35.123456),
        lon: Some(139.654321),
    };
    let plan = index::DrivePlan {
        ops: vec![
            index::Op::Insert(entry.clone()),
            index::Op::Update(entry.clone()),
        ],
        deletes: vec![(1, Some("derivatives/secretid0001.jpg".into()))],
        ..Default::default()
    };
    for text in [
        format!("{file:?}"),
        format!("{listing:?}"),
        format!("{:?}", index::Op::Insert(entry.clone())),
        format!("{entry:?}"),
        format!("{plan:?}"),
        format!("{:#?}", Some(&listing)),
    ] {
        for needle in [
            "secretid",
            "Private",
            "Holiday",
            "35.12",
            "139.65",
            "2024",
            "07:08",
            "derivatives/",
        ] {
            assert!(!text.contains(needle), "{needle} in {text}");
        }
    }
    // 件数と有無は残る（失敗の表示が役に立つ）
    assert!(format!("{listing:?}").contains("files: 1"));
    assert!(format!("{file:?}").contains("has_location: true"));
}

#[test]
fn the_actual_log_output_never_contains_names_coordinates_times_keys_or_tokens() {
    let e = env("drive-reallog");
    let key_dir = e.root().display().to_string();
    let log = capture::logs_of(|| {
        // 1. 個人情報つきの一覧の索引と取得（成功・写真の失敗）
        e.listing(vec![
            located(
                "secretid0001",
                "Private-Holiday-Name.jpg",
                "m1",
                "2024:05:06 07:08:09",
                35.123456,
                139.654321,
            ),
            located(
                "secretid0002",
                "Another-Private-Name.jpg",
                "m2",
                "2024:05:07 08:09:10",
                35.223456,
                139.754321,
            ),
        ]);
        e.stub.set_media("secretid0001", broken());
        e.stub.set_media("secretid0002", fixture_jpeg("typical"));
        e.index();
        e.done(&e.fetch_stage());
        // 2. 流量制限の再試行（DEBUG の行）と、使い切った失敗
        e.stub.add_fault(Box::new(|r| {
            r.path
                .ends_with("/files")
                .then(|| (429, "{}".to_string(), vec![]))
        }));
        let _ = e.run(&e.index_stage());
        e.stub.clear_faults();
        // 3. 大量削除・0 件の一覧の保留の警告
        seed_drive_rows(&e, 100);
        list_first(&e, 1);
        e.index();
        e.listing(vec![]);
        e.index();
        // 4. トークンの取り直し（期限切れ）と 401 のやり直し
        e.stub.set_expires_in(30);
        e.listing(vec![jpeg("f1", "Private-Holiday-Name.jpg", "m1")]);
        e.index();
        e.stub.revoke_tokens();
        e.index();
        // 5. 鍵ファイルの権限の警告
        fs::set_permissions(&e.key, fs::Permissions::from_mode(0o644)).unwrap();
        e.shared.permission_warned.store(false, Ordering::SeqCst);
        e.stub.set_expires_in(3599);
        e.shared.state().token = None;
        e.index();
        // 6. 原本を消せなかった警告
        let e2 = env("drive-reallog-rm");
        e2.listing(vec![jpeg("secretid0003", "Private-Holiday-Name.jpg", "m3")]);
        e2.stub.set_media("secretid0003", fixture_jpeg("typical"));
        e2.index();
        let tmp = e2.root().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let locked = tmp.clone();
        e2.stub.add_fault(Box::new(move |r| {
            if r.is_media() {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
            }
            None
        }));
        e2.done(&e2.fetch_stage());
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
    });
    // 何も捕まえていない試験にならないように、出るはずの行があること
    for expected in [
        "[fetch] 取得に失敗しました",
        "再試行します",
        "消す行が多すぎます",
        "一覧が 0 件でした",
        "所有者以外にも読める権限",
        "原本の一時ファイルを削除できませんでした",
    ] {
        assert!(log.contains(expected), "{expected} が無い:\n{log}");
    }
    for needle in [
        "Private",
        "Holiday",
        "Another",
        ".jpg",
        "35.12",
        "35.22",
        "139.65",
        "139.75",
        "2024",
        "07:08",
        "secretid",
        "stub-token",
        "Bearer",
        "PRIVATE",
        "private_key",
        "BEGIN",
        "eyJ",
        "sa@example",
        "http",
        "127.0.0.1",
        FOLDER_NAME,
        FOLDER_ID,
        "drive_key",
        key_dir.as_str(),
        ".dl_",
    ] {
        assert!(!log.contains(needle), "{needle} in:\n{log}");
    }
}

// ---------------------------------------------------------------------------
// 回転（P3 / T3.2 の E14）との関係

fn rotate_api(e: &Env, id: i64, delta: i64) -> Result<Value, crate::post::ApiError> {
    let payload = json!({"id": id, "delta": delta});
    crate::rotate::rotate(payload.as_object().unwrap(), Some(&e.writer), e.root())
}

fn derivative_names(e: &Env) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(e.root().join("derivatives"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// 回した写真が取り直されるとき（`display` が NULL に戻った: 推定の取り消し・同じ原本の再取得）、
/// `fetch_derivatives` は行の `rotation` を当て、回転の API と同じ名前・同じ向きになる。
#[test]
fn a_rotated_photo_is_fetched_again_with_the_same_name_and_orientation() {
    let e = env("drive-fetch-after-rotate");
    e.listing(vec![jpeg("j1", "a.jpg", "m1")]);
    e.stub.set_media("j1", crate::rotate::tests::jpeg(80, 40));
    e.index();
    e.done(&e.fetch_stage());
    let id = id_of(&e, "j1");
    let measure = |name: &str| {
        crate::rotate::tests::measure(&fs::read(e.root().join("derivatives").join(name)).unwrap())
    };
    assert_eq!(
        measure("j1.jpg"),
        ((80, 40), vec!["red", "green", "blue", "yellow"])
    );

    // 管理画面で時計回りに 90°
    let out = rotate_api(&e, id, 90).unwrap();
    assert_eq!(out["thumb"], "/derivatives/j1.r90.jpg");
    let rotated = measure("j1.r90.jpg");
    assert_eq!(rotated, ((40, 80), vec!["blue", "red", "yellow", "green"]));
    assert_eq!(derivative_names(&e), ["j1.r90.jpg"]);

    // `display` が NULL に戻って（`rotation` は残る）取り直される: 同じ名前・同じ向き
    e.sql("UPDATE photos SET display=NULL WHERE drive_id='j1'");
    let counts = e.done(&e.fetch_stage());
    assert_eq!(get(&counts, "fetched"), 1);
    let row = row_of(&e, "j1").unwrap();
    assert_eq!(
        text(&row, "display").as_deref(),
        Some("derivatives/j1.r90.jpg")
    );
    assert_eq!(int(&row, "rotation"), 90);
    assert_eq!(measure("j1.r90.jpg"), rotated);
    assert_eq!(derivative_names(&e), ["j1.r90.jpg"]);

    // 4 回目の回転で 0 に戻ると、取り直しも元の名前で元の向き
    rotate_api(&e, id, 90).unwrap();
    rotate_api(&e, id, 90).unwrap();
    rotate_api(&e, id, 90).unwrap();
    assert_eq!(derivative_names(&e), ["j1.jpg"]);
    e.sql("UPDATE photos SET display=NULL WHERE drive_id='j1'");
    e.done(&e.fetch_stage());
    assert_eq!(
        measure("j1.jpg"),
        ((80, 40), vec!["red", "green", "blue", "yellow"])
    );
    assert_eq!(derivative_names(&e), ["j1.jpg"]);
}

/// 回転の途中（新しいファイルを書いたあと・DB を更新する前）に、同期サイクルが同じ写真の `display` を
/// NULL にし、取得が別の写真を取りに来る。取得はファイルの書き込みと DB の更新の間、回転の終わりまで待ち、
/// 回転は 409 で新しいファイルを消す。取り直しのあとも、`display` は実在するファイルを指し、孤児は残らない。
#[test]
fn fetch_waits_for_a_rotation_in_progress_and_leaves_no_orphan() {
    let e = env("drive-fetch-rotate-race");
    e.listing(vec![jpeg("j1", "a.jpg", "m1")]);
    e.stub.set_media("j1", crate::rotate::tests::jpeg(80, 40));
    e.index();
    e.done(&e.fetch_stage());
    let id1 = id_of(&e, "j1");
    // 2 枚目が加わる（まだ取得していない）
    e.listing(vec![jpeg("j1", "a.jpg", "m1"), jpeg("j2", "b.jpg", "m2")]);
    e.stub.set_media("j2", crate::rotate::tests::jpeg(80, 40));
    e.index();

    let payload = json!({"id": id1, "delta": 90});
    let (result, counts) = std::thread::scope(|scope| {
        let fetch = std::cell::RefCell::new(None);
        let result = crate::rotate::rotate_with_for_tests(
            payload.as_object().unwrap(),
            &e.writer,
            e.root(),
            &|| {
                // 取得が始まる。候補（j2 だけ。j1 は display があるので入らない）を決めて原本を取り、
                // derivative を作る前の排他で止まる。その状態になるまで待つ（実時間で推し量らない）
                let handle = scope.spawn(|| e.done(&e.fetch_stage()));
                let waiting = crate::imaging::lock_waiters::wait_until_waiting(
                    handle.thread().id(),
                    || handle.is_finished(),
                    Duration::from_secs(60),
                );
                *fetch.borrow_mut() = Some(handle);
                assert!(
                    waiting,
                    "取得は derivative を作る前に、回転が持つロックを待つ"
                );
                // 取得が待っているところで、同期サイクルが j1 の display を NULL にした（回転が読んだあと）
                e.sql("UPDATE photos SET display=NULL WHERE drive_id='j1'");
            },
        );
        let counts = match fetch.borrow_mut().take() {
            Some(handle) => handle.join().unwrap(),
            None => panic!("取得が始まっていません"),
        };
        (result, counts)
    });
    assert_eq!(get(&counts, "fetched"), 1, "j2 だけが取れた");
    assert_eq!(
        crate::rotate::tests::status(result),
        "409",
        "回転は負けて何も書かない"
    );
    // j2 が取れ、j1 は display が NULL のまま。回転の新しいファイル（j1.r90.jpg）は消えている
    assert_eq!(derivative_names(&e), ["j1.jpg", "j2.jpg"]);
    assert!(text(&row_of(&e, "j1").unwrap(), "display").is_none());
    // 次の取得が j1 を同じ名前で取り直す: display は実在するファイルを指す
    e.done(&e.fetch_stage());
    for drive_id in ["j1", "j2"] {
        let row = row_of(&e, drive_id).unwrap();
        let display = text(&row, "display").unwrap();
        assert!(e.root().join(&display).is_file(), "{drive_id}: {display}");
    }
    assert_eq!(derivative_names(&e), ["j1.jpg", "j2.jpg"]);
}
