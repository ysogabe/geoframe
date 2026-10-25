#!/usr/bin/env python3
"""旅行ブログの保存と書き出し（E15・E16）の契約テスト用フィクスチャを再生成する。

- `blog-write-python.json`: 合成の旅程・写真・画像と、現行 Python の `POST /api/blog/save`・`export` の出力
  （状態コード・応答・保存された下書き、書き出しのファイルの全文と画像の寸法・EXIF・ICC）。DB は読み取りAPIの
  フィクスチャと同じ合成DB（`admin-api-catalog.sql`）

    python3 tools/gen_blog_write_fixture.py          # 書き出す
    python3 tools/gen_blog_write_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う（app/tests/blog_write_fixture.py）。リポジトリにある実際の旅程JSONは使わない。
出力先は crates/geoframe/tests/fixtures/ の下。Rust のテストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.blog_write_fixture import FIXTURE_PATH, build_cases, render  # noqa: E402


def main() -> int:
    outputs = [(FIXTURE_PATH, render(build_cases()))]
    if "--check" in sys.argv[1:]:
        stale = [path for path, text in outputs
                 if not path.exists() or path.read_text(encoding="utf-8") != text]
        for path in stale:
            print(f"{path} が最新ではありません。"
                  " python3 tools/gen_blog_write_fixture.py で再生成してください", file=sys.stderr)
        if stale:
            return 1
        print("旅行ブログ保存・書き出しのフィクスチャ: OK")
        return 0
    for path, text in outputs:
        path.write_text(text, encoding="utf-8")
        print(f"書き出しました: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
