"""更新API（`POST /api/admin/setloc`・`POST /api/admin/setflag`）と POST の入口（CSRF対策・ルーティング・
エラー応答）の契約テスト用フィクスチャ。

管理画面APIのフィクスチャ（admin_api_fixture）と同じ合成DBを本物のスキーマで作り、現行の
`handlers.Handler` を実際にHTTPで動かして、下の `CASES` を**順に**POSTする。各ケースについて、状態コード・
応答のJSON（JSONでなければ null）・直後の `photos` の変化（変わった行の変わった列だけ）を記録する。
Rust の結合テスト（crates/geoframe/tests/update_api.rs）は、同じDB（admin-api-catalog.sql）を作り直して
同じ要求を同じ順に流し、状態・応答・DBの変化が一致することを確かめる。

再生成: `python3 tools/gen_update_fixture.py`（差分の検査は `--check`）。
座標・地名・ファイル名は実在のものを使わない（座標は 35.1/139.1 のような丸い値）。

意図した差（NaN の文面、i64 を超える id、バックスラッシュ入りのパスなど）も記録する。Rust のテストは
その一覧（`KNOWN_DIFFERENCES`）で期待値を分け、差が増減したら落ちる（server.md §0・§17）。
"""
import json
import socket
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, store

from tests.admin_api_fixture import _http, seeded

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/update-api-python.json"

SETLOC = "/api/admin/setloc"
SETFLAG = "/api/admin/setflag"
JSON_CT = ("Content-Type", "application/json")


def case(name, target, body, *, headers=None, ct=True, host="localhost", length="auto",
         body_hex=None, status_only=False):
    """1件の要求。`headers` は Host・Content-Type・Content-Length のほかに足すもの。
    `length`: "auto"（本文の長さ）、None（ヘッダを付けない）、文字列（そのまま。本文は送る）、
    "declare:N"（N と宣言して本文は送らない）。`ct=False` で Content-Type を付けない。"""
    return {"name": name, "target": target, "body": body, "body_hex": body_hex,
            "headers": headers or [], "ct": ct, "host": host, "length": length,
            "status_only": status_only}


def j(obj):
    return json.dumps(obj, ensure_ascii=False)


def loc(name, payload, **kw):
    return case(name, SETLOC, j(payload), **kw)


def flag(name, payload, **kw):
    return case(name, SETFLAG, j(payload), **kw)


def exact_limit_body(size):
    """ちょうど `size` バイトの正しい JSON（上限 65536 の境目）。"""
    head, tail = '{"id": 29, "favorite": true, "pad": "', '"}'
    return head + "x" * (size - len(head) - len(tail)) + tail


