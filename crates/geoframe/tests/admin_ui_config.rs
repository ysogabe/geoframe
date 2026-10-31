//! 管理画面の設定タブ（読み取り専用の案内・RUNTIME・設定ファイルの表・3 つのパネル・AI の用途の表・失敗・古い応答の破棄）の描画を、Node で
//! 小さな DOM の代用品（`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/config_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_ai_waits.rs` と同じ。CI の Rust のジョブがこの描画試験を回す唯一の場所で、黙って省かれないようにする）。

use std::process::Command;

#[test]
fn the_admin_config_tab_renders_every_item_with_its_source_and_the_ai_tasks() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = match Command::new("node")
        .arg(format!("{root}/web/tests/config_render.mjs"))
        .arg(format!("{root}/web/static/js/admin.js"))
        .arg(format!("{root}/web/static/js/photos.js"))
        .arg(format!("{root}/web/static/js/config.js"))
        .output()
    {
        Ok(output) => output,
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
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
