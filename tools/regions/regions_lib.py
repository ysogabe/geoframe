"""境界データ（国・第1行政区画）の変換に使う、ネットワークにも pyshp にも依存しない部品。

`update_regions.py` が使い、`tools/tests/test_regions.py` が試験する。ここでやること:

- 形状の検査（閉じていない輪・有限でない座標・日付変更線をまたぐ辺・穴が外側に収まらない、など）
- 点の内外判定（国参照の食い違いを調べる診断用。分類器の本体は Rust の `regions` モジュール）
- 成果物 `polygons.bin.gz` の符号化と復号（形式は docs/international-places/design.md の「境界データと分類器」と
  `crates/geoframe/src/regions/data.rs` の冒頭に書いたものと同じ）
- 名称表（日本語→英語→原表記の補完、検索用の別名）

座標は IEEE 754 倍精度のビット列のまま、前の点との差を符号付き 64 ビット整数で取り、ジグザグ＋可変長整数にする。
丸めも簡略化もしないので、復号した座標は元の shapefile の値とビット単位で一致する。
"""

from __future__ import annotations

import math
import re
import struct
from typing import Iterable, Sequence

MAGIC = b"GFREGN01"
FORMAT_VERSION = 1
ID_RE = re.compile(r"^[A-Za-z0-9:_-]{1,64}\Z")
ISO_3166_2_RE = re.compile(r"^[A-Z]{2}-[A-Z0-9]{1,3}\Z")
_M64 = (1 << 64) - 1

Point = tuple[float, float]
Ring = list[Point]
Polygon = list[Ring]  # 先頭が外側、残りが穴


class RegionDataError(ValueError):
    """元データまたは対応表が不正。変換を止める。"""


# ---------------------------------------------------------------- 可変長整数

def _put_varint(out: bytearray, n: int) -> None:
    if n < 0:
        raise ValueError("varint は 0 以上")
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return


def _zigzag64(d: int) -> int:
    return ((d << 1) ^ (d >> 63)) & _M64


def _unzigzag64(z: int) -> int:
    return (z >> 1) ^ -(z & 1)


def _bits(v: float) -> int:
    return struct.unpack("<q", struct.pack("<d", v))[0]


def _from_bits(b: int) -> float:
    return struct.unpack("<d", struct.pack("<q", b))[0]


def _wrap_i64(n: int) -> int:
    return ((n + (1 << 63)) & _M64) - (1 << 63)


class _Reader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def byte(self) -> int:
        if self.pos >= len(self.data):
            raise RegionDataError("polygons: 途中で終わっている")
        b = self.data[self.pos]
        self.pos += 1
        return b

    def varint(self) -> int:
        shift = 0
        n = 0
        while True:
            b = self.byte()
            n |= (b & 0x7F) << shift
            if not b & 0x80:
                return n
            shift += 7
            if shift > 70:
                raise RegionDataError("polygons: 可変長整数が長すぎる")

    def take(self, n: int) -> bytes:
        if self.pos + n > len(self.data):
            raise RegionDataError("polygons: 途中で終わっている")
        out = self.data[self.pos : self.pos + n]
        self.pos += n
        return out


# ---------------------------------------------------------------- 符号化

def encode_polygons(countries: Sequence[dict], regions: Sequence[dict]) -> bytes:
    """特徴量（`id`・`polygons`、地域は `country`＝国の添字）を `polygons.bin`（gzip 前）にする。

    並びは呼び出し側で ID 順にしておく（再現性のため）。
    """
    out = bytearray(MAGIC)
    _put_varint(out, len(countries))
    _put_varint(out, len(regions))
    prev_x = 0
    prev_y = 0
    for is_region, features in ((False, countries), (True, regions)):
        for f in features:
            raw_id = f["id"].encode("utf-8")
            _put_varint(out, len(raw_id))
            out += raw_id
            if is_region:
                _put_varint(out, f["country"])
            _put_varint(out, len(f["polygons"]))
            for polygon in f["polygons"]:
                _put_varint(out, len(polygon))
                for ring in polygon:
                    _put_varint(out, len(ring))
                    for x, y in ring:
                        bx, by = _bits(x), _bits(y)
                        _put_varint(out, _zigzag64(_wrap_i64(bx - prev_x)))
                        _put_varint(out, _zigzag64(_wrap_i64(by - prev_y)))
                        prev_x, prev_y = bx, by
    return bytes(out)


