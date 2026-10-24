//! `GET /api/v1/photos`（T1.2）が、現行 Python の `api_photos()` と同じ行・同じ並び・同じ値を返すこと。
//!
//! 合成データだけを使う。フィクスチャの DB は `tools/gen_photo_feed_fixture.py` が現行 Python の
//! スキーマと行（app/tests/photo_feed_fixture.py）から書き出した SQL で、同じ DB を Rust 側でも作り、
//! エンドポイントの出力を Python の出力（photo-feed-python.json）と `serde_json::Value` として
//! 完全一致させる。除外・並び・旅番号・タイトルなどは、行を絞った小さな DB で個別に確かめる。
//!
//! 同順位（同じ `taken_at`）の写真の並びは SQLite の実行計画に依存し、ここでは固定しない
//! （T1.5 の本番コピーでの全件比較で確かめる。src/photos.rs の説明）。

mod common;

use std::time::Duration;

use common::{
    TempDir, build_catalog, build_empty_catalog, get, photos_from, python_fixture, spawn,
    validator, violations,
};
use geoframe::db::Catalog;
use geoframe::feed::Photo;
use serde_json::{Value, json};

const JSON_UTF8: &str = "application/json; charset=utf-8";

/// 写真1行の INSERT。`over` は列の上書き（値は SQL のリテラル）。撮影時刻は id の分（10:ID）。
fn photo(id: i64, over: &[(&str, &str)]) -> String {
    let defaults = [
        ("source", "'local'".to_string()),
        ("display", format!("'derivatives/p{id:02}.jpg'")),
        ("name", format!("'IMG_{id:04}.jpg'")),
        ("md5", format!("'md5-{id}'")),
        ("taken_at", format!("'2025-06-01 10:{id:02}'")),
        ("lat", "35.0".to_string()),
        ("lon", "139.0".to_string()),
    ];
    let mut cols = vec![("id", id.to_string())];
    for (name, value) in defaults {
        let value = over
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(value, |(_, v)| (*v).to_string());
        cols.push((name, value));
    }
    for (name, value) in over {
        if !cols.iter().any(|(n, _)| n == name) {
            cols.push((name, (*value).to_string()));
        }
    }
    let names: Vec<&str> = cols.iter().map(|(n, _)| *n).collect();
    let values: Vec<&str> = cols.iter().map(|(_, v)| v.as_str()).collect();
    format!(
        "INSERT INTO photos({}) VALUES({});\n",
        names.join(","),
        values.join(",")
    )
}

/// 品質解析の行（`md5` は写真の `md5` と一致させると適用される）。
fn quality(id: i64, md5: &str, group: i64, kind: &str, rank: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,
           group_id,group_kind,rank_in_group,recommended,score,algorithm,analyzed_at)
         VALUES({id},'derivatives/p{id:02}.jpg','{md5}',NULL,'sha',{group},'{kind}',{rank},
           {recommended},0.5,'t','2025-06-10T00:00:00');\n"
    )
}

fn ai_group(group: &str, status: &str, same_scene: i64) -> String {
    format!(
        "INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene)
         VALUES('{group}','scene','t','2025-06-10T00:00:00','{status}',{same_scene});\n"
    )
}

fn ai_member(group: &str, id: i64, rank: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
         VALUES('{group}',{id},{rank},{recommended},0.25);\n"
    )
}

/// 行を入れた合成 DB（フィクスチャの行は消す）の応答。
async fn feed(rows: &str) -> Value {
    let dir = TempDir::new("feed");
    let (path, _writer) = build_empty_catalog(dir.path(), rows);
    let response = photos_from(&path).await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.json()
}

fn column<'a>(list: &'a Value, key: &str) -> Vec<&'a Value> {
    list.as_array().unwrap().iter().map(|p| &p[key]).collect()
}

fn ids(list: &Value) -> Vec<i64> {
    column(list, "id")
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect()
}

fn trips(list: &Value) -> Vec<i64> {
    column(list, "trip")
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect()
}

