"""位置なし写真の撮影地候補（Decisionで足切り → Geminiで推定 → Nominatimで座標化）のテスト。"""
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from PIL import Image

from geoframe import admin, loc_suggest, store
from geoframe.ai_search import AiSearchUnavailable


def _answer(place, query, precision="city", photos=(), confidence="high"):
    value = {"country": "エジプト", "country_code": "EG", "city": "", "place_name": place,
             "geocode_query": query, "confidence": confidence, "precision": precision,
             "evidence": ["看板に「Karnak」とある"], "photos": list(photos)}
    return {"candidates": [{"content": {"parts": [{"text": json.dumps(value, ensure_ascii=False)}]}}]}


class FakeGemini:
    def __init__(self):
        self.calls = []
        self.responses = []

    def post(self, model, method, payload, timeout=60):
        parts = payload["contents"][0]["parts"]
        self.calls.append({"model": model,
                           "images": sum("inlineData" in p for p in parts),
                           "text": parts[0]["text"]})
        res = self.responses.pop(0) if self.responses else _answer("ルクソール", "Luxor, Egypt")
        if isinstance(res, Exception):
            raise res
        return res


GEO = {"Luxor, Egypt": (25.69, 32.64), "Karnak Temple, Luxor": (25.72, 32.66), "Giza": (30.0, 31.2)}


class LocSuggestCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "derivatives").mkdir()
        self.original_db = store._db
        self.original_db_path = store.DB_PATH
        store._db = None
        store.DB_PATH = self.root / "catalog.db"
        store.db()
        self.gemini = FakeGemini()
        self.clues = {}               # photo_id → {"clue","kind"} か 例外
        self.ready = True
        self.asked = []
        gemini = self.gemini

        class Factory:
            @staticmethod
            def from_environment():
                return gemini

        def fake_ask(image, model):
            pid = Image.open(io.BytesIO(image)).width - 40
            self.asked.append(pid)
            res = self.clues[pid]
            if isinstance(res, Exception):
                raise res
            return res

        self.geocoded = []

        def fake_nominatim(q, countrycode=""):
            self.geocoded.append((q, countrycode))
            if q in GEO:
                lat, lon = GEO[q]
                return [{"title": q, "lat": lat, "lon": lon, "source": "osm"}]
            return []

        for patcher in (
                patch.object(loc_suggest, "DATA_DIR", self.root),
                patch.object(loc_suggest, "DERIV_DIR", self.root / "derivatives"),
                patch.object(loc_suggest, "AgentPlatformEmbeddingClient", Factory),
                patch.object(loc_suggest, "decision_health",
                             lambda timeout=2.0: {"status": "ready", "model": "d3-lite",
                                                  "modalities": ["text", "image"]}
                             if self.ready else None),
                patch.object(loc_suggest, "decision_ask", fake_ask),
                patch.object(loc_suggest, "nominatim_search", fake_nominatim)):
            patcher.start()
            self.addCleanup(patcher.stop)

    def tearDown(self):
        if store._db is not None:
            store._db.close()
        store._db = self.original_db
        store.DB_PATH = self.original_db_path
        self.temp.cleanup()

    def add(self, pid, taken_at, clue=None, kind="street", lat=None):
        """写真を1枚足す。画像の幅にidを入れ、偽Decisionが見分けられるようにする
        （JPEGの再圧縮で画素値は変わるが、幅は変わらない）"""
        Image.new("RGB", (40 + pid, 30), (90, 90, 90)).save(self.root / "derivatives" / f"{pid}.jpg")
        store.dbx("""INSERT INTO photos(id,source,display,name,md5,taken_at,lat,lon)
                     VALUES(?,'drive',?,?,?,?,?,?)""",
                  (pid, f"derivatives/{pid}.jpg", f"{pid}.jpg", f"md5-{pid}", taken_at,
                   lat, None if lat is None else 139.0))
        if clue is not None:
            self.clues[pid] = {"clue": clue, "kind": kind}

    def suggestions(self):
        return {r["photo_id"]: r for r in store.dbq("SELECT * FROM photo_loc_suggestions")}


class GateAndGroupingTest(LocSuggestCase):
    def _p(self, pid, clue, kind, error=""):
        return {"id": pid, "clue": clue, "kind": kind, "clue_error": error}

    def test_gate_and_ranking(self):
        photos = [self._p(1, 0.15, "landmark"), self._p(2, 0.5, "street"),
                  self._p(3, 0.19, "street"), self._p(4, 0.9, "street", error="HTTP 422"),
                  self._p(5, 0.3, "landmark"), self._p(6, None, "")]
        self.assertEqual([p["id"] for p in loc_suggest.pick_send(photos)], [5, 1, 2])

    def test_send_is_capped(self):
        photos = [self._p(i, 0.5 + i / 100, "street") for i in range(1, 10)]
        sent = loc_suggest.pick_send(photos)
        self.assertEqual(len(sent), loc_suggest.MAX_SEND)
        self.assertEqual(sent[0]["id"], 9)

    def test_group_days_splits_on_gap_and_date(self):
        def p(pid, taken_at):
            from geoframe.util import t_epoch
            return {"id": pid, "taken_at": taken_at, "epoch": t_epoch(taken_at)}
        groups = loc_suggest.group_days([
            p(1, "2001-06-19 08:00"), p(2, "2001-06-19 19:00"),
            p(3, "2001-06-20 00:30"),                  # 日付が変わったので別
            p(4, "2001-06-25 10:00")])                 # 12時間以上空いたので別
        self.assertEqual([[x["id"] for x in g] for g in groups], [[1, 2], [3], [4]])


