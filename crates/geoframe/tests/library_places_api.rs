//! `GET /api/admin/library/places` の国・地域別の集計と、`photos` の絞り込み・文字検索・`location_region`、`trips` の国・地域
//! （docs/international-places/design.md「管理API」）の結合テスト。
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

    async fn get_ok(&self, path: &str, query: &str) -> Value {
        let target = format!("{path}{query}");
        let response = get(self.addr, &target).await;
        assert_eq!(response.status, 200, "{target}");
        let body = response.json();
        assert_eq!(body["ok"], true, "{target}");
        body
    }

    /// `photos`（`per=200` で 1 ページに全部入れる）。
    async fn photos(&self, query: &str) -> Value {
        self.get_ok(
            "/api/admin/library/photos",
            &format!("?per=200&{}", query.trim_start_matches(['?', '&'])),
        )
        .await
    }

    async fn trips(&self, query: &str) -> Value {
        self.get_ok("/api/admin/library/trips", query).await
    }

    /// 状態だけ（400 などの確認）。
    async fn status(&self, path: &str, query: &str) -> u16 {
        get(self.addr, &format!("{path}{query}")).await.status
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
    // 旧 `prefs` は意味が変わらない: pref の写真だけを pref/city で数える。日本の地域は市区町村を持たず、`pref`（都道府県名）で
    // 旧 `prefs` の行を引く（市区町村は旧 `prefs` のものが唯一）。地域の件数 − boundary_only は旧 `prefs` のその都道府県の件数
    let prefs = body["prefs"].as_array().unwrap();
    let old = |name: &str| prefs.iter().find(|p| p["pref"] == name).unwrap();
    assert_eq!(old("東京都")["count"], 5);
    assert!(tokyo.get("cities").is_none());
    assert_eq!(
        tokyo["count"].as_i64().unwrap() - tokyo["boundary_only"].as_i64().unwrap(),
        old("東京都")["count"].as_i64().unwrap()
    );
    // 日本のすべての地域で同じ（旧 `prefs` に行が無い都道府県 = pref の写真が無い地域は 0）
    for r in japan["regions"].as_array().unwrap() {
        let name = r["pref"].as_str().unwrap();
        let old_count = prefs
            .iter()
            .find(|p| p["pref"] == name)
            .map_or(0, |p| p["count"].as_i64().unwrap());
        assert_eq!(
            r["count"].as_i64().unwrap() - r["boundary_only"].as_i64().unwrap(),
            old_count,
            "{name}"
        );
        assert!(r.get("cities").is_none(), "{name}");
    }
    assert_eq!(old("架空県")["count"], 1);
    // 旧 `prefs` の合計は pref のある写真（除外を除く）7 枚、`abroad` は pref の無い写真
    let pref_sum: i64 = prefs.iter().map(|p| p["count"].as_i64().unwrap()).sum();
    assert_eq!(pref_sum, 7);
    // 除外（8・15）を除く 32 枚のうち pref の無い 25 枚
    assert_eq!(body["abroad"], 25);
    // 市区町村は旧 `prefs` の pref/city の集計そのまま
    let cities: i64 = old("東京都")["cities"]
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
async fn unclassified_photos_are_checked_for_the_date_line_too() {
    // 国が決まらない写真（分類行なし）の経度は、集計と同じ走査で取る。経度 178・-179 は日付変更線をまたぐ
    let across = [
        photo(1, Some("2025-08-01 10:00"), "-17.0", "178.0", None, 0),
        photo(2, Some("2025-08-02 10:00"), "-17.0", "-179.0", None, 0),
    ]
    .concat();
    let server = Server::start(&[], true, &across).await;
    let body = server.ok("").await;
    let unclassified = &body["unknown"]["unclassified"];
    assert_eq!(unclassified["count"], 2);
    assert_eq!(unclassified["bbox"], json!([-179.0, -17.0, 178.0, -17.0]));
    assert_eq!(unclassified["fit_bbox"], Value::Null);
    // 広いが、最小の区間は通常の範囲（空きが 80 度ずつ、戻る間が 120 度）なら、通常の範囲のまま
    let wide = [-100.0, -20.0, 60.0, 140.0]
        .iter()
        .enumerate()
        .map(|(i, lon)| {
            photo(
                10 + i as i64,
                Some("2025-08-01 10:00"),
                "55.0",
                &lon.to_string(),
                None,
                0,
            )
        })
        .collect::<String>();
    let server = Server::start(&[], true, &wide).await;
    let body = server.ok("").await;
    let unclassified = &body["unknown"]["unclassified"];
    assert_eq!(unclassified["bbox"], json!([-100.0, 55.0, 140.0, 55.0]));
    assert_eq!(unclassified["fit_bbox"], unclassified["bbox"]);
    // 座標が無効な写真（場所不明）の経度は含めない
    let invalid = [
        photo(20, Some("2025-08-01 10:00"), "10.0", "20.0", None, 0),
        photo(21, Some("2025-08-01 10:00"), "95.0", "-170.0", None, 0),
    ]
    .concat();
    let server = Server::start(&[], true, &invalid).await;
    let body = server.ok("").await;
    assert_eq!(body["unknown"]["unclassified"]["count"], 1);
    assert_eq!(body["unknown"]["no_location"]["count"], 1);
    assert_eq!(
        body["unknown"]["unclassified"]["fit_bbox"],
        json!([20.0, 10.0, 20.0, 10.0])
    );
}

#[tokio::test]
async fn japan_pref_photos_wider_than_half_the_globe_skip_fit_but_keep_the_old_bbox() {
    // 日本の `pref` の写真が経度で 260 度（東京都の 140 と、誤った座標の -120）に広がる。通常の範囲は 180 度を超えるが、
    // `pref` の写真は経度をそろえないので、最小の区間は確かめずに `fit_bbox` は null。旧 `prefs` の bbox は変更前と同じ
    let rows = [
        photo(
            1,
            Some("2025-04-01 10:00"),
            "35.7",
            "140.0",
            Some("東京都"),
            0,
        ),
        photo(
            2,
            Some("2025-04-02 10:00"),
            "36.0",
            "-120.0",
            Some("東京都"),
            0,
        ),
        photo(
            3,
            Some("2025-04-03 10:00"),
            "34.7",
            "135.5",
            Some("大阪府"),
            0,
        ),
    ]
    .concat();
    let server = Server::start(&[], true, &rows).await;
    let body = server.ok("").await;
    let japan = country(&body, "ne:JPN");
    let tokyo_id = names().region_of_pref("東京都").unwrap().id.clone();
    let tokyo = region(japan, &tokyo_id);
    assert_eq!(tokyo["bbox"], json!([-120.0, 35.7, 140.0, 36.0]));
    assert_eq!(tokyo["fit_bbox"], Value::Null);
    // 国全体も、東京都を含むので広い。日付変更線をまたぐかを確かめない側（null）に倒す
    assert_eq!(japan["bbox"], json!([-120.0, 34.7, 140.0, 36.0]));
    assert_eq!(japan["fit_bbox"], Value::Null);
    // 大阪府だけの地域は狭いので、通常の範囲そのまま
    let osaka = region(japan, &names().region_of_pref("大阪府").unwrap().id);
    assert_eq!(osaka["fit_bbox"], osaka["bbox"]);
    // 旧 `prefs` の bbox は従来どおりの通常の範囲（min..max）
    let old = body["prefs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pref"] == "東京都")
        .unwrap();
    assert_eq!(old["bbox"], json!([-120.0, 35.7, 140.0, 36.0]));
    assert_eq!(old["cities"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_coordinates_never_make_an_invalid_bbox_or_fit_bbox() {
    // 東京都: 緯度が範囲外の写真だけ。大阪府: 有効な写真 + 経度が範囲外の写真。京都府: 文字列の座標だけ。
    // 北海道: 有効な写真だけ。範囲外・文字列の座標は件数には数えるが、新しい bbox・fit_bbox には入れない
    let rows = [
        photo(1, Some("2025-04-01 10:00"), "95", "139", Some("東京都"), 0),
        photo(
            2,
            Some("2025-04-02 10:00"),
            "34.7",
            "135.5",
            Some("大阪府"),
            0,
        ),
        photo(
            3,
            Some("2025-04-03 10:00"),
            "34.0",
            "200",
            Some("大阪府"),
            0,
        ),
        photo(4, Some("2025-04-04 10:00"), "'x'", "'y'", Some("京都府"), 0),
        photo(
            5,
            Some("2025-04-05 10:00"),
            "43.0",
            "141.3",
            Some("北海道"),
            0,
        ),
        // `pref` の無い写真: 範囲外の座標は場所不明（`no_location`）で、範囲を持たない
        photo(6, Some("2025-04-06 10:00"), "95", "10", None, 0),
    ]
    .concat();
    let server = Server::start(&[], true, &rows).await;
    let body = server.ok("").await;
    let japan = country(&body, "ne:JPN");
    let by_pref = |pref: &str| region(japan, &names().region_of_pref(pref).unwrap().id).clone();
    let tokyo = by_pref("東京都");
    assert_eq!(tokyo["count"], 1);
    assert_eq!(tokyo["bbox"], Value::Null);
    assert_eq!(tokyo["fit_bbox"], Value::Null);
    let osaka = by_pref("大阪府");
    assert_eq!(osaka["count"], 2);
    assert_eq!(osaka["bbox"], json!([135.5, 34.7, 135.5, 34.7]));
    assert_eq!(osaka["fit_bbox"], osaka["bbox"]);
    let kyoto = by_pref("京都府");
    assert_eq!(kyoto["count"], 1);
    assert_eq!(kyoto["bbox"], Value::Null);
    assert_eq!(kyoto["fit_bbox"], Value::Null);
    let hokkaido = by_pref("北海道");
    assert_eq!(hokkaido["bbox"], json!([141.3, 43.0, 141.3, 43.0]));
    assert_eq!(hokkaido["fit_bbox"], hokkaido["bbox"]);
    // 国全体は有効な写真だけの範囲
    assert_eq!(japan["count"], 5);
    assert_eq!(japan["bbox"], json!([135.5, 34.7, 141.3, 43.0]));
    assert_eq!(japan["fit_bbox"], japan["bbox"]);
    // 旧 `prefs` の bbox は変更前のまま（数値の座標の写真の MIN/MAX）
    let old = |pref: &str| {
        body["prefs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["pref"] == pref)
            .unwrap()
            .clone()
    };
    assert_eq!(old("東京都")["bbox"], json!([139.0, 95.0, 139.0, 95.0]));
    assert_eq!(old("大阪府")["bbox"], json!([135.5, 34.0, 200.0, 34.7]));
    // 場所不明は範囲も fit_bbox も持たない
    assert_eq!(body["unknown"]["no_location"]["count"], 1);
    assert_eq!(body["unknown"]["no_location"]["bbox"], Value::Null);
    assert_eq!(body["unknown"]["no_location"]["fit_bbox"], Value::Null);
    // どの bbox・fit_bbox も、出ているなら有効（緯度 ±90・経度 ±180・最小 ≤ 最大）
    let mut all = vec![japan.clone()];
    all.extend(japan["regions"].as_array().unwrap().iter().cloned());
    all.push(japan["region_unknown"].clone());
    all.push(body["unknown"]["unclassified"].clone());
    for item in &all {
        for key in ["bbox", "fit_bbox"] {
            if let Some(b) = item[key].as_array() {
                let b: Vec<f64> = b.iter().map(|v| v.as_f64().unwrap()).collect();
                assert!(
                    b[0] >= -180.0 && b[2] <= 180.0 && b[1] >= -90.0 && b[3] <= 90.0,
                    "{item}"
                );
                assert!(b[0] <= b[2] && b[1] <= b[3], "{item}");
            }
        }
    }
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

// ---------------------------------------------------------------- photos・trips（段②）

/// クエリの値を `%XX` にする（ASCII 英数字と `-_.~:,+` 以外）。
fn enc(query: &str) -> String {
    query
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~:,+=&".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn ids(body: &Value) -> Vec<i64> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect()
}

fn sorted_ids(body: &Value) -> Vec<i64> {
    let mut v = ids(body);
    v.sort_unstable();
    v
}

/// `location_region` を外した写真（変更前と同じ項目だけ）。
fn without_location_region(item: &Value) -> Value {
    let mut item = item.clone();
    item.as_object_mut().unwrap().remove("location_region");
    item
}

/// 変更前の `photos` が上の合成データに返した結果（旧形式のクエリ。`total`・`pages`・ID の並び、一部は項目全体）。AC2 を、
/// 変更後も同じ値で確かめる。固定値は変更前の実装（段②の前のコミット）の出力で、新しい実装の出力を写していない。
const LEGACY_PHOTO_QUERIES: [&str; 19] = [
    "pref=東京都",
    "pref=東京都&city=市1",
    "city=市1",
    "city=市0&hidden=1",
    "abroad=1",
    "abroad=1&hidden=1",
    "pref=架空県",
    "pref=東京都&from=2025-04-02+00:00&to=2025-04-06+23:59",
    "abroad=1&bbox=-10,40,10,50",
    "pref=大阪府&bbox=-180,-90,180,90",
    "q=東京",
    "q=市1",
    "q=P1",
    "q=2025-05",
    "q=東京+市1",
    "untimed=1",
    "ids=1,9,11,31&hidden=1",
    "from=2025-05-01+00:00&to=2025-06-30+23:59",
    "q=東京&abroad=1",
];
/// 項目全体も固定する旧形式のクエリ。
const LEGACY_FULL_ITEMS: [&str; 2] = ["pref=東京都&city=市1", "ids=1,9,11,31&hidden=1"];

#[tokio::test]
async fn legacy_photo_queries_return_what_they_returned_before() {
    let f = fixture();
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/photos-old-golden.json")).unwrap();
    // 表がある DB と、旧 DB（表なし）の両方で、旧形式の結果は同じ
    let photos_only: String = f
        .sql
        .lines()
        .filter(|l| !l.contains("photo_regions"))
        .collect::<Vec<_>>()
        .join("\n");
    for with_table in [true, false] {
        let server = Server::start(
            &[],
            with_table,
            if with_table { &f.sql } else { &photos_only },
        )
        .await;
        for query in LEGACY_PHOTO_QUERIES {
            let body = server.photos(&enc(query)).await;
            let mut got =
                json!({"total": body["total"], "pages": body["pages"], "ids": ids(&body)});
            if LEGACY_FULL_ITEMS.contains(&query) {
                got["items"] = Value::Array(
                    body["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(without_location_region)
                        .collect(),
                );
            }
            if std::env::var("PLACES_DUMP_GOLDEN").is_ok() {
                println!("GOLDEN\tphotos\t{query}\t{got}");
                continue;
            }
            assert_eq!(got, golden[query], "{query} table={with_table}");
        }
    }
}

#[tokio::test]
async fn trips_keep_their_old_fields_and_places() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/trips-old-golden.json")).unwrap();
    for query in ["", "?hidden=1"] {
        let mut body = server.trips(query).await;
        for trip in body["trips"].as_array_mut().unwrap() {
            trip.as_object_mut().unwrap().remove("regions");
        }
        if std::env::var("PLACES_DUMP_GOLDEN").is_ok() {
            println!("GOLDEN\ttrips\t{query}\t{body}");
            continue;
        }
        assert_eq!(body, golden[query], "{query}");
    }
}

const PHOTOS: &str = "/api/admin/library/photos";

#[tokio::test]
async fn the_new_form_selects_countries_regions_and_the_unknowns() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let sel = |query: String| {
        let server = &server;
        async move { sorted_ids(&server.photos(&query).await) }
    };
    let (tokyo, fr_a, fr_b) = (&f.tokyo, &f.fr_a, &f.fr_b);
    // 日本: pref の写真（1〜7。8 は除外）と境界データの 9・10
    assert_eq!(
        sel("country=ne:JPN".into()).await,
        [1, 2, 3, 4, 5, 6, 7, 9, 10]
    );
    assert_eq!(
        sel("country=ne:JPN&hidden=1".into()).await,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    );
    // 東京都: pref の 1・2・3・5・6（座標なしの 6 も）と境界データの 9
    assert_eq!(
        sel(format!("country=ne:JPN&region={tokyo}")).await,
        [1, 2, 3, 5, 6, 9]
    );
    // 日本の地域不明: 47 件に無い pref の 7 と、国だけ決まった 10
    assert_eq!(sel("country=ne:JPN&region_unknown=1".into()).await, [7, 10]);
    // 日本の市区町村: pref = 東京都 AND city = 市1（奇数の ID）
    assert_eq!(
        sel(enc(&format!("country=ne:JPN&region={tokyo}&city=市1"))).await,
        [1, 3, 5]
    );
    assert_eq!(
        sel(enc(&format!("country=ne:JPN&region={tokyo}&city=市0"))).await,
        [2, 6]
    );
    // 市区町村は境界データの写真（city なし）には当たらない
    assert!(
        sel(enc(&format!("country=ne:JPN&region={tokyo}&city=")))
            .await
            .len()
            >= 5
    );
    // フランス（15 は除外）
    assert_eq!(sel("country=ne:FRA".into()).await, [11, 12, 13, 14]);
    assert_eq!(
        sel("country=ne:FRA&hidden=1".into()).await,
        [11, 12, 13, 14, 15]
    );
    assert_eq!(sel(format!("country=ne:FRA&region={fr_a}")).await, [11, 12]);
    assert_eq!(sel(format!("country=ne:FRA&region={fr_b}")).await, [13]);
    assert_eq!(sel("country=ne:FRA&region_unknown=1".into()).await, [14]);
    // 同名の地域は国で分かれる
    assert_eq!(
        sel(format!("country=ne:ARG&region={}", f.la_rioja_ar)).await,
        [16]
    );
    assert_eq!(
        sel(format!("country=ne:ESP&region={}", f.la_rioja_es)).await,
        [17, 18]
    );
    assert_eq!(sel("country=ne:ESP".into()).await, [17, 18]);
    // 日付変更線をまたぐフィジーと、地域不明だけのロシア
    assert_eq!(sel("country=ne:FJI".into()).await, [19, 20]);
    assert_eq!(
        sel(format!("country=ne:FJI&region={}", f.fiji)).await,
        [19, 20]
    );
    assert_eq!(sel("country=ne:RUS".into()).await, [21, 22, 23, 24]);
    assert_eq!(
        sel("country=ne:RUS&region_unknown=1".into()).await,
        [21, 22, 23, 24]
    );
    // 未判定: 25 区域外・26 競合・27 処理待ち・28 失敗・29 古い版・30 座標が違う行
    assert_eq!(sel("unclassified=1".into()).await, [25, 26, 27, 28, 29, 30]);
    // 場所不明: 31 座標なし・32 緯度なし・33 緯度 95・34 緯度が文字列
    assert_eq!(sel("no_location=1".into()).await, [31, 32, 33, 34]);
    // 期間・範囲・除外と組み合わせる
    assert_eq!(
        sel("country=ne:FRA&bbox=-10,40,10,50&from=2025-05-02+00:00".into()).await,
        [12, 13, 14]
    );
    // 範囲は両端を含む（27 は (2, 2)）
    assert_eq!(
        sel("unclassified=1&bbox=-1,-1,2,2".into()).await,
        [25, 26, 27]
    );
    assert_eq!(
        sel("no_location=1&bbox=-180,-90,180,90".into()).await,
        [] as [i64; 0]
    );
    // 旗の 0 は無いのと同じ
    assert_eq!(
        sel("country=ne:FRA&unclassified=0&no_location=0&region_unknown=0".into()).await,
        [11, 12, 13, 14]
    );
    // 文法が正しい未知の ID は 0 件（400 にしない）
    for query in [
        "country=ne:ZZZ".to_owned(),
        "country=ne:FRA&region=ne:FRA-ZZZ".to_owned(),
        "country=ne:ZZZ&region_unknown=1".to_owned(),
        format!("country=ne:JPN&region={tokyo}&city=存在しない市"),
        format!("country=a{}", "b".repeat(63)),
    ] {
        let body = server.photos(&enc(&query)).await;
        assert_eq!(body["total"], 0, "{query}");
        assert_eq!(body["items"], json!([]), "{query}");
    }
    // ページング: 日本の 9 枚を 4 枚ずつ（撮影日時の順）
    let page = server.get_ok(PHOTOS, "?country=ne:JPN&per=4&page=2").await;
    assert_eq!(
        (
            page["total"].as_i64(),
            page["pages"].as_i64(),
            page["page"].as_i64()
        ),
        (Some(9), Some(3), Some(2))
    );
    assert_eq!(ids(&page), [5, 6, 7, 9]);
}

#[tokio::test]
async fn bad_place_queries_are_rejected_without_echoing_the_value() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let (tokyo, fr_a) = (&f.tokyo, &f.fr_a);
    let long = "a".repeat(65);
    let bad = [
        // 地域だけ・地域不明だけ
        format!("region={tokyo}"),
        "region_unknown=1".to_owned(),
        // 国と地域・地域不明の同時
        format!("country=ne:JPN&region={tokyo}&region_unknown=1"),
        // 区分どうし・国と区分
        "unclassified=1&no_location=1".to_owned(),
        "country=ne:JPN&unclassified=1".to_owned(),
        "country=ne:JPN&no_location=1".to_owned(),
        "unclassified=1&region_unknown=1".to_owned(),
        // 別の国の地域
        format!("country=ne:JPN&region={fr_a}"),
        format!("country=ne:FRA&region={tokyo}"),
        format!("country=ne:ESP&region={}", f.la_rioja_ar),
        // 旧形式との混用
        "country=ne:JPN&pref=東京都".to_owned(),
        "country=ne:JPN&abroad=1".to_owned(),
        "unclassified=1&pref=東京都".to_owned(),
        "no_location=1&abroad=1".to_owned(),
        "unclassified=1&city=市1".to_owned(),
        format!("region={tokyo}&pref=東京都"),
        // 新形式の city は日本の地域のときだけ
        "country=ne:JPN&city=市1".to_owned(),
        "country=ne:JPN&region_unknown=1&city=市1".to_owned(),
        format!("country=ne:FRA&region={fr_a}&city=市1"),
        "country=ne:FRA&city=市1".to_owned(),
        "country=ne:JPN&region=ne:JPN-ZZZ&city=市1".to_owned(),
        // ID の文法
        "country=ne;FRA".to_owned(),
        "country=ne:FRA'--".to_owned(),
        "country=日本".to_owned(),
        format!("country={long}"),
        format!("country=ne:JPN&region={long}"),
        "country=ne:JPN&region=a b".to_owned(),
        // 旗は 0/1 だけ
        "unclassified=2".to_owned(),
        "no_location=true".to_owned(),
        "country=ne:JPN&region_unknown=yes".to_owned(),
        // 市区町村の長さ
        format!("country=ne:JPN&region={tokyo}&city={}", "あ".repeat(65)),
        // 期間・範囲の形
        "country=ne:JPN&from=bad".to_owned(),
        "country=ne:JPN&bbox=1,2,3".to_owned(),
    ];
    for query in bad {
        let response = get(server.addr, &format!("{PHOTOS}?{}", enc(&query))).await;
        assert_eq!(response.status, 400, "{query}");
        assert_eq!(response.json()["error"], "bad_params", "{query}");
        // 値を応答に出さない
        assert!(!response.text().contains("ne:"), "{query}");
    }
    // 旧形式の混用（変更前から 400）
    for query in ["abroad=1&pref=東京都", "abroad=1&city=市1"] {
        assert_eq!(
            server.status(PHOTOS, &format!("?{}", enc(query))).await,
            400,
            "{query}"
        );
    }
}

#[tokio::test]
async fn each_selection_returns_as_many_photos_as_places_counts() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    // places と photos に同じ期間・範囲・除外を掛けて、区分ごとの件数が一致する（AC5）
    for query in [
        "",
        "hidden=1",
        "from=2025-05-01+00:00&to=2025-06-30+23:59",
        "bbox=-10,40,10,50",
        "bbox=-180,-90,180,90&hidden=1&from=2025-04-01+00:00",
        "bbox=100,20,150,50",
        "from=2025-07-01+00:00&to=2025-08-31+23:59&hidden=1",
    ] {
        let places = server.ok(&format!("?{query}")).await;
        let mut checked = 0;
        let total = |selection: String| {
            let server = &server;
            let query = query.to_owned();
            async move {
                let body = server.photos(&format!("{selection}&{query}")).await;
                // total と実際に返る枚数が合う（per=200 で 1 ページに収まる）
                assert_eq!(
                    body["total"].as_u64(),
                    Some(ids(&body).len() as u64),
                    "{selection} {query}"
                );
                body["total"].as_i64().unwrap()
            }
        };
        for c in places["countries"].as_array().unwrap() {
            let id = c["id"].as_str().unwrap();
            assert_eq!(
                total(format!("country={id}")).await,
                c["count"],
                "{query} {id}"
            );
            for r in c["regions"].as_array().unwrap() {
                let rid = r["id"].as_str().unwrap();
                assert_eq!(
                    total(format!("country={id}&region={rid}")).await,
                    r["count"],
                    "{query} {id} {rid}"
                );
                checked += 1;
            }
            assert_eq!(
                total(format!("country={id}&region_unknown=1")).await,
                c["region_unknown"]["count"],
                "{query} {id} 地域不明"
            );
            checked += 1;
        }
        assert_eq!(
            total("unclassified=1".into()).await,
            places["unknown"]["unclassified"]["count"],
            "{query} 未判定"
        );
        assert_eq!(
            total("no_location=1".into()).await,
            places["unknown"]["no_location"]["count"],
            "{query} 場所不明"
        );
        // 旧形式の `abroad`（pref が空）は、新しい区分では「pref の無い国 + 未判定 + 場所不明」
        assert!(
            checked > 0 || places["countries"].as_array().unwrap().is_empty(),
            "{query}"
        );
    }
    // 日本の地域の件数 − boundary_only = 旧形式の `pref` の件数
    let places = server.ok("").await;
    let japan = country(&places, "ne:JPN");
    for r in japan["regions"].as_array().unwrap() {
        let Some(pref) = r["pref"].as_str() else {
            continue;
        };
        let old = server.photos(&enc(&format!("pref={pref}"))).await;
        assert_eq!(
            r["count"].as_i64().unwrap() - r["boundary_only"].as_i64().unwrap(),
            old["total"].as_i64().unwrap(),
            "{pref}"
        );
    }
}

/// 分類行の検索用文字列を、同梱の名称表から入れる（本物の分類段が書く値）。無効な行（29・30）にも入れて、有効性で外れることを確かめる。
fn fill_search_text(conn: &rusqlite::Connection) {
    let rows: Vec<(i64, Option<String>, Option<String>)> = conn
        .prepare("SELECT photo_id, country_id, region_id FROM photo_regions")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (id, country, region) in rows {
        if let Some(country) = country {
            let text = geoframe::regions::search_text(&country, region.as_deref()).unwrap();
            conn.execute(
                "UPDATE photo_regions SET search_text=?1 WHERE photo_id=?2",
                rusqlite::params![text, id],
            )
            .unwrap();
        }
    }
}

#[tokio::test]
async fn q_finds_countries_and_regions_by_japanese_english_and_original_names() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    fill_search_text(&server.writer);
    let q = |query: &str| {
        let server = &server;
        let query = enc(&format!("q={query}"));
        async move { sorted_ids(&server.photos(&query).await) }
    };
    let france = names().country("ne:FRA").unwrap();
    let region_a = names().region(&f.fr_a).unwrap();
    // 国: 日本語・英語・別名のどれでも。有効な分類の写真だけ（29 の旧版・30 の座標違いは外れる。15 は除外）
    for alias in &france.search {
        assert_eq!(q(alias).await, [11, 12, 13, 14], "{alias}");
    }
    assert_eq!(q("フランス").await, [11, 12, 13, 14]);
    assert_eq!(
        q("france").await,
        [11, 12, 13, 14],
        "大文字小文字を区別しない"
    );
    // 地域: 日本語・英語・原表記
    for alias in &region_a.search {
        let got = q(alias).await;
        assert!(got.contains(&11) && got.contains(&12), "{alias}: {got:?}");
        assert!(!got.contains(&29) && !got.contains(&30), "{alias}: {got:?}");
    }
    // 日本: pref の写真（1〜7。47 件に無い pref の 7 も）と、境界データの 9・10 が「日本」「Japan」で当たる
    let all_japan = [1, 2, 3, 4, 5, 6, 7, 9, 10];
    assert_eq!(q("日本").await, all_japan);
    assert_eq!(q("Japan").await, all_japan);
    // 都道府県は pref の写真も境界データの写真も、日本語・英語・原表記で当たる
    let tokyo = names().region(&f.tokyo).unwrap();
    for alias in &tokyo.search {
        assert_eq!(q(alias).await, [1, 2, 3, 5, 6, 9], "{alias}");
    }
    assert_eq!(q("Tokyo").await, [1, 2, 3, 5, 6, 9]);
    assert_eq!(q("大阪府").await, [4]);
    // 語は AND（日本の写真のうち東京）
    assert_eq!(q("日本 Tokyo").await, [1, 2, 3, 5, 6, 9]);
    assert_eq!(q("日本　フランス").await, [] as [i64; 0]);
    // 既存の列（市区町村・ファイル名・撮影日時）も引き続き当たる
    assert_eq!(q("市1").await, [1, 3, 5, 7]);
    assert_eq!(q("P11").await, [11]);
    assert_eq!(q("2025-05").await, [11, 12, 13, 14]);
    // 除外は hidden=1 のときだけ
    assert_eq!(
        sorted_ids(&server.photos(&enc("q=フランス&hidden=1")).await),
        [11, 12, 13, 14, 15]
    );
    // 国・地域の絞り込みと組み合わせる
    assert_eq!(
        sorted_ids(
            &server
                .photos(&enc("q=Tokyo&country=ne:JPN&region_unknown=1"))
                .await
        ),
        [] as [i64; 0]
    );
    assert_eq!(
        sorted_ids(&server.photos(&enc("q=日本&unclassified=1")).await),
        [] as [i64; 0]
    );
    // LIKE の記号は文字として扱う
    assert_eq!(q("%").await, [] as [i64; 0]);
    assert_eq!(q("_").await, [] as [i64; 0]);
}

/// 国名と地域名の両方に一致する語でも、写真は 1 回だけ返る（JOIN が行を増やさない）。
#[tokio::test]
async fn a_word_matching_both_the_country_and_the_region_returns_the_photo_once() {
    let f = fixture();
    // 西オーストラリア州の写真（国名も地域名も「オーストラリア」「Australia」を含む）
    let version = geoframe::regions::classifier_version().unwrap();
    let mut sql = f.sql.clone();
    sql += "BEGIN;\n";
    sql += &photo(101, Some("2025-09-01 10:00"), "-31.9", "115.8", None, 0);
    sql += &photo(102, Some("2025-09-02 10:00"), "-33.9", "151.2", None, 0);
    sql += &class_row(
        101,
        "-31.9",
        "115.8",
        version,
        Some("ne:AUS"),
        Some("ne:AUS-2651"),
        ("inside", "inside"),
    );
    sql += &class_row(
        102,
        "-33.9",
        "151.2",
        version,
        Some("ne:AUS"),
        None,
        ("inside", "outside"),
    );
    sql += "COMMIT;\n";
    let server = Server::start(&[], true, &sql).await;
    fill_search_text(&server.writer);
    let country = names().country("ne:AUS").unwrap();
    let region = names().region("ne:AUS-2651").unwrap();
    for word in ["Australia", "オーストラリア", "australia", "tralia"] {
        assert!(
            country
                .search
                .iter()
                .any(|s| s.to_lowercase().contains(&word.to_lowercase()))
                || word == "tralia"
        );
        assert!(
            region
                .search
                .iter()
                .any(|s| s.to_lowercase().contains(&word.to_lowercase()))
                || word == "tralia"
        );
        let body = server.photos(&enc(&format!("q={word}"))).await;
        // 101（国名も地域名も当たる）と 102（国名だけ）が 1 回ずつ
        assert_eq!(ids(&body), [101, 102], "{word}");
        assert_eq!(body["total"], 2, "{word}");
        let body = server
            .photos(&enc(&format!("q={word}&country=ne:AUS")))
            .await;
        assert_eq!(ids(&body), [101, 102], "{word}");
        assert_eq!(body["total"], 2, "{word}");
    }
    // 地域名だけに当たる語は、その地域の写真だけ
    let body = server.photos(&enc("q=Western")).await;
    assert_eq!(ids(&body), [101]);
}

#[tokio::test]
async fn an_old_database_without_the_table_still_searches_and_filters() {
    let f = fixture();
    let photos_only: String = f
        .sql
        .lines()
        .filter(|l| !l.contains("photo_regions"))
        .collect::<Vec<_>>()
        .join("\n");
    for env in [&[("GEOFRAME_INGEST", "0")][..], &[][..]] {
        let server = Server::start(env, false, &photos_only).await;
        let q = |query: &str| {
            let server = &server;
            let query = enc(&format!("q={query}"));
            async move { sorted_ids(&server.photos(&query).await) }
        };
        // pref の写真は日本・都道府県で当たる。分類が無い写真は国名で当たらない
        assert_eq!(q("日本").await, [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(q("Tokyo").await, [1, 2, 3, 5, 6]);
        assert_eq!(q("フランス").await, [] as [i64; 0]);
        assert_eq!(q("P11").await, [11]);
        let sel = |query: &str| {
            let server = &server;
            let query = query.to_owned();
            async move { sorted_ids(&server.photos(&enc(&query)).await) }
        };
        assert_eq!(sel("country=ne:JPN").await, [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(sel("country=ne:FRA").await, [] as [i64; 0]);
        // pref が空で座標が有効な写真（除外の 15 を除く）は、すべて未判定
        assert_eq!(
            sel("unclassified=1").await,
            (9..=30).filter(|id| *id != 15).collect::<Vec<i64>>()
        );
        assert_eq!(sel("no_location=1").await, [31, 32, 33, 34]);
        // 表は作られない
        assert_eq!(
            server.count("SELECT COUNT(*) FROM sqlite_master WHERE name='photo_regions'"),
            0
        );
        // location_region は pref の写真だけ
        let body = server.photos("ids=1,11").await;
        assert_eq!(body["items"][0]["location_region"]["country_id"], "ne:JPN");
        assert!(body["items"][1].get("location_region").is_none());
        // trips も動く
        let trips = server.trips("").await;
        assert!(!trips["trips"].as_array().unwrap().is_empty());
    }
}

fn location_of(body: &Value, id: i64) -> Option<&Value> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("写真 {id} が無い"))
        .get("location_region")
}

#[tokio::test]
async fn location_region_describes_pref_and_boundary_classifications_and_hides_the_rest() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    // 吹き出しは `photos?ids=` で同じ項目を読む
    let body = server
        .photos("ids=1,6,7,9,10,11,14,16,25,26,27,28,29,30,31,32,33,34&hidden=1")
        .await;
    let tokyo_name = &names().region(&f.tokyo).unwrap().name;
    // pref の写真: 国=日本、地域=その都道府県（地域 ID は対応表の ID）、判定方法 pref。座標が無い 6 も
    for id in [1, 6] {
        assert_eq!(
            location_of(&body, id).unwrap(),
            &json!({
                "country_id": "ne:JPN", "country": "日本",
                "region_id": f.tokyo, "region": tokyo_name,
                "method": "pref", "label": format!("日本・{tokyo_name}"),
            }),
            "{id}"
        );
    }
    // 47 件に無い pref: 日本、地域は不明
    assert_eq!(
        location_of(&body, 7).unwrap(),
        &json!({
            "country_id": "ne:JPN", "country": "日本", "region_id": null, "region": null,
            "method": "pref", "label": "日本",
        })
    );
    // 境界データの分類: 判定方法 inside。日本の地域は pref の写真と同じ ID
    assert_eq!(
        location_of(&body, 9).unwrap(),
        &json!({
            "country_id": "ne:JPN", "country": "日本",
            "region_id": f.tokyo, "region": tokyo_name,
            "method": "inside", "label": format!("日本・{tokyo_name}"),
        })
    );
    assert_eq!(location_of(&body, 10).unwrap()["region_id"], Value::Null);
    assert_eq!(location_of(&body, 10).unwrap()["method"], "inside");
    let fr = names().country("ne:FRA").unwrap();
    let a = names().region(&f.fr_a).unwrap();
    assert_eq!(
        location_of(&body, 11).unwrap(),
        &json!({
            "country_id": "ne:FRA", "country": fr.name,
            "region_id": f.fr_a, "region": a.name,
            "method": "inside", "label": format!("{}・{}", fr.name, a.name),
        })
    );
    assert_eq!(
        location_of(&body, 14).unwrap(),
        &json!({
            "country_id": "ne:FRA", "country": fr.name, "region_id": null, "region": null,
            "method": "inside", "label": fr.name,
        })
    );
    // 国が決まらない写真には付けない: 25 区域外・26 競合・27 処理待ち・28 失敗・29 古い版・30 座標違い、31〜34 場所不明
    for id in [25, 26, 27, 28, 29, 30, 31, 32, 33, 34] {
        assert!(location_of(&body, id).is_none(), "{id}");
    }
    // 分類版や入力座標は出さない
    let text = serde_json::to_string(&body).unwrap();
    for leaked in [
        "classifier",
        "input_lat",
        "gf1-",
        "country_method",
        "search_text",
    ] {
        assert!(!text.contains(leaked), "{leaked}");
    }
    // 日本語以外の名前は ID でなく名称表の名前（同名の地域も国で区別できる）
    let body = server.photos("ids=16,17").await;
    assert_eq!(location_of(&body, 16).unwrap()["country_id"], "ne:ARG");
    assert_eq!(location_of(&body, 17).unwrap()["country_id"], "ne:ESP");
    assert_eq!(
        location_of(&body, 16).unwrap()["region"],
        location_of(&body, 17).unwrap()["region"]
    );
}

#[tokio::test]
async fn a_classification_stops_being_returned_when_the_coordinates_or_the_version_change() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let located = |body: &Value, id| location_of(body, id).is_some();
    let in_france = || async { sorted_ids(&server.photos("country=ne:FRA").await) };
    assert!(located(&server.photos("ids=11,13").await, 11));
    assert_eq!(in_france().await, [11, 12, 13, 14]);
    // 座標が変わった写真（分類は古い座標のまま）→ 分類を返さず、未判定に入る
    server
        .writer
        .execute("UPDATE photos SET lat=10.0, lon=10.0 WHERE id=11", [])
        .unwrap();
    let body = server.photos("ids=11,13").await;
    assert!(!located(&body, 11));
    assert!(located(&body, 13));
    assert_eq!(in_france().await, [12, 13, 14]);
    assert!(sorted_ids(&server.photos("unclassified=1").await).contains(&11));
    // 座標が無効になった → 場所不明
    server
        .writer
        .execute("UPDATE photos SET lat=95.0 WHERE id=12", [])
        .unwrap();
    assert!(!located(&server.photos("ids=12").await, 12));
    assert!(sorted_ids(&server.photos("no_location=1").await).contains(&12));
    // 分類版が今の同梱版でない行（再分類待ち）→ 返さない
    server
        .writer
        .execute(
            "UPDATE photo_regions SET classifier_version='gf0-old' WHERE photo_id=13",
            [],
        )
        .unwrap();
    assert!(!located(&server.photos("ids=13").await, 13));
    assert_eq!(in_france().await, [14]);
    // 判定できなかった印（failed）→ 返さない
    server
        .writer
        .execute(
            "UPDATE photo_regions SET country_method='failed', region_method='failed' WHERE photo_id=14",
            [],
        )
        .unwrap();
    assert!(!located(&server.photos("ids=14").await, 14));
    assert_eq!(in_france().await, [] as [i64; 0]);
    // pref の写真は座標が変わっても pref のまま（規則 1）
    server
        .writer
        .execute("UPDATE photos SET lat=-10.0, lon=-10.0 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        location_of(&server.photos("ids=1").await, 1).unwrap()["method"],
        "pref"
    );
    // places も同じ見え方（Σ国 + 未判定 + 場所不明 = 全体）
    let places = server.ok("").await;
    let sum: i64 = countries(&places).iter().map(|c| c.1).sum();
    assert_eq!(
        sum + places["unknown"]["unclassified"]["count"].as_i64().unwrap()
            + places["unknown"]["no_location"]["count"].as_i64().unwrap(),
        server.count("SELECT COUNT(*) FROM photos WHERE COALESCE(hidden,0)=0")
    );
}

#[tokio::test]
async fn trips_name_the_top_countries_and_regions_and_keep_the_old_places() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    let tokyo_name = names().region(&f.tokyo).unwrap().name.clone();
    let osaka = names().region_of_pref("大阪府").unwrap();
    let fr = names().country("ne:FRA").unwrap().name.clone();
    let key = |t: &Value| {
        t["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["country_id"].as_str().unwrap().to_owned(),
                    r["region_id"].as_str().map(str::to_owned),
                    r["count"].as_i64().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let body = server.trips("").await;
    let by_start = |start: &str| {
        body["trips"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["start"] == start)
            .unwrap_or_else(|| panic!("{start} の旅が無い"))
    };
    // 4 月: 東京都 6（pref の 1・2・3・5・6 と境界データの 9）・日本の地域不明 2（7 と 10）・大阪府 1
    let april = by_start("2025-04-01 10:00");
    assert_eq!(
        key(april),
        [
            ("ne:JPN".to_owned(), Some(f.tokyo.clone()), 6),
            ("ne:JPN".to_owned(), None, 2),
            ("ne:JPN".to_owned(), Some(osaka.id.clone()), 1),
        ]
    );
    // 名前と表示用のラベル
    assert_eq!(april["regions"][0]["country"], "日本");
    assert_eq!(april["regions"][0]["region"], tokyo_name);
    assert_eq!(april["regions"][0]["label"], format!("日本・{tokyo_name}"));
    assert_eq!(april["regions"][1]["label"], "日本");
    // 旧 pref/city の候補は変わらない（海外・不明は pref が空の 1 つにまとまる）
    assert!(
        april["places"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.get("pref").is_some() && p.get("city").is_some())
    );
    // 5 月: フランス。地域 A が 2、地域 B と国だけが 1 ずつ（同数は地域不明が先）
    let may = by_start("2025-05-01 09:00");
    assert_eq!(
        key(may),
        [
            ("ne:FRA".to_owned(), Some(f.fr_a.clone()), 2),
            ("ne:FRA".to_owned(), None, 1),
            ("ne:FRA".to_owned(), Some(f.fr_b.clone()), 1),
        ]
    );
    assert_eq!(may["regions"][1]["label"], fr);
    // 6 月: 同名の地域は国で分かれる（スペイン 2 が先）
    let june = by_start("2025-06-01 09:00");
    assert_eq!(
        key(june),
        [
            ("ne:ESP".to_owned(), Some(f.la_rioja_es.clone()), 2),
            ("ne:ARG".to_owned(), Some(f.la_rioja_ar.clone()), 1),
        ]
    );
    // 7 月: ロシアの地域不明 4、フィジー 2
    let july = by_start("2025-07-01 09:00");
    assert_eq!(
        key(july),
        [
            ("ne:RUS".to_owned(), None, 4),
            ("ne:FJI".to_owned(), Some(f.fiji.clone()), 2),
        ]
    );
    // 8 月: 未判定と場所不明だけ。国・地域の候補は無い（旧 pref/city の候補は従来どおり）
    let august = by_start("2025-08-01 09:00");
    assert_eq!(august["regions"], json!([]));
    assert_eq!(
        august["places"],
        json!([{"pref": "", "city": "", "count": 9}])
    );
    // 除外を含めると、除外の写真（8: 東京都、15: フランス）も数える
    let hidden = server.trips("?hidden=1").await;
    let april = hidden["trips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["start"] == "2025-04-01 10:00")
        .unwrap();
    assert_eq!(
        key(april)[0],
        ("ne:JPN".to_owned(), Some(f.tokyo.clone()), 7)
    );
    // 表が無い DB: 日本（pref の写真）だけ
    let photos_only: String = f
        .sql
        .lines()
        .filter(|l| !l.contains("photo_regions"))
        .collect::<Vec<_>>()
        .join("\n");
    let old = Server::start(&[], false, &photos_only).await;
    let body = old.trips("").await;
    let april = body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["start"] == "2025-04-01 10:00")
        .unwrap();
    assert_eq!(
        key(april),
        [
            ("ne:JPN".to_owned(), Some(f.tokyo.clone()), 5),
            ("ne:JPN".to_owned(), None, 1),
            ("ne:JPN".to_owned(), Some(osaka.id.clone()), 1),
        ]
    );
    let may = body["trips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["start"] == "2025-05-01 09:00")
        .unwrap();
    assert_eq!(may["regions"], json!([]));
}

#[tokio::test]
async fn points_ignore_the_place_selection_and_stay_period_and_hidden_only() {
    let f = fixture();
    let server = Server::start(&[], true, &f.sql).await;
    // 緯度が文字列の壊れた行（34）は、points が数値として読めず 503 になる（この変更の前から）。ここでは外す
    server
        .writer
        .execute("UPDATE photos SET lat=NULL WHERE id=34", [])
        .unwrap();
    let all = server.get_ok("/api/admin/library/points", "").await;
    for query in [
        "?country=ne:FRA",
        "?country=ne:JPN&region=ne:JPN-1",
        "?unclassified=1",
        "?pref=%E6%9D%B1%E4%BA%AC%E9%83%BD",
    ] {
        let narrowed = server.get_ok("/api/admin/library/points", query).await;
        assert_eq!(narrowed, all, "{query}");
    }
    // 期間・除外では従来どおり絞れる
    let hidden = server
        .get_ok("/api/admin/library/points", "?hidden=1")
        .await;
    assert!(hidden["total"].as_i64() > all["total"].as_i64());
}
