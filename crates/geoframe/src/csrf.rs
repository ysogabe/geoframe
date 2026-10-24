//! CSRF 対策のうち、ヘッダの**値を読む**部分（現行 `handlers.py` の `_csrf_ok` の移植）。純関数だけ。
//!
//! # 現行と同じ読み方（Latin-1）
//!
//! 現行の `http.server` はヘッダの生バイトを **Latin-1（iso-8859-1）の `str`** として読み
//! （`http.client.parse_headers`）、`strip()`・`lower()`・`urllib.parse.urlparse()` を `str` に掛ける。
//! UTF-8 として読み直す（`String::from_utf8_lossy`）と、`\xC2\xA0`（UTF-8 の NBSP）が 1 文字の `\u00A0` に
//! なって `trim()` に取り除かれ、現行では `\xC2` が残って断られる値を通してしまう。ここでは**1 バイト＝1 文字**
//! （[`latin1`]）に直してから、現行と同じ規則で判定する。方式を Latin-1 にした理由は、正当な値を断らない
//! ため: 非 ASCII のバイトを含む値を一律に断る方式は、UTF-8 の国際化ドメインをそのまま `Origin`・`Host` に
//! 載せる（現行は Latin-1 のまま両者を比べて通す）非ブラウザのクライアントを断ってしまう。
//!
//! 空白は Python の `str.strip()` のもの（[`py_is_space`]）。Latin-1 の範囲では `\t\n\x0b\x0c\r`・
//! `\x1c`〜`\x1f`・空白・`\x85`・`\xa0`。
//!
//! # `urlparse(origin).netloc`
//!
//! 現行は `urlparse(origin).netloc != host.strip()` で比べる。`netloc` の取り出し（[`netloc`]）は
//! この環境の Python 3.13 の `urllib.parse.urlsplit` の移植で、先頭の制御文字・空白を除き、タブ・改行を
//! 取り除き、`scheme:` を読み飛ばし、`//` から次の `/`・`?`・`#` までを取る。角括弧が付いたホストは
//! Python が `ValueError`（[`check_bracketed_netloc`]）にする形を**同じ形で**断る。
//! 現行は `_csrf_ok` の中の例外を捕まえず、`do_POST` から抜けて**応答を返さず接続を閉じる**
//! （実機で確認。状態コードは付かない）。Rust は 403（クロスオリジン）の定型 JSON を返す。拒否の向きの差。
//!
//! 非 ASCII の `netloc` に Python が掛ける NFKC の検査（`/?#@:` に展開される文字を断る）は移植しない。
//! Latin-1 の範囲（U+0080〜U+00FF）に、NFKC で ASCII の `/?#@:` を生む文字が無いため（試験で全文字を確認）。
//!
//! # 末尾の空白（IO 層が補う）
//!
//! Python は `Origin` の値の**末尾の空白**を残し（先頭の空白だけ取り除く）、hyper（httparse）は両端を
//! 取り除く。末尾の空白（タブは `urlparse` が捨てるので、空白文字そのもの）が `netloc` に残ると現行は
//! 断る。hyper は取り除いた後の値しか見せないので、[`crate::limits::LimitedIo`] が生のバイト列を見て
//! 印を付け（[`crate::rawhead`]）、[`verdict`] の呼び出し側が値に空白を 1 つ足して渡す。

/// 判定の結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 通す。
    Allowed,
    /// `Content-Type` が `application/json` でない（403）。
    ContentType,
    /// `Origin` が `Host` と違う、または現行が例外にする形（403）。
    CrossOrigin,
}

