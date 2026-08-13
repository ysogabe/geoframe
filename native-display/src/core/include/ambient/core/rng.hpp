// ambient/core/rng.hpp — 決定的乱数（クロスプラットフォーム再現性の要）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜ標準の分布クラスを使わないのか
// ============================================================================
//
// 要件は「同じ seed なら Mac でも Tab5 でも同じ再生シーケンスになること」である。
//
//   ★ std::uniform_int_distribution / std::uniform_real_distribution などの
//     標準分布クラスを使ってはいけない。
//
// 規格はこれらの「分布」だけを定めていて、実装アルゴリズムを一切規定していない。
// したがって libstdc++ (Mac の Apple clang は libc++) と ESP32 の libstdc++ で
// 同じ seed から違う値が出る。これはバグではなく規格どおりの挙動であり、
// テストで捕まえられない（Mac 側では常に自己整合するため）。
// 発覚するのは Tab5 に載せた最終フェーズであり、そこで再設計する羽目になる。
//
// エンジンも同じ理由で自前にする。std::mt19937 は移植性こそあるが
// 状態が 2.5KB あり Tab5 の RAM / NVS 予算に対して重すぎる。
// ここでは xoshiro128** (状態 16 バイト) を採用し、実装をこのヘッダに固定する。
//
// ============================================================================
// 用途ごとに「別の型」にする理由
// ============================================================================
//
// 乱数を引くのはトランジション選択だけではない。写真スケジューラも引く
// （優先度のゆらぎ / トリップ順の Fisher-Yates シャッフル / 新着の差し込み位置）。
//
// 1 つのインスタンスを共有すると、**一方の呼び出し回数を変えただけで
// もう一方の乱数列が丸ごとずれる**。「トランジションを 1 種類増やした」だけで
// 写真の再生順が変わる、という原因追跡の困難な結合が生まれる。
//
// そこで phantom type で型レベルに分ける。SchedulerRng と TransitionRng は
// **別の型**なので、取り違えて渡すとコンパイルエラーになる。
//
//   void pick(TransitionRng&);
//   SchedulerRng s{1};
//   pick(s);   // ← コンパイルエラー。これが狙い。
//
// ============================================================================
// 消費カウンタ ― golden trace テストのための契約
// ============================================================================
//
// 「誰が・何回・どの順で引くか」は仕様である。実装の自由ではない。
// これを機械的に検証できるよう、Rng は 2 つのカウンタを持つ。
//
//   draws()  論理呼び出し回数。belowN() / unit() / next() を 1 回呼ぶと +1。
//            belowN() が棄却で内部的に何回エンジンを回しても +1 のまま。
//            ★仕様に書くのはこちら。「buildPlayOrder は draws を N 増やす」。
//   steps()  エンジン前進回数。棄却のぶんも数える。
//            エンジン実装が変わっていないことの検出に使う。
//
// 消費順序の契約は docs/CORE_CONTRACTS.md に集約する。
// 順序を変えるときは、契約文書と golden trace テストを同時に直すこと。
//
// ============================================================================
// 状態のシリアライズ
// ============================================================================
//
// 再起動をまたいで再生シーケンスを続けるため、RngState は
// SchedulerSnapshot（ambient/core/photo/snapshot.hpp）に含めて永続化する。
// save() / restore() で往復すること。

#pragma once

#include <bit>
#include <cstdint>

namespace ambient::core {

// ----------------------------------------------------------------------------
// シリアライズ可能な状態
// ----------------------------------------------------------------------------

// xoshiro128** の内部状態 + 消費カウンタ。
// これをそのまま永続化すれば、再起動後も同じ乱数列が続く。
struct RngState {
  std::uint32_t s0{0};
  std::uint32_t s1{0};
  std::uint32_t s2{0};
  std::uint32_t s3{0};
  std::uint64_t draws{0};  // 論理呼び出し回数（棄却は数えない）
  std::uint64_t steps{0};  // エンジン前進回数（棄却も数える）

  [[nodiscard]] bool operator==(const RngState&) const noexcept = default;
};

// ----------------------------------------------------------------------------
// 用途タグ（phantom type）
// ----------------------------------------------------------------------------
//
// 新しい用途が増えたら、ここにタグを足して using を 1 行足す。
// 既存の用途と Rng を共有してはいけない。共有した瞬間に上記の結合が生まれる。
namespace rng_domain {

// 写真スケジューラ用。優先度ゆらぎ / トリップシャッフル / 新着差し込み位置。
struct Scheduler {};

// トランジション選択用。演出の重み付き抽選。
struct Transition {};

}  // namespace rng_domain

// ----------------------------------------------------------------------------
// Rng
// ----------------------------------------------------------------------------
//
// アルゴリズム: xoshiro128** 1.1 (David Blackman / Sebastiano Vigna, public domain)
// シード展開 : SplitMix64
//
// ★このアルゴリズムは「仕様」である。差し替えると保存済みスナップショットとの
//   互換が切れ、Mac と Tab5 で列が食い違う。差し替えるなら RngState に
//   バージョン番号を足し、移行を明示的に扱うこと。
template <class Domain>
class Rng {
 public:
  using domain_type = Domain;

  // 全ビットが 0 の状態を復元してしまったときの縮退回避に使う固定シード。
  static constexpr std::uint64_t kFallbackSeed = 0x9E3779B97F4A7C15ULL;

