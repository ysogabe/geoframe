//! miharashi の `Feed`（一覧の取得と写真の読み込みの別スレッド）を、画面なしで GEOFRAME に向けて動かす。
//!
//! 使い方:
//!   miharashi-feed-check --url http://<ホスト>:8080 [--count 200] [--size 640x360]
//!       [--min-secs 1] [--max-secs 8] [--timeout-secs 600]
//!       [--stop-cmd '<止めるコマンド>' --stop-after 10 --start-cmd '<戻すコマンド>' --resume-after 8]
//!
//! `Feed::spawn` を実際の設定と同じ形で起こし、写真かカードが届くたびに `next()` を呼んで `--count` 枚まで進める。
//! 出力は件数だけ（写真の値・場所・タイトルは出さない）。終了コード 0 = `--count` 枚に届き、停止・再開の区間の外で
//! `Retrying`（一覧や写真を3回続けて読めず再接続に入った）が 0 回（停止・再開を指定したときは、下の条件も満たす）。
//! `Feed::spawn` の経路では、miharashi が別の写真へ飛ばして先へ進む1〜2回の取得・デコードの失敗はイベントにも標準エラーにも
//! 出ないので数えられない。検出できるのは3回続けて失敗して `Retrying` になったときだけ。
//! （標準エラーの `photo: …` は `PhotoItem::ensure_frame` と inline モードの `Inline::push_current` だけが出し、
//! このツールはどちらも通らないので、`photo:` の行は数えない。）
//!
//! 停止と再開（再接続の確認）: `--stop-cmd` を `--stop-after` 秒後に、`--start-cmd` を、その `--resume-after` 秒後に
//! `sh -c` で実行する。止めている間は `Retrying`（指数バックオフ）、戻したあとは `Online` が届き、自動で
//! 続きを読み込むはず。停止したあとは、件数に届いても時間切れでも、終了前に必ず再開コマンドを実行する
//! （停止したままにしない。失敗は終了コードに反映する）。コマンドの中身（検証環境への ssh など）は呼び出し側が決める。
//! 成功は、停止・再開の両コマンドが実行され、成功し、`Retrying` と（再開後の）`Online` が観測され、復帰後に写真を受信したとき。
//! 件数に先に届いて停止しなかった場合、復帰を観測できなかった場合は失敗（`--count` を増やす）。
//!
//! photos.rs・album.rs は miharashi-app のソースをそのまま取り込む（`vendor/miharashi` は setup.sh が張るリンク）。
//! 下の `screen`・`video`・`config`・`home` は、それらが参照する型だけの代役で、描画や取得の動作には関わらない。

#[path = "../vendor/miharashi/crates/miharashi-app/src/album.rs"]
#[allow(dead_code)]
mod album;
#[path = "../vendor/miharashi/crates/miharashi-app/src/photos.rs"]
#[allow(dead_code)]
mod photos;

/// photos.rs が使う画面側の型の代役。
#[allow(dead_code)]
mod screen {
    use miharashi_render::Pixmap;
    use miharashi_render::photo_layout::PhotoLayout;

    pub enum PhotoData<'a> {
        Frame(&'a Pixmap),
        Source {
            image: &'a image::DynamicImage,
            layout: &'a PhotoLayout,
        },
        Resident,
    }

    pub struct PhotoRef<'a> {
        pub id: u64,
        pub data: PhotoData<'a>,
    }
}

/// `Source::Demo`（動画の見本）用の代役。ここでは使わない。
#[allow(dead_code)]
mod video {
    pub mod photos {
        pub struct Scene {
            pub caption: &'static str,
            pub taken: &'static str,
        }
        pub static SCENES: [Scene; 0] = [];
        pub fn scene(_: usize) -> image::DynamicImage {
            unreachable!("Demo は使わない")
        }
    }
}

#[allow(dead_code)]
mod config {
    #[derive(Debug, Clone)]
    pub struct HomeAssistant {
        pub url: String,
        pub token_file: String,
    }
}

/// Home Assistant は使わない（`home: None` で起こすので呼ばれない）。
mod home {
    pub fn home_location(_: &ureq::Agent, _: &crate::config::HomeAssistant) -> Result<(f64, f64), String> {
        Err("Home Assistant は使わない".into())
    }
}

use photos::{Feed, FeedEvent, FeedStatus, Source};
use std::process::Command;
use std::time::{Duration, Instant};

