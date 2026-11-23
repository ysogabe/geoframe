//! T5: ジョブの流れ（design.md 4.1）、実行枠、ジョブの記録、`request_id`、`dry_run`、止まったジョブの復旧、`blocked`。
//! 偽の写真サービス（127.0.0.1）と偽の rclone・偽の Drive だけを使う（本物の写真サービス・rclone・Drive には触れない）。
//! 受け持つ受け入れ条件: AC5・AC5b・AC8・AC9（記録の側）・AC10（dry_run）・「一部の失敗」。
//! Drive の操作の記録は、中立の操作名（`list`・`read`・`create_folder`・`upload`・`move`・`trash`）で見る。`// rclone 固有` と
//! 書いた確認（`assert_rclone_safe_calls`）だけが rclone の引数・環境変数を見る（rclone 版を消すときに消す）。

mod common;

use std::time::Duration;

use common::drive::*;
use common::site::*;
use common::{FakeSite, Reply, closed_port_base};
use geoframe_export::cancel::Cancel;
use geoframe_export::error::ErrorCode;
use geoframe_export::flow::Tuning;
use geoframe_export::jobid::JobId;
use geoframe_export::jobs::{BlockedNote, Blocking, JobRecord, JobStore, Status};
use geoframe_export::lock::{self, Slot};
use geoframe_export::recovery::{Progress, Stage};
use geoframe_export::request::Destination;
use geoframe_export::service::{DryRunError, Lookup, Service, StartupReport, SubmitError};
use serde_json::{Value, json};

const Y: Destination = Destination::Yamareco;
const DAY: &str = "2030-03-10";

fn three_photos() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

fn submit_and_wait(svc: &Service, req: &geoframe_export::request::ExportRequest) -> Value {
    let s = submit_retry(svc, req).expect("受け付ける");
    wait_done(svc, &s.job_id)
}

/// Drive にフォルダ（またはその下のファイル）があるか。偽の Drive の `resolve` はフォルダだけを辿るので、最後の 1 つは
/// 親の子から探す。
fn drive_has(env: &Env, path: &[&str]) -> bool {
    let d = env.drive();
    if d.resolve(path).is_some() {
        return true;
    }
    let Some((name, parent)) = path.split_last() else {
        return false;
    };
    d.resolve(parent).is_some_and(|p| {
        d.children(&p)
            .iter()
            .any(|n| n.name == *name && !n.trashed)
    })
}

// ---- 成功とその記録 ----

#[test]
fn a_single_day_job_publishes_records_and_cleans_up() {
    let mut env = Env::new("flow-ok");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let s = submit_retry(&svc, &day()).unwrap();
    assert_eq!(s.status, Status::Running);
    assert!(!s.reused);
    // 受付の時点で、記録が書かれている（running）
    assert!(matches!(svc.lookup(s.job_id.as_str()), Lookup::Found(_)));
    let v = wait_done(&svc, &s.job_id);

    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["error"], Value::Null);
    assert_eq!(v["job_id"], s.job_id.as_str());
    assert_eq!(
        v["request"],
        json!({"from": DAY, "to": DAY, "destination": "yamareco", "on_exists": "error"})
    );
    let r = &v["result"];
    assert_eq!(r["folder"], DAY);
    assert_eq!(
        (r["catalog_count"].as_u64(), r["count"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(r["uploaded"], true);
    assert_eq!(r["replaced"], false);
    assert!(r["bytes"].as_u64().unwrap() > 0);
    assert_eq!(r["excluded"], json!([]));
    assert_eq!(r["existing_files"], 0);
    assert!(v.get("notes").is_none());

    // 時刻は時差つきの RFC 3339
    for key in ["started_at", "finished_at"] {
        let t = v[key].as_str().unwrap();
        assert_eq!(t.len(), 25, "{t}");
        assert!(t.as_bytes()[19] == b'+' || t.as_bytes()[19] == b'-', "{t}");
    }

    // Drive: 宛先に写真 3 枚と manifest。作業用のフォルダには何も残らない
    for f in [
        "090000_1.jpg",
        "090100_2.jpg",
        "103000_3.jpg",
        "manifest.json",
    ] {
        assert!(drive_has(&env, &["yamareco", DAY, f]), "{f}");
    }
    assert!(!drive_has(
        &env,
        &["geoframe-export-work", "incoming", s.job_id.as_str()]
    ));
    // 公開の記録
    assert!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap())
            .is_some()
    );
    // 作業領域は消えている。実行枠は外れている
    assert_eq!(staging_entries(&env), 0);
    assert!(slot_is_free(&env.state));
    // 最後に記録した段階は publishing（新規）で、incoming の ID は公開したフォルダの ID。各段階の記録が Drive の操作より先に
    // 書かれること自体は、T4 の試験（ProgressSink の呼び出しの順）で確かめている
    let rec = read_record(&env, &s.job_id);
    let progress = rec.progress.expect("段階の記録");
    assert_eq!(progress.stage, Stage::Publishing);
    assert_eq!(progress.old_id, None);
    assert_eq!(
        progress.incoming_id,
        env.drive().resolve(&["yamareco", DAY]),
        "記録した incoming の ID が、公開されたフォルダの ID"
    );
    assert!(rec.summary.is_some());
    // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
    env.assert_api_calls_safe();
}

#[test]
fn a_period_job_uses_day_subfolders() {
    let mut env = Env::new("flow-period");
    let photos = vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-11 08:00")];
    let site = photo_site(DAY, "2030-03-11", photos);
    let svc = service(&mut env, &site);
    let v = submit_and_wait(&svc, &request(DAY, Some("2030-03-11"), None, None));
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["result"]["folder"], "2030-03-10-11");
    assert!(drive_has(
        &env,
        &["yamareco", "2030-03-10-11", "2030-03-10", "090000_1.jpg"]
    ));
    assert!(drive_has(
        &env,
        &["yamareco", "2030-03-10-11", "2030-03-11", "080000_2.jpg"]
    ));
}

// ---- AC5: 0 枚 ----

#[test]
fn zero_photos_succeeds_without_touching_drive_even_if_the_destination_exists() {
    for existing in [false, true] {
        let mut env = Env::new(&format!("flow-zero-{existing}"));
        if existing {
            let set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
            env.seed_folder(Y, DAY, &set);
        }
        let before = env.drive().to_json();
        let site = photo_site(DAY, DAY, vec![]);
        let svc = service(&mut env, &site);
        let v = submit_and_wait(&svc, &day());
        assert_eq!(v["status"], "succeeded", "{v}");
        let r = &v["result"];
        assert_eq!(r["count"], 0);
        assert_eq!(r["uploaded"], false);
        assert_eq!(r["catalog_count"], 0);
        assert_eq!(r["replaced"], false);
        assert_eq!(v["error"], Value::Null);
        // rclone は 1 回も呼ばれず、Drive は変わらない（フォルダも作らない）
        assert!(env.calls().is_empty(), "{existing}: {:?}", env.calls());
        assert_eq!(env.drive().to_json(), before);
        assert_eq!(staging_entries(&env), 0);
    }
}

// ---- AC5b と「一部の失敗」 ----

#[test]
fn one_failing_photo_fails_the_job_and_drive_is_untouched() {
    for case in ["not found", "not a jpeg"] {
        let mut env = Env::new(&format!("flow-onebad-{}", case.len()));
        let photos = three_photos();
        let routes = {
            let mut r = vec![(
                "/api/admin/export/photos".to_owned(),
                common::Reply::json(serde_json::to_vec(&listing(DAY, DAY, photos)).unwrap()),
            )];
            r.push((
                "/derivatives/p1.jpg".into(),
                common::Reply::jpeg(jpeg(64, 48, 1)),
            ));
            if case == "not a jpeg" {
                r.push((
                    "/derivatives/p2.jpg".into(),
                    common::Reply::jpeg(b"not a jpeg at all".to_vec()),
                ));
            }
            r.push((
                "/derivatives/p3.jpg".into(),
                common::Reply::jpeg(jpeg(64, 48, 3)),
            ));
            r
        };
        let site = FakeSite::start(routes);
        let before = env.drive().to_json();
        let svc = service(&mut env, &site);
        let v = submit_and_wait(&svc, &day());
        let code = if case == "not found" {
            "fetch_failed"
        } else {
            "decode_failed"
        };
        assert_eq!(v["status"], "failed", "{v}");
        assert_eq!(v["error"]["code"], code, "{case}");
        // rclone は lsjson も呼ばれない。Drive は変わらず、作業領域も残らない
        assert!(env.calls().is_empty(), "{case}: {:?}", env.calls());
        assert_eq!(env.drive().to_json(), before);
        assert_eq!(staging_entries(&env), 0);
        // 失敗でも、そこまでの数は結果に載る。公開していないので count は 0
        assert_eq!(v["result"]["catalog_count"], 3);
        assert_eq!(v["result"]["count"], 0);
        assert_eq!(v["result"]["uploaded"], false);
    }
}

#[test]
fn pending_derivatives_fail_before_anything_is_fetched_or_touched() {
    let mut env = Env::new("flow-pending");
    let photos = vec![
        row(1, "2030-03-10 09:00"),
        pending_row(2, "2030-03-10 09:05"),
    ];
    let site = photo_site(DAY, DAY, photos);
    let svc = service(&mut env, &site);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "derivative_pending");
    assert_eq!(v["result"]["catalog_count"], 2);
    assert!(env.calls().is_empty());
    // 画像は 1 枚も取っていない（一覧の 1 回だけ）
    assert_eq!(site.request_lines().len(), 1, "{:?}", site.request_lines());
    assert_eq!(staging_entries(&env), 0);
}

