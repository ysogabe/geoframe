"""管理画面の読み取りAPI（`/api/admin/status`・`/api/admin/photos`）の契約テスト用フィクスチャ。

本物のスキーマ（store.SCHEMA と移行）で一時DBを作り、合成の写真・品質・AI・位置候補を入れて、
現行の Python 実装（`admin.api_admin_status()`、`handlers.Handler` が返す `/api/admin/photos`）の
出力を得る。Rust の結合テスト（crates/geoframe/tests/admin_api.rs）は、同じDBを作り直して
同じ入力を流し、出力が JSON として完全一致することを確かめる。

再生成: `python3 tools/gen_admin_fixture.py`（差分の検査は `--check`）。
座標・地名・ファイル名は実在のものを使わない。座標は 35.1/139.1 のような丸い値、地名は
「架空県」「見本市」「サンプル町」。

比べない値: `disk`（機体のディスクと実行時刻に依存する。Rust は形と、ディレクトリの数え方を別に検査する）と
`server_time`。それ以外はすべて比べる。
"""
import http.client
import json
import tempfile
import threading
from contextlib import contextmanager
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from geoframe import admin, geo, handlers, loc_suggest, store

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_DIR = ROOT / "crates/geoframe/tests/fixtures"
FIXTURE_PATH = FIXTURE_DIR / "admin-api-python.json"
CATALOG_SQL_PATH = FIXTURE_DIR / "admin-api-catalog.sql"

# 表示ファイルが無い行で、過去に作った derivative がディスクに残っている drive_id
# （Rust のテストは同じ名前のファイルをデータ置き場の derivatives/ に作る）
DERIVATIVE_IDS = ["d-has-deriv"]

PHOTO_COLUMNS = ("id", "source", "drive_id", "path", "display", "name", "md5", "mtime",
                 "taken_at", "lat", "lon", "loc_source", "infer_dt_s", "place", "pref", "city",
                 "favorite", "hidden", "rotation")


def photo_row(id, **over):
    local = over.get("source") == "local"
    row = {"id": id, "source": "drive", "drive_id": None if local else f"drive-{id}",
           "path": f"local/p{id:02d}.jpg" if local else None,
           "display": f"derivatives/p{id:02d}.jpg", "name": f"IMG_{id:04d}.jpg",
           "md5": f"m{id}", "mtime": id * 10.0, "taken_at": "2025-06-01 10:00",
           "lat": 35.0, "lon": 139.0, "loc_source": "exif", "infer_dt_s": None,
           "place": None, "pref": None, "city": None, "favorite": 0, "hidden": 0, "rotation": 0}
    row.update(over)
    row["drive_id"] = over.get("drive_id", row["drive_id"])
    row["path"] = over.get("path", row["path"])
    return row


def noloc(id, taken_at, **over):
    return photo_row(id, **{"taken_at": taken_at, "lat": None, "lon": None, "loc_source": None,
                            **over})


def inferred(id, taken_at, dt):
    return photo_row(id, taken_at=taken_at, loc_source="inferred", infer_dt_s=dt,
                     lat=35.1, lon=139.1)


