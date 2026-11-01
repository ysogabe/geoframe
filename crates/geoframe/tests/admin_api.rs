//! `GET /api/admin/status`・`GET /api/admin/photos`（T1.4）の結合テスト。合成データだけを使う。
//!
//! Python 版の出力（Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_admin_fixture.py` は P5b で削除した。再生成はできない）と、`serde_json::Value`
//! として完全一致することを確かめる。Python の出力と比べない値は `disk`（機体のディスクと実行時刻に
//! 依存）と `server_time` で、これは形と数え方を別に検査する。

mod common;

use std::time::Duration;

use common::{
    TempDir, admin_fixture, build_admin_catalog, get, request, spawn_state, spawn_with_env,
};
use geoframe::db::Catalog;
use serde_json::{Value, json};

const JSON_UTF8: &str = "application/json; charset=utf-8";

fn strip_unstable(mut body: Value) -> Value {
    if let Some(map) = body.as_object_mut() {
        map.remove("disk");
        map.remove("server_time");
    }
    body
}

/// Python 版に無い追加分を除く: `db.filters`、`items[].thumb_url`、`noloc`・`suggest` 以外のフィルタの `hint`。
fn python_shape(mut body: Value) -> Value {
    if let Some(db) = body.get_mut("db").and_then(Value::as_object_mut) {
        db.remove("filters");
        // Rust 固有の項目（Python には無い）
        if let Some(quality) = db.get_mut("quality").and_then(Value::as_object_mut) {
            quality.remove("curation_pending");
        }
    }
    if let Some(items) = body.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(map) = item.as_object_mut() {
                map.remove("thumb_url");
            }
        }
    }
    let hinted = matches!(body["filter"].as_str(), Some("noloc" | "suggest"));
    if !hinted && let Some(items) = body.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(map) = item.as_object_mut() {
                map.remove("hint");
            }
        }
    }
    body
}

struct Server {
    _dir: TempDir,
    writer: rusqlite::Connection,
    addr: std::net::SocketAddr,
}

impl Server {
    async fn start(env: &[(&str, &str)]) -> Self {
        let dir = TempDir::new("admin");
        let writer = build_admin_catalog(dir.path());
        let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        let addr = spawn_with_env(dir.path(), env, catalog).await;
        Self {
            _dir: dir,
            writer,
            addr,
        }
    }

    /// Python 版にも無い追加分（`db.filters`・全フィルタのヒント）を除かない、そのままの応答。
    async fn raw(&self, target: &str) -> (u16, Value) {
        let response = get(self.addr, target).await;
        (response.status, strip_unstable(response.json()))
    }

    /// Python 版のフィクスチャと比べられる形の応答（追加分を除く）。
    async fn json(&self, target: &str) -> (u16, Value) {
        let (status, body) = self.raw(target).await;
        (status, python_shape(body))
    }

    /// フィクスチャの「target・status・body」の並びを順に流して比べる。
    async fn check(&self, cases: &Value) {
        let cases = cases.as_array().expect("配列");
        assert!(!cases.is_empty());
        for case in cases {
            let target = case["target"].as_str().unwrap();
            let (status, body) = self.json(target).await;
            let shown: String = target.chars().take(80).collect();
            assert_eq!(status, case["status"].as_u64().unwrap() as u16, "{shown}");
            assert_eq!(body, case["body"], "{shown}");
        }
    }
}

#[tokio::test]
async fn photos_match_python_for_every_query() {
    let fixture = admin_fixture();
    let server = Server::start(&[]).await;
    let cases = &fixture["photos"];
    // 取りこぼしが無いよう、フィクスチャの規模を固定しておく（増やしたら更新する）
    assert!(cases.as_array().unwrap().len() >= 80);
    server.check(cases).await;
}

#[tokio::test]
async fn status_matches_python_and_the_prefix_forms_return_the_same() {
    let fixture = admin_fixture();
    let server = Server::start(&[]).await;
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(body, fixture["status"]["gemini"]);
    // 生のパスの前方一致（`/api/admin/status` で始まれば当たる）。クエリは無視する
    server.check(&fixture["status_targets"]).await;
}

/// 追加のキーを除いた応答。既存のキーの意味・値は Python 版のフィクスチャのまま。
fn without_ai_ops(mut body: Value) -> Value {
    let map = body.as_object_mut().unwrap();
    for key in [
        "ai_config_invalid",
        "ai_jobs",
        "ai_pauses",
        "ai_task_blocks",
        "ai_waits",
        "ai_scenes",
    ] {
        map.remove(key);
    }
    body
}

