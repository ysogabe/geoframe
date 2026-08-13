"""写真マニフェスト・旅行クラスタ・統計
（server.py L748-L857 / L1341-L1378 / L1402 / L1406-L1410 より移動。
_last_trips の循環を避けるため api_stats・get_manifest_cached も同居させる）"""
import time
from datetime import datetime, timedelta
from pathlib import Path

from .store import dbq
from .util import haversine_km, t_epoch


# ============================================================
# API
# ============================================================
_last_trips = []

# 品質結果は「同じ原本・同じ表示画像」にだけ適用する。Drive更新後はmd5が、ローカル更新後は
# mtimeが変わるため、解析を再実行するまで自動的に未評価へ戻る。
QUALITY_JOIN = """LEFT JOIN photo_quality q
  ON q.photo_id=p.id AND q.display=p.display
 AND q.source_md5 IS p.md5 AND q.source_mtime IS p.mtime"""
QUALITY_COLUMNS = """q.score AS quality_score, q.group_id AS quality_group,
  q.group_kind AS quality_kind, q.rank_in_group AS quality_rank,
  q.recommended AS quality_recommended"""
QUALITY_VISIBLE = """(q.group_kind IS NULL OR q.group_kind!='exact' OR q.recommended=1)"""
AI_JOIN = """LEFT JOIN photo_ai_group_members aim ON aim.photo_id=p.id
LEFT JOIN photo_ai_groups aig ON aig.group_id=aim.group_id"""
AI_COLUMNS = """aig.group_id AS ai_group, aig.status AS ai_status,
  aig.same_scene AS ai_same_scene, aim.rank_in_group AS ai_rank,
  aim.recommended AS ai_recommended, aim.local_score AS ai_local_score"""
# 完成済みかつ同一場面と確認されたグループだけを絞る。favoriteは常に利用者の明示選択を優先。
AI_VISIBLE = """(aig.group_id IS NULL OR aig.status!='judged' OR aig.same_scene!=1
  OR aim.recommended=1 OR COALESCE(p.favorite,0)=1)"""


def _quality_fields(row):
    kind = row["quality_kind"] or ""
    return {
        "quality_score": row["quality_score"] if kind else None,
        "quality_group": row["quality_group"] if kind else None,
        "quality_kind": kind,
        "quality_rank": row["quality_rank"] if kind else None,
        "quality_recommended": bool(row["quality_recommended"]) if kind else False,
        "quality_role": ("recommended" if row["quality_recommended"] else "alternate") if kind else "",
    }


def _ai_fields(row):
    grouped = bool(row["ai_group"])
    return {
        "ai_group": row["ai_group"] or "",
        "ai_status": row["ai_status"] or "",
        "ai_same_scene": bool(row["ai_same_scene"]) if grouped else False,
        "ai_rank": row["ai_rank"] if grouped else None,
        "ai_recommended": bool(row["ai_recommended"]) if grouped else False,
        "ai_local_score": row["ai_local_score"] if grouped else None,
    }


def api_photos():
    global _last_trips
    rows = dbq(f"""SELECT p.*, {QUALITY_COLUMNS}, {AI_COLUMNS} FROM photos p
                  {QUALITY_JOIN}
                  {AI_JOIN}
                  WHERE p.lat IS NOT NULL AND p.display IS NOT NULL
                    AND COALESCE(p.hidden,0)=0 AND {QUALITY_VISIBLE} AND {AI_VISIBLE}
                  ORDER BY CASE WHEN taken_at='' THEN 1 ELSE 0 END, taken_at""")
    items = [{
        # id は再生キューの安定キー。ORDER BY は assign_trips が時系列前提のため変更しない
        "id": r["id"],
        "fetched_at": r["fetched_at"] or "",   # 新着ブースト用
        "fav": bool(r["favorite"]),
        "file": r["display"],
        # タイトル優先順: 手動/AI設定 → 地名（市区町村＋町名）→ ファイル名
        "title": r["title"] or (f"{r['city']} {r['place'] or ''}".strip() if r["city"] else "")
                 or Path(r["name"]).stem,
        "lat": r["lat"], "lon": r["lon"],
        "time": r["taken_at"],
        "place": r["place"] or "",
        "pref": r["pref"] or "",
        "city": r["city"] or "",
        "elev": r["elev"],
        "inferred": r["loc_source"] == "inferred",
        **_quality_fields(r),
        **_ai_fields(r),
    } for r in rows]
    _last_trips = assign_trips(items)   # 各itemに trip 番号も付与される
    return items


def assign_trips(items):
    """撮影時刻の間隔（48時間超）で旅行クラスタに分割し、各itemにtrip番号を付与"""
    groups, last_te = [], None
    for it in items:
        te = t_epoch(it["time"])
        if not groups or te is None or last_te is None or te - last_te > 48 * 3600:
            groups.append([])
        groups[-1].append(it)
        if te is not None:
            last_te = te
    trips = []
    for gid, its in enumerate(groups):
        cities = {}
        for it in its:
            it["trip"] = gid
            c = it["city"] or it["pref"]
            if c:
                cities[c] = cities.get(c, 0) + 1
        top = max(cities.items(), key=lambda x: x[1])[0] if cities else ""
        start = its[0]["time"][:10] if its[0]["time"] else ""
        end = its[-1]["time"][:10] if its[-1]["time"] else ""
        label = (start[:7].replace("-", ".") + " " if start else "") + (top + "の旅" if top else "JOURNEY")
        trips.append({
            "id": gid, "label": label, "count": len(its),
            "start": start, "end": end,
            "bounds": [min(i["lon"] for i in its), min(i["lat"] for i in its),
                       max(i["lon"] for i in its), max(i["lat"] for i in its)],
        })
    return trips


