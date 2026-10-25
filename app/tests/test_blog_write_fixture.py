"""旅行ブログの保存と書き出し（POST /api/blog/save・export）のフィクスチャ。

Rust の結合テスト（crates/geoframe/tests/blog_write_api.rs）が読むフィクスチャが、現行実装の出力と一致していること
（再生成: python3 tools/gen_blog_write_fixture.py）と、フィクスチャが押さえている現行実装の癖を記録する。
"""
import json
import unittest

from tests import blog_write_fixture as fixture


class BlogWriteFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = fixture.build_cases()
        cls.by_name = {c["name"]: c for c in cls.doc["cases"]}

    def draft(self, name, trip_id="sample-trip-a"):
        files = self.by_name[name]["files"]
        entry = {**files["added"], **files["changed"]}[f"travel_blog/drafts/{trip_id}.json"]
        return entry["text"]

    def export_files(self, name, trip_id="sample-trip-a"):
        files = self.by_name[name]["files"]
        prefix = f"blog_exports/{trip_id}/"
        return {k[len(prefix):]: v for k, v in {**files["added"], **files["changed"]}.items()
                if k.startswith(prefix)}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.doc), "再生成: python3 tools/gen_blog_write_fixture.py")

    def test_case_names_are_unique_and_known_differences_exist(self):
        names = [c["name"] for c in fixture.CASES]
        self.assertEqual(len(names), len(set(names)))
        for name in fixture.KNOWN_DIFFERENCES:
            self.assertIn(name, names)

    def test_fixture_does_not_contain_the_real_plan(self):
        # 合成の値だけ。リポジトリの実際の旅程 JSON の trip_id を載せない（失敗してもその値を出さない）
        real = [json.loads(p.read_text(encoding="utf-8"))["trip_id"]
                for p in (fixture.ROOT / "app/travel_blog/plans").glob("*.json")]
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        for trip_id in real:
            self.assertFalse(trip_id in text, "fixture に実際の旅程の識別子が含まれている")

    def test_saved_draft_is_python_json_with_a_newline_in_insertion_order(self):
        text = self.draft("save_two_chapters_with_extra_keys_and_floats")
        self.assertTrue(text.endswith("}\n"))
        self.assertLess(text.index('"schema_version"'), text.index('"z_last"'))
        self.assertLess(text.index('"z_last"'), text.index('"a_first"'))      # 辞書順ではなく挿入順
        self.assertIn('"empty": []', text)
        self.assertIn('"a": {}', text)
        floats = self.draft("save_float_and_integer_forms")
        for expected in ('"a": 1e-05', '"b": 1.0', '"c": 100.0', '"d": 1e+22', '"f": -0.0',
                         '"g": 12345678901234567890', '"h": 9007199254740993', '"i": 100000.0',
                         '"j": 0.30000000000000004', '"k": 1.5e-07', '"n": 1e+16', '"o": 1000000000000000.0',
                         '"p": 5e-324', '"q": 1.7976931348623157e+308'):
            self.assertIn(expected, floats)
        self.assertIn("日本語", self.draft("save_string_escapes_and_unicode"))
        self.assertIn(" ", self.draft("save_string_escapes_and_unicode"))       # ensure_ascii=False

    def test_duplicate_json_keys_keep_the_first_position_and_the_last_value(self):
        text = self.draft("save_duplicate_keys_keep_the_first_position")
        self.assertIn('"title": "あと"', text)
        self.assertLess(text.index('"title"'), text.index('"chapters"'))

    def test_published_is_saved_as_a_draft_and_reviewed_stays(self):
        self.assertIn('"status": "draft"', self.draft("save_published_becomes_draft"))
        self.assertIn('"status": "reviewed"', self.draft("save_reviewed_stays_reviewed"))

    def test_save_returns_the_relative_path(self):
        self.assertEqual(self.by_name["save_minimal"]["body"],
                         {"ok": True, "trip_id": "sample-trip-a", "saved": "travel_blog/drafts/sample-trip-a.json"})

    def test_a_failed_save_changes_nothing(self):
        for name, case in self.by_name.items():
            if name.startswith("save_") and case["status"] != 200:
                self.assertEqual(case["files"], {"added": {}, "removed": [], "changed": {}}, name)

    def test_export_files_and_image_properties(self):
        files = self.export_files("export_all_kinds_of_images")
        self.assertEqual(sorted(k for k in files if k != "images"), ["blog-manifest.json", "images/photo-1.jpg", "images/photo-10.jpg",
                                         "images/photo-11.jpg", "images/photo-12.jpg", "images/photo-13.jpg",
                                         "images/photo-2.jpg", "images/photo-4.jpg", "images/photo-6.jpg",
                                         "images/photo-7.jpg", "images/photo-8.jpg", "images/photo-9.jpg",
                                         "index.html", "index.md"])
        image = lambda n: files[f"images/photo-{n}.jpg"]  # noqa: E731
        self.assertEqual(image(1)["size"], [1800, 900])                  # 2400x1200 を長辺 1800 に
        self.assertEqual(image(2)["size"], [1800, 900])                  # EXIF の向き 6 を当てて 2400x1200 → 1800x900
        self.assertEqual(image(8)["size"], [1600, 1000])                 # 1000x1600 の向き 6 は縮めない
        self.assertEqual(image(4)["size"], [160, 96])                    # 小さい画像は拡大しない
        self.assertEqual(image(11)["size"], [1800, 1200])
        self.assertEqual(image(12)["size"], [1800, 999])                 # Pillow の thumbnail の丸め
        self.assertEqual(image(13)["size"], [1800, 1800])
        self.assertEqual(image(6)["components"], 3)                      # グレーも RGB で出る
        for n in (1, 2, 4, 6, 7, 8, 9, 10, 11, 12, 13):
            self.assertFalse(image(n)["exif"], n)                        # EXIF・GPS は残さない
            self.assertFalse(image(n)["icc"], n)                         # ICC も付かない
            self.assertEqual(image(n)["components"], 3, n)

    def test_export_manifest_html_and_markdown(self):
        files = self.export_files("export_html_and_markdown_escapes")
        html = files["index.html"]["text"]
        self.assertIn("&lt;h1&gt;題&lt;/h1&gt; &amp; &quot;引用&quot; &#x27;x&#x27;", html)
        self.assertNotIn("<script>alert", html)
        self.assertTrue(files["index.md"]["text"].endswith("\n"))
        self.assertFalse(files["index.md"]["text"].endswith("\n\n"))
        manifest = json.loads(files["blog-manifest.json"]["text"])
        self.assertEqual(manifest["status"], "reviewed")
        self.assertEqual(manifest["chapters"][0]["images"][0]["src"], "images/photo-1.jpg")

    def test_same_photo_is_written_once_under_the_integer_name(self):
        files = self.export_files("export_same_photo_many_times_is_one_file")
        self.assertTrue(all(k == "images/photo-1.jpg" for k in files if k.startswith("images/") and k != "images/"))
        manifest = json.loads(files["blog-manifest.json"]["text"])
        self.assertEqual({i["src"] for i in manifest["chapters"][0]["images"]}, {"images/photo-1.jpg"})

    def test_export_replaces_the_previous_export_and_leaves_no_work_area(self):
        files = self.by_name["export_replaces_the_previous_export"]["files"]
        self.assertIn("blog_exports/sample-trip-a/old.txt", files["removed"])
        self.assertIn("blog_exports/sample-trip-a/images/photo-99.jpg", files["removed"])
        for case in self.doc["cases"]:
            for path in list(case["files"]["added"]) + case["files"]["removed"]:
                self.assertNotIn(".previous", path, case["name"])
                self.assertFalse(path.split("/")[1:2] and path.split("/")[1].startswith("."), case["name"])

    def test_a_failed_export_changes_nothing(self):
        for name, case in self.by_name.items():
            if name.startswith("export_") and case["status"] != 200:
                self.assertEqual(case["files"], {"added": {}, "removed": [], "changed": {}}, name)

    def test_python_quirks_recorded(self):
        by = self.by_name
        # dry_run は truthy なら写真を見ない。重複も数える
        self.assertEqual(by["export_dry_run_counts_duplicates"]["body"]["images"], 3)
        self.assertEqual(by["export_dry_run_does_not_check_photos"]["status"], 200)
        self.assertEqual(by["export_dry_run_truthy_false_string"]["body"]["dry_run"], True)
        # 範囲外の写真・不正な表示用ファイルのメッセージ
        self.assertEqual(by["export_hidden_photo"]["body"]["error"], "写真#3の表示用画像がありません")
        self.assertEqual(by["export_display_file_is_missing"]["body"]["error"], "写真#18のファイルを確認できません")
        # 画像として読めなければ 500（定型文）
        self.assertEqual((by["export_display_is_not_an_image"]["status"],
                          by["export_display_is_not_an_image"]["body"]["error"]), (500, "内部エラーが発生しました"))
        # i64 を超える id・上付き数字は 500
        for name in fixture.KNOWN_DIFFERENCES:
            self.assertEqual(by[name]["status"], 500, name)
        # `..` を含む display も、DATA_DIR の中に解決されれば読める
        self.assertEqual(by["export_dotdot_inside_the_data_dir"]["status"], 200)
        # Unicode の数字の写真 id は int() で読める（photo-1.jpg に畳まれる）
        self.assertEqual(by["export_same_photo_many_times_is_one_file"]["status"], 200)
