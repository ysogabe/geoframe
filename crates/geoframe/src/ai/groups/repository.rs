//! Explicit migration only. Call on the guarded INGEST writer; no automatic startup wiring.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params,
    types::{Value, ValueRef},
};

use super::*;
use crate::ai::space;
use crate::ai::{index, jobs, settings::fingerprint};

/// Tables and their columns in table order. A table that exists with other columns is never trusted.
const TABLES: &[(&str, &[&str])] = &[
    (
        "ai_scene_groups",
        &[
            "result_id",
            "space_id",
            "space_fingerprint",
            "input_generation",
            "processor_version",
            "window_s",
            "similarity",
            "shortlist",
            "state",
            "overlap",
            "rank_used",
            "weight_digest",
            "lo",
            "hi",
            "input_guard",
            "legacy_json",
            "vector_guard",
        ],
    ),
    (
        "ai_scene_members",
        &[
            "result_id",
            "photo_id",
            "generation",
            "rank",
            "score",
            "shortlisted",
            "representative",
            "shown",
            "favorite",
        ],
    ),
    ("ai_scene_active", &["photo_id", "result_id"]),
];
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
/// The one development shape of `ai_scene_groups` that predates `vector_guard`. The scene tables are
/// derived data that no release carried, so this single missing trailing column is added in place
/// (the rows are replaced by the next cycle); any other shape is not repaired.
const OUTDATED_COLUMN: &str = "vector_guard";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Absent,
    Current,
    /// `ai_scene_groups` without its last column (`vector_guard`).
    Outdated,
    Unknown,
}
fn classify(table: &str, columns: &[&str], found: &[String]) -> Shape {
    if found.is_empty() {
        Shape::Absent
    } else if found.iter().map(String::as_str).eq(columns.iter().copied()) {
        Shape::Current
    } else if table == "ai_scene_groups"
        && columns.split_last().is_some_and(|(last, rest)| {
            *last == OUTDATED_COLUMN && found.iter().map(String::as_str).eq(rest.iter().copied())
        })
    {
        Shape::Outdated
    } else {
        Shape::Unknown
    }
}
/// One statement for all three tables (a PRAGMA per table costs about a millisecond per read).
fn shapes(conn: &Connection) -> Result<Vec<(&'static str, Shape)>, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.name,i.name FROM sqlite_master m, pragma_table_info(m.name) i
          WHERE m.type='table' AND m.name IN ('ai_scene_groups','ai_scene_members','ai_scene_active')
          ORDER BY m.name,i.cid",
    )?;
    let mut columns: HashMap<String, Vec<String>> = HashMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        columns
            .entry(row.get(0)?)
            .or_default()
            .push(row.get::<_, String>(1)?);
    }
    Ok(TABLES
        .iter()
        .map(|(name, expected)| {
            let found = columns.remove(*name).unwrap_or_default();
            (*name, classify(name, expected, &found))
        })
        .collect())
}
/// Every scene table exists with exactly the current columns.
fn ready(conn: &Connection) -> Result<bool, Error> {
    Ok(shapes(conn)?.iter().all(|(_, s)| *s == Shape::Current))
}
/// Like [`ready`], but a table with other columns than any this code writes is a fixed error
/// (`SchemaMismatch`) instead of a silently empty result.
fn ready_or_mismatch(conn: &Connection) -> Result<bool, Error> {
    let shapes = shapes(conn)?;
    if shapes
        .iter()
        .any(|(_, s)| matches!(s, Shape::Outdated | Shape::Unknown))
    {
        return Err(Error::SchemaMismatch);
    }
    Ok(shapes.iter().all(|(_, s)| *s == Shape::Current))
}

