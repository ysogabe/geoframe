"""地図タイルのプロキシ（`GET/HEAD /tiles/{osm|gsi}/{z}/{x}/{y}.png`）とキャッシュの掃除の契約テスト用フィクスチャ。

現行の `handlers.Handler` に実際に GET / HEAD を流し、上流（OSM・地理院）はループバックのスタブ HTTP サーバーに
差し替える（`tiles.TILE_SOURCES` の URL だけを書き換え、`urllib` は本物。リダイレクト・切断・本文の途中切れなども
本物の挙動で記録できる）。時計（`tiles.time`）は偽の時計で、`advance`（その手順の前に進める秒数）で進む。タイルの置き場
（`tiles.TILE_DIR`）は一時ディレクトリ。各手順について、状態コード・ヘッダ・本文（長さと SHA-256）・上流が受けた要求
（パスと、`Host` 以外のヘッダ）・直後のキャッシュのファイル一覧（相対パスと SHA-256）を記録する。

Rust の単体テスト（crates/geoframe/src/tiles/tests.rs）は、同じ手順を同じ順に流し、同じ応答を返すループバックのスタブ・
同じ偽の時計で答えて、記録と一致することを確かめる。掃除（`tile_cache_prune`）も、偽のファイル群（大きさと mtime
を指定）に流した結果の残りを記録する。

再生成: `python3 tools/gen_tile_fixture.py`（差分の検査は `--check`）。
座標はすべて合成の値。上流のホスト名・実在の連絡先は載せない。

このフィクスチャに載せたケースは、Rust も Python と同じ結果にする。Python と意図して違える点（本文の上限、gzip、
壊れたキャッシュのファイル、リンク、座標の桁あふれなど）は載せず、Rust の単体テストが別に確かめる
（docs/rust-migration/server.md §0・§19）。
"""
import collections
import contextlib
import hashlib
import io
import json
import os
import socketserver
import struct
import tempfile
import threading
import zlib
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from geoframe import handlers, tiles

from tests.admin_api_fixture import _http

ROOT = Path(__file__).resolve().parents[2]
FIXTURE_PATH = ROOT / "crates/geoframe/tests/fixtures/tile-python.json"

START = 1000.0          # 偽の時計の起点
TEST_UA = "GeoFrame-Test/1.0 (synthetic contact)"
MAGIC = b"\x89PNG\r\n\x1a\n"


# ---- 合成の応答 -------------------------------------------------------------------------------

def make_png(r, g, b, size=8):
    """小さな単色の PNG（圧縮なしの deflate で作るので、zlib の版によらずバイト列が決まる）。"""
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    raw = b"".join(b"\x00" + bytes([r, g, b]) * size for _ in range(size))
    return (MAGIC + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 0)) + chunk(b"IEND", b""))


PNG_A = make_png(20, 80, 140)
PNG_B = make_png(200, 100, 50)


def png(data=PNG_A, **headers):
    return {"status": 200, "headers": {"Content-Type": "image/png", **headers}, "body_hex": data.hex()}


def status(code, **headers):
    return {"status": code, "headers": headers, "body_hex": ""}


def raw(data, code=200, **headers):
    return {"status": code, "headers": headers, "body_hex": data.hex()}


def pad(n):
    """PNG のマジック + n バイトの 0（`n` が大きくても、フィクスチャには長さだけを載せる）。"""
    return {"status": 200, "headers": {"Content-Type": "image/png"}, "body_pad": n}


HANGUP = {"hangup": True}


def chain(n):
    """異なる場所へ n 回続くリダイレクト（同じ場所への繰り返しは urllib が無限ループとして断る）。"""
    return [status(302, Location=f"/hop/{i}") for i in range(n)]


def step(target, replies=None, method="GET", advance=None, seed=None):
    out = {"method": method, "target": target}
    if replies is not None:
        out["replies"] = replies
    if advance is not None:
        out["advance"] = advance
    if seed is not None:
        out["seed"] = seed       # 手順の前に置くキャッシュのファイル [{path, body_hex}]
    return out


def get(target, *replies, **kw):
    return step(target, list(replies), **kw)


