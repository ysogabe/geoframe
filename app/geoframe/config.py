"""定数・環境変数（server.py L56-L174 より移動）"""
import ipaddress
import os
import socket
import sys
from pathlib import Path

try:
    from PIL import ExifTags, Image, ImageOps
except ImportError:
    sys.exit("Pillow が必要です:  pip install Pillow")

HEIC = False
try:
    from pillow_heif import register_heif_opener
    register_heif_opener()
    HEIC = True
except ImportError:
    pass

# ============================================================
# 設定
# ============================================================
# ※このファイルは app/geoframe/ にあるため、server.py 時代と同じ app/ を
#   指すよう parent を1段追加している（元: Path(__file__).parent.resolve()）
ROOT = Path(__file__).parent.parent.resolve()


def load_dotenv():
    """app/.env の KEY=VALUE を環境変数へ（既存の環境変数が優先）"""
    f = ROOT / ".env"
    if f.exists():
        for line in f.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


load_dotenv()

# ============================================================
# データルート（コードとデータの分離）
# ------------------------------------------------------------
# ソース（geoframe/ static/ vendor/）は ROOT 配下、書き換わるデータ
# （catalog.db・derivatives/・photos/・各種キャッシュ）は DATA_DIR 配下に置く。
# 本番では GEOFRAME_DATA_DIR=/var/lib/geoframe のようにデータだけ別の場所へ移せる。
# ※.env の読み込みは ROOT 側のまま。DATA_DIR は .env で指定できる必要があり、
#   .env を DATA_DIR 配下に置くと「読むために DATA_DIR が要る」鶏卵になるため。
# ※derivatives/ photos/ blog_exports/ というディレクトリ名は変更しないこと。
#   DBの display 列が「DATA_DIR/display＝実ファイル」かつ「/display＝配信URL」の
#   二重の意味を持っており、名前を変えると既存の全行が解決不能になる。
DATA_DIR = Path(os.environ.get("GEOFRAME_DATA_DIR") or ROOT / "var").resolve()

# ※PHOTOS_DIR は DATA_DIR 配下であること（drive.py が relative_to(DATA_DIR) で
#   DBに入れる相対パスを作るため、外に出すと索引できない）
PHOTOS_DIR = (DATA_DIR / os.environ.get("GEOFRAME_PHOTOS_DIR", "photos")).resolve()
DB_PATH = DATA_DIR / "catalog.db"
DERIV_DIR = DATA_DIR / "derivatives"
# Drive原本の一時ダウンロード先。以前は derivatives/ 配下に .dl_<drive_id> という名前で
# 置いていたが、そこは静的配信の許可prefix（/derivatives/）の内側なので、
# GET /derivatives/.dl_<drive_id> でフル解像度・EXIF完全保持の原本
# （GPS座標・カメラ機体シリアル・原撮影時刻を含む）がそのまま配信できてしまっていた。
# 許可prefixの外に出すことで、配信経路から構造的に外す。
# ※DATA_DIR配下（＝derivatives/ と同一ファイルシステム）に置くこと。
#   tempfile.gettempdir() 配下だと別デバイスになりうるため、
#   将来 os.replace でderivativeへ差し替える実装にした際に跨デバイスで失敗する
TMP_DIR = DATA_DIR / "tmp"
CACHE_FILE = DATA_DIR / ".cache.json"      # adsbdb・地理院ジオコーダ結果の永続キャッシュ
PORT = int(os.environ.get("GEOFRAME_PORT", "8080"))
# 開発時は GEOFRAME_HOST=127.0.0.1 でこの端末だけに限定できる。
# 実機の家庭内LAN配信は従来互換の 0.0.0.0 を既定とする。
HOST = os.environ.get("GEOFRAME_HOST", "0.0.0.0")
EXTS = {".jpg", ".jpeg", ".png", ".tif", ".tiff"} | ({".heic", ".heif"} if HEIC else set())
DERIV_PX = 2560
FETCH_PER_CYCLE = int(os.environ.get("GEOFRAME_FETCH_PER_CYCLE", "25"))
# 品質解析も同期1サイクルで処理量を制限する。未指定時はderivative生成数と揃え、
# 通常運用では新しく取り込んだ分を同じサイクル内で追いつけるようにする。
QUALITY_PER_CYCLE = int(os.environ.get("GEOFRAME_QUALITY_PER_CYCLE", str(FETCH_PER_CYCLE)))
# AI写真選定はDrive同期と別スレッドで少量ずつ進める。無効化する場合だけ0を指定する。
AI_ENABLED = os.environ.get("GEOFRAME_AI_ENABLED", "1") == "1"
AI_EMBED_PER_CYCLE = int(os.environ.get("GEOFRAME_AI_EMBED_PER_CYCLE", "12"))
AI_JUDGE_PER_CYCLE = int(os.environ.get("GEOFRAME_AI_JUDGE_PER_CYCLE", "2"))
AI_POLL_S = max(30.0, float(os.environ.get("GEOFRAME_AI_POLL_S", "60")))
AI_TIME_WINDOW_MIN = float(os.environ.get("GEOFRAME_AI_TIME_WINDOW_MIN", "15"))
AI_SIMILARITY = float(os.environ.get("GEOFRAME_AI_SIMILARITY", "0.82"))
AI_SHORTLIST_MAX = max(2, min(6, int(os.environ.get("GEOFRAME_AI_SHORTLIST_MAX", "6"))))
# /api/ai/search の流量上限（分あたり）。LAN内の任意ページから連打GETで
# 全embedding行の毎回フルスキャン（3テーブルJOIN、グローバルDBロック内）を
# 誘発できるため、tiles.pyのスライディング窓と同型で抑制する。
AI_SEARCH_PER_MIN = int(os.environ.get("GEOFRAME_AI_SEARCH_PER_MIN", "30"))

