//! AI 専用の HTTP クライアント（P4 / T4.2。設計は p4-design.md §5）。
//!
//! 取り込みの同期が使う [`crate::net::agent`] は**変えない**。LAN は [`AiHttp`] を通す。
//! クラウドの非公開transport（[`super::cloud`]）も、ここのagent構築・期限・本文制限を共有する。
//!
//! # 固定の振る舞い
//!
//! - 送信先は設定の `local_base_url` の host:port だけ（[`AiHttp::new`]）。呼び出し側が渡せるのは [`Endpoint`]
//!   （固定のパスの列挙。サービス ID は [`ServiceId`] で文字種を検査済み）で、任意の URL・パス・ヘッダーを受ける
//!   入口は無い。名前解決の結果は [`super::dest`] が private／loopback のものだけに固定する。
//! - リダイレクトは追わない（`max_redirects(0)`）。3xx は [`HttpError::Redirect`] で、本文を読まず、Location へは
//!   接続しない。認証・呼び出し側のヘッダーを別の宛先へ出す経路は無い（付けるヘッダーは `X-Client-Name: geoframe`・
//!   `Accept`・POST の `Content-Type` だけ。`Origin` は付けない）。
//! - プロキシは使わない（`proxy(None)`）。環境変数 `HTTP_PROXY`／`HTTPS_PROXY` は LAN でもクラウド既定でも読まない。
//!   クラウドのプロキシは、管理者が設定で明示した場合だけ（T4.6 以降の境界。今の設定 JSON には項目を足さない）。
//! - 停止の合図は、`send` の入口・名前解決のあと・**HTTP 要求の最初のバイトを書く直前**（接続・TLS の待ちのあと。
//!   [`super::gate`]）で見る。書き始めたあとは止めない。止めたときは固定の [`HttpError::Stopping`]。
//! - 時間は用途別の全体期限（[`Deadline`]。名前解決・接続・ヘッダー・本文の読み切りを含む）。Discovery 全体 2 秒、
//!   対話 15 秒、背景 60〜180 秒。接続は 5 秒まで。
//! - 要求 JSON は base64 込みで 8 MiB まで。**送る前に**検査し、超えたら接続も DNS もしない
//!   （[`HttpError::RequestTooLarge`]。同じ入力は何度送っても超えるので呼び出し側は恒久エラーにする）。
//!   応答は展開後 1 MiB まで（[`crate::net::read_body_limited`]。圧縮前の上限も同じ関数が掛ける）。
//!   2xx 以外の本文は読まない。
//! - 失敗は固定の列挙（[`HttpError`]）と HTTP の状態コードだけで、URL・本文・例外の文字列・応答を持たない。
//!
//! # 呼び出しの境界
//!
//! 同期の `ureq` を使うので、tokio の worker や writer のスレッドの上では呼ばない。背景は AI ワーカーの専用スレッド、
//! 対話（検索語の Embedding）は [`AiHttp::run_interactive`]（枠を先に取り、`spawn_blocking` で実行）を使う。
//! 枠は背景 1・対話 1 で独立（[`super::lanes`]）。停止の合図が立っていれば、枠も要求も始めない。

use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::unversioned::transport::{Connector, DefaultConnector};

use super::dest::{self, Lookup, PinResolver, SystemLookup};
use super::gate::{self, GateConnector};
use super::lanes::{Lane, LaneError, Lanes, Permit};
use super::settings::TaskKind;
use crate::net::{self, Failure};
use crate::stop::StopFlag;

/// 要求 JSON（base64 込み）の上限。
pub const MAX_REQUEST_BYTES: usize = 8 << 20;
/// 応答（展開後）の上限。
pub const MAX_RESPONSE_BYTES: u64 = 1 << 20;
/// 接続を張るまでの上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Discovery（catalog・service・health）全体の期限。
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// 対話（検索語の Embedding）の期限。
pub const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(15);
/// 背景の用途別の期限の範囲。
pub const BACKGROUND_MIN_TIMEOUT: Duration = Duration::from_secs(60);
pub const BACKGROUND_MAX_TIMEOUT: Duration = Duration::from_secs(180);

