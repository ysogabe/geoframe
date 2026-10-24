"""Display P3 相当の最小ICCプロファイル（行列/TRC型 v2）を作る。

iPhone の写真が持つ Display P3 の代わり。システムに P3 のプロファイルが無いため、
ガモット（原色）とsRGBと同じ転送曲線から自前で組み立てる。色の厳密さは求めない
（ICCの保持・変換の挙動を見るためのもの）。
"""
import struct


def _s15(v):
    return struct.pack(">i", round(v * 65536))


def _xyz_tag(x, y, z):
    return b"XYZ \x00\x00\x00\x00" + _s15(x) + _s15(y) + _s15(z)


def _curv_srgb(n=1024):
    vals = []
    for i in range(n):
        c = i / (n - 1)
        lin = c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4
        vals.append(round(lin * 65535))
    return b"curv\x00\x00\x00\x00" + struct.pack(">I", n) + b"".join(struct.pack(">H", v) for v in vals)


def _text(s):
    return b"text\x00\x00\x00\x00" + s.encode("ascii") + b"\x00"


def _desc(s):
    a = s.encode("ascii") + b"\x00"
    return b"desc\x00\x00\x00\x00" + struct.pack(">I", len(a)) + a + b"\x00" * 4 + b"\x00" * 4 + b"\x00\x00" + b"\x00" + b"\x00" * 67


def display_p3_profile():
    # D50に順応した Display P3 の原色（ICC仕様のPCS）
    tags = {
        b"desc": _desc("Synthetic Display P3"),
        b"cprt": _text("synthetic, no copyright"),
        b"wtpt": _xyz_tag(0.9642, 1.0, 0.8249),
        b"rXYZ": _xyz_tag(0.5151, 0.2412, -0.0011),
        b"gXYZ": _xyz_tag(0.2920, 0.6922, 0.0419),
        b"bXYZ": _xyz_tag(0.1571, 0.0666, 0.7841),
    }
    trc = _curv_srgb()
    tags[b"rTRC"] = tags[b"gTRC"] = tags[b"bTRC"] = trc
    n = len(tags)
    off = 128 + 4 + 12 * n
    table = b""
    body = b""
    placed = {}
    for sig, data in tags.items():
        if data in placed:
            o = placed[data]
        else:
            o = off + len(body)
            placed[data] = o
            body += data + b"\x00" * ((4 - len(data) % 4) % 4)
        table += sig + struct.pack(">II", o, len(data))
    size = off + len(body)
    header = struct.pack(">I4sI4s4s4s", size, b"\x00" * 4, 0x02100000, b"mntr", b"RGB ", b"XYZ ")
    header += b"\x00" * 12  # 日付
    header += b"acsp" + b"\x00" * 4 + b"\x00" * 4 + b"\x00" * 4 + b"\x00" * 4  # 署名・プラットフォーム・フラグ・製造元・モデル
    header += b"\x00" * 8 + b"\x00" * 4  # 属性・レンダリングインテント
    header += _s15(0.9642) + _s15(1.0) + _s15(0.8249)  # 光源
    header += b"\x00" * 4 + b"\x00" * 44  # 作成者・ID・予約
    assert len(header) == 128, len(header)
    return header + struct.pack(">I", n) + table + body
