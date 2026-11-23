//! Drive API 直接方式の HTTP の層（docs/note-photo-export/drive-api.md 4 章・5 章）: 通信先（[`Endpoints`]）、期限（[`Deadline`]）、
//! 失敗（[`ApiError`]）、再試行つきの送信（`Transport`）。
//!
//! 写真サービスの `crates/geoframe/src/drive.rs`（`send_with_retry`・`authorized`）と `net.rs`（`Failure`・本文の上限つきの読み出し）を
//! 写して、書き込み向けに直したもの（共通の crate は作らない）。写真サービスは読むだけなので、ここで足したのは次の 4 つ。
//!
//! - 「済んでいるか」の確認（`SendOpts::settled`）: 書き込みを再送する前と、4xx で断られたときに、今の状態を見て、目的の状態なら
//!   成功とする（移動・ゴミ箱へ移すは、再送すると別の結果になりうるため）。
//! - 308 などの状態コードを呼び出し側へそのまま返す（`SendOpts::passthrough`。resumable のアップロードの続き）。
//! - 期限が 2 種類（操作の期限とジョブ全体の期限）あり、どちらで切れたかを区別して返す（[`Deadline`]）。
//! - キャンセル（[`Cancel`]）を、要求の前と再試行の待ちの最中に見る。
//!
//! 守ること:
//! - リダイレクトはたどらない（`max_redirects(0)`。3xx は成功ではない）。環境変数の proxy も使わない。
//! - `Authorization` は TLS の上だけ。http は、試験用の loopback の通信先（[`Endpoints::loopback`]）のときだけ許す。
//! - ログに出すのは、操作名・状態コード・再試行の回数・Drive のエラーの `reason`（短い固定の語）だけ。URL（`pageToken`・
//!   `upload_id` を含みうる）・トークン・応答の本文・ファイル名は出さない。

use std::fmt;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cancel::Cancel;
use crate::remote::RemoteError;

use super::auth::CredentialsError;

/// 本物の Drive API v3 の基底。
pub const API_BASE: &str = "https://www.googleapis.com/drive/v3";
/// 本物のアップロードの基底。
pub const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
/// 本物のトークンの取得先。
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// 接続だけの待ち。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 応答の本文（JSON）の上限。1 ページ 1000 件で数百 KB。
pub const MAX_JSON_BYTES: u64 = 16 << 20;
/// エラー応答の本文の上限（Drive のエラーは数百バイトの JSON）。
const MAX_ERROR_BODY_BYTES: u64 = 64 << 10;
/// 待ちを刻む間隔（キャンセルと期限を見る）。
const SLEEP_SLICE: Duration = Duration::from_millis(20);

// ---- 通信先 ----

/// 通信先（API・アップロード・トークンの 3 つの URL）。本物の固定値か、試験用の loopback の http だけ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    api: String,
    upload: String,
    token: String,
    /// http（TLS なし）を許すか。loopback の通信先だけ真。
    plain_http: bool,
}

/// `--drive-endpoint` の値が、loopback の http（`http://127.0.0.1:<port>`・`http://[::1]:<port>`）の形でない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointError;

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "--drive-endpoint は http://127.0.0.1:<port> か http://[::1]:<port> の形だけ受け付けます",
        )
    }
}

impl std::error::Error for EndpointError {}

impl Endpoints {
    /// 本物の固定値（https）。
    pub fn production() -> Self {
        Self {
            api: API_BASE.to_owned(),
            upload: UPLOAD_BASE.to_owned(),
            token: TOKEN_URL.to_owned(),
            plain_http: false,
        }
    }

    /// 試験用: 偽の Drive の HTTP サーバーへ向ける。**`http://127.0.0.1:<port>` と `http://[::1]:<port>` だけ**（ポートは 1〜65535 の
    /// 10 進。経路・クエリ・ユーザー情報・末尾の `/`・ほかのホスト名は断る）。API・アップロード・トークンの 3 つの URL を、この下に
    /// 置く（`/drive/v3`・`/upload/drive/v3`・`/token`）。
    pub fn loopback(base: &str) -> Result<Self, EndpointError> {
        let rest = base.strip_prefix("http://").ok_or(EndpointError)?;
        let port = rest
            .strip_prefix("127.0.0.1:")
            .or_else(|| rest.strip_prefix("[::1]:"))
            .ok_or(EndpointError)?;
        let digits_ok = (1..=5).contains(&port.len())
            && port.bytes().all(|b| b.is_ascii_digit())
            && !port.starts_with('0');
        if !digits_ok || port.parse::<u32>().map_or(true, |p| p > 65535) {
            return Err(EndpointError);
        }
        Ok(Self {
            api: format!("{base}/drive/v3"),
            upload: format!("{base}/upload/drive/v3"),
            token: format!("{base}/token"),
            plain_http: true,
        })
    }

