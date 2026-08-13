#!/usr/bin/env bash
#
# capture-fixtures.sh --- GeoFrame サーバの実レスポンスを fixture として採取する
#
# 出力は2系統に分かれる:
#   tests/data/           フル版（実データそのまま・.gitignore 済み・コミット禁止）
#   assets/fixtures/      コミット版（件数を絞り、位置情報を合成値へ置換したもの）
#
# 「ステータスコードを取得してから本文を扱う」ことを徹底している。GeoFrame は
# Host 検証 403 / 静的ホワイトリスト外 403 / 404 を **HTML** で返すため、
# 無条件に JSON parse する実装はここで壊れる。採取スクリプト自身もその原則に従う。
#
# 冪等性: 同じ入力に対する変換は決定的（乱数を使わない）。再実行すると出力を
# 上書きするだけで、重複や差分の蓄積は起きない。
#
# --offline を付けると採取を飛ばし、既存の tests/data/ からコミット版だけを
# 作り直す。サーバの起動が要らないので、置換ロジックを直したときはこちらを使う。
#
set -euo pipefail

# ------------------------------------------------------------------
# 設定
# ------------------------------------------------------------------
MODE="fetch"
PORT=""
for arg in "$@"; do
  case "${arg}" in
    --offline|--from-cache) MODE="offline" ;;
    -h|--help)              MODE="help" ;;
    *)                      PORT="${arg}" ;;
  esac
done
PORT="${PORT:-${GEOFRAME_FIXTURE_PORT:-8099}}"

# Host ヘッダ検証を確実に通すため IP リテラルで叩く（ホスト名は 403 になりうる）。
# Host のポート部が実ポートと違っても 403 になるので、ポート書き換えはしないこと。
HOST_IP="127.0.0.1"
BASE="http://${HOST_IP}:${PORT}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"          # native-display/
FULL_DIR="${ROOT}/tests/data"                   # フル版（gitignore 済み）
PUB_DIR="${ROOT}/assets/fixtures"               # コミット版
PUB_API="${PUB_DIR}/api"
PUB_ERR="${PUB_DIR}/errors"

usage() {
  cat <<'USAGE'
使い方:
    tools/capture-fixtures.sh [PORT]        サーバから採取し直してコミット版を生成
    tools/capture-fixtures.sh --offline     採取を飛ばし tests/data/ から再生成
    GEOFRAME_FIXTURE_PORT=8099 tools/capture-fixtures.sh

    PORT 既定値: 8099

採取モードでは、事前に GeoFrame サーバを **採取専用ポートで** 起動しておくこと。
このスクリプトはサーバを起動しない（誤って本番の 8080 を触る事故を避けるため）。

    cd <repo>/app
    source .venv/bin/activate
    GEOFRAME_PORT=8099 GEOFRAME_DRIVE_KEY=/nonexistent-disable-sync.json python server.py

  * GEOFRAME_DRIVE_KEY を存在しないパスにすると Drive 同期ワーカーが起動せず、
    catalog.db は読み取りのみになる。既存サーバと同時起動しても書き込み競合しない。
  * ポート 8080 で動いているサーバは停止しないこと。

--offline は既に tests/data/ が揃っている場合のみ使える。サーバに触らないので
置換ロジックを直したときの再生成・冪等性の確認はこちらで行うこと（採取モードは
実サーバーの状態が変わりうるため、2回実行の差分比較には向かない）。
USAGE
}

if [[ "${MODE}" == "help" ]]; then
  usage; exit 0
fi

mkdir -p "${FULL_DIR}" "${PUB_API}" "${PUB_ERR}"

NAMES=(photos env roomenv presence stats admin-photos
       err-host-forbidden err-static-forbidden err-not-found)

