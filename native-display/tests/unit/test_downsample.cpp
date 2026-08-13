// test_downsample.cpp — ambient/core/image/downsample.hpp の検証
//
// ★このプロジェクトで **ピクセル完全一致** を採ってよいのはここだけである。
//   downsampleBoxRGBA は純粋な整数演算で、SDL も GPU も経由しない。
//   render_tests 側が構造アサーションに留める理由は CMakeLists.txt を参照。
//
// 検証の狙い:
//   1. box filter が「対応矩形の平均」であること（値を手計算で固定する）
//   2. ★nearest との差 ― 市松模様が平坦になること（モアレが出ない根拠）
//   3. stride 経由で cover クロップ後の部分矩形を直接渡せること
//   4. ★probe が **攻撃者制御下の寸法** で壊れないこと（切り詰め / 巨大値 / 0）
//   5. 予算判定が実測値（derivative は通し、24MP 原本は弾く）と一致すること

#include <doctest.h>

#include <array>
#include <cstdint>
#include <span>
#include <vector>

#include "ambient/core/image/downsample.hpp"

namespace {

using ambient::core::DecodeBudget;
using ambient::core::ImageSize;
using ambient::core::decodedBytesRGBA;
using ambient::core::downsampleBoxRGBA;
using ambient::core::fitsDecodeBudget;
using ambient::core::probeImageSize;

// 全チャンネルに同じ値を敷いた RGBA 画像を作る（値は行優先で与える）。
std::vector<std::uint8_t> gray(std::span<const std::uint8_t> values) {
  std::vector<std::uint8_t> out;
  out.reserve(values.size() * 4);
  for (const std::uint8_t v : values) {
    out.push_back(v);
    out.push_back(v);
    out.push_back(v);
    out.push_back(255);
  }
  return out;
}

std::array<std::uint8_t, 4> pixelAt(const std::vector<std::uint8_t>& buffer,
                                    std::size_t width, std::size_t x, std::size_t y) {
  const std::size_t base = (y * width + x) * 4;
  return {buffer[base], buffer[base + 1], buffer[base + 2], buffer[base + 3]};
}

std::vector<std::uint8_t> makePngHeader(std::uint32_t width, std::uint32_t height) {
  std::vector<std::uint8_t> bytes = {0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
                                     0x00, 0x00, 0x00, 0x0D, 'I',  'H',  'D',  'R'};
  for (int shift = 24; shift >= 0; shift -= 8) {
    bytes.push_back(static_cast<std::uint8_t>((width >> shift) & 0xFFU));
  }
  for (int shift = 24; shift >= 0; shift -= 8) {
    bytes.push_back(static_cast<std::uint8_t>((height >> shift) & 0xFFU));
  }
  bytes.push_back(8);  // bit depth
  bytes.push_back(2);  // color type
  bytes.push_back(0);  // compression
  bytes.push_back(0);  // filter
  bytes.push_back(0);  // interlace
  // IHDR の CRC（値は検査しないので 0 埋め）。ここまでで 33 バイト。
  bytes.insert(bytes.end(), 4, 0);
  return bytes;
}

// SOI + APP0 + SOF0 の最小構成。実物と同じマーカー配置にしてある。
std::vector<std::uint8_t> makeJpegHeader(std::uint32_t width, std::uint32_t height) {
  std::vector<std::uint8_t> bytes = {
      0xFF, 0xD8,                                            // SOI
      0xFF, 0xE0, 0x00, 0x10, 'J', 'F', 'I', 'F', 0x00,      // APP0（長さ 16）
      0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,  // APP0 の残り
      0xFF, 0xC0, 0x00, 0x11, 0x08,                          // SOF0（長さ 17）
  };
  bytes.push_back(static_cast<std::uint8_t>((height >> 8) & 0xFFU));
  bytes.push_back(static_cast<std::uint8_t>(height & 0xFFU));
  bytes.push_back(static_cast<std::uint8_t>((width >> 8) & 0xFFU));
  bytes.push_back(static_cast<std::uint8_t>(width & 0xFFU));
  // コンポーネント記述（3 成分）。probe は読まないが長さは合わせておく。
  const std::array<std::uint8_t, 10> components{0x03, 0x01, 0x22, 0x00, 0x02,
                                                0x11, 0x01, 0x03, 0x11, 0x01};
  bytes.insert(bytes.end(), components.begin(), components.end());
  return bytes;
}

}  // namespace