    pub fn api(&self) -> &str {
        &self.api
    }

    pub fn upload(&self) -> &str {
        &self.upload
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// 送る URL が許される形か（https、または loopback の通信先のときの http）。
    fn allows(&self, url: &str) -> bool {
        url.starts_with("https://") || (self.plain_http && url.starts_with("http://"))
    }

    /// `url` のスキーム・ホスト・ポートが、アップロードの通信先と同じか（resumable の `Location` の検査）。
    pub fn same_upload_origin(&self, url: &str) -> bool {
        match (origin(url), origin(&self.upload)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

/// URL のスキーム・ホスト（小文字）・ポート。ユーザー情報（`@`）・不正な形は `None`。
fn origin(url: &str) -> Option<(&'static str, String, u16)> {
    let (scheme, rest, default_port) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r, 443)
    } else {
        let r = url.strip_prefix("http://")?;
        ("http", r, 80)
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse::<u16>().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        (format!("[{h}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse::<u16>().ok()?),
            None => (authority.to_owned(), default_port),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host.to_ascii_lowercase(), port))
}

// ---- 期限 ----

/// どちらの期限が効いているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expiry {
    /// 操作の期限（`--drive-timeout`・`--drive-upload-timeout` や、呼び出し側が渡した締め切り）。
    Operation,
    /// ジョブ全体の期限。
    Job,
}

/// 締め切り。操作の期限とジョブ全体の期限の早いほうで、どちらかを覚えておく（切れたときに返すエラーが違う）。
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
    expiry: Expiry,
}

impl Deadline {
    /// 操作の期限（切れたら [`ApiError::Timeout`]）。
    pub fn operation(at: Instant) -> Self {
        Self {
            at,
            expiry: Expiry::Operation,
        }
    }

    /// ジョブ全体の期限（切れたら [`ApiError::JobDeadline`]）。
    pub fn job(at: Instant) -> Self {
        Self {
            at,
            expiry: Expiry::Job,
        }
    }

    /// `now + timeout` の操作の期限。`job` があり、それが早い（または同じ）なら、ジョブ全体の期限にする。
    pub fn within(timeout: Duration, job: Option<Instant>) -> Self {
        let at = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
        match job {
            Some(j) if j <= at => Self::job(j),
            _ => Self::operation(at),
        }
    }

    pub fn at(&self) -> Instant {
        self.at
    }

    /// 残り時間（過ぎていれば `None`）。
    pub fn remaining(&self) -> Option<Duration> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// 切れたときのエラー。
    pub fn error(&self) -> ApiError {
        match self.expiry {
            Expiry::Operation => ApiError::Timeout,
            Expiry::Job => ApiError::JobDeadline,
        }
    }
}

/// 1 つの操作の間、そのまま持ち回る締め切りとキャンセルの印。
#[derive(Debug, Clone, Copy)]
pub(super) struct Ctx<'a> {
    pub cancel: &'a Cancel,
    pub deadline: Deadline,
}

impl Ctx<'_> {
    /// キャンセルされていれば `Cancelled`（期限より先に見る）、期限が過ぎていればその期限のエラー。
    pub fn check(&self) -> Result<(), ApiError> {
        if self.cancel.is_cancelled() {
            return Err(ApiError::Cancelled);
        }
        if self.deadline.remaining().is_none() {
            return Err(self.deadline.error());
        }
        Ok(())
    }

    /// `wait` だけ眠る。細かく刻んで、キャンセルと期限を見る。期限までに終わらない待ちは、眠らずに期限のエラーを返す。
    pub fn sleep(&self, wait: Duration) -> Result<(), ApiError> {
        self.check()?;
        match self.deadline.remaining() {
            Some(left) if wait < left => {}
            _ => return Err(self.deadline.error()),
        }
        let end = Instant::now() + wait;
        loop {
            self.check()?;
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(SLEEP_SLICE));
        }
    }
}

