"""deploy/loc_decision_morning.py（毎朝の Decision 起動）のテスト"""
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "deploy" / "loc_decision_morning.py"
spec = importlib.util.spec_from_file_location("loc_decision_morning", SCRIPT)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

FREE = {"generated_at": "2026-10-16T05:00:00+09:00",
        "gpu": {"vram_used_mib": 1, "util_pct": 0, "processes": []},
        "lease": {"count": 0}}
NOW = 1792094400.0 + 5    # 2026-10-16T05:00:05+09:00


def with_gpu(**kw):
    return {**FREE, "gpu": {**FREE["gpu"], **kw}}


class GpuBusyTest(unittest.TestCase):
    def test_free(self):
        self.assertEqual(m.gpu_busy_reasons(FREE, [], now=NOW), [])

    def test_unknown_state_is_busy(self):
        self.assertTrue(m.gpu_busy_reasons(None, []))
        self.assertTrue(m.gpu_busy_reasons(FREE, None, now=NOW))
        self.assertTrue(m.gpu_busy_reasons({**FREE, "gpu": None}, [], now=NOW))

    def test_stale_status_is_busy(self):
        self.assertTrue(m.gpu_busy_reasons(FREE, [], now=NOW + 60))

    def test_lease_util_ollama_and_other_process(self):
        self.assertTrue(m.gpu_busy_reasons({**FREE, "lease": {"count": 1}}, [], now=NOW))
        self.assertTrue(m.gpu_busy_reasons(with_gpu(util_pct=50), [], now=NOW))
        self.assertTrue(m.gpu_busy_reasons(FREE, [{"name": "gemma"}], now=NOW))
        busy = with_gpu(processes=[{"pid": 9, "name": "python", "vram_mib": 4000}])
        self.assertTrue(m.gpu_busy_reasons(busy, [], now=NOW))

    def test_unknown_state_is_ignored_while_running(self):
        self.assertEqual(m.gpu_busy_reasons(None, None, own_pids={7}, strict=False), [])
        self.assertTrue(m.gpu_busy_reasons({**FREE, "lease": {"count": 1}}, None,
                                           own_pids={7}, strict=False))

    def test_own_decision_is_not_counted(self):
        own = with_gpu(util_pct=99, processes=[{"pid": 7, "name": "python", "vram_mib": 2344}])
        self.assertEqual(m.gpu_busy_reasons(own, [], own_pids={7}), [])
        other = with_gpu(processes=[{"pid": 7, "vram_mib": 2344}, {"pid": 9, "vram_mib": 3000}])
        self.assertTrue(m.gpu_busy_reasons(other, [], own_pids={7}))


LEASED = {**FREE, "lease": {"count": 1, "paused": ["decision"]}}


class FakeClock:
    def __init__(self):
        self.t = 0.0

    def sleep(self, s):
        self.t += s

    def __call__(self):
        return self.t


