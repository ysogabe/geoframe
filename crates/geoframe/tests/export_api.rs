//! 書き出し用の一覧 API（`GET /api/admin/export/photos`。docs/note-photo-export/design.md の 6.4）の結合試験。
//!
//! 合成の DB だけを使う（日付・座標はすべて架空）。期待値は下の構成から数えた値で、実装の出力を写していない。
//! 応答の例は `crates/geoframe-export/tests/fixtures/export_photos.json` と全体で一致させる（形が変わったら落ちる）。
//! 書き出し側の crate もこのファイルを読んで、応答の型の試験に使う。
//!
//! 期間 2030-03-10〜2030-03-11 の構成（`id`: 撮影日時・状態）:
//!
//! - 1: 03-10 00:00（期間の始まりの境界）。手で付けたタイトル・標高あり。
//! - 2: 03-09 23:59（期間の外）。3: 03-12 00:00（期間の外）。
//! - 4: 03-11 23:59（期間の終わりの境界）。`display` が NULL・位置なし。市と地名から作ったタイトル。
//! - 5: 03-10 08:30。非表示（返らない）。
//! - 6・7: 03-10 09:00（同じ時刻は id 順）。完全一致の重複グループで、6 が推奨、7 が非推奨（フィードには出ない）。
//! - 8: 03-10 10:15。位置は推定（`inferred`）。
//! - 9: 03-10 12:00。位置なし。`hidden` が NULL。
//! - 10・11: 03-10 13:00・13:01。AI の同一場面グループで、10 が推奨、11 が非推奨（フィードには出ない）。
//! - 日時なし: 12（NULL）・13（空）。形の崩れ: 14（`2030-03`）・15（`x`）。非表示の 16（NULL）・17（`y`）は数えない。

mod common;

use common::{TempDir, build_empty_catalog, get, legacy_generation_sql, request, spawn_with_env};
use geoframe::db::Catalog;
use serde_json::{Value, json};

const JSON_UTF8: &str = "application/json; charset=utf-8";

fn photo(id: i64, taken_at: &str, extra: &str) -> String {
    // `extra` は `UPDATE` で列を上書きする（`display`・`md5`・`mtime` の世代は `p<id>`・`m<id>`・`<id>.0`）
    let taken = if taken_at == "NULL" {
        "NULL".to_owned()
    } else {
        format!("'{taken_at}'")
    };
    format!(
        "INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,favorite,hidden)
         VALUES({id},'local','IMG_{id:04}.jpg','derivatives/p{id}.jpg','m{id}',{id}.0,{taken},
                12.{id},34.{id},0,0);
         {extra}\n"
    )
}

fn exact_quality(id: i64, rank: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
           group_kind,rank_in_group,recommended,score,algorithm,analyzed_at)
         VALUES({id},'derivatives/p{id}.jpg','m{id}',{id}.0,'sha-{id}',1,'exact',{rank},{recommended},
           0.5,'t','2030-03-20T00:00:00');\n"
    )
}

fn ai_member(id: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
         VALUES('g',{id},{id},{recommended},0.25);\n"
    )
}

