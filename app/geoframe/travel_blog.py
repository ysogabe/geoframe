"""GeoFrame写真カタログを使う旅行ブログ下書き・エクスポート機能。

元写真とcatalog.dbは変更しない。レビュー状態は DATA_DIR/travel_blog/drafts/、
公開前の静的成果物は DATA_DIR/blog_exports/ に分離して保存する。
※旅程JSON（plans/）だけはgit管理のソースなので ROOT 側に置く。
"""
from __future__ import annotations

import copy
import html
import json
import os
import re
import shutil
import tempfile
import threading
from datetime import datetime
from pathlib import Path

from PIL import Image, ImageOps

from .config import DATA_DIR, ROOT
from .store import dbq


PLAN_DIR = ROOT / "travel_blog" / "plans"          # git管理の旅程JSON（ソース側）
DRAFT_DIR = DATA_DIR / "travel_blog" / "drafts"    # 利用者の編集状態（データ側）
EXPORT_DIR = DATA_DIR / "blog_exports"
TRIP_ID_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,79}$")
MAX_CANDIDATE_GROUPS = 36
DEFAULT_SELECTED_PER_DAY = 6
EXPORT_MAX_PX = 1800
# 章横断の総画像数上限。章数上限40・1章100枚は既にあるが総枚数の上限が無いと
# 最大4000枚が通過しうる（64KBのPOST 1本で数分〜十数分のCPU/IO占有）。
# 既存の章数・1章あたり上限と同じくハードコード（envで調整可能にはしない）。
MAX_TOTAL_EXPORT_IMAGES = 200


class BlogBadRequest(ValueError):
    pass


class BlogNotFound(LookupError):
    pass


class BlogExportBusy(RuntimeError):
    """他のエクスポートが実行中のため受け付けられない。"""


# エクスポートはプロセス全体で1本だけ走らせる。同時実行すると
# tempfile.mkdtemp→os.replace が同じ destination を奪い合うため。
_export_lock = threading.Lock()


def _trip_id(value):
    value = str(value or "")
    if not TRIP_ID_RE.fullmatch(value):
        raise BlogBadRequest("trip_idが不正です")
    return value


def _read_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BlogBadRequest(f"JSONを読み込めません: {path.name}") from exc


def list_plans():
    items = []
    if not PLAN_DIR.exists():
        return items
    for path in sorted(PLAN_DIR.glob("*.json")):
        try:
            plan = _read_json(path)
            trip_id = _trip_id(plan.get("trip_id"))
            dr = plan.get("date_range") or {}
            items.append({
                "trip_id": trip_id,
                "title": str(plan.get("title") or trip_id),
                "subtitle": str(plan.get("subtitle") or ""),
                "start": dr.get("start", ""),
                "end": dr.get("end", ""),
                "days": len(plan.get("segments") or []),
                "has_draft": (DRAFT_DIR / f"{trip_id}.json").exists(),
                "has_export": (EXPORT_DIR / trip_id / "index.html").exists(),
            })
        except (BlogBadRequest, TypeError):
            continue
    return items


def load_plan(trip_id):
    trip_id = _trip_id(trip_id)
    path = PLAN_DIR / f"{trip_id}.json"
    if not path.exists():
        raise BlogNotFound("旅程が見つかりません")
    plan = _read_json(path)
    if plan.get("trip_id") != trip_id or not isinstance(plan.get("segments"), list):
        raise BlogBadRequest("旅程JSONの形式が不正です")
    return plan


def api_blog_trips():
    return {"ok": True, "items": list_plans()}


def api_blog_trip(trip_id):
    return {"ok": True, "plan": load_plan(trip_id)}


def _photo_src(row):
    display = str(row["display"] or "").replace("\\", "/").lstrip("/")
    if not display or display.startswith(".") or "/../" in f"/{display}/":
        return ""
    return "/" + display


def _location_label(row, fallback):
    # 正確なGPSはブログAPIへ出さない。既存の粗い地名だけを返す。
    parts = [row["city"] or "", row["place"] or ""]
    label = " ".join(dict.fromkeys(p for p in parts if p)).strip()
    return label or fallback


