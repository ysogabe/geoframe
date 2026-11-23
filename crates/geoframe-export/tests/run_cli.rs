//! T5: `run`・`recover` コマンドの JSON と終了コード（design.md 6.2。AC8 の CLI 側・AC11）。`geoframe_export::app::run_command` を
//! 直接呼ぶ（バイナリを起動しない）。偽の写真サービス（127.0.0.1）と偽の rclone・偽の Drive だけを使う。
//! 本物の写真サービス（127.0.0.1:8080）・rclone・Drive には触れない。

mod common;

use common::drive::*;
use common::site::*;
use common::{FakeSite, good_config, write_config};
use geoframe_export::app::{EXIT_USAGE, main_with, run_command};
use geoframe_export::jobid::JobId;
use geoframe_export::jobs::{BlockedNote, JobRecord, JobStore, ReadOutcome, Status};
use geoframe_export::lock::{self, Slot};
use geoframe_export::recovery::{Progress, Stage};
use geoframe_export::request::Destination;
use serde_json::Value;

const Y: Destination = Destination::Yamareco;
const DAY: &str = "2030-03-10";

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("JSON でない: {e}: {}", self.stdout))
    }
}

fn run_args(args: &[String], euid: Option<u32>) -> Out {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_command(euid, args, &mut out, &mut err);
    Out {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

/// 設定の引数（偽の rclone・専用の設定・状態ディレクトリ・偽の写真サービス）。
fn common_args(env: &Env, feed_base: &str) -> Vec<String> {
    write_config(&env.state, good_config("gdrive").as_bytes());
    [
        "--feed-base",
        feed_base,
        "--rclone",
        env!("CARGO_BIN_EXE_fake-rclone"),
        "--rclone-config",
        env.state.join("rclone/rclone.conf").to_str().unwrap(),
        "--state-dir",
        env.state.to_str().unwrap(),
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn command(head: &[&str], env: &Env, site: &FakeSite) -> Vec<String> {
    let mut v: Vec<String> = head.iter().map(|s| (*s).to_owned()).collect();
    v.extend(common_args(env, &format!("http://{}", site.addr)));
    v
}

fn run_cmd(head: &[&str], env: &Env, site: &FakeSite) -> Out {
    run_args(&command(head, env, site), None)
}

const SEC: i64 = 1_000_000_000;
const RUN: &[&str] = &["run", "--from", DAY, "--destination", "yamareco"];

fn three() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

/// 実行枠が取れずに終了コード 4 になったら、少し待ってやり直す（`common::site` の理由。ほかの試験の fork が、手放した枠の
/// fd を一瞬持つことがある）。「実行中」の試験では使わない。
fn run_free(head: &[&str], env: &Env, site: &FakeSite) -> Out {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let out = run_cmd(head, env, site);
        if out.code != 4 || std::time::Instant::now() >= deadline {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// ---- run ----

#[test]
fn a_successful_run_prints_the_get_shape_and_exits_0_then_conflicts_the_second_time() {
    let env = Env::new("cli-ok");
    let site = photo_site(DAY, DAY, three());
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["result"]["count"], 3);
    assert_eq!(v["result"]["uploaded"], true);
    assert_eq!(v["error"], Value::Null);
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "job_id",
            "status",
            "request",
            "started_at",
            "finished_at",
            "result",
            "error"
        ]
    );
    // 同じ記録が、GET の側（Service::lookup）と同じ形で引ける
    let id = v["job_id"].as_str().unwrap();
    let svc = geoframe_export::service::Service::new(env.settings.clone()).unwrap();
    assert_eq!(
        svc.lookup(id),
        geoframe_export::service::Lookup::Found(v.clone())
    );
    assert!(
        common::drive::path_of(&env, &env.drive().resolve(&["yamareco", DAY]).unwrap()).is_some()
    );
    assert!(out.stderr.is_empty(), "{}", out.stderr);

    // 2 回目は、宛先が既にあるので conflict（終了コード 3）
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 3, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["status"], "conflict");
    assert_eq!(v["result"]["conflict"], "exists");
    assert_eq!(v["result"]["uploaded"], false);
}

#[test]
fn zero_photos_exit_0_without_calling_rclone() {
    let env = Env::new("cli-zero");
    let site = photo_site(DAY, DAY, vec![]);
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["status"], "succeeded");
    assert_eq!(
        (
            v["result"]["count"].as_u64(),
            v["result"]["uploaded"].as_bool()
        ),
        (Some(0), Some(false))
    );
    assert!(env.calls().is_empty());
}

#[test]
fn failures_exit_1_with_the_error_in_the_json() {
    // 写真サービスに届かない
    let env = Env::new("cli-fail");
    let down = common::closed_port_base();
    let args = {
        let mut v: Vec<String> = RUN.iter().map(|s| (*s).to_owned()).collect();
        v.extend(common_args(&env, down.as_str()));
        v
    };
    let out = run_args(&args, None);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "feed_unavailable");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );

    // 1 枚の失敗（画像が無い）
    let env = Env::new("cli-fail2");
    let site = site_without_images(DAY, DAY, three());
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 1);
    assert_eq!(out.json()["error"]["code"], "fetch_failed");
    assert!(env.calls().is_empty());
}

