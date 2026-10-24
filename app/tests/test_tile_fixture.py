"""地図タイルのプロキシ（`GET/HEAD /tiles/…`）と掃除のフィクスチャ。

Rust のテスト（crates/geoframe/src/tiles/tests.rs）が読むフィクスチャが、現行実装の出力と一致していること
（再生成: python3 tools/gen_tile_fixture.py）と、フィクスチャが押さえている現行実装の癖を記録する。
"""
import unittest

from tests import tile_fixture as fixture


class TileFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = fixture.build_cases()
        cls.by_name = {c["name"]: c for c in cls.doc["cases"]}

    def statuses(self, name):
        return [(s["status"], s["headers"].get("x-tile-cache")) for s in self.by_name[name]["steps"]]

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.doc), "再生成: python3 tools/gen_tile_fixture.py")

    def test_case_names_are_unique(self):
        for cases in (fixture.CASES, fixture.PRUNE_CASES):
            names = [c["name"] for c in cases]
            self.assertEqual(len(names), len(set(names)))

    def test_redirects_stop_after_a_limit_and_throttled_requests_never_reach_the_upstream(self):
        steps = self.by_name["redirects_are_followed_like_urllib"]["steps"]
        self.assertEqual(steps[-2]["headers"]["x-tile-cache"], "miss")
        self.assertEqual(steps[-1]["headers"]["x-tile-cache"], "error")
        self.assertLess(len(steps[-1]["requests"]), 30)
        for case in self.by_name.values():
            for s in case["steps"]:
                if s["headers"].get("x-tile-cache") == "throttled":
                    self.assertEqual(s["requests"], [])

    def test_the_upstream_sees_only_the_expected_headers(self):
        for step in self.by_name["upstream_request_has_only_the_expected_headers"]["steps"]:
            for request in step["requests"]:
                self.assertEqual(request["headers"], {
                    "user-agent": fixture.TEST_UA, "accept": "image/png",
                    "accept-encoding": "identity", "connection": "close"})

    def test_missing_tiles_are_cached_as_transparent_but_other_failures_are_not(self):
        steps = self.by_name["upstream_404_is_cached_as_a_transparent_tile"]["steps"]
        self.assertEqual([s["headers"]["x-tile-cache"] for s in steps], ["miss", "hit", "miss", "hit"])
        for s in self.by_name["other_upstream_statuses_are_errors_and_not_cached"]["steps"][:11]:
            self.assertEqual(s["headers"]["x-tile-cache"], "error")
            self.assertEqual(s["headers"]["cache-control"], "no-store")

    def test_only_the_png_magic_is_checked_never_the_content_type(self):
        steps = self.by_name["bodies_that_are_not_png_are_errors"]["steps"]
        self.assertEqual([s["headers"]["x-tile-cache"] for s in steps[-6:]],
                         ["miss", "hit", "miss", "hit", "miss", "miss"])

    def test_head_does_the_whole_fetch(self):
        steps = self.by_name["head_does_the_same_work_without_a_body"]["steps"]
        self.assertEqual(steps[0]["requests"][0]["path"], "/osm/4/3/3.png")
        self.assertEqual(steps[0]["headers"]["x-tile-cache"], "miss")
        self.assertEqual(steps[0]["body_len"], 0)
        self.assertEqual(steps[2]["body_len"], 268)           # 同じ長さが GET では本文になる
        self.assertEqual(steps[0]["headers"]["content-length"], "268")

    def test_the_window_counts_failures_and_head_but_not_hits_or_404_paths(self):
        steps = self.by_name["failures_and_missing_tiles_consume_the_window_but_hits_and_bad_paths_do_not"]["steps"]
        self.assertEqual([s["headers"].get("x-tile-cache") for s in steps],
                         ["error", "miss", "throttled", "hit", "hit", None, "throttled", "miss", "hit"])

    def test_the_window_slides(self):
        steps = self.by_name["the_window_slides_over_the_last_60_seconds"]["steps"]
        self.assertEqual([s["headers"]["x-tile-cache"] for s in steps],
                         ["miss", "miss", "miss", "throttled", "hit", "throttled", "miss", "throttled", "miss", "miss"])

    def test_paths_are_judged_raw_not_normalized(self):
        steps = self.by_name["other_prefixes_never_reach_the_tile_proxy"]["steps"]
        self.assertTrue(all(s["status"] == 403 for s in steps))
        steps = self.by_name["paths_that_do_not_match_are_404"]["steps"]
        self.assertTrue(all(s["status"] == 404 for s in steps))

    def test_prune_cases(self):
        by = {c["name"]: c for c in self.doc["prune_cases"]}
        self.assertEqual(len(by["under_the_limit_keeps_everything"]["survivors"]), 2)
        # 上限ちょうどは消さず、1 バイト超えると 90% 以下になるまで古い順に消す
        self.assertEqual(len(by["exactly_at_the_limit_keeps_everything"]["survivors"]), 2)
        self.assertEqual(by["one_byte_over_removes_the_oldest_until_90_percent"]["survivors"], ["osm/5/2/3.png"])
        # mtime が同じなら大きさ、さらに同じならパスの文字列順（"10" は "9" より前）
        self.assertEqual(by["ties_go_by_size_then_path_string"]["survivors"],
                         ["osm/9/1/1.png", "osm/9/1/10.png", "osm/9/1/3.png", "osm/9/1/9.png"])
        self.assertEqual(by["zero_limit_removes_everything"]["survivors"], [])


if __name__ == "__main__":
    unittest.main()
