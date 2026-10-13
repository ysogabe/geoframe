"""GeoFrame管理画面向けのマルチモーダル写真検索API。

画像embeddingとGemini比較評価はSQLiteの派生表から読み、検索語だけを
embeddingする（GEOFRAME_EMBED_BACKEND=geminiならAgent PlatformのGemini Embedding 2、
localならこのサーバーのembeddinggemma-2）。画像ファイル自体は検索時に送信しない。
"""
from __future__ import annotations

import json
import math
import os
import struct
import threading
import time
import urllib.error
import urllib.request
from collections import OrderedDict, deque

from .config import (AI_ENABLED, AI_SEARCH_PER_MIN, AI_VISUAL_SIMILARITY, DRIVE_KEY,
                     EMBED_BACKEND, ROOT)
from .local_embedding import LOCAL_EMBED_MODEL, LocalEmbeddingClient
from .store import dbq, setting_get


GEMINI_EMBED_MODEL = "gemini-embedding-2"
# 今使うembeddingのモデル名。photo_ai_features.embedding_modelがこれと違う行は
# 再embeddingの対象で、検索にも使わない（別モデルのベクトルとは比べられない）。
EMBED_MODEL = LOCAL_EMBED_MODEL if EMBED_BACKEND == "local" else GEMINI_EMBED_MODEL
EMBED_DIM = 768
MAX_QUERY_CHARS = 300
MAX_GROUPS = 30
# 0.82の場面グループは連結成分なので、大きなイベントでは幅広い構図を含む。
# UIでは0.90以上（embeddinggemma-2では0.91以上）を「同じ構図」として再分類し、
# 代表写真だけに畳めるようにする。
VISUAL_CLUSTER_SIMILARITY = AI_VISUAL_SIMILARITY
_query_cache: OrderedDict[str, list[float]] = OrderedDict()
_query_lock = threading.Lock()
_client = None


class AiSearchUnavailable(RuntimeError):
    """認証・外部API障害など、一時的に自然言語検索を実行できない。"""


class AiSearchRateLimited(RuntimeError):
    """流量上限に達したため一時的に検索を受け付けない。"""


class AiSearchDisabled(AiSearchUnavailable):
    """AI機能自体が無効化されている（AiSearchUnavailableのサブクラス。
    handlers.py側でexcept節を書き忘れても親クラスの節で自動的に503に落ちるフェイルセーフ）。"""


def pack_embedding(values: list[float]) -> bytes:
    return struct.pack(f"<{len(values)}f", *values)


def unpack_embedding(blob: bytes, dimension: int) -> tuple[float, ...]:
    if not blob or len(blob) != dimension * 4:
        raise ValueError("invalid embedding blob")
    return struct.unpack(f"<{dimension}f", blob)


def cosine(left, right) -> float:
    if len(left) != len(right):
        return 0.0
    dot = sum(a * b for a, b in zip(left, right))
    ln = math.sqrt(sum(v * v for v in left))
    rn = math.sqrt(sum(v * v for v in right))
    return dot / (ln * rn) if ln and rn else 0.0


