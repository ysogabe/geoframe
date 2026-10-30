//! 合成写真/文字列/DB だけ。外部 HTTP・鍵・アプリ Python import は使わない。
use super::repository::Queue;
use super::*;
use crate::ai::image_input::ImageProfile;
use crate::ai::jobs::{self, Claimed, Completion, ErrorCode, Failure, InputCheck, JobState};
use crate::schema as dbschema;
use crate::testutil::TempDir;
use rusqlite::params;
use serde_json::{Value, json};

const MODEL: &str = "synthetic-suggest";
const T: i64 = 1_800_000_000;
struct Db {
    conn: Connection,
    data: DataDir,
    _dir: TempDir,
}
impl Db {
    fn new() -> Self {
        let dir = TempDir::new("suggest-synthetic");
        let mut conn = Connection::open_in_memory().unwrap();
        dbschema::migrate(&mut conn).unwrap();
        crate::ai::migrate(&mut conn).unwrap();
        let data = DataDir::open(dir.path()).unwrap();
        Self {
            conn,
            data,
            _dir: dir,
        }
    }
    fn add(&self, id: i64, time: &str) {
        std::fs::create_dir_all(self.data.root().join("photos")).unwrap();
        let mut bytes = Vec::new();
        jpeg_encoder::Encoder::new(&mut bytes, 88)
            .encode(&[id as u8; 16 * 8 * 3], 16, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        std::fs::write(
            self.data.root().join(format!("photos/synthetic-{id}.jpg")),
            &bytes,
        )
        .unwrap();
        self.conn.execute("INSERT INTO photos(id,source,name,display,taken_at,md5,mtime,rotation,hidden) VALUES(?1,'local','synthetic',?2,?3,'synthetic-md5',1,0,0)",params![id,format!("photos/synthetic-{id}.jpg"),time]).unwrap();
        self.conn.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,analyzed_at,error) SELECT id,display,md5,mtime,?2,'2026-01-02T00:00:00Z','' FROM photos WHERE id=?1",params![id,sha(&bytes)]).unwrap();
    }
    fn targets(&self) -> Vec<Target> {
        location_clue::load_targets(&self.conn, &self.data, "synthetic-decision")
            .unwrap()
            .into_iter()
            .map(|mut t| {
                t.readiness = Readiness::Ready(location_clue::Clue::new(0.5, "street").unwrap());
                t
            })
            .collect()
    }
    fn days(&self) -> Vec<Day> {
        group_days(
            &self.conn,
            &self.data,
            self.targets(),
            false,
            &[7200., 43200.],
        )
        .unwrap()
    }
    fn plan(&self) -> Plan {
        make_plan(self.days().remove(0))
    }
    fn seed(&self) -> Plan {
        self.add(1, "2026-01-02 10:00");
        self.add(2, "2026-01-02 11:00");
        self.plan()
    }
    fn claim(&mut self, p: &Plan) -> Claimed {
        assert!(matches!(
            enqueue(&mut self.conn, p, &[], T).unwrap(),
            Queue::Enqueued(_)
        ));
        jobs::claim_next(
            &self.conn,
            T,
            "synthetic-boot",
            "synthetic-token",
            &[JobKind::LocationSuggest],
        )
        .unwrap()
        .unwrap()
    }
    fn save(&mut self, p: &Plan) -> Claimed {
        let c = self.claim(p);
        assert_eq!(
            complete(&mut self.conn, &c, p, &answer(), T + 1).unwrap(),
            Completion::Succeeded
        );
        c
    }
    fn n(&self, sql: &str) -> i64 {
        self.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }
    fn observations(&self, day: &Day) -> Vec<SourceObservation> {
        saved_sent_ids(&self.conn, day, MODEL)
            .unwrap()
            .into_iter()
            .filter_map(|id| SourceObservation::observe(&self.conn, &self.data, id).unwrap())
            .collect()
    }
    fn lookup(&self) -> Lookup {
        let day = self.days().remove(0);
        lookup(&self.conn, &day, MODEL, &self.observations(&day)).unwrap()
    }
}
fn image() -> PreparedImage {
    PreparedImage::encode_jpeg_rgb(&[128; 16 * 8 * 3], 16, 8, 88, ImageProfile::Decision).unwrap()
}
fn make_plan(day: Day) -> Plan {
    let images = day
        .sent_ids
        .iter()
        .map(|id| SendImage {
            photo_id: *id,
            source_sha256: day
                .members
                .iter()
                .find(|g| g.photo_id() == *id)
                .unwrap()
                .source_sha256()
                .unwrap()
                .into(),
            image: image(),
        })
        .collect();
    Plan::new(day, MODEL, images).unwrap()
}
fn answer() -> Answer {
    Answer::parse(r#"{"country":"合成国","country_code":"zz","city":"合成都市","place_name":"合成都市","geocode_query":"Synthetic City","precision":"city","confidence":"medium","evidence":["合成看板"],"photos":[{"index":2,"place_name":"合成公園","geocode_query":"Synthetic Park","precision":"district","confidence":"low"}]}"#,2).unwrap()
}

#[test]
fn python_ast_constants_pure_functions_and_answer_normalization_match() {
    let f: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/location-suggest-python.json"
    ))
    .unwrap();
    assert_eq!(PROMPT, f["constants"]["PROMPT"].as_str().unwrap());
    assert_eq!(schema(), f["constants"]["SCHEMA"]);
    assert_eq!(PROMPT_VERSION, f["constants"]["PROMPT_VERSION"]);
    assert_eq!(
        MAX_SEND as u64,
        f["constants"]["MAX_SEND"].as_u64().unwrap()
    );
    assert_eq!(
        jobs::MAX_ATTEMPTS as u64,
        f["constants"]["MAX_ATTEMPTS"].as_u64().unwrap()
    );
    for c in f["run_ids"].as_array().unwrap() {
        let ids: Vec<i64> = serde_json::from_value(c["sent"].clone()).unwrap();
        assert_eq!(run_id_for(c["model"].as_str().unwrap(), &ids), c["run_id"]);
    }
    for c in f["answers"].as_array().unwrap() {
        let got = Answer::parse(&c["input"].to_string(), 2).unwrap();
        assert_eq!(serde_json::to_value(got).unwrap(), c["normalized"]);
    }
    for c in f["groups"].as_array().unwrap() {
        let db = Db::new();
        let mut targets = Vec::new();
        for p in c["photos"].as_array().unwrap() {
            let id = p["id"].as_i64().unwrap();
            let taken = p["taken_at"].as_str().unwrap();
            db.add(id, taken);
            let mut t = db.targets().into_iter().find(|t| t.photo_id == id).unwrap();
            t.epoch = p["epoch"].as_i64().unwrap();
            t.readiness = Readiness::Ready(
                location_clue::Clue::new(p["clue"].as_f64().unwrap(), p["kind"].as_str().unwrap())
                    .unwrap(),
            );
            targets.push(t);
        }
        let days = group_days(
            &db.conn,
            &db.data,
            targets,
            false,
            &[c["gap"].as_f64().unwrap()],
        )
        .unwrap();
        assert_eq!(
            json!(
                days.iter()
                    .map(|d| d
                        .members
                        .iter()
                        .map(Generation::photo_id)
                        .collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            ),
            c["groups"]
        );
        assert_eq!(
            json!(days.iter().map(|d| d.sent_ids.clone()).collect::<Vec<_>>()),
            c["sent"]
        );
    }
}

