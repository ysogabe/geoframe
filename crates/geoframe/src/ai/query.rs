//! E6 interactive runtime. No query text/vector is persisted or formatted for diagnostics.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::embedding::EmbeddingClient;
use super::http::Deadline;
use super::lanes::Lane;
use super::provider::{LanClient, ProviderError};
use super::search::QueryVector;
use super::settings::TaskKind;
use crate::config::Config;
use crate::db::Catalog;
use crate::ingest::LockHold;
use crate::pyquery;
use crate::stop::StopFlag;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    BadParams,
    Disabled,
    RateLimited,
    Unavailable,
}

// Deliberately no Debug: keys contain user input even when model identities are hashed.
#[derive(Default)]
struct Memory {
    window: VecDeque<Duration>,
    vectors: VecDeque<(String, Arc<QueryVector>)>,
}

pub struct Runtime {
    client: Option<EmbeddingClient>,
    stop: StopFlag,
    _lock: Option<LockHold>,
    memory: Mutex<Memory>,
    now: Arc<dyn Fn() -> Duration + Send + Sync>,
    #[cfg(test)]
    finished: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
impl Runtime {
    pub fn new(
        config: &Config,
        lan: Option<LanClient>,
        stop: StopFlag,
        lock: Option<LockHold>,
    ) -> Self {
        let start = Instant::now();
        Self {
            client: lan
                .clone()
                .and_then(|l| EmbeddingClient::new(&config.ai, l, TaskKind::QueryEmbedding)),
            stop,
            _lock: lock,
            memory: Mutex::new(Memory::default()),
            now: Arc::new(move || start.elapsed()),
            #[cfg(test)]
            finished: Mutex::new(None),
        }
    }

    /// Python handler parses groups before entering the function (including disabled/rate checks).
    pub async fn search(
        self: &Arc<Self>,
        config: Arc<Config>,
        catalog: Catalog,
        raw: Option<&str>,
    ) -> Result<Value, Error> {
        let whole = Deadline::interactive();
        let params = pyquery::parse_qs_first(raw.unwrap_or(""));
        let groups = params
            .get("groups")
            .map_or(Some(12), |v| pyquery::py_int(v))
            .ok_or(Error::BadParams)?
            .clamp(1, 30) as usize;
        if !config.ai.legacy().enabled {
            return Err(Error::Disabled);
        }
        // 入力の検査が先: 不正な要求は回数を消費しない。
        let query = pyquery::py_words(params.get("q").map_or("", String::as_str));
        if query.chars().count() > 300 {
            return Err(Error::BadParams);
        }
        {
            let now = (self.now)();
            let mut memory = self.memory.lock().unwrap();
            while memory
                .window
                .front()
                .is_some_and(|t| now.saturating_sub(*t) >= Duration::from_secs(60))
            {
                memory.window.pop_front();
            }
            if memory.window.len() >= config.ai.legacy().search_per_min as usize {
                return Err(Error::RateLimited);
            }
            memory.window.push_back(now);
        }
        let hidden = params
            .get("include_hidden")
            .is_some_and(|v| v == "1" || v == "true");
        let remaining = whole.remaining().ok_or(Error::Unavailable)?;
        tokio::time::timeout(
            remaining,
            self.search_until(config, catalog, query, groups, hidden, whole),
        )
        .await
        .map_err(|_| Error::Unavailable)?
    }