fn rows() -> String {
    let mut sql = String::new();
    sql += &photo(
        1,
        "2030-03-10 00:00",
        "UPDATE photos SET title='架空の朝', elev=100.5, loc_source='exif', lat=12.5, lon=34.5 WHERE id=1;",
    );
    sql += &photo(2, "2030-03-09 23:59", "");
    sql += &photo(3, "2030-03-12 00:00", "");
    sql += &photo(
        4,
        "2030-03-11 23:59",
        "UPDATE photos SET display=NULL, lat=NULL, lon=NULL, city='試験市', place='試験区' WHERE id=4;",
    );
    sql += &photo(
        5,
        "2030-03-10 08:30",
        "UPDATE photos SET hidden=1 WHERE id=5;",
    );
    sql += &photo(
        6,
        "2030-03-10 09:00",
        "UPDATE photos SET lat=12.6, lon=34.6 WHERE id=6;",
    );
    sql += &photo(
        7,
        "2030-03-10 09:00",
        "UPDATE photos SET lat=12.6, lon=34.6 WHERE id=7;",
    );
    sql += &exact_quality(6, 1, 1);
    sql += &exact_quality(7, 2, 0);
    sql += &photo(
        8,
        "2030-03-10 10:15",
        "UPDATE photos SET loc_source='inferred', lat=12.7, lon=34.7 WHERE id=8;",
    );
    sql += &photo(
        9,
        "2030-03-10 12:00",
        "UPDATE photos SET lat=NULL, lon=NULL, hidden=NULL WHERE id=9;",
    );
    sql += &photo(
        10,
        "2030-03-10 13:00",
        "UPDATE photos SET lat=12.8, lon=34.8 WHERE id=10;",
    );
    sql += &photo(
        11,
        "2030-03-10 13:01",
        "UPDATE photos SET lat=12.8, lon=34.8 WHERE id=11;",
    );
    sql += "INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene)
            VALUES('g','scene','t','2030-03-20T00:00:00','judged',1);\n";
    sql += &ai_member(10, 1);
    sql += &ai_member(11, 0);
    sql += &legacy_generation_sql(&[10, 11]);
    sql += &photo(12, "NULL", "");
    sql += &photo(13, "", "");
    sql += &photo(14, "2030-03", "");
    sql += &photo(15, "x", "");
    // 文字列の比較では期間内に入るが、分までの形ではない（TIMED の条件が抜けると一覧に出てしまう）
    sql += &photo(18, "2030-03-10 10:15:30", "");
    sql += &photo(16, "NULL", "UPDATE photos SET hidden=1 WHERE id=16;");
    sql += &photo(17, "y", "UPDATE photos SET hidden=1 WHERE id=17;");
    sql
}

struct Server {
    _dir: TempDir,
    addr: std::net::SocketAddr,
}

impl Server {
    async fn start(rows: &str) -> Self {
        let dir = TempDir::new("export-api");
        let (path, _db) = build_empty_catalog(dir.path(), rows);
        let addr = spawn_with_env(dir.path(), &[], Catalog::open(&path).unwrap()).await;
        Self { _dir: dir, addr }
    }

    async fn export(&self, query: &str) -> Value {
        let target = format!("/api/admin/export/photos?{query}");
        let response = get(self.addr, &target).await;
        assert_eq!(response.status, 200, "{target}: {}", response.text());
        assert!(
            response
                .headers
                .contains(&format!("content-type: {JSON_UTF8}")),
            "{target}"
        );
        assert!(
            response.headers.contains("cache-control: no-store"),
            "{target}"
        );
        response.json()
    }

    async fn status(&self, query: &str) -> u16 {
        get(self.addr, &format!("/api/admin/export/photos?{query}"))
            .await
            .status
    }
}

fn ids(body: &Value) -> Vec<i64> {
    body["photos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect()
}

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../geoframe-export/tests/fixtures/export_photos.json")
}

#[tokio::test]
async fn returns_the_period_with_every_visible_photo() {
    let server = Server::start(&rows()).await;
    let body = server.export("from=2030-03-10&to=2030-03-11").await;
    // 並びは撮影日時、同じ時刻は id の昇順。非表示・期間外・日時なし・形の崩れは出ない。
    // 位置なし（4・9）、重複で非推奨（7）、AI で非推奨（11）、表示用 JPEG なし（4）は出る
    assert_eq!(ids(&body), [1, 6, 7, 8, 9, 10, 11, 4]);
    assert_eq!(body["from"], "2030-03-10");
    assert_eq!(body["to"], "2030-03-11");
    assert_eq!(body["undated"], 2);
    assert_eq!(body["invalid_time"], 3);
    assert_eq!(body["truncated"], false);
    // 1 件のキーは 8 つだけで、無い値もキーを省かない
    for p in body["photos"].as_array().unwrap() {
        let mut keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "elev", "file", "id", "inferred", "lat", "lon", "time", "title"
            ]
        );
    }
}

