//! Explicit migration only. Call on the guarded INGEST writer; no automatic startup wiring.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params,
    types::{Value, ValueRef},
};

use super::*;
use crate::ai::space;
use crate::ai::{index, jobs, settings::fingerprint};

const TABLES: &[&str] = &["ai_scene_groups", "ai_scene_members", "ai_scene_active"];
/// Columns of `ai_scene_groups` in table order.
const HEADER_COLUMNS: usize = 17;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ai_scene_groups(
 result_id TEXT PRIMARY KEY, space_id TEXT NOT NULL, space_fingerprint TEXT NOT NULL,
 input_generation TEXT NOT NULL, processor_version TEXT NOT NULL,
 window_s INTEGER NOT NULL, similarity REAL NOT NULL, shortlist INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('waiting','local_ranked','review_overlap')),
 overlap TEXT NOT NULL, rank_used TEXT NOT NULL, weight_digest TEXT,
 lo INTEGER NOT NULL, hi INTEGER NOT NULL, input_guard TEXT NOT NULL, legacy_json TEXT NOT NULL,
 vector_guard TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_scene_members(
 result_id TEXT NOT NULL REFERENCES ai_scene_groups(result_id), photo_id INTEGER NOT NULL,
 generation TEXT NOT NULL, rank INTEGER NOT NULL CHECK(rank>0), score REAL,
 shortlisted INTEGER NOT NULL, representative INTEGER NOT NULL, shown INTEGER NOT NULL, favorite INTEGER NOT NULL,
 PRIMARY KEY(result_id,photo_id), UNIQUE(result_id,rank)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_scene_active(
 photo_id INTEGER PRIMARY KEY, result_id TEXT NOT NULL REFERENCES ai_scene_groups(result_id)
);
";

pub fn pending(conn: &Connection) -> Result<Vec<String>, Error> {
    TABLES
        .iter()
        .filter_map(|name| match jobs::object_exists(conn, "table", name) {
            Ok(true) => None,
            Ok(false) => Some(Ok((*name).to_string())),
            Err(e) => Some(Err(e.into())),
        })
        .collect()
}
/// Requires the caller's INGEST guard and Writer. This API is never invoked by readonly paths.
pub fn migrate(conn: &mut Connection) -> Result<Vec<String>, Error> {
    let missing = pending(conn)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.commit()?;
    Ok(missing)
}

#[derive(Clone)]
struct Row {
    values: Vec<Value>,
    digest: String,
    captured: Option<i64>,
}
impl Row {
    fn new(values: Vec<Value>) -> Self {
        Self {
            digest: row_digest(&values),
            captured: captured(&values[5]),
            values,
        }
    }
    fn id(&self) -> i64 {
        match self.values[0] {
            Value::Integer(id) => id,
            _ => 0,
        }
    }
    fn generation(&self) -> String {
        let mut values = self.values[..6].to_vec();
        values.extend_from_slice(&self.values[11..21]);
        row_digest(&values)
    }
    fn shown(&self) -> bool {
        let v = &self.values;
        text(&v[1]).is_some_and(|s| !s.is_empty())
            && number(&v[6]).unwrap_or(0.0) == 0.0
            && number(&v[8]).is_some()
            && number(&v[9]).is_some()
            && (text(&v[21]) != Some("exact") || number(&v[22]) == Some(1.0))
    }
    fn sha(&self) -> Option<&str> {
        text(&self.values[12]).filter(|s| valid_sha(s))
    }
    fn eligible(&self) -> Option<index::Eligible> {
        if text(&self.values[19]) != Some("") {
            return None;
        }
        Some(index::Eligible {
            photo_id: self.id(),
            input_sha256: self.sha()?.into(),
            display: text(&self.values[1]).filter(|s| !s.is_empty())?.into(),
            md5: self.values[2].clone(),
            mtime: self.values[3].clone(),
            rotation: self.values[4].clone(),
        })
    }
}
fn text(v: &Value) -> Option<&str> {
    if let Value::Text(t) = v {
        Some(t)
    } else {
        None
    }
}
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn row_digest(values: &[Value]) -> String {
    // Typed, length-delimited binary hashing. Never format DB values via Debug/log.
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for v in values {
        let (tag, bytes) = match v {
            Value::Null => (0, Vec::new()),
            Value::Integer(i) => (1, i.to_le_bytes().to_vec()),
            Value::Real(n) => (2, n.to_bits().to_le_bytes().to_vec()),
            Value::Text(s) => (3, s.as_bytes().to_vec()),
            Value::Blob(b) => (4, b.clone()),
        };
        digest.update(&[tag]);
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(&bytes);
    }
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
struct Vector {
    values: Vec<Value>,
    digest: String,
}
struct Legacy {
    origin: LegacyOrigin,
    ids: Vec<i64>,
    total: usize,
}
pub(super) struct Raw {
    identity: SpaceIdentity,
    rows: Vec<Row>,
    by_id: HashMap<i64, usize>,
    timed: BTreeMap<i64, Vec<usize>>,
    legacy_by_id: HashMap<i64, Vec<usize>>,
    pointers: HashMap<String, BTreeSet<i64>>,
    vectors: HashMap<String, Vector>,
    /// Vector BLOBs are loaded only for the cycle's snapshot and the writer's CAS.
    with_vectors: bool,
    failed: BTreeMap<(String, String), String>,
    generations: HashMap<String, String>,
    legacy: Vec<Legacy>,
    active: BTreeMap<i64, String>,
}
impl Raw {
    pub(super) fn pointer_guard(&self, ids: &[i64]) -> String {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        let parts: Vec<_> = ids
            .iter()
            .map(|id| self.active.get(id).cloned().unwrap_or_default())
            .collect();
        fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>())
    }
    /// Only the component's influence interval and its SHA dependencies participate.
    /// Changes in other time windows cannot starve a completed component.
    pub(super) fn guard(&self, lo: i64, hi: i64) -> String {
        if lo > hi {
            return fingerprint(&["invalid_scene_interval"]);
        }
        let mut rows: Vec<_> = self
            .timed
            .range(lo..=hi)
            .flat_map(|(_, indexes)| indexes.iter().map(|i| &self.rows[*i]))
            .collect();
        rows.sort_unstable_by_key(|r| r.id());
        let shas: BTreeSet<_> = rows.iter().filter_map(|r| r.sha()).collect();
        let ids: BTreeSet<_> = rows.iter().map(|r| r.id()).collect();
        let mut parts = vec![self.identity.fingerprint()];
        parts.extend(rows.iter().map(|r| r.digest.clone()));
        for sha in shas {
            parts.push(sha.into());
            // All duplicate generations, including those outside this time interval, affect failed classification.
            parts.push(self.generations.get(sha).cloned().unwrap_or_default());
            let target = index::image_embedding_key(&self.identity, sha).target_key;
            let key = self.generations.get(sha).cloned().unwrap_or_default();
            parts.push(self.failed.get(&(target, key)).cloned().unwrap_or_default());
        }
        let matches: BTreeSet<_> = ids
            .iter()
            .flat_map(|id| self.legacy_by_id.get(id).into_iter().flatten().copied())
            .collect();
        for i in matches {
            parts.push(self.legacy[i].origin.digest.clone());
        }
        fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>())
    }
    /// Digest of the vector BLOBs of the same interval. Only meaningful when the vectors were loaded
    /// (the cycle and the writer); read paths validate with [`Raw::guard`] alone.
    pub(super) fn vector_guard(&self, lo: i64, hi: i64) -> String {
        debug_assert!(self.with_vectors);
        let shas: BTreeSet<_> = self
            .timed
            .range(lo..=hi)
            .flat_map(|(_, indexes)| indexes.iter().filter_map(|i| self.rows[*i].sha()))
            .collect();
        let mut parts = vec!["scene-vectors-v1"];
        for sha in shas {
            parts.push(sha);
            parts.push(self.vectors.get(sha).map_or("missing", |v| &v.digest));
        }
        fingerprint(&parts)
    }
    pub(super) fn overlap(&self, ids: &[i64]) -> (Overlap, Vec<LegacyOrigin>) {
        let indexes: BTreeSet<_> = ids
            .iter()
            .flat_map(|id| self.legacy_by_id.get(id).into_iter().flatten().copied())
            .collect();
        let matches: Vec<_> = indexes.into_iter().map(|i| &self.legacy[i]).collect();
        let overlap = if matches.is_empty() {
            Overlap::Disjoint
        } else if matches.len() == 1 && matches[0].ids == ids && matches[0].total == ids.len() {
            Overlap::Exact
        } else {
            Overlap::Partial
        };
        (overlap, matches.iter().map(|g| g.origin.clone()).collect())
    }
}

