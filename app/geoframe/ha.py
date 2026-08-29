"""Home Assistant 連携（server.py L1152-L1270 より移動）"""
import ipaddress
import json
import subprocess
import threading
import time
import urllib.parse
import urllib.request

from . import screen_control
from .config import (HA_ENV_ENTITIES, HA_MOTION, HA_SCREEN_DECAY_MIN,
                     HA_SCREEN_MODE_ENTITY, HA_SCREEN_RECONCILE_SEC, HA_TOKEN,
                     HA_URL, IDLE_MIN, SCREEN_STATUS_CMD, SLEEP_CMD, WAKE_CMD)


# ============================================================
# Home Assistant: 人感センサーによる在/不在と画面スリープ制御
# ============================================================
_presence = {"present": True, "last_motion": time.time(), "screen": "on",
             "ha_ok": None, "motion_state": "",
             # night: True=日没後 / False=日の出後 / None=HA未設定・取得失敗
             #        （None のときフロントは固定時刻でのフォールバック判定に切り替える）
             "night": None,
             # mode: 画面電源の実効モード（auto / force_on / force_off）。
             #       HAの生の値ではなく effective_mode() で減衰・正規化した後の値
             "mode": "auto",
             # screen_confirmed: 実機の状態を読んで意図と一致したことを確認できたか。
             #                   True=確認済み / None=未確認（意図を書き換えた直後も含む）
             "screen_confirmed": None}

# ナイトモードの判定に使うHAエンティティ。state は above_horizon / below_horizon
HA_SUN = "sun.sun"
# 太陽の状態は1日2回しか変わらないので人感センサー（10秒毎）とは別に約1分間隔で引く。
# warned は同じ失敗ログでコンソールを埋めないための1回だけフラグ
_sun = {"ts": 0.0, "warned": False}

# 画面電源制御の内部作業状態。_presence（＝/api/presence でそのまま外に出る公開の器）
# とは意図的に分けている。ここは実装都合の値だけを置く
_screen_state = {
    # HAから読んだ生のmode文字列。未設定・一度も取得できていない場合は "auto" のまま。
    # 取得失敗時は直前の値を保持する（捨ててしまうと減衰の仕組みが意味を失う）
    "raw_mode": "auto",
    # mode entity への最終「疎通成功」時刻。値が VALID_MODES に無くても更新する。
    # staleness は「HAと話せているか」の話であって「値が妥当か」の話ではないため。
    # 値の妥当性で更新を止めると、HA側の選択肢タイポのような恒久的な設定ミスが
    # 「HA断」に化けて、直っていないのに減衰警告を出し続けることになる
    "mode_last_ok_ts": time.time(),
    "warned": False,           # 取得失敗ログの1回だけフラグ（_sun["warned"] と同型）
    "reconcile_ts": 0.0,       # 直近のreconcile実行時刻
    "force_off_since": None,   # force_off が続いている場合の開始時刻
    "force_off_warned": False,  # 24時間超過WARNの1回だけフラグ
    # 直近コマンドを発火した desired_screen 値。同じ値への再発火でログを出さないため
    "last_cmd_screen": None,
}


def _ha_url_safe():
    """HA_URL宛にトークンを送ってよいか判定する。戻り値: (許可か, 不許可の理由)
    平文http＋名前解決が要るホスト名（既定の homeassistant.local など）を許可しない。
    LAN内の悪意ある機器はmDNS応答レースに勝つだけでよく、こちらは60秒毎に/api/を叩くので
    待っているだけで Authorization: Bearer <トークン> がまるごと相手に渡る。
    他の上流（ADS-B・adsbdb・JMA・GSI・MLIT）は全てhttpsで、ここだけがMITM可能だった。
    httpでもIPアドレス直指定（192.0.2.x / 127.0.0.1 / ::1等）は名前解決を伴わず
    レースの対象外なので許可する（大半の利用者はこの形で運用するため塞がない）。"""
    u = urllib.parse.urlparse(HA_URL)
    if u.scheme == "https":
        return True, ""
    host = (u.hostname or "").strip()      # urlparseは[::1]形式の角括弧も外してくれる
    if not host:
        return False, f"GEOFRAME_HA_URL を解釈できない（{HA_URL}）"
    if host == "localhost":
        return True, ""
    try:
        ipaddress.ip_address(host)         # IPv4/IPv6リテラルならDNS/mDNSを引かない
        return True, ""
    except (ValueError, TypeError):
        pass
    return False, (f"平文httpでホスト名『{host}』宛にトークンを送る設定になっている"
                   "（mDNS/DNS応答を偽装されるとトークンを盗まれる）")


