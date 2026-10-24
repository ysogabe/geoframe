#!/usr/bin/env python3
"""位置の推定と地名（`infer_locations`・`enrich_rows`・`muni_table`）の契約テスト用フィクスチャ
（Pythonの`geoframe/geo.py`が基準）。

- `geo-python.json`: 合成の`photos`の行（座標・時刻・地名は架空か公開の代表点。実写真由来の値は無い）に、
  Pythonの`infer_locations`と`enrich_rows`を段階的に流した結果。地理院の通信は`geo.http_json`の差し替えで
  スタブにする（応答は合成。Pythonが実際に出した要求を、種類と座標で順に記録する）。各段階のあとの
  `photos`の全列（`id`順）・`infer_locations`の戻り値・`.cache.json`の`geo:`キーの中身・要求の記録を残す。
  `GEOFRAME_INFER_*`はPythonが`import`時に読むので、設定が違う場合分けは別のプロセスで流す。
  RustのテストがPythonと同じ行・同じ応答のスタブで`infer`・`enrich`の段を流し、列ごとに比べる
  （`crates/geoframe/src/geo/tests.rs`）。
- `muni_js`: 合成の`muni.js`の断片をPythonの`muni_table()`に通した表（Rustの手書きの解析との突き合わせ）。
- `float_keys`: Pythonの`f"{x:.3f}"`（キャッシュのキー）の結果。Rustの`{:.3}`との突き合わせ。
- `time_samples`: `datetime.strptime`の`%Y-%m-%d %H:%M`・`%Y-%m-%d %H:%M:%S`の結果（naiveな通算秒、読めなければnull）。
  日の空白埋め（`2024-05- 6`）、区切りの空白の種類などを含む。Rustの`naive_seconds`・`naive_seconds_with_seconds`との突き合わせ。
  全角の数字は入れない（Rustは読まない既知の差。server.md §0）。

    python3 tools/gen_geo_fixture.py          # 書き出す
    python3 tools/gen_geo_fixture.py --check  # 保存済みのシナリオをPythonに通し直し、結果が一致するか検査する

意図した差（通信の障害での中断、`lon`がNULLの行など）は、このフィクスチャには入れず、Rustの単体テストで確かめる
（`docs/rust-migration/server.md` §0・§15）。出力先は crates/geoframe/tests/fixtures/ の下。
"""

import json
import os
import random
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "geo-python.json"


# ---- シナリオ ---------------------------------------------------------------------------------

def lit(v):
    if v is None:
        return "NULL"
    if isinstance(v, str):
        return "'" + v.replace("'", "''") + "'"
    return repr(v)


def photo(pid, taken_at=None, lat=None, lon=None, loc_source=None, **extra):
    cols = {"id": pid, "source": "drive", "name": f"p{pid}", "taken_at": taken_at,
            "lat": lat, "lon": lon, "loc_source": loc_source}
    cols.update(extra)
    return ("INSERT INTO photos(" + ",".join(cols) + ") VALUES("
            + ",".join(lit(v) for v in cols.values()) + ")")


# 公開の代表点（駅・役所）と架空の座標だけを使う
TOKYO = (35.681236, 139.767125)       # 東京駅
SAPPORO = (43.064301, 141.346932)     # 札幌市役所
NAHA = (26.212401, 127.680932)        # 那覇市役所
PARIS = (48.858370, 2.294481)         # 凱旋門ではなくエッフェル塔の近く（公開の代表点）
SYDNEY = (-33.856784, 151.215297)     # オペラハウス


