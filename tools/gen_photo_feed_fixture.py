#!/usr/bin/env python3
"""写真フィードの契約テスト用フィクスチャ（Pythonの`api_photos()`の出力）を再生成する。

    python3 tools/gen_photo_feed_fixture.py          # 書き出す
    python3 tools/gen_photo_feed_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う（app/tests/photo_feed_fixture.py）。出力先は
crates/geoframe/tests/fixtures/photo-feed-python.json。Rust の契約テストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.photo_feed_fixture import FIXTURE_PATH, build_items, render  # noqa: E402


def main() -> int:
    text = render(build_items())
    if "--check" in sys.argv[1:]:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE_PATH} が最新ではありません。"
                  " python3 tools/gen_photo_feed_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("写真フィードのフィクスチャ: OK")
        return 0
    FIXTURE_PATH.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