_ha_url_ok, _ha_url_ng = _ha_url_safe()


def ha_get(path):
    # ha_start()以外の経路（/api/ha/entities 等）からも同じガードで弾く。
    # 呼ばれる度にprintするとログが溢れるので、ここでは例外にして呼び元の表示に任せる
    if not _ha_url_ok:
        raise RuntimeError(f"HA_URLが安全でないため接続しません: {_ha_url_ng}")
    req = urllib.request.Request(f"{HA_URL}{path}", headers={
        "Authorization": f"Bearer {HA_TOKEN}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read().decode("utf-8"))


_room_env = {"ok": False, "items": [], "ts": 0}


def ha_fetch_room_env():
    """ROOM ENV用エンティティを一括取得（/api/statesを1回叩いて抽出）"""
    if not HA_ENV_ENTITIES:
        return
    try:
        by_id = {s.get("entity_id"): s for s in ha_get("/api/states")}
        items = []
        for eid in HA_ENV_ENTITIES:
            s = by_id.get(eid)
            if not s or s.get("state") in ("unknown", "unavailable", ""):
                continue
            at = s.get("attributes") or {}
            items.append({"entity": eid, "value": s["state"],
                          "unit": at.get("unit_of_measurement", ""),
                          "class": at.get("device_class", ""),
                          "name": at.get("friendly_name", eid)})
        _room_env.update(ok=True, items=items, ts=time.time())
    except Exception as e:
        _room_env["ok"] = False
        print(f"[ha] ROOM ENV取得失敗: {e}")


def ha_fetch_screen_mode():
    """画面モード（input_select）を取得する。

    ※人感センサー／sun.sun の取得とは必ず別のtry/exceptにすること。同じブロックに
      同居させると、GEOFRAME_HA_SCREEN_MODE_ENTITY のタイポ1つで
      _presence["ha_ok"] が恒久Falseになり、自動スリープもオーバーライドの
      減衰判定も巻き添えで壊れる。
    未設定なら何もしない＝raw_modeは初期値 "auto" のままで、従来の人感センサー
    任せの式と完全に同一の挙動へフォールバックする（安全なrollback経路）。
    """
    if not HA_SCREEN_MODE_ENTITY:
        return
    try:
        st = ha_get(f"/api/states/{HA_SCREEN_MODE_ENTITY}") or {}
        _screen_state["raw_mode"] = st.get("state", "")
        # 値の妥当性は問わず、疎通できた時点で更新する（_screen_state のコメント参照）
        _screen_state["mode_last_ok_ts"] = time.time()
        _screen_state["warned"] = False
    except Exception as e:
        # 直前の raw_mode は保持したまま、時間経過で effective_mode() に減衰させる
        if not _screen_state["warned"]:
            _screen_state["warned"] = True
            print(f"[ha] {HA_SCREEN_MODE_ENTITY} を取得できません"
                  f"（不通が{HA_SCREEN_DECAY_MIN}分続いたらautoへ戻します）: {e}")


def reconcile_screen():
    """実機の画面状態を読み、こちらの意図（_presence["screen"]）と突き合わせる。

    帯域外のDPMS変化（誰かが手でxsetした・別プロセスが介入した等）を検知して
    意図を実機へ追従させるだけで、ここではコマンドを発火しない。ポリシー執行は
    ha_loop() のエッジ発火に一本化する（二箇所から画面を叩かない）。
    """
    if not SCREEN_STATUS_CMD:
        return
    if time.time() - _screen_state["reconcile_ts"] <= HA_SCREEN_RECONCILE_SEC:
        return
    # subprocess.run より前に更新する。後置すると、statusコマンドが失敗／
    # タイムアウトし続ける環境で10秒毎に叩き続けてしまう
    # （_sun["ts"] の「失敗時も次の試行まで空ける」と同じイディオム）
    _screen_state["reconcile_ts"] = time.time()
    try:
        # check=True は使わないこと。screen-ctl.sh の status は正常な「off」判定でも
        # exit 1 を返す仕様なので、終了コードでの成否判定は誤り。標準出力だけを見る
        r = subprocess.run(SCREEN_STATUS_CMD, shell=True, capture_output=True,
                           text=True, timeout=5)
    except Exception as e:
        # 実機の状態が読めなかっただけ。意図は書き換えない（勝手に諦めない）
        print(f"[ha] 画面状態の取得に失敗（意図は保持します）: {e}")
        return
    actual, matched = screen_control.reconcile(_presence["screen"], r.stdout)
    if actual is None:
        return                              # unknown・空文字列など解釈不能
    if matched:
        _presence["screen_confirmed"] = True
        return
    # 帯域外で実機が変わっていた。意図を実機へ追従させ、確認済みフラグは落とす
    # （実機から読んだばかりの値であって、まだ「確定した意図」ではないため）。
    # 直後の decide() がこの値を起点に正しい意図を計算し、必要ならエッジ発火する
    print(f"[ha] 画面状態が意図と不一致（意図={_presence['screen']} / 実機={actual}）"
          "→ 実機に追従します")
    _presence["screen"] = actual
    _presence["screen_confirmed"] = None


def ha_loop():
    while True:
        if time.time() - _room_env["ts"] > 55:
            ha_fetch_room_env()
        try:
            if HA_MOTION:
                st = ha_get(f"/api/states/{HA_MOTION}")
                _presence["ha_ok"] = True
                _presence["motion_state"] = st.get("state", "")
                if st.get("state") in ("on", "home"):   # 動き検知 or 在宅（person/device_tracker対応）
                    _presence["last_motion"] = time.time()
            else:
                ha_get("/api/")                       # 疎通確認のみ
                _presence["ha_ok"] = True
        except Exception as e:
            if _presence["ha_ok"] is not False:
                print(f"[ha] 接続失敗（リトライ継続）: {e}")
            _presence["ha_ok"] = False
        # ナイトモード用の日没判定。
        # ※人感センサーの有無（上のif/else）に関わらず走らせる。else側の中に入れると
        #   GEOFRAME_HA_MOTION_ENTITY を設定している構成でnightが永久にNoneになる
        if time.time() - _sun["ts"] > 55:
            _sun["ts"] = time.time()        # 失敗時も次の試行まで55秒空ける（連打しない）
            try:
                state = (ha_get(f"/api/states/{HA_SUN}") or {}).get("state", "")
                # unknown / unavailable / 想定外の値は「判定できない」= None にして
                # フロントの時刻フォールバックへ委ねる（古い値を残さない）
                _presence["night"] = (state == "below_horizon") \
                    if state in ("above_horizon", "below_horizon") else None
                _sun["warned"] = False
            except Exception as e:
                _presence["night"] = None
                if not _sun["warned"]:
                    _sun["warned"] = True
                    print(f"[ha] {HA_SUN} を取得できません"
                          f"（ナイトモードは端末の時刻で判定します）: {e}")
        # 画面モードの取得 → 実機との突き合わせ、の順に行う。
        # 先にreconcileで _presence["screen"] を実機へ追従させ、その直後の
        # decide() が新しい意図を計算する（逆順だと実機追従の反映が1サイクル遅れる）
        ha_fetch_screen_mode()
        reconcile_screen()
        idle = time.time() - _presence["last_motion"]
        _presence["present"] = idle < 60
        # ※分→秒の変換を落とさないこと。GEOFRAME_IDLE_MIN も
        #   GEOFRAME_HA_SCREEN_DECAY_MIN も分単位の環境変数で、判定側は秒で受け取る。
        #   * 60 を落とすと「10分で消灯」が「10秒で消灯」になる
        idle_min_s = IDLE_MIN * 60
        decay_after_s = HA_SCREEN_DECAY_MIN * 60
        mode_stale_s = time.time() - _screen_state["mode_last_ok_ts"]
        mode = screen_control.effective_mode(_screen_state["raw_mode"],
                                             mode_stale_s, decay_after_s)
        _presence["mode"] = mode
        new_screen = screen_control.decide(mode, _presence["ha_ok"], idle,
                                           ha_motion_configured=bool(HA_MOTION),
                                           idle_min_s=idle_min_s)
        if new_screen != _presence["screen"]:
            _presence["screen"] = new_screen
            _presence["screen_confirmed"] = None   # 意図を書き換えた直後は常に未確認へ
            # 同じ desired_screen への再発火ではログを省く。DPMS拡張が無効で
            # `xset dpms force off` が実機に無視される環境では、reconcileが毎回
            # 不一致を検出→意図を書き戻し→decide()がまた同じ値を要求、のループに
            # なりうる。コマンドはべき等なので送り続けてよいが、ログは遷移が
            # 実際に変わった時だけにする
            if _screen_state["last_cmd_screen"] != new_screen:
                if mode == "auto":
                    print(f"[ha] 画面{'スリープ' if new_screen == 'off' else '復帰'}"
                          f"（無人{int(idle // 60)}分）")
                else:
                    print(f"[ha] 画面{'消灯' if new_screen == 'off' else '点灯'}"
                          f"（手動オーバーライド: {mode}）")
            _screen_state["last_cmd_screen"] = new_screen
            # ※cmd は運用者が環境変数で与えた文字列そのもの。モード名やエンティティの
            #   値など外部由来の値を shell=True のコマンド文字列へ結合しないこと
            cmd = SLEEP_CMD if new_screen == "off" else WAKE_CMD
            if cmd:
                try:
                    subprocess.Popen(cmd, shell=True)
                except Exception as e:
                    print(f"[ha] コマンド失敗: {e}")
        # force_off の長時間継続を1回だけ警告する。自動解除は一切しない
        # （「意図して消したまま」を勝手に覆さないため。戻すのは人の操作）
        if mode == "force_off":
            if _screen_state["force_off_since"] is None:
                _screen_state["force_off_since"] = time.time()
                _screen_state["force_off_warned"] = False
            elif (not _screen_state["force_off_warned"]
                  and time.time() - _screen_state["force_off_since"] > 24 * 3600):
                _screen_state["force_off_warned"] = True
                print("[ha] force_offが24時間以上継続しています"
                      "（自動解除はしません。手動でautoへ戻してください）")
        else:
            _screen_state["force_off_since"] = None
            _screen_state["force_off_warned"] = False
        # 画面モードだけを設定している構成（人感センサー無し）でも10秒粒度で追従させる
        time.sleep(10 if (HA_MOTION or HA_SCREEN_MODE_ENTITY) else 60)


def ha_start():
    if not HA_TOKEN:
        print("[ha] トークンなし → Home Assistant連携OFF")
        return
    # トークンがある場合のみURLを検査する（トークンが無ければ漏れるものが無い）
    if not _ha_url_ok:
        print(f"[ha] Home Assistant連携OFF（安全でない接続先）: {_ha_url_ng}")
        print(f"[ha]   現在の接続先: {HA_URL}")
        print("[ha]   app/.env に Home Assistant のIPアドレスを直接指定してください:")
        print("[ha]     GEOFRAME_HA_URL=http://<HAのIPアドレス>:8123   （例: http://192.0.2.10:8123）")
        print("[ha]   HAをHTTPS化している場合は https:// のURLでも構いません。")
        return
    print(f"[ha] Home Assistant連携: ON（{HA_URL} / 人感={HA_MOTION or '未設定・疎通確認のみ'}"
          f" / 画面モード={HA_SCREEN_MODE_ENTITY or '未設定・auto固定'}）")
    threading.Thread(target=ha_loop, daemon=True).start()
