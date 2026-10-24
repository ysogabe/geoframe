"""表示クライアント（miharashi）との写真フィード契約。

docs/architecture/photo-feed-contract.md に書いたフィールドと配信パスが変わったら、
ここで落ちるようにする。契約を変えるときは miharashi 側の読み取りも合わせて直すこと。

応答の形は docs/architecture/photo-feed.schema.json にも定め、Rust側（crates/geoframe/tests）と
同じSchemaで検査する。Rustの契約テストが読むフィクスチャは、ここで現行実装から再生成して
差がないことを確かめる（再生成: python3 tools/gen_photo_feed_fixture.py）。
"""
import http.client
import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, photos

from tests import mini_jsonschema, photo_feed_fixture

# miharashi が読むフィールドと、その JSON 型
CONTRACT_FIELDS = {
    "id": int,
    "file": str,
    "time": str,
    "lat": float,
    "lon": float,
    "pref": str,
    "city": str,
    "title": str,
    "fav": bool,
    "fetched_at": str,
    "quality_kind": str,
    "quality_recommended": bool,
    "trip": int,
}


def _row(**over):
    row = {
        "id": 1, "fetched_at": "", "favorite": 0, "display": "derivatives/a.jpg",
        "name": "IMG_0001.jpg", "title": "", "city": "", "place": "", "pref": "",
        "lat": 35.0, "lon": 139.0, "elev": None, "taken_at": "2025-06-06 10:00",
        "loc_source": "exif",
        "quality_score": None, "quality_group": None, "quality_kind": None,
        "quality_rank": None, "quality_recommended": None,
        "ai_group": None, "ai_status": None, "ai_same_scene": None, "ai_rank": None,
        "ai_recommended": None, "ai_local_score": None,
    }
    row.update(over)
    return row


ROWS = [
    _row(),
    _row(id=2, favorite=1, display="derivatives/b.jpg", pref="架空県", city="見本市",
         place="サンプル町", taken_at="2025-06-07 11:00",
         quality_kind="similar", quality_score=0.9, quality_group=1,
         quality_rank=1, quality_recommended=1),
    _row(id=3, display="derivatives/c.jpg", taken_at=""),
]


class PhotoFeedFieldsTest(unittest.TestCase):
    def setUp(self):
        with patch.object(photos, "dbq", return_value=ROWS):
            self.items = photos.api_photos()

    def test_contract_fields_and_types(self):
        self.assertEqual(len(self.items), 3)
        for item in self.items:
            for name, kind in CONTRACT_FIELDS.items():
                self.assertIn(name, item)
                self.assertIsInstance(item[name], kind, name)

    def test_file_is_a_relative_derivative_path(self):
        for item in self.items:
            self.assertTrue(item["file"].startswith("derivatives/"), item["file"])

    def test_values(self):
        first, second, third = self.items
        self.assertEqual((first["fav"], first["quality_recommended"]), (False, False))
        self.assertEqual(first["title"], "IMG_0001")   # 地名が無ければファイル名
        self.assertEqual((second["fav"], second["quality_recommended"]), (True, True))
        self.assertEqual((second["pref"], second["title"]), ("架空県", "見本市 サンプル町"))
        self.assertEqual(second["time"], "2025-06-07 11:00")
        self.assertEqual(third["time"], "")             # 撮影時刻不明は空文字
        self.assertEqual([i["id"] for i in self.items], [1, 2, 3])
        self.assertEqual((second["city"], second["lat"], second["lon"]), ("見本市", 35.0, 139.0))
        self.assertEqual((first["city"], first["pref"]), ("", ""))   # 住所なし（海外など）
        self.assertEqual((first["quality_kind"], second["quality_kind"]), ("", "similar"))

    def test_trip_numbers_split_at_48_hour_gaps(self):
        rows = [_row(id=1, taken_at="2025-06-01 10:00"),
                _row(id=2, taken_at="2025-06-03 09:00"),   # 47時間後: 同じ旅
                _row(id=3, taken_at="2025-06-05 10:00"),   # 49時間後: 次の旅
                _row(id=4, taken_at="")]
        with patch.object(photos, "dbq", return_value=rows):
            items = photos.api_photos()
        self.assertEqual([i["trip"] for i in items], [0, 0, 1, 2])


SCHEMA = json.loads(photo_feed_fixture.SCHEMA_PATH.read_text(encoding="utf-8"))