if [[ "${MODE}" == "fetch" ]]; then
  # ----------------------------------------------------------------
  # 疎通確認
  # ----------------------------------------------------------------
  echo "[capture] 接続先: ${BASE}"
  # curl は失敗時にも %{http_code} として "000" を書き出すので、
  # `|| echo 000` を足すと "000000" になる。失敗時は変数ごと上書きする。
  probe_code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "${BASE}/api/stats" 2>/dev/null)" || probe_code="000"
  if [[ "${probe_code}" != "200" ]]; then
    echo "" >&2
    echo "ERROR: ${BASE}/api/stats に接続できません (HTTP ${probe_code})" >&2
    echo "" >&2
    usage >&2
    exit 1
  fi
  echo "[capture] 疎通 OK (HTTP ${probe_code})"

  # ----------------------------------------------------------------
  # fetch <name> <path> <expected_status>
  #   本文とヘッダを保存し、ステータスを確認してから中身を扱う。
  #   ステータスが期待と違えば即座に失敗させる（黙って壊れた fixture を作らない）。
  # ----------------------------------------------------------------
  fetch() {
    local name="$1" path="$2" expect="$3"; shift 3
    local body="${FULL_DIR}/${name}.body"
    local hdr="${FULL_DIR}/${name}.headers.txt"
    local code
    code="$(curl -s -D "${hdr}" -o "${body}" -w '%{http_code}' --max-time 30 "$@" "${BASE}${path}")" || code="000"
    if [[ "${code}" != "${expect}" ]]; then
      echo "ERROR: ${path} は HTTP ${expect} を期待したが ${code} が返った" >&2
      exit 1
    fi
    # Content-Type はヘッダから取り出す（CR を落とす）
    local ctype
    ctype="$(awk -F': ' 'tolower($1)=="content-type"{print $2}' "${hdr}" | tr -d '\r' | head -1)"
    echo "${code}" > "${FULL_DIR}/${name}.status"
    echo "${ctype}" > "${FULL_DIR}/${name}.ctype"
    printf '[capture]   %-22s %s  %s\n' "${name}" "${code}" "${ctype}"
  }

  echo "[capture] --- 正常系 ---"
  fetch photos       "/api/photos"                    200
  fetch env          "/api/env"                       200
  fetch roomenv      "/api/roomenv"                   200
  fetch presence     "/api/presence"                  200
  fetch stats        "/api/stats"                     200
  fetch admin-photos "/api/admin/photos?page=1&per=50" 200

  echo "[capture] --- 異常系（HTML が返る経路）---"
  # Host 検証で拒否される 403。ホスト名が許可リストに無いので落ちる。
  fetch err-host-forbidden   "/api/env"               403 -H "Host: evil.example.com:${PORT}"
  # 静的ホワイトリスト外の 403（/server.py はどの許可 prefix にも入らない）
  fetch err-static-forbidden "/server.py"             403
  # 404（許可 prefix 配下だが実在しないファイル）
  fetch err-not-found        "/static/nonexistent.js" 404
else
  echo "[capture] --offline: 採取を飛ばし ${FULL_DIR} から再生成する"
  missing=0
  for n in "${NAMES[@]}"; do
    for ext in body status ctype headers.txt; do
      if [[ ! -f "${FULL_DIR}/${n}.${ext}" ]]; then
        echo "ERROR: ${FULL_DIR}/${n}.${ext} が無い" >&2
        missing=1
      fi
    done
  done
  if [[ "${missing}" != "0" ]]; then
    echo "" >&2
    echo "--offline は tests/data/ が揃っているときだけ使える。まず採取モードで実行すること。" >&2
    echo "" >&2
    usage >&2
    exit 1
  fi
  echo "[capture] フル版の存在確認 OK"
fi

# ------------------------------------------------------------------
# コミット版の生成（決定的。乱数を使わない）
# ------------------------------------------------------------------
echo "[capture] --- コミット版を生成（個人環境由来の値を合成値へ置換）---"
FULL_DIR="${FULL_DIR}" PUB_DIR="${PUB_DIR}" PUB_API="${PUB_API}" PUB_ERR="${PUB_ERR}" \
python3 <<'PYEOF'
# -*- coding: utf-8 -*-
"""コミット版 fixture を生成する。

方針: fixture の目的は「構造とパターンの多様性」の再現であり、地名・座標の
実在性は要件ではない。よって位置を示す情報はすべて合成値に置換する。

* 座標は実座標を **一度も読まずに** 生成する。実座標にオフセットを足す/引く
  方式は使わない（差分を取れば元に戻せるため）。
* 地名・タイトルは架空の値に置換する。置換の索引は **元データ内での初出順**
  で振る。元の文字列をハッシュしたり辞書順に並べたりして索引を作ってはいけない
  --- 都道府県は47・市区町村は約1900しか候補が無いので、元の文字列の関数に
  すると総当たりで逆引きできてしまう。初出順は元データが無ければ再現できない。
* 撮影日時・取得日時は固定基準日から生成する。実際の日時や間隔は使わず、順序と
  文字列表現だけを保つ。
* 室内環境のセンサー名・entity ID・値・更新時刻と在室情報の更新時刻も合成する。
"""
import json, os, pathlib, collections, hashlib, re
from datetime import datetime, timedelta

FULL = pathlib.Path(os.environ["FULL_DIR"])
PUB  = pathlib.Path(os.environ["PUB_DIR"])
API  = pathlib.Path(os.environ["PUB_API"])
ERR  = pathlib.Path(os.environ["PUB_ERR"])

SAMPLE_N = 50    # コミット版 /api/photos の件数
ADMIN_N  = 20    # コミット版 /api/admin/photos の件数

# ===== 合成パラメータ =====
# このシードは公開して構わない。合成値は「元データ内での初出順の索引」の関数で
# あって元の座標・地名の関数ではないため、シードを知っても索引→合成値が計算
# できるだけで、索引→元の値は分からない。
SEED = "geoframe-fixture-synthesis-v2"

# 合成座標を置く範囲。日本の緯度経度の内側に収め、地図描画のテストが
# 不自然にならないようにする（実データの分布からは導出していない）。
JP_LAT = (33.2, 43.2)
JP_LON = (131.0, 142.5)
SYN_TIME_BASE = datetime(2024, 1, 15, 9, 0, 0)
SYN_FETCH_BASE = datetime(2024, 6, 1, 12, 0, 0)


def _h(*parts):
    key = SEED + "\x1f" + "\x1f".join(str(p) for p in parts)
    return int.from_bytes(hashlib.sha256(key.encode("utf-8")).digest()[:8], "big")


