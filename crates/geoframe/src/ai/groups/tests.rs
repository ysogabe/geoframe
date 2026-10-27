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
fn python_fixture_is_reproducible_without_weight_or_data_access() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/gen_scene_fixture.py"
        ))
        .arg("--check")
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(output.status.success(), "synthetic Python fixture failed");
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
        RankUsed::Rules(Some(rank::Reason::Overflow))
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
        RankUsed::Rules(Some(rank::Reason::MissingFeature))
    );
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
    let path = dir.path().join("synthetic.db");
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
    let result = save(&mut c, &p, 0, &|| false).unwrap();
    let save_time = start.elapsed();
    assert_eq!(result, Saved::Stored);
    assert_eq!(
        load_current(&c, p.groups[0].processor_version())
            .unwrap()
            .groups()
            .len(),
        1
    );
    eprintln!(
        "scene synthetic 6500x768 all-same-time: snapshot={snapshot_time:?}, compute={compute_time:?}, writer/save={save_time:?}; RSS before={before}, snapshot={after_snapshot}, compute={after_compute}, after-save/read={}",
        rss()
    );
}

#[test]
fn python_rollback_reads_and_rebuilds_legacy_without_touching_new_results() {
    let dir = TempDir::new("scene-python-rollback");
    let path = dir.path().join("catalog.db");
    let mut c = Connection::open(&path).unwrap();
    schema::migrate(&mut c).unwrap();
    index::migrate(&mut c).unwrap();
    jobs::migrate(&mut c).unwrap();
    migrate(&mut c).unwrap();
    activate(&mut c, &identity(2));
    ready(&mut c, 1, 0);
    ready(&mut c, 2, 100);
    legacy(&c, &[1, 2]);
    let p = plan(&c);
    save(&mut c, &p, 0, &|| false).unwrap();
    let tables = [
        "photos",
        "photo_ai_groups",
        "photo_ai_group_members",
        "photo_ai_features",
        "ai_scene_groups",
        "ai_scene_members",
        "ai_scene_active",
    ];
    let before: Vec<_> = tables.iter().map(|t| rows(&c, t)).collect();
    let script = format!(
        r#"
import os, sys, json
sys.path.insert(0, {app:?})
from geoframe import ai_pipeline, ai_search, photos, store
feed=photos.api_photos()
status=ai_search.api_ai_status()
loaded=ai_pipeline._load_photos()
ai_pipeline.load_rank_model=lambda *_: None
# Exact same synthetic legacy embedding model/shape; no file/HTTP/model calls.
ai_pipeline._rebuild_groups(loaded)
print(json.dumps(dict(feed=len(feed), status=isinstance(status,dict), loaded=len(loaded))))
"#,
        app = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app")
    );
    let output = std::process::Command::new("python3")
        .args(["-I", "-c", &script])
        .env_clear()
        .env("GEOFRAME_DATA_DIR", dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "synthetic Python rollback failed");
    let text = String::from_utf8(output.stdout).unwrap();
    let result: Json = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(result["status"], true);
    // Python intentionally rebuilds legacy groups in its cycle. Human columns and every new table remain exact.
    for index in [0, 4, 5, 6] {
        assert!(rows(&c, tables[index]) == before[index]);
    }
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
