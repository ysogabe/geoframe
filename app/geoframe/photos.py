"""写真マニフェスト・旅行クラスタ"""
from pathlib import Path

from .store import dbq
from .util import t_epoch


# ============================================================
# API
# ============================================================
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
    assign_trips(items)   # 各itemに trip 番号を付与する
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