// ============================================================================
// box filter
// ============================================================================

TEST_CASE("downsample: 2x2 -> 1x1 は 4 画素の平均（完全一致）") {
  const std::array<std::uint8_t, 4> values{0, 100, 200, 255};
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(4, 0);

  REQUIRE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 2 * 4, dst.data(),
                            ImageSize{1, 1}, 1 * 4));

  // (0 + 100 + 200 + 255) / 4 = 138.75 -> 四捨五入で 139
  CHECK(dst[0] == 139);
  CHECK(dst[1] == 139);
  CHECK(dst[2] == 139);
  CHECK(dst[3] == 255);  // アルファも同じ規則で平均される
}

TEST_CASE("downsample: 4x4 -> 2x2 は 2x2 ブロックごとの平均（完全一致）") {
  const std::array<std::uint8_t, 16> values{
      10,  20,  50,  60,   //
      30,  40,  70,  80,   //
      90,  100, 130, 140,  //
      110, 120, 150, 160,
  };
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(2 * 2 * 4, 0);

  REQUIRE(downsampleBoxRGBA(src.data(), ImageSize{4, 4}, 4 * 4, dst.data(),
                            ImageSize{2, 2}, 2 * 4));

  CHECK(pixelAt(dst, 2, 0, 0)[0] == 25);   // (10+20+30+40)/4
  CHECK(pixelAt(dst, 2, 1, 0)[0] == 65);   // (50+60+70+80)/4
  CHECK(pixelAt(dst, 2, 0, 1)[0] == 105);  // (90+100+110+120)/4
  CHECK(pixelAt(dst, 2, 1, 1)[0] == 145);  // (130+140+150+160)/4
}

TEST_CASE("downsample: ★市松模様が平坦になる（nearest ならモアレになる入力）") {
  // nearest は左上画素だけを拾うので 0 か 255 に振れる。
  // box filter なら必ず 128（= (0+255+255+0)/4 の四捨五入）になる。
  std::array<std::uint8_t, 64> values{};
  for (std::size_t y = 0; y < 8; ++y) {
    for (std::size_t x = 0; x < 8; ++x) {
      values[y * 8 + x] = static_cast<std::uint8_t>(((x + y) % 2 == 0) ? 0 : 255);
    }
  }
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(4 * 4 * 4, 0);

  REQUIRE(downsampleBoxRGBA(src.data(), ImageSize{8, 8}, 8 * 4, dst.data(),
                            ImageSize{4, 4}, 4 * 4));

  // ループの中で CHECK を撃たない（docs/CORE_CONTRACTS.md §11）。集計してから 1 回。
  int deviations = 0;
  for (std::size_t y = 0; y < 4; ++y) {
    for (std::size_t x = 0; x < 4; ++x) {
      if (pixelAt(dst, 4, x, y)[0] != 128) {
        ++deviations;
      }
    }
  }
  CHECK(deviations == 0);
}

TEST_CASE("downsample: 等倍は単純コピー（完全一致）") {
  const std::array<std::uint8_t, 4> values{1, 2, 3, 4};
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(2 * 2 * 4, 0);

  REQUIRE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 2 * 4, dst.data(),
                            ImageSize{2, 2}, 2 * 4));
  CHECK(dst == src);
}

TEST_CASE("downsample: 割り切れない比率でも矩形が隙間なく分割される") {
  // 3 -> 2。矩形は [0,1) と [1,3)。
  const std::array<std::uint8_t, 9> values{
      0,  10, 20,  //
      30, 40, 50,  //
      60, 70, 80,
  };
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(2 * 2 * 4, 0);

  REQUIRE(downsampleBoxRGBA(src.data(), ImageSize{3, 3}, 3 * 4, dst.data(),
                            ImageSize{2, 2}, 2 * 4));

  CHECK(pixelAt(dst, 2, 0, 0)[0] == 0);   // {0}
  CHECK(pixelAt(dst, 2, 1, 0)[0] == 15);  // {10,20}
  CHECK(pixelAt(dst, 2, 0, 1)[0] == 45);  // {30,60}
  CHECK(pixelAt(dst, 2, 1, 1)[0] == 60);  // {40,50,70,80} = 240/4
}

