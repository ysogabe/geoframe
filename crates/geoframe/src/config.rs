//! 設定。旧 Python 版（`app/geoframe/config.py`。P5b で削除）と同じ `GEOFRAME_*` の名前・既定値で読む
//!（AI の一部の既定だけ違う）。名前と既定値の一覧は `docs/reference/configuration.md`（手で保ち、
//! `tests/env_reference.rs` が、コードが読む名前の漏れを検査する）。
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
//! | タイルキャッシュの上限（管理画面のディスク表示と、掃除） | なし | `GEOFRAME_TILE_CACHE_MAX_MB` | `16384` |
//! | 地図タイル・地名検索の上流へ送る User-Agent（連絡先入りにする。ログには出さない） | なし | `GEOFRAME_TILE_UA` | `GeoFrame/0.1 (personal photo frame)` |
//! | 地図タイルの上流取得の毎分の上限（全体。0 は上流へ行かない） | なし | `GEOFRAME_TILE_FETCH_PER_MIN` | `240` |
//! | 取り込み（DB への書き込み・同期サイクル）を有効にするか（**Rust だけの項目**） | なし | `GEOFRAME_INGEST`（`0`/`1`） | `0`（無効） |
//! | 旅行ブログ（`/blog-admin`・`/api/blog/*`）を有効にするか（**Rust だけの項目**） | なし | `GEOFRAME_BLOG`（`0`/`1`） | `0`（無効） |
//! | 同期サイクルの間隔（分） | なし | `GEOFRAME_DRIVE_POLL_MIN` | `10` |
//! | ローカル写真の置き場（`DATA_DIR` からの相対。絶対でも、解決した先が `DATA_DIR` の中なら可） | なし | `GEOFRAME_PHOTOS_DIR` | `photos` |
//! | 同期 1 サイクルで derivative を作る枚数 | なし | `GEOFRAME_FETCH_PER_CYCLE` | `25` |
//! | 同期 1 サイクルで品質を解析する枚数（0 は 1 として扱う） | なし | `GEOFRAME_QUALITY_PER_CYCLE` | `GEOFRAME_FETCH_PER_CYCLE` と同じ |
//! | Drive のサービスアカウント鍵 JSON のパス（相対はカレントディレクトリ基準。現行は `app/` 基準） | なし | `GEOFRAME_DRIVE_KEY` | `drive_key.json` |
//! | 旅行ブログの旅程 JSON の置き場（**Rust だけの項目**。あれば埋め込みの代わりにこのディレクトリの `*.json` を読む。相対はカレントディレクトリ基準） | なし | `GEOFRAME_BLOG_PLANS_DIR` | （未設定＝バイナリに埋め込んだもの） |
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
//! Rust は `.env` を **読まない**（旧 Python 版は `app/.env` を読んだ）。単一バイナリには `app/` が無く、本番は systemd の
//! `EnvironmentFile=/etc/default/geoframe` が同じ役目を持っているため。手元で試すときは
//! `set -a; . ./環境ファイル; set +a` してから起動する。詳細は docs/rust-migration/server.md。
//!
//! # 秘密情報
//!
//! P1 の設定項目に秘密の値は無い。P2 以降で鍵を扱う項目（`GEOFRAME_DRIVE_KEY` など）が入るときも、
//! 設定が持つのは**鍵ファイルのパスだけ**で、中身は設定の型に載せない。`Config` の `Debug`・起動
//! ログ（[`Config::log_summary`]）に出すのは下に挙げた項目だけで、秘密になりうる値が増えたら
//! この一覧に足さない限りログへ出ない作りにしてある。
//!
//! 例外は `GET /api/admin/config`（管理画面の設定タブ。`admin_config.rs`）だけで、ユーザーの決定（2026-10-30）で
//! 鍵のパス・接続先・連絡先を含む全項目を**設定どおりの値**で出す（鍵の**中身**は `Config` が持たないので出ない）。
//! ログ・`ai-jobs status` は今までどおり出さない。出どころは [`Config::sources`]、項目の列挙は [`EnvItem`]。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::ai::settings::AiConfig;

pub const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_DATA_DIR: &str = "var";
pub const DB_FILE_NAME: &str = "catalog.db";

