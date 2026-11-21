//! 一覧 API の応答（design.md 6.4）の値の検査（4.4）と、候補の選び方（4.1 の手順 3・4・6 のうち、画像を取らない部分）。
//!
//! 一覧 API の値は取り込んだ写真の EXIF やファイル名に由来するので、書き出し側は受け取った値を信じずに検査し直す。
//! 応答は `serde_json::Value` から手で読む（`serde(default)` や `Option` に潰すと、キーが「欠けている」ことと
//! 「null」であることの区別が消える。欠けを許すのは `elev` だけ）。
//!
//! 検査の順序（先に見つかったものを返す）:
//! 1. 最上位の形（`from`・`to` が要求どおり、`photos` は配列、`undated`・`invalid_time` は 0 以上の整数、`truncated` は真偽値）
//!    → 崩れていれば `feed_invalid`
//! 2. `truncated: true`、または件数が上限を超えている → `too_many_photos`
//! 3. 各行の値（4.4）→ 崩れていれば `feed_invalid`。期間の外の日付も `feed_invalid`
//!
//! その後の選別は [`Selection`] に集める。**表示用 JPEG がまだ無い写真（`file: null`）は、ここでは失敗にしない**。
//! `dry_run` はその数を返すだけで、本番の書き出しだけが失敗にする（[`Selection::require_no_pending`]）。

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use crate::date::{Date, Minute, Period, TimeShape};
use crate::error::{ErrorCode, ExportError};

/// 一覧 API が 1 回で返す写真の上限（写真サービス側の `PHOTOS_MAX` と同じ値）。
pub const PHOTOS_MAX: usize = 2_000;

/// 表示用 JPEG の状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// `derivatives/<名前>.jpg` の形で、そのまま取れる。検査済みの相対パス。
    Ready(String),
    /// `file` が `null`（取り込みの直後や回転の直後。作成待ち）。
    Pending,
    /// `file` が表示用 JPEG の形に合わない（EXIF 付きの原本を外へ出さないため、書き出さない）。値は持たない。
    NotDerivative,
}

/// 検査済みの写真 1 枚（期間内に実在する日時を持つもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: i64,
    pub file: FileState,
    pub title: String,
    /// `lat`・`lon` は両方ある（範囲内）か、両方 `None`（位置の無い写真）。
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub time: Minute,
    pub elev: Option<f64>,
    pub inferred: bool,
}

/// 仕様上の除外（一覧 API の値だけで分かるもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exclusion {
    pub id: i64,
    pub reason: ExcludeReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeReason {
    NotDerivative,
}

impl ExcludeReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotDerivative => "not_derivative",
        }
    }
}

/// 一覧 API の応答から選んだ結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub period: Period,
    /// 期間内に実在する日時を持つ写真（`time` の昇順、同じ時刻は `id` の昇順）。表示用 JPEG の状態は問わない。
    pub candidates: Vec<Candidate>,
    /// `time` が空の写真の数（カタログ全体。非表示を除く）。一覧 API の `undated` に、
    /// 応答の `photos` の中にあった空の `time` の行を足したもの。
    pub undated: u64,
    /// `time` の形が崩れた、または暦に実在しない写真の数（カタログ全体。非表示を除く）。一覧 API の `invalid_time` に、
    /// 応答の `photos` の中にあった崩れた行（`2030-02-30 10:00` など）を足したもの。
    pub invalid_time: u64,
}

impl Selection {
    /// 候補の数（`catalog_count`、`dry_run` の `candidate_count`）。
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// 表示用 JPEG の作成待ちの数。1 以上なら本番の書き出しは失敗する。
    pub fn pending_count(&self) -> usize {
        self.candidates
            .iter()
            .filter(|c| c.file == FileState::Pending)
            .count()
    }

    /// 一覧 API だけで分かる除外（`not_derivative`）。
    pub fn exclusions(&self) -> Vec<Exclusion> {
        self.candidates
            .iter()
            .filter(|c| c.file == FileState::NotDerivative)
            .map(|c| Exclusion {
                id: c.id,
                reason: ExcludeReason::NotDerivative,
            })
            .collect()
    }

