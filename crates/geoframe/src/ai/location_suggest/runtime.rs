//! Production orchestration. No HTTP or filesystem access inside Persist callbacks.
use super::*;
use crate::ai::{
    cloud::{CloudClient, CloudClients},
    decision,
    http::Deadline,
    jobs::{self, Claimed, ErrorCode, InputCheck, Recheck},
    provider::LanClient,
    settings::TaskKind,
    worker::{Handler, Persist, RunCtx, RunError},
};
use crate::{
    cache::Cache,
    config::Config,
    geocode::{Geocoder, Outcome},
};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

/// 1 回の reconcile が積む送信候補の最大数。実際に送る数は周期の上限・日の上限・失敗の上限が別に抑える。
const MAX_QUEUE: usize = 32;
const GEOCODE_VERSION: &str = "nominatim-day-photo-coarser-v1";
const HOLDS: &[&str] = &[
    "pending",
    "legacy_unverified",
    "unverified",
    "projection_pending",
    "decision_wait",
    "no_images",
];

struct Shared {
    data: DataDir,
    model: String,
    clue_model: String,
    clue_enabled: bool,
    hidden: bool,
    windows: Vec<f64>,
    cloud: Option<CloudClient>,
    lan: Option<LanClient>,
    geocoder: Arc<Geocoder>,
    cache: Arc<Cache>,
}
impl Shared {
    fn days(&self, conn: &Connection) -> rusqlite::Result<Vec<Day>> {
        group_days(
            conn,
            &self.data,
            location_clue::load_targets(conn, &self.data, &self.clue_model)?,
            self.hidden,
            &self.windows,
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)
    }
    fn observations(
        &self,
        conn: &Connection,
        ids: &[i64],
    ) -> rusqlite::Result<Vec<SourceObservation>> {
        ids.iter()
            .map(|id| SourceObservation::observe(conn, &self.data, *id))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
    }
    fn lookup(
        &self,
        conn: &Connection,
        day: &Day,
    ) -> rusqlite::Result<(Lookup, Vec<SourceObservation>)> {
        let observations = self.observations(conn, &saved_sent_ids(conn, day, &self.model)?)?;
        Ok((lookup(conn, day, &self.model, &observations)?, observations))
    }
    /// 送る直前に 1 枚ずつ画像を作る（読み込み・デコード・縮小・JPEG）。件数は送る日の分だけ。
    fn plan(
        &self,
        conn: &Connection,
        intent: Intent,
        stopping: &dyn Fn() -> bool,
    ) -> Result<Plan, RunError> {
        let mut images = Vec::new();
        for id in intent.day().sent_ids() {
            if stopping() {
                return Err(RunError::Stopping);
            }
            let g = intent
                .day()
                .members()
                .iter()
                .find(|g| g.photo_id() == *id)
                .ok_or(RunError::InputChanged)?;
            images.push(SendImage {
                photo_id: *id,
                source_sha256: g.source_sha256().ok_or(RunError::InputChanged)?.into(),
                image: location_clue::prepare_image(conn, &self.data, g)?,
            });
        }
        intent.plan(images).map_err(|e| {
            RunError::Permanent(match e {
                DomainError::TooLarge => ErrorCode::InputTooLarge,
                _ => ErrorCode::InputInvalid,
            })
        })
    }
    fn check_plan(&self, conn: &Connection, plan: &Plan) -> Result<(), RunError> {
        if !plan.matches_db(conn).map_err(db_error)? {
            return Err(RunError::InputChanged);
        }
        // Re-read Ready/Pending, current rank and exact sent order, not just stored clues.
        let current = self
            .days(conn)
            .map_err(db_error)?
            .into_iter()
            .find(|d| encoded(d) == encoded(&plan.day))
            .ok_or(RunError::InputChanged)?;
        if !current.sendable() {
            return Err(RunError::InputPending);
        }
        let observed = self
            .observations(conn, &plan.sent_ids())
            .map_err(db_error)?;
        if plan.manifest.sent.iter().any(|s| {
            !observed
                .iter()
                .any(|o| o.photo_id() == s.photo_id && plan.day.members.contains(o.generation()))
        }) {
            return Err(RunError::InputChanged);
        }
        match self.lookup(conn, &plan.day).map_err(db_error)?.0 {
            Lookup::Absent => Ok(()),
            _ => Err(RunError::InputChanged),
        }
    }
    fn ready(&self, ctx: &RunCtx<'_>, whole: &Deadline) -> Result<(), RunError> {
        if ctx.is_stopping() {
            return Err(RunError::Stopping);
        }
        if !self.clue_enabled || !ctx.dependency_allowed(TaskKind::LocationClue) {
            return Err(RunError::DependencyWait(ErrorCode::DecisionWait));
        }
        let lan = self
            .lan
            .as_ref()
            .ok_or(RunError::DependencyWait(ErrorCode::DecisionWait))?;
        if whole.remaining().is_none() {
            return Err(RunError::DependencyWait(ErrorCode::DecisionWait));
        }
        lan.check_shared(ctx.config(), TaskKind::LocationClue, ctx.now())
            .map_err(|_| RunError::DependencyWait(ErrorCode::DecisionWait))?;
        let session = lan
            .background(ctx.stop())
            .map_err(|_| RunError::DependencyWait(ErrorCode::DecisionWait))?;
        decision::probe_until(
            &session,
            &self.clue_model,
            &whole.capped(Duration::from_secs(2)),
        )
        .map_err(|e| match e {
            crate::ai::provider::ProviderError::Stopping => RunError::Stopping,
            _ => RunError::DependencyWait(ErrorCode::DecisionWait),
        })
    }
}
fn db_error(_: rusqlite::Error) -> RunError {
    RunError::Attempt(ErrorCode::Internal)
}

