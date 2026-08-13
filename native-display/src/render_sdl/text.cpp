#include "ambient/render/text.hpp"

#include <cstdio>

namespace ambient::render {
namespace {

// テキスト1本をテクスチャ化して描く内部ヘルパ。
// 影と本体で 2 回呼ばれるので、色だけ差し替えられる形にしてある。
bool blit_text(SDL_Renderer* renderer, TTF_Font* font, const std::string& utf8_text,
               SDL_Color color, int x, int y, int* out_width) {
  SDL_Surface* surface = TTF_RenderUTF8_Blended(font, utf8_text.c_str(), color);
  if (surface == nullptr) {
    std::fprintf(stderr, "[text] TTF_RenderUTF8_Blended に失敗: %s\n", TTF_GetError());
    return false;
  }

  SDL_Texture* texture = SDL_CreateTextureFromSurface(renderer, surface);
  const int width = surface->w;
  const int height = surface->h;
  SDL_FreeSurface(surface);

  if (texture == nullptr) {
    std::fprintf(stderr, "[text] SDL_CreateTextureFromSurface に失敗: %s\n", SDL_GetError());
    return false;
  }

  SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_BLEND);
  SDL_SetTextureAlphaMod(texture, color.a);

  const SDL_Rect dst{x, y, width, height};
  SDL_RenderCopy(renderer, texture, nullptr, &dst);
  SDL_DestroyTexture(texture);

  if (out_width != nullptr) {
    *out_width = width;
  }
  return true;
}

}  // namespace

std::unique_ptr<Font> Font::open(const FontSpec& spec, std::string& error) {
  // face index を明示して開く。.ttc 差し替えのための contract。
  TTF_Font* font = TTF_OpenFontIndex(spec.path.c_str(), spec.point_size, spec.face_index);
  if (font == nullptr) {
    error = "フォントを開けなかった: " + spec.path + "\n"
            "  face index = " + std::to_string(spec.face_index) +
            ", size = " + std::to_string(spec.point_size) + "\n"
            "  SDL_ttf: " + TTF_GetError() + "\n"
            "  → フォントが未取得なら次を実行すること:\n"
            "      native-display/tools/fetch-fonts.sh";
    return nullptr;
  }

  TTF_SetFontHinting(font, TTF_HINTING_LIGHT);
  if (spec.synthetic_bold) {
    TTF_SetFontStyle(font, TTF_STYLE_BOLD);
  }

  std::unique_ptr<Font> handle(new Font());
  handle->font_ = font;

  const char* family = TTF_FontFaceFamilyName(font);
  const char* style = TTF_FontFaceStyleName(font);
  std::fprintf(stderr, "[text] フォント読み込み: %s (face=%ld, %dpt) family=%s style=%s faces=%ld\n",
               spec.path.c_str(), spec.face_index, spec.point_size,
               family != nullptr ? family : "?",
               style != nullptr ? style : "?",
               TTF_FontFaces(font));
  return handle;
}

Font::~Font() {
  if (font_ != nullptr) {
    TTF_CloseFont(font_);
  }
}

int Font::line_height() const noexcept {
  return TTF_FontLineSkip(font_);
}

int draw_text(SDL_Renderer* renderer, const Font& font, const std::string& utf8_text,
              int x, int y, const TextStyle& style, TextAlign align) {
  if (utf8_text.empty()) {
    return 0;
  }

  TTF_Font* raw = font.raw();

  int text_w = 0;
  int text_h = 0;
  if (TTF_SizeUTF8(raw, utf8_text.c_str(), &text_w, &text_h) != 0) {
    std::fprintf(stderr, "[text] TTF_SizeUTF8 に失敗: %s\n", TTF_GetError());
    return 0;
  }

  const int draw_x = (align == TextAlign::kRight) ? (x - text_w) : x;

  if (style.drop_shadow) {
    blit_text(renderer, raw, utf8_text, style.shadow_color,
              draw_x + style.shadow_offset, y + style.shadow_offset, nullptr);
  }

  int drawn_width = 0;
  if (!blit_text(renderer, raw, utf8_text, style.color, draw_x, y, &drawn_width)) {
    return 0;
  }
  return drawn_width;
}

}  // namespace ambient::render
