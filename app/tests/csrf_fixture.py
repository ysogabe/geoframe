"""POST の CSRF 対策の判定（Content-Type・Origin・Host）の差分試験用フィクスチャ。

現行の `handlers.Handler._csrf_ok`・`_host_ok` を、**実際のヘッダ解析**（`http.client.parse_headers`。生バイトを
Latin-1 の str にして `email` のパーサに通す）の上で動かし、判定を期待値として残す。Rust
（crates/geoframe/tests/csrf_python.rs）は同じ値で判定し、**Python が断るものを Rust が通さない**ことを
確かめる（Rust だけが断るものは HTTP 層が先に断る制御文字入りの値に限る）。

- `content_type`: `[値, 判定]`。Origin・Host は付けない（`Content-Type` の前後の空白・Latin-1 の 0x80〜0xFF・
  大文字小文字・`;` 以降）。
- `origin`: `[Origin, Host, 判定]`。`Content-Type: application/json` を付ける。Origin の scheme・userinfo・ポート・
  path/query/fragment・角括弧（IPv6・IPv4・ゾーン ID・IPvFuture・閉じ忘れ）・空白と Latin-1 の全バイト、
  IPv6 の書式を 1 文字ずつ崩した文字列。
- `mixed`: `[Content-Type, Origin, Host, 判定]`。`None` はヘッダが無い。順序（Content-Type が先）の確認。
- `host`: `[Host, 判定]`。`_host_ok` の判定（待ち受けポート 8080、許可する名前は `localhost` だけ）。
- `http`: 実際の `ThreadingHTTPServer` に生バイトの要求を送った結果。判定は**状態コード**（応答を返さずに
  接続を閉じたときは 0）と、DB が変わったか。

値は **Latin-1 の文字列**（1 文字＝1 バイト）。JSON には `\\u00xx` で書く。`判定`: `ok` / `content_type`
（Content-Type で 403）/ `origin`（Origin で 403）/ `error`（`_csrf_ok` の中で例外。現行はこの場合、応答を
返さず接続を閉じる）。ヘッダの値に CR・LF は入れない（行が分かれてしまう）。

再生成: `python3 tools/gen_csrf_fixture.py`（差分の検査は `--check`）。ホスト名・ポートは合成の値だけ
（localhost、ループバック、予約済みの example.test、192.0.2.0/24）。

Python のバージョンで `urlparse` の振る舞いが変わった（3.11.4 の角括弧の検査など）ので、`--check` は
この環境と CI の Python で同じ結果になる値だけを入れている。
"""
import http.client
import io
import json
import random
import socket
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, store

from tests.admin_api_fixture import _http, seeded
from tests.update_api_fixture import _diff, _snapshot

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/csrf-python.json"

JSON_CT = "application/json"
PORT = 8080
ALLOWED_NAMES = frozenset({"localhost"})


# ---- 現行の判定 ----------------------------------------------------------------------------

def _headers(content_type, origin, host):
    """生のヘッダ（`名前:値` の直後に空白を置かない）を現行と同じ手順で読む。"""
    raw = b""
    for name, value in (("Content-Type", content_type), ("Origin", origin), ("Host", host)):
        if value is not None:
            raw += name.encode("ascii") + b":" + value.encode("latin-1") + b"\r\n"
    return http.client.parse_headers(io.BytesIO(raw + b"\r\n"))


def _stub(content_type, origin, host):
    handler = object.__new__(handlers.Handler)
    handler.headers = _headers(content_type, origin, host)
    return handler


def csrf_verdict(content_type, origin, host):
    handler = _stub(content_type, origin, host)
    sent = []
    handler._send_json = lambda obj, status=200: sent.append((status, obj["error"]))
    try:
        ok = handler._csrf_ok()
    except Exception:  # noqa: BLE001 現行は捕まえない（接続が閉じる）
        return "error"
    if ok:
        assert not sent
        return "ok"
    assert len(sent) == 1 and sent[0][0] == 403, sent
    return "content_type" if "Content-Type" in sent[0][1] else "origin"


