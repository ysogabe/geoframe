//! 「場所の区分の規則」（docs/international-places/design.md）を SQL 片にまとめたもの。管理 API の集計（`places`）・
//! 絞り込み・検索は、規則を個別に書き直さず、この [`PlaceRules`] の式を使う。
//!
//! 写真 1 枚は、上から順に最初に当てはまった 1 つの区分にだけ入る。
//!
//! 1. `pref` が空でない → 国=日本。`pref` が対応表の 47 件にあればその地域、無い名前なら日本の地域不明。座標は見ない。
//! 2. `pref` が空で座標が無効 → `no_location`。
//! 3. `pref` が空で有効な分類があり国が `inside` → その国。地域が `inside` ならその地域、それ以外は地域不明。
//! 4. それ以外（国が `outside`/`ambiguous`、有効な分類が無い = 処理待ち・`failed`）→ `unclassified`。
//!
//! 日本の地域は、境界データの地域 ID がそのまま都道府県の地域 ID（名称表の 47 件。`pref` 付き）なので、規則 1 の対応表
//! （[`japan_prefectures_sql`]）と規則 3 の `r.region_id` は同じ ID になり、同じ都道府県にまとまる。
//!
//! 式は `photos` の別名 `p`、有効な分類の別名 `r`、都道府県の対応表の別名 `pm` を前提にする。対応表の列名（`pm_pref`・
//! `pm_region`・`pm_search`）は `photos`・`photo_regions` の列と重ならない（既存の絞り込みや表紙の並びは `pref`・`lat` を
//! 修飾せずに書くため、同名の列があると曖昧になる）。表の無い DB（[`super::available`] が偽）では、列だけを持つ空の
//! 派生表を `r` にして同じ式を使える（**API から表を作らない・書かない**）。

use std::sync::OnceLock;

use super::data::{self, JAPAN_ID};
use super::search::search_text;
use super::table;

/// 規則の SQL 片。`from` は `SELECT … FROM {from} WHERE …` の形で使う（`photos` は `p`）。
#[derive(Debug, Clone)]
pub struct PlaceRules {
    /// `photos p` と、都道府県の対応表 `pm`、有効な分類 `r` の結合。`p` の行は増えない
    /// （`pm` は `pref` ごとに 1 行、`r` は `photo_id` が主キー）。
    pub from: String,
    /// 国の ID（日本は名称表の日本）。国が決まらない写真（`no_location`・`unclassified`）は NULL。
    pub country: String,
    /// 地域の ID。国が決まっても地域が決まらない写真（地域不明）と、国が決まらない写真は NULL。
    pub region: String,
    /// 規則 1: `pref` が空でない（日本の `pref` の写真）。既存の `ABROAD` の否定と同じ式。
    pub has_pref: String,
    /// 規則 2: 区分が `no_location`（`pref` が空で座標が無効）。
    pub no_location: String,
    /// 規則 4: 区分が `unclassified`（`pref` が空で座標は有効なのに国が決まらない）。
    pub unclassified: String,
    /// 検索用の文字列。`pref` のある写真は日本と都道府県の別名、それ以外は有効な分類の `search_text`（無ければ空）。
    pub search: String,
    /// `{search} LIKE ? ESCAPE '\'` と同じ意味の、1 語ぶんの検索条件。`?` を [`SEARCH_LIKE_PARAMS`] 個持ち、**同じパターンを
    /// 全部にバインドする**。`pm`・`r` を結合せず（`photos p` だけで使える）、写真ごとに長い文字列を舐めない:
    /// 47 都道府県の別名（`pm_search`）を `LIKE` で一度だけ照合して `p.pref IN (…)` で引き、`pref` の無い写真は
    /// 分類の `search_text` に一致する写真の ID の集合（一度だけ作る）に入るかを見て、入るときだけ有効な分類かを確かめる
    /// （1 万件の `COUNT(*)` を `search` の `LIKE` と `r` の結合で数えると約 3.8ms、この条件だと約 2.3ms。変更前の 5 列だけでは約 1.4ms）。
    /// 同値性は試験で確かめる。
    pub search_like: String,
    /// `pref` が空の写真だけを扱う文では、これを使う（都道府県の対応表 `pm` を結合しない）。
    pub outside_pref: OutsidePref,
}

