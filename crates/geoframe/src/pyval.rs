//! JSON の値を、現行 Python が `str()`・`bool()`・`round()` で扱うのと同じに読む小さな関数（T3.5）。
//!
//! 旅行ブログの旅程 JSON・下書き JSON は、現行の `travel_blog.py` が `dict` のまま `or`・`str()`・f 文字列で
//! 扱う。型が想定どおり（文字列）なら素通しで済むが、手で書いた JSON の型が違っても Python と同じ文字に
//! なるように、スカラー値の変換をここにまとめる。配列・オブジェクトは Python の `repr` ではなく JSON の
//! 文字列にする（`repr` はコードポイントごとの「表示できる文字か」の表が要り、再現しない。server.md §20.8）。

use serde_json::Value;

use crate::pyquery::is_decimal_digit;

/// Python の真偽（`value or default` の左側）。`None`・`False`・0・`0.0`・空の文字列/配列/オブジェクトが偽。
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_none_or(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `Option<&Value>` の真偽（キーが無い＝`dict.get` が `None`＝偽）。
pub fn truthy_opt(value: Option<&Value>) -> bool {
    value.is_some_and(truthy)
}

/// Python の `str(value)`（f 文字列に埋めたときも同じ）。
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                n.as_f64().map_or_else(String::new, float_repr)
            }
        }
        Value::String(s) => s.clone(),
        // Python は `repr`。再現しないで JSON の文字列にする
        other => other.to_string(),
    }
}

/// `str(value or default)`。`value` が偽（無い・`null`・空・0）なら `default`。
pub fn py_str_or(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(v) if truthy(v) => py_str(v),
        _ => default.to_string(),
    }
}

/// Python の `repr(float)`（`1.0`・`1e-05`・`1e+16`）。有限の値だけを渡す。
pub fn float_repr(x: f64) -> String {
    let sci = format!("{x:e}"); // 最短の桁。例: 1.5e16 / 1e-5 / -2.5e-7
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    // 小数点の位置が (-4, 16] のとき固定小数、それ以外は指数表記（Python の 'r' 書式）
    if x == 0.0 || (-5 < exp && exp <= 15) {
        let mut text = format!("{x}");
        if !text.contains('.') {
            text.push_str(".0");
        }
        text
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    }
}

/// Python の `round(x, 3)`（小数 3 桁への最近接丸め。2 進の厳密な値で決まり、ちょうど半分は偶数側）。
/// Rust の固定小数点の書式も 2 進の厳密な値を偶数丸めするので、書いて読み直せば同じ値になる
/// （P1 の `round(x, 6)` と同じ方針）。
pub fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}

/// `str.isdigit()`（空は偽）。Unicode の Numeric_Type が Decimal か Digit の文字だけでできている。
pub fn is_py_digit_str(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_py_digit)
}

/// `isdigit` が真で `isdecimal` が偽の文字（上付き・下付き・丸数字など）。Python 3.13（Unicode 15.1）の
/// `unicodedata` から作った表。
const DIGIT_NOT_DECIMAL: [(u32, u32); 20] = [
    (0xb2, 0xb3),
    (0xb9, 0xb9),
    (0x1369, 0x1371),
    (0x19da, 0x19da),
    (0x2070, 0x2070),
    (0x2074, 0x2079),
    (0x2080, 0x2089),
    (0x2460, 0x2468),
    (0x2474, 0x247c),
    (0x2488, 0x2490),
    (0x24ea, 0x24ea),
    (0x24f5, 0x24fd),
    (0x24ff, 0x24ff),
    (0x2776, 0x277e),
    (0x2780, 0x2788),
    (0x278a, 0x2792),
    (0x10a40, 0x10a43),
    (0x10e60, 0x10e68),
    (0x11052, 0x1105a),
    (0x1f100, 0x1f10a),
];

fn is_py_digit(c: char) -> bool {
    if is_decimal_digit(c) {
        return true;
    }
    let code = u32::from(c);
    DIGIT_NOT_DECIMAL
        .iter()
        .any(|&(lo, hi)| (lo..=hi).contains(&code))
}

/// `s[:n]`（コードポイント数で切る）。
pub fn prefix_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 期待値は Python 3.13 の `repr(float)` で確かめたもの。
    #[test]
    fn float_repr_matches_python() {
        for (x, expected) in [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (-2.5, "-2.5"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e22, "1e+22"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (-1e-5, "-1e-05"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (123456789.123, "123456789.123"),
            (0.0, "0.0"),
        ] {
            assert_eq!(float_repr(x), expected, "{x:e}");
        }
    }

    #[test]
    fn py_str_and_truthiness_follow_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(-7)), "-7");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_str(&json!(2.0)), "2.0");
        assert_eq!(py_str(&json!("x")), "x");
        for falsy in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&falsy), "{falsy}");
        }
        for truthy_value in [json!(true), json!(1), json!(-0.5), json!("0"), json!([0])] {
            assert!(truthy(&truthy_value), "{truthy_value}");
        }
        assert_eq!(py_str_or(None, "d"), "d");
        assert_eq!(py_str_or(Some(&json!(0)), "d"), "d");
        assert_eq!(py_str_or(Some(&json!("a")), "d"), "a");
        assert_eq!(py_str_or(Some(&json!(12)), "d"), "12");
    }

    /// 期待値は Python 3.13 の `round(x, 3)`。
    #[test]
    fn round3_matches_python() {
        for (x, expected) in [
            (0.2 + 0.25 + 0.25 + 0.25 + 0.05, 1.0),
            (0.2 + 0.25 + 0.12 + 0.05, 0.62),
            (0.2 + 0.25, 0.45),
            (0.0005, 0.001),
            (0.0015, 0.002),
            (0.0025, 0.003),
            (1.0005, 1.0),
            (0.12345, 0.123),
        ] {
            assert_eq!(round3(x), expected, "{x}");
        }
    }

    /// 期待値は Python 3.13 の `str.isdigit()`。
    #[test]
    fn isdigit_matches_python() {
        for (s, expected) in [
            ("0", true),
            ("123", true),
            ("", false),
            ("１２", true),
            ("٣", true),
            ("²", true),
            ("①", true),
            ("½", false),
            ("Ⅷ", false),
            ("1a", false),
            ("-1", false),
            (" 1", false),
            ("１.", false),
            ("三", false),
        ] {
            assert_eq!(is_py_digit_str(s), expected, "{s:?}");
        }
    }

    #[test]
    fn prefix_counts_code_points() {
        assert_eq!(prefix_chars("あいうえお", 3), "あいう");
        assert_eq!(prefix_chars("ab", 5), "ab");
        assert_eq!(prefix_chars("😀😀😀", 2), "😀😀");
    }
}
