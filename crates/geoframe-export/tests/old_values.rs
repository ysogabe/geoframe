//! 実測: 旧 manifest の値と、Rust の実装の値の差（design.md 4.3、10 章の「4.3」と「配置の前」）。
//!
//! 手で回す試験（`#[ignore]`）。実際の写真と旧 manifest の値は、リポジトリに入れない。パスは環境変数で渡す。
//!
//! ```text
//! GEOFRAME_EXPORT_OLD_VALUES=<値の JSON> GEOFRAME_EXPORT_OLD_IMAGES=<画像のフォルダ> \
//!   cargo test --release -p geoframe-export --test old_values -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 値の JSON は `[{"id": 1, "w": 2560, "h": 1707, "b": 70.5, "s": 2000.5}, ...]`
//! （`b` = `mean_brightness`、`s` = `sharpness_laplacian_var`）、画像は `<フォルダ>/<id>.jpg`。
//! 1 枚ずつ順に処理するので、メモリは 1 枚分（数十 MB）。

use geoframe_export::jpeg::{self, Inspection};
use geoframe_export::measure;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn summary(name: &str, mut v: Vec<f64>) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{name}: n={} 中央値={:.3e} 95%={:.3e} 99%={:.3e} 最大={:.3e}",
        v.len(),
        percentile(&v, 0.5),
        percentile(&v, 0.95),
        percentile(&v, 0.99),
        v.last().copied().unwrap_or(f64::NAN)
    );
}

#[test]
#[ignore = "実際の写真と旧 manifest の値が要る（環境変数）。手で回す"]
fn compare_with_the_old_manifest_values() {
    let (Ok(values), Ok(images)) = (
        std::env::var("GEOFRAME_EXPORT_OLD_VALUES"),
        std::env::var("GEOFRAME_EXPORT_OLD_IMAGES"),
    ) else {
        panic!("GEOFRAME_EXPORT_OLD_VALUES と GEOFRAME_EXPORT_OLD_IMAGES を指定してください");
    };
    let rows: serde_json::Value = serde_json::from_slice(&std::fs::read(values).unwrap()).unwrap();
    let rows = rows.as_array().unwrap();

    let (mut b_abs, mut s_rel, mut s_abs) = (Vec::new(), Vec::new(), Vec::new());
    let (mut dims_ok, mut metadata, mut malformed, mut decode_err) = (0, 0, 0, 0);
    let mut worst: Vec<(f64, i64, f64, f64)> = Vec::new();
    let (mut b_over, mut s_over) = (0, 0);
    for row in rows {
        let id = row["id"].as_i64().unwrap();
        let (w, h) = (
            row["w"].as_u64().unwrap() as u32,
            row["h"].as_u64().unwrap() as u32,
        );
        let (b_old, s_old) = (row["b"].as_f64().unwrap(), row["s"].as_f64().unwrap());
        let bytes = std::fs::read(format!("{images}/{id}.jpg")).unwrap();
        let header = match jpeg::inspect(&bytes, 60_000_000) {
            Ok(Inspection::Clean(h)) => h,
            Ok(Inspection::HasMetadata) => {
                metadata += 1;
                continue;
            }
            Err(_) => {
                malformed += 1;
                continue;
            }
        };
        let Ok(gray) = jpeg::decode_gray(&bytes, header) else {
            decode_err += 1;
            continue;
        };
        dims_ok += usize::from((header.width, header.height) == (w, h));
        let m = measure::measure(&gray.data, gray.width, gray.height);
        let db = (m.mean_brightness - b_old).abs();
        let ds = (m.sharpness_laplacian_var - s_old).abs();
        let rel = ds / s_old.abs().max(1e-9);
        b_abs.push(db);
        s_abs.push(ds);
        s_rel.push(rel);
        b_over += usize::from(!measure::brightness_within_tolerance(
            m.mean_brightness,
            b_old,
        ));
        s_over += usize::from(!measure::sharpness_within_tolerance(
            m.sharpness_laplacian_var,
            s_old,
        ));
        worst.push((rel, id, m.sharpness_laplacian_var, s_old));
    }
    println!(
        "枚数={} 寸法の一致={dims_ok} has_metadata={metadata} 構造の誤り={malformed} デコード失敗={decode_err}",
        rows.len()
    );
    summary("明るさ 絶対差", b_abs);
    summary("シャープネス 相対差", s_rel);
    summary("シャープネス 絶対差", s_abs);
    println!("許容幅の超過: 明るさ={b_over} シャープネス={s_over}");
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (rel, _id, new, old) in worst.iter().take(5) {
        println!("  相対差 {rel:.3e}（新 {new} / 旧 {old}）");
    }
}
