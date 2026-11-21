//! T4: rclone の起動（design.md 4.2「rclone の呼び方」、8 章「子プロセス」）。子プロセスのパイプ・期限・kill・終了コード・
//! 標準エラーの伏せ字。短いシェルスクリプトを「rclone」として使う（本物の rclone・Drive には触れない）。

mod common;

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::TempDir;
use common::drive::*;
use geoframe_export::rclone::{Rclone, RemoteLoc, RunError};
use geoframe_export::request::Destination;
use geoframe_export::settings::{RawSettings, RemotePath, Settings};
use serde_json::json;

fn write_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-rclone.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn settings_for(program: &Path, state: &Path) -> Settings {
    Settings::from_raw(&RawSettings {
        rclone: Some(program.to_string_lossy().into_owned()),
        rclone_config: Some(
            state
                .join("rclone/rclone.conf")
                .to_string_lossy()
                .into_owned(),
        ),
        state_dir: Some(state.to_string_lossy().into_owned()),
        ..RawSettings::default()
    })
    .expect("settings")
}

fn loc() -> RemoteLoc {
    RemoteLoc::of(&RemotePath::parse("gdrive:yamareco").unwrap())
        .child("x.txt")
        .unwrap()
}

struct Sh {
    _dir: TempDir,
    state: PathBuf,
    rclone: Rclone,
}

fn sh(name: &str, body: &str, copy: Duration, other: Duration) -> Sh {
    capture_logs();
    let dir = TempDir::new(name);
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let script = write_script(dir.path(), body);
    let rclone = Rclone::new(&settings_for(&script, &state)).with_timeouts(copy, other);
    Sh {
        _dir: dir,
        state,
        rclone,
    }
}

