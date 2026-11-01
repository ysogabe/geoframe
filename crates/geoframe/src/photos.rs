//! `GET /api/v1/photos` の中身: DB から写真を選び、並べ、タイトルと旅番号を付ける（T1.2）。
//!
//! 現行 Python の `app/geoframe/photos.py:api_photos` / `assign_trips` を、同じ行・同じ並び・同じ
//! 値になるように移したもの。SQL（品質・AI の JOIN と表示条件）は、`ORDER BY` が `taken_at` の NULL を
//! 空文字として扱う点（[`feed`] のモジュール説明）と、旧 AI 選別の抑止条件（`AI_VISIBLE`）を除いて同じ。
//! 抑止条件は P4 / T4.3 で安全にした意図した差で、メンバーの世代と、表示可能な推薦写真が同じグループに
//! あることを検査する（[`crate::selection`]）。管理画面の集計と同じ SQL を共有する。
//! 応答の型は [`crate::feed::Photo`]。
//!
//! # 同順位（同じ `taken_at`）の並び
//!
//! `ORDER BY` が `taken_at` だけなので、同じ撮影日時の写真どうしの並びは SQLite の実行計画
//! （ソートの実装と、`LEFT JOIN` の走査順）に依存する。Python と同じ SQL なので通常は同じ並びに
//! なるが、リンクする SQLite のバージョンが違えば変わりうる。保証はせず、T1.5 の本番コピーでの
//! 全件比較で確かめる（差が出たら `ORDER BY` に `p.id` を足す等で直す）。
//!
//! # Python との意図した差と残る差
//!
//! - **`taken_at` が NULL**: Python は `time: null` で先頭に並べる。ここでは空文字として「時刻
//!   不明」と同じに扱い、最後に並べる（契約は `time` を文字列とする）。
//! - **`lon` が NULL**（`lat` はある）: Python は、同じ旅に他の写真があると `assign_trips` の
//!   `min(i["lon"] ...)` が `TypeError` になって**一覧全体が 500**、その行だけの旅のときは
//!   `lon: null` を出す。契約は数値なので、Rust はこの行を一覧から外し、写真IDだけをログに出す
//!   （miharashi は数値でない `lon` を「位置なし」として扱うので、見え方は `lon: null` と同じ）。
//!   通常は `setloc` が `lat`/`lon` を一緒に書くので起きない。
//! - **時刻の解釈**: Python の `t_epoch` は `strptime(...).timestamp()` でローカル時刻として
//!   解釈する。ここでは暦だけで数える（[`parse_epoch`]）。unit が与える `TZ=Asia/Tokyo` には
//!   夏時間が無いので、2つの時刻の差は同じ。夏時間のある `TZ` では、Python は切り替えをまたぐ
//!   差が1時間ずれる。
//! - 列の型が想定と違う（`lat` が文字列など）行があると、その行ではなく**読み取り全体が失敗**
//!   して 503 になる（Python は動的型なので素通しする）。スキーマの型が守られている限り起きない。
//! - ほかは Python と同じ。文字列の「空なら次へ」（`or`）、`bool()` の真偽（0・NULL・空文字は偽）、
//!   `str.strip()`、`pathlib.Path.stem` の規則までそろえてある。

use rusqlite::types::{Type, ValueRef};
use rusqlite::{Connection, Row};

use crate::feed::{Photo, QualityKind};

/// 品質結果は「同じ原本・同じ表示画像」にだけ適用する（`photos.py:QUALITY_JOIN`）。
const QUALITY_JOIN: &str = "LEFT JOIN photo_quality q
  ON q.photo_id=p.id AND q.display=p.display
 AND q.source_md5 IS p.md5 AND q.source_mtime IS p.mtime";
const QUALITY_COLUMNS: &str = "q.score AS quality_score, q.group_id AS quality_group,
  q.group_kind AS quality_kind, q.rank_in_group AS quality_rank,
  q.recommended AS quality_recommended";
const QUALITY_VISIBLE: &str = "(q.group_kind IS NULL OR q.group_kind!='exact' OR q.recommended=1)";
const AI_JOIN: &str = "LEFT JOIN photo_ai_group_members aim ON aim.photo_id=p.id
LEFT JOIN photo_ai_groups aig ON aig.group_id=aim.group_id";
const AI_COLUMNS: &str = "aig.group_id AS ai_group, aig.status AS ai_status,
  aig.same_scene AS ai_same_scene, aim.rank_in_group AS ai_rank,
  aim.recommended AS ai_recommended, aim.local_score AS ai_local_score";
