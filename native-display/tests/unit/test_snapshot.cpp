// tests/unit/test_snapshot.cpp — スケジューラ状態の直列化と上限（レーン 1B）
//
// ============================================================================
// このファイルが守っているもの
// ============================================================================
//
// 1. 形式の後方互換 — 保存済みスナップショットを新しいパーサが読めること。
//    ★未知バージョンは読まずに nullopt を返すこと。読むと壊れたデータを
//      正常として使い、しかもエラーが出ない。
//
// 2. 壊れた入力で例外を投げず、範囲外アクセスもしないこと。
//    保存先はファイル / NVS で、電源断による切り詰めが現実に起きる。
//
// 3. ★上限の実効性 — Tab5 の NVS 予算 (4KB) に設計値2000件が入らないことと、
//    truncateForStore を通せば入ることを **Mac 上で** 示す。
//    これを示せなければ SnapshotLimits は「あるだけ」で無意味である。

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
#include "ambient/core/state_store.hpp"

using ambient::core::decodeSnapshot;
using ambient::core::encodeSnapshot;
using ambient::core::MemoryStateStore;
using ambient::core::PhotoId;
using ambient::core::PhotoItem;
using ambient::core::PhotoManifest;
using ambient::core::RngState;
using ambient::core::SchedulerRng;
using ambient::core::SchedulerSnapshot;
using ambient::core::ShownEntry;
using ambient::core::SnapshotLimits;
using ambient::core::StoreCapacity;
using ambient::core::StoreStatus;
using ambient::core::truncateForStore;

namespace {

SchedulerSnapshot makeSnapshot() {
  SchedulerSnapshot snapshot{};
  snapshot.roundNo = 7;
  snapshot.photosSignature = "14:1l4esfj";
  snapshot.playQueue = {"p3", "p4"};
  snapshot.shown = {ShownEntry{"p1", 5}, ShownEntry{"p3", 7}};
  snapshot.schedulerRng = RngState{1, 2, 3, 4, 17, 19};
  return snapshot;
}

// shown を n 件持つスナップショット（容量テスト用）。
SchedulerSnapshot makeLarge(std::size_t count) {
  SchedulerSnapshot snapshot{};
  snapshot.roundNo = 28;
  snapshot.photosSignature = "2000:synthetic";
  snapshot.shown.reserve(count);
  for (std::size_t i = 0; i < count; ++i) {
    // IDは4桁の数字列として合成する。
    std::string id = std::to_string(1000 + i);
    snapshot.shown.push_back(ShownEntry{id, static_cast<std::uint32_t>(i % 28)});
  }
  // shown は photoId 昇順が不変条件。上のループはその順で作っている。
  return snapshot;
}

}  // namespace

TEST_CASE("encode -> decode で全フィールドが戻る") {
  const SchedulerSnapshot original = makeSnapshot();
  const auto restored = decodeSnapshot(encodeSnapshot(original));
  REQUIRE(restored.has_value());

  CHECK(restored->version == original.version);
  CHECK(restored->roundNo == original.roundNo);
  CHECK(restored->photosSignature == original.photosSignature);
  CHECK(restored->playQueue == original.playQueue);
  CHECK(restored->shown == original.shown);
  CHECK(restored->schedulerRng == original.schedulerRng);
}

TEST_CASE("★形式そのものを固定する（保存済みデータとの互換の要）") {
  // ここが動いたら、既に保存されているスナップショットが読めなくなる。
  // 意図して形式を変えたなら kSnapshotVersion を上げること。
  CHECK(encodeSnapshot(makeSnapshot()) ==
        "gfp1\n7\n1 2 3 4 17 19\n10:14:1l4esfj\n2\n2:p3\n2:p4\n2\n2:p1:5\n2:p3:7\n");
}

