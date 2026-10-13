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


class PipelineDbCase(unittest.TestCase):
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


class AutomaticAiPipelineTest(PipelineDbCase):
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


class CountingClient:
    """FakeGeminiと同じベクトルを返し、呼ばれた回数を数える。"""

    def __init__(self, inner):
        self.inner = inner
        self.embeds = 0
        self.judges = 0

    def embed_part(self, part):
        self.embeds += 1
        return self.inner.embed_part(part)

    def post(self, model, method, payload, timeout=60):
        self.judges += 1
        return self.inner.post(model, method, payload, timeout)


class EmbeddingBackendSwitchTest(PipelineDbCase):
    """embeddingのモデルを切り替えても、写真の中身が同じなら判定を消さない。"""

    def setUp(self):
        super().setUp()
        self.gemini = CountingClient(FakeGemini())
        self.local = CountingClient(FakeGemini())
        gemini, local = self.gemini, self.local

        class GeminiFactory:
            @staticmethod
            def from_environment():
                return gemini

        class LocalFactory:
            @staticmethod
            def shared():
                return local

        ai_pipeline.AgentPlatformEmbeddingClient = GeminiFactory
        self.saved = (ai_pipeline.EMBED_MODEL, ai_pipeline.EMBED_BACKEND,
                      ai_pipeline.LocalEmbeddingClient)
        ai_pipeline.LocalEmbeddingClient = LocalFactory
        self._add_photo(1, "#d02020", "sha-red-1")
        self._add_photo(2, "#c82020", "sha-red-2")
        self._add_photo(3, "#20c820", "sha-green")

    def tearDown(self):
        (ai_pipeline.EMBED_MODEL, ai_pipeline.EMBED_BACKEND,
         ai_pipeline.LocalEmbeddingClient) = self.saved
        super().tearDown()

    def _use(self, backend, model):
        ai_pipeline.EMBED_BACKEND = backend
        ai_pipeline.EMBED_MODEL = model

    def _judgments(self):
        return {row["photo_id"]: tuple(row)[1:] for row in store.dbq(
            """SELECT photo_id,subject,keep_score,recommended,judge_model
               FROM photo_ai_features ORDER BY photo_id""")}

    def test_switch_keeps_judgments_and_reuses_cached_embeddings(self):
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)
        before = self._judgments()
        self.assertTrue(any(row[2] is not None for row in before.values()))
        self.assertEqual((self.gemini.embeds, self.gemini.judges), (3, 1))

        self._use("local", "fake-local")
        result = ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)
        self.assertEqual(result["embedded"], 3)
        self.assertEqual(self.local.embeds, 3)
        models = {row["embedding_model"] for row in store.dbq(
            "SELECT embedding_model FROM photo_ai_features")}
        self.assertEqual(models, {"fake-local"})
        self.assertEqual(self._judgments(), before)
        # 同じ写真の組なのでgroup_idが変わらず、Geminiで比べ直さない。
        self.assertEqual(self.gemini.judges, 1)

        self._use("gemini", "gemini-embedding-2")
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)
        self.assertEqual(self.gemini.embeds, 3)  # キャッシュから戻すのでGeminiを呼ばない
        self.assertEqual(self._judgments(), before)

    def test_prefilled_cache_switches_without_computing(self):
        from geoframe import ai_search
        for pid, sha in ((1, "sha-red-1"), (2, "sha-red-2"), (3, "sha-green")):
            vector = FakeGemini().embed_part({"inlineData": {"data": base64.b64encode(
                (self.root / "derivatives" / f"{pid}.jpg").read_bytes()).decode()}})
            store.dbx("""INSERT INTO photo_embeddings VALUES(?,?,?,?,'now')""",
                      (sha, "fake-local", ai_pipeline.EMBED_DIM, ai_search.pack_embedding(vector)))
        self._use("local", "fake-local")
        result = ai_pipeline.ai_cycle(embed_limit=0, judge_limit=0)
        self.assertEqual(result["embedded"], 3)  # キャッシュからの分は上限0でも入る
        self.assertEqual(self.local.embeds, 0)

    def test_upsert_clears_judgment_only_when_content_differs(self):
        # 通常は中身が変わると掃除で行ごと消えるが、upsert自体も中身の違いで判定を消すことを確かめる。
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)
        before = self._judgments()
        store.dbx("UPDATE photo_quality_features SET content_sha256='sha-red-2b' WHERE photo_id=2")
        ai_pipeline._embed_pending(ai_pipeline._load_photos(), self.gemini, 10)
        after = self._judgments()
        self.assertEqual(after[1], before[1])
        self.assertEqual(after[2], ("", None, None, None))

    def test_changed_photo_content_clears_its_judgment(self):
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=2)
        before = self._judgments()
        store.dbx("UPDATE photos SET md5='md5-2b' WHERE id=2")
        store.dbx("""UPDATE photo_quality_features SET source_md5='md5-2b',
                     content_sha256='sha-red-2b' WHERE photo_id=2""")
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=0)
        after = self._judgments()
        self.assertEqual(after[1], before[1])
        self.assertEqual(after[2], ("", None, None, None))


if __name__ == "__main__":
    unittest.main()