# 位置あり（時刻のヒントの錨になるのは loc_source が exif / drive_meta のもの）
PHOTOS = [
    photo_row(1, source="local", taken_at="2025-06-01 10:00", lat=35.1, lon=139.1,
              pref="架空県", city="見本市", place="サンプル町", favorite=1, rotation=90),
    photo_row(2, taken_at="2025-06-01 12:00", lat=35.2, lon=139.2, pref="架空県", city="見本市",
              rotation=None),
    photo_row(3, taken_at="2025-06-03 09:00", lat=35.3, lon=139.3, loc_source="drive_meta",
              pref="架空県", city="北見本", place="公園前", hidden=1),
    photo_row(4, source="local", taken_at="2025-06-10 15:30", lat=35.4, lon=139.4),
    # 表示ファイルが無い（まだ生成されていない）位置あり写真
    photo_row(5, taken_at="2025-06-20 08:00", lat=35.5, lon=139.5, display=None,
              drive_id="d-orphan", rotation=270),
    # 位置を時刻から推定した写真。錨との時間差が各段階窓の境目になる値と、未計測（NULL）
    inferred(6, "2025-06-01 10:10", 600),
    inferred(7, "2025-06-01 10:20", 900),
    inferred(8, "2025-06-01 11:00", 901),
    inferred(9, "2025-06-01 13:00", 7200),
    inferred(10, "2025-06-01 14:00", 7201),
    inferred(11, "2025-06-02 00:00", 43200),
    inferred(12, "2025-06-02 01:00", 43201),
    inferred(13, "2025-06-02 02:00", None),
    # 手動で位置を決めた写真。時刻なしは最後に並ぶ
    photo_row(14, taken_at="2025-06-04 10:00", lat=35.6, lon=139.6, loc_source="manual",
              favorite=1, rotation=180),
    photo_row(15, taken_at="", lat=35.7, lon=139.7, loc_source="manual", favorite=1, hidden=1),
    # 位置なし。近い錨があるもの、候補があるもの、Decision の判定があるもの
    noloc(16, "2025-06-01 10:05"),
    noloc(17, "2025-06-01 12:30", display=None, drive_id="d-has-deriv"),
    noloc(18, "2025-06-03 09:30", display=None, drive_id="d-no-deriv"),
    noloc(19, "2025-07-30 10:00"),                       # どの錨からも7日より離れている
    noloc(20, "x"),                                      # 読めない時刻
    noloc(21, "", loc_source=""),                        # 時刻なし。loc_source が空文字
    noloc(22, None),                                     # 時刻が NULL
    noloc(23, "2025-06-01 10:30"),
    noloc(24, "2025-06-01 10:40"),
    noloc(25, "2025-06-01 10:50"),
    noloc(26, "2025-06-02 03:00"),
    noloc(27, "2025-06-01 23:30"),                       # 1時間後だが日付が変わる
    noloc(28, "2025-06-02 00:30"),
    noloc(29, "2025-06-03 00:10"),
    noloc(30, "2025-06-03 22:00"),                       # 前の写真から12時間を超える
]

# photo_quality: (photo_id, group_id, group_kind, rank_in_group, recommended, score, source_md5)
QUALITY = [
    (1, 1, "similar", 1, 1, 0.9, None), (2, 1, "similar", 2, 0, 0.4, None),
    (6, 2, "exact", 1, 1, 0.8, None), (7, 2, "exact", 2, 0, 0.7, None),
    (8, 3, "exact", 1, 0, 0.5, "stale"),                 # 原本が変わった後の古い解析: 数えない
]

# photo_quality_features: (photo_id, content_sha256, error, source_md5)
FEATURES = [
    (1, "sha-a", "", None), (2, "sha-a", "", None), (6, "sha-b", "", None),
    (7, "sha-c", "", None), (16, "sha-d", "", None),
    (8, "sha-e", "", "stale"),                           # 古い: 解析済みに数えない
    (9, "sha-f", "failed", None),                        # エラー: eligible に数えない
    (10, None, "", None),                                # ハッシュなし
]

# photo_ai_features: (photo_id, content_sha256, embedding_model, recommended, error, source_md5)
AI_FEATURES = [
    (1, "sha-a", "gemini-embedding-2", 1, "", None),
    (2, "sha-a", "gemini-embedding-2", None, "", None),
    (6, "sha-b", "gemini-embedding-2", 0, "", None),
    (7, "sha-c", "embeddinggemma-2", 1, "", None),
    (16, "sha-d", "gemini-embedding-2", 1, "boom", None),   # エラー
    (8, "sha-e", "gemini-embedding-2", 1, "", "stale"),     # 古い
]

AI_GROUPS = [("ai-1", "judged", 1), ("ai-2", "pending", 1), ("ai-3", "judged", 0)]
# (group_id, photo_id, rank_in_group, recommended)
AI_MEMBERS = [("ai-1", 6, 1, 1), ("ai-1", 7, 2, 0), ("ai-1", 8, 3, 0),
              ("ai-2", 9, 1, 0), ("ai-3", 10, 1, 0)]

