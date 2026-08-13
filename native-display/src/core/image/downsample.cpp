#include "ambient/core/image/downsample.hpp"

namespace ambient::core {
namespace {

// ---------------------------------------------------------------------------
// 小さなバイト読み出し（範囲検査つき）
// ---------------------------------------------------------------------------
//
// ★span の operator[] は範囲検査をしない。ここは **信頼できない入力** を読む
//   ので、添字を作る前に必ず長さを確かめること。

[[nodiscard]] bool readBigEndian16(std::span<const std::uint8_t> bytes, std::size_t offset,
                                   std::uint32_t& out) noexcept {
  if (offset + 2 > bytes.size()) {
    return false;
  }
  out = (static_cast<std::uint32_t>(bytes[offset]) << 8) |
        static_cast<std::uint32_t>(bytes[offset + 1]);
  return true;
}

[[nodiscard]] bool readBigEndian32(std::span<const std::uint8_t> bytes, std::size_t offset,
                                   std::uint32_t& out) noexcept {
  if (offset + 4 > bytes.size()) {
    return false;
  }
  out = (static_cast<std::uint32_t>(bytes[offset]) << 24) |
        (static_cast<std::uint32_t>(bytes[offset + 1]) << 16) |
        (static_cast<std::uint32_t>(bytes[offset + 2]) << 8) |
        static_cast<std::uint32_t>(bytes[offset + 3]);
  return true;
}

[[nodiscard]] bool startsWith(std::span<const std::uint8_t> bytes,
                              std::span<const std::uint8_t> prefix) noexcept {
  if (bytes.size() < prefix.size()) {
    return false;
  }
  for (std::size_t i = 0; i < prefix.size(); ++i) {
    if (bytes[i] != prefix[i]) {
      return false;
    }
  }
  return true;
}

[[nodiscard]] std::optional<ImageSize> makeSize(std::uint32_t width,
                                                std::uint32_t height) noexcept {
  if (width == 0 || height == 0 || width > kMaxImageDimension ||
      height > kMaxImageDimension) {
    return std::nullopt;
  }
  return ImageSize{width, height};
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------
//
// レイアウト（固定）:
//   0..7   シグネチャ 89 50 4E 47 0D 0A 1A 0A
//   8..11  最初のチャンクの長さ（IHDR は 13）
//   12..15 "IHDR"
//   16..19 幅   （ビッグエンディアン）
//   20..23 高さ （ビッグエンディアン）
//
// ★仕様上 IHDR は必ず先頭チャンクなので、チャンクを走査する必要はない。

// ★宣言されたチャンクが最後まで届いているかも確認する。
//   途中で切れた応答（回線断・上限で打ち切られた本文）を「正常な画像」として
//   通してしまうと、デコードで初めて失敗して原因が分かりにくくなる。
//   IHDR は長さ 13 固定 + CRC 4 なので、必要な総バイト数は 8+4+4+13+4 = 33。
[[nodiscard]] std::optional<ImageSize> probePng(
    std::span<const std::uint8_t> bytes) noexcept {
  static constexpr std::uint8_t kIhdr[] = {'I', 'H', 'D', 'R'};
  static constexpr std::size_t kIhdrTotalBytes = 33;

  if (bytes.size() < kIhdrTotalBytes) {
    return std::nullopt;
  }
  if (!startsWith(bytes.subspan(12, 4), std::span<const std::uint8_t>(kIhdr, 4))) {
    return std::nullopt;
  }

  std::uint32_t chunkLength = 0;
  if (!readBigEndian32(bytes, 8, chunkLength) || chunkLength != 13) {
    return std::nullopt;
  }

  std::uint32_t width = 0;
  std::uint32_t height = 0;
  if (!readBigEndian32(bytes, 16, width) || !readBigEndian32(bytes, 20, height)) {
    return std::nullopt;
  }
  return makeSize(width, height);
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------
//
// SOI (FFD8) の後ろにセグメントが並ぶ。各セグメントは
//   FF <marker> <length:2（length 自身を含む）> <payload>
// ただし SOI/EOI/TEM/RSTn は長さを持たない。
//
// 寸法は SOFn（FFC0〜FFCF のうち C4=DHT / C8=JPG / CC=DAC を除く）にある:
//   +0 長さ(2) / +2 精度(1) / +3 高さ(2) / +5 幅(2)
//
// ★SOS (FFDA) 以降は圧縮データ本体で、FF が任意の位置に現れるためマーカー走査が
//   成立しない。SOF を見つける前に SOS へ到達したら諦める（nullopt）。

[[nodiscard]] bool isJpegSofMarker(std::uint8_t marker) noexcept {
  if (marker < 0xC0 || marker > 0xCF) {
    return false;
  }
  return marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
}

[[nodiscard]] std::optional<ImageSize> probeJpeg(
    std::span<const std::uint8_t> bytes) noexcept {
  if (bytes.size() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8) {
    return std::nullopt;
  }

  std::size_t pos = 2;
  // 上限を掛ける（壊れた入力で延々と回らないように）。セグメント数の現実的な
  // 上限を大きく超える値であれば十分。
  for (int guard = 0; guard < 4096; ++guard) {
    if (pos >= bytes.size() || bytes[pos] != 0xFF) {
      return std::nullopt;  // マーカーは必ず FF から始まる
    }
    // マーカーの直前には FF の詰め物が並びうる（規格上許されている）。
    while (pos < bytes.size() && bytes[pos] == 0xFF) {
      ++pos;
    }
    if (pos >= bytes.size()) {
      return std::nullopt;
    }

    const std::uint8_t marker = bytes[pos];
    ++pos;

    // 長さを持たないマーカー。
    if (marker == 0xD8 || marker == 0x01 || (marker >= 0xD0 && marker <= 0xD7)) {
      continue;
    }
    if (marker == 0xD9 || marker == 0xDA) {
      return std::nullopt;  // EOI / SOS。ここから先に SOF は無い
    }

    std::uint32_t length = 0;
    if (!readBigEndian16(bytes, pos, length) || length < 2) {
      return std::nullopt;
    }

    // ★宣言されたセグメントが最後まで届いているかを先に確認する。
    //   途中で切れた応答を「正常な画像」として通さないため（PNG 側と同じ理由）。
    if (pos + length > bytes.size()) {
      return std::nullopt;
    }

    if (isJpegSofMarker(marker)) {
      std::uint32_t height = 0;
      std::uint32_t width = 0;
      // pos は長さフィールドの先頭。+3 が高さ、+5 が幅。
      if (length < 7 || !readBigEndian16(bytes, pos + 3, height) ||
          !readBigEndian16(bytes, pos + 5, width)) {
        return std::nullopt;
      }
      return makeSize(width, height);
    }

    pos += length;
  }
  return std::nullopt;
}

}  // namespace

// ---------------------------------------------------------------------------
// probe / 予算
// ---------------------------------------------------------------------------

std::optional<ImageSize> probeImageSize(std::span<const std::uint8_t> bytes) noexcept {
  static constexpr std::uint8_t kPngSignature[] = {0x89, 0x50, 0x4E, 0x47,
                                                   0x0D, 0x0A, 0x1A, 0x0A};

  if (bytes.size() >= 8 &&
      startsWith(bytes, std::span<const std::uint8_t>(kPngSignature, 8))) {
    return probePng(bytes);
  }
  if (bytes.size() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8) {
    return probeJpeg(bytes);
  }
  return std::nullopt;
}

std::uint64_t decodedBytesRGBA(ImageSize size) noexcept {
  if (size.width == 0 || size.height == 0 || size.width > kMaxImageDimension ||
      size.height > kMaxImageDimension) {
    return 0;
  }
  // 1 辺が kMaxImageDimension 以下なので 65535*65535*4 = 1.7e10 < 2^64。溢れない。
  return static_cast<std::uint64_t>(size.width) * static_cast<std::uint64_t>(size.height) *
         4ULL;
}

bool fitsDecodeBudget(ImageSize size, const DecodeBudget& budget) noexcept {
  const std::uint64_t bytes = decodedBytesRGBA(size);
  return bytes != 0 && bytes <= budget.maxDecodedBytes;
}

// ---------------------------------------------------------------------------
// box filter
// ---------------------------------------------------------------------------

bool downsampleBoxRGBA(const std::uint8_t* src, ImageSize srcSize, std::size_t srcStride,
                       std::uint8_t* dst, ImageSize dstSize, std::size_t dstStride) noexcept {
  if (src == nullptr || dst == nullptr) {
    return false;
  }
  if (srcSize.width == 0 || srcSize.height == 0 || dstSize.width == 0 ||
      dstSize.height == 0) {
    return false;
  }
  // 縮小専用（等倍は可）。拡大は box filter の定義から外れるので呼び出し側で
  // 別の手段を使うこと。
  if (dstSize.width > srcSize.width || dstSize.height > srcSize.height) {
    return false;
  }
  if (srcStride < static_cast<std::size_t>(srcSize.width) * 4U ||
      dstStride < static_cast<std::size_t>(dstSize.width) * 4U) {
    return false;
  }

  const std::uint64_t srcW = srcSize.width;
  const std::uint64_t srcH = srcSize.height;
  const std::uint64_t dstW = dstSize.width;
  const std::uint64_t dstH = dstSize.height;

  for (std::uint64_t dy = 0; dy < dstH; ++dy) {
    std::uint64_t y0 = (dy * srcH) / dstH;
    std::uint64_t y1 = ((dy + 1) * srcH) / dstH;
    if (y1 <= y0) {
      y1 = y0 + 1;
    }
    if (y1 > srcH) {
      y1 = srcH;
    }

    std::uint8_t* dstRow = dst + static_cast<std::size_t>(dy) * dstStride;

    for (std::uint64_t dx = 0; dx < dstW; ++dx) {
      std::uint64_t x0 = (dx * srcW) / dstW;
      std::uint64_t x1 = ((dx + 1) * srcW) / dstW;
      if (x1 <= x0) {
        x1 = x0 + 1;
      }
      if (x1 > srcW) {
        x1 = srcW;
      }

      std::uint64_t sum0 = 0;
      std::uint64_t sum1 = 0;
      std::uint64_t sum2 = 0;
      std::uint64_t sum3 = 0;

      for (std::uint64_t sy = y0; sy < y1; ++sy) {
        const std::uint8_t* srcRow = src + static_cast<std::size_t>(sy) * srcStride;
        const std::uint8_t* pixel = srcRow + static_cast<std::size_t>(x0) * 4U;
        for (std::uint64_t sx = x0; sx < x1; ++sx) {
          sum0 += pixel[0];
          sum1 += pixel[1];
          sum2 += pixel[2];
          sum3 += pixel[3];
          pixel += 4;
        }
      }

      const std::uint64_t area = (x1 - x0) * (y1 - y0);
      const std::uint64_t half = area / 2;

      std::uint8_t* out = dstRow + static_cast<std::size_t>(dx) * 4U;
      out[0] = static_cast<std::uint8_t>((sum0 + half) / area);
      out[1] = static_cast<std::uint8_t>((sum1 + half) / area);
      out[2] = static_cast<std::uint8_t>((sum2 + half) / area);
      out[3] = static_cast<std::uint8_t>((sum3 + half) / area);
    }
  }

  return true;
}

}  // namespace ambient::core