#[test]
fn hidden_is_removed_before_pending_gate_and_failed_never_sent() {
    let db = Db::new();
    for id in 1..=4 {
        db.add(id, "2026-01-02 10:00");
    }
    db.conn
        .execute("UPDATE photos SET hidden=1 WHERE id=1", [])
        .unwrap();
    let targets = || {
        db.targets()
            .into_iter()
            .map(|mut t| {
                t.readiness = match t.photo_id {
                    1 => Readiness::Pending,
                    2 => Readiness::Failed,
                    3 => Readiness::Ready(location_clue::Clue::new(0.19, "street").unwrap()),
                    _ => t.readiness,
                };
                t
            })
            .collect()
    };
    let day = group_days(&db.conn, &db.data, targets(), false, &[43200.])
        .unwrap()
        .remove(0);
    assert!(day.sendable());
    assert_eq!(day.sent_ids(), [4]);
    assert_eq!(day.members.len(), 3);
    let day = group_days(&db.conn, &db.data, targets(), true, &[43200.])
        .unwrap()
        .remove(0);
    assert!(day.pending());
    assert!(!day.sendable());
    assert_eq!(
        Plan::new(day, MODEL, Vec::new()).unwrap_err(),
        DomainError::Pending
    );
}

#[test]
fn numbered_request_preserves_send_order_but_hash_sorts_and_photo_judge_label_stays_english() {
    let db = Db::new();
    db.add(1, "2026-01-02 10:00");
    db.add(9, "2026-01-02 11:00");
    let mut targets = db.targets();
    targets[1].readiness = Readiness::Ready(location_clue::Clue::new(0.01, "landmark").unwrap());
    let day = group_days(&db.conn, &db.data, targets, false, &[43200.])
        .unwrap()
        .remove(0);
    let p = make_plan(day);
    assert_eq!(p.sent_ids(), [9, 1]);
    assert_eq!(p.key().target_key, run_id_for(MODEL, &[1, 9]));
    let b = p.request.body_json_for_test();
    assert_eq!(
        b["contents"][0]["parts"][0]["text"],
        PROMPT.replace("{date}", "2026-01-02")
    );
    assert_eq!(b["contents"][0]["parts"][1]["text"], "写真1");
    assert_eq!(b["contents"][0]["parts"][3]["text"], "写真2");
    assert_eq!(b["generationConfig"]["temperature"], 0.1);
    assert_eq!(b["generationConfig"]["responseJsonSchema"], schema());
    let r = GenerateRequest::photo_judge("synthetic", &json!({}), &[(1, image())]).unwrap();
    assert_eq!(
        r.body_json_for_test()["contents"][0]["parts"][1]["text"],
        "Image 1"
    );
    let mut changed = p.manifest.clone();
    changed.sent.reverse();
    assert_ne!(changed.result_key(), p.key().input_hash);
    assert_eq!(changed.run_id(), p.key().target_key);
    changed = p.manifest.clone();
    changed.day = "2026-01-03".into();
    assert_ne!(changed.result_key(), p.key().input_hash);
    changed = p.manifest.clone();
    changed.model = "synthetic-other".into();
    assert_ne!(changed.result_key(), p.key().input_hash);
    assert_ne!(
        processor_version(MODEL),
        sha(format!("{MODEL}\0{PROMPT_VERSION}\0other-process").as_bytes())
    );
}

