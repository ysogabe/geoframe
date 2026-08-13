"""ADS-B 連携（server.py L1399-L1401 / L1403 / L1413-L1502 より移動）"""
import random

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
    try:
        raw = http_json(ADSB_URL)
    except Exception as e:
        return {"ok": False, "error": f"ADS-B feed unreachable: {e}"}
    photos = get_manifest_cached()
    budget = [MAX_LOOKUPS_PER_POLL]
    airport_budget = [MAX_AIRPORT_LOOKUPS_PER_POLL]
    home_pref = setting_get("home_pref")   # 未設定なら None（従来どおり除外なし）
    out = []
    for a in raw.get("aircraft", []):
        if "lat" not in a or "lon" not in a or a.get("seen_pos", 99) > 60:
            continue
        hexid = a.get("hex", "").strip()
        callsign = (a.get("flight") or "").strip()
        item = {
            "hex": hexid, "callsign": callsign,
            "lat": a["lat"], "lon": a["lon"],
            "alt": a.get("alt_baro"), "gs": a.get("gs"),
            "track": a.get("track", 0),
            "dist_km": round(a["r_dst"] * 1.852, 1) if "r_dst" in a else None,
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
    return {"ok": True, "receiver": get_receiver(), "aircraft": out}