CASES = [
    # ---- E12 setloc: 正常 ----
    loc("setloc_ok", {"id": 4, "lat": 35.5, "lon": 139.5}),
    loc("setloc_int_coords", {"id": 5, "lat": 35, "lon": 139}),
    loc("setloc_on_inferred_clears_inference", {"id": 6, "lat": 10.25, "lon": 20.75}),
    loc("setloc_on_row_with_place", {"id": 1, "lat": -33.5, "lon": -70.25}),
    loc("setloc_on_noloc", {"id": 16, "lat": 1.5, "lon": 2.5}),
    # 範囲の端（閉区間）
    loc("setloc_edge_max", {"id": 17, "lat": 90, "lon": 180}),
    loc("setloc_edge_min", {"id": 18, "lat": -90.0, "lon": -180.0}),
    loc("setloc_zero", {"id": 19, "lat": 0, "lon": 0}),
    loc("setloc_negative_zero", {"id": 20, "lat": -0.0, "lon": -0.0}),
    loc("setloc_extra_keys_ignored", {"id": 21, "lat": 3.5, "lon": 4.5, "note": "x", "loc_source": "exif"}),
    loc("setloc_twice_same_row", {"id": 21, "lat": 5.5, "lon": 6.5}),
    # ---- E12: 範囲外・型 ----
    loc("setloc_lat_over", {"id": 4, "lat": 90.000001, "lon": 0}),
    loc("setloc_lat_under", {"id": 4, "lat": -90.000001, "lon": 0}),
    loc("setloc_lon_over", {"id": 4, "lat": 0, "lon": 180.000001}),
    loc("setloc_lon_under", {"id": 4, "lat": 0, "lon": -180.000001}),
    loc("setloc_lat_big_int", {"id": 4, "lat": 91, "lon": 0}),
    loc("setloc_lat_bool", {"id": 4, "lat": True, "lon": 0}),
    loc("setloc_lon_bool", {"id": 4, "lat": 0, "lon": False}),
    loc("setloc_lat_string", {"id": 4, "lat": "35.0", "lon": 0}),
    loc("setloc_lon_string", {"id": 4, "lat": 0, "lon": "139"}),
    loc("setloc_lat_null", {"id": 4, "lat": None, "lon": 0}),
    loc("setloc_lat_array", {"id": 4, "lat": [1], "lon": 0}),
    loc("setloc_lat_object", {"id": 4, "lat": {"a": 1}, "lon": 0}),
    loc("setloc_lat_missing", {"id": 4, "lon": 0}),
    loc("setloc_lon_missing", {"id": 4, "lat": 0}),
    loc("setloc_both_bad_reports_lat", {"id": 4, "lat": "x", "lon": "y"}),
    # ---- E12: id ----
    loc("setloc_id_zero", {"id": 0, "lat": 0, "lon": 0}),
    loc("setloc_id_negative", {"id": -1, "lat": 0, "lon": 0}),
    loc("setloc_id_string", {"id": "4", "lat": 0, "lon": 0}),
    loc("setloc_id_bool", {"id": True, "lat": 0, "lon": 0}),
    loc("setloc_id_float", {"id": 4.0, "lat": 0, "lon": 0}),
    loc("setloc_id_fraction", {"id": 4.5, "lat": 0, "lon": 0}),
    loc("setloc_id_null", {"id": None, "lat": 0, "lon": 0}),
    loc("setloc_id_missing", {"lat": 0, "lon": 0}),
    loc("setloc_id_bad_before_lat", {"id": 0, "lat": "x", "lon": "y"}),
    loc("setloc_not_found", {"id": 999, "lat": 0, "lon": 0}),
    loc("setloc_not_found_large", {"id": 9223372036854775807, "lat": 0, "lon": 0}),
    case("setloc_id_beyond_i64", SETLOC, '{"id": 9223372036854775808, "lat": 0, "lon": 0}'),
    case("setloc_id_beyond_i64_bad_lat_first", SETLOC,
         '{"id": 9223372036854775808, "lat": 99, "lon": 0}'),
    case("setloc_id_exponent_float", SETLOC, '{"id": 1e2, "lat": 0, "lon": 0}'),
    case("setloc_nan_lat", SETLOC, '{"id": 4, "lat": NaN, "lon": 0}'),
    case("setloc_infinity_lon", SETLOC, '{"id": 4, "lat": 0, "lon": Infinity}'),
    case("setloc_duplicate_key_last_wins", SETLOC, '{"id": 22, "lat": 1, "lat": 7.5, "lon": 8.5}'),
    # ---- E13 setflag ----
    flag("setflag_favorite_on", {"id": 2, "favorite": True}),
    flag("setflag_hidden_on", {"id": 2, "hidden": True}),
    flag("setflag_favorite_off_keeps_hidden", {"id": 2, "favorite": False}),
    flag("setflag_both", {"id": 3, "favorite": True, "hidden": False}),
    flag("setflag_both_off", {"id": 1, "favorite": False, "hidden": False}),
    flag("setflag_on_manual_row", {"id": 14, "hidden": True}),
    flag("setflag_extra_keys_ignored", {"id": 23, "favorite": True, "lat": 1}),
    flag("setflag_neither", {"id": 2}),
    flag("setflag_other_keys_only", {"id": 2, "other": True}),
    flag("setflag_favorite_null", {"id": 2, "favorite": None}),
    flag("setflag_favorite_int", {"id": 2, "favorite": 1}),
    flag("setflag_favorite_zero", {"id": 2, "favorite": 0}),
    flag("setflag_favorite_string", {"id": 2, "favorite": "true"}),
    flag("setflag_hidden_array", {"id": 2, "hidden": [True]}),
    flag("setflag_bad_hidden_blocks_valid_favorite", {"id": 2, "favorite": False, "hidden": 1}),
    flag("setflag_bad_favorite_reported_first", {"id": 2, "favorite": "x", "hidden": "y"}),
    flag("setflag_id_zero", {"id": 0, "favorite": True}),
    flag("setflag_id_negative", {"id": -5, "favorite": True}),
    flag("setflag_id_string", {"id": "2", "favorite": True}),
    flag("setflag_id_bool", {"id": True, "favorite": "x"}),
    flag("setflag_id_float", {"id": 2.0, "favorite": True}),
    flag("setflag_id_missing", {"favorite": True}),
    flag("setflag_not_found", {"id": 999, "favorite": True}),
    case("setflag_id_beyond_i64", SETFLAG, '{"id": 9223372036854775808, "favorite": true}'),
    # ---- ルーティング（Host の次、CSRF より先） ----
    case("route_unknown_admin", "/api/admin/nope", "{}"),
    case("route_unknown_blog", "/api/blog/nope", "{}"),
    case("route_unknown_root", "/", "{}"),
    case("route_healthz_post", "/healthz", "{}"),
    case("route_get_only_status", "/api/admin/status", "{}"),
    case("route_unknown_with_bad_content_type", "/api/admin/nope", "{}", ct=False),
    case("route_unknown_without_length", "/api/admin/nope", None, length=None),
    case("route_trailing_slash", SETFLAG + "/", j({"id": 24, "favorite": True})),
    case("route_double_slash", "/" + SETFLAG, j({"id": 25, "favorite": True})),
    case("route_percent_encoded", "/api/admin/%73etflag", j({"id": 26, "favorite": True})),
    case("route_dot_segments", "/api/admin/./../admin/setflag", j({"id": 27, "favorite": True})),
    case("route_with_query", SETFLAG + "?x=1&y=2", j({"id": 28, "favorite": True})),
    case("route_case_sensitive", "/api/admin/SetFlag", j({"id": 2, "favorite": True})),
    case("route_prefix_only", "/api/admin/setflagx", j({"id": 2, "favorite": True})),
    case("route_backslash", "/api\\admin\\setflag", j({"id": 21, "hidden": True})),
    # 回転（T3.2。処理は rotate_fixture）と旅行ブログ（T3.6、まだ Rust に無い）のルート。検査は同じ順序を通る
    case("route_rotate_bad_content_type", "/api/admin/rotate", "{}", ct=False),
    case("route_blog_save_cross_origin", "/api/blog/save", "{}",
         headers=[("Origin", "http://attacker.example")]),
    case("route_blog_export_not_object", "/api/blog/export", "[]"),
    # ---- CSRF (1) Content-Type ----
    flag("ct_missing", {"id": 2, "favorite": True}, ct=False),
    flag("ct_text_plain", {"id": 2, "favorite": True}, ct=False,
         headers=[("Content-Type", "text/plain")]),
    flag("ct_form", {"id": 2, "favorite": True}, ct=False,
         headers=[("Content-Type", "application/x-www-form-urlencoded")]),
    flag("ct_json_prefix_only", {"id": 2, "favorite": True}, ct=False,
         headers=[("Content-Type", "application/jsonx")]),
    flag("ct_json_ld", {"id": 2, "favorite": True}, ct=False,
         headers=[("Content-Type", "application/ld+json")]),
    flag("ct_empty", {"id": 2, "favorite": True}, ct=False, headers=[("Content-Type", "")]),
    flag("ct_json_charset_ok", {"id": 30, "favorite": True}, ct=False,
         headers=[("Content-Type", "application/json; charset=utf-8")]),
    flag("ct_json_uppercase_ok", {"id": 7, "favorite": True}, ct=False,
         headers=[("Content-Type", "Application/JSON")]),
    flag("ct_json_padded_ok", {"id": 8, "favorite": True}, ct=False,
         headers=[("Content-Type", "  application/json  ;charset=x")]),
    # ---- CSRF (2) Origin ----
    flag("origin_same_ok", {"id": 9, "favorite": True}, headers=[("Origin", "http://localhost")]),
    flag("origin_same_https_ok", {"id": 10, "favorite": True},
         headers=[("Origin", "https://localhost")]),
    flag("origin_scheme_uppercase_ok", {"id": 11, "favorite": True},
         headers=[("Origin", "HTTP://localhost")]),
    flag("origin_with_path_ok", {"id": 12, "favorite": True},
         headers=[("Origin", "http://localhost/some/path?q#f")]),
    flag("origin_other_host", {"id": 2, "favorite": True},
         headers=[("Origin", "http://attacker.example")]),
    flag("origin_port_mismatch", {"id": 2, "favorite": True},
         headers=[("Origin", "http://localhost:8080")]),
    flag("origin_host_uppercase", {"id": 2, "favorite": True},
         headers=[("Origin", "http://LOCALHOST")]),
    flag("origin_userinfo", {"id": 2, "favorite": True},
         headers=[("Origin", "http://user@localhost")]),
    flag("origin_null", {"id": 2, "favorite": True}, headers=[("Origin", "null")]),
    flag("origin_no_scheme", {"id": 2, "favorite": True}, headers=[("Origin", "localhost")]),
    flag("origin_empty_passes", {"id": 13, "favorite": True}, headers=[("Origin", "")]),
    flag("origin_lookalike_suffix", {"id": 2, "favorite": True},
         headers=[("Origin", "http://localhost.attacker.example")]),
    flag("origin_scheme_relative_ok", {"id": 15, "favorite": True},
         headers=[("Origin", "//localhost")]),
    flag("origin_other_host_header", {"id": 2, "favorite": True}, host="127.0.0.1",
         headers=[("Origin", "http://localhost")]),
    flag("origin_same_ip_host_ok", {"id": 22, "favorite": True}, host="127.0.0.1",
         headers=[("Origin", "http://127.0.0.1")]),
    # 順序: Content-Type が先
    flag("order_bad_content_type_before_bad_origin", {"id": 2, "favorite": True}, ct=False,
         headers=[("Content-Type", "text/plain"), ("Origin", "http://attacker.example")]),
    flag("order_bad_origin_before_bad_length", {"id": 2, "favorite": True}, length=None,
         headers=[("Origin", "http://attacker.example")]),
    flag("order_bad_origin_before_big_body", {"id": 2, "favorite": True},
         length="declare:70000", headers=[("Origin", "http://attacker.example")]),
    # ---- CSRF (3)(4)(5)(6) 本文 ----
    flag("length_missing", {"id": 2, "favorite": True}, length=None),
    flag("length_declared_too_big", {"id": 2, "favorite": True}, length="declare:65537"),
    flag("length_declared_huge", {"id": 2, "favorite": True}, length="declare:99999999999"),
    flag("length_big_before_json_check", {"id": 2, "favorite": True}, length="declare:70000"),
    case("body_exactly_limit_ok", SETFLAG, exact_limit_body(65536)),
    case("body_one_over_limit", SETFLAG, exact_limit_body(65537)),
    case("body_empty", SETFLAG, "", length="0"),
    case("body_not_json", SETFLAG, "{"),
    case("body_plain_text", SETFLAG, "id=2&favorite=true"),
    case("body_array", SETFLAG, "[]"),
    case("body_array_of_objects", SETFLAG, '[{"id": 2, "favorite": true}]'),
    case("body_string", SETFLAG, '"x"'),
    case("body_number", SETFLAG, "1"),
    case("body_null", SETFLAG, "null"),
    case("body_true", SETFLAG, "true"),
    case("body_trailing_garbage", SETFLAG, '{"id": 2, "favorite": true} x'),
    case("body_leading_whitespace_ok", SETFLAG, '  \n {"id": 25, "favorite": true}  \n'),
    case("body_bom", SETFLAG, "",
         body_hex="efbbbf" + '{"id": 2, "favorite": true}'.encode().hex()),
    case("body_invalid_utf8", SETFLAG, "", body_hex="7b22696422ff7d"),
    case("body_utf8_japanese_key_ok", SETFLAG, '{"id": 26, "favorite": true, "メモ": "あ"}'),
    case("body_single_quotes", SETFLAG, "{'id': 2}"),
    case("body_trailing_comma", SETFLAG, '{"id": 2, "favorite": true,}'),
    case("body_deeply_nested_ok", SETFLAG,
         '{"id": 27, "favorite": true, "x": ' + "[" * 50 + "]" * 50 + "}"),
]


