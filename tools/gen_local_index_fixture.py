#!/usr/bin/env python3
"""ローカル写真の索引（`index_local`）の契約テスト用フィクスチャ（Pythonの`drive.index_local`が基準）。

- `local-index-python.json`: 合成の写真の木（バイト列は合成JPEG・PNG・TIFF・HEIC。実写真ではない）を一時の
  `DATA_DIR/photos/` に置き、Pythonの`index_local`を空のDBに通した結果の`photos`の全行（`id`を含む全列）。
  何段階かの操作（追加・更新・削除・手動の位置の書き込み）を順に流し、各段階のあとの行を記録する。
  RustのテストがPythonと同じ木を作って同じ操作を流し、列ごとに比べる（`tests/local_index_python.rs`）。
  `indexed_at`は実行時刻なので値を記録せず、**その段階で書き換えられたか**（前に入れた目印の値が残ったか）
  だけを`touched`に記録する。

    python3 tools/gen_local_index_fixture.py          # 書き出す（PNG・TIFFの本体はPillowが作るので、版が変わると変わる）
    python3 tools/gen_local_index_fixture.py --check  # 保存済みのバイト列と操作をPythonに通し直し、行が一致するか検査する

`--check`は写真を作り直さない（Pillowの版でエンコード結果が変わってもCIが落ちないように、保存済みの
バイト列を入力にして、Pythonの索引の結果が保存済みの期待値と変わっていないことだけを見る）。
HEICを索引するには`pillow-heif`が要る（`app/requirements.txt`）。無いPythonで流すと`.heic`の行が欠けて、
気づけるよう失敗にする。出力先は crates/geoframe/tests/fixtures/ の下。
"""

import base64
import io
import json
import os
import sqlite3
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURES = REPO / "crates" / "geoframe" / "tests" / "fixtures"
FIXTURE = FIXTURES / "local-index-python.json"
EXIF_FIXTURE = FIXTURES / "exif-python.json"

SENTINEL = "2000-01-01T00:00:00"
S = 1_000_000_000  # 1秒（ns）
T0 = 1_700_000_000 * S


def exif_jpeg(name: str) -> bytes:
    doc = json.loads(EXIF_FIXTURE.read_text(encoding="utf-8"))
    for case in doc["cases"]:
        if case["name"] == name:
            return base64.b64decode(case["jpeg_base64"])
    raise KeyError(name)


def pillow_bytes(fmt: str) -> bytes:
    from PIL import Image
    buf = io.BytesIO()
    Image.new("RGB", (12, 8), (200, 30, 30)).save(buf, fmt)
    return buf.getvalue()


def op_write(path: str, data: bytes, mtime_ns: int) -> dict:
    return {"op": "write", "path": path, "b64": base64.b64encode(data).decode("ascii"),
            "mtime_ns": mtime_ns}


def build_steps() -> list:
    typical = exif_jpeg("typical")
    # 写真の木。mtimeはナノ秒まで違える（浮動小数への変換を突き合わせる）
    first = [
        op_write("photos/a_typical.jpg", typical, T0 + 500_000_000),
        op_write("photos/b_south_west.JPEG", exif_jpeg("south_west"), T0 + 1 * S + 123_456_789),
        op_write("photos/c_no_exif.png", pillow_bytes("PNG"), T0 + 2 * S + 999_999_999),
        op_write("photos/d_timed_only.jpg", exif_jpeg("time_digitized_only"), T0 + 3 * S),
        op_write("photos/sub/e_zero_date.jpg", exif_jpeg("time_zero_date"), T0 + 4 * S + 1),
        op_write("photos/sub/deeper/f_typical_le.jpg", exif_jpeg("typical_little_endian"), T0 + 5 * S),
        # 走査順: 文字列では `a-b.jpg` が `a/b.jpg` より前だが、パスの要素の列では逆
        op_write("photos/a-b.jpg", typical, T0 + 6 * S),
        op_write("photos/a/b.jpg", typical, T0 + 7 * S),
        # 除外: パスの要素に `gdrive`
        op_write("photos/gdrive/g.jpg", typical, T0 + 8 * S),
        op_write("photos/x/gdrive/deep/h.jpg", typical, T0 + 8 * S),
        # 要素の完全一致ではないので対象
        op_write("photos/gdrive2/i.jpg", typical, T0 + 9 * S),
        # 対象外の拡張子・画像として読めない
        op_write("photos/notes.txt", b"text", T0 + 10 * S),
        op_write("photos/broken.jpg", b"not an image", T0 + 11 * S),
        op_write("photos/i.heic", (FIXTURES / "synthetic_o1.heic").read_bytes(), T0 + 12 * S + 7),
        op_write("photos/j.tif", pillow_bytes("TIFF"), T0 + 13 * S),
        op_write("photos/.hidden/k.jpg", typical, T0 + 14 * S),
        op_write("photos/日本語/l.jpg", typical, T0 + 15 * S),
        op_write("photos/m_no_exif.jpg", exif_jpeg("no_exif"), T0 + 16 * S),
        # 壊れたGPS: Pythonは例外で索引しない。Rustは位置なしで索引する（意図した差。server.md §0）
        op_write("photos/n_two_components.jpg", exif_jpeg("two_components"), T0 + 17 * S),
    ]
    changes = [
        # 内容と時刻が変わる（位置が消える）
        op_write("photos/a_typical.jpg", exif_jpeg("no_exif"), T0 + 100 * S),
        # 0.5秒のずれ: 据え置き
        {"op": "utime", "path": "photos/b_south_west.JPEG", "mtime_ns": T0 + 1 * S + 123_456_789 + 500_000_000},
        # 内容は同じで時刻だけ5秒進む: 更新される
        {"op": "utime", "path": "photos/d_timed_only.jpg", "mtime_ns": T0 + 3 * S + 5 * S},
        # 消える（derivativeを持つ行は、そのファイルも消える）
        {"op": "mkfile", "path": "derivatives/z.jpg"},
        {"op": "sql", "sql": "UPDATE photos SET display='derivatives/z.jpg' WHERE path='photos/sub/e_zero_date.jpg'"},
        {"op": "delete", "path": "photos/sub/e_zero_date.jpg"},
        # 原本を display にしている行が消えても、`photos/` の原本は消さない（別ファイルの例）
        {"op": "sql", "sql": "UPDATE photos SET display='photos/a/b.jpg' WHERE path='photos/a-b.jpg'"},
        {"op": "delete", "path": "photos/a-b.jpg"},
        # 手動で位置・お気に入りなどを入れた行の mtime が変わると、位置は上書きされ、他は残る
        {"op": "sql", "sql": "UPDATE photos SET lat=1.5, lon=2.5, loc_source='manual', place='p', "
                             "favorite=1, hidden=1, rotation=90, title='t', fetched_at='2024-01-01T00:00:00', "
                             "display='photos/c_no_exif.png' WHERE path='photos/c_no_exif.png'"},
        {"op": "utime", "path": "photos/c_no_exif.png", "mtime_ns": T0 + 2 * S + 999_999_999 + 10 * S},
        # 追加
        op_write("photos/new1.jpg", typical, T0 + 200 * S),
        op_write("photos/zz/new2.png", pillow_bytes("PNG"), T0 + 201 * S + 5),
        # gdrive の行（Python が以前に作ったもの）は、走査に出ないので削除される
        {"op": "sql", "sql": "INSERT INTO photos(source, path, name, mtime) VALUES('local','photos/gdrive/old.jpg','old.jpg',1.0)"},
    ]
    return [
        {"name": "initial", "ops": first},
        {"name": "unchanged", "ops": []},
        {"name": "changes", "ops": changes},
        {"name": "unchanged_again", "ops": []},
    ]


