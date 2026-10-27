//! AI の設定（P4 / T4.1b。設計は p4-design.md §3、書式は `docs/rust-migration/ai-config.md`）。
//!
//! 用途別の送信先・モデル・クラウド許可は、任意の `GEOFRAME_AI_CONFIG` が指す小さな JSON（v1）に置く。旧
//! `GEOFRAME_AI_*`・`GEOFRAME_LOC_SUGGEST*` の数値・フラグは周期の上限などとして [`LegacyAi`] に対応付ける。
//! 未設定なら新しい外部 AI 呼び出しは無効。**誤った設定（JSON も環境変数の数値も）は AI だけを無効にし**、
//! 種類のコード（[`AiConfigError`]）を保持して、写真の配信・管理は止めない（[`AiConfig`]）。
//!
//! # 読み込み
//!
//! - ファイルは通常のファイルだけ・64KiB まで・UTF-8 の JSON オブジェクト。開いたあとの `fstat` で種類と大きさを
//!   見る（FIFO などで止まらない）。`serde` の型付き構造体（`deny_unknown_fields`・用途は名前つきの 5 つの
//!   フィールド）で読むので、**全ての階層で未知のキーと重複したキーを断る**（`Value`・`Map` は重複を黙って
//!   捨てるので使わない）。
//! - **起動時に DNS・ネットワーク・認証ファイルの読み取りをしない**。URL・パスは文字列として検査するだけ
//!   （接続先の private 固定は T4.2、認証は T4.6）。
//! - パス・JSON の中身・URL・連絡先・モデル名・`serde` のエラー文（値を含む）は `Debug`・ログに出さない。
//!   失敗は固定のコードだけ。
//!
//! # 送信の可否（[`AiConfig::gate`]）
//!
//! 用途ごとに、送る前に毎回評価する。`provider=cloud` でも `allow_cloud` が無ければ送らず、`daily_request_limit`
//! が未設定・0 でも、周期の上限が 0 でも送らない。`INGEST=0`（writer が無い）ではクラウドへ送らない
//! （予算を DB に予約できない）。LAN の検索語 Embedding は writer が無くても使える。
//!
//! # ハッシュ
//!
//! [`AiSettings::config_hash`]・[`AiSettings::policy_hash`] は監査・送信可否の検査用。**ジョブの冪等キー
//! （[`super::jobs::JobKey`]）の結果の同一性には混ぜない**（URL・許可・上限だけの変更で成功済みの結果を
//! 再送しないため）。停止の識別（[`AiSettings::provider_pause_key`]）はその理由に関係する項目だけから作る。
//! どれも SHA-256（再起動をまたいで同じ値）。

use std::fmt;
use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::budget::SendBudget;
use super::pause::{PauseKey, PauseScope};

pub const CONFIG_VERSION: u32 = 1;
/// 設定ファイルの最大の大きさ。
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;
/// `daily_request_limit` の上限（誤入力で事実上無制限にしない）。
pub const MAX_DAILY_REQUEST_LIMIT: u32 = 1_000_000;
/// `embedding_space.dimension` の上限（p4-design.md §6.2）。
pub const MAX_EMBEDDING_DIMENSION: u32 = 4096;

// 旧設定の既定（`app/geoframe/config.py`）。
pub const DEFAULT_AI_POLL_S: f64 = 60.0;
pub const MIN_AI_POLL_S: f64 = 30.0;
const MAX_AI_POLL_S: f64 = 86_400.0;
pub const DEFAULT_EMBED_PER_CYCLE: u32 = 12;
pub const DEFAULT_JUDGE_PER_CYCLE: u32 = 2;
pub const DEFAULT_SEARCH_PER_MIN: u32 = 30;
pub const DEFAULT_CLUE_PER_CYCLE: u32 = 60;
pub const DEFAULT_GEMINI_PER_CYCLE: u32 = 2;
/// 検索語のクラウド Embedding の周期窓（`GEOFRAME_AI_SEARCH_PER_MIN` は毎分）。
pub const QUERY_CYCLE_SECS: u32 = 60;

// ---------------------------------------------------------------- 型

/// AI の用途。`geocode` は LLM の routing を持たないので含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskKind {
    ImageEmbedding,
    QueryEmbedding,
    PhotoJudge,
    LocationClue,
    LocationSuggest,
}

impl TaskKind {
    pub const ALL: [Self; 5] = [
        Self::ImageEmbedding,
        Self::QueryEmbedding,
        Self::PhotoJudge,
        Self::LocationClue,
        Self::LocationSuggest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageEmbedding => "image_embedding",
            Self::QueryEmbedding => "query_embedding",
            Self::PhotoJudge => "photo_judge",
            Self::LocationClue => "location_clue",
            Self::LocationSuggest => "location_suggest",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == text)
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// LAN の入口（local-model-lab 等）。
    Local,
    /// 固定の HTTPS 宛先（Vertex 系の Gemini）。
    Cloud,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Normalization {
    /// L2 正規化したベクトルを保存する。
    #[serde(rename = "l2")]
    L2,
    /// 正規化しない。
    #[serde(rename = "none")]
    Raw,
}

impl Normalization {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L2 => "l2",
            Self::Raw => "none",
        }
    }
}

