"""背景タイルプロキシの航空・衛星写真対応テスト。"""
import io
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

from PIL import Image

from geoframe import tiles


class GsiPhotoTileTest(unittest.TestCase):
    def test_landsat_is_used_through_zoom_13(self):
        url = tiles._upstream_tile_url("gsi-photo", 13, 7276, 3225)
        self.assertEqual(
            url,
            "https://cyberjapandata.gsi.go.jp/xyz/lndst/13/7276/3225.png",
        )

    def test_seamless_photo_is_used_from_zoom_14(self):
        url = tiles._upstream_tile_url("gsi-photo", 14, 14552, 6451)
        self.assertEqual(
            url,
            "https://cyberjapandata.gsi.go.jp/xyz/seamlessphoto/14/14552/6451.jpg",
        )

    def test_zoom_outside_official_bands_is_rejected(self):
        with self.assertRaises(ValueError):
            tiles._upstream_tile_url("gsi-photo", 1, 1, 0)

    def test_jpeg_photo_tile_is_normalised_to_png(self):
        source = io.BytesIO()
        Image.new("RGB", (256, 256), (20, 80, 140)).save(source, format="JPEG")
        result = tiles._normalise_tile_bytes(source.getvalue())
        self.assertTrue(result.startswith(tiles.PNG_MAGIC))
        with Image.open(io.BytesIO(result)) as image:
            self.assertEqual(image.size, (256, 256))
            self.assertEqual(image.format, "PNG")

    def test_unexpected_payload_is_rejected(self):
        with self.assertRaises(ValueError):
            tiles._normalise_tile_bytes(b"<html>not a tile</html>")


class GsiDemTileUrlTest(unittest.TestCase):
    """z0-8 は全球DEM（demgm_png）、z9-14 は数値標高モデル（dem_png）へ振り分ける。"""

    def test_zoom0_is_global_demgm(self):
        url = tiles._upstream_tile_url("gsi-dem", 0, 0, 0)
        self.assertEqual(url, "https://cyberjapandata.gsi.go.jp/xyz/demgm_png/0/0/0.png")

    def test_zoom8_upper_bound_is_still_demgm(self):
        url = tiles._upstream_tile_url("gsi-dem", 8, 224, 97)
        self.assertEqual(
            url, "https://cyberjapandata.gsi.go.jp/xyz/demgm_png/8/224/97.png")

    def test_zoom9_lower_bound_switches_to_dem10b(self):
        url = tiles._upstream_tile_url("gsi-dem", 9, 454, 199)
        self.assertEqual(
            url, "https://cyberjapandata.gsi.go.jp/xyz/dem_png/9/454/199.png")

    def test_zoom14_upper_bound_is_still_dem10b(self):
        url = tiles._upstream_tile_url("gsi-dem", 14, 14552, 6451)
        self.assertEqual(
            url, "https://cyberjapandata.gsi.go.jp/xyz/dem_png/14/14552/6451.png")

    def test_zoom_above_supported_band_is_rejected(self):
        with self.assertRaises(ValueError):
            tiles._upstream_tile_url("gsi-dem", 15, 0, 0)

    def test_negative_zoom_below_supported_band_is_rejected(self):
        with self.assertRaises(ValueError):
            tiles._upstream_tile_url("gsi-dem", -1, 0, 0)


class GsiDemNoDataTileTest(unittest.TestCase):
    """欠測は透明PNGではなく、GSIエンコードの欠測値RGB(128,0,0)を返す。"""

    def test_no_data_tile_is_opaque_rgb_128_0_0(self):
        data = tiles._dem_no_data_tile_bytes()
        self.assertTrue(data.startswith(tiles.PNG_MAGIC))
        with Image.open(io.BytesIO(data)) as image:
            self.assertEqual(image.size, (256, 256))
            # RGBA/透明ではなく不透明RGB（raster-demデコーダが透明=標高0mと誤認しないため）
            self.assertEqual(image.mode, "RGB")
            for corner in [(0, 0), (255, 0), (0, 255), (255, 255)]:
                self.assertEqual(image.getpixel(corner), (128, 0, 0))

    def test_no_data_pixel_encodes_to_gsi_no_data_sentinel(self):
        r, g, b = tiles.DEM_NO_DATA_RGB
        x = 65536 * r + 256 * g + b
        self.assertEqual(x, 2 ** 23, "RGB(128,0,0)はGSI仕様上のno-data閾値2^23と一致する")

    def test_no_data_tile_is_cached_after_first_call(self):
        first = tiles._dem_no_data_tile_bytes()
        second = tiles._dem_no_data_tile_bytes()
        self.assertIs(first, second, "プロセス内で1回だけ生成して使い回す")


