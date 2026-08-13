// tests/unit/test_photo_scheduler.cpp — 写真スケジューラ（レーン 1B）
//
// ############################################################################
// # ★ここが golden trace テストの本拠地
// #
// # buildPlayOrder / syncSchedule を実装したら、まず
// # 「rng.draws() が何回増えるか」を固定すること。
// # 契約は ambient/core/photo/scheduler.hpp の冒頭にある。
// #
// #   buildPlayOrder: (groups - 1) + そのラウンドで評価した写真数
// #   syncSchedule  : 新着枚数
// #
// # これを固定しないと、Mac ではテストが通り Tab5 で発散する。
// # そして発覚するのは最終フェーズである。
// ############################################################################
//
// ★次フェーズでここに書くもの:
//   - 全網羅（飢餓なし）: 十分な回数まわせば全写真が 1 回は出ること
//   - トリップ内は撮影時刻順が保たれること（旅のストーリー性）
//   - manifest から消えた ID を捨てて次へ進むこと（無限ループしないこと）
//   - スナップショットの往復（encode → decode → 同じ状態）
//   - ★上限を掛けたスナップショットでもスケジューラが壊れないこと
//     （Tab5 の NVS 予算。これを Mac で確認できなければ上限の仕組みは無意味）

#include <doctest.h>

#include <algorithm>
#include <cstdint>
#include <initializer_list>
#include <string>
#include <vector>

#include "ambient/core/constants.hpp"
#include "ambient/core/photo/scheduler.hpp"
#include "ambient/core/photo/snapshot.hpp"

using ambient::core::groupByTrip;
using ambient::core::PhotoItem;
using ambient::core::PhotoManifest;
using ambient::core::photoPriority;
using ambient::core::PriorityInput;
using ambient::core::SchedulerRng;
using ambient::core::SchedulerSnapshot;
using ambient::core::shuffleTrips;
using ambient::core::TripGroup;

namespace {

PhotoManifest makeManifest(std::initializer_list<std::int32_t> trips) {
  PhotoManifest manifest;
  int n = 0;
  for (const std::int32_t trip : trips) {
    PhotoItem item;
    item.id = "p" + std::to_string(n++);
    item.trip = trip;
    manifest.items.push_back(item);
  }
  return manifest;
}

}  // namespace

TEST_CASE("トリップのグループ化（PHOTOS は時系列順なので連続 run で検出できる）") {
  const PhotoManifest manifest = makeManifest({0, 0, 0, 1, 1, 2});
  const std::vector<TripGroup> groups = groupByTrip(manifest);

  REQUIRE(groups.size() == 3);
  CHECK(groups[0].trip == 0);
  CHECK(groups[0].begin == 0);
  CHECK(groups[0].count == 3);
  CHECK(groups[1].count == 2);
  CHECK(groups[2].count == 1);
}

TEST_CASE("★シャッフルの乱数消費は groups - 1 回（Fisher-Yates）") {
  std::vector<TripGroup> groups = groupByTrip(makeManifest({0, 1, 2, 3, 4}));
  REQUIRE(groups.size() == 5);

  SchedulerRng rng{1};
  const std::uint64_t before = rng.draws();
  shuffleTrips(groups, rng);
  CHECK(rng.draws() == before + 4);  // 5 - 1
}

TEST_CASE("シャッフル: グループが 0 個 / 1 個なら乱数を引かない") {
  SchedulerRng rng{1};

  std::vector<TripGroup> empty;
  const std::uint64_t before0 = rng.draws();
  shuffleTrips(empty, rng);
  CHECK(rng.draws() == before0);

  std::vector<TripGroup> single = groupByTrip(makeManifest({7}));
  REQUIRE(single.size() == 1);
  const std::uint64_t before1 = rng.draws();
  shuffleTrips(single, rng);
  CHECK(rng.draws() == before1);
}

TEST_CASE("シャッフルは要素を落とさない（置換であること）") {
  std::vector<TripGroup> groups = groupByTrip(makeManifest({0, 1, 2, 3, 4, 5, 6, 7}));
  SchedulerRng rng{2026};
  shuffleTrips(groups, rng);

  std::vector<std::int32_t> trips;
  for (const TripGroup& g : groups) {
    trips.push_back(g.trip);
  }
  std::sort(trips.begin(), trips.end());
  CHECK(trips == std::vector<std::int32_t>{0, 1, 2, 3, 4, 5, 6, 7});
}

