// arbiter.cpp
//
// decide() は実装済み。理由: これは 20 行の純粋関数であり、
// ここが空だと「同一優先度は先勝ち」「無効 claim は除外」という
// 選択規則そのものをテストで固定できない。規則こそが契約である。
// claim を作る側（各 IClaimSource）の本実装はレーン 1A。

#include "ambient/core/state/arbiter.hpp"

namespace ambient::core {

const char* toString(ClaimPriority priority) noexcept {
  switch (priority) {
    case ClaimPriority::Photo:
      return "Photo";
    case ClaimPriority::Env:
      return "Env";
    case ClaimPriority::Degraded:
      return "Degraded";
    case ClaimPriority::Care:
      return "Care";
    case ClaimPriority::CareDue:
      return "CareDue";
    case ClaimPriority::RecordedAwaitingServer:
      return "RecordedAwaitingServer";
    case ClaimPriority::Alert:
      return "Alert";
  }
  return "?";
}

const char* toString(DisplayStateKind kind) noexcept {
  switch (kind) {
    case DisplayStateKind::Photo:
      return "Photo";
    case DisplayStateKind::Env:
      return "Env";
    case DisplayStateKind::Degraded:
      return "Degraded";
    case DisplayStateKind::Care:
      return "Care";
    case DisplayStateKind::CareDue:
      return "CareDue";
    case DisplayStateKind::RecordedAwaitingServer:
      return "RecordedAwaitingServer";
    case DisplayStateKind::Alert:
      return "Alert";
  }
  return "?";
}

std::optional<Decision> decide(std::span<const Claim> claims, SteadyTime now) noexcept {
  const Claim* winner = nullptr;

  for (const Claim& claim : claims) {
    if (!claim.activeAt(now)) {
      continue;
    }
    // ★厳密な > で比較する。>= にすると同一優先度で後勝ちになり、
    //   「span 内で先に現れたものが勝つ」という契約が壊れる。
    if (winner == nullptr || claim.priority > winner->priority) {
      winner = &claim;
    }
  }

  if (winner == nullptr) {
    return std::nullopt;
  }

  return Decision{winner->kind, winner->priority, winner->sourceId, winner->payloadId};
}

}  // namespace ambient::core
