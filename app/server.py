#!/usr/bin/env python3
"""
GeoFrame ローカルサーバー（写真管理基盤: SQLiteカタログ + Drive直接参照 + derivative配信）
==================================================================================
アーキテクチャ:
  - Google Drive = 写真ブロブの唯一の真実（GeoFrameフォルダ）
  - コードとデータは分離する。ソース（geoframe/ static/ vendor/）はこのディレクトリ、
      書き換わるデータは全て DATA_DIR 配下（既定 app/var/、GEOFRAME_DATA_DIR で変更可）。
      以下の catalog.db・derivatives/・photos/・tile_cache/ は全て DATA_DIR 基準。
  - catalog.db (SQLite) = 写真カタログの一次データ。
      Drive APIの imageMediaMetadata（撮影時刻・GPS）でダウンロード無しに索引化し、
      GPSの無い写真は撮影時刻が近いGPS付き写真から位置を推定（≈表示）する。
      ※Vertex AI 課金済み embedding・手動 GPS 修正・お気に入りを含み再構築不可。
        バックアップは必ず sqlite3 .backup で取得する（cp 禁止）。
  - derivatives/ = 表示用2560px JPEG。HEICもここでJPEG化されるためブラウザ表示可能。
      オリジナルはローカルに保持しない（Driveにのみ存在）。
  - ローカル photos/ フォルダも従来どおり索引対象（source='local'）。
      ※旧ミラー先 photos/gdrive/ は重複を避けるため索引から除外（削除してよい）
  - tile_cache/ = 管理ページの地図に使う背景タイル（OSM・地理院）のプロキシキャッシュ。
      /tiles/{osm,gsi}/{z}/{x}/{y}.png で配信。

使い方:
    pip install -r requirements.txt
    python server.py               # http://localhost:8080
    python server.py --organize    # ローカルの表示不能写真を _excluded/no_gps/ へ退避
    python server.py --quality-scan # 初回の写真品質特徴量を一括解析
    ※catalog.db は消さないこと（上記のとおり再構築不可の一次データ）

環境変数:
    GEOFRAME_DATA_DIR(app/var) … データルート。catalog.db・derivatives/・photos/・
        blog_exports/・各種キャッシュの置き場所。本番は /var/lib/geoframe 等に分離する。
        ※.env はこのディレクトリではなくソース側（app/.env）から読む
          （DATA_DIR自体を.envで指定できる必要があるため）
    GEOFRAME_HOST(0.0.0.0) GEOFRAME_PORT(8080) GEOFRAME_PHOTOS_DIR(photos)
    GEOFRAME_DRIVE_KEY(drive_key.json) GEOFRAME_DRIVE_FOLDER(GeoFrame)
    GEOFRAME_DRIVE_POLL_MIN(10) GEOFRAME_FETCH_PER_CYCLE(25)
    GEOFRAME_QUALITY_PER_CYCLE(同FETCH_PER_CYCLE) … 1同期あたりの写真品質解析数
    GEOFRAME_AI_ENABLED(1) GEOFRAME_AI_EMBED_PER_CYCLE(12)
    GEOFRAME_AI_JUDGE_PER_CYCLE(2) GEOFRAME_AI_POLL_S(60)
    GEOFRAME_AI_SEARCH_PER_MIN(30) … /api/ai/search の流量上限（超過分は429）
    GEOFRAME_INFER_MIN(15) GEOFRAME_INFER_OFFSET_MIN(0)
    GEOFRAME_TILE_UA(GeoFrame/0.1 …) GEOFRAME_TILE_CACHE_MAX_MB(16384)
    GEOFRAME_TILE_FETCH_PER_MIN(240) … 上流タイル取得の流量上限（キャッシュヒットは無制限）
    GEOFRAME_ALLOWED_HOSTS(未設定) … Host検証で追加許可するホスト名（カンマ区切り）。
        例 geoframe.local:8080,frame.lan 。localhost とIPリテラルは既定で許可される
※家庭内利用前提。インターネットに公開しないこと。
※静的配信はホワイトリスト方式（/favicon.ico /static/ /vendor/ /derivatives/ /photos/ /blog_exports/）。
  新しく配信したい静的ディレクトリが増えたら Handler.STATIC_ALLOW_* に追加すること。
  許可prefix配下でもドットファイルは配信しない（作業用の一時ファイル対策）。
※Hostヘッダを検証している（DNSリバインディング対策）。ホスト名でアクセスして403に
  なる場合は GEOFRAME_ALLOWED_HOSTS に足すこと。起動時に許可内容を1行ログに出す。
"""
import sys
import threading
from http.server import ThreadingHTTPServer

from geoframe.catalog import worker
from geoframe.ai_pipeline import ai_worker
from geoframe.config import DB_PATH, DRIVE_KEY, HOST, PORT, allowed_hosts_note
from geoframe.config import AI_ENABLED
from geoframe.drive import organize_photos, prepare_tmp_dir
from geoframe.handlers import Handler
from geoframe.quality import quality_backfill
from geoframe.store import (cache_saver, db, load_cache, purge_cache_legacy,
                            save_cache)
from geoframe.tiles import tile_cache_prune


if __name__ == "__main__":
    load_cache()
    if purge_cache_legacy():
        save_cache(force=True)     # 縮んだ状態をすぐ確定させる（以降は60秒デバウンス）
    db()
    if "--organize" in sys.argv:
        organize_photos()
        sys.exit(0)
    if "--quality-scan" in sys.argv:
        result = quality_backfill()
        print(f"[quality] 一括解析完了: 解析{result['analyzed']}件 / "
              f"待ち{result['remaining']}件 / 品質更新{'あり' if result['rebuilt'] else 'なし'}")
        sys.exit(0)
    if not DRIVE_KEY.exists():
        print(f"[drive] {DRIVE_KEY.name} なし → Drive直接参照OFF（ローカルのみ）")
    # サーバーを即時起動し、同期はバックグラウンドで実行（初回も含む）
    tile_cache_prune()   # 起動時に1回（以降はworkerが1時間に1回）
    prepare_tmp_dir()    # 原本一時置き場の用意＋残骸の掃除
    threading.Thread(target=worker, daemon=True).start()
    if AI_ENABLED:
        threading.Thread(target=ai_worker, daemon=True).start()
    threading.Thread(target=cache_saver, daemon=True).start()
    print(f"GeoFrame server: http://localhost:{PORT}/  (DB: {DB_PATH.name})")
    print(allowed_hosts_note())      # 403の原因を運用者が追えるように許可内容を出す
    print("[cycle] 同期はバックグラウンドで実行中（DB内の既存写真は即表示可能）")
    print(f"[ai] 自動写真選定: {'ON' if AI_ENABLED else 'OFF'}")
    ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
