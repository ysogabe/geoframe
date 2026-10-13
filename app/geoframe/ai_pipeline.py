"""新規写真をローカル品質解析からGemini選定まで増分処理する。

原則:
- SHA-256が同じ写真は1枚だけembedding対象にする。
- Gemini比較が完了した同一場面だけ表示候補を絞る。
- 未解析・失敗・別場面判定は全て表示可能にし、障害時に写真を消さない。
"""
from __future__ import annotations

import base64
import hashlib
import io
import json
import time
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

from PIL import Image, ImageOps

from .ai_search import (EMBED_DIM, EMBED_MODEL, AgentPlatformEmbeddingClient,
                        AiSearchUnavailable, cosine, pack_embedding,
                        unpack_embedding)
from .config import (AI_EMBED_PER_CYCLE, AI_JUDGE_PER_CYCLE, AI_POLL_S,
                     AI_SHORTLIST_MAX, AI_SIMILARITY, AI_TIME_WINDOW_MIN,
                     DATA_DIR, EMBED_BACKEND)
from .local_embedding import LocalEmbeddingClient
from .quality import Metrics, QualityPhoto, UnionFind, parse_taken_at, rank_group
from .store import (complete_photo_ai_group, dbm, dbq, dbx,
                    replace_photo_ai_groups, setting_set)


JUDGE_MODEL = "gemini-3.1-flash-lite"
JUDGE_VERSION = "judge-v3-horizon"
GROUP_ALGORITHM = (f"ai-scene-v1:t{AI_TIME_WINDOW_MIN:g}:"
                   f"s{AI_SIMILARITY:g}:max{AI_SHORTLIST_MAX}")
MAX_JUDGE_ATTEMPTS = 5

JUDGE_SCHEMA = {
    "type": "object",
    "properties": {
        "same_scene": {"type": "boolean"},
        "summary": {"type": "string"},
        "photos": {"type": "array", "items": {"type": "object", "properties": {
            "index": {"type": "integer"},
            "subject": {"type": "string", "enum": [
                "person", "dog", "cat", "person_and_pet", "landscape",
                "food", "vehicle", "other"]},
            "pet_state": {"type": "string", "enum": [
                "awake", "sleeping", "active", "uncertain", "not_applicable"]},
            "eyes": {"type": "string", "enum": [
                "open", "closed", "mixed", "not_visible", "uncertain", "not_applicable"]},
            "face_crop": {"type": "string", "enum": [
                "good", "intentional_closeup", "accidental_cut", "not_visible",
                "uncertain", "not_applicable"]},
            "horizon_status": {"type": "string", "enum": [
                "level", "intentional_tilt", "accidental_tilt",
                "uncertain", "not_applicable"]},
            "horizon_score": {"type": "integer", "minimum": 0, "maximum": 100},
            "subject_sharpness": {"type": "integer", "minimum": 0, "maximum": 100},
            "aesthetic_score": {"type": "integer", "minimum": 0, "maximum": 100},
            "keep_score": {"type": "integer", "minimum": 0, "maximum": 100},
            "recommended": {"type": "boolean"},
            "reasons": {"type": "array", "items": {"type": "string"}},
            "concerns": {"type": "array", "items": {"type": "string"}},
        }, "required": [
            "index", "subject", "pet_state", "eyes", "face_crop", "horizon_status",
            "horizon_score", "subject_sharpness", "aesthetic_score", "keep_score",
            "recommended", "reasons", "concerns"]}},
    },
    "required": ["same_scene", "summary", "photos"],
}

