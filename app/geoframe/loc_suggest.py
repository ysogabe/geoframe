"""位置なし写真の撮影地候補を作る（Decisionで手がかりのある写真を選び、Geminiに推定させる）。

流れ（ai_worker から1サイクルずつ少量ずつ進める）:
1. Decision（gpu-evalのd3-lite）に、写真ごとに「撮影地の手がかりが写っているか」と被写体の
   種類を聞く。Decisionが応答しない間は判定を止める。判定の済んでいない写真がある日は
   Geminiへ送らないので、家の中のペット写真などが判定なしで外へ出ることはない。
2. 同じ日に撮った写真をまとめ（INFER_WINDOWS_S の最大窓＝12時間以上の空きか、日付の
   変わり目で区切る）、手がかりありの上位 MAX_SEND 枚をGeminiへ送って撮影地を推定させる。
   手がかりのある写真が1枚も無い日は送らない。
3. 推定した地名をNominatimで座標にし、写真ごとの候補として photo_loc_suggestions に入れる。

候補は photos.lat/lon に書かない。管理画面の位置指定で候補の場所へ地図を動かすだけで、
確定は人が地図をクリックしてSAVEしたとき（loc_source='manual'）に限る。手動で1枚決めれば
infer_locations() が同じ日の残り（±12時間）へ広げるので、候補は1日1枚決めれば足りる。

関門の基準（2026-10-13、位置なし写真64枚を目視でラベル付けして検証。1人のラベルで、
手がかりありの写真はエジプト・中国の旅行に偏っている。暫定値として扱う）:
  clue ≥ 0.2 または kind=landmark で、手がかりありの19枚中18枚を拾い、選ばれた21枚中
  18枚が手がかりあり（clue単独のAUCは0.97。landmarkと判定された9枚はすべて手がかりあり）。
Geminiの推定（23日分）: 看板の文字と有名な建物はほぼ正確。3.1-flash-liteは読めた店名から
所在地を決めつけ、確信度を常にhighと答えた。3.8-flashは手がかりが弱いと地域・国レベルに
留めたため、こちらを既定にしている。どちらでも確信度は当てにならないので自動確定はしない。
"""
from __future__ import annotations

import base64
import hashlib
import io
import json
import urllib.error
import urllib.request
from datetime import datetime

from PIL import Image, ImageOps

from .ai_search import AgentPlatformEmbeddingClient, AiSearchUnavailable
from .config import (DATA_DIR, DECISION_URL, DERIV_DIR, INFER_WINDOWS_S,
                     LOC_SUGGEST_CLUE_PER_CYCLE, LOC_SUGGEST_GEMINI_PER_CYCLE,
                     LOC_SUGGEST_MODEL)
from .geo import nominatim_search
from .store import dbm, dbq, dbx, setting_set
from .util import t_epoch

# ---- Decision（手がかりの判定）----
CLUE_MIN = 0.2              # これ以上なら手がかりあり（上の検証で決めた暫定値）
MAX_SEND = 6                # 1日分でGeminiへ送る最大枚数
# 文字が読めるかどうかが要なので、比較評価（768px）より大きく送る。
# Decisionは最大1.6MPで読むので、それに近い大きさにしておく。
IMAGE_MAX_PX = 1536
GROUP_GAP_S = max(INFER_WINDOWS_S)     # 手動の1枚が広がる窓と同じ幅で日をまとめる
DECISION_STATE = "写真を1枚見て答えてください。"
# 質問文と選択肢の説明は検証に使ったものと同じにする（変えたら基準を測り直す）
DECISION_QUESTIONS = {
    "clue": {"type": "noul", "instructions":
             "この写真には、撮影された国や都市を推定できる手がかり（有名な建物や遺跡、"
             "読める看板の文字、その土地特有の街並みや建築）が写っていますか？"},
    "kind": {"type": "choice", "instructions": "この写真の主な被写体はどれですか？", "criteria": {
        "landmark": "有名な建物・遺跡・寺社・記念碑",
        "street": "街並み・市場・看板のある通り・都市の景観",
        "nature": "山・海・花・木などの自然の景色",
        "indoor": "部屋・店内・館内などの屋内",
        "animal_person": "ペットや人物のアップ",
        "object": "食べ物・飲み物・小物・展示品のアップ"}},
}

