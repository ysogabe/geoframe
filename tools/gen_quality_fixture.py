#!/usr/bin/env python3
"""簡易品質（`quality_cycle`・`quality_backfill`・グルーピング）の契約テスト用フィクスチャ
（Pythonの`geoframe/quality.py`が基準）。

- `quality-python.json`: 合成の`photos`の行と**合成の特徴量**（SHA・pHash・指標。画像は使わない）を入力に、
  Pythonの`quality_cycle`・`quality_backfill`を段階的に流した結果。画像からの特徴量の算出（`analyze_path`）だけを
  差し替えるので、孤児の削除・未解析の選び方と件数制限・無効化（`display`/`md5`/`mtime`）・「全員分そろうまで
  再構築しない」・`photo_quality_dirty`・グルーピング（完全重複・3分/pHash距離12の境界・推奨・`kind`・順位・
  並び）・`photo_quality`の全差し替えが、Pillowの版に依らずに突き合わせられる。各段階のあとの
  `photo_quality_features`・`photo_quality`・`settings`・戻り値を記録する（時刻の列は比べない）。
  RustのテストがPythonと同じ行・同じ特徴量で`quality`の段を流して、列ごとに比べる
  （`crates/geoframe/src/quality/tests.rs`）。
- `switchbacks`: **RustからPythonへ切り戻したとき**（P5のロールバック）の試験。①Pythonが特徴量と`photo_quality`
  を作る→②新しい写真（同じSHAの写し）をRustが解析して再構築する（Rustの再構築は「同じSHAの指標を`id`最小の
  行にそろえる」以外はPythonと同じなので、Pythonの`build_quality_rows`に同じそろえ方を足して流す。Rustの単体
  テストが、Rustの実際の結果とこの結果が全列で一致することを確かめる）→③Pythonで変化なしのサイクルを回す
  （`photo_quality`を保つ。`algorithm`は見ない）→④`photo_quality_dirty=1`にしてPythonで再構築する（Rustが作った
  特徴量も入力に含めて、Pythonの算法でグルーピングし直す）→⑤Rustが解析した特徴量の行を消してPythonに解析し直させる
  （同じバイト列のPythonの値になるので、Pythonだけで作った場合と同じ結果に戻る）。各段階の状態と、②と④・②と⑤の
  `photo_quality`の差を記録する。
- 特徴量の算出そのもの（pHash・鮮明度など）は算法が違ってよい（旧値との一致は不要）ので、ここには入れない。
  Rustの単体テストが決定性（同じ入力で同じ値）を見る。

    python3 tools/gen_quality_fixture.py          # 書き出す
    python3 tools/gen_quality_fixture.py --check  # 保存済みのシナリオをPythonに通し直し、結果が一致するか検査する

意図した差（エラー行の文面、同じSHAの指標をそろえる処理など）は、`cases`には入れず、`switchbacks`とRustの単体テスト
で確かめる（`docs/rust-migration/server.md` §0・§16）。出力先は crates/geoframe/tests/fixtures/ の下。
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
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "quality-python.json"


# ---- シナリオ ---------------------------------------------------------------------------------

def lit(v):
    if v is None:
        return "NULL"
    if isinstance(v, str):
        return "'" + v.replace("'", "''") + "'"
    return repr(v)


def disp(pid):
    return f"derivatives/q{pid}.jpg"


def photo_sql(pid, taken_at="2024-05-06 10:00", display="auto", md5="auto", mtime="auto", lat=None):
    cols = {"id": pid, "source": "drive", "name": f"p{pid}", "taken_at": taken_at,
            "display": disp(pid) if display == "auto" else display,
            "md5": f"m{pid}" if md5 == "auto" else md5,
            "mtime": float(pid) + 0.5 if mtime == "auto" else mtime, "lat": lat}
    return ("INSERT INTO photos(" + ",".join(cols) + ") VALUES("
            + ",".join(lit(v) for v in cols.values()) + ")")


BASE = 0x9E3779B97F4A7C15  # 適当な 64 ビット（pHash の見本）


def flip(value, n, start=0):
    """value の `start` ビット目から n ビットを反転する（ハミング距離 n）"""
    for i in range(n):
        value ^= 1 << ((start + i) % 64)
    return value


def feat(sha, phash, sharp=5.0, lum=120.0, exp=90.0, black=0.01, white=0.01, contrast=120.0):
    return {"sha256": sha, "phash": f"{phash:016x}",
            "metrics": {"sharpness": sharp, "luminance": lum, "exposure_score": exp,
                        "black_clip": black, "white_clip": white, "contrast": contrast}}


def err(text="boom"):
    return {"error": text}


def case(name, photos, feats, steps):
    """photos: INSERT 文の一覧、feats: display -> 特徴量（またはエラー）、steps: 各段階"""
    return {"name": name, "photos": photos, "feats": feats, "steps": steps}


def step(name, run, sql=(), feats=None):
    return {"name": name, "run": run, "sql": list(sql), "feats": feats or {}}


def cycle(limit=100):
    return {"kind": "cycle", "limit": limit}


def backfill(batch=100):
    return {"kind": "backfill", "batch": batch}


def fixed_cases():
    cases = []

    # 1. 完全重複
    p = [photo_sql(1), photo_sql(2, "2024-05-06 11:00"), photo_sql(3, "2024-05-06 12:00")]
    f = {disp(1): feat("a" * 64, BASE), disp(2): feat("a" * 64, BASE),
         disp(3): feat("b" * 64, flip(BASE, 30))}
    cases.append(case("exact_pair", p, f, [step("cycle", cycle())]))

    # 2. 3 分の境界（ちょうど 3 分は含む）と pHash の距離 12/13。それぞれ別の撮影日にする
    p, f = [], {}
    pid = 0

    def add(taken_at, ph, sha=None, **m):
        nonlocal pid
        pid += 1
        p.append(photo_sql(pid, taken_at))
        f[disp(pid)] = feat(sha or f"{pid:064x}", ph, **m)
        return pid

    add("2024-05-01 10:00", BASE)
    add("2024-05-01 10:03", BASE)                       # 3:00 ちょうど → 類似
    add("2024-05-02 10:00", BASE)
    add("2024-05-02 10:03:01", BASE)                    # 3:01 → 類似にならない
    add("2024-05-03 10:00:00", BASE)
    add("2024-05-03 10:02:59", BASE)                    # 2:59 → 類似
    add("2024-05-04 10:00", BASE)
    add("2024-05-04 10:01", flip(BASE, 12))             # 距離 12 → 類似
    add("2024-05-05 10:00", BASE)
    add("2024-05-05 10:01", flip(BASE, 13))             # 距離 13 → 類似にならない
    add("2024-05-07 10:00:30", BASE)
    add("2024-05-07 10:03:30", BASE)                    # 秒つき、ちょうど 3:00
    cases.append(case("boundaries", p, f, [step("cycle", cycle())]))

    # 3. 連鎖（A-B と B-C が類似なら A-B-C が 1 グループ。A-C は 6 分離れていても）
    p, f, pid = [], {}, 0
    add("2024-05-01 10:00", BASE)
    add("2024-05-01 10:03", flip(BASE, 6))
    add("2024-05-01 10:06", flip(BASE, 12))
    add("2024-05-01 10:09", flip(BASE, 18, 8))          # B から距離 24 以上 → 外れる
    cases.append(case("chain", p, f, [step("cycle", cycle())]))

    # 4. 類似グループの中に完全重複が混ざる、3 枚以上の推奨（上位 2 枚）、信頼度、同点は id 順
    p, f, pid = [], {}, 0
    add("2024-06-01 09:00", BASE, sha="s1" * 32, sharp=6.0)
    add("2024-06-01 09:00", BASE, sha="s1" * 32, sharp=6.0)       # 1 と同じ SHA
    add("2024-06-01 09:01", flip(BASE, 3), sha="s2" * 32, sharp=5.0)
    add("2024-06-01 09:01", flip(BASE, 4), sha="s3" * 32, sharp=4.0)
    add("2024-06-01 09:02", flip(BASE, 2), sha="s4" * 32, sharp=5.0)  # s2 と同じ指標 → 同点は id 順
    add("2024-06-01 09:02", flip(BASE, 2), sha="s4" * 32, sharp=5.0)  # s4 の重複
    cases.append(case("similar_with_duplicates", p, f, [step("cycle", cycle())]))

    # 5. 2 枚だけ（信頼度 = 1 位と 2 位の差ではなく、2 枚なら 1 位 - 2 位）、全指標が同じ（正規化 0.5）
    p, f, pid = [], {}, 0
    add("2024-06-02 09:00", BASE, sha="c1" * 32)
    add("2024-06-02 09:00", flip(BASE, 5), sha="c2" * 32)
    add("2024-06-02 15:00", BASE, sha="c3" * 32, sharp=9.5, exp=40.0, black=0.2, white=0.1, contrast=30.0)
    add("2024-06-02 15:01", flip(BASE, 1), sha="c4" * 32, sharp=2.5, exp=80.0, black=0.0, white=0.0, contrast=200.0)
    cases.append(case("two_photos", p, f, [step("cycle", cycle())]))

    # 6. 指標の組み合わせ（理由・注意の文言）
    p, f, pid = [], {}, 0
    add("2024-06-03 09:00", BASE, sha="d1" * 32, sharp=8.0, exp=95.0, black=0.0, white=0.0, contrast=150.0)
    add("2024-06-03 09:01", BASE, sha="d2" * 32, sharp=3.0, exp=30.0, black=0.2, white=0.12, contrast=20.0)
    add("2024-06-03 09:02", BASE, sha="d3" * 32, sharp=7.9, exp=60.0, black=0.01, white=0.0, contrast=60.0)
    add("2024-06-03 09:03", BASE, sha="d4" * 32, sharp=5.0, exp=76.0, black=0.014, white=0.0, contrast=44.9)
    cases.append(case("reasons_and_issues", p, f, [step("cycle", cycle())]))

    # 7. 撮影時刻が読めない写真は時刻では結びつかない（SHA が同じなら完全重複になる）
    p, f, pid = [], {}, 0
    for t in [None, "", "garbage", "2024-02-30 10:00", "2024-05-06 25:00", "2024-05-06 10:00:61",
              "2024-5-6 10:5", "2024-05-06 10:05 "]:
        pid += 1
        p.append(photo_sql(pid, t))
        f[disp(pid)] = feat("e0" * 32 if pid <= 2 else f"{pid:064x}", BASE)
    pid += 1
    p.append(photo_sql(pid, "2024-05-06 10:06"))        # 桁の足りない時刻（7 番目）と 1 分差 → 類似
    f[disp(pid)] = feat(f"{pid:064x}", BASE)
    cases.append(case("unparsable_times", p, f, [step("cycle", cycle())]))

    # 8. 段階的に: 件数制限で未解析が残る間は再構築しない（前回の photo_quality を保つ）
    p = [photo_sql(i, f"2024-07-01 10:0{i}") for i in range(1, 6)]
    f = {disp(i): feat(f"{i:064x}", flip(BASE, i)) for i in range(1, 6)}
    cases.append(case("limited_cycles", p, f,
                      [step("c1", cycle(2)), step("c2", cycle(2)), step("c3", cycle(2)),
                       step("c4", cycle(2))]))

    # 9. 一括解析（backfill）の批量
    cases.append(case("backfill", p, f, [step("b", backfill(2))]))

    # 10. エラー行: 以後は未解析にならず、グルーピングに入らない。画像が変わると（md5 が変わると）再解析
    p = [photo_sql(1), photo_sql(2), photo_sql(3)]
    f = {disp(1): feat("a" * 64, BASE), disp(2): err(), disp(3): feat("a" * 64, BASE)}
    cases.append(case("error_rows", p, f, [
        step("c1", cycle()),
        step("again", cycle()),
        step("fixed", cycle(), sql=["UPDATE photos SET md5='changed' WHERE id=2"],
             feats={disp(2): feat("a" * 64, BASE)}),
    ]))

    # 11. 孤児: 写真が消えた・display が空/NULL になった → 特徴量を消して再構築
    p = [photo_sql(i, "2024-08-01 10:00") for i in range(1, 5)]
    f = {disp(i): feat("a" * 64, BASE) for i in range(1, 5)}
    cases.append(case("orphans", p, f, [
        step("c1", cycle()),
        step("delete_one", cycle(), sql=["DELETE FROM photos WHERE id=1"]),
        step("blank_display", cycle(), sql=["UPDATE photos SET display='' WHERE id=2"]),
        step("null_display", cycle(), sql=["UPDATE photos SET display=NULL WHERE id=3"]),
        step("idle", cycle()),
    ]))

    # 12. 無効化: md5・mtime・display が変わると再解析（IS 比較で NULL どうしは一致）
    p = [photo_sql(1, md5=None, mtime=None), photo_sql(2), photo_sql(3)]
    f = {disp(1): feat("a" * 64, BASE), disp(2): feat("b" * 64, BASE), disp(3): feat("c" * 64, BASE)}
    cases.append(case("invalidation", p, f, [
        step("c1", cycle()),
        step("idle", cycle()),
        step("md5_null_to_value", cycle(), sql=["UPDATE photos SET md5='x' WHERE id=1"]),
        step("mtime_changed", cycle(), sql=["UPDATE photos SET mtime=99.25 WHERE id=2"]),
        step("display_renamed", cycle(), sql=["UPDATE photos SET display='derivatives/q3.r90.jpg' WHERE id=3"],
             feats={"derivatives/q3.r90.jpg": feat("d" * 64, flip(BASE, 40))}),
        step("idle2", cycle()),
    ]))

    # 13. 位置の無い写真も解析する。display の無い写真は解析しない
    p = [photo_sql(1, lat=None), photo_sql(2, lat=35.0), photo_sql(3, display=None)]
    f = {disp(1): feat("a" * 64, BASE), disp(2): feat("a" * 64, BASE)}
    cases.append(case("no_location_no_display", p, f, [step("c1", cycle())]))

    # 14. グループの並び（exact 先 → 大きい順 → 信頼度の高い順 → 先頭の撮影時刻）
    p, f, pid = [], {}, 0
    ph = {}
    for k in range(6):
        ph[k] = flip(BASE, 20 * (k + 1), 3 * k)         # グループごとに十分離す
    # グループ 0: 類似 2 枚（時刻 B）
    add("2024-09-02 10:00", ph[0], sha="g01" + "0" * 61, sharp=6.0)
    add("2024-09-02 10:01", ph[0], sha="g02" + "0" * 61, sharp=2.0)
    # グループ 1: 類似 3 枚（時刻 A: 一番早い）
    add("2024-09-01 10:00", ph[1], sha="g11" + "0" * 61, sharp=6.0)
    add("2024-09-01 10:01", ph[1], sha="g12" + "0" * 61, sharp=5.0)
    add("2024-09-01 10:02", ph[1], sha="g13" + "0" * 61, sharp=1.0)
    # グループ 2: exact 2 枚
    add("2024-09-03 10:00", ph[2], sha="g2" + "0" * 62)
    add("2024-09-03 10:00", ph[2], sha="g2" + "0" * 62)
    # グループ 3: 類似 2 枚（信頼度が 0 に近い、時刻 A より早い）
    add("2024-08-31 10:00", ph[3], sha="g31" + "0" * 61, sharp=5.0)
    add("2024-08-31 10:01", ph[3], sha="g32" + "0" * 61, sharp=5.0)
    # グループ 4: exact 3 枚
    for _ in range(3):
        add("2024-09-05 10:00", ph[4], sha="g4" + "0" * 62)
    # 単独
    add("2024-09-06 10:00", ph[5], sha="g5" + "0" * 62)
    cases.append(case("group_order", p, f, [step("cycle", cycle())]))

    # 18. 同じ SHA の 2 枚の pHash が違っても（算法の違う行が混ざった形）、類似の結びつきは各行の pHash で決める。
    #     A・B は同じ SHA（距離 2）、時刻差 10 分。C は B の 1 分後で、距離は B–C=12・A–C=14。B–C が類似なので
    #     3 枚が 1 つの `similar`（B の pHash を A にそろえると B–C=14 でつながらず、`exact` の A・B だけになる）
    p, f, pid = [], {}, 0
    b_hash = flip(BASE, 2)
    add("2024-05-06 10:00", BASE, sha="aa" * 32)
    add("2024-05-06 10:10", b_hash, sha="aa" * 32)
    add("2024-05-06 10:11", flip(b_hash, 12, 2), sha="bb" * 32)
    # 近くに別の写真が無い同じ SHA の 2 枚は、pHash が違っても `exact` のまま
    add("2024-05-07 10:00", BASE, sha="cc" * 32)
    add("2024-05-07 10:30", flip(BASE, 40, 3), sha="cc" * 32)
    cases.append(case("same_sha_phash_differs", p, f, [step("cycle", cycle())]))

    # 15. dirty の印が無く未解析も無いときは再構築しない（既存の photo_quality をそのまま保つ）
    p = [photo_sql(1), photo_sql(2)]
    f = {disp(1): feat("a" * 64, BASE), disp(2): feat("a" * 64, BASE)}
    pre = ["INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,phash,"
           "sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at) VALUES"
           f"(1,'{disp(1)}','m1',1.5,'{'a' * 64}','{BASE:016x}',5.0,120.0,90.0,0.01,0.01,120.0,'','t'),"
           f"(2,'{disp(2)}','m2',2.5,'{'a' * 64}','{BASE:016x}',5.0,120.0,90.0,0.01,0.01,120.0,'','t')",
           "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,"
           "group_kind,rank_in_group,recommended,score,reasons_json,issues_json,algorithm,analyzed_at) VALUES"
           f"(2,'{disp(2)}','m2',2.5,'{'a' * 64}',7,'exact',1,1,12.5,'[]','[]','old','t')"]
    cases.append(case("not_dirty", p, f, [
        step("idle", cycle(), sql=pre),
        step("set_dirty", cycle(), sql=["INSERT INTO settings(key,value) VALUES('photo_quality_dirty','1')"]),
    ]))

    # 17. 更新で未解析が残る間は、前回の photo_quality を保つ（途中結果で表示候補が揺れない）
    p = [photo_sql(i, "2024-11-01 10:00") for i in range(1, 5)]
    f = {disp(i): feat("a" * 64, BASE) for i in range(1, 5)}
    cases.append(case("keeps_previous_until_ready", p, f, [
        step("build", cycle()),
        step("stale_two", cycle(1), sql=["UPDATE photos SET md5='n1' WHERE id IN (1,2)"],
             feats={disp(1): feat("b" * 64, BASE), disp(2): feat("c" * 64, flip(BASE, 40))}),
        step("ready", cycle(1)),
    ]))

    # 16. 以前の photo_quality の行が消える（グループが無くなる）
    p = [photo_sql(1, "2024-10-01 10:00"), photo_sql(2, "2024-10-01 10:00")]
    f = {disp(1): feat("a" * 64, BASE), disp(2): feat("b" * 64, BASE)}
    cases.append(case("regroup", p, f, [
        step("c1", cycle()),
        step("split", cycle(), sql=["UPDATE photos SET md5='n' WHERE id=2"],
             feats={disp(2): feat("b" * 64, flip(BASE, 40))}),
        step("remerge", cycle(), sql=["UPDATE photos SET md5='o' WHERE id=2"],
             feats={disp(2): feat("b" * 64, BASE)}),
    ]))
    return cases


def random_case(seed, count):
    rng = random.Random(seed)
    p, f = [], {}
    clusters = [(f"2024-0{rng.randrange(1, 9)}-{rng.randrange(10, 28)}", rng.getrandbits(64))
                for _ in range(8)]
    by_sha = {}
    for pid in range(1, count + 1):
        day, base = rng.choice(clusters)
        sec = rng.randrange(0, 12 * 60) * (1 if rng.random() < 0.5 else 1)
        taken_at = f"{day} {10 + sec // 3600:02d}:{sec // 60 % 60:02d}"
        if rng.random() < 0.3:
            taken_at += f":{rng.randrange(60):02d}"
        if rng.random() < 0.05:
            taken_at = rng.choice(["", "bad"])
        p.append(photo_sql(pid, taken_at))
        # 同じ SHA（同じバイト列）の特徴量は同じ（現実のPythonでもそう。算法の違う行が混ざる Rust だけの
        # 場合は、Rust の単体テストが見る）
        if by_sha and rng.random() < 0.25:
            sha = rng.choice(sorted(by_sha))
            f[disp(pid)] = by_sha[sha]
            continue
        sha = f"{rng.getrandbits(256):064x}"
        ph = flip(base, rng.randrange(0, 16), rng.randrange(64))
        by_sha[sha] = f[disp(pid)] = feat(
            sha, ph, sharp=round(rng.uniform(0.5, 9.0), 3), lum=round(rng.uniform(20, 230), 3),
            exp=round(rng.uniform(5, 100), 3), black=round(rng.uniform(0, 0.25), 4),
            white=round(rng.uniform(0, 0.2), 4), contrast=float(rng.randrange(10, 220)))
    return case(f"random_{seed}", p, f, [step("cycle", cycle(count // 2)), step("rest", cycle(count))])


def switchback_cases():
    """Rust で解析・再構築した DB を Python へ切り戻す試験の入力。

    `python_feats`: Python が作った特徴量（先に 1 サイクルで `photo_quality` まで作る）。`rust_photos`・
    `rust_feats`: そのあとで Rust が解析する新しい写真（古い写真と同じバイト列の写しなので SHA は同じで、
    算法が違うので pHash と指標が少し違う。Rust の鮮明度は Python より 0.4%ほど高い）。
    `python_reanalyzed_feats`: 同じ写真（`rust_photo_ids`）を Python が解析したときの特徴量（同じバイト列なので、
    古い写真の Python の値と同じ）。
    """
    cases = []

    # 1. 同じバイト列の写しが 1 枚増えた（完全重複）。Python の行 A（id 1）と Rust の行 B（id 2）は指標が
    #    わずかに違う。Rust は B の指標を A にそろえるので A が推奨（B は配信から消える）。Python が再構築すると、
    #    B の指標の方が高いので B が推奨になり、表に出る写しが入れ替わる
    p = [photo_sql(1, "2024-05-06 10:00"), photo_sql(3, "2024-05-07 09:00")]
    f = {disp(1): feat("a" * 64, BASE, sharp=5.0), disp(3): feat("c" * 64, flip(BASE, 30), sharp=5.0)}
    rp = [photo_sql(2, "2024-05-06 10:00")]
    rf = {disp(2): feat("a" * 64, flip(BASE, 2), sharp=5.02, exp=90.2)}
    cases.append({"name": "exact_copy_added", "photos": p, "python_feats": f,
                  "rust_photos": rp, "rust_feats": rf, "rust_photo_ids": [2],
                  "python_reanalyzed_feats": {disp(2): feat("a" * 64, BASE, sharp=5.0)}})

    # 2. 同じ SHA の写し B（id 2）の pHash が A とも C とも違い、B だけが C と結びつく形（A–B=2・B–C=12・A–C=14）。
    #    どちらの実装も 3 枚が `similar`（pHash は各行のものを使う）。指標の差で順位・推奨だけが変わりうる
    b_hash = flip(BASE, 2)
    p = [photo_sql(1, "2024-05-06 10:00"), photo_sql(3, "2024-05-06 10:11")]
    f = {disp(1): feat("a" * 64, BASE, sharp=5.0), disp(3): feat("b" * 64, flip(b_hash, 12, 2), sharp=4.0)}
    rp = [photo_sql(2, "2024-05-06 10:10")]
    rf = {disp(2): feat("a" * 64, b_hash, sharp=5.3, exp=91.0)}
    cases.append({"name": "bridge_by_copy", "photos": p, "python_feats": f,
                  "rust_photos": rp, "rust_feats": rf, "rust_photo_ids": [2],
                  "python_reanalyzed_feats": {disp(2): feat("a" * 64, BASE, sharp=5.0)}})

    # 3. 同じバイト列で指標が同じ（Rust の算法でも同じ値になった）写しが増えるだけ: 切り戻しても何も変わらない
    p = [photo_sql(1, "2024-05-06 10:00")]
    f = {disp(1): feat("a" * 64, BASE, sharp=5.0)}
    rp = [photo_sql(2, "2024-05-06 10:00")]
    rf = {disp(2): feat("a" * 64, BASE, sharp=5.0)}
    cases.append({"name": "identical_copy_added", "photos": p, "python_feats": f,
                  "rust_photos": rp, "rust_feats": rf, "rust_photo_ids": [2],
                  "python_reanalyzed_feats": {disp(2): feat("a" * 64, BASE, sharp=5.0)}})
    return cases


def build():
    cases = fixed_cases() + [random_case(54, 60), random_case(7, 90), random_case(2026, 40)]
    return {"cases": cases, "switchbacks": switchback_cases()}


# ---- Python 本体を流す（子プロセス: GEOFRAME_DATA_DIR は import 時に読まれるため） ------------------

def _setup_python(case_feats):
    """Python の quality.py を、画像の解析だけ合成の特徴量に差し替えて読み込む（DB も作る）"""
    real_stdout, sys.stdout = sys.stdout, sys.stderr     # Python 本体の print は結果に混ぜない
    sys.path.insert(0, str(APP))
    from geoframe import quality, store
    from geoframe.store import db

    def fake_analyze(path):
        key = str(Path(path).relative_to(quality.DATA_DIR.resolve()))
        spec = case_feats[key]
        if "error" in spec:
            raise RuntimeError(spec["error"])
        m = spec["metrics"]
        return spec["sha256"], int(spec["phash"], 16), quality.Metrics(
            m["sharpness"], m["luminance"], m["exposure_score"], m["black_clip"], m["white_clip"],
            m["contrast"])

    quality.analyze_path = fake_analyze
    db()
    return real_stdout, quality, store


def _snapshot(name, result, algorithms=False):
    """各段階のあとの特徴量・photo_quality・dirty の印（時刻の列は含めない）"""
    from geoframe import store
    from geoframe.store import db

    conn = db()
    cur = conn.execute("""SELECT photo_id,display,source_md5,source_mtime,content_sha256,phash,
                                 sharpness,luminance,exposure_score,black_clip,white_clip,contrast,
                                 error!='' AS failed
                          FROM photo_quality_features ORDER BY photo_id""")
    names = [d[0] for d in cur.description]
    features = [dict(zip(names, r)) for r in cur.fetchall()]
    cur = conn.execute("""SELECT photo_id,display,source_md5,source_mtime,content_sha256,group_id,
                                 group_kind,rank_in_group,recommended,score,reasons_json,issues_json
                          FROM photo_quality ORDER BY photo_id""")
    names = [d[0] for d in cur.description]
    quality_rows = [dict(zip(names, r)) for r in cur.fetchall()]
    out = {"name": name, "result": result, "features": features, "quality": quality_rows,
           "dirty": store.setting_get("photo_quality_dirty")}
    if algorithms:
        out["algorithms"] = [r[0] for r in conn.execute(
            "SELECT DISTINCT algorithm FROM photo_quality ORDER BY algorithm")]
    return out


def worker():
    """標準入力の 1 ケースを Python の quality.py に流し、各段階のあとの状態を JSON で出す"""
    case = json.load(sys.stdin)
    if "python_feats" in case:
        return worker_switchback(case)
    feats = dict(case["feats"])
    real_stdout, quality, _store = _setup_python(feats)
    from geoframe.store import dbx

    for sql in case["photos"]:
        dbx(sql)

    out = []
    for st in case["steps"]:
        feats.update(st["feats"])
        for sql in st["sql"]:
            dbx(sql)
        run = st["run"]
        if run["kind"] == "cycle":
            result = quality.quality_cycle(run["limit"])
        else:
            result = quality.quality_backfill(run["batch"])
        out.append(_snapshot(st["name"], result))
    json.dump(out, real_stdout, ensure_ascii=False)


RUST_ALGORITHM = "rust-phash-v1:t3:h12"      # crates/geoframe/src/quality.rs の ALGORITHM


def worker_switchback(case):
    """Rust で解析・再構築した DB を Python へ切り戻したときの流れ（モジュールの説明を参照）"""
    feats = dict(case["python_feats"])
    real_stdout, quality, store = _setup_python(feats)
    from geoframe.store import dbx

    for sql in case["photos"]:
        dbx(sql)
    out = {"python_built": _snapshot("python_built", quality.quality_cycle(1000), True)}

    # Rust の再構築: Python の build_quality_rows に、Rust の「同じ SHA の指標を id 最小の行にそろえる」
    # （group.rs の unify_same_content_metrics。pHash はそろえない）を足したもの
    original = quality.build_quality_rows

    def rust_like(photos, analyzed_at):
        first = {}
        for photo in photos:
            first.setdefault(photo.sha256, photo)
        for photo in photos:
            photo.metrics = first[photo.sha256].metrics
        return original(photos, analyzed_at)

    feats.update(case["rust_feats"])
    for sql in case["rust_photos"]:
        dbx(sql)
    quality.build_quality_rows = rust_like
    result = quality.quality_cycle(1000)
    quality.build_quality_rows = original
    dbx("UPDATE photo_quality SET algorithm=?", (RUST_ALGORITHM,))
    out["rust_emulated"] = _snapshot("rust_emulated", result, True)

    # Python へ切り戻す: 変化なしのサイクル→dirty にして再構築
    out["python_idle"] = _snapshot("python_idle", quality.quality_cycle(1000), True)
    store.setting_set("photo_quality_dirty", "1")
    out["python_rebuild"] = _snapshot("python_rebuild", quality.quality_cycle(1000), True)

    # Rust が解析した特徴量の行を消すと、Python が解析し直して（同じバイト列の Python の値）再構築する
    feats.update(case["python_reanalyzed_feats"])
    marks = ",".join("?" * len(case["rust_photo_ids"]))
    dbx(f"DELETE FROM photo_quality_features WHERE photo_id IN ({marks})", tuple(case["rust_photo_ids"]))
    out["python_reanalyze"] = _snapshot("python_reanalyze", quality.quality_cycle(1000), True)
    json.dump(out, real_stdout, ensure_ascii=False)


def run_child(payload):
    env = dict(os.environ)
    for key in [k for k in env if k.startswith("GEOFRAME_")]:
        del env[key]
    with tempfile.TemporaryDirectory(prefix="geoframe-quality-fixture-") as data_dir:
        env.update({"GEOFRAME_DATA_DIR": data_dir, "TZ": "Asia/Tokyo", "PYTHONHASHSEED": "0"})
        result = subprocess.run([sys.executable, str(Path(__file__).resolve()), "--worker"],
                                input=json.dumps(payload), capture_output=True, text=True, env=env)
        if result.returncode != 0:
            sys.exit(f"Python の実行に失敗しました（{payload['name']}）:\n{result.stderr}")
        return json.loads(result.stdout)


QUALITY_COLUMNS = ["display", "source_md5", "source_mtime", "content_sha256", "group_id", "group_kind",
                   "rank_in_group", "recommended", "score", "reasons_json", "issues_json"]


def quality_diff(before, after):
    """2 つの photo_quality の差: 変わった（増えた・消えた）行と、変わった列"""
    old = {r["photo_id"]: r for r in before}
    new = {r["photo_id"]: r for r in after}
    diff = []
    for pid in sorted(old.keys() | new.keys()):
        if pid not in new:
            diff.append({"photo_id": pid, "change": "removed"})
        elif pid not in old:
            diff.append({"photo_id": pid, "change": "added"})
        else:
            columns = [c for c in QUALITY_COLUMNS if old[pid][c] != new[pid][c]]
            if columns:
                diff.append({"photo_id": pid, "change": "changed", "columns": columns})
    return diff


def run_python(doc):
    cases = []
    for c in doc["cases"]:
        steps = run_child(c)
        cases.append({**c, "steps": [{**s, "python": r} for s, r in zip(c["steps"], steps)]})
    switchbacks = []
    for c in doc["switchbacks"]:
        out = run_child(c)
        out["diff"] = quality_diff(out["rust_emulated"]["quality"], out["python_rebuild"]["quality"])
        out["diff_reanalyze"] = quality_diff(out["rust_emulated"]["quality"], out["python_reanalyze"]["quality"])
        switchbacks.append({**c, "python": out})
    return {"cases": cases, "switchbacks": switchbacks}


def render(doc):
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def main():
    if "--worker" in sys.argv[1:]:
        worker()
        return 0
    check = "--check" in sys.argv[1:]
    if check:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_quality_fixture.py で作ってください", file=sys.stderr)
            return 1
        stored = json.loads(FIXTURE.read_text(encoding="utf-8"))
        # 保存済みのシナリオ（入力）を流し直す。Python の結果の欄は入力に含めない
        doc = {"cases": [{**c, "steps": [{k: v for k, v in s.items() if k != "python"} for s in c["steps"]]}
                         for c in stored["cases"]],
               "switchbacks": [{k: v for k, v in c.items() if k != "python"} for c in stored["switchbacks"]]}
    else:
        doc = build()
    result = run_python(doc)
    text = render(result)
    if check:
        if FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE.name} が Python の結果と違います。python3 tools/gen_quality_fixture.py で作り直して"
                  "差分を確認してください", file=sys.stderr)
            return 1
        print(f"{FIXTURE.name}: 一致（{len(result['cases'])}ケース・切り戻し{len(result['switchbacks'])}ケース）")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"{FIXTURE} を書きました（{len(result['cases'])}ケース・切り戻し{len(result['switchbacks'])}ケース）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
