//! 旅行ブログの読み取り API（E7〜E10 `GET /api/blog/…`。T3.5）の結合テスト。
//!
//! 期待値は現行 Python の `travel_blog`（`handlers.Handler` 経由）が合成の旅程・下書き・写真に出した応答で、
//! `tests/fixtures/blog-python.json`（Python 版で生成した基準。Python 版は P5a のマージのコミット a804623 まで存在し、生成の道具 `tools/gen_blog_fixture.py` は P5b で削除した。再生成はできない）。旅程・下書き・書き出しの印は
//! フィクスチャから、写真の行は `blog-catalog.sql` から作り直す。リポジトリにある実際の旅程 JSON は使わない
//! （埋め込みの検査だけは、実ファイルと表が一致するかを真偽でだけ見る。中身は出力しない）。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{TempDir, get, request, test_state};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::server::router;
use serde_json::Value;

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blog-python.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn catalog_sql() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/blog-catalog.sql");
    std::fs::read_to_string(path).unwrap()
}

/// `{name: text | {"dir": true}}` をディレクトリに書く。
fn write_files(dir: &Path, files: &Value) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, content) in files.as_object().unwrap() {
        let path = dir.join(name);
        match content {
            Value::String(text) => std::fs::write(&path, text).unwrap(),
            _ => std::fs::create_dir(&path).unwrap(),
        }
    }
}

struct Site {
    _dir: TempDir,
    plans: PathBuf,
    data: PathBuf,
    addr: std::net::SocketAddr,
    /// 書き込み用の接続（WAL を残す。DB を壊す試験にも使う）
    conn: rusqlite::Connection,
}