#[test]
fn answer_bounds_invalid_types_indexes_nonfinite_and_private_debug() {
    for s in [
        "null",
        "[]",
        r#"{"place_name":3}"#,
        r#"{"place_name":"x","country":{}}"#,
        r#"{"place_name":"x","geocode_query":1}"#,
        r#"{"place_name":"x","photos":{}}"#,
        r#"{"place_name":"x","photos":[{"index":true,"place_name":"x"}]}"#,
        r#"{"place_name":"x","photos":[{"index":1.5,"place_name":"x"}]}"#,
        r#"{"place_name":"x","photos":[{"index":18446744073709551616,"place_name":"x"}]}"#,
        r#"{"place_name":"x","photos":[{"index":1,"place_name":"x","geocode_query":null}]}"#,
        r#"{"place_name":"x","n":NaN}"#,
        r#"{"place_name":"x","n":1e9999}"#,
    ] {
        assert_eq!(Answer::parse(s, 2).unwrap_err(), DomainError::BadResponse);
    }
    for v in [
        json!({"place_name":"x".repeat(513)}),
        json!({"place_name":"x","geocode_query":"a\nb"}),
        json!({"place_name":"x","evidence":vec!["x";65]}),
        json!({"place_name":"x","evidence":["x".repeat(2049)]}),
        json!({"place_name":"x","photos":vec![json!({});65]}),
        json!({"place_name":"x","photos":[{"index":1,"place_name":"x"},{"index":1,"place_name":"x"}]}),
    ] {
        assert_eq!(
            Answer::parse(&v.to_string(), 2).unwrap_err(),
            DomainError::BadResponse
        );
    }
    assert_eq!(
        Answer::parse(&" ".repeat(1024 * 1024 + 1), 2).unwrap_err(),
        DomainError::BadResponse
    );
    let a=Answer::parse(r#"{"place_name":"privatecanary","precision":"unknown","evidence":false,"photos":[{"index":0,"place_name":"bad"},{"index":3,"place_name":"bad"}]}"#,2).unwrap();
    assert!(a.photos().is_empty());
    assert!(a.evidence().is_empty());
    assert!(!format!("{a:?}").contains("privatecanary"));
}

#[test]
fn success_is_atomic_and_subset_reuses_without_auth() {
    let mut db = Db::new();
    let p = db.seed();
    let before = crate::ai::tests::dump(&db.conn);
    let c = db.save(&p);
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 1);
    assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 0);
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 1);
    assert_eq!(
        complete(&mut db.conn, &c, &p, &answer(), T + 2).unwrap(),
        Completion::Rejected
    );
    // Only photo_loc_runs changed in Python tables; photos/suggestions stay byte-for-value identical.
    let after = crate::ai::tests::dump(&db.conn);
    assert_eq!(
        before
            .into_iter()
            .filter(|s| !s.starts_with("photo_loc_runs:"))
            .collect::<Vec<_>>(),
        after
            .into_iter()
            .filter(|s| !s.starts_with("photo_loc_runs:"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        db.conn
            .query_row("SELECT COUNT(*) FROM photo_loc_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.conn
        .execute(
            "UPDATE photos SET lat=1,lon=2,loc_source='manual' WHERE id=2",
            [],
        )
        .unwrap();
    let Lookup::Reusable(s) = db.lookup() else {
        panic!("manual SAVE should reuse")
    };
    assert_eq!(s.sent_ids(), [1, 2]);
    assert_eq!(s.photo_id(&s.answer().photos()[0]), Some(2));
    assert_eq!(s.attempts(), 1);
    let next = db.plan();
    let observed = db.observations(&next.day);
    assert!(matches!(
        enqueue(&mut db.conn, &next, &observed, T + 2).unwrap(),
        Queue::Reusable(_)
    ));
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 1);
}

#[test]
fn reuse_rejects_changes_rotation_delete_addition_and_lost_source_proof() {
    for sql in [
        "UPDATE photos SET rotation=90 WHERE id=2",
        "UPDATE photos SET md5='changed' WHERE id=1",
        "UPDATE photos SET mtime=2 WHERE id=1",
        "UPDATE photos SET taken_at='2026-01-03 10:00' WHERE id=1",
        "DELETE FROM photos WHERE id=2",
        "DELETE FROM photo_quality_features WHERE photo_id=1",
        "UPDATE photo_quality_features SET content_sha256=printf('%064d',0) WHERE photo_id=1",
    ] {
        let mut db = Db::new();
        let p = db.seed();
        db.save(&p);
        db.conn.execute_batch(sql).unwrap();
        assert!(!matches!(db.lookup(), Lookup::Reusable(_)), "{sql}");
    }
    let mut db = Db::new();
    let p = db.seed();
    db.save(&p);
    db.add(3, "2026-01-02 12:00");
    assert!(!matches!(db.lookup(), Lookup::Reusable(_)));
    let mut db = Db::new();
    let p = db.seed();
    db.save(&p);
    db.conn
        .execute_batch("UPDATE photos SET lat=1 WHERE id=2; DELETE FROM photos WHERE id=2")
        .unwrap();
    assert!(!matches!(db.lookup(), Lookup::Reusable(_)));
}

#[test]
fn every_compat_column_mutation_and_corrupt_history_prevent_trusted_use_or_resend() {
    for assignment in [
        "day='other'",
        "model='other'",
        "prompt_version='other'",
        "member_ids_json='[1]'",
        "sent_ids_json='[2,1]'",
        "status='error'",
        "attempts=4",
        "answer_json='privatecanary'",
        "error='privatecanary'",
        "analyzed_at='other'",
        "run_id='other'",
    ] {
        let mut db = Db::new();
        let p = db.seed();
        db.save(&p);
        db.conn
            .execute_batch(&format!("UPDATE photo_loc_runs SET {assignment}"))
            .unwrap();
        let l = db.lookup();
        assert!(!matches!(l, Lookup::Reusable(_)), "{assignment}");
        assert!(!format!("{l:?}").contains("privatecanary"));
        assert!(matches!(
            enqueue(&mut db.conn, &p, &[], T + 2).unwrap(),
            Queue::Unverified | Queue::LegacyUnverified
        ));
    }
    for assignment in [
        "integrity_sha256=printf('%064d',0)",
        "manifest_json='{}'",
        "answer_json='{}'",
        "attempts=3",
        "analyzed_at='other'",
        "result_key=printf('%064d',0)",
    ] {
        let mut db = Db::new();
        let p = db.seed();
        db.save(&p);
        db.conn
            .execute_batch(&format!(
                "UPDATE ai_location_suggest_results SET {assignment}"
            ))
            .unwrap();
        assert!(matches!(db.lookup(), Lookup::Unverified));
        assert!(matches!(
            enqueue(&mut db.conn, &p, &[], T + 2).unwrap(),
            Queue::Unverified
        ));
    }
}

#[test]
fn generations_with_same_python_id_keep_history_and_key_collisions_do_not_overwrite() {
    let mut db = Db::new();
    let p = db.seed();
    db.save(&p);
    db.conn
        .execute("UPDATE photos SET rotation=90", [])
        .unwrap();
    let p2 = db.plan();
    assert_eq!(p.key().target_key, p2.key().target_key);
    assert_ne!(p.key().input_hash, p2.key().input_hash);
    let c = db.save(&p2);
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 2);
    let Lookup::Reusable(s) = db.lookup() else {
        panic!("current generation")
    };
    assert_eq!(s.plan_hash(), p2.key().input_hash);
    assert_eq!(s.result_key(), p2.manifest.result_key());
    // Duplicate result is immutable even if a caller tries another completion with a live token.
    db.conn
        .execute(
            "UPDATE ai_jobs SET state='running',claim_token='synthetic-token' WHERE id=?1",
            [c.id],
        )
        .unwrap();
    let other = Answer::parse(r#"{"place_name":"other"}"#, 2).unwrap();
    assert!(complete(&mut db.conn, &c, &p2, &other, T + 3).is_err());
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 2);
    assert_eq!(
        complete(&mut db.conn, &c, &p2, &answer(), T + 4).unwrap(),
        Completion::Succeeded
    );
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 2);
}

