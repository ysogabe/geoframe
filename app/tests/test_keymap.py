import contextlib
import http.client
import io
import json
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, keymap, store


# 注: "agents" は W4 で正式な語彙になったため、未知IDの例は存在しないIDに変えている
#     （テストの意図＝「語彙外IDは拒否」は変えていない）
class KeymapPureFunctionTest(unittest.TestCase):
    def valid(self):
        return {"keys": dict(keymap.DEFAULT_KEYS), "base_revision": 0}

    def test_default_and_render(self):
        self.assertEqual(keymap.DEFAULT_KEYS["0"], "sleep")
        self.assertEqual(keymap.DEFAULT_KEYS["1"], "rotation")
        self.assertEqual(set(keymap.DEFAULT_KEYS), set("0123456789"))
        self.assertEqual(keymap.render({"rev": 2, "updated_at": "x", "keys": keymap.DEFAULT_KEYS})["rev"], 2)

    def test_validation_rejects_six_invalid_shapes(self):
        cases = [
            (lambda p: p["keys"].update({"2": "no_such_mode"}), "unknown id"),
            (lambda p: p.update(extra=True), "extra field"),
            (lambda p: p["keys"].update({"2": 2}), "non string"),
            (lambda p: p["keys"].pop("9"), "missing key"),
            (lambda p: [p["keys"].__setitem__(k, "reserved") for k in p["keys"]], "no rotation"),
            (lambda p: p["keys"].update({"1": "sleep", "2": "sleep"}), "multiple sleep"),
        ]
        for mutate, name in cases:
            payload = self.valid(); mutate(payload)
            with self.subTest(name=name):
                self.assertIsInstance(keymap.validate(payload), str)

    def test_validation_rejects_all_invalid_base_revision_types(self):
        for value in (None, True, -1, "1"):
            with self.subTest(value=value):
                payload = self.valid()
                payload["base_revision"] = value
                self.assertIsInstance(keymap.validate(payload), str)

    def test_render_falls_back_for_invalid_saved_documents(self):
        invalid = [
            {"rev": 7, "updated_at": 123,
             "keys": {"0": ["not-string"], "10": "reserved"}},
            {"rev": "abc", "keys": dict(keymap.DEFAULT_KEYS)},
            {"rev": None, "keys": dict(keymap.DEFAULT_KEYS)},
            {"rev": True, "keys": dict(keymap.DEFAULT_KEYS)},
            {"rev": 1, "keys": {"0": "sleep"}},
        ]
        for doc in invalid:
            with self.subTest(doc=doc):
                result = keymap.render(doc)
                self.assertEqual(result["rev"], 0)
                self.assertIsNone(result["updated_at"])
                self.assertEqual(result["keys"], keymap.DEFAULT_KEYS)


