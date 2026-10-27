//! Local scene scheduling is independent of Provider routing, discovery, lanes and paid budgets.
use super::{rank, *};
use crate::{config::Config, stop::StopFlag, writer::Writer};
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
        let mut members = 0;
        indexes.retain(|&i| {
            let n = plan.groups()[i].members().len();
            if members != 0 && members + n > MEMBERS_PER_CYCLE {
                return false;
            }
            members += n;
            true
        });
        if indexes.is_empty() || cancelled() {
            return Ok(report);
        }
        report.selected = indexes.len();
        report.members = members;
        let queued = Instant::now();
        let stop = stop.clone();
        let result = writer
            .call_within(deadline, move |c| {
                let began = Instant::now();
                let result = save_batch(c, &plan, &indexes, &|| {
                    stop.is_stopped() || Instant::now() >= deadline
                });
                Ok((result, began.duration_since(queued), began.elapsed()))
            })
            .map_err(|_| Error::Database)?;
        report.stored = result
            .0?
            .into_iter()
            .filter(|s| *s == Saved::Stored)
            .count();
        report.writer_wait = result.1;
        report.writer_save = result.2;
        Ok(report)
    }
}

/// Rank-file I/O occurs before entering the API read transaction. The identity is compared
/// again inside that transaction: a concurrent space switch cannot use these processors.
pub struct Prepared {
    identity: Option<SpaceIdentity>,
    processors: Vec<String>,
    fallback: Option<std::collections::HashMap<String, String>>,
}
impl Prepared {
    pub fn read(conn: &Connection, config: &Config) -> Result<Self, Error> {
        let identity = crate::ai::index::active_in(conn)?.1;
        let mut processors = Vec::new();
        let mut fallback = None;
        if config.ai.invalid().is_none()
            && let Some(identity) = &identity
        {
            let model = rank::Model::load(&config.rank_model_path, identity);
            processors.push(
                config
                    .ai
                    .scene_options()
                    .processor_version(identity, model.as_ref().ok().map(|m| m.digest())),
            );
            if let Ok(model) = &model {
                let rules = config.ai.scene_options().processor_version(identity, None);
                // A model may fall back for one component. Verify that fallback with current
                // immutable inputs outside any transaction; never accept an old rules result
                // merely because its processor predates the newly installed weight.
                let tx = conn.unchecked_transaction()?;
                let candidates = super::repository::load_projection_in(&tx, &[&rules], true)?;
                tx.commit()?;
                let mut allowed = std::collections::HashMap::new();
                if !candidates.groups().is_empty()
                    && let Some(snapshot) = snapshot(conn)?
                {
                    let photos: std::collections::HashMap<_, _> =
                        snapshot.photos.iter().map(|p| (p.id, p)).collect();
                    for group in candidates.groups() {
                        if snapshot.identity() != identity
                            || snapshot.raw.guard(group.lo, group.hi) != group.guard
                        {
                            continue;
                        }
                        let inputs: Option<Vec<_>> = group
                            .members()
                            .iter()
                            .map(|m| {
                                let p = photos.get(&m.photo_id)?;
                                Some((p.metrics?, p.vector.as_ref()?.as_slice()))
                            })
                            .collect();
                        if let Some(inputs) = inputs
                            && let Err(reason) = model.scores(identity, &inputs)
                            && group.rank_used() == RankUsed::Rules(Some(reason))
                        {
                            allowed.insert(group.result_id.clone(), group.guard.clone());
                        }
                    }
                }
                processors.push(rules);
                fallback = Some(allowed);
            }
        }
        Ok(Self {
            identity,
            processors,
            fallback,
        })
    }
    pub fn load_in(&self, conn: &Connection) -> Result<Loaded, Error> {
        let mut loaded = super::repository::load_projection_in(
            conn,
            &self
                .processors
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            true,
        )?;
        if loaded.identity() != self.identity.as_ref() {
            return Ok(Loaded {
                identity: loaded.identity,
                groups: Vec::new(),
            });
        }
        if let Some(allowed) = &self.fallback {
            loaded.groups.retain(|g| {
                matches!(g.rank_used(), RankUsed::Linear)
                    || allowed.get(g.result_id()) == Some(&g.guard)
            });
        }
        Ok(loaded)
    }
}

/// Fixed codes and counts only; local ranking never increments paid-judgment counts.
pub fn summary(loaded: Result<&Loaded, &Error>) -> serde_json::Value {
    let Ok(loaded) = loaded else {
        return serde_json::json!({"state":"unavailable","local_ranked":0,"waiting":0,"review_overlap":0,"rules":0,"linear":0});
    };
    let count = |state| {
        loaded
            .groups()
            .iter()
            .filter(|g| g.state() == state)
            .count()
    };
    serde_json::json!({"state":if loaded.identity().is_some(){"active"}else{"no_index"},
        "local_ranked":count(State::LocalRanked),"waiting":count(State::Waiting),"review_overlap":count(State::ReviewOverlap),
        "rules":loaded.groups().iter().filter(|g| matches!(g.rank_used(),RankUsed::Rules(_))).count(),
        "linear":loaded.groups().iter().filter(|g| g.rank_used()==RankUsed::Linear).count()})
}