#[tokio::test]
async fn an_invalid_ai_config_shows_only_a_fixed_code_and_changes_no_other_field() {
    let fixture = admin_fixture();
    let secrets = TempDir::new("admin-ai-config");
    let bad = secrets.path().join("ai-secret-name.json");
    std::fs::write(&bad, r#"{"version": "sentinel-value-93ab"}"#).unwrap();
    let server = Server::start(&[("GEOFRAME_AI_CONFIG", bad.to_str().unwrap())]).await;
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["ai_config_invalid"],
        json!({"code": "bad_value", "disabled_tasks": 5})
    );
    // 追加したのはこの 1 つだけ。既存のフィールドは Python 版と同じ
    assert_eq!(
        body.as_object().unwrap().len(),
        fixture["status"]["gemini"].as_object().unwrap().len() + 1
    );
    assert_eq!(without_ai_ops(body.clone()), fixture["status"]["gemini"]);
    let text = body.to_string();
    for secret in ["sentinel-value-93ab", "ai-secret-name"] {
        assert!(!text.contains(secret), "{secret}");
    }
    // 旧設定の環境変数が不正でも同じ（固定のコード）
    let server = Server::start(&[("GEOFRAME_AI_POLL_S", "-1")]).await;
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body["ai_config_invalid"]["code"], "legacy_env");
    // 設定が正常・未指定なら、何も足さない
    let server = Server::start(&[]).await;
    let (_, body) = server.json("/api/admin/status").await;
    assert!(body.get("ai_config_invalid").is_none());
}

#[tokio::test]
async fn ai_jobs_and_pauses_appear_as_counts_and_fixed_reasons_when_the_tables_have_rows() {
    let fixture = admin_fixture();
    let mut server = Server::start(&[]).await;
    // AI の表があっても行が無ければ何も足さない
    geoframe::ai::migrate(&mut server.writer).unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body["ai_scenes"]["state"], "no_index");
    assert_eq!(without_ai_ops(body), fixture["status"]["gemini"]);

    server
        .writer
        .execute_batch(
            "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,
                                 last_error_code,created_at,updated_at) VALUES
               ('photo_judge','SECRET-T1','SECRET-H1','SECRET-P','deferred','clock_rewound',1,1),
               ('photo_judge','SECRET-T2','SECRET-H2','SECRET-P','pending','',1,1),
               ('location_clue','SECRET-T3','SECRET-H3','SECRET-P','failed','interrupted',1,1);
             INSERT INTO ai_provider_pauses(scope,subject,task,reason,created_at)
               VALUES ('provider','SECRET-SUBJECT','image_embedding','unauthorized',1)",
        )
        .unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(without_ai_ops(body.clone()), fixture["status"]["gemini"]);
    assert_eq!(
        body["ai_jobs"],
        json!({
            "total": 3,
            "by_state": {"deferred": 1, "failed": 1, "pending": 1},
            "by_kind": {
                "location_clue": {"failed": 1},
                "photo_judge": {"deferred": 1, "pending": 1}
            },
            "reasons": {
                "deferred": {"clock_rewound": 1},
                "failed": {"interrupted": 1}
            }
        })
    );
    assert_eq!(
        body["ai_pauses"],
        json!([{"task": "image_embedding", "scope": "provider", "reason": "unauthorized"}])
    );
    let text = body.to_string();
    assert!(!text.contains("SECRET"), "{text}");
}

