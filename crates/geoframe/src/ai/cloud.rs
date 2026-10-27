//! T4.6a: Vertex の認証・用途別クラウド通信境界。handler の登録や domain の判断はしない。
//!
//! プロセスで [`CloudClients::new`] を一度呼び、clone を共有する。同じ Provider の photo_judge /
//! location_suggest / token 交換は背景1・対話1を共有する。構築時の鍵読取り・DNS・HTTP は無い。
//! 入口は [`CloudClient::run`] / [`CloudClient::run_checked`]。任意 URL/header/JSON の公開 POST は無い。
mod auth;
mod request;
mod target;
mod transport;

use super::{
    http::Deadline,
    jobs::ErrorCode,
    lanes::{Lane, Lanes},
    provider::{Phase, ProviderError},
    settings::{AiConfig, CloudTarget, ProviderKind, TaskKind},
    worker::{RunCtx, RunError},
};
use auth::Auth;
pub use request::{GenerateRequest, JsonReply, RequestError};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use target::Target;
use transport::{SendError, Transport};

type Clock = dyn Fn() -> Option<u64> + Send + Sync;

struct Provider {
    cloud: CloudTarget,
    api: Transport,
    token: Transport,
    lanes: Arc<Lanes>,
    auth: Arc<Auth>,
    clock: Arc<Clock>,
}

/// 用途別のクライアント群。clone しても cache/agent/背景・対話の枠は分かれない。
#[derive(Clone)]
pub struct CloudClients {
    clients: HashMap<TaskKind, CloudClient>,
}
impl std::fmt::Debug for CloudClients {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudClients").finish_non_exhaustive()
    }
}

impl CloudClients {
    /// 設定の文字列だけを検査・固定する。ADC/環境/Drive鍵の探索はしない。
    pub fn new(config: &AiConfig) -> Result<Self, RunError> {
        let mut clients = HashMap::new();
        let mut providers = HashMap::<String, Arc<Provider>>::new();
        let auth = Arc::new(Auth::default());
        let clock: Arc<Clock> = Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        });
        if let Some(settings) = config.settings() {
            for task in [TaskKind::PhotoJudge, TaskKind::LocationSuggest] {
                let Some(s) = settings
                    .task(task)
                    .filter(|s| s.provider == ProviderKind::Cloud)
                else {
                    continue;
                };
                let cloud = s.cloud.as_ref().ok_or(contract())?;
                let target = Target::generate(cloud, &s.model).ok_or(contract())?;
                let id = settings.provider_fingerprint(task).ok_or(contract())?;
                let provider = providers
                    .entry(id)
                    .or_insert_with(|| {
                        Arc::new(Provider {
                            cloud: cloud.clone(),
                            api: Transport::new(&target.host),
                            token: Transport::new(target::TOKEN_HOST),
                            lanes: Lanes::new(),
                            auth: auth.clone(),
                            clock: clock.clone(),
                        })
                    })
                    .clone();
                clients.insert(
                    task,
                    CloudClient {
                        provider,
                        task,
                        target,
                        model: s.model.clone(),
                    },
                );
            }
        }
        Ok(Self { clients })
    }

    pub fn client(&self, task: TaskKind) -> Option<CloudClient> {
        self.clients.get(&task).cloned()
    }

    #[cfg(test)]
    pub(in crate::ai) fn loopback(
        config: &AiConfig,
        api: std::net::SocketAddr,
        token: std::net::SocketAddr,
    ) -> Self {
        use super::gate::GateConnector;
        use super::tests_http::FakeDns;
        use ureq::unversioned::transport::{Connector, DefaultConnector};
        let mut clients = Self::new(config).unwrap();
        let mut shared = HashMap::new();
        for client in clients.clients.values_mut() {
            let id = Arc::as_ptr(&client.provider) as usize;
            client.provider = shared
                .entry(id)
                .or_insert_with(|| {
                    let dns = FakeDns::new();
                    dns.script(&client.target.host, vec![vec![api]]);
                    dns.script(target::TOKEN_HOST, vec![vec![token]]);
                    Arc::new(Provider {
                        cloud: client.provider.cloud.clone(),
                        api: Transport::stub(
                            &client.target.host,
                            api.port(),
                            Arc::new(dns.clone()),
                            DefaultConnector::new().chain(GateConnector),
                        ),
                        token: Transport::stub(
                            target::TOKEN_HOST,
                            token.port(),
                            Arc::new(dns),
                            DefaultConnector::new().chain(GateConnector),
                        ),
                        auth: client.provider.auth.clone(),
                        lanes: client.provider.lanes.clone(),
                        clock: Arc::new(|| Some(crate::ai::tests_worker::T0 as u64)),
                    })
                })
                .clone();
        }
        clients
    }
}