# photo_loc_clues: (photo_id, display, source_md5, source_mtime, clue, kind, error)
CLUES = [
    (16, "derivatives/p16.jpg", "m16", 160.0, 0.9, "landmark", ""),            # 通る
    (17, "derivatives/d-has-deriv.jpg", "m17", 170.0, 0.3, "street", ""),      # 通る（derivative を拾う）
    (19, "derivatives/p19.jpg", "m19", 190.0, 0.1, "street", ""),              # 判定済みだが通らない
    (23, "derivatives/p23.jpg", "m23", 230.0, 0.05, "landmark", ""),           # 通る（landmark）
    (24, "derivatives/p24.jpg", "m24", 240.0, 0.9, "street", "x"),             # エラーは通らない
    (25, "derivatives/p25.jpg", "old", 250.0, 0.9, "street", ""),              # 原本が違う: 判定待ち
    (26, "derivatives/p26.jpg", "m26", 260.0, 0.5, "street", ""),              # 通る
    (27, "derivatives/p27.jpg", "m27", 270.0, None, "", "unavailable"),        # 値なし
    (28, "derivatives/other.jpg", "m28", 280.0, 0.9, "street", ""),            # 画像が違う: 判定待ち
]

# photo_loc_runs: (run_id, status)
RUNS = [("loc-a", "done"), ("loc-b", "done"), ("loc-c", "error"), ("loc-d", "pending")]

# photo_loc_suggestions: (photo_id, scope, place_name, day_place_name, precision, confidence,
#                         evidence_json, lat, lon, geocode_title)
SUGGESTIONS = [
    (16, "photo", "サンプル寺院", "架空の街", "landmark", "medium",
     json.dumps(["看板の文字", "建物の形"], ensure_ascii=False), 10.5, 20.5, "Sample Temple"),
    (18, "day", "架空の街", "架空の街", "city", "low", "not json", None, None, ""),
    (21, "day", "架空の地方", "", "region", "unknown", '{"a": 1}', 11.5, 21.5, "Sample Region"),
    (26, "photo", "サンプル広場", "", "district", "high", '["x"]', 12.5, 22.5, "Sample Square"),
    (4, "photo", "位置のある写真の候補", "", "city", "low", "[]", 13.5, 23.5, "Located"),
]

LOC_STATUS = {"decision": "ready", "at": "2025-06-20T10:00:00+09:00", "extra": 1}