#[test]
fn successful_generations_never_accumulate_as_failures() {
    let mut db = Db::new();
    let first = db.seed();
    let mut keys = std::collections::HashSet::new();
    for generation in 1..=8 {
        db.conn
            .execute("UPDATE photos SET mtime=?1", [generation])
            .unwrap();
        db.conn
            .execute(
                "UPDATE photo_quality_features SET source_mtime=?1",
                [generation],
            )
            .unwrap();
        let plan = db.plan();
        assert_eq!(plan.key().target_key, first.key().target_key);
        assert!(keys.insert(plan.key().input_hash));
        let claimed = db.save(&plan);
        assert_eq!(claimed.attempts, 0);
        assert_eq!(
            db.n("SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded' AND attempts=0"),
            generation
        );
        assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), generation);
        assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 1);
        assert_eq!(
            db.n("SELECT COUNT(*) FROM ai_location_suggest_results WHERE attempts=1"),
            generation
        );
        assert_eq!(
            db.n("SELECT COUNT(*) FROM ai_location_suggest_results"),
            generation
        );
        let Lookup::Reusable(saved) = db.lookup() else {
            panic!("current successful generation must remain reusable")
        };
        assert_eq!(saved.plan_hash(), plan.key().input_hash);
        assert_eq!(saved.result_key(), plan.manifest.result_key());
        assert_eq!(saved.attempts(), 1);
    }
}

#[test]
fn verified_done_inherits_real_failures_without_its_successful_request() {
    let mut db = Db::new();
    let first = db.seed();
    let mut claimed = db.claim(&first);
    for failure in 1..=4 {
        let now = T + failure * 1000;
        assert_eq!(
            jobs::record_failure_with(
                &mut db.conn,
                &claimed,
                now,
                ErrorCode::Timeout,
                Failure::Attempt,
                |_| panic!("nonterminal failure must not project")
            )
            .unwrap(),
            Some(JobState::Pending)
        );
        claimed = jobs::claim_next(
            &db.conn,
            now + 500,
            "synthetic-boot",
            "synthetic-token",
            &[JobKind::LocationSuggest],
        )
        .unwrap()
        .unwrap();
        assert_eq!(i64::from(claimed.attempts), failure);
    }
    assert_eq!(
        complete(&mut db.conn, &claimed, &first, &answer(), T + 5000).unwrap(),
        Completion::Succeeded
    );
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
    db.conn.execute("UPDATE photos SET mtime=2", []).unwrap();
    db.conn
        .execute("UPDATE photo_quality_features SET source_mtime=2", [])
        .unwrap();
    let next = db.plan();
    assert_eq!(next.key().target_key, first.key().target_key);
    assert_ne!(next.key().input_hash, first.key().input_hash);
    let claimed = db.save(&next);
    assert_eq!(claimed.attempts, 4);
    assert_eq!(
        db.n("SELECT COUNT(*) FROM ai_jobs WHERE state='succeeded' AND attempts=4"),
        2
    );
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
    assert_eq!(
        db.n("SELECT COUNT(*) FROM ai_location_suggest_results WHERE attempts=5"),
        2
    );
}

#[test]
fn completion_rejects_stale_token_and_changed_members_rolls_back_on_sql_error() {
    for mutation in [
        "UPDATE photos SET rotation=90 WHERE id=1",
        "DELETE FROM photos WHERE id=2",
        "UPDATE photos SET lat=1 WHERE id=1",
    ] {
        let mut db = Db::new();
        let p = db.seed();
        let c = db.claim(&p);
        db.conn.execute_batch(mutation).unwrap();
        assert_eq!(
            complete(&mut db.conn, &c, &p, &answer(), T + 1).unwrap(),
            Completion::Obsolete
        );
        assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 0);
        assert_eq!(db.n("SELECT COUNT(*) FROM photo_loc_runs"), 0);
    }
    let mut db = Db::new();
    let p = db.seed();
    let c = db.claim(&p);
    jobs::release_for_shutdown(&mut db.conn, "synthetic-boot", T + 1).unwrap();
    assert_eq!(
        complete(&mut db.conn, &c, &p, &answer(), T + 2).unwrap(),
        Completion::Rejected
    );
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 0);
    let mut db = Db::new();
    let p = db.seed();
    let c = db.claim(&p);
    db.conn.execute_batch("CREATE TRIGGER synthetic_fail BEFORE INSERT ON photo_loc_runs BEGIN SELECT RAISE(ABORT,'privatecanary'); END").unwrap();
    assert!(complete(&mut db.conn, &c, &p, &answer(), T + 1).is_err());
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 0);
    assert_eq!(db.n("SELECT COUNT(*) FROM photo_loc_runs"), 0);
    assert_eq!(
        db.n("SELECT COUNT(*) FROM ai_jobs WHERE state='running'"),
        1
    );
    db.conn
        .execute_batch("DROP TRIGGER synthetic_fail")
        .unwrap();
    assert_eq!(
        complete(&mut db.conn, &c, &p, &answer(), T + 2).unwrap(),
        Completion::Succeeded
    );
}

