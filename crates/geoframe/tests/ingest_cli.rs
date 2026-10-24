//! 取り込み（T2.1）をバイナリごと確かめる。合成の DB・合成の `.cache.json` だけを使う。
//!
//! - `sync --dry-run` は何も書かない（DB・キャッシュ・`tmp/`・ロックのファイル）。
//! - `GEOFRAME_INGEST` が無いと、`sync` も `serve` も書かない（既定は無効）。
//! - `GEOFRAME_INGEST=1` なら、DB が無くても作って 1 サイクル回し、`serve` はそのまま常駐する。

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_geoframe");
const PYTHON_SCHEMA: &str = include_str!("fixtures/schema-python.json");

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("geoframe-ingest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 本番相当（Python が全移行を終えた形）のスキーマの DB に、合成の行を入れる。閉じて WAL を畳む。
fn production_shaped_db(data_dir: &Path) {
    let conn = rusqlite::Connection::open(data_dir.join("catalog.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    let fixture: Value = serde_json::from_str(PYTHON_SCHEMA).unwrap();
    for object in fixture["objects"].as_array().unwrap() {
        if let Some(sql) = object["sql"].as_str() {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn.execute_batch(
        "INSERT INTO photos(source,drive_id,name,display,lat,lon,place) VALUES
           ('drive','a','n1','derivatives/a.jpg',1.0,2.0,'p'),
           ('drive','b','n2',NULL,1.0,2.0,NULL),
           ('drive','c','n3',NULL,NULL,NULL,NULL)",
    )
    .unwrap();
}

/// 検査の対象のファイルの中身（-wal/-shm は読み取りでも SQLite が作るので除く）。
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if rel.ends_with("-wal") || rel.ends_with("-shm") {
                continue;
            }
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(&path, root, out);
            } else {
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn dry_run_writes_nothing_and_reports_counts() {
    let dir = Dir::new("dry");
    let data = dir.0.as_path();
    production_shaped_db(data);
    fs::create_dir_all(data.join("tmp")).unwrap();
    fs::create_dir_all(data.join("derivatives")).unwrap();
    fs::write(data.join("tmp/.dl_a"), b"x").unwrap();
    fs::write(data.join("derivatives/.dl_b"), b"y").unwrap();
    fs::write(data.join(".cache.json"), r#"{"exif:a": 1, "geo:1": 2}"#).unwrap();
    let before = snapshot(data);

    let out = run(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(stdout.contains("dry-run: 何も書きません"), "{stdout}");
    assert!(
        stdout.contains(
            "スキーマ: 作る表・索引 0 / 足す列 0 / place を NULL に戻す なし / embedding の写し 0"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("消す .dl_* 2 件 / 捨てる .cache.json の旧キー 1 件"),
        "{stdout}"
    );
    for stage in [
        "index_local",
        "index_drive",
        "infer",
        "enrich",
        "local_derivatives",
        "fetch_derivatives",
        "quality",
    ] {
        assert!(
            stdout.contains(&format!("段 {stage}: 完了（件数なし）")),
            "{stage}\n{stdout}"
        );
    }
    assert!(
        stdout.contains("写真: 表示可能 1 / 取得待ち 1 / 全3"),
        "{stdout}"
    );

    // 何も変わっていない（ロックのファイルも作られない）
    assert_eq!(snapshot(data), before);
    assert!(!data.join(".ingest.lock").exists());
}

#[test]
fn dry_run_does_not_create_a_missing_database() {
    let dir = Dir::new("dry-missing");
    let out = run(
        &["sync", "--dry-run", "--data-dir", dir.0.to_str().unwrap()],
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir.0.join("catalog.db").exists());
    assert!(snapshot(&dir.0).is_empty());
}

#[test]
fn sync_without_ingest_refuses_and_writes_nothing() {
    let dir = Dir::new("noingest");
    production_shaped_db(&dir.0);
    let before = snapshot(&dir.0);
    for envs in [vec![], vec![("GEOFRAME_INGEST", "0")]] {
        let out = run(&["sync", "--data-dir", dir.0.to_str().unwrap()], &envs);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains("GEOFRAME_INGEST=1"));
    }
    assert_eq!(snapshot(&dir.0), before);
    assert!(!dir.0.join("tmp").exists() && !dir.0.join(".ingest.lock").exists());
}

#[test]
fn sync_with_ingest_creates_the_database_and_runs_one_cycle() {
    let dir = Dir::new("ingest");
    let data = dir.0.join("new-data");
    let out = run(
        &["sync", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_INGEST", "1")],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(data.join("catalog.db").exists());
    assert!(data.join("tmp").is_dir());
    assert!(stdout.contains("段 quality: 完了（件数なし）"), "{stdout}");
    assert!(
        stdout.contains("写真: 表示可能 0 / 取得待ち 0 / 全0"),
        "{stdout}"
    );
    // 作ったことは warn で知らせる
    assert!(
        stdout.contains("DB が無かったので新しく作ります"),
        "{stdout}"
    );
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 11);
}

#[test]
fn sync_with_ingest_on_a_production_shaped_database_changes_no_rows() {
    let dir = Dir::new("ingest-prod");
    production_shaped_db(&dir.0);
    let out = run(
        &["sync", "--data-dir", dir.0.to_str().unwrap()],
        &[("GEOFRAME_INGEST", "1")],
    );
    assert!(out.status.success(), "{}", text(&out.stdout));
    assert!(text(&out.stdout).contains("スキーマは最新です"));
    let conn = rusqlite::Connection::open(dir.0.join("catalog.db")).unwrap();
    let null_places: i64 = conn
        .query_row("SELECT COUNT(*) FROM photos WHERE place IS NULL", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(null_places, 2); // 入れたとおり。再 enrich の印は増えない
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn get_healthz(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

fn serve(data: &Path, port: u16, ingest: Option<&str>) -> Child {
    let mut command = Command::new(BIN);
    command
        .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
        .args(["--data-dir", data.to_str().unwrap()])
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(value) = ingest {
        command.env("GEOFRAME_INGEST", value);
    }
    command.spawn().unwrap()
}

fn wait_healthy(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            panic!("serve が終了しました");
        }
        if get_healthz(port).is_some_and(|r| r.starts_with("HTTP/1.1 200")) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("serve が /healthz に答えません");
}

/// SIGTERM を送って正常終了を待つ。標準出力を返す。
fn terminate(mut child: Child) -> (std::process::ExitStatus, String) {
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "SIGTERM で止まりません");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    (child.wait().unwrap(), stdout)
}

#[test]
fn serve_without_ingest_stays_read_only() {
    let dir = Dir::new("serve-ro");
    production_shaped_db(&dir.0);
    let before = snapshot(&dir.0);
    let port = free_port();
    let mut child = serve(&dir.0, port, None);
    wait_healthy(&mut child, port);
    let (status, stdout) = terminate(child);
    assert!(status.success(), "{stdout}");
    assert!(stdout.contains("取り込みは無効です"), "{stdout}");
    assert!(stdout.contains("ingest=0"), "{stdout}");
    // 既定では書かない: tmp/・ロックのファイルも作らず、DB もそのまま
    assert_eq!(snapshot(&dir.0), before);
}

#[test]
fn serve_without_ingest_refuses_a_missing_database() {
    let dir = Dir::new("serve-missing");
    let out = run(
        &[
            "serve",
            "--listen",
            &format!("127.0.0.1:{}", free_port()),
            "--data-dir",
            dir.0.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir.0.join("catalog.db").exists());
}

#[test]
fn serve_with_ingest_creates_the_database_runs_the_cycle_and_stops_cleanly() {
    let dir = Dir::new("serve-ingest");
    let data = dir.0.join("new-data");
    let port = free_port();
    let mut child = serve(&data, port, Some("1"));
    wait_healthy(&mut child, port);
    // 起動直後の 1 サイクルが回るのを待つ（ログは標準出力。終了後にまとめて見る）
    std::thread::sleep(Duration::from_millis(300));
    let (status, stdout) = terminate(child);
    assert!(status.success(), "{stdout}");
    assert!(stdout.contains("ingest=1"), "{stdout}");
    assert!(
        stdout.contains("DB が無かったので新しく作ります"),
        "{stdout}"
    );
    assert!(
        stdout.contains("サイクル完了: 表示可能 0 / 取得待ち 0 / 全0"),
        "{stdout}"
    );
    assert!(data.join("catalog.db").exists());
    assert!(data.join("tmp").is_dir());
    // ログに値は出ない（パス以外の個別の値が無いことの確認として、データ置き場以外のパスを出していない）
    assert!(!stdout.contains(".cache.json"), "{stdout}");
}

#[test]
fn a_second_ingest_process_on_the_same_data_dir_is_refused() {
    let dir = Dir::new("serve-lock");
    let data = dir.0.join("d");
    let port = free_port();
    let mut child = serve(&data, port, Some("1"));
    wait_healthy(&mut child, port);
    let out = run(
        &["sync", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_INGEST", "1")],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("別の geoframe が取り込み中"),
        "{}",
        text(&out.stderr)
    );
    // dry-run はロックを取らないので、動いている最中でも見られる
    let dry = run(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    assert!(dry.status.success(), "{}", text(&dry.stdout));
    let (status, _) = terminate(child);
    assert!(status.success());
}
