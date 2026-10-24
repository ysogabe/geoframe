//! Host ヘッダの検査（DNS リバインディング対策）。現行 `handlers.py` の `_host_ok`・
//! `_split_host_port` の移植で、純関数だけ。判定は全ルート（`/healthz` を含む）の前に行う。
//!
//! GEOFRAME には利用者認証が無く、LAN 内で使う前提なので、攻撃者のドメインを利用者のブラウザで
//! 開かせ、そのドメインを LAN 内の自機の IP へ引き直させる（リバインディング）と、同一オリジン
//! 扱いで `/api/v1/photos` や `/derivatives/` が読めてしまう。「Host が自分の名前であること」
//! を要求して防ぐ。
//!
//! - ホスト部が **IP リテラル**なら無条件に許可（IP を直接打つなら DNS を引かない＝経路にならない。
//!   自機の IP の列挙に頼らないので、NIC の追加や DHCP の変化でも 403 にならない）。
//! - **ホスト名**は `localhost` と `GEOFRAME_ALLOWED_HOSTS` の明示許可だけ（小文字、末尾の `.` を除く）。
//! - **ポート**は待ち受けポートの文字列と一致するか、省略（`localhost:` のような空も省略扱い）。
//!   文字列比較なので `localhost:08080` は拒否（Python と同じ）。
//! - Host ヘッダが無い・空は許可（HTTP/1.0 の素の要求や curl・監視を壊さない。ブラウザは必ず送る）。

use std::collections::BTreeSet;
use std::net::IpAddr;

use crate::csrf::py_strip;

/// Host ヘッダを (ホスト部, ポート部) に分ける。ポート未指定なら空文字。
pub fn split_host_port(raw: &str) -> (&str, &str) {
    let h = py_strip(raw);
    if let Some(rest) = h.strip_prefix('[') {
        // IPv6 リテラル: [::1] / [::1]:8080。`]` が無ければ全体をホスト部とみなす（Python と同じ）
        return match rest.split_once(']') {
            Some((host, after)) => (host, after.strip_prefix(':').unwrap_or("")),
            None => (rest, ""),
        };
    }
    if h.matches(':').count() == 1 {
        // name:port / 192.0.2.7:8080
        let (host, port) = h.split_once(':').unwrap();
        return (host, port);
    }
    (h, "") // ポート無し（ブラケット無しの IPv6 もここ）
}

/// Host ヘッダ（無ければ `None`）が許可されるか。`port` は待ち受けポート。
pub fn host_allowed(raw: Option<&str>, port: u16, names: &BTreeSet<String>) -> bool {
    let Some(raw) = raw.filter(|r| !py_strip(r).is_empty()) else {
        return true;
    };
    let (host, host_port) = split_host_port(raw);
    if !host_port.is_empty() && host_port != port.to_string() {
        return false;
    }
    // ゾーン ID（fe80::1%en0）を除いてから IP として読む
    if host
        .split('%')
        .next()
        .unwrap_or("")
        .parse::<IpAddr>()
        .is_ok()
    {
        return true;
    }
    names.contains(host.to_lowercase().trim_end_matches('.'))
}

/// 起動時に出す Host 検査の説明（1行）。許可する**名前そのものは出さず**件数だけ
/// （docs/rust-migration/server.md §3.2。名前は運用者が環境ファイルで確かめられる）。
pub fn startup_note(port: u16, names: &BTreeSet<String>) -> String {
    format!(
        "Host 検査: 名前は localhost を含む {} 件のみ許可、IP リテラルは全て許可、ポートは {port} か省略のみ \
         （名前を足すには GEOFRAME_ALLOWED_HOSTS=name:port,... ）",
        names.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn allowed(raw: Option<&str>) -> bool {
        host_allowed(raw, 8080, &names(&["localhost", "frame.lan"]))
    }

    #[test]
    fn splits_like_the_python_helper() {
        for (raw, expected) in [
            ("localhost", ("localhost", "")),
            ("localhost:8080", ("localhost", "8080")),
            ("localhost:", ("localhost", "")),
            (":8080", ("", "8080")),
            ("192.0.2.7:8080", ("192.0.2.7", "8080")),
            ("[::1]", ("::1", "")),
            ("[::1]:8080", ("::1", "8080")),
            ("[::1]x", ("::1", "")),
            ("[::1", ("::1", "")),
            ("::1", ("::1", "")),
            ("a:b:c", ("a:b:c", "")),
            ("  frame.lan:80  ", ("frame.lan", "80")),
        ] {
            assert_eq!(split_host_port(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn missing_or_empty_host_is_allowed() {
        assert!(allowed(None));
        assert!(allowed(Some("")));
        assert!(allowed(Some("   ")));
    }

    #[test]
    fn names_are_matched_case_insensitively_without_trailing_dots() {
        for raw in [
            "localhost",
            "LOCALHOST.",
            "localhost..",
            "localhost:8080",
            "frame.lan",
            "Frame.LAN:8080",
            "frame.lan.",
        ] {
            assert!(allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn other_names_are_refused() {
        for raw in [
            "evil.example",
            "evil.example:8080",
            "localhost.evil.example",
            "evil.localhost",
            "frame.lan.evil.example",
            "xframe.lan",
            ":8080", // 空のホスト名 + ポート
            "a:b:c",
            "1.2.3",
            "999.1.1.1",
        ] {
            assert!(!allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn port_must_match_exactly_or_be_omitted() {
        assert!(allowed(Some("localhost:8080")));
        assert!(allowed(Some("localhost:")));
        for raw in [
            "localhost:80",
            "localhost:8081",
            "localhost:08080",
            "localhost:+8080",
            "localhost:8080x",
            "192.0.2.7:9",
            "[::1]:9",
        ] {
            assert!(!allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn ip_literals_are_always_allowed_on_the_right_port() {
        for raw in [
            "192.0.2.7",
            "192.0.2.7:8080",
            "127.0.0.1",
            "0.0.0.0",
            "[::1]",
            "[::1]:8080",
            "::1",
            "[2001:db8::7]:8080",
            "::ffff:192.0.2.7",
            "fe80::1%en0",
            "[fe80::1%25en0]:8080",
        ] {
            assert!(allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn malformed_ip_literals_are_treated_as_names_and_refused() {
        // 先頭ゼロ（Python の ipaddress も拒否）、桁あふれ、足りない桁
        for raw in [
            "010.0.0.1",
            "192.0.2.07",
            "256.0.0.1",
            "192.0.2",
            "1.2.3.4.5",
        ] {
            assert!(!allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn dns_rebinding_shapes_are_refused() {
        for raw in [
            "attacker.example",
            "attacker.example:8080",
            "192.0.2.7.attacker.example",
            "localhost@attacker.example",
            "localhost%2eattacker.example",
        ] {
            assert!(!allowed(Some(raw)), "{raw}");
        }
    }

    #[test]
    fn startup_note_shows_the_count_but_not_the_names() {
        let note = startup_note(8080, &names(&["localhost", "private-name.example"]));
        assert!(note.contains("2 件"), "{note}");
        assert!(note.contains("8080"), "{note}");
        assert!(!note.contains("private-name"), "{note}");
    }
}
