//! コマンドライン。依存を増やさないため引数は手で解釈する（P1 で必要なら見直す）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::db::Catalog;
use crate::server::AppState;
use crate::stop::StopFlag;
use crate::sync::{CycleReport, Mode, StageCtx, StageResult};

/// 停止のときに、取り込みのスレッドを待つ上限。
const THREAD_STOP_LIMIT: Duration = Duration::from_secs(10);

/// 取り込みの 2 つのスレッド。
struct IngestThreads {
    worker: std::thread::JoinHandle<()>,
    saver: std::thread::JoinHandle<()>,
}

/// 止めるときの順序: **worker の終了を待ってから**キャッシュを最後に保存する。
///
/// 同じ停止の合図で両方を起こすので、saver が先に最後の保存を終えたあとで worker が最後の `put`
/// をすると、その更新は保存されずに失われる。saver は合図で止まるだけ（最後の保存はしない）にして、
/// 保存はここで、両方の終了を待ってから 1 回だけする。限られた時間内に worker が止まらなかったときも
/// 保存はする（そのあとの `put` は失われうるが、警告を出す）。
///
/// saver が I/O の最中に期限を超えて残った場合は、保存用の排他（`Cache::save_final`）で重ねて書かない。
/// 排他は `limit` まで待ち、それでも取れなければ最後の保存は省略する（警告を出す）。省略した分は、
/// 残った saver の保存が終わったあとの内容に含まれないが、書きかけの一時ファイルと競合して JSON を
/// 壊すよりは、キャッシュ（位置の逆引き結果で、作り直せる）の直近の更新を失うほうを選ぶ。
/// 期限を超えて残ったスレッドは `.ingest.lock` を持ち続ける（`LockHold`）ので、そのあいだ別の
/// 取り込みは始まらない。
/// 呼ぶ前に `stop` の合図を出しておくこと。
fn stop_ingest_threads(threads: IngestThreads, cache: &Cache, limit: Duration) {
    for (name, handle) in [
        ("同期サイクル", threads.worker),
        ("キャッシュ保存", threads.saver),
    ] {
        if !join_within(handle, limit) {
            tracing::warn!("{name}のスレッドが時間内に止まりませんでした（そのまま終了します）");
        }
    }
    // 省略したときの警告は save_final の中で出す
    cache.save_final(limit);
}