pub(in crate::ai) fn handlers(
    config: &Config,
    data: DataDir,
    lan: Option<LanClient>,
    cloud: Option<&CloudClients>,
    geocoder: Arc<Geocoder>,
    cache: Arc<Cache>,
) -> Vec<Arc<dyn Handler>> {
    let Ok(gate) = config.ai.gate(TaskKind::LocationSuggest, true) else {
        return Vec::new();
    };
    let clue = config
        .ai
        .settings()
        .and_then(|s| s.task(TaskKind::LocationClue));
    let shared = Arc::new(Shared {
        data,
        model: gate.settings.model.clone(),
        clue_model: clue.map_or(String::new(), |s| s.model.clone()),
        clue_enabled: config.ai.gate(TaskKind::LocationClue, true).is_ok(),
        hidden: gate.settings.include_hidden,
        windows: config.infer_windows_s(),
        cloud: cloud.and_then(|c| c.client(TaskKind::LocationSuggest)),
        lan,
        geocoder,
        cache,
    });
    vec![
        Arc::new(LocationSuggest {
            shared: shared.clone(),
            work: Mutex::new(Vec::new()),
            retry_saved: Mutex::new(Vec::new()),
        }),
        Arc::new(LocationGeocode {
            shared,
            work: Mutex::new(Vec::new()),
        }),
    ]
}

