//! 画像処理PoCのCLI。入力ディレクトリ（またはファイル）を処理し、1枚ごとの時間と全体の最大RSSを出す。
//! 出力する識別子はファイル名の拡張子を除いた部分だけ（入力側で連番に付け替えておく前提）。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use geoframe_imgpoc::{
    EncoderKind, HeicBackend, IccPolicy, JpegDecoderKind, Options, Order, OrientationSource,
    Output, Resizer, process,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "\
使い方: geoframe-imgpoc [オプション] <入力ディレクトリまたはファイル>...

  --decoder image|zune                       JPEGデコーダ（既定 image）
  --resize image:{catmullrom|lanczos3|triangle|nearest|gaussian}
           fir:{catmullrom|lanczos3|bilinear|box|hamming|mitchell}
                                              縮小（既定 fir:catmullrom）
  --encoder image|je420|je420opt|je444        JPEGエンコーダ（既定 je420）
  --quality N                                 JPEG品質（既定 85）
  --icc drop|keep|srgb                        ICCの扱い（既定 srgb）
  --order orient-first|resize-first           向き補正と縮小の順序（既定 orient-first）
  --orientation decoder|kamadak               Orientationの読み取り元（既定 decoder）
  --heic libheif|zune|heic-rs|oxide|<コマンド>  HEICの読み方（既定 libheif）
  --heic-threads N                            HEICのデコードのスレッド数（省略時は各ライブラリの既定）
  --max-px N                                  長辺の上限（既定 2560）
  --rotation 0|90|180|270                     手動回転（既定 0）
  --jobs N                                    並列数（既定 1）
  --out DIR                                   出力JPEGの保存先（省略時は保存しない）
  --report FILE                               1枚ごとの結果をJSON Linesで書く
";

fn parse_resizer(s: &str) -> Result<Resizer, String> {
    use fast_image_resize::FilterType as F;
    use image::imageops::FilterType as I;
    Ok(match s {
        "image:catmullrom" => Resizer::Image(I::CatmullRom),
        "image:lanczos3" => Resizer::Image(I::Lanczos3),
        "image:triangle" => Resizer::Image(I::Triangle),
        "image:nearest" => Resizer::Image(I::Nearest),
        "image:gaussian" => Resizer::Image(I::Gaussian),
        "fir:catmullrom" => Resizer::Fir(F::CatmullRom),
        "fir:lanczos3" => Resizer::Fir(F::Lanczos3),
        "fir:bilinear" => Resizer::Fir(F::Bilinear),
        "fir:box" => Resizer::Fir(F::Box),
        "fir:hamming" => Resizer::Fir(F::Hamming),
        "fir:mitchell" => Resizer::Fir(F::Mitchell),
        _ => return Err(format!("不明な--resize: {s}")),
    })
}

struct Cli {
    opts: Options,
    inputs: Vec<PathBuf>,
    jobs: usize,
    out: Option<PathBuf>,
    report: Option<PathBuf>,
}

fn parse_args() -> Result<Cli, String> {
    let mut cli = Cli {
        opts: Options::default(),
        inputs: Vec::new(),
        jobs: 1,
        out: None,
        report: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name}に値が要ります"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--decoder" => {
                cli.opts.decoder = match value(&a)?.as_str() {
                    "image" => JpegDecoderKind::Image,
                    "zune" => JpegDecoderKind::Zune,
                    v => return Err(format!("不明な--decoder: {v}")),
                }
            }
            "--resize" => cli.opts.resizer = parse_resizer(&value(&a)?)?,
            "--encoder" => {
                cli.opts.encoder = match value(&a)?.as_str() {
                    "image" => EncoderKind::Image,
                    "je420" => EncoderKind::Je420,
                    "je420opt" => EncoderKind::Je420Opt,
                    "je444" => EncoderKind::Je444,
                    v => return Err(format!("不明な--encoder: {v}")),
                }
            }
            "--quality" => cli.opts.quality = value(&a)?.parse().map_err(|e| format!("{e}"))?,
            "--icc" => {
                cli.opts.icc = match value(&a)?.as_str() {
                    "drop" => IccPolicy::Drop,
                    "keep" => IccPolicy::Keep,
                    "srgb" => IccPolicy::Srgb,
                    v => return Err(format!("不明な--icc: {v}")),
                }
            }
            "--order" => {
                cli.opts.order = match value(&a)?.as_str() {
                    "orient-first" => Order::OrientFirst,
                    "resize-first" => Order::ResizeFirst,
                    v => return Err(format!("不明な--order: {v}")),
                }
            }
            "--orientation" => {
                cli.opts.orientation = match value(&a)?.as_str() {
                    "decoder" => OrientationSource::Decoder,
                    "kamadak" => OrientationSource::Kamadak,
                    v => return Err(format!("不明な--orientation: {v}")),
                }
            }
            "--heic" => {
                cli.opts.heic = match value(&a)?.as_str() {
                    "libheif" => HeicBackend::Libheif,
                    "zune" => HeicBackend::Zune,
                    "heic-rs" => HeicBackend::HeicRs,
                    "oxide" => HeicBackend::Oxide,
                    cmd => HeicBackend::Command(cmd.to_string()),
                }
            }
            "--max-px" => cli.opts.max_px = value(&a)?.parse().map_err(|e| format!("{e}"))?,
            "--rotation" => cli.opts.rotation = value(&a)?.parse().map_err(|e| format!("{e}"))?,
            "--heic-threads" => {
                cli.opts.heic_threads = Some(value(&a)?.parse().map_err(|e| format!("{e}"))?)
            }
            "--jobs" => cli.jobs = value(&a)?.parse().map_err(|e| format!("{e}"))?,
            "--out" => cli.out = Some(value(&a)?.into()),
            "--report" => cli.report = Some(value(&a)?.into()),
            s if s.starts_with("--") => return Err(format!("不明なオプション: {s}")),
            _ => cli.inputs.push(a.into()),
        }
    }
    if cli.inputs.is_empty() {
        return Err("入力がありません（--helpで使い方）".into());
    }
    Ok(cli)
}