def _seed():
    marks = ",".join("?" * len(PHOTO_COLUMNS))
    store.dbm(f"INSERT INTO photos({','.join(PHOTO_COLUMNS)}) VALUES({marks})",
              [tuple(p[c] for c in PHOTO_COLUMNS) for p in PHOTOS])
    by_id = {p["id"]: p for p in PHOTOS}
    store.dbm("""INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime,
                   content_sha256, group_id, group_kind, rank_in_group, recommended, score,
                   algorithm, analyzed_at)
                 VALUES(?, ?, ?, ?, 'sha', ?, ?, ?, ?, ?, 'test', '2025-06-10T00:00:00')""",
              [(pid, by_id[pid]["display"], md5 or by_id[pid]["md5"], by_id[pid]["mtime"],
                gid, kind, rank, rec, score)
               for pid, gid, kind, rank, rec, score, md5 in QUALITY])
    store.dbm("""INSERT INTO photo_quality_features(photo_id, display, source_md5, source_mtime,
                   content_sha256, error, analyzed_at)
                 VALUES(?, ?, ?, ?, ?, ?, '2025-06-10T00:00:00')""",
              [(pid, by_id[pid]["display"], md5 or by_id[pid]["md5"], by_id[pid]["mtime"],
                sha, error)
               for pid, sha, error, md5 in FEATURES])
    store.dbm("""INSERT INTO photo_ai_features(photo_id, display, source_md5, source_mtime,
                   content_sha256, embedding_model, embedding_dim, embedding, recommended,
                   analyzed_at, error)
                 VALUES(?, ?, ?, ?, ?, ?, 768, x'00', ?, '2025-06-10T00:00:00', ?)""",
              [(pid, by_id[pid]["display"], md5 or by_id[pid]["md5"], by_id[pid]["mtime"],
                sha, model, rec, error)
               for pid, sha, model, rec, error, md5 in AI_FEATURES])
    store.dbm("""INSERT INTO photo_ai_groups(group_id, kind, algorithm, analyzed_at, status,
                   same_scene) VALUES(?, 'scene', 'test', '2025-06-10T00:00:00', ?, ?)""",
              AI_GROUPS)
    store.dbm("""INSERT INTO photo_ai_group_members(group_id, photo_id, rank_in_group,
                   recommended) VALUES(?, ?, ?, ?)""", AI_MEMBERS)
    store.dbm("""INSERT INTO photo_loc_clues(photo_id, display, source_md5, source_mtime, clue,
                   kind, model, analyzed_at, error)
                 VALUES(?, ?, ?, ?, ?, ?, 'test', '2025-06-10T00:00:00', ?)""", CLUES)
    store.dbm("""INSERT INTO photo_loc_runs(run_id, day, model, prompt_version, member_ids_json,
                   sent_ids_json, status, analyzed_at)
                 VALUES(?, '2025-06-01', 'test', 'loc-v2', '[]', '[]', ?, '2025-06-10T00:00:00')""",
              RUNS)
    store.dbm("""INSERT INTO photo_loc_suggestions(photo_id, run_id, scope, place_name,
                   day_place_name, precision, confidence, evidence_json, lat, lon, geocode_title,
                   model, analyzed_at)
                 VALUES(?, 'loc-a', ?, ?, ?, ?, ?, ?, ?, ?, ?, 'sample-model',
                        '2025-06-10T00:00:00')""", SUGGESTIONS)
    store.setting_set("photo_loc_status", json.dumps(LOC_STATUS))


@contextmanager
def seeded():
    """合成DBを本物のスキーマで作って行を入れ、その間 `store` と derivative の置き場がそれを指すようにする。"""
    with tempfile.TemporaryDirectory() as tmp:
        data = Path(tmp).resolve()
        (data / "derivatives").mkdir()
        for drive_id in DERIVATIVE_IDS:
            (data / "derivatives" / f"{drive_id}.jpg").write_bytes(b"")
        patches = [patch.object(store, "DB_PATH", data / "catalog.db"),
                   patch.object(store, "_db", None),
                   patch.object(admin, "DATA_DIR", data),
                   patch.object(admin, "DERIV_DIR", data / "derivatives"),
                   patch.object(loc_suggest, "DERIV_DIR", data / "derivatives"),
                   patch.object(loc_suggest, "DATA_DIR", data)]
        for p in patches:
            p.start()
        try:
            _seed()
            yield
        finally:
            if store._db is not None:
                store._db.close()
            for p in reversed(patches):
                p.stop()


def build_catalog_sql():
    """合成DB（スキーマ→行の順）のSQL。Rust のテストがこれを流し込んで同じDBを作る。"""
    with seeded():
        return "\n".join(store.db().iterdump()) + "\n"