def host_verdict(host):
    handler = _stub(None, None, host)
    denied = []
    handler._deny_host = lambda raw: denied.append(raw) or False
    try:
        with patch.object(handlers, "PORT", PORT), \
                patch.object(handlers, "ALLOWED_HOST_NAMES", ALLOWED_NAMES):
            ok = handler._host_ok()
    except Exception:  # noqa: BLE001
        return "error"
    return "ok" if ok else "denied"


# ---- 値の素材 -------------------------------------------------------------------------------

def _pads():
    """前後に付ける 1 つぶんの文字列。ASCII の空白・制御、Latin-1 の全バイト、UTF-8 の空白類。"""
    pads = [" ", "\t", "  ", " \t", "\t "]
    pads += [chr(b) for b in range(0x20) if b not in (0x0A, 0x0D, 0x09)]
    pads += [chr(0x7F)]
    pads += [chr(b) for b in range(0x80, 0x100)]
    pads += [s.encode("utf-8").decode("latin-1")
             for s in ("\u00a0", "\u3000", "\u2003", "\ufeff", "\u0085", "\u2028", "\u200b")]
    return pads


PADS = _pads()


def content_type_cases():
    out = []
    cores = [
        "application/json", "Application/JSON", "APPLICATION/JSON", "application/Json",
        "application/json; charset=utf-8", "application/json;charset=x", "application/json ;x",
        "application/json;", "application/json ; ", ";application/json", "application/jsonx",
        "application/json+x", "application/ld+json", "application/json,text/plain", "text/plain",
        "text/plain; application/json", "application/ json", "application/json\tx", "application/json\t;x",
        "", " ", ";", "application/json;application/json", "multipart/form-data; boundary=x",
        "application/x-www-form-urlencoded", "text/json", "application/vnd.api+json",
        "applicat\u00aeion/json", "application/js\u00f6n", "application/json\u00a0;x",
        "application/json;\u00a0x", "\u00a0;application/json",
    ]
    for core in cores:
        out.append([core, csrf_verdict(core, None, None)])
    for pad in PADS:
        for value in (pad + "application/json", "application/json" + pad,
                      pad + "application/json" + pad):
            out.append([value, csrf_verdict(value, None, None)])
    # 別の中身にも少数の前後の空白（結果が一致して見えるだけで通ってしまわないか）
    for core in ("text/plain", "application/jsonx", "application/json2"):
        for pad in (" ", "\t", "\u00a0", "\u00c2\u00a0", "\u0085", "\u0080"):
            for value in (pad + core, core + pad):
                out.append([value, csrf_verdict(value, None, None)])
    # `;` の前後
    for pad in PADS[:12] + PADS[40:60]:
        value = "application/json" + pad + ";charset=x"
        out.append([value, csrf_verdict(value, None, None)])
        value = "application/json;" + pad
        out.append([value, csrf_verdict(value, None, None)])
    return out


# 角括弧の中身（IPv6・IPv4・ゾーン ID・IPvFuture・壊れた書式）
BRACKET_HOSTS = [
    "::1", "::", "1::", "::2:3:4:5:6:7:8", "1:2:3:4:5:6:7:8", "1:2:3:4:5:6:7::", "1:2:3:4:5:6:7:8:9",
    "ABCD::ef01", "abcd::EF01", "2001:db8::1", "::ffff:192.0.2.1", "::ffff:192.0.2.01", "::ffff:192.0.2",
    "::ffff:192.0.2.256", "1:2:3:4:5:6:192.0.2.1", "1:2:3:4:5:6:7:192.0.2.1", "::192.0.2.1.5",
    "127.0.0.1", "192.0.2.1", "1.2.3", "::1%", "::1%eth0", "::1%eth0%x", "::1%%", "%eth0", "fe80::1%eth0",
    "fe80::1%25eth0", "fe80::1%en 0", "::1%e/th", "::1/64", "1:2", "1", ":", ":::", ":1::", "1::2:", "1::2::3",
    "12345::", "::g", "::1 ", " ::1", "::1\t", "v1.x", "v1.", "v.x", "vg.x", "V1.x", "vF.x", "v1.x:y", "v1x",
    "v1.\u00e9", "", " ", "x", "localhost", "[::1]", "::1]", "[::1", "::1][", "\u00a0::1", "::1\u00a0",
    "0:0:0:0:0:0:0:0", "0000:0000:0000:0000:0000:0000:0000:0001", "00000::1", "::01", "::1:", ":1:2:3:4:5:6:7",
]

