#!/usr/bin/env python3
"""P2の受入（T2.7）用の合成写真を作る。比較用PythonとRustの`photos/`に同じものを置いて、取り込みの結果を比べる。

実写真ではない（乱数から作った滑らかな画像）。座標は公開の代表点（駅・城・歌劇場）で、撮影時刻は架空。
ファイル名は連番で、中身を表さない。

    python3 tools/gen_acceptance_photos.py OUT_DIR

`OUT_DIR`の下に写真を作る（空のディレクトリを渡すこと。`gdrive`を含むパスは作らない）。`mtime`は固定の値にして、
同じ木をどこで作っても、`photos.mtime`が同じになるようにする（tarで写すときは`--preserve`／`-p`を付ける）。
作るもの（`kind`は説明用で、ファイルには書かない）:

| ファイル | 内容 |
| --- | --- |
| a01 | GPSあり（東京駅）。撮影 2025-06-01 10:00 |
| a02 | GPSあり（皇居）。撮影 2025-06-01 14:00。長辺3200px（2560pxへ縮む） |
| a03 | GPSあり（京都駅）。撮影 2025-06-02 09:00。EXIFの向き6 |
| n01 | GPSなし。a01の10分後 → 推定（±15分） |
| n02 | GPSなし。a02の1時間30分後 → 推定（±2時間） |
| n03 | GPSなし。a03の8時間後 → 推定（±12時間） |
| n04 | GPSなし。どの合成の錨からも遠い（9日後）。空のDBでは推定されないが、写真の入った既存のDBでは、窓の内側にある既存の行から推定されうる |
| n05 | GPSなし・撮影時刻なし → 推定されない |
| d01, sub/d02 | 同じバイト列（GPSあり、札幌）。完全重複 |
| s01, s02 | GPSあり（名古屋）。同じ画像の2分違い。ほぼ同じ写真 |
| h01.heic | HEIC。GPSあり（大阪城） |
| n06 | GPSなし。h01の5分後 → HEICの錨から推定 |
| f01 | GPSあり（シドニー、国外）。地名は空で確定する |
"""

import io
import os
import random
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageEnhance
import pillow_heif  # noqa: F401  (HEIC を書く)

pillow_heif.register_heif_opener()

BASE_MTIME = 1_750_000_000  # 秒。ファイルごとに +10 秒ずつ


def smooth_image(seed: int, size=(1600, 1200)) -> Image.Image:
    """乱数の粗い格子を滑らかに拡大し、図形を重ねた画像。シードごとに別の写真に見える（pHash が離れる）。"""
    rnd = random.Random(seed)
    small = Image.new("RGB", (8, 6))
    small.putdata([(rnd.randrange(256), rnd.randrange(256), rnd.randrange(256)) for _ in range(48)])
    im = small.resize(size, Image.BICUBIC)
    draw = ImageDraw.Draw(im)
    for _ in range(4):
        x, y = rnd.randrange(size[0] - 200), rnd.randrange(size[1] - 200)
        w, h = rnd.randrange(80, 400), rnd.randrange(80, 400)
        draw.rectangle([x, y, x + w, y + h], fill=(rnd.randrange(256), rnd.randrange(256), rnd.randrange(256)))
    return im


def dms(value: float):
    value = abs(value)
    deg = int(value)
    minutes = int((value - deg) * 60)
    seconds = (value - deg - minutes / 60) * 3600
    return (float(deg), float(minutes), round(seconds, 4))


def exif_of(taken=None, gps=None, orientation=None) -> bytes:
    """taken: 'YYYY-MM-DD HH:MM'、gps: (lat, lon)。"""
    exif = Image.Exif()
    if orientation:
        exif[0x0112] = orientation
    if taken:
        exif.get_ifd(0x8769)[36867] = taken.replace("-", ":") + ":00"
    if gps:
        lat, lon = gps
        ifd = exif.get_ifd(0x8825)
        ifd[1] = "N" if lat >= 0 else "S"
        ifd[2] = dms(lat)
        ifd[3] = "E" if lon >= 0 else "W"
        ifd[4] = dms(lon)
    return exif.tobytes()


def jpeg(im: Image.Image, **kw) -> bytes:
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=90, exif=exif_of(**kw))
    return buf.getvalue()


def heic(im: Image.Image, **kw) -> bytes:
    buf = io.BytesIO()
    im.save(buf, "HEIF", quality=80, exif=exif_of(**kw))
    return buf.getvalue()


TOKYO = (35.681236, 139.767125)       # 東京駅
KOKYO = (35.685175, 139.752799)       # 皇居
KYOTO = (34.985849, 135.758767)       # 京都駅
SAPPORO = (43.061936, 141.356466)     # さっぽろテレビ塔
NAGOYA = (35.170915, 136.881537)      # 名古屋駅
OSAKA_CASTLE = (34.687315, 135.526201)
SYDNEY = (-33.856784, 151.215297)     # シドニー・オペラハウス


def build() -> list:
    """(相対パス, バイト列) の並び（mtime の順）。"""
    dup = jpeg(smooth_image(501), taken="2025-06-03 12:00", gps=SAPPORO)
    base = smooth_image(601)
    return [
        ("a01.jpg", jpeg(smooth_image(101), taken="2025-06-01 10:00", gps=TOKYO)),
        ("a02.jpg", jpeg(smooth_image(102, (3200, 2400)), taken="2025-06-01 14:00", gps=KOKYO)),
        ("a03.jpg", jpeg(smooth_image(103), taken="2025-06-02 09:00", gps=KYOTO, orientation=6)),
        ("n01.jpg", jpeg(smooth_image(201), taken="2025-06-01 10:10")),
        ("n02.jpg", jpeg(smooth_image(202), taken="2025-06-01 15:30")),
        ("n03.jpg", jpeg(smooth_image(203), taken="2025-06-02 17:00")),
        ("n04.jpg", jpeg(smooth_image(204), taken="2025-06-10 10:00")),
        ("n05.jpg", jpeg(smooth_image(205))),
        ("d01.jpg", dup),
        ("sub/d02.jpg", dup),
        ("s01.jpg", jpeg(base, taken="2025-06-04 11:00", gps=NAGOYA)),
        ("s02.jpg", jpeg(ImageEnhance.Brightness(base).enhance(1.03), taken="2025-06-04 11:02", gps=NAGOYA)),
        ("h01.heic", heic(smooth_image(701), taken="2025-06-05 09:00", gps=OSAKA_CASTLE)),
        ("n06.jpg", jpeg(smooth_image(206), taken="2025-06-05 09:05")),
        ("f01.jpg", jpeg(smooth_image(801), taken="2025-06-06 15:00", gps=SYDNEY)),
    ]


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    out = Path(sys.argv[1])
    if out.exists() and any(out.iterdir()):
        print("空のディレクトリを渡してください", file=sys.stderr)
        return 2
    files = build()
    for i, (rel, data) in enumerate(files):
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        os.utime(path, (BASE_MTIME + 10 * i, BASE_MTIME + 10 * i))
    print(f"{len(files)} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