DRIVE_KEY = ROOT / os.environ.get("GEOFRAME_DRIVE_KEY", "drive_key.json")
DRIVE_FOLDER = os.environ.get("GEOFRAME_DRIVE_FOLDER", "GeoFrame")
DRIVE_POLL_S = float(os.environ.get("GEOFRAME_DRIVE_POLL_MIN", "10")) * 60
DRIVE_MIMES = ("image/jpeg", "image/png", "image/heic", "image/heif", "image/tiff")

INFER_WINDOW_S = float(os.environ.get("GEOFRAME_INFER_MIN", "15")) * 60
INFER_OFFSET_S = float(os.environ.get("GEOFRAME_INFER_OFFSET_MIN", "0")) * 60
# 位置推定の段階窓（秒）。近い窓ほど信頼できる推定。
# 1段目は従来どおり GEOFRAME_INFER_MIN で調整可能、2段目以降を固定で足している。
# 「同じ日に同じ場所に居た」程度の粗い推定でも、位置なしで死蔵させるよりは良いという方針。
# 実際にどの窓で当たったかは photos.infer_dt_s（錨との時間差・秒）に記録され、
# /admin で内訳を確認できる（loc_source は 'inferred' のまま＝表示側の互換を壊さない）。
INFER_WINDOWS_S = sorted({INFER_WINDOW_S, 2 * 3600, 12 * 3600})

ADSB_URL = os.environ.get("GEOFRAME_ADSB_URL",
                          "https://ads-b.dam-lab.work/tar1090/data/aircraft.json")
ADSB_RECEIVER_URL = ADSB_URL.rsplit("/", 1)[0] + "/receiver.json"
ADSBDB = "https://api.adsbdb.com/v0"
# 海外行き先、および国内だが都道府県解決に一時的に失敗した場合の
# フォールバック半径としてのみ使用する（詳細: adsb.py の3分岐マッチング）
MATCH_KM = float(os.environ.get("GEOFRAME_MATCH_KM", "80"))
MAX_LOOKUPS_PER_POLL = 3
MAX_AIRPORT_LOOKUPS_PER_POLL = 3   # GSI逆ジオコーディング用の予算（adsbdb用とは別枠）
RARE_TYPES = {"A124", "A225", "A400", "B52", "B742", "B748", "C130", "C17", "C5M",
              "E4", "E767", "K35R", "KC46", "U2", "VC25", "B701", "GLF6", "P8"}