# ---- Gemini（撮影地の推定）----
PROMPT_VERSION = "loc-v2"
MAX_ATTEMPTS = 5
PRECISIONS = ("landmark", "district", "city", "region", "country", "unknown")
CONFIDENCES = ("high", "medium", "low", "unknown")
PROMPT = """あなたは写真の撮影地を推定する専門家です。
以下の写真は同じ旅行の同じ日（{date}）に撮影されたものです。写っている建物・遺跡・看板の文字・
言語・街並み・植生・車両・ナンバープレートなどの手がかりから、撮影地を推定してください。
- 推測が難しい場合は無理に答えず confidence を "unknown" にしてください。
- geocode_query には OpenStreetMap の地名検索でヒットする英語の地名を入れてください
  （例: "Karnak Temple, Luxor"、"Zhujiajiao, Shanghai"）。特定できる最も細かい場所にします。
- 写真ごとに場所が違うと判断したら photos に写真ごとの推定を入れてください。
- evidence には判断の根拠（読めた文字、特定した建物など）を日本語で書いてください。

厳守事項:
- 看板の文字は、写真で実際に判読できた部分だけを「」で引用してください。一部しか読めない文字を
  既知の店名・施設名で補ってはいけません。
- 判読できた文字か、誰が見ても分かる有名な建物・景観がない限り、具体的な店名・施設名は答えず、
  市区町村や地域の単位にとどめてください。
- precision に、答えた場所の細かさ（landmark=特定の建物・施設、district=地区、city=市区町村、
  region=都道府県・州、country=国、unknown）を入れてください。photos の各写真にも同じ基準で入れます。
- confidence の high は「引用した文字か有名な建物で場所が確定する」場合だけです。"""
_CONF = {"type": "string", "enum": list(CONFIDENCES)}
_PREC = {"type": "string", "enum": list(PRECISIONS)}
SCHEMA = {
    "type": "object",
    "properties": {
        "country": {"type": "string"},
        "country_code": {"type": "string", "description": "ISO 3166-1 alpha-2"},
        "city": {"type": "string"},
        "place_name": {"type": "string", "description": "日本語の地名（最も細かい場所）"},
        "geocode_query": {"type": "string"},
        "confidence": _CONF,
        "precision": _PREC,
        "evidence": {"type": "array", "items": {"type": "string"}},
        "photos": {"type": "array", "items": {"type": "object", "properties": {
            "index": {"type": "integer"},
            "place_name": {"type": "string"},
            "geocode_query": {"type": "string"},
            "confidence": _CONF,
            "precision": _PREC,
        }, "required": ["index", "place_name", "geocode_query", "confidence", "precision"]}},
    },
    "required": ["country", "country_code", "city", "place_name", "geocode_query",
                 "confidence", "precision", "evidence", "photos"],
}


class DecisionUnavailable(Exception):
    """Decisionサーバーが応答しない（停止中・GPUを他の処理に譲っている等）。このサイクルは判定を止める"""


def _now() -> str:
    return datetime.now().astimezone().isoformat(timespec="seconds")


def _image_rel(row) -> str:
    """表示用画像のDATA_DIRからの相対パス。displayが無いときは過去に作ったderivativeを拾う
    （admin._thumb_for と同じ考え方。ここでDriveから取りに行くことはしない）"""
    if row["display"]:
        return row["display"]
    if row["drive_id"] and (DERIV_DIR / f"{row['drive_id']}.jpg").exists():
        return str((DERIV_DIR / f"{row['drive_id']}.jpg").relative_to(DATA_DIR))
    return ""


def _image_bytes(rel: str) -> bytes:
    root = DATA_DIR.resolve()
    path = (root / rel).resolve()
    path.relative_to(root)                 # DATA_DIRの外は読まない
    with Image.open(path) as image:
        image = ImageOps.exif_transpose(image)
        image.thumbnail((IMAGE_MAX_PX, IMAGE_MAX_PX), Image.Resampling.LANCZOS)
        if image.mode != "RGB":
            image = image.convert("RGB")
        output = io.BytesIO()
        image.save(output, "JPEG", quality=88)
        return output.getvalue()


