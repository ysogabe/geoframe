#!/usr/bin/env python3
"""EOI（JPEG の終わりのマーカー）の欠けや切れ方の契約テスト用フィクスチャ。基準は Python の`make_derivative`。

- `jpeg-eoi-python.json`: 合成の JPEG（64x48 のノイズ。実写真ではない）を切る・EOI を壊す・前後にゴミを足すなどして
  作った入力と、それを Python の`util.make_derivative`（Pillow）に通した結果（`ok`か`error`）。
  `cut_points`は同じ JPEG を SOS の手前から末尾の手前まで**全ての切断点**で切ったときに、Python が読めた
  長さの一覧。RustのテストがEOIの扱い（`docs/rust-migration/server.md` §0・§12.8）を、Pythonの実際の結果と
  突き合わせる。Rust が意図して違える点（Python が読むが Rust は失敗にする、その逆）は`tests/jpeg_eoi_python.rs`に列挙。

    python3 tools/gen_jpeg_eoi_fixture.py          # 書き出す（JPEG の本体は Pillow が作るので、版が変わると変わる）
    python3 tools/gen_jpeg_eoi_fixture.py --check  # 保存済みの入力を Python に通し直し、結果が一致するか検査する

出力先は crates/geoframe/tests/fixtures/ の下。
"""

import base64
import io
import json
import os
import random
import struct
import sys
import tempfile
import warnings
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "jpeg-eoi-python.json"


def base_jpeg() -> bytes:
    from PIL import Image, ImageFile
    ImageFile.MAXBLOCK = 1 << 24
    rng = random.Random(54)
    im = Image.new("RGB", (64, 48))
    px = im.load()
    for y in range(48):
        for x in range(64):
            px[x, y] = (rng.randrange(256), rng.randrange(256), rng.randrange(256))
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=85, subsampling=2)
    return buf.getvalue()


def cases(j: bytes) -> list:
    assert j.endswith(b"\xff\xd9")
    app0_end = 4 + struct.unpack(">H", j[4:6])[0]  # SOI の後ろの最初のセグメント（JFIF）の終わり
    body = j[:-2]  # EOI を取ったもの
    return [
        ("complete", j),
        ("trailing_garbage_after_eoi", j + b"\0" * 64 + b"trailer"),
        ("garbage_between_segments_after_app0", j[:app0_end] + b"\0\0\0\0" + j[app0_end:]),
        ("garbage_right_after_soi", j[:2] + b"\0\0\0\0" + j[2:]),
        # EOI だけが無く、ファイルがデータの直後で終わる
        ("no_eoi_file_ends", body),
        ("no_eoi_then_single_ff", body + b"\xff"),
        ("no_eoi_then_ff00", body + b"\xff\x00"),
        # EOI が無く、その位置がほかのバイトになっている
        ("eoi_replaced_by_zeros", body + b"\0\0"),
        ("no_eoi_then_zero_padding", body + b"\0" * 100),
        ("no_eoi_then_garbage", body + b"trailer"),
    ]


def python_outcome(data: bytes, tmp: Path) -> str:
    from geoframe.util import make_derivative
    src = tmp / "case.jpg"
    dest = tmp / "out.jpg"
    src.write_bytes(data)
    if dest.exists():
        dest.unlink()
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            make_derivative(src, dest, 0)
    except Exception:  # noqa: BLE001 - 期待値の記録
        return "error"
    return "ok"


def fill(doc: dict) -> dict:
    sys.path.insert(0, str(APP))
    with (tempfile.TemporaryDirectory(prefix="geoframe-eoi-") as data_dir,
          tempfile.TemporaryDirectory(prefix="geoframe-eoi-case-") as t):
        # config.py は import 時に環境変数を読む。置き場を作らせないよう一時の場所を指す（終わったら消す）
        os.environ["GEOFRAME_DATA_DIR"] = data_dir
        tmp = Path(t)
        for case in doc["cases"]:
            case["python"] = python_outcome(base64.b64decode(case["jpeg_base64"]), tmp)
        sweep = doc["sweep"]
        j = base64.b64decode(sweep["jpeg_base64"])
        sos = j.index(b"\xff\xda")
        sweep["python_ok_cuts"] = [n for n in range(sos, len(j)) if python_outcome(j[:n], tmp) == "ok"]
    return doc


def build() -> dict:
    j = base_jpeg()
    return {
        "cases": [{"name": n, "jpeg_base64": base64.b64encode(d).decode()} for n, d in cases(j)],
        "sweep": {"jpeg_base64": base64.b64encode(j).decode()},
    }


def render(doc: dict) -> str:
    return json.dumps(doc, indent=1) + "\n"


def main() -> int:
    if "--check" in sys.argv[1:]:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_jpeg_eoi_fixture.py で生成してください", file=sys.stderr)
            return 1
        saved = json.loads(FIXTURE.read_text(encoding="utf-8"))
        again = fill(json.loads(FIXTURE.read_text(encoding="utf-8")))
        if render(saved) != render(again):
            print(f"{FIXTURE} の期待値がPythonの結果と食い違います（PillowのJPEGの読み方が変わった）。"
                  " 内容を確かめて python3 tools/gen_jpeg_eoi_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("JPEGのEOIのフィクスチャ: OK")
        return 0
    FIXTURE.write_text(render(fill(build())), encoding="utf-8")
    print(f"書き出しました: {FIXTURE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
