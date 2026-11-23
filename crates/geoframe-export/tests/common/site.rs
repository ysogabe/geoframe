//! T5 の試験の共通部品: 偽の写真サービス（一覧 API と表示用 JPEG）と、偽の Drive・状態ディレクトリを組み合わせた世界。
//! 本物の写真サービス（127.0.0.1:8080）・Drive には触れない。
#![allow(dead_code)]

use std::time::{Duration, Instant};

use geoframe_export::flow::Tuning;
use geoframe_export::jobid::JobId;
use geoframe_export::publish::Timing;
use geoframe_export::request::{ExportRequest, RawRequest};
use geoframe_export::service::{Lookup, Service};
use serde_json::{Value, json};

use super::drive::Env;
use super::{FakeSite, Reply};

/// 合成の JPEG（寸法と種で中身が変わる）。
pub fn jpeg(w: u16, h: u16, seed: u8) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(usize::from(w) * usize::from(h) * 3);
    for y in 0..h {
        for x in 0..w {
            rgb.extend([
                (x as u8).wrapping_mul(3).wrapping_add(seed),
                (y as u8).wrapping_mul(5),
                (x as u8 ^ y as u8).wrapping_add(seed),
            ]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85)
        .encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .expect("encode");
    out
}

/// 一覧 API の 1 件（`derivatives/p<id>.jpg`）。
pub fn row(id: i64, time: &str) -> Value {
    json!({"id": id, "file": format!("derivatives/p{id}.jpg"), "title": format!("題{id}"),
           "lat": 12.5, "lon": 98.25, "time": time, "elev": 3.0, "inferred": false})
}

/// 表示用 JPEG の無い（作成待ちの）1 件。
pub fn pending_row(id: i64, time: &str) -> Value {
    let mut r = row(id, time);
    r["file"] = Value::Null;
    r
}

/// 表示用 JPEG の形に合わない（`not_derivative`）1 件。
pub fn original_row(id: i64, time: &str) -> Value {
    let mut r = row(id, time);
    r["file"] = json!(format!("photos/original{id}.jpg"));
    r
}

pub fn listing(from: &str, to: &str, photos: Vec<Value>) -> Value {
    json!({"from": from, "to": to, "photos": photos, "undated": 0, "invalid_time": 0, "truncated": false})
}

/// 一覧 API と、`photos` の各 `id` の表示用 JPEG（`p<id>.jpg`）を返す偽の写真サービス。`bad` の `id` の画像は、
/// （ステータス、JPEG でない本文）にできる。
pub fn photo_site(from: &str, to: &str, photos: Vec<Value>) -> FakeSite {
    let mut routes = vec![(
        "/api/admin/export/photos".to_owned(),
        Reply::json(serde_json::to_vec(&listing(from, to, photos.clone())).expect("json")),
    )];
    for (i, p) in photos.iter().enumerate() {
        if let Some(id) = p["id"].as_i64() {
            routes.push((
                format!("/derivatives/p{id}.jpg"),
                Reply::jpeg(jpeg(64, 48, i as u8)),
            ));
        }
    }
    FakeSite::start(routes)
}

/// 一覧 API だけを返す（画像の経路が無いので、取得は 404 になる）。
pub fn site_without_images(from: &str, to: &str, photos: Vec<Value>) -> FakeSite {
    FakeSite::start(vec![(
        "/api/admin/export/photos".to_owned(),
        Reply::json(serde_json::to_vec(&listing(from, to, photos)).expect("json")),
    )])
}

pub fn fast_tuning() -> Tuning {
    Tuning {
        timing: Timing {
            settle: Duration::from_millis(10),
            rounds: 3,
        },
        // 流れの試験は、API の再試行に助けられずに、失敗がそのまま復旧の表へ渡ることを確かめる（再試行そのものは別の試験）
        drive_retry: Some(super::drive::no_retry()),
        ..Tuning::default()
    }
}

pub fn request(
    from: &str,
    to: Option<&str>,
    on_exists: Option<&str>,
    rid: Option<&str>,
) -> ExportRequest {
    RawRequest {
        from: Some(from.to_owned()),
        to: to.map(str::to_owned),
        destination: Some("yamareco".to_owned()),
        on_exists: on_exists.map(str::to_owned),
        request_id: rid.map(str::to_owned),
        ..RawRequest::default()
    }
    .validate()
    .expect("request")
}

pub fn day() -> ExportRequest {
    request("2030-03-10", None, None, None)
}

/// 偽の写真サービスを向いた設定の `Service`。
pub fn service(env: &mut Env, site: &FakeSite) -> Service {
    env.settings.feed_base = site.base.clone();
    Service::with_tuning(env.settings.clone(), fast_tuning()).expect("service")
}

/// ジョブが終わる（`running` でなくなる）まで待って、GET の形の JSON を返す。
pub fn wait_done(svc: &Service, id: &JobId) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Lookup::Found(v) = svc.lookup(id.as_str()) else {
            panic!("記録が引けません")
        };
        if v["status"] != "running" {
            return v;
        }
        assert!(Instant::now() < deadline, "ジョブが終わりません: {v}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 手で作る記録を、`status` と整合する形にする（完了時刻・`result`・`error`。書く側〔`flow`〕が作る組み合わせ。読み込みの検査
/// [`geoframe_export::jobs::JobRecord::from_file_json`] に通る）。`blocked` の説明（`blocked`）は、呼び出し側が入れる。
pub fn settle(rec: &mut geoframe_export::jobs::JobRecord, status: geoframe_export::jobs::Status) {
    use geoframe_export::error::ErrorCode;
    use geoframe_export::jobs::Status;
    rec.status = status;
    if status != Status::Running {
        rec.finished_at = Some("2030-01-01T00:00:00+09:00".to_owned());
    }
    match status {
        Status::Succeeded => {
            rec.result = Some(json!({
                "folder": "2030-03-10", "catalog_count": 0, "count": 0, "uploaded": false, "replaced": false,
                "bytes": 0, "excluded": [], "undated_in_catalog": 0, "invalid_time_in_catalog": 0, "existing_files": 0
            }));
        }
        Status::Failed => rec.error = Some(ErrorCode::Internal),
        Status::Blocked => rec.error = Some(ErrorCode::Blocked),
        _ => {}
    }
}

/// 偽の Drive に、操作名 `op`（`upload` など）の要求が来る（`calls.log` に出る。効果の `sleep` は、記録のあとで眠る）まで待って、
/// そのときまでに来た `op` の要求の数を返す（最大 `secs` 秒）。そのとき、その要求は飛んでいる（応答を待っている）。
pub fn wait_for_call(env: &Env, op: &str, secs: u64) -> usize {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        // 偽のサーバーが書いている途中の行（読めない行）は、読み飛ばして、次の周回で読む
        let text = std::fs::read_to_string(env.fake_dir.join("calls.log")).unwrap_or_default();
        let n = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|c| c["kind"] == "http" && c["op"] == op)
            .count();
        if n > 0 {
            return n;
        }
        assert!(Instant::now() < deadline, "{op} が呼ばれません");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 呼び出しの記録の行数。止まったあと（`quiet_since`）に、新しい要求が来ないことの確認に使う。
pub fn call_count(env: &Env) -> usize {
    env.calls().len()
}

/// 呼び出しの記録の行数が、`ms` ミリ秒のあいだ増えない（止まったあとに、飛んでいる要求も新しい要求も無い）。
#[track_caller]
pub fn assert_quiet(env: &Env, ms: u64) {
    let before = call_count(env);
    std::thread::sleep(Duration::from_millis(ms));
    assert_eq!(call_count(env), before, "止まったあとに、要求が来ています");
}

/// 状態ディレクトリの `staging/` の直下の数。
pub fn staging_entries(env: &Env) -> usize {
    std::fs::read_dir(env.state.join("staging"))
        .map(|d| d.count())
        .unwrap_or(0)
}

pub fn job_files(env: &Env) -> usize {
    std::fs::read_dir(env.state.join("jobs"))
        .map(|d| d.count())
        .unwrap_or(0)
}

// ---- 実行枠の、ほかの試験の fork との相互作用への備え ----
//
// 試験は 1 つのプロセスの中で、ほかの試験のスレッドが子プロセス（CLI）を起動している。fork から exec までの短い間、子プロセスは
// 親の fd（`O_CLOEXEC` の実行枠のファイルを含む）を持っていて、flock は開いたファイルに付くので、その間は親が手放しても
// 枠が空かない。本番では、枠を持っている側だけが子プロセスを起動するので起きない。「取れるはず」の確かめは、少し待って取り直す。

use std::path::Path;

use geoframe_export::lock::{self, Slot};
use geoframe_export::service::SubmitError;

/// 実行枠が空いているか（空くまで最大 5 秒待つ）。取れたらすぐ手放す。
pub fn slot_is_free(state: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Slot::Got(_) = lock::try_acquire(state).expect("lock") {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 実行枠が空くまで待って受け付ける（上の理由。本当に実行中のときの `Busy` を確かめる試験では使わない）。
pub fn submit_retry(
    svc: &Service,
    req: &ExportRequest,
) -> Result<geoframe_export::service::Submitted, SubmitError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match svc.submit(req) {
            Err(SubmitError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

pub fn run_foreground_retry(
    svc: &Service,
    req: &ExportRequest,
) -> Result<geoframe_export::jobs::JobRecord, SubmitError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match svc.run_foreground(req) {
            Err(SubmitError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

pub fn dry_run_retry(
    svc: &Service,
    req: &ExportRequest,
) -> Result<Value, geoframe_export::service::DryRunError> {
    use geoframe_export::service::DryRunError;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match svc.dry_run(req) {
            Err(DryRunError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// 実行枠が空くまで待って起動時の復旧を行う（`Skipped` を確かめる試験では使わない）。
pub fn startup_recovery_retry(svc: &Service) -> geoframe_export::service::StartupReport {
    use geoframe_export::service::StartupReport;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match svc.startup_recovery() {
            StartupReport::Skipped if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}
