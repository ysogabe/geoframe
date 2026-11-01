//! 管理画面の写真タブ（チップの件数・タブの選択・AI 検索の場面・統一カード・ビューア・★の反映・位置の保存・要対応）の描画を、Node で小さな DOM の
//! 代用品（`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/photos_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_ai_waits.rs` と同じ。CI の Rust のジョブがこの描画試験を回す唯一の場所で、黙って省かれないようにする）。

mod common;

#[test]
fn the_admin_photos_tab_renders_chips_search_cards_viewer_and_flag_updates() {
    common::node::run_render_test("photos_render.mjs", &["admin.js", "photos.js"]);
}
