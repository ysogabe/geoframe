import json
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from geoframe import travel_blog


class TravelBlogTest(unittest.TestCase):
    def test_plan_is_discoverable(self):
        trips = travel_blog.api_blog_trips()["items"]
        ids = {trip["trip_id"] for trip in trips}
        self.assertIn("uzbekistan-silk-road-2025", ids)

    def test_candidates_fold_same_minute_and_hide_coordinates(self):
        rows = [
            {"id": 1, "display": "derivatives/1.jpg", "name": "a.jpg",
             "taken_at": "2025-06-06 10:00", "loc_source": "drive_meta",
             "place": "", "city": "", "title": "", "ai_caption": "", "favorite": 0},
            {"id": 2, "display": "derivatives/2.jpg", "name": "b.jpg",
             "taken_at": "2025-06-06 10:00", "loc_source": "drive_meta",
             "place": "", "city": "", "title": "", "ai_caption": "", "favorite": 1},
            {"id": 3, "display": "derivatives/3.jpg", "name": "c.jpg",
             "taken_at": "2025-06-06 11:00", "loc_source": "inferred",
             "place": "", "city": "", "title": "", "ai_caption": "", "favorite": 0},
        ]
        segment = {"id": "d1", "date": "2025-06-06", "title": "テスト",
                   "locations": ["都市"], "start_local": "00:00", "end_local": "23:59"}
        with patch.object(travel_blog, "dbq", return_value=rows):
            result = travel_blog._segment_candidates(segment)
        self.assertEqual(result["photo_count"], 3)
        self.assertEqual(result["duplicate_groups"], 2)
        self.assertEqual(len(result["candidates"]), 2)
        self.assertEqual(result["candidates"][0]["photo_id"], "2")
        self.assertEqual(result["candidates"][0]["duplicate_count"], 2)
        self.assertNotIn("lat", result["candidates"][0])
        self.assertNotIn("lon", result["candidates"][0])

    def test_dry_run_accepts_generated_manifest(self):
        rows = [{
            "id": 1,
            "display": "derivatives/sample.jpg",
            "name": "sample.jpg",
            "taken_at": "2025-06-06 10:00",
            "loc_source": "drive_meta",
            "place": "",
            "city": "",
            "title": "テスト写真",
            "ai_caption": "",
            "favorite": 1,
        }]
        with patch.object(travel_blog, "dbq", return_value=rows):
            manifest = travel_blog.generate_manifest("uzbekistan-silk-road-2025")
        result = travel_blog.export_blog({
            "trip_id": "uzbekistan-silk-road-2025",
            "manifest": manifest,
            "dry_run": True,
        })
        self.assertTrue(result["ok"])
        self.assertTrue(result["dry_run"])
        self.assertGreater(result["images"], 0)

    def test_html_escapes_user_text(self):
        manifest = {
            "title": "<script>alert(1)</script>", "subtitle": "", "summary": "",
            "chapters": [{"date": "2025-01-01", "title": "<b>x</b>",
                          "location_label": "", "body": "<img src=x>", "images": []}],
        }
        page = travel_blog._render_html(manifest)
        self.assertNotIn("<script>alert(1)</script>", page)
        self.assertIn("&lt;script&gt;", page)
        self.assertIn("&lt;img src=x&gt;", page)