AUTH_PLAIN = [
    "localhost", "LOCALHOST", "Localhost", "localhost:8080", "localhost:0", "localhost:08080", "localhost:",
    "localhost:80", "localhost.", "127.0.0.1", "127.0.0.1:8080", "127.0.0.1:", "192.0.2.1", "192.0.2.1:8080",
    "example.test", "Example.Test", "example.test:8080", "a", "a b", "a:b:c", "1:2", "::1", "user@localhost",
    "user:pw@localhost:8080", "@localhost", "user@", "@", "user@[::1]", "user@[::1]:8080", "u[@h]", "[u@h]",
    "u@[::1]@h", "h@u@localhost", "local\u00e9host", "\u00c3\u00a9.example", "\u00e4\u00be\u008b.example",
    "\u00a0localhost", "localhost\u00a0", "\u00c2\u00a0localhost",
]
AUTH_BRACKET = [f"[{h}]" for h in BRACKET_HOSTS] + [
    f"[{h}]:8080" for h in ("::1", "127.0.0.1", "::1%eth0", "v1.x", "")
] + ["[::1]:", "[::1]:x", "[::1]:80x", "[::1]x", "x[::1]", "[::1]]", "[[::1]]", "]::1[", "[::1]@h", "u@[::1]x",
     "[::1]:8080:9", "[::1]:/", "[::1]\u00a0"]
AUTHORITIES = AUTH_PLAIN + AUTH_BRACKET

FORMS = [
    "http://{a}", "https://{a}", "HTTP://{a}", "Http://{a}", "//{a}", "{a}", "null", "file://{a}", "x-y.z+w://{a}",
    "1http://{a}", "-http://{a}", "http:{a}", "http:/{a}", "http:///{a}", "http://{a}/", "http://{a}/p",
    "http://{a}/p?q#f", "http://{a}?q", "http://{a}#f", "http://{a}/?", "http://{a}?", "http://{a}#",
    "http://{a};p", " http://{a}", "\thttp://{a}", "\x01http://{a}", "http://{a} ", "http://{a}\t",
    "http://{a}\x0b", "http://{a}\u00a0", "\u00a0http://{a}", "ht\ttp://{a}", "http://{a}  /", "http:// {a}",
]


def origin_cases():
    out = []

    def add(origin, host):
        out.append([origin, host, csrf_verdict(JSON_CT, origin, host)])

    for a in AUTHORITIES:
        for form in FORMS:
            add(form.format(a=a), a)
    # 値は同じでも Host の側を変える（大文字小文字・ポート・前後の空白）
    for a in ("localhost", "127.0.0.1", "[::1]", "[::1]:8080", "example.test:8080"):
        for host in (a.upper(), a + ":8080", a + ":80", " " + a, a + " ", "\t" + a + "\t", "\u00a0" + a,
                     a + "\u00a0", "\u00c2\u00a0" + a, "", "other.test", a + ".", "u@" + a):
            add("http://" + a, host)
    # Host が無い・空・空白だけ
    for origin in ("http://", "//", "null", "localhost", "http://localhost", " ", "\t", "\u00a0", "http:// "):
        add(origin, None)
        add(origin, "")
        add(origin, " ")
    # Origin が空（通す）・空白だけ
    for origin in ("", " ", "  ", "\t", "\x0b", "\u00a0"):
        for host in (None, "", "localhost"):
            add(origin, host)
    # 前後・途中の 1 バイト
    for pad in PADS:
        add(pad + "http://localhost", "localhost")
        add("http://localhost" + pad, "localhost")
        add("http://" + pad + "localhost", "localhost")
        add("http://" + pad + "localhost", pad + "localhost")
        add("http://local" + pad + "host", "local" + pad + "host")
        add("ht" + pad + "tp://localhost", "localhost")
        add("http://localhost/" + pad, "localhost")
        add("http://localhost:" + pad + "8080", "localhost:" + pad + "8080")
        add("http://u" + pad + "@localhost", "u" + pad + "@localhost")
        add("http://[::1" + pad + "]", "[::1" + pad + "]")
    # IPv6 の書式を 1 文字ずつ崩す（`urlparse` と `ipaddress` の境目）
    rnd = random.Random(54)
    valid = [h for h in BRACKET_HOSTS if h.startswith(("::", "1:", "A", "a", "2", "f", "0")) or ":" in h]
    alphabet = ":.%/vx0123456789abcdefABCDEFg -@[]"
    for _ in range(500):
        s = list(rnd.choice(valid))
        for _ in range(rnd.choice((1, 1, 2))):
            op = rnd.choice(("ins", "del", "rep"))
            pos = rnd.randrange(len(s) + 1)
            if op == "ins":
                s.insert(pos, rnd.choice(alphabet))
            elif s and op == "del":
                del s[min(pos, len(s) - 1)]
            elif s:
                s[min(pos, len(s) - 1)] = rnd.choice(alphabet)
        inner = "".join(s)
        add(f"http://[{inner}]", f"[{inner}]")
        if rnd.random() < 0.3:
            add(f"http://[{inner}]:8080", f"[{inner}]:8080")
    return out


