//! 画面描画の試験（`web/tests/*_render.mjs`）を Node で起動する共通部品。
//!
//! 上限なしで起動すると、試験の失敗の仕方によってはホストごと止まる（assert の失敗メッセージが DOM を深く inspect して
//! 数十 GiB、解決済みの応答で読み直しを繰り返すループで `calls` が膨らむ、など）。そこで
//! V8 のヒープ上限（`--max-old-space-size`）と時間の上限（過ぎたら kill）を付ける。カーネルの上限（`RLIMIT_AS`）は
//! `pre_exec` が要り、このリポジトリでは `Command::new` 以外の起動の道具を使わない決まり（`no_python.rs`）なので付けない。
//! ヒープ上限は RSS をきっちり抑えないが（inspect の文字列などが外に出る）、暴走は時間の上限で止まる。
//! Node が無い環境では何もしない。ただし CI（環境変数 `CI` がある）では省かず失敗にする。

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// V8 の旧世代ヒープの上限（MiB）。描画試験は数十 MiB で足りる
pub const NODE_HEAP_MB: u32 = 512;
/// 試験 1 本の時間の上限。普段は数秒で終わる
pub const NODE_TIMEOUT: Duration = Duration::from_secs(120);

/// `web/tests/<script>` を、`web/static/js/<name>` の列を引数にして動かす。失敗・タイムアウトは panic。
pub fn run_render_test(script: &str, js_files: &[&str]) {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut cmd = Command::new("node");
    cmd.arg(format!("--max-old-space-size={NODE_HEAP_MB}"))
        .arg(format!("{root}/web/tests/{script}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in js_files {
        cmd.arg(format!("{root}/web/static/js/{name}"));
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI で node が無い: 画面描画の試験を省けない"
            );
            eprintln!("node が無いので画面描画の試験を省く");
            return;
        }
        Err(e) => panic!("node を起動できない: {e}"),
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = wait_with_deadline(&mut child, NODE_TIMEOUT);
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    match status {
        Some(status) => assert!(status.success(), "{script}: {status}\n{stdout}\n{stderr}"),
        None => panic!(
            "{script}: {} 秒で終わらないので止めた（ループか inspect の暴走の疑い）\n{stdout}\n{stderr}",
            NODE_TIMEOUT.as_secs()
        ),
    }
}

/// 出力を別スレッドで読み切る（パイプが詰まって子が止まらないように）
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        text
    })
}

/// 期限まで待つ。過ぎたら kill して None
fn wait_with_deadline(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("node の状態を読めない") {
            return Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
