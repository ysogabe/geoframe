//! Pinned local-model-lab text/image contract. No inferred revisions or arbitrary endpoints.
//! T4.4b calls `query` inside the shared LanClient's `run_interactive` closure.
use serde_json::{Value, json};

use super::discovery::{self, Expect};
use super::http::{DISCOVERY_TIMEOUT, Deadline, Endpoint, Session};
use super::image_input::PreparedImage;
use super::jobs::ErrorCode;
use super::provider::{LanClient, Phase, ProviderError};
use super::search::QueryVector;
use super::settings::{AiConfig, Normalization, ProviderKind, TaskKind};
use super::space::SpaceIdentity;
use super::worker::{RunCtx, RunError};

/// Explicit attestation to upstream c9b05b590d838b81a6b2f939af34e409f16469ad.
pub const CONTRACT: &str = "local-model-lab-eg2-bf16-c9b05b59-v1";
pub const MODEL: &str = "embeddinggemma-2-bf16";
pub const REVISION: &str = "914f7f89142e33e77833254d9c9b90c3cef7303b";
pub const PROVIDER_SPACE: &str = "eg2-bf16-c9b05b59-914f7f89-v1";
/// Includes checkpoint image processor, upstream EXIF/RGB and client preparation.
pub const IMAGE_PREPROCESS: &str = "eg2-pin-exif-rgb-srgb-768-jpeg-q82-v1";
pub const TEXT_PREPROCESS: &str = "eg2-raw-search-query-prefix-v1";
pub const QUERY_PREFIX: &str = "task: search result | query: ";
pub const DIMENSION: u32 = 768;
pub const MAX_REQUEST_BYTES: usize = 4 << 20;
pub const MAX_INPUTS: usize = 8;
pub const SERVICE_ID: &str = "embed";
const APIS: &[(&str, &str)] = &[("GET", "/v1/models"), ("POST", "/v1/embeddings")];

/// Must opt in to this known contract AND exactly declare its revisions/processing.
/// Unknown metadata never becomes trusted merely because model/dimension match.
pub fn configured_identity(config: &AiConfig, task: TaskKind) -> Option<SpaceIdentity> {
    if !matches!(task, TaskKind::ImageEmbedding | TaskKind::QueryEmbedding) {
        return None;
    }
    let gate = config.gate(task, task != TaskKind::QueryEmbedding).ok()?;
    let settings = config.settings()?;
    let declared = settings.embedding_space()?;
    if settings.embedding_contract()? != CONTRACT
        || gate.settings.provider != ProviderKind::Local
        || gate.settings.model != MODEL
        || gate.settings.service_id.as_deref() != Some(SERVICE_ID)
        || declared.model_id != MODEL
        || declared.revision != REVISION
        || declared.image_preprocess != IMAGE_PREPROCESS
        || declared.text_preprocess != TEXT_PREPROCESS
        || declared.dimension != DIMENSION
        || declared.normalization != Normalization::L2
    {
        return None;
    }
    SpaceIdentity::from_declared(declared, PROVIDER_SPACE).ok()
}

/// One shared agent/lanes, plus fully matched identity. Debug never prints input/config.
#[derive(Clone, Debug)]
pub struct EmbeddingClient {
    lan: LanClient,
    identity: SpaceIdentity,
    task: TaskKind,
}
impl EmbeddingClient {
    pub fn new(config: &AiConfig, lan: LanClient, task: TaskKind) -> Option<Self> {
        Some(Self {
            lan,
            identity: configured_identity(config, task)?,
            task,
        })
    }
    pub fn lan(&self) -> &LanClient {
        &self.lan
    }
    pub fn identity(&self) -> &SpaceIdentity {
        &self.identity
    }

    /// Read-only discovery; never spends inference budget. Caller supplies whole deadline.
    pub fn probe(&self, session: &Session, whole: &Deadline) -> Result<(), ProviderError> {
        let deadline = whole.capped(DISCOVERY_TIMEOUT);
        let service = super::http::ServiceId::parse(SERVICE_ID).expect("constant service");
        discovery::check(
            session,
            &Expect {
                service: &service,
                apis: APIS,
            },
            &deadline,
        )?;
        let reply = session
            .get(Endpoint::EmbeddingModels, &deadline)
            .map_err(|e| ProviderError::from_http(e, Phase::Probe))?;
        if !reply.is_success() {
            return Err(ProviderError::from_status(
                reply.status,
                reply.retry_after_secs,
                Phase::Probe,
                false,
            ));
        }
        check_models(&reply.body)
    }

    fn send(
        &self,
        session: &Session,
        body: &[u8],
        whole: &Deadline,
    ) -> Result<Vec<f32>, ProviderError> {
        let result = (|| {
            let reply = session
                .post_json(Endpoint::Embeddings, body, whole)
                .map_err(|e| ProviderError::from_http(e, Phase::Send))?;
            if !reply.is_success() {
                return Err(ProviderError::from_status(
                    reply.status,
                    reply.retry_after_secs,
                    Phase::Send,
                    false,
                ));
            }
            parse_reply(&reply.body, &self.identity, 1).map(|mut v| v.remove(0))
        })();
        match result {
            Err(ProviderError::Transient(code)) => {
                self.probe(session, whole)?;
                Err(ProviderError::Transient(code))
            }
            other => other,
        }
    }