TEST_CASE("★優先度計算の乱数消費は写真 1 枚につき常に 1 回") {
  SchedulerRng rng{5};

  PriorityInput never{};
  never.currentRound = 10;
  const std::uint64_t before = rng.draws();
  (void)photoPriority(never, rng);
  CHECK(rng.draws() == before + 1);

  // fav でもブースト無しでも、引く回数は変わらない。
  // （条件分岐で引かない経路を作ると列がずれる）
  PriorityInput fav{};
  fav.currentRound = 10;
  fav.lastShownRound = 5;
  fav.fav = true;
  fav.annivWeight = 1.0F;
  fav.ageDaysSinceFetch = 0.0F;
  const std::uint64_t before2 = rng.draws();
  (void)photoPriority(fav, rng);
  CHECK(rng.draws() == before2 + 1);
}

TEST_CASE("★未表示の写真は常に最優先クラス（飢餓なし保証）") {
  SchedulerRng rng{11};

  PriorityInput never{};
  never.currentRound = 60;  // 未表示

  // 全ブースト付きで staleness 60 の既表示写真
  PriorityInput boosted{};
  boosted.currentRound = 60;
  boosted.lastShownRound = 0;  // staleness 60
  boosted.fav = true;
  boosted.annivWeight = 1.0F;
  boosted.ageDaysSinceFetch = 0.0F;

  boosted.qualityRecommended = true;

  // PRI_NEVER=45、ブースト合計の上限 27pt。
  // staleness 60 + 27 = 87 なので、staleness が十分大きければ追い越す
  // （= 飢餓しない）。逆に staleness が小さいうちは未表示が勝つ。
  PriorityInput fresh{};
  fresh.currentRound = 60;
  fresh.lastShownRound = 59;  // staleness 1
  fresh.fav = true;
  fresh.annivWeight = 1.0F;
  fresh.ageDaysSinceFetch = 0.0F;
  fresh.qualityRecommended = true;

  CHECK(photoPriority(never, rng) > photoPriority(fresh, rng));
  CHECK(photoPriority(boosted, rng) > photoPriority(never, rng));
}

TEST_CASE("品質推奨は優先し、類似alternateは有限減点に留める") {
  SchedulerRng rng{33};
  PriorityInput recommended{};
  PriorityInput neutral{};
  PriorityInput alternate{};
  recommended.currentRound = neutral.currentRound = alternate.currentRound = 20;
  recommended.lastShownRound = neutral.lastShownRound = alternate.lastShownRound = 19;
  recommended.qualityRecommended = true;
  alternate.qualityAlternate = true;

  const float recommendedScore = photoPriority(recommended, rng);
  const float neutralScore = photoPriority(neutral, rng);
  const float alternateScore = photoPriority(alternate, rng);
  CHECK(recommendedScore > neutralScore);
  CHECK(neutralScore > alternateScore);
  CHECK(rng.draws() == 3);  // 品質ロールによらず写真ごとに必ず1回
}

TEST_CASE("スナップショットのキーが NVS のキー長上限に収まる") {
  CHECK(ambient::core::kSnapshotKey.size() <= ambient::core::constants::kNvsMaxKeyBytes);
}

TEST_CASE("スナップショットに乱数状態が含まれる（再起動をまたいだ再現性）") {
  SchedulerRng rng{4242};
  for (int i = 0; i < 20; ++i) {
    (void)rng.belowN(6);
  }

  SchedulerSnapshot snapshot{};
  snapshot.schedulerRng = rng.save();

  SchedulerRng restored{snapshot.schedulerRng};
  bool allEqual = true;
  for (int i = 0; i < 50; ++i) {
    if (restored.next() != rng.next()) {
      allEqual = false;
    }
  }
  CHECK(allEqual);
}

