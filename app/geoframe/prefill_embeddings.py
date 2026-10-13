"""ローカルembedding（embeddinggemma-2）を、切り替える前にキャッシュ表へ作っておく。

GEOFRAME_EMBED_BACKEND=local へ切り替えると全写真のembeddingが作り直しになり、
ai_workerの1分12枚のペースでは数時間かかる（その間、未embeddingの写真を含む場面は
グループにならない）。このコマンドでphoto_embeddingsを先に埋めておけば、切り替えた
最初のサイクルでキャッシュから一度に入れ替わる。サービスはgeminiのまま動かしてよい。

    cd /opt/geoframe/app
    sudo -u geoframe env GEOFRAME_DATA_DIR=/var/lib/geoframe \\
        nice -n 10 .venv/bin/python -m geoframe.prefill_embeddings [--threads 8] [--limit N]
"""
from __future__ import annotations

import argparse
import base64
import time

from .ai_pipeline import EMBED_DIM, _image_bytes, _load_photos, _now, _safe_path
from .ai_search import pack_embedding
from .local_embedding import LOCAL_EMBED_MODEL, LocalEmbeddingClient
from .store import dbm, dbq


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--threads", type=int, default=None, help="CPUスレッド数（既定は設定値）")
    parser.add_argument("--limit", type=int, default=0, help="作る枚数の上限（0は全部）")
    args = parser.parse_args(argv)
    client = LocalEmbeddingClient() if args.threads is None else LocalEmbeddingClient(threads=args.threads)
    have = {row["content_sha256"] for row in dbq(
        "SELECT content_sha256 FROM photo_embeddings WHERE embedding_model=? AND embedding_dim=?",
        (LOCAL_EMBED_MODEL, EMBED_DIM))}
    todo = [photo for photo in _load_photos() if photo.sha256 not in have]
    if args.limit:
        todo = todo[:args.limit]
    print(f"[prefill] {LOCAL_EMBED_MODEL}: 作成済み{len(have)}件、これから{len(todo)}件")
    batch, done, started = [], 0, time.monotonic()
    failed = 0
    for photo in todo:
        done += 1
        try:
            payload = base64.b64encode(_image_bytes(_safe_path(photo.display))).decode("ascii")
        except (OSError, ValueError) as exc:  # 派生画像が消えた等。ai_workerが後で扱う
            failed += 1
            print(f"[prefill] 読めない写真をとばす: id={photo.id}: {type(exc).__name__}")
            continue
        vector = client.embed_part({"inlineData": {"mimeType": "image/jpeg", "data": payload}})
        batch.append((photo.sha256, LOCAL_EMBED_MODEL, EMBED_DIM, pack_embedding(vector), _now()))
        if batch and (len(batch) >= 50 or done == len(todo)):
            dbm("""INSERT OR REPLACE INTO photo_embeddings(
                     content_sha256,embedding_model,embedding_dim,embedding,analyzed_at)
                   VALUES(?,?,?,?,?)""", batch)
            batch = []
            rate = done / max(1e-9, time.monotonic() - started)
            print(f"[prefill] {done}/{len(todo)}（{rate:.2f}枚/秒）", flush=True)
    if batch:
        dbm("""INSERT OR REPLACE INTO photo_embeddings(
                 content_sha256,embedding_model,embedding_dim,embedding,analyzed_at)
               VALUES(?,?,?,?,?)""", batch)
    print(f"[prefill] 完了: {done - failed}件、とばした{failed}件")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
