//! 管理画面 STATUS の AI 一時待機の描画（`web/static/js/admin.js` の `aiOpsCells`）を、Node で最小の DOM の
//! 代用品を使って確かめる（`web/tests/admin_ai_ops_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない（画面描画の確認はその環境では省く）。ただし CI（環境変数 `CI` がある）では
//! 省かず、Node が無ければ失敗にする（CI の Rust のジョブがこの描画試験を回す唯一の場所で、黙って省かれないようにする）。

mod common;

#[test]
fn the_admin_status_screen_renders_ai_waits_with_fixed_words_and_utc_deadlines() {
    common::node::run_render_test("admin_ai_ops_render.mjs", &["admin.js"]);
}
