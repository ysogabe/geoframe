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
//! # 期限つきの依頼
//!
//! [`Writer::call_within`] は、順番待ちを含めて期限までに**始まらなかった**依頼を実行しない（書き込みスレッドが
//! 取り出した時点で期限を過ぎていれば、`f` を呼ばずに [`WriteError::Expired`]）。待つ側は期限で戻るので、
//! 対話的な要求（検索語の送信枠の予約など）が書き込みの順番待ちで塞がれない。期限が来たあとに待つ側が去っても、
//! すでに始まっていた依頼は最後まで実行され確定する（巻き戻さない）。返信が溜まっていても、待つ側が受け取った
//! のが期限のあとなら成功を返さず `Expired` にする。戻り値が `Expired` でも書き込みが確定して
//! いることがあるので、呼び出し側は「確定しなかった」と決めつけず、保守的に扱う（送信枠なら消費済みとみなす）。
//! 後始末・パニックの扱い・接続の復旧は [`Writer::call`] と同じ経路。
//!
//! # ログ・エラー
//!
//! [`WriteError`] の `Display` は、SQLite が返すメッセージ（表名・列名・制約名までで値は入らない）と
//! 定型文だけ。SQL の文面・行の値・ファイルのパスは入れない。

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
    /// 期限つきの依頼（[`Writer::call_within`]）が期限までに終わらなかった。始まっていなければ実行されない。
    Expired,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(e) => write!(f, "DB を書き込み用に開けません: {e}"),
            Self::Query(e) => write!(f, "DB の書き込みに失敗しました: {e}"),
            Self::Task(m) => write!(f, "DB の書き込みタスクが異常終了しました: {m}"),
            Self::Expired => f.write_str("DB の書き込みの順番待ちが期限を過ぎました"),
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
    /// 試験用: 依頼を積んだあと、返信を待つ直前に 1 回だけ呼ぶ閉包（呼び出したスレッドで）。
    /// 『書き込みスレッドが期限内に始めて返信を済ませ、待つ側がまだ受け取っていない』状態を作るため。
    #[cfg(test)]
    before_wait: Option<Hook>,
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
            #[cfg(test)]
            before_wait: None,
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

    /// 試験用: 次の依頼を積んだあと返信を待つ直前に `f` を 1 回だけ呼ぶ複製を返す（元の `Writer` には効かない）。
    #[cfg(test)]
    pub(crate) fn interrupt_before_next_wait(&self, f: impl FnOnce() + Send + 'static) -> Self {
        Self {
            before_wait: Some(Arc::new(Mutex::new(Some(Box::new(f))))),
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
        self.dispatch(None, f)
    }

    /// [`Writer::call`] の期限つき。`deadline` までに（順番待ちを含めて）依頼が始まらなければ `f` を実行せず
    /// [`WriteError::Expired`] を返す。冒頭の「期限つきの依頼」を参照。
    pub fn call_within<T, F>(&self, deadline: Instant, f: F) -> Result<T, WriteError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.dispatch(Some(deadline), f)
    }

    fn dispatch<T, F>(&self, deadline: Option<Instant>, f: F) -> Result<T, WriteError>
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
        // 期限つきのとき、待つ側が去ったことを書き込みスレッドへ伝える旗（期限の判定は両側で行う）
        let gone = deadline.map(|d| (d, Arc::new(AtomicBool::new(false))));
        let job_gone = gone.clone();
        let job: Job = Box::new(move |conn| {
            let expired = job_gone
                .as_ref()
                .is_some_and(|(d, gone)| gone.load(Ordering::SeqCst) || Instant::now() >= *d);
            let result = if expired {
                Err(WriteError::Expired)
            } else {
                catch_unwind(AssertUnwindSafe(|| f(conn)))
                    .map_err(|_| WriteError::Task("書き込みがパニックしました"))
                    .and_then(|r| r.map_err(WriteError::Query))
            };
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
        #[cfg(test)]
        if let Some(hook) = &self.before_wait {
            let hook = hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let stopped = || WriteError::Task("書き込みスレッドが停止しています");
        match gone {
            None => answer.recv().map_err(|_| stopped())?,
            Some((deadline, gone)) => {
                match answer.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    // 返信が溜まっていても、受け取ったのが期限のあとなら成功として返さない（待つ側が
                    // 受け取る前に停止して期限を過ぎると、`recv_timeout` は溜まった返信を返せる）。
                    // 書き込みは確定しているので、呼び出し側は Expired でも消費済みとして扱う
                    Ok(result) => {
                        if Instant::now() >= deadline {
                            gone.store(true, Ordering::SeqCst);
                            Err(WriteError::Expired)
                        } else {
                            result
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // まだ始まっていない依頼は実行されない。始まっていれば最後まで進む（巻き戻さない）
                        gone.store(true, Ordering::SeqCst);
                        Err(WriteError::Expired)
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(stopped()),
                }
            }
        }
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

    // ------------------------------------------------------------ 期限つきの依頼（call_within）

    /// 依頼を 1 つ、解放の合図が来るまで書き込みスレッドで止めておく。止まったことを確かめてから返す。
    fn block(w: &Writer) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let w = w.clone();
        let handle = thread::spawn(move || {
            w.call(move |_| {
                started_tx.send(()).unwrap();
                let _ = release_rx.recv();
                Ok(())
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        (release_tx, handle)
    }

    #[test]
    fn call_within_returns_normally_when_the_writer_is_free() {
        let dir = TempDir::new("writer-within-ok");
        let w = writer(&dir);
        let n = w
            .call_within(Instant::now() + Duration::from_secs(30), |c| {
                c.execute("INSERT INTO t(v) VALUES ('x')", [])
            })
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(count(&w), 1);
    }

    /// 順番待ちの間に期限が来た依頼は、writer が空いても実行されない（DB に書かない）。
    #[test]
    fn call_within_never_runs_a_job_whose_deadline_passed_in_the_queue() {
        let dir = TempDir::new("writer-within-queue");
        let w = writer(&dir);
        let (release, blocker) = block(&w);
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let started = Instant::now();
        let err = w
            .call_within(started + Duration::from_millis(80), move |c| {
                flag.store(true, Ordering::SeqCst);
                c.execute("INSERT INTO t(v) VALUES ('late')", [])
            })
            .unwrap_err();
        assert!(matches!(err, WriteError::Expired), "{err:?}");
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(80), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");

        release.send(()).unwrap();
        blocker.join().unwrap();
        // キューの順に、塞いでいた依頼のあとで期限切れの依頼が取り出されて捨てられる。同期の依頼で追い付く
        assert_eq!(count(&w), 0);
        assert!(!ran.load(Ordering::SeqCst));
        // writer は何事もなく使える
        w.call(|c| c.execute("INSERT INTO t(v) VALUES ('ok')", []))
            .unwrap();
        assert_eq!(count(&w), 1);
    }

    #[test]
    fn call_within_with_a_passed_deadline_does_not_run_even_on_an_idle_writer() {
        let dir = TempDir::new("writer-within-passed");
        let w = writer(&dir);
        let err = w
            .call_within(Instant::now(), |c| {
                c.execute("INSERT INTO t(v) VALUES ('x')", [])
            })
            .unwrap_err();
        assert!(matches!(err, WriteError::Expired), "{err:?}");
        assert_eq!(count(&w), 0);
    }

    /// すでに始まっていた依頼は、待つ側が期限で去っても最後まで実行されて確定する（巻き戻さない）。
    #[test]
    fn call_within_lets_a_started_job_finish_after_the_waiter_left() {
        let dir = TempDir::new("writer-within-started");
        let w = writer(&dir);
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let waiter = {
            let w = w.clone();
            thread::spawn(move || {
                w.call_within(Instant::now() + Duration::from_millis(80), move |c| {
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                    c.execute("INSERT INTO t(v) VALUES ('started')", [])
                })
            })
        };
        started_rx.recv().unwrap();
        assert!(matches!(waiter.join().unwrap(), Err(WriteError::Expired)));
        release_tx.send(()).unwrap();
        w.call(|_| Ok(())).unwrap();
        assert_eq!(count(&w), 1);
    }

    /// 書き込みスレッドが期限内に依頼を終えて返信を済ませていても、待つ側が受け取るのが期限後なら
    /// 成功を返さない（`recv_timeout(0)` は溜まった返信を返せるので、受け取った結果の側でも期限を照合する）。
    /// 順序は同期で固定する: 積む → 期限内に実行・返信 → 待つ直前で待つ側が停止し期限を過ぎる → 再開。
    #[test]
    fn call_within_does_not_return_a_buffered_success_after_the_deadline() {
        let dir = TempDir::new("writer-within-buffered");
        let w = writer(&dir);
        let deadline = Instant::now() + Duration::from_millis(300);
        let (ran_tx, ran_rx) = mpsc::channel::<()>();
        let original = w.clone();
        let paused = w.interrupt_before_next_wait(move || {
            // 依頼が実行されたことを待ち、続けて同期の依頼を通す: 書き込みスレッドは 1 つずつ処理するので、
            // これが戻った時点で先の依頼の返信は（受け手が居ない箱に）積まれている
            ran_rx.recv().unwrap();
            original.call(|_| Ok(())).unwrap();
            // 期限を過ぎるまで、待つ側（このスレッド）を止める
            while Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        });
        let err = paused
            .call_within(deadline, move |c| {
                let n = c.execute("INSERT INTO t(v) VALUES ('buffered')", [])?;
                ran_tx.send(()).unwrap();
                Ok(n)
            })
            .unwrap_err();
        assert!(matches!(err, WriteError::Expired), "{err:?}");
        // 書き込み自体は期限内に始まって確定している。Expired でも消費は戻らない（保守的）
        assert_eq!(count(&w), 1);
    }

    /// 後始末・パニックの扱いは `call` と同じ: 開いたトランザクションは戻され、パニックは `Task` で、
    /// 接続はそのまま使える。
    #[test]
    fn call_within_rolls_back_and_contains_panics_like_call() {
        let dir = TempDir::new("writer-within-panic");
        let w = writer(&dir);
        let deadline = || Instant::now() + Duration::from_secs(30);
        w.call_within(deadline(), |c| {
            c.execute_batch("BEGIN; INSERT INTO t(v) VALUES ('x')")
        })
        .unwrap();
        assert_eq!(count(&w), 0);
        assert!(w.call(|c| Ok(c.is_autocommit())).unwrap());
        let err = w
            .call_within(deadline(), |c| -> rusqlite::Result<()> {
                c.execute_batch("BEGIN; INSERT INTO t(v) VALUES ('x')")?;
                panic!("secret-value");
            })
            .unwrap_err();
        let WriteError::Task(message) = err else {
            panic!("パニックが Task にならなかった");
        };
        assert!(!message.contains("secret-value"));
        assert_eq!(count(&w), 0);
        w.call_within(deadline(), |c| {
            c.execute("INSERT INTO t(v) VALUES ('y')", [])
        })
        .unwrap();
        assert_eq!(count(&w), 1);
    }

    /// 後始末に失敗して止まった writer では、期限つきの依頼も実行されず `Task`（成功は返らない）。
    #[test]
    fn call_within_on_a_stopped_writer_reports_task_not_success() {
        let dir = TempDir::new("writer-within-stopped");
        let conn = Connection::open(dir.path().join("w.db")).unwrap();
        let w = Writer::spawn_with(conn, |_| false);
        let deadline = Instant::now() + Duration::from_secs(30);
        assert!(matches!(
            w.call_within(deadline, |_| Ok(1)),
            Err(WriteError::Task(_))
        ));
        assert!(matches!(
            w.call_within(deadline, |_| Ok(2)),
            Err(WriteError::Task(_))
        ));
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
