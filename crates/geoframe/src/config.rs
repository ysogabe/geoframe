//! 設定。現行 Python（`app/geoframe/config.py`）と同じ `GEOFRAME_*` の名前・既定値で読む。
//!
//! # 優先順位
//!
//! 高い順に **起動引数 > 環境変数 > 既定値**。
//!
//! | 項目 | 起動引数 | 環境変数 | 既定値 |
//! |---|---|---|---|
//! | 待ち受け | `--listen <addr:port>`（host と port の両方を置き換える） | `GEOFRAME_HOST` / `GEOFRAME_PORT` | `0.0.0.0` / `8080` |
//! | データ置き場 | `--data-dir <path>` | `GEOFRAME_DATA_DIR` | `./var`（実行時のカレントディレクトリ基準） |
//! | 許可する Host 名 | なし | `GEOFRAME_ALLOWED_HOSTS`（`localhost` に足す） | `localhost` のみ |
//! | ログの詳しさ | なし | `GEOFRAME_LOG`（Rust だけの項目） | `info` |
//!
//! `TZ` は Rust 側では読まない。P1 は時刻の変換をせず、`taken_at` などの naive な日時は文字列の
//! まま扱う（inventory.md §6.1）。systemd unit が `TZ=Asia/Tokyo` を与え続けるのは、並走する
//! Python のためと、将来 `Local` を使うときの保険（[`Config::tz`] は起動ログ用に控えるだけ）。
//!
//! 環境変数が空文字のときは「未設定」と同じに扱う（現行の `GEOFRAME_DATA_DIR` の `or` と同じ。
//! systemd の `EnvironmentFile` で `KEY=` と書かれた場合の事故を避ける）。
//!
//! # `.env`
//!
//! Rust は `app/.env` を **読まない**。単一バイナリには `app/` が無く、本番は systemd の
//! `EnvironmentFile=/etc/default/geoframe` が同じ役目を持っているため。手元で試すときは
//! `set -a; . ./環境ファイル; set +a` してから起動する。詳細は docs/rust-migration/server.md。
//!
//! # 秘密情報
//!
//! P1 の設定項目に秘密の値は無い。P2 以降で鍵を扱う項目（`GEOFRAME_DRIVE_KEY` など）が入るときも、
//! 設定が持つのは**鍵ファイルのパスだけ**で、中身は設定の型に載せない。`Config` の `Debug`・起動
//! ログ（[`Config::log_summary`]）に出すのは下に挙げた項目だけで、秘密になりうる値が増えたら
//! この一覧に足さない限りログへ出ない作りにしてある。

use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

pub const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_DATA_DIR: &str = "var";
pub const DB_FILE_NAME: &str = "catalog.db";

/// P2 以降で読む `GEOFRAME_*`（現行の `config.py` にある項目）。P1 では**読まず、警告も出さない**。
/// `docs/reference/configuration.md` の項目がここか [`SUPPORTED_ENV`] のどちらかに載っているかを
/// テストで確かめている（Python 側で項目が増えたのに Rust の分類が漏れるのを防ぐ）。
pub const UNSUPPORTED_ENV: &[&str] = &[
    // P2: 取り込み
    "GEOFRAME_PHOTOS_DIR",
    "GEOFRAME_DRIVE_KEY",
    "GEOFRAME_DRIVE_FOLDER",
    "GEOFRAME_DRIVE_POLL_MIN",
    "GEOFRAME_FETCH_PER_CYCLE",
    "GEOFRAME_QUALITY_PER_CYCLE",
    "GEOFRAME_INFER_MIN",
    "GEOFRAME_INFER_OFFSET_MIN",
    // P3: 管理機能（地図タイル）
    "GEOFRAME_TILE_UA",
    "GEOFRAME_TILE_CACHE_MAX_MB",
    "GEOFRAME_TILE_FETCH_PER_MIN",
    // P4: AI・位置候補
    "GEOFRAME_AI_ENABLED",
    "GEOFRAME_AI_EMBED_PER_CYCLE",
    "GEOFRAME_AI_JUDGE_PER_CYCLE",
    "GEOFRAME_AI_POLL_S",
    "GEOFRAME_AI_TIME_WINDOW_MIN",
    "GEOFRAME_AI_SIMILARITY",
    "GEOFRAME_AI_VISUAL_SIMILARITY",
    "GEOFRAME_AI_SHORTLIST_MAX",
    "GEOFRAME_AI_JUDGE_SHUFFLE_RATE",
    "GEOFRAME_AI_SEARCH_PER_MIN",
    "GEOFRAME_EMBED_BACKEND",
    "GEOFRAME_LOCAL_EMBED_MODEL_DIR",
    "GEOFRAME_LOCAL_EMBED_THREADS",
    "GEOFRAME_RANK_MODEL",
    "GEOFRAME_LOC_SUGGEST",
    "GEOFRAME_DECISION_URL",
    "GEOFRAME_LOC_SUGGEST_MODEL",
    "GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE",
    "GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE",
];

