"""3D 航空機レイヤーへ渡す ADS-B フィールドの互換性テスト。"""
import unittest
from unittest.mock import patch

from geoframe import adsb


class AdsbAircraftApiTest(unittest.TestCase):
    def test_optional_number_safely_normalizes_ground_missing_and_non_finite(self):
        self.assertIsNone(adsb._optional_number("ground"))
        self.assertIsNone(adsb._optional_number(None))
        self.assertIsNone(adsb._optional_number(float("nan")))
        self.assertIsNone(adsb._optional_number(float("inf")))
        self.assertEqual(adsb._optional_number("123"), 123)
        self.assertEqual(adsb._optional_number("1.25"), 1.25)

    def test_api_keeps_legacy_fields_and_adds_motion_fields_and_feed_now(self):
        feed = {
            "now": 1700000000.5,
            "aircraft": [{
                "hex": "a1b2c3", "flight": " JAL123 ",
                "lat": "35.1", "lon": "139.1", "seen_pos": "2.5",
                "alt_baro": "ground", "alt_geom": "12500",
                "track": "180.5", "true_heading": "179",
                "mag_heading": "178", "track_rate": "-1.2",
                "roll": "4.5", "geom_rate": "640", "baro_rate": "600",
                "gs": "420", "r_dst": "10", "t": "A35K",
            }],
        }

        def fake_http_json(url):
            if url == adsb.ADSB_URL:
                return feed
            if url == adsb.ADSB_RECEIVER_URL:
                return {"lat": 35.0, "lon": 139.0}
            raise AssertionError(f"unexpected URL: {url}")

        adsb._receiver = None
        with patch.object(adsb, "http_json", side_effect=fake_http_json), \
             patch.object(adsb, "get_manifest_cached", return_value=[]), \
             patch.object(adsb, "setting_get", return_value=None), \
             patch.object(adsb, "adsbdb_lookup", return_value=None), \
             patch.object(adsb.time, "time", return_value=1700000000.25):
            result = adsb.api_aircraft()

        self.assertTrue(result["ok"])
        self.assertEqual(result["now"], 1700000000.5)
        self.assertEqual(len(result["aircraft"]), 1)
        item = result["aircraft"][0]
        self.assertEqual(item["callsign"], "JAL123")
        self.assertEqual(item["alt"], "ground")
        self.assertEqual(item["alt_baro"], "ground")
        self.assertEqual(item["alt_geom"], 12500)
        self.assertEqual(item["true_heading"], 179)
        self.assertEqual(item["mag_heading"], 178)
        self.assertEqual(item["track_rate"], -1.2)
        self.assertEqual(item["roll"], 4.5)
        self.assertEqual(item["geom_rate"], 640)
        self.assertEqual(item["baro_rate"], 600)
        self.assertEqual(item["seen_pos"], 2.5)
        self.assertEqual(item["track"], 180.5)
        self.assertEqual(item["dist_km"], 18.5)

    def test_missing_optional_motion_fields_are_null_without_dropping_aircraft(self):
        feed = {"aircraft": [{
            "hex": "d4e5f6", "lat": 35.1, "lon": 139.1,
            "seen_pos": 0, "alt_baro": None, "gs": None,
        }]}

        def fake_http_json(url):
            if url == adsb.ADSB_URL:
                return feed
            if url == adsb.ADSB_RECEIVER_URL:
                return {}
            raise AssertionError(f"unexpected URL: {url}")

        adsb._receiver = None
        with patch.object(adsb, "http_json", side_effect=fake_http_json), \
             patch.object(adsb, "get_manifest_cached", return_value=[]), \
             patch.object(adsb, "setting_get", return_value=None), \
             patch.object(adsb, "adsbdb_lookup", return_value=None):
            item = adsb.api_aircraft()["aircraft"][0]

        for field in (
            "alt_geom", "true_heading", "mag_heading", "track_rate",
            "roll", "geom_rate", "baro_rate",
        ):
            self.assertIsNone(item[field], field)
        self.assertEqual(item["seen_pos"], 0)

    def test_non_finite_position_and_altitude_are_rejected_without_invalid_json(self):
        feed = {"now": float("nan"), "aircraft": [
            {"hex": "bad-pos", "lat": float("nan"), "lon": 139,
             "seen_pos": 1, "alt_baro": float("inf")},
            {"hex": "bad-alt", "lat": 35.1, "lon": 139.1,
             "seen_pos": 1, "alt_baro": float("nan"), "alt_geom": float("inf"),
             "track": float("nan"), "gs": float("inf")},
        ]}

        def fake_http_json(url):
            if url == adsb.ADSB_URL:
                return feed
            if url == adsb.ADSB_RECEIVER_URL:
                return {}
            raise AssertionError(f"unexpected URL: {url}")

        adsb._receiver = None
        with patch.object(adsb, "http_json", side_effect=fake_http_json), \
             patch.object(adsb, "get_manifest_cached", return_value=[]), \
             patch.object(adsb, "setting_get", return_value=None), \
             patch.object(adsb, "adsbdb_lookup", return_value=None), \
             patch.object(adsb.time, "time", return_value=1700000000.25):
            result = adsb.api_aircraft()

        self.assertEqual(result["now"], 1700000000.25)
        self.assertEqual(len(result["aircraft"]), 1)
        item = result["aircraft"][0]
        self.assertIsNone(item["alt"])
        self.assertIsNone(item["alt_baro"])
        self.assertIsNone(item["alt_geom"])
        self.assertEqual(item["track"], 0)
        self.assertIsNone(item["gs"])


if __name__ == "__main__":
    unittest.main()
