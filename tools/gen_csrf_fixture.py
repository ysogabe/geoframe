#!/usr/bin/env python3
"""POST の CSRF 対策の判定（Content-Type・Origin・Host）の差分試験用フィクスチャを再生成する。

- `csrf-python.json`: 現行の `handlers.Handler._csrf_ok`・`_host_ok` を、実際のヘッダ解析
  （`http.client.parse_headers`）の上で、前後の空白・Latin-1 の全バイト・Origin の書式（scheme・userinfo・
  ポート・角括弧・IPv6）・Host の組み合わせに流した判定と、実際の HTTP サーバーに生バイトで POST した結果。
  内容と形式は `app/tests/csrf_fixture.py`。Rust のテスト（crates/geoframe/tests/csrf_python.rs）が
  同じ値で判定し、**Python が断るものを Rust が通さない**ことを確かめる。

    python3 tools/gen_csrf_fixture.py          # 書き出す
    python3 tools/gen_csrf_fixture.py --check  # ファイルと再生成結果の差を検査する

合成の値だけを使う。出力先は crates/geoframe/tests/fixtures/ の下。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.csrf_fixture import FIXTURE_PATH, build_cases, render  # noqa: E402


def main() -> int:
    text = render(build_cases())
    if "--check" in sys.argv[1:]:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE_PATH} が最新ではありません。"
                  " python3 tools/gen_csrf_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("CSRF 判定のフィクスチャ: OK")
        return 0
    FIXTURE_PATH.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
