"""RustのderivativeとPythonのderivativeの画質差（PSNR・SSIM）を集計する。ファイル名・画像の内容は出さない。

  python compare.py <基準の出力> <比べる出力> [<正解のPNGディレクトリ> <種別 jpeg|heic>] [--step N]

--step N: N枚ごとに1枚だけ比べる（SSIMが重いため）。
--only icc|noicc: prepare_data.pyがICCを付けた入力（連番が3で割って1余る）だけ、またはそれ以外だけを比べる。

numpy が要る（使い捨てのvenvに入れる）。SSIMは輝度のみ、一様7x7窓、2560pxのまま。
"""
import statistics
import sys
from pathlib import Path

import numpy as np
from PIL import Image

C1, C2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2


def box(a, k=7):
    """k×kの一様窓の平均（積分画像）"""
    c = np.cumsum(np.cumsum(a, axis=0), axis=1)
    c = np.pad(c, ((1, 0), (1, 0)))
    s = c[k:, k:] - c[:-k, k:] - c[k:, :-k] + c[:-k, :-k]
    return s / (k * k)


def ssim_luma(a, b):
    ya = np.asarray(a.convert("L"), dtype=np.float64)
    yb = np.asarray(b.convert("L"), dtype=np.float64)
    mua, mub = box(ya), box(yb)
    saa = box(ya * ya) - mua * mua
    sbb = box(yb * yb) - mub * mub
    sab = box(ya * yb) - mua * mub
    s = ((2 * mua * mub + C1) * (2 * sab + C2)) / ((mua**2 + mub**2 + C1) * (saa + sbb + C2))
    return float(s.mean())


def psnr(a, b):
    d = np.asarray(a, dtype=np.float64) - np.asarray(b, dtype=np.float64)
    mse = float((d * d).mean())
    return 99.0 if mse == 0 else 10 * np.log10(255**2 / mse)


def summarize(label, vals):
    if not vals:
        return f"{label}: データなし"
    v = sorted(vals)
    return f"{label}: n={len(v)} 最小={v[0]:.2f} 中央値={statistics.median(v):.2f} 最大={v[-1]:.2f}"


def main():
    args = sys.argv[1:]
    step = 1
    if "--step" in args:
        i = args.index("--step")
        step = int(args[i + 1])
        del args[i : i + 2]
    only = None
    if "--only" in args:
        i = args.index("--only")
        only = args[i + 1]
        del args[i : i + 2]
    py_dir, rs_dir = Path(args[0]), Path(args[1])
    gt_dir = Path(args[2]) if len(args) > 2 else None
    kind = args[3] if len(args) > 3 else None
    psnrs, ssims, gps, gss, mismatch, rsz = [], [], [], [], 0, 0
    gps_py, gss_py = [], []
    for p in sorted(py_dir.glob("*.jpg"))[::step]:
        q = rs_dir / p.name
        if not q.exists():
            continue
        if only and (int(p.stem) % 3 == 1) != (only == "icc"):
            continue
        a, b = Image.open(p).convert("RGB"), Image.open(q).convert("RGB")
        if a.size != b.size:
            mismatch += 1
            continue
        psnrs.append(psnr(a, b))
        ssims.append(ssim_luma(a, b))
        if gt_dir and kind:
            g = gt_dir / f"{p.stem}_{kind}.png"
            if g.exists():
                gt = Image.open(g).convert("RGB")
                if gt.size == b.size:
                    gps.append(psnr(gt, b))
                    gss.append(ssim_luma(gt, b))
                    gps_py.append(psnr(gt, a))
                    gss_py.append(ssim_luma(gt, a))
    print(f"寸法不一致={mismatch}")
    print(summarize("PSNR dB（Rust対Python）", psnrs))
    print(summarize("SSIM（Rust対Python）", ssims))
    if gps:
        print(summarize("PSNR dB（Rust対正解）", gps))
        print(summarize("PSNR dB（Python対正解）", gps_py))
        print(summarize("SSIM（Rust対正解）", gss))
        print(summarize("SSIM（Python対正解）", gss_py))


main()
