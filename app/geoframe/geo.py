"""位置の時刻推定・地理院逆ジオコーディング/標高（server.py L577-L693 より移動）"""
import re
import time
import urllib.parse
import urllib.request

from .config import (GSI_ELEV, GSI_GEOCODE, GSI_RGEO, INFER_OFFSET_S,
                     INFER_WINDOW_S, INFER_WINDOWS_S)
from .store import cache_get, cache_put, dbq, dbx
from .util import http_json, t_epoch


# ============================================================
# 位置の時刻推定・地理院enrich
# ============================================================
def infer_locations():
    """撮影時刻が近いGPS付き写真（錨）から位置を推定する。
    錨は exif/drive_meta（元々GPS付き）に加え、/admin で手動指定した写真（manual）も含む。
    手動で1枚直すと、同じ旅の近い時刻の未解決写真にも波及する。

    窓は INFER_WINDOWS_S の段階窓（既定 ±15分 → ±2時間 → ±12時間）。
    「近い窓から順に適用」は、錨を1回だけ最近傍探索して差 dt を求め、
    dt を含む最小の窓に振り分けるのと数学的に同値なので1パスで処理する
    （複数パスにすると段階間の順序バグを作り込む余地ができるだけで得が無い）。
    どの窓で当たったかは infer_dt_s（錨との差・秒）として残し、/admin で内訳を見る。
    ※loc_source は段階に関わらず 'inferred' のまま。api_photos の
      `loc_source == 'inferred'`・api_stats の集計・下の再評価WHERE句が
      すべてこのリテラルを見ているため、ここを段階別の値にすると
      表示側と再評価が黙って壊れる。段階は必ず別列で表現すること。
    """
    anchors = [(t_epoch(r["taken_at"]), r["lat"], r["lon"], r["id"])
               for r in dbq("""SELECT id, taken_at, lat, lon FROM photos
                               WHERE loc_source IN ('exif','drive_meta','manual')
                               AND taken_at != ''""")]
    anchors = [a for a in anchors if a[0] is not None]
    widest = INFER_WINDOWS_S[-1] if INFER_WINDOWS_S else INFER_WINDOW_S
    n = 0
    for r in dbq("""SELECT id, taken_at, lat FROM photos
                    WHERE loc_source IS NULL OR loc_source='inferred'"""):
        te = t_epoch(r["taken_at"])
        best, dt = None, None
        if te is not None and anchors:
            te += INFER_OFFSET_S           # オフセットの加算は1回だけ
            cand = min(anchors, key=lambda a: abs(a[0] - te))
            d = abs(cand[0] - te)
            if d <= widest:                # 最大窓に入らなければ推定しない
                best, dt = cand, d
        if best:
            if r["lat"] is None:
                n += 1
            dbx("""UPDATE photos SET lat=?, lon=?, loc_source='inferred',
                   inferred_from=?, infer_dt_s=? WHERE id=?""",
                (best[1], best[2], best[3], int(dt), r["id"]))
        elif r["lat"] is not None:  # 錨が消えた等 → 推定を取り消し
            dbx("""UPDATE photos SET lat=NULL, lon=NULL, loc_source=NULL,
                   inferred_from=NULL, infer_dt_s=NULL, display=NULL
                   WHERE id=?""", (r["id"],))
    return n


def infer_bucket(dt_s):
    """錨との時間差（秒）→ 段階窓のラベル。/admin の内訳表示用"""
    if dt_s is None:
        return ""
    for w in INFER_WINDOWS_S:
        if dt_s <= w:
            return f"±{int(w // 60)}分" if w < 3600 else f"±{int(w // 3600)}時間"
    return "範囲外"


# ============================================================
# 地名・住所検索（順ジオコーディング）。/admin の手動位置指定UI用
# ============================================================
GEOCODE_MIN_LEN = 2         # 1文字等の短すぎる入力は無駄なリクエストになるので弾く
GEOCODE_MAX_RESULTS = 10


def geocode_search(query):
    """地名・住所 → 座標候補のリスト。地理院のアドレス検索APIを使う（認証不要・無料）。
    日本国内の住所・地名のみ対応（海外地名は0件になる。既知の制約 — 写真自体は
    OSMタイルで世界中表示できるが、検索のショートカットは国内限定）。
    上流エラー・タイムアウト・想定外の応答形式は例外を握って空リストにする
    （検索結果ゼロ扱いにするだけで、/admin 全体を落とさない）"""
    q = (query or "").strip()
    if len(q) < GEOCODE_MIN_LEN:
        return []
    try:
        data = http_json(GSI_GEOCODE.format(q=urllib.parse.quote(q)))
        if not isinstance(data, list):
            return []
        out = []
        for feat in data[:GEOCODE_MAX_RESULTS]:
            geom = (feat or {}).get("geometry") or {}
            coords = geom.get("coordinates")
            if not (isinstance(coords, list) and len(coords) == 2):
                continue
            lon, lat = coords
            if not (isinstance(lat, (int, float)) and isinstance(lon, (int, float))):
                continue
            title = ((feat.get("properties") or {}).get("title") or "").strip()
            out.append({"title": title or q, "lat": float(lat), "lon": float(lon)})
        return out
    except Exception as e:
        print(f"[geo] 地名検索失敗（q={q!r}）: {e}")
        return []