    /// Blocking API, only within shared `lan.http().run_interactive` (or dedicated thread).
    /// Caller checks policy/pause/waits before entry AND in before_send (after discovery).
    /// No HTTP route, LRU or cloud fallback is installed here. Whole deadline includes probe.
    pub fn query(
        &self,
        session: &Session,
        query: &str,
        whole: &Deadline,
        before_send: impl FnOnce() -> Result<(), ProviderError>,
    ) -> Result<QueryVector, ProviderError> {
        if self.task != TaskKind::QueryEmbedding
            || query.trim().is_empty()
            || query.chars().count() > 300
        {
            return Err(ProviderError::Permanent(ErrorCode::InputInvalid));
        }
        let body = encode_inputs(vec![json!(format!("{QUERY_PREFIX}{query}"))])?;
        self.probe(session, whole)?;
        before_send()?;
        let values = self.send(session, &body, whole)?;
        QueryVector::new(&self.identity, &values).map_err(|_| ProviderError::BadResponse)
    }

    pub(super) fn image(
        &self,
        ctx: &RunCtx<'_>,
        image: &PreparedImage,
        before_send: impl FnOnce() -> Result<(), RunError>,
    ) -> Result<Vec<f32>, RunError> {
        if self.task != TaskKind::ImageEmbedding
            || configured_identity(ctx.config(), TaskKind::ImageEmbedding).as_ref()
                != Some(&self.identity)
        {
            return Err(RunError::PolicyDisabled);
        }
        let body = encode_inputs(vec![json!({"image": image.data_url()})])
            .map_err(|e| e.into_run_error(false))?;
        let whole = Deadline::for_task(TaskKind::ImageEmbedding);
        ctx.before_probe()?;
        let session = self
            .lan
            .background(ctx.stop())
            .map_err(|e| e.into_run_error(false))?;
        self.probe(&session, &whole)
            .map_err(|e| e.into_run_error(false))?;
        before_send()?;
        // Final check after disk reread and discovery, immediately before POST.
        ctx.before_send()?;
        self.send(&session, &body, &whole)
            .map_err(|e| e.into_run_error(false))
    }
}

fn encode_inputs(inputs: Vec<Value>) -> Result<Vec<u8>, ProviderError> {
    if inputs.is_empty() || inputs.len() > MAX_INPUTS {
        return Err(ProviderError::Permanent(ErrorCode::InputInvalid));
    }
    let body =
        serde_json::to_vec(&json!({"model": MODEL, "input": inputs, "encoding_format": "float"}))
            .map_err(|_| ProviderError::BadResponse)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(ProviderError::Permanent(ErrorCode::InputTooLarge));
    }
    Ok(body)
}

fn check_models(body: &[u8]) -> Result<(), ProviderError> {
    let bad = ProviderError::contract(false);
    let doc: Value = serde_json::from_slice(body).map_err(|_| bad)?;
    if doc["object"].as_str() != Some("list") {
        return Err(bad);
    }
    let models = doc["data"].as_array().ok_or(bad)?;
    // Ambiguous duplicate model entries are not evidence of the contract.
    let matches: Vec<_> = models
        .iter()
        .filter(|m| m["id"].as_str() == Some(MODEL))
        .collect();
    if matches.len() != 1 {
        return Err(bad);
    }
    let m = matches[0];
    let modalities = m["modalities"].as_array().ok_or(bad)?;
    if m["dimensions"].as_u64() != Some(u64::from(DIMENSION))
        || !modalities.iter().all(Value::is_string)
        || !["text", "image"]
            .iter()
            .all(|s| modalities.iter().any(|v| v.as_str() == Some(s)))
    {
        return Err(bad);
    }
    Ok(())
}

/// Array order is irrelevant: require every unique index in 0..expected exactly once.
fn parse_reply(
    body: &[u8],
    identity: &SpaceIdentity,
    expected: usize,
) -> Result<Vec<Vec<f32>>, ProviderError> {
    let doc: Value = serde_json::from_slice(body).map_err(|_| ProviderError::BadResponse)?;
    if doc["model"].as_str() != Some(MODEL) {
        return Err(ProviderError::contract(false));
    }
    let bad = ProviderError::BadResponse;
    if doc["object"].as_str() != Some("list") || !doc["usage"].is_object() {
        return Err(bad);
    }
    let data = doc["data"].as_array().ok_or(bad)?;
    if data.len() != expected || expected == 0 || expected > MAX_INPUTS {
        return Err(bad);
    }
    let mut out = vec![None; expected];
    for row in data {
        let index = row["index"]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < expected)
            .ok_or(bad)?;
        if out[index].is_some() {
            return Err(bad);
        }
        let values = row["embedding"]
            .as_array()
            .ok_or(bad)?
            .iter()
            .map(|v| v.as_f64().map(|n| n as f32))
            .collect::<Option<Vec<_>>>()
            .ok_or(bad)?;
        out[index] = Some(super::space::prepare(identity, &values).map_err(|_| bad)?);
    }
    out.into_iter().map(|v| v.ok_or(bad)).collect()
}

#[cfg(test)]
pub(crate) mod tests;