/// What [`migrate`] would add: missing tables by name, and `ai_scene_groups.vector_guard` for the one
/// outdated shape. A table with other columns is not in this list (migrate does not touch it): it is
/// reported by [`mismatched`], so a dry-run does not announce work that never happens.
pub fn pending(conn: &Connection) -> Result<Vec<String>, Error> {
    Ok(shapes(conn)?
        .into_iter()
        .filter_map(|(name, shape)| match shape {
            Shape::Current | Shape::Unknown => None,
            Shape::Absent => Some(name.to_string()),
            Shape::Outdated => Some(format!("{name}.{OUTDATED_COLUMN}")),
        })
        .collect())
}
/// Scene tables that exist with columns this code does not write (and that [`migrate`] leaves alone).
/// The cycle and the scene reads fail with `SchemaMismatch` for them until the derived tables are dropped.
pub fn mismatched(conn: &Connection) -> Result<Vec<String>, Error> {
    Ok(shapes(conn)?
        .into_iter()
        .filter(|(_, shape)| *shape == Shape::Unknown)
        .map(|(name, _)| name.to_string())
        .collect())
}
/// All three scene tables exist in exactly the current shape (nothing pending, nothing mismatched).
pub fn current(conn: &Connection) -> bool {
    ready(conn).unwrap_or(false)
}
/// Requires the caller's INGEST guard and Writer. This API is never invoked by readonly paths.
/// Missing tables are created and the outdated `ai_scene_groups` gets its `vector_guard` column. A table
/// of any other shape is left untouched (and not reported as created): the cycle and the scene reads
/// then fail with the fixed `SchemaMismatch` while delivery continues, until the derived scene tables
/// are dropped.
pub fn migrate(conn: &mut Connection) -> Result<Vec<String>, Error> {
    let created: Vec<String> = shapes(conn)?
        .into_iter()
        .filter_map(|(name, shape)| match shape {
            Shape::Absent => Some(name.to_string()),
            Shape::Outdated => Some(format!("{name}.{OUTDATED_COLUMN}")),
            Shape::Current | Shape::Unknown => None,
        })
        .collect();
    if created.is_empty() {
        return Ok(created);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    if shapes(&tx)?
        .iter()
        .any(|(name, s)| *name == "ai_scene_groups" && *s == Shape::Outdated)
    {
        tx.execute_batch(&format!(
            "ALTER TABLE ai_scene_groups ADD COLUMN {OUTDATED_COLUMN} TEXT NOT NULL DEFAULT ''"
        ))?;
    }
    tx.commit()?;
    Ok(created)
}

#[derive(Clone)]
struct Row {
    id: i64,
    /// All 23 columns for the cycle's snapshot only. Reads keep none: the facts they need are the
    /// fields below, and the digests cover every column.
    values: Vec<Value>,
    /// Digest of every column: any change of the photo or its quality changes it.
    digest: [u8; 32],
    /// Digest of the columns that make the photo's generation (its hex form is stored with each member).
    generation_digest: [u8; 32],
    captured: Option<i64>,
    /// A valid `content_sha256` (lower-case hex), and its index into [`Raw::shas`].
    sha: Option<[u8; 64]>,
    sha_ix: Option<usize>,
    shown: bool,
    favorite: bool,
    eligible: bool,
}
impl Row {
    /// One row of [`PHOTO_ROWS`]. `scratch` is a reusable buffer for the typed encoding of the
    /// columns (one hash call per digest); the encoding is taken from the borrowed column values.
    fn from_sql(
        r: &rusqlite::Row<'_>,
        scratch: &mut Vec<u8>,
        full: bool,
    ) -> rusqlite::Result<Self> {
        scratch.clear();
        let mut ends = [0usize; 23];
        for (i, end) in ends.iter_mut().enumerate() {
            encode_ref(scratch, r.get_ref(i)?);
            *end = scratch.len();
        }
        // The generation covers id..taken_at (columns 0..6) and the quality identity (columns 11..21).
        let mut generation = ring::digest::Context::new(&ring::digest::SHA256);
        generation.update(&scratch[..ends[5]]);
        generation.update(&scratch[ends[10]..ends[20]]);
        let generation_digest = finish32(generation);
        // The full digest is the generation digest plus the remaining columns, so no byte is hashed twice.
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(&generation_digest);
        digest.update(&scratch[ends[5]..ends[10]]);
        digest.update(&scratch[ends[20]..]);
        let value_text = |i: usize| match r.get_ref(i) {
            Ok(ValueRef::Text(t)) => std::str::from_utf8(t).ok(),
            _ => None,
        };
        let value_number = |i: usize| match r.get_ref(i) {
            Ok(ValueRef::Integer(n)) => Some(n as f64),
            Ok(ValueRef::Real(n)) if n.is_finite() => Some(n),
            _ => None,
        };
        let display = value_text(1).is_some_and(|s| !s.is_empty());
        let sha = value_text(12).filter(|s| valid_sha(s)).map(|s| {
            let mut out = [0u8; 64];
            out.copy_from_slice(s.as_bytes());
            out
        });
        let values = if full {
            (0..23)
                .map(|i| r.get::<_, Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            id: match r.get_ref(0)? {
                ValueRef::Integer(id) => id,
                _ => 0,
            },
            digest: finish32(digest),
            generation_digest,
            captured: value_text(5).and_then(captured_text),
            shown: display
                && value_number(6).unwrap_or(0.0) == 0.0
                && value_number(8).is_some()
                && value_number(9).is_some()
                && (value_text(21) != Some("exact") || value_number(22) == Some(1.0)),
            favorite: value_number(7) == Some(1.0),
            eligible: display && sha.is_some() && value_text(19) == Some(""),
            sha,
            sha_ix: None,
            values,
        })
    }
    fn id(&self) -> i64 {
        self.id
    }
    fn generation(&self) -> String {
        hex(&self.generation_digest)
    }
    /// Whether a stored (hex) generation is this photo's current one, without building the string.
    fn generation_is(&self, stored: &str) -> bool {
        hex_is(&self.generation_digest, stored)
    }
    fn shown(&self) -> bool {
        self.shown
    }
    fn sha(&self) -> Option<&str> {
        self.sha.as_ref().and_then(|s| std::str::from_utf8(s).ok())
    }
    fn is_eligible(&self) -> bool {
        self.eligible
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
/// Typed binary encoding for hashing: a tag, then a fixed-size payload for numbers and a length-prefixed
/// payload for text and blobs, so no two different value sequences encode alike. Never format DB
/// values via Debug/log.
fn encode_ref(out: &mut Vec<u8>, v: ValueRef<'_>) {
    match v {
        ValueRef::Null => out.push(0),
        ValueRef::Integer(i) => {
            out.push(1);
            out.extend_from_slice(&i.to_le_bytes());
        }
        ValueRef::Real(n) => {
            out.push(2);
            out.extend_from_slice(&n.to_bits().to_le_bytes());
        }
        ValueRef::Text(t) => {
            out.push(3);
            out.extend_from_slice(&(t.len() as u64).to_le_bytes());
            out.extend_from_slice(t);
        }
        ValueRef::Blob(b) => {
            out.push(4);
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
    }
}
fn finish32(ctx: ring::digest::Context) -> [u8; 32] {
    let mut out = [0; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}
/// `stored` is the lower-case hex of `bytes`.
fn hex_is(bytes: &[u8], stored: &str) -> bool {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let stored = stored.as_bytes();
    stored.len() == bytes.len() * 2
        && bytes.iter().enumerate().all(|(i, b)| {
            stored[2 * i] == DIGITS[usize::from(b >> 4)]
                && stored[2 * i + 1] == DIGITS[usize::from(b & 15)]
        })
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 15)] as char);
    }
    out
}
fn row_digest(values: &[Value]) -> String {
    let mut encoded = Vec::new();
    for v in values {
        encode_ref(&mut encoded, ValueRef::from(v));
    }
    hex(ring::digest::digest(&ring::digest::SHA256, &encoded).as_ref())
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
/// A photo that holds a local-result reference but is in no component of the plan that proposes to drop
/// it, with the state of the photo (its row, its sha's facts and its vector bytes) the plan saw.
#[derive(Clone)]
pub(super) struct Retire {
    pub(super) photo_id: i64,
    pub(super) result_id: String,
    guard: [u8; 32],
}
type Sha = [u8; 64];
/// What the scene guard needs to know about one `content_sha256`, computed once per read.
struct ShaPart {
    sha: Sha,
    /// Rows whose photo is eligible for this sha (the duplicates of its generation), as a range of
    /// [`Raw::sha_rows`] in photo id order.
    members: std::ops::Range<usize>,
    /// A vector row exists for the sha in the active space (its BLOB is not looked at here).
    vector: bool,
    /// Fixed reason code when the current generation's embedding job has failed.
    failed: Option<&'static str>,
    digest: [u8; 32],
}
impl ShaPart {
    fn sha(&self) -> &str {
        std::str::from_utf8(&self.sha).unwrap_or_default()
    }
}
fn to_sha(s: &str) -> Option<Sha> {
    Sha::try_from(s.as_bytes()).ok()
}
pub(super) struct Raw {
    identity: SpaceIdentity,
    identity_fingerprint: String,
    /// In photo id order.
    rows: Vec<Row>,
    /// `(captured seconds, row index)` ascending; the rows with a valid capture time.
    timed: Vec<(i64, usize)>,
    legacy_by_id: HashMap<i64, Vec<usize>>,
    /// result id -> the photos whose reference names it, ascending.
    pointers: HashMap<String, Vec<i64>>,
    shas: Vec<ShaPart>,
    /// Vector BLOBs are loaded only for the cycle's snapshot and the writer's CAS.
    vectors: HashMap<String, Vector>,
    with_vectors: bool,
    legacy: Vec<Legacy>,
    /// photo -> result of its reference. Only the cycle and the writer need it (reads use `pointers`).
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
    fn sha_part(&self, row: &Row) -> Option<&ShaPart> {
        row.sha_ix.map(|i| &self.shas[i])
    }
    fn row(&self, id: i64) -> Option<&Row> {
        self.rows
            .binary_search_by_key(&id, |r| r.id)
            .ok()
            .map(|i| &self.rows[i])
    }
    /// Rows whose capture time lies in `lo..=hi`.
    fn in_interval(&self, lo: i64, hi: i64) -> &[(i64, usize)] {
        let start = self.timed.partition_point(|(t, _)| *t < lo);
        let end = self.timed.partition_point(|(t, _)| *t <= hi);
        &self.timed[start..end.max(start)]
    }
    /// Only the component's influence interval and its SHA dependencies participate.
    /// Changes in other time windows cannot starve a completed component. A SHA contributes its
    /// duplicate generations (also outside the interval), whether a vector row exists for it, and a
    /// failed embedding of the current generation. No vector BLOB is read for this.
    pub(super) fn guard(&self, lo: i64, hi: i64) -> String {
        match self.guard_digest(lo, hi) {
            Some(digest) => hex(&digest),
            None => fingerprint(&["invalid_scene_interval"]),
        }
    }
    /// Whether `stored` (hex) is the current guard of the interval, without building the string.
    fn guard_is(&self, lo: i64, hi: i64, stored: &str) -> bool {
        self.guard_digest(lo, hi)
            .is_some_and(|digest| hex_is(&digest, stored))
    }
    fn guard_digest(&self, lo: i64, hi: i64) -> Option<[u8; 32]> {
        if lo > hi {
            return None;
        }
        let mut rows: Vec<_> = self
            .in_interval(lo, hi)
            .iter()
            .map(|(_, i)| &self.rows[*i])
            .collect();
        rows.sort_unstable_by_key(|r| r.id());
        let mut shas: Vec<_> = rows.iter().filter_map(|r| r.sha_ix).collect();
        shas.sort_unstable_by(|a, b| self.shas[*a].sha.cmp(&self.shas[*b].sha));
        shas.dedup();
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        ctx.update(b"scene-input-v2");
        ctx.update(&(self.identity_fingerprint.len() as u64).to_le_bytes());
        ctx.update(self.identity_fingerprint.as_bytes());
        ctx.update(&(rows.len() as u64).to_le_bytes());
        for row in &rows {
            ctx.update(&row.digest);
        }
        ctx.update(&(shas.len() as u64).to_le_bytes());
        for ix in shas {
            ctx.update(&self.shas[ix].digest);
        }
        let matches: BTreeSet<_> = rows
            .iter()
            .flat_map(|r| {
                self.legacy_by_id
                    .get(&r.id())
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect();
        ctx.update(&(matches.len() as u64).to_le_bytes());
        for i in matches {
            let digest = &self.legacy[i].origin.digest;
            ctx.update(&(digest.len() as u64).to_le_bytes());
            ctx.update(digest.as_bytes());
        }
        Some(finish32(ctx))
    }
    /// Digest of the vector BLOBs of the same interval. Only meaningful when the vectors were loaded
    /// (the cycle and the writer); read paths validate with [`Raw::guard`] alone.
    pub(super) fn vector_guard(&self, lo: i64, hi: i64) -> String {
        debug_assert!(self.with_vectors);
        let shas: BTreeSet<_> = self
            .in_interval(lo, hi)
            .iter()
            .filter_map(|(_, i)| self.rows[*i].sha())
            .collect();
        let mut parts = vec!["scene-vectors-v1"];
        for sha in shas {
            parts.push(sha);
            parts.push(self.vectors.get(sha).map_or("missing", |v| &v.digest));
        }
        fingerprint(&parts)
    }
    /// Digest of everything that decides whether one photo can belong to a component: its row and
    /// quality, its sha's facts (duplicates, vector row, failed job), the vector bytes. Equal digests
    /// mean the photo is in the same readiness state. Needs the vectors (the cycle and the writer).
    fn photo_guard(&self, id: i64) -> [u8; 32] {
        debug_assert!(self.with_vectors);
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        ctx.update(b"scene-photo-v1");
        match self.row(id) {
            None => ctx.update(&[0]),
            Some(row) => {
                ctx.update(&[1]);
                ctx.update(&row.digest);
                match self.sha_part(row) {
                    Some(part) => {
                        ctx.update(&part.digest);
                        let vector = self
                            .vectors
                            .get(part.sha())
                            .map_or("missing", |v| &v.digest);
                        ctx.update(&(vector.len() as u64).to_le_bytes());
                        ctx.update(vector.as_bytes());
                    }
                    None => ctx.update(&[0; 32]),
                }
            }
        }
        finish32(ctx)
    }
    /// References of photos that no component of the plan contains, in photo id order.
    pub(super) fn retire_candidates(&self, planned: &HashSet<i64>) -> Vec<Retire> {
        self.active
            .iter()
            .filter(|(id, _)| !planned.contains(id))
            .map(|(id, result)| Retire {
                photo_id: *id,
                result_id: result.clone(),
                guard: self.photo_guard(*id),
            })
            .collect()
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

/// The eligible duplicates of one sha, as the embedding job's generation key wants them. Only needed
/// for a sha that has a failed job, so the columns are read here instead of being kept for every row.
fn eligible_members(
    conn: &Connection,
    part: &ShaPart,
    rows: &[Row],
    sha_rows: &[usize],
) -> Result<Vec<index::Eligible>, Error> {
    let mut out = Vec::new();
    for &i in &sha_rows[part.members.clone()] {
        let id = rows[i].id;
        let member = conn
            .query_row(
                "SELECT display,md5,mtime,rotation FROM photos WHERE id=?1",
                [id],
                |r| {
                    Ok(index::Eligible {
                        photo_id: id,
                        input_sha256: part.sha().to_string(),
                        display: r.get(0)?,
                        md5: r.get(1)?,
                        mtime: r.get(2)?,
                        rotation: r.get(3)?,
                    })
                },
            )
            .optional()?;
        out.extend(member);
    }
    Ok(out)
}

fn raw_in(conn: &Connection, with_vectors: bool) -> Result<Option<Raw>, Error> {
    let (_, identity) = index::active_in(conn)?;
    let Some(identity) = identity else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(&format!("{PHOTO_ROWS} ORDER BY p.id"))?;
    let mut scratch = Vec::new();
    let mut rows = stmt
        .query_map([], |r| Row::from_sql(r, &mut scratch, with_vectors))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    // One entry per distinct valid sha, in first-seen (photo id) order; eligible duplicates grouped.
    let mut shas: Vec<ShaPart> = Vec::new();
    let mut by_sha: HashMap<Sha, usize> = HashMap::new();
    let mut eligible_rows: Vec<(usize, usize)> = Vec::new();
    for (i, row) in rows.iter_mut().enumerate() {
        let Some(sha) = row.sha else { continue };
        let ix = *by_sha.entry(sha).or_insert_with(|| {
            shas.push(ShaPart {
                sha,
                members: 0..0,
                vector: false,
                failed: None,
                digest: [0; 32],
            });
            shas.len() - 1
        });
        row.sha_ix = Some(ix);
        if row.is_eligible() {
            eligible_rows.push((ix, i));
        }
    }
    eligible_rows.sort_unstable();
    let sha_rows: Vec<usize> = eligible_rows.iter().map(|(_, i)| *i).collect();
    let mut at = 0;
    for chunk in eligible_rows.chunk_by(|a, b| a.0 == b.0) {
        shas[chunk[0].0].members = at..at + chunk.len();
        at += chunk.len();
    }
    // Existence of a vector row per sha. Reads take it from the primary key alone; the cycle and
    // the writer load the BLOBs (same rows, so the same answer).
    let mut vectors = HashMap::new();
    if jobs::object_exists(conn, "table", "ai_embedding_vectors")? {
        if with_vectors {
            let mut stmt = conn.prepare(
                "/* scene-vectors */ SELECT input_sha256,dimension,vector FROM ai_embedding_vectors WHERE space_id=?1",
            )?;
            let mut found = stmt.query([identity.space_id()])?;
            while let Some(row) = found.next()? {
                let Value::Text(sha) = row.get::<_, Value>(0)? else {
                    continue;
                };
                let Some(&ix) = to_sha(&sha).and_then(|k| by_sha.get(&k)) else {
                    continue;
                };
                shas[ix].vector = true;
                let values = vec![row.get(1)?, row.get(2)?];
                let digest = row_digest(&values);
                vectors.insert(sha, Vector { values, digest });
            }
        } else {
            let mut stmt = conn.prepare(
                "/* scene-vector-keys */ SELECT input_sha256 FROM ai_embedding_vectors WHERE space_id=?1",
            )?;
            let mut found = stmt.query([identity.space_id()])?;
            while let Some(row) = found.next()? {
                let key = match row.get_ref(0)? {
                    ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(to_sha),
                    _ => None,
                };
                if let Some(&ix) = key.and_then(|k| by_sha.get(&k)) {
                    shas[ix].vector = true;
                }
            }
        }
    }
    // A failed job counts when its key is the current generation of its sha. The generation key is
    // only built for shas that have a failed job at all.
    if jobs::object_exists(conn, "table", "ai_jobs")? {
        let prefix = format!("{}\u{1f}", identity.space_id());
        let mut stmt = conn.prepare("SELECT target_key,input_hash,last_error_code FROM ai_jobs WHERE kind='image_embedding' AND state='failed' AND processor_version=?1 ORDER BY target_key,input_hash")?;
        let mut found = stmt.query([index::image_embedding_processor_version(&identity)])?;
        while let Some(row) = found.next()? {
            let (target, input_hash, reason): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let Some(&ix) = target
                .strip_prefix(&prefix)
                .and_then(to_sha)
                .and_then(|k| by_sha.get(&k))
            else {
                continue;
            };
            let members = eligible_members(conn, &shas[ix], &rows, &sha_rows)?;
            let key = index::image_embedding_generation_key(&identity, shas[ix].sha(), &members);
            if key.target_key == target && key.input_hash == input_hash {
                shas[ix].failed = Some(index::reason_code(&reason));
            }
        }
    }
    for part in &mut shas {
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        ctx.update(b"scene-sha-v1");
        ctx.update(&part.sha);
        ctx.update(&(part.members.len() as u64).to_le_bytes());
        for &i in &sha_rows[part.members.clone()] {
            ctx.update(&rows[i].generation_digest);
        }
        ctx.update(&[u8::from(part.vector)]);
        let failed = part.failed.unwrap_or_default();
        ctx.update(&(failed.len() as u64).to_le_bytes());
        ctx.update(failed.as_bytes());
        part.digest = finish32(ctx);
    }
    let legacy = legacy_in(conn)?;
    let mut active = BTreeMap::new();
    let mut pointers = HashMap::<String, Vec<i64>>::new();
    if jobs::object_exists(conn, "table", "ai_scene_active")? {
        let mut stmt =
            conn.prepare("SELECT photo_id,result_id FROM ai_scene_active ORDER BY photo_id")?;
        let mut found = stmt.query([])?;
        while let Some(row) = found.next()? {
            let (id, result): (i64, String) = (row.get(0)?, row.get(1)?);
            match pointers.get_mut(&result) {
                Some(ids) => ids.push(id),
                None => {
                    pointers.insert(result.clone(), vec![id]);
                }
            }
            if with_vectors {
                active.insert(id, result);
            }
        }
    }
    let timed = {
        let mut timed: Vec<(i64, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.captured.map(|t| (t, i)))
            .collect();
        timed.sort_unstable();
        timed
    };
    let mut legacy_by_id = HashMap::<i64, Vec<usize>>::new();
    for (i, g) in legacy.iter().enumerate() {
        for id in &g.ids {
            legacy_by_id.entry(*id).or_default().push(i);
        }
    }
    Ok(Some(Raw {
        identity_fingerprint: identity.fingerprint(),
        identity,
        rows,
        timed,
        legacy_by_id,
        pointers,
        shas,
        vectors,
        with_vectors,
        legacy,
        active,
    }))
}

fn legacy_in(conn: &Connection) -> Result<Vec<Legacy>, Error> {
    // 人の代表の表があるときは、`shown` も有効な推奨で判定する（フィードと同じ意味にそろえる）
    let valid = crate::selection::valid_members(crate::curation::available(conn)?);
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
        } else if !row.is_eligible() {
            if text(&v[19]).is_some_and(|e| !e.is_empty()) {
                Readiness::FailedQuality
            } else {
                Readiness::PendingQuality
            }
        } else if vectors.is_some() && metrics.is_some() {
            Readiness::Ready
        } else {
            let part = raw.sha_part(row).unwrap();
            if vectors.is_none() && part.failed.is_some() {
                Readiness::FailedEmbedding
            } else if metrics.is_none() {
                Readiness::PendingQuality
            } else if part.vector {
                Readiness::CorruptVector
            } else {
                Readiness::PendingEmbedding
            }
        };
        let generation = row.generation().to_string();
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
            favorite: row.favorite,
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
    if saved == Saved::Stored {
        tidy(&tx, plan, &[group_index], &[])?;
    }
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
    save_batch_counted(conn, plan, indexes, stopped).map(|(saved, _)| saved)
}

/// [`save_batch`] that also reports how many references of photos outside the plan were dropped.
///
/// The same transaction drops the reference of every photo that holds one but is in no component of the
/// plan (a vector that was deleted or damaged, a failed embedding, a lost capture time, a deleted
/// photo ...): such a photo has no local result, and nothing else would ever remove its reference when
/// the cycle stores no component. The reference is dropped only while the photo is in the state the plan
/// saw (its row, its sha's facts and its vector bytes) and the reference still names the result the plan
/// saw; a photo that became ready meanwhile keeps it for the next cycle to decide. Only references to
/// results of the plan's space and options are dropped; ranked results and the headers stay as history.
/// `indexes` may be empty: the batch then only drops references.
pub fn save_batch_counted(
    conn: &mut Connection,
    plan: &Plan,
    indexes: &[usize],
    stopped: &dyn Fn() -> bool,
) -> Result<(Vec<Saved>, usize), Error> {
    if indexes.len() > super::runtime::GROUPS_PER_CYCLE {
        return Err(Error::InvalidOptions);
    }
    let interrupted = || Ok((vec![Saved::Interrupted; indexes.len()], 0));
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if stopped() {
        return interrupted();
    }
    let Some(raw) = raw_in(&tx, true)? else {
        return Ok((vec![Saved::Stale; indexes.len()], 0));
    };
    let mut saved = Vec::new();
    for &i in indexes {
        let result = save_in(&tx, &raw, plan, i, stopped)?;
        if result == Saved::Interrupted {
            return interrupted();
        }
        saved.push(result);
    }
    let Some(released) = release_in(&tx, &raw, plan, stopped)? else {
        return interrupted();
    };
    let stored: Vec<_> = indexes
        .iter()
        .zip(&saved)
        .filter(|(_, s)| **s == Saved::Stored)
        .map(|(i, _)| *i)
        .collect();
    tidy(&tx, plan, &stored, &released)?;
    if stopped() {
        return interrupted();
    }
    tx.commit()?;
    Ok((saved, released.len()))
}

/// A reference may be dropped unless its header exists and belongs to another space or option set.
const RETIRABLE: &str = "NOT EXISTS(SELECT 1 FROM ai_scene_groups g WHERE g.result_id=?2 AND NOT (g.space_fingerprint=?3 AND g.window_s=?4 AND g.similarity=?5 AND g.shortlist=?6))";

fn plan_params(plan: &Plan, photo: i64, result: &str) -> Vec<Value> {
    vec![
        photo.into(),
        result.to_string().into(),
        plan.identity.fingerprint().into(),
        plan.options.window_s.into(),
        plan.options.similarity.into(),
        (plan.options.shortlist as i64).into(),
    ]
}

/// Drops the references the plan retires that are still valid to drop (see [`save_batch_counted`]).
/// `None` when stopped. Returns the photos whose reference was dropped.
fn release_in(
    tx: &Connection,
    raw: &Raw,
    plan: &Plan,
    stopped: &dyn Fn() -> bool,
) -> Result<Option<Vec<i64>>, Error> {
    let mut released = Vec::new();
    if raw.identity != plan.identity {
        return Ok(Some(released));
    }
    let mut delete = tx.prepare_cached(&format!(
        "DELETE FROM ai_scene_active WHERE photo_id=?1 AND result_id=?2 AND {RETIRABLE}"
    ))?;
    for (n, retire) in plan.retire.iter().enumerate() {
        if n % 256 == 0 && stopped() {
            return Ok(None);
        }
        if raw.active.get(&retire.photo_id) != Some(&retire.result_id)
            || raw.photo_guard(retire.photo_id) != retire.guard
        {
            continue;
        }
        let params = plan_params(plan, retire.photo_id, &retire.result_id);
        if delete.execute(rusqlite::params_from_iter(params))? > 0 {
            released.push(retire.photo_id);
        }
    }
    Ok(Some(released))
}

/// How many references the plan retires still exist (and are of this plan's space and options): the
/// cycle writes for them even when it stores no component, and for nothing else. Writer-free.
pub fn pending_retire(conn: &Connection, plan: &Plan) -> Result<usize, Error> {
    if plan.retire.is_empty() {
        return Ok(0);
    }
    if !ready_or_mismatch(conn)? {
        return Err(Error::Database);
    }
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(&format!(
        "SELECT 1 FROM ai_scene_active WHERE photo_id=?1 AND result_id=?2 AND {RETIRABLE}"
    ))?;
    let mut count = 0;
    for retire in &plan.retire {
        let params = plan_params(plan, retire.photo_id, &retire.result_id);
        if stmt.exists(rusqlite::params_from_iter(params))? {
            count += 1;
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok(count)
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
    let expected = plan.options.group_processor_version(
        &plan.identity,
        group.rank_used,
        group.weight_digest.as_deref(),
    );
    if !valid_group(raw, group, plan.options, &expected)? {
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
    if stored_members_match && references_match(raw, group) {
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
        } else {
            // A waiting component has no local result: no earlier result stays referenced for its photos.
            tx.execute(
                "DELETE FROM ai_scene_active WHERE photo_id=?1",
                [m.photo_id],
            )?;
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
        ids.len() == group.members.len()
            && group
                .members
                .iter()
                .all(|m| ids.binary_search(&m.photo_id).is_ok())
    })
}
/// The component's references are as the cycle writes them: exactly its members point at a ranked
/// result; a waiting component has none (its photos reference no local result).
fn references_match(raw: &Raw, group: &Group) -> bool {
    if group.state == State::Waiting {
        group
            .members
            .iter()
            .all(|m| !raw.active.contains_key(&m.photo_id))
    } else {
        raw_pointers_match(raw, group)
    }
}

/// Housekeeping in the transaction of the stored components and of the released references. It removes
/// only derived placeholders and references, never a ranked result and nothing of another space or
/// option set:
/// * a waiting header of the same space and options that shares a photo with a stored component or a
///   released reference (the photo now has a newer statement), or that names a photo that no longer
///   exists, with its members;
/// * references of photos that no longer exist.
///
/// Ranked results that a newer result replaced stay as history; they no longer count because the
/// references of the replaced members point at the newer result.
fn tidy(tx: &Connection, plan: &Plan, stored: &[usize], released: &[i64]) -> Result<(), Error> {
    if stored.is_empty() && released.is_empty() {
        return Ok(());
    }
    let kept: Vec<&str> = stored
        .iter()
        .map(|&i| plan.groups[i].result_id.as_str())
        .collect();
    let touched: BTreeSet<i64> = stored
        .iter()
        .flat_map(|&i| plan.groups[i].members.iter().map(|m| m.photo_id))
        .chain(released.iter().copied())
        .collect();
    let options = plan.options;
    // One statement over the waiting headers of this space and options (a placeholder is stale when it
    // shares a photo with what this transaction decided or names a photo that is gone).
    let stale = tx
        .prepare(
            "SELECT g.result_id FROM ai_scene_groups g
              WHERE g.state='waiting' AND g.space_fingerprint=?1 AND g.window_s=?2 AND g.similarity=?3
                AND g.shortlist=?4 AND g.result_id NOT IN (SELECT value FROM json_each(?5))
                AND EXISTS(SELECT 1 FROM ai_scene_members m WHERE m.result_id=g.result_id
                      AND (m.photo_id IN (SELECT value FROM json_each(?6))
                           OR NOT EXISTS(SELECT 1 FROM photos p WHERE p.id=m.photo_id)))",
        )?
        .query_map(
            params![
                plan.identity.fingerprint(),
                options.window_s,
                options.similarity,
                options.shortlist as i64,
                encode(&kept)?,
                encode(&touched)?
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !stale.is_empty() {
        let stale = encode(&stale)?;
        tx.execute(
            "DELETE FROM ai_scene_members WHERE result_id IN (SELECT value FROM json_each(?1))",
            [&stale],
        )?;
        tx.execute(
            "DELETE FROM ai_scene_groups WHERE result_id IN (SELECT value FROM json_each(?1))",
            [&stale],
        )?;
    }
    tx.execute(
        "DELETE FROM ai_scene_active WHERE NOT EXISTS(SELECT 1 FROM photos p WHERE p.id=ai_scene_active.photo_id)",
        [],
    )?;
    Ok(())
}

/// Writer-free skip check against the computed plan; includes every header/member and pointer.
/// Never trusts result IDs alone. A mismatch is repaired only through the usual writer CAS.
pub fn pending_saves(conn: &Connection, plan: &Plan, limit: usize) -> Result<Vec<usize>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if !ready_or_mismatch(conn)? {
        return Err(Error::Database);
    }
    let tx = conn.unchecked_transaction()?;
    let mut pointers = HashMap::<String, BTreeSet<i64>>::new();
    let mut referenced = BTreeSet::<i64>::new();
    for row in tx
        .prepare("SELECT result_id,photo_id FROM ai_scene_active")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
    {
        let (result, id) = row?;
        referenced.insert(id);
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
            && if g.state == State::Waiting {
                g.members.iter().all(|m| !referenced.contains(&m.photo_id))
            } else {
                pointers.get(&g.result_id).is_some_and(|ids| {
                    ids.len() == g.members.len()
                        && g.members.iter().all(|m| ids.contains(&m.photo_id))
                })
            };
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
    let mut stmt = conn.prepare_cached(
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

/// One `ai_scene_members` row as selected by [`read_all_members`] (columns 1..=8), read in place.
fn member_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Member> {
    let wrong = |i: usize, v: ValueRef<'_>| {
        rusqlite::Error::InvalidColumnType(i, String::new(), v.data_type())
    };
    let integer = |i: usize| match r.get_ref(i)? {
        ValueRef::Integer(n) => Ok(n),
        v => Err(wrong(i, v)),
    };
    let generation = match r.get_ref(2)? {
        ValueRef::Text(t) => std::str::from_utf8(t)
            .map_err(|_| wrong(2, ValueRef::Text(t)))?
            .to_string(),
        v => return Err(wrong(2, v)),
    };
    let score = match r.get_ref(4)? {
        ValueRef::Null => None,
        ValueRef::Real(n) => Some(n),
        ValueRef::Integer(n) => Some(n as f64),
        v => return Err(wrong(4, v)),
    };
    Ok(Member {
        photo_id: integer(1)?,
        generation,
        rank: integer(3)? as usize,
        score,
        shortlisted: flag(r, 5)?,
        representative: flag(r, 6)?,
        shown: flag(r, 7)?,
        favorite: flag(r, 8)?,
    })
}

/// Members of every stored component of the space (waiting ones only when asked), in one pass.
/// A damaged row makes only its own component `CorruptResult`; an SQL failure fails the read.
fn read_all_members(
    conn: &Connection,
    fingerprint: &str,
    include_waiting: bool,
    processors: &[&str],
) -> Result<HashMap<String, Result<Vec<Member>, Error>>, Error> {
    let mut stmt = conn.prepare(&format!(
        "SELECT m.result_id,m.photo_id,m.generation,m.rank,m.score,m.shortlisted,m.representative,m.shown,m.favorite
           FROM ai_scene_groups g JOIN ai_scene_members m ON m.result_id=g.result_id
          WHERE g.space_fingerprint=?1 AND (?2 OR g.state!='waiting') AND g.processor_version IN ({})
          ORDER BY m.result_id,m.photo_id",
        marks_from(3, processors.len())
    ))?;
    let mut out: HashMap<String, Result<Vec<Member>, Error>> = HashMap::new();
    let mut current: Option<(String, Result<Vec<Member>, Error>)> = None;
    let mut values: Vec<Value> = vec![
        fingerprint.to_string().into(),
        Value::Integer(i64::from(include_waiting)),
    ];
    values.extend(processors.iter().map(|p| Value::Text(p.to_string())));
    let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
    while let Some(r) = rows.next()? {
        let same = match (&current, r.get_ref(0)) {
            (Some((id, _)), Ok(value)) => value.as_str().is_ok_and(|v| v == id),
            _ => false,
        };
        if !same {
            if let Some((id, members)) = current.take() {
                out.insert(id, members);
            }
            current = Some((
                r.get(0).map_err(stored_row_error)?,
                Ok(Vec::with_capacity(4)),
            ));
        }
        let member = member_row(r);
        let Some((_, slot)) = current.as_mut() else {
            continue;
        };
        match member {
            Ok(member) => {
                if let Ok(members) = slot {
                    members.push(member);
                }
            }
            Err(e) => match stored_row_error(e) {
                Error::Database => return Err(Error::Database),
                e => *slot = Err(e),
            },
        }
    }
    if let Some((id, members)) = current.take() {
        out.insert(id, members);
    }
    for members in out.values_mut().filter_map(|m| m.as_mut().ok()) {
        members.sort_by_key(|m| m.rank);
    }
    Ok(out)
}

fn stored_row_error(error: rusqlite::Error) -> Error {
    if error.sqlite_error_code().is_some() {
        Error::Database
    } else {
        Error::CorruptResult
    }
}

/// Shared by current reads and the save fast path; all members must describe one complete result.
fn valid_group(
    raw: &Raw,
    group: &Group,
    options: Options,
    expected_processor: &str,
) -> Result<bool, Error> {
    if group.members.is_empty()
        || (matches!(group.rank_used, RankUsed::Linear | RankUsed::Fallback(_))
            != group.weight_digest.is_some())
        || group
            .weight_digest
            .as_deref()
            .is_some_and(|d| !valid_sha(d))
        || group.processor_version != expected_processor
    {
        return Ok(false);
    }
    let mut members: Vec<&Member> = group.members.iter().collect();
    members.sort_unstable_by_key(|m| m.photo_id);
    if members.windows(2).any(|w| w[0].photo_id == w[1].photo_id) {
        return Ok(false);
    }
    let waiting = group.state == State::Waiting;
    let (mut lo, mut hi) = (i64::MAX, i64::MIN);
    for (i, m) in group.members.iter().enumerate() {
        let Some(row) = raw.row(m.photo_id) else {
            return Ok(false);
        };
        let Some(time) = row.captured else {
            return Ok(false);
        };
        lo = lo.min(time);
        hi = hi.max(time);
        if !row.generation_is(&m.generation)
            || m.rank != i + 1
            || m.shown != row.shown()
            || m.favorite != row.favorite
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
            .iter()
            .map(|m| m.generation.as_str())
            .collect::<Vec<_>>(),
    );
    if group.input_generation != generation
        || group.result_id
            != fingerprint(&["scene-result-v1", &group.processor_version, &generation])
    {
        return Ok(false);
    }
    let ids: Vec<_> = members.iter().map(|m| m.photo_id).collect();
    // Most components touch no old selection: nothing to classify or compare then.
    let (overlap, legacy) = if ids.iter().any(|id| raw.legacy_by_id.contains_key(id)) {
        raw.overlap(&ids)
    } else {
        (Overlap::Disjoint, Vec::new())
    };
    Ok(group.overlap == overlap
        && if legacy.is_empty() {
            group.legacy.is_empty()
        } else {
            encode(&group.legacy)? == encode(&legacy)?
        }
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
/// The processor version that stored components with these ranking inputs must carry (derived once per
/// distinct set of inputs per read), with the parsed ranking it came from.
struct ExpectedProcessor {
    rank_key: String,
    weight_digest: Option<String>,
    window_s: i64,
    similarity_bits: u64,
    shortlist: usize,
    version: String,
    rank_used: RankUsed,
}

pub fn load_projection_in(
    tx: &Connection,
    processors: &[&str],
    include_waiting: bool,
) -> Result<Loaded, Error> {
    // No vector BLOB is read here: validation uses rows, SHAs, vector rows' existence, failed jobs, legacy
    // state and the stored guard. Scene tables that are missing or of another shape yield no groups.
    let (_, identity) = index::active_in(tx)?;
    let Some(identity) = identity else {
        return Ok(Loaded {
            identity: None,
            groups: Vec::new(),
        });
    };
    if !ready(tx)? {
        return Ok(Loaded {
            identity: Some(identity),
            groups: Vec::new(),
        });
    }
    let identity_fingerprint = identity.fingerprint();
    // No stored component of this space and these processors: nothing to validate, so the photo scan
    // is skipped. Components of other processors (history) are neither read nor counted in the cost.
    let query_params = |extra: &[&str]| {
        let mut values: Vec<Value> = vec![
            identity_fingerprint.clone().into(),
            Value::Integer(i64::from(include_waiting)),
        ];
        values.extend(extra.iter().map(|p| Value::Text(p.to_string())));
        values
    };
    let processor_marks = marks_from(3, processors.len());
    let any: bool = !processors.is_empty()
        && tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM ai_scene_groups WHERE space_fingerprint=?1 AND (?2 OR state!='waiting') AND processor_version IN ({processor_marks}))"),
            rusqlite::params_from_iter(query_params(processors)),
            |r| r.get(0),
        )?;
    if !any {
        return Ok(Loaded {
            identity: Some(identity),
            groups: Vec::new(),
        });
    }
    let Some(raw) = raw_in(tx, false)? else {
        return Ok(Loaded {
            identity: None,
            groups: Vec::new(),
        });
    };
    let mut all_members = read_all_members(tx, &identity_fingerprint, include_waiting, processors)?;
    // (rank_used, weight digest, window, similarity bits, shortlist) -> expected processor version.
    let mut expected_processors: Vec<ExpectedProcessor> = Vec::new();
    let mut groups = Vec::new();
    // Rows are read in place: the strings of a component that is not used are never copied.
    let mut stmt = tx.prepare(&format!(
        "SELECT result_id,input_generation,processor_version,state,overlap,rank_used,lo,hi,input_guard,weight_digest,legacy_json,window_s,similarity,shortlist,space_id,vector_guard FROM ai_scene_groups
          WHERE space_fingerprint=?1 AND (?2 OR state!='waiting') AND processor_version IN ({processor_marks})
          ORDER BY result_id"
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(query_params(processors)))?;
    while let Some(r) = rows.next()? {
        let str_at = |i: usize| -> Result<&str, Error> {
            r.get_ref(i)
                .map_err(stored_row_error)?
                .as_str()
                .map_err(|_| Error::CorruptResult)
        };
        let int_at = |i: usize| r.get::<_, i64>(i).map_err(stored_row_error);
        let (lo, hi) = (int_at(6)?, int_at(7)?);
        if lo > hi {
            return Err(Error::CorruptResult);
        }
        let (result_id, state, processor_version, guard) =
            (str_at(0)?, str_at(3)?, str_at(2)?, str_at(8)?);
        if (state != "waiting" && !raw.pointers.contains_key(result_id))
            || !processors.contains(&processor_version)
            || !raw.guard_is(lo, hi, guard)
        {
            continue;
        }
        let state = match state {
            "waiting" => State::Waiting,
            "local_ranked" => State::LocalRanked,
            "review_overlap" => State::ReviewOverlap,
            _ => return Err(Error::CorruptResult),
        };
        let overlap = match str_at(4)? {
            "disjoint" => Overlap::Disjoint,
            "exact" => Overlap::Exact,
            "partial" => Overlap::Partial,
            _ => return Err(Error::CorruptResult),
        };
        let rank_key = str_at(5)?;
        let legacy_json = str_at(10)?;
        let legacy = if legacy_json == "[]" {
            Vec::new()
        } else {
            serde_json::from_str(legacy_json).map_err(|_| Error::CorruptResult)?
        };
        let weight_digest = match r.get_ref(9).map_err(stored_row_error)? {
            ValueRef::Null => None,
            ValueRef::Text(t) => Some(
                std::str::from_utf8(t)
                    .map_err(|_| Error::CorruptResult)?
                    .to_string(),
            ),
            _ => return Err(Error::CorruptResult),
        };
        let (window_s, similarity) = (int_at(11)?, r.get::<_, f64>(12).map_err(stored_row_error)?);
        let shortlist = usize::try_from(int_at(13)?).map_err(|_| Error::CorruptResult)?;
        let options =
            Options::new(window_s, similarity, shortlist).map_err(|_| Error::CorruptResult)?;
        let known = expected_processors.iter().position(|e| {
            e.rank_key == rank_key
                && e.weight_digest == weight_digest
                && e.window_s == window_s
                && e.similarity_bits == similarity.to_bits()
                && e.shortlist == shortlist
        });
        let rank_used = match known {
            Some(i) => expected_processors[i].rank_used,
            None => serde_json::from_str(rank_key).map_err(|_| Error::CorruptResult)?,
        };
        let members = all_members.remove(result_id).unwrap_or(Ok(Vec::new()))?;
        let group = Group {
            result_id: result_id.to_string(),
            input_generation: str_at(1)?.to_string(),
            processor_version: processor_version.to_string(),
            state,
            overlap,
            rank_used,
            members,
            legacy,
            lo,
            hi,
            guard: guard.to_string(),
            vector_guard: str_at(15)?.to_string(),
            weight_digest,
            expected_active: String::new(),
        };
        // The expected processor depends only on these inputs; derive it once per distinct set.
        let known = match known {
            Some(i) => i,
            None => {
                let version = options.group_processor_version(
                    &raw.identity,
                    group.rank_used,
                    group.weight_digest.as_deref(),
                );
                expected_processors.push(ExpectedProcessor {
                    rank_key: rank_key.to_string(),
                    weight_digest: group.weight_digest.clone(),
                    window_s,
                    similarity_bits: similarity.to_bits(),
                    shortlist,
                    version,
                    rank_used: group.rank_used,
                });
                expected_processors.len() - 1
            }
        };
        let expected = &expected_processors[known].version;
        if str_at(14)? != raw.identity.space_id() || !valid_group(&raw, &group, options, expected)?
        {
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

/// `?first,?first+1,...` for the processor list.
fn marks_from(first: usize, count: usize) -> String {
    (0..count)
        .map(|i| format!("?{}", first + i))
        .collect::<Vec<_>>()
        .join(",")
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

/// Header counts for the status views: no photo row, vector or member payload is read.
/// A header counts only while it is current by what the cycle writes: every member photo still exists
/// and can still belong to a component (it has a display, a capture time, its quality features and a
/// vector row of the active space: all primary-key lookups) and, unless it is waiting, every member's
/// active reference names this result. The cycle removes the references a newer result supersedes, the
/// waiting placeholder it replaces and the references of photos that are not ready any more (see
/// [`tidy`], [`save_batch_counted`]); the count does not wait for it. The per-component input guard is
/// not recomputed (advisory): a result whose inputs changed in another way counts until the next cycle
/// replaces it.
pub fn header_counts_in(tx: &Connection, processors: &[&str]) -> Result<HeaderCounts, Error> {
    let (_, identity) = index::active_in(tx)?;
    let Some(identity) = identity else {
        return Ok((None, Vec::new()));
    };
    if processors.is_empty()
        || !ready_or_mismatch(tx)?
        || !jobs::object_exists(tx, "table", "ai_embedding_vectors")?
    {
        return Ok((Some(identity), Vec::new()));
    }
    let mut stmt = tx.prepare(&format!(
        "SELECT state,rank_used,COUNT(*) FROM ai_scene_groups g
          WHERE space_fingerprint=?1 AND processor_version IN ({})
            AND EXISTS(SELECT 1 FROM ai_scene_members m WHERE m.result_id=g.result_id)
            AND NOT EXISTS(SELECT 1 FROM ai_scene_members m WHERE m.result_id=g.result_id
              AND (NOT EXISTS(SELECT 1 FROM photos p
                       JOIN photo_quality_features f ON f.photo_id=p.id AND f.display=p.display
                        AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime AND f.error=''
                       JOIN ai_embedding_vectors v ON v.space_id=?{space} AND v.input_sha256=f.content_sha256
                      WHERE p.id=m.photo_id AND p.display!='' AND p.taken_at!='')
                OR (g.state!='waiting' AND NOT EXISTS(SELECT 1 FROM ai_scene_active a
                      WHERE a.photo_id=m.photo_id AND a.result_id=g.result_id))))
          GROUP BY state,rank_used",
        processors_sql(processors),
        space = processors.len() + 2
    ))?;
    let params = std::iter::once(identity.fingerprint())
        .chain(processors.iter().map(|p| p.to_string()))
        .chain(std::iter::once(identity.space_id().to_string()));
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

/// Current input generation (the hex stored with each member) of the given photos only. Photos that
/// do not exist are absent. Reads those rows alone (no vector, no other photo), so a writer-side
/// check of a few photos does not grow with the catalog.
pub fn generations_in(tx: &Connection, ids: &[i64]) -> Result<HashMap<i64, String>, Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = (1..=ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = tx.prepare(&format!("{PHOTO_ROWS} WHERE p.id IN ({marks})"))?;
    let mut scratch = Vec::new();
    let rows = stmt.query_map(rusqlite::params_from_iter(ids), |r| {
        Row::from_sql(r, &mut scratch, false)
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let row = row?;
        out.insert(row.id(), row.generation());
    }
    Ok(out)
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
    if ids.is_empty() || processors.is_empty() || !ready(tx)? {
        return Ok((Some(identity), HashMap::new()));
    }
    let marks = |from: usize| {
        (0..ids.len())
            .map(|i| format!("?{}", from + i))
            .collect::<Vec<_>>()
            .join(",")
    };
    // Only photos that can still belong to a component: a capture time, quality features and a vector
    // row of the active space (the same facts the cycle's readiness rests on, without the BLOB).
    let mut candidates: Vec<(i64, String, String)> = Vec::new();
    let mut stmt = tx.prepare(&format!("{PHOTO_ROWS} WHERE p.id IN ({})", marks(1)))?;
    let mut scratch = Vec::new();
    let found = stmt.query_map(rusqlite::params_from_iter(ids), |r| {
        Row::from_sql(r, &mut scratch, false)
    })?;
    for row in found {
        let row = row?;
        if let (Some(_), true, Some(sha)) = (row.captured, row.is_eligible(), row.sha()) {
            candidates.push((row.id(), row.generation(), sha.to_string()));
        }
    }
    drop(stmt);
    let mut generations = HashMap::new();
    if !candidates.is_empty() && jobs::object_exists(tx, "table", "ai_embedding_vectors")? {
        let mut shas: Vec<&str> = candidates.iter().map(|c| c.2.as_str()).collect();
        shas.sort_unstable();
        shas.dedup();
        let mut stmt = tx.prepare(&format!(
            "SELECT input_sha256 FROM ai_embedding_vectors WHERE space_id=?1 AND input_sha256 IN ({})",
            marks_from(2, shas.len())
        ))?;
        let params = std::iter::once(identity.space_id()).chain(shas.iter().copied());
        let present = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        for (id, generation, sha) in candidates {
            if present.contains(&sha) {
                generations.insert(id, generation);
            }
        }
    }
    let first = 2 + processors.len();
    // Ranked members are found through the photo's reference (primary key) and waiting ones through
    // the few waiting headers; the members table has no index by photo and is never scanned.
    let columns =
        "m.photo_id,m.generation,g.state,g.overlap,m.rank,m.score,m.representative,m.shortlisted";
    let mut stmt = tx.prepare(&format!(
        "SELECT * FROM (
           SELECT {columns},g.result_id AS result_id FROM ai_scene_active a
             JOIN ai_scene_members m ON m.result_id=a.result_id AND m.photo_id=a.photo_id
             JOIN ai_scene_groups g ON g.result_id=m.result_id
            WHERE a.photo_id IN ({ids}) AND g.space_fingerprint=?1 AND g.processor_version IN ({processors})
           UNION ALL
           SELECT {columns},g.result_id AS result_id FROM ai_scene_groups g
             JOIN ai_scene_members m ON m.result_id=g.result_id
            WHERE g.state='waiting' AND g.space_fingerprint=?1 AND g.processor_version IN ({processors})
              AND m.photo_id IN ({ids}))
          ORDER BY photo_id,(state='waiting'),result_id",
        ids = marks(first),
        processors = processors_sql(processors)
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