JUDGE_PROMPT = """あなたは個人写真の選定補助者です。続く画像を入力順（1始まり）で評価してください。
同じ短時間の撮影場面かを判断し、各写真について人物・犬・猫を区別してください。
人物または動物の目、顔切れ、主被写体のピント、構図と自然な魅力を比較してください。
犬猫が寝ている場合、閉眼を欠点にしないでください。意図的な顔アップも顔切れとして
減点しないでください。一般的な映えより、家庭の思い出写真として良い瞬間を優先します。
風景・建築・海・街並みなど水平線や建物の垂直線が構図上重要な写真では、カメラの
ロール方向の傾きを評価してください。自然に水平・垂直が取れていれば美的品質と採用度の
加点要素にし、意図した表現とは考えにくい傾きは減点してください。ただし明確な
ダッチアングル、対角線を活かした動的構図、意図的な見上げ・見下ろしは減点せず
intentional_tiltとしてください。遠近法による建物の収束を傾きと混同しないでください。
基準が無ければuncertain、水平評価が関係しないアップ等はnot_applicableにしてください。
horizon_scoreは水平良好90〜100、効果的な意図的傾き65〜85、意図しない傾き0〜60、
uncertain/not_applicableは中立の50としてください。
同一場面なら原則1〜2枚だけrecommended=trueにし、理由を具体的な日本語で返してください。
不確かな目や顔は推測せずuncertain/not_visibleを使ってください。"""


@dataclass
class PipelinePhoto:
    id: int
    display: str
    name: str
    md5: str | None
    mtime: float | None
    taken_at: str
    indexed_at: str
    sha256: str
    phash: str
    metrics: Metrics
    embedding: tuple[float, ...] | None
    captured: datetime | None
    local_score: float = 0.0


def _now() -> str:
    return datetime.now().astimezone().isoformat(timespec="seconds")


def _image_bytes(path: Path, max_px=768, quality=82) -> bytes:
    with Image.open(path) as image:
        image = ImageOps.exif_transpose(image)
        image.thumbnail((max_px, max_px), Image.Resampling.LANCZOS)
        if image.mode != "RGB":
            image = image.convert("RGB")
        output = io.BytesIO()
        image.save(output, "JPEG", quality=quality, optimize=True)
        return output.getvalue()


def _safe_path(display: str) -> Path:
    root = DATA_DIR.resolve()
    path = (root / display).resolve()
    path.relative_to(root)
    return path


def _load_photos() -> list[PipelinePhoto]:
    rows = dbq("""SELECT p.id,p.display,p.name,p.md5,p.mtime,p.taken_at,p.indexed_at,
                         f.content_sha256,f.phash,f.sharpness,f.luminance,
                         f.exposure_score,f.black_clip,f.white_clip,f.contrast,
                         a.embedding,a.embedding_dim,a.embedding_model,a.error ai_error
                  FROM photos p JOIN photo_quality_features f
                    ON f.photo_id=p.id AND f.display=p.display
                   AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                  LEFT JOIN photo_ai_features a
                    ON a.photo_id=p.id AND a.display=p.display
                   AND a.source_md5 IS p.md5 AND a.source_mtime IS p.mtime
                   AND a.content_sha256=f.content_sha256
                  WHERE p.display IS NOT NULL AND p.display!='' AND f.error=''
                    AND f.content_sha256 IS NOT NULL AND f.phash IS NOT NULL
                  ORDER BY p.id""")
    # 完全重複は最初の1枚だけを正本にする。同一バイト列なので画質差は無い。
    canonical = {}
    for row in rows:
        canonical.setdefault(row["content_sha256"], row)
    photos = []
    for row in canonical.values():
        embedding = None
        if (row["embedding_model"] == EMBED_MODEL and row["embedding_dim"] == EMBED_DIM
                and not row["ai_error"]):
            try:
                embedding = unpack_embedding(row["embedding"], EMBED_DIM)
            except (ValueError, TypeError):
                pass
        metrics = Metrics(row["sharpness"], row["luminance"], row["exposure_score"],
                          row["black_clip"], row["white_clip"], row["contrast"])
        taken_at = row["taken_at"] or ""
        photos.append(PipelinePhoto(
            row["id"], row["display"], row["name"], row["md5"], row["mtime"],
            taken_at, row["indexed_at"] or "", row["content_sha256"], row["phash"],
            metrics, embedding, parse_taken_at(taken_at)))
    return photos


