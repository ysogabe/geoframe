// ambient_core の唯一の実装ファイル（現時点）。
//
// 【重要】このファイルおよび src/core/ 配下は標準ライブラリのみに依存すること。
// SDL を include するとビルドが落ちる（ambient_core の include path に
// SDL のパスを入れていないため）。それが正しい状態である。
// 詳細は CMakeLists.txt の先頭コメントを参照。

#include "ambient/core/version.hpp"

namespace ambient::core {

const char* version_string() noexcept {
  return "0.1.0";
}

const char* target_note() noexcept {
  return "platform-independent core (reference host: macOS/SDL2, "
         "final target: M5Stack Tab5 / ESP32-P4)";
}

}  // namespace ambient::core