class FetchTileDemAbsenceTest(unittest.TestCase):
    """上流404（提供範囲外）を安全にキャッシュし、gsi-demだけno-dataエンコードで返す。"""

    def setUp(self):
        self._tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmpdir.cleanup)
        patcher = patch.object(tiles, "TILE_DIR", Path(self._tmpdir.name))
        patcher.start()
        self.addCleanup(patcher.stop)
        # 各テストがフェッチ許可の宣言的な状態から始まるよう、共有スライディング窓をリセットする。
        win_patcher = patch.object(tiles, "_tile_fetch_win", tiles.deque())
        win_patcher.start()
        self.addCleanup(win_patcher.stop)

    def test_upstream_404_caches_no_data_tile_for_gsi_dem(self):
        def fake_urlopen(*a, **k):
            raise urllib.error.HTTPError("http://example", 404, "not found", {}, None)

        with patch.object(tiles.urllib.request, "urlopen", side_effect=fake_urlopen):
            body, status = tiles.fetch_tile("gsi-dem", 10, 1, 1)
        self.assertEqual(status, "miss")
        self.assertEqual(body, tiles._dem_no_data_tile_bytes())

        cached = tiles.TILE_DIR / "gsi-dem" / "10" / "1" / "1.png"
        self.assertTrue(cached.exists(), "欠測を後続リクエストのためファイルへ永続化する")
        self.assertEqual(cached.read_bytes(), tiles._dem_no_data_tile_bytes())

        # 2回目はキャッシュヒットで上流へ行かない（urlopenを呼んだら例外で気づける）。
        with patch.object(tiles.urllib.request, "urlopen",
                           side_effect=AssertionError("キャッシュヒット時に上流へ行った")):
            body2, status2 = tiles.fetch_tile("gsi-dem", 10, 1, 1)
        self.assertEqual(status2, "hit")
        self.assertEqual(body2, tiles._dem_no_data_tile_bytes())

    def test_upstream_404_still_caches_transparent_tile_for_photo_layer(self):
        """gsi-dem専用の欠測エンコードが他layerへ漏れないことの陰性対照。"""
        def fake_urlopen(*a, **k):
            raise urllib.error.HTTPError("http://example", 404, "not found", {}, None)

        with patch.object(tiles.urllib.request, "urlopen", side_effect=fake_urlopen):
            body, status = tiles.fetch_tile("gsi-photo", 14, 1, 1)
        self.assertEqual(status, "miss")
        self.assertEqual(body, tiles._transparent_tile())
        self.assertNotEqual(body, tiles._dem_no_data_tile_bytes())

    def test_upstream_5xx_returns_no_data_without_caching_for_gsi_dem(self):
        def fake_urlopen(*a, **k):
            raise urllib.error.HTTPError("http://example", 503, "busy", {}, None)

        with patch.object(tiles.urllib.request, "urlopen", side_effect=fake_urlopen):
            body, status = tiles.fetch_tile("gsi-dem", 10, 2, 2)
        self.assertEqual(status, "error")
        self.assertEqual(body, tiles._dem_no_data_tile_bytes())
        cached = tiles.TILE_DIR / "gsi-dem" / "10" / "2" / "2.png"
        self.assertFalse(cached.exists(), "5xx/一時エラーは永続キャッシュしない（次回再試行させる）")

    def test_throttled_gsi_dem_request_returns_no_data_without_network(self):
        with patch.object(tiles, "_tile_fetch_allowed", return_value=False), \
             patch.object(tiles.urllib.request, "urlopen",
                           side_effect=AssertionError("流量制限中に上流へ行った")):
            body, status = tiles.fetch_tile("gsi-dem", 5, 0, 0)
        self.assertEqual(status, "throttled")
        self.assertEqual(body, tiles._dem_no_data_tile_bytes())

    def test_unsupported_zoom_returns_no_data_without_upstream_or_budget(self):
        """z>14（提供ズーム外）は上流へ行かず、分/枚の共有予算も消費せずno-dataを返す。
        上流URL解決を予算チェックより先に行う回帰テスト。urlopen/_tile_fetch_allowed
        のどちらかが呼ばれたらAssertionErrorで検出する。"""
        with patch.object(tiles.urllib.request, "urlopen",
                          side_effect=AssertionError("提供ズーム外で上流へ行った")), \
             patch.object(tiles, "_tile_fetch_allowed",
                          side_effect=AssertionError("提供ズーム外でfetch予算を消費した")):
            body, status = tiles.fetch_tile("gsi-dem", 15, 0, 0)
        self.assertEqual(status, "error")
        self.assertEqual(body, tiles._dem_no_data_tile_bytes())

    def test_real_elevation_png_bytes_pass_through_unmodified(self):
        """24bit標高PNGはJPEG写真と違い、再エンコードせず生バイト列をそのまま配信する。"""
        source = io.BytesIO()
        # (R,G,B)=(1,0,0) -> x=65536, 標高655.36m相当。中身の値そのものは無関係で、
        # 「PNGバイト列が変質しない」ことだけを確認する。
        Image.new("RGB", (256, 256), (1, 0, 0)).save(source, format="PNG", compress_level=0)
        raw = source.getvalue()

        def fake_urlopen(*a, **k):
            class Resp:
                def __enter__(self_):
                    return self_

                def __exit__(self_, *exc):
                    return False

                def read(self_):
                    return raw
            return Resp()

        with patch.object(tiles.urllib.request, "urlopen", side_effect=fake_urlopen):
            body, status = tiles.fetch_tile("gsi-dem", 9, 3, 3)
        self.assertEqual(status, "miss")
        self.assertEqual(body, raw, "標高PNGは非可逆な再圧縮をしない")


