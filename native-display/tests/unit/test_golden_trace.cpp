// tests/unit/test_golden_trace.cpp — ★乱数消費順序の golden trace（レーン 1B）
//
// ############################################################################
// # このファイルが守っているもの
// #
// # 要件は「同じ seed なら Mac でも Tab5 でも同じ再生シーケンスになること」。
// # そのためには全消費者が **同じ順序で・同じ回数** 引かなければならない。
// # 1 箇所でも引く回数が変わると、以降の乱数列が丸ごとずれる。
// #
// # ★この違反は Mac 上のテストでは絶対に検出できない。Mac 側は常に
// #   自己整合するからである。発覚するのは Tab5 に載せた最終フェーズになる。
// #
// # そこでここでは 2 種類を固定する。
// #   (a) draws() の増分   — 「誰が何回引くか」。仕様に書くのはこちら
// #   (b) 出てくる ID の列 — 「同じ入力から同じ答え」。実装差の最終検出網
// #
// # ★ここが落ちたときの直し方:
// #   期待値を書き換えて緑に戻すのは **最後の手段**である。まず
// #   「消費順序を変えてよい変更だったのか」を疑うこと。変えてよいなら
// #   docs/CORE_CONTRACTS.md の表・scheduler.hpp 冒頭・このファイルを
// #   3 つ同時に直す。片方だけ直すと契約が嘘になる。
// ############################################################################
//
// 契約（scheduler.hpp 冒頭 / docs/CORE_CONTRACTS.md §2.3）:
//
//   photoPriority   : unit()   を写真 1 枚につき常に 1 回（入力によらず）
//   shuffleTrips    : belowN() を groups-1 回
//   buildPlayOrder  : (groups - 1) + そのラウンドで評価した写真数
//   peekNext        : キューが空でなければ 0 回。空なら buildPlayOrder 1 回ぶん
//   markShown       : 0 回
//   syncSchedule    : min(新着枚数, kNewInsertWindow) 回

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "ambient/core/constants.hpp"
#include "ambient/core/model/photo.hpp"
#include "ambient/core/photo/scheduler.hpp"
#include "ambient/core/photo/snapshot.hpp"
#include "ambient/core/rng.hpp"

using ambient::core::buildPlayOrder;
using ambient::core::decodeSnapshot;
using ambient::core::encodeSnapshot;
using ambient::core::markShown;
using ambient::core::peekNext;
using ambient::core::PhotoId;
using ambient::core::PhotoItem;
using ambient::core::PhotoManifest;
using ambient::core::SchedulerNow;
using ambient::core::SchedulerRng;
using ambient::core::SchedulerSnapshot;
using ambient::core::syncSchedule;
using ambient::core::WallTime;

namespace {

// ★golden trace の seed。値そのものに意味は無いが、変えると期待値が全部変わる。
constexpr std::uint64_t kGoldenSeed = 20260730;

std::string twoDigits(int value) {
  const std::string s = std::to_string(value);
  return s.size() < 2 ? "0" + s : s;
}

// golden trace 用の合成 manifest。
//
// ★実データ (fixture) を使わない理由: fixture は再採取で中身が変わりうる。
//   消費順序の契約は入力が動いても固定でなければならないので、
//   ここでは完全に自前で組み立てた 14 枚を使う。
//   トリップ構成 7/3/2/2 → groups = 4。
//   ★PRI_FAVの加算はfixtureの状態に依存しない合成データで検証する。
PhotoManifest makeGoldenManifest() {
  static const std::int32_t kTrips[14] = {0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 3, 3};
  PhotoManifest manifest;
  for (std::size_t i = 0; i < 14; ++i) {
    PhotoItem item;
    item.id = "p" + std::to_string(i);
    item.trip = kTrips[i];
    const int month = static_cast<int>(i % 12) + 1;
    const int day = static_cast<int>((i * 7) % 28) + 1;
    item.time = "2021-" + twoDigits(month) + "-" + twoDigits(day) + " 05:29";
    item.fetchedAt = (i % 5 == 0) ? "2026-07-29T05:40:28" : "";
    item.fav = (i % 7 == 3);
    manifest.items.push_back(item);
  }
  manifest.rebuildIndex();
  return manifest;
}

// 「今」も固定する。★Core は時計を引かない（引くとテストが決定的でなくなる）。
SchedulerNow goldenNow() {
  SchedulerNow now;
  now.wall = WallTime{1785000000};
  now.todayDayOfYear = 210;
  return now;
}

std::vector<PhotoId> slice(const std::vector<PhotoId>& queue, std::size_t from) {
  return std::vector<PhotoId>(queue.begin() + static_cast<std::ptrdiff_t>(from), queue.end());
}

}  // namespace

