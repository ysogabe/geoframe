"""背景地図タイルのプロキシ（server.py L923-L1051 より移動）"""
import io
import os
import threading
import time
import urllib.error
import urllib.request
from collections import deque

from PIL import Image

from .config import (TILE_CACHE_MAX_MB, TILE_DIR, TILE_FETCH_PER_MIN,
                     TILE_SOURCES, TILE_UA)


# ============================================================
# 背景地図タイルのプロキシ（ファイル永続キャッシュ）
# ============================================================
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
# ソースごとに上流への同時接続数を絞る（OSMのtile usage policy配慮）
_tile_sems = {k: threading.Semaphore(v[2]) for k, v in TILE_SOURCES.items()}
_blank_tile = None
_blank_lock = threading.Lock()
_tile_prune_at = 0.0
# 上流フェッチのスライディング窓（直近60秒の実行時刻）。ThreadingHTTPServerなのでlockで保護する
_tile_fetch_win = deque()
_tile_fetch_lock = threading.Lock()
_tile_throttled = False      # 抑制中フラグ。開始時と解除時に1行ずつだけログを出すために持つ


def _upstream_tile_url(layer, z, x, y):
    return TILE_SOURCES[layer][0].format(z=z, x=x, y=y)


def _check_png(data):
    """キャプティブポータル等のHTMLを拒否する（osm/gsi はPNGで配信される）。"""
    if not data.startswith(PNG_MAGIC):
        raise ValueError(f"PNGではない応答 ({len(data)}B)")
    return data


def _transparent_tile():
    """256×256の透明PNG（欠損タイルの穴埋め）。プロセス起動後1回だけ生成して使い回す"""
    global _blank_tile
    if _blank_tile is None:
        with _blank_lock:
            if _blank_tile is None:
                buf = io.BytesIO()
                Image.new("RGBA", (256, 256), (0, 0, 0, 0)).save(buf, format="PNG")
                _blank_tile = buf.getvalue()
    return _blank_tile


def _tile_fetch_allowed():
    """上流フェッチを1件許可してよいか（直近60秒のスライディング窓）
    ※osm/gsiで枠を分けず全体で1つの窓にする。分けると層を交互に叩くだけで上限が倍になるため。
    ※lockは窓の更新だけに使い、urlopenは必ずlockの外で行う（握ったままI/Oすると全取得が直列化する）。
      判定と記録を同一クリティカルセクションで行わないと、同時ミスが全部ゲートを通り抜ける。"""
    global _tile_throttled
    now = time.time()
    with _tile_fetch_lock:
        while _tile_fetch_win and now - _tile_fetch_win[0] >= 60.0:
            _tile_fetch_win.popleft()
        if len(_tile_fetch_win) >= TILE_FETCH_PER_MIN:
            if not _tile_throttled:
                _tile_throttled = True
                print(f"[tile] 上流フェッチ抑制 開始: 直近60秒で{TILE_FETCH_PER_MIN}枚に到達"
                      f"（GEOFRAME_TILE_FETCH_PER_MIN で調整可）")
            return False
        if _tile_throttled:
            _tile_throttled = False
            print("[tile] 上流フェッチ抑制 解除")
        _tile_fetch_win.append(now)
        return True


def fetch_tile(layer, z, x, y):
    """地図タイル1枚を取得（ファイル永続キャッシュ。エラーは透明タイル扱いでログ）
    戻り値: (PNGバイト列, cache_status)
      "hit"       キャッシュから返した
      "miss"      上流から取得してキャッシュに保存した（404の透明タイル永続化を含む）
      "error"     上流エラー。透明タイルを返すがキャッシュしない（次回再試行させる）
      "throttled" 流量上限に達したため上流へ行かなかった。透明タイルを返しキャッシュしない"""
    f = TILE_DIR / layer / str(z) / str(x) / f"{y}.png"
    if f.exists():
        try:
            return f.read_bytes(), "hit"      # ヒットは無制限（正常な再描画・リロードを妨げない）
        except Exception:
            pass
    url = _upstream_tile_url(layer, z, x, y)
    if not _tile_fetch_allowed():
        # 上限超過。404にするとMapLibreがリトライを連発するので200＋透明タイルで返しきる
        return _transparent_tile(), "throttled"
    try:
        req = urllib.request.Request(url, headers={
            "User-Agent": TILE_UA, "Accept": "image/png"})
        with _tile_sems[layer]:
            with urllib.request.urlopen(req, timeout=10) as r:
                data = r.read()
        data = _check_png(data)
    except urllib.error.HTTPError as e:
        if e.code != 404:
            print(f"[tile] {layer} z{z}/{x}/{y}: {e}")
            return _transparent_tile(), "error"
        # 提供範囲外は恒久的 → 透明タイルを永続化して以後問い合わせない
        data = _transparent_tile()
    except Exception as e:
        print(f"[tile] {layer} z{z}/{x}/{y}: {e}")
        return _transparent_tile(), "error"   # 5xx・タイムアウト・回線断はキャッシュしない
    try:
        f.parent.mkdir(parents=True, exist_ok=True)
        # ThreadingHTTPServerの並行書き込み対策: 同一ディレクトリのtmpへ書いてatomicに差し替える
        # （single-flightロックは張らない。同じタイルを2回取りに行っても実害がないため）
        tmp = f.with_name(f"{y}.{os.getpid()}.{threading.get_ident()}.tmp")
        tmp.write_bytes(data)
        os.replace(tmp, f)
    except Exception as e:
        print(f"[tile] {layer} z{z}/{x}/{y} キャッシュ書込失敗: {e}")
    return data, "miss"


def tile_cache_prune():
    """TILE_DIRの合計が TILE_CACHE_MAX_MB を超えたら mtime の古い順に消して90%まで縮める
    ※地図タイルは基本的に不変なのでTTLによる期限切れ削除はしない（容量のみで管理）"""
    global _tile_prune_at
    _tile_prune_at = time.time()
    if not TILE_DIR.exists():
        return
    files, total = [], 0
    for p in TILE_DIR.rglob("*.png"):
        try:
            st = p.stat()
        except OSError:
            continue
        files.append((st.st_mtime, st.st_size, p))
        total += st.st_size
    limit = TILE_CACHE_MAX_MB * 1024 * 1024
    if total <= limit:
        return
    files.sort()                       # mtimeの古い順
    freed, n = 0, 0
    for _mtime, size, p in files:
        if total - freed <= limit * 0.9:
            break
        try:
            p.unlink()
        except OSError:
            continue
        freed += size
        n += 1
    mb = 1024 * 1024
    print(f"[tile] キャッシュ整理: {n}枚削除 {total // mb}MB → {(total - freed) // mb}MB")
