//! T6: `geoframe-export serve`（本物のバイナリ）。起動の検査（弱いトークン・`--listen`）と、SIGTERM・SIGINT での終わり方
//! （実行中のジョブの rclone を止めてから終わる。AC9 の SIGINT・SIGTERM の行の HTTP 側）。偽の写真サービス（127.0.0.1 の空きポート）・
//! 偽の rclone・偽の Drive だけを使う（`--feed-base` は必ず偽のサイトへ。本物の写真サービス 127.0.0.1:8080 には接続しない。
//! `serve` の待ち受けも空きポート）。

mod common;

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::symlink;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::drive::*;
use common::http::*;
use common::site::*;
use common::{FakeSite, good_config, set_mode, write_config};
use geoframe_export::jobs::{JobStore, Status};
use serde_json::{Value, json};

const DAY: &str = "2030-03-10";
const TOKEN: &str = "CLI-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0YaE";

fn three() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

/// 使われていない 127.0.0.1 のポート（取って、すぐ手放す）。
fn free_addr() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr")
}

fn token_file(env: &Env, name: &str, content: &[u8], mode: u32) -> std::path::PathBuf {
    let path = env.root.path().join(name);
    std::fs::write(&path, content).expect("token");
    set_mode(&path, mode);
    path
}

/// `geoframe-export run`（別のプロセスの CLI）。偽の rclone・偽の写真サービス・同じ状態ディレクトリ。
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