def head(target, *replies, **kw):
    return step(target, list(replies), method="HEAD", **kw)


# ---- ケース -----------------------------------------------------------------------------------
# 1ケース = 新しい偽の時計・空のキャッシュ・空の窓・`per_min`（既定 240）で、`steps` を順に流す。

CASES = [
    {"name": "miss_then_hit", "steps": [
        get("/tiles/osm/3/1/2.png", png()),
        get("/tiles/osm/3/1/2.png"),
        get("/tiles/gsi/10/5/6.png", png(PNG_B)),
        get("/tiles/gsi/10/5/6.png"),
        get("/tiles/osm/3/1/2.png"),
        # 層が違えば別のタイル
        get("/tiles/gsi/3/1/2.png", png(PNG_B)),
    ]},
    {"name": "upstream_request_has_only_the_expected_headers", "steps": [
        get("/tiles/osm/0/0/0.png", png()),
        get("/tiles/gsi/18/200/100.png", png()),
        get("/tiles/osm/19/524287/524287.png", png()),
    ]},
    {"name": "zoom_and_range_boundaries", "steps": [
        get("/tiles/osm/19/0/0.png", png()),
        get("/tiles/osm/20/0/0.png"),
        get("/tiles/gsi/18/262143/0.png", png()),
        get("/tiles/gsi/19/0/0.png"),
        get("/tiles/gsi/18/262144/0.png"),
        get("/tiles/gsi/18/0/262144.png"),
        get("/tiles/osm/3/7/7.png", png()),
        get("/tiles/osm/3/8/0.png"),
        get("/tiles/osm/3/0/8.png"),
        get("/tiles/osm/0/1/0.png"),
        get("/tiles/osm/0/0/1.png"),
        get("/tiles/osm/1/2/0.png"),
        get("/tiles/osm/99/0/0.png"),
        get("/tiles/osm/64/0/0.png"),
        get("/tiles/osm/63/0/0.png"),
        get("/tiles/gsi/00/0/0.png", png()),
        get("/tiles/osm/100/0/0.png"),
        get("/tiles/osm/3/999999999999999999999999999/0.png"),
        get("/tiles/osm/3/0/18446744073709551616.png"),
    ]},
    {"name": "leading_zeros_are_folded_into_the_same_tile", "steps": [
        get("/tiles/osm/05/003/004.png", png()),
        get("/tiles/osm/5/3/4.png"),
        get("/tiles/osm/005/3/4.png"),
        get("/tiles/osm/5/0003/00004.png"),
        get("/tiles/osm/00/0000/00.png", png(PNG_B)),
        get("/tiles/osm/0/0/0.png"),
    ]},
    {"name": "paths_that_do_not_match_are_404", "steps": [get(t) for t in [
        "/tiles/", "/tiles/osm", "/tiles/osm/", "/tiles/osm/1", "/tiles/osm/1/0", "/tiles/osm/1/0/0",
        "/tiles/osm/1/0/0.png/", "/tiles/osm/1/0/0.png/x", "/tiles/osm/1/0/0.PNG", "/tiles/osm/1/0/0.jpg",
        "/tiles/osm/1/0/.png", "/tiles/osm/1/0/0.png.png", "/tiles/osm/1/0/0png", "/tiles/osm/1/0/x.png",
        "/tiles/OSM/1/0/0.png", "/tiles/Osm/1/0/0.png", "/tiles/osmx/1/0/0.png", "/tiles/gsi-photo/1/0/0.png",
        "/tiles/gsi-dem/1/0/0.png", "/tiles//1/0/0.png", "/tiles/osm//1/0/0.png", "/tiles/osm/1//0/0.png",
        "/tiles/osm/1/0//0.png", "/tiles/osm/-1/0/0.png", "/tiles/osm/1/-1/0.png", "/tiles/osm/1/0/-1.png",
        "/tiles/osm/+1/0/0.png", "/tiles/osm/1/+0/0.png", "/tiles/osm/1.0/0/0.png", "/tiles/osm/1/0.0/0.png",
        "/tiles/osm/%201/0/0.png", "/tiles/osm/1/0/0.png%20", "/tiles/osm/%31/0/0.png", "/tiles/osm/1/%30/0.png",
        "/tiles/osm/1/0/0%2epng", "/tiles/osm/1/0/0.png%00", "/tiles/osm/1/0/0.png%0a", "/tiles/%6fsm/1/0/0.png",
        "/tiles/osm/../osm/1/0/0.png", "/tiles/osm/./1/0/0.png", "/tiles/osm/1/0/../0/0.png", "/tiles/./osm/1/0/0.png",
        "/tiles/osm/1/0/0.png;x", "/tiles/osm/1/0/0.png,", "/tiles/osm\\1/0/0.png",
        "/tiles/osm/%2e%2e/1/0/0.png",
    ]]},
    {"name": "query_string_is_dropped_before_matching", "steps": [
        get("/tiles/osm/2/1/1.png?v=1", png()),
        get("/tiles/osm/2/1/1.png?"),
        get("/tiles/osm/2/1/1.png?x=%2e%2e&y=1#a"),
        get("/tiles/osm/2/1/2.png?/../x", png()),
        get("/tiles/osm/2/1/3.png/?a=1"),
    ]},
    {"name": "leading_double_slash_is_folded_like_http_server", "steps": [
        get("//tiles/osm/2/1/1.png", png()),
        get("///tiles/osm/2/1/1.png"),
        get("//tiles/osm/2/1/9.png"),
    ]},
    {"name": "other_prefixes_never_reach_the_tile_proxy", "steps": [
        get("/tiles"), get("/Tiles/osm/1/0/0.png"), get("/%74iles/osm/1/0/0.png"),
        get("/derivatives/../tiles/osm/1/0/0.png"),
        get("/tiles%2fosm/1/0/0.png"), get("/x/tiles/osm/1/0/0.png"),
    ]},
    {"name": "head_does_the_same_work_without_a_body", "steps": [
        head("/tiles/osm/4/3/3.png", png()),
        head("/tiles/osm/4/3/3.png"),
        get("/tiles/osm/4/3/3.png"),
        head("/tiles/osm/4/3/4.png", status(503)),
        head("/tiles/osm/20/0/0.png"),
        head("/tiles/osm/bad"),
        head("/tiles/gsi/4/3/5.png", status(404)),
        head("/tiles/gsi/4/3/5.png"),
    ]},
    {"name": "upstream_404_is_cached_as_a_transparent_tile", "steps": [
        get("/tiles/gsi/17/1/1.png", status(404)),
        get("/tiles/gsi/17/1/1.png"),
        get("/tiles/osm/17/1/2.png", raw(b"<html>nothing</html>", 404, **{"Content-Type": "text/html"})),
        get("/tiles/osm/17/1/2.png"),
    ]},
    {"name": "other_upstream_statuses_are_errors_and_not_cached", "steps": [
        get("/tiles/osm/6/1/1.png", status(500)),
        get("/tiles/osm/6/1/1.png", status(502)),
        get("/tiles/osm/6/1/1.png", status(503, **{"Retry-After": "5"})),
        get("/tiles/osm/6/1/1.png", status(403)),
        get("/tiles/osm/6/1/1.png", status(429)),
        get("/tiles/osm/6/1/1.png", status(400)),
        get("/tiles/osm/6/1/1.png", status(410)),
        get("/tiles/osm/6/1/1.png", status(304)),
        get("/tiles/osm/6/1/1.png", status(300)),
        get("/tiles/osm/6/1/1.png", status(301)),
        get("/tiles/osm/6/1/1.png", status(302)),
        get("/tiles/osm/6/1/1.png", png()),
        get("/tiles/osm/6/1/1.png"),
    ]},
    {"name": "bodies_that_are_not_png_are_errors", "steps": [
        get("/tiles/osm/7/1/1.png", raw(b"<html>captive portal</html>", **{"Content-Type": "text/html"})),
        get("/tiles/osm/7/1/1.png", raw(b"<html>looks fine</html>", **{"Content-Type": "image/png"})),
        get("/tiles/osm/7/1/1.png", raw(b"\xff\xd8\xff\xe0JFIF\x00", **{"Content-Type": "image/jpeg"})),
        get("/tiles/osm/7/1/1.png", raw(b"", **{"Content-Type": "image/png"})),
        get("/tiles/osm/7/1/1.png", status(204)),
        get("/tiles/osm/7/1/1.png", raw(MAGIC[:7])),
        get("/tiles/osm/7/1/1.png", raw(b" " + MAGIC)),
        get("/tiles/osm/7/1/1.png", raw(b"\x89PNG\r\n\x1a\r")),
        # マジックだけ・マジック + ゴミは通る（現行は先頭 8 バイトしか見ない）
        get("/tiles/osm/7/1/2.png", raw(MAGIC)),
        get("/tiles/osm/7/1/2.png"),
        get("/tiles/osm/7/1/3.png", raw(MAGIC + b"not really a png", **{"Content-Type": "text/plain"})),
        get("/tiles/osm/7/1/3.png"),
        # Content-Type は見ない
        get("/tiles/osm/7/1/4.png", raw(PNG_A, **{"Content-Type": "text/html"})),
        get("/tiles/osm/7/1/5.png", raw(PNG_A)),
    ]},
    {"name": "connection_failures_are_errors", "steps": [
        get("/tiles/osm/8/1/1.png", HANGUP),
        # Content-Length より短い本文で閉じる
        get("/tiles/osm/8/1/1.png", {"status": 200, "headers": {"Content-Length": str(len(PNG_A) + 100)},
                                    "body_hex": PNG_A.hex()}),
        get("/tiles/osm/8/1/1.png", png()),
    ]},
    {"name": "redirects_are_followed_like_urllib", "steps": [
        get("/tiles/osm/9/1/1.png", status(302, Location="/moved/a.png"), png(PNG_B)),
        get("/tiles/osm/9/1/1.png"),
        get("/tiles/osm/9/1/2.png", status(301, Location="/moved/b.png"), png()),
        get("/tiles/osm/9/1/3.png", status(307, Location="/moved/c.png"), png()),
        get("/tiles/osm/9/1/4.png", status(303, Location="/moved/d.png"), png()),
        get("/tiles/osm/9/1/5.png", status(308, Location="/moved/e.png"), png()),
        get("/tiles/osm/9/1/6.png", status(302, Location="/moved/f.png"), status(404)),
        get("/tiles/osm/9/1/6.png"),
        get("/tiles/osm/9/1/7.png", status(302, Location="/moved/g.png"), status(500)),
        # 連なるリダイレクトをたどる（上限は urllib と ureq で数え方が違うので、境界は載せない）
        get("/tiles/osm/9/1/8.png", *chain(4), png()),
        get("/tiles/osm/9/1/9.png", *chain(30), png()),
    ]},
    {"name": "large_bodies_are_stored_whole", "steps": [
        get("/tiles/osm/10/1/1.png", pad(300000)),
        get("/tiles/osm/10/1/1.png"),
        get("/tiles/osm/10/1/2.png", pad(0)),
    ]},
    {"name": "the_window_slides_over_the_last_60_seconds", "per_min": 3, "steps": [
        get("/tiles/osm/11/1/1.png", png()),                      # t=1000
        get("/tiles/gsi/11/1/2.png", png(), advance=10),          # 1010（層をまたいで 1 つの窓）
        get("/tiles/osm/11/1/3.png", png(), advance=10),          # 1020
        get("/tiles/osm/11/1/4.png", advance=10),                 # 1030: 3 枚に到達 → throttled
        get("/tiles/osm/11/1/1.png"),                             # ヒットは数えず、抑制中でも返る
        get("/tiles/gsi/11/1/5.png", advance=29.9),               # 1059.9: throttled（先頭は 59.9 秒前）
        get("/tiles/gsi/11/1/5.png", png(), advance=0.1),         # 1060: 先頭が 60 秒前で窓から出る → miss
        get("/tiles/gsi/11/1/6.png", advance=0.0),                # 1060: 1010・1020・1060 で満杯 → throttled
        get("/tiles/gsi/11/1/6.png", png(), advance=10),          # 1070: 1010 が出る → miss
        get("/tiles/gsi/11/1/7.png", png(), advance=1000),        # 窓が空になれば取得できる
    ]},
    {"name": "failures_and_missing_tiles_consume_the_window_but_hits_and_bad_paths_do_not", "per_min": 2, "steps": [
        get("/tiles/osm/12/1/1.png", status(500)),                # 1 枚目（失敗も数える）
        get("/tiles/osm/12/1/2.png", status(404)),                # 2 枚目（404 の保存も数える）
        get("/tiles/osm/12/1/3.png", png()),                      # throttled
        get("/tiles/osm/12/1/2.png"),                             # ヒット
        get("/tiles/osm/12/1/2.png"),                             # ヒット
        get("/tiles/osm/20/1/1.png"),                             # 404（数えない）
        head("/tiles/osm/12/1/3.png"),                            # throttled（HEAD も窓の判定を通る）
        head("/tiles/osm/12/1/3.png", png(), advance=60),         # 60 秒後: HEAD が取得して保存
        get("/tiles/osm/12/1/3.png"),
    ]},
    {"name": "zero_per_minute_never_reaches_the_upstream", "per_min": 0, "steps": [
        get("/tiles/osm/13/1/1.png"),
        get("/tiles/gsi/13/1/1.png", advance=100000),
        get("/tiles/gsi/13/1/1.png", seed=[{"path": "gsi/13/1/1.png", "body_hex": PNG_A.hex()}]),
    ]},
    {"name": "seeded_cache_files_are_hits_without_a_request", "steps": [
        get("/tiles/osm/14/1/1.png", seed=[{"path": "osm/14/1/1.png", "body_hex": PNG_A.hex()}]),
        get("/tiles/gsi/14/2/2.png", seed=[{"path": "gsi/14/2/2.png", "body_hex": PNG_B.hex()}]),
        head("/tiles/gsi/14/2/2.png"),
        # 先頭ゼロの別名は同じファイル
        get("/tiles/gsi/14/002/0002.png"),
    ]},
]