/// 外向きの失敗（固定の列挙。URL・本文・例外の文字列は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// 停止の合図が来ている（送っていない）。
    Stopping,
    /// 枠が使用中（対話は定型 503 に当たる）。送っていない。
    Busy,
    /// 設定した入口以外、または解決先が private／loopback でない（[`super::dest::Denied`]）。送っていない。
    DestinationDenied,
    /// 要求が [`MAX_REQUEST_BYTES`] を超える（送っていない）。
    RequestTooLarge,
    /// 応答が [`MAX_RESPONSE_BYTES`] を超える（展開後、または圧縮前の上限）。
    ResponseTooLarge,
    /// 3xx（追わない）。
    Redirect(u16),
    /// 通信の失敗（`net::Failure` の分類）。
    Net(Failure),
    /// ヘッダーは来たが、本文が読めなかった。
    BodyUnreadable,
    /// 期限を使い切っている。
    Timeout,
    /// 設定した入口の URL が使えない（検査済みのはずの設定の不整合）。
    BadTarget,
    /// 内部の失敗（スレッドの異常終了など）。
    Internal,
}

impl HttpError {
    /// 固定のコード（ログ・STATUS 用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::Busy => "busy",
            Self::DestinationDenied => "destination_denied",
            Self::RequestTooLarge => "request_too_large",
            Self::ResponseTooLarge => "response_too_large",
            Self::Redirect(_) => "redirect",
            Self::Net(failure) => failure.as_str(),
            Self::BodyUnreadable => "body_unreadable",
            Self::Timeout => "timeout",
            Self::BadTarget => "bad_target",
            Self::Internal => "internal",
        }
    }
}

impl From<LaneError> for HttpError {
    fn from(error: LaneError) -> Self {
        match error {
            LaneError::Busy => Self::Busy,
            LaneError::Stopping => Self::Stopping,
        }
    }
}

pub(super) fn classify(error: &ureq::Error) -> HttpError {
    if gate::is_stop(error) {
        return HttpError::Stopping;
    }
    if dest::denied(error).is_some() {
        return HttpError::DestinationDenied;
    }
    match error {
        ureq::Error::BodyExceedsLimit(_) => HttpError::ResponseTooLarge,
        other => match Failure::from(other) {
            Failure::Timeout => HttpError::Timeout,
            failure => HttpError::Net(failure),
        },
    }
}

/// 1 つの操作（Discovery の一連の GET や 1 回の POST）の全体期限。
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    pub fn after(limit: Duration) -> Self {
        Self {
            at: Instant::now() + limit,
        }
    }

    /// Discovery 全体（catalog＋service＋health をまとめて 2 秒）。
    pub fn discovery() -> Self {
        Self::after(DISCOVERY_TIMEOUT)
    }

    pub fn interactive() -> Self {
        Self::after(INTERACTIVE_TIMEOUT)
    }

    /// 背景の用途別の期限。範囲外は 60〜180 秒に収める。
    pub fn background(secs: u64) -> Self {
        Self::after(Duration::from_secs(secs).clamp(BACKGROUND_MIN_TIMEOUT, BACKGROUND_MAX_TIMEOUT))
    }

    /// 用途ごとの背景の期限（Decision は上流が 120 秒でヘッダーを返さなければ 504 にするので、その後に届くよう 150 秒）。
    pub fn for_task(task: TaskKind) -> Self {
        match task {
            TaskKind::QueryEmbedding => Self::interactive(),
            TaskKind::ImageEmbedding => Self::background(60),
            TaskKind::LocationClue => Self::background(150),
            TaskKind::PhotoJudge => Self::background(120),
            TaskKind::LocationSuggest => Self::background(180),
        }
    }

    /// 同じ全体期限内の短い操作（認証など）。全体期限を延ばさない。
    pub(super) fn capped(&self, limit: Duration) -> Self {
        Self {
            at: self.at.min(Instant::now() + limit),
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        let left = self.at.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }
}

/// LAN のサービス ID（`/llm/services/<id>` のパスに入る）。英数字で始まる 64 字以内（英数字と `.` `_` `-`）で、
/// `..` を含まない。設定の検査（[`super::settings`]）を通っていても、パスを作る前にここでもう一度検査する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn parse(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        (first_ok && rest_ok && text.len() <= 64 && !text.contains(".."))
            .then(|| Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 呼べる API（固定のパスだけ。任意のパスを受ける入口は作らない）。
#[derive(Debug, Clone, Copy)]
pub enum Endpoint<'a> {
    /// `GET /llm/catalog`
    Catalog,
    /// `GET /llm/services/<id>`
    Service(&'a ServiceId),
    /// `GET /llm-decision/health`
    DecisionHealth,
    /// `GET /llm-decision/v1/models`
    DecisionModels,
    /// `POST /llm-decision/v1/systemone`
    DecisionSystemOne,
}

impl Endpoint<'_> {
    pub fn is_post(&self) -> bool {
        matches!(self, Self::DecisionSystemOne)
    }

    fn path(&self) -> String {
        match self {
            Self::Catalog => "/llm/catalog".to_string(),
            Self::Service(id) => format!("/llm/services/{}", id.as_str()),
            Self::DecisionHealth => "/llm-decision/health".to_string(),
            Self::DecisionModels => "/llm-decision/v1/models".to_string(),
            Self::DecisionSystemOne => "/llm-decision/v1/systemone".to_string(),
        }
    }
}

