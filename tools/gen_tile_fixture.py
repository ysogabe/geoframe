#!/usr/bin/env python3
"""地図タイルのプロキシ（`GET/HEAD /tiles/…`）とキャッシュの掃除の契約テスト用フィクスチャを再生成する。

- `tile-python.json`: 現行の `handlers.Handler` に GET / HEAD を順に流し、上流（OSM・地理院）をループバックのスタブに、
  時計を偽の時計に差し替えた結果（状態コード・ヘッダ・本文・上流への要求・キャッシュのファイル）と、偽のファイル群に
  `tile_cache_prune` を流した結果。要求は `app/tests/tile_fixture.py` の `CASES`・`PRUNE_CASES`（順序つき）。

    python3 tools/gen_tile_fixture.py          # 書き出す
    python3 tools/gen_tile_fixture.py --check  # ファイルと再生成結果の差を検査する

合成データだけを使う。出力先は crates/geoframe/tests/fixtures/ の下。Rust のテストが読む。
"""

import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1] / "app"
sys.path.insert(0, str(APP))

from tests.tile_fixture import FIXTURE_PATH, build_cases, render  # noqa: E402


def main() -> int:
    text = render(build_cases())
    if "--check" in sys.argv[1:]:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE_PATH} が最新ではありません。"
                  " python3 tools/gen_tile_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("地図タイルのフィクスチャ: OK")
        return 0
    FIXTURE_PATH.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