fn collect(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for p in inputs {
        if p.is_dir() {
            for e in std::fs::read_dir(p).map_err(|e| e.to_string())? {
                let path = e.map_err(|e| e.to_string())?.path();
                if path.is_file() {
                    files.push(path);
                }
            }
        } else {
            files.push(p.clone());
        }
    }
    files.sort();
    Ok(files)
}

/// 存在しない部分を含むパスを、実在する最も深い祖先の正規化＋残りの要素に直す
fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut rest = Vec::new();
    let mut cur = abs.as_path();
    loop {
        match cur.canonicalize() {
            Ok(mut base) => {
                base.extend(rest.iter().rev());
                return Ok(base);
            }
            Err(_) => {
                let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
                    return Ok(abs);
                };
                rest.push(name.to_owned());
                cur = parent;
            }
        }
    }
}

/// `--out`が入力のディレクトリ（ファイル入力ならその親）と同じか、その下なら拒否する。
/// 入力の`<id>.jpg`を出力で上書きしたり、出力を次の入力として拾ったりしないため
fn check_out_dir(out: &Path, inputs: &[PathBuf]) -> Result<(), String> {
    let out_abs = resolve(out).map_err(|e| e.to_string())?;
    for input in inputs {
        let root = if input.is_dir() {
            input.clone()
        } else {
            input
                .parent()
                .map(Path::to_path_buf)
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| PathBuf::from("."))
        };
        let root_abs = resolve(&root).map_err(|e| e.to_string())?;
        if out_abs.starts_with(&root_abs) {
            return Err("--outは入力のディレクトリと同じか、その下にはできません".into());
        }
    }
    Ok(())
}

/// `/proc/self/status`の`VmHWM`（最大RSS、KiB）
fn max_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

struct Row {
    id: String,
    result: Result<Output, String>,
}

fn run(cli: &Cli, files: &[PathBuf]) -> Vec<Row> {
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(Vec::<(usize, Row)>::new());
    std::thread::scope(|s| {
        for _ in 0..cli.jobs.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // 読み込み時間は計測に含めない（ページキャッシュの状態に左右されるため）
                    let result = std::fs::read(path)
                        .map_err(|e| e.to_string())
                        .and_then(|bytes| process(&bytes, Some(path), &cli.opts));
                    if let (Ok(o), Some(dir)) = (&result, &cli.out) {
                        let _ = std::fs::write(dir.join(format!("{id}.jpg")), &o.jpeg);
                    }
                    rows.lock().unwrap().push((i, Row { id, result }));
                }
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|(i, _)| *i);
    rows.into_iter().map(|(_, r)| r).collect()
}

