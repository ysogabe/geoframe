//! `GET /api/admin/config`（管理画面の設定タブ。T7）の結合テスト。合成データと予約済みの名前だけを使う。
//!
//! 全項目を設定どおりの値で返す（ユーザーの決定、2026-10-30）ので、目印（検査に通る形の ASCII）が値として応答に
//! 含まれ、鍵・認証ファイルの**中身**（`SECRET-mark`）がどこにも出ないことを、`router` を通した本文で確かめる。

mod common;

use std::path::Path;

use common::{TempDir, build_admin_catalog, get, request, spawn_with_env};
use geoframe::config::EnvItem;
use geoframe::db::Catalog;
use serde_json::Value;

const SECRET: &str = "SECRET-mark";

struct Server {
    dir: TempDir,
    writer: rusqlite::Connection,
    addr: std::net::SocketAddr,
}

fn ai_json(key: &Path) -> String {
    format!(
        r#"{{
  "version": 1,
  "local_base_url": "http://mark-host.invalid:8100",
  "embedding_space": {{
    "space_id": "mark-space-id", "model_id": "mark-space-model", "revision": "mark-space-rev",
    "image_preprocess": "mark-space-img", "text_preprocess": "mark-space-txt",
    "dimension": 768, "normalization": "l2"
  }},
  "tasks": {{
    "image_embedding": {{"provider": "local", "model": "mark-model-embed", "service_id": "mark-svc"}},
    "query_embedding": {{"provider": "local", "model": "mark-model-embed", "service_id": "mark-svc"}},
    "photo_judge": {{"provider": "cloud", "model": "mark-model-judge", "allow_cloud": true,
      "daily_request_limit": 20, "credential_path": "{key}",
      "project": "mark-project", "region": "mark-region1"}},
    "location_clue": {{"provider": "local", "model": "mark-model-decision"}},
    "location_suggest": {{"provider": "cloud", "model": "mark-model-suggest",
      "credential_path": "{key}", "project": "mark-project", "region": "mark-region1"}}
  }}
}}"#,
        key = key.display()
    )
}

impl Server {
    /// `ai` が真なら、目印つきの AI 設定と鍵ファイル（中身は `SECRET-mark`）を置いて、環境変数で与える。
    async fn start(ai: bool, extra: &[(&str, &str)]) -> Self {
        let dir = TempDir::new("admin-config");
        let writer = build_admin_catalog(dir.path());
        let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
        let key = dir.path().join("mark-key-1.json");
        let ai_path = dir.path().join("mark-ai.json");
        let (key_s, ai_s) = (key.display().to_string(), ai_path.display().to_string());
        let mut env: Vec<(&str, &str)> = extra.to_vec();
        if ai {
            std::fs::write(&key, SECRET).unwrap();
            std::fs::write(&ai_path, ai_json(&key)).unwrap();
            env.push(("GEOFRAME_AI_CONFIG", &ai_s));
            env.push(("GEOFRAME_DRIVE_KEY", &key_s));
            env.push(("GEOFRAME_TILE_UA", "mark@example.test"));
            env.push(("GEOFRAME_DRIVE_FOLDER", "mark-folder"));
            env.push(("GEOFRAME_ALLOWED_HOSTS", "mark-host.example.test"));
        }
        let addr = spawn_with_env(dir.path(), &env, catalog).await;
        Self { dir, writer, addr }
    }
}

#[tokio::test]
async fn the_config_is_served_as_no_store_json_with_items_and_ai() {
    let server = Server::start(true, &[("GEOFRAME_INGEST", "1")]).await;
    let response = get(server.addr, "/api/admin/config").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert!(body["started_at"].as_str().unwrap().len() == 19);
    assert!(body["uptime_s"].is_u64());
    // 全項目が、EnvItem::ALL の順に出る
    let keys: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = EnvItem::ALL.iter().map(|i| i.name()).collect();
    assert_eq!(keys, expected);
    // `spawn_with_env` は data_dir を引数で与える
    let item = |key: &str| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(item("GEOFRAME_DATA_DIR")["source"], "arg");
    assert_eq!(
        item("GEOFRAME_DATA_DIR")["value"],
        server.dir.path().display().to_string()
    );
    assert_eq!(item("GEOFRAME_INGEST")["value"], "1");
    assert_eq!(item("GEOFRAME_INGEST")["source"], "env");
    assert_eq!(item("GEOFRAME_BLOG")["source"], "default");
    assert_eq!(item("GEOFRAME_DRIVE_KEY")["state"], "present");
    assert_eq!(item("GEOFRAME_AI_CONFIG")["state"], "loaded");
    assert_eq!(body["ai"]["configured"], true);
    assert_eq!(body["ai"]["invalid"], Value::Null);
    assert_eq!(body["ai"]["tasks"].as_array().unwrap().len(), 5);
    // 目印は値として出る。鍵の中身と内部の値は出ない
    let text = response.text();
    for mark in [
        "mark-host.invalid:8100",
        "mark-space-id",
        "mark-svc",
        "mark@example.test",
        "mark-project",
        "mark-region1",
        "mark-model-judge",
        "mark-folder",
        "mark-key-1.json",
        "mark-host.example.test",
    ] {
        assert!(text.contains(mark), "{mark}");
    }
    assert!(!text.contains(SECRET), "{text}");
    assert!(!text.contains("embedding_contract") && !text.contains("config_hash"));
}

