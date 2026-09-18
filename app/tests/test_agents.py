import contextlib
import http.client
import importlib.util
import io
import json
import os
import tempfile
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import agents, handlers, keymap, store

EXPORTER_PATH = Path(__file__).parents[2] / "deploy" / "agents" / "hermes-status-exporter.py"
_spec = importlib.util.spec_from_file_location("hermes_status_exporter", EXPORTER_PATH)
assert _spec is not None and _spec.loader is not None
exporter = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(exporter)


class AgentReaderTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        agents._last_good.clear()
        self.now = 1_700_000_000.0

    def tearDown(self):
        self.tmp.cleanup()

    def drop(self, age=0, **changes):
        item = {"id": "hermes", "published_at": self.now - age,
                "state": "busy", "task_kind": "coding", "metrics": []}
        item.update(changes)
        (self.root / "hermes.json").write_text(json.dumps(item), encoding="utf-8")

    def test_staleness_boundaries_and_future_clock_skew(self):
        for age, expected in ((0, "live"), (29, "live"), (30, "live"),
                              (31, "stale"), (120, "stale"), (121, "unknown")):
            with self.subTest(age=age):
                self.drop(age)
                self.assertEqual(agents.render_agents(self.root, self.now)["agents"][0]["state"], expected)
        self.drop(-121)
        self.assertEqual(agents.render_agents(self.root, self.now)["agents"][0]["state"], "stale")

    def test_corrupt_type_extra_missing_oversize_symlink_and_fifo_keep_last_good(self):
        self.drop()
        good = agents.render_agents(self.root, self.now)
        cases = [b"{", json.dumps({"id": "hermes"}).encode(),
                 json.dumps({"id": "hermes", "published_at": "now", "state": "busy", "task_kind": "coding", "metrics": []}).encode(),
                 json.dumps({"id": "hermes", "published_at": self.now, "state": "busy", "task_kind": "coding", "metrics": [], "secret": "x"}).encode(),
                 b"x" * (64 * 1024 + 1)]
        for raw in cases:
            (self.root / "hermes.json").write_bytes(raw)
            self.assertEqual(agents.render_agents(self.root, self.now), good)
        outside = self.root / "outside"
        outside.write_text(json.dumps({"id": "hermes", "published_at": self.now + 1000, "state": "live", "task_kind": "coding", "metrics": []}))
        (self.root / "hermes.json").unlink(); (self.root / "hermes.json").symlink_to(outside)
        self.assertEqual(agents.render_agents(self.root, self.now), good)
        (self.root / "hermes.json").unlink()
        try:
            os.mkfifo(self.root / "hermes.json")
        except (OSError, NotImplementedError):
            self.skipTest("FIFO unavailable")
        self.assertEqual(agents.render_agents(self.root, self.now), good)

    def test_partial_and_all_stale_rotation(self):
        self.drop(0)
        self.assertTrue(agents.render_agents(self.root, self.now)["rotation_allowed"])
        self.drop(121)
        self.assertFalse(agents.render_agents(self.root, self.now)["rotation_allowed"])
        self.assertEqual(len(agents.render_agents(self.root, self.now)["agents"]), 1)

    def test_public_shape_has_no_sensitive_values_and_unknowns_are_closed(self):
        self.drop(state="secret", task_kind="prompt", metrics=[])
        body = agents.render_agents(self.root, self.now)
        # Invalid source values are rejected; no raw path, host, pid, token or prompt can pass.
        self.assertEqual(body["agents"], [])

    def test_filename_and_payload_ids_must_match(self):
        self.drop()
        (self.root / "miach.json").write_text(json.dumps({
            "id": "hermes", "published_at": self.now, "state": "busy",
            "task_kind": "coding", "metrics": []}), encoding="utf-8")
        body = agents.render_agents(self.root, self.now)
        self.assertEqual([item["id"] for item in body["agents"]], ["hermes"])

    def test_api_shape_and_cache_control_over_http(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handlers.Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            safe = {"agents": [{"id": "hermes", "label": "みなも (Hermes)",
                                "state": "live", "age_s": 0, "task_kind": "idle", "metrics": []}],
                    "rotation_allowed": True}
            with patch.object(handlers, "render_agents", return_value=safe):
                conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=3)
                conn.request("GET", "/api/agents", headers={"Host": "127.0.0.1"})
                response = conn.getresponse()
                raw = response.read()
                headers = dict(response.getheaders())
                conn.close()
            self.assertEqual(response.status, 200)
            self.assertEqual(headers["Cache-Control"], "no-store")
            body = json.loads(raw)
            self.assertEqual(set(body), {"agents", "rotation_allowed"})
            self.assertEqual(set(body["agents"][0]), {"id", "label", "state", "age_s", "task_kind", "metrics"})
            self.assertNotIn("/home/", raw.decode())
            self.assertNotIn("token", raw.decode().lower())
            self.assertNotIn("pid", raw.decode().lower())
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_keymap_agents_default(self):
        self.assertEqual(keymap.DEFAULT_KEYS["2"], "agents")
        self.assertIn("agents", {mode["id"] for mode in keymap.MODES})