pub const DEFAULT_INFER_MIN: f64 = 15.0;
/// カメラ時計のずれの補正（分）の既定。現行の `GEOFRAME_INFER_OFFSET_MIN`。
pub const DEFAULT_INFER_OFFSET_MIN: f64 = 0.0;
pub const DEFAULT_TILE_CACHE_MAX_MB: i64 = 16384;
/// 地図タイルの上流へ送る User-Agent の既定。現行の `TILE_UA`。OSM の利用規約は連絡先入りを求めるので、
/// 本番は `GEOFRAME_TILE_UA` で与える（値は起動ログにも載せない）。
pub const DEFAULT_TILE_UA: &str = "GeoFrame/0.1 (personal photo frame)";
/// 地図タイルの上流取得（キャッシュミス）の流量の上限（枚/分、全体）の既定。現行の `TILE_FETCH_PER_MIN`。
pub const DEFAULT_TILE_FETCH_PER_MIN: i64 = 240;
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

    /// 設定に書く小文字の名前（`GEOFRAME_LOG` の値）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
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
    /// 設定に書く小文字の名前（`GEOFRAME_EMBED_BACKEND` の値）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Local => "local",
        }
    }

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

/// 設定値の出どころ（`GET /api/admin/config` の `source`）。AI 設定 JSON 由来の値は API が別に `ai_config` と書く。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 起動引数（`--listen`・`--data-dir`）。
    Arg,
    /// 環境変数。
    Env,
    /// 既定値（環境変数が未設定か空）。
    Default,
}

/// 設定 API での出し方。2026-10-30 のユーザーの決定で、どの項目も**値**を出す（`docs/admin-rework/design.md` の「安全面」）。
/// `File` は値（パス）に加えてファイルの有無も出す項目。ログ・`ai-jobs status` は今までどおり値を出さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    Value,
    /// 値（パス）に加えて、ファイルの有無（`state`）を出す。
    File,
}

/// Rust が読む環境変数を 1 つ 1 つ列挙したもの（`docs/reference/configuration.md` の「Rust が読む環境変数」と同じ集合。
/// `tests/env_reference.rs` が名前を照合する）。[`EnvItem::name`]・[`EnvItem::exposure`] と、設定 API の値の組み立て
/// （`admin_config.rs`）は**ワイルドカード無しの `match`** なので、項目を足して枝を書き忘れるとコンパイルで落ちる。
/// [`EnvItem::ALL`] への入れ忘れだけは、`config.rs` と `env_reference.rs` の試験が捕まえる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvItem {
    DataDir,
    Host,
    Port,
    AllowedHosts,
    Log,
    Blog,
    Ingest,
    DrivePollMin,
    PhotosDir,
    FetchPerCycle,
    QualityPerCycle,
    DriveKey,
    DriveFolder,
    BlogPlansDir,
    InferMin,
    InferOffsetMin,
    TileUa,
    TileCacheMaxMb,
    TileFetchPerMin,
    AiConfig,
    AiEnabled,
    AiPollS,
    AiBacklogPollS,
    AiEmbedPerCycle,
    AiJudgePerCycle,
    AiSearchPerMin,
    LocSuggest,
    LocSuggestCluePerCycle,
    LocSuggestGeminiPerCycle,
    EmbedBackend,
    AiTimeWindowMin,
    AiSimilarity,
    AiShortlistMax,
    AiVisualSimilarity,
    RankModel,
}

