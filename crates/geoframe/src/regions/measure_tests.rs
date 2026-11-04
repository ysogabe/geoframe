//! 計測（AC10: 索引作成後の常駐メモリの増加・索引作成の時間・1 点あたりの判定時間・手放した後のメモリ）。
//! 通常の `cargo test` では回さない（`#[ignore]`）。他の試験と同じプロセスでは RSS が混ざるので、この 1 本だけを回す:
//!
//! ```text
//! cargo test --release -p geoframe --lib regions::measure_tests -- --ignored --nocapture
//! # 本番と同じ mimalloc で測るときは、musl ターゲットで回し、このファイルの先頭に一時的に
//! #   #[global_allocator] static A: mimalloc::MiMalloc = mimalloc::MiMalloc;
//! # を足す（コミットしない）
//! ```
//!
//! 本番のコードには計測用の処理を入れない。座標は決まった乱数の合成値で、実際の撮影座標は使わない。

use std::time::{Duration, Instant};

use super::data;
use super::index::RegionIndex;

fn status_kb(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

fn rss_mb() -> f64 {
    status_kb("VmRSS:") as f64 / 1024.0
}

/// RssAnon（確保したメモリ）。RssFile（バイナリに埋め込んだデータを読んだページ。再読込できるので OS が回収できる）は含まない。
fn anon_mb() -> f64 {
    status_kb("RssAnon:") as f64 / 1024.0
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
#[ignore = "計測。単独で回す"]
fn measure_index_memory() {
    // 名称表は読み取り API が常に持つ小さな表なので、先に読んで基準に含める
    data::names().unwrap();
    let base = rss_mb();
    let base_anon = anon_mb();

    let t = Instant::now();
    let idx = RegionIndex::load().unwrap();
    let build = t.elapsed();
    let built = rss_mb();
    let peak = status_kb("VmHWM:") as f64 / 1024.0;
    let (c, r) = (&idx.polygons().countries, &idx.polygons().regions);
    println!("基準 RSS            : {base:.1} MB (anon {base_anon:.1})");
    println!(
        "索引作成後 RSS      : {built:.1} MB (増加 {:.1} MB、うち anon {:.1} MB)",
        built - base,
        anon_mb() - base_anon
    );
    println!(
        "ピーク RSS (VmHWM)  : {peak:.1} MB (増加 {:.1} MB)",
        peak - base
    );
    println!(
        "内訳: 座標 {:.1} MB（{} 点）、格子の表 {:.1} MB",
        (c.points_len() + r.points_len()) as f64 * 16.0 / 1048576.0,
        c.points_len() + r.points_len(),
        idx.grid_bytes() as f64 / 1048576.0
    );
    println!("索引作成時間        : {:.0} ms", build.as_secs_f64() * 1e3);

    drop(idx);
    let after_drop = rss_mb();
    println!(
        "手放した直後 RSS    : {after_drop:.1} MB (基準比 {:+.1} MB、anon {:+.1} MB)",
        after_drop - base,
        anon_mb() - base_anon
    );
    std::thread::sleep(Duration::from_secs(2));
    let later = rss_mb();
    println!(
        "2 秒待った後 RSS    : {later:.1} MB (基準比 {:+.1} MB、anon {:+.1} MB)",
        later - base,
        anon_mb() - base_anon
    );
    // 返却が次の確保で進むアロケータのために、小さな確保を繰り返してからもう一度見る
    for _ in 0..1000 {
        std::hint::black_box(vec![0u8; 4096]);
    }
    std::thread::sleep(Duration::from_millis(500));
    let churn = rss_mb();
    println!(
        "小さな確保の後 RSS  : {churn:.1} MB (基準比 {:+.1} MB、anon {:+.1} MB)",
        churn - base,
        anon_mb() - base_anon
    );
}

#[test]
#[ignore = "計測。単独で回す"]
fn measure_classify_time() {
    let idx = RegionIndex::load().unwrap();
    // 世界一様な点と、陸地に寄せた点（国の頂点の近傍）を判定する
    let mut rng = Lcg(42);
    let uniform: Vec<(f64, f64)> = (0..20_000)
        .map(|_| (rng.next() * 180.0 - 90.0, rng.next() * 360.0 - 180.0))
        .collect();
    let verts: Vec<[f64; 2]> = idx
        .polygons()
        .countries
        .rings()
        .flatten()
        .copied()
        .collect();
    let landish: Vec<(f64, f64)> = (0..20_000)
        .map(|_| {
            let v = verts[(rng.next() * verts.len() as f64) as usize];
            (
                (v[1] + (rng.next() - 0.5) * 0.2).clamp(-90.0, 90.0),
                (v[0] + (rng.next() - 0.5) * 0.2).clamp(-180.0, 180.0),
            )
        })
        .collect();
    drop(verts);
    for (name, pts) in [("一様", &uniform), ("陸地寄り", &landish)] {
        let mut worst = Duration::ZERO;
        let mut inside = 0;
        let t = Instant::now();
        for &(lat, lon) in pts {
            let s = Instant::now();
            if idx.classify(lat, lon).unwrap().country_id.is_some() {
                inside += 1;
            }
            worst = worst.max(s.elapsed());
        }
        let total = t.elapsed();
        println!(
            "判定 {name}: 平均 {:.1} µs/点, 最悪 {:.0} µs ({} 点中 国あり {inside})",
            total.as_secs_f64() * 1e6 / pts.len() as f64,
            worst.as_secs_f64() * 1e6,
            pts.len()
        );
    }
}
