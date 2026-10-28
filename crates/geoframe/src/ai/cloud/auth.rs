//! Drive と独立した token cache。mutex はメモリ操作だけ、I/O・署名・交換中には持たない。
use super::super::{
    http::Deadline,
    pause::PauseReason,
    provider::ProviderError,
    waits::{Wait, WaitReason, WaitScope},
    worker::RunError,
};
use super::{
    target::{SCOPE, TOKEN_URL},
    transport::Transport,
};
use crate::{
    jwt::{GoogleAssertion, Rs256Signer},
    service_account::{self, Context, KeyError},
    stop::StopFlag,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_MARGIN: u64 = 60;
const MAX_TOKEN_BYTES: usize = 16 << 10;

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    credential: std::path::PathBuf,
    scope: &'static str,
    audience: &'static str,
}

pub(super) struct Token {
    value: String,
    issued: u64,
    expires: u64,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

impl Token {
    pub fn bearer(&self) -> &str {
        &self.value
    }
    fn valid_at(&self, now: u64) -> bool {
        now >= self.issued
            && now
                .checked_add(REFRESH_MARGIN)
                .is_some_and(|t| t < self.expires)
    }
}

#[derive(Default)]
pub(super) struct Auth {
    cache: Mutex<HashMap<CacheKey, Arc<Token>>>,
    warned: AtomicBool,
}

fn unauthorized() -> RunError {
    RunError::ProviderPause(PauseReason::Unauthorized)
}
fn unreachable() -> RunError {
    RunError::ProviderWait(Wait::new(WaitScope::Entrance, WaitReason::Unreachable))
}

impl Auth {
    fn key(path: &Path) -> CacheKey {
        CacheKey {
            credential: path.to_path_buf(),
            scope: SCOPE,
            audience: TOKEN_URL,
        }
    }

    pub fn invalidate(&self, path: &Path, rejected: &Arc<Token>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let key = Self::key(path);
        // 遅れて来た旧要求の 401 は、別スレッドが保存した新しい token を消さない。
        if cache
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, rejected))
        {
            cache.remove(&key);
        }
    }

    pub fn token(
        &self,
        path: &Path,
        transport: &Transport,
        deadline: &Deadline,
        stop: &StopFlag,
        clock: &dyn Fn() -> Option<u64>,
    ) -> Result<Arc<Token>, RunError> {
        if stop.is_stopped() {
            return Err(RunError::Stopping);
        }
        let now = clock().ok_or_else(unauthorized)?;
        let key = Self::key(path);
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        if let Some(token) = cached.filter(|t| t.valid_at(now)) {
            return Ok(token);
        }
        let deadline = deadline.capped(AUTH_TIMEOUT);
        if deadline.remaining().is_none() {
            return Err(unreachable());
        }
        let assertion = self.assertion(path, now)?;
        if stop.is_stopped() {
            return Err(RunError::Stopping);
        }
        // JWT の文字種は base64url + '.' のみ。form の値として percent encode を要しない。
        let form = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
        );
        let reply = transport
            .post("/token", form.as_bytes(), None, &deadline, stop)
            .map_err(|e| {
                // 認証通信は費用・job attempt に数えない。
                match e.error {
                    super::super::http::HttpError::Stopping => RunError::Stopping,
                    super::super::http::HttpError::DestinationDenied
                    | super::super::http::HttpError::BadTarget
                    | super::super::http::HttpError::Redirect(_)
                    | super::super::http::HttpError::ResponseTooLarge => {
                        ProviderError::contract(true).into_run_error(false)
                    }
                    _ => unreachable(),
                }
            })?;
        if !reply.is_success() {
            return Err(match reply.status {
                403 => RunError::ProviderPause(PauseReason::Forbidden),
                429 => RunError::ProviderWait(Wait {
                    scope: WaitScope::Entrance,
                    reason: WaitReason::RateLimited,
                    retry_after_secs: reply.retry_after_secs,
                }),
                500..=599 | 408 => unreachable(),
                400..=499 => unauthorized(),
                _ => ProviderError::contract(true).into_run_error(false),
            });
        }
        let token = Arc::new(parse_token(&reply.body, now).ok_or_else(unauthorized)?);
        if !token.valid_at(clock().ok_or_else(unauthorized)?) {
            return Err(unauthorized());
        }
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, token.clone());
        Ok(token)
    }

    fn assertion(&self, path: &Path, now: u64) -> Result<String, RunError> {
        if now.checked_add(3600).is_none() {
            return Err(unauthorized());
        }
        let account = service_account::load_service_account(path, &self.warned, Context::Ai)
            .map_err(|error| {
                // 権限・所有者で拒否したときだけ、理由を 1 回だけ残す（パスも値も出さない）。
                // 停止の理由は他の認証失敗と同じ `unauthorized` のまま（保存できる理由の列挙は固定）。
                if error == KeyError::Insecure && !self.warned.swap(true, Ordering::SeqCst) {
                    tracing::warn!(
                        "[ai] クラウドの認証ファイルの権限または所有者が安全でないため使いません（0600・実行ユーザーの所有にしてから geoframe ai-jobs release-pause を実行）"
                    );
                }
                unauthorized()
            })?;
        let signer = Rs256Signer::from_pem(&account.private_key).map_err(|_| unauthorized())?;
        signer
            .sign(&GoogleAssertion::new(&account.email, SCOPE, TOKEN_URL, now))
            .map_err(|_| unauthorized())
    }
}

fn parse_token(body: &[u8], now: u64) -> Option<Token> {
    #[derive(Deserialize)]
    struct Response {
        access_token: String,
        token_type: String,
        expires_in: u64,
    }
    let r: Response = serde_json::from_slice(body).ok()?;
    // RFC 6750 b64token。空白・CRLF・非ASCII・巨大なヘッダーを許さない。
    if r.access_token.is_empty()
        || r.access_token.len() > MAX_TOKEN_BYTES
        || !r.token_type.eq_ignore_ascii_case("Bearer")
        || !(61..=3600).contains(&r.expires_in)
        || !r.access_token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
        })
    {
        return None;
    }
    Some(Token {
        value: r.access_token,
        issued: now,
        expires: now.checked_add(r.expires_in)?,
    })
}

#[cfg(test)]
mod tests;
