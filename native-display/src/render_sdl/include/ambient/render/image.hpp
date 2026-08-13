// 写真の読み込みと cover 表示。

#pragma once

#include <SDL.h>

#include <memory>
#include <string>

namespace ambient::render {

class Texture {
 public:
  // 失敗時は nullptr を返し、error にメッセージを入れる。
  [[nodiscard]] static std::unique_ptr<Texture> load(SDL_Renderer* renderer,
                                                     const std::string& path,
                                                     std::string& error);

  ~Texture();

  Texture(const Texture&) = delete;
  Texture& operator=(const Texture&) = delete;
  Texture(Texture&&) = delete;
  Texture& operator=(Texture&&) = delete;

  [[nodiscard]] SDL_Texture* raw() const noexcept { return texture_; }
  [[nodiscard]] int width() const noexcept { return width_; }
  [[nodiscard]] int height() const noexcept { return height_; }

 private:
  Texture() = default;

  SDL_Texture* texture_ = nullptr;
  int width_ = 0;
  int height_ = 0;
};

// cover 表示のために、元画像から切り出す矩形（srcrect）を計算する。
//
// cover = アスペクト比を保ったまま表示領域を埋め、はみ出す分はクロップする。
// contain（全体を収めて余白を作る）とは逆の挙動。
//
// 例: 2560x1707 (3:2) を 1280x720 (16:9) へ cover すると、
//     幅は使い切り、高さの上下が均等にクロップされる。
[[nodiscard]] SDL_Rect cover_source_rect(int source_width, int source_height,
                                         int dest_width, int dest_height) noexcept;

// 画像を論理領域 (0,0,dest_width,dest_height) 全体へ cover 表示する。
void draw_cover(SDL_Renderer* renderer, const Texture& texture,
                int dest_width, int dest_height);

}  // namespace ambient::render
