// ambient_core — プラットフォーム非依存層のバージョン情報。
//
// 【この層の制約】CMakeLists.txt の先頭コメントを必ず読むこと。
// SDL ヘッダ / std::filesystem / std::thread / std::mutex / <format> は禁止。
// 依存してよいのは標準ライブラリのうち、ESP32-P4 (ESP-IDF) でも使えるものだけ。

#pragma once

namespace ambient::core {

// Core 層のバージョン文字列（例 "0.1.0"）。
[[nodiscard]] const char* version_string() noexcept;

// この Core がどのターゲットを想定して書かれているかの覚え書き。
// 起動ログに出して、Mac 版と Tab5 版のどちらを動かしているか一目で分かるようにする。
[[nodiscard]] const char* target_note() noexcept;

}  // namespace ambient::core