class RunTest(unittest.TestCase):
    """systemctl・STATUS・GPU の状態を偽物にして、起動と停止の流れを確かめる"""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.calls = []
        self.pending = [3, 2, 0]
        self.active = False
        self.gpu = [(FREE, [])]
        self.ready_after = 0
        self.saved = {k: getattr(m, k) for k in (
            "read_clue_state", "decision_active", "read_gpu_state", "systemctl",
            "decision_ready", "decision_pid", "gpu_busy_reasons", "STATE_DIR", "OWNED_FILE")}
        m.STATE_DIR = Path(self.tmp.name)
        m.OWNED_FILE = m.STATE_DIR / "decision-owned"
        m.read_clue_state = lambda: (lambda n: m.ClueState(n, n, ""))(
            self.pending.pop(0) if len(self.pending) > 1 else self.pending[0])
        m.decision_active = lambda: self.active
        m.read_gpu_state = lambda: self.gpu.pop(0) if len(self.gpu) > 1 else self.gpu[0]
        m.decision_pid = lambda: 7
        m.decision_ready = self._ready
        m.systemctl = self._systemctl
        real = self.saved["gpu_busy_reasons"]
        m.gpu_busy_reasons = lambda s, o, now=None, own_pids=(), strict=True: real(
            s, o, None, own_pids, strict)

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(m, k, v)
        self.tmp.cleanup()

    def _ready(self):
        self.ready_after -= 1
        return self.ready_after < 0

    def _systemctl(self, *args):
        self.calls.append(args[0])
        self.active = {"start": True, "stop": False}.get(args[0], self.active)
        return subprocess.CompletedProcess(args, 0, "", "")

    def run_script(self):
        clock = FakeClock()
        return m.run(sleep=clock.sleep, clock=clock)

    def use_status(self, *locs):
        """read_clue_state を本物に戻し、STATUS（get_json）だけを偽物にする。最後の値は繰り返す"""
        statuses = [{"db": {"loc": loc}} for loc in locs]
        self.reads = 0
        m.read_clue_state = self.saved["read_clue_state"]
        real = m.get_json
        self.addCleanup(setattr, m, "get_json", real)

        def fake(url, *a, **k):
            self.reads += 1
            return statuses.pop(0) if len(statuses) > 1 else statuses[0]
        m.get_json = fake

    def test_total_pending_but_nothing_runnable_does_not_start(self):
        # Provider の停止中など。総待ちは残っても、起動しても進まない
        self.use_status({"clue_pending": 40, "clue_runnable": 0, "clue_pause": "unauthorized"})
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])
        self.assertFalse(m.owned())

    def test_old_status_without_runnable_uses_pending(self):
        self.use_status({"clue_pending": 3}, {"clue_pending": 0})
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_transient_zero_runnable_keeps_waiting_and_resumes(self):
        # 最後の 1 件が 429 などで後回し（runnable 0）→ 期限が来て再開 → 判定できて総待ち 0
        self.use_status({"clue_pending": 3, "clue_runnable": 3},
                        {"clue_pending": 2, "clue_runnable": 2},
                        {"clue_pending": 1, "clue_runnable": 0},
                        {"clue_pending": 1, "clue_runnable": 0},
                        {"clue_pending": 1, "clue_runnable": 1},
                        {"clue_pending": 0, "clue_runnable": 0})
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertEqual(self.reads, 6, "一時的な 0 で止めずに最後の 0 まで見る")

    def test_zero_total_pending_stops_after_start(self):
        self.use_status({"clue_pending": 9, "clue_runnable": 4},
                        {"clue_pending": 0, "clue_runnable": 0})
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertEqual(self.reads, 2)

    def test_fixed_pause_stops_immediately_after_start(self):
        self.use_status({"clue_pending": 9, "clue_runnable": 4},
                        {"clue_pending": 9, "clue_runnable": 0, "clue_pause": "unauthorized"},
                        {"clue_pending": 9, "clue_runnable": 4})
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertEqual(self.reads, 2, "停止が出たらそれ以上待たない")
        self.assertFalse(m.owned())

    def test_transient_zero_runnable_past_the_stall_limit_stops(self):
        self.use_status({"clue_pending": 9, "clue_runnable": 4},
                        {"clue_pending": 9, "clue_runnable": 0})
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertEqual(self.reads, 2 + m.STALL_S // m.POLL_S, "猶予（停滞の期限）の分は待つ")

    def test_total_decrease_extends_the_grace_while_runnable_is_zero(self):
        # 総待ちが減るたびに猶予を数え直す。runnable が 0 の間に 1 枚ずつ片付いても止めない
        locs = [{"clue_pending": 40 - i, "clue_runnable": 0 if i else 4}
                for i in range(2 * m.STALL_S // m.POLL_S)]
        locs.append({"clue_pending": 0, "clue_runnable": 0})
        self.use_status(*locs)
        self.assertEqual(self.run_script(), 0)

    def test_runnable_not_decreasing_is_a_stall(self):
        self.use_status({"clue_pending": 9, "clue_runnable": 4})
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_run_limit_still_applies_while_waiting_on_deferrals(self):
        # 総待ちが少しずつ減り続けても、上限の時間を過ぎたら止める（続きは翌朝）
        n = m.RUN_LIMIT_S // m.POLL_S + 2
        self.use_status(*[{"clue_pending": n + 5 - i, "clue_runnable": 0} for i in range(n)])
        self.assertEqual(self.run_script(), 0)
        self.assertLess(self.reads, n)

    def test_status_failure_after_start_keeps_the_last_state(self):
        self.pending = [3]
        calls = {"n": 0}

        def flaky():
            calls["n"] += 1
            if calls["n"] == 1:
                return real_state
            raise OSError("down")
        real_state = m.ClueState(3, 3, "")
        m.read_clue_state = flaky
        self.assertEqual(self.run_script(), 1, "取れない間は進んでいないものとして停滞で止める")
        self.assertEqual(self.calls, ["start", "stop"])

    def test_nothing_pending(self):
        self.pending = [0]
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])

    def test_already_started_by_hand_is_left_alone(self):
        self.active = True
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])

    def test_gpu_busy_skips(self):
        self.gpu = [({**FREE, "lease": {"count": 1}}, [])]
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])
        self.assertFalse(m.owned())

    def test_runs_until_pending_is_zero_then_stops(self):
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertFalse(self.active)
        self.assertFalse(m.owned())

    def test_not_ready_in_time_stops(self):
        self.ready_after = 10 ** 6
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_stall_stops(self):
        self.pending = [5]
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_yields_to_another_process(self):
        self.pending = [5, 4, 3, 2, 1, 0]
        other = {**FREE, "gpu": {**FREE["gpu"], "processes": [{"pid": 9, "vram_mib": 8000}]}}
        self.gpu = [(FREE, []), (other, [])]
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])
        self.assertFalse(m.owned())

    def test_lease_pause_waits_for_restore_then_stops(self):
        self.pending = [5, 4]
        # 起動前は空き → 判定中に lease（llm-manager が Decision を止める）→ lease が終わり、
        # llm-manager が Decision を起動し直す
        states = [(FREE, []), (LEASED, []), (LEASED, []), (FREE, [])]

        def manager():
            state = states.pop(0) if len(states) > 1 else states[0]
            if state[0] is LEASED:
                self.active = False
            elif not states[1:] and self.calls == ["start"]:
                self.active = True        # 戻された
            return state
        m.read_gpu_state = manager
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"], "戻された Decision を止める")
        self.assertFalse(self.active)
        self.assertFalse(m.owned())

    def test_restored_by_manager_after_timeout_is_adopted_next_morning(self):
        # 前回の印が残っていて、llm-manager が戻した Decision が動いている
        m.set_owned(True)
        self.active = True
        self.pending = [0]
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["stop"])
        self.assertFalse(m.owned())

    def test_restored_and_pending_is_continued(self):
        m.set_owned(True)
        self.active = True
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["stop"])

    def test_mark_is_kept_while_manager_still_holds_it(self):
        self.pending = [5, 4, 3]
        self.gpu = [(FREE, [])] + [(LEASED, [])] * 500
        self.assertEqual(self.run_script(), 0)
        self.assertTrue(m.owned(), "lease が続いている間は、翌朝止めるために印を残す")


