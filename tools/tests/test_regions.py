"""tools/regions/ の試験（標準ライブラリだけ。pyshp は要らない）。

形状の検査・符号化の往復・名称の補完・対応表と、コミット済みの成果物（crates/geoframe/data/regions/）が
manifest と一致することを確かめる。pyshp と取得済みの元データがあるときだけ、元の shapefile との
ビット単位の一致も確かめる（環境変数 REGIONS_SOURCE_DIR に取得済みのディレクトリを指す）。
"""

import gzip
import hashlib
import importlib.util
import json
import math
import os
import random
import sys
import tempfile
import time
import unittest
from fractions import Fraction
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools" / "regions"
DATA = ROOT / "crates" / "geoframe" / "data" / "regions"
sys.path.insert(0, str(TOOL))

import regions_lib as lib  # noqa: E402

spec = importlib.util.spec_from_file_location("update_regions", TOOL / "update_regions.py")
update_regions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update_regions)

SQUARE = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
HOLE = [(4.0, 4.0), (4.0, 6.0), (6.0, 6.0), (6.0, 4.0), (4.0, 4.0)]


class ValidateRing(unittest.TestCase):
    def test_accepts_a_closed_ring(self):
        lib.validate_ring(SQUARE, "t")

    def test_rejects_bad_rings(self):
        bad = {
            "点が少ない": [(0.0, 0.0), (1.0, 0.0), (0.0, 0.0)],
            "閉じていない": [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            "NaN": [(0.0, 0.0), (1.0, math.nan), (1.0, 1.0), (0.0, 0.0)],
            "無限": [(0.0, 0.0), (math.inf, 0.0), (1.0, 1.0), (0.0, 0.0)],
            "経度が範囲外": [(0.0, 0.0), (181.0, 0.0), (1.0, 1.0), (0.0, 0.0)],
            "緯度が範囲外": [(0.0, 0.0), (1.0, 91.0), (1.0, 1.0), (0.0, 0.0)],
            "面積 0": [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (0.0, 0.0)],
        }
        for label, ring in bad.items():
            with self.subTest(label), self.assertRaises(lib.RegionDataError):
                lib.validate_ring(ring, "t")

    def test_rejects_an_edge_crossing_the_antimeridian(self):
        ring = [(170.0, 0.0), (-170.0, 0.0), (-170.0, 5.0), (170.0, 5.0), (170.0, 0.0)]
        with self.assertRaisesRegex(lib.RegionDataError, "±180"):
            lib.validate_ring(ring, "t")

    def test_allows_edges_along_the_map_border(self):
        # 南極の縁のように、両端がちょうど ±180 の辺は図の端に沿うだけ
        ring = [(-180.0, -90.0), (180.0, -90.0), (180.0, -80.0), (-180.0, -80.0), (-180.0, -90.0)]
        lib.validate_ring(ring, "t")
        # 片方だけ ±180 の長い辺（-180 → 100 は 280 度）や、東西が反対側の点をつなぐ辺は拒否
        for ring in (
            [(-180.0, -90.0), (100.0, -90.0), (100.0, -80.0), (-180.0, -80.0), (-180.0, -90.0)],
            [(-179.0, 0.0), (179.0, 0.0), (179.0, 1.0), (-179.0, 1.0), (-179.0, 0.0)],
        ):
            with self.assertRaises(lib.RegionDataError):
                lib.validate_ring(ring, "t")


class ValidatePolygon(unittest.TestCase):
    def test_hole_inside(self):
        lib.validate_polygon([SQUARE, HOLE], "t")

    def test_hole_touching_the_outer_ring_at_a_point_is_fine(self):
        # 頂点が外側の辺に乗る・頂点どうしが一致する接触は許す（偶奇判定を変えない）
        on_edge = [(0.0, 5.0), (2.0, 4.0), (2.0, 6.0), (0.0, 5.0)]
        at_vertex = [(0.0, 0.0), (2.0, 1.0), (1.0, 2.0), (0.0, 0.0)]
        lib.validate_polygon([SQUARE, on_edge], "t")
        lib.validate_polygon([SQUARE, at_vertex], "t")

    def test_hole_sharing_an_edge_with_the_outer_ring_is_rejected(self):
        # 一直線上で長さを持って重なる辺は、外周に厚み 0 の首ができるので拒否する
        sharing = [(0.0, 4.0), (0.0, 6.0), (2.0, 6.0), (2.0, 4.0), (0.0, 4.0)]
        with self.assertRaises(lib.GeometryError) as cm:
            lib.validate_polygon([SQUARE, sharing], "t")
        self.assertEqual(cm.exception.code, "ring_intersection")

    def test_hole_outside_is_rejected(self):
        outside = [(20.0, 20.0), (20.0, 22.0), (22.0, 22.0), (22.0, 20.0), (20.0, 20.0)]
        with self.assertRaises(lib.RegionDataError):
            lib.validate_polygon([SQUARE, outside], "t")

    def test_hole_within_bbox_but_outside_the_shape_is_rejected(self):
        l_shape = [(0.0, 0.0), (10.0, 0.0), (10.0, 2.0), (2.0, 2.0), (2.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
        hole = [(6.0, 6.0), (6.0, 8.0), (8.0, 8.0), (8.0, 6.0), (6.0, 6.0)]
        with self.assertRaises(lib.RegionDataError):
            lib.validate_polygon([l_shape, hole], "t")

    def test_empty_polygon_is_rejected(self):
        with self.assertRaises(lib.RegionDataError):
            lib.validate_polygon([], "t")

    def code_of(self, polygon):
        with self.assertRaises(lib.GeometryError) as cm:
            lib.validate_polygon(polygon, "t")
        return cm.exception.code

    def test_hole_poking_out_of_an_l_shaped_outer_ring_is_rejected(self):
        # 頂点の半数以上が内側でも、辺が外周を横切って凹部へはみ出す穴（以前は受理されていた）
        l_shape = [(0.0, 0.0), (10.0, 0.0), (10.0, 2.0), (2.0, 2.0), (2.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
        hole = [(1.0, 1.0), (1.0, 8.0), (6.0, 6.0), (1.0, 1.0)]
        self.assertEqual(self.code_of([l_shape, hole]), "ring_intersection")

    def test_hole_with_every_vertex_inside_but_an_edge_across_the_notch_is_rejected(self):
        l_shape = [(0.0, 0.0), (10.0, 0.0), (10.0, 2.0), (2.0, 2.0), (2.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
        hole = [(1.0, 1.0), (9.0, 1.0), (1.0, 9.0), (1.0, 1.0)]  # 3 頂点とも L の内側。斜辺は凹部を横切る
        idx = lib.RingIndex(l_shape)
        for x, y in hole[:3]:
            self.assertEqual(lib._classify(idx, x, y), 1)
        self.assertEqual(self.code_of([l_shape, hole]), "ring_intersection")

    def test_holes_that_cross_each_other_are_rejected(self):
        a = [(2.0, 2.0), (2.0, 6.0), (6.0, 6.0), (6.0, 2.0), (2.0, 2.0)]
        b = [(4.0, 4.0), (4.0, 8.0), (8.0, 8.0), (8.0, 4.0), (4.0, 4.0)]
        self.assertEqual(self.code_of([SQUARE, a, b]), "ring_intersection")

    def test_nested_and_identical_holes_are_rejected(self):
        big = [(2.0, 2.0), (2.0, 8.0), (8.0, 8.0), (8.0, 2.0), (2.0, 2.0)]
        small = [(4.0, 4.0), (4.0, 6.0), (6.0, 6.0), (6.0, 4.0), (4.0, 4.0)]
        self.assertEqual(self.code_of([SQUARE, big, small]), "holes_overlap")
        self.assertEqual(self.code_of([SQUARE, small, big]), "holes_overlap")
        self.assertEqual(self.code_of([SQUARE, small, list(small)]), "ring_intersection")

    def test_separate_and_touching_holes_are_fine(self):
        a = [(2.0, 2.0), (2.0, 4.0), (4.0, 4.0), (4.0, 2.0), (2.0, 2.0)]
        b = [(6.0, 6.0), (6.0, 8.0), (8.0, 8.0), (8.0, 6.0), (6.0, 6.0)]
        c = [(4.0, 4.0), (4.0, 6.0), (6.0, 5.0), (4.0, 4.0)]  # a と頂点 (4,4) だけで接する
        lib.validate_polygon([SQUARE, a, b], "t")
        lib.validate_polygon([SQUARE, a, c], "t")

    def test_self_intersecting_ring_is_rejected(self):
        bow = [(0.0, 0.0), (4.0, 4.0), (0.0, 4.0), (3.0, 0.0), (0.0, 0.0)]  # 以前は受理されていた
        self.assertEqual(self.code_of([bow]), "self_intersection")
        # 8 の字（対角の辺どうしが辺の内側で交差）
        eight = [(0.0, 0.0), (4.0, 4.0), (4.0, 0.0), (0.0, 5.0), (0.0, 0.0)]
        self.assertEqual(self.code_of([eight]), "self_intersection")

    def test_ring_that_doubles_back_or_overlaps_itself_is_rejected(self):
        spike = [(0.0, 0.0), (4.0, 0.0), (2.0, 0.0), (2.0, 3.0), (0.0, 0.0)]  # (4,0)→(2,0) は折り返し
        self.assertEqual(self.code_of([spike]), "self_intersection")
        # 隣り合わない辺が一直線上で重なる
        overlap = [(0.0, 0.0), (6.0, 0.0), (6.0, 3.0), (4.0, 3.0), (4.0, 0.0), (2.0, 0.0), (2.0, 5.0), (0.0, 5.0), (0.0, 0.0)]
        self.assertEqual(self.code_of([overlap]), "self_intersection")

    def test_ring_touching_itself_at_a_point_is_fine(self):
        # 頂点どうしの接触（くびれ）は許す。偶奇判定は変わらない
        pinch = [(0.0, 0.0), (4.0, 0.0), (2.0, 2.0), (4.0, 4.0), (0.0, 4.0), (2.0, 2.0), (0.0, 0.0)]
        lib.validate_polygon([pinch], "t")

    def test_ring_with_repeated_points_is_fine(self):
        lib.validate_polygon([[(0.0, 0.0), (10.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0), (0.0, 10.0), (0.0, 0.0)]], "t")

    def test_a_tiny_edge_crossing_a_very_long_edge_is_found(self):
        # 格子の細かさは辺の平均の長さで決まるので、世界を横切る長い辺と細かい辺の組でも見落とさない
        top = [(80.0 - 160.0 * i / 2000.0, 10.0) for i in range(2001)]
        top[1000] = (0.0, -1.0)  # 下辺 (y=0) を突き破る
        ring = [(-80.0, 0.0), (80.0, 0.0)] + top + [(-80.0, 0.0)]
        self.assertEqual(self.code_of([ring]), "self_intersection")


def brute_force_hits(polygon):
    rings = [lib._dedupe_ring(r) for r in polygon]
    segs = [(ri, i, r[i], r[i + 1]) for ri, r in enumerate(rings) for i in range(len(r) - 1)]
    sizes = [len(r) - 1 for r in rings]
    hits = 0
    for a in range(len(segs)):
        for b in range(a + 1, len(segs)):
            r1, i1, p, q = segs[a]
            r2, i2, c, d = segs[b]
            if r1 == r2 and abs(i1 - i2) in (1, sizes[r1] - 1):
                continue
            hits += bool(lib.segment_hit(p, q, c, d))
    return hits


class Intersections(unittest.TestCase):
    def test_orient_is_exact_for_nearly_collinear_points(self):
        rng = random.Random(7)
        for _ in range(2000):
            ax, ay = rng.uniform(-1, 1), rng.uniform(-1, 1)
            t = rng.uniform(1.5, 40)
            bx, by = ax + t * 0.5, ay + t * 0.25
            # c をほぼ a-b の延長上に置き、1〜数 ulp だけずらす
            cx = ax + (bx - ax) * 3.0 + rng.choice([0.0, 1e-15, -1e-15, 4e-16])
            cy = ay + (by - ay) * 3.0 + rng.choice([0.0, 1e-15, -1e-15, 4e-16])
            fa, fb, fc = (Fraction(ax), Fraction(ay)), (Fraction(bx), Fraction(by)), (Fraction(cx), Fraction(cy))
            exact = (fb[0] - fa[0]) * (fc[1] - fa[1]) - (fb[1] - fa[1]) * (fc[0] - fa[0])
            self.assertEqual(lib.orient(ax, ay, bx, by, cx, cy), (exact > 0) - (exact < 0))

    def test_segment_hit_kinds(self):
        self.assertEqual(lib.segment_hit((0, 0), (4, 4), (0, 4), (4, 0)), 1)  # X 字
        self.assertEqual(lib.segment_hit((0, 0), (4, 0), (2, 0), (6, 0)), 2)  # 一直線上で重なる
        self.assertEqual(lib.segment_hit((0, 0), (4, 0), (4, 0), (6, 0)), 0)  # 端点だけ接する
        self.assertEqual(lib.segment_hit((0, 0), (4, 0), (2, 0), (2, 3)), 0)  # T 字（端点が辺に乗る）
        self.assertEqual(lib.segment_hit((0, 0), (4, 0), (0, 1), (4, 1)), 0)  # 平行
        self.assertEqual(lib.segment_hit((0, 0), (1, 1), (2, 2), (3, 3)), 0)  # 同じ直線上で離れている

    def test_grid_search_agrees_with_brute_force(self):
        rng = random.Random(1)
        for _ in range(300):
            n = rng.randint(4, 40)
            g = rng.choice([3, 6, 50])
            pts = [(float(rng.randint(0, g)), float(rng.randint(0, g))) for _ in range(n)]
            pts.append(pts[0])
            hits = lib.find_intersections([pts], limit=10**9)
            # 折り返し（backtrack）は隣り合う辺の組なので、総当たりの対象外。数えるのは交差と重なりだけ
            pairs = sum(1 for kind, *_ in hits if kind != "backtrack")
            self.assertEqual(pairs, brute_force_hits([pts]), pts)

    def test_large_ring_finishes_in_practical_time(self):
        # 頂点 12 万の輪（細かくぎざぎざした円。交差しない）。素朴な総当たり（約 72 億組）なら終わらない大きさ
        n = 120_000
        ring = []
        for i in range(n):
            a = 2 * math.pi * i / n
            r = 50.0 + (0.001 if i % 2 else 0.0)
            ring.append((r * math.cos(a), r * math.sin(a)))
        ring.append(ring[0])
        started = time.monotonic()
        self.assertEqual(lib.find_intersections([ring]), [])
        self.assertLess(time.monotonic() - started, 60.0)


class PointInPolygon(unittest.TestCase):
    def test_hole_and_island(self):
        idx = lib.PolygonIndex([SQUARE, HOLE])
        self.assertTrue(idx.contains(1.0, 1.0))
        self.assertFalse(idx.contains(5.0, 5.0))  # 穴
        self.assertFalse(idx.contains(11.0, 5.0))
        self.assertFalse(idx.contains(5.0, 11.0))

    def test_interior_point_is_inside(self):
        c_shape = [(0.0, 0.0), (10.0, 0.0), (10.0, 3.0), (3.0, 3.0), (3.0, 7.0), (10.0, 7.0), (10.0, 10.0), (0.0, 10.0), (0.0, 0.0)]
        for polygon in ([SQUARE], [SQUARE, HOLE], [c_shape]):
            idx = lib.PolygonIndex(polygon)
            x, y = idx.interior_point()
            self.assertTrue(idx.contains(x, y), (polygon, x, y))

    def test_long_edges_span_several_bands(self):
        tall = [(0.0, 0.0), (1.0, 0.0), (1.0, 50.0), (0.0, 50.0), (0.0, 0.0)]
        idx = lib.PolygonIndex([tall])
        self.assertTrue(idx.contains(0.5, 25.5))
        self.assertFalse(idx.contains(1.5, 25.5))


class Codec(unittest.TestCase):
    def features(self):
        countries = [
            {"id": "ne:AAA", "polygons": [[SQUARE, HOLE], [[(20.0, 20.0), (21.0, 20.0), (21.0, 21.0), (20.0, 20.0)]]]},
            {"id": "ne:BBB", "polygons": [[[(-179.999999999, -89.5), (179.5, -89.5), (179.5, 89.0), (-179.999999999, -89.5)]]]},
        ]
        regions = [
            {"id": "ne:AAA-1", "country": 0, "polygons": [[SQUARE]]},
            {"id": "ne:BBB-X00", "country": 1, "polygons": [[[(0.1, 0.2), (0.30000000000000004, 0.2), (0.1, 0.4), (0.1, 0.2)]]]},
        ]
        return countries, regions

    def test_round_trip_is_exact(self):
        countries, regions = self.features()
        raw = lib.encode_polygons(countries, regions)
        got_c, got_r = lib.decode_polygons(raw)
        self.assertEqual([c["polygons"] for c in got_c], [c["polygons"] for c in countries])
        self.assertEqual([r["polygons"] for r in got_r], [r["polygons"] for r in regions])
        self.assertEqual([r["country"] for r in got_r], [0, 1])
        self.assertEqual([c["id"] for c in got_c], ["ne:AAA", "ne:BBB"])

    def test_extreme_values_survive(self):
        values = [0.0, -0.0, 5e-324, -5e-324, 1.7976931348623157e308, -1.7976931348623157e308, 1e-300, 34.24835085700005]
        ring = [(v, w) for v, w in zip(values, reversed(values))]
        ring.append(ring[0])
        raw = lib.encode_polygons([{"id": "ne:X", "polygons": [[ring]]}], [])
        got = lib.decode_polygons(raw)[0][0]["polygons"][0][0]
        for (ax, ay), (bx, by) in zip(ring, got):
            self.assertEqual(lib._bits(ax), lib._bits(bx))
            self.assertEqual(lib._bits(ay), lib._bits(by))

    def test_rejects_corrupt_data(self):
        countries, regions = self.features()
        raw = lib.encode_polygons(countries, regions)
        for label, data in {
            "magic": b"XXXXXXXX" + raw[8:],
            "途中で終わる": raw[:-3],
            "余り": raw + b"\x00",
            "空": b"",
        }.items():
            with self.subTest(label), self.assertRaises(lib.RegionDataError):
                lib.decode_polygons(data)


class Names(unittest.TestCase):
    def test_clean_name(self):
        self.assertEqual(lib.clean_name(None), "")
        self.assertEqual(lib.clean_name("  東京都 "), "東京都")
        self.assertEqual(lib.clean_name("-99"), "")
        self.assertEqual(lib.clean_name(-99), "")

    def test_display_name_fallback_order(self):
        self.assertEqual(lib.display_name("日本", "Japan", "にほん"), "日本")
        self.assertEqual(lib.display_name("", "Japan", "にほん"), "Japan")
        self.assertEqual(lib.display_name("", "", "にほん"), "にほん")
        self.assertEqual(lib.display_name("", "", ""), "")

    def test_aliases_are_deduplicated_and_cleaned(self):
        out = lib.dedupe(["東京都", "Tokyo", "", "Tokyo", "Edo", "a\x00b", "bad�name"])
        self.assertEqual(out, ["東京都", "Tokyo", "Edo"])
        self.assertEqual(lib.split_alt("Edo|Yedo| Tokio "), ["Edo", "Yedo", "Tokio"])
        self.assertEqual(lib.split_alt(None), [""])

    def test_id_pattern(self):
        for ok in ("ne:JPN", "ne:JPN-1860", "ne:MCO-X00", "a_b-c:1"):
            self.assertTrue(lib.ID_RE.match(ok), ok)
        for bad in ("", "ne:GAZ+00?", "ne:a b", "ne:" + "x" * 62, "ne:日本", "ne:a\n"):
            self.assertFalse(lib.ID_RE.match(bad), bad)

    def test_iso_3166_2_pattern(self):
        self.assertTrue(lib.ISO_3166_2_RE.match("JP-13"))
        self.assertTrue(lib.ISO_3166_2_RE.match("US-MN"))
        self.assertFalse(lib.ISO_3166_2_RE.match("AQ-X01~"))


class Tables(unittest.TestCase):
    def test_miharashi_overrides(self):
        got = json.loads((TOOL / "tables" / "country_names_ja.json").read_text(encoding="utf-8"))["overrides"]
        # miharashi の tools/world_geojson_to_txt.py の NAME_OVERRIDES
        self.assertEqual(got, {"TWN": "台湾", "USA": "アメリカ", "GBR": "イギリス", "KOR": "韓国", "PRK": "北朝鮮"})

    def test_japan_prefecture_table(self):
        rows = json.loads((TOOL / "tables" / "japan_prefectures.json").read_text(encoding="utf-8"))["prefectures"]
        self.assertEqual(len(rows), 47)
        self.assertEqual([r["iso_3166_2"] for r in rows], [f"JP-{i:02d}" for i in range(1, 48)])
        self.assertEqual(len({r["adm1_code"] for r in rows}), 47)
        self.assertEqual(len({r["pref"] for r in rows}), 47)
        self.assertEqual(rows[0]["pref"], "北海道")
        self.assertEqual(rows[12]["pref"], "東京都")
        self.assertEqual(rows[25]["pref"], "京都府")
        self.assertEqual(rows[26]["pref"], "大阪府")
        self.assertEqual(rows[46]["pref"], "沖縄県")
        for r in rows:
            self.assertRegex(r["pref"], r"^(北海道|.+[都道府県])$")

    def test_id_resolution_targets_are_valid_and_distinct(self):
        t = json.loads((TOOL / "tables" / "id_resolution.json").read_text(encoding="utf-8"))
        targets = list(t["admin1_id"].values())
        self.assertEqual(len(set(targets)), len(targets))
        for source, target in t["admin1_id"].items():
            self.assertFalse(lib.ID_RE.match("ne:" + source), f"{source} は対応表が要らない")
            self.assertTrue(lib.ID_RE.match("ne:" + target), target)
        self.assertFalse(set(t["admin1_id"]) & set(t["admin1_exclude"]["codes"]))

    def test_check_japan_rejects_incomplete_tables(self):
        rows = json.loads((TOOL / "tables" / "japan_prefectures.json").read_text(encoding="utf-8"))["prefectures"]
        by_code = {r["adm1_code"]: r for r in rows}
        regions = [{"id": f"ne:{r['adm1_code']}", "names": {"country": "ne:JPN", "pref": r["pref"]}} for r in rows]
        update_regions.check_japan(rows, by_code, set(by_code), regions)  # 47 件そろえば通る
        with self.assertRaises(lib.RegionDataError):
            update_regions.check_japan(rows[:46], {r["adm1_code"]: r for r in rows[:46]}, {r["adm1_code"] for r in rows[:46]}, regions[:46])
        with self.assertRaises(lib.RegionDataError):  # 使われない都道府県
            update_regions.check_japan(rows, by_code, set(list(by_code)[:-1]), regions)
        extra = regions + [{"id": "ne:JPN-9999", "names": {"country": "ne:JPN"}}]
        with self.assertRaises(lib.RegionDataError):  # 対応しない日本の Admin1
            update_regions.check_japan(rows, by_code, set(by_code), extra)


def square_feature(fid, x0, y0, size=10.0):
    ring = [(x0, y0), (x0 + size, y0), (x0 + size, y0 + size), (x0, y0 + size), (x0, y0)]
    return {"id": fid, "polygons": [[ring]]}


class CrossReference(unittest.TestCase):
    def built(self, regions):
        countries = [square_feature("ne:AAA", 0.0, 0.0), square_feature("ne:BBB", 20.0, 0.0)]
        return {"countries": countries, "regions": regions}

    def region(self, rid, x0, declared, parent, reassigned):
        r = square_feature(rid, x0, 2.0, 4.0)
        r.update(
            {
                "names": {"country": parent, "name": rid},
                "declared": declared,
                "reassigned": reassigned,
            }
        )
        return r

    def test_known_mismatch_is_listed_but_does_not_stop(self):
        built = self.built([self.region("ne:AAA-1", 22.0, "AAA", "ne:AAA", False)])  # B の中にある
        mismatches, lines = update_regions.cross_reference(built)
        self.assertEqual([m["region"] for m in mismatches], ["ne:AAA-1"])
        self.assertTrue(any("ne:AAA-1" in line for line in lines))

    def test_reassigned_region_must_land_in_its_new_country(self):
        ok = self.built([self.region("ne:ZZZ-X00", 2.0, "ZZZ", "ne:AAA", True)])
        mismatches, lines = update_regions.cross_reference(ok)
        self.assertEqual(mismatches, [])
        self.assertFalse(any("ne:ZZZ-X00" in line for line in lines))
        # 付け替え先ではない国に入るなら、既知の食い違いとしては扱わず止める
        wrong = self.built([self.region("ne:ZZZ-X00", 22.0, "ZZZ", "ne:AAA", True)])
        with self.assertRaisesRegex(lib.RegionDataError, "ZZZ→ne:AAA"):
            update_regions.cross_reference(wrong)
        # どの国にも入らない（海上）も、付け替えでは不一致
        nowhere = self.built([self.region("ne:ZZZ-X00", 12.0, "ZZZ", "ne:AAA", True)])
        with self.assertRaises(lib.RegionDataError):
            update_regions.cross_reference(nowhere)


class CheckCommittedArtifacts(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.out = Path(self.tmp.name)
        for name in ("manifest.json", "polygons.bin.gz", "names.json.gz"):
            (self.out / name).write_bytes((DATA / name).read_bytes())
        self.manifest = json.loads((self.out / "manifest.json").read_text(encoding="utf-8"))

    def test_the_committed_artifacts_verify(self):
        raws = update_regions.verify_committed(self.out, self.manifest)
        self.assertEqual(set(raws), {"polygons", "names"})

    def test_a_corrupted_gzip_is_not_reported_as_a_zlib_difference(self):
        path = self.out / "names.json.gz"
        data = bytearray(path.read_bytes())
        data[len(data) // 2] ^= 0xFF
        path.write_bytes(bytes(data))
        with self.assertRaisesRegex(SystemExit, "names.json.gz"):
            update_regions.verify_committed(self.out, self.manifest)

    def test_a_truncated_or_replaced_gzip_is_rejected(self):
        path = self.out / "polygons.bin.gz"
        path.write_bytes(path.read_bytes()[:-10])
        with self.assertRaisesRegex(SystemExit, "polygons.bin.gz"):
            update_regions.verify_committed(self.out, self.manifest)

    def test_a_valid_gzip_with_other_content_is_rejected_even_if_the_manifest_is_edited_to_match(self):
        # 圧縮後のサイズと SHA256 を manifest に合わせても、展開後の SHA256 が違えば拒否する
        gz = update_regions.gz(b"other content\n")
        (self.out / "names.json.gz").write_bytes(gz)
        art = self.manifest["artifacts"]["names"]
        art["bytes"], art["sha256"] = len(gz), update_regions.sha256_hex(gz)
        with self.assertRaisesRegex(SystemExit, "展開後"):
            update_regions.verify_committed(self.out, self.manifest)

    def test_a_bomb_beyond_the_expansion_limit_is_rejected(self):
        gz = update_regions.gz(b"\0" * 4_000_000)
        (self.out / "names.json.gz").write_bytes(gz)
        art = self.manifest["artifacts"]["names"]
        art["bytes"], art["sha256"] = len(gz), update_regions.sha256_hex(gz)
        with self.assertRaisesRegex(SystemExit, "上限"):
            update_regions.verify_committed(self.out, self.manifest)

    def test_a_missing_artifact_is_rejected(self):
        (self.out / "polygons.bin.gz").unlink()
        with self.assertRaisesRegex(SystemExit, "無い"):
            update_regions.verify_committed(self.out, self.manifest)

    def test_existing_gzip_is_kept_only_when_it_expands_to_the_same_content(self):
        raws = update_regions.verify_committed(self.out, self.manifest)
        other_level = gzip.compress(raws["names"], compresslevel=1, mtime=0)
        self.assertNotEqual(other_level, update_regions.gz(raws["names"]))
        (self.out / "names.json.gz").write_bytes(other_level)
        blobs = {
            "names_raw": raws["names"],
            "names_gz": update_regions.gz(raws["names"]),
            "polygons_raw": raws["polygons"],
            "polygons_gz": update_regions.gz(raws["polygons"]),
        }
        kept = update_regions.keep_existing_gz(blobs, self.out)
        self.assertIn("names.json.gz", kept)
        self.assertEqual(blobs["names_gz"], other_level)
        # 内容が違えば既存を使わない
        changed = dict(blobs, names_raw=raws["names"] + b" ", names_gz=update_regions.gz(raws["names"] + b" "))
        self.assertNotIn("names.json.gz", update_regions.keep_existing_gz(changed, self.out))


class CommittedArtifacts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = json.loads((DATA / "manifest.json").read_text(encoding="utf-8"))

    def test_hashes_and_sizes_match_the_manifest(self):
        total = 0
        for art in self.manifest["artifacts"].values():
            gz = (DATA / art["file"]).read_bytes()
            self.assertEqual(len(gz), art["bytes"], art["file"])
            self.assertEqual(hashlib.sha256(gz).hexdigest(), art["sha256"], art["file"])
            self.assertLessEqual(len(gz), art["max_bytes"])
            raw = gzip.decompress(gz)
            self.assertEqual(len(raw), art["raw_bytes"])
            self.assertEqual(hashlib.sha256(raw).hexdigest(), art["raw_sha256"])
            self.assertLessEqual(len(raw), art["max_raw_bytes"])
            total += len(gz)
        self.assertLessEqual(total, self.manifest["max_total_bytes"])

    def test_sources_json_is_the_manifest_source(self):
        sources = json.loads((TOOL / "sources.json").read_text(encoding="utf-8"))
        m = self.manifest["source"]
        self.assertEqual(m["data_version"], sources["data_version"])
        self.assertEqual(m["git_commit"], sources["git"]["commit"])
        for key, layer in sources["layers"].items():
            for ext, meta in layer["files"].items():
                got = m["layers"][key]["files"][ext]
                self.assertEqual((got["bytes"], got["sha256"]), (meta["bytes"], meta["sha256"]))
                self.assertTrue(got["url"].startswith(sources["base_url"]))

    def test_decoded_polygons_match_the_counts(self):
        raw = gzip.decompress((DATA / "polygons.bin.gz").read_bytes())
        countries, regions = lib.decode_polygons(raw)
        names = json.loads(gzip.decompress((DATA / "names.json.gz").read_bytes()))
        self.assertEqual([c["id"] for c in countries], [c["id"] for c in names["countries"]])
        self.assertEqual([r["id"] for r in regions], [r["id"] for r in names["regions"]])
        for key, features in (("countries", countries), ("regions", regions)):
            counts = self.manifest["counts"][key]
            polygons = [p for f in features for p in f["polygons"]]
            self.assertEqual(len(features), counts["features"])
            self.assertEqual(len(polygons), counts["polygons"])
            self.assertEqual(sum(len(p) for p in polygons), counts["rings"])
            self.assertEqual(sum(len(p) - 1 for p in polygons), counts["holes"])
            self.assertEqual(sum(len(r) for p in polygons for r in p), counts["vertices"])
        for region, name in zip(regions, names["regions"]):
            self.assertEqual(countries[region["country"]]["id"], name["country"])

    @unittest.skipUnless(os.environ.get("REGIONS_SOURCE_DIR"), "REGIONS_SOURCE_DIR（取得済みの元データ）が無い")
    def test_bit_exact_against_the_source_shapefile(self):
        try:
            import shapefile  # noqa: F401
        except ImportError:
            self.skipTest("pyshp が無い")
        sources = json.loads((TOOL / "sources.json").read_text(encoding="utf-8"))
        built = update_regions.build(Path(os.environ["REGIONS_SOURCE_DIR"]), sources, TOOL / "tables")
        raw = gzip.decompress((DATA / "polygons.bin.gz").read_bytes())
        countries, regions = lib.decode_polygons(raw)
        for got, want in ((countries, built["countries"]), (regions, built["regions"])):
            self.assertEqual(len(got), len(want))
            for g, w in zip(got, want):
                self.assertEqual(g["id"], w["id"])
                self.assertEqual(
                    [[[(lib._bits(x), lib._bits(y)) for x, y in ring] for ring in poly] for poly in g["polygons"]],
                    [[[(lib._bits(x), lib._bits(y)) for x, y in ring] for ring in poly] for poly in w["polygons"]],
                )


if __name__ == "__main__":
    unittest.main()