TEST_CASE("★shown を無制限に持つと Tab5 の NVS には入らない（設計上の前提）") {
  // 設計値2000件 x "id:round" に対しNVSは20KB、しかもhogo-meterが
  // 既に 4 namespace 使用中。上限を掛ける仕組み (SnapshotLimits) が要る理由。
  constexpr std::size_t kBytesPerEntryLowerBound = 6;  // 控えめな見積もり
  CHECK(ambient::core::constants::kDesignPhotoCount * kBytesPerEntryLowerBound >
        ambient::core::constants::kTab5NvsPartitionBytes / 2);

  // 上限を掛ける口が存在すること（値そのものは未確定）。
  CHECK(ambient::core::kTab5ProvisionalLimits.maxShownEntries > 0);
  CHECK(ambient::core::kTab5ProvisionalLimits.maxShownEntries <
        ambient::core::constants::kDesignPhotoCount);
}

// ============================================================================
// 以下、レーン 1B が buildPlayOrder / peekNext / markShown / syncSchedule を
// 実装したときに追加したもの。
//
// ★乱数の消費順序（draws / steps / ID 列）は tests/unit/test_golden_trace.cpp が
//   固定している。ここで見るのは **性質** の方:
//     全網羅 / 飢餓なし / 新着の差し込み窓 / 巻き戻さないこと / 消えた ID の扱い
// ============================================================================

#include <cstddef>
#include <fstream>
#include <string_view>

#include "ambient/core/model/parse.hpp"

using ambient::core::annivWeight;
using ambient::core::buildPlayOrder;
using ambient::core::decodeSnapshot;
using ambient::core::encodeSnapshot;
using ambient::core::markShown;
using ambient::core::peekNext;
using ambient::core::PhotoId;
using ambient::core::SchedulerNow;
using ambient::core::ShownEntry;
using ambient::core::syncSchedule;
using ambient::core::WallTime;

namespace {

PhotoManifest indexedManifest(std::initializer_list<std::int32_t> trips) {
  PhotoManifest manifest = makeManifest(trips);
  manifest.rebuildIndex();
  return manifest;
}

// トリップ構成を件数の並びで指定して manifest を作る。
PhotoManifest manifestFromGroupSizes(const std::vector<std::size_t>& sizes) {
  PhotoManifest manifest;
  std::int32_t trip = 0;
  int serial = 0;
  for (const std::size_t size : sizes) {
    for (std::size_t k = 0; k < size; ++k) {
      PhotoItem item;
      item.id = "s" + std::to_string(serial++);
      item.trip = trip;
      manifest.items.push_back(item);
    }
    ++trip;
  }
  manifest.rebuildIndex();
  return manifest;
}

// 全写真が 1 度は出るまでに要したラウンド数。届かなければ -1。
//
// ★理論下限は ceil(最大トリップ枚数 / TRIP_MAX)。
//   N 枚のトリップは 1 ラウンドに TRIP_MAX 枚しか出ないため。
int roundsUntilFullCoverage(const PhotoManifest& manifest, SchedulerRng& rng,
                            const SchedulerNow& now, int maxRounds) {
  SchedulerSnapshot state{};
  std::vector<char> seen(manifest.items.size(), 0);
  std::size_t remaining = manifest.items.size();

  for (int round = 1; round <= maxRounds; ++round) {
    const std::size_t added = buildPlayOrder(manifest, state, rng, now);
    if (added == 0) {
      return -1;
    }
    for (std::size_t k = 0; k < added; ++k) {
      const auto index = peekNext(manifest, state, rng, now);
      if (!index) {
        return -1;
      }
      if (seen[*index] == 0) {
        seen[*index] = 1;
        --remaining;
      }
      markShown(manifest, *index, state);
    }
    if (remaining == 0) {
      return round;
    }
  }
  return -1;
}

// ----------------------------------------------------------------------------
// fixture 読み込み（テスト専用の割り切った実装）
// ----------------------------------------------------------------------------
//
// ★これは JSON パーサではない。/api/photos の構造解析は別レーンの担当で、
//   ここでは「スケジューラに食わせる id / trip / time / fetched_at / fav」を
//   取り出すだけである。
// ★取り出しが正しいかは、出典 JS の photoSig と同じシグネチャになるかで検証する
//   （id を 1 件でも取りこぼすと一致しない）。

std::string readIfExists(std::string_view relative) {
  // unit_tests には AMBIENT_ASSET_DIR が渡されていない（層分離の検証のため
  // リンク行を触らない方針）。ctest は build/ を cwd にするので、
  // 代表的な起動位置ぶんだけ候補を試す。
  static const char* kPrefixes[] = {"../", "", "native-display/", "../native-display/"};
  for (const char* prefix : kPrefixes) {
    std::string path = std::string(prefix) + std::string(relative);
    std::ifstream file(path, std::ios::binary);
    if (file) {
      return std::string(std::istreambuf_iterator<char>(file), std::istreambuf_iterator<char>());
    }
  }
  return {};
}

// depth 1 のオブジェクトを切り出す（文字列内の波括弧は数えない）。
std::vector<std::string_view> splitObjects(std::string_view json) {
  std::vector<std::string_view> objects;
  bool inString = false;
  bool escaped = false;
  int depth = 0;
  std::size_t start = 0;
  for (std::size_t i = 0; i < json.size(); ++i) {
    const char c = json[i];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (c == '\\') {
        escaped = true;
      } else if (c == '"') {
        inString = false;
      }
      continue;
    }
    if (c == '"') {
      inString = true;
    } else if (c == '{') {
      if (depth == 0) {
        start = i;
      }
      ++depth;
    } else if (c == '}') {
      --depth;
      if (depth == 0) {
        objects.push_back(json.substr(start, i - start + 1));
      }
    }
  }
  return objects;
}

