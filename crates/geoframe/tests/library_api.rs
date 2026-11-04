//! `GET /api/admin/library/{trips,photos,points,calendar,places}`（ライブラリ。docs/admin-library/design.md）の結合テスト。
//!
//! 合成データは `tests/fixtures/library-catalog.sql`（冒頭のコメントに旅の構成を書いてある）。期待値はそのコメントと
//! 行から数えた値で、実装の出力を写していない。

mod common;

use std::collections::BTreeSet;

use common::{TempDir, build_library_catalog, get, request, spawn_with_env};
use geoframe::db::Catalog;
use serde_json::{Value, json};

const JSON_UTF8: &str = "application/json; charset=utf-8";

struct Server {
    _dir: TempDir,
    writer: rusqlite::Connection,
    addr: std::net::SocketAddr,
}

impl Server {
    /// フィクスチャの行に `then` のSQLを足した合成DBで起動する。
    async fn start_with(env: &[(&str, &str)], then: &str) -> Self {
        let dir = TempDir::new("library");
        let (path, writer) = build_library_catalog(dir.path(), then);
        let catalog = Catalog::open(&path).unwrap();
        let addr = spawn_with_env(dir.path(), env, catalog).await;
        Self {
            _dir: dir,
            writer,
            addr,
        }
    }

    async fn start() -> Self {
        Self::start_with(&[], "").await
    }

    async fn json(&self, target: &str) -> (u16, Value) {
        let response = get(self.addr, target).await;
        (response.status, response.json())
    }

    async fn ok(&self, target: &str) -> Value {
        let (status, body) = self.json(target).await;
        assert_eq!(status, 200, "{target}: {body}");
        assert_eq!(body["ok"], true, "{target}");
        body
    }

    async fn bad(&self, target: &str) {
        let response = get(self.addr, target).await;
        assert_eq!(response.status, 400, "{target}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "bad_params"}),
            "{target}"
        );
    }
}

/// 値をクエリに入れるためのパーセント符号化（英数字と `-_.` 以外は `%XX`）。
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn ids(body: &Value) -> Vec<i64> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect()
}

fn point_ids(body: &Value) -> Vec<i64> {
    body["features"]
        .as_array()
        .expect("features")
        .iter()
        .map(|f| f["properties"]["id"].as_i64().unwrap())
        .collect()
}

fn trip<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap_or_else(|| panic!("旅 {key} が無い: {body}"))
}

fn trip_keys(body: &Value) -> Vec<&str> {
    body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["key"].as_str().unwrap())
        .collect()
}

fn is_thumb_url(url: &str, id: i64) -> bool {
    url.strip_prefix(&format!("/thumb/{id}.jpg?v="))
        .is_some_and(|v| v.len() == 12 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

const A: &str = "2025-03-01 09:00";
const B: &str = "2025-03-12 19:30";
const C: &str = "2025-03-14 20:01";
const D: &str = "2025-05-01 12:00";
const E: &str = "2025-06-01 08:00";

// ---------------------------------------------------------------- trips

#[tokio::test]
async fn trips_split_only_where_the_gap_exceeds_48_hours() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/trips").await;
    // 新しい順。除外だけの旅 D は出ない
    assert_eq!(trip_keys(&body), [E, C, B, A]);
    assert_eq!(body["untimed"], 2);

    // 旅 A: 10 分・1 時間 30 分・8 時間・ちょうど 48 時間の空きは同じ旅。位置なしは id 4・5・6 の 3 枚
    let a = trip(&body, A);
    assert_eq!(a["start"], A);
    assert_eq!(a["end"], "2025-03-03 19:30");
    assert_eq!(a["count"], 10);
    assert_eq!(a["noloc"], 3);
    // 9 日空いた次の旅 B（先頭の id 12 は位置なし）
    let b = trip(&body, B);
    assert_eq!(
        (&b["start"], &b["end"]),
        (&json!(B), &json!("2025-03-12 20:00"))
    );
    assert_eq!((&b["count"], &b["noloc"]), (&json!(2), &json!(1)));
    // 48 時間 + 1 分の空きは別の旅 C。全部位置なし
    let c = trip(&body, C);
    assert_eq!(c["end"], "2025-03-14 20:02");
    assert_eq!((&c["count"], &c["noloc"]), (&json!(2), &json!(2)));
    // 旅 E: 同じ分の 2 枚
    let e = trip(&body, E);
    assert_eq!(
        (&e["end"], &e["count"], &e["noloc"]),
        (&json!(E), &json!(2), &json!(0))
    );
    // 旅の枚数 + 日時なし = 除外でない写真 18 枚（全 23 枚のうち除外は id 11・13・17・18・23 の 5 枚）
    let timed: i64 = body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["count"].as_i64().unwrap())
        .sum();
    assert_eq!(timed + body["untimed"].as_i64().unwrap(), 18);
}

#[tokio::test]
async fn hidden_photos_join_trips_only_when_asked() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/trips?hidden=1").await;
    assert_eq!(trip_keys(&body), [E, D, C, B, A]);
    assert_eq!(body["untimed"], 3);
    // 旅 A は id 11（03-04 08:00。位置あり）が加わる
    let a = trip(&body, A);
    assert_eq!(
        (&a["end"], &a["count"], &a["noloc"]),
        (&json!("2025-03-04 08:00"), &json!(11), &json!(3))
    );
    // 旅 B は id 13（位置あり）が加わる
    let b = trip(&body, B);
    assert_eq!((&b["count"], &b["noloc"]), (&json!(3), &json!(1)));
    // 除外だけの旅 D
    let d = trip(&body, D);
    assert_eq!(
        (&d["end"], &d["count"], &d["noloc"]),
        (&json!("2025-05-01 12:05"), &json!(2), &json!(0))
    );
    // `hidden` が 1 以外なら既定のまま
    for target in [
        "/api/admin/library/trips?hidden=0",
        "/api/admin/library/trips?hidden=2",
        "/api/admin/library/trips?hidden=true",
    ] {
        assert_eq!(
            server.ok(target).await["trips"].as_array().unwrap().len(),
            4,
            "{target}"
        );
    }
}

#[tokio::test]
async fn trip_places_are_the_three_biggest_pref_city_groups() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/trips").await;
    // 旅 A: 架空県/試験市 4（id 1, 2, 3, 7）、pref の空（海外・不明）3（id 4, 5, 6）、架空県/見本町 2、試験県/サンプル市 1（落ちる）
    assert_eq!(
        trip(&body, A)["places"],
        json!([
            {"pref": "架空県", "city": "試験市", "count": 4},
            {"pref": "", "city": "", "count": 3},
            {"pref": "架空県", "city": "見本町", "count": 2},
        ])
    );
    // 旅 B: 位置なし（pref なし）1 と 架空県/試験市 1（順は決めない）
    let places = trip(&body, B)["places"].as_array().unwrap().clone();
    assert_eq!(places.len(), 2);
    assert!(places.contains(&json!({"pref": "", "city": "", "count": 1})));
    assert!(places.contains(&json!({"pref": "架空県", "city": "試験市", "count": 1})));
    assert_eq!(
        trip(&body, C)["places"],
        json!([{"pref": "", "city": "", "count": 2}])
    );
    assert_eq!(
        trip(&body, E)["places"],
        json!([{"pref": "試験県", "city": "サンプル市", "count": 2}])
    );
    // 除外を含めると、旅 A の試験市は 5
    let hidden = server.ok("/api/admin/library/trips?hidden=1").await;
    assert_eq!(
        trip(&hidden, A)["places"][0],
        json!({"pref": "架空県", "city": "試験市", "count": 5})
    );
}

#[tokio::test]
async fn trip_cover_is_the_first_located_visible_photo_else_the_first() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/trips").await;
    let cover = |key: &str| trip(&body, key)["cover"].clone();
    // A: 先頭の id 1 は位置がある
    assert_eq!(cover(A)["id"], 1);
    assert!(is_thumb_url(cover(A)["thumb_url"].as_str().unwrap(), 1));
    // B: 先頭の id 12 は位置なしなので、位置のある id 14
    assert_eq!(cover(B)["id"], 14);
    // C: 位置のある写真が無いので先頭の id 15。表示ファイルが無い行は thumb_url が空
    assert_eq!(cover(C)["id"], 15);
    assert_eq!(cover(C)["thumb_url"], "");
    // E: 同じ分は id 順
    assert_eq!(cover(E)["id"], 19);
    assert_eq!(cover(E).as_object().unwrap().len(), 2);

    // 除外を含めても、表紙は除外でない写真を選ぶ（旅 B の id 13 は位置があるが除外）。除外だけの旅は先頭
    let hidden = server.ok("/api/admin/library/trips?hidden=1").await;
    assert_eq!(trip(&hidden, B)["cover"]["id"], 14);
    assert_eq!(trip(&hidden, D)["cover"]["id"], 17);
    assert_eq!(trip(&hidden, A)["cover"]["id"], 1);

    // 表紙の thumb_url は、同じ写真の photos の thumb_url と同じ
    let one = server.ok("/api/admin/library/photos?ids=14").await;
    assert_eq!(one["items"][0]["thumb_url"], cover(B)["thumb_url"]);
}

#[tokio::test]
async fn trips_of_an_empty_catalog() {
    let server = Server::start_with(&[], "DELETE FROM photos;").await;
    let body = server.ok("/api/admin/library/trips").await;
    assert_eq!(body, json!({"ok": true, "trips": [], "untimed": 0}));
}

