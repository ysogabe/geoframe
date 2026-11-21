//! T5: SIGINT・SIGTERM（design.md 4.1 の手順 11、8 章「同時実行」）。本物のバイナリ（`geoframe-export run`）を起動して signal を送り、
//! 親が終わったとき、偽の rclone（子）がもう居ないこと、そのあとに次のプロセスが実行枠を取れること、記録が `blocked`・`failed` に
//! ならず次の復旧に渡されることを確かめる。偽の写真サービス（127.0.0.1 の空きポート）と偽の rclone・偽の Drive だけを使う
//! （`--feed-base` は必ず偽のサイトへ。本物の写真サービス 127.0.0.1:8080 には接続しない）。

mod common;

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::drive::*;
use common::site::*;
use common::{FakeSite, good_config, write_config};
use geoframe_export::jobs::{JobStore, Status};
use geoframe_export::recovery::Stage;
use serde_json::Value;

const DAY: &str = "2030-03-10";

fn three() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

/// `geoframe-export run`（本物のバイナリ）を起動する。
fn spawn_run(env: &Env, site: &FakeSite) -> Child {
    write_config(&env.state, good_config("gdrive").as_bytes());
    Command::new(env!("CARGO_BIN_EXE_geoframe-export"))
        .args(["run", "--from", DAY, "--destination", "yamareco"])
        .args(["--feed-base", &format!("http://{}", site.addr)])
        .args(["--rclone", env!("CARGO_BIN_EXE_fake-rclone")])
        .args([
            "--rclone-config",
            env.state.join("rclone/rclone.conf").to_str().unwrap(),
        ])
        .args(["--state-dir", env.state.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("起動")
}

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// 最大 `secs` 秒で終わるのを待つ（終わらなければ、その子だけを kill して失敗）。
fn wait_exit(mut child: Child, secs: u64) -> Finished {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{secs} 秒たっても終わりません（signal で止まらない）");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    Finished {
        code: status.code(),
        stdout,
        stderr,
    }
}

fn send(pid: u32, sig: libc::c_int) {
    // SAFETY: この試験が起動した子（`geoframe-export`）への kill(2) だけ。
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    assert_eq!(rc, 0, "signal を送れません");
}

/// 実行中の `run` に signal を送ると、実行中の rclone を止めて `wait` し終えてから終わる（子が残らない）。実行枠はそのあとで外れ、
/// 記録は確定せず `running` のまま（Drive の段階が記録にある）。次の起動の復旧が、その記録を表のとおり片付けて、書き出しを進める。
#[test]
fn sigint_and_sigterm_stop_the_child_before_the_parent_exits_and_leave_the_record_to_recovery() {
    for (name, sig) in [("SIGINT", libc::SIGINT), ("SIGTERM", libc::SIGTERM)] {
        let mut env = Env::new(&format!("sig-{}", name.to_lowercase()));
        let site = photo_site(DAY, DAY, three());
        // 写真を送る copy を、30 秒止める（signal で止まらなければ、30 秒かかる）
        env.add_rule("copy", 0, 1, sleep_ms(30_000));
        let child = spawn_run(&env, &site);
        let pid = child.id();
        let copy_pid = wait_for_call(&env, "copy", 30);
        assert!(process_exists(copy_pid), "{name}: copy が動いている");

        let sent = Instant::now();
        send(pid, sig);
        let out = wait_exit(child, 15);
        assert!(
            sent.elapsed() < Duration::from_secs(15),
            "{name}: 止まるのが遅い: {:?}",
            sent.elapsed()
        );

        // 親が終わった時点で、子（偽の rclone）はもう居ない。そのあとで、次のプロセスが実行枠を取れる
        assert!(
            !process_exists(copy_pid),
            "{name}: 親が終わったのに、rclone が残っている"
        );
        assert!(slot_is_free(&env.state), "{name}: 実行枠が外れない");

        // 終了コード 1。記録（running）を標準出力に、説明を標準エラーに出す
        assert_eq!(out.code, Some(1), "{name}: {} {}", out.stdout, out.stderr);
        let v: Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|e| panic!("{name}: JSON でない: {e}: {}", out.stdout));
        assert_eq!(v["status"], "running", "{name}: {v}");
        assert!(out.stderr.contains("中断"), "{name}: {}", out.stderr);

        // 記録: running のまま（blocked にも failed にもしない）。段階は uploading で、incoming の ID が記録されている
        let scan = JobStore::open(&env.state).unwrap().scan();
        assert!(scan.blocking().is_none(), "{name}");
        assert_eq!(scan.records.len(), 1, "{name}");
        let rec = &scan.records[0];
        assert_eq!(rec.status, Status::Running, "{name}");
        assert_eq!(rec.error, None, "{name}");
        let progress = rec.progress.clone().expect("段階の記録");
        assert_eq!(progress.stage, Stage::Uploading, "{name}");
        let incoming = progress.incoming_id.expect("incoming の ID");
        assert!(
            !env.drive().trashed.contains(&incoming),
            "{name}: 片付けは次の復旧が行う"
        );
        assert_eq!(staging_entries(&env), 0, "{name}: 作業領域は消える");

        // 次の起動の復旧（待ちを短くした Service）: 前のジョブを表のとおり復旧（interrupted。incoming はゴミ箱へ）し、
        // 続く書き出しが成功する
        let next = service(&mut env, &site);
        assert!(matches!(
            startup_recovery_retry(&next),
            geoframe_export::service::StartupReport::Done { .. }
        ));
        let scan = JobStore::open(&env.state).unwrap().scan();
        assert_eq!(scan.records[0].status, Status::Interrupted, "{name}");
        assert!(env.drive().trashed.contains(&incoming), "{name}");
        let req = request(DAY, None, None, None);
        let s = submit_retry(&next, &req).unwrap();
        let v = wait_done(&next, &s.job_id);
        assert_eq!(v["status"], "succeeded", "{name}: {v}");
        assert_safe_calls(&env);
    }
}

