// tests/unit/test_transition.cpp — 遷移と滞在（レーン 2A）
//
// ############################################################################
// # このファイルが守っているもの
// #
// # 要件は「同じ seed なら Mac でも Tab5 でも同じ演出になること」。
// # そのためには **誰が・何回・どの順で引くか** が固定されていなければならない。
// # 1 箇所でも回数が変わると以降の乱数列が丸ごとずれる。
// #
// # ★この違反は Mac 上のテストでは絶対に検出できない。Mac 側は常に自己整合する。
// #   発覚するのは Tab5 に載せた最終フェーズになる。
// #
// # そこでここでは 3 段構えで固定する。
// #   (a) 予算の不変条件 — planTransition は weights にも caps にも依存せず
// #       常に kTransitionPlanDraws 回引く（仕様から導いた期待値で検査する）
// #   (b) draws / steps の絶対値 — 実測値の固定
// #   (c) 出てくる演出とパラメータの列 — 実装差の最終検出網
// #
// # ★ここが落ちたときの直し方:
// #   期待値を書き換えて緑に戻すのは **最後の手段**。まず「消費順序を変えて
// #   よい変更だったのか」を疑う。変えてよいなら transition.hpp 冒頭の表・
// #   docs/CORE_CONTRACTS.md §2.3・このファイルを 3 つ同時に直すこと。
// #
// # 消費順序の仕様（transition.hpp 冒頭が正典）:
// #   pickTransition          : belowN 1 回
// #   deriveTransitionParams  : 演出によらず 3 回
// #     CrossFade/FadeThroughBlack : 余り next() x3
// #     Slide  : belowN(4)=方向 / 余り x2
// #     Wipe   : belowN(4)=角度index / 余り x2
// #     Blinds : belowN(7)=枚数 / belowN(2)=向き / 余り x1
// #   planTransition          : 1 + 3 = 4 回
// #   deriveKenBurns/planDwell: belowN(2), belowN(201), belowN(余白+1) x2, 余り = 5 回
// ############################################################################

#include <doctest.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <string>
#include <type_traits>

#include "ambient/core/clock.hpp"
#include "ambient/core/rng.hpp"
#include "ambient/core/transition.hpp"

using ambient::core::allowedWeights;
using ambient::core::CompositeClass;
using ambient::core::deriveKenBurns;
using ambient::core::deriveTransitionParams;
using ambient::core::durationOf;
using ambient::core::DwellEffect;
using ambient::core::fallbackTransition;
using ambient::core::kAllTransitionKinds;
using ambient::core::kBlindsSlatsMax;
using ambient::core::kBlindsSlatsMin;
using ambient::core::kDefaultTransitionWeights;
using ambient::core::kDwellPlanDraws;
using ambient::core::KenBurnsParams;
using ambient::core::kKenBurnsScaleMaxMilli;
using ambient::core::kKenBurnsScaleMinMilli;
using ambient::core::kTransitionKindCount;
using ambient::core::kTransitionParamDraws;
using ambient::core::kTransitionPlanDraws;
using ambient::core::kWipeAngles;
using ambient::core::Millis;
using ambient::core::NormRect;
using ambient::core::pickTransition;
using ambient::core::PlannedDwell;
using ambient::core::PlannedTransition;
using ambient::core::planDwell;
using ambient::core::planTransition;
using ambient::core::Progress;
using ambient::core::RenderCaps;
using ambient::core::SteadyTime;
using ambient::core::TransitionKind;
using ambient::core::TransitionPlan;
using ambient::core::TransitionRng;
using ambient::core::TransitionWeights;

