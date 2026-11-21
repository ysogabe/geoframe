//! note 記事用の写真書き出しに向けた、期間つきの写真の一覧
//! （`GET /api/admin/export/photos?from=YYYY-MM-DD&to=YYYY-MM-DD`。docs/note-photo-export/design.md の 6.4）。
//!
//! 期間に撮った、非表示でない**全写真**を返す。写真フィード（`GET /api/v1/photos`）と違い、位置の無い写真、
//! 重複・類似の選別で非推奨になった写真、表示用 JPEG がまだ無い写真（`file: null`）も含める
//! （選別の表は読まない）。外のプロセス（`geoframe-export`）が HTTP で読む。
//!
//! - 読み取りだけ。書き込み・外部プロセス・外部への通信は無い。経路は完全一致の `GET` のみで、`server.rs` の
//!   `site` が呼ぶ（既存の管理 API と同じ区域・同じ守り。HEAD・POST は 403）。
//! - クエリ: `from`・`to` はどちらも必須で、実在する日付（`YYYY-MM-DD`）。`from <= to`、期間は両端を含めて
//!   [`SPAN_DAYS_MAX`] 日まで。ほかのキー、同じキーの繰り返し、空の値は 400 `bad_params`（値は応答にもログにも出さない）。
//!   既存の管理 API の `parse_qs` の読み方（最初の値だけ使い、空の値は無かったことにする）は使わない。
//! - 対象: 非表示でなく、撮影日時が分までの形（`admin_library` の `TIMED` と同じ）で、`from 00:00`〜`to 23:59`
//!   （両端を含む）に入る写真。並びは `taken_at`、`id` の昇順。`idx_photos_time` の範囲検索で引く。
//! - 1 件: `id`・`file`（`photos.display`。無ければ `null`）・`title`（フィードと同じ [`photo_title`]）・`lat`・`lon`
//!   （無ければ `null`。キーは省かない）・`time`・`elev`（無ければ `null`）・`inferred`（`loc_source == "inferred"`）。
//!   列の値はそのまま出す。受け取る側が検査し直す（design 4.4）。
//! - 応答: `{from, to, photos, undated, invalid_time, truncated}`。`undated` は撮影日時が NULL か空の、`invalid_time` は
//!   空でないが分までの形でない、非表示でない写真の数（カタログ全体。期間では絞らない）。
//!   分までの形でも暦として存在しない日時（`2030-02-30 10:00`）は形の検査を通るので、期間に入れば `photos` に出て、
//!   `invalid_time` には数えない（書き出し側が検査する）。上限は [`PHOTOS_MAX`] 件で、超えたら先頭の
//!   [`PHOTOS_MAX`] 件と `truncated: true`。

use axum::response::Response;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::admin_library::{TIMED, VISIBLE, bad_params, respond};
use crate::photos::{parse_epoch, photo_title};
use crate::server::AppState;
use crate::webpath::percent_decode;

/// 1 回で返す写真の上限。
pub const PHOTOS_MAX: usize = 2_000;
/// 期間の長さの上限（日数。`from` と `to` の両端を含む）。
pub const SPAN_DAYS_MAX: i64 = 14;

/// 期間の指定（`YYYY-MM-DD` の 2 つ）。
#[derive(Debug, PartialEq, Eq)]
struct Range {
    from: String,
    to: String,
}

/// `%XX` と `+` を戻した文字列（`pyquery` と同じ読み方）。
fn unquote(s: &str) -> String {
    String::from_utf8_lossy(&percent_decode(&s.replace('+', " "))).into_owned()
}

/// `YYYY-MM-DD`（各桁が ASCII の数字の 10 文字）で、暦に実在する日付。`parse_epoch` は 1〜2 桁の月日や余分な空白も
/// 通すので、形は先に自分で見る。
fn parse_day(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    parse_epoch(&format!("{s} 00:00"))
}

