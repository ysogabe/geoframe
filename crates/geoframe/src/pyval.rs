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

/// Python の `dict` のキーとして等しいかを見るための値（`hash`・`==` が同じものは同じキー）。
/// 数は値で比べる（`5` と `5.0` と `True`・`1`、`0` と `0.0` と `False` は同じ。`"5"` は別）。
/// 整数と浮動小数の `==` は厳密なので、2^53 を超える整数と、それに丸まる浮動小数は別になる。
/// `u64` を超える整数は `serde_json` が `f64` にする（server.md §0）ので、その浮動小数と同じキーになる
/// （Python は別）。`NaN` は `serde_json` が読まないので現れない。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DictKey {
    None,
    Int(i128),
    /// 整数でない浮動小数のビット列（`-0.0` は整数の 0 に寄るのでここには来ない）。
    Float(u64),
    Str(String),
}

/// `dict` のキーにできる値のキー。配列・オブジェクトは Python が `TypeError`（unhashable）なので `None`。
pub fn dict_key(value: &Value) -> Option<DictKey> {
    Some(match value {
        Value::Null => DictKey::None,
        Value::Bool(b) => DictKey::Int(i128::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DictKey::Int(i128::from(i))
            } else if let Some(u) = n.as_u64() {
                DictKey::Int(i128::from(u))
            } else {
                let f = n.as_f64()?;
                // i128 に収まる整数値の浮動小数は、同じ値の整数と等しい（2^126 未満なら厳密に変換できる）
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 2f64.powi(126) {
                    DictKey::Int(f as i128)
                } else {
                    DictKey::Float(f.to_bits())
                }
            }
        }
        Value::String(s) => DictKey::Str(s.clone()),
        Value::Array(_) | Value::Object(_) => return None,
    })
}

/// `s[:n]`（コードポイント数で切る）。
pub fn prefix_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python の `json.dumps(value, ensure_ascii=False, indent=2)` と同じバイト列（末尾の改行は付けない）。
/// `serde_json` の `PrettyFormatter`（インデントと区切りは同じ）に、浮動小数だけ Python の `repr` を使う。
/// 違いが残るのは、`u64` を超える整数（`serde_json` が浮動小数にする。Python は桁をそのまま出す）。
/// キーの並びは `Value` の並び（`serde_json` の `preserve_order`）で、Python の挿入順と同じ。
pub fn dumps_indent2(value: &Value) -> Vec<u8> {
    use serde::Serialize;
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PyFormatter::default());
    value
        .serialize(&mut serializer)
        .expect("JSON を直列化できません");
    out
}

/// インデント 2 の整形（`serde_json::ser::PrettyFormatter` と同じ動き）で、浮動小数は [`float_repr`]。
#[derive(Default)]
struct PyFormatter {
    depth: usize,
    has_value: bool,
}

impl PyFormatter {
    fn indent<W: ?Sized + std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        for _ in 0..self.depth {
            w.write_all(b"  ")?;
        }
        Ok(())
    }

    fn begin_item<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        w.write_all(if first { b"\n" } else { b",\n" })?;
        self.indent(w)
    }

    fn end_container<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        close: &[u8],
    ) -> std::io::Result<()> {
        self.depth -= 1;
        if self.has_value {
            w.write_all(b"\n")?;
            self.indent(w)?;
        }
        w.write_all(close)
    }
}

impl serde_json::ser::Formatter for PyFormatter {
    fn write_f64<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        value: f64,
    ) -> std::io::Result<()> {
        w.write_all(float_repr(value).as_bytes())
    }

    fn begin_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        w.write_all(b"[")
    }

    fn end_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.end_container(w, b"]")
    }

    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.begin_item(w, first)
    }

    fn end_array_value<W: ?Sized + std::io::Write>(&mut self, _: &mut W) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        w.write_all(b"{")
    }

    fn end_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.end_container(w, b"}")
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.begin_item(w, first)
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }

    fn end_object_value<W: ?Sized + std::io::Write>(&mut self, _: &mut W) -> std::io::Result<()> {
        self.has_value = true;
        Ok(())
    }
}