def unit(*parts):
    """[0, 1) の決定的な値"""
    return _h(*parts) / float(1 << 64)


def spread(lo, hi, *parts):
    return lo + (hi - lo) * unit(*parts)


def pick(seq, *parts):
    return seq[_h(*parts) % len(seq)]


def format_time_like(value, synthetic):
    """元の文字列表現だけを保ち、値は合成日時へ置換する。"""
    if not isinstance(value, str) or not value:
        return value
    if "T" in value:
        return synthetic.strftime("%Y-%m-%dT%H:%M:%S")
    if len(value) == 10:
        return synthetic.strftime("%Y-%m-%d")
    return synthetic.strftime("%Y-%m-%d %H:%M")


def alpha(i):
    """0 -> A, 25 -> Z, 26 -> AA"""
    s = ""
    i += 1
    while i > 0:
        i, r = divmod(i - 1, 26)
        s = chr(ord("A") + r) + s
    return s


class Registry:
    """実在の文字列 -> 架空の文字列。索引は初出順（元データ依存＝逆引き不能）。

    空文字列は空文字列のまま返す（「地名が取れなかった」分岐はテスト対象なので
    潰してはいけない）。
    """

    def __init__(self, fmt):
        self.fmt = fmt
        self._idx = {}
        self._name = {}

    def _ensure(self, real):
        if real not in self._idx:
            i = len(self._idx)
            self._idx[real] = i
            self._name[real] = self.fmt(i)

    def has(self, real):
        return real in self._idx

    def name(self, real):
        if real is None:
            return None
        if real == "":
            return ""
        self._ensure(real)
        return self._name[real]

    def index(self, real):
        if real is None or real == "":
            return None
        self._ensure(real)
        return self._idx[real]

    def __len__(self):
        return len(self._idx)


PREF   = Registry(lambda i: "%s県" % alpha(i))
CITY   = Registry(lambda i: "%s市" % alpha(i))
PLACE  = Registry(lambda i: "%s町" % alpha(i))
REGION = Registry(lambda i: "%s地方" % alpha(i))


def load(name):
    return json.loads((FULL / ("%s.body" % name)).read_text(encoding="utf-8"))


def status_of(name):
    return int((FULL / ("%s.status" % name)).read_text().strip())


def ctype_of(name):
    return (FULL / ("%s.ctype" % name)).read_text().strip()


def dump(path, obj):
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


meta = {}


def note(name, out_rel, status, ctype, desc):
    meta[name] = {"status": status, "content_type": ctype,
                  "body": out_rel, "description": desc}


# ==================================================================
# 1. 置換表を作る（初出順で索引を振る）
# ==================================================================
photos = load("photos")
assert isinstance(photos, list), "/api/photos は裸の配列のはず"

# 実際の撮影間隔は使わない。旅を固定間隔で並べ、旅の中では元の並び順へ固定間隔を
# 割り当てる。時系列を前提とするテストに必要な順序とまとまりだけを維持する。
SYN_CAPTURED = {}
trip_bases = {}
trip_counts = collections.Counter()
for p in photos:
    tid = p.get("trip")
    if tid not in trip_bases:
        trip_bases[tid] = SYN_TIME_BASE + timedelta(days=45 * len(trip_bases))
    SYN_CAPTURED[p.get("id")] = trip_bases[tid] + timedelta(
        minutes=37 * trip_counts[tid])
    trip_counts[tid] += 1

for p in photos:
    PREF.name(p.get("pref", ""))
    CITY.name(p.get("city", ""))
    PLACE.name(p.get("place", ""))

adm_all = load("admin-photos")
for it in adm_all.get("items", []):
    PREF.name(it.get("pref", ""))
    CITY.name(it.get("city", ""))
    PLACE.name(it.get("place", ""))

stats = load("stats")
for c in stats.get("top_cities", []):
    if isinstance(c, list) and c:
        CITY.name(c[0])
for x in stats.get("pref_list", []):
    PREF.name(x)


# ==================================================================
# 2. 合成座標を作る（実座標を一度も読まない）
# ==================================================================
# 階層構造: 県中心 -> 市オフセット -> 大字オフセット -> 写真ごとの微小ゆらぎ。
# 実データと同じく「同じ地名なら近くに集まる」性質を保つので、地図描画・
# グループ化のテストが不自然にならない。
def pref_center(pi):
    return (spread(JP_LAT[0], JP_LAT[1], "pref-lat", pi),
            spread(JP_LON[0], JP_LON[1], "pref-lon", pi))


def city_center(pi, ci):
    la, lo = pref_center(pi)
    return (la + spread(-0.32, 0.32, "city-lat", pi, ci),
            lo + spread(-0.38, 0.38, "city-lon", pi, ci))


def place_center(pi, ci, qi):
    la, lo = city_center(pi, ci)
    return (la + spread(-0.06, 0.06, "place-lat", pi, ci, qi),
            lo + spread(-0.07, 0.07, "place-lon", pi, ci, qi))


trip_photos = collections.OrderedDict()
for p in photos:
    trip_photos.setdefault(p.get("trip"), []).append(p)

