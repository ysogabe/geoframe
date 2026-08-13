import base64
import json
import shutil
import tempfile
import unittest
from pathlib import Path

from PIL import Image

from geoframe import ai_pipeline, store


class FakeGemini:
    def embed_part(self, part):
        raw = base64.b64decode(part["inlineData"]["data"])
        from io import BytesIO
        with Image.open(BytesIO(raw)) as image:
            red, green, _blue = image.resize((1, 1)).getpixel((0, 0))
        # 赤系2枚は同じ場面、緑系は別場面になる正規化済みに近いベクトル。
        vector = [0.0] * ai_pipeline.EMBED_DIM
        if red > green:
            vector[0], vector[1] = 0.999, 0.04
        else:
            vector[0], vector[1] = 0.04, 0.999
        return vector

    def post(self, model, method, payload, timeout=60):
        count = len(payload["contents"][0]["parts"]) - 1
        photos = []
        for index in range(1, count + 1):
            photos.append({
                "index": index, "subject": "landscape", "pet_state": "not_applicable",
                "eyes": "not_applicable", "face_crop": "not_applicable",
                "horizon_status": "level", "horizon_score": 95,
                "subject_sharpness": 90 - index, "aesthetic_score": 90 - index,
                "keep_score": 90 - index, "recommended": index == 1,
                "reasons": ["水平と構図が良好"], "concerns": [],
            })
        value = {"same_scene": True, "summary": "同じ場面", "photos": photos}
        return {"candidates": [{"content": {"parts": [{"text": json.dumps(value)}]}}]}


class AutomaticAiPipelineTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "derivatives").mkdir()
        self.original_root = ai_pipeline.DATA_DIR
        self.original_client = ai_pipeline.AgentPlatformEmbeddingClient
        self.original_db_path = store.DB_PATH
        self.original_db = store._db
        if store._db is not None:
            store._db.close()
        store._db = None
        store.DB_PATH = self.root / "catalog.db"
        ai_pipeline.DATA_DIR = self.root
        fake = FakeGemini()

        class Factory:
            @staticmethod
            def from_environment():
                return fake

        ai_pipeline.AgentPlatformEmbeddingClient = Factory
        store.db()

    def tearDown(self):
        if store._db is not None:
            store._db.close()
        store._db = self.original_db
        store.DB_PATH = self.original_db_path
        ai_pipeline.DATA_DIR = self.original_root
        ai_pipeline.AgentPlatformEmbeddingClient = self.original_client
        self.temp.cleanup()

    def _add_photo(self, pid, color, sha, filename=None):
        filename = filename or f"{pid}.jpg"
        path = self.root / "derivatives" / filename
        Image.new("RGB", (120, 80), color).save(path)
        store.dbx("""INSERT INTO photos(
            id,source,display,name,md5,taken_at,indexed_at,lat,lon
          ) VALUES(?,'local',?,?,?,?,?,?,?)""",
                  (pid, f"derivatives/{filename}", filename, f"md5-{pid}",
                   "2026-08-05 10:00", "2026-08-05 10:05", 35.0, 139.0))
        store.dbx("""INSERT INTO photo_quality_features VALUES(
          ?,?,?,NULL,?,?,5.0,128,90,0,0,100,'','now')""",
                  (pid, f"derivatives/{filename}", f"md5-{pid}", sha, f"{pid:016x}"))

    def test_full_incremental_flow_excludes_exact_duplicate_and_judges_scene(self):
        self._add_photo(1, "#d02020", "sha-red-1")
        self._add_photo(2, "#c82020", "sha-red-2")
        self._add_photo(3, "#20c820", "sha-green")
        self._add_photo(4, "#d02020", "sha-red-1")  # SHA-256完全重複
        shutil.copyfile(self.root / "derivatives/1.jpg",
                        self.root / "derivatives/4.jpg")

        result = ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)

        self.assertEqual(result["embedded"], 3)
        self.assertEqual(store.dbq("SELECT COUNT(*) c FROM photo_ai_features")[0]["c"], 3)
        groups = store.dbq("SELECT status,same_scene,summary FROM photo_ai_groups")
        self.assertEqual(len(groups), 1)
        self.assertEqual((groups[0]["status"], groups[0]["same_scene"]), ("judged", 1))
        members = store.dbq(
            "SELECT photo_id,recommended FROM photo_ai_group_members ORDER BY photo_id")
        self.assertEqual([row["photo_id"] for row in members], [1, 2])
        self.assertEqual(sum(row["recommended"] for row in members), 1)
        judged = store.dbq("""SELECT horizon_status,horizon_score FROM photo_ai_features
                              WHERE recommended IS NOT NULL""")
        self.assertTrue(judged)
        self.assertTrue(all(row["horizon_status"] == "level" for row in judged))


if __name__ == "__main__":
    unittest.main()
