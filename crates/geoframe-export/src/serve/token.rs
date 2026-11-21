//! Bearer トークン（design.md 3.4・8 章）。ファイルから読む検査と、定数時間の照合。
//!
//! トークンの値は、ログにも応答にも、エラーの表示にも出さない（[`Token`] の `Debug` は伏せる。[`TokenError`] はパスも値も持たない）。
//!
//! ## ファイルの検査
//!
//! 開いたハンドルで見る（確かめたあとに差し替えられない）。最後の要素がシンボリックリンクなら断る（`O_NOFOLLOW`）。FIFO などで
//! 待たないよう `O_NONBLOCK` で開き、通常のファイルだと確かめてから読む。次をすべて満たさなければ起動しない。
//!
//! - 通常のファイル（ディレクトリ・FIFO・デバイスは不可）
//! - 実行ユーザーの所有
//! - グループ・その他の権限が無い（`0600`・`0400` など。`mode & 0o077 == 0`）
//! - 前後の改行（`\r`・`\n`）を除いて 32 バイト以上 [`MAX_LEN`] バイト以下。ファイルの大きさにも上限を掛ける（改行の余裕を足して 1,088 バイト）
//! - 文字は RFC 6750 の token68（`A-Za-z0-9` と `-._~+/`、末尾の `=` の詰め物）だけ。空白・制御文字・日本語は不可
//!   （`Authorization: Bearer <token>` の値としてそのまま照合できる文字の範囲）
//! - 異なるバイトが 8 種類以上（`aaaa…`・`abab…` のような弱い値を断る。16 進 32 文字の乱数でも、8 種類を下回る確率は無視できる）
//!
//! 親ディレクトリには何も求めない（設計が縛るのはファイルだけ。unit の `StateDirectory` や `/etc` の下に置ける）。親の途中の
//! シンボリックリンクは、`O_NOFOLLOW` では見ない（最後の要素だけ。配置時に所有者だけが書ける場所に置く）。

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// トークンの長さの下限と上限（バイト）。
pub const MIN_LEN: usize = 32;
pub const MAX_LEN: usize = 1024;
/// ファイルの大きさの上限（前後の改行の余裕を見る）。これを超える中身は読み切らずに断る。
const MAX_FILE: usize = MAX_LEN + 64;
/// 異なるバイトの種類の下限。
const MIN_DISTINCT: usize = 8;

/// トークンの値。`Debug` は伏せる。
#[derive(Clone, PartialEq, Eq)]
pub struct Token(Vec<u8>);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<伏せ字>)")
    }
}

impl Token {
    /// 前後の改行を除いて、上の文字・長さ・種類の検査にかける。
    pub fn parse(bytes: &[u8]) -> Result<Self, TokenError> {
        let start = bytes
            .iter()
            .position(|b| !matches!(b, b'\r' | b'\n'))
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !matches!(b, b'\r' | b'\n'))
            .map_or(start, |i| i + 1);
        let value = &bytes[start..end];
        if value.len() < MIN_LEN {
            return Err(TokenError::TooShort);
        }
        if value.len() > MAX_LEN {
            return Err(TokenError::TooLong);
        }
        let body = {
            let pad = value.iter().rev().take_while(|b| **b == b'=').count();
            &value[..value.len() - pad]
        };
        let plain = |b: &u8| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        };
        if body.is_empty() || !body.iter().all(plain) {
            return Err(TokenError::BadCharacters);
        }
        let mut seen = [false; 256];
        for b in value {
            seen[usize::from(*b)] = true;
        }
        if seen.iter().filter(|s| **s).count() < MIN_DISTINCT {
            return Err(TokenError::LowVariety);
        }
        Ok(Self(value.to_vec()))
    }

    /// `Authorization` ヘッダの値（`Bearer <token>`）が、このトークンか。スキームは大文字小文字を区別しない（RFC 7235）。
    /// トークンの照合は定数時間（[`ct_eq`]）。
    pub fn matches_authorization(&self, value: &[u8]) -> bool {
        const SCHEME: &[u8] = b"Bearer ";
        let presented =
            if value.len() >= SCHEME.len() && value[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
                &value[SCHEME.len()..]
            } else {
                // スキームが違っても、照合の時間は変えない（空の値と比べる）
                &[][..]
            };
        let ok = ct_eq(&self.0, presented);
        ok && presented.len() == self.0.len()
    }
}

/// 長さが違っても、比べる回数は `secret` の長さで決まる（`presented` の長さで時間が変わらない）。違いは、全バイトを見てから
/// まとめて判定する（途中で抜けない）。
fn ct_eq(secret: &[u8], presented: &[u8]) -> bool {
    let mut diff = secret.len() ^ presented.len();
    for (i, s) in secret.iter().enumerate() {
        let p = presented.get(i).copied().unwrap_or(0);
        diff |= usize::from(s ^ p);
    }
    std::hint::black_box(diff) == 0
}

/// トークンのファイルを受け付けない理由。パスも値も含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// 開けない・読めない（無い・権限・入出力）。
    Unreadable(io::ErrorKind),
    /// 通常のファイルでない（シンボリックリンク・ディレクトリ・FIFO など）。
    NotRegular,
    /// 実行ユーザーの持ち物でない。
    WrongOwner,
    /// グループ・その他から読める（または書ける）。
    TooPermissive,
    TooShort,
    TooLong,
    BadCharacters,
    LowVariety,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(kind) => write!(f, "トークンのファイルを読めません（{kind}）"),
            Self::NotRegular => f.write_str(
                "トークンのファイルが通常のファイルではありません（シンボリックリンクはたどりません）",
            ),
            Self::WrongOwner => f.write_str("トークンのファイルが実行ユーザーの持ち物ではありません"),
            Self::TooPermissive => f.write_str(
                "トークンのファイルをグループ・その他が読めます（0600 にしてください）",
            ),
            Self::TooShort => write!(f, "トークンが短すぎます（改行を除いて {MIN_LEN} バイト以上）"),
            Self::TooLong => write!(f, "トークンが長すぎます（{MAX_LEN} バイト以下）"),
            Self::BadCharacters => f.write_str(
                "トークンに使えない文字があります（英数字と -._~+/ と末尾の = だけ）",
            ),
            Self::LowVariety => f.write_str("トークンが単調すぎます（乱数で作った値にしてください）"),
        }
    }
}

