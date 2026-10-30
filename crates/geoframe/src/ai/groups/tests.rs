use super::*;
use crate::{
    ai::{index, jobs, rank, settings::Normalization, space::SpaceIdentity},
    schema,
    testutil::TempDir,
};
use rusqlite::{Connection, params, types::Value};
use serde_json::{Value as Json, json};

fn identity(dim: u32) -> SpaceIdentity {
    SpaceIdentity::new(
        "scene-a",
        "shared-a",
        "model-a",
        "rev-a",
        "img-a",
        "txt-a",
        dim,
        Normalization::Raw,
    )
    .unwrap()
}
fn db() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c).unwrap();
    index::migrate(&mut c).unwrap();
    jobs::migrate(&mut c).unwrap();
    migrate(&mut c).unwrap();
    activate(&mut c, &identity(2));
    c
}
fn activate(c: &mut Connection, s: &SpaceIdentity) {
    index::register_space(c, s, 0).unwrap();
    c.execute("INSERT INTO settings VALUES('ai_active_space',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[s.space_id()]).unwrap();
}
fn photo(c: &Connection, id: i64, seconds: i64, sha: i64) {
    let time = format!(
        "2026-01-01 {:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    c.execute("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at,lat,lon) VALUES(?1,'local','synthetic',?2,'m',1,0,?3,1,2)",params![id,format!("synthetic-{id}"),time]).unwrap();
    c.execute("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
       VALUES(?1,?2,'m',1,?3,'0',5,128,90,0,0,100,'','synthetic')",params![id,format!("synthetic-{id}"),format!("{sha:064x}")]).unwrap();
}
fn vector(c: &mut Connection, sha: i64, v: &[f32]) {
    index::store_vector(
        c,
        &identity(v.len() as u32),
        &format!("{sha:064x}"),
        &"f".repeat(64),
        v,
        0,
    )
    .unwrap();
}
fn ready(c: &mut Connection, id: i64, seconds: i64) {
    photo(c, id, seconds, id);
    vector(c, id, &[1., 0.]);
}
fn plan(c: &Connection) -> Plan {
    compute(
        &snapshot(c).unwrap().unwrap(),
        Options::default(),
        Err(rank::Reason::Unconfigured),
        &|| false,
    )
    .unwrap()
}
fn fail(c: &mut Connection, sha: i64) -> jobs::JobKey {
    let states = index::scan(c, Some(&identity(2)), false).unwrap();
    let state = states
        .iter()
        .find(|s| s.input_sha256() == format!("{sha:064x}"))
        .unwrap();
    let key = state.job_key(&identity(2));
    jobs::enqueue(c, &key, 0).unwrap();
    c.execute("UPDATE ai_jobs SET state='failed',terminal=1,last_error_code='input' WHERE target_key=?1 AND input_hash=?2 AND processor_version=?3",params![key.target_key,key.input_hash,key.processor_version]).unwrap();
    key
}
fn pair() -> Connection {
    let mut c = db();
    ready(&mut c, 1, 0);
    ready(&mut c, 2, 100);
    c
}
fn rows(c: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut stmt = c
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
        .unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| (0..n).map(|i| r.get(i)).collect())
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn legacy(c: &Connection, ids: &[i64]) {
    c.execute("INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene,judge_model,judge_version) VALUES('old','scene','old','synthetic','judged',1,'synthetic-model','old-prompt')",[]).unwrap();
    for (rank, id) in ids.iter().enumerate() {
        c.execute("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error)
           SELECT id,display,md5,mtime,?2,'old-model',2,zeroblob(8),'synthetic','' FROM photos WHERE id=?1",params![id,format!("{id:064x}")]).unwrap();
        c.execute("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,shortlisted,local_score) VALUES('old',?1,?2,?3,1,50)",params![id,rank as i64+1,rank==0]).unwrap();
    }
}
fn rank_json(s: &SpaceIdentity) -> Json {
    json!({"format":"geoframe-rank-linear-v1","name":"SECRET-NAME","embedding_model":s.model_id(),"embedding_dim":s.dimension(),
       "base_features":["sharpness"],"base_weights":[-1.0],"pca_mean":vec![0.0;s.dimension() as usize],
       "pca_components":[vec![0.0;s.dimension() as usize]],"pca_weights":[0.0],
       "compatibility":{"space_fingerprint":s.fingerprint(),"embedding_processor_version":index::image_embedding_processor_version(s),"input_representation":"stored-f32-v1"}})
}
fn model(j: &Json) -> Result<rank::Model, rank::Reason> {
    rank::Model::parse(&serde_json::to_vec(j).unwrap(), &identity(2))
}

#[test]
fn python_components_rank_ties_normalization_and_shortlist_fixture() {
    let fixture: Json = serde_json::from_str(include_str!("python_fixture.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mut c = db();
        for input in case["inputs"].as_array().unwrap() {
            let id = input["id"].as_i64().unwrap();
            photo(&c, id, input["seconds"].as_i64().unwrap(), id);
            c.execute(
                "UPDATE photo_quality_features SET sharpness=?2 WHERE photo_id=?1",
                params![id, input["sharpness"].as_f64().unwrap()],
            )
            .unwrap();
            if let Some(v) = input["vector"].as_array() {
                vector(
                    &mut c,
                    id,
                    &v.iter()
                        .map(|n| n.as_f64().unwrap() as f32)
                        .collect::<Vec<_>>(),
                );
            }
        }
        let mut raw = fixture["model"].clone();
        raw["compatibility"] = rank_json(&identity(2))["compatibility"].clone();
        let m = model(&raw).unwrap();
        let snapshot = snapshot(&c).unwrap().unwrap();
        let p = compute(
            &snapshot,
            Options::new(900, case["similarity"].as_f64().unwrap(), 6).unwrap(),
            if case["learned"] == true {
                Ok(&m)
            } else {
                Err(rank::Reason::Unconfigured)
            },
            &|| false,
        )
        .unwrap();
        let mut actual:Vec<Json>=p.groups.iter().filter(|g|g.state!=State::Waiting&&g.members.len()>1).map(|g|json!({"ids":g.members.iter().map(|m|m.photo_id).collect::<Vec<_>>(),"scores":g.members.iter().map(|m|m.score.unwrap()).collect::<Vec<_>>(),"shortlist":g.shortlist_ids()})).collect();
        actual.sort_by_key(|g| {
            let mut ids = g["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_i64().unwrap())
                .collect::<Vec<_>>();
            ids.sort();
            ids
        });
        assert_eq!(
            json!(actual),
            case["groups"],
            "synthetic case {}",
            case["name"]
        );
    }
}
#[test]
fn window_equality_excess_and_threshold_equality() {
    let mut c = db();
    ready(&mut c, 1, 0);
    ready(&mut c, 2, 900);
    ready(&mut c, 3, 1801);
    let s = snapshot(&c).unwrap().unwrap();
    let p = compute(
        &s,
        Options::new(900, 1.0, 6).unwrap(),
        Err(rank::Reason::Unconfigured),
        &|| false,
    )
    .unwrap();
    assert_eq!(
        p.groups.iter().map(|g| g.members.len()).collect::<Vec<_>>(),
        [2, 1]
    );
}
#[test]
fn pending_window_bounds_include_unanalyzed_and_unregistered() {
    for (time, wait) in [(1000, true), (1001, false)] {
        let c = pair();
        photo(&c, 3, time, 3);
        for unanalyzed in [false, true] {
            if unanalyzed {
                c.execute("DELETE FROM photo_quality_features WHERE photo_id=3", [])
                    .unwrap();
            }
            let p = plan(&c);
            assert_eq!(p.groups[0].state == State::Waiting, wait);
            assert_eq!(p.groups[0].members[0].score.is_none(), wait);
        }
    }
}
#[test]
fn pending_running_deferred_obsolete_do_not_complete_window() {
    for state in ["pending", "running", "deferred", "obsolete", "succeeded"] {
        let mut c = pair();
        photo(&c, 3, 100, 3);
        let key = fail(&mut c, 3);
        c.execute(
            "UPDATE ai_jobs SET state=?1 WHERE input_hash=?2",
            params![state, key.input_hash],
        )
        .unwrap();
        assert_eq!(plan(&c).groups[0].state, State::Waiting);
    }
}
#[test]
fn current_failed_excluded_retry_returns_and_all_failed_safe() {
    let mut c = pair();
    photo(&c, 3, 100, 3);
    fail(&mut c, 3);
    assert_eq!(plan(&c).groups[0].members.len(), 2);
    vector(&mut c, 3, &[1., 0.]);
    assert_eq!(plan(&c).groups[0].members.len(), 3);
    c.execute("DELETE FROM ai_embedding_vectors", []).unwrap();
    for id in 1..=3 {
        fail(&mut c, id);
    }
    let p = plan(&c);
    assert!(p.groups.is_empty());
    assert_eq!(
        snapshot(&c)
            .unwrap()
            .unwrap()
            .photos
            .iter()
            .filter(|p| p.readiness == Readiness::FailedEmbedding)
            .count(),
        3
    );
}
#[test]
fn failed_old_generation_other_processor_and_other_space_do_not_exclude() {
    for change in [
        "UPDATE photos SET rotation=90 WHERE id=3",
        "UPDATE ai_jobs SET processor_version='other'",
        "UPDATE ai_jobs SET target_key='other'",
    ] {
        let mut c = pair();
        photo(&c, 3, 100, 3);
        fail(&mut c, 3);
        c.execute(change, []).unwrap();
        assert_eq!(plan(&c).groups[0].state, State::Waiting);
    }
}
#[test]
fn duplicates_all_members_survive_deletion_and_update() {
    let c = pair();
    photo(&c, 3, 100, 1);
    let p = plan(&c);
    assert_eq!(p.groups[0].members.len(), 3);
    c.execute("DELETE FROM photos WHERE id=1", []).unwrap();
    let p = plan(&c);
    assert_eq!(
        p.groups[0]
            .members
            .iter()
            .map(|m| m.photo_id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    c.execute("UPDATE photos SET md5='new' WHERE id=2", [])
        .unwrap();
    let p = plan(&c);
    assert_eq!(p.groups[0].members[0].photo_id, 3);
    assert_eq!(p.groups[0].state, State::Waiting);
}
#[test]
fn failed_duplicate_generation_changes_outside_window_invalidate() {
    let mut c = pair();
    photo(&c, 3, 100, 3);
    photo(&c, 4, 10000, 3);
    fail(&mut c, 3);
    let p = plan(&c);
    assert_eq!(p.groups[0].state, State::LocalRanked);
    c.execute("DELETE FROM photos WHERE id=4", []).unwrap();
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
    assert_eq!(plan(&c).groups[0].state, State::Waiting);
}
#[test]
fn malformed_times_are_explicit_exclusions_without_primary_writes() {
    let c = pair();
    for (id, value, expected) in [
        (3, Value::Null, Readiness::MissingTime),
        (4, Value::Text("bad-time".into()), Readiness::InvalidTime),
    ] {
        photo(&c, id, 100, id);
        c.execute(
            "UPDATE photos SET taken_at=?2 WHERE id=?1",
            params![id, value],
        )
        .unwrap();
        assert_eq!(
            snapshot(&c)
                .unwrap()
                .unwrap()
                .photos
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .readiness,
            expected
        );
    }
    assert_eq!(plan(&c).groups[0].state, State::LocalRanked);
}
#[test]
fn corrupt_wrong_dimension_zero_nan_and_foreign_vectors_wait() {
    for blob in [
        vec![0_u8; 8],
        vec![0_u8; 4],
        crate::ai::space::encode(&[f32::NAN, 1.]),
    ] {
        let c = pair();
        photo(&c, 3, 100, 3);
        c.execute("PRAGMA ignore_check_constraints=ON", []).unwrap();
        c.execute(
            "INSERT INTO ai_embedding_vectors VALUES('scene-a',?1,?2,2,?3,0)",
            params![format!("{:064x}", 3), "f".repeat(64), blob],
        )
        .unwrap();
        assert_eq!(plan(&c).groups[0].state, State::Waiting);
    }
    let mut c = pair();
    photo(&c, 3, 100, 3);
    let other = SpaceIdentity::new(
        "other",
        "other",
        "model-a",
        "other",
        "img-a",
        "txt-a",
        2,
        Normalization::Raw,
    )
    .unwrap();
    index::register_space(&mut c, &other, 0).unwrap();
    index::store_vector(
        &mut c,
        &other,
        &format!("{:064x}", 3),
        &"f".repeat(64),
        &[1., 0.],
        0,
    )
    .unwrap();
    assert_eq!(plan(&c).groups[0].state, State::Waiting);
}
#[test]
fn whole_membership_not_only_shortlist_changes_result_identity() {
    let mut c = db();
    for id in 1..=8 {
        ready(&mut c, id, 0);
    }
    let before = plan(&c);
    c.execute("UPDATE photos SET rotation=90 WHERE id=8", [])
        .unwrap();
    let after = plan(&c);
    assert_eq!(
        before.groups[0].shortlist_ids(),
        after.groups[0].shortlist_ids()
    );
    assert_ne!(before.groups[0].result_id, after.groups[0].result_id);
}
#[test]
fn options_full_space_and_effective_weight_digest_define_processor() {
    let s = identity(2);
    let a = Options::default().processor_version(&s, None);
    for changed in [
        Options::new(899, 0.83, 6).unwrap(),
        Options::new(900, 0.84, 6).unwrap(),
        Options::new(900, 0.83, 5).unwrap(),
    ] {
        assert_ne!(a, changed.processor_version(&s, None));
    }
    let other = SpaceIdentity::new(
        "scene-a",
        "shared-a",
        "model-a",
        "rev-b",
        "img-a",
        "txt-a",
        2,
        Normalization::Raw,
    )
    .unwrap();
    assert_ne!(a, Options::default().processor_version(&other, None));
    assert_ne!(
        a,
        Options::default().processor_version(&s, Some("synthetic"))
    );
}
#[test]
fn save_cas_manual_favorite_hidden_rotation_delete_and_features() {
    for sql in [
        "UPDATE photos SET lat=3,lon=4,loc_source='manual' WHERE id=1",
        "UPDATE photos SET favorite=1 WHERE id=1",
        "UPDATE photos SET hidden=1 WHERE id=1",
        "UPDATE photos SET rotation=90 WHERE id=1",
        "UPDATE photos SET md5='new' WHERE id=1",
        "UPDATE photos SET mtime=2 WHERE id=1",
        "UPDATE photos SET display='changed' WHERE id=1",
        "UPDATE photos SET taken_at='2026-01-02 00:00' WHERE id=1",
        "DELETE FROM photos WHERE id=1",
        "UPDATE photo_quality_features SET sharpness=6 WHERE photo_id=1",
    ] {
        let mut c = pair();
        let p = plan(&c);
        c.execute(sql, []).unwrap();
        assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
        assert!(rows(&c, "ai_scene_groups").is_empty());
    }
}
#[test]
fn same_window_additions_refuse_but_unrelated_additions_make_progress() {
    let mut c = pair();
    let p = plan(&c);
    photo(&c, 3, 10000, 3);
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stored);
    let p = plan(&c);
    photo(&c, 4, 100, 4);
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
}
#[test]
fn atomic_rollback_failure_interruption_idempotence_and_restart() {
    let mut c = pair();
    let p = plan(&c);
    c.execute_batch("CREATE TRIGGER break_scene BEFORE INSERT ON ai_scene_members WHEN NEW.photo_id=2 BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert_eq!(save(&mut c, &p, 0, &|| false), Err(Error::Database));
    for t in ["ai_scene_groups", "ai_scene_members", "ai_scene_active"] {
        assert!(rows(&c, t).is_empty());
    }
    c.execute_batch("DROP TRIGGER break_scene").unwrap();
    let calls = std::cell::Cell::new(0);
    assert_eq!(
        save(&mut c, &p, 0, &|| {
            calls.set(calls.get() + 1);
            calls.get() > 1
        })
        .unwrap(),
        Saved::Interrupted
    );
    assert!(rows(&c, "ai_scene_groups").is_empty());
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stored);
    let changes = c.total_changes();
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Unchanged);
    assert_eq!(c.total_changes(), changes);
    let read = load_current(&c, p.groups[0].processor_version()).unwrap();
    assert_eq!(read.groups().len(), 1);
}
#[test]
fn current_reads_reject_stale_input_old_processor_and_other_space_keep_history() {
    let mut c = pair();
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    assert!(load_current(&c, "other").unwrap().groups().is_empty());
    c.execute("UPDATE photos SET hidden=1 WHERE id=1", [])
        .unwrap();
    assert!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .is_empty()
    );
    let other = SpaceIdentity::new(
        "other",
        "shared-b",
        "model-a",
        "rev-b",
        "img-a",
        "txt-a",
        2,
        Normalization::Raw,
    )
    .unwrap();
    activate(&mut c, &other);
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
    assert!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .is_empty()
    );
    assert_eq!(rows(&c, "ai_scene_groups").len(), 1);
    activate(&mut c, &identity(2));
    c.execute("UPDATE photos SET hidden=0 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
}
#[test]
fn waiting_never_publishes_and_empty_plan_keeps_old_selection() {
    let mut c = pair();
    legacy(&c, &[1, 2]);
    let old = rows(&c, "photo_ai_groups");
    photo(&c, 3, 100, 3);
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    assert!(rows(&c, "ai_scene_active").is_empty());
    assert!(rows(&c, "photo_ai_groups") == old);
    c.execute("DELETE FROM ai_embedding_vectors", []).unwrap();
    let p = plan(&c);
    assert!(p.groups.is_empty());
    assert!(rows(&c, "photo_ai_groups") == old);
}

#[test]
fn current_pointer_membership_must_match_ids_not_only_count() {
    let mut c = pair();
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    c.execute(
        "UPDATE ai_scene_active SET photo_id=999 WHERE photo_id=2",
        [],
    )
    .unwrap();
    assert!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .is_empty()
    );
    assert_ne!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Unchanged);
    let fresh = plan(&c);
    assert_eq!(save(&mut c, &fresh, 0, &|| false).unwrap(), Saved::Stored);
    assert_eq!(
        load_current(&c, fresh.groups[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
    assert_eq!(rows(&c, "ai_scene_active").len(), 2);
}

#[test]
fn stored_scene_corruption_is_rejected_by_reads_and_unchanged_checks() {
    for case in 0..12 {
        let mut c = pair();
        let p = plan(&c);
        save(&mut c, &p, 0, &|| false).unwrap();
        match case {
            0 => c
                .execute(
                    "UPDATE ai_scene_members SET generation='synthetic-wrong' WHERE photo_id=2",
                    [],
                )
                .unwrap(),
            1 => {
                c.execute("DELETE FROM ai_scene_members WHERE photo_id=2", [])
                    .unwrap();
                c.execute("DELETE FROM ai_scene_active WHERE photo_id=2", [])
                    .unwrap()
            }
            2 => c
                .execute(
                    "UPDATE ai_scene_members SET score=?1 WHERE photo_id=2",
                    [f64::INFINITY],
                )
                .unwrap(),
            3 => c
                .execute("UPDATE ai_scene_members SET rank=3 WHERE rank=2", [])
                .unwrap(),
            4 => c
                .execute(
                    "UPDATE ai_scene_members SET representative=1 WHERE rank=2",
                    [],
                )
                .unwrap(),
            5 => c
                .execute("UPDATE ai_scene_members SET shortlisted=2 WHERE rank=2", [])
                .unwrap(),
            6 => c
                .execute(
                    "UPDATE ai_scene_groups SET input_generation='synthetic-wrong'",
                    [],
                )
                .unwrap(),
            7 => c
                .execute("UPDATE ai_scene_groups SET shortlist=1", [])
                .unwrap(),
            8 => c
                .execute("UPDATE ai_scene_members SET shown=0 WHERE rank=2", [])
                .unwrap(),
            9 => c
                .execute("UPDATE ai_scene_members SET score=101 WHERE rank=2", [])
                .unwrap(),
            10 => c
                .execute(
                    "UPDATE ai_scene_groups SET shortlist='review-invalid-type'",
                    [],
                )
                .unwrap(),
            _ => {
                c.execute_batch(
                    "PRAGMA foreign_keys=OFF;
                    UPDATE ai_scene_groups SET result_id='synthetic-wrong-result';
                    UPDATE ai_scene_members SET result_id='synthetic-wrong-result';
                    UPDATE ai_scene_active SET result_id='synthetic-wrong-result';",
                )
                .unwrap();
                0
            }
        };
        assert!(
            matches!(
                load_current(&c, p.groups[0].processor_version()),
                Err(Error::CorruptResult)
            ),
            "case {case}"
        );
        assert_ne!(
            save(&mut c, &p, 0, &|| false).unwrap(),
            Saved::Unchanged,
            "case {case}"
        );
        let fresh = plan(&c);
        assert_eq!(
            save(&mut c, &fresh, 0, &|| false).unwrap(),
            Saved::Stored,
            "case {case}"
        );
        assert_eq!(
            load_current(&c, fresh.groups[0].processor_version())
                .unwrap()
                .groups()
                .len(),
            1,
            "case {case}"
        );
    }
}
#[test]
fn overlap_disjoint_exact_partial_and_visibility_share_selection_rules() {
    for ids in [vec![1, 2], vec![1, 3], vec![3, 4]] {
        let mut c = pair();
        ready(&mut c, 3, 10000);
        ready(&mut c, 4, 10000);
        legacy(&c, &ids);
        let p = plan(&c);
        let group = p
            .groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.photo_id == 1))
            .unwrap();
        let expected = if ids == [1, 2] {
            Overlap::Exact
        } else if ids == [1, 3] {
            Overlap::Partial
        } else {
            Overlap::Disjoint
        };
        assert_eq!(group.overlap, expected);
        assert_eq!(
            group.state == State::ReviewOverlap,
            expected == Overlap::Partial
        );
        for origin in &group.legacy {
            assert!(!origin.paid_reuse_verified());
        }
        c.execute("UPDATE photos SET hidden=1 WHERE id=?1", [ids[0]])
            .unwrap();
        let p = plan(&c);
        let group = p
            .groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.photo_id == 1))
            .unwrap();
        assert_eq!(group.overlap, Overlap::Disjoint);
    }
}
#[test]
fn legacy_cas_and_representative_absence_never_mutate_primary_or_old_tables() {
    let mut c = pair();
    legacy(&c, &[1, 2]);
    let tables = [
        "photos",
        "photo_ai_features",
        "photo_ai_groups",
        "photo_ai_group_members",
        "photo_embeddings",
        "photo_quality",
        "photo_quality_features",
    ];
    let before: Vec<_> = tables.iter().map(|t| rows(&c, t)).collect();
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    for (t, b) in tables.iter().zip(before) {
        assert!(rows(&c, t) == b);
    }
    let p = plan(&c);
    c.execute(
        "UPDATE photo_ai_group_members SET recommended=1 WHERE photo_id=2",
        [],
    )
    .unwrap();
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
    c.execute("UPDATE photos SET hidden=1 WHERE id=1", [])
        .unwrap();
    let p = plan(&c);
    assert!(!p.groups[0].has_shown_representative());
}
#[test]
fn explicit_migration_pending_no_readonly_schema_changes() {
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c).unwrap();
    assert_eq!(pending(&c).unwrap().len(), 3);
    assert!(snapshot(&c).unwrap().is_none());
    assert!(load_current(&c, "unknown").unwrap().groups().is_empty());
    assert_eq!(pending(&c).unwrap().len(), 3);
    assert_eq!(migrate(&mut c).unwrap().len(), 3);
    let changes = c.total_changes();
    assert!(migrate(&mut c).unwrap().is_empty());
    assert_eq!(changes, c.total_changes());
}
#[test]
fn model_absent_unverified_and_bad_file_are_fixed_reasons() {
    let dir = TempDir::new("scene-rank-missing");
    let path = dir.path().join("synthetic-only.json");
    assert_eq!(
        rank::Model::load(&path, &identity(2)).unwrap_err(),
        rank::Reason::Unreadable
    );
    let mut j = rank_json(&identity(2));
    j.as_object_mut().unwrap().remove("compatibility");
    assert_eq!(model(&j).unwrap_err(), rank::Reason::UnverifiedSpace);
    std::fs::write(&path, b"{SECRET-BODY").unwrap();
    assert_eq!(
        rank::Model::load(&path, &identity(2)).unwrap_err(),
        rank::Reason::Invalid
    );
}
#[test]
fn model_shape_feature_bounds_duplicate_and_depth_rejection() {
    for (field, value, reason) in [
        ("embedding_dim", json!(4097), rank::Reason::Shape),
        ("pca_mean", json!([0.]), rank::Reason::Shape),
        ("pca_components", json!([[0.]]), rank::Reason::Shape),
        (
            "base_features",
            json!(["SECRET-FEATURE"]),
            rank::Reason::Feature,
        ),
        ("base_weights", json!([]), rank::Reason::Shape),
    ] {
        let mut j = rank_json(&identity(2));
        j[field] = value;
        assert_eq!(model(&j).unwrap_err(), reason);
    }
    let mut j = rank_json(&identity(2));
    j["base_features"] = json!(["sharpness", "sharpness"]);
    j["base_weights"] = json!([1., 1.]);
    assert_eq!(model(&j).unwrap_err(), rank::Reason::Feature);
    assert_eq!(
        rank::Model::parse(b"[[[[[[[[[0]]]]]]]]]", &identity(2)).unwrap_err(),
        rank::Reason::Invalid
    );
    let mut j = rank_json(&identity(2));
    j["pca_components"] = json!(vec![vec![0., 0.]; 257]);
    j["pca_weights"] = json!(vec![0.; 257]);
    assert_eq!(model(&j).unwrap_err(), rank::Reason::Shape);
    j = rank_json(&identity(2));
    j["pca_mean"] = json!(vec![0.; 4097]);
    assert_eq!(model(&j).unwrap_err(), rank::Reason::Shape);
    assert_eq!(
        rank::Model::parse(b"{\"format\":1,\"format\":2}", &identity(2)).unwrap_err(),
        rank::Reason::Invalid
    );
}
#[test]
fn model_large_special_files_and_handle_bytes_digest() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("scene-rank-file");
    let path = dir.path().join("synthetic.json");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(rank::MAX_BYTES as u64 + 1).unwrap();
    assert_eq!(
        rank::Model::load(&path, &identity(2)).unwrap_err(),
        rank::Reason::TooLarge
    );
    assert_eq!(
        rank::Model::load(dir.path(), &identity(2)).unwrap_err(),
        rank::Reason::NotRegular
    );
    let fifo = dir.path().join("synthetic-fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        rank::Model::load(&fifo, &identity(2)).unwrap_err(),
        rank::Reason::NotRegular
    );
    symlink(&fifo, dir.path().join("link")).unwrap();
    assert_eq!(
        rank::Model::load(&dir.path().join("link"), &identity(2)).unwrap_err(),
        rank::Reason::NotRegular
    );
    let bytes = serde_json::to_vec(&rank_json(&identity(2))).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let m = rank::Model::load(&path, &identity(2)).unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert_eq!(m.source_digest(), crate::quality::sha256_hex(&bytes));
}
#[test]
fn rank_digest_reads_the_weight_file_again_only_when_it_changes() {
    let dir = crate::testutil::TempDir::new("rank-digest-cache");
    let path = dir.path().join("weights.json");
    let id = identity(2);
    // この試験だけの記憶（本番の共有の記憶は、ほかの試験のパスで満杯になりうる）。
    let cache = rank::DigestCache::new();
    let loads = || cache.loads();
    let write = |weight: f64| {
        let mut j = rank_json(&id);
        j["base_weights"] = json!([weight]);
        std::fs::write(&path, j.to_string()).unwrap();
    };
    write(-1.0);
    let first = cache.digest(&path, &id).unwrap();
    for _ in 0..5 {
        assert_eq!(cache.digest(&path, &id).as_deref(), Some(&*first));
    }
    assert_eq!(loads(), 1, "unchanged file is not parsed again");
    // 内容と大きさが変われば読み直す。
    write(-12.5);
    let second = cache.digest(&path, &id).unwrap();
    assert_ne!(first, second);
    assert_eq!(loads(), 2);
    // 不正な内容も、変わるまでは読み直さない（結果は None のまま）。
    std::fs::write(&path, b"invalid synthetic weight, longer").unwrap();
    for _ in 0..3 {
        assert_eq!(cache.digest(&path, &id), None);
    }
    assert_eq!(loads(), 3);
    // ファイルが無ければ読まずに None。
    std::fs::remove_file(&path).unwrap();
    assert_eq!(cache.digest(&path, &id), None);
    assert_eq!(loads(), 3);
}
#[test]
fn model_compatibility_checks_every_full_identity_field_and_legacy_never_guesses() {
    let base = identity(2);
    for s in [
        SpaceIdentity::new(
            "scene-b",
            "shared-a",
            "model-a",
            "rev-a",
            "img-a",
            "txt-a",
            2,
            Normalization::Raw,
        )
        .unwrap(),
        SpaceIdentity::new(
            "scene-a",
            "shared-b",
            "model-a",
            "rev-a",
            "img-a",
            "txt-a",
            2,
            Normalization::Raw,
        )
        .unwrap(),
        SpaceIdentity::new(
            "scene-a",
            "shared-a",
            "model-a",
            "rev-b",
            "img-a",
            "txt-a",
            2,
            Normalization::Raw,
        )
        .unwrap(),
        SpaceIdentity::new(
            "scene-a",
            "shared-a",
            "model-a",
            "rev-a",
            "img-b",
            "txt-a",
            2,
            Normalization::Raw,
        )
        .unwrap(),
        SpaceIdentity::new(
            "scene-a",
            "shared-a",
            "model-a",
            "rev-a",
            "img-a",
            "txt-b",
            2,
            Normalization::Raw,
        )
        .unwrap(),
        SpaceIdentity::new(
            "scene-a",
            "shared-a",
            "model-a",
            "rev-a",
            "img-a",
            "txt-a",
            2,
            Normalization::L2,
        )
        .unwrap(),
    ] {
        let bytes = serde_json::to_vec(&rank_json(&base)).unwrap();
        assert_eq!(
            rank::Model::parse(&bytes, &s).unwrap_err(),
            rank::Reason::IncompatibleSpace
        );
    }
    for field in [
        "space_fingerprint",
        "embedding_processor_version",
        "input_representation",
    ] {
        let mut j = rank_json(&base);
        j["compatibility"][field] = json!("SECRET-UNVERIFIED");
        assert_eq!(model(&j).unwrap_err(), rank::Reason::IncompatibleSpace);
    }
    let mut j = rank_json(&base);
    j["embedding_model"] = json!("old-q8-model");
    assert_eq!(model(&j).unwrap_err(), rank::Reason::IncompatibleSpace);
}
#[test]
fn pca_fold_scores_constant_fifty_overflow_missing_feature_and_fallback() {
    let c = pair();
    let snap = snapshot(&c).unwrap().unwrap();
    let mut j = rank_json(&identity(2));
    j["pca_mean"] = json!([1., 2.]);
    j["pca_components"] = json!([[1., 2.]]);
    j["pca_weights"] = json!([0.5]);
    let m = model(&j).unwrap();
    assert_eq!(
        m.scores(
            &identity(2),
            &[
                (snap.photos[0].metrics.unwrap(), &[1., 0.]),
                (snap.photos[1].metrics.unwrap(), &[1., 0.])
            ]
        )
        .unwrap(),
        [50., 50.]
    );
    j["pca_weights"] = json!([1e308]);
    assert_eq!(model(&j).unwrap_err(), rank::Reason::Overflow);
    j = rank_json(&identity(2));
    j["base_weights"] = json!([1e308]);
    let m = model(&j).unwrap();
    let p = compute(&snap, Options::default(), Ok(&m), &|| false).unwrap();
    assert_eq!(
        p.groups[0].rank_used,
        RankUsed::Fallback(rank::Reason::Overflow)
    );
    assert_eq!(
        p.groups[0].members[0].score,
        plan(&c).groups[0].members[0].score
    );
    c.execute(
        "UPDATE photo_quality_features SET luminance=NULL WHERE photo_id=1",
        [],
    )
    .unwrap();
    let snap = snapshot(&c).unwrap().unwrap();
    j = rank_json(&identity(2));
    j["base_features"] = json!(["luminance"]);
    let m = model(&j).unwrap();
    let p = compute(&snap, Options::default(), Ok(&m), &|| false).unwrap();
    assert_eq!(
        p.groups[0].rank_used,
        RankUsed::Fallback(rank::Reason::MissingFeature)
    );
    assert!(p.groups[0].weight_digest.is_some());
}
#[test]
fn nonfinite_json_scores_span_and_invalid_options_rejected() {
    for text in ["NaN", "Infinity", "1e999"] {
        let j = serde_json::to_string(&rank_json(&identity(2)))
            .unwrap()
            .replace("[-1.0]", &format!("[{text}]"));
        assert!(rank::Model::parse(j.as_bytes(), &identity(2)).is_err());
    }
    let mut j = rank_json(&identity(2));
    j["base_weights"] = json!([1e308]);
    let m = model(&j).unwrap();
    let metric = Metrics {
        sharpness: 1.,
        luminance: 128.,
        exposure_score: 90.,
        black_clip: 0.,
        white_clip: 0.,
        contrast: 100.,
    };
    let neg = Metrics {
        sharpness: -1.,
        ..metric
    };
    assert_eq!(
        m.scores(&identity(2), &[(metric, &[1., 0.]), (neg, &[1., 0.])])
            .unwrap_err(),
        rank::Reason::Overflow
    );
    assert!(Options::new(0, 0.8, 6).is_err());
    assert!(Options::new(900, f64::NAN, 6).is_err());
    assert!(Options::new(900, 0.8, 7).is_err());
}
#[test]
fn safe_diagnostics_and_weight_name_is_not_version() {
    let c = pair();
    let snap = snapshot(&c).unwrap().unwrap();
    let p = plan(&c);
    let mut raw = rank_json(&identity(2));
    let a = model(&raw).unwrap();
    raw["name"] = json!("ANOTHER-SECRET");
    let b = model(&raw).unwrap();
    assert_eq!(a.digest(), b.digest());
    assert_ne!(a.source_digest(), b.source_digest());
    let debug = format!(
        "{snap:?}{p:?}{:?}{:?}{a:?}{:?}",
        snap.photos[0],
        snap.photos[0].input,
        Error::Database
    );
    for secret in [
        "synthetic-1",
        "SECRET-NAME",
        "ANOTHER-SECRET",
        "model-a",
        "scene-a",
        "0000000000000000",
    ] {
        assert!(!debug.contains(secret));
    }
}
#[test]
fn interruption_and_no_active_space_are_safe() {
    let c = pair();
    let s = snapshot(&c).unwrap().unwrap();
    assert!(
        compute(
            &s,
            Options::default(),
            Err(rank::Reason::Unconfigured),
            &|| true
        )
        .is_none()
    );
    c.execute("DELETE FROM settings WHERE key='ai_active_space'", [])
        .unwrap();
    assert!(snapshot(&c).unwrap().is_none());
}

