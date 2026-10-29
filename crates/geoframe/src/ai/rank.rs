//! Optional linear-v1 weights. No inference, paths, names or JSON in diagnostics.
use std::{
    collections::HashMap,
    fmt,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::Deserialize;

use super::{index, settings::fingerprint, space::SpaceIdentity};
use crate::quality::{Metrics, sha256_hex};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPONENTS: usize = 256;
const MAX_CACHED_PATHS: usize = 8;

/// 変更の判定: (mtime 秒, ナノ秒, 大きさ, dev, ino)。
type Stamp = (i64, i64, u64, u64, u64);
struct Cached {
    stamp: Stamp,
    space: String,
    digest: Option<String>,
}

/// 重みファイルの digest の記憶。本番はプロセスで 1 つ（[`cached_digest`]）。件数に上限（[`MAX_CACHED_PATHS`]）が
/// あるので、試験は自分の記憶を作り、ほかの試験が入れたパスの影響を受けないようにする。
pub(super) struct DigestCache {
    map: Mutex<Option<HashMap<PathBuf, Cached>>>,
    #[cfg(test)]
    loads: std::sync::atomic::AtomicUsize,
}
static DIGESTS: DigestCache = DigestCache::new();

impl DigestCache {
    pub(super) const fn new() -> Self {
        Self {
            map: Mutex::new(None),
            #[cfg(test)]
            loads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// ファイルを読んで解析し直した回数。
    #[cfg(test)]
    pub(super) fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 重みファイルの digest（読めない・不正なら `None`）。ファイルの mtime・大きさ・dev・ino と空間が
    /// 同じ間は前回の結果を返し、変わったときだけ読み直す。
    pub(super) fn digest(&self, path: &Path, identity: &SpaceIdentity) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let stamp = (
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len(),
            meta.dev(),
            meta.ino(),
        );
        let space = identity.fingerprint();
        if let Some(hit) = self
            .map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(path))
            .filter(|c| c.stamp == stamp && c.space == space)
        {
            return hit.digest.clone();
        }
        #[cfg(test)]
        self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let digest = Model::load(path, identity)
            .ok()
            .map(|m| m.digest().to_owned());
        let mut cache = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let map = cache.get_or_insert_with(HashMap::new);
        if map.len() >= MAX_CACHED_PATHS && !map.contains_key(path) {
            map.clear();
        }
        map.insert(
            path.to_path_buf(),
            Cached {
                stamp,
                space,
                digest: digest.clone(),
            },
        );
        digest
    }
}

/// 読み取り経路（状態・検索・管理一覧）が使う、重みファイルの digest（読めない・不正なら `None`）。
/// 呼ぶたびに最大 8 MiB を読んで解析し直さないよう、[`DigestCache`] で変わったときだけ読み直す。
pub fn cached_digest(path: &Path, identity: &SpaceIdentity) -> Option<String> {
    DIGESTS.digest(path, identity)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reason {
    Unconfigured,
    Unreadable,
    NotRegular,
    TooLarge,
    Invalid,
    Shape,
    Feature,
    NonFinite,
    UnverifiedSpace,
    IncompatibleSpace,
    MissingFeature,
    Overflow,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Feature {
    Sharpness,
    Luminance,
    ExposureScore,
    BlackClip,
    WhiteClip,
    Contrast,
}
impl Feature {
    fn value(self, m: &Metrics) -> f64 {
        match self {
            Self::Sharpness => m.sharpness,
            Self::Luminance => m.luminance,
            Self::ExposureScore => m.exposure_score,
            Self::BlackClip => m.black_clip,
            Self::WhiteClip => m.white_clip,
            Self::Contrast => m.contrast,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Sharpness => "sharpness",
            Self::Luminance => "luminance",
            Self::ExposureScore => "exposure_score",
            Self::BlackClip => "black_clip",
            Self::WhiteClip => "white_clip",
            Self::Contrast => "contrast",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Compatibility {
    space_fingerprint: String,
    embedding_processor_version: String,
    /// Linear-v1 scores the stored f32 vector, before cosine unit conversion.
    input_representation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    format: String,
    name: String,
    embedding_model: String,
    embedding_dim: usize,
    base_features: Vec<String>,
    base_weights: Vec<f64>,
    pca_mean: Vec<f64>,
    pca_components: Vec<Vec<f64>>,
    pca_weights: Vec<f64>,
    compatibility: Option<Compatibility>,
}

pub struct Model {
    identity: SpaceIdentity,
    features: Vec<Feature>,
    weights: Vec<f64>,
    u: Vec<f64>,
    c: f64,
    digest: String,
    source_digest: String,
}
impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RankModel { .. }")
    }
}
impl Model {
    /// Exact bytes inspected, even if the path was replaced during/after reading.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    /// Effective coefficients + full space, independent of file name/path/display name.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn parse(bytes: &[u8], identity: &SpaceIdentity) -> Result<Self, Reason> {
        if bytes.len() > MAX_BYTES {
            return Err(Reason::TooLarge);
        }
        check_structure(bytes)?;
        let raw: Raw = serde_json::from_slice(bytes).map_err(|_| Reason::Invalid)?;
        if raw.format != "geoframe-rank-linear-v1"
            || raw.name.len() > 256
            || raw.embedding_model.len() > 256
        {
            return Err(Reason::Invalid);
        }
        let dim = raw.embedding_dim;
        if dim == 0
            || dim > 4096
            || raw.base_features.len() > 6
            || raw.base_features.len() != raw.base_weights.len()
            || raw.pca_mean.len() != dim
            || raw.pca_weights.len() != raw.pca_components.len()
            || raw.pca_components.len() > MAX_COMPONENTS
            || raw.pca_components.iter().any(|r| r.len() != dim)
        {
            return Err(Reason::Shape);
        }
        let mut features = Vec::new();
        for name in &raw.base_features {
            if features.iter().any(|f: &Feature| f.name() == name) {
                return Err(Reason::Feature);
            }
            let feature: Feature = serde_json::from_value(serde_json::Value::String(name.clone()))
                .map_err(|_| Reason::Feature)?;
            features.push(feature);
        }
        if raw
            .base_weights
            .iter()
            .chain(&raw.pca_mean)
            .chain(&raw.pca_weights)
            .chain(raw.pca_components.iter().flatten())
            .any(|n| !n.is_finite())
        {
            return Err(Reason::NonFinite);
        }
        let compat = raw.compatibility.ok_or(Reason::UnverifiedSpace)?;
        if raw.embedding_model != identity.model_id()
            || dim != identity.dimension() as usize
            || compat.space_fingerprint != identity.fingerprint()
            || compat.embedding_processor_version
                != index::image_embedding_processor_version(identity)
            || compat.input_representation != "stored-f32-v1"
        {
            return Err(Reason::IncompatibleSpace);
        }
        let u: Vec<f64> = (0..dim)
            .map(|i| {
                raw.pca_weights
                    .iter()
                    .zip(&raw.pca_components)
                    .map(|(w, r)| w * r[i])
                    .sum()
            })
            .collect();
        let c = -raw.pca_mean.iter().zip(&u).map(|(m, u)| m * u).sum::<f64>();
        if !c.is_finite() || u.iter().any(|n| !n.is_finite()) {
            return Err(Reason::Overflow);
        }
        let mut parts = vec![
            "rank-folded-v1".to_string(),
            identity.fingerprint(),
            c.to_bits().to_string(),
        ];
        for (f, w) in features.iter().zip(&raw.base_weights) {
            parts.extend([f.name().into(), w.to_bits().to_string()]);
        }
        parts.extend(u.iter().map(|v| v.to_bits().to_string()));
        Ok(Self {
            identity: identity.clone(),
            features,
            weights: raw.base_weights,
            u,
            c,
            digest: fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>()),
            source_digest: sha256_hex(bytes),
        })
    }

    pub fn load(path: &Path, identity: &SpaceIdentity) -> Result<Self, Reason> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| Reason::Unreadable)?;
        let meta = file.metadata().map_err(|_| Reason::Unreadable)?;
        if !meta.is_file() {
            return Err(Reason::NotRegular);
        }
        if meta.len() > MAX_BYTES as u64 {
            return Err(Reason::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Reason::Unreadable)?;
        Self::parse(&bytes, identity)
    }

    pub fn scores(
        &self,
        identity: &SpaceIdentity,
        photos: &[(Metrics, &[f32])],
    ) -> Result<Vec<f64>, Reason> {
        if identity != &self.identity {
            return Err(Reason::IncompatibleSpace);
        }
        let mut scores = Vec::with_capacity(photos.len());
        for (metrics, vector) in photos {
            if vector.len() != self.u.len() {
                return Err(Reason::Shape);
            }
            let base = self
                .features
                .iter()
                .zip(&self.weights)
                .try_fold(0.0, |acc, (f, w)| {
                    let value = f.value(metrics);
                    if !value.is_finite() {
                        Err(Reason::MissingFeature)
                    } else {
                        Ok(acc + value * w)
                    }
                })?;
            let score = base
                + vector
                    .iter()
                    .zip(&self.u)
                    .map(|(e, u)| f64::from(*e) * u)
                    .sum::<f64>()
                + self.c;
            if !score.is_finite() {
                return Err(Reason::Overflow);
            }
            scores.push(score);
        }
        if scores.is_empty() {
            return Ok(scores);
        }
        let lo = scores.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let span = hi - lo;
        if !span.is_finite() {
            return Err(Reason::Overflow);
        }
        for score in &mut scores {
            *score = if hi > lo {
                // Divide before multiplying: finite raw values may still overflow 100*x.
                let value = 100.0 * ((*score - lo) / span);
                format!("{value:.2}")
                    .parse()
                    .map_err(|_| Reason::Overflow)?
            } else {
                50.0
            };
            if !score.is_finite() {
                return Err(Reason::Overflow);
            }
        }
        Ok(scores)
    }
}

/// Bound nesting and each array before serde allocates. Quotes/escapes are respected.
fn check_structure(bytes: &[u8]) -> Result<(), Reason> {
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let (mut quoted, mut escaped) = (false, false);
    for &b in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
            continue;
        }
        match b {
            b'"' => quoted = true,
            b'[' | b'{' => {
                stack.push((b, 0));
                if stack.len() > 8 {
                    return Err(Reason::Invalid);
                }
            }
            b']' | b'}' => {
                stack.pop();
            }
            b',' => {
                if let Some((b'[', count)) = stack.last_mut() {
                    *count += 1;
                    if *count >= 4096 {
                        return Err(Reason::Shape);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
