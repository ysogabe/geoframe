#!/usr/bin/env python3
"""撮影地候補の関門（Decisionの手がかり判定）を、目視ラベル付きの64枚で測り直す。

    # Decisionに64枚を問い合わせて測る（本番の /opt/geoframe で、geoframe ユーザーとして）
    sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe \\
        app/.venv/bin/python tools/loc_clue_eval/eval_decision.py
    # ai_worker が photo_loc_clues に保存した判定で測る（Decisionは呼ばない）
    sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe \\
        app/.venv/bin/python tools/loc_clue_eval/eval_decision.py --from-db
    # 保存済みの判定ファイルで測る（DBもDecisionも使わない）
    python3 tools/loc_clue_eval/eval_decision.py --scores tools/loc_clue_eval/baseline-2026-10-13.json

質問文・画像の縮小・合否の判定は app/geoframe/loc_suggest.py のものをそのまま使うので、
そちらを変えたらこれを流して、手がかりありの写真の取りこぼしと、手がかりなしの写真が
Geminiへ出る割合を比べる。--save で判定結果を JSON に残せば、次回 --scores で比べられる。
"""
import argparse
import json
import sqlite3
from contextlib import closing
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "app"))

from geoframe import loc_suggest  # noqa: E402
from geoframe.config import DATA_DIR, DB_PATH, DECISION_URL  # noqa: E402

THRESHOLDS = (0.1, 0.2, 0.3, 0.5, 0.7)


def dbq(sql: str, args=()) -> list:
    """本番のDBを読むだけなので読み取り専用で開く（store.db() はスキーマの作成・移行をする）"""
    with closing(sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)) as conn:
        conn.row_factory = sqlite3.Row
        return conn.execute(sql, args).fetchall()


def ask_decision(ids: list[int], model: str) -> list[dict]:
    if loc_suggest.decision_health() is None:
        sys.exit(f"Decision（{DECISION_URL}）が応答しないか、画像を扱えません")
    rows = {r["id"]: r for r in dbq(
        f"SELECT id,display,drive_id FROM photos WHERE id IN ({','.join('?' * len(ids))})", ids)}
    out = []
    for pid in ids:
        rel = loc_suggest._image_rel(rows[pid]) if pid in rows else ""
        if not rel:
            print(f"  写真{pid}: 画像が見つからないので除外（DATA_DIR={DATA_DIR}）", file=sys.stderr)
            continue
        try:
            out.append({"photo_id": pid, **loc_suggest.decision_ask(loc_suggest._image_bytes(rel), model)})
        except (ValueError, OSError) as exc:
            print(f"  写真{pid}: {exc}", file=sys.stderr)
    return out


def from_db(ids: list[int]) -> list[dict]:
    rows = dbq(f"""SELECT photo_id,clue,kind FROM photo_loc_clues
                   WHERE error='' AND photo_id IN ({','.join('?' * len(ids))})""", ids)
    return [{"photo_id": r["photo_id"], "clue": r["clue"], "kind": r["kind"]} for r in rows]


def auc(scores: list[dict], pos: set[int]) -> float:
    p = [s["clue"] for s in scores if s["photo_id"] in pos]
    n = [s["clue"] for s in scores if s["photo_id"] not in pos]
    if not p or not n:
        return float("nan")
    return sum((a > b) + 0.5 * (a == b) for a in p for b in n) / (len(p) * len(n))


def line(label: str, selected: set[int], pos: set[int]) -> str:
    tp = len(selected & pos)
    return (f"  {label:22s} 選択{len(selected):3d}  手がかりあり{tp:3d}/{len(pos)}"
            f"  適合率{tp / max(1, len(selected)):.2f}  取りこぼし{sorted(pos - selected)}")


def report(scores: list[dict], labels: list[dict]) -> None:
    got = {s["photo_id"] for s in scores}
    pos = {lb["photo_id"] for lb in labels if lb["clue"] and lb["photo_id"] in got}
    print(f"判定あり {len(scores)}/{len(labels)}枚（うち目視で手がかりあり {len(pos)}枚）")
    print(f"clue の AUC = {auc(scores, pos):.3f}")
    print("== 現在の基準（loc_suggest.passes）")
    print(line(f"clue≥{loc_suggest.CLUE_MIN} or landmark",
               {s["photo_id"] for s in scores if loc_suggest.passes({"clue_error": "", **s})}, pos))
    print("== clue のしきい値だけで選んだ場合")
    for th in THRESHOLDS:
        print(line(f"clue≥{th}", {s["photo_id"] for s in scores if s["clue"] >= th}, pos))
    print("== kind ごと")
    kinds: dict[str, list[int]] = {}
    for s in scores:
        kinds.setdefault(s["kind"], []).append(s["photo_id"])
    for kind, ids in sorted(kinds.items()):
        print(f"  {kind:14s} {len(ids):3d}枚  うち手がかりあり {len(set(ids) & pos)}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    src = ap.add_mutually_exclusive_group()
    src.add_argument("--from-db", action="store_true", help="photo_loc_clues の判定を使う")
    src.add_argument("--scores", type=Path, help="保存済みの判定（[{photo_id, clue, kind}]）を使う")
    ap.add_argument("--labels", type=Path, default=HERE / "labels.json")
    ap.add_argument("--model", default="d3-lite", help="Decisionのモデル名")
    ap.add_argument("--save", type=Path, help="Decisionに聞いた結果をこのJSONへ保存する")
    args = ap.parse_args()

    labels = json.loads(args.labels.read_text())
    ids = [lb["photo_id"] for lb in labels]
    if args.scores:
        scores = [s for s in json.loads(args.scores.read_text()) if s["photo_id"] in set(ids)]
    elif args.from_db:
        scores = from_db(ids)
    else:
        scores = ask_decision(ids, args.model)
        if args.save:
            args.save.write_text(json.dumps(scores, ensure_ascii=False, indent=1) + "\n")
    report(scores, labels)


if __name__ == "__main__":
    main()
