//! 管理画面の「運用」タブ（操作のボタン・送信中と実行中の disabled・段の表・大量削除の保留の 2 段階の確認・409 での読み直し・
//! 定期サイクルと履歴の描画・古い応答の破棄・タブを離れたときの読み直しの停止）の描画を、Node で小さな DOM の代用品
//! （`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/ops_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_curation.rs` と同じ）。実際のブラウザでの見た目は、検証環境での目視で確かめる。

mod common;

#[test]
fn the_admin_ops_tab_renders_actions_hold_confirmation_cycle_and_history() {
    common::node::run_render_test("ops_render.mjs", &["admin.js", "photos.js", "ops.js"]);
}