ONTHISDAY_WINDOW_D = 3     # 今日の前後何日までを「今日」とみなすか
ONTHISDAY_PER_YEAR = 3     # 1つの年から返す代表枚数
ONTHISDAY_MAX = 12         # 全体の上限


def api_onthisday(today=None):
    """「◯年前の今日」カット用。撮影月日が今日±3日（環状）で、撮影年が今年より前の写真を
    年ごとに代表数枚返す。月日の比較は実日付の加減算で行うので年跨ぎ（12/30 ⇔ 1/2）も自然に扱える。"""
    today = today or datetime.now().date()
    # 今日±N日の (MM-DD) → 距離（日数）。年跨ぎ・閏日は date 演算に任せる
    offsets = {}
    for d in range(-ONTHISDAY_WINDOW_D, ONTHISDAY_WINDOW_D + 1):
        key = (today + timedelta(days=d)).strftime("%m-%d")
        offsets[key] = min(abs(d), offsets.get(key, 99))
    rows = dbq(f"""SELECT p.*, {QUALITY_COLUMNS}, {AI_COLUMNS} FROM photos p
                  {QUALITY_JOIN}
                  {AI_JOIN}
                  WHERE p.lat IS NOT NULL AND p.lon IS NOT NULL AND p.display IS NOT NULL
                    AND COALESCE(p.hidden,0)=0 AND {QUALITY_VISIBLE} AND {AI_VISIBLE}
                    AND taken_at IS NOT NULL AND taken_at != ''
                    AND substr(taken_at,6,5) IN (%s)
                    AND CAST(substr(taken_at,1,4) AS INTEGER) < ?
                  ORDER BY taken_at""" % ",".join("?" * len(offsets)),
               tuple(offsets.keys()) + (today.year,))
    by_year = {}
    for r in rows:
        by_year.setdefault(int(r["taken_at"][:4]), []).append(r)
    items = []
    for year in sorted(by_year, reverse=True):     # 直近の年から
        # 代表選出: お気に入り優先 → 今日との日数差が小さい順 → 撮影時刻順
        picked = sorted(by_year[year],
                        key=lambda r: (0 if r["favorite"] else 1,
                                       0 if r["quality_recommended"] else 1,
                                       0 if r["ai_recommended"] else 1,
                                       offsets.get(r["taken_at"][5:10], 99),
                                       r["taken_at"]))[:ONTHISDAY_PER_YEAR]
        for r in picked:
            items.append({
                "id": r["id"],
                "file": r["display"],           # 画像URLは /api/photos と同じ形式
                "title": r["title"] or (f"{r['city']} {r['place'] or ''}".strip()
                                        if r["city"] else "") or Path(r["name"]).stem,
                "time": r["taken_at"],
                "taken_at": r["taken_at"],
                "lat": r["lat"], "lon": r["lon"],
                "place": r["place"] or "", "pref": r["pref"] or "", "city": r["city"] or "",
                "fav": bool(r["favorite"]),
                "years_ago": today.year - year,
                **_quality_fields(r),
                **_ai_fields(r),
            })
    return {"ok": True, "today": today.isoformat(), "items": items[:ONTHISDAY_MAX]}


def api_stats():
    """写真EXIFから生成する「自分統計」— すべて実データ"""
    rows = dbq("""SELECT taken_at, lat, lon, pref, city FROM photos
                  WHERE lat IS NOT NULL""")
    prefs, cities = {}, {}
    months = [0] * 12
    pts = []
    times = []
    for r in rows:
        if r["pref"]:
            prefs[r["pref"]] = prefs.get(r["pref"], 0) + 1
        if r["city"]:
            cities[r["city"]] = cities.get(r["city"], 0) + 1
        if r["taken_at"]:
            times.append(r["taken_at"])
            try:
                months[int(r["taken_at"][5:7]) - 1] += 1
            except Exception:
                pass
            te = t_epoch(r["taken_at"])
            if te:
                pts.append((te, r["lat"], r["lon"]))
    pts.sort()
    dist = sum(haversine_km(a[1], a[2], b[1], b[2]) for a, b in zip(pts, pts[1:]))
    inferred = dbq("SELECT COUNT(*) c FROM photos WHERE loc_source='inferred'")[0]["c"]
    pending = dbq("SELECT COUNT(*) c FROM photos WHERE lat IS NOT NULL AND display IS NULL")[0]["c"]
    return {
        "total": len(rows),
        "pref_count": len(prefs),
        "pref_list": sorted(prefs.keys()),
        "months": months,
        "dist_km": round(dist),
        "span": [min(times), max(times)] if times else ["", ""],
        "top_cities": sorted(cities.items(), key=lambda x: -x[1])[:5],
        "inferred": inferred,
        "pending": pending,
        "trips": (api_photos() and _last_trips),
    }


_manifest_cache = {"items": [], "ts": 0}


def get_manifest_cached():
    if time.time() - _manifest_cache["ts"] > 60:
        _manifest_cache["items"] = api_photos()
        _manifest_cache["ts"] = time.time()
    return _manifest_cache["items"]
