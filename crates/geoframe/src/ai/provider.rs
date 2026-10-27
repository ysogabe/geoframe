//! LAN の Provider の共通部分（P4 / T4.2。設計は p4-design.md §5）。
//!
//! - [`LanClient`]: 設定の `local_base_url` に固定した [`AiHttp`]（接続プールと背景・対話の枠を持つ。プロセスで 1 つ作って
//!   AI ワーカーの handler と HTTP の受付で共有する）。
//! - [`ProviderError`]: Discovery・Decision の失敗を、**ジョブをどう扱うか**で分けた固定の列挙。URL・本文・例外の
//!   文字列・モデル応答は持たない。[`ProviderError::into_run_error`] が [`RunError`]（ワーカーの分類）へ対応付ける。
//! - HTTP の失敗・状態コードの分類表（下）。
//!
//! # 分類表
//!
//! | 状況 | Discovery（catalog・service・health） | 推論の POST |
//! | --- | --- | --- |
//! | DNS・接続・TLS・時間切れ・切断 | 入口全体の待ち（`unreachable`） | 送信後の失敗。Discovery をやり直し、使えなければ待ち、使えるなら**そのジョブの反復失敗**（`Repeated`。クラウドは `Attempt`） |
//! | 解決先が private／loopback でない・3xx・設定外の入口 | 永続停止（`contract_mismatch`、Provider 全体） | 同左 |
//! | 401・403 | 永続停止（`unauthorized`・`forbidden`、Provider 全体） | 同左 |
//! | 404 | catalog は Provider 全体の契約不一致、service は用途の `model_not_found` | 用途の `model_not_found` |
//! | 429 | 入口全体の待ち（`rate_limited`、`Retry-After`） | 同左 |
//! | 503 | 入口全体の待ち（`busy`。lock の競合） | 送信後の失敗（上の 1 行目と同じ扱い） |
//! | その他の 5xx | 入口全体の待ち（`unreachable`） | 送信後の失敗（同上） |
//! | 400・413・422 | 契約不一致（Provider 全体） | **そのジョブだけ**の恒久失敗（`input_too_large`／`input_invalid`） |
//! | 契約に合わない 2xx の本文 | 永続停止（`contract_mismatch`） | ジョブの失敗 1 回（`bad_response`） |
//!
//! 「送信後の失敗」は、Discovery が `available` を返した直後でも起きる（確認は予約ではない）。GPU の lease が始まった
//! 直後の 502・接続断は、ジョブの不良ではないので、失敗に数える前に Discovery を 1 回やり直して分類する。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::http::{AiHttp, HttpError};
use super::jobs::ErrorCode;
use super::lanes::Lane;
use super::pause::{PauseReason, PauseScope};
use super::settings::AiConfig;
use super::waits::{Wait, WaitReason, WaitScope};
use super::worker::RunError;
use crate::net::Failure;
use crate::stop::StopFlag;

/// 背景の枠が使用中のとき待つ長さ（ワーカーは 1 本なので、通常は使用中にならない）。
const BACKGROUND_SLOT_WAIT: Duration = Duration::from_secs(2);

/// LAN の入口への固定のクライアント。
#[derive(Clone, Debug)]
pub struct LanClient {
    pub(super) http: AiHttp,
    availability: Arc<Mutex<Availability>>,
}

impl LanClient {
    /// 設定の `local_base_url` から作る。設定が未指定・不正、または LAN の入口が無ければ `None`。
    pub fn new(config: &AiConfig) -> Option<Self> {
        let url = config.settings()?.local_base_url()?;
        Some(Self {
            http: AiHttp::new(url).ok()?,
            availability: Arc::default(),
        })
    }

    #[cfg(test)]
    pub(super) fn from_http(http: AiHttp) -> Self {
        Self {
            http,
            availability: Arc::default(),
        }
    }

    pub fn http(&self) -> &AiHttp {
        &self.http
    }