// ---- 失敗 ----

/// 通信の失敗の種類（メッセージ・URL を持たない。ログに出してよい）。`crates/geoframe/src/net.rs` の `Failure` と同じ分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Dns,
    Connect,
    /// 接続は張れたが、応答の頭を受ける前、または本文の途中で切れた。
    Disconnected,
    Tls,
    /// 要求 1 回の時間切れ（締め切りの超過は [`ApiError::Timeout`]・[`ApiError::JobDeadline`]）。
    Timeout,
    /// 応答の形が壊れている・要求を組み立てられないなど、通信の障害と言えないもの（再試行しない）。
    Other,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Disconnected => "disconnected",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Other => "other",
        }
    }

    /// 再試行する失敗か（`Other` 以外）。
    fn retryable(self) -> bool {
        !matches!(self, Self::Other)
    }
}

impl From<&ureq::Error> for Failure {
    fn from(e: &ureq::Error) -> Self {
        use io::ErrorKind as K;
        match e {
            ureq::Error::HostNotFound => Self::Dns,
            ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => Self::Connect,
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::Tls(_)
            | ureq::Error::Rustls(_)
            | ureq::Error::Pem(_)
            | ureq::Error::TlsRequired => Self::Tls,
            ureq::Error::Io(io) => match io.kind() {
                K::InvalidData => Self::Tls,
                K::TimedOut | K::WouldBlock => Self::Timeout,
                K::ConnectionRefused
                | K::NetworkUnreachable
                | K::HostUnreachable
                | K::NetworkDown => Self::Connect,
                _ => Self::Disconnected,
            },
            _ => Self::Other,
        }
    }
}

/// Drive API の操作の失敗。固定の語・状態コード・種類だけを持つ（URL・本文・トークン・ファイル名を含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// キャンセルの印（SIGINT・SIGTERM）が立った。変更系の操作の途中なら、結果は分からない。
    Cancelled,
    /// 操作の期限（`--drive-timeout`・`--drive-upload-timeout`）が切れた。
    Timeout,
    /// ジョブ全体の期限が切れた。
    JobDeadline,
    /// 認可が失効している（トークンの取得が `invalid_grant`・`invalid_client`・`unauthorized_client`）。再認可が要る。
    AuthInvalid,
    /// 404（その ID が無い）。
    NotFound,
    /// 成功以外の状態コード（再試行しても直らなかった、または再試行しない 4xx・3xx）。
    Status(u16),
    /// 流量制限（429・403 の `rateLimitExceeded`）が、再試行の回数を使い切っても直らなかった。
    RateLimited,
    /// 通信の失敗（再試行を使い切った）。
    Net(Failure),
    /// 応答が、決められた形・値でない。
    BadResponse,
    /// 応答・ファイルが上限を超えた。
    TooLarge,
    /// ページ送りが止まらない（上限のページ数、または同じ `nextPageToken` の繰り返し）、または件数・深さの上限を超えた。
    TooManyPages,
    /// 書き込みのあとの照合（409 のあとの `get`、送ったあとの大きさ・MD5）が、期待と違う。
    Mismatch,
    /// 送る前に断った（形の違う ID・名前、https でない URL、別のホストの `Location`、手元のファイルの大きさ・MD5 の違い）。
    Rejected(&'static str),
    /// 手元のファイルの読み出しの失敗。
    Io(io::ErrorKind),
    /// 資格情報のファイルを読めない。
    Credentials(CredentialsError),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("キャンセルされました"),
            Self::Timeout => f.write_str("Drive の操作が期限内に終わりませんでした"),
            Self::JobDeadline => f.write_str("ジョブ全体の期限を過ぎました"),
            Self::AuthInvalid => f.write_str("Drive の認可が失効しています（再認可が要ります）"),
            Self::NotFound => f.write_str("Drive に指定の ID がありません"),
            Self::Status(code) => write!(f, "Drive API が HTTP {code} を返しました"),
            Self::RateLimited => f.write_str("Drive API が流量制限を返し続けました"),
            Self::Net(failure) => write!(f, "通信に失敗しました（{}）", failure.as_str()),
            Self::BadResponse => f.write_str("Drive の応答が決められた形ではありません"),
            Self::TooLarge => f.write_str("Drive の応答かファイルが上限を超えています"),
            Self::TooManyPages => {
                f.write_str("Drive の一覧の上限（ページ数・件数・深さ）を超えました")
            }
            Self::Mismatch => f.write_str("Drive の状態が、期待した書き込みの結果と違います"),
            Self::Rejected(why) => write!(f, "送る前に断りました（{why}）"),
            Self::Io(kind) => write!(f, "手元のファイルを読めません（{kind}）"),
            Self::Credentials(e) => write!(f, "資格情報を読めません: {e}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<ApiError> for RemoteError {
    /// 呼び出し側（[`crate::remote::RemoteError::into_query_error`]・`into_op_error`）が、ジョブのエラーコードにする
    /// （`drive_unavailable`・`rclone_failed`・`rclone_timeout`・`timeout`・`drive_auth_invalid`）。
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Cancelled => Self::Cancelled,
            ApiError::Timeout => Self::Timeout,
            ApiError::JobDeadline => Self::JobDeadline,
            ApiError::AuthInvalid => Self::AuthInvalid,
            ApiError::NotFound => Self::NotFound,
            ApiError::TooLarge => Self::TooLarge,
            ApiError::BadResponse | ApiError::TooManyPages => Self::BadOutput,
            ApiError::Rejected(_) | ApiError::Io(_) | ApiError::Credentials(_) => Self::Spawn,
            ApiError::Status(_) | ApiError::RateLimited | ApiError::Net(_) | ApiError::Mismatch => {
                Self::Failed
            }
        }
    }
}

