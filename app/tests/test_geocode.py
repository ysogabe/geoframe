"""/admin の地名検索（地理院 → 0件なら Nominatim）のテスト。"""
import unittest
from unittest.mock import patch

from geoframe import geo

GSI_HIT = [{"geometry": {"coordinates": [139.70, 35.66]},
            "properties": {"title": "東京都渋谷区"}}]
OSM_HIT = [{"lat": "39.6550017", "lon": "66.9756954",
            "display_name": "サマルカンド, サマルカンド州, ウズベキスタン"}]


class GeocodeSearchTest(unittest.TestCase):
    def setUp(self):
        self.cache = {}
        self.calls = []
        for patcher in (
                patch.object(geo, "cache_get", lambda k, d=None: self.cache.get(k, d)),
                patch.object(geo, "cache_put", self.cache.__setitem__),
                patch.object(geo.time, "sleep", lambda s: None),
                patch.object(geo, "_nominatim_last", 0.0)):
            patcher.start()
            self.addCleanup(patcher.stop)

    def _route(self, gsi, osm):
        """URLで地理院/Nominatimを振り分けるhttp_jsonの差し替え。値が例外なら送出する"""
        def fake(url):
            self.calls.append("osm" if "nominatim" in url else "gsi")
            res = osm if "nominatim" in url else gsi
            if isinstance(res, Exception):
                raise res
            return res
        return patch.object(geo, "http_json", fake)

    def test_domestic_hit_does_not_query_nominatim(self):
        with self._route(GSI_HIT, OSM_HIT):
            items = geo.geocode_search("渋谷区")
        self.assertEqual(self.calls, ["gsi"])
        self.assertEqual(items, [{"title": "東京都渋谷区", "lat": 35.66, "lon": 139.70,
                                  "source": "gsi"}])

    def test_falls_back_to_nominatim_when_gsi_empty(self):
        with self._route([], OSM_HIT):
            items = geo.geocode_search("サマルカンド")
        self.assertEqual(self.calls, ["gsi", "osm"])
        self.assertEqual(items, [{"title": "サマルカンド, サマルカンド州, ウズベキスタン",
                                  "lat": 39.6550017, "lon": 66.9756954, "source": "osm"}])

    def test_falls_back_when_gsi_fails(self):
        with self._route(OSError("timeout"), OSM_HIT):
            items = geo.geocode_search("サマルカンド")
        self.assertEqual([i["source"] for i in items], ["osm"])

    def test_nominatim_result_is_cached_including_zero_hits(self):
        with self._route([], []):
            self.assertEqual(geo.geocode_search("存在しない地名"), [])
            self.assertEqual(geo.geocode_search("存在しない地名"), [])
        # 2回目は地理院だけ引き、Nominatimはキャッシュから返す
        self.assertEqual(self.calls, ["gsi", "osm", "gsi"])

    def test_nominatim_failure_is_not_cached(self):
        with self._route([], OSError("timeout")):
            self.assertEqual(geo.geocode_search("サマルカンド"), [])
        with self._route([], OSM_HIT):
            self.assertEqual(len(geo.geocode_search("サマルカンド")), 1)
        self.assertEqual(self.calls, ["gsi", "osm", "gsi", "osm"])

    def test_unexpected_nominatim_payload_is_not_cached(self):
        with self._route([], {"error": "rate limited"}):
            self.assertEqual(geo.geocode_search("サマルカンド"), [])
        self.assertNotIn("geocode_osm:サマルカンド", self.cache)

    def test_skips_malformed_nominatim_rows(self):
        rows = [{"lat": "abc", "lon": "1"}, {"lon": "1"}, {"lat": "95", "lon": "1"},
                *OSM_HIT]
        with self._route([], rows):
            items = geo.geocode_search("サマルカンド")
        self.assertEqual([i["lat"] for i in items], [39.6550017])

    def test_waits_between_nominatim_requests(self):
        slept = []
        with patch.object(geo.time, "sleep", slept.append), \
                patch.object(geo.time, "monotonic", lambda: 100.0), \
                patch.object(geo, "_nominatim_last", 99.6), \
                self._route([], OSM_HIT):
            geo.geocode_search("サマルカンド")
        self.assertEqual(len(slept), 1)
        self.assertAlmostEqual(slept[0], 0.6)

    def test_short_query_makes_no_request(self):
        with self._route(GSI_HIT, OSM_HIT):
            self.assertEqual(geo.geocode_search(" a "), [])
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