def http_loc(name, k, **kw):
    """HTTP 層（Rust では axum/hyper）が Content-Length を先に検査する境界のケース。書き込みが起きたかを
    見分けられるよう、どれも同じ行（id 4）へ**毎回違う** lat・lon を書く（書けば必ず lat・lon が変わる）。
    戻り値の `body_len` は本文のバイト数（`length` の文字列を作るため）。"""
    body = '{"id": 4, "lat": 50.%02d, "lon": 60.%02d}' % (k, k)
    return case(name, SETLOC, body, **kw), len(body.encode("utf-8"))


def _http_cases():
    cases = []
    n = len('{"id": 4, "lat": 50.00, "lon": 60.00}')  # 本文の長さはどのケースも同じ

    def add(name, k, **kw):
        c, length = http_loc(name, k, **kw)
        assert length == n
        cases.append(c)

    # 巨大・不正な Content-Length（本文は送らない）
    add("http_cl_u64_max", 1, length="declare:18446744073709551615")
    add("http_cl_u64_max_plus1", 2, length="declare:18446744073709551616")
    # 数字でない・負・空
    add("http_cl_not_digits", 3, length="abc")
    add("http_cl_not_digits_text_plain", 4, ct=False,
        headers=[("Content-Type", "text/plain")], length="abc")
    add("http_cl_negative", 5, length="-1")
    add("http_cl_empty", 6, length="")
    # Python の int() は + 記号・アンダースコア入りも読む
    add("http_cl_plus_sign", 7, length=f"+{n}")
    add("http_cl_underscore", 8, length=f"{n // 10}_{n % 10}")
    add("http_cl_list_same", 9, length=f"{n}, {n}")
    # 重複した Content-Length（Python は最初の値だけを見る）
    add("http_cl_dup_same", 10, length=None,
        headers=[("Content-Length", str(n)), ("Content-Length", str(n))])
    add("http_cl_dup_first_ok_second_big", 11, length=None,
        headers=[("Content-Length", str(n)), ("Content-Length", "99999")])
    add("http_cl_dup_first_big_second_ok", 12, length=None,
        headers=[("Content-Length", "99999"), ("Content-Length", str(n))])
    add("http_cl_dup_second_over_limit", 13, length=None,
        headers=[("Content-Length", str(n)), ("Content-Length", "70000")])
    # Python の int() は 10 進の文字列を 4300 桁までしか読まない（sys.int_info.default_max_str_digits。
    # 符号・前後の空白・_ は数えず、先頭の 0 は数える）。hyper は先頭の 0 の長い表記を通すので、
    # 境界の前後を固定する（4301 桁以上は Python が 400。Rust が書いてはならない）
    def padded(digits, value=n):
        return "0" * (digits - len(str(value))) + str(value)

    add("http_cl_zeros_4300", 16, length=padded(4300))
    add("http_cl_zeros_4301", 17, length=padded(4301))
    add("http_cl_zeros_4302", 18, length=padded(4302))
    # 桁数の検査は 413（上限超過）の判定より前。4301 桁なら値が上限超過でも 413 でなく 400
    add("http_cl_zeros_4300_over_limit", 19, length="declare:" + padded(4300, 65537))
    add("http_cl_zeros_4301_over_limit", 20, length="declare:" + padded(4301, 65537))
    # 前後の空白・タブ（hyper が取り除く。Python の int() も取り除く）は桁数に数えない
    add("http_cl_spaces_zeros_4300", 21, length=" " + padded(4300) + " ")
    add("http_cl_spaces_zeros_4301", 22, length=" " + padded(4301) + " ")
    add("http_cl_tab_padded", 23, length="\t" + str(n) + "\t")
    # 4301 桁以上は +付きでも Python が断る（hyper は + を先に断る）
    add("http_cl_plus_zeros_4301", 24, length="+" + padded(4301))
    # 重複: Python は最初の値だけを int() に渡す。hyper は重複を数値が同じなら通す
    add("http_cl_dup_first_zeros_4301", 25, length=None,
        headers=[("Content-Length", padded(4301)), ("Content-Length", str(n))])
    add("http_cl_dup_second_zeros_4301", 26, length=None,
        headers=[("Content-Length", str(n)), ("Content-Length", padded(4301))])
    # HTTP 層の大きさの上限。Python（http.server）は 1 行 65536 バイト（改行を含む）・ヘッダ 100 個まで。
    # 超えると 431（要求行は 414）。Rust（hyper）の既定の上限は別なので、境界の前後を固定する
    add("http_hdr_line_at_limit", 27, headers=[("X-A", "a" * (65536 - len("X-A: ") - 2))])
    add("http_hdr_line_over_limit", 28, headers=[("X-A", "a" * (65536 - len("X-A: ") - 1))])
    add("http_hdr_count_at_limit", 29, headers=[(f"X-{i}", "1") for i in range(95)])
    add("http_hdr_count_over_limit", 30, headers=[(f"X-{i}", "1") for i in range(96)])
    # 要求行（"POST " + 対象 + " HTTP/1.1\r\n"）も 1 行 65536 バイトまで。超えると 414
    for name, k, extra in (("http_line_at_limit", 31, 0), ("http_line_over_limit", 32, 1)):
        room = 65536 - len("POST ") - len(" HTTP/1.1\r\n") - len(SETLOC) - len("?")
        body = '{"id": 4, "lat": 50.%02d, "lon": 60.%02d}' % (k, k)
        cases.append(case(name, SETLOC + "?" + "a" * (room + extra), body))
    return cases


