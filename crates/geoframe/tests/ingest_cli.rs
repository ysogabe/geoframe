//! 取り込み（T2.1）をバイナリごと確かめる。合成の DB・合成の `.cache.json` だけを使う。
//!
//! - `sync --dry-run` は何も書かない（DB・キャッシュ・`tmp/`・ロックのファイル）。
//! - `GEOFRAME_INGEST` が無いと、`sync` も `serve` も書かない（既定は無効）。
//! - `GEOFRAME_INGEST=1` なら、DB が無くても作って 1 サイクル回し、`serve` はそのまま常駐する。
//! - 本番への誤指定の防止（`guard.rs`）: 検証環境の印が無い・Python が非稼働と確認できないときは、
//!   書き込みの前に終了コード 2 で断り、置き場を一切変えない。
//!
//! 取り込みを有効にして動かすテストは、デバッグビルドだけにある差し替え（`GEOFRAME_TEST_STAGING_MARKER`・
//! `GEOFRAME_TEST_PYTHON_STATE`）で、印と Python の状態を与える。この機体は本番機で、本物の印は無く
//! `geoframe-server` が動いているので、差し替えなしでは取り込みのテストは全て断られる。

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

/// 検証環境の印（置き場の外の別ディレクトリに作る）。取り込みを動かすテストに渡す環境変数を返す。
struct Staging {
    _dir: Dir,
    marker: PathBuf,
}

impl Staging {
    fn new(tag: &str) -> Self {
        let dir = Dir::new(&format!("{tag}-staging"));
        let marker = dir.0.join("staging");
        fs::write(&marker, b"").unwrap();
        Self { _dir: dir, marker }
    }

    /// 印があり、Python が未導入（`LoadState=not-found`）の環境。
    fn envs(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "GEOFRAME_TEST_STAGING_MARKER",
                self.marker.to_str().unwrap().to_string(),
            ),
            ("GEOFRAME_TEST_PYTHON_STATE", "not-found/inactive".into()),
        ]
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

/// 検査の対象のファイルの中身（-wal/-shm は読み取りでも SQLite が作るので除く。つまり
/// 「何も書かない」は WAL 関連ファイルを除く）。
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

fn run_owned(args: &[&str], envs: &[(&str, String)]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
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

    // 行・ファイル・キャッシュは変わらず、ロックのファイルも作られない。増えてよいのは SQLite が
    // 読み取りでも作る WAL 関連ファイル（-wal・-shm）だけ
    assert_eq!(snapshot(data), before);
    assert!(!data.join(".ingest.lock").exists());
    let mut names: Vec<_> = fs::read_dir(data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n != "catalog.db-wal" && n != "catalog.db-shm")
        .collect();
    names.sort();
    assert_eq!(names, [".cache.json", "catalog.db", "derivatives", "tmp"]);
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
    let staging = Staging::new("ingest");
    let data = dir.0.join("new-data");
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&["sync", "--data-dir", data.to_str().unwrap()], &envs);
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
    let staging = Staging::new("ingest-prod");
    production_shaped_db(&dir.0);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&["sync", "--data-dir", dir.0.to_str().unwrap()], &envs);
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

