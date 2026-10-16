"""deploy/loc_decision_morning.py（毎朝の Decision 起動）のテスト"""
import importlib.util
import subprocess
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

    def test_own_decision_is_not_counted(self):
        own = with_gpu(util_pct=99, processes=[{"pid": 7, "name": "python", "vram_mib": 2344}])
        self.assertEqual(m.gpu_busy_reasons(own, [], own_pids={7}), [])
        other = with_gpu(processes=[{"pid": 7, "vram_mib": 2344}, {"pid": 9, "vram_mib": 3000}])
        self.assertTrue(m.gpu_busy_reasons(other, [], own_pids={7}))


class FakeClock:
    def __init__(self):
        self.t = 0.0

    def sleep(self, s):
        self.t += s

    def __call__(self):
        return self.t


class RunTest(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.pending = [3, 2, 0]
        self.active = False
        self.gpu = (FREE, [])
        self.ready_after = 0
        self.saved = {k: getattr(m, k) for k in (
            "clue_pending", "decision_active", "read_gpu_state", "systemctl",
            "decision_ready", "decision_pid", "gpu_busy_reasons")}
        m.clue_pending = lambda: self.pending.pop(0) if len(self.pending) > 1 else self.pending[0]
        m.decision_active = lambda: self.active
        m.read_gpu_state = lambda: self.gpu
        m.decision_pid = lambda: 7
        m.decision_ready = self._ready
        m.systemctl = self._systemctl
        real = self.saved["gpu_busy_reasons"]
        m.gpu_busy_reasons = lambda s, o, now=None, own_pids=(): real(s, o, None, own_pids)

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(m, k, v)

    def _ready(self):
        self.ready_after -= 1
        return self.ready_after < 0

    def _systemctl(self, *args):
        self.calls.append(args[0])
        return subprocess.CompletedProcess(args, 0, "", "")

    def run_script(self):
        clock = FakeClock()
        return m.run(sleep=clock.sleep, clock=clock)

    def test_nothing_pending(self):
        self.pending = [0]
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])

    def test_already_started_by_hand_is_left_alone(self):
        self.active = True
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])

    def test_gpu_busy_skips(self):
        self.gpu = ({**FREE, "lease": {"count": 1}}, [])
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, [])

    def test_runs_until_pending_is_zero_then_stops(self):
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_not_ready_in_time_stops(self):
        self.ready_after = 10 ** 6
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_stall_stops(self):
        self.pending = [5]
        self.assertEqual(self.run_script(), 1)
        self.assertEqual(self.calls, ["start", "stop"])

    def test_yields_when_someone_else_takes_the_gpu(self):
        self.pending = [5, 4, 3, 2, 1, 0]
        state = iter([(FREE, [])] + [({**FREE, "lease": {"count": 1}}, [])] * 10)
        m.read_gpu_state = lambda: next(state)
        self.assertEqual(self.run_script(), 0)
        self.assertEqual(self.calls, ["start", "stop"])


if __name__ == "__main__":
    unittest.main()