def decode_polygons(data: bytes) -> tuple[list[dict], list[dict]]:
    r = _Reader(data)
    if r.take(len(MAGIC)) != MAGIC:
        raise RegionDataError("polygons: マジックが違う")
    n_countries = r.varint()
    n_regions = r.varint()
    prev_x = 0
    prev_y = 0
    layers: list[list[dict]] = []
    for is_region, count in ((False, n_countries), (True, n_regions)):
        features = []
        for _ in range(count):
            fid = r.take(r.varint()).decode("utf-8")
            feature: dict = {"id": fid}
            if is_region:
                feature["country"] = r.varint()
            polygons = []
            for _ in range(r.varint()):
                rings = []
                for _ in range(r.varint()):
                    ring = []
                    for _ in range(r.varint()):
                        prev_x = _wrap_i64(prev_x + _unzigzag64(r.varint()))
                        prev_y = _wrap_i64(prev_y + _unzigzag64(r.varint()))
                        ring.append((_from_bits(prev_x), _from_bits(prev_y)))
                    rings.append(ring)
                polygons.append(rings)
            feature["polygons"] = polygons
            features.append(feature)
        layers.append(features)
    if r.pos != len(data):
        raise RegionDataError("polygons: 末尾に余りがある")
    return layers[0], layers[1]


# ---------------------------------------------------------------- 形状の検査

def ring_area2(ring: Sequence[Point]) -> float:
    """符号付き面積の 2 倍（靴ひも）。"""
    return sum(x1 * y2 - x2 * y1 for (x1, y1), (x2, y2) in zip(ring, ring[1:]))


def validate_ring(ring: Sequence[Point], label: str) -> None:
    if len(ring) < 4:
        raise RegionDataError(f"{label}: 輪の点が 4 未満")
    if ring[0] != ring[-1]:
        raise RegionDataError(f"{label}: 輪が閉じていない")
    for x, y in ring:
        if not (math.isfinite(x) and math.isfinite(y)):
            raise RegionDataError(f"{label}: 有限でない座標")
        if not (-180.0 <= x <= 180.0 and -90.0 <= y <= 90.0):
            raise RegionDataError(f"{label}: 範囲外の座標 ({x}, {y})")
    for (x1, _), (x2, _) in zip(ring, ring[1:]):
        # 経度 ±180 をまたぐ辺（東西が反対側の点を直接つなぐ辺）は、平面の判定では世界を横切る。
        # 両端がちょうど ±180 の辺（南極の縁など、図の端に沿う辺）は図の端に沿うだけなので許す
        if abs(x2 - x1) > 180.0 and not (abs(x1) == 180.0 and abs(x2) == 180.0):
            raise RegionDataError(f"{label}: 経度 ±180 をまたぐ辺がある")
    if ring_area2(ring) == 0.0:
        raise RegionDataError(f"{label}: 面積が 0 の輪")


class RingIndex:
    """輪の辺を緯度 1 度の帯に分けて持つ。診断用の点の内外判定を速くするための部品。"""

    def __init__(self, ring: Sequence[Point]) -> None:
        self.bands: dict[int, list[tuple[float, float, float, float]]] = {}
        xs = [p[0] for p in ring]
        ys = [p[1] for p in ring]
        self.bbox = (min(xs), min(ys), max(xs), max(ys))
        for (x1, y1), (x2, y2) in zip(ring, ring[1:]):
            if y1 == y2:
                continue
            lo, hi = (y1, y2) if y1 < y2 else (y2, y1)
            for band in range(math.floor(lo), math.floor(hi) + 1):
                self.bands.setdefault(band, []).append((x1, y1, x2, y2))

    def crossings_at(self, py: float) -> list[float]:
        """水平線 y=py と辺の交点の x（半開区間の規則）。"""
        out = []
        for x1, y1, x2, y2 in self.bands.get(math.floor(py), ()):
            if (y1 > py) != (y2 > py):
                out.append(x1 + (py - y1) * (x2 - x1) / (y2 - y1))
        return out

    def contains(self, px: float, py: float) -> bool:
        """偶奇判定（境界上は不定）。診断にだけ使う。"""
        inside = False
        for x in self.crossings_at(py):
            if x > px:
                inside = not inside
        return inside