#[test]
fn when_everything_is_excluded_nothing_to_export_never_queries_drive() {
    // 宛先が無い・ある・照会が失敗する、のどの状態でも同じ結果で、Drive を照会しない
    for state in ["absent", "exists", "lsjson fails"] {
        let mut env = Env::new(&format!("flow-nothing-{}", state.len()));
        match state {
            "exists" => {
                let set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
                env.seed_folder(Y, DAY, &set);
            }
            "lsjson fails" => env.add_rule("list", 0, 99, fail(1)),
            _ => {}
        }
        let photos = vec![
            original_row(1, "2030-03-10 09:00"),
            original_row(2, "2030-03-10 09:05"),
        ];
        let site = site_without_images(DAY, DAY, photos);
        let svc = service(&mut env, &site);
        let v = submit_and_wait(&svc, &day());
        assert_eq!(v["status"], "failed", "{state}: {v}");
        assert_eq!(v["error"]["code"], "nothing_to_export", "{state}");
        assert!(env.calls().is_empty(), "{state}: {:?}", env.calls());
        assert_eq!(v["result"]["catalog_count"], 2);
        assert_eq!(
            v["result"]["excluded"],
            json!([{"id": 1, "reason": "not_derivative"}, {"id": 2, "reason": "not_derivative"}])
        );
    }
}

#[test]
fn an_unreachable_photo_service_fails_without_touching_drive() {
    let mut env = Env::new("flow-nofeed");
    env.settings.feed_base = closed_port_base();
    let svc = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "feed_unavailable");
    assert_eq!(v["result"], Value::Null);
    assert!(env.calls().is_empty());
    // 失敗でも、実行枠は外れている
    assert!(slot_is_free(&env.state));
}

// ---- conflict と replace ----

#[test]
fn an_existing_destination_ends_in_conflict_unless_replace_targets_a_managed_folder() {
    let mut env = Env::new("flow-conflict");
    let old = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);

    // 既定の error: conflict（exists）。copy・moveto・purge は呼ばれない
    let old_id = env.seed_managed(Y, DAY, &old);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "conflict", "{v}");
    assert_eq!(v["result"]["conflict"], "exists");
    assert_eq!(v["result"]["uploaded"], false);
    assert_eq!(v["result"]["count"], 0);
    assert_eq!(v["result"]["catalog_count"], 3);
    assert_eq!(v["result"]["existing_files"], 2, "写真 1 枚と manifest");
    assert_eq!(v["error"], Value::Null);
    // 呼んだのは、トークン・起動後の宛先の親と作業用のフォルダの確認（get）・一覧だけ（読むだけ）
    assert_eq!(
        env.ops_used(),
        ["get", "list", "token"],
        "{:?}",
        env.ops_used()
    );
    assert_eq!(
        env.drive().resolve(&["yamareco", DAY]),
        Some(old_id.clone())
    );

    // replace でも、管理していないフォルダは not_managed
    let mut env2 = Env::new("flow-conflict2");
    let set2 = env2.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    env2.seed_folder(Y, DAY, &set2);
    let svc2 = service(&mut env2, &site);
    let v = submit_and_wait(&svc2, &request(DAY, None, Some("replace"), None));
    assert_eq!(v["status"], "conflict", "{v}");
    assert_eq!(v["result"]["conflict"], "not_managed");
    assert!(!env2.ops_used().contains(&"move".to_owned()));
    assert!(!env2.ops_used().contains(&"trash".to_owned()));

    // 管理しているフォルダの replace は成功し、旧フォルダはゴミ箱へ。公開の記録は新しい ID になる
    let v = submit_and_wait(&svc, &request(DAY, None, Some("replace"), None));
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["result"]["replaced"], true);
    assert_eq!(v["result"]["uploaded"], true);
    assert_eq!(v["result"]["count"], 3);
    assert_eq!(v["result"]["existing_files"], 2);
    let new_id = env.drive().resolve(&["yamareco", DAY]).unwrap();
    assert_ne!(new_id, old_id);
    assert_eq!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap()),
        Some(new_id)
    );
    assert!(env.drive().trashed.contains(&old_id));
    assert_eq!(staging_entries(&env), 0);
}

// ---- AC8: 実行枠 ----

#[test]
fn a_held_slot_refuses_jobs_and_dry_run_and_creates_no_record() {
    let mut env = Env::new("flow-busy");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let Slot::Got(held) = lock::try_acquire(&env.state).unwrap() else {
        panic!("取れるはず")
    };
    assert_eq!(svc.submit(&day()).unwrap_err(), SubmitError::Busy);
    assert_eq!(svc.dry_run(&day()).unwrap_err(), DryRunError::Busy);
    assert_eq!(svc.startup_recovery(), StartupReport::Skipped);
    assert_eq!(job_files(&env), 0, "断った要求の記録は作らない");
    assert!(env.calls().is_empty());
    assert_eq!(SubmitError::Busy.code(), ErrorCode::Busy);
    drop(held);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "succeeded");
}

/// 本文の検査のあと（HTTP の入口の確認のあと）に終了の印が立つ隙間: 排他と実行枠を取ったあと、記録を書く直前に印を確かめ、
/// 立っていれば何も登録しない（記録も rclone の呼び出しも実行枠の保持も残らない）。
#[test]
fn a_shutdown_mark_raised_just_before_the_registration_registers_nothing() {
    let mut env = Env::new("flow-shutdown-race");
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let cancel = Cancel::new();
    let svc = Service::with_cancel(env.settings.clone(), fast_tuning(), cancel.clone()).unwrap();
    // 最初の request_id の照会のあと、実行枠を取る前に印が立つ（上の隙間を、決まった順に起こす）
    let raiser = cancel.clone();
    let e = svc
        .submit_with_hook(&request(DAY, None, None, Some("rid-race")), &|| {
            raiser.cancel()
        })
        .unwrap_err();
    assert_eq!(e, SubmitError::ShuttingDown);
    assert_eq!(e.code(), ErrorCode::ShuttingDown);
    assert_eq!(job_files(&env), 0, "記録を作らない");
    assert!(env.calls().is_empty(), "rclone を呼ばない");
    assert!(slot_is_free(&env.state), "実行枠を残さない");
    assert!(svc.wait_idle(Duration::ZERO), "ジョブのスレッドは作らない");
    // 印が立っている間は、そのあとの受付もすべて断る
    assert_eq!(svc.submit(&day()).unwrap_err(), SubmitError::ShuttingDown);
    assert_eq!(job_files(&env), 0);
}

#[test]
fn a_second_job_while_one_is_running_is_refused_and_the_first_still_finishes() {
    let mut env = Env::new("flow-running");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    // 写真を送る copy を 1 秒止める（実行中を作る）
    env.add_rule("upload", 0, 1, sleep_ms(1000));
    let first = svc.submit(&day()).unwrap();
    // 2 件目（別の要求）は断る。dry_run も
    assert_eq!(
        svc.submit(&request("2030-03-12", None, None, None))
            .unwrap_err(),
        SubmitError::Busy
    );
    assert_eq!(svc.dry_run(&day()).unwrap_err(), DryRunError::Busy);
    let v = wait_done(&svc, &first.job_id);
    assert_eq!(v["status"], "succeeded", "{v}");
}

// ---- request_id ----

#[test]
fn a_repeated_request_id_returns_the_same_job_even_while_it_runs() {
    let mut env = Env::new("flow-rid");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    env.add_rule("upload", 0, 1, sleep_ms(800));
    let req = request(DAY, None, None, Some("req-1"));
    let first = svc.submit(&req).unwrap();
    // 実行中（実行枠は取れない）でも、同じ request_id は同じジョブを返す（Busy にならない）
    let again = svc.submit(&req).unwrap();
    assert_eq!(again.job_id, first.job_id);
    assert!(again.reused);
    assert_eq!(again.status, Status::Running);
    // 中身が違えば request_id_mismatch（実行中でも）
    for other in [
        request("2030-03-11", None, None, Some("req-1")),
        request(DAY, Some("2030-03-11"), None, Some("req-1")),
        request(DAY, None, Some("replace"), Some("req-1")),
    ] {
        let e = svc.submit(&other).unwrap_err();
        assert_eq!(e, SubmitError::RequestIdMismatch);
        assert_eq!(e.code(), ErrorCode::RequestIdMismatch);
    }
    let v = wait_done(&svc, &first.job_id);
    assert_eq!(v["status"], "succeeded");
    // 終わったあとも、同じ request_id は同じジョブ（今の状態つき）。新しいジョブは作らない
    let after = svc.submit(&req).unwrap();
    assert_eq!(
        (after.job_id.clone(), after.status, after.reused),
        (first.job_id.clone(), Status::Succeeded, true)
    );
    assert_eq!(job_files(&env), 1);
    // request_id が違えば、新しいジョブ（宛先が既にあるので conflict で終わる）
    let other = submit_and_wait(&svc, &request(DAY, None, None, Some("req-2")));
    assert_eq!(other["status"], "conflict");
    assert_eq!(job_files(&env), 2);
}

// ---- AC9: 記録 ----

