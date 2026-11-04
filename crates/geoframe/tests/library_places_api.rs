//! `GET /api/admin/library/places` の国・地域別の集計（docs/international-places/design.md「管理API」）の結合テスト。
//!
//! 合成データだけ（実際の撮影座標を使わない）。国・地域の ID は同梱の名称表のもの。`photo_regions` の行は現行の分類版で
//! 直接入れる（分類段は通さない）。期待値は下のコメントの構成から数えた値で、実装の出力を写していない。
//!
//! 構成（`taken_at` は月で分ける。除外は `hidden=1`）:
//! - 日本の `pref` あり（4 月）: 1・2・3 東京都、4 大阪府、5 東京都（座標が海上）、6 東京都（座標なし = GPS が消えた）、
//!   7 `pref` が 47 件に無い架空県、8 東京都（除外）。
//! - 日本の境界データ（`pref` 空、4 月）: 9 東京都の地域、10 国のみ。
//! - フランス（5 月）: 11・12 地域 A、13 地域 B、14 国のみ、15 地域 A（除外）。
//! - 同名の地域（6 月）: 16 アルゼンチンの La Rioja、17・18 スペインの La Rioja。
//! - 日付変更線（7 月）: 19・20 フィジー（経度 178 と -179）。21〜24 ロシアの地域不明（経度 -100・-20・60・140）。
//! - 未判定（8 月）: 25 区域外、26 競合、27 処理待ち（分類行なし）、28 失敗（`failed`）、29 古い分類版、30 座標が合わない行。
//! - 座標なし・無効（8 月）: 31 座標なし、32 緯度なし、33 緯度 95、34 緯度が文字列。

mod common;

use common::{TempDir, build_empty_catalog, get, spawn_with_env};
use geoframe::db::Catalog;
use geoframe::regions::data::{self, JAPAN_ID};
use serde_json::{Value, json};

struct Server {
    _dir: TempDir,
    writer: rusqlite::Connection,
    addr: std::net::SocketAddr,
}

impl Server {
    async fn start(env: &[(&str, &str)], with_table: bool, rows: &str) -> Self {
        let dir = TempDir::new("places-api");
        let (path, mut writer) = build_empty_catalog(dir.path(), "");
        if with_table {
            geoframe::regions::ensure_tables(&mut writer).unwrap();
        }
        writer.execute_batch(rows).unwrap();
        let catalog = Catalog::open(&path).unwrap();
        let addr = spawn_with_env(dir.path(), env, catalog).await;
        Self {
            _dir: dir,
            writer,
            addr,
        }
    }

    async fn ok(&self, query: &str) -> Value {
        let target = format!("/api/admin/library/places{query}");
        let response = get(self.addr, &target).await;
        assert_eq!(response.status, 200, "{target}");
        let body = response.json();
        assert_eq!(body["ok"], true, "{target}");
        body
    }

