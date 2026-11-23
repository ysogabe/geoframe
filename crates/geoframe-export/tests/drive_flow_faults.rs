//! 流れ（ジョブ）の段の、故障注入の受け入れ試験（docs/note-photo-export/drive-api.md 7 章）。偽の写真サービスと、偽の Drive の
//! HTTP サーバー（故障を差し込める）だけを使う。本物の写真サービス・Google には触れない。
//!
//! 見るもの: 応答を失った作成・送信で重複したフォルダ・ファイルができないこと、流量制限（403・429）を待って成功すること、401 で
//! トークンを取り直すこと、`invalid_grant` でジョブが `drive_auth_invalid` になること、一覧のページ送り、途中で切れた resumable
//! （続きから・同じ ID で始め直し）、写真の並列送信と manifest が最後であること、1 つの失敗で残りを始めないこと、取り消しで全スレッドが
//! 止まって記録が復旧へ渡ること。再試行の待ちは短くする（`Tuning::drive_retry`）。

mod common;

use std::time::{Duration, Instant};

use common::FakeSite;
use common::drive::*;
use common::fake_drive as fk;
use common::site::*;
use geoframe_export::cancel::Cancel;
use geoframe_export::error::ErrorCode;
use geoframe_export::flow::Tuning;
use geoframe_export::jobs::{JobRecord, Status};
use geoframe_export::service::Service;
use serde_json::Value;

const DAY: &str = "2030-03-10";
const NAMES: [&str; 4] = [
    "090000_1.jpg",
    "090100_2.jpg",
    "103000_3.jpg",
    "manifest.json",
];

fn three() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

fn six() -> Vec<Value> {
    (1..=6)
        .map(|i| row(i, &format!("2030-03-10 09:0{i}")))
        .collect()
}

/// 再試行の待ちを短くした `Service`（`retries` が 0 なら、再試行しない）。
fn service_with(env: &mut Env, site: &FakeSite, tuning: Tuning) -> Service {
    env.settings.feed_base = site.base.clone();
    Service::with_tuning(env.settings.clone(), tuning).expect("service")
}

fn retrying() -> Tuning {
    Tuning {
        drive_retry: Some(fast_retry()),
        ..fast_tuning()
    }
}

fn run_job(svc: &Service) -> JobRecord {
    run_foreground_retry(svc, &day()).expect("受け付ける")
}

/// 宛先（ゴミ箱に入っていないもの）の直下の名前。重複があれば、そのまま重複して出る。
fn dest_names(env: &Env) -> Vec<String> {
    let d = env.drive();
    let Some(dest) = d.resolve(&["yamareco", DAY]) else {
        return Vec::new();
    };
    let mut v: Vec<String> = d
        .children(&dest)
        .iter()
        .filter(|n| !n.trashed)
        .map(|n| n.name.clone())
        .collect();
    v.sort();
    v
}

fn assert_published_once(env: &Env) {
    assert_eq!(dest_names(env), NAMES, "宛先に、重複のない一式がある");
    // `incoming/` は 1 つだけ。そこに `incoming/<job_id>` は残っていない
    let d = env.drive();
    let work = d.resolve(&["geoframe-export-work"]).unwrap();
    let incomings: Vec<_> = d
        .children(&work)
        .into_iter()
        .filter(|n| n.name == "incoming" && !n.trashed)
        .collect();
    assert_eq!(incomings.len(), 1, "incoming/ が重複していない");
    assert!(
        d.children(&incomings[0].id).iter().all(|n| n.trashed),
        "incoming/ に何も残っていない"
    );
}

