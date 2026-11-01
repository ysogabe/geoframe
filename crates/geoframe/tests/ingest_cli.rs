//! 取り込み（T2.1）をバイナリごと確かめる。合成の DB・合成の `.cache.json` だけを使う。
//!
//! - `sync --dry-run` は何も書かない（DB・キャッシュ・`tmp/`・ロックのファイル）。
//! - `GEOFRAME_INGEST` が無いと、`sync` も `serve` も書かない（既定は無効）。
//! - `GEOFRAME_INGEST=1` なら、DB が無くても作って 1 サイクル回し、`serve` はそのまま常駐する。
//! - 書き込みの前の守り（`guard.rs`）: root での実行は、書き込みの前に終了コード 2 で断り、置き場を
//!   一切変えない。書き込み系の CLI は、DATA_DIR に `catalog.db` が無ければ作らずに終了コード 2。
//!
//! 取り込みを有効にして動かすテストは、デバッグビルドだけにある差し替え（`GEOFRAME_TEST_EUID`）で
//! 実効 UID を与える。root で `cargo test` を回すこともありうるので、差し替えなしでは結果が機体に左右される。
//! 実行するバイナリには、差し替えなしで書き込みの経路に入る呼び出しを残さない。

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

/// 取り込みを動かすテストに渡す環境変数。置き場の外の別ディレクトリに、存在しない鍵のパスを置く。
struct IngestEnv {
    _dir: Dir,
    no_key: PathBuf,
}

impl IngestEnv {
    fn new(tag: &str) -> Self {
        let dir = Dir::new(&format!("{tag}-ingest-env"));
        let no_key = dir.0.join("no_such_drive_key.json");
        Self { _dir: dir, no_key }
    }