/// 設定が不正な理由の種類。**固定のコードだけ**（値・パス・`serde` のエラー文は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiConfigError {
    /// ファイルを開けない・読めない。
    Unreadable,
    /// 通常のファイルでない（ディレクトリ・FIFO など）。
    NotRegularFile,
    /// 64KiB を超える。
    TooLarge,
    /// JSON として読めない（構文・UTF-8・オブジェクトでない）。
    Syntax,
    DuplicateKey,
    UnknownKey,
    /// 必須のキーが無い。
    MissingKey,
    /// 値の型・範囲・文字が違う。
    BadValue,
    UnsupportedVersion,
    /// キーの組み合わせが矛盾する（ローカルにクラウドの項目、`location_clue` のクラウドなど）。
    Conflict,
    /// 旧 `GEOFRAME_AI_*` の数値・フラグが不正。
    LegacyEnv,
    /// `GEOFRAME_AI_CONFIG` のパスを絶対パスにできない。
    PathInvalid,
}

impl AiConfigError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::NotRegularFile => "not_regular_file",
            Self::TooLarge => "too_large",
            Self::Syntax => "syntax",
            Self::DuplicateKey => "duplicate_key",
            Self::UnknownKey => "unknown_key",
            Self::MissingKey => "missing_key",
            Self::BadValue => "bad_value",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Conflict => "conflict",
            Self::LegacyEnv => "legacy_env",
            Self::PathInvalid => "path_invalid",
        }
    }
}

impl fmt::Display for AiConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AiConfigError {}

/// クラウドの接続情報。秘密は持たない（認証ファイルの**パスだけ**。中身は読まない）。
#[derive(Clone, PartialEq, Eq)]
pub struct CloudTarget {
    /// 認証ファイルの絶対パス。
    pub credential_path: PathBuf,
    pub project: String,
    pub region: String,
}

/// 1 つの用途の設定（検査済み）。
#[derive(Clone, PartialEq, Eq)]
pub struct TaskSettings {
    pub provider: ProviderKind,
    pub model: String,
    /// LAN のサービス ID（`provider=local` の `location_clue` 以外）。
    pub service_id: Option<String>,
    /// クラウドへの送信を許すか（既定 false）。
    pub allow_cloud: bool,
    /// クラウドの 1 UTC 日の送信数。未設定・0 はクラウドへ送らない。
    pub daily_request_limit: Option<u32>,
    /// クラウドの画像送信に `hidden` の写真を含めるか（既定 false）。
    pub include_hidden: bool,
    pub cloud: Option<CloudTarget>,
}

impl fmt::Debug for TaskSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskSettings")
            .field("provider", &self.provider)
            .field("allow_cloud", &self.allow_cloud)
            .finish_non_exhaustive()
    }
}

/// 画像と検索語に共通のベクトル空間の識別（検査済みのメタデータ。形式を確かめただけで、モデルが実際にこの空間を
/// 出力することも用途間の同一性も保証しない。それらは後続の SpaceIdentity が Provider の応答と照合する）。
#[derive(Clone, PartialEq, Eq)]
pub struct EmbeddingSpace {
    pub space_id: String,
    pub model_id: String,
    pub revision: String,
    pub image_preprocess: String,
    pub text_preprocess: String,
    pub dimension: u32,
    pub normalization: Normalization,
}

impl fmt::Debug for EmbeddingSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingSpace")
            .field("dimension", &self.dimension)
            .field("normalization", &self.normalization)
            .finish_non_exhaustive()
    }
}

/// 検査済みの AI 設定（JSON v1）。
#[derive(Clone, PartialEq, Eq)]
pub struct AiSettings {
    local_base_url: Option<String>,
    embedding_space: Option<EmbeddingSpace>,
    embedding_contract: Option<String>,
    tasks: [Option<TaskSettings>; 5],
    config_hash: String,
}

impl fmt::Debug for AiSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiSettings")
            .field("tasks", &self.tasks.iter().flatten().count())
            .finish_non_exhaustive()
    }
}

impl AiSettings {
    pub fn task(&self, task: TaskKind) -> Option<&TaskSettings> {
        self.tasks[task.index()].as_ref()
    }

    /// 検査・正規化済みの LAN の入口（`scheme://host[:port]`。末尾の `/` なし）。
    pub fn local_base_url(&self) -> Option<&str> {
        self.local_base_url.as_deref()
    }

    /// Explicit pinned provider contract; metadata alone never enables the adapter.
    pub fn embedding_contract(&self) -> Option<&str> {
        self.embedding_contract.as_deref()
    }

    pub fn embedding_space(&self) -> Option<&EmbeddingSpace> {
        self.embedding_space.as_ref()
    }

    /// 設定全体のハッシュ（監査用）。**ジョブの冪等キーには使わない。**
    pub fn config_hash(&self) -> &str {
        &self.config_hash
    }