def infer_cases():
    day = "2024-05-06"
    nxt = "2024-05-07"
    rows = [
        photo(1, f"{day} 10:00", *TOKYO, "exif"),
        photo(2, f"{day} 12:00", *SAPPORO, "drive_meta"),
        photo(3, f"{day} 20:00", *NAHA, "manual"),
        photo(4, f"{day} 10:10"),                       # 1 の 10 分後
        photo(5, f"{day} 10:15"),                       # 1 から 15 分ちょうど
        photo(6, f"{day} 10:16"),                       # 1 から 16 分（2 段目の窓）
        photo(7, f"{day} 22:00"),                       # 3 から 2 時間ちょうど
        photo(8, f"{nxt} 08:00"),                       # 3 から 12 時間ちょうど
        photo(9, f"{nxt} 08:01"),                       # 12 時間 1 分: 範囲外
        photo(11, f"{day} 14:00", *PARIS, "exif"),      # 2 と 11 の真ん中に 10 が来る
        photo(10, f"{day} 13:00"),                      # 2（id 2）と 11 から等距離 → id の小さい 2
        photo(12, f"{day} 15:30", *SYDNEY, "exif"),     # 同じ分の錨が 2 枚
        photo(13, f"{day} 15:30", *TOKYO, "drive_meta"),
        photo(14, f"{day} 15:40"),                      # 12 と 13 から等距離 → 12
        photo(15, f"{day} 10:20", place="旧地名"),      # 地名は触らない
        photo(16, None),                                # 時刻なし
        photo(17, ""),                                  # 時刻が空
        photo(18, "2024-02-30 10:00"),                  # 暦として不正
        photo(19, "2024-5-6 10:5"),                     # 桁が足りない（strptime は通す）
        # 別の日（上の行の錨に影響しない）。手動は再推定しない
        photo(20, "2024-05-20 12:00", *NAHA, "manual"),
        photo(21, "2024-05-20 12:05", *PARIS, "exif"),
        photo(22, "2024-05-20 12:10", *PARIS, None),    # loc_source が無いのに位置がある
        photo(28, "2024-05-20 12:02"),                  # 20（2 分）と 21（3 分）の間 → 20
        photo(23, f"{day} 11:11", *PARIS, "inferred", inferred_from=99, infer_dt_s=1,
              place="残る地名", display="derivatives/x23.jpg"),
        photo(24, "不正な時刻", *PARIS, "inferred", inferred_from=99, infer_dt_s=1,
              display="derivatives/x24.jpg"),
        photo(25, f"{day}  10:30"),                     # 区切りの空白が 2 つ（`\s+`）
        photo(26, f"{day} 10:00 "),                     # 末尾の余分: 解釈できない
        photo(27, "２０２４-０５-０６ １０:４０"),       # 全角の数字（Python も読めない: 月・日・時は ASCII だけ）
        # 日だけは空白埋めを読む（`%d` の ` [1-9]`）。月・時・分・2 桁の日（` 06`・` 16`）は読めない
        photo(29, "2024-05- 6 10:03"),                   # 1 の 3 分後（空白埋めの日）
        photo(30, "2024-05- 6 11:11", *PARIS, "inferred", inferred_from=99, infer_dt_s=1,
              display="derivatives/x30.jpg"),            # 読めるので 1 から再推定される（取り消されない）
        photo(31, "2024-05- 06 10:04", *PARIS, "inferred", inferred_from=99, infer_dt_s=1,
              display="derivatives/x31.jpg"),            # 読めない（取り消す）
        photo(32, "2024- 5-06 10:05"),                   # 月の空白埋めは読めない
        photo(33, "2024-05-06 10: 5"),                   # 分の空白埋めは読めない
    ]
    base = [{"name": "initial", "sql": rows, "run": ["infer"]}]
    steps = base + [
        {"name": "unchanged", "sql": [], "run": ["infer"]},
        {"name": "give_display", "sql": [
            "UPDATE photos SET display='derivatives/d4.jpg' WHERE id IN (4,5,6,7,10)"], "run": []},
        # 錨 1 が消える: 4・5・6 は別の錨へ（または範囲外で取り消し）、display は取り消されたものだけ NULL
        {"name": "anchor_deleted", "sql": ["DELETE FROM photos WHERE id=1"], "run": ["infer"]},
        # 錨の位置が変わる: 推定した行の位置が動き、地名は残る
        {"name": "anchor_moved", "sql": [f"UPDATE photos SET lat={TOKYO[0]}, lon={TOKYO[1]} WHERE id=2",
                                         "UPDATE photos SET place='古い地名' WHERE id=10"], "run": ["infer"]},
        {"name": "unchanged_again", "sql": [], "run": ["infer"]},
        # 推定した行が手動になる → 再推定の対象外になり、ほかの行の錨になる
        {"name": "became_manual", "sql": [
            f"UPDATE photos SET lat={SAPPORO[0]}, lon={SAPPORO[1]}, loc_source='manual', "
            "inferred_from=NULL, infer_dt_s=NULL WHERE id=7"], "run": ["infer"]},
        # 錨が全部消える: 推定はすべて取り消される
        {"name": "all_anchors_gone", "sql": [
            "DELETE FROM photos WHERE loc_source IN ('exif','drive_meta','manual')"], "run": ["infer"]},
    ]
    cases = [{"name": "infer_default", "env": {}, "steps": steps}]
    # 時計のずれ（分）。小数の補正は切り捨て（`int(dt)`）を確かめる
    for name, env in [
        ("offset_minus_90", {"GEOFRAME_INFER_OFFSET_MIN": "-90"}),
        ("offset_plus_30", {"GEOFRAME_INFER_OFFSET_MIN": "30"}),
        ("offset_fraction_plus", {"GEOFRAME_INFER_OFFSET_MIN": "0.01"}),
        ("offset_fraction_minus", {"GEOFRAME_INFER_OFFSET_MIN": "-0.01"}),
        # 1 段目が 12 時間より長ければ、それが最大の窓
        ("wide_first_window", {"GEOFRAME_INFER_MIN": "1000"}),
        ("narrow_first_window", {"GEOFRAME_INFER_MIN": "1"}),
    ]:
        cases.append({"name": f"infer_{name}", "env": env,
                      "steps": [{"name": "initial", "sql": rows, "run": ["infer"]}]})
    return cases


