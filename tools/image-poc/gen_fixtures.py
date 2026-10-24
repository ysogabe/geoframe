"""crates/geoframe/tests/fixtures/ の合成HEIC（synthetic_*.heic）を作る（左半分が赤・右半分が青の64x48。写真ではない）。

pillow-heifのあるvenvで実行する:
    python gen_fixtures.py <出力ディレクトリ>
- synthetic_o1.heic: 向きなし
- synthetic_o6.heic: EXIFの向き6（pillow-heifがirotとして書く）。デコード後は48x64
- synthetic_p3.heic: Display P3相当のICC付き（向きなし）
どれにも合成のGPS・機種名のEXIFを付ける（出力に残らないことの確認用）。
"""
import sys
from pathlib import Path

import pillow_heif
from PIL import Image

pillow_heif.register_heif_opener()
sys.path.insert(0, str(Path(__file__).parent))
from icc_profiles import display_p3_profile  # noqa: E402

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)


def two_tone():
    im = Image.new("RGB", (64, 48))
    px = im.load()
    for y in range(48):
        for x in range(64):
            px[x, y] = (255, 0, 0) if x < 32 else (0, 0, 255)
    return im


def exif(orientation):
    ex = Image.Exif()
    ex[0x0112] = orientation
    ex[0x010F] = "SynthMaker"
    g = ex.get_ifd(0x8825)
    g[1] = "N"
    g[2] = (10.0, 20.0, 30.0)
    return ex.tobytes()


two_tone().save(out / "synthetic_o1.heic", "HEIF", quality=90, exif=exif(1))
two_tone().save(out / "synthetic_o6.heic", "HEIF", quality=90, exif=exif(6))
two_tone().save(out / "synthetic_p3.heic", "HEIF", quality=90, exif=exif(1), icc_profile=display_p3_profile())
