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
            // 鍵は無い（置き場の外の、存在しないパス）。Drive の段は通信せずに飛ばす
            (
                "GEOFRAME_DRIVE_KEY",
                self.marker
                    .with_file_name("no_such_drive_key.json")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ),
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
        // 位置のある行は `exif`（`infer` が取り消さない）。(1, 2) は国外なので、`enrich` は通信せずに済む
        "INSERT INTO photos(source,drive_id,name,display,lat,lon,loc_source,place) VALUES
           ('drive','a','n1','derivatives/a.jpg',1.0,2.0,'exif','p'),
           ('drive','b','n2',NULL,1.0,2.0,'exif',NULL),
           ('drive','c','n3',NULL,NULL,NULL,NULL,NULL)",
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
    // SIGKILL で残った書きかけ（消す対象に数える）と、対象外のもの
    fs::write(data.join("derivatives/x.jpg.1.00ff.part"), b"p").unwrap();
    fs::write(data.join("derivatives/x.jpg"), b"d").unwrap();
    fs::write(data.join(".cache.json"), r#"{"exif:a": 1, "geo:1": 2}"#).unwrap();
    let before = snapshot(data);

    // 鍵が無い（置き場の中の、存在しないパス）ので、Drive の 2 つの段は通信せずに飛ばす
    let missing_key = data.join("no_such_drive_key.json");
    let out = run(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_DRIVE_KEY", missing_key.to_str().unwrap())],
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
        stdout.contains(
            "消す .dl_* 2 件 / 消す書きかけの .part 1 件 / 捨てる .cache.json の旧キー 1 件"
        ),
        "{stdout}"
    );
    // 写真の置き場が無い（dry-run は作らない）ので、ローカル写真の段は 0 件
    assert!(
        stdout.contains("段 index_local: 完了 files=0 added=0 updated=0 removed=0 "),
        "{stdout}"
    );
    assert!(
        stdout.contains("段 local_derivatives: 完了 original=0 made=0 failed=0 icc_dropped=0"),
        "{stdout}"
    );
    for stage in ["index_drive", "fetch_derivatives"] {
        assert!(
            stdout.contains(&format!(
                "段 {stage}: 省略（Drive の鍵が無いので飛ばします）"
            )),
            "{stage}\n{stdout}"
        );
    }
    // 錨が無い（撮影時刻のある位置つきの写真が無い）ので、推定は何もしない。地名は、国外の 1 件を
    // 通信なしで空で確定するはず（dry-run は何も書かない）
    assert!(
        stdout.contains(
            "段 infer: 完了 anchors=0 targets=1 inferred=0 updated=0 unchanged=0 cancelled=0 untimed=1"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "段 enrich: 完了 candidates=1 foreign=1 cache_hit=0 would_query=0 unresolved=0 skipped_backoff=0 skipped_blocked=0 unusable=0 pending=1"
        ),
        "{stdout}"
    );
    // 表示用の画像がある 1 枚が未解析（解析は dry-run では実行しない）。解析すれば再構築になる
    assert!(
        stdout.contains(
            "段 quality: 完了 orphans=0 pending=1 would_analyze=1 remaining=0 would_rebuild=1"
        ),
        "{stdout}"
    );
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
    // 写真が無いので解析も再構築も無い
    assert!(
        stdout.contains(
            "段 quality: 完了 removed=0 pending=0 analyzed=0 failed=0 remaining=0 rebuilt=0 rows=0"
        ),
        "{stdout}"
    );
    // 鍵が無いので Drive の 2 つの段は飛ばす（通信しない。失敗ではないので終了コードは 0）
    for stage in ["index_drive", "fetch_derivatives"] {
        assert!(
            stdout.contains(&format!(
                "段 {stage}: 省略（Drive の鍵が無いので飛ばします）"
            )),
            "{stage}\n{stdout}"
        );
    }
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
    // 国外の 1 件は `enrich` が空で確定した（NULL でなくなる）。位置の無い行だけが残る。
    // 移行が再 enrich の印（`place=NULL`）を増やしていないことは、スキーマの確認（上）で見ている
    assert_eq!(null_places, 1);
    // `quality` の段（T2.6）は、`display` があるのにファイルの無い行 `a` を解析して、失敗の行を 1 つ書く
    // （`photos` の行は変えない）。次のサイクルから再解析しない。グループは無いので `photo_quality` は空
    let failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM photo_quality_features WHERE error!='' AND content_sha256 IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let grouped: i64 = conn
        .query_row("SELECT COUNT(*) FROM photo_quality", [], |r| r.get(0))
        .unwrap();
    assert_eq!((failed, grouped), (1, 0));
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

// ---- organize（T2.3）-----------------------------------------------------------

/// EXIF の無い合成 JPEG（`tests/fixtures/exif-python.json` の `no_exif`）。
fn plain_jpeg() -> Vec<u8> {
    use base64::Engine;
    let doc: Value = serde_json::from_str(include_str!("fixtures/exif-python.json")).unwrap();
    let case = doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "no_exif")
        .unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(case["jpeg_base64"].as_str().unwrap())
        .unwrap()
}