MUNI = {"13101": ["東京都", "千代田区"], "1100": ["北海道", "札幌市"], "47201": ["沖縄県", "那覇市"]}


def reply_json(obj):
    return {"json": obj}


def enrich_case():
    t = TOKYO
    rows = [
        photo(1, lat=t[0], lon=t[1], loc_source="exif"),
        photo(2, lat=SAPPORO[0], lon=SAPPORO[1], loc_source="exif"),
        photo(3, lat=NAHA[0], lon=NAHA[1], loc_source="exif"),
        photo(4, lat=35.2, lon=135.3, loc_source="exif"),                # 両方失敗
        photo(5, lat=t[0] + 0.0001, lon=t[1] + 0.0001, loc_source="exif"),   # 1 と同じキー
        photo(6, lat=36.1, lon=136.1, loc_source="exif"),                # 未知の自治体コード
        photo(7, lat=PARIS[0], lon=PARIS[1], loc_source="exif"),         # 国外
        photo(8, lat=20.0, lon=122.0, loc_source="exif"),                # 矩形の角（国内）
        photo(9, lat=46.5, lon=154.5, loc_source="exif"),                # 矩形の角（国内）
        photo(10, lat=19.999, lon=130.0, loc_source="exif"),             # 矩形の外（国外）
        photo(11, lat=37.1, lon=137.1, loc_source="exif"),               # 先頭に 0 のある自治体コード
        photo(12, lat=37.2, lon=137.2, loc_source="exif"),               # 数字でない自治体コード
        photo(13, lat=37.3, lon=137.3, loc_source="exif"),               # 先にキャッシュにある
        photo(14, lat=37.4, lon=137.4, loc_source="exif"),               # 逆ジオが JSON でない、標高は成功
        photo(15, lat=37.5, lon=137.5, loc_source="exif"),               # 逆ジオが null
        photo(16, lat=37.6, lon=137.6, loc_source="exif"),               # results が配列
        photo(17, lat=35.0625, lon=139.0625, loc_source="exif"),         # .3f の丸め（ちょうど中間）
        photo(18, lat=35.1875, lon=139.1875, loc_source="exif"),
        photo(19, lat=-0.0004, lon=0.0004, loc_source="exif"),           # 負のゼロ（国外）
        photo(20, lat=37.7, lon=137.7, loc_source="exif"),               # 標高が整数・偶数丸め
        photo(21, lat=37.8, lon=137.8, loc_source="exif"),               # 標高 13.5 → 14
        photo(22, lat=37.9, lon=137.9, loc_source="exif"),               # 標高 -0.5 → 0
        photo(23, lat=38.0, lon=138.0, loc_source="exif"),               # 標高が文字列の数字
        photo(24, lat=t[0], lon=t[1], loc_source="inferred", inferred_from=1, infer_dt_s=60),
        photo(25, lat=SYDNEY[0], lon=SYDNEY[1], loc_source="manual", place="設定済み"),   # 対象外
        photo(26, loc_source=None),                                       # 位置なし: 対象外
    ]
    rgeo = [
        (t, reply_json({"results": {"muniCd": "13101", "lv01Nm": "テスト町一丁目"}})),
        (SAPPORO, reply_json({"results": {"muniCd": "01100", "lv01Nm": "テスト町二丁目"}})),
        (NAHA, reply_json({})),
        ((35.2, 135.3), {"status": 404}),
        ((36.1, 136.1), reply_json({"results": {"muniCd": "99999", "lv01Nm": "未知"}})),
        ((20.0, 122.0), reply_json({"results": {"muniCd": 47201, "lv01Nm": "角の町"}})),
        ((46.5, 154.5), reply_json({"results": {"lv01Nm": "北の角"}})),
        ((37.1, 137.1), reply_json({"results": {"muniCd": "00013101"}})),
        ((37.2, 137.2), reply_json({"results": {"muniCd": "ab", "lv01Nm": "x"}})),
        ((37.3, 137.3), {"status": 404}),
        ((37.4, 137.4), {"text": "<html>not json</html>"}),
        ((37.5, 137.5), reply_json(None)),
        ((37.6, 137.6), reply_json({"results": ["a"]})),
        ((35.0625, 139.0625), reply_json({"results": {"lv01Nm": "丸め1"}})),
        ((35.1875, 139.1875), reply_json({"results": {"lv01Nm": "丸め2"}})),
        ((37.7, 137.7), reply_json({})),
        ((37.8, 137.8), reply_json({})),
        ((37.9, 137.9), reply_json({})),
        ((38.0, 138.0), reply_json({})),
    ]
    elev = [
        (t, reply_json({"elevation": 12.5, "hsrc": "5m"})),
        (SAPPORO, reply_json({"elevation": 13.5})),
        (NAHA, reply_json({"elevation": "-----", "hsrc": "-----"})),
        ((35.2, 135.3), {"status": 404}),
        ((36.1, 136.1), reply_json({"elevation": 2.5})),
        ((20.0, 122.0), reply_json({"elevation": 7})),
        ((46.5, 154.5), reply_json({"elevation": 1234.4})),
        ((37.1, 137.1), reply_json({"elevation": 0.4})),
        ((37.2, 137.2), reply_json({"elevation": None})),
        ((37.3, 137.3), reply_json({"elevation": 5.0})),
        ((37.4, 137.4), reply_json({"elevation": 8.6})),
        ((37.5, 137.5), reply_json({"elevation": -3.5})),
        ((37.6, 137.6), {"status": 404}),
        ((35.0625, 139.0625), {"status": 404}),
        ((35.1875, 139.1875), {"text": "oops"}),
        ((37.7, 137.7), reply_json({"elevation": 100})),
        ((37.8, 137.8), reply_json({"elevation": 13.5})),
        ((37.9, 137.9), reply_json({"elevation": -0.5})),
        ((38.0, 138.0), reply_json({"elevation": "12"})),
    ]
    pre = {"geo:37.300,137.300": {"place": "キャッシュの町", "elev": 3, "muni_cd": "13101",
                                  "pref": "東京都", "city": "千代田区"}}
    return {"name": "enrich", "env": {}, "muni": MUNI,
            "gsi": {"rgeo": [{"lat": c[0], "lon": c[1], "reply": r} for c, r in rgeo],
                    "elev": [{"lat": c[0], "lon": c[1], "reply": r} for c, r in elev]},
            "steps": [
                {"name": "first", "sql": rows, "cache": pre, "run": ["enrich"]},
                # 両方失敗した行は、キャッシュも DB も変えず、次のサイクルで問い合わせ直す
                {"name": "second", "sql": [], "run": ["enrich"]},
                # 位置を直して NULL に戻した行は、次のサイクルで再 enrich される
                {"name": "reset_place", "sql": [
                    "UPDATE photos SET place=NULL WHERE id IN (1,7)"], "run": ["enrich"]},
            ]}


