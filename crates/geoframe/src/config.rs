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
//! | 位置推定の窓（1 段目。推定と管理画面の内訳の区切り） | なし | `GEOFRAME_INFER_MIN` | `15`（分） |
//! | カメラ時計のずれの補正（推定する写真の時刻に足す。負でもよい） | なし | `GEOFRAME_INFER_OFFSET_MIN` | `0`（分） |
//! | embedding の場所（管理画面の AI 件数が数えるモデル） | なし | `GEOFRAME_EMBED_BACKEND` | `gemini` |
//! | タイルキャッシュの上限（管理画面のディスク表示） | なし | `GEOFRAME_TILE_CACHE_MAX_MB` | `16384` |
//! | 取り込み（DB への書き込み・同期サイクル）を有効にするか（**Rust だけの項目**） | なし | `GEOFRAME_INGEST`（`0`/`1`） | `0`（無効） |
//! | 同期サイクルの間隔（分） | なし | `GEOFRAME_DRIVE_POLL_MIN` | `10` |
//! | ローカル写真の置き場（`DATA_DIR` からの相対。絶対でも、解決した先が `DATA_DIR` の中なら可） | なし | `GEOFRAME_PHOTOS_DIR` | `photos` |
//! | 同期 1 サイクルで derivative を作る枚数 | なし | `GEOFRAME_FETCH_PER_CYCLE` | `25` |
//! | Drive のサービスアカウント鍵 JSON のパス（相対はカレントディレクトリ基準。現行は `app/` 基準） | なし | `GEOFRAME_DRIVE_KEY` | `drive_key.json` |
//! | 取り込む Drive のフォルダ（名前、または 20 字以上の英数字なら ID） | なし | `GEOFRAME_DRIVE_FOLDER` | `GeoFrame` |
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
    "GEOFRAME_QUALITY_PER_CYCLE",
    // P3: 管理機能（地図タイル）
    "GEOFRAME_TILE_UA",
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
    // T1.4: 管理画面の読み取り API（/api/admin/status）の値を Python と同じにするために読む。
    // 取り込みや AI の処理はまだ Rust に無いので、この3つは表示にだけ効く
    "GEOFRAME_INFER_MIN",
    "GEOFRAME_EMBED_BACKEND",
    "GEOFRAME_TILE_CACHE_MAX_MB",
    // T2.1: 同期サイクルの間隔
    "GEOFRAME_DRIVE_POLL_MIN",
    // T2.3: ローカル写真の索引（index_local・local_derivatives）
    "GEOFRAME_PHOTOS_DIR",
    "GEOFRAME_FETCH_PER_CYCLE",
    // T2.4: Drive の索引と取得
    "GEOFRAME_DRIVE_KEY",
    "GEOFRAME_DRIVE_FOLDER",
    // T2.5: 位置の推定（`GEOFRAME_INFER_MIN` は T1.4 から読んでいて、推定の窓にもなった）
    "GEOFRAME_INFER_OFFSET_MIN",
];

pub const DEFAULT_INFER_MIN: f64 = 15.0;
/// カメラ時計のずれの補正（分）の既定。現行の `GEOFRAME_INFER_OFFSET_MIN`。
pub const DEFAULT_INFER_OFFSET_MIN: f64 = 0.0;
pub const DEFAULT_TILE_CACHE_MAX_MB: i64 = 16384;
/// 同期サイクルの間隔（分）の既定。現行の `GEOFRAME_DRIVE_POLL_MIN`。
pub const DEFAULT_DRIVE_POLL_MIN: f64 = 10.0;
/// ローカル写真の置き場の既定（`DATA_DIR` からの相対）。ディレクトリ名は変えない（`display`・`path` 列の
/// 相対パスが `photos/...` で、配信 URL も兼ねる）。
pub const DEFAULT_PHOTOS_DIR: &str = "photos";
/// 同期 1 サイクルで derivative を作る枚数の既定。現行の `FETCH_PER_CYCLE`。
pub const DEFAULT_FETCH_PER_CYCLE: u32 = 25;
/// Drive の鍵ファイルの既定の名前。現行は `app/drive_key.json`。Rust は `app/` が無いので、
/// 相対パスはカレントディレクトリ基準（`GEOFRAME_DATA_DIR` の既定 `./var` と同じ扱い）。本番は絶対パス。
pub const DEFAULT_DRIVE_KEY: &str = "drive_key.json";
/// 取り込む Drive のフォルダの既定の名前。
pub const DEFAULT_DRIVE_FOLDER: &str = "GeoFrame";
/// `GEOFRAME_DRIVE_POLL_MIN` の上限（分）。1 年。これを超える値は設定の誤りとして断る。
const MAX_DRIVE_POLL_MIN: f64 = 525_600.0;
/// 位置推定の段階窓のうち固定のもの（秒）。現行の `INFER_WINDOWS_S = sorted({INFER_WINDOW_S, 2h, 12h})`。
const FIXED_INFER_WINDOWS_S: [f64; 2] = [2.0 * 3600.0, 12.0 * 3600.0];

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

