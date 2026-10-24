"""現行Python（Pillow＋pillow-heif、util.make_derivativeと同じ手順）で、同じ入力からderivativeを作り、
1枚ごとの時間と最大RSSを測る。ファイル名は出さない。

  python run_python_ref.py <入力ディレクトリ> <出力ディレクトリ> [--jobs N]
"""
import resource
import statistics
import sys
import time
from multiprocessing import Pool
from pathlib import Path

import pillow_heif
from PIL import Image, ImageOps

pillow_heif.register_heif_opener()


def make_derivative(src: Path, dest: Path, px=2560):  # app/geoframe/util.py と同じ
    with Image.open(src) as im:
        im = ImageOps.exif_transpose(im)
        im.thumbnail((px, px))
        if im.mode not in ("RGB", "L"):
            im = im.convert("RGB")
        im.save(dest, "JPEG", quality=85)


def one(args):
    src, dest = args
    t = time.perf_counter()
    make_derivative(src, dest)
    return (time.perf_counter() - t) * 1000, dest.stat().st_size


def main():
    src_dir, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 1
    out_dir.mkdir(parents=True, exist_ok=True)
    files = sorted(p for p in src_dir.iterdir() if p.is_file())
    work = [(p, out_dir / f"{p.stem}.jpg") for p in files]
    t = time.perf_counter()
    if jobs == 1:
        res = [one(w) for w in work]
        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    else:
        with Pool(jobs) as pool:
            res = pool.map(one, work, chunksize=1)
        rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    wall = time.perf_counter() - t
    ms = sorted(r[0] for r in res)
    p95 = ms[min(len(ms) - 1, round((len(ms) - 1) * 0.95))]
    print(f"枚数={len(ms)} jobs={jobs} wall={wall:.2f}s 処理量={len(ms) / wall:.2f}枚/秒 "
          f"1枚(ms) 中央値={statistics.median(ms):.1f} p95={p95:.1f} 最大={ms[-1]:.1f} "
          f"出力合計={sum(r[1] for r in res)}B 平均={sum(r[1] for r in res) // len(res)}B "
          f"最大RSS={rss}KiB（子プロセスの最大）")


main()
