//! 管理画面の読み取り API のクエリを、現行 Python と同じに解釈する小さな関数（T1.4）。
//!
//! 現行の `handlers.py` は `urllib.parse.parse_qs(urlparse(self.path).query)` で取り出した最初の値を
//! `int()` に渡し、`ValueError` なら 400 `bad_params` にする。`parse_qs` の既定（空の値は無かったことに
//! する、`+` は空白、`%XX` を戻す、同じ名前の2つ目以降は使わない）と、`int()` の寛容さ（前後の空白、
//! 符号、数字の間の `_`、全角などの Unicode の 10 進数字、桁数の上限）を合わせてある。
//! 値は 64 ビットに飽和させる（使い道は `min`/`max` での丸めだけで、桁あふれしても結果は同じ）。
//! 仕様と差は docs/rust-migration/server.md §10。
//!
//! T3.3 で、地名検索（`geocode.rs`）が使う `str.strip()`・`float(str)`・`urllib.parse.quote` も加えた。

use std::collections::HashMap;

use crate::webpath::percent_decode;

/// `int(str)` が受け付ける桁数の上限（`sys.int_info.default_max_str_digits`）。`_` は数えない。
const MAX_DIGITS: usize = 4300;

/// `parse_qs(query)` の各名前の最初の値。`#` 以降は落とす（`urlparse` がフラグメントとして分ける）。
///
/// 値が空の項目（`page=`）と `=` の無い項目（`page`）は、`keep_blank_values=False` なので無かったことに
/// なる（`page=&page=2` の `page` は `2`）。名前も値も `+` を空白にしてから `%XX` を戻し、不正な
/// UTF-8 は U+FFFD にする。
pub fn parse_qs_first(query: &str) -> HashMap<String, String> {
    let query = query.split('#').next().unwrap_or("");
    let mut out = HashMap::new();
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        out.entry(unquote_plus(name))
            .or_insert_with(|| unquote_plus(value));
    }
    out
}

fn unquote_plus(s: &str) -> String {
    let spaced = s.replace('+', " ");
    String::from_utf8_lossy(&percent_decode(&spaced)).into_owned()
}

/// Python の `int(str)`。受け付けない形（`ValueError`）は `None`、桁あふれは `i64` の端に飽和させる。
pub fn py_int(s: &str) -> Option<i64> {
    let trimmed = s.trim_matches(is_int_space);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let mut value: i64 = 0;
    let mut count = 0usize;
    let mut after_digit = false;
    for c in digits.chars() {
        if c == '_' {
            // 数字と数字の間に1つだけ
            if !after_digit {
                return None;
            }
            after_digit = false;
            continue;
        }
        let d = decimal_digit(c)?;
        count += 1;
        value = value.saturating_mul(10).saturating_add(i64::from(d));
        after_digit = true;
    }
    // 空・符号だけ・末尾の `_` は不可
    if !after_digit || count > MAX_DIGITS {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `int()` が前後から落とす空白。ASCII は ` \t\n\v\f\r` だけ（`\x1c`〜`\x1f` は落とさない）、
/// ASCII 以外は Unicode の空白（`\xa0`・`\x85`・全角空白など）。
fn is_int_space(c: char) -> bool {
    if c.is_ascii() {
        matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
    } else {
        c.is_whitespace()
    }
}

/// Python の `str.strip()`（引数なし）。`str.isspace` が真の文字を前後から落とす。Rust の `trim` が落とす
/// Unicode の `White_Space` に、`\x1c`〜`\x1f`（FS・GS・RS・US）を足したもの。
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python の `float(str)`。受け付けない形（`ValueError`）は `None`。`inf`・`nan`・桁あふれ（`1e400`）は
/// `Some` で返す（呼び出し側が有限かを見る）。
///
/// CPython と同じく、まず ASCII 以外の 10 進数字を ASCII に、ASCII 以外の空白を ` ` にし（その他の ASCII 以外の
/// 文字は不可。ASCII の `\x1c`〜`\x1f` はここでは空白にならない）、前後の ASCII の空白（` \t\n\v\f\r`）を落として、
/// `_` を数字と数字の間にだけ許し（取り除く）、残りを `[符号]数字[.数字][e[符号]数字]`・`inf`・`infinity`・`nan`
/// （大文字小文字を問わない）として読む。
pub fn py_float(s: &str) -> Option<f64> {
    let mut ascii = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            ascii.push(c);
        } else if let Some(d) = decimal_digit(c) {
            ascii.push(char::from(b'0' + d));
        } else if c.is_whitespace() {
            ascii.push(' ');
        } else {
            return None;
        }
    }
    let trimmed = ascii.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let bytes = trimmed.as_bytes();
    let mut digits = String::with_capacity(trimmed.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let before = i.checked_sub(1).map(|j| bytes[j]);
            let after = bytes.get(i + 1).copied();
            if !before.is_some_and(|c| c.is_ascii_digit())
                || !after.is_some_and(|c| c.is_ascii_digit())
            {
                return None;
            }
        } else {
            digits.push(char::from(b));
        }
    }
    // Rust の `f64::from_str` は、CPython の `float` が受け付ける形（上の文法）を過不足なく読む
    digits.parse::<f64>().ok()
}

/// Python の `urllib.parse.quote(s)`（`safe='/'`）。UTF-8 にして、英数字と `_.-~/` 以外を `%XX`（大文字）にする。
pub fn py_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 各 10 進数字の並び（Unicode の Nd）の `0` の位置。ASCII 以外。Python 3.13（Unicode 15.1）の
/// `unicodedata` から作った表で、どの並びも `0`〜`9` の 10 文字が連続する。
const DIGIT_ZEROS: [u32; 67] = [
    0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6,
    0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50,
    0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16A60, 0x16AC0,
    0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E950,
    0x1FBF0,
];