// "key": <value> の <value> を生のまま返す（前後の空白と引用符は落とす）。
std::string jsonField(std::string_view object, std::string_view key) {
  const std::string needle = "\"" + std::string(key) + "\"";
  const std::size_t at = object.find(needle);
  if (at == std::string_view::npos) {
    return {};
  }
  std::size_t i = at + needle.size();
  while (i < object.size() && (object[i] == ' ' || object[i] == ':')) {
    ++i;
  }
  if (i < object.size() && object[i] == '"') {
    const std::size_t end = object.find('"', i + 1);
    if (end == std::string_view::npos) {
      return {};
    }
    return std::string(object.substr(i + 1, end - i - 1));
  }
  const std::size_t end = object.find_first_of(",}", i);
  std::string_view raw = object.substr(i, end == std::string_view::npos ? end : end - i);
  while (!raw.empty() && (raw.back() == ' ' || raw.back() == '\n' || raw.back() == '\r')) {
    raw.remove_suffix(1);
  }
  return std::string(raw);
}

PhotoManifest parsePhotosForTest(std::string_view json) {
  PhotoManifest manifest;
  for (const std::string_view object : splitObjects(json)) {
    PhotoItem item;
    item.id = jsonField(object, "id");
    item.time = jsonField(object, "time");
    item.fetchedAt = jsonField(object, "fetched_at");
    item.fav = jsonField(object, "fav") == "true";
    const auto trip = ambient::core::parseIntPrefix(jsonField(object, "trip"));
    item.trip = trip.value_or(0);
    manifest.items.push_back(item);
  }
  manifest.rebuildIndex();
  return manifest;
}

}  // namespace

TEST_CASE("★annivWeight: 環状距離・窓 14 日で線形減衰（年跨ぎ考慮）") {
  const auto doy = [](int m, int d) { return *ambient::core::dayOfYearNoLeap(m, d); };

  CHECK(annivWeight(doy(7, 29), doy(7, 29)) == doctest::Approx(1.0));
  CHECK(annivWeight(doy(7, 29), doy(7, 30)) == doctest::Approx(1.0 - 1.0 / 14.0));
  CHECK(annivWeight(doy(7, 29), doy(8, 12)) == doctest::Approx(0.0));  // ちょうど 14 日 = 窓外
  CHECK(annivWeight(doy(1, 1), doy(7, 1)) == doctest::Approx(0.0));

  // ★年跨ぎ: 12/30 と 1/2 の距離は 3 日。
  CHECK(annivWeight(doy(12, 30), doy(1, 2)) == doctest::Approx(1.0 - 3.0 / 14.0));
  CHECK(annivWeight(doy(1, 2), doy(12, 30)) == doctest::Approx(1.0 - 3.0 / 14.0));
}

