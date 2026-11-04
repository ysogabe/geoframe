"""境界データ（国・第1行政区画）の変換に使う、ネットワークにも pyshp にも依存しない部品。

`update_regions.py` が使い、`tools/tests/test_regions.py` が試験する。ここでやること:

- 形状の検査（閉じていない輪・有限でない座標・日付変更線をまたぐ辺・輪の自己交差・穴が外側に収まらない・穴同士の重なり、など）
- 点の内外判定（国参照の食い違いを調べる診断用。分類器の本体は Rust の `regions` モジュール）
- 成果物 `polygons.bin.gz` の符号化と復号（形式は docs/international-places/design.md の「境界データと分類器」と
  `crates/geoframe/src/regions/data.rs` の冒頭に書いたものと同じ）
- 名称表（日本語→英語→原表記の補完、検索用の別名）

座標は IEEE 754 倍精度のビット列のまま、前の点との差を符号付き 64 ビット整数で取り、ジグザグ＋可変長整数にする。
丸めも簡略化もしないので、復号した座標は元の shapefile の値とビット単位で一致する。
"""

from __future__ import annotations

import functools
import math
import re
import struct
from fractions import Fraction
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


class GeometryError(RegionDataError):
    """形状の不正。`code` は不正の種類（将来、例外表を設けるときの項目に使える）、`detail` は座標つきの説明。"""

    def __init__(self, code: str, label: str, detail: str) -> None:
        super().__init__(f"{label}: {detail}")
        self.code = code
        self.label = label
        self.detail = detail


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
    """1 本の輪の検査（自己交差を除く。自己交差は `validate_polygon` が多角形ぜんたいで調べる）。"""
    if len(ring) < 4:
        raise GeometryError("ring_too_short", label, "輪の点が 4 未満")
    if ring[0] != ring[-1]:
        raise GeometryError("ring_not_closed", label, "輪が閉じていない")
    for x, y in ring:
        if not (math.isfinite(x) and math.isfinite(y)):
            raise GeometryError("non_finite", label, "有限でない座標")
        if not (-180.0 <= x <= 180.0 and -90.0 <= y <= 90.0):
            raise GeometryError("out_of_range", label, f"範囲外の座標 ({x}, {y})")
    for (x1, _), (x2, _) in zip(ring, ring[1:]):
        # 経度 ±180 をまたぐ辺（東西が反対側の点を直接つなぐ辺）は、平面の判定では世界を横切る。
        # 両端がちょうど ±180 の辺（南極の縁など、図の端に沿う辺）は図の端に沿うだけなので許す
        if abs(x2 - x1) > 180.0 and not (abs(x1) == 180.0 and abs(x2) == 180.0):
            raise GeometryError("antimeridian_edge", label, "経度 ±180 をまたぐ辺がある")
    if ring_area2(ring) == 0.0:
        raise GeometryError("zero_area", label, "面積が 0 の輪")


# ---------------------------------------------------------------- 正確な幾何（交差・内外）

# Shewchuk の ccw の誤差上界（倍精度）。これより大きい行列式の符号は浮動小数点でも確実
_CCW_ERRBOUND = 3.3306690738754716e-16


def orient(ax: float, ay: float, bx: float, by: float, cx: float, cy: float) -> int:
    """a→b→c の向き。+1 が左回り、-1 が右回り、0 が一直線。厳密（誤差が怪しいときだけ有理数で計算し直す）。"""
    left = (bx - ax) * (cy - ay)
    right = (by - ay) * (cx - ax)
    det = left - right
    if abs(det) > _CCW_ERRBOUND * (abs(left) + abs(right)):
        return 1 if det > 0 else -1
    fa, fb, fc = (Fraction(ax), Fraction(ay)), (Fraction(bx), Fraction(by)), (Fraction(cx), Fraction(cy))
    exact = (fb[0] - fa[0]) * (fc[1] - fa[1]) - (fb[1] - fa[1]) * (fc[0] - fa[0])
    return (exact > 0) - (exact < 0)


def _within(p: Point, a: Point, b: Point) -> bool:
    """一直線上にある点 p が、線分 ab の範囲（端点を含む）にあるか。"""
    return min(a[0], b[0]) <= p[0] <= max(a[0], b[0]) and min(a[1], b[1]) <= p[1] <= max(a[1], b[1])


