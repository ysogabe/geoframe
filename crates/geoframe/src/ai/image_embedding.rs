//! Production background image embedding. One fresh JPEG and one inference per job.
//! Content vectors are shared by SHA; jobs additionally bind every current member generation.
use rusqlite::{Connection, params};
use std::collections::HashSet;
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
        let prefix = format!("{}\u{1f}", self.identity().space_id());
        let Some(sha) = key.target_key.strip_prefix(&prefix) else {
            return Ok(None);
        };
        Ok(index::scan_sha(read, self.identity(), sha)?
            .filter(|s| s.job_key(self.identity()) == *key))
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
            let valid = index::scan_sha(conn, &identity, &member.input_sha256)?
                .is_some_and(|s| s.has_vector);
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
    fn reconcile(&self, _read: &Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        // Cleanup enqueues and supersedes atomically with the lineage pointer. There is
        // deliberately no later generic enqueue based on a potentially stale read snapshot.
        Ok(Vec::new())
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
        let prefix = format!("{}\u{1f}", identity.space_id());
        let processor = index::image_embedding_processor_version(&identity);
        // Plan only on the read connection. Each action is one content target, so the
        // worker's ENQUEUE_CHUNK bounds actual writer work and permits stopping between chunks.
        let mut targets: HashSet<String> = self
            .states(read)?
            .into_iter()
            .map(|s| s.input_sha256().to_string())
            .collect();
        for target in read
            .prepare(
                "SELECT DISTINCT target_key FROM ai_jobs
            WHERE kind='image_embedding' AND processor_version=?1
            AND substr(target_key,1,length(?2))=?2
            AND state IN ('pending','running','deferred','failed')",
            )?
            .query_map(params![processor, prefix], |r| r.get::<_, String>(0))?
        {
            if let Some(sha) = target?.strip_prefix(&prefix) {
                targets.insert(sha.to_string());
            }
        }
        let mut targets: Vec<_> = targets.into_iter().collect();
        targets.sort();
        let mut actions: Vec<Persist> = Vec::with_capacity(targets.len() + 1);
        let register = identity.clone();
        actions.push(Box::new(move |conn| {
            index::register_space_in(conn, &register, now)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(InputCheck::Current)
        }));
        for sha in targets {
            let identity = identity.clone();
            actions.push(Box::new(move |conn| {
                // Fresh indexed SHA read inside this chunk's transaction: membership and
                // result corruption can change after planning or between writer chunks.
                if let Some(state) = index::scan_sha(conn,&identity,&sha)? {
                    let key = state.job_key(&identity);
                    jobs::enqueue_image_lineage_in(conn,&key,now)?;
                    if !state.has_vector {
                        conn.execute("UPDATE ai_jobs SET state=CASE WHEN terminal THEN 'failed' ELSE 'pending' END,
                            claim_token=NULL,updated_at=?4 WHERE kind='image_embedding' AND state='succeeded'
                            AND target_key=?1 AND input_hash=?2 AND processor_version=?3",
                            params![key.target_key,key.input_hash,key.processor_version,now])?;
                    }
                } else {
                    conn.execute("UPDATE ai_jobs SET state='obsolete',claim_token=NULL,updated_at=?3
                        WHERE kind='image_embedding' AND target_key=?1 AND processor_version=?2
                        AND state IN ('pending','running','deferred','failed')",
                        params![format!("{}\u{1f}{sha}",identity.space_id()),
                            index::image_embedding_processor_version(&identity),now])?;
                }
                Ok(InputCheck::Current)
            }));
        }
        Ok(actions)
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
    fn maintenance(&self, read: &Connection, _now: i64) -> rusqlite::Result<Option<Maintenance>> {
        // The active pointer never needs republishing. Missing vectors with queued work
        // also cannot publish yet. Avoid full writer scans after every job completion.
        // This is only a read-side optimization; the eventual activation still validates
        // all members and vectors atomically in the writer transaction.
        let skip: bool = read.query_row(
            "SELECT CASE WHEN EXISTS(SELECT 1 FROM settings WHERE key='ai_active_space' AND value=?3)
            THEN 1 ELSE EXISTS(SELECT 1 FROM ai_jobs
            WHERE kind='image_embedding' AND processor_version=?1
            AND substr(target_key,1,length(?2))=?2
            AND state IN ('pending','running','deferred')
            AND NOT EXISTS(SELECT 1 FROM ai_embedding_vectors v WHERE v.space_id=?3
                AND v.input_sha256=substr(ai_jobs.target_key,length(?2)+1))) END",
            params![
                index::image_embedding_processor_version(self.identity()),
                format!("{}\u{1f}", self.identity().space_id()),
                self.identity().space_id()
            ],
            |r| r.get(0),
        )?;
        if skip {
            return Ok(None);
        }
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
