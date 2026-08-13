// ambient/core/state/state_machine.hpp — decide() の結果を保持する薄い層
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ★薄いこと。ここに優先順位のロジックを書かないこと。
//   優先順位は decide()（純粋関数）が全部持っている。
//   この層の責務は「前回の結果を覚えていて、変化したら遷移を報告する」だけ。
//   ロジックがこちらへ漏れると、テストが履歴依存になり決定性が失われる。

#pragma once

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/state/arbiter.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/display_state.hpp"

namespace ambient::core {

// 状態が変わった、という報告。
struct Transition {
  DisplayStateKind from{DisplayStateKind::Photo};
  DisplayStateKind to{DisplayStateKind::Photo};
  std::uint32_t fromPayloadId{0};
  std::uint32_t toPayloadId{0};
  SteadyTime at{};
};

class StateMachine {
 public:
  // 有効な claim が 1 つも無かったときに落ち着く状態。
  // ★decide() が nullopt を返すのは異常であり、ここはその安全網である。
  //   通常運転で到達したら、写真の claim source が claim を出していない。
  explicit StateMachine(DisplayStateKind fallback = DisplayStateKind::Degraded) noexcept;

  // claim を集めて decide() し、結果を適用する。
  //
  // ★source の登録順（= claims に積む順）は結果に影響する
  //   （同一優先度は先勝ち。arbiter.hpp の契約）。順序を安定させること。
  [[nodiscard]] std::optional<Transition> update(std::span<const Claim> claims,
                                                 SteadyTime now);

  [[nodiscard]] DisplayStateKind current() const noexcept { return current_; }
  [[nodiscard]] std::uint32_t currentPayloadId() const noexcept { return currentPayloadId_; }
  [[nodiscard]] SteadyTime enteredAt() const noexcept { return enteredAt_; }

  // 直近の遷移の履歴（デバッグ・テスト用）。上限付きのリング。
  // ★上限を設けているのは Tab5 の RAM 予算のため。無制限に積まない。
  static constexpr std::size_t kHistoryMax = 16;
  [[nodiscard]] const std::vector<Transition>& history() const noexcept { return history_; }

 private:
  DisplayStateKind fallback_;
  DisplayStateKind current_;
  std::uint32_t currentPayloadId_{0};
  SteadyTime enteredAt_{};
  std::vector<Transition> history_;
};

// ----------------------------------------------------------------------------
// 遷移トレース ― golden test の対象
// ----------------------------------------------------------------------------
//
// Phase 0 の完了条件に「同一 seed + 同一イベント列 → 同一の状態遷移トレース」がある。
// 後で golden test に使うので、**決定的な文字列** として取り出せる形にしてある。
//
// 形式（1 遷移 1 行、末尾に改行）:
//
//   <at_ms> <From>#<fromPayloadId> -> <To>#<toPayloadId>
//   例: 1000 Photo#0 -> Alert#1
//
// ★決定性のために守っていること:
//   - ロケール非依存（<format> も iostream も使わない。手書きの 10 進変換）
//   - 浮動小数点を含めない
//   - 状態名は toString() 由来なので、enum の **値** を並べ替えても
//     トレースは変わらない（名前で書く。数値で書くと並べ替えで golden が壊れる）
[[nodiscard]] std::string traceLine(const Transition& transition);

// 遷移列をまとめて 1 つの文字列にする（各行末に '\n'）。
[[nodiscard]] std::string formatTrace(std::span<const Transition> transitions);

// ----------------------------------------------------------------------------
// claim の収集ヘルパ
// ----------------------------------------------------------------------------

// 登録順に collect() を呼んで claims を組み立てる。
// ★out はクリアしてから積む。呼び出しごとに作り直すこと
//   （前 tick の claim が残るとレベルの意味が壊れる）。
void collectClaims(std::span<IClaimSource* const> sources, SteadyTime now,
                   std::vector<Claim>& out);

}  // namespace ambient::core
