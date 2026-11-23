//! コマンドラインの解釈（design.md 6.2）。依存を増やさないため手で解釈する（`crates/geoframe/src/cli.rs` と同じ方針）。
//!
//! ```text
//! geoframe-export run --from 2026-01-10 [--to ...] --destination yamareco [--on-exists replace] [--dry-run] [設定の引数]
//! geoframe-export serve --listen <ip:port> --token-file <path> --allow <ip> [--allow <ip> ...] [設定の引数]
//! geoframe-export recover [--resolve <job_id>] [設定の引数]
//! geoframe-export import-credentials --rclone-config <path> --remote <name> --out <path>
//! ```
//!
//! 設定の引数: `--feed-base --state-dir --drive-credentials --folder-yamareco --folder-travel-journal --work-folder
//! --drive-upload-timeout --drive-timeout`（と、help に出さない試験専用の `--drive-endpoint`）。`--名前 値` と `--名前=値` の
//! どちらでも書ける。引数の誤り（知らない・重複・別のサブコマンドの引数・値の欠け・検査に通らない値）はすべて [`CliError`] で、
//! 終了コード 2。
//! エラーの表示に引数の値は写さない（トークンのファイルのパスなどが端末に残らないように、名前だけを出す）。

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use crate::error::ErrorCode;
use crate::request::{ExportRequest, RawRequest};
use crate::settings::{
    OPT_DRIVE_CREDENTIALS, OPT_DRIVE_ENDPOINT, OPT_DRIVE_TIMEOUT, OPT_DRIVE_UPLOAD_TIMEOUT,
    OPT_FEED_BASE, OPT_FOLDER_TRAVEL_JOURNAL, OPT_FOLDER_YAMARECO, OPT_STATE_DIR, OPT_WORK_FOLDER,
    RawSettings, Settings, SettingsError,
};

pub const USAGE: &str = "\
使い方:
  geoframe-export --version
  geoframe-export run --from <YYYY-MM-DD> [--to <YYYY-MM-DD>] --destination <yamareco|travel-journal>
                      [--on-exists <error|replace>] [--dry-run] [設定の引数]
                                  期間（両端を含む 14 日以下。--to を省くと 1 日）の写真を Drive へ書き出す。
                                  結果は JSON（status・result・error）を標準出力へ。終了コード 0 成功・0 枚 / 1 失敗 /
                                  2 引数・設定の誤り / 3 conflict / 4 実行中
  geoframe-export serve --listen <ip:port> --token-file <path> --allow <ip> [--allow <ip> ...] [設定の引数]
                                  書き出しの HTTP の入口を起動する
  geoframe-export recover [--resolve <job_id>] [設定の引数]
                                  未完了のジョブの記録と Drive の状態を照らし合わせて表示する。--resolve は blocked の解除
  geoframe-export import-credentials --rclone-config <path> --remote <name> --out <path>
                                  今の rclone の設定から、Drive の資格情報のファイル（0600）を 1 回だけ作る。値は表示しない
設定の引数:
  --feed-base <url>               写真サービスの基底 URL（http://ホスト[:ポート]。既定 http://127.0.0.1:8080）
  --state-dir <dir>               状態ディレクトリ（絶対パス。必須）
  --drive-credentials <path>      Drive の資格情報の JSON（絶対パス。必須。client_id・client_secret・refresh_token の 3 つ）
  --folder-yamareco <ID>          宛先 yamareco の親フォルダの Drive ID（必須）
  --folder-travel-journal <ID>    宛先 travel-journal の親フォルダの Drive ID（必須）
  --work-folder <ID>              作業用のフォルダ（中に incoming/・replaced/ を作る）の Drive ID（必須）
  --drive-upload-timeout <秒>     写真を送る操作全体の期限（既定 2700）
  --drive-timeout <秒>            それ以外の Drive の操作の期限（既定 300）
終了コード: 引数・設定の誤りは 2";

/// 解釈した結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Version,
    Help,
    Run {
        request: ExportRequest,
        settings: Settings,
    },
    Serve {
        listen: SocketAddr,
        token_file: PathBuf,
        allow: Vec<IpAddr>,
        settings: Settings,
    },
    Recover {
        resolve: Option<String>,
        settings: Settings,
    },
    /// rclone の設定から、Drive の資格情報のファイルを 1 回だけ作る（root が sudo で動かす。起動時の検査は通らない）。
    ImportCredentials {
        rclone_config: PathBuf,
        remote: String,
        out: PathBuf,
    },
}