/// signal を受けていない通常の終わりは、これまでどおり（終了コード 0、子は残らない）。handler を入れても、`run` が壊れない。
#[test]
fn a_run_without_a_signal_still_finishes_normally_with_the_handlers_installed() {
    let env = Env::new("sig-none");
    let site = photo_site(DAY, DAY, three());
    let out = wait_exit(spawn_run(&env, &site), 60);
    assert_eq!(out.code, Some(0), "{} {}", out.stdout, out.stderr);
    let v: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["status"], "succeeded");
    assert!(slot_is_free(&env.state));
}

/// 異常終了（SIGKILL。印を見られない終わり方）の補助: 子に `PR_SET_PDEATHSIG`（SIGKILL）が付いているので、親が死ぬと子も止まる。
/// これは排他の保証ではない（親が死んでから子が止まるまでに隙がある）。排他は、signal を印にして、子を止めてから枠を外す側で守る。
#[test]
fn a_killed_parent_takes_its_rclone_child_with_it_as_a_backstop() {
    let env = Env::new("sig-kill");
    let site = photo_site(DAY, DAY, three());
    env.add_rule("copy", 0, 1, sleep_ms(30_000));
    let child = spawn_run(&env, &site);
    let pid = child.id();
    let copy_pid = wait_for_call(&env, "copy", 30);
    assert!(process_exists(copy_pid));
    send(pid, libc::SIGKILL);
    let _ = wait_exit(child, 15);
    // 親は印も見られずに死んだ。子は、カーネルが SIGKILL で止める（数 ms。init が回収するまで、短い間ゾンビで残りうる）
    let until = Instant::now() + Duration::from_secs(5);
    while process_exists(copy_pid) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_exists(copy_pid),
        "親が死んでも、rclone が残っている"
    );
    assert!(slot_is_free(&env.state));
}
