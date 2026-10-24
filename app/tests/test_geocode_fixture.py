"""地名検索（`GET /api/admin/geocode`）のフィクスチャ。

Rust のテスト（crates/geoframe/src/geocode/tests.rs）が読むフィクスチャが、現行実装の出力と一致していること
（再生成: python3 tools/gen_geocode_fixture.py）と、フィクスチャが押さえている現行実装の癖を記録する。
"""
import unittest

from tests import geocode_fixture as fixture


class GeocodeFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = fixture.build_cases()
        cls.by_name = {c["name"]: c for c in cls.doc["cases"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.doc), "再生成: python3 tools/gen_geocode_fixture.py")

    def test_case_names_are_unique(self):
        names = [c["name"] for c in fixture.CASES]
        self.assertEqual(len(names), len(set(names)))

    def test_gsi_failure_and_zero_hits_both_fall_back_to_nominatim(self):
        steps = self.by_name["gsi_failures_fall_back_to_nominatim"]["steps"]
        self.assertTrue(all([r["upstream"] for r in s["requests"]] == ["gsi", "osm"] for s in steps))
        self.assertTrue(all(len(s["body"]["items"]) == 2 for s in steps))

    def test_a_poisoned_gsi_element_empties_the_whole_gsi_result(self):
        # 真の非 dict の要素などは例外になり、集めた要素も捨てて Nominatim へ回る（Nominatim は 0 件）
        for step in self.by_name["gsi_poisoned_element_empties_the_whole_gsi_result"]["steps"][:11]:
            self.assertEqual(step["body"]["items"], [], step["get"])
            self.assertEqual([r["upstream"] for r in step["requests"]], ["gsi", "osm"], step["get"])

    def test_gsi_takes_the_first_ten_features_before_filtering(self):
        steps = self.by_name["gsi_takes_the_first_ten_before_filtering"]["steps"]
        self.assertEqual([len(s["body"]["items"]) for s in steps], [10, 6, 0])

    def test_nominatim_waits_one_second_after_the_previous_call_ended(self):
        steps = self.by_name["nominatim_waits_one_second_after_the_previous_call_ended"]["steps"]
        self.assertEqual([s["sleeps"] for s in steps],
                         [[], [1.0], [0.75], [0.25], [], [], [0.5]])

    def test_only_the_stripped_query_is_the_cache_key(self):
        steps = self.by_name["cache_key_is_the_stripped_query"]["steps"]
        self.assertIn("geocode_osm:a b", steps[0]["cache"])
        self.assertEqual(steps[1]["requests"][0]["upstream"], "gsi")
        self.assertEqual(len(steps[1]["requests"]), 1)

    def test_admin_gets_outside_the_three_reads_are_403(self):
        self.assertTrue(all(g["status"] == 403 for g in self.doc["admin_gets"]))


if __name__ == "__main__":
    unittest.main()
