//! コマンドライン。依存を増やさないため引数は手で解釈する（P1 で必要なら見直す）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use crate::ai::jobs::{self, JobKind};
use crate::ai::ops::{self, PauseTarget, RetryTarget};
use crate::ai::settings::TaskKind;
use crate::ai::worker::{self, Clock, WorkerDeps};
use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::db::Catalog;
use crate::ingest::{Ingest, LockHold};
use crate::server::AppState;
use crate::stop::{StopFlag, join_within};
use crate::sync::{CycleReport, Mode, StageCtx, StageResult};

/// 停止のときに、取り込みの各スレッド（同期・AI・キャッシュ保存・タイル掃除）を待つ 1 つあたりの上限。
/// 実際に待つのは、これと SIGTERM からの共通期限（[`StopFlag::phase_limit`]）の残りの短いほう。
const THREAD_STOP_LIMIT: Duration = Duration::from_secs(10);
/// 停止の最後の runtime の停止を待つ上限（共通期限の残りでも制限する）。
const RUNTIME_STOP_LIMIT: Duration = Duration::from_secs(5);

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
/// 最後の保存（`Cache::save_final`）は `LockHold` を持つ別スレッドで行い、ここは `limit` までしか待たない。
/// saver が I/O の最中に期限を超えて残った場合も、保存の排他で重ねて書かない。排他の待ちだけでなく、取れたあとの
/// 書き込み・`fsync` が固まっても、期限が来たら最後の保存は省略して戻る（警告を出す）。省略した分は、
/// 残ったスレッドの保存が終わったあとの内容に含まれないが、書きかけの一時ファイルと競合して JSON を
/// 壊すよりは、キャッシュ（位置の逆引き結果で、作り直せる）の直近の更新を失うほうを選ぶ。
/// 期限を超えて残ったスレッドは `.ingest.lock` を持ち続ける（`LockHold`）ので、そのあいだ別の
/// 取り込みは始まらない。
/// 呼ぶ前に `stop` の合図を出しておくこと。各待ちは `limit` と、`stop` の共通期限の残りのうち短いほうまで
/// （SIGTERM から 60 秒を越えない）。
fn stop_ingest_threads(
    threads: IngestThreads,
    cache: &Arc<Cache>,
    lock: LockHold,
    limit: Duration,
    stop: &StopFlag,
) {
    for (name, handle) in [
        ("同期サイクル", threads.worker),
        ("キャッシュ保存", threads.saver),
    ] {
        if !join_within(handle, stop.phase_limit(limit)) {
            tracing::warn!("{name}のスレッドが時間内に止まりませんでした（そのまま終了します）");
        }
    }
    // 実際の保存は `lock` を持つ別スレッドで行い、ここは共通期限の残りまでしか待たない。省略したときの警告は
    // save_final の中で出す
    cache.save_final(lock, stop.phase_limit(limit));
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
  geoframe img-probe [--out <dir>] [--rotation <度>] [--jobs <n>] [--quality] <画像>...
                                        画像処理の確認・計測用。DB・データ置き場には触れない。画像を derivative にして
                                        （--out があれば <連番>.jpg で書き出す）、連番・形式・寸法・向き・ICC・所要時間を出す。
                                        ファイル名は出さない。HEIC が読めないときは構造の分類（ブランド・グリッド・コーデック等）が付く。
                                        --quality は derivative の代わりに品質の特徴量（SHA-256・pHash・鮮明度・露出）を出す
  geoframe sync [--dry-run] [--allow-mass-delete] [--data-dir <dir>]
                                        同期サイクルを 1 回だけ動かして終わる。--dry-run は行・写真・キャッシュ・
                                        ロックを変えず（SQLite が読み取りでも作る WAL 関連ファイル catalog.db-wal・
                                        -shm はできうる）、やるとしたら何か（スキーマ・掃除・各段）を件数だけ出す。
                                        Drive の鍵（GEOFRAME_DRIVE_KEY）があれば、一覧を読みに Drive へ接続する
                                        （原本のダウンロードはしない。鍵が無ければ Drive の段は通信せずに飛ばす。地名の段（enrich）は
                                        --dry-run では地理院に問い合わせず、件数だけを出す。Drive への通信を避けるなら、
                                        存在しない鍵ファイルの絶対パスを GEOFRAME_DRIVE_KEY に与える。--data-dir では避けられない）。
                                        Drive の一覧が空、または消す行が多すぎる（50 件超かつ Drive の行の 20% 超）ときは
                                        削除を保留する。意図した削除なら --allow-mass-delete を付けた 1 回だけで外す
                                        （serve の定期実行では外せない。--dry-run と併せれば、外した場合の件数を見られる）。
                                        --dry-run はガードを通さないが、本番機で実行してよいという意味ではない
                                        （検証環境の本番コピーに向ける。本番の DB には向けない）。
                                        書く実行は GEOFRAME_INGEST=1 が要り、serve と同じ印・Python の確認を通す
  geoframe organize [--dry-run] [--data-dir <dir>]
                                        位置の無いローカル写真を DATA_DIR/_excluded/no_gps/ へ移し、行を削除する
                                        （現行の server.py --organize）。先に index_local と位置の推定（infer）を 1 回ずつ
                                        動かし、推定で位置が付いた写真は移さない。同名のファイルは
                                        上書きせず「名前 (2).拡張子」にする。--dry-run は件数だけ。書く実行は
                                        GEOFRAME_INGEST=1 が要り、sync と同じ印・Python の確認を通す
  geoframe quality-scan [--dry-run] [--data-dir <dir>]
                                        品質の一括解析（現行の server.py --quality-scan）。孤児の特徴量を消し、未解析の写真を
                                        GEOFRAME_QUALITY_PER_CYCLE 件ずつ全部解析して、グループ（完全重複・類似）と推奨を
                                        作り直す。--dry-run は解析せず、件数（消す孤児・解析する枚数・再構築するか）だけ。
                                        書く実行は GEOFRAME_INGEST=1 が要り、sync と同じ印・Python の確認を通す
  geoframe ai-jobs status [--data-dir <dir>]
                                        AI ジョブ・用途の状態を件数と固定の理由コードだけで出す（読み取り専用。AI の表が無い
                                        旧 DB でも 0 件。対象・入力・モデル・接続先・パスは出さない）
  geoframe ai-jobs retry (--kind <種類> | --all) [--data-dir <dir>]
                                        失敗（failed）のジョブだけを、現在の入力・設定・既存の結果に照らして再検査し、
                                        再実行待ち／無効／成功済みに振り分ける（外部へは送らない）。検査できない種類
                                        （用途別の処理が未登録・設定で使えない用途）は戻さず件数だけ報告する。
                                        種類: image_embedding / location_clue / location_suggest / location_geocode / photo_judge
  geoframe ai-jobs release-pause (--task <用途> | --all) [--data-dir <dir>]
                                        Provider・用途の停止（認証・契約の不一致で自動的に止まったもの）を明示的に解除する。
                                        用途: image_embedding / query_embedding / photo_judge / location_clue / location_suggest。
                                        --task は今の設定でその用途に当たる停止だけ、--all は全部
  geoframe ai-jobs deactivate-space [--data-dir <dir>]
                                        有効な検索空間（settings.ai_active_space）を外し、旧結果参照へ戻す。旧結果・
                                        新しい空間・ベクトル・ジョブは消さない
                                        retry・release-pause・deactivate-space は書くので、サービスを止めてから実行する
                                        （稼働中は .ingest.lock により断る）。GEOFRAME_INGEST=1 と、sync と同じ印・Python の
                                        確認が要る
終了コード: 0 成功 / 1 失敗 / 2 引数・設定の誤り、または取り込みを断った（本番への誤指定の防止）";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    CheckDb(PathBuf),
    SelfCheckNet,
    ImgProbe(crate::imaging::probe::ProbeArgs),
    Serve(Overrides),
    Sync {
        dry_run: bool,
        /// Drive の大量削除の保留を、この 1 回だけ外す。
        allow_mass_delete: bool,
        data_dir: Option<PathBuf>,
    },
    Organize {
        dry_run: bool,
        data_dir: Option<PathBuf>,
    },
    QualityScan {
        dry_run: bool,
        data_dir: Option<PathBuf>,
    },
    AiJobs(AiJobsCommand),
}