    /// 用途の送信ポリシーのハッシュ（監査・送信可否の検査用。provider・許可・上限・`include_hidden`）。
    /// **ジョブの冪等キーには使わない。**
    pub fn policy_hash(&self, task: TaskKind) -> Option<String> {
        let t = self.task(task)?;
        let daily = t
            .daily_request_limit
            .map_or(String::new(), |n| n.to_string());
        Some(fingerprint(&[
            "policy",
            task.as_str(),
            t.provider.as_str(),
            if t.allow_cloud { "1" } else { "0" },
            &daily,
            if t.include_hidden { "1" } else { "0" },
        ]))
    }

    /// 用途の接続先・認証の識別を作る材料（Provider の停止の単位）。モデル・許可・上限は入れない。
    fn provider_identity(&self, t: &TaskSettings) -> Vec<String> {
        match (&t.cloud, t.provider) {
            (Some(cloud), ProviderKind::Cloud) => vec![
                "cloud".into(),
                cloud.credential_path.to_string_lossy().into_owned(),
                cloud.project.clone(),
                cloud.region.clone(),
            ],
            _ => vec![
                "local".into(),
                self.local_base_url.clone().unwrap_or_default(),
            ],
        }
    }

    /// Provider（接続先・認証が同じ全用途）の停止の識別。関係する設定（LAN の入口、クラウドの認証ファイル・
    /// project・region）が変わると変わり、タイムアウト・別の用途の許可や上限・モデルでは変わらない。
    pub fn provider_pause_key(&self, task: TaskKind) -> Option<PauseKey> {
        let t = self.task(task)?;
        let mut parts = vec!["provider".to_string()];
        parts.extend(self.provider_identity(t));
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        Some(PauseKey::new(PauseScope::Provider, fingerprint(&refs)))
    }

    /// 用途（とそのモデル・サービス・接続先）の停止の識別。モデルを変えると変わる。
    pub fn task_pause_key(&self, task: TaskKind) -> Option<PauseKey> {
        let t = self.task(task)?;
        let mut parts = vec![
            "task".to_string(),
            task.as_str().to_string(),
            t.model.clone(),
            t.service_id.clone().unwrap_or_default(),
        ];
        if matches!(task, TaskKind::ImageEmbedding | TaskKind::QueryEmbedding) {
            parts.push(self.embedding_contract.clone().unwrap_or_default());
            if let Some(space) = &self.embedding_space {
                parts.extend([
                    space.space_id.clone(),
                    space.model_id.clone(),
                    space.revision.clone(),
                    space.image_preprocess.clone(),
                    space.text_preprocess.clone(),
                    space.dimension.to_string(),
                    space.normalization.as_str().to_string(),
                ]);
            }
        }
        parts.extend(self.provider_identity(t));
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        Some(PauseKey::new(PauseScope::Task, fingerprint(&refs)))
    }

    /// Provider（接続先・認証が同じ全用途）の**一時待機**の識別（[`super::waits`]）。永続停止の識別
    /// （[`AiSettings::provider_pause_key`]）とは別の名前空間で、モデル・許可・上限では変わらない。
    pub fn provider_fingerprint(&self, task: TaskKind) -> Option<String> {
        let t = self.task(task)?;
        let mut parts = vec!["wait-provider".to_string()];
        parts.extend(self.provider_identity(t));
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        Some(fingerprint(&refs))
    }

    /// 用途が使うサービスの一時待機の識別。LAN は同じサービス ID（`location_clue` は固定の `decision`）を使う用途が
    /// 同じ識別になり、同じ入口の別サービスは別。クラウドはモデル単位。
    pub fn service_key(&self, task: TaskKind) -> Option<String> {
        let t = self.task(task)?;
        let name = match t.provider {
            ProviderKind::Local => t
                .service_id
                .clone()
                .unwrap_or_else(|| super::decision::SERVICE_ID.to_string()),
            ProviderKind::Cloud => format!("cloud:{}", t.model),
        };
        Some(fingerprint(&["wait-service", &name]))
    }
}

/// 長さ付きで連結した SHA-256 の 16 進。ハッシュの区切りの曖昧さ（`ab`+`c` と `a`+`bc`）を作らない。
/// `DefaultHasher` は起動ごとに値が変わるので、永続する識別には使わない。
pub(crate) fn fingerprint(parts: &[&str]) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        ctx.update(&(part.len() as u64).to_le_bytes());
        ctx.update(part.as_bytes());
    }
    ctx.finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- JSON の読み込み

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    local_base_url: Option<String>,
    embedding_space: Option<RawSpace>,
    embedding_contract: Option<String>,
    #[serde(default)]
    tasks: RawTasks,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpace {
    space_id: String,
    model_id: String,
    revision: String,
    image_preprocess: String,
    text_preprocess: String,
    dimension: u32,
    normalization: Normalization,
}

