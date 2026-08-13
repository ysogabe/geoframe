#include "ambient/render/sdl_runtime.hpp"

#include <SDL.h>
#include <SDL_image.h>
#include <SDL_ttf.h>

#include <cstdint>
#include <cstdio>
#include <cstring>

#ifdef __APPLE__
#include <mach-o/dyld.h>
#endif

namespace ambient::render {
namespace {

bool same_version(const SDL_version& a, const SDL_version& b) noexcept {
  return a.major == b.major && a.minor == b.minor && a.patch == b.patch;
}

// プロセスにロードされている libSDL2 の実体を dyld から数える。
//
// これが唯一「1プロセスに libSDL2 が2つ」を検出できる検査である。
// SDL_GetVersion() や IMG_Linked_Version() は、それぞれのライブラリが
// 自分の掴んでいる SDL について答えるだけなので、二重ロード状態でも
// 個別には矛盾のない値を返してしまい、検出できない。
void check_duplicate_sdl_images() {
#ifdef __APPLE__
  const std::uint32_t count = _dyld_image_count();
  int hits = 0;
  for (std::uint32_t i = 0; i < count; ++i) {
    const char* name = _dyld_get_image_name(i);
    if (name == nullptr) {
      continue;
    }
    if (std::strstr(name, "libSDL2-2.0") != nullptr) {
      ++hits;
      std::fprintf(stderr, "[sdl] ロード済み libSDL2 #%d: %s\n", hits, name);
    }
  }
  if (hits > 1) {
    std::fprintf(stderr,
                 "[sdl] ★致命的★ 1プロセスに libSDL2 が %d 個ロードされている。\n"
                 "[sdl]   実 sdl2 と sdl2-compat が混在している状態。\n"
                 "[sdl]   `brew unlink sdl2` で実 sdl2 を外し sdl2-compat に統一すること。\n"
                 "[sdl]   この状態では TTF 描画などで理由不明のクラッシュが起きうる。\n",
                 hits);
  } else if (hits == 0) {
    std::fprintf(stderr,
                 "[sdl] 注意: dyld から libSDL2 を検出できなかった（静的リンク？）。\n");
  }
#else
  std::fprintf(stderr, "[sdl] libSDL2 の二重ロード検査は macOS のみ対応。\n");
#endif
}

}  // namespace

void log_runtime_versions() {
  std::fprintf(stderr, "[sdl] ---- SDL ランタイム診断 ----\n");

  SDL_version compiled_sdl;
  SDL_version linked_sdl;
  SDL_VERSION(&compiled_sdl);
  SDL_GetVersion(&linked_sdl);

  std::fprintf(stderr, "[sdl] SDL       compile=%u.%u.%u link=%u.%u.%u\n",
               compiled_sdl.major, compiled_sdl.minor, compiled_sdl.patch,
               linked_sdl.major, linked_sdl.minor, linked_sdl.patch);

  const char* revision = SDL_GetRevision();
  std::fprintf(stderr, "[sdl] SDL       revision=%s\n",
               (revision != nullptr && revision[0] != '\0') ? revision : "(なし)");

  if (!same_version(compiled_sdl, linked_sdl)) {
    std::fprintf(stderr,
                 "[sdl] ⚠ 警告: SDL のコンパイル時とリンク時のバージョンが食い違う。\n");
  }

  SDL_version compiled_img;
  SDL_IMAGE_VERSION(&compiled_img);
  const SDL_version* linked_img = IMG_Linked_Version();
  std::fprintf(stderr, "[sdl] SDL_image compile=%u.%u.%u link=%u.%u.%u\n",
               compiled_img.major, compiled_img.minor, compiled_img.patch,
               linked_img->major, linked_img->minor, linked_img->patch);
  if (!same_version(compiled_img, *linked_img)) {
    std::fprintf(stderr,
                 "[sdl] ⚠ 警告: SDL_image のコンパイル時とリンク時のバージョンが食い違う。\n");
  }

  SDL_version compiled_ttf;
  SDL_TTF_VERSION(&compiled_ttf);
  const SDL_version* linked_ttf = TTF_Linked_Version();
  std::fprintf(stderr, "[sdl] SDL_ttf   compile=%u.%u.%u link=%u.%u.%u\n",
               compiled_ttf.major, compiled_ttf.minor, compiled_ttf.patch,
               linked_ttf->major, linked_ttf->minor, linked_ttf->patch);
  if (!same_version(compiled_ttf, *linked_ttf)) {
    std::fprintf(stderr,
                 "[sdl] ⚠ 警告: SDL_ttf のコンパイル時とリンク時のバージョンが食い違う。\n");
  }

  check_duplicate_sdl_images();
  std::fprintf(stderr, "[sdl] ---------------------------\n");
}

bool init_runtime(std::string& error) {
  if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_EVENTS) != 0) {
    error = std::string("SDL_Init に失敗: ") + SDL_GetError();
    return false;
  }

  constexpr int kImageFlags = IMG_INIT_JPG | IMG_INIT_PNG;
  const int image_ready = IMG_Init(kImageFlags);
  if ((image_ready & kImageFlags) != kImageFlags) {
    error = std::string("IMG_Init に失敗（JPG/PNG が使えない）: ") + IMG_GetError();
    SDL_Quit();
    return false;
  }

  if (TTF_Init() != 0) {
    error = std::string("TTF_Init に失敗: ") + TTF_GetError();
    IMG_Quit();
    SDL_Quit();
    return false;
  }

  return true;
}

void shutdown_runtime() noexcept {
  TTF_Quit();
  IMG_Quit();
  SDL_Quit();
}

}  // namespace ambient::render