/// P1 で読む `GEOFRAME_*`（`GEOFRAME_LOG` は Rust だけの項目なので Python の一覧には無い）。
pub const SUPPORTED_ENV: &[&str] = &[
    "GEOFRAME_DATA_DIR",
    "GEOFRAME_HOST",
    "GEOFRAME_PORT",
    "GEOFRAME_ALLOWED_HOSTS",
];

/// ログの詳しさ。`GEOFRAME_LOG` で指定する（既定 `info`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    pub fn as_tracing(self) -> tracing::Level {
        match self {
            Self::Error => tracing::Level::ERROR,
            Self::Warn => tracing::Level::WARN,
            Self::Info => tracing::Level::INFO,
            Self::Debug => tracing::Level::DEBUG,
            Self::Trace => tracing::Level::TRACE,
        }
    }
}

/// `serve` の起動引数で上書きできる項目。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Overrides {
    pub listen: Option<SocketAddr>,
    pub data_dir: Option<PathBuf>,
}

/// 設定の読み込みエラー。メッセージには設定値そのものを入れない（環境変数の名前と理由だけ）。
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// 待ち受けアドレス（起動引数・環境変数・既定値を解決した結果）。Host 検査のポート判定にも使う。
    pub listen: SocketAddr,
    /// データルート（絶対パス。シンボリックリンクは辿らない）。
    pub data_dir: PathBuf,
    /// 許可する Host 名（小文字、末尾の `.` なし、ポートなし）。`localhost` を必ず含む。
    /// IP リテラル宛は T1.3 の Host 検査が無条件に許可するので、ここには入れない。
    pub allowed_hosts: BTreeSet<String>,
    pub log_level: LogLevel,
    /// 環境変数 `TZ` の値。解釈せず、起動ログに出すためだけに控える。
    pub tz: Option<String>,
}

impl Config {
    /// `lookup` で環境変数を引いて設定を作る。プロセスの環境を直接読まないので、テストで
    /// 並行に走らせても干渉しない（`main` は `|k| std::env::var(k).ok()` を渡す）。
    pub fn load(
        lookup: impl Fn(&str) -> Option<String>,
        overrides: &Overrides,
    ) -> Result<Self, ConfigError> {
        // 空文字は未設定扱い
        let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());

        let listen = match overrides.listen {
            Some(addr) => addr,
            None => {
                let host = match get("GEOFRAME_HOST") {
                    Some(raw) => raw.trim().parse::<IpAddr>().map_err(|_| {
                        ConfigError(
                            "GEOFRAME_HOST は IP アドレス（例: 0.0.0.0、127.0.0.1）で指定してください"
                                .into(),
                        )
                    })?,
                    None => DEFAULT_HOST,
                };
                let port = match get("GEOFRAME_PORT") {
                    Some(raw) => raw.trim().parse::<u16>().map_err(|_| {
                        ConfigError("GEOFRAME_PORT は 0〜65535 の整数で指定してください".into())
                    })?,
                    None => DEFAULT_PORT,
                };
                SocketAddr::new(host, port)
            }
        };

        let data_dir = match &overrides.data_dir {
            Some(path) => path.clone(),
            None => PathBuf::from(get("GEOFRAME_DATA_DIR").unwrap_or(DEFAULT_DATA_DIR.into())),
        };
        let data_dir = std::path::absolute(&data_dir)
            .map_err(|e| ConfigError(format!("データ置き場のパスを絶対パスにできません: {e}")))?;