def _remove_noncanonical_embeddings() -> int:
    """更新済み・削除済み・SHA完全重複の非代表embeddingを除去する。"""
    rows = dbq("""SELECT COUNT(*) c FROM photo_ai_features a
                  WHERE NOT EXISTS (
                    SELECT 1 FROM photos p JOIN photo_quality_features f
                      ON f.photo_id=p.id AND f.display=p.display
                     AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                    WHERE p.id=a.photo_id AND a.display=p.display
                      AND a.source_md5 IS p.md5 AND a.source_mtime IS p.mtime
                      AND a.content_sha256=f.content_sha256 AND f.error=''
                      AND p.id=(
                        SELECT MIN(p2.id) FROM photos p2 JOIN photo_quality_features f2
                          ON f2.photo_id=p2.id AND f2.display=p2.display
                         AND f2.source_md5 IS p2.md5 AND f2.source_mtime IS p2.mtime
                        WHERE f2.error='' AND f2.content_sha256=f.content_sha256))""")
    count = rows[0]["c"]
    if count:
        dbx("""DELETE FROM photo_ai_features WHERE NOT EXISTS (
              SELECT 1 FROM photos p JOIN photo_quality_features f
                ON f.photo_id=p.id AND f.display=p.display
               AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
              WHERE p.id=photo_ai_features.photo_id
                AND photo_ai_features.display=p.display
                AND photo_ai_features.source_md5 IS p.md5
                AND photo_ai_features.source_mtime IS p.mtime
                AND photo_ai_features.content_sha256=f.content_sha256 AND f.error=''
                AND p.id=(
                  SELECT MIN(p2.id) FROM photos p2 JOIN photo_quality_features f2
                    ON f2.photo_id=p2.id AND f2.display=p2.display
                   AND f2.source_md5 IS p2.md5 AND f2.source_mtime IS p2.mtime
                  WHERE f2.error='' AND f2.content_sha256=f.content_sha256))""")
    # どの写真にも使われなくなった中身のキャッシュも捨てる。
    dbx("""DELETE FROM photo_embeddings WHERE content_sha256 NOT IN (
             SELECT content_sha256 FROM photo_quality_features
             WHERE content_sha256 IS NOT NULL)""")
    return count


def _embed_pending(photos: list[PipelinePhoto], client, limit: int) -> int:
    pending = [photo for photo in photos if photo.embedding is None]
    pending.sort(key=lambda photo: (photo.indexed_at, photo.id), reverse=True)
    values = []
    analyzed_at = _now()
    cached = []
    computed = 0
    try:
        for photo in pending:
            # 同じ中身を前に同じモデルでembeddingしていれば、それを使う（切り替えて戻したとき、
            # prefill_embeddingsで先に作っておいたとき）。キャッシュからの分は上限に数えない。
            hit = dbq("""SELECT embedding FROM photo_embeddings
                         WHERE content_sha256=? AND embedding_model=? AND embedding_dim=?""",
                      (photo.sha256, EMBED_MODEL, EMBED_DIM))
            embedding = None
            if hit:
                try:
                    embedding = list(unpack_embedding(hit[0]["embedding"], EMBED_DIM))
                except (ValueError, TypeError):
                    embedding = None
            if embedding is None:
                if computed >= max(0, limit):
                    continue
                computed += 1
                payload = base64.b64encode(_image_bytes(_safe_path(photo.display))).decode("ascii")
                embedding = client.embed_part({"inlineData": {
                    "mimeType": "image/jpeg", "data": payload}})
                cached.append((photo.sha256, EMBED_MODEL, EMBED_DIM,
                               pack_embedding(embedding), analyzed_at))
            values.append((
                photo.id, photo.display, photo.md5, photo.mtime, photo.sha256,
                EMBED_MODEL, EMBED_DIM, pack_embedding(embedding),
                "", "", "", "", "", None, None, None, None, None,
                "[]", "[]", None, analyzed_at, "",
            ))
            photo.embedding = tuple(embedding)
    finally:
        # 途中でAPI障害になっても、それ以前に課金・取得済みのembeddingは失わない。
        if values:
            dbm("""INSERT INTO photo_ai_features(
            photo_id,display,source_md5,source_mtime,content_sha256,
            embedding_model,embedding_dim,embedding,subject,pet_state,eyes,face_crop,
            horizon_status,horizon_score,subject_sharpness,aesthetic_score,keep_score,
            recommended,reasons_json,concerns_json,judge_model,analyzed_at,error
          ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
          ON CONFLICT(photo_id) DO UPDATE SET
            display=excluded.display,source_md5=excluded.source_md5,
            source_mtime=excluded.source_mtime,content_sha256=excluded.content_sha256,
            embedding_model=excluded.embedding_model,embedding_dim=excluded.embedding_dim,
            embedding=excluded.embedding,
            -- 判定は写真の中身に対するもの。embeddingのモデルを替えただけ（中身が同じ）なら残し、
            -- 中身が変わったときだけ消す。SETの右辺は更新前の値を参照する。
            subject=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                         THEN subject ELSE '' END,
            pet_state=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                           THEN pet_state ELSE '' END,
            eyes=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                      THEN eyes ELSE '' END,
            face_crop=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                           THEN face_crop ELSE '' END,
            horizon_status=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                                THEN horizon_status ELSE '' END,
            horizon_score=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                               THEN horizon_score END,
            subject_sharpness=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                                   THEN subject_sharpness END,
            aesthetic_score=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                                 THEN aesthetic_score END,
            keep_score=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                            THEN keep_score END,
            recommended=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                             THEN recommended END,
            reasons_json=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                              THEN reasons_json ELSE '[]' END,
            concerns_json=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                               THEN concerns_json ELSE '[]' END,
            judge_model=CASE WHEN photo_ai_features.content_sha256=excluded.content_sha256
                             THEN judge_model END,
            analyzed_at=excluded.analyzed_at,error=''""",
            values)
        if cached:
            dbm("""INSERT OR REPLACE INTO photo_embeddings(
                     content_sha256,embedding_model,embedding_dim,embedding,analyzed_at)
                   VALUES(?,?,?,?,?)""", cached)
            print(f"[ai] embedding {len(values)}件（{EMBED_MODEL}、うちキャッシュ"
                  f"{len(values) - len(cached)}件）")
    return len(values)