/// `geoframe ai-jobs …`。
#[derive(Debug, PartialEq, Eq)]
pub enum AiJobsCommand {
    Status {
        data_dir: Option<PathBuf>,
    },
    Retry {
        target: RetryTarget,
        data_dir: Option<PathBuf>,
    },
    ReleasePause {
        target: PauseTarget,
        data_dir: Option<PathBuf>,
    },
    DeactivateSpace {
        data_dir: Option<PathBuf>,
    },
}

fn parse_ai_jobs<I: Iterator<Item = String>>(mut args: I) -> Result<AiJobsCommand, String> {
    let sub = args.next().ok_or(
        "ai-jobs にはサブコマンドが要ります（status・retry・release-pause・deactivate-space）",
    )?;
    let (mut data_dir, mut all, mut kind, mut task) = (None, false, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    args.next().ok_or("--data-dir に値が要ります")?,
                ));
            }
            "--all" if sub == "retry" || sub == "release-pause" => all = true,
            "--kind" if sub == "retry" => {
                let value = args.next().ok_or("--kind に値が要ります")?;
                kind =
                    Some(JobKind::parse(&value).ok_or(
                        "--kind の値が不正です（image_embedding など。--help の一覧を参照）",
                    )?);
            }
            "--task" if sub == "release-pause" => {
                let value = args.next().ok_or("--task に値が要ります")?;
                task =
                    Some(TaskKind::parse(&value).ok_or(
                        "--task の値が不正です（image_embedding など。--help の一覧を参照）",
                    )?);
            }
            other => return Err(format!("ai-jobs {sub} の引数 {other:?} は不明です")),
        }
    }
    match sub.as_str() {
        "status" => Ok(AiJobsCommand::Status { data_dir }),
        "retry" => match (all, kind) {
            (true, None) => Ok(AiJobsCommand::Retry {
                target: RetryTarget::All,
                data_dir,
            }),
            (false, Some(kind)) => Ok(AiJobsCommand::Retry {
                target: RetryTarget::Kind(kind),
                data_dir,
            }),
            _ => Err("ai-jobs retry には --kind <種類> か --all のどちらか 1 つが要ります".into()),
        },
        "release-pause" => match (all, task) {
            (true, None) => Ok(AiJobsCommand::ReleasePause {
                target: PauseTarget::All,
                data_dir,
            }),
            (false, Some(task)) => Ok(AiJobsCommand::ReleasePause {
                target: PauseTarget::Task(task),
                data_dir,
            }),
            _ => Err(
                "ai-jobs release-pause には --task <用途> か --all のどちらか 1 つが要ります"
                    .into(),
            ),
        },
        "deactivate-space" => Ok(AiJobsCommand::DeactivateSpace { data_dir }),
        other => Err(format!("ai-jobs のサブコマンド {other:?} は不明です")),
    }
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
        Some("img-probe") => crate::imaging::probe::parse(args).map(Command::ImgProbe),
        Some("ai-jobs") => parse_ai_jobs(args).map(Command::AiJobs),
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
        Some(name @ ("sync" | "organize" | "quality-scan")) => {
            let (mut dry_run, mut allow_mass_delete, mut data_dir) = (false, false, None);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--dry-run" => dry_run = true,
                    // 大量削除の保留を外すのは sync の明示的な 1 回だけ（serve・organize には与えない）
                    "--allow-mass-delete" if name == "sync" => allow_mass_delete = true,
                    "--data-dir" => {
                        let value = args.next().ok_or("--data-dir に値が要ります")?;
                        data_dir = Some(PathBuf::from(value));
                    }
                    other => return Err(format!("{name} の引数 {other:?} は不明です")),
                }
            }
            Ok(match name {
                "sync" => Command::Sync {
                    dry_run,
                    allow_mass_delete,
                    data_dir,
                },
                "organize" => Command::Organize { dry_run, data_dir },
                _ => Command::QualityScan { dry_run, data_dir },
            })
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
        Command::ImgProbe(args) => crate::imaging::probe::run(&args),
        Command::Serve(overrides) => serve(&overrides),
        Command::Sync {
            dry_run,
            allow_mass_delete,
            data_dir,
        } => sync(dry_run, allow_mass_delete, data_dir),
        Command::Organize { dry_run, data_dir } => organize(dry_run, data_dir),
        Command::QualityScan { dry_run, data_dir } => quality_scan(dry_run, data_dir),
        Command::AiJobs(command) => ai_jobs(command),
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
    // AI の設定が不正でも写真の配信・管理は続ける（新しい AI 呼び出しだけが無効）。パス・値は出さず、種類のコードだけ
    if let Some(code) = config.ai.invalid() {
        tracing::warn!(
            "AI の設定が不正です（種類: {code}）。新しい AI 呼び出しだけ無効にして続けます"
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
    // AI（取り込みが有効のときだけ）: 前の起動の running の回収と、AI が有効なときの背景ワーカー
    let ai_clock: Clock = Arc::new(jobs::unix_now);
    // 地名検索の `.cache.json`。取り込みが有効なら同期サイクルと同じキャッシュ（保存は保存スレッドが行う）。
    // 無効なら、読んでメモリに入れるだけでディスクへは書かない（本番の書き手は P5 の切替まで Python）
    let cache = match &ingest {
        Some(ingest) => Arc::clone(&ingest.cache),
        None => Arc::new(Cache::load_memory_only(&config.data_dir)),
    };
    let mut state = AppState::new(Arc::clone(&config), catalog).with_cache(cache);
    let cloud = crate::ai::cloud::CloudClients::new(&config.ai).ok();
    let lan = crate::ai::provider::LanClient::new(&config.ai);
    let ai_worker = ingest.as_ref().and_then(|ingest| {
        start_ai(
            &config,
            ingest,
            &stop,
            &ai_clock,
            worker::production_handlers(
                &config,
                lan.clone(),
                cloud.as_ref(),
                Arc::clone(&state.geocoder),
                Arc::clone(&state.cache),
            ),
        )
    });
    state = state.with_ai_search(Arc::new(crate::ai::query::Runtime::new(
        &config,
        lan,
        stop.clone(),
        ingest.as_ref().map(Ingest::lock_hold),
    )));
    // Drive の状態。大量削除の保留の記憶は、管理画面（サーバーの状態）と `index_drive` で同じものを使う
    let drive = Arc::new(crate::drive::DriveShared::new());
    let mass_delete = drive.mass_delete_board();
    let threads = ingest.as_ref().map(|ingest| IngestThreads {
        worker: crate::sync::spawn_worker(
            Arc::clone(&config),
            ingest.writer.clone(),
            Arc::clone(&ingest.cache),
            crate::sync::default_stages_with(drive),
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
    if ingest.is_some() {
        state = state.with_mass_delete_board(mass_delete);
    }
    if state.data_root.as_path() != state.config.data_dir.as_path() {
        // データ置き場がシンボリックリンクのとき、解決先の内側かどうかで静的配信を判定する
        tracing::info!(
            "データ置き場はリンクを辿って {} に解決されました",
            state.data_root.display()
        );
    }
    // 地図タイルのキャッシュの掃除（B4）。起動時に 1 回、以後 1 時間ごと。取り込みの有無に関わらず動く
    // （キャッシュは catalog.db に書かないので、writer が無くても掃除する）
    let pruner = crate::tiles::spawn_pruner(
        Arc::clone(&state.tiles),
        stop.clone(),
        crate::tiles::PRUNE_INTERVAL,
    );
    // SIGTERM の直後に共有の停止の合図が立つ（HTTP の終了待ちより前。起点は SIGTERM の受信時刻）。以降の各
    // 処理は SIGTERM から 60 秒の共通期限の残りまでしか待たない。bind に失敗してここへ来たときも、続けて
    // スレッドを止める（`stop` は冪等）
    let result = runtime.block_on(crate::server::serve(state, stop.clone()));
    // 取り込みのスレッドを止める（サイクルの最中なら今の段が終わるまで。待つのは限られた時間だけ）
    stop.stop();
    if !join_within(pruner, stop.phase_limit(THREAD_STOP_LIMIT)) {
        tracing::warn!(
            "タイルキャッシュの掃除のスレッドが時間内に止まりませんでした（そのまま終了します）"
        );
    }
    if let (Some(ai), Some(ingest)) = (ai_worker, &ingest) {
        // join の間は token が有効（有効な応答は確定される）。終わらなければ戻して token を失効させる
        ai.shutdown(&ingest.writer, &stop, THREAD_STOP_LIMIT, &ai_clock);
    }
    if let (Some(threads), Some(ingest)) = (threads, &ingest) {
        stop_ingest_threads(
            threads,
            &ingest.cache,
            ingest.lock_hold(),
            THREAD_STOP_LIMIT,
            &stop,
        );
    }
    // 期限内に止まらなかったスレッドが残っていても、`.ingest.lock` はそのスレッドが終わる（プロセスが
    // 終わる）まで外れない（スレッドが `LockHold` を持つ）。ここで Ingest を捨てても二重には書かれない
    drop(ingest);
    // 固まった spawn_blocking（DB の読み取り）を待ち続けて、停止が終わらないことがないように
    runtime.shutdown_timeout(stop.phase_limit(RUNTIME_STOP_LIMIT));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("サーバーが異常終了しました: {e}");
            ExitCode::FAILURE
        }
    }
}

/// AI の起動の準備と、背景ワーカーの起動。取り込みが有効（ロックを取ったあと）の `serve` だけが呼ぶ。
/// 回収と Provider 停止の整理は、ワーカーが無効でも行う（AI を無効に戻したあとに `running` が残り続けない
/// ように）。ワーカーは `GEOFRAME_AI_ENABLED=1` で AI の設定が不正でないときだけ（Provider のジョブは有効な背景用途があるときだけ処理する）。
fn start_ai(
    config: &Arc<Config>,
    ingest: &Ingest,
    stop: &StopFlag,
    clock: &Clock,
    handlers: Vec<Arc<dyn worker::Handler>>,
) -> Option<worker::AiWorker> {
    let started = worker::startup(config, &ingest.writer, clock)?;
    if !worker::should_start(config) {
        tracing::info!("[ai] 背景ワーカーは起動しません（AI が無効、または AI の設定が不正です）");
        return None;
    }
    tracing::info!(
        "[ai] 背景ワーカーを起動します（Provider 用途 {} 件と、同一場面の局所計算）",
        worker::background_tasks(&config.ai, true).len()
    );
    Some(worker::spawn(WorkerDeps {
        config: Arc::clone(config),
        writer: ingest.writer.clone(),
        stop: stop.clone(),
        lock: ingest.lock_hold(),
        handlers,
        clock: Arc::clone(clock),
        poll: config.ai.legacy().poll(),
        run_id: started.run_id,
    }))
}

/// `geoframe sync [--dry-run]`: 同期サイクルを 1 回だけ動かす（棚卸し §5 の手動トリガー）。
fn sync(dry_run: bool, allow_mass_delete: bool, data_dir: Option<PathBuf>) -> ExitCode {
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
        sync_dry_run(&config, allow_mass_delete)
    } else {
        sync_apply(&config, allow_mass_delete)
    }
}

/// `index_drive` の段を含む段の一覧。`--allow-mass-delete` の 1 回だけ、大量削除の保留を外す。
fn stages_for(allow_mass_delete: bool) -> Vec<Box<dyn crate::sync::Stage>> {
    if allow_mass_delete {
        crate::sync::default_stages_allowing_mass_delete()
    } else {
        crate::sync::default_stages()
    }
}

/// 書かずに計画を出す。書き込み接続を作らず、DB は読み取り専用、キャッシュは凍結して読む。
fn sync_dry_run(config: &Config, allow_mass_delete: bool) -> ExitCode {
    println!(
        "dry-run: 何も書きません（行・ファイル・キャッシュ・ロックは変えません。SQLite の WAL 関連ファイルだけはできうります）。\
         Drive の鍵があれば一覧を読みに接続します（原本のダウンロードはしません）"
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
            if !plan.scene_mismatch.is_empty() {
                println!(
                    "AI の場面の表: 列が想定と違う表 {} 個（足す予定には数えません。直さず、場面は使われません。派生の表を落として再作成するまで）",
                    plan.scene_mismatch.len()
                );
            }
            println!(
                "掃除: 消す .dl_* {} 件 / 消す書きかけの .part {} 件 / 捨てる .cache.json の旧キー {} 件",
                plan.leftover_files, plan.part_files, plan.legacy_cache_keys
            );
            if plan.skipped_dirs > 0 {
                println!(
                    "掃除を飛ばす tmp/・derivatives/（リンクなどで通常のディレクトリではない）: {} つ",
                    plan.skipped_dirs
                );
            }
        }
        Err(e) => {
            eprintln!("geoframe: 計画を作れません: {e}");
            return ExitCode::FAILURE;
        }
    }
    // 後続の段は、先の段が済んだあとの状態で数える（先の段が自分の計画を写しに適用する）
    let sim = match crate::sync::simulation_copy(&read) {
        Ok(sim) => sim,
        Err(e) => {
            eprintln!("geoframe: DB のメモリ上の写しを作れません: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "後続の段は、先の段が済んだあとの状態（メモリ上の写し）で数えます。「写真:」の行も同じです"
    );
    let cache = Cache::load_frozen(&config.data_dir);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::DryRun,
        config,
        read: &sim,
        writer: None,
        cache: &cache,
        http: &http,
        stop: &stop,
    };
    if allow_mass_delete {
        println!("--allow-mass-delete: Drive の大量削除の保留を外した場合の件数を数えます");
    }
    let report = crate::sync::run_cycle(&stages_for(allow_mass_delete), &ctx);
    print_report(&report);
    exit_for(&report)
}

/// 実際に 1 回書く。`GEOFRAME_INGEST=1` が要る（P5 の切替まで、本番の書き手は Python）。
fn sync_apply(config: &Config, allow_mass_delete: bool) -> ExitCode {
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
    let report = crate::sync::run_cycle(&stages_for(allow_mass_delete), &ctx);
    ingest.cache.save_now();
    print_report(&report);
    exit_for(&report)
}

/// `geoframe organize [--dry-run]`: `index_local` を 1 回動かしてから、位置の無いローカル写真を
/// `_excluded/no_gps/` へ移す（現行の `server.py --organize`）。書く実行は `sync` と同じ条件と
/// ガードを通す。
fn organize(dry_run: bool, data_dir: Option<PathBuf>) -> ExitCode {
    one_off(
        "organize",
        dry_run,
        data_dir,
        "organize の対象は、index_local が済んだあとの状態（メモリ上の写し）で数えます",
        |ctx| {
            // 先に index_local → infer（現行の organize_photos と同じ順序）。その結果を使って移す。
            // どちらかが失敗・省略されたときは移さない（古い行を信じて、まだある写真・位置が付く写真を動かさないため）
            let mut failed = false;
            for stage in [
                &crate::local::IndexLocal as &dyn crate::sync::Stage,
                &crate::geo::Infer,
            ] {
                if failed {
                    break;
                }
                let name = stage.name();
                match stage.run(ctx) {
                    Ok(crate::sync::StageOutcome::Done(counts)) => {
                        println!("{name}: 完了 {counts}")
                    }
                    Ok(crate::sync::StageOutcome::Skipped(reason)) => {
                        println!("{name}: 省略（{reason}）");
                        failed = true;
                    }
                    Err(e) => {
                        println!("{name}: 失敗: {e}");
                        failed = true;
                    }
                }
            }
            if !failed {
                match crate::local::organize(ctx) {
                    Ok(counts) => println!("organize: 完了 {counts}"),
                    Err(e) => {
                        println!("organize: 失敗: {e}");
                        failed = true;
                    }
                }
            }
            if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        },
    )
}

/// `geoframe quality-scan [--dry-run]`: 孤児の特徴量を消し、未解析の写真を**全部**解析して、品質を
/// 再構築する（現行の `server.py --quality-scan`）。書く実行は `sync` と同じ条件とガードを通す。
fn quality_scan(dry_run: bool, data_dir: Option<PathBuf>) -> ExitCode {
    one_off(
        "quality-scan",
        dry_run,
        data_dir,
        "解析（重い処理）は実行せず、件数だけを数えます",
        |ctx| match crate::quality::scan(ctx) {
            Ok(crate::sync::StageOutcome::Done(counts)) => {
                println!("quality-scan: 完了 {counts}");
                ExitCode::SUCCESS
            }
            Ok(crate::sync::StageOutcome::Skipped(reason)) => {
                println!("quality-scan: 省略（{reason}）");
                ExitCode::FAILURE
            }
            Err(e) => {
                println!("quality-scan: 失敗: {e}");
                ExitCode::FAILURE
            }
        },
    )
}

/// `organize`・`quality-scan` のような、段を 1 回だけ動かして終わるコマンドの共通部分。設定を読み、
/// 書く実行ではガードとロックを通して書き込み接続を作り、`--dry-run` では DB のメモリ上の写しを作って、
/// `body` に渡す。
fn one_off(
    command: &str,
    dry_run: bool,
    data_dir: Option<PathBuf>,
    dry_run_note: &str,
    body: impl FnOnce(&StageCtx<'_>) -> ExitCode,
) -> ExitCode {
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
    let ingest = if dry_run {
        println!(
            "dry-run: 何も書きません（行・ファイルは変えません。SQLite の WAL 関連ファイルだけはできうります）。\
             {dry_run_note}"
        );
        None
    } else {
        if !config.ingest {
            eprintln!(
                "geoframe: {command} は GEOFRAME_INGEST=1 のときだけ動きます（書かずに件数だけ見るなら --dry-run）"
            );
            return ExitCode::from(2);
        }
        match crate::ingest::start(&config) {
            Ok(ingest) => Some(ingest),
            Err(e) => {
                eprintln!("geoframe: {e}");
                return exit_for_ingest_error(&e);
            }
        }
    };
    let read = match crate::db::open_read_only(&config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("geoframe: DB を読み取り用に開けません: {e}");
            return ExitCode::FAILURE;
        }
    };
    // dry-run は、先の段が自分の計画を適用した写しの上で数える
    let sim;
    let read = if dry_run {
        sim = match crate::sync::simulation_copy(&read) {
            Ok(sim) => sim,
            Err(e) => {
                eprintln!("geoframe: DB のメモリ上の写しを作れません: {e}");
                return ExitCode::FAILURE;
            }
        };
        &sim
    } else {
        &read
    };
    let cache = match &ingest {
        Some(ingest) => Arc::clone(&ingest.cache),
        None => Arc::new(Cache::load_frozen(&config.data_dir)),
    };
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: if dry_run { Mode::DryRun } else { Mode::Apply },
        config: &config,
        read,
        writer: ingest.as_ref().map(|i| &i.writer),
        cache: &cache,
        http: &http,
        stop: &stop,
    };
    body(&ctx)
}

