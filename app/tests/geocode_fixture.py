"""地名検索（`GET /api/admin/geocode`）の契約テスト用フィクスチャ。

現行の `handlers.Handler` に実際に GET を流し、地理院・Nominatim への通信（`urllib.request.urlopen`）を
合成の応答に差し替えて、下の `CASES` を**順に**流す。各ステップについて、状態コード・応答のJSON・上流への要求
（種類・クエリ文字列・User-Agent・タイムアウト）・Nominatim の1秒1件のための待ち（`time.sleep` の引数）・直後の
`.cache.json` 相当のメモリキャッシュの全体を記録する。時計（`geo.time`）は偽の時計で、`advance`（要求の前に進める）と
応答の `took`（通信にかかった時間）で進む。実際には待たない。

Rust の結合テスト（crates/geoframe/src/geocode/tests.rs）は、同じ要求を同じ順に流し、同じ偽の時計と同じ応答を返す
ループバックのスタブで答えて、記録と一致することを確かめる。

再生成: `python3 tools/gen_geocode_fixture.py`（差分の検査は `--check`）。
地名・座標・検索語はすべて合成の値で、実在のものを使わない。

このフィクスチャに載せたケースは、Rust も Python と同じ結果にする。Python と意図して違える点（本文の上限、JSON に書けない
数値、巨大な整数など）は載せず、Rust の単体テストが別に確かめる（docs/rust-migration/server.md §0・§18）。
"""
import contextlib
import copy
import http.client
import io
import json
import urllib.error
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from geoframe import geo, handlers, store

from tests.admin_api_fixture import _http

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/geocode-python.json"

GEOCODE = "/api/admin/geocode"
START = 100.0           # 偽の時計の起点（`_nominatim_last` の初期値 0 から1秒以上離す）


# ---- 合成の応答 -------------------------------------------------------------------------------

def ok(body):
    """200 と、JSON にした本文。"""
    return {"status": 200, "body": json.dumps(body, ensure_ascii=False)}


def raw(text, status=200):
    """200 と、JSON とは限らない本文。"""
    return {"status": status, "body": text}


def raw_hex(data, status=200):
    """200 と、UTF-8 とは限らないバイト列の本文。"""
    return {"status": status, "body_hex": data.hex()}


def status(code):
    return {"status": code, "body": ""}


HANGUP = {"hangup": True}


def feat(title, lon, lat):
    """地理院の1件（座標は [経度, 緯度]）。"""
    return {"geometry": {"type": "Point", "coordinates": [lon, lat]}, "properties": {"title": title}}


def row(name, lat, lon):
    """Nominatim の1件（座標は文字列で返る）。"""
    return {"lat": str(lat), "lon": str(lon), "display_name": name, "place_id": 1}


def get(target, gsi=None, osm=None, advance=None):
    step = {"get": target}
    if gsi is not None:
        step["gsi"] = gsi
    if osm is not None:
        step["osm"] = osm
    if advance is not None:
        step["advance"] = advance
    return step


def q(text, **kw):
    """`/api/admin/geocode?q=` に検索語を（パーセントエンコードして）付けた GET。"""
    from urllib.parse import quote
    return get(f"{GEOCODE}?q={quote(text, safe='')}", **kw)


def direct(text, countrycode, osm=None, advance=None):
    """`geo.nominatim_search(q, countrycode)` の直接の呼び出し（E4 からは国コードを付けない）。"""
    step = {"nominatim": {"q": text, "countrycode": countrycode}}
    if osm is not None:
        step["osm"] = osm
    if advance is not None:
        step["advance"] = advance
    return step


SAMPLE = ok([feat("架空町一丁目", 135.5, 35.25), feat("架空町二丁目", 135.75, 35.5)])
SAMPLE_OSM = ok([row("Nowhereville, Sampleland", "12.5", "45.25"),
                 row("Nowhereville Station, Sampleland", "12.75", "45.5")])
NO_HIT = ok([])


def many_gsi(n):
    return ok([feat(f"架空{i}", 100.0 + i, 10.0 + i) for i in range(n)])


def many_osm(n):
    return ok([row(f"Place {i}", 10.5 + i, 100.5 + i) for i in range(n)])


# ---- ケース -----------------------------------------------------------------------------------
# 1ケース = 新しい偽の時計・空のメモリキャッシュ（`initial_cache` があればそこから）・`_nominatim_last = 0` で、
# `steps` を順に流す。ステップに載せた `gsi`・`osm` が、そのステップで上流が返す応答。

