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
//!   順番待ちはセマフォで非同期に行うので、worker は塞がらない（上限 [`QUEUE_TIMEOUT`]）。
//!   `/healthz` 用に、この4本とは別の専用の接続を1本持つ。
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

/// `SQLITE_BUSY` のときに待つ時間（API の読み取り用）。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// `/healthz` 専用の接続の `busy_timeout`。`/healthz` 自身の上限（3 秒、`server.rs`）より短くして、
/// DB が詰まっているときも 503 を**自分で**返せるようにする。
const HEALTH_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// 読み取り用の接続が空くのを待つ上限。超えたら [`CatalogError::QueueTimeout`]（API は 503 にする）。
/// 4 本がすべて長い読み取りで埋まっているときに、リクエストが際限なく溜まらないようにする。
pub const QUEUE_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// ブロッキングタスクの異常終了（パニックなど）や、プールの停止。文言は定型で、パニックの
    /// メッセージ（切り出しに失敗した文字列などが入りうる）は含めない。
    Task(&'static str),
    /// 読み取り用の接続が [`QUEUE_TIMEOUT`] 以内に空かなかった。
    QueueTimeout,
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
            Self::QueueTimeout => write!(
                f,
                "DB の読み取りの順番待ちが {} 秒を超えました",
                QUEUE_TIMEOUT.as_secs()
            ),
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
    open_read_only_with(path, BUSY_TIMEOUT)
}

fn open_read_only_with(path: &Path, busy_timeout: Duration) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        // NO_MUTEX: 1つの接続は同時に1スレッドしか使わない（プールが保証する）
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(busy_timeout)?;
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

/// 読み取り専用の接続の小さなプール。接続は `permits` の許可を持つ呼び出しだけが出し入れする。
struct Pool {
    path: PathBuf,
    /// 空いている接続。
    idle: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
    busy_timeout: Duration,
    queue_timeout: Duration,
}

impl Pool {
    fn new(
        path: &Path,
        connections: usize,
        busy_timeout: Duration,
        queue_timeout: Duration,
        first: Option<Connection>,
    ) -> Arc<Self> {
        Arc::new(Self {
            path: path.to_path_buf(),
            idle: Mutex::new(first.into_iter().collect()),
            permits: Arc::new(Semaphore::new(connections.max(1))),
            busy_timeout,
            queue_timeout,
        })
    }

    async fn run<T, F>(self: &Arc<Self>, f: F) -> Result<T, CatalogError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let permit = tokio::time::timeout(
            self.queue_timeout,
            Arc::clone(&self.permits).acquire_owned(),
        )
        .await
        .map_err(|_| CatalogError::QueueTimeout)?
        .map_err(|_| CatalogError::Task("読み取りの待ち行列が閉じています"))?;
        let pool = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let pooled = pool
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            let conn = match pooled {
                Some(conn) => conn,
                None => open_read_only_with(&pool.path, pool.busy_timeout)
                    .map_err(CatalogError::Open)?,
            };
            let result = f(&conn);
            if reusable(&conn, result.as_ref().err()) {
                pool.idle
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(conn);
            }
            result.map_err(CatalogError::Query)
        })
        .await
        .map_err(|e| {
            // JoinError の文字列にはパニックのメッセージが入る。ログに出さないよう定型文にする
            if e.is_panic() {
                CatalogError::Task("読み取りがパニックしました")
            } else {
                CatalogError::Task("読み取りが中断されました")
            }
        })?
    }
}

/// 読み取りのあと、その接続をプールに戻してよいか。
///
/// - 接続の状態が壊れたと見られるエラー（I/O・破損・DB ではない等）が出たら捨てる。SQL の誤りや
///   読み取り専用の拒否のような普通のエラーでは戻す。
/// - `f` がトランザクションを閉じていなければ `ROLLBACK` する。開いたまま戻すと、その接続は古い
///   スナップショットを見続け、Python 側の `-wal` のチェックポイントも進められない。
///   `ROLLBACK` もできなければ捨てる。
fn reusable(conn: &Connection, error: Option<&rusqlite::Error>) -> bool {
    use rusqlite::ErrorCode::{
        CannotOpen, DatabaseCorrupt, DiskFull, InternalMalfunction, NotADatabase, OutOfMemory,
        SystemIoFailure,
    };
    if let Some(code) = error.and_then(rusqlite::Error::sqlite_error_code)
        && matches!(
            code,
            SystemIoFailure
                | DatabaseCorrupt
                | NotADatabase
                | CannotOpen
                | DiskFull
                | OutOfMemory
                | InternalMalfunction
        )
    {
        return false;
    }
    conn.is_autocommit() || conn.execute_batch("ROLLBACK").is_ok()
}