def segment_hit(a: Point, b: Point, c: Point, d: Point, contacts: list[Point] | None = None) -> int:
    """線分 ab と cd の関係。0 = 交わらない・端点で接するだけ、1 = 互いの内側で交差（X 字）、
    2 = 一直線上で長さを持って重なる。

    端点が相手の線分に乗る接触（輪の自己接触・穴が外周に接する）は 0 とする。ただし接点で輪が互いに
    突き抜けるか（横断）どうかは線分の組だけでは決まらないので、`contacts` が渡されたときは接点（4 つの端点の
    どれか）を入れて返し、呼び出し側が接点ごとに横断を調べる（`find_intersections`）。
    """
    if max(a[0], b[0]) < min(c[0], d[0]) or max(c[0], d[0]) < min(a[0], b[0]):
        return 0
    if max(a[1], b[1]) < min(c[1], d[1]) or max(c[1], d[1]) < min(a[1], b[1]):
        return 0
    o1 = orient(*a, *b, *c)
    o2 = orient(*a, *b, *d)
    o3 = orient(*c, *d, *a)
    o4 = orient(*c, *d, *b)
    if o1 * o2 < 0 and o3 * o4 < 0:
        return 1
    if contacts is not None and (o1 == 0 or o2 == 0 or o3 == 0 or o4 == 0):
        for o, p, q, r in ((o1, c, a, b), (o2, d, a, b), (o3, a, c, d), (o4, b, c, d)):
            if o == 0 and _within(p, q, r) and p not in contacts:
                contacts.append(p)
    if o1 == o2 == o3 == o4 == 0:
        # 一直線上。広がりの大きい軸に射影して、区間の重なりに長さがあるか
        axis = 0 if max(a[0], b[0], c[0], d[0]) - min(a[0], b[0], c[0], d[0]) >= max(
            a[1], b[1], c[1], d[1]
        ) - min(a[1], b[1], c[1], d[1]) else 1
        lo1, hi1 = sorted((a[axis], b[axis]))
        lo2, hi2 = sorted((c[axis], d[axis]))
        return 2 if max(lo1, lo2) < min(hi1, hi2) else 0
    return 0


def _dedupe_ring(ring: Sequence[Point]) -> list[Point]:
    """連続して同じ点を 1 つにした輪（閉じた形のまま）。長さ 0 の辺を取り除く。"""
    out = [ring[0]]
    for p in ring[1:]:
        if p != out[-1]:
            out.append(p)
    return out


def find_intersections(polygon: Polygon, limit: int = 8) -> list[tuple[str, int, int, int, int, Point]]:
    """多角形（外側の輪と穴）の辺どうしの不正な交差を、最大 `limit` 件返す。

    不正なのは (a) 互いの内側での交差（X 字）、(b) 一直線上で長さを持つ重なり（輪の折り返し・辺の重複を含む）。
    端点での接触は許すが、接点で輪が互いに突き抜ける横断（"touch_cross"。頂点を通る X 字）は不正とする。戻りは (種類, 輪 i, 辺 i, 輪 j, 辺 j, 目安の座標)。種類は "cross" / "overlap" / "backtrack" / "touch_cross"。

    辺を格子に入れ、同じ格子にある辺の組だけを調べる（頂点が百万単位でも実用的な時間で終わる）。
    """
    found: list[tuple[str, int, int, int, int, Point]] = []
    rings = [_dedupe_ring(r) for r in polygon]
    # 隣り合う辺の折り返し（A→B→A のように同じ向きへ戻る）
    for ri, r in enumerate(rings):
        n = len(r) - 1  # 辺の数（r[n] == r[0]）
        for i in range(n):
            p, q, s = r[i], r[i + 1], r[(i + 1) % n + 1]
            if orient(*p, *q, *s) == 0 and (q[0] - p[0]) * (s[0] - q[0]) + (q[1] - p[1]) * (s[1] - q[1]) < 0:
                found.append(("backtrack", ri, i, ri, (i + 1) % n, q))
                if len(found) >= limit:
                    return found
    edges: list[tuple[float, float, float, float, int, int]] = []
    total_len = 0.0
    minx = miny = math.inf
    maxx = maxy = -math.inf
    for ri, r in enumerate(rings):
        for i in range(len(r) - 1):
            (x1, y1), (x2, y2) = r[i], r[i + 1]
            edges.append((min(x1, x2), min(y1, y2), max(x1, x2), max(y1, y2), ri, i))
            total_len += max(abs(x2 - x1), abs(y2 - y1))
            minx, maxx = min(minx, x1, x2), max(maxx, x1, x2)
            miny, maxy = min(miny, y1, y2), max(maxy, y1, y2)
    if len(edges) < 4:
        return found
    cell = max(4.0 * total_len / len(edges), max(maxx - minx, maxy - miny) / 4096.0, 1e-12)
    grid: dict[tuple[int, int], list[int]] = {}
    floor = math.floor
    for k, (x0, y0, x1, y1, ri, i) in enumerate(edges):
        r = rings[ri]
        (ax, ay), (bx, by) = r[i], r[i + 1]
        c0, c1 = floor(x0 / cell), floor(x1 / cell)
        for cx in range(c0, c1 + 1):
            # この列の中で辺が通る y の範囲（列の左右の端での y と、辺の端点を含む）
            if ax == bx:
                lo, hi = y0, y1
            else:
                xl = max(cx * cell, x0)
                xr = min((cx + 1) * cell, x1)
                ya = ay + (xl - ax) * (by - ay) / (bx - ax)
                yb = ay + (xr - ax) * (by - ay) / (bx - ax)
                pad = cell * 1e-9
                lo, hi = max(min(ya, yb) - pad, y0), min(max(ya, yb) + pad, y1)
            for cy in range(floor(lo / cell), floor(hi / cell) + 1):
                grid.setdefault((cx, cy), []).append(k)
    seen: set[tuple[int, int]] = set()
    sizes = [len(r) - 1 for r in rings]
    touches: dict[Point, set[tuple[int, int]]] = {}
    contacts: list[Point] = []
    for members in grid.values():
        m = len(members)
        if m < 2:
            continue
        for u in range(m - 1):
            k1 = members[u]
            ex0, ey0, ex1, ey1, r1, i1 = edges[k1]
            for v in range(u + 1, m):
                k2 = members[v]
                fx0, fy0, fx1, fy1, r2, i2 = edges[k2]
                if ex1 < fx0 or fx1 < ex0 or ey1 < fy0 or fy1 < ey0:
                    continue
                if r1 == r2:
                    gap = abs(i1 - i2)
                    if gap == 1 or gap == sizes[r1] - 1:
                        continue  # 隣り合う辺（折り返しは上で調べた）
                pair = (k1, k2) if k1 < k2 else (k2, k1)
                if pair in seen:
                    continue
                seen.add(pair)
                a, b = rings[r1][i1], rings[r1][i1 + 1]
                c, d = rings[r2][i2], rings[r2][i2 + 1]
                contacts.clear()
                hit = segment_hit(a, b, c, d, contacts)
                if hit:
                    found.append(("cross" if hit == 1 else "overlap", r1, i1, r2, i2, a))
                    if len(found) >= limit:
                        return found
                for p in contacts:
                    touches.setdefault(p, set()).update(((r1, i1), (r2, i2)))
    for p, edge_ids in touches.items():
        pair = _crossing_at_contact(rings, p, edge_ids)
        if pair is not None:
            (r1, i1), (r2, i2) = pair
            found.append(("touch_cross", r1, i1, r2, i2, p))
            if len(found) >= limit:
                break
    return found


