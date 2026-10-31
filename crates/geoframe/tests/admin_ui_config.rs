//! 管理画面の設定タブ（読み取り専用の案内・RUNTIME・設定ファイルの表・3 つのパネル・AI の用途の表・失敗・古い応答の破棄）の描画を、Node で
//! 小さな DOM の代用品（`web/tests/fakedom.mjs`）を使って確かめる（`web/tests/config_render.mjs`）。合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_ai_waits.rs` と同じ。CI の Rust のジョブがこの描画試験を回す唯一の場所で、黙って省かれないようにする）。

use std::process::Command;

use geoframe::config::EnvItem;

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

/// 設定タブの表（`config.js` の `CONFIG_ITEMS`）の key が、サーバーの `EnvItem::ALL` の全項目と過不足なく一致する。
/// 打ち間違いや項目の追加漏れがあると、その項目が「その他」に落ちるか、表に出ない。Node が無くても動く。
#[test]
fn the_config_tab_table_names_exactly_the_items_the_server_reports() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let js = std::fs::read_to_string(format!("{root}/web/static/js/config.js")).unwrap();
    let mut table: Vec<String> = js
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("{key: '"))
        .filter_map(|l| l.split('\'').next())
        .map(str::to_string)
        .collect();
    table.sort();
    let mut server: Vec<String> = EnvItem::ALL.iter().map(|i| i.name().to_string()).collect();
    server.sort();
    assert_eq!(table, server);
}