namespace {

// 演出 + パラメータの短い署名。golden 比較を 1 回の CHECK で済ませるため。
std::string signature(const PlannedTransition& pt) {
  std::string s = ambient::core::toString(pt.plan.kind);
  s += ':';
  s += std::to_string(pt.plan.duration.value);
  switch (pt.plan.kind) {
    case TransitionKind::Slide:
      s += ":d";
      s += std::to_string(static_cast<int>(pt.params.slide.direction));
      break;
    case TransitionKind::Wipe:
      s += ":a";
      s += std::to_string(static_cast<int>(pt.params.wipe.angleDeg));
      break;
    case TransitionKind::Blinds:
      s += ':';
      s += std::to_string(static_cast<int>(pt.params.blinds.slats));
      s += pt.params.blinds.vertical ? "v" : "h";
      break;
    default:
      break;
  }
  return s;
}

std::string sequenceOf(std::uint64_t seed, const TransitionWeights& weights, RenderCaps caps,
                       int count) {
  TransitionRng rng{seed};
  std::string out;
  for (int i = 0; i < count; ++i) {
    if (i != 0) {
      out += '|';
    }
    out += signature(planTransition(weights, caps, SteadyTime{1000}, rng));
  }
  return out;
}

std::string rectToString(const NormRect& r) {
  std::string s = "(";
  s += std::to_string(r.xMilli);
  s += ',';
  s += std::to_string(r.yMilli);
  s += ',';
  s += std::to_string(r.wMilli);
  s += ',';
  s += std::to_string(r.hMilli);
  s += ')';
  return s;
}

std::string dwellSequenceOf(std::uint64_t seed, RenderCaps caps, int count) {
  TransitionRng rng{seed};
  std::string out;
  for (int i = 0; i < count; ++i) {
    if (i != 0) {
      out += '|';
    }
    const PlannedDwell pd = planDwell(caps, SteadyTime{0}, rng);
    out += ambient::core::toString(pd.plan.effect);
    out += rectToString(pd.kenBurns.start);
    out += ">";
    out += rectToString(pd.kenBurns.end);
  }
  return out;
}

// 検査に使う重みテーブルの一覧（極端 / 一部ゼロ / 全ゼロを含む）。
const std::array<TransitionWeights, 6> kWeightMatrix{{
    kDefaultTransitionWeights,              // 均等
    TransitionWeights{{1000, 1, 1, 1, 1}},  // 極端
    TransitionWeights{{0, 1, 1, 1, 1}},     // 一部ゼロ
    TransitionWeights{{3, 2, 0, 0, 0}},     // blend 群だけ
    TransitionWeights{{0, 0, 0, 0, 7}},     // 領域コピー 1 種だけ
    TransitionWeights{{0, 0, 0, 0, 0}},     // 全ゼロ
}};

}  // namespace

// ============================================================================
// Progress ― 時間駆動であること
// ============================================================================

TEST_CASE("★進捗は時間駆動: fps が違っても同じ経過時間なら同じ進捗") {
  TransitionPlan plan{};
  plan.kind = TransitionKind::CrossFade;
  plan.duration = Millis{600};
  plan.startedAt = SteadyTime{1000};

  // 60fps で 18 フレーム目 = 300ms
  const float at60 = plan.progressAt(SteadyTime{1000 + 300}).value();
  // 15fps で 4.5 フレーム目 = 同じ 300ms
  const float at15 = plan.progressAt(SteadyTime{1000 + 300}).value();

  CHECK(at60 == at15);
  CHECK(at60 == doctest::Approx(0.5));

  // ★どちらも 600ms で終わる。低 fps でも間延びしない。
  CHECK(plan.progressAt(SteadyTime{1000 + 600}).done());
}

TEST_CASE("進捗は 0..1 にクランプされる") {
  CHECK(Progress::fromElapsed(Millis{-100}, Millis{600}).value() == 0.0F);
  CHECK(Progress::fromElapsed(Millis{99999}, Millis{600}).value() == 1.0F);
  CHECK(Progress::fromElapsed(Millis{10}, Millis{0}).done());
}

TEST_CASE("進捗の境界: duration 0 / elapsed 0 / 負の duration") {
  // duration <= 0 は 0 除算ではなく即完了。
  CHECK(Progress::fromElapsed(Millis{0}, Millis{0}).value() == 1.0F);
  CHECK(Progress::fromElapsed(Millis{-1}, Millis{0}).done());
  CHECK(Progress::fromElapsed(Millis{5}, Millis{-100}).done());

  // 開始直後は 0.0。既定構築が無いので「未開始」と混同されない。
  CHECK(Progress::fromElapsed(Millis{0}, Millis{600}).value() == 0.0F);
  CHECK_FALSE(Progress::fromElapsed(Millis{0}, Millis{600}).done());

  // 実際に使う 5 種の duration すべてで [0,1] に収まること。
  int outOfRange = 0;
  for (const TransitionKind kind : kAllTransitionKinds) {
    const Millis d = durationOf(kind);
    for (std::int64_t ms = -50; ms <= d.value + 50; ms += 25) {
      const float v = Progress::fromElapsed(Millis{ms}, d).value();
      if (v < 0.0F || v > 1.0F) {
        ++outOfRange;
      }
    }
  }
  CHECK(outOfRange == 0);
}