    /// Shared even without a writer (INGEST=0), and while a busy writer prevents persistence.
    pub(super) fn check_shared(
        &self,
        config: &AiConfig,
        task: super::settings::TaskKind,
        now: i64,
    ) -> Result<(), ProviderError> {
        let Some(settings) = config.settings() else {
            return Err(ProviderError::contract(false));
        };
        let state = self.availability.lock().unwrap();
        for (key, error) in &state.pauses {
            if Some(key) == settings.provider_pause_key(task).as_ref()
                || Some(key) == settings.task_pause_key(task).as_ref()
            {
                return Err(*error);
            }
        }
        for (key, until, wait) in &state.waits {
            if *until > now
                && settings.provider_fingerprint(task).as_ref() == Some(&key.provider)
                && (key.service.is_none() || key.service == settings.service_key(task))
            {
                return Err(ProviderError::Wait(*wait));
            }
        }
        Ok(())
    }

    pub(super) fn record_shared(
        &self,
        config: &AiConfig,
        task: super::settings::TaskKind,
        error: ProviderError,
        now: i64,
    ) {
        let Some(settings) = config.settings() else {
            return;
        };
        let mut state = self.availability.lock().unwrap();
        match error {
            ProviderError::Pause(scope, _) => {
                let key = if scope == PauseScope::Provider {
                    settings.provider_pause_key(task)
                } else {
                    settings.task_pause_key(task)
                };
                if let Some(key) = key
                    && !state.pauses.iter().any(|(k, _)| *k == key)
                {
                    state.pauses.push((key, error));
                }
            }
            ProviderError::Wait(wait) => {
                if let Some(key) = wait_key(config, task, wait) {
                    state.waits.retain(|(_, until, _)| *until > now);
                    let until = now.saturating_add(i64::from(wait.secs()));
                    if let Some(entry) = state.waits.iter_mut().find(|(k, _, _)| *k == key) {
                        if until >= entry.1 {
                            *entry = (key, until, wait);
                        }
                    } else {
                        state.waits.push((key, until, wait));
                    }
                }
            }
            _ => {}
        }
    }

    /// 背景の枠を取る。
    pub(super) fn background(
        &self,
        stop: &StopFlag,
    ) -> Result<super::http::Session, ProviderError> {
        self.http
            .session(Lane::Background, BACKGROUND_SLOT_WAIT, stop)
            .map_err(|e| ProviderError::from_http(e, Phase::Probe))
    }
}