    /// そのまま取れる写真（表示用 JPEG の形に合うもの）。
    pub fn ready(&self) -> impl Iterator<Item = (&Candidate, &str)> {
        self.candidates.iter().filter_map(|c| match &c.file {
            FileState::Ready(path) => Some((c, path.as_str())),
            _ => None,
        })
    }

    /// 日ごとの振り分け（期間のときの `days`、日付ごとのサブフォルダ）。写真のある日だけ、日付の昇順。
    /// 各日の中は `time`・`id` の昇順。除外される写真も含むので、書き出す写真だけが要るときは `file` で絞る。
    pub fn by_day(&self) -> BTreeMap<Date, Vec<&Candidate>> {
        let mut days: BTreeMap<Date, Vec<&Candidate>> = BTreeMap::new();
        for c in &self.candidates {
            days.entry(c.time.date).or_default().push(c);
        }
        days
    }

    /// 本番の書き出しの入口: 表示用 JPEG の作成待ちが 1 枚でもあれば `derivative_pending`。
    pub fn require_no_pending(self) -> Result<Self, ExportError> {
        if self.pending_count() > 0 {
            return Err(ExportError::from(ErrorCode::DerivativePending));
        }
        Ok(self)
    }
}

fn invalid(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::FeedInvalid, detail)
}

/// `file` が `^derivatives/[A-Za-z0-9_-][A-Za-z0-9._-]*\.jpg$` に合い、`..` を含まないか。
pub fn is_derivative_path(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("derivatives/") else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".jpg") else {
        return false;
    };
    let mut bytes = stem.bytes();
    let first_ok = bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    first_ok
        && bytes.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        && !s.contains("..")
}

fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Value, ExportError> {
    obj.get(key).ok_or(invalid("応答の行のキーが欠けています"))
}

/// 行の `lat`・`lon`（キーは必須。`null` か有限の数値）。
fn coordinate(value: &Value, limit: f64) -> Result<Option<f64>, ExportError> {
    match value {
        Value::Null => Ok(None),
        Value::Number(n) => {
            let v = n.as_f64().filter(|v| v.is_finite() && v.abs() <= limit);
            v.map(Some).ok_or(invalid("lat・lon が範囲外です"))
        }
        _ => Err(invalid("lat・lon が数値でも null でもありません")),
    }
}

/// 時刻の問題があって候補にならない行の種類。
enum Row {
    Candidate(Candidate),
    NoTime(TimeShape),
}

fn parse_row(value: &Value, period: &Period, seen: &mut HashSet<i64>) -> Result<Row, ExportError> {
    let obj = value
        .as_object()
        .ok_or(invalid("応答の行がオブジェクトではありません"))?;

    let id = field(obj, "id")?
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or(invalid("id が正の整数ではありません"))?;
    if !seen.insert(id) {
        return Err(invalid("同じ id が 2 度出ています"));
    }

    let time = field(obj, "time")?
        .as_str()
        .ok_or(invalid("time が文字列ではありません"))?;

    let file = match field(obj, "file")? {
        Value::Null => FileState::Pending,
        Value::String(s) if is_derivative_path(s) => FileState::Ready(s.clone()),
        Value::String(_) => FileState::NotDerivative,
        _ => return Err(invalid("file が文字列でも null でもありません")),
    };

    let inferred = field(obj, "inferred")?
        .as_bool()
        .ok_or(invalid("inferred が真偽値ではありません"))?;

    // 欠けは null として扱う（elev だけ。旧い応答には無い）
    let elev = match obj.get("elev") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_f64()
                .filter(|v| v.is_finite())
                .ok_or(invalid("elev が有限の数値ではありません"))?,
        ),
        Some(_) => return Err(invalid("elev が数値でも null でもありません")),
    };

    let lat = coordinate(field(obj, "lat")?, 90.0)?;
    let lon = coordinate(field(obj, "lon")?, 180.0)?;
    if lat.is_some() != lon.is_some() {
        return Err(invalid("lat・lon の片方だけが null です"));
    }
    if lat.is_none() && inferred {
        return Err(invalid("位置が無いのに inferred が true です"));
    }

    let title = field(obj, "title")?
        .as_str()
        .ok_or(invalid("title が文字列ではありません"))?
        .to_owned();

    match Minute::classify(time) {
        shape @ (TimeShape::Empty | TimeShape::Malformed) => Ok(Row::NoTime(shape)),
        TimeShape::Valid(time) => {
            if !period.contains(time.date) {
                return Err(invalid("time の日付が要求の期間の外です"));
            }
            Ok(Row::Candidate(Candidate {
                id,
                file,
                title,
                lat,
                lon,
                time,
                elev,
                inferred,
            }))
        }
    }
}