impl EnvItem {
    /// 全項目（設定リファレンスの並び）。
    pub const ALL: &'static [EnvItem] = &[
        EnvItem::DataDir,
        EnvItem::Host,
        EnvItem::Port,
        EnvItem::AllowedHosts,
        EnvItem::Log,
        EnvItem::Blog,
        EnvItem::Ingest,
        EnvItem::DrivePollMin,
        EnvItem::PhotosDir,
        EnvItem::FetchPerCycle,
        EnvItem::QualityPerCycle,
        EnvItem::DriveKey,
        EnvItem::DriveFolder,
        EnvItem::BlogPlansDir,
        EnvItem::InferMin,
        EnvItem::InferOffsetMin,
        EnvItem::TileUa,
        EnvItem::TileCacheMaxMb,
        EnvItem::TileFetchPerMin,
        EnvItem::AiConfig,
        EnvItem::AiEnabled,
        EnvItem::AiPollS,
        EnvItem::AiBacklogPollS,
        EnvItem::AiEmbedPerCycle,
        EnvItem::AiJudgePerCycle,
        EnvItem::AiSearchPerMin,
        EnvItem::LocSuggest,
        EnvItem::LocSuggestCluePerCycle,
        EnvItem::LocSuggestGeminiPerCycle,
        EnvItem::EmbedBackend,
        EnvItem::AiTimeWindowMin,
        EnvItem::AiSimilarity,
        EnvItem::AiShortlistMax,
        EnvItem::AiVisualSimilarity,
        EnvItem::RankModel,
    ];

    pub fn name(self) -> &'static str {
        match self {
            EnvItem::DataDir => "GEOFRAME_DATA_DIR",
            EnvItem::Host => "GEOFRAME_HOST",
            EnvItem::Port => "GEOFRAME_PORT",
            EnvItem::AllowedHosts => "GEOFRAME_ALLOWED_HOSTS",
            EnvItem::Log => "GEOFRAME_LOG",
            EnvItem::Blog => "GEOFRAME_BLOG",
            EnvItem::Ingest => "GEOFRAME_INGEST",
            EnvItem::DrivePollMin => "GEOFRAME_DRIVE_POLL_MIN",
            EnvItem::PhotosDir => "GEOFRAME_PHOTOS_DIR",
            EnvItem::FetchPerCycle => "GEOFRAME_FETCH_PER_CYCLE",
            EnvItem::QualityPerCycle => "GEOFRAME_QUALITY_PER_CYCLE",
            EnvItem::DriveKey => "GEOFRAME_DRIVE_KEY",
            EnvItem::DriveFolder => "GEOFRAME_DRIVE_FOLDER",
            EnvItem::BlogPlansDir => "GEOFRAME_BLOG_PLANS_DIR",
            EnvItem::InferMin => "GEOFRAME_INFER_MIN",
            EnvItem::InferOffsetMin => "GEOFRAME_INFER_OFFSET_MIN",
            EnvItem::TileUa => "GEOFRAME_TILE_UA",
            EnvItem::TileCacheMaxMb => "GEOFRAME_TILE_CACHE_MAX_MB",
            EnvItem::TileFetchPerMin => "GEOFRAME_TILE_FETCH_PER_MIN",
            EnvItem::AiConfig => "GEOFRAME_AI_CONFIG",
            EnvItem::AiEnabled => "GEOFRAME_AI_ENABLED",
            EnvItem::AiPollS => "GEOFRAME_AI_POLL_S",
            EnvItem::AiBacklogPollS => "GEOFRAME_AI_BACKLOG_POLL_S",
            EnvItem::AiEmbedPerCycle => "GEOFRAME_AI_EMBED_PER_CYCLE",
            EnvItem::AiJudgePerCycle => "GEOFRAME_AI_JUDGE_PER_CYCLE",
            EnvItem::AiSearchPerMin => "GEOFRAME_AI_SEARCH_PER_MIN",
            EnvItem::LocSuggest => "GEOFRAME_LOC_SUGGEST",
            EnvItem::LocSuggestCluePerCycle => "GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE",
            EnvItem::LocSuggestGeminiPerCycle => "GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE",
            EnvItem::EmbedBackend => "GEOFRAME_EMBED_BACKEND",
            EnvItem::AiTimeWindowMin => "GEOFRAME_AI_TIME_WINDOW_MIN",
            EnvItem::AiSimilarity => "GEOFRAME_AI_SIMILARITY",
            EnvItem::AiShortlistMax => "GEOFRAME_AI_SHORTLIST_MAX",
            EnvItem::AiVisualSimilarity => "GEOFRAME_AI_VISUAL_SIMILARITY",
            EnvItem::RankModel => "GEOFRAME_RANK_MODEL",
        }
    }

    pub fn exposure(self) -> Exposure {
        match self {
            EnvItem::DriveKey | EnvItem::RankModel | EnvItem::AiConfig => Exposure::File,
            EnvItem::DataDir
            | EnvItem::Host
            | EnvItem::Port
            | EnvItem::AllowedHosts
            | EnvItem::Log
            | EnvItem::Blog
            | EnvItem::Ingest
            | EnvItem::DrivePollMin
            | EnvItem::PhotosDir
            | EnvItem::FetchPerCycle
            | EnvItem::QualityPerCycle
            | EnvItem::DriveFolder
            | EnvItem::BlogPlansDir
            | EnvItem::InferMin
            | EnvItem::InferOffsetMin
            | EnvItem::TileUa
            | EnvItem::TileCacheMaxMb
            | EnvItem::TileFetchPerMin
            | EnvItem::AiEnabled
            | EnvItem::AiPollS
            | EnvItem::AiBacklogPollS
            | EnvItem::AiEmbedPerCycle
            | EnvItem::AiJudgePerCycle
            | EnvItem::AiSearchPerMin
            | EnvItem::LocSuggest
            | EnvItem::LocSuggestCluePerCycle
            | EnvItem::LocSuggestGeminiPerCycle
            | EnvItem::EmbedBackend
            | EnvItem::AiTimeWindowMin
            | EnvItem::AiSimilarity
            | EnvItem::AiShortlistMax
            | EnvItem::AiVisualSimilarity => Exposure::Value,
        }
    }
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
    pub ai_visual_similarity: f64,
    /// Optional validated linear rank file. Never included in diagnostics (logs, `Debug`). `GET /api/admin/config`
    /// alone shows it as configured (user decision, 2026-10-30).
    pub rank_model_path: PathBuf,
    /// `GEOFRAME_EMBED_BACKEND` が `gemini`・`local` のどちらでもなかった（現行と同じく `gemini` に
    /// 落とした）。起動ログで知らせる。
    pub embed_backend_invalid: bool,
    /// 地図タイルのキャッシュの上限（MB。0 以上）。`GEOFRAME_TILE_CACHE_MAX_MB`。
    pub tile_cache_max_mb: i64,
    /// 地図タイル・地名検索の上流へ送る User-Agent。`GEOFRAME_TILE_UA`（印字可能な ASCII だけ）。連絡先が入りうるので、
    /// `Debug`・起動ログには載せない（`GET /api/admin/config` だけは、ユーザーの決定（2026-10-30）で設定どおりの値を出す）。
    pub tile_ua: String,
    /// 地図タイルの上流取得（キャッシュミス）の毎分の上限（全体。0 以上）。`GEOFRAME_TILE_FETCH_PER_MIN`。
    /// 0 なら上流へ行かない。
    pub tile_fetch_per_min: i64,
    /// 取り込み（DB への書き込みと同期サイクル）を有効にするか。`GEOFRAME_INGEST`。既定は無効。
    /// Python 版と共通の設定をそのまま使っても二重に書かないよう、明示しない限り Rust は書かない。
    pub ingest: bool,
    /// 旅行ブログ（`/blog-admin`・`/api/blog/*`）を有効にするか。`GEOFRAME_BLOG`。既定は無効。
    pub blog: bool,
    /// 同期サイクルの間隔（分）。`GEOFRAME_DRIVE_POLL_MIN`（現行と同じく分で、小数も可）。
    pub drive_poll_min: f64,
    /// `GEOFRAME_PHOTOS_DIR` の値そのまま（`DATA_DIR` からの相対、または絶対）。現行と同じく
    /// `DATA_DIR / 値`（絶対ならその値）を解決して使う。`DATA_DIR` の外に解決されたら、索引は断る
    /// （[`crate::local`]）。
    photos_dir_setting: PathBuf,
    /// 同期 1 サイクルで derivative を作る枚数。`GEOFRAME_FETCH_PER_CYCLE`（0 以上の整数）。
    pub fetch_per_cycle: u32,
    /// 同期 1 サイクルで品質（特徴量）を解析する枚数。`GEOFRAME_QUALITY_PER_CYCLE`（0 以上の整数。未指定は
    /// `fetch_per_cycle`）。0 でも 1 枚は解析する（現行の `max(1, limit)`）。
    pub quality_per_cycle: u32,
    /// Drive のサービスアカウント鍵 JSON のパス（絶対パス）。`GEOFRAME_DRIVE_KEY`。**持つのはパスだけ**で、
    /// 中身は [`crate::drive`] が段の中で読む（設定にも起動ログにも載せない。パスは `GET /api/admin/config` だけが出す）。
    pub drive_key: PathBuf,
    /// 取り込む Drive のフォルダ。`GEOFRAME_DRIVE_FOLDER`（名前、または 20 字以上の英数字・`_`・`-` なら ID）。
    /// 起動ログには載せない（フォルダ名は利用者の情報。`GET /api/admin/config` だけが設定どおりに出す）。
    pub drive_folder: String,
    /// 旅行ブログの旅程 JSON を読むディレクトリ（絶対パス）。`GEOFRAME_BLOG_PLANS_DIR`（Rust だけの項目）。
    /// `None` のときはバイナリに埋め込んだ `web/travel_blog/plans/*.json`（現行の `PLAN_DIR` と同じ内容）。
    /// 旅程の中身は利用者の情報なので、パスもログの要約には載せない（埋め込みかディレクトリかだけ。
    /// `GET /api/admin/config` だけが設定どおりのパスを出す）。
    pub blog_plans_dir: Option<PathBuf>,
    /// AI の設定（T4.1b）。旧 `GEOFRAME_AI_*` の周期・上限・フラグと、任意の `GEOFRAME_AI_CONFIG`（JSON）。
    /// **不正でも `Config` は作れる**（AI だけが無効で、種類のコードを持つ）。パス・JSON の中身はログ・`Debug` に載せない
    /// （`GET /api/admin/config`（管理画面の設定タブ）だけは、ユーザーの決定（2026-10-30）で設定どおりの値を出す）。
    pub ai: AiConfig,
    /// `GEOFRAME_AI_CONFIG` のパス（絶対パスにしたもの。未設定は `None`）。`GET /api/admin/config` が値として出す
    /// （設定 API だけの例外。ログ・`Debug` には載せない）。
    pub ai_config_path: Option<PathBuf>,
    /// `GEOFRAME_AI_CONFIG` のファイルの更新時刻（起動時に控える。読めなければ `None`）。
    pub ai_config_modified: Option<SystemTime>,
    /// 各項目の出どころ（キーは [`EnvItem::name`]、[`EnvItem::ALL`] の全項目）。`load()` の流れとは独立に、最後に作る。
    /// `Debug`・起動ログには載せない。
    pub sources: BTreeMap<&'static str, Source>,
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
        // 現行は `int(...)`。負の値は掃除が毎回すべてのタイルを消す意味になるので、Rust は断る
        let tile_cache_max_mb = match get("GEOFRAME_TILE_CACHE_MAX_MB") {
            Some(raw) => raw
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| {
                    ConfigError(
                        "GEOFRAME_TILE_CACHE_MAX_MB は 0 以上の整数（MB）で指定してください".into(),
                    )
                })?,
            None => DEFAULT_TILE_CACHE_MAX_MB,
        };
        // 現行は `int(...)`（負の値は「常に上限超過」）。Rust は断る。0 は上流へ行かない
        let tile_fetch_per_min = match get("GEOFRAME_TILE_FETCH_PER_MIN") {
            Some(raw) => raw
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| {
                    ConfigError(
                        "GEOFRAME_TILE_FETCH_PER_MIN は 0 以上の整数（枚/分）で指定してください"
                            .into(),
                    )
                })?,
            None => DEFAULT_TILE_FETCH_PER_MIN,
        };
        // ヘッダの値になるので、印字可能な ASCII（と空白）だけ。現行は不正な値でも起動し、取得のたびに失敗する
        let tile_ua = match get("GEOFRAME_TILE_UA") {
            Some(raw) => {
                let ua = raw.trim().to_string();
                if !ua.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                    return Err(ConfigError(
                        "GEOFRAME_TILE_UA は印字可能な ASCII で指定してください".into(),
                    ));
                }
                ua
            }
            None => DEFAULT_TILE_UA.to_string(),
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
        let blog = match get("GEOFRAME_BLOG") {
            Some(raw) => match raw.trim() {
                "1" => true,
                "0" => false,
                _ => {
                    return Err(ConfigError(
                        "GEOFRAME_BLOG は 0（無効）か 1（有効）で指定してください".into(),
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
        let quality_per_cycle = match get("GEOFRAME_QUALITY_PER_CYCLE") {
            Some(raw) => raw.trim().parse::<u32>().map_err(|_| {
                ConfigError("GEOFRAME_QUALITY_PER_CYCLE は 0 以上の整数で指定してください".into())
            })?,
            None => fetch_per_cycle,
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

        // 空文字は未設定と同じ（`get` が除く）。相対パスはカレントディレクトリ基準（`GEOFRAME_DRIVE_KEY` と同じ）
        let blog_plans_dir = get("GEOFRAME_BLOG_PLANS_DIR")
            .map(|v| std::path::absolute(PathBuf::from(v.trim())))
            .transpose()
            .map_err(|e| {
                ConfigError(format!(
                    "GEOFRAME_BLOG_PLANS_DIR を絶対パスにできません: {e}"
                ))
            })?;

        let ai = AiConfig::load(&get);
        let ai_visual_similarity =
            ai.visual_similarity()
                .unwrap_or(if embed_backend == EmbedBackend::Local {
                    0.91
                } else {
                    0.90
                });
        let rank_model_path = get("GEOFRAME_RANK_MODEL")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("models/rank_model.json"));
        let ai_config_path = get("GEOFRAME_AI_CONFIG").map(|raw| {
            let raw = raw.trim();
            std::path::absolute(raw).unwrap_or_else(|_| PathBuf::from(raw))
        });
        let ai_config_modified = ai_config_path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok());
        let sources = sources_of(&lookup, overrides);
        Ok(Self {
            listen,
            data_dir,
            allowed_hosts,
            log_level,
            tz: get("TZ"),
            infer_min,
            infer_offset_min,
            embed_backend,
            ai_visual_similarity,
            rank_model_path,
            embed_backend_invalid,
            tile_cache_max_mb,
            tile_ua,
            tile_fetch_per_min,
            ingest,
            blog,
            drive_poll_min,
            photos_dir_setting,
            fetch_per_cycle,
            quality_per_cycle,
            drive_key,
            drive_folder,
            blog_plans_dir,
            ai,
            ai_config_path,
            ai_config_modified,
            sources,
        })
    }

    /// ローカル写真の置き場（字句のパス。リンクは辿っていない）。現行の `DATA_DIR / GEOFRAME_PHOTOS_DIR`。
    pub fn photos_dir(&self) -> PathBuf {
        self.data_dir.join(&self.photos_dir_setting)
    }

    /// `GEOFRAME_PHOTOS_DIR` の値そのまま（`DATA_DIR` からの相対、または絶対。設定 API が値として出す）。
    pub fn photos_dir_setting(&self) -> &std::path::Path {
        &self.photos_dir_setting
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
            "listen={} data_dir={} allowed_hosts={} log={:?} tz={} infer_min={} infer_offset_min={} embed={} tile_cache_max_mb={} blog={} ingest={} drive_poll_min={} fetch_per_cycle={} quality_per_cycle={} blog_plans={} ai={}",
            self.listen,
            self.data_dir.display(),
            self.allowed_hosts.len(),
            self.log_level,
            self.tz.as_deref().unwrap_or("(未設定)"),
            self.infer_min,
            self.infer_offset_min,
            self.embed_backend.model(),
            self.tile_cache_max_mb,
            if self.blog { "on" } else { "off" },
            if self.ingest { 1 } else { 0 },
            self.drive_poll_min,
            self.fetch_per_cycle,
            self.quality_per_cycle,
            if self.blog_plans_dir.is_some() {
                "dir"
            } else {
                "embedded"
            },
            self.ai.summary(),
        )
    }
}

// 派生の Debug にしない。項目が増えたとき、秘密を含む値がうっかり `{:?}` でログへ出るのを防ぐ。
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Config({})", self.log_summary())
    }
}