/// 作成の要求に使った ID の種類（再送は同じ ID を使うので、要求の数より少ない）。
fn distinct_new_ids(env: &Env, op: &str) -> usize {
    let mut ids: Vec<String> = env
        .http_calls()
        .iter()
        .filter(|c| c["op"] == op)
        .filter_map(|c| c["new_id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    ids.len()
}

fn requests_of(env: &Env, op: &str) -> usize {
    env.http_calls().iter().filter(|c| c["op"] == op).count()
}

// ---- 応答を失った作成・送信は、重複しない ----

#[test]
fn a_lost_create_or_upload_response_never_duplicates_a_folder_or_a_file() {
    let mut env = Env::new("faults-lost");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    // `incoming/` の作成・`incoming/<job_id>` の作成・最初の写真 2 枚の送信、の応答を失う（サーバーは作ってから接続を切る）
    env.add_rule("create_folder", 0, 2, fk::do_then_drop());
    env.add_rule("upload", 0, 2, fk::do_then_drop());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    // 要求は再送された（2 + 2 回の失敗）が、使った ID は 2 つ（フォルダ）・4 つ（ファイル）だけ
    assert_eq!(requests_of(&env, "create_folder"), 4);
    assert_eq!(distinct_new_ids(&env, "create_folder"), 2);
    assert_eq!(requests_of(&env, "upload"), 6);
    assert_eq!(distinct_new_ids(&env, "upload"), 4);
    env.assert_api_calls_safe();
}

// ---- 流量制限は待って成功する。上限を超えたら失敗する ----

#[test]
fn rate_limits_are_waited_out_and_a_limit_that_never_ends_fails_the_job() {
    let mut env = Env::new("faults-rate");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("list", 0, 2, fk::rate_limit_403());
    env.add_rule("upload", 1, 3, fk::too_many_requests(1));
    env.add_rule("move", 0, 1, fk::rate_limit_403());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    assert!(requests_of(&env, "list") > 2);

    // 送信が流量制限を返し続ける: 回数の上限で失敗し（rclone_failed）、宛先は公開されず、incoming は片付く
    let mut env = Env::new("faults-rate-upload");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("upload", 0, u64::MAX, fk::rate_limit_403());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Failed);
    assert_eq!(rec.error, Some(ErrorCode::RcloneFailed));
    assert!(dest_names(&env).is_empty());
    assert_eq!(env.op_count("move"), 0);
    // 一覧が流量制限を返し続ける: 照会の失敗（drive_unavailable）。何も書かない
    let mut env = Env::new("faults-rate-list");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("list", 0, u64::MAX, fk::too_many_requests(0));
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Failed);
    assert_eq!(rec.error, Some(ErrorCode::DriveUnavailable));
    env.assert_no_mutations();
}

// ---- 401: トークンの取り直し。invalid_grant: drive_auth_invalid ----

#[test]
fn a_rejected_token_is_taken_again_once_and_the_job_succeeds() {
    let mut env = Env::new("faults-401");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("list", 2, 1, fk::unauthorized());
    env.add_rule("upload", 1, 1, fk::unauthorized());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    // 最初の 1 回と、401 のあとの取り直しだけ（要求のたびには取らない）
    let tokens = env.op_count("token");
    assert!((2..=3).contains(&tokens), "{tokens}");
}

#[test]
fn a_revoked_grant_fails_the_job_with_drive_auth_invalid_before_and_during_the_job() {
    // 最初から: トークンが取れない。Drive の中身には触れない
    let mut env = Env::new("faults-grant-first");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("token", 0, u64::MAX, fk::invalid_grant());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Failed);
    assert_eq!(rec.error, Some(ErrorCode::DriveAuthInvalid));
    assert_eq!(env.ops_used(), ["token"]);
    assert_eq!(env.op_count("token"), 1, "再試行しない");

    // ジョブの途中: 送信が 401 になり、取り直したトークンも `invalid_grant`
    let mut env = Env::new("faults-grant-mid");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(&mut env, &site, retrying());
    env.add_rule("upload", 1, 1, fk::unauthorized());
    env.add_rule("token", 1, u64::MAX, fk::invalid_grant());
    let rec = run_job(&svc);
    // 送信の結果が分からないまま、観察もできない（認可が失われた）ので、記録は `blocked`（unobservable）。`recover` で Drive を確かめて
    // 解除するまで、次の書き出しは断る（これまでの、観察できないときの扱いと同じ）。公開はしていない
    assert_eq!(
        rec.status,
        Status::Blocked,
        "{:?} {:?}",
        rec.error,
        rec.notes
    );
    assert_eq!(
        rec.blocked.as_ref().map(|b| b.reason.as_str()),
        Some("unobservable")
    );
    assert!(dest_names(&env).is_empty());
    assert_eq!(env.op_count("move"), 0);
    assert!(svc.blocking().is_some());
}

// ---- ページ送り ----

