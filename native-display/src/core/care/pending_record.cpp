// pending_record.cpp
//
// ★実装済み。理由: これは「不変条件そのもの」であり、スタブにすると
//   契約が文章にしか存在しなくなる。leave() が 1 箇所であることと、
//   既定が確定であることは、コードで示さないと守られない。
//
// 送信キューへの積み方（QUEUE_SIZE=8、満杯なら epoch 最小を上書き）と
// リトライ規律は別モジュール。ここは「確定するか破棄するか」だけを決める。

#include "ambient/core/care/pending_record.hpp"

namespace ambient::core {

const char* toString(PendingExit reason) noexcept {
  // ★default 節を足さないこと。
  //   この switch が「PendingExit に値が足された」ことの検出器である
  //   （default が無いと -Wswitch が出る。詳細はヘッダの kPendingExitCount）。
  switch (reason) {
    case PendingExit::UndoHit:
      return "UndoHit";
    case PendingExit::ToastExpired:
      return "ToastExpired";
    case PendingExit::OtherCardTapped:
      return "OtherCardTapped";
    case PendingExit::NavigatedAway:
      return "NavigatedAway";
    case PendingExit::IdleTimeout:
      return "IdleTimeout";
    case PendingExit::AppShutdown:
      return "AppShutdown";
  }
  return "?";
}

std::optional<CommittedRecord> PendingCareRecord::begin(CareItem item, SteadyTime now,
                                                        WallTime wallNow) {
  // 既存の保留は「別カードのタップ」として **確定** する。破棄しない。
  std::optional<CommittedRecord> previous = leave(PendingExit::OtherCardTapped);

  active_ = true;
  item_ = item;
  startedAt_ = now;
  recordedAt_ = wallNow;
  lastOutcome_.reset();

  return previous;
}

Millis PendingCareRecord::remaining(SteadyTime now) const noexcept {
  if (!active_) {
    return Millis{0};
  }
  const Millis elapsed = now - startedAt_;
  if (elapsed.value >= grace_.value) {
    return Millis{0};
  }
  return grace_ - elapsed;
}

std::optional<CommittedRecord> PendingCareRecord::leave(PendingExit reason) {
  if (!active_) {
    return std::nullopt;  // 冪等。保留が無いときに呼んでも無害。
  }

  active_ = false;

  // ★不変条件はこの 1 行に集約されている。
  //   経路を switch で列挙しないこと。既定が「確定」であること。
  if (discardsPending(reason)) {
    lastOutcome_ = PendingOutcome::Discarded;
    return std::nullopt;
  }

  lastOutcome_ = PendingOutcome::Committed;
  return CommittedRecord{item_, recordedAt_};
}

std::optional<CommittedRecord> PendingCareRecord::tick(SteadyTime now) {
  if (!active_) {
    return std::nullopt;
  }
  if ((now - startedAt_).value < grace_.value) {
    return std::nullopt;
  }
  return leave(PendingExit::ToastExpired);
}

}  // namespace ambient::core