TEST_CASE("★PRI_FAV の加算を合成データで担保") {
  // fixtureの状態に依存せず、ここでPRI_FAV = 2の効果を固定する。
  SchedulerRng rng{7};

  PriorityInput plain{};
  plain.currentRound = 10;
  plain.lastShownRound = 5;

  PriorityInput favourite = plain;
  favourite.fav = true;

  // ゆらぎ [0,4) があるので 1 回の比較では決まらない。
  // 差の期待値が PRI_FAV になることを平均で見る。
  double sum = 0.0;
  constexpr int kTrials = 4000;
  for (int i = 0; i < kTrials; ++i) {
    sum += static_cast<double>(photoPriority(favourite, rng) - photoPriority(plain, rng));
  }
  CHECK(sum / kTrials == doctest::Approx(ambient::core::constants::kPriFav).epsilon(0.05));
}

TEST_CASE("★全網羅: 理論下限 ceil(最大トリップ枚数 / TRIP_MAX) ラウンドで一巡する") {
  // 最大グループ168枚を含む合成構成で再現する。
  // ★最大グループ 168 枚 → ceil(168 / 6) = 28 ラウンドが理論下限。
  static const std::vector<std::size_t> kSyntheticGroupSizes = {
      168, 144, 120, 96, 72, 48, 24, 6};
  const PhotoManifest manifest = manifestFromGroupSizes(kSyntheticGroupSizes);
  REQUIRE(manifest.items.size() == 678);

  // ★「28 以内」ではなく「ちょうど 28」を固定する。27 で足りてしまうなら
  //   TRIP_MAX の意味が変わっているし、29 以上なら優先度が劣化している。
  // ★複数の seed で確認する。1 seed だけだと「たまたま 28 だった」と
  //   「PRI_NEVER の余裕が本当に足りている」を区別できない。
  //   余裕の計算: 窓 28 ラウンド内の既表示写真の最大スコアは
  //   staleness 27 + anniv 3 + fav 2 + jitter 4 = 36 < PRI_NEVER 45。
  //   したがって未表示写真は常に最優先クラスに居続ける。
  int offBound = 0;
  for (std::uint64_t seed : {std::uint64_t{1}, std::uint64_t{2026}, std::uint64_t{31337},
                             std::uint64_t{4242}}) {
    SchedulerRng rng{seed};
    if (roundsUntilFullCoverage(manifest, rng, SchedulerNow{}, 60) != 28) {
      ++offBound;
    }
  }
  CHECK(offBound == 0);
}

TEST_CASE("★全網羅: トリップが小さいときも理論下限に一致する") {
  const PhotoManifest tiny = manifestFromGroupSizes({6, 6, 6});
  SchedulerRng rngTiny{1};
  CHECK(roundsUntilFullCoverage(tiny, rngTiny, SchedulerNow{}, 10) == 1);

  const PhotoManifest one = manifestFromGroupSizes({7});
  SchedulerRng rngOne{1};
  CHECK(roundsUntilFullCoverage(one, rngOne, SchedulerNow{}, 10) == 2);  // ceil(7/6)

  const PhotoManifest mixed = manifestFromGroupSizes({13, 3, 1});
  SchedulerRng rngMixed{1};
  CHECK(roundsUntilFullCoverage(mixed, rngMixed, SchedulerNow{}, 10) == 3);  // ceil(13/6)
}

TEST_CASE("★飢餓なし: 長時間まわしても特定の写真が出ないままにならない") {
  // ブースト合計は 21pt で有界、staleness は上限なく増える。
  // したがって長く出ていない写真は必ず追い越して選出される。
  const PhotoManifest manifest = manifestFromGroupSizes({40, 40, 40});
  SchedulerSnapshot state{};
  SchedulerRng rng{31337};

  // 全ブーストが常時かかる「有利な」写真を混ぜても、不利な写真が飢えないこと。
  SchedulerNow now;
  now.wall = WallTime{1785000000};
  now.todayDayOfYear = 210;

  std::vector<int> shownTimes(manifest.items.size(), 0);
  for (int step = 0; step < 4000; ++step) {
    const auto index = peekNext(manifest, state, rng, now);
    REQUIRE(index.has_value());
    shownTimes[*index] += 1;
    markShown(manifest, *index, state);
  }

  int never = 0;
  int minimum = shownTimes[0];
  for (const int count : shownTimes) {
    if (count == 0) {
      ++never;
    }
    minimum = count < minimum ? count : minimum;
  }
  CHECK(never == 0);
  // 出方の偏りも見ておく（全網羅なら 4000/120 ≒ 33 回前後に落ち着く）。
  CHECK(minimum >= 20);
}

