//! 固定 prompt / schema の Python 一致、応答の検査、同一性の鍵、request の形。通信・DB は使わない。
use super::*;
use crate::ai::image_input::ImageProfile;
use serde_json::{Value, json};
use std::process::Command;

fn python_constant(name: &str) -> Value {
    let script = format!(
        "import ast,json,sys\n\
         t=ast.parse(open(sys.argv[1]).read())\n\
         for n in t.body:\n\
         \x20\x20if isinstance(n,ast.Assign) and n.targets[0].id=={name:?}:\n\
         \x20\x20\x20\x20print(json.dumps(ast.literal_eval(n.value)))"
    );
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/geoframe/ai_pipeline.py");
    let out = Command::new("python3")
        .args(["-I", "-c", &script])
        .arg(source)
        .output()
        .unwrap();
    assert!(out.status.success(), "AST extraction failed");
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn prompt_schema_and_versions_match_the_python_constants() {
    assert_eq!(json!(PROMPT), python_constant("JUDGE_PROMPT"));
    assert_eq!(json!(PROMPT_VERSION), python_constant("JUDGE_VERSION"));
    assert_eq!(python_constant("MAX_JUDGE_ATTEMPTS"), json!(5));
    assert_eq!(crate::ai::jobs::MAX_ATTEMPTS, 5);
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/ai-cloud-schemas-python.json"
    ))
    .unwrap();
    assert_eq!(schema(), fixture["schemas"]["photo_judge"]);
    assert!(
        schema()["properties"]["photos"]["items"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "keep_score")
    );
}

fn photo(index: usize, keep: u8, recommended: bool) -> Value {
    json!({"index":index,"subject":"person","pet_state":"not_applicable","eyes":"open",
        "face_crop":"good","horizon_status":"level","horizon_score":90,"subject_sharpness":80,
        "aesthetic_score":70,"keep_score":keep,"recommended":recommended,
        "reasons":["合成の理由"],"concerns":[]})
}
fn reply(same: bool, photos: Vec<Value>) -> String {
    json!({"same_scene":same,"summary":"合成の要約","photos":photos}).to_string()
}

#[test]
fn verdict_requires_every_index_and_normalizes_like_python() {
    let ok = Verdict::parse(
        &reply(true, vec![photo(2, 40, false), photo(1, 70, true)]),
        2,
    )
    .unwrap();
    assert_eq!(
        ok.photos()
            .iter()
            .map(PhotoVerdict::index)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    // 同一場面なのに推薦 0 枚なら keep_score が最大の 1 枚（同点は送信順で先）を推薦にする。
    let none = Verdict::parse(
        &reply(
            true,
            vec![
                photo(1, 40, false),
                photo(2, 90, false),
                photo(3, 90, false),
            ],
        ),
        3,
    )
    .unwrap();
    assert_eq!(
        none.photos()
            .iter()
            .map(PhotoVerdict::recommended)
            .collect::<Vec<_>>(),
        [false, true, false]
    );
    // 別場面では推薦の補完はしない。
    let apart = Verdict::parse(
        &reply(false, vec![photo(1, 40, false), photo(2, 90, false)]),
        2,
    )
    .unwrap();
    assert!(apart.photos().iter().all(|p| !p.recommended()));
    // 範囲外の番号は無視する（Python と同じ）。足りない・重複・型/列挙/範囲の不正は拒否する。
    assert!(
        Verdict::parse(
            &reply(
                true,
                vec![photo(1, 40, true), photo(2, 40, false), photo(9, 1, true)]
            ),
            2
        )
        .is_ok()
    );
    for bad in [
        reply(true, vec![photo(1, 40, true)]),
        reply(true, vec![photo(1, 40, true), photo(1, 40, true)]),
        "not json".to_string(),
        "[]".to_string(),
        json!({"same_scene":"yes","summary":"x","photos":[]}).to_string(),
        json!({"same_scene":true,"photos":[]}).to_string(),
    ] {
        assert_eq!(
            Verdict::parse(&bad, 2).unwrap_err(),
            DomainError::BadResponse
        );
    }
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut one = photo(1, 40, true);
        f(&mut one);
        Verdict::parse(&reply(true, vec![one, photo(2, 40, false)]), 2)
    };
    assert!(mutate(&|p| p["subject"] = json!("unknown_subject")).is_err());
    assert!(mutate(&|p| p["keep_score"] = json!(101)).is_err());
    assert!(mutate(&|p| p["keep_score"] = json!(-1)).is_err());
    assert!(mutate(&|p| p["keep_score"] = json!(1.5)).is_err());
    assert!(mutate(&|p| p["recommended"] = json!("yes")).is_err());
    assert!(mutate(&|p| p["reasons"] = json!("text")).is_err());
    // 理由・懸念は欠けても空として読む（Python の `or []`）。
    assert!(
        mutate(&|p| {
            p.as_object_mut().unwrap().remove("reasons");
        })
        .is_ok()
    );
    assert!(Verdict::parse(&reply(true, vec![]), 0).is_err());
    assert!(Verdict::parse(&"x".repeat(2 * 1024 * 1024), 2).is_err());
}