/// Provider の停止は、停止を起こした用途だけでなく、同じ停止にかかる全用途を `ai_task_blocks` に出す
/// （用途名・範囲・固定の理由・待ちの件数だけ）。`ai_pauses` の形は変わらない。停止が無ければキーを足さない。
#[tokio::test]
async fn a_provider_pause_lists_every_task_behind_it_with_waiting_counts_only() {
    use geoframe::ai::pause::{PauseReason, pause};
    use geoframe::ai::settings::{AiConfig, TaskKind};

    let fixture = admin_fixture();
    let cfg_dir = TempDir::new("admin-ai-blocks");
    let path = cfg_dir.path().join("ai-secret-blocks.json");
    std::fs::write(
        &path,
        r#"{"version":1,
            "tasks":{
              "photo_judge":{"provider":"cloud","model":"secret-model-j","allow_cloud":true,
                "daily_request_limit":9,"credential_path":"/secret/cred-q7.json",
                "project":"secret-project-q7","region":"secret-region-q7"},
              "location_suggest":{"provider":"cloud","model":"secret-model-s","allow_cloud":true,
                "daily_request_limit":9,"credential_path":"/secret/cred-q7.json",
                "project":"secret-project-q7","region":"secret-region-q7"}}}"#,
    )
    .unwrap();
    let path = path.to_str().unwrap().to_string();
    let config = AiConfig::load(&|k| (k == "GEOFRAME_AI_CONFIG").then(|| path.clone()));
    assert_eq!(config.invalid(), None);
    let key = config
        .settings()
        .unwrap()
        .provider_pause_key(TaskKind::LocationSuggest)
        .unwrap();

    let mut server = Server::start(&[("GEOFRAME_AI_CONFIG", &path)]).await;
    geoframe::ai::migrate(&mut server.writer).unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert!(body.get("ai_task_blocks").is_none(), "{body}");

    server
        .writer
        .execute_batch(
            "INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,state,
                                 last_error_code,created_at,updated_at) VALUES
               ('photo_judge','SECRET-T1','SECRET-H1','SECRET-P','pending','',1,1),
               ('photo_judge','SECRET-T2','SECRET-H2','SECRET-P','deferred','provider_paused',1,1),
               ('location_geocode','SECRET-T3','SECRET-H3','SECRET-P','deferred','',1,1),
               ('location_suggest','SECRET-T4','SECRET-H4','SECRET-P','failed','interrupted',1,1)",
        )
        .unwrap();
    pause(
        &mut server.writer,
        TaskKind::LocationSuggest,
        &key,
        PauseReason::Forbidden,
        1,
    )
    .unwrap();
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["ai_task_blocks"],
        json!([
            {"task": "photo_judge", "scope": "provider", "reason": "forbidden", "waiting": 2},
            {"task": "location_suggest", "scope": "provider", "reason": "forbidden", "waiting": 1},
        ])
    );
    assert_eq!(
        body["ai_pauses"],
        json!([{"task": "location_suggest", "scope": "provider", "reason": "forbidden"}])
    );
    assert_eq!(without_ai_ops(body.clone()), fixture["status"]["gemini"]);
    let text = body.to_string();
    for secret in ["SECRET", "secret-", "cred-q7", "ai-secret-blocks"] {
        assert!(!text.contains(secret), "{secret}\n{text}");
    }
}

/// 有効な共有の一時待機だけが、今の設定の用途に結び直されて固定の語と UTC の期限で出る。期限切れ・古い設定の待ちは
/// 出ず、接続先・モデル・ハッシュは返らない。待ちが無ければキーを足さない。
#[tokio::test]
async fn live_provider_waits_appear_with_task_scope_reason_and_utc_deadline_only() {
    use geoframe::ai::settings::{AiConfig, TaskKind};
    use geoframe::ai::waits::{WaitKey, WaitReason, set};

    let fixture = admin_fixture();
    let cfg_dir = TempDir::new("admin-ai-waits");
    let path = cfg_dir.path().join("ai.json");
    std::fs::write(
        &path,
        r#"{"version":1,"local_base_url":"http://192.0.2.77:8100",
            "tasks":{"location_clue":{"provider":"local","model":"secret-model-q7"}}}"#,
    )
    .unwrap();
    let path = path.to_str().unwrap().to_string();
    let config = AiConfig::load(&|k| (k == "GEOFRAME_AI_CONFIG").then(|| path.clone()));
    let provider = config
        .settings()
        .unwrap()
        .provider_fingerprint(TaskKind::LocationClue)
        .unwrap();

    let mut server = Server::start(&[("GEOFRAME_AI_CONFIG", &path)]).await;
    geoframe::ai::migrate(&mut server.writer).unwrap();
    // 待ちが無い（表だけある）なら何も足さない
    let (_, body) = server.json("/api/admin/status").await;
    assert!(body.get("ai_waits").is_none());

    let now = geoframe::ai::jobs::unix_now();
    let until = now + 1_800;
    set(
        &mut server.writer,
        &WaitKey {
            provider,
            service: None,
        },
        WaitReason::Leased,
        until,
        now,
    )
    .unwrap();
    // 古い設定の待ち・期限切れの待ち（行は残っていても出ない）
    set(
        &mut server.writer,
        &WaitKey {
            provider: "stale-provider-hash".into(),
            service: None,
        },
        WaitReason::Unreachable,
        until,
        now,
    )
    .unwrap();
    server
        .writer
        .execute(
            "INSERT INTO ai_provider_waits(provider, service, reason, until, updated_at)
             VALUES ('expired-provider-hash', '', 'busy', ?1, ?1)",
            [now - 10],
        )
        .unwrap();
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["ai_waits"],
        json!({"items": [{
            "task": "location_clue", "scope": "entrance", "reason": "leased",
            "until": geoframe::ai::waits::utc_iso(until)
        }]})
    );
    assert_eq!(without_ai_ops(body.clone()), fixture["status"]["gemini"]);
    let text = body.to_string();
    for secret in [
        "192.0.2.77",
        "secret-model-q7",
        "stale-provider-hash",
        "expired-provider-hash",
    ] {
        assert!(!text.contains(secret), "{secret}");
    }
    // 秒まで・Z つきの UTC
    let shown = body["ai_waits"]["items"][0]["until"].as_str().unwrap();
    assert!(
        shown.len() == 20 && shown.ends_with('Z') && shown.as_bytes()[10] == b'T',
        "{shown}"
    );
}