/// 旅を区切る間隔。これを**超える**空きで次の旅になる（ちょうど48時間は同じ旅）。
pub(crate) const TRIP_GAP_SECONDS: i64 = 48 * 3600;

/// 一覧に出す写真を、出す順に返す（旅番号つき）。`Catalog::read` の中で呼ぶ。
pub fn list_photos(conn: &Connection) -> rusqlite::Result<Vec<Photo>> {
    // Python の `p.*` の代わりに使う列だけを並べる（行と並びは同じ）。ORDER BY の
    // `COALESCE(p.taken_at,'')` は NULL を空文字として最後に回すための唯一の違い。
    // 旧選別の抑止条件。メンバーの世代と表示可能な推薦写真の存在を検査する（[`crate::selection`]）
    let ai_visible = crate::selection::ai_visible();
    let sql = format!(
        "SELECT p.id, p.fetched_at, p.favorite, p.display, p.name, p.title, p.lat, p.lon,
                p.taken_at, p.place, p.pref, p.city, p.elev, p.loc_source,
                {QUALITY_COLUMNS}, {AI_COLUMNS} FROM photos p
         {QUALITY_JOIN}
         {AI_JOIN}
         WHERE p.lat IS NOT NULL AND p.display IS NOT NULL
           AND COALESCE(p.hidden,0)=0 AND {QUALITY_VISIBLE} AND {ai_visible}
         ORDER BY CASE WHEN COALESCE(p.taken_at,'')='' THEN 1 ELSE 0 END, COALESCE(p.taken_at,'')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut photos = Vec::new();
    while let Some(row) = rows.next()? {
        if let Some(photo) = photo_from_row(row)? {
            photos.push(photo);
        }
    }
    assign_trips(&mut photos);
    Ok(photos)
}

/// 行を [`Photo`] にする。`trip` は後で [`assign_trips`] が付けるので 0。`lon` が NULL の行は
/// `None`（外す。モジュール説明）。
fn photo_from_row(row: &Row<'_>) -> rusqlite::Result<Option<Photo>> {
    let id: i64 = row.get("id")?;
    let Some(lon) = row.get::<_, Option<f64>>("lon")? else {
        tracing::warn!("写真 {id}: lon が NULL なので一覧から外します");
        return Ok(None);
    };
    let text = |name: &str| -> rusqlite::Result<String> {
        Ok(row.get::<_, Option<String>>(name)?.unwrap_or_default())
    };
    let city = text("city")?;
    let place = text("place")?;
    let title = {
        let manual = text("title")?;
        if !manual.is_empty() {
            manual
        } else {
            let from_place = if city.is_empty() {
                String::new()
            } else {
                py_strip(&format!("{city} {place}")).to_string()
            };
            if from_place.is_empty() {
                file_stem(&row.get::<_, String>("name")?).to_string()
            } else {
                from_place
            }
        }
    };

    let kind = match text("quality_kind")?.as_str() {
        "" => QualityKind::None,
        "exact" => QualityKind::Exact,
        "similar" => QualityKind::Similar,
        // photo_quality.group_kind は CHECK で exact/similar だけ。Python は素通しするが、型に
        // 無い値は出せないので、読み取りを失敗させる（値はエラーに入れない）
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                Type::Text,
                "quality_kind が exact/similar ではありません".into(),
            ));
        }
    };
    let in_quality_group = kind != QualityKind::None;
    let quality_recommended = truthy(row, "quality_recommended")?;

    let ai_group = text("ai_group")?;
    let in_ai_group = !ai_group.is_empty();

    Ok(Some(Photo {
        id,
        fetched_at: text("fetched_at")?,
        fav: truthy(row, "favorite")?,
        file: row.get("display")?,
        title,
        lat: row.get("lat")?,
        lon,
        time: text("taken_at")?,
        place,
        pref: text("pref")?,
        city,
        elev: row.get("elev")?,
        inferred: row.get::<_, Option<String>>("loc_source")?.as_deref() == Some("inferred"),
        quality_score: if in_quality_group {
            row.get("quality_score")?
        } else {
            None
        },
        quality_group: if in_quality_group {
            row.get("quality_group")?
        } else {
            None
        },
        quality_kind: kind,
        quality_rank: if in_quality_group {
            row.get("quality_rank")?
        } else {
            None
        },
        quality_recommended: in_quality_group && quality_recommended,
        quality_role: match (in_quality_group, quality_recommended) {
            (false, _) => "",
            (true, true) => "recommended",
            (true, false) => "alternate",
        }
        .to_string(),
        ai_group,
        ai_status: text("ai_status")?,
        ai_same_scene: in_ai_group && truthy(row, "ai_same_scene")?,
        ai_rank: if in_ai_group {
            row.get("ai_rank")?
        } else {
            None
        },
        ai_recommended: in_ai_group && truthy(row, "ai_recommended")?,
        ai_local_score: if in_ai_group {
            row.get("ai_local_score")?
        } else {
            None
        },
        trip: 0,
    }))
}