#[test]
fn synthetic_6500x768_compute_releases_writer_and_measures_save() {
    use crate::writer::{OpenMode, Writer};
    use std::{
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, Ordering},
        },
        time::Instant,
    };
    let dir = TempDir::new("scene-6500");
    let path = dir.path().join("catalog.db");
    let mut c = Connection::open(&path).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    schema::migrate(&mut c).unwrap();
    index::migrate(&mut c).unwrap();
    jobs::migrate(&mut c).unwrap();
    migrate(&mut c).unwrap();
    let s = identity(768);
    activate(&mut c, &s);
    let mut v = vec![0.0_f32; 768];
    v[0] = 1.;
    let blob = crate::ai::space::encode(&v);
    let tx = c.transaction().unwrap();
    for id in 1..=6500 {
        photo(&tx, id, 0, id);
        tx.execute(
            "INSERT INTO ai_embedding_vectors VALUES('scene-a',?1,?2,768,?3,0)",
            params![format!("{id:064x}"), "f".repeat(64), blob],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let rss = || {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .unwrap()
            .to_string()
    };
    let before = rss();
    let start = Instant::now();
    let snap = snapshot(&c).unwrap().unwrap();
    let snapshot_time = start.elapsed();
    let after_snapshot = rss();
    let gate = Arc::new(Barrier::new(2));
    let first = Arc::new(AtomicBool::new(true));
    let writer = Writer::open(&path, OpenMode::MustExist).unwrap();
    let (p, compute_time) = std::thread::scope(|scope| {
        let child = scope.spawn(|| {
            let start = Instant::now();
            let p = compute(
                &snap,
                Options::default(),
                Err(rank::Reason::Unconfigured),
                &|| {
                    if first.swap(false, Ordering::SeqCst) {
                        gate.wait();
                        gate.wait();
                    }
                    false
                },
            )
            .unwrap();
            (p, start.elapsed())
        });
        gate.wait();
        // The computation is held inside its cancellation callback, after read COMMIT.
        writer
            .call(|c| {
                c.execute(
                    "INSERT INTO settings VALUES('synthetic_writer_available','1')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        gate.wait();
        child.join().unwrap()
    });
    assert_eq!(p.groups.len(), 1);
    assert_eq!(p.groups[0].members.len(), 6500);
    let after_compute = rss();
    let start = Instant::now();
    let config = Arc::new(
        crate::config::Config::load(
            |k| match k {
                "GEOFRAME_INGEST" => Some("1".into()),
                "GEOFRAME_EMBED_BACKEND" => Some("local".into()),
                _ => None,
            },
            &crate::config::Overrides {
                data_dir: Some(dir.path().into()),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let runtime = runtime::Runtime::production(config).unwrap();
    let report = runtime
        .cycle_report(&c, &writer, &crate::stop::StopFlag::new())
        .unwrap();
    assert_eq!(
        (report.stored, report.selected, report.members),
        (1, 1, 6500)
    );
    let save_time = start.elapsed();
    let loaded = load_current(&c, p.groups[0].processor_version()).unwrap();
    assert_eq!(loaded.groups().len(), 1);
    assert_eq!(loaded.groups()[0].members().len(), 6500);
    // 同じ入力の 2 周期目は、何も選ばず何も保存しない（書き込みを再び占有しない）
    let again = runtime
        .cycle_report(&c, &writer, &crate::stop::StopFlag::new())
        .unwrap();
    assert_eq!((again.stored, again.selected, again.members), (0, 0, 0));
    eprintln!(
        "scene synthetic 6500x768 all-same-time: snapshot={snapshot_time:?}, compute={compute_time:?}, periodic-total={save_time:?}, report={report:?}; RSS before={before}, snapshot={after_snapshot}, compute={after_compute}, after-save/read={}",
        rss()
    );
}

#[test]
fn no_display_and_failed_with_missing_metrics_do_not_hold_other_members() {
    let mut c = pair();
    photo(&c, 3, 100, 3);
    fail(&mut c, 3);
    c.execute(
        "UPDATE photo_quality_features SET sharpness=NULL WHERE photo_id=3",
        [],
    )
    .unwrap();
    photo(&c, 4, 100, 4);
    c.execute("UPDATE photos SET display=NULL WHERE id=4", [])
        .unwrap();
    let snap = snapshot(&c).unwrap().unwrap();
    assert_eq!(snap.photos[2].readiness, Readiness::FailedEmbedding);
    assert_eq!(snap.photos[3].readiness, Readiness::NoDisplay);
    assert_eq!(plan(&c).groups[0].state, State::LocalRanked);
}

#[test]
fn read_snapshot_pointer_and_rows_are_consistent_across_concurrent_space_change() {
    use std::sync::Mutex;
    static CHANGE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
    fn trace(event: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event
            && sql.starts_with("SELECT result_id,input_generation")
            && let Some(path) = CHANGE.lock().unwrap().take()
        {
            let c = Connection::open(path).unwrap();
            c.execute("DELETE FROM settings WHERE key='ai_active_space'", [])
                .unwrap();
        }
    }
    let dir = TempDir::new("scene-snapshot-race");
    let path = dir.path().join("synthetic.db");
    let mut c = Connection::open(&path).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    schema::migrate(&mut c).unwrap();
    index::migrate(&mut c).unwrap();
    migrate(&mut c).unwrap();
    activate(&mut c, &identity(2));
    ready(&mut c, 1, 0);
    ready(&mut c, 2, 100);
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    drop(c); // Fresh process/connection can resume from persisted result/member/pointers.
    let c = Connection::open(&path).unwrap();
    assert_eq!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
    *CHANGE.lock().unwrap() = Some(path);
    c.trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
        Some(trace),
    );
    let read = load_current(&c, p.groups[0].processor_version()).unwrap();
    c.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
    assert_eq!(read.groups().len(), 1);
    assert!(CHANGE.lock().unwrap().is_none());
    assert!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .is_empty()
    );
}

#[test]
fn concurrent_processor_plans_cannot_replace_newer_active_references() {
    let mut c = pair();
    let snap = snapshot(&c).unwrap().unwrap();
    let old = plan(&c);
    let new = compute(
        &snap,
        Options::new(900, 0.9, 6).unwrap(),
        Err(rank::Reason::Unconfigured),
        &|| false,
    )
    .unwrap();
    assert_eq!(save(&mut c, &new, 0, &|| false).unwrap(), Saved::Stored);
    assert_eq!(save(&mut c, &old, 0, &|| false).unwrap(), Saved::Stale);
    assert_eq!(save(&mut c, &new, 0, &|| false).unwrap(), Saved::Unchanged);
    assert_eq!(
        load_current(&c, new.groups[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
}

#[test]
fn vector_and_failed_retry_changes_after_plan_refuse_atomic_publication() {
    let mut c = pair();
    let p = plan(&c);
    c.execute(
        "UPDATE ai_embedding_vectors SET vector=zeroblob(8) WHERE input_sha256=?1",
        [format!("{:064x}", 1)],
    )
    .unwrap();
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
    assert!(rows(&c, "ai_scene_members").is_empty());
    let mut c = pair();
    photo(&c, 3, 100, 3);
    fail(&mut c, 3);
    let p = plan(&c);
    c.execute("UPDATE ai_jobs SET state='pending',terminal=0", [])
        .unwrap();
    assert_eq!(save(&mut c, &p, 0, &|| false).unwrap(), Saved::Stale);
    assert!(rows(&c, "ai_scene_groups").is_empty());
}

// T4.5b production-local runtime fixtures. Only synthetic SQLite, rank JSON and vectors.
struct LocalEnv {
    _dir: TempDir,
    config: Arc<crate::config::Config>,
    ingest: crate::ingest::Ingest,
    conn: Connection,
}
impl LocalEnv {
    fn new(vars: &[(&str, &str)]) -> Self {
        let dir = TempDir::new("scene-runtime");
        let mut env = std::collections::HashMap::from([
            ("GEOFRAME_INGEST".to_string(), "1".to_string()),
            ("GEOFRAME_EMBED_BACKEND".to_string(), "local".to_string()),
        ]);
        for (k, v) in vars {
            env.insert(k.to_string(), v.to_string());
        }
        let config = Arc::new(
            crate::config::Config::load(
                |k| env.get(k).cloned(),
                &crate::config::Overrides {
                    data_dir: Some(dir.path().join("data")),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let ingest = crate::ingest::start_with(&config, &crate::guard::Guard::skip()).unwrap();
        let mut conn = Connection::open(config.db_path()).unwrap();
        activate(&mut conn, &identity(2));
        Self {
            _dir: dir,
            config,
            ingest,
            conn,
        }
    }
    fn cycle(&self) -> usize {
        runtime::Runtime::production(self.config.clone())
            .unwrap()
            .cycle(
                &self.conn,
                &self.ingest.writer,
                &crate::stop::StopFlag::new(),
            )
            .unwrap()
    }
    fn view(&self) -> Json {
        crate::ai::search_view::search(
            &self.conn,
            &self.config,
            "",
            30,
            false,
            None,
            &crate::ai::http::Deadline::interactive(),
        )
        .unwrap()
        .unwrap()
    }
    fn status(&self) -> Json {
        crate::ai::search_view::status(&self.conn, &self.config).unwrap()
    }
    fn feed(&self) -> Vec<i64> {
        crate::photos::list_photos(&self.conn)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect()
    }
    fn loaded(&self) -> Loaded {
        let prepared = runtime::Prepared::read(&self.conn, &self.config).unwrap();
        let tx = self.conn.unchecked_transaction().unwrap();
        let result = prepared.load_in(&tx).unwrap();
        tx.commit().unwrap();
        result
    }
}

#[test]
fn local_factory_migrates_cycles_projects_and_restart_never_resaves_identical_results() {
    let mut e = LocalEnv::new(&[]);
    assert!(pending(&e.conn).unwrap().is_empty());
    assert_eq!(
        e.ingest
            .migration
            .created
            .iter()
            .filter(|s| s.starts_with("ai_scene_"))
            .count(),
        3
    );
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    let before = rows(&e.conn, "ai_scene_groups");
    assert_eq!(e.cycle(), 0);
    assert_eq!(
        runtime::Runtime::production(e.config.clone())
            .unwrap()
            .cycle(&e.conn, &e.ingest.writer, &crate::stop::StopFlag::new())
            .unwrap(),
        0
    );
    assert_eq!(rows(&e.conn, "ai_scene_groups"), before);
    let status = e.status();
    assert_eq!(status["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(status["judged"], 0);
    let view = e.view();
    assert_eq!(view["mode"], "recommended");
    assert_eq!(view["groups"].as_array().unwrap().len(), 1);
    assert_eq!(view["groups"][0]["kind"], "scene");
    assert_eq!(view["groups"][0]["photos"].as_array().unwrap().len(), 2);
    assert_eq!(view["groups"][0]["photos"][0]["local_representative"], true);
    assert!(view["groups"][0]["photos"][0]["recommended"].is_null());
    assert_eq!(e.feed().len(), 2); // LocalRanked grants no suppression.
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let admin = crate::admin::status(&e.conn, &ctx).unwrap();
    assert_eq!(admin["ai_scenes"]["local_ranked"], 1);
}

#[test]
fn local_periodic_pending_running_deferred_failed_and_retry_have_one_view() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    photo(&e.conn, 2, 100, 2);
    for state in ["pending", "running", "deferred"] {
        let key = index::scan(&e.conn, Some(&identity(2)), false)
            .unwrap()
            .into_iter()
            .find(|s| s.input_sha256() == format!("{:064x}", 2))
            .unwrap()
            .job_key(&identity(2));
        jobs::enqueue(&mut e.conn, &key, 0).unwrap();
        e.conn
            .execute("UPDATE ai_jobs SET state=?1", [state])
            .unwrap();
        e.cycle();
        assert_eq!(e.loaded().groups()[0].state(), State::Waiting);
        assert_eq!(e.status()["pipeline"]["scenes"]["waiting"], 1);
        assert!(
            e.view()["groups"][0]["photos"][0]
                .get("local_rank")
                .is_none()
        );
        assert_eq!(e.feed().len(), 2);
    }
    fail(&mut e.conn, 2);
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.loaded().groups()[0].state(), State::LocalRanked);
    vector(&mut e.conn, 2, &[1., 0.]);
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.loaded().groups()[0].members().len(), 2);
    assert_eq!(e.feed().len(), 2);
}

#[test]
fn local_partial_exact_disjoint_and_favorite_keep_paid_selection_unchanged() {
    let mut e = LocalEnv::new(&[]);
    for id in 1..=3 {
        ready(&mut e.conn, id, id * 10);
    }
    legacy(&e.conn, &[1, 2]);
    let old = rows(&e.conn, "photo_ai_group_members");
    let before = e.feed();
    assert_eq!(before, vec![1, 3]);
    e.cycle();
    assert_eq!(e.loaded().groups()[0].state(), State::ReviewOverlap);
    assert_eq!(e.feed(), before);
    e.conn
        .execute("UPDATE photos SET favorite=1 WHERE id=2", [])
        .unwrap();
    e.cycle();
    assert_eq!(e.feed(), vec![1, 2, 3]);
    // 管理画面の `suppressed` は従来どおり favorite を含め、フィードで実際に隠す数は 0
    let counts = crate::selection::admin_counts(&e.conn).unwrap();
    assert_eq!((counts.suppressed, counts.feed_suppressed), (1, 0));
    e.conn
        .execute(
            "UPDATE photos SET taken_at='2026-01-01 01:00:00' WHERE id=3",
            [],
        )
        .unwrap();
    e.cycle();
    let loaded = e.loaded();
    assert!(
        loaded
            .groups()
            .iter()
            .any(|g| g.overlap() == Overlap::Exact)
    );
    assert!(
        loaded
            .groups()
            .iter()
            .any(|g| g.overlap() == Overlap::Disjoint)
    );
    assert_eq!(rows(&e.conn, "photo_ai_group_members"), old);
    assert_eq!(e.feed(), vec![1, 2, 3]);
}

#[test]
fn local_cycle_rep_invalidation_and_space_switch_preserve_old_selection_boundary() {
    for change in [
        "UPDATE photos SET hidden=1 WHERE id=1",
        "UPDATE photos SET lat=NULL WHERE id=1",
        "UPDATE photos SET mtime=2 WHERE id=1",
        "DELETE FROM photos WHERE id=1",
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,group_kind,rank_in_group,recommended,score,algorithm,analyzed_at) VALUES(1,'synthetic-1','m',1,'synthetic',1,'exact',1,0,50,'synthetic','synthetic')",
    ] {
        let mut e = LocalEnv::new(&[]);
        ready(&mut e.conn, 1, 0);
        ready(&mut e.conn, 2, 100);
        legacy(&e.conn, &[1, 2]);
        e.cycle();
        let old = e.feed();
        assert_eq!(old, vec![1]);
        let other = SpaceIdentity::new(
            "scene-b",
            "shared-b",
            "model-b",
            "rev-b",
            "img-b",
            "txt-b",
            2,
            Normalization::Raw,
        )
        .unwrap();
        activate(&mut e.conn, &other);
        assert_eq!(e.feed(), old);
        assert!(e.loaded().groups().is_empty());
        e.conn.execute_batch(change).unwrap();
        assert!(e.feed().contains(&2));
    }
}

#[test]
fn local_batch_cas_checks_each_group_input_pointer_and_other_space_and_rolls_back_stop() {
    let mut c = pair();
    ready(&mut c, 3, 7200);
    let p = plan(&c);
    c.execute("UPDATE photos SET favorite=1 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        save_batch(&mut c, &p, &[0, 1], &|| false).unwrap(),
        vec![Saved::Stale, Saved::Stored]
    );
    let fresh = plan(&c);
    let n = std::cell::Cell::new(0);
    let before = rows(&c, "ai_scene_groups");
    assert!(
        save_batch(&mut c, &fresh, &[0, 1], &|| {
            n.set(n.get() + 1);
            n.get() > 3
        })
        .unwrap()
        .iter()
        .all(|s| *s == Saved::Interrupted)
    );
    assert_eq!(rows(&c, "ai_scene_groups"), before);
    assert_eq!(
        save_batch(&mut c, &fresh, &[0; 17], &|| false),
        Err(Error::InvalidOptions)
    );
}

#[test]
fn local_repair_skip_check_does_not_trust_ids_or_broken_members() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    e.cycle();
    for corrupt in [
        "UPDATE ai_scene_groups SET lo=hi+1",
        "UPDATE ai_scene_members SET score=999 WHERE rank=1",
        "UPDATE ai_scene_groups SET shortlist='broken'",
        "DELETE FROM ai_scene_members WHERE rank=2",
        "INSERT INTO ai_scene_active VALUES(999,(SELECT result_id FROM ai_scene_groups LIMIT 1))",
    ] {
        e.conn.execute_batch(corrupt).unwrap();
        let view = e.view();
        assert_eq!(view["mode"], "recommended"); // Corruption never stops serving.
        assert!(view["groups"][0]["photos"][0].get("local_rank").is_none());
        assert_eq!(e.cycle(), 1);
        assert_eq!(e.loaded().groups().len(), 1);
        assert_eq!(e.cycle(), 0);
    }
}

#[test]
fn local_model_configuration_changes_invalidate_projection_before_recompute() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    e.conn
        .execute(
            "UPDATE photo_quality_features SET sharpness=20 WHERE photo_id=2",
            [],
        )
        .unwrap();
    e.cycle();
    let original = e.loaded().groups()[0].processor_version().to_string();
    std::fs::create_dir_all(e.config.data_dir.join("models")).unwrap();
    let model = rank_json(&identity(2));
    std::fs::write(&e.config.rank_model_path, model.to_string()).unwrap();
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.cycle(), 1);
    assert_ne!(e.loaded().groups()[0].processor_version(), original);
    assert_eq!(e.loaded().groups()[0].members()[0].photo_id, 1);
    std::fs::write(&e.config.rank_model_path, b"invalid synthetic weight").unwrap();
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.loaded().groups()[0].processor_version(), original);
    assert!(matches!(
        e.loaded().groups()[0].rank_used(),
        RankUsed::Rules(Some(rank::Reason::Invalid))
    ));
    let changed = Arc::new(
        crate::config::Config::load(
            |k| match k {
                "GEOFRAME_INGEST" => Some("1".into()),
                "GEOFRAME_AI_TIME_WINDOW_MIN" => Some("1".into()),
                "GEOFRAME_EMBED_BACKEND" => Some("local".into()),
                _ => None,
            },
            &crate::config::Overrides {
                data_dir: Some(e.config.data_dir.clone()),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    e.config = changed;
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.cycle(), 2);
    assert_eq!(e.loaded().groups().len(), 2);
}

#[test]
fn local_many_small_components_are_bounded_and_progress_without_rescanning_on_writer_per_group() {
    let mut e = LocalEnv::new(&[]);
    for id in 1..=40 {
        ready(&mut e.conn, id, id * 1800);
    }
    assert_eq!(e.cycle(), runtime::GROUPS_PER_CYCLE);
    assert_eq!(e.loaded().groups().len(), 16);
    assert_eq!(e.cycle(), 16);
    assert_eq!(e.cycle(), 8);
    assert_eq!(e.cycle(), 0);
    assert_eq!(e.loaded().groups().len(), 40);
    let p = compute(
        &snapshot(&e.conn).unwrap().unwrap(),
        e.config.ai.scene_options(),
        Err(rank::Reason::Unreadable),
        &|| false,
    )
    .unwrap();
    assert!(pending_saves(&e.conn, &p, 16).unwrap().is_empty());
}

#[test]
fn local_disabled_invalid_and_old_db_read_paths_create_no_scene_tables() {
    for vars in [
        vec![("GEOFRAME_AI_ENABLED", "0")],
        vec![("GEOFRAME_AI_SIMILARITY", "NaN")],
        vec![("GEOFRAME_AI_TIME_WINDOW_MIN", "private-invalid")],
    ] {
        let e = LocalEnv::new(&vars);
        assert!(runtime::Runtime::production(e.config.clone()).is_none());
        assert_eq!(pending(&e.conn).unwrap().len(), 3);
        assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 0);
        assert_eq!(pending(&e.conn).unwrap().len(), 3);
        assert_eq!(rows(&e.conn, "photo_ai_groups").len(), 0);
    }
    let dir = TempDir::new("scene-readonly");
    let config = crate::config::Config::load(
        |_| None,
        &crate::config::Overrides {
            data_dir: Some(dir.path().into()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut c = Connection::open_in_memory().unwrap();
    schema::migrate(&mut c).unwrap();
    let before = rows(&c, "sqlite_master");
    assert!(!runtime::enabled(&config));
    let view = crate::ai::search_view::status(&c, &config).unwrap();
    assert_eq!(view["photos"], 0);
    assert_eq!(rows(&c, "sqlite_master"), before);
}

#[test]
fn local_writer_wait_is_bounded_and_expired_or_stopped_work_never_saves_later() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    let (entered, rx) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let writer = e.ingest.writer.clone();
    let held = std::thread::spawn(move || {
        writer
            .call(move |_| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    assert_eq!(
        runtime::Runtime::production(e.config.clone())
            .unwrap()
            .cycle_report_until(
                &e.conn,
                &e.ingest.writer,
                &crate::stop::StopFlag::new(),
                Instant::now() + Duration::from_millis(100)
            )
            .map(|r| r.stored),
        // 書き込みの順番待ちの期限切れは、DB の失敗と別のコード（混雑）
        Err(Error::WriterBusy)
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    held.join().unwrap();
    e.ingest.writer.call(|_| Ok(())).unwrap();
    assert!(rows(&e.conn, "ai_scene_groups").is_empty());
    let stop = crate::stop::StopFlag::new();
    stop.stop();
    assert_eq!(
        runtime::Runtime::production(e.config.clone())
            .unwrap()
            .cycle(&e.conn, &e.ingest.writer, &stop)
            .unwrap(),
        0
    );
    assert!(rows(&e.conn, "ai_scene_groups").is_empty());
}

#[test]
fn portable_fixture_starts_without_scene_tables_migrates_and_runs_validated_weight() {
    // 場面の表が無い合成の DB と重み（Python の生成ツールが作ったものを SQL と JSON に固めた基準）
    let dir = TempDir::new("scene-portable");
    let destination = dir.path().join("fixture");
    std::fs::create_dir_all(destination.join("models")).unwrap();
    Connection::open(destination.join("catalog.db"))
        .unwrap()
        .execute_batch(include_str!(
            "../../../tests/fixtures/scene-runtime-portable.sql"
        ))
        .unwrap();
    std::fs::write(
        destination.join("models/rank_model.json"),
        include_str!("../../../tests/fixtures/scene-runtime-portable-rank-model.json"),
    )
    .unwrap();
    let config = Arc::new(
        crate::config::Config::load(
            |k| match k {
                "GEOFRAME_INGEST" => Some("1".into()),
                "GEOFRAME_EMBED_BACKEND" => Some("local".into()),
                _ => None,
            },
            &crate::config::Overrides {
                data_dir: Some(destination),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let conn = crate::db::open_read_only(&config.db_path()).unwrap();
    assert_eq!(pending(&conn).unwrap().len(), 3);
    assert_eq!(
        crate::ingest::plan(&config, &conn)
            .unwrap()
            .schema
            .missing_objects
            .len(),
        10
    );
    let old = rows(&conn, "photos");
    let ingest = crate::ingest::start_with(&config, &crate::guard::Guard::skip()).unwrap();
    assert_eq!(ingest.migration.created.len(), 10);
    assert_eq!(
        runtime::Runtime::production(config.clone())
            .unwrap()
            .cycle(&conn, &ingest.writer, &crate::stop::StopFlag::new())
            .unwrap(),
        1
    );
    let status = crate::ai::search_view::status(&conn, &config).unwrap();
    assert_eq!(status["pipeline"]["scenes"]["linear"], 1);
    assert_eq!(rows(&conn, "photos"), old);
}

#[test]
fn local_batch_old_expected_active_cannot_replace_new_processor_even_with_same_members() {
    let mut c = pair();
    let old = plan(&c);
    let new = compute(
        &snapshot(&c).unwrap().unwrap(),
        Options::new(900, 0.8, 6).unwrap(),
        Err(rank::Reason::Unconfigured),
        &|| false,
    )
    .unwrap();
    assert_eq!(
        save_batch(&mut c, &new, &[0], &|| false).unwrap(),
        vec![Saved::Stored]
    );
    assert_eq!(
        save_batch(&mut c, &old, &[0], &|| false).unwrap(),
        vec![Saved::Stale]
    );
    assert_eq!(
        load_current(&c, new.groups()[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
}

#[test]
fn local_verified_weight_component_fallback_is_usable_but_a_changed_weight_must_be_replanned() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    e.conn
        .execute("UPDATE photo_quality_features SET luminance=NULL", [])
        .unwrap();
    std::fs::create_dir_all(e.config.data_dir.join("models")).unwrap();
    let mut model = rank_json(&identity(2));
    model["base_features"] = json!(["luminance"]);
    std::fs::write(&e.config.rank_model_path, model.to_string()).unwrap();
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.loaded().groups().len(), 1);
    assert_eq!(
        e.loaded().groups()[0].rank_used(),
        RankUsed::Fallback(rank::Reason::MissingFeature)
    );
    assert_eq!(
        e.view()["groups"][0]["photos"][0]["local_representative"],
        true
    );
    model["base_features"] = json!(["sharpness"]);
    std::fs::write(&e.config.rank_model_path, model.to_string()).unwrap();
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.loaded().groups()[0].rank_used(), RankUsed::Linear);
}

// ---- T4.5b review fixes: read paths, ordering and inheritance -------------------------------

std::thread_local! {
    static SQL_LOG: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}
fn log_sql(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event {
        SQL_LOG.with(|log| log.borrow_mut().push(sql.to_string()));
    }
}
std::thread_local! {
    static PROFILE: std::cell::RefCell<Vec<(String, std::time::Duration)>> = const { std::cell::RefCell::new(Vec::new()) };
}
fn log_profile(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Profile(stmt, d) = event {
        PROFILE.with(|p| p.borrow_mut().push((stmt.sql().to_string(), d)));
    }
}
/// Statements executed on `c` (this thread) while `f` runs.
fn traced<T>(c: &Connection, f: impl FnOnce() -> T) -> (T, Vec<String>) {
    SQL_LOG.with(|log| log.borrow_mut().clear());
    c.trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
        Some(log_sql),
    );
    let out = f();
    c.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
    (
        out,
        SQL_LOG.with(|log| std::mem::take(&mut *log.borrow_mut())),
    )
}
/// Full scans of every photo row for the scene guard.
fn full_scans(sql: &[String]) -> usize {
    sql.iter()
        .filter(|s| s.contains("scene-raw") && !s.contains("WHERE p.id IN"))
        .count()
}
/// Loads of the vector BLOB column by the scene layer.
fn vector_loads(sql: &[String]) -> usize {
    sql.iter().filter(|s| s.contains("scene-vectors")).count()
}
fn keep(c: &Connection, id: i64, score: i64) {
    c.execute(
        "INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error,keep_score)
         SELECT id,display,md5,mtime,?2,'old-model',2,zeroblob(8),'synthetic','',?3 FROM photos WHERE id=?1
         ON CONFLICT(photo_id) DO UPDATE SET keep_score=excluded.keep_score",
        params![id, format!("{id:064x}"), score],
    )
    .unwrap();
}
fn sharp(c: &Connection, id: i64, value: f64) {
    c.execute(
        "UPDATE photo_quality_features SET sharpness=?2 WHERE photo_id=?1",
        params![id, value],
    )
    .unwrap();
}
fn ids(view: &Json) -> Vec<Vec<i64>> {
    view["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            g["photos"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["id"].as_i64().unwrap())
                .collect()
        })
        .collect()
}
fn view_n(e: &LocalEnv, groups: usize) -> Json {
    crate::ai::search_view::search(
        &e.conn,
        &e.config,
        "",
        groups,
        false,
        None,
        &crate::ai::http::Deadline::interactive(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn read_apis_load_no_vector_blobs_and_never_recompute_the_snapshot_even_with_a_weight_file() {
    let mut e = LocalEnv::new(&[]);
    for (id, at) in [(1, 0), (2, 100), (3, 5000), (4, 5100)] {
        ready(&mut e.conn, id, at);
    }
    // 重みはあるが、写真 1・2 の成分だけ特徴量不足で固定式 fallback になる
    e.conn
        .execute(
            "UPDATE photo_quality_features SET luminance=NULL WHERE photo_id IN (1,2)",
            [],
        )
        .unwrap();
    std::fs::create_dir_all(e.config.data_dir.join("models")).unwrap();
    let mut model = rank_json(&identity(2));
    model["base_features"] = json!(["luminance"]);
    std::fs::write(&e.config.rank_model_path, model.to_string()).unwrap();
    // 周期は vector を読む（対照: 計測の印が付いている）
    let (stored, cycle_sql) = traced(&e.conn, || e.cycle());
    assert_eq!(stored, 2);
    assert!(vector_loads(&cycle_sql) >= 1);
    let kinds: Vec<_> = e.loaded().groups().iter().map(|g| g.rank_used()).collect();
    assert!(kinds.contains(&RankUsed::Linear));
    assert!(kinds.contains(&RankUsed::Fallback(rank::Reason::MissingFeature)));

    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let (view, e6) = traced(&e.conn, || e.view());
    assert_eq!(view["groups"].as_array().unwrap().len(), 2);
    let (status, e5) = traced(&e.conn, || e.status());
    assert_eq!(status["pipeline"]["scenes"]["linear"], 1);
    assert_eq!(status["pipeline"]["scenes"]["rules"], 1);
    let (admin, status_sql) = traced(&e.conn, || crate::admin::status(&e.conn, &ctx).unwrap());
    assert_eq!(admin["ai_scenes"]["local_ranked"], 2);
    let (page, photos_sql) = traced(&e.conn, || {
        crate::admin::photos(&e.conn, &ctx, 1, 2, crate::admin::Filter::All).unwrap()
    });
    let page = serde_json::to_value(&page).unwrap();
    assert_eq!(page["items"][0]["scene"]["state"], "local_ranked");
    assert!(page["items"][2].is_null()); // 2 件の頁

    // どの読み取りも、場面のために vector BLOB を読まない
    for (name, sql) in [
        ("E6", &e6),
        ("E5", &e5),
        ("admin status", &status_sql),
        ("admin photos", &photos_sql),
    ] {
        assert_eq!(vector_loads(sql), 0, "{name}");
    }
    // 全写真の行の走査は、E6 の検証付き読み取りの 1 回だけ（snapshot の再計算も、fallback の再検証も無い）。
    // 件数だけの STATUS・E5 と、頁の注記だけの管理一覧は走査しない
    assert_eq!(full_scans(&e6), 1);
    assert_eq!(full_scans(&e5), 0);
    assert_eq!(full_scans(&status_sql), 0);
    assert_eq!(full_scans(&photos_sql), 0);

    // 検証付きの読み取り単体でも、準備（rankfile）と本体で BLOB を読まない
    let (_, direct) = traced(&e.conn, || {
        assert_eq!(e.loaded().groups().len(), 2);
    });
    assert_eq!((vector_loads(&direct), full_scans(&direct)), (0, 1));

    // 同じ重みの別の係数では、古い fallback も Linear も採用しない（周期の再計画まで）
    model["base_weights"] = json!([-2.0]);
    std::fs::write(&e.config.rank_model_path, model.to_string()).unwrap();
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 0);
    assert_eq!(e.cycle(), 2);
    assert_eq!(e.loaded().groups().len(), 2);
}

#[test]
fn partial_overlap_is_information_only_and_keeps_the_old_keep_score_order() {
    let mut e = LocalEnv::new(&[]);
    for id in 1..=3 {
        ready(&mut e.conn, id, id * 10);
    }
    legacy(&e.conn, &[1, 2]);
    // 旧来の keep_score では 2 → 3 → 1。場面の順位（鮮明度）では 1 → 3 → 2
    for (id, keep_score, sharpness) in [(1, 30, 90.0), (2, 90, 10.0), (3, 60, 50.0)] {
        keep(&e.conn, id, keep_score);
        sharp(&e.conn, id, sharpness);
    }
    e.cycle();
    assert_eq!(e.loaded().groups()[0].state(), State::ReviewOverlap);
    let view = e.view();
    assert_eq!(ids(&view), vec![vec![2, 1], vec![3]]);
    let first = &view["groups"][0];
    assert_eq!(first["group_id"], "old");
    assert_eq!(first["photos"][0]["scene_state"], "review_overlap");
    assert_eq!(first["photos"][0]["scene_overlap"], "partial");
    // 情報としては付く（順位・代表）が、並びには使われない
    assert_eq!(first["photos"][1]["local_rank"], 1);
    assert_eq!(first["photos"][1]["local_representative"], true);
}

#[test]
fn an_exact_scene_inherits_the_old_group_key_and_its_paid_fields() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    legacy(&e.conn, &[1, 2]);
    e.conn
        .execute(
            "UPDATE photo_ai_groups SET summary='kept summary',min_similarity=0.8,max_similarity=0.9,avg_similarity=0.85",
            [],
        )
        .unwrap();
    sharp(&e.conn, 1, 10.0);
    sharp(&e.conn, 2, 90.0);
    e.cycle();
    assert_eq!(e.loaded().groups()[0].overlap(), Overlap::Exact);
    let view = e.view();
    let group = &view["groups"][0];
    assert_eq!(view["groups"].as_array().unwrap().len(), 1);
    assert_eq!(group["group_id"], "old");
    assert_eq!(group["kind"], "scene");
    assert_eq!(group["summary"], "kept summary");
    assert_eq!(group["min_similarity"], 0.8);
    assert_eq!(group["max_similarity"], 0.9);
    assert_eq!(group["avg_similarity"], 0.85);
    assert_eq!(group["scene_state"], "local_ranked");
    assert_eq!(group["scene_overlap"], "exact");
    assert!(group["scene_result_id"].is_string());
    // 推薦一覧では場面の順位（鮮明度）が並びを決める（正確な重複でも同じ）
    assert_eq!(ids(&view), vec![vec![2, 1]]);
}

#[test]
fn group_placement_uses_the_old_score_scale_so_scenes_do_not_always_win() {
    let mut e = LocalEnv::new(&[]);
    // 場面 S（1・2）は旧来の点が低い。単独の 3 と旧判定の群（4・5。6 を含む場面とは部分重複）は高い
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    ready(&mut e.conn, 3, 20_000);
    ready(&mut e.conn, 4, 40_000);
    ready(&mut e.conn, 5, 40_050);
    ready(&mut e.conn, 6, 40_100);
    legacy(&e.conn, &[4, 5]);
    for (id, score) in [(1, 10), (2, 20), (3, 80), (4, 70), (5, 60), (6, 5)] {
        keep(&e.conn, id, score);
    }
    e.cycle();
    let all = view_n(&e, 30);
    let order = ids(&all);
    // 上位は旧来の点の順: 単独 3（80）→ 旧判定の群（70）→ 場面 S（20）
    assert_eq!(order[0], vec![3]);
    assert_eq!(order[1], vec![4, 5]);
    assert_eq!(order[2], vec![1, 2]); // 場面の中は場面の順位（同点は ID 順）
    // 打ち切っても、点の高い単独写真と旧判定の群が場面に押し出されない
    assert_eq!(ids(&view_n(&e, 1)), vec![vec![3]]);
    assert_eq!(ids(&view_n(&e, 2)), vec![vec![3], vec![4, 5]]);
    // 場面の点（群の中で 0〜100 に正規化）は並びにも similarity にも使わない
    assert_eq!(all["groups"][0]["photos"][0]["similarity"], 0.8);
}

#[test]
fn admin_scene_annotations_follow_the_photo_generation_and_the_current_space() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    e.cycle();
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let page = |e: &LocalEnv| {
        serde_json::to_value(
            crate::admin::photos(&e.conn, &ctx, 1, 10, crate::admin::Filter::All).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(page(&e)["items"][0]["scene"]["rank"], 1);
    // 写真の世代が変わったら、その写真の古い注記は出さない（もう片方は残る）
    e.conn
        .execute("UPDATE photos SET mtime=2 WHERE id=1", [])
        .unwrap();
    let after = page(&e);
    let by_id = |id: i64| {
        after["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    assert!(by_id(1).get("scene").is_none());
    assert_eq!(by_id(2)["scene"]["state"], "local_ranked");
    // The note is advisory (the component guard is not recomputed), but it only names a photo that can
    // still belong to a component. The cycle then drops the changed photo's reference as well.
    e.cycle();
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=1"
        ),
        0
    );
    assert!(
        page(&e)["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != 1 || p.get("scene").is_none())
    );
}

// ---- 2nd review: vector presence in the input guard, current-only counts, schema shape ----------

fn scene_count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn scene_marks(view: &Json) -> usize {
    view["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["photos"].as_array().unwrap().iter())
        .filter(|c| c["scene_state"].is_string())
        .count()
}
fn sha_hex(id: i64) -> String {
    format!("{id:064x}")
}
fn states(e: &LocalEnv) -> Vec<State> {
    e.loaded().groups().iter().map(|g| g.state()).collect()
}

#[test]
fn a_deleted_vector_stops_the_stored_scene_and_no_cycle_brings_it_back() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    assert_eq!(states(&e), vec![State::LocalRanked]);
    assert_eq!(scene_marks(&e.view()), 2);
    e.conn
        .execute(
            "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
            [sha_hex(2)],
        )
        .unwrap();
    // Before any cycle: the existence of the vector is part of the input guard, so the old R{1,2}
    // is no longer current for the reads (E6, status of the projection).
    assert!(e.loaded().groups().is_empty());
    assert_eq!(scene_marks(&e.view()), 0);
    // The cycle states "waiting" for the photo that still has a vector, and removes its old reference.
    assert_eq!(e.cycle(), 1);
    assert_eq!(states(&e), vec![State::Waiting]);
    assert_eq!(e.loaded().groups()[0].members().len(), 1);
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=1"
        ),
        0
    );
    for _ in 0..3 {
        assert_eq!(e.cycle(), 0);
        assert_eq!(states(&e), vec![State::Waiting]);
        assert_eq!(scene_marks(&e.view()), 0);
    }
    let scenes = e.status()["pipeline"]["scenes"].clone();
    assert_eq!(
        (&scenes["local_ranked"], &scenes["waiting"]),
        (&json!(0), &json!(1))
    );
    // The vector comes back: the same ranked result is current again and the placeholder is gone.
    vector(&mut e.conn, 2, &[1., 0.]);
    assert_eq!(e.cycle(), 1);
    assert_eq!(states(&e), vec![State::LocalRanked]);
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_groups WHERE state='waiting'"
        ),
        0
    );
    assert_eq!(scene_marks(&e.view()), 2);
}

#[test]
fn a_damaged_vector_is_dropped_by_the_search_check_and_the_cycle_waits() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    // Same length, non-finite values: the row still exists, so the cheap stored guard cannot see it.
    e.conn
        .execute(
            "UPDATE ai_embedding_vectors SET vector=?2 WHERE input_sha256=?1",
            params![sha_hex(2), crate::ai::space::encode(&[f32::NAN, 0.])],
        )
        .unwrap();
    // The reads do not attach the old result: E6 requires every member to pass its own vector check.
    assert_eq!(scene_marks(&e.view()), 0);
    // The cycle's snapshot sees the damaged vector and states waiting; the old reference is removed.
    assert_eq!(e.cycle(), 1);
    assert!(states(&e).iter().all(|s| *s == State::Waiting));
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=1"
        ),
        0
    );
    for _ in 0..3 {
        assert_eq!(e.cycle(), 0);
        assert_eq!(scene_marks(&e.view()), 0);
        assert!(!states(&e).contains(&State::LocalRanked));
    }
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 0);
}

#[test]
fn a_replaced_waiting_placeholder_does_not_stay_counted() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    photo(&e.conn, 3, 200, 3); // quality row without a vector yet
    assert_eq!(e.cycle(), 1);
    assert_eq!(states(&e), vec![State::Waiting]);
    assert_eq!(e.status()["pipeline"]["scenes"]["waiting"], 1);
    vector(&mut e.conn, 3, &[1., 0.]);
    assert_eq!(e.cycle(), 1);
    let scenes = e.status()["pipeline"]["scenes"].clone();
    assert_eq!(
        (&scenes["local_ranked"], &scenes["waiting"]),
        (&json!(1), &json!(0))
    );
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_groups WHERE state='waiting'"
        ),
        0
    );
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_members m WHERE NOT EXISTS(SELECT 1 FROM ai_scene_groups g WHERE g.result_id=m.result_id)"
        ),
        0
    );
}

#[test]
fn a_deleted_photo_leaves_no_reference_and_is_not_counted() {
    let mut e = LocalEnv::new(&[]);
    for (id, seconds) in [(1, 0), (2, 100), (3, 200)] {
        ready(&mut e.conn, id, seconds);
    }
    ready(&mut e.conn, 9, 50_000); // a singleton elsewhere
    assert_eq!(e.cycle(), 2);
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 2);
    e.conn.execute("DELETE FROM photos WHERE id=2", []).unwrap();
    // Counted by what is current: the result that names the missing photo no longer counts.
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(e.cycle(), 1);
    let scenes = e.status()["pipeline"]["scenes"].clone();
    assert_eq!(scenes["local_ranked"], 2);
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=2"
        ),
        0
    );
    // A singleton whose photo is deleted: not counted at once, its reference goes with the next write.
    e.conn.execute("DELETE FROM photos WHERE id=9", []).unwrap();
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    ready(&mut e.conn, 10, 80_000);
    assert_eq!(e.cycle(), 1);
    assert_eq!(
        scene_count(
            &e.conn,
            "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=9"
        ),
        0
    );
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 2);
}

