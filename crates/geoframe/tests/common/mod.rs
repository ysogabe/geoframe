//! 契約テスト（photo_feed_*.rs）の共通部品。合成データだけを使う。

// テストファイルごとに使う部品が違うので、使われない関数があっても警告しない
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::feed::{Photo, QualityKind};
use geoframe::server::{AppState, router};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 契約のフィールド（docs/architecture/photo-feed-contract.md の表）。
pub const CONTRACT_FIELDS: [&str; 13] = [
    "id",
    "file",
    "time",
    "lat",
    "lon",
    "pref",
    "city",
    "title",
    "fav",
    "fetched_at",
    "quality_kind",
    "quality_recommended",
    "trip",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: PathBuf) -> Value {
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// docs/architecture/photo-feed.schema.json
pub fn schema_json() -> Value {
    read_json(repo_root().join("docs/architecture/photo-feed.schema.json"))
}

/// 現行 Python の `api_photos()` が合成DBに対して出した JSON（再生成は
/// `python3 tools/gen_photo_feed_fixture.py`）。
pub fn python_fixture() -> Value {
    read_json(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/photo-feed-python.json"),
    )
}

pub fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema_json()).expect("Schema が不正")
}

/// Schema の違反の説明（空なら適合）。
pub fn violations(validator: &jsonschema::Validator, instance: &Value) -> Vec<String> {
    validator
        .iter_errors(instance)
        .map(|e| format!("{} ({})", e, e.instance_path()))
        .collect()
}

/// 内部値を全部埋めた合成の写真と、グループに入らない合成の写真。
pub fn sample_photos() -> Vec<Photo> {
    let plain = Photo {
        id: 1,
        fetched_at: String::new(),
        fav: false,
        file: "derivatives/s01.jpg".into(),
        title: "IMG_0001".into(),
        lat: 35.0,
        lon: 139.0,
        time: "2025-06-01 10:00".into(),
        place: String::new(),
        pref: String::new(),
        city: String::new(),
        elev: None,
        inferred: false,
        quality_score: None,
        quality_group: None,
        quality_kind: QualityKind::None,
        quality_rank: None,
        quality_recommended: false,
        quality_role: String::new(),
        ai_group: String::new(),
        ai_status: String::new(),
        ai_same_scene: false,
        ai_rank: None,
        ai_recommended: false,
        ai_local_score: None,
        trip: 0,
    };
    let grouped = Photo {
        id: 2,
        fetched_at: "2025-06-02T09:30:00".into(),
        fav: true,
        file: "derivatives/s02.jpg".into(),
        title: "見本市 サンプル町".into(),
        lat: 35.2,
        lon: 139.2,
        time: "2025-06-07 11:00".into(),
        place: "サンプル町".into(),
        pref: "架空県".into(),
        city: "見本市".into(),
        elev: Some(12.5),
        inferred: true,
        quality_score: Some(0.9),
        quality_group: Some(1),
        quality_kind: QualityKind::Similar,
        quality_rank: Some(1),
        quality_recommended: true,
        quality_role: "recommended".into(),
        ai_group: "ai-1".into(),
        ai_status: "judged".into(),
        ai_same_scene: true,
        ai_rank: Some(1),
        ai_recommended: true,
        ai_local_score: Some(0.8),
        trip: 1,
    };
    let exact_unknown_time = Photo {
        id: 3,
        file: "derivatives/s03.jpg".into(),
        time: String::new(),
        quality_kind: QualityKind::Exact,
        quality_role: "alternate".into(),
        quality_group: Some(2),
        quality_rank: Some(2),
        quality_score: Some(0.7),
        trip: 2,
        ..plain.clone()
    };
    vec![plain, grouped, exact_unknown_time]
}

/// テスト用の一時ディレクトリ。落ちたテストでも `Drop` で消える。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "geoframe-test-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// フィクスチャを作った合成DB（本物のスキーマ＋行）のSQL。`python3 tools/gen_photo_feed_fixture.py`
/// が現行 Python から書き出す。
pub fn catalog_sql() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/photo-feed-catalog.sql");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// フィクスチャと同じ合成DBを作る。`then` は続けて流すSQL（行の追加・削除）。戻り値の接続は書き込み用で、
/// 持ち続けるとWALが残る（読み取りが書き込み中のDBも読めることの確認にもなる）。
pub fn build_catalog(dir: &Path, then: &str) -> (PathBuf, rusqlite::Connection) {
    let path = dir.join("catalog.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(&catalog_sql()).unwrap();
    conn.execute_batch(then).unwrap();
    (path, conn)
}

/// フィクスチャの行をすべて消した合成DB（スキーマだけ）に、`rows` のSQLで行を入れる。
pub fn build_empty_catalog(dir: &Path, rows: &str) -> (PathBuf, rusqlite::Connection) {
    build_catalog(
        dir,
        &format!(
            "DELETE FROM photo_ai_group_members; DELETE FROM photo_ai_groups;
             DELETE FROM photo_quality; DELETE FROM photos; {rows}"
        ),
    )
}

pub fn app_state(catalog: Catalog) -> AppState {
    let config = Config::load(|_| None, &Overrides::default()).unwrap();
    AppState::new(Arc::new(config), catalog)
}

/// 127.0.0.1 の空きポートで router を起動する。
pub async fn spawn(catalog: Catalog) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(app_state(catalog));
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
}

