//! Google Drive 連携（P2 / T2.4）: 認証・フォルダの解決・`files.list`・原本のダウンロード。
//! 現行 `app/geoframe/drive.py` の `drive_service`・`drive_resolve_folder`・`index_drive`・`fetch_derivatives`
//! にあたる。詳細は `docs/rust-migration/server.md` §14。
//!
//! # 構成
//!
//! - この階層: 通信の部品（[`DriveShared`] が段をまたいで持つ状態、[`Session`] が 1 回の段の通信）。
//! - [`IndexDrive`]（`index.rs`）: 同期サイクルの段 `index_drive`。一覧を読んで `photos` を追加・更新・削除する。
//! - [`FetchDerivatives`]（`fetch.rs`）: 同期サイクルの段 `fetch_derivatives`。原本を落として derivative を作る。
//!
//! # 認証
//!
//! サービスアカウントの鍵 JSON（`GEOFRAME_DRIVE_KEY`）から `client_email`・`private_key` だけを読み、JWT
//! （RS256。[`crate::jwt`]）を作ってアクセストークンと交換する（スコープ `drive.readonly`）。
//! **トークンの交換先は定数 [`TOKEN_URL`] で、鍵 JSON の `token_uri` は読まない**（鍵ファイルの書き換えで
//! 署名付きの assertion を任意の URL へ送らせないため。現行の google-auth はファイルの値を使う）。
//! トークンは期限の 60 秒前に更新する。鍵・トークン・`Authorization` ヘッダ・URL・`ureq` のエラー文字列を
//! ログにもエラー文字列にも出さない（[`DriveError`] は固定の語と状態コードだけを持つ）。
//!
//! 通信先（トークンの URL と API の基底 URL）は [`Endpoints`] が持ち、**本番の経路は定数だけ**。
//! 試験の差し替えは `#[cfg(test)]` のコンストラクタだけで、環境変数・設定からは与えられない。
//!
//! # 再試行
//!
//! 429・5xx・403 のうち Drive が返す流量制限（`rateLimitExceeded` など）は、`Retry-After`（あれば）か
//! 指数バックオフで [`Retry::max_retries`] 回まで再試行する。待ちの最中も停止の合図を見る。401 は
//! トークンを取り直して 1 回だけ再試行する。それ以外の状態コードは [`DriveError::Status`] で返す。
//! 再試行を使い切った流量制限の 403 は、権限の不足と区別できるよう [`DriveError::RateLimited`] にする。

mod fetch;
mod index;
#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::jwt::{GoogleAssertion, JwtError, Rs256Signer};
use crate::net::{self, Failure};
use crate::stop::StopFlag;

pub use fetch::FetchDerivatives;
pub use index::IndexDrive;

/// 読み取り専用のスコープ（現行と同じ）。
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
/// アクセストークンの交換先。
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Drive API v3 の基底 URL。
pub const API_BASE: &str = "https://www.googleapis.com/drive/v3";
/// 取り込む MIME タイプ（現行の `DRIVE_MIMES`）。
pub const DRIVE_MIMES: [&str; 5] = [
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/tiff",
];
/// `files.list` の `fields`（現行と同じ文字列）。
pub const LIST_FIELDS: &str =
    "nextPageToken,files(id,name,md5Checksum,mimeType,imageMediaMetadata(time,location))";
/// `files.list` のページの大きさ（現行と同じ。Drive の上限）。
pub const PAGE_SIZE: u32 = 1000;
/// ページ送りの回数の上限（1 ページ 1000 件なら 200 万件。止まらない応答への備え）。
const MAX_PAGES: u32 = 2000;
/// 一覧・トークンの応答の大きさの上限。1 ページ 1000 件で数百 KB なので十分。
const MAX_JSON_BYTES: u64 = 16 << 20;
/// 鍵ファイルの大きさの上限（実物は 2 KB ほど）。
const MAX_KEY_BYTES: u64 = 64 << 10;
/// 原本 1 枚のダウンロードの時間の上限（名前解決から読み切りまで）。agent の 30 秒は大きな原本に足りない。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// トークンの期限のこれだけ前に取り直す。
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// ダウンロードを書き出す単位。停止の合図をこの単位で見る。
const COPY_CHUNK: usize = 256 << 10;

