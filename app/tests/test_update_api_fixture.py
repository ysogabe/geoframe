"""更新API（setloc・setflag）と POST の入口（CSRF対策・ルーティング）のフィクスチャ。

Rust の結合テスト（crates/geoframe/tests/update_api.rs）が読むフィクスチャが、現行実装の出力と
一致していること（再生成: python3 tools/gen_update_fixture.py）と、フィクスチャが押さえている
現行実装の癖を記録する。
"""
import unittest

from tests import update_api_fixture as fixture


class UpdateApiFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = fixture.build_cases()
        cls.by_name = {c["name"]: c for c in cls.cases["cases"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(
            text, fixture.render(self.cases),
            "再生成: python3 tools/gen_update_fixture.py")

    def test_case_names_are_unique(self):
        names = [c["name"] for c in fixture.CASES]
        self.assertEqual(len(names), len(set(names)))
        for name in fixture.KNOWN_DIFFERENCES:
            self.assertIn(name, names)

    def test_order_of_checks(self):
        by = self.by_name
        # ルート無しは Content-Type・Content-Length を見る前に 403（本文は http.server の HTML）
        for name in ("route_unknown_admin", "route_unknown_blog",
                     "route_unknown_with_bad_content_type", "route_unknown_without_length"):
            self.assertEqual((by[name]["status"], by[name]["body"]), (403, None), name)
        # Content-Type → Origin → Content-Length → 413 → JSON → オブジェクト
        self.assertIn("Content-Type", by["order_bad_content_type_before_bad_origin"]["body"]["error"])
        self.assertIn("クロスオリジン", by["order_bad_origin_before_bad_length"]["body"]["error"])
        self.assertEqual(by["order_bad_origin_before_big_body"]["status"], 403)
        self.assertEqual(by["length_missing"]["status"], 400)
        self.assertEqual(by["length_big_before_json_check"]["status"], 413)
        self.assertEqual(by["body_not_json"]["status"], 400)
        self.assertEqual(by["body_array"]["status"], 400)

    def test_unknown_blog_post_is_403_not_404(self):
        # 依頼の文面では「/api/blog/ 配下の未知パスだけ 404」だが、現行の do_POST は GET と違って
        # 未知のパスを一律 403 にする（404 は do_GET の /api/blog/ 配下だけ）
        self.assertEqual(self.by_name["route_unknown_blog"]["status"], 403)

    def test_body_limit_is_inclusive(self):
        self.assertEqual(self.by_name["body_exactly_limit_ok"]["status"], 200)
        self.assertEqual(self.by_name["body_one_over_limit"]["status"], 413)

    def test_python_quirks_recorded(self):
        by = self.by_name
        # NaN・Infinity は json.loads が受け付け、範囲外の 400 になる
        self.assertEqual(by["setloc_nan_lat"]["status"], 400)
        self.assertIn("範囲外", by["setloc_nan_lat"]["body"]["error"])
        # i64 を超える id は SQLite に渡せず 500
        self.assertEqual(by["setloc_id_beyond_i64"]["status"], 500)
        self.assertEqual(by["setloc_id_beyond_i64"]["body"]["error"], "内部エラーが発生しました")
        # パスの \ は / に読み替えられて当たる
        self.assertEqual(by["route_backslash"]["status"], 200)
        # 空の Origin は検査されない
        self.assertEqual(by["origin_empty_passes"]["status"], 200)
        # Origin の host は大文字小文字を区別して Host と比べる
        self.assertEqual(by["origin_host_uppercase"]["status"], 403)

    def test_setloc_changes_the_python_columns(self):
        changes = self.by_name["setloc_on_row_with_place"]["changes"]["1"]
        self.assertEqual(
            changes,
            {"lat": -33.5, "lon": -70.25, "loc_source": "manual", "place": None, "pref": None,
             "city": None})
        inferred = self.by_name["setloc_on_inferred_clears_inference"]["changes"]["6"]
        self.assertEqual(inferred["infer_dt_s"], None)
        self.assertEqual(inferred["loc_source"], "manual")
        # display・favorite・hidden・rotation は変わらない
        for column in ("display", "favorite", "hidden", "rotation"):
            self.assertNotIn(column, inferred)