    fn count(&self, sql: &str) -> i64 {
        self.writer.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

fn names() -> &'static data::NameTable {
    data::names().unwrap()
}

fn first_region(country: &str) -> String {
    names()
        .regions
        .iter()
        .find(|r| r.country.as_deref() == Some(country))
        .unwrap_or_else(|| panic!("{country} の地域が名称表に無い"))
        .id
        .clone()
}

fn nth_region(country: &str, n: usize) -> String {
    names()
        .regions
        .iter()
        .filter(|r| r.country.as_deref() == Some(country))
        .nth(n)
        .unwrap_or_else(|| panic!("{country} の地域が足りない"))
        .id
        .clone()
}

fn lit(value: Option<&str>) -> String {
    value.map_or("NULL".to_owned(), |v| format!("'{v}'"))
}

/// `photos` の 1 行。`lat`・`lon` はそのままの SQL の式。
fn photo(
    id: i64,
    taken: Option<&str>,
    lat: &str,
    lon: &str,
    pref: Option<&str>,
    hidden: u8,
) -> String {
    format!(
        "INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, pref, city, hidden) \
         VALUES({id},'local',NULL,'p/{id}.jpg','d/{id}.jpg','P{id}.jpg','m{id}',1.0,{taken},{lat},{lon},{pref},{city},{hidden});\n",
        taken = lit(taken),
        pref = lit(pref),
        city = if pref.is_some() {
            format!("'市{}'", id % 2)
        } else {
            "NULL".to_owned()
        },
    )
}

/// `photo_regions` の 1 行（`version` を指定する）。座標は `photos` と同じ数値を渡す。
fn class_row(
    id: i64,
    lat: &str,
    lon: &str,
    version: &str,
    country: Option<&str>,
    region: Option<&str>,
    methods: (&str, &str),
) -> String {
    format!(
        "INSERT INTO photo_regions VALUES({id},{lat},{lon},'{version}',{country},{region},'',{cm},{rm},'t');\n",
        country = lit(country),
        region = lit(region),
        cm = lit(Some(methods.0)),
        rm = lit(Some(methods.1)),
    )
}

struct Fixture {
    sql: String,
    tokyo: String,
    fr_a: String,
    fr_b: String,
    la_rioja_ar: String,
    la_rioja_es: String,
    fiji: String,
}

fn fixture() -> Fixture {
    let version = geoframe::regions::classifier_version().unwrap();
    let tokyo = names().region_of_pref("東京都").unwrap().id.clone();
    let fr_a = nth_region("ne:FRA", 0);
    let fr_b = nth_region("ne:FRA", 1);
    let (la_rioja_ar, la_rioja_es) = ("ne:ARG-1302".to_owned(), "ne:ESP-5828".to_owned());
    assert_eq!(
        names().region(&la_rioja_ar).unwrap().name,
        names().region(&la_rioja_es).unwrap().name,
        "同名の地域の組"
    );
    let fiji = first_region("ne:FJI");
    let mut sql = String::from("BEGIN;\n");
    let jp = |id, day: &str, lat: &str, lon: &str, pref: &str, hidden| {
        photo(
            id,
            Some(&format!("2025-04-{day} 10:00")),
            lat,
            lon,
            Some(pref),
            hidden,
        )
    };
    // 日本の pref あり
    sql += &jp(1, "01", "35.7", "139.7", "東京都", 0);
    sql += &jp(2, "02", "35.7", "139.8", "東京都", 0);
    sql += &jp(3, "03", "35.6", "139.6", "東京都", 0);
    sql += &jp(4, "04", "34.7", "135.5", "大阪府", 0);
    sql += &jp(5, "05", "30.5", "139.9", "東京都", 0);
    sql += &jp(6, "06", "NULL", "NULL", "東京都", 0);
    sql += &jp(7, "07", "36.0", "138.0", "架空県", 0);
    sql += &jp(8, "08", "35.5", "139.5", "東京都", 1);
    // 日本の境界データ
    sql += &photo(9, Some("2025-04-09 10:00"), "35.0", "139.0", None, 0);
    sql += &photo(10, Some("2025-04-10 10:00"), "43.0", "141.0", Some(""), 0);
    // フランス
    let fr = |id, day: &str, lat: &str, lon: &str, hidden| {
        photo(
            id,
            Some(&format!("2025-05-{day} 09:00")),
            lat,
            lon,
            None,
            hidden,
        )
    };
    sql += &fr(11, "01", "48.0", "2.0", 0);
    sql += &fr(12, "02", "48.5", "2.5", 0);
    sql += &fr(13, "03", "45.0", "5.0", 0);
    sql += &fr(14, "04", "46.0", "3.0", 0);
    sql += &fr(15, "05", "49.0", "3.0", 1);
    // 同名の地域
    let june = |id, lat: &str, lon: &str| photo(id, Some("2025-06-01 09:00"), lat, lon, None, 0);
    sql += &june(16, "-29.0", "-67.0");
    sql += &june(17, "42.3", "-2.5");
    sql += &june(18, "42.4", "-2.4");
    // 日付変更線・経度が広い
    let july = |id, lat: &str, lon: &str| photo(id, Some("2025-07-01 09:00"), lat, lon, None, 0);
    sql += &july(19, "-17.0", "178.0");
    sql += &july(20, "-17.0", "-179.0");
    for (id, lon) in [(21, "-100.0"), (22, "-20.0"), (23, "60.0"), (24, "140.0")] {
        sql += &july(id, "55.0", lon);
    }
    // 未判定
    let aug = |id, lat: &str, lon: &str| photo(id, Some("2025-08-01 09:00"), lat, lon, None, 0);
    sql += &aug(25, "0.0", "0.0");
    sql += &aug(26, "1.0", "1.0");
    sql += &aug(27, "2.0", "2.0");
    sql += &aug(28, "3.0", "3.0");
    sql += &aug(29, "4.0", "4.0");
    sql += &aug(30, "5.5", "5.5");
    // 座標なし・無効
    sql += &photo(31, Some("2025-08-02 09:00"), "NULL", "NULL", None, 0);
    sql += &photo(32, None, "NULL", "10.0", None, 0);
    sql += &photo(33, Some("2025-08-03 09:00"), "95.0", "10.0", None, 0);
    sql += &photo(34, Some("2025-08-04 09:00"), "'abc'", "10.0", None, 0);
    sql += "COMMIT;\nBEGIN;\n";
    let inside = ("inside", "inside");
    let country_only = ("inside", "outside");
    sql += &class_row(
        9,
        "35.0",
        "139.0",
        version,
        Some(JAPAN_ID),
        Some(&tokyo),
        inside,
    );
    sql += &class_row(
        10,
        "43.0",
        "141.0",
        version,
        Some(JAPAN_ID),
        None,
        country_only,
    );
    sql += &class_row(
        11,
        "48.0",
        "2.0",
        version,
        Some("ne:FRA"),
        Some(&fr_a),
        inside,
    );
    sql += &class_row(
        12,
        "48.5",
        "2.5",
        version,
        Some("ne:FRA"),
        Some(&fr_a),
        inside,
    );
    sql += &class_row(
        13,
        "45.0",
        "5.0",
        version,
        Some("ne:FRA"),
        Some(&fr_b),
        inside,
    );
    sql += &class_row(
        14,
        "46.0",
        "3.0",
        version,
        Some("ne:FRA"),
        None,
        country_only,
    );
    sql += &class_row(
        15,
        "49.0",
        "3.0",
        version,
        Some("ne:FRA"),
        Some(&fr_a),
        inside,
    );
    sql += &class_row(
        16,
        "-29.0",
        "-67.0",
        version,
        Some("ne:ARG"),
        Some(&la_rioja_ar),
        inside,
    );
    sql += &class_row(
        17,
        "42.3",
        "-2.5",
        version,
        Some("ne:ESP"),
        Some(&la_rioja_es),
        inside,
    );
    sql += &class_row(
        18,
        "42.4",
        "-2.4",
        version,
        Some("ne:ESP"),
        Some(&la_rioja_es),
        inside,
    );
    sql += &class_row(
        19,
        "-17.0",
        "178.0",
        version,
        Some("ne:FJI"),
        Some(&fiji),
        inside,
    );
    sql += &class_row(
        20,
        "-17.0",
        "-179.0",
        version,
        Some("ne:FJI"),
        Some(&fiji),
        inside,
    );
    for (id, lon) in [(21, "-100.0"), (22, "-20.0"), (23, "60.0"), (24, "140.0")] {
        sql += &class_row(id, "55.0", lon, version, Some("ne:RUS"), None, country_only);
    }
    sql += &class_row(
        25,
        "0.0",
        "0.0",
        version,
        None,
        None,
        ("outside", "outside"),
    );
    sql += &class_row(
        26,
        "1.0",
        "1.0",
        version,
        None,
        None,
        ("ambiguous", "outside"),
    );
    sql += &class_row(28, "3.0", "3.0", version, None, None, ("failed", "failed"));
    sql += &class_row(
        29,
        "4.0",
        "4.0",
        "gf0-old",
        Some("ne:FRA"),
        None,
        country_only,
    );
    sql += &class_row(
        30,
        "5.0",
        "5.0",
        version,
        Some("ne:FRA"),
        None,
        country_only,
    );
    sql += "COMMIT;\n";
    Fixture {
        sql,
        tokyo,
        fr_a,
        fr_b,
        la_rioja_ar,
        la_rioja_es,
        fiji,
    }
}

fn countries(body: &Value) -> Vec<(&str, i64)> {
    body["countries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_str().unwrap(), c["count"].as_i64().unwrap()))
        .collect()
}

fn country<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["countries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("国 {id} が無い: {body}"))
}

fn region<'a>(country: &'a Value, id: &str) -> &'a Value {
    country["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("地域 {id} が無い: {country}"))
}

#[tokio::test]
async fn countries_and_regions_are_counted_by_id_and_sorted_by_count_then_name() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let body = server.ok("").await;
    // 除外（8・15）は数えない。日本 9（1〜7・9・10）、フランス 4（11〜14）、ロシア 4（21〜24）、スペイン 2、フィジー 2、アルゼンチン 1
    // 件数の多い順、同数は表示名（フランス < ロシア、スペイン < フィジー）
    assert_eq!(
        countries(&body),
        [
            ("ne:JPN", 9),
            ("ne:FRA", 4),
            ("ne:RUS", 4),
            ("ne:ESP", 2),
            ("ne:FJI", 2),
            ("ne:ARG", 1)
        ]
    );
    assert_eq!(body["classification_available"], true);
    // 同名の地域は別の国の下に別の ID で出る
    let es = region(country(&body, "ne:ESP"), &f.la_rioja_es);
    let ar = region(country(&body, "ne:ARG"), &f.la_rioja_ar);
    assert_eq!(
        (es["count"].clone(), ar["count"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(es["name"], ar["name"]);
    assert_ne!(es["id"], ar["id"]);
    // 表示名と検索別名
    let japan = country(&body, "ne:JPN");
    assert_eq!(japan["name"], "日本");
    let aliases: Vec<&str> = japan["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(aliases.contains(&"Japan"));
    // フランス: 地域 A が 2（15 は除外）、地域 B が 1、国のみ（地域不明）が 1
    let fr = country(&body, "ne:FRA");
    assert_eq!(region(fr, &f.fr_a)["count"], 2);
    assert_eq!(region(fr, &f.fr_b)["count"], 1);
    assert_eq!(fr["region_unknown"]["count"], 1);
    assert_eq!(fr["regions"].as_array().unwrap().len(), 2);
    assert_eq!(fr["regions"][0]["id"], f.fr_a.as_str());
    // ロシアは国のみの判定だけ
    let ru = country(&body, "ne:RUS");
    assert_eq!(ru["regions"], json!([]));
    assert_eq!(ru["region_unknown"]["count"], 4);
}

#[tokio::test]
async fn japan_regions_join_pref_photos_and_boundary_photos_and_keep_old_prefs() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let body = server.ok("").await;
    let japan = country(&body, "ne:JPN");
    // 東京都: pref の 1・2・3・5・6（8 は除外）+ 境界データの 9
    let tokyo = region(japan, &f.tokyo);
    assert_eq!(tokyo["pref"], "東京都");
    assert_eq!(tokyo["count"], 6);
    assert_eq!(tokyo["boundary_only"], 1);
    assert_eq!(
        region(japan, &names().region_of_pref("大阪府").unwrap().id)["count"],
        1
    );
    assert_eq!(
        region(japan, &names().region_of_pref("大阪府").unwrap().id)["boundary_only"],
        0
    );
    // 47 件に無い pref（7）と、国のみの境界データ（10）は日本の地域不明
    assert_eq!(japan["region_unknown"]["count"], 2);
    // 旧 `prefs` は意味が変わらない: pref の写真だけを pref/city で数える。地域の件数 − boundary_only と市区町村が一致する
    let prefs = body["prefs"].as_array().unwrap();
    let old = |name: &str| prefs.iter().find(|p| p["pref"] == name).unwrap();
    assert_eq!(old("東京都")["count"], 5);
    assert_eq!(old("東京都")["cities"], tokyo["cities"]);
    assert_eq!(
        tokyo["count"].as_i64().unwrap() - tokyo["boundary_only"].as_i64().unwrap(),
        5
    );
    assert_eq!(old("架空県")["count"], 1);
    // 旧 `prefs` の合計は pref のある写真（除外を除く）7 枚、`abroad` は pref の無い写真
    let pref_sum: i64 = prefs.iter().map(|p| p["count"].as_i64().unwrap()).sum();
    assert_eq!(pref_sum, 7);
    // 除外（8・15）を除く 32 枚のうち pref の無い 25 枚
    assert_eq!(body["abroad"], 25);
    // 市区町村は pref/city の集計そのまま
    let cities: i64 = tokyo["cities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["count"].as_i64().unwrap())
        .sum();
    assert_eq!(cities, 5);
    // 日本以外の地域には pref・cities・boundary_only が無い
    let fr = country(&body, "ne:FRA");
    assert!(region(fr, &f.fr_a).get("pref").is_none());
    assert!(region(fr, &f.fr_a).get("cities").is_none());
}

#[tokio::test]
async fn every_branch_of_the_classification_rules_lands_in_its_bucket() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let body = server.ok("").await;
    // 未判定: 25 区域外・26 競合・27 処理待ち・28 失敗・29 古い版・30 座標が違う行
    assert_eq!(body["unknown"]["unclassified"]["count"], 6);
    // 座標なし・無効: 31・32・33・34
    assert_eq!(body["unknown"]["no_location"]["count"], 4);
    assert_eq!(body["unknown"]["no_location"]["bbox"], Value::Null);
    // 処理待ち: 分類行が無い 27、古い版の 29、座標が合わない 30（失敗の印 28 は処理済み）
    assert_eq!(body["classification_pending"], 3);
    assert_eq!(
        body["classification_pending"].as_i64().unwrap(),
        geoframe::regions::pending_photos(&server.writer).unwrap()
    );
    // 座標が欠けた pref の写真（6）は日本の東京都に入る
    let tokyo = region(country(&body, "ne:JPN"), &f.tokyo);
    assert_eq!(tokyo["count"], 6);
}

#[tokio::test]
async fn totals_add_up_for_every_combination_of_period_bbox_and_hidden() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    // (クエリ, 同じ条件で数える SQL の WHERE)
    let timed = "taken_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]'";
    let cases: Vec<(&str, String)> = vec![
        ("", "COALESCE(hidden,0)=0".into()),
        ("?hidden=1", "1=1".into()),
        (
            "?from=2025-05-01+00:00&to=2025-06-30+23:59",
            format!("COALESCE(hidden,0)=0 AND {timed} AND taken_at >= '2025-05-01 00:00' AND taken_at <= '2025-06-30 23:59'"),
        ),
        (
            "?bbox=-10,40,10,50",
            "COALESCE(hidden,0)=0 AND lat IS NOT NULL AND lon IS NOT NULL AND lon >= -10 AND lon <= 10 AND lat >= 40 AND lat <= 50".into(),
        ),
        (
            "?bbox=-180,-90,180,90&hidden=1&from=2025-04-01+00:00",
            format!("{timed} AND taken_at >= '2025-04-01 00:00' AND lat IS NOT NULL AND lon IS NOT NULL AND lon >= -180 AND lon <= 180 AND lat >= -90 AND lat <= 90"),
        ),
    ];
    for (query, clause) in cases {
        let body = server.ok(query).await;
        let total = server.count(&format!("SELECT COUNT(*) FROM photos WHERE {clause}"));
        let sum_countries: i64 = countries(&body).iter().map(|c| c.1).sum();
        let unclassified = body["unknown"]["unclassified"]["count"].as_i64().unwrap();
        let no_location = body["unknown"]["no_location"]["count"].as_i64().unwrap();
        // Σ国 + 未判定 + 場所不明 = 全体（各写真を 1 回だけ数える）
        assert_eq!(sum_countries + unclassified + no_location, total, "{query}");
        // 国 = Σ地域 + 地域不明
        for c in body["countries"].as_array().unwrap() {
            let regions: i64 = c["regions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["count"].as_i64().unwrap())
                .sum();
            assert_eq!(
                c["count"].as_i64().unwrap(),
                regions + c["region_unknown"]["count"].as_i64().unwrap(),
                "{query} {}",
                c["id"]
            );
        }
        // 旧形式: `abroad` は pref の無い写真、`prefs` は pref のある写真
        let pref_sum: i64 = body["prefs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["count"].as_i64().unwrap())
            .sum();
        assert_eq!(
            pref_sum + body["abroad"].as_i64().unwrap(),
            total,
            "{query}"
        );
    }
    // bbox では座標のない写真は数えない
    let body = server.ok("?bbox=-180,-90,180,90").await;
    assert_eq!(body["unknown"]["no_location"]["count"], 0);
}

#[tokio::test]
async fn covers_come_from_the_same_photos_the_counts_do() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let body = server.ok("").await;
    let cover = |v: &Value| v["cover"]["id"].as_i64().unwrap();
    // 日本: 位置があり除外でなく日時がある中でいちばん新しい 10（境界データの国のみ）。東京都の地域は 9（境界データ）、
    // 旧 prefs の東京都は pref の写真だけなので 5
    let japan = country(&body, "ne:JPN");
    assert_eq!(cover(japan), 10);
    assert_eq!(cover(region(japan, &f.tokyo)), 9);
    assert_eq!(cover(&japan["region_unknown"]), 10);
    let old_tokyo = body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pref"] == "東京都")
        .unwrap();
    assert_eq!(cover(old_tokyo), 5);
    // フランス: 14（国のみ）。地域 A は 12。除外の 15 は選ばれない
    let fr = country(&body, "ne:FRA");
    assert_eq!(cover(fr), 14);
    assert_eq!(cover(region(fr, &f.fr_a)), 12);
    assert_eq!(cover(region(fr, &f.fr_b)), 13);
    // 除外を含めても、除外でない写真が先
    let hidden = server.ok("?hidden=1").await;
    assert_eq!(cover(country(&hidden, "ne:FRA")), 14);
    assert_eq!(cover(region(country(&hidden, "ne:FRA"), &f.fr_a)), 12);
    // 期間で絞ると、その期間の中から選ぶ
    let april_only = server
        .ok("?from=2025-04-01+00:00&to=2025-04-05+23:59")
        .await;
    let japan = country(&april_only, "ne:JPN");
    assert_eq!(cover(japan), 5);
    assert!(country_ids(&april_only) == ["ne:JPN"]);
    // 未判定の代表は未判定の写真から（8 月で位置があるもののうち新しい日時、同じなら ID が大きい）
    assert_eq!(body["unknown"]["unclassified"]["cover"]["id"], 30);
    assert!(
        !body["unknown"]["unclassified"]["cover"]["thumb_url"]
            .as_str()
            .unwrap()
            .is_empty()
    );
}

fn country_ids(body: &Value) -> Vec<&str> {
    countries(body).into_iter().map(|c| c.0).collect()
}

#[tokio::test]
async fn fit_bbox_is_the_minimal_arc_and_null_across_the_date_line() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let body = server.ok("").await;
    // 通常の範囲は min..max（既存と同じ形）
    let fj = country(&body, "ne:FJI");
    assert_eq!(fj["bbox"], json!([-179.0, -17.0, 178.0, -17.0]));
    // 178 と -179 は円周上で 3 度しか離れていない = 日付変更線をまたぐ
    assert_eq!(fj["fit_bbox"], Value::Null);
    assert_eq!(region(fj, &f.fiji)["fit_bbox"], Value::Null);
    // 経度が -100・-20・60・140 の広がりは、最小の区間が通常の範囲（空きが 80 度ずつ、戻る間が 120 度）
    let ru = country(&body, "ne:RUS");
    assert_eq!(ru["bbox"], json!([-100.0, 55.0, 140.0, 55.0]));
    assert_eq!(ru["fit_bbox"], ru["bbox"]);
    assert_eq!(ru["region_unknown"]["fit_bbox"], ru["bbox"]);
    // 小さな範囲はそのまま
    let fr = country(&body, "ne:FRA");
    assert_eq!(fr["fit_bbox"], fr["bbox"]);
    // 座標のない集計に範囲は無い
    assert_eq!(
        country(&body, "ne:JPN")["bbox"].as_array().unwrap().len(),
        4
    );
    // 期間でフィジーの片側だけに絞ると、またがなくなる
    let one = server.ok("?bbox=170,-20,180,-10").await;
    let fj = country(&one, "ne:FJI");
    assert_eq!(fj["count"], 1);
    assert_eq!(fj["fit_bbox"], fj["bbox"]);
}

#[tokio::test]
async fn without_the_table_pref_photos_stay_japan_and_the_rest_is_unclassified() {
    let f = fixture();
    // 分類の行は入れない（表を作らない）。pref の写真と同じ行だけ使う
    let photos_only: String = f
        .sql
        .lines()
        .filter(|l| !l.contains("photo_regions"))
        .collect::<Vec<_>>()
        .join("\n");
    for env in [&[("GEOFRAME_INGEST", "0")][..], &[][..]] {
        let server = Server::start(env, false, &photos_only).await;
        let body = server.ok("").await;
        assert_eq!(body["classification_available"], false, "{env:?}");
        // 日本だけ（pref の写真 7 枚）。残りは国が決まらない
        assert_eq!(countries(&body), [("ne:JPN", 7)]);
        let japan = country(&body, "ne:JPN");
        assert_eq!(region(japan, &f.tokyo)["count"], 5);
        assert_eq!(region(japan, &f.tokyo)["boundary_only"], 0);
        assert_eq!(japan["region_unknown"]["count"], 1);
        let unclassified = body["unknown"]["unclassified"]["count"].as_i64().unwrap();
        let no_location = body["unknown"]["no_location"]["count"].as_i64().unwrap();
        assert_eq!(no_location, 4);
        // 旧未分類（pref の無い写真）は、未判定と座標なし・無効に分かれる
        assert_eq!(
            unclassified + no_location,
            body["abroad"].as_i64().unwrap(),
            "{env:?}"
        );
        // すべて処理待ち扱い（表が無いので、pref 空で座標が有効な全件）
        assert_eq!(
            body["classification_pending"].as_i64().unwrap(),
            geoframe::regions::pending_photos(&server.writer).unwrap()
        );
        // API は表を作らない
        assert_eq!(
            server.count("SELECT COUNT(*) FROM sqlite_master WHERE name='photo_regions'"),
            0,
            "{env:?}"
        );
    }
}

#[tokio::test]
async fn reads_work_with_ingest_disabled_when_the_table_exists() {
    let f = fixture();
    let server = Server::start(&[("GEOFRAME_INGEST", "0")], true, &f.sql).await;
    let body = server.ok("").await;
    assert_eq!(body["classification_available"], true);
    assert_eq!(countries(&body)[0], ("ne:JPN", 9));
}

#[tokio::test]
async fn an_empty_library_has_empty_lists() {
    let server = Server::start(&[], true, "").await;
    let body = server.ok("").await;
    assert_eq!(body["countries"], json!([]));
    assert_eq!(body["unknown"]["unclassified"]["count"], 0);
    assert_eq!(body["classification_pending"], 0);
}

/// 変更前の `places` が上の合成データに返した `prefs`・`abroad`（`git show 109c253:crates/geoframe/src/admin_library.rs` の実装の出力を
/// 固定値にしたもの）。旧フィールドの意味が変わっていないこと（AC2）を、新しい集計のあとも同じ値で確かめる。
const OLD_QUERIES: [&str; 5] = [
    "",
    "?hidden=1",
    "?from=2025-04-03+00:00&to=2025-05-02+23:59",
    "?bbox=-10,30,150,50",
    "?bbox=-180,-90,180,90&hidden=1&from=2025-04-01+00:00",
];

#[tokio::test]
async fn old_prefs_and_abroad_are_what_they_were_before() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/places-old-golden.json")).unwrap();
    for query in OLD_QUERIES {
        let body = server.ok(query).await;
        let got = json!({"prefs": body["prefs"], "abroad": body["abroad"]});
        if std::env::var("PLACES_DUMP_GOLDEN").is_ok() {
            println!("GOLDEN\t{query}\t{got}");
            continue;
        }
        assert_eq!(got, golden[query], "{query}");
    }
}