/// 書いたばかりの実行ファイルを、別のスレッドの fork と重なって実行すると ETXTBSY になることがある。起動できなかったら数回やり直す。
fn retry<T>(mut f: impl FnMut() -> Result<T, RunError>) -> Result<T, RunError> {
    let mut last = Err(RunError::Spawn);
    for _ in 0..5 {
        last = f();
        if !matches!(last, Err(RunError::Spawn)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    last
}

const SECS: Duration = Duration::from_secs(20);

#[test]
fn large_stdout_and_stderr_do_not_stall_the_child() {
    // パイプの上限（64KB）を超える出力。読み続けないと、子が詰まって期限まで止まる
    let s = sh(
        "big-out",
        "head -c 3000000 /dev/zero | tr '\\000' 'a'\nhead -c 2000000 /dev/zero | tr '\\000' 'e' >&2",
        SECS,
        SECS,
    );
    let started = Instant::now();
    let out = retry(|| s.rclone.cat(&loc(), 4 * 1024 * 1024)).expect("ok");
    assert_eq!(out.len(), 3_000_000);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn stdout_over_the_cap_is_a_failure_and_the_rest_is_discarded() {
    let s = sh(
        "cap",
        "head -c 200000 /dev/zero | tr '\\000' 'a'",
        SECS,
        SECS,
    );
    // 読む量の上限は max+1。超えて出力する子は、読み捨てて失敗にする
    assert_eq!(
        retry(|| s.rclone.cat(&loc(), 100)),
        Err(RunError::BadOutput)
    );
}

#[test]
fn the_command_timeout_kills_the_child_and_its_descendants() {
    // 子孫（バックグラウンドの sleep）がパイプを握っていても、プロセスグループごと kill して返る
    let s = sh(
        "timeout",
        "sleep 30 &\nsleep 30",
        SECS,
        Duration::from_millis(400),
    );
    let started = Instant::now();
    assert_eq!(retry(|| s.rclone.cat(&loc(), 10)), Err(RunError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn copy_uses_the_copy_timeout_and_everything_else_the_other_one() {
    let s = sh("copy-limit", "sleep 30", Duration::from_millis(400), SECS);
    let started = Instant::now();
    assert_eq!(
        retry(|| s.rclone.copy(
            Path::new("/tmp"),
            &loc(),
            geoframe_export::rclone::CopyScope::PhotosOnly
        )),
        Err(RunError::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    // copy 以外（cat）は 20 秒の期限なので、すぐ終わる子は成功する
    let ok = sh("copy-limit-ok", "echo hi", Duration::from_millis(400), SECS);
    assert_eq!(retry(|| ok.rclone.cat(&loc(), 10)).unwrap(), b"hi\n");
}

#[test]
fn the_job_deadline_stops_a_running_child_and_blocks_new_ones() {
    let s = sh("deadline", "sleep 30", SECS, SECS);
    let r = s
        .rclone
        .with_deadline(Some(Instant::now() + Duration::from_millis(400)));
    let started = Instant::now();
    assert_eq!(retry(|| r.cat(&loc(), 10)), Err(RunError::JobDeadline));
    assert!(started.elapsed() < Duration::from_secs(8));
    // 過ぎたあとは、起動しない
    let dir = TempDir::new("deadline2");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let marker = state.join("ran");
    let script = write_script(dir.path(), &format!("touch '{}'", marker.display()));
    let r = Rclone::new(&settings_for(&script, &state))
        .with_deadline(Some(Instant::now() - Duration::from_millis(1)));
    assert_eq!(r.cat(&loc(), 10), Err(RunError::JobDeadline));
    assert!(!marker.exists(), "期限を過ぎたあとは起動しない");
}

fn pid_gone(pid: i32) -> bool {
    // SAFETY: シグナル 0 は、存在の確かめだけ（何も送らない）。
    unsafe { libc::kill(pid, 0) != 0 }
}

fn read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[test]
fn a_cancel_stops_the_child_and_its_descendants_and_waits_for_them() {
    let dir = TempDir::new("cancel-run");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let (pid_file, grand_file) = (state.join("pid"), state.join("grandpid"));
    let script = write_script(
        dir.path(),
        &format!(
            "echo $$ > '{}'\nsleep 30 &\necho $! > '{}'\nwait",
            pid_file.display(),
            grand_file.display()
        ),
    );
    let cancel = geoframe_export::cancel::Cancel::new();
    let rclone = Rclone::new(&settings_for(&script, &state))
        .with_timeouts(SECS, SECS)
        .with_cancel(cancel.clone());
    let worker = {
        let rclone = rclone.clone();
        std::thread::spawn(move || retry(|| rclone.cat(&loc(), 10)))
    };
    // 子とその子孫が動き出すまで待つ
    let deadline = Instant::now() + Duration::from_secs(10);
    let (child, grand) = loop {
        if let (Some(c), Some(g)) = (read_pid(&pid_file), read_pid(&grand_file)) {
            break (c, g);
        }
        assert!(Instant::now() < deadline, "子が動き出さない");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!pid_gone(child) && !pid_gone(grand));
    let started = Instant::now();
    cancel.cancel();
    assert_eq!(worker.join().unwrap(), Err(RunError::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    // 返った時点で、子は `wait` 済み（居ない）。子孫も、グループごと kill されている（親が居なくなった子孫は、init が
    // 回収するまでの短い間だけ残りうる）
    assert!(pid_gone(child), "子が残っている");
    let until = Instant::now() + Duration::from_secs(5);
    while !pid_gone(grand) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pid_gone(grand), "子孫が残っている");
}

#[test]
fn a_cancel_that_is_already_set_never_starts_the_child() {
    let dir = TempDir::new("cancel-first");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let marker = state.join("ran");
    let script = write_script(dir.path(), &format!("touch '{}'", marker.display()));
    let cancel = geoframe_export::cancel::Cancel::new();
    cancel.cancel();
    let r = Rclone::new(&settings_for(&script, &state)).with_cancel(cancel);
    assert_eq!(r.cat(&loc(), 10), Err(RunError::Cancelled));
    assert_eq!(
        r.lsjson(&loc(), false, true).unwrap_err(),
        RunError::Cancelled
    );
    assert!(r.is_cancelled());
    assert!(!marker.exists(), "印が立っていれば起動しない");
}

#[test]
fn exit_codes_are_distinguished_and_stdin_is_closed() {
    let nf = sh("exit3", "exit 3", SECS, SECS);
    assert_eq!(retry(|| nf.rclone.cat(&loc(), 10)), Err(RunError::NotFound));
    let e7 = sh("exit7", "echo secret-looking-token >&2\nexit 7", SECS, SECS);
    assert_eq!(retry(|| e7.rclone.cat(&loc(), 10)), Err(RunError::Exit(7)));
    let sig = sh("signal", "kill -9 $$", SECS, SECS);
    assert_eq!(
        retry(|| sig.rclone.cat(&loc(), 10)),
        Err(RunError::Exit(-1))
    );
    // 標準入力は閉じてある（読もうとしてもすぐ終わる）
    let stdin = sh("stdin", "cat\necho done", SECS, SECS);
    assert_eq!(retry(|| stdin.rclone.cat(&loc(), 10)).unwrap(), b"done\n");
    // 実行ファイルが無い
    let dir = TempDir::new("nobin");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let r = Rclone::new(&settings_for(&dir.path().join("missing"), &state));
    assert_eq!(r.cat(&loc(), 10), Err(RunError::Spawn));
}

#[test]
fn the_private_work_directories_are_created_with_mode_0700() {
    let s = sh("dirs", "echo ok", SECS, SECS);
    retry(|| s.rclone.cat(&loc(), 10)).unwrap();
    for d in ["home", "cache"] {
        let mode = std::fs::metadata(s.state.join("rclone").join(d))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{d}");
    }
}

// ---- 標準エラーのログ ----

/// ログの捕まえ方。tracing の呼び出し点は、最初に通ったときの購読者の有無を覚えるので、この試験の実行ファイル全体で、最初の失敗の前に
/// 1 回だけ大域の購読者を入れる（試験は並列に走るので、他の試験のログも入る。確かめるのは「秘密が 1 つも出ていない」こと）。
static LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INIT: std::sync::Once = std::sync::Once::new();

fn capture_logs() {
    INIT.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Capture);
    });
}

struct Capture;

struct Visitor(String);
impl tracing::field::Visit for Visitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut v = Visitor(String::new());
        event.record(&mut v);
        LOGS.lock().unwrap().push(v.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn stderr_lines_with_tokens_are_masked_in_the_log_and_never_reach_the_error() {
    let env = Env::new("redact");
    let secret_lines = [
        "ERROR : auth failed: access_token=ya29.SECRETVALUE",
        "Authorization: Bearer abcdefSECRET",
        "refresh TOKEN expired SECRETTOKEN2",
    ];
    let stderr = format!(
        "first plain line\n{}\nlast plain line",
        secret_lines.join("\n")
    );
    env.add_rule(
        "lsjson",
        0,
        1,
        json!({"kind": "fail", "code": 1, "stderr": stderr}),
    );
    capture_logs();
    let layout = env.layout(Destination::Yamareco, FOLDER, JOB_A);
    let err = geoframe_export::drive::Drive::new(&env.rclone(), &layout)
        .target()
        .unwrap_err();
    // エラーは固定の説明（rclone の出力は載らない）
    let shown = format!("{err} {err:?}");
    for bad in ["SECRET", "Bearer", "access_token", "plain line"] {
        assert!(!shown.contains(bad), "{shown}");
    }
    let all = LOGS.lock().unwrap().clone();
    let ours: Vec<&String> = all
        .iter()
        .filter(|l| l.contains("first plain line"))
        .collect();
    assert_eq!(ours.len(), 1, "{all:?}");
    let logged = ours[0];
    assert!(logged.contains("rclone が失敗しました"), "{logged}");
    assert!(logged.contains("[伏せ字]"), "{logged}");
    assert!(logged.contains("last plain line"), "{logged}");
    // どのログにも、秘密に見える語は出ていない
    for line in &all {
        for bad in [
            "SECRET",
            "Bearer",
            "access_token",
            "ya29",
            "TOKEN expired",
            "secret-looking",
        ] {
            assert!(!line.contains(bad), "{bad}: {line}");
        }
    }
}

#[test]
fn a_very_long_stderr_line_that_starts_with_a_bearer_token_is_masked_as_a_whole() {
    // 末尾の 64KiB だけを先に切り出して検査すると、先頭の `Bearer` が切れて、残り（資格情報の本体）が出る
    let env = Env::new("redact-long");
    let body = "LONGSECRETBODY".repeat(20_000); // 約 280KB の 1 行
    let stderr = format!("Authorization: Bearer {body}\nlong-case tail marker line");
    env.add_rule(
        "lsjson",
        0,
        1,
        json!({"kind": "fail", "code": 1, "stderr": stderr}),
    );
    capture_logs();
    let layout = env.layout(Destination::Yamareco, FOLDER, JOB_A);
    let _ = geoframe_export::drive::Drive::new(&env.rclone(), &layout)
        .target()
        .unwrap_err();
    let all = LOGS.lock().unwrap().clone();
    let ours: Vec<&String> = all
        .iter()
        .filter(|l| l.contains("long-case tail marker line"))
        .collect();
    assert_eq!(ours.len(), 1, "{}", all.len());
    assert!(ours[0].contains("[伏せ字]"), "{}", ours[0]);
    for line in &all {
        assert!(
            !line.contains("LONGSECRET"),
            "{}",
            &line[..line.len().min(200)]
        );
    }
}