MIL_OWNER_WORDS = ("air force", "navy", "army", "coast guard", "government",
                   "police", "self-def", "self def", "military")
EMERGENCY_SQUAWKS = {"7500", "7600", "7700"}

# 8地方区分（都道府県の単純一致では羽田=東京都/成田=千葉県で「関東」判定が
# できないため、地方でグループ化して判定する。用途: adsb.py の自宅地方スキップ）
PREF_REGIONS = {
    "北海道地方": ["北海道"],
    "東北地方": ["青森県", "岩手県", "宮城県", "秋田県", "山形県", "福島県"],
    "関東地方": ["茨城県", "栃木県", "群馬県", "埼玉県", "千葉県", "東京都", "神奈川県"],
    "中部地方": ["新潟県", "富山県", "石川県", "福井県", "山梨県", "長野県",
                "岐阜県", "静岡県", "愛知県"],
    "近畿地方": ["三重県", "滋賀県", "京都府", "大阪府", "兵庫県", "奈良県", "和歌山県"],
    "中国地方": ["鳥取県", "島根県", "岡山県", "広島県", "山口県"],
    "四国地方": ["徳島県", "香川県", "愛媛県", "高知県"],
    "九州・沖縄地方": ["福岡県", "佐賀県", "長崎県", "熊本県", "大分県",
                     "宮崎県", "鹿児島県", "沖縄県"],
}
PREF_TO_REGION = {p: r for r, ps in PREF_REGIONS.items() for p in ps}
JP_PREFS = [p for ps in PREF_REGIONS.values() for p in ps]   # 47件・vendor/pref.geojsonと同順

# 不動産情報ライブラリ（要APIキー）。地震速報時の防災レイヤ表示に使用
MLIT_KEY = os.environ.get("GEOFRAME_MLIT_KEY", "")
MLIT_URL = "https://www.reinfolib.mlit.go.jp/ex-api/external"
HAZARD_ZOOM = 11                    # 2x2タイルで約40km四方をカバー
HZ_DIR = DATA_DIR / "hazard_cache"
# 地震時に意味のある防災系（target_apis番号, エンドポイント, 表示名, zoom, タイルグリッド幅）
# ※洪水(XKT026)・津波(XKT028)はz14以上のみ提供（z11は400。2026-07-29実測）
HAZARD_APIS = [
    (25, "XKT025", "液状化発生傾向", 11, 2),
    (29, "XKT029", "土砂災害警戒区域", 11, 2),
    (22, "XKT022", "急傾斜地崩壊危険", 11, 2),
    (21, "XKT021", "地すべり防止地区", 11, 2),
    (28, "XKT028", "津波浸水想定", 14, 4),
    (26, "XKT026", "洪水浸水想定", 14, 4),
]

