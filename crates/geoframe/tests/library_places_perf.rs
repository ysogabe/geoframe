//! `GET /api/admin/library/places` の応答時間の計測（手動。`#[ignore]`）。
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