#[test]
fn records_survive_a_restart_and_unknown_or_malformed_ids_are_not_found() {
    let mut env = Env::new("flow-restart");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let s = svc.submit(&request(DAY, None, None, Some("r"))).unwrap();
    let done = wait_done(&svc, &s.job_id);
    drop(svc);

    // 再起動に相当する、新しい Service（同じ状態ディレクトリ）
    let svc2 = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert_eq!(svc2.lookup(s.job_id.as_str()), Lookup::Found(done.clone()));
    let list = svc2.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["job_id"], s.job_id.as_str());
    assert_eq!(list[0]["status"], "succeeded");
    assert!(
        list[0].get("result").is_none(),
        "一覧は job_id・status・request・時刻だけ"
    );
    assert_eq!(list[0]["request"]["destination"], "yamareco");
    // request_id の再送も、再起動のあとで同じジョブ
    let again = svc2.submit(&request(DAY, None, None, Some("r"))).unwrap();
    assert_eq!((again.job_id, again.reused), (s.job_id, true));

    for bad in [
        "",
        "../../etc/passwd",
        "0123456789ABCDEF0123456789abcdef",
        "00000000000000000000000000000001",
    ] {
        assert_eq!(svc2.lookup(bad), Lookup::NotFound, "{bad}");
    }
}

#[test]
fn only_the_newest_finished_records_are_kept() {
    let mut env = Env::new("flow-prune");
    let site = photo_site(DAY, DAY, vec![]);
    let svc = service(&mut env, &site);
    // 0 枚のジョブは速い。直前に、読めない記録・未完了でない古い blocked を足しておく
    let store = JobStore::open(&env.state).unwrap();
    let mut blocked = JobRecord::new(JobId::parse(&format!("{:032x}", 9_000)).unwrap(), &day());
    settle(&mut blocked, Status::Blocked);
    blocked.blocked = Some(BlockedNote {
        reason: "unobservable".into(),
        description: String::new(),
    });
    blocked.started_ns = 1;
    blocked.resolved_at = Some("2030-01-01T00:00:00+09:00".into());
    store.write(&blocked).unwrap();
    let mut last = None;
    for _ in 0..55 {
        let rec = run_foreground_retry(&svc, &day()).unwrap();
        assert_eq!(rec.status, Status::Succeeded);
        last = Some(rec.job_id);
    }
    assert_eq!(job_files(&env), 50, "終わったジョブは直近 50 件だけ");
    assert!(matches!(
        svc.lookup(last.unwrap().as_str()),
        Lookup::Found(_)
    ));
    assert_eq!(
        svc.lookup(&format!("{:032x}", 9_000)),
        Lookup::NotFound,
        "古い解除済みの記録は間引かれる"
    );
}

// ---- dry_run ----

#[test]
fn dry_run_reports_the_destination_and_only_queries_drive() {
    let mut env = Env::new("flow-dry");
    let photos = vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:05"),
        original_row(3, "2030-03-10 10:00"),
        pending_row(4, "2030-03-10 11:00"),
    ];
    let site = photo_site(DAY, DAY, photos);
    let svc = service(&mut env, &site);

    let v = dry_run_retry(&svc, &day()).unwrap();
    assert_eq!(
        v,
        json!({
            "folder": DAY, "destination": "yamareco", "candidate_count": 4,
            "days": {DAY: {"count": 4, "first": "2030-03-10 09:00", "last": "2030-03-10 11:00", "ids": [1, 2, 3, 4]}},
            "target": "absent", "managed": null, "existing_files": 0,
            "excluded": {"not_derivative": 1}, "derivative_pending": 1,
            "undated_in_catalog": 0, "invalid_time_in_catalog": 0
        })
    );
    // 管理しているフォルダ・していないフォルダ
    let set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    env.seed_managed(Y, DAY, &set);
    let v = dry_run_retry(&svc, &day()).unwrap();
    assert_eq!(
        (
            v["target"].as_str(),
            v["managed"].as_bool(),
            v["existing_files"].as_u64()
        ),
        (Some("exists"), Some(true), Some(2))
    );
    let mut env2 = Env::new("flow-dry2");
    let set2 = env2.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    env2.seed_folder(Y, DAY, &set2);
    let svc2 = service(&mut env2, &site);
    let v = dry_run_retry(&svc2, &day()).unwrap();
    assert_eq!(
        (v["target"].as_str(), v["managed"].as_bool()),
        (Some("exists"), Some(false))
    );

    // 呼んだのは、一覧・読み出しとトークンだけ（読むだけ）。実行枠は外れている。ジョブの記録は作らない
    for e in [&env, &env2] {
        for c in e.ops_used() {
            assert!(c == "list" || c == "read" || c == "token", "{c}");
        }
    }
    assert_eq!(job_files(&env), 0);
    assert!(slot_is_free(&env.state));
    // 画像は取っていない（一覧の 3 回だけ）
    assert!(
        site.request_lines()
            .iter()
            .all(|l| l.contains("/api/admin/export/photos"))
    );
}

#[test]
fn dry_run_failures_are_reported_as_failed_errors() {
    let mut env = Env::new("flow-dry-fail");
    env.settings.feed_base = closed_port_base();
    let svc = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert_eq!(
        dry_run_retry(&svc, &day()).unwrap_err().code(),
        ErrorCode::FeedUnavailable
    );

    let mut env = Env::new("flow-dry-ambig");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let mut d = env.drive();
    d.mkdir_dup(&["yamareco"], DAY);
    d.mkdir_dup(&["yamareco"], DAY);
    env.save(&d);
    assert_eq!(
        dry_run_retry(&svc, &day()).unwrap_err().code(),
        ErrorCode::AmbiguousDestination
    );
}

// ---- 期限・パニック・公開の記録 ----

#[test]
fn a_panic_in_the_job_fails_it_and_releases_the_slot() {
    let mut env = Env::new("flow-panic");
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_tuning(
        env.settings.clone(),
        Tuning {
            panic_in_job: true,
            ..fast_tuning()
        },
    )
    .unwrap();
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "internal");
    assert!(slot_is_free(&env.state));
    assert!(env.calls().is_empty());
}

#[test]
fn the_job_deadline_kills_rclone_and_fails_with_timeout() {
    let mut env = Env::new("flow-timeout");
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_tuning(
        env.settings.clone(),
        Tuning {
            job_timeout: Duration::from_millis(1500),
            ..fast_tuning()
        },
    )
    .unwrap();
    // 写真を送る copy が、期限より長くかかる
    env.add_rule("upload", 0, 1, sleep_ms(20_000));
    let started = std::time::Instant::now();
    let v = submit_and_wait(&svc, &day());
    assert!(started.elapsed() < Duration::from_secs(15), "期限で止まる");
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(v["error"]["code"], "timeout", "{v}");
    // 宛先は変わっていない。残った incoming は片付いている
    assert!(!drive_has(&env, &["yamareco", DAY]));
    assert_eq!(staging_entries(&env), 0);
}

#[test]
fn a_publish_that_cannot_be_recorded_leaves_a_note_but_still_succeeds() {
    let mut env = Env::new("flow-unrecorded");
    // 公開の記録が読めない（上書きしない）
    let record = env.published.path_of(
        geoframe_export::request::Destination::Yamareco,
        &geoframe_export::folder::FolderName::parse(DAY).unwrap(),
    );
    std::fs::create_dir_all(env.published.dir()).unwrap();
    std::fs::write(&record, b"{broken").unwrap();
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["result"]["uploaded"], true);
    let notes = v["notes"].as_array().expect("notes");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap().contains("published/")),
        "{notes:?}"
    );
    assert!(drive_has(&env, &["yamareco", DAY, "manifest.json"]));
    assert_eq!(std::fs::read(&record).unwrap(), b"{broken");
}

// ---- 止まったジョブの復旧（AC9） ----

fn running_record(env: &Env, n: u32, progress: Option<Progress>) -> JobRecord {
    let store = JobStore::open(&env.state).unwrap();
    let mut rec = JobRecord::new(JobId::parse(&format!("{n:032x}")).unwrap(), &day());
    rec.started_ns -= i64::from(1000 - n) * 1_000_000_000;
    rec.progress = progress;
    store.write(&rec).unwrap();
    rec
}

