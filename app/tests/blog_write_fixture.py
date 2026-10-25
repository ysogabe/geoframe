"""旅行ブログの保存と書き出し（`POST /api/blog/save`・`POST /api/blog/export`）の契約テスト用フィクスチャ。

合成の旅程JSON・合成の写真の行（update_api / rotate のフィクスチャと同じ合成DB）・合成の画像（derivative と、
`photos/` の原本）を作り、現行の `handlers.Handler` に実際に POST して、下の `CASES` を**順に**流す。各ケースに
ついて、状態コード・応答のJSON・データ置き場のファイルの変化（増えた・消えた・中身が変わったもの）を記録する。
テキスト（下書きの JSON、書き出しの manifest・Markdown・HTML）は**全文を記録**する（Rust は同じバイト列を作る）。
画像は寸法・成分数（1=グレー、3=カラー）・EXIF と ICC の有無・四隅の目印の色の名前（Pillow のバージョンで揺れる
画素の値そのものは記録しない）。Rust の結合テスト（crates/geoframe/tests/blog_write_api.rs）は、同じDB・同じ旅程・
同じ画像（`IMAGES` の仕様で Rust 側が描く）を作り直して同じ要求を同じ順に流し、一致を確かめる。

再生成: `python3 tools/gen_blog_write_fixture.py`（差分の検査は `--check`）。
座標・地名・ファイル名・旅程は実在のものを使わない（`sample-trip-a` のような合成。写真の行の座標は 35.1/139.1 のような
丸い値で、書き出しには出ない）。

意図した差は `KNOWN_DIFFERENCES`。Rust のテストはその一覧で期待値を分け、差が増減したら落ちる（server.md §0・§21）。
同時に 2 本走らせる 409 は HTTP では作れない（順に流す）ので、Rust の単体テストが別に確かめる。
"""
import io
import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

from PIL import Image

from geoframe import handlers, store, travel_blog

from tests.admin_api_fixture import _http, seeded
from tests.blog_fixture import plan, segment
from tests.update_api_fixture import _send, build_request, case

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/blog-write-python.json"

SAVE = "/api/blog/save"
EXPORT = "/api/blog/export"

RED, GREEN, BLUE, YELLOW = (255, 0, 0), (0, 160, 0), (0, 0, 255), (255, 255, 0)
COLORS = (("red", RED), ("green", GREEN), ("blue", BLUE), ("yellow", YELLOW))
GRAYS = (40, 110, 180, 250)
MARK = 10   # 四隅の目印を見る位置（角から内側へ）
FAKE_ICC = b"synthetic-icc-profile-" + bytes(range(64))

# 画像の名前 → 仕様。左上・右上・左下・右下の順に 4 色（グレーは 4 段階）。`orient` は EXIF の向き
IMAGES = {
    "wide": {"mode": "RGB", "size": [2400, 1200], "fmt": "JPEG"},
    "tall_exif6": {"mode": "RGB", "size": [1200, 2400], "fmt": "JPEG", "orient": 6, "gps": True},
    "mid_exif6": {"mode": "RGB", "size": [1000, 1600], "fmt": "JPEG", "orient": 6},
    "small": {"mode": "RGB", "size": [160, 96], "fmt": "JPEG"},
    "gray": {"mode": "L", "size": [400, 300], "fmt": "JPEG"},
    "icc": {"mode": "RGB", "size": [300, 200], "fmt": "JPEG", "icc": True, "orient": 3, "gps": True},
    "png": {"mode": "RGB", "size": [320, 200], "fmt": "PNG"},
    "png_rgba": {"mode": "RGBA", "size": [100, 100], "fmt": "PNG"},
    "exact": {"mode": "RGB", "size": [1800, 1200], "fmt": "JPEG"},
    "just_over": {"mode": "RGB", "size": [1801, 1000], "fmt": "JPEG"},
    "square": {"mode": "RGB", "size": [2000, 2000], "fmt": "JPEG"},
}


