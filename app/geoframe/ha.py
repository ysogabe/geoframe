"""Home Assistant 連携（server.py L1152-L1270 より移動）"""
import ipaddress
import json
import threading
import time
import urllib.parse
import urllib.request

from .config import (HA_ENV_ENTITIES, HA_MOTION, HA_TOKEN, HA_URL, IDLE_MIN,
                     SLEEP_CMD, WAKE_CMD)


# ============================================================
# Home Assistant: 人感センサーによる在/不在と画面スリープ制御
# ============================================================
_presence = {"present": True, "last_motion": time.time(), "screen": "on",
             "ha_ok": None, "motion_state": "",
             # night: True=日没後 / False=日の出後 / None=HA未設定・取得失敗
             #        （None のときフロントは固定時刻でのフォールバック判定に切り替える）
             "night": None}

# ナイトモードの判定に使うHAエンティティ。state は above_horizon / below_horizon
HA_SUN = "sun.sun"
# 太陽の状態は1日2回しか変わらないので人感センサー（10秒毎）とは別に約1分間隔で引く。
# warned は同じ失敗ログでコンソールを埋めないための1回だけフラグ
_sun = {"ts": 0.0, "warned": False}


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


def ha_loop():
    import subprocess
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
        idle = time.time() - _presence["last_motion"]
        _presence["present"] = idle < 60
        new_screen = "off" if (HA_MOTION and _presence["ha_ok"] and idle > IDLE_MIN * 60) else "on"
        if new_screen != _presence["screen"]:
            _presence["screen"] = new_screen
            print(f"[ha] 画面{'スリープ' if new_screen == 'off' else '復帰'}（無人{int(idle // 60)}分）")
            cmd = SLEEP_CMD if new_screen == "off" else WAKE_CMD
            if cmd:
                try:
                    subprocess.Popen(cmd, shell=True)
                except Exception as e:
                    print(f"[ha] コマンド失敗: {e}")
        time.sleep(10 if HA_MOTION else 60)


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
    print(f"[ha] Home Assistant連携: ON（{HA_URL} / 人感={HA_MOTION or '未設定・疎通確認のみ'}）")
    threading.Thread(target=ha_loop, daemon=True).start()
