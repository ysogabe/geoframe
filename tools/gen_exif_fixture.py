#!/usr/bin/env python3
"""EXIF（撮影時刻・GPS）の読み取りの契約テスト用フィクスチャ（Pythonの`util.extract_exif`・`norm_time`が基準）。

- `exif-python.json`: 合成のJPEG（8x8。EXIFはこのスクリプトが組み立てたバイト列。実写真ではない）と、
  それをPythonの`extract_exif`に通した結果（`lat`・`lon`・`taken_at`、または例外の型名）。
  `norm_time`の入力と結果の表も入る。RustのテストがJPEGのバイト列を取り出して同じ関数を通し、
  Pythonと同じか、意図して違える点（`docs/rust-migration/server.md` §0・§12）だけが違うことを確かめる。

    python3 tools/gen_exif_fixture.py          # 書き出す（JPEGの本体はPillowが作るので、版が変わると変わる）
    python3 tools/gen_exif_fixture.py --check  # 保存済みのJPEGのバイト列をPythonに通し直し、期待値が一致するか検査する

`--check`はJPEGを作り直さない（Pillowの版でエンコード結果が変わってもCIが落ちないように、保存済みの
バイト列を入力にして、Pythonの読み取りの結果が保存済みの期待値と変わっていないことだけを見る）。
出力先は crates/geoframe/tests/fixtures/ の下。
"""

import base64
import io
import json
import os
import struct
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
APP = REPO / "app"
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "exif-python.json"

# TIFFのデータ型
BYTE, ASCII, SHORT, LONG, RATIONAL, UNDEFINED, SRATIONAL = 1, 2, 3, 4, 5, 7, 10


def ascii_(s: str) -> tuple:
    """終端のNULを付けたASCII（UTF-8の多バイトも1バイトずつ入る）"""
    return (ASCII, s.encode("utf-8") + b"\0")


def rationals(*pairs: tuple) -> tuple:
    return (RATIONAL, list(pairs))


def short(v: int) -> tuple:
    return (SHORT, [v])


def encode(t: tuple, end: str) -> tuple[int, bytes, int]:
    """(型, 値) をバイト列と要素数にする。値の数値は指定のバイト順で書く"""
    typ, value = t
    if typ == RATIONAL:
        return typ, b"".join(struct.pack(end + "II", n, d) for n, d in value), len(value)
    if typ == SHORT:
        return typ, b"".join(struct.pack(end + "H", v) for v in value), len(value)
    if typ == LONG:
        return typ, b"".join(struct.pack(end + "I", v) for v in value), len(value)
    return typ, value, len(value)


def build_tiff(ifd0: dict, exif: dict | None = None, gps: dict | None = None,
               big_endian: bool = True) -> bytes:
    """各IFDは {tag: (型, 値)}。ExifとGPSのIFDへのポインタは自動で足す"""
    end = ">" if big_endian else "<"
    ifds = [("ifd0", dict(ifd0))]
    if exif is not None:
        ifds[0][1][0x8769] = (LONG, [0])
        ifds.append(("exif", dict(exif)))
    if gps is not None:
        ifds[0][1][0x8825] = (LONG, [0])
        ifds.append(("gps", dict(gps)))
    offsets, cursor = {}, 8
    for name, entries in ifds:
        offsets[name] = cursor
        cursor += 2 + 12 * len(entries) + 4
    data_base, data = cursor, bytearray()
    out = bytearray(b"MM\0*" if big_endian else b"II*\0")
    out += struct.pack(end + "I", 8)
    for name, entries in ifds:
        if name == "ifd0":
            if exif is not None:
                entries[0x8769] = (LONG, [offsets["exif"]])
            if gps is not None:
                entries[0x8825] = (LONG, [offsets["gps"]])
        out += struct.pack(end + "H", len(entries))
        for tag in sorted(entries):
            typ, value, count = encode(entries[tag], end)
            if len(value) <= 4:
                inline = value.ljust(4, b"\0")
            else:
                inline = struct.pack(end + "I", data_base + len(data))
                data += value
                if len(data) % 2:
                    data += b"\0"
            out += struct.pack(end + "HHI", tag, typ, count) + inline
        out += struct.pack(end + "I", 0)
    return bytes(out + data)


def base_jpeg() -> bytes:
    from PIL import Image
    im = Image.new("RGB", (8, 8), (200, 80, 40))
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=80)
    return buf.getvalue()


def with_exif(jpeg: bytes, tiff: bytes) -> bytes:
    seg = b"Exif\0\0" + tiff
    return jpeg[:2] + b"\xff\xe1" + struct.pack(">H", len(seg) + 2) + seg + jpeg[2:]