/// `str.isdecimal()` が真の文字（Unicode の Nd）。
pub fn is_decimal_digit(c: char) -> bool {
    decimal_digit(c).is_some()
}

fn decimal_digit(c: char) -> Option<u8> {
    if c.is_ascii() {
        return c.to_digit(10).map(|d| d as u8);
    }
    let code = u32::from(c);
    let i = DIGIT_ZEROS
        .partition_point(|&zero| zero <= code)
        .checked_sub(1)?;
    let offset = code - DIGIT_ZEROS[i];
    (offset < 10).then_some(offset as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qs(query: &str) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = parse_qs_first(query).into_iter().collect();
        pairs.sort();
        pairs
    }

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    /// 期待値は Python 3.13 の `urllib.parse.parse_qs` で確かめたもの。
    #[test]
    fn parse_qs_matches_python() {
        assert_eq!(qs(""), vec![]);
        assert_eq!(
            qs("page=2&per=10&filter=fav"),
            vec![pair("filter", "fav"), pair("page", "2"), pair("per", "10")]
        );
        // 空の値・`=` の無い項目は無かったことになる。2つ目に値があればそちら
        assert_eq!(qs("page=&per"), vec![]);
        assert_eq!(qs("page=&page=2"), vec![pair("page", "2")]);
        // 同じ名前は最初の値
        assert_eq!(qs("page=2&page=3"), vec![pair("page", "2")]);
        // `+` は空白、%XX は戻す、名前の側も同じ
        assert_eq!(qs("filter=a+b%21"), vec![pair("filter", "a b!")]);
        assert_eq!(qs("%70age=4"), vec![pair("page", "4")]);
        // 壊れた % はそのまま、不正な UTF-8 は U+FFFD
        assert_eq!(qs("filter=%zz%"), vec![pair("filter", "%zz%")]);
        assert_eq!(qs("filter=%ff"), vec![pair("filter", "\u{fffd}")]);
        // `;` は区切りではない（3.10 以降）。値に `=` を含んでよい
        assert_eq!(qs("page=1;per=2"), vec![pair("page", "1;per=2")]);
        assert_eq!(qs("filter=a=b"), vec![pair("filter", "a=b")]);
        // 空の項目は飛ばす。フラグメントは落とす
        assert_eq!(qs("&&page=3&"), vec![pair("page", "3")]);
        assert_eq!(qs("page=5#per=9"), vec![pair("page", "5")]);
    }

    /// 期待値は Python 3.13 の `int()` で確かめたもの（`None` は `ValueError`）。
    #[test]
    fn py_int_matches_python() {
        for (input, expected) in [
            ("1", Some(1)),
            ("007", Some(7)),
            ("+5", Some(5)),
            ("-3", Some(-3)),
            (" 4 ", Some(4)),
            ("\t5\n", Some(5)),
            ("\x0b7", Some(7)),
            ("\u{a0}6\u{3000}", Some(6)),
            ("\u{85}1", Some(1)),
            ("1_000", Some(1000)),
            ("0_1", Some(1)),
            ("00_1", Some(1)),
            ("１２", Some(12)),
            ("＋１", None),
            ("+１", Some(1)),
            ("٣", Some(3)),
            ("１_２", Some(12)),
            ("1__0", None),
            ("1_", None),
            ("_1", None),
            ("+_1", None),
            ("0_", None),
            ("+-1", None),
            ("- 1", None),
            ("", None),
            ("  ", None),
            ("+", None),
            ("_", None),
            ("1 2", None),
            ("1.5", None),
            ("1e3", None),
            ("0x10", None),
            ("abc", None),
            // `\x1c`〜`\x1f` と ` ` は（`str.isspace` は真だが）`int()` は落とさない
            ("\x1c1", None),
            ("1\u{2028}1", None),
        ] {
            assert_eq!(py_int(input), expected, "{input:?}");
        }
    }

    #[test]
    fn py_int_saturates_and_limits_digits() {
        assert_eq!(py_int("99999999999999999999999"), Some(i64::MAX));
        assert_eq!(py_int("-99999999999999999999999"), Some(-i64::MAX));
        assert_eq!(py_int(&"1".repeat(4300)), Some(i64::MAX));
        // 4301 桁は ValueError。`_` と先頭の 0 は桁として数える/数えない（`_` は数えない）
        assert_eq!(py_int(&"1".repeat(4301)), None);
        assert_eq!(py_int(&format!("{}5", "0".repeat(5000))), None);
        assert_eq!(py_int(&"1_".repeat(3000)), None); // 末尾が `_`
        assert_eq!(py_int(&format!("{}1", "1_".repeat(3000))), Some(i64::MAX));
        assert_eq!(py_int(&format!("{}1", "1_".repeat(4300))), None);
    }

    #[test]
    fn digit_table_edges() {
        assert_eq!(decimal_digit('\u{660}'), Some(0));
        assert_eq!(decimal_digit('\u{669}'), Some(9));
        assert_eq!(decimal_digit('\u{66a}'), None);
        assert_eq!(decimal_digit('\u{ff19}'), Some(9));
        assert_eq!(decimal_digit('\u{1fbf9}'), Some(9));
        assert_eq!(decimal_digit('é'), None);
        // 先頭より前の文字
        assert_eq!(decimal_digit('\u{7f}'), None);
    }
}