TEST_CASE("downsample: stride で cover クロップ後の部分矩形を直接渡せる") {
  // 4x4 の中央 2x2 だけを 1x1 へ落とす（コピーを挟まずポインタ + stride で渡す）。
  const std::array<std::uint8_t, 16> values{
      0, 0,  0,   0,  //
      0, 40, 60,  0,  //
      0, 80, 100, 0,  //
      0, 0,  0,   0,
  };
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(4, 0);

  const std::size_t stride = 4 * 4;
  const std::uint8_t* origin = src.data() + stride + 4;  // (x=1, y=1)

  REQUIRE(downsampleBoxRGBA(origin, ImageSize{2, 2}, stride, dst.data(), ImageSize{1, 1},
                            1 * 4));
  CHECK(dst[0] == 70);  // (40+60+80+100)/4
}

TEST_CASE("downsample: 不正な入力は false を返す（例外を投げない）") {
  const std::array<std::uint8_t, 4> values{1, 2, 3, 4};
  const std::vector<std::uint8_t> src = gray(values);
  std::vector<std::uint8_t> dst(4 * 4 * 4, 0);

  SUBCASE("null") {
    CHECK_FALSE(
        downsampleBoxRGBA(nullptr, ImageSize{2, 2}, 8, dst.data(), ImageSize{1, 1}, 4));
    CHECK_FALSE(
        downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 8, nullptr, ImageSize{1, 1}, 4));
  }
  SUBCASE("寸法 0") {
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{0, 2}, 8, dst.data(),
                                  ImageSize{1, 1}, 4));
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 8, dst.data(),
                                  ImageSize{1, 0}, 4));
  }
  SUBCASE("拡大は受け付けない") {
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 8, dst.data(),
                                  ImageSize{4, 4}, 16));
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 8, dst.data(),
                                  ImageSize{2, 4}, 8));
  }
  SUBCASE("stride が幅*4 未満") {
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 7, dst.data(),
                                  ImageSize{1, 1}, 4));
    CHECK_FALSE(downsampleBoxRGBA(src.data(), ImageSize{2, 2}, 8, dst.data(),
                                  ImageSize{1, 1}, 3));
  }
}

// ============================================================================
// probe — ★寸法は攻撃者制御下の値である
// ============================================================================

TEST_CASE("probe: PNG の IHDR を読む") {
  const std::vector<std::uint8_t> png = makePngHeader(2560, 1707);
  const auto size = probeImageSize(std::span<const std::uint8_t>(png));
  REQUIRE(size.has_value());
  CHECK(size->width == 2560);
  CHECK(size->height == 1707);
}

TEST_CASE("probe: JPEG の SOF0 を読む（APP0 を読み飛ばす）") {
  const std::vector<std::uint8_t> jpeg = makeJpegHeader(2560, 1707);
  const auto size = probeImageSize(std::span<const std::uint8_t>(jpeg));
  REQUIRE(size.has_value());
  CHECK(size->width == 2560);
  CHECK(size->height == 1707);
}

TEST_CASE("probe: 途中で切れた入力は nullopt（範囲外アクセスしない）") {
  const std::vector<std::uint8_t> png = makePngHeader(2560, 1707);
  const std::vector<std::uint8_t> jpeg = makeJpegHeader(2560, 1707);

  int unexpected = 0;
  for (std::size_t len = 0; len < png.size(); ++len) {
    if (probeImageSize(std::span<const std::uint8_t>(png.data(), len)).has_value()) {
      ++unexpected;
    }
  }
  for (std::size_t len = 0; len < jpeg.size(); ++len) {
    if (probeImageSize(std::span<const std::uint8_t>(jpeg.data(), len)).has_value()) {
      ++unexpected;
    }
  }
  CHECK(unexpected == 0);
}

