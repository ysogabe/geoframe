//! 管理画面の「ライブラリ」タブ（旅・日付のカードと、選ぶと下に出る写真の格子・年の月のチップ・除外の切り替え・古い応答の捨て方・
//! ビューア・★の反映・場所の行・地図の点とクラスタ・吹き出し・表示範囲の写真）の描画を、Node で小さな DOM と
//! MapLibre の代用品（`web/tests/fakedom.mjs`・`web/tests/fake_maplibre.mjs`）を使って確かめる（`web/tests/library_render.mjs`）。
//! 合成の JSON だけで、ネットワークには出ない。
//! Node が無い環境ではこの試験は何もしない。ただし CI（環境変数 `CI` がある）では省かず、Node が無ければ失敗にする
//! （`admin_ui_photos.rs` と同じ）。実際のクラスタの操作・タブに戻ったあとの地図の大きさ・カードの横スクロールは、
//! 検証環境での目視で確かめる。CSS の文字列（カードの並び・狭い画面の横スクロール）はこのファイルの試験で確かめる。

mod common;

#[test]
fn the_admin_library_tab_renders_cards_detail_months_viewer_map_and_popup() {
    common::node::run_render_test(
        "library_render.mjs",
        &["admin.js", "photos.js", "library.js"],
    );
}

/// コメントと空白を除いた `admin.css`
fn compact_css() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/static/admin.css");
    let mut text = std::fs::read_to_string(path).expect("admin.css を読めない");
    while let Some(start) = text.find("/*") {
        let end = text[start..]
            .find("*/")
            .map(|i| start + i + 2)
            .expect("コメントが閉じていない");
        text.replace_range(start..end, "");
    }
    text.split_whitespace().collect()
}

/// `needle` で始まる `{…}` の中身（波括弧の入れ子を数える）。`from` 以降の最初の一致
fn block_after(css: &str, needle: &str, from: usize) -> Option<(usize, String)> {
    let at = from + css[from..].find(needle)?;
    let open = at + needle.len();
    assert_eq!(&css[open - 1..open], "{", "{needle} の直後が {{ でない");
    let mut depth = 1;
    for (i, c) in css[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((at, css[open..open + i].to_string()));
                }
            }
            _ => {}
        }
    }
    panic!("{needle} の波括弧が閉じていない");
}

/// `selector{…}` の本体（media の外・中を問わず、最初に見つかった一致）
fn rule_body<'a>(css: &'a str, selector: &str) -> Option<&'a str> {
    let needle = format!("}}{selector}{{");
    let start = css
        .find(&needle)
        .map(|i| i + 1)
        .or_else(|| css.starts_with(&format!("{selector}{{")).then_some(0))?;
    let open = start + selector.len() + 1;
    let end = open + css[open..].find('}')?;
    Some(&css[open..end])
}

/// AC1: 選択が無いとき、旅・年のカードの容器は幅に合わせて並べる。選択があるとき（`.is-strip`）は 1 段の横スクロール
#[test]
fn the_library_cards_flow_by_width_and_become_a_scrolling_strip_when_one_is_selected() {
    let css = compact_css();
    let cards = rule_body(&css, ".lib-cards").expect(".lib-cards の規則が無い");
    assert!(cards.contains("display:grid"), "{cards}");
    assert!(
        cards.contains("grid-template-columns:repeat(auto-fill,minmax(220px,1fr))"),
        "{cards}"
    );
    let strip = rule_body(&css, ".lib-cards.is-strip").expect(".lib-cards.is-strip の規則が無い");
    assert!(strip.contains("overflow-x:auto"), "{strip}");
    assert!(strip.contains("scroll-snap-type:x"), "{strip}");
    assert!(strip.contains("grid-auto-flow:column"), "{strip}");
    let card = rule_body(&css, ".lib-card").expect(".lib-card の規則が無い");
    assert!(card.contains("scroll-snap-align:start"), "{card}");
    // 選んだカードはシアンの枠と光（色は :root の変数）
    let selected =
        rule_body(&css, ".lib-card.is-selected").expect(".lib-card.is-selected の規則が無い");
    assert!(selected.contains("var(--cyan)"), "{selected}");
    assert!(selected.contains("box-shadow"), "{selected}");
    assert!(
        !selected.contains('#'),
        "色は :root の変数で書く: {selected}"
    );
}

/// AC16: 幅 640px 以下では、カードの列が横スクロール（`overflow-x:auto`）になり、列に `min-width:0`。
/// `.is-strip` の `overflow-x:auto`（media の外）では満たさない — media の中身を取り出して調べる
#[test]
fn a_narrow_screen_scrolls_the_library_card_rows_sideways() {
    let css = compact_css();
    let mut from = 0;
    let mut found = false;
    while let Some((at, body)) = block_after(&css, "@media(max-width:640px){", from) {
        from = at + 1;
        if let Some(rule) = body.split('}').find(|r| {
            r.starts_with(".lib-cards,.lib-cards.is-strip{") || r.starts_with(".lib-cards{")
        }) {
            assert!(rule.contains("overflow-x:auto"), "{rule}");
            assert!(rule.contains("grid-auto-flow:column"), "{rule}");
            assert!(rule.contains("grid-template-columns:none"), "{rule}");
            assert!(
                body.contains("#lib-trips,#lib-calendar,#lib-detail{min-width:0}"),
                "列に min-width:0: {body}"
            );
            assert!(body.contains("#lib-map{"), "狭い画面の地図の高さ: {body}");
            found = true;
        }
    }
    assert!(found, "幅 640px 以下の @media に .lib-cards の規則が無い");
}
