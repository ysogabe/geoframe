//! DB だけを扱う。有償回答は immutable な世代別履歴、Python run は互換投影。
use super::*;
use crate::ai::jobs::{self, Claimed, Completion, Enqueued, ErrorCode, InputCheck};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub(in crate::ai) const TABLES: &[&str] = &[
    "ai_location_suggest_results",
    "ai_location_candidates",
    "ai_location_suggest_holds",
];
pub(in crate::ai) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ai_location_suggest_results(
 result_key TEXT PRIMARY KEY CHECK(length(result_key)=64),
 manifest_json TEXT NOT NULL,
 answer_json TEXT NOT NULL,
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 1 AND 5),
 analyzed_at TEXT NOT NULL,
 integrity_sha256 TEXT NOT NULL CHECK(length(integrity_sha256)=64)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_location_candidates(
 photo_id INTEGER PRIMARY KEY,
 result_key TEXT NOT NULL,
 day_json TEXT NOT NULL,
 row_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_location_suggest_holds(
 reason TEXT PRIMARY KEY CHECK(reason IN ('pending','legacy_unverified','unverified','projection_pending','decision_wait','no_images')),
 count INTEGER NOT NULL CHECK(count>=0)
) WITHOUT ROWID;";

#[derive(Clone)]
pub struct SavedAnswer {
    manifest: Manifest,
    answer: Answer,
    attempts: u32,
    analyzed_at: String,
}
impl std::fmt::Debug for SavedAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SavedAnswer(..)")
    }
}
impl SavedAnswer {
    pub(super) fn model(&self) -> &str {
        &self.manifest.model
    }
    pub fn answer(&self) -> &Answer {
        &self.answer
    }
    pub fn result_key(&self) -> String {
        self.manifest.result_key()
    }
    pub fn run_id(&self) -> String {
        self.manifest.run_id()
    }
    pub fn sent_ids(&self) -> Vec<i64> {
        self.manifest.sent_ids()
    }
    pub fn photo_id(&self, answer: &PhotoAnswer) -> Option<i64> {
        self.manifest
            .sent
            .get(answer.index().checked_sub(1)?)
            .map(|s| s.photo_id)
    }
    pub fn analyzed_at(&self) -> &str {
        &self.analyzed_at
    }
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
    fn compat(&self) -> Compat {
        Compat {
            run_id: self.run_id(),
            day: self.manifest.day.clone(),
            model: self.manifest.model.clone(),
            prompt_version: self.manifest.prompt_version.clone(),
            member_ids_json: encoded(
                &self
                    .manifest
                    .members
                    .iter()
                    .map(Generation::photo_id)
                    .collect::<Vec<_>>(),
            ),
            sent_ids_json: encoded(&self.sent_ids()),
            status: "done".into(),
            attempts: i64::from(self.attempts),
            answer_json: self.answer.normalized_json(),
            error: String::new(),
            analyzed_at: self.analyzed_at.clone(),
        }
    }
    /// c2 の候補 CAS にも使える。GPS を持った sent も generation を検査する。
    pub fn applicable(
        &self,
        conn: &Connection,
        day: &Day,
        observed: &[SourceObservation],
    ) -> rusqlite::Result<bool> {
        if self.matches_inputs(conn, day, observed)? != InputMatch::Current {
            return Ok(false);
        }
        let (history, corrupt) = history(conn)?;
        Ok(!corrupt
            && history
                .iter()
                .any(|s| s.result_key() == self.result_key() && s.compat() == self.compat())
            && Compat::read(conn)?.iter().any(|c| *c == self.compat()))
    }
    fn matches_inputs(
        &self,
        conn: &Connection,
        day: &Day,
        observed: &[SourceObservation],
    ) -> rusqlite::Result<InputMatch> {
        if self.manifest.prompt_version != PROMPT_VERSION
            || self.manifest.processor_version != processor_version(&self.manifest.model)
            || day.day != self.manifest.day
            || !day.matches_db(conn)?
        {
            return Ok(InputMatch::Changed);
        }
        for g in &day.members {
            if !self.manifest.members.contains(g) {
                return Ok(InputMatch::Changed);
            }
        }
        for sent in &self.manifest.sent {
            let saved = self
                .manifest
                .members
                .iter()
                .find(|g| g.photo_id() == sent.photo_id)
                .unwrap();
            if !saved.matches_db(conn)? {
                return Ok(InputMatch::Changed);
            }
            let Some(current) = observed.iter().find(|g| g.photo_id() == sent.photo_id) else {
                return Ok(InputMatch::Unverified);
            };
            if current.generation() != saved || !current.generation().matches_db(conn)? {
                return Ok(InputMatch::Changed);
            }
        }
        Ok(InputMatch::Current)
    }
}
#[derive(PartialEq, Eq)]
enum InputMatch {
    Current,
    Changed,
    Unverified,
}
#[derive(Debug)]
pub enum Lookup {
    Reusable(Box<SavedAnswer>),
    /// 適合する履歴はあるが別の既知 Rust 世代が互換行へ投影中。再送せず再投影する。
    ProjectionPending,
    Absent,
    /// provenance のない旧 done。自動課金・自動 trusted 採用を止める。
    LegacyUnverified,
    /// 破損した履歴/互換投影。未知値や旧自由文は持たない。
    Unverified,
}
#[derive(Debug)]
pub enum Queue {
    Enqueued(Enqueued),
    Reusable(Box<SavedAnswer>),
    ProjectionPending,
    LegacyUnverified,
    Unverified,
    Changed,
}