class AgentPlatformEmbeddingClient:
    def __init__(self, project: str, location: str, credentials):
        self.project = project
        self.location = location
        self.credentials = credentials

    @classmethod
    def from_environment(cls):
        # サーバー起動自体はgoogle-auth無しでも可能にするため遅延importする。
        try:
            import google.auth
            from google.oauth2 import service_account
        except ImportError as exc:
            raise AiSearchUnavailable("google-authがインストールされていません") from exc

        scopes = ["https://www.googleapis.com/auth/cloud-platform"]
        configured = os.environ.get("GOOGLE_APPLICATION_CREDENTIALS", "").strip()
        credential_path = DRIVE_KEY if not configured else __import__("pathlib").Path(configured)
        if not credential_path.is_absolute():
            credential_path = (ROOT / credential_path).resolve()
        if credential_path.is_file():
            credentials = service_account.Credentials.from_service_account_file(
                credential_path, scopes=scopes)
            detected_project = getattr(credentials, "project_id", None)
        else:
            credentials, detected_project = google.auth.default(scopes=scopes)
        project = (os.environ.get("GOOGLE_CLOUD_PROJECT") or
                   os.environ.get("GOOGLE_CLOUD_PROJECT_ID") or detected_project)
        if not project:
            raise AiSearchUnavailable("Google Cloudのproject IDを解決できません")
        return cls(project, os.environ.get("GOOGLE_CLOUD_LOCATION", "us"), credentials)

    def post(self, model: str, method: str, payload: dict, timeout=60) -> dict:
        from google.auth.transport.requests import Request as AuthRequest

        if not self.credentials.valid or self.credentials.expired:
            try:
                self.credentials.refresh(AuthRequest())
            except Exception as exc:
                raise AiSearchUnavailable(f"Google Cloud認証に失敗しました: {exc}") from exc
        url = (f"https://aiplatform.{self.location}.rep.googleapis.com/v1/projects/"
               f"{self.project}/locations/{self.location}/publishers/google/models/"
               f"{model}:{method}")
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        request = urllib.request.Request(url, data=body, method="POST", headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {self.credentials.token}",
        })
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")[:300]
            raise AiSearchUnavailable(f"Agent Platform HTTP {exc.code}: {detail}") from exc
        except (urllib.error.URLError, TimeoutError) as exc:
            raise AiSearchUnavailable(f"Agent Platformへの接続に失敗しました: {exc}") from exc
    def embed_part(self, part: dict) -> list[float]:
        result = self.post(GEMINI_EMBED_MODEL, "embedContent", {
            "content": {"parts": [part]},
            "outputDimensionality": EMBED_DIM,
        })
        embeddings = result.get("embeddings") or (
            [result["embedding"]] if result.get("embedding") else [])
        values = embeddings[0].get("values", []) if embeddings else []
        if len(values) != EMBED_DIM:
            raise AiSearchUnavailable("embedding応答の次元数が不正です")
        return values

    def embed_text(self, text: str) -> list[float]:
        return self.embed_part({"text": text})


def embedding_client():
    """今のバックエンドのembeddingクライアント（画像と検索語の両方に使う）。"""
    if EMBED_BACKEND == "local":
        return LocalEmbeddingClient.shared()
    return AgentPlatformEmbeddingClient.from_environment()


def _embed_query(query: str) -> list[float]:
    """検索語のembeddingを取得する（キャッシュ付き）。
    ※lockはキャッシュの読み書きだけに使い、クライアント構築（credentials読込）と
      embed_text（外部HTTP）は必ずlockの外で行う。握ったままI/Oすると1件の遅い
      呼び出しで他の全検索リクエストが直列に足止めされるため。
    ※同時キャッシュミス時は複数スレッドがembed_textを重複して呼びうるが、
      同一クエリは決定的に同一結果を返すため冪等で実害がない。"""
    global _client
    # モデルを切り替えたときに別モデルのベクトルを返さないよう、モデル名もキーに入れる
    key = EMBED_MODEL + "\0" + " ".join(query.casefold().split())
    with _query_lock:
        cached = _query_cache.get(key)
        if cached is not None:
            _query_cache.move_to_end(key)
            return cached
        client = _client
    if client is None:
        client = embedding_client()
        with _query_lock:
            if _client is None:
                _client = client
            else:
                client = _client
    values = client.embed_text(query)
    with _query_lock:
        _query_cache[key] = values
        _query_cache.move_to_end(key)
        while len(_query_cache) > 64:
            _query_cache.popitem(last=False)
    return values


# 検索リクエストのスライディング窓（直近60秒の受付時刻）。tiles.py と同型。
_search_win = deque()
_search_lock = threading.Lock()
_search_throttled = False    # 抑制中フラグ。開始時と解除時に1行ずつだけログを出すために持つ