def _rank_locally(bucket: list[PipelinePhoto]) -> None:
    quality_photos = []
    by_id = {}
    for photo in bucket:
        item = QualityPhoto(photo.id, photo.display, photo.md5, photo.mtime,
                            photo.taken_at, photo.sha256, int(photo.phash, 16), photo.metrics)
        quality_photos.append(item)
        by_id[item.id] = photo
    rank_group(quality_photos)
    for item in quality_photos:
        by_id[item.id].local_score = item.score


def build_scene_candidates(photos: list[PipelinePhoto]) -> list[dict]:
    embedded = [photo for photo in photos if photo.embedding is not None and photo.captured]
    embedded.sort(key=lambda photo: photo.captured)
    union = UnionFind(len(embedded))
    window = timedelta(minutes=AI_TIME_WINDOW_MIN)
    left = 0
    for right, current in enumerate(embedded):
        while left < right and current.captured - embedded[left].captured > window:
            left += 1
        for previous_index in range(left, right):
            previous = embedded[previous_index]
            if cosine(current.embedding, previous.embedding) >= AI_SIMILARITY:
                union.union(right, previous_index)
    buckets = {}
    for index, photo in enumerate(embedded):
        buckets.setdefault(union.find(index), []).append(photo)
    pending_times = [photo.captured for photo in photos
                     if photo.embedding is None and photo.captured]
    candidates = []
    for bucket in buckets.values():
        if len(bucket) < 2:
            continue
        lo = min(photo.captured for photo in bucket) - window
        hi = max(photo.captured for photo in bucket) + window
        if any(lo <= captured <= hi for captured in pending_times):
            continue
        _rank_locally(bucket)
        ranked = sorted(bucket, key=lambda photo: (-photo.local_score, photo.id))
        shortlist = ranked[:AI_SHORTLIST_MAX]
        fingerprint = "|".join(sorted(photo.sha256 for photo in shortlist))
        group_id = "scene-" + hashlib.sha256(
            f"{JUDGE_VERSION}|{fingerprint}".encode()).hexdigest()[:16]
        similarities = [cosine(left.embedding, right.embedding)
                        for index, left in enumerate(bucket)
                        for right in bucket[index + 1:]]
        candidates.append({
            "group_id": group_id, "photos": ranked, "shortlist": shortlist,
            "min_similarity": min(similarities), "max_similarity": max(similarities),
            "avg_similarity": sum(similarities) / len(similarities),
        })
    candidates.sort(key=lambda group: (-len(group["photos"]),
                                       -group["avg_similarity"], group["group_id"]))
    return candidates


