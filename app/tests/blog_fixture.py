"""旅行ブログの読み取りAPI（`/api/blog/trips`・`trip`・`candidates`・`draft`）の契約テスト用フィクスチャ。

合成の旅程JSON・合成の下書き・書き出しの印・合成の写真の行（本物のスキーマの一時DB）を作り、現行の
`handlers.Handler` を実際にHTTPで動かして、下の `TARGETS` を順にGETする。各要求について、状態コードと
応答のJSON（JSONでなければ null）を記録する。
Rust の結合テスト（crates/geoframe/tests/blog_api.rs）は、同じ旅程・下書き・書き出しの印をディレクトリに作り直し、
同じDB（blog-catalog.sql）を開いて同じ要求を流し、状態・応答が JSON として一致することを確かめる。

再生成: `python3 tools/gen_blog_fixture.py`（差分の検査は `--check`）。
実在の地名・座標・ファイル名・旅程は使わない。座標は 35.1/139.1 のような丸い値、地名は「架空県」「見本市」「サンプル町」、
旅程は `sample-trip-a` のような合成。写真の行には座標を入れてある（応答に出ないことを Rust のテストが確かめる）。

載せない形（Python が未処理の例外で応答を返さない。Rust は 400 か一覧から外す。server.md §20.8）: 旅程が配列、
`date_range` が文字列、`segments` の要素がオブジェクトでない、`locations` に文字列でない要素、`activities` の要素が
オブジェクトでない、UTF-8 でないファイル。Rust の単体テストが別に確かめる。
"""
import contextlib
import http.client
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from geoframe import handlers, store, travel_blog

from tests.admin_api_fixture import _http

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_DIR = ROOT / "crates/geoframe/tests/fixtures"
FIXTURE_PATH = FIXTURE_DIR / "blog-python.json"
CATALOG_SQL_PATH = FIXTURE_DIR / "blog-catalog.sql"


def j(obj):
    return json.dumps(obj, ensure_ascii=False, indent=2)


def segment(id, date, **over):
    seg = {"id": id, "date": date, "title": f"{id} の日", "timezone": "Asia/Sample",
           "locations": ["見本市", "サンプル町"], "start_local": "09:00", "end_local": "18:00",
           "activities": [{"time": "09:00", "title": "出発"}, {"title": "昼食"}]}
    seg.update(over)
    return seg


def plan(trip_id, segments, **over):
    doc = {"schema_version": "1.0", "trip_id": trip_id, "title": "テスト旅程", "subtitle": "見本の旅",
           "date_range": {"start": "2025-06-06", "end": "2025-06-12"}, "segments": segments}
    doc.update(over)
    return doc


# ---- 旅程JSON（PLAN_DIR のファイル名 → 中身の文字列、または {"dir": True}） --------------------