// ---------------------------------------------------------------------------
// 失敗

/// 鍵ファイルの失敗の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// 通常のファイルではない（ディレクトリなど）、または大きすぎる。
    NotRegular,
    /// 読めない（`ErrorKind` だけを持つ）。
    Unreadable(io::ErrorKind),
    /// JSON として読めない、または `client_email`・`private_key` が文字列でない。
    Malformed,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegular => f.write_str("鍵ファイルが通常のファイルではないか大きすぎます"),
            Self::Unreadable(kind) => write!(f, "鍵ファイルを読めません（{kind}）"),
            Self::Malformed => {
                f.write_str("鍵ファイルが JSON として不正か、必要な項目がありません")
            }
        }
    }
}

/// Drive との通信の失敗。**固定の語・状態コード・`ErrorKind` だけ**を持つ（URL・本文・鍵・トークンを含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveError {
    Key(KeyError),
    Jwt(JwtError),
    /// 接続・名前解決・TLS・時間切れなど（[`Failure`]）。
    Net(Failure),
    /// API が成功以外の状態コードを返した（再試行しても直らなかった）。
    Status(u16),
    /// 流量制限（403 の `rateLimitExceeded` など）を再試行の回数だけ受けても直らなかった。権限の不足の
    /// 403（[`DriveError::Status`]）と区別する。写真のせいではない。
    RateLimited(u16),
    /// 本文の受信が時間の上限（[`DOWNLOAD_TIMEOUT`]）を超えた。接続や応答の待ちの時間切れ
    /// （[`DriveError::Net`] の `Timeout`）と違い、その写真が大きい・遅いだけで通信の障害とは言えない。
    BodyTimeout,
    /// トークンの交換が成功以外の状態コードで断られた。
    TokenRejected(u16),
    /// 応答が JSON として読めない・想定した形でない。
    BadResponse,
    /// 応答・ダウンロードが大きすぎる。
    TooLarge,
    /// ページ送りが止まらない。
    TooManyPages,
    /// 名前で指定したフォルダが見つからない。
    FolderNotFound,
    /// 出力先への書き込み・本文の読み出しの失敗。
    Io(io::ErrorKind),
    /// 停止の合図で中断した。
    Stopped,
}

impl fmt::Display for DriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(e) => e.fmt(f),
            Self::Jwt(e) => write!(f, "JWT を作れません: {e}"),
            Self::Net(failure) => write!(f, "通信に失敗しました（{}）", failure.as_str()),
            Self::Status(code) => write!(f, "Drive API が HTTP {code} を返しました"),
            Self::RateLimited(code) => {
                write!(f, "Drive API が流量制限（HTTP {code}）を返し続けました")
            }
            Self::BodyTimeout => f.write_str("ダウンロードが時間の上限を超えました"),
            Self::TokenRejected(code) => {
                write!(f, "トークンの交換が HTTP {code} で断られました")
            }
            Self::BadResponse => f.write_str("Drive の応答が想定した形ではありません"),
            Self::TooLarge => f.write_str("応答かファイルが大きすぎます"),
            Self::TooManyPages => f.write_str("一覧のページ送りが止まりません"),
            Self::FolderNotFound => {
                f.write_str("Drive に指定のフォルダが見つかりません（共有を確認してください）")
            }
            Self::Io(kind) => write!(f, "入出力に失敗しました（{kind}）"),
            Self::Stopped => f.write_str("停止の合図で中断しました"),
        }
    }
}

impl std::error::Error for DriveError {}

impl From<JwtError> for DriveError {
    fn from(e: JwtError) -> Self {
        Self::Jwt(e)
    }
}

