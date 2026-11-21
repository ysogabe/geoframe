#!/usr/bin/env python3
"""4.3（明るさ・シャープネス）の試験の期待値と、JPEG の fixture を作る。

Pillow 11.1.0 で計算する（旧 manifest の値の再現に使った版。システムの Python の Pillow は 11.1.0）。
**試験の実行時には Python を使わない**。このスクリプトの出力（expected.json と *.jpg）をコミットしてあり、
Rust の試験（tests/measure_fixtures.rs）が読む。パターンは合成で、ユーザーの写真は使わない。

  python3 -I crates/geoframe-export/tests/fixtures/measure/gen_expected.py

パターンの定義（pattern・pattern_rgb）は、試験側（tests/measure_fixtures.rs）の Rust と同じ整数演算で書いてある。
変えるときは両方を揃えて、このスクリプトを実行し直す。
"""
import io
import json
import os
import sys
import zlib

import PIL
from PIL import Image, ImageFilter, ImageStat

PILLOW_VERSION = "11.1.0"
if PIL.__version__ != PILLOW_VERSION and "--allow-other-version" not in sys.argv:
    sys.exit(f"Pillow {PILLOW_VERSION} が要ります（今は {PIL.__version__}）")

HERE = os.path.dirname(os.path.abspath(__file__))
KERNEL = ImageFilter.Kernel((3, 3), [0, 1, 0, 1, -4, 1, 0, 1, 0], scale=1, offset=128)


