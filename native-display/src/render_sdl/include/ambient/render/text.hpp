// 日本語テキスト描画。

#pragma once

#include <SDL.h>
#include <SDL_ttf.h>

#include <memory>
#include <string>

namespace ambient::render {

// フォントの指定。
//
// 【契約】フォントのパスと face index は必ず設定値として外に出すこと。
// 将来 .ttc（TrueType Collection: 1ファイルに複数 face を含む形式）へ
// 差し替える可能性があるため、TTF_OpenFontIndex(path, size, face_index) を使う。
// Tab5 へ移植する際にフォントを差し替えるための contract である。
// ハードコードした TTF_OpenFont に戻さないこと。
struct FontSpec {
  std::string path;
  int point_size = 28;
  // .ttf は face 0 のみ。.ttc では 0 以外を指定して別 face を選ぶ。
  long face_index = 0;
  // 合成ボールド（SDL_ttf 経由で FreeType の TTF_STYLE_BOLD を掛ける）。
  //
  // 【経緯】以前は NotoSansJP.ttf が可変フォント（wght 軸）で、SDL_ttf 2.24 は
  // 可変軸を指定する API を持たないため、既定インスタンスの Thin が選ばれて
  // しまっていた。Thin は写真の上に重ねると細すぎて読めず、その場しのぎとして
  // ここで合成ボールドを掛けていた。
  //
  // 今は tools/fetch-fonts.sh が最初から静的な Medium インスタンス（wght 軸を
  // 持たない通常の TrueType）を取得するので、この問題は起きない。合成ボールドは
  // 字形が崩れる副作用があるため、通常は false のままにしておくこと。
  // 将来さらに太いウェイトが要る場面向けに指定自体は残してある。
  bool synthetic_bold = false;
};

class Font {
 public:
  // 失敗時は nullptr を返し、error に「何が悪いか」と「どう直すか」を入れる。
  [[nodiscard]] static std::unique_ptr<Font> open(const FontSpec& spec, std::string& error);

  ~Font();

  Font(const Font&) = delete;
  Font& operator=(const Font&) = delete;
  Font(Font&&) = delete;
  Font& operator=(Font&&) = delete;

  [[nodiscard]] TTF_Font* raw() const noexcept { return font_; }
  [[nodiscard]] int line_height() const noexcept;

 private:
  Font() = default;

  TTF_Font* font_ = nullptr;
};

struct TextStyle {
  SDL_Color color{255, 255, 255, 255};
  // 写真の上に重ねるので、可読性のために影を落とせるようにしておく。
  bool drop_shadow = true;
  SDL_Color shadow_color{0, 0, 0, 180};
  int shadow_offset = 2;
};

enum class TextAlign { kLeft, kRight };

// UTF-8 文字列を論理座標 (x, y) に描画する。y は行の上端。
// align が kRight のとき x は右端として扱う。
// 描画したテキストの幅を返す（失敗時は 0）。
int draw_text(SDL_Renderer* renderer, const Font& font, const std::string& utf8_text,
              int x, int y, const TextStyle& style, TextAlign align = TextAlign::kLeft);

}  // namespace ambient::render
