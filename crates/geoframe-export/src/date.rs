//! 日付・時刻・期間（design.md 4.4、6.1）。暦は手で扱う（chrono は使わない）。桁は ASCII の数字だけを通す。

use std::fmt;

use crate::error::ErrorCode;

/// 期間の長さの上限（日数。`from` と `to` の両端を含む）。一覧 API（写真サービス側）と同じ値。
pub const SPAN_DAYS_MAX: i64 = 14;

/// `digits(b"0123")` のように、すべて ASCII の数字なら数値にする。
fn digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0u32, |n, c| n * 10 + u32::from(c - b'0')))
}

fn is_leap(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// 暦に実在する日付（西暦 1〜9999 年）。並びは年・月・日の順。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        let ok = (1..=9999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(year, month);
        ok.then_some(Self { year, month, day })
    }

    /// `YYYY-MM-DD`（各桁が ASCII の数字の 10 文字）で、暦に実在するものだけ。1〜2 桁の月日や前後の空白は通さない。
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let year = u16::try_from(digits(&b[0..4])?).ok()?;
        let month = u8::try_from(digits(&b[5..7])?).ok()?;
        let day = u8::try_from(digits(&b[8..10])?).ok()?;
        Self::new(year, month, day)
    }

    pub const fn year(self) -> u16 {
        self.year
    }

    pub const fn month(self) -> u8 {
        self.month
    }

    pub const fn day(self) -> u8 {
        self.day
    }

    /// 1970-01-01 からの日数（日数の差・期間の長さを求めるため）。
    pub fn days_since_epoch(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// 分までの撮影時刻（`YYYY-MM-DD HH:MM`。ローカル時刻のオフセット無し）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Minute {
    pub date: Date,
    pub hour: u8,
    pub minute: u8,
}

/// 一覧 API の `time` の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeShape {
    /// 実在する日付・時刻。
    Valid(Minute),
    /// 空文字（日付が分からない）。
    Empty,
    /// 形が違う、または暦・時刻の範囲に実在しない（`2030-02-30 10:00`、`24:00` など）。
    Malformed,
}

impl Minute {
    /// `^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$` にそのまま合い、数値にして実在するものだけ（時 0〜23、分 0〜59）。
    pub fn parse(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() != 16 || b[10] != b' ' || b[13] != b':' {
            return None;
        }
        let date = Date::parse(s.get(0..10)?)?;
        let hour = u8::try_from(digits(&b[11..13])?).ok()?;
        let minute = u8::try_from(digits(&b[14..16])?).ok()?;
        (hour <= 23 && minute <= 59).then_some(Self { date, hour, minute })
    }

    pub fn classify(s: &str) -> TimeShape {
        if s.is_empty() {
            TimeShape::Empty
        } else {
            Self::parse(s).map_or(TimeShape::Malformed, TimeShape::Valid)
        }
    }
}

impl fmt::Display for Minute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:02}:{:02}", self.date, self.hour, self.minute)
    }
}

/// 書き出す期間（両端を含む）。`from <= to` で、14 日以下であることを作るときに確かめる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    from: Date,
    to: Date,
}

impl Period {
    /// `from > to` と、14 日（両端を含む）を超える期間は `invalid_range`。
    pub fn new(from: Date, to: Date) -> Result<Self, ErrorCode> {
        if to < from || to.days_since_epoch() - from.days_since_epoch() + 1 > SPAN_DAYS_MAX {
            return Err(ErrorCode::InvalidRange);
        }
        Ok(Self { from, to })
    }

    /// `from`（必須）と `to`（省略可。省略すると `from` と同じ）の文字列から作る。
    /// 日付の形・暦の誤りは `invalid_date`、順序・長さの誤りは `invalid_range`。
    pub fn parse(from: Option<&str>, to: Option<&str>) -> Result<Self, ErrorCode> {
        let from = from.and_then(Date::parse).ok_or(ErrorCode::InvalidDate)?;
        let to = match to {
            Some(s) => Date::parse(s).ok_or(ErrorCode::InvalidDate)?,
            None => from,
        };
        Self::new(from, to)
    }

    pub const fn from(&self) -> Date {
        self.from
    }

    pub const fn to(&self) -> Date {
        self.to
    }

    /// 日数（両端を含む。1〜14）。
    pub fn days(&self) -> i64 {
        self.to.days_since_epoch() - self.from.days_since_epoch() + 1
    }

    pub fn contains(&self, date: Date) -> bool {
        self.from <= date && date <= self.to
    }