// 全11列を比較し、Python による互換投影の変更を Rust の結果と誤認しない。
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Compat {
    run_id: String,
    day: String,
    model: String,
    prompt_version: String,
    member_ids_json: String,
    sent_ids_json: String,
    status: String,
    attempts: i64,
    answer_json: String,
    error: String,
    analyzed_at: String,
}
impl Compat {
    fn read(conn: &Connection) -> rusqlite::Result<Vec<Self>> {
        if !jobs::object_exists(conn, "table", "photo_loc_runs")? {
            return Ok(Vec::new());
        }
        conn.prepare("SELECT run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,attempts,answer_json,error,analyzed_at FROM photo_loc_runs")?
            .query_map([],|r|Ok(Self { run_id:r.get(0)?,day:r.get(1)?,model:r.get(2)?,prompt_version:r.get(3)?,member_ids_json:r.get(4)?,sent_ids_json:r.get(5)?,status:r.get(6)?,attempts:r.get(7)?,answer_json:r.get(8)?,error:r.get(9)?,analyzed_at:r.get(10)? }))?.collect()
    }
    fn write(&self, conn: &Connection) -> rusqlite::Result<()> {
        conn.execute("INSERT INTO photo_loc_runs(run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,attempts,answer_json,error,analyzed_at)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
          ON CONFLICT(run_id) DO UPDATE SET day=excluded.day,model=excluded.model,prompt_version=excluded.prompt_version,
          member_ids_json=excluded.member_ids_json,sent_ids_json=excluded.sent_ids_json,status=excluded.status,
          attempts=excluded.attempts,answer_json=excluded.answer_json,error=excluded.error,analyzed_at=excluded.analyzed_at",
            params![self.run_id,self.day,self.model,self.prompt_version,self.member_ids_json,self.sent_ids_json,self.status,self.attempts,self.answer_json,self.error,self.analyzed_at])?;
        Ok(())
    }
}

fn integrity(manifest: &str, answer: &str, attempts: u32, at: &str) -> String {
    sha(encoded(&(manifest, answer, attempts, at)).as_bytes())
}
fn valid_manifest(m: &Manifest) -> bool {
    let ids = m
        .members
        .iter()
        .map(Generation::photo_id)
        .collect::<std::collections::HashSet<_>>();
    let sent_ids = m
        .sent_ids()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    !m.prompt_version.is_empty()
        && m.prompt_version.len() <= 64
        && valid_hash(&m.processor_version)
        && !m.model.is_empty()
        && m.model.len() <= 256
        && m.day.len() == 10
        && (1..=MAX_MEMBERS).contains(&m.members.len())
        && ids.len() == m.members.len()
        && (1..=MAX_SEND).contains(&m.sent.len())
        && sent_ids.len() == m.sent.len()
        && m.sent.iter().all(|s| {
            valid_hash(&s.source_sha256)
                && valid_hash(&s.jpeg_sha256)
                && m.members.iter().any(|g| {
                    g.photo_id() == s.photo_id && g.source_sha256() == Some(&s.source_sha256)
                })
        })
}
// Any corrupt history is conservatively a fixed hold, never a reason to bill again.
pub(super) fn history(conn: &Connection) -> rusqlite::Result<(Vec<SavedAnswer>, bool)> {
    if !jobs::object_exists(conn, "table", TABLES[0])? {
        return Ok((Vec::new(), false));
    }
    let mut stmt=conn.prepare("SELECT result_key,manifest_json,answer_json,attempts,analyzed_at,integrity_sha256 FROM ai_location_suggest_results ORDER BY analyzed_at DESC,result_key")?;
    let mut saved = Vec::new();
    let mut corrupt = false;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u32>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })? {
        let (key, m, a, n, at, hash) = row?;
        let parsed = (|| {
            if m.len() > MAX_MANIFEST_BYTES
                || a.len() > 1024 * 1024
                || !(1..=jobs::MAX_ATTEMPTS).contains(&n)
                || at.len() != 20
                || integrity(&m, &a, n, &at) != hash
            {
                return None;
            }
            let manifest: Manifest = serde_json::from_str(&m).ok()?;
            if !valid_manifest(&manifest) || manifest.result_key() != key || encoded(&manifest) != m
            {
                return None;
            }
            let answer = Answer::parse(&a, manifest.sent.len()).ok()?;
            if answer.normalized_json() != a {
                return None;
            }
            Some(SavedAnswer {
                manifest,
                answer,
                attempts: n,
                analyzed_at: at,
            })
        })();
        if let Some(s) = parsed {
            saved.push(s);
        } else {
            corrupt = true;
        }
    }
    Ok((saved, corrupt))
}

