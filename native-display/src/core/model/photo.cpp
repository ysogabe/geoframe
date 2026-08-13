// photo.cpp — 写真モデルのスタブ。
//
// joinUrl の 2 つのオーバーロードは実装済み（URL の非対称は実測済みの
// 落とし穴であり、ここが空だと各レーンが素朴な連結を書いて 404 を踏む）。
// signature() / indexOf() の本実装はレーン 1B。
//
// このファイルは env / roomenv / presence の各ヘッダも include している。
// ★理由: 移植性ゲート (tools/check-esp-portability.sh) が検証するのは
//   src/core/**/*.cpp であり、どの .cpp からも include されないヘッダは
//   ESP32-P4 でコンパイルされないまま「PASS」になってしまう。

#include "ambient/core/model/photo.hpp"

#include <algorithm>
#include <cstddef>
#include <cstdint>

#include "ambient/core/model/env.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"

namespace ambient::core {

namespace {

// 末尾のスラッシュを落とした base を返す。
[[nodiscard]] std::string_view trimTrailingSlash(std::string_view base) noexcept {
  while (!base.empty() && base.back() == '/') {
    base.remove_suffix(1);
  }
  return base;
}

}  // namespace

std::string joinUrl(std::string_view base, const PhotoRelPath& path) {
  // /api/photos の file は先頭スラッシュ **無し** なので、区切りを補う。
  std::string out{trimTrailingSlash(base)};
  out += '/';
  out += path.value;
  return out;
}

std::optional<AdminAbsPath> AdminAbsPath::make(std::string_view raw) {
  // 空、または先頭が '/' でない値は、連結すると base の authority 部へ
  // 食い込む。"@evil.example/x.jpg" や ".evil.example/x.jpg" で接続先ホストを
  // すり替えられる（理由の詳細はヘッダのコメント）。
  if (raw.empty() || raw.front() != '/') {
    return std::nullopt;
  }
  // "//..." はプロトコル相対 URL。"http:" + "//evil.example/..." になって
  // やはり別ホストへ行く。先頭が '/' であることだけでは防げない。
  if (raw.size() >= 2 && raw[1] == '/') {
    return std::nullopt;
  }
  return AdminAbsPath{raw};
}

std::string joinUrl(std::string_view base, const AdminAbsPath& path) {
  // /api/admin/photos の thumb は先頭スラッシュ **あり** なので、補わない。
  // 補うと "//" になり 404 になる。
  //
  // 先頭スラッシュの検証はここではなく AdminAbsPath::make() が済ませている。
  // 不正な値を持つ AdminAbsPath は構築できないので、ここでの再検査は不要。
  std::string out{trimTrailingSlash(base)};
  out += path.value();
  return out;
}

void PhotoManifest::rebuildIndex() {
  idOrder.clear();
  idOrder.reserve(items.size());
  for (std::size_t i = 0; i < items.size(); ++i) {
    idOrder.push_back(static_cast<std::uint32_t>(i));
  }
  const std::vector<PhotoItem>& snapshot = items;
  std::sort(idOrder.begin(), idOrder.end(),
            [&snapshot](std::uint32_t a, std::uint32_t b) noexcept {
              return snapshot[a].id < snapshot[b].id;
            });
}

std::string PhotoManifest::signature() const {
  // 出典 scheduler.js の photoSig:
  //   for each photo: for each char of String(p.id): h = (h*31 + charCode) | 0
  //   return PHOTOS.length + ':' + (h>>>0).toString(36)
  //
  // JS の |0 は 32bit へ切り詰めるので、uint32 の自然な巻き戻りと一致する。
  std::uint32_t hash = 0;
  for (const PhotoItem& item : items) {
    for (const char c : item.id) {
      hash = hash * 31U + static_cast<std::uint32_t>(static_cast<unsigned char>(c));
    }
  }

  // 36 進（0-9a-z）。JS の Number.prototype.toString(36) と同じ字種・同じ順。
  char digits[7];  // 2^32-1 は 36 進で 6 桁 ("1z141z")
  std::size_t len = 0;
  if (hash == 0) {
    digits[len++] = '0';
  }
  while (hash != 0) {
    const std::uint32_t d = hash % 36U;
    digits[len++] = d < 10U ? static_cast<char>('0' + d) : static_cast<char>('a' + (d - 10U));
    hash /= 36U;
  }

  std::string out = std::to_string(items.size());
  out += ':';
  while (len > 0) {
    out += digits[--len];
  }
  return out;
}

std::optional<std::size_t> indexOf(const PhotoManifest& manifest, std::string_view id) noexcept {
  // 索引があれば二分探索。noexcept なのでここでは一切確保しない。
  if (manifest.idOrder.size() == manifest.items.size()) {
    std::size_t lo = 0;
    std::size_t hi = manifest.idOrder.size();
    while (lo < hi) {
      const std::size_t mid = lo + (hi - lo) / 2;
      const std::size_t candidate = manifest.idOrder[mid];
      // 索引が古くて範囲外を指していても落ちないようにする。
      if (candidate >= manifest.items.size()) {
        break;
      }
      const std::string& key = manifest.items[candidate].id;
      if (key < id) {
        lo = mid + 1;
      } else if (id < key) {
        hi = mid;
      } else {
        return candidate;
      }
    }
  }

  // 索引が無い / 古い / 二分探索が外した場合の保険。
  // ★ここを省くと、rebuildIndex() の呼び忘れが「写真が消えた」に化ける。
  for (std::size_t i = 0; i < manifest.items.size(); ++i) {
    if (manifest.items[i].id == id) {
      return i;
    }
  }
  return std::nullopt;
}

}  // namespace ambient::core