impl ApiError {
    /// 状態コード（成功以外）をエラーにする。
    pub(super) fn status(code: u16) -> Self {
        if code == 404 {
            Self::NotFound
        } else {
            Self::Status(code)
        }
    }
}

// ---- 要求・応答 ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    Get,
    Post,
    Patch,
    Put,
}

/// 1 つの要求（毎回、同じ内容で送り直せる）。
pub(super) struct Req<'a> {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: &'a [u8],
}

impl<'a> Req<'a> {
    pub fn get(url: String) -> Self {
        Self {
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: &[],
        }
    }

    pub fn with_body(method: Method, url: String, content_type: &str, body: &'a [u8]) -> Self {
        Self {
            method,
            url,
            headers: vec![("Content-Type", content_type.to_owned())],
            body,
        }
    }

    pub fn header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }
}

/// 受け取った応答（本文は、上限つきで全部読んである）。
pub(super) struct Reply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub location: Option<String>,
    pub range: Option<String>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<Value, ApiError> {
        serde_json::from_slice(&self.body).map_err(|_| ApiError::BadResponse)
    }

    /// Drive のエラー応答の `reason`（`error.errors[].reason`）。決まった語だけ返し、ほかは `other`（ログ向け。本文は出さない）。
    /// 流量制限の理由が 1 つでもあれば、それを返す。
    pub fn reason(&self) -> &'static str {
        const KNOWN: [&str; 10] = [
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "dailyLimitExceeded",
            "backendError",
            "internalError",
            "notFound",
            "forbidden",
            "insufficientFilePermissions",
            "fileNotDownloadable",
            "authError",
        ];
        if self.is_rate_limit() {
            return if self.text().contains("userRateLimitExceeded") {
                "userRateLimitExceeded"
            } else {
                "rateLimitExceeded"
            };
        }
        let reasons: Vec<String> = serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|v| v["error"]["errors"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e["reason"].as_str().map(str::to_owned))
            .collect();
        reasons
            .iter()
            .find_map(|r| KNOWN.into_iter().find(|k| k == r))
            .unwrap_or("other")
    }

    fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// 流量制限の 403 か。本文のどこかに `rateLimitExceeded`・`userRateLimitExceeded` があれば真（`errors[]` の何番目でも、
    /// 形が少し違っても拾う。写真サービスの `is_rate_limit_body` と同じ判定。権限の不足の 403 は、これらを含まない）。
    fn is_rate_limit(&self) -> bool {
        let text = self.text();
        text.contains("rateLimitExceeded") || text.contains("userRateLimitExceeded")
    }
}

