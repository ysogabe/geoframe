//! コマンドの振り分け。引数の解釈 → 起動時の検査 → サブコマンド。
//!
//! 起動時の検査（[`crate::startup::startup_check`]）は、`run`（`--dry-run` を含む）・`serve`・`recover` のどれでも、
//! 中身を始める前に通る。通らなければ何もせず終了コード 2。表示には設定ファイルの値を出さない。

use std::io::Write;

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

/// `args` はプログラム名を除いた引数。終了コードを返す。
pub fn main_with(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(e) => return usage_error(&e, err),
    };
    let outcome = match command {
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
            run::run(&settings, &request)
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
            serve::serve(&settings, listen, &token_file, &allow)
        }
        Command::Recover { resolve, settings } => {
            if let Err(code) = startup(&settings, err) {
                return code;
            }
            recover::recover(&settings, resolve.as_deref())
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