def gps_ok(lat_ref="N", lon_ref="E", lat=((35, 1), (40, 1), (30, 1)), lon=((139, 1), (45, 1), (15, 1))):
    g = {2: rationals(*lat), 4: rationals(*lon)}
    if lat_ref is not None:
        g[1] = ascii_(lat_ref)
    if lon_ref is not None:
        g[3] = ascii_(lon_ref)
    return g


def dto(s):  # DateTimeOriginal
    return {36867: ascii_(s)}


# (名前, ifd0, exif, gps, big_endian)。JPEGにEXIFを付けて作る
CASES = [
    ("typical", {}, dto("2024:05:06 07:08:09"), gps_ok(), True),
    ("typical_little_endian", {}, dto("2024:05:06 07:08:09"), gps_ok(), False),
    ("south_west", {}, dto("2024:05:06 07:08:09"), gps_ok("S", "W"), True),
    ("lowercase_refs_are_positive", {}, dto("2024:05:06 07:08:09"), gps_ok("s", "w"), True),
    ("unknown_refs_are_positive", {}, dto("2024:05:06 07:08:09"), gps_ok("X", "Y"), True),
    ("missing_refs_default_n_e", {}, dto("2024:05:06 07:08:09"), gps_ok(None, None), True),
    ("only_latitude", {}, dto("2024:05:06 07:08:09"), {1: ascii_("N"), 2: rationals((35, 1), (0, 1), (0, 1))}, True),
    ("only_longitude", {}, dto("2024:05:06 07:08:09"), {3: ascii_("E"), 4: rationals((139, 1), (0, 1), (0, 1))}, True),
    ("zero_zero", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((0, 1), (0, 1), (0, 1)), lon=((0, 1), (0, 1), (0, 1))), True),
    ("fractional_seconds", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 1), (40, 1), (3050, 100)), lon=((139, 1), (45, 1), (1534, 100))), True),
    ("rounding_6_digits", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 1), (1, 1), (1, 7)), lon=((139, 1), (0, 1), (5, 1000))), True),
    ("out_of_range", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((95, 1), (0, 1), (0, 1)), lon=((200, 1), (0, 1), (0, 1))), True),
    ("denominator_zero", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 0), (40, 1), (30, 1))), True),
    ("denominator_zero_seconds", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 1), (40, 1), (0, 0))), True),
    ("two_components", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 1), (40, 1))), True),
    ("four_components", {}, dto("2024:05:06 07:08:09"),
     gps_ok(lat=((35, 1), (40, 1), (30, 1), (9, 1))), True),
    ("lat_is_short", {}, dto("2024:05:06 07:08:09"),
     {1: ascii_("N"), 2: short(35), 3: ascii_("E"), 4: rationals((139, 1), (0, 1), (0, 1))}, True),
    ("empty_gps_ifd", {}, dto("2024:05:06 07:08:09"), {}, True),
    # --- 撮影時刻: `36867 or 36868 or 306` の連鎖 ---
    ("time_digitized_when_original_empty", {}, {36867: ascii_(""), 36868: ascii_("2023:01:02 03:04:05")}, None, True),
    ("time_original_blank_does_not_fall_through", {306: ascii_("2022:01:02 03:04:05")},
     {36867: ascii_("    :  :     :  :  ")}, None, True),
    ("time_digitized_only", {}, {36868: ascii_("2023:01:02 03:04:05")}, None, True),
    ("time_datetime_ifd0_only", {306: ascii_("2022:01:02 03:04:05")}, None, None, True),
    ("time_original_beats_others", {306: ascii_("2022:01:02 03:04:05")},
     {36867: ascii_("2024:05:06 07:08:09"), 36868: ascii_("2023:01:02 03:04:05")}, None, True),
    ("time_in_wrong_ifd_is_ignored", {36867: ascii_("2024:05:06 07:08:09")}, None, None, True),
    ("time_dashes_and_t", {}, dto("2024-05-06T07:08:09"), None, True),
    ("time_mixed_separators", {}, dto("2024:05-06 07:08:09"), None, True),
    ("time_date_only", {}, dto("2024:05:06"), None, True),
    ("time_without_seconds", {}, dto("2024:05:06 07:08"), None, True),
    ("time_zero_date", {}, dto("0000:00:00 00:00:00"), None, True),
    ("time_not_calendar", {}, dto("2024:13:45 25:61:00"), None, True),
    ("time_trailing_text", {}, dto("2024:05:06 07:08:09 +09:00"), None, True),
    ("time_leading_space", {}, dto(" 2024:05:06 07:08:09"), None, True),
    ("time_fullwidth_digits_utf8", {}, dto("２０２４:０５:０６ ０７:０８:０９"), None, True),
    ("time_extra_nul", {}, {36867: (ASCII, b"2024:05:06 07:08:09\0\0\0")}, None, True),
    ("time_no_nul", {}, {36867: (ASCII, b"2024:05:06 07:08:09")}, None, True),
    ("time_embedded_nul", {}, {36867: (ASCII, b"2024:05:06 07:08:09\0junk\0")}, None, True),
    ("time_only_nuls_falls_through", {}, {36867: (ASCII, b"\0\0\0\0\0"), 36868: ascii_("2023:01:02 03:04:05")}, None, True),
    ("ref_embedded_nul", {}, None, {1: (ASCII, b"S\0X\0"), 2: rationals((35, 1), (0, 1), (0, 1)),
                                    3: (ASCII, b"W\0"), 4: rationals((139, 1), (0, 1), (0, 1))}, True),
    ("time_wrong_type_short", {}, {36867: short(5)}, None, True),
    ("empty_ifd0", {}, None, None, True),
]


