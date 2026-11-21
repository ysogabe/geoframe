//! コマンドの振り分け。引数の解釈 → 起動時の検査 → サブコマンド。
//!
//! 起動時の検査（[`crate::startup::startup_check`]）は、`run`（`--dry-run` を含む）・`serve`・`recover` のどれでも、
//! 中身を始める前に通る。通らなければ何もせず終了コード 2。表示には設定ファイルの値を出さない。
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
/// 後のタスクで作るサブコマンド（T2 の骨組みだけの間）。
pub const EXIT_NOT_IMPLEMENTED: u8 = 70;

/// まだ作っていないサブコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotImplemented {
    /// 作るタスク（表示用）。
    pub task: &'static str,
}

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

/// [`run_command`] に、キャンセルの印を渡せる形。`run` と `serve` が使う。`handle_signals` が真なら、`run`・`serve` の前に
/// SIGINT・SIGTERM の handler を入れる（[`crate::cancel::install_signal_handlers`]。以後、この 2 つの signal はプロセスを
/// 終わらせず、`cancel` の印を立てる。実行中の rclone を止めて `wait` し終えてから終わる）。バイナリの `main` だけが真にする
/// （プロセス内の試験は並列で 1 つのプロセスを共有するので、handler を入れない）。`recover` と `--help`・`--version` は、
/// handler を入れない（signal でふつうに終わる）。
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
    if euid == Some(0) && !matches!(command, Command::Version | Command::Help) {
        let _ = writeln!(
            err,
            "geoframe-export: root では動かしません。専用のユーザーで実行してください"
        );
        return EXIT_USAGE;
    }
    let outcome: Result<u8, NotImplemented> = match command {
        Command::Version => {
            let _ = writeln!(out, "geoframe-export {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Command::Help => {
            let _ = writeln!(out, "{}", cli::USAGE);
            return 0;
        }
        Command::Run { request, settings } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            if handle_signals && let Err(code) = install_handlers(cancel, err) {
                return code;
            }
            return run::run_with(
                &settings,
                &request,
                crate::flow::Tuning::default(),
                cancel,
                out,
                err,
            );
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
            if handle_signals && let Err(code) = install_handlers(cancel, err) {
                return code;
            }
            serve::serve(&settings, listen, &token_file, &allow, cancel)
        }
        Command::Recover { resolve, settings } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            return recover::recover(&settings, resolve.as_deref(), out, err);
        }
    };
    match outcome {
        Ok(code) => code,
        Err(NotImplemented { task }) => {
            let _ = writeln!(
                err,
                "geoframe-export: このコマンドは未実装です（{task}で作ります）"
            );
            EXIT_NOT_IMPLEMENTED
        }
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
        EXIT_USAGE
    })
}

fn usage_error(e: &CliError, err: &mut dyn Write) -> u8 {
    let _ = writeln!(err, "geoframe-export: {e}（使い方は --help）");
    EXIT_USAGE
}