// No derived Debug: keys carry hashed configuration identities, not useful diagnostics.
#[derive(Default)]
struct Availability {
    waits: Vec<(super::waits::WaitKey, i64, Wait)>,
    pauses: Vec<(super::pause::PauseKey, ProviderError)>,
}
impl std::fmt::Debug for Availability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Availability { .. }")
    }
}
fn wait_key(
    config: &AiConfig,
    task: super::settings::TaskKind,
    wait: Wait,
) -> Option<super::waits::WaitKey> {
    let settings = config.settings()?;
    Some(super::waits::WaitKey {
        provider: settings.provider_fingerprint(task)?,
        service: match wait.scope {
            WaitScope::Entrance => None,
            WaitScope::Service => Some(settings.service_key(task)?),
        },
    })
}
/// Record only shared availability, never queries, jobs, usage or schema.
pub(super) fn record_db(
    conn: &mut rusqlite::Connection,
    config: &AiConfig,
    task: super::settings::TaskKind,
    error: ProviderError,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(settings) = config.settings() else {
        return Ok(());
    };
    match error {
        ProviderError::Wait(wait)
            if super::jobs::object_exists(conn, "table", "ai_provider_waits")? =>
        {
            if let Some(key) = wait_key(config, task, wait) {
                super::waits::set(
                    conn,
                    &key,
                    wait.reason,
                    now.saturating_add(i64::from(wait.secs())),
                    now,
                )?;
            }
        }
        ProviderError::Pause(scope, reason)
            if super::jobs::object_exists(conn, "table", "ai_provider_pauses")? =>
        {
            let key = if scope == PauseScope::Provider {
                settings.provider_pause_key(task)
            } else {
                settings.task_pause_key(task)
            };
            if let Some(key) = key {
                super::pause::pause(conn, task, &key, reason, now)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// どの段階の失敗か（分類が変わる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Discovery・health（読み取りだけ）。
    Probe,
    /// 推論の POST（送ったあと）。
    Send,
}

/// Provider とのやり取りの失敗（固定の列挙）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    /// 一時的な待ち（失敗に数えない）。
    Wait(Wait),
    /// 永続の停止（管理者の操作か設定の変更まで）。
    Pause(PauseScope, PauseReason),
    /// そのジョブだけの恒久的な失敗。
    Permanent(ErrorCode),
    /// 契約に合わない 2xx の本文（ジョブの失敗 1 回）。
    BadResponse,
    /// 送信後の通信失敗・5xx。Discovery をやり直して分類する（[`ProviderError::into_run_error`] の前に）。
    Transient(ErrorCode),
    /// 停止の合図（送っていない）。
    Stopping,
}

impl ProviderError {
    pub fn wait(scope: WaitScope, reason: WaitReason) -> Self {
        Self::Wait(Wait::new(scope, reason))
    }

    pub fn contract(provider_wide: bool) -> Self {
        Self::Pause(
            if provider_wide {
                PauseScope::Provider
            } else {
                PauseScope::Task
            },
            PauseReason::ContractMismatch,
        )
    }

    /// HTTP の失敗を分類する（表は上）。
    pub fn from_http(error: HttpError, phase: Phase) -> Self {
        let unreachable = Self::wait(WaitScope::Entrance, WaitReason::Unreachable);
        match error {
            HttpError::Stopping => Self::Stopping,
            // 背景の枠が塞がっている（別の背景要求が残っている）: 少し待つ
            HttpError::Busy => Self::wait(WaitScope::Service, WaitReason::Busy),
            HttpError::DestinationDenied | HttpError::BadTarget | HttpError::Redirect(_) => {
                Self::contract(true)
            }
            HttpError::RequestTooLarge => Self::Permanent(ErrorCode::InputTooLarge),
            HttpError::Internal => Self::wait(WaitScope::Service, WaitReason::Busy),
            HttpError::ResponseTooLarge => match phase {
                Phase::Probe => Self::contract(true),
                Phase::Send => Self::BadResponse,
            },
            HttpError::BodyUnreadable | HttpError::Net(Failure::Other) => match phase {
                Phase::Probe => unreachable,
                Phase::Send => Self::BadResponse,
            },
            HttpError::Timeout | HttpError::Net(Failure::Timeout) => match phase {
                Phase::Probe => unreachable,
                Phase::Send => Self::Transient(ErrorCode::Timeout),
            },
            HttpError::Net(_) => match phase {
                Phase::Probe => unreachable,
                Phase::Send => Self::Transient(ErrorCode::ServerError),
            },
        }
    }

    /// 2xx 以外の状態コードを分類する（表は上）。`catalog` は、404 が Provider 全体の契約不一致になる読み取り。
    pub fn from_status(
        status: u16,
        retry_after_secs: Option<u32>,
        phase: Phase,
        catalog: bool,
    ) -> Self {
        match status {
            401 => Self::Pause(PauseScope::Provider, PauseReason::Unauthorized),
            403 => Self::Pause(PauseScope::Provider, PauseReason::Forbidden),
            404 if catalog => Self::contract(true),
            404 => Self::Pause(PauseScope::Task, PauseReason::ModelNotFound),
            429 => Self::Wait(Wait {
                scope: WaitScope::Entrance,
                reason: WaitReason::RateLimited,
                retry_after_secs,
            }),
            413 if phase == Phase::Send => Self::Permanent(ErrorCode::InputTooLarge),
            400 | 422 if phase == Phase::Send => Self::Permanent(ErrorCode::InputInvalid),
            408 if phase == Phase::Send => Self::Transient(ErrorCode::Timeout),
            503 if phase == Phase::Probe => Self::wait(WaitScope::Entrance, WaitReason::Busy),
            500..=599 => match phase {
                Phase::Probe => Self::wait(WaitScope::Entrance, WaitReason::Unreachable),
                Phase::Send => Self::Transient(ErrorCode::ServerError),
            },
            // 想定外の状態コード（3xx は HTTP 層で Redirect になる）
            _ => Self::contract(true),
        }
    }

    /// ワーカーの分類へ。`billable` は、送った時点で課金され得る Provider（クラウド。T4.6）で、送信後の通信失敗を
    /// 反復失敗でなく**失敗 1 回**として数える（送信予算と合わせて上限へ）。LAN は常に `false`。
    pub fn into_run_error(self, billable: bool) -> RunError {
        match self {
            Self::Wait(wait) => RunError::ProviderWait(wait),
            Self::Pause(PauseScope::Provider, reason) => RunError::ProviderPause(reason),
            Self::Pause(PauseScope::Task, reason) => RunError::TaskPause(reason),
            Self::Permanent(code) => RunError::Permanent(code),
            Self::BadResponse => RunError::Attempt(ErrorCode::BadResponse),
            Self::Transient(code) if billable => RunError::Attempt(code),
            Self::Transient(code) => RunError::Repeated(code),
            Self::Stopping => RunError::Stopping,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_transport_classification_follows_the_table() {
        use Phase::{Probe, Send};
        let entrance = |r| ProviderError::wait(WaitScope::Entrance, r);
        assert_eq!(
            ProviderError::from_status(401, None, Probe, false),
            ProviderError::Pause(PauseScope::Provider, PauseReason::Unauthorized)
        );
        assert_eq!(
            ProviderError::from_status(403, None, Send, false),
            ProviderError::Pause(PauseScope::Provider, PauseReason::Forbidden)
        );
        assert_eq!(
            ProviderError::from_status(404, None, Probe, true),
            ProviderError::contract(true)
        );
        assert_eq!(
            ProviderError::from_status(404, None, Probe, false),
            ProviderError::Pause(PauseScope::Task, PauseReason::ModelNotFound)
        );
        assert_eq!(
            ProviderError::from_status(429, Some(300), Send, false),
            ProviderError::Wait(Wait {
                scope: WaitScope::Entrance,
                reason: WaitReason::RateLimited,
                retry_after_secs: Some(300)
            })
        );
        assert_eq!(
            ProviderError::from_status(503, None, Probe, false),
            entrance(WaitReason::Busy)
        );
        assert_eq!(
            ProviderError::from_status(502, None, Probe, false),
            entrance(WaitReason::Unreachable)
        );
        assert_eq!(
            ProviderError::from_status(502, None, Send, false),
            ProviderError::Transient(ErrorCode::ServerError)
        );
        assert_eq!(
            ProviderError::from_status(413, None, Send, false),
            ProviderError::Permanent(ErrorCode::InputTooLarge)
        );
        assert_eq!(
            ProviderError::from_status(422, None, Send, false),
            ProviderError::Permanent(ErrorCode::InputInvalid)
        );
        // 読み取りの 4xx は契約の不一致（個々の写真の入力ではない）
        assert_eq!(
            ProviderError::from_status(422, None, Probe, false),
            ProviderError::contract(true)
        );
        assert_eq!(
            ProviderError::from_http(HttpError::Net(Failure::Connect), Probe),
            entrance(WaitReason::Unreachable)
        );
        assert_eq!(
            ProviderError::from_http(HttpError::Timeout, Send),
            ProviderError::Transient(ErrorCode::Timeout)
        );
        assert_eq!(
            ProviderError::from_http(HttpError::Redirect(302), Send),
            ProviderError::contract(true)
        );
        assert_eq!(
            ProviderError::from_http(HttpError::DestinationDenied, Probe),
            ProviderError::contract(true)
        );
        assert_eq!(
            ProviderError::from_http(HttpError::RequestTooLarge, Send),
            ProviderError::Permanent(ErrorCode::InputTooLarge)
        );
        // 送信後の失敗: LAN は反復、課金され得る Provider は失敗 1 回
        let transient = ProviderError::Transient(ErrorCode::Timeout);
        assert_eq!(
            transient.into_run_error(false),
            RunError::Repeated(ErrorCode::Timeout)
        );
        assert_eq!(
            transient.into_run_error(true),
            RunError::Attempt(ErrorCode::Timeout)
        );
    }
}