/// クエリを読む。`from`・`to` のほかのキー、同じキーの繰り返し、欠けた・空の値、実在しない日付、`from > to`、
/// [`SPAN_DAYS_MAX`] 日を超える期間は `None`（400）。
fn parse_range(query: Option<&str>) -> Option<Range> {
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    for pair in query.unwrap_or("").split('&') {
        if pair.is_empty() {
            // `a=1&&b=2` や末尾の `&` は、何も言っていない項目として読み飛ばす（キーも値も無い）
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let slot = match unquote(name).as_str() {
            "from" => &mut from,
            "to" => &mut to,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(unquote(value));
    }
    let (from, to) = (from?, to?);
    let (start, end) = (parse_day(&from)?, parse_day(&to)?);
    if start > end || (end - start) / 86_400 + 1 > SPAN_DAYS_MAX {
        return None;
    }
    Some(Range { from, to })
}

/// `GET /api/admin/export/photos`。
pub async fn photos(state: &AppState, query: Option<&str>) -> Response {
    let Some(range) = parse_range(query) else {
        return bad_params();
    };
    respond(state.catalog.read(move |conn| build(conn, &range)).await)
}

/// 期間の写真を引く SQL。`LIMIT` は上限より 1 件多く取り、`truncated` の判定に使う。
/// 文字列の比較なので、日時なしや形の違う値が範囲に紛れ込まないよう、分までの形の行（[`TIMED`]）だけにする。
/// `ORDER BY` は列そのもの（式にしない）で、`idx_photos_time`（`taken_at`、同じ値は行 ID の順）の走査をそのまま使う。
fn list_sql() -> String {
    format!(
        "SELECT id, display, title, city, place, name, lat, lon, taken_at, elev, loc_source FROM photos \
         WHERE {VISIBLE} AND taken_at >= ?1 AND taken_at <= ?2 AND {TIMED} \
         ORDER BY taken_at, id LIMIT ?3"
    )
}

fn build(conn: &Connection, range: &Range) -> rusqlite::Result<Value> {
    // 一覧と件数が同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let (from, to) = (
        format!("{} 00:00", range.from),
        format!("{} 23:59", range.to),
    );

    let mut stmt = conn.prepare(&list_sql())?;
    let mut rows = stmt.query(params![from, to, (PHOTOS_MAX + 1) as i64])?;
    let mut photos = Vec::new();
    while let Some(row) = rows.next()? {
        let text = |col: usize| -> rusqlite::Result<String> {
            Ok(row.get::<_, Option<String>>(col)?.unwrap_or_default())
        };
        photos.push(json!({
            "id": row.get::<_, i64>(0)?,
            "file": row.get::<_, Option<String>>(1)?,
            "title": photo_title(&text(2)?, &text(3)?, &text(4)?, &row.get::<_, String>(5)?),
            "lat": row.get::<_, Option<f64>>(6)?,
            "lon": row.get::<_, Option<f64>>(7)?,
            "time": text(8)?,
            "elev": row.get::<_, Option<f64>>(9)?,
            "inferred": row.get::<_, Option<String>>(10)?.as_deref() == Some("inferred"),
        }));
    }
    let truncated = photos.len() > PHOTOS_MAX;
    photos.truncate(PHOTOS_MAX);

    let (undated, invalid_time) = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(taken_at IS NULL OR taken_at = ''), 0),
                    COALESCE(SUM(taken_at IS NOT NULL AND taken_at <> '' AND NOT ({TIMED})), 0)
               FROM photos WHERE {VISIBLE}"
        ),
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;

    Ok(json!({
        "from": range.from,
        "to": range.to,
        "photos": photos,
        "undated": undated,
        "invalid_time": invalid_time,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(query: &str) -> Option<Range> {
        parse_range(Some(query))
    }

    fn range(from: &str, to: &str) -> Option<Range> {
        Some(Range {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }

    #[test]
    fn range_accepts_real_dates_up_to_the_span_limit() {
        assert_eq!(
            r("from=2030-03-01&to=2030-03-01"),
            range("2030-03-01", "2030-03-01")
        );
        // 順序と、%・+ の符号化は問わない
        assert_eq!(
            r("to=2030-03-14&from=2030-03-01"),
            range("2030-03-01", "2030-03-14")
        );
        assert_eq!(
            r("from=2030-03-01&to=2030-03-14&"),
            range("2030-03-01", "2030-03-14")
        );
        assert_eq!(
            r("%66rom=2030%2D03-01&to=2030-03-02"),
            range("2030-03-01", "2030-03-02")
        );
        // 月またぎ・年またぎ・閏日
        assert_eq!(
            r("from=2030-12-25&to=2031-01-07"),
            range("2030-12-25", "2031-01-07")
        );
        assert_eq!(
            r("from=2032-02-29&to=2032-02-29"),
            range("2032-02-29", "2032-02-29")
        );
    }

    #[test]
    fn range_rejects_everything_else() {
        for q in [
            "",
            "from=2030-03-01",
            "to=2030-03-01",
            // 15 日
            "from=2030-03-01&to=2030-03-15",
            // 逆順
            "from=2030-03-02&to=2030-03-01",
            // 実在しない日付
            "from=2030-02-30&to=2030-03-01",
            "from=2031-02-29&to=2031-03-01",
            "from=2030-13-01&to=2030-13-02",
            "from=2030-00-10&to=2030-01-10",
            "from=0000-01-01&to=0000-01-02",
            // 形が違う
            "from=2030-3-1&to=2030-3-2",
            "from=20300301&to=20300302",
            "from=2030-03-01%2000:00&to=2030-03-02",
            "from=2030-03-01&to=2030-03-02%20",
            "from=%EF%BC%92030-03-01&to=2030-03-02",
            "from=&to=2030-03-02",
            "from&to=2030-03-02",
            // 未知のキー・重複（値が同じでも）
            "from=2030-03-01&to=2030-03-02&x=1",
            "from=2030-03-01&to=2030-03-02&hidden=1",
            "from=2030-03-01&from=2030-03-01&to=2030-03-02",
            "from=2030-03-01&to=2030-03-02&to=2030-03-02",
            "From=2030-03-01&to=2030-03-02",
            "from=2030-03-01&to=2030-03-02&=",
        ] {
            assert_eq!(r(q), None, "{q}");
        }
        assert_eq!(parse_range(None), None);
    }

    /// 期間の検索が撮影時刻の索引（範囲検索）で引け、並べ替えの一時領域を使わないこと。
    #[test]
    fn list_sql_uses_the_time_index_without_a_sort() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::schema::SCHEMA).unwrap();
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", list_sql()))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params!["a", "b", 1_i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let plan = plan.join(" | ");
        assert!(plan.contains("idx_photos_time"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}