#[tokio::test]
async fn status_counts_the_models_of_the_configured_backend() {
    let fixture = admin_fixture();
    let server = Server::start(&[("GEOFRAME_EMBED_BACKEND", " Local ")]).await;
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body, fixture["status"]["local"]);
    // gemini と local の違いが、実際に件数に出ている
    assert_ne!(
        fixture["status"]["local"]["db"]["ai"],
        fixture["status"]["gemini"]["db"]["ai"]
    );
    // 不正な値は gemini に落ちる（現行と同じ。起動は続ける）
    let server = Server::start(&[("GEOFRAME_EMBED_BACKEND", "other")]).await;
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body, fixture["status"]["gemini"]);
}

#[tokio::test]
async fn the_inference_window_setting_changes_labels_and_day_groups() {
    let fixture = admin_fixture();
    for (name, variant) in fixture["infer_variants"].as_object().unwrap() {
        let env: Vec<(&str, &str)> = variant["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let server = Server::start(&env).await;
        let (_, body) = server.json("/api/admin/status").await;
        assert_eq!(body, variant["status"], "{name}");
        server.check(&variant["photos"]).await;
    }
}

#[tokio::test]
async fn the_last_decision_state_comes_from_the_settings_table() {
    let fixture = admin_fixture();
    let server = Server::start(&[]).await;
    // 前回の処理時の状態が無い・壊れている: 空文字
    server.writer.execute_batch("DELETE FROM settings").unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body, fixture["status"]["no_settings"]);
    server
        .writer
        .execute(
            "INSERT INTO settings(key, value) VALUES('photo_loc_status', '{broken')",
            [],
        )
        .unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body, fixture["status"]["broken_settings"]);
    // オブジェクトでない JSON は、Python は 500 になるが Rust は空として扱う
    server
        .writer
        .execute(
            "UPDATE settings SET value='[1]' WHERE key='photo_loc_status'",
            [],
        )
        .unwrap();
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(body["db"]["loc"]["decision"], "");
    // 設定の値がそのまま出る（文字列でなくても）
    server
        .writer
        .execute(
            r#"UPDATE settings SET value='{"decision":"unavailable","at":123}' WHERE key='photo_loc_status'"#,
            [],
        )
        .unwrap();
    let (_, body) = server.json("/api/admin/status").await;
    assert_eq!(body["db"]["loc"]["decision"], "unavailable");
    assert_eq!(body["db"]["loc"]["last_at"], 123);
}

#[tokio::test]
async fn empty_tables_work() {
    let fixture = admin_fixture();
    let server = Server::start(&[]).await;
    let tables: Vec<String> = server
        .writer
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in tables {
        server
            .writer
            .execute_batch(&format!("DELETE FROM {table}"))
            .unwrap();
    }
    let (status, body) = server.json("/api/admin/status").await;
    assert_eq!(status, 200);
    assert_eq!(body, fixture["empty_status"]);
    server.check(&fixture["empty_photos"]).await;
    // ディスク使用量は DB の中身によらず出る
    let response = get(server.addr, "/api/admin/status").await;
    assert!(response.json()["disk"]["total_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn per_is_capped_at_200_and_pages_split_there() {
    let dir = TempDir::new("cap");
    let writer = build_admin_catalog(dir.path());
    writer
        .execute_batch(
            "DELETE FROM photo_loc_suggestions; DELETE FROM photo_loc_clues;
             DELETE FROM photo_ai_group_members; DELETE FROM photo_ai_groups;
             DELETE FROM photo_ai_features; DELETE FROM photo_quality_features;
             DELETE FROM photo_quality; DELETE FROM photos;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<205)
             INSERT INTO photos(id, source, name, taken_at)
               SELECT i, 'local', 'IMG_' || i, printf('2025-01-01 %02d:%02d', i / 60, i % 60) FROM n;",
        )
        .unwrap();
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog).await;
    for (query, per, page, items, ids) in [
        ("per=1000", 200, 1, 200, (1, 200)),
        ("per=200&page=2", 200, 2, 5, (201, 205)),
        ("per=201&page=9", 200, 2, 5, (201, 205)),
        ("per=0&page=205", 1, 205, 1, (205, 205)),
    ] {
        let response = get(addr, &format!("/api/admin/photos?{query}")).await;
        let body = response.json();
        assert_eq!(body["per"], per, "{query}");
        assert_eq!(body["page"], page, "{query}");
        assert_eq!(body["total"], 205, "{query}");
        let listed: Vec<i64> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_i64().unwrap())
            .collect();
        assert_eq!(listed.len(), items, "{query}");
        assert_eq!((listed[0], *listed.last().unwrap()), ids, "{query}");
    }
    // 位置なしの一覧（ヒントを付ける）でも、錨が無ければ `hint` のキーは付かない
    let body = get(addr, "/api/admin/photos?filter=noloc&per=2")
        .await
        .json();
    assert_eq!(body["total"], 205);
    assert!(body["items"][0].get("hint").is_none(), "{body}");
    assert!(body["items"][0].get("suggest").is_none(), "{body}");
}

