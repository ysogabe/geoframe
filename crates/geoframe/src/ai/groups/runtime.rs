//! Local scene scheduling is independent of Provider routing, discovery, lanes and paid budgets.
use super::{rank, *};
use crate::{
    config::Config,
    stop::StopFlag,
    writer::{WriteError, Writer},
};
use rusqlite::Connection;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub const GROUPS_PER_CYCLE: usize = 16;
const MEMBERS_PER_CYCLE: usize = 8192;
const CYCLE_TIME: Duration = Duration::from_secs(10);

pub fn enabled(config: &Config) -> bool {
    config.ingest && config.ai.legacy().enabled && config.ai.invalid().is_none()
}

#[derive(Debug, Default)]
pub struct CycleReport {
    pub stored: usize,
    /// References of photos outside the plan (not ready any more) that this cycle dropped.
    pub retired: usize,
    pub selected: usize,
    pub members: usize,
    pub snapshot: Duration,
    pub compute: Duration,
    pub writer_wait: Duration,
    pub writer_save: Duration,
}

/// Production factory. No client, image lock or external-I/O dependency is constructed.
pub struct Runtime {
    config: Arc<Config>,
}
impl Runtime {
    pub fn production(config: Arc<Config>) -> Option<Self> {
        enabled(&config).then_some(Self { config })
    }
    pub fn cycle(
        &self,
        read: &Connection,
        writer: &Writer,
        stop: &StopFlag,
    ) -> Result<usize, Error> {
        Ok(self.cycle_report(read, writer, stop)?.stored)
    }
    pub fn cycle_report(
        &self,
        read: &Connection,
        writer: &Writer,
        stop: &StopFlag,
    ) -> Result<CycleReport, Error> {
        self.cycle_report_until(read, writer, stop, Instant::now() + CYCLE_TIME)
    }
    pub(super) fn cycle_report_until(
        &self,
        read: &Connection,
        writer: &Writer,
        stop: &StopFlag,
        deadline: Instant,
    ) -> Result<CycleReport, Error> {
        let start = Instant::now();
        let mut report = CycleReport::default();
        let cancelled = || stop.is_stopped() || Instant::now() >= deadline;
        if cancelled() {
            return Ok(report);
        }
        let Some(snapshot) = snapshot(read)? else {
            return Ok(report);
        };
        report.snapshot = start.elapsed();
        let computing = Instant::now();
        let model = rank::Model::load(&self.config.rank_model_path, snapshot.identity());
        let Some(plan) = compute(
            &snapshot,
            self.config.ai.scene_options(),
            model.as_ref().map_err(|e| *e),
            &cancelled,
        ) else {
            return Ok(report);
        };
        report.compute = computing.elapsed();
        let mut indexes = pending_saves(read, &plan, GROUPS_PER_CYCLE)?;
        let retire = pending_retire(read, &plan)?;
        let mut members = 0;
        indexes.retain(|&i| {
            let n = plan.groups()[i].members().len();
            if members != 0 && members + n > MEMBERS_PER_CYCLE {
                return false;
            }
            members += n;
            true
        });
        // A cycle with nothing to store still writes when a reference of a photo that is not ready any
        // more exists; with neither, it writes nothing.
        if (indexes.is_empty() && retire == 0) || cancelled() {
            return Ok(report);
        }
        report.selected = indexes.len();
        report.members = members;
        let queued = Instant::now();
        let stop = stop.clone();
        let result = writer
            .call_within(deadline, move |c| {
                let began = Instant::now();
                let result = save_batch_counted(c, &plan, &indexes, &|| {
                    stop.is_stopped() || Instant::now() >= deadline
                });
                Ok((result, began.duration_since(queued), began.elapsed()))
            })
            .map_err(|e| match e {
                WriteError::Expired => Error::WriterBusy,
                _ => Error::Database,
            })?;
        let (saved, retired) = result.0?;
        report.stored = saved.into_iter().filter(|s| *s == Saved::Stored).count();
        report.retired = retired;
        report.writer_wait = result.1;
        report.writer_save = result.2;
        Ok(report)
    }
}