# 背景地図タイルのプロキシ＋ローカルキャッシュ
# ※キオスクは回線が不安定になりうる。一度見た地域はオフラインでも表示できるようにする
TILE_DIR = DATA_DIR / "tile_cache"
# layer -> (URLテンプレート, maxzoom, 上流への同時接続数)
TILE_SOURCES = {
    "osm": ("https://tile.openstreetmap.org/{z}/{x}/{y}.png", 19, 2),
    "gsi": ("https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png", 18, 4),
    # 3D PHOTO JOURNEY専用。実URLはズームに応じてGSI_PHOTO_TILE_BANDSから選ぶ。
    # 淡色地図の既存キャッシュと混ざらないよう、独立したlayer名を使う。
    "gsi-photo": ("", 18, 4),
    # 3D地形（Navara raster-dem）専用。実URLはズームに応じてGSI_DEM_TILE_BANDSから選ぶ。
    # 同時接続数はN100トライアル向けに2へ抑制（gsi-photoの4より控えめ）。
    # 上流フェッチの分/枚上限は全layer共有のまま増やさない。
    "gsi-dem": ("", 14, 2),
}
# 国土地理院の公式提供ズームに合わせた航空・衛星写真ピラミッド。
# Z2-13: Landsat 8、Z14-18: 全国最新写真（シームレス）。
GSI_PHOTO_TILE_BANDS = (
    (2, 13, "https://cyberjapandata.gsi.go.jp/xyz/lndst/{z}/{x}/{y}.png"),
    (14, 18, "https://cyberjapandata.gsi.go.jp/xyz/seamlessphoto/{z}/{x}/{y}.jpg"),
)
# 国土地理院の公式標高タイル（Navara raster-dem用）。Z0-8: 全球DEM（demgm_png、
# GMTED2010等由来・世界規模だが粗い）、Z9-14: 数値標高モデル（dem_png、国内10mメッシュ
# 相当・海外や海上は欠測)。エンコードはdemgm_png/dem_pngで共通:
#   x = 65536*R + 256*G + B
#   x <  2^23            => 標高 = x * 0.01m
#   x == 2^23 (=RGB 128,0,0) => 欠測（データなし）
#   x >  2^23            => 標高 = (x - 2^24) * 0.01m
# 欠測はfetch_tileがRGB(128,0,0)で塗り潰したPNGを返す（透明PNGにはしない。
# raster-demデコーダは透明ピクセルを標高0mの実データと区別できないため）。
GSI_DEM_TILE_BANDS = (
    (0, 8, "https://cyberjapandata.gsi.go.jp/xyz/demgm_png/{z}/{x}/{y}.png"),
    (9, 14, "https://cyberjapandata.gsi.go.jp/xyz/dem_png/{z}/{x}/{y}.png"),
)
# OSM tile usage policy 準拠に必須。実運用では連絡先入り（例 "GeoFrame/0.1 (me@example.com)"）を
# GEOFRAME_TILE_UA に設定しておくこと。既定値のままだとブロックされうる
TILE_UA = os.environ.get("GEOFRAME_TILE_UA", "GeoFrame/0.1 (personal photo frame)")
TILE_CACHE_MAX_MB = int(os.environ.get("GEOFRAME_TILE_CACHE_MAX_MB", "2048"))
# 上流フェッチ（＝キャッシュミス）だけを対象にした流量上限（枚/分）。キャッシュヒットは無制限。
# /tiles/ は無認証なので、LAN内の任意のWebページが <img src="http://<frame-ip>:8080/tiles/…">
# を大量に並べるだけで上流を叩かせられる（クロスオリジンの<img>はSOPで止まらない）。
# 上限に達したら上流へ行かず透明PNGを返すことで、OSM tile usage policy上の
# bulk downloading とみなされて自宅グローバルIPがBANされる事態とディスク枯渇を防ぐ。
# 既定240の根拠: コールド起動時の1画面が実測116枚。通常の初回描画＋パン1回分に
# 約2倍の余裕を持たせた値（gsiの上流同時接続4でも詰まらない）。
TILE_FETCH_PER_MIN = int(os.environ.get("GEOFRAME_TILE_FETCH_PER_MIN", "240"))

# Home Assistant連携（.envのHOME_ASSISTANT_API_KEY または GEOFRAME_HA_TOKEN）
HA_URL = os.environ.get("GEOFRAME_HA_URL", "http://homeassistant.local:8123").rstrip("/")
HA_TOKEN = os.environ.get("GEOFRAME_HA_TOKEN") or os.environ.get("HOME_ASSISTANT_API_KEY", "")
HA_MOTION = os.environ.get("GEOFRAME_HA_MOTION_ENTITY", "")   # 例: binary_sensor.display_motion
IDLE_MIN = float(os.environ.get("GEOFRAME_IDLE_MIN", "10"))   # 無人この分数で画面スリープ
SLEEP_CMD = os.environ.get("GEOFRAME_SLEEP_CMD", "")          # 例: xset dpms force off（N100実機用）
WAKE_CMD = os.environ.get("GEOFRAME_WAKE_CMD", "")            # 例: xset dpms force on
# ROOM ENV表示に使うHAエンティティ（カンマ区切り。.envで指定）
HA_ENV_ENTITIES = [e.strip() for e in
                   os.environ.get("GEOFRAME_HA_ENV_ENTITIES", "").split(",") if e.strip()]

