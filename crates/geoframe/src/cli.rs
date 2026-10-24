//! コマンドライン。依存を増やさないため引数は手で解釈する（P1 で必要なら見直す）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
使い方:
  geoframe --version
  geoframe check-db <catalog.db>        photos の行数と SQLite のバージョンを出す（読み取り専用）
  geoframe serve --listen <addr:port>   GET /healthz だけを返すサーバーを起動する";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    CheckDb(PathBuf),
    Serve(SocketAddr),
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
            let mut listen = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--listen" => {
                        let value = args.next().ok_or("--listen に値が要ります")?;
                        listen = Some(
                            value
                                .parse::<SocketAddr>()
                                .map_err(|e| format!("--listen の値 {value:?} が不正です: {e}"))?,
                        );
                    }
                    other => return Err(format!("serve の引数 {other:?} は不明です")),
                }
            }
            Ok(Command::Serve(
                listen.ok_or("serve には --listen が要ります")?,
            ))
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
        Command::Serve(addr) => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("geoframe: tokio を起動できません: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(crate::server::serve(addr)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("geoframe: サーバーが異常終了しました: {e}");
                    ExitCode::FAILURE
                }
            }
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
            Ok(Command::Serve("127.0.0.1:18080".parse().unwrap()))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
        assert!(p(&["check-db"]).is_err());
        assert!(p(&["check-db", "a", "b"]).is_err());
        assert!(p(&["serve"]).is_err());
        assert!(p(&["serve", "--listen", "bad"]).is_err());
    }
}
