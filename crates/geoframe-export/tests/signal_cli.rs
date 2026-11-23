//! T5: SIGINT・SIGTERM（design.md 4.1 の手順 11、8 章「同時実行」）。本物のバイナリ（`geoframe-export run`）を起動して signal を送り、
//! 親が終わったとき、飛んでいる Drive の要求も、新しい要求も無いこと（アップロードのスレッドが全部止まって join された）、そのあとに
//! 次のプロセスが実行枠を取れること、記録が `blocked`・`failed` にならず次の復旧に渡されることを確かめる。偽の写真サービス
//! （127.0.0.1 の空きポート）と、偽の Drive の HTTP サーバー（`--drive-endpoint`）だけを使う
//! （`--feed-base` は必ず偽のサイトへ。本物の写真サービス 127.0.0.1:8080 にも、本物の Google にも接続しない）。
//! キャンセルは、要求の前・再試行の待ち・チャンクの間で見る（要求の最中には見ない）ので、飛んでいる要求が終わってから止まる。
//! その要求は、印のあとで Drive に届きうる（結果不明として、次の復旧が今の Drive の状態から続ける）。

mod common;

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::FakeSite;
use common::drive::*;
use common::site::*;
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
    Command::new(env!("CARGO_BIN_EXE_geoframe-export"))
        .args(["run", "--from", DAY, "--destination", "yamareco"])
        .args(["--feed-base", &format!("http://{}", site.addr)])
        .args(env.cli_settings_args())
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

/// 実行中の `run` に signal を送ると、飛んでいる要求が終わるのを待ち、アップロードのスレッドを全部止めて join してから終わる。実行枠はそのあとで外れ、
/// 記録は確定せず `running` のまま（Drive の段階が記録にある）。次の起動の復旧が、その記録を表のとおり片付けて、書き出しを進める。
#[test]
fn sigint_and_sigterm_stop_every_thread_before_the_parent_exits_and_leave_the_record_to_recovery() {
    for (name, sig) in [("SIGINT", libc::SIGINT), ("SIGTERM", libc::SIGTERM)] {
        let mut env = Env::new(&format!("sig-{}", name.to_lowercase()));
        let site = photo_site(DAY, DAY, three());
        // 写真を送る要求を、2 秒止める（要求の最中には印を見ないので、その要求が終わってから止まる）
        env.add_rule("upload", 0, 1, sleep_ms(2000));
        let child = spawn_run(&env, &site);
        let pid = child.id();
        wait_for_call(&env, "upload", 30);

        let sent = Instant::now();
        send(pid, sig);
        let out = wait_exit(child, 15);
        assert!(
            sent.elapsed() < Duration::from_secs(15),
            "{name}: 止まるのが遅い: {:?}",
            sent.elapsed()
        );

        // 親が終わった時点で、飛んでいる要求も新しい要求も無い（全スレッドが止まって join された）。そのあとで、次のプロセスが
        // 実行枠を取れる
        assert_quiet(&env, 300);
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
        // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
        env.assert_api_calls_safe();
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

/// 異常終了（SIGKILL。印を見られない終わり方）: 子プロセスは無く、実行枠（flock）はカーネルが外す。記録は running のまま次の復旧に渡る
/// （飛んでいた要求が Drive に届いたかどうかは分からない。次の復旧が、今の Drive の状態から表のとおり片付ける）。
#[test]
fn a_killed_parent_releases_the_slot_and_leaves_the_record_to_recovery() {
    let mut env = Env::new("sig-kill");
    let site = photo_site(DAY, DAY, three());
    env.add_rule("upload", 0, 1, sleep_ms(1500));
    let child = spawn_run(&env, &site);
    let pid = child.id();
    wait_for_call(&env, "upload", 30);
    send(pid, libc::SIGKILL);
    let out = wait_exit(child, 15);
    assert_eq!(out.code, None, "signal で死んだ");
    assert!(slot_is_free(&env.state));
    let scan = JobStore::open(&env.state).unwrap().scan();
    assert!(scan.blocking().is_none());
    assert_eq!(scan.records[0].status, Status::Running);
    // 飛んでいた要求が終わるのを待って、次の起動の復旧が片付け、書き出しが進む
    std::thread::sleep(Duration::from_millis(1800));
    let next = service(&mut env, &site);
    assert!(matches!(
        startup_recovery_retry(&next),
        geoframe_export::service::StartupReport::Done { .. }
    ));
    let scan = JobStore::open(&env.state).unwrap().scan();
    assert_eq!(scan.records[0].status, Status::Interrupted);
}

/// バイナリは、標準エラーに完了ログ（1 行）を出す。標準出力は、コマンドの JSON だけ（ログが混ざらない）。
#[test]
fn the_binary_logs_the_completion_to_stderr_and_keeps_stdout_for_the_json() {
    let env = Env::new("log-completion");
    let site = photo_site(DAY, DAY, three());
    let out = wait_exit(spawn_run(&env, &site), 60);
    assert_eq!(out.code, Some(0), "{}\n{}", out.stdout, out.stderr);
    let v: Value = serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("標準出力が JSON でない: {e}: {}", out.stdout));
    assert_eq!(v["status"], "succeeded", "{v}");
    let line = out
        .stderr
        .lines()
        .find(|l| l.contains("書き出しのジョブが終わりました"))
        .unwrap_or_else(|| panic!("完了ログが無い: {:?}", out.stderr));
    assert!(line.contains("status=\"succeeded\"") || line.contains("status=succeeded"));
    assert!(line.contains("count=3"), "{line}");
}