/// 試験が頼る「フィードには出ない」写真が、実際にフィードから外れていること（出なければ選別を掛けないことの試験にならない）。
#[tokio::test]
async fn the_rows_hidden_from_the_feed_are_really_hidden_there() {
    let server = Server::start(&rows()).await;
    let feed = get(server.addr, "/api/v1/photos").await.json();
    let feed_ids: Vec<i64> = feed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect();
    // 位置なし（4・9）・重複の非推奨（7）・AI の非推奨（11）はフィードに無い。同じ行が書き出しの一覧には出る
    for id in [4, 7, 9, 11] {
        assert!(
            !feed_ids.contains(&id),
            "{id} はフィードに出ない: {feed_ids:?}"
        );
    }
    for id in [1, 6, 8, 10] {
        assert!(
            feed_ids.contains(&id),
            "{id} はフィードに出る: {feed_ids:?}"
        );
    }
    let body = server.export("from=2030-03-10&to=2030-03-11").await;
    for id in [4, 7, 9, 11] {
        assert!(ids(&body).contains(&id), "{id}");
    }
    // タイトルはフィードと同じ規則（同じ写真の title が一致する）
    for p in feed.as_array().unwrap() {
        let id = p["id"].as_i64().unwrap();
        if let Some(mine) = body["photos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["id"] == id)
        {
            assert_eq!(mine["title"], p["title"], "{id}");
            assert_eq!(mine["file"], p["file"], "{id}");
            assert_eq!(mine["inferred"], p["inferred"], "{id}");
        }
    }
}

#[tokio::test]
async fn values_are_passed_through_with_nulls() {
    let server = Server::start(&rows()).await;
    let body = server.export("from=2030-03-10&to=2030-03-11").await;
    let by_id = |id: i64| {
        body["photos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(1)["title"], "架空の朝");
    assert_eq!(by_id(1)["elev"], 100.5);
    // `display` が NULL なら `file: null`（作成待ち）。市と地名のタイトル。位置なし
    assert_eq!(by_id(4)["file"], Value::Null);
    assert_eq!(by_id(4)["title"], "試験市 試験区");
    assert_eq!(by_id(4)["lat"], Value::Null);
    assert_eq!(by_id(4)["lon"], Value::Null);
    // 手がかりが無ければファイル名の stem
    assert_eq!(by_id(6)["title"], "IMG_0006");
    assert_eq!(by_id(6)["inferred"], false);
    assert_eq!(by_id(8)["inferred"], true);
    assert_eq!(by_id(9)["lat"], Value::Null);
}

#[tokio::test]
async fn day_boundaries_are_inclusive_of_00_00_and_23_59() {
    let server = Server::start(&rows()).await;
    // 1 日だけ。03-10 は 00:00 の写真を含み、03-09 の 23:59 と 03-11 の写真を含まない
    let day = server.export("from=2030-03-10&to=2030-03-10").await;
    assert_eq!(ids(&day), [1, 6, 7, 8, 9, 10, 11]);
    let eve = server.export("from=2030-03-09&to=2030-03-09").await;
    assert_eq!(ids(&eve), [2]);
    // 23:59 は終わりの日に含まれ、次の日の 00:00 は含まれない
    let last = server.export("from=2030-03-11&to=2030-03-11").await;
    assert_eq!(ids(&last), [4]);
    let next = server.export("from=2030-03-12&to=2030-03-12").await;
    assert_eq!(ids(&next), [3]);
    // 写真の無い日は空の一覧。件数はカタログ全体のまま
    let empty = server.export("from=2030-04-01&to=2030-04-01").await;
    assert_eq!(ids(&empty), Vec::<i64>::new());
    assert_eq!(empty["undated"], 2);
    assert_eq!(empty["invalid_time"], 3);
    assert_eq!(empty["truncated"], false);
}

#[tokio::test]
async fn span_limit_and_bad_queries() {
    let server = Server::start(&rows()).await;
    // 14 日（両端を含む）は可、15 日は 400
    let ok = server.export("from=2030-03-01&to=2030-03-14").await;
    assert_eq!(ids(&ok), [2, 1, 6, 7, 8, 9, 10, 11, 4, 3]);
    assert_eq!(server.status("from=2030-03-01&to=2030-03-15").await, 400);
    // 逆順・実在しない日付・欠け・形の違い・未知のキー・重複キー
    for q in [
        "from=2030-03-11&to=2030-03-10",
        "from=2030-02-30&to=2030-03-01",
        "from=2031-02-29&to=2031-03-01",
        "from=2030-03-10",
        "to=2030-03-10",
        "",
        "from=2030-3-10&to=2030-3-11",
        "from=2030-03-10%2000:00&to=2030-03-11",
        "from=2030-03-10&to=2030-03-11&hidden=1",
        "from=2030-03-10&to=2030-03-11&x=1",
        "from=2030-03-10&from=2030-03-10&to=2030-03-11",
        "from=2030-03-10&to=2030-03-11&to=2030-03-11",
    ] {
        let response = get(server.addr, &format!("/api/admin/export/photos?{q}")).await;
        assert_eq!(response.status, 400, "{q}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "bad_params"}),
            "{q}"
        );
    }
    // 入力の値は応答に出ない
    let echoed = get(
        server.addr,
        "/api/admin/export/photos?from=SECRET-VALUE-93ab&to=2030-03-11",
    )
    .await;
    assert_eq!(echoed.status, 400);
    assert!(!echoed.text().contains("SECRET-VALUE"));
}

#[tokio::test]
async fn the_list_is_capped_at_2000_and_says_when_it_was_cut() {
    // 2030-04-01 00:00 から 1 分ずつの写真（id = 1000 + n）。2,000 件ちょうどでは切らない
    let fill = |count: usize| {
        format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i<{})
             INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,favorite,hidden)
             SELECT 1000+i,'local','IMG_'||i||'.jpg','derivatives/f'||i||'.jpg','m'||i,i,
                    strftime('%Y-%m-%d %H:%M','2030-04-01 00:00','+'||i||' minutes'),
                    12.5,34.5,0,0 FROM n;",
            count - 1
        )
    };
    let exact = Server::start(&fill(2000)).await;
    let body = exact.export("from=2030-04-01&to=2030-04-03").await;
    assert_eq!(body["photos"].as_array().unwrap().len(), 2000);
    assert_eq!(body["truncated"], false);

    // 2,001 件では、先頭の 2,000 件（撮影日時の昇順）と `truncated: true`
    let over = Server::start(&fill(2001)).await;
    let body = over.export("from=2030-04-01&to=2030-04-03").await;
    let got = ids(&body);
    assert_eq!(got.len(), 2000);
    assert_eq!(got[0], 1000);
    assert_eq!(got[1999], 2999);
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn only_get_on_the_exact_path_is_served() {
    let server = Server::start(&rows()).await;
    let path = "/api/admin/export/photos?from=2030-03-10&to=2030-03-10";
    // POST・HEAD は既存の管理 API と同じく 403
    for method in ["POST", "HEAD"] {
        let response = request(server.addr, method, path, &[("Host", "localhost")]).await;
        assert_eq!(response.status, 403, "{method}");
    }
    // 似た名前・末尾の `/`・大文字は静的配信の判定に回って 403
    for target in [
        "/api/admin/export/photosX?from=2030-03-10&to=2030-03-10",
        "/api/admin/export/photos/?from=2030-03-10&to=2030-03-10",
        "/api/admin/export/Photos?from=2030-03-10&to=2030-03-10",
        "/api/admin/export?from=2030-03-10&to=2030-03-10",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
        assert!(!response.text().contains("\"photos\""), "{target}");
    }
    // Host の検査（許可されていない名前は 403）
    let response = request(server.addr, "GET", path, &[("Host", "unlisted.example")]).await;
    assert_eq!(response.status, 403);
    assert!(!response.text().contains("\"photos\""));
}

/// 応答の例（書き出し側の fixture）と、実際の応答が全体で一致する。形・キー・値の型が変わったら落ちる。
/// 期待値は手で書いたファイルで、この試験は書き換えない。
#[tokio::test]
async fn the_response_matches_the_export_fixture() {
    let server = Server::start(&rows()).await;
    let body = server.export("from=2030-03-10&to=2030-03-11").await;
    let path = fixture_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let fixture: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body, fixture);
    // fixture の `file` は書き出し側が通す形（design 4.4）
    for p in fixture["photos"].as_array().unwrap() {
        if let Some(file) = p["file"].as_str() {
            assert!(
                file.starts_with("derivatives/")
                    && file.ends_with(".jpg")
                    && !file.contains("..")
                    && file["derivatives/".len()..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
                "{file}"
            );
        }
    }
}

/// 読み取り専用で、DB を変えない（応答のあとも行数が同じ）。
#[tokio::test]
async fn a_request_does_not_change_the_catalog() {
    let dir = TempDir::new("export-api-ro");
    let (path, db) = build_empty_catalog(dir.path(), &rows());
    let before: i64 = db
        .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))
        .unwrap();
    let addr = spawn_with_env(dir.path(), &[], Catalog::open(&path).unwrap()).await;
    let response = get(
        addr,
        "/api/admin/export/photos?from=2030-03-10&to=2030-03-11",
    )
    .await;
    assert_eq!(response.status, 200);
    let after: i64 = db
        .query_row("SELECT COUNT(*) FROM photos", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}