/// 各項目の出どころ。`load()` の読み方とは独立に、[`EnvItem::ALL`] を回して作る: 起動引数があれば `Arg`
/// （`--listen` は `HOST`・`PORT` の両方、`--data-dir` は `DATA_DIR`）、なければ環境変数が空でない（`load()` の
/// `get` と同じく空白だけは未設定）なら `Env`、それ以外は `Default`。
fn sources_of(
    lookup: &impl Fn(&str) -> Option<String>,
    overrides: &Overrides,
) -> BTreeMap<&'static str, Source> {
    EnvItem::ALL
        .iter()
        .map(|item| {
            let name = item.name();
            let from_arg = match item {
                EnvItem::Host | EnvItem::Port => overrides.listen.is_some(),
                EnvItem::DataDir => overrides.data_dir.is_some(),
                _ => false,
            };
            let source = if from_arg {
                Source::Arg
            } else if lookup(name).is_some_and(|v| !v.trim().is_empty()) {
                Source::Env
            } else {
                Source::Default
            };
            (name, source)
        })
        .collect()
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
    fn tile_settings_default_to_the_python_values_and_the_user_agent_stays_out_of_the_log() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.tile_ua, "GeoFrame/0.1 (personal photo frame)");
        assert_eq!(c.tile_fetch_per_min, 240);
        let c = load(
            &[
                (
                    "GEOFRAME_TILE_UA",
                    "  GeoFrame/0.1 (contact-private@example.test) ",
                ),
                ("GEOFRAME_TILE_FETCH_PER_MIN", " 0 "),
                ("GEOFRAME_TILE_CACHE_MAX_MB", "0"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.tile_ua, "GeoFrame/0.1 (contact-private@example.test)");
        assert_eq!((c.tile_fetch_per_min, c.tile_cache_max_mb), (0, 0));
        // 連絡先が入りうる User-Agent は Debug にも起動ログの要約にも載せない
        for text in [format!("{c:?}"), c.log_summary()] {
            assert!(!text.contains("contact-private"), "{text}");
        }
        // 空文字は未設定と同じ
        let c = load(
            &[
                ("GEOFRAME_TILE_UA", ""),
                ("GEOFRAME_TILE_FETCH_PER_MIN", " "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            (c.tile_ua.as_str(), c.tile_fetch_per_min),
            (DEFAULT_TILE_UA, 240)
        );
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
            ("GEOFRAME_TILE_CACHE_MAX_MB", "-1"),
            ("GEOFRAME_TILE_FETCH_PER_MIN", "1.5"),
            ("GEOFRAME_TILE_FETCH_PER_MIN", "many"),
            ("GEOFRAME_TILE_FETCH_PER_MIN", "-1"),
            (
                "GEOFRAME_TILE_UA",
                "GeoFrame/1 (a@example.test)\r\nX-Evil: 1",
            ),
            ("GEOFRAME_TILE_UA", "GeoFrame/1 (連絡先)"),
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
    fn blog_is_off_by_default_and_read_from_the_environment() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert!(!c.blog);
        assert!(c.log_summary().contains("blog=off ingest=0 "));
        // 空文字は未設定と同じ
        assert!(
            !load(&[("GEOFRAME_BLOG", " ")], &Overrides::default())
                .unwrap()
                .blog
        );
        assert!(
            !load(&[("GEOFRAME_BLOG", "0")], &Overrides::default())
                .unwrap()
                .blog
        );
        let c = load(&[("GEOFRAME_BLOG", "1")], &Overrides::default()).unwrap();
        assert!(c.blog);
        assert!(c.log_summary().contains("blog=on ingest=0 "));
    }

    #[test]
    fn invalid_blog_is_an_error_without_echoing_the_value() {
        for value in ["2", "true", "yes", "on"] {
            let err = load(&[("GEOFRAME_BLOG", value)], &Overrides::default()).unwrap_err();
            assert!(err.0.contains("GEOFRAME_BLOG"), "{err}");
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
    fn quality_per_cycle_defaults_to_fetch_per_cycle() {
        let c = load(&[], &Overrides::default()).unwrap();
        assert_eq!(c.quality_per_cycle, 25);
        let c = load(&[("GEOFRAME_FETCH_PER_CYCLE", "7")], &Overrides::default()).unwrap();
        assert_eq!(c.quality_per_cycle, 7);
        let c = load(
            &[
                ("GEOFRAME_FETCH_PER_CYCLE", "7"),
                ("GEOFRAME_QUALITY_PER_CYCLE", " 3 "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.quality_per_cycle, 3);
        // 空文字は未設定と同じ
        let c = load(
            &[("GEOFRAME_QUALITY_PER_CYCLE", " ")],
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(c.quality_per_cycle, 25);
        for value in ["abc", "-1", "1.5"] {
            let err = load(
                &[("GEOFRAME_QUALITY_PER_CYCLE", value)],
                &Overrides::default(),
            )
            .unwrap_err();
            assert!(err.0.contains("GEOFRAME_QUALITY_PER_CYCLE"), "{err}");
            assert!(!err.0.contains(value), "値を出さない: {err}");
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

    #[test]
    fn every_item_is_listed_once_and_sources_cover_exactly_the_listed_items() {
        let names: Vec<&str> = EnvItem::ALL.iter().map(|i| i.name()).collect();
        let unique: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "名前の重複: {names:?}");
        assert_eq!(names.len(), 35);
        assert!(names.iter().all(|n| n.starts_with("GEOFRAME_")));
        let c = load(&[], &Overrides::default()).unwrap();
        let keys: BTreeSet<&str> = c.sources.keys().copied().collect();
        assert_eq!(keys, unique);
        // 出し方: ファイルの有無も出すのは 3 項目だけ
        let files: Vec<&str> = EnvItem::ALL
            .iter()
            .filter(|i| i.exposure() == Exposure::File)
            .map(|i| i.name())
            .collect();
        assert_eq!(
            files,
            [
                "GEOFRAME_DRIVE_KEY",
                "GEOFRAME_AI_CONFIG",
                "GEOFRAME_RANK_MODEL"
            ]
        );
    }

    #[test]
    fn sources_are_default_arg_or_env() {
        // 既定だけなら、全項目が既定
        let c = load(&[], &Overrides::default()).unwrap();
        assert!(c.sources.values().all(|s| *s == Source::Default));
        assert_eq!(c.ai_config_path, None);
        assert_eq!(c.ai_config_modified, None);
        // 環境変数 1 つ（前後の空白があっても）だけが Env
        let c = load(&[("GEOFRAME_INGEST", " 1 ")], &Overrides::default()).unwrap();
        for (name, source) in &c.sources {
            let expected = if *name == "GEOFRAME_INGEST" {
                Source::Env
            } else {
                Source::Default
            };
            assert_eq!(*source, expected, "{name}");
        }
        // 空文字・空白だけは未設定と同じ（`get` と同じ判定。ALLOWED_HOSTS も）
        let c = load(
            &[
                ("GEOFRAME_PORT", ""),
                ("GEOFRAME_ALLOWED_HOSTS", "  "),
                ("GEOFRAME_AI_POLL_S", " "),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert!(c.sources.values().all(|s| *s == Source::Default));
        // AI の旧設定・ALLOWED_HOSTS・Rust だけの項目も Env と記録する
        let c = load(
            &[
                ("GEOFRAME_ALLOWED_HOSTS", "a.example.test"),
                ("GEOFRAME_AI_POLL_S", "45"),
                ("GEOFRAME_BLOG", "1"),
                ("GEOFRAME_RANK_MODEL", "/srv/g/rank.json"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        for name in [
            "GEOFRAME_ALLOWED_HOSTS",
            "GEOFRAME_AI_POLL_S",
            "GEOFRAME_BLOG",
            "GEOFRAME_RANK_MODEL",
        ] {
            assert_eq!(c.sources[name], Source::Env, "{name}");
        }
        assert_eq!(c.sources["GEOFRAME_LOG"], Source::Default);
    }

    #[test]
    fn command_line_arguments_are_recorded_as_arg_even_when_the_environment_is_set() {
        let env = [
            ("GEOFRAME_DATA_DIR", "/srv/from-env"),
            ("GEOFRAME_HOST", "0.0.0.0"),
            ("GEOFRAME_PORT", "8081"),
        ];
        let both = Overrides {
            listen: Some("127.0.0.1:9000".parse().unwrap()),
            data_dir: Some(PathBuf::from("/srv/from-cli")),
        };
        let c = load(&env, &both).unwrap();
        for name in ["GEOFRAME_HOST", "GEOFRAME_PORT", "GEOFRAME_DATA_DIR"] {
            assert_eq!(c.sources[name], Source::Arg, "{name}");
        }
        // 片方だけ: もう片方は環境変数
        let listen_only = Overrides {
            listen: both.listen,
            data_dir: None,
        };
        let c = load(&env, &listen_only).unwrap();
        assert_eq!(c.sources["GEOFRAME_HOST"], Source::Arg);
        assert_eq!(c.sources["GEOFRAME_PORT"], Source::Arg);
        assert_eq!(c.sources["GEOFRAME_DATA_DIR"], Source::Env);
        let dir_only = Overrides {
            listen: None,
            data_dir: both.data_dir.clone(),
        };
        let c = load(&env, &dir_only).unwrap();
        assert_eq!(c.sources["GEOFRAME_DATA_DIR"], Source::Arg);
        assert_eq!(c.sources["GEOFRAME_HOST"], Source::Env);
        assert_eq!(c.sources["GEOFRAME_PORT"], Source::Env);
        // 引数があれば、不正な環境変数があっても読まない（出どころは Arg）
        let c = load(&[("GEOFRAME_PORT", "abc")], &both).unwrap();
        assert_eq!(c.sources["GEOFRAME_PORT"], Source::Arg);
    }

    #[test]
    fn sources_are_made_even_when_the_ai_config_is_invalid_and_stay_out_of_the_log() {
        let c = load(
            &[
                ("GEOFRAME_AI_CONFIG", "/nonexistent-dir/ai.json"),
                ("GEOFRAME_AI_POLL_S", "bad"),
            ],
            &Overrides::default(),
        )
        .unwrap();
        assert!(c.ai.invalid().is_some());
        assert_eq!(c.sources.len(), EnvItem::ALL.len());
        assert_eq!(c.sources["GEOFRAME_AI_CONFIG"], Source::Env);
        assert_eq!(c.sources["GEOFRAME_AI_POLL_S"], Source::Env);
        // パスは控えるが、読めないファイルの更新時刻は無い
        assert_eq!(
            c.ai_config_path,
            Some(PathBuf::from("/nonexistent-dir/ai.json"))
        );
        assert_eq!(c.ai_config_modified, None);
        // 出どころ・パスは Debug にも起動ログにも載せない
        for text in [format!("{c:?}"), c.log_summary()] {
            assert!(!text.contains("nonexistent-dir"), "{text}");
            assert!(!text.contains("sources"), "{text}");
        }
    }

    #[test]
    fn the_ai_config_modification_time_is_taken_at_load() {
        let dir = crate::testutil::TempDir::new("cfg-mtime");
        let path = dir.path().join("ai.json");
        std::fs::write(&path, br#"{"version":1}"#).unwrap();
        let c = load(
            &[("GEOFRAME_AI_CONFIG", path.to_str().unwrap())],
            &Overrides::default(),
        )
        .unwrap();
        let expected = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(c.ai_config_modified, Some(expected));
        assert_eq!(c.ai_config_path.as_deref(), Some(path.as_path()));
    }
}