def _base_score(row):
    score = 0.20
    reasons = ["旅程日の撮影時刻と一致"]
    if row["display"]:
        score += 0.25
        reasons.append("表示用画像を取得済み")
    if row["loc_source"] in ("exif", "drive_meta", "manual"):
        score += 0.25
        reasons.append("位置情報あり")
    elif row["loc_source"] == "inferred":
        score += 0.12
        reasons.append("周辺写真から位置を推定")
    if row["favorite"]:
        score += 0.25
        reasons.append("お気に入り")
    if row["title"] or row["ai_caption"]:
        score += 0.05
        reasons.append("説明メタデータあり")
    return min(1.0, score), reasons


def _evenly_spaced(items, limit):
    if len(items) <= limit:
        return list(items)
    if limit <= 1:
        return [items[0]]
    indexes = {round(i * (len(items) - 1) / (limit - 1)) for i in range(limit)}
    # お気に入りは時系列サンプリングから漏らさない。
    indexes.update(i for i, item in enumerate(items) if item["favorite"])
    return [item for i, item in enumerate(items) if i in indexes][:limit]


def _segment_candidates(segment):
    date = str(segment.get("date") or "")
    start = f"{date} {segment.get('start_local') or '00:00'}"
    end = f"{date} {segment.get('end_local') or '23:59'}"
    rows = dbq("""SELECT id, display, name, taken_at, loc_source, place, city,
                         title, ai_caption, favorite
                  FROM photos
                  WHERE COALESCE(hidden,0)=0 AND taken_at>=? AND taken_at<=?
                        AND display IS NOT NULL
                  ORDER BY taken_at, id""", (start, end))
    locations = segment.get("locations") or []
    fallback = "・".join(locations[:2])
    groups = []
    current_key = None
    for row in rows:
        # DBの時刻精度は分。同一分の連写を重複候補グループとして扱う。
        group_key = row["taken_at"] or f"photo-{row['id']}"
        if group_key != current_key:
            groups.append([])
            current_key = group_key
        score, reasons = _base_score(row)
        groups[-1].append({
            "photo_id": str(row["id"]),
            "src": _photo_src(row),
            "captured_at": row["taken_at"] or "",
            "location_label": _location_label(row, fallback),
            "timezone_source": row["loc_source"] or "unknown",
            "score": round(score, 3),
            "selection_reason": reasons,
            "favorite": bool(row["favorite"]),
            "alt": row["title"] or row["ai_caption"] or segment.get("title") or "旅の写真",
            "caption": row["ai_caption"] or "",
        })

    representatives = []
    for number, group in enumerate(groups, 1):
        group.sort(key=lambda p: (-p["score"], int(p["photo_id"])))
        rep = copy.deepcopy(group[0])
        rep["duplicate_group_id"] = f"{segment.get('id')}-g{number}"
        rep["duplicate_count"] = len(group)
        rep["alternates"] = [
            {k: photo[k] for k in ("photo_id", "src", "captured_at", "score", "alt")}
            for photo in group[1:4]
        ]
        representatives.append(rep)

    candidates = _evenly_spaced(representatives, MAX_CANDIDATE_GROUPS)
    selected_indexes = set()
    if candidates:
        wanted = min(DEFAULT_SELECTED_PER_DAY, len(candidates))
        selected_indexes = {
            round(i * (len(candidates) - 1) / max(1, wanted - 1)) for i in range(wanted)
        }
        selected_indexes.update(i for i, item in enumerate(candidates) if item["favorite"])
    for i, item in enumerate(candidates):
        item["selected"] = i in selected_indexes
    return {
        "segment_id": segment.get("id"),
        "date": date,
        "title": segment.get("title") or date,
        "timezone": segment.get("timezone") or "",
        "location_label": fallback,
        "photo_count": len(rows),
        "duplicate_groups": len(groups),
        "candidates": candidates,
    }