def _search_rate_limit_allowed():
    """検索を1件受け付けてよいか（直近60秒のスライディング窓）
    ※判定と記録を同一クリティカルセクションで行わないと、同時到着が全部ゲートを通り抜ける。
    ※tiles.pyはtime.time()だがここはtime.monotonic()を使う。ウォールクロックはNTP補正で
      巻き戻ると窓が詰まったまま戻らなくなるため意図的に差異化している。"""
    global _search_throttled
    now = time.monotonic()
    with _search_lock:
        while _search_win and now - _search_win[0] >= 60.0:
            _search_win.popleft()
        if len(_search_win) >= AI_SEARCH_PER_MIN:
            if not _search_throttled:
                _search_throttled = True
                print(f"[ai-search] 抑制 開始: 直近60秒で{AI_SEARCH_PER_MIN}件に到達"
                      f"（GEOFRAME_AI_SEARCH_PER_MIN で調整可）")
            return False
        if _search_throttled:
            _search_throttled = False
            print("[ai-search] 抑制 解除")
        _search_win.append(now)
        return True


def _valid_rows(include_hidden: bool):
    hidden_sql = "" if include_hidden else "AND COALESCE(p.hidden,0)=0"
    return dbq(f"""SELECT p.id,p.name,p.display,p.taken_at,p.place,p.pref,p.city,p.lat,p.lon,
                         COALESCE(p.favorite,0) favorite,COALESCE(p.hidden,0) hidden,
                         a.embedding,a.embedding_dim,a.embedding_model,a.subject,a.pet_state,
                         a.eyes,a.face_crop,a.horizon_status,a.horizon_score,
                         a.subject_sharpness,a.aesthetic_score,
                         a.keep_score,a.recommended,a.reasons_json,a.concerns_json,
                         gm.group_id ai_group_id,
                         q.group_id quality_group_id,q.group_kind quality_group_kind,
                         q.recommended quality_recommended,q.score quality_score
                  FROM photo_ai_features a JOIN photos p
                    ON p.id=a.photo_id AND p.display=a.display
                   AND p.md5 IS a.source_md5 AND p.mtime IS a.source_mtime
                  LEFT JOIN photo_ai_group_members gm ON gm.photo_id=p.id
                  LEFT JOIN photo_quality q
                    ON q.photo_id=p.id AND q.display=p.display
                   AND p.md5 IS q.source_md5 AND p.mtime IS q.source_mtime
                  WHERE a.error='' AND a.embedding_model=? {hidden_sql}
                    AND NOT (COALESCE(q.group_kind,'')='exact' AND q.recommended=0)""",
               (EMBED_MODEL,))


def api_ai_status():
    row = dbq("""SELECT COUNT(*) rows,
                    COUNT(DISTINCT a.content_sha256) total,
                    COALESCE(SUM(a.recommended IS NOT NULL),0) judged,
                    COALESCE(SUM(a.recommended=1),0) recommended
                 FROM photo_ai_features a JOIN photos p
                   ON p.id=a.photo_id AND p.display=a.display
                  AND p.md5 IS a.source_md5 AND p.mtime IS a.source_mtime
                 WHERE a.error='' AND a.embedding_model=?""", (EMBED_MODEL,))[0]
    # 切り替え直後は前のモデルの行が残る。検索と場面のまとめには使わず、順に作り直す。
    other_model = dbq("""SELECT COUNT(DISTINCT content_sha256) c FROM photo_ai_features
                         WHERE error='' AND embedding_model!=?""", (EMBED_MODEL,))[0]["c"]
    group_status = {row["status"]: row["c"] for row in dbq(
        "SELECT status,COUNT(*) c FROM photo_ai_groups GROUP BY status")}
    groups = sum(group_status.values())
    eligible = dbq("""SELECT COUNT(DISTINCT f.content_sha256) c
                       FROM photos p JOIN photo_quality_features f
                         ON f.photo_id=p.id AND f.display=p.display
                        AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
                       WHERE p.display IS NOT NULL AND f.error=''
                         AND f.content_sha256 IS NOT NULL""")[0]["c"]
    try:
        pipeline = json.loads(setting_get("photo_ai_status", "{}"))
    except json.JSONDecodeError:
        pipeline = {}
    return {"ok": True, "photos": row["total"], "judged": row["judged"],
            "recommended": row["recommended"], "groups": groups,
            "eligible": eligible, "pending_embeddings": max(0, eligible - row["total"]),
            "group_status": group_status, "pipeline": pipeline,
            "embedding_model": EMBED_MODEL, "embedding_backend": EMBED_BACKEND,
            "other_model_embeddings": other_model}