#[test]
fn a_sixteen_column_scene_table_is_migrated_and_its_old_rows_are_replaced_by_the_cycle() {
    let mut c = pair();
    let plan = plan(&c);
    let version = plan.groups()[0].processor_version().to_string();
    assert_eq!(save(&mut c, &plan, 0, &|| false).unwrap(), Saved::Stored);
    // The intermediate development shape: the same rows without the last column.
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; ALTER TABLE ai_scene_groups DROP COLUMN vector_guard;",
    )
    .unwrap();
    assert_eq!(pending(&c).unwrap(), vec!["ai_scene_groups.vector_guard"]);
    // Until migrated, nothing is read from or written to it, with a fixed error instead of a silent no-op.
    assert_eq!(
        pending_saves(&c, &plan, 8).unwrap_err(),
        Error::SchemaMismatch
    );
    assert_eq!(
        repository::header_counts_in(&c, &[&version]).unwrap_err(),
        Error::SchemaMismatch
    );
    assert!(load_current(&c, &version).unwrap().groups().is_empty());
    assert_eq!(
        migrate(&mut c).unwrap(),
        vec!["ai_scene_groups.vector_guard"]
    );
    assert!(pending(&c).unwrap().is_empty());
    assert!(migrate(&mut c).unwrap().is_empty());
    let plan = self::plan(&c); // the stored references changed since the first plan
    // The old row is still a current result for the reads (its input guard is unchanged) ...
    assert_eq!(load_current(&c, &version).unwrap().groups().len(), 1);
    assert_eq!(
        scene_count(
            &c,
            "SELECT COUNT(*) FROM ai_scene_groups WHERE vector_guard=''"
        ),
        1
    );
    // ... and the cycle replaces it, because the stored header differs from the plan's.
    assert_eq!(pending_saves(&c, &plan, 8).unwrap(), vec![0]);
    assert_eq!(save(&mut c, &plan, 0, &|| false).unwrap(), Saved::Stored);
    assert_eq!(
        scene_count(
            &c,
            "SELECT COUNT(*) FROM ai_scene_groups WHERE vector_guard=''"
        ),
        0
    );
    assert!(pending_saves(&c, &plan, 8).unwrap().is_empty());
}