        let mut allowed_hosts =
            parse_allowed_hosts(&lookup("GEOFRAME_ALLOWED_HOSTS").unwrap_or_default());
        allowed_hosts.insert("localhost".into());

        let log_level = match get("GEOFRAME_LOG") {
            Some(raw) => LogLevel::parse(raw.trim()).ok_or_else(|| {
                ConfigError("GEOFRAME_LOG は error・warn・info・debug・trace のどれかです".into())
            })?,
            None => LogLevel::Info,
        };

        Ok(Self {
            listen,
            data_dir,
            allowed_hosts,
            log_level,
            tz: get("TZ"),
        })
    }

    /// `catalog.db` のパス。
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join(DB_FILE_NAME)
    }

    /// 起動ログ用の要約。**ここに載せた項目だけ**がログに出る（秘密になりうる項目は載せない）。
    pub fn log_summary(&self) -> String {
        format!(
            "listen={} data_dir={} allowed_hosts={} log={:?} tz={}",
            self.listen,
            self.data_dir.display(),
            self.allowed_hosts.len(),
            self.log_level,
            self.tz.as_deref().unwrap_or("(未設定)"),
        )
    }
}

// 派生の Debug にしない。項目が増えたとき、秘密を含む値がうっかり `{:?}` でログへ出るのを防ぐ。
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Config({})", self.log_summary())
    }
}