fn join_within(handle: std::thread::JoinHandle<()>, limit: Duration) -> bool {
    let started = std::time::Instant::now();
    while !handle.is_finished() {
        if started.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    handle.join().is_ok()
}

const USAGE: &str = "\
使い方:
  geoframe --version
  geoframe check-db <catalog.db>        photos の行数と SQLite のバージョンを出す（読み取り専用）
  geoframe selfcheck-net                地理院の逆ジオコーダへ公開の代表点を 1 件問い合わせ、外向き HTTPS（名前解決・
                                        TLS・証明書）が通るかを確かめる。出力は状態と HTTP ステータスだけ
  geoframe serve [--listen <addr:port>] [--data-dir <dir>]
                                        サーバーを起動する（DB は <dir>/catalog.db）。GEOFRAME_INGEST=1 のときだけ
                                        取り込み（DB への書き込みと同期サイクル）も動かす。それ以外は読み取り専用。
                                        取り込みは P5 の切替まで検証環境専用: 検証環境の印（/etc/geoframe/staging）が
                                        無い・geoframe-server が非稼働と確認できないなら、何も書かず終了コード 2 で断る
                                        設定は 起動引数 > 環境変数 GEOFRAME_* > 既定値 の順
                                        （詳細は docs/rust-migration/server.md）
  geoframe sync [--dry-run] [--data-dir <dir>]
                                        同期サイクルを 1 回だけ動かして終わる。--dry-run は行・写真・キャッシュ・
                                        ロックを変えず（SQLite が読み取りでも作る WAL 関連ファイル catalog.db-wal・
                                        -shm はできうる）、やるとしたら何か（スキーマ・掃除・各段）を件数だけ出す。
                                        書く実行は GEOFRAME_INGEST=1 が要り、serve と同じ印・Python の確認を通す
終了コード: 0 成功 / 1 失敗 / 2 引数・設定の誤り、または取り込みを断った（本番への誤指定の防止）";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    CheckDb(PathBuf),
    SelfCheckNet,
    Serve(Overrides),
    Sync {
        dry_run: bool,
        data_dir: Option<PathBuf>,
    },
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("--version" | "-V") => Ok(Command::Version),
        Some("check-db") => {
            let path = args.next().ok_or("check-db にはDBのパスが要ります")?;
            if args.next().is_some() {
                return Err("check-db の引数が多すぎます".into());
            }
            Ok(Command::CheckDb(PathBuf::from(path)))
        }
        Some("selfcheck-net") => match args.next() {
            None => Ok(Command::SelfCheckNet),
            Some(_) => Err("selfcheck-net に引数はありません".into()),
        },
        Some("serve") => {
            let mut overrides = Overrides::default();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--listen" => {
                        let value = args.next().ok_or("--listen に値が要ります")?;
                        // config.rs と同じく、不正な値そのものはメッセージに書かない
                        overrides.listen = Some(value.parse::<SocketAddr>().map_err(
                            |_| "--listen の値が不正です（<addr:port> の形で指定してください）",
                        )?);
                    }
                    "--data-dir" => {
                        let value = args.next().ok_or("--data-dir に値が要ります")?;
                        overrides.data_dir = Some(PathBuf::from(value));
                    }
                    other => return Err(format!("serve の引数 {other:?} は不明です")),
                }
            }
            Ok(Command::Serve(overrides))
        }
        Some("sync") => {
            let (mut dry_run, mut data_dir) = (false, None);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--dry-run" => dry_run = true,
                    "--data-dir" => {
                        let value = args.next().ok_or("--data-dir に値が要ります")?;
                        data_dir = Some(PathBuf::from(value));
                    }
                    other => return Err(format!("sync の引数 {other:?} は不明です")),
                }
            }
            Ok(Command::Sync { dry_run, data_dir })
        }
        Some(other) => Err(format!("不明なコマンド {other:?}")),
        None => Err("コマンドがありません".into()),
    }
}