// ---- 再試行の方針 ----

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
    /// 1・2・4・8・16 秒の 5 回（上限 30 秒。揺らぎ込みで合計 1 分ほど）。
    pub const fn production() -> Self {
        Self {
            max_retries: 5,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
    }

    /// `attempt`（0 から）回目の失敗のあとの待ち。`Retry-After` があればそれ（上限で切る）、無ければ指数の待ちに 0〜25% の揺らぎ。
    fn wait_for(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra.min(self.cap);
        }
        let backoff = self
            .base
            .checked_mul(1u32 << attempt.min(16))
            .unwrap_or(self.cap)
            .min(self.cap);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let jitter = backoff.mul_f64(f64::from(nanos % 1000) / 4000.0);
        (backoff + jitter).min(self.cap)
    }
}

/// 送信の指定。
pub(super) struct SendOpts<'a> {
    /// ログ用の操作名（`list` など）。
    pub op: &'static str,
    /// 応答の本文の上限（超えたら [`ApiError::TooLarge`]）。
    pub max_body: u64,
    /// 要求 1 回の時間切れの上限（締め切りの残りが短ければそちら）。
    pub cap: Duration,
    /// 呼び出し側が自分で扱う成功以外の状態コード（そのまま [`Reply`] で返す。409・404・308 など）。401 は常にそのまま返す。
    pub passthrough: &'a [u16],
    /// 「もう目的の状態か」の確認。再送の前と、再試行しない 4xx のときに呼び、真なら [`Outcome::AlreadyDone`]。
    pub settled: Option<&'a dyn Fn() -> Result<bool, ApiError>>,
}

impl<'a> SendOpts<'a> {
    pub fn new(op: &'static str) -> Self {
        Self {
            op,
            max_body: MAX_JSON_BYTES,
            cap: Duration::from_secs(60),
            passthrough: &[],
            settled: None,
        }
    }
}

pub(super) enum Outcome {
    Reply(Reply),
    /// 「済んでいるか」の確認が真だった。
    AlreadyDone,
}

// ---- 送信 ----

/// HTTP の送り手。`Clone` は安く、接続は共有される。
#[derive(Debug, Clone)]
pub(super) struct Transport {
    agent: ureq::Agent,
    endpoints: Endpoints,
    retry: Retry,
}

impl Transport {
    pub fn new(endpoints: Endpoints, retry: Retry) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .proxy(None)
            // 4xx/5xx も応答として受け、状態コードで判断する
            .http_status_as_error(false)
            .user_agent(concat!("geoframe-export/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            endpoints,
            retry,
        }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    pub fn retry(&self) -> Retry {
        self.retry
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    /// 要求を 1 回だけ送り、応答（本文まで）を受ける。通信の失敗は [`ApiError::Net`]。
    pub fn attempt(
        &self,
        ctx: &Ctx<'_>,
        req: &Req<'_>,
        token: Option<&str>,
        opts: &SendOpts<'_>,
    ) -> Result<Reply, ApiError> {
        ctx.check()?;
        if !self.endpoints.allows(&req.url) {
            return Err(ApiError::Rejected("https でない URL には送りません"));
        }
        let timeout = ctx
            .deadline
            .remaining()
            .ok_or_else(|| ctx.deadline.error())?
            .min(opts.cap);
        let result = self.send_once(req, token, timeout);
        // 要求の途中で期限が切れた（締め切りの残りで切った時間切れ）は、再試行せず、その期限のエラーにする
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                let failure = Failure::from(&e);
                if failure == Failure::Timeout && ctx.deadline.remaining().is_none() {
                    return Err(ctx.deadline.error());
                }
                return Err(ApiError::Net(failure));
            }
        };
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let retry_after = header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let location = header("location");
        let range = header("range");
        let (_, mut body) = response.into_parts();
        // 成功の本文は呼び出し側の上限（`read` は上限つきで読む）、エラーの本文は小さな JSON なので固定の上限
        let limit = if (200..300).contains(&status) {
            opts.max_body
        } else {
            MAX_ERROR_BODY_BYTES
        };
        let bytes = match read_body_limited(&mut body, limit) {
            Ok(b) => b,
            Err(ureq::Error::BodyExceedsLimit(_)) => return Err(ApiError::TooLarge),
            Err(e) => {
                let failure = Failure::from(&e);
                if failure == Failure::Timeout && ctx.deadline.remaining().is_none() {
                    return Err(ctx.deadline.error());
                }
                return Err(ApiError::Net(failure));
            }
        };
        Ok(Reply {
            status,
            retry_after,
            location,
            range,
            body: bytes,
        })
    }