fn data_with_a_photo(dir: &Dir) -> PathBuf {
    let data = dir.0.join("data");
    fs::create_dir_all(data.join("photos")).unwrap();
    production_shaped_db(&data);
    fs::write(data.join("photos/no_gps.jpg"), plain_jpeg()).unwrap();
    data
}

#[test]
fn organize_without_ingest_or_marker_refuses_and_moves_nothing() {
    let dir = Dir::new("organize-refused");
    let data = data_with_a_photo(&dir);
    let before = snapshot(&data);
    // GEOFRAME_INGEST が無い
    let out = run(&["organize", "--data-dir", data.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(snapshot(&data), before);
    // 印が無い（Python が止まっていても断る）
    let missing = dir.0.join("no-such-marker");
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        (
            "GEOFRAME_TEST_STAGING_MARKER",
            missing.to_str().unwrap().to_string(),
        ),
        ("GEOFRAME_TEST_PYTHON_STATE", "not-found/inactive".into()),
    ];
    let out = run_owned(&["organize", "--data-dir", data.to_str().unwrap()], &envs);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_untouched_after_refusal(&data, &before);
    assert!(data.join("photos/no_gps.jpg").exists());
}

#[test]
fn organize_dry_run_reports_counts_and_changes_nothing() {
    let dir = Dir::new("organize-dry");
    let data = data_with_a_photo(&dir);
    let before = snapshot(&data);
    let out = run(
        &[
            "organize",
            "--dry-run",
            "--data-dir",
            data.to_str().unwrap(),
        ],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains("index_local: 完了 files=1 added=1"),
        "{stdout}"
    );
    // 計画は索引の結果を反映する（実 DB でなくメモリ上の写しに適用する）。位置の無い 1 枚が対象
    assert!(
        stdout.contains("organize: 完了 targets=1 moved=1"),
        "{stdout}"
    );
    assert_eq!(snapshot(&data), before);
    assert!(!data.join("_excluded").exists());
}

#[test]
fn organize_moves_photos_without_a_location_when_the_guard_passes() {
    let dir = Dir::new("organize-apply");
    let staging = Staging::new("organize-apply");
    let data = data_with_a_photo(&dir);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&["organize", "--data-dir", data.to_str().unwrap()], &envs);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains("organize: 完了 targets=1 moved=1"),
        "{stdout}"
    );
    assert!(!data.join("photos/no_gps.jpg").exists());
    assert!(data.join("_excluded/no_gps/no_gps.jpg").is_file());
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let local: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM photos WHERE source='local'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let drive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM photos WHERE source='drive'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((local, drive), (0, 3)); // Drive の行は触らない
    // ログ・出力に写真の名前を出さない
    assert!(!stdout.contains("no_gps"), "{stdout}");
    assert!(!text(&out.stderr).contains("no_gps"));
}

// ---- quality-scan（T2.6）-------------------------------------------------------

/// 合成の JPEG（単色。EXIF なし）。
fn solid_jpeg(value: u8) -> Vec<u8> {
    let mut out = Vec::new();
    jpeg_encoder_encode(&mut out, value);
    out
}

