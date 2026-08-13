// tests/unit/test_rng.cpp — 乱数（レーン 1B）
//
// ############################################################################
// # ★次フェーズで **必ず** ここに書くもの: golden trace テスト
// #
// # 「Params を core 側で導出する」だけでは足りない。全消費者が
// # **同じ順序で・同じ回数** 引かなければ、Mac と Tab5 で列がずれる。
// # そしてこの違反は Mac 上のテストでは検出できない（Mac は自己整合するため）。
// #
// # 書くべきテスト:
// #   1. 既知 seed から belowN/unit を N 回引いた **値の列** を配列で固定する
// #      （Tab5 でも同じ列になることの根拠。ここが動いたら移植性が壊れている）
// #   2. buildPlayOrder() を 1 回呼んだあとの rng.draws() が
// #      (groups - 1) + 評価した写真数 と一致すること
// #   3. syncSchedule() の draws() 増分が新着枚数と一致すること
// #   4. pickTransition() の draws() 増分が常に 1 であること
// #      （重みが全 0 のときも 1 であること）
// #   5. save() / restore() の往復で以後の列が完全に一致すること
// #
// # 契約の本文は docs/CORE_CONTRACTS.md と
// # ambient/core/photo/scheduler.hpp の冒頭にある。
// ############################################################################

//
// 状況（1B）: 上記 1〜5 のうち
//   1 (値の列)          … このファイル下部の「★golden: ...」3 件
//   4 (pickTransition)  … このファイル末尾
//   2 / 3 / 5           … tests/unit/test_golden_trace.cpp（スケジューラ側の文脈が要るため）

#include <doctest.h>

#include <array>
#include <cstdint>

#include "ambient/core/rng.hpp"
#include "ambient/core/transition.hpp"

using ambient::core::RngState;
using ambient::core::SchedulerRng;
using ambient::core::TransitionRng;