impl DriveError {
    /// 鍵・署名・トークンの交換の失敗か（Python の「認証エラー」にあたる）。
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Key(_) | Self::Jwt(_) | Self::TokenRejected(_))
    }

    /// 写真 1 枚のせいではなく、通信や Drive 側の不調と言える失敗か。取得の途中でこれが出たら、残りの
    /// 写真を試しても同じなので段を止める（写真の失敗の記録も付けない）。
    ///
    /// 接続・名前解決・TLS・応答の待ちの時間切れ（`Net`）と流量制限は障害。**本文の受信が上限を超えた**
    /// （[`DriveError::BodyTimeout`]）ことだけは、その写真の失敗として数える。
    pub fn is_outage(&self) -> bool {
        match self {
            // `Timeout` は要求を送って応答の頭を受けるまでの時間切れと、403 の理由の本文の受信の時間切れだけ
            // （原本の本文の読み出しは `BodyTimeout`）。応答の頭の前の切断・本文の途中の切断も障害。
            // 403 の理由の本文が読めない（`ReasonUnreadable`）のも障害
            Self::Net(failure) => failure.is_outage(),
            Self::RateLimited(_) => true,
            // 401 は、取り直したトークンでも断られた（`authorized` が 1 回やり直したあと）。写真ではなく
            // サービスアカウント側（スコープ・無効化）の問題
            Self::Status(code) => *code == 401 || *code == 429 || *code >= 500,
            Self::Key(_) | Self::Jwt(_) | Self::TokenRejected(_) | Self::Stopped => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// 通信先・再試行

/// 通信先。本番は定数だけ（[`Endpoints::production`]）。
#[derive(Debug, Clone)]
pub struct Endpoints {
    token_url: String,
    api_base: String,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            token_url: TOKEN_URL.to_string(),
            api_base: API_BASE.to_string(),
        }
    }

    /// 試験用: ループバックのスタブへ向ける。**試験のビルドだけにある**（配布するバイナリにはコードが無い）。
    #[cfg(test)]
    pub(crate) fn stub(base: &str) -> Self {
        Self {
            token_url: format!("{base}/token"),
            api_base: format!("{base}/drive/v3"),
        }
    }
}

/// 再試行の方針。
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    /// 最初の要求のあとに再試行する回数の上限。
    pub max_retries: u32,
    /// 1 回目の再試行までの待ち。以降は倍々。
    pub base: Duration,
    /// 1 回の待ちの上限（`Retry-After` も、この値で切る）。
    pub cap: Duration,
}

impl Retry {
    /// 1・2・4・8 秒の 4 回（合計 15 秒）。同期サイクルは 10 分おきなので、長く粘らない。
    pub const fn production() -> Self {
        Self {
            max_retries: 4,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
    }

    fn wait_for(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self
            .base
            .checked_mul(1u32 << attempt.min(16))
            .unwrap_or(self.cap);
        retry_after.unwrap_or(backoff).min(self.cap)
    }
}

// ---------------------------------------------------------------------------
// 段をまたぐ状態

struct CachedToken {
    value: String,
    expires_at: Instant,
}

// トークンの中身を `{:?}` で出さない
impl fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CachedToken(..)")
    }
}

#[derive(Debug, Default)]
struct State {
    token: Option<CachedToken>,
    /// 解決したフォルダの ID（現行の `_drive_folder_id`。プロセスの間ずっと覚える）。
    folder_id: Option<String>,
}

/// `index_drive`・`fetch_derivatives` が共有する状態（トークン・フォルダ ID・通信先）。
/// 現行は 1 サイクルで `svc` を作って両方に渡す。Rust の段は別のオブジェクトなので、これを `Arc` で共有する。
#[derive(Debug)]
pub struct DriveShared {
    endpoints: Endpoints,
    retry: Retry,
    state: Mutex<State>,
    /// このサイクルの `index_drive` が認証に失敗した（現行は `svc` が `None` になり、両方の段を飛ばす）。
    auth_failed: AtomicBool,
    /// 鍵ファイルの権限の警告を出した（プロセスで 1 回）。
    permission_warned: AtomicBool,
    /// 原本 1 枚のダウンロードの時間の上限。本番は [`DOWNLOAD_TIMEOUT`] だけ（試験が短くできる）。
    download_timeout: Duration,
}