class PolygonIndex:
    def __init__(self, polygon: Polygon) -> None:
        self.rings = [RingIndex(r) for r in polygon]
        self.bbox = self.rings[0].bbox

    def contains(self, px: float, py: float) -> bool:
        x0, y0, x1, y1 = self.bbox
        if not (x0 <= px <= x1 and y0 <= py <= y1):
            return False
        n = sum(1 for r in self.rings if r.contains(px, py))
        return n % 2 == 1

    def interior_point(self) -> Point:
        """外側の輪と穴の内側にある代表点。水平線の最長の内側区間の中点。"""
        x0, y0, x1, y1 = self.bbox
        best: tuple[float, Point] | None = None
        # 帯の端と頂点の y に当たらないよう、複数の高さを試して最長の区間を採る
        for frac in (0.5, 0.37, 0.63, 0.21, 0.79):
            py = y0 + (y1 - y0) * frac + 1.234567e-7
            xs = sorted(x for r in self.rings for x in r.crossings_at(py))
            for a, b in zip(xs[0::2], xs[1::2]):
                if best is None or b - a > best[0]:
                    best = (b - a, ((a + b) / 2.0, py))
            if best is not None and best[0] > 0:
                break
        if best is None:
            raise RegionDataError("代表点を取れない輪")
        return best[1]


def validate_polygon(polygon: Polygon, label: str) -> None:
    if not polygon:
        raise RegionDataError(f"{label}: 輪が無い多角形")
    for i, ring in enumerate(polygon):
        validate_ring(ring, f"{label} 輪{i}")
    if len(polygon) == 1:
        return
    outer = RingIndex(polygon[0])
    ox0, oy0, ox1, oy1 = outer.bbox
    for i, hole in enumerate(polygon[1:], start=1):
        xs = [p[0] for p in hole]
        ys = [p[1] for p in hole]
        if min(xs) < ox0 or max(xs) > ox1 or min(ys) < oy0 or max(ys) > oy1:
            raise RegionDataError(f"{label}: 穴{i}が外側の範囲からはみ出す")
        # 穴の頂点の過半数が外側の内側か境界上にあること（境界に接する穴を許す）。
        # 内外が不定な境界上の点を避けるため、各頂点をごく小さくずらして数える
        inside = sum(1 for x, y in hole if outer.contains(x, y + 1e-9) or outer.contains(x, y - 1e-9))
        if inside * 2 < len(hole):
            raise RegionDataError(f"{label}: 穴{i}が外側に収まらない")


# ---------------------------------------------------------------- 名称表

def clean_name(value: object) -> str:
    """表記の前後の空白を落とす。欠損値（None・空・'-99'）は空文字にする。"""
    if value is None:
        return ""
    text = str(value).strip()
    if text in ("-99", "-1"):
        return ""
    return text


def usable_alias(text: str) -> bool:
    """検索の別名に使える表記か。空・制御文字・置換文字（文字化け）を含むものは外す。"""
    if not text:
        return False
    return not any(ord(c) < 0x20 or c == "�" for c in text)


def dedupe(items: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    out = []
    for item in items:
        if usable_alias(item) and item not in seen:
            seen.add(item)
            out.append(item)
    return out


def split_alt(value: object) -> list[str]:
    return [clean_name(p) for p in clean_name(value).split("|")]


def display_name(*candidates: str) -> str:
    """日本語→英語→原表記の順に、最初の空でない表記。全て空なら空文字。"""
    for c in candidates:
        if c:
            return c
    return ""
