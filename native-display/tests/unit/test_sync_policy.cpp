// test_sync_policy.cpp — hogo-meter 同期ポリシー（Wave 4 / Phase 3）
//
// ############################################################################
// # ★ここで固定するのは「上流 firmware の規律」であって、こちらの都合ではない
// #
// #   出典: hogo-meter firmware/lib/hogo_common/server_sync.{h,cpp}
// #   基準コミットと引用箇所の SHA-256 は tests/contract/HOGO_CONTRACT.md。
// #
// #   値を変えたくなったら、まず上流を読み、HOGO_CONTRACT.md を直すこと。
// ############################################################################
//
// このファイルは **HTTP を一切模さない**。SyncPolicy が純粋だからテストも純粋にできる。
// FakeHttpClient を使う「実際に 1 件だけ投げたか」の検証は
// tests/contract/test_hogo_client.cpp（contract_tests）の担当である。
//
// ★ループの中で CHECK を撃たないこと（CORE_CONTRACTS §11）。違反を集計して 1 回だけ検査する。

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/care/sync_policy.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"

namespace {

using ambient::core::CareItem;
using ambient::core::Millis;
using ambient::core::SteadyTime;
using ambient::core::SyncAction;
using ambient::core::SyncActionKind;
using ambient::core::SyncPolicy;
using ambient::core::SyncReadiness;
using ambient::core::WallTime;
namespace constants = ambient::core::constants;

constexpr SyncReadiness kReady{true, true};
constexpr SyncReadiness kOffline{false, true};
constexpr SyncReadiness kNoTime{true, false};

[[nodiscard]] SteadyTime at(std::int64_t ms) noexcept { return SteadyTime{ms}; }
[[nodiscard]] WallTime epoch(std::int64_t seconds) noexcept { return WallTime{seconds}; }

}  // namespace

// ============================================================================
// 定数が constants.hpp の移植値と一致していること
// ============================================================================

TEST_CASE("sync_policy: 既定値は constants.hpp の移植値そのもの") {
  const SyncPolicy policy;
  CHECK(policy.config().pollInterval == constants::kPollInterval);
  CHECK(policy.config().retryInterval == constants::kRecordRetryInterval);
  CHECK(policy.config().maxAttempts == constants::kRecordMaxRetries);
  CHECK(SyncPolicy::kQueueSize == constants::kRecordQueueSize);

  // ★「1 回の update() で HTTP は最大 1 件」は SyncAction が 1 個であることで表現される。
  CHECK(constants::kMaxHttpRequestsPerUpdate == 1);
}

// ============================================================================
// 契約 5: 起動直後は間隔を待たずに即時 1 回 poll する
// ============================================================================