#[test]
fn a_held_slot_exits_4_for_run_dry_run_and_recover() {
    let env = Env::new("cli-busy");
    let site = photo_site(DAY, DAY, three());
    let Slot::Got(_held) = lock::try_acquire(&env.state).unwrap() else {
        panic!("取れるはず")
    };
    for head in [
        RUN.to_vec(),
        [RUN, &["--dry-run"]].concat(),
        vec!["recover"],
        vec!["recover", "--resolve", "00000000000000000000000000000001"],
    ] {
        let out = run_cmd(&head, &env, &site);
        assert_eq!(out.code, 4, "{head:?}: {}", out.stderr);
    }
    let out = run_cmd(RUN, &env, &site);
    let v = out.json();
    assert_eq!(v["job_id"], Value::Null);
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "busy");
    assert!(env.calls().is_empty());
    assert_eq!(
        std::fs::read_dir(env.state.join("jobs"))
            .map(|d| d.count())
            .unwrap_or(0),
        0,
        "断ったものの記録は作らない"
    );
}

#[test]
fn dry_run_prints_the_200_shape_and_exits_0() {
    let env = Env::new("cli-dry");
    let site = photo_site(DAY, DAY, three());
    let out = run_free(&[RUN, &["--dry-run"]].concat(), &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["folder"], DAY);
    assert_eq!(v["candidate_count"], 3);
    assert_eq!(v["target"], "absent");
    assert_eq!(v["managed"], Value::Null);
    assert_eq!(v["derivative_pending"], 0);
    assert_eq!(v["excluded"]["not_derivative"], 0);
    assert!(v.get("status").is_none());
    for c in env.ops_used() {
        assert!(c == "list" || c == "read", "{c}");
    }
    // dry_run の失敗は、失敗の JSON と終了コード 1
    let env = Env::new("cli-dry2");
    let args = {
        let mut v: Vec<String> = RUN.iter().map(|s| (*s).to_owned()).collect();
        v.push("--dry-run".into());
        v.extend(common_args(&env, common::closed_port_base().as_str()));
        v
    };
    let out = run_args(&args, None);
    assert_eq!(out.code, 1);
    assert_eq!(out.json()["error"]["code"], "feed_unavailable");
}

#[test]
fn a_blocked_record_makes_run_exit_1_and_explain_on_stderr() {
    let env = Env::new("cli-blocked");
    let site = photo_site(DAY, DAY, three());
    let store = JobStore::open(&env.state).unwrap();
    let mut b = JobRecord::new(job(1), &common::site::day());
    settle(&mut b, Status::Blocked);
    b.blocked = Some(BlockedNote {
        reason: "stage_inconsistent".into(),
        description: "説明".into(),
    });
    store.write(&b).unwrap();
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 1);
    let v = out.json();
    assert_eq!(v["job_id"], Value::Null);
    assert_eq!(v["error"]["code"], "blocked");
    assert!(out.stderr.contains("recover --resolve"), "{}", out.stderr);
    assert!(out.stderr.contains(job(1).as_str()));
    assert!(env.calls().is_empty());
}

fn job(n: u32) -> JobId {
    JobId::parse(&format!("{n:032x}")).unwrap()
}