const PHOTO_ROWS: &str = "/* scene-raw */ SELECT p.id,p.display,p.md5,p.mtime,p.rotation,p.taken_at,
       p.hidden,p.favorite,p.lat,p.lon,p.loc_source,f.photo_id,f.content_sha256,
       f.sharpness,f.luminance,f.exposure_score,f.black_clip,f.white_clip,f.contrast,f.error,f.phash,
       q.group_kind,q.recommended
       FROM photos p LEFT JOIN photo_quality_features f ON f.photo_id=p.id AND f.display=p.display
        AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
       LEFT JOIN photo_quality q ON q.photo_id=p.id AND q.display=p.display AND q.source_md5 IS p.md5 AND q.source_mtime IS p.mtime";

fn raw_in(conn: &Connection, with_vectors: bool) -> Result<Option<Raw>, Error> {
    let (_, identity) = index::active_in(conn)?;
    let Some(identity) = identity else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(&format!("{PHOTO_ROWS} ORDER BY p.id"))?;
    let rows = stmt
        .query_map([], |r| {
            (0..23)
                .map(|i| r.get::<_, Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .map(|r| r.map(Row::new))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_sha: BTreeMap<&str, Vec<index::Eligible>> = BTreeMap::new();
    for row in &rows {
        if let Some(eligible) = row.eligible() {
            by_sha.entry(row.sha().unwrap()).or_default().push(eligible);
        }
    }
    let generations = by_sha
        .into_iter()
        .map(|(sha, members)| {
            (
                sha.to_string(),
                index::image_embedding_generation_key(&identity, sha, &members).input_hash,
            )
        })
        .collect();
    let wanted: BTreeSet<_> = rows.iter().filter_map(|r| r.sha()).collect();
    let mut vectors = HashMap::new();
    if with_vectors && jobs::object_exists(conn, "table", "ai_embedding_vectors")? {
        let mut stmt = conn.prepare(
            "/* scene-vectors */ SELECT input_sha256,dimension,vector FROM ai_embedding_vectors WHERE space_id=?1",
        )?;
        let mut found = stmt.query([identity.space_id()])?;
        while let Some(row) = found.next()? {
            let Value::Text(sha) = row.get::<_, Value>(0)? else {
                continue;
            };
            if !wanted.contains(sha.as_str()) {
                continue;
            }
            let values = vec![row.get(1)?, row.get(2)?];
            let digest = row_digest(&values);
            vectors.insert(sha, Vector { values, digest });
        }
    }
    let mut failed = BTreeMap::new();
    if jobs::object_exists(conn, "table", "ai_jobs")? {
        let mut stmt = conn.prepare("SELECT target_key,input_hash,last_error_code FROM ai_jobs WHERE kind='image_embedding' AND state='failed' AND processor_version=?1 ORDER BY target_key,input_hash")?;
        let mut found = stmt.query([index::image_embedding_processor_version(&identity)])?;
        while let Some(row) = found.next()? {
            let reason: String = row.get(2)?;
            failed.insert(
                (row.get(0)?, row.get(1)?),
                index::reason_code(&reason).into(),
            );
        }
    }
    let legacy = legacy_in(conn)?;
    let active: BTreeMap<i64, String> = if jobs::object_exists(conn, "table", "ai_scene_active")? {
        conn.prepare("SELECT photo_id,result_id FROM ai_scene_active ORDER BY photo_id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?
    } else {
        BTreeMap::new()
    };
    let by_id = rows.iter().enumerate().map(|(i, r)| (r.id(), i)).collect();
    let mut timed = BTreeMap::<i64, Vec<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        if let Some(t) = row.captured {
            timed.entry(t).or_default().push(i);
        }
    }
    let mut legacy_by_id = HashMap::<i64, Vec<usize>>::new();
    for (i, g) in legacy.iter().enumerate() {
        for id in &g.ids {
            legacy_by_id.entry(*id).or_default().push(i);
        }
    }
    let mut pointers = HashMap::<String, BTreeSet<i64>>::new();
    for (id, result) in &active {
        pointers.entry(result.clone()).or_default().insert(*id);
    }
    Ok(Some(Raw {
        identity,
        rows,
        by_id,
        timed,
        legacy_by_id,
        pointers,
        vectors,
        with_vectors,
        failed,
        generations,
        legacy,
        active,
    }))
}

fn legacy_in(conn: &Connection) -> Result<Vec<Legacy>, Error> {
    let valid = crate::selection::VALID_MEMBERS;
    let mut stmt = conn.prepare(&format!("SELECT g.group_id,g.judge_model,g.judge_version,
        (SELECT COUNT(*) FROM photo_ai_group_members m WHERE m.group_id=g.group_id)
        FROM photo_ai_groups g WHERE g.status='judged' AND g.same_scene=1
        AND EXISTS(SELECT 1 FROM ({valid}) v WHERE v.group_id=g.group_id AND v.recommended=1 AND v.shown=1)
        ORDER BY g.group_id"))?;
    let heads = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as usize,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Bulk reads preserve old result lineage and avoid a query per group/member.
    let mut all = conn.prepare("SELECT * FROM photo_ai_groups ORDER BY group_id")?;
    let cols = all.column_count();
    let group_rows = all
        .query_map([], |r| {
            (0..cols)
                .map(|i| r.get::<_, Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let raw_groups: HashMap<_, _> = group_rows
        .iter()
        .filter_map(|r| Some((text(&r[0])?.to_string(), row_digest(r))))
        .collect();
    let mut members = conn.prepare(&format!(
        "SELECT group_id,photo_id,recommended,shown FROM ({valid}) ORDER BY group_id,photo_id"
    ))?;
    let mut valid_members: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut found = members.query([])?;
    while let Some(row) = found.next()? {
        let values = (0..4)
            .map(|i| row.get::<_, Value>(i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        valid_members
            .entry(row.get(0)?)
            .or_default()
            .push((row.get(1)?, row_digest(&values)));
    }
    // Include full old member rows (shortlist, local rank/score, recommendation), not only IDs.
    let mut members =
        conn.prepare("SELECT * FROM photo_ai_group_members ORDER BY group_id,photo_id")?;
    let cols = members.column_count();
    let mut old_members: HashMap<String, Vec<String>> = HashMap::new();
    let mut found = members.query([])?;
    while let Some(row) = found.next()? {
        let values = (0..cols)
            .map(|i| row.get::<_, Value>(i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let Some(id) = text(&values[0]) {
            old_members
                .entry(id.into())
                .or_default()
                .push(row_digest(&values));
        }
    }
    Ok(heads
        .into_iter()
        .map(|(group_id, judge_model, judge_version, total)| {
            let members = valid_members.remove(&group_id).unwrap_or_default();
            let ids = members.iter().map(|(id, _)| *id).collect();
            let mut parts = vec![raw_groups.get(&group_id).cloned().unwrap_or_default()];
            parts.extend(members.into_iter().map(|(_, d)| d));
            parts.extend(old_members.remove(&group_id).unwrap_or_default());
            let digest = fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>());
            Legacy {
                origin: LegacyOrigin {
                    group_id,
                    digest,
                    judge_model,
                    judge_version,
                },
                ids,
                total,
            }
        })
        .collect())
}

/// Captures one consistent read snapshot; vector validation and all ranking happen after COMMIT.
pub fn snapshot(conn: &Connection) -> Result<Option<Snapshot>, Error> {
    let tx = conn.unchecked_transaction()?;
    let raw = raw_in(&tx, true)?;
    tx.commit()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    type CheckedVector = (Arc<Vec<f32>>, f64);
    let mut checked: HashMap<&str, CheckedVector> = HashMap::new();
    for (sha, vector) in &raw.vectors {
        if number(&vector.values[0]) != Some(f64::from(raw.identity.dimension())) {
            continue;
        }
        let Value::Blob(blob) = &vector.values[1] else {
            continue;
        };
        if let Ok(values) = space::decode(&raw.identity, blob) {
            // f32 extremes squared in f64 remain finite/nonzero. This also matches Python cosine's
            // f64 accumulation without rounding a second unit vector back to f32.
            let norm = values
                .iter()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
                .sqrt();
            checked.insert(sha, (Arc::new(values), norm));
        }
    }
    let mut photos = Vec::new();
    for row in &raw.rows {
        let v = &row.values;
        let metrics = metrics(v);
        let sha = row.sha();
        let vectors = sha.and_then(|s| checked.get(s));
        let readiness = if !text(&v[1]).is_some_and(|s| !s.is_empty()) {
            Readiness::NoDisplay
        } else if row.captured.is_none() {
            if matches!(&v[5], Value::Null) || text(&v[5]) == Some("") {
                Readiness::MissingTime
            } else {
                Readiness::InvalidTime
            }
        } else if row.eligible().is_none() {
            if text(&v[19]).is_some_and(|e| !e.is_empty()) {
                Readiness::FailedQuality
            } else {
                Readiness::PendingQuality
            }
        } else if vectors.is_some() && metrics.is_some() {
            Readiness::Ready
        } else {
            let sha = sha.unwrap();
            let target = index::image_embedding_key(&raw.identity, sha).target_key;
            let generation = raw.generations.get(sha).cloned().unwrap_or_default();
            if vectors.is_none() && raw.failed.contains_key(&(target, generation)) {
                Readiness::FailedEmbedding
            } else if metrics.is_none() {
                Readiness::PendingQuality
            } else if raw.vectors.contains_key(sha) {
                Readiness::CorruptVector
            } else {
                Readiness::PendingEmbedding
            }
        };
        let generation = row.generation();
        let shown = row.shown();
        photos.push(Photo {
            input: Input {
                photo_id: row.id(),
                display: v[1].clone(),
                md5: v[2].clone(),
                mtime: v[3].clone(),
                rotation: v[4].clone(),
                taken_at: v[5].clone(),
                quality_sha: sha.map(str::to_string),
            },
            id: row.id(),
            captured: row.captured,
            readiness,
            generation,
            metrics,
            vector: vectors.map(|(v, _)| v.clone()),
            norm: vectors.map_or(0.0, |(_, norm)| *norm),
            shown,
            favorite: number(&v[7]) == Some(1.0),
        });
    }
    Ok(Some(Snapshot {
        identity: raw.identity.clone(),
        photos,
        raw,
    }))
}
fn metrics(v: &[Value]) -> Option<Metrics> {
    // Luminance is optional for the rule formula, but not for a weight that requests it.
    let m = Metrics {
        sharpness: number(&v[13])?,
        luminance: number(&v[14]).unwrap_or(f64::NAN),
        exposure_score: number(&v[15])?,
        black_clip: number(&v[16])?,
        white_clip: number(&v[17])?,
        contrast: number(&v[18])?,
    };
    // Bound physical feature ranges so the fixed fallback cannot overflow either.
    (m.sharpness >= 0.0
        && m.sharpness <= 100.0
        && (0.0..=100.0).contains(&m.exposure_score)
        && (0.0..=1.0).contains(&m.black_clip)
        && (0.0..=1.0).contains(&m.white_clip)
        && (0.0..=255.0).contains(&m.contrast)
        && (m.luminance.is_nan() || (0.0..=255.0).contains(&m.luminance)))
    .then_some(m)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Stored,
    Unchanged,
    Stale,
    Interrupted,
}
/// One component per short transaction. No component/weight/vector arithmetic runs on the writer.
/// CAS covers the whole influence window, duplicates and effective legacy selection.
pub fn save(
    conn: &mut Connection,
    plan: &Plan,
    group_index: usize,
    stopped: &dyn Fn() -> bool,
) -> Result<Saved, Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(raw) = raw_in(&tx, true)? else {
        return Ok(Saved::Stale);
    };
    let saved = save_in(&tx, &raw, plan, group_index, stopped)?;
    if matches!(saved, Saved::Stored | Saved::Unchanged) {
        tx.commit()?;
    }
    Ok(saved)
}

/// Bounded shared validation: one current-input scan per short batch, never per component.
/// Every group still checks its full influence guard, structure and expected-active CAS.
pub fn save_batch(
    conn: &mut Connection,
    plan: &Plan,
    indexes: &[usize],
    stopped: &dyn Fn() -> bool,
) -> Result<Vec<Saved>, Error> {
    if indexes.len() > super::runtime::GROUPS_PER_CYCLE {
        return Err(Error::InvalidOptions);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if stopped() {
        return Ok(vec![Saved::Interrupted; indexes.len()]);
    }
    let Some(raw) = raw_in(&tx, true)? else {
        return Ok(vec![Saved::Stale; indexes.len()]);
    };
    let mut saved = Vec::new();
    for &i in indexes {
        let result = save_in(&tx, &raw, plan, i, stopped)?;
        if result == Saved::Interrupted {
            return Ok(vec![Saved::Interrupted; indexes.len()]);
        }
        saved.push(result);
    }
    if stopped() {
        return Ok(vec![Saved::Interrupted; indexes.len()]);
    }
    tx.commit()?;
    Ok(saved)
}

fn save_in(
    tx: &Connection,
    raw: &Raw,
    plan: &Plan,
    group_index: usize,
    stopped: &dyn Fn() -> bool,
) -> Result<Saved, Error> {
    let Some(group) = plan.groups.get(group_index) else {
        return Err(Error::CorruptResult);
    };
    if stopped() {
        return Ok(Saved::Interrupted);
    }
    if raw.identity != plan.identity
        || raw.guard(group.lo, group.hi) != group.guard
        || raw.vector_guard(group.lo, group.hi) != group.vector_guard
    {
        return Ok(Saved::Stale);
    }
    if !valid_group(raw, group, plan.options)? {
        return Err(Error::CorruptResult);
    }
    let values = group_values(&plan.identity, group, plan.options)?;
    let existing: Option<Vec<Value>> = tx
        .query_row(
            "SELECT * FROM ai_scene_groups WHERE result_id=?1",
            [&group.result_id],
            |r| (0..HEADER_COLUMNS).map(|i| r.get(i)).collect(),
        )
        .optional()?;
    let stored_members_match = if existing.as_ref() == Some(&values) {
        match read_members(tx, &group.result_id) {
            Ok(members) => members == group.members,
            Err(Error::CorruptResult) => false,
            Err(error) => return Err(error),
        }
    } else {
        false
    };
    if stored_members_match && (group.state == State::Waiting || raw_pointers_match(raw, group)) {
        return Ok(Saved::Unchanged);
    }
    let ids: Vec<_> = group.members.iter().map(|m| m.photo_id).collect();
    if raw.pointer_guard(&ids) != group.expected_active {
        return Ok(Saved::Stale);
    }
    tx.execute("INSERT INTO ai_scene_groups VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
       ON CONFLICT(result_id) DO UPDATE SET space_id=excluded.space_id,space_fingerprint=excluded.space_fingerprint,
       input_generation=excluded.input_generation,processor_version=excluded.processor_version,window_s=excluded.window_s,
       similarity=excluded.similarity,shortlist=excluded.shortlist,state=excluded.state,overlap=excluded.overlap,
       rank_used=excluded.rank_used,weight_digest=excluded.weight_digest,lo=excluded.lo,hi=excluded.hi,
       input_guard=excluded.input_guard,legacy_json=excluded.legacy_json,vector_guard=excluded.vector_guard",
       rusqlite::params_from_iter(values.iter()))?;
    tx.execute(
        "DELETE FROM ai_scene_members WHERE result_id=?1",
        [&group.result_id],
    )?;
    let mut insert =
        tx.prepare("INSERT INTO ai_scene_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")?;
    for m in &group.members {
        insert.execute(params![
            group.result_id,
            m.photo_id,
            m.generation,
            m.rank as i64,
            m.score,
            m.shortlisted,
            m.representative,
            m.shown,
            m.favorite
        ])?;
        if group.state != State::Waiting {
            tx.execute("INSERT INTO ai_scene_active VALUES(?1,?2) ON CONFLICT(photo_id) DO UPDATE SET result_id=excluded.result_id",params![m.photo_id,group.result_id])?;
        }
    }
    drop(insert);
    if group.state != State::Waiting {
        tx.execute(
            "DELETE FROM ai_scene_active WHERE result_id=?1 AND NOT EXISTS(
            SELECT 1 FROM ai_scene_members m WHERE m.result_id=ai_scene_active.result_id
            AND m.photo_id=ai_scene_active.photo_id)",
            [&group.result_id],
        )?;
    }
    if stopped() {
        return Ok(Saved::Interrupted);
    }
    Ok(Saved::Stored)
}
fn raw_pointers_match(raw: &Raw, group: &Group) -> bool {
    raw.pointers.get(&group.result_id).is_some_and(|ids| {
        ids.len() == group.members.len() && group.members.iter().all(|m| ids.contains(&m.photo_id))
    })
}

/// Writer-free skip check against the computed plan; includes every header/member and pointer.
/// Never trusts result IDs alone. A mismatch is repaired only through the usual writer CAS.
pub fn pending_saves(conn: &Connection, plan: &Plan, limit: usize) -> Result<Vec<usize>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if !pending(conn)?.is_empty() {
        return Err(Error::Database);
    }
    let tx = conn.unchecked_transaction()?;
    let mut pointers = HashMap::<String, BTreeSet<i64>>::new();
    for row in tx
        .prepare("SELECT result_id,photo_id FROM ai_scene_active")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
    {
        let (result, id) = row?;
        pointers.entry(result).or_default().insert(id);
    }
    let mut pending = Vec::new();
    let mut stmt = tx.prepare("SELECT * FROM ai_scene_groups WHERE result_id=?1")?;
    for (i, g) in plan.groups.iter().enumerate() {
        let existing = stmt
            .query_row([&g.result_id], |r| {
                (0..HEADER_COLUMNS)
                    .map(|j| r.get::<_, Value>(j))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .optional()?;
        let matches = existing.as_ref() == Some(&group_values(&plan.identity, g, plan.options)?)
            && match read_members(&tx, &g.result_id) {
                Ok(m) => m == g.members,
                Err(Error::CorruptResult) => false,
                Err(e) => return Err(e),
            }
            && (g.state == State::Waiting
                || pointers.get(&g.result_id).is_some_and(|ids| {
                    ids.len() == g.members.len()
                        && g.members.iter().all(|m| ids.contains(&m.photo_id))
                }));
        if !matches {
            pending.push(i);
            if pending.len() == limit {
                break;
            }
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok(pending)
}

fn group_values(
    identity: &SpaceIdentity,
    group: &Group,
    options: Options,
) -> Result<Vec<Value>, Error> {
    Ok(vec![
        group.result_id.clone().into(),
        identity.space_id().to_string().into(),
        identity.fingerprint().into(),
        group.input_generation.clone().into(),
        group.processor_version.clone().into(),
        options.window_s.into(),
        options.similarity.into(),
        (options.shortlist as i64).into(),
        state_name(group.state).to_string().into(),
        overlap_name(group.overlap).to_string().into(),
        encode(&group.rank_used)?.into(),
        group.weight_digest.clone().map_or(Value::Null, Value::Text),
        group.lo.into(),
        group.hi.into(),
        group.guard.clone().into(),
        encode(&group.legacy)?.into(),
        group.vector_guard.clone().into(),
    ])
}

fn flag(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<bool> {
    match row.get_ref(index)? {
        ValueRef::Integer(0) => Ok(false),
        ValueRef::Integer(1) => Ok(true),
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            Box::new(Error::CorruptResult),
        )),
    }
}

fn read_members(conn: &Connection, result_id: &str) -> Result<Vec<Member>, Error> {
    let mut stmt = conn.prepare(
        "SELECT photo_id,generation,rank,score,shortlisted,representative,shown,favorite
        FROM ai_scene_members WHERE result_id=?1 ORDER BY rank",
    )?;
    stmt.query_map([result_id], |r| {
        Ok(Member {
            photo_id: r.get(0)?,
            generation: r.get(1)?,
            rank: r.get::<_, i64>(2)? as usize,
            score: r.get(3)?,
            shortlisted: flag(r, 4)?,
            representative: flag(r, 5)?,
            shown: flag(r, 6)?,
            favorite: flag(r, 7)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()
    .map_err(stored_row_error)
}

fn stored_row_error(error: rusqlite::Error) -> Error {
    if error.sqlite_error_code().is_some() {
        Error::Database
    } else {
        Error::CorruptResult
    }
}

/// Shared by current reads and the save fast path; all members must describe one complete result.
fn valid_group(raw: &Raw, group: &Group, options: Options) -> Result<bool, Error> {
    if group.members.is_empty()
        || (matches!(group.rank_used, RankUsed::Linear | RankUsed::Fallback(_))
            != group.weight_digest.is_some())
        || group
            .weight_digest
            .as_deref()
            .is_some_and(|d| !valid_sha(d))
        || group.processor_version
            != options.group_processor_version(
                &raw.identity,
                group.rank_used,
                group.weight_digest.as_deref(),
            )
    {
        return Ok(false);
    }
    let members: BTreeMap<_, _> = group.members.iter().map(|m| (m.photo_id, m)).collect();
    if members.len() != group.members.len() {
        return Ok(false);
    }
    let waiting = group.state == State::Waiting;
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for (i, m) in group.members.iter().enumerate() {
        let Some(row) = raw.by_id.get(&m.photo_id).map(|i| &raw.rows[*i]) else {
            return Ok(false);
        };
        let Some(time) = row.captured else {
            return Ok(false);
        };
        lo = lo.min(time);
        hi = hi.max(time);
        if m.generation != row.generation()
            || m.rank != i + 1
            || m.shown != row.shown()
            || m.favorite != (number(&row.values[7]) == Some(1.0))
            || m.shortlisted != (!waiting && m.rank <= options.shortlist)
            || m.representative != (!waiting && m.rank == 1)
            || if waiting {
                m.score.is_some()
            } else {
                !m.score
                    .is_some_and(|s| s.is_finite() && (0.0..=100.0).contains(&s))
            }
        {
            return Ok(false);
        }
    }
    if group.lo != lo.saturating_sub(options.window_s)
        || group.hi != hi.saturating_add(options.window_s)
    {
        return Ok(false);
    }
    if !waiting
        && group.members.windows(2).any(|m| {
            let order = m[0].score.unwrap().total_cmp(&m[1].score.unwrap());
            order.is_lt() || (order.is_eq() && m[0].photo_id > m[1].photo_id)
        })
    {
        return Ok(false);
    }
    let generation = fingerprint(
        &members
            .values()
            .map(|m| m.generation.as_str())
            .collect::<Vec<_>>(),
    );
    if group.input_generation != generation
        || group.result_id
            != fingerprint(&["scene-result-v1", &group.processor_version, &generation])
    {
        return Ok(false);
    }
    let ids: Vec<_> = members.keys().copied().collect();
    let (overlap, legacy) = raw.overlap(&ids);
    Ok(group.overlap == overlap
        && encode(&group.legacy)? == encode(&legacy)?
        && (waiting || (group.state == State::ReviewOverlap) == (overlap == Overlap::Partial)))
}
fn state_name(state: State) -> &'static str {
    match state {
        State::Waiting => "waiting",
        State::LocalRanked => "local_ranked",
        State::ReviewOverlap => "review_overlap",
    }
}
fn overlap_name(overlap: Overlap) -> &'static str {
    match overlap {
        Overlap::Disjoint => "disjoint",
        Overlap::Exact => "exact",
        Overlap::Partial => "partial",
    }
}
fn encode<T: serde::Serialize>(v: &T) -> Result<String, Error> {
    serde_json::to_string(v).map_err(|_| Error::CorruptResult)
}

pub struct Loaded {
    pub(super) identity: Option<SpaceIdentity>,
    pub(super) groups: Vec<Group>,
}
impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneLoaded")
            .field("groups", &self.groups.len())
            .finish_non_exhaustive()
    }
}
impl Loaded {
    pub fn identity(&self) -> Option<&SpaceIdentity> {
        self.identity.as_ref()
    }
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
}
/// Pointer, rows, active space and current input guards in a single read transaction.
/// `processor_version` is required: callers cannot accidentally accept another ranking configuration.
pub fn load_current(conn: &Connection, processor_version: &str) -> Result<Loaded, Error> {
    let tx = conn.unchecked_transaction()?;
    let loaded = load_current_in(&tx, &[processor_version])?;
    tx.commit()?;
    Ok(loaded)
}

pub fn load_current_in(tx: &Connection, processors: &[&str]) -> Result<Loaded, Error> {
    load_projection_in(tx, processors, false)
}
pub fn load_projection_in(
    tx: &Connection,
    processors: &[&str],
    include_waiting: bool,
) -> Result<Loaded, Error> {
    // No vector BLOB is read here: validation uses rows, SHAs, failed jobs, legacy state and the stored guard.
    let Some(raw) = raw_in(tx, false)? else {
        return Ok(Loaded {
            identity: None,
            groups: Vec::new(),
        });
    };
    if !pending(tx)?.is_empty() {
        return Ok(Loaded {
            identity: Some(raw.identity),
            groups: Vec::new(),
        });
    }
    let mut stmt=tx.prepare("SELECT result_id,input_generation,processor_version,state,overlap,rank_used,lo,hi,input_guard,weight_digest,legacy_json,window_s,similarity,shortlist,space_id,vector_guard FROM ai_scene_groups g
      WHERE space_fingerprint=?1 AND (?2 OR state!='waiting')
      ORDER BY result_id")?;
    let rows = stmt
        .query_map(params![raw.identity.fingerprint(), include_waiting], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, f64>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, String>(14)?,
                r.get::<_, String>(15)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(stored_row_error)?;
    drop(stmt);
    let mut groups = Vec::new();
    for (
        result_id,
        input_generation,
        processor_version,
        state,
        overlap,
        rank_used,
        lo,
        hi,
        guard,
        weight_digest,
        legacy,
        window_s,
        similarity,
        shortlist,
        space_id,
        vector_guard,
    ) in rows
    {
        if lo > hi {
            return Err(Error::CorruptResult);
        }
        if (state != "waiting" && !raw.pointers.contains_key(&result_id))
            || !processors.contains(&processor_version.as_str())
            || raw.guard(lo, hi) != guard
        {
            continue;
        }
        let state = match state.as_str() {
            "waiting" => State::Waiting,
            "local_ranked" => State::LocalRanked,
            "review_overlap" => State::ReviewOverlap,
            _ => return Err(Error::CorruptResult),
        };
        let overlap = match overlap.as_str() {
            "disjoint" => Overlap::Disjoint,
            "exact" => Overlap::Exact,
            "partial" => Overlap::Partial,
            _ => return Err(Error::CorruptResult),
        };
        let rank_used = serde_json::from_str(&rank_used).map_err(|_| Error::CorruptResult)?;
        let legacy = serde_json::from_str(&legacy).map_err(|_| Error::CorruptResult)?;
        let members = read_members(tx, &result_id)?;
        let group = Group {
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
            guard,
            vector_guard,
            weight_digest,
            expected_active: String::new(),
        };
        let shortlist = usize::try_from(shortlist).map_err(|_| Error::CorruptResult)?;
        let options =
            Options::new(window_s, similarity, shortlist).map_err(|_| Error::CorruptResult)?;
        if space_id != raw.identity.space_id() || !valid_group(&raw, &group, options)? {
            return Err(Error::CorruptResult);
        }
        if group.state == State::Waiting || raw_pointers_match(&raw, &group) {
            groups.push(group);
        }
    }
    Ok(Loaded {
        identity: Some(raw.identity),
        groups,
    })
}

fn processors_sql(processors: &[&str]) -> String {
    (0..processors.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",")
}
fn parse_state(state: &str) -> Result<State, Error> {
    match state {
        "waiting" => Ok(State::Waiting),
        "local_ranked" => Ok(State::LocalRanked),
        "review_overlap" => Ok(State::ReviewOverlap),
        _ => Err(Error::CorruptResult),
    }
}

/// The active identity and `(state, ranking, count)` of the current stored headers.
pub type HeaderCounts = (Option<SpaceIdentity>, Vec<(State, RankUsed, i64)>);

/// Header-only counts for the status views: no photo row, vector or member is read.
/// The result is advisory (no per-component guard); the cycle replaces a stale header at the next poll.
pub fn header_counts_in(tx: &Connection, processors: &[&str]) -> Result<HeaderCounts, Error> {
    let (_, identity) = index::active_in(tx)?;
    let Some(identity) = identity else {
        return Ok((None, Vec::new()));
    };
    if !pending(tx)?.is_empty() || processors.is_empty() {
        return Ok((Some(identity), Vec::new()));
    }
    let mut stmt = tx.prepare(&format!(
        "SELECT state,rank_used,COUNT(*) FROM ai_scene_groups g
          WHERE space_fingerprint=?1 AND processor_version IN ({})
            AND (state='waiting' OR EXISTS(SELECT 1 FROM ai_scene_active a WHERE a.result_id=g.result_id))
          GROUP BY state,rank_used",
        processors_sql(processors)
    ))?;
    let params =
        std::iter::once(identity.fingerprint()).chain(processors.iter().map(|p| p.to_string()));
    let counts = stmt
        .query_map(rusqlite::params_from_iter(params), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(stored_row_error)?;
    let mut out = Vec::new();
    for (state, rank_used, n) in counts {
        out.push((
            parse_state(&state)?,
            serde_json::from_str(&rank_used).map_err(|_| Error::CorruptResult)?,
            n,
        ));
    }
    Ok((Some(identity), out))
}

/// One photo's stored scene position, for the management list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Annotation {
    pub state: State,
    pub overlap: Overlap,
    pub rank: usize,
    pub score: Option<f64>,
    pub representative: bool,
    pub shortlisted: bool,
}

/// Scene positions of the given photos only (a page). Reads those photos' rows and their stored
/// members; a member is used only when its generation still equals the photo's current generation,
/// the result carries the current space and processor, and (unless waiting) the active pointer
/// still names it. Advisory: the per-component guard is not recomputed here.
pub fn annotations_in(
    tx: &Connection,
    processors: &[&str],
    ids: &[i64],
) -> Result<(Option<SpaceIdentity>, HashMap<i64, Annotation>), Error> {
    let (_, identity) = index::active_in(tx)?;
    let Some(identity) = identity else {
        return Ok((None, HashMap::new()));
    };
    if ids.is_empty() || processors.is_empty() || !pending(tx)?.is_empty() {
        return Ok((Some(identity), HashMap::new()));
    }
    let marks = |from: usize| {
        (0..ids.len())
            .map(|i| format!("?{}", from + i))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut generations = HashMap::new();
    let mut stmt = tx.prepare(&format!("{PHOTO_ROWS} WHERE p.id IN ({})", marks(1)))?;
    let found = stmt.query_map(rusqlite::params_from_iter(ids), |r| {
        (0..23)
            .map(|i| r.get::<_, Value>(i))
            .collect::<rusqlite::Result<Vec<_>>>()
    })?;
    for row in found {
        let row = Row::new(row?);
        generations.insert(row.id(), row.generation());
    }
    drop(stmt);
    let first = 2 + processors.len();
    let mut stmt = tx.prepare(&format!(
        "SELECT m.photo_id,m.generation,g.state,g.overlap,m.rank,m.score,m.representative,m.shortlisted
           FROM ai_scene_members m JOIN ai_scene_groups g ON g.result_id=m.result_id
          WHERE m.photo_id IN ({}) AND g.space_fingerprint=?1 AND g.processor_version IN ({})
            AND (g.state='waiting' OR EXISTS(SELECT 1 FROM ai_scene_active a
                  WHERE a.photo_id=m.photo_id AND a.result_id=g.result_id))
          ORDER BY m.photo_id,(g.state='waiting'),g.result_id",
        marks(first),
        processors_sql(processors)
    ))?;
    let mut values: Vec<Value> = vec![identity.fingerprint().into()];
    values.extend(processors.iter().map(|p| Value::Text(p.to_string())));
    values.extend(ids.iter().map(|id| Value::Integer(*id)));
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<f64>>(5)?,
                flag(r, 6)?,
                flag(r, 7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(stored_row_error)?;
    let mut out = HashMap::new();
    for (id, generation, state, overlap, rank, score, representative, shortlisted) in rows {
        if out.contains_key(&id) || generations.get(&id) != Some(&generation) {
            continue;
        }
        let overlap = match overlap.as_str() {
            "disjoint" => Overlap::Disjoint,
            "exact" => Overlap::Exact,
            "partial" => Overlap::Partial,
            _ => return Err(Error::CorruptResult),
        };
        out.insert(
            id,
            Annotation {
                state: parse_state(&state)?,
                overlap,
                rank: usize::try_from(rank).map_err(|_| Error::CorruptResult)?,
                score,
                representative,
                shortlisted,
            },
        );
    }
    Ok((Some(identity), out))
}
