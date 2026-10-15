"""表示クライアント（miharashi）との写真フィード契約。

docs/architecture/photo-feed-contract.md に書いたフィールドと配信パスが変わったら、
ここで落ちるようにする。契約を変えるときは miharashi 側の読み取りも合わせて直すこと。
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
    _row(id=2, favorite=1, display="derivatives/b.jpg", pref="千葉県", city="千葉市",
         place="中央", taken_at="2025-06-07 11:00",
         quality_kind="similar", quality_score=0.9, quality_group="g1",
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
        self.assertEqual((second["pref"], second["title"]), ("千葉県", "千葉市 中央"))
        self.assertEqual(second["time"], "2025-06-07 11:00")
        self.assertEqual(third["time"], "")             # 撮影時刻不明は空文字
        self.assertEqual([i["id"] for i in self.items], [1, 2, 3])
        self.assertEqual((second["city"], second["lat"], second["lon"]), ("千葉市", 35.0, 139.0))
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
