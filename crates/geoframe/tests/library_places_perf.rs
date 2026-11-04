//! `GET /api/admin/library/places`・`photos`・`trips` の応答時間の計測（手動。`#[ignore]`）。
//!
//! 1 万件の合成写真（pref ありが 7 割、pref 空が 3 割。pref 空のうち有効座標のものに現行版の `photo_regions` 行を持たせる）で
//! `places` を繰り返し呼び、中央値を出す。設計の目標は、変更前の 1.5 倍以内（docs/international-places/design.md「テスト計画」）。
//! 実行: `cargo test --release --test library_places_perf -- --ignored --nocapture`。実際の撮影座標は使わない（疑似乱数）。

mod common;

use std::time::Instant;

use common::{TempDir, build_empty_catalog, get, spawn_with_env};
use geoframe::db::Catalog;

const PHOTOS: usize = 10_000;
const ROUNDS: usize = 40;

/// 疑似乱数（線形合同法。再現できる合成データ用）。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
}

fn synthetic_sql() -> String {
    let names = geoframe::regions::data::names().unwrap();
    let prefs: Vec<&str> = names
        .prefectures()
        .filter_map(|p| p.pref.as_deref())
        .collect();
    // 国ごとの地域（日本以外の最初の 25 国。地域が 1 つ以上ある国）
    let mut abroad: Vec<(&str, Vec<&str>, (f64, f64))> = Vec::new();
    let mut centers = Lcg(7);
    for c in &names.countries {
        if c.id == "ne:JPN" {
            continue;
        }
        let regions: Vec<&str> = names
            .regions
            .iter()
            .filter(|r| r.country.as_deref() == Some(c.id.as_str()))
            .map(|r| r.id.as_str())
            .take(12)
            .collect();
        if !regions.is_empty() {
            // 国の中心（国ごとに離れた場所。写真はその周り ±4 度）
            let center = (
                -50.0 + 100.0 * centers.unit(),
                -170.0 + 340.0 * centers.unit(),
            );
            abroad.push((c.id.as_str(), regions, center));
        }
        if abroad.len() == 25 {
            break;
        }
    }
    let version = geoframe::regions::classifier_version().unwrap();
    let mut rng = Lcg(42);
    let mut sql = String::from("BEGIN;\n");
    let mut region_rows = String::new();
    for id in 1..=PHOTOS as u64 {
        let minute = rng.below(60 * 24 * 365 * 3);
        let taken = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            2023 + minute / (60 * 24 * 365),
            1 + (minute / (60 * 24 * 30)) % 12,
            1 + (minute / (60 * 24)) % 28,
            (minute / 60) % 24,
            minute % 60
        );
        let hidden = u8::from(rng.below(50) == 0);
        let mut lat = -60.0 + 120.0 * rng.unit();
        let mut lon = -179.0 + 358.0 * rng.unit();
        let (pref, city, lat_sql, lon_sql) = if rng.below(10) < 7 {
            // 日本の周り
            lat = 31.0 + 13.0 * rng.unit();
            lon = 130.0 + 13.0 * rng.unit();
            let pref = prefs[rng.below(prefs.len() as u64) as usize];
            let city = format!("市{}", rng.below(30));
            (
                format!("'{pref}'"),
                format!("'{city}'"),
                format!("{lat}"),
                format!("{lon}"),
            )
        } else if rng.below(20) == 0 {
            ("NULL".into(), "NULL".into(), "NULL".into(), "NULL".into())
        } else {
            let (country, regions, center) = &abroad[rng.below(abroad.len() as u64) as usize];
            // 地域が決まる写真・国だけの写真・未判定の写真
            let kind = rng.below(10);
            if kind < 9 {
                lat = (center.0 + 8.0 * rng.unit() - 4.0).clamp(-89.0, 89.0);
                lon = (center.1 + 8.0 * rng.unit() - 4.0).clamp(-179.0, 179.0);
            }
            if kind < 7 {
                let region = regions[rng.below(regions.len() as u64) as usize];
                region_rows.push_str(&format!(
                    "INSERT INTO photo_regions VALUES({id},{lat},{lon},'{version}','{country}','{region}','','inside','inside','2025-01-01T00:00:00Z');\n"
                ));
            } else if kind < 9 {
                region_rows.push_str(&format!(
                    "INSERT INTO photo_regions VALUES({id},{lat},{lon},'{version}','{country}',NULL,'','inside','outside','2025-01-01T00:00:00Z');\n"
                ));
            }
            (
                "NULL".into(),
                "NULL".into(),
                format!("{lat}"),
                format!("{lon}"),
            )
        };
        sql.push_str(&format!(
            "INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, pref, city, hidden) \
             VALUES({id},'local',NULL,'p/{id}.jpg','d/{id}.jpg','P{id}.jpg','m{id}',1.0,'{taken}',{lat_sql},{lon_sql},{pref},{city},{hidden});\n"
        ));
    }
    sql.push_str("COMMIT;\nBEGIN;\n");
    sql.push_str(&region_rows);
    sql.push_str("COMMIT;\n");
    sql
}

