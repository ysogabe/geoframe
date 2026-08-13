// care_model.cpp — ambient/core/care/care_model.hpp の実装
//
// ★設計判断と出典はヘッダに書いてある。ここは実装だけ。

#include "ambient/core/care/care_model.hpp"

#include <algorithm>

namespace ambient::core {
namespace {

// UTF-8 の継続バイト（10xxxxxx）か。
[[nodiscard]] constexpr bool isContinuationByte(char ch) noexcept {
  return (static_cast<unsigned char>(ch) & 0xC0U) == 0x80U;
}

}  // namespace

// ----------------------------------------------------------------------------
// 純粋関数
// ----------------------------------------------------------------------------

CareLayout careLayout(std::size_t activeSlots) noexcept {
  // 出典: firmware/variants/tab5/main.cpp:140-142
  //   return (serverSync.activeItemCount() >= 4) ? 6 : 3;
  // ★0（未 poll）も 3 に落ちる。境界を変えないこと。
  if (activeSlots >= 4) {
    return CareLayout{6, 2, 3};
  }
  return CareLayout{3, 1, 3};
}

std::string formatElapsed(WallTime lastEpoch, WallTime now) {
  // 出典: firmware/lib/hogo_common/ui_model.cpp:13-36。分岐と閾値を変えないこと。
  if (lastEpoch.epochSeconds == 0) {
    return "未記録";
  }

  // ★巻き戻り（NTP 再同期など）は 0 にクランプする（上流と同じ）。
  const std::int64_t diff =
      (now.epochSeconds > lastEpoch.epochSeconds) ? (now.epochSeconds - lastEpoch.epochSeconds) : 0;

  if (diff < 60) {
    return "たった今";
  }
  if (diff < 3600) {
    return std::to_string(diff / 60) + "分前";
  }
  if (diff < 86400) {
    const std::int64_t hours = diff / 3600;
    const std::int64_t minutes = (diff % 3600) / 60;
    if (minutes == 0) {
      return std::to_string(hours) + "時間前";
    }
    return std::to_string(hours) + "時間" + std::to_string(minutes) + "分前";
  }
  return std::to_string(diff / 86400) + "日前";
}

std::string truncateLabelUtf8(std::string_view label, std::size_t maxBytes) {
  if (label.size() <= maxBytes) {
    return std::string{label};
  }

  // maxBytes 番目のバイトが継続バイトなら、コードポイントの途中で切っている。
  // 先頭側へ戻って境界を探す（上流の strlcpy はこれをしないので化ける）。
  std::size_t cut = maxBytes;
  while (cut > 0 && isContinuationByte(label[cut])) {
    --cut;
  }
  return std::string{label.substr(0, cut)};
}

// ----------------------------------------------------------------------------
// CareModel
// ----------------------------------------------------------------------------

std::size_t CareModel::indexOf(CareItem item) noexcept {
  const auto raw = static_cast<std::size_t>(item);
  return raw < constants::kCareItemCount ? raw : 0;
}

// 期限到来のうち最優先の 1 件。
// ★規則: Red があれば最も小さい slot の Red。無ければ最も小さい slot の Yellow。
//   「先に見つけた方」ではないので、Yellow(slot0) と Red(slot2) では Red が勝つ。
//   走査するのは表示される slot だけ（レイアウト外の残留 status を拾わない）。
CareModel::DueSlot CareModel::findDue() const noexcept {
  const CareLayout layout = careLayout(snapshot_.activeSlots);
  const std::size_t limit = std::min(layout.itemCount, constants::kCareItemCount);

  DueSlot yellow;
  for (std::size_t i = 0; i < limit; ++i) {
    const ScheduleStatus status = snapshot_.slots[i].status;
    if (status == ScheduleStatus::Red) {
      return DueSlot{i, ScheduleStatus::Red};
    }
    if (status == ScheduleStatus::Yellow && !yellow.slot) {
      yellow = DueSlot{i, ScheduleStatus::Yellow};
    }
  }
  return yellow;
}

std::optional<CommittedRecord> CareModel::beginRecord(CareItem item, SteadyTime now,
                                                      WallTime wallNow) {
  // ★同じカードの保留中に再タップ -> 無視（重複記録防止）。
  //   出典: firmware/variants/tab5/main.cpp:305-307
  //
  //   このガードが無いと pending_.begin() が leave(OtherCardTapped) 経由で
  //   1 件目を **確定** し、1 タップのつもりが 2 件記録される。
  //   ダッシュボードを見るまで気づけないので、ここで止める。
  //
  //   ★上流はこれをタッチハンドラ側に置いているが、こちらは CareModel に置く。
  //     app 層が 1 つとは限らない（タッチ / キーボード / テスト）ので、
  //     どの入力経路から来ても成立させるため。
  //   ★保留は破棄しない（猶予の起点も動かさない）。D-4 の不変条件に触れない。
  if (pending_.active() && pending_.pendingItem() == item) {
    return std::nullopt;
  }

  // 既存の保留は「別カードのタップ」として **確定** される（破棄されない）。
  std::optional<CommittedRecord> previous = pending_.begin(item, now, wallNow);
  if (previous) {
    noteCommitted(*previous, now);
  }
  return previous;
}

std::optional<CommittedRecord> CareModel::leave(PendingExit reason, SteadyTime now) {
  // ★不変条件は pending_record.hpp が持つ。ここで経路を列挙しない。
  //   このメソッドは「確定したらローカルへ反映する」を足しているだけである。
  std::optional<CommittedRecord> committed = pending_.leave(reason);
  if (committed) {
    noteCommitted(*committed, now);
  }
  return committed;
}

std::optional<CommittedRecord> CareModel::tick(SteadyTime now) {
  std::optional<CommittedRecord> committed = pending_.tick(now);
  if (committed) {
    noteCommitted(*committed, now);
  }
  return committed;
}

void CareModel::noteCommitted(const CommittedRecord& record, SteadyTime now) noexcept {
  const std::size_t index = indexOf(record.item);
  // ★max を取る。巻き戻った壁時計で最終記録時刻を古くしない。
  if (record.recordedAt > localLastEpoch_[index]) {
    localLastEpoch_[index] = record.recordedAt;
  }
  awaitingSince_[index] = now;
}

void CareModel::applyConfig(const HogoConfigSnapshot& snapshot) {
  snapshot_ = snapshot;
  configReceived_ = true;

  for (std::size_t i = 0; i < constants::kCareItemCount; ++i) {
    if (!snapshot_.slots[i].present) {
      // ★この poll に居ない slot は「サーバ待ち」を解除する。
      //   6→3 の縮小で消えた slot は present になる機会が二度と来ないため、
      //   ここで解除しないと最強 claim（RecordedAwaitingServer）が
      //   画面に無いカードのために出続け、地震アラートまで抑止する。
      awaitingSince_[i].reset();
      continue;
    }
    // サーバがローカルに追いついたら「サーバ待ち」を解除する。
    if (snapshot_.slots[i].lastEpoch >= localLastEpoch_[i]) {
      awaitingSince_[i].reset();
    }
  }
}

void CareModel::openCareScreen(SteadyTime now, Millis duration) noexcept {
  careScreenUntil_ = now + (duration.value > 0 ? duration : Millis{20000});
}

void CareModel::closeCareScreen() noexcept { careScreenUntil_.reset(); }

void CareModel::onTimeJump(const TimeJump& jump) noexcept {
  if (catchUp(jump).value <= 0) {
    return;
  }
  for (auto& since : awaitingSince_) {
    if (since) {
      since = rebaseForJump(*since, jump);
    }
  }
  if (careScreenUntil_) {
    careScreenUntil_ = rebaseForJump(*careScreenUntil_, jump);
  }
  // ★保留（2 秒猶予）は意図的に触らない。clock.hpp の該当節を参照
  //   （復帰後の最初の tick が ToastExpired で **確定** する。それが正しい）。
}

bool CareModel::awaitingServer(SteadyTime now) const noexcept {
  for (const auto& since : awaitingSince_) {
    if (since && (now - *since) < awaitingMax_) {
      return true;
    }
  }
  return false;
}

WallTime CareModel::lastEpochFor(CareItem item) const noexcept {
  const std::size_t index = indexOf(item);
  // ★上流 tab5/main.cpp:156-157 と同じ max(ローカル, サーバ)。
  return std::max(localLastEpoch_[index], snapshot_.slots[index].lastEpoch);
}

void CareModel::collect(SteadyTime now, std::vector<Claim>& out) const {
  // ---- 最強: 記録操作と取り消し猶予 / サーバ確定待ち --------------------
  //
  // claim.hpp の突合結果より、企画書 §2.3 rank 2「記録操作と取り消し猶予」が
  // RecordedAwaitingServer に対応する。保留中もここに入る。
  if (pending_.active()) {
    Claim claim;
    claim.priority = ClaimPriority::RecordedAwaitingServer;
    claim.kind = DisplayStateKind::RecordedAwaitingServer;
    claim.sourceId = kSourceId;
    claim.payloadId = static_cast<std::uint32_t>(indexOf(pending_.pendingItem())) + 1U;
    // 猶予が切れたら自動的に無効になる（tick() が確定へ回すまでの安全網）。
    claim.expiresAt = now + pending_.remaining(now);
    out.push_back(claim);
    return;
  }

  // ★走査は表示される slot だけ（findDue() と同じ境界）。
  //   レイアウト外の残留を拾うと、画面に無いカードのために最強 claim が出る。
  const std::size_t awaitingLimit =
      std::min(careLayout(snapshot_.activeSlots).itemCount, constants::kCareItemCount);
  for (std::size_t i = 0; i < awaitingLimit; ++i) {
    if (!awaitingSince_[i]) {
      continue;
    }
    const SteadyTime expires = *awaitingSince_[i] + awaitingMax_;
    // ★source 側でも期限を見る。expiresAt だけに頼らないこと（claim.hpp:148-150）。
    if (now >= expires) {
      continue;
    }
    Claim claim;
    claim.priority = ClaimPriority::RecordedAwaitingServer;
    claim.kind = DisplayStateKind::RecordedAwaitingServer;
    claim.sourceId = kSourceId;
    claim.payloadId = static_cast<std::uint32_t>(i) + 1U;
    claim.expiresAt = expires;
    out.push_back(claim);
    return;
  }

  // ---- 期限到来（Red / Yellow）------------------------------------------
  //
  // ★display_state.hpp:36-37 が CareDue を「Red / Yellow」と定義している。
  //   Yellow を Care へ落とさないこと。
  const auto [due, dueStatus] = findDue();

  if (due) {
    Claim claim;
    claim.priority = ClaimPriority::CareDue;
    claim.kind = DisplayStateKind::CareDue;
    claim.sourceId = kSourceId;
    // ★status も payloadId に混ぜる。Yellow → Red へ変わったら「別の内容」として
    //   画面を更新させるため（slot が同じでも色が変わる）。
    claim.payloadId = (static_cast<std::uint32_t>(*due) + 1U) * 16U +
                      static_cast<std::uint32_t>(dueStatus);
    out.push_back(claim);
    return;
  }

  // ---- ユーザが開いたお世話画面 ------------------------------------------
  if (careScreenUntil_ && now < *careScreenUntil_) {
    Claim claim;
    claim.priority = ClaimPriority::Care;
    claim.kind = DisplayStateKind::Care;
    claim.sourceId = kSourceId;
    claim.expiresAt = *careScreenUntil_;
    out.push_back(claim);
  }
}

CareView CareModel::view(SteadyTime now, WallTime wallNow) const {
  CareView view;
  view.layout = careLayout(snapshot_.activeSlots);
  view.configReceived = configReceived_;

  const bool pendingActive = pending_.active();
  const std::size_t pendingIndex = pendingActive ? indexOf(pending_.pendingItem())
                                                 : constants::kCareItemCount;
  if (pendingActive) {
    view.pendingItem = pending_.pendingItem();
    view.undoRemaining = pending_.remaining(now);
  }

  view.cards.reserve(view.layout.itemCount);
  for (std::size_t i = 0; i < view.layout.itemCount && i < constants::kCareItemCount; ++i) {
    const CareSlotConfig& slot = snapshot_.slots[i];

    CareCardView card;
    card.slot = i;
    card.label = slot.label;
    card.status = slot.status;
    card.shouldLight = slot.shouldLight;
    // ★max(ローカル, サーバ)。記録直後は色がサーバ待ちでも経過時間だけは即座に戻る。
    card.lastEpoch = std::max(localLastEpoch_[i], slot.lastEpoch);
    card.elapsed = formatElapsed(card.lastEpoch, wallNow);
    card.awaitingServer = awaitingSince_[i].has_value() &&
                          (now - *awaitingSince_[i]) < awaitingMax_;
    card.pending = pendingActive && i == pendingIndex;
    view.cards.push_back(std::move(card));
  }

  // ★claim と同じ関数を使う。別々に書くと「画面は Red なのに遷移は Yellow」が起きる。
  const DueSlot due = findDue();
  view.dueSlot = due.slot;
  view.dueStatus = due.status;

  return view;
}

}  // namespace ambient::core