#[test]
fn a_scene_table_of_another_shape_stops_the_cycle_with_a_fixed_error_and_delivery_continues() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    e.conn
        .execute_batch(
            "PRAGMA foreign_keys=OFF; DROP TABLE ai_scene_active; DROP TABLE ai_scene_members; DROP TABLE ai_scene_groups;
             CREATE TABLE ai_scene_groups(result_id TEXT PRIMARY KEY, unexpected TEXT);
             CREATE TABLE ai_scene_members(result_id TEXT, photo_id INTEGER);
             CREATE TABLE ai_scene_active(photo_id INTEGER PRIMARY KEY, result_id TEXT);",
        )
        .unwrap();
    // A table of another shape is not "to be created": it is its own class, so a dry-run does not
    // announce work that never happens (and the status keeps hiding the scene counts).
    assert!(pending(&e.conn).unwrap().is_empty());
    assert_eq!(
        mismatched(&e.conn).unwrap(),
        vec!["ai_scene_groups", "ai_scene_members"]
    );
    assert!(!current(&e.conn));
    // Not repaired and not reported as created.
    assert!(migrate(&mut e.conn).unwrap().is_empty());
    assert!(pending(&e.conn).unwrap().is_empty());
    assert_eq!(
        mismatched(&e.conn).unwrap(),
        vec!["ai_scene_groups", "ai_scene_members"]
    );
    let dry = crate::ingest::plan(&e.config, &e.conn).unwrap();
    assert_eq!(
        dry.scene_mismatch,
        vec!["ai_scene_groups", "ai_scene_members"]
    );
    assert!(
        !dry.schema
            .missing_objects
            .iter()
            .any(|o| o.starts_with("ai_scene_"))
    );
    let err = runtime::Runtime::production(e.config.clone())
        .unwrap()
        .cycle(&e.conn, &e.ingest.writer, &crate::stop::StopFlag::new())
        .unwrap_err();
    assert_eq!(err, Error::SchemaMismatch);
    assert_eq!(err.to_string(), "scene_SchemaMismatch");
    // Reads and the feed carry on; the scene status says unavailable.
    assert_eq!(e.status()["pipeline"]["scenes"]["state"], "unavailable");
    let view = e.view();
    assert!(!view["groups"].as_array().unwrap().is_empty());
    assert_eq!(scene_marks(&view), 0);
    assert!(e.loaded().groups().is_empty());
    assert_eq!(e.feed().len(), 2);
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    assert!(
        !crate::admin::status(&e.conn, &ctx)
            .unwrap()
            .contains_key("ai_scenes")
    );
}

