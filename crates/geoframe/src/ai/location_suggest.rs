//! 日候補の固定 domain と有償回答 repository。handler/HTTP/座標化は T4.6c2。
//! writer 内では DB のみを読み、画像準備・ファイル観測は呼出し側で行う。
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::cloud::GenerateRequest;
use super::image_input::PreparedImage;
use super::jobs::{JobKey, JobKind};
pub use super::location_clue::SourceObservation;
use super::location_clue::{self, Generation, Readiness, Target};
use crate::datadir::DataDir;

mod answer;
mod repository;
pub use answer::{Answer, Confidence, PhotoAnswer, Precision};
pub use repository::{
    Lookup, Queue, SavedAnswer, complete, enqueue, enqueue_in, lookup, persist_answer,
    project_failure, restore_projection, saved_sent_ids,
};
pub(super) use repository::{SCHEMA, TABLES};
#[cfg(test)]
mod tests;

pub const PROMPT_VERSION: &str = "loc-v2";
pub const PROCESS_VERSION: &str = "location-suggest-v1";
pub const MAX_SEND: usize = 6;
const MAX_MEMBERS: usize = 16_384;
const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub const PROMPT: &str = include_str!("location_suggest/prompt.txt");

pub fn schema() -> serde_json::Value {
    serde_json::from_str(include_str!("location_suggest/schema.json")).expect("fixed schema")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    InvalidInput,
    Pending,
    NoImages,
    BadResponse,
}

fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn encoded<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("bounded domain")
}
fn valid_hash(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn run_id_for(model: &str, sent_ids: &[i64]) -> String {
    run_id_with_prompt(PROMPT_VERSION, model, sent_ids)
}
fn run_id_with_prompt(prompt: &str, model: &str, sent_ids: &[i64]) -> String {
    let mut ids = sent_ids.to_vec();
    ids.sort_unstable();
    let ids = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "loc-{}",
        &sha(format!("{prompt}|{model}|{ids}").as_bytes())[..16]
    )
}
pub fn processor_version(model: &str) -> String {
    sha(format!(
        "{model}\0{PROMPT_VERSION}\0{PROCESS_VERSION}\0{}",
        location_clue::PREPROCESS_VERSION
    )
    .as_bytes())
}