class ClueStateTest(unittest.TestCase):
    """STATUS の読み方: clue_runnable があれば work に、無ければ clue_pending を使う"""

    @staticmethod
    def status(**loc):
        return {"db": {"loc": loc}}

    def test_runnable_is_work_and_pending_is_total(self):
        self.assertEqual(m.clue_state(self.status(clue_pending=5, clue_runnable=2)),
                         m.ClueState(2, 5, ""))
        self.assertEqual(m.clue_state(self.status(clue_pending=5, clue_runnable=0)),
                         m.ClueState(0, 5, ""))

    def test_falls_back_to_pending_without_runnable(self):
        self.assertEqual(m.clue_state(self.status(clue_pending=4)), m.ClueState(4, 4, ""))
        self.assertEqual(m.clue_state(self.status(clue_pending=0)), m.ClueState(0, 0, ""))

    def test_pause_code_is_read(self):
        state = m.clue_state(self.status(clue_pending=5, clue_runnable=0, clue_pause="forbidden"))
        self.assertEqual(state, m.ClueState(0, 5, "forbidden"))

    def test_missing_keys_are_an_error(self):
        with self.assertRaises(KeyError):
            m.clue_state(self.status(clue_runnable=1))

    def test_read_clue_state_reads_status_url(self):
        saved = m.get_json
        try:
            m.get_json = lambda url, *a, **k: self.status(clue_pending=7, clue_runnable=3)
            self.assertEqual(m.read_clue_state(), m.ClueState(3, 7, ""))
            m.get_json = lambda url, *a, **k: self.status(clue_pending=7)
            self.assertEqual(m.read_clue_state(), m.ClueState(7, 7, ""))
        finally:
            m.get_json = saved


class DecisionEntranceTest(unittest.TestCase):
    def test_default_url_is_llm_entrance(self):
        self.assertEqual(m.DECISION_URL, "http://127.0.0.1:8100/llm-decision")

    def test_health_check_sends_client_name(self):
        seen = []

        class Resp:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return b'{"status": "ready"}'

        def fake_urlopen(req, timeout=None):
            seen.append(req)
            return Resp()

        real = m.urllib.request.urlopen
        m.urllib.request.urlopen = fake_urlopen
        try:
            self.assertTrue(m.decision_ready())
        finally:
            m.urllib.request.urlopen = real
        self.assertEqual(seen[0].full_url, m.DECISION_URL + "/health")
        self.assertEqual(seen[0].get_header("X-client-name"), "geoframe-loc-morning")
        self.assertIsNone(seen[0].get_header("Origin"))


if __name__ == "__main__":
    unittest.main()
