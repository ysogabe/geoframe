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
import sys
import unittest
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

    def test_hole_touching_the_outer_ring_is_fine(self):
        touching = [(0.0, 4.0), (0.0, 6.0), (2.0, 6.0), (2.0, 4.0), (0.0, 4.0)]
        lib.validate_polygon([SQUARE, touching], "t")

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