PLANS = {
    "sample-trip-a.json": j(plan("sample-trip-a", [
        segment("d1", "2025-06-06", title="初日", locations=["見本市", "サンプル町", "第三の地"]),
        # 開始・終了・タイムゾーン・タイトル・場所なし → 既定の 00:00〜23:59、タイトルは日付
        {"id": "d2", "date": "2025-06-07"},
        segment("d3", "2025-06-12", start_local="00:00", end_local="23:59", activities=[]),
        # 0 埋めのない開始（文字列比較なので 0 埋めの行は入らない）
        segment("d4", "2025-06-09", start_local="9:00", end_local="9:59"),
        # 終了が開始より前 → 0 件
        segment("d5", "2025-06-09", start_local="18:00", end_local="09:00"),
        # 日付をまたぐ境目（23:59 は入り、秒つきと翌日 00:00 は入らない）
        segment("d6", "2025-06-10", start_local="22:00", end_local="23:59", timezone="America/Sample"),
        segment("d7", "2025-06-11", start_local="00:00", end_local="01:00", timezone="America/Sample"),
        # 日付なし → どの行にも当たらない
        {"id": "d8", "title": "日付なし"},
        # 場所が空・活動の時刻が null
        segment("d9", "2025-06-13", locations=[], activities=[{"time": None, "title": "名前"}]),
    ])),
    "sample-trip-b.json": j(plan("sample-trip-b", [segment("b1", "2025-06-06")],
                                 title="", subtitle=None, date_range={})),
    # ファイル名と trip_id が違う: 一覧には中の trip_id で載る。ファイル名では形式エラー
    "mismatch-file.json": j(plan("sample-trip-c", [segment("c1", "2025-06-06")])),
    "bad-json.json": "{broken",
    "segments-empty.json": j(plan("segments-empty", {})),
    "segments-string.json": j(plan("segments-string", "abc")),
    "segments-null.json": j(plan("segments-null", None)),
    # trip_id が数値: 一覧には "5" で載るが、ファイルの trip_id（5）と "5" は等しくないので 400
    "5.json": j(plan(5, [])),
    "title-number.json": j(plan("title-number", [], title=12, subtitle=3.5, date_range={"start": 20250606})),
    "dir-plan.json": {"dir": True},
    ".dot-plan.json": j(plan("dot-plan", [])),
    "notes.txt": "plan ではない",
    "invalid-id.json": j(plan("Bad_ID", [])),
    "sample-trip-odd.json": j(plan("sample-trip-odd", [
        # id なし・場所が文字列・活動の時刻/名前が null
        {"date": "2025-06-06", "title": None, "locations": "ab", "start_local": "00:00",
         "activities": [{"time": None, "title": None}, {"title": "x"}, {"time": 9}]},
        # id が数値・タイトルが数値・タイムゾーンが 0
        {"id": 5, "date": "2025-06-07", "title": 7, "timezone": 0, "locations": ["一つだけ"]},
        # 同じ id の区間が 2 つ: 初期案の本文は後の区間のもの
        {"id": "dup", "date": "2025-06-06", "title": "前の区間", "activities": []},
        {"id": "dup", "date": "2025-06-07", "title": "後の区間", "activities": []},
    ])),
}

# ---- 下書き（DRAFT_DIR のファイル名 → 中身の文字列、または {"dir": True}） ----------------------


def image(photo_id, **over):
    img = {"photo_id": photo_id, "src": f"/derivatives/{photo_id}.jpg", "alt": "代替", "caption": "説明",
           "score": 0.5, "selection_reason": ["x"]}
    img.update(over)
    return img


def chapter(images, **over):
    ch = {"id": "b1", "date": "2025-06-06", "title": "章", "location_label": "見本市", "body": "本文",
          "images": images}
    ch.update(over)
    return ch


def manifest(trip_id, chapters, **over):
    doc = {"schema_version": "1.0", "trip_id": trip_id, "title": "題", "subtitle": "副題",
           "status": "draft", "summary": "概要", "chapters": chapters}
    doc.update(over)
    return doc


