// tests/unit/test_state_machine.cpp — 遷移トレース（レーン 1A）
//
// ############################################################################
// # Phase 0 の完了条件:
// #   「同一 seed + 同一イベント列 → 同一の状態遷移トレース」
// #
// # ここではその **トレース側** を固定する（乱数側は test_rng.cpp / 1B）。
// # トレースは後で golden test に使うので、決定的な文字列でなければならない。
// ############################################################################
//
// ★決定性のために守っていること:
//   - 時刻はすべて SteadyTime 由来。実時刻を読まない
//   - トレースに浮動小数点を含めない
//   - 状態名は toString() 由来。enum の **値** を並べ替えてもトレースは変わらない
//     （数値で書くと、優先度の並べ替えで golden が意味も無く壊れる）

#include <doctest.h>

#include <cstdint>
#include <string>
#include <vector>

#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/state_machine.hpp"

using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::collectClaims;
using ambient::core::DisplayStateKind;
using ambient::core::formatTrace;
using ambient::core::IClaimSource;
using ambient::core::StateMachine;
using ambient::core::SteadyTime;
using ambient::core::traceLine;
using ambient::core::Transition;

namespace {

Claim makeClaim(ClaimPriority priority, DisplayStateKind kind, std::uint32_t sourceId = 0,
                std::uint32_t payloadId = 0) {
  Claim c{};
  c.priority = priority;
  c.kind = kind;
  c.sourceId = sourceId;
  c.payloadId = payloadId;
  return c;
}

// 条件が真であるあいだ claim を出し続けるだけの source（レベルトリガ）。
class ToggleSource final : public IClaimSource {
 public:
  ToggleSource(ClaimPriority priority, DisplayStateKind kind, std::uint32_t sourceId)
      : priority_(priority), kind_(kind), sourceId_(sourceId) {}

  void collect(SteadyTime /*now*/, std::vector<Claim>& out) const override {
    if (!active_) {
      return;
    }
    out.push_back(makeClaim(priority_, kind_, sourceId_, payloadId_));
  }

  void setActive(bool active) noexcept { active_ = active; }
  void setPayloadId(std::uint32_t id) noexcept { payloadId_ = id; }

 private:
  ClaimPriority priority_;
  DisplayStateKind kind_;
  std::uint32_t sourceId_;
  std::uint32_t payloadId_{0};
  bool active_{true};
};

}  // namespace

TEST_CASE("トレース行の形式") {
  Transition t{};
  t.from = DisplayStateKind::Photo;
  t.to = DisplayStateKind::Alert;
  t.fromPayloadId = 0;
  t.toPayloadId = 42;
  t.at = SteadyTime{1500};

  CHECK(traceLine(t) == "1500 Photo#0 -> Alert#42");
}

TEST_CASE("トレース行は負の単調時刻でも壊れない") {
  // SteadyTime の起点は任意なので、負の値を渡されうる。
  Transition t{};
  t.from = DisplayStateKind::Env;
  t.to = DisplayStateKind::Photo;
  t.at = SteadyTime{-250};
  CHECK(traceLine(t) == "-250 Env#0 -> Photo#0");
}

TEST_CASE("★同一のイベント列からは同一のトレースが出る（決定性）") {
  // 同じ手順を 2 回まわして、トレース文字列が完全一致することを見る。
  const auto run = [] {
    ToggleSource photo{ClaimPriority::Photo, DisplayStateKind::Photo, 1};
    ToggleSource careDue{ClaimPriority::CareDue, DisplayStateKind::CareDue, 2};
    ToggleSource alert{ClaimPriority::Alert, DisplayStateKind::Alert, 3};
    careDue.setActive(false);
    alert.setActive(false);

    // ★登録順は結果に影響する（同一優先度は先勝ち）。順序を安定させること。
    IClaimSource* sources[] = {&photo, &careDue, &alert};

    StateMachine machine;
    std::vector<Claim> claims;
    std::vector<Transition> transitions;

    const auto tick = [&](std::int64_t ms) {
      collectClaims(sources, SteadyTime{ms}, claims);
      if (const auto t = machine.update(claims, SteadyTime{ms})) {
        transitions.push_back(*t);
      }
    };

    tick(0);  // Photo
    careDue.setActive(true);
    tick(1000);  // -> CareDue
    alert.setActive(true);
    alert.setPayloadId(7);
    tick(2000);  // -> Alert
    alert.setPayloadId(8);
    tick(3000);  // Alert のまま payloadId だけ変わる
    alert.setActive(false);
    tick(4000);  // ★レベルなので CareDue が自動で戻る
    careDue.setActive(false);
    tick(5000);  // -> Photo
    return formatTrace(transitions);
  };

  const std::string first = run();
  const std::string second = run();
  CHECK(first == second);

  // ★golden: 期待するトレースそのもの。
  //   「Alert が明けたら CareDue が出る」が再配達の仕組み無しで成立していること、
  //   payloadId だけの変化も遷移として記録されることが、この 1 文字列に写る。
  CHECK(first ==
        "0 Degraded#0 -> Photo#0\n"
        "1000 Photo#0 -> CareDue#0\n"
        "2000 CareDue#0 -> Alert#7\n"
        "3000 Alert#7 -> Alert#8\n"
        "4000 Alert#8 -> CareDue#0\n"
        "5000 CareDue#0 -> Photo#0\n");
}

