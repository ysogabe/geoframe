//! 固定のエラーコード（design.md 6.1）。HTTP の 400 の `{"error": ...}` にも、ジョブの `error.code` にも、
//! `run` の標準出力にも同じ文字列を使う。メッセージは固定の短い説明で、受け取った値・rclone の出力・設定の中身は含めない。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // 要求の検査（HTTP は 400、CLI は終了コード 2）
    /// `from`・`to` が `YYYY-MM-DD` の実在する日付でない（`from` が無いときも）。
    InvalidDate,
    /// `to` が `from` より前、または期間が 14 日（両端を含む）を超える。
    InvalidRange,
    InvalidDestination,
    InvalidOnExists,
    InvalidRequestId,
    // ジョブの失敗（design.md 6.1 の `error.code`）
    FeedUnavailable,
    FeedInvalid,
    TooManyPhotos,
    DerivativePending,
    FetchFailed,
    DecodeFailed,
    NothingToExport,
    DriveUnavailable,
    AmbiguousDestination,
    RcloneFailed,
    RcloneTimeout,
    VerifyMismatch,
    Timeout,
    Blocked,
    Internal,
    // 受付の断り（HTTP は 409。ジョブの `error.code` には載らない。CLI の `run` の標準出力にも同じ文字列を使う）
    /// 実行枠（flock）が取れない（別のジョブか CLI の `run` が実行中）。
    Busy,
    /// 同じ `request_id` で、期間・宛先・`on_exists` が違う。
    RequestIdMismatch,
}

impl ErrorCode {
    /// 全部（記録から読んだ文字列を [`ErrorCode::parse`] で戻すのに使う）。
    pub const ALL: [ErrorCode; 22] = [
        Self::InvalidDate,
        Self::InvalidRange,
        Self::InvalidDestination,
        Self::InvalidOnExists,
        Self::InvalidRequestId,
        Self::FeedUnavailable,
        Self::FeedInvalid,
        Self::TooManyPhotos,
        Self::DerivativePending,
        Self::FetchFailed,
        Self::DecodeFailed,
        Self::NothingToExport,
        Self::DriveUnavailable,
        Self::AmbiguousDestination,
        Self::RcloneFailed,
        Self::RcloneTimeout,
        Self::VerifyMismatch,
        Self::Timeout,
        Self::Blocked,
        Self::Internal,
        Self::Busy,
        Self::RequestIdMismatch,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidDate => "invalid_date",
            Self::InvalidRange => "invalid_range",
            Self::InvalidDestination => "invalid_destination",
            Self::InvalidOnExists => "invalid_on_exists",
            Self::InvalidRequestId => "invalid_request_id",
            Self::FeedUnavailable => "feed_unavailable",
            Self::FeedInvalid => "feed_invalid",
            Self::TooManyPhotos => "too_many_photos",
            Self::DerivativePending => "derivative_pending",
            Self::FetchFailed => "fetch_failed",
            Self::DecodeFailed => "decode_failed",
            Self::NothingToExport => "nothing_to_export",
            Self::DriveUnavailable => "drive_unavailable",
            Self::AmbiguousDestination => "ambiguous_destination",
            Self::RcloneFailed => "rclone_failed",
            Self::RcloneTimeout => "rclone_timeout",
            Self::VerifyMismatch => "verify_mismatch",
            Self::Timeout => "timeout",
            Self::Blocked => "blocked",
            Self::Internal => "internal",
            Self::Busy => "busy",
            Self::RequestIdMismatch => "request_id_mismatch",
        }
    }

    /// 固定の短い説明（値を含まない）。
    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidDate => "日付は YYYY-MM-DD の実在する日付で指定してください",
            Self::InvalidRange => "期間は from 以上で、両端を含めて 14 日以下にしてください",
            Self::InvalidDestination => "destination は yamareco か travel-journal です",
            Self::InvalidOnExists => "on_exists は error か replace です",
            Self::InvalidRequestId => "request_id は英数字・_・- の 1〜64 文字です",
            Self::FeedUnavailable => {
                "写真サービスの一覧 API に接続できない、または 200 以外が返りました"
            }
            Self::FeedInvalid => "写真サービスの一覧 API の応答が検査に通りません",
            Self::TooManyPhotos => "期間の写真が上限（2,000 件）を超えています",
            Self::DerivativePending => "表示用 JPEG がまだ無い写真があります",
            Self::FetchFailed => "写真の取得に失敗しました",
            Self::DecodeFailed => "写真のデコードに失敗しました",
            Self::NothingToExport => "書き出せる写真がありません",
            Self::DriveUnavailable => "Drive に照会できません",
            Self::AmbiguousDestination => "書き出し先に同じ名前のものが複数あります",
            Self::RcloneFailed => "rclone が失敗しました",
            Self::RcloneTimeout => "rclone が時間内に終わりませんでした",
            Self::VerifyMismatch => "送った内容が manifest と一致しません",
            Self::Timeout => "ジョブ全体の期限を過ぎました",
            Self::Blocked => "前の書き出しの状態を照合できていません",
            Self::Internal => "内部エラー",
            Self::Busy => "実行中の書き出しがあります",
            Self::RequestIdMismatch => "同じ request_id で、期間・宛先・on_exists が違います",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// コードと、受け取った値を含まない固定の理由（ログ向け）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportError {
    pub code: ErrorCode,
    pub detail: &'static str,
}

impl ExportError {
    pub const fn new(code: ErrorCode, detail: &'static str) -> Self {
        Self { code, detail }
    }
}

impl From<ErrorCode> for ExportError {
    fn from(code: ErrorCode) -> Self {
        Self {
            code,
            detail: code.message(),
        }
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for ExportError {}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ErrorCode; 22] = ErrorCode::ALL;

    #[test]
    fn codes_are_fixed_unique_snake_case_strings() {
        let mut seen = std::collections::HashSet::new();
        for code in ALL {
            let s = code.as_str();
            assert!(!s.is_empty());
            assert!(
                s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{s}"
            );
            assert!(seen.insert(s), "重複: {s}");
            assert!(!code.message().is_empty());
        }
    }

    #[test]
    fn the_codes_named_in_the_design_keep_their_spelling() {
        // 呼び出し側が文字列で判定するので、綴りを固定する
        assert_eq!(ErrorCode::InvalidDate.as_str(), "invalid_date");
        assert_eq!(ErrorCode::FeedUnavailable.as_str(), "feed_unavailable");
        assert_eq!(ErrorCode::FeedInvalid.as_str(), "feed_invalid");
        assert_eq!(ErrorCode::TooManyPhotos.as_str(), "too_many_photos");
        assert_eq!(ErrorCode::DerivativePending.as_str(), "derivative_pending");
        assert_eq!(ErrorCode::NothingToExport.as_str(), "nothing_to_export");
        assert_eq!(ErrorCode::Busy.as_str(), "busy");
        assert_eq!(ErrorCode::RequestIdMismatch.as_str(), "request_id_mismatch");
    }

    #[test]
    fn every_code_parses_back_from_its_string() {
        for code in ALL {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(ErrorCode::parse(""), None);
        assert_eq!(ErrorCode::parse("Internal"), None);
        assert_eq!(ErrorCode::parse("interrupted"), None);
    }
}