/// Rank-file I/O occurs before entering the API read transaction. The identity is compared
/// again inside that transaction: a concurrent space switch cannot use these processors.
///
/// Read paths never recompute a ranking. Whether a stored component used the installed weight,
/// or is a fixed-rule fallback of that same weight, is decided by the cycle and recorded in the
/// component's processor version (the weight digest is part of it); a changed or removed weight
/// therefore stops matching until the next cycle replaces the result.
pub struct Prepared {
    identity: Option<SpaceIdentity>,
    processors: Vec<String>,
}
impl Prepared {
    pub fn read(conn: &Connection, config: &Config) -> Result<Self, Error> {
        let identity = crate::ai::index::active_in(conn)?.1;
        let mut processors = Vec::new();
        if config.ai.invalid().is_none()
            && let Some(identity) = &identity
        {
            let options = config.ai.scene_options();
            match rank::Model::load(&config.rank_model_path, identity) {
                Ok(model) => {
                    processors.push(options.processor_version(identity, Some(model.digest())));
                    processors.push(options.fallback_processor_version(identity, model.digest()));
                }
                Err(_) => processors.push(options.processor_version(identity, None)),
            }
        }
        Ok(Self {
            identity,
            processors,
        })
    }
    fn processors(&self) -> Vec<&str> {
        self.processors.iter().map(String::as_str).collect()
    }
    /// Validated projection (per-component input guard). Used by the search view.
    pub fn load_in(&self, conn: &Connection) -> Result<Loaded, Error> {
        let loaded = super::repository::load_projection_in(conn, &self.processors(), true)?;
        if loaded.identity() != self.identity.as_ref() {
            return Ok(Loaded {
                identity: loaded.identity,
                groups: Vec::new(),
            });
        }
        Ok(loaded)
    }
    /// Counts from the stored headers only (status views).
    pub fn summary_in(&self, conn: &Connection) -> serde_json::Value {
        let counts = super::repository::header_counts_in(conn, &self.processors());
        let mut summary = summary(counts.as_ref().map_err(|e| *e), self.identity.as_ref());
        // 保存済みの Gemini 比較の件数（履歴の総数。用途が無効でも過去の回答は残る）。
        if let Some(map) = summary.as_object_mut()
            && let Ok(n) = crate::ai::scene_judge::stored_count(conn)
        {
            map.insert("paid_judgments".into(), n.into());
        }
        summary
    }
    /// Stored positions of one page of photos (management list).
    pub fn annotations_in(
        &self,
        conn: &Connection,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Annotation>, Error> {
        let (identity, found) = super::repository::annotations_in(conn, &self.processors(), ids)?;
        Ok(if identity == self.identity {
            found
        } else {
            Default::default()
        })
    }
}

/// Status counts for a prepared (or failed-to-prepare) read.
pub fn status_summary(prepared: &Result<Prepared, Error>, conn: &Connection) -> serde_json::Value {
    match prepared {
        Ok(prepared) => prepared.summary_in(conn),
        Err(_) => summary(Err(Error::Database), None),
    }
}

/// Fixed codes and counts only; local ranking never increments paid-judgment counts.
/// A space switched after `Prepared::read` yields zero counts, as for the validated projection.
fn summary(
    counts: Result<&super::repository::HeaderCounts, Error>,
    prepared: Option<&SpaceIdentity>,
) -> serde_json::Value {
    let Ok((identity, groups)) = counts else {
        return serde_json::json!({"state":"unavailable","local_ranked":0,"waiting":0,"review_overlap":0,"rules":0,"linear":0});
    };
    let groups: &[(State, RankUsed, i64)] = if identity.as_ref() == prepared {
        groups
    } else {
        &[]
    };
    let count = |state| {
        groups
            .iter()
            .filter(|g| g.0 == state)
            .map(|g| g.2)
            .sum::<i64>()
    };
    let by_rank = |rank: fn(&RankUsed) -> bool| {
        groups
            .iter()
            .filter(|g| rank(&g.1))
            .map(|g| g.2)
            .sum::<i64>()
    };
    serde_json::json!({"state":if identity.is_some(){"active"}else{"no_index"},
        "local_ranked":count(State::LocalRanked),"waiting":count(State::Waiting),"review_overlap":count(State::ReviewOverlap),
        "rules":by_rank(|r| matches!(r,RankUsed::Rules(_)|RankUsed::Fallback(_))),
        "linear":by_rank(|r| *r==RankUsed::Linear)})
}