#[test]
fn stored_verdicts_round_trip_and_any_tampering_is_refused() {
    let v = Verdict::parse(
        &reply(true, vec![photo(1, 70, true), photo(2, 40, false)]),
        2,
    )
    .unwrap();
    let text = v.normalized_json();
    assert!(Verdict::from_stored(&text, 2).is_some_and(|r| r == v));
    assert!(Verdict::from_stored(&text, 3).is_none());
    assert!(Verdict::from_stored(&text.replace("\"level\"", "\"other\""), 2).is_none());
    assert!(
        Verdict::from_stored(&text.replace("\"keep_score\":70", "\"keep_score\":170"), 2).is_none()
    );
    // 同一場面で推薦が 0 枚の保存は破損。
    assert!(
        Verdict::from_stored(
            &text.replace("\"recommended\":true", "\"recommended\":false"),
            2
        )
        .is_none()
    );
    assert!(Verdict::from_stored(&format!(" {text}"), 2).is_none());
    // Python と同じ意味: 同一場面は送った写真の推薦、送らなかった構成員は非推薦、別場面は全員を残す。
    assert!(v.recommended(Some(0)) && !v.recommended(Some(1)) && !v.recommended(None));
    let apart = Verdict::parse(
        &reply(false, vec![photo(1, 70, false), photo(2, 40, false)]),
        2,
    )
    .unwrap();
    assert!(apart.recommended(Some(1)) && apart.recommended(None));
    // 内容は Debug に出ない。
    assert!(!format!("{v:?}").contains("合成"));
}

fn candidate(model: &str, pairs: &[(i64, &str)], overlap: Overlap) -> Candidate {
    let _ = model;
    Candidate {
        result_id: "r".repeat(64),
        overlap,
        sent: pairs.iter().map(|(i, g)| (*i, g.repeat(64))).collect(),
    }
}

#[test]
fn identity_keys_follow_the_sent_set_not_the_group_urls_or_order() {
    let a = candidate("m", &[(1, "a"), (2, "b"), (3, "c")], Overlap::Disjoint);
    let reordered = candidate("m", &[(3, "c"), (1, "a"), (2, "b")], Overlap::Exact);
    // 順位・グループの結果 ID・Overlap が違っても同じ送信集合・世代なら同じ鍵（再送しない）。
    assert!(a == reordered);
    assert_eq!(a.input_key("m"), reordered.input_key("m"));
    assert_eq!(a.key("m"), reordered.key("m"));
    // モデル・世代・写真が変われば別の鍵。
    assert_ne!(a.input_key("m"), a.input_key("n"));
    assert_ne!(a.key("m").processor_version, a.key("n").processor_version);
    let moved = candidate("m", &[(1, "a"), (2, "z"), (3, "c")], Overlap::Disjoint);
    assert!(a != moved);
    assert_ne!(a.input_key("m"), moved.input_key("m"));
    // 世代だけが違う job は同じ対象（古い job は enqueue で無効になる）。
    assert_eq!(a.key("m").target_key, moved.key("m").target_key);
    assert_ne!(a.key("m").input_hash, moved.key("m").input_hash);
    let subset = candidate("m", &[(1, "a"), (2, "b")], Overlap::Disjoint);
    assert_ne!(a.key("m").target_key, subset.key("m").target_key);
    assert!(a.sendable() && !candidate("m", &[(1, "a")], Overlap::Exact).sendable());
    assert!(!format!("{a:?}").contains('r'));
}

