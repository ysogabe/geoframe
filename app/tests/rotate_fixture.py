"""回転 API（`POST /api/admin/rotate`）の契約テスト用フィクスチャ。

更新APIのフィクスチャ（update_api_fixture）と同じ合成DB（admin_api_fixture の `seeded()`）に、合成の
derivative（JPEG）を置き、現行の `handlers.Handler` に実際に POST して、下の `CASES` を**順に**流す。各ケースに
ついて、状態コード・応答のJSON・直後の `photos` の変化と、データ置き場のファイルの変化（増えた・消えた・
中身が変わったもの）を記録する。画像は寸法と、四隅の目印の色の名前（Pillow のバージョンで揺れる画素の
値そのものは記録しない）、EXIF の有無だけ。Rust の結合テスト（crates/geoframe/tests/rotate_api.rs）は、
同じ合成DB・同じ画像（`IMAGES` の名前で Rust 側が描く）を作り直して同じ要求を同じ順に流し、一致を確かめる。

再生成: `python3 tools/gen_rotate_fixture.py`（差分の検査は `--check`）。
座標・地名・ファイル名は実在のものを使わない。

意図した差は `KNOWN_DIFFERENCES`（小数の delta、シンボリックリンクの display、`derivatives/` の外の display、
`i64` を超える id）。Rust のテストはその一覧で期待値を分け、差が増減したら落ちる（server.md §0・§17）。
"""
import io
import json
import os
from pathlib import Path
from unittest.mock import patch

from PIL import Image

from geoframe import handlers, store

from tests.admin_api_fixture import _http, seeded
from tests.update_api_fixture import _diff, _send, _snapshot, build_request, case, j

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/rotate-python.json"

ROTATE = "/api/admin/rotate"

RED, GREEN, BLUE, YELLOW = (255, 0, 0), (0, 160, 0), (0, 0, 255), (255, 255, 0)
COLORS = (("red", RED), ("green", GREEN), ("blue", BLUE), ("yellow", YELLOW))
GRAYS = (40, 110, 180, 250)
MARK = 10   # 四隅の目印を見る位置（角から内側へ）

# 画像の名前 → (モード, 幅, 高さ, EXIF を付けるか)。左上・右上・左下・右下の順に 4 色（グレーは 4 段階）
IMAGES = {
    "land": ("RGB", 160, 96, False),
    "port": ("RGB", 96, 160, False),
    "gray": ("L", 128, 64, False),
    "land_exif": ("RGB", 160, 96, True),
}


