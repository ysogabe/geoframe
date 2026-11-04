//! `photo_regions` 表（写真ごとの国・地域の分類。派生データ）と、読み取り側が使う「有効な分類」の SQL 片。
//! 設計は docs/international-places/design.md の「保存データと有効性」。
//!
//! - 表は [`crate::curation`] と同じ型で、`schema::SCHEMA` には足さない（Python の fixture と比べる試験があるため）。
//!   書き込み側が起動時に [`ensure_tables`] で作る。無い DB（`GEOFRAME_INGEST=0` の古い DB）では [`available`] が偽になり、
//!   読む側は JOIN しない。API からは作らない。
//! - 行は「分類に使った座標（`input_lat`/`input_lon`）」と「分類版」を持つ。**現在の写真の座標と一致し、分類版が今の同梱版と
//!   同じ行だけが有効**（[`join`]）。座標が変わった・版が上がった・写真が消えた行は読まれず、同期段が分類し直す・掃除する。
//! - 判定そのものができなかった座標（`failed`。同梱データの不整合）も、同じ座標・同じ版では選び直さないよう行に残す。
//!   `country_method`/`region_method` が `failed` の行は**有効な分類ではない**（[`join`] に一致しない = 読み取り側では
//!   `unclassified`）。版が変われば選び直す。
//! - 表には写真名・鍵・画像・応答本文を入れない。国・地域の表示名は同梱の ID→名称表から引く。

use rusqlite::{Connection, TransactionBehavior};

use super::search::classifier_version;

/// この表が作る表の名前（[`ensure_tables`] の対象）。
pub const TABLES: &[&str] = &["photo_regions"];

/// `photo_regions` の DDL。`IF NOT EXISTS` だけで、他の表には触れない。
///
/// - `input_lat`/`input_lon` は分類に使った座標を元の数値のまま持つ（`photos.lat`/`lon` と SQLite で同値比較する）。
/// - `country_id`/`region_id` は未判定なら NULL。`search_text` は未判定なら空文字。
/// - `country_method`/`region_method` の `failed` は「判定できなかった」印（国・地域・検索用文字列は空）。`inside`/`outside`/
///   `ambiguous` と違い分類の結果ではなく、[`join`] は `failed` の行を引かない。
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS photo_regions(
  photo_id INTEGER PRIMARY KEY,
  input_lat REAL NOT NULL,
  input_lon REAL NOT NULL,
  classifier_version TEXT NOT NULL,
  country_id TEXT,
  region_id TEXT,
  search_text TEXT NOT NULL DEFAULT '',
  country_method TEXT NOT NULL CHECK(country_method IN ('inside','outside','ambiguous','failed')),
  region_method TEXT NOT NULL CHECK(region_method IN ('inside','outside','ambiguous','failed')),
  classified_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS photo_regions_place ON photo_regions(country_id, region_id);";

/// 表があるか（`sqlite_master` の 1 行）。読み取り接続でも呼べる。
pub fn available(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='photo_regions')",
        [],
        |row| row.get(0),
    )
}

/// まだ無い表の名前（書き込みなし。`--dry-run` 用）。
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    Ok(if available(conn)? {
        Vec::new()
    } else {
        TABLES.iter().map(|t| (*t).to_string()).collect()
    })
}

/// 表を作る（無いときだけ。作った名前を返す）。書き込み用の接続で、起動時に 1 回呼ぶ。
pub fn ensure_tables(conn: &mut Connection) -> rusqlite::Result<Vec<String>> {
    let missing = pending(conn)?;
    if missing.is_empty() {
        return Ok(missing);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    tx.commit()?;
    Ok(missing)
}

// ---- SQL 片 ----

/// 写真の別名 `p` の座標が**有効**な条件: 数値の型（NULL・文字列・BLOB を除く）で、緯度 ±90・経度 ±180 以内
/// （`BETWEEN` は無限大・NaN を通さない）。分類の対象にする座標も、有効な分類の JOIN も、この条件を使う。
pub fn valid_coordinates(p: &str) -> String {
    format!(
        "(typeof({p}.lat) IN ('real','integer') AND typeof({p}.lon) IN ('real','integer')
  AND {p}.lat BETWEEN -90 AND 90 AND {p}.lon BETWEEN -180 AND 180)"
    )
}