def collision_case():
    """国内外の境界をまたいで丸めたキー（`.3f`）が衝突する行（緯度 20.0 の境目）。現行は `id` 順に 1 行ずつ処理し、
    先に来た行がキャッシュを作る。国外の行は、国内の行が作ったキャッシュを空で上書きする。"""
    rows = [
        photo(1, lat=20.0001, lon=130.0, loc_source="exif"),     # 国内: 問い合わせる
        photo(2, lat=19.9999, lon=130.0, loc_source="exif"),     # 国外: 同じキーを空で上書きする
        photo(3, lat=20.0002, lon=130.0, loc_source="exif"),     # 国内: 空のキャッシュに当たる（問い合わせない）
        photo(4, lat=19.9999, lon=131.0, loc_source="exif"),     # 国外が先
        photo(5, lat=20.0001, lon=131.0, loc_source="exif"),     # 国内: 国外の空のキャッシュに当たる
        photo(6, lat=20.0001, lon=132.0, loc_source="exif"),     # 国内: 先にキャッシュにある
        photo(7, lat=19.9999, lon=132.0, loc_source="exif"),     # 国外: それを空で上書きする
        photo(8, lat=20.0002, lon=132.0, loc_source="exif"),     # 国内: 空のキャッシュに当たる
        photo(9, lat=20.0001, lon=133.0, loc_source="exif"),     # 国内: 問い合わせる
        photo(10, lat=20.0002, lon=133.0, loc_source="exif"),    # 国内: 9 のキャッシュに当たる
    ]
    pre = {"geo:20.000,132.000": {"place": "キャッシュの町", "elev": 3, "muni_cd": "13101",
                                  "pref": "東京都", "city": "千代田区"}}
    rgeo = [((20.0001, 130.0), reply_json({"results": {"muniCd": "13101", "lv01Nm": "衝突の町"}})),
            ((20.0001, 133.0), reply_json({"results": {"muniCd": "01100", "lv01Nm": "ふつうの町"}}))]
    elev = [((20.0001, 130.0), reply_json({"elevation": 5.5})),
            ((20.0001, 133.0), reply_json({"elevation": 6.5}))]
    return {"name": "enrich_key_collision", "env": {}, "muni": MUNI,
            "gsi": {"rgeo": [{"lat": c[0], "lon": c[1], "reply": r} for c, r in rgeo],
                    "elev": [{"lat": c[0], "lon": c[1], "reply": r} for c, r in elev]},
            "steps": [
                {"name": "first", "sql": rows, "cache": pre, "run": ["enrich"]},
                # 再 enrich の印を戻しても、キャッシュ（国外の空）が残るので、問い合わせずに空で確定する
                {"name": "reset_place", "sql": [
                    "UPDATE photos SET place=NULL WHERE id IN (1,3,5,6,8,9,10)"], "run": ["enrich"]},
            ]}