DRAFTS = {
    # 長さの切り詰め・photo_id の文字列化・既定値の補い・知らないキーの保持・status の据え置き
    "sample-trip-b.json": j(manifest("sample-trip-b", [
        chapter([image(5), image("７"), image("0", alt=None, caption=None), image("12", extra={"k": 1})],
                id="c" * 150, date="2025-06-06-extra", title="t" * 250, location_label="l" * 350,
                body="あ" * 20100, note="残る"),
        chapter([]),
    ], title="題" * 250, subtitle="副" * 350, summary="概" * 4100, status="published", unknown=[1, 2])),
    # 旅程が無くても保存済みなら返す
    "draft-without-plan.json": j(manifest("draft-without-plan", [chapter([image(1)])], status="reviewed")),
    "draft-invalid-json.json": "{broken",
    "draft-array.json": "[]",
    "draft-id-mismatch.json": j(manifest("other-trip", [])),
    "draft-id-missing.json": j({"status": "draft", "chapters": []}),
    "draft-bad-status.json": j(manifest("draft-bad-status", [], status="done")),
    "draft-status-missing.json": j({"trip_id": "draft-status-missing", "chapters": []}),
    "draft-chapters-not-list.json": j(manifest("draft-chapters-not-list", {})),
    "draft-chapters-41.json": j(manifest("draft-chapters-41", [chapter([]) for _ in range(41)])),
    "draft-chapters-40.json": j(manifest("draft-chapters-40", [chapter([]) for _ in range(40)])),
    "draft-chapter-bad.json": j(manifest("draft-chapter-bad", [chapter([]), "x"])),
    "draft-chapter-no-images.json": j(manifest("draft-chapter-no-images", [{"id": "x"}])),
    "draft-images-101.json": j(manifest("draft-images-101", [chapter([image(i + 1) for i in range(101)])])),
    "draft-images-100.json": j(manifest("draft-images-100", [chapter([image(i + 1) for i in range(100)])])),
    "draft-total-201.json": j(manifest("draft-total-201", [
        chapter([image(i + 1) for i in range(100)]), chapter([image(i + 1) for i in range(100)]),
        chapter([image(1)])])),
    "draft-total-200.json": j(manifest("draft-total-200", [
        chapter([image(i + 1) for i in range(100)]), chapter([image(i + 1) for i in range(100)])])),
    "draft-photo-id-letters.json": j(manifest("draft-photo-id-letters", [chapter([image("1a")])])),
    "draft-photo-id-zero.json": j(manifest("draft-photo-id-zero", [chapter([image(0)])])),
    "draft-photo-id-empty.json": j(manifest("draft-photo-id-empty", [chapter([image("")])])),
    "draft-photo-id-null.json": j(manifest("draft-photo-id-null", [chapter([image(None)])])),
    "draft-photo-id-superscript.json": j(manifest("draft-photo-id-superscript", [chapter([image("²")])])),
    "draft-image-not-object.json": j(manifest("draft-image-not-object", [chapter([7])])),
    "draft-dir.json": {"dir": True},
}

# 書き出しの印（EXPORT_DIR からの相対。末尾が / のものはディレクトリだけ）
EXPORTS = ["sample-trip-b/index.html", "sample-trip-a/"]


# ---- 合成の写真 -------------------------------------------------------------------------------

COLUMNS = ("id", "source", "drive_id", "display", "name", "md5", "mtime", "taken_at", "lat", "lon",
           "loc_source", "place", "pref", "city", "title", "ai_caption", "favorite", "hidden")


def photo(id, taken_at, **over):
    row = {"id": id, "source": "drive", "drive_id": f"drive-{id}", "display": f"derivatives/p{id:03d}.jpg",
           "name": f"IMG_{id:04d}.jpg", "md5": f"m{id}", "mtime": id * 10.0, "taken_at": taken_at,
           "lat": 35.1, "lon": 139.1, "loc_source": "exif", "place": None, "pref": "架空県",
           "city": None, "title": None, "ai_caption": None, "favorite": 0, "hidden": 0}
    row.update(over)
    return row