def grad(x, y, w, h):
    return min(255, (x * 255 // max(w - 1, 1)) * 3 // 4 + (y * 64) // max(h - 1, 1))


def pattern(kind, w, h):
    """グレー（8 ビット 1 チャンネル）。行の順。"""
    if kind == "flat":
        return bytes([77]) * (w * h)
    if kind == "noise":
        out = bytearray(w * h)
        s = 12345
        for i in range(w * h):
            s = (s * 1103515245 + 12345) & 0x7FFFFFFF
            out[i] = (s >> 16) & 255
        return bytes(out)
    rows = []
    for y in range(h):
        if kind == "gradient":
            rows.append(bytes(grad(x, y, w, h) for x in range(w)))
        elif kind == "stripes":
            rows.append(bytes(255 if (x // 2) % 2 else 0 for x in range(w)))
        elif kind == "checker":
            rows.append(bytes(255 if (x ^ y) & 1 else 0 for x in range(w)))
        elif kind == "mixed":
            if y >= h // 2:
                rows.append(bytes(255 if (x // 3) % 2 else 20 for x in range(w)))
            else:
                rows.append(bytes(grad(x, y, w, h) for x in range(w)))
        else:
            raise ValueError(kind)
    return b"".join(rows)


def pattern_rgb(kind, w, h):
    """RGB（8 ビット 3 チャンネル）。"""
    if kind == "flat":
        return bytes([120, 80, 200]) * (w * h)
    if kind == "noise":
        out = bytearray(w * h * 3)
        s = 777
        for i in range(w * h * 3):
            s = (s * 1103515245 + 12345) & 0x7FFFFFFF
            out[i] = (s >> 16) & 255
        return bytes(out)
    out = bytearray()
    for y in range(h):
        for x in range(w):
            if kind == "gradient":
                out += bytes([grad(x, y, w, h), 255 - (y * 255) // max(h - 1, 1), ((x // 64 + y // 64) * 8) % 256])
            elif kind == "stripes":
                v = 255 if (x // 2) % 2 else 0
                out += bytes([v, 255 - v, ((x // 64 + y // 64) * 8) % 256])
            else:
                raise ValueError(kind)
    return bytes(out)


def pil_stats(gray_img):
    """縮小前のグレーから、旧 manifest と同じ定義の値を返す（小数 2 桁）と、途中の縮小・応答の CRC。"""
    mean = ImageStat.Stat(gray_img).mean[0]
    small = gray_img.copy()
    small.thumbnail((1024, 1024))
    lap = small.filter(KERNEL)
    var = ImageStat.Stat(lap).var[0]
    return {
        "thumb": list(small.size),
        "thumb_crc32": zlib.crc32(small.tobytes()),
        "lap_crc32": zlib.crc32(lap.tobytes()),
        "mean_brightness": f"{round(mean, 2):.2f}",
        "sharpness_laplacian_var": f"{round(var, 2):.2f}",
    }


# 縮小前の生のグレーのバッファ（Rust の自前の縮小が Pillow と画素単位で一致するかを見る）
RAW_CASES = [
    ("flat", 2560, 1707), ("gradient", 2560, 1707), ("gradient", 1707, 2560), ("gradient", 2560, 1920),
    ("gradient", 1920, 2560), ("gradient", 1025, 1024), ("gradient", 1024, 1025), ("gradient", 3000, 2000),
    ("stripes", 2560, 1920), ("checker", 2560, 1707), ("noise", 2560, 1707), ("noise", 1707, 2560),
    ("checker", 2048, 2048), ("mixed", 1100, 100), ("mixed", 100, 1100), ("mixed", 4000, 30),
    ("mixed", 30, 4000), ("mixed", 1024, 1000), ("mixed", 800, 600), ("mixed", 3000, 1),
    ("mixed", 1, 3000), ("mixed", 2, 2), ("mixed", 3, 3), ("noise", 5, 5), ("noise", 17, 9),
    ("noise", 1500, 1000),
]

# JPEG（Pillow が品質 85 で作ったもの。期待値は Pillow がそのファイルをデコードして計算した値。
# Rust のデコーダ（zune-jpeg）との差は許容幅の中に収まるはず）。(名前, モード, パターン, 幅, 高さ, subsampling)
JPEG_CASES = [
    ("flat_color_2560x1707", "RGB", "flat", 2560, 1707, 2),
    ("gradient_color_1300x900", "RGB", "gradient", 1300, 900, 2),
    ("stripes_gray_1100x1030", "L", "stripes", 1100, 1030, 0),
    ("stripes_color_1050x1100", "RGB", "stripes", 1050, 1100, 2),
    ("mixed_gray_640x480", "L", "mixed", 640, 480, 0),
    ("noise_color_300x200", "RGB", "noise", 300, 200, 2),
    ("noise_gray_300x200", "L", "noise", 300, 200, 0),
]


def main():
    expected = {"pillow": PIL.__version__, "raw": [], "jpeg": []}
    for kind, w, h in RAW_CASES:
        data = pattern(kind, w, h)
        entry = {"kind": kind, "width": w, "height": h}
        entry.update(pil_stats(Image.frombytes("L", (w, h), data)))
        expected["raw"].append(entry)
        print("raw", kind, w, h, file=sys.stderr)
    for name, mode, kind, w, h, sub in JPEG_CASES:
        if mode == "L":
            img = Image.frombytes("L", (w, h), pattern(kind, w, h))
            buf = io.BytesIO()
            img.save(buf, "JPEG", quality=85)
        else:
            img = Image.frombytes("RGB", (w, h), pattern_rgb(kind, w, h))
            buf = io.BytesIO()
            img.save(buf, "JPEG", quality=85, subsampling=sub)
        jpeg = buf.getvalue()
        with open(os.path.join(HERE, name + ".jpg"), "wb") as f:
            f.write(jpeg)
        # 期待値は、保存した JPEG を Pillow でデコードして求める（そのまま convert("L")）
        decoded = Image.open(io.BytesIO(jpeg)).convert("L")
        entry = {"name": name, "color": mode == "RGB", "width": w, "height": h, "bytes": len(jpeg)}
        entry.update(pil_stats(decoded))
        expected["jpeg"].append(entry)
        print("jpeg", name, len(jpeg), file=sys.stderr)
    with open(os.path.join(HERE, "expected.json"), "w") as f:
        json.dump(expected, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main()
