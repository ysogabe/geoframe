"""同期サイクル（バックグラウンドワーカー）（server.py L696-L745 より移動）"""
import time

# worker() は可変なモジュール変数 tiles._tile_prune_at を読むため、値ではなく
# モジュールを import して属性参照する（from .tiles import _tile_prune_at は不可）
from . import tiles
from .config import DRIVE_KEY, DRIVE_POLL_S, FETCH_PER_CYCLE
from .drive import (drive_service, fetch_derivatives, index_drive, index_local,
                    local_derivatives)
from .geo import enrich_rows, infer_locations
from .quality import quality_cycle
from .store import dbq
from .tiles import tile_cache_prune


# ============================================================
# 同期サイクル（バックグラウンドワーカー）
# ============================================================
def cycle():
    svc = None
    if DRIVE_KEY.exists():
        try:
            svc = drive_service()
        except Exception as e:
            print(f"[drive] 認証エラー: {e}")
    index_local()
    if svc:
        try:
            total, n_new, n_del = index_drive(svc)
            if n_new or n_del:
                print(f"[drive] 索引: リモート{total} 追加{n_new} 削除{n_del}")
        except Exception as e:
            print(f"[drive] 索引エラー: {e}")
    infer_locations()
    enrich_rows()
    local_derivatives(FETCH_PER_CYCLE)
    if svc:
        try:
            fetch_derivatives(svc, FETCH_PER_CYCLE)
        except Exception as e:
            print(f"[drive] 取得エラー: {e}")
    try:
        quality = quality_cycle()
        if quality["remaining"]:
            print(f"[quality] 特徴量の解析待ち {quality['remaining']}件")
    except Exception as e:
        # 品質解析に失敗しても、写真の索引・取得サイクルは止めない。
        # photo_quality は前回の完成結果を維持する。
        print(f"[quality] 解析エラー（前回結果を維持）: {e}")
    ready = dbq("SELECT COUNT(*) c FROM photos WHERE display IS NOT NULL AND lat IS NOT NULL")[0]["c"]
    total = dbq("SELECT COUNT(*) c FROM photos")[0]["c"]
    pend = dbq("SELECT COUNT(*) c FROM photos WHERE lat IS NOT NULL AND display IS NULL")[0]["c"]
    print(f"[cycle] 表示可能 {ready} / 取得待ち {pend} / 全{total}")


def worker():
    # 初回サイクルもバックグラウンドで実行（HTTPサーバーを待たせない）
    while True:
        try:
            cycle()
        except Exception as e:
            print(f"[cycle] エラー: {e}")
        # タイルキャッシュ整理は1時間に1回。1日1回だと超過分が丸一日滞留してディスクを圧迫する。
        # 累積書込量トリガではなく単純な時間間隔にしたのは、書込パスに別ロックの
        # カウンタを通す必要がなく、TILE_FETCH_PER_MIN で書込レート自体に上限が
        # かかっているため時間間隔で十分に抑えられるから。
        # （最悪ケース: 240枚/分 × 約15KB ≒ 216MB/時 ＝ 上限2048MBの約10%/時）
        if time.time() - tiles._tile_prune_at > 3600:
            try:
                tile_cache_prune()
            except Exception as e:
                print(f"[tile] キャッシュ整理エラー: {e}")
        time.sleep(DRIVE_POLL_S)