CASES = [
    # ---- 検索語の取り出しと2文字未満 ----
    {"name": "no_q_is_empty", "steps": [get(GEOCODE), get(GEOCODE + "?"), get(GEOCODE + "?x=1")]},
    {"name": "blank_q_is_dropped_like_parse_qs", "steps": [
        get(GEOCODE + "?q="), get(GEOCODE + "?q=&q=%20"), get(GEOCODE + "?q"),
    ]},
    {"name": "one_char_makes_no_request", "steps": [
        q("a"), q("東"), q(" a "), q("　　あ　"), q("a "), q("\x1ca\x1f"),
        q("   "), q("\x1c\x1d\x1e\x1f"), 
    ]},
    {"name": "two_chars_after_strip_makes_requests", "steps": [
        q("ab", gsi=SAMPLE),
        q("  ab  ", gsi=SAMPLE),
        q("\x1cab\x1f", gsi=SAMPLE),
        q("　あい　", gsi=SAMPLE),
        q("東京", gsi=SAMPLE),
        q("\u200ba", gsi=SAMPLE),                           # ZERO WIDTH SPACE は空白ではない（2文字）
    ]},
    {"name": "first_q_wins_and_plus_is_space", "steps": [
        get(GEOCODE + "?q=ab&q=cd", gsi=SAMPLE),
        get(GEOCODE + "?q=&q=cd", gsi=SAMPLE),
        get(GEOCODE + "?q=a+b", gsi=SAMPLE),
        get(GEOCODE + "?q=a%2Bb", gsi=SAMPLE),
        get(GEOCODE + "?q=%E3%81%82%E3%81%84&x=1&q=zz", gsi=SAMPLE),
        get(GEOCODE + "?q=ab#fragment", gsi=SAMPLE),
        get(GEOCODE + "?q=ab%ff", gsi=SAMPLE),
        get(GEOCODE + "?q=%ff%fe", gsi=SAMPLE),
    ]},
    {"name": "route_matches_by_raw_prefix", "steps": [
        get(GEOCODE + "XYZ?q=ab", gsi=SAMPLE),
        get("/" + GEOCODE + "?q=ab", gsi=SAMPLE),
        get("///" + GEOCODE.lstrip("/") + "?q=cd", gsi=SAMPLE),
    ]},
    # ---- URL のエンコード（urllib.parse.quote） ----
    {"name": "query_is_percent_encoded_like_quote", "steps": [
        q("a b", gsi=SAMPLE),
        q("a/b", gsi=SAMPLE),
        q("a~b_c-d.e", gsi=SAMPLE),
        q("a&b=c?d#e", gsi=SAMPLE),
        q("(a)*b'c!", gsi=SAMPLE),
        q("架空町 1-2-3", gsi=SAMPLE),
        q("a%b", gsi=SAMPLE),
        q("a+b", gsi=SAMPLE),
        q("\U0001f600ab", gsi=SAMPLE),
        q("a:b@c$d,e;f", gsi=SAMPLE),
    ]},
    # ---- 地理院 ----
    {"name": "gsi_hit_does_not_query_nominatim", "steps": [q("架空町", gsi=SAMPLE, osm=SAMPLE_OSM)]},
    {"name": "gsi_values_and_titles", "steps": [
        q("ab", gsi=ok([
            feat("  余白あり  ", 1, 2),                           # 整数の座標は float になる。title は strip
            feat("", 3.5, 4.5),                                   # 空の title は検索語
            {"geometry": {"coordinates": [5.5, 6.5]}},            # properties 無し
            {"geometry": {"coordinates": [7.5, 8.5]}, "properties": None},
            {"geometry": {"coordinates": [9.5, 10.5]}, "properties": {}},
            {"geometry": {"coordinates": [11.5, 12.5]}, "properties": {"title": None}},
            {"geometry": {"coordinates": [13.5, 14.5]}, "properties": {"title": 0}},
            {"geometry": {"coordinates": [15.5, 16.5]}, "properties": {"title": []}},
            {"geometry": {"coordinates": [17.5, 18.5]}, "properties": {"title": "\x1c　"}},
            feat("範囲外でも通る", 500.5, -200.25),                 # 地理院の座標は範囲を見ない
        ])),
        q("cd", gsi=ok([
            {"geometry": {"coordinates": [True, False]}, "properties": {"title": "bool"}},   # bool は int のうち
            feat("指数", 1e2, 2.5e1),
            feat("大きい整数", 12345678901234567890, -9007199254740993),
            feat("小さい値", 1e-300, -0.0),
            feat("桁の多い値", 135.12345678901234567890, 35.00000000000000011),
        ])),
    ]},
    {"name": "gsi_skips_unusable_features_without_dropping_the_rest", "steps": [
        # 先頭から10件だけを見るので、使えない要素を9件ずつ並べて、最後に使える1件を置く
        q("a1", gsi=ok([None, 0, False, "", [], {}, 0.0,                       # 偽の要素は {} 扱いで飛ばす
                        {"geometry": None}, {"geometry": {}}, feat("残る1", 20.5, 30.5)])),
        q("a2", gsi=ok([{"geometry": 0}, {"geometry": ""}, {"geometry": []}, {"geometry": False},
                        {"geometry": {"coordinates": None}}, {"geometry": {"coordinates": "ab"}},
                        {"geometry": {"coordinates": [1.5]}}, {"geometry": {"coordinates": []}},
                        {"geometry": {"coordinates": [1.5, 2.5, 3.5]}}, feat("残る2", 21.5, 31.5)])),
        q("a3", gsi=ok([{"geometry": {"coordinates": ["1.5", 2.5]}}, {"geometry": {"coordinates": [1.5, None]}},
                        {"geometry": {"coordinates": [[1.5], 2.5]}}, {"geometry": {"coordinates": [1.5, [2.5]]}},
                        {"geometry": {"coordinates": [{"a": 1}, 2.5]}},
                        {"geometry": {"coordinates": {"0": 1.5, "1": 2.5}}},
                        {"geometry": {"coordinates": [None, None]}}, {"geometry": {"coordinates": ["a", "b"]}},
                        {"properties": {"title": "座標なし"}}, feat("残る3", 22.5, 32.5)])),
    ]},
    {"name": "gsi_poisoned_element_empties_the_whole_gsi_result", "steps": [
        # 真の非 dict の要素・geometry・properties・数や配列の title は、`.get`・`.strip` が例外になって
        # 地理院の結果全体が空になり、Nominatim へ回る（その前に集めた要素も捨てる）
        q("a1", gsi=ok([feat("前", 1.5, 2.5), "text"]), osm=NO_HIT),
        q("a2", gsi=ok([feat("前", 1.5, 2.5), [1]]), osm=NO_HIT),
        q("a3", gsi=ok([feat("前", 1.5, 2.5), 5]), osm=NO_HIT),
        q("a4", gsi=ok([feat("前", 1.5, 2.5), True]), osm=NO_HIT),
        q("a5", gsi=ok([feat("前", 1.5, 2.5), {"geometry": "x"}]), osm=NO_HIT),
        q("a6", gsi=ok([feat("前", 1.5, 2.5), {"geometry": [1]}]), osm=NO_HIT),
        q("a7", gsi=ok([feat("前", 1.5, 2.5), {"geometry": {"coordinates": [1.5, 2.5]}, "properties": "x"}]),
          osm=NO_HIT),
        q("a8", gsi=ok([feat("前", 1.5, 2.5), {"geometry": {"coordinates": [1.5, 2.5]}, "properties": [1]}]),
          osm=NO_HIT),
        q("a9", gsi=ok([feat("前", 1.5, 2.5), {"geometry": {"coordinates": [1.5, 2.5]},
                                               "properties": {"title": 5}}]), osm=NO_HIT),
        q("b1", gsi=ok([feat("前", 1.5, 2.5), {"geometry": {"coordinates": [1.5, 2.5]},
                                               "properties": {"title": ["x"]}}]), osm=NO_HIT),
        q("b2", gsi=ok([feat("前", 1.5, 2.5), {"geometry": {"coordinates": [1.5, 2.5]},
                                               "properties": {"title": True}}]), osm=NO_HIT),
        # 座標が使えない要素は、geometry・properties の型を見る前に飛ばされるので、毒にならない
        q("b3", gsi=ok([{"geometry": {"coordinates": [1.5]}, "properties": "x"}, feat("残る", 3.5, 4.5)])),
        # 11件目以降は見ない
        q("b4", gsi=ok([feat(f"n{i}", 1.5 + i, 2.5) for i in range(10)] + ["text"])),
    ]},
    {"name": "gsi_takes_the_first_ten_before_filtering", "steps": [
        q("ab", gsi=many_gsi(25)),
        q("cd", gsi=ok([{"geometry": None}] * 4 + [feat(f"n{i}", 1.5 + i, 2.5) for i in range(10)])),
        q("ef", gsi=ok([{"geometry": None}] * 10 + [feat("11件目", 1.5, 2.5)]), osm=NO_HIT),
    ]},
    {"name": "gsi_non_list_payload_falls_back", "steps": [
        q("ab", gsi=ok({"type": "FeatureCollection", "features": []}), osm=NO_HIT),
        q("cd", gsi=ok("text"), osm=NO_HIT),
        q("ef", gsi=ok(None), osm=NO_HIT),
        q("gh", gsi=ok(5), osm=NO_HIT),
    ]},
    {"name": "gsi_zero_hits_falls_back_to_nominatim", "steps": [
        q("ab", gsi=NO_HIT, osm=SAMPLE_OSM),
    ]},
    # ---- 地理院の失敗 ----
    {"name": "gsi_failures_fall_back_to_nominatim", "steps": [
        q("a1", gsi=status(404), osm=SAMPLE_OSM, advance=2.0),
        q("a2", gsi=status(500), osm=SAMPLE_OSM, advance=2.0),
        q("a3", gsi=status(429), osm=SAMPLE_OSM, advance=2.0),
        q("a4", gsi=HANGUP, osm=SAMPLE_OSM, advance=2.0),
        q("a5", gsi=raw("not json"), osm=SAMPLE_OSM, advance=2.0),
        q("a6", gsi=raw(""), osm=SAMPLE_OSM, advance=2.0),
        q("a7", gsi=raw_hex(b"[\xff]"), osm=SAMPLE_OSM, advance=2.0),
        q("a8", gsi=raw("﻿[]"), osm=SAMPLE_OSM, advance=2.0),
        q("a9", gsi=raw_hex(b"\xef\xbb\xbf[]"), osm=SAMPLE_OSM, advance=2.0),
        q("b1", gsi=raw("[1, 2,]"), osm=SAMPLE_OSM, advance=2.0),
        q("b2", gsi=raw("  []  "), osm=SAMPLE_OSM, advance=2.0),
        q("b3", gsi=status(204), osm=SAMPLE_OSM, advance=2.0),
        q("b4", gsi=raw('[{"geometry": {"coordinates": [1.5, 2.5]}}, '), osm=SAMPLE_OSM, advance=2.0),
    ]},
    # ---- Nominatim ----
    {"name": "nominatim_values", "steps": [
        q("ab", gsi=NO_HIT, osm=ok([
            row("  余白あり  ", "12.5", "45.25"),
            {"lat": "1", "lon": "2", "display_name": ""},        # 空の名前は検索語
            {"lat": "3", "lon": "4"},                              # 名前無し
            {"lat": "5", "lon": "6", "display_name": None},
            {"lat": "7", "lon": "8", "display_name": 0},
            {"lat": "9", "lon": "10", "display_name": []},
            {"lat": 11.5, "lon": 12.5, "display_name": "数値の座標"},
            {"lat": 13, "lon": 14, "display_name": "整数の座標"},
            {"lat": True, "lon": False, "display_name": "bool"},
            {"lat": "90", "lon": "180", "display_name": "上限"},
            {"lat": "-90", "lon": "-180", "display_name": "下限"},
            {"lat": "1e1", "lon": "2.5E1", "display_name": "指数"},
            {"lat": ".5", "lon": "5.", "display_name": "省略形"},
            {"lat": "+5", "lon": "-5", "display_name": "符号"},
            {"lat": "0.1234567890123456789", "lon": "0", "display_name": "桁の多い値"},
        ])),
    ]},
    {"name": "nominatim_skips_unusable_rows", "steps": [
        q("ab", gsi=NO_HIT, osm=ok([
            {"lat": "abc", "lon": "1"}, {"lon": "1"}, {"lat": "1"},
            {"lat": "90.0001", "lon": "1"}, {"lat": "-90.0001", "lon": "1"},
            {"lat": "1", "lon": "180.0001"}, {"lat": "1", "lon": "-180.0001"},
            {"lat": "nan", "lon": "1"}, {"lat": "inf", "lon": "1"}, {"lat": "-inf", "lon": "1"},
            {"lat": "1e400", "lon": "1"}, {"lat": "1", "lon": "-1e400"},
            {"lat": None, "lon": "1"}, {"lat": "1", "lon": None},
            {"lat": [1], "lon": "1"}, {"lat": "1", "lon": {"a": 1}},
            {"lat": "", "lon": "1"}, {"lat": " ", "lon": "1"}, {"lat": "1,5", "lon": "1"},
            {"lat": "0x10", "lon": "1"}, {"lat": "1 2", "lon": "1"},
            {"lat": 91, "lon": 1}, {"lat": 1, "lon": 181},
            "text", 5, None, [], [1, 2], True,
            row("残る", "20.5", "30.5"),
        ])),
    ]},
    {"name": "nominatim_float_like_python", "steps": [
        q("ab", gsi=NO_HIT, osm=ok([
            {"lat": " 12.5 ", "lon": "\t45.25\n", "display_name": "空白"},
            {"lat": " 13.5　", "lon": " 46.5 ", "display_name": "Unicodeの空白"},
            {"lat": "\x1c13.5", "lon": "46.5", "display_name": "ASCIIの制御文字の空白"},
            {"lat": "\x0b13.5\x0c", "lon": "46.5\r\n", "display_name": "ASCIIの空白"},
            {"lat": "１２.５", "lon": "１２", "display_name": "全角の数字とASCIIの小数点"},
            {"lat": "1_2.5", "lon": "4_5", "display_name": "アンダースコア"},
            {"lat": "1__2", "lon": "1", "display_name": "二重のアンダースコア"},
            {"lat": "_12", "lon": "1", "display_name": "先頭のアンダースコア"},
            {"lat": "12_", "lon": "1", "display_name": "末尾のアンダースコア"},
            {"lat": "1_.5", "lon": "1", "display_name": "小数点の前のアンダースコア"},
            {"lat": "1._5", "lon": "1", "display_name": "小数点の後のアンダースコア"},
            {"lat": "1e1_0", "lon": "1", "display_name": "指数のアンダースコア"},
            {"lat": "１２．５", "lon": "12", "display_name": "全角の小数点"},
            {"lat": "١٢", "lon": "٣٤", "display_name": "アラビア・インドの数字"},
            {"lat": "1e-2", "lon": "-0", "display_name": "負のゼロ"},
            {"lat": "+.5e+1", "lon": "-.5E-1", "display_name": "符号と指数"},
            {"lat": "1e", "lon": "1", "display_name": "指数の途中"},
            {"lat": ".", "lon": "1", "display_name": "小数点だけ"},
            {"lat": "+-1", "lon": "1", "display_name": "符号の重複"},
            {"lat": "12abc", "lon": "1", "display_name": "文字つき"},
            {"lat": "Infinity", "lon": "1", "display_name": "無限大"},
        ])),
    ]},
    {"name": "nominatim_takes_ten_valid_rows", "steps": [
        q("ab", gsi=NO_HIT, osm=many_osm(25)),
        q("cd", gsi=NO_HIT, osm=ok([{"lat": "x"}] * 4 + [row(f"p{i}", 1.5 + i, 2.5) for i in range(12)]),
          advance=2.0),
    ]},
    {"name": "nominatim_zero_hits_are_cached", "steps": [
        q("ab", gsi=NO_HIT, osm=NO_HIT),
        q("ab", gsi=NO_HIT),
        q("ab", gsi=NO_HIT, advance=5.0),
    ]},
    {"name": "nominatim_hit_is_cached_and_gsi_is_not", "steps": [
        q("ab", gsi=NO_HIT, osm=SAMPLE_OSM),
        q("ab", gsi=NO_HIT),                       # 地理院は毎回引き、Nominatim はキャッシュから
        q("ab", gsi=NO_HIT, advance=0.25),
        q(" ab ", gsi=NO_HIT),                     # 同じキー（strip 後）
        q("ac", gsi=NO_HIT, osm=SAMPLE_OSM),       # 別の語は別のキー
    ]},
    {"name": "gsi_hit_is_never_cached", "steps": [
        q("ab", gsi=SAMPLE),
        q("ab", gsi=SAMPLE),
    ]},
    {"name": "nominatim_failures_are_not_cached", "steps": [
        q("a1", gsi=NO_HIT, osm=status(429)),
        q("a1", gsi=NO_HIT, osm=status(500), advance=2.0),
        q("a1", gsi=NO_HIT, osm=HANGUP, advance=2.0),
        q("a1", gsi=NO_HIT, osm=raw("not json"), advance=2.0),
        q("a1", gsi=NO_HIT, osm=raw_hex(b"[\xff]"), advance=2.0),
        q("a1", gsi=NO_HIT, osm=raw(""), advance=2.0),
        q("a1", gsi=NO_HIT, osm=ok({"error": "rate limited"}), advance=2.0),       # 想定外の形式
        q("a1", gsi=NO_HIT, osm=ok({"lat": "1", "lon": "2"}), advance=2.0),
        q("a1", gsi=NO_HIT, osm=ok("text"), advance=2.0),
        q("a1", gsi=NO_HIT, osm=ok(None), advance=2.0),
        q("a1", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),                          # 最後に成功すれば保存
    ]},
    {"name": "both_upstreams_fail", "steps": [
        q("ab", gsi=HANGUP, osm=HANGUP),
        q("cd", gsi=status(500), osm=status(500), advance=3.0),
    ]},
    {"name": "cache_entries_written_by_python_are_used_as_is", "initial_cache": {
        "geocode_osm:ab": [{"title": "保存済み", "lat": 1.5, "lon": 2.5, "source": "osm"}],
        "geocode_osm:cd": [],
        "geocode_osm:ef": [{"extra": 1}, 5, "text", None],
        "geocode_osm:gh": {"not": "a list"},
        "geocode_osm:ij": "text",
        "geocode_osm:kl": None,
        "geocode_osm:mn": 0,
        "geo:35.000,135.000": {"place": "架空", "elev": 1},
        "muni_table": {"1": ["架空県", "架空市"]},
    }, "steps": [
        q("ab", gsi=NO_HIT),
        q("cd", gsi=NO_HIT),
        q("ef", gsi=NO_HIT),
        q("gh", gsi=NO_HIT, osm=SAMPLE_OSM),       # リストでない値はミスとして取り直して上書き
        q("ij", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
        q("kl", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
        q("mn", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
    ]},
    {"name": "cache_key_is_the_stripped_query", "steps": [
        q("  a b  ", gsi=NO_HIT, osm=SAMPLE_OSM),
        q("a b", gsi=NO_HIT),
        q("a+b", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
        q("geo:x", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
        q("jp:ab", gsi=NO_HIT, osm=SAMPLE_OSM, advance=2.0),
    ]},
    # ---- 1秒1件 ----
    {"name": "nominatim_waits_one_second_after_the_previous_call_ended", "steps": [
        q("a1", gsi=NO_HIT, osm=dict(NO_HIT, took=0.5)),                      # 最初は待たない
        q("a2", gsi=NO_HIT, osm=dict(NO_HIT, took=0.25)),                     # 前の終わりから 1 秒 → 1.0 待つ
        q("a3", gsi=NO_HIT, osm=dict(NO_HIT, took=0.25), advance=0.25),      # 0.75 待つ
        q("a4", gsi=NO_HIT, osm=dict(NO_HIT, took=0.0), advance=0.75),       # 0.25 待つ
        q("a5", gsi=NO_HIT, osm=NO_HIT, advance=1.0),                          # ちょうど 1 秒たっている → 待たない
        q("a6", gsi=NO_HIT, osm=NO_HIT, advance=1.5),                          # 待たない
        q("a7", gsi=NO_HIT, osm=NO_HIT, advance=0.5),                          # 0.5 待つ
    ]},
    {"name": "failures_also_start_the_wait", "steps": [
        q("a1", gsi=NO_HIT, osm=dict(status(500), took=0.5)),
        q("a2", gsi=NO_HIT, osm=dict(HANGUP, took=0.25)),                     # 前の失敗の終わりから 1 秒
        q("a3", gsi=NO_HIT, osm=dict(raw("not json"), took=0.5)),
        q("a4", gsi=NO_HIT, osm=NO_HIT),
    ]},
    {"name": "cache_hits_and_gsi_hits_do_not_wait", "steps": [
        q("a1", gsi=NO_HIT, osm=dict(SAMPLE_OSM, took=0.5)),
        q("a1", gsi=NO_HIT),                                                   # キャッシュ: 待たず、時計も進まない
        q("a2", gsi=SAMPLE),                                                   # 地理院で済む: Nominatim に触れない
        q("a3", gsi=NO_HIT, osm=NO_HIT),                                       # 1 秒の待ちは a1 の終わりから
    ]},
    {"name": "gsi_calls_are_not_paced", "steps": [
        q("a1", gsi=dict(SAMPLE, took=0.25)), q("a2", gsi=dict(SAMPLE, took=0.25)),
        q("a3", gsi=dict(SAMPLE, took=0.25)),
    ]},
    # ---- 国コード（E4 からは付かない。位置候補の座標化が使う） ----
    {"name": "country_code_goes_to_url_and_cache_key", "steps": [
        direct("Cairo", "EG", osm=SAMPLE_OSM),
        direct("Cairo", "EG"),                                                  # キャッシュ
        direct("Cairo", "", osm=SAMPLE_OSM),                                    # 国を絞らない検索は別のキー
        direct("Cairo", "eg"),                                                  # 小文字でも同じキー
        direct("Cairo", " Eg ", osm=SAMPLE_OSM),
        direct("Cairo", "E1", osm=SAMPLE_OSM, advance=2.0),                    # 不正な国コードは無視（国なしのキー）
        direct("Cairo", "EGY", osm=SAMPLE_OSM, advance=2.0),
        direct("Cairo", "E", osm=SAMPLE_OSM, advance=2.0),
        direct("Cairo", "ＪＰ", osm=SAMPLE_OSM, advance=2.0),
        direct("Cairo", "KK", osm=SAMPLE_OSM, advance=2.0),           # K (KELVIN SIGN) は小文字化すると ASCII の k
        direct("Cairo", "jp", osm=SAMPLE_OSM, advance=2.0),
    ]},
    {"name": "direct_call_does_not_strip_the_query", "steps": [
        direct("  Cairo  ", "", osm=SAMPLE_OSM),
        direct("Cairo", "", osm=SAMPLE_OSM, advance=2.0),
    ]},
]

# GET のうち、`/api/admin/` 配下で Python が 403 にするもの（`status`・`photos`・`geocode` の接頭辞に当たらない）
ADMIN_GETS = [
    "/api/admin/setloc", "/api/admin/setflag", "/api/admin/rotate",
    "/api/admin/", "/api/admin", "/api/admin/zzz", "/api/admin/Status", "/api/admin/Geocode?q=ab",
    "/api/admin/%67eocode?q=ab", "/api/admin/stat", "/api/admin/photo", "/api/admin/geo?q=ab",
    "//api/admin/setloc", "/api/admin/setloc?id=1", "/api/admin/setloc/", "/api/admin/rotate/x",
    "/api/admin/publish", "/api/admin/trips",
    # `..` で領域の外へ出る形（正規化後も許可の接頭辞に入らず、現行は 403）
    "/api/admin/../static/js/admin.js", "/api/admin/%2e%2e/static/js/admin.js",
    "/api/admin/./../static/js/admin.js",
]

# ---- 実行 -------------------------------------------------------------------------------------

class _FakeResponse:
    def __init__(self, body):
        self._body = body

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self):
        return self._body


def _body_of(reply):
    if "body_hex" in reply:
        return bytes.fromhex(reply["body_hex"])
    return reply.get("body", "").encode("utf-8")


def _target_get(port, target):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", target, headers={"Host": "127.0.0.1"})
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


def run_case(case, port):
    clock = {"t": START}
    sleeps, requests, unscripted = [], [], []
    replies = {}

    def fake_urlopen(req, timeout=None):
        url = req.full_url
        upstream = "osm" if "nominatim" in url else "gsi"
        reply = replies.get(upstream)
        if reply is None:
            # 呼び出し側（geo.py）が例外を握りつぶすので、その場で落とさず、あとで検出する
            unscripted.append(f"{case['name']}: {upstream}")
            raise urllib.error.URLError("unscripted")
        requests.append({
            "upstream": upstream,
            "query": url.split("?", 1)[1],
            "user_agent": req.get_header("User-agent"),
            "timeout": timeout,
        })
        clock["t"] += reply.get("took", 0.0)
        if reply.get("hangup"):
            raise urllib.error.URLError("closed")
        code = reply.get("status", 200)
        if not 200 <= code < 300:
            raise urllib.error.HTTPError(url, code, "x", {}, io.BytesIO(b""))
        return _FakeResponse(_body_of(reply))

    def fake_sleep(seconds):
        sleeps.append(seconds)
        clock["t"] += seconds

    fake_time = SimpleNamespace(sleep=fake_sleep, monotonic=lambda: clock["t"])
    out = {"name": case["name"], "steps": []}
    if "initial_cache" in case:
        out["initial_cache"] = case["initial_cache"]
    with patch.object(geo, "time", fake_time), patch.object(geo, "_nominatim_last", 0.0), \
            patch.object(store, "_cache", copy.deepcopy(case.get("initial_cache", {}))), \
            patch("urllib.request.urlopen", fake_urlopen):
        for step in case["steps"]:
            requests.clear()
            sleeps.clear()
            replies.clear()
            for upstream in ("gsi", "osm"):
                if upstream in step:
                    replies[upstream] = step[upstream]
            clock["t"] += step.get("advance", 0.0)
            record = dict(step)
            if "get" in step:
                code, body = _target_get(port, step["get"])
                record["status"] = code
                record["body"] = json.loads(body)
            else:
                spec = step["nominatim"]
                record["items"] = geo.nominatim_search(spec["q"], spec["countrycode"])
            record["requests"] = [dict(r) for r in requests]
            record["sleeps"] = list(sleeps)
            record["cache"] = copy.deepcopy(store._cache)
            out["steps"].append(record)
    assert not unscripted, f"応答の指定が無い上流への通信: {unscripted}"
    return out


def samples():
    """Python の関数そのものの出力（Rust の小さな関数の突き合わせ用）。"""
    from urllib.parse import quote
    float_inputs = [
        "0", "1", "-1", "+1", "1.5", ".5", "5.", "1e3", "1E3", "1e-3", "+1e+3", " 1 ", "\t1\n", "\x0b1\x0c", "\x1c1\x1f",
        " 1　", " 1 ", "​1", "1_0", "1_0.5", "1__0", "_1", "1_", "1_e3", "1e_3", "1e3_", "1_000_000",
        "0_0", "1._5", "1_.5", "٣", "١٢٫٥", "１２", "１２．５", "１_２", "1e１", "Ⅻ", "½", "²", "1,5", "", " ", "-", "+", ".", "e5", "1e",
        "1e+", "0x10", "1 2", "--1", "+-1", "inf", "-inf", "+Infinity", "INF", "nan", "-NaN", "infinit", "in f", "1e400", "-1e400",
        "1e-400", "0.1", "0.30000000000000004", "123456789012345678901234567890", "1.7976931348623157e308", "1.8e308",
        "4.9e-324", "2.5e-324", "12.3456789012345678", "-0", "-0.0", "\x00", "1\x00", "a", "1a", "١٢٣٤٥٦٧٨٩٠", "０．５",
    ]
    floats = []
    for s in float_inputs:
        try:
            floats.append({"s": s, "f": repr(float(s))})
        except ValueError:
            floats.append({"s": s, "f": None})
    quote_inputs = [
        "", "abc", "ABC xyz 019", "a b", "a/b", "a~b", "a_b", "a-b", "a.b", "a+b", "a%b", "a&b=c?d#e", "(a)*b'c!", "a:b@c$d,e;f",
        "\"<>\\^`{|}[]", "架空町", "架空町 1-2-3", "\U0001f600", "é", " ", "\x00\x1f\x7f", "�", "a\tb\nc\rd",
    ]
    quotes = [{"s": s, "q": quote(s)} for s in quote_inputs]
    spaces = [i for i in range(0x110000) if chr(i).isspace()]
    return {"float_samples": floats, "quote_samples": quotes, "space_code_points": spaces}


def build_cases():
    out = io.StringIO()
    with contextlib.redirect_stdout(out), \
            patch.object(handlers.Handler, "log_message", lambda *a, **k: None):
        with _http() as port:
            cases = [run_case(c, port) for c in CASES]
            admin_gets = []
            with patch("urllib.request.urlopen", _fail_urlopen):
                for target in ADMIN_GETS:
                    code, _ = _target_get(port, target)
                    admin_gets.append({"target": target, "status": code})
    doc = {"cases": cases, "admin_gets": admin_gets,
           "constants": {"user_agent": "GeoFrame/0.2", "timeout": 8, "min_len": geo.GEOCODE_MIN_LEN,
                         "max_results": geo.GEOCODE_MAX_RESULTS,
                         "min_interval": geo.NOMINATIM_MIN_INTERVAL_S}}
    doc.update(samples())
    return doc


def _fail_urlopen(*args, **kwargs):
    raise AssertionError("403 になるはずの GET が外部へ通信しようとしました")


def render(doc):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    body = ",\n".join("  " + dump(item) for item in doc["cases"])
    gets = ",\n".join("  " + dump(item) for item in doc["admin_gets"])
    floats = ",\n".join("  " + dump(item) for item in doc["float_samples"])
    quotes = ",\n".join("  " + dump(item) for item in doc["quote_samples"])
    return ("{\n"
            f' "admin_gets": [\n{gets}\n ],\n'
            f' "cases": [\n{body}\n ],\n'
            f' "constants": {dump(doc["constants"])},\n'
            f' "float_samples": [\n{floats}\n ],\n'
            f' "quote_samples": [\n{quotes}\n ],\n'
            f' "space_code_points": {dump(doc["space_code_points"])}\n'
            "}\n")