def mixed_cases():
    out = []
    cts = [None, "", JSON_CT, "text/plain", "Application/JSON; charset=x", "\u00c2\u00a0application/json",
           "\u00a0application/json"]
    origins = [None, "", "http://localhost", "http://evil.test", "http://[127.0.0.1]", "null",
               "http://localhost "]
    hosts = [None, "", "localhost", "[127.0.0.1]", "evil.test"]
    for ct in cts:
        for origin in origins:
            for host in hosts:
                out.append([ct, origin, host, csrf_verdict(ct, origin, host)])
    return out


def host_cases():
    out = []
    seen = set()

    def add(host):
        if host in seen:
            return
        seen.add(host)
        out.append([host, host_verdict(host)])

    for a in AUTHORITIES + ["", " ", None, "localhost:8080", "LOCALHOST.", "localhost..", "[::1]:8080",
                            "[::1]:80", "[::1", "::1%eth0", "::1%", "[::1%]", "[127.0.0.1]", "127.1",
                            "127.0.0.01", "0x7f.1", "1.2.3.4:8080", "1.2.3.4:08080", "1.2.3.4:80x"]:
        add(a)
    for pad in PADS:
        add(pad + "localhost")
        add("localhost" + pad)
        add(pad + "127.0.0.1")
        add("127.0.0.1" + pad)
        add(pad)
        add("[::1" + pad + "]")
        add("local" + pad + "host")
        add("localhost:" + pad + "8080")
        add("localhost" + pad + ":8080")
    return out


# ---- 実際の HTTP -------------------------------------------------------------------------------

SETLOC = "/api/admin/setloc"


def http_case(name, headers, *, k, after=()):
    """`headers`: Host・Content-Type・Origin など、そのまま送る (名前, Latin-1 の値) の並び。`after` は
    Content-Length の行（自動で付ける）の**後ろ**に送るもの。
    本文は id 4 へ**毎回違う** lat・lon を書く JSON（書けば必ず lat・lon が変わる）。"""
    body = '{"id": 4, "lat": 50.%02d, "lon": 60.%02d}' % (k, k)
    return {"name": name, "target": SETLOC, "headers": [list(h) for h in headers],
            "after": [list(h) for h in after], "body": body}


def _std(*extra, host="localhost", ct=JSON_CT):
    headers = [("Host", host)]
    if ct is not None:
        headers.append(("Content-Type", ct))
    return headers + list(extra)