#[tokio::test]
async fn it_answers_200_without_ingest_and_without_ai_and_today_is_null_or_a_number() {
    // AI 設定なし・INGEST なし
    let server = Server::start(false, &[]).await;
    let body = get(server.addr, "/api/admin/config").await.json();
    assert_eq!(body["ai"]["configured"], false);
    assert_eq!(body["ai"]["tasks"], serde_json::json!([]));
    // AI 設定あり・INGEST なし。使用量の表が無いので、クラウドの用途の today_utc は null（応答は 200 のまま）
    let server = Server::start(true, &[]).await;
    let response = get(server.addr, "/api/admin/config").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    let tasks = body["ai"]["tasks"].as_array().unwrap();
    let judge = tasks.iter().find(|t| t["task"] == "photo_judge").unwrap();
    assert_eq!(judge["today_utc"], Value::Null);
    assert_eq!(judge["daily_limit"], 20);
    // INGEST=0 の理由は、設定ではなく実行時の状態なので disabled に混ぜない（writer があるものとして評価）
    assert_eq!(judge["disabled"], Value::Null);
    // 使用量の表があれば、今日（UTC 日）の件数が数として出る。ほかの日の行は数えない
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let day = now - now.rem_euclid(86_400);
    server
        .writer
        .execute_batch(&format!(
            "CREATE TABLE ai_request_usage(
               task TEXT NOT NULL, scope TEXT NOT NULL, window_start INTEGER NOT NULL,
               count INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL,
               PRIMARY KEY(task, scope, window_start)) WITHOUT ROWID;
             INSERT INTO ai_request_usage VALUES ('photo_judge','day',{day},7,{now});
             INSERT INTO ai_request_usage VALUES ('photo_judge','day',{prev},9,{now});",
            prev = day - 86_400
        ))
        .unwrap();
    let body = get(server.addr, "/api/admin/config").await.json();
    let tasks = body["ai"]["tasks"].as_array().unwrap();
    let judge = tasks.iter().find(|t| t["task"] == "photo_judge").unwrap();
    assert_eq!(judge["today_utc"], 7);
    let suggest = tasks
        .iter()
        .find(|t| t["task"] == "location_suggest")
        .unwrap();
    assert_eq!(suggest["today_utc"], 0);
    // ローカルの用途は送信数を持たない
    let local = tasks
        .iter()
        .find(|t| t["task"] == "image_embedding")
        .unwrap();
    assert!(local.get("today_utc").is_none());
}

#[tokio::test]
async fn only_get_on_the_exact_path_is_served() {
    let server = Server::start(true, &[]).await;
    // HEAD・POST は既存の管理 API と同じく 403
    for method in ["HEAD", "POST"] {
        let response = request(
            server.addr,
            method,
            "/api/admin/config",
            &[("Host", "localhost")],
        )
        .await;
        assert_eq!(response.status, 403, "{method}");
    }
    // 完全一致だけ: 似た名前・末尾の `/`・大文字・パーセント符号化は静的配信の判定に回って 403
    for target in [
        "/api/admin/configX",
        "/api/admin/config/",
        "/api/admin/config/x",
        "/api/admin/Config",
        "/api/admin/%63onfig",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
        assert!(!response.text().contains("mark"), "{target}");
    }
    // クエリは無視して 200（`/api/admin/status` と同じ）。先頭の `//` も畳む
    for target in ["/api/admin/config?x=1", "//api/admin/config"] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 200, "{target}");
    }
    // 前方一致の既存 API は今までどおり
    assert_eq!(get(server.addr, "/api/admin/status").await.status, 200);
}

#[tokio::test]
async fn no_other_response_leaks_the_key_body() {
    let server = Server::start(true, &[("GEOFRAME_INGEST", "1")]).await;
    for target in [
        "/api/admin/config",
        "/api/admin/status",
        "/api/ai/status",
        "/api/v1/photos",
    ] {
        let response = get(server.addr, target).await;
        assert!(!response.text().contains(SECRET), "{target}");
    }
}