/// Python の `bool(値)` と同じ真偽: NULL・0・0.0・空の文字列/BLOB は偽。
fn truthy(row: &Row<'_>, name: &str) -> rusqlite::Result<bool> {
    Ok(match row.get_ref(name)? {
        ValueRef::Null => false,
        ValueRef::Integer(v) => v != 0,
        ValueRef::Real(v) => v != 0.0,
        ValueRef::Text(v) | ValueRef::Blob(v) => !v.is_empty(),
    })
}

/// Python の `str.isspace` に近い空白（Unicode の空白＋ `\x1c`〜`\x1f`）。
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python の `str.strip()`。
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `pathlib.PurePosixPath(name).stem`。最後の要素（空と `.` の要素は無いものとして扱う）から、
/// 最後の `.` 以降を取り除く。ただし `.` が先頭（`.hidden`）か末尾（`name.`）にあるときは取り除かない。
/// `std::path::Path::file_stem` とは末尾が `.` の名前などで結果が違うので使わない。
pub fn file_stem(name: &str) -> &str {
    let last = name
        .split('/')
        .rfind(|part| !part.is_empty() && *part != ".")
        .unwrap_or("");
    match last.rfind('.') {
        Some(i) if 0 < i && i < last.len() - 1 => &last[..i],
        _ => last,
    }
}

/// 撮影時刻（`YYYY-MM-DD HH:MM`）を、ある基準からの秒数にする。読めなければ `None`。
///
/// Python の `t_epoch`（`datetime.strptime(s, "%Y-%m-%d %H:%M").timestamp()`）に合わせて、
/// `strptime` が通す形を同じに通す: 月・日・時・分は1〜2桁、日付と時刻のあいだは1つ以上の空白、
/// 前後に余分な文字は不可、存在しない日付（2月30日・閏年でない年の2月29日・0000年）は不可。
/// 数字は ASCII のみ（Python は全角数字なども通す。`norm_time` は全角数字を正規化しないので
/// 差になりうる。P2 の Rust のインデクサで保存時に正規化して解消する。docs/rust-migration/server.md §8.2）。
///
/// 返す秒数は暦だけで数えたもの（タイムゾーンも夏時間も無し）。使い道は2つの時刻の差だけ。
pub fn parse_epoch(s: &str) -> Option<i64> {
    let mut cur = Cursor { rest: s };
    let year = cur.year()?;
    cur.literal('-')?;
    let month = cur.field(1, 12, 1, 9)?;
    cur.literal('-')?;
    // 日だけは、空白に1桁を続けた形（`-%d` に ` 5`）も strptime が通す
    let day = if cur.rest.starts_with(' ') {
        cur.rest = &cur.rest[1..];
        let d = cur.digit(0).filter(|d| (1..=9).contains(d))?;
        cur.rest = &cur.rest[1..];
        d
    } else {
        cur.field(1, 31, 1, 9)?
    };
    cur.whitespace()?;
    let hour = cur.field(0, 23, 0, 9)?;
    cur.literal(':')?;
    let minute = cur.field(0, 59, 0, 9)?;
    if !cur.rest.is_empty() || day > days_in_month(year, month) {
        return None;
    }
    Some(
        days_from_civil(year, month, day) * 86400 + i64::from(hour) * 3600 + i64::from(minute) * 60,
    )
}

struct Cursor<'a> {
    rest: &'a str,
}