/// 生バイトを Latin-1 の文字列にする（1 バイト＝1 文字）。
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Python の `str.isspace()`（Latin-1 の範囲）。Rust の `char::is_whitespace` に、Python だけが空白とする
/// `\x1c`〜`\x1f` を足したもの。
fn py_is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// Python の `str.strip()`。
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// 現行の `_csrf_ok`。`content_type`・`origin`・`host` はヘッダの値の生バイト（無ければ `None`）。
/// `Content-Type` が先、`Origin` が後（現行と同じ順序で、どちらも 403）。
pub fn verdict(content_type: Option<&[u8]>, origin: Option<&[u8]>, host: Option<&[u8]>) -> Verdict {
    let content_type = latin1(content_type.unwrap_or_default());
    let main = content_type.split(';').next().unwrap_or("");
    // Python の `.lower()` は Latin-1 の非 ASCII を ASCII に落とさないので、ASCII だけの比較でよい
    if !py_strip(main).eq_ignore_ascii_case("application/json") {
        return Verdict::ContentType;
    }
    if let Some(origin) = origin.filter(|o| !o.is_empty()) {
        let host = latin1(host.unwrap_or_default());
        return match netloc(&latin1(origin)) {
            Some(netloc) if netloc == py_strip(&host) => Verdict::Allowed,
            _ => Verdict::CrossOrigin,
        };
    }
    Verdict::Allowed
}

/// `scheme_chars`（`urllib.parse`）: 英数字と `+-.`。
fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// Python の `urlparse(url).netloc`。Python が `ValueError` にするときは `None`。
pub fn netloc(url: &str) -> Option<String> {
    // `url.lstrip(_WHATWG_C0_CONTROL_OR_SPACE)`（U+0000〜U+0020）、続けて `\t\r\n` をどこからでも除く
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    // `i = url.find(':')`、`i > 0 and url[0].isascii() and url[0].isalpha()` で、`url[:i]` が全て scheme の文字
    if let Some(i) = rest.find(':')
        && i > 0
        && rest.starts_with(|c: char| c.is_ascii_alphabetic())
        && rest[..i].chars().all(is_scheme_char)
    {
        rest = &rest[i + 1..];
    }
    let Some(after) = rest.strip_prefix("//") else {
        return Some(String::new());
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    let open = netloc.contains('[');
    let close = netloc.contains(']');
    if open != close {
        return None; // "Invalid IPv6 URL"
    }
    if open && !check_bracketed_netloc(netloc) {
        return None;
    }
    Some(netloc.to_owned())
}

/// Python の `_check_bracketed_netloc`（`[` と `]` の両方が `netloc` にあるときだけ呼ばれる）。通れば真。
fn check_bracketed_netloc(netloc: &str) -> bool {
    // `netloc.rpartition('@')[2]`
    let hostname_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, tail)| tail);
    let hostname = match hostname_and_port.split_once('[') {
        Some((before_bracket, bracketed)) => {
            // 角括弧の前に何も置けない
            if !before_bracket.is_empty() {
                return false;
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            // 角括弧の後ろは、空かポートの区切り（`:`）だけ
            if !port.is_empty() && !port.starts_with(':') {
                return false;
            }
            hostname
        }
        None => hostname_and_port
            .split_once(':')
            .map_or(hostname_and_port, |(host, _)| host),
    };
    check_bracketed_host(hostname)
}

/// Python の `_check_bracketed_host`: `v` で始まれば IPvFuture（`v` 16進数 `.` 1文字以上）、そうでなければ
/// IPv6（IPv4 は角括弧に入れられない）。
fn check_bracketed_host(hostname: &str) -> bool {
    if let Some(rest) = hostname.strip_prefix('v') {
        // re.match(r"\Av[a-fA-F0-9]+\..+\Z", hostname)
        let digits = rest.chars().take_while(char::is_ascii_hexdigit).count();
        return digits >= 1
            && rest[digits..]
                .strip_prefix('.')
                .is_some_and(|t| !t.is_empty());
    }
    valid_ipv6(hostname)
}

/// Python の `ipaddress.IPv6Address(s)` が通る形か（`ip_address()` は IPv4 を先に試すが、IPv4 として
/// 通る文字列は IPv6 としては通らず、どちらも `ValueError` になる）。
fn valid_ipv6(s: &str) -> bool {
    if s.contains('/') {
        return false;
    }
    // `_split_scope_id`: ゾーン ID（`%` の後ろ）は空でも `%` を含んでもいけない
    let addr = match s.split_once('%') {
        None => s,
        Some((_, scope)) if scope.is_empty() || scope.contains('%') => return false,
        Some((addr, _)) => addr,
    };
    ipv6_int_from_string(addr)
}