/// [`PlaceRules`] のうち `WHERE NOT has_pref`（規則 1 に当たらない写真だけ）の文で使える式。規則 1 の枝（日本・`pm`）を
/// 持たないので、都道府県の対応表を結合・実体化しない。`pref` のある写真には使わないこと（国・地域が NULL になる）。
#[derive(Debug, Clone)]
pub struct OutsidePref {
    /// `photos p` と有効な分類 `r` の結合（`pm` なし）。
    pub from: String,
    /// 国の ID（規則 3）。決まらなければ NULL。
    pub country: String,
    /// 地域の ID（規則 3）。決まらなければ NULL。
    pub region: String,
    /// 規則 2: 座標が無効（`no_location`）。
    pub no_location: String,
}

/// [`PlaceRules::search_like`] の `?` の数。
pub const SEARCH_LIKE_PARAMS: usize = 3;

/// `pref` が空でない（既存の `ABROAD` の否定。`NULL`・空文字だけが空）。
const HAS_PREF: &str = "(p.pref IS NOT NULL AND p.pref <> '')";

impl PlaceRules {
    /// `classification_available` は [`super::available`] の結果。偽なら有効な分類は常に無い。
    pub fn new(classification_available: bool) -> Self {
        let valid = table::valid_coordinates("p");
        let japan = sql_text(JAPAN_ID);
        let classification = if classification_available {
            // `pref` のある写真は規則 1 で決まるので、分類は引かない（写真ごとの探索を省く）
            format!("{} AND NOT {HAS_PREF}", table::join("p", "r"))
        } else {
            // 表が無い DB。同じ列を持つ空の派生表（一致する行が無い）。表は作らない
            "LEFT JOIN (SELECT NULL AS photo_id, NULL AS country_id, NULL AS region_id, \
             NULL AS country_method, NULL AS region_method, NULL AS search_text WHERE 0) r ON 0"
                .to_owned()
        };
        let from = format!(
            "photos p LEFT JOIN {} pm ON pm.pm_pref = p.pref {classification}",
            japan_prefectures_sql()
        );
        let outside_pref = OutsidePref {
            from: format!("photos p {classification}"),
            country: "(CASE WHEN r.country_method = 'inside' THEN r.country_id END)".to_owned(),
            region: "(CASE WHEN r.country_method = 'inside' AND r.region_method = 'inside' \
                     THEN r.region_id END)"
                .to_owned(),
            no_location: format!("(NOT {valid})"),
        };
        let country = format!(
            "(CASE WHEN {HAS_PREF} THEN {japan} WHEN r.country_method = 'inside' THEN r.country_id END)"
        );
        let region = format!(
            "(CASE WHEN {HAS_PREF} THEN pm.pm_region \
             WHEN r.country_method = 'inside' AND r.region_method = 'inside' THEN r.region_id END)"
        );
        let no_location = format!("(NOT {HAS_PREF} AND NOT {valid})");
        // 国の式の補集合（`inside` なのに国の ID が無い壊れた行も取りこぼさず、ここに入れる）
        let unclassified = format!("(NOT {HAS_PREF} AND {valid} AND {country} IS NULL)");
        let japan_search = sql_text(&search_text(JAPAN_ID, None).unwrap_or_default());
        let search = format!(
            "(CASE WHEN {HAS_PREF} THEN COALESCE(pm.pm_search, {japan_search}) \
             ELSE COALESCE(r.search_text, '') END)"
        );
        // 1 語ぶんの検索条件。`pref` が 47 件にあれば都道府県の別名（`pm_search`）、無ければ日本の別名だけ、
        // `pref` が空なら有効な分類の `search_text`（`search` の CASE と同じ分岐）。分類の表が無い DB では、
        // 分類の分は偽（`?` の数をそろえるため、使わない `?` を置く）
        let like = "LIKE ? ESCAPE '\\'";
        let classified = if classification_available {
            format!(
                "p.id IN (SELECT sr.photo_id FROM photo_regions sr WHERE sr.search_text {like}) \
                 AND EXISTS (SELECT 1 FROM photo_regions sr WHERE {})",
                table::valid_on("p", "sr")
            )
        } else {
            "0 AND ? IS NULL".to_owned()
        };
        let search_like = format!(
            "(({HAS_PREF} AND (p.pref IN (SELECT pm_pref FROM {pm} WHERE pm_search {like}) \
             OR (p.pref NOT IN ({names}) AND {japan_search} {like}))) \
             OR (NOT {HAS_PREF} AND {classified}))",
            pm = japan_prefectures_sql(),
            names = japan_pref_names_sql(),
        );
        Self {
            from,
            country,
            region,
            has_pref: HAS_PREF.to_owned(),
            no_location,
            unclassified,
            search,
            search_like,
            outside_pref,
        }
    }
}