#[derive(Clone)]
enum Work {
    Send(Box<Intent>),
    Invalid(Day, JobKey, ErrorCode),
}
impl Work {
    fn key(&self) -> JobKey {
        match self {
            Self::Send(i) => i.key(),
            Self::Invalid(_, k, _) => k.clone(),
        }
    }
}
struct LocationSuggest {
    shared: Arc<Shared>,
    work: Mutex<Vec<Work>>,
    retry_saved: Mutex<Vec<GeoPlan>>,
}
impl LocationSuggest {
    /// 送れる日を `Intent`（日・モデル・キー）として集める。**画像は作らない**（読み込み・デコード・縮小は
    /// 送る直前の `run` で 1 件ずつ）。積む数は [`MAX_QUEUE`] まで。`skip_failed` が真のとき、失敗の履歴で止まっている
    /// キーは数えず積まない（外さないと先頭の日が毎周期選ばれ続け、後ろの日がジョブにならない）。
    fn prepare(
        &self,
        read: &Connection,
        now: i64,
        skip_failed: bool,
    ) -> rusqlite::Result<Vec<Persist>> {
        let mut work = Vec::new();
        let mut actions: Vec<Persist> = Vec::new();
        let mut holds = BTreeMap::<&'static str, i64>::new();
        let failed = if skip_failed {
            failed_keys(read)?
        } else {
            HashSet::new()
        };
        // 今も有効な投影（写真 → 結果・日の集合）。これ以外の自分の投影は、下で画面から外す。
        let mut valid = BTreeMap::<i64, (String, String)>::new();
        for day in self.shared.days(read)? {
            let (lookup, observed) = self.shared.lookup(read, &day)?;
            let reason = match lookup {
                Lookup::Reusable(saved) => {
                    let (result, json) = (saved.result_key(), encoded(&day));
                    for g in day.members() {
                        valid.insert(g.photo_id(), (result.clone(), json.clone()));
                    }
                    continue;
                }
                Lookup::ProjectionPending => {
                    let (d, m, o) = (day.clone(), self.shared.model.clone(), observed.clone());
                    actions.push(Box::new(move |tx| {
                        repository::restore_projection_in(tx, &d, &m, &o)?;
                        Ok(InputCheck::Current)
                    }));
                    Some("projection_pending")
                }
                Lookup::LegacyUnverified => Some("legacy_unverified"),
                Lookup::Unverified => Some("unverified"),
                Lookup::Absent if day.pending() => Some("pending"),
                Lookup::Absent if !day.sendable() => Some("no_images"),
                Lookup::Absent if !self.shared.clue_enabled || self.shared.lan.is_none() => {
                    Some("decision_wait")
                }
                Lookup::Absent => None,
            };
            if let Some(reason) = reason {
                *holds.entry(reason).or_default() += 1;
                continue;
            }
            if work.len() >= MAX_QUEUE {
                continue;
            }
            match Intent::new(day.clone(), &self.shared.model) {
                Ok(intent) => {
                    if failed.contains(&key_triple(&intent.key())) {
                        continue;
                    }
                    let p = intent.clone();
                    actions.push(Box::new(move |tx| {
                        enqueue_in(tx, &p, &observed, now)?;
                        Ok(InputCheck::Current)
                    }));
                    work.push(Work::Send(Box::new(intent)));
                }
                Err(_) => {
                    let key = JobKey::new(
                        JobKind::LocationSuggest,
                        run_id_for(&self.shared.model, day.sent_ids()),
                        sha(encoded(&day).as_bytes()),
                        format!("{}-preparation", processor_version(&self.shared.model)),
                    );
                    work.push(Work::Invalid(day, key, ErrorCode::InputInvalid));
                }
            }
        }
        // 画像ファイルだけが差し替わった・消えた場合は、画面の一覧（DB だけで判定する）では分からない。
        // ファイルを観測するこの周期が、有効でなくなった自分の投影を画面の行から外す。候補（`ai_location_candidates`）は
        // 残すので、有効に戻れば座標化の投影が復元する。
        actions.push(Box::new(move |tx| {
            retract_stale_projection(tx, &valid)?;
            Ok(InputCheck::Current)
        }));
        actions.push(Box::new(move |tx| {
            tx.execute("DELETE FROM ai_location_suggest_holds", [])?;
            for (reason, count) in holds {
                tx.execute(
                    "INSERT INTO ai_location_suggest_holds VALUES(?1,?2)",
                    params![reason, count],
                )?;
            }
            Ok(InputCheck::Current)
        }));
        *self.work.lock().unwrap_or_else(PoisonError::into_inner) = work;
        Ok(actions)
    }
    fn work(&self, key: &JobKey) -> Option<Work> {
        self.work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|p| p.key() == *key)
            .cloned()
    }
}
impl Handler for LocationSuggest {
    fn kind(&self) -> JobKind {
        JobKind::LocationSuggest
    }
    fn reconcile_cleanup(&self, read: &Connection, now: i64) -> rusqlite::Result<Vec<Persist>> {
        self.prepare(read, now, true)
    }
    fn reconcile(&self, _: &Connection, _: i64) -> rusqlite::Result<Vec<JobKey>> {
        Ok(self
            .work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(Work::key)
            .collect())
    }
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        let work = self.work(&job.key).ok_or(RunError::InputChanged)?;
        let intent = match work {
            Work::Invalid(day, _, code) => {
                if !day.matches_db(ctx.read()).map_err(db_error)? {
                    return Err(RunError::InputChanged);
                }
                return Err(RunError::Permanent(code));
            }
            Work::Send(i) => *i,
        };
        // 画像を作る（読み込み・デコード）前に、安い照合で足切りする: 世代・クラウドの有無・許可・停止・pause・送信枠。
        if !intent.matches_db(ctx.read()).map_err(db_error)? {
            return Err(RunError::InputChanged);
        }
        let cloud = self.shared.cloud.as_ref().ok_or(RunError::PolicyDisabled)?;
        ctx.budget_left()?;
        let plan = self
            .shared
            .plan(ctx.read(), intent, &|| ctx.is_stopping())?;
        self.shared.check_plan(ctx.read(), &plan)?;
        let reply = cloud.run_checked(ctx, plan.request(), |deadline| {
            self.shared.ready(ctx, deadline)?;
            self.shared.check_plan(ctx.read(), &plan)
        })?;
        let answer = Answer::parse(reply.json_text(), plan.sent_ids().len())
            .map_err(|_| RunError::Attempt(ErrorCode::BadResponse))?;
        // Received paid answer: no Nominatim or further readiness I/O before durable commit.
        self.shared.check_plan(ctx.read(), &plan)?;
        let (claimed, now) = (job.clone(), ctx.now());
        Ok(Box::new(move |tx| {
            persist_answer(tx, &claimed, &plan, &answer, now)
        }))
    }
    fn projection(
        &self,
        _: &Connection,
        key: &JobKey,
        now: i64,
    ) -> rusqlite::Result<Option<Persist>> {
        Ok(self.work(key).map(|work| -> Persist {
            Box::new(move |tx| match work {
                Work::Send(plan) => project_failure(tx, &plan, now),
                Work::Invalid(day, _, _) => Ok(if day.matches_db(tx)? {
                    InputCheck::Current
                } else {
                    InputCheck::Changed
                }),
            })
        }))
    }
    fn prepare_retry(&self) -> rusqlite::Result<()> {
        let read =
            crate::db::open_read_only(&self.shared.data.root().join(crate::config::DB_FILE_NAME))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        drop(self.prepare(&read, jobs::unix_now(), false)?);
        let mut saved = Vec::new();
        for day in self.shared.days(&read)? {
            if let (Lookup::Reusable(answer), observed) = self.shared.lookup(&read, &day)? {
                saved.push(GeoPlan {
                    saved: *answer,
                    day,
                    observed,
                });
            }
        }
        *self
            .retry_saved
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = saved;
        Ok(())
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        for p in self
            .retry_saved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            if p.saved.plan_hash() == key.input_hash
                && p.saved.run_id() == key.target_key
                && processor_version(p.saved.model()) == key.processor_version
                && p.saved.applicable(conn, &p.day, &p.observed)?
            {
                return Ok(Recheck::Satisfied);
            }
        }
        Ok(match self.work(key) {
            Some(Work::Send(p)) if p.matches_db(conn)? => Recheck::Requeue,
            Some(Work::Invalid(d, _, _)) if d.matches_db(conn)? => Recheck::Requeue,
            _ => Recheck::Obsolete,
        })
    }
}