fn legacy(db: &Db, p: &Plan, status: &str, n: u32) {
    db.conn.execute("INSERT INTO photo_loc_runs VALUES(?1,'2026-01-02',?2,?3,'[1,2]','[1,2]',?4,?5,?6,'privatecanary','2026-01-02T00:00:00Z')",
        params![p.key().target_key,MODEL,PROMPT_VERSION,status,n,if status=="done" {answer().normalized_json()} else {String::new()}]).unwrap();
}
#[test]
fn legacy_paid_done_collision_and_subset_are_held_and_unchanged() {
    let mut db = Db::new();
    let p = db.seed();
    legacy(&db, &p, "done", 5);
    let before = crate::ai::tests::dump(&db.conn);
    assert!(matches!(db.lookup(), Lookup::LegacyUnverified));
    assert!(matches!(
        enqueue(&mut db.conn, &p, &[], T).unwrap(),
        Queue::LegacyUnverified
    ));
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 0);
    assert_eq!(before, crate::ai::tests::dump(&db.conn));
    db.conn.execute("UPDATE photos SET mtime=2", []).unwrap();
    db.conn
        .execute("UPDATE photo_quality_features SET source_mtime=2", [])
        .unwrap();
    let updated = db.plan();
    assert_eq!(updated.key().target_key, p.key().target_key);
    assert_ne!(updated.key().input_hash, p.key().input_hash);
    assert!(matches!(
        enqueue(&mut db.conn, &updated, &[], T + 1).unwrap(),
        Queue::LegacyUnverified
    ));
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 0);
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
    db.conn
        .execute("UPDATE photos SET lat=1 WHERE id=2", [])
        .unwrap();
    let next = db.plan();
    assert!(matches!(
        enqueue(&mut db.conn, &next, &[], T).unwrap(),
        Queue::LegacyUnverified
    ));
    assert!(!format!("{:?}", db.lookup()).contains("privatecanary"));
}

#[test]
fn legacy_attempts_seed_once_terminal_limit_and_manual_retry_is_only_reset() {
    for n in [4, 5, 20] {
        let mut db = Db::new();
        let p = db.seed();
        legacy(&db, &p, "error", n);
        assert!(matches!(
            enqueue(&mut db.conn, &p, &[], T).unwrap(),
            Queue::Enqueued(_)
        ));
        assert_eq!(db.n("SELECT attempts FROM ai_jobs"), i64::from(n.min(5)));
        if n >= 5 {
            assert!(
                jobs::claim_next(&db.conn, T, "boot", "token", &[JobKind::LocationSuggest])
                    .unwrap()
                    .is_none()
            );
            enqueue(&mut db.conn, &p, &[], T + 1).unwrap();
            assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 5);
            jobs::retry_failed(&mut db.conn, T + 2, jobs::RetryScope::All, |_, _| {
                Ok(jobs::Recheck::Requeue)
            })
            .unwrap();
            enqueue(&mut db.conn, &p, &[], T + 3).unwrap();
            assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 0);
        } else {
            let c = jobs::claim_next(&db.conn, T, "boot", "token", &[JobKind::LocationSuggest])
                .unwrap()
                .unwrap();
            assert_eq!(
                complete(&mut db.conn, &c, &p, &answer(), T + 1).unwrap(),
                Completion::Succeeded
            );
            assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
            assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 4);
        }
    }
}

#[test]
fn terminal_failure_projection_is_atomic_token_checked_and_retry_does_not_double_count() {
    let mut db = Db::new();
    let p = db.seed();
    legacy(&db, &p, "error", 4);
    let c = db.claim(&p);
    let result = jobs::record_failure_with(
        &mut db.conn,
        &c,
        T + 1,
        ErrorCode::BadResponse,
        Failure::Attempt,
        |tx| project_failure(tx, &p, T + 1),
    )
    .unwrap();
    assert_eq!(result, Some(JobState::Failed));
    assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 5);
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
    assert_eq!(
        db.n("SELECT COUNT(*) FROM photo_loc_runs WHERE error='bad_response'"),
        1
    );
    assert_eq!(
        jobs::record_failure_with(
            &mut db.conn,
            &c,
            T + 2,
            ErrorCode::Timeout,
            Failure::Attempt,
            |_| panic!("stale callback")
        )
        .unwrap(),
        None
    );
    let mut db = Db::new();
    let p = db.seed();
    let c = db.claim(&p);
    db.conn.execute_batch("CREATE TRIGGER synthetic_fail BEFORE INSERT ON photo_loc_runs BEGIN SELECT RAISE(ABORT,'privatecanary'); END").unwrap();
    assert!(
        jobs::record_failure_with(
            &mut db.conn,
            &c,
            T + 1,
            ErrorCode::InputInvalid,
            Failure::Permanent,
            |tx| project_failure(tx, &p, T + 1)
        )
        .is_err()
    );
    assert_eq!(
        db.n("SELECT COUNT(*) FROM ai_jobs WHERE state='running' AND attempts=0"),
        1
    );
    db.conn
        .execute_batch("DROP TRIGGER synthetic_fail; UPDATE photos SET rotation=90")
        .unwrap();
    assert_eq!(
        jobs::record_failure_with(
            &mut db.conn,
            &c,
            T + 2,
            ErrorCode::InputInvalid,
            Failure::Permanent,
            |tx| project_failure(tx, &p, T + 2)
        )
        .unwrap(),
        Some(JobState::Obsolete)
    );
    assert_eq!(db.n("SELECT COUNT(*) FROM photo_loc_runs"), 0);
}