#[test]
fn usage_errors_exit_2_and_root_is_refused() {
    let env = Env::new("cli-usage");
    let site = photo_site(DAY, DAY, three());
    let mut bad = command(RUN, &env, &site);
    bad.push("--unknown".into());
    assert_eq!(
        main_with(&bad, &mut Vec::new(), &mut Vec::new()),
        EXIT_USAGE
    );

    // root で直接動かされたら、run・serve・recover は何もせず 2（試験は実効ユーザー ID を渡して確かめる）
    for head in [
        RUN.to_vec(),
        [RUN, &["--dry-run"]].concat(),
        vec!["recover"],
        vec![
            "serve",
            "--listen",
            "127.0.0.1:9100",
            "--token-file",
            "/nonexistent/token",
            "--allow",
            "192.0.2.1",
        ],
    ] {
        let out = run_args(&command(&head, &env, &site), Some(0));
        assert_eq!(out.code, EXIT_USAGE, "{head:?}");
        assert!(out.stderr.contains("root"), "{}", out.stderr);
        assert!(out.stdout.is_empty());
    }
    assert!(env.calls().is_empty());
    assert!(
        !env.state.join("jobs").exists(),
        "root の拒否は何も作らない"
    );
    // 専用のユーザー（root でない）なら動く。--help・--version は root でも動く
    assert_eq!(run_args(&["--help".to_owned()], Some(0)).code, 0);
    assert_eq!(run_args(&["--version".to_owned()], Some(0)).code, 0);
    assert_eq!(
        run_args(&command(&["recover"], &env, &site), Some(1000)).code,
        0
    );
}

// ---- recover ----

#[test]
fn recover_without_records_says_so_and_changes_nothing() {
    let env = Env::new("cli-rec-empty");
    let site = photo_site(DAY, DAY, three());
    let out = run_free(&["recover"], &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("ありません"), "{}", out.stdout);
    assert!(env.calls().is_empty());
}

