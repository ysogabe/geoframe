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


if __name__ == "__main__":
    unittest.main()