def draw(name):
    """名前の画像を JPEG（品質 90）か PNG のバイト列にする。Rust のテストも同じ絵を描く。"""
    spec = IMAGES[name]
    mode, (w, h) = spec["mode"], spec["size"]
    im = Image.new(mode, (w, h))
    if mode == "L":
        fills = GRAYS
    elif mode == "RGBA":
        fills = [c + (128,) for _, c in COLORS]
    else:
        fills = [c for _, c in COLORS]
    for k, (x, y) in enumerate(((0, 0), (w // 2, 0), (0, h // 2), (w // 2, h // 2))):
        im.paste(fills[k], (x, y, x + w // 2, y + h // 2))
    out = io.BytesIO()
    kwargs = {}
    if spec["fmt"] == "JPEG":
        kwargs["quality"] = 90
        if "orient" in spec:
            exif = Image.Exif()
            exif[0x0112] = spec["orient"]
            exif[0x010F] = "Synthetic"
            if spec.get("gps"):
                gps = exif.get_ifd(0x8825)     # GPS（出力に残ってはいけない）
                gps[1] = "N"
                gps[2] = (35.0, 0.0, 0.0)
            kwargs["exif"] = exif
        if spec.get("icc"):
            kwargs["icc_profile"] = FAKE_ICC
    im.save(out, spec["fmt"], **kwargs)
    return out.getvalue()


def label(px):
    """四隅の色の名前。グレー（3 成分が近い）は段階の名前。"""
    r, g, b = px[:3]
    if abs(r - g) < 12 and abs(g - b) < 12:
        return "g%d" % min(GRAYS, key=lambda level: abs(level - g))
    return min(COLORS, key=lambda c: sum((c[1][i] - px[i]) ** 2 for i in range(3)))[0]


def jpeg_components(data):
    """JPEG の成分数（SOF マーカー）。JPEG でなければ 0。"""
    if data[:2] != b"\xff\xd8":
        return 0
    i = 2
    while i + 4 <= len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if marker in (0xC0, 0xC1, 0xC2):
            return data[i + 9]
        if marker == 0xD8 or marker == 0x01 or 0xD0 <= marker <= 0xD7 or marker == 0xFF:
            i += 2 if marker != 0xFF else 1
            continue
        i += 2 + int.from_bytes(data[i + 2:i + 4], "big")
    return 0


TEXT_SUFFIXES = (".json", ".md", ".html", ".txt")


def describe(path):
    """ファイルの記録: テキストは全文、画像なら寸法・成分数・EXIF・ICC・四隅の色、それ以外は種類と大きさ。"""
    if path.is_symlink():
        return {"kind": "symlink"}
    if path.is_dir():
        return {"kind": "dir"}
    data = path.read_bytes()
    if path.name.endswith(TEXT_SUFFIXES):
        return {"kind": "text", "text": data.decode("utf-8")}
    try:
        with Image.open(io.BytesIO(data)) as im:
            im.load()
            w, h = im.size
            rgb = im.convert("RGB")
            corners = [label(rgb.getpixel(p)) for p in (
                (MARK, MARK), (w - 1 - MARK, MARK), (MARK, h - 1 - MARK), (w - 1 - MARK, h - 1 - MARK))]
            return {"kind": "image", "size": [w, h], "components": jpeg_components(data),
                    "corners": corners, "exif": b"Exif\x00\x00" in data,
                    "icc": b"ICC_PROFILE" in data}
    except OSError:
        return {"kind": "other", "bytes": len(data)}


def listing(root):
    """データ置き場のファイル（DB のファイルを除く）→ 記録。相対パスは `/` 区切り。"""
    out = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for name in sorted(dirnames + filenames):
            full = Path(dirpath) / name
            rel = full.relative_to(root).as_posix()
            if rel.startswith("catalog.db"):
                continue
            out[rel] = describe(full)
    return out


def files_diff(before, after):
    return {"added": {k: v for k, v in after.items() if k not in before},
            "removed": sorted(k for k in before if k not in after),
            "changed": {k: v for k, v in after.items() if k in before and before[k] != v}}


# ---- 前提の置き方（Rust のテストも同じ操作を再現する） ----
def image(path, name):
    return {"op": "image", "path": path, "image": name}


def text(path, content):
    return {"op": "text", "path": path, "text": content}


def sql(statement):
    return {"op": "sql", "sql": statement}


def apply_setup(root, ops):
    for op in ops:
        kind = op["op"]
        if kind == "sql":
            store.dbx(op["sql"])
            continue
        path = root / op["path"]
        path.parent.mkdir(parents=True, exist_ok=True)
        if kind == "image":
            path.write_bytes(draw(op["image"]))
        elif kind == "text":
            path.write_text(op["text"], encoding="utf-8")
        elif kind == "garbage":
            path.write_bytes(b"not an image")
        elif kind == "dir":
            path.mkdir()
        elif kind == "symlink":
            path.symlink_to(op["to"])
        else:
            raise ValueError(kind)


# ---- 旅程（PLAN_DIR のファイル名 → 中身の文字列） ----
def jd(obj):
    return json.dumps(obj, ensure_ascii=False, indent=2)


PLANS = {
    "sample-trip-a.json": jd(plan("sample-trip-a", [segment("d1", "2025-06-06"), segment("d2", "2025-06-07")])),
    "sample-trip-b.json": jd(plan("sample-trip-b", [segment("d1", "2025-06-08")])),
    "sample-trip-1.json": jd(plan("sample-trip-1", [segment("d1", "2025-06-09")])),
    # ファイルの名前と中の trip_id が違う（400）
    "mismatch-trip.json": jd(plan("sample-trip-b", [segment("d1", "2025-06-08")])),
    # segments が配列でない（400）
    "broken-shape.json": jd(plan("broken-shape", "abc")),
}

# 合成DBの行（admin_api_fixture）。display は derivatives/pNN.jpg。id 3・15 は hidden、id 5 は display が NULL
INITIAL = [
    image("derivatives/p01.jpg", "wide"),
    image("derivatives/p02.jpg", "tall_exif6"),
    image("derivatives/p04.jpg", "small"),
    image("derivatives/p06.jpg", "gray"),
    image("derivatives/p07.jpg", "icc"),
    image("derivatives/p08.jpg", "mid_exif6"),
    image("derivatives/p03.jpg", "small"),                       # hidden の写真にも画像はある
    image("photos/o09.png", "png"),
    sql("UPDATE photos SET display='photos/o09.png' WHERE id=9"),
    image("photos/o10.png", "png_rgba"),
    sql("UPDATE photos SET display='photos/o10.png' WHERE id=10"),
    image("derivatives/p11.jpg", "exact"),
    image("derivatives/p12.jpg", "just_over"),
    image("derivatives/p13.jpg", "square"),
]


def manifest(trip_id="sample-trip-a", **over):
    doc = {"schema_version": "1.0", "trip_id": trip_id, "title": "見本の旅", "subtitle": "サブタイトル",
           "status": "draft", "summary": "概要の文章です。", "chapters": []}
    doc.update(over)
    return doc


def img(pid, **over):
    doc = {"photo_id": str(pid), "src": f"/derivatives/p{pid:02d}.jpg" if isinstance(pid, int) else "",
           "alt": f"写真{pid}", "caption": f"キャプション{pid}"}
    doc.update(over)
    return doc


def chapter(id="d1", images=(), **over):
    doc = {"id": id, "date": "2025-06-06", "title": f"{id} の章", "location_label": "見本市",
           "body": "本文です。\n二行目です。", "images": list(images)}
    doc.update(over)
    return doc


def post(name, route, payload=None, *, raw=None, setup=(), **kw):
    body = raw if raw is not None else json.dumps(payload, ensure_ascii=False)
    c = case(name, route, body, **kw)
    c["setup"] = list(setup)
    return c


def save(name, trip_id, manifest_value, **kw):
    return post(name, SAVE, {"trip_id": trip_id, "manifest": manifest_value}, **kw)


def export(name, trip_id, manifest_value, **kw):
    extra = kw.pop("extra", {})
    return post(name, EXPORT, {"trip_id": trip_id, "manifest": manifest_value, **extra}, **kw)


def many(count, first=1):
    return [img(first + (i % 20)) for i in range(count)]


def ja(n):
    """3 バイト文字の n 文字（コードポイントで切ること）。POST の本文は 64 KiB までなので、長い文字列は欄ごとに分ける。"""
    return "あ" * n


TRICKY = 'クォート" 円記号\\ スラッシュ/ 改行\n タブ\t 制御\x01 DEL\x7f 絵文字😀 行区切り  NEL\x85   </script> \'single\' &amp;'

SAVE_CASES = [
    save("save_minimal", "sample-trip-a", manifest()),
    save("save_two_chapters_with_extra_keys_and_floats", "sample-trip-a", manifest(
        z_last="最後", a_first=1, unknown={"b": [1, 2.5, None, True], "a": {}, "empty": []},
        chapters=[
            chapter("d1", [img(1, score=0.7, selection_reason=["旅程日の撮影時刻と一致", "お気に入り"], extra="x"),
                           img(2, score=1.0)], note="章の知らないキー"),
            chapter("d2", []),
        ])),
    post("save_float_and_integer_forms", SAVE, raw=(
        '{"trip_id": "sample-trip-a", "manifest": {"trip_id": "sample-trip-a", "status": "draft", '
        '"chapters": [{"id": "d1", "images": [{"photo_id": "1", "a": 1e-05, "b": 1.0, "c": 100.0, "d": 1e22, '
        '"e": 0.1, "f": -0.0, "g": 12345678901234567890, "h": 9007199254740993, "i": 1E5, '
        '"j": 0.30000000000000004, "k": 1.5e-7, "l": 123456789.123, "m": -5, "n": 1e16, "o": 1e15, '
        '"p": 5e-324, "q": 1.7976931348623157e308}]}]}}')),
    post("save_duplicate_keys_keep_the_first_position", SAVE, raw=(
        '{"trip_id": "sample-trip-a", "manifest": {"trip_id": "sample-trip-a", "status": "draft", '
        '"title": "最初", "chapters": [], "title": "あと", "zz": 1}}')),
    save("save_published_becomes_draft", "sample-trip-a", manifest(status="published", chapters=[chapter()])),
    save("save_reviewed_stays_reviewed", "sample-trip-a", manifest(status="reviewed")),
    save("save_overwrites_an_existing_draft", "sample-trip-a", manifest(title="新しい題"),
         setup=[text("travel_blog/drafts/sample-trip-a.json", '{"old": true}\n')]),
    save("save_second_trip_keeps_the_first_draft", "sample-trip-b", manifest("sample-trip-b")),
    save("save_numeric_trip_id_in_the_name", "sample-trip-1", manifest("sample-trip-1")),
    save("save_string_escapes_and_unicode", "sample-trip-a", manifest(
        title=TRICKY, subtitle="日本語　全角空白", summary="a\r\nb",
        chapters=[chapter("d1", [img(1, alt=TRICKY, caption=TRICKY)], body=TRICKY,
                          location_label="結合文字 é")])),
    save("save_truncates_texts_over_the_limits", "sample-trip-a", manifest(
        title=ja(201), subtitle=ja(301), summary=ja(4001),
        chapters=[chapter(ja(101), [img(1, alt=ja(501), caption=ja(2001))], date=ja(11), title=ja(201),
                          location_label=ja(301), body="x")])),
    save("save_keeps_texts_at_the_limits", "sample-trip-a", manifest(
        marker="at-the-limits", title=ja(200), subtitle=ja(300), summary=ja(4000),
        chapters=[chapter(ja(100), [img(1, alt=ja(500), caption=ja(2000))], date=ja(10), title=ja(200),
                          location_label=ja(300), body="x")])),
    save("save_truncates_a_body_over_the_limit", "sample-trip-a",
         manifest(chapters=[chapter("d1", [], body="😀" + ja(20000))])),
    save("save_keeps_a_body_at_the_limit", "sample-trip-a", manifest(chapters=[chapter("d1", [], body=ja(20000))])),
    save("save_non_string_texts", "sample-trip-a", manifest(
        title=123, subtitle=None, summary=False,
        chapters=[chapter(7, [img(1, alt=0, caption=True)], date=20250606, title=1.5,
                          location_label=None, body=[])])),
    save("save_photo_id_forms", "sample-trip-a", manifest(chapters=[chapter("d1", [
        {"photo_id": 12}, {"photo_id": "012"}, {"photo_id": "٣"}, {"photo_id": "²"}, {"photo_id": "７"},
        {"photo_id": "1", "alt": "", "caption": None}])])),
    save("save_exactly_forty_chapters", "sample-trip-a", manifest(
        chapters=[chapter(f"c{i}") for i in range(40)])),
    save("save_two_hundred_images_in_two_chapters", "sample-trip-a", manifest(
        chapters=[chapter("d1", many(100)), chapter("d2", many(100))])),
    # ---- 400 / 404 ----
    save("save_over_two_hundred_images", "sample-trip-a", manifest(
        chapters=[chapter("d1", many(100)), chapter("d2", many(100)), chapter("d3", many(1))])),
    save("save_over_a_hundred_images_in_a_chapter", "sample-trip-a", manifest(chapters=[chapter("d1", many(101))])),
    save("save_forty_one_chapters", "sample-trip-a", manifest(chapters=[chapter(f"c{i}") for i in range(41)])),
    save("save_manifest_is_a_list", "sample-trip-a", [1, 2]),
    save("save_manifest_is_null", "sample-trip-a", None),
    post("save_manifest_is_missing", SAVE, {"trip_id": "sample-trip-a"}),
    post("save_trip_id_is_missing", SAVE, {"manifest": manifest()}),
    save("save_trip_id_is_invalid", "Sample_Trip", manifest()),
    save("save_trip_id_is_a_number", 5, manifest()),
    save("save_unknown_trip", "no-such-trip", manifest("no-such-trip")),
    save("save_plan_file_with_another_trip_id", "mismatch-trip", manifest("mismatch-trip")),
    save("save_plan_with_broken_segments", "broken-shape", manifest("broken-shape")),
    save("save_manifest_trip_id_differs", "sample-trip-a", manifest("sample-trip-b")),
    save("save_manifest_trip_id_is_invalid", "sample-trip-a", manifest("BAD")),
    save("save_status_is_invalid", "sample-trip-a", manifest(status="done")),
    save("save_status_is_missing", "sample-trip-a", {k: v for k, v in manifest().items() if k != "status"}),
    save("save_chapters_missing", "sample-trip-a", {k: v for k, v in manifest().items() if k != "chapters"}),
    save("save_chapters_not_a_list", "sample-trip-a", manifest(chapters={"a": 1})),
    save("save_chapter_not_an_object", "sample-trip-a", manifest(chapters=["x"])),
    save("save_images_not_a_list", "sample-trip-a", manifest(chapters=[{"id": "d1", "images": "x"}])),
    save("save_images_missing", "sample-trip-a", manifest(chapters=[{"id": "d1"}])),
    save("save_image_not_an_object", "sample-trip-a", manifest(chapters=[chapter("d1", [5])])),
    *[save(f"save_photo_id_{tag}", "sample-trip-a", manifest(chapters=[chapter("d1", [{"photo_id": value}])]))
      for tag, value in (("letters", "abc"), ("empty", ""), ("null", None), ("zero_int", 0), ("negative", "-1"),
                         ("float", 1.5), ("true", True), ("trailing_space", "1 "))],
    save("save_photo_id_key_missing", "sample-trip-a", manifest(chapters=[chapter("d1", [{"alt": "x"}])])),
    post("save_body_is_not_json", SAVE, raw="{not json"),
    post("save_body_is_a_list", SAVE, raw="[]"),
    # 失敗した保存は、既存の下書きを壊さない
    save("save_failure_keeps_the_existing_draft", "sample-trip-a", manifest(status="bad")),
]

# 書き出しのための共通の章
def export_chapters():
    return [
        chapter("d1", [img(1), img(2, caption="")], body="一日目。\n\n  \n本文2行目\nx"),
        chapter("d2", [img(4), img(6), img(7), img(8)], location_label="", date="2025-06-07"),
        chapter("d3", [img(9), img(10), img(11), img(12), img(13)], body=""),
    ]


EXPORT_CASES = [
    # ---- dry_run ----
    export("export_dry_run_true", "sample-trip-a", manifest(chapters=export_chapters()), extra={"dry_run": True}),
    *[export(f"export_dry_run_truthy_{tag}", "sample-trip-a", manifest(chapters=[chapter("d1", [img(1)])]),
             extra={"dry_run": value})
      for tag, value in (("one", 1), ("string", "x"), ("list", [0]), ("object", {"a": 0}), ("false_string", "false"))],
    export("export_dry_run_counts_duplicates", "sample-trip-a",
           manifest(chapters=[chapter("d1", [img(1), img(1), {"photo_id": "001"}])]), extra={"dry_run": True}),
    export("export_dry_run_does_not_check_photos", "sample-trip-a",
           manifest(chapters=[chapter("d1", [{"photo_id": "99999"}, {"photo_id": "3"}])]), extra={"dry_run": True}),
    export("export_dry_run_invalid_manifest", "sample-trip-a", manifest(status="x"), extra={"dry_run": True}),
    export("export_dry_run_unknown_trip", "no-such-trip", manifest("no-such-trip"), extra={"dry_run": True}),
    # ---- 書き出し ----
    export("export_all_kinds_of_images", "sample-trip-a", manifest(chapters=export_chapters())),
    export("export_replaces_the_previous_export", "sample-trip-a",
           manifest(title="二度目 <b>&\"'", status="published",
                    chapters=[chapter("d1", [img(1)], body="差し替え後")]),
           setup=[text("blog_exports/sample-trip-a/old.txt", "old"),
                  image("blog_exports/sample-trip-a/images/photo-99.jpg", "small")]),
    export("export_other_trip_keeps_the_first", "sample-trip-b",
           manifest("sample-trip-b", chapters=[chapter("d1", [img(2)])])),
    export("export_same_photo_many_times_is_one_file", "sample-trip-a", manifest(chapters=[
        chapter("d1", [img(1), img(1), {"photo_id": "001"}, {"photo_id": 1}, {"photo_id": "１"}])])),
    export("export_with_no_images_and_no_chapters", "sample-trip-a", manifest(subtitle="", summary="")),
    export("export_empty_subtitle_and_summary_with_a_chapter", "sample-trip-a",
           manifest(subtitle="", summary="", chapters=[chapter("d1", [])])),
    export("export_html_and_markdown_escapes", "sample-trip-a", manifest(
        title="<h1>題</h1> & \"引用\" 'x'", subtitle="<i>サブ</i>", summary="一行目\n\x1c\n \t \n二行目\u0085\n　\nlast",
        chapters=[chapter("d1", [img(1, alt='a"b<c>&\'', caption="<script>alert(1)</script>"), img(4, caption=TRICKY)],
                          title="<章>", date="2025-06-06", location_label="場所 & <所>",
                          body="段落1\n  \n\n段落2 <b>\n\x1c\n　末尾　\n\n"),
                  chapter("d2", [], body="\n\n　\n\x1c")])),
    export("export_markdown_ends_with_whitespace_stripped", "sample-trip-a", manifest(
        chapters=[chapter("d1", [], body="末尾の空白\n \n\t　\n")])),
    export("export_unknown_keys_and_order_in_the_manifest", "sample-trip-a", manifest(
        zz=1, aa={"k": [1.5, 1e22]}, chapters=[chapter("d1", [img(1, score=0.7, src="/x", extra=1)], z=0, a=1)])),
    export("export_follows_a_symlink_inside_the_data_dir", "sample-trip-a",
           manifest(chapters=[chapter("d1", [img(14)])]),
           setup=[{"op": "symlink", "path": "derivatives/inlink.jpg", "to": "p01.jpg"},
                  sql("UPDATE photos SET display='derivatives/inlink.jpg' WHERE id=14")]),
    export("export_dotdot_inside_the_data_dir", "sample-trip-a", manifest(chapters=[chapter("d1", [img(15 + 1)])]),
           setup=[sql("UPDATE photos SET display='derivatives/../derivatives/p01.jpg' WHERE id=16")]),
    export("export_the_same_photo_by_another_id", "sample-trip-b", manifest("sample-trip-b", chapters=[
        chapter("d1", [img(16), img(2)])])),
    # ---- 失敗（前の書き出しは残る。作業領域も残らない） ----
    export("export_hidden_photo", "sample-trip-a", manifest(chapters=[chapter("d1", [img(1), img(3)])])),
    export("export_photo_without_display", "sample-trip-a", manifest(chapters=[chapter("d1", [img(1), img(5)])])),
    export("export_photo_that_does_not_exist", "sample-trip-a",
           manifest(chapters=[chapter("d1", [img(1), {"photo_id": "99999"}])])),
    export("export_hidden_flag_is_checked_for_every_photo", "sample-trip-a",
           manifest(chapters=[chapter("d1", [img(2)]), chapter("d2", [img(15)])])),
    export("export_display_is_an_empty_string", "sample-trip-a", manifest(chapters=[chapter("d1", [img(17)])]),
           setup=[sql("UPDATE photos SET display='' WHERE id=17")]),
    export("export_display_file_is_missing", "sample-trip-a", manifest(chapters=[chapter("d1", [img(18)])]),
           setup=[sql("UPDATE photos SET display='derivatives/missing.jpg' WHERE id=18")]),
    export("export_display_is_absolute", "sample-trip-a", manifest(chapters=[chapter("d1", [img(19)])]),
           setup=[sql("UPDATE photos SET display='/etc/hostname' WHERE id=19")]),
    export("export_display_goes_up_out_of_the_data_dir", "sample-trip-a", manifest(chapters=[chapter("d1", [img(20)])]),
           setup=[sql("UPDATE photos SET display='../outside.jpg' WHERE id=20")]),
    export("export_display_is_a_symlink_to_outside", "sample-trip-a", manifest(chapters=[chapter("d1", [img(21)])]),
           setup=[{"op": "symlink", "path": "derivatives/outlink.jpg", "to": "/etc/hostname"},
                  sql("UPDATE photos SET display='derivatives/outlink.jpg' WHERE id=21")]),
    export("export_display_is_a_directory", "sample-trip-a", manifest(chapters=[chapter("d1", [img(22)])]),
           setup=[{"op": "dir", "path": "derivatives/adir.jpg"},
                  sql("UPDATE photos SET display='derivatives/adir.jpg' WHERE id=22")]),
    export("export_display_is_not_an_image", "sample-trip-a", manifest(chapters=[chapter("d1", [img(1), img(23)])]),
           setup=[{"op": "garbage", "path": "derivatives/p23.jpg"}]),
    export("export_photo_id_beyond_i64", "sample-trip-a",
           manifest(chapters=[chapter("d1", [{"photo_id": "99999999999999999999"}])])),
    export("export_photo_id_superscript", "sample-trip-a", manifest(chapters=[chapter("d1", [{"photo_id": "²"}])])),
    export("export_over_two_hundred_images", "sample-trip-a", manifest(chapters=[
        chapter("d1", many(100)), chapter("d2", many(100)), chapter("d3", many(1))])),
    export("export_unknown_trip", "no-such-trip", manifest("no-such-trip")),
    export("export_manifest_trip_id_differs", "sample-trip-a", manifest("sample-trip-b")),
    post("export_trip_id_missing", EXPORT, {"manifest": manifest()}),
    post("export_manifest_missing", EXPORT, {"trip_id": "sample-trip-a"}),
    # 再び成功すると、失敗のあとも前の書き出しが使える
    export("export_after_failures_still_works", "sample-trip-a",
           manifest(chapters=[chapter("d1", [img(4)])])),
]

CASES = SAVE_CASES + EXPORT_CASES

# 意図した差（名前 → 理由）。Rust のテストが期待値を別に持つ
KNOWN_DIFFERENCES = {
    "export_photo_id_beyond_i64": "i64 を超える写真の id は、Python は SQLite に渡せず 500。Rust は実在しない写真として 400",
    "export_photo_id_superscript": "上付き数字の写真の id は、Python は int() が読めず 500。Rust は実在しない写真として 400",
}


def build_cases():
    """合成DBとファイルに対する現行 Python の出力（JSON に直せる dict）。"""
    results = []
    with tempfile.TemporaryDirectory() as plans_tmp, seeded(), \
            patch.object(handlers.Handler, "log_message", lambda *a, **k: None), \
            patch("traceback.print_exc", lambda *a, **k: None), \
            patch("builtins.print", lambda *a, **k: None):
        plan_dir = Path(plans_tmp)
        for name, content in PLANS.items():
            (plan_dir / name).write_text(content, encoding="utf-8")
        root = store.DB_PATH.parent
        with patch.object(travel_blog, "DATA_DIR", root), \
                patch.object(travel_blog, "PLAN_DIR", plan_dir), \
                patch.object(travel_blog, "DRAFT_DIR", root / "travel_blog" / "drafts"), \
                patch.object(travel_blog, "EXPORT_DIR", root / "blog_exports"):
            apply_setup(root, INITIAL)
            with _http() as port:
                for c in CASES:
                    apply_setup(root, c["setup"])
                    headers, body = build_request(c)
                    before = listing(root)
                    status, parsed = _send(port, c["target"], headers, body)
                    results.append({
                        "name": c["name"], "target": c["target"],
                        "headers": [list(h) for h in headers],
                        "body_hex": body.hex(),
                        "setup": c["setup"],
                        "status": status, "body": parsed,
                        "files": files_diff(before, listing(root)),
                    })
    return {"initial": INITIAL, "plans": PLANS, "images": IMAGES, "cases": results,
            "known_differences": KNOWN_DIFFERENCES}


def render(doc):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    body = ",\n".join("  " + dump(item) for item in doc["cases"])
    return ("{\n"
            f' "cases": [\n{body}\n ],\n'
            f' "images": {dump(doc["images"])},\n'
            f' "initial": {dump(doc["initial"])},\n'
            f' "known_differences": {dump(doc["known_differences"])},\n'
            f' "plans": {dump(doc["plans"])}\n'
            "}\n")