/// 分類版を SQL に埋め込める形（`'…'`）にする。版は ASCII 英数字と `-` だけ（固定ハッシュ）。それ以外が混じるときは `None`
/// （呼び手は何にも一致しない条件にする）。
pub(super) fn version_literal(version: &str) -> Option<String> {
    (!version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then(|| format!("'{version}'"))
}

/// 今の同梱の分類版を [`version_literal`] にしたもの。得られない（同梱データの破損）ときは `NULL`（何にも一致しない）。
fn current_version_literal() -> String {
    classifier_version()
        .ok()
        .and_then(version_literal)
        .unwrap_or_else(|| "NULL".to_string())
}

/// `photos` の別名 `p` の写真に、**有効な分類**だけを付ける `LEFT JOIN`。`r` は `photo_regions` の別名。
/// 有効 = 行の `input_lat`/`input_lon` が現在の座標と一致し、`classifier_version` が今の同梱版で、現在の座標が有効
/// （[`valid_coordinates`]）で、判定できた行（`country_method` が `failed` でない）。`photo_id` が主キーなので `p` の行が
/// 増えることはない。
///
/// `failed` の行（判定できなかった座標を、選び直さないために残した印）は結合しないので、`r` の列は NULL になる。読み取り側から見ると
/// 分類行が無い写真と区別がつかず、座標があるのに国が決まらない写真として `unclassified` に入る（`failed` を別の区分にしない）。
///
/// 表があること（[`available`]）は呼び手が確かめる（無ければ JOIN しない）。分類版が得られない（同梱データの破損）ときは、
/// 何にも一致しない JOIN になる（壊れた分類を提供しない）。
pub fn join(p: &str, r: &str) -> String {
    format!("LEFT JOIN photo_regions {r} ON {}", valid_on(p, r))
}

/// [`join`] の `ON` の中身（有効な分類の条件）。`EXISTS (SELECT 1 FROM photo_regions r WHERE …)` にも使える。
pub(super) fn valid_on(p: &str, r: &str) -> String {
    format!(
        "{r}.photo_id={p}.id AND {r}.input_lat={p}.lat AND {r}.input_lon={p}.lon
  AND {r}.classifier_version={version} AND {r}.country_method<>'failed' AND {valid}",
        version = current_version_literal(),
        valid = valid_coordinates(p)
    )
}

/// `photos` の別名 `p` の写真が**分類の対象**になる条件: `pref` が空（NULL か空文字）で、座標が有効。
/// 有効な分類が既にあるかは別（[`needs_classification`]）。
pub(super) fn classifiable(p: &str) -> String {
    format!(
        "({p}.pref IS NULL OR {p}.pref='') AND {valid}",
        valid = valid_coordinates(p)
    )
}

/// 同期段が**まだ処理していない**（分類行が無い・座標が合わない・分類版が違う）ことの条件。`version` は [`version_literal`] の結果。
/// `failed` の行も処理済みに数える（同じ座標・同じ版では選び直さない）。読み取りの有効性は [`join`]。
pub(super) fn needs_classification(p: &str, version: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM photo_regions r0 WHERE r0.photo_id={p}.id
     AND r0.input_lat={p}.lat AND r0.input_lon={p}.lon AND r0.classifier_version={version})"
    )
}

/// 分類待ちの写真の数: `pref` が空で座標が有効なのに、有効な分類（現在の座標・今の分類版）がまだ無いもの。
/// `regions` 段の `pending` と同じ条件。表が無い DB では、`pref` 空で座標が有効な全件。読み取り接続で呼べる。
pub fn pending_photos(conn: &Connection) -> rusqlite::Result<i64> {
    let missing = if available(conn)? {
        format!(
            " AND {}",
            needs_classification("p", &current_version_literal())
        )
    } else {
        String::new()
    };
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM photos p WHERE {}{missing}",
            classifiable("p")
        ),
        [],
        |row| row.get(0),
    )
}
