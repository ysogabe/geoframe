"""画面電源の判定ロジック（純粋関数のみ・I/Oは一切しない）

ha.py の ha_loop() から呼ばれる。HTTP・subprocess・時刻取得をここに持ち込まないので、
テストからモック無しでそのまま呼べる。逆に言えば、設定値（IDLE_MIN 等）も
config から読まずに引数で受け取ること。
"""

# HA側の input_select（GEOFRAME_HA_SCREEN_MODE_ENTITY）が取りうる値。
# ここに「厳密一致」しないものは全て不正値として auto へ倒す。
# 大文字小文字の揺れも正規化しない（"Force_Off" は不正値＝auto）。
# 理由: 選択肢のタイポや unknown/unavailable が「画面が消えたまま戻らない」に
#       化けるのが最悪の失敗なので、迷ったら人感センサー任せの auto に落とす。
VALID_MODES = ("auto", "force_on", "force_off")


def effective_mode(mode: str, mode_stale_s: float, decay_after_s: float) -> str:
    """force_on/force_off を実効モードへ正規化。

    mode          : HAから読んだ生の値
    mode_stale_s  : mode entity へ最後に「疎通できた」時刻からの経過秒
    decay_after_s : オーバーライドを捨てて auto へ戻すまでの秒数（分ではない）

    HAが長時間不通のときにオーバーライドを維持し続けると、外出先で force_off に
    したまま HA が落ちた場合に画面が二度と点かなくなる。一定時間で auto へ
    減衰させ、人感センサー側の判断へ戻す。
    """
    if mode not in VALID_MODES:
        return "auto"
    if mode == "auto":
        return "auto"                       # auto 自体に減衰の概念は無い
    # ちょうど閾値と一致する時点ではまだ有効（`>` であって `>=` ではない）
    if mode_stale_s > decay_after_s:
        return "auto"
    return mode


def decide(mode: str, ha_ok, idle_s: float, *,
           ha_motion_configured: bool, idle_min_s: float) -> str:
    """画面をどうすべきかを決める。優先度: force_off > force_on > auto。

    mode                 : effective_mode() の戻り値
    ha_ok                : HAとの疎通状態（True/False/None。None は起動直後）
    idle_s               : 最後に人を検知してからの経過秒
    ha_motion_configured : 人感エンティティが設定されているか
    idle_min_s           : 無人と見なすまでの秒数（分ではない）

    戻り値は "on" か "off"。
    """
    if mode == "force_off":
        return "off"                        # ha_ok・idle_s に関わらず無条件
    if mode == "force_on":
        return "on"
    # auto（および防御的フォールバックとしての未知の値）。
    # ha.py の従来式 `"off" if (HA_MOTION and ha_ok and idle > IDLE_MIN * 60) else "on"`
    # と完全に同一。境界の idle_s == idle_min_s は "on"（`>` であって `>=` ではない）
    return "off" if (ha_motion_configured and ha_ok and idle_s > idle_min_s) else "on"


def reconcile(expected_screen: str, status_output: str):
    """実機の画面状態と、こちらの意図を突き合わせる。戻り値 (actual, matched)。

    status_output は deploy/kiosk/screen-ctl.sh status の標準出力。
    ※終了コードは一切見ない。あのスクリプトは `set -euo pipefail` 下で
      正常な「画面off」判定でも exit 1 を返す仕様のため、終了コードで
      成否を判定するのは誤り（呼び出し側でも check=True を使わないこと）。

    解釈できない出力（unknown・空文字列・想定外の文字列）は (None, None) を返し、
    呼び出し側に「何もしない」を選ばせる（古い値も新しい値も書き込まない）。
    """
    actual = (status_output or "").strip().lower()
    if actual not in ("on", "off"):
        return None, None
    return actual, actual == expected_screen
