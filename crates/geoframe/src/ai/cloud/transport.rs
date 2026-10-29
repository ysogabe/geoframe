//! 外へ公開しない POST。固定済みホスト、公開 IP の pin、同じ停止境界と本文上限を使う。
#[cfg(test)]
use super::super::dest::Lookup;
use super::super::{
    dest::{PinResolver, SystemLookup},
    gate::{self, GateConnector},
    http::{self, Deadline, HttpError, MAX_REQUEST_BYTES, Reply},
};
use crate::stop::StopFlag;
use std::sync::Arc;
use ureq::unversioned::transport::{Connector, DefaultConnector};

pub(super) struct Transport {
    agent: ureq::Agent,
    base: String,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SendError {
    pub error: HttpError,
    pub started: bool,
}

impl Transport {
    pub fn new(host: &str) -> Self {
        Self {
            agent: http::build_agent(
                DefaultConnector::new().chain(GateConnector),
                PinResolver::public(host, Arc::new(SystemLookup)),
            ),
            base: format!("https://{host}"),
        }
    }

    #[cfg(test)]
    pub fn stub(host: &str, port: u16, lookup: Arc<dyn Lookup>, connector: impl Connector) -> Self {
        Self {
            agent: http::build_agent(connector, PinResolver::stub(host, port, lookup)),
            base: format!("http://{host}:{port}"),
        }
    }

    #[cfg(test)]
    pub fn tls_stub(host: &str, port: u16, lookup: Arc<dyn Lookup>) -> Self {
        Self {
            agent: http::build_agent(
                DefaultConnector::new().chain(GateConnector),
                PinResolver::stub(host, port, lookup),
            ),
            base: format!("https://{host}:{port}"),
        }
    }

    #[cfg(test)]
    pub fn public_with(host: &str, lookup: Arc<dyn Lookup>, connector: impl Connector) -> Self {
        Self {
            agent: http::build_agent(connector, PinResolver::public(host, lookup)),
            base: format!("https://{host}"),
        }
    }

    // 呼び出しは cloud 内だけ。path は Target または固定 /token のみ。
    pub fn post(
        &self,
        path: &str,
        body: &[u8],
        bearer: Option<&str>,
        deadline: &Deadline,
        stop: &StopFlag,
    ) -> Result<Reply, SendError> {
        let unsent = |error| SendError {
            error,
            started: false,
        };
        if body.len() > MAX_REQUEST_BYTES {
            return Err(unsent(HttpError::RequestTooLarge));
        }
        if stop.is_stopped() {
            return Err(unsent(HttpError::Stopping));
        }
        let left = deadline.remaining().ok_or(unsent(HttpError::Timeout))?;
        let mut req = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Accept", "application/json")
            .header(
                "Content-Type",
                if bearer.is_some() {
                    "application/json"
                } else {
                    "application/x-www-form-urlencoded"
                },
            );
        if let Some(token) = bearer {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let armed = gate::arm(stop);
        let result = req.config().timeout_global(Some(left)).build().send(body);
        let started = armed.started();
        drop(armed);
        let response = result.map_err(|e| SendError {
            error: http::classify(&e),
            started,
        })?;
        http::read_reply(response).map_err(|error| SendError {
            error,
            started: true,
        })
    }
}