// ---- 3rd review: a photo that is not ready any more keeps no local-result reference -------------

fn references(e: &LocalEnv) -> i64 {
    scene_count(&e.conn, "SELECT COUNT(*) FROM ai_scene_active")
}
fn reference_of(e: &LocalEnv, id: i64) -> i64 {
    scene_count(
        &e.conn,
        &format!("SELECT COUNT(*) FROM ai_scene_active WHERE photo_id={id}"),
    )
}
/// `items` of the management photo list keyed by id: whether the photo carries a `scene`.
fn admin_scenes(e: &LocalEnv) -> Vec<(i64, Option<Json>)> {
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let page = serde_json::to_value(
        crate::admin::photos(&e.conn, &ctx, 1, 50, crate::admin::Filter::All).unwrap(),
    )
    .unwrap();
    let mut out: Vec<_> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_i64().unwrap(), p.get("scene").cloned()))
        .collect();
    out.sort_by_key(|p| p.0);
    out
}
fn admin_local_ranked(e: &LocalEnv) -> Json {
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    crate::admin::status(&e.conn, &ctx).unwrap()["ai_scenes"]["local_ranked"].clone()
}
impl LocalEnv {
    fn report(&self) -> runtime::CycleReport {
        runtime::Runtime::production(self.config.clone())
            .unwrap()
            .cycle_report(
                &self.conn,
                &self.ingest.writer,
                &crate::stop::StopFlag::new(),
            )
            .unwrap()
    }
    fn scene_tables(&self) -> Vec<Vec<Vec<Value>>> {
        ["ai_scene_groups", "ai_scene_members", "ai_scene_active"]
            .iter()
            .map(|t| rows(&self.conn, t))
            .collect()
    }
}