def extract(path: Path) -> dict:
    """Python本体の`util.extract_exif`を呼ぶ（例外は型名だけを残す）。

    分母0の有理数は、Pillowの版で結果が違う（11.xは`ZeroDivisionError`、13は`IFDRational`がNaN）ので、
    どちらも`NonFinite`にそろえる（どちらもRustは位置なしにする）"""
    from geoframe.util import extract_exif
    try:
        lat, lon, taken = extract_exif(path)
    except ZeroDivisionError:
        return {"error": "NonFinite"}
    except Exception as e:  # noqa: BLE001 - 期待値の記録
        return {"error": type(e).__name__}
    if any(isinstance(v, float) and v != v for v in (lat, lon)):
        return {"error": "NonFinite"}
    return {"lat": lat, "lon": lon, "taken_at": taken}


NORM_TIME_INPUTS = [
    "2024:05:06 07:08:09", "2024-05-06 07:08:09", "2024-05-06T07:08:09.123Z", "2024:05:06 07:08",
    "2024:05:06", "", None, "２０２４:０５:０６ ０７:０８:０９", "２０２４-０５-０６Ｔ０７:０８:０９",
    "٢٠٢٤:٠٥:٠٦ ٠٧:٠٨:٠٩", "2024/05/06 07:08:09", " 2024:05:06 07:08:09", "0000:00:00 00:00:00",
    "2024:05:06  07:08:09", "20240506T070809", "2024-05-06 7:08:09",
]


def build() -> dict:
    jpeg = base_jpeg()
    cases = []
    for name, ifd0, exif, gps, big in CASES:
        data = with_exif(jpeg, build_tiff(ifd0, exif, gps, big))
        cases.append({"name": name, "jpeg_base64": base64.b64encode(data).decode()})
    cases.append({"name": "no_exif", "jpeg_base64": base64.b64encode(jpeg).decode()})
    # EXIFのセグメントが途中で切れている / 壊れている
    good = with_exif(jpeg, build_tiff({}, dto("2024:05:06 07:08:09"), gps_ok(), True))
    cases.append({"name": "exif_segment_garbage",
                  "jpeg_base64": base64.b64encode(
                      jpeg[:2] + b"\xff\xe1" + struct.pack(">H", 20) + b"Exif\0\0" + b"\x00" * 12 + jpeg[2:]).decode()})
    cases.append({"name": "not_an_image", "jpeg_base64": base64.b64encode(b"not an image at all").decode()})
    del good
    return fill({"cases": cases})


def fill(doc: dict) -> dict:
    """保存済みの入力から、Pythonの結果を埋める"""
    os.environ.setdefault("GEOFRAME_DATA_DIR", tempfile.mkdtemp(prefix="geoframe-exif-"))
    sys.path.insert(0, str(APP))
    from geoframe.util import norm_time
    with tempfile.TemporaryDirectory(prefix="geoframe-exif-case-") as tmp:
        for case in doc["cases"]:
            path = Path(tmp) / "case.jpg"
            path.write_bytes(base64.b64decode(case["jpeg_base64"]))
            case["python"] = extract(path)
    doc["norm_time"] = [{"input": s, "python": norm_time(s)} for s in NORM_TIME_INPUTS]
    return doc


def render(doc: dict) -> str:
    return json.dumps(doc, ensure_ascii=False, indent=1) + "\n"


def main() -> int:
    if "--check" in sys.argv[1:]:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_exif_fixture.py で生成してください", file=sys.stderr)
            return 1
        saved = json.loads(FIXTURE.read_text(encoding="utf-8"))
        again = fill(json.loads(FIXTURE.read_text(encoding="utf-8")))
        if render(saved) != render(again):
            print(f"{FIXTURE} の期待値がPythonの結果と食い違います（Pythonの読み取りが変わった）。"
                  " 内容を確かめて python3 tools/gen_exif_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("EXIFのフィクスチャ: OK")
        return 0
    FIXTURE.write_text(render(build()), encoding="utf-8")
    print(f"書き出しました: {FIXTURE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