class KeymapHTTPTest(unittest.TestCase):
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
        self.addCleanup(self._stop_server)
        self.addCleanup(self._restore_db)

    def _stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def _restore_db(self):
        if store._db is not None:
            store._db.close()
        store._db = self.old_db

    def request(self, method, path, body=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=3)
        headers = {"Host": "127.0.0.1"}
        encoded = None
        if body is not None:
            encoded = json.dumps(body, ensure_ascii=False).encode()
            headers["Content-Type"] = "application/json"
            headers["Content-Length"] = str(len(encoded))
        conn.request(method, path, encoded, headers)
        response = conn.getresponse()
        raw = response.read()
        result = json.loads(raw) if raw else None
        headers = dict(response.getheaders())
        conn.close()
        return response.status, headers, result

    @staticmethod
    def payload(revision, keys=None):
        return {"keys": dict(keymap.DEFAULT_KEYS if keys is None else keys),
                "base_revision": revision}

    def test_get_default_and_cache_control(self):
        status, headers, body = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(body["rev"], 0)
        self.assertIsNone(body["updated_at"])
        self.assertEqual(body["keys"], keymap.DEFAULT_KEYS)

    def test_post_update_increments_revision_and_journals_changed_keys(self):
        keys = dict(keymap.DEFAULT_KEYS); keys["2"] = "rotation"
        with contextlib.redirect_stdout(io.StringIO()) as output:
            status, _, body = self.request("POST", "/api/admin/setkeymap",
                                           self.payload(0, keys))
        self.assertEqual(status, 200)
        self.assertEqual(body["rev"], 1)
        self.assertIsNotNone(body["updated_at"])
        self.assertEqual(body["keys"], keys)
        journal = output.getvalue()
        self.assertIn("[journal] keymap", journal)
        self.assertIn("keys=2", journal)
        self.assertNotIn("sleep", journal)

    def test_default_post_when_unset_is_noop_and_creates_no_row(self):
        status, _, body = self.request("POST", "/api/admin/setkeymap",
                                       self.payload(0))
        self.assertEqual(status, 200)
        self.assertEqual(body["rev"], 0)
        self.assertIsNone(store.setting_get(handlers.KEYMAP_SETTING))

    def test_noop_post_keeps_revision_timestamp_and_writes_no_journal(self):
        keys = dict(keymap.DEFAULT_KEYS); keys["2"] = "rotation"
        _, _, first = self.request("POST", "/api/admin/setkeymap", self.payload(0, keys))
        with contextlib.redirect_stdout(io.StringIO()) as output:
            status, _, same = self.request("POST", "/api/admin/setkeymap",
                                           self.payload(first["rev"], keys))
        self.assertEqual(status, 200)
        self.assertEqual(same["rev"], first["rev"])
        self.assertEqual(same["updated_at"], first["updated_at"])
        self.assertEqual(output.getvalue(), "")

    def test_stale_revision_returns_409_with_current_document(self):
        keys = dict(keymap.DEFAULT_KEYS); keys["2"] = "rotation"
        self.request("POST", "/api/admin/setkeymap", self.payload(0, keys))
        other = dict(keymap.DEFAULT_KEYS); other["3"] = "rotation"
        status, _, body = self.request("POST", "/api/admin/setkeymap",
                                       self.payload(0, other))
        self.assertEqual(status, 409)
        self.assertEqual(body["rev"], 1)
        self.assertEqual(body["keys"], keys)

    def test_six_validation_errors_are_http_400_with_japanese_reason(self):
        cases = [
            (lambda p: p["keys"].update({"2": "no_such_mode"}), "unknown id"),
            (lambda p: p.update(extra=True), "extra field"),
            (lambda p: p["keys"].update({"2": 2}), "non string"),
            (lambda p: p["keys"].pop("9"), "missing key"),
            (lambda p: [p["keys"].__setitem__(k, "reserved") for k in p["keys"]], "no rotation"),
            (lambda p: p["keys"].update({"1": "sleep", "2": "sleep"}), "multiple sleep"),
        ]
        for mutate, name in cases:
            payload = self.payload(0); mutate(payload)
            with self.subTest(name=name):
                status, _, body = self.request("POST", "/api/admin/setkeymap", payload)
                self.assertEqual(status, 400)
                self.assertRegex(body["error"], "[ぁ-んァ-ン一-龥]")

    def test_invalid_base_revision_types_are_http_400(self):
        for value in (None, True, -1, "1"):
            with self.subTest(value=value):
                status, _, body = self.request("POST", "/api/admin/setkeymap",
                                               self.payload(value))
                self.assertEqual(status, 400)
                self.assertIn("base_revision", body["error"])

    def test_deleting_setting_restores_default_over_http(self):
        keys = dict(keymap.DEFAULT_KEYS); keys["2"] = "rotation"
        self.request("POST", "/api/admin/setkeymap", self.payload(0, keys))
        store.setting_set(handlers.KEYMAP_SETTING, None)
        status, _, body = self.request("GET", "/api/keymap")
        self.assertEqual(status, 200)
        self.assertEqual(body["rev"], 0)
        self.assertEqual(body["keys"], keymap.DEFAULT_KEYS)

    def test_parallel_same_revision_is_one_200_and_one_409_without_thread_exceptions(self):
        keys_a = dict(keymap.DEFAULT_KEYS); keys_a["2"] = "rotation"
        keys_b = dict(keymap.DEFAULT_KEYS); keys_b["3"] = "rotation"
        barrier = threading.Barrier(2)
        results, errors = [], []
        def post(keys):
            try:
                barrier.wait()
                results.append(self.request("POST", "/api/admin/setkeymap",
                                            self.payload(0, keys))[0])
            except Exception as exc:  # thread例外を結果から脱落させない
                errors.append(exc)
        threads = [threading.Thread(target=post, args=(keys,))
                   for keys in (keys_a, keys_b)]
        for thread in threads: thread.start()
        for thread in threads: thread.join(timeout=3)
        self.assertEqual(errors, [])
        self.assertEqual(sorted(results), [200, 409])


if __name__ == "__main__":
    unittest.main()
