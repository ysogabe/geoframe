"""POST の CSRF 対策の判定（Content-Type・Origin・Host）のフィクスチャ。

Rust の差分試験（crates/geoframe/tests/csrf_python.rs）が読むフィクスチャが、現行実装の出力と
一致していること（再生成: python3 tools/gen_csrf_fixture.py）と、フィクスチャが押さえている現行実装の癖を
記録する。
"""
import unittest

from tests import csrf_fixture as fixture


class CsrfFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = fixture.build_cases()
        cls.http = {c["name"]: c for c in cls.cases["http"]}

    def test_fixture_file_is_up_to_date(self):
        text = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(text, fixture.render(self.cases),
                         "再生成: python3 tools/gen_csrf_fixture.py")

    def test_case_names_are_unique(self):
        names = [c["name"] for c in self.cases["http"]]
        self.assertEqual(len(names), len(set(names)))

    def test_header_values_are_read_as_latin1(self):
        # UTF-8 の NBSP は Latin-1 では \xc2 が残るので断られる。Latin-1 の NBSP 1 バイトは strip() が取り除く
        v = fixture.csrf_verdict
        self.assertEqual(v("Â application/json", None, None), "content_type")
        self.assertEqual(v(" application/json ", None, None), "ok")
        self.assertEqual(v("\u001fapplication/json\u001c", None, None), "ok")

    def test_bracketed_hosts_that_are_not_ipv6_raise(self):
        # 例外は捕まえられず do_POST を抜ける＝応答を返さずに接続が閉じる（フィクスチャの状態コードは 0）
        v = fixture.csrf_verdict
        self.assertEqual(v("application/json", "http://[127.0.0.1]", "[127.0.0.1]"), "error")
        self.assertEqual(v("application/json", "http://[::1%]", "[::1%]"), "error")
        self.assertEqual(v("application/json", "http://[::1]", "[::1]"), "ok")
        for name in ("origin_bracket_ipv4", "origin_bracket_zone_empty", "origin_bracket_unclosed"):
            self.assertEqual(self.http[name]["status"], 0, name)
            self.assertFalse(self.http[name]["wrote"], name)

    def test_trailing_space_in_origin_is_kept_but_tab_is_dropped(self):
        self.assertEqual(self.http["origin_trailing_space"]["status"], 403)
        self.assertEqual(self.http["origin_trailing_tab"]["status"], 200)
        self.assertEqual(self.http["origin_trailing_space_after_path"]["status"], 200)
        self.assertEqual(self.http["origin_leading_space"]["status"], 200)

    def test_an_unreadable_header_line_hides_every_header_after_it(self):
        # email のパーサは、ヘッダとして読めない行でヘッダの読み込みを打ち切る
        self.assertEqual(self.http["bad_line_hides_cross_origin"]["status"], 200)
        self.assertTrue(self.http["bad_line_hides_cross_origin"]["wrote"])
        self.assertEqual(self.http["bad_line_hides_content_type"]["status"], 403)
        self.assertEqual(self.http["bad_line_hides_content_length"]["status"], 400)

    def test_scale(self):
        c = self.cases
        self.assertGreaterEqual(len(c["content_type"]) + len(c["origin"]) + len(c["mixed"]) + len(c["host"]), 9000)
        verdicts = {x[-1] for x in c["origin"]}
        self.assertEqual(verdicts, {"ok", "origin", "error"})