TEST_CASE("sync_policy: 起動直後は即時 poll、以降は 30 秒間隔") {
  SyncPolicy policy;

  // t=0 でいきなり PollConfig。
  const SyncAction first = policy.nextAction(at(0), kReady);
  CHECK(first.kind == SyncActionKind::PollConfig);
  policy.applyResult(first, /*success=*/true, at(0));
  CHECK(policy.polledOnce());
  CHECK(policy.configPollSucceededOnce());

  // 直後は何もしない。
  CHECK(policy.nextAction(at(1), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(29'999), kReady).kind == SyncActionKind::None);

  // ちょうど 30 秒で次の poll。
  CHECK(policy.nextAction(at(30'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: readiness ゲートは即時 poll の権利を消費しない") {
  // ★上流は wifiConnected && timeValid を _polledOnce の判定より **前** で見る
  //   (server_sync.cpp:56-58)。ここを取り違えると「起動時に圏外だと最初の poll が
  //   30 秒遅れる」という、実機でしか出ない差になる。
  SyncPolicy policy;

  CHECK(policy.nextAction(at(0), kOffline).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(0), kNoTime).kind == SyncActionKind::None);
  CHECK_FALSE(policy.polledOnce());

  // 100 秒後にネットワークが上がったら、30 秒待たずに即座に 1 回目が飛ぶ。
  CHECK(policy.nextAction(at(100'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: poll が失敗しても次回間隔は 30 秒空ける") {
  // 上流 server_sync.cpp:74-77（成否に関わらず _lastPollMs と _polledOnce を更新）。
  SyncPolicy policy;
  const SyncAction first = policy.nextAction(at(0), kReady);
  policy.applyResult(first, /*success=*/false, at(0));

  CHECK(policy.polledOnce());
  CHECK_FALSE(policy.configPollSucceededOnce());  // 成功フラグだけは立たない
  CHECK(policy.nextAction(at(29'999), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(30'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: 間隔の起点はリクエスト完了時刻") {
  // ★上流は _lastPollMs = millis() を HTTP の **後** で取る (server_sync.cpp:75)。
  //   送信前の時刻を起点にすると、5 秒タイムアウトが効いたときに間隔が詰まる。
  SyncPolicy policy;
  const SyncAction first = policy.nextAction(at(0), kReady);
  policy.applyResult(first, /*success=*/true, at(5'000));  // 5 秒かかった

  CHECK(policy.nextAction(at(34'999), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(35'000), kReady).kind == SyncActionKind::PollConfig);
}

// ============================================================================
// 契約 4: 未送信キューがあれば record を config poll より優先する
// ============================================================================

TEST_CASE("sync_policy: record が config poll より優先される") {
  SyncPolicy policy;

  // 起動直後で poll したい状態。そこへ記録が入る。
  CHECK(policy.nextAction(at(0), kReady).kind == SyncActionKind::PollConfig);

  policy.enqueueRecord(CareItem::Eye, epoch(1'700'000'000), at(0));

  const SyncAction action = policy.nextAction(at(0), kReady);
  CHECK(action.kind == SyncActionKind::SendRecord);
  CHECK(action.item == CareItem::Eye);
  CHECK(action.recordedAt == epoch(1'700'000'000));

  // 送信成功でキューが空になり、初めて poll に戻る。
  policy.applyResult(action, /*success=*/true, at(0));
  CHECK(policy.activeCount() == 0);
  CHECK(policy.sentRecords() == 1);
  CHECK(policy.nextAction(at(0), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: ★バックオフ中は config poll も走らない（上流の挙動）") {
  // 上流 update() は「active な要素があれば drainOneRecord() して return」する
  // (server_sync.cpp:62-67)。drainOneRecord は期限前なら何もせず false
  // (server_sync.cpp:387-390)。つまり 5 秒のバックオフ中は poll が飢える。
  // ★これは意図的な移植である。直さないこと（HOGO_CONTRACT.md に記載）。
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  const SyncAction attempt = policy.nextAction(at(0), kReady);
  REQUIRE(attempt.kind == SyncActionKind::SendRecord);
  policy.applyResult(attempt, /*success=*/false, at(0));  // 失敗 → 5 秒待ち

  // 未 poll なのに poll しない。
  CHECK_FALSE(policy.polledOnce());
  CHECK(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(4'999), kReady).kind == SyncActionKind::None);

  // 5 秒でリトライが起きる。
  CHECK(policy.nextAction(at(5'000), kReady).kind == SyncActionKind::SendRecord);
}

// ============================================================================
// 契約 1: キュー満杯時は epoch 最小（最も古い）を上書きする
// ============================================================================

TEST_CASE("sync_policy: キュー満杯で epoch 最小が追い出される") {
  SyncPolicy policy;

  // 8 件を epoch の昇順ではない順で積む（「添字順 = 古い順」ではないことを保証する）。
  // 最小は 1'000'050（index 3）。
  const std::int64_t epochs[SyncPolicy::kQueueSize] = {
      1'000'400, 1'000'200, 1'000'900, 1'000'050, 1'000'700, 1'000'300, 1'000'600, 1'000'800,
  };
  for (std::size_t i = 0; i < SyncPolicy::kQueueSize; ++i) {
    const auto outcome = policy.enqueueRecord(CareItem::Toilet, epoch(epochs[i]), at(0));
    CHECK_FALSE(outcome.evicted);
  }
  REQUIRE(policy.activeCount() == SyncPolicy::kQueueSize);

  // 9 件目 → epoch 最小（1'000'050）が追い出される。
  const auto outcome = policy.enqueueRecord(CareItem::Food, epoch(2'000'000), at(0));
  CHECK(outcome.evicted);
  CHECK(outcome.index == 3);
  CHECK(outcome.evictedAt == epoch(1'000'050));
  CHECK(policy.evictedRecords() == 1);

  // 追い出された epoch はもうキューに無く、新しいものが同じ位置に入っている。
  std::size_t survivorsWithOldEpoch = 0;
  bool newRecordPresent = false;
  for (const auto& record : policy.queue()) {
    if (record.active && record.recordedAt == epoch(1'000'050)) {
      ++survivorsWithOldEpoch;
    }
    if (record.active && record.recordedAt == epoch(2'000'000)) {
      newRecordPresent = true;
    }
  }
  CHECK(survivorsWithOldEpoch == 0);
  CHECK(newRecordPresent);
  CHECK(policy.activeCount() == SyncPolicy::kQueueSize);  // 8 件のまま
}

TEST_CASE("sync_policy: epoch が同値なら添字の小さい方が残る（上流の strict < と同じ）") {
  SyncPolicy policy;
  for (std::size_t i = 0; i < SyncPolicy::kQueueSize; ++i) {
    policy.enqueueRecord(CareItem::Toilet, epoch(1'000'000), at(0));
  }
  const auto outcome = policy.enqueueRecord(CareItem::Food, epoch(1'000'001), at(0));
  CHECK(outcome.evicted);
  CHECK(outcome.index == 0);  // 先頭が上書きされる
}

TEST_CASE("sync_policy: 追い出しても新しい記録は満試行から始まる") {
  SyncPolicy policy;
  for (std::size_t i = 0; i < SyncPolicy::kQueueSize; ++i) {
    policy.enqueueRecord(CareItem::Toilet, epoch(1'000'000 + static_cast<std::int64_t>(i)), at(0));
  }
  const auto outcome = policy.enqueueRecord(CareItem::Food, epoch(2'000'000), at(0));
  REQUIRE(outcome.evicted);
  CHECK(policy.queue()[outcome.index].retriesLeft == constants::kRecordMaxRetries);
  CHECK(policy.queue()[outcome.index].item == CareItem::Food);
}

// ============================================================================
// 契約 2: 総試行 5 回（初回 1 回 + 再試行 4 回）で諦める。間隔は 5 秒
// ============================================================================

TEST_CASE("sync_policy: 総試行 5 回で諦める（初回 1 回 + 再試行 4 回）") {
  // ★RECORD_MAX_RETRIES = 5 は「5 回再試行」ではない（constants.hpp:116-118）。
  //   上流 server_sync.cpp:432-442 は失敗のたび 1 減らし、0 になった時点で破棄する。
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  std::vector<std::int64_t> attemptTimes;
  std::size_t violations = 0;

  // 十分に長い時間を 1 秒刻みで進めながら、SendRecord が返るたびに失敗させる。
  for (std::int64_t ms = 0; ms <= 60'000; ms += 1'000) {
    const SyncAction action = policy.nextAction(at(ms), kReady);
    if (action.kind != SyncActionKind::SendRecord) {
      continue;
    }
    attemptTimes.push_back(ms);
    if (action.item != CareItem::Toilet) {
      ++violations;  // ★ループ内で CHECK を撃たない。集計する。
    }
    policy.applyResult(action, /*success=*/false, at(ms));
  }

  CHECK(violations == 0);

  // 総試行 5 回。
  REQUIRE(attemptTimes.size() == 5);
  // 間隔は 5 秒（0 / 5000 / 10000 / 15000 / 20000）。
  CHECK(attemptTimes[0] == 0);
  CHECK(attemptTimes[1] == 5'000);
  CHECK(attemptTimes[2] == 10'000);
  CHECK(attemptTimes[3] == 15'000);
  CHECK(attemptTimes[4] == 20'000);

  // 5 回目の失敗で破棄され、キューが空になる。
  CHECK(policy.activeCount() == 0);
  CHECK(policy.droppedRecords() == 1);
  CHECK(policy.sentRecords() == 0);

  // キューが空になったので config poll へ戻る。
  CHECK(policy.nextAction(at(21'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: 4 回失敗してから成功すれば破棄されない") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Food, epoch(1'700'000'000), at(0));

  std::int64_t ms = 0;
  for (int i = 0; i < 4; ++i) {
    const SyncAction action = policy.nextAction(at(ms), kReady);
    REQUIRE(action.kind == SyncActionKind::SendRecord);
    policy.applyResult(action, /*success=*/false, at(ms));
    ms += 5'000;
  }
  const SyncAction last = policy.nextAction(at(ms), kReady);
  REQUIRE(last.kind == SyncActionKind::SendRecord);
  policy.applyResult(last, /*success=*/true, at(ms));

  CHECK(policy.droppedRecords() == 0);
  CHECK(policy.sentRecords() == 1);
  CHECK(policy.activeCount() == 0);
}

TEST_CASE("sync_policy: リトライ間隔の起点もリクエスト完了時刻") {
  // 上流 server_sync.cpp:441 は millis() + RETRY_INTERVAL_MS を **完了後** に積む。
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  const SyncAction action = policy.nextAction(at(0), kReady);
  REQUIRE(action.kind == SyncActionKind::SendRecord);
  policy.applyResult(action, /*success=*/false, at(5'000));  // タイムアウトで 5 秒消費

  CHECK(policy.nextAction(at(9'999), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(10'000), kReady).kind == SyncActionKind::SendRecord);
}

TEST_CASE("sync_policy: 失敗の種類でリトライ規律を変えない") {
  // ★上流は HTTP ステータスで分岐しない。401 も 5xx も接続失敗も同じ減算経路
  //   (server_sync.cpp:418-443)。「401 は永続的だから即諦める」を足すと
  //   secret ローテーション直後の記録が黙って消える。
  //   ここでは SyncPolicy が bool しか受け取らないことで構造的に保証されている。
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));
  const SyncAction action = policy.nextAction(at(0), kReady);
  REQUIRE(action.kind == SyncActionKind::SendRecord);
  policy.applyResult(action, /*success=*/false, at(0));
  CHECK(policy.queue()[action.queueIndex].retriesLeft == constants::kRecordMaxRetries - 1);
  CHECK(policy.activeCount() == 1);
}

// ============================================================================
// 契約 3: 1 回の判断で返る action は常に 1 個以下
// ============================================================================

TEST_CASE("sync_policy: nextAction は副作用を持たない（何度呼んでも同じ）") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  const SyncAction a = policy.nextAction(at(0), kReady);
  const SyncAction b = policy.nextAction(at(0), kReady);
  const SyncAction c = policy.nextAction(at(0), kReady);
  CHECK(a.kind == b.kind);
  CHECK(b.kind == c.kind);
  CHECK(a.queueIndex == c.queueIndex);
  // 3 回呼んでもキューは 1 件のまま（applyResult していないので減らない）。
  CHECK(policy.activeCount() == 1);
}

TEST_CASE("sync_policy: 複数件のキューは 1 tick に 1 件ずつしか進まない") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'001), at(0));
  policy.enqueueRecord(CareItem::Eye, epoch(1'700'000'002), at(0));
  policy.enqueueRecord(CareItem::Food, epoch(1'700'000'003), at(0));
  REQUIRE(policy.activeCount() == 3);

  std::vector<CareItem> order;
  std::size_t violations = 0;
  for (int i = 0; i < 3; ++i) {
    const SyncAction action = policy.nextAction(at(0), kReady);
    if (action.kind != SyncActionKind::SendRecord) {
      ++violations;
      break;
    }
    order.push_back(action.item);
    policy.applyResult(action, /*success=*/true, at(0));
  }
  CHECK(violations == 0);
  REQUIRE(order.size() == 3);
  // 走査は添字順（上流 drainOneRecord も先頭から最初に見つけた 1 件）。
  CHECK(order[0] == CareItem::Toilet);
  CHECK(order[1] == CareItem::Eye);
  CHECK(order[2] == CareItem::Food);
  CHECK(policy.activeCount() == 0);
}

// ============================================================================
// 二重適用・壊れた action への耐性
// ============================================================================

TEST_CASE("sync_policy: 同じ action を二度 applyResult しても壊れない") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));
  const SyncAction action = policy.nextAction(at(0), kReady);
  REQUIRE(action.kind == SyncActionKind::SendRecord);

  policy.applyResult(action, /*success=*/true, at(0));
  policy.applyResult(action, /*success=*/true, at(0));  // 二度目は無視される
  CHECK(policy.sentRecords() == 1);
  CHECK(policy.activeCount() == 0);
}

TEST_CASE("sync_policy: 範囲外の queueIndex は状態を変えない") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  SyncAction broken;
  broken.kind = SyncActionKind::SendRecord;
  broken.queueIndex = SyncPolicy::kQueueSize + 5;
  policy.applyResult(broken, /*success=*/false, at(0));

  CHECK(policy.activeCount() == 1);
  CHECK(policy.queue()[0].retriesLeft == constants::kRecordMaxRetries);
}

TEST_CASE("sync_policy: None の applyResult は何もしない") {
  SyncPolicy policy;
  const SyncAction none;
  REQUIRE(none.kind == SyncActionKind::None);
  policy.applyResult(none, /*success=*/true, at(12'345));
  CHECK_FALSE(policy.polledOnce());
  CHECK_FALSE(policy.configPollSucceededOnce());
}

// ============================================================================
// 時間跳躍（スリープ復帰）
// ============================================================================

TEST_CASE("sync_policy: スリープ復帰で poll 予定が巻き戻り、即時 poll になる") {
  // clock.hpp:208-211「同じ rebaseForJump() を次回ポーリング予定時刻に当てれば
  // 『即時 1 回ポーリング』になる」。
  SyncPolicy policy;
  const SyncAction first = policy.nextAction(at(0), kReady);
  policy.applyResult(first, /*success=*/true, at(0));
  CHECK(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::None);

  // 8 時間スリープ: 壁時計だけ進み、単調時刻は 1 秒しか進んでいない。
  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{1'000};
  jump.wallDelta = Millis{8 * 60 * 60 * 1000};
  jump.skew = jump.wallDelta - jump.steadyDelta;
  policy.onTimeJump(jump);

  CHECK(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: 壁時計の巻き戻しでは何もしない") {
  // ★skew < 0 で基準点を未来へずらすと、間隔が延びて同期が止まる（clock.hpp のポリシー）。
  SyncPolicy policy;
  const SyncAction first = policy.nextAction(at(0), kReady);
  policy.applyResult(first, /*success=*/true, at(0));

  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{1'000};
  jump.wallDelta = Millis{-3'600'000};
  jump.skew = jump.wallDelta - jump.steadyDelta;
  policy.onTimeJump(jump);

  CHECK(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::None);
  CHECK(policy.nextAction(at(30'000), kReady).kind == SyncActionKind::PollConfig);
}

TEST_CASE("sync_policy: スリープ復帰でリトライのバックオフも明ける") {
  SyncPolicy policy;
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));
  const SyncAction action = policy.nextAction(at(0), kReady);
  REQUIRE(action.kind == SyncActionKind::SendRecord);
  policy.applyResult(action, /*success=*/false, at(0));
  REQUIRE(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::None);

  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{0};
  jump.wallDelta = Millis{60'000};
  jump.skew = Millis{60'000};
  policy.onTimeJump(jump);

  CHECK(policy.nextAction(at(1'000), kReady).kind == SyncActionKind::SendRecord);
}

// ============================================================================
// PendingCareRecord（D-4）との接続
// ============================================================================

TEST_CASE("sync_policy: 確定した記録をそのまま積める") {
  // ★保留の不変条件そのものは tests/unit/test_care_pending.cpp が持つ。
  //   ここは「確定 → 送信キュー」の接続だけを見る。
  ambient::core::PendingCareRecord pending;
  SyncPolicy policy;

  const auto ignored = pending.begin(CareItem::Eye, at(0), epoch(1'700'000'500));
  CHECK_FALSE(ignored.has_value());

  const auto committed = pending.leave(ambient::core::PendingExit::ToastExpired);
  REQUIRE(committed.has_value());
  policy.enqueue(*committed, at(2'000));

  CHECK(policy.activeCount() == 1);
  CHECK(policy.hasPendingRecord(CareItem::Eye));
  CHECK_FALSE(policy.hasPendingRecord(CareItem::Toilet));
  CHECK(policy.queue()[0].recordedAt == epoch(1'700'000'500));
}

TEST_CASE("sync_policy: 取り消された記録は積まれない") {
  ambient::core::PendingCareRecord pending;
  SyncPolicy policy;

  const auto ignored = pending.begin(CareItem::Eye, at(0), epoch(1'700'000'500));
  CHECK_FALSE(ignored.has_value());
  const auto discarded = pending.leave(ambient::core::PendingExit::UndoHit);
  CHECK_FALSE(discarded.has_value());

  CHECK(policy.activeCount() == 0);
}

// ============================================================================
// toString（doctest の失敗メッセージが "{?}" にならないこと。CORE_CONTRACTS §11）
// ============================================================================

TEST_CASE("sync_policy: SyncActionKind の toString") {
  CHECK(std::string{ambient::core::toString(SyncActionKind::None)} == "None");
  CHECK(std::string{ambient::core::toString(SyncActionKind::SendRecord)} == "SendRecord");
  CHECK(std::string{ambient::core::toString(SyncActionKind::PollConfig)} == "PollConfig");
}