    async fn search_until(
        self: &Arc<Self>,
        config: Arc<Config>,
        catalog: Catalog,
        query: String,
        groups: usize,
        hidden: bool,
        whole: Deadline,
    ) -> Result<Value, Error> {
        let vector = if query.is_empty() {
            None
        } else {
            let client = self.client.as_ref().ok_or(Error::Unavailable)?.clone();
            // Only metadata, never load/copy all vectors before the network round trip.
            let expected = client.identity().clone();
            if super::embedding::configured_identity(&config.ai, TaskKind::QueryEmbedding).as_ref()
                != Some(&expected)
            {
                return Err(Error::Unavailable);
            }
            let identity = catalog
                .read(|c| Ok(super::index::active_in(c)?.1))
                .await
                .map_err(|_| Error::Unavailable)?;
            if identity.as_ref() != Some(&expected) {
                return Err(Error::Unavailable);
            }
            let key = format!(
                "{}\0query-normalization-python-v1\0{}",
                expected.fingerprint(),
                pyquery::py_casefold(&query)
            );
            // Cache lookup + nonwaiting lane acquisition are atomic against concurrent misses.
            // The lane remains owned by the blocking closure even if this HTTP future is dropped.
            let lookup = {
                let mut memory = self.memory.lock().unwrap();
                if let Some(i) = memory.vectors.iter().position(|(k, _)| k == &key) {
                    let entry = memory.vectors.remove(i).unwrap();
                    let vector = Arc::clone(&entry.1);
                    memory.vectors.push_back(entry);
                    Ok(vector)
                } else {
                    Err(client
                        .lan()
                        .http()
                        .session(Lane::Interactive, Duration::ZERO, &self.stop)
                        .map_err(|_| Error::Unavailable)?)
                }
            };
            Some(match lookup {
                Ok(vector) => vector,
                Err(session) => {
                    let rt = Arc::clone(self);
                    let catalog = catalog.clone();
                    let config = Arc::clone(&config);
                    let query = query.clone();
                    let handle = tokio::runtime::Handle::current();
                    tokio::task::spawn_blocking(move || {
                        #[cfg(test)]
                        let _finished = Finished(&rt);
                        let check = || {
                            if whole.remaining().is_none() || rt.stop.is_stopped() {
                                return Err(ProviderError::Stopping);
                            }
                            if super::embedding::configured_identity(
                                &config.ai,
                                TaskKind::QueryEmbedding,
                            )
                            .as_ref()
                                != Some(&expected)
                            {
                                return Err(ProviderError::contract(false));
                            }
                            client.lan().check_shared(
                                &config.ai,
                                TaskKind::QueryEmbedding,
                                super::jobs::unix_now(),
                            )?;
                            let ai = config.ai.clone();
                            let expected = expected.clone();
                            let remaining = whole.remaining().ok_or(ProviderError::Stopping)?;
                            let allowed = handle.block_on(async {
                                tokio::time::timeout(
                                    remaining,
                                    catalog.read(move |c| {
                                        let tx = c.unchecked_transaction()?;
                                        let settings = ai.settings().expect("validated settings");
                                        let now = super::jobs::unix_now();
                                        let allowed = super::index::active_in(&tx)?.1.as_ref()
                                            == Some(&expected)
                                            && super::pause::status(
                                                &tx,
                                                settings,
                                                TaskKind::QueryEmbedding,
                                            )?
                                            .is_none()
                                            && !super::waits::blocks(
                                                &super::waits::load(&tx, now)?,
                                                &settings
                                                    .provider_fingerprint(TaskKind::QueryEmbedding)
                                                    .unwrap(),
                                                settings
                                                    .service_key(TaskKind::QueryEmbedding)
                                                    .as_deref(),
                                                now,
                                            );
                                        tx.commit()?;
                                        Ok(allowed)
                                    }),
                                )
                                .await
                            });
                            if !matches!(allowed, Ok(Ok(true))) {
                                return Err(ProviderError::Stopping);
                            }
                            if rt.stop.is_stopped() || whole.remaining().is_none() {
                                return Err(ProviderError::Stopping);
                            }
                            client.lan().check_shared(
                                &config.ai,
                                TaskKind::QueryEmbedding,
                                super::jobs::unix_now(),
                            )?;
                            Ok(())
                        };
                        check().map_err(|_| Error::Unavailable)?;
                        let result = client.query(&session, &query, &whole, check);
                        if let Err(error) = result {
                            client.lan().record_shared(
                                &config.ai,
                                TaskKind::QueryEmbedding,
                                error,
                                super::jobs::unix_now(),
                            );
                            if let Some(writer) = catalog.writer() {
                                let ai = config.ai.clone();
                                // LAN needs no budget. Failure recording must never queue behind an unbounded writer.
                                let _ = writer.call_within(
                                    whole.wait_until(Duration::from_millis(200)),
                                    move |c| {
                                        super::provider::record_db(
                                            c,
                                            &ai,
                                            TaskKind::QueryEmbedding,
                                            error,
                                            super::jobs::unix_now(),
                                        )
                                    },
                                );
                            }
                            return Err(Error::Unavailable);
                        }
                        let vector = Arc::new(result.unwrap());
                        if whole.remaining().is_none() {
                            return Err(Error::Unavailable);
                        }
                        let mut memory = rt.memory.lock().unwrap();
                        memory.vectors.push_back((key, Arc::clone(&vector)));
                        while memory.vectors.len() > 64 {
                            memory.vectors.pop_front();
                        }
                        Ok(vector)
                    })
                    .await
                    .map_err(|_| Error::Unavailable)??
                }
            })
        };
        catalog
            .read(move |c| {
                super::search_view::search(
                    c,
                    &config,
                    &query,
                    groups,
                    hidden,
                    vector.as_deref(),
                    &whole,
                )
            })
            .await
            .map_err(|_| Error::Unavailable)?
    }
}

#[cfg(test)]
struct Finished<'a>(&'a Runtime);
#[cfg(test)]
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        if let Some(done) = self.0.finished.lock().unwrap().take() {
            let _ = done.send(());
        }
    }
}
#[cfg(test)]
mod tests;
