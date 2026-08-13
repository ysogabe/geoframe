// rng.cpp — Rng は完全にヘッダ内テンプレート。
//
// このファイルの役割は 2 つ。
//
//   1. 明示的インスタンス化により、テンプレート本体を実際にコード生成させる。
//      これにより ESP32-P4 移植性ゲート (tools/check-esp-portability.sh) が
//      Rng の中身までコンパイルする。ヘッダを include するだけの .cpp が
//      無いと、テンプレートは実体化されず「通ったつもり」になる。
//   2. アルゴリズム不変条件を static_assert で固定する。

#include "ambient/core/rng.hpp"

#include <type_traits>

namespace ambient::core {

// 明示的インスタンス化（用途ごとに 1 つずつ）。
template class Rng<rng_domain::Scheduler>;
template class Rng<rng_domain::Transition>;

namespace {

// xoshiro128** 1.1 の既知ベクタ相当の自己検査。
// 状態 {1,2,3,4} からの最初の 3 出力を固定する。値が変わったらエンジンが
// 差し替わったということであり、保存済みスナップショットとの互換が切れる。
constexpr bool engineIsStable() noexcept {
  RngState st{};
  st.s0 = 1U;
  st.s1 = 2U;
  st.s2 = 3U;
  st.s3 = 4U;
  SchedulerRng rng{st};
  const std::uint32_t a = rng.next();
  const std::uint32_t b = rng.next();
  const std::uint32_t c = rng.next();
  return a == 11520U && b == 0U && c == 5927040U;
}

static_assert(engineIsStable(),
              "xoshiro128** の出力が変わっている。エンジンを差し替えたなら "
              "RngState にバージョンを足し、移行を明示的に扱うこと。");

// unit() は 1.0 を返さない（24bit 仮数 x 2^-24 の上限は 1 - 2^-24）。
static_assert(static_cast<float>(0x00FFFFFFU) * (1.0F / 16777216.0F) < 1.0F,
              "unit() の上限が 1.0 未満であること");

// 用途ごとに別の型であること（取り違えがコンパイルエラーになること）の明示。
static_assert(!std::is_same_v<SchedulerRng, TransitionRng>,
              "SchedulerRng と TransitionRng は別の型でなければならない");

}  // namespace

}  // namespace ambient::core