#[tokio::test]
async fn the_order_is_time_then_blank_then_id() {
    let dir = TempDir::new("order");
    let writer = build_admin_catalog(dir.path());
    writer
        .execute_batch(
            "DELETE FROM photo_loc_suggestions; DELETE FROM photo_loc_clues;
             DELETE FROM photo_ai_group_members; DELETE FROM photo_ai_groups;
             DELETE FROM photo_ai_features; DELETE FROM photo_quality_features;
             DELETE FROM photo_quality; DELETE FROM photos;
             INSERT INTO photos(id, source, name, taken_at) VALUES
               (9, 'local', 'a', '2025-02-01 10:00'), (8, 'local', 'b', '2025-02-01 10:00'),
               (7, 'local', 'c', ''), (3, 'local', 'd', NULL), (5, 'local', 'e', ''),
               (4, 'local', 'f', NULL), (1, 'local', 'g', '2025-01-01 09:00');",
        )
        .unwrap();
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog).await;
    let body = get(addr, "/api/admin/photos").await.json();
    let ids: Vec<i64> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    // 時刻順（同じ時刻は id 順）、そのあと時刻なし（NULL が先、空文字が後、それぞれ id 順）
    assert_eq!(ids, [1, 8, 9, 3, 4, 5, 7]);
}

#[tokio::test]
async fn responses_carry_the_json_headers() {
    let server = Server::start(&[]).await;
    for target in [
        "/api/admin/status",
        "/api/admin/photos",
        "/api/admin/photos?page=x",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.header("content-type"), Some(JSON_UTF8), "{target}");
        assert_eq!(
            response.header("cache-control"),
            Some("no-store"),
            "{target}"
        );
        // 日本語はエスケープしないままの UTF-8
        assert!(!response.text().contains("\\u"), "{target}");
    }
    let body = get(server.addr, "/api/admin/photos?page=x").await;
    assert_eq!(body.status, 400);
    assert_eq!(body.json(), json!({"ok": false, "error": "bad_params"}));
    // 日本語（地名）がそのまま出る
    let listing = get(server.addr, "/api/admin/photos?per=3").await;
    assert!(listing.text().contains("架空県"), "{}", listing.text());
}

#[tokio::test]
async fn only_the_three_reads_are_implemented() {
    let server = Server::start(&[]).await;
    // 更新ルートへの GET と、残りの `/api/admin/` は、現行でも静的のホワイトリストに当たらず 403
    // （T3.3 で P1 の未実装の 503 から揃えた）
    for target in [
        "/api/admin/setloc",
        "/api/admin/setflag",
        "/api/admin/rotate",
        "/api/admin/",
        "/api/admin/stat",
        "/api/admin/Status",
        "/api/admin/Geocode?q=x",
        // 生のパスで見るので、エンコードした形は当たらない（現行も 403）
        "/api/admin/%73tatus",
        "/api/admin/%67eocode?q=x",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
    }
    // 地名検索。`q` は 1 文字以下なので通信せずに空（このテストは外へ出ない。通信のある試験は
    // `geocode/tests.rs` のループバックのスタブ）
    for target in [
        "/api/admin/geocode",
        "/api/admin/geocode?q=x",
        "/api/admin/geocode?q=",
        "/api/admin/geocodeXYZ?q=%20x%20",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 200, "{target}");
        assert_eq!(
            response.json(),
            json!({"ok": true, "items": []}),
            "{target}"
        );
    }
    // GET だけ。HEAD は `/api/` の接頭辞なので現行と同じく 403、POST は更新ルートではないので 403
    for method in ["HEAD", "POST"] {
        for target in ["/api/admin/status", "/api/admin/photos"] {
            let response = request(server.addr, method, target, &[("Host", "localhost")]).await;
            assert_eq!(response.status, 403, "{method} {target}");
        }
    }
    // 先頭の `//` は畳んでから判定する（現行の `http.server` と同じ。`//api/v1/photos` と
    // 未実装の接頭辞も同じ）
    for (target, status) in [
        ("//api/admin/status", 200),
        ("///api/admin/photos?per=1", 200),
        ("//api/v1/photos", 200),
        ("//api/admin/geocode?q=x", 200),
        ("//api/ai/status", 200),
        // 地図タイル（T3.4）。範囲外は上流へ行かずに 404（`//` は畳んだうえで `/tiles/` に当たる）
        ("//tiles/osm/1/2/0.png", 404),
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, status, "{target}");
    }
    // `/api/admin/` で始まらない形は当たらず、静的配信のホワイトリスト外で 403
    for target in ["/%61pi/admin/status", "/API/admin/status"] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
    }
}

