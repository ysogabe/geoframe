//! Scene components and local ranking. Snapshot I/O, pure computation and atomic repository
//! publication are separate steps; no feed suppression or worker registration here.
use std::{collections::BTreeMap, fmt, sync::Arc};

use super::{rank, settings::fingerprint, space::SpaceIdentity};
use crate::quality::{Metrics, rank_scores};

mod repository;
pub use repository::{
    Annotation, Loaded, Saved, load_current, load_current_in, migrate, pending, pending_saves,
    save, save_batch, snapshot,
};
pub mod runtime;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidOptions,
    Database,
    CorruptResult,
    /// The bounded wait for the single writer expired; nothing was saved by this cycle.
    WriterBusy,
    /// A scene table exists with columns this code does not write. It is neither read nor repaired.
    SchemaMismatch,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scene_{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    window_s: i64,
    similarity: f64,
    shortlist: usize,
}
impl Options {
    pub fn new(window_s: i64, similarity: f64, shortlist: usize) -> Result<Self, Error> {
        if !(1..=86400).contains(&window_s)
            || !similarity.is_finite()
            || !(-1.0..=1.0).contains(&similarity)
            || !(2..=6).contains(&shortlist)
        {
            return Err(Error::InvalidOptions);
        }
        Ok(Self {
            window_s,
            similarity: similarity + 0.0,
            shortlist,
        })
    }
    pub fn processor_version(
        &self,
        identity: &SpaceIdentity,
        weight_digest: Option<&str>,
    ) -> String {
        fingerprint(&[
            "scene-components-v1",
            &identity.fingerprint(),
            &self.window_s.to_string(),
            &self.similarity.to_bits().to_string(),
            &self.shortlist.to_string(),
            "quality-rank-v1:linear-v1",
            weight_digest.unwrap_or("rules"),
        ])
    }
    /// Processor of a component that a present weight could not score (fixed-rule fallback). The
    /// weight digest is part of the version, so a changed weight never accepts the old fallback.
    pub fn fallback_processor_version(&self, identity: &SpaceIdentity, digest: &str) -> String {
        self.processor_version(identity, Some(&format!("fallback:{digest}")))
    }
    /// The processor a stored component must carry for the ranking it used.
    pub(crate) fn group_processor_version(
        &self,
        identity: &SpaceIdentity,
        rank_used: RankUsed,
        weight_digest: Option<&str>,
    ) -> String {
        match (rank_used, weight_digest) {
            (RankUsed::Fallback(_), Some(digest)) => {
                self.fallback_processor_version(identity, digest)
            }
            _ => self.processor_version(identity, weight_digest),
        }
    }
}
impl Default for Options {
    fn default() -> Self {
        Self {
            window_s: 900,
            similarity: 0.83,
            shortlist: 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    PendingEmbedding,
    CorruptVector,
    PendingQuality,
    FailedEmbedding,
    FailedQuality,
    NoDisplay,
    MissingTime,
    InvalidTime,
}
/// Current primary input values are retained without logging them.
#[derive(Clone, PartialEq)]
pub struct Input {
    pub photo_id: i64,
    pub display: rusqlite::types::Value,
    pub md5: rusqlite::types::Value,
    pub mtime: rusqlite::types::Value,
    pub rotation: rusqlite::types::Value,
    pub taken_at: rusqlite::types::Value,
    pub quality_sha: Option<String>,
}
impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SceneInput { .. }")
    }
}
#[derive(Clone)]
pub struct Photo {
    input: Input,
    id: i64,
    captured: Option<i64>,
    readiness: Readiness,
    generation: String,
    metrics: Option<Metrics>,
    vector: Option<Arc<Vec<f32>>>,
    norm: f64,
    shown: bool,
    favorite: bool,
}
impl fmt::Debug for Photo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScenePhoto { .. }")
    }
}
impl Photo {
    pub fn input(&self) -> &Input {
        &self.input
    }
    pub fn metrics(&self) -> Option<Metrics> {
        self.metrics
    }
    pub fn id(&self) -> i64 {
        self.id
    }
    pub fn readiness(&self) -> Readiness {
        self.readiness
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn shown(&self) -> bool {
        self.shown
    }
    pub fn favorite(&self) -> bool {
        self.favorite
    }
}

pub struct Snapshot {
    identity: SpaceIdentity,
    photos: Vec<Photo>,
    raw: repository::Raw,
}
impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SceneSnapshot")
            .field("photos", &self.photos.len())
            .finish_non_exhaustive()
    }
}
impl Snapshot {
    pub fn identity(&self) -> &SpaceIdentity {
        &self.identity
    }
    pub fn photos(&self) -> &[Photo] {
        &self.photos
    }
}