#[test]
fn callback_changed_rolls_back_both_tables_even_after_writes() {
    let mut db = Db::new();
    let p = db.seed();
    let c = db.claim(&p);
    assert_eq!(
        jobs::complete(&mut db.conn, &c, T + 1, |tx| {
            super::repository::persist_answer(tx, &c, &p, &answer(), T + 1)?;
            Ok(InputCheck::Changed)
        })
        .unwrap(),
        Completion::Obsolete
    );
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 0);
    assert_eq!(db.n("SELECT COUNT(*) FROM photo_loc_runs"), 0);
}

#[test]
fn migration_is_additive_idempotent_and_pending_reports_exactly_one_new_table() {
    let mut db = Db::new();
    db.seed();
    legacy(&db, &db.plan(), "done", 1);
    db.conn.execute("INSERT INTO photo_loc_suggestions(photo_id,run_id,scope,place_name,model,analyzed_at) VALUES(1,'synthetic','day','合成都市','synthetic','2026-01-02T00:00:00Z')",[]).unwrap();
    db.conn
        .execute("DROP TABLE ai_location_suggest_results", [])
        .unwrap();
    let before = crate::ai::tests::dump(&db.conn);
    assert_eq!(
        crate::ai::pending_active(&db.conn, false, true, false, false).unwrap(),
        ["ai_location_suggest_results"]
    );
    assert_eq!(
        crate::ai::migrate_active(&mut db.conn, false, true, false, false).unwrap(),
        ["ai_location_suggest_results"]
    );
    assert!(
        crate::ai::migrate_active(&mut db.conn, false, true, false, false)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::ai::pending_active(&db.conn, false, true, false, false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(before, crate::ai::tests::dump(&db.conn));
    assert_eq!(
        db.n("SELECT COUNT(*) FROM pragma_table_info('photo_loc_runs')"),
        11
    );
    assert_eq!(
        db.n("SELECT COUNT(*) FROM pragma_table_info('photo_loc_suggestions')"),
        14
    );
}

fn config(
    dir: &TempDir,
    doc: &str,
    ingest: bool,
    disabled: Option<(&str, &str)>,
) -> crate::config::Config {
    let path = dir.path().join("synthetic-ai.json");
    std::fs::write(&path, doc).unwrap();
    crate::config::Config::load(
        |key| match key {
            "GEOFRAME_AI_CONFIG" => Some(path.to_string_lossy().into()),
            "GEOFRAME_INGEST" => Some(if ingest { "1" } else { "0" }.into()),
            key if disabled.is_some_and(|(k, _)| k == key) => disabled.map(|(_, v)| v.into()),
            _ => None,
        },
        &crate::config::Overrides {
            listen: None,
            data_dir: Some(dir.path().into()),
        },
    )
    .unwrap()
}
fn enabled_doc() -> String {
    let mut v: Value = serde_json::from_str(crate::ai::tests_settings::SAMPLE).unwrap();
    v["tasks"]["location_suggest"]["allow_cloud"] = json!(true);
    v["tasks"]["location_suggest"]["daily_request_limit"] = json!(10);
    v.to_string()
}
#[test]
fn guarded_start_creates_suggest_table_only_for_valid_enabled_configuration() {
    let enabled = enabled_doc();
    for (doc, disabled, want) in [
        (enabled.as_str(), None, true),
        ("{}", None, false),
        ("{", None, false),
        (crate::ai::tests_settings::SAMPLE, None, false),
        (enabled.as_str(), Some(("GEOFRAME_AI_ENABLED", "0")), false),
        (enabled.as_str(), Some(("GEOFRAME_LOC_SUGGEST", "0")), false),
        (
            enabled.as_str(),
            Some(("GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE", "0")),
            false,
        ),
    ] {
        let dir = TempDir::new("suggest-active-schema");
        let cfg = config(&dir, doc, true, disabled);
        let path = cfg.db_path();
        dbschema::migrate(&mut Connection::open(&path).unwrap()).unwrap();
        let read = crate::db::open_read_only(&path).unwrap();
        let before = crate::ai::tests::dump(&read);
        let plan = crate::ingest::plan(&cfg, &read).unwrap();
        assert_eq!(
            plan.schema
                .missing_objects
                .iter()
                .filter(|n| n.as_str() == "ai_location_suggest_results")
                .count(),
            usize::from(want)
        );
        drop(read);
        let ingest = crate::ingest::start_with(&cfg, &crate::guard::Guard::skip()).unwrap();
        let read = crate::db::open_read_only(&path).unwrap();
        assert_eq!(
            jobs::object_exists(&read, "table", "ai_location_suggest_results").unwrap(),
            want
        );
        assert_eq!(before, crate::ai::tests::dump(&read));
        assert!(
            crate::ai::worker::production_handlers_for_test(
                &cfg,
                crate::ai::provider::LanClient::new(&cfg.ai)
            )
            .iter()
            .filter(|h| h.kind() == JobKind::LocationSuggest || h.kind() == JobKind::LocationGeocode)
            .count() == if want { 2 } else { 0 }
        );
        drop(ingest);
    }
}

#[test]
fn ingest_zero_old_readonly_database_has_no_new_tables_or_writes() {
    let dir = TempDir::new("suggest-old-readonly");
    let (path, _) = crate::testutil::synthetic_catalog(dir.path(), 2);
    let cfg = config(&dir, &enabled_doc(), false, None);
    let before = std::fs::read(&path).unwrap();
    let conn = crate::db::open_read_only(&path).unwrap();
    crate::db::check_schema(&conn).unwrap();
    let targets = location_clue::load_targets(
        &conn,
        &DataDir::open(dir.path()).unwrap(),
        "synthetic-decision",
    )
    .unwrap();
    let days = group_days(
        &conn,
        &DataDir::open(dir.path()).unwrap(),
        targets,
        false,
        &cfg.infer_windows_s(),
    )
    .unwrap();
    for d in days {
        assert!(matches!(
            lookup(&conn, &d, MODEL, &[]).unwrap(),
            Lookup::Absent
        ));
    }
    assert!(
        !crate::ingest::plan(&cfg, &conn)
            .unwrap()
            .schema
            .missing_objects
            .contains(&"ai_location_suggest_results".into())
    );
    assert!(!jobs::object_exists(&conn, "table", "ai_location_suggest_results").unwrap());
    assert_eq!(before, std::fs::read(&path).unwrap());
}

#[test]
fn fallback_generation_observation_tracks_file_changes_and_allows_saved_sent_photo() {
    let mut db = Db::new();
    db.add(1, "2026-01-02 10:00");
    db.add(2, "2026-01-02 11:00");
    db.conn.execute_batch("UPDATE photos SET display='',drive_id='synthetic-fallback' WHERE id=2; DELETE FROM photo_quality_features WHERE photo_id=2").unwrap();
    std::fs::create_dir_all(db.data.root().join("derivatives")).unwrap();
    let file = db.data.root().join("derivatives/synthetic-fallback.jpg");
    std::fs::write(&file, b"synthetic-bytes-a").unwrap();
    let p = db.plan();
    db.save(&p);
    db.conn
        .execute("UPDATE photos SET lat=1 WHERE id=2", [])
        .unwrap();
    assert!(matches!(db.lookup(), Lookup::Reusable(_)));
    std::fs::write(&file, b"synthetic-bytes-b").unwrap();
    assert!(!matches!(db.lookup(), Lookup::Reusable(_)));
    std::fs::remove_file(file).unwrap();
    assert!(!matches!(db.lookup(), Lookup::Reusable(_)));
}

#[test]
fn generation_round_trip_restores_only_owned_projection_without_new_job_or_auth() {
    let mut db = Db::new();
    let a = db.seed();
    db.save(&a);
    db.conn
        .execute("UPDATE photos SET rotation=90", [])
        .unwrap();
    let b = db.plan();
    let claimed = db.claim(&b);
    // Both successful generations now have attempts=1; use the actual later save time
    // to distinguish their otherwise identical Python projections.
    assert_eq!(
        complete(&mut db.conn, &claimed, &b, &answer(), T + 2).unwrap(),
        Completion::Succeeded
    );
    db.conn.execute("UPDATE photos SET rotation=0", []).unwrap();
    let day = db.days().remove(0);
    let observed = db.observations(&day);
    assert!(matches!(
        lookup(&db.conn, &day, MODEL, &observed).unwrap(),
        Lookup::ProjectionPending
    ));
    let Lookup::Reusable(saved) = restore_projection(&mut db.conn, &day, MODEL, &observed).unwrap()
    else {
        panic!("restore verified A")
    };
    assert_eq!(saved.plan_hash(), a.key().input_hash);
    assert_eq!(saved.result_key(), a.manifest.result_key());
    assert!(saved.applicable(&db.conn, &day, &observed).unwrap());
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 2);
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 2);
    // Changes made after a previous lookup cannot pass the final c2 candidate CAS.
    db.conn
        .execute("UPDATE photo_loc_runs SET error='privatecanary'", [])
        .unwrap();
    assert!(!saved.applicable(&db.conn, &day, &observed).unwrap());
    assert!(matches!(
        restore_projection(&mut db.conn, &day, MODEL, &observed).unwrap(),
        Lookup::LegacyUnverified
    ));
}

