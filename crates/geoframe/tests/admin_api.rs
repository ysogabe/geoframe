//! `GET /api/admin/status`・`GET /api/admin/photos`（T1.4）の結合テスト。合成データだけを使う。
//!
//! 現行 Python の出力（`tools/gen_admin_fixture.py` が書き出すフィクスチャ）と、`serde_json::Value`
//! として完全一致することを確かめる。Python の出力と比べない値は `disk`（機体のディスクと実行時刻に
//! 依存）と `server_time` で、これは形と数え方を別に検査する。

mod common;

use std::time::Duration;

use common::{TempDir, admin_fixture, build_admin_catalog, get, request, spawn_with_env};
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

    async fn json(&self, target: &str) -> (u16, Value) {
        let response = get(self.addr, target).await;
        (response.status, strip_unstable(response.json()))
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
async fn only_the_two_reads_are_implemented() {
    let server = Server::start(&[]).await;
    // 地名検索と残りの管理 API は P3 まで 503（定型）
    for target in [
        "/api/admin/geocode?q=sample",
        "/api/admin/setloc",
        "/api/admin/",
        "/api/admin/stat",
        "/api/admin/Status",
        // 生のパスで見るので、エンコードした形は当たらない（現行は 403。Rust は未実装の 503）
        "/api/admin/%73tatus",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 503, "{target}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "この機能は Rust 版ではまだ使えません"}),
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
        ("//api/admin/geocode?q=x", 503),
        ("//api/ai/status", 503),
        ("//tiles/osm/1/0/0.png", 503),
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