/// 応答。2xx のときだけ本文を読む（それ以外は空）。`Debug` は本文を出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    /// `Retry-After` が秒数（10 進の整数）のとき。日付形式は読まない。
    pub retry_after_secs: Option<u32>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Reply {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

struct Inner {
    agent: ureq::Agent,
    scheme: &'static str,
    authority: String,
    lanes: Arc<Lanes>,
}

/// AI 専用の HTTP クライアント。複製は安く、接続プールと枠は共有される。プロセスで 1 つ作って共有する
/// （背景の枠・対話の枠をワーカーと HTTP の受付で共有するため）。
#[derive(Clone)]
pub struct AiHttp(Arc<Inner>);

impl std::fmt::Debug for AiHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiHttp").finish_non_exhaustive()
    }
}

impl AiHttp {
    /// 設定の `local_base_url`（`http`／`https` の `scheme://host[:port]`）から作る。名前解決は OS。
    pub fn new(base_url: &str) -> Result<Self, HttpError> {
        Self::build(
            base_url,
            Arc::new(SystemLookup),
            DefaultConnector::new().chain(GateConnector),
        )
    }

    /// 試験だけが偽の名前解決を差し込む（本番の設定・環境変数からは選べない）。
    #[cfg(test)]
    pub fn with_lookup(base_url: &str, lookup: Arc<dyn Lookup>) -> Result<Self, HttpError> {
        Self::build(
            base_url,
            lookup,
            DefaultConnector::new().chain(GateConnector),
        )
    }

    /// 試験だけ: 接続（TCP・TLS）を終えたあと、要求を書き始める前に `after_connect` を呼ぶ。
    /// 「接続・TLS の待ちの間に停止が来た」順序を固定するための差し込み（本番の構築口には無い）。
    #[cfg(test)]
    pub fn with_lookup_and_connect_hook(
        base_url: &str,
        lookup: Arc<dyn Lookup>,
        after_connect: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, HttpError> {
        Self::build(
            base_url,
            lookup,
            DefaultConnector::new()
                .chain(gate::HookConnector(after_connect))
                .chain(GateConnector),
        )
    }

    fn build(
        base_url: &str,
        lookup: Arc<dyn Lookup>,
        connector: impl Connector,
    ) -> Result<Self, HttpError> {
        let uri: ureq::http::Uri = base_url.trim().parse().map_err(|_| HttpError::BadTarget)?;
        let scheme = match uri.scheme_str() {
            Some("http") => "http",
            Some("https") => "https",
            _ => return Err(HttpError::BadTarget),
        };
        let authority = uri.authority().ok_or(HttpError::BadTarget)?;
        if authority.as_str().contains('@')
            || uri.query().is_some()
            || !matches!(uri.path(), "" | "/")
        {
            return Err(HttpError::BadTarget);
        }
        let port = authority
            .port_u16()
            .unwrap_or(if scheme == "https" { 443 } else { 80 });
        let host = authority.host();
        let pinned_host = host.trim_start_matches('[').trim_end_matches(']');
        // ureq の unversioned API は、このファイルのこの 1 か所（と dest.rs の Resolver・gate.rs の Connector）だけ
        let agent = build_agent(connector, PinResolver::new(pinned_host, port, lookup));
        Ok(Self(Arc::new(Inner {
            agent,
            scheme,
            authority: authority.as_str().to_string(),
            lanes: Lanes::new(),
        })))
    }

    /// 枠を取って [`Session`] を作る。`wait` は枠を待つ最大の長さ（0 なら待たない）。
    pub fn session(
        &self,
        lane: Lane,
        wait: Duration,
        stop: &StopFlag,
    ) -> Result<Session, HttpError> {
        let permit = self.0.lanes.acquire(lane, wait, stop)?;
        Ok(Session {
            http: self.clone(),
            stop: stop.clone(),
            _permit: permit,
        })
    }

    /// 対話の要求を、枠を**先に**取ってから専用の blocking スレッドで実行する。枠が使用中なら待たず
    /// [`HttpError::Busy`]（定型 503 に当たる）で、blocking スレッドも使わない。tokio の worker で同期の `ureq`
    /// を呼ばないための境界。枠は `f` が終わる（panic を含む）まで保持され、呼び出し元の future が破棄されても
    /// 実行中の要求は期限（[`Deadline`]）までで終わる。
    pub async fn run_interactive<T, F>(&self, stop: &StopFlag, f: F) -> Result<T, HttpError>
    where
        F: FnOnce(Session) -> T + Send + 'static,
        T: Send + 'static,
    {
        let session = self.session(Lane::Interactive, Duration::ZERO, stop)?;
        tokio::task::spawn_blocking(move || f(session))
            .await
            .map_err(|_| HttpError::Internal)
    }
}

/// 枠を持った間の外向き要求。drop で枠が返る。
pub struct Session {
    http: AiHttp,
    stop: StopFlag,
    _permit: Permit,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    pub fn stop(&self) -> &StopFlag {
        &self.stop
    }