#[tokio::test]
async fn a_broken_db_is_503_with_fixed_bodies() {
    let server = Server::start(&[]).await;
    // 起動後に DB の中身が壊れた・入れ替わった状況
    server
        .writer
        .execute_batch("DROP TABLE photo_loc_suggestions")
        .unwrap();
    // 位置なしの行があれば候補の表を引くので、`photos` も 503（位置なしの行が無い一覧は引かない）
    for target in [
        "/api/admin/status",
        "/api/admin/photos?filter=suggest",
        "/api/admin/photos",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 503, "{target}");
        assert_eq!(response.header("content-type"), Some(JSON_UTF8));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        let body = response.json();
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()));
        assert_eq!(body.as_object().unwrap().len(), 2);
        // 内部のエラー文字列（SQLite のメッセージ・テーブル名・パス）を返さない
        let text = response.text().to_lowercase();
        for secret in [
            "photo_loc",
            "no such table",
            "sqlite",
            "catalog.db",
            "geoframe-test",
        ] {
            assert!(!text.contains(secret), "{target}: {text}");
        }
    }
    // 壊れていない側は読める（不正なクエリは DB を読む前に 400）
    assert_eq!(
        get(server.addr, "/api/admin/photos?page=x").await.status,
        400
    );
    assert_eq!(
        get(server.addr, "/api/admin/photos?filter=fav")
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn waiting_too_long_for_a_connection_is_503() {
    let dir = TempDir::new("admin-queue");
    let _writer = build_admin_catalog(dir.path());
    let catalog = Catalog::open_with_queue_timeout(
        &dir.path().join("catalog.db"),
        1,
        Duration::from_millis(100),
    )
    .unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog.clone()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let holder = tokio::spawn({
        let catalog = catalog.clone();
        async move {
            catalog
                .read(move |_| {
                    let _ = started_tx.send(());
                    std::thread::sleep(Duration::from_millis(600));
                    Ok(())
                })
                .await
        }
    });
    started_rx.await.unwrap();
    for target in ["/api/admin/status", "/api/admin/photos"] {
        let response = get(addr, target).await;
        assert_eq!(response.status, 503, "{target}");
        assert_eq!(response.json()["ok"], false);
    }
    holder.await.unwrap().unwrap();
    assert_eq!(get(addr, "/api/admin/status").await.status, 200);
    assert_eq!(get(addr, "/api/admin/photos").await.status, 200);
}

// ---- disk / server_time ----

fn assert_local_timestamp(text: &str) {
    let b = text.as_bytes();
    assert_eq!(b.len(), 19, "{text}");
    for (i, c) in b.iter().enumerate() {
        match i {
            4 | 7 => assert_eq!(*c, b'-', "{text}"),
            10 => assert_eq!(*c, b' ', "{text}"),
            13 | 16 => assert_eq!(*c, b':', "{text}"),
            _ => assert!(c.is_ascii_digit(), "{text}"),
        }
    }
}

#[tokio::test]
async fn disk_and_server_time_have_the_python_shape() {
    let dir = TempDir::new("disk");
    let _writer = build_admin_catalog(dir.path());
    std::fs::create_dir_all(dir.path().join("tile_cache/z1")).unwrap();
    std::fs::write(dir.path().join("derivatives/a.jpg"), vec![0u8; 100]).unwrap();
    std::fs::write(dir.path().join("tile_cache/z1/t.png"), vec![0u8; 7]).unwrap();
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let addr = spawn_with_env(
        dir.path(),
        &[("GEOFRAME_TILE_CACHE_MAX_MB", "1234")],
        catalog,
    )
    .await;
    let body = get(addr, "/api/admin/status").await.json();

    assert_local_timestamp(body["server_time"].as_str().unwrap());
    let disk = body["disk"].as_object().unwrap();
    let mut keys: Vec<&str> = disk.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "dirs",
            "free_bytes",
            "measured_at",
            "tile_cache_max_mb",
            "total_bytes",
            "used_bytes",
            "used_pct"
        ]
    );
    let total = disk["total_bytes"].as_u64().unwrap();
    assert!(total > 0 && disk["used_bytes"].as_u64().unwrap() <= total);
    assert!(disk["free_bytes"].as_u64().unwrap() <= total);
    let pct = disk["used_pct"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&pct), "{pct}");
    // 小数1桁に丸めてある
    assert!(((pct * 10.0).round() - pct * 10.0).abs() < 1e-9, "{pct}");
    assert_local_timestamp(disk["measured_at"].as_str().unwrap());
    assert_eq!(disk["tile_cache_max_mb"], 1234);
    // derivatives/ には、合成DBのフィクスチャが作る空ファイル1つと、このテストの100バイト
    assert_eq!(
        disk["dirs"]["derivatives"],
        json!({"files": 2, "bytes": 100, "exists": true})
    );
    assert_eq!(
        disk["dirs"]["tile_cache"],
        json!({"files": 1, "bytes": 7, "exists": true})
    );

    // 60秒キャッシュ: 直後に足したファイルは、次の応答にはまだ出ない（admin.js は30秒ごとに見る）
    std::fs::write(dir.path().join("derivatives/b.jpg"), vec![0u8; 50]).unwrap();
    let again = get(addr, "/api/admin/status").await.json();
    assert_eq!(again["disk"], body["disk"]);
}

