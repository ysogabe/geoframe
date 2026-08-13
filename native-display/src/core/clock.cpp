// clock.cpp — TimeJumpDetector と FakeClock の実装。
//
// ★TimeJumpDetector は実装済み（跳躍検知は仕様が確定しており、
//   ここが空だとスリープ復帰のテストが書けないため）。
//   IClock の実機実装は Mac 側 (app / render 層) と Tab5 側が各自持つ。

#include "ambient/core/clock.hpp"

namespace ambient::core {

// ---- TimeJumpDetector ------------------------------------------------------

TimeJumpDetector::TimeJumpDetector(Millis threshold) noexcept : threshold_(threshold) {}

void TimeJumpDetector::reset() noexcept {
  primed_ = false;
  lastSteady_ = SteadyTime{};
  lastWall_ = WallTime{};
}

std::optional<TimeJump> TimeJumpDetector::update(SteadyTime steady, WallTime wall) noexcept {
  if (!primed_) {
    // 初回は基準点を取るだけ。ここで跳躍を返すと起動直後に必ず誤検知する。
    primed_ = true;
    lastSteady_ = steady;
    lastWall_ = wall;
    return std::nullopt;
  }

  TimeJump jump{};
  jump.steadyDelta = steady - lastSteady_;
  jump.wallDelta = wall - lastWall_;
  jump.skew = jump.wallDelta - jump.steadyDelta;

  lastSteady_ = steady;
  lastWall_ = wall;

  const std::int64_t magnitude = jump.skew.value < 0 ? -jump.skew.value : jump.skew.value;
  if (magnitude < threshold_.value) {
    return std::nullopt;
  }
  return jump;
}

// ---- FakeClock -------------------------------------------------------------

void FakeClock::advance(Millis d) noexcept {
  steady_ = steady_ + d;
  // 壁時計は秒精度。切り捨てで進める（ミリ秒未満の精度を偽らないため）。
  wall_.epochSeconds += d.value / 1000;
}

void FakeClock::simulateSleep(Millis wallElapsed) noexcept {
  // ★単調時刻は進めない。これが macOS のスリープ挙動そのもの。
  wall_.epochSeconds += wallElapsed.value / 1000;
}

}  // namespace ambient::core
