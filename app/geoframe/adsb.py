"""ADS-B 連携（server.py L1399-L1401 / L1403 / L1413-L1502 より移動）"""
import math
import random
import time

from .config import (ADSBDB, ADSB_RECEIVER_URL, ADSB_URL, EMERGENCY_SQUAWKS,
                     MATCH_KM, MAX_AIRPORT_LOOKUPS_PER_POLL, MAX_LOOKUPS_PER_POLL,
                     MIL_OWNER_WORDS, PREF_TO_REGION, RARE_TYPES)
from .geo import resolve_airport_pref
from .photos import get_manifest_cached
from .store import _MISS, cache_get, cache_put, setting_get
from .util import haversine_km, http_json


# ============================================================
# ADS-B 連携
# ============================================================
_receiver = None


def _finite_number(value):
    """ADS-Bの任意数値をJSONへ安全に渡す（ground/NaN/欠損はNone）。"""
    if isinstance(value, bool):
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def _optional_number(value):
    """整数値は整数のまま、その他の有限値はfloatへ正規化する。"""
    number = _finite_number(value)
    if number is None:
        return None
    return int(number) if number.is_integer() else number


def _altitude_value(value):
    """Keep readsb's ``ground`` sentinel while rejecting invalid JSON numbers."""
    if isinstance(value, str) and value.strip().lower() == "ground":
        return "ground"
    return _optional_number(value)


def get_receiver():
    global _receiver
    if _receiver is None:
        try:
            r = http_json(ADSB_RECEIVER_URL)
            if "lat" in r and "lon" in r:
                _receiver = (r["lat"], r["lon"])
        except Exception:
            _receiver = ()
    return _receiver or None


def adsbdb_lookup(kind, key, budget):
    ck = f"adsbdb:{kind}:{key}"
    hit = cache_get(ck, _MISS)       # Falseも「照会済み」を意味する正当な値
    if hit is not _MISS:
        return hit
    if budget[0] <= 0:
        return None
    budget[0] -= 1
    try:
        val = http_json(f"{ADSBDB}/{kind}/{key}").get("response")
        if isinstance(val, str):
            val = False
    except Exception:
        val = False
    cache_put(ck, val)
    return val


