//! `catalog.db` への書き込み（writer）。**単一の接続を専用スレッドに置き、チャネルで依頼を受ける**。
//!
//! 現行 Python の「1 接続＋グローバルロック」と同じ直列化を、ロックではなく所有で表す。読み取り側
//! （[`crate::db::Catalog::read`] と接続のプール）には触らない: 読み取りは WAL なので書き込みを
//! 待たない（`db.rs`）。
//!
//! # 使い方
//!
//! ```ignore
//! let writer = Writer::open(&db_path, OpenMode::CreateIfMissing)?;
//! writer.call(|conn| schema::migrate(conn))?;       // スレッド上で実行され、結果が返る
//! writer.call(|conn| { /* UPDATE … */ Ok(()) })?;
//! ```
//!
//! [`Writer::call`] は呼び出したスレッドを**ブロック**する。同期サイクル（[`crate::sync`]）の専用 std
//! スレッドから使う前提。tokio の worker から呼ぶ（P3 の更新 API）ときは `spawn_blocking` に包む。
//!
//! # 後始末の約束
//!
//! - 依頼が終わったとき、トランザクションが開いたままなら `ROLLBACK` し、自動コミットの状態に戻った
//!   ことを確かめる（`BEGIN` だけして戻ると、以後の依頼が同じトランザクションの続きになってしまう）。
//!   **この後始末が終わってから**、結果を呼び出し側へ返す。
//! - 依頼がパニックしたら、同じく後始末をして**接続はそのまま使い続け**、呼び出し側には
//!   [`WriteError::Task`] を返す。パニックのメッセージ（値が入りうる）は持たない。
//! - `ROLLBACK` できない・自動コミットの状態に戻らないときは、**その接続を捨てて writer を止める**。
//!   その依頼の結果は（成功していても）[`WriteError::Task`] に差し替え、以後の依頼（キューに残って
//!   いたものも）は実行せずに同じ [`WriteError::Task`] を返す。開いたままのトランザクションを次の依頼が
//!   引き継いで、後続の `COMMIT` で前の依頼の行まで確定する事故を防ぐ。再接続はしない（再起動で直る）。
//! - 全ての `Writer` が drop されるとスレッドが終わり、接続が閉じる（WAL がチェックポイントされる）。
//!
//! # ログ・エラー
//!
//! [`WriteError`] の `Display` は、SQLite が返すメッセージ（表名・列名・制約名までで値は入らない）と
//! 定型文だけ。SQL の文面・行の値・ファイルのパスは入れない。

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// 別のプロセスが書き込み中のとき（移行の間の Python など）に待つ時間。
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// DB が無いときの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// 無ければ新しく作る（取り込みが有効なときの `serve`・`sync`）。
    CreateIfMissing,
    /// 無ければ失敗する。
    MustExist,
}