# /api/admin/photos に流すクエリ。ページ境界・per の上限・各 filter・不正な値・`int()` の寛容さ
PHOTO_TARGETS = [
    "/api/admin/photos",
    "/api/admin/photos?page=1&per=50&filter=all",
    # ページ境界: 30件を per=7 で 5ページ。最後のページ、その先（丸められる）、0・負数
    *[f"/api/admin/photos?per=7&page={n}" for n in (1, 2, 5, 6, 0, -3)],
    "/api/admin/photos?per=30", "/api/admin/photos?per=29&page=2", "/api/admin/photos?per=15&page=2",
    # per: 0 は 1 に、負数は 1 に、上限 200 に丸められる
    "/api/admin/photos?per=0&page=3", "/api/admin/photos?per=-5&page=2",
    "/api/admin/photos?per=200", "/api/admin/photos?per=201", "/api/admin/photos?per=1000",
    # 各 filter（件数の少ないものは1ページ、noloc・all は複数ページ）
    *[f"/api/admin/photos?filter={f}&per=4&page={n}"
      for f in ("noloc", "suggest") for n in (1, 2, 3, 4, 9)],
    *[f"/api/admin/photos?filter={f}&per=3&page=1"
      for f in ("fav", "hidden", "inferred", "manual")],
    "/api/admin/photos?filter=inferred&per=3&page=3",
    "/api/admin/photos?filter=noloc", "/api/admin/photos?filter=suggest",
    "/api/admin/photos?filter=fav", "/api/admin/photos?filter=hidden",
    "/api/admin/photos?filter=inferred", "/api/admin/photos?filter=manual",
    # 未知の filter は all（大文字小文字を区別する）。空の値は無かったことになる
    "/api/admin/photos?filter=bogus&per=2", "/api/admin/photos?filter=FAV&per=2",
    "/api/admin/photos?filter=&per=2", "/api/admin/photos?filter=fav%00&per=2",
    "/api/admin/photos?filter=%3Cscript%3E&per=2", "/api/admin/photos?filter=noloc&filter=fav&per=2",
    # parse_qs の癖: 空の値・`=` の無い項目・同じ名前・`+`・パーセントエンコードの名前
    "/api/admin/photos?page=&per=", "/api/admin/photos?page&per=3", "/api/admin/photos?page=2&page=3&per=5",
    "/api/admin/photos?page=&page=2&per=5", "/api/admin/photos?%70age=2&%70er=5",
    "/api/admin/photos?page=+2&per=5", "/api/admin/photos?per=5;page=2", "/api/admin/photos?per=5#page=2",
    "/api/admin/photos?x=1&per=5&y", "/api/admin/photos?page=2&per=5&page=bad",
    # int() が受け付ける形
    "/api/admin/photos?page=%202%20&per=5", "/api/admin/photos?page=%2B2&per=5",
    "/api/admin/photos?page=1_0&per=5", "/api/admin/photos?page=0_2&per=5",
    "/api/admin/photos?page=%EF%BC%92&per=%EF%BC%95", "/api/admin/photos?page=%E0%A5%A9&per=5",
    "/api/admin/photos?page=%C2%A02&per=5", "/api/admin/photos?page=007&per=005",
    "/api/admin/photos?page=99999999999999999999&per=5", "/api/admin/photos?page=-99999999999999999999&per=5",
    "/api/admin/photos?per=99999999999999999999", "/api/admin/photos?per=-99999999999999999999",
    # int() が受け付けない形: 400
    "/api/admin/photos?page=abc", "/api/admin/photos?per=abc", "/api/admin/photos?page=1.5",
    "/api/admin/photos?per=1e3", "/api/admin/photos?page=%20", "/api/admin/photos?page=1__0",
    "/api/admin/photos?page=_1", "/api/admin/photos?page=1_", "/api/admin/photos?page=0x10",
    "/api/admin/photos?page=%2B-1", "/api/admin/photos?per=%EF%BC%8B%EF%BC%91",
    "/api/admin/photos?page=%1C2", "/api/admin/photos?page=2%E2%80%A8",
    "/api/admin/photos?page=%ff", "/api/admin/photos?page=2&per=NaN&filter=fav",
    f"/api/admin/photos?page={'9' * 4300}&per=5", f"/api/admin/photos?page={'9' * 4301}&per=5",
    # 生のパスの前方一致（`/api/admin/photos` で始まれば当たる）
    "/api/admin/photosX?per=2", "/api/admin/photos/?per=2", "/api/admin/photos.json?per=2",
    # 先頭の `//` は http.server が `/` に畳んでから判定する
    "//api/admin/photos?per=2", "///api/admin/photos?filter=fav&per=2",
]