def api_blog_candidates(trip_id):
    plan = load_plan(trip_id)
    chapters = [_segment_candidates(segment) for segment in plan["segments"]]
    return {
        "ok": True,
        "trip_id": plan["trip_id"],
        "title": plan.get("title") or plan["trip_id"],
        "photo_count": sum(ch["photo_count"] for ch in chapters),
        "chapters": chapters,
        "privacy": {"exact_gps_in_response": False},
    }


def _outline_body(segment, photo_count):
    activities = segment.get("activities") or []
    outline = "、".join(
        f"{a.get('time', '時刻未確認')} {a.get('title', '')}" for a in activities
    )
    return (
        f"しおりでは「{segment.get('title', '')}」を予定。"
        f"写真DBにはこの日の写真が{photo_count}枚あります。"
        f"予定の流れは {outline}。実際に訪れた場所、時刻、感想は写真を見ながら追記してください。[要確認]"
    )


def generate_manifest(trip_id):
    plan = load_plan(trip_id)
    data = api_blog_candidates(trip_id)
    by_id = {s.get("id"): s for s in plan["segments"]}
    chapters = []
    for group in data["chapters"]:
        segment = by_id[group["segment_id"]]
        images = []
        for photo in group["candidates"]:
            if not photo["selected"]:
                continue
            images.append({
                "photo_id": photo["photo_id"],
                "src": photo["src"],
                "alt": photo["alt"],
                "caption": photo["caption"] or f"{group['title']}（{photo['captured_at'][11:16]}）[要確認]",
                "score": photo["score"],
                "selection_reason": photo["selection_reason"],
            })
        chapters.append({
            "id": group["segment_id"],
            "date": group["date"],
            "title": group["title"],
            "location_label": group["location_label"],
            "body": _outline_body(segment, group["photo_count"]),
            "images": images,
        })
    return {
        "schema_version": "1.0",
        "trip_id": plan["trip_id"],
        "title": plan.get("title") or plan["trip_id"],
        "subtitle": plan.get("subtitle") or "",
        "status": "draft",
        "summary": "旅程と写真の撮影時刻から生成した下書きです。公開前に事実と文章を確認してください。",
        "chapters": chapters,
    }


def _validate_manifest(value, expected_trip_id=None):
    if not isinstance(value, dict):
        raise BlogBadRequest("manifestはJSONオブジェクトにしてください")
    trip_id = _trip_id(value.get("trip_id"))
    if expected_trip_id and trip_id != expected_trip_id:
        raise BlogBadRequest("trip_idが一致しません")
    if value.get("status") not in ("draft", "reviewed", "published"):
        raise BlogBadRequest("statusが不正です")
    if not isinstance(value.get("chapters"), list) or len(value["chapters"]) > 40:
        raise BlogBadRequest("chaptersが不正です")
    clean = copy.deepcopy(value)
    clean["title"] = str(clean.get("title") or "")[:200]
    clean["subtitle"] = str(clean.get("subtitle") or "")[:300]
    clean["summary"] = str(clean.get("summary") or "")[:4000]
    total_images = 0
    for chapter in clean["chapters"]:
        if not isinstance(chapter, dict) or not isinstance(chapter.get("images"), list):
            raise BlogBadRequest("chapterの形式が不正です")
        chapter["id"] = str(chapter.get("id") or "")[:100]
        chapter["date"] = str(chapter.get("date") or "")[:10]
        chapter["title"] = str(chapter.get("title") or "")[:200]
        chapter["location_label"] = str(chapter.get("location_label") or "")[:300]
        chapter["body"] = str(chapter.get("body") or "")[:20000]
        if len(chapter["images"]) > 100:
            raise BlogBadRequest("1章あたりの画像が多すぎます")
        total_images += len(chapter["images"])
        if total_images > MAX_TOTAL_EXPORT_IMAGES:
            raise BlogBadRequest(f"画像の合計が多すぎます（上限{MAX_TOTAL_EXPORT_IMAGES}枚）")
        for image in chapter["images"]:
            if not isinstance(image, dict) or not str(image.get("photo_id") or "").isdigit():
                raise BlogBadRequest("画像のphoto_idが不正です")
            image["photo_id"] = str(image["photo_id"])
            image["alt"] = str(image.get("alt") or "旅の写真")[:500]
            image["caption"] = str(image.get("caption") or "")[:2000]
    return clean