# ---- Python本体を流す ------------------------------------------------------------

def apply_op(root: Path, op: dict, db_exec) -> None:
    kind = op["op"]
    if kind == "write":
        p = root / op["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(base64.b64decode(op["b64"]))
        os.utime(p, ns=(op["mtime_ns"], op["mtime_ns"]))
    elif kind == "utime":
        os.utime(root / op["path"], ns=(op["mtime_ns"], op["mtime_ns"]))
    elif kind == "delete":
        (root / op["path"]).unlink()
    elif kind == "mkfile":
        p = root / op["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(b"derivative")
    elif kind == "sql":
        db_exec(op["sql"])
    else:
        raise ValueError(kind)


def columns_rows(conn: sqlite3.Connection) -> list:
    cur = conn.execute("SELECT * FROM photos ORDER BY id")
    names = [d[0] for d in cur.description]
    return [dict(zip(names, row)) for row in cur.fetchall()]


def normalize(rows: list) -> list:
    """`indexed_at` は値を捨て、書き換えられたか（`touched`）だけを残す"""
    out = []
    for row in rows:
        row = dict(row)
        at = row.pop("indexed_at")
        row["touched"] = at != SENTINEL
        out.append(row)
    return out


def run_python(steps: list) -> list:
    """各段階の操作をPythonの`index_local`に流し、段階ごとの結果（行・ファイルの有無）を返す"""
    data_dir = tempfile.mkdtemp(prefix="geoframe-local-index-")
    os.environ["GEOFRAME_DATA_DIR"] = data_dir
    sys.path.insert(0, str(APP))
    from geoframe import drive
    from geoframe.config import EXTS
    from geoframe.store import db, dbx
    if ".heic" not in EXTS:
        sys.exit("pillow-heif が無い Python では .heic を索引しないので、このフィクスチャは作れません")
    root = Path(data_dir)
    results = []
    for step in steps:
        dbx("UPDATE photos SET indexed_at=?", (SENTINEL,))
        for op in step["ops"]:
            apply_op(root, op, lambda sql: dbx(sql))
        # 操作で目印が消えた行（追加）も、`touched` は目印の有無で決まる
        dbx("UPDATE photos SET indexed_at=? WHERE indexed_at IS NULL", (SENTINEL,))
        with open(os.devnull, "w") as quiet:
            real, sys.stdout = sys.stdout, quiet
            try:
                drive.index_local()
            finally:
                sys.stdout = real
        exists = {p: (root / p).exists() for p in sorted({
            op["path"] for s in steps for op in s["ops"] if op["op"] in ("mkfile", "write", "delete")
        })}
        results.append({"rows": normalize(columns_rows(db())), "exists": exists})
    return results


def render(doc: dict) -> str:
    return json.dumps(doc, indent=1, ensure_ascii=False) + "\n"


def main() -> int:
    check = "--check" in sys.argv[1:]
    if check:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_local_index_fixture.py で作ってください", file=sys.stderr)
            return 1
        stored = json.loads(FIXTURE.read_text(encoding="utf-8"))
        steps = [{"name": s["name"], "ops": s["ops"]} for s in stored["steps"]]
    else:
        steps = build_steps()
    results = run_python(steps)
    doc = {"steps": [{"name": s["name"], "ops": s["ops"], "python": r} for s, r in zip(steps, results)]}
    text = render(doc)
    if check:
        if FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE.name} が Python の結果と違います。python3 tools/gen_local_index_fixture.py で作り直して"
                  "差分を確認してください", file=sys.stderr)
            return 1
        print(f"{FIXTURE.name}: 一致（{len(steps)}段階）")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"{FIXTURE} を書きました（{len(steps)}段階、最後の行数 {len(results[-1]['rows'])}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
