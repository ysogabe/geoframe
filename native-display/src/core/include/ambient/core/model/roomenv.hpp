// ambient/core/model/roomenv.hpp — /api/roomenv のデータ型（Home Assistant 由来）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★`ok:false` は障害ではない ― ここが最重要
// ============================================================================
//
// /api/roomenv は `ok:false` を **固定で返すことがある**。これは
// **Home Assistant が未設定である** ことを意味する。
//
//   ★障害ではない。「機能なし」である。
//   ★Degraded にしてはいけない。STALE 表示にしてもいけない。再試行を煽ってもいけない。
//
// HA を使っていない家庭では常にこの状態であり、そこで永続的に
// 「通信異常」を出す端末は壊れて見える。
//
// 型でこれを表現する。bool ok ではなく enum にしてあるのは、
// `if (!ok) { degrade(); }` という一行を書きにくくするため。

#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace ambient::core {

enum class RoomEnvAvailability : std::uint8_t {
  // ok:true。データがある。
  Available = 0,
  // ok:false。★Home Assistant 未設定 = 機能なし。障害ではない。
  // このときカット自体を出さない（Degraded にもしない）。
  NotConfigured,
};

// items のキー（実測）: entity / value / unit / class / name
struct RoomEnvItem {
  std::string entity;

  // ★value は **常に文字列** で返る。数値ではない。
  //   "23.4" のような値も文字列。数値が要るなら明示的に変換すること。
  std::string value;

  std::string unit;

  // JSON のキーは `class`（C++ の予約語なので識別子を変えてある）。
  std::string entityClass;

  std::string name;
};

struct RoomEnvSnapshot {
  RoomEnvAvailability availability{RoomEnvAvailability::NotConfigured};
  std::vector<RoomEnvItem> items;

  // 表示すべきか。NotConfigured なら false（障害表示ではなく「出さない」）。
  [[nodiscard]] bool displayable() const noexcept {
    return availability == RoomEnvAvailability::Available && !items.empty();
  }
};

}  // namespace ambient::core
