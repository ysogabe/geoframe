//! GEOFRAME の Rust 実装。P1 では読み取り専用の API 基盤（設定・SQLite 読み取り層・ログ・ヘルス確認・写真一覧・静的配信・Host 検査・管理画面のページ）を持つ。

// unsafe は sysinfo.rs だけ（libc の statvfs・localtime_r）
#![deny(unsafe_code)]

pub mod admin;
pub mod backoff;
pub mod cache;
pub mod cli;
pub mod config;
pub mod csrf;
pub mod datadir;
pub mod db;
pub mod drive;
pub mod embedded;
pub mod feed;
pub mod geo;
pub mod geocode;
pub mod guard;
pub mod hostcheck;
pub mod imaging;
pub mod ingest;
pub mod jwt;
pub mod limits;
pub mod local;
pub mod logging;
pub mod net;
pub mod photos;
pub mod post;
pub mod pyquery;
pub mod quality;
pub mod rotate;
pub mod schema;
pub mod server;
pub mod statics;
pub mod stop;
pub mod sync;
pub mod sysinfo;
pub mod update;
pub mod webpath;
pub mod writer;

#[cfg(test)]
mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

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

    /// 必要なテーブル・列（`db::REQUIRED_SCHEMA`）だけを持つ合成の `catalog.db` を作る。
    /// 列の型は付けない（SQLite は型なしの列を許す）。`photos` に `rows` 行を入れる。
    /// 戻り値の接続は書き込み用で、WAL モードにしてある。呼び出し側が持ち続けると、閉じたときの
    /// チェックポイントで WAL が消えるのを避けられる。
    pub fn synthetic_catalog(dir: &Path, rows: i64) -> (PathBuf, rusqlite::Connection) {
        let path = dir.join("catalog.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        for (table, columns) in crate::db::REQUIRED_SCHEMA {
            conn.execute_batch(&format!("CREATE TABLE {table} ({})", columns.join(", ")))
                .unwrap();
        }
        for i in 0..rows {
            conn.execute("INSERT INTO photos (name) VALUES (?1)", [format!("p{i}")])
                .unwrap();
        }
        (path, conn)
    }

    /// `tracing` の出力を、試験のスレッドごとに捕まえる。
    ///
    /// `with_default` だけだと、並列に動く他の試験が先に通した呼び出し地点の「興味なし」の判定が残って、
    /// 出るはずの行が捕まらないことがある。そこで**プロセスで 1 つの購読側**（全レベル）を 1 回だけ入れ、
    /// 書き込み先だけをスレッドごとに差し替える（捕まえていないスレッドの出力は捨てる）。
    pub mod capture {
        use std::cell::RefCell;
        use std::io::Write;
        use std::sync::{Arc, Mutex, Once};

        type Sink = Arc<Mutex<Vec<u8>>>;

        thread_local! {
            static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
        }

        struct ThreadWriter;

        impl Write for ThreadWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                SINK.with(|sink| {
                    if let Some(sink) = sink.borrow().as_ref() {
                        sink.lock().unwrap().extend_from_slice(buf);
                    }
                });
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        struct Maker;

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Maker {
            type Writer = ThreadWriter;

            fn make_writer(&'a self) -> ThreadWriter {
                ThreadWriter
            }
        }

        /// `f` を動かし、そのスレッドで出たログ（本番と同じ書式・フィルター。全レベル）を返す。
        pub fn logs_of(f: impl FnOnce()) -> String {
            static INSTALL: Once = Once::new();
            INSTALL.call_once(|| {
                tracing::subscriber::set_global_default(crate::logging::subscriber(
                    crate::config::LogLevel::Trace,
                    Maker,
                ))
                .expect("試験のログの購読側は 1 回だけ入れる");
            });
            let sink: Sink = Arc::default();
            SINK.with(|s| *s.borrow_mut() = Some(Arc::clone(&sink)));
            // `f` がパニックしても、次の試験に出力を持ち越さない
            struct Reset;
            impl Drop for Reset {
                fn drop(&mut self) {
                    SINK.with(|s| *s.borrow_mut() = None);
                }
            }
            let _reset = Reset;
            f();
            let bytes = sink.lock().unwrap().clone();
            String::from_utf8(bytes).expect("ログは UTF-8")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