# ------------------------------------------------------------
# 画面ON/OFFの手動オーバーライド
# ------------------------------------------------------------
# HA側の input_select（選択肢: auto / force_on / force_off）を既存のポーリングで
# 読むだけの構成にしている。こちら側に書き込み用のHTTPエンドポイントは足さない
# （このサーバーは無認証なので、外から画面を消せる口を増やしたくない）。
# 未設定なら常に auto 相当＝従来の人感センサー任せの挙動に戻る（rollback経路）。
# 例: GEOFRAME_HA_SCREEN_MODE_ENTITY=input_select.geoframe_screen_mode
HA_SCREEN_MODE_ENTITY = os.environ.get("GEOFRAME_HA_SCREEN_MODE_ENTITY", "")
# HAへ疎通できないままこの分数を超えたら、オーバーライドを捨てて auto へ戻す。
# 外出先で force_off にした直後にHAが落ちて画面が二度と点かない、を避ける保険
HA_SCREEN_DECAY_MIN = float(os.environ.get("GEOFRAME_HA_SCREEN_DECAY_MIN", "5"))
# 実機の画面状態を読むコマンド（例: /opt/geoframe/deploy/kiosk/screen-ctl.sh status）。
# ※標準出力の on/off だけを見る。あのスクリプトは正常な「off」判定でも exit 1 を
#   返す仕様なので、終了コードで成否を判定してはいけない
SCREEN_STATUS_CMD = os.environ.get("GEOFRAME_SCREEN_STATUS_CMD", "")
HA_SCREEN_RECONCILE_SEC = float(os.environ.get("GEOFRAME_HA_SCREEN_RECONCILE_SEC", "90"))

# 気象庁（無認証）。GEOFRAME_JMA_AREA: 予報区コード（既定120000=千葉県）
JMA_AREA = os.environ.get("GEOFRAME_JMA_AREA", "120000")
JMA_FORECAST = f"https://www.jma.go.jp/bosai/forecast/data/forecast/{JMA_AREA}.json"
JMA_QUAKES = "https://www.jma.go.jp/bosai/quake/data/list.json"

# 開発用API（/api/ha/entities・/api/mlit/probe）のガード。既定は無効
DEBUG_API = os.environ.get("GEOFRAME_DEBUG_API", "") == "1"

# ============================================================
# Host ヘッダ検証（DNSリバインディング対策）
# ------------------------------------------------------------
# このサーバーには認証が無いため、「ブラウザから見て同一オリジンかどうか」が
# 唯一の防壁になっている。ところが攻撃者が evil.example を8080番で運用し、
# DNSのTTLを極小にしてフレーム機のLAN IPへ再解決させると、ブラウザにとっての
# オリジンは evil.example:8080 のまま中身はこのサーバーになる（DNSリバインディング）。
# この状態では Origin も Host も evil.example:8080 で一致してしまうので、
# do_POST のCSRF2段構え（application/json必須＋Origin一致）は原理的にすり抜ける。
# 書き込みだけでなく GET /api/admin/photos の全件ダンプ（ファイル名・撮影時刻・
# lat/lon・地名・drive_id）や /derivatives/ の画像本体まで攻撃者のJSから読める。
#
# 対策は「Hostヘッダが自分の名前であること」の検証。実際の判定は
# handlers.Handler._host_ok（ルーティングより前の共通入口）で行う。
# ============================================================
def _detect_local_ips():
    """この機体が持つIPアドレス（起動ログの案内用。best-effort）。
    UDPソケットの connect は宛先へパケットを送らない（ルーティング表を引いて
    送信元アドレスを決めるだけ）ので、外部通信は実際には発生しない。
    宛先は TEST-NET-1 / ドキュメンテーション用プレフィクスの到達しないアドレス。
    ※検出に失敗しても機能上の影響は無い（IPリテラル宛は無条件に許可するため）"""
    ips = set()
    for family, probe in ((socket.AF_INET, ("192.0.2.1", 9)),
                          (socket.AF_INET6, ("2001:db8::1", 9))):
        try:
            s = socket.socket(family, socket.SOCK_DGRAM)
            try:
                s.connect(probe)
                ips.add(s.getsockname()[0])
            finally:
                s.close()
        except OSError:
            pass          # そのファミリの経路が無い環境（IPv6無効等）
    try:
        # 複数NICを持つ機体を拾うためホスト名の解決も足す（Pi等では /etc/hosts 参照）
        for info in socket.getaddrinfo(socket.gethostname(), None):
            ips.add(info[4][0])
    except Exception:
        pass              # mDNS環境で失敗しうる。ログの案内が減るだけなので黙って続行
    return {ip.split("%", 1)[0] for ip in ips}      # IPv6のゾーンID（%en0）を落とす


