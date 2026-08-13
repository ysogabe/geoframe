#include "ambient/render/image.hpp"

#include <SDL_image.h>

#include <cstdio>

namespace ambient::render {

std::unique_ptr<Texture> Texture::load(SDL_Renderer* renderer, const std::string& path,
                                       std::string& error) {
  SDL_Surface* surface = IMG_Load(path.c_str());
  if (surface == nullptr) {
    error = "画像を読み込めなかった: " + path + "\n"
            "  SDL_image: " + IMG_GetError() + "\n"
            "  → fixture が未生成なら次を実行すること:\n"
            "      native-display/tools/make-sample-fixture.py";
    return nullptr;
  }

  const int width = surface->w;
  const int height = surface->h;

  SDL_Texture* texture = SDL_CreateTextureFromSurface(renderer, surface);
  SDL_FreeSurface(surface);

  if (texture == nullptr) {
    error = std::string("SDL_CreateTextureFromSurface に失敗: ") + SDL_GetError();
    return nullptr;
  }

  std::unique_ptr<Texture> handle(new Texture());
  handle->texture_ = texture;
  handle->width_ = width;
  handle->height_ = height;

  std::fprintf(stderr, "[image] 読み込み: %s (%dx%d)\n", path.c_str(), width, height);
  return handle;
}

Texture::~Texture() {
  if (texture_ != nullptr) {
    SDL_DestroyTexture(texture_);
  }
}

SDL_Rect cover_source_rect(int source_width, int source_height,
                           int dest_width, int dest_height) noexcept {
  if (source_width <= 0 || source_height <= 0 || dest_width <= 0 || dest_height <= 0) {
    return SDL_Rect{0, 0, source_width, source_height};
  }

  // 元画像のほうが横長なら左右を、縦長なら上下をクロップする。
  // 整数演算のまま比較するためにたすき掛けする（浮動小数の丸めを避ける）。
  const long long src_ratio = static_cast<long long>(source_width) * dest_height;
  const long long dst_ratio = static_cast<long long>(dest_width) * source_height;

  SDL_Rect rect{0, 0, source_width, source_height};

  if (src_ratio > dst_ratio) {
    // 元画像が相対的に横長 → 高さを使い切り、幅をクロップ。
    const int crop_width =
        static_cast<int>((static_cast<long long>(source_height) * dest_width) / dest_height);
    rect.w = crop_width;
    rect.x = (source_width - crop_width) / 2;
  } else if (src_ratio < dst_ratio) {
    // 元画像が相対的に縦長 → 幅を使い切り、高さをクロップ。
    const int crop_height =
        static_cast<int>((static_cast<long long>(source_width) * dest_height) / dest_width);
    rect.h = crop_height;
    rect.y = (source_height - crop_height) / 2;
  }
  // 比が一致する場合はクロップ不要（初期値のまま）。

  return rect;
}

void draw_cover(SDL_Renderer* renderer, const Texture& texture,
                int dest_width, int dest_height) {
  const SDL_Rect src =
      cover_source_rect(texture.width(), texture.height(), dest_width, dest_height);
  const SDL_Rect dst{0, 0, dest_width, dest_height};
  SDL_RenderCopy(renderer, texture.raw(), &src, &dst);
}

}  // namespace ambient::render