pub fn run<I: IntoIterator<Item = String>>(args: I) -> ExitCode {
    let command = match parse(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("geoframe: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Version => {
            println!("geoframe {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::CheckDb(path) => match crate::db::check_db(&path) {
            Ok(report) => {
                println!("photos: {}", report.photos);
                println!("sqlite: {}", report.sqlite_version);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("geoframe: {} を確認できません: {e}", path.display());
                ExitCode::FAILURE
            }
        },
        Command::SelfCheckNet => {
            let result = crate::net::selfcheck(&crate::net::agent());
            println!("{}", result.summary());
            if result.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::Serve(overrides) => serve(&overrides),
        Command::Sync { dry_run, data_dir } => sync(dry_run, data_dir),
    }
}

fn serve(overrides: &Overrides) -> ExitCode {
    let config = match Config::load(|key| std::env::var(key).ok(), overrides) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("geoframe: 設定が不正です: {e}");
            return ExitCode::from(2);
        }
    };
    crate::logging::init(config.log_level);
    crate::logging::install_panic_hook();
    tracing::info!(
        "geoframe {} を起動します: {}",
        env!("CARGO_PKG_VERSION"),
        config.log_summary()
    );
    if config.embed_backend_invalid {
        tracing::warn!(
            "GEOFRAME_EMBED_BACKEND が gemini・local のどちらでもないので gemini を使います"
        );
    }

    // 取り込み（GEOFRAME_INGEST=1）: writer を開き、スキーマの作成・移行と起動時の掃除をする。
    // 無効（既定）のときは P1 と同じ読み取り専用で、DB が無ければ下で断る
    let ingest = if config.ingest {
        match crate::ingest::start(&config) {
            Ok(ingest) => Some(ingest),
            Err(e) => {
                tracing::error!("{e}");
                return exit_for_ingest_error(&e);
            }
        }
    } else {
        tracing::info!(
            "取り込みは無効です（DB に書きません）。有効にするには GEOFRAME_INGEST=1 を設定します"
        );
        None
    };

    // 起動時の DB の確認。開けない・スキーマが古いときは起動しない（設定の誤りや配備の誤りを、
    // 503 を返し続けるプロセスに隠さず、systemd の失敗として見せる）
    let catalog = match Catalog::open(&config.db_path()) {
        Ok(catalog) => catalog,
        Err(e) => {
            tracing::error!(
                "{e}（GEOFRAME_DATA_DIR を確認してください。DB のあるディレクトリには書き込める必要があります）"
            );
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!("tokio を起動できません: {e}");
            return ExitCode::FAILURE;
        }
    };
    let catalog = match &ingest {
        Some(ingest) => catalog.with_writer(ingest.writer.clone()),
        None => catalog,
    };
    let config = Arc::new(config);
    // 同期サイクルとキャッシュ保存のスレッド。HTTP サーバーの起動を待たない
    let stop = StopFlag::new();
    let threads = ingest.as_ref().map(|ingest| IngestThreads {
        worker: crate::sync::spawn_worker(
            Arc::clone(&config),
            ingest.writer.clone(),
            Arc::clone(&ingest.cache),
            crate::sync::default_stages(),
            stop.clone(),
            config.drive_poll(),
            ingest.lock_hold(),
        ),
        saver: crate::cache::spawn_saver(
            Arc::clone(&ingest.cache),
            stop.clone(),
            ingest.lock_hold(),
        ),
    });
    let state = AppState::new(Arc::clone(&config), catalog);
    if state.data_root.as_path() != state.config.data_dir.as_path() {
        // データ置き場がシンボリックリンクのとき、解決先の内側かどうかで静的配信を判定する
        tracing::info!(
            "データ置き場はリンクを辿って {} に解決されました",
            state.data_root.display()
        );
    }
    let result = runtime.block_on(crate::server::serve(state));
    // 取り込みのスレッドを止める（サイクルの最中なら今の段が終わるまで。待つのは限られた時間だけ）
    stop.stop();
    if let (Some(threads), Some(ingest)) = (threads, &ingest) {
        stop_ingest_threads(threads, &ingest.cache, THREAD_STOP_LIMIT);
    }
    // 期限内に止まらなかったスレッドが残っていても、`.ingest.lock` はそのスレッドが終わる（プロセスが
    // 終わる）まで外れない（スレッドが `LockHold` を持つ）。ここで Ingest を捨てても二重には書かれない
    drop(ingest);
    // 固まった spawn_blocking（DB の読み取り）を待ち続けて、停止が終わらないことがないように
    runtime.shutdown_timeout(Duration::from_secs(5));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("サーバーが異常終了しました: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `geoframe sync [--dry-run]`: 同期サイクルを 1 回だけ動かす（棚卸し §5 の手動トリガー）。
fn sync(dry_run: bool, data_dir: Option<PathBuf>) -> ExitCode {
    let overrides = Overrides {
        listen: None,
        data_dir,
    };
    let config = match Config::load(|key| std::env::var(key).ok(), &overrides) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("geoframe: 設定が不正です: {e}");
            return ExitCode::from(2);
        }
    };
    crate::logging::init(config.log_level);
    crate::logging::install_panic_hook();
    if dry_run {
        sync_dry_run(&config)
    } else {
        sync_apply(&config)
    }
}

/// 書かずに計画を出す。書き込み接続を作らず、DB は読み取り専用、キャッシュは凍結して読む。
fn sync_dry_run(config: &Config) -> ExitCode {
    println!(
        "dry-run: 何も書きません（行・ファイル・キャッシュ・ロックは変えません。SQLite の WAL 関連ファイルだけはできうります）"
    );
    let read = match crate::db::open_read_only(&config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("geoframe: DB を読み取り用に開けません: {e}");
            return ExitCode::FAILURE;
        }
    };
    match crate::ingest::plan(config, &read) {
        Ok(plan) => {
            let s = &plan.schema;
            println!(
                "スキーマ: 作る表・索引 {} / 足す列 {} / place を NULL に戻す {} / embedding の写し {}",
                s.missing_objects.len(),
                s.missing_columns.len(),
                if s.would_reset_places {
                    "あり"
                } else {
                    "なし"
                },
                s.embeddings_to_copy
            );
            println!(
                "掃除: 消す .dl_* {} 件 / 捨てる .cache.json の旧キー {} 件",
                plan.leftover_files, plan.legacy_cache_keys
            );
        }
        Err(e) => {
            eprintln!("geoframe: 計画を作れません: {e}");
            return ExitCode::FAILURE;
        }
    }
    let cache = Cache::load_frozen(&config.data_dir);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::DryRun,
        config,
        read: &read,
        writer: None,
        cache: &cache,
        http: &http,
        stop: &stop,
    };
    let report = crate::sync::run_cycle(&crate::sync::default_stages(), &ctx);
    print_report(&report);
    exit_for(&report)
}