# /api/admin/status と、前方一致で同じものが返る形
STATUS_TARGETS = ["/api/admin/status", "/api/admin/status?x=1", "/api/admin/statusX",
                  "/api/admin/status/?filter=fav", "//api/admin/status"]

# 空のテーブルに流すもの
EMPTY_TARGETS = ["/api/admin/photos", "/api/admin/photos?filter=noloc&page=3",
                 "/api/admin/photos?filter=suggest&per=0", "/api/admin/photos?page=abc"]

TABLES = ("photo_loc_suggestions", "photo_loc_runs", "photo_loc_clues", "photo_ai_group_members",
          "photo_ai_groups", "photo_ai_features", "photo_embeddings", "photo_quality_features",
          "photo_quality", "settings", "photos")


@contextmanager
def _http():
    server = ThreadingHTTPServer(("127.0.0.1", 0), handlers.Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_port
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def _get(port, target):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", target, headers={"Host": "127.0.0.1"})
        response = conn.getresponse()
        return response.status, json.loads(response.read())
    finally:
        conn.close()


def _without_unstable(body):
    body = dict(body)
    body.pop("disk", None)
    body.pop("server_time", None)
    return body


def _fetch(port, targets):
    out = []
    for target in targets:
        status, body = _get(port, target)
        out.append({"target": target, "status": status, "body": _without_unstable(body)})
    return out


def build_cases():
    """合成DBに対する現行 Python の出力（JSON に直せる dict）。"""
    cases = {"derivative_ids": DERIVATIVE_IDS}
    with seeded(), patch.object(handlers.Handler, "log_message", lambda *a, **k: None):
        with _http() as port:
            cases["photos"] = _fetch(port, PHOTO_TARGETS)
            cases["status_targets"] = _fetch(port, STATUS_TARGETS)
        status = {"gemini": _without_unstable(admin.api_admin_status())}
        with patch.object(admin, "EMBED_MODEL", "embeddinggemma-2"):
            status["local"] = _without_unstable(admin.api_admin_status())
        # 位置推定の窓（GEOFRAME_INFER_MIN）を変えたとき: 内訳のラベルと、日をまとめる間隔が変わる
        cases["infer_variants"] = {}
        for name, minutes in (("infer30", "30"), ("infer1000", "1000")):
            windows = sorted({float(minutes) * 60, 2 * 3600, 12 * 3600})
            with patch.object(geo, "INFER_WINDOWS_S", windows), \
                    patch.object(loc_suggest, "GROUP_GAP_S", max(windows)):
                with _http() as port:
                    listing = _fetch(port, ["/api/admin/photos?filter=inferred&per=20"])
                cases["infer_variants"][name] = {
                    "env": {"GEOFRAME_INFER_MIN": minutes},
                    "status": _without_unstable(admin.api_admin_status()),
                    "photos": listing}
        # 前回の処理時の状態: 無い・壊れている
        store.dbx("DELETE FROM settings")
        status["no_settings"] = _without_unstable(admin.api_admin_status())
        store.setting_set("photo_loc_status", "{broken")
        status["broken_settings"] = _without_unstable(admin.api_admin_status())
        cases["status"] = status
        # 空のテーブル
        for table in TABLES:
            store.dbx(f"DELETE FROM {table}")
        with _http() as port:
            cases["empty_status"] = _fetch(port, ["/api/admin/status"])[0]["body"]
            cases["empty_photos"] = _fetch(port, EMPTY_TARGETS)
    return cases


def render(cases):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま）。差分が読めるよう、
    配列は1要素1行にする。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    lines = []
    for key in sorted(cases):
        value = cases[key]
        if isinstance(value, list) and value and isinstance(value[0], dict):
            body = ",\n".join("  " + dump(item) for item in value)
            lines.append(f" {dump(key)}: [\n{body}\n ]")
        else:
            lines.append(f" {dump(key)}: {dump(value)}")
    return "{\n" + ",\n".join(lines) + "\n}\n"