# ============================================================
# Decision
# ============================================================
def decision_health(timeout=2.0) -> dict | None:
    """判定できる状態なら /health の応答（model名を含む）、できなければ None"""
    try:
        with urllib.request.urlopen(DECISION_URL + "/health", timeout=timeout) as r:
            data = json.loads(r.read().decode("utf-8"))
    except Exception:
        return None
    if not isinstance(data, dict) or data.get("status") != "ready":
        return None
    if "image" not in (data.get("modalities") or []):
        return None
    return data


def decision_ask(image: bytes, model: str) -> dict:
    """1枚を判定して {"clue": 0〜1, "kind": 選択肢} を返す。
    4xx（画像が大きすぎる等）はその写真だけの失敗として ValueError、
    接続失敗・タイムアウト・5xxはサーバー側の問題として DecisionUnavailable"""
    body = json.dumps({
        "model": model, "state": DECISION_STATE, "questions": DECISION_QUESTIONS,
        "images": ["data:image/jpeg;base64," + base64.b64encode(image).decode("ascii")],
    }).encode()
    req = urllib.request.Request(DECISION_URL + "/v1/systemone", data=body,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            answers = json.loads(r.read().decode("utf-8"))["answers"]
    except urllib.error.HTTPError as exc:
        if 400 <= exc.code < 500:
            raise ValueError(f"Decision HTTP {exc.code}") from exc
        raise DecisionUnavailable(f"Decision HTTP {exc.code}") from exc
    except (urllib.error.URLError, OSError) as exc:
        raise DecisionUnavailable(str(exc)) from exc
    clue = float(answers["clue"]["noul"])
    kind = str(answers["kind"]["choice"])
    if not 0.0 <= clue <= 1.0 or kind not in DECISION_QUESTIONS["kind"]["criteria"]:
        raise ValueError(f"Decisionの応答が想定外です: clue={clue!r} kind={kind!r}")
    return {"clue": clue, "kind": kind}


def passes(photo: dict) -> bool:
    """Geminiへ送ってよい写真か（手がかりあり）"""
    return (not photo["clue_error"] and photo["clue"] is not None
            and (photo["clue"] >= CLUE_MIN or photo["kind"] == "landmark"))


def _rank_key(photo: dict):
    return (photo["kind"] != "landmark", -photo["clue"], photo["id"])


# ============================================================
# 対象の読み込みと日ごとのまとまり
# ============================================================
def _load_targets() -> list[dict]:
    """位置なし・撮影時刻ありの写真と、現在の画像に対するDecision判定（無ければNone）"""
    clues = {r["photo_id"]: r for r in dbq("SELECT * FROM photo_loc_clues")}
    out = []
    for r in dbq("""SELECT id,display,drive_id,md5,mtime,taken_at FROM photos
                    WHERE lat IS NULL AND taken_at IS NOT NULL AND taken_at!=''"""):
        epoch = t_epoch(r["taken_at"])
        if epoch is None:
            continue
        rel = _image_rel(r)
        c = clues.get(r["id"])
        fresh = (c is not None and c["display"] == rel and c["source_md5"] == r["md5"]
                 and c["source_mtime"] == r["mtime"])
        out.append({"id": r["id"], "rel": rel, "md5": r["md5"], "mtime": r["mtime"],
                    "taken_at": r["taken_at"], "epoch": epoch,
                    "classified": fresh,
                    "clue": c["clue"] if fresh else None,
                    "kind": c["kind"] if fresh else "",
                    "clue_error": c["error"] if fresh else ""})
    out.sort(key=lambda p: (p["epoch"], p["id"]))
    return out


def group_days(photos: list[dict]) -> list[list[dict]]:
    """撮影時刻順に、GROUP_GAP_S を超える空きか日付の変わり目で区切る"""
    groups: list[list[dict]] = []
    for photo in sorted(photos, key=lambda p: (p["epoch"], p["id"])):
        if (not groups or photo["epoch"] - groups[-1][-1]["epoch"] > GROUP_GAP_S
                or photo["taken_at"][:10] != groups[-1][0]["taken_at"][:10]):
            groups.append([])
        groups[-1].append(photo)
    return groups


def pick_send(group: list[dict]) -> list[dict]:
    """Geminiへ送る写真。landmarkを先に、次に手がかりの強い順で MAX_SEND 枚まで"""
    return sorted((p for p in group if passes(p)), key=_rank_key)[:MAX_SEND]


def _classify(targets: list[dict], limit: int, model: str) -> int:
    """未判定の写真をDecisionで判定する。日の途中で止まらないよう撮影時刻順に進める"""
    done = 0
    for photo in targets:
        if photo["classified"]:
            continue
        if done >= max(0, limit):
            break
        clue, kind, error = None, "", ""
        if not photo["rel"]:
            error = "表示用画像がありません"
        else:
            try:
                result = decision_ask(_image_bytes(photo["rel"]), model)
                clue, kind = result["clue"], result["kind"]
            except DecisionUnavailable:
                raise
            except Exception as exc:     # この写真だけの失敗は手がかり無しとして記録して進む
                error = f"{type(exc).__name__}: {exc}"[:300]
        dbx("""INSERT INTO photo_loc_clues(
                 photo_id,display,source_md5,source_mtime,clue,kind,model,analyzed_at,error)
               VALUES(?,?,?,?,?,?,?,?,?)
               ON CONFLICT(photo_id) DO UPDATE SET
                 display=excluded.display,source_md5=excluded.source_md5,
                 source_mtime=excluded.source_mtime,clue=excluded.clue,kind=excluded.kind,
                 model=excluded.model,analyzed_at=excluded.analyzed_at,error=excluded.error""",
            (photo["id"], photo["rel"], photo["md5"], photo["mtime"], clue, kind, model,
             _now(), error))
        photo.update(classified=True, clue=clue, kind=kind, clue_error=error)
        done += 1
    return done


# ============================================================
# Gemini
# ============================================================
def run_id_for(model: str, sent_ids: list[int]) -> str:
    key = f"{PROMPT_VERSION}|{model}|{','.join(str(i) for i in sorted(sent_ids))}"
    return "loc-" + hashlib.sha256(key.encode()).hexdigest()[:16]


def _parse_answer(result: dict, sent_count: int) -> dict:
    try:
        value = json.loads(result["candidates"][0]["content"]["parts"][0]["text"])
    except Exception as exc:
        raise RuntimeError("Geminiの応答にJSONがありません") from exc
    if not isinstance(value, dict) or not isinstance(value.get("place_name"), str):
        raise RuntimeError("Geminiの撮影地JSONが不完全です")
    if value.get("precision") not in PRECISIONS:
        value["precision"] = "unknown"
    if value.get("confidence") not in CONFIDENCES:
        value["confidence"] = "unknown"
    evidence = value.get("evidence")
    value["evidence"] = [str(e) for e in evidence] if isinstance(evidence, list) else []
    photos = []
    for item in value.get("photos") or []:
        if (isinstance(item, dict) and isinstance(item.get("index"), int)
                and 1 <= item["index"] <= sent_count and isinstance(item.get("place_name"), str)):
            if item.get("precision") not in PRECISIONS:
                item["precision"] = value["precision"]
            if item.get("confidence") not in CONFIDENCES:
                item["confidence"] = "unknown"
            photos.append(item)
    value["photos"] = photos
    return value


def ask_gemini(client, day: str, rels: list[str]) -> dict:
    parts = [{"text": PROMPT.format(date=day)}]
    for index, rel in enumerate(rels, 1):
        parts.append({"text": f"写真{index}"})
        parts.append({"inlineData": {"mimeType": "image/jpeg",
                                     "data": base64.b64encode(_image_bytes(rel)).decode("ascii")}})
    result = client.post(LOC_SUGGEST_MODEL, "generateContent", {
        "contents": [{"role": "user", "parts": parts}],
        "generationConfig": {"temperature": 0.1, "responseMimeType": "application/json",
                             "responseJsonSchema": SCHEMA},
    }, timeout=180)
    return _parse_answer(result, len(rels))


# 地名の後ろの区切りだけで引き直したときの細かさ（"Giza Plateau, Giza" → "Giza"）
COARSER = {"landmark": "city", "district": "city", "city": "region", "region": "country",
           "country": "country"}


def _geocode(query: str, precision: str, countrycode: str) -> tuple[dict | None, str]:
    """AIの地名を座標にする。見つからなければ最初の区切りを外して1回だけ引き直し、
    当たったら細かさを1段粗くする。返り値は (候補 or None, 細かさ)"""
    query = query.strip()
    if not query or precision == "unknown":
        return None, precision
    hits = nominatim_search(query, countrycode)
    if hits:
        return hits[0], precision
    parts = [part.strip() for part in query.split(",") if part.strip()]
    if len(parts) > 1:
        hits = nominatim_search(", ".join(parts[1:]), countrycode)
        if hits:
            return hits[0], COARSER[precision]
    return None, precision


def _apply_run(run, members: list[dict]) -> int:
    """終わった問い合わせの答えを、今そのまとまりにいる写真の候補として書く"""
    answer = json.loads(run["answer_json"])
    sent_ids = json.loads(run["sent_ids_json"])
    member_ids = {p["id"] for p in members}
    country = answer.get("country_code") if isinstance(answer.get("country_code"), str) else ""
    day_hit, day_precision = _geocode(answer.get("geocode_query", ""), answer["precision"], country)
    per_photo = {}
    for item in answer["photos"]:
        pid = sent_ids[item["index"] - 1]
        if pid in member_ids and item["place_name"].strip():
            per_photo[pid] = item
    rows = []
    for pid in sorted(member_ids):
        item = per_photo.get(pid)
        if item and item["confidence"] != "unknown" and item["precision"] != "unknown":
            hit, precision = _geocode(item.get("geocode_query", ""), item["precision"], country)
            if hit is None:          # 写真ごとの地名が引けなければ日の代表点。拡大率もそれに合わせる
                hit, precision = day_hit, day_precision
            rows.append((pid, run["run_id"], "photo", item["place_name"], answer["place_name"],
                         item.get("geocode_query", ""), precision, item["confidence"],
                         json.dumps(answer["evidence"], ensure_ascii=False),
                         hit["lat"] if hit else None, hit["lon"] if hit else None,
                         hit["title"] if hit else "", run["model"], run["analyzed_at"]))
        else:
            rows.append((pid, run["run_id"], "day", answer["place_name"], answer["place_name"],
                         answer.get("geocode_query", ""), day_precision,
                         answer["confidence"],
                         json.dumps(answer["evidence"], ensure_ascii=False),
                         day_hit["lat"] if day_hit else None, day_hit["lon"] if day_hit else None,
                         day_hit["title"] if day_hit else "", run["model"], run["analyzed_at"]))
    dbm("""INSERT OR REPLACE INTO photo_loc_suggestions(
             photo_id,run_id,scope,place_name,day_place_name,geocode_query,precision,
             confidence,evidence_json,lat,lon,geocode_title,model,analyzed_at)
           VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)""", rows)
    return len(rows)


def _needs_apply(run_id: str, members: list[dict], current: dict) -> bool:
    """候補が未作成・別の問い合わせのもの・座標化がまだ、のいずれかなら書き直す。
    座標化の再試行はNominatimのキャッシュに当たるので、0件だった地名で通信は増えない"""
    for photo in members:
        row = current.get(photo["id"])
        if row is None or row["run_id"] != run_id:
            return True
        if row["lat"] is None and row["geocode_query"] and row["precision"] != "unknown":
            return True
    return False


def _reusable_run(day: str, members: list[dict], runs: list) -> object | None:
    """同じ日の終わった問い合わせで、今の写真を全部含んでいたものがあれば使い回す。
    1枚を手動で決めて同じ日の写真が減っただけなら、Geminiに聞き直さない"""
    ids = {p["id"] for p in members}
    best = None
    for run in runs:
        if (run["day"] == day and run["status"] == "done" and run["model"] == LOC_SUGGEST_MODEL
                and run["prompt_version"] == PROMPT_VERSION
                and ids <= set(json.loads(run["member_ids_json"]))):
            if best is None or run["analyzed_at"] > best["analyzed_at"]:
                best = run
    return best


def loc_suggest_cycle(clue_limit=LOC_SUGGEST_CLUE_PER_CYCLE,
                      gemini_limit=LOC_SUGGEST_GEMINI_PER_CYCLE) -> dict:
    targets = _load_targets()
    health = decision_health()
    decision = "ready" if health else "unavailable"
    classified = 0
    if health:
        try:
            classified = _classify(targets, clue_limit, str(health.get("model") or "decision"))
        except DecisionUnavailable as exc:
            decision = "unavailable"
            print(f"[loc] Decisionが途中で応答しなくなりました（次回再開）: {exc}")

    runs = dbq("SELECT * FROM photo_loc_runs")
    by_id = {run["run_id"]: run for run in runs}
    current = {r["photo_id"]: r for r in dbq("SELECT * FROM photo_loc_suggestions")}
    client = None
    asked = applied = waiting = 0
    for group in group_days(targets):
        if not all(p["classified"] for p in group):
            waiting += 1          # 判定の済んでいない写真がある日は送らない（関門を飛ばさない）
            continue
        send = pick_send(group)
        if not send:
            continue
        day = group[0]["taken_at"][:10]
        sent_ids = [p["id"] for p in send]
        run_id = run_id_for(LOC_SUGGEST_MODEL, sent_ids)
        run = by_id.get(run_id)
        if not (run and run["status"] == "done"):
            run = _reusable_run(day, group, runs) or run
        if run and run["status"] == "done":
            if _needs_apply(run["run_id"], group, current):
                applied += _apply_run(run, group)
            continue
        if run and run["attempts"] >= MAX_ATTEMPTS:
            continue
        if asked >= max(0, gemini_limit):
            waiting += 1
            continue
        attempts = run["attempts"] if run else 0
        members_json = json.dumps([p["id"] for p in group])
        asked += 1
        try:
            if client is None:
                client = AgentPlatformEmbeddingClient.from_environment()
            answer = ask_gemini(client, day, [p["rel"] for p in send])
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"[:500]
            dbx("""INSERT OR REPLACE INTO photo_loc_runs(
                     run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,
                     attempts,answer_json,error,analyzed_at)
                   VALUES(?,?,?,?,?,?,'error',?,'',?,?)""",
                (run_id, day, LOC_SUGGEST_MODEL, PROMPT_VERSION, members_json,
                 json.dumps(sent_ids), attempts + 1, error, _now()))
            print(f"[loc] 撮影地の推定に失敗（次回再試行）: {day}: {error}")
            if isinstance(exc, AiSearchUnavailable):
                break
            continue
        dbx("""INSERT OR REPLACE INTO photo_loc_runs(
                 run_id,day,model,prompt_version,member_ids_json,sent_ids_json,status,
                 attempts,answer_json,error,analyzed_at)
               VALUES(?,?,?,?,?,?,'done',?,?,'',?)""",
            (run_id, day, LOC_SUGGEST_MODEL, PROMPT_VERSION, members_json,
             json.dumps(sent_ids), attempts + 1, json.dumps(answer, ensure_ascii=False), _now()))
        run = dbq("SELECT * FROM photo_loc_runs WHERE run_id=?", (run_id,))[0]
        applied += _apply_run(run, group)
        print(f"[loc] 撮影地の候補: {day} → {answer['place_name']}"
              f"（{answer['precision']}、{len(send)}枚を送信）")
    result = {"decision": decision, "classified": classified, "asked": asked,
              "applied": applied, "waiting_days": waiting, "at": _now()}
    setting_set("photo_loc_status", json.dumps(result, ensure_ascii=False))
    return result


def loc_status() -> dict:
    """管理画面のSTATUS用の集計"""
    try:
        last = json.loads(dbq("SELECT value FROM settings WHERE key='photo_loc_status'")[0]["value"])
    except (IndexError, TypeError, ValueError):
        last = {}
    targets = _load_targets()
    groups = group_days(targets)
    suggested = dbq("""SELECT COUNT(*) c FROM photo_loc_suggestions s JOIN photos p
                         ON p.id=s.photo_id WHERE p.lat IS NULL""")[0]["c"]
    runs = {r["status"]: r["c"] for r in dbq(
        "SELECT status, COUNT(*) c FROM photo_loc_runs GROUP BY status")}
    return {
        "decision": last.get("decision", ""),
        "last_at": last.get("at", ""),
        "targets": len(targets),
        "clue_pending": sum(not p["classified"] for p in targets),
        "clue_passed": sum(p["classified"] and passes(p) for p in targets),
        "days": len(groups),
        "days_with_clue": sum(bool(pick_send(g)) for g in groups),
        "runs_done": runs.get("done", 0),
        "runs_error": runs.get("error", 0),
        "suggested": suggested,
    }