fn titles(list: &Value) -> Vec<&str> {
    column(list, "title")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn output_equals_the_python_output_exactly() {
    let dir = TempDir::new("fixture");
    let (path, _writer) = build_catalog(dir.path(), "");
    let response = photos_from(&path).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let fixture = python_fixture();
    // キー・値・null・整数と小数の区別まで同じ
    assert_eq!(response.json(), fixture);
    // 型で読み直しても、契約の Schema に照らしても問題ない
    assert_eq!(
        violations(&validator(), &response.json()),
        Vec::<String>::new()
    );
    let photos: Vec<Photo> = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(photos.len(), fixture.as_array().unwrap().len());
}

#[tokio::test]
async fn headers_are_the_same_as_the_python_server() {
    let dir = TempDir::new("headers");
    let (path, _writer) = build_catalog(dir.path(), "");
    let response = photos_from(&path).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(
        response.header("content-length"),
        Some(response.body.len().to_string().as_str())
    );
    // 日本語はエスケープしない UTF-8 のまま（Python の ensure_ascii=False）
    let text = response.text();
    assert!(text.contains("\"city\":\"見本市\""), "{text}");
    assert!(!text.contains("\\u"), "{text}");
}

#[tokio::test]
async fn query_string_is_ignored() {
    let dir = TempDir::new("query");
    let (path, _writer) = build_catalog(dir.path(), "");
    let addr = spawn(Catalog::open(&path).unwrap()).await;
    let plain = get(addr, "/api/v1/photos").await;
    let with_query = get(addr, "/api/v1/photos?x=1&y=%E3%81%82").await;
    assert_eq!(with_query.status, 200);
    assert_eq!(with_query.json(), plain.json());
}

#[tokio::test]
async fn an_empty_catalog_gives_an_empty_array() {
    let list = feed("").await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn selection_rules() {
    let mut sql = String::new();
    // 1: ふつうに出る。5: hidden が NULL でも出る（COALESCE）
    sql += &photo(1, &[]);
    sql += &photo(5, &[("hidden", "NULL")]);
    // 出ない: 2 非表示、3 表示用画像なし、4 位置なし
    sql += &photo(2, &[("hidden", "1")]);
    sql += &photo(3, &[("display", "NULL")]);
    sql += &photo(4, &[("lat", "NULL"), ("lon", "NULL")]);
    // exact: 6 推奨は出る、7 推奨外は出ない。similar: 8・9 は推奨外でも出る
    sql += &photo(6, &[]);
    sql += &photo(7, &[]);
    sql += &photo(8, &[]);
    sql += &photo(9, &[]);
    sql += &quality(6, "md5-6", 1, "exact", 1, 1);
    sql += &quality(7, "md5-7", 1, "exact", 2, 0);
    sql += &quality(8, "md5-8", 2, "similar", 1, 1);
    sql += &quality(9, "md5-9", 2, "similar", 2, 0);
    // 10: 品質解析の md5 が写真と違う（古い結果）→ 適用されず、exact の推奨外でも出る
    // 11: 品質解析の display が写真と違う → 同じ
    sql += &photo(10, &[]);
    sql += &photo(11, &[]);
    sql += &quality(10, "md5-old", 3, "exact", 2, 0);
    sql += &format!(
        "{}UPDATE photo_quality SET display='derivatives/other.jpg' WHERE photo_id=11;\n",
        quality(11, "md5-11", 3, "exact", 3, 0)
    );
    // AI（同一場面と判定済み）: 12 推奨は出る、13 推奨外は出ない、14 推奨外でもお気に入りは出る
    sql += &ai_group("g1", "judged", 1);
    sql += &photo(12, &[]);
    sql += &photo(13, &[]);
    sql += &photo(14, &[("favorite", "1")]);
    sql += &ai_member("g1", 12, 1, 1);
    sql += &ai_member("g1", 13, 2, 0);
    sql += &ai_member("g1", 14, 3, 0);
    // AI: 15 未判定のグループ、16 同一場面ではないグループ は推奨外でも出る
    sql += &ai_group("g2", "pending", 1);
    sql += &ai_group("g3", "judged", 0);
    sql += &photo(15, &[]);
    sql += &photo(16, &[]);
    sql += &ai_member("g2", 15, 1, 0);
    sql += &ai_member("g3", 16, 1, 0);

    let list = feed(&sql).await;
    assert_eq!(ids(&list), [1, 5, 6, 8, 9, 10, 11, 12, 14, 15, 16]);
    let by_id = |id: i64| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
    };

    // 古い品質解析は未評価へ戻る
    for id in [10, 11] {
        let p = by_id(id);
        assert_eq!(p["quality_kind"], "", "{id}");
        assert_eq!(p["quality_score"], Value::Null, "{id}");
        assert_eq!(p["quality_group"], Value::Null, "{id}");
        assert_eq!(p["quality_recommended"], false, "{id}");
        assert_eq!(p["quality_role"], "", "{id}");
    }
    // 品質グループの内部値
    let p = by_id(9);
    assert_eq!(
        (
            p["quality_kind"].as_str(),
            p["quality_role"].as_str(),
            p["quality_recommended"].as_bool()
        ),
        (Some("similar"), Some("alternate"), Some(false))
    );
    assert_eq!(
        (p["quality_group"].as_i64(), p["quality_rank"].as_i64()),
        (Some(2), Some(2))
    );
    assert_eq!(by_id(6)["quality_role"], "recommended");
    // AI の内部値
    let p = by_id(14);
    assert_eq!(p["fav"], true);
    assert_eq!(p["ai_group"], "g1");
    assert_eq!(p["ai_status"], "judged");
    assert_eq!(
        (p["ai_same_scene"].as_bool(), p["ai_recommended"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(
        (p["ai_rank"].as_i64(), p["ai_local_score"].as_f64()),
        (Some(3), Some(0.25))
    );
    let p = by_id(16);
    assert_eq!(
        (p["ai_status"].as_str(), p["ai_same_scene"].as_bool()),
        (Some("judged"), Some(false))
    );
    let p = by_id(1);
    assert_eq!(p["ai_group"], "");
    assert_eq!(p["ai_status"], "");
    assert_eq!(p["ai_rank"], Value::Null);
    assert_eq!(p["ai_local_score"], Value::Null);
    assert_eq!(p["ai_same_scene"], false);
    assert_eq!(violations(&validator(), &list), Vec::<String>::new());
}

#[tokio::test]
async fn a_photo_in_two_ai_groups_comes_out_twice_like_python() {
    // Python と同じ SQL なので、複数の AI グループに入った写真は行が重なる（本番では起きない前提）
    let mut sql = photo(1, &[]);
    sql += &ai_group("g1", "pending", 1);
    sql += &ai_group("g2", "pending", 1);
    sql += &ai_member("g1", 1, 1, 0);
    sql += &ai_member("g2", 1, 1, 0);
    let list = feed(&sql).await;
    assert_eq!(ids(&list), [1, 1]);
}

#[tokio::test]
async fn order_is_by_time_with_unknown_times_last() {
    let mut sql = String::new();
    sql += &photo(1, &[("taken_at", "'2025-06-03 10:00'")]);
    sql += &photo(2, &[("taken_at", "'2025-06-01 10:00'")]);
    sql += &photo(3, &[("taken_at", "''")]);
    sql += &photo(4, &[("taken_at", "NULL")]);
    sql += &photo(5, &[("taken_at", "'2025-06-02 10:00'")]);
    sql += &photo(6, &[("taken_at", "''")]);
    let list = feed(&sql).await;
    let got = ids(&list);
    assert_eq!(&got[..3], [2, 5, 1]);
    // 時刻不明（空文字・NULL）は最後。この3枚どうしの順は固定しない
    let mut unknown = got[3..].to_vec();
    unknown.sort_unstable();
    assert_eq!(unknown, [3, 4, 6]);
    assert!(column(&list, "time")[3..].iter().all(|t| *t == ""));
    // 時刻不明はそれぞれ別の旅
    assert_eq!(trips(&list), [0, 0, 0, 1, 2, 3]);
    assert_eq!(violations(&validator(), &list), Vec::<String>::new());
}

#[tokio::test]
async fn null_taken_at_is_an_unknown_time_not_null() {
    // 現行 Python は time:null で先頭に並べる。Rust は契約どおり空文字で最後に並べる（意図した差）
    let list = feed(&(photo(1, &[]) + &photo(2, &[("taken_at", "NULL")]))).await;
    assert_eq!(ids(&list), [1, 2]);
    assert_eq!(column(&list, "time"), ["2025-06-01 10:01", ""]);
    assert_eq!(trips(&list), [0, 1]);
    assert_eq!(violations(&validator(), &list), Vec::<String>::new());
}

#[tokio::test]
async fn trip_numbers_split_at_gaps_over_48_hours() {
    let mut sql = String::new();
    sql += &photo(1, &[("taken_at", "'2025-06-01 10:00'")]);
    // ちょうど48時間後: 同じ旅
    sql += &photo(2, &[("taken_at", "'2025-06-03 10:00'")]);
    // さらに48時間と1分後: 次の旅
    sql += &photo(3, &[("taken_at", "'2025-06-05 10:01'")]);
    // 47時間59分後は同じ旅、間隔は直前の写真との差（全体の長さではない）
    sql += &photo(4, &[("taken_at", "'2025-06-07 10:00'")]);
    sql += &photo(5, &[("taken_at", "'2025-06-09 09:59'")]);
    sql += &photo(6, &[("taken_at", "'2025-06-11 10:00'")]);
    let list = feed(&sql).await;
    assert_eq!(ids(&list), [1, 2, 3, 4, 5, 6]);
    // 5→6 は48時間1分
    assert_eq!(trips(&list), [0, 0, 1, 1, 1, 2]);
    // 月・年をまたぐ間隔（閏年の2月29日を含む）
    let mut sql = String::new();
    sql += &photo(1, &[("taken_at", "'2024-02-28 23:00'")]);
    sql += &photo(2, &[("taken_at", "'2024-03-01 22:00'")]);
    sql += &photo(3, &[("taken_at", "'2024-12-31 23:59'")]);
    sql += &photo(4, &[("taken_at", "'2025-01-02 23:59'")]);
    sql += &photo(5, &[("taken_at", "'2025-01-05 00:00'")]);
    let list = feed(&sql).await;
    // 1→2: 47時間後、2→3: 長い空き、3→4: ちょうど48時間、4→5: 48時間1分
    assert_eq!(trips(&list), [0, 0, 1, 1, 2]);
}

#[tokio::test]
async fn an_unreadable_time_is_its_own_trip_and_the_next_one_compares_with_the_last_known() {
    let mut sql = String::new();
    sql += &photo(1, &[("taken_at", "'2025-06-01 10:00'")]);
    // 読めない文字列は文字列として並ぶ（1の後、3の前）。時刻はそのまま出す
    sql += &photo(2, &[("taken_at", "'2025-06-01 10:30 garbage'")]);
    sql += &photo(3, &[("taken_at", "'2025-06-02 10:00'")]);
    sql += &photo(4, &[("taken_at", "'2025-06-04 10:01'")]);
    let list = feed(&sql).await;
    assert_eq!(ids(&list), [1, 2, 3, 4]);
    assert_eq!(column(&list, "time")[1], "2025-06-01 10:30 garbage");
    // 3 は 2 ではなく、読めた直前（1）との差が24時間なので 2 と同じ旅。4 は 3 との差が48時間1分
    assert_eq!(trips(&list), [0, 1, 1, 2]);
}

#[tokio::test]
async fn title_follows_manual_then_place_then_file_name() {
    let mut sql = String::new();
    let cases: [(i64, &[(&str, &str)]); 14] = [
        // 手動/AI のタイトルが最優先
        (
            1,
            &[
                ("title", "'夏の海'"),
                ("city", "'見本市'"),
                ("place", "'サンプル町'"),
            ],
        ),
        // 市区町村＋町名。町名なしは市区町村だけ
        (2, &[("city", "'見本市'"), ("place", "'サンプル町'")]),
        (3, &[("city", "'見本市'"), ("place", "NULL")]),
        (4, &[("city", "'見本市'"), ("place", "''")]),
        // 市区町村が無ければ町名があってもファイル名（拡張子なし。複数のドットは最後だけ外す）
        (5, &[("place", "'サンプル町'"), ("name", "'a.b.c.jpg'")]),
        (6, &[("name", "'noext'")]),
        // 末尾のドット・先頭のドット・ディレクトリつき
        (7, &[("name", "'trail.'")]),
        (8, &[("name", "'sub/dir/IMG_8.JPG'")]),
        (9, &[("name", "'.hidden'")]),
        (10, &[("name", "'.hidden.png'")]),
        // 空白だけの市区町村は町名が無ければ strip して空になるのでファイル名（町名があれば町名だけ）
        (11, &[("city", "'  '"), ("place", "NULL")]),
        // 空のタイトルは未設定と同じ
        (12, &[("title", "''")]),
        (13, &[("name", "'日本語の名前.heic'")]),
        // 市区町村の前後の空白は strip するが、中の空白は残す
        (14, &[("city", "'見本市'"), ("place", "' サンプル 町 '")]),
    ];
    for (id, over) in cases {
        sql += &photo(id, over);
    }
    let list = feed(&sql).await;
    assert_eq!(
        titles(&list),
        [
            "夏の海",
            "見本市 サンプル町",
            "見本市",
            "見本市",
            "a.b.c",
            "noext",
            "trail.",
            "IMG_8",
            ".hidden",
            ".hidden",
            "IMG_0011",
            "IMG_0012",
            "日本語の名前",
            "見本市  サンプル 町",
        ]
    );
    assert_eq!(violations(&validator(), &list), Vec::<String>::new());
}

#[tokio::test]
async fn text_columns_come_out_as_empty_strings_not_null() {
    let list = feed(&photo(1, &[])).await;
    let p = &list[0];
    for key in [
        "fetched_at",
        "place",
        "pref",
        "city",
        "ai_group",
        "ai_status",
        "quality_kind",
        "quality_role",
    ] {
        assert_eq!(p[key], "", "{key}");
    }
    for key in [
        "elev",
        "quality_score",
        "quality_group",
        "quality_rank",
        "ai_rank",
        "ai_local_score",
    ] {
        assert_eq!(p[key], Value::Null, "{key}");
    }
    for key in [
        "fav",
        "inferred",
        "quality_recommended",
        "ai_same_scene",
        "ai_recommended",
    ] {
        assert_eq!(p[key], false, "{key}");
    }
}

#[tokio::test]
async fn favorite_and_inferred_are_read_like_python() {
    let mut sql = String::new();
    sql += &photo(1, &[("favorite", "NULL")]);
    sql += &photo(2, &[("favorite", "2"), ("loc_source", "'inferred'")]);
    sql += &photo(3, &[("favorite", "0"), ("loc_source", "'manual'")]);
    let list = feed(&sql).await;
    assert_eq!(column(&list, "fav"), [false, true, false]);
    assert_eq!(column(&list, "inferred"), [false, true, false]);
}

#[tokio::test]
async fn a_row_without_lon_is_left_out() {
    // lat だけある行。Python は lon:null を出すが、契約は数値なので外す（miharashi は位置なし扱い）
    let mut sql = String::new();
    sql += &photo(1, &[]);
    sql += &photo(2, &[("lon", "NULL")]);
    sql += &photo(3, &[]);
    let list = feed(&sql).await;
    assert_eq!(ids(&list), [1, 3]);
    assert_eq!(trips(&list), [0, 0]);
}

#[tokio::test]
async fn a_broken_db_is_503_with_a_fixed_body() {
    let dir = TempDir::new("broken");
    let (path, writer) = build_catalog(dir.path(), "");
    let addr = spawn(Catalog::open(&path).unwrap()).await;
    // 起動後に DB の中身が壊れた・入れ替わった状況
    writer.execute_batch("DROP TABLE photo_quality").unwrap();
    let response = get(addr, "/api/v1/photos").await;
    assert_eq!(response.status, 503);
    assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert_eq!(body.as_object().unwrap().len(), 2);
    // 内部のエラー文字列（SQLite のメッセージ・テーブル名・パス）を返さない
    let text = response.text();
    for secret in [
        "photo_quality",
        "no such table",
        "sqlite",
        "catalog.db",
        "geoframe-test",
    ] {
        assert!(!text.to_lowercase().contains(secret), "{secret}: {text}");
    }
}

#[tokio::test]
async fn waiting_too_long_for_a_connection_is_503() {
    let dir = TempDir::new("queue");
    let (path, _writer) = build_catalog(dir.path(), "");
    let catalog = Catalog::open_with_queue_timeout(&path, 1, Duration::from_millis(100)).unwrap();
    let addr = spawn(catalog.clone()).await;
    // 唯一の接続を長い読み取りで塞ぐ
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
    let response = get(addr, "/api/v1/photos").await;
    assert_eq!(response.status, 503);
    assert_eq!(response.header("content-type"), Some(JSON_UTF8));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(response.json()["ok"], false);
    holder.await.unwrap().unwrap();
    // 接続が空けば、また読める
    assert_eq!(get(addr, "/api/v1/photos").await.status, 200);
}
