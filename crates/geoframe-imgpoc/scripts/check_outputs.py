"""出力JPEGのメタデータを集計する（EXIF・GPS・ICCが残っていないか、クロマサブサンプリング、寸法の上限）。
ファイル名・値は出さない。

  python check_outputs.py <ディレクトリ>...
"""
import sys
from pathlib import Path

from PIL import Image, JpegImagePlugin

for d in map(Path, sys.argv[1:]):
    n = exif = gps = icc = 0
    over = 0
    samp = {}
    for p in sorted(d.glob("*.jpg")):
        n += 1
        data = p.read_bytes()
        with Image.open(p) as im:
            ex = im.getexif()
            exif += 1 if (len(ex) or b"Exif\x00\x00" in data) else 0
            gps += 1 if len(ex.get_ifd(0x8825)) else 0
            icc += 1 if im.info.get("icc_profile") else 0
            over += 1 if max(im.size) > 2560 else 0
            s = JpegImagePlugin.get_sampling(im)
            samp[s] = samp.get(s, 0) + 1
    print(f"{d.name}: n={n} EXIFあり={exif} GPSあり={gps} ICCあり={icc} 長辺2560超={over} サブサンプリング={samp}")