/// Python の `IPv6Address._ip_int_from_string`（値は要らないので、通るかだけ）。
fn ipv6_int_from_string(ip: &str) -> bool {
    const HEXTET_COUNT: isize = 8;
    if ip.is_empty() || ip.chars().count() > 45 {
        return false;
    }
    let max_parts = HEXTET_COUNT as usize + 1;
    let mut parts: Vec<&str> = ip.splitn(max_parts + 1, ':').collect();
    if parts.len() < 3 {
        return false;
    }
    // 末尾が IPv4 形式なら 16 進 2 つに直す（通るかだけ見るので、代わりに有効な 2 つを足す）
    if parts.last().is_some_and(|p| p.contains('.')) {
        let v4 = parts.pop().unwrap_or_default();
        if !valid_ipv4(v4) {
            return false;
        }
        parts.extend(["0", "0"]);
    }
    if parts.len() > max_parts {
        return false;
    }
    let last = parts.len() - 1;
    let mut skip_index = None;
    for (i, part) in parts.iter().enumerate().take(last).skip(1) {
        if part.is_empty() {
            if skip_index.is_some() {
                return false; // `::` は 1 つまで
            }
            skip_index = Some(i);
        }
    }
    let (parts_hi, parts_lo);
    if let Some(skip) = skip_index {
        let mut hi = skip;
        let mut lo = parts.len() - skip - 1;
        if parts[0].is_empty() {
            hi -= 1;
            if hi != 0 {
                return false; // 先頭の `:` は `::` の一部だけ
            }
        }
        if parts[last].is_empty() {
            lo -= 1;
            if lo != 0 {
                return false; // 末尾の `:` は `::` の一部だけ
            }
        }
        if HEXTET_COUNT - ((hi + lo) as isize) < 1 {
            return false;
        }
        (parts_hi, parts_lo) = (hi, lo);
    } else {
        if parts.len() != HEXTET_COUNT as usize || parts[0].is_empty() || parts[last].is_empty() {
            return false;
        }
        (parts_hi, parts_lo) = (parts.len(), 0);
    }
    parts[..parts_hi]
        .iter()
        .chain(&parts[parts.len() - parts_lo..])
        .all(|h| valid_hextet(h))
}

