"""大きい入力（約48MP: 8064x6048）のJPEGとHEICを1枚ずつ作る。最大RSSの目安を測るため。

  python prepare_big.py <data/jpegのディレクトリ> <出力ディレクトリ>
入力の最初の1枚を拡大して使う（名前は出さない）。出力は big/jpeg/001.jpg と big/heic/001.heic。
"""
import sys
from pathlib import Path

import pillow_heif
from PIL import Image, ImageChops

pillow_heif.register_heif_opener()
src, out = Path(sys.argv[1]), Path(sys.argv[2])
(out / "jpeg").mkdir(parents=True, exist_ok=True)
(out / "heic").mkdir(parents=True, exist_ok=True)
first = sorted(src.glob("*.jpg"))[0]
with Image.open(first) as im:
    im = im.convert("RGB").resize((8064, 6048), Image.Resampling.LANCZOS)
im = ImageChops.add(im, Image.effect_noise(im.size, 3.0).convert("RGB"), 1.0, -128)
ex = Image.Exif()
ex[0x0112] = 1
im.save(out / "jpeg" / "001.jpg", "JPEG", quality=92, exif=ex.tobytes())
im.save(out / "heic" / "001.heic", "HEIF", quality=85, exif=ex.tobytes())
print("作成: 8064x6048 の JPEG と HEIC")
