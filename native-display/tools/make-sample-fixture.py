#!/usr/bin/env python3
"""合成テスト画像（fixture）を生成する。

【なぜ合成画像なのか】
実写真は個人データなのでリポジトリにコミットしない。代わりに、cover クロップの
挙動が目視で分かる合成画像を置く。合成画像なのでプライバシー問題がなく、
コミット対象にできる。

【なぜ 2560x1707 なのか】
実際の写真の寸法に合わせてある（3:2）。後続フェーズでメモリ予算を検証する際、
実物と同じピクセル数でテクスチャを確保できるようにするため。
1280x720 (16:9) へ cover すると、幅は使い切り、高さの上下が均等にクロップされる。

【何が見えるべきか】
  * 上下の色帯（赤 TOP / 橙 BOTTOM）… cover 後に「丸ごと消える」のが正解。
    帯の高さはクロップ量とピクセル単位で一致させてある。
  * 帯の中の番号付きマーカー 1〜4 … 上と同じく消えるのが正解。
    実行画面に数字が1つでも見えたら cover の計算が間違っている。
  * 左右の色帯（青 LEFT / 緑 RIGHT）… 幅は使い切るので「残る」のが正解。
  * 中央の大きな円と十字 … 画面中央に来るはず。
  * グリッド線 … 拡縮の比率が正しいか確認できる。
  * 緑の破線 … cover 後に残るセーフエリアの境界（画面の上下端に一致するはず）。

【文字は ASCII のみ】
このスクリプトはシステムの Arial 等を使うため、日本語を書くと豆腐（□）になる。
fixture は技術的なテストパターンなのでラベルは英数字に統一してある。
日本語表示の確認は SDL アプリ側（NotoSansJP.ttf）の担当。

使い方:
    python3 tools/make-sample-fixture.py
    python3 tools/make-sample-fixture.py --force

Pillow が必要:
    source ../app/.venv/bin/activate
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover
    sys.exit(
        "Pillow が見つかりません。\n"
        "  source ../app/.venv/bin/activate\n"
        "または pip install Pillow を実行してください。"
    )

# 実写真と同じ寸法（3:2）。メモリ予算検証のため実物に揃えている。
WIDTH = 2560
HEIGHT = 1707

# 表示側の論理解像度（16:9）。cover 後に残る範囲を描き込むために使う。
LOGICAL_WIDTH = 1280
LOGICAL_HEIGHT = 720

SIDE_BAND = 90  # 左右の色帯の太さ（こちらは cover 後も残る）

TOP_COLOR = (214, 69, 65)      # 赤: cover で切れる側
BOTTOM_COLOR = (232, 152, 48)  # 橙: cover で切れる側
LEFT_COLOR = (56, 132, 214)    # 青: cover で残る側
RIGHT_COLOR = (72, 176, 128)   # 緑: cover で残る側


def load_font(size: int) -> ImageFont.ImageFont:
    """等幅寄りのフォントを探す。見つからなければ Pillow の既定フォント。"""
    candidates = [
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
    ]
    for path in candidates:
        if Path(path).is_file():
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
    return ImageFont.load_default()


def cover_crop_box() -> tuple[int, int, int, int]:
    """1280x720 へ cover したとき、元画像のどこが残るかを返す。"""
    if WIDTH * LOGICAL_HEIGHT > LOGICAL_WIDTH * HEIGHT:
        # 元が相対的に横長 → 幅がクロップされる
        crop_w = HEIGHT * LOGICAL_WIDTH // LOGICAL_HEIGHT
        x = (WIDTH - crop_w) // 2
        return (x, 0, x + crop_w, HEIGHT)
    # 元が相対的に縦長 → 高さがクロップされる
    crop_h = WIDTH * LOGICAL_HEIGHT // LOGICAL_WIDTH
    y = (HEIGHT - crop_h) // 2
    return (0, y, WIDTH, y + crop_h)


def draw_centered(
    draw: ImageDraw.ImageDraw,
    xy: tuple[int, int],
    text: str,
    font: ImageFont.ImageFont,
    fill: tuple[int, int, int],
) -> None:
    draw.text(xy, text, font=font, fill=fill, anchor="mm")


def build_image() -> Image.Image:
    image = Image.new("RGB", (WIDTH, HEIGHT), (28, 32, 40))
    draw = ImageDraw.Draw(image)

    # cover 後に残る範囲。上下の帯はこの境界にピクセル単位で合わせる。
    safe_left, safe_top, safe_right, safe_bottom = cover_crop_box()
    top_band = safe_top                # 上でクロップされる高さ
    bottom_band = HEIGHT - safe_bottom  # 下でクロップされる高さ

    # --- 背景のグラデーション（縦方向）---
    for y in range(HEIGHT):
        t = y / (HEIGHT - 1)
        color = (
            int(30 + 46 * t),
            int(36 + 30 * t),
            int(52 + 26 * t),
        )
        draw.line([(0, y), (WIDTH, y)], fill=color)

    # --- グリッド線 ---
    step = 128
    for x in range(0, WIDTH + 1, step):
        draw.line([(x, 0), (x, HEIGHT)], fill=(255, 255, 255, 40), width=1)
    for y in range(0, HEIGHT + 1, step):
        draw.line([(0, y), (WIDTH, y)], fill=(255, 255, 255, 40), width=1)

    # --- 左右の色帯（cover 後も残る側）---
    draw.rectangle([0, 0, SIDE_BAND, HEIGHT], fill=LEFT_COLOR)
    draw.rectangle([WIDTH - SIDE_BAND, 0, WIDTH, HEIGHT], fill=RIGHT_COLOR)

    # --- 上下の色帯（cover で丸ごと消える側）---
    # 高さをクロップ量と一致させてあるので、実行画面に赤や橙が1ラインでも
    # 残っていたら cover の計算がずれている。
    draw.rectangle([0, 0, WIDTH, top_band - 1], fill=TOP_COLOR)
    draw.rectangle([0, HEIGHT - bottom_band, WIDTH, HEIGHT], fill=BOTTOM_COLOR)

    band_font = load_font(52)
    draw_centered(draw, (WIDTH // 2, top_band // 2),
                  f"TOP BAND {top_band}px - MUST DISAPPEAR", band_font, (255, 255, 255))
    draw_centered(draw, (WIDTH // 2, HEIGHT - bottom_band // 2),
                  f"BOTTOM BAND {bottom_band}px - MUST DISAPPEAR", band_font, (40, 30, 10))

    # --- 中央の大きな円 ---
    radius = 460
    cx, cy = WIDTH // 2, HEIGHT // 2
    draw.ellipse(
        [cx - radius, cy - radius, cx + radius, cy + radius],
        outline=(255, 255, 255),
        width=10,
    )
    draw.ellipse(
        [cx - radius // 2, cy - radius // 2, cx + radius // 2, cy + radius // 2],
        outline=(255, 214, 92),
        width=6,
    )

    # --- 中央の十字（センター確認用）---
    draw.line([(cx - 120, cy), (cx + 120, cy)], fill=(255, 255, 255), width=5)
    draw.line([(cx, cy - 120), (cx, cy + 120)], fill=(255, 255, 255), width=5)

    # --- cover 後に残る範囲（セーフエリア）を破線風に描く ---
    left, top, right, bottom = safe_left, safe_top, safe_right, safe_bottom
    dash = 40
    for x in range(left, right, dash * 2):
        draw.line([(x, top), (min(x + dash, right), top)], fill=(120, 255, 190), width=8)
        draw.line([(x, bottom - 1), (min(x + dash, right), bottom - 1)],
                  fill=(120, 255, 190), width=8)
    for y in range(top, bottom, dash * 2):
        draw.line([(left, y), (left, min(y + dash, bottom))], fill=(120, 255, 190), width=8)
        draw.line([(right - 1, y), (right - 1, min(y + dash, bottom))],
                  fill=(120, 255, 190), width=8)

    safe_font = load_font(44)
    draw.text(
        (left + SIDE_BAND + 28, top + 22),
        f"SAFE AREA 16:9  {right - left}x{bottom - top}",
        font=safe_font,
        fill=(120, 255, 190),
    )

    # --- 番号付きマーカー 1〜4 ---
    # 上下の色帯の「中」に置く。cover 後は帯ごと消えるので、実行画面に数字が
    # 1つでも見えたらクロップの計算が間違っている、という判定に使える。
    corner_font = load_font(64)
    marker_r = min(top_band, bottom_band) // 2 - 12
    top_y = top_band // 2
    bottom_y = HEIGHT - bottom_band // 2
    markers = [
        (360, top_y, "1"),
        (WIDTH - 360, top_y, "2"),
        (360, bottom_y, "3"),
        (WIDTH - 360, bottom_y, "4"),
    ]
    for x, y, number in markers:
        draw.ellipse(
            [x - marker_r, y - marker_r, x + marker_r, y + marker_r],
            fill=(18, 20, 28),
            outline=(255, 255, 255),
            width=5,
        )
        draw_centered(draw, (x, y), number, corner_font, (255, 255, 255))

    # --- 中央の説明テキスト（ASCII のみ。日本語は豆腐になる）---
    title_font = load_font(64)
    sub_font = load_font(38)
    draw_centered(draw, (cx, cy - 300), "SAMPLE FIXTURE (synthetic)", title_font,
                  (255, 255, 255))
    draw_centered(draw, (cx, cy + 250), f"source {WIDTH} x {HEIGHT}  (3:2)", sub_font,
                  (215, 222, 235))
    draw_centered(draw, (cx, cy + 300), "cover into 1280 x 720 (16:9)", sub_font,
                  (215, 222, 235))
    draw_centered(
        draw,
        (cx, cy + 356),
        "PASS = red/orange bands and markers 1-4 are all gone",
        sub_font,
        (120, 255, 190),
    )
    draw_centered(
        draw,
        (cx, cy + 402),
        "PASS = blue/green side bands stay visible at screen edges",
        sub_font,
        (120, 255, 190),
    )

    return image


def main() -> int:
    parser = argparse.ArgumentParser(description="合成 fixture 画像を生成する")
    parser.add_argument("--force", action="store_true", help="既存ファイルを上書きする")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "assets" / "fixtures" / "sample-photo.jpg",
        help="出力先",
    )
    args = parser.parse_args()

    output: Path = args.output
    if output.exists() and not args.force:
        print(f"既に存在します: {output}")
        print("上書きする場合は --force")
        return 0

    output.parent.mkdir(parents=True, exist_ok=True)
    image = build_image()
    image.save(output, format="JPEG", quality=88, optimize=True)

    print(f"生成しました: {output}")
    print(f"  寸法   : {image.width} x {image.height}")
    print(f"  サイズ : {output.stat().st_size} バイト")
    left, top, right, bottom = cover_crop_box()
    print(f"  1280x720 へ cover したときに残る範囲: ({left},{top})-({right},{bottom})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