pub struct Response {
    /// 状態コード
    pub status: u16,
    /// ヘッダ行（小文字にしたもの）
    pub headers: String,
    pub body: Vec<u8>,
}

impl Response {
    /// ヘッダの値（名前は小文字で渡す）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
    }

    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("本文は UTF-8")
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("本文は JSON")
    }
}

pub async fn get(addr: std::net::SocketAddr, target: &str) -> Response {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("ヘッダの終わり");
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let (status_line, headers) = head.split_once("\r\n").unwrap();
    Response {
        status: status_line.split(' ').nth(1).unwrap().parse().unwrap(),
        headers: headers.to_lowercase(),
        body: raw[split + 4..].to_vec(),
    }
}

/// 任意のメソッド・ヘッダでリクエストを送る。**Host は自動では付けない**（`headers` で指定する。
/// 無い場合の挙動も確かめるため）。`target` は加工せずそのまま送る（`%2e%2e` や `//` を確かめるため）。
pub async fn request(
    addr: std::net::SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> Response {
    let mut head = format!("{method} {target} HTTP/1.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("ヘッダの終わり");
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let (status_line, headers) = head.split_once("\r\n").unwrap_or((head, ""));
    Response {
        status: status_line.split(' ').nth(1).unwrap().parse().unwrap(),
        headers: headers.to_lowercase(),
        body: raw[split + 4..].to_vec(),
    }
}

/// 合成DBを開いて router を起動し、`/api/v1/photos` の応答を返す。
pub async fn photos_from(path: &Path) -> Response {
    let addr = spawn(Catalog::open(path).unwrap()).await;
    get(addr, "/api/v1/photos").await
}

/// 管理画面の読み取り API のフィクスチャ（`python3 tools/gen_admin_fixture.py` が現行 Python から
/// 書き出す）。
pub fn admin_fixture() -> Value {
    read_json(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin-api-python.json"),
    )
}

/// 管理画面 API のフィクスチャを作った合成DB（本物のスキーマ＋行）のSQL。
pub fn admin_catalog_sql() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/admin-api-catalog.sql");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// `dir` をデータ置き場にして、管理画面 API のフィクスチャと同じ合成DB（`dir/catalog.db`）を作る。
/// `derivatives/<drive_id>.jpg` の空ファイルも、フィクスチャが指定する名前で作る。戻り値の接続は書き込み用。
pub fn build_admin_catalog(dir: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(dir.join("catalog.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(&admin_catalog_sql()).unwrap();
    std::fs::create_dir_all(dir.join("derivatives")).unwrap();
    for id in admin_fixture()["derivative_ids"].as_array().unwrap() {
        std::fs::write(
            dir.join("derivatives")
                .join(format!("{}.jpg", id.as_str().unwrap())),
            b"",
        )
        .unwrap();
    }
    conn
}

/// 環境変数 `env` とデータ置き場 `dir` で設定を作り、その `catalog` で router を起動する。
pub async fn spawn_with_env(
    dir: &Path,
    env: &[(&str, &str)],
    catalog: Catalog,
) -> std::net::SocketAddr {
    let map: std::collections::HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let config = Config::load(
        |k| map.get(k).cloned(),
        &Overrides {
            data_dir: Some(dir.to_path_buf()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(AppState::new(Arc::new(config), catalog));
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
}

/// `POST` を、ヘッダを加工せずそのまま（`Content-Length` も自動では付けない）送り、応答を読む。
/// サーバーが本文を読まずに閉じたときは、本文の送信が失敗したりリセットされたりするが、
/// それまでに届いた応答は返す（上限超過の 413 の確認用）。
pub async fn post_raw(
    addr: std::net::SocketAddr,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Response {
    let mut head = format!("POST {target} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _ = stream.write_all(&request).await;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 65536];
    // 接続が閉じられないまま止まった試験を、ハングではなく失敗にする
    let read_all = async {
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(15), read_all)
        .await
        .is_err()
    {
        panic!("応答が閉じられない: POST {target}（Connection: close を付けたか）");
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("応答が無い: POST {target}"));
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let (status_line, headers) = head.split_once("\r\n").unwrap_or((head, ""));
    Response {
        status: status_line.split(' ').nth(1).unwrap().parse().unwrap(),
        headers: headers.to_lowercase(),
        body: raw[split + 4..].to_vec(),
    }
}