TEST_CASE("★操作中の保留がトレースに現れる（地震で遷移しない）") {
  ToggleSource photo{ClaimPriority::Photo, DisplayStateKind::Photo, 1};
  ToggleSource alert{ClaimPriority::Alert, DisplayStateKind::Alert, 2};
  ToggleSource recording{ClaimPriority::RecordedAwaitingServer,
                         DisplayStateKind::RecordedAwaitingServer, 3};
  alert.setActive(false);
  recording.setActive(false);

  IClaimSource* sources[] = {&photo, &alert, &recording};

  StateMachine machine;
  std::vector<Claim> claims;
  std::vector<Transition> transitions;
  const auto tick = [&](std::int64_t ms) {
    collectClaims(sources, SteadyTime{ms}, claims);
    if (const auto t = machine.update(claims, SteadyTime{ms})) {
      transitions.push_back(*t);
    }
  };

  tick(0);  // Photo
  // ユーザがお世話カードを押した。取り消し猶予に入る。
  recording.setActive(true);
  tick(100);
  // 猶予のさなかに地震が来た。★遷移してはいけない。
  alert.setActive(true);
  alert.setPayloadId(9);
  tick(200);
  tick(1000);
  // 猶予が明けた。★抑止解除のコードはどこにも無いのに Alert が出る。
  recording.setActive(false);
  tick(2100);

  CHECK(formatTrace(transitions) ==
        "0 Degraded#0 -> Photo#0\n"
        "100 Photo#0 -> RecordedAwaitingServer#0\n"
        "2100 RecordedAwaitingServer#0 -> Alert#9\n");
}

TEST_CASE("collectClaims は毎 tick 作り直す（前 tick の claim が残らない）") {
  ToggleSource photo{ClaimPriority::Photo, DisplayStateKind::Photo, 1};
  IClaimSource* sources[] = {&photo};

  std::vector<Claim> claims;
  collectClaims(sources, SteadyTime{0}, claims);
  CHECK(claims.size() == 1);

  // 2 回目。out はクリアされてから積まれる。積み上がったらレベルの意味が壊れる。
  collectClaims(sources, SteadyTime{1000}, claims);
  CHECK(claims.size() == 1);

  photo.setActive(false);
  collectClaims(sources, SteadyTime{2000}, claims);
  CHECK(claims.empty());
}

TEST_CASE("nullptr の source を含んでいても落ちない") {
  ToggleSource photo{ClaimPriority::Photo, DisplayStateKind::Photo, 1};
  IClaimSource* sources[] = {nullptr, &photo, nullptr};

  std::vector<Claim> claims;
  collectClaims(sources, SteadyTime{0}, claims);
  CHECK(claims.size() == 1);
}

TEST_CASE("履歴からもトレースが取れる（上限つき）") {
  StateMachine machine;
  const std::vector<Claim> a{makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};
  const std::vector<Claim> b{makeClaim(ClaimPriority::Env, DisplayStateKind::Env)};

  for (std::int64_t i = 0; i < 5; ++i) {
    (void)machine.update((i % 2 == 0) ? a : b, SteadyTime{i * 1000});
  }

  CHECK(formatTrace(machine.history()) ==
        "0 Degraded#0 -> Photo#0\n"
        "1000 Photo#0 -> Env#0\n"
        "2000 Env#0 -> Photo#0\n"
        "3000 Photo#0 -> Env#0\n"
        "4000 Env#0 -> Photo#0\n");
}
