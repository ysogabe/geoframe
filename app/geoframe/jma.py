"""気象庁: 天気予報・地震（server.py L1273-L1338 より移動）"""
import re
import time
from datetime import datetime

from .config import JMA_FORECAST, JMA_QUAKES
from .util import http_json


_env_cache = {"ts": 0, "data": None}

# 週間予報とみなす timeSeries の最小日数。
# 同じJSONに入っている「今日・明日」の系列は timeDefines が2〜4個なので、
# ここを閾値にすると週間（7日）の系列だけが候補に残る
WEEKLY_MIN_DAYS = 5
WEEKLY_MAX_DAYS = 7


def _ymd(s):
    """timeDefines の "2026-07-30T00:00:00+09:00" から "2026-07-30" を取る"""
    return s[:10] if isinstance(s, str) and len(s) >= 10 else ""


def _int_or_none(v):
    """気象庁は未確定値を空文字 "" で返す（週間予報の初日の降水確率・気温など）"""
    try:
        return int(str(v).strip())
    except (TypeError, ValueError):
        return None


def _parse_weekly(fc):
    """気象庁 forecast JSON から週間予報を抽出する。

    timeSeries の要素数・並びは地域によって変わるので添字を決め打ちしない。
    「areas[0] が持つキー名」＋「timeDefines の個数」で系列を見分ける:
      - 天気・降水確率 … weatherCodes を持ち timeDefines が WEEKLY_MIN_DAYS 個以上
      - 最高／最低気温 … tempsMax / tempsMin を持つ系列（観測地点単位なので日付で突き合わせる）
    構造が想定と違えば空リストを返す。例外は必ずここで飲む（/api/env 全体を落とさない）。
    """
    try:
        wx = tmp = None
        for blk in (fc if isinstance(fc, list) else []):
            if not isinstance(blk, dict):
                continue
            for ts in (blk.get("timeSeries") or []):
                if not isinstance(ts, dict):
                    continue
                tds, areas = ts.get("timeDefines") or [], ts.get("areas") or []
                if len(tds) < WEEKLY_MIN_DAYS or not areas or not isinstance(areas[0], dict):
                    continue
                a0 = areas[0]
                # 同種の候補が複数あれば日数の多い方を採る
                if "weatherCodes" in a0 and (wx is None or len(tds) > len(wx[0])):
                    wx = (tds, a0)
                if (("tempsMax" in a0 or "tempsMin" in a0)
                        and (tmp is None or len(tds) > len(tmp[0]))):
                    tmp = (tds, a0)
        if wx is None:
            return []
        tds, a0 = wx
        codes, pops = a0.get("weatherCodes") or [], a0.get("pops") or []
        tmin, tmax = {}, {}
        if tmp:
            ttds, t0 = tmp
            tmin = {_ymd(d): v for d, v in zip(ttds, t0.get("tempsMin") or [])}
            tmax = {_ymd(d): v for d, v in zip(ttds, t0.get("tempsMax") or [])}
        out = []
        for i, td in enumerate(tds[:WEEKLY_MAX_DAYS]):
            date = _ymd(td)
            if not date:
                continue
            out.append({
                "date": date,
                "code": str(codes[i]) if i < len(codes) else "",
                "pop": _int_or_none(pops[i] if i < len(pops) else ""),
                "temp_min": _int_or_none(tmin.get(date)),
                "temp_max": _int_or_none(tmax.get(date)),
            })
        return out
    except Exception as e:
        print(f"[jma] 週間予報の解析に失敗（weeklyは空で返します）: {e}")
        return []


def api_env():
    """気象庁: 天気予報（今日＋週間）＋直近24時間の地震（5分キャッシュ）"""
    if _env_cache["data"] and time.time() - _env_cache["ts"] < 300:
        return _env_cache["data"]
    out = {"weather": None, "weekly": [], "quakes": []}
    fc = None
    try:
        fc = http_json(JMA_FORECAST)
        area0 = fc[0]["timeSeries"][0]["areas"][0]
        w = {"area": area0["area"]["name"],
             "text": area0["weathers"][0].replace("　", "")}
        try:
            pops = fc[0]["timeSeries"][1]["areas"][0]["pops"]
            w["pop"] = max(int(p) for p in pops if p != "")
        except Exception:
            pass
        try:
            temps = [t for t in fc[0]["timeSeries"][2]["areas"][0]["temps"] if t != ""]
            if temps:
                w["temps"] = temps[:2]
        except Exception:
            pass
        out["weather"] = w
    except Exception as e:
        print(f"[jma] 予報取得失敗: {e}")
    # 週間予報は取得済みのJSONを「別のtry」で解析する。
    # 上の out["weather"] = w より後に置くこと（同じtryに入れると週間の失敗で今日分も捨ててしまう）
    if fc is not None:
        out["weekly"] = _parse_weekly(fc)

    def cod_deg(v, limit):
        """JMAのcodは電文により度表記(+32.5)と度分表記(+3237.5)が混在するため吸収"""
        v = float(v)
        sign = -1 if v < 0 else 1
        v = abs(v)
        if v > limit:  # 度分(ddmm.m)表記
            d = int(v // 100)
            v = d + (v - d * 100) / 60
        return round(sign * v, 4)

    try:
        now = time.time()
        seen_eid = set()
        for q in http_json(JMA_QUAKES)[:60]:
            m = re.match(r"([+-][\d.]+)([+-][\d.]+)", q.get("cod", ""))
            at = q.get("at") or q.get("rdt") or ""
            eid = q.get("eid", "")
            if not m or not at or eid in seen_eid:
                continue
            try:
                tq = datetime.fromisoformat(at).timestamp()
            except Exception:
                continue
            if now - tq > 86400:
                continue
            lat, lon = cod_deg(m[1], 90), cod_deg(m[2], 180)
            if abs(lat) > 90 or abs(lon) > 180:
                continue
            seen_eid.add(eid)
            out["quakes"].append({
                "eid": eid, "lat": lat, "lon": lon,
                "mag": q.get("mag", ""), "maxi": q.get("maxi", ""),
                # "time" は "HH:MM" の5文字で日付が落ちている。Web UI が使っているので温存する。
                # "at" は日付を含む ISO 8601 原文。上の fromisoformat() を通っているので
                # 解析可能であることが保証される。native クライアントは日跨ぎの同定に
                # "time" ではなく "at" を使うこと（"昨日23:50" と "今日23:50" が区別できないため）。
                "place": q.get("anm", ""), "time": at[11:16], "at": at,
            })
        out["quakes"] = out["quakes"][:10]
    except Exception as e:
        print(f"[jma] 地震取得失敗: {e}")
    _env_cache.update(ts=time.time(), data=out)
    return out
