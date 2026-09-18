import contextlib
import http.client
import importlib.util
import io
import json
import os
import shutil
import tempfile
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import agents, config, handlers, keymap, store

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

    def test_public_shape_exposes_only_the_closed_card_fields(self):
        self.drop(path="/private/path", host="example", pid=123, token="secret", prompt="hidden")
        body = agents.render_agents(self.root, self.now)
        self.assertEqual(set(body["agents"][0]),
                         {"id", "label", "state", "age_s", "task_kind", "metrics"})
        encoded = json.dumps(body, ensure_ascii=False)
        for value in ("/private/path", "example", "123", "secret", "hidden"):
            self.assertNotIn(value, encoded)

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
            with patch.object(exporter, "DEST", dest_dir / "hermes.json"):
                summary = exporter.read_source(now, source)
                self.assertTrue(exporter.publish(summary))
                drop = dest_dir / "hermes.json"
                self.assertTrue(drop.is_file())
                self.assertEqual(drop.stat().st_mode & 0o777, 0o640)
                before = drop.read_bytes()
                os.utime(source, (now - 100, now - 100))
                stale = exporter.read_source(now, source)
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
            with patch.object(exporter, "DEST", dest):
                summary = exporter.read_source(now, source)
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
            with patch.object(exporter, "DEST", dest):
                self.assertTrue(exporter.publish(exporter.read_source(now, source)))
            body = agents.render_agents(dest.parent, now)
            self.assertEqual(body["agents"][0]["state"], "unknown")
            self.assertNotEqual(body["agents"][0]["state"], "live")

    def test_missing_heartbeat_publishes_unknown_card(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "missing-heartbeat.json"
            dest = root / "drops" / "hermes.json"
            now = 1_700_000_000.0
            with patch.object(exporter, "DEST", dest):
                summary = exporter.read_source(now, source)
                self.assertEqual(summary["state"], "unknown")
                self.assertEqual(summary["published_at"], now)
                self.assertTrue(exporter.publish(summary))
            body = agents.render_agents(dest.parent, now)
            self.assertEqual(len(body["agents"]), 1)
            self.assertEqual(body["agents"][0]["state"], "unknown")

    def test_fixed_profile_with_missing_heartbeat_publishes_unknown_profile_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            (home / "profiles" / "example-profile").mkdir(parents=True)
            dest = Path(tmp) / "drops" / "hermes.json"
            env = {"HERMES_HOME": str(home), "HERMES_PROFILE": "example-profile"}
            with patch.dict(os.environ, env, clear=True), patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertTrue(dest.is_file())
            self.assertFalse((dest.parent / "hermes-example-profile.json").exists())
            body = json.loads(dest.read_text(encoding="utf-8"))
            self.assertEqual(body["id"], "hermes")
            self.assertEqual(body["state"], "unknown")

    def test_fixed_missing_heartbeat_file_publishes_unknown_hermes_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            missing = home / "missing-heartbeat"
            dest = Path(tmp) / "drops" / "hermes.json"
            env = {"HERMES_HOME": str(home), "HERMES_HEARTBEAT_FILE": str(missing)}
            with patch.dict(os.environ, env, clear=True), patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertTrue(dest.is_file())
            body = json.loads(dest.read_text(encoding="utf-8"))
            self.assertEqual(body["id"], "hermes")
            self.assertEqual(body["state"], "unknown")

    def test_fixed_invalid_profile_name_falls_back_to_hermes_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            source = home / "profiles" / "Bad_Name" / "state" / "gateway.heartbeat"
            source.parent.mkdir(parents=True)
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = time.time()
            os.utime(source, (now, now))
            dest = Path(tmp) / "drops" / "hermes.json"
            env = {"HERMES_HOME": str(home), "HERMES_PROFILE": "Bad_Name"}
            with patch.dict(os.environ, env, clear=True), patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertTrue(dest.is_file())
            body = json.loads(dest.read_text(encoding="utf-8"))
            self.assertEqual(body["id"], "hermes")
            self.assertEqual(body["state"], "live")

    def test_named_profile_publishes_a_profile_drop_with_matching_id(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            source = home / "profiles" / "example-profile" / "state" / "gateway.heartbeat"
            source.parent.mkdir(parents=True)
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = time.time()
            os.utime(source, (now, now))
            dest = Path(tmp) / "drops" / "hermes.json"
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                self.assertTrue(exporter.tick() is None)
            profile_drop = dest.parent / "hermes-example-profile.json"
            self.assertTrue(profile_drop.is_file())
            self.assertEqual(json.loads(profile_drop.read_text(encoding="utf-8"))["id"],
                             "hermes-example-profile")
            self.assertFalse(dest.exists())

    def test_default_profile_publishes_the_hermes_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            source = home / "state" / "gateway.heartbeat"
            source.parent.mkdir(parents=True)
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = time.time()
            os.utime(source, (now, now))
            dest = Path(tmp) / "drops" / "hermes.json"
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertTrue(dest.is_file())
            self.assertEqual(json.loads(dest.read_text(encoding="utf-8"))["id"], "hermes")

    def test_invalid_profile_names_do_not_publish_drops(self):
        for name in ("Bad", "bad_name", "..", "a" * 33):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                home = Path(tmp) / "home"
                source = home / "profiles" / name / "state" / "gateway.heartbeat"
                source.parent.mkdir(parents=True)
                source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
                now = time.time()
                os.utime(source, (now, now))
                dest = Path(tmp) / "drops" / "hermes.json"
                with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                        patch.object(exporter, "DEST", dest), \
                        patch.object(exporter, "candidate_sources", return_value=(source,)):
                    exporter.tick()
                self.assertFalse(dest.parent.exists() and any(dest.parent.iterdir()))

    def test_missing_candidates_do_not_publish_ghost_drops(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            (home / "profiles" / "example-profile").mkdir(parents=True)
            dest = Path(tmp) / "drops" / "hermes.json"
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertFalse(dest.parent.exists() and any(dest.parent.iterdir()))

    def test_discovery_removes_drop_for_profile_deleted_between_cycles(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            self._write_discovery_heartbeat(home / "state" / "gateway.heartbeat", 100)
            first = home / "profiles" / "example-profile" / "state" / "gateway.heartbeat"
            second = home / "profiles" / "another-profile" / "state" / "gateway.heartbeat"
            self._write_discovery_heartbeat(first, 200)
            self._write_discovery_heartbeat(second, 300)
            drop_dir = Path(tmp) / "drops"
            dest = drop_dir / "hermes.json"
            drop_dir.mkdir()
            miach = drop_dir / "miach.json"
            miach.write_text("keep", encoding="utf-8")
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                exporter.tick()
                shutil.rmtree(first.parents[1])
                exporter.tick()
            self.assertTrue((drop_dir / "hermes.json").is_file())
            self.assertFalse((drop_dir / "hermes-example-profile.json").exists())
            self.assertTrue((drop_dir / "hermes-another-profile.json").is_file())
            self.assertEqual(miach.read_text(encoding="utf-8"), "keep")

    def test_discovery_removes_stale_default_drop_when_default_heartbeat_disappears(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            default = home / "state" / "gateway.heartbeat"
            self._write_discovery_heartbeat(default, 100)
            dest = Path(tmp) / "drops" / "hermes.json"
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                exporter.tick()
                default.unlink()
                exporter.tick()
            self.assertFalse(dest.exists())

    def test_discovery_cleanup_does_not_remove_other_drop_ids(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            self._write_discovery_heartbeat(home / "state" / "gateway.heartbeat", 100)
            drop_dir = Path(tmp) / "drops"
            dest = drop_dir / "hermes.json"
            miach = drop_dir / "miach.json"
            hermesx = drop_dir / "hermesx.json"
            drop_dir.mkdir()
            miach.write_text("keep-miach", encoding="utf-8")
            hermesx.write_text("keep-hermesx", encoding="utf-8")
            with patch.dict(os.environ, {"HERMES_HOME": str(home)}, clear=True), \
                    patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertEqual(miach.read_text(encoding="utf-8"), "keep-miach")
            self.assertEqual(hermesx.read_text(encoding="utf-8"), "keep-hermesx")

    @staticmethod
    def _write_discovery_heartbeat(path, mtime):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
        os.utime(path, (mtime, mtime))

    def test_fixed_profile_publishes_only_its_drop_and_removes_other_hermes_drops(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            source = home / "profiles" / "example-profile" / "state" / "gateway.heartbeat"
            source.parent.mkdir(parents=True)
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            now = time.time()
            os.utime(source, (now, now))
            drop_dir = Path(tmp) / "drops"
            drop_dir.mkdir()
            for name in ("hermes.json", "hermes-other.json", "hermes-invalid.json"):
                (drop_dir / name).write_text("old", encoding="utf-8")
            miach = drop_dir / "miach.json"
            miach.write_text("keep", encoding="utf-8")
            dest = drop_dir / "hermes.json"
            env = {"HERMES_HOME": str(home), "HERMES_PROFILE": "example-profile"}
            with patch.dict(os.environ, env, clear=True), patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertTrue(dest.is_file())
            self.assertEqual(json.loads(dest.read_text(encoding="utf-8"))["id"], "hermes")
            self.assertFalse((drop_dir / "hermes-other.json").exists())
            self.assertFalse((drop_dir / "hermes-invalid.json").exists())
            self.assertEqual(miach.read_text(encoding="utf-8"), "keep")

    def test_old_heartbeat_writes_an_unknown_drop(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            source = home / "profiles" / "example-profile" / "state" / "gateway.heartbeat"
            source.parent.mkdir(parents=True)
            source.write_text(json.dumps({"state": "live", "task_kind": "coding"}), encoding="utf-8")
            old = time.time() - exporter.HEARTBEAT_MAX_AGE - 1
            os.utime(source, (old, old))
            dest = Path(tmp) / "drops" / "hermes.json"
            env = {"HERMES_HOME": str(home), "HERMES_PROFILE": "example-profile"}
            with patch.dict(os.environ, env, clear=True), patch.object(exporter, "DEST", dest):
                exporter.tick()
            self.assertEqual(json.loads(dest.read_text(encoding="utf-8"))["state"], "unknown")


class ExporterSourceDiscoveryTest(unittest.TestCase):
    """既定の心拍源は、unitのdrop-inを足さなくても稼働中プロファイルに追随する。"""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.default_heartbeat = self.home / "state" / "gateway.heartbeat"
        self.profile_heartbeat = self.home / "profiles" / "example-profile" / "state" / "gateway.heartbeat"

    def _write(self, path, mtime, **payload):
        path.parent.mkdir(parents=True, exist_ok=True)
        item = {"state": "live", "task_kind": "coding"}
        item.update(payload)
        path.write_text(json.dumps(item), encoding="utf-8")
        os.utime(path, (mtime, mtime))
        return path

    def _env(self, **values):
        return patch.dict(os.environ, values, clear=True)

    def test_env_override_pins_a_single_heartbeat(self):
        pinned = self._write(self.home / "pinned.json", 100)
        self._write(self.profile_heartbeat, 200)
        with self._env(HERMES_HOME=str(self.home), HERMES_HEARTBEAT_FILE=str(pinned)):
            self.assertEqual(exporter.select_source(), pinned)
            self.assertEqual(exporter.read_source(105.0)["published_at"], 100)

    def test_profile_pin_wins_over_a_newer_heartbeat(self):
        pinned = self._write(self.home / "profiles" / "other" / "state" / "gateway.heartbeat", 100)
        self._write(self.profile_heartbeat, 200)  # より新しいが別プロファイル
        with self._env(HERMES_HOME=str(self.home), HERMES_PROFILE="other"):
            self.assertEqual(exporter.select_source(), pinned)

    def test_unknown_profile_name_falls_back_to_discovery(self):
        self._write(self.profile_heartbeat, 200)
        with self._env(HERMES_HOME=str(self.home), HERMES_PROFILE="no-such-profile"):
            self.assertEqual(exporter.select_source(), self.profile_heartbeat)

    def test_hermes_home_defaults_to_the_user_home(self):
        with patch.dict(os.environ, {"HOME": str(self.home)}, clear=True):
            self.assertEqual(exporter.hermes_home(), self.home / ".hermes")
            self.assertEqual(exporter.candidate_sources()[0],
                             self.home / ".hermes" / "state" / "gateway.heartbeat")

    def test_discovery_picks_the_newest_heartbeat_across_profiles(self):
        self._write(self.default_heartbeat, 100)  # 既定プロファイル（停止している）
        self._write(self.home / "profiles" / "other" / "state" / "gateway.heartbeat", 50)
        self._write(self.profile_heartbeat, 200)  # 稼働中プロファイル（最新）
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(exporter.select_source(), self.profile_heartbeat)
            summary = exporter.read_source(205.0)
            self.assertEqual(summary["published_at"], 200)
            self.assertEqual(summary["state"], "live")
            self.assertEqual(summary["hb_age_s"], 5)

    def test_discovery_falls_back_to_the_default_path_and_publishes_unknown(self):
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(exporter.select_source(), self.default_heartbeat)
            dest = self.home / "drops" / "hermes.json"
            now = 1_700_000_000.0
            with patch.object(exporter, "DEST", dest):
                summary = exporter.read_source(now)
                self.assertEqual(summary["state"], "unknown")
                self.assertEqual(summary["published_at"], now)
                self.assertTrue(exporter.publish(summary))
            body = agents.render_agents(dest.parent, now)
            self.assertEqual([item["id"] for item in body["agents"]], ["hermes"])
            self.assertEqual(body["agents"][0]["state"], "unknown")

    def test_discovery_skips_non_regular_candidates(self):
        broken = self.home / "profiles" / "broken" / "state" / "gateway.heartbeat"
        broken.mkdir(parents=True)
        os.utime(broken, (300, 300))  # 最新でもディレクトリは採用しない
        self._write(self.profile_heartbeat, 200)
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(exporter.select_source(), self.profile_heartbeat)

    def test_two_fresh_heartbeats_race_and_the_newest_wins(self):
        # 複数プロファイルが同時に稼働している状態。既定は最新を選ぶ（入れ替わりうる）ことを固定する。
        self._write(self.home / "profiles" / "other" / "state" / "gateway.heartbeat", 990)
        self._write(self.profile_heartbeat, 995)
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(len(exporter.fresh_sources(now=1000.0)), 2)
            self.assertEqual(exporter.select_source(), self.profile_heartbeat)
            self.assertEqual(exporter.read_source(1000.0)["state"], "live")
            # 相手が先に書けば選択は入れ替わる（既定では曖昧さを消せない）
            self._write(self.home / "profiles" / "other" / "state" / "gateway.heartbeat", 999)
            self.assertEqual(exporter.select_source(),
                             self.home / "profiles" / "other" / "state" / "gateway.heartbeat")

    def test_equal_mtimes_keep_the_default_profile_first(self):
        self._write(self.default_heartbeat, 500)
        self._write(self.profile_heartbeat, 500)
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(exporter.select_source(), self.default_heartbeat)

    def test_ambiguity_notice_only_with_several_fresh_heartbeats(self):
        self.assertEqual(exporter.ambiguity_notice(self.profile_heartbeat, ()), None)
        self.assertEqual(exporter.ambiguity_notice(self.profile_heartbeat, (self.profile_heartbeat,)), None)
        notice = exporter.ambiguity_notice(self.profile_heartbeat, (self.default_heartbeat, self.profile_heartbeat))
        self.assertIn("2 fresh heartbeats", notice)
        self.assertIn(str(self.profile_heartbeat), notice)
        self.assertIn("HERMES_PROFILE", notice)

    def test_ambiguity_notice_dedupes_by_the_fresh_set_not_the_winner(self):
        fresh = (self.default_heartbeat, self.profile_heartbeat)
        self.assertIsNotNone(exporter.ambiguity_notice(self.default_heartbeat, fresh))
        # 勝者（＝reporting のパス）が入れ替わっても、fresh集合が同じなら再出力しない
        self.assertIsNone(exporter.ambiguity_notice(self.profile_heartbeat, fresh, fresh))
        # fresh集合が変われば再出力する
        changed = (self.default_heartbeat, self.profile_heartbeat, self.home / "profiles" / "third" / "state" / "gateway.heartbeat")
        self.assertIsNotNone(exporter.ambiguity_notice(self.profile_heartbeat, changed, fresh))

    def test_tick_logs_the_ambiguity_once_while_the_winner_keeps_flipping(self):
        first = self._write(self.home / "profiles" / "aaa" / "state" / "gateway.heartbeat", 100)
        second = self._write(self.home / "profiles" / "bbb" / "state" / "gateway.heartbeat", 200)
        now = time.time()
        os.utime(first, (now - 2, now - 2))
        os.utime(second, (now - 1, now - 1))
        stderr = io.StringIO()
        with self._env(HERMES_HOME=str(self.home)), tempfile.TemporaryDirectory() as tmp, \
                patch.object(exporter, "DEST", Path(tmp) / "hermes.json"), \
                contextlib.redirect_stderr(stderr):
            state = exporter.tick()
            os.utime(first, (now, now))          # 勝者が first へ入れ替わる
            state = exporter.tick(state)
            os.utime(second, (now + 1, now + 1))  # 勝者が second へ戻る
            state = exporter.tick(state)
        self.assertEqual(stderr.getvalue().count("fresh heartbeats"), 1)

    def test_a_fifo_heartbeat_never_blocks_the_loop(self):
        fifo = self.home / "profiles" / "fifo-profile" / "state" / "gateway.heartbeat"
        fifo.parent.mkdir(parents=True)
        try:
            os.mkfifo(fifo)
        except (OSError, NotImplementedError):
            self.skipTest("FIFO unavailable")
        os.utime(fifo, (600, 600))  # 候補の中で最新でもある
        self._write(self.profile_heartbeat, 200)
        with self._env(HERMES_HOME=str(self.home)):
            self.assertEqual(exporter.select_source(), self.profile_heartbeat)
            self.assertEqual(exporter.read_source(700.0, fifo)["state"], "unknown")
            self.assertEqual(exporter.read_source(700.0, fifo)["published_at"], 700.0)
        with self._env(HERMES_HOME=str(self.home), HERMES_PROFILE="fifo-profile"):
            self.assertEqual(exporter.select_source(), fifo)
            self.assertEqual(exporter.read_source(700.0)["state"], "unknown")


class ConfiguredAgentCardsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        agents._last_good.clear()
        self.now = 1_700_000_000.0

    def tearDown(self):
        self.tmp.cleanup()

    def _drop(self, agent_id, published_at, state="busy"):
        (self.root / f"{agent_id}.json").write_text(json.dumps({
            "id": agent_id, "published_at": published_at, "state": state,
            "task_kind": "coding", "metrics": [],
        }), encoding="utf-8")

    def _with_labels(self, value):
        return patch.dict(os.environ, {"GEOFRAME_AGENT_LABELS": value}, clear=False)

    def test_labels_follow_env_order_and_unconfigured_drops_are_ignored(self):
        try:
            with self._with_labels("alpha=Alpha label, beta=Beta label"):
                importlib.reload(config)
                importlib.reload(agents)
                self._drop("beta", self.now)
                self._drop("alpha", self.now)
                self._drop("hermes", self.now)
                body = agents.render_agents(self.root, self.now)
                self.assertEqual([item["id"] for item in body["agents"]], ["alpha", "beta"])
                self.assertEqual([item["label"] for item in body["agents"]],
                                 ["Alpha label", "Beta label"])
        finally:
            importlib.reload(config)
            importlib.reload(agents)

    def test_two_live_cards_win_over_a_newer_stale_card_and_are_sorted(self):
        try:
            with self._with_labels("alpha=Alpha, beta=Beta, gamma=Gamma"):
                importlib.reload(config)
                importlib.reload(agents)
                self._drop("alpha", self.now - 20)
                self._drop("beta", self.now - 10)
                self._drop("gamma", self.now - 40)
                body = agents.render_agents(self.root, self.now)
                self.assertEqual([item["id"] for item in body["agents"]], ["beta", "alpha"])
                self.assertTrue(body["rotation_allowed"])
        finally:
            importlib.reload(config)
            importlib.reload(agents)

    def test_all_stale_or_unknown_cards_remain_but_rotation_is_disabled(self):
        try:
            with self._with_labels("alpha=Alpha, beta=Beta"):
                importlib.reload(config)
                importlib.reload(agents)
                self._drop("alpha", self.now - 121)
                self._drop("beta", self.now, state="unknown")
                body = agents.render_agents(self.root, self.now)
                self.assertEqual(len(body["agents"]), 2)
                self.assertFalse(body["rotation_allowed"])
        finally:
            importlib.reload(config)
            importlib.reload(agents)

    def test_unconfigured_profile_drop_warns_only_once(self):
        try:
            with self._with_labels("hermes=Hermes"):
                importlib.reload(config)
                importlib.reload(agents)
                self._drop("hermes-example-profile", self.now)
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    first = agents.render_agents(self.root, self.now)
                    second = agents.render_agents(self.root, self.now)
                self.assertEqual(first, {"agents": [], "rotation_allowed": False})
                self.assertEqual(second, first)
                self.assertEqual(stderr.getvalue().count("hermes-example-profile"), 1)
                self.assertIn("add it to GEOFRAME_AGENT_LABELS", stderr.getvalue())
        finally:
            importlib.reload(config)
            importlib.reload(agents)

    def test_configured_profile_drop_does_not_warn(self):
        try:
            with self._with_labels("hermes=Hermes, hermes-example-profile=Example"):
                importlib.reload(config)
                importlib.reload(agents)
                self._drop("hermes-example-profile", self.now)
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    body = agents.render_agents(self.root, self.now)
                self.assertEqual(stderr.getvalue(), "")
                self.assertEqual([item["id"] for item in body["agents"]],
                                 ["hermes-example-profile"])
        finally:
            importlib.reload(config)
            importlib.reload(agents)


if __name__ == "__main__":
    unittest.main()