/// 用途は名前つきのフィールド（`Map` だと重複したキーが黙って 1 つになる）。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTasks {
    image_embedding: Option<RawTask>,
    query_embedding: Option<RawTask>,
    photo_judge: Option<RawTask>,
    location_clue: Option<RawTask>,
    location_suggest: Option<RawTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTask {
    provider: ProviderKind,
    model: String,
    service_id: Option<String>,
    allow_cloud: Option<bool>,
    daily_request_limit: Option<u32>,
    include_hidden: Option<bool>,
    credential_path: Option<String>,
    project: Option<String>,
    region: Option<String>,
}

/// `serde_json` のエラーを固定のコードにする。メッセージ（値を含みうる）は捨てる。重複・未知・欠落は
/// `serde` の定型の書き出しで見分ける（試験で固定してある）。
fn classify(err: &serde_json::Error) -> AiConfigError {
    use serde_json::error::Category;
    match err.classify() {
        Category::Io | Category::Syntax | Category::Eof => AiConfigError::Syntax,
        Category::Data => {
            let message = err.to_string();
            if message.starts_with("duplicate field") {
                AiConfigError::DuplicateKey
            } else if message.starts_with("unknown field") {
                AiConfigError::UnknownKey
            } else if message.starts_with("missing field") {
                AiConfigError::MissingKey
            } else {
                AiConfigError::BadValue
            }
        }
    }
}

/// 識別子（モデル・サービス ID など）: 英数字で始まり、英数字と `.` `_` `-`（`extra` も）だけ。
fn is_token(text: &str, max: usize, extra: &[u8]) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') || extra.contains(b)
        })
}

/// project・region: 小文字・数字・`-` だけ（クラウドの宛先の組み立てに入る）。
fn is_cloud_name(text: &str, max: usize) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// LAN の入口の URL を検査して正規化する（`scheme://host[:port]`）。`http`・`https` だけ、userinfo・query・
/// fragment・パス・空白・制御文字・非 ASCII・`%`・`\` は断る。ホストは IP リテラルか DNS 名の書式だけ見る
/// （名前の解決も、private かどうかの判定もここではしない。接続時に T4.2 が固定する）。
fn normalize_base_url(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > 255 || !raw.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return None;
    }
    if raw
        .bytes()
        .any(|b| matches!(b, b'@' | b'?' | b'#' | b'\\' | b'%'))
    {
        return None;
    }
    let (scheme, rest) = match raw.strip_prefix("http://") {
        Some(rest) => ("http", rest),
        None => ("https", raw.strip_prefix("https://")?),
    };
    let authority = match rest.split_once('/') {
        Some((authority, "")) => authority,
        Some(_) => return None,
        None => rest,
    };
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (addr, after) = inner.split_once(']')?;
        let port = match after {
            "" => None,
            other => Some(other.strip_prefix(':')?),
        };
        (format!("[{}]", addr.parse::<Ipv6Addr>().ok()?), port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if !is_host_name(host) {
            return None;
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None => String::new(),
        Some(digits) => {
            let valid = !digits.is_empty()
                && digits.len() <= 5
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits
                    .parse::<u32>()
                    .is_ok_and(|n| (1..=65535).contains(&n));
            if !valid {
                return None;
            }
            format!(":{}", digits.parse::<u32>().ok()?)
        }
    };
    Some(format!("{scheme}://{host}{port}"))
}

/// IPv4 リテラル、または DNS 名の書式（ラベルは英数字と `-`、先頭・末尾は英数字、最後のラベルは数字だけでない）。
fn is_host_name(host: &str) -> bool {
    if host.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let well_formed = labels.iter().all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 63
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    });
    well_formed
        && labels
            .last()
            .is_some_and(|last| !last.bytes().all(|b| b.is_ascii_digit()))
}