def limit_case():
    rows = [photo(i, lat=48.0 + i / 100, lon=2.0, loc_source="exif") for i in range(1, 46)]
    return {"name": "enrich_limit", "env": {}, "muni": MUNI, "gsi": {"rgeo": [], "elev": []},
            "steps": [{"name": "first", "sql": rows, "run": ["enrich"]},
                      {"name": "second", "sql": [], "run": ["enrich"]}]}


def both_case():
    """infer の直後に enrich を流す（同じサイクルの順序: 推定で付いた位置にも地名が付く）。"""
    rows = [photo(1, "2024-05-06 10:00", *TOKYO, "exif", place="済み"),
            photo(2, "2024-05-06 10:05"),
            photo(3, "2024-05-06 23:59")]
    return {"name": "infer_then_enrich", "env": {}, "muni": MUNI,
            "gsi": {"rgeo": [{"lat": TOKYO[0], "lon": TOKYO[1],
                              "reply": reply_json({"results": {"muniCd": "13101", "lv01Nm": "テスト町"}})}],
                    "elev": [{"lat": TOKYO[0], "lon": TOKYO[1], "reply": reply_json({"elevation": 4.4})}]},
            "steps": [{"name": "cycle", "sql": rows, "run": ["infer", "enrich"]},
                      {"name": "cycle_again", "sql": [], "run": ["infer", "enrich"]}]}