def _json_list(raw):
    try:
        value = json.loads(raw or "[]")
        return value if isinstance(value, list) else []
    except (TypeError, json.JSONDecodeError):
        return []


def _group_key(row):
    if row["ai_group_id"]:
        return row["ai_group_id"]
    if row["quality_group_kind"] == "similar":
        return f"local-similar-{row['quality_group_id']}"
    return f"photo-{row['id']}"


def _visual_clusters(members):
    """品質順の先頭を代表に、embeddingが近い写真を同じ構図へまとめる。"""
    representatives = []
    assignments = {}
    cluster_sizes = []
    for row in members:
        try:
            embedding = unpack_embedding(row["embedding"], row["embedding_dim"])
        except (ValueError, TypeError, struct.error):
            embedding = None
        best_similarity, best_index = 0.0, -1
        if embedding is not None:
            for index, representative in enumerate(representatives):
                if representative is None:
                    continue
                similarity = cosine(embedding, representative)
                if similarity > best_similarity:
                    best_similarity, best_index = similarity, index
        if best_index < 0 or best_similarity < VISUAL_CLUSTER_SIMILARITY:
            best_index = len(representatives)
            representatives.append(embedding)
            cluster_sizes.append(0)
            best_similarity = 1.0
        cluster_sizes[best_index] += 1
        assignments[row["id"]] = {
            "visual_cluster": best_index + 1,
            "visual_similarity": round(float(best_similarity), 4),
            "visual_representative": cluster_sizes[best_index] == 1,
        }
    for value in assignments.values():
        value["visual_cluster_size"] = cluster_sizes[value["visual_cluster"] - 1]
    return assignments, len(representatives)


def _photo_json(row, similarity, visual=None):
    visual = visual or {}
    return {
        "id": row["id"], "name": row["name"],
        "thumb": "/" + row["display"].lstrip("/"),
        "taken_at": row["taken_at"] or "", "place": row["place"] or "",
        "pref": row["pref"] or "", "city": row["city"] or "",
        "lat": row["lat"], "lon": row["lon"],
        "favorite": bool(row["favorite"]), "hidden": bool(row["hidden"]),
        "similarity": round(float(similarity), 4),
        "visual_cluster": visual.get("visual_cluster", 1),
        "visual_similarity": visual.get("visual_similarity", 1.0),
        "visual_representative": visual.get("visual_representative", True),
        "visual_cluster_size": visual.get("visual_cluster_size", 1),
        "quality_group_kind": row["quality_group_kind"] or "",
        "quality_group_id": row["quality_group_id"],
        "quality_recommended": (None if row["quality_recommended"] is None
                                else bool(row["quality_recommended"])),
        "quality_score": row["quality_score"],
        "subject": row["subject"] or "", "pet_state": row["pet_state"] or "",
        "eyes": row["eyes"] or "", "face_crop": row["face_crop"] or "",
        "horizon_status": row["horizon_status"] or "",
        "horizon_score": row["horizon_score"],
        "subject_sharpness": row["subject_sharpness"],
        "aesthetic_score": row["aesthetic_score"], "keep_score": row["keep_score"],
        "recommended": None if row["recommended"] is None else bool(row["recommended"]),
        "reasons": _json_list(row["reasons_json"]),
        "concerns": _json_list(row["concerns_json"]),
    }