fn read_record(env: &Env, id: &JobId) -> JobRecord {
    match JobStore::open(&env.state).unwrap().read(id) {
        geoframe_export::jobs::ReadOutcome::Found(r) => *r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_job_that_stopped_before_touching_drive_becomes_interrupted() {
    let mut env = Env::new("rec-nodrive");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let rec = running_record(&env, 1, None);
    std::fs::create_dir_all(env.state.join("staging/leftover/2030-03-10")).unwrap();
    std::fs::write(env.state.join("staging/leftover/2030-03-10/a.jpg"), b"x").unwrap();

    // GET では、片付くまで running のまま
    let Lookup::Found(v) = svc.lookup(rec.job_id.as_str()) else {
        panic!()
    };
    assert_eq!(v["status"], "running");

    match startup_recovery_retry(&svc) {
        StartupReport::Done { cleaned, recovery } => {
            assert_eq!(cleaned, 1, "staging/ の残骸を消す");
            assert_eq!((recovery.recovered, recovery.blocked), (1, false));
        }
        other => panic!("{other:?}"),
    }
    let after = read_record(&env, &rec.job_id);
    assert_eq!(after.status, Status::Interrupted);
    assert!(after.finished_at.is_some());
    assert_eq!(after.error, None);
    assert!(env.calls().is_empty(), "Drive には触れない");
    assert_eq!(staging_entries(&env), 0);
    // 片付いたジョブがあっても、次の書き出しは動く
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "succeeded");
}

#[test]
fn startup_recovery_follows_the_table_for_uploading_and_for_a_roll_forward() {
    // uploading: incoming は消し、interrupted
    let mut env = Env::new("rec-uploading");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let job = JobId::parse(&format!("{:032x}", 2)).unwrap();
    let inc = env.seed_set_at(&["geoframe-export-work", "incoming", job.as_str()], &set);
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let rec = running_record(
        &env,
        2,
        Some(Progress {
            stage: Stage::Uploading,
            old_id: None,
            incoming_id: Some(inc.clone()),
            digests: None,
        }),
    );
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Interrupted);
    assert!(env.drive().trashed.contains(&inc));
    assert!(!drive_has(
        &env,
        &["geoframe-export-work", "incoming", job.as_str()]
    ));
    assert!(!drive_has(&env, &["yamareco", DAY]));

    // retiring（旧フォルダは退避済み、incoming は検証済み）: 前へ進めて成功にし、公開の記録を書く。数は記録の summary から
    let mut env = Env::new("rec-forward");
    let old_set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    let new_set = env.make_set(
        "new",
        Y,
        DAY,
        &[("090000_1.jpg", 100), ("090100_2.jpg", 200)],
    );
    let job = JobId::parse(&format!("{:032x}", 3)).unwrap();
    let old_id = env.seed_managed(Y, DAY, &old_set);
    let inc = env.seed_set_at(
        &["geoframe-export-work", "incoming", job.as_str()],
        &new_set,
    );
    let layout = env.layout(Y, DAY, job.as_str());
    let mut d = env.drive();
    assert!(d.move_dir(
        &old_id,
        &[
            "geoframe-export-work".to_owned(),
            "replaced".to_owned(),
            layout.retired_name().to_owned()
        ]
    ));
    env.save(&d);
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let mut rec = JobRecord::new(job.clone(), &request(DAY, None, Some("replace"), None));
    rec.progress = Some(Progress {
        stage: Stage::Retiring,
        old_id: Some(old_id.clone()),
        incoming_id: Some(inc.clone()),
        digests: None,
    });
    rec.summary = Some(
        json!({"folder": DAY, "catalog_count": 4, "count": 2, "bytes": 300,
        "excluded": [{"id": 8, "reason": "has_metadata"}], "undated_in_catalog": 1, "invalid_time_in_catalog": 0}),
    );
    JobStore::open(&env.state).unwrap().write(&rec).unwrap();
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    let after = read_record(&env, &job);
    assert_eq!(after.status, Status::Succeeded);
    let result = after.result.unwrap();
    assert_eq!(result["count"], 2);
    assert_eq!(result["catalog_count"], 4);
    assert_eq!(result["bytes"], 300);
    assert_eq!(result["uploaded"], true);
    assert_eq!(result["replaced"], true);
    assert_eq!(
        result["excluded"],
        json!([{"id": 8, "reason": "has_metadata"}])
    );
    assert_eq!(env.drive().resolve(&["yamareco", DAY]), Some(inc.clone()));
    assert_eq!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap()),
        Some(inc)
    );
    assert!(env.drive().trashed.contains(&old_id));
}

#[test]
fn an_unexplainable_state_blocks_everything_until_it_is_resolved() {
    let mut env = Env::new("rec-blocked");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    // 新規の publishing なのに、incoming も宛先も無い（あるはずのものが無い）
    let job = JobId::parse(&format!("{:032x}", 4)).unwrap();
    running_record(
        &env,
        4,
        Some(Progress {
            stage: Stage::Publishing,
            old_id: None,
            incoming_id: Some("fid-gone".into()),
            digests: None,
        }),
    );
    match startup_recovery_retry(&svc) {
        StartupReport::Done { recovery, .. } => assert!(recovery.blocked),
        other => panic!("{other:?}"),
    }
    let rec = read_record(&env, &job);
    assert_eq!(rec.status, Status::Blocked);
    assert_eq!(rec.error, Some(ErrorCode::Blocked));
    let note = rec.blocked.clone().unwrap();
    assert_eq!(note.reason, "stage_inconsistent");
    assert!(
        note.description.contains("publishing"),
        "{}",
        note.description
    );
    assert_eq!(rec.progress.as_ref().unwrap().stage, Stage::Publishing);

    // 次の書き出しも dry_run も断る（503 に当たる）。照会は引ける
    assert!(matches!(
        submit_retry(&svc, &day()),
        Err(SubmitError::Blocked(Blocking::Job { .. }))
    ));
    assert!(matches!(
        dry_run_retry(&svc, &day()),
        Err(DryRunError::Blocked(_))
    ));
    assert_eq!(
        SubmitError::Blocked(svc.blocking().unwrap()).code(),
        ErrorCode::Blocked
    );
    let Lookup::Found(v) = svc.lookup(job.as_str()) else {
        panic!()
    };
    assert_eq!(v["status"], "blocked");
    assert_eq!(v["error"]["code"], "blocked");
    assert!(
        !v.to_string().contains("fid-gone"),
        "GET に Drive の ID は出さない"
    );

    // いったん blocked になったジョブは、自動では動かさない（もう一度復旧しても変わらない）
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    assert_eq!(read_record(&env, &job).status, Status::Blocked);
    assert!(env.calls().iter().all(|c| c["kind"] != "hook"));
}

#[test]
fn a_stuck_job_is_recovered_before_the_next_job_and_a_new_block_fails_that_job() {
    // 各ジョブの前の復旧: 止まったジョブが先に片付いて、新しいジョブが動く
    let mut env = Env::new("rec-prejob");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let stuck = running_record(&env, 5, None);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(read_record(&env, &stuck.job_id).status, Status::Interrupted);

    // 復旧で blocked が出たら、そのジョブは failed（blocked）で終え、ほかの未完了には進まない
    let mut env = Env::new("rec-prejob-blocked");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let a = running_record(
        &env,
        6,
        Some(Progress {
            stage: Stage::Publishing,
            old_id: None,
            incoming_id: Some("fid-gone".into()),
            digests: None,
        }),
    );
    let b = running_record(&env, 7, None);
    let v = submit_and_wait(&svc, &day());
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(v["error"]["code"], "blocked");
    assert_eq!(read_record(&env, &a.job_id).status, Status::Blocked);
    assert_eq!(
        read_record(&env, &b.job_id).status,
        Status::Running,
        "blocked のあとは進まない"
    );
    assert!(env.calls().iter().all(|c| c["op"] != "upload"));
}

#[test]
fn unreadable_records_block_new_jobs_and_are_never_overwritten_or_pruned() {
    let mut env = Env::new("rec-unreadable");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let bad = format!("{:032x}", 77);
    let path = env.state.join("jobs").join(format!("{bad}.json"));
    std::fs::write(&path, b"{\"version\": 1, \"job_id\"").unwrap();
    assert_eq!(svc.lookup(&bad), Lookup::Unreadable);
    assert!(matches!(
        submit_retry(&svc, &day()),
        Err(SubmitError::Blocked(Blocking::UnreadableRecord(_)))
    ));
    assert!(matches!(
        dry_run_retry(&svc, &day()),
        Err(DryRunError::Blocked(_))
    ));
    assert!(svc.blocking().unwrap().text().contains(&bad));
    // 一覧は動く（読めない記録は出さない）。記録はそのまま残る
    assert!(svc.list().unwrap().is_empty());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{\"version\": 1, \"job_id\""
    );
    // 手で退かせば、書き出せる
    std::fs::remove_file(&path).unwrap();
    assert_eq!(submit_and_wait(&svc, &day())["status"], "succeeded");
}