const COUNT_OVERFLOW: &str = "undated・invalid_time の件数が表現できる範囲を超えます";

fn count(obj: &Map<String, Value>, key: &str) -> Result<u64, ExportError> {
    field_top(obj, key)?.as_u64().ok_or(invalid(
        "undated・invalid_time が 0 以上の整数ではありません",
    ))
}

fn field_top<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Value, ExportError> {
    obj.get(key).ok_or(invalid("応答のキーが欠けています"))
}

/// 一覧 API の応答（JSON）を検査して、候補を選ぶ。
pub fn select(response: &Value, period: &Period) -> Result<Selection, ExportError> {
    let top = response
        .as_object()
        .ok_or(invalid("応答がオブジェクトではありません"))?;
    for (key, want) in [("from", period.from()), ("to", period.to())] {
        if field_top(top, key)?.as_str() != Some(want.to_string().as_str()) {
            return Err(invalid("応答の from・to が要求と違います"));
        }
    }
    let photos = field_top(top, "photos")?
        .as_array()
        .ok_or(invalid("photos が配列ではありません"))?;
    let mut undated = count(top, "undated")?;
    let mut invalid_time = count(top, "invalid_time")?;
    let truncated = field_top(top, "truncated")?
        .as_bool()
        .ok_or(invalid("truncated が真偽値ではありません"))?;

    if truncated || photos.len() > PHOTOS_MAX {
        return Err(ExportError::from(ErrorCode::TooManyPhotos));
    }

    let mut seen = HashSet::with_capacity(photos.len());
    let mut candidates = Vec::with_capacity(photos.len());
    for photo in photos {
        match parse_row(photo, period, &mut seen)? {
            Row::Candidate(c) => candidates.push(c),
            // 日付が分からない行は期間に割り当てられないので、候補に入れず件数に足す（黙って落とさない）。
            // 一覧 API は `taken_at` が分までの形の行だけを `photos` に出すので、ここに来るのは暦に実在しない日時
            // （`2030-02-30 10:00`）か、写真サービスの契約に反する行
            // 件数は表現できる範囲だけ通す（応答の件数が u64::MAX のとき、足して折り返さない・パニックしない）
            Row::NoTime(TimeShape::Empty) => {
                undated = undated.checked_add(1).ok_or(invalid(COUNT_OVERFLOW))?;
            }
            Row::NoTime(_) => {
                invalid_time = invalid_time.checked_add(1).ok_or(invalid(COUNT_OVERFLOW))?;
            }
        }
    }
    candidates.sort_by_key(|c| (c.time, c.id));

    Ok(Selection {
        period: *period,
        candidates,
        undated,
        invalid_time,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn period() -> Period {
        Period::parse(Some("2030-03-10"), Some("2030-03-11")).unwrap()
    }

    fn row(id: i64, time: &str) -> Value {
        json!({"id": id, "file": format!("derivatives/p{id}.jpg"), "title": "t",
               "lat": 12.5, "lon": 34.5, "time": time, "elev": 1.5, "inferred": false})
    }

    fn response(photos: Vec<Value>) -> Value {
        json!({"from": "2030-03-10", "to": "2030-03-11", "photos": photos,
               "undated": 0, "invalid_time": 0, "truncated": false})
    }

    fn code(r: Result<Selection, ExportError>) -> ErrorCode {
        r.expect_err("拒まれるはず").code
    }

    /// 1 行だけ書き換えて検査に通す。
    fn with_row(edit: impl FnOnce(&mut Value)) -> Result<Selection, ExportError> {
        let mut r = response(vec![row(1, "2030-03-10 09:00")]);
        edit(&mut r["photos"][0]);
        select(&r, &period())
    }

    #[test]
    fn a_well_formed_response_gives_sorted_candidates() {
        let r = response(vec![
            row(3, "2030-03-11 10:00"),
            row(2, "2030-03-10 09:00"),
            row(1, "2030-03-10 09:00"),
        ]);
        let s = select(&r, &period()).unwrap();
        let ids: Vec<i64> = s.candidates.iter().map(|c| c.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(s.candidate_count(), 3);
        assert_eq!(s.pending_count(), 0);
        assert!(s.exclusions().is_empty());
        assert_eq!((s.undated, s.invalid_time), (0, 0));
        let c = &s.candidates[0];
        assert_eq!(c.file, FileState::Ready("derivatives/p1.jpg".into()));
        assert_eq!(
            (c.lat, c.lon, c.elev, c.inferred),
            (Some(12.5), Some(34.5), Some(1.5), false)
        );
        assert_eq!(c.time.to_string(), "2030-03-10 09:00");
    }

    #[test]
    fn days_split_the_period_in_order() {
        let r = response(vec![
            row(3, "2030-03-11 00:00"),
            row(1, "2030-03-10 23:59"),
            row(2, "2030-03-10 00:00"),
        ]);
        let s = select(&r, &period()).unwrap();
        let days = s.by_day();
        let keys: Vec<String> = days.keys().map(ToString::to_string).collect();
        assert_eq!(keys, ["2030-03-10", "2030-03-11"]);
        let first: Vec<i64> = days.values().next().unwrap().iter().map(|c| c.id).collect();
        assert_eq!(first, [2, 1]);
        // 境界の時刻（00:00・23:59）は、その日の写真として数える
        assert_eq!(days.values().map(Vec::len).sum::<usize>(), 3);
        assert_eq!(s.ready().count(), 3);
    }

    #[test]
    fn top_level_shape_errors_are_feed_invalid() {
        let good = response(vec![]);
        assert!(select(&good, &period()).is_ok());
        assert_eq!(code(select(&json!([]), &period())), ErrorCode::FeedInvalid);
        assert_eq!(code(select(&json!("x"), &period())), ErrorCode::FeedInvalid);
        for key in [
            "from",
            "to",
            "photos",
            "undated",
            "invalid_time",
            "truncated",
        ] {
            let mut r = good.clone();
            r.as_object_mut().unwrap().remove(key);
            assert_eq!(
                code(select(&r, &period())),
                ErrorCode::FeedInvalid,
                "{key} の欠け"
            );
        }
        let edits: [(&str, Value); 8] = [
            ("from", json!("2030-03-09")),
            ("to", json!("2030-03-12")),
            ("from", json!(null)),
            ("photos", json!({})),
            ("undated", json!(-1)),
            ("undated", json!("2")),
            ("invalid_time", json!(1.5)),
            ("truncated", json!("false")),
        ];
        for (key, value) in edits {
            let mut r = good.clone();
            r[key] = value.clone();
            assert_eq!(
                code(select(&r, &period())),
                ErrorCode::FeedInvalid,
                "{key}={value}"
            );
        }
        // truncated の欠けを false とみなさない
        let mut r = good;
        r["truncated"] = Value::Null;
        assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid);
    }

    #[test]
    fn truncated_or_oversized_means_too_many_photos() {
        let mut r = response(vec![row(1, "2030-03-10 09:00")]);
        r["truncated"] = json!(true);
        assert_eq!(code(select(&r, &period())), ErrorCode::TooManyPhotos);
        // 行の検査より前に判定する（truncated の応答の行に不正があっても too_many_photos）
        let mut r = response(vec![json!({"id": "x"})]);
        r["truncated"] = json!(true);
        assert_eq!(code(select(&r, &period())), ErrorCode::TooManyPhotos);
        // 件数が上限ちょうどなら通る。超えていれば、truncated が false でも断る
        let rows = |n: usize| -> Vec<Value> {
            (1..=n as i64).map(|i| row(i, "2030-03-10 09:00")).collect()
        };
        assert_eq!(
            select(&response(rows(PHOTOS_MAX)), &period())
                .unwrap()
                .candidate_count(),
            PHOTOS_MAX
        );
        assert_eq!(
            code(select(&response(rows(PHOTOS_MAX + 1)), &period())),
            ErrorCode::TooManyPhotos
        );
    }

    #[test]
    fn ids_must_be_positive_integers_and_unique() {
        for bad in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(null),
            json!(true),
            json!(1e30),
        ] {
            assert_eq!(
                code(with_row(|p| p["id"] = bad.clone())),
                ErrorCode::FeedInvalid,
                "{bad}"
            );
        }
        assert_eq!(
            code(with_row(|p| {
                p.as_object_mut().unwrap().remove("id");
            })),
            ErrorCode::FeedInvalid
        );
        let r = response(vec![row(1, "2030-03-10 09:00"), row(1, "2030-03-11 09:00")]);
        assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid);
        // 行がオブジェクトでない
        let r = response(vec![json!(1)]);
        assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid);
    }

    #[test]
    fn time_outside_the_period_is_feed_invalid() {
        assert_eq!(
            code(with_row(|p| p["time"] = json!("2030-03-09 23:59"))),
            ErrorCode::FeedInvalid
        );
        assert_eq!(
            code(with_row(|p| p["time"] = json!("2030-03-12 00:00"))),
            ErrorCode::FeedInvalid
        );
        // 両端の日は通る
        assert!(with_row(|p| p["time"] = json!("2030-03-10 00:00")).is_ok());
        assert!(with_row(|p| p["time"] = json!("2030-03-11 23:59")).is_ok());
        // 型が違う・欠け
        assert_eq!(
            code(with_row(|p| p["time"] = json!(5))),
            ErrorCode::FeedInvalid
        );
        assert_eq!(
            code(with_row(|p| p["time"] = json!(null))),
            ErrorCode::FeedInvalid
        );
        assert_eq!(
            code(with_row(|p| {
                p.as_object_mut().unwrap().remove("time");
            })),
            ErrorCode::FeedInvalid
        );
    }

    #[test]
    fn rows_without_a_usable_time_are_counted_not_listed() {
        let mut r = response(vec![
            row(1, "2030-03-10 09:00"),
            row(2, ""),
            row(3, "2030-02-30 10:00"),
            row(4, "2030-03-10 24:00"),
            row(5, "2030-03-10T09:00"),
        ]);
        r["undated"] = json!(2);
        r["invalid_time"] = json!(1);
        let s = select(&r, &period()).unwrap();
        assert_eq!(s.candidates.iter().map(|c| c.id).collect::<Vec<_>>(), [1]);
        // 一覧 API の件数（カタログ全体）に、応答の中の行を足す
        assert_eq!(s.undated, 3);
        assert_eq!(s.invalid_time, 4);
        // 日付が分からない行の表示用 JPEG の状態は、作成待ちの数に入れない
        let mut r = response(vec![row(2, "2030-02-30 10:00")]);
        r["photos"][0]["file"] = json!(null);
        assert_eq!(select(&r, &period()).unwrap().pending_count(), 0);
        // ただし値の検査（inferred など）は、そういう行にも掛ける
        let mut r = response(vec![row(2, "")]);
        r["photos"][0].as_object_mut().unwrap().remove("inferred");
        assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid);
    }

    #[test]
    fn counters_at_the_edge_of_u64_are_exact_or_feed_invalid() {
        // (カウンターのキー, 足される行の time)
        for (key, time) in [("undated", ""), ("invalid_time", "2030-02-30 10:00")] {
            let get = |s: &Selection| {
                if key == "undated" {
                    s.undated
                } else {
                    s.invalid_time
                }
            };
            let other = |s: &Selection| {
                if key == "undated" {
                    s.invalid_time
                } else {
                    s.undated
                }
            };
            // 行が無ければ、u64::MAX のままで通る
            let mut r = response(vec![row(1, "2030-03-10 09:00")]);
            r[key] = json!(u64::MAX);
            let s = select(&r, &period()).unwrap();
            assert_eq!((get(&s), other(&s)), (u64::MAX, 0), "{key}");
            // u64::MAX - 1 に 1 行足すと、ちょうど u64::MAX
            let mut r = response(vec![row(1, time)]);
            r[key] = json!(u64::MAX - 1);
            let s = select(&r, &period()).unwrap();
            assert_eq!(get(&s), u64::MAX, "{key}");
            // u64::MAX に 1 行足すと表現できない
            let mut r = response(vec![row(1, time)]);
            r[key] = json!(u64::MAX);
            assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid, "{key}");
            // 2 行目で超えるときも同じ
            let mut r = response(vec![row(1, time), row(2, time)]);
            r[key] = json!(u64::MAX - 1);
            assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid, "{key}");
        }
    }

    #[test]
    fn a_null_file_is_pending_and_fails_only_the_real_export() {
        let s = with_row(|p| p["file"] = json!(null)).unwrap();
        assert_eq!(s.pending_count(), 1);
        assert_eq!(s.candidate_count(), 1);
        assert_eq!(s.ready().count(), 0);
        // dry_run は件数として返し、本番だけが失敗にする
        assert_eq!(code(s.require_no_pending()), ErrorCode::DerivativePending);
        let ok = with_row(|_| {}).unwrap();
        assert!(ok.require_no_pending().is_ok());
        // file の型が違う・欠けは feed_invalid（pending ではない）
        assert_eq!(
            code(with_row(|p| p["file"] = json!(5))),
            ErrorCode::FeedInvalid
        );
        assert_eq!(
            code(with_row(|p| {
                p.as_object_mut().unwrap().remove("file");
            })),
            ErrorCode::FeedInvalid
        );
    }

    #[test]
    fn a_feed_invalid_row_wins_over_derivative_pending() {
        let mut r = response(vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-10 09:00")]);
        r["photos"][0]["file"] = json!(null);
        r["photos"][1]["inferred"] = json!("no");
        assert_eq!(code(select(&r, &period())), ErrorCode::FeedInvalid);
    }

    #[test]
    fn files_that_are_not_derivatives_are_excluded_not_failed() {
        for bad in [
            "",
            "derivatives/",
            "derivatives/.jpg",
            "derivatives/a.png",
            "derivatives/a.JPG",
            "derivatives/.a.jpg",
            "derivatives/a/b.jpg",
            "derivatives/../a.jpg",
            "derivatives/a..b.jpg",
            "derivatives//a.jpg",
            "/derivatives/a.jpg",
            "derivative/a.jpg",
            "local/IMG_0001.jpg",
            "derivatives/a b.jpg",
            "derivatives/日本.jpg",
            "derivatives/a.jpg\n",
            "derivatives/a.jpg?x=1",
            "http://example.test/derivatives/a.jpg",
        ] {
            assert!(!is_derivative_path(bad), "{bad:?}");
            let s = with_row(|p| p["file"] = json!(bad)).unwrap();
            assert_eq!(s.candidate_count(), 1, "{bad:?}");
            assert_eq!(s.pending_count(), 0, "{bad:?}");
            assert_eq!(
                s.exclusions(),
                [Exclusion {
                    id: 1,
                    reason: ExcludeReason::NotDerivative
                }],
                "{bad:?}"
            );
            assert_eq!(s.ready().count(), 0);
        }
        for ok in [
            "derivatives/a.jpg",
            "derivatives/_a-b.c.jpg",
            "derivatives/0123abcXYZ.jpg",
            "derivatives/ab.r90.jpg",
            "derivatives/-a.jpg",
        ] {
            assert!(is_derivative_path(ok), "{ok:?}");
        }
        // 除外される写真の `file` の値は結果に残らない
        let s = with_row(|p| p["file"] = json!("local/secret-name.jpg")).unwrap();
        assert!(!format!("{s:?}").contains("secret-name"));
    }

    #[test]
    fn inferred_must_be_a_bool() {
        assert_eq!(
            code(with_row(|p| {
                p.as_object_mut().unwrap().remove("inferred");
            })),
            ErrorCode::FeedInvalid
        );
        for bad in [json!(null), json!("true"), json!(1), json!(0)] {
            assert_eq!(
                code(with_row(|p| p["inferred"] = bad.clone())),
                ErrorCode::FeedInvalid,
                "{bad}"
            );
        }
        assert!(
            with_row(|p| p["inferred"] = json!(true))
                .unwrap()
                .candidates[0]
                .inferred
        );
    }

    #[test]
    fn elev_is_a_number_or_null_and_may_be_missing() {
        assert_eq!(
            with_row(|p| p["elev"] = json!(null)).unwrap().candidates[0].elev,
            None
        );
        assert_eq!(
            with_row(|p| {
                p.as_object_mut().unwrap().remove("elev");
            })
            .unwrap()
            .candidates[0]
                .elev,
            None
        );
        assert_eq!(
            with_row(|p| p["elev"] = json!(-12)).unwrap().candidates[0].elev,
            Some(-12.0)
        );
        for bad in [json!("12"), json!(true), json!([]), json!({})] {
            assert_eq!(
                code(with_row(|p| p["elev"] = bad.clone())),
                ErrorCode::FeedInvalid,
                "{bad}"
            );
        }
    }

    #[test]
    fn coordinates_are_both_valid_or_both_null() {
        let none = with_row(|p| {
            p["lat"] = json!(null);
            p["lon"] = json!(null);
        })
        .unwrap();
        assert_eq!(
            (none.candidates[0].lat, none.candidates[0].lon),
            (None, None)
        );
        // 範囲の端
        assert!(
            with_row(|p| {
                p["lat"] = json!(90);
                p["lon"] = json!(-180);
            })
            .is_ok()
        );
        assert!(
            with_row(|p| {
                p["lat"] = json!(-90.0);
                p["lon"] = json!(180.0);
            })
            .is_ok()
        );
        for (lat, lon) in [
            (json!(null), json!(34.5)),
            (json!(12.5), json!(null)),
            (json!(90.1), json!(34.5)),
            (json!(-90.1), json!(34.5)),
            (json!(12.5), json!(180.1)),
            (json!(12.5), json!(-180.1)),
            (json!("12.5"), json!(34.5)),
            (json!(12.5), json!(true)),
            (json!(1e308), json!(34.5)),
        ] {
            assert_eq!(
                code(with_row(|p| {
                    p["lat"] = lat.clone();
                    p["lon"] = lon.clone();
                })),
                ErrorCode::FeedInvalid,
                "{lat} {lon}"
            );
        }
        for key in ["lat", "lon"] {
            assert_eq!(
                code(with_row(|p| {
                    p.as_object_mut().unwrap().remove(key);
                })),
                ErrorCode::FeedInvalid,
                "{key} の欠け"
            );
        }
        // 位置が無いのに inferred
        assert_eq!(
            code(with_row(|p| {
                p["lat"] = json!(null);
                p["lon"] = json!(null);
                p["inferred"] = json!(true);
            })),
            ErrorCode::FeedInvalid
        );
    }

    #[test]
    fn title_must_be_a_string_but_may_be_empty() {
        assert_eq!(
            with_row(|p| p["title"] = json!("")).unwrap().candidates[0].title,
            ""
        );
        for bad in [json!(null), json!(1), json!([])] {
            assert_eq!(
                code(with_row(|p| p["title"] = bad.clone())),
                ErrorCode::FeedInvalid,
                "{bad}"
            );
        }
        assert_eq!(
            code(with_row(|p| {
                p.as_object_mut().unwrap().remove("title");
            })),
            ErrorCode::FeedInvalid
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        assert!(with_row(|p| p["future"] = json!(1)).is_ok());
        let mut r = response(vec![]);
        r["future"] = json!(1);
        assert!(select(&r, &period()).is_ok());
    }
}