/// `geoframe ai-jobs …`。`status` は読み取りだけ（ガード・ロック不要。AI の表が無い旧 DB でも 0 件）。
/// `retry`・`release-pause` は書くので、`organize` などと同じく `GEOFRAME_INGEST=1`・検証環境のガード・
/// `.ingest.lock`・writer を通す（稼働中の `serve` はロックを持っているので断る）。出力は件数・用途・固定の
/// 理由コードだけで、対象のキー・入力・token・起動 ID・モデル・URL・パス・写真の値は出さない。
fn ai_jobs(command: AiJobsCommand) -> ExitCode {
    let data_dir = match &command {
        AiJobsCommand::Status { data_dir }
        | AiJobsCommand::Retry { data_dir, .. }
        | AiJobsCommand::ReleasePause { data_dir, .. }
        | AiJobsCommand::DeactivateSpace { data_dir } => data_dir.clone(),
    };
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
    if let AiJobsCommand::Status { .. } = command {
        return ai_jobs_status(&config);
    }
    if !config.ingest {
        eprintln!(
            "geoframe: ai-jobs の retry・release-pause・deactivate-space は GEOFRAME_INGEST=1 のときだけ動きます（読み取りだけなら ai-jobs status）"
        );
        return ExitCode::from(2);
    }
    let ingest = match crate::ingest::start(&config) {
        Ok(ingest) => ingest,
        Err(e) => {
            eprintln!("geoframe: {e}");
            if matches!(e, crate::ingest::IngestError::Locked) {
                eprintln!("geoframe: サービスを止めてから実行してください（稼働中は書きません）");
            }
            return exit_for_ingest_error(&e);
        }
    };
    let clock: Clock = Arc::new(jobs::unix_now);
    match command {
        AiJobsCommand::Retry { target, .. } => ai_jobs_retry(&config, &ingest, target, &clock),
        AiJobsCommand::ReleasePause { target, .. } => {
            ai_jobs_release_pause(&config, &ingest, target)
        }
        AiJobsCommand::DeactivateSpace { .. } => ai_jobs_deactivate_space(&ingest),
        AiJobsCommand::Status { .. } => ExitCode::SUCCESS,
    }
}