class CycleTest(LocSuggestCase):
    def test_sends_only_photos_with_clues_and_writes_candidates(self):
        self.add(1, "2001-06-20 09:00", 0.3, "landmark")
        self.add(2, "2001-06-20 10:00", 0.25, "street")
        self.add(3, "2001-06-20 11:00", 0.03, "animal_person")      # ペット: 送らない
        self.add(4, "2001-06-22 10:00", 0.02, "indoor")             # 手がかりの無い日
        self.gemini.responses = [_answer(
            "ルクソール", "Luxor, Egypt", photos=[
                {"index": 1, "place_name": "カルナック神殿", "geocode_query": "Karnak Temple, Luxor",
                 "confidence": "high", "precision": "landmark"}])]
        result = loc_suggest.loc_suggest_cycle()
        self.assertEqual(result["decision"], "ready")
        self.assertEqual(len(self.gemini.calls), 1)
        self.assertEqual(self.gemini.calls[0]["images"], 2)
        self.assertEqual(self.gemini.calls[0]["model"], loc_suggest.LOC_SUGGEST_MODEL)
        self.assertIn("2001-06-20", self.gemini.calls[0]["text"])
        s = self.suggestions()
        self.assertEqual(set(s), {1, 2, 3})
        self.assertEqual((s[1]["scope"], s[1]["place_name"], s[1]["precision"], s[1]["lat"]),
                         ("photo", "カルナック神殿", "landmark", 25.72))
        self.assertEqual((s[3]["scope"], s[3]["place_name"], s[3]["lat"]),
                         ("day", "ルクソール", 25.69))
        # 位置そのものは書かない
        self.assertEqual(store.dbq("SELECT COUNT(*) c FROM photos WHERE lat IS NOT NULL")[0]["c"], 0)

    def test_second_cycle_does_not_ask_again(self):
        self.add(1, "2001-06-20 09:00", 0.3, "landmark")
        loc_suggest.loc_suggest_cycle()
        loc_suggest.loc_suggest_cycle()
        self.assertEqual(len(self.gemini.calls), 1)
        self.assertEqual(self.asked, [1])

    def test_decision_down_sends_nothing(self):
        self.ready = False
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        result = loc_suggest.loc_suggest_cycle()
        self.assertEqual(result["decision"], "unavailable")
        self.assertEqual(self.gemini.calls, [])
        self.assertEqual(self.asked, [])

    def test_unclassified_photo_holds_its_day(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-20 10:00", 0.1, "street")
        loc_suggest.loc_suggest_cycle(clue_limit=1)       # 2枚目が未判定のまま
        self.assertEqual(self.gemini.calls, [])
        loc_suggest.loc_suggest_cycle(clue_limit=1)
        self.assertEqual(len(self.gemini.calls), 1)

    def test_decision_lost_mid_cycle_stops_classification(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-20 10:00")
        self.clues[2] = loc_suggest.DecisionUnavailable("connection refused")
        result = loc_suggest.loc_suggest_cycle()
        self.assertEqual(result["decision"], "unavailable")
        self.assertEqual(self.gemini.calls, [])
        self.assertEqual(store.dbq("SELECT photo_id FROM photo_loc_clues")[0]["photo_id"], 1)

    def test_per_photo_error_does_not_block_the_day(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-20 10:00")
        self.clues[2] = ValueError("Decision HTTP 422")
        loc_suggest.loc_suggest_cycle()
        self.assertEqual(len(self.gemini.calls), 1)
        self.assertEqual(self.gemini.calls[0]["images"], 1)
        row = store.dbq("SELECT * FROM photo_loc_clues WHERE photo_id=2")[0]
        self.assertIn("422", row["error"])
        self.assertIn(2, self.suggestions())             # 失敗した写真もその日の候補は持つ

    def test_gemini_failure_is_retried_then_given_up(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.gemini.responses = [RuntimeError("boom")] * loc_suggest.MAX_ATTEMPTS
        for _ in range(loc_suggest.MAX_ATTEMPTS + 2):
            loc_suggest.loc_suggest_cycle()
        self.assertEqual(len(self.gemini.calls), loc_suggest.MAX_ATTEMPTS)
        run = store.dbq("SELECT * FROM photo_loc_runs")[0]
        self.assertEqual((run["status"], run["attempts"]), ("error", loc_suggest.MAX_ATTEMPTS))

    def test_auth_failure_stops_the_cycle(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-21 09:00", 0.9, "landmark")
        self.gemini.responses = [AiSearchUnavailable("no credentials")]
        result = loc_suggest.loc_suggest_cycle()
        self.assertEqual(len(self.gemini.calls), 1)
        self.assertEqual(result["asked"], 1)

    def test_gemini_limit_per_cycle(self):
        for day in range(1, 5):
            self.add(day, f"2001-06-0{day} 09:00", 0.9, "landmark")
        loc_suggest.loc_suggest_cycle(gemini_limit=2)
        self.assertEqual(len(self.gemini.calls), 2)

    def test_photo_geocode_miss_falls_back_to_day_point_and_zoom(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.gemini.responses = [_answer("ルクソール", "Luxor, Egypt", photos=[
            {"index": 1, "place_name": "どこかの神殿", "geocode_query": "Unknown Temple",
             "confidence": "high", "precision": "landmark"}])]
        loc_suggest.loc_suggest_cycle()
        row = self.suggestions()[1]
        self.assertEqual((row["place_name"], row["lat"], row["precision"]),
                         ("どこかの神殿", 25.69, "city"))

    def test_unknown_answer_has_no_point(self):
        self.add(1, "2012-05-25 20:00", 0.3, "street")
        self.gemini.responses = [_answer("日本", "Japan", precision="unknown", confidence="unknown")]
        loc_suggest.loc_suggest_cycle()
        row = self.suggestions()[1]
        self.assertEqual((row["place_name"], row["lat"]), ("日本", None))

    def test_pinning_one_photo_reuses_the_answer(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-20 10:00", 0.8, "landmark")
        self.add(3, "2001-06-20 11:00", 0.02, "indoor")
        loc_suggest.loc_suggest_cycle()
        # 1枚目を手動で決めた（送った写真の組が変わる）→ 同じ日の答えを使い回す
        store.dbx("UPDATE photos SET lat=25.7, lon=32.6, loc_source='manual' WHERE id=1")
        loc_suggest.loc_suggest_cycle()
        self.assertEqual(len(self.gemini.calls), 1)


    def test_country_code_is_passed_to_geocoder(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        loc_suggest.loc_suggest_cycle()
        self.assertIn(("Luxor, Egypt", "EG"), self.geocoded)

    def test_combined_place_falls_back_to_broader_part(self):
        self.add(1, "2001-06-19 15:00", 0.9, "landmark")
        self.gemini.responses = [_answer("ギザの台地", "Giza Plateau, Giza", precision="landmark")]
        loc_suggest.loc_suggest_cycle()
        row = self.suggestions()[1]
        self.assertEqual((row["lat"], row["precision"]), (30.0, "city"))
        self.assertEqual(self.geocoded[-2:], [("Giza Plateau, Giza", "EG"), ("Giza", "EG")])


class AdminApiTest(LocSuggestCase):
    def test_suggestion_appears_only_while_location_is_missing(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-20 10:00", 0.9, "landmark")
        loc_suggest.loc_suggest_cycle()
        page = admin.api_admin_photos(filt="suggest")
        self.assertEqual([i["id"] for i in page["items"]], [1, 2])
        self.assertEqual(page["items"][0]["suggest"]["place_name"], "ルクソール")
        self.assertEqual(page["items"][0]["suggest"]["evidence"], ["看板に「Karnak」とある"])
        store.dbx("UPDATE photos SET lat=25.7, lon=32.6, loc_source='manual' WHERE id=1")
        page = admin.api_admin_photos(filt="suggest")
        self.assertEqual([i["id"] for i in page["items"]], [2])
        page = admin.api_admin_photos(filt="all")
        self.assertNotIn("suggest", next(i for i in page["items"] if i["id"] == 1))

    def test_status_counts(self):
        self.add(1, "2001-06-20 09:00", 0.9, "landmark")
        self.add(2, "2001-06-22 09:00", 0.01, "indoor")
        self.add(3, "2001-06-23 09:00", 0.9, "landmark")
        loc_suggest.loc_suggest_cycle(gemini_limit=1)
        st = loc_suggest.loc_status()
        self.assertEqual((st["decision"], st["targets"], st["clue_pending"], st["clue_passed"]),
                         ("ready", 3, 0, 2))
        self.assertEqual((st["days"], st["days_with_clue"], st["runs_done"], st["suggested"]),
                         (3, 2, 1, 1))


class CacheTest(unittest.TestCase):
    def test_geocode_cache_survives_legacy_purge(self):
        with patch.object(store, "_cache", {"geocode_osm:サマルカンド": [], "geo:1,2": {},
                                            "exif:old": 1}):
            store.purge_cache_legacy()
            self.assertEqual(set(store._cache), {"geocode_osm:サマルカンド", "geo:1,2"})


if __name__ == "__main__":
    unittest.main()