def _rebuild_groups(photos: list[PipelinePhoto]) -> int:
    candidates = build_scene_candidates(photos)
    old_groups = {row["group_id"]: row for row in dbq("SELECT * FROM photo_ai_groups")}
    old_members = {}
    for row in dbq("SELECT * FROM photo_ai_group_members"):
        old_members.setdefault(row["group_id"], {})[row["photo_id"]] = row
    # 直前のPoC取込結果とshortlist集合が完全一致する場合は、判定を新group_idへ移植する。
    # 部分一致では誤適用しうるため、集合の完全一致だけに限定する。
    legacy_by_members = {}
    for old_gid, members in old_members.items():
        old = old_groups.get(old_gid)
        if old and old["status"] == "judged" and old["kind"] == "scene":
            legacy_by_members[frozenset(members)] = (old, members)
    analyzed_at = _now()
    group_rows, member_rows = [], []
    for candidate in candidates:
        gid = candidate["group_id"]
        old = old_groups.get(gid)
        preserve = old is not None and old["judge_version"] == JUDGE_VERSION
        shortlist_ids = [photo.id for photo in candidate["shortlist"]]
        seeded = None if preserve else legacy_by_members.get(frozenset(shortlist_ids))
        if seeded:
            old, seeded_members = seeded
            preserve = True
        status = old["status"] if preserve else "pending"
        same_scene = old["same_scene"] if preserve else 1
        summary = old["summary"] if preserve else "Gemini比較待ち"
        error = old["error"] if preserve else ""
        attempts = old["attempts"] if preserve else 0
        judge_model = old["judge_model"] if preserve else ""
        group_rows.append((
            gid, "scene", summary, candidate["min_similarity"],
            candidate["max_similarity"], candidate["avg_similarity"],
            GROUP_ALGORITHM, analyzed_at, status, same_scene, JUDGE_VERSION,
            judge_model, json.dumps(shortlist_ids), error, attempts,
        ))
        prior = seeded_members if seeded else old_members.get(gid, {})
        for rank, photo in enumerate(candidate["photos"], 1):
            if status == "judged":
                recommended = (prior[photo.id]["recommended"] if photo.id in prior
                               else int(not same_scene))
            else:
                recommended = 1
            member_rows.append((gid, photo.id, rank, recommended,
                                int(photo.id in shortlist_ids), photo.local_score))
    replace_photo_ai_groups(group_rows, member_rows)
    return len(candidates)


def _judge_group(group_id: str, client) -> None:
    group = dbq("SELECT * FROM photo_ai_groups WHERE group_id=?", (group_id,))[0]
    shortlist_ids = json.loads(group["shortlist_json"])
    placeholders = ",".join("?" for _ in shortlist_ids)
    rows = dbq(f"""SELECT p.id,p.display FROM photos p
                    WHERE p.id IN ({placeholders})""", tuple(shortlist_ids))
    by_id = {row["id"]: row for row in rows}
    if len(by_id) != len(shortlist_ids):
        raise RuntimeError("shortlistの写真が更新または削除されました")
    parts = [{"text": JUDGE_PROMPT}]
    for pid in shortlist_ids:
        encoded = base64.b64encode(_image_bytes(_safe_path(by_id[pid]["display"]))).decode("ascii")
        parts.append({"inlineData": {"mimeType": "image/jpeg", "data": encoded}})
    result = client.post(JUDGE_MODEL, "generateContent", {
        "contents": [{"role": "user", "parts": parts}],
        "generationConfig": {"temperature": 0.1, "responseMimeType": "application/json",
                             "responseJsonSchema": JUDGE_SCHEMA},
    }, timeout=120)
    try:
        value = json.loads(result["candidates"][0]["content"]["parts"][0]["text"])
        items = {item["index"]: item for item in value["photos"]}
        ordered = [items[index] for index in range(1, len(shortlist_ids) + 1)]
    except Exception as exc:
        raise RuntimeError("Gemini判定JSONが不完全です") from exc
    same_scene = bool(value.get("same_scene"))
    recommendations = {}
    members = dbq("SELECT photo_id,shortlisted,local_score FROM photo_ai_group_members WHERE group_id=?",
                  (group_id,))
    item_by_pid = dict(zip(shortlist_ids, ordered))
    if same_scene and not any(item.get("recommended") for item in ordered):
        best = max(shortlist_ids, key=lambda pid: item_by_pid[pid].get("keep_score", 0))
        item_by_pid[best]["recommended"] = True
    for member in members:
        recommendations[member["photo_id"]] = (
            bool(item_by_pid[member["photo_id"]].get("recommended"))
            if same_scene and member["photo_id"] in item_by_pid else not same_scene)
    analyzed_at = _now()
    feature_rows = []
    for pid in shortlist_ids:
        item = item_by_pid[pid]
        feature_rows.append((
            item["subject"], item["pet_state"], item["eyes"], item["face_crop"],
            item["horizon_status"], item["horizon_score"], item["subject_sharpness"],
            item["aesthetic_score"], item["keep_score"], int(recommendations[pid]),
            json.dumps(item.get("reasons") or [], ensure_ascii=False),
            json.dumps(item.get("concerns") or [], ensure_ascii=False),
            JUDGE_MODEL, analyzed_at, pid,
        ))
    complete_photo_ai_group(
        group_id, same_scene, value.get("summary") or "", recommendations,
        feature_rows, JUDGE_MODEL, JUDGE_VERSION, analyzed_at)


