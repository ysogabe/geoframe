"""管理ページ /admin のAPI（Phase 4a/4b/4c）

家庭内LAN前提のため認証は無い。その代わり、状態を変えるAPIは全て
handlers.do_POST 側の関門（application/json必須＋Origin同一オリジン制限＋
サイズ上限）を通ったものだけがここに来る。このモジュールは値の検証と
DB更新に専念し、HTTPの作法は handlers.py 側に寄せている。
"""
import json
import os
import shutil
import time
from pathlib import Path

from PIL import Image

from .config import DATA_DIR, DERIV_DIR, TILE_DIR, TILE_CACHE_MAX_MB
from .ai_search import EMBED_MODEL
from .geo import infer_bucket
from .loc_suggest import loc_status
from .store import dbq, dbx
from .util import derivative_name, t_epoch


# ============================================================
# 状態サマリ
# ============================================================
def _count(sql, args=()):
    return dbq(f"SELECT COUNT(*) c FROM photos WHERE {sql}", args)[0]["c"]


def _group(col):
    """列の値ごとの件数（NULLは '' に寄せる）"""
    return {(r[col] if r[col] is not None else ""): r["c"]
            for r in dbq(f"SELECT {col}, COUNT(*) c FROM photos GROUP BY {col}")}


# ディスク使用量は全走査になるため60秒キャッシュする。
# ※ここで store.cache_put を使わないこと。あれは .cache.json への永続キャッシュで、
#   起動時に CACHE_LIVE_PREFIXES 以外のキーが破棄される（＝毎起動でゴミが増えるだけ）
_disk_cache = {"data": None, "ts": 0.0}
DISK_CACHE_TTL_S = 60


def _dir_usage(path: Path):
    """ディレクトリ配下のファイル数と合計バイト数（再帰）"""
    files = total = 0
    if not path.exists():
        return {"files": 0, "bytes": 0, "exists": False}
    for root, _dirs, names in os.walk(path):
        for nm in names:
            try:
                total += os.path.getsize(os.path.join(root, nm))
                files += 1
            except OSError:
                pass          # 走査中に消えたファイルは無視
    return {"files": files, "bytes": total, "exists": True}


def disk_info():
    if _disk_cache["data"] and time.time() - _disk_cache["ts"] < DISK_CACHE_TTL_S:
        return _disk_cache["data"]
    du = shutil.disk_usage(DATA_DIR)
    info = {
        "total_bytes": du.total, "used_bytes": du.used, "free_bytes": du.free,
        "used_pct": round(du.used / du.total * 100, 1) if du.total else 0,
        "dirs": {
            "derivatives": _dir_usage(DERIV_DIR),
            "tile_cache": _dir_usage(TILE_DIR),
        },
        "tile_cache_max_mb": TILE_CACHE_MAX_MB,
        "measured_at": time.strftime("%Y-%m-%d %H:%M:%S"),
    }
    _disk_cache.update(data=info, ts=time.time())
    return info