/// 引数の誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    NoCommand,
    UnknownCommand,
    /// 知らない引数、またはそのサブコマンドでは使えない引数（名前だけ。`=` の後ろは写さない）。
    UnknownOption(String),
    /// 引数ではない余分な語（値は写さない）。
    UnexpectedArgument,
    MissingValue(&'static str),
    UnexpectedValue(&'static str),
    Duplicate(&'static str),
    MissingRequired(&'static str),
    InvalidValue(&'static str),
    Request(ErrorCode),
    Settings(SettingsError),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCommand => f.write_str(
                "サブコマンド（run・serve・recover・import-credentials）を指定してください",
            ),
            Self::UnknownCommand => {
                f.write_str("知らないサブコマンドです（run・serve・recover・import-credentials）")
            }
            Self::UnknownOption(name) => write!(f, "このコマンドでは使えない引数です: {name}"),
            Self::UnexpectedArgument => {
                f.write_str("引数の形が違います（--名前 値 の形で指定してください）")
            }
            Self::MissingValue(o) => write!(f, "{o} に値がありません"),
            Self::UnexpectedValue(o) => write!(f, "{o} は値を取りません"),
            Self::Duplicate(o) => write!(f, "{o} が重複しています"),
            Self::MissingRequired(o) => write!(f, "{o} が必要です"),
            Self::InvalidValue(o) => write!(f, "{o} の値が正しくありません"),
            Self::Request(code) => write!(f, "{code}: {}", code.message()),
            Self::Settings(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<SettingsError> for CliError {
    fn from(e: SettingsError) -> Self {
        Self::Settings(e)
    }
}

/// サブコマンドごとに受け付ける引数。
struct Spec {
    /// 値を 1 つ取る引数（重複は誤り）。
    values: &'static [&'static str],
    /// 値を取り、何度でも書ける引数。
    repeatable: &'static [&'static str],
    /// 値を取らない引数。
    flags: &'static [&'static str],
}

const RUN_SPEC: Spec = Spec {
    values: &[
        "--from",
        "--to",
        "--destination",
        "--on-exists",
        OPT_FEED_BASE,
        OPT_STATE_DIR,
        OPT_DRIVE_CREDENTIALS,
        OPT_FOLDER_YAMARECO,
        OPT_FOLDER_TRAVEL_JOURNAL,
        OPT_WORK_FOLDER,
        OPT_DRIVE_ENDPOINT,
        OPT_DRIVE_UPLOAD_TIMEOUT,
        OPT_DRIVE_TIMEOUT,
    ],
    repeatable: &[],
    flags: &["--dry-run", "--help"],
};

const SERVE_SPEC: Spec = Spec {
    values: &[
        "--listen",
        "--token-file",
        OPT_FEED_BASE,
        OPT_STATE_DIR,
        OPT_DRIVE_CREDENTIALS,
        OPT_FOLDER_YAMARECO,
        OPT_FOLDER_TRAVEL_JOURNAL,
        OPT_WORK_FOLDER,
        OPT_DRIVE_ENDPOINT,
        OPT_DRIVE_UPLOAD_TIMEOUT,
        OPT_DRIVE_TIMEOUT,
    ],
    repeatable: &["--allow"],
    flags: &["--help"],
};

const RECOVER_SPEC: Spec = Spec {
    values: &[
        "--resolve",
        OPT_FEED_BASE,
        OPT_STATE_DIR,
        OPT_DRIVE_CREDENTIALS,
        OPT_FOLDER_YAMARECO,
        OPT_FOLDER_TRAVEL_JOURNAL,
        OPT_WORK_FOLDER,
        OPT_DRIVE_ENDPOINT,
        OPT_DRIVE_UPLOAD_TIMEOUT,
        OPT_DRIVE_TIMEOUT,
    ],
    repeatable: &[],
    flags: &["--help"],
};

const IMPORT_SPEC: Spec = Spec {
    values: &["--rclone-config", "--remote", "--out"],
    repeatable: &[],
    flags: &["--help"],
};

/// 読んだ引数。
#[derive(Default)]
struct Parsed {
    values: Vec<(&'static str, String)>,
    flags: Vec<&'static str>,
}

impl Parsed {
    /// 値を 1 つ取る引数の値。
    fn one(&self, name: &'static str) -> Option<String> {
        self.values
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
    }

    fn many(&self, name: &'static str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn flag(&self, name: &'static str) -> bool {
        self.flags.contains(&name)
    }

    fn required(&self, name: &'static str) -> Result<String, CliError> {
        self.one(name).ok_or(CliError::MissingRequired(name))
    }

    fn settings(&self) -> Result<Settings, CliError> {
        Ok(Settings::from_raw(&RawSettings {
            feed_base: self.one(OPT_FEED_BASE),
            state_dir: self.one(OPT_STATE_DIR),
            drive_credentials: self.one(OPT_DRIVE_CREDENTIALS),
            folder_yamareco: self.one(OPT_FOLDER_YAMARECO),
            folder_travel_journal: self.one(OPT_FOLDER_TRAVEL_JOURNAL),
            work_folder: self.one(OPT_WORK_FOLDER),
            drive_endpoint: self.one(OPT_DRIVE_ENDPOINT),
            drive_upload_timeout: self.one(OPT_DRIVE_UPLOAD_TIMEOUT),
            drive_timeout: self.one(OPT_DRIVE_TIMEOUT),
        })?)
    }
}

fn scan(args: &[String], spec: &Spec) -> Result<Parsed, CliError> {
    let mut parsed = Parsed::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if !arg.starts_with("--") {
            return Err(CliError::UnexpectedArgument);
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (arg.as_str(), None),
        };
        if let Some(flag) = spec.flags.iter().find(|f| **f == name) {
            if inline.is_some() {
                return Err(CliError::UnexpectedValue(flag));
            }
            if parsed.flags.contains(flag) {
                return Err(CliError::Duplicate(flag));
            }
            parsed.flags.push(flag);
            continue;
        }
        let (option, repeatable) = if let Some(o) = spec.values.iter().find(|o| **o == name) {
            (*o, false)
        } else if let Some(o) = spec.repeatable.iter().find(|o| **o == name) {
            (*o, true)
        } else {
            return Err(CliError::UnknownOption(name.to_owned()));
        };
        let value = match inline {
            Some(v) => v,
            // 次の語が別の引数（`--` で始まる）なら、値が無いものとして断る。`--名前=値` なら `--` で始まる値も書ける
            None => match it.as_slice().first() {
                Some(next) if !next.starts_with("--") => it.next().cloned().unwrap_or_default(),
                _ => return Err(CliError::MissingValue(option)),
            },
        };
        if !repeatable && parsed.values.iter().any(|(n, _)| *n == option) {
            return Err(CliError::Duplicate(option));
        }
        parsed.values.push((option, value));
    }
    Ok(parsed)
}

/// `recover --resolve` のジョブ ID の目印の形（`[A-Za-z0-9_-]{1,64}`）。ジョブ ID の本来の形の検査は、ジョブの記録を扱う
/// 側（T5）がかける。ここでは、パスや引数として危ない文字を弾くだけ。
fn plausible_job_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 引数（プログラム名を除く）を解釈する。
pub fn parse(args: &[String]) -> Result<Command, CliError> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err(CliError::NoCommand);
    };
    match cmd.as_str() {
        "--version" | "-V" => return Ok(Command::Version),
        "--help" | "-h" | "help" => return Ok(Command::Help),
        _ => {}
    }
    match cmd.as_str() {
        "run" => {
            let p = scan(rest, &RUN_SPEC)?;
            if p.flag("--help") {
                return Ok(Command::Help);
            }
            let raw = RawRequest {
                from: Some(p.required("--from")?),
                to: p.one("--to"),
                destination: Some(p.required("--destination")?),
                on_exists: p.one("--on-exists"),
                dry_run: p.flag("--dry-run"),
                request_id: None,
            };
            let request = raw.validate().map_err(CliError::Request)?;
            Ok(Command::Run {
                request,
                settings: p.settings()?,
            })
        }
        "serve" => {
            let p = scan(rest, &SERVE_SPEC)?;
            if p.flag("--help") {
                return Ok(Command::Help);
            }
            let listen = p
                .required("--listen")?
                .parse::<SocketAddr>()
                .map_err(|_| CliError::InvalidValue("--listen"))?;
            let token_file = p.required("--token-file")?;
            if token_file.is_empty() {
                return Err(CliError::InvalidValue("--token-file"));
            }
            let allow = p
                .many("--allow")
                .iter()
                .map(|a| {
                    a.parse::<IpAddr>()
                        .map_err(|_| CliError::InvalidValue("--allow"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if allow.is_empty() {
                return Err(CliError::MissingRequired("--allow"));
            }
            Ok(Command::Serve {
                listen,
                token_file: PathBuf::from(token_file),
                allow,
                settings: p.settings()?,
            })
        }
        "recover" => {
            let p = scan(rest, &RECOVER_SPEC)?;
            if p.flag("--help") {
                return Ok(Command::Help);
            }
            let resolve = match p.one("--resolve") {
                None => None,
                Some(id) if plausible_job_id(&id) => Some(id),
                Some(_) => return Err(CliError::InvalidValue("--resolve")),
            };
            Ok(Command::Recover {
                resolve,
                settings: p.settings()?,
            })
        }
        "import-credentials" => {
            let p = scan(rest, &IMPORT_SPEC)?;
            if p.flag("--help") {
                return Ok(Command::Help);
            }
            let rclone_config = p.required("--rclone-config")?;
            let remote = p.required("--remote")?;
            let out = p.required("--out")?;
            for (name, v) in [("--rclone-config", &rclone_config), ("--remote", &remote), ("--out", &out)] {
                if v.is_empty() || v.contains('\0') {
                    return Err(CliError::InvalidValue(name));
                }
            }
            Ok(Command::ImportCredentials {
                rclone_config: PathBuf::from(rclone_config),
                remote,
                out: PathBuf::from(out),
            })
        }
        _ => Err(CliError::UnknownCommand),
    }
}

#[cfg(test)]
mod tests {
    use crate::request::{Destination, OnExists};

    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    const COMMON: &str = "--drive-credentials /x/cred/drive-credentials --folder-yamareco FolderY1 \
        --folder-travel-journal FolderT2 --work-folder FolderW3 --state-dir /x/state";

    fn parse_line(line: &str) -> Result<Command, CliError> {
        parse(&args(line))
    }

    #[test]
    fn run_accepts_the_full_set_of_arguments() {
        let line = format!(
            "run --from 2030-03-10 --to=2030-03-12 --destination travel-journal --on-exists replace --dry-run \
             --feed-base http://127.0.0.1:9 --drive-upload-timeout 60 --drive-timeout 30 \
             --drive-endpoint http://127.0.0.1:8123 {COMMON}"
        );
        let Command::Run { request, settings } = parse_line(&line).unwrap() else {
            panic!("run のはず");
        };
        assert_eq!(request.period.from().to_string(), "2030-03-10");
        assert_eq!(request.period.to().to_string(), "2030-03-12");
        assert_eq!(request.destination, Destination::TravelJournal);
        assert_eq!(request.on_exists, OnExists::Replace);
        assert!(request.dry_run);
        assert_eq!(request.request_id, None);
        assert_eq!(settings.feed_base.as_str(), "http://127.0.0.1:9");
        assert_eq!(settings.folder_yamareco, "FolderY1");
        assert_eq!(settings.folder_travel_journal, "FolderT2");
        assert_eq!(settings.work_folder, "FolderW3");
        assert_eq!(
            settings.drive_credentials,
            PathBuf::from("/x/cred/drive-credentials")
        );
        assert_eq!(settings.drive_upload_timeout.as_secs(), 60);
        assert_eq!(settings.drive_timeout.as_secs(), 30);
        assert_eq!(
            settings.endpoints(),
            crate::drive::http::Endpoints::loopback("http://127.0.0.1:8123").unwrap()
        );
    }

    #[test]
    fn run_defaults() {
        let Command::Run { request, settings } = parse_line(&format!(
            "run --from 2030-03-10 --destination yamareco {COMMON}"
        ))
        .unwrap() else {
            panic!("run のはず");
        };
        assert!(request.period.is_single_day());
        assert_eq!(request.on_exists, OnExists::Error);
        assert!(!request.dry_run);
        assert_eq!(settings.drive_upload_timeout.as_secs(), 45 * 60);
        assert_eq!(settings.drive_timeout.as_secs(), 5 * 60);
        assert_eq!(
            settings.endpoints(),
            crate::drive::http::Endpoints::production()
        );
    }

    #[test]
    fn run_request_errors_are_usage_errors_with_the_fixed_codes() {
        let run = |extra: &str| parse_line(&format!("run {extra} {COMMON}"));
        assert_eq!(
            run("--destination yamareco"),
            Err(CliError::MissingRequired("--from"))
        );
        assert_eq!(
            run("--from 2030-03-10"),
            Err(CliError::MissingRequired("--destination"))
        );
        assert_eq!(
            run("--from 2030-02-30 --destination yamareco"),
            Err(CliError::Request(ErrorCode::InvalidDate))
        );
        assert_eq!(
            run("--from 2030-03-11 --to 2030-03-10 --destination yamareco"),
            Err(CliError::Request(ErrorCode::InvalidRange))
        );
        assert_eq!(
            run("--from 2030-03-01 --to 2030-03-15 --destination yamareco"),
            Err(CliError::Request(ErrorCode::InvalidRange))
        );
        assert!(run("--from 2030-03-01 --to 2030-03-14 --destination yamareco").is_ok());
        assert_eq!(
            run("--from 2030-03-10 --destination drive"),
            Err(CliError::Request(ErrorCode::InvalidDestination))
        );
        assert_eq!(
            run("--from 2030-03-10 --destination yamareco --on-exists append"),
            Err(CliError::Request(ErrorCode::InvalidOnExists))
        );
    }

    #[test]
    fn serve_and_recover_accept_their_arguments() {
        let Command::Serve { listen, token_file, allow, settings } = parse_line(&format!(
            "serve --listen 127.0.0.1:9100 --token-file /x/token --allow 192.0.2.1 --allow ::1 {COMMON}"
        ))
        .unwrap() else {
            panic!("serve のはず");
        };
        assert_eq!(listen.to_string(), "127.0.0.1:9100");
        assert_eq!(token_file, PathBuf::from("/x/token"));
        assert_eq!(allow.len(), 2);
        assert_eq!(settings.work_folder, "FolderW3");

        assert!(matches!(
            parse_line(&format!("recover {COMMON}")),
            Ok(Command::Recover { resolve: None, .. })
        ));
        let Ok(Command::Recover { resolve, .. }) =
            parse_line(&format!("recover --resolve abc-123_X {COMMON}"))
        else {
            panic!("recover のはず");
        };
        assert_eq!(resolve.as_deref(), Some("abc-123_X"));
    }

    #[test]
    fn serve_needs_its_own_arguments() {
        let serve = |extra: &str| parse_line(&format!("serve {extra} {COMMON}"));
        assert_eq!(
            serve("--token-file /x/t --allow 192.0.2.1"),
            Err(CliError::MissingRequired("--listen"))
        );
        assert_eq!(
            serve("--listen 127.0.0.1:1 --allow 192.0.2.1"),
            Err(CliError::MissingRequired("--token-file"))
        );
        assert_eq!(
            serve("--listen 127.0.0.1:1 --token-file /x/t"),
            Err(CliError::MissingRequired("--allow"))
        );
        assert_eq!(
            serve("--listen 127.0.0.1 --token-file /x/t --allow 192.0.2.1"),
            Err(CliError::InvalidValue("--listen"))
        );
        assert_eq!(
            serve("--listen 127.0.0.1:1 --token-file /x/t --allow 192.0.2.0/24"),
            Err(CliError::InvalidValue("--allow"))
        );
        assert_eq!(
            serve("--listen 127.0.0.1:1 --token-file= --allow 192.0.2.1"),
            Err(CliError::InvalidValue("--token-file"))
        );
    }

    #[test]
    fn recover_resolve_rejects_unsafe_ids() {
        for bad in ["a/b", "../x", "a b", "", &"x".repeat(65)] {
            assert_eq!(
                parse(&[
                    "recover".to_owned(),
                    format!("--resolve={bad}"),
                    "--drive-credentials=/x/c".to_owned(),
                    "--folder-yamareco=A".to_owned(),
                    "--folder-travel-journal=B".to_owned(),
                    "--work-folder=C".to_owned(),
                    "--state-dir=/x/s".to_owned(),
                ]),
                Err(CliError::InvalidValue("--resolve")),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn arguments_of_other_subcommands_are_refused() {
        // run に serve の引数、serve に run の引数、recover に両方
        assert!(matches!(
            parse_line(&format!("run --from 2030-03-10 --destination yamareco --listen 127.0.0.1:1 {COMMON}")),
            Err(CliError::UnknownOption(n)) if n == "--listen"
        ));
        assert!(matches!(
            parse_line(&format!("serve --listen 127.0.0.1:1 --token-file /x/t --allow 192.0.2.1 --dry-run {COMMON}")),
            Err(CliError::UnknownOption(n)) if n == "--dry-run"
        ));
        assert!(matches!(
            parse_line(&format!("recover --from 2030-03-10 {COMMON}")),
            Err(CliError::UnknownOption(n)) if n == "--from"
        ));
        assert!(matches!(
            parse_line(&format!("run --from 2030-03-10 --destination yamareco --resolve x {COMMON}")),
            Err(CliError::UnknownOption(n)) if n == "--resolve"
        ));
        // 知らない引数。`=` の後ろの値は表示に写さない
        let e = parse_line(&format!("recover --token=SECRETVALUE {COMMON}")).unwrap_err();
        assert_eq!(e, CliError::UnknownOption("--token".into()));
        assert!(!e.to_string().contains("SECRETVALUE"));
    }

    #[test]
    fn malformed_arguments_are_usage_errors() {
        let run = |extra: &str| parse_line(&format!("run --destination yamareco {extra} {COMMON}"));
        assert_eq!(run("--from"), Err(CliError::MissingValue("--from")));
        assert_eq!(
            run("--from 2030-03-10 --from 2030-03-11"),
            Err(CliError::Duplicate("--from"))
        );
        assert_eq!(
            run("--from=2030-03-10 --dry-run --dry-run"),
            Err(CliError::Duplicate("--dry-run"))
        );
        assert_eq!(
            run("--from=2030-03-10 --dry-run=1"),
            Err(CliError::UnexpectedValue("--dry-run"))
        );
        assert_eq!(
            run("--from 2030-03-10 extra"),
            Err(CliError::UnexpectedArgument)
        );
        assert_eq!(
            run("--from 2030-03-10 -x"),
            Err(CliError::UnexpectedArgument)
        );
        // 値の位置に別の引数があれば、値が無いものとして断る
        assert_eq!(
            run("--from --dry-run"),
            Err(CliError::MissingValue("--from"))
        );
        assert_eq!(
            run("--from=--dry-run"),
            Err(CliError::Request(ErrorCode::InvalidDate))
        );
        // serve の --allow は何度でも書けるが、他は重複できない
        assert_eq!(
            parse_line(&format!(
                "serve --listen 127.0.0.1:1 --listen 127.0.0.1:2 --token-file /x/t --allow 192.0.2.1 {COMMON}"
            )),
            Err(CliError::Duplicate("--listen"))
        );
    }

    #[test]
    fn setting_errors_surface_as_usage_errors() {
        let base = "run --from 2030-03-10 --destination yamareco";
        assert_eq!(
            parse_line(&format!(
                "{base} --folder-yamareco A --folder-travel-journal B --work-folder C --state-dir /x/s"
            )),
            Err(CliError::Settings(SettingsError::MissingOption(
                OPT_DRIVE_CREDENTIALS
            )))
        );
        // 空の ID（unit の `${VAR}` が空のとき）は終了コード 2 の設定の誤り
        assert_eq!(
            parse_line(&format!(
                "{base} --drive-credentials /x/c --folder-yamareco= --folder-travel-journal B --work-folder C --state-dir /x/s"
            )),
            Err(CliError::Settings(SettingsError::InvalidDriveId(
                OPT_FOLDER_YAMARECO
            )))
        );
        // 3 つが同じなら断る
        assert_eq!(
            parse_line(&format!(
                "{base} --drive-credentials /x/c --folder-yamareco A --folder-travel-journal A --work-folder C --state-dir /x/s"
            )),
            Err(CliError::Settings(SettingsError::SameFolder(
                OPT_FOLDER_YAMARECO,
                OPT_FOLDER_TRAVEL_JOURNAL
            )))
        );
        assert_eq!(
            parse_line(&format!("{base} {COMMON} --feed-base https://x")),
            Err(CliError::Settings(SettingsError::InvalidFeedBase))
        );
        // 試験専用の通信先は loopback の http だけ
        assert_eq!(
            parse_line(&format!(
                "{base} {COMMON} --drive-endpoint https://www.googleapis.com"
            )),
            Err(CliError::Settings(SettingsError::InvalidEndpoint))
        );
    }

    #[test]
    fn removed_rclone_arguments_are_unknown_options() {
        for old in [
            "--rclone /x/rclone",
            "--rclone-config /x/c.conf",
            "--remote-yamareco gdrive:a",
            "--remote-travel-journal gdrive:b",
            "--work-root gdrive:w",
            "--rclone-copy-timeout 60",
            "--rclone-timeout 30",
        ] {
            for cmd in [
                "run --from 2030-03-10 --destination yamareco",
                "serve --listen 127.0.0.1:1 --token-file /x/t --allow 192.0.2.1",
                "recover",
            ] {
                let name = old.split(' ').next().unwrap();
                assert_eq!(
                    parse_line(&format!("{cmd} {COMMON} {old}")),
                    Err(CliError::UnknownOption(name.into())),
                    "{cmd} {old}"
                );
            }
        }
    }

    #[test]
    fn help_does_not_mention_the_test_only_endpoint_or_rclone_arguments() {
        assert!(!USAGE.contains("--drive-endpoint"));
        // （`import-credentials` だけは、取り込み元として `--rclone-config` を取る）
        for gone in ["--rclone-copy-timeout", "--rclone-timeout", "--rclone <"] {
            assert!(!USAGE.contains(gone), "{gone}");
        }
        assert!(!USAGE.contains("--remote-"));
        assert!(!USAGE.contains("--work-root"));
        for name in [
            "--drive-credentials",
            "--folder-yamareco",
            "--folder-travel-journal",
            "--work-folder",
            "--drive-upload-timeout",
            "--drive-timeout",
        ] {
            assert!(USAGE.contains(name), "{name}");
        }
    }

    #[test]
    fn import_credentials_takes_its_three_arguments() {
        let ok = parse_line("import-credentials --rclone-config /x/rclone.conf --remote gdrive --out /x/out.json");
        assert_eq!(
            ok,
            Ok(Command::ImportCredentials {
                rclone_config: PathBuf::from("/x/rclone.conf"),
                remote: "gdrive".into(),
                out: PathBuf::from("/x/out.json"),
            })
        );
        assert_eq!(
            parse_line("import-credentials --remote gdrive --out /x/o"),
            Err(CliError::MissingRequired("--rclone-config"))
        );
        assert_eq!(
            parse_line("import-credentials --rclone-config /x/c --out /x/o"),
            Err(CliError::MissingRequired("--remote"))
        );
        assert_eq!(
            parse_line("import-credentials --rclone-config /x/c --remote gdrive"),
            Err(CliError::MissingRequired("--out"))
        );
        // 設定の引数は取らない
        assert!(matches!(
            parse_line(&format!(
                "import-credentials --rclone-config /x/c --remote gdrive --out /x/o {COMMON}"
            )),
            Err(CliError::UnknownOption(_))
        ));
        assert_eq!(
            parse_line("import-credentials --rclone-config= --remote gdrive --out /x/o"),
            Err(CliError::InvalidValue("--rclone-config"))
        );
    }

    #[test]
    fn top_level_words() {
        assert_eq!(parse(&[]), Err(CliError::NoCommand));
        assert_eq!(parse(&args("--version")), Ok(Command::Version));
        assert_eq!(parse(&args("--help")), Ok(Command::Help));
        assert_eq!(parse(&args("run --help")), Ok(Command::Help));
        assert_eq!(parse(&args("frobnicate")), Err(CliError::UnknownCommand));
        assert_eq!(
            parse(&args("--from 2030-03-10")),
            Err(CliError::UnknownCommand)
        );
    }
}
