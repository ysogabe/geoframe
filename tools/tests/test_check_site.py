"""tools/check_site.py の試験（標準ライブラリだけ）。"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("check_site", ROOT / "tools" / "check_site.py")
check_site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_site)

PAGE = """<!doctype html>
<html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>t</title>
<meta name="description" content="d">
<meta property="og:title" content="t"><meta property="og:description" content="d">
<meta property="og:url" content="https://ysogabe.github.io/x/">
<link rel="stylesheet" href="styles.css">
</head><body>
<a href="#main">skip</a>
<main id="main">{body}</main>
</body></html>
"""


def make_site(body="", css="body{color:#000}", extra=None):
    tmp = tempfile.TemporaryDirectory()
    site = Path(tmp.name)
    (site / "index.html").write_text(PAGE.format(body=body), encoding="utf-8")
    (site / "styles.css").write_text(css, encoding="utf-8")
    for name, content in (extra or {}).items():
        (site / name).write_text(content, encoding="utf-8")
    return tmp, site


class CheckSiteTest(unittest.TestCase):
    def errors(self, **kw):
        tmp, site = make_site(**kw)
        with tmp:
            return check_site.check_site(site)

    def test_real_site_passes(self):
        self.assertEqual(check_site.check_site(ROOT / "site"), [])

    def test_minimal_site_passes(self):
        self.assertEqual(self.errors(body='<a href="https://github.com/ysogabe/x">g</a><a href="#main">m</a>'), [])

    def test_missing_fragment_and_asset(self):
        errs = self.errors(body='<a href="#nope">a</a><img src="missing.png" alt="">')
        self.assertTrue(any("fragment" in e for e in errs), errs)
        self.assertTrue(any("参照先がありません" in e for e in errs), errs)

    def test_external_cdn_script_and_font_are_rejected(self):
        errs = self.errors(
            body='<script src="https://cdn.example.test/a.js"></script><a href="https://example.com/">x</a>',
            css='@import url("https://fonts.googleapis.com/css2?family=X");',
        )
        joined = "\n".join(errs)
        self.assertIn("<script> は使えません", joined)
        self.assertIn("許可していない外部ホスト", joined)
        self.assertIn("CSSが外部を読んでいます", joined)

    def test_allowed_navigation_host_cannot_supply_assets(self):
        for asset in [
            '<link rel="stylesheet" href="https://github.com/x/a.css">',
            '<img src="https://github.com/x/a.png" alt="example">',
        ]:
            with self.subTest(asset=asset):
                self.assertTrue(any("外部資産" in e for e in self.errors(body=asset)))

    def test_escape_and_data_files_are_rejected(self):
        errs = self.errors(body='<a href="../README.md">r</a>', extra={"photos.json": "{}"})
        joined = "\n".join(errs)
        self.assertIn("site/ の外", joined)
        self.assertIn("置けない種類", joined)

    def test_img_requires_alt(self):
        (tmp, site) = make_site(body='<img src="a.png">', extra={})
        with tmp:
            (site / "a.png").write_bytes(b"x")
            self.assertTrue(any("alt" in e for e in check_site.check_site(site)))


if __name__ == "__main__":
    unittest.main()
