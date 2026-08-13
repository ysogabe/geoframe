// state_store.cpp — MemoryStateStore（テスト用ダブル）の実装。
//
// 実機の IStateStore 実装:
//   Mac  : app / platform 層（std::filesystem を使ってよい。Core の外なので）
//   Tab5 : NVS（20KB パーティション。hogo-meter が既に 4 namespace 使用中）

#include "ambient/core/state_store.hpp"

namespace ambient::core {

const char* toString(StoreStatus status) noexcept {
  switch (status) {
    case StoreStatus::Ok:
      return "Ok";
    case StoreStatus::NotFound:
      return "NotFound";
    case StoreStatus::OutOfSpace:
      return "OutOfSpace";
    case StoreStatus::ValueTooLarge:
      return "ValueTooLarge";
    case StoreStatus::KeyTooLong:
      return "KeyTooLong";
    case StoreStatus::KeyInvalid:
      return "KeyInvalid";
    case StoreStatus::BackendError:
      return "BackendError";
  }
  return "?";
}

MemoryStateStore::MemoryStateStore(StoreCapacity budget) noexcept : budget_(budget) {}

std::size_t MemoryStateStore::usedBytes() const noexcept {
  std::size_t total = 0;
  for (const auto& [key, value] : entries_) {
    total += key.size() + value.size();
  }
  return total;
}

StoreCapacity MemoryStateStore::capacity() const noexcept {
  StoreCapacity cap = budget_;
  cap.usedBytes = usedBytes();
  return cap;
}

StoreStatus MemoryStateStore::read(std::string_view key, std::string& out) const {
  const auto it = entries_.find(key);
  if (it == entries_.end()) {
    return StoreStatus::NotFound;
  }
  out = it->second;
  return StoreStatus::Ok;
}

StoreStatus MemoryStateStore::write(std::string_view key, std::string_view value) {
  if (key.empty()) {
    return StoreStatus::KeyInvalid;
  }
  if (budget_.maxKeyBytes != 0 && key.size() > budget_.maxKeyBytes) {
    return StoreStatus::KeyTooLong;
  }
  if (budget_.maxValueBytes != 0 && value.size() > budget_.maxValueBytes) {
    return StoreStatus::ValueTooLarge;
  }

  // 上書きの場合、既存ぶんは解放されるので使用量から差し引いて判定する。
  std::size_t projected = usedBytes() + key.size() + value.size();
  if (const auto it = entries_.find(key); it != entries_.end()) {
    projected -= it->first.size() + it->second.size();
  }
  if (budget_.totalBytes != 0 && projected > budget_.totalBytes) {
    return StoreStatus::OutOfSpace;
  }

  entries_[std::string(key)] = std::string(value);
  return StoreStatus::Ok;
}

StoreStatus MemoryStateStore::erase(std::string_view key) {
  const auto it = entries_.find(key);
  if (it == entries_.end()) {
    return StoreStatus::NotFound;
  }
  entries_.erase(it);
  return StoreStatus::Ok;
}

StoreStatus MemoryStateStore::flush() {
  ++flushCount_;
  return StoreStatus::Ok;
}

}  // namespace ambient::core