/// A provisional component never constitutes permission to suppress a photograph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Waiting,
    LocalRanked,
    ReviewOverlap,
}
impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::LocalRanked => "local_ranked",
            Self::ReviewOverlap => "review_overlap",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlap {
    Disjoint,
    Exact,
    Partial,
}
impl Overlap {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disjoint => "disjoint",
            Self::Exact => "exact",
            Self::Partial => "partial",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RankUsed {
    /// No usable weight at all (fixed rules for every component).
    Rules(Option<rank::Reason>),
    /// A verified weight scored this component.
    Linear,
    /// A verified weight exists but could not score this component (fixed-rule fallback). The
    /// weight digest is recorded, so reads accept it only while that same weight is installed.
    Fallback(rank::Reason),
}

#[derive(Clone, PartialEq)]
pub struct Member {
    pub photo_id: i64,
    pub generation: String,
    pub rank: usize,
    pub score: Option<f64>,
    pub shortlisted: bool,
    /// Proposed rule representative; b must still check current visibility and State.
    pub representative: bool,
    pub shown: bool,
    pub favorite: bool,
}
impl fmt::Debug for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SceneMember { .. }")
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LegacyOrigin {
    group_id: String,
    digest: String,
    judge_model: String,
    judge_version: String,
}
impl fmt::Debug for LegacyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LegacyOrigin { .. }")
    }
}
impl LegacyOrigin {
    pub fn group_id(&self) -> &str {
        &self.group_id
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn judge_model(&self) -> &str {
        &self.judge_model
    }
    pub fn judge_version(&self) -> &str {
        &self.judge_version
    }
    /// Old rows lack full prompt/rotation lineage. This is overlap provenance, not paid-result authorization.
    pub fn paid_reuse_verified(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct Group {
    result_id: String,
    input_generation: String,
    processor_version: String,
    state: State,
    overlap: Overlap,
    rank_used: RankUsed,
    members: Vec<Member>,
    legacy: Vec<LegacyOrigin>,
    lo: i64,
    hi: i64,
    guard: String,
    /// Digest of the vectors the result was computed from. Checked by the writer's CAS only;
    /// read paths never load vector BLOBs, so a changed vector is replanned by the next cycle.
    vector_guard: String,
    weight_digest: Option<String>,
    expected_active: String,
}
impl fmt::Debug for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SceneGroup")
            .field("state", &self.state)
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}
impl Group {
    pub fn result_id(&self) -> &str {
        &self.result_id
    }
    pub fn input_generation(&self) -> &str {
        &self.input_generation
    }
    pub fn processor_version(&self) -> &str {
        &self.processor_version
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn overlap(&self) -> Overlap {
        self.overlap
    }
    pub fn rank_used(&self) -> RankUsed {
        self.rank_used
    }
    pub fn members(&self) -> &[Member] {
        &self.members
    }
    pub fn legacy_origins(&self) -> &[LegacyOrigin] {
        &self.legacy
    }
    pub fn shortlist_ids(&self) -> Vec<i64> {
        self.members
            .iter()
            .filter(|m| m.shortlisted)
            .map(|m| m.photo_id)
            .collect()
    }
    pub fn has_shown_representative(&self) -> bool {
        self.members.iter().any(|m| m.representative && m.shown)
    }
}

pub struct Plan {
    identity: SpaceIdentity,
    options: Options,
    groups: Vec<Group>,
}
impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScenePlan")
            .field("groups", &self.groups.len())
            .finish_non_exhaustive()
    }
}
impl Plan {
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
    pub fn identity(&self) -> &SpaceIdentity {
        &self.identity
    }
}

/// No DB, file, writer or network access. Cancellation discards the entire unfinished plan.
/// Pending windows are retained as Waiting, including singleton components.
pub fn compute(
    snapshot: &Snapshot,
    options: Options,
    model: Result<&rank::Model, rank::Reason>,
    stopped: &dyn Fn() -> bool,
) -> Option<Plan> {
    let mut timed: Vec<usize> = snapshot
        .photos
        .iter()
        .enumerate()
        .filter(|(_, p)| p.readiness == Readiness::Ready)
        .map(|(i, _)| i)
        .collect();
    timed.sort_by_key(|&i| (snapshot.photos[i].captured, snapshot.photos[i].id));
    let mut union = Union::new(timed.len());
    let mut left = 0;
    for right in 0..timed.len() {
        if stopped() {
            return None;
        }
        let current = &snapshot.photos[timed[right]];
        while left < right
            && current.captured? - snapshot.photos[timed[left]].captured? > options.window_s
        {
            left += 1;
        }
        for (previous, &previous_photo) in timed.iter().enumerate().take(right).skip(left) {
            if previous % 128 == 0 && stopped() {
                return None;
            }
            // Same-component edges do not affect connectivity. Avoid pathological identical-vector N²*d work.
            if union.find(right) == union.find(previous) {
                continue;
            }
            let a = current.vector.as_ref()?;
            let prior = &snapshot.photos[previous_photo];
            let b = prior.vector.as_ref()?;
            let similarity: f64 = a
                .iter()
                .zip(b.iter())
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>()
                / (current.norm * prior.norm);
            if similarity >= options.similarity {
                union.join(right, previous);
            }
        }
    }
    let mut buckets: BTreeMap<usize, Vec<&Photo>> = BTreeMap::new();
    for (index, &i) in timed.iter().enumerate() {
        buckets
            .entry(union.find(index))
            .or_default()
            .push(&snapshot.photos[i]);
    }
    let pending: Vec<i64> = snapshot
        .photos
        .iter()
        .filter(|p| {
            matches!(
                p.readiness,
                Readiness::PendingEmbedding | Readiness::CorruptVector | Readiness::PendingQuality
            )
        })
        .filter_map(|p| p.captured)
        .collect();
    let mut groups = Vec::new();
    for mut bucket in buckets.into_values() {
        if stopped() {
            return None;
        }
        bucket.sort_by_key(|p| p.id);
        let lo = bucket
            .iter()
            .filter_map(|p| p.captured)
            .min()?
            .saturating_sub(options.window_s);
        let hi = bucket
            .iter()
            .filter_map(|p| p.captured)
            .max()?
            .saturating_add(options.window_s);
        let waiting = pending.iter().any(|t| lo <= *t && *t <= hi);
        let metrics: Vec<_> = bucket.iter().filter_map(|p| p.metrics).collect();
        let (scores, rank_used, weight_digest) = match model {
            Ok(model) => {
                let inputs: Vec<_> = bucket
                    .iter()
                    .map(|p| (p.metrics.unwrap(), p.vector.as_ref().unwrap().as_slice()))
                    .collect();
                match model.scores(&snapshot.identity, &inputs) {
                    Ok(scores) => (scores, RankUsed::Linear, Some(model.digest().to_string())),
                    Err(reason) => (
                        rank_scores(&metrics),
                        RankUsed::Fallback(reason),
                        Some(model.digest().to_string()),
                    ),
                }
            }
            Err(reason) => (rank_scores(&metrics), RankUsed::Rules(Some(reason)), None),
        };
        let processor_version = options.group_processor_version(
            &snapshot.identity,
            rank_used,
            weight_digest.as_deref(),
        );
        let input_generation = fingerprint(
            &bucket
                .iter()
                .map(|p| p.generation.as_str())
                .collect::<Vec<_>>(),
        );
        let result_id = fingerprint(&["scene-result-v1", &processor_version, &input_generation]);
        let ids: Vec<_> = bucket.iter().map(|p| p.id).collect();
        let (overlap, legacy) = snapshot.raw.overlap(&ids);
        let state = if waiting {
            State::Waiting
        } else if overlap == Overlap::Partial {
            State::ReviewOverlap
        } else {
            State::LocalRanked
        };
        let mut order: Vec<usize> = (0..bucket.len()).collect();
        order.sort_by(|&a, &b| {
            scores[b]
                .total_cmp(&scores[a])
                .then(bucket[a].id.cmp(&bucket[b].id))
        });
        let members = order
            .iter()
            .enumerate()
            .map(|(rank, &i)| Member {
                photo_id: bucket[i].id,
                generation: bucket[i].generation.clone(),
                rank: rank + 1,
                score: (!waiting).then_some(scores[i]),
                shortlisted: !waiting && rank < options.shortlist,
                representative: !waiting && rank == 0,
                shown: bucket[i].shown,
                favorite: bucket[i].favorite,
            })
            .collect();
        groups.push(Group {
            result_id,
            input_generation,
            processor_version,
            state,
            overlap,
            rank_used,
            members,
            legacy,
            lo,
            hi,
            guard: snapshot.raw.guard(lo, hi),
            vector_guard: snapshot.raw.vector_guard(lo, hi),
            expected_active: snapshot.raw.pointer_guard(&ids),
            weight_digest,
        });
    }
    groups.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(a.result_id.cmp(&b.result_id))
    });
    Some(Plan {
        identity: snapshot.identity.clone(),
        options,
        groups,
    })
}

struct Union {
    parent: Vec<usize>,
    rank: Vec<u8>,
}
impl Union {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }
    fn join(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        if self.rank[a] < self.rank[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b] = a;
        if self.rank[a] == self.rank[b] {
            self.rank[a] += 1;
        }
    }
}

fn captured_text(text: &str) -> Option<i64> {
    crate::geo::naive_seconds_with_seconds(text)
        .or_else(|| crate::geo::naive_seconds(text))
        .map(|v| v as i64)
}
