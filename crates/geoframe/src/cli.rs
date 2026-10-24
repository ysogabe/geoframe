//! コマンドライン。依存を増やさないため引数は手で解釈する（P1 で必要なら見直す）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Config, Overrides};
use crate::db::Catalog;
use crate::server::AppState;

const USAGE: &str = "\
使い方:
  geoframe --version
  geoframe check-db <catalog.db>        photos の行数と SQLite のバージョンを出す（読み取り専用）
  geoframe serve [--listen <addr:port>] [--data-dir <dir>]
                                        サーバーを起動する（読み取り専用。DB は <dir>/catalog.db）
                                        設定は 起動引数 > 環境変数 GEOFRAME_* > 既定値 の順
                                        （詳細は docs/rust-migration/server.md）";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    CheckDb(PathBuf),
    Serve(Overrides),
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
        Command::Serve(overrides) => serve(&overrides),
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
    let state = AppState::new(Arc::new(config), catalog);
    if state.data_root.as_path() != state.config.data_dir.as_path() {
        // データ置き場がシンボリックリンクのとき、解決先の内側かどうかで静的配信を判定する
        tracing::info!(
            "データ置き場はリンクを辿って {} に解決されました",
            state.data_root.display()
        );
    }
    let result = runtime.block_on(crate::server::serve(state));
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
    }
}