def _draft_path(trip_id):
    return DRAFT_DIR / f"{_trip_id(trip_id)}.json"


def api_blog_draft(trip_id):
    path = _draft_path(trip_id)
    if path.exists():
        manifest = _validate_manifest(_read_json(path), _trip_id(trip_id))
        source = "saved"
    else:
        manifest = generate_manifest(trip_id)
        source = "generated"
    return {"ok": True, "source": source, "manifest": manifest}


def save_blog_draft(payload):
    trip_id = _trip_id(payload.get("trip_id"))
    load_plan(trip_id)
    manifest = _validate_manifest(payload.get("manifest"), trip_id)
    manifest["status"] = "draft" if manifest["status"] == "published" else manifest["status"]
    DRAFT_DIR.mkdir(parents=True, exist_ok=True)
    path = _draft_path(trip_id)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)
    return {"ok": True, "trip_id": trip_id, "saved": str(path.relative_to(DATA_DIR))}


def _photo_source(photo_id):
    rows = dbq("SELECT id, display FROM photos WHERE id=? AND COALESCE(hidden,0)=0", (int(photo_id),))
    if not rows or not rows[0]["display"]:
        raise BlogBadRequest(f"写真#{photo_id}の表示用画像がありません")
    path = (DATA_DIR / rows[0]["display"]).resolve()
    if DATA_DIR not in path.parents or not path.is_file():
        raise BlogBadRequest(f"写真#{photo_id}のファイルを確認できません")
    return path


def _export_image(source, target):
    with Image.open(source) as image:
        image = ImageOps.exif_transpose(image)
        image.thumbnail((EXPORT_MAX_PX, EXPORT_MAX_PX), Image.Resampling.LANCZOS)
        if image.mode != "RGB":
            image = image.convert("RGB")
        # exifを渡さず再エンコードし、GPSを含むメタデータを除去する。
        image.save(target, "JPEG", quality=88, optimize=True)


def _render_markdown(manifest):
    lines = [f"# {manifest['title']}", ""]
    if manifest.get("subtitle"):
        lines += [manifest["subtitle"], ""]
    if manifest.get("summary"):
        lines += [manifest["summary"], ""]
    for chapter in manifest["chapters"]:
        lines += [f"## {chapter['date']} {chapter['title']}", "", chapter["body"], ""]
        for image in chapter["images"]:
            lines += [f"![{image['alt']}]({image['src']})", ""]
            if image.get("caption"):
                lines += [f"_{image['caption']}_", ""]
    return "\n".join(lines).rstrip() + "\n"


def _paragraphs(text):
    return "".join(f"<p>{html.escape(p)}</p>" for p in str(text).split("\n") if p.strip())