  // seed からの決定的な初期化。同じ seed なら Mac / Tab5 で同じ列になる。
  explicit constexpr Rng(std::uint64_t seed) noexcept { seedFrom(seed); }

  // 永続化した状態からの復元。
  explicit constexpr Rng(const RngState& state) noexcept : st_(state) { normalize(); }

  // ---- 引く ----------------------------------------------------------------

  // 32bit の生の乱数。論理カウンタ +1。
  constexpr std::uint32_t next() noexcept {
    ++st_.draws;
    return advance();
  }

  // [0, n) の一様整数。
  //
  // ★剰余バイアスを棄却法で除去している。`next() % n` と書いてはいけない。
  //   2^32 が n で割り切れない限り小さい値が出やすくなり、
  //   Fisher-Yates シャッフルの一様性が壊れる。
  //
  // 前提: n >= 1。n == 0 は契約違反だが、UB を作らないよう 0 を返す。
  // 論理カウンタは棄却回数によらず必ず +1（仕様に書けるようにするため）。
  constexpr std::uint32_t belowN(std::uint32_t n) noexcept {
    ++st_.draws;
    if (n <= 1U) {
      return 0U;
    }
    // 2^32 mod n。これ未満の値を棄却すると [threshold, 2^32) が n の倍数長になり、
    // % n が完全に一様になる。
    const std::uint32_t threshold = (0xFFFFFFFFU - n + 1U) % n;
    std::uint32_t r = advance();
    while (r < threshold) {
      r = advance();
    }
    return r % n;
  }

  // [0.0, 1.0) の float。
  //
  // ★24bit 仮数を使う。float の仮数は 24bit（暗黙の 1 を含む）なので、
  //   32bit をそのまま割ると下位ビットが丸めで消え、値の分布に段差が出る。
  //   上位 24bit を取って 2^-24 を掛ければ、すべての結果が float で厳密に表現でき、
  //   Mac と Tab5 で完全に同じビット列になる（両者とも IEEE 754 binary32）。
  //   1.0 は決して返さない。
  constexpr float unit() noexcept {
    ++st_.draws;
    const std::uint32_t bits = advance() >> 8;  // 上位 24bit
    return static_cast<float>(bits) * (1.0F / 16777216.0F);
  }

  // ---- 消費カウンタ --------------------------------------------------------

  // 論理呼び出し回数。golden trace テストで「引いた回数」を固定するのに使う。
  [[nodiscard]] constexpr std::uint64_t draws() const noexcept { return st_.draws; }

  // エンジン前進回数。棄却のぶんも含む。エンジン実装の変化検出用。
  [[nodiscard]] constexpr std::uint64_t steps() const noexcept { return st_.steps; }

  // ---- 永続化 --------------------------------------------------------------

  [[nodiscard]] constexpr const RngState& save() const noexcept { return st_; }

  constexpr void restore(const RngState& state) noexcept {
    st_ = state;
    normalize();
  }

 private:
  // xoshiro128** はゼロ状態から抜け出せないので、復元値が全ゼロなら作り直す。
  // カウンタは維持する（「何回引いたか」の履歴は壊さない）。
  constexpr void normalize() noexcept {
    if ((st_.s0 | st_.s1 | st_.s2 | st_.s3) == 0U) {
      spread(kFallbackSeed);
    }
  }

  constexpr void seedFrom(std::uint64_t seed) noexcept {
    spread(seed);
    st_.draws = 0;
    st_.steps = 0;
  }

  // SplitMix64 で 64bit seed を 4 x 32bit へ展開する。
  // seed が 0 でも良質な初期状態になるのが SplitMix64 を挟む理由。
  constexpr void spread(std::uint64_t seed) noexcept {
    std::uint64_t x = seed;
    const std::uint64_t a = splitmix64(x);
    const std::uint64_t b = splitmix64(x);
    st_.s0 = static_cast<std::uint32_t>(a);
    st_.s1 = static_cast<std::uint32_t>(a >> 32);
    st_.s2 = static_cast<std::uint32_t>(b);
    st_.s3 = static_cast<std::uint32_t>(b >> 32);
    if ((st_.s0 | st_.s1 | st_.s2 | st_.s3) == 0U) {
      st_.s0 = 1U;  // 天文学的に起きないが、ゼロ状態だけは必ず避ける
    }
  }

  static constexpr std::uint64_t splitmix64(std::uint64_t& x) noexcept {
    x += 0x9E3779B97F4A7C15ULL;
    std::uint64_t z = x;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
  }

  // xoshiro128** の 1 ステップ。
  constexpr std::uint32_t advance() noexcept {
    ++st_.steps;
    const std::uint32_t result = std::rotl(st_.s1 * 5U, 7) * 9U;
    const std::uint32_t t = st_.s1 << 9U;
    st_.s2 ^= st_.s0;
    st_.s3 ^= st_.s1;
    st_.s1 ^= st_.s2;
    st_.s0 ^= st_.s3;
    st_.s2 ^= t;
    st_.s3 = std::rotl(st_.s3, 11);
    return result;
  }

  RngState st_{};
};

// ----------------------------------------------------------------------------
// 用途ごとの型（★この 2 つは互いに代入も変換もできない）
// ----------------------------------------------------------------------------

using SchedulerRng = Rng<rng_domain::Scheduler>;
using TransitionRng = Rng<rng_domain::Transition>;

}  // namespace ambient::core