/// 用途の設定を検査する（組み合わせの矛盾も）。
fn validate_task(
    kind: TaskKind,
    raw: RawTask,
    local_base_url: Option<&str>,
) -> Result<TaskSettings, AiConfigError> {
    if !is_token(&raw.model, 64, &[]) {
        return Err(AiConfigError::BadValue);
    }
    match raw.provider {
        ProviderKind::Local => {
            // クラウドだけの項目は置かせない（効かない値を黙って受けない）
            if raw.allow_cloud.is_some()
                || raw.daily_request_limit.is_some()
                || raw.include_hidden.is_some()
                || raw.credential_path.is_some()
                || raw.project.is_some()
                || raw.region.is_some()
            {
                return Err(AiConfigError::Conflict);
            }
            if local_base_url.is_none() {
                return Err(AiConfigError::MissingKey);
            }
            // Decision（location_clue）は固定のパスを使うので service_id は要らない・置かせない
            let service_id = match (kind, raw.service_id) {
                (TaskKind::LocationClue, None) => None,
                (TaskKind::LocationClue, Some(_)) => return Err(AiConfigError::Conflict),
                (_, None) => return Err(AiConfigError::MissingKey),
                (_, Some(id)) => {
                    if !is_token(&id, 64, &[]) {
                        return Err(AiConfigError::BadValue);
                    }
                    Some(id)
                }
            };
            Ok(TaskSettings {
                provider: ProviderKind::Local,
                model: raw.model,
                service_id,
                allow_cloud: false,
                daily_request_limit: None,
                include_hidden: false,
                cloud: None,
            })
        }
        ProviderKind::Cloud => {
            // 撮影地の手がかりは LAN の Decision に限る（家の中の写真を外へ出さない関門）
            if kind == TaskKind::LocationClue || raw.service_id.is_some() {
                return Err(AiConfigError::Conflict);
            }
            let credential_path = raw.credential_path.ok_or(AiConfigError::MissingKey)?;
            let project = raw.project.ok_or(AiConfigError::MissingKey)?;
            let region = raw.region.ok_or(AiConfigError::MissingKey)?;
            let path = PathBuf::from(&credential_path);
            if !path.is_absolute()
                || credential_path.len() > 4096
                || credential_path.bytes().any(|b| b < 0x20 || b == 0x7f)
                || !is_cloud_name(&project, 63)
                || !is_cloud_name(&region, 32)
            {
                return Err(AiConfigError::BadValue);
            }
            if raw
                .daily_request_limit
                .is_some_and(|n| n > MAX_DAILY_REQUEST_LIMIT)
            {
                return Err(AiConfigError::BadValue);
            }
            Ok(TaskSettings {
                provider: ProviderKind::Cloud,
                model: raw.model,
                service_id: None,
                allow_cloud: raw.allow_cloud.unwrap_or(false),
                daily_request_limit: raw.daily_request_limit,
                include_hidden: raw.include_hidden.unwrap_or(false),
                cloud: Some(CloudTarget {
                    credential_path: path,
                    project,
                    region,
                }),
            })
        }
    }
}

fn validate_space(raw: RawSpace) -> Result<EmbeddingSpace, AiConfigError> {
    let tokens = [
        &raw.space_id,
        &raw.model_id,
        &raw.revision,
        &raw.image_preprocess,
        &raw.text_preprocess,
    ];
    if !tokens.iter().all(|t| is_token(t, 64, b":")) {
        return Err(AiConfigError::BadValue);
    }
    if !(1..=MAX_EMBEDDING_DIMENSION).contains(&raw.dimension) {
        return Err(AiConfigError::BadValue);
    }
    Ok(EmbeddingSpace {
        space_id: raw.space_id,
        model_id: raw.model_id,
        revision: raw.revision,
        image_preprocess: raw.image_preprocess,
        text_preprocess: raw.text_preprocess,
        dimension: raw.dimension,
        normalization: raw.normalization,
    })
}

impl AiSettings {
    /// JSON の本文（v1）を読んで検査する。値は検査・正規化済みで、エラーは固定のコードだけ。
    pub fn parse(bytes: &[u8]) -> Result<Self, AiConfigError> {
        let raw: RawConfig = serde_json::from_slice(bytes).map_err(|e| classify(&e))?;
        if raw.version != CONFIG_VERSION {
            return Err(AiConfigError::UnsupportedVersion);
        }
        let local_base_url = match raw.local_base_url {
            Some(url) => Some(normalize_base_url(&url).ok_or(AiConfigError::BadValue)?),
            None => None,
        };
        if raw
            .embedding_contract
            .as_deref()
            .is_some_and(|c| c != super::embedding::CONTRACT)
        {
            return Err(AiConfigError::BadValue);
        }
        let embedding_space = raw.embedding_space.map(validate_space).transpose()?;
        let raw_tasks = [
            (TaskKind::ImageEmbedding, raw.tasks.image_embedding),
            (TaskKind::QueryEmbedding, raw.tasks.query_embedding),
            (TaskKind::PhotoJudge, raw.tasks.photo_judge),
            (TaskKind::LocationClue, raw.tasks.location_clue),
            (TaskKind::LocationSuggest, raw.tasks.location_suggest),
        ];
        let mut tasks: [Option<TaskSettings>; 5] = [None, None, None, None, None];
        for (kind, raw_task) in raw_tasks {
            if let Some(raw_task) = raw_task {
                tasks[kind.index()] =
                    Some(validate_task(kind, raw_task, local_base_url.as_deref())?);
            }
        }
        let mut settings = Self {
            local_base_url,
            embedding_space,
            embedding_contract: raw.embedding_contract,
            tasks,
            config_hash: String::new(),
        };
        settings.config_hash = settings.compute_config_hash();
        Ok(settings)
    }