def _render_html(manifest):
    chapters = []
    for chapter in manifest["chapters"]:
        figures = "".join(
            "<figure><img loading=\"lazy\" src=\"{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>".format(
                html.escape(image["src"], quote=True),
                html.escape(image["alt"], quote=True),
                html.escape(image.get("caption") or ""),
            ) for image in chapter["images"]
        )
        chapters.append(
            f"<article><div class=\"eyebrow\">{html.escape(chapter['date'])} · "
            f"{html.escape(chapter.get('location_label') or '')}</div>"
            f"<h2>{html.escape(chapter['title'])}</h2>{_paragraphs(chapter['body'])}"
            f"<div class=\"gallery\">{figures}</div></article>"
        )
    return """<!doctype html><html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title>
<style>
:root{{--paper:#f6f0e4;--ink:#17211b;--rust:#a4422d;--sage:#4b6958}}*{{box-sizing:border-box}}
body{{margin:0;background:var(--paper);color:var(--ink);font-family:"Hiragino Mincho ProN",serif;line-height:1.85}}
header{{min-height:58vh;display:grid;place-content:center;padding:10vw;background:linear-gradient(135deg,#17352e,#486350);color:#fff}}
header div{{max-width:900px}}h1{{font-size:clamp(2.6rem,7vw,6.8rem);line-height:1.02;margin:.2em 0}}header p{{font-size:1.1rem}}
main{{width:min(1120px,92vw);margin:auto}}article{{padding:7rem 0;border-bottom:1px solid #c9bda7}}article h2{{font-size:clamp(2rem,4vw,4rem);line-height:1.2;margin:.2em 0 1rem}}
.eyebrow{{color:var(--rust);font:700 .78rem sans-serif;letter-spacing:.14em;text-transform:uppercase}}
.gallery{{columns:2 320px;column-gap:1.25rem;margin-top:2.5rem}}figure{{break-inside:avoid;margin:0 0 1.25rem;background:#fff;padding:.55rem;box-shadow:0 8px 28px #17211b18}}
img{{display:block;width:100%;height:auto}}figcaption{{padding:.65rem .35rem .35rem;font:400 .82rem/1.55 sans-serif;color:#435048}}
footer{{padding:4rem;text-align:center;font:600 .75rem sans-serif;letter-spacing:.14em;color:var(--sage)}}
</style></head><body><header><div><div>GEOFRAME TRAVEL JOURNAL</div><h1>{title}</h1><p>{subtitle}</p></div></header>
<main><section>{summary}</section>{chapters}</main><footer>EXPORTED FROM GEOFRAME · REVIEW BEFORE PUBLISHING</footer></body></html>
""".format(
        title=html.escape(manifest["title"]),
        subtitle=html.escape(manifest.get("subtitle") or ""),
        summary=_paragraphs(manifest.get("summary") or ""),
        chapters="".join(chapters),
    )


def export_blog(payload):
    trip_id = _trip_id(payload.get("trip_id"))
    load_plan(trip_id)
    manifest = _validate_manifest(payload.get("manifest"), trip_id)
    selected = sum(len(ch["images"]) for ch in manifest["chapters"])
    if payload.get("dry_run"):
        return {"ok": True, "dry_run": True, "trip_id": trip_id, "images": selected}

    # ファイルI/Oを伴う本処理はプロセス全体で直列化する（dry_runはここより手前で返すので
    # 見積もりだけの呼び出しは待たされない）。解放漏れを防ぐため必ずfinallyで返す。
    if not _export_lock.acquire(blocking=False):
        raise BlogExportBusy("他のエクスポート処理が実行中です。しばらくしてから再試行してください")
    try:
        EXPORT_DIR.mkdir(parents=True, exist_ok=True)
        staging = Path(tempfile.mkdtemp(prefix=f".{trip_id}-", dir=EXPORT_DIR))
        try:
            image_dir = staging / "images"
            image_dir.mkdir()
            exported = copy.deepcopy(manifest)
            for chapter in exported["chapters"]:
                for image in chapter["images"]:
                    name = f"photo-{int(image['photo_id'])}.jpg"
                    target = image_dir / name
                    if not target.exists():
                        _export_image(_photo_source(image["photo_id"]), target)
                    image["src"] = f"images/{name}"
            exported["status"] = "reviewed"
            (staging / "blog-manifest.json").write_text(
                json.dumps(exported, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            (staging / "index.md").write_text(_render_markdown(exported), encoding="utf-8")
            (staging / "index.html").write_text(_render_html(exported), encoding="utf-8")

            destination = EXPORT_DIR / trip_id
            backup = EXPORT_DIR / f".{trip_id}.previous"
            if backup.exists():
                shutil.rmtree(backup)
            if destination.exists():
                os.replace(destination, backup)
            os.replace(staging, destination)
            if backup.exists():
                shutil.rmtree(backup)
        except Exception:
            if staging.exists():
                shutil.rmtree(staging)
            raise
        return {
            "ok": True,
            "trip_id": trip_id,
            "images": selected,
            "path": str((EXPORT_DIR / trip_id).relative_to(DATA_DIR)),
            "url": f"/blog_exports/{trip_id}/index.html",
        }
    finally:
        _export_lock.release()
