//! SQLite（`catalog.db`）の読み取り層。
//!
//! # 方針
//!
//! - **P1 は読み取り専用**。`SQLITE_OPEN_READ_ONLY`（`CREATE` なし）で開くので、DB を作らず、
//!   書かず、スキーマ移行もしない。`PRAGMA query_only` も重ねて掛ける。スキーマの作成と移行は
//!   当面 Python（`app/geoframe/store.py:db`）が行う。Rust は**足りない列を検出して起動を断る**
//!   だけ（[`REQUIRED_SCHEMA`]）。
//! - SQLite は同期 API なので、tokio の worker では直接実行せず `spawn_blocking` に出す。接続は
//!   少数（[`READ_CONNECTIONS`]）を使い回す小さなプールで、同時に走る読み取りは接続数が上限。
//!   順番待ちはセマフォで非同期に行うので、worker は塞がらない。
//! - Python が書き込み中でも読める。DB は WAL（Python が `journal_mode=WAL` にした状態が DB に
//!   残る）なので、読み取りは書き込みを待たない。チェックポイント前のコミットも WAL から読める。
//!   `busy_timeout` は、WAL の復旧中などで一瞬 `SQLITE_BUSY` になる場合のため。
//! - 読み取り専用でも WAL の DB を開くと `-shm`（共有メモリ）が必要で、無ければ同じディレクトリに
//!   作る。つまり**ディレクトリには書ける**必要がある（systemd の `StateDirectory=` なら満たす）。
//!   DB 本体と `-wal` は変更しない（テストで確認）。
//!
//! # P3 で書き込みを足すとき
//!
//! 書き込みは**単一の接続**を専用スレッドに置き、チャネルで依頼を受ける形にする（現行の「1接続＋
//! グローバルロック」と同じ直列化）。[`Catalog`] に `writer` を足す形で、読み取り側の API
//! （[`Catalog::read`]）は変えずに済む。今は作らない。
//!
//! # ログ・エラー
//!
//! 写真の内容・位置・ファイル名を返す層なので、エラー型の `Display` には SQL の中身や行の値を
//! 入れない（SQLite が返すメッセージ、足りない列・テーブルの**名前**、DB ファイルのパスだけ）。
//! この文字列は**ログにだけ**出し、HTTP 応答には載せない（応答は定型文）。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use tokio::sync::Semaphore;

/// 同時に走らせる読み取りの上限（開く接続の最大数）。N100 の 4 コアに対して十分で、LAN の
/// 家庭用途では余る。
pub const READ_CONNECTIONS: usize = 4;

/// `SQLITE_BUSY` のときに待つ時間。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// P1 の読み取り（T1.2 の `/api/v1/photos`、T1.4 の管理画面の読み取り API）が使うテーブルと列。
/// 現行 Python の `SCHEMA`（`store.py`）と起動時の `ALTER TABLE` の結果にあたる。
/// これが無い DB（古い版、または別物）は起動時に断る。足りない列はここに足す。
pub const REQUIRED_SCHEMA: &[(&str, &[&str])] = &[
    (
        "photos",
        &[
            "id",
            "source",
            "drive_id",
            "path",
            "display",
            "name",
            "md5",
            "mtime",
            "taken_at",
            "lat",
            "lon",
            "loc_source",
            "inferred_from",
            "infer_dt_s",
            "place",
            "elev",
            "title",
            "indexed_at",
            "fetched_at",
            "muni_cd",
            "pref",
            "city",
            "favorite",
            "hidden",
            "rotation",
        ],
    ),
    (
        "photo_quality",
        &[
            "photo_id",
            "display",
            "source_md5",
            "source_mtime",
            "group_id",
            "group_kind",
            "rank_in_group",
            "recommended",
            "score",
        ],
    ),
    ("photo_ai_groups", &["group_id", "status", "same_scene"]),
    (
        "photo_ai_group_members",
        &[
            "group_id",
            "photo_id",
            "rank_in_group",
            "recommended",
            "local_score",
        ],
    ),
];