MUNI_JS_SAMPLES = [
    # 実物に近い形（合成）
    "GSI.MUNI_ARRAY[\"13101\"] = '13,東京都,13101,千代田区';\n"
    "GSI.MUNI_ARRAY[\"01100\"] = '1,北海道,01100,札幌市　';\n"
    "GSI.MUNI_ARRAY[\"47201\"] = '47,沖縄県,47201,那覇市';\n",
    # 市区町村名にカンマ、全角空白が途中にも、同じコードが重なる、先頭に 0 が並ぶ、コードが 0
    "x = 'use strict'; y = '1,a'; z = '2,B,00012,C,D'; w = '3,E,12,　F　G　'; v = '4,H,0012,I'; u = '5,J,000,K';",
    # 都道府県名に引用符・改行、閉じない引用符、空の名前、数字でない
    "'1,it\"s,2,x'\n'3,a\nb,4,c' 'q,r,5,s' '6,,7,t' '8,u,v,w' '9,x,10,unterminated",
    # 何も一致しない
    "no entries here 'a' 'b,c'",
]

FLOAT_KEY_SAMPLES = [
    0.0625, 0.1875, 0.3125, 35.0625, 35.1875, 139.0625, -0.0004, 0.0004, -0.0006, 0.0005, 1.0005,
    35.6895, 139.6917, 20.0, 46.5, -33.856784, 151.215297, 1e-7, 123456.7895, -1.2345, 2.5, 1.0e15,
]


TIME_HAND = [
    "2024-05-06 10:05", "2024-05- 6 10:05", "2024-05- 6 10:05:07", "2024-05-6 10:05", "2024-5-6 1:5", "2024- 5-06 10:05",
    "2024-05- 06 10:05", "2024-05- 16 10:05", "2024-05-16 10:05", "2024-05- 0 10:05", "2024-05-00 10:05",
    "2024-05-  6 10:05", "2024-05-0 6 10:05", "2024-05- 6\t10:05", "2024-05- 6\x1c10:05", "2024-05-06\x1f10:05",
    "2024-05-06\u00a010:05", "2024-05-06\u202f10:05", "2024-05-06\u3000 10:05", "2024-05-06\u200b10:05",
    "2024-05-06\u180e10:05", "2024-05-06\x0b10:05", "2024-05-06\n10:05", "2024-05-06 10: 5", "2024-05-06 10:05 ",
    "2024-05-06 10:05:", "2024-05-06 10:05:60", "2024-05-06 10:05:61", "2024-05-06 10:05:5", "2024-05-06 10:05:05x",
    "2024-02-29 10:05", "2023-02-29 10:05", "2024-02- 9 10:05", "2024-04-31 10:05", "2024-04- 31 10:05",
    "2024-12-31 23:59:59", "0001-01-01 00:00", "0000-01-01 00:00", "9999-12-31 23:59", "2024-05-32 10:05",
    "2024-05-3 10:05", "2024-05-30 10:05", "2024-05-31 10:05", " 2024-05-06 10:05", "", "abc",
]


