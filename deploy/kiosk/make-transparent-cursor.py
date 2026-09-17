#!/usr/bin/env python3
"""透明カーソルの Xcursor テーマを最小構成で生成する（rootで実行）。
1x1 の透明PNGを画像チャンクに持つ Xcursor ファイルを書き、一般的なカーソル名を張る。
"""
import os, shutil, struct, zlib, subprocess

THEME = "/usr/share/icons/transparent"
CURSORS = THEME + "/cursors"
NAMES = [
    "left_ptr", "default", "arrow", "top_left_arrow", "pointer", "hand", "hand1", "hand2",
    "text", "xterm", "ibeam", "crosshair", "cross", "grab", "grabbing", "fleur", "move",
    "wait", "watch", "progress", "left_ptr_watch", "help", "question_arrow", "context-menu",
    "col-resize", "row-resize", "size_ver", "size_hor", "size_fdiag", "size_bdiag",
    "up_arrow", "down_arrow", "left_arrow", "right_arrow", "zoom-in", "zoom-out",
]

def png_1x1_transparent():
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))
    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)  # 1x1, 8bit, RGBA
    raw = b"\x00" + b"\x00\x00\x00\x00"                    # filter0 + RGBA(0,0,0,0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

def xcursor_file(png, size=24):
    """Xcursor 形式: magic/header/ntoc + image チャンク"""
    out = b"Xcur" + struct.pack("<III", 16, 0x00010000, 1)
    out += struct.pack("<III", 0xFFFD0002, size, 1)                 # TOC: image, size, pos=1
    img = struct.pack("<IIIII", 1, 1, 0, 0, 0) + png                # w,h,xhot,yhot,delay + PNG
    out += struct.pack("<IIII", 0xFFFD0002, size, 1, len(img) + 16)  # chunk header (size は自身含む)
    out += img
    return out

def main():
    if os.geteuid() != 0:
        raise SystemExit("root で実行してください")
    os.makedirs(CURSORS, exist_ok=True)
    data = xcursor_file(png_1x1_transparent())
    with open(os.path.join(CURSORS, "left_ptr"), "wb") as f:
        f.write(data)
    for name in NAMES[1:]:
        path = os.path.join(CURSORS, name)
        if not os.path.exists(path):
            os.symlink("left_ptr", path)
    with open(os.path.join(THEME, "cursor.theme"), "w", encoding="utf-8") as f:
        f.write("[Icon Theme]\nName=Transparent\nComment=Invisible cursor for kiosk\n")
    with open(os.path.join(THEME, "index.theme"), "w", encoding="utf-8") as f:
        f.write("[Icon Theme]\nName=Transparent\nDirectories=cursors\n\n[cursors]\nSize=24\n")
    subprocess.run(["chmod", "-R", "a+rX", THEME], check=False)
    print("theme written:", THEME)
    print("files:", len(os.listdir(CURSORS)), "/ left_ptr bytes:", os.path.getsize(os.path.join(CURSORS, "left_ptr")))

if __name__ == "__main__":
    main()