# 逆ジオコーディングが失敗した写真（pref/city/place が空）は地名から座標を
# 決められない。同じ trip の中で最初に地名が取れた写真の周りに置くことで、
# trip の bounds が日本中に広がってしまうのを防ぐ。地名が1件も無い trip は
# trip の索引から中心を決める。
trip_anchor = {}
for tid, ps in trip_photos.items():
    geo = None
    for p in ps:
        if p.get("pref"):
            geo = p
            break
    if geo is not None:
        trip_anchor[tid] = place_center(PREF.index(geo["pref"]),
                                        CITY.index(geo["city"]),
                                        PLACE.index(geo["place"]))
    else:
        trip_anchor[tid] = (spread(JP_LAT[0], JP_LAT[1], "trip-lat", tid),
                            spread(JP_LON[0], JP_LON[1], "trip-lon", tid))

# 写真 id -> 合成座標。/api/photos と /api/admin/photos は同じ写真を返すので、
# id を鍵にして両者で必ず同じ座標になるようにする。
SYN = {}
for p in photos:
    pid = p.get("id")
    if p.get("pref"):
        la, lo = place_center(PREF.index(p["pref"]), CITY.index(p["city"]),
                              PLACE.index(p["place"]))
        jl, jo = 0.012, 0.014
    else:
        la, lo = trip_anchor[p.get("trip")]
        jl, jo = 0.16, 0.18
    SYN[pid] = (round(la + spread(-jl, jl, "ph-lat", pid), 6),
                round(lo + spread(-jo, jo, "ph-lon", pid), 6))


def syn_coord(pid):
    """写真 id から合成座標を引く。photos に無い id は id 単体から作る。"""
    if pid in SYN:
        return SYN[pid]
    return (round(spread(JP_LAT[0], JP_LAT[1], "orphan-lat", pid), 6),
            round(spread(JP_LON[0], JP_LON[1], "orphan-lon", pid), 6))


def syn_elev(rec):
    """標高。null と数値の分岐を保つ（実データでは place が空のとき null）。"""
    if rec.get("elev", None) is None:
        return None
    qi = PLACE.index(rec.get("place", ""))
    if qi is None:
        base = spread(5.0, 1200.0, "elev-base", "t", rec.get("trip"))
    else:
        base = spread(5.0, 1200.0, "elev-base", "p", qi)
    return round(base + spread(-40.0, 40.0, "elev-jit", rec.get("id")), 1)


# ==================================================================
# 3. タイトルの合成
# ==================================================================
# 実データのタイトルは2系統:
#   * 逆ジオコーディング成功 -> 地名を含む日本語タイトル
#   * 失敗 -> カメラ既定のファイル名（純 ASCII）または UUID
# 日本語側は **元の文字数を保ってはいけない**。地名を含むので、文字数を保つと
# 「実在の地名の文字数の分布」が漏れ、候補となる市区町村リストと突き合わせ
# られてしまう。長さは合成後の索引と写真 id から決める。
CAM = re.compile(r"^[A-Za-z]{2,6}[-_]?\d{3,10}$")
TITLE_WORDS = ["ながめ", "ひろば", "こみち", "みなも", "たかだい",
               "せせらぎ", "あさひ", "ゆうひ", "みずべ", "かぜのおか"]


def syn_title(p):
    t = p.get("title", "")
    pid = p.get("id")
    if not isinstance(t, str) or t == "":
        return t
    if t.isascii():
        if CAM.match(t):
            # カメラ既定のファイル名。位置情報を持たないので長さをそのまま保つ
            # （表示幅・省略のテストに効く）。
            n = len(t)
            body_len = max(1, n - 4)
            digits = ("%0*d" % (body_len, _h("title-cam", pid) % (10 ** body_len)))
            return ("FIX_" + digits)[:n]
        # UUID 等の長い ASCII 識別子。区切りの位置と長さだけ保つ。
        hexs = hashlib.sha256(("%s|title-uuid|%s" % (SEED, pid)).encode()).hexdigest()
        out, k = [], 0
        for c in t:
            if c.isalnum():
                out.append(hexs[k % len(hexs)])
                k += 1
            else:
                out.append(c)
        return "".join(out)
    head = (PLACE.name(p.get("place", "")) or PREF.name(p.get("pref", ""))
            or ("%s地方" % alpha(int(p.get("trip") or 0))))
    nw = 1 + (_h("title-nw", pid) % 3)
    return head + "".join(pick(TITLE_WORDS, "title-w", pid, k) for k in range(nw))


# ==================================================================
# 4. trip のラベル
# ==================================================================
LABEL_RE = re.compile(r"^(\d{4}\.\d{2})\s+(.*)$")


def syn_label(lab, month_prefix):
    """旅の年月と地名を合成値へ置換する。"""
    if not isinstance(lab, str):
        return lab
    m = LABEL_RE.match(lab)
    body = m.group(2) if m else lab
    if body.endswith("の旅"):
        core, suffix = body[:-2], "の旅"
    else:
        core, suffix = body, ""
    if CITY.has(core):
        name = CITY.name(core)
    elif PREF.has(core):
        name = PREF.name(core)
    else:
        name = REGION.name(core)
    return month_prefix + " " + name + suffix