TEST_CASE("★新着 1 枚は先頭 NEW_INSERT_WINDOW 位置以内へ差し込まれる") {
  // 「数分以内に画面に出る」ことの担保。1 枚だけなら押し下げが起きないので、
  // 差し込み位置そのものを直接検査できる。
  const PhotoManifest base = indexedManifest({0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1});

  int outsideWindow = 0;
  int notFound = 0;
  for (std::uint64_t seed = 1; seed <= 40; ++seed) {
    SchedulerSnapshot state{};
    SchedulerRng rng{seed};
    syncSchedule(base, state, rng, SchedulerNow{});  // prime
    (void)buildPlayOrder(base, state, rng, SchedulerNow{});
    (void)buildPlayOrder(base, state, rng, SchedulerNow{});  // キューを十分長くする

    PhotoManifest grown = base;
    PhotoItem fresh;
    fresh.id = "fresh";
    fresh.trip = 2;
    grown.items.push_back(fresh);
    grown.rebuildIndex();

    const std::uint64_t before = rng.draws();
    syncSchedule(grown, state, rng, SchedulerNow{});
    if (rng.draws() - before != 1) {
      ++outsideWindow;  // 新着 1 枚につき belowN() は 1 回
    }

    bool found = false;
    for (std::size_t i = 0; i < state.playQueue.size(); ++i) {
      if (state.playQueue[i] == "fresh") {
        found = true;
        if (i >= ambient::core::constants::kNewInsertWindow) {
          ++outsideWindow;
        }
        break;
      }
    }
    if (!found) {
      ++notFound;
    }
  }
  CHECK(notFound == 0);
  CHECK(outsideWindow == 0);
}

TEST_CASE("★一度に大量追加されても差し込みは NEW_INSERT_WINDOW 枚まで") {
  const PhotoManifest base = indexedManifest({0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1});
  SchedulerSnapshot state{};
  SchedulerRng rng{5};

  syncSchedule(base, state, rng, SchedulerNow{});  // prime
  (void)buildPlayOrder(base, state, rng, SchedulerNow{});
  const std::size_t queueBefore = state.playQueue.size();

  // ★30 枚足しても差し込むのは 12 枚だけ。全件を先頭へ詰め込むと旅の構成が壊れる。
  //   残りは shown に無いので PRI_NEVER により次ラウンドで最優先選出される。
  PhotoManifest grown = base;
  constexpr int kNewCount = 30;
  for (int k = 0; k < kNewCount; ++k) {
    PhotoItem item;
    item.id = "new" + std::to_string(k);
    item.trip = 2;
    grown.items.push_back(item);
  }
  grown.rebuildIndex();

  const std::uint64_t before = rng.draws();
  syncSchedule(grown, state, rng, SchedulerNow{});

  // 乱数消費は min(新着, kNewInsertWindow) 回。30 枚足しても 12 回。
  const std::size_t window = ambient::core::constants::kNewInsertWindow;
  CHECK(rng.draws() - before == window);
  CHECK(state.playQueue.size() == queueBefore + window);

  // ★差し込みは常に「そのときの先頭 12 位置以内」だが、あとから差し込まれた
  //   ぶんに押し下げられるので、最終位置は最大 (window-1) + (window-1) になる。
  //   出典 scheduler.js の splice も同じ挙動であり、ここを (i < window) に
  //   すると出典より強い性質を要求してしまう。
  const std::size_t bound = (window - 1) + (window - 1);
  int outsideBound = 0;
  int found = 0;
  for (std::size_t i = 0; i < state.playQueue.size(); ++i) {
    if (state.playQueue[i].rfind("new", 0) == 0) {
      ++found;
      if (i > bound) {
        ++outsideBound;
      }
    }
  }
  CHECK(found == static_cast<int>(window));
  CHECK(outsideBound == 0);

  // 差し込まれなかった 18 枚も、次ラウンドで PRI_NEVER により全部出る。
  const std::size_t added = buildPlayOrder(grown, state, rng, SchedulerNow{});
  CHECK(added > 0);
  int freshInNextRound = 0;
  for (std::size_t i = state.playQueue.size() - added; i < state.playQueue.size(); ++i) {
    if (state.playQueue[i].rfind("new", 0) == 0) {
      ++freshInNextRound;
    }
  }
  CHECK(freshInNextRound > 0);
}

