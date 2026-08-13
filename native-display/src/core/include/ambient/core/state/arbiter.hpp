// ambient/core/state/arbiter.hpp — 優先度アービター（純粋関数）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜ decide() を純粋関数にするのか
// ============================================================================
//
// claim の集合と現在時刻だけを受け取り、次の表示状態を返す。
// **副作用なし、メンバ状態なし、隠れた入力なし。**
//
// これでテストが決定的になる。「claim をこう並べたらこれが勝つ」を
// 表で書けるようになり、7 段の優先順位を網羅的に検証できる。
//
// 状態を持つ層（履歴・遷移の記録）は StateMachine に分ける。
// 混ぜると「前回の呼び出し結果に依存する」テストになり、
// 優先順位のバグが再現しにくくなる。

#pragma once

#include <cstdint>
#include <optional>
#include <span>

#include "ambient/core/clock.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/display_state.hpp"

namespace ambient::core {

// 勝った claim の要約。
struct Decision {
  DisplayStateKind kind{DisplayStateKind::Photo};
  ClaimPriority priority{ClaimPriority::Photo};
  std::uint32_t sourceId{0};
  std::uint32_t payloadId{0};

  [[nodiscard]] bool operator==(const Decision&) const noexcept = default;
};

// 次に表示すべきものを決める。
//
// ★純粋関数。claims と now 以外を見ない。何度呼んでも同じ答えを返す。
//
// 選択規則:
//   1. activeAt(now) が false の claim は候補から外す
//      （notBefore 前 / expiresAt 後）
//   2. 残りのうち priority が最大のものが勝つ
//   3. ★同一優先度のときは **span 内で先に現れたもの** が勝つ（安定選択）
//      これは契約である。呼び出し側が claim を集める順序で結果が決まるので、
//      集める順序（source の登録順）を安定させること。
//      「同点なら sourceId が小さい方」のような暗黙規則を後から足さないこと。
//
// 戻り値:
//   nullopt = 有効な claim が 1 つも無い。
//   ★通常は起きない。写真の source は常に claim を出す前提だからである。
//     nullopt を受け取ったときにどうするか（フォールバック表示）は
//     StateMachine 側のポリシーであり、decide() の責務ではない。
//     ここで既定値を返してしまうと「claim が無い」という異常が見えなくなる。
[[nodiscard]] std::optional<Decision> decide(std::span<const Claim> claims,
                                             SteadyTime now) noexcept;

}  // namespace ambient::core