#[derive(Debug)]
pub enum WriteError {
    /// 接続を開けない。
    Open(rusqlite::Error),
    /// 依頼の SQL が失敗した。
    Query(rusqlite::Error),
    /// 依頼がパニックした・書き込みスレッドが止まっている。文言は定型。
    Task(&'static str),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(e) => write!(f, "DB を書き込み用に開けません: {e}"),
            Self::Query(e) => write!(f, "DB の書き込みに失敗しました: {e}"),
            Self::Task(m) => write!(f, "DB の書き込みタスクが異常終了しました: {m}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// 書き込み用に接続を開く（読み書き。`NO_MUTEX`: 1 つのスレッドだけが使う）。
fn open_read_write(path: &Path, mode: OpenMode) -> rusqlite::Result<Connection> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if mode == OpenMode::CreateIfMissing {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// 依頼。接続で実行し、**後始末が済んだあとに結果を返信する**ための閉包を返す
/// （引数は後始末が成功したか。失敗なら結果を捨ててエラーを返信する）。
type Job = Box<dyn FnOnce(&mut Connection) -> Reply + Send>;
type Reply = Box<dyn FnOnce(bool) + Send>;

/// 後始末: 開いたトランザクションを戻し、自動コミットの状態に戻ったことを確かめる。
/// 戻せなかった（接続を使い続けてはいけない）ときは `false`。
type Cleanup = fn(&Connection) -> bool;

fn rollback_and_verify(conn: &Connection) -> bool {
    if conn.is_autocommit() {
        return true;
    }
    if conn.execute_batch("ROLLBACK").is_err() {
        tracing::error!("書き込み接続のトランザクションを戻せません。書き込みを止めます");
        return false;
    }
    if !conn.is_autocommit() {
        tracing::error!("ROLLBACK のあとも自動コミットの状態に戻りません。書き込みを止めます");
        return false;
    }
    true
}

/// 試験用の割り込み口（[`Writer::interrupt_next_call`]）。
#[cfg(test)]
type Hook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// 書き込み用の接続を持つスレッドへの入口。複製は安く、全ての複製が同じ接続（同じスレッド）を使う。
#[derive(Clone)]
pub struct Writer {
    jobs: mpsc::Sender<Job>,
    /// 試験用: 次の [`Writer::call`] が依頼を積む直前に 1 回だけ呼ぶ閉包（呼び出したスレッドで）。
    /// 同期サイクルの段が「計画を読んでから書くまで」の間に更新 API を割り込ませる試験のため。
    #[cfg(test)]
    before_call: Option<Hook>,
}

impl fmt::Debug for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Writer(..)")
    }
}

impl Writer {
    /// 接続を開き、書き込みスレッドを起動する。開けないときはここで失敗する（スレッドは作らない）。
    pub fn open(path: &Path, mode: OpenMode) -> Result<Self, WriteError> {
        let conn = open_read_write(path, mode).map_err(WriteError::Open)?;
        Ok(Self::spawn(conn))
    }

    fn spawn(conn: Connection) -> Self {
        Self::spawn_with(conn, rollback_and_verify)
    }

    /// 後始末を差し替えられる形（故障注入の試験用に分けてある）。
    fn spawn_with(mut conn: Connection, cleanup: Cleanup) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("db-writer".into())
            .spawn(move || {
                // 全ての Writer が drop されると recv が Err になり、スレッドが終わる
                while let Ok(job) = inbox.recv() {
                    let reply = job(&mut conn);
                    // 後始末（トランザクションを戻して自動コミットに戻ったことの確認）が先、返信が後
                    let clean = cleanup(&conn);
                    reply(clean);
                    if !clean {
                        // この接続は使い続けない。ここで抜けると inbox が drop され、キューに残った
                        // 依頼も実行されずに『停止しています』で返る。conn の drop で開いたままの
                        // トランザクションは SQLite が捨てる
                        break;
                    }
                }
            })
            .expect("書き込みスレッドを起動できません");
        Self {
            jobs,
            #[cfg(test)]
            before_call: None,
        }
    }

    /// 試験用: 次の [`Writer::call`] の直前に `f` を 1 回だけ呼ぶ複製を返す（元の `Writer` には効かない）。
    /// `f` の中で元の `Writer` に書けば、その書き込みはこの `call` の依頼より先に確定する。
    #[cfg(test)]
    pub(crate) fn interrupt_next_call(&self, f: impl FnOnce() + Send + 'static) -> Self {
        Self {
            before_call: Some(Arc::new(Mutex::new(Some(Box::new(f))))),
            ..self.clone()
        }
    }

    /// `f` を書き込みスレッドの接続で実行し、結果を返す（呼び出しスレッドはその間ブロックされる）。
    ///
    /// `f` の中で `BEGIN`／`COMMIT` を自分で扱ってよい。コミットしないまま戻ると `ROLLBACK` される。
    pub fn call<T, F>(&self, f: F) -> Result<T, WriteError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        #[cfg(test)]
        if let Some(hook) = &self.before_call {
            let hook = hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let (reply, answer) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |conn| {
            let result = catch_unwind(AssertUnwindSafe(|| f(conn)))
                .map_err(|_| WriteError::Task("書き込みがパニックしました"))
                .and_then(|r| r.map_err(WriteError::Query));
            Box::new(move |clean| {
                // 後始末に失敗したら、結果が成功でも返さない（確定していない可能性がある）
                let result = if clean {
                    result
                } else {
                    Err(WriteError::Task(
                        "書き込み接続を元に戻せなかったので書き込みを止めました",
                    ))
                };
                // 呼び出し側が待つのをやめていても（drop 済み）構わない
                let _ = reply.send(result);
            })
        });
        self.jobs
            .send(job)
            .map_err(|_| WriteError::Task("書き込みスレッドが停止しています"))?;
        answer
            .recv()
            .map_err(|_| WriteError::Task("書き込みスレッドが停止しています"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn writer(dir: &TempDir) -> Writer {
        let w = Writer::open(&dir.path().join("w.db"), OpenMode::CreateIfMissing).unwrap();
        w.call(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.execute_batch("CREATE TABLE t(k INTEGER PRIMARY KEY, v TEXT)")
        })
        .unwrap();
        w
    }

    fn count(w: &Writer) -> i64 {
        w.call(|c| c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)))
            .unwrap()
    }

