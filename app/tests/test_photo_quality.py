import sqlite3
import unittest
from datetime import date

from geoframe import photos


class PhotoQualityApiTest(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.conn.row_factory = sqlite3.Row
        self.conn.executescript("""
          CREATE TABLE photos(
            id INTEGER PRIMARY KEY, source TEXT, drive_id TEXT, path TEXT, display TEXT,
            name TEXT, md5 TEXT, mtime REAL, taken_at TEXT, lat REAL, lon REAL,
            loc_source TEXT, inferred_from INTEGER, infer_dt_s INTEGER, place TEXT, elev REAL,
            title TEXT, ai_caption TEXT, indexed_at TEXT, fetched_at TEXT,
            muni_cd TEXT, pref TEXT, city TEXT, favorite INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0
          );
          CREATE TABLE photo_quality(
            photo_id INTEGER PRIMARY KEY, display TEXT, source_md5 TEXT, source_mtime REAL,
            content_sha256 TEXT, group_id INTEGER, group_kind TEXT, rank_in_group INTEGER,
            recommended INTEGER, score REAL, reasons_json TEXT, issues_json TEXT,
            algorithm TEXT, analyzed_at TEXT
          );
          CREATE TABLE photo_ai_groups(
            group_id TEXT PRIMARY KEY, status TEXT, same_scene INTEGER
          );
          CREATE TABLE photo_ai_group_members(
            group_id TEXT, photo_id INTEGER, rank_in_group INTEGER,
            recommended INTEGER, local_score REAL
          );
        """)
        for pid, name in enumerate(("exact-best", "exact-copy", "similar-best", "similar-alt"), 1):
            self.conn.execute(
                """INSERT INTO photos(id,source,display,name,md5,taken_at,lat,lon,loc_source,
                                      place,pref,city,favorite,hidden)
                   VALUES(?, 'drive', ?, ?, ?, '2020-08-04 10:00', 35, 139, 'drive_meta',
                          '', '東京都', '千代田区', 0, 0)""",
                (pid, f"derivatives/{pid}.jpg", name, f"md5-{pid}"))
        rows = ((1, "exact", 1, 1, 90), (2, "exact", 2, 0, 80),
                (3, "similar", 1, 1, 95), (4, "similar", 2, 0, 40))
        for pid, kind, rank, rec, score in rows:
            self.conn.execute(
                """INSERT INTO photo_quality VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
                (pid, f"derivatives/{pid}.jpg", f"md5-{pid}", None, "sha", 1, kind,
                 rank, rec, score, "[]", "[]", "test", "now"))
        self.conn.commit()
        self.original_dbq = photos.dbq
        photos.dbq = lambda sql, args=(): self.conn.execute(sql, args).fetchall()

    def tearDown(self):
        photos.dbq = self.original_dbq
        self.conn.close()

    def test_manifest_suppresses_only_non_representative_exact_duplicate(self):
        items = photos.api_photos()
        self.assertEqual([item["id"] for item in items], [1, 3, 4])
        self.assertEqual(items[0]["quality_role"], "recommended")
        self.assertEqual(items[1]["quality_kind"], "similar")
        self.assertEqual(items[2]["quality_role"], "alternate")

    def test_on_this_day_also_suppresses_exact_duplicate_and_prefers_recommendation(self):
        result = photos.api_onthisday(date(2026, 8, 4))
        self.assertEqual([item["id"] for item in result["items"]], [1, 3, 4])

    def test_manifest_suppresses_only_completed_same_scene_alternates(self):
        self.conn.execute("INSERT INTO photo_ai_groups VALUES('g1','judged',1)")
        self.conn.executemany("INSERT INTO photo_ai_group_members VALUES('g1',?,?,?,?)", [
            (3, 1, 1, 95), (4, 2, 0, 40)])
        self.conn.commit()
        self.assertEqual([item["id"] for item in photos.api_photos()], [1, 3])
        self.conn.execute("UPDATE photos SET favorite=1 WHERE id=4")
        self.conn.commit()
        self.assertEqual([item["id"] for item in photos.api_photos()], [1, 3, 4])

    def test_pending_or_not_same_scene_never_suppresses(self):
        self.conn.execute("INSERT INTO photo_ai_groups VALUES('g1','pending',1)")
        self.conn.executemany("INSERT INTO photo_ai_group_members VALUES('g1',?,?,?,?)", [
            (3, 1, 1, 95), (4, 2, 0, 40)])
        self.conn.commit()
        self.assertEqual([item["id"] for item in photos.api_photos()], [1, 3, 4])


if __name__ == "__main__":
    unittest.main()