impl Cursor<'_> {
    fn digit(&self, at: usize) -> Option<u32> {
        self.rest
            .as_bytes()
            .get(at)
            .filter(|b| b.is_ascii_digit())
            .map(|b| u32::from(b - b'0'))
    }

    fn literal(&mut self, c: char) -> Option<()> {
        self.rest = self.rest.strip_prefix(c)?;
        Some(())
    }

    /// 1つ以上の空白（Python の `\s+`）。
    fn whitespace(&mut self) -> Option<()> {
        let trimmed = self.rest.trim_start_matches(py_space);
        (trimmed.len() < self.rest.len()).then(|| self.rest = trimmed)
    }

    /// 4桁の年（0000 は存在しない）。
    fn year(&mut self) -> Option<i32> {
        let digits = (0..4)
            .map(|k| self.digit(k))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .fold(0, |acc, d| acc * 10 + d);
        self.rest = &self.rest[4..];
        (digits >= 1).then_some(digits as i32)
    }

    /// 2桁（`two_min..=two_max` の範囲内）か1桁（`one_min..=one_max`）。2桁を先に試す。
    fn field(&mut self, two_min: u32, two_max: u32, one_min: u32, one_max: u32) -> Option<u32> {
        if let (Some(a), Some(b)) = (self.digit(0), self.digit(1)) {
            let v = a * 10 + b;
            if (two_min..=two_max).contains(&v) {
                self.rest = &self.rest[2..];
                return Some(v);
            }
        }
        let v = self.digit(0).filter(|v| (one_min..=one_max).contains(v))?;
        self.rest = &self.rest[1..];
        Some(v)
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 1970-01-01 からの日数（グレゴリオ暦）。
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 撮影時刻の間隔（48時間超）で旅に分け、各写真に旅番号（0から）を付ける。
///
/// 写真は時系列に並んでいる前提（[`list_photos`] の `ORDER BY`）。時刻が読めない写真は、それ
/// だけで1つの旅になる（前後の写真とはつながない）。その次の読める写真は、時刻不明の写真を
/// 飛ばして**直前の読めた時刻**との差で決まる。
pub fn assign_trips(photos: &mut [Photo]) {
    let mut trip: i64 = -1;
    let mut last: Option<i64> = None;
    for photo in photos {
        let epoch = parse_epoch(&photo.time);
        let starts_new_trip = match (epoch, last) {
            (Some(e), Some(l)) => trip < 0 || e - l > TRIP_GAP_SECONDS,
            _ => true,
        };
        if starts_new_trip {
            trip += 1;
        }
        photo.trip = trip;
        if epoch.is_some() {
            last = epoch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(time: &str) -> Photo {
        Photo {
            id: 1,
            fetched_at: String::new(),
            fav: false,
            file: "derivatives/a.jpg".into(),
            title: "t".into(),
            lat: 35.0,
            lon: 139.0,
            time: time.into(),
            place: String::new(),
            pref: String::new(),
            city: String::new(),
            elev: None,
            inferred: false,
            quality_score: None,
            quality_group: None,
            quality_kind: QualityKind::None,
            quality_rank: None,
            quality_recommended: false,
            quality_role: String::new(),
            ai_group: String::new(),
            ai_status: String::new(),
            ai_same_scene: false,
            ai_rank: None,
            ai_recommended: false,
            ai_local_score: None,
            trip: 0,
        }
    }

    fn trips(times: &[&str]) -> Vec<i64> {
        let mut photos: Vec<Photo> = times.iter().map(|t| photo(t)).collect();
        assign_trips(&mut photos);
        photos.iter().map(|p| p.trip).collect()
    }

    #[test]
    fn stem_follows_pathlib() {
        // 値は Python 3.13 の PurePosixPath(...).stem で確かめたもの
        for (name, stem) in [
            ("IMG_0001.jpg", "IMG_0001"),
            ("a.b.jpg", "a.b"),
            ("noext", "noext"),
            ("name.", "name."),
            ("x..", "x.."),
            ("..", ".."),
            (".hidden", ".hidden"),
            (".hidden.jpg", ".hidden"),
            ("dir/sub/IMG_1.JPG", "IMG_1"),
            ("dir/IMG_1.jpg/", "IMG_1"),
            ("dir//IMG_1.jpg", "IMG_1"),
            ("./IMG_1.jpg", "IMG_1"),
            ("dir/./IMG_1.jpg", "IMG_1"),
            ("dir/.", "dir"),
            ("a.JPG.jpg", "a.JPG"),
            ("日本語.png", "日本語"),
            ("日本語.", "日本語."),
            ("", ""),
            ("/", ""),
            (".", ""),
        ] {
            assert_eq!(file_stem(name), stem, "{name:?}");
        }
    }

    #[test]
    fn python_strip_whitespace() {
        assert_eq!(py_strip(" \t見本市 \u{3000}\u{a0}"), "見本市");
        assert_eq!(py_strip("\x1c見本市\x1f"), "見本市");
        assert_eq!(py_strip("  "), "");
    }

    #[test]
    fn parse_epoch_reads_what_strptime_reads() {
        let base = parse_epoch("2025-06-01 10:00").unwrap();
        assert_eq!(parse_epoch("2025-06-01 10:01"), Some(base + 60));
        assert_eq!(parse_epoch("2025-06-02 10:00"), Some(base + 86400));
        assert_eq!(parse_epoch("1970-01-01 00:00"), Some(0));
        assert_eq!(parse_epoch("2000-03-01 00:00"), Some(951_868_800));
        // 閏年
        assert!(parse_epoch("2024-02-29 00:00").is_some());
        assert!(parse_epoch("2000-02-29 00:00").is_some());
        assert_eq!(
            parse_epoch("2024-03-01 00:00").unwrap() - parse_epoch("2024-02-29 00:00").unwrap(),
            86400
        );
        // 桁が少なくても、日付と時刻のあいだが複数の空白でも strptime は通す
        assert_eq!(parse_epoch("2025-6-1 10:0"), Some(base));
        assert_eq!(parse_epoch("2025-06-01 1:00"), Some(base - 9 * 3600));
        assert_eq!(parse_epoch("2025-06-01   10:00"), Some(base));
        assert_eq!(parse_epoch("2025-06-01\t10:00"), Some(base));
        assert_eq!(parse_epoch("2025-06- 1 10:00"), Some(base));
        assert_eq!(
            parse_epoch("2025-06-01 23:59").unwrap(),
            base + 13 * 3600 + 59 * 60
        );
        for bad in [
            "",
            "garbage",
            "2025-06-01",
            "2025-06-01 10",
            "2025-06-01 10:00:00",
            "2025-06-01T10:00",
            "2025-06-01 10:00 ",
            " 2025-06-01 10:00",
            "2025-06-01 10:00x",
            "2025-06-01 10:60",
            "2025-06-01 24:00",
            "2025-06- 12 10:00",
            "2025-06-01 10:5x",
            "2025-13-01 10:00",
            "2025-00-01 10:00",
            "2025-06-00 10:00",
            "2025-06-32 10:00",
            "2025-06-31 10:00",
            "2025-02-29 10:00",
            "1900-02-29 10:00",
            "0000-01-01 00:00",
            "25-06-01 10:00",
            "20255-06-01 10:00",
            "2025-06-01-10:00",
            "2025/06/01 10:00",
        ] {
            assert_eq!(parse_epoch(bad), None, "{bad:?}");
        }
        // 既知の差: Python の strptime は全角数字も通すが、ここでは読めない扱い
        assert_eq!(parse_epoch("２０２５-06-01 10:00"), None);
    }

    #[test]
    fn trip_boundary_is_strictly_more_than_48_hours() {
        // ちょうど48時間は同じ旅、1分超で次の旅
        assert_eq!(trips(&["2025-06-01 10:00", "2025-06-03 10:00"]), [0, 0]);
        assert_eq!(trips(&["2025-06-01 10:00", "2025-06-03 10:01"]), [0, 1]);
        assert_eq!(trips(&["2025-06-01 10:00", "2025-06-03 09:59"]), [0, 0]);
        // 間隔は直前の写真との差（全体の長さではない）
        assert_eq!(
            trips(&[
                "2025-06-01 10:00",
                "2025-06-03 10:00",
                "2025-06-05 10:00",
                "2025-06-07 10:01"
            ]),
            [0, 0, 0, 1]
        );
        assert_eq!(trips(&[]), Vec::<i64>::new());
        assert_eq!(trips(&["2025-06-01 10:00"]), [0]);
    }

    #[test]
    fn unknown_times_are_their_own_trips() {
        // 時刻不明は1枚ごとに別の旅
        assert_eq!(trips(&["2025-06-01 10:00", "", ""]), [0, 1, 2]);
        // 読めない文字列も不明と同じ
        assert_eq!(trips(&["2025-06-01 10:00", "garbage"]), [0, 1]);
        // 先頭が不明でも番号は0から
        assert_eq!(trips(&["", "2025-06-01 10:00"]), [0, 1]);
        // 不明を挟んだ次の写真は、不明な写真ではなく直前の読めた時刻との差で決まる。
        // 差が48時間以内なら、不明な写真と同じ旅に入る（Python と同じ）
        assert_eq!(
            trips(&["2025-06-01 10:00", "", "2025-06-02 10:00"]),
            [0, 1, 1]
        );
        assert_eq!(
            trips(&["2025-06-01 10:00", "", "2025-06-04 10:00"]),
            [0, 1, 2]
        );
        // 続く写真は、そこから通常どおり
        assert_eq!(
            trips(&[
                "2025-06-01 10:00",
                "",
                "2025-06-02 10:00",
                "2025-06-02 11:00"
            ]),
            [0, 1, 1, 1]
        );
    }
}