TEST_CASE("★ID に区切り文字が混ざっても壊れない（長さ前置きの理由）") {
  // photoId は認証の無い LAN 上のサーバから来る文字列である。
  // 改行やコロンが入りうる前提で扱う。エスケープではなく長さ前置きで解く。
  SchedulerSnapshot snapshot{};
  snapshot.playQueue = {"a:b", "c\nd", "", "e:5\n"};
  snapshot.shown = {ShownEntry{"", 1}, ShownEntry{"x:y\nz", 2}};
  snapshot.photosSignature = "3:a\nb";

  const auto restored = decodeSnapshot(encodeSnapshot(snapshot));
  REQUIRE(restored.has_value());
  CHECK(restored->playQueue == snapshot.playQueue);
  CHECK(restored->photosSignature == snapshot.photosSignature);
  CHECK(restored->shown.size() == 2);
}

TEST_CASE("★未知バージョンは読まない（黙って壊れたデータを使わない）") {
  std::string encoded = encodeSnapshot(makeSnapshot());
  encoded[3] = '9';  // "gfp1" -> "gfp9"
  CHECK_FALSE(decodeSnapshot(encoded).has_value());
}

TEST_CASE("★壊れた入力で nullopt を返す（例外を投げない・範囲外を読まない）") {
  const std::string good = encodeSnapshot(makeSnapshot());

  // 空・マジック違い
  CHECK_FALSE(decodeSnapshot("").has_value());
  CHECK_FALSE(decodeSnapshot("g").has_value());
  CHECK_FALSE(decodeSnapshot("xyz1\n").has_value());
  CHECK_FALSE(decodeSnapshot("gfp").has_value());
  CHECK_FALSE(decodeSnapshot("gfp\n").has_value());

  // 電源断による途中切れ。あらゆる長さで落ちないこと。
  int failures = 0;
  for (std::size_t cut = 0; cut < good.size(); ++cut) {
    if (decodeSnapshot(std::string_view{good}.substr(0, cut)).has_value()) {
      ++failures;  // 途中で切れた入力が「成功」してはいけない
    }
  }
  CHECK(failures == 0);

  // 末尾にゴミが付いている
  CHECK_FALSE(decodeSnapshot(good + "x").has_value());
  CHECK_FALSE(decodeSnapshot(good + good).has_value());

  // 長さ前置きが残りバイト数を超える（巨大確保の誘発を防いでいるか）
  CHECK_FALSE(decodeSnapshot("gfp1\n0\n0 0 0 0 0 0\n4294967295:x\n").has_value());
  // 件数が残りバイト数に対して多すぎる
  CHECK_FALSE(decodeSnapshot("gfp1\n0\n0 0 0 0 0 0\n0:\n999999999\n").has_value());
  // 数値の桁あふれ
  CHECK_FALSE(
      decodeSnapshot("gfp1\n99999999999999999999999\n0 0 0 0 0 0\n0:\n0\n0\n").has_value());
  // uint32 に収まらない round
  CHECK_FALSE(decodeSnapshot("gfp1\n4294967296\n0 0 0 0 0 0\n0:\n0\n0\n").has_value());
  // 数字が無い
  CHECK_FALSE(decodeSnapshot("gfp1\n\n0 0 0 0 0 0\n0:\n0\n0\n").has_value());
}

TEST_CASE("空のスナップショットも往復できる") {
  const SchedulerSnapshot empty{};
  const auto restored = decodeSnapshot(encodeSnapshot(empty));
  REQUIRE(restored.has_value());
  CHECK(restored->playQueue.empty());
  CHECK(restored->shown.empty());
  CHECK(restored->roundNo == 0);
}

TEST_CASE("★実行時のみの状態（knownIds）は永続化されない") {
  // 出典 scheduler.js の knownIds は「メモリのみ・null = 未初期化」。
  // 復元直後は未初期化に戻り、初回 syncSchedule が既知集合を作り直す。
  SchedulerSnapshot snapshot = makeSnapshot();
  snapshot.knownIdHashes = {1, 2, 3};
  snapshot.knownIdsPrimed = true;

  const auto restored = decodeSnapshot(encodeSnapshot(snapshot));
  REQUIRE(restored.has_value());
  CHECK(restored->knownIdHashes.empty());
  CHECK_FALSE(restored->knownIdsPrimed);
}