    #[test]
    fn jobs_run_in_order_on_one_connection_and_thread() {
        let dir = TempDir::new("writer-order");
        let w = writer(&dir);
        let a = w.call(|_| Ok(thread::current().id())).unwrap();
        let b = w.call(|_| Ok(thread::current().id())).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, thread::current().id());
        for i in 0..10 {
            w.call(move |c| c.execute("INSERT INTO t(v) VALUES (?1)", [i.to_string()]))
                .unwrap();
        }
        assert_eq!(count(&w), 10);
        // 一時テーブルは接続に付く: 別の依頼からも見える = 同じ接続
        w.call(|c| c.execute_batch("CREATE TEMP TABLE x(a)"))
            .unwrap();
        w.call(|c| c.execute_batch("INSERT INTO x VALUES (1)"))
            .unwrap();
    }

    #[test]
    fn concurrent_callers_are_serialized() {
        let dir = TempDir::new("writer-concurrent");
        let w = writer(&dir);
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let w = w.clone();
                thread::spawn(move || {
                    for i in 0..25 {
                        w.call(move |c| {
                            // 読んでから書く。直列化されていなければ値が重なる
                            let max: i64 =
                                c.query_row("SELECT COALESCE(MAX(k),0) FROM t", [], |r| r.get(0))?;
                            c.execute(
                                "INSERT INTO t(k,v) VALUES (?1,?2)",
                                rusqlite::params![max + 1, format!("{n}-{i}")],
                            )
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(count(&w), 200);
    }

    #[test]
    fn an_open_transaction_is_rolled_back_after_the_job() {
        let dir = TempDir::new("writer-rollback");
        let w = writer(&dir);
        w.call(|c| c.execute_batch("BEGIN; INSERT INTO t(v) VALUES ('x')"))
            .unwrap();
        assert_eq!(count(&w), 0);
        // 次の依頼は新しいトランザクションとして始まる
        assert!(w.call(|c| Ok(c.is_autocommit())).unwrap());
    }

    #[test]
    fn a_failing_job_reports_the_error_and_the_writer_keeps_working() {
        let dir = TempDir::new("writer-error");
        let w = writer(&dir);
        w.call(|c| c.execute("INSERT INTO t(k,v) VALUES (1,'secret-value')", []))
            .unwrap();
        let err = w
            .call(|c| c.execute("INSERT INTO t(k,v) VALUES (1,'secret-value')", []))
            .unwrap_err();
        assert!(matches!(err, WriteError::Query(_)));
        // 制約違反の文言は表と列の名前まで。値・SQL は入らない
        let text = err.to_string();
        assert!(!text.contains("secret-value"), "{text}");
        assert!(!text.contains("INSERT"), "{text}");
        assert_eq!(count(&w), 1);
    }

    #[test]
    fn a_panicking_job_is_contained_and_rolled_back() {
        let dir = TempDir::new("writer-panic");
        let w = writer(&dir);
        let err = w
            .call(|c| -> rusqlite::Result<()> {
                c.execute_batch("BEGIN; INSERT INTO t(v) VALUES ('x')")?;
                panic!("secret-value");
            })
            .unwrap_err();
        let WriteError::Task(message) = err else {
            panic!("パニックが Task にならなかった");
        };
        assert!(!message.contains("secret-value"));
        // スレッドも接続も生きていて、書きかけは残らない
        assert_eq!(count(&w), 0);
        w.call(|c| c.execute("INSERT INTO t(v) VALUES ('y')", []))
            .unwrap();
        assert_eq!(count(&w), 1);
    }

    #[test]
    fn missing_database_is_created_only_when_asked() {
        let dir = TempDir::new("writer-open");
        let path = dir.path().join("none.db");
        assert!(matches!(
            Writer::open(&path, OpenMode::MustExist),
            Err(WriteError::Open(_))
        ));
        assert!(!path.exists());
        Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn the_connection_closes_when_every_writer_is_dropped() {
        let dir = TempDir::new("writer-close");
        let path = dir.path().join("w.db");
        let w = Writer::open(&path, OpenMode::CreateIfMissing).unwrap();
        w.call(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES (1)")
        })
        .unwrap();
        let w2 = w.clone();
        drop(w);
        w2.call(|c| c.execute_batch("INSERT INTO t VALUES (2)"))
            .unwrap();
        drop(w2);
        // スレッドが終わるのを待つ（接続が閉じると WAL がチェックポイントされ、-wal が消える）
        for _ in 0..100 {
            if !path.with_extension("db-wal").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!path.with_extension("db-wal").exists());
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    /// 故障注入: ROLLBACK が効かない接続（失敗したジョブのトランザクションが残る）。
    /// 失敗は呼び出し側に返り、writer は止まり、後続の依頼は実行されず、行は確定しない。
    #[test]
    fn a_failed_rollback_stops_the_writer_and_never_runs_later_jobs() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = TempDir::new("writer-rollback-fail");
        let path = dir.path().join("w.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch("CREATE TABLE t(k INTEGER PRIMARY KEY, v TEXT)")
                .unwrap();
        }
        // ROLLBACK が効かない後始末: トランザクションが開いていれば戻せず失敗を返す
        let w = Writer::spawn_with(
            open_read_write(&path, OpenMode::MustExist).unwrap(),
            |conn| conn.is_autocommit(),
        );
        // 失敗するジョブ: BEGIN して行を入れ、エラーで戻る（コミットしない）
        let err = w
            .call(|c| -> rusqlite::Result<()> {
                c.execute_batch("BEGIN; INSERT INTO t(v) VALUES ('failed-job')")?;
                Err(rusqlite::Error::InvalidQuery)
            })
            .unwrap_err();
        // 元のエラーではなく、接続を戻せなかったことが返る（呼び出し側は結果を信用しない）
        assert!(matches!(err, WriteError::Task(_)), "{err:?}");

        // 後続のジョブ（COMMIT するもの）は実行されない
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let err = w
            .call(move |c| {
                flag.store(true, Ordering::SeqCst);
                c.execute_batch("INSERT INTO t(v) VALUES ('later-job'); COMMIT")
            })
            .unwrap_err();
        assert!(matches!(err, WriteError::Task(_)), "{err:?}");
        assert!(!ran.load(Ordering::SeqCst));

        // 別の接続から見て、どちらの行も確定していない
        drop(w);
        let check = Connection::open(&path).unwrap();
        let n: i64 = check
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// 返信は後始末のあとに届く: 成功したジョブでも、後始末に失敗したら成功は返らない。
    #[test]
    fn a_successful_job_is_not_reported_when_cleanup_fails() {
        let dir = TempDir::new("writer-cleanup-fail");
        let conn = Connection::open(dir.path().join("w.db")).unwrap();
        let w = Writer::spawn_with(conn, |_| false);
        let err = w.call(|_| Ok(1)).unwrap_err();
        assert!(matches!(err, WriteError::Task(_)));
        assert!(matches!(w.call(|_| Ok(2)), Err(WriteError::Task(_))));
    }
}
