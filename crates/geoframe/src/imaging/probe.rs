//! `geoframe img-probe`: 画像処理の確認・計測用のコマンド。DB にも `DATA_DIR` にも触れない。
//!
//! 指定したファイルを順に derivative にして（`--out` があれば書き出す）、1 枚ごとに**連番・種類・寸法・向き・ICC・
//! 所要時間**だけを出す。**ファイル名・パスは出さない**（失敗の文面も [`ImageError`] の定型文だけ）。
//! HEIC が読めなかったときは、構造の分類（ブランド・グリッドか・コーデック・ビット深度など）が付くので、
//! 実機の HEIC の切り分け（T2.4 の受入）にそのまま使える。
//!
//! `--quality` を付けると、derivative を作る代わりに**品質の特徴量**（SHA-256・pHash・鮮明度・輝度・露出・
//! 白飛び・黒つぶれ・コントラスト。T2.6）を出す。Python が保存した値との比較（server.md §16.2）に使う。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::derivative::read_limited;
use super::{DerivativeInfo, IccOutcome, ImageError, make_derivative};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProbeArgs {
    pub files: Vec<PathBuf>,
    /// derivative の書き出し先ディレクトリ（`<連番>.jpg`）。無ければ書かない。
    pub out: Option<PathBuf>,
    pub rotation: i32,
    pub jobs: usize,
    /// derivative の代わりに品質の特徴量を出す。
    pub quality: bool,
}

pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Result<ProbeArgs, String> {
    let mut parsed = ProbeArgs {
        jobs: 1,
        ..ProbeArgs::default()
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => parsed.out = Some(PathBuf::from(args.next().ok_or("--out に値が要ります")?)),
            "--quality" => parsed.quality = true,
            "--rotation" => {
                parsed.rotation = args
                    .next()
                    .ok_or("--rotation に値が要ります")?
                    .parse()
                    .map_err(|_| "--rotation は整数で指定してください")?;
            }
            "--jobs" => {
                parsed.jobs = args
                    .next()
                    .ok_or("--jobs に値が要ります")?
                    .parse()
                    .ok()
                    .filter(|n| (1..=64).contains(n))
                    .ok_or("--jobs は 1〜64 の整数で指定してください")?;
            }
            other if other.starts_with("--") => {
                return Err(format!("img-probe の引数 {other:?} は不明です"));
            }
            file => parsed.files.push(PathBuf::from(file)),
        }
    }
    if parsed.files.is_empty() {
        return Err("img-probe には画像ファイルが 1 つ以上要ります".into());
    }
    Ok(parsed)
}

fn describe(info: &DerivativeInfo) -> String {
    let icc = match info.icc {
        IccOutcome::None => "none",
        IccOutcome::ToSrgb => "srgb",
        IccOutcome::Dropped => "dropped",
    };
    format!(
        "format={:?} in={}x{} out={}x{} exif_orientation={} gray={} icc={icc} bytes={}",
        info.format,
        info.input.0,
        info.input.1,
        info.output.0,
        info.output.1,
        info.exif_orientation,
        u8::from(info.gray),
        info.bytes
    )
}

/// 最大 RSS（KiB）。読めなければ `None`。
fn max_rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

/// 1 枚の結果（出力する説明と所要ミリ秒）。
type Slot = std::sync::Mutex<Option<(Result<String, ImageError>, f64)>>;

fn describe_quality(f: &crate::quality::Features) -> String {
    let m = &f.metrics;
    format!(
        "sha256={} phash={:016x} sharpness={:.9} luminance={:.6} exposure={:.6} black_clip={:.9} white_clip={:.9} contrast={}",
        f.sha256,
        f.phash,
        m.sharpness,
        m.luminance,
        m.exposure_score,
        m.black_clip,
        m.white_clip,
        m.contrast
    )
}

pub fn run(args: &ProbeArgs) -> ExitCode {
    if let Some(dir) = &args.out
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("geoframe: 出力先を作れません（{}）", e.kind());
        return ExitCode::FAILURE;
    }
    let next = AtomicUsize::new(0);
    let results: Vec<Slot> = args
        .files
        .iter()
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(src) = args.files.get(i) else { break };
                    let dest = args.out.as_ref().map_or_else(
                        || std::env::temp_dir().join("geoframe-img-probe-unused.jpg"),
                        |d| d.join(format!("{i:04}.jpg")),
                    );
                    let t = Instant::now();
                    let result = if args.quality {
                        read_limited(src).and_then(|bytes| {
                            crate::quality::analyze_bytes(&bytes).map(|f| describe_quality(&f))
                        })
                    } else if args.out.is_some() {
                        make_derivative(src, &dest, args.rotation).map(|info| describe(&info))
                    } else {
                        // 書き出さない: 読んで変換するだけ（読み込みには `make_derivative` と同じ大きさの上限）
                        read_limited(src).and_then(|bytes| {
                            super::render(&bytes, args.rotation).map(|r| describe(&r.info))
                        })
                    };
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    if let Ok(mut slot) = results[i].lock() {
                        *slot = Some((result, ms));
                    }
                }
            });
        }
    });
    let wall = started.elapsed();
    let (mut ok, mut failed) = (0, 0);
    let mut times = Vec::new();
    for (i, slot) in results.iter().enumerate() {
        match slot.lock().ok().and_then(|mut s| s.take()) {
            Some((Ok(text), ms)) => {
                ok += 1;
                times.push(ms);
                println!("#{i:04} ok {text} ms={ms:.0}");
            }
            Some((Err(e), ms)) => {
                failed += 1;
                println!("#{i:04} error {e} ms={ms:.0}");
            }
            None => failed += 1,
        }
    }
    times.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        times
            .get(((times.len() as f64 - 1.0) * p).round() as usize)
            .copied()
    };
    println!(
        "summary files={} ok={ok} failed={failed} jobs={} wall_ms={:.0} median_ms={} p95_ms={} max_rss_kib={}",
        args.files.len(),
        args.jobs,
        wall.as_secs_f64() * 1000.0,
        pct(0.5).map_or("-".into(), |v| format!("{v:.0}")),
        pct(0.95).map_or("-".into(), |v| format!("{v:.0}")),
        max_rss_kib().map_or("-".into(), |v| v.to_string())
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
