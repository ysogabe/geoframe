#!/usr/bin/env python3
"""derivativeの縮小後の寸法の契約テスト用フィクスチャ（Pillowの`Image.thumbnail`が基準）。

- `thumbnail-python.json`: 元の寸法 `[幅, 高さ]` と、Pythonの`make_derivative`と同じ
  `im.thumbnail((DERIV_PX, DERIV_PX))`の結果の寸法。合成の寸法だけ（写真は使わない）。
  RustのテストがPillowの丸め（縮める側と反対の辺は、縦横比の誤差が小さい方へ切り捨て/切り上げ）と同じか確かめる。

    python3 tools/gen_thumbnail_fixture.py          # 書き出す
    python3 tools/gen_thumbnail_fixture.py --check  # 再生成した結果とファイルの差を検査する
"""

import json
import os
import random
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "thumbnail-python.json"


def sizes() -> list:
    fixed = [
        (4032, 3024), (3024, 4032), (8064, 6048), (6048, 8064), (2560, 2560), (2561, 2561),
        (2560, 1707), (1707, 2560), (2561, 1), (1, 2561), (30000, 1), (1, 30000), (5000, 7),
        (2600, 100), (100, 2600), (2559, 2559), (2560, 2559), (2559, 2560), (3000, 2000),
        (2000, 3000), (4000, 3000), (3264, 2448), (5472, 3648), (4608, 3456), (1920, 1080),
        (7680, 4320), (10000, 10000), (65535, 65535), (65535, 3), (3, 65535),
    ]
    rng = random.Random(54)
    # 縦横比が丸めの境目になりやすい寸法も含める
    rand = [(rng.randint(2561, 9000), rng.randint(1, 9000)) for _ in range(60)]
    rand += [(rng.randint(1, 9000), rng.randint(2561, 9000)) for _ in range(60)]
    small = [(rng.randint(1, 2560), rng.randint(1, 2560)) for _ in range(20)]
    return [list(s) for s in fixed + rand + small]


def thumb(w: int, h: int) -> list:
    sys.path.insert(0, str(APP))
    from PIL import Image
    from geoframe.config import DERIV_PX
    # 1ビット画像なら大きな寸法でも軽い。寸法の計算は画素の種類に依らない
    im = Image.new("1", (w, h))
    im.thumbnail((DERIV_PX, DERIV_PX))
    return list(im.size)


def render() -> str:
    # config.py は import 時に環境変数を読む。置き場を作らせないよう一時の場所を指す（終わったら消す）
    with tempfile.TemporaryDirectory(prefix="geoframe-thumb-") as data_dir:
        os.environ["GEOFRAME_DATA_DIR"] = data_dir
        cases = [{"input": s, "python": thumb(*s)} for s in sizes()]
    return json.dumps({"cases": cases}, indent=1) + "\n"


def main() -> int:
    text = render()
    if "--check" in sys.argv[1:]:
        if not FIXTURE.exists() or FIXTURE.read_text(encoding="utf-8") != text:
            print(f"{FIXTURE} が最新ではありません。"
                  " python3 tools/gen_thumbnail_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("縮小寸法のフィクスチャ: OK")
        return 0
    FIXTURE.write_text(text, encoding="utf-8")
    print(f"書き出しました: {FIXTURE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