#[test]
fn listings_follow_every_page_and_a_page_token_that_never_changes_fails() {
    let mut env = Env::new("faults-pages");
    let mut d = env.drive();
    for i in 0..5 {
        d.mkdir_p(&["yamareco", &format!("2020-01-0{}", i + 1)]);
    }
    env.save(&d);
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(
        &mut env,
        &site,
        Tuning {
            drive_sizes: Some((2, 8 << 20, 8 << 20)),
            ..retrying()
        },
    );
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    let paged = env
        .http_calls()
        .iter()
        .filter(|c| c["op"] == "list" && c["has_page_token"] == true)
        .count();
    assert!(paged >= 2, "{paged}");

    // 同じ nextPageToken が返り続ける: 上限に達する前に、失敗として止める（drive_unavailable。何も書かない）
    let mut env = Env::new("faults-page-loop");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(
        &mut env,
        &site,
        Tuning {
            drive_sizes: Some((2, 8 << 20, 8 << 20)),
            ..retrying()
        },
    );
    env.add_rule("list", 0, u64::MAX, fk::repeat_page_token());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Failed);
    assert_eq!(rec.error, Some(ErrorCode::DriveUnavailable));
    env.assert_no_mutations();
}

// ---- 途中で切れた resumable ----

#[test]
fn a_resumable_upload_cut_in_the_middle_resumes_and_a_lost_session_restarts_with_the_same_id() {
    // どの写真も resumable にする（multipart の上限を 1 バイトにする）
    let sizes = Some((1000, 1, 256 << 10));
    let mut env = Env::new("faults-resumable");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(
        &mut env,
        &site,
        Tuning {
            drive_sizes: sizes,
            ..retrying()
        },
    );
    // 最初の 2 回のチャンクが、途中で切れる
    env.fake
        .add_rule_contains("upload", "phase:chunk", 0, 2, fk::cut_after(300));
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    let status_queries = env
        .http_calls()
        .iter()
        .filter(|c| c["op"] == "upload" && c["phase"] == "status")
        .count();
    assert!(
        status_queries >= 2,
        "受け取り済みの位置を問い合わせた: {status_queries}"
    );
    // 写真 3 枚と manifest（4 つ）の送信を、別の ID で始めていない
    assert_eq!(distinct_new_ids(&env, "upload"), 4);
    // 中身は、送ったものと同じ（作業領域の MD5）
    let d = env.drive();
    let dest = d.resolve(&["yamareco", DAY]).unwrap();
    for n in d.children(&dest) {
        let blob = env.fake.blob(&n.id).expect("blob");
        assert_eq!(n.effective_md5(), model::md5_hex(&blob), "{}", n.name);
    }

    // セッションが失われた: 同じ ID で始め直す（重複しない）
    let mut env = Env::new("faults-lost-session");
    let site = photo_site(DAY, DAY, three());
    let svc = service_with(
        &mut env,
        &site,
        Tuning {
            drive_sizes: sizes,
            ..retrying()
        },
    );
    env.fake
        .add_rule_contains("upload", "phase:chunk", 0, 1, fk::lose_session());
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    assert_published_once(&env);
    assert_eq!(
        distinct_new_ids(&env, "upload"),
        4,
        "始め直しても ID は同じ"
    );
    let inits = env
        .http_calls()
        .iter()
        .filter(|c| c["op"] == "upload" && c["phase"] == "init")
        .count();
    assert_eq!(inits, 5, "セッションは 1 つ多く始めた");
}

// ---- 並列の送信・manifest は最後・1 つの失敗で残りを始めない ----