class ServeTileDemUnsupportedZoomTest(unittest.TestCase):
    """handlers._serve_tile が gsi-dem の提供ズーム外（z>14）を404ではなく
    200 + RGB(128,0,0) で返す回帰テスト。404だとNavaraのraster-dem loaderが
    応答bodyをデコードせず初期化を失敗させる（AbortableImageLoaderが非200を拒否）。"""

    def _serve(self, path):
        from geoframe import handlers

        class _FakeRequest:
            def makefile(self, mode, bufsize=-1):
                return io.BytesIO()

        h = handlers.Handler(_FakeRequest(), ("127.0.0.1", 0), None)
        # __init__はfinish()でwfile/rfileを閉じてしまうため、応答先を差し替える。
        # 空rfileのままhandle_one_requestは即returnするので、requestline等の
        # リクエスト由来属性もsend_response/send_error/end_headersが参照する分だけ設定する。
        h.wfile = io.BytesIO()
        h.path = path
        h.requestline = ""
        h.request_version = "HTTP/1.0"
        h.command = "GET"
        h._serve_tile(path)
        return h.wfile.getvalue()

    def test_unsupported_zoom_gsi_dem_returns_200_no_data(self):
        raw = self._serve("/tiles/gsi-dem/15/0/0.png")
        head, _, body = raw.partition(b"\r\n\r\n")
        status_line = head.split(b"\r\n")[0]
        self.assertIn(b" 200 ", status_line,
                      "z15+は404ではなく200で返す（404はraster-dem loaderの初期化を失敗させる）")
        self.assertEqual(body, tiles._dem_no_data_tile_bytes())

    def test_invalid_tile_coordinates_still_404(self):
        """陰性対照: タイル座標範囲外（x>=2^z）は従来どおり404のまま。"""
        raw = self._serve("/tiles/gsi-dem/5/64/0.png")   # z=5 → xは0..31のはず
        head, _, _ = raw.partition(b"\r\n\r\n")
        status_line = head.split(b"\r\n")[0]
        self.assertIn(b" 404 ", status_line)


if __name__ == "__main__":
    unittest.main()