fn jpeg_encoder_encode(out: &mut Vec<u8>, value: u8) {
    // `plain_jpeg()` は固定の 1 枚。値を変えた複数の画像が要るので、PNG ではなく JPEG を自前で作る
    let mut data = Vec::new();
    for y in 0..48u32 {
        for x in 0..64u32 {
            let v = value.wrapping_add((x * 3 + y) as u8);
            data.extend_from_slice(&[v, v.wrapping_mul(2), 255 - v]);
        }
    }
    jpeg_encoder::Encoder::new(out, 85)
        .encode(&data, 64, 48, jpeg_encoder::ColorType::Rgb)
        .unwrap();
}

fn data_with_derivatives(dir: &Dir) -> PathBuf {
    let data = dir.0.join("data");
    fs::create_dir_all(data.join("derivatives")).unwrap();
    production_shaped_db(&data);
    // 行 1 の display は derivatives/a.jpg。同じバイト列の b.jpg を行 3 に付ける（完全重複）
    let a = solid_jpeg(10);
    fs::write(data.join("derivatives/a.jpg"), &a).unwrap();
    fs::write(data.join("derivatives/b.jpg"), &a).unwrap();
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    conn.execute_batch(
        "UPDATE photos SET display='derivatives/b.jpg', taken_at='2024-05-06 10:00' WHERE drive_id='c';
         UPDATE photos SET taken_at='2024-05-06 10:00' WHERE drive_id='a'",
    )
    .unwrap();
    data
}

#[test]
fn quality_scan_without_ingest_or_marker_refuses_and_changes_nothing() {
    let dir = Dir::new("qscan-refused");
    let data = data_with_derivatives(&dir);
    let before = snapshot(&data);
    let out = run(&["quality-scan", "--data-dir", data.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(snapshot(&data), before);
    let missing = dir.0.join("no-such-marker");
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        (
            "GEOFRAME_TEST_STAGING_MARKER",
            missing.to_str().unwrap().to_string(),
        ),
        ("GEOFRAME_TEST_PYTHON_STATE", "not-found/inactive".into()),
    ];
    let out = run_owned(
        &["quality-scan", "--data-dir", data.to_str().unwrap()],
        &envs,
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_untouched_after_refusal(&data, &before);
}

#[test]
fn quality_scan_dry_run_counts_without_analyzing_or_writing() {
    let dir = Dir::new("qscan-dry");
    let data = data_with_derivatives(&dir);
    let before = snapshot(&data);
    let out = run(
        &[
            "quality-scan",
            "--dry-run",
            "--data-dir",
            data.to_str().unwrap(),
        ],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains(
            "quality-scan: 完了 orphans=0 pending=2 would_analyze=2 remaining=0 would_rebuild=1"
        ),
        "{stdout}"
    );
    assert_eq!(snapshot(&data), before);
}

#[test]
fn quality_scan_analyzes_everything_and_rebuilds_when_the_guard_passes() {
    let dir = Dir::new("qscan-apply");
    let staging = Staging::new("qscan-apply");
    let data = data_with_derivatives(&dir);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    envs.push(("GEOFRAME_QUALITY_PER_CYCLE", "1".into())); // 1 件ずつ → 2 回に分かれても全部解析する
    let out = run_owned(
        &["quality-scan", "--data-dir", data.to_str().unwrap()],
        &envs,
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains(
            "quality-scan: 完了 removed=0 analyzed=2 failed=0 remaining=0 rebuilt=1 rows=2"
        ),
        "{stdout}"
    );
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let rows: Vec<(i64, String, i64)> = conn
        .prepare("SELECT photo_id, group_kind, recommended FROM photo_quality ORDER BY photo_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        rows,
        [(1, "exact".to_string(), 1), (3, "exact".to_string(), 0)]
    );
    let dirty: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key='photo_quality_dirty'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dirty, "0");
    // 出力にファイル名を出さない
    assert!(!stdout.contains("a.jpg") && !text(&out.stderr).contains("a.jpg"));
}