TEST_CASE("probe: 寸法 0 と巨大値は nullopt（w*h*4 のオーバーフロー対策）") {
  SUBCASE("幅 0") {
    const std::vector<std::uint8_t> png = makePngHeader(0, 1080);
    CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(png)).has_value());
  }
  SUBCASE("高さ 0") {
    const std::vector<std::uint8_t> png = makePngHeader(1920, 0);
    CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(png)).has_value());
  }
  SUBCASE("PNG は 2^31-1 まで表現できる ― 上限超は弾く") {
    const std::vector<std::uint8_t> png = makePngHeader(0x7FFFFFFFU, 0x7FFFFFFFU);
    CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(png)).has_value());
  }
  SUBCASE("上限ちょうど（65535）は通す") {
    const std::vector<std::uint8_t> png = makePngHeader(65535, 65535);
    const auto size = probeImageSize(std::span<const std::uint8_t>(png));
    REQUIRE(size.has_value());
    // ★uint64 で溢れないことがここで担保される。
    CHECK(decodedBytesRGBA(*size) == 65535ULL * 65535ULL * 4ULL);
  }
}

TEST_CASE("probe: 未知の形式・空・ゴミは nullopt") {
  const std::array<std::uint8_t, 8> garbage{'<', '!', 'D', 'O', 'C', 'T', 'Y', 'P'};
  CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(garbage)).has_value());
  CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>()).has_value());

  // ★403 で返ってくる HTML を画像として食わせても落ちないこと。
  static constexpr char kHtml[] = "<!DOCTYPE HTML>\n<html lang=\"en\"><p>Error code: 403</p>";
  std::array<std::uint8_t, sizeof(kHtml) - 1> html{};
  for (std::size_t i = 0; i < html.size(); ++i) {
    html[i] = static_cast<std::uint8_t>(kHtml[i]);
  }
  CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(html)).has_value());
}

TEST_CASE("probe: SOF より先に SOS が来たら諦める（圧縮本体を走査しない）") {
  // SOS 以降は FF が任意の位置に現れるためマーカー走査が成立しない。
  const std::array<std::uint8_t, 6> bytes{0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x0C};
  CHECK_FALSE(probeImageSize(std::span<const std::uint8_t>(bytes)).has_value());
}

// ============================================================================
// デコード予算 — 実測値で固定する
// ============================================================================

TEST_CASE("budget: derivative は通し、リサイズされないローカル原本は弾く") {
  const DecodeBudget budget;  // 既定 32MiB

  // 実測: derivative は 2560x1707。RGBA 展開で 17,479,680 バイト。
  const ImageSize derivative{2560, 1707};
  CHECK(decodedBytesRGBA(derivative) == 17479680ULL);
  CHECK(fitsDecodeBudget(derivative, budget));

  // ★ローカル原本はサーバ側でリサイズされずに来る（リサイズ用のクエリ
  //   パラメータも専用エンドポイントも存在しない）。24MP なら約 96MB。
  const ImageSize original24mp{6000, 4000};
  CHECK(decodedBytesRGBA(original24mp) == 96000000ULL);
  CHECK_FALSE(fitsDecodeBudget(original24mp, budget));

  // 縮小後の 1280x720 は 3,686,400 バイト（先読み 2 枚 + 現在でも約 11MB）。
  CHECK(decodedBytesRGBA(ImageSize{1280, 720}) == 3686400ULL);
}

TEST_CASE("budget: 不正な寸法は 0 バイト扱いで必ず予算外") {
  const DecodeBudget budget;
  CHECK(decodedBytesRGBA(ImageSize{0, 100}) == 0);
  CHECK(decodedBytesRGBA(ImageSize{100, 0}) == 0);
  CHECK(decodedBytesRGBA(ImageSize{70000, 10}) == 0);
  CHECK_FALSE(fitsDecodeBudget(ImageSize{0, 100}, budget));
  CHECK_FALSE(fitsDecodeBudget(ImageSize{70000, 10}, budget));
}

TEST_CASE("budget: 上限は設定できる（Tab5 では小さくする）") {
  const DecodeBudget tight{4ULL * 1024 * 1024};
  CHECK_FALSE(fitsDecodeBudget(ImageSize{2560, 1707}, tight));
  CHECK(fitsDecodeBudget(ImageSize{1024, 1024}, tight));  // 4,194,304 = ちょうど
}
