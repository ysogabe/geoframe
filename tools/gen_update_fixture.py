#!/usr/bin/env python3
"""更新API（setloc・setflag）と POST の入口（CSRF対策・ルーティング・エラー応答）の契約テスト用フィクスチャを
再生成する。

- `update-api-python.json`: 現行の `handlers.Handler` に実際に POST した結果（状態コード・応答のJSON・
  `photos` の変化）。要求は `app/tests/update_api_fixture.py` の `CASES`（順序つき）。

    python3 tools/gen_update_fixture.py          # 書き出す
    python3 tools/gen_update_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う（admin_api_fixture と同じ合成DB）。出力先は crates/geoframe/tests/fixtures/ の下。
Rust のテストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.update_api_fixture import FIXTURE_PATH, build_cases, render  # noqa: E402


def main() -> int:
    text = render(build_cases())
    if "--check" in sys.argv[1:]:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE_PATH} が最新ではありません。"
                  " python3 tools/gen_update_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("更新APIのフィクスチャ: OK")
        return 0
    FIXTURE_PATH.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