/// 公開の途中（`publishing`）で止まったジョブの記録を、検証済みの MD5 つきで書く。`digests_json` で、記録の中の `digests` を
/// そのまま差し替える（`None` はキーを省く）。
fn publishing_record_with_digests(
    env: &Env,
    n: u32,
    folder_id: &str,
    set: &Set,
    digests_json: Option<Value>,
) -> std::path::PathBuf {
    let rec = running_record(
        env,
        n,
        Some(Progress {
            stage: Stage::Publishing,
            old_id: None,
            incoming_id: Some(folder_id.to_owned()),
            digests: Some(set.recorded()),
        }),
    );
    let path = JobStore::open(&env.state).unwrap().record_path(&rec.job_id);
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let progress = v["progress"].as_object_mut().unwrap();
    match digests_json {
        Some(d) => progress.insert("digests".into(), d),
        None => progress.remove("digests"),
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    path
}

#[test]
fn a_null_or_malformed_digests_in_the_job_record_blocks_instead_of_skipping_the_md5_check() {
    // 公開の記録がまだ無い（同じフォルダ ID の行が無い）復旧で、ジョブの記録の `digests` が `null`（書き手は MD5 が無いときキーごと
    // 省く）や型の違う値になっている。これを旧形式として読むと、止まっている間に写真が同じ名前・同じ大きさのまま書き換えられて
    // いても MD5 の照合を飛ばして公開済みにしてしまう。記録の破損として読めないものにし（`blocked`）、何にも触れない
    for (name, bad) in [
        ("null", Value::Null),
        ("string", json!("x")),
        ("empty", json!([])),
        (
            "no-manifest",
            json!([{"path": "a.jpg", "md5": "1".repeat(32)}]),
        ),
    ] {
        let mut env = Env::new(&format!("rec-digests-{name}"));
        let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
        let id = env.seed_folder(Y, DAY, &set);
        // 止まっている間の書き換え（同じ名前・同じ大きさのまま、MD5 だけ変わった）
        {
            let mut d = env.drive();
            let p = d.resolve(&[Y.as_str(), DAY]).unwrap();
            let photo = d
                .children(&p)
                .iter()
                .find(|n| n.name == "090000_1.jpg")
                .unwrap()
                .id
                .clone();
            d.set_md5(&photo, &"f".repeat(32));
            env.save(&d);
        }
        let site = photo_site(DAY, DAY, three_photos());
        let svc = service(&mut env, &site);
        let path = publishing_record_with_digests(&env, 31, &id, &set, Some(bad));
        let record_before = std::fs::read(&path).unwrap();
        let drive_before = env.drive().to_json();

        let report = startup_recovery_retry(&svc);
        // 読めない記録は、復旧の対象にならず、次の書き出しを止める
        match report {
            StartupReport::Done { recovery, .. } => {
                assert_eq!(recovery.recovered, 0, "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
        assert!(
            matches!(svc.blocking(), Some(Blocking::UnreadableRecord(_))),
            "{name}: {:?}",
            svc.blocking()
        );
        assert!(
            matches!(
                submit_retry(&svc, &day()),
                Err(SubmitError::Blocked(Blocking::UnreadableRecord(_)))
            ),
            "{name}"
        );
        // 記録・公開の記録・Drive は変わらない
        assert_eq!(std::fs::read(&path).unwrap(), record_before, "{name}");
        assert!(!env.state.join("published").exists(), "{name}");
        assert_eq!(env.drive().to_json(), drive_before, "{name}");
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
        assert_eq!(svc.lookup(&format!("{:032x}", 31)), Lookup::Unreadable);
    }

    // 対照: キーが無い（MD5 を残す前の古い記録）は読め、復旧は進む（MD5 の無い公開の記録になり、次の replace は断られる）
    let mut env = Env::new("rec-digests-absent");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let id = env.seed_folder(Y, DAY, &set);
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    publishing_record_with_digests(&env, 32, &id, &set, None);
    match startup_recovery_retry(&svc) {
        StartupReport::Done { recovery, .. } => {
            assert_eq!((recovery.recovered, recovery.blocked), (1, false));
        }
        other => panic!("{other:?}"),
    }
    assert!(svc.blocking().is_none());
    let row = env
        .published
        .find(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap())
        .unwrap();
    assert_eq!(row.folder_id, id);
    assert_eq!(row.files, None);
}

#[test]
fn blocked_records_are_not_pruned_and_resolved_ones_free_the_service() {
    let mut env = Env::new("rec-resolved");
    let site = photo_site(DAY, DAY, vec![]);
    let svc = service(&mut env, &site);
    let store = JobStore::open(&env.state).unwrap();
    let mut b = JobRecord::new(JobId::parse(&format!("{:032x}", 8)).unwrap(), &day());
    settle(&mut b, Status::Blocked);
    b.started_ns = 1;
    b.blocked = Some(BlockedNote {
        reason: "unobservable".into(),
        description: "d".into(),
    });
    store.write(&b).unwrap();
    for _ in 0..3 {
        assert!(submit_retry(&svc, &day()).is_err());
    }
    b.resolved_at = Some("2030-01-01T00:00:00+09:00".into());
    store.write(&b).unwrap();
    assert!(svc.blocking().is_none());
    let rec = run_foreground_retry(&svc, &day()).unwrap();
    assert_eq!(rec.status, Status::Succeeded);
    // 解除済みの blocked は GET に resolved_at を出す
    let Lookup::Found(v) = svc.lookup(b.job_id.as_str()) else {
        panic!()
    };
    assert_eq!(v["status"], "blocked");
    assert_eq!(v["resolved_at"], "2030-01-01T00:00:00+09:00");
}

#[test]
fn an_incoming_folder_whose_id_was_never_recorded_is_left_and_reported_not_deleted() {
    let mut env = Env::new("rec-leftover");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let job = JobId::parse(&format!("{:032x}", 12)).unwrap();
    let inc = env.seed_set_at(&["geoframe-export-work", "incoming", job.as_str()], &set);
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    // ID を記録する前に止まった（uploading の最初の記録だけがある）
    running_record(
        &env,
        12,
        Some(Progress {
            stage: Stage::Uploading,
            old_id: None,
            incoming_id: None,
            digests: None,
        }),
    );
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    let rec = read_record(&env, &job);
    assert_eq!(rec.status, Status::Interrupted);
    assert!(
        rec.notes
            .iter()
            .any(|n| n.contains(&inc) && n.contains("incoming")),
        "{:?}",
        rec.notes
    );
    // 名前だけでは消さない
    assert!(drive_has(
        &env,
        &["geoframe-export-work", "incoming", job.as_str()]
    ));
    assert!(env.drive().trashed.is_empty());
    let Lookup::Found(v) = svc.lookup(job.as_str()) else {
        panic!()
    };
    assert_eq!(v["status"], "interrupted");
    assert!(v["notes"][0].as_str().unwrap().contains(&inc));
}

#[test]
fn startup_recovery_finishes_a_published_job_and_a_pending_trash_and_ends_an_aborting_one() {
    let site_rows = three_photos();

    // publishing（新規）で、宛先がもう incoming の ID のフォルダになっている → 公開済み。公開の記録に足す
    let mut env = Env::new("rec-published");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let inc = env.seed_folder(Y, DAY, &set);
    let site = photo_site(DAY, DAY, site_rows.clone());
    let svc = service(&mut env, &site);
    let rec = running_record(
        &env,
        20,
        Some(Progress {
            stage: Stage::Publishing,
            old_id: None,
            incoming_id: Some(inc.clone()),
            digests: None,
        }),
    );
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    let after = read_record(&env, &rec.job_id);
    assert_eq!(after.status, Status::Succeeded);
    assert_eq!(after.result.as_ref().unwrap()["uploaded"], true);
    assert_eq!(after.result.as_ref().unwrap()["replaced"], false);
    assert_eq!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap()),
        Some(inc)
    );
    assert!(env.drive().trashed.is_empty());

    // trashing で、旧フォルダがまだ replaced/ にある → ゴミ箱への移動をやり直して成功
    let mut env = Env::new("rec-trashing");
    let old_set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    let new_set = env.make_set("new", Y, DAY, &[("090000_1.jpg", 100)]);
    let job = JobId::parse(&format!("{:032x}", 21)).unwrap();
    let old_id = env.seed_managed(Y, DAY, &old_set);
    let layout = env.layout(Y, DAY, job.as_str());
    let mut d = env.drive();
    assert!(d.move_dir(
        &old_id,
        &[
            "geoframe-export-work".to_owned(),
            "replaced".to_owned(),
            layout.retired_name().to_owned()
        ]
    ));
    env.save(&d);
    let new_id = env.seed_folder(Y, DAY, &new_set);
    let site = photo_site(DAY, DAY, site_rows.clone());
    let svc = service(&mut env, &site);
    let mut rec = JobRecord::new(job.clone(), &request(DAY, None, Some("replace"), None));
    rec.progress = Some(Progress {
        stage: Stage::Trashing,
        old_id: Some(old_id.clone()),
        incoming_id: Some(new_id.clone()),
        digests: None,
    });
    JobStore::open(&env.state).unwrap().write(&rec).unwrap();
    assert!(matches!(
        startup_recovery_retry(&svc),
        StartupReport::Done { .. }
    ));
    let after = read_record(&env, &job);
    assert_eq!(after.status, Status::Succeeded, "{after:?}");
    assert_eq!(after.result.as_ref().unwrap()["replaced"], true);
    assert!(env.drive().trashed.contains(&old_id));
    assert_eq!(
        env.drive().resolve(&["yamareco", DAY]),
        Some(new_id.clone())
    );
    assert_eq!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap()),
        Some(new_id)
    );

    // aborting: incoming が残っていれば消し、もう無ければ片付け済みとして、どちらも interrupted
    for incoming_left in [true, false] {
        let mut env = Env::new(&format!("rec-aborting-{incoming_left}"));
        let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
        let job = JobId::parse(&format!("{:032x}", 22)).unwrap();
        let inc = if incoming_left {
            env.seed_set_at(&["geoframe-export-work", "incoming", job.as_str()], &set)
        } else {
            "fid-already-gone".to_owned()
        };
        let site = photo_site(DAY, DAY, site_rows.clone());
        let svc = service(&mut env, &site);
        running_record(
            &env,
            22,
            Some(Progress {
                stage: Stage::Aborting,
                old_id: None,
                incoming_id: Some(inc.clone()),
                digests: None,
            }),
        );
        assert!(matches!(
            startup_recovery_retry(&svc),
            StartupReport::Done { .. }
        ));
        assert_eq!(
            read_record(&env, &job).status,
            Status::Interrupted,
            "{incoming_left}"
        );
        assert_eq!(env.drive().trashed.contains(&inc), incoming_left);
        assert!(!drive_has(&env, &["yamareco", DAY]));
    }
}