def api_ai_search(query: str, group_limit=12, include_hidden=False):
    # 重いDBフェッチ（全embedding行の3テーブルJOIN）へ入る前に落とす。
    if not AI_ENABLED:
        raise AiSearchDisabled("GEOFRAME_AI_ENABLED=0 のためAI検索は無効化されています")
    if not _search_rate_limit_allowed():
        raise AiSearchRateLimited(
            f"検索リクエストが多すぎます。しばらくしてから再試行してください（上限: 毎分{AI_SEARCH_PER_MIN}件）")
    if not isinstance(query, str):
        raise ValueError("q is required")
    query = " ".join(query.strip().split())
    if len(query) > MAX_QUERY_CHARS:
        raise ValueError(f"q must be {MAX_QUERY_CHARS} characters or fewer")
    group_limit = max(1, min(int(group_limit), MAX_GROUPS))
    rows = _valid_rows(bool(include_hidden))
    if not rows:
        return {"ok": True, "query": query, "mode": "empty", "groups": [],
                "searched_photos": 0}

    if query:
        query_embedding = _embed_query(query)
        scored = []
        for row in rows:
            try:
                embedding = unpack_embedding(row["embedding"], row["embedding_dim"])
                score = cosine(query_embedding, embedding)
            except (ValueError, struct.error):
                continue
            scored.append((score, row))
        mode = "semantic"
    else:
        # 空検索はAPI費用を使わず、AI採用度とローカル品質でレビュー候補を並べる。
        scored = [((row["keep_score"] if row["keep_score"] is not None
                    else row["quality_score"] or 0) / 100.0, row) for row in rows]
        mode = "recommended"
    scored.sort(key=lambda item: (item[0], item[1]["favorite"]), reverse=True)

    row_scores = {row["id"]: score for score, row in scored}
    members_by_group = {}
    for row in rows:
        members_by_group.setdefault(_group_key(row), []).append(row)
    group_meta = {row["group_id"]: dict(row) for row in dbq(
        """SELECT group_id,kind,summary,min_similarity,max_similarity,avg_similarity
             FROM photo_ai_groups""")}

    groups = []
    seen = set()
    for best_score, seed in scored:
        key = _group_key(seed)
        if key in seen:
            continue
        seen.add(key)
        members = members_by_group[key]
        members.sort(key=lambda row: (
            bool(row["favorite"]), bool(row["recommended"]),
            row_scores.get(row["id"], -1), row["keep_score"] or -1), reverse=True)
        visual, visual_cluster_count = _visual_clusters(members)
        meta = group_meta.get(key)
        groups.append({
            "group_id": key,
            "kind": meta["kind"] if meta else (
                "similar" if key.startswith("local-similar-") else "single"),
            "summary": meta["summary"] if meta else "",
            "best_similarity": round(float(best_score), 4),
            "min_similarity": meta.get("min_similarity") if meta else None,
            "max_similarity": meta.get("max_similarity") if meta else None,
            "avg_similarity": meta.get("avg_similarity") if meta else None,
            "visual_similarity_threshold": VISUAL_CLUSTER_SIMILARITY,
            "visual_cluster_count": visual_cluster_count,
            "collapsed_photo_count": len(members) - visual_cluster_count,
            "photos": [_photo_json(row, row_scores.get(row["id"], 0.0),
                                   visual.get(row["id"]))
                       for row in members],
        })
        if len(groups) >= group_limit:
            break
    return {"ok": True, "query": query, "mode": mode, "groups": groups,
            "searched_photos": len(scored), "include_hidden": bool(include_hidden)}
