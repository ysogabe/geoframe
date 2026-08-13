// ambient/core/model/presence.hpp — /api/presence のデータ型
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★night は 3 値 ― bool にしてはいけない
// ============================================================================
//
// /api/presence の `night` は `true` / `false` / **`null`** の 3 値を取る。
//
//   true  : 夜
//   false : 夜ではない
//   null  : **判定できない**（センサ未設定・データ未取得）
//
// bool に潰すと null が false（= 昼）になる。夜間に画面が煌々と光る。
// 「判定できない」と「昼である」は別の情報であり、既定の振る舞いも別でよい
// （例: 判定不能なら時刻ベースのフォールバックを使う）。

#pragma once

#include <optional>

namespace ambient::core {

struct PresenceSnapshot {
  // ★std::optional。nullopt が JSON の null に対応する。
  //   value_or(false) で潰すコードを書かないこと（null と false を混ぜることになる）。
  std::optional<bool> night;
};

}  // namespace ambient::core