# ==================================================================
# 5. /api/photos
# ==================================================================
# 1800件超あるので件数を絞るが、構造と多様性は保つ。
# pref ごとのグループを巡回して拾い、さらに inferred / elev None / 空 place の
# 各パターンが必ず1件以上入るようにする。乱数は使わない。
by_pref = collections.OrderedDict()
for i, p in enumerate(photos):
    by_pref.setdefault(p.get("pref", ""), []).append(i)

picked = []
seen = set()


def take(idx):
    if idx not in seen and len(picked) < SAMPLE_N:
        seen.add(idx)
        picked.append(idx)


def first_where(pred):
    for i, p in enumerate(photos):
        if pred(p):
            return i
    return None


# 1) 必須パターンを先に確保（テストの分岐に直結するため）
for pred in (
    lambda p: p.get("inferred") is True,
    lambda p: p.get("inferred") is False,
    lambda p: p.get("elev") is None,
    lambda p: p.get("elev") is not None,
    lambda p: p.get("place") == "",
    lambda p: p.get("place") not in ("", None),
    lambda p: p.get("city") == "",
    lambda p: p.get("city") not in ("", None),
    lambda p: p.get("fav") is True,
    lambda p: p.get("fav") is False,
):
    i = first_where(pred)
    if i is not None:
        take(i)

# 2) 残りは pref グループを round-robin で巡回（地域の偏りを避ける）
round_i = 0
while len(picked) < SAMPLE_N:
    added = False
    for pref, idxs in by_pref.items():
        if round_i < len(idxs):
            take(idxs[round_i])
            added = True
            if len(picked) >= SAMPLE_N:
                break
    if not added:
        break
    round_i += 1

picked.sort()   # 元の並び（撮影時刻順）を保つ。trip 割当が時系列前提のため
PUBLIC_ID = {photos[idx].get("id"): n for n, idx in enumerate(picked, start=1)}

pub_photos = []
for n, idx in enumerate(picked, start=1):
    src = photos[idx]
    item = dict(src)                       # キーの追加・削除・改名をしない
    la, lo = syn_coord(src.get("id"))
    item["lat"] = la
    item["lon"] = lo
    item["place"] = PLACE.name(src.get("place", ""))
    item["pref"] = PREF.name(src.get("pref", ""))
    item["city"] = CITY.name(src.get("city", ""))
    item["elev"] = syn_elev(src)
    item["title"] = syn_title(src)
    item["id"] = PUBLIC_ID[src.get("id")]
    captured = SYN_CAPTURED[src.get("id")]
    if "time" in item:
        item["time"] = format_time_like(src.get("time"), captured)
    if "fetched_at" in item:
        item["fetched_at"] = format_time_like(
            src.get("fetched_at"), SYN_FETCH_BASE + timedelta(seconds=2 * n))
    # file は Drive のファイルID を含むので合成名に置換。
    # 先頭スラッシュが無い実データの性質はそのまま保つ（/api/admin/photos の
    # thumb は先頭スラッシュ有りで非対称。この差はテストで効くので壊さない）
    f = src.get("file", "") or ""
    ext = f.rsplit(".", 1)[-1] if "." in f else "jpg"
    item["file"] = "derivatives/fixture-%04d.%s" % (n, ext)
    pub_photos.append(item)

# 真偽の両分岐を、元データの状態に依存せず必ず含める。
if pub_photos:
    pub_photos[0]["fav"] = False
if len(pub_photos) > 1:
    pub_photos[1]["fav"] = True
PUBLIC_FAV = {item["id"]: item.get("fav", False) for item in pub_photos}

dump(API / "photos.json", pub_photos)
note("photos", "api/photos.json", status_of("photos"), ctype_of("photos"),
     "/api/photos の縮小版（裸の配列・%d件・日時/座標/地名/標高/タイトルは合成値）" % len(pub_photos))

# ==================================================================
# 6. /api/env
# ==================================================================
# 実際の予報値は地域と採取日時の推測に使えるため、天気・週間予報・地震を同じ
# スキーマと型を持つ合成値へ置換する。
env = load("env")
pub_env = dict(env)
w = env.get("weather")
if isinstance(w, dict):
    w2 = dict(w)
    w2["area"] = "仮想北部"
    if "text" in w2:
        w2["text"] = "晴れ時々くもり"
    if "pop" in w2:
        w2["pop"] = 30
    if "temps" in w2:
        w2["temps"] = ["18", "25"]
    pub_env["weather"] = w2
weekly = []
for i, day in enumerate(env.get("weekly", [])):
    day2 = dict(day)
    day2["date"] = (datetime(2024, 6, 2) + timedelta(days=i)).strftime("%Y-%m-%d")
    if "code" in day2:
        day2["code"] = ("101", "200", "201")[i % 3]
    if "pop" in day2:
        day2["pop"] = None if i == 0 else 10 + 10 * (i % 4)
    if "temp_min" in day2:
        day2["temp_min"] = None if i == 0 else 16 + (i % 3)
    if "temp_max" in day2:
        day2["temp_max"] = None if i == 0 else 23 + (i % 4)
    weekly.append(day2)