struct Args {
    url: String,
    count: usize,
    size: (u32, u32),
    min: Duration,
    max: Duration,
    timeout: Duration,
    stop_cmd: Option<String>,
    stop_after: Duration,
    start_cmd: Option<String>,
    resume_after: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        url: String::new(),
        count: 200,
        size: (640, 360),
        min: Duration::from_secs(1),
        max: Duration::from_secs(8),
        timeout: Duration::from_secs(600),
        stop_cmd: None,
        stop_after: Duration::from_secs(10),
        start_cmd: None,
        resume_after: Duration::from_secs(8),
    };
    let mut it = std::env::args().skip(1);
    let secs = |s: String, name: &str| -> Result<Duration, String> {
        s.parse::<u64>().map(Duration::from_secs).map_err(|_| format!("{name} は整数（秒）"))
    };
    while let Some(k) = it.next() {
        let mut v = |name: &str| it.next().ok_or_else(|| format!("{name} には値が必要"));
        match k.as_str() {
            "--url" => a.url = v("--url")?,
            "--count" => a.count = v("--count")?.parse().map_err(|_| "--count は整数")?,
            "--size" => {
                let s = v("--size")?;
                let (w, h) = s.split_once('x').ok_or("--size は 幅x高さ")?;
                a.size = (
                    w.parse().map_err(|_| "--size は 幅x高さ")?,
                    h.parse().map_err(|_| "--size は 幅x高さ")?,
                );
            }
            "--min-secs" => a.min = secs(v("--min-secs")?, "--min-secs")?,
            "--max-secs" => a.max = secs(v("--max-secs")?, "--max-secs")?,
            "--timeout-secs" => a.timeout = secs(v("--timeout-secs")?, "--timeout-secs")?,
            "--stop-cmd" => a.stop_cmd = Some(v("--stop-cmd")?),
            "--stop-after" => a.stop_after = secs(v("--stop-after")?, "--stop-after")?,
            "--start-cmd" => a.start_cmd = Some(v("--start-cmd")?),
            "--resume-after" => a.resume_after = secs(v("--resume-after")?, "--resume-after")?,
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    if a.url.is_empty() {
        return Err("--url が必要".into());
    }
    if a.stop_cmd.is_some() != a.start_cmd.is_some() {
        return Err("--stop-cmd と --start-cmd は両方指定する".into());
    }
    Ok(a)
}

fn sh(cmd: &str) -> bool {
    Command::new("sh").arg("-c").arg(cmd).status().is_ok_and(|s| s.success())
}

fn main() {
    run();
}

fn run() {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(2);
        }
    };
    let t0 = Instant::now();
    let feed = Feed::spawn(Source::Geoframe(a.url.clone()), a.size, None, a.min, a.max, None);

    let (mut photos, mut cards, mut retrying, mut online) = (0usize, 0usize, 0usize, 0usize);
    let mut shown = 0usize;
    let mut first_retry: Option<Instant> = None;
    let mut resume_gap: Option<Duration> = None;
    let mut stopped_at: Option<Instant> = None;
    let mut started_at: Option<Instant> = None;
    let mut cmd_failed = false;
    // 停止から復帰までの区間の外で届いた Retrying（想定外の取得失敗）
    let mut unplanned_retrying = 0usize;
    // 再開コマンドのあとに届いた Online（復帰）と、それ以後に届いた写真
    let mut recovered = false;
    let mut photos_after_recovery = 0usize;

    while shown < a.count && t0.elapsed() < a.timeout {
        // 停止 → 再開（指定があれば）
        if let (Some(stop), Some(start)) = (&a.stop_cmd, &a.start_cmd) {
            if stopped_at.is_none() && t0.elapsed() >= a.stop_after {
                eprintln!("[feed-check] {}s: 停止コマンドを実行", t0.elapsed().as_secs());
                cmd_failed |= !sh(stop);
                stopped_at = Some(Instant::now());
            }
            if let Some(s) = stopped_at
                && started_at.is_none()
                && s.elapsed() >= a.resume_after
            {
                eprintln!("[feed-check] {}s: 再開コマンドを実行", t0.elapsed().as_secs());
                cmd_failed |= !sh(start);
                started_at = Some(Instant::now());
            }
        }
        match feed.try_recv() {
            Some(FeedEvent::Photo(_)) => {
                photos += 1;
                shown += 1;
                if recovered {
                    photos_after_recovery += 1;
                }
                if first_retry.is_some() && resume_gap.is_none() {
                    resume_gap = first_retry.map(|t| t.elapsed());
                }
                feed.next();
            }
            Some(FeedEvent::Card(_)) => {
                cards += 1;
                shown += 1;
                feed.next();
            }
            Some(FeedEvent::Status(FeedStatus::Retrying { .. })) => {
                retrying += 1;
                if stopped_at.is_none() || recovered {
                    unplanned_retrying += 1;
                }
                first_retry.get_or_insert_with(Instant::now);
            }
            Some(FeedEvent::Status(FeedStatus::Online)) => {
                online += 1;
                if started_at.is_some() && retrying > 0 {
                    recovered = true;
                }
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    // 停止したまま終わらない: 件数に届いた・時間切れが再開の時刻より前でも、止めたサービスは戻す
    if let (Some(_), None, Some(start)) = (stopped_at, started_at, &a.start_cmd) {
        eprintln!("[feed-check] {}s: 終了前に再開コマンドを実行", t0.elapsed().as_secs());
        cmd_failed |= !sh(start);
        started_at = Some(Instant::now());
    }

    println!("読み込み: 写真 {photos} 枚、旅・季節のカード {cards} 枚（計 {shown} / 目標 {}）", a.count);
    println!("再接続: Retrying {retrying} 回、Online {online} 回");
    if let Some(g) = resume_gap {
        println!("最初の Retrying から次の写真まで: {:.1} 秒", g.as_secs_f64());
    }
    println!("経過: {:.0} 秒", t0.elapsed().as_secs_f64());
    let mut problems: Vec<String> = Vec::new();
    if shown < a.count {
        problems.push("目標の枚数に届かなかった".into());
    }
    if unplanned_retrying > 0 {
        problems.push(format!("停止・再開の区間の外で Retrying が {unplanned_retrying} 回あった（3回続けて取得に失敗した）"));
    }
    if cmd_failed {
        problems.push("停止または再開のコマンドが失敗した".into());
    }
    if a.stop_cmd.is_some() {
        if stopped_at.is_none() || started_at.is_none() {
            problems.push("停止・再開のコマンドが両方は実行されなかった（先に件数へ届いた。--count を増やす）".into());
        }
        if retrying == 0 {
            problems.push("Retrying を観測できなかった".into());
        }
        if !recovered {
            problems.push("再開後の Online を観測できなかった".into());
        }
        if photos_after_recovery == 0 {
            problems.push("復帰後に写真を受信できなかった".into());
        }
    }
    for p in &problems {
        eprintln!("NG: {p}");
    }
    std::process::exit(if problems.is_empty() { 0 } else { 1 });
}
