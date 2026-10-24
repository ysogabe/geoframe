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
use crate::testutil::TempDir;
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
        let http = crate::net::agent();
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read,
            writer: (mode == Mode::Apply).then_some(&self.writer),
            cache: &self.cache,
            http: &http,
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
    assert!(!DriveError::Status(404).is_outage());
    assert!(!DriveError::Status(403).is_outage());
    assert!(DriveError::Net(Failure::Dns).is_outage());
    assert!(!DriveError::Net(Failure::Timeout).is_outage());
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

#[test]
fn a_changed_md5_updates_the_row_and_resets_the_derived_columns() {
    let e = env("drive-update");
    e.listing(vec![located(
        "f1",
        "a.jpg",
        "m1",
        "2024:05:06 07:08:09",
        35.0,
        139.0,
    )]);
    e.index();
    // 手動の位置・お気に入り・回転・表示・地名・推定の痕跡
    e.sql(
        "UPDATE photos SET loc_source='manual', lat=1.0, lon=2.0, favorite=1, hidden=1, rotation=90,
         place='東京', elev=12.5, display='derivatives/f1.r90.jpg', fetched_at='2024-01-01T00:00:00',
         inferred_from=7, infer_dt_s=60, title='題名', muni_cd='13101', pref='東京都', city='千代田区',
         indexed_at='2000-01-01T00:00:00'",
    );
    fs::create_dir_all(e.root().join("derivatives")).unwrap();
    fs::write(e.root().join("derivatives/f1.r90.jpg"), b"x").unwrap();
    e.listing(vec![jpeg("f1", "a2.jpg", "m1-new")]);
    let counts = e.index();
    assert_eq!(get(&counts, "updated"), 1);
    let rows = e.rows();
    let r = &rows[0];
    assert_eq!(text(r, "name").as_deref(), Some("a2.jpg"));
    assert_eq!(text(r, "md5").as_deref(), Some("m1-new"));
    // 手動の位置も上書きされる（現行と同じ。server.md §0）。位置が無くなれば NULL
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

    for phase in doc["phases"].as_array().unwrap() {
        let name = phase["name"].as_str().unwrap();
        let py = &phase["python"];
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
                    assert_eq!(rows_now, py["rows"], "{name}: photos の全列");
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
                    assert_eq!(normalized(&e.rows()), py["rows"], "{name}");
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