def time_samples():
    rng = random.Random(55)
    months = ["5", "05", "12", "13", "0", "00", " 5", "1", "01"]
    days = ["6", "06", " 6", " 06", "16", " 16", "31", "32", "0", "00", " 0", "3 ", "9", " 9", "30", "29", "28"]
    seps = [" ", "  ", "\t", "\x1c", "\u00a0", "\u200b", "", " \t", "\x1f ", "\u3000"]
    hours = ["7", "07", "24", "10", " 7", "23", "00", "0", "1"]
    minutes = ["5", "05", "59", "60", " 5", "00", "0", "10"]
    tails = ["", ":09", ":60", ":5", " ", ":", ":07x", ":61"]
    out = list(TIME_HAND)
    for _ in range(600):
        out.append(f"2024-{rng.choice(months)}-{rng.choice(days)}{rng.choice(seps)}"
                   f"{rng.choice(hours)}:{rng.choice(minutes)}{rng.choice(tails)}")
    seen, uniq = set(), []
    for t in out:
        if t not in seen:
            seen.add(t)
            uniq.append(t)
    return uniq


def python_times(samples):
    from datetime import datetime
    epoch = datetime(1970, 1, 1)
    out = []
    for text in samples:
        row = {"text": text}
        for name, fmt in (("minute", "%Y-%m-%d %H:%M"), ("second", "%Y-%m-%d %H:%M:%S")):
            try:
                row[name] = (datetime.strptime(text, fmt) - epoch).total_seconds()
            except ValueError:
                row[name] = None
        out.append(row)
    return out


def build():
    cases = infer_cases() + [enrich_case(), collision_case(), limit_case(), both_case()]
    rng = random.Random(54)
    samples = list(FLOAT_KEY_SAMPLES) + [round(rng.uniform(-180, 180), 7) for _ in range(40)]
    return {"cases": cases, "muni_js": [{"text": t} for t in MUNI_JS_SAMPLES],
            "float_keys": [{"x": x} for x in samples],
            "time_samples": [{"text": t} for t in time_samples()]}


# ---- Python 本体を流す（子プロセス: GEOFRAME_* は import 時に読まれるため） --------------------------

def worker():
    """標準入力の 1 ケースを Python の geo.py に流し、結果を標準出力へ JSON で出す"""
    case = json.load(sys.stdin)
    real_stdout, sys.stdout = sys.stdout, sys.stderr     # Python 本体の print は結果に混ぜない
    sys.path.insert(0, str(APP))
    import urllib.error
    from geoframe import geo, store
    from geoframe.store import cache_put, db, dbq, dbx

    requests = []
    replies = {}
    for kind in ("rgeo", "elev"):
        for entry in case.get("gsi", {}).get(kind, []):
            replies[(kind, entry["lat"], entry["lon"])] = entry["reply"]

    def fake_http_json(url):
        from urllib.parse import parse_qs, urlparse
        parsed = urlparse(url)
        q = parse_qs(parsed.query)
        lat, lon = float(q["lat"][0]), float(q["lon"][0])
        kind = "rgeo" if "LonLatToAddress" in parsed.path else "elev"
        requests.append([kind, lat, lon])
        reply = replies.get((kind, lat, lon), {"json": {}})
        if "status" in reply:
            raise urllib.error.HTTPError(url, reply["status"], "x", {}, None)
        if "text" in reply:
            return json.loads(reply["text"])      # JSON でなければ例外
        return reply["json"]

    geo.http_json = fake_http_json
    geo.time.sleep = lambda s: None
    db()
    if case.get("muni"):
        cache_put("muni_table", case["muni"])

    out = []
    for step in case["steps"]:
        for key, value in (step.get("cache") or {}).items():
            cache_put(key, value)
        for sql in step["sql"]:
            dbx(sql)
        del requests[:]
        n = None
        for what in step["run"]:
            if what == "infer":
                n = geo.infer_locations()
            elif what == "enrich":
                geo.enrich_rows()
        cur = db().execute("SELECT * FROM photos ORDER BY id")
        names = [d[0] for d in cur.description]
        rows = [dict(zip(names, r)) for r in cur.fetchall()]
        geo_cache = {k: v for k, v in store._cache.items() if k.startswith("geo:")}
        out.append({"name": step["name"], "inferred": n, "rows": rows,
                    "cache": geo_cache, "requests": [list(r) for r in requests]})
    json.dump(out, real_stdout, ensure_ascii=False)


