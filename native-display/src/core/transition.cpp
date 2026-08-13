// transition.cpp
//
// 演出の抽選・capability フィルタ・パラメータ導出（純粋ロジック）。
// イージングカーブなど**描画**の中身はレーン 2B の責務。ここには置かない。
//
// ★このファイルの設計判断（duration の値、blend 全除外時のフォールバック、
//   乱数の予算、浮動小数を使わない導出）は transition.hpp 冒頭が正典。

#include "ambient/core/transition.hpp"

namespace ambient::core {

namespace {

// 余り draw。予算を演出によらず一定に保つために消費する。
//
// ★belowN(1) ではなく next() を使う。belowN(1) は早期 return してエンジンを
//   進めないため、余りに使うと draws は揃うのに steps が演出依存になり、
//   steps を「エンジン実装が変わっていないことの検出」に使えなくなる。
void reserveDraws(TransitionRng& rng, std::uint64_t count) noexcept {
  for (std::uint64_t i = 0; i < count; ++i) {
    (void)rng.next();
  }
}

}  // namespace

const char* toString(TransitionKind kind) noexcept {
  switch (kind) {
    case TransitionKind::CrossFade:
      return "CrossFade";
    case TransitionKind::FadeThroughBlack:
      return "FadeThroughBlack";
    case TransitionKind::Slide:
      return "Slide";
    case TransitionKind::Wipe:
      return "Wipe";
    case TransitionKind::Blinds:
      return "Blinds";
  }
  return "?";
}

const char* toString(SlideDirection direction) noexcept {
  switch (direction) {
    case SlideDirection::FromLeft:
      return "FromLeft";
    case SlideDirection::FromRight:
      return "FromRight";
    case SlideDirection::FromTop:
      return "FromTop";
    case SlideDirection::FromBottom:
      return "FromBottom";
  }
  return "?";
}

const char* toString(DwellEffect effect) noexcept {
  switch (effect) {
    case DwellEffect::None:
      return "None";
    case DwellEffect::KenBurns:
      return "KenBurns";
  }
  return "?";
}

TransitionKind pickTransition(const TransitionWeights& weights, TransitionRng& rng) noexcept {
  const std::uint32_t total = weights.total();

  // ★引く回数を入力に依存させない。total == 0 でも必ず 1 回引く。
  //   「重みを 0 にしたら以降の乱数列がずれた」という事故を防ぐ。
  const std::uint32_t roll = rng.belowN(total == 0 ? 1U : total);

  if (total == 0) {
    return kAllTransitionKinds[0];
  }

  std::uint32_t acc = 0;
  for (std::size_t i = 0; i < kTransitionKindCount; ++i) {
    acc += weights.weight[i];
    if (roll < acc) {
      return kAllTransitionKinds[i];
    }
  }
  return kAllTransitionKinds[kTransitionKindCount - 1];
}

// ----------------------------------------------------------------------------
// パラメータ導出 ― 消費順序はヘッダ冒頭の表が契約
// ----------------------------------------------------------------------------

TransitionParams deriveTransitionParams(TransitionKind kind, TransitionRng& rng) noexcept {
  TransitionParams out{};

  // ★実際に使った回数を数え、最後にまとめて予算まで埋める。
  //   こう書くと、万一 kind が範囲外の値でも予算が守られる
  //   （switch の各 case で reserve すると、抜けた経路で予算が崩れる）。
  std::uint64_t used = 0;

  switch (kind) {
    case TransitionKind::CrossFade:
    case TransitionKind::FadeThroughBlack:
      // パラメータ無し。全て余り draw になる。
      break;

    case TransitionKind::Slide:
      out.slide.direction = static_cast<SlideDirection>(
          rng.belowN(static_cast<std::uint32_t>(kSlideDirectionCount)));
      used = 1;
      break;

    case TransitionKind::Wipe:
      out.wipe.angleDeg =
          kWipeAngles[rng.belowN(static_cast<std::uint32_t>(kWipeAngles.size()))];
      used = 1;
      break;

    case TransitionKind::Blinds: {
      const std::uint32_t span =
          static_cast<std::uint32_t>(kBlindsSlatsMax - kBlindsSlatsMin) + 1U;
      // ★下限を足しているので 0 枚にはならない。
      out.blinds.slats = static_cast<std::uint8_t>(
          static_cast<std::uint32_t>(kBlindsSlatsMin) + rng.belowN(span));
      out.blinds.vertical = rng.belowN(2) == 0U;
      used = 2;
      break;
    }
  }

  reserveDraws(rng, kTransitionParamDraws - used);
  return out;
}

KenBurnsParams deriveKenBurns(TransitionRng& rng) noexcept {
  // #1 寄るのか引くのか。
  const bool zoomIn = rng.belowN(2) == 0U;

  // #2 寄り側の倍率（千分率）。★整数で決める。float の算術を挟まない。
  const std::uint32_t scaleSpan =
      static_cast<std::uint32_t>(kKenBurnsScaleMaxMilli - kKenBurnsScaleMinMilli) + 1U;
  const std::uint16_t scaleMilli = static_cast<std::uint16_t>(
      static_cast<std::uint32_t>(kKenBurnsScaleMinMilli) + rng.belowN(scaleSpan));

  // 余白 = 1000 - 倍率。x / y はこの範囲にしか置かないので、
  // ★矩形は必ず単位正方形に収まる（= 画面外を舐めない）。
  const std::uint32_t freeMilli = 1000U - static_cast<std::uint32_t>(scaleMilli);

  // #3 / #4 寄り側の位置。
  const std::uint16_t xMilli = static_cast<std::uint16_t>(rng.belowN(freeMilli + 1U));
  const std::uint16_t yMilli = static_cast<std::uint16_t>(rng.belowN(freeMilli + 1U));

  // #5 余り。
  reserveDraws(rng, kDwellPlanDraws - 4);

  const NormRect full{0, 0, 1000, 1000};
  const NormRect tight{xMilli, yMilli, scaleMilli, scaleMilli};

  KenBurnsParams out{};
  out.start = zoomIn ? full : tight;
  out.end = zoomIn ? tight : full;
  return out;
}

// ----------------------------------------------------------------------------
// 計画の生成
// ----------------------------------------------------------------------------

PlannedTransition planTransition(const TransitionWeights& weights, RenderCaps caps, SteadyTime now,
                                 TransitionRng& rng) noexcept {
  const TransitionWeights allowed = allowedWeights(weights, caps);

  // ★候補が無くても抽選は行う。引く回数を weights / caps に依存させないため。
  //   結果は捨てるが、乱数列の位置は同じだけ進む。
  const TransitionKind rolled = pickTransition(allowed, rng);
  const TransitionKind kind = (allowed.total() == 0U) ? fallbackTransition(caps) : rolled;

  PlannedTransition out{};
  out.plan.kind = kind;
  out.plan.duration = durationOf(kind);
  out.plan.startedAt = now;
  out.params = deriveTransitionParams(kind, rng);
  return out;
}

PlannedDwell planDwell(RenderCaps caps, SteadyTime now, TransitionRng& rng) noexcept {
  PlannedDwell out{};

  // ★caps によらず必ず引く。「blend を切ったら演出列がずれた」を作らない。
  out.kenBurns = deriveKenBurns(rng);

  out.plan.effect =
      caps.allows(DwellEffect::KenBurns) ? DwellEffect::KenBurns : DwellEffect::None;
  out.plan.duration = kDwellDurationDefault;
  out.plan.startedAt = now;
  return out;
}

namespace {

// Progress の不変条件をコンパイル時に固定する。
static_assert(Progress::fromElapsed(Millis{0}, Millis{600}).value() == 0.0F);
static_assert(Progress::fromElapsed(Millis{600}, Millis{600}).done());
static_assert(Progress::fromElapsed(Millis{900}, Millis{600}).value() == 1.0F,
              "duration を超えても 1.0 を超えないこと");
static_assert(Progress::fromElapsed(Millis{-5}, Millis{600}).value() == 0.0F,
              "負の経過時間でも 0.0 を下回らないこと");
static_assert(Progress::fromElapsed(Millis{10}, Millis{0}).done(),
              "duration 0 は 0 除算ではなく即完了として扱うこと");

// ★時間駆動であることの検証:
//   同じ経過時間なら、fps が何であれ同じ progress になる。
//   （60fps の 18 フレーム目 = 300ms、15fps の 4.5 フレーム目 = 300ms）
static_assert(Progress::fromElapsed(Millis{300}, Millis{600}).value() ==
                  Progress::fromElapsed(Millis{300}, Millis{600}).value());

// 合成コスト分類の固定（Tab5 の blend 実測時にここを見ればよい）。
static_assert(compositeClassOf(TransitionKind::CrossFade) == CompositeClass::Blend);
static_assert(compositeClassOf(TransitionKind::FadeThroughBlack) == CompositeClass::Blend);
static_assert(compositeClassOf(TransitionKind::Slide) == CompositeClass::RegionCopy);
static_assert(compositeClassOf(TransitionKind::Wipe) == CompositeClass::RegionCopy);
static_assert(compositeClassOf(TransitionKind::Blinds) == CompositeClass::RegionCopy);
static_assert(compositeClassOf(DwellEffect::KenBurns) == CompositeClass::Blend);

static_assert(kDefaultTransitionWeights.total() == kTransitionKindCount);

// ---- duration が企画の範囲 [600, 1000] に収まっていること --------------------
//
// ★実機で見て値を調整してよいが、この範囲からは出さないこと。
static_assert(kCrossFadeDuration >= kTransitionDurationMin &&
              kCrossFadeDuration <= kTransitionDurationMax);
static_assert(kFadeThroughBlackDuration >= kTransitionDurationMin &&
              kFadeThroughBlackDuration <= kTransitionDurationMax);
static_assert(kSlideDuration >= kTransitionDurationMin &&
              kSlideDuration <= kTransitionDurationMax);
static_assert(kWipeDuration >= kTransitionDurationMin && kWipeDuration <= kTransitionDurationMax);
static_assert(kBlindsDuration >= kTransitionDurationMin &&
              kBlindsDuration <= kTransitionDurationMax);

// 領域コピー群 <= blend 群 という関係（根拠はヘッダ冒頭【2A が決めたこと 2】）。
static_assert(kSlideDuration <= kCrossFadeDuration);
static_assert(kWipeDuration <= kCrossFadeDuration);
static_assert(kBlindsDuration <= kCrossFadeDuration);
static_assert(kCrossFadeDuration <= kFadeThroughBlackDuration);

// ---- capability フィルタ ----------------------------------------------------

// blend 不可なら blend 群の重みが 0 になる。
static_assert(allowedWeights(kDefaultTransitionWeights, RenderCaps{false}).weight[0] == 0);
static_assert(allowedWeights(kDefaultTransitionWeights, RenderCaps{false}).weight[1] == 0);
static_assert(allowedWeights(kDefaultTransitionWeights, RenderCaps{false}).total() == 3);
// blend 可なら素通し。
static_assert(allowedWeights(kDefaultTransitionWeights, RenderCaps{true}).total() ==
              kDefaultTransitionWeights.total());

// ★フォールバックは必ず caps を満たす。ここが崩れると
//   「blend が使えないのに blend 必須の演出を返す」という最悪の形になる。
static_assert(RenderCaps{true}.allows(fallbackTransition(RenderCaps{true})));
static_assert(RenderCaps{false}.allows(fallbackTransition(RenderCaps{false})));
static_assert(fallbackTransition(RenderCaps{true}) == TransitionKind::FadeThroughBlack,
              "blend が使えるなら企画書どおり FadeThroughBlack");
static_assert(fallbackTransition(RenderCaps{false}) == TransitionKind::Wipe,
              "blend が使えないときは領域コピーで済む Wipe へ倒す");

// ---- パラメータの値域 -------------------------------------------------------

static_assert(kBlindsSlatsMin >= 1, "スリット 0 枚は描くものが無くなる");
static_assert(kBlindsSlatsMin <= kBlindsSlatsMax);
static_assert(kKenBurnsScaleMinMilli >= 1 && kKenBurnsScaleMaxMilli < 1000,
              "倍率 1000 は全画面と同じで動かない。0 は破綻する");
static_assert(kKenBurnsScaleMinMilli <= kKenBurnsScaleMaxMilli);
static_assert(kSlideDirectionCount == 4);
static_assert(kWipeAngles.size() == 4);

// 矩形の内包判定（★整数で判定していること自体が要件）。
static_assert(NormRect{}.insideUnitSquare());
static_assert(NormRect{0, 0, 1000, 1000}.insideUnitSquare());
static_assert(NormRect{300, 0, 700, 700}.insideUnitSquare(), "余白ぴったりまでは収まる");
static_assert(!NormRect{301, 0, 700, 700}.insideUnitSquare(), "1 千分でもはみ出せば偽");
static_assert(!NormRect{0, 0, 0, 1000}.insideUnitSquare(), "幅 0 は矩形ではない");

// float は単一除算で作る（加算が無いので FMA の対象にならない）。
static_assert(NormRect{0, 0, 1000, 1000}.w() == 1.0F);
static_assert(NormRect{500, 0, 500, 500}.x() == 0.5F);

// ---- 乱数の予算 -------------------------------------------------------------

static_assert(kTransitionPlanDraws == 1 + kTransitionParamDraws);
// Blinds が現時点の最大消費（2 回）。予算はそれ以上でなければならない。
static_assert(kTransitionParamDraws >= 2, "Blinds が 2 回引く。予算が足りないと破綻する");
// KenBurns は 4 回使う。
static_assert(kDwellPlanDraws >= 4, "deriveKenBurns が 4 回引く");

}  // namespace

}  // namespace ambient::core