def api_admin_status():
    """同期状態・DB内訳・ディスク。
    ※api_photos() は呼ばない（全行スキャン＋旅行クラスタ計算が走るため）。
      cycle() が出しているのと同じ3つのCOUNTを直接引く"""
    ready = _count("display IS NOT NULL AND lat IS NOT NULL")
    pending = _count("lat IS NOT NULL AND display IS NULL")
    total = dbq("SELECT COUNT(*) c FROM photos")[0]["c"]

    # 推定の段階内訳（infer_dt_s から窓ラベルへ）
    buckets = {}
    for r in dbq("""SELECT infer_dt_s FROM photos
                    WHERE loc_source='inferred'"""):
        b = infer_bucket(r["infer_dt_s"]) or "未計測"
        buckets[b] = buckets.get(b, 0) + 1

    quality = dict(dbq("""SELECT COUNT(*) analyzed,
                      COALESCE(SUM(q.recommended),0) recommended,
                      COALESCE(SUM(q.group_kind='exact' AND q.recommended=0),0) exact_suppressed,
                      COALESCE(SUM(q.group_kind='similar' AND q.recommended=0),0) similar_alternates
                      FROM photo_quality q JOIN photos p
                        ON p.id=q.photo_id AND p.display=q.display
                       AND p.md5 IS q.source_md5 AND p.mtime IS q.source_mtime""")[0])
    coverage = dbq("""SELECT
                        COUNT(f.photo_id) features,
                        COUNT(*)-COUNT(f.photo_id) pending
                       FROM photos p LEFT JOIN photo_quality_features f
                         ON f.photo_id=p.id AND f.display=p.display
                        AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                       WHERE p.display IS NOT NULL AND p.display!=''""")[0]
    quality.update(dict(coverage))
    ai = dict(dbq("""SELECT COUNT(DISTINCT a.content_sha256) embedded,
                      COALESCE(SUM(a.recommended IS NOT NULL),0) detailed
                    FROM photo_ai_features a JOIN photos p
                      ON p.id=a.photo_id AND p.display=a.display
                     AND p.md5 IS a.source_md5 AND p.mtime IS a.source_mtime
                    WHERE a.error='' AND a.embedding_model=?""", (EMBED_MODEL,))[0])
    ai["eligible"] = dbq("""SELECT COUNT(DISTINCT f.content_sha256) c
                              FROM photos p JOIN photo_quality_features f
                                ON f.photo_id=p.id AND f.display=p.display
                               AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                              WHERE f.error='' AND f.content_sha256 IS NOT NULL""")[0]["c"]
    ai["pending"] = max(0, ai["eligible"] - ai["embedded"])
    ai["groups"] = dbq("SELECT COUNT(*) c FROM photo_ai_groups")[0]["c"]
    ai["group_pending"] = dbq(
        "SELECT COUNT(*) c FROM photo_ai_groups WHERE status!='judged'")[0]["c"]
    ai["suppressed"] = dbq("""SELECT COUNT(*) c FROM photo_ai_group_members m
                                JOIN photo_ai_groups g ON g.group_id=m.group_id
                                WHERE g.status='judged' AND g.same_scene=1
                                  AND m.recommended=0""")[0]["c"]

    return {
        "ok": True,
        # cycle() のログと同じ3値
        "sync": {"ready": ready, "pending": pending, "total": total},
        "db": {
            "total": total,
            "no_loc": _count("lat IS NULL"),
            "no_display": _count("display IS NULL"),
            "hidden": _count("COALESCE(hidden,0)=1"),
            "favorite": _count("COALESCE(favorite,0)=1"),
            "enrich_pending": _count("lat IS NOT NULL AND place IS NULL"),
            "no_time": _count("taken_at IS NULL OR taken_at=''"),
            "by_loc_source": _group("loc_source"),
            "by_source": _group("source"),
            "infer_buckets": buckets,
            "quality": quality,
            "ai": ai,
            "loc": loc_status(),
        },
        "disk": disk_info(),
        "server_time": time.strftime("%Y-%m-%d %H:%M:%S"),
    }


# ============================================================
# 写真一覧（ページング）
# ============================================================
PER_PAGE_MAX = 200
FILTERS = {
    "all": "1=1",
    "noloc": "lat IS NULL",
    "fav": "COALESCE(favorite,0)=1",
    "hidden": "COALESCE(hidden,0)=1",
    "inferred": "loc_source='inferred'",
    "manual": "loc_source='manual'",
    # 位置なしのうち、AIの撮影地候補がある写真（候補は確定するまで photos に書かない）
    "suggest": "lat IS NULL AND id IN (SELECT photo_id FROM photo_loc_suggestions)",
}


def _thumb_for(row):
    """一覧用サムネイルのURL。
    display があればそれ。無い場合でも、過去サイクルで生成された derivative が
    ディスクに残っていることがある（推定取り消し時に display=NULL にするだけで
    ファイルは消していないため）ので拾う。※ここでDriveから取りに行くことはしない
    （無認証GETで外部ダウンロードを誘発できてしまうため）"""
    if row["display"]:
        return "/" + row["display"]
    did = row["drive_id"]
    if did and (DERIV_DIR / f"{did}.jpg").exists():
        return f"/derivatives/{did}.jpg"
    return ""


def _time_anchors():
    """位置あり写真の (時刻, 地名) 一覧。位置なし写真の「近い時刻に居た場所」ヒント用"""
    out = []
    for r in dbq("""SELECT taken_at, lat, lon, place, pref, city FROM photos
                    WHERE lat IS NOT NULL AND taken_at != ''
                      AND loc_source IN ('exif','drive_meta')"""):
        te = t_epoch(r["taken_at"])
        if te is not None:
            out.append((te, r))
    out.sort(key=lambda x: x[0])
    return out