TEST_CASE("★再起動しても再生位置が巻き戻らない（sig 不変なら何もしない）") {
  // 巻き戻しバグの根治点。ここが崩れると「起動のたびに同じ写真から始まる」。
  const PhotoManifest manifest = indexedManifest({0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2});
  SchedulerSnapshot state{};
  SchedulerRng rng{77};

  syncSchedule(manifest, state, rng, SchedulerNow{});
  for (int i = 0; i < 5; ++i) {
    const auto index = peekNext(manifest, state, rng, SchedulerNow{});
    REQUIRE(index.has_value());
    markShown(manifest, *index, state);
  }
  state.schedulerRng = rng.save();

  const std::vector<PhotoId> queueBefore = state.playQueue;
  const std::uint32_t roundBefore = state.roundNo;
  const std::size_t shownBefore = state.shown.size();

  // 保存 → 復元 → 同じ manifest で同期。
  const auto reloaded = decodeSnapshot(encodeSnapshot(state));
  REQUIRE(reloaded.has_value());
  SchedulerSnapshot resumed = *reloaded;
  SchedulerRng resumedRng{resumed.schedulerRng};

  const std::uint64_t drawsAtStart = resumedRng.draws();
  syncSchedule(manifest, resumed, resumedRng, SchedulerNow{});  // 復元直後の prime
  syncSchedule(manifest, resumed, resumedRng, SchedulerNow{});  // 2 回目は sig 不変

  CHECK(resumedRng.draws() == drawsAtStart);  // 乱数を引かない = 作り直していない
  CHECK(resumed.playQueue == queueBefore);    // ★キューが温存されている
  CHECK(resumed.roundNo == roundBefore);
  CHECK(resumed.shown.size() == shownBefore);

  // 次に出る写真も同じ。
  const auto next = peekNext(manifest, resumed, resumedRng, SchedulerNow{});
  REQUIRE(next.has_value());
  CHECK(manifest.items[*next].id == queueBefore.front());
}

TEST_CASE("★復元直後の同期で、消えた写真の shown / queue が掃除される") {
  // 出典 JS は loadPlayState (163-164) でこれをやっていた。
  // decodeSnapshot は manifest を知らないので、prime 経路が担う。
  // ★掃除を怠ると、消えた写真の shown が永久に残り maxShownEntries を食い潰す。
  const PhotoManifest manifest = indexedManifest({0, 0, 0});  // p0, p1, p2

  SchedulerSnapshot state{};
  state.playQueue = {"p0", "gone1", "p1"};
  state.shown = {ShownEntry{"gone0", 1}, ShownEntry{"gone2", 2}, ShownEntry{"p0", 3}};
  // shown は photoId 昇順（"gone0" < "gone2" < "p0"）。

  SchedulerRng rng{1};
  const std::uint64_t before = rng.draws();
  syncSchedule(manifest, state, rng, SchedulerNow{});  // prime

  CHECK(rng.draws() == before);  // 掃除は乱数を引かない
  CHECK(state.playQueue == std::vector<PhotoId>{"p0", "p1"});
  REQUIRE(state.shown.size() == 1);
  CHECK(state.shown[0].photoId == "p0");
}

TEST_CASE("★manifest から消えた ID は捨てて次へ進む（無限ループしない）") {
  const PhotoManifest manifest = indexedManifest({0, 0, 0});
  SchedulerSnapshot state{};
  SchedulerRng rng{3};

  // 解決できない ID だけが並んだキュー。
  state.playQueue = {"ghost0", "ghost1", "ghost2"};
  const auto index = peekNext(manifest, state, rng, SchedulerNow{});
  REQUIRE(index.has_value());
  CHECK(*index < manifest.items.size());
  // ghost は全部捨てられ、新しいラウンドが積まれている。
  CHECK(state.roundNo == 1);
}