/// lookup 用に writer 外で観測すべき sent IDs。手動 SAVE 済みも含む。
pub fn saved_sent_ids(conn: &Connection, day: &Day, model: &str) -> rusqlite::Result<Vec<i64>> {
    let (history, _) = history(conn)?;
    let mut ids: Vec<_> = history
        .iter()
        .filter(|s| s.manifest.day == day.day && s.manifest.model == model)
        .flat_map(SavedAnswer::sent_ids)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
fn collides(c: &Compat, day: &Day, model: &str, run_id: Option<&str>) -> bool {
    c.status == "done"
        && (run_id == Some(c.run_id.as_str())
            || (c.model == model
                && c.prompt_version == PROMPT_VERSION
                && c.day == day.day
                && serde_json::from_str::<Vec<i64>>(&c.member_ids_json)
                    .ok()
                    .is_some_and(|ids| day.members.iter().all(|g| ids.contains(&g.photo_id())))))
}
fn classify(
    conn: &Connection,
    day: &Day,
    model: &str,
    run_id: Option<&str>,
    observed: &[SourceObservation],
) -> rusqlite::Result<Lookup> {
    let (history, corrupt) = history(conn)?;
    if corrupt {
        return Ok(Lookup::Unverified);
    }
    let runs = Compat::read(conn)?;
    for c in &runs {
        if collides(c, day, model, run_id) && !history.iter().any(|s| s.compat() == *c) {
            return Ok(Lookup::LegacyUnverified);
        }
    }
    let mut unverified = false;
    for s in &history {
        if s.manifest.model == model
            && s.manifest.prompt_version == PROMPT_VERSION
            && s.manifest.processor_version == processor_version(model)
        {
            match s.matches_inputs(conn, day, observed)? {
                InputMatch::Changed => continue,
                InputMatch::Unverified => {
                    unverified = true;
                    continue;
                }
                InputMatch::Current => {}
            }
            if !runs.iter().any(|c| s.compat() == *c) {
                if runs
                    .iter()
                    .any(|c| c.run_id == s.run_id() && history.iter().any(|h| h.compat() == *c))
                {
                    return Ok(Lookup::ProjectionPending);
                }
                return Ok(Lookup::Unverified);
            }
            return Ok(Lookup::Reusable(Box::new(s.clone())));
        }
    }
    Ok(if unverified {
        Lookup::Unverified
    } else {
        Lookup::Absent
    })
}
/// 認証/CloudClient なしで検証済み回答を読む。observed は saved_sent_ids の現世代。
pub fn lookup(
    conn: &Connection,
    day: &Day,
    model: &str,
    observed: &[SourceObservation],
) -> rusqlite::Result<Lookup> {
    let run = (!day.sent_ids.is_empty()).then(|| run_id_for(model, &day.sent_ids));
    classify(conn, day, model, run.as_deref(), observed)
}

/// 異なる既知 Rust 世代への投影だけを短い writer tx で復元する。認証/送信/試行 reset は不要。
pub fn restore_projection(
    conn: &mut Connection,
    day: &Day,
    model: &str,
    observed: &[SourceObservation],
) -> rusqlite::Result<Lookup> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = restore_projection_in(&tx, day, model, observed)?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn restore_projection_in(
    tx: &Connection,
    day: &Day,
    model: &str,
    observed: &[SourceObservation],
) -> rusqlite::Result<Lookup> {
    let run = (!day.sent_ids.is_empty()).then(|| run_id_for(model, &day.sent_ids));
    let outcome = classify(tx, day, model, run.as_deref(), observed)?;
    if !matches!(outcome, Lookup::ProjectionPending) {
        return Ok(outcome);
    }
    let (history, _) = history(tx)?;
    for s in history {
        if s.manifest.model == model && s.matches_inputs(tx, day, observed)? == InputMatch::Current
        {
            s.compat().write(tx)?;
            return Ok(Lookup::Reusable(Box::new(s)));
        }
    }
    Ok(Lookup::Unverified)
}

/// 既存 run の失敗回数を新 job へ継承する。検証済み Rust done の成功1回は含めない。
/// 初回 INSERT だけで seed し、手動 retry 後は reset を妨げない。
/// この短い tx の外で画像観測/準備を終える。policy/STOP/budget gate は c2 の送信直前に別途必要。
pub fn enqueue(
    conn: &mut Connection,
    plan: &Plan,
    observed: &[SourceObservation],
    now: i64,
) -> rusqlite::Result<Queue> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = enqueue_in(&tx, plan, observed, now)?;
    tx.commit()?;
    Ok(outcome)
}