/// 暦として読めない・壊れた `taken_at` の行（DB の行として足す。フィクスチャの旅は変えない）。
///   24 `2025-02-30 10:00`（形は合うが暦が無い。位置あり。旅 A の前）
///   25 `2025-03-01 24:30`（同上。旅 A の真ん中: id 6 の 03-01 19:00 と id 7 の 03-03 19:00 のあいだ。位置なし）
///   26 `x`（形が違う。位置あり）・27 `2025-06`（同上。位置なし）・28 `x`（除外）
const BROKEN_TIMES: &str = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (24, 'local', NULL, 'local/l24.jpg', 'derivatives/l24.jpg', 'IMG_L24.jpg', 'md5-l24', 240.0, '2025-02-30 10:00', 36.0, 140.0, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (25, 'drive', 'drive-l25', NULL, 'derivatives/l25.jpg', 'IMG_L25.jpg', 'md5-l25', 250.0, '2025-03-01 24:30', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (26, 'local', NULL, 'local/l26.jpg', 'derivatives/l26.jpg', 'IMG_L26.jpg', 'md5-l26', 260.0, 'x', 36.1, 140.1, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (27, 'drive', 'drive-l27', NULL, 'derivatives/l27.jpg', 'IMG_L27.jpg', 'md5-l27', 270.0, '2025-06', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (28, 'local', NULL, 'local/l28.jpg', 'derivatives/l28.jpg', 'IMG_L28.jpg', 'md5-l28', 280.0, 'x', 36.2, 140.2, 'exif', '試験区', '架空県', '試験市', 0, 1, 0);
";
const F: &str = "2025-02-30 10:00";
const G: &str = "2025-03-01 24:30";

fn total_of(body: &Value) -> i64 {
    body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["count"].as_i64().unwrap())
        .sum::<i64>()
        + body["untimed"].as_i64().unwrap()
}

#[tokio::test]
async fn trips_keep_unreadable_times_as_single_photo_trips_and_count_every_photo() {
    let server = Server::start_with(&[], BROKEN_TIMES).await;
    let body = server.ok("/api/admin/library/trips").await;
    // 新しい順。暦の無い 2 枚は 1 枚だけの旅（前後の旅に合流しない）。旅 A は G で割れ、G の次の読める行（id 7）は新しい旅
    assert_eq!(
        trip_keys(&body),
        [E, C, B, "2025-03-03 19:00", G, A, F],
        "{body}"
    );
    for key in [F, G] {
        let t = trip(&body, key);
        assert_eq!(
            (&t["start"], &t["end"], &t["count"]),
            (&json!(key), &json!(key), &json!(1)),
            "{key}"
        );
    }
    assert_eq!(trip(&body, F)["noloc"], 0);
    assert_eq!(trip(&body, F)["cover"]["id"], 24);
    assert_eq!(trip(&body, G)["noloc"], 1);
    let a1 = trip(&body, A);
    assert_eq!(
        (&a1["end"], &a1["count"]),
        (&json!("2025-03-01 19:00"), &json!(6))
    );
    let a2 = trip(&body, "2025-03-03 19:00");
    assert_eq!(
        (&a2["end"], &a2["count"]),
        (&json!("2025-03-03 19:30"), &json!(4))
    );
    // 日時なしは形の違う値（x・2025-06）を含めた 4 枚（id 21 NULL・22 空・26・27）。除外の 28 は数えない
    assert_eq!(body["untimed"], 4);
    // 旅の枚数 + 日時なし = 除外でない写真 22 枚（全 28 枚のうち除外は id 11・13・17・18・23・28 の 6 枚）
    assert_eq!(total_of(&body), 22);

    let hidden = server.ok("/api/admin/library/trips?hidden=1").await;
    assert_eq!(hidden["untimed"], 6);
    assert_eq!(total_of(&hidden), 28);
    // id 11（03-04 08:00）は G の後の旅に加わる
    assert_eq!(trip(&hidden, "2025-03-03 19:00")["count"], 5);
    // 暦の無い日時が無ければ、フィクスチャのままの旅（A は割れない）
    let plain = Server::start().await;
    let plain = plain.ok("/api/admin/library/trips").await;
    assert_eq!(total_of(&plain), 18);
}

/// 暦として読めない同じ `taken_at` の行をもう 1 枚（`BROKEN_TIMES` のあとに足す）。
///   29 `2025-02-30 10:00`（id 24 と同じ文字列。位置なし）
const SAME_BROKEN_TIME: &str = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (29, 'drive', 'drive-l29', NULL, 'derivatives/l29.jpg', 'IMG_L29.jpg', 'md5-l29', 290.0, '2025-02-30 10:00', NULL, NULL, NULL, NULL, '', '', 0, 0, 0);
";

#[tokio::test]
async fn trips_merge_photos_with_the_same_unreadable_time_into_one_trip() {
    let server = Server::start_with(&[], &format!("{BROKEN_TIMES}{SAME_BROKEN_TIME}")).await;
    let body = server.ok("/api/admin/library/trips").await;
    // 同じ文字列の 2 枚（id 24・29）は 1 つの旅。`key` が同じ旅が 2 つできない
    assert_eq!(
        trip_keys(&body),
        [E, C, B, "2025-03-03 19:00", G, A, F],
        "{body}"
    );
    let t = trip(&body, F);
    assert_eq!(
        (&t["start"], &t["end"], &t["count"], &t["noloc"]),
        (&json!(F), &json!(F), &json!(2), &json!(1))
    );
    // 表紙は位置のある最初の写真（id 24）
    assert_eq!(t["cover"]["id"], 24);
    // 旅の枚数 + 日時なし = 除外でない写真 23 枚
    assert_eq!(total_of(&body), 23);
    // 画面が旅の `key` をそのまま渡すと、旅の枚数と同じだけ返る
    let photos = server
        .ok(&format!(
            "/api/admin/library/photos?from={}&to={}",
            enc(F),
            enc(F)
        ))
        .await;
    assert_eq!(photos["total"], 2);
    assert_eq!(ids(&photos), [24, 29]);
}

// ---------------------------------------------------------------- photos

#[tokio::test]
async fn photos_in_a_range_include_both_ends() {
    let server = Server::start().await;
    async fn get_ids(server: &Server, query: &str) -> Vec<i64> {
        ids(&server
            .ok(&format!("/api/admin/library/photos?{query}"))
            .await)
    }
    // 09:10 と 10:40 の両端を含む（09:00 の id 1 と 18:40 の id 4 は含まない）
    assert_eq!(
        get_ids(&server, "from=2025-03-01%2009%3A10&to=2025-03-01%2010%3A40").await,
        [2, 3]
    );
    // `+` も空白
    assert_eq!(
        get_ids(&server, "from=2025-03-01+09:10&to=2025-03-01+09:10").await,
        [2]
    );
    // 旅 A の `start`・`end` で引くと旅の全員（除外の id 11 は含まない）
    assert_eq!(
        get_ids(&server, "from=2025-03-01%2009%3A00&to=2025-03-03%2019%3A30").await,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    );
    // 片方だけ。日時なし（NULL・空）は入らない
    assert_eq!(get_ids(&server, "to=2025-03-01%2009%3A10").await, [1, 2]);
    assert_eq!(
        get_ids(&server, "from=2025-05-01%2012%3A00").await,
        [19, 20]
    );
    // 除外を含める
    assert_eq!(
        get_ids(
            &server,
            "from=2025-05-01%2012%3A00&to=2025-05-01%2012%3A05&hidden=1"
        )
        .await,
        [17, 18]
    );
    assert_eq!(
        get_ids(&server, "from=2025-05-01%2012%3A00&to=2025-05-01%2012%3A05").await,
        Vec::<i64>::new()
    );
}

#[tokio::test]
async fn photos_in_a_bbox_include_the_edges_and_skip_unlocated() {
    let server = Server::start().await;
    let q =
        |bbox: &str, extra: &str| format!("/api/admin/library/photos?bbox={}{extra}", enc(bbox));
    // 北西・南東の端にちょうど乗る id 1（135.1, 34.1）と id 3（135.3, 34.3）を含む
    let body = server.ok(&q("135.1,34.1,135.3,34.3", "")).await;
    assert_eq!(ids(&body), [1, 2, 3]);
    assert_eq!(body["total"], 3);
    // 範囲の外の端は含まない
    assert_eq!(
        ids(&server.ok(&q("135.11,34.1,135.3,34.3", "")).await),
        [2, 3]
    );
    // 地球全体: 位置のある除外でない 11 枚だけ（位置なしは入らない）。日時なしは最後
    let all = server.ok(&q("-180,-90,180,90", "")).await;
    assert_eq!(ids(&all), [1, 2, 3, 7, 8, 9, 10, 14, 19, 20, 21]);
    // 除外を含めると 16 枚
    let all = server.ok(&q("-180,-90,180,90", "&hidden=1")).await;
    assert_eq!(all["total"], 16);
    // 経度 138〜140・緯度 34〜36
    assert_eq!(ids(&server.ok(&q("138,34,140,36", "")).await), [10, 19, 20]);
    assert_eq!(
        ids(&server.ok(&q("138,34,140,36", "&hidden=1")).await),
        [10, 17, 18, 19, 20]
    );
    // 期間と組み合わせ
    assert_eq!(
        ids(&server
            .ok(&q("138,34,140,36", "&from=2025-06-01%2000%3A00"))
            .await),
        [19, 20]
    );
}

#[tokio::test]
async fn photos_by_ids_are_capped_at_200_before_deduplication() {
    let server = Server::start().await;
    let by = |list: &str| format!("/api/admin/library/photos?ids={list}");
    // 並びは要求の順でなく日時の順。重複は 1 つ。除外は既定では出ない
    let body = server.ok(&by("20,1,3,3,11,999")).await;
    assert_eq!(ids(&body), [1, 3, 20]);
    assert_eq!(body["total"], 3);
    assert_eq!(
        ids(&server.ok(&format!("{}&hidden=1", by("11,1"))).await),
        [1, 11]
    );
    // 200 個はよい。201 個は同じ ID の繰り返しでも 400（重複を除く前の個数）
    let n = |count: usize| {
        (1..=count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let ok = server.ok(&by(&n(200))).await;
    assert_eq!(ok["total"], 18);
    server.bad(&by(&n(201))).await;
    server.bad(&by(&vec!["1"; 201].join(","))).await;
    assert_eq!(server.ok(&by(&vec!["1"; 200].join(","))).await["total"], 1);
    // 整数でない・空の要素
    for bad in [
        "1,a",
        "1,,2",
        "1,",
        ",1",
        "1.5",
        "1;2",
        "0x1",
        "99999999999999999999",
    ] {
        server.bad(&by(bad)).await;
    }
}

#[tokio::test]
async fn photos_by_pref_and_city() {
    let server = Server::start().await;
    let p = |query: &str| format!("/api/admin/library/photos?{query}");
    let pref = enc("架空県");
    let body = server.ok(&p(&format!("pref={pref}"))).await;
    assert_eq!(ids(&body), [1, 2, 3, 7, 8, 9, 14, 21]);
    // 除外を含める（日時なしの id 21・23 は最後、id 順）
    assert_eq!(
        ids(&server.ok(&p(&format!("pref={pref}&hidden=1"))).await),
        [1, 2, 3, 7, 8, 9, 11, 13, 14, 21, 23]
    );
    let city = enc("見本町");
    assert_eq!(ids(&server.ok(&p(&format!("city={city}"))).await), [8, 9]);
    assert_eq!(
        ids(&server.ok(&p(&format!("city={city}&hidden=1"))).await),
        [8, 9, 13, 23]
    );
    // 両方
    let both = format!("pref={}&city={}", enc("試験県"), enc("サンプル市"));
    assert_eq!(ids(&server.ok(&p(&both)).await), [10, 19, 20]);
    let none = format!("pref={pref}&city={}", enc("サンプル市"));
    assert_eq!(server.ok(&p(&none)).await["total"], 0);
    // `=` の比較（部分一致・ワイルドカードは効かない）
    for value in ["架空", "%", "架空県%", "_空県"] {
        assert_eq!(
            server.ok(&p(&format!("pref={}", enc(value)))).await["total"],
            0,
            "{value}"
        );
    }
    // 64 文字まではよい。65 文字は 400
    server
        .ok(&p(&format!("city={}", enc(&"あ".repeat(64)))))
        .await;
    server
        .bad(&p(&format!("city={}", enc(&"あ".repeat(65)))))
        .await;
    server
        .bad(&p(&format!("pref={}", enc(&"a".repeat(65)))))
        .await;
}

#[tokio::test]
async fn photos_without_a_time_come_from_untimed() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/photos?untimed=1").await;
    assert_eq!(ids(&body), [21, 22]);
    assert_eq!(
        ids(&server
            .ok("/api/admin/library/photos?untimed=1&hidden=1")
            .await),
        [21, 22, 23]
    );
    // 日時なしを日時で絞ると空
    let none = server
        .ok("/api/admin/library/photos?untimed=1&from=2025-01-01%2000%3A00")
        .await;
    assert_eq!(none["total"], 0);
    // `untimed` は `1` のときだけ絞り込みになる
    server.bad("/api/admin/library/photos?untimed=0").await;
    server.bad("/api/admin/library/photos?untimed=true").await;
}

#[tokio::test]
async fn photos_treat_the_timed_shape_alike_in_ranges_and_untimed() {
    let server = Server::start_with(&[], BROKEN_TIMES).await;
    async fn get_ids(server: &Server, query: &str) -> Vec<i64> {
        ids(&server
            .ok(&format!("/api/admin/library/photos?{query}"))
            .await)
    }
    // 暦の無い日時も、旅の `key`・`end` をそのまま `from`・`to` に渡せば、その 1 枚を引ける
    assert_eq!(
        get_ids(&server, &format!("from={}&to={}", enc(F), enc(F))).await,
        [24]
    );
    assert_eq!(
        get_ids(&server, &format!("from={}&to={}", enc(G), enc(G))).await,
        [25]
    );
    // 形で揃える: 2 月 1 日〜3 月 1 日 0 時の範囲に 2025-02-30 10:00 は入る
    assert_eq!(
        get_ids(&server, "from=2025-02-01%2000%3A00&to=2025-03-01%2000%3A00").await,
        [24]
    );
    // 日時なしは NULL・空・形の違う値（x・2025-06）。日時なしは並びの最後で、`2025-02-30 10:00` は含まない。
    // 空・NULL → `2025-06` → `x` の順（`COALESCE(taken_at,'')`、同じなら id）
    assert_eq!(get_ids(&server, "untimed=1").await, [21, 22, 27, 26]);
    assert_eq!(
        get_ids(&server, "untimed=1&hidden=1").await,
        [21, 22, 23, 27, 26, 28]
    );
    // 日時の範囲には、形の違う値（x・2025-06）や空は入らない
    assert_eq!(
        get_ids(&server, "from=2025-01-01%2000%3A00&to=2099-01-01%2000%3A00").await,
        [
            24, 1, 2, 3, 4, 5, 6, 25, 7, 8, 9, 10, 12, 14, 15, 16, 19, 20
        ]
    );
}

#[tokio::test]
async fn photos_need_a_narrowing_filter_and_a_valid_shape() {
    let server = Server::start().await;
    for target in [
        "/api/admin/library/photos",
        "/api/admin/library/photos?hidden=1",
        "/api/admin/library/photos?page=2&per=10",
        "/api/admin/library/photos?from=",
        "/api/admin/library/photos?foo=1",
    ] {
        server.bad(target).await;
    }
    for target in [
        // 日時（形だけを見る。暦の正しさは見ないので、2025-02-30 10:00・25:00・10:60 は通る）
        "from=2025-06-01",
        "from=2025-6-1%2010%3A00",
        "from=2025-06-01T10%3A00",
        "from=2025-06-01%2010%3A00%3A00",
        "from=%EF%BC%92025-06-01%2010%3A00",
        "to=x",
        "to=2025-06-01%2010%3A00%20",
        // from > to
        "from=2025-06-01%2010%3A01&to=2025-06-01%2010%3A00",
        // bbox
        "bbox=1,2,3",
        "bbox=1,2,3,4,5",
        "bbox=a,2,3,4",
        "bbox=nan,0,1,1",
        "bbox=0,0,inf,1",
        "bbox=2,0,1,1",
        "bbox=0,2,1,1",
        "bbox=-180.5,0,0,1",
        "bbox=0,0,180.5,1",
        "bbox=0,-90.5,1,0",
        "bbox=0,0,1,90.5",
        // 整数でない
        "ids=a",
        "page=x&untimed=1",
        "per=x&untimed=1",
        "page=1.5&untimed=1",
        // 絞り込みの 1 つが壊れていれば、ほかが正しくても 400
        "untimed=1&bbox=1,2,3",
        "untimed=1&from=bad",
        "pref=%E6%9E%B6%E7%A9%BA%E7%9C%8C&ids=a",
    ] {
        server
            .bad(&format!("/api/admin/library/photos?{target}"))
            .await;
    }
}

#[tokio::test]
async fn photos_page_through_with_clamping() {
    let server = Server::start().await;
    let base = "/api/admin/library/photos?from=2025-01-01%2000%3A00&to=2025-12-31%2023%3A59";
    // 除外でない日時のある写真は 16 枚
    let first = server.ok(&format!("{base}&per=5")).await;
    assert_eq!(
        (
            first["page"].as_i64(),
            first["per"].as_i64(),
            first["pages"].as_i64(),
            first["total"].as_i64()
        ),
        (Some(1), Some(5), Some(4), Some(16))
    );
    assert_eq!(ids(&first), [1, 2, 3, 4, 5]);
    assert_eq!(
        ids(&server.ok(&format!("{base}&per=5&page=3")).await),
        [12, 14, 15, 16, 19]
    );
    // 最後のページは端数
    let last = server.ok(&format!("{base}&per=5&page=4")).await;
    assert_eq!(ids(&last), [20]);
    // page は 1〜pages に丸める
    for (query, page) in [("page=99", 4), ("page=0", 1), ("page=-3", 1), ("page=4", 4)] {
        let body = server.ok(&format!("{base}&per=5&{query}")).await;
        assert_eq!(body["page"], page, "{query}");
    }
    // 既定の per は 60。per は 1〜200 に丸める
    let default = server.ok(base).await;
    assert_eq!(
        (&default["per"], &default["pages"]),
        (&json!(60), &json!(1))
    );
    let zero = server.ok(&format!("{base}&per=0")).await;
    assert_eq!(
        (
            &zero["per"],
            &zero["pages"],
            zero["items"].as_array().unwrap().len()
        ),
        (&json!(1), &json!(16), 1)
    );
    let neg = server.ok(&format!("{base}&per=-7")).await;
    assert_eq!(neg["per"], 1);
    let big = server.ok(&format!("{base}&per=1000")).await;
    assert_eq!((&big["per"], &big["pages"]), (&json!(200), &json!(1)));
    assert_eq!(big["items"].as_array().unwrap().len(), 16);
    // 空の結果は pages 1・page 1
    let empty = server
        .ok("/api/admin/library/photos?from=2030-01-01%2000%3A00&page=5")
        .await;
    assert_eq!(
        (
            &empty["page"],
            &empty["pages"],
            &empty["total"],
            empty["items"].as_array().unwrap().len()
        ),
        (&json!(1), &json!(1), &json!(0), 0)
    );
}

#[tokio::test]
async fn library_photo_has_the_documented_keys_and_values() {
    let server = Server::start().await;
    let body = server
        .ok("/api/admin/library/photos?ids=2,3,4,11,15&hidden=1")
        .await;
    assert_eq!(ids(&body), [2, 3, 4, 11, 15]);
    let keys: BTreeSet<&str> = [
        "id",
        "name",
        "thumb",
        "thumb_url",
        "taken_at",
        "lat",
        "lon",
        "loc_source",
        "place",
        "pref",
        "city",
        "favorite",
        "hidden",
        "rotation",
        "source",
    ]
    .into_iter()
    .collect();
    for item in body["items"].as_array().unwrap() {
        let have: BTreeSet<&str> = item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(have, keys, "id {}", item["id"]);
    }
    let by_id = |id: i64| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    let p3 = by_id(3);
    assert_eq!(p3["name"], "IMG_L03.jpg");
    assert_eq!(p3["thumb"], "/derivatives/l03.jpg");
    assert!(is_thumb_url(p3["thumb_url"].as_str().unwrap(), 3));
    assert_eq!(p3["taken_at"], "2025-03-01 10:40");
    assert_eq!((&p3["lat"], &p3["lon"]), (&json!(34.3), &json!(135.3)));
    assert_eq!(p3["loc_source"], "exif");
    assert_eq!(p3["place"], "試験区");
    assert_eq!(
        (&p3["pref"], &p3["city"]),
        (&json!("架空県"), &json!("試験市"))
    );
    assert_eq!(
        (&p3["favorite"], &p3["hidden"], &p3["rotation"]),
        (&json!(false), &json!(false), &json!(90))
    );
    assert_eq!(p3["source"], "local");
    assert_eq!(by_id(2)["favorite"], true);
    assert_eq!(by_id(2)["source"], "drive");
    // 除外
    assert_eq!(by_id(11)["hidden"], true);
    // 位置なし: 座標は null、文字列は空
    let p4 = by_id(4);
    assert_eq!((&p4["lat"], &p4["lon"]), (&Value::Null, &Value::Null));
    assert_eq!(
        (&p4["loc_source"], &p4["place"], &p4["pref"], &p4["city"]),
        (&json!(""), &json!(""), &json!(""), &json!(""))
    );
    // 表示ファイルが無く derivative も無い行は thumb も thumb_url も空
    let p15 = by_id(15);
    assert_eq!((&p15["thumb"], &p15["thumb_url"]), (&json!(""), &json!("")));
    // 世代は写真ごとに違う（display・md5・mtime から作る）
    assert_ne!(p3["thumb_url"], by_id(2)["thumb_url"]);
}

// ---------------------------------------------------------------- points

#[tokio::test]
async fn points_are_a_geojson_of_located_photos_newest_first() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/points").await;
    assert_eq!(body["type"], "FeatureCollection");
    assert_eq!(body["truncated"], false);
    assert_eq!(body["total"], 11);
    // 新しい順（同じ分は id の大きい方が先）。日時なしの id 21 は最後。位置なしは出ない
    assert_eq!(point_ids(&body), [20, 19, 14, 10, 9, 8, 7, 3, 2, 1, 21]);
    let feature = &body["features"][9];
    assert_eq!(
        feature,
        &json!({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [135.1, 34.1]},
            "properties": {"id": 1},
        })
    );
    // 除外を含めると 16 点
    let hidden = server.ok("/api/admin/library/points?hidden=1").await;
    assert_eq!(hidden["total"], 16);
    // 新しい順。日時なしは最後で、id の大きい方が先
    assert_eq!(
        point_ids(&hidden),
        [20, 19, 18, 17, 14, 13, 11, 10, 9, 8, 7, 3, 2, 1, 23, 21]
    );
}

#[tokio::test]
async fn points_in_a_range_skip_untimed_photos() {
    let server = Server::start().await;
    // 03-03 19:00〜03-12 20:00。id 7〜10 と 14（除外の 11・13 は出ない）
    let body = server
        .ok("/api/admin/library/points?from=2025-03-03%2019%3A00&to=2025-03-12%2020%3A00")
        .await;
    assert_eq!(point_ids(&body), [14, 10, 9, 8, 7]);
    assert_eq!(body["total"], 5);
    let with_hidden = server
        .ok("/api/admin/library/points?from=2025-03-03%2019%3A00&to=2025-03-12%2020%3A00&hidden=1")
        .await;
    assert_eq!(point_ids(&with_hidden), [14, 13, 11, 10, 9, 8, 7]);
    // 片方だけ。日時なしの id 21 は入らない
    let from = server
        .ok("/api/admin/library/points?from=2025-06-01%2000%3A00")
        .await;
    assert_eq!(point_ids(&from), [20, 19]);
    let to = server
        .ok("/api/admin/library/points?to=2025-03-01%2009%3A10")
        .await;
    assert_eq!(point_ids(&to), [2, 1]);
    // 形の誤り
    for bad in [
        "from=x",
        "to=2025-06-01",
        "from=2025-06-01%2010%3A01&to=2025-06-01%2010%3A00",
    ] {
        server
            .bad(&format!("/api/admin/library/points?{bad}"))
            .await;
    }
    // 空の結果
    let empty = server
        .ok("/api/admin/library/points?from=2030-01-01%2000%3A00")
        .await;
    assert_eq!(
        empty,
        json!({"ok": true, "truncated": false, "total": 0, "type": "FeatureCollection", "features": []})
    );
}

#[tokio::test]
async fn points_use_the_same_timed_shape_and_accept_unreadable_dates() {
    let server = Server::start_with(&[], BROKEN_TIMES).await;
    // 位置のある、形は合うが暦の無い日時（id 24）も範囲に入る。旅の `key` をそのまま渡せる
    let body = server
        .ok(&format!(
            "/api/admin/library/points?from={}&to={}",
            enc(F),
            enc(F)
        ))
        .await;
    assert_eq!(point_ids(&body), [24]);
    assert_eq!(body["total"], 1);
    // 暦の無い id 25 は位置なしなので点にならない
    let g = server
        .ok(&format!(
            "/api/admin/library/points?from={}&to={}",
            enc(G),
            enc(G)
        ))
        .await;
    assert_eq!(point_ids(&g), Vec::<i64>::new());
    // 2 月 1 日〜3 月 1 日 0 時に id 24 は入る
    let feb = server
        .ok("/api/admin/library/points?from=2025-02-01%2000%3A00&to=2025-03-01%2000%3A00")
        .await;
    assert_eq!(point_ids(&feb), [24]);
    // 範囲なしでは、日時なし（形の違う `x` の id 26 と NULL の id 21）が最後。`x` は NULL より後ろの値なので新しい順では先
    let all = server.ok("/api/admin/library/points").await;
    assert_eq!(
        point_ids(&all),
        [20, 19, 14, 10, 9, 8, 7, 3, 2, 1, 24, 26, 21]
    );
    // 範囲を付けると、形の違う値（x）は入らない
    let ranged = server
        .ok("/api/admin/library/points?from=2025-01-01%2000%3A00")
        .await;
    assert_eq!(point_ids(&ranged), [20, 19, 14, 10, 9, 8, 7, 3, 2, 1, 24]);
}

#[tokio::test]
async fn points_over_the_limit_drop_the_oldest_and_say_so() {
    // 2024 年の 20,001 点（位置あり）を足す。フィクスチャの 2025 年の点より古い
    let bulk = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<20001)
        INSERT INTO photos(id, source, name, taken_at, lat, lon)
          SELECT 1000+i, 'local', 'bulk'||i,
                 strftime('%Y-%m-%d %H:%M', '2024-01-01 00:00', '+'||i||' minutes'), 35.0, 136.0
            FROM n;";
    let server = Server::start_with(&[], bulk).await;
    let body = server.ok("/api/admin/library/points").await;
    // 位置のある写真は 20,001 + 11 = 20,012
    assert_eq!(body["total"], 20012);
    assert_eq!(body["truncated"], true);
    let got = point_ids(&body);
    assert_eq!(got.len(), 20000);
    // 新しい 10 枚（フィクスチャの日時あり）が先頭、その後ろに 2024 年の新しい順
    assert_eq!(got[..10], [20, 19, 14, 10, 9, 8, 7, 3, 2, 1]);
    assert_eq!(got[10], 21001);
    // 切られるのは、日時なしの id 21 と、2024 年の古い 11 枚（id 1001〜1011）
    let have: BTreeSet<i64> = got.into_iter().collect();
    assert!(!have.contains(&21));
    assert!(!have.contains(&1011));
    assert!(have.contains(&1012));
    // 範囲で絞れば切られない
    let narrow = server
        .ok("/api/admin/library/points?from=2025-01-01%2000%3A00")
        .await;
    assert_eq!(
        (&narrow["truncated"], &narrow["total"]),
        (&json!(false), &json!(10))
    );
}

// ---------------------------------------------------------------- calendar・places

/// `calendar` の年（`years[].year`）の並び。
fn years_of(body: &Value) -> Vec<&str> {
    body["years"]
        .as_array()
        .unwrap()
        .iter()
        .map(|y| y["year"].as_str().unwrap())
        .collect()
}

/// `calendar` の `year` の月ごとの件数 `[(月, 件数)]`。
fn months_of(body: &Value, year: &str) -> Vec<(String, i64)> {
    body["years"]
        .as_array()
        .unwrap()
        .iter()
        .find(|y| y["year"] == year)
        .unwrap_or_else(|| panic!("年 {year} が無い: {body}"))["months"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["month"].as_str().unwrap().to_owned(),
                m["count"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn year_count(body: &Value, year: &str) -> i64 {
    body["years"]
        .as_array()
        .unwrap()
        .iter()
        .find(|y| y["year"] == year)
        .unwrap()["count"]
        .as_i64()
        .unwrap()
}

fn m(month: &str, count: i64) -> (String, i64) {
    (month.to_owned(), count)
}

#[tokio::test]
async fn calendar_counts_each_year_and_month_of_the_timed_photos() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/calendar").await;
    // フィクスチャの行から数える。除外でない日時ありは 2025-03 が id 1〜10・12・14・15・16 の 14 枚、2025-06 が id 19・20 の 2 枚。
    // 除外の id 11・13（03）と 17・18（05）は数えない。日時なしは id 21・22（除外の 23 は数えない）
    assert_eq!(years_of(&body), ["2025"]);
    assert_eq!(year_count(&body, "2025"), 16);
    assert_eq!(months_of(&body, "2025"), [m("03", 14), m("06", 2)]);
    assert_eq!(body["untimed"], 2);

    // 除外を含める: 03 は +11・13 で 16、05 は 17・18 で 2、日時なしは +23
    let hidden = server.ok("/api/admin/library/calendar?hidden=1").await;
    assert_eq!(year_count(&hidden, "2025"), 20);
    assert_eq!(
        months_of(&hidden, "2025"),
        [m("03", 16), m("05", 2), m("06", 2)]
    );
    assert_eq!(hidden["untimed"], 3);
    // `1` 以外は既定
    let other = server.ok("/api/admin/library/calendar?hidden=0").await;
    assert_eq!(other, body);
    // 年と月の値は文字列（4 桁・2 桁）
    let first = &body["years"][0];
    assert!(first["year"].is_string() && first["months"][0]["month"].is_string());
}

#[tokio::test]
async fn calendar_orders_years_newest_first_and_months_oldest_first() {
    let extra = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (31, 'drive', 'drive-l31', NULL, 'derivatives/l31.jpg', 'IMG_L31.jpg', 'md5-l31', 310.0, '2023-12-31 23:59', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (32, 'drive', 'drive-l32', NULL, 'derivatives/l32.jpg', 'IMG_L32.jpg', 'md5-l32', 320.0, '2024-09-09 09:09', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (33, 'drive', 'drive-l33', NULL, 'derivatives/l33.jpg', 'IMG_L33.jpg', 'md5-l33', 330.0, '2024-01-01 00:00', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (34, 'drive', 'drive-l34', NULL, 'derivatives/l34.jpg', 'IMG_L34.jpg', 'md5-l34', 340.0, '2024-09-30 12:00', NULL, NULL, NULL, NULL, '', '', 0, 0, 0);
";
    let server = Server::start_with(&[], extra).await;
    let body = server.ok("/api/admin/library/calendar").await;
    assert_eq!(years_of(&body), ["2025", "2024", "2023"]);
    assert_eq!(months_of(&body, "2024"), [m("01", 1), m("09", 2)]);
    assert_eq!(year_count(&body, "2024"), 3);
    assert_eq!(months_of(&body, "2023"), [m("12", 1)]);
    assert_eq!(year_count(&body, "2023"), 1);
}

#[tokio::test]
async fn calendar_shows_unreadable_dates_as_they_are_and_skips_misshapen_ones() {
    let server = Server::start_with(&[], BROKEN_TIMES).await;
    let body = server.ok("/api/admin/library/calendar").await;
    // 暦として読めない `2025-02-30 10:00`（id 24）・`2025-03-01 24:30`（id 25）も形が合えばそのまま数える。
    // 形の違う `x`・`2025-06`（id 26・27）は日時なし（除外の 28 を除いて 4 枚）
    assert_eq!(
        months_of(&body, "2025"),
        [m("02", 1), m("03", 15), m("06", 2)]
    );
    assert_eq!(year_count(&body, "2025"), 18);
    assert_eq!(body["untimed"], 4);
    // 年の件数は月の件数の合計、年の合計 + 日時なしは除外でない 22 枚
    assert_eq!(
        year_count(&body, "2025") + body["untimed"].as_i64().unwrap(),
        22
    );
}

/// 暦として読めない月末の行（`calendar` は 03 月に数えるので、月の格子の範囲にも入らなければならない）。
const LATE_BROKEN_TIME: &str = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (29, 'local', NULL, 'local/l29.jpg', 'derivatives/l29.jpg', 'IMG_L29.jpg', 'md5-l29', 290.0, '2025-03-31 24:30', 36.0, 140.0, 'exif', '試験区', '架空県', '試験市', 0, 0, 0);
";

#[tokio::test]
async fn calendar_month_counts_match_the_grid_range_even_for_unreadable_dates() {
    let server = Server::start_with(&[], &format!("{BROKEN_TIMES}{LATE_BROKEN_TIME}")).await;
    let cal = server.ok("/api/admin/library/calendar").await;
    let march = months_of(&cal, "2025")
        .into_iter()
        .find(|(month, _)| month == "03")
        .unwrap()
        .1;
    let grid = server
        .ok("/api/admin/library/photos?from=2025-03-00%2000%3A00&to=2025-03-99%2099%3A99&per=200")
        .await;
    assert_eq!(grid["total"], march);
    assert!(ids(&grid).contains(&29), "31 日 24 時台の行も入る");
    let year = server
        .ok("/api/admin/library/photos?from=2025-00-00%2000%3A00&to=2025-99-99%2099%3A99&per=200")
        .await;
    assert_eq!(year["total"], year_count(&cal, "2025"));
}

#[tokio::test]
async fn calendar_of_an_empty_catalog() {
    let server = Server::start_with(&[], "DELETE FROM photos;").await;
    let body = server.ok("/api/admin/library/calendar").await;
    assert_eq!(body, json!({"ok": true, "years": [], "untimed": 0}));
}

/// `places` の都道府県の `(都道府県, 件数, [(市区町村, 件数)])`。
type PrefRow = (String, i64, Vec<(String, i64)>);
fn prefs_of(body: &Value) -> Vec<PrefRow> {
    body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["pref"].as_str().unwrap().to_owned(),
                p["count"].as_i64().unwrap(),
                p["cities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| {
                        (
                            c["city"].as_str().unwrap().to_owned(),
                            c["count"].as_i64().unwrap(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

fn pref_row(pref: &str, count: i64, cities: &[(&str, i64)]) -> PrefRow {
    (
        pref.to_owned(),
        count,
        cities.iter().map(|(c, n)| ((*c).to_owned(), *n)).collect(),
    )
}

#[tokio::test]
async fn places_count_prefs_and_cities_and_abroad() {
    let server = Server::start().await;
    let body = server.ok("/api/admin/library/places").await;
    // フィクスチャの行から数える（除外でないもの）。架空県: 試験市は id 1・2・3・7・14・21 の 6、見本町は 8・9 の 2 で 8 枚。
    // 試験県: サンプル市は 10・19・20 の 3。海外・不明（pref が NULL か空）は 4・5・6・12・15・16・22 の 7
    assert_eq!(
        prefs_of(&body),
        [
            pref_row("架空県", 8, &[("試験市", 6), ("見本町", 2)]),
            pref_row("試験県", 3, &[("サンプル市", 3)]),
        ]
    );
    assert_eq!(body["abroad"], 7);

    // 除外を含める: 架空県は +11・13・23（試験市 7、見本町 2+2=4）で 11、試験県は +17・18 で 5
    let hidden = server.ok("/api/admin/library/places?hidden=1").await;
    assert_eq!(
        prefs_of(&hidden),
        [
            pref_row("架空県", 11, &[("試験市", 7), ("見本町", 4)]),
            pref_row("試験県", 5, &[("サンプル市", 5)]),
        ]
    );
    assert_eq!(hidden["abroad"], 7);
    // 全部で 23 枚（除外を含める）／18 枚（除外を除く）
    let total =
        |b: &Value| prefs_of(b).iter().map(|p| p.1).sum::<i64>() + b["abroad"].as_i64().unwrap();
    assert_eq!(total(&body), 18);
    assert_eq!(total(&hidden), 23);
    assert!(body.get("noloc").is_none());
}

#[tokio::test]
async fn places_sort_by_count_then_name_and_keep_an_empty_city() {
    // 同数（2）の「あ県」「い県」、市区町村が空・NULL の行（どちらも city が空の 1 項目に）、
    // 位置があるのに pref が空の行（海外・不明）
    let extra = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (31, 'drive', 'drive-l31', NULL, 'derivatives/l31.jpg', 'IMG_L31.jpg', 'md5-l31', 310.0, '2024-01-01 00:00', 1.0, 2.0, 'exif', '', 'い県', '', 0, 0, 0),
  (32, 'drive', 'drive-l32', NULL, 'derivatives/l32.jpg', 'IMG_L32.jpg', 'md5-l32', 320.0, '2024-01-02 00:00', 1.0, 2.0, 'exif', '', 'い県', NULL, 0, 0, 0),
  (33, 'drive', 'drive-l33', NULL, 'derivatives/l33.jpg', 'IMG_L33.jpg', 'md5-l33', 330.0, '2024-01-03 00:00', 1.0, 2.0, 'exif', '', 'あ県', 'う市', 0, 0, 0),
  (34, 'drive', 'drive-l34', NULL, 'derivatives/l34.jpg', 'IMG_L34.jpg', 'md5-l34', 340.0, '2024-01-04 00:00', 1.0, 2.0, 'exif', '', 'あ県', 'え市', 0, 0, 0),
  (35, 'drive', 'drive-l35', NULL, 'derivatives/l35.jpg', 'IMG_L35.jpg', 'md5-l35', 350.0, '2024-01-05 00:00', 1.0, 2.0, 'exif', '', '', '', 0, 0, 0);
";
    let server = Server::start_with(&[], extra).await;
    let body = server.ok("/api/admin/library/places").await;
    assert_eq!(
        prefs_of(&body),
        [
            pref_row("架空県", 8, &[("試験市", 6), ("見本町", 2)]),
            pref_row("試験県", 3, &[("サンプル市", 3)]),
            // 同数は名前の順（あ < い）。市区町村も同数は名前の順（う < え）
            pref_row("あ県", 2, &[("う市", 1), ("え市", 1)]),
            pref_row("い県", 2, &[("", 2)]),
        ]
    );
    // 位置があっても pref が空なら海外・不明（フィクスチャの 7 + id 35）
    assert_eq!(body["abroad"], 8);
}

#[tokio::test]
async fn places_of_an_empty_catalog() {
    let server = Server::start_with(&[], "DELETE FROM photos;").await;
    let body = server.ok("/api/admin/library/places").await;
    // 旧形式のフィールドは変わらず、新しいフィールドは空（分類の表が無い DB なので `classification_available` は偽）
    assert_eq!(body["prefs"], json!([]));
    assert_eq!(body["abroad"], 0);
    assert_eq!(body["countries"], json!([]));
    assert_eq!(body["classification_available"], false);
    assert_eq!(body["unknown"]["unclassified"]["count"], 0);
    assert_eq!(body["unknown"]["no_location"]["count"], 0);
}

#[tokio::test]
async fn photos_abroad_are_the_ones_without_a_pref() {
    let server = Server::start().await;
    let p = |query: &str| format!("/api/admin/library/photos?{query}");
    // pref が NULL か空: 4・5・6・12・15・16・22（除外を含めても同じ）
    let body = server.ok(&p("abroad=1")).await;
    assert_eq!(ids(&body), [4, 5, 6, 12, 15, 16, 22]);
    assert_eq!(body["total"], 7);
    assert_eq!(
        ids(&server.ok(&p("abroad=1&hidden=1")).await),
        [4, 5, 6, 12, 15, 16, 22]
    );
    // `places` の abroad と同じ件数
    let places = server.ok("/api/admin/library/places").await;
    assert_eq!(places["abroad"], body["total"]);
    // 期間と合わせられる（旅 A の範囲では 4・5・6）
    assert_eq!(
        ids(&server
            .ok(&p(
                "abroad=1&from=2025-03-01%2009%3A00&to=2025-03-03%2019%3A30"
            ))
            .await),
        [4, 5, 6]
    );
    // `1` 以外は絞り込みにならない
    server.bad(&p("abroad=0")).await;
    server.bad(&p("abroad=yes")).await;
    // pref・city とは同時に受けない
    server
        .bad(&p(&format!("abroad=1&pref={}", enc("架空県"))))
        .await;
    server
        .bad(&p(&format!("abroad=1&city={}", enc("試験市"))))
        .await;
    server
        .bad(&p(&format!(
            "abroad=1&pref={}&city={}",
            enc("架空県"),
            enc("試験市")
        )))
        .await;
    // 空の値は parse_qs が捨てるので、無いのと同じ
    assert_eq!(ids(&server.ok(&p("abroad=1&pref=")).await).len(), 7);
    // 年・月の格子（from/to）の形が通り、calendar の件数と合う
    let year = server
        .ok(&p(
            "from=2025-00-00%2000%3A00&to=2025-99-99%2099%3A99&per=200",
        ))
        .await;
    assert_eq!(year["total"], 16);
    let month = server
        .ok(&p(
            "from=2025-03-00%2000%3A00&to=2025-03-99%2099%3A99&per=200",
        ))
        .await;
    assert_eq!(month["total"], 14);
}

// ---------------------------------------------------------------- 代表の写真・範囲・期間・文字検索（ライブラリの見直し）

/// 代表の写真・範囲の試験の追加行。年・都道府県ごとの代表（除外でない → 位置あり → 新しい → ID が大きい）を手で数える。
///   2024 / ア県: 40 除外・位置あり（ア市・07-01）、41 位置なし（ア市・06-01）、42 位置あり（エ市・05-01）
///   2023 / イ県: 43・44・45 は全部位置なし（イ市）。44・45 は同じ分（→ 大きい ID の 45）。45 は表示ファイルが無い
///   2022 / ウ県: 46 除外・位置あり、47 除外・位置なし（除外だけの組）
///   2021 / 海外: 48 位置あり（pref なし。パリの近く）
const COVER_EXTRA: &str = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (40, 'local', NULL, 'local/l40.jpg', 'derivatives/l40.jpg', 'IMG_L40.jpg', 'md5-l40', 400.0, '2024-07-01 10:00', 30.0, 130.0, 'exif', '架', 'ア県', 'ア市', 0, 1, 0),
  (41, 'drive', 'drive-l41', NULL, 'derivatives/l41.jpg', 'IMG_L41.jpg', 'md5-l41', 410.0, '2024-06-01 12:00', NULL, NULL, NULL, NULL, 'ア県', 'ア市', 0, 0, 0),
  (42, 'local', NULL, 'local/l42.jpg', 'derivatives/l42.jpg', 'IMG_L42.jpg', 'md5-l42', 420.0, '2024-05-01 10:00', 30.1, 130.1, 'exif', '架', 'ア県', 'エ市', 0, 0, 0),
  (43, 'drive', 'drive-l43', NULL, 'derivatives/l43.jpg', 'IMG_L43.jpg', 'md5-l43', 430.0, '2023-09-01 10:00', NULL, NULL, NULL, NULL, 'イ県', 'イ市', 0, 0, 0),
  (44, 'drive', 'drive-l44', NULL, 'derivatives/l44.jpg', 'IMG_L44.jpg', 'md5-l44', 440.0, '2023-10-01 10:00', NULL, NULL, NULL, NULL, 'イ県', 'イ市', 0, 0, 0),
  (45, 'drive', 'drive-l45', NULL, NULL, 'IMG_L45.jpg', 'md5-l45', 450.0, '2023-10-01 10:00', NULL, NULL, NULL, NULL, 'イ県', 'イ市', 0, 0, 0),
  (46, 'local', NULL, 'local/l46.jpg', 'derivatives/l46.jpg', 'IMG_L46.jpg', 'md5-l46', 460.0, '2022-04-01 10:00', 31.0, 131.0, 'exif', '架', 'ウ県', 'ウ市', 0, 1, 0),
  (47, 'drive', 'drive-l47', NULL, 'derivatives/l47.jpg', 'IMG_L47.jpg', 'md5-l47', 470.0, '2022-05-01 10:00', NULL, NULL, NULL, NULL, 'ウ県', 'ウ市', 0, 1, 0),
  (48, 'local', NULL, 'local/l48.jpg', 'derivatives/l48.jpg', 'IMG_L48.jpg', 'md5-l48', 480.0, '2021-01-01 10:00', 48.85, 2.35, 'exif', 'Paris', NULL, NULL, 0, 0, 0);
";

fn year_of<'a>(body: &'a Value, year: &str) -> &'a Value {
    body["years"]
        .as_array()
        .unwrap()
        .iter()
        .find(|y| y["year"] == year)
        .unwrap_or_else(|| panic!("年 {year} が無い: {body}"))
}

fn pref_of<'a>(body: &'a Value, pref: &str) -> &'a Value {
    body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pref"] == pref)
        .unwrap_or_else(|| panic!("都道府県 {pref} が無い: {body}"))
}

fn city_of<'a>(pref: &'a Value, city: &str) -> &'a Value {
    pref["cities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["city"] == city)
        .unwrap_or_else(|| panic!("市区町村 {city} が無い: {pref}"))
}

fn pref_names(body: &Value) -> BTreeSet<&str> {
    body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pref"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn calendar_years_carry_a_cover_chosen_by_visible_located_newest_then_biggest_id() {
    let server = Server::start_with(&[], COVER_EXTRA).await;
    let body = server.ok("/api/admin/library/calendar").await;
    // 除外だけの 2022 は出ない
    assert_eq!(years_of(&body), ["2025", "2024", "2023", "2021"]);
    // 2025: 除外でなく位置のある写真のうち、いちばん新しい 06-01 08:00 は id 19・20 の同分 → 大きい 20
    assert_eq!(year_of(&body, "2025")["cover"]["id"], 20);
    assert!(is_thumb_url(
        year_of(&body, "2025")["cover"]["thumb_url"]
            .as_str()
            .unwrap(),
        20
    ));
    assert_eq!(
        year_of(&body, "2025")["cover"].as_object().unwrap().len(),
        2
    );
    // 2024: 除外の 40（いちばん新しい）と位置なしの 41（除外でない）より、除外でなく位置のある 42
    assert_eq!(year_of(&body, "2024")["cover"]["id"], 42);
    // 2023: 位置のある写真が無いので新しい順。44・45 は同じ分で、大きい 45。表示ファイルが無いので thumb_url は空
    assert_eq!(year_of(&body, "2023")["cover"]["id"], 45);
    assert_eq!(year_of(&body, "2023")["cover"]["thumb_url"], "");
    // 2021: 海外でも位置のある写真
    assert_eq!(year_of(&body, "2021")["cover"]["id"], 48);
    // 月には表紙を付けない。件数は今のまま
    assert!(year_of(&body, "2025")["months"][0].get("cover").is_none());
    assert_eq!(year_count(&body, "2025"), 16);

    // 除外を含める: 2022 は除外だけの組。位置のある 46 を選ぶ。2024 は除外が先に来ない（42 のまま）。2025 も 20
    let hidden = server.ok("/api/admin/library/calendar?hidden=1").await;
    assert_eq!(years_of(&hidden), ["2025", "2024", "2023", "2022", "2021"]);
    assert_eq!(year_of(&hidden, "2022")["cover"]["id"], 46);
    assert_eq!(year_of(&hidden, "2024")["cover"]["id"], 42);
    assert_eq!(year_of(&hidden, "2025")["cover"]["id"], 20);
    assert_eq!(year_count(&hidden, "2024"), 3);
}

#[tokio::test]
async fn place_prefs_carry_a_cover_and_a_bbox_and_cities_a_bbox() {
    let server = Server::start_with(&[], COVER_EXTRA).await;
    let body = server.ok("/api/admin/library/places").await;
    // 除外だけのウ県は出ない
    assert_eq!(
        pref_names(&body),
        BTreeSet::from(["架空県", "試験県", "ア県", "イ県"])
    );
    // 代表: 架空県は位置のある除外でない写真のうち、日時のある中でいちばん新しい 03-12 20:00 の id 14
    // （id 21 は位置があるが日時なしなので最後）。試験県は 06-01 の同分 19・20 → 20。ア県は 42。イ県は位置なしの 45（表示ファイルなし）
    let cover = |pref: &str| pref_of(&body, pref)["cover"].clone();
    assert_eq!(cover("架空県")["id"], 14);
    assert!(is_thumb_url(
        cover("架空県")["thumb_url"].as_str().unwrap(),
        14
    ));
    assert_eq!(cover("試験県")["id"], 20);
    assert_eq!(cover("ア県")["id"], 42);
    assert_eq!(cover("イ県")["id"], 45);
    assert_eq!(cover("イ県")["thumb_url"], "");
    assert_eq!(cover("架空県").as_object().unwrap().len(), 2);

    // 範囲 [最小経度, 最小緯度, 最大経度, 最大緯度]（位置のある写真だけ）。架空県（除外でない）の位置のある写真は
    // id 1 (34.1,135.1)・2 (34.2,135.2)・3 (34.3,135.3)・7 (34.4,135.4)・8 (35.1,137.1)・9 (35.2,137.2)・14 (34.9,136.1)・21 (35.0,136.0)
    let kagu = pref_of(&body, "架空県");
    assert_eq!(kagu["bbox"], json!([135.1, 34.1, 137.2, 35.2]));
    // 試験市は 1・2・3・7・14・21、見本町は 8・9
    assert_eq!(
        city_of(kagu, "試験市")["bbox"],
        json!([135.1, 34.1, 136.1, 35.0])
    );
    assert_eq!(
        city_of(kagu, "見本町")["bbox"],
        json!([137.1, 35.1, 137.2, 35.2])
    );
    // 試験県は 10 (35.9,139.1)・19 (35.5,139.5)・20 (35.6,139.6)
    let test = pref_of(&body, "試験県");
    assert_eq!(test["bbox"], json!([139.1, 35.5, 139.6, 35.9]));
    assert_eq!(
        city_of(test, "サンプル市")["bbox"],
        json!([139.1, 35.5, 139.6, 35.9])
    );
    // 位置のある写真が 1 枚（点）だけなら範囲も点。位置なしだけの組は null
    let a = pref_of(&body, "ア県");
    assert_eq!(a["bbox"], json!([130.1, 30.1, 130.1, 30.1]));
    assert_eq!(
        city_of(a, "エ市")["bbox"],
        json!([130.1, 30.1, 130.1, 30.1])
    );
    assert_eq!(city_of(a, "ア市")["bbox"], Value::Null);
    let i = pref_of(&body, "イ県");
    assert_eq!(i["bbox"], Value::Null);
    assert_eq!(city_of(i, "イ市")["bbox"], Value::Null);
    // 件数は今のまま。海外・不明は位置なしの 7 + 位置のある 48
    assert_eq!(pref_of(&body, "架空県")["count"], 8);
    assert_eq!(body["abroad"], 8);

    // 除外を含める: 範囲に除外の写真の位置も入る。ア県は 40 (30.0,130.0) を足した範囲、ウ県は除外だけの組で 46 が代表
    let hidden = server.ok("/api/admin/library/places?hidden=1").await;
    assert_eq!(
        pref_of(&hidden, "ア県")["bbox"],
        json!([130.0, 30.0, 130.1, 30.1])
    );
    assert_eq!(pref_of(&hidden, "ア県")["cover"]["id"], 42);
    assert_eq!(pref_of(&hidden, "ウ県")["cover"]["id"], 46);
    assert_eq!(
        pref_of(&hidden, "ウ県")["bbox"],
        json!([131.0, 31.0, 131.0, 31.0])
    );
    assert_eq!(pref_of(&hidden, "ウ県")["count"], 2);
    // 試験県は 17 (34.6,138.5)・18 (34.7,138.6) を足す
    assert_eq!(
        pref_of(&hidden, "試験県")["bbox"],
        json!([138.5, 34.6, 139.6, 35.9])
    );
    // 架空県に足される 11・13・23 は、もとの範囲の内側
    assert_eq!(
        pref_of(&hidden, "架空県")["bbox"],
        json!([135.1, 34.1, 137.2, 35.2])
    );
}

/// 代表の優先順位の試験の追加行（COVER_EXTRA とは別の組だけ。既存の件数・一覧を変えない）。手で数える。
///   2020 / エ県: 70 除外でない・位置なし（01-01）、71 除外・位置あり（06-01、こちらのほうが新しい）
///   2019 / オ県: 72 正しい日時・位置あり（03-01）、73 taken_at='x'・位置あり（id が大きい）。どちらも除外でない
const COVER_ORDER_EXTRA: &str = "
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (70, 'drive', 'drive-o70', NULL, 'derivatives/o70.jpg', 'IMG_O70.jpg', 'md5-o70', 700.0, '2020-01-01 10:00', NULL, NULL, NULL, NULL, 'エ県', 'エ市', 0, 0, 0),
  (71, 'local', NULL, 'local/o71.jpg', 'derivatives/o71.jpg', 'IMG_O71.jpg', 'md5-o71', 710.0, '2020-06-01 10:00', 32.0, 132.0, 'exif', '架', 'エ県', 'エ市', 0, 1, 0),
  (72, 'local', NULL, 'local/o72.jpg', 'derivatives/o72.jpg', 'IMG_O72.jpg', 'md5-o72', 720.0, '2019-03-01 10:00', 33.0, 133.0, 'exif', '架', 'オ県', 'オ市', 0, 0, 0),
  (73, 'local', NULL, 'local/o73.jpg', 'derivatives/o73.jpg', 'IMG_O73.jpg', 'md5-o73', 730.0, 'x', 33.1, 133.1, 'exif', '架', 'オ県', 'オ市', 0, 0, 0);
";

#[tokio::test]
async fn cover_prefers_a_visible_unlocated_photo_over_a_hidden_located_one() {
    let server = Server::start_with(&[], COVER_ORDER_EXTRA).await;
    // 除外を含めると、除外の 71（位置あり・新しい）より、除外でない 70（位置なし）が代表
    let places = server.ok("/api/admin/library/places?hidden=1").await;
    assert_eq!(pref_of(&places, "エ県")["cover"]["id"], 70);
    let calendar = server.ok("/api/admin/library/calendar?hidden=1").await;
    assert_eq!(year_of(&calendar, "2020")["cover"]["id"], 70);
    // 除外を含めなければ 71 は候補に入らず、70 だけが代表の候補
    let visible = server.ok("/api/admin/library/places").await;
    assert_eq!(pref_of(&visible, "エ県")["cover"]["id"], 70);
}

#[tokio::test]
async fn cover_puts_a_malformed_taken_at_after_a_real_one() {
    let server = Server::start_with(&[], COVER_ORDER_EXTRA).await;
    // 73 は id が大きく、'x' は文字の並びで '2019-…' より後ろ（降順で先）だが、日時なし扱いなので 72 が代表
    let places = server.ok("/api/admin/library/places").await;
    assert_eq!(pref_of(&places, "オ県")["cover"]["id"], 72);
    let calendar = server.ok("/api/admin/library/calendar").await;
    assert_eq!(year_of(&calendar, "2019")["cover"]["id"], 72);
}

#[tokio::test]
async fn places_with_a_bbox_count_only_what_is_inside_it() {
    let server = Server::start_with(&[], COVER_EXTRA).await;
    let p = |query: &str| format!("/api/admin/library/places?{query}");

    // 架空県は両端に写真がある（経度 135.1〜137.2）が、範囲が中央だけで中に 1 枚も無ければ出ない。
    // 範囲が県の範囲と重なっていても、写真が無ければ出さない。位置のない海外・不明の写真は数えない
    let middle = server.ok(&p("bbox=136.3,34.0,136.9,36.0")).await;
    assert_eq!(middle["prefs"], json!([]));
    assert_eq!(middle["abroad"], 0);
    let middle_hidden = server.ok(&p("bbox=136.3,34.0,136.9,36.0&hidden=1")).await;
    assert_eq!(middle_hidden["prefs"], json!([]));

    // 西端だけ（id 1・2・3）。件数・市区町村・代表・範囲はすべて範囲の中の写真だけ
    let west = server.ok(&p("bbox=135.0,34.0,135.35,34.35")).await;
    assert_eq!(pref_names(&west), BTreeSet::from(["架空県"]));
    assert_eq!(prefs_of(&west), [pref_row("架空県", 3, &[("試験市", 3)])]);
    let kagu = pref_of(&west, "架空県");
    assert_eq!(kagu["cover"]["id"], 3);
    assert_eq!(kagu["bbox"], json!([135.1, 34.1, 135.3, 34.3]));
    assert_eq!(
        city_of(kagu, "試験市")["bbox"],
        json!([135.1, 34.1, 135.3, 34.3])
    );
    assert_eq!(west["abroad"], 0);

    // 縁は含む（id 1 の点だけ）
    let edge = server.ok(&p("bbox=135.1,34.1,135.1,34.1")).await;
    assert_eq!(prefs_of(&edge), [pref_row("架空県", 1, &[("試験市", 1)])]);
    assert_eq!(pref_of(&edge, "架空県")["cover"]["id"], 1);

    // 中央の細い範囲: 位置があり日時のない id 21 (35.0,136.0) も数える。除外の 13・23 は hidden=1 のときだけ
    let strip = server.ok(&p("bbox=136.0,34.9,136.3,35.1")).await;
    assert_eq!(prefs_of(&strip), [pref_row("架空県", 2, &[("試験市", 2)])]);
    assert_eq!(pref_of(&strip, "架空県")["cover"]["id"], 14);
    let strip_hidden = server.ok(&p("bbox=136.0,34.9,136.3,35.1&hidden=1")).await;
    assert_eq!(
        prefs_of(&strip_hidden),
        [pref_row("架空県", 4, &[("見本町", 2), ("試験市", 2)])]
    );
    assert_eq!(pref_of(&strip_hidden, "架空県")["cover"]["id"], 14);

    // 海外・不明は、範囲の中にある位置のある写真だけ（id 48）。県は出ない
    let paris = server.ok(&p("bbox=2.0,48.0,3.0,49.0")).await;
    assert_eq!(paris["prefs"], json!([]));
    assert_eq!(paris["abroad"], 1);
    // 範囲が無ければ、位置なしも数える（フィクスチャの 7 + 48）
    assert_eq!(server.ok(&p("hidden=0")).await["abroad"], 8);

    // 世界全体の範囲でも、位置のない写真は数えない: 位置のある除外でない写真は、架空県 8（id 1・2・3・7・8・9・14・21）、
    // 試験県 3（10・19・20）、ア県 1（42）
    let world = server.ok(&p("bbox=-180,-90,180,90")).await;
    assert_eq!(
        prefs_of(&world)
            .iter()
            .map(|p| (p.0.as_str(), p.1))
            .collect::<Vec<_>>(),
        [("架空県", 8), ("試験県", 3), ("ア県", 1)]
    );
    // 位置のない海外・不明の 7 は数えず、位置のある id 48 だけ
    assert_eq!(world["abroad"], 1);
}

#[tokio::test]
async fn places_in_a_period_count_only_that_period() {
    let server = Server::start().await;
    let p = |query: &str| format!("/api/admin/library/places?{query}");
    let at = |from: &str, to: &str| format!("from={}&to={}", enc(from), enc(to));

    // 旅 A の範囲（03-01 09:00〜03-03 19:30）: 除外でない id 1〜10。架空県は 1・2・3・7（試験市）と 8・9（見本町）、
    // 試験県は 10、海外・不明は位置なしの 4・5・6
    let a = server
        .ok(&p(&at("2025-03-01 09:00", "2025-03-03 19:30")))
        .await;
    assert_eq!(
        prefs_of(&a),
        [
            pref_row("架空県", 6, &[("試験市", 4), ("見本町", 2)]),
            pref_row("試験県", 1, &[("サンプル市", 1)]),
        ]
    );
    assert_eq!(a["abroad"], 3);
    // 代表と範囲も期間の中の写真だけ。架空県の新しい位置あり（03-03 19:20）は id 9
    let kagu = pref_of(&a, "架空県");
    assert_eq!(kagu["cover"]["id"], 9);
    assert_eq!(kagu["bbox"], json!([135.1, 34.1, 137.2, 35.2]));
    assert_eq!(
        city_of(kagu, "試験市")["bbox"],
        json!([135.1, 34.1, 135.4, 34.4])
    );
    assert_eq!(
        city_of(kagu, "見本町")["bbox"],
        json!([137.1, 35.1, 137.2, 35.2])
    );
    assert_eq!(pref_of(&a, "試験県")["cover"]["id"], 10);
    assert_eq!(
        pref_of(&a, "試験県")["bbox"],
        json!([139.1, 35.9, 139.1, 35.9])
    );

    // from だけ・to だけ
    let june = server.ok(&p("from=2025-06-01%2000%3A00")).await;
    assert_eq!(
        prefs_of(&june),
        [pref_row("試験県", 2, &[("サンプル市", 2)])]
    );
    assert_eq!(june["abroad"], 0);
    let early = server.ok(&p("to=2025-03-01%2009%3A10")).await;
    assert_eq!(prefs_of(&early), [pref_row("架空県", 2, &[("試験市", 2)])]);
    assert_eq!(early["abroad"], 0);

    // 期間があると日時なしは数えない（全期間では架空県 8・海外・不明 7、年の形の範囲では 21・22 が落ちて 7・6）
    let year = server
        .ok(&p("from=2025-00-00%2000%3A00&to=2025-99-99%2099%3A99"))
        .await;
    assert_eq!(pref_of(&year, "架空県")["count"], 7);
    assert_eq!(year["abroad"], 6);
    let all = server.ok(&p("hidden=0")).await;
    assert_eq!(pref_of(&all, "架空県")["count"], 8);
    assert_eq!(all["abroad"], 7);

    // 除外を含める: 05-01 12:00〜12:05 は除外の 17・18 だけ（試験県）
    let may = at("2025-05-01 12:00", "2025-05-01 12:05");
    assert_eq!(server.ok(&p(&may)).await["prefs"], json!([]));
    let may_hidden = server.ok(&p(&format!("{may}&hidden=1"))).await;
    assert_eq!(
        prefs_of(&may_hidden),
        [pref_row("試験県", 2, &[("サンプル市", 2)])]
    );
    assert_eq!(pref_of(&may_hidden, "試験県")["cover"]["id"], 18);

    // 期間と範囲は同時に掛かる（旅 A の範囲で、西端の id 1・2・3 だけ）
    let both = server
        .ok(&p(&format!(
            "{}&bbox=135.0,34.0,135.35,34.35",
            at("2025-03-01 09:00", "2025-03-03 19:30")
        )))
        .await;
    assert_eq!(prefs_of(&both), [pref_row("架空県", 3, &[("試験市", 3)])]);
    assert_eq!(both["abroad"], 0);
}

#[tokio::test]
async fn places_reject_a_bad_period_or_bbox() {
    let server = Server::start().await;
    let p = |query: &str| format!("/api/admin/library/places?{query}");
    for bad in [
        "from=x",
        "to=2025-06-01",
        "from=2025-06-01%2010%3A01&to=2025-06-01%2010%3A00",
        "from=2025-6-1%2010%3A00",
        "bbox=1,2,3",
        "bbox=1,2,3,4,5",
        "bbox=140,34,135,36",
        "bbox=135,36,140,34",
        "bbox=nan,0,1,1",
        "bbox=-181,0,0,1",
        "bbox=0,-91,1,0",
        "bbox=a,b,c,d",
        "from=2025-06-01%2010%3A00&bbox=1,2,3",
    ] {
        server.bad(&p(bad)).await;
    }
    // 形が合えば通る（暦は見ない。範囲は縁を含む）
    server.ok(&p("from=2025-02-30%2010%3A00")).await;
    server.ok(&p("bbox=-180,-90,180,90")).await;
}

/// 文字検索の試験の追加行（名前の特殊文字。どれも日時なし・位置なし・pref なしで、フィクスチャの検索結果には混ざらない）。
///   60 `ZIP_Q1.jpg`・61 `ZIPxQ2.jpg`（`_` が文字か任意の 1 文字か）
///   62 `PCT%OFF.jpg`・63 `PCTxxOFF.jpg`（`%` が文字か任意の長さか）
///   64 `xa\%y.jpg`・65 `xa\zy.jpg`・66 `xa%y.jpg`（`\` と `a\%`）
///   67 `BS\Q.jpg`・68 `BSxQ.jpg`
const Q_EXTRA: &str = r"
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (60, 'local', NULL, 'local/q60.jpg', 'derivatives/q60.jpg', 'ZIP_Q1.jpg', 'md5-q60', 600.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (61, 'local', NULL, 'local/q61.jpg', 'derivatives/q61.jpg', 'ZIPxQ2.jpg', 'md5-q61', 610.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (62, 'local', NULL, 'local/q62.jpg', 'derivatives/q62.jpg', 'PCT%OFF.jpg', 'md5-q62', 620.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (63, 'local', NULL, 'local/q63.jpg', 'derivatives/q63.jpg', 'PCTxxOFF.jpg', 'md5-q63', 630.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (64, 'local', NULL, 'local/q64.jpg', 'derivatives/q64.jpg', 'xa\%y.jpg', 'md5-q64', 640.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (65, 'local', NULL, 'local/q65.jpg', 'derivatives/q65.jpg', 'xa\zy.jpg', 'md5-q65', 650.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (66, 'local', NULL, 'local/q66.jpg', 'derivatives/q66.jpg', 'xa%y.jpg', 'md5-q66', 660.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (67, 'local', NULL, 'local/q67.jpg', 'derivatives/q67.jpg', 'BS\Q.jpg', 'md5-q67', 670.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (68, 'local', NULL, 'local/q68.jpg', 'derivatives/q68.jpg', 'BSxQ.jpg', 'md5-q68', 680.0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0);
";

#[tokio::test]
async fn photos_q_matches_any_of_the_five_columns_by_substring() {
    let server = Server::start_with(&[], Q_EXTRA).await;
    let q = |text: &str| format!("/api/admin/library/photos?q={}&per=200", enc(text));
    // pref: 試験県は 10・19・20（除外の 17・18 は入らない）
    assert_eq!(ids(&server.ok(&q("試験県")).await), [10, 19, 20]);
    // city の部分一致（見本町は 8・9。除外の 13・23 は入らない）
    assert_eq!(ids(&server.ok(&q("見本")).await), [8, 9]);
    // name の部分一致（IMG_L1x は 10〜19。除外の 11・13・17・18 は入らない。大文字小文字は問わない）
    let l1 = [10, 12, 14, 15, 16, 19];
    assert_eq!(ids(&server.ok(&q("L1")).await), l1);
    assert_eq!(ids(&server.ok(&q("l1")).await), l1);
    // taken_at の部分一致（03-12 は 12・14。除外の 13 は入らない）
    assert_eq!(ids(&server.ok(&q("03-12")).await), [12, 14]);
    // place の部分一致（試験区は位置のある行にだけ入る）
    assert_eq!(ids(&server.ok(&q("試験区")).await).len(), 11);
    // q だけで足りる（ほかの絞り込みは要らない）。何にも当たらなければ 200 と空
    let none = server.ok(&q("存在しない地名")).await;
    assert_eq!(ids(&none), Vec::<i64>::new());
    assert_eq!(none["total"], 0);
    // `+` は空白（語の区切り）、前後の空白は無視する
    assert_eq!(
        ids(&server
            .ok("/api/admin/library/photos?q=+%E8%A9%A6%E9%A8%93%E7%9C%8C+")
            .await),
        [10, 19, 20]
    );
}

#[tokio::test]
async fn photos_q_words_are_anded_across_columns() {
    let server = Server::start_with(&[], Q_EXTRA).await;
    let q = |text: &str| format!("/api/admin/library/photos?q={}&per=200", enc(text));
    // 試験県（pref）と サンプル（city）: どちらにも当たるのは 10・19・20
    assert_eq!(ids(&server.ok(&q("試験県 サンプル")).await), [10, 19, 20]);
    // 語の順は関係ない
    assert_eq!(ids(&server.ok(&q("サンプル 試験県")).await), [10, 19, 20]);
    // どちらかにしか当たらなければ出ない
    assert_eq!(ids(&server.ok(&q("試験県 見本")).await), Vec::<i64>::new());
    // city（試験市）と taken_at（2025-03-12）: 14 だけ（12 は city が無い）
    assert_eq!(ids(&server.ok(&q("試験市 2025-03-12")).await), [14]);
    // 3 語
    assert_eq!(ids(&server.ok(&q("試験市 2025-03-12 L14")).await), [14]);
    assert_eq!(
        ids(&server.ok(&q("試験市 2025-03-12 L15")).await),
        Vec::<i64>::new()
    );
    // 全角の空白でも語に分ける。前後の全角の空白は無視する
    assert_eq!(ids(&server.ok(&q("試験市\u{3000}2025-03-12")).await), [14]);
    assert_eq!(
        ids(&server
            .ok(&q("\u{3000}試験県\u{3000}サンプル\u{3000}"))
            .await),
        [10, 19, 20]
    );
}

#[tokio::test]
async fn photos_q_percent_underscore_and_backslash_match_as_characters() {
    let server = Server::start_with(&[], Q_EXTRA).await;
    let q = |text: &str| format!("/api/admin/library/photos?q={}&per=200", enc(text));
    // `_` は任意の 1 文字ではなく文字そのもの: ZIP_Q1 だけ（ZIPxQ2 は入らない）
    assert_eq!(ids(&server.ok(&q("P_Q")).await), [60]);
    // `%` は任意の長さではなく文字そのもの: PCT%OFF だけ（PCTxxOFF は入らない）
    assert_eq!(ids(&server.ok(&q("T%O")).await), [62]);
    // `%` だけ: 名前に `%` を含む 62・64・66（`%` が任意なら全件になる）
    assert_eq!(ids(&server.ok(&q("%")).await), [62, 64, 66]);
    // `\` は文字そのもの: BS\Q だけ（BSxQ は入らない）。`\` だけなら名前に `\` を含む 64・65・67
    assert_eq!(ids(&server.ok(&q("S\\Q")).await), [67]);
    assert_eq!(ids(&server.ok(&q("\\")).await), [64, 65, 67]);
    // `a\%`（3 文字）: `a`・`\`・`%` をその順に含む 64 だけ。65（a\z）・66（a%）は入らない
    assert_eq!(ids(&server.ok(&q("a\\%")).await), [64]);
    // 2 語でも同じ（各語を別々に逃がす）
    assert_eq!(ids(&server.ok(&q("a\\% xa")).await), [64]);
    // SQL に見える文字列も、ただの語として探すだけ
    for text in ["' OR 1=1 --", "\"); DROP TABLE photos;--", "%' OR '%'='"] {
        let body = server.ok(&q(text)).await;
        assert_eq!(body["total"], 0, "{text}");
    }
    assert_eq!(ids(&server.ok(&q("試験県")).await), [10, 19, 20]);
}

#[tokio::test]
async fn photos_q_works_together_with_the_other_filters() {
    let server = Server::start_with(&[], Q_EXTRA).await;
    let p = |query: &str| format!("/api/admin/library/photos?{query}&per=200");
    let q = |text: &str, rest: &str| p(&format!("q={}&{rest}", enc(text)));
    // 除外: 見本は 8・9。hidden=1 なら除外の 13・23 も（日時なしの 23 は最後）
    assert_eq!(
        ids(&server.ok(&q("見本", "hidden=1")).await),
        [8, 9, 13, 23]
    );
    assert_eq!(
        ids(&server.ok(&q("L11", "hidden=0")).await),
        Vec::<i64>::new()
    );
    assert_eq!(ids(&server.ok(&q("L11", "hidden=1")).await), [11]);
    // 期間
    let march = format!(
        "from={}&to={}",
        enc("2025-03-12 00:00"),
        enc("2025-03-31 23:59")
    );
    assert_eq!(ids(&server.ok(&q("試験市", &march)).await), [14]);
    // pref・city・abroad・untimed・ids・bbox
    assert_eq!(
        ids(&server
            .ok(&q("見本", &format!("pref={}", enc("架空県"))))
            .await),
        [8, 9]
    );
    assert_eq!(
        ids(&server
            .ok(&q(
                "見本",
                &format!("pref={}&city={}", enc("架空県"), enc("試験市"))
            ))
            .await),
        Vec::<i64>::new()
    );
    assert_eq!(ids(&server.ok(&q("ZIP", "abroad=1")).await), [60, 61]);
    assert_eq!(ids(&server.ok(&q("ZIP", "untimed=1")).await), [60, 61]);
    assert_eq!(ids(&server.ok(&q("ZIP", "ids=61,62")).await), [61]);
    assert_eq!(
        ids(&server
            .ok(&q("試験市", "bbox=135.0,34.0,135.25,34.25"))
            .await),
        [1, 2]
    );
    // ページ送りでも q は効く（総数は q で絞った数）
    let page = server.ok(&q("試験県", "per=2&page=2")).await;
    assert_eq!(
        (page["total"].clone(), page["pages"].clone()),
        (json!(3), json!(2))
    );
    assert_eq!(ids(&page), [20]);
}

#[tokio::test]
async fn photos_q_is_limited_to_64_characters_and_5_words() {
    let server = Server::start().await;
    let q = |text: &str| format!("/api/admin/library/photos?q={}", enc(text));
    // 64 文字は通り、65 文字は 400（全角でも文字数）
    assert_eq!(server.ok(&q(&"あ".repeat(64))).await["total"], 0);
    server.bad(&q(&"あ".repeat(65))).await;
    server.bad(&q(&"a".repeat(65))).await;
    // 長さは前後の空白を除いたあとで数える
    assert_eq!(
        server
            .ok(&q(&format!(" \u{3000}{}\u{3000} ", "あ".repeat(64))))
            .await["total"],
        0
    );
    server.bad(&q(&format!(" {} ", "あ".repeat(65)))).await;
    // 5 語は通り、6 語は 400（全角の空白でも数える）
    assert_eq!(server.ok(&q("a b c d e")).await["total"], 0);
    server.bad(&q("a b c d e f")).await;
    server
        .bad(&q("a\u{3000}b\u{3000}c\u{3000}d\u{3000}e\u{3000}f"))
        .await;
    // 空白を詰めて並べても語数は変わらない
    assert_eq!(
        server.ok(&q("a   b\u{3000}\u{3000}c d e")).await["total"],
        0
    );
    // 他の絞り込みと一緒でも上限は守る
    server
        .bad(&format!("{}&pref={}", q("a b c d e f"), enc("架空県")))
        .await;
    // 空白だけの q は無いのと同じ: 絞り込みにならない（単独では 400）。ほかの絞り込みがあればそちらだけで通る
    server.bad(&q("   ")).await;
    server.bad(&q("\u{3000}")).await;
    let only_pref = server
        .ok(&format!(
            "/api/admin/library/photos?q=%20&pref={}&per=200",
            enc("試験県")
        ))
        .await;
    assert_eq!(ids(&only_pref), [10, 19, 20]);
}

// ---------------------------------------------------------------- AC7

#[tokio::test]
async fn reads_work_without_a_writer_and_without_the_new_tables() {
    for env in [
        &[("GEOFRAME_INGEST", "0")][..],
        &[][..],
        &[("GEOFRAME_INGEST", "1")][..],
    ] {
        let server = Server::start_with(env, "").await;
        // 新しい表（photo_curation）の無い DB
        let tables: i64 = server
            .writer
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='photo_curation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        for target in [
            "/api/admin/library/trips",
            "/api/admin/library/photos?untimed=1",
            "/api/admin/library/points",
            "/api/admin/library/photos?abroad=1",
            "/api/admin/library/calendar",
            "/api/admin/library/places",
        ] {
            let response = get(server.addr, target).await;
            assert_eq!(response.status, 200, "{target} {env:?}");
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
            assert_eq!(response.json()["ok"], true);
        }
    }
}

#[tokio::test]
async fn errors_are_json_with_no_store() {
    let server = Server::start().await;
    let response = get(server.addr, "/api/admin/library/photos").await;
    assert_eq!(response.status, 400);
    assert!(
        response
            .headers
            .contains(&format!("content-type: {JSON_UTF8}"))
    );
    assert!(response.headers.contains("cache-control: no-store"));
    // 値は応答に出さない
    let echoed = get(
        server.addr,
        "/api/admin/library/photos?from=SECRET-VALUE-93ab",
    )
    .await;
    assert_eq!(echoed.status, 400);
    assert!(!echoed.text().contains("SECRET-VALUE"));
}

#[tokio::test]
async fn only_get_on_the_exact_paths_is_served() {
    let server = Server::start().await;
    let routes = [
        "/api/admin/library/trips",
        "/api/admin/library/photos",
        "/api/admin/library/points",
        "/api/admin/library/calendar",
        "/api/admin/library/places",
    ];
    // POST・HEAD は既存の管理 API と同じく 403
    for path in routes {
        for method in ["POST", "HEAD"] {
            let response = request(server.addr, method, path, &[("Host", "localhost")]).await;
            assert_eq!(response.status, 403, "{method} {path}");
        }
    }
    // 完全一致だけ: 似た名前・末尾の `/`・大文字・パーセント符号化・ほかの名前は静的配信の判定に回って 403
    for target in [
        "/api/admin/library/tripsX",
        "/api/admin/library/trips/",
        "/api/admin/library/trips/x",
        "/api/admin/library/photosX",
        "/api/admin/library/photos/",
        "/api/admin/library/pointsX",
        "/api/admin/library/points/",
        "/api/admin/library",
        "/api/admin/library/",
        "/api/admin/library/calendarX",
        "/api/admin/library/calendar/",
        "/api/admin/library/placesX",
        "/api/admin/library/places/",
        "/api/admin/Library/calendar",
        "/api/admin/library/Places",
        "/api/admin/library/cal%65ndar",
        "/api/admin/Library/trips",
        "/api/admin/library/Trips",
        "/api/admin/%6Cibrary/trips",
        "/api/admin/library/tri%70s",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
        assert!(!response.text().contains("\"trips\""), "{target}");
        assert!(!response.text().contains("\"years\""), "{target}");
        assert!(!response.text().contains("\"prefs\""), "{target}");
    }
    // クエリは無視して 200。先頭の `//` も畳む
    for target in [
        "/api/admin/library/trips?x=1",
        "//api/admin/library/trips",
        "/api/admin/library/points?x=1",
        "/api/admin/library/calendar?x=1",
        "/api/admin/library/places?x=1",
        "//api/admin/library/places",
    ] {
        assert_eq!(get(server.addr, target).await.status, 200, "{target}");
    }
}