TEST_CASE("★decode は shown の不変条件（photoId 昇順・重複なし）を立て直す") {
  // 保存ファイルは外から書き換えられうる。不変条件が崩れると
  // upsert の二分探索が壊れ、同じ写真の shown が二重に増える。
  const std::string handEdited =
      "gfp1\n3\n1 2 3 4 0 0\n0:\n0\n4\n2:p9:1\n2:p1:2\n2:p9:5\n2:p5:3\n";
  const auto restored = decodeSnapshot(handEdited);
  REQUIRE(restored.has_value());
  REQUIRE(restored->shown.size() == 3);  // p9 の重複が 1 件に潰れている
  CHECK(restored->shown[0].photoId == "p1");
  CHECK(restored->shown[1].photoId == "p5");
  CHECK(restored->shown[2].photoId == "p9");
}

// ============================================================================
// 上限
// ============================================================================

TEST_CASE("truncateForStore: 上限 0 は無制限") {
  const SchedulerSnapshot large = makeLarge(500);
  const SchedulerSnapshot kept = truncateForStore(large, SnapshotLimits{});
  CHECK(kept.shown.size() == 500);
}

TEST_CASE("★truncateForStore: shown は round が古い順に捨てる") {
  SchedulerSnapshot snapshot{};
  // photoId 昇順（不変条件）。round はばらばら。
  snapshot.shown = {ShownEntry{"a", 9}, ShownEntry{"b", 1}, ShownEntry{"c", 5},
                    ShownEntry{"d", 3}, ShownEntry{"e", 7}};

  SnapshotLimits limits{};
  limits.maxShownEntries = 3;
  const SchedulerSnapshot kept = truncateForStore(snapshot, limits);

  REQUIRE(kept.shown.size() == 3);
  // round 1 (b) と 3 (d) が捨てられ、5 / 7 / 9 が残る。
  CHECK(kept.shown[0].photoId == "a");
  CHECK(kept.shown[1].photoId == "c");
  CHECK(kept.shown[2].photoId == "e");
  // ★photoId 昇順の不変条件が保たれていること。
  CHECK(kept.shown[0].photoId < kept.shown[1].photoId);
  CHECK(kept.shown[1].photoId < kept.shown[2].photoId);
}

TEST_CASE("★truncateForStore: 同じ round は photoId 昇順で先に捨てる（決定的）") {
  // ★同点の扱いを決めておかないと、Mac と Tab5 で捨てる写真が食い違い、
  //   「同じ seed なら同じシーケンス」が壊れる。
  SchedulerSnapshot snapshot{};
  snapshot.shown = {ShownEntry{"a", 4}, ShownEntry{"b", 4}, ShownEntry{"c", 4}};

  SnapshotLimits limits{};
  limits.maxShownEntries = 1;
  const SchedulerSnapshot kept = truncateForStore(snapshot, limits);

  REQUIRE(kept.shown.size() == 1);
  CHECK(kept.shown[0].photoId == "c");
}

TEST_CASE("★truncateForStore: playQueue は末尾から捨てる（先頭 = 次に出る写真）") {
  SchedulerSnapshot snapshot{};
  snapshot.playQueue = {"p0", "p1", "p2", "p3", "p4"};

  SnapshotLimits limits{};
  limits.maxQueueEntries = 2;
  const SchedulerSnapshot kept = truncateForStore(snapshot, limits);

  REQUIRE(kept.playQueue.size() == 2);
  CHECK(kept.playQueue[0] == "p0");  // 先頭を捨てると再生位置が飛ぶ
  CHECK(kept.playQueue[1] == "p1");
}

TEST_CASE("★truncateForStore: バイト上限にも収まる（反復は有限回で止まる）") {
  const SchedulerSnapshot large = makeLarge(2000);
  SnapshotLimits limits{};
  limits.maxTotalBytes = 1024;

  const SchedulerSnapshot kept = truncateForStore(large, limits);
  CHECK(encodeSnapshot(kept).size() <= 1024);
  CHECK(kept.shown.size() < 2000);
  CHECK(decodeSnapshot(encodeSnapshot(kept)).has_value());
}