/// `parent` の直下の `name` のファイルの MD5 を別の値にする（同じ名前・同じ大きさのまま中身を書き換えた、を作る）。
fn change_md5(env: &Env, parent: &[&str], name: &str) {
    let mut d = env.drive();
    let p = d.resolve(parent).expect("parent");
    let ids: Vec<String> = d
        .children(&p)
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(ids.len(), 1, "{name}");
    d.set_md5(&ids[0], &"f".repeat(32));
    env.save(&d);
}

#[test]
fn startup_recovery_checks_the_kept_md5_and_never_overwrites_the_published_record_with_the_drives_md5()
 {
    let folder = geoframe_export::folder::FolderName::parse(DAY).unwrap();
    let site_rows = three_photos();
    // (名前, 記録した段階, 公開の記録を新しい行に更新済みか, 書き換えるか)
    for (name, stage, row_updated, tamper) in [
        // 公開の記録を書く前に止まった（ジョブの記録は publishing）
        ("before-row", Stage::Publishing, false, true),
        // 公開の記録を更新した直後・ジョブの完了の前に止まった（ジョブの記録は trashing）
        ("after-row", Stage::Trashing, true, true),
        // 書き換えられていなければ、ジョブの記録の MD5 で公開の記録を書いて完了する
        ("clean", Stage::Publishing, false, false),
    ] {
        let mut env = Env::new(&format!("rec-md5-{name}"));
        let old_set = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
        let new_set = env.make_set(
            "new",
            Y,
            DAY,
            &[("090000_1.jpg", 100), ("090100_2.jpg", 200)],
        );
        let job = JobId::parse(&format!("{:032x}", 40)).unwrap();
        let old_id = env.seed_managed(Y, DAY, &old_set);
        let layout = env.layout(Y, DAY, job.as_str());
        let mut d = env.drive();
        assert!(d.move_dir(
            &old_id,
            &[
                "geoframe-export-work".to_owned(),
                "replaced".to_owned(),
                layout.retired_name().to_owned()
            ]
        ));
        env.save(&d);
        let new_id = env.seed_folder(Y, DAY, &new_set);
        if row_updated {
            env.published
                .upsert(geoframe_export::published::PublishedRecord {
                    destination: Y,
                    folder: folder.clone(),
                    folder_id: new_id.clone(),
                    job_id: job.clone(),
                    files: Some(new_set.recorded()),
                    previous: Some(Box::new(geoframe_export::published::PreviousFolder {
                        folder_id: old_id.clone(),
                        files: old_set.recorded(),
                    })),
                })
                .unwrap();
        }
        if tamper {
            change_md5(&env, &["yamareco", DAY], "090000_1.jpg");
        }
        let row_before = std::fs::read(env.published.path_of(Y, &folder)).unwrap();
        let site = photo_site(DAY, DAY, site_rows.clone());
        let svc = service(&mut env, &site);
        let mut rec = JobRecord::new(job.clone(), &request(DAY, None, Some("replace"), None));
        rec.progress = Some(Progress {
            stage,
            old_id: Some(old_id.clone()),
            incoming_id: Some(new_id.clone()),
            // ジョブの記録には、退避の前に検証済みの MD5 が書いてある
            digests: Some(new_set.recorded()),
        });
        JobStore::open(&env.state).unwrap().write(&rec).unwrap();
        assert!(matches!(
            startup_recovery_retry(&svc),
            StartupReport::Done { .. }
        ));
        let after = read_record(&env, &job);
        if tamper {
            // blocked で止め、公開の記録（旧フォルダの行、または新しい行）を書き換えず、旧フォルダもゴミ箱へ移さない
            assert_eq!(after.status, Status::Blocked, "{name}: {after:?}");
            assert_eq!(
                std::fs::read(env.published.path_of(Y, &folder)).unwrap(),
                row_before,
                "{name}"
            );
            assert!(env.drive().trashed.is_empty(), "{name}");
        } else {
            assert_eq!(after.status, Status::Succeeded, "{name}: {after:?}");
            let row = env.published.find(Y, &folder).unwrap();
            assert_eq!(row.folder_id, new_id, "{name}");
            assert_eq!(row.files, Some(new_set.recorded()), "{name}");
            assert!(env.drive().trashed.contains(&old_id), "{name}");
        }
    }
}

#[test]
fn a_skipped_startup_recovery_touches_neither_records_nor_staging() {
    let mut env = Env::new("rec-skipped");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    let rec = running_record(&env, 30, None);
    std::fs::create_dir_all(env.state.join("staging/in-use")).unwrap();
    std::fs::write(env.state.join("staging/in-use/a.jpg"), b"x").unwrap();
    let record_bytes =
        std::fs::read(env.state.join("jobs").join(format!("{}.json", rec.job_id))).unwrap();
    let Slot::Got(held) = lock::try_acquire(&env.state).unwrap() else {
        panic!("取れるはず")
    };
    assert_eq!(svc.startup_recovery(), StartupReport::Skipped);
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Running);
    assert_eq!(
        std::fs::read(env.state.join("jobs").join(format!("{}.json", rec.job_id))).unwrap(),
        record_bytes
    );
    assert!(env.state.join("staging/in-use/a.jpg").exists());
    assert!(env.calls().is_empty());
    drop(held);
}

#[test]
fn the_same_fixture_gives_the_same_manifest() {
    // 別々の状態ディレクトリ・偽の Drive で同じ要求を流すと、Drive に置かれる manifest.json の中身が同じ（AC11）
    let mut manifests = Vec::new();
    for name in ["same-a", "same-b"] {
        let mut env = Env::new(name);
        let site = photo_site(DAY, DAY, three_photos());
        let svc = service(&mut env, &site);
        let rec = run_foreground_retry(&svc, &day()).unwrap();
        assert_eq!(rec.status, Status::Succeeded);
        let d = env.drive();
        let dest = d.resolve(&["yamareco", DAY]).unwrap();
        let manifest = d
            .children(&dest)
            .into_iter()
            .find(|n| n.name == "manifest.json")
            .and_then(|n| n.content.clone())
            .expect("manifest.json の中身");
        manifests.push(manifest);
    }
    assert_eq!(manifests[0], manifests[1]);
    let m: Value = serde_json::from_str(&manifests[0]).unwrap();
    assert_eq!(m["count"], 3);
    assert_eq!(m["destination"], "yamareco");
    assert_eq!(m["folder"], DAY);
}

// ---- キャンセル（SIGINT・SIGTERM）・パニックの後の復旧・request_id の再照会・記録の整合・完了ログ（T5 のレビュー対応） ----

/// `cancel` を共有する `Service`（偽の写真サービスを向く）。
fn cancellable_service(env: &mut Env, site: &FakeSite, cancel: &Cancel) -> Service {
    env.settings.feed_base = site.base.clone();
    Service::with_cancel(env.settings.clone(), fast_tuning(), cancel.clone()).expect("service")
}

fn all_records(env: &Env) -> Vec<JobRecord> {
    JobStore::open(&env.state).unwrap().scan().records
}

/// 印が立つと、飛んでいる要求が終わるのを待って（要求の最中には見ない）、全スレッドを止めてから返る（実行枠は、そのあとで外れる）。
/// 記録は確定せず、`blocked` にも `failed` にもならない。次の復旧が、表のとおりに片付ける。
#[test]
fn a_cancel_stops_every_upload_thread_before_returning_and_hands_the_record_to_recovery() {
    let mut env = Env::new("flow-cancel");
    let site = photo_site(DAY, DAY, three_photos());
    let cancel = Cancel::new();
    let svc = cancellable_service(&mut env, &site, &cancel);
    // 写真を送る要求を、1.5 秒止める（要求の最中には印を見ないので、その要求が終わってから止まる）
    env.add_rule("upload", 0, 1, sleep_ms(1500));
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || run_foreground_retry(&svc, &day()))
    };
    wait_for_call(&env, "upload", 20);
    let t = std::time::Instant::now();
    cancel.cancel();
    let rec = worker.join().unwrap().unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "印を見て止まる: {:?}",
        t.elapsed()
    );
    // 返った時点で、全スレッドが止まっている（飛んでいる要求も、新しい要求も無い。次のジョブの復旧と競合しない）
    assert_quiet(&env, 300);
    assert!(slot_is_free(&env.state));
    assert_eq!(staging_entries(&env), 0);

    // 記録は running のまま（Drive の段階が記録にある）。blocked にも failed にもしない
    assert_eq!(rec.status, Status::Running);
    let on_disk = read_record(&env, &rec.job_id);
    assert_eq!(on_disk.status, Status::Running);
    assert_eq!(on_disk.error, None);
    let progress = on_disk.progress.expect("段階の記録");
    assert_eq!(progress.stage, Stage::Uploading);
    assert!(progress.incoming_id.is_some());
    assert!(svc.blocking().is_none());
    let incoming = progress.incoming_id.unwrap();
    assert!(
        drive_has(
            &env,
            &["geoframe-export-work", "incoming", rec.job_id.as_str()]
        ),
        "incoming は、復旧が片付けるまで残る"
    );
    assert!(!env.drive().trashed.contains(&incoming));

    // 次の起動の復旧（別の印）が、表のとおり incoming を片付けて interrupted にし、そのあとの書き出しが動く
    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    match startup_recovery_retry(&next) {
        StartupReport::Done { recovery, .. } => {
            assert_eq!((recovery.recovered, recovery.blocked), (1, false));
            assert!(!recovery.cancelled);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Interrupted);
    assert!(env.drive().trashed.contains(&incoming));
    let v = submit_and_wait(&next, &day());
    assert_eq!(v["status"], "succeeded", "{v}");
    // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
    env.assert_api_calls_safe();
}