def _chunked_cases():
    body = '{"id": 4, "lat": 50.%02d, "lon": 60.%02d}'
    out = []
    for name, k, length in (("http_te_chunked_only", 14, None),
                            ("http_te_chunked_with_cl", 15, "auto")):
        raw = body % (k, k)
        framed = f"{len(raw.encode('utf-8')):x}\r\n{raw}\r\n0\r\n\r\n"
        out.append(case(name, SETLOC, framed, length=length,
                        headers=[("Transfer-Encoding", "chunked")]))
    return out


CASES += _http_cases() + _chunked_cases()

# Python と Rust で応答が違うケース。名前 → 理由。Rust のテストは、ここに無いケースは状態コードと
# 応答のJSON・DBの変化がすべて一致することを要求し、ここにあるケースは Rust のテストが持つ期待値を使う。
KNOWN_DIFFERENCES = {
    "setloc_nan_lat": "JSON の NaN は Python だけが受け付ける。Rust は JSON として解釈できず 400（文面だけ違う）",
    "setloc_infinity_lon": "JSON の Infinity は Python だけが受け付ける。Rust は 400（文面だけ違う）",
    "setloc_id_beyond_i64": "i64 を超える id: Python は SQLite に渡せず 500、Rust は存在しない id として 404",
    "setflag_id_beyond_i64": "同上",
    "route_backslash": "パスの \\ : Python は / に読み替えて当たる、Rust は 403（より厳しい）",
    # HTTP 層（axum/hyper）が Content-Length を先に拒否する境界。どれも Rust は書き込まない（拒否の向きの差。
    # server.md §17.1）。Python が書く（200）ケースもあるが、Rust が書いて Python が拒否するケースは、
    # 4300 桁超の Content-Length（http_cl_zeros_*）と大きさの上限（http_hdr_*・http_line_*）で見つかり、
    # Rust 側（post.rs）で断るようにした。ここに載せるのは Rust が書かないものだけ
    "http_cl_u64_max": "u64 の最大値: Python は 413、Rust は hyper が 431（本文を読まず接続を閉じる）",
    "http_cl_u64_max_plus1": "u64 を超える値: Python は 413、Rust は hyper が 400（本文なし）",
    "http_cl_not_digits": "数字でない Content-Length: 状態コードは 400 で同じ。Rust は hyper が先に断り本文が定型 JSON でない",
    "http_cl_not_digits_text_plain": "数字でない Content-Length と text/plain: Python は 403（Content-Type が先）、Rust は hyper が 400",
    "http_cl_negative": "負の Content-Length: 400 で同じ。Rust は本文が定型 JSON でない",
    "http_cl_empty": "空の Content-Length: 400 で同じ。Rust は本文が定型 JSON でない",
    "http_cl_plus_sign": "+付きの Content-Length: Python は int() が読んで 200、Rust は hyper が 400",
    "http_cl_underscore": "アンダースコア入りの Content-Length: Python は int() が読んで 200、Rust は hyper が 400",
    "http_cl_list_same": "Content-Length: N, N: 400 で同じ。Rust は本文が定型 JSON でない",
    "http_cl_dup_first_ok_second_big": "重複して後ろが違う値: Python は最初の値で 200、Rust は hyper が 400",
    "http_cl_dup_first_big_second_ok": "重複して最初が巨大: Python は 413、Rust は hyper が 400",
    "http_cl_dup_second_over_limit": "重複して後ろが上限超過: Python は最初の値で 200、Rust は hyper が 400",
    "http_cl_plus_zeros_4301": "+付きで 4301 桁の Content-Length: 状態コードは 400 で同じ（Python は桁数の上限で、Rust は hyper が + を先に断る）。Rust は本文が定型 JSON でない",
    "http_te_chunked_with_cl": "Transfer-Encoding: chunked と Content-Length の併記: 400 で同じ。Python は CL の長さで読んで JSON として解釈できず、Rust は Content-Length が無い扱い（文面が違う）",
}