# 掃除（`tile_cache_prune`）。`max_mb` MB の上限で、`files` を置いて流し、残るファイルを記録する。
# 大きさ（バイト）と mtime（秒）を指定する。mtime が同じなら大きさ、さらに同じならパスの文字列順で消す。
MB = 1024 * 1024
PRUNE_CASES = [
    {"name": "under_the_limit_keeps_everything", "max_mb": 1, "files": [
        {"path": "osm/3/1/1.png", "size": 400000, "mtime": 100},
        {"path": "osm/3/1/2.png", "size": 400000, "mtime": 200},
    ]},
    {"name": "exactly_at_the_limit_keeps_everything", "max_mb": 1, "files": [
        {"path": "osm/3/1/1.png", "size": MB // 2, "mtime": 100},
        {"path": "gsi/3/1/2.png", "size": MB // 2, "mtime": 200},
    ]},
    {"name": "one_byte_over_removes_the_oldest_until_90_percent", "max_mb": 10, "files": [
        # 合計 10485761 > 10485760。最も古い 1048577 を消すと 9437184 で、上限の 90%（9437184）ちょうど → 止まる
        {"path": "osm/5/2/2.png", "size": 1048577, "mtime": 100},
        {"path": "osm/5/2/3.png", "size": 9437184, "mtime": 200},
    ]},
    {"name": "removes_in_mtime_order_not_name_order", "max_mb": 1, "files": [
        {"path": "osm/4/1/1.png", "size": 300000, "mtime": 500},
        {"path": "osm/4/1/2.png", "size": 300000, "mtime": 100},
        {"path": "gsi/4/1/1.png", "size": 300000, "mtime": 300},
        {"path": "gsi/4/1/2.png", "size": 300000, "mtime": 200},
        {"path": "osm/4/2/1.png", "size": 300000, "mtime": 400},
        {"path": "osm/4/2/2.png", "size": 300000, "mtime": 600},
    ]},
    {"name": "ties_go_by_size_then_path_string", "max_mb": 1, "files": [
        {"path": "osm/9/1/1.png", "size": 200000, "mtime": 100},
        {"path": "osm/10/1/1.png", "size": 200000, "mtime": 100},
        {"path": "osm/9/1/2.png", "size": 100000, "mtime": 100},
        {"path": "gsi/9/1/1.png", "size": 200000, "mtime": 100},
        {"path": "osm/9/1/10.png", "size": 200000, "mtime": 100},
        {"path": "osm/9/1/9.png", "size": 200000, "mtime": 100},
        {"path": "osm/9/1/3.png", "size": 200000, "mtime": 100},
    ]},
    {"name": "zero_limit_removes_everything", "max_mb": 0, "files": [
        {"path": "osm/3/1/1.png", "size": 10, "mtime": 100},
        {"path": "gsi/3/1/2.png", "size": 10, "mtime": 200},
    ]},
    {"name": "an_empty_cache_is_fine", "max_mb": 1, "files": []},
]


# ---- 実行 -------------------------------------------------------------------------------------

class _Upstream:
    """ループバックの上流のスタブ。手順ごとに `replies` を順に返す（リダイレクトで複数回来る）。"""

    def __init__(self):
        self.replies = collections.deque()
        self.seen = []
        self.unscripted = []
        self.lock = threading.Lock()
        outer = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                data = b""
                while b"\r\n\r\n" not in data:
                    chunk = self.request.recv(4096)
                    if not chunk:
                        return
                    data += chunk
                lines = data.split(b"\r\n\r\n")[0].decode("latin-1").split("\r\n")
                path = lines[0].split(" ")[1]
                headers = {}
                for line in lines[1:]:
                    name, _, value = line.partition(":")
                    headers[name.strip().lower()] = value.strip()
                headers.pop("host", None)
                with outer.lock:
                    outer.seen.append({"path": path, "headers": headers})
                    reply = outer.replies.popleft() if outer.replies else None
                    if reply is None:
                        outer.unscripted.append(path)
                if reply is None:
                    reply = status(500)
                if reply.get("hangup"):
                    return
                body = _body_of(reply)
                reply_headers = dict(reply.get("headers", {}))
                if not any(k.lower() == "content-length" for k in reply_headers):
                    reply_headers["Content-Length"] = str(len(body))
                head = f"HTTP/1.1 {reply['status']} X\r\n" + "".join(
                    f"{k}: {v}\r\n" for k, v in reply_headers.items()) + "Connection: close\r\n\r\n"
                self.request.sendall(head.encode("latin-1") + body)

        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address = True
            daemon_threads = True

        self.server = Server(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def _body_of(reply):
    if "body_pad" in reply:
        return MAGIC + b"\x00" * reply["body_pad"]
    return bytes.fromhex(reply.get("body_hex", ""))


def _request(port, method, target):
    """生のソケットで送る（`http.client` は URL の検証で `\x00` などを断るため）。"""
    import socket
    sock = socket.create_connection(("127.0.0.1", port), timeout=20)
    try:
        sock.sendall(f"{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                     .encode("latin-1"))
        data = b""
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            data += chunk
    finally:
        sock.close()
    head, _, body = data.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    code = int(lines[0].split(" ")[1])
    headers = {}
    for line in lines[1:]:
        name, _, value = line.partition(":")
        if name.lower() not in ("date", "server"):
            headers[name.lower()] = value.strip()
    return code, headers, body


def _sha(data):
    return hashlib.sha256(data).hexdigest()


def _listing(root):
    out = {}
    for path in sorted(Path(root).rglob("*")):
        if path.is_file():
            out[path.relative_to(root).as_posix()] = _sha(path.read_bytes())
    return out


def run_case(case, port, upstream):
    clock = {"t": START}
    out = {"name": case["name"], "steps": []}
    if "per_min" in case:
        out["per_min"] = case["per_min"]
    sources = {
        "osm": (f"http://127.0.0.1:{upstream.port}/osm/{{z}}/{{x}}/{{y}}.png", 19, 2),
        "gsi": (f"http://127.0.0.1:{upstream.port}/gsi/{{z}}/{{x}}/{{y}}.png", 18, 4),
    }
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        with patch.object(tiles, "TILE_DIR", root), \
                patch.object(tiles, "TILE_SOURCES", sources), \
                patch.object(tiles, "TILE_UA", TEST_UA), \
                patch.object(tiles, "TILE_FETCH_PER_MIN", case.get("per_min", 240)), \
                patch.object(tiles, "_tile_fetch_win", collections.deque()), \
                patch.object(tiles, "_tile_throttled", False), \
                patch.object(tiles, "time", SimpleNamespace(time=lambda: clock["t"])):
            for s in case["steps"]:
                for seed in s.get("seed", []):
                    path = root / seed["path"]
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(bytes.fromhex(seed["body_hex"]))
                clock["t"] += s.get("advance", 0.0)
                with upstream.lock:
                    upstream.seen.clear()
                    upstream.replies.clear()
                    upstream.replies.extend(s.get("replies", []))
                code, headers, body = _request(port, s["method"], s["target"])
                record = dict(s)
                record["status"] = code
                record["headers"] = headers
                record["body_len"] = len(body)
                record["body_sha256"] = _sha(body)
                if headers.get("content-type", "").startswith("text/html"):
                    record["body_text"] = body.decode("utf-8")
                with upstream.lock:
                    record["requests"] = [dict(r) for r in upstream.seen]
                    leftover = len(upstream.replies)
                record["unused_replies"] = leftover
                record["cache"] = _listing(root)
                out["steps"].append(record)
    return out


def run_prune_case(case):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for f in case["files"]:
            path = root / f["path"]
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"\x00" * f["size"])
            os.utime(path, (f["mtime"], f["mtime"]))
        with patch.object(tiles, "TILE_DIR", root), \
                patch.object(tiles, "TILE_CACHE_MAX_MB", case["max_mb"]), \
                contextlib.redirect_stdout(io.StringIO()):
            tiles.tile_cache_prune()
        survivors = sorted(p.relative_to(root).as_posix() for p in root.rglob("*.png"))
    return {"name": case["name"], "max_mb": case["max_mb"], "files": case["files"], "survivors": survivors}


def build_cases():
    out = io.StringIO()
    upstream = _Upstream()
    try:
        with contextlib.redirect_stdout(out), \
                patch.object(handlers.Handler, "log_message", lambda *a, **k: None):
            with _http() as port:
                cases = [run_case(c, port, upstream) for c in CASES]
    finally:
        upstream.close()
    prune_cases = [run_prune_case(c) for c in PRUNE_CASES]
    blank = tiles._transparent_tile()
    doc = {
        "cases": cases,
        "prune_cases": prune_cases,
        "constants": {
            "test_ua": TEST_UA,
            "default_ua": "GeoFrame/0.1 (personal photo frame)",
            "default_per_min": 240,
            "default_cache_max_mb": 16384,
            "timeout": 10,
            "window_seconds": 60.0,
            "prune_interval": 3600,
            "max_zoom": {k: v[1] for k, v in tiles.TILE_SOURCES.items()},
            "concurrency": {k: v[2] for k, v in tiles.TILE_SOURCES.items()},
            "urls": [{"layer": layer, "z": z, "x": x, "y": y,
                      "url": tiles._upstream_tile_url(layer, z, x, y)}
                     for layer, z, x, y in [("osm", 3, 1, 2), ("gsi", 18, 200, 100), ("osm", 19, 524287, 0)]],
            "blank_png_hex": blank.hex(),
            "blank_png_sha256": _sha(blank),
            "png_a_sha256": _sha(PNG_A),
            "png_b_sha256": _sha(PNG_B),
        },
    }
    return doc


def render(doc):
    """フィクスチャのファイルに書く文字列（キー順固定・1ケース1行）。"""
    def dump(value):
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(", ", ": "))

    cases = ",\n".join("  " + dump(item) for item in doc["cases"])
    prunes = ",\n".join("  " + dump(item) for item in doc["prune_cases"])
    return ("{\n"
            f' "cases": [\n{cases}\n ],\n'
            f' "constants": {dump(doc["constants"])},\n'
            f' "prune_cases": [\n{prunes}\n ]\n'
            "}\n")