#[test]
fn db_callbacks_require_a_transaction_and_worker_style_enqueue_seeds_legacy() {
    let mut db = Db::new();
    let p = db.seed();
    legacy(&db, &p, "error", 4);
    assert!(enqueue_in(&db.conn, &p, &[], T).is_err());
    let tx = db.conn.transaction().unwrap();
    assert!(matches!(
        enqueue_in(&tx, &p, &[], T).unwrap(),
        Queue::Enqueued(_)
    ));
    tx.commit().unwrap();
    let c = jobs::claim_next(&db.conn, T, "boot", "token", &[JobKind::LocationSuggest])
        .unwrap()
        .unwrap();
    assert!(persist_answer(&db.conn, &c, &p, &answer(), T + 1).is_err());
    assert!(project_failure(&db.conn, &p, T + 1).is_err());
    assert_eq!(
        jobs::complete(&mut db.conn, &c, T + 1, |tx| persist_answer(
            tx,
            &c,
            &p,
            &answer(),
            T + 1
        ))
        .unwrap(),
        Completion::Succeeded
    );
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
}

#[test]
fn invalid_plan_never_accepts_wrong_source_or_order_and_sent_jpeg_changes_result_identity_only() {
    let db = Db::new();
    let p = db.seed();
    let day = db.days().remove(0);
    let wrong = |reverse: bool, hash: bool| {
        let mut ids = day.sent_ids.clone();
        if reverse {
            ids.reverse();
        }
        ids.into_iter()
            .map(|id| SendImage {
                photo_id: id,
                source_sha256: if hash {
                    "0".repeat(64)
                } else {
                    day.members
                        .iter()
                        .find(|g| g.photo_id() == id)
                        .unwrap()
                        .source_sha256()
                        .unwrap()
                        .into()
                },
                image: image(),
            })
            .collect()
    };
    assert_eq!(
        Plan::new(day.clone(), MODEL, wrong(true, false)).unwrap_err(),
        DomainError::InvalidInput
    );
    assert_eq!(
        Plan::new(day.clone(), MODEL, wrong(false, true)).unwrap_err(),
        DomainError::InvalidInput
    );
    let images = day
        .sent_ids
        .iter()
        .map(|id| SendImage {
            photo_id: *id,
            source_sha256: day
                .members
                .iter()
                .find(|g| g.photo_id() == *id)
                .unwrap()
                .source_sha256()
                .unwrap()
                .into(),
            image: PreparedImage::encode_jpeg_rgb(
                &[64; 16 * 8 * 3],
                16,
                8,
                88,
                ImageProfile::Decision,
            )
            .unwrap(),
        })
        .collect();
    let other = Plan::new(day, MODEL, images).unwrap();
    // 送信 JPEG が違えば履歴の同一性（result_key）は変わるが、ジョブのキーは変わらない
    // （JPEG は元画像の SHA と処理版から決まるので、画像を作る前にキーを決められる）。
    assert_ne!(p.manifest.result_key(), other.manifest.result_key());
    assert_eq!(p.key(), other.key());
    let failed = db
        .targets()
        .into_iter()
        .map(|mut t| {
            t.readiness = Readiness::Failed;
            t
        })
        .collect();
    let day = group_days(&db.conn, &db.data, failed, false, &[43200.])
        .unwrap()
        .remove(0);
    assert!(!day.pending());
    assert_eq!(
        Plan::new(day, MODEL, Vec::new()).unwrap_err(),
        DomainError::NoImages
    );
}