TEST_CASE("遷移と滞在は別の型（同じ progress に押し込まない）") {
  ambient::core::DwellPlan dwell{};
  dwell.effect = DwellEffect::KenBurns;
  dwell.duration = Millis{20000};
  dwell.startedAt = SteadyTime{0};

  TransitionPlan trans{};
  trans.kind = TransitionKind::Slide;
  trans.duration = Millis{800};
  trans.startedAt = SteadyTime{0};

  // 同じ時刻でも意味が違う: 遷移はもう終わり、滞在はまだ 4%
  CHECK(trans.progressAt(SteadyTime{800}).done());
  CHECK_FALSE(dwell.progressAt(SteadyTime{800}).done());
}

// ============================================================================
// ★TransitionKind と DwellEffect が混ざらないこと
// ============================================================================

TEST_CASE("★DwellEffect と TransitionKind は互いに変換できない別の型") {
  // enum class 同士なので暗黙変換は無い。「同じ enum に押し込む」リファクタが
  // 入ったらここが落ちる。
  static_assert(!std::is_same_v<TransitionKind, DwellEffect>);
  static_assert(!std::is_convertible_v<TransitionKind, DwellEffect>);
  static_assert(!std::is_convertible_v<DwellEffect, TransitionKind>);

  // 計画の型も分かれている。TransitionPlan は kind を、DwellPlan は effect を持つ。
  static_assert(std::is_same_v<decltype(PlannedTransition{}.plan.kind), TransitionKind>);
  static_assert(std::is_same_v<decltype(PlannedDwell{}.plan.effect), DwellEffect>);

  // planTransition が返す種別は必ず 5 種の遷移のいずれか（KenBurns は出てこない）。
  TransitionRng rng{31337};
  int notATransition = 0;
  for (int i = 0; i < 500; ++i) {
    const PlannedTransition pt =
        planTransition(kDefaultTransitionWeights, RenderCaps{true}, SteadyTime{0}, rng);
    bool found = false;
    for (const TransitionKind k : kAllTransitionKinds) {
      if (k == pt.plan.kind) {
        found = true;
      }
    }
    if (!found) {
      ++notATransition;
    }
  }
  CHECK(notATransition == 0);
}

// ============================================================================
// 合成コスト分類と capability フィルタ
// ============================================================================

TEST_CASE("合成コスト分類（Tab5 の blend 実測で重みを調整するための軸）") {
  CHECK(ambient::core::compositeClassOf(TransitionKind::CrossFade) == CompositeClass::Blend);
  CHECK(ambient::core::compositeClassOf(TransitionKind::FadeThroughBlack) ==
        CompositeClass::Blend);
  CHECK(ambient::core::compositeClassOf(TransitionKind::Slide) == CompositeClass::RegionCopy);
  CHECK(ambient::core::compositeClassOf(TransitionKind::Wipe) == CompositeClass::RegionCopy);
  CHECK(ambient::core::compositeClassOf(TransitionKind::Blinds) == CompositeClass::RegionCopy);
  CHECK(ambient::core::compositeClassOf(DwellEffect::KenBurns) == CompositeClass::Blend);
}

TEST_CASE("★blend が使えないとき blend 群は選ばれない") {
  const RenderCaps noBlend{false};

  // 重みは blend 群にも入っているが、caps 側で除外される。
  CHECK(allowedWeights(kDefaultTransitionWeights, noBlend).weight[0] == 0);
  CHECK(allowedWeights(kDefaultTransitionWeights, noBlend).weight[1] == 0);
  CHECK(allowedWeights(kDefaultTransitionWeights, noBlend).total() == 3);

  TransitionRng rng{2026};
  int blendPicked = 0;
  for (int i = 0; i < 2000; ++i) {
    const PlannedTransition pt =
        planTransition(kDefaultTransitionWeights, noBlend, SteadyTime{0}, rng);
    if (!noBlend.allows(pt.plan.kind)) {
      ++blendPicked;
    }
  }
  CHECK(blendPicked == 0);
}

