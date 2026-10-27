//! Production background image embedding. One fresh JPEG and one inference per job.
//! Content vectors are shared by SHA; jobs additionally bind every current member generation.
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{Arc, Mutex};

use super::activation::{self, BuildSnapshot};
use super::embedding::EmbeddingClient;
use super::image_input::{ImageProfile, PreparedImage};
use super::index::{self, Eligible, PreparedVector, ShaState};
use super::jobs::{self, Claimed, ErrorCode, InputCheck, JobKey, JobKind, Recheck};
use super::location_clue::read_image_source;
use super::space::SpaceIdentity;
use super::worker::{Handler, Maintenance, Persist, RunCtx, RunError};
use crate::{datadir::DataDir, imaging};

pub struct ImageEmbedding {
    data: DataDir,
    client: EmbeddingClient,
    snapshot: Arc<Mutex<Option<BuildSnapshot>>>,
}
impl ImageEmbedding {
    pub fn new(data: DataDir, client: EmbeddingClient) -> Self {
        Self {
            data,
            client,
            snapshot: Arc::new(Mutex::new(None)),
        }
    }
    fn identity(&self) -> &SpaceIdentity {
        self.client.identity()
    }
    fn states(&self, read: &Connection) -> rusqlite::Result<Vec<ShaState>> {
        if !read.is_autocommit() {
            return index::scan(read, Some(self.identity()), false);
        }
        let tx = read.unchecked_transaction()?;
        let result = index::scan(&tx, Some(self.identity()), false)?;
        tx.commit()?;
        Ok(result)
    }
    fn current(&self, read: &Connection, key: &JobKey) -> rusqlite::Result<Option<ShaState>> {
        Ok(self
            .states(read)?
            .into_iter()
            .find(|s| s.job_key(self.identity()) == *key))
    }
}
fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn db_error(_: rusqlite::Error) -> RunError {
    RunError::Attempt(ErrorCode::Internal)
}
fn current_member(conn: &Connection, member: &Eligible) -> rusqlite::Result<bool> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 {} WHERE {} AND p.id=?1 AND f.content_sha256=?2
        AND p.display=?3 AND p.md5 IS ?4 AND p.mtime IS ?5 AND p.rotation IS ?6)",
        index::ELIGIBLE_JOIN,
        index::ELIGIBLE_WHERE
    );
    conn.query_row(
        &sql,
        params![
            member.photo_id,
            member.input_sha256,
            member.display,
            member.md5,
            member.mtime,
            member.rotation
        ],
        |r| r.get(0),
    )
}
fn persist(
    member: Eligible,
    identity: SpaceIdentity,
    vector: Option<PreparedVector>,
    now: i64,
) -> Persist {
    Box::new(move |conn| {
        if !current_member(conn, &member)? {
            return Ok(InputCheck::Changed);
        }
        if let Some(vector) = vector {
            index::store_prepared_in(conn, &vector, now)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        } else {
            // Reused vector might have been damaged after the read. Do not succeed without it.
            let valid = index::scan(conn, Some(&identity), false)?
                .iter()
                .any(|s| s.input_sha256() == member.input_sha256 && s.has_vector);
            if !valid {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(InputCheck::Current)
    })
}
impl Handler for ImageEmbedding {
    fn kind(&self) -> JobKind {
        JobKind::ImageEmbedding
    }
    fn claim_scope(&self) -> jobs::ClaimScope {
        jobs::ClaimScope {
            kind: JobKind::ImageEmbedding,
            target_prefix: Some(format!("{}\u{1f}", self.identity().space_id())),
            processor_version: Some(index::image_embedding_processor_version(self.identity())),
        }
    }
    fn reconcile(&self, read: &Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        // Cleanup owns enqueue/history transfer. A generation appearing after cleanup must
        // wait for the next cycle rather than slip through the generic enqueue path.
        let mut keys = Vec::new();
        for state in self.states(read)? {
            let key = state.job_key(self.identity());
            let exists: bool = read.query_row("SELECT EXISTS(SELECT 1 FROM ai_jobs
                WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2 AND processor_version=?3
                AND state!='obsolete')", params![key.target_key,key.input_hash,key.processor_version], |r|r.get(0))?;
            if exists {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    fn reconcile_cleanup(&self, read: &Connection, now: i64) -> rusqlite::Result<Vec<Persist>> {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if snapshot.as_ref().is_none_or(BuildSnapshot::is_empty) {
            *snapshot = Some(activation::snapshot_targets(read, self.identity())?);
        }
        let identity = self.identity().clone();
        Ok(vec![Box::new(move |conn| {
            index::register_space_in(conn, &identity, now)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            let states = index::scan(conn, Some(&identity), false)?;
            let keys: Vec<_> = states.iter().map(|s| s.job_key(&identity)).collect();
            // Transfer failure/interruption history before generic enqueue can obsolete it.
            // Never clear terminal, counters or a provider wait by changing only DB generation.
            for (s, key) in states.iter().zip(&keys) {
                let history: Option<(i64,i64,i64,bool,i64,String)> = conn.query_row(
                    "SELECT attempts,defer_count,interruption_count,terminal,next_attempt_at,last_error_code
                     FROM ai_jobs WHERE kind='image_embedding' AND target_key=?1 AND processor_version=?2
                     ORDER BY (state='obsolete'),updated_at DESC,id DESC LIMIT 1", params![key.target_key,key.processor_version],
                     |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
                let (attempts, defers, interruptions, terminal, next, code) =
                    history.unwrap_or_default();
                let previous: Option<String> = conn.query_row("SELECT state FROM ai_jobs
                    WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2 AND processor_version=?3",
                    params![key.target_key,key.input_hash,key.processor_version], |r|r.get(0)).optional()?;
                jobs::enqueue_in(conn, key, now)?;
                if previous.as_deref().is_none_or(|s| s == "obsolete") && !s.has_vector {
                    conn.execute("UPDATE ai_jobs SET attempts=?4,interruption_count=?5,terminal=?6,defer_count=?8,
                        next_attempt_at=MAX(next_attempt_at,?7), state=CASE WHEN ?6 THEN 'failed' ELSE 'pending' END,
                        last_error_code=?9
                        WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2 AND processor_version=?3",
                        params![key.target_key,key.input_hash,key.processor_version,attempts,interruptions,terminal,next,defers,code])?;
                }
                if !s.has_vector {
                    // Repair only a demonstrably missing/corrupt result. Successful keys with valid
                    // vectors are never reset. Preserve counters and all terminal limits.
                    conn.execute("UPDATE ai_jobs SET state=CASE WHEN terminal THEN 'failed' ELSE 'pending' END,
                        claim_token=NULL,updated_at=?4 WHERE kind='image_embedding' AND state='succeeded'
                        AND target_key=?1 AND input_hash=?2 AND processor_version=?3",
                        params![key.target_key,key.input_hash,key.processor_version,now])?;
                }
            }
            let prefix = format!("{}\u{1f}", identity.space_id());
            let old = conn.prepare("SELECT target_key,input_hash,processor_version FROM ai_jobs
                WHERE kind='image_embedding' AND state IN ('pending','running','deferred','failed')")?
                .query_map([], |r| Ok(JobKey::new(JobKind::ImageEmbedding,r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for key in old
                .into_iter()
                .filter(|k| k.target_key.starts_with(&prefix) && !keys.contains(k))
            {
                conn.execute("UPDATE ai_jobs SET state='obsolete',claim_token=NULL,updated_at=?4
                    WHERE kind='image_embedding' AND target_key=?1 AND input_hash=?2 AND processor_version=?3
                    AND state IN ('pending','running','deferred','failed')",
                    params![key.target_key,key.input_hash,key.processor_version,now])?;
            }
            Ok(InputCheck::Current)
        })])
    }
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        ctx.before_probe()?;
        if super::embedding::configured_identity(
            ctx.config(),
            super::settings::TaskKind::ImageEmbedding,
        )
        .as_ref()
            != Some(self.identity())
        {
            return Err(RunError::PolicyDisabled);
        }
        let state = self
            .current(ctx.read(), &job.key)
            .map_err(db_error)?
            .ok_or(RunError::InputChanged)?;
        // A missing/broken member must not block its duplicate: choose an actually matching source.
        let mut source = None;
        let mut error = RunError::Permanent(ErrorCode::InputInvalid);
        for m in &state.members {
            match read_image_source(&self.data, &m.display) {
                Ok(bytes) if sha(&bytes) == m.input_sha256 => {
                    source = Some((m.clone(), bytes));
                    break;
                }
                Ok(_) => error = RunError::InputChanged,
                Err(e) if error != RunError::InputChanged => error = e,
                Err(_) => {}
            }
        }
        let Some((member, bytes)) = source else {
            if self
                .current(ctx.read(), &job.key)
                .map_err(db_error)?
                .is_none()
            {
                return Err(RunError::InputChanged);
            }
            return Err(error);
        };
        let unchanged = || {
            if !current_member(ctx.read(), &member).map_err(db_error)?
                || read_image_source(&self.data, &member.display)
                    .map_or(true, |b| sha(&b) != member.input_sha256)
            {
                return Err(RunError::InputChanged);
            }
            Ok(())
        };
        if state.has_vector {
            unchanged()?;
            return Ok(persist(member, self.identity().clone(), None, ctx.now()));
        }
        let rgb =
            imaging::prepare_embedding_rgb(&bytes, !member.display.starts_with("derivatives/"))
                .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
        drop(bytes);
        let image = PreparedImage::encode_jpeg_rgb(
            &rgb.pixels,
            rgb.width as u16,
            rgb.height as u16,
            82,
            ImageProfile::Embedding,
        )
        .map_err(|_| RunError::Permanent(ErrorCode::InputInvalid))?;
        let sent_sha = image.sha256();
        let values = self.client.image(ctx, &image, unchanged)?;
        unchanged()?;
        let vector = PreparedVector::new(self.identity(), &member.input_sha256, &sent_sha, &values)
            .map_err(|_| RunError::Attempt(ErrorCode::BadResponse))?;
        Ok(persist(
            member,
            self.identity().clone(),
            Some(vector),
            ctx.now(),
        ))
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        Ok(match self.current(conn, key)? {
            None => Recheck::Obsolete,
            Some(s) if s.has_vector => Recheck::Satisfied,
            Some(_) => Recheck::Requeue,
        })
    }
    fn maintenance(&self, _read: &Connection, _now: i64) -> rusqlite::Result<Option<Maintenance>> {
        let snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let (identity, shared) = (self.identity().clone(), self.snapshot.clone());
        Ok(Some(Box::new(move |conn| {
            let result = activation::try_activate_default(conn, &snapshot, &identity)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if result.counts().valid == 0 {
                *shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            Ok(())
        })))
    }
}
pub(super) fn handler(data: DataDir, client: EmbeddingClient) -> Arc<dyn Handler> {
    Arc::new(ImageEmbedding::new(data, client))
}

#[cfg(test)]
pub(crate) mod tests;