/// 読み取り専用の `catalog.db`。複製は安い（中身は共有される）。
#[derive(Clone)]
pub struct Catalog {
    /// API の読み取り用（最大 [`READ_CONNECTIONS`] 本）。
    api: Arc<Pool>,
    /// `/healthz` 専用の1本。API の読み取りが接続を使い切っていても、DB が正常なら 200 を返せる。
    health: Arc<Pool>,
}

impl Catalog {
    /// DB を開いてスキーマを確かめる（ブロッキング。起動時に1回だけ呼ぶ）。
    /// 開けない・スキーマが足りないときは Err を返し、`serve` は起動を断る。
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        Self::open_with(path, READ_CONNECTIONS)
    }

    pub fn open_with(path: &Path, connections: usize) -> Result<Self, CatalogError> {
        Self::open_with_queue_timeout(path, connections, QUEUE_TIMEOUT)
    }

    fn open_with_queue_timeout(
        path: &Path,
        connections: usize,
        queue_timeout: Duration,
    ) -> Result<Self, CatalogError> {
        let conn = open_read_only(path).map_err(CatalogError::Open)?;
        check_schema(&conn)?;
        Ok(Self {
            // 1本だけ先に開いた状態にして、残りは必要になったときに開く
            api: Pool::new(path, connections, BUSY_TIMEOUT, queue_timeout, Some(conn)),
            health: Pool::new(path, 1, HEALTH_BUSY_TIMEOUT, queue_timeout, None),
        })
    }

    /// `f` を読み取り専用の接続で、tokio の blocking スレッドで実行する。
    ///
    /// 呼び出し側が先に Future を捨てても（タイムアウトなど）、始まった `f` は最後まで走って
    /// 接続をプールに戻す。`f` がパニックした場合は、その接続を捨てる（次の読み取りが開き直す）。
    /// 接続が空くのを [`QUEUE_TIMEOUT`] 待っても空かなければ [`CatalogError::QueueTimeout`]。
    pub async fn read<T, F>(&self, f: F) -> Result<T, CatalogError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.api.run(f).await
    }

    /// ヘルス確認用: `photos` を実際に1行読めるか（空のテーブルでも成功）。API の読み取りとは
    /// 別の専用の接続を使う。
    pub async fn check_readable(&self) -> Result<(), CatalogError> {
        self.health
            .run(|conn| {
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
        // ログに出る文字列に、パニックのメッセージ（値が入りうる）が混ざらない
        assert!(!err.to_string().contains("テスト用のパニック"), "{err}");
        assert!(err.to_string().contains("パニック"), "{err}");
        // 接続は捨てられたが、許可は戻り、次の読み取りが開き直す
        assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
    }

    fn idle_count(catalog: &Catalog) -> usize {
        catalog.api.idle.lock().unwrap().len()
    }

    /// `f` がトランザクションを開けたまま Ok で返っても、プールに戻る前に閉じられる。
    /// 閉じられなければ、次の読み取りは古いスナップショットを見続ける。
    #[tokio::test]
    async fn an_unclosed_transaction_is_rolled_back_before_reuse() {
        let dir = TempDir::new("txn-open");
        let (path, writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();

        let seen = catalog
            .read(|c| {
                c.execute_batch("BEGIN; SELECT COUNT(*) FROM photos")?;
                photo_count(c)
            })
            .await
            .unwrap();
        assert_eq!(seen, 1);
        writer
            .execute("INSERT INTO photos (name) VALUES ('later')", [])
            .unwrap();

        assert!(catalog.read(|c| Ok(c.is_autocommit())).await.unwrap());
        assert_eq!(catalog.read(photo_count).await.unwrap(), 2);
        assert_eq!(idle_count(&catalog), 1, "ROLLBACK できた接続は戻る");
    }

    /// トランザクションの途中でエラーを返しても同じ。
    #[tokio::test]
    async fn a_failing_read_inside_a_transaction_is_rolled_back_too() {
        let dir = TempDir::new("txn-err");
        let (path, writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();

        let err = catalog
            .read(|c| -> rusqlite::Result<()> {
                c.execute_batch("BEGIN; SELECT COUNT(*) FROM photos")?;
                c.execute_batch("SELECT no_such_column FROM photos")
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CatalogError::Query(_)));
        writer
            .execute("INSERT INTO photos (name) VALUES ('later')", [])
            .unwrap();

        assert!(catalog.read(|c| Ok(c.is_autocommit())).await.unwrap());
        assert_eq!(catalog.read(photo_count).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn io_and_corruption_errors_discard_the_connection_but_sql_errors_do_not() {
        use rusqlite::ffi;
        let dir = TempDir::new("discard");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();
        assert_eq!(idle_count(&catalog), 1);

        // 普通の SQL の誤りでは戻る
        catalog
            .read(|c| c.execute_batch("SELECT no_such_column FROM photos"))
            .await
            .unwrap_err();
        assert_eq!(idle_count(&catalog), 1);

        for code in [ffi::SQLITE_IOERR, ffi::SQLITE_CORRUPT, ffi::SQLITE_NOTADB] {
            catalog
                .read(move |_| -> rusqlite::Result<()> {
                    Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
                })
                .await
                .unwrap_err();
            assert_eq!(idle_count(&catalog), 0, "コード {code} の接続は捨てる");
            // 次の読み取りは開き直し、成功すれば戻る
            assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
            assert_eq!(idle_count(&catalog), 1);
        }
    }

    /// 呼び出し側が `timeout` で Future を捨てても、始まった読み取りは最後まで走り、
    /// 接続と許可が戻る（接続1本のプールで、次の読み取りが成功する）。
    #[tokio::test]
    async fn the_connection_returns_when_the_caller_gives_up() {
        let dir = TempDir::new("abandon");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();

        let slow = catalog.read(|c| {
            std::thread::sleep(Duration::from_millis(300));
            photo_count(c)
        });
        let abandoned = tokio::time::timeout(Duration::from_millis(30), slow).await;
        assert!(abandoned.is_err(), "呼び出し側が先に諦める想定");

        // 許可はまだ眠る読み取りが持っている。終われば次が通る
        assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
        assert_eq!(idle_count(&catalog), 1);
    }

    #[tokio::test]
    async fn waiting_for_a_connection_times_out() {
        let dir = TempDir::new("queue");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog =
            Catalog::open_with_queue_timeout(&path, 1, Duration::from_millis(50)).unwrap();

        let busy = {
            let catalog = catalog.clone();
            tokio::spawn(async move {
                catalog
                    .read(|c| {
                        std::thread::sleep(Duration::from_millis(400));
                        photo_count(c)
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let started = std::time::Instant::now();
        let err = catalog.read(photo_count).await.unwrap_err();
        assert!(matches!(err, CatalogError::QueueTimeout), "{err}");
        assert!(started.elapsed() < Duration::from_millis(300));

        assert_eq!(busy.await.unwrap().unwrap(), 1);
        assert_eq!(catalog.read(photo_count).await.unwrap(), 1);
    }

    /// API の読み取りが接続を使い切っていても、`/healthz` 用の接続は別なので待たされない。
    #[tokio::test]
    async fn health_check_does_not_queue_behind_api_reads() {
        let dir = TempDir::new("health-sep");
        let (path, _writer) = synthetic_catalog(dir.path(), 1);
        let catalog = Catalog::open_with(&path, 1).unwrap();

        let busy = {
            let catalog = catalog.clone();
            tokio::spawn(async move {
                catalog
                    .read(|c| {
                        std::thread::sleep(Duration::from_millis(400));
                        photo_count(c)
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let started = std::time::Instant::now();
        catalog.check_readable().await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(300));
        busy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn health_connection_has_a_shorter_busy_timeout() {
        let dir = TempDir::new("health-busy");
        let (path, _writer) = synthetic_catalog(dir.path(), 0);
        let catalog = Catalog::open(&path).unwrap();
        let ms = |pool: &Arc<Pool>| {
            let pool = Arc::clone(pool);
            async move {
                pool.run(|c| c.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0)))
                    .await
                    .unwrap()
            }
        };
        assert_eq!(ms(&catalog.api).await, 5000);
        assert_eq!(ms(&catalog.health).await, 2000);
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