/// 実際に 1 回書く。`GEOFRAME_INGEST=1` が要る（P5 の切替まで、本番の書き手は Python）。
fn sync_apply(config: &Config) -> ExitCode {
    if !config.ingest {
        eprintln!(
            "geoframe: sync は GEOFRAME_INGEST=1 のときだけ書きます（書かずに計画だけ見るなら --dry-run）"
        );
        return ExitCode::from(2);
    }
    let ingest = match crate::ingest::start(config) {
        Ok(ingest) => ingest,
        Err(e) => {
            eprintln!("geoframe: {e}");
            return exit_for_ingest_error(&e);
        }
    };
    let read = match crate::db::open_read_only(&config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("geoframe: DB を読み取り用に開けません: {e}");
            return ExitCode::FAILURE;
        }
    };
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::Apply,
        config,
        read: &read,
        writer: Some(&ingest.writer),
        cache: &ingest.cache,
        http: &http,
        stop: &stop,
    };
    let report = crate::sync::run_cycle(&crate::sync::default_stages(), &ctx);
    ingest.cache.save_now();
    print_report(&report);
    exit_for(&report)
}

/// 取り込みを始められなかったときの終了コード。ガードが断ったのは 2（設定の誤りと同じ扱いで、
/// unit の `RestartPreventExitStatus=2` により再起動ループにしない。再起動しても直らない）。
fn exit_for_ingest_error(e: &crate::ingest::IngestError) -> ExitCode {
    match e {
        crate::ingest::IngestError::Refused(_) => ExitCode::from(2),
        _ => ExitCode::FAILURE,
    }
}

/// 段ごとの結果と件数だけを出す（値は持っていない）。
fn print_report(report: &CycleReport) {
    for stage in &report.stages {
        let state = match &stage.result {
            StageResult::Done(counts) if counts.is_empty() => "完了（件数なし）".to_string(),
            StageResult::Done(counts) => format!("完了 {counts}"),
            StageResult::Skipped(reason) => format!("省略（{reason}）"),
            StageResult::Failed(message) => format!("失敗: {message}"),
            StageResult::Panicked => "パニック".to_string(),
            StageResult::NotRun => "未実行".to_string(),
        };
        println!("段 {}: {state}", stage.name);
    }
    if let Some(p) = report.photos {
        println!(
            "写真: 表示可能 {} / 取得待ち {} / 全{}",
            p.ready, p.pending, p.total
        );
    }
}