/// 別のジョブの復旧の最中の印: 復旧の `rclone` を止め、その別のジョブを `blocked` にしない（`Unobservable` を確定しない）。
/// 復旧の待ちも眠り通さない。
#[test]
fn a_cancel_during_another_jobs_recovery_does_not_block_it() {
    let mut env = Env::new("rec-cancel");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let job = JobId::parse(&format!("{:032x}", 2)).unwrap();
    let inc = env.seed_set_at(&["geoframe-export-work", "incoming", job.as_str()], &set);
    let site = photo_site(DAY, DAY, three_photos());
    let cancel = Cancel::new();
    env.settings.feed_base = site.base.clone();
    // 待ちを長くして、印が待ちを切り上げることも確かめる
    let svc = Service::with_cancel(
        env.settings.clone(),
        Tuning {
            timing: geoframe_export::publish::Timing {
                settle: Duration::from_secs(30),
                rounds: 4,
            },
            ..fast_tuning()
        },
        cancel.clone(),
    )
    .unwrap();
    let rec = running_record(
        &env,
        2,
        Some(Progress {
            stage: Stage::Uploading,
            old_id: None,
            incoming_id: Some(inc.clone()),
            digests: None,
        }),
    );
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || startup_recovery_retry(&svc))
    };
    // 復旧が一覧を取る前の待ち（30 秒）の最中に、印を立てる
    std::thread::sleep(Duration::from_millis(300));
    let t = std::time::Instant::now();
    cancel.cancel();
    let report = worker.join().unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "待ちを眠り通さない: {:?}",
        t.elapsed()
    );
    match report {
        StartupReport::Done { recovery, .. } => {
            assert!(recovery.cancelled);
            assert!(!recovery.blocked);
        }
        other => panic!("{other:?}"),
    }
    let after = read_record(&env, &rec.job_id);
    assert_eq!(after.status, Status::Running, "確定しない");
    assert!(svc.blocking().is_none());
    assert!(!env.drive().trashed.contains(&inc), "何も片付けていない");
    assert!(slot_is_free(&env.state));

    // 印のない次の起動で、片付く
    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert!(matches!(
        startup_recovery_retry(&next),
        StartupReport::Done { .. }
    ));
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Interrupted);
}

/// 復旧が `incoming` をゴミ箱へ移している最中に印が立った: 止められた片付けは「手作業の片付けが要る残りもの」として
/// `interrupted` に確定しない（次の復旧なら自動で消せる）。記録は `running` のまま、`incoming` も残り、次の復旧で片付く。
#[test]
fn a_cancel_while_clearing_incoming_is_not_recorded_as_a_manual_leftover() {
    let mut env = Env::new("rec-cancel-purge");
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let job = JobId::parse(&format!("{:032x}", 2)).unwrap();
    let inc = env.seed_set_at(&["geoframe-export-work", "incoming", job.as_str()], &set);
    let site = photo_site(DAY, DAY, three_photos());
    let cancel = Cancel::new();
    let svc = cancellable_service(&mut env, &site, &cancel);
    let rec = running_record(
        &env,
        2,
        Some(Progress {
            stage: Stage::Uploading,
            old_id: None,
            incoming_id: Some(inc.clone()),
            digests: None,
        }),
    );
    // ゴミ箱への移動の要求を、1.5 秒止めておく（要求の最中には印を見ない。その要求は、印のあとで Drive に届いてしまう）
    env.add_rule("trash", 0, 1, sleep_ms(1500));
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || startup_recovery_retry(&svc))
    };
    wait_for_call(&env, "trash", 20);
    cancel.cancel();
    let report = worker.join().unwrap();
    assert_quiet(&env, 300);
    match report {
        StartupReport::Done { recovery, .. } => assert!(recovery.cancelled),
        other => panic!("{other:?}"),
    }
    let after = read_record(&env, &rec.job_id);
    assert_eq!(after.status, Status::Running, "interrupted に確定しない");
    assert!(after.notes.is_empty());
    // 飛んでいた要求は、印のあとで Drive に届く（結果不明。記録は確定しない）。次の復旧が、今の Drive の状態から続ける
    assert!(svc.blocking().is_none());

    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert!(matches!(
        startup_recovery_retry(&next),
        StartupReport::Done { .. }
    ));
    let done = read_record(&env, &rec.job_id);
    assert_eq!(done.status, Status::Interrupted);
    assert!(done.notes.is_empty(), "次の復旧が自動で片付ける");
    assert!(env.drive().trashed.contains(&inc));
}

/// 印が、Drive に触れる前に立っていれば、ジョブは `interrupted`（rclone は 1 回も呼ばない）。
#[test]
fn a_cancel_before_drive_is_touched_ends_the_job_as_interrupted() {
    let mut env = Env::new("flow-cancel-early");
    let site = photo_site(DAY, DAY, three_photos());
    let cancel = Cancel::new();
    let svc = cancellable_service(&mut env, &site, &cancel);
    cancel.cancel();
    let rec = run_foreground_retry(&svc, &day()).unwrap();
    assert_eq!(rec.status, Status::Interrupted, "{:?}", rec.error);
    assert_eq!(rec.error, None);
    assert!(rec.progress.is_none());
    assert!(env.calls().is_empty(), "Drive に触れない");
    assert_eq!(staging_entries(&env), 0);
    assert!(slot_is_free(&env.state));
    assert!(svc.blocking().is_none());
    // 次のジョブは、別の印で動く
    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    let v = submit_and_wait(&next, &day());
    assert_eq!(v["status"], "succeeded", "{v}");
}

/// 動いているジョブのスレッドの終わりを待てる（`serve` が、印を立てたあとに使う）。
#[test]
fn wait_idle_returns_when_the_job_thread_has_released_the_slot() {
    let mut env = Env::new("flow-idle");
    let site = photo_site(DAY, DAY, three_photos());
    let svc = service(&mut env, &site);
    assert!(svc.wait_idle(Duration::from_millis(10)), "何も動いていない");
    env.add_rule("upload", 0, 1, sleep_ms(600));
    let s = submit_retry(&svc, &day()).unwrap();
    assert!(!svc.wait_idle(Duration::from_millis(50)), "まだ動いている");
    assert!(svc.wait_idle(Duration::from_secs(30)));
    assert!(
        matches!(svc.lookup(s.job_id.as_str()), Lookup::Found(v) if v["status"] == "succeeded")
    );
    assert!(slot_is_free(&env.state));
}

// ---- パニックの後 ----

/// 旧フォルダを退避した直後（`publishing` を記録した直後）のパニックは、`failed` を確定せず、実行枠を持ったまま復旧の表を
/// 当てる。宛先が空のまま、復旧の対象から外れることがない（前へ進めて公開し、旧フォルダをゴミ箱へ）。
#[test]
fn a_panic_right_after_retiring_is_recovered_in_place_not_failed() {
    let mut env = Env::new("flow-panic-retired");
    let old = env.make_set("old", Y, DAY, &[("090000_9.jpg", 50)]);
    let old_id = env.seed_managed(Y, DAY, &old);
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_tuning(
        env.settings.clone(),
        Tuning {
            panic_after_stage: Some(Stage::Publishing),
            ..fast_tuning()
        },
    )
    .unwrap();
    let rec = run_foreground_retry(&svc, &request(DAY, None, Some("replace"), None)).unwrap();
    // 旧フォルダの退避は済んでいる（記録の段階は publishing）。パニックのあと、復旧の表で公開まで進んだ
    assert_eq!(
        rec.status,
        Status::Succeeded,
        "{:?} {:?}",
        rec.error,
        rec.notes
    );
    assert_eq!(rec.error, None);
    let new_id = env
        .drive()
        .resolve(&["yamareco", DAY])
        .expect("宛先が空でない");
    assert_ne!(new_id, old_id);
    for f in [
        "090000_1.jpg",
        "090100_2.jpg",
        "103000_3.jpg",
        "manifest.json",
    ] {
        assert!(drive_has(&env, &["yamareco", DAY, f]), "{f}");
    }
    assert!(
        env.drive().trashed.contains(&old_id),
        "旧フォルダはゴミ箱へ"
    );
    assert!(
        env.published
            .find_id(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap())
            == Some(new_id)
    );
    // 記録は終わりの状態（読める）。作業領域は消え、実行枠は外れている
    let on_disk = read_record(&env, &rec.job_id);
    assert_eq!(on_disk.status, Status::Succeeded);
    assert_eq!(
        on_disk.result.as_ref().map(|r| r["count"].clone()),
        Some(json!(3)),
        "数は、公開の前に記録した summary から"
    );
    // 実際の流れ（Recorder → jobs/<job_id>.json）が、検証済みの MD5（写真 3 枚と manifest.json）を段階の記録に残している。
    // それは公開の記録の MD5 と同じ（ファイルから読み直して確かめる）
    let kept = on_disk
        .progress
        .as_ref()
        .and_then(|p| p.digests.clone())
        .expect("ジョブの記録に検証済みの MD5");
    let mut paths: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "090000_1.jpg",
            "090100_2.jpg",
            "103000_3.jpg",
            "manifest.json"
        ]
    );
    let row = env
        .published
        .find(Y, &geoframe_export::folder::FolderName::parse(DAY).unwrap())
        .unwrap();
    assert_eq!(row.files, Some(kept));
    assert_eq!(staging_entries(&env), 0);
    assert!(slot_is_free(&env.state));
    assert!(svc.blocking().is_none());
    assert_eq!(all_records(&env).len(), 1);
}