/// 日本の `pref` の表記（名称表の 47 件）に対する日本の地域の ID。47 件に無い表記（表記揺れなど）は `None`
/// （規則 1: 日本の地域不明）。SQL の式（`pm` の対応表）と同じ名称表から引く。
pub fn pref_region(pref: &str) -> Option<&'static str> {
    data::names()
        .ok()?
        .region_of_pref(pref)
        .map(|r| r.id.as_str())
}

/// 47 都道府県の `pref` の表記を `'東京都','大阪府',…` と並べた文字列（`IN` の中身）。名称表を読めなければ `NULL`。
fn japan_pref_names_sql() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    CELL.get_or_init(|| {
        let list: Vec<String> = data::names()
            .map(|names| {
                names
                    .prefectures()
                    .filter_map(|r| r.pref.as_deref().map(sql_text))
                    .collect()
            })
            .unwrap_or_default();
        if list.is_empty() {
            "NULL".to_owned()
        } else {
            list.join(",")
        }
    })
}

/// SQL の文字列リテラル（`'` は 2 つにする）。引数は同梱データ由来の定数だけで、利用者の入力は渡さない。
fn sql_text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 日本の 47 都道府県の対応表（`pm_pref`・`pm_region`・`pm_search`）を返す派生表。同梱の名称表から 1 回だけ作る
/// 固定の文字列で、利用者の入力を含まない。名称表を読めないときは空の表になる（`pref` のある写真は日本の地域不明）。
fn japan_prefectures_sql() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    CELL.get_or_init(|| {
        let rows: Vec<String> = data::names()
            .map(|names| {
                names
                    .prefectures()
                    .filter_map(|r| {
                        let pref = r.pref.as_deref()?;
                        let search = search_text(JAPAN_ID, Some(&r.id))?;
                        Some(format!(
                            "({},{},{})",
                            sql_text(pref),
                            sql_text(&r.id),
                            sql_text(&search)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if rows.is_empty() {
            "(SELECT NULL AS pm_pref, NULL AS pm_region, NULL AS pm_search WHERE 0)".to_owned()
        } else {
            // `LIMIT` は意味を変えず、派生表を実体化させて自動の索引（`pref` で引く）を使わせるためのもの。無いと写真ごとに
            // 47 行を舐める。`CAST` は `p.pref`（TEXT）と索引の列の型を揃える
            format!(
                "(SELECT CAST(column1 AS TEXT) AS pm_pref, column2 AS pm_region, column3 AS pm_search \
                 FROM (VALUES {}) LIMIT 1000)",
                rows.join(",")
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// 区分 = (国, 地域, 区分名) を式から引く。
    fn classify(
        conn: &Connection,
        rules: &PlaceRules,
    ) -> Vec<(i64, Option<String>, Option<String>, &'static str)> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT p.id, {c}, {g}, CASE WHEN {n} THEN 'no_location' WHEN {u} THEN 'unclassified' ELSE 'placed' END
                   FROM {f} ORDER BY p.id",
                c = rules.country,
                g = rules.region,
                n = rules.no_location,
                u = rules.unclassified,
                f = rules.from
            ))
            .unwrap();
        stmt.query_map([], |row| {
            let kind: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                match kind.as_str() {
                    "no_location" => "no_location",
                    "unclassified" => "unclassified",
                    _ => "placed",
                },
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn memory(photos: &str, regions: &str, with_table: bool) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE photos(id INTEGER PRIMARY KEY, lat REAL, lon REAL, pref TEXT, city TEXT);",
        )
        .unwrap();
        if with_table {
            table::ensure_tables(&mut conn).unwrap();
        }
        conn.execute_batch(photos).unwrap();
        if with_table {
            conn.execute_batch(regions).unwrap();
        }
        conn
    }

    #[test]
    fn rules_one_to_four_each_branch() {
        let version = super::super::classifier_version().unwrap();
        let tokyo = data::names()
            .unwrap()
            .region_of_pref("東京都")
            .unwrap()
            .id
            .clone();
        let photos = "INSERT INTO photos VALUES
            (1, 35.0, 139.0, '東京都', 'A'),   -- 1: pref あり
            (2, NULL, NULL, '東京都', 'A'),    -- 1: pref あり・座標なし
            (3, 35.0, 139.0, '架空県', 'A'),   -- 1: 47 件に無い pref
            (4, NULL, NULL, NULL, NULL),       -- 2: 座標なし
            (5, 95.0, 10.0, '', NULL),         -- 2: 無効な座標
            (6, 48.0, 2.0, NULL, NULL),        -- 3: 地域まで
            (7, 48.0, 2.0, '', NULL),          -- 3: 国のみ
            (8, 36.0, 139.0, NULL, NULL),      -- 3: 日本の境界（地域は都道府県）
            (9, 0.0, 0.0, NULL, NULL),         -- 4: outside
            (10, 1.0, 1.0, NULL, NULL),        -- 4: ambiguous
            (11, 2.0, 2.0, NULL, NULL),        -- 4: 処理待ち（行なし）
            (12, 3.0, 3.0, NULL, NULL),        -- 4: failed
            (13, 4.0, 4.0, NULL, NULL),        -- 4: 古い版
            (14, 5.0, 5.0, NULL, NULL);        -- 4: 座標が違う";
        let regions = format!(
            "INSERT INTO photo_regions VALUES
            (6, 48.0, 2.0, '{version}', 'ne:FRA', 'ne:FRA-1', '', 'inside', 'inside', 't'),
            (7, 48.0, 2.0, '{version}', 'ne:FRA', NULL, '', 'inside', 'outside', 't'),
            (8, 36.0, 139.0, '{version}', 'ne:JPN', '{tokyo}', '', 'inside', 'inside', 't'),
            (9, 0.0, 0.0, '{version}', NULL, NULL, '', 'outside', 'outside', 't'),
            (10, 1.0, 1.0, '{version}', NULL, NULL, '', 'ambiguous', 'outside', 't'),
            (12, 3.0, 3.0, '{version}', NULL, NULL, '', 'failed', 'failed', 't'),
            (13, 4.0, 4.0, 'old', 'ne:FRA', NULL, '', 'inside', 'outside', 't'),
            (14, 6.0, 6.0, '{version}', 'ne:FRA', NULL, '', 'inside', 'outside', 't')"
        );
        let conn = memory(photos, &regions, true);
        let got = classify(&conn, &PlaceRules::new(true));
        let s = |x: &str| Some(x.to_owned());
        let expected = vec![
            (1, s("ne:JPN"), Some(tokyo.clone()), "placed"),
            (2, s("ne:JPN"), Some(tokyo.clone()), "placed"),
            (3, s("ne:JPN"), None, "placed"),
            (4, None, None, "no_location"),
            (5, None, None, "no_location"),
            (6, s("ne:FRA"), s("ne:FRA-1"), "placed"),
            (7, s("ne:FRA"), None, "placed"),
            (8, s("ne:JPN"), Some(tokyo), "placed"),
            (9, None, None, "unclassified"),
            (10, None, None, "unclassified"),
            (11, None, None, "unclassified"),
            (12, None, None, "unclassified"),
            (13, None, None, "unclassified"),
            (14, None, None, "unclassified"),
        ];
        assert_eq!(got, expected);
    }

    #[test]
    fn outside_pref_expressions_equal_the_full_ones_for_photos_without_pref() {
        let version = super::super::classifier_version().unwrap();
        let tokyo = data::names()
            .unwrap()
            .region_of_pref("東京都")
            .unwrap()
            .id
            .clone();
        let photos = "INSERT INTO photos VALUES
            (1, 35.0, 139.0, '東京都', 'A'), (2, NULL, NULL, NULL, NULL), (3, 95.0, 10.0, '', NULL),
            (4, 48.0, 2.0, NULL, NULL), (5, 48.0, 2.0, '', NULL), (6, 36.0, 139.0, NULL, NULL),
            (7, 0.0, 0.0, NULL, NULL), (8, 3.0, 3.0, NULL, NULL), (9, 2.0, 2.0, NULL, NULL)";
        let regions = format!(
            "INSERT INTO photo_regions VALUES
            (4, 48.0, 2.0, '{version}', 'ne:FRA', 'ne:FRA-1', '', 'inside', 'inside', 't'),
            (5, 48.0, 2.0, '{version}', 'ne:FRA', NULL, '', 'inside', 'outside', 't'),
            (6, 36.0, 139.0, '{version}', 'ne:JPN', '{tokyo}', '', 'inside', 'inside', 't'),
            (7, 0.0, 0.0, '{version}', NULL, NULL, '', 'outside', 'outside', 't'),
            (8, 3.0, 3.0, '{version}', NULL, NULL, '', 'failed', 'failed', 't')"
        );
        for with_table in [true, false] {
            let conn = memory(photos, &regions, with_table);
            let rules = PlaceRules::new(with_table);
            let rows = |from: &str,
                        c: &str,
                        r: &str,
                        n: &str|
             -> Vec<(i64, Option<String>, Option<String>, bool)> {
                conn.prepare(&format!(
                    "SELECT p.id, {c}, {r}, {n} FROM {from} WHERE NOT {} ORDER BY p.id",
                    rules.has_pref
                ))
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
            };
            let full = rows(
                &rules.from,
                &rules.country,
                &rules.region,
                &rules.no_location,
            );
            let outside = &rules.outside_pref;
            let slim = rows(
                &outside.from,
                &outside.country,
                &outside.region,
                &outside.no_location,
            );
            assert_eq!(slim, full, "table: {with_table}");
            assert_eq!(slim.len(), 8);
            // 都道府県の対応表を結合しない
            assert!(!outside.from.contains("pm_pref"));
        }
    }

    #[test]
    fn without_the_table_only_pref_photos_are_placed() {
        let photos = "INSERT INTO photos VALUES
            (1, 35.0, 139.0, '東京都', 'A'), (2, NULL, NULL, NULL, NULL),
            (3, 48.0, 2.0, NULL, NULL), (4, 95.0, 2.0, '', NULL)";
        let conn = memory(photos, "", false);
        let got = classify(&conn, &PlaceRules::new(false));
        let kinds: Vec<_> = got.iter().map(|g| (g.0, g.1.is_some(), g.3)).collect();
        assert_eq!(
            kinds,
            [
                (1, true, "placed"),
                (2, false, "no_location"),
                (3, false, "unclassified"),
                (4, false, "no_location")
            ]
        );
        // 表は作られない
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='photo_regions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn search_text_of_pref_photos_names_japan_and_the_prefecture() {
        let photos = "INSERT INTO photos VALUES (1, 35.0, 139.0, '東京都', 'A'), (2, 1.0, 1.0, '架空県', 'A'), (3, 1.0, 1.0, NULL, NULL)";
        let conn = memory(photos, "", true);
        let rules = PlaceRules::new(true);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {} FROM {} ORDER BY p.id",
                rules.search, rules.from
            ))
            .unwrap();
        let got: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            got[0].contains("日本") && got[0].contains("Japan") && got[0].contains("Tokyo"),
            "{}",
            got[0]
        );
        assert!(
            got[1].contains("Japan") && !got[1].contains("Tokyo"),
            "{}",
            got[1]
        );
        assert_eq!(got[2], "");
    }

    /// `search_like` は `search LIKE ?` と同じ写真に当たる（pref あり・47 件に無い pref・分類あり・分類なし・失敗・旧版・
    /// 座標が違う行）。表の有無の両方で。
    #[test]
    fn search_like_matches_the_same_photos_as_the_search_expression() {
        let version = super::super::classifier_version().unwrap();
        let tokyo = data::names()
            .unwrap()
            .region_of_pref("東京都")
            .unwrap()
            .id
            .clone();
        let fr = search_text("ne:FRA", None).unwrap();
        let fr_region = data::names()
            .unwrap()
            .regions
            .iter()
            .find(|r| r.country.as_deref() == Some("ne:FRA"))
            .unwrap()
            .id
            .clone();
        let fr_a = search_text("ne:FRA", Some(&fr_region)).unwrap();
        let jp_tokyo = search_text(JAPAN_ID, Some(&tokyo)).unwrap();
        let photos = "INSERT INTO photos VALUES
            (1, 35.0, 139.0, '東京都', 'A'), (2, NULL, NULL, '大阪府', 'A'), (3, 35.0, 139.0, '架空県', 'A'),
            (4, NULL, NULL, NULL, NULL), (5, 48.0, 2.0, NULL, NULL), (6, 48.0, 2.0, '', NULL),
            (7, 36.0, 139.0, NULL, NULL), (8, 3.0, 3.0, NULL, NULL), (9, 4.0, 4.0, NULL, NULL),
            (10, 5.0, 5.0, NULL, NULL), (11, 6.0, 6.0, NULL, NULL), (12, 48.0, 2.0, '東京都', NULL)";
        let regions = format!(
            "INSERT INTO photo_regions VALUES
            (5, 48.0, 2.0, '{version}', 'ne:FRA', '{fr_region}', '{fr_a}', 'inside', 'inside', 't'),
            (6, 48.0, 2.0, '{version}', 'ne:FRA', NULL, '{fr}', 'inside', 'outside', 't'),
            (7, 36.0, 139.0, '{version}', 'ne:JPN', '{tokyo}', '{jp_tokyo}', 'inside', 'inside', 't'),
            (8, 3.0, 3.0, '{version}', NULL, NULL, '', 'failed', 'failed', 't'),
            (9, 4.0, 4.0, 'old', 'ne:FRA', NULL, '{fr}', 'inside', 'outside', 't'),
            (10, 6.0, 6.0, '{version}', 'ne:FRA', NULL, '{fr}', 'inside', 'outside', 't'),
            (11, 6.0, 6.0, '{version}', NULL, NULL, '', 'outside', 'outside', 't'),
            (12, 48.0, 2.0, '{version}', 'ne:FRA', NULL, '{fr}', 'inside', 'outside', 't')"
        );
        for with_table in [true, false] {
            let conn = memory(photos, &regions, with_table);
            let rules = PlaceRules::new(with_table);
            let ids = |condition: &str, from: &str, pattern: &str| -> Vec<i64> {
                conn.prepare(&format!(
                    "SELECT p.id FROM {from} WHERE {condition} ORDER BY p.id"
                ))
                .unwrap()
                .query_map(
                    rusqlite::params_from_iter(vec![pattern; SEARCH_LIKE_PARAMS]),
                    |row| row.get(0),
                )
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
            };
            for word in [
                "Tokyo",
                "東京",
                "日本",
                "Japan",
                "Osaka",
                "大阪",
                "France",
                "フランス",
                "Île",
                "架空",
                "e",
                "x",
                "zzz",
                "tokyo",
                "TOKYO",
            ] {
                let pattern = format!("%{word}%");
                let by_expression = conn
                    .prepare(&format!(
                        "SELECT p.id FROM {} WHERE {} LIKE ?1 ESCAPE '\\' ORDER BY p.id",
                        rules.from, rules.search
                    ))
                    .unwrap()
                    .query_map([&pattern], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                // 何も結合しない `photos p` だけでよい
                let by_condition = ids(&rules.search_like, "photos p", &pattern);
                assert_eq!(by_condition, by_expression, "{word} table={with_table}");
                // 結合していても同じ
                assert_eq!(
                    ids(&rules.search_like, &rules.outside_pref.from, &pattern),
                    by_expression,
                    "{word} table={with_table} (r)"
                );
                assert_eq!(
                    ids(&rules.search_like, &rules.from, &pattern),
                    by_expression,
                    "{word} table={with_table} (pm)"
                );
            }
            // 日本の語は pref のある写真（47 件に無い pref も）に当たる
            let japan = ids(&rules.search_like, &rules.from, "%Japan%");
            assert!(japan.contains(&1) && japan.contains(&3) && japan.contains(&12));
        }
    }

    #[test]
    fn the_prefecture_table_has_all_47_and_quotes_are_doubled() {
        assert_eq!(japan_prefectures_sql().matches("'ne:JPN-").count(), 47);
        assert_eq!(sql_text("a'b"), "'a''b'");
    }
}
