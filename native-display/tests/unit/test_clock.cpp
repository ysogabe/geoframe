// tests/unit/test_clock.cpp — 時刻とスリープ復帰（レーン 1A）
//
// ★次フェーズでここに書くもの:
//   - 跳躍を検知したあと、ポーリング予定 / トースト / クールダウンが
//     実時間に合わせて組み直されること（各消費者ができてから）
//   - 壁時計が巻き戻ったとき（NTP 補正）の扱い
//   - 単調時刻と壁時計を取り違えていないこと（型が違うのでコンパイルで落ちるはず）

#include <doctest.h>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"

using ambient::core::FakeClock;
using ambient::core::Millis;
using ambient::core::SteadyTime;
using ambient::core::TimeJump;
using ambient::core::TimeJumpDetector;
using ambient::core::WallTime;

TEST_CASE("初回 update は基準点を取るだけで跳躍を報告しない") {
  TimeJumpDetector detector;
  CHECK_FALSE(detector.update(SteadyTime{0}, WallTime{1700000000}).has_value());
}

TEST_CASE("通常の時間経過では跳躍を報告しない") {
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;

  (void)detector.update(clock.steadyNow(), clock.wallNow());
  for (int i = 0; i < 10; ++i) {
    clock.advance(Millis{1000});
    CHECK_FALSE(detector.update(clock.steadyNow(), clock.wallNow()).has_value());
  }
}

TEST_CASE("★macOS のスリープ復帰を検知する（単調時計が止まり、壁だけ進む）") {
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  // 8 時間スリープ。mach_absolute_time は止まっているので steady は進まない。
  clock.simulateSleep(Millis{8 * 60 * 60 * 1000});

  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());
  CHECK(jump->steadyDelta.value == 0);
  CHECK(jump->wallDelta.value == 8 * 60 * 60 * 1000);
  CHECK(jump->skew.value > 0);  // 正 = 単調時計が止まっていた
}

TEST_CASE("壁時計が飛んだ場合も検知する（NTP 初回同期）") {
  FakeClock clock{SteadyTime{0}, WallTime{0}};  // 未同期 = 1970 年
  TimeJumpDetector detector;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  clock.advance(Millis{100});
  clock.setWall(WallTime{1700000000});  // NTP 同期で一気に現在へ

  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());
  CHECK(jump->skew.value > 0);
}

TEST_CASE("壁時計の巻き戻しも検知する") {
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  clock.advance(Millis{100});
  clock.setWall(WallTime{1700000000 - 3600});  // 1 時間巻き戻る

  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());
  CHECK(jump->skew.value < 0);
}

TEST_CASE("しきい値未満のずれは報告しない（通常運転で誤検知しない）") {
  // ★WallTime は **秒**、SteadyTime / Millis は **ミリ秒**。単位を混ぜないこと。
  TimeJumpDetector detector{Millis{2000}};
  (void)detector.update(SteadyTime{0}, WallTime{1000});

  // 単調 +1000ms、壁 +1 秒 → skew 0
  CHECK_FALSE(detector.update(SteadyTime{1000}, WallTime{1001}).has_value());

  // 単調 +1000ms、壁 +2 秒 → skew 1000ms。
  // ★壁時計が秒精度である以上、この程度のずれは通常運転で普通に起きる。
  //   既定しきい値 2000ms がこれより大きいのはそのため。
  CHECK_FALSE(detector.update(SteadyTime{2000}, WallTime{1003}).has_value());
}

TEST_CASE("reset 後は次の update が初回扱いになる") {
  TimeJumpDetector detector;
  (void)detector.update(SteadyTime{0}, WallTime{0});
  detector.reset();
  CHECK_FALSE(detector.update(SteadyTime{999999}, WallTime{999999}).has_value());
}

TEST_CASE("時間の型: 差分と加算") {
  constexpr SteadyTime t0{1000};
  constexpr SteadyTime t1 = t0 + Millis{500};
  static_assert(t1.ms == 1500);
  static_assert((t1 - t0).value == 500);
  static_assert(t0 < t1);
  CHECK(true);
}

// ============================================================================
// 跳躍を検知したら何をするか ― ポリシー（レーン 1A が決定）
// ============================================================================
//
// ★決めたこと（詳細な理由は clock.hpp のコメント）:
//
//   skew > 0  スリープ復帰 / 壁が前へ飛んだ
//             → 経過時間の基準点を巻き戻す = 「その時間は実際に経過した」扱い。
//               クールダウンは明け、古いアラートは画面から消える。
//
//   skew < 0  壁が巻き戻った（NTP 補正 / 手動変更）
//             → **何もしない**。実時間が経過した証拠ではないので、
//               基準点を未来へずらすとクールダウンが延び、
//               本物の地震アラートを黙って握り潰す。
//
//   PendingCareRecord（2 秒の取り消し猶予）
//             → **意図的に何もしない**。不変条件が既に安全側に倒れているため。
//               詳細は test_care_pending.cpp の該当テスト。

TEST_CASE("★catchUp: 前へ飛んだぶんだけを「経過した時間」として拾う") {
  TimeJump forward{};
  forward.steadyDelta = Millis{0};  // スリープ中は単調時計が止まる
  forward.wallDelta = Millis{8 * 60 * 60 * 1000};
  forward.skew = forward.wallDelta - forward.steadyDelta;
  CHECK(ambient::core::catchUp(forward).value == 8 * 60 * 60 * 1000);

  // ★巻き戻しは 0（握り潰しを避けるため、経過時間を増やしも減らしもしない）。
  TimeJump backward{};
  backward.steadyDelta = Millis{1000};
  backward.wallDelta = Millis{-3600 * 1000};
  backward.skew = backward.wallDelta - backward.steadyDelta;
  CHECK(ambient::core::catchUp(backward).value == 0);
}

TEST_CASE("★rebaseForJump: 基準点を過去へずらすと経過時間が増える") {
  TimeJump jump{};
  jump.steadyDelta = Millis{0};
  jump.wallDelta = Millis{600000};  // 10 分
  jump.skew = jump.wallDelta - jump.steadyDelta;

  constexpr SteadyTime anchor{5000};
  const SteadyTime rebased = ambient::core::rebaseForJump(anchor, jump);
  CHECK(rebased.ms == 5000 - 600000);

  // 補正前は 0ms しか経っていないが、補正後は 10 分経ったことになる。
  CHECK((anchor - anchor).value == 0);
  CHECK((anchor - rebased).value == 600000);
}

TEST_CASE("★rebaseForJump: 巻き戻し（skew < 0）は no-op") {
  TimeJump backward{};
  backward.steadyDelta = Millis{1000};
  backward.wallDelta = Millis{-3600 * 1000};
  backward.skew = backward.wallDelta - backward.steadyDelta;

  constexpr SteadyTime anchor{5000};
  CHECK(ambient::core::rebaseForJump(anchor, backward) == anchor);
}

TEST_CASE("★スリープ復帰の一連の流れ（FakeClock::simulateSleep）") {
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  // 30 秒ごとのポーリング予定を立てた。
  SteadyTime nextPoll = clock.steadyNow() + ambient::core::constants::kPollInterval;

  clock.simulateSleep(Millis{8 * 60 * 60 * 1000});
  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());

  // 補正前: 単調時計が止まっていたので「まだ 30 秒経っていない」ように見える。
  CHECK(clock.steadyNow() < nextPoll);

  // 同じ rebaseForJump() をポーリング予定にも当てれば「即時 1 回」になる。
  nextPoll = ambient::core::rebaseForJump(nextPoll, *jump);
  CHECK(nextPoll <= clock.steadyNow());
}