pub_env["weekly"] = weekly
quakes = []
for i, quake in enumerate(env.get("quakes", []), start=1):
    quake2 = dict(quake)
    occurred = datetime(2024, 6, 1, 9, 0) + timedelta(minutes=43 * i)
    quake2.update({
        "eid": "FIXTURE-ENV-%04d" % i,
        "lat": round(33.0 + 0.7 * i, 1),
        "lon": round(130.0 + 0.8 * i, 1),
        "mag": "%.1f" % (2.0 + 0.1 * i),
        "maxi": "1" if i % 2 else "2",
        "place": "%s県沖" % alpha(i - 1),
        "time": occurred.strftime("%H:%M"),
        "at": occurred.strftime("%Y-%m-%dT%H:%M:%S+09:00"),
    })
    quakes.append(quake2)
pub_env["quakes"] = quakes
dump(API / "env.json", pub_env)
note("env", "api/env.json", status_of("env"), ctype_of("env"),
     "/api/env（quakes/weather/weekly は合成値。ok フィールドは無い）")

# ==================================================================
# 7. /api/roomenv, /api/presence
# ==================================================================
roomenv = load("roomenv")
pub_roomenv = dict(roomenv)
sensor_values = {
    "temperature": "22.50",
    "humidity": "48.00",
    "carbon_dioxide": "650",
    "volatile_organic_compounds": "120",
    "pressure": "1013.20",
}
sensor_names = {
    "temperature": "温度",
    "humidity": "湿度",
    "carbon_dioxide": "二酸化炭素",
    "volatile_organic_compounds": "揮発性有機化合物",
    "pressure": "気圧",
}
items = []
for i, item in enumerate(roomenv.get("items", []), start=1):
    item2 = dict(item)
    kind = str(item.get("class") or "sensor")
    safe_kind = re.sub(r"[^a-z0-9_]+", "_", kind.lower())
    item2["entity"] = "sensor.fixture_%s_%02d" % (safe_kind, i)
    item2["name"] = "サンプルセンサー %s" % sensor_names.get(kind, alpha(i - 1))
    item2["value"] = sensor_values.get(kind, str(10 * i))
    items.append(item2)
pub_roomenv["items"] = items
if "ts" in pub_roomenv:
    pub_roomenv["ts"] = 1705309200.0
dump(API / "roomenv.json", pub_roomenv)
note("roomenv", "api/roomenv.json", status_of("roomenv"), ctype_of("roomenv"),
     "/api/roomenv（センサー識別子・値・更新時刻は合成値）")

presence = load("presence")
pub_presence = dict(presence)
if "last_motion" in pub_presence:
    pub_presence["last_motion"] = 1705309260.0
dump(API / "presence.json", pub_presence)
note("presence", "api/presence.json", status_of("presence"), ctype_of("presence"),
     "/api/presence（更新時刻は合成値。night は true/false/null の3値）")

# night の3値すべてを網羅する派生 fixture（構造は同一・値だけ差し替え）
for label, val in (("true", True), ("false", False), ("null", None)):
    v = dict(pub_presence)
    v["night"] = val
    dump(API / ("presence-night-%s.json" % label), v)
    note("presence-night-%s" % label, "api/presence-night-%s.json" % label,
         200, ctype_of("presence"),
         "[合成] presence の night を %s に差し替えたもの（3値網羅用）" % label)

# ==================================================================
# 8. /api/stats
# ==================================================================
pub_stats = dict(stats)
pub_stats["pref_list"] = [PREF.name(x) for x in stats.get("pref_list", [])]
pub_stats["top_cities"] = [
    [CITY.name(c[0]), 120 - 12 * i] if isinstance(c, list) and c else c
    for i, c in enumerate(stats.get("top_cities", []))
]

# bounds は [west, south, east, north]。フル版の27件すべてで w<e / s<n を
# 満たすことを確認済み。合成座標から **その trip の全写真** を使って
# 計算し直す（コミット版の photos.json は50件の抜粋なのでそちらでは足りない）。
trips = []
month_counts = [0] * 12
all_trip_times = []
for trip_index, t in enumerate(stats.get("trips", [])):
    t2 = dict(t)
    tid = t.get("id")
    synthetic_count = 24 + (17 * trip_index) % 145
    trip_start = SYN_TIME_BASE + timedelta(days=45 * trip_index)
    trip_end = trip_start + timedelta(minutes=37 * (synthetic_count - 1))
    t2["count"] = synthetic_count
    t2["label"] = syn_label(t.get("label"), trip_start.strftime("%Y.%m"))
    if "start" in t2:
        t2["start"] = format_time_like(t.get("start"), trip_start)
    if "end" in t2:
        t2["end"] = format_time_like(t.get("end"), trip_end)
    month_counts[trip_start.month - 1] += synthetic_count
    all_trip_times.extend((trip_start, trip_end))
    pts = [syn_coord(p.get("id")) for p in trip_photos.get(tid, [])]
    if pts:
        las = [a for a, _ in pts]
        los = [b for _, b in pts]
        w, s, e, n = min(los), min(las), max(los), max(las)
        # 1件だけの trip で幅0にならないよう最低幅を確保する
        if e - w < 1e-6:
            w, e = w - 5e-4, e + 5e-4
        if n - s < 1e-6:
            s, n = s - 5e-4, n + 5e-4
        t2["bounds"] = [round(w, 6), round(s, 6), round(e, 6), round(n, 6)]
    elif isinstance(t.get("bounds"), list) and len(t["bounds"]) == 4:
        # 写真が引けない trip。実座標を残さないため trip 索引から合成する
        cla, clo = (spread(JP_LAT[0], JP_LAT[1], "trip-lat", tid),
                    spread(JP_LON[0], JP_LON[1], "trip-lon", tid))
        t2["bounds"] = [round(clo - 0.05, 6), round(cla - 0.05, 6),
                        round(clo + 0.05, 6), round(cla + 0.05, 6)]
    trips.append(t2)
