//! 管理画面の「ライブラリ」タブ（旅の一覧・サムネイルの格子・ビューア・★の反映・地図の点とクラスタ・吹き出し・
//! 表示範囲の写真）の描画を、Node で小さな DOM と MapLibre の代用品（`web/tests/fakedom.mjs`・
//! `web/tests/fake_maplibre.mjs`）を使って確かめる（`web/tests/library_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_photos.rs` と同じ）。実際のクラスタの操作・タブに戻ったあとの地図の大きさは、検証環境での目視で確かめる。

mod common;

#[test]
fn the_admin_library_tab_renders_trips_grid_viewer_map_and_popup() {
    common::node::run_render_test(
        "library_render.mjs",
        &["admin.js", "photos.js", "library.js"],
    );
}
