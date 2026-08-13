// scheduler.cpp — 写真の再生スケジューラ（レーン 1B）
//
// 移植元: app/static/js/scheduler.js:25-208
//   photoPriority (63) / buildPlayOrder (80) / nextQueueIdx (113) /
//   markShown (127) / photoSig (139) / syncSchedule (172)
//
// ★乱数の消費順序が契約である。ヘッダ冒頭の表と
//   tests/unit/test_golden_trace.cpp を同時に直さずにここを触らないこと。
//   消費順序がずれても Mac のテストは通り、Tab5 でだけ再生が発散する。

#include "ambient/core/photo/scheduler.hpp"

#include <algorithm>
#include <cmath>
#include <string_view>
#include <vector>

#include "ambient/core/model/parse.hpp"

namespace ambient::core {

namespace {

// ----------------------------------------------------------------------------
// shown（photoId 昇順の不変条件に依存する）
// ----------------------------------------------------------------------------

[[nodiscard]] std::vector<ShownEntry>::const_iterator lowerBoundShown(
    const std::vector<ShownEntry>& shown, std::string_view id) noexcept {
  return std::lower_bound(shown.begin(), shown.end(), id,
                          [](const ShownEntry& entry, std::string_view key) noexcept {
                            return entry.photoId < key;
                          });
}

[[nodiscard]] std::optional<std::uint32_t> findShownRound(const SchedulerSnapshot& state,
                                                          std::string_view id) noexcept {
  const auto it = lowerBoundShown(state.shown, id);
  if (it == state.shown.end() || it->photoId != id) {
    return std::nullopt;
  }
  return it->round;
}

void upsertShown(SchedulerSnapshot& state, const PhotoId& id, std::uint32_t round) {
  const auto it = lowerBoundShown(state.shown, id);
  if (it != state.shown.end() && it->photoId == id) {
    // const_iterator から非 const の位置を得る。
    const auto offset = it - state.shown.begin();
    state.shown[static_cast<std::size_t>(offset)].round = round;
    return;
  }
  state.shown.insert(it, ShownEntry{id, round});
}

// ----------------------------------------------------------------------------
// knownIdHashes（syncSchedule 用。実行時のみの状態）
// ----------------------------------------------------------------------------

// FNV-1a 32bit。ID は数字列なので分散があれば何でもよい。
// ★衝突しても壊れない（snapshot.hpp の knownIdHashes のコメント参照）。
[[nodiscard]] std::uint32_t idHash(std::string_view id) noexcept {
  std::uint32_t hash = 2166136261U;
  for (const char c : id) {
    hash ^= static_cast<std::uint32_t>(static_cast<unsigned char>(c));
    hash *= 16777619U;
  }
  return hash;
}

void rememberIds(SchedulerSnapshot& state, const PhotoManifest& manifest) {
  state.knownIdHashes.clear();
  state.knownIdHashes.reserve(manifest.items.size());
  for (const PhotoItem& item : manifest.items) {
    state.knownIdHashes.push_back(idHash(item.id));
  }
  std::sort(state.knownIdHashes.begin(), state.knownIdHashes.end());
  state.knownIdHashes.erase(
      std::unique(state.knownIdHashes.begin(), state.knownIdHashes.end()),
      state.knownIdHashes.end());
  state.knownIdsPrimed = true;
}

[[nodiscard]] bool wasKnown(const SchedulerSnapshot& state, std::string_view id) noexcept {
  return std::binary_search(state.knownIdHashes.begin(), state.knownIdHashes.end(), idHash(id));
}

// ----------------------------------------------------------------------------
// 優先度の入力を組み立てる
// ----------------------------------------------------------------------------
//
// ★ここでは乱数を引かない。引くのは photoPriority だけ（1 枚につき 1 回）。
[[nodiscard]] PriorityInput makeInput(const PhotoItem& item, const SchedulerSnapshot& state,
                                      const SchedulerNow& now) noexcept {
  PriorityInput input{};
  input.currentRound = state.roundNo;
  input.lastShownRound = findShownRound(state, item.id);
  input.fav = item.fav;
  input.qualityRecommended = item.qualityRecommended;
  input.qualityAlternate = item.qualityAlternate;

  // 新着ブースト。fetched_at が読めないときは掛けない（出典 JS も isNaN で弾く）。
  if (now.wall.has_value() && !item.fetchedAt.empty()) {
    if (const auto fetched = parseIso8601Fixed(item.fetchedAt)) {
      const WallTime at = toWallTime(*fetched);
      const std::int64_t elapsed = now.wall->epochSeconds - at.epochSeconds;
      if (elapsed >= 0) {
        input.ageDaysSinceFetch = static_cast<float>(elapsed) / 86400.0F;
      }
    }
  }

  // 記念日ブースト。`time` は撮影地のローカル時刻なので月日しか使わない。
  if (now.todayDayOfYear.has_value()) {
    if (const auto taken = parsePhotoTime(item.time)) {
      if (const auto doy = dayOfYearNoLeap(taken->month, taken->day)) {
        input.annivWeight = annivWeight(*doy, *now.todayDayOfYear);
      }
    }
  }
  return input;
}

// 1 ラウンドのスコア付け結果。
struct ScoredPhoto {
  std::size_t index{0};
  float score{0.0F};
};

// manifest に無くなった ID を playQueue と shown から取り除く。
//
// ★出典 scheduler.js では 2 箇所でこれをやっている:
//   loadPlayState (163-164)  復元直後のフィルタ
//   syncSchedule  (182-183)  差分同期時の除去
//   decodeSnapshot は manifest を知らないので前者を担えない。したがって
//   **syncSchedule の初回（prime）でも必ず通す**こと。ここを通さないと、
//   消えた写真の shown エントリが永久に残り、maxShownEntries の予算を
//   食い潰す（= 上限を掛ける意味が無くなる）。
//
// ★乱数を引かない。したがって初回同期の draws は 0 のまま。
void removeMissing(const PhotoManifest& manifest, SchedulerSnapshot& state) {
  // manifest の ID を 1 度だけソートして使う。
  // （PhotoManifest::rebuildIndex() が呼ばれていない場合でも O(n log n) で済む）
  std::vector<std::string_view> presentIds;
  presentIds.reserve(manifest.items.size());
  for (const PhotoItem& item : manifest.items) {
    presentIds.emplace_back(item.id);
  }
  std::sort(presentIds.begin(), presentIds.end());
  const auto present = [&presentIds](std::string_view id) noexcept {
    return std::binary_search(presentIds.begin(), presentIds.end(), id);
  };

  // ★順序は変えない。先頭が「次に出る写真」なので、並べ替えると再生位置が飛ぶ。
  state.playQueue.erase(
      std::remove_if(state.playQueue.begin(), state.playQueue.end(),
                     [&present](const PhotoId& id) noexcept { return !present(id); }),
      state.playQueue.end());
  state.shown.erase(
      std::remove_if(state.shown.begin(), state.shown.end(),
                     [&present](const ShownEntry& e) noexcept { return !present(e.photoId); }),
      state.shown.end());
}

}  // namespace

float photoPriority(const PriorityInput& input, SchedulerRng& rng) noexcept {
  // staleness。未表示なら PRI_NEVER から始まる。
  float score = 0.0F;
  if (input.lastShownRound.has_value()) {
    const std::uint32_t last = *input.lastShownRound;
    const std::uint32_t age = input.currentRound >= last ? input.currentRound - last : 0U;
    score = static_cast<float>(age);
  } else {
    score = constants::kPriNever;
  }

  // 新着ブースト（半減期 kNewHalflifeDays の指数減衰）。
  if (input.ageDaysSinceFetch.has_value() && *input.ageDaysSinceFetch >= 0.0F) {
    score += constants::kPriNewMax *
             std::pow(0.5F, *input.ageDaysSinceFetch / constants::kNewHalflifeDays);
  }

  // 記念日ブースト（環状距離・窓 kAnnivWindowDays 日で線形減衰）。
  score += constants::kPriAnnivMax * input.annivWeight;

  if (input.fav) {
    score += constants::kPriFav;
  }
  if (input.qualityRecommended) {
    score += constants::kPriQualityRecommended;
  } else if (input.qualityAlternate) {
    score += constants::kPriQualityAlternate;
  }

  // ★乱数消費点。入力によらず必ずここで 1 回引く。
  //   「fav なら引かない」のような分岐を作ると列がずれる。
  return score + rng.unit() * constants::kPriJitter;
}

std::vector<TripGroup> groupByTrip(const PhotoManifest& manifest) {
  std::vector<TripGroup> groups;
  for (std::size_t i = 0; i < manifest.items.size(); ++i) {
    const std::int32_t trip = manifest.items[i].trip;
    if (groups.empty() || groups.back().trip != trip) {
      groups.push_back(TripGroup{trip, i, 0});
    }
    ++groups.back().count;
  }
  return groups;
}

void shuffleTrips(std::span<TripGroup> groups, SchedulerRng& rng) noexcept {
  // ★Fisher-Yates。std::shuffle は使わない（分布実装が処理系依存）。
  //   消費は belowN() を groups.size() - 1 回。
  if (groups.size() < 2) {
    return;
  }
  for (std::size_t i = groups.size() - 1; i > 0; --i) {
    const std::uint32_t j = rng.belowN(static_cast<std::uint32_t>(i + 1));
    TripGroup tmp = groups[i];
    groups[i] = groups[j];
    groups[j] = tmp;
  }
}

float annivWeight(int photoDayOfYear, int todayDayOfYear) noexcept {
  // 出典 scheduler.js:43-52。年跨ぎを考慮した環状距離。
  int diff = photoDayOfYear - todayDayOfYear;
  if (diff < 0) {
    diff = -diff;
  }
  if (diff > 182) {
    diff = 365 - diff;  // 12/30 と 1/2 の距離は 3 日
  }
  if (diff < 0) {
    diff = 0;  // 入力が 0..364 の外だった場合の防御
  }
  const float distance = static_cast<float>(diff);
  if (distance >= constants::kAnnivWindowDays) {
    return 0.0F;
  }
  return 1.0F - distance / constants::kAnnivWindowDays;
}

std::size_t buildPlayOrder(const PhotoManifest& manifest, SchedulerSnapshot& state,
                           SchedulerRng& rng, const SchedulerNow& now) {
  // ★空 manifest では乱数を 1 回も引かずに戻ること（出典 scheduler.js:81）。
  if (manifest.items.empty()) {
    return 0;
  }
  ++state.roundNo;

  // 1. トリップ順の Fisher-Yates シャッフル（belowN x (groups-1)）。
  std::vector<TripGroup> groups = groupByTrip(manifest);
  shuffleTrips(groups, rng);

  std::vector<ScoredPhoto> scored;
  std::vector<std::size_t> picked;
  std::size_t appended = 0;

  for (const TripGroup& group : groups) {
    // 2. グループ内の **全** 写真を評価する（unit x 枚数）。
    //    ★TRIP_MAX で絞るのは評価の後。「上位 6 枚だけ評価」に変えると
    //      消費回数が変わり、以降の乱数列が丸ごとずれる。
    //    ★スコアは比較の前に確定させる。比較関数の中で乱数を引くと
    //      比較が非一貫になりソート結果が壊れる（出典 scheduler.js:98）。
    scored.clear();
    scored.reserve(group.count);
    for (std::size_t i = group.begin; i < group.begin + group.count; ++i) {
      const float score = photoPriority(makeInput(manifest.items[i], state, now), rng);
      scored.push_back(ScoredPhoto{i, score});
    }

    // 3. スコア降順で上位 kTripMax 枚。
    //    ★同点は添字昇順で決める。全順序にしておくと std::sort の実装差
    //      （libc++ と libstdc++ で等価要素の並びが違う）に依存しなくなる。
    std::sort(scored.begin(), scored.end(),
              [](const ScoredPhoto& a, const ScoredPhoto& b) noexcept {
                if (a.score != b.score) {
                  return a.score > b.score;
                }
                return a.index < b.index;
              });

    const std::size_t take =
        std::min<std::size_t>(static_cast<std::size_t>(constants::kTripMax), group.count);
    picked.clear();
    picked.reserve(take);
    for (std::size_t k = 0; k < take; ++k) {
      picked.push_back(scored[k].index);
    }

    // 4. 窓内は撮影時刻順（= 配列添字順）へ戻す。旅のストーリー性を保つため。
    std::sort(picked.begin(), picked.end());
    for (const std::size_t i : picked) {
      state.playQueue.push_back(manifest.items[i].id);
      ++appended;
    }
  }
  return appended;
}

std::optional<std::size_t> peekNext(const PhotoManifest& manifest, SchedulerSnapshot& state,
                                    SchedulerRng& rng, const SchedulerNow& now) {
  if (manifest.items.empty()) {
    return std::nullopt;
  }

  // ★絶対に無限ループしないこと。出典 scheduler.js:113 と同じく回数で打ち切る。
  constexpr int kGuard = 64;
  for (int guard = 0; guard < kGuard; ++guard) {
    if (state.playQueue.empty() && buildPlayOrder(manifest, state, rng, now) == 0) {
      return std::nullopt;
    }
    if (state.playQueue.empty()) {
      return std::nullopt;
    }
    if (const auto index = indexOf(manifest, state.playQueue.front())) {
      return index;  // ★消費しない。実際に表示したときだけ markShown が消費する。
    }
    // manifest から消えた ID。捨てて次へ。
    state.playQueue.erase(state.playQueue.begin());
  }

  // 異常時のフォールバック（出典 scheduler.js:119-120）。
  state.playQueue.clear();
  if (buildPlayOrder(manifest, state, rng, now) == 0 || state.playQueue.empty()) {
    return std::nullopt;
  }
  if (const auto index = indexOf(manifest, state.playQueue.front())) {
    return index;
  }
  return std::size_t{0};
}

void markShown(const PhotoManifest& manifest, std::size_t index, SchedulerSnapshot& state) {
  if (index >= manifest.items.size()) {
    return;
  }
  const PhotoId& id = manifest.items[index].id;
  // 先頭が当該写真のときだけ消費する。割り込みで別の写真を出した場合は
  // キューを動かさない（出典 scheduler.js:127）。
  if (!state.playQueue.empty() && state.playQueue.front() == id) {
    state.playQueue.erase(state.playQueue.begin());
  }
  upsertShown(state, id, state.roundNo);
}

void syncSchedule(const PhotoManifest& manifest, SchedulerSnapshot& state, SchedulerRng& rng,
                  const SchedulerNow& now) {
  const std::string signature = manifest.signature();

  // 起動直後の初回同期。現 manifest を既知集合として記録するだけで、
  // キューは作り直さない。停止中に増えていた写真は shown に無いので、
  // 次ラウンドに PRI_NEVER で自然と上位選出される（出典 scheduler.js:173-180）。
  //
  // ★ただし消えた ID の掃除はここでも行う。復元直後に必ず通るのがこの経路で、
  //   出典 JS が loadPlayState (163-164) でやっていたフィルタに相当する。
  //   decodeSnapshot は manifest を知らないので代われない。
  if (!state.knownIdsPrimed) {
    removeMissing(manifest, state);
    rememberIds(state, manifest);
    state.photosSignature = signature;
    return;
  }

  // ★巻き戻しバグの根治点: シグネチャが変わっていなければ何もしない。
  //   キューを作り直さないので、再起動しても再生位置が戻らない。
  if (signature == state.photosSignature) {
    return;
  }

  removeMissing(manifest, state);

  // 旧 ID と 1 枚も重ならない = データセットごと入れ替わった。
  // 差し込みは「少数の新着」前提なので、ここでは正規手順で作り直す
  // （出典 scheduler.js:186-195）。
  bool overlap = false;
  for (const PhotoItem& item : manifest.items) {
    if (wasKnown(state, item.id)) {
      overlap = true;
      break;
    }
  }
  if (!overlap) {
    state.playQueue.clear();
    rememberIds(state, manifest);
    state.photosSignature = signature;
    (void)buildPlayOrder(manifest, state, rng, now);
    return;
  }

  // 新規 ID を先頭 kNewInsertWindow 位置以内のランダム位置へ差し込む。
  // ★差し込むのは先頭 kNewInsertWindow 枚まで（出典 scheduler.js:199 の slice）。
  //   一度に大量追加されたときに全件を先頭へ詰め込むと旅の構成が壊れるため。
  //   残りは PRI_NEVER により次ラウンドで最優先選出される。
  // ★乱数消費: ここで差し込む 1 枚につき belowN() を 1 回。
  const std::size_t window = static_cast<std::size_t>(constants::kNewInsertWindow);
  std::size_t inserted = 0;
  for (const PhotoItem& item : manifest.items) {
    if (inserted >= window) {
      break;
    }
    if (wasKnown(state, item.id)) {
      continue;
    }
    const auto bound = static_cast<std::uint32_t>(std::min(window, state.playQueue.size() + 1));
    const std::uint32_t position = rng.belowN(bound);
    state.playQueue.insert(
        state.playQueue.begin() + static_cast<std::ptrdiff_t>(position), item.id);
    ++inserted;
  }

  rememberIds(state, manifest);
  state.photosSignature = signature;
}

}  // namespace ambient::core
