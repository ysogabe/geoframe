import tempfile
import unittest
from pathlib import Path

from PIL import Image

from geoframe import admin, quality, store, util


def _left_red(path):
    """左半分が赤・右半分が青の画像を作る（回した向きを画素で確かめるため）"""
    im = Image.new("RGB", (200, 100), "blue")
    im.paste(Image.new("RGB", (100, 100), "red"), (0, 0))
    im.save(path, "JPEG", quality=95)


def _is_red(px):
    return px[0] > 150 and px[2] < 100


class AdminRotateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "derivatives").mkdir()
        self.saved = (admin.DATA_DIR, admin.DERIV_DIR, store.DB_PATH, store._db)
        if store._db is not None:
            store._db.close()
        store._db = None
        store.DB_PATH = self.root / "catalog.db"
        admin.DATA_DIR = self.root
        admin.DERIV_DIR = self.root / "derivatives"
        store.db()
        _left_red(self.root / "derivatives" / "abc.jpg")
        store.dbx("""INSERT INTO photos(id,source,drive_id,display,name,md5)
                     VALUES(1,'drive','abc','derivatives/abc.jpg','a.jpg','m1')""")
        store.dbx("""INSERT INTO photos(id,source,display,name)
                     VALUES(2,'local','photos/b.jpg','b.jpg')""")

    def tearDown(self):
        if store._db is not None:
            store._db.close()
        admin.DATA_DIR, admin.DERIV_DIR, store.DB_PATH, store._db = self.saved
        self.temp.cleanup()

    def test_rotate_writes_new_derivative_and_back(self):
        res = admin.admin_rotate({"id": 1, "delta": 90})
        self.assertEqual((res["rotation"], res["thumb"]), (90, "/derivatives/abc.r90.jpg"))
        self.assertFalse((self.root / "derivatives" / "abc.jpg").exists())
        with Image.open(self.root / "derivatives" / "abc.r90.jpg") as im:
            self.assertEqual(im.size, (100, 200))
            # 時計回りに90°: 左にあった赤が上に来る
            self.assertTrue(_is_red(im.getpixel((50, 20))))
            self.assertFalse(_is_red(im.getpixel((50, 180))))
        row = store.dbq("SELECT display, rotation FROM photos WHERE id=1")[0]
        self.assertEqual((row["display"], row["rotation"]), ("derivatives/abc.r90.jpg", 90))
        # displayが変わったので品質解析の対象に戻る
        self.assertIn(1, [r["id"] for r in quality._pending_rows(10)])

        res = admin.admin_rotate({"id": 1, "delta": -90})
        self.assertEqual((res["rotation"], res["thumb"]), (0, "/derivatives/abc.jpg"))
        self.assertEqual(sorted(p.name for p in (self.root / "derivatives").iterdir()), ["abc.jpg"])
        with Image.open(self.root / "derivatives" / "abc.jpg") as im:
            self.assertEqual(im.size, (200, 100))
            self.assertTrue(_is_red(im.getpixel((20, 50))))

    def test_rejects_bad_requests(self):
        for payload, exc in (({"id": 1, "delta": 45}, admin.BadRequest),
                             ({"id": 1, "delta": True}, admin.BadRequest),
                             ({"id": 2, "delta": 90}, admin.BadRequest),
                             ({"id": 9, "delta": 90}, admin.NotFound)):
            with self.assertRaises(exc):
                admin.admin_rotate(payload)
        (self.root / "derivatives" / "abc.jpg").unlink()
        with self.assertRaises(admin.BadRequest):
            admin.admin_rotate({"id": 1, "delta": 90})

    def test_make_derivative_applies_rotation(self):
        src = self.root / "src.jpg"
        _left_red(src)
        dest = self.root / "derivatives" / util.derivative_name("abc", 270)
        util.make_derivative(src, dest, 270)
        self.assertEqual(dest.name, "abc.r270.jpg")
        with Image.open(dest) as im:
            self.assertEqual(im.size, (100, 200))
            # 時計回りに270°（= 反時計回りに90°）: 左にあった赤が下に来る
            self.assertTrue(_is_red(im.getpixel((50, 180))))
        self.assertEqual(util.derivative_name("abc"), "abc.jpg")


if __name__ == "__main__":
    unittest.main()