def build_request(c):
    """要求のバイト列（ヘッダ行の並びと本文）。Python に送るものと、フィクスチャに残すものが同じになる。"""
    if c["body_hex"] is not None:
        body = bytes.fromhex(c["body_hex"])
    else:
        body = (c["body"] or "").encode("utf-8")
    headers = [("Host", c["host"])]
    if c["ct"]:
        headers.append(JSON_CT)
    headers += [tuple(h) for h in c["headers"]]
    send_body = True
    length = c["length"]
    if length == "auto":
        headers.append(("Content-Length", str(len(body))))
    elif isinstance(length, str) and length.startswith("declare:"):
        headers.append(("Content-Length", length.split(":", 1)[1]))
        send_body = False
    elif length is not None:
        headers.append(("Content-Length", length))
    headers.append(("Connection", "close"))
    return headers, (body if send_body else b"")


def _send(port, target, headers, body):
    head = f"POST {target} HTTP/1.1\r\n" + "".join(f"{k}: {v}\r\n" for k, v in headers) + "\r\n"
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        s.sendall(head.encode("utf-8") + body)
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
    status = int(head.split(b" ", 2)[1]) if head else 0
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        parsed = None
    return status, parsed


def _snapshot():
    return {r["id"]: dict(r) for r in store.dbq("SELECT * FROM photos ORDER BY id")}