def _angle_cmp(u: tuple[Fraction, Fraction], v: tuple[Fraction, Fraction]) -> int:
    """原点から出る 2 つの向きの、反時計回りの順（+x 軸から [0, 2π)）。同じ向きは 0。"""
    hu = 0 if (u[1] > 0 or (u[1] == 0 and u[0] > 0)) else 1
    hv = 0 if (v[1] > 0 or (v[1] == 0 and v[0] > 0)) else 1
    if hu != hv:
        return hu - hv
    cross = u[0] * v[1] - u[1] * v[0]
    return -1 if cross > 0 else (1 if cross < 0 else 0)


def _crossing_at_contact(
    rings: Sequence[Sequence[Point]], p: Point, edge_ids: set[tuple[int, int]]
) -> tuple[tuple[int, int], tuple[int, int]] | None:
    """接点 p で、輪（の通り道）が互いに突き抜けているか。突き抜けていれば、その 2 本の通り道の辺を返す。

    通り道は、p が辺の内側にあるときはその辺ぜんたい（まっすぐ通る）、p が輪の頂点のときはその頂点の前後の辺。
    2 本の通り道の 4 つの向きが、p のまわりで交互に並ぶ（ABAB）なら横断。並びが AABB なら、接するだけ。
    """
    passes: dict[tuple, tuple[tuple[int, int], tuple[Fraction, Fraction], tuple[Fraction, Fraction]]] = {}
    px, py = Fraction(p[0]), Fraction(p[1])

    def vec(q: Point) -> tuple[Fraction, Fraction]:
        return (Fraction(q[0]) - px, Fraction(q[1]) - py)

    for ri, ei in sorted(edge_ids):
        ring = rings[ri]
        n = len(ring) - 1
        if p == ring[ei] or p == ring[ei + 1]:
            pos = ei if p == ring[ei] else (ei + 1) % n
            prev = ring[pos - 1] if pos > 0 else ring[n - 1]
            passes.setdefault(("v", ri, pos), ((ri, ei), vec(prev), vec(ring[pos + 1])))
        else:
            passes.setdefault(("e", ri, ei), ((ri, ei), vec(ring[ei]), vec(ring[ei + 1])))
    items = list(passes.values())
    for x in range(len(items) - 1):
        for y in range(x + 1, len(items)):
            (_, a1, a2), (_, b1, b2) = items[x], items[y]
            order = sorted(
                [("a", a1), ("a", a2), ("b", b1), ("b", b2)],
                key=functools.cmp_to_key(lambda s, t: _angle_cmp(s[1], t[1])),
            )
            labels = [name for name, _ in order]
            if labels[0] == labels[2]:
                return items[x][0], items[y][0]
    return None


