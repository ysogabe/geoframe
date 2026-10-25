"""回転API（`POST /api/admin/rotate`）のフィクスチャ。

Rust の結合テスト（crates/geoframe/tests/rotate_api.rs）が読むフィクスチャが、現行実装の出力と一致していること
（再生成: python3 tools/gen_rotate_fixture.py）と、フィクスチャが押さえている現行実装の癖を記録する。
"""
import unittest

from tests import rotate_fixture as fixture


class RotateFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = fixture.build_cases()
        cls.by_name = {c["name"]: c for c in cls.doc["cases"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.doc), "再生成: python3 tools/gen_rotate_fixture.py")

    def test_case_names_are_unique(self):
        names = [c["name"] for c in fixture.CASES]
        self.assertEqual(len(names), len(set(names)))
        for name in fixture.KNOWN_DIFFERENCES:
            self.assertIn(name, names)

    def test_four_quarter_turns_return_to_the_original_name_and_orientation(self):
        by = self.by_name
        self.assertEqual(by["cw90_fourth_returns_to_the_original_name"]["body"]["thumb"],
                         "/derivatives/drive-2.jpg")
        added = by["cw90_fourth_returns_to_the_original_name"]["files"]["added"]["derivatives/drive-2.jpg"]
        self.assertEqual((added["size"], added["corners"]),
                         ([160, 96], ["red", "green", "blue", "yellow"]))

    def test_rotation_is_clockwise(self):
        # 左上の赤は、時計回りに 90° で右上へ移る
        added = self.by_name["cw90_from_null_rotation"]["files"]["added"]["derivatives/drive-2.r90.jpg"]
        self.assertEqual((added["size"], added["corners"]),
                         ([96, 160], ["blue", "red", "yellow", "green"]))

    def test_exif_orientation_is_not_applied_and_exif_is_not_kept(self):
        added = self.by_name["exif_orientation_is_ignored_and_exif_is_dropped"]["files"]["added"][
            "derivatives/drive-6.r90.jpg"]
        # 入力は 160x96。EXIF の向き 6 を当てると 96x160 になり、さらに 90° 回すと 160x96 に戻る。当てていない
        self.assertEqual(added["size"], [96, 160])
        self.assertFalse(added["exif"])

    def test_python_quirks_recorded(self):
        by = self.by_name
        # 小数の delta は 90.0 == 90 で通り、名前と応答が小数になる
        self.assertEqual(by["delta_float_90"]["body"]["rotation"], 90.0)
        self.assertEqual(by["delta_float_90"]["body"]["thumb"], "/derivatives/drive-25.r90.0.jpg")
        # シンボリックリンクの display は辿られ、リンクが消える
        self.assertEqual(by["symlinked_display"]["status"], 200)
        self.assertEqual(by["symlinked_display"]["files"]["removed"], ["derivatives/p23.jpg"])
        # derivatives/ の外の display も回して消す
        self.assertEqual(by["display_outside_derivatives"]["files"]["removed"], ["photos/outside.jpg"])
        # 新しい名前が今の display と同じなら上書きするだけで、消さない
        same = by["new_name_equals_the_current_display"]
        self.assertEqual(same["files"]["removed"], [])
        self.assertIn("derivatives/drive-10.jpg", same["files"]["changed"])
        # 画像でない display は例外 → 500
        self.assertEqual(by["display_is_not_an_image"]["status"], 500)


if __name__ == "__main__":
    unittest.main()
