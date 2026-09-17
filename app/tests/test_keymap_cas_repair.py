import http.client
import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, keymap, store


class InvalidKeymapCASRepairTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.old_db = store._db
        store._db = None
        self.db_path = Path(self.tmp.name) / "catalog.db"
        self.db_patch = patch.object(store, "DB_PATH", self.db_path)
        self.db_patch.start()
        self.addCleanup(self.db_patch.stop)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handlers.Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)
        self.addCleanup(self.restore_db)

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def request(self, method, path, body=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=3)
        headers = {"Host": "127.0.0.1"}
        encoded = None
        if body is not None:
            encoded = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
            headers["Content-Length"] = str(len(encoded))
        conn.request(method, path, encoded, headers)
        response = conn.getresponse()
        raw = response.read()
        result = json.loads(raw) if raw else None
        status = response.status
        conn.close()
        return status, result

    def restore_db(self):
        if store._db is not None:
            store._db.close()
        store._db = self.old_db

    def put_raw(self, document):
        store.dbx(
            "INSERT INTO settings(key, value) VALUES(?, ?)",
            (handlers.KEYMAP_SETTING, json.dumps(document)),
        )

    def test_invalid_revision_is_repaired_over_http(self):
        self.put_raw({"rev": "abc", "updated_at": 123, "keys": dict(keymap.DEFAULT_KEYS)})
        status, before = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(before["rev"], 0)
        self.assertEqual(before["keys"], keymap.DEFAULT_KEYS)
        status, repaired = self.request(
            "POST", "/api/admin/setkeymap",
            {"keys": dict(keymap.DEFAULT_KEYS), "base_revision": 0},
        )
        self.assertEqual(status, 200)
        self.assertEqual(repaired["rev"], 1)
        status, after = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(after["rev"], 1)
        self.assertEqual(after["keys"], keymap.DEFAULT_KEYS)

    def test_invalid_keys_with_numeric_revision_is_repaired_over_http(self):
        self.put_raw({"rev": 7, "updated_at": "old", "keys": {"0": "sleep"}})
        status, before = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(before["rev"], 0)
        self.assertEqual(before["keys"], keymap.DEFAULT_KEYS)
        status, repaired = self.request(
            "POST", "/api/admin/setkeymap",
            {"keys": dict(keymap.DEFAULT_KEYS), "base_revision": 0},
        )
        self.assertEqual(status, 200)
        self.assertEqual(repaired["rev"], 1)
        status, after = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(after["rev"], 1)
        self.assertEqual(after["keys"], keymap.DEFAULT_KEYS)

    def test_invalid_revision_is_treated_as_unset_and_repaired(self):
        self.put_raw({"rev": "abc", "updated_at": 123, "keys": dict(keymap.DEFAULT_KEYS)})
        result = store.setting_compare_and_set(
            handlers.KEYMAP_SETTING, {"keys": dict(keymap.DEFAULT_KEYS)}, expected_rev=0
        )
        self.assertEqual(result["rev"], 1)
        self.assertEqual(keymap.render(result)["keys"], keymap.DEFAULT_KEYS)

    def test_invalid_keys_with_numeric_revision_is_treated_as_unset_and_repaired(self):
        self.put_raw({"rev": 7, "updated_at": "old", "keys": {"0": "sleep"}})
        result = store.setting_compare_and_set(
            handlers.KEYMAP_SETTING, {"keys": dict(keymap.DEFAULT_KEYS)}, expected_rev=0
        )
        self.assertEqual(result["rev"], 1)
        self.assertEqual(keymap.render(result)["keys"], keymap.DEFAULT_KEYS)


if __name__ == "__main__":
    unittest.main()