class ExporterTest(unittest.TestCase):
    def test_publish_creates_group_readable_drop_and_preserves_stale_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "heartbeat.json"
            dest_dir = root / "drops"
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = time.time()
            os.utime(source, (now, now))
            with patch.object(exporter, "SOURCE", source), patch.object(exporter, "DEST", dest_dir / "hermes.json"):
                summary = exporter.read_source(now)
                self.assertTrue(exporter.publish(summary))
                drop = dest_dir / "hermes.json"
                self.assertTrue(drop.is_file())
                self.assertEqual(drop.stat().st_mode & 0o777, 0o640)
                before = drop.read_bytes()
                os.utime(source, (now - 100, now - 100))
                stale = exporter.read_source(now)
                self.assertIsNotNone(stale)
                self.assertEqual(stale["published_at"], now - 100)
                self.assertTrue(exporter.publish(stale))
                self.assertNotEqual(drop.read_bytes(), before)

    def test_old_heartbeat_publishes_drop_with_heartbeat_mtime(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "heartbeat.json"
            dest = root / "drops" / "hermes.json"
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = 1_700_000_000.0
            heartbeat_mtime = now - 13 * 24 * 60 * 60
            os.utime(source, (heartbeat_mtime, heartbeat_mtime))
            with patch.object(exporter, "SOURCE", source), patch.object(exporter, "DEST", dest):
                summary = exporter.read_source(now)
                self.assertEqual(summary["published_at"], heartbeat_mtime)
                self.assertTrue(exporter.publish(summary))
                self.assertEqual(json.loads(dest.read_text(encoding="utf-8"))["published_at"], heartbeat_mtime)

    def test_old_heartbeat_drop_renders_unknown_not_live(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "heartbeat.json"
            dest = root / "drops" / "hermes.json"
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = 1_700_000_000.0
            heartbeat_mtime = now - 13 * 24 * 60 * 60
            os.utime(source, (heartbeat_mtime, heartbeat_mtime))
            with patch.object(exporter, "SOURCE", source), patch.object(exporter, "DEST", dest):
                self.assertTrue(exporter.publish(exporter.read_source(now)))
            body = agents.render_agents(dest.parent, now)
            self.assertEqual(body["agents"][0]["state"], "unknown")
            self.assertNotEqual(body["agents"][0]["state"], "live")

    def test_missing_heartbeat_publishes_unknown_card(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "missing-heartbeat.json"
            dest = root / "drops" / "hermes.json"
            now = 1_700_000_000.0
            with patch.object(exporter, "SOURCE", source), patch.object(exporter, "DEST", dest):
                summary = exporter.read_source(now)
                self.assertEqual(summary["state"], "unknown")
                self.assertEqual(summary["published_at"], now)
                self.assertTrue(exporter.publish(summary))
            body = agents.render_agents(dest.parent, now)
            self.assertEqual(len(body["agents"]), 1)
            self.assertEqual(body["agents"][0]["state"], "unknown")


if __name__ == "__main__":
    unittest.main()