TEST_CASE("★golden: buildPlayOrder の draws は (groups-1) + 評価した写真数") {
  const PhotoManifest manifest = makeGoldenManifest();
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  // groups = 4（トリップ 0/1/2/3）、写真 14 枚 → 3 + 14 = 17。
  // ★14 であって 13 ではない。積まれるのは 13 枚（トリップ 0 は 7 枚中 6 枚）だが、
  //   評価はグループ内の **全** 写真に対して行うため 14 回引く。
  //   ここを「上位 6 枚だけ評価」に変えると列がずれる。
  const std::uint64_t drawsBefore = rng.draws();
  const std::size_t added = buildPlayOrder(manifest, state, rng, goldenNow());

  CHECK(added == 13);
  CHECK(rng.draws() - drawsBefore == 17);
  // 棄却が起きなければ steps も同じ。エンジンを差し替えたらここが動く。
  CHECK(rng.steps() == 17);
}

TEST_CASE("★golden: 3 ラウンドぶんの再生順が固定される") {
  const PhotoManifest manifest = makeGoldenManifest();
  const SchedulerNow now = goldenNow();
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  static const std::vector<std::vector<PhotoId>> kExpected = {
      {"p1", "p2", "p3", "p4", "p5", "p6", "p10", "p11", "p7", "p8", "p9", "p12", "p13"},
      {"p10", "p11", "p12", "p13", "p7", "p8", "p9", "p0", "p1", "p3", "p4", "p5", "p6"},
      {"p12", "p13", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9", "p10", "p11"},
  };

  std::size_t consumed = 0;
  int mismatches = 0;
  for (const std::vector<PhotoId>& expected : kExpected) {
    (void)buildPlayOrder(manifest, state, rng, now);
    if (slice(state.playQueue, consumed) != expected) {
      ++mismatches;
    }
    consumed = state.playQueue.size();
  }
  CHECK(mismatches == 0);

  // 実際の列も 1 ラウンドぶん明示的に比べる（失敗時に差分が読めるように）。
  CHECK(slice(state.playQueue, 26) == kExpected[2]);

  CHECK(rng.draws() == 51);  // 17 x 3
  CHECK(rng.steps() == 51);
  CHECK(state.roundNo == 3);
}

TEST_CASE("★golden: トリップ内は撮影時刻順（配列添字順）へ戻る") {
  // 上の golden の 1 ラウンド目 {"p1","p2","p3","p4","p5","p6", ...} は
  // トリップ 0 の窓が添字昇順に並んでいる = 旅のストーリー性が保たれている。
  // ここでは「どのラウンドでも、同一トリップの写真は添字昇順で現れる」を
  // 性質として検査する。
  const PhotoManifest manifest = makeGoldenManifest();
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  int violations = 0;
  for (int round = 0; round < 8; ++round) {
    const std::size_t before = state.playQueue.size();
    (void)buildPlayOrder(manifest, state, rng, goldenNow());
    // 同一トリップの連続 run ごとに添字が昇順であることを確認する。
    std::int32_t currentTrip = -1;
    std::size_t previousIndex = 0;
    for (std::size_t i = before; i < state.playQueue.size(); ++i) {
      const auto index = ambient::core::indexOf(manifest, state.playQueue[i]);
      if (!index) {
        ++violations;
        continue;
      }
      const std::int32_t trip = manifest.items[*index].trip;
      if (trip != currentTrip) {
        currentTrip = trip;
      } else if (*index <= previousIndex) {
        ++violations;
      }
      previousIndex = *index;
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("★golden: peekNext は消費せず、markShown が消費する") {
  const PhotoManifest manifest = makeGoldenManifest();
  const SchedulerNow now = goldenNow();
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  // キューが空なので、最初の peek だけ buildPlayOrder 1 回ぶん (17) を引く。
  const auto first = peekNext(manifest, state, rng, now);
  REQUIRE(first.has_value());
  CHECK(*first == 1);  // golden 1 ラウンド目の先頭は "p1"
  CHECK(rng.draws() == 17);
  CHECK(state.playQueue.size() == 13);

  // ★2 回目の peek は乱数を引かず、同じ写真を返す（覗くだけ）。
  const auto again = peekNext(manifest, state, rng, now);
  REQUIRE(again.has_value());
  CHECK(*again == *first);
  CHECK(rng.draws() == 17);
  CHECK(state.playQueue.size() == 13);

  // markShown で初めて消費される。★markShown は乱数を引かない。
  markShown(manifest, *first, state);
  CHECK(rng.draws() == 17);
  CHECK(state.playQueue.size() == 12);

  const auto third = peekNext(manifest, state, rng, now);
  REQUIRE(third.has_value());
  CHECK(*third == 2);  // "p2"
  CHECK(rng.draws() == 17);
}

TEST_CASE("★golden: syncSchedule の draws 増分") {
  const PhotoManifest manifest = makeGoldenManifest();
  const SchedulerNow now = goldenNow();
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  // 初回同期は既知集合を記録するだけ。乱数を引かない。
  syncSchedule(manifest, state, rng, now);
  CHECK(rng.draws() == 0);
  CHECK(state.photosSignature == "14:1l4esfj");

  (void)buildPlayOrder(manifest, state, rng, now);
  const std::uint64_t afterBuild = rng.draws();

  // ★シグネチャが変わっていなければ何もしない（巻き戻しバグの根治点）。
  syncSchedule(manifest, state, rng, now);
  CHECK(rng.draws() == afterBuild);

  // 新着 3 枚 → belowN() を 3 回。
  PhotoManifest grown = manifest;
  for (int k = 0; k < 3; ++k) {
    PhotoItem item;
    item.id = "n" + std::to_string(k);
    item.trip = 3;
    item.time = "2024-08-01 10:00";
    grown.items.push_back(item);
  }
  grown.rebuildIndex();

  syncSchedule(grown, state, rng, now);
  CHECK(rng.draws() == afterBuild + 3);

  // 差し込み位置も固定する。★3 枚とも先頭 kNewInsertWindow(=12) 位置以内に入る。
  static const std::vector<PhotoId> kExpectedQueue = {
      "n0",  "n1",  "p1", "p2", "p3", "p4", "p5", "p6",
      "p10", "p11", "n2", "p7", "p8", "p9", "p12", "p13"};
  CHECK(state.playQueue == kExpectedQueue);
}

TEST_CASE("★golden: save/restore を挟んでも以後の列が完全に一致する") {
  const PhotoManifest manifest = makeGoldenManifest();
  const SchedulerNow now = goldenNow();

  // 参照: 通しで 4 ラウンド。
  SchedulerSnapshot reference{};
  SchedulerRng referenceRng{kGoldenSeed};
  for (int i = 0; i < 4; ++i) {
    (void)buildPlayOrder(manifest, reference, referenceRng, now);
  }

  // 再起動を挟む: 2 ラウンド → 保存 → 復元 → 2 ラウンド。
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};
  for (int i = 0; i < 2; ++i) {
    (void)buildPlayOrder(manifest, state, rng, now);
  }
  state.schedulerRng = rng.save();

  SchedulerSnapshot restored = state;
  SchedulerRng restoredRng{restored.schedulerRng};
  for (int i = 0; i < 2; ++i) {
    (void)buildPlayOrder(manifest, restored, restoredRng, now);
  }

  CHECK(restored.playQueue == reference.playQueue);
  CHECK(restored.roundNo == reference.roundNo);
  CHECK(restoredRng.draws() == referenceRng.draws());
  CHECK(restoredRng.steps() == referenceRng.steps());
  CHECK(restoredRng.save() == referenceRng.save());
}

TEST_CASE("★golden: encode -> decode を挟んでも以後の列が完全に一致する") {
  // 上のテストとの違いは「メモリ上のコピー」ではなく
  // **直列化を往復する** こと。形式が乱数状態を落としていたらここで落ちる。
  const PhotoManifest manifest = makeGoldenManifest();
  const SchedulerNow now = goldenNow();

  SchedulerSnapshot reference{};
  SchedulerRng referenceRng{kGoldenSeed};
  for (int i = 0; i < 4; ++i) {
    (void)buildPlayOrder(manifest, reference, referenceRng, now);
  }

  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};
  for (int i = 0; i < 2; ++i) {
    (void)buildPlayOrder(manifest, state, rng, now);
  }
  state.schedulerRng = rng.save();

  const auto decoded = decodeSnapshot(encodeSnapshot(state));
  REQUIRE(decoded.has_value());

  SchedulerSnapshot resumed = *decoded;
  SchedulerRng resumedRng{resumed.schedulerRng};
  for (int i = 0; i < 2; ++i) {
    (void)buildPlayOrder(manifest, resumed, resumedRng, now);
  }

  CHECK(resumed.playQueue == reference.playQueue);
  CHECK(resumed.roundNo == reference.roundNo);
  CHECK(resumedRng.draws() == referenceRng.draws());
  CHECK(resumedRng.save() == referenceRng.save());
}

TEST_CASE("★golden: now を変えても draws は動かない（消費回数は入力非依存）") {
  // 新着・記念日ブーストの有無で「引く回数」が変わってはいけない。
  // 変わると「今日が誰かの記念日だったので Tab5 と列がずれた」が起きる。
  const PhotoManifest manifest = makeGoldenManifest();

  SchedulerSnapshot withNow{};
  SchedulerRng rngWith{kGoldenSeed};
  (void)buildPlayOrder(manifest, withNow, rngWith, goldenNow());

  SchedulerSnapshot without{};
  SchedulerRng rngWithout{kGoldenSeed};
  (void)buildPlayOrder(manifest, without, rngWithout, SchedulerNow{});

  CHECK(rngWith.draws() == rngWithout.draws());
  CHECK(rngWith.steps() == rngWithout.steps());
  CHECK(rngWith.save() == rngWithout.save());
  // 引く回数は同じでも、選ばれる写真は変わってよい（ブーストが効くため）。
  CHECK(withNow.playQueue.size() == without.playQueue.size());
}

TEST_CASE("★golden: 空 manifest では 1 回も引かない") {
  const PhotoManifest empty;
  SchedulerSnapshot state{};
  SchedulerRng rng{kGoldenSeed};

  CHECK(buildPlayOrder(empty, state, rng, goldenNow()) == 0);
  CHECK(rng.draws() == 0);
  CHECK_FALSE(peekNext(empty, state, rng, goldenNow()).has_value());
  CHECK(rng.draws() == 0);
  CHECK(state.roundNo == 0);  // ラウンドも進めない
}

TEST_CASE("★golden: PhotoManifest::signature が出典 JS と同じ値を出す") {
  // 出典 scheduler.js:139 photoSig。
  //   h = (h*31 + charCode) | 0 を全 ID にわたって回し、
  //   PHOTOS.length + ':' + (h>>>0).toString(36)
  // 期待値は node で出典実装を実行して得たもの（実装同士の突き合わせ）。
  PhotoManifest empty;
  CHECK(empty.signature() == "0:0");

  PhotoManifest single;
  PhotoItem item;
  item.id = "1";  // 合成fixtureの先頭ID
  single.items.push_back(item);
  CHECK(single.signature() == "1:1d");

  CHECK(makeGoldenManifest().signature() == "14:1l4esfj");
}