#[test]
fn directory_usage_counts_like_os_walk() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("walk");
    let root = dir.path();
    // 無いディレクトリ
    let missing = geoframe::admin::disk_info(root, 16384).unwrap();
    assert_eq!(
        missing["dirs"]["derivatives"],
        json!({"files": 0, "bytes": 0, "exists": false})
    );
    assert_eq!(missing["tile_cache_max_mb"], 16384);

    let deriv = root.join("derivatives");
    std::fs::create_dir_all(deriv.join("a/b")).unwrap();
    std::fs::create_dir_all(deriv.join("empty")).unwrap();
    std::fs::write(deriv.join("top.jpg"), vec![0u8; 10]).unwrap();
    std::fs::write(deriv.join("a/mid.jpg"), vec![0u8; 20]).unwrap();
    std::fs::write(deriv.join("a/b/deep.jpg"), vec![0u8; 30]).unwrap();
    // ファイルへのリンクは先の大きさで数える。ディレクトリへのリンクは辿らず数えない。壊れたリンクは無視
    symlink(deriv.join("a/mid.jpg"), deriv.join("link-file")).unwrap();
    symlink(deriv.join("a"), deriv.join("link-dir")).unwrap();
    symlink(deriv.join("nowhere"), deriv.join("broken")).unwrap();
    let outside = root.join("outside.bin");
    std::fs::write(&outside, vec![0u8; 1000]).unwrap();
    symlink(&outside, deriv.join("link-out")).unwrap();

    let info = geoframe::admin::disk_info(root, 1).unwrap();
    assert_eq!(
        info["dirs"]["derivatives"],
        json!({"files": 5, "bytes": 10 + 20 + 30 + 20 + 1000, "exists": true})
    );
    // ファイルのパスを渡しても（ディレクトリでなくても）存在はする
    std::fs::write(root.join("tile_cache"), b"x").unwrap();
    let info = geoframe::admin::disk_info(root, 1).unwrap();
    assert_eq!(
        info["dirs"]["tile_cache"],
        json!({"files": 0, "bytes": 0, "exists": true})
    );
    // データ置き場が無ければ失敗（Python も OSError で 500）
    assert!(geoframe::admin::disk_info(&root.join("none"), 1).is_err());
}

/// T3.7 ⑤: 大量削除の保留があるときだけ `sync.mass_delete_hold` が付く。保留が無い・記憶が無い（取り込み無効）ときは
/// キーが無く、応答は Python と同じ（フィクスチャと完全一致）。
#[tokio::test]
async fn the_mass_delete_hold_appears_only_while_there_is_one() {
    use geoframe::drive::{HoldReason, MassDeleteBoard};
    use std::sync::Arc;

    let fixture = admin_fixture();
    let dir = TempDir::new("admin-hold");
    let _writer = build_admin_catalog(dir.path());
    let board = Arc::new(MassDeleteBoard::default());
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let shared = Arc::clone(&board);
    let addr = spawn_state(dir.path(), &[], catalog, move |s| {
        s.with_mass_delete_board(shared)
    })
    .await;
    let status = || async {
        let response = get(addr, "/api/admin/status").await;
        assert_eq!(response.status, 200);
        python_shape(strip_unstable(response.json()))
    };
    // 記憶はあるが、まだ何も保留していない（サイクル前も、保留なしのサイクルのあとも同じ）: Python と同じ応答
    assert_eq!(status().await, fixture["status"]["gemini"]);
    board.record(None, "2026-10-25 10:00:00");
    assert_eq!(status().await, fixture["status"]["gemini"]);

    // 保留中: `sync` の末尾にキーが付き、ほかは変わらない
    board.record(Some((99, HoldReason::Ratio, 100)), "2026-10-25 10:05:00");
    let body = status().await;
    assert_eq!(
        body["sync"]["mass_delete_hold"],
        json!({
            "held": 99,
            "reason": "ratio",
            "drive_rows": 100,
            "since": "2026-10-25 10:05:00",
            "checked_at": "2026-10-25 10:05:00",
        })
    );
    let mut without = body.clone();
    without["sync"]
        .as_object_mut()
        .unwrap()
        .remove("mass_delete_hold");
    assert_eq!(without, fixture["status"]["gemini"]);
    // キーの並びは `ready, pending, total, mass_delete_hold`
    let keys: Vec<&String> = body["sync"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["ready", "pending", "total", "mass_delete_hold"]);

    // 一覧が空の保留は `reason` が変わる
    board.record(
        Some((100, HoldReason::EmptyListing, 100)),
        "2026-10-25 10:10:00",
    );
    let body = status().await;
    assert_eq!(body["sync"]["mass_delete_hold"]["reason"], "empty_listing");
    assert_eq!(
        body["sync"]["mass_delete_hold"]["since"],
        "2026-10-25 10:05:00"
    );

    // 解除されたあとのサイクルで消える
    board.record(None, "2026-10-25 10:20:00");
    assert_eq!(status().await, fixture["status"]["gemini"]);
}