/// Handler::reconcile_cleanup の Persist 用。既存 writer tx 内で使い、直接 jobs::enqueue より先に呼ぶ。
pub fn enqueue_in(
    conn: &Connection,
    plan: &Plan,
    observed: &[SourceObservation],
    now: i64,
) -> rusqlite::Result<Queue> {
    if conn.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if !plan.matches_db(conn)? {
        return Ok(Queue::Changed);
    }
    match classify(
        conn,
        &plan.day,
        &plan.manifest.model,
        Some(&plan.manifest.run_id()),
        observed,
    )? {
        Lookup::Reusable(s) => return Ok(Queue::Reusable(s)),
        Lookup::LegacyUnverified => return Ok(Queue::LegacyUnverified),
        Lookup::Unverified => return Ok(Queue::Unverified),
        Lookup::ProjectionPending => return Ok(Queue::ProjectionPending),
        Lookup::Absent => {}
    }
    let key = plan.key();
    let mut enqueued = jobs::enqueue_in(conn, &key, now)?;
    if enqueued == Enqueued::Created {
        // A Python run_id is model/prompt/sent-set keyed, even when a new image generation is planned.
        let (history, _) = history(conn)?;
        let failure_floor = Compat::read(conn)?
            .into_iter()
            .filter(|c| c.run_id == key.target_key)
            .map(|c| {
                // Only an exact, verified Rust projection proves the successful request.
                // Unverified legacy done rows were held by classify, never discounted here.
                history
                    .iter()
                    .find(|s| s.compat() == c)
                    .map_or(c.attempts.max(0), |s| i64::from(s.attempts - 1))
            })
            .max()
            .unwrap_or(0)
            .min(i64::from(jobs::MAX_ATTEMPTS));
        conn.execute("UPDATE ai_jobs SET attempts=?5, terminal=(?5>=5),state=CASE WHEN ?5>=5 THEN 'failed' ELSE state END
          WHERE kind=?1 AND target_key=?2 AND input_hash=?3 AND processor_version=?4",
          params![key.kind.as_str(),key.target_key,key.input_hash,key.processor_version,failure_floor])?;
        if failure_floor >= i64::from(jobs::MAX_ATTEMPTS) {
            enqueued = Enqueued::Existing(jobs::JobState::Failed);
        }
    }
    Ok(Queue::Enqueued(enqueued))
}