class BlogExportGuardTest(unittest.TestCase):
    """総枚数上限とエクスポートの排他制御。

    実データ（app/var/catalog.db・blog_exports/）を汚さないため、
    test_ai_pipeline.py と同じくモジュール属性を一時ディレクトリへ差し替える。
    DATA_DIR も併せて差し替える点に注意（export_blog 末尾が
    relative_to(DATA_DIR) を呼ぶため、EXPORT_DIR だけ差し替えるとValueErrorになる）。
    """

    TRIP_ID = "test-trip-2026"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.originals = {
            name: getattr(travel_blog, name)
            for name in ("DATA_DIR", "PLAN_DIR", "DRAFT_DIR", "EXPORT_DIR")
        }
        travel_blog.DATA_DIR = self.root
        travel_blog.PLAN_DIR = self.root / "travel_blog" / "plans"
        travel_blog.DRAFT_DIR = self.root / "travel_blog" / "drafts"
        travel_blog.EXPORT_DIR = self.root / "blog_exports"
        travel_blog.PLAN_DIR.mkdir(parents=True)
        (travel_blog.PLAN_DIR / f"{self.TRIP_ID}.json").write_text(json.dumps({
            "schema_version": "1.0",
            "trip_id": self.TRIP_ID,
            "title": "テスト旅程",
            "subtitle": "",
            "status": "draft",
            "default_timezone": "Asia/Tokyo",
            "date_range": {"start": "2026-01-01", "end": "2026-01-01"},
            "segments": [{
                "id": "d1", "day": 1, "date": "2026-01-01",
                "timezone": "Asia/Tokyo", "title": "初日",
                "locations": ["東京"], "start_local": "00:00", "end_local": "23:59",
                "activities": [],
            }],
        }, ensure_ascii=False), encoding="utf-8")

    def tearDown(self):
        for name, value in self.originals.items():
            setattr(travel_blog, name, value)
        self.temp.cleanup()

    def _manifest(self, images_per_chapter):
        return {
            "schema_version": "1.0",
            "trip_id": self.TRIP_ID,
            "title": "テスト旅程",
            "subtitle": "",
            "status": "draft",
            "summary": "",
            "chapters": [{
                "id": f"d{n}", "date": "2026-01-01", "title": f"{n}日目",
                "location_label": "東京", "body": "本文",
                "images": [{"photo_id": str(i + 1), "src": "/derivatives/1.jpg",
                            "alt": "写真", "caption": ""} for i in range(count)],
            } for n, count in enumerate(images_per_chapter, 1)],
        }

    def test_total_images_over_limit_is_rejected(self):
        limit = travel_blog.MAX_TOTAL_EXPORT_IMAGES
        # 1章100枚・章数40の既存上限はどれも超えないが、合計だけが上限を超える構成
        manifest = self._manifest([100, 100, 1])
        with self.assertRaises(travel_blog.BlogBadRequest) as ctx:
            travel_blog._validate_manifest(manifest, self.TRIP_ID)
        self.assertIn(str(limit), str(ctx.exception))

    def test_total_images_exactly_at_limit_is_accepted(self):
        limit = travel_blog.MAX_TOTAL_EXPORT_IMAGES
        manifest = self._manifest([100, limit - 100])
        clean = travel_blog._validate_manifest(manifest, self.TRIP_ID)
        self.assertEqual(sum(len(ch["images"]) for ch in clean["chapters"]), limit)

    def test_export_rejects_concurrent_request_while_locked(self):
        self.assertTrue(travel_blog._export_lock.acquire(blocking=False))
        # assert が落ちてもロックを必ず返す（残すと以後の全テストがブロックされる）
        self.addCleanup(travel_blog._export_lock.release)
        with self.assertRaises(travel_blog.BlogExportBusy):
            travel_blog.export_blog({"trip_id": self.TRIP_ID,
                                     "manifest": self._manifest([0])})

    def test_export_releases_lock_after_success(self):
        result = travel_blog.export_blog({"trip_id": self.TRIP_ID,
                                          "manifest": self._manifest([0])})
        self.assertTrue(result["ok"])
        self.assertEqual(result["images"], 0)
        self.assertTrue((travel_blog.EXPORT_DIR / self.TRIP_ID / "index.html").is_file())
        self.assertTrue(travel_blog._export_lock.acquire(blocking=False),
                        "成功後にロックが解放されていない")
        travel_blog._export_lock.release()

    def test_export_releases_lock_after_failure(self):
        # 写真が見つからず、ロック取得後の staging 処理内で失敗させる
        with patch.object(travel_blog, "dbq", return_value=[]):
            with self.assertRaises(travel_blog.BlogBadRequest):
                travel_blog.export_blog({"trip_id": self.TRIP_ID,
                                         "manifest": self._manifest([1])})
        self.assertTrue(travel_blog._export_lock.acquire(blocking=False),
                        "例外後にロックが解放されていない")
        travel_blog._export_lock.release()
        # staging ディレクトリを残していないこと
        self.assertEqual(list(travel_blog.EXPORT_DIR.glob(f".{self.TRIP_ID}-*")), [])

    def test_export_serializes_real_threads(self):
        holding = threading.Event()
        finish = threading.Event()

        def worker():
            travel_blog._export_lock.acquire()
            try:
                holding.set()
                finish.wait(timeout=10)
            finally:
                travel_blog._export_lock.release()

        thread = threading.Thread(target=worker)
        thread.start()
        try:
            self.assertTrue(holding.wait(timeout=10), "ワーカーがロックを取得できなかった")
            with self.assertRaises(travel_blog.BlogExportBusy):
                travel_blog.export_blog({"trip_id": self.TRIP_ID,
                                         "manifest": self._manifest([0])})
        finally:
            finish.set()
            thread.join(timeout=10)
        self.assertFalse(thread.is_alive(), "ワーカースレッドが終了していない")
        # ワーカー解放後は通常どおり受け付けられる
        self.assertTrue(travel_blog.export_blog({"trip_id": self.TRIP_ID,
                                                 "manifest": self._manifest([0])})["ok"])


if __name__ == "__main__":
    unittest.main()