class PhotoFeedSchemaTest(unittest.TestCase):
    def test_output_satisfies_the_schema(self):
        with patch.object(photos, "dbq", return_value=ROWS):
            items = photos.api_photos()
        self.assertEqual(mini_jsonschema.errors(items, SCHEMA), [])

    def test_schema_requires_exactly_the_contract_fields(self):
        required = set(SCHEMA["$defs"]["photo"]["required"])
        self.assertEqual(required, set(CONTRACT_FIELDS))
        # JSON型は上の CONTRACT_FIELDS と同じ（floatのlat/lonはSchemaでは number）
        kinds = {"integer": int, "string": str, "boolean": bool, "number": float}
        for name, prop in SCHEMA["$defs"]["photo"]["properties"].items():
            if "type" in prop:
                self.assertIs(kinds[prop["type"]], CONTRACT_FIELDS[name], name)

    def test_checker_rejects_broken_items(self):
        good = {"id": 1, "file": "derivatives/a.jpg", "time": "2025-06-07 11:00", "lat": 35.0,
                "lon": 139.0, "pref": "", "city": "", "title": "t", "fav": False,
                "fetched_at": "2025-06-08T09:30:00", "quality_kind": "", "quality_recommended": False,
                "trip": 0}
        self.assertEqual(mini_jsonschema.errors([good], SCHEMA), [])
        broken = [("id", "1"), ("id", True), ("file", "/derivatives/a.jpg"), ("time", "2025-06-07"),
                  ("time", "2025-06-07 11:00\n"), ("fetched_at", "2025-06-08T09:30:00\n"),
                  ("lat", "35.0"), ("lat", 91.0), ("title", ""), ("fav", 1),
                  ("fetched_at", "2025-06-08 09:30:00"), ("quality_kind", "other"),
                  ("quality_recommended", None), ("trip", -1), ("trip", 0.5)]
        for name, value in broken:
            self.assertTrue(mini_jsonschema.errors([{**good, name: value}], SCHEMA), (name, value))
        for name in good:
            rest = {k: v for k, v in good.items() if k != name}
            self.assertTrue(mini_jsonschema.errors([rest], SCHEMA), name)
        self.assertTrue(mini_jsonschema.errors({}, SCHEMA))   # 配列でない


class PhotoFeedFixtureTest(unittest.TestCase):
    """Rustの契約テストが読むフィクスチャが、現行実装の出力と一致している。"""

    @classmethod
    def setUpClass(cls):
        cls.items = photo_feed_fixture.build_items()

    def test_fixture_file_is_up_to_date(self):
        text = photo_feed_fixture.FIXTURE_PATH.read_text(encoding="utf-8")
        self.assertEqual(
            text, photo_feed_fixture.render(self.items),
            "再生成: python3 tools/gen_photo_feed_fixture.py")

    def test_real_schema_output_satisfies_the_schema(self):
        self.assertEqual(mini_jsonschema.errors(self.items, SCHEMA), [])

    def test_null_taken_at_comes_out_as_null_and_first(self):
        # 現行実装の挙動の記録（T1.2 で Rust の正規化方針を決める）。契約は time を文字列と
        # するが、taken_at=NULL の行は `time: null` で先頭に並び、それだけで1つの旅になる。
        # 本番コピーには NULL の行は無い（管理者確認済み）
        row = photo_feed_fixture.photo_row(14, taken_at=None, lat=35.6, lon=139.6)
        items = photo_feed_fixture.build_items([row])
        self.assertEqual([i["id"] for i in items], [14, 1, 2, 3, 4, 6, 8, 9, 10])
        self.assertIsNone(items[0]["time"])
        self.assertEqual(items[0]["trip"], 0)
        self.assertEqual([i["trip"] for i in items[1:]], [1, 1, 1, 2, 2, 2, 2, 3])
        self.assertTrue(mini_jsonschema.errors(items, SCHEMA))   # 契約（string）には合わない

    def test_fixture_covers_the_selection_rules(self):
        ids = [i["id"] for i in self.items]
        # 除外: exact推奨外(5)、AI同一場面の推奨外(7)、非表示(11)、表示画像なし(12)、位置なし(13)
        self.assertEqual(ids, [1, 2, 3, 4, 6, 8, 9, 10])
        by_id = {i["id"]: i for i in self.items}
        self.assertEqual((by_id[3]["quality_kind"], by_id[3]["quality_recommended"]), ("similar", False))
        self.assertTrue(by_id[8]["fav"])                        # AI推奨外でもお気に入りは残る
        self.assertEqual(by_id[10]["time"], "")                 # 時刻不明は最後
        self.assertEqual([by_id[i]["trip"] for i in ids], [0, 0, 0, 1, 1, 1, 1, 2])
        self.assertIsNone(by_id[1]["elev"])                     # 内部値のnullも含む
        self.assertEqual(by_id[2]["quality_group"], 1)          # 品質グループ番号は整数


class PhotoFeedHttpTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        data = Path(self.tmp.name)
        (data / "derivatives").mkdir()
        (data / "derivatives" / "a.jpg").write_bytes(b"\xff\xd8\xff\xd9")
        self.patches = [patch.object(photos, "dbq", return_value=ROWS),
                        patch.object(handlers, "DATA_DIR", data)]
        for p in self.patches:
            p.start()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handlers.Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()

    def get(self, path):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=3)
        try:
            conn.request("GET", path, headers={"Host": "127.0.0.1"})
            response = conn.getresponse()
            return response.status, response.getheader("Content-Type"), response.read()
        finally:
            conn.close()

    def test_v1_path_returns_the_list(self):
        status, _, body = self.get("/api/v1/photos")
        self.assertEqual(status, 200)
        items = json.loads(body)
        self.assertIsInstance(items, list)
        self.assertEqual([i["file"] for i in items],
                         ["derivatives/a.jpg", "derivatives/b.jpg", "derivatives/c.jpg"])

    def test_file_is_served_relative_to_base_url(self):
        status, ctype, body = self.get("/derivatives/a.jpg")
        self.assertEqual(status, 200)
        self.assertEqual(ctype, "image/jpeg")
        self.assertEqual(body, b"\xff\xd8\xff\xd9")


if __name__ == "__main__":
    unittest.main()
