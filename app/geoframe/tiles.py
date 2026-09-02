"""背景地図タイルのプロキシ（server.py L923-L1051 より移動）"""
import io
import os
import threading
import time
import urllib.error
import urllib.request
from collections import deque

from PIL import Image

from .config import (GSI_DEM_TILE_BANDS, GSI_PHOTO_TILE_BANDS,
                     TILE_CACHE_MAX_MB, TILE_DIR, TILE_FETCH_PER_MIN,
                     TILE_SOURCES, TILE_UA)


# ============================================================
# 背景地図タイルのプロキシ（ファイル永続キャッシュ）
# ============================================================
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
JPEG_MAGIC = b"\xff\xd8\xff"
# ソースごとに上流への同時接続数を絞る（OSMのtile usage policy配慮）
_tile_sems = {k: threading.Semaphore(v[2]) for k, v in TILE_SOURCES.items()}
_blank_tile = None
_blank_lock = threading.Lock()
_tile_prune_at = 0.0
# 上流フェッチのスライディング窓（直近60秒の実行時刻）。ThreadingHTTPServerなのでlockで保護する
_tile_fetch_win = deque()
_tile_fetch_lock = threading.Lock()
_tile_throttled = False      # 抑制中フラグ。開始時と解除時に1行ずつだけログを出すために持つ


def _band_tile_url(bands, label, z, x, y):
    """min/max zoom帯からURLテンプレートを選ぶ（gsi-photo/gsi-demで共有）。"""
    for min_zoom, max_zoom, template in bands:
        if min_zoom <= z <= max_zoom:
            return template.format(z=z, x=x, y=y)
    raise ValueError(f"{label}の提供ズーム外: z{z}")


def _upstream_tile_url(layer, z, x, y):
    """layerとzoomから上流URLを決める。3D写真・DEMはGSIの公式ズーム帯を継ぎ合わせる。"""
    if layer == "gsi-photo":
        return _band_tile_url(GSI_PHOTO_TILE_BANDS, "gsi-photo", z, x, y)
    if layer == "gsi-dem":
        return _band_tile_url(GSI_DEM_TILE_BANDS, "gsi-dem", z, x, y)
    return TILE_SOURCES[layer][0].format(z=z, x=x, y=y)


def _normalise_tile_bytes(data):
    """クライアント向けPNGへ正規化する。GSIシームレス写真のJPEGも受け入れる。"""
    if data.startswith(PNG_MAGIC):
        return data
    if not data.startswith(JPEG_MAGIC):
        raise ValueError(f"画像ではない応答 ({len(data)}B)")
    with Image.open(io.BytesIO(data)) as image:
        image.load()
        if image.size != (256, 256):
            raise ValueError(f"想定外のタイルサイズ: {image.size[0]}x{image.size[1]}")
        buf = io.BytesIO()
        image.convert("RGB").save(buf, format="PNG", compress_level=3)
        return buf.getvalue()


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


DEM_NO_DATA_RGB = (128, 0, 0)  # GSI標高エンコードの欠測値: x=128*65536=2^23 => 「データなし」
_dem_no_data_tile = None
_dem_no_data_lock = threading.Lock()


def _dem_no_data_tile_bytes():
    """256×256のDEM欠測タイル（RGB 128,0,0で塗り潰したPNG）。
    透明PNGにはしない: raster-demデコーダは透明ピクセルを標高0mの実データと区別できず
    地形が不自然に落ち込む。GSIの欠測エンコードそのものを返すことで、フロント側の
    JAPAN_GSI_ELEVATION_DECODERが「データなし」と正しく判定できるようにする。"""
    global _dem_no_data_tile
    if _dem_no_data_tile is None:
        with _dem_no_data_lock:
            if _dem_no_data_tile is None:
                buf = io.BytesIO()
                Image.new("RGB", (256, 256), DEM_NO_DATA_RGB).save(buf, format="PNG")
                _dem_no_data_tile = buf.getvalue()
    return _dem_no_data_tile


def _placeholder_tile(layer):
    """欠損時に返す代替タイル。gsi-demだけは透明ではなくRGB(128,0,0)欠測エンコードを返す。"""
    return _dem_no_data_tile_bytes() if layer == "gsi-dem" else _transparent_tile()


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
    """地図タイル1枚を取得（ファイル永続キャッシュ。エラーは代替タイル扱いでログ）
    代替タイルはlayerが gsi-dem かどうかで変わる（_placeholder_tile）:
    通常layerは透明PNG、gsi-demはRGB(128,0,0)のDEM欠測エンコード。
    戻り値: (PNGバイト列, cache_status)
      "hit"       キャッシュから返した
      "miss"      上流から取得してキャッシュに保存した（404の代替タイル永続化を含む）
      "error"     上流エラー。代替タイルを返すがキャッシュしない（次回再試行させる）
      "throttled" 流量上限に達したため上流へ行かなかった。代替タイルを返しキャッシュしない"""
    f = TILE_DIR / layer / str(z) / str(x) / f"{y}.png"
    if f.exists():
        try:
            return f.read_bytes(), "hit"      # ヒットは無制限（正常な再描画・リロードを妨げない）
        except Exception:
            pass
    try:
        url = _upstream_tile_url(layer, z, x, y)
    except ValueError:
        # 提供ズーム外（gsi-demならz>14など）は上流へ行けない恒久欠測。
        # 分/枚の共有予算チェックより先に返すことで、上流へ行かない要求が
        # 予算を消費しないようにする（スライディング窓の枯渇を防ぐ）。
        return _placeholder_tile(layer), "error"
    if not _tile_fetch_allowed():
        # 上限超過。404にするとMapLibreがリトライを連発するので200＋代替タイルで返しきる
        return _placeholder_tile(layer), "throttled"
    try:
        req = urllib.request.Request(url, headers={
            "User-Agent": TILE_UA, "Accept": "image/jpeg,image/png,image/*;q=0.8"})
        with _tile_sems[layer]:
            with urllib.request.urlopen(req, timeout=10) as r:
                data = r.read()
        # キャプティブポータル等のHTMLを拒否し、JPEG配信の航空写真もPNGへ統一する。
        # gsi-demは常にPNGで届く（DEMタイルにJPEG配信は無い）ため、そのままraw bytesを通す。
        data = _normalise_tile_bytes(data)
    except urllib.error.HTTPError as e:
        if e.code != 404:
            print(f"[tile] {layer} z{z}/{x}/{y}: {e}")
            return _placeholder_tile(layer), "error"
        # 提供範囲外は恒久的 → 代替タイルを永続化して以後問い合わせない
        data = _placeholder_tile(layer)
    except Exception as e:
        print(f"[tile] {layer} z{z}/{x}/{y}: {e}")
        return _placeholder_tile(layer), "error"   # 5xx・タイムアウト・回線断はキャッシュしない
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
