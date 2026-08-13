import shutil
import tempfile
import unittest
from pathlib import Path

from PIL import Image, ImageDraw

from geoframe import quality, store


class AutomaticQualityTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.original_root = quality.DATA_DIR
        self.original_db_path = store.DB_PATH
        self.original_db = store._db
        if store._db is not None:
            store._db.close()
        store._db = None
        store.DB_PATH = self.root / "catalog.db"
        quality.DATA_DIR = self.root
        (self.root / "derivatives").mkdir()
        store.db()

    def tearDown(self):
        if store._db is not None:
            store._db.close()
        store._db = self.original_db
        store.DB_PATH = self.original_db_path
        quality.DATA_DIR = self.original_root
        self.temp.cleanup()

    def _photo(self, photo_id, filename, md5, taken_at="2026-08-04 10:00"):
        store.dbx(
            """INSERT INTO photos(id,source,display,name,md5,taken_at)
               VALUES(?,'local',?,?,?,?)""",
            (photo_id, f"derivatives/{filename}", filename, md5, taken_at),
        )

    def _make_source_pair(self):
        first = self.root / "derivatives/first.jpg"
        image = Image.new("RGB", (180, 120), "#315b7a")
        draw = ImageDraw.Draw(image)
        draw.rectangle((18, 18, 160, 100), outline="white", width=4)
        draw.line((18, 100, 160, 18), fill="yellow", width=3)
        image.save(first, quality=92)
        shutil.copyfile(first, self.root / "derivatives/copy.jpg")

    def test_partial_scan_keeps_old_result_then_replaces_atomically(self):
        self._make_source_pair()
        self._photo(1, "first.jpg", "md5-1")
        self._photo(2, "copy.jpg", "md5-2")
        store.dbx(
            """INSERT INTO photo_quality VALUES(
                 1,'derivatives/first.jpg','md5-1',NULL,'old',99,'similar',1,1,50,
                 '[]','[]','old','yesterday')"""
        )

        first = quality.quality_cycle(limit=1)
        self.assertEqual(first["analyzed"], 1)
        self.assertEqual(first["remaining"], 1)
        self.assertFalse(first["rebuilt"])
        old = store.dbq("SELECT algorithm FROM photo_quality")
        self.assertEqual([row["algorithm"] for row in old], ["old"])

        second = quality.quality_cycle(limit=1)
        self.assertTrue(second["rebuilt"])
        self.assertEqual(second["remaining"], 0)
        rows = store.dbq(
            "SELECT photo_id,group_kind,recommended,algorithm FROM photo_quality ORDER BY photo_id"
        )
        self.assertEqual([row["photo_id"] for row in rows], [1, 2])
        self.assertTrue(all(row["group_kind"] == "exact" for row in rows))
        self.assertEqual(sum(row["recommended"] for row in rows), 1)
        self.assertTrue(all(row["algorithm"] == quality.ALGORITHM for row in rows))

    def test_unchanged_photos_are_not_reanalyzed(self):
        self._make_source_pair()
        self._photo(1, "first.jpg", "md5-1")
        self._photo(2, "copy.jpg", "md5-2")
        quality.quality_backfill(batch_size=10)
        before = store.dbq(
            "SELECT photo_id,analyzed_at FROM photo_quality_features ORDER BY photo_id"
        )

        result = quality.quality_cycle(limit=10)
        after = store.dbq(
            "SELECT photo_id,analyzed_at FROM photo_quality_features ORDER BY photo_id"
        )
        self.assertEqual(result["analyzed"], 0)
        self.assertFalse(result["rebuilt"])
        self.assertEqual([tuple(row) for row in before], [tuple(row) for row in after])


if __name__ == "__main__":
    unittest.main()
