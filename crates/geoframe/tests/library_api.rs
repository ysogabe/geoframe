//! `GET /api/admin/library/{trips,photos,points}`（ライブラリ。docs/admin-library/design.md）の結合テスト。
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
        "/api/admin/library/calendar",
        "/api/admin/Library/trips",
        "/api/admin/library/Trips",
        "/api/admin/%6Cibrary/trips",
        "/api/admin/library/tri%70s",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
        assert!(!response.text().contains("\"trips\""), "{target}");
    }
    // クエリは無視して 200。先頭の `//` も畳む
    for target in [
        "/api/admin/library/trips?x=1",
        "//api/admin/library/trips",
        "/api/admin/library/points?x=1",
    ] {
        assert_eq!(get(server.addr, target).await.status, 200, "{target}");
    }
}
