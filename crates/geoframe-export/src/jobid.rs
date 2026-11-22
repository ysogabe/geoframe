//! ジョブ ID（design.md 7 章）。サーバーが作る 128 ビットの乱数で、形は小文字の 16 進 32 文字に固定する。
//! Drive のパス（`incoming/<job_id>`、`replaced/<種類>-<フォルダ名>-<job_id>`）と、作業領域・ジョブの記録のファイル名に入るので、
//! 形を確かめていない文字列から [`JobId`] は作れない。

use std::io::Read;

use crate::error::{ErrorCode, ExportError};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    pub const LEN: usize = 32;

    /// 形の検査（`[0-9a-f]{32}`）。記録から読んだ文字列も、使う前にここを通す。
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == Self::LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(s.to_owned()))
    }

    /// OS の乱数（`/dev/urandom`）から作る。
    pub fn generate() -> Result<Self, ExportError> {
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|_| ExportError::new(ErrorCode::Internal, "乱数を読めません"))?;
        let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Self::parse(&s).ok_or(ExportError::from(ErrorCode::Internal))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_32_lowercase_hex_digits_are_accepted() {
        let ok = "0123456789abcdef0123456789abcdef";
        assert_eq!(JobId::parse(ok).unwrap().as_str(), ok);
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcdeg",
            "../23456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcde\n",
            "0123456789abcdef0123456789abcde/",
        ] {
            assert!(JobId::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn generated_ids_have_the_fixed_shape_and_differ() {
        let a = JobId::generate().unwrap();
        let b = JobId::generate().unwrap();
        assert!(JobId::parse(a.as_str()).is_some());
        assert_ne!(a, b);
    }
}
