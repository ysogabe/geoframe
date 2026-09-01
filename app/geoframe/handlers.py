"""HTTPサーバーの Handler（server.py L1505-L1706 より移動）"""
import ipaddress
import json
import os
import posixpath
import re
import traceback
import urllib.parse
from http.server import SimpleHTTPRequestHandler

# _presence / _room_env は可変なモジュール変数のため、値ではなくモジュール属性で参照する
from . import ha
from .admin import (BadRequest, NotFound, admin_setflag, admin_setloc,
                    admin_sethome, api_admin_home, api_admin_photos, api_admin_status)
from .adsb import api_aircraft
from .ai_search import (AiSearchDisabled, AiSearchRateLimited,
                        AiSearchUnavailable, api_ai_search, api_ai_status)
from .config import (ALLOWED_HOST_NAMES, DATA_DIR, DEBUG_API, PORT, ROOT,
                     TILE_SOURCES)
from .geo import geocode_search
from .ha import ha_get
from .jma import api_env
from .mlit import api_hazard, api_intel, hazard_tile
from .photos import api_onthisday, api_photos, api_stats
from .tiles import fetch_tile
from .travel_blog import (BlogBadRequest, BlogExportBusy, BlogNotFound,
                          api_blog_candidates, api_blog_draft, api_blog_trip,
                          api_blog_trips, export_blog, save_blog_draft)
from .util import latlon_to_tile


