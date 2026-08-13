"""不動産情報ライブラリ（ハザード / AREA INTEL）
（server.py L869-L920 / L1054-L1149 より移動）"""
import json
import re
import time
import urllib.request
from datetime import datetime

from .config import HAZARD_APIS, HZ_DIR, MLIT_KEY, MLIT_URL
from .store import cache_get, cache_put
from .util import haversine_km, latlon_to_tile


def hazard_tile(xkt, z, x, y, extra=""):
    """不動産情報ライブラリのタイル1枚を取得（ファイル永続キャッシュ。エラーは空扱いでログ）
    extra: 追加クエリ（例: "year=2026" — XPT002地価公示は年指定必須）"""
    HZ_DIR.mkdir(exist_ok=True)
    suffix = ("_" + re.sub(r"[^A-Za-z0-9]", "", extra)) if extra else ""
    f = HZ_DIR / f"{xkt}_{z}_{x}_{y}{suffix}.json"
    if f.exists():
        try:
            return json.loads(f.read_text(encoding="utf-8"))
        except Exception:
            pass
    url = f"{MLIT_URL}/{xkt}?response_format=geojson&z={z}&x={x}&y={y}"
    if extra:
        url += "&" + extra
    empty = {"type": "FeatureCollection", "features": []}
    try:
        req = urllib.request.Request(url, headers={
            "Ocp-Apim-Subscription-Key": MLIT_KEY, "Accept": "*/*",
            "User-Agent": "GeoFrame/0.2"})
        with urllib.request.urlopen(req, timeout=15) as r:
            data = json.loads(r.read().decode("utf-8"))
        if not isinstance(data, dict) or "features" not in data:
            data = empty
    except Exception as e:
        print(f"[hazard] {xkt} z{z}/{x}/{y}: {e}")
        return empty   # エラーはキャッシュしない（次回再試行）
    f.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    time.sleep(0.15)   # 連続リクエスト抑制（不動産情報ライブラリの運用ポリシー配慮）
    return data


def _tile_grid(lat, lon, zoom, n):
    """中心点を含む n×n タイルの座標リスト"""
    x, y, xf, yf = latlon_to_tile(lat, lon, zoom)
    x0 = x - (n - 1) // 2 - (1 if n % 2 == 0 and xf <= 0.5 else 0)
    y0 = y - (n - 1) // 2 - (1 if n % 2 == 0 and yf <= 0.5 else 0)
    return [(x0 + i, y0 + j) for i in range(n) for j in range(n)]


def api_hazard(lat, lon):
    """震央周辺の防災レイヤをまとめて返す（APIごとに対応zoom/グリッドが異なる）"""
    if not MLIT_KEY:
        return {"ok": False, "reason": "no_key",
                "hint": "GEOFRAME_MLIT_KEY に不動産情報ライブラリAPIキーを設定すると有効になります"}
    layers = []
    for code, xkt, name, zoom, grid in HAZARD_APIS:
        feats = []
        for tx, ty in _tile_grid(lat, lon, zoom, grid):
            feats += hazard_tile(xkt, zoom, tx, ty).get("features", [])
        layers.append({"code": code, "name": name, "count": len(feats),
                       "geojson": {"type": "FeatureCollection", "features": feats}})
    return {"ok": True, "layers": layers}


# ============================================================
# AREA INTEL 実データ化（写真地点の用途地域・地価・浸水・人口）
# ============================================================
def _pip(lat, lon, geom):
    """point-in-polygon（レイキャスティング。外環のみ判定）"""
    def in_ring(ring):
        inside = False
        j = len(ring) - 1
        for i in range(len(ring)):
            xi, yi = ring[i][0], ring[i][1]
            xj, yj = ring[j][0], ring[j][1]
            if ((yi > lat) != (yj > lat)) and \
               (lon < (xj - xi) * (lat - yi) / (yj - yi + 1e-12) + xi):
                inside = not inside
            j = i
        return inside
    t = geom.get("type")
    if t == "Polygon":
        return in_ring(geom["coordinates"][0])
    if t == "MultiPolygon":
        return any(in_ring(p[0]) for p in geom["coordinates"])
    return False


def _find_poly(feats, lat, lon):
    for f in feats:
        try:
            if _pip(lat, lon, f.get("geometry") or {}):
                return f
        except Exception:
            continue
    return None


def api_intel(lat, lon):
    """写真地点の不動産情報ライブラリ実データ（永続キャッシュ）"""
    if not MLIT_KEY:
        return {"ok": False, "reason": "no_key"}
    key = f"intel2:{lat:.4f},{lon:.4f}"
    hit = cache_get(key)
    if isinstance(hit, dict):
        return {"ok": True, **hit}
    out = {}
    # 用途地域（XKT002, z13）— 容積率・建蔽率つき
    x, y, _, _ = latlon_to_tile(lat, lon, 13)
    f = _find_poly(hazard_tile("XKT002", 13, x, y).get("features", []), lat, lon)
    if f:
        p = f["properties"]
        out["youto"] = p.get("use_area_ja", "")
        out["ratio"] = f"{p.get('u_floor_area_ratio_ja', '')}/{p.get('u_building_coverage_ratio_ja', '')}".strip("/")
    else:
        out["youto"] = "用途地域外"
    # 洪水浸水想定（XKT026, z14）
    x, y, _, _ = latlon_to_tile(lat, lon, 14)
    f = _find_poly(hazard_tile("XKT026", 14, x, y).get("features", []), lat, lon)
    if f:
        p = f["properties"]
        # A31a_205=浸水深ランク, A31a_202=河川名（2026-07-29実測）
        ranks = {1: "0.5m未満", 2: "0.5〜3m", 3: "3〜5m", 4: "5〜10m", 5: "10〜20m", 6: "20m以上"}
        rank = p.get("A31a_205")
        label = ranks.get(int(rank)) if str(rank).isdigit() else None
        river = p.get("A31a_202", "")
        out["flood"] = (label or "区域内") + (f"({river})" if river else "")
    else:
        out["flood"] = "区域外"
    # 将来推計人口 2050（XKT013, z13）
    x, y, _, _ = latlon_to_tile(lat, lon, 13)
    f = _find_poly(hazard_tile("XKT013", 13, x, y).get("features", []), lat, lon)
    if f:
        v = f["properties"].get("PT00_2050")
        out["pop2050"] = round(v) if isinstance(v, (int, float)) else None
    # 地価公示ポイント（XPT002, z15の2x2タイルから最寄り。year必須 → 最新年から順に試す）
    pts = []
    this_year = datetime.now().year
    for yr in (this_year, this_year - 1):
        for tx, ty in _tile_grid(lat, lon, 15, 2):
            pts += hazard_tile("XPT002", 15, tx, ty, f"year={yr}").get("features", [])
        if pts:
            break
    best = None
    for f in pts:
        c = (f.get("geometry") or {}).get("coordinates") or []
        if len(c) < 2 or not isinstance(c[0], (int, float)):
            continue
        km = haversine_km(lat, lon, c[1], c[0])
        if best is None or km < best[0]:
            best = (km, f)
    if best:
        p = best[1]["properties"]
        price = next((p[k] for k in p
                      if "price" in k.lower() and p[k] and re.search(r"\d", str(p[k]))), None)
        if price is not None:
            out["price"] = str(price)
            out["price_m"] = round(best[0] * 1000)
    cache_put(key, out)
    return {"ok": True, **out}