/// Python の `str.isspace()` が真になる文字（`strip()`・`split()` が空白とみなすもの）。Rust の
/// `char::is_whitespace` に、区切り文字 U+001C〜U+001F を足したもの。
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// `html.escape(s, quote=True)`。
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
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

    /// 期待値は Python 3.13 の `json.dumps(v, ensure_ascii=False, indent=2)` の出力。
    #[test]
    fn dumps_indent2_matches_python() {
        let value: Value = serde_json::from_str(
            r#"{"b": [], "a": {}, "c": [1, 2.5, "あ/\"\n\t\u0001\u007f\u2028", 1e22, 1e-05, -0.0],
                "d": null, "e": true, "f": {"x": [{"y": 1}, [], [[]]]}}"#,
        )
        .unwrap();
        let python = "{\n  \"b\": [],\n  \"a\": {},\n  \"c\": [\n    1,\n    2.5,\n    \"あ/\\\"\\n\\t\\u0001\u{7f}\u{2028}\",\n    1e+22,\n    1e-05,\n    -0.0\n  ],\n  \"d\": null,\n  \"e\": true,\n  \"f\": {\n    \"x\": [\n      {\n        \"y\": 1\n      },\n      [],\n      [\n        []\n      ]\n    ]\n  }\n}";
        assert_eq!(String::from_utf8(dumps_indent2(&value)).unwrap(), python);
        assert_eq!(dumps_indent2(&json!({})), b"{}");
        assert_eq!(dumps_indent2(&json!([])), b"[]");
        // u64 を超える整数は浮動小数になる（Python は桁をそのまま出す。server.md §21）
        let big: Value = serde_json::from_str("123456789012345678901234567890").unwrap();
        assert_eq!(dumps_indent2(&big), b"1.2345678901234568e+29");
    }

    #[test]
    fn python_whitespace_and_html_escape() {
        for c in [
            '\u{1c}', '\u{1f}', ' ', '\t', '\u{85}', '\u{a0}', '\u{3000}', '\u{2028}',
        ] {
            assert!(is_py_space(c), "{c:?}");
        }
        for c in [
            'a',
            '\u{200b}',
            '\u{feff}',
            '\u{1b}',
            '\u{20}'.to_ascii_uppercase(),
        ] {
            assert_eq!(is_py_space(c), c == ' ', "{c:?}");
        }
        assert_eq!(
            html_escape("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
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

    /// 期待値は Python 3.13 の `{k: ...}`（同じキーになるか）で確かめたもの。
    #[test]
    fn dict_keys_follow_python_equality() {
        let key = |v: Value| dict_key(&v).expect("hashable");
        let same = |a: Value, b: Value| key(a) == key(b);
        assert!(same(json!(5), json!(5.0)));
        assert!(same(json!(1), json!(true)));
        assert!(same(json!(1.0), json!(true)));
        assert!(same(json!(0), json!(false)));
        assert!(same(json!(0.0), json!(false)));
        assert!(same(json!(0), json!(-0.0)));
        assert!(same(json!(100), serde_json::from_str("1e2").unwrap()));
        assert!(same(json!(1.5), json!(1.5)));
        assert!(same(json!("a"), json!("a")));
        assert!(same(json!(null), json!(null)));
        assert!(!same(json!("5"), json!(5)));
        assert!(!same(json!("5"), json!(5.0)));
        assert!(!same(json!("True"), json!(true)));
        assert!(!same(json!(null), json!(0)));
        assert!(!same(json!(null), json!("")));
        assert!(!same(json!(5), json!(5.5)));
        assert!(!same(json!(2), json!(true)));
        assert!(!same(json!(-1), json!(1)));
        // 2^53+1 の整数は、2^53 の浮動小数と別（Python: 9007199254740993 != 9007199254740992.0）
        assert!(!same(
            json!(9_007_199_254_740_993_i64),
            json!(9_007_199_254_740_992.0)
        ));
        assert!(same(
            json!(9_007_199_254_740_992_i64),
            json!(9_007_199_254_740_992.0)
        ));
        // 整数にならない大きな浮動小数も自分とは等しい
        assert!(same(json!(1e300), json!(1e300)));
        assert!(!same(json!(1e300), json!(1e299)));
        assert!(dict_key(&json!([1])).is_none());
        assert!(dict_key(&json!({"a": 1})).is_none());
    }

    #[test]
    fn prefix_counts_code_points() {
        assert_eq!(prefix_chars("あいうえお", 3), "あいう");
        assert_eq!(prefix_chars("ab", 5), "ab");
        assert_eq!(prefix_chars("😀😀😀", 2), "😀😀");
    }
}