pub_stats["trips"] = trips
pub_stats["total"] = sum(t["count"] for t in trips)
pub_stats["pref_count"] = len(pub_stats["pref_list"])
pub_stats["months"] = month_counts
pub_stats["span"] = [
    format_time_like(stats.get("span", [""])[0], min(all_trip_times)),
    format_time_like(stats.get("span", ["", ""])[1], max(all_trip_times)),
] if all_trip_times else []
pub_stats["dist_km"] = 12500
pub_stats["inferred"] = pub_stats["total"] // 3
pub_stats["pending"] = 0
dump(API / "stats.json", pub_stats)
note("stats", "api/stats.json", status_of("stats"), ctype_of("stats"),
     "/api/stats（日時/地名は合成値・trips[].bounds は合成座標から再計算。ok フィールドは無い）")

# ==================================================================
# 9. /api/admin/photos
# ==================================================================
pub_adm = dict(adm_all)
items = []
for n, it in enumerate(adm_all.get("items", [])[:ADMIN_N], start=1):
    it2 = dict(it)
    la, lo = syn_coord(it.get("id"))
    it2["lat"] = la
    it2["lon"] = lo
    it2["place"] = PLACE.name(it.get("place", ""))
    it2["pref"] = PREF.name(it.get("pref", ""))
    it2["city"] = CITY.name(it.get("city", ""))
    it2["id"] = PUBLIC_ID.get(it.get("id"), 10000 + n)
    it2["fav"] = PUBLIC_FAV.get(it2["id"], bool(n % 5 == 0))
    captured = SYN_CAPTURED.get(
        it.get("id"), SYN_TIME_BASE + timedelta(minutes=37 * (n - 1)))
    if "taken_at" in it2:
        it2["taken_at"] = format_time_like(it.get("taken_at"), captured)
    th = it.get("thumb") or ""
    ext = th.rsplit(".", 1)[-1] if "." in th else "jpg"
    # 先頭スラッシュ有り。/api/photos の file（無し）との非対称をそのまま残す
    it2["thumb"] = "/derivatives/fixture-%04d.%s" % (n, ext)
    nm = it.get("name") or ""
    nmext = nm.rsplit(".", 1)[-1] if "." in nm else "JPG"
    it2["name"] = "FIXTURE_%04d.%s" % (n, nmext)
    items.append(it2)