# ============================================================
# HTTPサーバー
# ============================================================
class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=str(ROOT), **kw)

    def end_headers(self):
        # 静的ファイルの Cache-Control 方針（キオスク運用で実際に踏んだ事故の修正）:
        #   常時起動のChromiumがサーバー側コード更新後の深夜リロードでも旧JS/CSSの
        #   ままになる実害があった。原因は「HTML以外の静的ファイルにCache-Controlを
        #   一切付けていない」ことで、ヘッダが無いとブラウザはLast-Modifiedから
        #   ヒューリスティックに有効期限を推測し、再検証せずキャッシュを使ってしまう。
        #
        #   HTML（/, /index.html, /admin→static/admin.html）・/static/ 配下(JS/CSS)・
        #   /vendor/ 配下は "no-cache" にする。
        #   no-store（キャッシュ自体を禁止し常にフル転送させる）ではなく
        #   no-cache（キャッシュはするが毎回サーバーに再検証＝If-Modified-Sinceを
        #   要求させる）を選ぶ理由: SimpleHTTPRequestHandlerが既にLast-Modifiedを
        #   付与しているため、ファイルが変わっていなければ304 Not Modified
        #   （本文転送なし）で返せ、帯域やキオスク機の低速SDカード読み出しを
        #   浪費しない。一方でファイルが変わっていれば必ず新しい本文が返るため、
        #   上記の「更新が反映されない」事故を防げる。
        #
        #   /vendor/ の扱い: maplibre-gl-4.7.1.min.js のようにファイル名に
        #   バージョンが入っているものは本来 max-age の長期キャッシュでも安全だが、
        #   pref.geojson と fonts.css はバージョン無しファイル名のまま更新され得る。
        #   prefix単位でファイルごとに例外を分けると、この2つで同じ「無言のまま
        #   古いキャッシュが使われ続ける」事故が再発しうる。/vendor/ 配下は
        #   数百KB程度・アクセス頻度も低くREQ毎の304検証コストは無視できるため、
        #   安全側に倒して /static/ と同じ no-cache に統一する。
        #
        #   /derivatives/ と /photos/（写真本体）は内容不変で重いため対象外
        #   （Cache-Controlを付けずブラウザのヒューリスティックキャッシュに任せる、
        #   従来どおりの挙動）。
        #   /tiles/ は _serve_tile が個別に Cache-Control / X-Tile-Cache を
        #   付与済みで、この関数は静的ファイル配信時にしか通らないため無関係。
        #   /api/ は _send_json が Cache-Control: no-store を個別に付与している。
        #
        # 注: 不正リクエスト時はparse前でself.pathが存在しないためgetattrで防御
        p = getattr(self, "path", "").split("?")[0]
        is_html = p in ("/", "/index.html") or p.endswith(".html")
        if is_html or p.startswith("/static/") or p.startswith("/vendor/"):
            self.send_header("Cache-Control", "no-cache")
        if is_html:
            # クリックジャッキング対策。無認証なので、透明にした<iframe>で /admin を
            # 重ねて利用者に誘導クリックさせると、同一オリジン扱いのまま
            # setflag（全件hidden化）等が実行できてしまう。
            # 本体画面（キオスク）も管理ページもiframeに入れて使う想定は無いため一律DENY。
            # ※X-Frame-Optionsを無視する新しめのブラウザ向けにCSPも併記する
            self.send_header("X-Frame-Options", "DENY")
            self.send_header("Content-Security-Policy", "frame-ancestors 'none'")
        super().end_headers()

    def _send_json(self, obj, status=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _debug_api_ok(self):
        """開発用APIのガード。GEOFRAME_DEBUG_API=1 のときだけ通す"""
        if DEBUG_API:
            return True
        self._send_json({"ok": False, "error": "debug API disabled"}, status=403)
        return False

    # 静的配信はホワイトリスト方式（ブラックリストは追加ファイルの度に穴が開くため）。
    # ここに無いパスは一律403 ⇒ .env / server.py / catalog.db / .venv/ 等が自動的に守られる
    STATIC_ALLOW_PREFIXES = ("/static/", "/vendor/", "/derivatives/", "/photos/",
                             "/blog_exports/")
    STATIC_ALLOW_EXACT = frozenset({"/index.html", "/favicon.ico"})

    # 許可prefixのうち、実ファイルが ROOT ではなく DATA_DIR 配下にあるもの
    # （コードとデータの分離。詳細は config.DATA_DIR のコメント）。
    # 残りの /static/ /vendor/ /index.html はソース側なので従来どおり ROOT 配下。
    DATA_ALLOW_PREFIXES = ("/derivatives/", "/photos/", "/blog_exports/")

    # 管理ページ。正規化後の完全一致だけを内部で /static/admin.html に読み替える。
    # ※prefix一致にしないこと（/admin/../.env のようなパスまで通ってしまう）。
    #   normpath により "/admin" と "/admin/" は同じ "/admin" になり、
    #   "/admin/../.env" は "/.env" になるのでここには入らず従来どおり403
    ADMIN_PAGE_FILE = "/static/admin.html"
    BLOG_PAGE_FILE = "/static/blog-admin.html"
    AI_SEARCH_PAGE_FILE = "/static/ai-search.html"

    def _admin_page_rewrite(self):
        """/admin ・ /admin/ なら self.path を admin.html へ差し替えて True を返す。
        差し替え後は既存のホワイトリスト判定（/static/ 配下＝許可済み）をそのまま通る"""
        if self._norm_path(self.path) == "/admin":
            self.path = self.ADMIN_PAGE_FILE
            return True
        return False

    def _blog_page_rewrite(self):
        """/blog-admin を旅行ブログのレビュー画面へ読み替える。"""
        if self._norm_path(self.path) == "/blog-admin":
            self.path = self.BLOG_PAGE_FILE
            return True
        return False

    def _ai_search_page_rewrite(self):
        """/ai-search を管理画面内のAI写真検索PoCへ読み替える。"""
        if self._norm_path(self.path) == "/ai-search":
            self.path = self.AI_SEARCH_PAGE_FILE
            return True
        return False

    TILE_RE = re.compile(r"^/tiles/(osm|gsi|gsi-photo)/(\d{1,2})/(\d+)/(\d+)\.png$")

    # ============================================================
    # Host ヘッダ検証（DNSリバインディング対策。詳しい攻撃の説明は config.py 側）
    # ------------------------------------------------------------
    # do_GET / do_HEAD / do_POST の「ルーティングより前」の共通入口で検査する。
    # _csrf_ok の中だけに入れると書き込み（setflag/setloc）は塞げるが、
    # 被害の大半を占める読み取り（/api/admin/photos の全件ダンプ・
    # /derivatives/ の画像本体）が残ってしまうため、必ず入口に置く。
    #
    # 判定の整理:
    #   - ホスト部がIPリテラルなら無条件に許可。
    #     IPで直接アクセスするにはブラウザにIPを打ち込む必要があり、DNSを一切
    #     引かない＝リバインディングの経路にならない。この一本でLAN内の他端末
    #     からのアクセスが全NIC・IPv6ともに漏れなく通る（自機IPの列挙に頼らない
    #     ので、NIC追加やDHCPでのアドレス変化でも403にならない）。
    #   - ホスト名は許可リスト（localhost + GEOFRAME_ALLOWED_HOSTS）と突き合わせる。
    #     攻撃者のドメイン名はここに無いので403になる。
    #   - ポート部は PORT と一致するか省略なら許可
    #     （Host: 192.0.2.7:8080とHost: 192.0.2.7の両方を通す）。
    #   - Hostヘッダが無い（または空）場合は許可する。HTTP/1.0の素のリクエストや
    #     監視スクリプト・curl を壊さないため。ブラウザはHostを必ず送るので、
    #     リバインディング対策としてはこれで十分。
    # ============================================================
    @staticmethod
    def _split_host_port(raw):
        """Hostヘッダを (ホスト部, ポート部) に分解する。ポート未指定なら ''"""
        h = raw.strip()
        if h.startswith("["):                 # IPv6リテラル: [::1] / [::1]:8080
            host, _, rest = h[1:].partition("]")
            return host, (rest[1:] if rest.startswith(":") else "")
        if h.count(":") == 1:                 # name:port / 192.0.2.7:8080
            host, _, port = h.partition(":")
            return host, port
        return h, ""                          # ポート無し（ブラケット無しIPv6もここ）

    def _host_ok(self):
        """Hostヘッダを検証する。不許可なら403を返して False"""
        raw = self.headers.get("Host")
        if raw is None or not raw.strip():
            return True                       # 上のコメント参照（Hostなし・空は許可）
        host, port = self._split_host_port(raw)
        if port not in ("", str(PORT)):
            return self._deny_host(raw)
        try:
            ipaddress.ip_address(host.split("%", 1)[0])   # ゾーンID（%en0）を除去
            return True                       # IPリテラル＝DNSを引かないので常に許可
        except ValueError:
            pass                              # ホスト名なので許可リストと突き合わせる
        if host.lower().rstrip(".") in ALLOWED_HOST_NAMES:
            return True
        return self._deny_host(raw)

    def _deny_host(self, raw):
        # ログを先に出す。send_error はクライアントが既に切断していると
        # BrokenPipeError で抜けるため、後に置くと「拒否したのにログが無い」状態に
        # なりうる（＝運用者が403の原因を追えない。まさに追いたいケースで欠ける）
        print(f"[host] 拒否: Host={raw!r} （GEOFRAME_ALLOWED_HOSTS で追加できます）")
        # send_error の message はステータス行に入るためASCIIのみ
        # （latin-1で厳格にエンコードされる）。日本語の説明は explain 側に置く
        self.send_error(403, "Forbidden host",
                        "許可されていないHostヘッダです。LAN内の他端末からホスト名で"
                        "開く場合は、環境変数 GEOFRAME_ALLOWED_HOSTS に追加してください。")
        return False

    @staticmethod
    def _norm_path(raw):
        """クエリ・フラグメントを落とし、URLデコード後に正規化したパスを返す。
        ※判定は必ずデコード後に行う（%2e%2e 等でホワイトリストを迂回されないように）"""
        p = raw.split("?", 1)[0].split("#", 1)[0]
        try:
            p = urllib.parse.unquote(p, errors="surrogatepass")
        except UnicodeDecodeError:
            p = urllib.parse.unquote(p)
        p = p.replace("\\", "/")
        if "\x00" in p:
            return ""
        if not p.startswith("/"):
            p = "/" + p
        return posixpath.normpath(p)      # ".." を解決し、末尾スラッシュも落とす

    def translate_path(self, path):
        """URLパスを実ファイルパスへ。DATA_ALLOW_PREFIXES 配下だけ DATA_DIR を基準にする。

        判定は _norm_path 済み（URLデコード＋".."解決済み）のパスで行う。
        生のパスで startswith を見ると /derivatives/%2e%2e/.env のような入力が
        DATA_DIR側の分岐に入ってしまうため。正規化後は "/.env" になるので
        ここには入らず、従来どおり ROOT 側（＝ホワイトリスト外なので403）へ回る。
        ※末尾に "/" を足して比較するのは "/derivatives" 単体（ディレクトリ自体）も
        　DATA_DIR側へ寄せるため。ROOT側に流すと実体が無く404になり、
        　_static_allowed の isdir 判定によるリスティング封鎖（403）が外れてしまう。
        実際のパス組み立ては親クラスに委譲する（".."やパス区切りを含む要素を
        捨てる封じ込めをそのまま使うため）。self.directory は translate_path が
        呼ばれた時点で読まれるので、差し替えて呼べば基準だけを変えられる。
        """
        if (self._norm_path(path) + "/").startswith(self.DATA_ALLOW_PREFIXES):
            saved = self.directory
            self.directory = str(DATA_DIR)
            try:
                return super().translate_path(path)
            finally:
                self.directory = saved
        return super().translate_path(path)

    def _static_allowed(self, raw):
        """静的ファイルとして配信してよいパスか（do_GET / do_HEAD 共通の判定）"""
        p = self._norm_path(raw)
        if not p.startswith("/"):
            return False
        if p == "/":
            p = "/index.html"
        # ドットファイルは拒否する。旧実装（FORBIDDEN_NAMES方式）にあった判定が
        # ホワイトリスト化の際に落ちていたもの。許可prefix配下に置かれた隠しファイルは
        # 「配信対象ではない作業用ファイル」なので、prefixの内側でも一律で塞ぐ。
        # 例: /derivatives/.dl_<drive_id>（Drive原本の一時ファイル。フル解像度・
        #     EXIF完全保持でGPS座標やカメラ機体シリアルを含む）
        # ※存在確認（isdir）より前に置くので、実在しないドットファイルも同じく403。
        #   ファイルの有無が応答の違いから漏れない
        # basenameだけでなく途中の隠しディレクトリも拒否する。旅行ブログの書き出しは
        # /blog_exports/.<trip>-<temp>/ を作業領域にするため、処理途中の成果物や
        # 差し替え前バックアップをHTTPから読めないことが重要。
        if any(part.startswith(".") for part in p.split("/") if part):
            return False
        if not (p in self.STATIC_ALLOW_EXACT
                or p.startswith(self.STATIC_ALLOW_PREFIXES)):
            return False
        # 許可prefix配下でもディレクトリ自体は配信しない（リスティング封鎖）
        if os.path.isdir(self.translate_path(p)):
            return False
        return True

    def _serve_tile(self, path, head_only=False):
        """背景地図タイルを配信（上流取得はfetch_tile、失敗しても200＋透明PNG）
        ※404を返すとMapLibreがリトライを連発するため、取得失敗も200で返しきる"""
        m = self.TILE_RE.match(path)
        if not m:
            self.send_error(404)
            return
        layer, z, x, y = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4))
        if z > TILE_SOURCES[layer][1] or x >= 2 ** z or y >= 2 ** z:
            self.send_error(404)      # ズーム範囲外・タイル座標範囲外
            return
        body, status = fetch_tile(layer, z, x, y)
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control",
                         "no-store" if status in ("error", "throttled")
                         else "public, max-age=86400")
        # デバッグ用（hit|miss|error|throttled）。throttledは流量上限で上流へ行かなかった印
        self.send_header("X-Tile-Cache", status)
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def do_HEAD(self):
        if not self._host_ok():      # ルーティングより前（読み取りも塞ぐため）
            return
        # タイルは実ファイルではないのでHEADも自前で応答する（既定実装だと404になる）
        p = self.path.split("?")[0]
        if p.startswith("/tiles/"):
            self._serve_tile(p, head_only=True)
            return
        self._admin_page_rewrite()      # HEADでも /admin を通す（GETと挙動を揃える）
        self._blog_page_rewrite()
        self._ai_search_page_rewrite()
        # GETと同じホワイトリストを通す（HEADだけ素通しだと存在有無が漏れる）
        if not self._static_allowed(self.path):
            self.send_error(403)
            return
        if p == "/":
            self.path = "/index.html"
        super().do_HEAD()

    def do_GET(self):
        if not self._host_ok():      # ルーティングより前（読み取りも塞ぐため）
            return
        # /tiles/ と /api/ は実ファイルではないので専用ハンドラへ先に分岐する。
        # それ以外の静的パスは末尾のホワイトリスト判定を必ず通る
        p = self.path.split("?")[0]
        if p.startswith("/tiles/"):
            self._serve_tile(p)
            return
        if self.path.startswith("/api/presence"):
            self._send_json(ha._presence)
            return
        if self.path.startswith("/api/mlit/probe"):
            # 開発用: 任意XKTタイルの件数と属性スキーマを確認する
            if not self._debug_api_ok():
                return
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                xkt = q["xkt"][0]
                lat, lon = float(q["lat"][0]), float(q["lon"][0])
                z = int(q.get("z", ["13"])[0])
                if not re.fullmatch(r"(XKT|XPT|XIT|XCT)\d{3}", xkt):
                    raise ValueError("bad xkt")
                x, y, _, _ = latlon_to_tile(lat, lon, z)
                yr = q.get("year", [None])[0]
                d = hazard_tile(xkt, z, x, y, f"year={int(yr)}" if yr else "")
                feats = d.get("features", [])
                self._send_json({"ok": True, "xkt": xkt, "z": z, "count": len(feats),
                                 "sample": (feats[0].get("properties") if feats else None)})
            except Exception as e:
                self._send_json({"ok": False, "error": str(e)})
            return
        if self.path.startswith("/api/roomenv"):
            self._send_json(ha._room_env)
            return
        if self.path.startswith("/api/ha/entities"):
            # HAの全エンティティ一覧（LAN内のHAへserver.pyが代理アクセス）
            # ※自宅住所を含む全エンティティが出るため開発時専用に限定する
            if not self._debug_api_ok():
                return
            try:
                states = ha_get("/api/states")
                ents = [{"entity_id": s.get("entity_id", ""),
                         "state": s.get("state", ""),
                         "name": (s.get("attributes") or {}).get("friendly_name", ""),
                         "unit": (s.get("attributes") or {}).get("unit_of_measurement", ""),
                         "device_class": (s.get("attributes") or {}).get("device_class", ""),
                         } for s in states]
                self._send_json({"ok": True, "count": len(ents), "entities": ents})
            except Exception as e:
                self._send_json({"ok": False, "error": str(e)})
            return
        if self.path.startswith("/api/aircraft"):
            self._send_json(api_aircraft())
            return
        if self.path.startswith("/api/photos"):
            self._send_json(api_photos())
            return
        if self.path.startswith("/api/onthisday"):
            try:
                self._send_json(api_onthisday())
            except Exception as e:
                self._send_json({"ok": False, "error": str(e), "items": []})
            return
        if self.path.startswith("/api/stats"):
            self._send_json(api_stats())
            return
        if self.path.startswith("/api/env"):
            self._send_json(api_env())
            return
        if self.path.startswith("/api/hazard"):
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                self._send_json(api_hazard(float(q["lat"][0]), float(q["lon"][0])))
            except (KeyError, ValueError):
                self._send_json({"ok": False, "reason": "bad_params"})
            return
        if self.path.startswith("/api/intel"):
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                self._send_json(api_intel(float(q["lat"][0]), float(q["lon"][0])))
            except (KeyError, ValueError):
                self._send_json({"ok": False, "reason": "bad_params"})
            return
        if self.path.startswith("/api/admin/status"):
            self._send_json(api_admin_status())
            return
        if self.path.startswith("/api/admin/home"):
            self._send_json(api_admin_home())
            return
        if self.path.startswith("/api/admin/photos"):
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                self._send_json(api_admin_photos(
                    page=int(q.get("page", ["1"])[0]),
                    per=int(q.get("per", ["50"])[0]),
                    filt=q.get("filter", ["all"])[0]))
            except (ValueError, TypeError):
                self._send_json({"ok": False, "error": "bad_params"}, status=400)
            return
        if self.path.startswith("/api/admin/geocode"):
            # 地名・住所検索（順ジオコーディング）。読み取り専用・状態変更なしなので
            # do_POST側のCSRF/DEBUG_APIガードは不要（地名→座標の変換だけで個人情報も返さない）
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            self._send_json({"ok": True, "items": geocode_search(q.get("q", [""])[0])})
            return
        if p == "/api/ai/status":
            self._send_json(api_ai_status())
            return
        if p == "/api/ai/search":
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                include_hidden = q.get("include_hidden", ["0"])[0] in ("1", "true")
                self._send_json(api_ai_search(
                    q.get("q", [""])[0],
                    group_limit=int(q.get("groups", ["12"])[0]),
                    include_hidden=include_hidden))
            except (ValueError, TypeError) as e:
                self._send_json({"ok": False, "error": str(e)}, status=400)
            except AiSearchRateLimited as e:
                self._send_json({"ok": False, "error": str(e)}, status=429)
            # AiSearchDisabled は AiSearchUnavailable のサブクラスなので必ず先に置く
            # （順序を逆にすると常に親の節へ吸われ、専用メッセージが出なくなる）
            except AiSearchDisabled as e:
                print(f"[ai-search] {e}")
                self._send_json({"ok": False, "error": "AI検索は無効化されています"}, status=503)
            except AiSearchUnavailable as e:
                print(f"[ai-search] {e}")
                self._send_json({"ok": False,
                                 "error": "AI検索サービスへ接続できません。しばらくして再試行してください"},
                                status=503)
            return
        if p.startswith("/api/blog/"):
            from urllib.parse import parse_qs, urlparse
            q = parse_qs(urlparse(self.path).query)
            try:
                if p == "/api/blog/trips":
                    self._send_json(api_blog_trips())
                elif p == "/api/blog/trip":
                    self._send_json(api_blog_trip(q.get("trip_id", [""])[0]))
                elif p == "/api/blog/candidates":
                    self._send_json(api_blog_candidates(q.get("trip_id", [""])[0]))
                elif p == "/api/blog/draft":
                    self._send_json(api_blog_draft(q.get("trip_id", [""])[0]))
                else:
                    self.send_error(404)
            except BlogBadRequest as e:
                self._send_json({"ok": False, "error": str(e)}, status=400)
            except BlogNotFound as e:
                self._send_json({"ok": False, "error": str(e)}, status=404)
            return
        # 管理ページ本体（/admin → static/admin.html）。以降は通常のホワイトリスト判定
        self._admin_page_rewrite()
        self._blog_page_rewrite()
        self._ai_search_page_rewrite()
        # ここまでで拾われなかったものは静的配信。許可prefix以外は一律403
        if not self._static_allowed(self.path):
            self.send_error(403)
            return
        if p == "/":
            self.path = "/index.html"
        super().do_GET()

    # ============================================================
    # 更新API（POST）
    # ------------------------------------------------------------
    # 受け付けるのは /api/admin/ 配下の既知エンドポイントのみ。
    #
    # CSRF対策（このサーバーには認証が無いことへの手当て）:
    #   認証が無いということは「LAN内の任意のWebページが撃ったリクエスト」と
    #   「管理ページが撃ったリクエスト」をCookieやトークンで区別できないという意味。
    #   悪意あるページが <form> やfetchで http://<frame>:8080/api/admin/setflag を
    #   叩けば、利用者が普通のサイトを見ているだけで写真を全部hiddenにされうる。
    #   そこで、ブラウザ側の同一オリジンポリシーを利用して2段で塞ぐ:
    #     (1) Content-Type: application/json を必須にする
    #         → これは「単純リクエスト」の条件（text/plain・form系のみ）から外れるので、
    #           クロスオリジンのfetchは必ずプリフライト(OPTIONS)が先に飛ぶ。
    #           こちらはCORSヘッダを一切返さない＝ブラウザが本リクエストを送る前に中止する。
    #           <form>はapplication/jsonを送れないのでフォーム経由の攻撃も成立しない。
    #     (2) Originヘッダがある場合は Host と一致するもののみ許可
    #         → プリフライトを迂回する経路が将来生えても、ブラウザが必ず付ける
    #           Originで弾ける。curl等Originを付けない非ブラウザからは従来どおり通す
    #           （LAN内の手元運用・本タスクの検証がこれに当たる）。
    #   併せて本文サイズにも上限を設ける（無認証POSTは無制限に読むとメモリ枯渇の的）。
    # ============================================================
    MAX_POST_BYTES = 64 * 1024

    def _post_routes(self):
        return {"/api/admin/setloc": admin_setloc,
                "/api/admin/setflag": admin_setflag,
                "/api/admin/sethome": admin_sethome,
                "/api/blog/save": save_blog_draft,
                "/api/blog/export": export_blog}

    def _csrf_ok(self):
        """Content-Type と Origin を検査する。不許可なら応答を返して False"""
        ctype = (self.headers.get("Content-Type") or "").split(";", 1)[0].strip().lower()
        if ctype != "application/json":
            self._send_json({"ok": False,
                             "error": "Content-Type: application/json が必要です"},
                            status=403)
            return False
        origin = self.headers.get("Origin")
        if origin:
            from urllib.parse import urlparse
            host = (self.headers.get("Host") or "").strip()
            if urlparse(origin).netloc != host:
                self._send_json({"ok": False, "error": "クロスオリジンは拒否されます"},
                                status=403)
                return False
        return True

    def do_POST(self):
        if not self._host_ok():      # ルーティングより前（CSRF2段構えの外側の関門）
            return
        path = self._norm_path(self.path)
        handler = self._post_routes().get(path)
        if handler is None:
            # /api/admin/ 配下の既知エンドポイント以外へのPOSTは一律403
            self.send_error(403)
            return
        if not self._csrf_ok():
            return
        # サイズ検査は読み込みの前に行う（読んでから測ると上限の意味が無い）
        try:
            length = int(self.headers.get("Content-Length"))
            if length < 0:
                raise ValueError
        except (TypeError, ValueError):
            self._send_json({"ok": False, "error": "Content-Length が必要です"},
                            status=400)
            return
        if length > self.MAX_POST_BYTES:
            # 本文を読まずに返すため、未読データが残る。接続を再利用させない
            self.close_connection = True
            self._send_json({"ok": False,
                             "error": f"本文が大きすぎます（上限{self.MAX_POST_BYTES}バイト）"},
                            status=413)
            return
        try:
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._send_json({"ok": False, "error": "JSONとして解釈できません"}, status=400)
            return
        if not isinstance(payload, dict):
            self._send_json({"ok": False, "error": "JSONオブジェクトを送ってください"},
                            status=400)
            return
        try:
            self._send_json(handler(payload))
        except BadRequest as e:
            self._send_json({"ok": False, "error": str(e)}, status=400)
        except NotFound as e:
            self._send_json({"ok": False, "error": str(e)}, status=404)
        except BlogBadRequest as e:
            self._send_json({"ok": False, "error": str(e)}, status=400)
        except BlogNotFound as e:
            self._send_json({"ok": False, "error": str(e)}, status=404)
        except BlogExportBusy as e:
            self._send_json({"ok": False, "error": str(e)}, status=409)
        except Exception as e:
            # 例外文字列はクライアントへ返さない。sqlite3やOSError経由で絶対パス
            # ローカル絶対パスやDBスキーマの断片がLAN内クライアントに漏れるため、
            # 応答は定型文にし、詳細はサーバーログにのみ出す
            print(f"[api] {path} 内部エラー: {type(e).__name__}: {e}")
            traceback.print_exc()
            self._send_json({"ok": False, "error": "内部エラーが発生しました"}, status=500)

    def log_message(self, fmt, *args):
        pass