def _diff(before, after):
    out = {}
    for pid, row in after.items():
        old = before.get(pid)
        if old is None:
            out[str(pid)] = {"_new": row}
            continue
        changed = {k: v for k, v in row.items() if old.get(k) != v}
        if changed:
            out[str(pid)] = changed
    for pid in before:
        if pid not in after:
            out[str(pid)] = {"_deleted": True}
    return out


def build_cases():
    """合成DBに対する現行 Python の出力（JSON に直せる dict）。"""
    results = []
    with seeded(), patch.object(handlers.Handler, "log_message", lambda *a, **k: None), \
            patch("traceback.print_exc", lambda *a, **k: None), \
            patch("builtins.print", lambda *a, **k: None):
        with _http() as port:
            for c in CASES:
                headers, body = build_request(c)
                before = _snapshot()
                status, parsed = _send(port, c["target"], headers, body)
                results.append({
                    "name": c["name"], "target": c["target"],
                    "headers": [list(h) for h in headers],
                    "body_hex": body.hex(),
                    "status": status, "body": parsed,
                    "changes": _diff(before, _snapshot()),
                    "status_only": c["status_only"],
                })
    return {"cases": results, "known_differences": KNOWN_DIFFERENCES}


def render(cases):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    body = ",\n".join("  " + dump(item) for item in cases["cases"])
    return ("{\n"
            f' "cases": [\n{body}\n ],\n'
            f' "known_differences": {dump(cases["known_differences"])}\n'
            "}\n")
