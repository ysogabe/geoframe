//! 管理画面の「ライブラリ」タブ（旅の一覧・サムネイルの格子・ビューア・★の反映・地図の点とクラスタ・吹き出し・
//! 表示範囲の写真）の描画を、Node で小さな DOM と MapLibre の代用品（`web/tests/fakedom.mjs`・
//! `web/tests/fake_maplibre.mjs`）を使って確かめる（`web/tests/library_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_photos.rs` と同じ）。実際のクラスタの操作・タブに戻ったあとの地図の大きさは、検証環境での目視で確かめる。

use std::process::Command;

#[test]
fn the_admin_library_tab_renders_trips_grid_viewer_map_and_popup() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = match Command::new("node")
        .arg(format!("{root}/web/tests/library_render.mjs"))
        .arg(format!("{root}/web/static/js/admin.js"))
        .arg(format!("{root}/web/static/js/photos.js"))
        .arg(format!("{root}/web/static/js/library.js"))
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
