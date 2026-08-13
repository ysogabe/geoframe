// state_machine.cpp
//
// 薄い層なので実装済み。ここに優先順位のロジックを足さないこと（arbiter が持つ）。

#include "ambient/core/state/state_machine.hpp"

#include <cstddef>

namespace ambient::core {
namespace {

// 符号なし 10 進変換。★<format> も iostream も使わない
// （コードサイズと、ロケールによるトレースの揺れを避けるため）。
void appendDecimal(std::string& out, std::uint64_t value) {
  if (value == 0) {
    out.push_back('0');
    return;
  }
  char buffer[20];  // uint64 の 10 進最大桁数
  std::size_t length = 0;
  while (value != 0U) {
    buffer[length++] = static_cast<char>('0' + static_cast<char>(value % 10U));
    value /= 10U;
  }
  while (length != 0U) {
    out.push_back(buffer[--length]);
  }
}

void appendSigned(std::string& out, std::int64_t value) {
  auto magnitude = static_cast<std::uint64_t>(value);
  if (value < 0) {
    out.push_back('-');
    magnitude = ~magnitude + 1U;  // INT64_MIN でも UB にならない絶対値
  }
  appendDecimal(out, magnitude);
}

}  // namespace

StateMachine::StateMachine(DisplayStateKind fallback) noexcept
    : fallback_(fallback), current_(fallback) {
  history_.reserve(kHistoryMax);
}

std::optional<Transition> StateMachine::update(std::span<const Claim> claims, SteadyTime now) {
  const std::optional<Decision> decision = decide(claims, now);

  const DisplayStateKind nextKind = decision.has_value() ? decision->kind : fallback_;
  const std::uint32_t nextPayload = decision.has_value() ? decision->payloadId : 0U;

  // ★kind だけでなく payloadId も見る。
  //   地震 A → 地震 B は kind が Alert のままだが、内容は別物なので遷移させる。
  if (nextKind == current_ && nextPayload == currentPayloadId_) {
    return std::nullopt;
  }

  const Transition transition{current_, nextKind, currentPayloadId_, nextPayload, now};

  current_ = nextKind;
  currentPayloadId_ = nextPayload;
  enteredAt_ = now;

  if (history_.size() >= kHistoryMax) {
    history_.erase(history_.begin());
  }
  history_.push_back(transition);

  return transition;
}

std::string traceLine(const Transition& transition) {
  std::string line;
  appendSigned(line, transition.at.ms);
  line.push_back(' ');
  line.append(toString(transition.from));
  line.push_back('#');
  appendDecimal(line, transition.fromPayloadId);
  line.append(" -> ");
  line.append(toString(transition.to));
  line.push_back('#');
  appendDecimal(line, transition.toPayloadId);
  return line;
}

std::string formatTrace(std::span<const Transition> transitions) {
  std::string out;
  for (const Transition& transition : transitions) {
    out.append(traceLine(transition));
    out.push_back('\n');
  }
  return out;
}

void collectClaims(std::span<IClaimSource* const> sources, SteadyTime now,
                   std::vector<Claim>& out) {
  out.clear();
  for (IClaimSource* const source : sources) {
    if (source != nullptr) {
      source->collect(now, out);
    }
  }
}

}  // namespace ambient::core
