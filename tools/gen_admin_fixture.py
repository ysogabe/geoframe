#!/usr/bin/env python3
"""管理画面の読み取りAPIの契約テスト用フィクスチャを再生成する。

- `admin-api-python.json`: Pythonの`/api/admin/status`・`/api/admin/photos`の出力（合成DBに対する）
- `admin-api-catalog.sql`: その出力を作った合成DBのSQL（RustのテストがDBを作り直す）

    python3 tools/gen_admin_fixture.py          # 書き出す
    python3 tools/gen_admin_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う（app/tests/admin_api_fixture.py）。出力先は crates/geoframe/tests/fixtures/ の下。
Rust のテストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.admin_api_fixture import (  # noqa: E402
    CATALOG_SQL_PATH, FIXTURE_PATH, build_cases, build_catalog_sql, render)


def main() -> int:
    outputs = [(FIXTURE_PATH, render(build_cases())), (CATALOG_SQL_PATH, build_catalog_sql())]
    if "--check" in sys.argv[1:]:
        stale = [path for path, text in outputs
                 if not path.exists() or path.read_text(encoding="utf-8") != text]
        for path in stale:
            print(f"{path} が最新ではありません。"
                  " python3 tools/gen_admin_fixture.py で再生成してください", file=sys.stderr)
        if stale:
            return 1
        print("管理画面APIのフィクスチャ: OK")
        return 0
    for path, text in outputs:
        path.write_text(text, encoding="utf-8")
        print(f"書き出しました: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