def _judge_pending(client, limit: int) -> int:
    rows = dbq("""SELECT group_id FROM photo_ai_groups
                  WHERE status IN ('pending','error') AND attempts<?
                  ORDER BY CASE status WHEN 'pending' THEN 0 ELSE 1 END, analyzed_at
                  LIMIT ?""", (MAX_JUDGE_ATTEMPTS, max(0, limit)))
    done = 0
    for row in rows:
        gid = row["group_id"]
        try:
            _judge_group(gid, client)
            done += 1
            print(f"[ai] Gemini比較完了: {gid}")
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"[:500]
            dbx("""UPDATE photo_ai_groups SET status='error',error=?,attempts=attempts+1,
                   analyzed_at=? WHERE group_id=?""", (error, _now(), gid))
            print(f"[ai] Gemini比較失敗（次回再試行）: {gid}: {error}")
            if isinstance(exc, AiSearchUnavailable):
                break
    return done


def ai_cycle(embed_limit=AI_EMBED_PER_CYCLE, judge_limit=AI_JUDGE_PER_CYCLE) -> dict:
    removed = _remove_noncanonical_embeddings()
    photos = _load_photos()
    # embeddingはバックエンド次第（localならこのサーバーのCPU）。比較評価は常にGemini。
    # localではGeminiの認証に失敗しても、embeddingと場面のまとめは先に進める。
    if EMBED_BACKEND == "local":
        embedded = _embed_pending(photos, LocalEmbeddingClient.shared(), embed_limit)
        groups = _rebuild_groups(photos)
        judge_client = AgentPlatformEmbeddingClient.from_environment()
    else:
        judge_client = AgentPlatformEmbeddingClient.from_environment()
        embedded = _embed_pending(photos, judge_client, embed_limit)
        groups = _rebuild_groups(photos)
    judged = _judge_pending(judge_client, judge_limit)
    pending_embeddings = sum(photo.embedding is None for photo in photos)
    pending_groups = dbq("""SELECT COUNT(*) c FROM photo_ai_groups
                            WHERE status!='judged'""")[0]["c"]
    result = {"embedded": embedded, "removed": removed,
              "pending_embeddings": pending_embeddings,
              "groups": groups, "judged": judged, "pending_groups": pending_groups,
              "at": _now()}
    setting_set("photo_ai_status", json.dumps(result, ensure_ascii=False))
    setting_set("photo_ai_last_error", None)
    return result


def ai_worker() -> None:
    time.sleep(10)  # HTTPとDrive同期を先に立ち上げる
    while True:
        try:
            result = ai_cycle()
            if result["pending_embeddings"] or result["pending_groups"]:
                print(f"[ai] 待ち embedding={result['pending_embeddings']} / "
                      f"group={result['pending_groups']}")
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"[:500]
            setting_set("photo_ai_last_error", error)
            print(f"[ai] パイプライン失敗（次回再試行）: {error}")
        time.sleep(AI_POLL_S)