    /// ファイルから読む。通常のファイルだけ・64KiB まで。パスは結果にも `Debug` にも出さない。
    pub fn load_file(path: &Path) -> Result<Self, AiConfigError> {
        // FIFO などを開いて止まらないよう O_NONBLOCK。種類と大きさは開いたあとのハンドルで見る（差し替えに強い）
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| AiConfigError::Unreadable)?;
        let meta = file.metadata().map_err(|_| AiConfigError::Unreadable)?;
        if !meta.is_file() {
            return Err(AiConfigError::NotRegularFile);
        }
        if meta.len() > MAX_CONFIG_BYTES {
            return Err(AiConfigError::TooLarge);
        }
        let mut body = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|_| AiConfigError::Unreadable)?;
        if body.len() as u64 > MAX_CONFIG_BYTES {
            return Err(AiConfigError::TooLarge);
        }
        Self::parse(&body)
    }

    fn compute_config_hash(&self) -> String {
        let mut parts: Vec<String> = vec![
            format!("v{CONFIG_VERSION}"),
            self.local_base_url.clone().unwrap_or_default(),
            self.embedding_contract.clone().unwrap_or_default(),
        ];
        match &self.embedding_space {
            Some(s) => parts.extend([
                s.space_id.clone(),
                s.model_id.clone(),
                s.revision.clone(),
                s.image_preprocess.clone(),
                s.text_preprocess.clone(),
                s.dimension.to_string(),
                s.normalization.as_str().to_string(),
            ]),
            None => parts.push("no-space".into()),
        }
        for kind in TaskKind::ALL {
            parts.push(kind.as_str().to_string());
            match self.task(kind) {
                None => parts.push("-".into()),
                Some(t) => {
                    parts.extend([
                        t.provider.as_str().to_string(),
                        t.model.clone(),
                        t.service_id.clone().unwrap_or_default(),
                        t.allow_cloud.to_string(),
                        t.daily_request_limit
                            .map_or(String::new(), |n| n.to_string()),
                        t.include_hidden.to_string(),
                    ]);
                    match &t.cloud {
                        Some(c) => parts.extend([
                            c.credential_path.to_string_lossy().into_owned(),
                            c.project.clone(),
                            c.region.clone(),
                        ]),
                        None => parts.push("-".into()),
                    }
                }
            }
        }
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        fingerprint(&refs)
    }
}

// ---------------------------------------------------------------- 旧設定・状態

/// 旧 `GEOFRAME_AI_*`・`GEOFRAME_LOC_SUGGEST*` の数値・フラグ（既定は `config.py` と同じ）。後続のワーカーが
/// 周期と 1 周期の上限として使う。**Rust は旧設定だけでは推論を始めない**（新しい呼び出しは JSON の routing だけ）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegacyAi {
    /// `GEOFRAME_AI_ENABLED`（`1` のときだけ有効。既定は `1`）。0 のとき AI 検索（E6）は現行どおり 503。
    pub enabled: bool,
    /// `GEOFRAME_AI_POLL_S`（秒。30 未満は 30 に切り上げる。既定 60）。
    pub poll_secs: f64,
    /// `GEOFRAME_AI_EMBED_PER_CYCLE`（画像 Embedding。既定 12）。
    pub embed_per_cycle: u32,
    /// `GEOFRAME_AI_JUDGE_PER_CYCLE`（写真比較。既定 2）。
    pub judge_per_cycle: u32,
    /// `GEOFRAME_AI_SEARCH_PER_MIN`（検索語の Embedding・AI 検索。毎分。既定 30）。
    pub search_per_min: u32,
    /// `GEOFRAME_LOC_SUGGEST`（`1` のときだけ。既定 `1`。`enabled` も要る）。
    pub loc_suggest: bool,
    /// `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`（Decision の判定。既定 60）。
    pub clue_per_cycle: u32,
    /// `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`（位置候補の Gemini。既定 2）。
    pub gemini_per_cycle: u32,
}

impl Default for LegacyAi {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_secs: DEFAULT_AI_POLL_S,
            embed_per_cycle: DEFAULT_EMBED_PER_CYCLE,
            judge_per_cycle: DEFAULT_JUDGE_PER_CYCLE,
            search_per_min: DEFAULT_SEARCH_PER_MIN,
            loc_suggest: true,
            clue_per_cycle: DEFAULT_CLUE_PER_CYCLE,
            gemini_per_cycle: DEFAULT_GEMINI_PER_CYCLE,
        }
    }
}