def _photos():
    rows = [
        # d1（2025-06-06 09:00〜18:00）: 境目
        photo(1, "2025-06-06 08:59"),
        # 同じ分（09:00）に 6 枚: 点・お気に入り・id の順で代表が決まり、alternates は 3 枚まで
        photo(2, "2025-06-06 09:00", loc_source=None),
        photo(3, "2025-06-06 09:00", loc_source="inferred"),
        photo(4, "2025-06-06 09:00", loc_source="drive_meta", ai_caption="説明つき"),
        photo(5, "2025-06-06 09:00", loc_source="exif", title="題つき"),
        photo(6, "2025-06-06 09:00", loc_source="exif", title="題つき"),
        photo(7, "2025-06-06 09:00", loc_source="manual", favorite=1),
        photo(8, "2025-06-06 09:01", hidden=1),
        photo(9, "2025-06-06 09:01", display=None),
        photo(10, "2025-06-06 09:01", display="", loc_source=""),
        photo(11, "2025-06-06 12:30", loc_source="inferred", city="見本市", place="サンプル町"),
        photo(12, "2025-06-06 12:30", city="同名", place="同名"),
        photo(13, "2025-06-06 17:59", city="", place="場所だけ", hidden=None),
        photo(14, "2025-06-06 18:00", city=" ", place=None),
        photo(15, "2025-06-06 18:01"),
        photo(16, "2025-06-06 18:00:30"),
        photo(17, "2025-06-06 10:10", display="\\derivatives\\win.jpg"),
        photo(18, "2025-06-06 10:11", display="../escape.jpg"),
        photo(19, "2025-06-06 10:12", display=".hidden.jpg"),
        photo(20, "2025-06-06 10:13", display="a/../b.jpg"),
        photo(21, "2025-06-06 10:14", display="/lead/slash.jpg"),
        photo(22, "2025-06-06 10:15", display="a/./b.jpg"),
        photo(23, "2025-06-06 10:16"),
        # d2（2025-06-07 の終日）: 00:00・23:59 は入り、翌日 00:00 は入らない
        photo(30, "2025-06-07 00:00"),
        photo(31, "2025-06-07 23:59", favorite=1),
        photo(32, "2025-06-08 00:00"),
        # d4/d5（2025-06-09）
        photo(40, "2025-06-09 09:30"),
        photo(41, "2025-06-09 12:00"),
        photo(42, "2025-06-09 9:30"),
        # d6/d7: 日付をまたぐ
        photo(50, "2025-06-10 21:59"),
        photo(51, "2025-06-10 22:00"),
        photo(52, "2025-06-10 23:59"),
        photo(53, "2025-06-10 23:59:30"),
        photo(54, "2025-06-11 00:00"),
        photo(55, "2025-06-11 01:00"),
        photo(56, "2025-06-11 01:01"),
        # taken_at が NULL・空
        photo(60, None),
        photo(61, ""),
    ]
    # d3（2025-06-12）: 45 の分（グループ）。お気に入りは末尾の 2 つと途中の 1 つ。最後の分は 2 枚
    for k in range(45):
        rows.append(photo(100 + k, f"2025-06-12 10:{k:02d}", favorite=1 if k in (20, 43, 44) else 0,
                          city="見本市" if k % 5 == 0 else None))
    rows.append(photo(150, "2025-06-12 10:44", favorite=0, title="同じ分の別の写真"))
    # d9（2025-06-13）: 少数（6 枚未満）
    for k, hhmm in enumerate(["09:00", "10:00", "11:00"]):
        rows.append(photo(200 + k, f"2025-06-13 {hhmm}", favorite=1 if k == 1 else 0))
    # sample-trip-odd の区間（2025-06-06 と 06-07）にも同じ行が当たる
    return rows


PHOTOS = _photos()


@contextlib.contextmanager
def seeded():
    """旅程・下書き・書き出し・DB を一時ディレクトリに作り、その間 `travel_blog` と `store` がそれを指すようにする。"""
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp).resolve()
        plan_dir, data = base / "plans", base / "data"
        drafts, exports = data / "travel_blog" / "drafts", data / "blog_exports"
        for d in (plan_dir, drafts, exports):
            d.mkdir(parents=True)
        for directory, files in ((plan_dir, PLANS), (drafts, DRAFTS)):
            for name, content in files.items():
                if isinstance(content, dict):
                    (directory / name).mkdir()
                else:
                    (directory / name).write_text(content, encoding="utf-8")
        for mark in EXPORTS:
            target = exports / mark
            target.parent.mkdir(parents=True, exist_ok=True)
            if mark.endswith("/"):
                target.mkdir(exist_ok=True)
            else:
                target.write_text("<html></html>", encoding="utf-8")
        patches = [patch.object(store, "DB_PATH", data / "catalog.db"),
                   patch.object(store, "_db", None),
                   patch.object(travel_blog, "PLAN_DIR", plan_dir),
                   patch.object(travel_blog, "DRAFT_DIR", drafts),
                   patch.object(travel_blog, "EXPORT_DIR", exports),
                   patch.object(travel_blog, "DATA_DIR", data)]
        for p in patches:
            p.start()
        try:
            marks = ",".join("?" * len(COLUMNS))
            store.dbm(f"INSERT INTO photos({','.join(COLUMNS)}) VALUES({marks})",
                      [tuple(p[c] for c in COLUMNS) for p in PHOTOS])
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


