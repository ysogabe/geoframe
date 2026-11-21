//! 書き出し先のフォルダ名（design.md 3.5）。
//!
//! - 1 日: `YYYY-MM-DD`
//! - 同じ月の期間: `YYYY-MM-DD-DD`
//! - 月・年をまたぐ期間: `YYYY-MM-DD_YYYY-MM-DD`
//!
//! 名前は検査済みの日付から作り、`^\d{4}-\d{2}-\d{2}(-\d{2}|_\d{4}-\d{2}-\d{2})?$`（`\d` は ASCII の数字）に合うことを
//! 作るたびに確かめる（正規表現の crate は使わない）。確かめていない文字列から [`FolderName`] は作れない。

use crate::date::Period;
use crate::error::{ErrorCode, ExportError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderName(String);

fn is_date_shape(b: &[u8]) -> bool {
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// 名前の形の検査（上の正規表現と同じ）。
pub fn is_valid_folder_name(s: &str) -> bool {
    let b = s.as_bytes();
    match b.len() {
        10 => is_date_shape(b),
        13 => is_date_shape(&b[..10]) && b[10] == b'-' && b[11..].iter().all(u8::is_ascii_digit),
        21 => is_date_shape(&b[..10]) && b[10] == b'_' && is_date_shape(&b[11..]),
        _ => false,
    }
}

impl FolderName {
    /// 期間から作る。作った名前を形の検査に通し、合わなければ `internal`（起きないはずの食い違い）。
    pub fn for_period(period: &Period) -> Result<Self, ExportError> {
        let (from, to) = (period.from(), period.to());
        let name = if from == to {
            from.to_string()
        } else if from.year() == to.year() && from.month() == to.month() {
            format!("{from}-{:02}", to.day())
        } else {
            format!("{from}_{to}")
        };
        Self::parse(&name).ok_or(ExportError::new(
            ErrorCode::Internal,
            "フォルダ名が決められた形になりません",
        ))
    }

    /// 文字列（記録から読んだものなど）を形の検査に通す。
    pub fn parse(s: &str) -> Option<Self> {
        is_valid_folder_name(s).then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FolderName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(from: &str, to: Option<&str>) -> String {
        let p = Period::parse(Some(from), to).unwrap();
        FolderName::for_period(&p).unwrap().to_string()
    }

    #[test]
    fn the_four_shapes() {
        // 1 日（`to` の省略と `to == from` は同じ名前）
        assert_eq!(name("2030-03-10", None), "2030-03-10");
        assert_eq!(name("2030-03-10", Some("2030-03-10")), "2030-03-10");
        // 同じ月
        assert_eq!(name("2030-03-10", Some("2030-03-11")), "2030-03-10-11");
        assert_eq!(name("2030-03-01", Some("2030-03-14")), "2030-03-01-14");
        // 月をまたぐ（閏日を含む）
        assert_eq!(
            name("2030-04-30", Some("2030-05-02")),
            "2030-04-30_2030-05-02"
        );
        assert_eq!(
            name("2028-02-28", Some("2028-03-01")),
            "2028-02-28_2028-03-01"
        );
        // 年をまたぐ
        assert_eq!(
            name("2030-12-31", Some("2031-01-01")),
            "2030-12-31_2031-01-01"
        );
    }

    #[test]
    fn the_shape_check_matches_the_documented_pattern() {
        for ok in [
            "2030-03-10",
            "2030-03-10-11",
            "2030-04-30_2030-05-02",
            "0001-01-01",
        ] {
            assert!(is_valid_folder_name(ok), "{ok}");
        }
        for bad in [
            "",
            "2030-03-1",
            "2030-03-10-1",
            "2030-03-10-111",
            "2030-03-10_",
            "2030-03-10_2030-05",
            "2030-03-10_2030-05-02x",
            "2030-03-10/2030-03-11",
            "../2030-03-10",
            "2030-03-10/",
            "2030-03-10\n",
            "2030_03-10",
            "２０３０-03-10",
            "2030-03-10-1a",
            "2030-03-10_2030-05-0２",
        ] {
            assert!(!is_valid_folder_name(bad), "{bad:?}");
            assert_eq!(FolderName::parse(bad), None);
        }
    }
}