fn image(n: u8) -> PreparedImage {
    PreparedImage::encode_jpeg_rgb(&vec![n; 16 * 8 * 3], 16, 8, 85, ImageProfile::Embedding)
        .unwrap()
}
fn images(ids: &[i64]) -> Vec<SendImage> {
    ids.iter()
        .map(|id| SendImage {
            photo_id: *id,
            source_sha256: "d".repeat(64),
            image: image(*id as u8),
        })
        .collect()
}

#[test]
fn plan_requires_the_exact_sent_order_and_builds_the_python_shaped_request() {
    let c = candidate("m", &[(5, "a"), (2, "b"), (9, "c")], Overlap::Disjoint);
    let plan = Plan::new(c.clone(), "judge-model", images(&[5, 2, 9])).unwrap();
    let body = plan.request().body_json_for_test();
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], PROMPT);
    // 画像は順位順。番号は 1 始まりのラベル（photo_judge は従来どおり英語の "Image n"）。
    assert_eq!(
        parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .skip(1)
            .collect::<Vec<_>>(),
        ["Image 1", "Image 2", "Image 3"]
    );
    assert_eq!(
        parts
            .iter()
            .filter(|p| p.get("inlineData").is_some())
            .count(),
        3
    );
    assert_eq!(body["generationConfig"]["responseJsonSchema"], schema());
    assert!(body["generationConfig"].get("responseSchema").is_none());
    assert_eq!(body["generationConfig"]["temperature"], 0.1);
    assert_eq!(plan.sent_ids(), [5, 2, 9]);
    assert_eq!(plan.key(), c.key("judge-model"));
    for wrong in [vec![2, 5, 9], vec![5, 2], vec![5, 2, 9, 1]] {
        assert_eq!(
            Plan::new(c.clone(), "judge-model", images(&wrong)).unwrap_err(),
            DomainError::InvalidInput
        );
    }
    let mut bad_hash = images(&[5, 2, 9]);
    bad_hash[0].source_sha256 = "not-a-hash".into();
    assert!(Plan::new(c.clone(), "judge-model", bad_hash).is_err());
    assert!(Plan::new(c.clone(), "", images(&[5, 2, 9])).is_err());
    assert!(Plan::new(c, "bad\nmodel", images(&[5, 2, 9])).is_err());
    assert!(!format!("{plan:?}").contains("judge"));
}

#[test]
fn processor_version_separates_models_and_names_the_wire_contract() {
    assert_ne!(processor_version("a"), processor_version("b"));
    assert!(valid_hash(&processor_version("a")));
    assert_eq!(IMAGE_PREPROCESS, "embed-rgb-768-q82-v1");
    assert_eq!(MAX_SEND, 6);
}

#[test]
fn migration_is_additive_idempotent_and_reports_exactly_one_new_table() {
    let dir = crate::testutil::TempDir::new("judge-migrate");
    let mut conn = rusqlite::Connection::open(dir.path().join("catalog.db")).unwrap();
    crate::schema::migrate(&mut conn).unwrap();
    crate::ai::migrate_active(&mut conn, false, false, true, false).unwrap();
    // 許可の無い用途では計画にも作成にも入らない。
    assert!(
        !crate::ai::pending_active(&conn, false, false, true, false)
            .unwrap()
            .iter()
            .any(|t| t == "ai_scene_judgments")
    );
    assert_eq!(
        crate::ai::pending_active(&conn, false, false, true, true).unwrap(),
        ["ai_scene_judgments"]
    );
    assert_eq!(
        crate::ai::migrate_active(&mut conn, false, false, true, true).unwrap(),
        ["ai_scene_judgments"]
    );
    assert!(
        crate::ai::migrate_active(&mut conn, false, false, true, true)
            .unwrap()
            .is_empty()
    );
    // 履歴は主キーで一意。同じ入力の回答を別の内容で上書きする書き方は DB が拒む。
    let insert = |key: &str| {
        conn.execute(
            "INSERT INTO ai_scene_judgments VALUES(?1,'{}','{}',1,'2026-01-01T00:00:00Z',?2)",
            rusqlite::params![key, "e".repeat(64)],
        )
    };
    insert(&"a".repeat(64)).unwrap();
    assert!(insert(&"a".repeat(64)).is_err());
    assert!(insert("short").is_err());
}
