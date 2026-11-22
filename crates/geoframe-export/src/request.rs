//! 書き出しの要求の検査（design.md 6.1 の `POST /v1/exports` の引数。HTTP と CLI で共通）。
//!
//! HTTP の本文（JSON）も CLI の引数も、まず [`RawRequest`]（文字列のまま）に入れて [`RawRequest::validate`] に通す。
//! 型や未知のキーの検査（本文の上限・未知のキーは 400）は serve（T6）が JSON を読むときに行う。

use std::fmt;

use crate::date::Period;
use crate::error::ErrorCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Yamareco,
    TravelJournal,
}

impl Destination {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "yamareco" => Some(Self::Yamareco),
            "travel-journal" => Some(Self::TravelJournal),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yamareco => "yamareco",
            Self::TravelJournal => "travel-journal",
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 宛先が既にあるときの扱い。既定は `error`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnExists {
    #[default]
    Error,
    Replace,
}

impl OnExists {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "error" => Some(Self::Error),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Replace => "replace",
        }
    }
}

/// 呼び出し側が付ける再送の目印。`[A-Za-z0-9_-]{1,64}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn parse(s: &str) -> Option<Self> {
        let ok = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        ok.then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 検査前の要求（文字列のまま）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawRequest {
    pub from: Option<String>,
    pub to: Option<String>,
    pub destination: Option<String>,
    pub on_exists: Option<String>,
    pub dry_run: bool,
    pub request_id: Option<String>,
}

/// 検査済みの要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    pub period: Period,
    pub destination: Destination,
    pub on_exists: OnExists,
    pub dry_run: bool,
    pub request_id: Option<RequestId>,
}

impl RawRequest {
    /// 検査する。見る順は `from`・`to`（形・暦は `invalid_date`、順序・長さは `invalid_range`）、`destination`、
    /// `on_exists`、`request_id`。最初に見つかった誤りだけを返す。
    pub fn validate(&self) -> Result<ExportRequest, ErrorCode> {
        let period = Period::parse(self.from.as_deref(), self.to.as_deref())?;
        let destination = self
            .destination
            .as_deref()
            .and_then(Destination::parse)
            .ok_or(ErrorCode::InvalidDestination)?;
        let on_exists = match self.on_exists.as_deref() {
            None => OnExists::default(),
            Some(s) => OnExists::parse(s).ok_or(ErrorCode::InvalidOnExists)?,
        };
        let request_id = match self.request_id.as_deref() {
            None => None,
            Some(s) => Some(RequestId::parse(s).ok_or(ErrorCode::InvalidRequestId)?),
        };
        Ok(ExportRequest {
            period,
            destination,
            on_exists,
            dry_run: self.dry_run,
            request_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(from: &str, to: Option<&str>, dest: &str) -> RawRequest {
        RawRequest {
            from: Some(from.to_owned()),
            to: to.map(str::to_owned),
            destination: Some(dest.to_owned()),
            ..RawRequest::default()
        }
    }

    #[test]
    fn a_minimal_request_gets_the_defaults() {
        let r = raw("2030-03-10", None, "yamareco").validate().unwrap();
        assert_eq!(r.period.from(), r.period.to());
        assert_eq!(r.destination, Destination::Yamareco);
        assert_eq!(r.on_exists, OnExists::Error);
        assert!(!r.dry_run);
        assert_eq!(r.request_id, None);
    }

    #[test]
    fn every_field_is_checked_and_reported_with_its_own_code() {
        assert_eq!(
            RawRequest::default().validate(),
            Err(ErrorCode::InvalidDate)
        );
        assert_eq!(
            raw("2030-03-10", Some("2030-03-09"), "yamareco").validate(),
            Err(ErrorCode::InvalidRange)
        );
        assert_eq!(
            raw("2030-03-10", None, "").validate(),
            Err(ErrorCode::InvalidDestination)
        );
        assert_eq!(
            raw("2030-03-10", None, "yamareco/../x").validate(),
            Err(ErrorCode::InvalidDestination)
        );
        assert_eq!(
            raw("2030-03-10", None, "Yamareco").validate(),
            Err(ErrorCode::InvalidDestination)
        );
        let mut r = raw("2030-03-10", None, "travel-journal");
        assert_eq!(
            r.validate().map(|r| r.destination),
            Ok(Destination::TravelJournal)
        );
        r.on_exists = Some("append".to_owned());
        assert_eq!(r.validate(), Err(ErrorCode::InvalidOnExists));
        r.on_exists = Some("replace".to_owned());
        assert_eq!(r.validate().map(|r| r.on_exists), Ok(OnExists::Replace));
        r.dry_run = true;
        assert!(r.validate().unwrap().dry_run);
    }

    #[test]
    fn request_ids_use_a_narrow_alphabet_and_length() {
        for ok in ["a", "A-b_9", &"x".repeat(64)] {
            assert_eq!(RequestId::parse(ok).unwrap().as_str(), ok);
        }
        for bad in [
            "",
            &"x".repeat(65),
            "a b",
            "a/b",
            "a.b",
            "日本",
            "a\n",
            "a=b",
        ] {
            assert_eq!(RequestId::parse(bad), None, "{bad:?}");
        }
        let mut r = raw("2030-03-10", None, "yamareco");
        r.request_id = Some("../x".to_owned());
        assert_eq!(r.validate(), Err(ErrorCode::InvalidRequestId));
        r.request_id = Some("job-1".to_owned());
        assert_eq!(r.validate().unwrap().request_id, RequestId::parse("job-1"));
    }
}