#[test]
fn when_every_vector_is_gone_no_reference_stays_in_any_cycle_and_nothing_counts() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    assert_eq!(references(&e), 2);
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(admin_local_ranked(&e), 1);
    assert!(admin_scenes(&e).iter().all(|(_, s)| s.is_some()));
    e.conn
        .execute("DELETE FROM ai_embedding_vectors", [])
        .unwrap();
    // Before any cycle the counts and the management notes already fall back to "nothing current".
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 0);
    assert_eq!(admin_local_ranked(&e), 0);
    assert!(admin_scenes(&e).iter().all(|(_, s)| s.is_none()));
    assert!(e.loaded().groups().is_empty());
    // The first cycle stores nothing, but still drops the references (it writes for them alone).
    let first = e.report();
    assert_eq!((first.stored, first.selected, first.retired), (0, 0, 2));
    assert_eq!(references(&e), 0);
    // The ranked header stays as history and is not counted; later cycles write nothing at all.
    assert_eq!(
        scene_count(&e.conn, "SELECT COUNT(*) FROM ai_scene_groups"),
        1
    );
    let tables = e.scene_tables();
    for _ in 0..4 {
        let report = e.report();
        assert_eq!((report.stored, report.selected, report.retired), (0, 0, 0));
        assert_eq!(e.scene_tables(), tables);
        assert_eq!(references(&e), 0);
        let scenes = e.status()["pipeline"]["scenes"].clone();
        assert_eq!(
            (&scenes["local_ranked"], &scenes["waiting"]),
            (&json!(0), &json!(0))
        );
        assert_eq!(admin_local_ranked(&e), 0);
        assert!(admin_scenes(&e).iter().all(|(_, s)| s.is_none()));
    }
    assert_eq!(scene_marks(&e.view()), 0);
    // Vectors come back: the same ranked result is current again.
    vector(&mut e.conn, 1, &[1., 0.]);
    vector(&mut e.conn, 2, &[1., 0.]);
    assert_eq!(e.cycle(), 1);
    assert_eq!(references(&e), 2);
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(scene_marks(&e.view()), 2);
}