fn exit_for(report: &CycleReport) -> ExitCode {
    if report.failed() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_commands() {
        assert_eq!(p(&["--version"]), Ok(Command::Version));
        assert_eq!(
            p(&["check-db", "a.db"]),
            Ok(Command::CheckDb(PathBuf::from("a.db")))
        );
        assert_eq!(
            p(&["serve", "--listen", "127.0.0.1:18080"]),
            Ok(Command::Serve(Overrides {
                listen: Some("127.0.0.1:18080".parse().unwrap()),
                data_dir: None,
            }))
        );
        assert_eq!(
            p(&["serve", "--data-dir", "/srv/x", "--listen", "[::1]:9"]),
            Ok(Command::Serve(Overrides {
                listen: Some("[::1]:9".parse().unwrap()),
                data_dir: Some(PathBuf::from("/srv/x")),
            }))
        );
        // 引数なしでも起動できる（環境変数と既定値で決まる）
        assert_eq!(p(&["serve"]), Ok(Command::Serve(Overrides::default())));
        assert_eq!(p(&["selfcheck-net"]), Ok(Command::SelfCheckNet));
        assert_eq!(
            p(&["sync"]),
            Ok(Command::Sync {
                dry_run: false,
                data_dir: None
            })
        );
        assert_eq!(
            p(&["sync", "--dry-run", "--data-dir", "/srv/x"]),
            Ok(Command::Sync {
                dry_run: true,
                data_dir: Some(PathBuf::from("/srv/x"))
            })
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
        assert!(p(&["check-db"]).is_err());
        assert!(p(&["check-db", "a", "b"]).is_err());
        assert!(p(&["serve", "--listen"]).is_err());
        assert!(p(&["serve", "--listen", "bad"]).is_err());
        // 不正な値はエラーメッセージに書かない
        let err = p(&["serve", "--listen", "secret-host:99999"]).unwrap_err();
        assert!(!err.contains("secret-host"), "{err}");
        assert!(p(&["serve", "--data-dir"]).is_err());
        assert!(p(&["serve", "--port", "1"]).is_err());
        assert!(p(&["selfcheck-net", "x"]).is_err());
        assert!(p(&["sync", "--listen", "127.0.0.1:1"]).is_err());
        assert!(p(&["sync", "--data-dir"]).is_err());
    }

    /// 停止の順序: worker が止まる直前の最後の `put` も、最後の保存に入る。
    #[test]
    fn the_final_save_waits_for_the_worker_to_finish() {
        use crate::testutil::TempDir;
        use serde_json::json;

        let dir = TempDir::new("cli-stop-order");
        let cache = Arc::new(Cache::load(dir.path()));
        let stop = StopFlag::new();
        let saver = crate::cache::spawn_saver(
            Arc::clone(&cache),
            stop.clone(),
            crate::ingest::LockHold::for_test(dir.path()),
        );
        // 偽の worker: 合図を見てから、少し遅れて最後の更新をする（saver が先に終わっても間に合う長さ）
        let worker = {
            let (cache, stop) = (Arc::clone(&cache), stop.clone());
            std::thread::spawn(move || {
                stop.wait(Duration::from_secs(30));
                std::thread::sleep(Duration::from_millis(300));
                cache.put("geo:late", json!(1));
            })
        };
        stop.stop();
        stop_ingest_threads(
            IngestThreads { worker, saver },
            &cache,
            Duration::from_secs(10),
        );
        let saved = std::fs::read_to_string(dir.path().join(".cache.json")).unwrap();
        assert!(saved.contains("geo:late"), "{saved}");
    }
    /// 保存スレッドが停止の期限を超えて残っている間は、`Ingest` を drop しても `.ingest.lock` が外れず、
    /// 別の取り込みを始められない。スレッドが終わったら取れる（故障注入: 止まったままの保存スレッド）。
    #[test]
    fn the_ingest_lock_stays_while_a_stuck_saver_remains() {
        use crate::config::Overrides;
        use crate::guard::Guard;
        use crate::ingest::{IngestError, start_with};
        use crate::testutil::TempDir;

        let dir = TempDir::new("cli-stuck-saver");
        let overrides = Overrides {
            listen: None,
            data_dir: Some(dir.path().to_path_buf()),
        };
        let config = Config::load(|_| None, &overrides).unwrap();
        let ingest = start_with(&config, &Guard::skip()).unwrap();

        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, running) = std::sync::mpsc::channel::<()>();
        let hold = ingest.lock_hold();
        let saver = std::thread::spawn(move || {
            let _lock = hold; // 本物の保存スレッドと同じく、終わるまで持つ
            started.send(()).unwrap();
            let _ = wait.recv(); // I/O で固まっているつもり
        });
        running.recv().unwrap();
        let worker = std::thread::spawn(|| {});
        stop_ingest_threads(
            IngestThreads { worker, saver },
            &ingest.cache,
            Duration::from_millis(100),
        );
        // serve の末尾と同じく Ingest を捨てる。固まった保存スレッドはまだ生きている
        drop(ingest);
        assert!(matches!(
            start_with(&config, &Guard::skip()),
            Err(IngestError::Locked)
        ));
        release.send(()).unwrap();
        for _ in 0..200 {
            match start_with(&config, &Guard::skip()) {
                Ok(_) => return,
                Err(IngestError::Locked) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("{e}"),
            }
        }
        panic!("保存スレッドが終わってもロックが外れない");
    }
}