/// 取り込みが無効の `serve`（記憶が無い）は、キーを出さない。
#[tokio::test]
async fn without_the_ingest_there_is_no_hold_key() {
    let server = Server::start(&[]).await;
    let (_, body) = server.json("/api/admin/status").await;
    assert!(body["sync"].get("mass_delete_hold").is_none());
}

const FILTER_NAMES: [&str; 7] = [
    "all", "noloc", "suggest", "inferred", "manual", "fav", "hidden",
];

#[tokio::test]
async fn status_filters_match_each_filtered_total() {
    let server = Server::start(&[]).await;
    let (status, body) = server.raw("/api/admin/status").await;
    assert_eq!(status, 200);
    let filters = body["db"]["filters"].as_object().expect("db.filters");
    assert_eq!(filters.len(), 7, "{filters:?}");
    assert_eq!(filters["all"], body["db"]["total"]);
    for name in FILTER_NAMES {
        let (_, page) = server
            .raw(&format!("/api/admin/photos?filter={name}&per=200"))
            .await;
        assert_eq!(page["filter"], name);
        assert_eq!(filters[name], page["total"], "{name}");
    }
}

#[tokio::test]
async fn hints_go_to_no_location_rows_under_every_filter() {
    let dir = TempDir::new("hints");
    let writer = build_admin_catalog(dir.path());
    writer
        .execute_batch(
            "DELETE FROM photo_loc_suggestions; DELETE FROM photo_loc_clues;
             DELETE FROM photo_ai_group_members; DELETE FROM photo_ai_groups;
             DELETE FROM photo_ai_features; DELETE FROM photo_quality_features;
             DELETE FROM photo_quality; DELETE FROM photos;
             INSERT INTO photos(id, source, name, taken_at, lat, lon, loc_source, city)
               VALUES (1, 'local', 'anchor', '2025-01-01 10:00', 35.0, 139.0, 'exif', '東京');
             INSERT INTO photos(id, source, name, taken_at, favorite) VALUES (2, 'local', 'fav-noloc', '2025-01-01 10:05', 1);
             INSERT INTO photos(id, source, name, taken_at) VALUES (3, 'local', 'plain-noloc', '2025-01-01 10:10');
             INSERT INTO photos(id, source, name, taken_at, lat, lon, loc_source, hidden)
               VALUES (4, 'local', 'hidden-loc', '2025-01-01 10:20', 35.1, 139.1, 'exif', 1);",
        )
        .unwrap();
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog).await;
    let hinted = |body: &Value| -> Vec<(i64, bool)> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["id"].as_i64().unwrap(), i.get("hint").is_some()))
            .collect()
    };
    let all = get(addr, "/api/admin/photos?filter=all&per=200")
        .await
        .json();
    assert_eq!(
        hinted(&all),
        [(1, false), (2, true), (3, true), (4, false)],
        "{all}"
    );
    assert_eq!(all["items"][1]["hint"]["label"], "東京");
    assert_eq!(all["items"][1]["hint"]["dt_s"], 300);
    let fav = get(addr, "/api/admin/photos?filter=fav&per=200")
        .await
        .json();
    assert_eq!(hinted(&fav), [(2, true)], "{fav}");
    // ページ内に位置なしの行が無ければ、どの行にも `hint` は付かない
    let hidden = get(addr, "/api/admin/photos?filter=hidden&per=200")
        .await
        .json();
    assert_eq!(hinted(&hidden), [(4, false)], "{hidden}");
    let none = get(addr, "/api/admin/photos?filter=manual&per=200")
        .await
        .json();
    assert!(none["items"].as_array().unwrap().is_empty());
}