# ---- 要求 -------------------------------------------------------------------------------------

LONG_OK = "a" * 80
LONG_NG = "a" * 81

TARGETS = [
    # E7 一覧
    "/api/blog/trips", "/api/blog/trips?x=1", "/api/blog/trips?trip_id=sample-trip-a", "//api/blog/trips",
    # E8 旅程そのもの
    "/api/blog/trip?trip_id=sample-trip-a", "/api/blog/trip?trip_id=sample-trip-b",
    "/api/blog/trip?trip_id=sample-trip-odd", "/api/blog/trip?trip_id=title-number",
    "/api/blog/trip?trip_id=mismatch-file", "/api/blog/trip?trip_id=sample-trip-c",
    "/api/blog/trip?trip_id=bad-json", "/api/blog/trip?trip_id=dir-plan",
    "/api/blog/trip?trip_id=segments-empty", "/api/blog/trip?trip_id=segments-string",
    "/api/blog/trip?trip_id=segments-null", "/api/blog/trip?trip_id=5", "/api/blog/trip?trip_id=dot-plan",
    "/api/blog/trip?trip_id=.dot-plan", "/api/blog/trip?trip_id=notes", "/api/blog/trip?trip_id=nonexistent",
    # trip_id の境界
    "/api/blog/trip", "/api/blog/trip?trip_id=", "/api/blog/trip?trip_id=&trip_id=sample-trip-a",
    "/api/blog/trip?trip_id=sample-trip-a&trip_id=zzz", "/api/blog/trip?trip_id=zzz&trip_id=sample-trip-a",
    "/api/blog/trip?trip_id=sample%2Dtrip%2Da", "/api/blog/trip?%74rip_id=sample-trip-a",
    "/api/blog/trip?trip_id=sample-trip-a%0A", "/api/blog/trip?trip_id=sample-trip-a+",
    "/api/blog/trip?trip_id=%20sample-trip-a", "/api/blog/trip?trip_id=Sample-Trip-A",
    "/api/blog/trip?trip_id=sample_trip_a", "/api/blog/trip?trip_id=-sample", "/api/blog/trip?trip_id=a",
    "/api/blog/trip?trip_id=a-", "/api/blog/trip?trip_id=0", "/api/blog/trip?trip_id=%E3%81%82",
    "/api/blog/trip?trip_id=%ff", "/api/blog/trip?trip_id=../plans/sample-trip-a",
    "/api/blog/trip?trip_id=sample-trip-a/", "/api/blog/trip?trip_id=sample-trip-a.json",
    f"/api/blog/trip?trip_id={LONG_OK}", f"/api/blog/trip?trip_id={LONG_NG}",
    "/api/blog/trip?trip_id=sample-trip-a;trip_id=x", "/api/blog/trip?trip_id=sample-trip-a#x",
    "/api/blog/trip?trip_id=sample%00", "/api/blog/trip?trip_id=nonexistent&trip_id=sample-trip-a",
    # E9 候補
    "/api/blog/candidates?trip_id=sample-trip-a", "/api/blog/candidates?trip_id=sample-trip-odd",
    "/api/blog/candidates?trip_id=sample-trip-b", "/api/blog/candidates?trip_id=title-number",
    "/api/blog/candidates?trip_id=segments-empty", "/api/blog/candidates?trip_id=segments-string",
    "/api/blog/candidates?trip_id=mismatch-file", "/api/blog/candidates?trip_id=bad-json",
    "/api/blog/candidates?trip_id=nonexistent", "/api/blog/candidates", "/api/blog/candidates?trip_id=",
    "/api/blog/candidates?trip_id=BAD", f"/api/blog/candidates?trip_id={LONG_NG}",
    "/api/blog/candidates?trip_id=&trip_id=sample-trip-b",
    # E10 下書き（保存済み・生成）
    "/api/blog/draft?trip_id=sample-trip-a", "/api/blog/draft?trip_id=sample-trip-b",
    "/api/blog/draft?trip_id=sample-trip-odd", "/api/blog/draft?trip_id=title-number",
    "/api/blog/draft?trip_id=draft-without-plan", "/api/blog/draft?trip_id=draft-invalid-json",
    "/api/blog/draft?trip_id=draft-array", "/api/blog/draft?trip_id=draft-id-mismatch",
    "/api/blog/draft?trip_id=draft-id-missing", "/api/blog/draft?trip_id=draft-bad-status",
    "/api/blog/draft?trip_id=draft-status-missing", "/api/blog/draft?trip_id=draft-chapters-not-list",
    "/api/blog/draft?trip_id=draft-chapters-41", "/api/blog/draft?trip_id=draft-chapters-40",
    "/api/blog/draft?trip_id=draft-chapter-bad", "/api/blog/draft?trip_id=draft-chapter-no-images",
    "/api/blog/draft?trip_id=draft-images-101", "/api/blog/draft?trip_id=draft-images-100",
    "/api/blog/draft?trip_id=draft-total-201", "/api/blog/draft?trip_id=draft-total-200",
    "/api/blog/draft?trip_id=draft-photo-id-letters", "/api/blog/draft?trip_id=draft-photo-id-zero",
    "/api/blog/draft?trip_id=draft-photo-id-empty", "/api/blog/draft?trip_id=draft-photo-id-null",
    "/api/blog/draft?trip_id=draft-photo-id-superscript", "/api/blog/draft?trip_id=draft-image-not-object",
    "/api/blog/draft?trip_id=draft-dir", "/api/blog/draft?trip_id=nonexistent",
    "/api/blog/draft?trip_id=mismatch-file", "/api/blog/draft?trip_id=bad-json",
    "/api/blog/draft", "/api/blog/draft?trip_id=", "/api/blog/draft?trip_id=Bad", f"/api/blog/draft?trip_id={LONG_NG}",
    # ルーティング: /api/blog/ 配下の未知は 404（HTML）
    "/api/blog/", "/api/blog/nope", "/api/blog/trips/", "/api/blog/TRIPS", "/api/blog/%74rips",
    "/api/blog/trips;x", "/api/blog/trip/", "/api/blog/candidates/", "/api/blog/draftX",
    "/api/blog/save", "/api/blog/export", "/api/blog/trips/extra", "/api/blogx/trips",
]


def _get(port, target):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", target, headers={"Host": "127.0.0.1"})
        response = conn.getresponse()
        raw = response.read()
        ctype = response.getheader("Content-Type") or ""
        body = json.loads(raw) if ctype.startswith("application/json") else None
        return {"target": target, "status": response.status, "body": body,
                "cache_control": response.getheader("Cache-Control")}
    finally:
        conn.close()


def build_cases():
    """合成の旅程・DBに対する現行 Python の出力（JSON に直せる dict）。"""
    with seeded(), patch.object(handlers.Handler, "log_message", lambda *a, **k: None), \
            patch("builtins.print", lambda *a, **k: None):
        with _http() as port:
            cases = [_get(port, t) for t in TARGETS]
    return {"plans": PLANS, "drafts": DRAFTS, "exports": EXPORTS, "cases": cases}


def render(doc):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    body = ",\n".join("  " + dump(item) for item in doc["cases"])
    return ("{\n"
            f' "plans": {dump(doc["plans"])},\n'
            f' "drafts": {dump(doc["drafts"])},\n'
            f' "exports": {dump(doc["exports"])},\n'
            f' "cases": [\n{body}\n ]\n'
            "}\n")
