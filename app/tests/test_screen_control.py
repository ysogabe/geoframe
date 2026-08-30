"""screen_control（画面電源の判定ロジック）のテスト。

対象が純粋関数だけなので、モックもフィクスチャも要らない。
"""
import unittest

from geoframe import screen_control

IDLE_MIN_S = 600.0        # GEOFRAME_IDLE_MIN の既定10分を秒にしたもの
DECAY_S = 300.0           # GEOFRAME_HA_SCREEN_DECAY_MIN の既定5分を秒にしたもの


def decide(mode, ha_ok=True, idle_s=0.0, ha_motion_configured=True,
           idle_min_s=IDLE_MIN_S):
    """既定値付きの薄いラッパ。各テストが着目する引数だけを書けるようにする"""
    return screen_control.decide(mode, ha_ok, idle_s,
                                 ha_motion_configured=ha_motion_configured,
                                 idle_min_s=idle_min_s)


class DecideAutoModeTest(unittest.TestCase):
    """auto の式が従来の ha.py の式と完全に一致することを確認する。
    従来式: "off" if (HA_MOTION and ha_ok and idle > IDLE_MIN * 60) else "on"
    """

    def test_unattended_past_threshold_turns_screen_off(self):
        self.assertEqual(decide("auto", idle_s=IDLE_MIN_S + 1), "off")

    def test_idle_exactly_at_threshold_stays_on(self):
        # `>` であって `>=` ではない。ちょうど10分の時点ではまだ消さない
        self.assertEqual(decide("auto", idle_s=IDLE_MIN_S), "on")

    def test_recent_motion_keeps_screen_on(self):
        self.assertEqual(decide("auto", idle_s=0.0), "on")

    def test_ha_unreachable_falls_back_to_on(self):
        self.assertEqual(decide("auto", ha_ok=False, idle_s=IDLE_MIN_S + 1), "on")

    def test_ha_state_unknown_at_startup_falls_back_to_on(self):
        # _presence["ha_ok"] の初期値は None。falsy なので従来どおり "on" になる
        self.assertEqual(decide("auto", ha_ok=None, idle_s=IDLE_MIN_S + 1), "on")

    def test_without_motion_entity_screen_is_always_on(self):
        self.assertEqual(
            decide("auto", ha_motion_configured=False, idle_s=IDLE_MIN_S + 1), "on")

    def test_unknown_mode_falls_back_to_auto_expression(self):
        # 防御的フォールバック。effective_mode を通していない値が万一届いても
        # 「消えたまま戻らない」にはしない
        self.assertEqual(decide("foo", idle_s=IDLE_MIN_S + 1), "off")
        self.assertEqual(decide("foo", idle_s=0.0), "on")


class DecideOverrideTest(unittest.TestCase):
    """優先度 force_off > force_on > auto"""

    def test_force_off_wins_over_detected_motion(self):
        self.assertEqual(decide("force_off", idle_s=0.0), "off")

    def test_force_off_wins_even_when_ha_is_unreachable(self):
        # auto なら "on" に倒れる条件だが、明示的な意図はそれより優先する
        self.assertEqual(decide("force_off", ha_ok=False, idle_s=0.0), "off")
        self.assertEqual(decide("force_off", ha_ok=None, idle_s=0.0), "off")

    def test_force_off_works_without_motion_entity(self):
        # auto 式は常に "on" を返す経路なので、ここが "off" ならオーバーライドが
        # 効いていることの確認になる（人感センサー無し・手動制御のみの構成）
        self.assertEqual(
            decide("force_off", ha_motion_configured=False, idle_s=0.0), "off")

    def test_force_on_wins_over_long_absence(self):
        self.assertEqual(decide("force_on", idle_s=IDLE_MIN_S * 100), "on")

    def test_force_on_wins_even_when_ha_is_unreachable(self):
        self.assertEqual(
            decide("force_on", ha_ok=False, idle_s=IDLE_MIN_S * 100), "on")