TEST_CASE("belowN が [0, n) に収まる") {
  // ループ内で CHECK を撃つと assertion が数万件になり実行が遅くなるので、
  // 違反を集計してから 1 回だけ検査する。
  SchedulerRng rng{12345};
  int violations = 0;
  for (std::uint32_t n = 1; n <= 17; ++n) {
    for (int i = 0; i < 500; ++i) {
      if (rng.belowN(n) >= n) {
        ++violations;
      }
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("belowN(0) と belowN(1) は 0 を返す（UB を作らない）") {
  SchedulerRng rng{1};
  CHECK(rng.belowN(0) == 0U);
  CHECK(rng.belowN(1) == 0U);
}

TEST_CASE("unit が [0, 1) に収まり、1.0 を返さない") {
  SchedulerRng rng{99};
  float lo = 2.0F;
  float hi = -1.0F;
  for (int i = 0; i < 50000; ++i) {
    const float v = rng.unit();
    lo = v < lo ? v : lo;
    hi = v > hi ? v : hi;
  }
  CHECK(lo >= 0.0F);
  CHECK(hi < 1.0F);
  // 24bit 仮数なので上限は 1 - 2^-24 に張り付く
  CHECK(hi > 0.999F);
}

TEST_CASE("消費カウンタ: 論理呼び出しは棄却回数によらず 1 回ぶん") {
  SchedulerRng rng{7};
  const std::uint64_t before = rng.draws();

  (void)rng.belowN(3);  // 棄却が起きうる n
  CHECK(rng.draws() == before + 1);

  (void)rng.unit();
  CHECK(rng.draws() == before + 2);

  (void)rng.next();
  CHECK(rng.draws() == before + 3);

  // エンジン前進は論理呼び出し以上（棄却のぶん増えることがある）。
  CHECK(rng.steps() >= rng.draws());
}

TEST_CASE("同じ seed からは同じ列が出る") {
  SchedulerRng a{2026};
  SchedulerRng b{2026};
  bool allEqual = true;
  for (int i = 0; i < 200; ++i) {
    if (a.next() != b.next()) {
      allEqual = false;
    }
  }
  CHECK(allEqual);
}

TEST_CASE("違う seed からは違う列が出る") {
  SchedulerRng a{1};
  SchedulerRng b{2};
  bool differs = false;
  for (int i = 0; i < 32; ++i) {
    if (a.next() != b.next()) {
      differs = true;
    }
  }
  CHECK(differs);
}

TEST_CASE("save/restore の往復で以後の列が一致する（再起動をまたいだ再現性）") {
  SchedulerRng original{4242};
  for (int i = 0; i < 37; ++i) {
    (void)original.belowN(11);
  }

  const RngState saved = original.save();
  SchedulerRng restored{saved};

  CHECK(restored.draws() == original.draws());
  CHECK(restored.steps() == original.steps());
  CHECK(restored.save() == saved);

  bool allEqual = true;
  for (int i = 0; i < 100; ++i) {
    if (restored.next() != original.next()) {
      allEqual = false;
    }
  }
  CHECK(allEqual);
}

TEST_CASE("全ゼロ状態から復元しても縮退しない") {
  // 壊れたスナップショットを読んだ場合。xoshiro はゼロ状態から抜け出せないので、
  // 決定的な代替シードで作り直すことになっている。
  SchedulerRng rng{RngState{}};
  bool nonZeroSeen = false;
  for (int i = 0; i < 16; ++i) {
    if (rng.next() != 0U) {
      nonZeroSeen = true;
    }
  }
  CHECK(nonZeroSeen);
}

TEST_CASE("用途ごとに独立している（一方を引いても他方の列は動かない）") {
  // ★この独立性が契約の本体。共有すると一方の呼び出し回数の変更で
  //   もう一方の列が破壊される。型が違うので取り違えはコンパイルエラーになる。
  TransitionRng reference{555};
  std::array<std::uint32_t, 8> expected{};
  for (auto& v : expected) {
    v = reference.next();
  }

  SchedulerRng scheduler{555};
  TransitionRng transition{555};
  for (int i = 0; i < 50; ++i) {
    (void)scheduler.belowN(6);  // スケジューラ側をどれだけ引いても
  }
  bool unchanged = true;
  for (const std::uint32_t want : expected) {
    if (transition.next() != want) {  // トランジション側の列は不変
      unchanged = false;
    }
  }
  CHECK(unchanged);
}

// ############################################################################
// ★golden: エンジンそのものの値の列
//
// ここが動いたら「エンジン実装が変わった」ということであり、
// 保存済みスナップショットとの互換も Mac / Tab5 の一致も失われている。
//
// ★この 3 件は **エンジンの差し替えを検出できる唯一の網**である。
//   スケジューラ側の golden trace は draws/steps と ID 列を固定するが、
//   それらはエンジンが変わっても「同じ回数引いた」までしか言えない。
// ############################################################################

TEST_CASE("★golden: next() の値の列（xoshiro128** / SplitMix64 シード展開）") {
  SchedulerRng rng{20260730};
  static const std::array<std::uint32_t, 8> kExpected{
      1595230211U, 2816827480U, 3750099081U, 261020891U,
      2068532880U, 743109904U,  796224329U,  1449671901U};

  int mismatches = 0;
  for (const std::uint32_t want : kExpected) {
    if (rng.next() != want) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);
  CHECK(rng.draws() == 8);
  CHECK(rng.steps() == 8);
}

TEST_CASE("★golden: belowN(6) の値の列（棄却法込み）") {
  SchedulerRng rng{20260730};
  static const std::array<std::uint32_t, 8> kExpected{5U, 4U, 3U, 5U, 0U, 4U, 5U, 3U};

  int mismatches = 0;
  for (const std::uint32_t want : kExpected) {
    if (rng.belowN(6) != want) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);
  CHECK(rng.draws() == 8);
  // この seed のこの範囲では棄却が起きない。棄却が起きるようになったら
  // 閾値の計算か advance() が変わっている。
  CHECK(rng.steps() == 8);
}

TEST_CASE("★golden: unit() の値の列（24bit 仮数。ビット単位で一致すること）") {
  // ★小数として書かない。unit() = bits * 2^-24 なので 2^24 を掛けると
  //   binary32 で厳密に整数へ戻る。整数で比べれば十進表記の丸めに
  //   邪魔されず、Mac と Tab5 のビット一致をそのまま検査できる。
  SchedulerRng rng{20260730};
  static const std::array<std::uint32_t, 8> kExpected{6231368U, 11003232U, 14648824U, 1019612U,
                                                      8080206U, 2902773U,  3110251U,  5662780U};

  int mismatches = 0;
  for (const std::uint32_t want : kExpected) {
    const float value = rng.unit();
    if (static_cast<std::uint32_t>(value * 16777216.0F) != want) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);
  CHECK(rng.draws() == 8);
}

TEST_CASE("★golden: pickTransition は重みが全 0 でも必ず 1 回だけ引く") {
  // ★ここが崩れると「重みを 0 にしたら以降の乱数列がずれた」という事故になる。
  //   さらに TransitionRng と SchedulerRng が分かれていることの実証でもある。
  using ambient::core::pickTransition;
  using ambient::core::TransitionWeights;

  TransitionRng rng{20260730};

  const std::uint64_t before = rng.draws();
  (void)pickTransition(ambient::core::kDefaultTransitionWeights, rng);
  CHECK(rng.draws() == before + 1);

  const TransitionWeights allZero{{0, 0, 0, 0, 0}};
  const std::uint64_t beforeZero = rng.draws();
  (void)pickTransition(allZero, rng);
  CHECK(rng.draws() == beforeZero + 1);  // 0 回ではなく 1 回

  // 極端に偏った重みでも 1 回。
  const TransitionWeights skewed{{1000, 0, 0, 0, 0}};
  const std::uint64_t beforeSkewed = rng.draws();
  (void)pickTransition(skewed, rng);
  CHECK(rng.draws() == beforeSkewed + 1);
}

TEST_CASE("★pickTransition を挟んでも SchedulerRng の列は 1 ビットも動かない") {
  // 型が違うので取り違えはコンパイルエラーになるが、「インスタンスを
  // 共有していない」ことは型では保証できない。値で確認する。
  // ★これが崩れると「演出を 1 種類足しただけで写真の再生順が変わった」になる。
  using ambient::core::pickTransition;

  SchedulerRng reference{4242};
  std::array<std::uint32_t, 6> expected{};
  for (auto& v : expected) {
    v = reference.next();
  }

  SchedulerRng scheduler{4242};
  TransitionRng transition{4242};
  int mismatches = 0;
  for (const std::uint32_t want : expected) {
    // 写真 1 枚の表示につき pickTransition が 1 回引く、を模す。
    (void)pickTransition(ambient::core::kDefaultTransitionWeights, transition);
    if (scheduler.next() != want) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);
  CHECK(scheduler.draws() == expected.size());
  CHECK(transition.draws() == expected.size());
}

TEST_CASE("belowN の分布に剰余バイアスが無い（粗い検査）") {
  // n = 3 で 60000 回。完全一様なら各 20000。棄却法が効いていれば
  // 素朴な % による偏り（数百〜数千のずれ）は出ない。
  SchedulerRng rng{31337};
  std::array<int, 3> counts{0, 0, 0};
  constexpr int kTrials = 60000;
  for (int i = 0; i < kTrials; ++i) {
    counts[rng.belowN(3)] += 1;
  }
  for (const int c : counts) {
    CHECK(c > 19000);
    CHECK(c < 21000);
  }
}
