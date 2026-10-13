import threading
import time
import unittest
from unittest.mock import patch

from geoframe import ai_search


def row(pid, vector, group=None, recommended=None, keep=None):
    return {
        "id": pid, "name": f"photo-{pid}.jpg", "display": f"derivatives/{pid}.jpg",
        "taken_at": "2026-08-05 10:00", "place": "", "pref": "", "city": "",
        "lat": 35.0, "lon": 139.0,
        "favorite": 0, "hidden": 0, "embedding": ai_search.pack_embedding(vector),
        "embedding_dim": len(vector), "embedding_model": "test", "subject": "dog",
        "pet_state": "active", "eyes": "open", "face_crop": "good",
        "horizon_status": "level", "horizon_score": 95,
        "subject_sharpness": 80, "aesthetic_score": 80, "keep_score": keep,
        "recommended": recommended, "reasons_json": "[]", "concerns_json": "[]",
        "ai_group_id": group, "quality_group_id": None, "quality_group_kind": None,
        "quality_recommended": None, "quality_score": None,
    }


class AiSearchTest(unittest.TestCase):
    def setUp(self):
        # モジュールグローバルの窓・キャッシュを毎回リセットして順序非依存にする。
        # 怠ると意味的検索のテストが無関係な AiSearchRateLimited で落ちる。
        ai_search._search_win.clear()
        ai_search._search_throttled = False
        ai_search._query_cache.clear()
        ai_search._client = None
        self.original_client_class = ai_search.AgentPlatformEmbeddingClient

    def tearDown(self):
        ai_search.AgentPlatformEmbeddingClient = self.original_client_class
        ai_search._search_win.clear()
        ai_search._search_throttled = False
        ai_search._query_cache.clear()
        ai_search._client = None

    def test_query_cache_is_separated_by_embedding_model(self):
        calls = []

        class Client:
            def embed_text(self, text):
                calls.append(text)
                return [1.0, 0.0, 0.0]

        ai_search._client = Client()
        with patch.object(ai_search, "EMBED_MODEL", "model-a"):
            ai_search._embed_query("犬の写真")
            ai_search._embed_query("犬の写真")
        with patch.object(ai_search, "EMBED_MODEL", "model-b"):
            ai_search._embed_query("犬の写真")
        self.assertEqual(len(calls), 2)

    def test_float32_embedding_round_trip_and_cosine(self):
        values = [0.25, -0.5, 0.75]
        unpacked = ai_search.unpack_embedding(ai_search.pack_embedding(values), 3)
        self.assertEqual(len(unpacked), 3)
        self.assertAlmostEqual(ai_search.cosine(unpacked, values), 1.0, places=6)

    def test_visual_clusters_collapse_close_compositions_only(self):
        rows = [
            row(1, [1.0, 0.0, 0.0]),
            row(2, [0.99, 0.05, 0.0]),
            row(3, [0.0, 1.0, 0.0]),
        ]
        assignments, count = ai_search._visual_clusters(rows)
        self.assertEqual(count, 2)
        self.assertTrue(assignments[1]["visual_representative"])
        self.assertFalse(assignments[2]["visual_representative"])
        self.assertEqual(assignments[1]["visual_cluster_size"], 2)
        self.assertTrue(assignments[3]["visual_representative"])

    def test_semantic_search_groups_same_scene_and_orders_members(self):
        rows = [
            row(1, [0.9, 0.1, 0.0], "scene-a", recommended=0, keep=60),
            row(2, [0.8, 0.2, 0.0], "scene-a", recommended=1, keep=90),
            row(3, [0.0, 1.0, 0.0], None, recommended=None, keep=None),
        ]
        original_valid = ai_search._valid_rows
        original_embed = ai_search._embed_query
        original_dbq = ai_search.dbq
        try:
            ai_search._valid_rows = lambda include_hidden: rows
            ai_search._embed_query = lambda query: [1.0, 0.0, 0.0]
            ai_search.dbq = lambda sql, args=(): [{
                "group_id": "scene-a", "kind": "scene", "summary": "同じ場面",
                "min_similarity": 0.95, "max_similarity": 0.95,
                "avg_similarity": 0.95,
            }]
            result = ai_search.api_ai_search("走っている犬", group_limit=2)
        finally:
            ai_search._valid_rows = original_valid
            ai_search._embed_query = original_embed
            ai_search.dbq = original_dbq
        self.assertEqual(result["mode"], "semantic")
        self.assertEqual(result["groups"][0]["group_id"], "scene-a")
        self.assertEqual([p["id"] for p in result["groups"][0]["photos"]], [2, 1])
        self.assertEqual(result["groups"][0]["summary"], "同じ場面")
        self.assertEqual(result["groups"][0]["visual_cluster_count"], 1)
        self.assertTrue(result["groups"][0]["photos"][0]["visual_representative"])
        self.assertGreater(result["groups"][0]["photos"][1]["visual_similarity"], 0.9)

    # 上限は環境変数で上書きできる（GEOFRAME_AI_SEARCH_PER_MIN）。実値でループすると
    # 大きな値を設定した環境でテストが事実上ハングするため、小さな既知の値に固定する。
    # 固定した値どおりに抑制されること自体が config → 実装の結線チェックにもなる。
    LIMIT = 5

    def test_rate_limit_allows_up_to_limit_then_blocks(self):
        with patch.object(ai_search, "AI_SEARCH_PER_MIN", self.LIMIT):
            for i in range(self.LIMIT):
                self.assertTrue(ai_search._search_rate_limit_allowed(),
                                f"{i + 1}件目で誤って抑制された")
            self.assertFalse(ai_search._search_rate_limit_allowed())
            self.assertTrue(ai_search._search_throttled)

    def test_rate_limit_window_releases_after_60_seconds(self):
        with patch.object(ai_search, "AI_SEARCH_PER_MIN", self.LIMIT):
            with patch("geoframe.ai_search.time.monotonic", return_value=1000.0):
                for _ in range(self.LIMIT):
                    self.assertTrue(ai_search._search_rate_limit_allowed())
                self.assertFalse(ai_search._search_rate_limit_allowed())
            # 窓の途中（59.9秒後）ではまだ解放されない
            with patch("geoframe.ai_search.time.monotonic", return_value=1059.9):
                self.assertFalse(ai_search._search_rate_limit_allowed())
            with patch("geoframe.ai_search.time.monotonic", return_value=1060.0):
                self.assertTrue(ai_search._search_rate_limit_allowed())
                self.assertFalse(ai_search._search_throttled)

    def test_rate_limited_search_never_reaches_database(self):
        calls = []
        original_dbq = ai_search.dbq
        try:
            ai_search.dbq = lambda sql, args=(): calls.append(sql) or []
            with patch.object(ai_search, "AI_SEARCH_PER_MIN", self.LIMIT):
                for _ in range(self.LIMIT):
                    ai_search._search_rate_limit_allowed()
                with self.assertRaises(ai_search.AiSearchRateLimited):
                    ai_search.api_ai_search("走っている犬")
        finally:
            ai_search.dbq = original_dbq
        self.assertEqual(calls, [], "抑制中にDBフェッチへ到達している")

    def test_disabled_ai_raises_before_any_work(self):
        with patch.object(ai_search, "AI_ENABLED", False):
            with self.assertRaises(ai_search.AiSearchDisabled):
                ai_search.api_ai_search("走っている犬")
        # 親クラスの節で拾えるフェイルセーフになっていること
        self.assertTrue(issubclass(ai_search.AiSearchDisabled, ai_search.AiSearchUnavailable))
        # 無効化時は流量窓を消費しない
        self.assertEqual(len(ai_search._search_win), 0)

    def test_concurrent_embed_query_is_idempotent_without_holding_lock(self):
        vector = [0.5, 0.25, 0.125]
        calls = []
        lock_free = []
        call_lock = threading.Lock()

        class FakeClient:
            def embed_text(self, text):
                # 外部HTTP相当の呼び出しが _query_lock の外で行われていること。
                # 握ったままなら acquire は False になる（threading.Lock は非reentrantで、
                # 同一スレッドが保持していても取得に失敗するため確実に検出できる）。
                acquired = ai_search._query_lock.acquire(blocking=False)
                if acquired:
                    ai_search._query_lock.release()
                lock_free.append(acquired)
                with call_lock:
                    calls.append(text)
                time.sleep(0.05)      # 競合窓を広げ、ロック外でI/Oしていることを確かめる
                return list(vector)

            @staticmethod
            def from_environment():
                return FakeClient()

        ai_search.AgentPlatformEmbeddingClient = FakeClient
        results, errors = [], []

        def worker():
            try:
                results.append(ai_search._embed_query("走っている犬"))
            except Exception as exc:      # スレッド内例外を本体へ伝える
                errors.append(exc)

        threads = [threading.Thread(target=worker) for _ in range(8)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=10)
            self.assertFalse(thread.is_alive(), "_embed_query がハングしている")
        self.assertEqual(errors, [])
        self.assertEqual(len(results), 8)
        # 重複呼び出しは許容する（同一クエリは決定的に同一結果なので冪等）。
        # 呼び出し回数を断定するとスケジューリング依存でflakyになるため検証しない。
        self.assertTrue(all(r == vector for r in results))
        self.assertEqual(len(ai_search._query_cache), 1)
        self.assertGreaterEqual(len(calls), 1)
        self.assertTrue(lock_free and all(lock_free),
                        "embed_text が _query_lock を握ったまま呼ばれている"
                        "（1件の遅い呼び出しで全検索が直列に足止めされる）")


if __name__ == "__main__":
    unittest.main()