def draw(name):
    """名前の画像を JPEG（品質 90）のバイト列にする。Rust のテストも同じ絵を描く。"""
    mode, w, h, with_exif = IMAGES[name]
    im = Image.new(mode, (w, h))
    fills = GRAYS if mode == "L" else [c for _, c in COLORS]
    for k, (x, y) in enumerate(((0, 0), (w // 2, 0), (0, h // 2), (w // 2, h // 2))):
        im.paste(fills[k], (x, y, x + w // 2, y + h // 2))
    out = io.BytesIO()
    kwargs = {}
    if with_exif:
        exif = Image.Exif()
        exif[0x0112] = 6               # 向き（回転は EXIF の向きを当てない）
        exif[0x010F] = "Synthetic"
        gps = exif.get_ifd(0x8825)     # GPS（出力に残ってはいけない）
        gps[1] = "N"
        gps[2] = (35.0, 0.0, 0.0)
        kwargs["exif"] = exif
    im.save(out, "JPEG", quality=90, **kwargs)
    return out.getvalue()


def label(px):
    """四隅の色の名前。グレー（3 成分が近い）は段階の名前。"""
    r, g, b = px[:3]
    if abs(r - g) < 12 and abs(g - b) < 12:
        return "g%d" % min(GRAYS, key=lambda level: abs(level - g))
    return min(COLORS, key=lambda c: sum((c[1][i] - px[i]) ** 2 for i in range(3)))[0]


def describe(path):
    """ファイルの記録: 画像なら寸法・四隅の色・EXIF の有無、それ以外は種類（と大きさ）。"""
    if path.is_symlink():
        return {"kind": "symlink"}
    if path.is_dir():
        return {"kind": "dir"}
    data = path.read_bytes()
    try:
        with Image.open(io.BytesIO(data)) as im:
            im.load()
            w, h = im.size
            rgb = im.convert("RGB")
            corners = [label(rgb.getpixel(p)) for p in (
                (MARK, MARK), (w - 1 - MARK, MARK), (MARK, h - 1 - MARK), (w - 1 - MARK, h - 1 - MARK))]
            return {"kind": "image", "size": [w, h], "corners": corners,
                    "exif": b"Exif\x00\x00" in data}
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
        elif kind == "garbage":
            path.write_bytes(b"not an image")
        elif kind == "dir":
            path.mkdir()
        elif kind == "symlink":
            path.symlink_to(op["to"])
        else:
            raise ValueError(kind)


# 合成DBの行（admin_api_fixture）。display は derivatives/pNN.jpg、drive_id は drive-N（local は 1・4）
INITIAL = [
    image("derivatives/p02.jpg", "land"),     # id 2: rotation は NULL
    image("derivatives/p03.jpg", "port"),     # id 3: rotation 0
    image("derivatives/p14.jpg", "gray"),     # id 14: rotation 180
]


def rot(name, payload, **kw):
    return case(name, ROTATE, j(payload), **kw)


def raw(name, body, **kw):
    return case(name, ROTATE, body, **kw)


def with_setup(c, *ops):
    c["setup"] = list(ops)
    return c


CASES = [
    # ---- 正常: 回転の累積と名前（NULL は 0 として足す。0 に戻れば元の名前に戻る） ----
    rot("cw90_from_null_rotation", {"id": 2, "delta": 90}),
    rot("cw90_again_accumulates", {"id": 2, "delta": 90}),
    rot("cw90_third", {"id": 2, "delta": 90}),
    rot("cw90_fourth_returns_to_the_original_name", {"id": 2, "delta": 90}),
    rot("ccw90_from_zero", {"id": 2, "delta": -90}),
    rot("half_turn", {"id": 2, "delta": 180}),
    rot("270_back_to_zero", {"id": 2, "delta": 270}),
    rot("half_turn_portrait", {"id": 3, "delta": 180}),
    rot("cw90_portrait", {"id": 3, "delta": 90}),
    rot("gray_ccw90", {"id": 14, "delta": -90}),
    rot("gray_cw90", {"id": 14, "delta": 90}),
    rot("extra_keys_ignored", {"id": 3, "delta": 270, "note": "x", "rotation": 5}),
    # EXIF の向きは当てない。出力に EXIF（GPS を含む）を残さない
    with_setup(rot("exif_orientation_is_ignored_and_exif_is_dropped", {"id": 6, "delta": 90}),
               image("derivatives/p06.jpg", "land_exif")),
    # rotation が 90 の倍数でない・範囲外の行
    with_setup(rot("stored_rotation_not_a_multiple_of_90", {"id": 7, "delta": 90}),
               sql("UPDATE photos SET rotation=45 WHERE id=7"), image("derivatives/p07.jpg", "land")),
    with_setup(rot("stored_negative_rotation_wraps_to_zero", {"id": 8, "delta": 90}),
               sql("UPDATE photos SET rotation=-90 WHERE id=8"), image("derivatives/p08.jpg", "land")),
    with_setup(rot("stored_large_rotation_wraps", {"id": 9, "delta": -90}),
               sql("UPDATE photos SET rotation=1000 WHERE id=9"), image("derivatives/p09.jpg", "land")),
    # display と rotation が食い違った行（新しい名前が今の display と同じ）: そのファイルに上書きして消さない
    with_setup(rot("new_name_equals_the_current_display", {"id": 10, "delta": 270}),
               sql("UPDATE photos SET display='derivatives/drive-10.jpg', rotation=90 WHERE id=10"),
               image("derivatives/drive-10.jpg", "land")),
    # ---- 対象の検査 ----
    rot("local_photo_is_refused", {"id": 4, "delta": 90}),
    rot("bad_delta_is_reported_before_the_source_check", {"id": 4, "delta": 45}),
    rot("no_display_yet", {"id": 5, "delta": 90}),
    rot("display_file_missing", {"id": 16, "delta": 90}),
    with_setup(rot("display_is_not_an_image", {"id": 19, "delta": 90}),
               {"op": "garbage", "path": "derivatives/p19.jpg"}),
    with_setup(rot("display_is_a_directory", {"id": 20, "delta": 90}),
               {"op": "dir", "path": "derivatives/p20.jpg"}),
    with_setup(rot("drive_id_is_null", {"id": 21, "delta": 90}),
               sql("UPDATE photos SET drive_id=NULL WHERE id=21"), image("derivatives/p21.jpg", "land")),
    with_setup(rot("drive_id_is_empty", {"id": 22, "delta": 90}),
               sql("UPDATE photos SET drive_id='' WHERE id=22"), image("derivatives/p22.jpg", "land")),
    rot("not_found", {"id": 999, "delta": 90}),
    rot("bad_delta_is_reported_before_the_lookup", {"id": 999, "delta": 45}),
    rot("not_found_largest_i64", {"id": 9223372036854775807, "delta": 90}),
    # ---- delta の検査（行 11 は最後まで回さない） ----
    with_setup(rot("delta_45", {"id": 11, "delta": 45}), image("derivatives/p11.jpg", "land")),
    rot("delta_zero", {"id": 11, "delta": 0}),
    rot("delta_360", {"id": 11, "delta": 360}),
    rot("delta_minus_180", {"id": 11, "delta": -180}),
    rot("delta_minus_270", {"id": 11, "delta": -270}),
    rot("delta_450", {"id": 11, "delta": 450}),
    rot("delta_string", {"id": 11, "delta": "90"}),
    rot("delta_true", {"id": 11, "delta": True}),
    rot("delta_false", {"id": 11, "delta": False}),
    rot("delta_null", {"id": 11, "delta": None}),
    rot("delta_missing", {"id": 11}),
    rot("delta_array", {"id": 11, "delta": [90]}),
    rot("delta_object", {"id": 11, "delta": {"a": 90}}),
    rot("delta_fraction", {"id": 11, "delta": 90.5}),
    rot("delta_huge", {"id": 11, "delta": 10 ** 30}),
    # ---- id の検査 ----
    rot("id_missing", {"delta": 90}),
    rot("id_zero", {"id": 0, "delta": 90}),
    rot("id_negative", {"id": -1, "delta": 90}),
    rot("id_string", {"id": "11", "delta": 90}),
    rot("id_true", {"id": True, "delta": 90}),
    rot("id_float", {"id": 11.0, "delta": 90}),
    rot("id_null", {"id": None, "delta": 90}),
    rot("id_bad_before_bad_delta", {"id": 0, "delta": 45}),
    raw("duplicate_key_last_wins", '{"id": 11, "delta": 45, "delta": 90}'),
    # ---- 意図した差 ----
    raw("id_beyond_i64", '{"id": 9223372036854775808, "delta": 90}'),
    with_setup(rot("symlinked_display", {"id": 23, "delta": 90}),
               image("derivatives/p23-real.jpg", "land"), {"op": "symlink", "path": "derivatives/p23.jpg",
                                                            "to": "p23-real.jpg"}),
    with_setup(rot("display_outside_derivatives", {"id": 24, "delta": 90}),
               sql("UPDATE photos SET display='photos/outside.jpg' WHERE id=24"),
               image("photos/outside.jpg", "land")),
    with_setup(rot("delta_float_90", {"id": 25, "delta": 90.0}), image("derivatives/p25.jpg", "land")),
    with_setup(raw("delta_float_exponent", '{"id": 26, "delta": 9e1}'), image("derivatives/p26.jpg", "land")),
]

KNOWN_DIFFERENCES = {
    "id_beyond_i64": "i64 を超える id: Python は SQLite に渡せず 500、Rust は存在しない id として 404",
    "symlinked_display": "display がシンボリックリンク: Python は辿って回し、リンクを消す。Rust はリンクを辿らず 400",
    "display_outside_derivatives": "display が derivatives/<名前> でない: Python は DATA_DIR 配下のそのファイルを回して消す。Rust は 400",
    "delta_float_90": "小数の delta（90.0）: Python は 90.0 == 90 で通し、drive_id.r90.0.jpg を作って rotation を 90.0 で返す。Rust は 400",
    "delta_float_exponent": "小数の delta（9e1）: 同上",
}


def build_cases():
    """合成DBとファイルに対する現行 Python の出力（JSON に直せる dict）。"""
    results = []
    with seeded(), patch.object(handlers.Handler, "log_message", lambda *a, **k: None), \
            patch("traceback.print_exc", lambda *a, **k: None), \
            patch("builtins.print", lambda *a, **k: None):
        root = store.DB_PATH.parent
        apply_setup(root, INITIAL)
        with _http() as port:
            for c in CASES:
                apply_setup(root, c.get("setup", []))
                headers, body = build_request(c)
                before_db, before_files = _snapshot(), listing(root)
                status, parsed = _send(port, c["target"], headers, body)
                results.append({
                    "name": c["name"], "target": c["target"],
                    "headers": [list(h) for h in headers],
                    "body_hex": body.hex(),
                    "setup": c.get("setup", []),
                    "status": status, "body": parsed,
                    "changes": _diff(before_db, _snapshot()),
                    "files": files_diff(before_files, listing(root)),
                })
    return {"initial": INITIAL, "images": {k: list(v) for k, v in IMAGES.items()},
            "cases": results, "known_differences": KNOWN_DIFFERENCES}


def render(doc):
    """フィクスチャのファイルに書く文字列（キー順固定・日本語はそのまま・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    body = ",\n".join("  " + dump(item) for item in doc["cases"])
    return ("{\n"
            f' "cases": [\n{body}\n ],\n'
            f' "images": {dump(doc["images"])},\n'
            f' "initial": {dump(doc["initial"])},\n'
            f' "known_differences": {dump(doc["known_differences"])}\n'
            "}\n")