pub_adm["items"] = items
pub_adm["per"] = len(items)
pub_adm["total"] = pub_stats["total"]
if "pages" in pub_adm:
    pub_adm["pages"] = max(1, (pub_adm["total"] + len(items) - 1) // len(items))
dump(API / "admin-photos.json", pub_adm)
note("admin-photos", "api/admin-photos.json", status_of("admin-photos"),
     ctype_of("admin-photos"),
     "/api/admin/photos の縮小版（日時/座標/地名は合成値。thumb は先頭スラッシュ有り＝photos.file と非対称）")

# ==================================================================
# 10. 異常系（HTML 本文をそのまま保存）
# ==================================================================
errs = [
    ("err-host-forbidden",   "host-forbidden-403.html",
     "Host 検証で拒否された 403。本文は HTML（JSON ではない）"),
    ("err-static-forbidden", "static-forbidden-403.html",
     "静的ホワイトリスト外の 403（/server.py）。本文は HTML"),
    ("err-not-found",        "not-found-404.html",
     "許可 prefix 配下の実在しないファイルへの 404。本文は HTML"),
]
for name, out, desc in errs:
    body = (FULL / ("%s.body" % name)).read_text(encoding="utf-8")
    (ERR / out).write_text(body, encoding="utf-8")
    headers = (FULL / ("%s.headers.txt" % name)).read_text(encoding="utf-8")
    headers = re.sub(r"(?im)^Date:.*$", "Date: Mon, 01 Jan 2024 00:00:00 GMT", headers)
    (ERR / (out.rsplit(".", 1)[0] + ".headers.txt")).write_text(headers, encoding="utf-8")
    note(name, "errors/%s" % out, status_of(name), ctype_of(name), desc)

# ==================================================================
# 11. 合成: 注入用の地震データ
# ==================================================================
# 実データの quakes には震度3以上がめったに現れないため、構造だけ実データから
# 借り、震度と時刻を差し替えた合成データを用意する。
# eid は "FIXTURE-" 始まりで、実データ（14桁の数字）と絶対に混同されない。
#
# place は実在の都道府県名を使わない。この配列は手書きの合成データなので実在の
# 地名を書いても漏洩にはならないが、「フル版の地名がコミット版に1件も現れない」
# という検証を偶発一致で汚さないため、十干（甲乙丙…＝都道府県名には決して
# 使われない）を県名の代わりに使う。気象庁の表記フォーマット
# （◯◯県沖 / ◯◯県北部 / ◯◯地方 / ◯◯地方）とその文字数の幅は保つ。
def quake(eid, lat, lon, mag, maxi, place, at):
    # time は "HH:MM" の5文字。日付が落ちているので同定には at を使うこと
    return {"eid": eid, "lat": lat, "lon": lon, "mag": mag, "maxi": maxi,
            "place": place, "time": at[11:16], "at": at}


synth = [
    quake("FIXTURE-0001", 35.7, 139.7, "2.4", "1",  "甲県中央区",   "2026-07-30T09:10:00+09:00"),
    # 閾値未満: アラートを出してはいけない
    quake("FIXTURE-0002", 39.3, 142.1, "3.6", "2",  "乙県沖",       "2026-07-30T10:20:00+09:00"),
    # 閾値ちょうど: 出す
    quake("FIXTURE-0003", 36.1, 139.0, "4.2", "3",  "丙県北部",     "2026-07-30T11:30:00+09:00"),
    quake("FIXTURE-0004", 34.7, 135.5, "4.8", "4",  "丁県北部",     "2026-07-30T12:40:00+09:00"),
    # "5-" / "5+" は std::stoi で解析できない表記。この形が実在することを示す
    quake("FIXTURE-0005", 38.3, 141.0, "5.5", "5-", "戊県沖",       "2026-07-30T13:50:00+09:00"),
    quake("FIXTURE-0006", 33.6, 130.4, "6.1", "5+", "己県北部",     "2026-07-30T14:00:00+09:00"),
    quake("FIXTURE-0007", 43.1, 141.3, "6.4", "6-", "庚地方",       "2026-07-30T15:00:00+09:00"),
    quake("FIXTURE-0008", 31.6, 130.6, "6.9", "6+", "辛県西部",     "2026-07-30T16:00:00+09:00"),
    quake("FIXTURE-0009", 37.4, 138.9, "7.3", "7",  "壬県中越地方", "2026-07-30T17:00:00+09:00"),
    # time が同じ "23:50" だが at の日付が違う2件。
    # time だけでは同定できない（＝重複判定に time を使ってはいけない）ことを示す
    quake("FIXTURE-0010", 35.0, 138.0, "3.1", "3",  "癸県中部",     "2026-07-29T23:50:00+09:00"),
    quake("FIXTURE-0011", 35.0, 138.0, "3.3", "3",  "癸県中部",     "2026-07-30T23:50:00+09:00"),
    # 同一 eid の重複（サーバ側で重複排除されない場合があることを示す）
    quake("FIXTURE-0012", 36.4, 138.5, "4.0", "3",  "甲県南部",     "2026-07-30T18:00:00+09:00"),
    quake("FIXTURE-0012", 36.4, 138.5, "4.0", "3",  "甲県南部",     "2026-07-30T18:00:00+09:00"),
]
# /api/env と完全に同じ形（weather が null・weekly が空なのは実際に起こりうる状態）
dump(PUB / "quakes-inject.json",
     {"weather": None, "weekly": [], "quakes": synth})
note("quakes-inject", "quakes-inject.json", 200, "application/json; charset=utf-8",
     "[合成] /api/env と同形。震度と時刻のみ差し替えた注入用データ。eid は FIXTURE-*")

# ==================================================================
# 12. index.json
# ==================================================================
# 復元に使える具体的な数値（オフセット等）は **書かない**。何をしたかの記録は残す。
dump(PUB / "index.json", {
    "generated_by": "tools/capture-fixtures.sh",
    "note": "status / content_type は採取時の実測値。body は assets/fixtures/ からの相対パス",
    "synthesis": {
        "coordinates": "実座標から導出しない合成値。実データを読まず、初出順の索引から決定的に生成する",
        "place_names": "pref/city/place/title/trips[].label は架空の名前。同じ元値は必ず同じ置換値になる",
        "elevation": "合成値（null と数値の分岐は保持）",
        "trip_bounds": "合成座標から再計算",
        "outdoor_environment": "天気・週間予報・地震を同じスキーマの合成値に置換",
        "timestamps": "実際の日時・間隔を使わず、固定基準日から生成",
        "room_environment": "センサー識別子・実測値・更新時刻を合成値に置換",
        "presence": "在室情報の更新時刻を合成値に置換",
        "photos_sample_n": SAMPLE_N,
        "admin_photos_sample_n": ADMIN_N,
        "detail": "詳細は docs/FIXTURES.md",
    },
    "fixtures": meta,
})

print("[capture]   photos: %d件 -> %d件に縮小" % (len(photos), len(pub_photos)))
print("[capture]   置換表: pref %d / city %d / place %d / region %d"
      % (len(PREF), len(CITY), len(PLACE), len(REGION)))
print("[capture]   合成 quakes: %d件" % len(synth))
PYEOF

echo "[capture] 完了"
echo "[capture]   フル版:     ${FULL_DIR}  (gitignore 済み・コミット禁止)"
echo "[capture]   コミット版: ${PUB_DIR}"