/// embedding の計算場所（`GEOFRAME_EMBED_BACKEND`）。管理画面の AI の件数は、今のモデルの行だけを数える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedBackend {
    Gemini,
    Local,
}

impl EmbedBackend {
    /// `photo_ai_features.embedding_model` に入るモデル名（現行の `ai_search.EMBED_MODEL`）。
    pub fn model(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-embedding-2",
            Self::Local => "embeddinggemma-2",
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

#[derive(Clone, PartialEq)]
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
    /// 位置推定の1段目の窓（分）。`GEOFRAME_INFER_MIN`。管理画面の「推定の内訳」の区切りに使う。
    pub infer_min: f64,
    /// カメラ時計のずれの補正（分）。`GEOFRAME_INFER_OFFSET_MIN`。位置を推定する写真の撮影時刻に足す
    /// （負でもよい）。現行は検証なしで `float()` に通すが、Rust は有限の数だけを受ける。
    pub infer_offset_min: f64,
    pub embed_backend: EmbedBackend,
    /// `GEOFRAME_EMBED_BACKEND` が `gemini`・`local` のどちらでもなかった（現行と同じく `gemini` に
    /// 落とした）。起動ログで知らせる。
    pub embed_backend_invalid: bool,
    pub tile_cache_max_mb: i64,
    /// 取り込み（DB への書き込みと同期サイクル）を有効にするか。`GEOFRAME_INGEST`。既定は無効。
    /// 本番は P5 の切替まで Python が書き手なので、誤って二重に書かないよう、明示しない限り Rust は書かない。
    pub ingest: bool,
    /// 同期サイクルの間隔（分）。`GEOFRAME_DRIVE_POLL_MIN`（現行と同じく分で、小数も可）。
    pub drive_poll_min: f64,
    /// `GEOFRAME_PHOTOS_DIR` の値そのまま（`DATA_DIR` からの相対、または絶対）。現行と同じく
    /// `DATA_DIR / 値`（絶対ならその値）を解決して使う。`DATA_DIR` の外に解決されたら、索引は断る
    /// （[`crate::local`]）。
    photos_dir_setting: PathBuf,
    /// 同期 1 サイクルで derivative を作る枚数。`GEOFRAME_FETCH_PER_CYCLE`（0 以上の整数）。
    pub fetch_per_cycle: u32,
    /// Drive のサービスアカウント鍵 JSON のパス（絶対パス）。`GEOFRAME_DRIVE_KEY`。**持つのはパスだけ**で、
    /// 中身は [`crate::drive`] が段の中で読む（設定にも起動ログにも載せない）。
    pub drive_key: PathBuf,
    /// 取り込む Drive のフォルダ。`GEOFRAME_DRIVE_FOLDER`（名前、または 20 字以上の英数字・`_`・`-` なら ID）。
    /// 起動ログには載せない（フォルダ名は利用者の情報）。
    pub drive_folder: String,
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

        let infer_min = match get("GEOFRAME_INFER_MIN") {
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| {
                    ConfigError("GEOFRAME_INFER_MIN は 0 以上の数（分）で指定してください".into())
                })?,
            None => DEFAULT_INFER_MIN,
        };
        let infer_offset_min = match get("GEOFRAME_INFER_OFFSET_MIN") {
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| {
                    ConfigError(
                        "GEOFRAME_INFER_OFFSET_MIN は数（分。負でもよい）で指定してください".into(),
                    )
                })?,
            None => DEFAULT_INFER_OFFSET_MIN,
        };
        // 現行は `.strip().lower()` で、gemini・local 以外は警告して gemini に落とす（起動は続ける）
        let (embed_backend, embed_backend_invalid) = match get("GEOFRAME_EMBED_BACKEND") {
            Some(raw) => match raw.trim().to_lowercase().as_str() {
                "gemini" => (EmbedBackend::Gemini, false),
                "local" => (EmbedBackend::Local, false),
                _ => (EmbedBackend::Gemini, true),
            },
            None => (EmbedBackend::Gemini, false),
        };
        let tile_cache_max_mb = match get("GEOFRAME_TILE_CACHE_MAX_MB") {
            Some(raw) => raw.trim().parse::<i64>().map_err(|_| {
                ConfigError("GEOFRAME_TILE_CACHE_MAX_MB は整数（MB）で指定してください".into())
            })?,
            None => DEFAULT_TILE_CACHE_MAX_MB,
        };

        let ingest = match get("GEOFRAME_INGEST") {
            Some(raw) => match raw.trim() {
                "1" => true,
                "0" => false,
                _ => {
                    return Err(ConfigError(
                        "GEOFRAME_INGEST は 0（無効）か 1（有効）で指定してください".into(),
                    ));
                }
            },
            None => false,
        };
        // 現行は `float(...)`。0 以下や非数だと Python は空回りか例外になるので、Rust は断る
        let drive_poll_min = match get("GEOFRAME_DRIVE_POLL_MIN") {
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= MAX_DRIVE_POLL_MIN)
                .ok_or_else(|| {
                    ConfigError(
                        "GEOFRAME_DRIVE_POLL_MIN は 0 より大きい数（分。上限 525600）で指定してください"
                            .into(),
                    )
                })?,
            None => DEFAULT_DRIVE_POLL_MIN,
        };

        let photos_dir_setting = PathBuf::from(
            get("GEOFRAME_PHOTOS_DIR")
                .map(|v| v.trim().to_string())
                .unwrap_or_else(|| DEFAULT_PHOTOS_DIR.into()),
        );
        // 現行は `int(...)`（負の値は SQLite の `LIMIT` で「無制限」になるが、意図とは思えないので断る）
        let fetch_per_cycle = match get("GEOFRAME_FETCH_PER_CYCLE") {
            Some(raw) => raw.trim().parse::<u32>().map_err(|_| {
                ConfigError("GEOFRAME_FETCH_PER_CYCLE は 0 以上の整数で指定してください".into())
            })?,
            None => DEFAULT_FETCH_PER_CYCLE,
        };

        // 現行は `ROOT / 値`（`ROOT` は `app/`）。Rust は `app/` が無いので、相対パスはカレントディレクトリ基準にする
        let drive_key = std::path::absolute(PathBuf::from(
            get("GEOFRAME_DRIVE_KEY")
                .map(|v| v.trim().to_string())
                .unwrap_or_else(|| DEFAULT_DRIVE_KEY.into()),
        ))
        .map_err(|e| ConfigError(format!("GEOFRAME_DRIVE_KEY を絶対パスにできません: {e}")))?;
        let drive_folder = get("GEOFRAME_DRIVE_FOLDER")
            .unwrap_or_else(|| DEFAULT_DRIVE_FOLDER.into())
            .trim()
            .to_string();

        Ok(Self {
            listen,
            data_dir,
            allowed_hosts,
            log_level,
            tz: get("TZ"),
            infer_min,
            infer_offset_min,
            embed_backend,
            embed_backend_invalid,
            tile_cache_max_mb,
            ingest,
            drive_poll_min,
            photos_dir_setting,
            fetch_per_cycle,
            drive_key,
            drive_folder,
        })
    }

    /// ローカル写真の置き場（字句のパス。リンクは辿っていない）。現行の `DATA_DIR / GEOFRAME_PHOTOS_DIR`。
    pub fn photos_dir(&self) -> PathBuf {
        self.data_dir.join(&self.photos_dir_setting)
    }

    /// 同期サイクルの間隔。
    pub fn drive_poll(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.drive_poll_min * 60.0)
    }

    /// 取り込みの一時ダウンロード先（`DATA_DIR/tmp`）。`derivatives/` の外に置く（配信の許可 prefix の外）。
    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("tmp")
    }

    /// derivative の置き場（`DATA_DIR/derivatives`）。
    pub fn derivatives_dir(&self) -> PathBuf {
        self.data_dir.join("derivatives")
    }

    /// 永続キャッシュ（`DATA_DIR/.cache.json`）。
    pub fn cache_file(&self) -> PathBuf {
        self.data_dir.join(".cache.json")
    }

    /// 位置推定の段階窓（秒、昇順、重複なし）。現行の `INFER_WINDOWS_S`
    /// （`sorted({INFER_WINDOW_S, 2時間, 12時間})`）。
    pub fn infer_windows_s(&self) -> Vec<f64> {
        let mut windows = vec![self.infer_min * 60.0];
        windows.extend(FIXED_INFER_WINDOWS_S);
        windows.sort_by(f64::total_cmp);
        windows.dedup();
        windows
    }

    /// 位置推定の最大の窓（秒）。現行の `INFER_WINDOWS_S[-1]`（`GEOFRAME_INFER_MIN` が 12 時間より長ければそれ）。
    pub fn infer_widest_s(&self) -> f64 {
        self.infer_windows_s().last().copied().unwrap_or(0.0)
    }

    /// カメラ時計のずれの補正（秒）。現行の `INFER_OFFSET_S`。
    pub fn infer_offset_s(&self) -> f64 {
        self.infer_offset_min * 60.0
    }

    /// `catalog.db` のパス。
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join(DB_FILE_NAME)
    }

    /// 起動ログ用の要約。**ここに載せた項目だけ**がログに出る（秘密になりうる項目は載せない）。
    pub fn log_summary(&self) -> String {
        format!(
            "listen={} data_dir={} allowed_hosts={} log={:?} tz={} infer_min={} infer_offset_min={} embed={} tile_cache_max_mb={} ingest={} drive_poll_min={} fetch_per_cycle={}",
            self.listen,
            self.data_dir.display(),
            self.allowed_hosts.len(),
            self.log_level,
            self.tz.as_deref().unwrap_or("(未設定)"),
            self.infer_min,
            self.infer_offset_min,
            self.embed_backend.model(),
            self.tile_cache_max_mb,
            if self.ingest { 1 } else { 0 },
            self.drive_poll_min,
            self.fetch_per_cycle,
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
    fn admin_display_settings_default_to_the_python_values() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.infer_min, 15.0);
        assert_eq!(c.infer_windows_s(), [900.0, 7200.0, 43200.0]);
        assert_eq!(c.embed_backend, EmbedBackend::Gemini);
        assert!(!c.embed_backend_invalid);
        assert_eq!(c.embed_backend.model(), "gemini-embedding-2");
        assert_eq!(c.tile_cache_max_mb, 16384);
    }

    #[test]
    fn admin_display_settings_are_read_from_the_environment() {
        let c = load(
            &[
                ("GEOFRAME_INFER_MIN", " 30 "),
                ("GEOFRAME_EMBED_BACKEND", " Local "),
                ("GEOFRAME_TILE_CACHE_MAX_MB", "2048"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.infer_windows_s(), [1800.0, 7200.0, 43200.0]);
        assert_eq!(c.embed_backend, EmbedBackend::Local);
        assert_eq!(c.embed_backend.model(), "embeddinggemma-2");
        assert_eq!(c.tile_cache_max_mb, 2048);
        // 窓は昇順・重複なし（現行の `sorted({…})`）。固定の窓と同じ値や、それより大きい値
        let same = load(&[("GEOFRAME_INFER_MIN", "120")], &Overrides::default()).unwrap();
        assert_eq!(same.infer_windows_s(), [7200.0, 43200.0]);
        let wide = load(&[("GEOFRAME_INFER_MIN", "1000")], &Overrides::default()).unwrap();
        assert_eq!(wide.infer_windows_s(), [7200.0, 43200.0, 60000.0]);
        let fractional = load(&[("GEOFRAME_INFER_MIN", "0.5")], &Overrides::default()).unwrap();
        assert_eq!(fractional.infer_windows_s(), [30.0, 7200.0, 43200.0]);
    }

    #[test]
    fn infer_offset_defaults_to_zero_and_accepts_negative_and_fractional_minutes() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.infer_offset_min, 0.0);
        assert_eq!(c.infer_offset_s(), 0.0);
        assert_eq!(c.infer_widest_s(), 43200.0);
        let c = load(
            &[
                ("GEOFRAME_INFER_OFFSET_MIN", " -7.5 "),
                ("GEOFRAME_INFER_MIN", "1000"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.infer_offset_s(), -450.0);
        // 最大の窓は、1 段目が 12 時間より長ければそちら
        assert_eq!(c.infer_widest_s(), 60000.0);
        for bad in ["abc", "inf", "NaN"] {
            let err =
                load(&[("GEOFRAME_INFER_OFFSET_MIN", bad)], &Overrides::default()).unwrap_err();
            assert!(err.0.contains("GEOFRAME_INFER_OFFSET_MIN"), "{err}");
            assert!(!err.0.contains(bad), "{err}");
        }
    }

    #[test]
    fn an_unknown_embed_backend_falls_back_to_gemini_like_python() {
        let c = load(
            &[("GEOFRAME_EMBED_BACKEND", "other")],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.embed_backend, EmbedBackend::Gemini);
        assert!(c.embed_backend_invalid);
        // 空文字は未設定と同じ
        let c = load(&[("GEOFRAME_EMBED_BACKEND", "")], &Overrides::default()).unwrap();
        assert!(!c.embed_backend_invalid);
    }

    #[test]
    fn invalid_admin_display_settings_are_errors_without_echoing_the_value() {
        for (key, value) in [
            ("GEOFRAME_INFER_MIN", "abc"),
            ("GEOFRAME_INFER_MIN", "-5"),
            ("GEOFRAME_INFER_MIN", "inf"),
            ("GEOFRAME_INFER_MIN", "NaN"),
            ("GEOFRAME_TILE_CACHE_MAX_MB", "1.5"),
            ("GEOFRAME_TILE_CACHE_MAX_MB", "big"),
        ] {
            let err = load(&[(key, value)], &Overrides::default()).unwrap_err();
            assert!(err.0.contains(key), "{err}");
            assert!(!err.0.contains(value), "{err}");
        }
    }

    #[test]
    fn ingest_is_off_by_default_and_poll_defaults_to_ten_minutes() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert!(!c.ingest);
        assert_eq!(c.drive_poll_min, 10.0);
        assert_eq!(c.drive_poll(), std::time::Duration::from_secs(600));
        // 空文字は未設定と同じ（EnvironmentFile の `KEY=` の事故）
        let c = load(
            &[("GEOFRAME_INGEST", ""), ("GEOFRAME_DRIVE_POLL_MIN", " ")],
            &Overrides::default(),
        )
        .unwrap();
        assert!(!c.ingest);
        assert_eq!(c.drive_poll_min, 10.0);
        assert!(
            c.log_summary()
                .contains("ingest=0 drive_poll_min=10 fetch_per_cycle=25")
        );
    }

    #[test]
    fn ingest_and_poll_are_read_from_the_environment() {
        let c = load(
            &[
                ("GEOFRAME_INGEST", "1"),
                ("GEOFRAME_DRIVE_POLL_MIN", " 0.5 "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert!(c.ingest);
        assert_eq!(c.drive_poll(), std::time::Duration::from_secs(30));
        assert!(c.log_summary().contains("ingest=1 drive_poll_min=0.5 "));
        assert!(
            !load(&[("GEOFRAME_INGEST", "0")], &Overrides::default())
                .unwrap()
                .ingest
        );
        let c = load(&[("GEOFRAME_DRIVE_POLL_MIN", "1e1")], &Overrides::default()).unwrap();
        assert_eq!(c.drive_poll_min, 10.0);
    }

    #[test]
    fn invalid_ingest_and_poll_are_errors_without_echoing_the_value() {
        for (key, value) in [
            ("GEOFRAME_INGEST", "true"),
            ("GEOFRAME_INGEST", "2"),
            ("GEOFRAME_INGEST", "yes"),
            ("GEOFRAME_DRIVE_POLL_MIN", "abc"),
            ("GEOFRAME_DRIVE_POLL_MIN", "0"),
            ("GEOFRAME_DRIVE_POLL_MIN", "-3"),
            ("GEOFRAME_DRIVE_POLL_MIN", "inf"),
            ("GEOFRAME_DRIVE_POLL_MIN", "NaN"),
            ("GEOFRAME_DRIVE_POLL_MIN", "525601"),
        ] {
            let err = load(&[(key, value)], &Overrides::default()).unwrap_err();
            assert!(err.0.contains(key), "{err}");
            // 1〜2 文字の値（"0"・"2"）は、メッセージの文言と偶然重なるので除く
            if value.len() > 2 {
                assert!(!err.0.contains(value), "{err}");
            }
        }
    }

    #[test]
    fn drive_key_and_folder_follow_the_python_defaults() {
        let cwd = std::env::current_dir().unwrap();
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.drive_key, cwd.join("drive_key.json"));
        assert_eq!(c.drive_folder, "GeoFrame");
        let c = load(
            &[
                ("GEOFRAME_DRIVE_KEY", "/etc/g/key.json"),
                ("GEOFRAME_DRIVE_FOLDER", " Photos "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.drive_key, PathBuf::from("/etc/g/key.json"));
        assert_eq!(c.drive_folder, "Photos");
        // 相対パスはカレントディレクトリ基準、空文字は未設定と同じ
        let c = load(
            &[
                ("GEOFRAME_DRIVE_KEY", "k/x.json"),
                ("GEOFRAME_DRIVE_FOLDER", " "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.drive_key, cwd.join("k/x.json"));
        assert_eq!(c.drive_folder, "GeoFrame");
        // 鍵のパスもフォルダ名も、Debug・起動ログの要約には載せない
        let c = load(
            &[
                ("GEOFRAME_DRIVE_KEY", "/etc/g/secret-name.json"),
                ("GEOFRAME_DRIVE_FOLDER", "PrivateFolderName"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        for text in [format!("{c:?}"), c.log_summary()] {
            assert!(!text.contains("secret-name") && !text.contains("PrivateFolderName"));
        }
    }

    #[test]
    fn photos_dir_and_fetch_per_cycle_follow_the_python_defaults() {
        let c = load(&[("GEOFRAME_DATA_DIR", "/srv/g")], &Overrides::default()).unwrap();
        assert_eq!(c.photos_dir(), PathBuf::from("/srv/g/photos"));
        assert_eq!(c.fetch_per_cycle, 25);
        let c = load(
            &[
                ("GEOFRAME_DATA_DIR", "/srv/g"),
                ("GEOFRAME_PHOTOS_DIR", " pics "),
                ("GEOFRAME_FETCH_PER_CYCLE", "0"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.photos_dir(), PathBuf::from("/srv/g/pics"));
        assert_eq!(c.fetch_per_cycle, 0);
        // 絶対パスは `DATA_DIR / 値` で値そのものになる（現行の `Path` の結合と同じ）
        let c = load(
            &[
                ("GEOFRAME_DATA_DIR", "/srv/g"),
                ("GEOFRAME_PHOTOS_DIR", "/srv/g/in"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.photos_dir(), PathBuf::from("/srv/g/in"));
        // 空文字は未設定と同じ
        let c = load(&[("GEOFRAME_PHOTOS_DIR", " ")], &Overrides::default()).unwrap();
        assert!(c.photos_dir().ends_with("photos"));
        for value in ["abc", "-1", "1.5", "99999999999"] {
            let err = load(
                &[("GEOFRAME_FETCH_PER_CYCLE", value)],
                &Overrides::default(),
            )
            .unwrap_err();
            assert!(err.0.contains("GEOFRAME_FETCH_PER_CYCLE"), "{err}");
            assert!(!err.0.contains(value), "{err}");
        }
    }

    #[test]
    fn data_paths_for_ingest_are_under_the_data_dir() {
        let c = load(&[("GEOFRAME_DATA_DIR", "/srv/g")], &Overrides::default()).unwrap();
        assert_eq!(c.tmp_dir(), PathBuf::from("/srv/g/tmp"));
        assert_eq!(c.derivatives_dir(), PathBuf::from("/srv/g/derivatives"));
        assert_eq!(c.cache_file(), PathBuf::from("/srv/g/.cache.json"));
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