    pub fn get(&self, endpoint: Endpoint<'_>, deadline: &Deadline) -> Result<Reply, HttpError> {
        debug_assert!(!endpoint.is_post());
        self.send(endpoint, None, deadline)
    }

    /// 完成した JSON（base64 込み）を POST する。[`MAX_REQUEST_BYTES`] を超えるものは送らない。
    pub fn post_json(
        &self,
        endpoint: Endpoint<'_>,
        body: &[u8],
        deadline: &Deadline,
    ) -> Result<Reply, HttpError> {
        debug_assert!(endpoint.is_post());
        self.send(endpoint, Some(body), deadline)
    }

    fn send(
        &self,
        endpoint: Endpoint<'_>,
        body: Option<&[u8]>,
        deadline: &Deadline,
    ) -> Result<Reply, HttpError> {
        // 送る前の確認（上限は接続も名前解決もしないうちに）
        if body.is_some_and(|b| b.len() > MAX_REQUEST_BYTES) {
            return Err(HttpError::RequestTooLarge);
        }
        if self.stop.is_stopped() {
            return Err(HttpError::Stopping);
        }
        let remaining = deadline.remaining().ok_or(HttpError::Timeout)?;
        let inner = &self.http.0;
        let url = format!("{}://{}{}", inner.scheme, inner.authority, endpoint.path());
        // 名前解決・接続・TLS を待っている間に来た停止は、要求の最初のバイトを書く直前（gate.rs）で止める。
        // 書き始めたあとは止めない（ヘッダー・本文の残り・応答の受信は続ける）
        let armed = gate::arm(&self.stop);
        let result = match body {
            None => inner
                .agent
                .get(&url)
                .header("X-Client-Name", "geoframe")
                .header("Accept", "application/json")
                .config()
                .timeout_global(Some(remaining))
                .build()
                .call(),
            Some(body) => inner
                .agent
                .post(&url)
                .header("X-Client-Name", "geoframe")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .config()
                .timeout_global(Some(remaining))
                .build()
                .send(body),
        };
        drop(armed);
        read_reply(result.map_err(|e| classify(&e))?)
    }
}

/// LAN とクラウドが共有する、proxy/redirect 無効の agent。公開の任意URL入口ではない。
pub(super) fn build_agent(connector: impl Connector, resolver: PinResolver) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .proxy(None)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(BACKGROUND_MAX_TIMEOUT))
        .max_idle_connections(2)
        .max_idle_connections_per_host(2)
        .user_agent(concat!("geoframe/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::with_parts(config, connector, resolver)
}

pub(super) fn read_reply(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<Reply, HttpError> {
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(HttpError::Redirect(status));
    }
    let retry_after_secs = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let v = v.trim();
            (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<u32>().ok())
                .flatten()
        });
    let body = if (200..300).contains(&status) {
        net::read_body_limited(response.body_mut(), MAX_RESPONSE_BYTES).map_err(
            |e| match classify(&e) {
                HttpError::Net(Failure::Other) => HttpError::BodyUnreadable,
                other => other,
            },
        )?
    } else {
        Vec::new()
    };
    Ok(Reply {
        status,
        retry_after_secs,
        body,
    })
}
