// sync_policy.cpp — ambient/core/care/sync_policy.hpp の実装
//
// ★出典と「なぜこうなっているか」はヘッダに書いてある。ここは実装だけ。
//   値を変えたくなったら constants.hpp と tests/contract/HOGO_CONTRACT.md を先に読むこと。

#include "ambient/core/care/sync_policy.hpp"

namespace ambient::core {

const char* toString(SyncActionKind kind) noexcept {
  // ★default 節を足さないこと。値を足したときに -Wswitch で気づくための switch。
  switch (kind) {
    case SyncActionKind::None:
      return "None";
    case SyncActionKind::SendRecord:
      return "SendRecord";
    case SyncActionKind::PollConfig:
      return "PollConfig";
  }
  return "?";
}

EnqueueOutcome SyncPolicy::enqueueRecord(CareItem item, WallTime recordedAt,
                                         SteadyTime now) noexcept {
  EnqueueOutcome outcome;

  // まず空きを探す（上流 server_sync.cpp:82-91）。
  for (std::size_t i = 0; i < kQueueSize; ++i) {
    if (!queue_[i].active) {
      queue_[i].active = true;
      queue_[i].item = item;
      queue_[i].recordedAt = recordedAt;
      queue_[i].retriesLeft = config_.maxAttempts;
      queue_[i].nextAttemptAt = now;  // 次の nextAction() で即時送信対象になる
      outcome.index = i;
      return outcome;
    }
  }

  // 満杯: epoch が最小（= 最も古い）要素を上書きする（上流 server_sync.cpp:97-112）。
  // ★同値なら添字の小さい方が残る。上流が strict `<` で走査するのと同じ。
  std::size_t oldest = 0;
  for (std::size_t i = 1; i < kQueueSize; ++i) {
    if (queue_[i].recordedAt < queue_[oldest].recordedAt) {
      oldest = i;
    }
  }

  outcome.evicted = true;
  outcome.index = oldest;
  outcome.evictedItem = queue_[oldest].item;
  outcome.evictedAt = queue_[oldest].recordedAt;
  ++evicted_;

  queue_[oldest].active = true;
  queue_[oldest].item = item;
  queue_[oldest].recordedAt = recordedAt;
  queue_[oldest].retriesLeft = config_.maxAttempts;
  queue_[oldest].nextAttemptAt = now;
  return outcome;
}

SyncAction SyncPolicy::nextAction(SteadyTime now, SyncReadiness ready) const noexcept {
  SyncAction action;

  // ★readiness ゲートは初回即時 poll の判定より前（上流 server_sync.cpp:56-58）。
  //   ここで return しても polledOnce_ は消費されない。
  if (!ready.ok()) {
    return action;
  }

  // 優先: active な record が 1 件でもあれば、この tick は record だけを見る。
  bool anyActive = false;
  for (std::size_t i = 0; i < kQueueSize; ++i) {
    if (queue_[i].active) {
      anyActive = true;
      break;
    }
  }

  if (anyActive) {
    for (std::size_t i = 0; i < kQueueSize; ++i) {
      if (queue_[i].active && now >= queue_[i].nextAttemptAt) {
        action.kind = SyncActionKind::SendRecord;
        action.queueIndex = i;
        action.item = queue_[i].item;
        action.recordedAt = queue_[i].recordedAt;
        return action;
      }
    }
    // ★全件バックオフ中。config poll はしない（上流と同じ。ヘッダ冒頭の節を参照）。
    return action;
  }

  if (!polledOnce_ || (now - lastPollAt_) >= config_.pollInterval) {
    action.kind = SyncActionKind::PollConfig;
    return action;
  }

  return action;
}

void SyncPolicy::applyResult(const SyncAction& action, bool success,
                             SteadyTime completedAt) noexcept {
  // ★default 節を足さないこと（-Wswitch で新しい種別に気づくため）。
  switch (action.kind) {
    case SyncActionKind::None:
      return;

    case SyncActionKind::PollConfig:
      // 上流は成否に関わらず次回間隔の起点を更新し、初回即時 poll を消費する
      // （失敗しても 30 秒は間隔を空ける。server_sync.cpp:74-77）。
      lastPollAt_ = completedAt;
      polledOnce_ = true;
      if (success) {
        configPollOk_ = true;
      }
      return;

    case SyncActionKind::SendRecord: {
      if (action.queueIndex >= kQueueSize) {
        return;  // 壊れた action。状態を変えない。
      }
      QueuedRecord& record = queue_[action.queueIndex];
      if (!record.active) {
        return;  // 既に決着済み（追い出し等）。二重適用しない。
      }

      if (success) {
        record.active = false;  // キューから除去（上流 server_sync.cpp:422-425）。
        ++sent_;
        return;
      }

      // 失敗（400/401/403/404/409/5xx/タイムアウト/接続不可のいずれも同じ扱い）。
      // ★上流は HTTP ステータスで分岐しない。code < 0（トランスポート失敗）も
      //   同じ減算経路を通る (server_sync.cpp:418-443)。
      //   ここで「401 は永続的な失敗だから即諦める」等の分岐を足さないこと。
      if (record.retriesLeft > 0) {
        --record.retriesLeft;
      }
      if (record.retriesLeft == 0) {
        // 総試行 5 回を使い切った。破棄する。
        // ★ローカルの記録は別に保持されている前提（上流 server_sync.cpp:436-439）。
        record.active = false;
        ++dropped_;
      } else {
        record.nextAttemptAt = completedAt + config_.retryInterval;
      }
      return;
    }
  }
}

void SyncPolicy::onTimeJump(const TimeJump& jump) noexcept {
  // clock.hpp のポリシー: 巻き戻し（skew < 0）では何もしない。
  if (catchUp(jump).value <= 0) {
    return;
  }
  lastPollAt_ = rebaseForJump(lastPollAt_, jump);
  for (std::size_t i = 0; i < kQueueSize; ++i) {
    if (queue_[i].active) {
      queue_[i].nextAttemptAt = rebaseForJump(queue_[i].nextAttemptAt, jump);
    }
  }
}

std::size_t SyncPolicy::activeCount() const noexcept {
  std::size_t count = 0;
  for (std::size_t i = 0; i < kQueueSize; ++i) {
    if (queue_[i].active) {
      ++count;
    }
  }
  return count;
}

bool SyncPolicy::hasPendingRecord(CareItem item) const noexcept {
  for (std::size_t i = 0; i < kQueueSize; ++i) {
    if (queue_[i].active && queue_[i].item == item) {
      return true;
    }
  }
  return false;
}

}  // namespace ambient::core