/// 試験が失敗しても、起動した子（と、その偽の rclone）を残さない。
struct Reaper(Vec<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for c in &mut self.0 {
            if matches!(c.try_wait(), Ok(None)) {
                send(c.id(), libc::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs(15);
                while matches!(c.try_wait(), Ok(None)) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

fn spawn_serve(env: &Env, site: &FakeSite, listen: &str, token: &std::path::Path) -> Child {
    write_config(&env.state, good_config("gdrive").as_bytes());
    Command::new(env!("CARGO_BIN_EXE_geoframe-export"))
        .args(["serve", "--listen", listen])
        .args(["--token-file", token.to_str().unwrap()])
        .args(["--allow", "127.0.0.1"])
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

fn wait_exit(mut child: Child, secs: u64) -> Finished {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{secs} 秒たっても終わりません");
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
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, sig) }, 0);
}

/// `/healthz` が答えるまで待つ（最大 30 秒。その間に終わってしまったら失敗）。
fn wait_ready(child: &mut Child, addr: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            panic!("起動の途中で終わりました: {st}");
        }
        if let Ok(s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            drop(s);
            if http_request(addr, "GET", "/healthz", &[], None).status == 200 {
                return;
            }
        }
        assert!(Instant::now() < deadline, "待ち受けを始めません");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn nothing_listens(addr: SocketAddr) -> bool {
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err()
}

fn expect_refused(env: &Env, site: &FakeSite, listen: &str, token: &std::path::Path, label: &str) {
    let child = spawn_serve(env, site, listen, token);
    let out = wait_exit(child, 20);
    assert_eq!(out.code, Some(2), "{label}: {} {}", out.stdout, out.stderr);
    // エラーにトークンの値もパスも出ない
    for text in [&out.stdout, &out.stderr] {
        assert!(
            !text.contains(TOKEN) && !text.contains(&TOKEN[..12]),
            "{label}: {text}"
        );
        assert!(
            !text.contains(token.to_str().unwrap()),
            "{label}: パスが出ている: {text}"
        );
        assert!(
            !text.contains(env.root.path().to_str().unwrap()),
            "{label}: {text}"
        );
    }
    assert!(out.stdout.is_empty(), "{label}");
    assert!(env.calls().is_empty(), "{label}: rclone に触れた");
    assert!(
        !env.state.join("jobs").exists(),
        "{label}: 検査に通らないのに状態を作った"
    );
}

#[test]
fn serve_refuses_to_start_with_a_weak_token_file_or_a_wildcard_listen() {
    let env = Env::new("cli-refuse");
    let site = photo_site(DAY, DAY, three());
    let listen = free_addr().to_string();

    let short = token_file(&env, "short", b"too-short-token\n", 0o600);
    expect_refused(&env, &site, &listen, &short, "短い");
    let open = token_file(&env, "open", TOKEN.as_bytes(), 0o644);
    expect_refused(&env, &site, &listen, &open, "0644");
    let group = token_file(&env, "group", TOKEN.as_bytes(), 0o640);
    expect_refused(&env, &site, &listen, &group, "0640");
    let real = token_file(&env, "real", TOKEN.as_bytes(), 0o600);
    let link = env.root.path().join("link");
    symlink(&real, &link).unwrap();
    expect_refused(&env, &site, &listen, &link, "シンボリックリンク");
    expect_refused(
        &env,
        &site,
        &listen,
        &env.root.path().join("missing"),
        "無い",
    );
    let weak = token_file(&env, "weak", "z".repeat(64).as_bytes(), 0o600);
    expect_refused(&env, &site, &listen, &weak, "単調");

    // 0.0.0.0・::・ポート 0、tailnet でもループバックでもないアドレス（LAN の私的アドレス・文書用の予約アドレス・
    // 帯のすぐ外）は、トークンが良くても起動しない
    let port = free_addr().port();
    for bad in [
        format!("0.0.0.0:{port}"),
        format!("[::]:{port}"),
        "127.0.0.1:0".to_owned(),
        format!("192.168.1.10:{port}"),
        format!("10.0.0.5:{port}"),
        format!("172.16.0.1:{port}"),
        format!("192.0.2.1:{port}"),
        format!("100.63.255.255:{port}"),
        format!("100.128.0.1:{port}"),
        format!("[2001:db8::1]:{port}"),
        format!("[fd7a:115c:a1e1::1]:{port}"),
    ] {
        expect_refused(&env, &site, &bad, &real, &bad);
    }
}

#[test]
fn sigterm_and_sigint_stop_the_running_jobs_rclone_before_serve_exits() {
    for (name, sig) in [("SIGTERM", libc::SIGTERM), ("SIGINT", libc::SIGINT)] {
        let env = Env::new(&format!("cli-sig-{}", name.to_lowercase()));
        let site = photo_site(DAY, DAY, three());
        // 写真を送る copy を 30 秒止める（signal で止まらなければ、30 秒かかる）
        env.add_rule("copy", 0, 1, sleep_ms(30_000));
        let token = token_file(&env, "token", format!("{TOKEN}\n").as_bytes(), 0o600);
        let addr = free_addr();
        let mut child = spawn_serve(&env, &site, &addr.to_string(), &token);
        let pid = child.id();
        wait_ready(&mut child, addr);

        // 受け付ける
        let deadline = Instant::now() + Duration::from_secs(10);
        let accepted = loop {
            let r = post_json(
                addr,
                TOKEN,
                &json!({"from": DAY, "destination": "yamareco"}),
            );
            if r.status != 409 || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(accepted.status, 202, "{name}: {}", accepted.body);
        let job_id = accepted.json()["job_id"].as_str().unwrap().to_owned();
        let copy_pid = wait_for_call(&env, "copy", 30);
        assert!(process_exists(copy_pid), "{name}: copy が動いている");

        let sent = Instant::now();
        send(pid, sig);
        let out = wait_exit(child, 20);
        assert!(
            sent.elapsed() < Duration::from_secs(20),
            "{name}: 止まるのが遅い"
        );
        // 親が終わった時点で、rclone はもう居ない。実行枠も外れている
        assert!(
            !process_exists(copy_pid),
            "{name}: 親が終わったのに rclone が残っている"
        );
        assert!(slot_is_free(&env.state), "{name}: 実行枠が外れない");
        assert_eq!(out.code, Some(0), "{name}: {} {}", out.stdout, out.stderr);
        assert!(out.stdout.is_empty(), "{name}: {}", out.stdout);
        assert!(
            !out.stderr.contains(TOKEN),
            "{name}: ログにトークンが出ている"
        );
        // 待ち受けは閉じている
        assert!(nothing_listens(addr), "{name}: 終わったのに待ち受けている");

        // 記録: running のまま（blocked にも failed にもしない）。次の復旧に渡す
        let scan = JobStore::open(&env.state).unwrap().scan();
        assert!(scan.blocking().is_none(), "{name}");
        assert_eq!(scan.records.len(), 1, "{name}");
        assert_eq!(scan.records[0].job_id.as_str(), job_id, "{name}");
        assert_eq!(scan.records[0].status, Status::Running, "{name}");
        assert_eq!(scan.records[0].error, None, "{name}");
    }
}

/// 何も実行中でない `serve` は、SIGTERM ですぐ終わる（待ち受けも閉じる）。
#[test]
fn an_idle_serve_stops_on_sigterm_at_once() {
    let env = Env::new("cli-idle");
    let site = photo_site(DAY, DAY, three());
    let token = token_file(&env, "token", TOKEN.as_bytes(), 0o600);
    let addr = free_addr();
    let mut child = spawn_serve(&env, &site, &addr.to_string(), &token);
    wait_ready(&mut child, addr);
    let started = Instant::now();
    send(child.id(), libc::SIGTERM);
    let out = wait_exit(child, 20);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(!out.stderr.contains(TOKEN));
    assert!(nothing_listens(addr));
}

/// 起動時の復旧（止まったジョブの片付け。Drive の反映を待つので、本番の待ちでは 30 秒以上かかる）は、待ち受けより前に済ませる。
/// その間に SIGTERM を受けたら、待ち受けずに（signal の受け口は復旧より前に入っている）、復旧が止まってから 0 で終わる。
#[test]
fn a_sigterm_during_the_startup_recovery_ends_serve_without_listening() {
    let env = Env::new("cli-recovery-sig");
    let site = photo_site(DAY, DAY, three());
    let token = token_file(&env, "token", TOKEN.as_bytes(), 0o600);
    env.add_rule("copy", 0, 1, sleep_ms(30_000));
    let addr = free_addr();
    let mut child = spawn_serve(&env, &site, &addr.to_string(), &token);
    wait_ready(&mut child, addr);
    let r = post_json(
        addr,
        TOKEN,
        &json!({"from": DAY, "destination": "yamareco"}),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    wait_for_call(&env, "copy", 30);
    send(child.id(), libc::SIGTERM);
    assert_eq!(wait_exit(child, 20).code, Some(0));

    // 起こし直す。前のジョブ（Drive の段階を記録した running）の復旧が始まる
    let addr = free_addr();
    let mut child = spawn_serve(&env, &site, &addr.to_string(), &token);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "復旧の途中のはずが、終わっている"
    );
    assert!(nothing_listens(addr), "復旧の前に待ち受けている");
    let started = Instant::now();
    send(child.id(), libc::SIGTERM);
    let out = wait_exit(child, 30);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(
        nothing_listens(addr),
        "復旧の途中の signal のあとに待ち受けた"
    );
    assert!(slot_is_free(&env.state), "実行枠が外れない");
    assert!(!out.stderr.contains(TOKEN));
}

/// 別のプロセスの CLI（`run`）が実行枠を持っている間、HTTP の `POST`（受付も `dry_run` も）は 409 `busy`。CLI が終わって枠が
/// 空けば、HTTP は受け付けられる。プロセスをまたいだ排他（flock）の確かめ。
#[test]
fn while_a_separate_cli_run_holds_the_slot_http_posts_get_409_until_it_ends() {
    let env = Env::new("cli-vs-http");
    let site = photo_site(DAY, DAY, three());
    // 写真を送る copy を 30 秒止める（CLI の run が実行中のまま）
    env.add_rule("copy", 0, 1, sleep_ms(30_000));
    let mut kids = Reaper(Vec::new());
    kids.0.push(spawn_run(&env, &site));
    let copy_pid = wait_for_call(&env, "copy", 30);
    assert!(process_exists(copy_pid));

    let token = token_file(&env, "token", TOKEN.as_bytes(), 0o600);
    let addr = free_addr();
    kids.0
        .push(spawn_serve(&env, &site, &addr.to_string(), &token));
    wait_ready(&mut kids.0[1], addr);
    // serve の起動時の復旧は、枠が取れないので見送られる（CLI の記録には触れない）

    for body in [
        json!({"from": DAY, "destination": "yamareco"}),
        json!({"from": DAY, "destination": "yamareco", "dry_run": true}),
        json!({"from": "2030-03-12", "destination": "yamareco", "request_id": "rid-http"}),
    ] {
        let r = post_json(addr, TOKEN, &body);
        assert_eq!((r.status, r.error().as_str()), (409, "busy"), "{body}");
    }
    // CLI のジョブは、HTTP からも見える（実行中）。HTTP は新しいジョブを作っていない
    let list = get(addr, "/v1/exports", TOKEN).json();
    assert_eq!(list["jobs"].as_array().map(Vec::len), Some(1), "{list}");
    assert_eq!(list["jobs"][0]["status"], "running", "{list}");
    assert!(process_exists(copy_pid), "CLI の rclone が止められた");

    // CLI を止める（SIGTERM。rclone を止めて回収してから、枠を手放して 1 で終わる）
    let run = kids.0.remove(0);
    send(run.id(), libc::SIGTERM);
    let out = wait_exit(run, 20);
    assert_eq!(out.code, Some(1), "{} {}", out.stdout, out.stderr);
    assert!(!process_exists(copy_pid));
    assert!(slot_is_free(&env.state), "CLI が終わったのに枠が空かない");
    // 枠が空けば、HTTP の dry_run は通る（書かない確認）
    let r = post_json(
        addr,
        TOKEN,
        &json!({"from": DAY, "destination": "yamareco", "dry_run": true}),
    );
    assert_eq!(r.status, 200, "{}", r.body);

    let serve = kids.0.remove(0);
    send(serve.id(), libc::SIGTERM);
    assert_eq!(wait_exit(serve, 20).code, Some(0));
}
