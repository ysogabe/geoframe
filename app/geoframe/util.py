"""汎用ヘルパー（server.py L268-L271 / L341-L405 / L860-L866 より移動）"""
import json
import re
import urllib.request
from datetime import datetime
from pathlib import Path

from PIL import ExifTags, Image, ImageOps

from .config import DERIV_PX


def http_json(url):
    req = urllib.request.Request(url, headers={"User-Agent": "GeoFrame/0.2"})
    with urllib.request.urlopen(req, timeout=8) as r:
        return json.loads(r.read().decode("utf-8"))


# ============================================================
# EXIF・時刻ユーティリティ
# ============================================================
def norm_time(s):
    """EXIF/Driveの 'YYYY:MM:DD HH:MM:SS' 等を 'YYYY-MM-DD HH:MM' に正規化"""
    m = re.match(r"(\d{4})[:-](\d{2})[:-](\d{2})[ T](\d{2}):(\d{2})", str(s or ""))
    return f"{m[1]}-{m[2]}-{m[3]} {m[4]}:{m[5]}" if m else ""


def t_epoch(s):
    try:
        return datetime.strptime(s, "%Y-%m-%d %H:%M").timestamp()
    except Exception:
        return None


def _rational(v):
    try:
        return float(v)
    except TypeError:
        return float(v[0]) / float(v[1])


def dms_to_deg(dms, ref):
    deg = _rational(dms[0]) + _rational(dms[1]) / 60 + _rational(dms[2]) / 3600
    return -deg if ref in ("S", "W") else deg


def extract_exif(path: Path):
    """ローカルファイルからGPS・撮影時刻を抽出"""
    with Image.open(path) as im:
        exif = im.getexif()
        lat = lon = None
        time_s = ""
        if exif:
            gps = exif.get_ifd(ExifTags.IFD.GPSInfo)
            if gps and 2 in gps and 4 in gps:
                lat = round(dms_to_deg(gps[2], gps.get(1, "N")), 6)
                lon = round(dms_to_deg(gps[4], gps.get(3, "E")), 6)
            ex = exif.get_ifd(ExifTags.IFD.Exif)
            dt = ex.get(36867) or ex.get(36868) or exif.get(306)
            time_s = norm_time(dt)
        return lat, lon, time_s


def haversine_km(lat1, lon1, lat2, lon2):
    from math import asin, cos, radians, sin, sqrt
    a = (sin(radians(lat2 - lat1) / 2) ** 2
         + cos(radians(lat1)) * cos(radians(lat2)) * sin(radians(lon2 - lon1) / 2) ** 2)
    return 6371 * 2 * asin(sqrt(a))


# ============================================================
# derivative生成（表示用2560px JPEG。HEIC→JPEG変換もここ）
# ============================================================
def make_derivative(src: Path, dest: Path):
    with Image.open(src) as im:
        im = ImageOps.exif_transpose(im)
        im.thumbnail((DERIV_PX, DERIV_PX))
        if im.mode not in ("RGB", "L"):
            im = im.convert("RGB")
        dest.parent.mkdir(parents=True, exist_ok=True)
        tmp = dest.with_suffix(".part")
        im.save(tmp, "JPEG", quality=85)
        tmp.rename(dest)


def latlon_to_tile(lat, lon, zoom):
    """緯度経度→XYZタイル座標（整数）と、タイル内相対位置（小数）"""
    from math import cos, log, pi, radians, tan
    n = 2 ** zoom
    xf = n * ((lon + 180) / 360)
    yf = n * (1 - (log(tan(radians(lat)) + 1 / cos(radians(lat))) / pi)) / 2
    return int(xf), int(yf), xf - int(xf), yf - int(yf)