/// Photos 1 and 2 form R{1,2}, photo 9 is a singleton far away. `lose` makes photo 2 not ready.
fn photo_two_becomes_not_ready(lose: impl FnOnce(&mut Connection)) -> LocalEnv {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    ready(&mut e.conn, 9, 50_000);
    assert_eq!(e.cycle(), 2);
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 2);
    lose(&mut e.conn);
    // At once, before any cycle: R{1,2} and the note of photo 2 are not current any more.
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(admin_local_ranked(&e), 1);
    let notes = admin_scenes(&e);
    assert!(notes.iter().find(|n| n.0 == 2).unwrap().1.is_none());
    assert!(notes.iter().find(|n| n.0 == 9).unwrap().1.is_some());
    for _ in 0..4 {
        e.cycle();
    }
    // The reference of photo 2 is gone for good; it stays gone in later cycles and photo 9 keeps its own.
    assert_eq!(reference_of(&e, 2), 0);
    assert_eq!(reference_of(&e, 9), 1);
    let report = e.report();
    assert_eq!((report.stored, report.selected, report.retired), (0, 0, 0));
    assert_eq!(reference_of(&e, 2), 0);
    assert_eq!(admin_scenes(&e).iter().find(|n| n.0 == 2).unwrap().1, None);
    assert!(scene_marks(&e.view()) >= 1);
    e
}
fn has_no_scene_mark(e: &LocalEnv, id: i64) -> bool {
    !e.view()["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["photos"].as_array().unwrap().iter())
        .any(|c| c["id"] == id && c["scene_state"].is_string())
}

#[test]
fn a_photo_whose_vector_is_gone_loses_its_reference_while_the_rest_stays() {
    let e = photo_two_becomes_not_ready(|c| {
        c.execute(
            "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
            [sha_hex(2)],
        )
        .unwrap();
    });
    // Photo 1 is next to a photo whose embedding is missing: a waiting placeholder, no reference.
    assert_eq!(reference_of(&e, 1), 0);
    assert_eq!(states(&e), vec![State::Waiting, State::LocalRanked]);
    assert!(has_no_scene_mark(&e, 2));
}

#[test]
fn a_photo_whose_embedding_failed_loses_its_reference() {
    let e = photo_two_becomes_not_ready(|c| {
        c.execute(
            "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
            [sha_hex(2)],
        )
        .unwrap();
        fail(c, 2);
    });
    // A failed photo is excluded instead of holding its neighbours back: photo 1 is a ranked singleton.
    assert_eq!(reference_of(&e, 1), 1);
    assert_eq!(states(&e), vec![State::LocalRanked, State::LocalRanked]);
    assert!(has_no_scene_mark(&e, 2));
}

#[test]
fn a_photo_that_lost_its_capture_time_loses_its_reference() {
    let e = photo_two_becomes_not_ready(|c| {
        c.execute("UPDATE photos SET taken_at=NULL WHERE id=2", [])
            .unwrap();
    });
    assert_eq!(reference_of(&e, 1), 1); // alone and ranked: the missing time is not a pending window
    assert!(has_no_scene_mark(&e, 2));
}

#[test]
fn a_reference_is_kept_when_the_photo_became_ready_again_after_the_plan() {
    let mut c = pair();
    let first = plan(&c);
    assert_eq!(save(&mut c, &first, 0, &|| false).unwrap(), Saved::Stored);
    c.execute(
        "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
        [sha_hex(2)],
    )
    .unwrap();
    let stale = plan(&c);
    assert_eq!(pending_retire(&c, &stale).unwrap(), 1);
    // The vector is back before the writer runs: the state the plan saw no longer holds, so the
    // reference stays (the next cycle decides) instead of being dropped and re-created back and forth.
    vector(&mut c, 2, &[1., 0.]);
    let (saved, retired) = save_batch_counted(&mut c, &stale, &[], &|| false).unwrap();
    assert!(saved.is_empty());
    assert_eq!(retired, 0);
    assert_eq!(
        scene_count(&c, "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=2"),
        1
    );
    // The plan taken now sees both ready: the stored result is unchanged and there is nothing to retire.
    let now = plan(&c);
    assert_eq!(now.retire_len(), 0);
    assert!(pending_saves(&c, &now, 8).unwrap().is_empty());
    assert_eq!(pending_retire(&c, &now).unwrap(), 0);
}

#[test]
fn references_of_other_spaces_or_options_are_not_retired() {
    let mut c = pair();
    let first = plan(&c);
    assert_eq!(save(&mut c, &first, 0, &|| false).unwrap(), Saved::Stored);
    c.execute(
        "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
        [sha_hex(2)],
    )
    .unwrap();
    // The same photos under other options: their stored result is not this plan's to touch.
    let other = Options::new(60, 0.9, 3).unwrap();
    let snap = snapshot(&c).unwrap().unwrap();
    let plan_other = compute(&snap, other, Err(rank::Reason::Unconfigured), &|| false).unwrap();
    assert_eq!(plan_other.retire_len(), 1);
    assert_eq!(pending_retire(&c, &plan_other).unwrap(), 0);
    let (_, retired) = save_batch_counted(&mut c, &plan_other, &[], &|| false).unwrap();
    assert_eq!(retired, 0);
    assert_eq!(
        scene_count(&c, "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=2"),
        1
    );
    // The default options own it and drop it.
    let own = plan(&c);
    assert_eq!(pending_retire(&c, &own).unwrap(), 1);
    assert_eq!(
        save_batch_counted(&mut c, &own, &[], &|| false).unwrap().1,
        1
    );
    assert_eq!(
        scene_count(&c, "SELECT COUNT(*) FROM ai_scene_active WHERE photo_id=2"),
        0
    );
}

#[test]
fn references_kept_while_another_space_is_active_are_not_retired_and_return_with_their_space() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    assert_eq!(references(&e), 2);
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    let tables = e.scene_tables();
    // Another space becomes active; it has no vector yet, so the photos are not ready there.
    let other = SpaceIdentity::new(
        "scene-b",
        "shared-b",
        "model-b",
        "rev-b",
        "img-b",
        "txt-b",
        2,
        Normalization::Raw,
    )
    .unwrap();
    activate(&mut e.conn, &other);
    // The plan does see references that belong to no component of this space...
    let snap = snapshot(&e.conn).unwrap().unwrap();
    let plan_b = compute(
        &snap,
        Options::default(),
        Err(rank::Reason::Unconfigured),
        &|| false,
    )
    .unwrap();
    assert_eq!(plan_b.retire_len(), 2);
    // ...but they are the stored result of the first space: nothing is retired, nothing is written.
    assert_eq!(pending_retire(&e.conn, &plan_b).unwrap(), 0);
    for _ in 0..3 {
        let report = e.report();
        assert_eq!((report.stored, report.selected, report.retired), (0, 0, 0));
        assert_eq!(report.writer_save, std::time::Duration::ZERO);
        assert_eq!(e.scene_tables(), tables);
        assert_eq!(references(&e), 2);
    }
    // Back to the first space: the same stored result is current again, with no cycle needed to rebuild.
    activate(&mut e.conn, &identity(2));
    assert_eq!(e.status()["pipeline"]["scenes"]["local_ranked"], 1);
    assert_eq!(scene_marks(&e.view()), 2);
    let report = e.report();
    assert_eq!((report.stored, report.selected, report.retired), (0, 0, 0));
    assert_eq!(e.scene_tables(), tables);
}

#[test]
fn a_plan_of_one_space_retires_nothing_once_another_space_is_active() {
    let mut c = pair();
    let first = plan(&c);
    assert_eq!(save(&mut c, &first, 0, &|| false).unwrap(), Saved::Stored);
    c.execute(
        "DELETE FROM ai_embedding_vectors WHERE input_sha256=?1",
        [sha_hex(2)],
    )
    .unwrap();
    let stale = plan(&c);
    assert_eq!(pending_retire(&c, &stale).unwrap(), 1);
    // The active space is switched between the plan and the writer: the plan is not this space's to apply.
    let other = SpaceIdentity::new(
        "scene-b",
        "shared-b",
        "model-b",
        "rev-b",
        "img-b",
        "txt-b",
        2,
        Normalization::Raw,
    )
    .unwrap();
    activate(&mut c, &other);
    let tables = ["ai_scene_groups", "ai_scene_members", "ai_scene_active"].map(|t| rows(&c, t));
    let (saved, retired) = save_batch_counted(&mut c, &stale, &[], &|| false).unwrap();
    assert!(saved.is_empty());
    assert_eq!(retired, 0);
    assert_eq!(
        ["ai_scene_groups", "ai_scene_members", "ai_scene_active"].map(|t| rows(&c, t)),
        tables
    );
}

#[test]
fn a_sixteen_column_scene_table_makes_the_status_unavailable_and_hides_the_admin_counts() {
    let mut e = LocalEnv::new(&[]);
    ready(&mut e.conn, 1, 0);
    ready(&mut e.conn, 2, 100);
    assert_eq!(e.cycle(), 1);
    assert_eq!(e.status()["pipeline"]["scenes"]["state"], "active");
    // The intermediate development shape appears under a running service (no migration ran).
    e.conn
        .execute_batch(
            "PRAGMA foreign_keys=OFF; ALTER TABLE ai_scene_groups DROP COLUMN vector_guard;",
        )
        .unwrap();
    assert_eq!(
        pending(&e.conn).unwrap(),
        vec!["ai_scene_groups.vector_guard"]
    );
    assert!(mismatched(&e.conn).unwrap().is_empty());
    // E5: the scene block says so, with zero counts; the rest of the status and the feed carry on.
    let status = e.status();
    assert_eq!(status["pipeline"]["scenes"]["state"], "unavailable");
    assert_eq!(status["pipeline"]["scenes"]["local_ranked"], 0);
    assert_eq!(e.feed().len(), 2);
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    assert!(
        !crate::admin::status(&e.conn, &ctx)
            .unwrap()
            .contains_key("ai_scenes")
    );
    assert!(admin_scenes(&e).iter().all(|(_, s)| s.is_none()));
    assert_eq!(scene_marks(&e.view()), 0);
    let err = runtime::Runtime::production(e.config.clone())
        .unwrap()
        .cycle(&e.conn, &e.ingest.writer, &crate::stop::StopFlag::new())
        .unwrap_err();
    assert_eq!(err, Error::SchemaMismatch);
}