/// 有効 token + 現集合の DB CAS + immutable 履歴 + Python 全列投影 + succeeded を原子的に保存。
/// 呼出し側は応答直後に実 source SHA/現 policy を再検査する。ファイル/HTTP はこの中で行わない。
pub fn complete(
    conn: &mut Connection,
    claimed: &Claimed,
    plan: &Plan,
    answer: &Answer,
    now: i64,
) -> rusqlite::Result<Completion> {
    jobs::complete(conn, claimed, now, |tx| {
        persist_answer(tx, claimed, plan, answer, now)
    })
}
/// Handler::run が返す Persist 用。jobs::complete の tx 内のみ。旧 token では書かない。
pub fn persist_answer(
    conn: &Connection,
    claimed: &Claimed,
    plan: &Plan,
    answer: &Answer,
    now: i64,
) -> rusqlite::Result<InputCheck> {
    if conn.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if !jobs::owns(conn, claimed)? || claimed.key != plan.key() || !plan.matches_db(conn)? {
        return Ok(InputCheck::Changed);
    }
    let (history, corrupt) = history(conn)?;
    let runs = Compat::read(conn)?;
    if corrupt
        || runs.iter().any(|c| {
            collides(
                c,
                &plan.day,
                &plan.manifest.model,
                Some(&plan.manifest.run_id()),
            ) && !history.iter().any(|s| s.compat() == *c)
        })
    {
        return Ok(InputCheck::Changed);
    }
    let attempts: u32 = conn.query_row(
        "SELECT attempts FROM ai_jobs WHERE id=?1",
        [claimed.id],
        |r| r.get(0),
    )?;
    if attempts >= jobs::MAX_ATTEMPTS
        || answer
            .photos()
            .iter()
            .any(|p| p.index() > plan.manifest.sent.len())
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let saved = SavedAnswer {
        manifest: plan.manifest.clone(),
        answer: answer.clone(),
        attempts: attempts + 1,
        analyzed_at: crate::ai::waits::utc_iso(now),
    };
    let key = saved.result_key();
    // A duplicate key may never silently replace a different paid answer.
    let prior = history.iter().find(|s| s.result_key() == key);
    let saved = if let Some(prior) = prior {
        if prior.answer != saved.answer {
            return Err(rusqlite::Error::InvalidQuery);
        }
        prior
    } else {
        let m = encoded(&saved.manifest);
        let a = saved.answer.normalized_json();
        conn.execute("INSERT INTO ai_location_suggest_results(result_key,manifest_json,answer_json,attempts,analyzed_at,integrity_sha256) VALUES(?1,?2,?3,?4,?5,?6)",
            params![key,m,a,saved.attempts,saved.analyzed_at,integrity(&m,&a,saved.attempts,&saved.analyzed_at)])?;
        &saved
    };
    saved.compat().write(conn)?;
    Ok(InputCheck::Current)
}

/// jobs::record_failure_with の terminal callback。ai_jobs の試行が唯一の正。
/// retry/defer の中間投影はせず、旧 paid done は上書きしない。
pub fn project_failure(conn: &Connection, plan: &Plan, now: i64) -> rusqlite::Result<InputCheck> {
    if conn.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if !plan.matches_db(conn)? {
        return Ok(InputCheck::Changed);
    }
    let (history, corrupt) = history(conn)?;
    if corrupt {
        return Ok(InputCheck::Changed);
    }
    let mut c = Compat {
        run_id: plan.manifest.run_id(),
        day: plan.manifest.day.clone(),
        model: plan.manifest.model.clone(),
        prompt_version: PROMPT_VERSION.into(),
        member_ids_json: encoded(
            &plan
                .day
                .members
                .iter()
                .map(Generation::photo_id)
                .collect::<Vec<_>>(),
        ),
        sent_ids_json: encoded(&plan.sent_ids()),
        status: "error".into(),
        attempts: 0,
        answer_json: String::new(),
        error: String::new(),
        analyzed_at: crate::ai::waits::utc_iso(now),
    };
    if Compat::read(conn)?
        .iter()
        .any(|r| r.run_id == c.run_id && r.status == "done")
    {
        return Ok(InputCheck::Changed);
    }
    if history
        .iter()
        .any(|s| s.result_key() == plan.manifest.result_key())
    {
        return Ok(InputCheck::Changed);
    }
    let row = conn
        .query_row(
            "SELECT state,attempts,last_error_code FROM ai_jobs WHERE kind='location_suggest' AND target_key=?1 AND input_hash=?2 AND processor_version=?3 AND claim_token IS NULL",
            params![plan.key().target_key, plan.key().input_hash, plan.key().processor_version],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_,String>(2)?)),
        )
        .optional()?;
    let Some((state, n, code)) = row else {
        return Ok(InputCheck::Changed);
    };
    if state != "failed" {
        return Ok(InputCheck::Changed);
    }
    c.attempts = n;
    c.error = ErrorCode::parse(&code)
        .map_or("other", ErrorCode::as_str)
        .into();
    c.write(conn)?;
    Ok(InputCheck::Current)
}
