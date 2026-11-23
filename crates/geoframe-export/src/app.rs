//! コマンドの振り分け。引数の解釈 → 起動時の検査 → サブコマンド。
//!
//! 起動時の検査（[`crate::startup::startup_check`]）は、`run`（`--dry-run` を含む）・`serve`・`recover` のどれでも、
//! 中身を始める前に通る（Drive へトークンを 1 回取りに行き、3 つのフォルダを確かめる）。通らなければ何もせず、設定の誤り・
//! 再認可が要るなら終了コード 2、通信の失敗・5xx なら 1。表示には資格情報・ID の値を出さない。
//! `import-credentials` は、起動時の検査を通らない（Drive に触れない。root が sudo で 1 回だけ動かす）ので、root でも動く。
//!
//! root（実効ユーザー ID が 0）で直接動かされたら、`run`・`serve`・`recover` は断る（終了コード 2）。専用のユーザーで動かす
//! （design.md 6.2）。実効ユーザー ID は [`run_command`] の引数で受け取る（バイナリの `main` が本物の値を渡す。
//! [`main_with`] は確かめない。root の CI で試験が落ちないように、判定を試験から切り離している）。

use std::io::Write;

use crate::cancel::Cancel;
use crate::cli::{self, CliError, Command};
use crate::startup::startup_check;
use crate::{recover, run, serve};

/// 引数・設定の誤り（`run` の終了コードの 2）。
pub const EXIT_USAGE: u8 = 2;

/// `args` はプログラム名を除いた引数。終了コードを返す。実効ユーザー ID は確かめない（試験用。バイナリは [`run_command`]）。
pub fn main_with(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    run_command(None, args, out, err)
}

/// [`main_with`] に、実効ユーザー ID（`Some(0)` なら root）を渡せる形。`run`・`serve`・`recover` は、root なら何もせず
/// 終了コード 2。`--version`・`--help` は root でも動く。
pub fn run_command(
    euid: Option<u32>,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    run_command_cancellable(euid, args, out, err, &Cancel::new(), false)
}

/// [`run_command`] に、キャンセルの印を渡せる形。`run` と `serve` が使う。`handle_signals` が真なら、SIGINT・SIGTERM を
/// プロセスを終わらせずに `cancel` の印を立てる受け口にする（実行中の rclone を止めて `wait` し終えてから終わる）。バイナリの
/// `main` だけが真にする（プロセス内の試験は並列で 1 つのプロセスを共有するので、受け口を入れない）。
///
/// - `run`: `run` の前に libc の handler を入れる（[`crate::cancel::install_signal_handlers`]）。
/// - `serve`: libc の handler は入れない。`serve` が tokio のシグナルで受けて [`Cancel::cancel`] を呼ぶ（[`crate::serve`]。
///   起動時の復旧より前に登録する）。
/// - `recover`・`--help`・`--version`: 入れない（signal でふつうに終わる）。
pub fn run_command_cancellable(
    euid: Option<u32>,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
    cancel: &Cancel,
    handle_signals: bool,
) -> u8 {
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(e) => return usage_error(&e, err),
    };
    if euid == Some(0)
        && !matches!(
            command,
            Command::Version | Command::Help | Command::ImportCredentials { .. }
        )
    {
        let _ = writeln!(
            err,
            "geoframe-export: root では動かしません。専用のユーザーで実行してください"
        );
        return EXIT_USAGE;
    }
    match command {
        Command::Version => {
            let _ = writeln!(out, "geoframe-export {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Command::Help => {
            let _ = writeln!(out, "{}", cli::USAGE);
            0
        }
        Command::Run { request, settings } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            if handle_signals && let Err(code) = install_handlers(cancel, err) {
                return code;
            }
            run::run_with(
                &settings,
                &request,
                crate::flow::Tuning::default(),
                cancel,
                out,
                err,
            )
        }
        Command::Serve {
            listen,
            token_file,
            allow,
            settings,
        } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            // `serve` は、SIGINT・SIGTERM を tokio で受ける（libc の handler は入れない。`handle_signals` が真のとき）。
            // 復旧より前に入れるのが要点なので、受け口の登録は `serve::serve` の中の最初の段階で行う
            serve::serve(
                &settings,
                listen,
                &token_file,
                &allow,
                cancel,
                handle_signals,
                err,
            )
        }
        Command::Recover { resolve, settings } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            recover::recover(&settings, resolve.as_deref(), out, err)
        }
        Command::ImportCredentials {
            rclone_config,
            remote,
            out: out_path,
        } => crate::import::run(&rclone_config, &remote, &out_path, out, err),
    }
}

fn install_handlers(cancel: &Cancel, err: &mut dyn Write) -> Result<(), u8> {
    crate::cancel::install_signal_handlers(cancel).map_err(|_| {
        let _ = writeln!(err, "geoframe-export: signal の handler を入れられません");
        EXIT_USAGE
    })
}

fn startup(settings: &crate::settings::Settings, err: &mut dyn Write) -> Result<(), u8> {
    startup_check(settings).map_err(|e| {
        let _ = writeln!(err, "geoframe-export: 起動しません: {e}");
        e.exit_code()
    })
}

fn usage_error(e: &CliError, err: &mut dyn Write) -> u8 {
    let _ = writeln!(err, "geoframe-export: {e}（使い方は --help）");
    EXIT_USAGE
}