impl std::error::Error for TokenError {}

/// 実行ユーザー（実効 UID）の持ち物として読む。
pub fn load_token(path: &Path) -> Result<Token, TokenError> {
    load_token_for_uid(path, crate::sys::euid())
}

/// 持ち主を `uid` として検査する（試験で「別のユーザーの持ち物」を作るため。通常は [`load_token`]）。
pub fn load_token_for_uid(path: &Path, uid: u32) -> Result<Token, TokenError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            // 最後の要素がシンボリックリンク
            Some(libc::ELOOP) => TokenError::NotRegular,
            _ => TokenError::Unreadable(e.kind()),
        })?;
    let meta = file
        .metadata()
        .map_err(|e| TokenError::Unreadable(e.kind()))?;
    if !meta.is_file() {
        return Err(TokenError::NotRegular);
    }
    if meta.uid() != uid {
        return Err(TokenError::WrongOwner);
    }
    if meta.mode() & 0o077 != 0 {
        return Err(TokenError::TooPermissive);
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| TokenError::Unreadable(e.kind()))?;
    if bytes.len() > MAX_FILE {
        return Err(TokenError::TooLong);
    }
    Token::parse(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "Zq3vK9tR7mB2xLw8Hn5cYd4JpF6sGa1E";

    #[test]
    fn a_long_enough_plain_value_is_accepted_with_or_without_newlines() {
        for text in [
            GOOD.to_owned(),
            format!("{GOOD}\n"),
            format!("\r\n{GOOD}\r\n\n"),
        ] {
            assert!(Token::parse(text.as_bytes()).is_ok(), "{text:?}");
        }
        // base64 の詰め物（末尾の =）と、URL 向けの記号
        assert!(Token::parse(format!("{GOOD}+/-._~==").as_bytes()).is_ok());
    }

    #[test]
    fn short_odd_or_monotonous_values_are_refused() {
        assert_eq!(Token::parse(b""), Err(TokenError::TooShort));
        assert_eq!(Token::parse(b"\n\n"), Err(TokenError::TooShort));
        assert_eq!(
            Token::parse(&GOOD.as_bytes()[..31]),
            Err(TokenError::TooShort)
        );
        // 前後の改行は長さに数えない（改行で 32 バイトにはしない）
        assert_eq!(
            Token::parse(format!("{}\n\n\n\n", &GOOD[..28]).as_bytes()),
            Err(TokenError::TooShort)
        );
        // 中の空白・改行・制御文字・日本語
        for bad in [
            format!("{} {}", &GOOD[..16], &GOOD[16..]),
            format!("{}\n{}", &GOOD[..16], &GOOD[16..]),
            format!("{}\t{}", &GOOD[..16], &GOOD[16..]),
            format!("{GOOD}あ"),
            format!("{GOOD}\"x"),
            format!("{GOOD}\0"),
        ] {
            assert_eq!(
                Token::parse(bad.as_bytes()),
                Err(TokenError::BadCharacters),
                "{bad:?}"
            );
        }
        assert_eq!(
            Token::parse("=".repeat(40).as_bytes()),
            Err(TokenError::BadCharacters)
        );
        assert_eq!(
            Token::parse("a".repeat(40).as_bytes()),
            Err(TokenError::LowVariety)
        );
        assert_eq!(
            Token::parse("ab".repeat(20).as_bytes()),
            Err(TokenError::LowVariety)
        );
        assert_eq!(
            Token::parse("x".repeat(MAX_LEN + 1).as_bytes()),
            Err(TokenError::TooLong)
        );
    }

    #[test]
    fn the_header_is_matched_exactly() {
        let t = Token::parse(GOOD.as_bytes()).unwrap();
        for ok in [
            format!("Bearer {GOOD}"),
            format!("bearer {GOOD}"),
            format!("BEARER {GOOD}"),
        ] {
            assert!(t.matches_authorization(ok.as_bytes()), "{ok}");
        }
        for bad in [
            String::new(),
            GOOD.to_owned(),
            format!("Basic {GOOD}"),
            format!("Bearer{GOOD}"),
            format!("Bearer  {GOOD}"),
            format!("Bearer {GOOD} "),
            format!("Bearer {GOOD}x"),
            format!("Bearer {}", &GOOD[..GOOD.len() - 1]),
            format!("Bearer {}", GOOD.to_lowercase()),
            "Bearer ".to_owned(),
        ] {
            assert!(!t.matches_authorization(bad.as_bytes()), "{bad:?}");
        }
    }

    #[test]
    fn the_value_never_appears_in_debug_output() {
        let t = Token::parse(GOOD.as_bytes()).unwrap();
        assert!(!format!("{t:?}").contains(&GOOD[..8]));
    }

    #[test]
    fn the_comparison_does_not_depend_on_where_the_values_differ() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"abc", b""));
        // 長さが違うだけで、先頭が同じでも一致しない（末尾が 0 バイトの詰め物でも）
        assert!(!ct_eq(b"abc", b"abc\0"));
        assert!(!ct_eq(b"ab\0", b"ab"));
    }
}