class EffectiveModeTest(unittest.TestCase):
    def test_override_is_kept_while_outage_is_short(self):
        self.assertEqual(
            screen_control.effective_mode("force_off", DECAY_S - 1, DECAY_S),
            "force_off")

    def test_override_is_kept_exactly_at_threshold(self):
        # `>` であって `>=` ではない
        self.assertEqual(
            screen_control.effective_mode("force_off", DECAY_S, DECAY_S), "force_off")
        self.assertEqual(
            screen_control.effective_mode("force_on", DECAY_S, DECAY_S), "force_on")

    def test_override_decays_to_auto_past_threshold(self):
        self.assertEqual(
            screen_control.effective_mode("force_off", DECAY_S + 1, DECAY_S), "auto")
        self.assertEqual(
            screen_control.effective_mode("force_on", DECAY_S + 1, DECAY_S), "auto")

    def test_auto_never_decays(self):
        self.assertEqual(screen_control.effective_mode("auto", 0.0, DECAY_S), "auto")
        self.assertEqual(
            screen_control.effective_mode("auto", DECAY_S * 1000, DECAY_S), "auto")

    def test_case_variants_are_treated_as_invalid(self):
        # 正規化しない。VALID_MODES への厳密一致のみを有効な値として扱う
        self.assertEqual(
            screen_control.effective_mode("Force_Off", 0.0, DECAY_S), "auto")
        self.assertEqual(screen_control.effective_mode("AUTO", 0.0, DECAY_S), "auto")

    def test_invalid_values_normalize_to_auto(self):
        for bad in ("foo", "", "unknown", "unavailable", "on"):
            with self.subTest(bad=bad):
                self.assertEqual(
                    screen_control.effective_mode(bad, 0.0, DECAY_S), "auto")


class DecayScenarioTest(unittest.TestCase):
    def test_force_off_during_long_ha_outage_ends_up_back_on(self):
        """外出先で force_off にした直後にHAが落ちても、画面が二度と点かなくならない"""
        # 不通が始まった直後: まだ意図を尊重して消灯のまま
        mode = screen_control.effective_mode("force_off", 60.0, DECAY_S)
        self.assertEqual(mode, "force_off")
        self.assertEqual(decide(mode, ha_ok=False, idle_s=0.0), "off")
        # 減衰後: auto へ戻り、HA不通なので auto 式が安全側の "on" を返す
        mode = screen_control.effective_mode("force_off", DECAY_S + 1, DECAY_S)
        self.assertEqual(mode, "auto")
        self.assertEqual(decide(mode, ha_ok=False, idle_s=IDLE_MIN_S * 100), "on")


class ReconcileTest(unittest.TestCase):
    def test_actual_matches_intent(self):
        self.assertEqual(screen_control.reconcile("on", "on\n"), ("on", True))
        self.assertEqual(screen_control.reconcile("off", "off\n"), ("off", True))

    def test_actual_differs_from_intent(self):
        self.assertEqual(screen_control.reconcile("on", "off\n"), ("off", False))
        self.assertEqual(screen_control.reconcile("off", "on\n"), ("on", False))

    def test_uninterpretable_output_changes_nothing(self):
        # unknown（screen-ctl.sh が xset q に失敗した場合の出力）・空文字列とも
        # (None, None) を返し、呼び出し側に意図の書き換えをさせない
        self.assertEqual(screen_control.reconcile("on", "unknown\n"), (None, None))
        self.assertEqual(screen_control.reconcile("on", ""), (None, None))
        self.assertEqual(screen_control.reconcile("on", "   \n"), (None, None))
        self.assertEqual(screen_control.reconcile("on", "Monitor is On"), (None, None))

    def test_surrounding_whitespace_and_case_are_absorbed(self):
        self.assertEqual(screen_control.reconcile("on", "  ON  \n"), ("on", True))


if __name__ == "__main__":
    unittest.main()
