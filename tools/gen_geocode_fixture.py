#!/usr/bin/env python3
"""地名検索（`GET /api/admin/geocode`）の契約テスト用フィクスチャを再生成する。

- `geocode-python.json`: 現行の `handlers.Handler` に GET を順に流し、地理院・Nominatim への通信と時計を合成の
  応答・偽の時計に差し替えた結果（状態コード・応答のJSON・上流への要求・1秒1件の待ち・メモリキャッシュ）。
  要求は `app/tests/geocode_fixture.py` の `CASES`（順序つき）。あわせて、`float()`・`urllib.parse.quote`・
  `str.isspace` の出力（Rust の小さな関数の突き合わせ用）と、`/api/admin/` 配下で Python が 403 にする GET を残す。

    python3 tools/gen_geocode_fixture.py          # 書き出す
    python3 tools/gen_geocode_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う。出力先は crates/geoframe/tests/fixtures/ の下。Rust のテストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.geocode_fixture import FIXTURE_PATH, build_cases, render  # noqa: E402


def main() -> int:
    text = render(build_cases())
    if "--check" in sys.argv[1:]:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE_PATH} が最新ではありません。"
                  " python3 tools/gen_geocode_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("地名検索のフィクスチャ: OK")
        return 0
    FIXTURE_PATH.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