fn serve(data: &Path, port: u16, ingest: Option<(&str, &Staging)>) -> Child {
    let mut command = Command::new(BIN);
    command
        .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
        .args(["--data-dir", data.to_str().unwrap()])
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((value, staging)) = ingest {
        command.env("GEOFRAME_INGEST", value).envs(staging.envs());
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
    let staging = Staging::new("serve-ingest");
    let mut child = serve(&data, port, Some(("1", &staging)));
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
    let staging = Staging::new("serve-lock");
    let mut child = serve(&data, port, Some(("1", &staging)));
    wait_healthy(&mut child, port);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&["sync", "--data-dir", data.to_str().unwrap()], &envs);
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

/// 置き場の全体（相対パスと中身）。ディレクトリの有無も含めて、断られたあとに変わっていないことを見る。
fn assert_untouched_after_refusal(data: &Path, before: &[(String, Vec<u8>)]) {
    assert_eq!(snapshot(data), before);
    for absent in [".ingest.lock", "tmp"] {
        assert!(!data.join(absent).exists(), "{absent}");
    }
    assert!(!data.join("catalog.db-wal").exists());
    assert!(!data.join("catalog.db-shm").exists());
}

fn data_with_leftovers(dir: &Dir) -> PathBuf {
    let data = dir.0.join("data");
    fs::create_dir_all(&data).unwrap();
    production_shaped_db(&data);
    fs::create_dir_all(data.join("derivatives")).unwrap();
    fs::write(data.join("derivatives/.dl_open"), b"downloading").unwrap();
    fs::write(data.join(".cache.json"), r#"{"exif:a": 1, "geo:1": 2}"#).unwrap();
    data
}

#[test]
fn without_the_staging_marker_ingest_is_refused_before_any_write() {
    let dir = Dir::new("guard-nomarker");
    let missing_marker = dir.0.join("no-such-marker");
    let data = data_with_leftovers(&dir);
    let before = snapshot(&data);
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        (
            "GEOFRAME_TEST_STAGING_MARKER",
            missing_marker.to_str().unwrap().to_string(),
        ),
        // Python が止まっていても、印が無ければ断る
        ("GEOFRAME_TEST_PYTHON_STATE", "not-found/inactive".into()),
    ];

    let out = run_owned(&["sync", "--data-dir", data.to_str().unwrap()], &envs);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("検証環境の印"),
        "{}",
        text(&out.stderr)
    );
    assert_untouched_after_refusal(&data, &before);
    // 開いている .dl_* は消されない（Python のダウンロードを壊さない）
    assert!(data.join("derivatives/.dl_open").exists());

    let out = run_owned(
        &[
            "serve",
            "--listen",
            &format!("127.0.0.1:{}", free_port()),
            "--data-dir",
            data.to_str().unwrap(),
        ],
        &envs,
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
    assert!(
        text(&out.stdout).contains("検証環境の印"),
        "{}",
        text(&out.stdout)
    );
    assert_untouched_after_refusal(&data, &before);

    // 置き場そのものが無いときも、作られない
    let fresh = dir.0.join("fresh");
    let out = run_owned(&["sync", "--data-dir", fresh.to_str().unwrap()], &envs);
    assert_eq!(out.status.code(), Some(2));
    assert!(!fresh.exists());
}

#[test]
fn the_staging_marker_alone_is_not_enough_while_python_runs_or_is_unknown() {
    let dir = Dir::new("guard-python");
    let staging = Staging::new("guard-python");
    let data = data_with_leftovers(&dir);
    let before = snapshot(&data);
    for state in [
        "loaded/active",
        "loaded/activating",
        "loaded/reloading",
        "unavailable",
    ] {
        let mut envs = staging.envs();
        envs.retain(|(k, _)| *k != "GEOFRAME_TEST_PYTHON_STATE");
        envs.push(("GEOFRAME_TEST_PYTHON_STATE", state.into()));
        envs.push(("GEOFRAME_INGEST", "1".into()));
        let out = run_owned(&["sync", "--data-dir", data.to_str().unwrap()], &envs);
        assert_eq!(out.status.code(), Some(2), "{state}: {}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("geoframe-server.service"),
            "{state}: {}",
            text(&out.stderr)
        );
        assert_untouched_after_refusal(&data, &before);
    }
}

#[test]
fn dry_run_and_read_only_serve_need_neither_the_marker_nor_a_stopped_python() {
    // この機体（本番）では印が無く Python が動いている。それでも書かないコマンドは動く
    let dir = Dir::new("guard-readonly");
    let data = data_with_leftovers(&dir);
    let dry = run_owned(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_TEST_PYTHON_STATE", "loaded/active".into())],
    );
    assert!(dry.status.success(), "{}", text(&dry.stderr));
    let port = free_port();
    let mut child = serve(&data, port, None);
    wait_healthy(&mut child, port);
    let (status, _) = terminate(child);
    assert!(status.success());
}