impl DriveShared {
    pub fn new() -> Self {
        Self::with(Endpoints::production(), Retry::production())
    }

    pub(crate) fn with(endpoints: Endpoints, retry: Retry) -> Self {
        Self {
            endpoints,
            retry,
            state: Mutex::new(State::default()),
            auth_failed: AtomicBool::new(false),
            permission_warned: AtomicBool::new(false),
            download_timeout: DOWNLOAD_TIMEOUT,
        }
    }

    /// 試験用: ダウンロードの時間の上限を短くする。
    #[cfg(test)]
    pub(crate) fn with_download_timeout(mut self, timeout: Duration) -> Self {
        self.download_timeout = timeout;
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_auth_failed(&self, failed: bool) {
        self.auth_failed.store(failed, Ordering::SeqCst);
    }

    fn auth_failed(&self) -> bool {
        self.auth_failed.load(Ordering::SeqCst)
    }
}

impl Default for DriveShared {
    fn default() -> Self {
        Self::new()
    }
}

/// 鍵ファイルから読む 2 つの値。
struct ServiceAccount {
    email: String,
    private_key: String,
}

/// 鍵ファイルを読む。通常のファイルだけ（`DRIVE_KEY.exists()` と同じくリンクは辿る）。
/// 他のユーザーに読める権限なら、プロセスで 1 回だけ警告する（値は出さない）。
fn load_service_account(path: &Path, warned: &AtomicBool) -> Result<ServiceAccount, KeyError> {
    // 開く前に種類を見る（FIFO を開くと、書き手が来るまで止まる）。リンクは辿る
    let before = fs::metadata(path).map_err(|e| KeyError::Unreadable(e.kind()))?;
    if !before.is_file() {
        return Err(KeyError::NotRegular);
    }
    let file = fs::File::open(path).map_err(|e| KeyError::Unreadable(e.kind()))?;
    let meta = file
        .metadata()
        .map_err(|e| KeyError::Unreadable(e.kind()))?;
    if !meta.is_file() || meta.len() > MAX_KEY_BYTES {
        return Err(KeyError::NotRegular);
    }
    if meta.permissions().mode() & 0o077 != 0 && !warned.swap(true, Ordering::SeqCst) {
        tracing::warn!(
            "[drive] 鍵ファイルが所有者以外にも読める権限です（{:03o}）。0600 にしてください",
            meta.permissions().mode() & 0o777
        );
    }
    let mut text = String::new();
    file.take(MAX_KEY_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| KeyError::Unreadable(e.kind()))?;
    let json: Value = serde_json::from_str(&text).map_err(|_| KeyError::Malformed)?;
    let field = |name: &str| {
        json.get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(KeyError::Malformed)
    };
    Ok(ServiceAccount {
        email: field("client_email")?,
        private_key: field("private_key")?,
    })
}

/// 現行の `re.fullmatch(r"[A-Za-z0-9_-]{20,}", name)`。フォルダの設定が ID なら、名前の検索をしない。
pub fn looks_like_folder_id(setting: &str) -> bool {
    setting.len() >= 20
        && setting
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Drive の検索式の文字列リテラルに入れる値のエスケープ（`\` と `'`）。現行は無加工で、`'` を含む名前は
/// 検索式が壊れて HTTP 400 になる。
fn escape_query_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '\'') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// 一覧

/// `files.list` が返した 1 ファイル（取り込み対象の MIME のものだけ）。
///
/// `Debug` は手書きで、**ID・ファイル名・撮影時刻・座標を出さない**（有無だけ。個人情報がログ・パニックの
/// 表示・試験の失敗の表示に漏れないように）。
#[derive(Clone, PartialEq)]
pub struct RemoteFile {
    pub id: String,
    /// 無いことは本来ない。無いファイルは索引に入れず、一覧には「有る」ものとして数える。
    pub name: Option<String>,
    pub md5: Option<String>,
    /// `imageMediaMetadata.time`（Drive の文字列のまま。`norm_time` は索引が通す）。
    pub time: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteFile")
            .field("has_name", &self.name.is_some())
            .field("has_md5", &self.md5.is_some())
            .field("has_time", &self.time.is_some())
            .field("has_location", &(self.lat.is_some() || self.lon.is_some()))
            .finish_non_exhaustive()
    }
}

/// 一覧の結果。`files` は**Drive が返した順**で、同じ ID が 2 回あれば最初の位置に最後の値を入れる
/// （現行の `remote[f["id"]] = f` の dict と同じ。新しい行の `id` はこの順に振られる）。
#[derive(Default, PartialEq)]
pub struct Listing {
    pub files: Vec<RemoteFile>,
    pub pages: u32,
}

// 件数だけ（`RemoteFile` の `Debug` も中身を出さないが、一覧の全件を並べない）
impl fmt::Debug for Listing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Listing")
            .field("files", &self.files.len())
            .field("pages", &self.pages)
            .finish()
    }
}

fn remote_file(value: &Value) -> Result<Option<RemoteFile>, DriveError> {
    // MIME で絞るのは現行と同じ。絞り込みの外のものは `id` を見ない
    let Some(mime) = value.get("mimeType").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !DRIVE_MIMES.contains(&mime) {
        return Ok(None);
    }
    // 現行は `f["id"]` で、無ければ例外（一覧の取得ごと失敗し、DB には触れない）
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(DriveError::BadResponse)?;
    let meta = value.get("imageMediaMetadata");
    let loc = meta.and_then(|m| m.get("location"));
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    Ok(Some(RemoteFile {
        id: id.to_owned(),
        name: text(value.get("name")),
        md5: text(value.get("md5Checksum")),
        time: text(meta.and_then(|m| m.get("time"))),
        lat: loc.and_then(|l| l.get("latitude")).and_then(Value::as_f64),
        lon: loc.and_then(|l| l.get("longitude")).and_then(Value::as_f64),
    }))
}

// ---------------------------------------------------------------------------
// 1 回の段の通信

/// 段の 1 回の実行で使う通信。トークンや通信先は [`DriveShared`] が持つ。
pub struct Session<'a> {
    shared: &'a DriveShared,
    http: &'a ureq::Agent,
    stop: &'a StopFlag,
    key_path: &'a Path,
}

impl<'a> Session<'a> {
    pub fn new(
        shared: &'a DriveShared,
        http: &'a ureq::Agent,
        stop: &'a StopFlag,
        key_path: &'a Path,
    ) -> Self {
        Self {
            shared,
            http,
            stop,
            key_path,
        }
    }

    /// 有効なアクセストークン（無い・期限が近いときは取り直す）。**呼び出し側は値をログに出さないこと。**
    pub fn token(&self) -> Result<String, DriveError> {
        {
            let state = self.shared.state();
            if let Some(token) = &state.token
                && token.expires_at > Instant::now() + TOKEN_REFRESH_MARGIN
            {
                return Ok(token.value.clone());
            }
        }
        let fresh = self.fetch_token()?;
        let value = fresh.value.clone();
        self.shared.state().token = Some(fresh);
        Ok(value)
    }

    fn invalidate_token(&self) {
        self.shared.state().token = None;
    }

    fn fetch_token(&self) -> Result<CachedToken, DriveError> {
        let account = load_service_account(self.key_path, &self.shared.permission_warned)
            .map_err(DriveError::Key)?;
        let signer = Rs256Signer::from_pem(&account.private_key)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let assertion = signer.sign(&GoogleAssertion::new(
            &account.email,
            SCOPE,
            &self.shared.endpoints.token_url,
            now,
        ))?;
        // JWT は URL 安全な Base64 と `.` だけなので、そのまま form に入れられる
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
        );
        let started = Instant::now();
        let response = self
            .send_with_retry(|| {
                self.http
                    .post(&self.shared.endpoints.token_url)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .send(body.as_str())
            })
            .map_err(|e| match e {
                // 4xx は鍵・assertion を断られた。429・5xx は Google 側の不調（認証の失敗ではない）
                DriveError::Status(code) if code != 429 && code < 500 => {
                    DriveError::TokenRejected(code)
                }
                other => other,
            })?;
        let json = read_json(response)?;
        let value = json
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or(DriveError::BadResponse)?
            .to_owned();
        // Google は 3599。無い・異常なら 1 時間までの安全な値にする
        let lifetime = json
            .get("expires_in")
            .and_then(Value::as_u64)
            .map_or(Duration::from_secs(3000), |s| {
                Duration::from_secs(s.min(3600))
            });
        Ok(CachedToken {
            value,
            expires_at: started + lifetime,
        })
    }

    /// 要求を送る。429・5xx・流量制限の 403 は待って再試行する。成功（2xx）以外は [`DriveError`]。
    /// `send` は毎回新しい要求を作って送る関数。
    fn send_with_retry(
        &self,
        send: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<ureq::http::Response<ureq::Body>, DriveError> {
        let retry = self.shared.retry;
        let mut attempt = 0;
        loop {
            if self.stop.is_stopped() {
                return Err(DriveError::Stopped);
            }
            let mut response = send().map_err(|e| DriveError::Net(Failure::from(&e)))?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(response);
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let transient = status == 429
                || status >= 500
                // 403 は権限の不足と流量制限が同じ状態コード。本文の理由で見分ける（本文は読んで捨てる。
                // 理由の本文の受信に失敗したら、権限の不足と決めつけず通信の障害として返す）
                || (status == 403 && is_rate_limit_body(&mut response)?);
            if !transient {
                return Err(DriveError::Status(status));
            }
            if attempt >= retry.max_retries {
                // 403 は流量制限だった（`transient` は本文の理由で決めた）。理由を失って権限の不足と見分けが
                // つかなくならないよう、別の失敗として返す
                return Err(if status == 403 {
                    DriveError::RateLimited(status)
                } else {
                    DriveError::Status(status)
                });
            }
            tracing::debug!(
                "[drive] HTTP {status}。再試行します（{}/{}）",
                attempt + 1,
                retry.max_retries
            );
            if self.stop.wait(retry.wait_for(attempt, retry_after)) {
                return Err(DriveError::Stopped);
            }
            attempt += 1;
        }
    }

    /// トークンつきの GET。401 ならトークンを取り直して 1 回だけやり直す。`build` が要求を組み立てる
    /// （引数はトークン）。
    fn authorized(
        &self,
        build: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<ureq::http::Response<ureq::Body>, DriveError> {
        for refreshed in [false, true] {
            let token = self.token()?;
            match self.send_with_retry(|| build(&token)) {
                Err(DriveError::Status(401)) if !refreshed => self.invalidate_token(),
                other => return other,
            }
        }
        Err(DriveError::Status(401))
    }

    fn list_page(
        &self,
        query: &str,
        fields: &str,
        page_size: u32,
        page_token: Option<&str>,
    ) -> Result<Value, DriveError> {
        let url = format!("{}/files", self.shared.endpoints.api_base);
        let response = self.authorized(|token| {
            let mut request = self
                .http
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .query("q", query)
                .query("fields", fields)
                .query("pageSize", page_size.to_string());
            if let Some(page_token) = page_token {
                request = request.query("pageToken", page_token);
            }
            request.call()
        })?;
        read_json(response)
    }

    /// 設定のフォルダ（名前、または ID）を ID にする。解決した ID は覚える（現行の `_drive_folder_id`）。
    pub fn resolve_folder(&self, setting: &str) -> Result<String, DriveError> {
        if let Some(id) = self.shared.state().folder_id.clone() {
            return Ok(id);
        }
        let id = if looks_like_folder_id(setting) {
            setting.to_owned()
        } else {
            let query = format!(
                "name='{}' and mimeType='application/vnd.google-apps.folder' and trashed=false",
                escape_query_literal(setting)
            );
            let json = self.list_page(&query, "files(id)", 5, None)?;
            json.get("files")
                .and_then(Value::as_array)
                .and_then(|files| files.first())
                .and_then(|f| f.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(DriveError::FolderNotFound)?
                .to_owned()
        };
        self.shared.state().folder_id = Some(id.clone());
        Ok(id)
    }

    /// フォルダ直下の画像の一覧（サブフォルダは辿らない。現行と同じ）。**全ページが取れたときだけ**
    /// `Ok` を返す（途中で失敗したら、取れた分は捨てて `Err`。呼び出し側はそれで削除を判断しない）。
    pub fn list_folder(&self, folder_id: &str) -> Result<Listing, DriveError> {
        let query = format!("'{folder_id}' in parents and trashed=false");
        let mut files: Vec<RemoteFile> = Vec::new();
        let mut position: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut token: Option<String> = None;
        let mut pages = 0u32;
        loop {
            if self.stop.is_stopped() {
                return Err(DriveError::Stopped);
            }
            if pages >= MAX_PAGES {
                return Err(DriveError::TooManyPages);
            }
            let json = self.list_page(&query, LIST_FIELDS, PAGE_SIZE, token.as_deref())?;
            pages += 1;
            let items = match json.get("files") {
                None => &[][..],
                Some(Value::Array(items)) => items.as_slice(),
                Some(_) => return Err(DriveError::BadResponse),
            };
            for item in items {
                if let Some(file) = remote_file(item)? {
                    match position.get(&file.id) {
                        Some(&i) => files[i] = file,
                        None => {
                            position.insert(file.id.clone(), files.len());
                            files.push(file);
                        }
                    }
                }
            }
            let next = match json.get("nextPageToken") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.is_empty() => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(DriveError::BadResponse),
            };
            match next {
                None => break,
                // 同じトークンが続くと止まらない
                Some(next) if token.as_deref() == Some(next.as_str()) => {
                    return Err(DriveError::TooManyPages);
                }
                Some(next) => token = Some(next),
            }
        }
        Ok(Listing { files, pages })
    }

    /// 原本をダウンロードして `out` に書く。`limit` バイトを超えたら [`DriveError::TooLarge`]。
    /// 戻り値は書いたバイト数。`file_id` は呼び出し側が [`crate::datadir::check_drive_id`] を通したもの。
    pub fn download(
        &self,
        file_id: &str,
        out: &mut impl Write,
        limit: u64,
    ) -> Result<u64, DriveError> {
        let url = format!("{}/files/{file_id}", self.shared.endpoints.api_base);
        let response = self.authorized(|token| {
            self.http
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .query("alt", "media")
                .config()
                .timeout_global(Some(self.shared.download_timeout))
                .build()
                .call()
        })?;
        let (_, body) = response.into_parts();
        if body.content_length().is_some_and(|len| len > limit) {
            return Err(DriveError::TooLarge);
        }
        // `ureq` の `limit` は圧縮された（gzip の）バイト列に当たり、展開後には効かないので付けない。
        // 展開後の大きさは、下の読み出しで数えて上限（`limit`）を超えたところで止める
        let mut reader = body.into_with_config().reader();
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut total = 0u64;
        loop {
            if self.stop.is_stopped() {
                return Err(DriveError::Stopped);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(read_error(&e)),
            };
            total += n as u64;
            if total > limit {
                return Err(DriveError::TooLarge);
            }
            out.write_all(&buf[..n])
                .map_err(|e| DriveError::Io(e.kind()))?;
        }
        Ok(total)
    }
}

/// 原本の本文を読んでいる最中の失敗（応答のヘッダーは受け取り済み）。`ureq` の本文の読み出しは `ureq` の
/// エラーを `io::Error` に包んで返す（`Io` は中身の `io::Error` をそのまま返す）。
///
/// | 失敗 | `DriveError` | 扱い |
/// | --- | --- | --- |
/// | 時間切れ（`Timeout(_)` を包んだもの・`TimedOut`・`WouldBlock`） | `BodyTimeout` | その写真の失敗（遅いだけ） |
/// | 切断（[`crate::net::is_disconnect`] の種類: `UnexpectedEof`・`ConnectionReset`・`ConnectionAborted`・`BrokenPipe` など） | `Net(Disconnected)` | 通信の障害（段を中断） |
/// | 包まれた `ureq` のエラー（`Tls` など） | `Net(`[`Failure::from`]`)` | `Other` 以外は障害 |
/// | それ以外の I/O | `Io(kind)` | その写真の失敗 |
///
/// ヘッダーの前の失敗の分類は [`Failure`] の表（`net.rs`）。
fn read_error(e: &io::Error) -> DriveError {
    if let Some(inner) = e.get_ref().and_then(|x| x.downcast_ref::<ureq::Error>()) {
        return match Failure::from(inner) {
            Failure::Timeout => DriveError::BodyTimeout,
            failure => DriveError::Net(failure),
        };
    }
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => DriveError::BodyTimeout,
        kind if net::is_disconnect(kind) => DriveError::Net(Failure::Disconnected),
        kind => DriveError::Io(kind),
    }
}

/// 403 の理由の本文の読み出しの失敗（`ureq` のエラー）を、障害の種類にする。`BodyExceedsLimit`（上限超過）は
/// 呼び出し側が先に取り除く。
///
/// | `ureq` のエラー | `Failure` |
/// | --- | --- |
/// | 切断・時間切れ・TLS など、[`Failure::from`] が障害にするもの | そのまま（`disconnected`・`timeout`・`tls` など） |
/// | それ以外すべて（`Protocol`＝不正なチャンク長、`Decompress`＝途中で終わる gzip など） | [`Failure::ReasonUnreadable`] |
///
/// ヘッダーの `Protocol` は写真の失敗のまま（[`Failure`] の表）。ここは**理由が読めない**ことそのものが
/// 障害なので、種類によらず段を止める。
fn reason_body_failure(e: &ureq::Error) -> Failure {
    match Failure::from(e) {
        failure if failure.is_outage() => failure,
        _ => Failure::ReasonUnreadable,
    }
}

/// 403 の本文が流量制限の理由か（読み捨てる。小さい上限つき）。
///
/// 理由の本文が上限（16 KiB）を超えたものは、Drive の流量制限の応答（数百バイト）ではないので `false`
/// （権限の不足など。写真の失敗）。**それ以外の読み出しの失敗は、種類によらず `Err`** にして通信の障害
/// （[`DriveError::Net`]）として伝える。**権限の不足（`Status(403)`）と決めつけて写真の失敗にしない**。
/// 時間切れも `BodyTimeout` ではなく `Net(Timeout)`（原本の本文が遅いのとは違い、数百バイトの理由の本文が
/// 届かないのは回線の不調）。切断・時間切れ・TLS 以外（`Protocol`・`Decompress` など）は
/// [`Failure::ReasonUnreadable`]（[`reason_body_failure`]）。
fn is_rate_limit_body(response: &mut ureq::http::Response<ureq::Body>) -> Result<bool, DriveError> {
    // 上限は展開後の大きさに効かせる（`net::read_body_limited`）
    let bytes = match net::read_body_limited(response.body_mut(), 16 << 10) {
        Ok(bytes) => bytes,
        Err(ureq::Error::BodyExceedsLimit(_)) => return Ok(false),
        Err(e) => return Err(DriveError::Net(reason_body_failure(&e))),
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.contains("rateLimitExceeded") || text.contains("userRateLimitExceeded"))
}

fn read_json(response: ureq::http::Response<ureq::Body>) -> Result<Value, DriveError> {
    let (_, mut body) = response.into_parts();
    // 上限は展開後の大きさに効かせる（`net::read_body_limited`）
    let bytes = net::read_body_limited(&mut body, MAX_JSON_BYTES).map_err(|e| match e {
        ureq::Error::BodyExceedsLimit(_) => DriveError::TooLarge,
        other => DriveError::Net(Failure::from(&other)),
    })?;
    serde_json::from_slice(&bytes).map_err(|_| DriveError::BadResponse)
}