    fn send_once(
        &self,
        req: &Req<'_>,
        token: Option<&str>,
        timeout: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        macro_rules! prepare {
            ($builder:expr) => {{
                let mut b = $builder.config().timeout_global(Some(timeout)).build();
                for (name, value) in &req.headers {
                    b = b.header(*name, value.as_str());
                }
                if let Some(token) = token {
                    b = b.header("Authorization", format!("Bearer {token}"));
                }
                b
            }};
        }
        match req.method {
            Method::Get => prepare!(self.agent.get(&req.url)).call(),
            Method::Post => prepare!(self.agent.post(&req.url)).send(req.body),
            Method::Patch => prepare!(self.agent.patch(&req.url)).send(req.body),
            Method::Put => prepare!(self.agent.put(&req.url)).send(req.body),
        }
    }

    /// 再試行つきで送る。
    ///
    /// - 2xx、`opts.passthrough` の状態コード、401 は、そのまま [`Outcome::Reply`] で返す。
    /// - 通信の失敗・429・5xx・403 の流量制限は、`Retry-After`（秒。無ければ指数の待ち）で待って再試行する。回数の上限は
    ///   [`Retry::max_retries`]。待ちの最中にキャンセルと期限を見る。使い切ったら、通信の失敗は [`ApiError::Net`]、流量制限は
    ///   [`ApiError::RateLimited`]、5xx は [`ApiError::Status`]。
    /// - それ以外の 4xx・3xx は、再試行せず [`ApiError::Status`]（404 は [`ApiError::NotFound`]）。`opts.settled` があれば、その前に
    ///   確認して、済んでいれば [`Outcome::AlreadyDone`]。
    pub fn send_with_retry(
        &self,
        ctx: &Ctx<'_>,
        req: &Req<'_>,
        token: Option<&str>,
        opts: &SendOpts<'_>,
    ) -> Result<Outcome, ApiError> {
        let mut attempt: u32 = 0;
        loop {
            ctx.check()?;
            if attempt > 0
                && let Some(settled) = opts.settled
                && settled()?
            {
                return Ok(Outcome::AlreadyDone);
            }
            let (retry_after, failed): (Option<Duration>, ApiError) = match self
                .attempt(ctx, req, token, opts)
            {
                Ok(reply) => {
                    let status = reply.status;
                    if reply.ok() || status == 401 || opts.passthrough.contains(&status) {
                        return Ok(Outcome::Reply(reply));
                    }
                    let rate_limited = status == 429 || (status == 403 && reply.is_rate_limit());
                    if !(rate_limited || status >= 500) {
                        if let Some(settled) = opts.settled
                            && settled()?
                        {
                            return Ok(Outcome::AlreadyDone);
                        }
                        tracing::warn!(
                            op = opts.op,
                            status,
                            reason = reply.reason(),
                            "Drive API が断りました"
                        );
                        return Err(ApiError::status(status));
                    }
                    let err = if rate_limited {
                        ApiError::RateLimited
                    } else {
                        ApiError::Status(status)
                    };
                    tracing::debug!(
                        op = opts.op,
                        status,
                        reason = reply.reason(),
                        retry = attempt + 1,
                        "Drive API が再試行できる状態コードを返しました"
                    );
                    (reply.retry_after, err)
                }
                Err(ApiError::Net(failure)) if failure.retryable() => {
                    tracing::debug!(
                        op = opts.op,
                        failure = failure.as_str(),
                        retry = attempt + 1,
                        "Drive との通信に失敗しました"
                    );
                    (None, ApiError::Net(failure))
                }
                Err(e) => return Err(e),
            };
            if attempt >= self.retry.max_retries {
                tracing::warn!(op = opts.op, retries = attempt, "再試行を使い切りました");
                return Err(failed);
            }
            ctx.sleep(self.retry.wait_for(attempt, retry_after))?;
            attempt += 1;
        }
    }
}

