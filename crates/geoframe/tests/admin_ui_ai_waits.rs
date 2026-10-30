//! 管理画面 STATUS の AI 一時待機の描画（`web/static/js/admin.js` の `aiOpsCells`）を、Node で最小の DOM の
//! 代用品を使って確かめる（`web/tests/admin_ai_ops_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない（画面描画の確認はその環境では省く）。

use std::process::Command;

#[test]
fn the_admin_status_screen_renders_ai_waits_with_fixed_words_and_utc_deadlines() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = match Command::new("node")
        .arg(format!("{root}/web/tests/admin_ai_ops_render.mjs"))
        .arg(format!("{root}/web/static/js/admin.js"))
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("node が無いので画面描画の試験を省く");
            return;
        }
        Err(e) => panic!("node を起動できない: {e}"),
    };
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