fn summarize(rows: &[Row], wall: std::time::Duration, jobs: usize) -> String {
    let ok: Vec<&Output> = rows.iter().filter_map(|r| r.result.as_ref().ok()).collect();
    let mut out = String::new();
    let mut total: Vec<f64> = ok.iter().map(|o| ms(o.timings.total)).collect();
    total.sort_by(f64::total_cmp);
    let stage = |f: fn(&Output) -> std::time::Duration| {
        let mut v: Vec<f64> = ok.iter().map(|o| ms(f(o))).collect();
        v.sort_by(f64::total_cmp);
        percentile(&v, 0.5)
    };
    let bytes: u64 = ok.iter().map(|o| o.jpeg.len() as u64).sum();
    let _ = writeln!(
        out,
        "枚数 ok={} err={} jobs={jobs} wall={:.2}s 処理量={:.2}枚/秒",
        ok.len(),
        rows.len() - ok.len(),
        wall.as_secs_f64(),
        ok.len() as f64 / wall.as_secs_f64()
    );
    let _ = writeln!(
        out,
        "1枚(ms) 中央値={:.1} p95={:.1} 最大={:.1} | 段階の中央値: decode={:.1} orient={:.1} resize={:.1} icc={:.1} encode={:.1}",
        percentile(&total, 0.5),
        percentile(&total, 0.95),
        percentile(&total, 1.0),
        stage(|o| o.timings.decode),
        stage(|o| o.timings.orient),
        stage(|o| o.timings.resize),
        stage(|o| o.timings.icc),
        stage(|o| o.timings.encode),
    );
    let _ = writeln!(
        out,
        "出力サイズ 合計={bytes}B 平均={}B 最大RSS={}KiB",
        if ok.is_empty() {
            0
        } else {
            bytes / ok.len() as u64
        },
        max_rss_kib().unwrap_or(0)
    );
    out
}

fn write_report(path: &Path, rows: &[Row]) -> std::io::Result<()> {
    let mut s = String::new();
    for r in rows {
        match &r.result {
            Ok(o) => {
                let _ = writeln!(
                    s,
                    "{{\"id\":\"{}\",\"kind\":\"{:?}\",\"in\":[{},{}],\"out\":[{},{}],\"bytes\":{},\"orientation\":{},\"icc_in\":{},\"ms\":{:.2},\"decode_ms\":{:.2},\"orient_ms\":{:.2},\"resize_ms\":{:.2},\"icc_ms\":{:.2},\"encode_ms\":{:.2}}}",
                    r.id,
                    o.kind,
                    o.in_dims.0,
                    o.in_dims.1,
                    o.out_dims.0,
                    o.out_dims.1,
                    o.jpeg.len(),
                    o.exif_orientation,
                    o.icc_in,
                    ms(o.timings.total),
                    ms(o.timings.decode),
                    ms(o.timings.orient),
                    ms(o.timings.resize),
                    ms(o.timings.icc),
                    ms(o.timings.encode)
                );
            }
            Err(e) => {
                let _ = writeln!(
                    s,
                    "{{\"id\":\"{}\",\"error\":{:?}}}",
                    r.id,
                    e.replace('\n', " ")
                );
            }
        }
    }
    std::fs::write(path, s)
}

fn main() {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let files = match collect(&cli.inputs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Some(dir) = &cli.out
        && let Err(e) = check_out_dir(dir, &cli.inputs)
            .and_then(|()| std::fs::create_dir_all(dir).map_err(|e| e.to_string()))
    {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let t = Instant::now();
    let rows = run(&cli, &files);
    let wall = t.elapsed();
    print!("{}", summarize(&rows, wall, cli.jobs));
    if let Some(p) = &cli.report
        && let Err(e) = write_report(p, &rows)
    {
        eprintln!("{e}");
    }
    if rows.iter().any(|r| r.result.is_err()) {
        // 失敗の内容は画像の中身を含まないが、件数だけ標準エラーに出す
        eprintln!("失敗した入力があります（--reportで確認）");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("geoframe-imgpoc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn outは入力と同じかその下なら拒否する() {
        let base = tmp("out-check");
        let input = base.join("in");
        std::fs::create_dir_all(input.join("sub")).unwrap();
        std::fs::write(input.join("a.jpg"), b"x").unwrap();
        let inputs = [input.clone()];

        assert!(check_out_dir(&input, &inputs).is_err());
        assert!(check_out_dir(&input.join("sub"), &inputs).is_err());
        // まだ無いディレクトリでも、入力の下なら拒否する
        assert!(check_out_dir(&input.join("new/deeper"), &inputs).is_err());
        // `..`を経由した同じ場所
        assert!(check_out_dir(&input.join("sub/.."), &inputs).is_err());
        // ファイル入力は、その親ディレクトリを入力の場所とみなす
        assert!(check_out_dir(&input, &[input.join("a.jpg")]).is_err());
        // 兄弟のディレクトリ（名前が前方一致でも別）は通す
        assert!(check_out_dir(&base.join("in-out"), &inputs).is_ok());
        assert!(check_out_dir(&base.join("out"), &inputs).is_ok());
        assert!(check_out_dir(&base, &inputs).is_ok());
        // シンボリックリンク経由でも同じ場所は拒否する
        let link = base.join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(check_out_dir(&link.join("x"), &inputs).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