def api_admin_photos(page=1, per=50, filt="all"):
    # 未知の値は 'all' に落とす。応答にも解決後の名前を返す
    # （受け取った文字列をそのまま返すと、未検証の入力を画面に反射させることになる）
    filt = filt if filt in FILTERS else "all"
    where = FILTERS[filt]
    per = max(1, min(int(per), PER_PAGE_MAX))
    total = dbq(f"SELECT COUNT(*) c FROM photos WHERE {where}")[0]["c"]
    pages = max(1, -(-total // per))
    page = max(1, min(int(page), pages))
    rows = dbq(f"""SELECT * FROM photos WHERE {where}
                   ORDER BY CASE WHEN taken_at IS NULL OR taken_at='' THEN 1 ELSE 0 END,
                            taken_at, id
                   LIMIT ? OFFSET ?""", (per, (page - 1) * per))
    anchors = _time_anchors() if filt in ("noloc", "suggest") else []
    suggestions = _suggestions([r["id"] for r in rows if r["lat"] is None])
    items = []
    for r in rows:
        it = {
            "id": r["id"],
            "name": r["name"],
            "thumb": _thumb_for(r),
            "taken_at": r["taken_at"] or "",
            "lat": r["lat"], "lon": r["lon"],
            "loc_source": r["loc_source"] or "",
            "infer_dt_s": r["infer_dt_s"],
            "infer_bucket": (infer_bucket(r["infer_dt_s"])
                             if r["loc_source"] == "inferred" else ""),
            "place": r["place"] or "", "pref": r["pref"] or "", "city": r["city"] or "",
            "fav": bool(r["favorite"]), "hidden": bool(r["hidden"]),
            "rotation": r["rotation"] or 0,
            "source": r["source"],
        }
        if anchors:
            it["hint"] = _nearest_hint(anchors, r["taken_at"])
        if r["id"] in suggestions:
            it["suggest"] = suggestions[r["id"]]
        items.append(it)
    return {"ok": True, "page": page, "per": per, "pages": pages,
            "total": total, "filter": filt, "items": items}


def _suggestions(ids):
    """位置なし写真のAI撮影地候補（loc_suggest.py が作る。画面に出すだけでDBの位置にはしない）"""
    if not ids:
        return {}
    marks = ",".join("?" for _ in ids)
    out = {}
    for r in dbq(f"SELECT * FROM photo_loc_suggestions WHERE photo_id IN ({marks})", tuple(ids)):
        try:
            evidence = json.loads(r["evidence_json"])
        except ValueError:
            evidence = []
        out[r["photo_id"]] = {
            "place_name": r["place_name"], "day_place_name": r["day_place_name"],
            "scope": r["scope"], "precision": r["precision"], "confidence": r["confidence"],
            "evidence": evidence if isinstance(evidence, list) else [],
            "lat": r["lat"], "lon": r["lon"], "geocode_title": r["geocode_title"],
            "model": r["model"],
        }
    return out


HINT_MAX_DT_S = 7 * 86400      # これより離れた「近い写真」は手掛かりにならないので出さない


def _nearest_hint(anchors, taken_at):
    """撮影時刻が最も近い位置あり写真の場所（DBには書かない、表示ヒントのみ）。
    自動推定の最大窓（±12時間）を超えていても、地図の初期位置と
    『この頃どこに居たか』の手掛かりとしては使えるので7日まで出す"""
    te = t_epoch(taken_at or "")
    if te is None or not anchors:
        return None
    best = min(anchors, key=lambda a: abs(a[0] - te))
    dt = int(abs(best[0] - te))
    if dt > HINT_MAX_DT_S:
        return None
    r = best[1]
    label = " ".join(x for x in (r["pref"], r["city"], r["place"]) if x)
    return {"label": label, "lat": r["lat"], "lon": r["lon"],
            "taken_at": r["taken_at"], "dt_s": dt}


# ============================================================
# 更新API（POST）
# ============================================================
class BadRequest(Exception):
    """入力値が不正（handlers側で400にする）"""


class NotFound(Exception):
    """対象idが無い（handlers側で404にする）"""


def _req_id(payload):
    v = payload.get("id")
    # bool は int のサブクラス。True が id=1 として通らないよう明示的に弾く
    if isinstance(v, bool) or not isinstance(v, int):
        raise BadRequest("id は整数で指定してください")
    if v <= 0:
        raise BadRequest("id が範囲外です")
    return v


def _req_coord(payload, key, lo, hi):
    v = payload.get(key)
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise BadRequest(f"{key} は数値で指定してください")
    v = float(v)
    if not (lo <= v <= hi) or v != v:      # v != v は NaN 判定
        raise BadRequest(f"{key} が範囲外です（{lo}〜{hi}）")
    return v


def admin_setloc(payload):
    """手動で位置を指定する。loc_source='manual' は infer_locations() の
    再評価対象（loc_source IS NULL OR 'inferred'）から外れるので、
    次サイクルで推定に上書きされることも取り消されることもない。
    地名系の列はNULLに戻して enrich_rows()（lat IS NOT NULL AND place IS NULL）に
    拾わせる ＝ 明示的な再enrich呼び出しは不要"""
    pid = _req_id(payload)
    lat = _req_coord(payload, "lat", -90.0, 90.0)
    lon = _req_coord(payload, "lon", -180.0, 180.0)
    cur = dbx("""UPDATE photos SET lat=?, lon=?, loc_source='manual',
                 inferred_from=NULL, infer_dt_s=NULL,
                 place=NULL, elev=NULL, muni_cd=NULL, pref=NULL, city=NULL
                 WHERE id=?""", (lat, lon, pid))
    if cur.rowcount == 0:
        raise NotFound(f"id={pid} は存在しません")
    return {"ok": True, "id": pid, "lat": lat, "lon": lon, "loc_source": "manual"}


def admin_setflag(payload):
    """favorite / hidden のトグル。表示側はPhase1bで実装済みなので列を書くだけ"""
    pid = _req_id(payload)
    sets, args = [], []
    for key in ("favorite", "hidden"):
        if key in payload:
            v = payload[key]
            if not isinstance(v, bool):
                raise BadRequest(f"{key} は true/false で指定してください")
            sets.append(f"{key}=?")
            args.append(1 if v else 0)
    if not sets:
        raise BadRequest("favorite か hidden のいずれかを指定してください")
    cur = dbx(f"UPDATE photos SET {', '.join(sets)} WHERE id=?", tuple(args) + (pid,))
    if cur.rowcount == 0:
        raise NotFound(f"id={pid} は存在しません")
    r = dbq("SELECT favorite, hidden FROM photos WHERE id=?", (pid,))[0]
    return {"ok": True, "id": pid,
            "favorite": bool(r["favorite"]), "hidden": bool(r["hidden"])}


def admin_rotate(payload):
    """写真を時計回りに delta 度（90/180/270、-90も可）回して表示する。

    元の写真が横倒しのまま保存されている（EXIFの向きも無い）ものを手で直すため。
    今のderivativeを回して別名で保存し（URLを変えてブラウザのキャッシュを避ける）、
    display と rotation を書き換える。displayが変わるので、品質解析とAIのembedding・
    同一場面の比較は次のサイクルで作り直される。Driveから取り直すときも rotation を当てる"""
    pid = _req_id(payload)
    delta = payload.get("delta")
    if isinstance(delta, bool) or delta not in (90, 180, 270, -90):
        raise BadRequest("delta は 90 / 180 / 270 / -90 で指定してください")
    rows = dbq("SELECT source, drive_id, display, rotation FROM photos WHERE id=?", (pid,))
    if not rows:
        raise NotFound(f"id={pid} は存在しません")
    r = rows[0]
    if r["source"] != "drive" or not r["drive_id"]:
        raise BadRequest("回せるのはDriveの写真だけです")
    src = (DATA_DIR / r["display"]) if r["display"] else None
    if src is None or not src.is_file():
        raise BadRequest("表示用の画像がまだありません")
    rotation = ((r["rotation"] or 0) + delta) % 360
    dest = DERIV_DIR / derivative_name(r["drive_id"], rotation)
    tmp = dest.with_suffix(".part")
    with Image.open(src) as im:
        im.rotate(-delta, expand=True).save(tmp, "JPEG", quality=85)
    tmp.rename(dest)
    display = dest.relative_to(DATA_DIR).as_posix()
    dbx("UPDATE photos SET display=?, rotation=? WHERE id=?", (display, rotation, pid))
    if src.resolve() != dest.resolve():
        src.unlink(missing_ok=True)
    return {"ok": True, "id": pid, "rotation": rotation, "thumb": "/" + display}

