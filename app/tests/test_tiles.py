"""背景タイルプロキシの航空・衛星写真対応テスト。"""
import io
import unittest

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


if __name__ == "__main__":
    unittest.main()
