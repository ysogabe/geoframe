"""計測用の入力を作る（検証環境の中で実行する。写真もファイル名も標準出力へ出さない）。

本番コピーのderivatives（長辺2560のJPEG）から無作為に選び、実際の原本（スマートフォンの
4032x3024相当）に近づけて、JPEGとHEICの入力を作る。

  - 長辺を4032へ拡大し、輝度ノイズを足す（拡大だけだとファイルが小さくなりすぎ、デコード時間が
    実際より短くなるため）
  - EXIFの向き（1/3/6/8）は、表示したときに元の向きになるよう画素を逆変換してから付ける
  - 一部はICC（Display P3相当）を付け、画素もP3へ変換しておく（実機の広色域写真に近づける）
  - EXIFには合成のGPS・撮影時刻・機種を入れる（出力に残らないことの確認用。実写真の値は使わない）
  - 入力名は連番（001.jpg…）。元のファイル名は記録しない

実行（pyrefのvenvで）:
  python prepare_data.py <derivativesのディレクトリ> <作業ディレクトリ> [--seed N]
作業ディレクトリの下に jpeg/ heic/ gt/（正解: sRGBの2560px PNG）を作る。
"""
import io
import os
import random
import sys
from pathlib import Path

import pillow_heif
from PIL import Image, ImageChops, ImageCms, ImageOps

pillow_heif.register_heif_opener()
sys.path.insert(0, str(Path(__file__).parent))
from icc_profiles import display_p3_profile  # noqa: E402

src_dir = Path(sys.argv[1])
work = Path(sys.argv[2])
seed = int(sys.argv[sys.argv.index("--seed") + 1]) if "--seed" in sys.argv else 54
N_JPEG, N_HEIC = 50, 50
LONG = 4032

names = sorted(p for p in src_dir.iterdir() if p.suffix == ".jpg" and ".r" not in p.suffixes[:-1])
rng = random.Random(seed)
picked = rng.sample(names, N_JPEG + N_HEIC)

for d in ("jpeg", "heic", "gt"):
    (work / d).mkdir(mode=0o700, exist_ok=True)

p3 = display_p3_profile()
srgb_prof = ImageCms.createProfile("sRGB")
to_p3 = ImageCms.buildTransform(srgb_prof, ImageCms.ImageCmsProfile(io.BytesIO(p3)), "RGB", "RGB")
ORIENTS = [1, 3, 6, 8]
# 逆変換（保存する画素）。Pillowのexif_transposeが当てる変換の逆
INVERSE = {1: None, 3: Image.Transpose.ROTATE_180, 6: Image.Transpose.ROTATE_90, 8: Image.Transpose.ROTATE_270}


def exif_bytes(orientation, i):
    ex = Image.Exif()
    ex[0x0112] = orientation
    ex[0x010F] = "SynthMaker"
    ex[0x0110] = "SynthCam"
    ex.get_ifd(0x8769)[36867] = "2020:01:02 03:04:05"
    g = ex.get_ifd(0x8825)
    g[1] = "N"
    g[2] = (10.0 + i % 40, 20.0, 30.0)
    g[3] = "E"
    g[4] = (100.0 + i % 70, 40.0, 50.0)
    return ex.tobytes()


def noisy(im, sigma):
    noise = Image.effect_noise(im.size, sigma)
    return ImageChops.add(im, noise.convert("RGB"), 1.0, -128)


def build(i, path):
    with Image.open(path) as im:
        im = ImageOps.exif_transpose(im).convert("RGB")
    w, h = im.size
    # 12枚に1枚（各種類で4枚）はそのまま2560（拡大しない）
    if i % 12 != 11:
        scale = LONG / max(w, h)
        im = im.resize((round(w * scale), round(h * scale)), Image.Resampling.LANCZOS)
        im = noisy(im, 3.0)
    return im


def main():
    stats = {"jpeg": 0, "heic": 0}
    for idx, path in enumerate(picked):
        kind = "jpeg" if idx < N_JPEG else "heic"
        i = idx if kind == "jpeg" else idx - N_JPEG
        orient = ORIENTS[i % 4]
        use_icc = (i % 3 == 0)
        display = build(i, path)
        # 正解（sRGBの表示向き、2560px）
        gt = display.copy()
        gt.thumbnail((2560, 2560), Image.Resampling.LANCZOS)
        name = f"{i + 1:03d}"
        gt.save(work / "gt" / f"{name}_{kind}.png", compress_level=1)
        stored = display
        if INVERSE[orient] is not None:
            stored = stored.transpose(INVERSE[orient])
        icc = None
        if use_icc:
            stored = ImageCms.applyTransform(stored, to_p3)
            icc = p3
        exif = exif_bytes(orient, i)
        if kind == "jpeg":
            stored.save(work / "jpeg" / f"{name}.jpg", "JPEG", quality=92, exif=exif, icc_profile=icc)
        else:
            # pillow-heifは、EXIFの向きがあるとirot/imirとして書く。渡したEXIFのOrientationも残るので、ファイルは
            # irotとEXIFの両方を持つ（実機のHEICと同じ。デコーダがEXIF側を無視することの確認になる）
            stored.save(work / "heic" / f"{name}.heic", "HEIF", quality=85, exif=exif, icc_profile=icc)
        stats[kind] += 1
    print("作成:", stats)


main()
