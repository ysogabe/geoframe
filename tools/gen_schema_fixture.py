#!/usr/bin/env python3
"""Pythonが空のDBから作る`catalog.db`のスキーマを書き出す（RustのスキーマのテストのGolden）。

- `schema-python.json`: `app/geoframe/store.py:db()`を空のデータ置き場に対して実行した結果の
  `sqlite_master`（作成順）と、各表の`PRAGMA table_xinfo`・各表の索引（`index_list`・`index_info`）。
  合成の空DBだけを使う（写真・位置は入らない）。

    python3 tools/gen_schema_fixture.py          # 書き出す
    python3 tools/gen_schema_fixture.py --check  # ファイルと再生成結果の差を検査する

出力先は crates/geoframe/tests/fixtures/ の下。RustのテストがPythonの空DBとRustの空DBの構造を比べる
（`crates/geoframe/src/schema.rs`）。SQLiteの`ALTER TABLE ADD COLUMN`は`sqlite_master.sql`の文字列を
書き換えるので、`photos`の`sql`の文字列は一致しない。比べるのは構造（列の名前・型・NOT NULL・既定値・
主キー・並び、索引の列と一意性）。それ以外の表・索引は`sql`の文字列も一致する。
"""

import json
import os
import sqlite3
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "schema-python.json"


def build() -> dict:
    with tempfile.TemporaryDirectory(prefix="geoframe-schema-") as tmp:
        # config.py は import 時に環境変数を読むので、import より前に空の置き場を指す
        os.environ["GEOFRAME_DATA_DIR"] = tmp
        os.environ.pop("GEOFRAME_PHOTOS_DIR", None)
        sys.path.insert(0, str(APP))
        from geoframe import store  # noqa: E402

        store.db()
        path = Path(tmp) / "catalog.db"
        conn = sqlite3.connect(path)
        conn.row_factory = sqlite3.Row
        objects = [
            {"type": r["type"], "name": r["name"], "tbl_name": r["tbl_name"], "sql": r["sql"]}
            for r in conn.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY rowid")
        ]
        tables = {}
        for o in objects:
            if o["type"] != "table":
                continue
            name = o["name"]
            columns = [
                {"cid": r["cid"], "name": r["name"], "type": r["type"], "notnull": r["notnull"],
                 "dflt_value": r["dflt_value"], "pk": r["pk"], "hidden": r["hidden"]}
                for r in conn.execute(f"PRAGMA table_xinfo({name})")
            ]
            indexes = []
            for ix in conn.execute(f"PRAGMA index_list({name})").fetchall():
                cols = [r["name"] for r in conn.execute(f"PRAGMA index_info({ix['name']})")]
                indexes.append({"name": ix["name"], "unique": ix["unique"], "origin": ix["origin"],
                                "partial": ix["partial"], "columns": cols})
            indexes.sort(key=lambda i: i["name"])
            tables[name] = {"columns": columns, "indexes": indexes}
        conn.close()
        store._db.close()
        return {"objects": objects, "tables": tables}


def render() -> str:
    return json.dumps(build(), ensure_ascii=False, indent=1, sort_keys=False) + "\n"


def main() -> int:
    text = render()
    if "--check" in sys.argv[1:]:
        if not FIXTURE.exists() or FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE} が最新ではありません。"
                  " python3 tools/gen_schema_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("スキーマのフィクスチャ: OK")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
