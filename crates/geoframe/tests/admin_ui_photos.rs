//! 管理画面の写真タブ（チップの件数・タブの選択・AI 検索の場面・統一カード・ビューア・★の反映・位置の保存・要対応）の描画を、Node で小さな DOM の
//! 代用品（`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/photos_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_ai_waits.rs` と同じ。CI の Rust のジョブがこの描画試験を回す唯一の場所で、黙って省かれないようにする）。

use std::process::Command;

#[test]
fn the_admin_photos_tab_renders_chips_search_cards_viewer_and_flag_updates() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = match Command::new("node")
        .arg(format!("{root}/web/tests/photos_render.mjs"))
        .arg(format!("{root}/web/static/js/admin.js"))
        .arg(format!("{root}/web/static/js/photos.js"))
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
