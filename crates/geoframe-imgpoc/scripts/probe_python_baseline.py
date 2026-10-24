"""現行Python（make_derivativeと同じ呼び出し）の挙動を合成画像で確かめる。写真は使わない。

検証環境の /opt/geoframe-pyref/app/.venv/bin/python で実行する:
    python probe_python_baseline.py <作業ディレクトリ>
"""
import io
import sys
from pathlib import Path

import pillow_heif
from PIL import Image, ImageCms, ImageOps

pillow_heif.register_heif_opener()
sys.path.insert(0, str(Path(__file__).parent))
from icc_profiles import display_p3_profile  # noqa: E402

work = Path(sys.argv[1])


def synth(w, h):
    """向きの確認用に、左上が赤・右下が青の勾配画像"""
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            px[x, y] = (int(255 * (1 - x / w)), int(255 * y / h), int(255 * x / w))
    return im


def exif_bytes(orientation, gps=True):
    ex = Image.Exif()
    ex[0x0112] = orientation
    ex[0x010F] = "SynthMaker"
    if gps:
        g = ex.get_ifd(0x8825)
        g[1] = "N"
        g[2] = (10.0, 20.0, 30.0)
        g[3] = "E"
        g[4] = (100.0, 40.0, 50.0)
    return ex.tobytes()


def make_derivative(src, dest, rotation=0, px=2560):  # util.make_derivative と同じ手順
    with Image.open(src) as im:
        im = ImageOps.exif_transpose(im)
        if rotation:
            im = im.rotate(-rotation, expand=True)
        im.thumbnail((px, px))
        if im.mode not in ("RGB", "L"):
            im = im.convert("RGB")
        im.save(dest, "JPEG", quality=85)


def describe(path):
    with Image.open(path) as im:
        ex = im.getexif()
        data = path.read_bytes()
        return {
            "size": im.size,
            "bytes": len(data),
            "exif_tags": len(ex),
            "gps_ifd": len(ex.get_ifd(0x8825)),
            "icc": im.info.get("icc_profile") is not None,
            "app1_exif_in_file": b"Exif\x00\x00" in data,
            "jfif": "jfif" in im.info,
            "subsampling": getattr(__import__("PIL.JpegImagePlugin", fromlist=["x"]), "get_sampling")(im),
        }


icc = display_p3_profile()
cases = []
for orient in (1, 3, 6, 8):
    src = work / f"probe_{orient}.jpg"
    synth(400, 300).save(src, "JPEG", quality=92, exif=exif_bytes(orient), icc_profile=icc)
    cases.append(("jpeg", orient, src))
    heic = work / f"probe_{orient}.heic"
    synth(400, 300).save(heic, "HEIF", quality=85, exif=exif_bytes(orient), icc_profile=icc)
    cases.append(("heic", orient, heic))

print("# 入力と、Pythonのderivative（長辺2560へ縮小、品質85）")
for kind, orient, src in cases:
    dest = work / f"out_{src.stem}_{kind}.jpg"
    with Image.open(src) as im:
        raw_orient = im.getexif().get(0x0112)
        isize = im.size
        has_icc = im.info.get("icc_profile") is not None
    make_derivative(src, dest, px=200)
    print(kind, "指定Orientation", orient, "読めたOrientation", raw_orient, "デコード後の寸法", isize,
          "ICC有", has_icc, "→", describe(dest))

# 小さい画像は拡大しない
small = work / "probe_small.jpg"
synth(300, 200).save(small, "JPEG", quality=92)
make_derivative(small, work / "out_small.jpg")
print("小さい画像", describe(work / "out_small.jpg")["size"])

# 縮小の挙動: thumbnail の既定フィルタ
print("Pillow", Image.__version__ if hasattr(Image, "__version__") else "?")
print("thumbnail既定 resample", Image.Resampling.BICUBIC, "（Pillowの既定。reducing_gapは2.0）")