def api_aircraft():
    request_now = time.time()
    try:
        raw = http_json(ADSB_URL)
    except Exception as e:
        return {"ok": False, "now": request_now,
                "error": f"ADS-B feed unreachable: {e}"}
    if not isinstance(raw, dict):
        return {"ok": False, "now": request_now,
                "error": "ADS-B feed returned an invalid document"}
    # readsb/tar1090の aircraft.json には観測フィード生成時刻 now がある。
    # 古い受信機やモックが持たない場合はリクエスト時刻へ安全にfallbackする。
    feed_now = _finite_number(raw.get("now"))
    if feed_now is None:
        feed_now = request_now
    photos = get_manifest_cached()
    budget = [MAX_LOOKUPS_PER_POLL]
    airport_budget = [MAX_AIRPORT_LOOKUPS_PER_POLL]
    home_pref = setting_get("home_pref")   # 未設定なら None（従来どおり除外なし）
    out = []
    rows = raw.get("aircraft", [])
    if not isinstance(rows, (list, tuple)):
        rows = []
    for a in rows:
        if not isinstance(a, dict):
            continue
        lat = _finite_number(a.get("lat"))
        lon = _finite_number(a.get("lon"))
        seen_pos = _finite_number(a.get("seen_pos"))
        # 3Dレイヤが「最後に位置を観測した時刻」を判定できるよう、
        # seen_pos欠損も新鮮とはみなさず除外する。通常モードのDOM表示と
        # 同じAPI応答なので、従来の60秒上限も維持する。
        if (lat is None or lon is None or seen_pos is None or
                seen_pos < 0 or seen_pos > 60):
            continue
        hexid = str(a.get("hex") or "").strip()
        callsign = str(a.get("flight") or "").strip()
        track = _optional_number(a.get("track"))
        if track is None:
            track = 0
        r_dst = _finite_number(a.get("r_dst"))
        alt_baro = _altitude_value(a.get("alt_baro"))
        item = {
            "hex": hexid, "callsign": callsign,
            "lat": lat, "lon": lon,
            "alt": alt_baro, "gs": _optional_number(a.get("gs")),
            # altは既存クライアント互換。alt_baroも明示して新クライアントの
            # ground判定と、将来のAPI利用者の移行を容易にする。
            "alt_baro": alt_baro,
            "alt_geom": _optional_number(a.get("alt_geom")),
            "track": track,
            "true_heading": _optional_number(a.get("true_heading")),
            "mag_heading": _optional_number(a.get("mag_heading")),
            "track_rate": _optional_number(a.get("track_rate")),
            "roll": _optional_number(a.get("roll")),
            "geom_rate": _optional_number(a.get("geom_rate")),
            "baro_rate": _optional_number(a.get("baro_rate")),
            "seen_pos": seen_pos,
            "dist_km": round(r_dst * 1.852, 1) if r_dst is not None else None,
            "squawk": a.get("squawk", ""),
            "type": a.get("t", ""), "reg": a.get("r", ""),
            "owner": "", "photo_thumb": None, "route": None,
            "rare": False, "emergency": a.get("squawk") in EMERGENCY_SQUAWKS,
            "match": None,
        }
        info = adsbdb_lookup("aircraft", hexid, budget) if hexid else None
        if isinstance(info, dict):
            ac = info.get("aircraft") or {}
            item["type"] = item["type"] or ac.get("icao_type", "")
            item["reg"] = item["reg"] or ac.get("registration", "")
            item["owner"] = ac.get("registered_owner", "") or ""
            item["photo_thumb"] = ac.get("url_photo_thumbnail")
        owner_l = item["owner"].lower()
        item["rare"] = (item["type"] in RARE_TYPES
                        or any(w in owner_l for w in MIL_OWNER_WORDS)
                        or bool(a.get("dbFlags", 0) & 1))
        route = adsbdb_lookup("callsign", callsign, budget) if callsign else None
        if isinstance(route, dict):
            fr = (route.get("flightroute") or {})
            o, d = fr.get("origin") or {}, fr.get("destination") or {}
            if d.get("latitude") is not None:
                item["route"] = {
                    "from": {"iata": o.get("iata_code", ""), "city": o.get("municipality", "")},
                    "to": {"iata": d.get("iata_code", ""), "city": d.get("municipality", ""),
                           "lat": d.get("latitude"), "lon": d.get("longitude")},
                }
                dest_pref, in_jp = resolve_airport_pref(
                    d["latitude"], d["longitude"], airport_budget)
                if not in_jp or (in_jp and not dest_pref):
                    # ③ 海外、または国内だが都道府県解決に失敗（一時的）→ 既存の80km半径マッチ
                    best = None
                    for p in photos:
                        km = haversine_km(d["latitude"], d["longitude"], p["lat"], p["lon"])
                        if km <= MATCH_KM and (best is None or km < best[0]):
                            best = (km, p)
                    if best:
                        item["match"] = {"km": round(best[0]), "file": best[1]["file"],
                                         "title": best[1]["title"], "place": best[1]["place"],
                                         "time": best[1]["time"]}
                elif home_pref and PREF_TO_REGION.get(dest_pref) == PREF_TO_REGION.get(home_pref):
                    pass   # ② 自宅と同じ地方 → 演出スキップ（routeは残る）
                else:
                    # ① 都道府県ランダム
                    pool = [p for p in photos if p["pref"] == dest_pref]
                    if pool:
                        p = random.choice(pool)
                        km = round(haversine_km(d["latitude"], d["longitude"], p["lat"], p["lon"]))
                        item["match"] = {"km": km, "file": p["file"], "title": p["title"],
                                         "place": p["place"], "time": p["time"]}
        out.append(item)
    # ここでのsave_cacheは行わない（5秒毎ポーリング×全量書き込みでSD/eMMCを消耗させるため）。
    # 新規に引いたadsbdb結果は cache_put でdirty化され、cache_saverが60秒間隔で書き出す
    out.sort(key=lambda x: x["dist_km"] if x["dist_km"] is not None else 9e9)
    # nowはフィード生成時刻。各機体のseen_pos（秒）と組み合わせることで、
    # 3D側が古い位置を除外しつつ、アラート中の補間基準を持てる。
    return {"ok": True, "now": feed_now, "receiver": get_receiver(), "aircraft": out}