TEST_CASE("★Tab5 のNVS予算に設計値2000件は入らない") {
  // これが「上限を掛ける機構が要る」ことの実証。
  // MemoryStateStore を Tab5 の縮退モード相当（4KB）に設定する。
  StoreCapacity budget{};
  budget.totalBytes = ambient::core::kTab5ProvisionalLimits.maxTotalBytes;
  budget.maxValueBytes = ambient::core::kTab5ProvisionalLimits.maxTotalBytes;
  budget.maxKeyBytes = ambient::core::constants::kNvsMaxKeyBytes;
  MemoryStateStore store{budget};

  const SchedulerSnapshot full = makeLarge(2000);
  const std::string fullEncoded = encodeSnapshot(full);
  CHECK(fullEncoded.size() > budget.totalBytes);  // 約 18KB
  // ★write() の戻り値を必ず見ること。握り潰すと Tab5 で静かにデータを失う。
  CHECK(store.write(ambient::core::kSnapshotKey, fullEncoded) == StoreStatus::ValueTooLarge);

  // 切り詰めれば入る。
  const SchedulerSnapshot trimmed =
      truncateForStore(full, ambient::core::kTab5ProvisionalLimits);
  const std::string trimmedEncoded = encodeSnapshot(trimmed);
  CHECK(trimmed.shown.size() <= ambient::core::kTab5ProvisionalLimits.maxShownEntries);
  CHECK(trimmedEncoded.size() <= ambient::core::kTab5ProvisionalLimits.maxTotalBytes);
  CHECK(store.write(ambient::core::kSnapshotKey, trimmedEncoded) == StoreStatus::Ok);
  CHECK(store.flush() == StoreStatus::Ok);

  // 書いたものが読めて、復元できること。
  std::string readBack;
  CHECK(store.read(ambient::core::kSnapshotKey, readBack) == StoreStatus::Ok);
  CHECK(readBack == trimmedEncoded);
  CHECK(decodeSnapshot(readBack).has_value());
}

TEST_CASE("★Macの既定上限なら設計値2000件がそのまま入る") {
  const SchedulerSnapshot full = makeLarge(2000);
  const SchedulerSnapshot kept = truncateForStore(full, ambient::core::kMacDefaultLimits);
  CHECK(kept.shown.size() == 2000);  // 切り詰められない
  CHECK(encodeSnapshot(kept).size() <= ambient::core::kMacDefaultLimits.maxTotalBytes);
}

TEST_CASE("★上限を掛けたスナップショットでもスケジューラが壊れない") {
  // ヘッダの主張:「捨てられた写真は未表示 (PRI_NEVER) に戻るだけで飢餓は起きない。
  // 壊れるのは全網羅の周期である」。それを Mac 上で確認する。
  // これを示せなければ SnapshotLimits は無意味である。
  PhotoManifest manifest;
  for (int i = 0; i < 60; ++i) {
    PhotoItem item;
    item.id = "q" + std::to_string(i);
    item.trip = i / 15;  // 4 トリップ x 15 枚
    manifest.items.push_back(item);
  }
  manifest.rebuildIndex();

  SchedulerSnapshot state{};
  SchedulerRng rng{99};

  SnapshotLimits tiny{};
  tiny.maxShownEntries = 8;  // 60 件に対して極端に小さい

  std::vector<bool> seen(manifest.items.size(), false);
  int shownCount = 0;
  for (int step = 0; step < 600; ++step) {
    const auto index = ambient::core::peekNext(manifest, state, rng);
    if (!index) {
      break;
    }
    seen[*index] = true;
    ++shownCount;
    ambient::core::markShown(manifest, *index, state);
    // 毎回「保存 → 切り詰め → 復元」を挟む（実機の保存サイクルを模す）。
    const auto reloaded = decodeSnapshot(encodeSnapshot(truncateForStore(state, tiny)));
    REQUIRE(reloaded.has_value());
    state = *reloaded;
  }

  CHECK(shownCount == 600);  // 止まらない
  CHECK(state.shown.size() <= 8);
  // ★飢餓は起きない: 上限が極端でも全 60 枚が出る（周期が伸びるだけ）。
  int unseen = 0;
  for (const bool v : seen) {
    if (!v) {
      ++unseen;
    }
  }
  CHECK(unseen == 0);
}