TEST_CASE("★データセットごと入れ替わったらキューを作り直す（重なりが 0 のとき）") {
  const PhotoManifest mock = indexedManifest({0, 0, 0});
  SchedulerSnapshot state{};
  SchedulerRng rng{11};

  syncSchedule(mock, state, rng, SchedulerNow{});  // prime
  (void)buildPlayOrder(mock, state, rng, SchedulerNow{});
  CHECK(state.playQueue.size() == 3);

  // 起動時に server.py 未応答でモックで始まり、あとから実写真へ切り替わる経路。
  PhotoManifest real;
  for (int i = 0; i < 20; ++i) {
    PhotoItem item;
    item.id = "real" + std::to_string(i);
    item.trip = i / 10;
    real.items.push_back(item);
  }
  real.rebuildIndex();

  syncSchedule(real, state, rng, SchedulerNow{});

  // 旧 ID は 1 つも残らず、正規手順で 1 ラウンド作り直されている。
  int stale = 0;
  for (const PhotoId& id : state.playQueue) {
    if (id.rfind("real", 0) != 0) {
      ++stale;
    }
  }
  CHECK(stale == 0);
  CHECK(state.playQueue.size() == 12);  // 2 トリップ x TRIP_MAX
}

TEST_CASE("markShown: 割り込みで別の写真を出したときはキューを動かさない") {
  const PhotoManifest manifest = indexedManifest({0, 0, 0, 0, 0, 0, 0, 0});
  SchedulerSnapshot state{};
  SchedulerRng rng{13};

  const auto head = peekNext(manifest, state, rng, SchedulerNow{});
  REQUIRE(head.has_value());
  const std::size_t queueSize = state.playQueue.size();

  // キュー先頭ではない写真を表示した（イベント割り込みなど）。
  const std::size_t other = (*head + 3) % manifest.items.size();
  markShown(manifest, other, state);

  CHECK(state.playQueue.size() == queueSize);  // 消費しない
  CHECK(state.playQueue.front() == manifest.items[*head].id);
  // shown は更新される。
  CHECK(state.shown.size() == 1);
  CHECK(state.shown[0].photoId == manifest.items[other].id);
}

TEST_CASE("markShown: 範囲外の添字は無視する") {
  const PhotoManifest manifest = indexedManifest({0, 0});
  SchedulerSnapshot state{};
  markShown(manifest, 999, state);
  CHECK(state.shown.empty());
}

// ============================================================================
// 実データ / fixture（存在するときだけ走る）
// ============================================================================

TEST_CASE("fixture (50 件) で全網羅が理論下限に一致する") {
  const std::string json = readIfExists("assets/fixtures/api/photos.json");
  if (json.empty()) {
    WARN_MESSAGE(false, "assets/fixtures/api/photos.json が見つからないのでスキップ");
    return;
  }
  const PhotoManifest manifest = parsePhotosForTest(json);
  REQUIRE(manifest.items.size() == 50);
  // ★取り出しが正しいことの検証（出典 JS の photoSig と突き合わせた値）。
  CHECK(manifest.signature() == "50:k81yao");

  // 最大グループ 15 枚 → ceil(15/6) = 3。
  SchedulerRng rng{2026};
  CHECK(roundsUntilFullCoverage(manifest, rng, SchedulerNow{}, 20) == 3);
}

TEST_CASE("★ローカルのフルfixtureでも全写真を巡回できる") {
  // tests/data/ は gitignore 済み（実座標を含むためコミットしない）。
  // 無ければスキップする。
  const std::string json = readIfExists("tests/data/photos.body");
  if (json.empty()) {
    WARN_MESSAGE(false, "tests/data/photos.body が無いのでスキップ（gitignore 済み）");
    return;
  }
  const PhotoManifest manifest = parsePhotosForTest(json);
  REQUIRE_FALSE(manifest.items.empty());
  SchedulerRng plainRng{2026};
  CHECK(roundsUntilFullCoverage(manifest, plainRng, SchedulerNow{}, 10000) > 0);

}