def http_cases():
    specs = [
        # 指摘 1: Content-Type の前の UTF-8 の NBSP（Latin-1 では \xc2 が残る）
        ("ct_utf8_nbsp_prefix", _std(ct="\u00c2\u00a0application/json")),
        ("ct_utf8_nbsp_suffix", _std(ct="application/json\u00c2\u00a0")),
        ("ct_utf8_ideographic_space", _std(ct="\u00e3\u0080\u0080application/json")),
        ("ct_utf8_bom", _std(ct="\u00ef\u00bb\u00bfapplication/json")),
        # Latin-1 の NBSP・NEL 1 バイトは strip() が取り除く（現行は通す）
        ("ct_latin1_nbsp_prefix", _std(ct="\u00a0application/json")),
        ("ct_latin1_nbsp_suffix", _std(ct="application/json\u00a0")),
        ("ct_latin1_nel_prefix", _std(ct="\u0085application/json")),
        ("ct_latin1_high_prefix", _std(ct="\u00aaapplication/json")),
        ("ct_latin1_nbsp_before_semicolon", _std(ct="application/json\u00a0;charset=x")),
        # 制御文字は Python の strip() が取り除くが、HTTP 層（hyper）が先に 400 にする
        ("ct_vt_suffix", _std(ct="application/json\u000b")),
        ("ct_ff_prefix", _std(ct="\u000capplication/json")),
        ("ct_fs_prefix", _std(ct="\u001capplication/json")),
        ("ct_us_suffix", _std(ct="application/json\u001f")),
        ("ct_ascii_padded", _std(ct="  application/json \t")),
        ("ct_duplicate_first_ok", [("Host", "localhost"), ("Content-Type", JSON_CT),
                                   ("Content-Type", "text/plain")]),
        ("ct_duplicate_first_bad", [("Host", "localhost"), ("Content-Type", "text/plain"),
                                    ("Content-Type", JSON_CT)]),
        ("ct_name_lowercase", [("Host", "localhost"), ("content-type", JSON_CT)]),
        # 指摘 2: 角括弧の中が IPv6 でない
        ("origin_bracket_ipv4", _std(("Origin", "http://[127.0.0.1]"), host="[127.0.0.1]")),
        ("origin_bracket_zone_empty", _std(("Origin", "http://[::1%]"), host="[::1%]")),
        ("origin_bracket_zone_empty_port", _std(("Origin", "http://[::1%]:8080"), host="[::1%]:8080")),
        ("origin_bracket_ipv6_ok", _std(("Origin", "http://[::1]"), host="[::1]")),
        ("origin_bracket_ipv6_port_ok", _std(("Origin", "http://[::1]:8080"), host="[::1]:8080")),
        ("origin_bracket_zone_ok", _std(("Origin", "http://[fe80::1%eth0]"), host="[fe80::1%eth0]")),
        ("origin_bracket_ipvfuture_ok", _std(("Origin", "http://[v1.x]"), host="[v1.x]")),
        ("origin_bracket_ipvfuture_bad", _std(("Origin", "http://[v1.]"), host="[v1.]")),
        ("origin_bracket_empty", _std(("Origin", "http://[]"), host="[]")),
        ("origin_bracket_junk_after", _std(("Origin", "http://[::1]x"), host="[::1]x")),
        ("origin_bracket_junk_before", _std(("Origin", "http://x[::1]"), host="x[::1]")),
        ("origin_bracket_unclosed", _std(("Origin", "http://[::1"), host="[::1")),
        ("origin_bracket_two_zones", _std(("Origin", "http://[::1%a%b]"), host="[::1%a%b]")),
        ("origin_bracket_ipv4_mapped_ok", _std(("Origin", "http://[::ffff:192.0.2.1]"),
                                               host="[::ffff:192.0.2.1]")),
        ("origin_bracket_ipv4_mapped_leading_zero", _std(("Origin", "http://[::ffff:192.0.2.01]"),
                                                         host="[::ffff:192.0.2.01]")),
        ("origin_bracket_in_path_only", _std(("Origin", "http://localhost/[x"))),
        ("origin_bracket_userinfo", _std(("Origin", "http://[x]@localhost"), host="[x]@localhost")),
        # Origin の値の前後の空白: 現行は先頭の空白・タブだけ取り除き、末尾は残す（hyper は両端を取り除く）
        ("origin_trailing_space", _std(("Origin", "http://localhost "))),
        ("origin_trailing_spaces", _std(("Origin", "http://localhost   "))),
        ("origin_trailing_tab", _std(("Origin", "http://localhost\t"))),
        ("origin_trailing_tab_space", _std(("Origin", "http://localhost\t "))),
        ("origin_trailing_space_tab", _std(("Origin", "http://localhost \t"))),
        ("origin_trailing_space_after_path", _std(("Origin", "http://localhost/ "))),
        ("origin_trailing_space_after_query", _std(("Origin", "http://localhost?q "))),
        ("origin_leading_space", _std(("Origin", " http://localhost"))),
        ("origin_leading_tab", _std(("Origin", "\thttp://localhost"))),
        ("origin_leading_and_trailing_space", _std(("Origin", "  http://localhost  "))),
        ("origin_only_spaces", _std(("Origin", "   "))),
        ("origin_inner_space", _std(("Origin", "http://local host"), host="local host")),
        ("origin_trailing_space_bracket", _std(("Origin", "http://[::1] "), host="[::1]")),
        ("origin_trailing_space_ip", _std(("Origin", "http://127.0.0.1 "), host="127.0.0.1")),
        ("origin_trailing_space_scheme_only", _std(("Origin", "null "))),
        ("origin_trailing_space_empty_netloc", _std(("Origin", "http:// "), host="")),
        ("origin_trailing_space_no_host", [("Content-Type", JSON_CT), ("Origin", "http://localhost ")]),
        ("origin_clean_no_host", [("Content-Type", JSON_CT), ("Origin", "http://")]),
        ("origin_name_uppercase_trailing_space", _std(("ORIGIN", "http://localhost "))),
        ("origin_duplicate_first_ok", _std(("Origin", "http://localhost"), ("Origin", "http://evil.test"))),
        ("origin_duplicate_first_bad", _std(("Origin", "http://evil.test"), ("Origin", "http://localhost"))),
        ("origin_duplicate_second_trailing_space", _std(("Origin", "http://localhost"),
                                                         ("Origin", "http://localhost "))),
        ("origin_duplicate_first_trailing_space", _std(("Origin", "http://localhost "),
                                                        ("Origin", "http://localhost"))),
        ("origin_latin1_in_host_ok", _std(("Origin", "http://\u00e9x"), host="\u00e9x")),
        ("origin_latin1_nbsp_prefix", _std(("Origin", "\u00a0http://localhost"))),
        ("origin_utf8_nbsp_prefix", _std(("Origin", "\u00c2\u00a0http://localhost"))),
        ("origin_latin1_nbsp_suffix", _std(("Origin", "http://localhost\u00a0"))),
        ("origin_vt_suffix", _std(("Origin", "http://localhost\u000b"))),
        ("origin_nul_prefix_is_bad_header", _std(("Origin", "\u0001http://localhost"))),
        ("origin_null", _std(("Origin", "null"))),
        ("origin_file", _std(("Origin", "file://localhost"))),
        ("origin_userinfo", _std(("Origin", "http://user@localhost"))),
        ("origin_host_ipv4_same", _std(("Origin", "http://127.0.0.1"), host="127.0.0.1")),
        # Host の前後
        ("host_latin1_nbsp_prefix_origin_ok", _std(("Origin", "http://localhost"), host="\u00a0localhost")),
        ("host_latin1_nbsp_prefix", _std(host="\u00a0localhost")),
        ("host_latin1_nbsp_suffix", _std(host="localhost\u00a0")),
        ("host_utf8_nbsp_prefix", _std(host="\u00c2\u00a0localhost")),
        ("host_utf8_nbsp_suffix", _std(host="localhost\u00c2\u00a0")),
        ("host_utf8_ideographic_space", _std(host="\u00e3\u0080\u0080localhost")),
        ("host_bracket_ipv4_ok_for_host_check", _std(host="[127.0.0.1]")),
        ("host_bracket_zone_empty_ok_for_host_check", _std(host="[::1%]")),
        ("host_ascii_padded", _std(host="  localhost  ")),
        # 折り返し（obs-fold）。現行は読む。hyper は 400
        ("fold_origin", _std(("Origin", "http://localhost\r\n  ")) ),
        ("fold_content_type", [("Host", "localhost"), ("Content-Type", "application/json;\r\n charset=x")]),
        ("fold_origin_bad", _std(("Origin", "http://evil.test\r\n .localhost"))),
        # ヘッダとして読めない行（名前とコロンの間の空白）。現行（email のパーサ）はそこでヘッダの読み込みを
        # 打ち切り、後ろのヘッダが全て消える。hyper は 400
        ("name_space_before_colon_origin", [("Host", "localhost"), ("Content-Type", JSON_CT),
                                            ("Origin ", "http://evil.test")]),
        ("bad_line_hides_content_length", [("Host", "localhost"), ("Content-Type", JSON_CT),
                                           ("Bad Name", "x")]),
        ("bad_line_hides_content_type", [("Host", "localhost"), ("Bad Name", "x"),
                                         ("Content-Type", JSON_CT)]),
    ]
    cases = [http_case(name, headers, k=k) for k, (name, headers) in enumerate(specs, start=1)]
    k = len(cases)
    after = [
        # Content-Length の後ろの読めない行が、後ろの Origin（他人のオリジン）を消す: 現行は通して書く
        ("bad_line_hides_cross_origin", _std(), [("Bad Name", "x"), ("Origin", "http://evil.test")]),
        ("bad_line_hides_host_mismatch", _std(host="localhost"),
         [("Bad Name", "x"), ("Origin", "http://other.test")]),
    ]
    for name, headers, tail in after:
        k += 1
        cases.append(http_case(name, headers, k=k, after=tail))
    return cases


