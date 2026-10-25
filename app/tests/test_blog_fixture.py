"""旅行ブログの読み取りAPI（/api/blog/trips・trip・candidates・draft）のフィクスチャ。

Rust の結合テスト（crates/geoframe/tests/blog_api.rs）が読むフィクスチャが、現行実装の出力と一致していること
（再生成: python3 tools/gen_blog_fixture.py）と、フィクスチャが押さえている現行実装の癖を記録する。
"""
import unittest

from tests import blog_fixture as fixture


class BlogFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = fixture.build_cases()
        cls.by_target = {c["target"]: c for c in cls.doc["cases"]}

    def body(self, target):
        return self.by_target[target]["body"]

    def chapters(self, trip_id="sample-trip-a"):
        return {c["segment_id"]: c for c in self.body(f"/api/blog/candidates?trip_id={trip_id}")["chapters"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.doc), "再生成: python3 tools/gen_blog_fixture.py")

    def test_catalog_sql_is_up_to_date(self):
        text = fixture.CATALOG_SQL_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.build_catalog_sql(), "再生成: python3 tools/gen_blog_fixture.py")

    def test_targets_are_unique(self):
        self.assertEqual(len(fixture.TARGETS), len(set(fixture.TARGETS)))

    def test_fixture_does_not_contain_the_real_plan(self):
        # 合成の値だけ。リポジトリの実際の旅程 JSON の trip_id を載せない
        import json
        from geoframe import travel_blog  # noqa: F401 （実ファイルの場所を知るため）
        real = [json.loads(p.read_text(encoding="utf-8"))["trip_id"]
                for p in (fixture.ROOT / "app/travel_blog/plans").glob("*.json")]
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        for trip_id in real:
            self.assertNotIn(trip_id, text)

    def test_list_uses_the_trip_id_inside_the_file_and_skips_unreadable_files(self):
        ids = [i["trip_id"] for i in self.body("/api/blog/trips")["items"]]
        self.assertIn("sample-trip-c", ids)          # ファイル名は mismatch-file.json
        self.assertNotIn("mismatch-file", ids)
        self.assertIn("5", ids)                        # 数値の trip_id は文字列になる
        self.assertIn("dot-plan", ids)                 # glob はドット始まりも拾う
        for skipped in ("bad-json", "dir-plan", "Bad_ID", "notes"):
            self.assertNotIn(skipped, ids)
        # 並びはファイル名順（trip_id 順ではない）
        self.assertEqual(ids, sorted(ids, key=lambda i: {"dot-plan": ".dot-plan.json", "5": "5.json",
                                                         "sample-trip-c": "mismatch-file.json"}.get(i, i + ".json")))
        by_id = {i["trip_id"]: i for i in self.body("/api/blog/trips")["items"]}
        self.assertEqual((by_id["sample-trip-b"]["has_draft"], by_id["sample-trip-b"]["has_export"]), (True, True))
        # 書き出しのディレクトリだけ（index.html なし）は false
        self.assertEqual((by_id["sample-trip-a"]["has_draft"], by_id["sample-trip-a"]["has_export"]), (False, False))
        self.assertEqual(by_id["segments-string"]["days"], 3)    # len("abc")
        self.assertEqual(by_id["title-number"]["title"], "12")   # str(12)

    def test_load_plan_checks_file_name_and_shape(self):
        self.assertEqual(self.by_target["/api/blog/trip?trip_id=mismatch-file"]["status"], 400)
        self.assertEqual(self.by_target["/api/blog/trip?trip_id=sample-trip-c"]["status"], 404)
        self.assertEqual(self.by_target["/api/blog/trip?trip_id=5"]["status"], 400)
        self.assertEqual(self.by_target["/api/blog/trip?trip_id=dir-plan"]["body"]["error"],
                         "JSONを読み込めません: dir-plan.json")

    def test_trip_id_boundaries(self):
        status = lambda t: self.by_target[t]["status"]  # noqa: E731
        self.assertEqual(status(f"/api/blog/trip?trip_id={fixture.LONG_OK}"), 404)
        self.assertEqual(status(f"/api/blog/trip?trip_id={fixture.LONG_NG}"), 400)
        self.assertEqual(status("/api/blog/trip?trip_id=sample-trip-a%0A"), 400)
        self.assertEqual(status("/api/blog/trip?trip_id=&trip_id=sample-trip-a"), 200)
        self.assertEqual(status("/api/blog/trip?trip_id=zzz&trip_id=sample-trip-a"), 404)

    def test_date_range_is_a_string_comparison_and_not_a_time_calculation(self):
        ch = self.chapters()
        # 境目（09:00 と 18:00 を含み、08:59・18:01・秒つきを含まない）。hidden=1・display NULL は数えない
        self.assertEqual(ch["d1"]["photo_count"], 18)
        # 0 埋めのない開始/終了では、0 埋めの行が入らない
        self.assertEqual([c["photo_id"] for c in ch["d4"]["candidates"]], ["42"])
        # 終了が開始より前なら 0 件
        self.assertEqual(ch["d5"]["photo_count"], 0)
        # 日付をまたぐ: 23:59 は入り、秒つきの 23:59:30 は入らず、翌日 00:00 は翌日の区間
        self.assertEqual([c["photo_id"] for c in ch["d6"]["candidates"]], ["51", "52"])
        self.assertEqual([c["photo_id"] for c in ch["d7"]["candidates"]], ["54", "55"])
        # タイムゾーンは換算せず、そのまま返す
        self.assertEqual((ch["d6"]["timezone"], ch["d2"]["timezone"]), ("America/Sample", ""))
        # 日付がない区間は何にも当たらない
        self.assertEqual(ch["d8"]["photo_count"], 0)

    def test_groups_representatives_and_selection(self):
        ch = self.chapters()["d1"]
        top = ch["candidates"][0]
        # 同じ分の 6 枚: お気に入りの 7 が代表、alternates は 3 枚まで
        self.assertEqual((top["photo_id"], top["duplicate_count"], len(top["alternates"])), ("7", 6, 3))
        self.assertEqual(top["duplicate_group_id"], "d1-g1")
        # 36 グループの上限。お気に入りを足した結果は、時系列の先頭から 36 件に切られる
        d3 = self.chapters()["d3"]
        self.assertEqual((d3["duplicate_groups"], len(d3["candidates"])), (45, 36))
        ids = [c["photo_id"] for c in d3["candidates"]]
        self.assertEqual(ids, sorted(ids, key=int))                # 時系列の順のまま
        self.assertIn("120", ids)                                  # 途中のお気に入りは漏れない
        self.assertEqual(ids[-1], "144")
        # 初期案は 6 枚＋お気に入り
        self.assertEqual(sum(c["selected"] for c in ch["candidates"]), 6)

    def test_exact_positions_never_leave(self):
        for target, case in self.by_target.items():
            if "candidates" in target or "draft" in target:
                text = str(case["body"])
                for secret in ("IMG_", "35.1", "139.1", "drive-"):
                    self.assertNotIn(secret, text, target)

    def test_saved_draft_is_cleaned_and_does_not_need_a_plan(self):
        manifest = self.body("/api/blog/draft?trip_id=sample-trip-b")["manifest"]
        self.assertEqual(self.body("/api/blog/draft?trip_id=sample-trip-b")["source"], "saved")
        self.assertEqual((len(manifest["title"]), len(manifest["subtitle"]), len(manifest["summary"])),
                         (200, 300, 4000))
        self.assertEqual(manifest["status"], "published")       # 読むときは据え置き
        self.assertEqual(manifest["unknown"], [1, 2])            # 知らないキーは残る
        first = manifest["chapters"][0]
        self.assertEqual((len(first["id"]), first["date"], len(first["title"]), len(first["body"])),
                         (100, "2025-06-06", 200, 20000))
        self.assertEqual([i["photo_id"] for i in first["images"]], ["5", "７", "0", "12"])
        self.assertEqual(first["images"][2]["alt"], "旅の写真")
        self.assertEqual(self.by_target["/api/blog/draft?trip_id=draft-without-plan"]["body"]["source"], "saved")
        self.assertEqual(self.by_target["/api/blog/draft?trip_id=sample-trip-a"]["body"]["source"], "generated")

    def test_python_digit_rules(self):
        status = lambda t: self.by_target[f"/api/blog/draft?trip_id={t}"]["status"]  # noqa: E731
        self.assertEqual(status("draft-photo-id-superscript"), 200)   # "²".isdigit()
        for bad in ("draft-photo-id-zero", "draft-photo-id-empty", "draft-photo-id-null",
                    "draft-photo-id-letters"):
            self.assertEqual(status(bad), 400, bad)

    def test_unknown_paths_are_404_html(self):
        for target in ("/api/blog/", "/api/blog/nope", "/api/blog/trips/", "/api/blog/save", "/api/blog/export"):
            self.assertEqual((self.by_target[target]["status"], self.by_target[target]["body"]), (404, None))
        self.assertEqual(self.by_target["/api/blogx/trips"]["status"], 403)
        self.assertEqual(self.by_target["//api/blog/trips"]["status"], 200)

    def test_odd_but_non_crashing_shapes(self):
        odd = self.chapters("sample-trip-odd")
        self.assertIn(None, odd)                                  # id なしの区間は segment_id が null
        self.assertEqual(odd[None]["location_label"], "a・b")     # 場所が文字列なら先頭 2 文字
        self.assertEqual(odd[None]["candidates"][0]["duplicate_group_id"], "None-g1")
        manifest = self.body("/api/blog/draft?trip_id=sample-trip-odd")["manifest"]
        self.assertIn("None None", manifest["chapters"][0]["body"])
        # 同じ id の区間が 2 つあると、初期案の本文は後ろの区間から作る
        dup = [c for c in manifest["chapters"] if c["id"] == "dup"]
        self.assertEqual(len(dup), 2)
        self.assertTrue(all("後の区間" in c["body"] for c in dup))