def _parse_allowed_hosts(raw):
    """GEOFRAME_ALLOWED_HOSTS（カンマ区切り）をホスト名の集合にする。
    'geoframe.local:8080' のようにポートが書かれていても無視する
    （ポートは PORT との一致で別途判定するため。サーバーはPORTでしか待ち受けない）"""
    out = set()
    for item in raw.split(","):
        h = item.strip().lower().rstrip(".")
        if not h:
            continue
        if h.startswith("["):                 # [::1]:8080 形式
            h = h[1:].split("]", 1)[0]
        elif h.count(":") == 1:               # name:port / 192.0.2.50:8080
            h = h.split(":", 1)[0]
        if h:
            out.add(h)
    return out


LOCAL_IPS = _detect_local_ips()
# 許可するホスト名。IPリテラル宛（192.0.2.7等）は_host_ok側で無条件に許可するため、
# ここには名前だけを入れる。GEOFRAME_ALLOWED_HOSTS はTailscaleや固定DNS名で
# 運用する場合の追加指定用（例: GEOFRAME_ALLOWED_HOSTS=geoframe.local:8080,frame.lan）
ALLOWED_HOST_NAMES = frozenset(
    {"localhost"} | _parse_allowed_hosts(os.environ.get("GEOFRAME_ALLOWED_HOSTS", "")))


def allowed_hosts_note():
    """起動時に出す1行の案内。運用者が「なぜ403なのか」を追えるように"""
    names = ", ".join(sorted(ALLOWED_HOST_NAMES))
    # 案内として役に立たないアドレスは省く（リンクローカルfe80::/169.254. とループバック）。
    # 許可判定そのものはIPリテラルを無条件に通すので、ここでの絞り込みは表示だけの話
    show = []
    for ip in LOCAL_IPS:
        try:
            a = ipaddress.ip_address(ip)
        except ValueError:
            continue
        if not (a.is_link_local or a.is_loopback):
            show.append(ip)
    ips = ", ".join(sorted(show)) or "(検出できず)"
    return (f"[host] 許可Host: 名前={{{names}}} + IPリテラルは全て許可"
            f"（この機体のIP: {ips}）/ ポートは{PORT}か省略のみ"
            " / 名前を足すには GEOFRAME_ALLOWED_HOSTS=name:port,... ")

GSI_RGEO = "https://mreversegeocoder.gsi.go.jp/reverse-geocoder/LonLatToAddress?lat={lat}&lon={lon}"
GSI_ELEV = "https://cyberjapandata2.gsi.go.jp/general/dem/scripts/getelevation.php?lon={lon}&lat={lat}&outtype=JSON"
# 地名・住所検索（順ジオコーディング）。/admin の手動位置指定で「地名で近くまで飛ぶ」用途。
# 日本国内の住所・地名のみ対応（海外写真には使えない既知の制約）
GSI_GEOCODE = "https://msearch.gsi.go.jp/address-search/AddressSearch?q={q}"