impl LegacyAi {
    /// 環境変数から読む。数値が不正（数でない・負・有限でない）なら `None`（値は残さない）。
    /// 周期は負だけを不正とし、0 や 30 未満の正値は 30 に切り上げる。
    /// フラグは現行と同じく `1` だけが有効。
    fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        fn int(get: &dyn Fn(&str) -> Option<String>, key: &str, default: u32) -> Option<u32> {
            match get(key) {
                Some(raw) => raw.trim().parse::<u32>().ok(),
                None => Some(default),
            }
        }
        let flag = |key: &str| get(key).is_none_or(|raw| raw.trim() == "1");
        let poll_secs = match get("GEOFRAME_AI_POLL_S") {
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .ok()
                // 負は不正（clamp の前に検査する。0 や小さな正値は 30 に切り上げる）
                .filter(|v| v.is_finite() && *v >= 0.0)?
                .clamp(MIN_AI_POLL_S, MAX_AI_POLL_S),
            None => DEFAULT_AI_POLL_S,
        };
        Some(Self {
            enabled: flag("GEOFRAME_AI_ENABLED"),
            poll_secs,
            embed_per_cycle: int(get, "GEOFRAME_AI_EMBED_PER_CYCLE", DEFAULT_EMBED_PER_CYCLE)?,
            judge_per_cycle: int(get, "GEOFRAME_AI_JUDGE_PER_CYCLE", DEFAULT_JUDGE_PER_CYCLE)?,
            search_per_min: int(get, "GEOFRAME_AI_SEARCH_PER_MIN", DEFAULT_SEARCH_PER_MIN)?,
            loc_suggest: flag("GEOFRAME_LOC_SUGGEST"),
            clue_per_cycle: int(
                get,
                "GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE",
                DEFAULT_CLUE_PER_CYCLE,
            )?,
            gemini_per_cycle: int(
                get,
                "GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE",
                DEFAULT_GEMINI_PER_CYCLE,
            )?,
        })
    }

    /// AI ワーカーの周期。
    pub fn poll(&self) -> Duration {
        Duration::from_secs_f64(self.poll_secs)
    }

    /// 用途の 1 周期の上限と、その窓の長さ（秒）。検索語は毎分、そのほかはワーカーの周期。
    pub fn cycle(&self, task: TaskKind) -> (u32, u32) {
        let poll = self.poll_secs.ceil() as u32;
        match task {
            TaskKind::ImageEmbedding => (self.embed_per_cycle, poll),
            TaskKind::QueryEmbedding => (self.search_per_min, QUERY_CYCLE_SECS),
            TaskKind::PhotoJudge => (self.judge_per_cycle, poll),
            TaskKind::LocationClue => (self.clue_per_cycle, poll),
            TaskKind::LocationSuggest => (self.gemini_per_cycle, poll),
        }
    }
}

#[derive(Clone, PartialEq)]
enum AiState {
    /// `GEOFRAME_AI_CONFIG` が無い。
    Unset,
    Invalid(AiConfigError),
    Ready(Box<AiSettings>),
}

/// 設定が持つ AI の状態。`Config::ai`。誤った設定でも `Config` は作れる（AI だけが無効）。
#[derive(Clone, PartialEq)]
pub struct AiConfig {
    legacy: LegacyAi,
    state: AiState,
}

impl fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AiConfig({})", self.summary())
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            legacy: LegacyAi::default(),
            state: AiState::Unset,
        }
    }
}

impl AiConfig {
    /// 旧設定の環境変数と `GEOFRAME_AI_CONFIG` から作る。**失敗しない**: 不正は [`AiConfig::invalid`] の
    /// コードとして保持する。旧設定が不正ならファイルは読まない。ファイルの読み取りはここだけで、
    /// 通信・認証ファイルの読み取りはしない。
    pub fn load(get: &dyn Fn(&str) -> Option<String>) -> Self {
        let Some(legacy) = LegacyAi::from_env(get) else {
            return Self {
                legacy: LegacyAi::default(),
                state: AiState::Invalid(AiConfigError::LegacyEnv),
            };
        };
        let state = match get("GEOFRAME_AI_CONFIG") {
            None => AiState::Unset,
            Some(raw) => match std::path::absolute(raw.trim()) {
                Err(_) => AiState::Invalid(AiConfigError::PathInvalid),
                Ok(path) => match AiSettings::load_file(&path) {
                    Ok(settings) => AiState::Ready(Box::new(settings)),
                    Err(code) => AiState::Invalid(code),
                },
            },
        };
        Self { legacy, state }
    }

    pub fn legacy(&self) -> &LegacyAi {
        &self.legacy
    }

    /// 検査済みの設定（未設定・不正なら `None`）。
    pub fn settings(&self) -> Option<&AiSettings> {
        match &self.state {
            AiState::Ready(settings) => Some(settings),
            _ => None,
        }
    }

    /// 設定が不正なときの種類のコード（`ai_config_invalid` の理由）。
    pub fn invalid(&self) -> Option<AiConfigError> {
        match self.state {
            AiState::Invalid(code) => Some(code),
            _ => None,
        }
    }

    /// 起動ログ用の短い要約（コードと件数だけ）。
    pub fn summary(&self) -> String {
        match &self.state {
            AiState::Unset => "unset".into(),
            AiState::Invalid(code) => format!("invalid:{code}"),
            AiState::Ready(settings) => format!(
                "ready:tasks={}{}",
                settings.tasks.iter().flatten().count(),
                if self.legacy.enabled { "" } else { ",off" }
            ),
        }
    }

