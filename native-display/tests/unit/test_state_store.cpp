// tests/unit/test_state_store.cpp — 永続化（レーン 1B）
//
// ★次フェーズでここに書くもの:
//   - スナップショットが Tab5 の予算に収まること
//     （MemoryStateStore を Tab5 相当の容量で作り、write が Ok を返すこと）
//   - 上限を掛けたあとでもスケジューラが壊れないこと
//   - decodeSnapshot が壊れた入力で nullopt を返すこと（例外を投げないこと）

#include <doctest.h>

#include <cstddef>
#include <string>

#include "ambient/core/constants.hpp"
#include "ambient/core/state_store.hpp"

using ambient::core::MemoryStateStore;
using ambient::core::StoreCapacity;
using ambient::core::StoreStatus;

namespace {

StoreCapacity smallBudget() {
  StoreCapacity cap{};
  cap.totalBytes = 64;
  cap.maxValueBytes = 32;
  cap.maxKeyBytes = ambient::core::constants::kNvsMaxKeyBytes;  // 15
  return cap;
}

}  // namespace

TEST_CASE("読み書きの往復") {
  MemoryStateStore store{smallBudget()};
  CHECK(store.write("k1", "hello") == StoreStatus::Ok);

  std::string out;
  CHECK(store.read("k1", out) == StoreStatus::Ok);
  CHECK(out == "hello");
}

TEST_CASE("未知のキーは NotFound（out を書き換えない）") {
  MemoryStateStore store{smallBudget()};
  std::string out = "untouched";
  CHECK(store.read("nope", out) == StoreStatus::NotFound);
  CHECK(out == "untouched");
}

TEST_CASE("★容量超過が戻り値で表現される（Tab5 の NVS 20KB 制約の縮図）") {
  MemoryStateStore store{smallBudget()};  // 総量 64 バイト

  CHECK(store.write("a", std::string(30, 'x')) == StoreStatus::Ok);
  CHECK(store.write("b", std::string(30, 'y')) == StoreStatus::Ok);
  // ここで 62 バイト。あと 2 バイトしか無い。
  CHECK(store.write("c", std::string(30, 'z')) == StoreStatus::OutOfSpace);
}

TEST_CASE("1 値あたりの上限") {
  MemoryStateStore store{smallBudget()};  // maxValueBytes = 32
  CHECK(store.write("k", std::string(33, 'x')) == StoreStatus::ValueTooLarge);
}

TEST_CASE("★NVS のキー長上限（15 文字）を超えると弾かれる") {
  MemoryStateStore store{smallBudget()};
  // 旧実装の 'geoframe.play.v1' は 16 文字。Tab5 では通らない。
  CHECK(store.write("geoframe.play.v1", "x") == StoreStatus::KeyTooLong);
  CHECK(store.write("gf.play.v1", "x") == StoreStatus::Ok);
}

TEST_CASE("空キーは受け付けない") {
  MemoryStateStore store{smallBudget()};
  CHECK(store.write("", "x") == StoreStatus::KeyInvalid);
}

TEST_CASE("上書きは使用量を増やさない") {
  MemoryStateStore store{smallBudget()};
  CHECK(store.write("a", std::string(30, 'x')) == StoreStatus::Ok);
  const std::size_t used1 = store.capacity().usedBytes;
  CHECK(store.write("a", std::string(30, 'y')) == StoreStatus::Ok);
  CHECK(store.capacity().usedBytes == used1);
}

TEST_CASE("erase と capacity") {
  MemoryStateStore store{smallBudget()};
  CHECK(store.write("a", "12345") == StoreStatus::Ok);
  CHECK(store.capacity().usedBytes == 6);  // key 1 + value 5
  CHECK(store.erase("a") == StoreStatus::Ok);
  CHECK(store.capacity().usedBytes == 0);
  CHECK(store.erase("a") == StoreStatus::NotFound);
}

TEST_CASE("freeBytes は下方向に飽和する") {
  StoreCapacity cap{};
  cap.totalBytes = 10;
  cap.usedBytes = 15;
  CHECK(cap.freeBytes() == 0);
}

TEST_CASE("flush が呼べる（NVS の nvs_commit に対応）") {
  MemoryStateStore store{smallBudget()};
  CHECK(store.flush() == StoreStatus::Ok);
  CHECK(store.flushCount() == 1);
}