/// Measurement only (`--ignored --nocapture`): writer time of one batch that stores a component while
/// thousands of waiting placeholders exist (the first creation of the scenes of a large catalog).
/// The placeholders are synthetic headers of the plan's space and options, two members each; the stored
/// component is two ready photos elsewhere, so the batch's housekeeping has to look at every placeholder.
#[test]
#[ignore = "measurement: writer time with thousands of waiting placeholders"]
fn thousands_of_waiting_placeholders_measure_the_writer_batch() {
    use std::time::Instant;
    const PHOTOS: i64 = 8000;
    for waiting in [0_i64, 4000] {
        let dir = TempDir::new("scene-waiting-measure");
        let mut c = Connection::open(dir.path().join("catalog.db")).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        schema::migrate(&mut c).unwrap();
        index::migrate(&mut c).unwrap();
        jobs::migrate(&mut c).unwrap();
        migrate(&mut c).unwrap();
        activate(&mut c, &identity(2));
        let tx = c.transaction().unwrap();
        for id in 1..=PHOTOS {
            tx.execute("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,lat,lon) VALUES(?1,'local','synthetic',?2,'m',1,0,1,2)",params![id,format!("synthetic-{id}")]).unwrap();
        }
        let options = Options::default();
        for n in 0..waiting {
            let id = format!("waiting-{n}");
            tx.execute("INSERT INTO ai_scene_groups VALUES(?1,'scene-a',?2,'g','p',?3,?4,?5,'waiting','disjoint','null',NULL,0,1,'x','[]','v')",
                params![id, identity(2).fingerprint(), options.window_s, options.similarity, options.shortlist as i64]).unwrap();
            for (rank, photo) in [n * 2 + 1, n * 2 + 2].into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO ai_scene_members VALUES(?1,?2,'g',?3,NULL,0,0,1,0)",
                    params![id, photo, rank as i64 + 1],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
        ready(&mut c, 10_001, 0);
        ready(&mut c, 10_002, 100);
        let plan = plan(&c);
        assert_eq!(plan.groups().len(), 1);
        let start = Instant::now();
        let (saved, _) = save_batch_counted(&mut c, &plan, &[0], &|| false).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(saved, vec![Saved::Stored]);
        // The placeholders share no photo with the stored component and name existing photos: kept.
        assert_eq!(
            scene_count(
                &c,
                "SELECT COUNT(*) FROM ai_scene_groups WHERE state='waiting'"
            ),
            waiting
        );
        eprintln!(
            "WAITING_TIDY placeholders={waiting} photos={PHOTOS} writer_batch_ms={:.1}",
            elapsed.as_secs_f64() * 1000.
        );
    }
}

/// Measurement only (`--ignored --nocapture`): cost of the stored-scene read paths on a synthetic
/// 6500x768 catalog. The scenes are computed once with a verified weight (some components fall back
/// to the fixed rule: missing luminance), published through the repository, and mixed with old
/// exact/partial groups. The read functions are called directly; nothing leaves the process.
#[test]
#[ignore = "measurement: synthetic 6500x768 read paths"]
fn synthetic_6500x768_read_paths_measure() {
    use crate::ai::{http::Deadline, search::QueryVector, search_view};
    use std::time::{Duration, Instant};
    const SCENES: i64 = 1000;
    const SCENE_SIZE: i64 = 5;
    const SINGLES: i64 = 1500;
    const OLD_GROUPS: i64 = 200; // components below this carry an old group (exact / partial)
    const SLOT_S: i64 = 2000; // components are further apart than the 900 s window
    const SLOTS_PER_DAY: i64 = 43;
    const CALLS: usize = 20;
    fn rss_kib(key: &str) -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|l| l.starts_with(key))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }
    fn measure<T>(name: &str, mut run: impl FnMut() -> T, check: impl Fn(&T)) -> f64 {
        let before = rss_kib("VmRSS:");
        let mut times = Vec::new();
        for _ in 0..CALLS {
            let start = Instant::now();
            let out = run();
            times.push(start.elapsed());
            check(&out);
        }
        let first = times[0];
        times.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.;
        let median = ms((times[CALLS / 2 - 1] + times[CALLS / 2]) / 2);
        eprintln!(
            "READ_PATH {name}: calls={CALLS} first_ms={:.1} median_ms={median:.1} max_ms={:.1} rss_before_kib={before} rss_after_kib={}",
            ms(first),
            ms(times[CALLS - 1]),
            rss_kib("VmRSS:")
        );
        median
    }
    fn profile(name: &str, c: &Connection, run: impl FnOnce()) {
        PROFILE.with(|p| p.borrow_mut().clear());
        c.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(log_profile),
        );
        let start = Instant::now();
        run();
        let total = start.elapsed();
        c.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
        let mut by_sql: BTreeMap<String, (usize, Duration)> = BTreeMap::new();
        for (sql, d) in PROFILE.with(|p| std::mem::take(&mut *p.borrow_mut())) {
            let key: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
            let key = key.chars().take(110).collect::<String>();
            let slot = by_sql.entry(key).or_default();
            slot.0 += 1;
            slot.1 += d;
        }
        let mut rows: Vec<_> = by_sql.into_iter().collect();
        rows.sort_by_key(|(_, (_, d))| std::cmp::Reverse(*d));
        eprintln!(
            "PROFILE {name}: total_ms={:.1}",
            total.as_secs_f64() * 1000.
        );
        for (sql, (n, d)) in rows.iter().take(10) {
            eprintln!("PROFILE   {:>7.1} ms x{n}: {sql}", d.as_secs_f64() * 1000.);
        }
    }

    let mut e = LocalEnv::new(&[]);
    // `LocalEnv` already registered a 2-dimensional space under the shared test id.
    let s = SpaceIdentity::new(
        "scene-768",
        "shared-768",
        "model-a",
        "rev-a",
        "img-a",
        "txt-a",
        768,
        Normalization::Raw,
    )
    .unwrap();
    activate(&mut e.conn, &s);
    // A verified weight on `luminance`: components whose photos lack it fall back to the fixed rule.
    std::fs::create_dir_all(e.config.data_dir.join("models")).unwrap();
    let mut weight = rank_json(&s);
    weight["base_features"] = json!(["luminance"]);
    std::fs::write(&e.config.rank_model_path, weight.to_string()).unwrap();

    let components: Vec<Vec<i64>> = (0..SCENES)
        .map(|c| (1..=SCENE_SIZE).map(|k| c * SCENE_SIZE + k).collect())
        .chain((0..SINGLES).map(|k| vec![SCENES * SCENE_SIZE + k + 1]))
        .collect();
    assert_eq!(components.iter().map(Vec::len).sum::<usize>(), 6500);
    let stamp = |c: i64, k: i64| {
        let t = (c / SLOTS_PER_DAY) * 86400 + (c % SLOTS_PER_DAY) * SLOT_S + k * 30;
        let (day, sec) = (t / 86400, t % 86400);
        format!(
            "2026-{:02}-{:02} {:02}:{:02}:{:02}",
            1 + day / 28,
            1 + day % 28,
            sec / 3600,
            (sec / 60) % 60,
            sec % 60
        )
    };
    let tx = e.conn.transaction().unwrap();
    {
        let mut photo = tx.prepare("INSERT INTO photos(id,source,name,display,md5,mtime,rotation,taken_at,lat,lon) VALUES(?1,'local','synthetic',?2,'m',1,0,?3,1,2)").unwrap();
        let mut quality = tx.prepare("INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
           VALUES(?1,?2,'m',1,?3,'0',?4,?5,90,0,0,100,'','synthetic')").unwrap();
        let mut vector = tx
            .prepare("INSERT INTO ai_embedding_vectors VALUES(?4,?1,?2,768,?3,0)")
            .unwrap();
        for (c, ids) in components.iter().enumerate() {
            let c = c as i64;
            for (k, &id) in ids.iter().enumerate() {
                let sha = format!("{id:064x}");
                let display = format!("synthetic-{id}");
                let luminance = (c % 4 != 0).then_some(128.0);
                let mut v = vec![0.0_f32; 768];
                v[0] = 1.;
                v[1 + (id % 700) as usize] = 0.02;
                photo
                    .execute(params![id, display, stamp(c, k as i64)])
                    .unwrap();
                quality
                    .execute(params![id, display, sha, 1.0 + (id % 9) as f64, luminance])
                    .unwrap();
                vector
                    .execute(params![
                        sha,
                        "f".repeat(64),
                        crate::ai::space::encode(&v),
                        s.space_id()
                    ])
                    .unwrap();
            }
        }
    }
    // Old selection: every tenth component exactly, and the one after it only partly.
    let (mut exact, mut partial) = (0, 0);
    for c in 0..OLD_GROUPS {
        let (kind, ids) = match c % 10 {
            0 => ("exact", &components[c as usize][..]),
            1 => ("partial", &components[c as usize][..3]),
            _ => continue,
        };
        if kind == "exact" {
            exact += 1;
        } else {
            partial += 1;
        }
        let group = format!("old-{kind}-{c}");
        tx.execute("INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene,judge_model,judge_version) VALUES(?1,'scene','old','synthetic','judged',1,'synthetic-model','old-prompt')",[&group]).unwrap();
        for (rank, id) in ids.iter().enumerate() {
            tx.execute("INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,embedding_model,embedding_dim,embedding,analyzed_at,error)
               SELECT id,display,md5,mtime,?2,'old-model',2,zeroblob(8),'synthetic','' FROM photos WHERE id=?1",params![id,format!("{id:064x}")]).unwrap();
            tx.execute("INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,shortlisted,local_score) VALUES(?1,?2,?3,?4,1,50)",params![group,id,rank as i64+1,rank==0]).unwrap();
        }
    }
    tx.commit().unwrap();

    // Publish the scenes the way the cycle does (one weight, bounded batches), without 150+ full cycles.
    let started = Instant::now();
    let snap = snapshot(&e.conn).unwrap().unwrap();
    let model = rank::Model::load(&e.config.rank_model_path, snap.identity());
    assert!(model.is_ok());
    let plan = compute(
        &snap,
        e.config.ai.scene_options(),
        model.as_ref().map_err(|r| *r),
        &|| false,
    )
    .unwrap();
    let count = |f: &dyn Fn(&Group) -> bool| plan.groups().iter().filter(|g| f(g)).count() as i64;
    let local_ranked = count(&|g| g.state() == State::LocalRanked);
    let review_overlap = count(&|g| g.state() == State::ReviewOverlap);
    let linear = count(&|g| g.rank_used() == RankUsed::Linear);
    let fallback = count(&|g| matches!(g.rank_used(), RankUsed::Fallback(_)));
    assert_eq!(plan.groups().len() as i64, SCENES + SINGLES);
    assert_eq!(count(&|g| g.overlap() == Overlap::Exact), exact);
    assert_eq!(review_overlap, partial);
    assert_eq!(linear + fallback, SCENES + SINGLES);
    assert!(linear > 0 && fallback > 0);
    let pending = pending_saves(&e.conn, &plan, usize::MAX).unwrap();
    assert_eq!(pending.len(), plan.groups().len());
    let plan = Arc::new(plan);
    for chunk in pending.chunks(runtime::GROUPS_PER_CYCLE) {
        let (p, indexes) = (Arc::clone(&plan), chunk.to_vec());
        let saved = e
            .ingest
            .writer
            .call(move |c| Ok(save_batch(c, &p, &indexes, &|| false)))
            .unwrap()
            .unwrap();
        assert!(saved.iter().all(|s| *s == Saved::Stored));
    }
    eprintln!(
        "READ_PATH setup: groups={} (local_ranked={local_ranked} review_overlap={review_overlap} exact={exact} linear={linear} fallback={fallback}) publish_ms={:.0} rss_kib={} hwm_kib={}",
        plan.groups().len(),
        started.elapsed().as_secs_f64() * 1000.,
        rss_kib("VmRSS:"),
        rss_kib("VmHWM:")
    );
    let scenes = |v: &Json| {
        json!([
            v["local_ranked"],
            v["review_overlap"],
            v["linear"],
            v["rules"]
        ])
    };
    let expected = json!([local_ranked, review_overlap, linear, fallback]);

    // E6: every returned card must carry its stored scene (an empty read would be a silent miss).
    let cards = |view: &Json| -> Vec<Json> {
        view["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["photos"].as_array().unwrap().clone())
            .collect()
    };
    let ctx = crate::admin::AdminContext::new(&e.config, &e.config.data_dir);
    let mut q = vec![0.0_f32; 768];
    q[0] = 1.;
    let query = QueryVector::new(&s, &q).unwrap();
    // The same four routes (plus the management page) in three conditions: scenes published,
    // scene tables present but empty, scene tables absent. The difference is what the scene layer adds.
    let routes = |phase: &str, with_scenes: bool| -> [f64; 5] {
        let tag = |name: &str| format!("{phase}: {name}");
        let check_view = |view: &Json| {
            assert_eq!(view["searched_photos"], 6500);
            let cards = cards(view);
            assert!(!cards.is_empty());
            assert_eq!(
                cards.iter().all(|c| c["scene_state"].is_string()),
                with_scenes
            );
            assert_eq!(
                cards.iter().any(|c| c["scene_overlap"] == "exact"),
                with_scenes
            );
        };
        let e6_empty = measure(
            &tag("E6 empty query"),
            || {
                search_view::search(
                    &e.conn,
                    &e.config,
                    "",
                    12,
                    false,
                    None,
                    &Deadline::interactive(),
                )
                .unwrap()
                .unwrap()
            },
            check_view,
        );
        let e6_semantic = measure(
            &tag("E6 semantic query"),
            || {
                search_view::search(
                    &e.conn,
                    &e.config,
                    "synthetic",
                    12,
                    false,
                    Some(&query),
                    &Deadline::interactive(),
                )
                .unwrap()
                .unwrap()
            },
            check_view,
        );
        let e5 = measure(
            &tag("E5 status"),
            || search_view::status(&e.conn, &e.config).unwrap(),
            |status| {
                if with_scenes {
                    assert_eq!(status["pipeline"]["scenes"]["state"], "active");
                    assert_eq!(scenes(&status["pipeline"]["scenes"]), expected);
                } else {
                    assert_eq!(status["pipeline"]["scenes"]["local_ranked"], 0);
                }
            },
        );
        let admin = measure(
            &tag("admin STATUS"),
            || crate::admin::status(&e.conn, &ctx).unwrap(),
            |status| {
                if with_scenes {
                    assert_eq!(status["ai_scenes"]["state"], "active");
                    assert_eq!(scenes(&status["ai_scenes"]), expected);
                }
            },
        );
        let photos = measure(
            &tag("admin photos page (per=50)"),
            || crate::admin::photos(&e.conn, &ctx, 1, 50, crate::admin::Filter::All).unwrap(),
            |page| {
                let page = serde_json::to_value(page).unwrap();
                let items = page["items"].as_array().unwrap();
                assert_eq!(items.len(), 50);
                assert_eq!(
                    items.iter().all(|i| i["scene"]["state"].is_string()),
                    with_scenes
                );
            },
        );
        [e6_empty, e6_semantic, e5, admin, photos]
    };
    let with = routes("scenes", true);
    // Breakdown of the stored-scene work, per function, on the published scenes.
    let timed = |name: &str, f: &mut dyn FnMut()| {
        let mut times = Vec::new();
        for _ in 0..CALLS {
            let start = Instant::now();
            f();
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "BREAKDOWN {name}: median_ms={:.2}",
            (times[CALLS / 2 - 1] + times[CALLS / 2]) / 2.
        );
    };
    {
        use crate::ai::groups::runtime::{Prepared, status_summary};
        let prepared = Prepared::read(&e.conn, &e.config);
        timed("scene: Prepared::read (active_in + rankfile)", &mut || {
            Prepared::read(&e.conn, &e.config).unwrap();
        });
        timed("scene: load_in (validated projection, E6)", &mut || {
            let tx = e.conn.unchecked_transaction().unwrap();
            assert!(
                !prepared
                    .as_ref()
                    .unwrap()
                    .load_in(&tx)
                    .unwrap()
                    .groups()
                    .is_empty()
            );
        });
        timed(
            "scene: status_summary (header counts, E5/admin)",
            &mut || {
                let tx = e.conn.unchecked_transaction().unwrap();
                assert_eq!(status_summary(&prepared, &tx)["state"], "active");
            },
        );
        timed("scene: annotations_in (50 photos)", &mut || {
            let tx = e.conn.unchecked_transaction().unwrap();
            let ids: Vec<i64> = (1..=50).collect();
            assert!(
                !prepared
                    .as_ref()
                    .unwrap()
                    .annotations_in(&tx, &ids)
                    .unwrap()
                    .is_empty()
            );
        });
        timed("prior: index::status_in (corrupt count, scan)", &mut || {
            let tx = e.conn.unchecked_transaction().unwrap();
            crate::ai::index::status_in(&tx, false).unwrap();
        });
        timed("prior: search::load_in (E6 vectors)", &mut || {
            let tx = e.conn.unchecked_transaction().unwrap();
            crate::ai::search::load_in(&tx).unwrap();
        });
        timed(
            "prior: selection::admin_counts (old selection)",
            &mut || {
                let tx = e.conn.unchecked_transaction().unwrap();
                crate::selection::admin_counts(&tx).unwrap();
            },
        );
    }
    // Per-statement time of the two status routes (SQLite profile; includes the Rust work between steps).
    profile("E5 status", &e.conn, || {
        search_view::status(&e.conn, &e.config).unwrap();
    });
    profile("admin STATUS", &e.conn, || {
        crate::admin::status(&e.conn, &ctx).unwrap();
    });
    profile("E6 empty query", &e.conn, || {
        search_view::search(
            &e.conn,
            &e.config,
            "",
            12,
            false,
            None,
            &Deadline::interactive(),
        )
        .unwrap()
        .unwrap();
    });
    e.conn.execute("DELETE FROM ai_scene_active", []).unwrap();
    e.conn.execute("DELETE FROM ai_scene_members", []).unwrap();
    e.conn.execute("DELETE FROM ai_scene_groups", []).unwrap();
    let empty = routes("empty scene tables", false);
    e.conn
        .execute_batch(
            "DROP TABLE ai_scene_active; DROP TABLE ai_scene_members; DROP TABLE ai_scene_groups;",
        )
        .unwrap();
    let absent = routes("absent scene tables", false);
    let names = [
        "E6 empty query",
        "E6 semantic query",
        "E5 status",
        "admin STATUS",
        "admin photos page",
    ];
    for i in 0..5 {
        eprintln!(
            "SCENE_LAYER {}: with_scenes_ms={:.1} empty_tables_ms={:.1} absent_tables_ms={:.1} added_vs_empty_ms={:.1} added_vs_absent_ms={:.1}",
            names[i],
            with[i],
            empty[i],
            absent[i],
            with[i] - empty[i],
            with[i] - absent[i]
        );
    }
    eprintln!("READ_PATH peak: hwm_kib={}", rss_kib("VmHWM:"));
}
