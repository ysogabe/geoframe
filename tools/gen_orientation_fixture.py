#!/usr/bin/env python3
"""向き（EXIF の Orientation と XMP の tiff:Orientation）の契約テスト用フィクスチャ。基準は Python の`make_derivative`。

- `orientation-python.json`: 合成の画像（140x90。四隅に色の付いた 20x20 の目印がある。実写真ではない）に
  EXIF や XMP で向きを付けたもの（JPEG・PNG・TIFF・HEIC）と、それをPythonの`util.make_derivative`
  （`ImageOps.exif_transpose` → 手動回転 → 縮小 → JPEG）に通した出力の**寸法**と**四隅の色**。
  RustのテストがPillowと同じ向きになるか、意図して違える点（`docs/rust-migration/server.md` §0・§12）
  だけが違うかを確かめる。

    python3 tools/gen_orientation_fixture.py          # 書き出す（画像の本体はPillowが作るので、版が変わると変わる）
    python3 tools/gen_orientation_fixture.py --check  # 保存済みの画像をPythonに通し直し、期待値が一致するか検査する

`--check`は画像を作り直さない（Pillowの版でエンコード結果が変わってもCIが落ちないように、保存済みの
バイト列を入力にして、Pythonの結果が保存済みの期待値と変わっていないことだけを見る）。
出力先は crates/geoframe/tests/fixtures/ の下。HEIC は pillow-heif（app/requirements.txt）が要る。

Pillow の読み方（`Image.getexif()`）: EXIF の Orientation の**キーが無いとき**だけ XMP の
`tiff:Orientation(="|>)([0-9])` の最初の一致を使う。キーがあれば値が 1 や 0 でも XMP は見ない。
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
FIXTURE = REPO / "crates" / "geoframe" / "tests" / "fixtures" / "orientation-python.json"

W, H = 140, 90
MARK = 20
# 四隅の目印（左上・右上・左下・右下）。背景は白
COLORS = {"red": (255, 0, 0), "green": (0, 160, 0), "blue": (0, 0, 255), "yellow": (255, 255, 0),
          "white": (255, 255, 255)}
CORNERS = [("red", (0, 0)), ("green", (W - MARK, 0)), ("blue", (0, H - MARK)), ("yellow", (W - MARK, H - MARK))]
NS = b"http://ns.adobe.com/xap/1.0/\0"
NS_EXT = b"http://ns.adobe.com/xmp/extension/\0"


def base_image():
    from PIL import Image
    im = Image.new("RGB", (W, H), COLORS["white"])
    for name, (x, y) in CORNERS:
        im.paste(COLORS[name], (x, y, x + MARK, y + MARK))
    return im


def xmp(attr=None, element=None, raw=None) -> bytes:
    """tiff:Orientation を属性形式（attr）や要素形式（element）で持つ XMP パケット。raw があればそのまま差し込む"""
    desc = ' xmlns:tiff="http://ns.adobe.com/tiff/1.0/"'
    if attr is not None:
        desc += f' tiff:Orientation="{attr}"'
    body = ""
    if element is not None:
        body += f"<tiff:Orientation>{element}</tiff:Orientation>"
    if raw is not None:
        body += raw
    return ('<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/">'
            '<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
            f"<rdf:Description{desc}>{body}</rdf:Description></rdf:RDF></x:xmpmeta>"
            '<?xpacket end="w"?>').encode()


def jpeg(app1: list | None = None, exif: dict | None = None) -> bytes:
    """app1: APP1 セグメントの本体の並び（XMP など）。exif: {タグ: 値} の EXIF（Pillow が書く）"""
    from PIL import Image
    ex = None
    if exif is not None:
        ex = Image.Exif()
        for tag, value in exif.items():
            ex[tag] = value
    buf = io.BytesIO()
    kwargs = {"exif": ex} if ex is not None else {}
    base_image().save(buf, "JPEG", quality=90, subsampling=0, **kwargs)
    data = buf.getvalue()
    segs = b"".join(b"\xff\xe1" + struct.pack(">H", len(s) + 2) + s for s in (app1 or []))
    return data[:2] + segs + data[2:]


def png(text=None, itxt=None, itxt_zip=None, ztxt=None, exif: dict | None = None) -> bytes:
    from PIL import Image, PngImagePlugin
    info = PngImagePlugin.PngInfo()
    key = "XML:com.adobe.xmp"
    if text is not None:
        info.add_text(key, text.decode())
    if itxt is not None:
        info.add_itxt(key, itxt.decode())
    if itxt_zip is not None:
        info.add_itxt(key, itxt_zip.decode(), zip=True)
    if ztxt is not None:
        info.add_text(key, ztxt.decode(), zip=True)
    ex = None
    if exif is not None:
        ex = Image.Exif()
        for tag, value in exif.items():
            ex[tag] = value
    buf = io.BytesIO()
    base_image().save(buf, "PNG", pnginfo=info, **({"exif": ex} if ex is not None else {}))
    return buf.getvalue()


def tiff(orientation=None, xmp_bytes=None) -> bytes:
    from PIL import TiffImagePlugin
    ifd = TiffImagePlugin.ImageFileDirectory_v2()
    if orientation is not None:
        ifd[274] = orientation
    if xmp_bytes is not None:
        ifd[700] = xmp_bytes
    buf = io.BytesIO()
    base_image().save(buf, "TIFF", tiffinfo=ifd, compression="tiff_lzw")
    return buf.getvalue()


def heic(xmp_orientation=None, exif_orientation=None, patch=None) -> bytes:
    """pillow-heif で作る。XMP の向きを付けて保存すると irot になる。XMP だけの向き（irot なし）は、
    向き 1 の XMP で保存したあと、同じ長さのままバイト列の `1` を `patch` に差し替えて作る"""
    import pillow_heif
    pillow_heif.register_heif_opener()
    kwargs = {}
    if xmp_orientation is not None:
        kwargs["xmp"] = xmp(attr=xmp_orientation)
    if exif_orientation is not None:
        from PIL import Image
        ex = Image.Exif()
        ex[0x0112] = exif_orientation
        kwargs["exif"] = ex
    buf = io.BytesIO()
    base_image().save(buf, "HEIF", quality=90, **kwargs)
    data = buf.getvalue()
    if patch is not None:
        needle = b'tiff:Orientation="1"'
        assert data.count(needle) == 1
        data = data.replace(needle, f'tiff:Orientation="{patch}"'.encode())
    return data


def cases() -> list:
    out = []

    def add(name, fmt, data, rotation=0):
        out.append({"name": name, "format": fmt, "rotation": rotation,
                    "data_base64": base64.b64encode(data).decode()})

    # --- JPEG: EXIF の向き ---
    add("jpeg_none", "jpeg", jpeg())
    for o in range(1, 9):
        add(f"jpeg_exif_{o}", "jpeg", jpeg(exif={0x0112: o}))
    # --- JPEG: XMP だけに向き（属性形式） ---
    for o in [1, 2, 3, 4, 5, 6, 7, 8, 0, 9]:
        add(f"jpeg_xmp_attr_{o}", "jpeg", jpeg([NS + xmp(attr=o)]))
    # --- JPEG: XMP だけに向き（要素形式） ---
    for o in [3, 6, 8]:
        add(f"jpeg_xmp_element_{o}", "jpeg", jpeg([NS + xmp(element=o)]))
    # --- XMP の読み方の境目 ---
    add("jpeg_xmp_two_digits_is_first_digit", "jpeg", jpeg([NS + xmp(attr=16)]))
    add("jpeg_xmp_two_digits_61", "jpeg", jpeg([NS + xmp(attr=61)]))
    # 属性形式と要素形式が両方あるとき: 先頭に近い方が勝つ（Description の属性は要素より前に書かれる）
    add("jpeg_xmp_attr6_then_element8_leftmost_wins", "jpeg", jpeg([NS + xmp(attr=6, element=8)]))
    add("jpeg_xmp_attr8_then_element6_leftmost_wins", "jpeg", jpeg([NS + xmp(attr=8, element=6)]))
    add("jpeg_xmp_element8_then_attr6_leftmost_wins", "jpeg", jpeg([
        NS + b'<x:xmpmeta><a><tiff:Orientation>8</tiff:Orientation><b tiff:Orientation="6"/></a></x:xmpmeta>']))
    add("jpeg_xmp_non_digit_then_valid", "jpeg",
        jpeg([NS + xmp(raw='<x a="tiff:Orientation=&quot;x" tiff:Orientation="x" />', element=6)]))
    add("jpeg_xmp_not_closed_attr_still_matches", "jpeg",
        jpeg([NS + b'<x tiff:Orientation="6 />']))
    add("jpeg_xmp_two_segments_6_then_3", "jpeg", jpeg([NS + xmp(attr=6), NS + xmp(attr=3)]))
    add("jpeg_xmp_two_segments_3_then_6", "jpeg", jpeg([NS + xmp(attr=3), NS + xmp(attr=6)]))
    add("jpeg_xmp_empty_packet", "jpeg", jpeg([NS]))
    add("jpeg_xmp_extension_is_ignored", "jpeg", jpeg([NS_EXT + xmp(attr=6)]))
    add("jpeg_xmp_without_orientation", "jpeg", jpeg([NS + xmp()]))
    # --- EXIF と XMP の優先順位 ---
    add("jpeg_exif3_xmp6_exif_wins", "jpeg", jpeg([NS + xmp(attr=6)], exif={0x0112: 3}))
    add("jpeg_exif1_xmp6_exif_key_present_so_no_xmp", "jpeg", jpeg([NS + xmp(attr=6)], exif={0x0112: 1}))
    add("jpeg_exif0_xmp6_out_of_range_key_present", "jpeg", jpeg([NS + xmp(attr=6)], exif={0x0112: 0}))
    add("jpeg_exif9_xmp6_out_of_range_key_present", "jpeg", jpeg([NS + xmp(attr=6)], exif={0x0112: 9}))
    add("jpeg_exif_without_orientation_xmp6", "jpeg", jpeg([NS + xmp(attr=6)], exif={0x0131: "synthetic"}))
    add("jpeg_xmp6_then_manual_rotation_90", "jpeg", jpeg([NS + xmp(attr=6)]), rotation=90)
    add("jpeg_exif6_then_manual_rotation_90", "jpeg", jpeg(exif={0x0112: 6}), rotation=90)
    # --- PNG ---
    add("png_none", "png", png())
    add("png_exif_6", "png", png(exif={0x0112: 6}))
    add("png_xmp_itxt_6", "png", png(itxt=xmp(attr=6)))
    add("png_xmp_itxt_element_8", "png", png(itxt=xmp(element=8)))
    add("png_xmp_text_6", "png", png(text=xmp(attr=6)))
    add("png_exif3_xmp6_exif_wins", "png", png(itxt=xmp(attr=6), exif={0x0112: 3}))
    add("png_exif1_xmp6_exif_key_present_so_no_xmp", "png", png(itxt=xmp(attr=6), exif={0x0112: 1}))
    # 圧縮した XMP: Pillow は展開して読む。Rust は展開しないので読まない（意図した差）
    add("png_xmp_itxt_compressed_6", "png", png(itxt_zip=xmp(attr=6)))
    add("png_xmp_ztxt_6", "png", png(ztxt=xmp(attr=6)))
    # --- TIFF ---
    add("tiff_none", "tiff", tiff())
    add("tiff_orientation_6", "tiff", tiff(orientation=6))
    add("tiff_xmp_6", "tiff", tiff(xmp_bytes=xmp(attr=6)))
    add("tiff_xmp_element_8", "tiff", tiff(xmp_bytes=xmp(element=8)))
    add("tiff_orientation3_xmp6_tag_wins", "tiff", tiff(orientation=3, xmp_bytes=xmp(attr=6)))
    add("tiff_orientation1_xmp6_key_present_so_no_xmp", "tiff", tiff(orientation=1, xmp_bytes=xmp(attr=6)))
    # --- HEIC ---
    add("heic_none", "heic", heic())
    # EXIF の向き 6 で保存しても Pillow（pillow-heif）の出力は回らない。Rust も HEIC の向きは EXIF から当てない
    add("heic_exif_6_without_irot", "heic", heic(exif_orientation=6))
    add("heic_irot_from_xmp_6", "heic", heic(xmp_orientation=6))
    # irot の無い HEIC に XMP だけの向き 6: pillow-heif が開くときに XMP の向きを取り除くので、Pillow は回さない
    add("heic_xmp_only_6_is_not_applied", "heic", heic(xmp_orientation=1, patch=6))
    return out


def corner_names(im) -> list:
    """出力の四隅の目印の色の名前（左上・右上・左下・右下）。画素に最も近い色。JPEG の揺れは見ない"""
    px = im.convert("RGB").load()
    w, h = im.size
    names = []
    for x, y in [(MARK // 2, MARK // 2), (w - 1 - MARK // 2, MARK // 2),
                 (MARK // 2, h - 1 - MARK // 2), (w - 1 - MARK // 2, h - 1 - MARK // 2)]:
        p = px[x, y]
        names.append(min(COLORS, key=lambda n: sum((a - b) ** 2 for a, b in zip(COLORS[n], p))))
    return names


SUFFIX = {"jpeg": ".jpg", "png": ".png", "tiff": ".tif", "heic": ".heic"}


def python_result(case: dict, tmp: Path) -> dict:
    """Python本体の`util.make_derivative`を通した出力の寸法と四隅の色"""
    from PIL import Image
    from geoframe.util import make_derivative
    src = tmp / ("case" + SUFFIX[case["format"]])
    src.write_bytes(base64.b64decode(case["data_base64"]))
    dest = tmp / "out.jpg"
    if dest.exists():
        dest.unlink()
    make_derivative(src, dest, case["rotation"])
    with Image.open(dest) as im:
        return {"size": list(im.size), "corners": corner_names(im)}


def fill(doc: dict) -> dict:
    sys.path.insert(0, str(APP))
    import pillow_heif
    pillow_heif.register_heif_opener()
    with (tempfile.TemporaryDirectory(prefix="geoframe-orient-") as data_dir,
          tempfile.TemporaryDirectory(prefix="geoframe-orient-case-") as tmp):
        # config.py は import 時に環境変数を読む。置き場を作らせないよう一時の場所を指す（終わったら消す）
        os.environ["GEOFRAME_DATA_DIR"] = data_dir
        for case in doc["cases"]:
            case["python"] = python_result(case, Path(tmp))
    return doc


def render(doc: dict) -> str:
    return json.dumps(doc, indent=1) + "\n"


def main() -> int:
    if "--check" in sys.argv[1:]:
        if not FIXTURE.exists():
            print(f"{FIXTURE} がありません。python3 tools/gen_orientation_fixture.py で生成してください",
                  file=sys.stderr)
            return 1
        saved = json.loads(FIXTURE.read_text(encoding="utf-8"))
        again = fill(json.loads(FIXTURE.read_text(encoding="utf-8")))
        if render(saved) != render(again):
            print(f"{FIXTURE} の期待値がPythonの結果と食い違います（Pillowの向きの扱いが変わった）。"
                  " 内容を確かめて python3 tools/gen_orientation_fixture.py で再生成してください", file=sys.stderr)
            return 1
        print("向きのフィクスチャ: OK")
        return 0
    FIXTURE.write_text(render(fill({"cases": cases()})), encoding="utf-8")
    print(f"書き出しました: {FIXTURE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