def _send(port, case):
    """生の要求を送る。ヘッダの値は Latin-1 のバイト（`\\r\\n` を含む値は折り返しとしてそのまま）。"""
    body = case["body"].encode("utf-8")
    head = f"POST {case['target']} HTTP/1.1\r\n".encode("latin-1")
    for name, value in case["headers"]:
        head += name.encode("latin-1") + b": " + value.encode("latin-1") + b"\r\n"
    head += f"Content-Length: {len(body)}\r\n".encode("latin-1")
    for name, value in case["after"]:
        head += name.encode("latin-1") + b": " + value.encode("latin-1") + b"\r\n"
    head += b"Connection: close\r\n\r\n"
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        s.sendall(head + body)
        chunks = []
        while True:
            try:
                data = s.recv(65536)
            except (ConnectionResetError, socket.timeout):
                break
            if not data:
                break
            chunks.append(data)
    raw = b"".join(chunks)
    head, _, payload = raw.partition(b"\r\n\r\n")
    if not head:
        return 0, None
    status = int(head.split(b" ", 2)[1])
    try:
        return status, json.loads(payload.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        return status, None


def build_http():
    results = []
    cases = http_cases()
    with seeded(), patch.object(handlers.Handler, "log_message", lambda *a, **k: None), \
            patch("traceback.print_exc", lambda *a, **k: None), \
            patch("builtins.print", lambda *a, **k: None), \
            patch("socketserver.BaseServer.handle_error", lambda *a, **k: None), \
            patch.object(handlers, "PORT", PORT):
        with _http() as port:
            for case in cases:
                before = _snapshot()
                status, parsed = _send(port, case)
                changes = _diff(before, _snapshot())
                results.append({**case, "status": status, "response": parsed,
                                "wrote": bool(changes)})
    return results


def build_cases():
    return {
        "content_type": content_type_cases(),
        "origin": origin_cases(),
        "mixed": mixed_cases(),
        "host": host_cases(),
        "http": build_http(),
    }


def render(cases):
    """1 ケース 1 行。ASCII だけ（Latin-1 の文字は \\u00xx で書く）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(", ", ": "))

    sections = []
    for key in ("content_type", "origin", "mixed", "host", "http"):
        body = ",\n".join("  " + dump(item) for item in cases[key])
        sections.append(f' "{key}": [\n{body}\n ]')
    return "{\n" + ",\n".join(sections) + "\n}\n"