#[test]
fn photos_are_sent_four_at_a_time_and_the_manifest_goes_last() {
    let mut env = Env::new("faults-parallel");
    let site = photo_site(DAY, DAY, six());
    let svc = service_with(&mut env, &site, retrying());
    // どの送信も 700ms かかる（要求は、記録されてから眠る）
    env.add_rule("upload", 0, u64::MAX, sleep_ms(700));
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || run_job(&svc))
    };
    // 最初の送信が記録された時点から、700ms のあいだに、飛んでいる送信がちょうど 4 つ（並列は 4）。起点は、最初の送信の記録
    // （一覧・画像・フォルダの作成の時間を含めない）
    let wait_started = Instant::now();
    while requests_of(&env, "upload") == 0 {
        assert!(wait_started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(2));
    }
    let first_seen = Instant::now();
    let n = loop {
        let n = requests_of(&env, "upload");
        if n >= 4 || first_seen.elapsed() > Duration::from_millis(600) {
            break n;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(
        n, 4,
        "最初の送信から 600ms 以内に、4 つが同時に飛ぶ（並列は 4）: {n}"
    );
    let rec = worker.join().unwrap();
    assert_eq!(rec.status, Status::Succeeded, "{:?}", rec.error);
    // 写真 6 枚と manifest。manifest は、全部の写真のあと（最後）
    let sent = env.uploaded_subjects();
    assert_eq!(sent.len(), 7);
    assert!(sent[6].ends_with("/manifest.json"), "{sent:?}");
    assert!(sent[..6].iter().all(|s| s.ends_with(".jpg")), "{sent:?}");
}

#[test]
fn a_failed_photo_stops_the_rest_and_the_manifest_is_never_sent() {
    let mut env = Env::new("faults-photo-fails");
    let site = photo_site(DAY, DAY, six());
    // 再試行しない: 最初の送信が 500 で失敗する
    let svc = service_with(&mut env, &site, fast_tuning());
    env.add_rule("upload", 0, 1, fail(500));
    let rec = run_job(&svc);
    assert_eq!(rec.status, Status::Failed);
    assert_eq!(rec.error, Some(ErrorCode::RcloneFailed));
    let sent = env.uploaded_subjects();
    assert!(
        sent.iter().all(|s| !s.ends_with("/manifest.json")),
        "manifest は、全部の写真が成功してから: {sent:?}"
    );
    assert!(sent.len() <= 6);
    assert!(dest_names(&env).is_empty());
    assert_eq!(env.op_count("move"), 0);
    // 失敗のあとの片付け: incoming/<job_id> はゴミ箱へ
    assert_eq!(env.drive().trashed.len(), 1);
    // 全スレッドが止まっている
    assert_quiet(&env, 200);
}

// ---- 取り消し: 全スレッドが止まって、記録が復旧へ渡る ----

#[test]
fn a_cancel_during_parallel_uploads_stops_every_thread_and_hands_the_record_to_recovery() {
    let mut env = Env::new("faults-cancel");
    let site = photo_site(DAY, DAY, six());
    let cancel = Cancel::new();
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_cancel(env.settings.clone(), retrying(), cancel.clone()).unwrap();
    env.add_rule("upload", 0, u64::MAX, sleep_ms(800));
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || run_job(&svc))
    };
    while requests_of(&env, "upload") < 4 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = Instant::now();
    cancel.cancel();
    let rec = worker.join().unwrap();
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    // 飛んでいた 4 つが終わるのを待って、残りの 2 枚と manifest は始めない。返ったあとに、要求は来ない
    assert_eq!(requests_of(&env, "upload"), 4);
    assert_quiet(&env, 300);
    assert_eq!(rec.status, Status::Running, "確定しない（復旧へ渡す）");
    assert_eq!(rec.error, None);
    assert!(slot_is_free(&env.state));
    assert!(svc.blocking().is_none());
    // 次の起動の復旧が、今の Drive の状態（写真は届いている）から片付ける
    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert!(matches!(
        startup_recovery_retry(&next),
        geoframe_export::service::StartupReport::Done { .. }
    ));
    let after = match geoframe_export::jobs::JobStore::open(&env.state)
        .unwrap()
        .read(&rec.job_id)
    {
        geoframe_export::jobs::ReadOutcome::Found(r) => *r,
        other => panic!("{other:?}"),
    };
    assert_eq!(after.status, Status::Interrupted);
}

// ---- ジョブの始めの確認（起動のあとに、フォルダがゴミ箱へ移された） ----

#[test]
fn a_root_folder_moved_to_the_trash_after_startup_fails_the_job_before_anything_is_written() {
    for (name, which) in [("work", 0), ("prefix", 1), ("shortcut-work", 2)] {
        let mut env = Env::new(&format!("faults-roots-{name}"));
        let site = photo_site(DAY, DAY, three());
        let svc = service_with(&mut env, &site, retrying());
        let mut d = env.drive();
        match which {
            0 => d.node_mut_pub(&env.work_id).unwrap().trashed = true,
            1 => d.node_mut_pub(&env.yamareco_id).unwrap().trashed = true,
            _ => {
                // 作業用のフォルダの ID が、ファイルを指すようになった（フォルダでない）
                d.node_mut_pub(&env.work_id).unwrap().kind = common::drive::model::Kind::File;
            }
        }
        env.save(&d);
        let rec = run_job(&svc);
        assert_eq!(rec.status, Status::Failed, "{name}");
        assert_eq!(rec.error, Some(ErrorCode::DriveUnavailable), "{name}");
        env.assert_no_mutations();
        assert!(
            env.ops_used().iter().all(|o| o == "token" || o == "get"),
            "{name}: {:?}",
            env.ops_used()
        );
    }
}