TEST_CASE("★どの重み × どの caps でも、返る演出は必ず caps を満たす") {
  int violations = 0;
  for (const bool blend : {true, false}) {
    const RenderCaps caps{blend};
    for (const TransitionWeights& w : kWeightMatrix) {
      TransitionRng rng{555};
      for (int i = 0; i < 200; ++i) {
        const PlannedTransition pt = planTransition(w, caps, SteadyTime{0}, rng);
        if (!caps.allows(pt.plan.kind)) {
          ++violations;
        }
      }
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("★候補が全滅したときのフォールバック（企画書の矛盾の解消）") {
  // 企画書は「フォールバックは FadeThroughBlack」だが、それ自体が blend 群である。
  // よって caps の関数にした。詳細は transition.hpp 冒頭【2A が決めたこと 1】。
  CHECK(fallbackTransition(RenderCaps{true}) == TransitionKind::FadeThroughBlack);
  CHECK(fallbackTransition(RenderCaps{false}) == TransitionKind::Wipe);

  // (a) blend 群だけに重みがあり、blend が使えない → 候補ゼロ → Wipe
  const TransitionWeights blendOnly{{3, 2, 0, 0, 0}};
  {
    TransitionRng rng{4242};
    const PlannedTransition pt = planTransition(blendOnly, RenderCaps{false}, SteadyTime{0}, rng);
    CHECK(pt.plan.kind == TransitionKind::Wipe);
    CHECK(RenderCaps{false}.allows(pt.plan.kind));
  }

  // (b) 重みが全てゼロ、blend は使える → FadeThroughBlack
  const TransitionWeights allZero{{0, 0, 0, 0, 0}};
  {
    TransitionRng rng{4242};
    const PlannedTransition pt = planTransition(allZero, RenderCaps{true}, SteadyTime{0}, rng);
    CHECK(pt.plan.kind == TransitionKind::FadeThroughBlack);
  }

  // (c) 重みが全てゼロ、blend も使えない → Wipe
  {
    TransitionRng rng{4242};
    const PlannedTransition pt = planTransition(allZero, RenderCaps{false}, SteadyTime{0}, rng);
    CHECK(pt.plan.kind == TransitionKind::Wipe);
  }
}

// ============================================================================
// 重み付き抽選
// ============================================================================

TEST_CASE("★pickTransition は必ず乱数を 1 回だけ引く（重みが 0 でも）") {
  TransitionRng rng{1};

  const std::uint64_t before = rng.draws();
  (void)pickTransition(kDefaultTransitionWeights, rng);
  CHECK(rng.draws() == before + 1);

  // 重みが全て 0 でも引く回数は変わらない。
  // （変わると「重みを 0 にしたら以降の乱数列がずれた」という事故になる）
  const TransitionWeights zero{{0, 0, 0, 0, 0}};
  const std::uint64_t before2 = rng.draws();
  (void)pickTransition(zero, rng);
  CHECK(rng.draws() == before2 + 1);
}

TEST_CASE("重み 0 の演出は選ばれない（重みテーブルだけで頻度を制御できる）") {
  // blend 群 (CrossFade / FadeThroughBlack) の重みを 0 にする。
  // Tab5 で blend が重いと分かったときの操作そのもの。
  const TransitionWeights regionCopyOnly{{0, 0, 1, 1, 1}};
  TransitionRng rng{2026};

  int blendPicked = 0;
  for (int i = 0; i < 300; ++i) {
    const TransitionKind kind = pickTransition(regionCopyOnly, rng);
    if (ambient::core::compositeClassOf(kind) != CompositeClass::RegionCopy) {
      ++blendPicked;
    }
  }
  CHECK(blendPicked == 0);
}

TEST_CASE("均等な重みなら全種類が出る") {
  TransitionRng rng{7};
  std::array<int, kTransitionKindCount> counts{};
  for (int i = 0; i < 2000; ++i) {
    const TransitionKind kind = pickTransition(kDefaultTransitionWeights, rng);
    counts[static_cast<std::size_t>(kind)] += 1;
  }
  for (const int c : counts) {
    CHECK(c > 0);
  }
}

TEST_CASE("極端な重みは抽選に反映される") {
  // CrossFade だけ 1000、他は 1 → 1000/1004 ≒ 99.6%
  const TransitionWeights lopsided{{1000, 1, 1, 1, 1}};
  TransitionRng rng{909};
  int crossFade = 0;
  const int kTrials = 4000;
  for (int i = 0; i < kTrials; ++i) {
    if (pickTransition(lopsided, rng) == TransitionKind::CrossFade) {
      ++crossFade;
    }
  }
  // 期待 3984 件。裾を広めに取って偶然の失敗を避ける。
  CHECK(crossFade > 3900);
  CHECK(crossFade <= kTrials);

  // 単独の非ゼロ重みなら必ずそれが出る。
  const TransitionWeights onlyBlinds{{0, 0, 0, 0, 7}};
  int notBlinds = 0;
  for (int i = 0; i < 500; ++i) {
    if (pickTransition(onlyBlinds, rng) != TransitionKind::Blinds) {
      ++notBlinds;
    }
  }
  CHECK(notBlinds == 0);
}

// ============================================================================
// ★乱数の予算 ― これが「blend を切ったら列がずれた」を原理的に防ぐ
// ============================================================================

TEST_CASE("★planTransition の draws は weights にも caps にも依存せず常に一定") {
  int wrongDelta = 0;
  int iterations = 0;
  for (const bool blend : {true, false}) {
    const RenderCaps caps{blend};
    for (const TransitionWeights& w : kWeightMatrix) {
      TransitionRng rng{20260730};
      for (int i = 0; i < 50; ++i) {
        const std::uint64_t before = rng.draws();
        (void)planTransition(w, caps, SteadyTime{0}, rng);
        if (rng.draws() - before != kTransitionPlanDraws) {
          ++wrongDelta;
        }
        ++iterations;
      }
    }
  }
  CHECK(iterations == 600);
  CHECK(wrongDelta == 0);

  // ★同じ回数だけ引くので、weights / caps が違っても **乱数列の位置は揃う**。
  //   （出てくる演出は違ってよい。ずれてはいけないのは位置のほう）
  TransitionRng a{777};
  TransitionRng b{777};
  for (int i = 0; i < 30; ++i) {
    (void)planTransition(kDefaultTransitionWeights, RenderCaps{true}, SteadyTime{0}, a);
    (void)planTransition(TransitionWeights{{9, 0, 0, 0, 1}}, RenderCaps{false}, SteadyTime{0}, b);
  }
  CHECK(a.draws() == b.draws());
  CHECK(a.draws() == 30 * kTransitionPlanDraws);
}

TEST_CASE("★deriveTransitionParams は演出によらず常に kTransitionParamDraws 回引く") {
  int wrongDelta = 0;
  for (const TransitionKind kind : kAllTransitionKinds) {
    TransitionRng rng{1234};
    for (int i = 0; i < 20; ++i) {
      const std::uint64_t before = rng.draws();
      (void)deriveTransitionParams(kind, rng);
      if (rng.draws() - before != kTransitionParamDraws) {
        ++wrongDelta;
      }
    }
  }
  CHECK(wrongDelta == 0);

  // 予算は現在の最大消費（Blinds の 2 回）以上でなければならない。
  CHECK(kTransitionParamDraws >= 2);
  CHECK(kTransitionPlanDraws == 1 + kTransitionParamDraws);
}

TEST_CASE("★planDwell の draws は caps に依存せず常に kDwellPlanDraws 回") {
  int wrongDelta = 0;
  for (const bool blend : {true, false}) {
    TransitionRng rng{31415};
    for (int i = 0; i < 50; ++i) {
      const std::uint64_t before = rng.draws();
      (void)planDwell(RenderCaps{blend}, SteadyTime{0}, rng);
      if (rng.draws() - before != kDwellPlanDraws) {
        ++wrongDelta;
      }
    }
  }
  CHECK(wrongDelta == 0);

  // blend が使えなければ効果は None。ただし引く回数は同じ。
  TransitionRng rng{5};
  CHECK(planDwell(RenderCaps{false}, SteadyTime{0}, rng).plan.effect == DwellEffect::None);
  CHECK(planDwell(RenderCaps{true}, SteadyTime{0}, rng).plan.effect == DwellEffect::KenBurns);
}

// ============================================================================
// ★golden ― 同じ seed から同じ演出列・同じ Params
// ============================================================================

TEST_CASE("★同一 seed の 2 インスタンスは完全に同じ列を出す") {
  // 実装が変わってもこれは成立し続けるべき性質（絶対値の固定とは別物）。
  const std::string a = sequenceOf(4242, kDefaultTransitionWeights, RenderCaps{true}, 30);
  const std::string b = sequenceOf(4242, kDefaultTransitionWeights, RenderCaps{true}, 30);
  CHECK(a == b);

  const std::string da = dwellSequenceOf(99, RenderCaps{true}, 20);
  const std::string db = dwellSequenceOf(99, RenderCaps{true}, 20);
  CHECK(da == db);

  // seed が違えば列も違う（固定値を返しているだけではないことの確認）。
  CHECK(a != sequenceOf(4243, kDefaultTransitionWeights, RenderCaps{true}, 30));
}

// ★golden 列は「余り draw を含めて何回引くか」に紐づいている。予算を変えると
//   演出は同じでも乱数列の位置がずれ、以降の要素が丸ごと変わる。
//   予算を変えるときは、下の golden 3 本をすべて再生成すること。
static_assert(kTransitionParamDraws == 3,
              "golden 列はこの予算に紐づく。変えるなら golden を 3 本とも再生成すること");
static_assert(kDwellPlanDraws == 5,
              "KenBurns の golden 列はこの予算に紐づく。変えるなら再生成すること");

TEST_CASE("★golden: seed 4242 / 均等重み / blend 可 の演出列") {
  // ★この期待値は実装出力をそのまま貼ったものではなく、transition.hpp 冒頭の
  //   消費仕様を手で辿って先頭 3 件を照合したうえで固定している。
  //     [0] belowN(5)=0 -> 累積(CrossFade)=1 を初めて超える -> CrossFade
  //                        パラメータ無しなので余り next() x3
  //     [1] belowN(5)=4 -> 累積 5 で初めて超える            -> Blinds
  //         belowN(7)=5 -> 4+5=9 枚 / belowN(2)=1 -> vertical=false（"h"）
  //     [2] belowN(5)=4 -> Blinds / belowN(7)=3 -> 7 枚 / belowN(2)=0 -> "v"
  const std::string expected =
      "CrossFade:900|Blinds:800:9h|Blinds:800:7v|CrossFade:900|CrossFade:900|"
      "FadeThroughBlack:1000|FadeThroughBlack:1000|FadeThroughBlack:1000|Blinds:800:6v|"
      "CrossFade:900|Wipe:700:a180|Blinds:800:4h";
  CHECK(sequenceOf(4242, kDefaultTransitionWeights, RenderCaps{true}, 12) == expected);
}

TEST_CASE("★golden: seed 4242 / 均等重み / blend 不可 の演出列") {
  // 同じ seed でも caps が違えば列は変わる（当然）。変わってはいけないのは
  // **引く回数**であって、選ばれる演出ではない。
  //     [0] 許可後の重みは {0,0,1,1,1} で total=3。belowN(3)=1 -> Wipe
  //         belowN(4)=0 -> kWipeAngles[0] = 0 度
  const std::string expected =
      "Wipe:700:a0|Blinds:800:9h|Wipe:700:a270|Wipe:700:a180|Blinds:800:9h|Blinds:800:6v|"
      "Blinds:800:7v|Wipe:700:a0|Wipe:700:a90|Blinds:800:4v|Blinds:800:7h|Blinds:800:4h";
  CHECK(sequenceOf(4242, kDefaultTransitionWeights, RenderCaps{false}, 12) == expected);
}

TEST_CASE("★golden: KenBurns の矩形列（seed 99）") {
  //     [0] belowN(2)=1 -> zoomIn=false（寄りから全画面へ引く）
  //         belowN(201)=199 -> 倍率 899 / 余白 101 なので belowN(102) x2 -> x=37, y=9
  const std::string expected =
      "KenBurns(37,9,899,899)>(0,0,1000,1000)|"
      "KenBurns(41,13,898,898)>(0,0,1000,1000)|"
      "KenBurns(11,104,870,870)>(0,0,1000,1000)|"
      "KenBurns(0,0,1000,1000)>(45,67,830,830)|"
      "KenBurns(123,114,833,833)>(0,0,1000,1000)";
  CHECK(dwellSequenceOf(99, RenderCaps{true}, 5) == expected);
}

TEST_CASE("★golden: draws / steps の絶対値") {
  // draws は **仕様**（transition.hpp 冒頭の表から導ける）。
  // steps は **エンジン実装が変わっていないことの検出用**であって仕様ではない。
  // ここが落ちて draws が合っているなら、xoshiro128** の実装か belowN の
  // 棄却条件が変わったということ。契約違反ではないので、まずそちらを疑うこと。
  {
    TransitionRng rng{4242};
    for (int i = 0; i < 12; ++i) {
      (void)planTransition(kDefaultTransitionWeights, RenderCaps{true}, SteadyTime{0}, rng);
    }
    CHECK(rng.draws() == 12 * kTransitionPlanDraws);  // 48（仕様から導ける）
    CHECK(rng.steps() == 48);                         // 実測（棄却が起きていない）
  }
  {
    // ★候補ゼロの経路だけは steps が 1 少ない。belowN(1) が早期 return して
    //   エンジンを進めないため。**draws は変わらない**ので仕様は守られている。
    const TransitionWeights allZero{{0, 0, 0, 0, 0}};
    TransitionRng rng{4242};
    for (int i = 0; i < 4; ++i) {
      (void)planTransition(allZero, RenderCaps{true}, SteadyTime{0}, rng);
    }
    CHECK(rng.draws() == 4 * kTransitionPlanDraws);  // 16（仕様から導ける）
    CHECK(rng.steps() == 12);                        // 実測
  }
  {
    TransitionRng rng{99};
    for (int i = 0; i < 5; ++i) {
      (void)planDwell(RenderCaps{true}, SteadyTime{0}, rng);
    }
    CHECK(rng.draws() == 5 * kDwellPlanDraws);  // 25（仕様から導ける）
    CHECK(rng.steps() == 25);                   // 実測
  }
}

// ============================================================================
// パラメータの値域
// ============================================================================

TEST_CASE("★Slide / Wipe / Blinds のパラメータが値域に収まる") {
  int badSlide = 0;
  int badWipe = 0;
  int badSlats = 0;

  for (std::uint64_t seed = 1; seed <= 40; ++seed) {
    TransitionRng rng{seed};
    for (int i = 0; i < 60; ++i) {
      const PlannedTransition pt =
          planTransition(kDefaultTransitionWeights, RenderCaps{true}, SteadyTime{0}, rng);
      switch (pt.plan.kind) {
        case TransitionKind::Slide:
          if (static_cast<std::size_t>(pt.params.slide.direction) >=
              ambient::core::kSlideDirectionCount) {
            ++badSlide;
          }
          break;
        case TransitionKind::Wipe: {
          bool known = false;
          for (const std::uint16_t a : kWipeAngles) {
            if (a == pt.params.wipe.angleDeg) {
              known = true;
            }
          }
          if (!known) {
            ++badWipe;
          }
          break;
        }
        case TransitionKind::Blinds:
          // ★0 枚は「描くものが無い」= 画面が固まる。絶対に起きてはいけない。
          if (pt.params.blinds.slats < kBlindsSlatsMin ||
              pt.params.blinds.slats > kBlindsSlatsMax) {
            ++badSlats;
          }
          break;
        default:
          break;
      }
    }
  }
  CHECK(badSlide == 0);
  CHECK(badWipe == 0);
  CHECK(badSlats == 0);
  CHECK(kBlindsSlatsMin >= 1);
}

TEST_CASE("★KenBurns の矩形は必ず画面内（単位正方形）に収まる") {
  int outside = 0;
  int notMoving = 0;
  int badScale = 0;
  int aspectChanged = 0;

  for (std::uint64_t seed = 1; seed <= 200; ++seed) {
    TransitionRng rng{seed};
    for (int i = 0; i < 20; ++i) {
      const KenBurnsParams kb = deriveKenBurns(rng);
      if (!kb.start.insideUnitSquare() || !kb.end.insideUnitSquare()) {
        ++outside;
      }
      if (!kb.moves()) {
        ++notMoving;
      }
      // ★w == h であること。写真は画面にフィット済みなので、正規化座標で
      //   w == h なら画面と同じアスペクトになる（Core は画素数を知らない）。
      if (kb.start.wMilli != kb.start.hMilli || kb.end.wMilli != kb.end.hMilli) {
        ++aspectChanged;
      }
      // 片方は全画面、もう片方は寄り。
      const NormRect& tight = (kb.start.wMilli < kb.end.wMilli) ? kb.start : kb.end;
      const NormRect& full = (kb.start.wMilli < kb.end.wMilli) ? kb.end : kb.start;
      if (tight.wMilli < kKenBurnsScaleMinMilli || tight.wMilli > kKenBurnsScaleMaxMilli ||
          full.wMilli != 1000 || full.xMilli != 0 || full.yMilli != 0) {
        ++badScale;
      }
    }
  }
  CHECK(outside == 0);
  CHECK(notMoving == 0);
  CHECK(badScale == 0);
  CHECK(aspectChanged == 0);
}

TEST_CASE("NormRect の float は単一除算で作る（加算を挟まない）") {
  // ★x() + w() <= 1.0F と書くと浮動小数の加算が入り、FMA 契約の差で
  //   Mac と Tab5 のビットが食い違いうる。内包判定は整数で行う。
  const NormRect r{250, 125, 750, 750};
  CHECK(r.x() == 0.25F);
  CHECK(r.y() == 0.125F);
  CHECK(r.w() == 0.75F);
  CHECK(r.insideUnitSquare());
  CHECK_FALSE(NormRect{251, 0, 750, 750}.insideUnitSquare());
  CHECK_FALSE(NormRect{0, 0, 0, 500}.insideUnitSquare());
}

// ============================================================================
// duration
// ============================================================================

TEST_CASE("★各演出の duration は企画の範囲 [600,1000] に収まり、計画に反映される") {
  int outOfRange = 0;
  for (const TransitionKind kind : kAllTransitionKinds) {
    const Millis d = durationOf(kind);
    if (d < ambient::core::kTransitionDurationMin || d > ambient::core::kTransitionDurationMax) {
      ++outOfRange;
    }
  }
  CHECK(outOfRange == 0);

  // 領域コピー群は blend 群より短い（根拠は transition.hpp 冒頭）。
  CHECK(durationOf(TransitionKind::Slide) == Millis{700});
  CHECK(durationOf(TransitionKind::Wipe) == Millis{700});
  CHECK(durationOf(TransitionKind::Blinds) == Millis{800});
  CHECK(durationOf(TransitionKind::CrossFade) == Millis{900});
  CHECK(durationOf(TransitionKind::FadeThroughBlack) == Millis{1000});

  // 計画に載る duration / startedAt は入力どおりになる。
  TransitionRng rng{2468};
  int mismatch = 0;
  for (int i = 0; i < 300; ++i) {
    const PlannedTransition pt =
        planTransition(kDefaultTransitionWeights, RenderCaps{true}, SteadyTime{4000}, rng);
    if (pt.plan.duration != durationOf(pt.plan.kind)) {
      ++mismatch;
    }
    if (pt.plan.startedAt != SteadyTime{4000}) {
      ++mismatch;
    }
  }
  CHECK(mismatch == 0);
}

TEST_CASE("★Tab5 の低 fps でも duration どおりに終わる（時間駆動の再確認）") {
  // 15fps = 約 67ms/frame。Slide の 700ms なら 10 フレーム強。
  // 何フレームで刻もうと、経過 700ms で完了する。
  TransitionPlan plan{};
  plan.kind = TransitionKind::Slide;
  plan.duration = durationOf(TransitionKind::Slide);
  plan.startedAt = SteadyTime{0};

  int wrong = 0;
  for (const std::int64_t frameMs : {17, 33, 67, 100}) {  // 60/30/15/10 fps
    std::int64_t t = 0;
    while (t < plan.duration.value) {
      if (plan.progressAt(SteadyTime{t}).done()) {
        ++wrong;  // duration 前に完了してはいけない
      }
      t += frameMs;
    }
    if (!plan.progressAt(SteadyTime{plan.duration.value}).done()) {
      ++wrong;  // duration ちょうどで完了していなければいけない
    }
  }
  CHECK(wrong == 0);
}