    /// root ではない（実効 UID 1000）環境。
    fn envs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("GEOFRAME_TEST_EUID", "1000".into()),
            // 鍵は無い（置き場の外の、存在しないパス）。Drive の段は通信せずに飛ばす
            (
                "GEOFRAME_DRIVE_KEY",
                self.no_key.to_str().unwrap().to_string(),
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
        .env("GEOFRAME_TEST_EUID", "1000")
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

fn run_owned(args: &[&str], envs: &[(&str, String)]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .env("GEOFRAME_TEST_EUID", "1000")
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
    let local_plan = run(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[
            ("GEOFRAME_INGEST", "1"),
            ("GEOFRAME_DRIVE_KEY", missing_key.to_str().unwrap()),
        ],
    );
    assert!(local_plan.status.success());
    assert!(text(&local_plan.stdout).contains("スキーマ: 作る表・索引 14 / 足す列 0"));
    assert_eq!(snapshot(data), before);

    assert!(
        stdout.contains(
            "スキーマ: 作る表・索引 11 / 足す列 0 / place を NULL に戻す なし / embedding の写し 0"
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
fn an_invalid_blog_flag_is_refused_as_a_config_error_before_anything_is_written() {
    let dir = Dir::new("badblog");
    production_shaped_db(&dir.0);
    let before = snapshot(&dir.0);
    for value in ["2", "true"] {
        let out = run(
            &[
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
                dir.0.to_str().unwrap(),
            ],
            &[("GEOFRAME_BLOG", value)],
        );
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        let stderr = text(&out.stderr);
        assert!(stderr.contains("GEOFRAME_BLOG"), "{stderr}");
        if value.len() > 1 {
            assert!(!stderr.contains(value), "{stderr}");
        }
    }
    assert_eq!(snapshot(&dir.0), before);
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
fn sync_with_ingest_creates_the_schema_in_an_empty_database_file_and_runs_one_cycle() {
    let dir = Dir::new("ingest");
    let staging = IngestEnv::new("ingest");
    let data = dir.0.join("new-data");
    // 書き込み系の CLI は DB を作らない。空のファイル（SQLite は空のファイルを空の DB として開く）なら、
    // スキーマを作って 1 サイクル回す
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("catalog.db"), b"").unwrap();
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
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Python と同じ 11 表と、P4 の AI 表 `ai_jobs`・`ai_request_usage`・`ai_provider_pauses`・`ai_provider_waits`・`ai_worker_runs`・`ai_embedding_spaces`・`ai_embedding_vectors`、選別の表 `photo_curation`
    assert_eq!(tables, 23);
}

#[test]
fn sync_with_ingest_on_a_production_shaped_database_changes_no_rows() {
    let dir = Dir::new("ingest-prod");
    let staging = IngestEnv::new("ingest-prod");
    production_shaped_db(&dir.0);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&["sync", "--data-dir", dir.0.to_str().unwrap()], &envs);
    assert!(out.status.success(), "{}", text(&out.stdout));
    // 本番相当の DB に足されるのは P4 の AI 表（`ai_jobs`・索引・送信予算・Provider 停止・共有の一時待機・起動 ID・空間とベクトルの表）と選別の表 `photo_curation` だけ。ほかの表・列は変えない
    assert!(
        text(&out.stdout).contains("スキーマを更新しました: 作成 14件 / 列の追加 0件"),
        "{}",
        text(&out.stdout)
    );
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
    get(port, "/healthz")
}

fn get(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

fn serve(data: &Path, port: u16, ingest: Option<(&str, &IngestEnv)>) -> Child {
    serve_with_env(data, port, ingest, &[])
}

/// `extra` は追加の環境変数（AI の設定など）。
fn serve_with_env(
    data: &Path,
    port: u16,
    ingest: Option<(&str, &IngestEnv)>,
    extra: &[(&str, &str)],
) -> Child {
    let mut command = Command::new(BIN);
    command
        .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
        .args(["--data-dir", data.to_str().unwrap()])
        .env_clear()
        .env("GEOFRAME_TEST_EUID", "1000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((value, staging)) = ingest {
        command.env("GEOFRAME_INGEST", value).envs(staging.envs());
    }
    command.envs(extra.iter().copied());
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

/// `INGEST=0` では AI の回収・表の作成・Provider 停止の整理を一切しない（旧 DB 全行不変）。AI の設定が有効で、
/// `running` のジョブと今の設定に当たらない停止が残っていても、そのまま。
#[test]
fn serve_without_ingest_never_reclaims_creates_or_releases_anything_for_ai() {
    let dir = Dir::new("serve-ro-ai");
    let data = data_with_ai_rows(&dir);
    {
        let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
        conn.execute_batch(
            "UPDATE ai_jobs SET state='running', run_id='old-run', claim_token='old-token'
             WHERE kind='location_clue'",
        )
        .unwrap();
    }
    let ai = dir.0.join("ai.json");
    fs::write(
        &ai,
        r#"{"version":1,"local_base_url":"http://192.0.2.10:8100",
            "tasks":{"location_clue":{"provider":"local","model":"example-decision"}}}"#,
    )
    .unwrap();
    let before = snapshot(&data);
    let port = free_port();
    let mut child = serve_with_env(
        &data,
        port,
        None,
        &[("GEOFRAME_AI_CONFIG", ai.to_str().unwrap())],
    );
    wait_healthy(&mut child, port);
    let (status, stdout) = terminate(child);
    assert!(status.success(), "{stdout}");
    assert!(!stdout.contains("背景ワーカー"), "{stdout}");
    assert_eq!(snapshot(&data), before, "INGEST=0 は DB の全行を変えない");
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let running: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_jobs WHERE state='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let pauses: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_provider_pauses", [], |r| r.get(0))
        .unwrap();
    let runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='ai_worker_runs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((running, pauses), (1, 1));
    // data_with_ai_rows が足した表の一覧には ai_worker_runs もある（migrate の結果）。serve は増減させない
    assert_eq!(runs, 1);
    assert!(!data.join(".ingest.lock").exists());
}

/// 取り込みが無効（既定）でも、地図タイルのキャッシュは掃除する（B4。同期サイクルは動かないので、サーバー側の
/// 独立のタイマー）。起動時に 1 回走り、停止の合図で正常に終わる。DB には触れない。
#[test]
fn serve_without_ingest_still_prunes_the_tile_cache_at_start_and_stops_cleanly() {
    let dir = Dir::new("serve-tile-prune");
    production_shaped_db(&dir.0);
    let db_before = fs::read(dir.0.join("catalog.db")).unwrap();
    let tiles = dir.0.join("tile_cache/osm/5/3");
    fs::create_dir_all(&tiles).unwrap();
    for (y, secs) in [(1u64, 100u64), (2, 200), (3, 300)] {
        let file = fs::File::create(tiles.join(format!("{y}.png"))).unwrap();
        file.set_len(600_000).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }
    // 上限は 1 MB（1,048,576 バイト）。合計 1,800,000 バイトは超えているので、古い順に 90% 以下まで消す
    let port = free_port();
    let mut child = Command::new(BIN)
        .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
        .args(["--data-dir", dir.0.to_str().unwrap()])
        .env_clear()
        .env("GEOFRAME_TEST_EUID", "1000")
        .env("GEOFRAME_TILE_CACHE_MAX_MB", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_healthy(&mut child, port);
    let deadline = Instant::now() + Duration::from_secs(15);
    while fs::read_dir(&tiles).unwrap().count() > 1 {
        assert!(Instant::now() < deadline, "起動時の掃除が走りません");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, stdout) = terminate(child);
    assert!(status.success(), "{stdout}");
    assert!(stdout.contains("取り込みは無効です"), "{stdout}");
    assert!(stdout.contains("キャッシュ整理: 2枚削除"), "{stdout}");
    // 新しい 1 枚だけが残る。座標（z/x/y）はログに出ない
    assert!(tiles.join("3.png").is_file());
    assert!(!stdout.contains("osm/5/3"), "{stdout}");
    assert_eq!(fs::read(dir.0.join("catalog.db")).unwrap(), db_before);
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
    let staging = IngestEnv::new("serve-ingest");
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

/// 目印（ログに出てはいけない文字列）を入れた、不正な AI 設定ファイルを置く。
const AI_SENTINEL_KEY: &str = "sentinel-key-4c1f";
const AI_SENTINEL_VALUE: &str = "sentinel-value-93ab";

fn broken_ai_config(dir: &Dir, body: &str) -> PathBuf {
    let path = dir.0.join("sentinel-ai-path-5e72.json");
    fs::write(&path, body).unwrap();
    path
}

/// AI の設定が不正でも（JSON・旧環境変数の数値のどちらでも）、写真の配信は動く: 起動して `/healthz` と
/// フィード（`/api/v1/photos`）に答え、ログには種類のコードだけが出る（パス・中身・値は出ない）。
/// 取り込みが無効なので DB にも書かない。
#[test]
fn serve_with_a_broken_ai_config_still_serves_health_and_the_feed() {
    type LegacyEnv = Option<(&'static str, &'static str)>;
    let cases: [(&str, &str, LegacyEnv); 5] = [
        // (本文, 期待するコード, 追加の旧環境変数)
        (
            r#"{"version":1,"sentinel-key-4c1f":"sentinel-value-93ab"}"#,
            "unknown_key",
            None,
        ),
        (
            r#"{"version":1,"version":1,"x":"sentinel-value-93ab"}"#,
            "duplicate_key",
            None,
        ),
        (
            r#"{"version":1 "sentinel-key-4c1f":"sentinel-value-93ab""#,
            "syntax",
            None,
        ),
        // JSON は正しくても、旧設定の数値が不正なら AI だけ無効
        (
            r#"{"version":1}"#,
            "legacy_env",
            Some(("GEOFRAME_AI_POLL_S", "sentinel-value-93ab")),
        ),
        (
            r#"{"version":1}"#,
            "legacy_env",
            Some(("GEOFRAME_AI_VISUAL_SIMILARITY", AI_SENTINEL_VALUE)),
        ),
    ];
    for (n, (body, code, legacy)) in cases.into_iter().enumerate() {
        let dir = Dir::new(&format!("serve-ai-broken-{n}"));
        production_shaped_db(&dir.0);
        let config = broken_ai_config(&dir, body);
        let before = snapshot(&dir.0);
        let mut extra = vec![("GEOFRAME_AI_CONFIG", config.to_str().unwrap())];
        extra.extend(legacy);
        if legacy.is_some_and(|(key, _)| key == "GEOFRAME_AI_VISUAL_SIMILARITY") {
            extra.push(("GEOFRAME_AI_ENABLED", "0"));
        }
        let port = free_port();
        let mut child = serve_with_env(&dir.0, port, None, &extra);
        wait_healthy(&mut child, port);
        let feed = get(port, "/api/v1/photos").expect("フィードに答えない");
        assert!(feed.starts_with("HTTP/1.1 200"), "{code}: {feed}");
        assert!(
            feed.contains("\"file\":\"derivatives/a.jpg\""),
            "{code}: {feed}"
        );
        let (status, stdout) = terminate(child);
        assert!(status.success(), "{stdout}");
        assert!(
            stdout.contains(&format!("ai=invalid:{code}")),
            "{code}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("AI の設定が不正です（種類: {code}）")),
            "{code}: {stdout}"
        );
        for secret in [AI_SENTINEL_KEY, AI_SENTINEL_VALUE, "sentinel-ai-path-5e72"] {
            assert!(!stdout.contains(secret), "{code}: {stdout}");
        }
        assert_eq!(snapshot(&dir.0), before, "{code}");
    }
}

/// 取り込みが有効なときも、不正な AI 設定で起動は止まらない。追加の AI 表（送信予算・Provider 停止）は
/// 取り込みの起動で足され、2 回目の起動では何も作らない。有効な設定は起動時にネットワーク・認証ファイル
/// （ここでは存在しないパス）に触れず、予約アドレスの入口・クラウド許可があっても起動する。
#[test]
fn serve_with_ingest_adds_the_ai_tables_once_and_never_needs_the_network_or_credentials() {
    let dir = Dir::new("serve-ai-ingest");
    let data = dir.0.join("data");
    let staging = IngestEnv::new("serve-ai-ingest");
    let valid = dir.0.join("ai-valid.json");
    fs::write(
        &valid,
        format!(
            r#"{{"version":1,"local_base_url":"http://192.0.2.10:8100","tasks":{{
              "location_clue":{{"provider":"local","model":"example-decision"}},
              "photo_judge":{{"provider":"cloud","model":"example-judge","allow_cloud":true,
                "daily_request_limit":10,"credential_path":"{}/no-such-credentials.json",
                "project":"example-project","region":"example-region1"}}}}}}"#,
            dir.0.display()
        ),
    )
    .unwrap();
    let broken = broken_ai_config(&dir, r#"{"version":"sentinel-value-93ab"}"#);

    for (round, (config, expected)) in [
        (&valid, "ai=ready:tasks=2"),
        (&broken, "ai=invalid:bad_value"),
    ]
    .into_iter()
    .enumerate()
    {
        let port = free_port();
        let mut child = serve_with_env(
            &data,
            port,
            Some(("1", &staging)),
            &[("GEOFRAME_AI_CONFIG", config.to_str().unwrap())],
        );
        wait_healthy(&mut child, port);
        let (status, stdout) = terminate(child);
        assert!(status.success(), "{stdout}");
        assert!(stdout.contains(expected), "{stdout}");
        if round == 0 {
            assert!(stdout.contains("[db] スキーマを更新しました"), "{stdout}");
        } else {
            // 2 回目: AI の表は作らない（何も更新しない）
            assert!(stdout.contains("[db] スキーマは最新です"), "{stdout}");
        }
        for secret in [
            AI_SENTINEL_VALUE,
            "no-such-credentials",
            "192.0.2.10",
            "example-judge",
            "example-project",
            "ai-valid.json",
        ] {
            assert!(!stdout.contains(secret), "{stdout}");
        }
    }
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    for table in [
        "ai_jobs",
        "ai_request_usage",
        "ai_provider_pauses",
        "ai_provider_waits",
        "ai_worker_runs",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table}");
    }
}

#[test]
fn a_second_ingest_process_on_the_same_data_dir_is_refused() {
    let dir = Dir::new("serve-lock");
    let data = dir.0.join("d");
    let port = free_port();
    let staging = IngestEnv::new("serve-lock");
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

/// AI の表（空）と、`failed` のジョブ・Provider 停止の行を持つ本番相当の DB（`geoframe::ai::migrate` で足す）。
fn data_with_ai_rows(dir: &Dir) -> PathBuf {
    let data = dir.0.join("ai-data");
    fs::create_dir_all(&data).unwrap();
    production_shaped_db(&data);
    let mut conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    geoframe::ai::migrate(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,attempts,terminal,
                             last_error_code,created_at,updated_at) VALUES
           ('location_clue','SECRET-T1','SECRET-H1','SECRET-P','failed',5,1,'bad_response',1,1),
           ('photo_judge','SECRET-T2','SECRET-H2','SECRET-P','failed',5,1,'interrupted',1,1),
           ('image_embedding','SECRET-T3','SECRET-H3','SECRET-P','succeeded',0,0,'',1,1);
         INSERT INTO ai_provider_pauses(scope,subject,task,reason,created_at)
           VALUES ('provider','SECRET-SUBJECT','image_embedding','unauthorized',1)",
    )
    .unwrap();
    data
}

fn ai_job_states(data: &Path) -> Vec<(String, String, i64)> {
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT kind, state, attempts FROM ai_jobs ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn ai_jobs_status_is_read_only_prints_only_counts_and_works_on_a_database_without_ai_tables() {
    let dir = Dir::new("aijobs-status-old");
    production_shaped_db(&dir.0);
    let before = snapshot(&dir.0);
    // INGEST は要らない（読み取りだけ）
    let out = run(
        &["ai-jobs", "status", "--data-dir", dir.0.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(stdout.contains("AI 設定: 未指定"), "{stdout}");
    assert!(stdout.contains("ジョブ: 0 件"), "{stdout}");
    assert!(stdout.contains("停止中の Provider・用途: なし"), "{stdout}");
    assert_eq!(snapshot(&dir.0), before);
    assert!(!dir.0.join(".ingest.lock").exists() && !dir.0.join("tmp").exists());
    let conn = rusqlite::Connection::open(dir.0.join("catalog.db")).unwrap();
    let ai_tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'ai\\_%' ESCAPE '\\'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ai_tables, 0, "読み取りは AI の表を作らない");

    // 行がある DB: 件数・固定の理由・用途だけ
    let dir = Dir::new("aijobs-status-rows");
    let data = data_with_ai_rows(&dir);
    let before = snapshot(&data);
    let out = run(
        &["ai-jobs", "status", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("ジョブ location_clue failed: 1 件"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ジョブ image_embedding succeeded: 1 件"),
        "{stdout}"
    );
    assert!(stdout.contains("理由 failed interrupted: 1 件"), "{stdout}");
    assert!(
        stdout.contains("停止中 provider image_embedding: unauthorized"),
        "{stdout}"
    );
    assert!(!stdout.contains("SECRET"), "{stdout}");
    assert_eq!(snapshot(&data), before);
}

#[test]
fn ai_jobs_writes_need_ingest_the_guard_and_a_stopped_service() {
    let dir = Dir::new("aijobs-guard");
    let data = data_with_ai_rows(&dir);
    let before = snapshot(&data);
    let arg = data.to_str().unwrap();
    // INGEST が無い → 2
    for args in [
        &["ai-jobs", "retry", "--all", "--data-dir", arg][..],
        &["ai-jobs", "release-pause", "--all", "--data-dir", arg],
    ] {
        let out = run(args, &[]);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
        assert!(text(&out.stderr).contains("GEOFRAME_INGEST=1"));
        let out = run(args, &[("GEOFRAME_INGEST", "0")]);
        assert_eq!(out.status.code(), Some(2));
    }
    // INGEST=1 でも、root なら書く前に断る → 2
    let out = run(
        &["ai-jobs", "retry", "--all", "--data-dir", arg],
        &[("GEOFRAME_INGEST", "1"), ("GEOFRAME_TEST_EUID", "0")],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_untouched_after_refusal(&data, &before);

    // 稼働中の serve（ロックを持つ）があるときは、ガードを通っても断る → 1。ジョブも停止も変わらない
    let staging = IngestEnv::new("aijobs-guard");
    let port = free_port();
    let mut child = serve(&data, port, Some(("1", &staging)));
    wait_healthy(&mut child, port);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    for args in [
        &["ai-jobs", "retry", "--all", "--data-dir", arg][..],
        &["ai-jobs", "release-pause", "--all", "--data-dir", arg],
    ] {
        let out = run_owned(args, &envs);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("サービスを止めてから"),
            "{}",
            text(&out.stderr)
        );
    }
    // 稼働中でも status は読める
    let status = run(&["ai-jobs", "status", "--data-dir", arg], &[]);
    assert!(status.status.success());
    let (stopped, _) = terminate(child);
    assert!(stopped.success());
    // serve が起動時に足した行はなく、ジョブは failed のまま
    assert_eq!(
        ai_job_states(&data),
        [
            ("location_clue".to_string(), "failed".to_string(), 5),
            ("photo_judge".to_string(), "failed".to_string(), 5),
            ("image_embedding".to_string(), "succeeded".to_string(), 0),
        ]
    );
}

#[test]
fn ai_jobs_retry_keeps_what_it_cannot_check_and_release_pause_removes_the_stops() {
    let dir = Dir::new("aijobs-write");
    let data = data_with_ai_rows(&dir);
    let staging = IngestEnv::new("aijobs-write");
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let arg = data.to_str().unwrap();

    // 用途別の処理はまだ登録されていない（配布バイナリは空）ので、failed は検査できず保持。成功済みも触れない
    let out = run_owned(&["ai-jobs", "retry", "--all", "--data-dir", arg], &envs);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains(
            "再実行待ちに戻した 0 / 無効にした 0 / 成功済みとした 0 / 検査できず保持（処理が未登録）2 / 検査できず保持（設定で使えない）0"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("SECRET"), "{stdout}");
    let states = ai_job_states(&data);
    assert_eq!(states.iter().filter(|(_, s, _)| s == "failed").count(), 2);
    assert!(
        states
            .iter()
            .all(|(_, _, attempts)| *attempts == 0 || *attempts == 5)
    );

    // 設定が未指定の用途指定の解除は、停止を特定できないので何も消さない → 2。--all だけが消す
    let out = run_owned(
        &[
            "ai-jobs",
            "release-pause",
            "--task",
            "image_embedding",
            "--data-dir",
            arg,
        ],
        &envs,
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let pauses = |data: &Path| -> i64 {
        rusqlite::Connection::open(data.join("catalog.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ai_provider_pauses", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(pauses(&data), 1);
    let out = run_owned(
        &["ai-jobs", "release-pause", "--all", "--data-dir", arg],
        &envs,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("停止を解除した: 1 件"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(pauses(&data), 0);
}

#[test]
fn serve_with_ingest_and_a_valid_ai_config_records_its_run_and_the_stop_and_exits_cleanly() {
    let dir = Dir::new("serve-ai-run");
    let data = dir.0.join("d");
    let staging = IngestEnv::new("serve-ai-run");
    let ai = dir.0.join("ai.json");
    fs::write(
        &ai,
        r#"{"version":1,"local_base_url":"http://192.0.2.10:8100",
            "tasks":{"location_clue":{"provider":"local","model":"example-decision"}}}"#,
    )
    .unwrap();
    let port = free_port();
    let mut child = serve_with_env(
        &data,
        port,
        Some(("1", &staging)),
        &[("GEOFRAME_AI_CONFIG", ai.to_str().unwrap())],
    );
    wait_healthy(&mut child, port);
    let began = Instant::now();
    let (status, stdout) = terminate(child);
    assert!(status.success(), "{stdout}");
    assert!(began.elapsed() < Duration::from_secs(10), "停止が遅い");
    // 有効な背景用途があるので背景ワーカーが起動し、起動 ID の行と停止開始が残っている（配布バイナリの handler は空）
    assert!(stdout.contains("[ai] 背景ワーカーを起動します"), "{stdout}");
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let rows: Vec<(i64, Option<i64>)> = conn
        .prepare("SELECT started_at, stop_requested_at FROM ai_worker_runs")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].1.is_some(), "SIGTERM の直後の停止開始が記録される");
    for secret in ["192.0.2.10", "example-decision", "ai.json"] {
        assert!(!stdout.contains(secret), "{secret}\n{stdout}");
    }

    // AI の設定ファイルが無くても、保存済み vector だけで動く同一場面の局所計算のために、ワーカーは起動する
    let dir = Dir::new("serve-ai-off");
    let data = dir.0.join("d");
    let port = free_port();
    let mut child = serve(&data, port, Some(("1", &staging)));
    wait_healthy(&mut child, port);
    let (status, stdout) = terminate(child);
    assert!(status.success());
    assert!(stdout.contains("背景ワーカーを起動します"), "{stdout}");
    let dir = Dir::new("serve-ai-disabled");
    let data = dir.0.join("d");
    let port = free_port();
    let mut child = serve_with_env(
        &data,
        port,
        Some(("1", &staging)),
        &[("GEOFRAME_AI_ENABLED", "0")],
    );
    wait_healthy(&mut child, port);
    let (status, stdout) = terminate(child);
    assert!(status.success());
    assert!(!stdout.contains("背景ワーカーを起動します"));
    // AI を無効にしても、起動時の回収（起動 ID の行）は行う。ワーカー（局所計算を含む）は起動しない
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ai_worker_runs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'ai_scene_%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
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

/// 書き込み系の CLI（`ingest::start_existing` を通るもの）の引数。`data` は DATA_DIR。
fn write_commands(data: &Path) -> Vec<Vec<String>> {
    let d = data.to_str().unwrap().to_string();
    let with = |args: &[&str]| -> Vec<String> {
        args.iter()
            .map(|a| a.to_string())
            .chain(["--data-dir".to_string(), d.clone()])
            .collect()
    };
    vec![
        with(&["sync"]),
        with(&["organize"]),
        with(&["quality-scan"]),
        with(&["ai-jobs", "retry", "--all"]),
        with(&["ai-jobs", "release-pause", "--all"]),
        with(&["ai-jobs", "deactivate-space"]),
    ]
}

fn run_args(args: &[String], envs: &[(&str, String)]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_owned(&args, envs)
}

fn serve_once(data: &Path, envs: &[(&str, String)]) -> Output {
    run_owned(
        &[
            "serve",
            "--listen",
            &format!("127.0.0.1:{}", free_port()),
            "--data-dir",
            data.to_str().unwrap(),
        ],
        envs,
    )
}

#[test]
fn root_is_refused_before_any_write_by_serve_and_every_write_command() {
    let dir = Dir::new("guard-root");
    let data = data_with_leftovers(&dir);
    let before = snapshot(&data);
    // INGEST=1 でも、root なら断る
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        ("GEOFRAME_TEST_EUID", "0".to_string()),
    ];
    for args in write_commands(&data) {
        let out = run_args(&args, &envs);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(text(&out.stderr).contains("root"), "{}", text(&out.stderr));
        assert_untouched_after_refusal(&data, &before);
    }
    // serve は、取り込みが有効でも無効（読み取り専用）でも、root では起動しない
    for envs in [envs.clone(), vec![("GEOFRAME_TEST_EUID", "0".to_string())]] {
        let out = serve_once(&data, &envs);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
        assert!(text(&out.stdout).contains("root"), "{}", text(&out.stdout));
        assert_untouched_after_refusal(&data, &before);
    }
    // 書かない sync --dry-run は root でも動く（ガードの対象外）
    let out = run_owned(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_TEST_EUID", "0".to_string())],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn write_commands_refuse_a_missing_database_and_create_nothing_but_serve_creates_it() {
    let dir = Dir::new("guard-nodb");
    let env = IngestEnv::new("guard-nodb");
    let mut envs = env.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    // DATA_DIR そのものが無い（指定の誤り）。作らない
    let missing = dir.0.join("no-such-data-dir");
    for args in write_commands(&missing) {
        let out = run_args(&args, &envs);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains("catalog.db"),
            "{}",
            text(&out.stderr)
        );
        assert!(!missing.exists(), "{args:?}");
    }
    // DATA_DIR はあるが catalog.db が無い。ロックも tmp/ も DB も作らない
    let empty = dir.0.join("empty-data-dir");
    fs::create_dir_all(empty.join("photos")).unwrap();
    let before = snapshot(&empty);
    for args in write_commands(&empty) {
        let out = run_args(&args, &envs);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(snapshot(&empty), before, "{args:?}");
        assert!(!empty.join(".ingest.lock").exists() && !empty.join("tmp").exists());
        assert!(!empty.join("catalog.db").exists());
    }
    // 書かない sync --dry-run は今までどおり DB を作らない（終了コードは別の理由。置き場が変わらないことだけ見る）
    let _ = run_owned(
        &["sync", "--dry-run", "--data-dir", empty.to_str().unwrap()],
        &envs,
    );
    assert!(!empty.join("catalog.db").exists());
    // `serve` は GEOFRAME_INGEST=1 なら DB を作る（今までどおり）
    let port = free_port();
    let mut child = serve(&empty, port, Some(("1", &env)));
    wait_healthy(&mut child, port);
    let (status, _) = terminate(child);
    assert!(status.success());
    assert!(empty.join("catalog.db").is_file());
}

#[test]
fn dry_run_and_read_only_serve_need_no_ingest() {
    // 取り込みを有効にしなくても、書かないコマンドは動く
    let dir = Dir::new("guard-readonly");
    let data = data_with_leftovers(&dir);
    let dry = run_owned(
        &["sync", "--dry-run", "--data-dir", data.to_str().unwrap()],
        &[("GEOFRAME_TEST_EUID", "1000".to_string())],
    );
    assert!(dry.status.success(), "{}", text(&dry.stderr));
    let port = free_port();
    let mut child = serve_with_env(&data, port, None, &[("GEOFRAME_TEST_EUID", "1000")]);
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
fn organize_without_ingest_or_as_root_refuses_and_moves_nothing() {
    let dir = Dir::new("organize-refused");
    let data = data_with_a_photo(&dir);
    let before = snapshot(&data);
    // GEOFRAME_INGEST が無い
    let out = run(&["organize", "--data-dir", data.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(snapshot(&data), before);
    // root なら断る
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        ("GEOFRAME_TEST_EUID", "0".into()),
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
    let staging = IngestEnv::new("organize-apply");
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
fn quality_scan_without_ingest_or_as_root_refuses_and_changes_nothing() {
    let dir = Dir::new("qscan-refused");
    let data = data_with_derivatives(&dir);
    let before = snapshot(&data);
    let out = run(&["quality-scan", "--data-dir", data.to_str().unwrap()], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_eq!(snapshot(&data), before);
    let envs = vec![
        ("GEOFRAME_INGEST", "1".to_string()),
        ("GEOFRAME_TEST_EUID", "0".into()),
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
    let staging = IngestEnv::new("qscan-apply");
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

// ---------------------------------------------------------------- 検索索引（P4 / T4.3）

/// 本番相当の DB に、新しい空間（登録）・ベクトル・有効な検索空間のポインタを足す。旧結果（`photo_ai_features`・
/// `photo_embeddings`）は別に持たせ、新しい空間の操作で変わらないことを後で確かめる。
fn data_with_active_space(dir: &Dir) -> PathBuf {
    use geoframe::ai::settings::Normalization;
    use geoframe::ai::space::SpaceIdentity;
    let data = dir.0.join("space-data");
    fs::create_dir_all(&data).unwrap();
    production_shaped_db(&data);
    let mut conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    geoframe::ai::migrate(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,
                                            error,analyzed_at)
           VALUES (1,'derivatives/a.jpg',NULL,NULL,'SECRET-SHA','','t');
         INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,
                embedding_model,embedding_dim,embedding,analyzed_at)
           VALUES (1,'derivatives/a.jpg',NULL,NULL,'SECRET-SHA','old-model',3,X'0102','t');
         INSERT INTO photo_embeddings VALUES ('SECRET-SHA','old-model',3,X'0102','t');",
    )
    .unwrap();
    let space = SpaceIdentity::new(
        "space-1",
        "shared-1",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    geoframe::ai::index::register_space(&mut conn, &space, 0).unwrap();
    geoframe::ai::index::store_vector(&mut conn, &space, "SECRET-SHA", "sent", &[1.0, 0.0, 0.0], 0)
        .unwrap();
    let snapshot = geoframe::ai::activation::snapshot_targets(&conn, &space).unwrap();
    assert!(
        geoframe::ai::activation::try_activate(&mut conn, &snapshot, &space, 0.95)
            .unwrap()
            .published()
    );
    data
}

fn active_pointer(data: &Path) -> Option<String> {
    rusqlite::Connection::open(data.join("catalog.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM settings WHERE key='ai_active_space'",
            [],
            |r| r.get(0),
        )
        .ok()
}

fn count(data: &Path, table: &str) -> i64 {
    rusqlite::Connection::open(data.join("catalog.db"))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn ai_jobs_status_shows_the_search_index_with_counts_only_and_writes_nothing() {
    // 空間の表が無い旧 DB: 新しい空間なし・旧結果参照。書き込み 0（AI の表も作らない）
    let dir = Dir::new("aiindex-status-old");
    production_shaped_db(&dir.0);
    let before = snapshot(&dir.0);
    let out = run(
        &["ai-jobs", "status", "--data-dir", dir.0.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains("検索空間: なし（空間の表が無い。旧結果参照）（登録 0 件）"),
        "{stdout}"
    );
    assert_eq!(snapshot(&dir.0), before);

    // 有効な空間がある DB: 件数だけ（空間の ID・SHA・モデルは出さない）
    let dir = Dir::new("aiindex-status-active");
    let data = data_with_active_space(&dir);
    let before = snapshot(&data);
    let out = run(
        &["ai-jobs", "status", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("検索空間: あり（登録 1 件）"), "{stdout}");
    assert!(
        stdout.contains("検索索引: 対象 1 / 保存済み 1 / 待ち 0 / 失敗で除外 0 / 壊れ 0"),
        "{stdout}"
    );
    assert!(stdout.contains("旧結果 1"), "{stdout}");
    for secret in ["SECRET", "space-1", "model-a", "shared-1", "old-model"] {
        assert!(!stdout.contains(secret), "{secret}: {stdout}");
    }
    assert_eq!(snapshot(&data), before);
}

#[test]
fn ai_jobs_status_never_prints_a_stored_failure_reason_that_is_not_a_known_code() {
    let dir = Dir::new("aiindex-status-reasons");
    let data = data_with_active_space(&dir);
    let space = {
        let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
        geoframe::ai::index::load_space(&conn, "space-1")
            .unwrap()
            .unwrap()
    };
    let mut conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    for (id, sha, code) in [
        (
            101,
            "SECRET-SHA-2",
            "synthetic-private-value\nINJECTED-LINE",
        ),
        (102, "SECRET-SHA-3", "other-private"),
        (103, "SECRET-SHA-4", "bad_response"),
    ] {
        conn.execute(
            "INSERT INTO photos(id,source,name,display,md5,mtime,lat,lon)
             VALUES(?1,'drive','synthetic',?2,?3,?1,35,139)",
            rusqlite::params![id, format!("derivatives/p{id}.jpg"), format!("m{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,
                                                error,analyzed_at)
             SELECT id,display,md5,mtime,?2,'','t' FROM photos WHERE id=?1",
            rusqlite::params![id, sha],
        )
        .unwrap();
        let snapshot = geoframe::ai::activation::snapshot_targets(&conn, &space).unwrap();
        let key = snapshot
            .targets()
            .iter()
            .find(|t| t.input_sha256() == sha)
            .unwrap()
            .job_key(&space);
        geoframe::ai::jobs::enqueue(&mut conn, &key, 0).unwrap();
        conn.execute(
            "UPDATE ai_jobs SET state='failed', terminal=1, last_error_code=?1 WHERE input_hash=?2",
            rusqlite::params![code, key.input_hash],
        )
        .unwrap();
    }
    drop(conn);
    let before = snapshot(&data);
    let out = run(
        &["ai-jobs", "status", "--data-dir", data.to_str().unwrap()],
        &[],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));
    assert!(
        stdout.contains("検索索引 除外の理由 bad_response: 1 件"),
        "{stdout}"
    );
    assert!(
        stdout.contains("検索索引 除外の理由 other: 2 件"),
        "{stdout}"
    );
    assert!(stdout.contains("失敗で除外 3"), "{stdout}");
    for marker in [
        "synthetic-private-value",
        "INJECTED",
        "other-private",
        "SECRET",
    ] {
        assert!(
            !stdout.contains(marker) && !text(&out.stderr).contains(marker),
            "{marker}: {stdout}"
        );
    }
    assert_eq!(snapshot(&data), before);
}

#[test]
fn deactivate_space_needs_ingest_the_guard_and_a_stopped_service_and_keeps_every_result() {
    let dir = Dir::new("aiindex-deactivate");
    let data = data_with_active_space(&dir);
    let before = snapshot(&data);
    let arg = data.to_str().unwrap();
    let args = ["ai-jobs", "deactivate-space", "--data-dir", arg];
    // INGEST が無い → 2。root → 2。どちらも何も書かない
    let out = run(&args, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("GEOFRAME_INGEST=1"));
    let out = run(
        &args,
        &[("GEOFRAME_INGEST", "1"), ("GEOFRAME_TEST_EUID", "0")],
    );
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert_untouched_after_refusal(&data, &before);

    // 稼働中の serve（ロックを持つ）があるときは、ガードを通っても断る → 1。ポインタは変わらない
    let staging = IngestEnv::new("aiindex-deactivate");
    let port = free_port();
    let mut child = serve(&data, port, Some(("1", &staging)));
    wait_healthy(&mut child, port);
    let mut envs = staging.envs();
    envs.push(("GEOFRAME_INGEST", "1".into()));
    let out = run_owned(&args, &envs);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("サービスを止めてから"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(active_pointer(&data).as_deref(), Some("space-1"));
    let (stopped, _) = terminate(child);
    assert!(stopped.success());

    // 停止後は外せる。旧結果・空間・ベクトル・ジョブは消さず、設定の ai_active_space だけが消える
    let legacy = (
        count(&data, "photo_ai_features"),
        count(&data, "photo_embeddings"),
        count(&data, "ai_embedding_spaces"),
        count(&data, "ai_embedding_vectors"),
    );
    let out = run_owned(&args, &envs);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("有効な検索空間を外した（旧結果参照へ戻した）"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(active_pointer(&data), None);
    assert_eq!(
        legacy,
        (
            count(&data, "photo_ai_features"),
            count(&data, "photo_embeddings"),
            count(&data, "ai_embedding_spaces"),
            count(&data, "ai_embedding_vectors"),
        )
    );
    assert_eq!(legacy, (1, 1, 1, 1));
    // 2 回目は何も変えない
    let out = run_owned(&args, &envs);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("有効な検索空間は設定されていない"),
        "{}",
        text(&out.stdout)
    );
    // status は旧結果参照へ戻ったと示す
    let out = run(&["ai-jobs", "status", "--data-dir", arg], &[]);
    assert!(
        text(&out.stdout).contains("検索空間: なし（旧結果参照）（登録 1 件）"),
        "{}",
        text(&out.stdout)
    );
}
