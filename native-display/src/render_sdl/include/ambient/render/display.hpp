// ウィンドウとレンダラ。論理解像度・レターボックス・入力座標の逆変換を担う。

#pragma once

#include <SDL.h>

#include <memory>
#include <string>

namespace ambient::render {

// 論理座標系（1280x720 固定）上の点。
// ウィンドウをどう拡縮しても、UI のロジックはこの座標系だけを見る。
struct LogicalPoint {
  float x = 0.0F;
  float y = 0.0F;
};

struct DisplayConfig {
  int logical_width = 1280;
  int logical_height = 720;
  int window_width = 1280;
  int window_height = 720;
  std::string title = "GeoFrame Ambient Display";
};

class Display {
 public:
  // 失敗時は nullptr を返し、error にメッセージを入れる。
  [[nodiscard]] static std::unique_ptr<Display> create(const DisplayConfig& config,
                                                       std::string& error);

  ~Display();

  Display(const Display&) = delete;
  Display& operator=(const Display&) = delete;
  Display(Display&&) = delete;
  Display& operator=(Display&&) = delete;

  [[nodiscard]] SDL_Renderer* renderer() const noexcept { return renderer_; }
  [[nodiscard]] SDL_Window* window() const noexcept { return window_; }

  [[nodiscard]] int logical_width() const noexcept { return logical_width_; }
  [[nodiscard]] int logical_height() const noexcept { return logical_height_; }

  // ウィンドウ座標（SDL のイベントが返す座標）を論理座標へ変換する。
  //
  // 【契約】タッチ/マウス入力を扱う後続の担当者は必ずこの関数を通すこと。
  // レターボックスの黒帯ぶんのオフセット、論理解像度への拡縮、
  // macOS Retina の「ポイント vs ピクセル」の差を全部吸収する。
  // 黒帯上をクリックした場合は論理解像度の範囲外の値（負値や 1280 超）が返るので、
  // 呼び出し側で範囲チェックすること。
  //
  // Tab5 移植時はこの関数の中身がタッチパネルのキャリブレーションに置き換わる。
  // 呼び出し側のコードは変えなくてよい、というのがこの seam の狙い。
  [[nodiscard]] LogicalPoint window_to_logical(int window_x, int window_y) const noexcept;

  // 現在のレンダーターゲットの内容をファイルへ保存する。
  //
  // 【重要】必ず描画後・SDL_RenderPresent の前に呼ぶこと。
  // present 後の読み出しはバックバッファが未定義になり黒画像になる。
  //
  // 拡張子が .png なら PNG、それ以外は BMP で保存する。
  // 保存される画像はレンダラの「出力ピクセルサイズ」なので、Retina では
  // 論理解像度の 2 倍（例 2560x1440）になり、レターボックスの黒帯も含まれる。
  [[nodiscard]] bool save_screenshot(const std::string& path, std::string& error) const;

 private:
  Display() = default;

  SDL_Window* window_ = nullptr;
  SDL_Renderer* renderer_ = nullptr;
  int logical_width_ = 0;
  int logical_height_ = 0;
};

}  // namespace ambient::render