#[tokio::test]
#[ignore = "手動の計測"]
async fn places_response_time_with_ten_thousand_photos() {
    let dir = TempDir::new("places-perf");
    let (path, mut writer) = build_empty_catalog(dir.path(), "");
    geoframe::regions::ensure_tables(&mut writer).unwrap();
    writer.execute_batch(&synthetic_sql()).unwrap();
    let catalog = Catalog::open(&path).unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog).await;
    for query in [
        "",
        "?from=2024-01-01+00:00&to=2024-12-31+23:59",
        "?bbox=-30,-60,60,60",
        "?bbox=125,28,148,46",
        "?bbox=-180,-90,180,90",
    ] {
        let target = format!("/api/admin/library/places{query}");
        let first = get(addr, &target).await;
        assert_eq!(first.status, 200);
        let bytes = first.body.len();
        let mut times = Vec::new();
        for _ in 0..ROUNDS {
            let t = Instant::now();
            let r = get(addr, &target).await;
            assert_eq!(r.status, 200);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "places{query:<48} median {:6.2} ms  p90 {:6.2} ms  min {:6.2} ms  ({bytes} bytes)",
            times[ROUNDS / 2],
            times[ROUNDS * 9 / 10],
            times[0]
        );
    }
}

/// 値を `%XX` にする（ASCII 英数字と `-_.~:,` 以外）。
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~:,".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 中央値などを出す。200 でない（その版が受けない形）ときは状態だけを出す。
async fn measure(addr: std::net::SocketAddr, target: &str) {
    let first = get(addr, target).await;
    if first.status != 200 {
        println!("{target:<90} status {}", first.status);
        return;
    }
    let bytes = first.body.len();
    let total = first.json()["total"].as_i64();
    let mut times = Vec::new();
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let r = get(addr, target).await;
        assert_eq!(r.status, 200);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{target:<90} median {:6.2} ms  p90 {:6.2} ms  min {:6.2} ms  ({bytes} bytes, total {total:?})",
        times[ROUNDS / 2],
        times[ROUNDS * 9 / 10],
        times[0]
    );
}

/// `photos`（旧形式・新形式・文字検索・期間・範囲・ID）と `trips` の応答時間。新形式は、それを受けない版では状態だけを出す。
#[tokio::test]
#[ignore = "手動の計測"]
async fn photos_and_trips_response_time_with_ten_thousand_photos() {
    let dir = TempDir::new("photos-perf");
    let (path, mut writer) = build_empty_catalog(dir.path(), "");
    geoframe::regions::ensure_tables(&mut writer).unwrap();
    writer.execute_batch(&synthetic_sql()).unwrap();
    let catalog = Catalog::open(&path).unwrap();
    let addr = spawn_with_env(dir.path(), &[], catalog).await;
    let base = "/api/admin/library/photos";
    let ids: Vec<String> = (1..=60).map(|i| (i * 150).to_string()).collect();
    let mut targets = vec![
        "/api/admin/library/trips".to_owned(),
        "/api/admin/library/trips?hidden=1".to_owned(),
        format!("{base}?pref={}", encode("東京都")),
        format!("{base}?pref={}&city={}", encode("東京都"), encode("市1")),
        format!("{base}?city={}", encode("市1")),
        format!("{base}?abroad=1"),
        format!("{base}?untimed=1"),
        format!("{base}?from=2024-01-01+00:00&to=2024-12-31+23:59"),
        format!("{base}?bbox=125,28,148,46"),
        format!("{base}?ids={}", ids.join(",")),
        format!("{base}?q={}", encode("東京")),
        format!("{base}?q={}", encode("Tokyo")),
        format!("{base}?q={}", encode("日本")),
        format!("{base}?q={}", encode("市1 P99")),
        format!(
            "{base}?q={}&from=2024-01-01+00:00&to=2024-12-31+23:59",
            encode("日本")
        ),
    ];
    let tokyo = geoframe::regions::data::names()
        .unwrap()
        .region_of_pref("東京都")
        .unwrap()
        .id
        .clone();
    for query in [
        "country=ne:JPN".to_owned(),
        format!("country=ne:JPN&region={tokyo}"),
        format!("country=ne:JPN&region={tokyo}&city={}", encode("市1")),
        "country=ne:JPN&region_unknown=1".to_owned(),
        "country=ne:FRA".to_owned(),
        "unclassified=1".to_owned(),
        "no_location=1".to_owned(),
    ] {
        targets.push(format!("{base}?{query}"));
    }
    for target in targets {
        measure(addr, &target).await;
    }
}
