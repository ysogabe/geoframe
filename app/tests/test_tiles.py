"""背景地図タイルプロキシ（管理ページの位置修正用の osm / gsi）のテスト。"""
import io
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

from PIL import Image

from geoframe import handlers, tiles


def _png():
    buf = io.BytesIO()
    Image.new("RGB", (256, 256), (20, 80, 140)).save(buf, format="PNG")
    return buf.getvalue()


def _response(raw):
    class Resp:
        def __enter__(self_):
            return self_

        def __exit__(self_, *exc):
            return False

        def read(self_):
            return raw
    return Resp()


class FetchTileTest(unittest.TestCase):
    def setUp(self):
        self._tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmpdir.cleanup)
        for patcher in (patch.object(tiles, "TILE_DIR", Path(self._tmpdir.name)),
                        # 各テストが共有スライディング窓の空の状態から始まるようにする
                        patch.object(tiles, "_tile_fetch_win", tiles.deque())):
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_upstream_urls(self):
        self.assertEqual(tiles._upstream_tile_url("osm", 3, 1, 2),
                         "https://tile.openstreetmap.org/3/1/2.png")
        self.assertEqual(tiles._upstream_tile_url("gsi", 3, 1, 2),
                         "https://cyberjapandata.gsi.go.jp/xyz/pale/3/1/2.png")

    def test_png_is_cached_and_then_served_without_upstream(self):
        raw = _png()
        with patch.object(tiles.urllib.request, "urlopen", return_value=_response(raw)):
            body, status = tiles.fetch_tile("gsi", 10, 1, 1)
        self.assertEqual((body, status), (raw, "miss"))
        with patch.object(tiles.urllib.request, "urlopen",
                          side_effect=AssertionError("キャッシュヒット時に上流へ行った")):
            body, status = tiles.fetch_tile("gsi", 10, 1, 1)
        self.assertEqual((body, status), (raw, "hit"))

    def test_upstream_404_caches_transparent_tile(self):
        error = urllib.error.HTTPError("http://example", 404, "not found", {}, None)
        with patch.object(tiles.urllib.request, "urlopen", side_effect=error):
            body, status = tiles.fetch_tile("osm", 10, 1, 1)
        self.assertEqual((body, status), (tiles._transparent_tile(), "miss"))
        self.assertTrue((tiles.TILE_DIR / "osm" / "10" / "1" / "1.png").exists())

    def test_upstream_5xx_is_not_cached(self):
        error = urllib.error.HTTPError("http://example", 503, "busy", {}, None)
        with patch.object(tiles.urllib.request, "urlopen", side_effect=error):
            body, status = tiles.fetch_tile("osm", 10, 1, 1)
        self.assertEqual((body, status), (tiles._transparent_tile(), "error"))
        self.assertFalse((tiles.TILE_DIR / "osm" / "10" / "1" / "1.png").exists())

    def test_non_png_payload_is_rejected_without_caching(self):
        with patch.object(tiles.urllib.request, "urlopen",
                          return_value=_response(b"<html>captive portal</html>")):
            body, status = tiles.fetch_tile("gsi", 10, 1, 1)
        self.assertEqual((body, status), (tiles._transparent_tile(), "error"))
        self.assertFalse((tiles.TILE_DIR / "gsi" / "10" / "1" / "1.png").exists())

    def test_throttled_request_does_not_reach_upstream(self):
        with patch.object(tiles, "_tile_fetch_allowed", return_value=False), \
                patch.object(tiles.urllib.request, "urlopen",
                             side_effect=AssertionError("流量上限なのに上流へ行った")):
            body, status = tiles.fetch_tile("osm", 10, 1, 1)
        self.assertEqual((body, status), (tiles._transparent_tile(), "throttled"))


class ServeTileTest(unittest.TestCase):
    def _serve(self, path):
        class _FakeRequest:
            def makefile(self, mode, bufsize=-1):
                return io.BytesIO()

        h = handlers.Handler(_FakeRequest(), ("127.0.0.1", 0), None)
        # __init__はfinish()でwfile/rfileを閉じてしまうため、応答先を差し替える。
        h.wfile = io.BytesIO()
        h.path = path
        h.requestline = ""
        h.request_version = "HTTP/1.0"
        h.command = "GET"
        with patch.object(handlers, "fetch_tile", return_value=(b"png", "hit")) as fetch:
            h._serve_tile(path)
            self.fetch_call = fetch.call_args
        head, _, body = h.wfile.getvalue().partition(b"\r\n\r\n")
        return head.split(b"\r\n")[0], body

    def test_known_layer_is_served(self):
        status_line, body = self._serve("/tiles/gsi/5/1/1.png")
        self.assertIn(b" 200 ", status_line)
        self.assertEqual(body, b"png")
        self.assertEqual(self.fetch_call.args, ("gsi", 5, 1, 1))

    def test_removed_3d_layers_are_not_routed(self):
        for layer in ("gsi-photo", "gsi-dem"):
            status_line, _ = self._serve(f"/tiles/{layer}/5/1/1.png")
            self.assertIn(b" 404 ", status_line, layer)

    def test_out_of_range_coordinates_and_zoom_are_404(self):
        for path in ("/tiles/osm/5/64/0.png", "/tiles/gsi/19/0/0.png"):
            status_line, _ = self._serve(path)
            self.assertIn(b" 404 ", status_line, path)


if __name__ == "__main__":
    unittest.main()
