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



class LearnedRankTest(PipelineDbCase):
    """学習済みの順位付けモデルがあれば、固定の式の代わりにそれで順位を付ける。"""

    def setUp(self):
        super().setUp()
        from geoframe import rank_model
        self.rank_model = rank_model
        self.saved_path = rank_model.RANK_MODEL_PATH
        rank_model.RANK_MODEL_PATH = self.root / "rank_model.json"
        rank_model._cache["key"] = None
        self._add_photo(1, "#d02020", "sha-red-1")
        self._add_photo(2, "#c82020", "sha-red-2")
        # 固定の式では鮮明な1が上になる
        store.dbx("UPDATE photo_quality_features SET sharpness=4.0 WHERE photo_id=2")

    def tearDown(self):
        self.rank_model.RANK_MODEL_PATH = self.saved_path
        self.rank_model._cache["key"] = None
        super().tearDown()

    def _write_model(self, embedding_model):
        dim = ai_pipeline.EMBED_DIM
        (self.root / "rank_model.json").write_text(json.dumps({
            "format": "geoframe-rank-linear-v1", "name": "test", "embedding_model": embedding_model,
            "embedding_dim": dim, "base_features": ["sharpness"], "base_weights": [-1.0],
            "pca_mean": [0.0] * dim, "pca_components": [[0.0] * dim], "pca_weights": [0.0],
        }))

    def _order(self):
        return [row["photo_id"] for row in store.dbq(
            "SELECT photo_id FROM photo_ai_group_members ORDER BY rank_in_group")]

    def test_learned_model_ranks_group(self):
        self._write_model(ai_pipeline.EMBED_MODEL)
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=0)
        self.assertEqual(self._order(), [2, 1])
        algorithm = store.dbq("SELECT algorithm FROM photo_ai_groups")[0]["algorithm"]
        self.assertTrue(algorithm.endswith(":rank-test"))
        scores = {row["photo_id"]: row["local_score"] for row in store.dbq(
            "SELECT photo_id,local_score FROM photo_ai_group_members")}
        self.assertEqual(scores, {1: 0.0, 2: 100.0})

    def test_model_for_other_embedding_falls_back_to_quality_rank(self):
        self._write_model("other-embedding")
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=0)
        self.assertEqual(self._order(), [1, 2])

    def test_broken_model_falls_back_to_quality_rank(self):
        (self.root / "rank_model.json").write_text("{")
        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=0)
        self.assertEqual(self._order(), [1, 2])


if __name__ == "__main__":
    unittest.main()


class JudgeOrderTest(PipelineDbCase):
    def setUp(self):
        super().setUp()
        self.original_rate = ai_pipeline.AI_JUDGE_SHUFFLE_RATE

    def tearDown(self):
        ai_pipeline.AI_JUDGE_SHUFFLE_RATE = self.original_rate
        super().tearDown()

    def test_presentation_order_is_deterministic_and_shuffles_about_rate(self):
        ai_pipeline.AI_JUDGE_SHUFFLE_RATE = 0.2
        ids = [11, 12, 13, 14, 15, 16]
        modes = [ai_pipeline.presentation_order(f"scene-{n:016x}", ids) for n in range(2000)]
        shuffled = [presented for mode, presented in modes if mode == "shuffled"]
        self.assertTrue(0.17 < len(shuffled) / len(modes) < 0.23)
        self.assertTrue(all(presented == ids for mode, presented in modes if mode == "rank"))
        self.assertTrue(all(sorted(presented) == ids for presented in shuffled))
        self.assertTrue(any(presented != ids for presented in shuffled))
        self.assertEqual(ai_pipeline.presentation_order("scene-x", ids),
                         ai_pipeline.presentation_order("scene-x", ids))
        ai_pipeline.AI_JUDGE_SHUFFLE_RATE = 0.0
        self.assertEqual(ai_pipeline.presentation_order("scene-x", ids), ("rank", ids))

    def test_shuffled_judgment_maps_back_to_presented_photos(self):
        ai_pipeline.AI_JUDGE_SHUFFLE_RATE = 1.0
        for pid, color in ((1, "#d02020"), (2, "#c82020"), (3, "#c01818"), (4, "#d81c1c")):
            self._add_photo(pid, color, f"sha-red-{pid}")

        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=1)

        group = store.dbq("SELECT group_id,judge_order,presented_json,shortlist_json FROM photo_ai_groups")[0]
        presented = json.loads(group["presented_json"])
        self.assertEqual(group["judge_order"], "shuffled")
        self.assertEqual(sorted(presented), sorted(json.loads(group["shortlist_json"])))
        # FakeGeminiは見せた1枚目を推奨し、keep_scoreを見せた順に89, 88, ...と付ける。
        recommended = [row["photo_id"] for row in store.dbq(
            "SELECT photo_id FROM photo_ai_group_members WHERE recommended=1")]
        self.assertEqual(recommended, [presented[0]])
        scores = {row["photo_id"]: row["keep_score"] for row in store.dbq(
            "SELECT photo_id,keep_score FROM photo_ai_features")}
        self.assertEqual([scores[pid] for pid in presented], [89, 88, 87, 86])

        ai_pipeline.ai_cycle(embed_limit=10, judge_limit=1)
        kept = store.dbq("SELECT group_id,judge_order,presented_json FROM photo_ai_groups")[0]
        self.assertEqual(tuple(kept), (group["group_id"], "shuffled", group["presented_json"]))
