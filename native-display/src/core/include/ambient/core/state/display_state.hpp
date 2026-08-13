// ambient/core/state/display_state.hpp — 表示状態
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。

#pragma once

#include <cstdint>

namespace ambient::core {

// 画面が今なにを出しているか。
//
// ★RecordedAwaitingServer について（企画書に無いが必要と判明した状態）
//
// お世話記録を送信して **成功した直後** の状態である。
//
// 記録の状態色（Red / Yellow / Green）は **サーバ側が計算して返す**
// （hogo-meter の ScheduleStatus。Mac 側でしきい値計算を再実装してはいけない）。
// したがって記録の POST が 200 で返っても、**次の config poll（最大 30 秒後）まで
// 色は Red のまま** である。
//
//   ★「記録したら即 Green」は成立しない。
//
// これを Care と区別せずに扱うと、ユーザは「記録したのに赤いまま」を見る。
// この状態は「ローカルの記録時刻を経過時間表示に反映しつつ、色はサーバ待ち」
// を表現するために存在する。次の poll が届いたら Care / CareDue へ落ちる。
enum class DisplayStateKind : std::uint8_t {
  // 既定。写真が流れている。
  Photo = 0,
  // 環境情報カット（天気・週間予報・室内環境）。
  Env,
  // 通信断・データ陳腐化の告知。
  Degraded,
  // お世話カード（通常）。
  Care,
  // お世話の期限超過（Red / Yellow）。
  CareDue,
  // ★記録送信成功、サーバの状態確定待ち（上記）。
  RecordedAwaitingServer,
  // 地震アラート。
  Alert,
};

inline constexpr std::size_t kDisplayStateKindCount = 7;

[[nodiscard]] const char* toString(DisplayStateKind kind) noexcept;

}  // namespace ambient::core