/// `_parse_hextet`: 16 進の数字だけで 1〜4 文字。
fn valid_hextet(h: &str) -> bool {
    (1..=4).contains(&h.len()) && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// `IPv4Address(s)`: 4 つの 10 進数（各 1〜3 桁、先頭の `0` は `0` 自身だけ、255 以下）。
fn valid_ipv4(s: &str) -> bool {
    let octets: Vec<&str> = s.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|o| {
            (1..=3).contains(&o.len())
                && o.chars().all(|c| c.is_ascii_digit())
                && (*o == "0" || !o.starts_with('0'))
                && o.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(ct: Option<&[u8]>, origin: Option<&[u8]>, host: Option<&[u8]>) -> Verdict {
        verdict(ct, origin, host)
    }

    const JSON: Option<&[u8]> = Some(b"application/json");

    #[test]
    fn content_type_is_read_as_latin1_so_a_utf8_nbsp_prefix_is_refused() {
        // 現行: `\xC2` が残るので 403（UTF-8 として読むと NBSP が取り除かれて通ってしまう）
        assert_eq!(
            check(Some(b"\xC2\xA0application/json"), None, None),
            Verdict::ContentType
        );
        // Latin-1 の NBSP 1 バイトは Python の strip() が取り除く
        assert_eq!(
            check(Some(b"\xA0application/json\xA0"), None, None),
            Verdict::Allowed
        );
        // 全角空白（UTF-8 の 3 バイト）は取り除かれない
        assert_eq!(
            check(Some(b"\xE3\x80\x80application/json"), None, None),
            Verdict::ContentType
        );
        // \x1c〜\x1f は Python だけが空白とする
        assert_eq!(
            check(Some(b"\x1fapplication/json\x1c"), None, None),
            Verdict::Allowed
        );
    }

    #[test]
    fn bracketed_hosts_are_refused_when_python_raises() {
        let origin = |o: &[u8], h: &[u8]| check(JSON, Some(o), Some(h));
        // 指摘: 角括弧の中が IPv4 / ゾーン ID だけ（Python は ValueError）
        assert_eq!(
            origin(b"http://[127.0.0.1]", b"[127.0.0.1]"),
            Verdict::CrossOrigin
        );
        assert_eq!(origin(b"http://[::1%]", b"[::1%]"), Verdict::CrossOrigin);
        // 通る形
        assert_eq!(origin(b"http://[::1]", b"[::1]"), Verdict::Allowed);
        assert_eq!(
            origin(b"http://[::1]:8080", b"[::1]:8080"),
            Verdict::Allowed
        );
        assert_eq!(
            origin(b"http://[fe80::1%eth0]", b"[fe80::1%eth0]"),
            Verdict::Allowed
        );
        assert_eq!(origin(b"http://[v1.x]", b"[v1.x]"), Verdict::Allowed);
        assert_eq!(
            origin(b"http://[::ffff:1.2.3.4]", b"[::ffff:1.2.3.4]"),
            Verdict::Allowed
        );
        // IPvFuture の書式違反・IPv6 でない・閉じ忘れ・前後の余り
        for bad in [
            &b"http://[v.x]"[..],
            b"http://[v1.]",
            b"http://[vg.x]",
            b"http://[V1.x]",
            b"http://[]",
            b"http://[",
            b"http://]",
            b"http://[::1",
            b"http://[::1]x",
            b"http://x[::1]",
            b"http://[[::1]]",
            b"http://[::ffff:01.2.3.4]",
            b"http://[1::2::3]",
            b"http://[::1%a%b]",
            b"http://[::1/64]",
        ] {
            // Host を Origin の authority に揃えても、Python が例外にするので通らない
            let host = &bad[b"http://".len()..];
            assert_eq!(origin(bad, host), Verdict::CrossOrigin, "{}", latin1(bad));
        }
    }

    #[test]
    fn netloc_follows_urlparse() {
        let n = |u: &str| netloc(u);
        assert_eq!(n("http://localhost/a?b#c").as_deref(), Some("localhost"));
        assert_eq!(n("//localhost").as_deref(), Some("localhost"));
        assert_eq!(n("localhost").as_deref(), Some(""));
        assert_eq!(n("null").as_deref(), Some(""));
        assert_eq!(n("file:///x").as_deref(), Some(""));
        assert_eq!(n("  \t http://loc\nalhost").as_deref(), Some("localhost"));
        assert_eq!(n("http://u:p@h:80").as_deref(), Some("u:p@h:80"));
        assert_eq!(n("1http://a").as_deref(), Some(""));
        assert_eq!(n("ht+tp-x.y://a").as_deref(), Some("a"));
        // 末尾の空白は残る
        assert_eq!(n("http://localhost ").as_deref(), Some("localhost "));
    }

    #[test]
    fn ipv6_follows_python_ipaddress() {
        for ok in [
            "::",
            "::1",
            "1::",
            "1:2:3:4:5:6:7:8",
            "1:2:3:4:5:6:7::",
            "::2:3:4:5:6:7:8",
            "::ffff:1.2.3.4",
            "1:2:3:4:5:6:1.2.3.4",
            "fe80::1%eth0",
            "ABCD::ef01",
        ] {
            assert!(valid_ipv6(ok), "{ok}");
        }
        for bad in [
            "",
            ":",
            ":::",
            "1",
            "1:2",
            "1::2::3",
            ":1::",
            "1::2:",
            "1:2:3:4:5:6:7:8:9",
            "1:2:3:4:5:6:7:8::",
            "::1.2.3",
            "::1.2.3.256",
            "::1.2.3.04",
            "::12345",
            "::g",
            "::1%",
            "::1%%",
            "::1%a%b",
            "::1/64",
            "1:2:3:4:5:6:7::8:9",
            "127.0.0.1",
            "::ffff:1.2.3.4.5",
            "0:0:0:0:0:0:0:0:0",
        ] {
            assert!(!valid_ipv6(bad), "{bad}");
        }
    }
}