fn ai_jobs_status(config: &Config) -> ExitCode {
    let read = match crate::db::open_read_only(&config.db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("geoframe: DB を読み取り用に開けません: {e}");
            return ExitCode::FAILURE;
        }
    };
    match ops::report(&read, &config.ai) {
        Ok(report) => {
            print!("{}", format_ai_report(&report));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("geoframe: AI ジョブの状態を読めません: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `ai-jobs status` の出力（件数・用途・固定コードだけ）。
fn format_ai_report(report: &ops::Report) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let config = match report.config {
        ops::ConfigState::Unset => "未指定（新しい外部 AI 呼び出しは無効）".to_string(),
        ops::ConfigState::Invalid(code) => {
            format!("不正（種類: {code}。新しい AI 呼び出しだけ無効）")
        }
        ops::ConfigState::Ready => "有効".to_string(),
    };
    let _ = writeln!(out, "AI 設定: {config}");
    for (task, state) in &report.tasks {
        match state {
            Ok(()) => {
                let _ = writeln!(out, "用途 {}: 使える", task.as_str());
            }
            Err(reason) => {
                let _ = writeln!(
                    out,
                    "用途 {}: 使えない（{}）",
                    task.as_str(),
                    reason.as_str()
                );
            }
        }
    }
    if report.jobs.is_empty() {
        let _ = writeln!(out, "ジョブ: 0 件");
    }
    for (kind, state, count) in &report.jobs {
        let _ = writeln!(
            out,
            "ジョブ {} {}: {count} 件",
            kind.as_str(),
            state.as_str()
        );
    }
    for (state, code, count) in &report.reasons {
        let _ = writeln!(out, "理由 {} {code}: {count} 件", state.as_str());
    }
    for (code, count) in &report.location_holds {
        let _ = writeln!(out, "日候補 保留 {code}: {count} 件");
    }
    for w in &report.waits.waits {
        let scope = match w.scope {
            crate::ai::waits::WaitScope::Entrance => "入口全体",
            crate::ai::waits::WaitScope::Service => "サービス単体",
        };
        let _ = writeln!(
            out,
            "待機中 {} {scope}: {}（期限 {} UTC）",
            w.task.as_str(),
            w.reason.as_str(),
            crate::ai::waits::utc_iso(w.until)
        );
    }
    if report.waits.unidentified > 0 {
        let _ = writeln!(
            out,
            "待機中 用途を特定できない（設定が使えない）: {} 件",
            report.waits.unidentified
        );
    }
    if report.waits.is_empty() {
        let _ = writeln!(out, "共有の一時待機: なし");
    }
    if report.pauses.is_empty() {
        let _ = writeln!(out, "停止中の Provider・用途: なし");
    }
    out.push_str(&format_index_status(&report.index));
    out.push_str(&format_building_status(report.building.as_ref()));
    for p in &report.pauses {
        let _ = writeln!(
            out,
            "停止中 {} {}: {}",
            p.scope.as_str(),
            p.task.as_str(),
            p.reason.as_str()
        );
    }
    out
}

/// `ai-jobs status` の検索索引の部分（件数と固定のコードだけ。空間の ID・モデル・SHA は出さない）。
fn format_index_status(status: &crate::ai::index::IndexStatus) -> String {
    use crate::ai::index::ActiveState;
    use std::fmt::Write;
    let mut out = String::new();
    let active = match status.active {
        ActiveState::NoTables => "なし（空間の表が無い。旧結果参照）",
        ActiveState::Unset => "なし（旧結果参照）",
        ActiveState::Active(_) => "あり",
        ActiveState::Unusable => "なし（指す空間が未登録・不正。旧結果参照）",
    };
    let p = &status.progress;
    let _ = writeln!(out, "検索空間: {active}（登録 {} 件）", status.spaces);
    if matches!(status.active, ActiveState::Active(_)) {
        let _ = writeln!(
            out,
            "検索索引: 対象 {} / 保存済み {} / 待ち {} / 失敗で除外 {} / 壊れ {}",
            p.eligible, p.indexed, p.pending, p.failed_excluded, status.corrupt
        );
        for (code, count) in &p.failed_reasons {
            let _ = writeln!(out, "検索索引 除外の理由 {code}: {count} 件");
        }
    }
    let _ = writeln!(
        out,
        "他の空間・旧結果のベクトル: {} 種類（うち旧結果 {}。新しい空間には使わない）",
        status.other_shas, status.legacy_shas
    );
    out
}

/// `ai-jobs status` の公開前の空間の部分（件数と固定のコードだけ）。設定（`GEOFRAME_AI_CONFIG`）が無い・空間が未登録・
/// 既に有効なときは何も出さない。保存済みはベクトルの有無だけを数える（検査は公開時）。
fn format_building_status(building: Option<&crate::ai::index::Building>) -> String {
    use crate::ai::index::PublishState;
    use std::fmt::Write;
    let Some(b) = building else {
        return String::new();
    };
    let p = &b.progress;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "公開前の空間: 対象 {} / 保存済み {} / 待ち {} / 失敗で除外 {}（成功率 {:.1}%）",
        p.eligible,
        p.indexed,
        p.pending,
        p.failed_excluded,
        b.coverage * 100.0
    );
    for (code, count) in &p.failed_reasons {
        let _ = writeln!(out, "公開前の空間 除外の理由 {code}: {count} 件");
    }
    let verdict = match b.publish {
        PublishState::Waiting => "待ち 0 になるまで公開しない",
        PublishState::NoVectors => "保存済みが 0 件のため公開しない",
        PublishState::BelowThreshold => "成功率が閾値に届かず公開しない（管理確認が要る）",
        PublishState::Ready => "公開できる状態（次の切替の試みで公開）",
    };
    let _ = writeln!(
        out,
        "公開の条件: 待ち 0 かつ 成功率 {:.0}% 以上 → {verdict}",
        b.min_coverage * 100.0
    );
    out
}

fn ai_jobs_retry(config: &Config, ingest: &Ingest, target: RetryTarget, clock: &Clock) -> ExitCode {
    // Construction only: retry never obtains a token or contacts any service.
    let cloud = crate::ai::cloud::CloudClients::new(&config.ai).ok();
    let handlers = worker::production_handlers(
        config,
        crate::ai::provider::LanClient::new(&config.ai),
        cloud.as_ref(),
        Arc::new(crate::geocode::Geocoder::production(&config.tile_ua)),
        Arc::clone(&ingest.cache),
    );
    match ops::retry(&ingest.writer, &config.ai, &handlers, target, clock) {
        Ok(r) => {
            println!(
                "再実行待ちに戻した {} / 無効にした {} / 成功済みとした {} / 検査できず保持（処理が未登録）{} / 検査できず保持（設定で使えない）{}",
                r.requeued, r.obsolete, r.satisfied, r.unsupported, r.policy_disabled
            );
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("geoframe: 再試行に失敗しました");
            ExitCode::FAILURE
        }
    }
}

fn ai_jobs_deactivate_space(ingest: &Ingest) -> ExitCode {
    match ingest.writer.call(crate::ai::activation::deactivate) {
        Ok(true) => {
            println!("有効な検索空間を外した（旧結果参照へ戻した）");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("有効な検索空間は設定されていない（何も変えていない）");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("geoframe: 検索空間の解除に失敗しました");
            ExitCode::FAILURE
        }
    }
}

fn ai_jobs_release_pause(config: &Config, ingest: &Ingest, target: PauseTarget) -> ExitCode {
    match ops::release_pause(&ingest.writer, &config.ai, target) {
        Ok(n) => {
            println!("停止を解除した: {n} 件");
            ExitCode::SUCCESS
        }
        Err(ops::ReleaseError::ConfigUnavailable) => {
            eprintln!(
                "geoframe: AI の設定が未指定・不正、またはその用途が未設定なので、今の設定に当たる停止を特定できません（全部を解除するなら --all）"
            );
            ExitCode::from(2)
        }
        Err(ops::ReleaseError::Write) => {
            eprintln!("geoframe: 停止の解除に失敗しました");
            ExitCode::FAILURE
        }
    }
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
                allow_mass_delete: false,
                data_dir: None
            })
        );
        assert_eq!(
            p(&["sync", "--dry-run", "--data-dir", "/srv/x"]),
            Ok(Command::Sync {
                dry_run: true,
                allow_mass_delete: false,
                data_dir: Some(PathBuf::from("/srv/x"))
            })
        );
        // 大量削除の保留を外すのは sync の明示的な引数だけ（serve・organize は受けない）
        assert_eq!(
            p(&["sync", "--allow-mass-delete", "--dry-run"]),
            Ok(Command::Sync {
                dry_run: true,
                allow_mass_delete: true,
                data_dir: None
            })
        );
        assert!(p(&["organize", "--allow-mass-delete"]).is_err());
        assert!(p(&["serve", "--allow-mass-delete"]).is_err());
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
        assert!(p(&["organize", "--listen", "127.0.0.1:1"]).is_err());
        assert!(p(&["organize", "--data-dir"]).is_err());
    }

    #[test]
    fn parses_quality_scan() {
        assert_eq!(
            p(&["quality-scan"]),
            Ok(Command::QualityScan {
                dry_run: false,
                data_dir: None
            })
        );
        assert_eq!(
            p(&["quality-scan", "--dry-run", "--data-dir", "/srv/x"]),
            Ok(Command::QualityScan {
                dry_run: true,
                data_dir: Some(PathBuf::from("/srv/x"))
            })
        );
        // 大量削除の保留を外すのは sync の明示的な引数だけ
        assert!(p(&["quality-scan", "--allow-mass-delete"]).is_err());
        assert!(p(&["quality-scan", "--listen", "127.0.0.1:1"]).is_err());
    }

    #[test]
    fn parses_organize() {
        assert_eq!(
            p(&["organize"]),
            Ok(Command::Organize {
                dry_run: false,
                data_dir: None
            })
        );
        assert_eq!(
            p(&["organize", "--dry-run", "--data-dir", "/srv/x"]),
            Ok(Command::Organize {
                dry_run: true,
                data_dir: Some(PathBuf::from("/srv/x"))
            })
        );
    }

    #[test]
    fn parses_ai_jobs() {
        use crate::ai::ops::{PauseTarget, RetryTarget};
        assert_eq!(
            p(&["ai-jobs", "status"]),
            Ok(Command::AiJobs(AiJobsCommand::Status { data_dir: None }))
        );
        assert_eq!(
            p(&["ai-jobs", "status", "--data-dir", "/srv/x"]),
            Ok(Command::AiJobs(AiJobsCommand::Status {
                data_dir: Some(PathBuf::from("/srv/x"))
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "retry", "--kind", "photo_judge"]),
            Ok(Command::AiJobs(AiJobsCommand::Retry {
                target: RetryTarget::Kind(JobKind::PhotoJudge),
                data_dir: None
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "retry", "--all", "--data-dir", "/srv/x"]),
            Ok(Command::AiJobs(AiJobsCommand::Retry {
                target: RetryTarget::All,
                data_dir: Some(PathBuf::from("/srv/x"))
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "release-pause", "--task", "image_embedding"]),
            Ok(Command::AiJobs(AiJobsCommand::ReleasePause {
                target: PauseTarget::Task(TaskKind::ImageEmbedding),
                data_dir: None
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "deactivate-space", "--data-dir", "/srv/x"]),
            Ok(Command::AiJobs(AiJobsCommand::DeactivateSpace {
                data_dir: Some("/srv/x".into())
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "deactivate-space"]),
            Ok(Command::AiJobs(AiJobsCommand::DeactivateSpace {
                data_dir: None
            }))
        );
        assert_eq!(
            p(&["ai-jobs", "release-pause", "--all"]),
            Ok(Command::AiJobs(AiJobsCommand::ReleasePause {
                target: PauseTarget::All,
                data_dir: None
            }))
        );
    }

    #[test]
    fn rejects_bad_ai_jobs_input_without_echoing_values() {
        for args in [
            &["ai-jobs"][..],
            &["ai-jobs", "nope"],
            // 対象は 1 つだけ。何も付けない・両方付ける・種類違いは断る（生の対象 ID は受け付けない）
            &["ai-jobs", "retry"],
            &["ai-jobs", "retry", "--all", "--kind", "photo_judge"],
            &["ai-jobs", "retry", "--kind"],
            &["ai-jobs", "retry", "--id", "7"],
            &["ai-jobs", "release-pause"],
            &["ai-jobs", "release-pause", "--all", "--task", "photo_judge"],
            &["ai-jobs", "release-pause", "--kind", "photo_judge"],
            &["ai-jobs", "status", "--all"],
            &["ai-jobs", "status", "--data-dir"],
            &["ai-jobs", "deactivate-space", "--all"],
            &["ai-jobs", "deactivate-space", "--space", "x"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
        let err = p(&["ai-jobs", "retry", "--kind", "secret-kind-value"]).unwrap_err();
        assert!(!err.contains("secret-kind-value"), "{err}");
        let err = p(&["ai-jobs", "release-pause", "--task", "secret-task-value"]).unwrap_err();
        assert!(!err.contains("secret-task-value"), "{err}");
    }

    /// `ai-jobs status` の出力は件数・用途・固定コードだけ。
    #[test]
    fn the_ai_jobs_status_output_has_only_counts_and_fixed_codes() {
        use crate::ai::jobs::{JobKey, enqueue};
        use crate::ai::pause::{self, PauseReason};
        use crate::testutil::TempDir;
        use crate::writer::{OpenMode, Writer};

        let dir = TempDir::new("cli-ai-status");
        let ai_json = dir.path().join("ai.json");
        std::fs::write(
            &ai_json,
            r#"{"version":1,"local_base_url":"http://192.0.2.55:9100",
                "tasks":{"location_clue":{"provider":"local","model":"secret-model-name"}}}"#,
        )
        .unwrap();
        let config = Config::load(
            |k| (k == "GEOFRAME_AI_CONFIG").then(|| ai_json.to_str().unwrap().to_string()),
            &Overrides {
                listen: None,
                data_dir: Some(dir.path().join("data")),
            },
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
        writer.call(crate::schema::migrate).unwrap();
        writer.call(crate::ai::migrate).unwrap();
        let settings = config.ai.settings().unwrap().clone();
        writer
            .call(move |c| {
                let k = JobKey::new(JobKind::LocationClue, "SECRET-TARGET", "SECRET-HASH", "p");
                enqueue(c, &k, 100)?;
                c.execute(
                    "UPDATE ai_jobs SET state='failed', last_error_code='interrupted',
                            claim_token='SECRET-TOKEN', run_id='SECRET-RUN'",
                    [],
                )?;
                let key = settings.provider_pause_key(TaskKind::LocationClue).unwrap();
                pause::pause(c, TaskKind::LocationClue, &key, PauseReason::Forbidden, 100)?;
                Ok(())
            })
            .unwrap();
        // 共有の一時待機（入口全体と、設定に当たらない古い待ち）
        let wait_settings = config.ai.settings().unwrap().clone();
        writer
            .call(move |c| {
                let now = crate::ai::jobs::unix_now();
                let entrance = crate::ai::waits::WaitKey {
                    provider: wait_settings
                        .provider_fingerprint(TaskKind::LocationClue)
                        .unwrap(),
                    service: None,
                };
                crate::ai::waits::set(
                    c,
                    &entrance,
                    crate::ai::waits::WaitReason::Leased,
                    now + 1_800,
                    now,
                )?;
                let stale = crate::ai::waits::WaitKey {
                    provider: "SECRET-OLD-PROVIDER".into(),
                    service: None,
                };
                crate::ai::waits::set(
                    c,
                    &stale,
                    crate::ai::waits::WaitReason::Unreachable,
                    now + 1_800,
                    now,
                )
            })
            .unwrap();
        let read = crate::db::open_read_only(&config.db_path()).unwrap();
        let report = ops::report(&read, &config.ai).unwrap();
        let out = format_ai_report(&report);
        assert!(
            out.contains("待機中 location_clue 入口全体: leased（期限 20")
                && out.contains(" UTC）"),
            "{out}"
        );
        assert!(!out.contains("共有の一時待機: なし"), "{out}");
        assert!(out.contains("ジョブ location_clue failed: 1 件"), "{out}");
        assert!(out.contains("理由 failed interrupted: 1 件"), "{out}");
        assert!(
            out.contains("停止中 provider location_clue: forbidden"),
            "{out}"
        );
        assert!(out.contains("用途 location_clue: 使える"), "{out}");
        assert!(
            out.contains("用途 photo_judge: 使えない（task_not_configured）"),
            "{out}"
        );
        for secret in [
            "SECRET",
            "192.0.2.55",
            "secret-model-name",
            "ai.json",
            dir.path().to_str().unwrap(),
        ] {
            assert!(!out.contains(secret), "{secret}\n{out}");
        }
    }

    /// 公開前の空間の進み具合は、件数と公開の条件（固定の文）だけを出す。無いときは何も出さない。
    #[test]
    fn building_space_progress_prints_counts_and_the_publish_rule_only() {
        use crate::ai::index::{Building, Progress, PublishState};
        assert_eq!(format_building_status(None), "");
        let building = Building {
            progress: Progress {
                eligible: 6439,
                indexed: 2000,
                pending: 4300,
                failed_excluded: 139,
                failed_reasons: vec![("timeout", 139)],
            },
            min_coverage: 0.95,
            coverage: 2000.0 / 6439.0,
            publish: PublishState::Waiting,
        };
        let out = format_building_status(Some(&building));
        assert!(
            out.contains("公開前の空間: 対象 6439 / 保存済み 2000 / 待ち 4300 / 失敗で除外 139（成功率 31.1%）"),
            "{out}"
        );
        assert!(
            out.contains("公開前の空間 除外の理由 timeout: 139 件"),
            "{out}"
        );
        assert!(
            out.contains("公開の条件: 待ち 0 かつ 成功率 95% 以上 → 待ち 0 になるまで公開しない"),
            "{out}"
        );
    }

    /// 停止の各待ちは、1 つあたりの上限が長くても SIGTERM からの共通期限の残りを越えない。
    #[test]
    fn the_stop_phases_never_wait_past_the_common_deadline() {
        use crate::testutil::TempDir;
        let dir = TempDir::new("cli-stop-deadline");
        let cache = Arc::new(Cache::load(dir.path()));
        let stop = StopFlag::with_budget(Duration::from_millis(300));
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (release2, wait2) = std::sync::mpsc::channel::<()>();
        // どちらのスレッドも止まらない（I/O で固まっているつもり）
        let worker = std::thread::spawn(move || {
            let _ = wait.recv();
        });
        let saver = std::thread::spawn(move || {
            let _ = wait2.recv();
        });
        stop.stop();
        let began = std::time::Instant::now();
        stop_ingest_threads(
            IngestThreads { worker, saver },
            &cache,
            crate::ingest::LockHold::for_test(dir.path()),
            Duration::from_secs(10),
            &stop,
        );
        let waited = began.elapsed();
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        release.send(()).unwrap();
        release2.send(()).unwrap();
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
            crate::ingest::LockHold::for_test(dir.path()),
            Duration::from_secs(10),
            &stop,
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
            ingest.lock_hold(),
            Duration::from_millis(100),
            &StopFlag::new(),
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
    /// 最後の保存の実 I/O（一時ファイルが FIFO で、読み手が現れるまで書き込みが戻らない）が固まっても、
    /// 停止の呼び出しは共通期限の内に戻る。固まった保存は `.ingest.lock` を持ち続け、読み手を付けると手放す。
    #[test]
    fn a_stuck_final_save_does_not_push_the_stop_past_the_common_deadline() {
        use crate::testutil::TempDir;
        use serde_json::json;

        let dir = TempDir::new("cli-stuck-final-save");
        let tmp = dir.path().join(".cache.json.tmp");
        if !std::process::Command::new("mkfifo")
            .arg(&tmp)
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("mkfifo が使えないので省略");
            return;
        }
        let cache = Arc::new(Cache::load(dir.path()));
        cache.put("geo:1", json!(1));
        let lock = crate::ingest::LockHold::for_test(dir.path());
        let stop = StopFlag::with_budget(Duration::from_millis(400));
        stop.stop();
        let (worker, saver) = (std::thread::spawn(|| {}), std::thread::spawn(|| {}));
        let began = std::time::Instant::now();
        // 1 つあたりの上限は 10 秒でも、共通期限（400ms）の残りまでしか待たない
        stop_ingest_threads(
            IngestThreads { worker, saver },
            &cache,
            lock.clone(),
            Duration::from_secs(10),
            &stop,
        );
        let waited = began.elapsed();
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        // 残った保存のスレッドが所有権を持っている（呼び出し側の複製 + スレッドの複製）
        assert_eq!(lock.holders(), 2);
        drop(std::fs::File::open(&tmp).unwrap()); // 読み手を付けて解放する
        for _ in 0..1000 {
            if lock.holders() == 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("最後の保存のスレッドが終わらない");
    }
}