class RingIndex:
    """輪の辺を緯度 1 度の帯に分けて持つ。診断用の点の内外判定を速くするための部品。"""

    def __init__(self, ring: Sequence[Point]) -> None:
        self.bands: dict[int, list[tuple[float, float, float, float]]] = {}
        xs = [p[0] for p in ring]
        ys = [p[1] for p in ring]
        self.bbox = (min(xs), min(ys), max(xs), max(ys))
        for (x1, y1), (x2, y2) in zip(ring, ring[1:]):
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


def _classify(index: "RingIndex", px: float, py: float) -> int:
    """点と輪の位置。1 = 内側、0 = 外側、-1 = 境界上。厳密（辺の向きの判定に `orient` を使う）。"""
    x0, y0, x1, y1 = index.bbox
    if not (x0 <= px <= x1 and y0 <= py <= y1):
        return 0
    inside = False
    for ax, ay, bx, by in index.bands.get(math.floor(py), ()):
        o = orient(ax, ay, bx, by, px, py)
        if o == 0 and min(ax, bx) <= px <= max(ax, bx) and min(ay, by) <= py <= max(ay, by):
            return -1
        if (ay > py) != (by > py) and (o > 0) == (by > ay):
            # 水平線との交点が点の右側にある（上向きの辺なら点が辺の左、下向きなら右）
            inside = not inside
    return 1 if inside else 0


def _probe_points(hole: Sequence[Point]) -> list[Point]:
    """穴の頂点と辺の中点（内外の判定に使う点）。"""
    pts = list(hole[:-1])
    pts += [((x1 + x2) / 2.0, (y1 + y2) / 2.0) for (x1, y1), (x2, y2) in zip(hole, hole[1:])]
    return pts


def validate_polygon(polygon: Polygon, label: str) -> None:
    """多角形の検査。輪ごとの検査、辺の自己交差・輪どうしの交差、穴が外側に収まること、穴どうしが重ならないこと。

    見つけた不正は `GeometryError`（`code` つき）で報告する。同じ多角形に複数の種類があるときは最初の 1 つ。
    """
    if not polygon:
        raise GeometryError("no_rings", label, "輪が無い多角形")
    for i, ring in enumerate(polygon):
        validate_ring(ring, f"{label} 輪{i}")
    hits = find_intersections(polygon)
    if hits:
        kind, ri, ei, rj, ej, at = hits[0]
        same = ri == rj
        where = f"輪{ri}の辺{ei}" if same else f"輪{ri}の辺{ei}と輪{rj}の辺{ej}"
        more = f"。他に {len(hits) - 1} 件以上" if len(hits) > 1 else ""
        raise GeometryError(
            "self_intersection" if same else "ring_intersection",
            label,
            f"{'自己交差' if same else '輪どうしの交差'}（{kind}）: {where} 付近 ({at[0]}, {at[1]}){more}",
        )
    if len(polygon) == 1:
        return
    indexes = [RingIndex(r) for r in polygon]
    for i, hole in enumerate(polygon[1:], start=1):
        # 辺が交差しないので、穴は外側の内側にあるか外側の外にあるかのどちらか（接触を除く）。
        # 頂点と辺の中点がどれも外側の外に出ていないこと、少なくとも 1 点は厳密に内側にあることを確かめる
        states = [_classify(indexes[0], x, y) for x, y in _probe_points(hole)]
        if 0 in states or 1 not in states:
            raise GeometryError("hole_outside", label, f"穴{i}が外側に収まらない")
    # 穴どうし: 一方が他方の内側に入っていない（辺が交差しないので、1 点の内外で決まる）
    for i in range(1, len(polygon)):
        for j in range(i + 1, len(polygon)):
            bi, bj = indexes[i].bbox, indexes[j].bbox
            if bi[2] < bj[0] or bj[2] < bi[0] or bi[3] < bj[1] or bj[3] < bi[1]:
                continue
            for a, b in ((i, j), (j, i)):
                states = [_classify(indexes[b], x, y) for x, y in _probe_points(polygon[a])]
                if 1 in states:
                    raise GeometryError("holes_overlap", label, f"穴{a}と穴{b}が重なる")


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