#[derive(Debug)]
pub enum CatalogError {
    /// DB を開けない（無い・読めない・SQLite の DB ではない）。
    Open(rusqlite::Error),
    /// 必要なテーブル・列が無い（`photos.hidden` のような名前の一覧）。
    Schema(Vec<String>),
    /// 読み取りに失敗した。
    Query(rusqlite::Error),
    /// ブロッキングタスクの異常終了や、プールの停止。
    Task(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(e) => write!(f, "DB を開けません: {e}"),
            Self::Schema(missing) => write!(
                f,
                "DB のスキーマが古いか別物です（足りない項目: {}）",
                missing.join(", ")
            ),
            Self::Query(e) => write!(f, "DB の読み取りに失敗しました: {e}"),
            Self::Task(m) => write!(f, "DB タスクが異常終了しました: {m}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// DB を読み取り専用で開く。`SQLITE_OPEN_READ_ONLY` なので書き込みは SQLite 側が拒否し、
/// `CREATE` を付けないので、無いパスを新規作成することもない。
///
/// WAL モードの DB は、読み取り専用で開いても同じディレクトリに空の `-wal`・`-shm` が作られる
/// ことがある。DB 本体の内容は変わらない（docs/rust-migration/build.md）。
pub fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        // NO_MUTEX: 1つの接続は同時に1スレッドしか使わない（プールが保証する）
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

/// [`REQUIRED_SCHEMA`] のテーブル・列がそろっているか確かめる。
pub fn check_schema(conn: &Connection) -> Result<(), CatalogError> {
    let mut missing = Vec::new();
    for (table, columns) in REQUIRED_SCHEMA {
        // テーブル名は定数なので、プレースホルダーが使えない PRAGMA にそのまま埋めてよい
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(CatalogError::Query)?;
        let present: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(CatalogError::Query)?
            .collect::<Result<_, _>>()
            .map_err(CatalogError::Query)?;
        if present.is_empty() {
            missing.push(format!("{table}（テーブル）"));
            continue;
        }
        for column in *columns {
            if !present.iter().any(|p| p == column) {
                missing.push(format!("{table}.{column}"));
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CatalogError::Schema(missing))
    }
}

struct Inner {
    path: PathBuf,
    /// 空いている接続。`permits` の許可を持つ呼び出しだけが出し入れする。
    idle: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

/// 読み取り専用の `catalog.db`。複製は安い（中身は共有される）。
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<Inner>,
}

impl Catalog {
    /// DB を開いてスキーマを確かめる（ブロッキング。起動時に1回だけ呼ぶ）。
    /// 開けない・スキーマが足りないときは Err を返し、`serve` は起動を断る。
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        Self::open_with(path, READ_CONNECTIONS)
    }

    pub fn open_with(path: &Path, connections: usize) -> Result<Self, CatalogError> {
        let connections = connections.max(1);
        let conn = open_read_only(path).map_err(CatalogError::Open)?;
        check_schema(&conn)?;
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                // 1本だけ先に開いた状態にして、残りは必要になったときに開く
                idle: Mutex::new(vec![conn]),
                permits: Arc::new(Semaphore::new(connections)),
            }),
        })
    }

    /// `f` を読み取り専用の接続で、tokio の blocking スレッドで実行する。
    ///
    /// 呼び出し側が先に Future を捨てても（タイムアウトなど）、始まった `f` は最後まで走って
    /// 接続をプールに戻す。`f` がパニックした場合は、その接続を捨てる（次の読み取りが開き直す）。
    pub async fn read<T, F>(&self, f: F) -> Result<T, CatalogError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|e| CatalogError::Task(e.to_string()))?;
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let pooled = inner
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            let conn = match pooled {
                Some(conn) => conn,
                None => open_read_only(&inner.path).map_err(CatalogError::Open)?,
            };
            let result = f(&conn).map_err(CatalogError::Query);
            inner
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(conn);
            result
        })
        .await
        .map_err(|e| CatalogError::Task(e.to_string()))?
    }

    /// ヘルス確認用: `photos` を実際に1行読めるか（空のテーブルでも成功）。
    pub async fn check_readable(&self) -> Result<(), CatalogError> {
        self.read(|conn| {
            conn.prepare("SELECT 1 FROM photos LIMIT 1")?
                .exists([])
                .map(|_| ())
        })
        .await
    }
}