/// 段階を記録した直後（新規の公開の前）のパニックも、`failed` を確定せず復旧の表を当てる: `incoming` を片付けて `interrupted`。
#[test]
fn a_panic_after_the_first_stage_is_settled_by_the_table_and_leaves_nothing_behind() {
    let mut env = Env::new("flow-panic-uploading");
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_tuning(
        env.settings.clone(),
        Tuning {
            panic_after_stage: Some(Stage::Verifying),
            ..fast_tuning()
        },
    )
    .unwrap();
    let rec = run_foreground_retry(&svc, &day()).unwrap();
    // 検証の段階まで進んだ（写真は送った）。宛先は前のまま（無い）なので、incoming を片付けて interrupted
    assert_eq!(rec.status, Status::Interrupted, "{:?}", rec.error);
    assert!(!drive_has(&env, &["yamareco", DAY]));
    assert!(!drive_has(
        &env,
        &["geoframe-export-work", "incoming", rec.job_id.as_str()]
    ));
    assert_eq!(staging_entries(&env), 0);
    assert!(slot_is_free(&env.state));
    assert!(svc.blocking().is_none());
}

/// パニックの時点で、ジョブの記録が読めない: メモリ上の段階で Drive を操作せず、壊れた記録を上書きもしない（`running` のまま／
/// 読めないまま残し、次の起動で `blocked` として扱われる）。
#[test]
fn a_panic_with_an_unreadable_record_touches_neither_drive_nor_the_record() {
    let mut env = Env::new("flow-panic-unreadable");
    let site = photo_site(DAY, DAY, three_photos());
    env.settings.feed_base = site.base.clone();
    let svc = Service::with_tuning(
        env.settings.clone(),
        Tuning {
            panic_after_stage: Some(Stage::Uploading),
            corrupt_record_on_panic: true,
            ..fast_tuning()
        },
    )
    .unwrap();
    let rec = run_foreground_retry(&svc, &day()).unwrap();
    let path = JobStore::open(&env.state).unwrap().record_path(&rec.job_id);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{broken",
        "壊れた記録を上書きしない"
    );
    // 段階を記録した直後（写真を送る前）のパニック。復旧の表を当てていれば、incoming のゴミ箱への移動（purge）などが走る
    let seq = env.ops();
    assert!(
        !seq.iter()
            .any(|c| c == "trash" || c == "move" || c == "upload"),
        "パニックの後に Drive を操作しない: {seq:?}"
    );
    assert!(slot_is_free(&env.state));
    // 次の起動では、読めない記録として blocked 扱い
    let next = Service::with_tuning(env.settings.clone(), fast_tuning()).unwrap();
    assert!(next.blocking().is_some());
}

// ---- 一覧の取得の前後のキャンセル ----

/// 印が立っているとき、一覧の取得の前に止まる。候補が 0 枚でも `succeeded` にしない（段階が未記録なので `interrupted`）。
#[test]
fn a_cancel_before_the_listing_is_interrupted_even_when_nothing_would_be_exported() {
    let mut env = Env::new("flow-cancel-before-list");
    let site = photo_site(DAY, DAY, vec![]);
    let cancel = Cancel::new();
    let svc = cancellable_service(&mut env, &site, &cancel);
    cancel.cancel();
    let rec = run_foreground_retry(&svc, &day()).unwrap();
    assert_eq!(rec.status, Status::Interrupted, "{:?}", rec.result);
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Interrupted);
    assert_eq!(site.connections(), 0, "一覧を取りに行かない");
    assert!(env.calls().is_empty());
    assert!(slot_is_free(&env.state));
}

/// 一覧の取得の最中に印が立ったら、返ってきた結果（候補 0 枚）を判定せずに `interrupted`。
#[test]
fn a_cancel_during_the_listing_is_interrupted_not_succeeded() {
    let mut env = Env::new("flow-cancel-during-list");
    let site = FakeSite::start(vec![(
        "/api/admin/export/photos".to_owned(),
        Reply::Delayed {
            wait: Duration::from_millis(800),
            then: Box::new(Reply::json(
                serde_json::to_vec(&listing(DAY, DAY, vec![])).unwrap(),
            )),
        },
    )]);
    let cancel = Cancel::new();
    let svc = cancellable_service(&mut env, &site, &cancel);
    let worker = {
        let svc = svc.clone();
        std::thread::spawn(move || run_foreground_retry(&svc, &day()))
    };
    std::thread::sleep(Duration::from_millis(300));
    cancel.cancel();
    let rec = worker.join().unwrap().unwrap();
    assert_eq!(rec.status, Status::Interrupted, "{:?}", rec.result);
    assert_eq!(read_record(&env, &rec.job_id).status, Status::Interrupted);
    assert!(env.calls().is_empty());
    assert!(slot_is_free(&env.state));
}

// ---- request_id の再照会（別のプロセスに相当する別の Service） ----

fn second_service(env: &Env) -> Service {
    Service::with_tuning(env.settings.clone(), fast_tuning()).expect("service")
}

/// 最初の照会のあと（実行枠を取る前）に、別のプロセスが同じ `request_id` を登録して終えた: 実行枠を取った直後の照会で見つけて、
/// 二重に登録しない。
#[test]
fn a_request_id_registered_by_another_instance_after_the_first_lookup_is_not_registered_twice() {
    let mut env = Env::new("flow-rid-after");
    let site = photo_site(DAY, DAY, three_photos());
    let a = service(&mut env, &site);
    let b = second_service(&env);
    let req = request(DAY, None, None, Some("req-x"));
    let other: std::cell::RefCell<Option<geoframe_export::service::Submitted>> = Default::default();
    let found = a
        .submit_with_hook(&req, &|| {
            let s = submit_retry(&b, &req).unwrap();
            wait_done(&b, &s.job_id);
            *other.borrow_mut() = Some(s);
        })
        .unwrap();
    let first = other.borrow().clone().unwrap();
    assert_eq!(
        found.job_id, first.job_id,
        "別のインスタンスが登録したジョブを返す"
    );
    assert!(found.reused);
    assert_eq!(found.status, Status::Succeeded);
    assert_eq!(job_files(&env), 1, "二重に登録しない");
    assert!(slot_is_free(&env.state), "取った実行枠は手放している");
}

/// その間に別のプロセスが登録して、まだ実行中（実行枠が取れない）: `Busy` ではなく、そのジョブを返す。
#[test]
fn a_request_id_registered_meanwhile_is_returned_instead_of_busy() {
    let mut env = Env::new("flow-rid-busy");
    let site = photo_site(DAY, DAY, three_photos());
    let a = service(&mut env, &site);
    let b = second_service(&env);
    env.add_rule("upload", 0, 1, sleep_ms(800));
    let req = request(DAY, None, None, Some("req-y"));
    let other: std::cell::RefCell<Option<geoframe_export::service::Submitted>> = Default::default();
    let found = a
        .submit_with_hook(&req, &|| {
            *other.borrow_mut() = Some(submit_retry(&b, &req).unwrap());
        })
        .unwrap();
    let first = other.borrow().clone().unwrap();
    assert_eq!(found.job_id, first.job_id);
    assert!(found.reused);
    assert_eq!(found.status, Status::Running);
    // 別の要求は、ふつうに Busy
    assert_eq!(
        a.submit(&request("2030-03-12", None, None, Some("req-z")))
            .unwrap_err(),
        SubmitError::Busy
    );
    assert_eq!(job_files(&env), 1);
    wait_done(&b, &first.job_id);
}

/// その間に別のプロセスが、同じ `request_id` で中身の違う要求を登録した: `request_id_mismatch`（Busy でも、新しい登録でもない）。
#[test]
fn a_request_id_registered_meanwhile_with_other_content_is_a_mismatch() {
    let mut env = Env::new("flow-rid-mismatch");
    let site = photo_site(DAY, DAY, three_photos());
    let a = service(&mut env, &site);
    let b = second_service(&env);
    let theirs = request(DAY, None, Some("replace"), Some("req-w"));
    let mine = request(DAY, None, None, Some("req-w"));
    // 実行枠が空いている場合
    let e = a
        .submit_with_hook(&mine, &|| {
            let s = submit_retry(&b, &theirs).unwrap();
            wait_done(&b, &s.job_id);
        })
        .unwrap_err();
    assert_eq!(e, SubmitError::RequestIdMismatch);
    assert_eq!(job_files(&env), 1);
    assert!(slot_is_free(&env.state));
    // 実行中で取れない場合
    let mut env2 = Env::new("flow-rid-mismatch2");
    let a2 = service(&mut env2, &site);
    let b2 = second_service(&env2);
    env2.add_rule("upload", 0, 1, sleep_ms(800));
    let other: std::cell::RefCell<Option<JobId>> = Default::default();
    let e = a2
        .submit_with_hook(&mine, &|| {
            *other.borrow_mut() = Some(submit_retry(&b2, &theirs).unwrap().job_id);
        })
        .unwrap_err();
    assert_eq!(e, SubmitError::RequestIdMismatch);
    wait_done(&b2, other.borrow().as_ref().unwrap());
}
