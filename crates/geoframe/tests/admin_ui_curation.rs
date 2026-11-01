//! 管理画面の「選別」タブ（段の描画・チェックして代表にする・このままで確認済み・取り消す・409 の読み直し・ビューア・
//! ★/除外の反映・絞り込み・ページング・状況タブの件数からの移動）の描画を、Node で小さな DOM の代用品
//! （`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/curation_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_photos.rs` と同じ）。実際のブラウザでの見た目・label の中の画像を押したときの挙動は、検証環境での目視で確かめる。

mod common;

#[test]
fn the_admin_curation_tab_renders_groups_confirms_resets_and_rereads_on_conflict() {
    common::node::run_render_test(
        "curation_render.mjs",
        &["admin.js", "photos.js", "library.js", "curation.js"],
    );
}