/// cloud 許可範囲の一つの時刻集合。写真情報は Debug に出さない。
#[derive(Clone)]
pub struct Day {
    day: String,
    members: Vec<Generation>,
    sent_ids: Vec<i64>,
    pending: bool,
    gap_s: f64,
    include_hidden: bool,
}
impl std::fmt::Debug for Day {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Day(..)")
    }
}
impl Day {
    pub fn date(&self) -> &str {
        &self.day
    }
    pub fn members(&self) -> &[Generation] {
        &self.members
    }
    pub fn sent_ids(&self) -> &[i64] {
        &self.sent_ids
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
    pub fn sendable(&self) -> bool {
        !self.pending && !self.sent_ids.is_empty()
    }
    /// 短い writer transaction 用。集合への追加/削除/SAVE と全構成員の世代を照合。
    pub fn matches_db(&self, conn: &Connection) -> rusqlite::Result<bool> {
        let groups = db_groups(conn, self.include_hidden, self.gap_s)?;
        let ids: Vec<i64> = self.members.iter().map(Generation::photo_id).collect();
        if !groups
            .iter()
            .any(|g| g[0].1 == self.day && g.iter().map(|p| p.0).eq(ids.iter().copied()))
        {
            return Ok(false);
        }
        for g in &self.members {
            if !g.matches_db(conn)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// 日付文字列は Python と同じ taken_at[:10]（時差の UTC 日へ丸めない）。
fn split<T>(mut rows: Vec<(i64, String, i64, T)>, gap: f64) -> Vec<Vec<(i64, String, i64, T)>> {
    rows.sort_by_key(|p| (p.2, p.0));
    let mut groups: Vec<Vec<(i64, String, i64, T)>> = Vec::new();
    for p in rows {
        if groups
            .last()
            .is_none_or(|g| (p.2 as f64 - g.last().unwrap().2 as f64) > gap || p.1 != g[0].1)
        {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(p);
    }
    groups
}
type DbGroup = Vec<(i64, String, i64, ())>;
fn db_groups(conn: &Connection, hidden: bool, gap: f64) -> rusqlite::Result<Vec<DbGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id,taken_at FROM photos WHERE lat IS NULL AND (?1 OR COALESCE(hidden,0)=0)",
    )?;
    let mut out = Vec::new();
    for r in stmt.query_map([hidden], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (id, t) = r?;
        if let Some(t) = t
            && let Some(e) = crate::photos::parse_epoch(&t)
        {
            out.push((id, t.chars().take(10).collect(), e, ()));
        }
    }
    Ok(split(out, gap))
}

/// load_targets の全写真から hidden を独立除外してから group/pick する。
/// infer_windows_s は Config::infer_windows_s() を渡す。外部 I/O は fallback 観測のみ。
pub fn group_days(
    conn: &Connection,
    data: &DataDir,
    targets: Vec<Target>,
    include_hidden: bool,
    infer_windows_s: &[f64],
) -> Result<Vec<Day>, DomainError> {
    let gap = infer_windows_s
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .max_by(f64::total_cmp)
        .ok_or(DomainError::InvalidInput)?;
    if infer_windows_s.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Err(DomainError::InvalidInput);
    }
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in targets {
        if !seen.insert(t.photo_id) {
            return Err(DomainError::InvalidInput);
        }
        let hidden: bool = conn
            .query_row(
                "SELECT COALESCE(hidden,0)!=0 FROM photos WHERE id=?1",
                [t.photo_id],
                |r| r.get(0),
            )
            .map_err(|_| DomainError::InvalidInput)?;
        if hidden && !include_hidden {
            continue;
        }
        let generation = Generation::observe(conn, data, t.photo_id)
            .map_err(|_| DomainError::InvalidInput)?
            .ok_or(DomainError::InvalidInput)?;
        // A changed target is not promoted to a fresh Ready clue.
        if !generation.matches_target(&t.key) {
            return Err(DomainError::InvalidInput);
        }
        rows.push((
            t.photo_id,
            t.taken_at.chars().take(10).collect(),
            t.epoch,
            (generation, t.readiness),
        ));
    }
    let mut days = Vec::new();
    for group in split(rows, gap) {
        if group.len() > MAX_MEMBERS {
            return Err(DomainError::InvalidInput);
        }
        let pending = group.iter().any(|p| p.3.1 == Readiness::Pending);
        let mut passing: Vec<_> = group
            .iter()
            .filter_map(|p| match &p.3.1 {
                Readiness::Ready(c) if c.passes() => Some((p.0, c)),
                _ => None,
            })
            .collect();
        passing.sort_by(|a, b| {
            (a.1.kind() != "landmark")
                .cmp(&(b.1.kind() != "landmark"))
                .then_with(|| b.1.score().total_cmp(&a.1.score()))
                .then(a.0.cmp(&b.0))
        });
        let sent_ids = passing.into_iter().take(MAX_SEND).map(|p| p.0).collect();
        days.push(Day {
            day: group[0].1.clone(),
            members: group.into_iter().map(|p| p.3.0).collect(),
            sent_ids,
            pending,
            gap_s: gap,
            include_hidden,
        });
    }
    Ok(days)
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Sent {
    photo_id: i64,
    source_sha256: String,
    jpeg_sha256: String,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Manifest {
    day: String,
    model: String,
    prompt_version: String,
    processor_version: String,
    members: Vec<Generation>,
    sent: Vec<Sent>,
}
impl Manifest {
    fn result_key(&self) -> String {
        sha(encoded(self).as_bytes())
    }
    fn sent_ids(&self) -> Vec<i64> {
        self.sent.iter().map(|s| s.photo_id).collect()
    }
    fn run_id(&self) -> String {
        run_id_with_prompt(&self.prompt_version, &self.model, &self.sent_ids())
    }
}

/// c2 が writer 外で source SHA を検査して用意する入力。画像の入口は PreparedImage のみ。
pub struct SendImage {
    pub photo_id: i64,
    pub source_sha256: String,
    pub image: PreparedImage,
}
/// 実送信画像 SHA と番号順まで固定された計画。URL/timeout/policy 全体を同一性へ混ぜない。
#[derive(Clone)]
pub struct Plan {
    day: Day,
    manifest: Manifest,
    request: GenerateRequest,
}
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Plan(..)")
    }
}
impl Plan {
    pub fn new(day: Day, model: &str, images: Vec<SendImage>) -> Result<Self, DomainError> {
        if day.pending {
            return Err(DomainError::Pending);
        }
        if day.sent_ids.is_empty() {
            return Err(DomainError::NoImages);
        }
        if model.is_empty()
            || model.len() > 256
            || model.chars().any(char::is_control)
            || !images
                .iter()
                .map(|i| i.photo_id)
                .eq(day.sent_ids.iter().copied())
        {
            return Err(DomainError::InvalidInput);
        }
        let mut sent = Vec::new();
        let mut numbered = Vec::new();
        for (n, i) in images.into_iter().enumerate() {
            if !valid_hash(&i.source_sha256)
                || !day.members.iter().any(|g| {
                    g.photo_id() == i.photo_id && g.source_sha256() == Some(&i.source_sha256)
                })
            {
                return Err(DomainError::InvalidInput);
            }
            sent.push(Sent {
                photo_id: i.photo_id,
                source_sha256: i.source_sha256,
                jpeg_sha256: i.image.sha256(),
            });
            numbered.push(((n + 1) as u32, i.image));
        }
        let request = GenerateRequest::location_suggest(
            &PROMPT.replace("{date}", &day.day),
            &schema(),
            &numbered,
        )
        .map_err(|_| DomainError::InvalidInput)?;
        let manifest = Manifest {
            day: day.day.clone(),
            model: model.into(),
            prompt_version: PROMPT_VERSION.into(),
            processor_version: processor_version(model),
            members: day.members.clone(),
            sent,
        };
        if encoded(&manifest).len() > MAX_MANIFEST_BYTES {
            return Err(DomainError::InvalidInput);
        }
        Ok(Self {
            day,
            manifest,
            request,
        })
    }
    pub fn key(&self) -> JobKey {
        JobKey::new(
            JobKind::LocationSuggest,
            self.manifest.run_id(),
            self.manifest.result_key(),
            &self.manifest.processor_version,
        )
    }
    pub fn request(&self) -> &GenerateRequest {
        &self.request
    }
    pub fn sent_ids(&self) -> Vec<i64> {
        self.manifest.sent_ids()
    }
    pub fn matches_db(&self, conn: &Connection) -> rusqlite::Result<bool> {
        self.day.matches_db(conn)
    }
}