/// `check-db` の結果。
#[derive(Debug, PartialEq, Eq)]
pub struct DbReport {
    pub photos: i64,
    pub sqlite_version: String,
}

/// `photos` テーブルの行数と、リンクされた SQLite のバージョンを返す（`check-db` 用）。
/// スキーマの全項目は確かめない（古い DB の調査にも使えるように）。
pub fn check_db(path: &Path) -> rusqlite::Result<DbReport> {
    let conn = open_read_only(path)?;
    let photos = conn.query_row("SELECT COUNT(*) FROM photos", [], |row| row.get(0))?;
    Ok(DbReport {
        photos,
        sqlite_version: rusqlite::version().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, synthetic_catalog};

    fn photo_count(conn: &Connection) -> rusqlite::Result<i64> {
        conn.query_row("SELECT COUNT(*) FROM photos", [], |row| row.get(0))
    }

    #[test]
    fn counts_photos_and_reports_version() {
        let dir = TempDir::new("count");
        let (path, _writer) = synthetic_catalog(dir.path(), 3);
        let report = check_db(&path).unwrap();
        assert_eq!(report.photos, 3);
        assert_eq!(report.sqlite_version, rusqlite::version());
    }

    #[test]
    fn connection_is_read_only_and_file_is_untouched() {
        let dir = TempDir::new("ro");
        let (path, writer) = synthetic_catalog(dir.path(), 1);
        drop(writer); // 閉じるとチェックポイントされ、以降はファイル1つの状態になる
        let before = std::fs::read(&path).unwrap();

        let conn = open_read_only(&path).unwrap();
        let err = conn
            .execute("INSERT INTO photos (name) VALUES ('x')", [])
            .unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
        drop(conn);
        check_db(&path).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn missing_file_is_an_error_and_not_created() {
        let dir = TempDir::new("missing");
        let path = dir.path().join("none.db");
        assert!(check_db(&path).is_err());
        assert!(matches!(Catalog::open(&path), Err(CatalogError::Open(_))));
        assert!(!path.exists());
    }

    #[test]
    fn missing_photos_table_is_an_error() {
        let dir = TempDir::new("notable");
        let path = dir.path().join("empty.db");
        Connection::open(&path).unwrap();
        assert!(check_db(&path).is_err());
    }

    #[test]
    fn non_sqlite_file_is_rejected_at_open() {
        let dir = TempDir::new("garbage");
        let path = dir.path().join("catalog.db");
        std::fs::write(&path, vec![b'x'; 4096]).unwrap();
        assert!(Catalog::open(&path).is_err());
    }

    #[test]
    fn stale_schema_lists_missing_names_only() {
        let dir = TempDir::new("stale");
        let path = dir.path().join("catalog.db");
        let conn = Connection::open(&path).unwrap();
        // 旧版を模す: favorite・hidden・rotation が無く、他のテーブルも無い
        conn.execute_batch("CREATE TABLE photos (id, source, name, display, lat, lon)")
            .unwrap();
        drop(conn);
        let Err(CatalogError::Schema(missing)) = Catalog::open(&path) else {
            panic!("スキーマ不足が検出されなかった");
        };
        assert!(
            missing.contains(&"photos.hidden".to_string()),
            "{missing:?}"
        );
        assert!(missing.contains(&"photos.rotation".to_string()));
        assert!(missing.contains(&"photo_quality（テーブル）".to_string()));
        assert!(!missing.iter().any(|m| m == "photos.id"));
    }

    #[test]
    fn full_schema_passes() {
        let dir = TempDir::new("full");
        let (path, _writer) = synthetic_catalog(dir.path(), 0);
        Catalog::open(&path).unwrap();
    }

    #[tokio::test]
    async fn read_goes_through_the_pool_and_refuses_writes() {
        let dir = TempDir::new("pool");
        let (path, _writer) = synthetic_catalog(dir.path(), 2);
        let catalog = Catalog::open(&path).unwrap();

        assert_eq!(catalog.read(photo_count).await.unwrap(), 2);
        let err = catalog
            .read(|c| c.execute("INSERT INTO photos (name) VALUES ('x')", []))
            .await
            .unwrap_err();
        assert!(matches!(err, CatalogError::Query(_)));
        assert!(err.to_string().contains("readonly"), "{err}");
        // 書き込みの失敗のあとも接続は使える
        assert_eq!(catalog.read(photo_count).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn concurrent_reads_are_bounded_by_the_pool_size() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = TempDir::new("bound");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 2).unwrap();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (catalog, running, peak) = (catalog.clone(), running.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                catalog
                    .read(move |_| {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(30));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let peak = peak.load(Ordering::SeqCst);
        assert!((1..=2).contains(&peak), "同時実行が上限を超えた: {peak}");
    }

    #[tokio::test]
    async fn a_panicking_read_does_not_break_the_pool() {
        let dir = TempDir::new("panic");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();
        let err = catalog
            .read(|_| -> rusqlite::Result<()> { panic!("テスト用のパニック") })
            .await
            .unwrap_err();
        assert!(matches!(err, CatalogError::Task(_)));
        // 接続は捨てられたが、許可は戻り、次の読み取りが開き直す
        assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn check_readable_works_on_empty_and_fails_when_table_is_gone() {
        let dir = TempDir::new("readable");
        let (path, writer) = synthetic_catalog(dir.path(), 0);
        let catalog = Catalog::open(&path).unwrap();
        catalog.check_readable().await.unwrap();
        // 開いたあとにテーブルが消えた DB（壊れた・入れ替わった）
        writer.execute_batch("DROP TABLE photos").unwrap();
        assert!(catalog.check_readable().await.is_err());
    }

    // --- WAL（T0.3 レビューからの持ち越し） ---

    fn wal_path(db: &Path) -> PathBuf {
        let mut os = db.as_os_str().to_owned();
        os.push("-wal");
        PathBuf::from(os)
    }

    /// 書き込み側がチェックポイントせずにコミットした行が読めること。DB 本体と既存の WAL は
    /// 読み取りで変わらないこと。
    #[test]
    fn uncheckpointed_commits_are_readable_and_files_are_untouched() {
        let dir = TempDir::new("wal");
        let (path, writer) = synthetic_catalog(dir.path(), 1);
        // 自動チェックポイントを止め、いまの内容を本体へ書き出してから、さらにコミットする
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        let main_at_checkpoint = std::fs::read(&path).unwrap();
        for i in 0..5 {
            writer
                .execute("INSERT INTO photos (name) VALUES (?1)", [format!("w{i}")])
                .unwrap();
        }
        // 追加の5行は WAL にだけある（本体は変わっていない）
        let wal = wal_path(&path);
        assert_eq!(std::fs::read(&path).unwrap(), main_at_checkpoint);
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);

        let main_before = std::fs::read(&path).unwrap();
        let wal_before = std::fs::read(&wal).unwrap();

        let conn = open_read_only(&path).unwrap();
        assert_eq!(photo_count(&conn).unwrap(), 6, "WAL 上のコミットが見えない");
        check_schema(&conn).unwrap();
        assert!(conn.execute("DELETE FROM photos", []).is_err());
        drop(conn);

        assert_eq!(
            std::fs::read(&path).unwrap(),
            main_before,
            "DB 本体が変わった"
        );
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before, "WAL が変わった");
        drop(writer);
    }

    /// Python 側が書き込みトランザクションを持っている間も読める。コミット前の行は見えず、
    /// コミット後の読み取りには見える。
    #[tokio::test]
    async fn reads_do_not_wait_for_an_open_write_transaction() {
        let dir = TempDir::new("busy");
        let (path, writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open(&path).unwrap();

        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .execute("INSERT INTO photos (name) VALUES ('pending')", [])
            .unwrap();
        // 書き込み中でも、待たずにコミット済みの内容が読める（busy_timeout の 5 秒に達しない）
        let started = std::time::Instant::now();
        assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
        assert!(started.elapsed() < Duration::from_secs(2));

        writer.execute_batch("COMMIT").unwrap();
        assert_eq!(catalog.read(photo_count).await.unwrap(), 2);
    }
}