    /// 用途を今使ってよいか。送る前に毎回評価する（設定の変更・上限・許可は再起動で反映）。
    /// `has_writer` は取り込み（writer）が有効か。使えないときは理由（[`Disabled`]）。
    pub fn gate(&self, task: TaskKind, has_writer: bool) -> Result<Gate<'_>, Disabled> {
        let settings = match &self.state {
            AiState::Unset => return Err(Disabled::Unset),
            AiState::Invalid(_) => return Err(Disabled::ConfigInvalid),
            AiState::Ready(settings) => settings,
        };
        if !self.legacy.enabled {
            return Err(Disabled::AiDisabled);
        }
        if matches!(task, TaskKind::LocationClue | TaskKind::LocationSuggest)
            && !self.legacy.loc_suggest
        {
            return Err(Disabled::LocSuggestOff);
        }
        let task_settings = settings.task(task).ok_or(Disabled::TaskNotConfigured)?;
        let (cycle_limit, cycle_secs) = self.legacy.cycle(task);
        if cycle_limit == 0 {
            return Err(Disabled::CycleLimitZero);
        }
        // ここで確かめるのは `embedding_space` のメタデータが**書かれていること**だけ。この用途のモデルが
        // その空間を実際に出力することの保証ではない（Provider の応答との照合・用途間の同一性の検査は、
        // 後続の SpaceIdentity の責務。この gate を通っても、空間が一致するとは限らない）
        if matches!(task, TaskKind::ImageEmbedding | TaskKind::QueryEmbedding)
            && settings.embedding_space.is_none()
        {
            return Err(Disabled::NoEmbeddingSpace);
        }
        let budget = match task_settings.provider {
            ProviderKind::Local => None,
            ProviderKind::Cloud => {
                if !task_settings.allow_cloud {
                    return Err(Disabled::CloudNotAllowed);
                }
                let daily = task_settings.daily_request_limit.unwrap_or(0);
                if daily == 0 {
                    return Err(Disabled::NoDailyLimit);
                }
                Some(SendBudget {
                    daily,
                    cycle: cycle_limit,
                    cycle_secs,
                })
            }
        };
        // 予算を予約する writer が要る（クラウド）。ジョブの用途は ai_jobs にも書くので常に要る。
        // LAN の検索語 Embedding だけは writer なしで使える
        let needs_writer = budget.is_some() || task != TaskKind::QueryEmbedding;
        if needs_writer && !has_writer {
            return Err(Disabled::NoWriter);
        }
        Ok(Gate {
            task,
            settings: task_settings,
            local_base_url: settings.local_base_url(),
            embedding_space: settings.embedding_space(),
            cycle_limit,
            cycle_secs,
            budget,
        })
    }

    /// 今使ってよい用途（AI ワーカーを起動するかの判断に使う。空なら起動しない）。
    pub fn enabled_tasks(&self, has_writer: bool) -> Vec<TaskKind> {
        TaskKind::ALL
            .into_iter()
            .filter(|t| self.gate(*t, has_writer).is_ok())
            .collect()
    }
}

/// 用途を使えない理由（STATUS・ログに出す固定のコード）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disabled {
    /// `GEOFRAME_AI_CONFIG` が無い（新しい外部 AI 呼び出しは無効）。
    Unset,
    ConfigInvalid,
    /// `GEOFRAME_AI_ENABLED` が `1` でない。
    AiDisabled,
    /// `GEOFRAME_LOC_SUGGEST` が `1` でない。
    LocSuggestOff,
    TaskNotConfigured,
    /// `provider=cloud` だが `allow_cloud` が無い。
    CloudNotAllowed,
    /// クラウドの `daily_request_limit` が未設定または 0。
    NoDailyLimit,
    /// 1 周期の上限が 0。
    CycleLimitZero,
    /// 画像・検索語の Embedding に `embedding_space` が無い。
    NoEmbeddingSpace,
    /// 書き込み（writer）が無い（`INGEST=0`）。
    NoWriter,
}

impl Disabled {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::ConfigInvalid => "config_invalid",
            Self::AiDisabled => "ai_disabled",
            Self::LocSuggestOff => "loc_suggest_off",
            Self::TaskNotConfigured => "task_not_configured",
            Self::CloudNotAllowed => "cloud_not_allowed",
            Self::NoDailyLimit => "no_daily_limit",
            Self::CycleLimitZero => "cycle_limit_zero",
            Self::NoEmbeddingSpace => "no_embedding_space",
            Self::NoWriter => "no_writer",
        }
    }
}

/// 使ってよい用途と、その接続情報・上限（[`AiConfig::gate`]）。`Debug` は URL・モデル名を出さない。
#[derive(Clone, Copy)]
pub struct Gate<'a> {
    pub task: TaskKind,
    pub settings: &'a TaskSettings,
    pub local_base_url: Option<&'a str>,
    pub embedding_space: Option<&'a EmbeddingSpace>,
    /// 1 周期の上限（LAN のワーカーの処理数にも、クラウドの周期窓にも使う）。
    pub cycle_limit: u32,
    /// 周期窓（直近この秒数のローリング窓）の長さ。
    pub cycle_secs: u32,
    /// クラウドの用途だけ。送る直前に [`super::budget::reserve`] で枠を予約する。
    pub budget: Option<SendBudget>,
}

impl fmt::Debug for Gate<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gate")
            .field("task", &self.task)
            .field("cycle_limit", &self.cycle_limit)
            .field("cycle_secs", &self.cycle_secs)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}