impl Site {
    /// 旅程のディレクトリに置くものを `plans` で与える。`None` は埋め込み（`GEOFRAME_BLOG_PLANS_DIR` なし）。
    async fn start(plans: Option<&Value>, drafts: &Value, exports: &[String]) -> Self {
        let dir = TempDir::new("blog-api");
        let data = dir.path().join("data");
        let plan_dir = dir.path().join("plans");
        if let Some(plans) = plans {
            write_files(&plan_dir, plans);
        }
        write_files(&data.join("travel_blog/drafts"), drafts);
        for mark in exports {
            let path = data.join("blog_exports").join(mark);
            if mark.ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "<html></html>").unwrap();
            }
        }
        let db = data.join("catalog.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(&catalog_sql()).unwrap();
        let plan_arg = plans.is_some().then(|| plan_dir.display().to_string());
        let config = Config::load(
            |key| match key {
                "GEOFRAME_BLOG" => Some("1".to_string()),
                "GEOFRAME_BLOG_PLANS_DIR" => plan_arg.clone(),
                _ => None,
            },
            &Overrides {
                data_dir: Some(data.clone()),
                ..Overrides::default()
            },
        )
        .unwrap();
        assert!(!config.ingest, "取り込みは無効（書き込みなしで動く）");
        let state = test_state(Arc::new(config), Catalog::open(&db).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        Self {
            _dir: dir,
            plans: plan_dir,
            data,
            addr,
            conn,
        }
    }

    async fn from_fixture() -> Self {
        let f = fixture();
        let exports: Vec<String> = f["exports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        Self::start(Some(&f["plans"]), &f["drafts"], &exports).await
    }

    async fn json(&self, target: &str) -> (u16, Value) {
        let r = get(self.addr, target).await;
        (r.status, r.json())
    }
}

/// 応答の中の全キー。
fn keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                keys(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| keys(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn every_response_matches_python() {
    let site = Site::from_fixture().await;
    let f = fixture();
    let cases = f["cases"].as_array().unwrap();
    assert!(cases.len() >= 100, "{}", cases.len());
    let mut statuses = std::collections::BTreeSet::new();
    for case in cases {
        let target = case["target"].as_str().unwrap();
        let r = get(site.addr, target).await;
        let expected = case["status"].as_u64().unwrap() as u16;
        assert_eq!(r.status, expected, "{target}");
        statuses.insert(r.status);
        if case["body"].is_null() {
            // Python の send_error（HTML）。JSON ではない
            assert!(
                r.header("content-type").unwrap().starts_with("text/html"),
                "{target}"
            );
            continue;
        }
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8"),
            "{target}"
        );
        assert_eq!(
            r.header("cache-control"),
            case["cache_control"].as_str(),
            "{target}"
        );
        // JSON として一致（キーの並びは問わない）。失敗時に旅程の中身を出さないため、真偽で比べる
        assert!(r.json() == case["body"], "{target}: 応答が Python と違う");
    }
    // 200・400・403・404 の全てを通した
    assert_eq!(
        statuses.into_iter().collect::<Vec<_>>(),
        vec![200, 400, 403, 404]
    );
}

/// AC7: 候補・初期案の応答に、位置（`lat`・`lon`・`pref`・`elev`）・ファイル名（`name`）・座標の値が入らない。
/// 合成の行には座標・都道府県・ファイル名を入れてある。
#[tokio::test]
async fn responses_never_carry_exact_positions_or_file_names() {
    let site = Site::from_fixture().await;
    let f = fixture();
    let mut checked = 0;
    for case in f["cases"].as_array().unwrap() {
        let target = case["target"].as_str().unwrap();
        if !target.contains("candidates") && !target.contains("draft") {
            continue;
        }
        let r = get(site.addr, target).await;
        if !r
            .header("content-type")
            .unwrap()
            .starts_with("application/json")
        {
            continue; // 未知のパスの 404（HTML）
        }
        let mut all = Vec::new();
        keys(&r.json(), &mut all);
        for forbidden in [
            "lat",
            "lon",
            "name",
            "pref",
            "elev",
            "latitude",
            "longitude",
        ] {
            assert!(
                !all.iter().any(|k| k == forbidden),
                "{target} に {forbidden} がある"
            );
        }
        let text = r.text();
        for secret in ["IMG_", "35.1", "139.1", "架空県", "drive-"] {
            assert!(!text.contains(secret), "{target} に {secret} がある");
        }
        checked += 1;
    }
    assert!(checked >= 40, "{checked}");
    let (_, body) = site
        .json("/api/blog/candidates?trip_id=sample-trip-a")
        .await;
    assert_eq!(body["privacy"]["exact_gps_in_response"], false);
}

/// 候補は `hidden` の写真を含まない。`hidden` を立てると候補・初期案から消え、外すと戻る
/// （書き込みなし＝`GEOFRAME_INGEST=0` でも読める。DB の更新は試験の接続で行う）。
#[tokio::test]
async fn hidden_photos_are_excluded_and_changes_show_up() {
    let site = Site::from_fixture().await;
    let ids = |body: &Value| -> Vec<String> {
        body["chapters"][0]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["photo_id"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, before) = site
        .json("/api/blog/candidates?trip_id=sample-trip-a")
        .await;
    assert!(ids(&before).contains(&"7".to_string()));
    assert_eq!(before["chapters"][0]["photo_count"], 18);
    site.conn
        .execute("UPDATE photos SET hidden=1 WHERE id=7", [])
        .unwrap();
    let (_, after) = site
        .json("/api/blog/candidates?trip_id=sample-trip-a")
        .await;
    // 同じ分の代表が変わり、数も 1 減る
    assert!(!ids(&after).contains(&"7".to_string()));
    assert_eq!(after["chapters"][0]["photo_count"], 17);
    let (_, draft) = site.json("/api/blog/draft?trip_id=sample-trip-a").await;
    assert!(!draft.to_string().contains("p007.jpg"));
    site.conn
        .execute("UPDATE photos SET hidden=0 WHERE id=7", [])
        .unwrap();
    let (_, again) = site
        .json("/api/blog/candidates?trip_id=sample-trip-a")
        .await;
    assert!(again == before);
}

#[tokio::test]
async fn saved_drafts_are_read_fresh_each_time_and_a_new_file_changes_the_source() {
    let site = Site::from_fixture().await;
    let (_, generated) = site.json("/api/blog/draft?trip_id=sample-trip-a").await;
    assert_eq!(generated["source"], "generated");
    let (_, trips) = site.json("/api/blog/trips").await;
    let has_draft = |trips: &Value| {
        trips["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["trip_id"] == "sample-trip-a")
            .unwrap()["has_draft"]
            .clone()
    };
    assert_eq!(has_draft(&trips), false);
    // 下書きを置くと saved になり、has_draft も真になる
    let manifest = serde_json::json!({
        "trip_id": "sample-trip-a", "status": "draft", "chapters": []
    });
    std::fs::write(
        site.data.join("travel_blog/drafts/sample-trip-a.json"),
        manifest.to_string(),
    )
    .unwrap();
    let (status, saved) = site.json("/api/blog/draft?trip_id=sample-trip-a").await;
    assert_eq!(status, 200);
    assert_eq!(saved["source"], "saved");
    // 検査で補われた既定値
    assert_eq!(saved["manifest"]["title"], "");
    assert_eq!(saved["manifest"]["chapters"], serde_json::json!([]));
    let (_, trips) = site.json("/api/blog/trips").await;
    assert_eq!(has_draft(&trips), true);
}

/// 旅程の置き場が無い・空でも落ちない（一覧は空、旅程は 404）。
#[tokio::test]
async fn a_missing_or_empty_plan_directory_lists_nothing() {
    let site = Site::start(Some(&serde_json::json!({})), &serde_json::json!({}), &[]).await;
    std::fs::remove_dir_all(&site.plans).unwrap();
    let (status, body) = site.json("/api/blog/trips").await;
    assert_eq!((status, &body["items"]), (200, &serde_json::json!([])));
    let (status, _) = site.json("/api/blog/trip?trip_id=sample-trip-a").await;
    assert_eq!(status, 404);
    let (status, _) = site.json("/api/blog/draft?trip_id=sample-trip-a").await;
    assert_eq!(status, 404);
}

/// Python が未処理の例外で応答を返さない形（フィクスチャに載せない）。Rust は一覧から外す・400 にする。
#[tokio::test]
async fn malformed_plan_shapes_are_rejected_without_crashing() {
    let seg = |extra: Value| {
        let mut base = serde_json::json!({"id": "s", "date": "2025-06-06"});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    };
    let plan = |id: &str, segments: Value| {
        serde_json::json!({"trip_id": id, "segments": segments}).to_string()
    };
    let plans = serde_json::json!({
        "top-array.json": "[1, 2]",
        "top-string.json": "\"x\"",
        "range-string.json": serde_json::json!({"trip_id": "range-string", "date_range": "x", "segments": []}).to_string(),
        "range-list.json": serde_json::json!({"trip_id": "range-list", "date_range": [1], "segments": []}).to_string(),
        "segments-number.json": serde_json::json!({"trip_id": "segments-number", "segments": 3}).to_string(),
        "seg-not-object.json": plan("seg-not-object", serde_json::json!(["x"])),
        "locations-number.json": plan("locations-number", serde_json::json!([seg(serde_json::json!({"locations": [5]}))])),
        "locations-object.json": plan("locations-object", serde_json::json!([seg(serde_json::json!({"locations": {"a": 1}}))])),
        "activities-string.json": plan("activities-string", serde_json::json!([seg(serde_json::json!({"activities": "x"}))])),
        "activity-string.json": plan("activity-string", serde_json::json!([seg(serde_json::json!({"activities": ["x"]}))])),
        "id-list.json": plan("id-list", serde_json::json!([seg(serde_json::json!({"id": [1]}))])),
        "id-object.json": plan("id-object", serde_json::json!([seg(serde_json::json!({"id": {"a": 1}}))])),
        "nan.json": "{\"trip_id\": \"nan\", \"segments\": [], \"x\": NaN}",
        "ok.json": plan("ok", serde_json::json!([seg(serde_json::json!({}))])),
    });
    let site = Site::start(Some(&plans), &serde_json::json!({}), &[]).await;
    // 旅程が壊れていても UTF-8 でないファイル・BOM つきも同じ扱い
    std::fs::write(site.plans.join("latin1.json"), b"{\"trip_id\": \"\xff\"}").unwrap();
    std::fs::write(
        site.plans.join("bom.json"),
        b"\xef\xbb\xbf{\"trip_id\": \"bom\", \"segments\": []}",
    )
    .unwrap();

    let (status, trips) = site.json("/api/blog/trips").await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = trips["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["trip_id"].as_str().unwrap())
        .collect();
    // 一覧に残るのは、形が合うもの（`segments` が数・`date_range` が文字列/配列のものは外れる）
    assert_eq!(
        ids,
        [
            "activities-string",
            "activity-string",
            "id-list",
            "id-object",
            "locations-number",
            "locations-object",
            "ok",
            "seg-not-object"
        ]
    );

    // `date_range` が文字列でも、旅程そのもの（E8）は返る（一覧から外れるだけ）
    assert_eq!(
        site.json("/api/blog/trip?trip_id=range-string").await.0,
        200
    );
    for id in [
        "top-array",
        "top-string",
        "segments-number",
        "nan",
        "latin1",
        "bom",
    ] {
        let (status, body) = site.json(&format!("/api/blog/trip?trip_id={id}")).await;
        assert_eq!(status, 400, "{id}");
        assert_eq!(body["ok"], false, "{id}");
    }
    // 区間の形が壊れていれば候補・初期案は 400（200 の旅程の中身を壊さない）
    for id in ["seg-not-object", "locations-number", "locations-object"] {
        let (status, body) = site
            .json(&format!("/api/blog/candidates?trip_id={id}"))
            .await;
        assert_eq!(
            (status, &body["error"]),
            (400, &Value::from("旅程JSONの形式が不正です")),
            "candidates {id}"
        );
    }
    // 活動の形が壊れているのは初期案の本文を作るときに分かる（候補だけなら使わない）
    // 区間の id が配列・オブジェクトなら、Python は初期案の `dict` のキーにできず落ちる（候補は返る）
    for id in [
        "id-list",
        "id-object",
        "activities-string",
        "activity-string",
    ] {
        assert_eq!(
            site.json(&format!("/api/blog/candidates?trip_id={id}"))
                .await
                .0,
            200
        );
        let (status, _) = site.json(&format!("/api/blog/draft?trip_id={id}")).await;
        assert_eq!(status, 400, "draft {id}");
    }
    // 形が合えば普通に返る
    assert_eq!(site.json("/api/blog/candidates?trip_id=ok").await.0, 200);
    assert_eq!(site.json("/api/blog/draft?trip_id=ok").await.0, 200);
}

/// 大きすぎるファイル・通常ファイルでないもの（FIFO）は読まずに「読めない」扱い（待たない）。
#[tokio::test]
async fn huge_files_and_fifos_are_not_read() {
    let site = Site::start(Some(&serde_json::json!({})), &serde_json::json!({}), &[]).await;
    let big = format!(
        "{{\"trip_id\": \"huge\", \"segments\": [], \"pad\": \"{}\"}}",
        "x".repeat(9 * 1024 * 1024)
    );
    std::fs::write(site.plans.join("huge.json"), big).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(site.plans.join("pipe.json"))
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        site.data.join("travel_blog/drafts/huge-draft.json"),
        vec![b' '; 9 * 1024 * 1024],
    )
    .unwrap();
    let (_, trips) = site.json("/api/blog/trips").await;
    assert_eq!(trips["items"], serde_json::json!([]));
    for target in [
        "/api/blog/trip?trip_id=huge",
        "/api/blog/trip?trip_id=pipe",
        "/api/blog/draft?trip_id=huge-draft",
    ] {
        let (status, body) = site.json(target).await;
        assert_eq!(status, 400, "{target}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("JSONを読み込めません"),
            "{target}"
        );
    }
}

/// DB を読めないとき、候補・初期案（生成）だけ 503（定型の本文。テーブル名・SQL を出さない）。
/// 旅程・一覧・保存済みの下書きは DB を使わないので 200 のまま。
#[tokio::test]
async fn an_unreadable_catalog_is_a_fixed_503_only_where_the_db_is_used() {
    let site = Site::from_fixture().await;
    site.conn.execute_batch("DROP TABLE photos").unwrap();
    for target in [
        "/api/blog/candidates?trip_id=sample-trip-a",
        "/api/blog/draft?trip_id=sample-trip-a",
    ] {
        let r = get(site.addr, target).await;
        assert_eq!(r.status, 503, "{target}");
        let text = r.text();
        assert!(text.contains("\"ok\":false"), "{text}");
        assert!(
            !text.contains("photos") && !text.contains("sqlite"),
            "{text}"
        );
    }
    for target in [
        "/api/blog/trips",
        "/api/blog/trip?trip_id=sample-trip-a",
        "/api/blog/draft?trip_id=sample-trip-b",
    ] {
        assert_eq!(get(site.addr, target).await.status, 200, "{target}");
    }
    // 旅程が無い・trip_id が不正なら、DB を見る前に 404/400
    assert_eq!(
        get(site.addr, "/api/blog/candidates?trip_id=nonexistent")
            .await
            .status,
        404
    );
    assert_eq!(get(site.addr, "/api/blog/candidates").await.status, 400);
}

#[tokio::test]
async fn only_get_reaches_the_blog_routes() {
    let site = Site::from_fixture().await;
    // HEAD は現行でも静的のホワイトリストに当たらず 403
    for target in ["/api/blog/trips", "/api/blog/nope"] {
        assert_eq!(
            request(site.addr, "HEAD", target, &[("Host", "localhost")])
                .await
                .status,
            403,
            "{target}"
        );
    }
    // 読み取りの API に POST は無い（ルート無し＝403。保存・書き出しは blog_write_api.rs）
    let r = request(
        site.addr,
        "POST",
        "/api/blog/trips",
        &[
            ("Host", "localhost"),
            ("Content-Type", "application/json"),
            ("Content-Length", "2"),
        ],
    )
    .await;
    assert_eq!(r.status, 403);
    // OPTIONS は 501、CORS ヘッダなし
    let r = request(
        site.addr,
        "OPTIONS",
        "/api/blog/trips",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(r.status, 501);
    assert!(!r.headers.contains("access-control-"));
    // Host が許可されない要求は、ルーティングより前に 403
    let r = request(
        site.addr,
        "GET",
        "/api/blog/trips",
        &[("Host", "attacker.invalid")],
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(!r.text().contains("sample-trip"));
}

// ---- 埋め込み（既定）----------------------------------------------------------------------

fn repo_plan_files() -> Vec<(String, Value)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/travel_blog/plans");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !p.file_name().unwrap().to_string_lossy().starts_with('.')
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            (p.file_name().unwrap().to_string_lossy().into_owned(), value)
        })
        .collect()
}

/// `GEOFRAME_BLOG_PLANS_DIR` が無いとき、バイナリに埋め込んだ旅程が、リポジトリの `web/travel_blog/plans/*.json` と
/// 同じ内容・同じ並びで返る（実ファイルとの一致を真偽で見るだけ。中身は出力しない）。
#[tokio::test]
async fn embedded_plans_are_the_repository_files() {
    let site = Site::start(None, &serde_json::json!({}), &[]).await;
    let files = repo_plan_files();
    assert!(!files.is_empty());
    let (status, trips) = site.json("/api/blog/trips").await;
    assert_eq!(status, 200);
    let listed: Vec<Value> = trips["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["trip_id"].clone())
        .collect();
    let expected: Vec<Value> = files
        .iter()
        .map(|(_, plan)| plan["trip_id"].clone())
        .collect();
    assert!(listed == expected, "一覧の trip_id が実ファイルと違う");
    for (_, plan) in &files {
        let id = plan["trip_id"].as_str().unwrap();
        let (status, body) = site.json(&format!("/api/blog/trip?trip_id={id}")).await;
        assert_eq!(status, 200);
        assert!(body["plan"] == *plan, "埋め込みの旅程が実ファイルと違う");
        let (status, _) = site
            .json(&format!("/api/blog/candidates?trip_id={id}"))
            .await;
        assert_eq!(status, 200);
        let (status, draft) = site.json(&format!("/api/blog/draft?trip_id={id}")).await;
        assert_eq!(status, 200);
        assert_eq!(draft["source"], "generated");
    }
}

/// ディレクトリの指定があれば、埋め込みではなくそちらだけを読む。
#[tokio::test]
async fn the_directory_setting_replaces_the_embedded_plans() {
    let f = fixture();
    let site = Site::start(Some(&f["plans"]), &f["drafts"], &[]).await;
    let (_, trips) = site.json("/api/blog/trips").await;
    let embedded_ids: Vec<Value> = repo_plan_files()
        .into_iter()
        .map(|(_, plan)| plan["trip_id"].clone())
        .collect();
    for item in trips["items"].as_array().unwrap() {
        assert!(!embedded_ids.contains(&item["trip_id"]));
    }
    for id in &embedded_ids {
        let target = format!("/api/blog/trip?trip_id={}", id.as_str().unwrap());
        assert_eq!(site.json(&target).await.0, 404);
    }
}