def worker_muni():
    real_stdout, sys.stdout = sys.stdout, sys.stderr
    sys.path.insert(0, str(APP))
    import io
    from geoframe import geo, store
    samples = json.load(sys.stdin)
    out = []
    for sample in samples:
        store._cache.pop("muni_table", None)

        class Resp(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        geo.urllib.request.urlopen = lambda req, timeout=0, text=sample["text"]: Resp(text.encode("utf-8"))
        table = geo.muni_table()
        out.append({"text": sample["text"], "table": table})
    json.dump(out, real_stdout, ensure_ascii=False)


def run_child(mode, payload, env_extra):
    env = dict(os.environ)
    for key in [k for k in env if k.startswith("GEOFRAME_")]:
        del env[key]
    data_dir = tempfile.mkdtemp(prefix="geoframe-geo-fixture-")
    env.update({"GEOFRAME_DATA_DIR": data_dir, "TZ": "Asia/Tokyo", "PYTHONHASHSEED": "0"})
    env.update(env_extra)
    result = subprocess.run([sys.executable, str(Path(__file__).resolve()), mode],
                            input=json.dumps(payload), capture_output=True, text=True, env=env)
    if result.returncode != 0:
        sys.exit(f"Python の実行に失敗しました（{mode}）:\n{result.stderr}")
    return json.loads(result.stdout)


def run_python(doc):
    cases = []
    for case in doc["cases"]:
        steps = run_child("--worker", case, case.get("env", {}))
        cases.append({**case, "steps": [{**s, "python": r} for s, r in zip(case["steps"], steps)]})
    muni = run_child("--worker-muni", doc["muni_js"], {})
    floats = [{"x": f["x"], "key": f"{f['x']:.3f}"} for f in doc["float_keys"]]
    times = python_times([t["text"] for t in doc["time_samples"]])
    return {"cases": cases, "muni_js": muni, "float_keys": floats, "time_samples": times}


def render(doc):
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def main():
    if "--worker" in sys.argv[1:]:
        worker()
        return 0
    if "--worker-muni" in sys.argv[1:]:
        worker_muni()
        return 0
    check = "--check" in sys.argv[1:]
    if check:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_geo_fixture.py で作ってください", file=sys.stderr)
            return 1
        stored = json.loads(FIXTURE.read_text(encoding="utf-8"))
        # 保存済みのシナリオ（入力）を流し直す。Python の結果の欄は入力に含めない
        doc = {"cases": [{**c, "steps": [{k: v for k, v in s.items() if k != "python"} for s in c["steps"]]}
                         for c in stored["cases"]],
               "muni_js": [{"text": m["text"]} for m in stored["muni_js"]],
               "float_keys": [{"x": f["x"]} for f in stored["float_keys"]],
               "time_samples": [{"text": t["text"]} for t in stored["time_samples"]]}
    else:
        doc = build()
    result = run_python(doc)
    text = render(result)
    if check:
        if FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE.name} が Python の結果と違います。python3 tools/gen_geo_fixture.py で作り直して"
                  "差分を確認してください", file=sys.stderr)
            return 1
        print(f"{FIXTURE.name}: 一致（{len(result['cases'])}ケース）")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"{FIXTURE} を書きました（{len(result['cases'])}ケース、muni.js {len(result['muni_js'])}件）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