#[test]
fn recover_shows_unfinished_and_blocked_jobs_without_changing_drive_or_records() {
    let env = Env::new("cli-rec-show");
    let site = photo_site(DAY, DAY, three());
    let set = env.make_set("s", Y, DAY, &[("090000_1.jpg", 100)]);
    let inc = env.seed_set_at(&["geoframe-export-work", "incoming", job(2).as_str()], &set);
    let store = JobStore::open(&env.state).unwrap();
    // 未完了（uploading。incoming は記録済み）、Drive に触れる前、blocked、終わったもの
    let mut running = JobRecord::new(job(2), &common::site::day());
    running.started_ns -= 30 * SEC;
    running.progress = Some(Progress {
        stage: Stage::Uploading,
        old_id: None,
        incoming_id: Some(inc.clone()),
        digests: None,
    });
    store.write(&running).unwrap();
    let mut early = JobRecord::new(job(3), &common::site::day());
    early.started_ns -= 20 * SEC;
    store.write(&early).unwrap();
    let mut blocked = JobRecord::new(job(4), &common::site::day());
    blocked.started_ns -= 10 * SEC;
    settle(&mut blocked, Status::Blocked);
    blocked.progress = Some(Progress {
        stage: Stage::Publishing,
        old_id: None,
        incoming_id: Some("fid-x".into()),
        digests: None,
    });
    blocked.blocked = Some(BlockedNote {
        reason: "stage_inconsistent".into(),
        description: "止めたときの説明の本文".into(),
    });
    store.write(&blocked).unwrap();
    let mut done = JobRecord::new(job(5), &common::site::day());
    settle(&mut done, Status::Succeeded);
    store.write(&done).unwrap();
    std::fs::write(
        env.state.join("jobs").join(format!("{}.json", job(6))),
        b"{broken",
    )
    .unwrap();

    let drive_before = env.drive().to_json();
    let records_before: Vec<_> = (2..=6)
        .map(|n| std::fs::read(env.state.join("jobs").join(format!("{}.json", job(n)))).unwrap())
        .collect();
    let out = run_free(&["recover"], &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = &out.stdout;
    assert!(
        text.contains(job(2).as_str()) && text.contains("uploading"),
        "{text}"
    );
    assert!(
        text.contains("宛先は前のまま"),
        "表のとおりの動きを出す: {text}"
    );
    assert!(
        text.contains(job(3).as_str()) && text.contains("Drive に触れる前"),
        "{text}"
    );
    assert!(
        text.contains(job(4).as_str()) && text.contains("止めたときの説明の本文"),
        "{text}"
    );
    assert!(text.contains("recover --resolve"), "{text}");
    assert!(
        !text.contains(job(5).as_str()),
        "終わったジョブは出さない: {text}"
    );
    assert!(
        text.contains(&format!("{}.json", job(6))) && text.contains("手で退かして"),
        "{text}"
    );
    // 古いほうから
    assert!(text.find(job(2).as_str()) < text.find(job(4).as_str()));

    // Drive も記録も変えない。呼んだ rclone は lsjson（と cat）だけ
    assert_eq!(env.drive().to_json(), drive_before);
    let records_after: Vec<_> = (2..=6)
        .map(|n| std::fs::read(env.state.join("jobs").join(format!("{}.json", job(n)))).unwrap())
        .collect();
    assert_eq!(records_after, records_before);
    for c in env.ops_used() {
        assert!(c == "list" || c == "read", "{c}");
    }
    assert!(env.drive().trashed.is_empty());
}

#[test]
fn resolve_clears_only_a_blocked_job_without_touching_drive() {
    let env = Env::new("cli-resolve");
    let site = photo_site(DAY, DAY, three());
    let store = JobStore::open(&env.state).unwrap();
    let mut blocked = JobRecord::new(job(7), &common::site::day());
    settle(&mut blocked, Status::Blocked);
    blocked.progress = Some(Progress {
        stage: Stage::Retiring,
        old_id: Some("fid-old".into()),
        incoming_id: Some("fid-inc".into()),
        digests: None,
    });
    blocked.blocked = Some(BlockedNote {
        reason: "retire_mismatch".into(),
        description: "本文".into(),
    });
    store.write(&blocked).unwrap();
    let mut running = JobRecord::new(job(8), &common::site::day());
    settle(&mut running, Status::Running);
    store.write(&running).unwrap();
    let mut done = JobRecord::new(job(9), &common::site::day());
    settle(&mut done, Status::Failed);
    store.write(&done).unwrap();
    let drive_before = env.drive().to_json();

    // blocked でないジョブ・無いジョブ・形の違う ID は断る
    for (id, code) in [
        (job(8).to_string(), 1),
        (job(9).to_string(), 1),
        (job(10).to_string(), 1),
        ("abc-123".to_owned(), 2),
    ] {
        let out = run_free(&["recover", "--resolve", &id], &env, &site);
        assert_eq!(out.code, code, "{id}: {}", out.stderr);
        assert!(out.stdout.is_empty(), "{id}");
    }
    assert_eq!(read(&store, 8).status, Status::Running);

    // blocked は解除できる。解除の日時が記録に残り、パスと ID が出る。Drive には触れない
    let out = run_free(&["recover", "--resolve", job(7).as_str()], &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let rec = read(&store, 7);
    assert_eq!(
        rec.status,
        Status::Blocked,
        "status は blocked のまま、解除の日時だけを足す"
    );
    let at = rec.resolved_at.clone().expect("解除の日時");
    assert_eq!(at.len(), 25, "{at}");
    assert!(out.stdout.contains(&at));
    assert!(
        out.stdout.contains("incoming/") && out.stdout.contains("fid-inc"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("replaced/") && out.stdout.contains("fid-old"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("本文"), "{}", out.stdout);
    assert!(env.calls().is_empty(), "Drive には触れない");
    assert_eq!(env.drive().to_json(), drive_before);

    // 2 回目は断る。解除したあとは、書き出しが動く（未完了の記録は、次の書き出しの前の復旧が片付ける）
    let again = run_free(&["recover", "--resolve", job(7).as_str()], &env, &site);
    assert_eq!(again.code, 1);
    assert_eq!(read(&store, 7).resolved_at, Some(at));
    let out = run_free(RUN, &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(read(&store, 8).status, Status::Interrupted);
}

fn read(store: &JobStore, n: u32) -> JobRecord {
    match store.read(&job(n)) {
        ReadOutcome::Found(r) => *r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unreadable_record_cannot_be_resolved() {
    let env = Env::new("cli-resolve-bad");
    let site = photo_site(DAY, DAY, three());
    let store = JobStore::open(&env.state).unwrap();
    let path = env.state.join("jobs").join(format!("{}.json", job(11)));
    std::fs::write(&path, b"{").unwrap();
    let _ = store;
    let out = run_free(&["recover", "--resolve", job(11).as_str()], &env, &site);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("手で退かして"), "{}", out.stderr);
    assert_eq!(std::fs::read(&path).unwrap(), b"{");
}

// ---- 完了済みでも、手作業の片付けが要る知らせ（T5 のレビュー対応） ----

#[test]
fn recover_lists_finished_jobs_whose_notes_ask_for_manual_cleanup_without_touching_anything() {
    let env = Env::new("cli-notes");
    let site = photo_site(DAY, DAY, three());
    let store = JobStore::open(&env.state).unwrap();
    // 失敗・中断・成功のそれぞれで、知らせが残っている記録と、残っていない記録
    let mut failed = JobRecord::new(job(21), &common::site::day());
    failed.started_ns -= 40 * SEC;
    settle(&mut failed, Status::Failed);
    failed.notes =
        vec!["作業用のフォルダに incoming/AAA が残っています（手で片付けてください）".into()];
    store.write(&failed).unwrap();
    let mut interrupted = JobRecord::new(job(22), &common::site::day());
    interrupted.started_ns -= 30 * SEC;
    settle(&mut interrupted, Status::Interrupted);
    interrupted.notes = vec!["旧フォルダが replaced/ に残っています".into()];
    store.write(&interrupted).unwrap();
    let mut succeeded = JobRecord::new(job(23), &common::site::day());
    succeeded.started_ns -= 20 * SEC;
    settle(&mut succeeded, Status::Succeeded);
    succeeded.notes = vec!["公開の記録 published/ に書けていません".into()];
    store.write(&succeeded).unwrap();
    let mut quiet = JobRecord::new(job(24), &common::site::day());
    quiet.started_ns -= 10 * SEC;
    settle(&mut quiet, Status::Succeeded);
    store.write(&quiet).unwrap();

    let records_before: Vec<_> = (21..=24)
        .map(|n| std::fs::read(env.state.join("jobs").join(format!("{}.json", job(n)))).unwrap())
        .collect();
    let out = run_free(&["recover"], &env, &site);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = &out.stdout;
    for n in [21, 22, 23] {
        assert!(text.contains(job(n).as_str()), "{n}: {text}");
    }
    assert!(
        !text.contains(job(24).as_str()),
        "知らせの無い完了済みは出さない: {text}"
    );
    for note in [
        "incoming/AAA が残っています",
        "replaced/ に残っています",
        "published/ に書けていません",
    ] {
        assert!(
            text.contains("知らせ: ") && text.contains(note),
            "{note}: {text}"
        );
    }
    assert!(text.contains("解除の対象ではありません"), "{text}");
    // 古いほうから
    assert!(text.find(job(21).as_str()) < text.find(job(23).as_str()));
    // Drive にも記録にも触れない。解除の対象にもならない
    assert!(env.calls().is_empty(), "Drive は照会しない");
    let records_after: Vec<_> = (21..=24)
        .map(|n| std::fs::read(env.state.join("jobs").join(format!("{}.json", job(n)))).unwrap())
        .collect();
    assert_eq!(records_after, records_before);
    let resolve = run_free(&["recover", "--resolve", job(21).as_str()], &env, &site);
    assert_eq!(resolve.code, 1, "{}", resolve.stdout);
    assert!(
        resolve.stderr.contains("blocked ではない"),
        "{}",
        resolve.stderr
    );

    // 知らせが 1 つも無ければ、「ありません」
    let env2 = Env::new("cli-notes-none");
    let site2 = photo_site(DAY, DAY, three());
    let out = run_free(&["recover"], &env2, &site2);
    assert_eq!(out.code, 0);
    assert!(
        out.stdout.contains("知らせが残っているジョブもありません"),
        "{}",
        out.stdout
    );
}