#[derive(Clone)]
struct GeoPlan {
    saved: SavedAnswer,
    day: Day,
    observed: Vec<SourceObservation>,
}
impl GeoPlan {
    fn key(&self) -> JobKey {
        let ids: Vec<_> = self
            .day
            .members()
            .iter()
            .map(Generation::photo_id)
            .collect();
        JobKey::new(
            JobKind::LocationGeocode,
            sha(encoded(&(self.day.date(), ids)).as_bytes()),
            sha(encoded(&(self.saved.result_key(), &self.day, GEOCODE_VERSION)).as_bytes()),
            GEOCODE_VERSION,
        )
    }
    fn applicable(&self, shared: &Shared, conn: &Connection) -> rusqlite::Result<bool> {
        let observed = shared.observations(conn, &self.saved.sent_ids())?;
        self.saved.applicable(conn, &self.day, &observed)
    }
    fn projected(&self, conn: &Connection) -> rusqlite::Result<bool> {
        for g in self.day.members() {
            let row: Option<(String,String,String)> = conn.query_row(
                "SELECT result_key,day_json,row_json FROM ai_location_candidates WHERE photo_id=?1",
                [g.photo_id()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if !row.is_some_and(|(key, day, row)| {
                key == self.saved.result_key()
                    && day == encoded(&self.day)
                    && row_snapshot(conn, g.photo_id()).ok().flatten().as_ref() == Some(&row)
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
struct LocationGeocode {
    shared: Arc<Shared>,
    work: Mutex<Vec<GeoPlan>>,
}
impl LocationGeocode {
    fn prepare(&self, conn: &Connection) -> rusqlite::Result<Vec<GeoPlan>> {
        let mut work = Vec::new();
        for day in self.shared.days(conn)? {
            if let (Lookup::Reusable(saved), observed) = self.shared.lookup(conn, &day)? {
                work.push(GeoPlan {
                    saved: *saved,
                    day,
                    observed,
                });
            }
        }
        Ok(work)
    }
    fn work(&self, key: &JobKey) -> Option<GeoPlan> {
        self.work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|p| p.key() == *key)
            .cloned()
    }
}
impl Handler for LocationGeocode {
    fn kind(&self) -> JobKind {
        JobKind::LocationGeocode
    }
    fn reconcile(&self, read: &Connection, _: i64) -> rusqlite::Result<Vec<JobKey>> {
        let work = self.prepare(read)?;
        let keys = work.iter().map(GeoPlan::key).collect();
        *self.work.lock().unwrap_or_else(PoisonError::into_inner) = work;
        Ok(keys)
    }
    fn run(&self, ctx: &RunCtx<'_>, job: &Claimed) -> Result<Persist, RunError> {
        let plan = self.work(&job.key).ok_or(RunError::InputChanged)?;
        if !plan
            .applicable(&self.shared, ctx.read())
            .map_err(db_error)?
        {
            return Err(RunError::InputChanged);
        }
        if plan.projected(ctx.read()).map_err(db_error)? {
            let observed = self
                .shared
                .observations(ctx.read(), &plan.saved.sent_ids())
                .map_err(db_error)?;
            return Ok(Box::new(move |tx| {
                Ok(if plan.saved.applicable(tx, &plan.day, &observed)? {
                    InputCheck::Current
                } else {
                    InputCheck::Changed
                })
            }));
        }
        // before_probe checks policy/pause, without Cloud token or inference reservation.
        ctx.before_probe()?;
        let answer = plan.saved.answer();
        let day = geocode(
            &self.shared,
            ctx,
            answer.geocode_query(),
            answer.precision(),
            answer.country_code(),
        )?;
        let mut rows = Vec::new();
        for g in plan.day.members() {
            let item = answer.photos().iter().find(|p| {
                plan.saved.photo_id(p) == Some(g.photo_id())
                    && !p.place_name().trim().is_empty()
                    && p.confidence() != Confidence::Unknown
                    && p.precision() != Precision::Unknown
            });
            let (scope, place, query, confidence, hit) = if let Some(item) = item {
                let hit = geocode(
                    &self.shared,
                    ctx,
                    item.geocode_query(),
                    item.precision(),
                    answer.country_code(),
                )?;
                (
                    "photo",
                    item.place_name(),
                    item.geocode_query(),
                    item.confidence(),
                    if hit.0.is_some() { hit } else { day.clone() },
                )
            } else {
                (
                    "day",
                    answer.place_name(),
                    answer.geocode_query(),
                    answer.confidence(),
                    day.clone(),
                )
            };
            rows.push(Candidate {
                id: g.photo_id(),
                run: plan.saved.run_id(),
                scope: scope.into(),
                place: place.into(),
                day_place: answer.place_name().into(),
                query: query.into(),
                precision: hit.1,
                confidence,
                evidence: encoded(&answer.evidence()),
                lat: hit.0.as_ref().and_then(|v| v["lat"].as_f64()),
                lon: hit.0.as_ref().and_then(|v| v["lon"].as_f64()),
                title: hit
                    .0
                    .as_ref()
                    .and_then(|v| v["title"].as_str())
                    .unwrap_or("")
                    .into(),
                model: plan.saved.model().into(),
                at: plan.saved.analyzed_at().into(),
            });
        }
        // Re-observe after all Nominatim I/O; callback uses these observations and DB only.
        let observed = self
            .shared
            .observations(ctx.read(), &plan.saved.sent_ids())
            .map_err(db_error)?;
        if !plan
            .saved
            .applicable(ctx.read(), &plan.day, &observed)
            .map_err(db_error)?
        {
            return Err(RunError::InputChanged);
        }
        Ok(Box::new(move |tx| {
            if !plan.saved.applicable(tx, &plan.day, &observed)? {
                return Ok(InputCheck::Changed);
            }
            for row in rows {
                row.save(tx)?;
                tx.execute("INSERT OR REPLACE INTO ai_location_candidates(photo_id,result_key,day_json,row_json) VALUES(?1,?2,?3,?4)",
                    params![row.id,plan.saved.result_key(),encoded(&plan.day),row_snapshot(tx,row.id)?])?;
            }
            Ok(InputCheck::Current)
        }))
    }
    fn projection(
        &self,
        read: &Connection,
        key: &JobKey,
        _: i64,
    ) -> rusqlite::Result<Option<Persist>> {
        let Some(plan) = self.work(key) else {
            return Ok(None);
        };
        let observed = self.shared.observations(read, &plan.saved.sent_ids())?;
        let key = key.clone();
        Ok(Some(Box::new(move |tx| {
            if !plan.saved.applicable(tx, &plan.day, &observed)? {
                return Ok(InputCheck::Changed);
            }
            let succeeded:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_jobs WHERE kind='location_geocode' AND target_key=?1 AND input_hash=?2 AND processor_version=?3 AND state='succeeded')",
                params![key.target_key,key.input_hash,key.processor_version],|r|r.get(0))?;
            if succeeded {
                for g in plan.day.members() {
                    // Restore only a missing projection with exact proven ownership; preserve Python rewrites.
                    if row_snapshot(tx, g.photo_id())?.is_none() {
                        let row:Option<String> = tx.query_row("SELECT row_json FROM ai_location_candidates WHERE photo_id=?1 AND result_key=?2 AND day_json=?3",
                            params![g.photo_id(),plan.saved.result_key(),encoded(&plan.day)],|r|r.get(0)).optional()?;
                        if let Some(row) = row {
                            restore_row(tx, g.photo_id(), &row)?;
                        }
                    }
                }
            }
            Ok(InputCheck::Current)
        })))
    }
    fn prepare_retry(&self) -> rusqlite::Result<()> {
        let read =
            crate::db::open_read_only(&self.shared.data.root().join(crate::config::DB_FILE_NAME))
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        *self.work.lock().unwrap_or_else(PoisonError::into_inner) = self.prepare(&read)?;
        Ok(())
    }
    fn recheck(&self, conn: &Connection, key: &JobKey) -> rusqlite::Result<Recheck> {
        let Some(plan) = self.work(key) else {
            return Ok(Recheck::Obsolete);
        };
        // File observations prepared outside writer. The cached day/generations are rechecked here.
        if !plan.saved.applicable(conn, &plan.day, &plan.observed)? {
            return Ok(Recheck::Obsolete);
        }
        Ok(if plan.projected(conn)? {
            Recheck::Satisfied
        } else {
            Recheck::Requeue
        })
    }
}

fn geocode(
    shared: &Shared,
    ctx: &RunCtx<'_>,
    query: &str,
    precision: Precision,
    country: &str,
) -> Result<(Option<Value>, Precision), RunError> {
    let query = query.trim();
    if query.is_empty() || precision == Precision::Unknown {
        return Ok((None, precision));
    }
    let search = |query: &str| {
        ctx.before_probe()?;
        match shared
            .geocoder
            .nominatim_checked(&shared.cache, query, country, ctx.stop())
        {
            Outcome::Success(items) => Ok(items.into_iter().next()),
            Outcome::Stopping => Err(RunError::Stopping),
            Outcome::Transient => Err(RunError::Attempt(ErrorCode::GeocodeWait)),
        }
    };
    if let Some(hit) = search(query)? {
        return Ok((Some(hit), precision));
    }
    let parts: Vec<_> = query
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() > 1
        && let Some(hit) = search(&parts[1..].join(", "))?
    {
        let coarser = match precision {
            Precision::Landmark | Precision::District => Precision::City,
            Precision::City => Precision::Region,
            Precision::Region | Precision::Country => Precision::Country,
            Precision::Unknown => Precision::Unknown,
        };
        return Ok((Some(hit), coarser));
    }
    Ok((None, precision))
}

struct Candidate {
    id: i64,
    run: String,
    scope: String,
    place: String,
    day_place: String,
    query: String,
    precision: Precision,
    confidence: Confidence,
    evidence: String,
    lat: Option<f64>,
    lon: Option<f64>,
    title: String,
    model: String,
    at: String,
}
impl Candidate {
    fn save(&self, conn: &Connection) -> rusqlite::Result<()> {
        let precision = serde_json::to_value(self.precision).expect("enum");
        let confidence = serde_json::to_value(self.confidence).expect("enum");
        conn.execute("INSERT OR REPLACE INTO photo_loc_suggestions(photo_id,run_id,scope,place_name,day_place_name,geocode_query,precision,confidence,evidence_json,lat,lon,geocode_title,model,analyzed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![self.id,self.run,self.scope,self.place,self.day_place,self.query,precision.as_str(),confidence.as_str(),self.evidence,self.lat,self.lon,self.title,self.model,self.at])?;
        Ok(())
    }
}
fn restore_row(conn: &Connection, id: i64, row: &str) -> rusqlite::Result<()> {
    let Ok(Value::Array(values)) = serde_json::from_str::<Value>(row) else {
        return Ok(());
    };
    if values.len() != 14 || values[0].as_i64() != Some(id) {
        return Ok(());
    }
    let mut cells = Vec::new();
    for value in values {
        cells.push(match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::String(s) => rusqlite::types::Value::Text(s),
            Value::Number(n) => match n.as_i64() {
                Some(i) => rusqlite::types::Value::Integer(i),
                None => {
                    rusqlite::types::Value::Real(n.as_f64().ok_or(rusqlite::Error::InvalidQuery)?)
                }
            },
            _ => return Ok(()),
        });
    }
    conn.execute("INSERT INTO photo_loc_suggestions(photo_id,run_id,scope,place_name,day_place_name,geocode_query,precision,confidence,evidence_json,lat,lon,geocode_title,model,analyzed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",rusqlite::params_from_iter(cells))?;
    Ok(())
}

/// 自分の投影（`row_json` と画面の行が一致）のうち、今の有効な結果・日と違うものの画面の行を消す。
/// Python が書き換えた行（一致しない）や、候補の無い行には触れない。
fn retract_stale_projection(
    tx: &Connection,
    valid: &BTreeMap<i64, (String, String)>,
) -> rusqlite::Result<()> {
    if !jobs::object_exists(tx, "table", "ai_location_candidates")? {
        return Ok(());
    }
    let rows = tx
        .prepare("SELECT photo_id,result_key,day_json,row_json FROM ai_location_candidates")?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, result, day, row) in rows {
        if valid.get(&id) != Some(&(result, day)) && row_snapshot(tx, id)?.as_ref() == Some(&row) {
            tx.execute("DELETE FROM photo_loc_suggestions WHERE photo_id=?1", [id])?;
        }
    }
    Ok(())
}

pub fn hold_counts(conn: &Connection) -> rusqlite::Result<Vec<(&'static str, i64)>> {
    if !jobs::object_exists(conn, "table", "ai_location_suggest_holds")? {
        return Ok(Vec::new());
    }
    let rows = conn
        .prepare(
            "SELECT reason,count FROM ai_location_suggest_holds WHERE count>0 ORDER BY reason",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(s, n)| HOLDS.iter().find(|v| **v == s).map(|s| (*s, n)))
        .collect())
}

#[cfg(test)]
mod tests;

type KeyTriple = (String, String, String);
fn key_triple(key: &JobKey) -> KeyTriple {
    (
        key.target_key.clone(),
        key.input_hash.clone(),
        key.processor_version.clone(),
    )
}
/// 失敗の履歴で止まっている location_suggest のキー。`obsolete` でも `terminal` なら enqueue が `failed` に戻すので
/// 同じ扱い。手動の再試行（`terminal` を 0 にして `pending` へ）をすれば、ここから外れる。
fn failed_keys(conn: &Connection) -> rusqlite::Result<HashSet<KeyTriple>> {
    conn.prepare(
        "SELECT target_key,input_hash,processor_version FROM ai_jobs
         WHERE kind='location_suggest' AND (state='failed' OR (state='obsolete' AND terminal=1))",
    )?
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
    .collect()
}