#[test]
fn saved_photo_indexes_use_original_rank_even_if_current_clue_order_changes() {
    let mut db = Db::new();
    db.add(1, "2026-01-02 10:00");
    db.add(9, "2026-01-02 11:00");
    let mut targets = db.targets();
    targets[1].readiness = Readiness::Ready(location_clue::Clue::new(0.01, "landmark").unwrap());
    let p = make_plan(
        group_days(&db.conn, &db.data, targets, false, &[43200.])
            .unwrap()
            .remove(0),
    );
    assert_eq!(p.sent_ids(), [9, 1]);
    db.save(&p);
    let Lookup::Reusable(saved) = db.lookup() else {
        panic!("reuse original send order")
    };
    assert_eq!(saved.photo_id(&saved.answer().photos()[0]), Some(1));
}

#[test]
fn past_prompt_and_processor_are_preserved_but_not_reused_as_current_results() {
    for prompt in [false, true] {
        let mut db = Db::new();
        let current = db.seed();
        let mut old = current.clone();
        if prompt {
            old.manifest.prompt_version = "loc-v1".into();
        } else {
            old.manifest.processor_version = sha(b"past-processor");
        }
        // 過去の処理版は、ジョブのキーも別（処理版を含む）。
        old.intent.key = JobKey::new(
            JobKind::LocationSuggest,
            old.manifest.run_id(),
            old.manifest.plan_hash(),
            &old.manifest.processor_version,
        );
        db.save(&old);
        assert!(matches!(db.lookup(), Lookup::Absent));
        db.save(&current);
        assert_eq!(db.n("SELECT COUNT(*) FROM ai_location_suggest_results"), 2);
        let Lookup::Reusable(saved) = db.lookup() else {
            panic!("current process")
        };
        assert_eq!(saved.plan_hash(), current.key().input_hash);
        assert_eq!(saved.result_key(), current.manifest.result_key());
    }
}

#[test]
fn attempt_five_is_terminal_and_generation_oscillation_does_not_reset_it() {
    let mut db = Db::new();
    let p = db.seed();
    legacy(&db, &p, "error", 3);
    let c = db.claim(&p);
    assert_eq!(
        jobs::record_failure_with(
            &mut db.conn,
            &c,
            T + 1,
            ErrorCode::Timeout,
            Failure::Attempt,
            |_| panic!("nonterminal projection")
        )
        .unwrap(),
        Some(JobState::Pending)
    );
    assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 4);
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 3);
    let c = jobs::claim_next(
        &db.conn,
        T + 1000,
        "boot",
        "token2",
        &[JobKind::LocationSuggest],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        jobs::record_failure_with(
            &mut db.conn,
            &c,
            T + 1001,
            ErrorCode::Timeout,
            Failure::Attempt,
            |tx| project_failure(tx, &p, T + 1001)
        )
        .unwrap(),
        Some(JobState::Failed)
    );
    assert_eq!(db.n("SELECT attempts FROM photo_loc_runs"), 5);
    let mut other = p.key();
    other.input_hash = sha(b"other-generation");
    jobs::supersede(&mut db.conn, &other, T + 1002).unwrap();
    assert!(matches!(
        enqueue(&mut db.conn, &p, &[], T + 1003).unwrap(),
        Queue::Enqueued(jobs::Enqueued::Existing(JobState::Failed))
    ));
    assert_eq!(db.n("SELECT attempts FROM ai_jobs"), 5);
}

#[test]
fn actual_source_update_deletion_or_missing_proof_holds_paid_reuse_and_rebilling() {
    for manual in [false, true] {
        for deletion in [false, true] {
            let mut db = Db::new();
            let p = db.seed();
            db.save(&p);
            if manual {
                db.conn
                    .execute("UPDATE photos SET lat=1 WHERE id=2", [])
                    .unwrap();
            }
            let day = db.days().remove(0);
            assert!(matches!(
                lookup(&db.conn, &day, MODEL, &[]).unwrap(),
                Lookup::Unverified
            ));
            let file = db.data.root().join("photos/synthetic-2.jpg");
            if deletion {
                std::fs::remove_file(file).unwrap();
            } else {
                std::fs::write(file, b"synthetic-source-changed").unwrap();
            }
            assert!(
                SourceObservation::observe(&db.conn, &db.data, 2)
                    .unwrap()
                    .is_none()
            );
            assert!(matches!(db.lookup(), Lookup::Unverified));
            let plan = db.plan();
            let observations = db.observations(&plan.day);
            assert!(matches!(
                enqueue(&mut db.conn, &plan, &observations, T + 2).unwrap(),
                Queue::Unverified
            ));
            assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 1);
        }
    }
}

#[test]
fn planned_prompt_date_must_match_the_current_database_group() {
    let mut db = Db::new();
    db.seed();
    let targets = db
        .targets()
        .into_iter()
        .map(|mut t| {
            t.taken_at = "2026-01-03 10:00".into();
            t
        })
        .collect();
    let day = group_days(&db.conn, &db.data, targets, false, &[43200.])
        .unwrap()
        .remove(0);
    let plan = make_plan(day);
    assert!(matches!(
        enqueue(&mut db.conn, &plan, &[], T).unwrap(),
        Queue::Changed
    ));
    assert_eq!(db.n("SELECT COUNT(*) FROM ai_jobs"), 0);
}