MUNI_URL = "https://maps.gsi.go.jp/js/muni.js"


def muni_table():
    """市区町村コード → (都道府県名, 市区町村名)。地理院muni.jsから一度だけ構築"""
    t = cache_get("muni_table")
    if not t:
        t = {}
        try:
            req = urllib.request.Request(MUNI_URL, headers={"User-Agent": "GeoFrame/0.2"})
            with urllib.request.urlopen(req, timeout=15) as r:
                text = r.read().decode("utf-8", errors="replace")
            for m in re.finditer(r"'(\d+),([^,]+),(\d+),([^']*)'", text):
                t[str(int(m[3]))] = [m[2], m[4].replace("　", "")]
            if t:
                cache_put("muni_table", t)
                print(f"[geo] 自治体テーブル取得: {len(t)}件")
        except Exception as e:
            print(f"[geo] muni.js取得失敗（次サイクルで再試行）: {e}")
    return t


# 日本のおおよその範囲（この外は地理院APIを呼ばずオフライン逆ジオコーダへ）
JP_BBOX = (20.0, 122.0, 46.5, 154.5)


def _rg():
    """海外用オフライン逆ジオコーダ（pip install reverse-geocode 導入時のみ有効）"""
    try:
        import reverse_geocode
        return reverse_geocode
    except ImportError:
        return None


def enrich_rows():
    mt = muni_table()
    rg = _rg()
    for r in dbq("""SELECT id, lat, lon FROM photos
                    WHERE lat IS NOT NULL AND place IS NULL LIMIT 40"""):
        key = f"geo:{r['lat']:.3f},{r['lon']:.3f}"
        in_jp = (JP_BBOX[0] <= r["lat"] <= JP_BBOX[2]
                 and JP_BBOX[1] <= r["lon"] <= JP_BBOX[3])
        if not in_jp:
            # 海外: 地理院は圏外。オフライン解決（無ければ空で確定し、再試行しない）
            g = {"place": "", "elev": None, "muni_cd": "", "pref": "", "city": ""}
            if rg:
                try:
                    res = rg.search([(r["lat"], r["lon"])])[0]
                    g["city"] = res.get("city", "")
                    g["place"] = res.get("country", "")   # タイトルは「都市名 国名」になる
                except Exception:
                    pass
            cache_put(key, g)
            dbx("""UPDATE photos SET place=?, elev=?, muni_cd=?, pref=?, city=? WHERE id=?""",
                (g["place"], g["elev"], "", "", g["city"], r["id"]))
            continue
        hit = cache_get(key)
        if isinstance(hit, dict) and "city" in hit:
            g = hit
        else:
            g = {"place": "", "elev": None, "muni_cd": "", "pref": "", "city": ""}
            # 「通信自体が失敗した」と「応答は返ったが中身が空だった」を区別する。
            # 前者は一時障害なので次サイクルで再試行、後者は海上や地理院のカバー外で
            # 何度問い合わせても埋まらないため、空のまま確定させる。
            # ※両者を同じ扱い（結果が空なら continue）にすると、解決不能な座標が永久に
            #   place IS NULL のまま残り、上のクエリの LIMIT 40 を恒久的に占有して
            #   他の行の地名解決まで止まる（石垣島沖・宮古島周辺・瀬戸内海の座標で
            #   実際に発生した。rgeo が {}、elev が '-----' を返す領域）。
            answered = False
            try:
                data = http_json(GSI_RGEO.format(lat=r["lat"], lon=r["lon"]))
                answered = True        # 応答が返った時点で確定扱い（中身の解釈失敗も含む）
                res = (data or {}).get("results") or {}
                g["place"] = res.get("lv01Nm", "") or ""
                mc = str(res.get("muniCd", "") or "")
                if mc.isdigit():
                    g["muni_cd"] = mc
                    ent = mt.get(str(int(mc)))
                    if ent:
                        g["pref"], g["city"] = ent
            except Exception:
                pass
            try:
                data = http_json(GSI_ELEV.format(lat=r["lat"], lon=r["lon"]))
                answered = True
                ev = (data or {}).get("elevation")
                if isinstance(ev, (int, float)):
                    g["elev"] = round(ev)
            except Exception:
                pass
            if not answered:
                continue   # 両APIとも通信失敗＝一時障害 → キャッシュせず次サイクルで再試行
            cache_put(key, g)   # 空でも確定として記録し、同じ座標を二度と問い合わせない
            time.sleep(0.2)   # 地理院APIへの連続リクエストを抑制
        dbx("""UPDATE photos SET place=?, elev=?, muni_cd=?, pref=?, city=? WHERE id=?""",
            (g["place"], g["elev"], g.get("muni_cd", ""), g.get("pref", ""),
             g.get("city", ""), r["id"]))