    /// 1 日だけの期間か（manifest の `date` やサブフォルダの有無が変わる）。
    pub fn is_single_day(&self) -> bool {
        self.from == self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn dates_must_exist_in_the_calendar() {
        for ok in [
            "2030-03-10",
            "2030-12-31",
            "2028-02-29",
            "2000-02-29",
            "0001-01-01",
            "9999-12-31",
        ] {
            assert_eq!(d(ok).to_string(), ok);
        }
        for bad in [
            "",
            "2030-3-10",
            "2030-03-1",
            "2030-02-30",
            "2029-02-29",
            "1900-02-29",
            "2030-13-01",
            "2030-00-10",
            "2030-03-00",
            "2030-03-32",
            "2030/03/10",
            "2030-03-10 ",
            " 2030-03-10",
            "２０３０-03-10",
            "+030-03-10",
            "2030-03-10T00:00",
            "0000-01-01",
            "20300310",
        ] {
            assert_eq!(Date::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn day_counts_follow_the_calendar() {
        assert_eq!(d("1970-01-01").days_since_epoch(), 0);
        assert_eq!(d("1970-01-02").days_since_epoch(), 1);
        assert_eq!(
            d("2030-03-01").days_since_epoch() - d("2030-02-28").days_since_epoch(),
            1
        );
        // 閏日をまたぐ
        assert_eq!(
            d("2028-03-01").days_since_epoch() - d("2028-02-28").days_since_epoch(),
            2
        );
        // 年をまたぐ
        assert_eq!(
            d("2031-01-01").days_since_epoch() - d("2030-12-31").days_since_epoch(),
            1
        );
        assert_eq!(
            d("2000-03-01").days_since_epoch() - d("1999-03-01").days_since_epoch(),
            366
        );
    }

    #[test]
    fn minutes_need_the_exact_shape_and_a_real_time() {
        let m = Minute::parse("2030-03-10 23:59").unwrap();
        assert_eq!((m.hour, m.minute), (23, 59));
        assert_eq!(m.to_string(), "2030-03-10 23:59");
        assert!(Minute::parse("2030-03-10 00:00").is_some());
        for bad in [
            "2030-03-10 24:00",
            "2030-03-10 12:60",
            "2030-02-30 10:00",
            "2030-03-10  10:00",
            "2030-03-10T10:00",
            "2030-03-10 10:00:00",
            "2030-03-10 1:00",
            "2030-03-10",
            "2030-03-10 10:0a",
            "２０３０-03-10 10:00",
        ] {
            assert_eq!(Minute::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Minute::classify(""), TimeShape::Empty);
        assert_eq!(Minute::classify("2030-02-30 10:00"), TimeShape::Malformed);
        assert_eq!(Minute::classify("昨日"), TimeShape::Malformed);
        assert!(matches!(
            Minute::classify("2030-03-10 10:00"),
            TimeShape::Valid(_)
        ));
    }

    #[test]
    fn a_missing_to_means_the_same_day() {
        let p = Period::parse(Some("2030-03-10"), None).unwrap();
        assert_eq!(
            p,
            Period::parse(Some("2030-03-10"), Some("2030-03-10")).unwrap()
        );
        assert!(p.is_single_day());
        assert_eq!(p.days(), 1);
    }

    #[test]
    fn spans_of_14_days_pass_and_15_days_do_not() {
        let ok = Period::parse(Some("2030-03-01"), Some("2030-03-14")).unwrap();
        assert_eq!(ok.days(), 14);
        assert_eq!(
            Period::parse(Some("2030-03-01"), Some("2030-03-15")),
            Err(ErrorCode::InvalidRange)
        );
        // 月・年・閏日をまたいでも両端を含めて数える
        assert!(Period::parse(Some("2030-12-25"), Some("2031-01-07")).is_ok());
        assert_eq!(
            Period::parse(Some("2030-12-25"), Some("2031-01-08")),
            Err(ErrorCode::InvalidRange)
        );
        assert!(Period::parse(Some("2028-02-20"), Some("2028-03-04")).is_ok());
        assert_eq!(
            Period::parse(Some("2028-02-20"), Some("2028-03-05")),
            Err(ErrorCode::InvalidRange)
        );
    }

    #[test]
    fn reversed_and_nonexistent_dates_are_refused() {
        assert_eq!(
            Period::parse(Some("2030-03-11"), Some("2030-03-10")),
            Err(ErrorCode::InvalidRange)
        );
        assert_eq!(Period::parse(None, None), Err(ErrorCode::InvalidDate));
        assert_eq!(
            Period::parse(Some("2030-02-30"), None),
            Err(ErrorCode::InvalidDate)
        );
        assert_eq!(
            Period::parse(Some("2030-03-10"), Some("2030-02-30")),
            Err(ErrorCode::InvalidDate)
        );
        assert_eq!(Period::parse(Some(""), None), Err(ErrorCode::InvalidDate));
        assert_eq!(
            Period::parse(Some("2030-03-10"), Some("")),
            Err(ErrorCode::InvalidDate)
        );
    }

    #[test]
    fn contains_includes_both_ends() {
        let p = Period::parse(Some("2030-03-10"), Some("2030-03-12")).unwrap();
        assert!(p.contains(d("2030-03-10")));
        assert!(p.contains(d("2030-03-12")));
        assert!(!p.contains(d("2030-03-09")));
        assert!(!p.contains(d("2030-03-13")));
    }
}