/// 応答の本文を、**展開後の大きさ**に上限をかけて読む（`crates/geoframe/src/net.rs` の `read_body_limited` と同じ）。
/// 圧縮前にも独立の上限（`max × 2 + 64KiB`）をかける。
fn read_body_limited(body: &mut ureq::Body, max: u64) -> Result<Vec<u8>, ureq::Error> {
    let wire = max.saturating_mul(2).saturating_add(64 << 10);
    let reader = body.with_config().limit(wire).reader();
    let mut out = Vec::new();
    reader
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| match ureq::Error::from(e) {
            ureq::Error::BodyExceedsLimit(_) => ureq::Error::BodyExceedsLimit(max),
            other => other,
        })?;
    if out.len() as u64 > max {
        return Err(ureq::Error::BodyExceedsLimit(max));
    }
    Ok(out)
}

/// URL に入れる値のパーセントエンコード（英数字と `-._~` 以外を `%XX` にする）。
pub(super) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_endpoints_accept_only_the_two_exact_forms() {
        for ok in [
            "http://127.0.0.1:8080",
            "http://127.0.0.1:1",
            "http://[::1]:65535",
        ] {
            let e = Endpoints::loopback(ok).unwrap_or_else(|_| panic!("{ok}"));
            assert_eq!(e.api(), format!("{ok}/drive/v3"));
            assert_eq!(e.upload(), format!("{ok}/upload/drive/v3"));
            assert_eq!(e.token(), format!("{ok}/token"));
        }
        for bad in [
            "",
            "http://127.0.0.1",
            "http://127.0.0.1:",
            "http://127.0.0.1:0",
            "http://127.0.0.1:080",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:1234567",
            "http://127.0.0.1:80/",
            "http://127.0.0.1:80/x",
            "http://127.0.0.1:80?x=1",
            "http://127.0.0.1:80#x",
            "http://127.0.0.1:80@host",
            "http://127.0.0.1.evil:80",
            "http://127.0.0.1:80.evil",
            "http://user@127.0.0.1:80",
            "http://localhost:80",
            "http://0.0.0.0:80",
            "http://127.0.0.2:80",
            "http://[::2]:80",
            "http://[::1]",
            "https://127.0.0.1:80",
            "https://www.googleapis.com",
            "http://example.com:80",
            " http://127.0.0.1:80",
            "http://127.0.0.1:80 ",
            "HTTP://127.0.0.1:80",
        ] {
            assert!(Endpoints::loopback(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn production_endpoints_are_https_and_refuse_plain_http() {
        let e = Endpoints::production();
        assert!(e.api().starts_with("https://www.googleapis.com/"));
        assert!(e.upload().starts_with("https://www.googleapis.com/"));
        assert!(e.token().starts_with("https://oauth2.googleapis.com/"));
        assert!(e.allows("https://example.com/x"));
        assert!(!e.allows("http://127.0.0.1:80/x"));
        assert!(
            Endpoints::loopback("http://127.0.0.1:80")
                .unwrap()
                .allows("http://127.0.0.1:80/x")
        );
    }

    #[test]
    fn the_session_location_must_have_the_upload_origin() {
        let e = Endpoints::loopback("http://127.0.0.1:8080").unwrap();
        assert!(e.same_upload_origin("http://127.0.0.1:8080/upload/drive/v3/files?upload_id=x"));
        assert!(e.same_upload_origin("http://127.0.0.1:8080"));
        for bad in [
            "http://localhost:8080/upload",
            "http://127.0.0.1:8081/upload",
            "https://127.0.0.1:8080/upload",
            "http://127.0.0.1.evil:8080/upload",
            "http://127.0.0.1:8080@evil/upload",
            "http://evil@127.0.0.1:8080/upload",
            "/upload/drive/v3/files",
            "",
        ] {
            assert!(!e.same_upload_origin(bad), "{bad:?}");
        }
        let p = Endpoints::production();
        assert!(
            p.same_upload_origin("https://www.googleapis.com/upload/drive/v3/files?upload_id=x")
        );
        assert!(p.same_upload_origin("https://WWW.GOOGLEAPIS.COM:443/upload"));
        assert!(!p.same_upload_origin("https://www.googleapis.com.evil/upload"));
        assert!(!p.same_upload_origin("https://oauth2.googleapis.com/upload"));
        assert!(!p.same_upload_origin("http://www.googleapis.com/upload"));
    }

    #[test]
    fn the_deadline_remembers_which_limit_applies() {
        let far = Instant::now() + Duration::from_secs(3600);
        assert_eq!(
            Deadline::within(Duration::from_secs(1), Some(far)).error(),
            ApiError::Timeout
        );
        let near = Instant::now() + Duration::from_millis(10);
        assert_eq!(
            Deadline::within(Duration::from_secs(60), Some(near)).error(),
            ApiError::JobDeadline
        );
        assert_eq!(
            Deadline::within(Duration::from_secs(60), None).error(),
            ApiError::Timeout
        );
        let past = Deadline::operation(Instant::now() - Duration::from_secs(1));
        assert!(past.remaining().is_none());
    }

    #[test]
    fn waits_honour_retry_after_and_are_capped() {
        let r = Retry {
            max_retries: 3,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(2),
        };
        assert_eq!(
            r.wait_for(0, Some(Duration::from_secs(1))),
            Duration::from_secs(1)
        );
        assert_eq!(
            r.wait_for(0, Some(Duration::from_secs(3600))),
            Duration::from_secs(2)
        );
        for attempt in 0..8 {
            let w = r.wait_for(attempt, None);
            let base = Duration::from_millis(100 << attempt.min(16)).min(r.cap);
            assert!(w >= base && w <= r.cap, "{attempt}: {w:?}");
        }
    }

    #[test]
    fn a_sleep_stops_on_cancel_and_never_outlives_the_deadline() {
        let cancel = Cancel::new();
        let deadline = Deadline::operation(Instant::now() + Duration::from_secs(60));
        let ctx = Ctx {
            cancel: &cancel,
            deadline,
        };
        let c2 = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c2.cancel();
        });
        let start = Instant::now();
        assert_eq!(ctx.sleep(Duration::from_secs(30)), Err(ApiError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(5));
        t.join().unwrap();

        // 期限より長い待ちは、眠らずに期限のエラー
        let cancel = Cancel::new();
        let ctx = Ctx {
            cancel: &cancel,
            deadline: Deadline::job(Instant::now() + Duration::from_millis(200)),
        };
        let start = Instant::now();
        assert_eq!(
            ctx.sleep(Duration::from_secs(30)),
            Err(ApiError::JobDeadline)
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn api_errors_map_to_remote_errors() {
        use crate::error::ErrorCode;
        let code = |e: ApiError| {
            let r = RemoteError::from(e);
            (r.into_query_error().code, r.into_op_error().code)
        };
        assert_eq!(
            code(ApiError::AuthInvalid),
            (ErrorCode::DriveAuthInvalid, ErrorCode::DriveAuthInvalid)
        );
        assert_eq!(
            code(ApiError::Timeout),
            (ErrorCode::DriveUnavailable, ErrorCode::RcloneTimeout)
        );
        assert_eq!(
            code(ApiError::JobDeadline),
            (ErrorCode::Timeout, ErrorCode::Timeout)
        );
        assert_eq!(
            code(ApiError::Net(Failure::Disconnected)),
            (ErrorCode::DriveUnavailable, ErrorCode::RcloneFailed)
        );
        assert_eq!(
            code(ApiError::Status(500)),
            (ErrorCode::DriveUnavailable, ErrorCode::RcloneFailed)
        );
        assert_eq!(
            code(ApiError::NotFound),
            (ErrorCode::DriveUnavailable, ErrorCode::RcloneFailed)
        );
        assert_eq!(
            RemoteError::from(ApiError::Cancelled),
            RemoteError::Cancelled
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("a b'c"), "a%20b%27c");
        assert_eq!(encode("abc-._~123"), "abc-._~123");
        assert_eq!(encode("日"), "%E6%97%A5");
    }
}
