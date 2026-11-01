//! 管理画面の「選別」タブ（段の描画・チェックして代表にする・このままで確認済み・取り消す・409 の読み直し・ビューア・
//! ★/除外の反映・絞り込み・ページング・状況タブの件数からの移動）の描画を、Node で小さな DOM の代用品
//! （`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/curation_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_photos.rs` と同じ）。実際のブラウザでの見た目・label の中の画像を押したときの挙動は、検証環境での目視で確かめる。

use std::process::Command;

#[test]
fn the_admin_curation_tab_renders_groups_confirms_resets_and_rereads_on_conflict() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = match Command::new("node")
        .arg(format!("{root}/web/tests/curation_render.mjs"))
        .arg(format!("{root}/web/static/js/admin.js"))
        .arg(format!("{root}/web/static/js/photos.js"))
        .arg(format!("{root}/web/static/js/library.js"))
        .arg(format!("{root}/web/static/js/curation.js"))
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