/// `GEOFRAME_ALLOWED_HOSTS`（カンマ区切り）をホスト名の集合にする。
/// 現行 `config.py:_parse_allowed_hosts` と同じ規則: 小文字にし、末尾の `.` を落とし、
/// `name:port`・`[v6]:port` のポートを無視する（ポートは待ち受けポートとの一致で別に見る）。
pub fn parse_allowed_hosts(raw: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for item in raw.split(',') {
        let mut host = item.trim().to_lowercase();
        while host.ends_with('.') {
            host.pop();
        }
        if host.is_empty() {
            continue;
        }
        if let Some(rest) = host.strip_prefix('[') {
            // [::1]:8080 形式
            host = rest.split(']').next().unwrap_or("").to_string();
        } else if host.matches(':').count() == 1 {
            // name:port / 192.0.2.50:8080
            host = host.split(':').next().unwrap_or("").to_string();
        }
        if !host.is_empty() {
            out.insert(host);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(env: &[(&str, &str)], overrides: &Overrides) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::load(|k| map.get(k).cloned(), overrides)
    }

    fn hosts(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_match_the_python_defaults() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.listen, "0.0.0.0:8080".parse().unwrap());
        assert!(c.data_dir.is_absolute());
        assert!(c.data_dir.ends_with("var"));
        assert_eq!(c.allowed_hosts, hosts(&["localhost"]));
        assert_eq!(c.log_level, LogLevel::Info);
        assert_eq!(c.tz, None);
        assert_eq!(c.db_path(), c.data_dir.join("catalog.db"));
    }

    #[test]
    fn environment_overrides_defaults() {
        let c = load(
            &[
                ("GEOFRAME_DATA_DIR", "/srv/geo-test"),
                ("GEOFRAME_HOST", "127.0.0.1"),
                ("GEOFRAME_PORT", "18080"),
                ("GEOFRAME_ALLOWED_HOSTS", "Frame.LAN., geoframe.local:8080"),
                ("GEOFRAME_LOG", "DEBUG"),
                ("TZ", "Asia/Tokyo"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.listen, "127.0.0.1:18080".parse().unwrap());
        assert_eq!(c.data_dir, PathBuf::from("/srv/geo-test"));
        assert_eq!(
            c.allowed_hosts,
            hosts(&["frame.lan", "geoframe.local", "localhost"])
        );
        assert_eq!(c.log_level, LogLevel::Debug);
        assert_eq!(c.tz.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn command_line_beats_environment() {
        let overrides = Overrides {
            listen: Some("127.0.0.1:9000".parse().unwrap()),
            data_dir: Some(PathBuf::from("/srv/from-cli")),
        };
        let c = load(
            &[
                ("GEOFRAME_DATA_DIR", "/srv/from-env"),
                ("GEOFRAME_HOST", "0.0.0.0"),
                ("GEOFRAME_PORT", "8081"),
            ],
            &overrides,
        )
        .unwrap();
        assert_eq!(c.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(c.data_dir, PathBuf::from("/srv/from-cli"));
    }

    #[test]
    fn listen_option_replaces_host_and_port_even_if_env_is_invalid() {
        // --listen が host/port の両方を決めるので、環境変数の不正は問われない
        let overrides = Overrides {
            listen: Some("127.0.0.1:9000".parse().unwrap()),
            data_dir: None,
        };
        let c = load(&[("GEOFRAME_PORT", "abc")], &overrides).unwrap();
        assert_eq!(c.listen.port(), 9000);
    }

    #[test]
    fn empty_values_count_as_unset() {
        let c = load(
            &[
                ("GEOFRAME_DATA_DIR", ""),
                ("GEOFRAME_HOST", "  "),
                ("GEOFRAME_PORT", ""),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.listen, "0.0.0.0:8080".parse().unwrap());
        assert!(c.data_dir.ends_with("var"));
    }

    #[test]
    fn invalid_values_are_errors_without_echoing_the_value() {
        for (key, value) in [
            ("GEOFRAME_PORT", "abc"),
            ("GEOFRAME_PORT", "70000"),
            ("GEOFRAME_HOST", "not-an-ip"),
            ("GEOFRAME_LOG", "loud"),
        ] {
            let err = load(&[(key, value)], &Overrides::default()).unwrap_err();
            assert!(err.0.contains(key), "{err}");
            assert!(!err.0.contains(value), "{err}");
        }
    }

    #[test]
    fn ipv6_host_is_accepted() {
        let c = load(&[("GEOFRAME_HOST", "::1")], &Overrides::default()).unwrap();
        assert_eq!(c.listen, "[::1]:8080".parse().unwrap());
    }

    #[test]
    fn allowed_hosts_parsing_matches_python() {
        // config.py:_parse_allowed_hosts と同じ入出力
        assert_eq!(
            parse_allowed_hosts("geoframe.local:8080,frame.lan"),
            hosts(&["geoframe.local", "frame.lan"])
        );
        assert_eq!(parse_allowed_hosts(" A.B. , ,"), hosts(&["a.b"]));
        assert_eq!(parse_allowed_hosts("[::1]:8080"), hosts(&["::1"]));
        assert_eq!(
            parse_allowed_hosts("192.0.2.50:8080"),
            hosts(&["192.0.2.50"])
        );
        // コロンが2つ以上（ブラケットなしの IPv6）はそのまま
        assert_eq!(parse_allowed_hosts("fe80::1"), hosts(&["fe80::1"]));
        assert_eq!(parse_allowed_hosts(""), hosts(&[]));
        assert_eq!(parse_allowed_hosts("[]"), hosts(&[]));
    }

    #[test]
    fn debug_and_summary_do_not_leak_unlisted_values() {
        let c = load(
            &[("GEOFRAME_ALLOWED_HOSTS", "private-name.example")],
            &Overrides::default(),
        )
        .unwrap();
        let text = format!("{c:?} {}", c.log_summary());
        // 許可ホスト名は件数だけ。名前そのものは出さない
        assert!(!text.contains("private-name"), "{text}");
        assert!(text.contains("allowed_hosts=2"), "{text}");
    }

    /// `docs/reference/configuration.md` にある `GEOFRAME_*` がすべて、P1 で読む項目か
    /// 「未対応」の一覧のどちらかに載っていること（Python に項目が増えたら、ここで気づく）。
    #[test]
    fn every_documented_variable_is_classified() {
        let doc = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/configuration.md"),
        )
        .unwrap();
        let mut documented = BTreeSet::new();
        for line in doc.lines().filter(|l| l.starts_with("- `GEOFRAME_")) {
            let name = line[3..].split('`').next().unwrap();
            documented.insert(name.to_string());
        }
        assert!(documented.len() >= 30, "{documented:?}");
        for name in &documented {
            let known =
                SUPPORTED_ENV.contains(&name.as_str()) || UNSUPPORTED_ENV.contains(&name.as_str());
            assert!(known, "{name} が config.rs の分類に無い");
        }
        // 逆向き: 分類側にだけある名前（綴りの誤りなど）を防ぐ
        for name in SUPPORTED_ENV.iter().chain(UNSUPPORTED_ENV) {
            assert!(
                documented.contains(*name),
                "{name} は設定リファレンスに無い"
            );
        }
    }
}
