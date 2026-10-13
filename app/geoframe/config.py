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
CACHE_FILE = DATA_DIR / ".cache.json"      # 地理院ジオコーダ結果の永続キャッシュ
# HTTPの待ち受けポート
PORT = int(os.environ.get("GEOFRAME_PORT", "8080"))
# 開発時は GEOFRAME_HOST=127.0.0.1 でこの端末だけに限定できる。
# 実機の家庭内LAN配信は従来互換の 0.0.0.0 を既定とする。
HOST = os.environ.get("GEOFRAME_HOST", "0.0.0.0")
EXTS = {".jpg", ".jpeg", ".png", ".tif", ".tiff"} | ({".heic", ".heif"} if HEIC else set())
DERIV_PX = 2560
# 同期1サイクルで取り込む写真の枚数。初回のDrive全件取り込みもこの数ずつ進む
FETCH_PER_CYCLE = int(os.environ.get("GEOFRAME_FETCH_PER_CYCLE", "25"))
# 品質解析も同期1サイクルで処理量を制限する。未指定時はderivative生成数と揃え、
# 通常運用では新しく取り込んだ分を同じサイクル内で追いつけるようにする。
QUALITY_PER_CYCLE = int(os.environ.get("GEOFRAME_QUALITY_PER_CYCLE", str(FETCH_PER_CYCLE)))
# AI写真選定はDrive同期と別スレッドで少量ずつ進める。無効化する場合だけ0を指定する。
AI_ENABLED = os.environ.get("GEOFRAME_AI_ENABLED", "1") == "1"
# AI選定の1サイクルでembeddingする枚数
AI_EMBED_PER_CYCLE = int(os.environ.get("GEOFRAME_AI_EMBED_PER_CYCLE", "12"))
# AI選定の1サイクルでGemini比較まで進めるグループ数
AI_JUDGE_PER_CYCLE = int(os.environ.get("GEOFRAME_AI_JUDGE_PER_CYCLE", "2"))
# ai_workerが次のサイクルまで待つ秒数。30秒未満は30秒へ切り上げる
AI_POLL_S = max(30.0, float(os.environ.get("GEOFRAME_AI_POLL_S", "60")))
# 同一場面候補とみなす撮影時刻の窓（分）。この範囲内の写真だけを類似比較する
AI_TIME_WINDOW_MIN = float(os.environ.get("GEOFRAME_AI_TIME_WINDOW_MIN", "15"))
# 同一場面候補とみなすembeddingのcosine類似度しきい値
AI_SIMILARITY = float(os.environ.get("GEOFRAME_AI_SIMILARITY", "0.82"))
# 1グループでGemini比較に回す最大枚数（2〜6へクランプ）
AI_SHORTLIST_MAX = max(2, min(6, int(os.environ.get("GEOFRAME_AI_SHORTLIST_MAX", "6"))))
# /api/ai/search の流量上限（分あたり）。LAN内の任意ページから連打GETで
# 全embedding行の毎回フルスキャン（3テーブルJOIN、グローバルDBロック内）を
# 誘発できるため、tiles.pyのスライディング窓と同型で抑制する。
AI_SEARCH_PER_MIN = int(os.environ.get("GEOFRAME_AI_SEARCH_PER_MIN", "30"))

# Google Driveサービスアカウント鍵JSONのパス（ROOT＝app/ からの相対）
DRIVE_KEY = ROOT / os.environ.get("GEOFRAME_DRIVE_KEY", "drive_key.json")
# 取り込み対象のDriveフォルダ名
DRIVE_FOLDER = os.environ.get("GEOFRAME_DRIVE_FOLDER", "GeoFrame")
# Driveの変更確認間隔（分）。値は秒へ換算して使う
DRIVE_POLL_S = float(os.environ.get("GEOFRAME_DRIVE_POLL_MIN", "10")) * 60
DRIVE_MIMES = ("image/jpeg", "image/png", "image/heic", "image/heif", "image/tiff")

# 位置推定の錨に使う撮影時刻の窓（分）。この範囲内の位置つき写真から推定する
INFER_WINDOW_S = float(os.environ.get("GEOFRAME_INFER_MIN", "15")) * 60
# カメラ時計のずれ補正（分）。撮影時刻が恒常的にずれる機体で調整する
INFER_OFFSET_S = float(os.environ.get("GEOFRAME_INFER_OFFSET_MIN", "0")) * 60
# 位置推定の段階窓（秒）。近い窓ほど信頼できる推定。
# 1段目は従来どおり GEOFRAME_INFER_MIN で調整可能、2段目以降を固定で足している。
# 「同じ日に同じ場所に居た」程度の粗い推定でも、位置なしで死蔵させるよりは良いという方針。
# 実際にどの窓で当たったかは photos.infer_dt_s（錨との時間差・秒）に記録され、
# /admin で内訳を確認できる（loc_source は 'inferred' のまま＝表示側の互換を壊さない）。
INFER_WINDOWS_S = sorted({INFER_WINDOW_S, 2 * 3600, 12 * 3600})

# 8地方区分（管理画面の都道府県選択の正本）
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
JP_PREFS = [p for ps in PREF_REGIONS.values() for p in ps]   # 47件

# 背景地図タイルのプロキシ＋ローカルキャッシュ（管理ページの位置修正用の地図）
TILE_DIR = DATA_DIR / "tile_cache"
# layer -> (URLテンプレート, maxzoom, 上流への同時接続数)
TILE_SOURCES = {
    "osm": ("https://tile.openstreetmap.org/{z}/{x}/{y}.png", 19, 2),
    "gsi": ("https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png", 18, 4),
}
# OSM tile usage policy 準拠に必須。実運用では連絡先入り（例 "GeoFrame/0.1 (me@example.com)"）を
# GEOFRAME_TILE_UA に設定しておくこと。既定値のままだとブロックされうる
TILE_UA = os.environ.get("GEOFRAME_TILE_UA", "GeoFrame/0.1 (personal photo frame)")
# タイルキャッシュの上限（全レイヤー共有）。容量を抑えたい場合は
# GEOFRAME_TILE_CACHE_MAX_MB で上書きする。
TILE_CACHE_MAX_MB = int(os.environ.get("GEOFRAME_TILE_CACHE_MAX_MB", "16384"))
# 上流フェッチ（＝キャッシュミス）だけを対象にした流量上限（枚/分）。キャッシュヒットは無制限。
# /tiles/ は無認証なので、LAN内の任意のWebページが <img src="http://<frame-ip>:8080/tiles/…">
# を大量に並べるだけで上流を叩かせられる（クロスオリジンの<img>はSOPで止まらない）。
# 上限に達したら上流へ行かず透明PNGを返すことで、OSM tile usage policy上の
# bulk downloading とみなされて自宅グローバルIPがBANされる事態とディスク枯渇を防ぐ。
# 既定240の根拠: コールド起動時の1画面が実測116枚。通常の初回描画＋パン1回分に
# 約2倍の余裕を持たせた値（gsiの上流同時接続4でも詰まらない）。
TILE_FETCH_PER_MIN = int(os.environ.get("GEOFRAME_TILE_FETCH_PER_MIN", "240"))

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
