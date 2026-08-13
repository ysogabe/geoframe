#include "ambient/render/display.hpp"

#include <SDL_image.h>

#include <cstdio>

namespace ambient::render {
namespace {

bool ends_with_ignore_case(const std::string& text, const std::string& suffix) {
  if (text.size() < suffix.size()) {
    return false;
  }
  const std::size_t offset = text.size() - suffix.size();
  for (std::size_t i = 0; i < suffix.size(); ++i) {
    const unsigned char a = static_cast<unsigned char>(text[offset + i]);
    const unsigned char b = static_cast<unsigned char>(suffix[i]);
    if (SDL_tolower(a) != SDL_tolower(b)) {
      return false;
    }
  }
  return true;
}

}  // namespace

std::unique_ptr<Display> Display::create(const DisplayConfig& config, std::string& error) {
  // HiDPI を有効にする。macOS Retina では出力バッファが 2 倍のピクセル数になるが、
  // SDL_RenderSetLogicalSize がその差も吸収するので、描画コードは常に
  // 1280x720 の論理座標だけを見ればよい。
  const Uint32 window_flags = SDL_WINDOW_SHOWN | SDL_WINDOW_RESIZABLE |
                              SDL_WINDOW_ALLOW_HIGHDPI;

  SDL_Window* window = SDL_CreateWindow(config.title.c_str(),
                                        SDL_WINDOWPOS_CENTERED,
                                        SDL_WINDOWPOS_CENTERED,
                                        config.window_width,
                                        config.window_height,
                                        window_flags);
  if (window == nullptr) {
    error = std::string("SDL_CreateWindow に失敗: ") + SDL_GetError();
    return nullptr;
  }

  SDL_Renderer* renderer = SDL_CreateRenderer(
      window, -1, SDL_RENDERER_ACCELERATED | SDL_RENDERER_PRESENTVSYNC);
  if (renderer == nullptr) {
    error = std::string("SDL_CreateRenderer に失敗: ") + SDL_GetError();
    SDL_DestroyWindow(window);
    return nullptr;
  }

  // 論理解像度の設定。これでウィンドウを拡縮してもアスペクト比が保たれ、
  // 余った領域には自動でレターボックス（黒帯）が入る。
  if (SDL_RenderSetLogicalSize(renderer, config.logical_width, config.logical_height) != 0) {
    error = std::string("SDL_RenderSetLogicalSize に失敗: ") + SDL_GetError();
    SDL_DestroyRenderer(renderer);
    SDL_DestroyWindow(window);
    return nullptr;
  }

  // 拡縮時の品質。写真を cover 表示するため線形補間にする。
  SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "linear");
  SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);

  SDL_RendererInfo info;
  if (SDL_GetRendererInfo(renderer, &info) == 0) {
    std::fprintf(stderr, "[display] レンダラ: %s\n",
                 info.name != nullptr ? info.name : "(不明)");
  }

  int output_w = 0;
  int output_h = 0;
  SDL_GetRendererOutputSize(renderer, &output_w, &output_h);
  std::fprintf(stderr,
               "[display] 論理解像度 %dx%d / ウィンドウ %dx%d(pt) / 出力 %dx%d(px)\n",
               config.logical_width, config.logical_height,
               config.window_width, config.window_height,
               output_w, output_h);

  // make_unique はコンストラクタが private なので使えない。
  std::unique_ptr<Display> display(new Display());
  display->window_ = window;
  display->renderer_ = renderer;
  display->logical_width_ = config.logical_width;
  display->logical_height_ = config.logical_height;
  return display;
}

Display::~Display() {
  if (renderer_ != nullptr) {
    SDL_DestroyRenderer(renderer_);
  }
  if (window_ != nullptr) {
    SDL_DestroyWindow(window_);
  }
}

LogicalPoint Display::window_to_logical(int window_x, int window_y) const noexcept {
  LogicalPoint point;
  // SDL 2.0.18 以降が提供する公式の逆変換。自前で計算しないこと。
  // 論理サイズのスケール・ビューポートのオフセット（レターボックス）・
  // HiDPI のポイント/ピクセル比を全部まとめて処理してくれる。
  SDL_RenderWindowToLogical(renderer_, window_x, window_y, &point.x, &point.y);
  return point;
}

bool Display::save_screenshot(const std::string& path, std::string& error) const {
  int width = 0;
  int height = 0;
  if (SDL_GetRendererOutputSize(renderer_, &width, &height) != 0) {
    error = std::string("SDL_GetRendererOutputSize に失敗: ") + SDL_GetError();
    return false;
  }

  SDL_Surface* surface =
      SDL_CreateRGBSurfaceWithFormat(0, width, height, 32, SDL_PIXELFORMAT_ARGB8888);
  if (surface == nullptr) {
    error = std::string("スクリーンショット用サーフェス確保に失敗: ") + SDL_GetError();
    return false;
  }

  // 【落とし穴】SDL_RenderReadPixels に rect=nullptr を渡すと、レンダーターゲット
  // 全体ではなく「現在のビューポート」が読まれる。論理解像度を設定していると
  // ビューポートはレターボックスの内側だけなので、出力サイズぶん確保した
  // サーフェスの上部にだけ絵が入り、残りが黒帯ではなく未初期化領域になる。
  //
  // そこで一時的に論理解像度を解除してビューポートを出力全域へ戻し、
  // 読み終わったら元に戻す。これで実際に画面に見えているとおり
  // （黒帯を含む全体）が保存される。描画済みのバックバッファは
  // 論理解像度の変更では消えないので、絵はそのまま残る。
  int saved_logical_w = 0;
  int saved_logical_h = 0;
  SDL_RenderGetLogicalSize(renderer_, &saved_logical_w, &saved_logical_h);
  const bool had_logical_size = (saved_logical_w > 0 && saved_logical_h > 0);
  if (had_logical_size) {
    SDL_RenderSetLogicalSize(renderer_, 0, 0);
  }

  const int read_result = SDL_RenderReadPixels(renderer_, nullptr, SDL_PIXELFORMAT_ARGB8888,
                                               surface->pixels, surface->pitch);

  if (had_logical_size) {
    SDL_RenderSetLogicalSize(renderer_, saved_logical_w, saved_logical_h);
  }

  if (read_result != 0) {
    error = std::string("SDL_RenderReadPixels に失敗: ") + SDL_GetError();
    SDL_FreeSurface(surface);
    return false;
  }

  bool ok = false;
  if (ends_with_ignore_case(path, ".png")) {
    ok = (IMG_SavePNG(surface, path.c_str()) == 0);
    if (!ok) {
      error = std::string("IMG_SavePNG に失敗: ") + IMG_GetError();
    }
  } else {
    ok = (SDL_SaveBMP(surface, path.c_str()) == 0);
    if (!ok) {
      error = std::string("SDL_SaveBMP に失敗: ") + SDL_GetError();
    }
  }

  SDL_FreeSurface(surface);

  if (ok) {
    std::fprintf(stderr, "[display] スクリーンショット保存: %s (%dx%d px)\n",
                 path.c_str(), width, height);
  }
  return ok;
}

}  // namespace ambient::render
