// ambient/core/state_store.hpp — 永続化の抽象
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
// ★Core が永続化に触ってよいのはこのインターフェース越しだけ。
//   <filesystem> は Core で禁止（移植性ゲートが exit 1 にする）。
//   Mac 側の実装は std::filesystem を使ってよいが、それは Core の外に置く。
//
// ============================================================================
// なぜ「容量に上限がある」ことをインターフェースに書くのか
// ============================================================================
//
// Mac 側の保存先はファイルで、実質無制限に書ける。
// Tab5 側の保存先は **NVS で、パーティションは 20KB しかない**。
// しかも同じ端末で動く hogo-meter が既に 4 つの namespace を使っている。
//
// 「無限に書ける」前提のインターフェースにすると、
//   - Mac では全部通る
//   - Tab5 で初めて書き込みが落ちる
// という、最終フェーズまで発覚しない失敗になる。移植の段階で設計をやり直す羽目になる。
//
// そこで次の 2 つをインターフェースに出す。
//
//   1. capacity()      使ってよい総量・現在の使用量・1 値あたり上限・キー長上限
//   2. StoreStatus     write() が OutOfSpace / ValueTooLarge で失敗しうることを
//                      戻り値で表現する（例外を制御フローに使わない）
//
// ★呼び出し側は write() の戻り値を必ず見ること。[[nodiscard]] を付けてある。
//   OutOfSpace を握り潰す実装を書いたら、それは Tab5 で静かにデータを失う。
//
// 容量超過への対処（何を捨てるか）は Core のポリシーであり、
// ambient/core/photo/snapshot.hpp の SnapshotLimits で表現する。

#pragma once

#include <cstddef>
#include <cstdint>
#include <map>
#include <string>
#include <string_view>

namespace ambient::core {

// 操作結果。例外は使わない（ESP32 は緊急用プールが 0 で、
// ヒープ枯渇時の throw が abort() になる。docs/PORTABILITY.md 参照）。
enum class StoreStatus : std::uint8_t {
  Ok = 0,
  NotFound,       // キーが存在しない（read / erase）
  OutOfSpace,     // 領域不足。★Tab5 で現実に起きる
  ValueTooLarge,  // 1 値あたり上限超過
  KeyTooLong,     // キー長上限超過（NVS は 15 文字）
  KeyInvalid,     // 空キーなど、そもそも受け付けられない形
  BackendError,   // I/O 失敗、破損など
};

[[nodiscard]] const char* toString(StoreStatus status) noexcept;

// この store の予算。
// ★Mac 実装も「無制限」ではなく現実的な値を入れること。
//   無制限にすると Tab5 との差が Mac 側のテストで一切見えなくなる。
struct StoreCapacity {
  std::size_t totalBytes{0};     // この store が使ってよい総バイト数
  std::size_t usedBytes{0};      // 現在の使用量（キー + 値の概算でよい）
  std::size_t maxValueBytes{0};  // 1 値あたりの上限
  std::size_t maxKeyBytes{0};    // キー長の上限（NVS: 15）

  [[nodiscard]] constexpr std::size_t freeBytes() const noexcept {
    return usedBytes >= totalBytes ? 0U : totalBytes - usedBytes;
  }
};

// ----------------------------------------------------------------------------
// IStateStore
// ----------------------------------------------------------------------------
//
// 値は「不透明なバイト列」。std::string を使うが、テキストとは限らない
// （NUL を含んでよい）。Core は中身の意味を知らない。
//
// キーは短く保つこと（NVS の上限 15 文字）。名前空間を "/" で切るような
// 長いキーは Tab5 で KeyTooLong になる。
class IStateStore {
 public:
  IStateStore() = default;
  virtual ~IStateStore() = default;
  IStateStore(const IStateStore&) = delete;
  IStateStore& operator=(const IStateStore&) = delete;
  IStateStore(IStateStore&&) = delete;
  IStateStore& operator=(IStateStore&&) = delete;

  // 予算の問い合わせ。書く前にこれを見て切り詰めるのが正しい使い方。
  [[nodiscard]] virtual StoreCapacity capacity() const noexcept = 0;

  // 読み出し。NotFound のとき out は変更しない。
  [[nodiscard]] virtual StoreStatus read(std::string_view key, std::string& out) const = 0;

  // 書き込み。★戻り値を必ず見ること。
  [[nodiscard]] virtual StoreStatus write(std::string_view key, std::string_view value) = 0;

  [[nodiscard]] virtual StoreStatus erase(std::string_view key) = 0;

  // 実際の媒体へ反映する。
  // NVS は nvs_commit() が必要で、これを省くと電源断で書き込みが消える。
  // ファイル実装では fsync 相当、または no-op でよい。
  // ★write() ごとに commit するとフラッシュ寿命を削る。まとめてから flush すること。
  [[nodiscard]] virtual StoreStatus flush() = 0;
};

// ----------------------------------------------------------------------------
// MemoryStateStore — テスト用の差し替え実装
// ----------------------------------------------------------------------------
//
// Core に置く理由: unit_tests は ambient_core だけにリンクする（層分離の検証）。
// 容量を明示的に設定できるので、**Mac 上で OutOfSpace 経路をテストできる**。
// これが無いと、容量制約はレビューでしか担保できない。
class MemoryStateStore final : public IStateStore {
 public:
  explicit MemoryStateStore(StoreCapacity budget) noexcept;

  [[nodiscard]] StoreCapacity capacity() const noexcept override;
  [[nodiscard]] StoreStatus read(std::string_view key, std::string& out) const override;
  [[nodiscard]] StoreStatus write(std::string_view key, std::string_view value) override;
  [[nodiscard]] StoreStatus erase(std::string_view key) override;
  [[nodiscard]] StoreStatus flush() override;

  // テスト用。flush() が呼ばれた回数。
  [[nodiscard]] std::size_t flushCount() const noexcept { return flushCount_; }

 private:
  [[nodiscard]] std::size_t usedBytes() const noexcept;

  StoreCapacity budget_;
  std::map<std::string, std::string, std::less<>> entries_;
  std::size_t flushCount_{0};
};

}  // namespace ambient::core