/// Provider ごとの共有基盤を参照する用途別の入口。Debug はモデル・宛先・認証パスを出さない。
#[derive(Clone)]
pub struct CloudClient {
    provider: Arc<Provider>,
    task: TaskKind,
    target: Target,
    model: String,
}
impl std::fmt::Debug for CloudClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudClient")
            .field("task", &self.task)
            .finish_non_exhaustive()
    }
}

fn contract() -> RunError {
    ProviderError::contract(true).into_run_error(true)
}

impl CloudClient {
    /// 同期の専用ワーカースレッドで呼ぶ。DB/writer/画像ロックを保持して呼ばない。
    /// request 検査 → probe gate → Provider 枠 → token → send gate（1予約）→ 推論。
    /// 401 だけ一度 token を更新する。毎送信前に gate/予約を通し、期限は延長しない。
    pub fn run(&self, ctx: &RunCtx<'_>, request: &GenerateRequest) -> Result<JsonReply, RunError> {
        self.run_until(ctx, request, &Deadline::for_task(self.task))
    }

    /// 入力と依存サービスを token より前、および各 POST の予約待ち後に再検査する。
    /// callback の I/O にも同じ全体期限を渡す。401 refresh でも期限を作り直さない。
    pub fn run_checked(
        &self,
        ctx: &RunCtx<'_>,
        request: &GenerateRequest,
        check: impl Fn(&Deadline) -> Result<(), RunError>,
    ) -> Result<JsonReply, RunError> {
        self.run_checked_until(ctx, request, &Deadline::for_task(self.task), check)
    }

    fn run_until(
        &self,
        ctx: &RunCtx<'_>,
        request: &GenerateRequest,
        deadline: &Deadline,
    ) -> Result<JsonReply, RunError> {
        self.run_checked_until(ctx, request, deadline, |_| Ok(()))
    }

    fn run_checked_until(
        &self,
        ctx: &RunCtx<'_>,
        request: &GenerateRequest,
        deadline: &Deadline,
        check: impl Fn(&Deadline) -> Result<(), RunError>,
    ) -> Result<JsonReply, RunError> {
        // 準備を完了し、上限/用途を鍵・DNS・token より先に検査する。
        if request.body.len() > super::http::MAX_REQUEST_BYTES {
            return Err(RunError::Permanent(ErrorCode::InputTooLarge));
        }
        if request.task != self.task || ctx.task() != self.task {
            return Err(RunError::Permanent(ErrorCode::InputInvalid));
        }
        let gate = ctx.before_probe()?;
        if gate.settings.provider != ProviderKind::Cloud
            || gate.settings.cloud.as_ref() != Some(&self.provider.cloud)
            || gate.settings.model != self.model
        {
            return Err(contract());
        }
        check(deadline)?;
        let wait = deadline
            .remaining()
            .unwrap_or_default()
            .min(Duration::from_secs(2));
        let _permit = self
            .provider
            .lanes
            .acquire(Lane::Background, wait, ctx.stop())
            .map_err(|e| ProviderError::from_http(e.into(), Phase::Probe).into_run_error(true))?;
        let mut token = self.provider.auth.token(
            &self.provider.cloud.credential_path,
            &self.provider.token,
            deadline,
            ctx.stop(),
            &*self.provider.clock,
        )?;
        for round in 0..2 {
            // token 更新や枠待ち中に停止・pause された場合にも送らない。
            ctx.before_probe()?;
            ctx.before_send_until(deadline)?;
            check(deadline)?;
            let reply = self
                .provider
                .api
                .post(
                    &self.target.path,
                    &request.body,
                    Some(token.bearer()),
                    deadline,
                    ctx.stop(),
                )
                .map_err(inference_error)?;
            if reply.status == 401 && round == 0 {
                self.provider
                    .auth
                    .invalidate(&self.provider.cloud.credential_path, &token);
                ctx.before_probe()?;
                token = self.provider.auth.token(
                    &self.provider.cloud.credential_path,
                    &self.provider.token,
                    deadline,
                    ctx.stop(),
                    &*self.provider.clock,
                )?;
                continue;
            }
            if !reply.is_success() {
                return Err(ProviderError::from_status(
                    reply.status,
                    reply.retry_after_secs,
                    Phase::Send,
                    false,
                )
                .into_run_error(true));
            }
            return request::parse_reply(&reply.body)
                .ok_or(RunError::Attempt(ErrorCode::BadResponse));
        }
        unreachable!("401 再送は1回だけ")
    }
}

/// DNS/接続/TLS と実際の最初の write 試行を区別する。予約は取り消さない。
fn inference_error(e: SendError) -> RunError {
    let phase = if e.started { Phase::Send } else { Phase::Probe };
    ProviderError::from_http(e.error, phase).into_run_error(true)
}

#[cfg(test)]
mod tests;
