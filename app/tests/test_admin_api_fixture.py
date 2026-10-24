"""管理画面の読み取りAPI（/api/admin/status・/api/admin/photos）のフィクスチャ。

Rust の結合テスト（crates/geoframe/tests/admin_api.rs）が読むフィクスチャが、現行実装の出力と
一致していること（再生成: python3 tools/gen_admin_fixture.py）と、フィクスチャが押さえている
現行実装の癖を記録する。
"""
import unittest

from tests import admin_api_fixture as fixture


class AdminApiFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = fixture.build_cases()
        cls.by_target = {c["target"]: c for c in cls.cases["photos"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(
            text, fixture.render(self.cases),
            "再生成: python3 tools/gen_admin_fixture.py")

    def test_catalog_sql_is_up_to_date(self):
        text = fixture.CATALOG_SQL_PATH.read_text(encoding="utf-8")
        self.assertEqual(
            text, fixture.build_catalog_sql(),
            "再生成: python3 tools/gen_admin_fixture.py")

    def test_prefix_forms_return_the_status(self):
        # 生のパスの前方一致。クエリや末尾の文字が付いても同じ応答
        bodies = [c["body"] for c in self.cases["status_targets"]]
        self.assertEqual(len({str(b) for b in bodies}), 1)
        self.assertEqual(bodies[0], self.cases["status"]["gemini"])
        for target in ("/api/admin/photosX?per=2", "/api/admin/photos/?per=2",
                       "//api/admin/photos?per=2", "///api/admin/photos?filter=fav&per=2"):
            self.assertEqual(self.by_target[target]["status"], 200)
            self.assertEqual(self.by_target[target]["body"]["per"], 2)

    def test_status_quirks(self):
        status = self.cases["status"]["gemini"]["db"]
        # NULL と空文字の loc_source は同じキー "" に寄り、後から数えた空文字の件数が残る
        self.assertEqual(status["by_loc_source"][""], 1)
        # 窓の境目（<=）と、未計測・範囲外
        self.assertEqual(status["infer_buckets"],
                         {"±15分": 2, "±2時間": 2, "±12時間": 2, "範囲外": 1, "未計測": 1})
        # 前回の処理時の状態が無い・壊れているときは空文字
        for name in ("no_settings", "broken_settings"):
            loc = self.cases["status"][name]["db"]["loc"]
            self.assertEqual((loc["decision"], loc["last_at"]), ("", ""))
        # 使うモデルが違えば AI の件数が変わる
        self.assertNotEqual(self.cases["status"]["local"]["db"]["ai"],
                            self.cases["status"]["gemini"]["db"]["ai"])

    def test_photos_quirks(self):
        # per は 1〜200 に丸め、page は 1〜総ページ数に丸める。応答には丸めた値を返す
        body = self.by_target["/api/admin/photos?per=1000"]["body"]
        self.assertEqual((body["per"], body["pages"], body["page"]), (200, 1, 1))
        self.assertEqual(self.by_target["/api/admin/photos?per=7&page=6"]["body"]["page"], 5)
        self.assertEqual(self.by_target["/api/admin/photos?per=7&page=-3"]["body"]["page"], 1)
        # 未知の filter は all。応答には解決後の名前
        self.assertEqual(self.by_target["/api/admin/photos?filter=FAV&per=2"]["body"]["filter"], "all")
        # 空の値は無かったことになる（400 にならない）
        self.assertEqual(self.by_target["/api/admin/photos?page=&per="]["body"]["per"], 50)
        # 同じ名前は最初の値、`;` は区切りではない
        self.assertEqual(self.by_target["/api/admin/photos?page=2&page=3&per=5"]["body"]["page"], 2)
        self.assertEqual(self.by_target["/api/admin/photos?per=5;page=2"]["status"], 400)
        # int() が通す形
        for target in ("/api/admin/photos?page=1_0&per=5", "/api/admin/photos?page=%EF%BC%92&per=%EF%BC%95",
                       "/api/admin/photos?page=%E0%A5%A9&per=5", "/api/admin/photos?page=%202%20&per=5"):
            self.assertEqual(self.by_target[target]["status"], 200, target)
        self.assertEqual(self.by_target["/api/admin/photos?page=1_0&per=5"]["body"]["page"], 6)
        self.assertEqual(self.by_target["/api/admin/photos?page=%EF%BC%92&per=%EF%BC%95"]["body"]["per"], 5)
        # 通らない形は 400
        for target in ("/api/admin/photos?page=abc", "/api/admin/photos?page=1__0",
                       "/api/admin/photos?page=%20", "/api/admin/photos?page=%1C2"):
            self.assertEqual(self.by_target[target]["status"], 400, target)
            self.assertEqual(self.by_target[target]["body"], {"ok": False, "error": "bad_params"})
        # 桁数の上限（4300桁）
        self.assertEqual(self.by_target[f"/api/admin/photos?page={'9' * 4300}&per=5"]["status"], 200)
        self.assertEqual(self.by_target[f"/api/admin/photos?page={'9' * 4301}&per=5"]["status"], 400)

    def test_hint_and_suggest_keys(self):
        noloc = {i["id"]: i for c in self.cases["photos"]
                 if c["target"].startswith("/api/admin/photos?filter=noloc&per=4")
                 for i in c["body"]["items"]}
        # 位置なしの一覧では hint のキーが付く。近い写真が7日より離れている・時刻が読めない・時刻なしは null
        self.assertEqual(noloc[16]["hint"]["dt_s"], 300)
        self.assertEqual(noloc[16]["hint"]["label"], "架空県 見本市 サンプル町")
        for pid in (19, 20, 21, 22):
            self.assertIn("hint", noloc[pid])
            self.assertIsNone(noloc[pid]["hint"], pid)
        # 候補があるときだけ suggest のキー。壊れた evidence・配列でない evidence は []
        self.assertEqual(noloc[16]["suggest"]["evidence"], ["看板の文字", "建物の形"])
        self.assertEqual(noloc[18]["suggest"]["evidence"], [])
        self.assertEqual(noloc[21]["suggest"]["evidence"], [])
        self.assertNotIn("suggest", noloc[17])
        # all の一覧には hint のキーが付かない
        everyone = self.by_target["/api/admin/photos"]["body"]["items"]
        self.assertTrue(all("hint" not in i for i in everyone))
        # 表示ファイルが無くても derivative が残っていればサムネイルを拾う
        self.assertEqual(noloc[17]["thumb"], "/derivatives/d-has-deriv.jpg")
        self.assertEqual(noloc[18]["thumb"], "")


if __name__ == "__main__":
    unittest.main()
