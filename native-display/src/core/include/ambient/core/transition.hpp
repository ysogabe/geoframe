// ambient/core/transition.hpp — 遷移演出と滞在効果
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜ TransitionKind と DwellEffect を別の型にするのか
// ============================================================================
//
// 5 種の演出は 600〜1000ms の **遷移** である（写真 A から写真 B へ移る間だけ存在する）。
// KenBurns は約 20 秒の **滞在** 効果である（1 枚の写真を表示している間ずっと動く）。
//
// 同じ enum に押し込んで同じ progress で駆動すると、
// **種別ごとに progress の意味が変わる**。
//   CrossFade の progress=0.5 → A と B が半々
//   KenBurns  の progress=0.5 → 1 枚の写真のズーム途中
// 消費側（Renderer）は必ず全種別の分岐を持つ羽目になり、
// 新しい演出を足すたびに分岐が増える。
//
// 別の型にすれば、Renderer は「遷移を描く関数」と「滞在を描く関数」に分かれる。
// 同時に両方が動くこともある（KenBurns しながら CrossFade で抜ける）ので、
// そもそも直交した概念である。
//
// ============================================================================
// なぜ進捗を「経過時間」からしか作れなくするのか ← 最重要級の設計判断
// ============================================================================
//
// 最終ターゲットの Tab5 は Mac より遥かに低い fps でしか描けない可能性が高い。
//
// フレーム数駆動（「30 フレームかけてフェード」）で書くと:
//   Mac  60fps → 0.5 秒で完了
//   Tab5 15fps → 2.0 秒かかる。演出が間延びし、写真の滞在時間もずれる
//
// 時間駆動（progress = elapsed / duration）なら:
//   Mac  60fps → 同じカーブを密にサンプル
//   Tab5 15fps → 同じカーブを疎にサンプル
// **どちらも 600ms で終わる。** カーブの形も同じ。fps は見た目の滑らかさにしか効かない。
//
// これを規約ではなく型で強制する。Progress は
//   Progress::fromElapsed(Millis elapsed, Millis duration)
// からしか作れない。既定構築も、float からの構築も、整数からの構築も無い。
// フレーム番号を渡す経路が **存在しない**。
//
// ★「フレーム 12 / 全 30 フレーム」を Millis に詰め替えれば通ってしまうが、
//   それは Millis{} と明示的に書く必要があり、レビューで必ず目に入る。
//   善意で Progress に別のファクトリを足さないこと。
//
// ============================================================================
// なぜ blend 群と領域コピー群にタグ分けするのか
// ============================================================================
//
// Tab5 のハードウェアアクセラレータの blend 性能が **未測定** である。
//   Blend が必要      : CrossFade / FadeThroughBlack / KenBurns
//   領域コピーで済む  : Slide / Wipe / Blinds
//
// 実測して blend が重いと分かったら、**重みテーブルの数値だけ書き換えて**
// blend 群の出現頻度を下げられるようにしておく。コードは変えない。
// 重みは TransitionWeights として設定値に出してある。
//
// ★重みを下げるのは「遅い」への対処である。「使えない」への対処は別に要る。
//   RenderCaps がそれで、blend 群を抽選から**除外**する（重み 0 と同じ効果を
//   設定ではなく能力から導く）。両者は独立に効く。
//
// ============================================================================
// 【2A が決めたこと 1】blend が使えないときのフォールバック ― 企画書の矛盾の解消
// ============================================================================
//
// 企画書は「フォールバックは FadeThroughBlack」としている。ところが
// FadeThroughBlack 自身が blend 群である。blend が使えない状況で
// blend 必須の演出へ逃げることはできない。企画書のこの記述は
// **blend が常に使える前提で書かれている**。
//
// 解消: フォールバックを **caps の関数**にする。
//
//   blend 使用可 : FadeThroughBlack （企画書どおり。黒を経由するので
//                  2 枚が同時に見えず、どんな写真の組でも破綻しない）
//   blend 使用不可: Wipe
//
// なぜ Wipe か（Slide / Blinds ではなく）:
//   フォールバックは「他が全部だめだったときに通る道」なので、
//   **前提が最も少ないもの**を選ぶ。
//     Wipe   : A は動かさない。毎フレーム「新しく現れた軸平行の帯」を 1 枚
//              転送するだけで成立する。パラメータで重くなりようがない。
//     Slide  : 2 枚を毎フレーム動かす。画面外へはみ出す転送元矩形が要る。
//     Blinds : スリット枚数 N に比例して転送回数が増える。
//
// ★却下した案: FadeThroughBlack を「黒への一括塗り + カット」へ劣化させて
//   blend 無しでも成立させる。却下理由は、**同じ TransitionKind の意味が
//   caps によって変わる**こと。Renderer が種別ごとの分岐の中にさらに caps
//   分岐を持つことになり、CompositeClass を切って「重みだけで制御できる」
//   状態を作った目的そのものを潰す。
//
// ============================================================================
// 【2A が決めたこと 2】各演出の duration ― Tab5 のフレーム数から決める
// ============================================================================
//
// 確定していたのは 600〜1000ms の範囲だけだった。値は次のとおり。
//
//   Slide            700ms
//   Wipe             700ms
//   Blinds           800ms
//   CrossFade        900ms
//   FadeThroughBlack 1000ms
//
// 根拠:
//
// (a) 領域コピー群を短く、blend 群を長く。
//     線や矩形が画面を横切る演出は「速いほど切り替わった感」が出る。
//     逆にフェードは短いとカットに見え、演出として読めない。
//     FadeThroughBlack は「A を消す」+「B を出す」の 2 段階なので、
//     CrossFade と同じ体感速度にするには時間が要る。よって範囲上限。
//     Blinds は要素が N 枚あり、Slide/Wipe と同じ時間だと 1 枚あたりの
//     動きが速すぎて何が起きたか読めない。よって一段長くする。
//
// (b) ★下限 600ms を使わない理由は Tab5 のフレーム数である。
//     Tab5 が 15fps しか出せない場合、600ms は **9 フレーム**しかない。
//     空間移動する演出（Slide / Wipe / Blinds）では 1 フレームあたりの
//     移動量が大きくなりすぎ、動きではなく飛びに見える。
//     時間駆動なので fps が変わっても終了時刻は変わらない。
//     fps で変わるのは**サンプル密度だけ**であり、密度が足りないぶんは
//     duration を伸ばすしかない。
//
// ★実機で見て調整してよい。ただし範囲 [600, 1000] は静的表明で固定してある。
//
// ============================================================================
// 【2A が決めたこと 3】★乱数の消費 ― 「予算」を固定する
// ============================================================================
//
// docs/CORE_CONTRACTS.md §2.3 の原則は「**人間が後から調整する設定値**によって
// 引く回数が変わってはならない」である（重みが全 0 でも 1 回引く、の理由）。
//
// 演出のパラメータ導出を素直に書くと、この原則が破れる:
//   CrossFade は引かない / Slide は 1 回 / Blinds は 2 回 …… となり、
//   **どの演出が当たったかで以降の乱数列がずれる**。さらに悪いことに、
//   caps で blend 群を除外しただけで当たる演出が変わり、列が丸ごとずれる。
//   「Tab5 で blend を切ったら演出列が全部変わった」は、まさに
//   Mac のテストでは検出できない事故である。
//
// そこで **予算を固定する**。
//
//   deriveTransitionParams() は演出によらず常に kTransitionParamDraws 回引く。
//   足りないぶんは next() で消費する（余り draw）。
//
// 結果として得られる不変条件（これが本命）:
//
//   ★ planTransition() は weights にも caps にも依存せず、
//     **常に kTransitionPlanDraws (= 4) 回**引く。
//   ★ planDwell() は caps に依存せず、**常に kDwellPlanDraws (= 5) 回**引く。
//
// つまり「重みを調整した」「blend を切った」だけで乱数列がずれることが
// **原理的に起きない**。
//
// 余りを belowN(1) ではなく next() で消費する理由: belowN(1) は早期 return し
// **エンジンを進めない**。余りに使うと draws は揃うのに steps が演出依存になり、
// steps を「エンジン実装が変わっていないことの検出」に使えなくなる。
// next() は必ず draws +1 / steps +1 で、棄却も起きない。
//
// ---- 消費順序の仕様（★これが契約。golden trace で固定してある）--------------
//
// | 関数                        | 引くもの                          | 回数 |
// |-----------------------------|-----------------------------------|------|
// | pickTransition(w, rng)      | belowN(total or 1)                | 1    |
// | deriveTransitionParams      | 下記                              | 3    |
// | planTransition              | pick 1 + params 3                 | 4    |
// | deriveKenBurns              | 下記                              | 5    |
// | planDwell                   | deriveKenBurns のみ               | 5    |
//
// deriveTransitionParams の内訳（★引く順序も契約）:
//
//   CrossFade / FadeThroughBlack : #1..#3 すべて余り next()
//   Slide            : #1 belowN(4)  → 方向    / #2,#3 余り
//   Wipe             : #1 belowN(4)  → 角度 index / #2,#3 余り
//   Blinds           : #1 belowN(枚数の幅) → 枚数 / #2 belowN(2) → 向き / #3 余り
//
// deriveKenBurns の内訳:
//
//   #1 belowN(2)          → 寄るのか引くのか
//   #2 belowN(倍率の幅)   → 寄り側の倍率（千分率）
//   #3 belowN(余白+1)     → 寄り側の x（千分率）
//   #4 belowN(余白+1)     → 寄り側の y（千分率）
//   #5 余り next()
//
// ★消費順序を変えるときは、この表・docs/CORE_CONTRACTS.md §2.3・
//   tests/unit/test_transition.cpp の golden を同時に直すこと。
//
// ============================================================================
// 【2A が決めたこと 4】★Params の導出に浮動小数の算術を使わない
// ============================================================================
//
// 「同じ seed なら Mac でも Tab5 でも同じ演出になること」が要件である。
// ところが `min + u * (max - min)` のような式は **積和融合 (FMA) の対象**で、
// GCC は既定で -ffp-contract=fast、clang は -ffp-contract=on である。
// 融合されるかどうかがコンパイラで違えば **最下位ビットが食い違う**。
// rng.hpp が unit() の仮数を 24bit に抑えて「完全に同じビット列」を保証しても、
// その先の算術で台無しになる。しかもこれも Mac のテストでは検出できない。
//
// そこで **Params は整数（千分率）で導出し、float は最後に 1 回の除算で作る**。
//   static_cast<float>(int) / 1000.0F
// は int→float 変換も除算も IEEE 754 で正確に丸められる単一演算であり、
// 加算が無いので融合されようがない。両プラットフォームで同一ビットになる。
//
// NormRect が千分率の整数を正典として持ち、float を関数で返すのはこのためである。
// 矩形の内包判定も **整数で**行う（x + w <= 1.0F と書くと加算が復活する）。

#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

#include "ambient/core/clock.hpp"
#include "ambient/core/rng.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// 種別
// ----------------------------------------------------------------------------

// 遷移演出（写真 A → 写真 B）。600〜1000ms。
enum class TransitionKind : std::uint8_t {
  CrossFade = 0,
  FadeThroughBlack,
  Slide,
  Wipe,
  Blinds,
};

inline constexpr std::size_t kTransitionKindCount = 5;

inline constexpr std::array<TransitionKind, kTransitionKindCount> kAllTransitionKinds{
    TransitionKind::CrossFade, TransitionKind::FadeThroughBlack, TransitionKind::Slide,
    TransitionKind::Wipe,      TransitionKind::Blinds,
};

// 滞在効果（1 枚を表示している間ずっと）。約 20 秒。
// ★遷移ではない。TransitionKind と混ぜないこと。
enum class DwellEffect : std::uint8_t {
  None = 0,
  KenBurns,
};

[[nodiscard]] const char* toString(TransitionKind kind) noexcept;
[[nodiscard]] const char* toString(DwellEffect effect) noexcept;

// ----------------------------------------------------------------------------
// 合成コスト分類 ― Tab5 の blend 性能が測れたら重みを調整するための軸
// ----------------------------------------------------------------------------

enum class CompositeClass : std::uint8_t {
  // アルファ合成が必要。2 枚のピクセルを混ぜる。Tab5 で重い可能性がある。
  Blend = 0,
  // 領域コピーで済む。矩形の転送だけ。ハードウェアが得意な形。
  RegionCopy,
};

[[nodiscard]] constexpr CompositeClass compositeClassOf(TransitionKind kind) noexcept {
  switch (kind) {
    case TransitionKind::CrossFade:
    case TransitionKind::FadeThroughBlack:
      return CompositeClass::Blend;
    case TransitionKind::Slide:
    case TransitionKind::Wipe:
    case TransitionKind::Blinds:
      return CompositeClass::RegionCopy;
  }
  return CompositeClass::Blend;  // 到達しない。安全側（重い前提）へ倒す。
}

[[nodiscard]] constexpr CompositeClass compositeClassOf(DwellEffect effect) noexcept {
  switch (effect) {
    case DwellEffect::None:
      return CompositeClass::RegionCopy;
    case DwellEffect::KenBurns:
      // 拡縮を伴うサンプリングなので blend 群として扱う。
      return CompositeClass::Blend;
  }
  return CompositeClass::Blend;
}

// ----------------------------------------------------------------------------
// Progress ― 経過時間からしか作れない進捗
// ----------------------------------------------------------------------------

class Progress {
 public:
  // ★既定構築なし。0 埋めで作れてしまうと「まだ始まっていない」と
  //   「開始直後」の区別が消える。
  Progress() = delete;

  // ★唯一のファクトリ。フレーム数から作る経路は用意しない（意図的）。
  //   duration <= 0 は「即完了」として 1.0 を返す（0 除算を作らない）。
  //   elapsed は [0, duration] にクランプされる。
  [[nodiscard]] static constexpr Progress fromElapsed(Millis elapsed, Millis duration) noexcept {
    if (duration.value <= 0) {
      return Progress{1.0F};
    }
    if (elapsed.value <= 0) {
      return Progress{0.0F};
    }
    if (elapsed.value >= duration.value) {
      return Progress{1.0F};
    }
    return Progress{static_cast<float>(elapsed.value) / static_cast<float>(duration.value)};
  }

  // 0.0 <= value() <= 1.0
  [[nodiscard]] constexpr float value() const noexcept { return v_; }
  [[nodiscard]] constexpr bool done() const noexcept { return v_ >= 1.0F; }

 private:
  constexpr explicit Progress(float v) noexcept : v_(v) {}

  float v_;
};

// ----------------------------------------------------------------------------
// 計画
// ----------------------------------------------------------------------------

// 企画の 5 種は 600〜1000ms。実際の値は演出ごとに 1C が決める。
inline constexpr Millis kTransitionDurationMin{600};
inline constexpr Millis kTransitionDurationMax{1000};

// KenBurns は約 20 秒（写真 1 枚の滞在時間に合わせる）。
inline constexpr Millis kDwellDurationDefault{20000};

struct TransitionPlan {
  TransitionKind kind{TransitionKind::CrossFade};
  Millis duration{kTransitionDurationMin};
  SteadyTime startedAt{};

  // ★単調時刻から進捗を出す。フレーム番号は登場しない。
  [[nodiscard]] constexpr Progress progressAt(SteadyTime now) const noexcept {
    return Progress::fromElapsed(now - startedAt, duration);
  }

  [[nodiscard]] constexpr CompositeClass compositeClass() const noexcept {
    return compositeClassOf(kind);
  }
};

struct DwellPlan {
  DwellEffect effect{DwellEffect::None};
  Millis duration{kDwellDurationDefault};
  SteadyTime startedAt{};

  [[nodiscard]] constexpr Progress progressAt(SteadyTime now) const noexcept {
    return Progress::fromElapsed(now - startedAt, duration);
  }

  [[nodiscard]] constexpr CompositeClass compositeClass() const noexcept {
    return compositeClassOf(effect);
  }
};

// ----------------------------------------------------------------------------
// 重みテーブル ― 設定値として外に出す
// ----------------------------------------------------------------------------

// 各演出の抽選重み。値は kAllTransitionKinds と同じ並び。
//
// ★Tab5 の blend 性能を実測したら、**この数値だけ**を書き換えること。
//   blend 群 (CrossFade / FadeThroughBlack) の重みを下げれば出現頻度が下がる。
//   pickTransition() の実装や Renderer のコードは触らない。
//   重み 0 の演出は選ばれない（合計が 0 なら先頭を返す）。
struct TransitionWeights {
  std::array<std::uint16_t, kTransitionKindCount> weight{};

  [[nodiscard]] constexpr std::uint32_t total() const noexcept {
    std::uint32_t sum = 0;
    for (const std::uint16_t w : weight) {
      sum += w;
    }
    return sum;
  }
};

// 既定は均等。blend 性能が未測定である以上、偏らせる根拠がまだ無い。
inline constexpr TransitionWeights kDefaultTransitionWeights{{1, 1, 1, 1, 1}};

// 重み付き抽選。
//
// ★乱数消費の契約: TransitionRng から belowN() を **ちょうど 1 回** 引く。
//   重みが全て 0 のときも 1 回引く（引く回数を入力依存にしないため）。
//   この回数は仕様である。変えるときは docs/CORE_CONTRACTS.md と
//   golden trace テストを同時に直すこと。
[[nodiscard]] TransitionKind pickTransition(const TransitionWeights& weights,
                                            TransitionRng& rng) noexcept;

// ----------------------------------------------------------------------------
// 描画側の能力申告
// ----------------------------------------------------------------------------
//
// ★重みテーブルとは役割が違う。
//   重み   = 「どれくらいの頻度で出したいか」（人間が調整する設定）
//   caps   = 「そもそも描けるか」（プラットフォームの事実）
// 重みで 0 にするのと caps で除外するのは効果は似ているが、由来が違うので分ける。
// 混ぜると「Tab5 では使えないから重みを 0 にした」設定が Mac にも効いてしまう。
struct RenderCaps {
  // アルファ合成が実用的な速度で使えると描画側が申告するか。
  // ★既定は true（Mac / SDL の現状）。Tab5 の blend 性能を実測して
  //   使い物にならなければ false を渡す。Core 側のコードは変えない。
  bool blend{true};

  [[nodiscard]] constexpr bool allows(CompositeClass cls) const noexcept {
    return cls == CompositeClass::RegionCopy || blend;
  }
  [[nodiscard]] constexpr bool allows(TransitionKind kind) const noexcept {
    return allows(compositeClassOf(kind));
  }
  [[nodiscard]] constexpr bool allows(DwellEffect effect) const noexcept {
    return allows(compositeClassOf(effect));
  }
};

// caps で描けない演出の重みを 0 にした重みテーブルを返す（純粋関数、乱数を引かない）。
// ★重み 0 にするだけなので、pickTransition() の抽選ロジックは 1 つのままで済む。
[[nodiscard]] constexpr TransitionWeights allowedWeights(const TransitionWeights& weights,
                                                         RenderCaps caps) noexcept {
  TransitionWeights out{};
  for (std::size_t i = 0; i < kTransitionKindCount; ++i) {
    out.weight[i] =
        caps.allows(kAllTransitionKinds[i]) ? weights.weight[i] : static_cast<std::uint16_t>(0);
  }
  return out;
}

// 候補が 1 つも残らなかったときの最終手段。
// 判断の根拠はこのファイル冒頭【2A が決めたこと 1】を読むこと。
// ★戻り値は必ず caps.allows() を満たす（transition.cpp の静的表明で固定）。
[[nodiscard]] constexpr TransitionKind fallbackTransition(RenderCaps caps) noexcept {
  return caps.blend ? TransitionKind::FadeThroughBlack : TransitionKind::Wipe;
}

// ----------------------------------------------------------------------------
// 各演出の duration ― 根拠はこのファイル冒頭【2A が決めたこと 2】
// ----------------------------------------------------------------------------

inline constexpr Millis kCrossFadeDuration{900};
inline constexpr Millis kFadeThroughBlackDuration{1000};
inline constexpr Millis kSlideDuration{700};
inline constexpr Millis kWipeDuration{700};
inline constexpr Millis kBlindsDuration{800};

[[nodiscard]] constexpr Millis durationOf(TransitionKind kind) noexcept {
  switch (kind) {
    case TransitionKind::CrossFade:
      return kCrossFadeDuration;
    case TransitionKind::FadeThroughBlack:
      return kFadeThroughBlackDuration;
    case TransitionKind::Slide:
      return kSlideDuration;
    case TransitionKind::Wipe:
      return kWipeDuration;
    case TransitionKind::Blinds:
      return kBlindsDuration;
  }
  return kCrossFadeDuration;  // 到達しない
}

// ----------------------------------------------------------------------------
// 演出ごとのパラメータ
// ----------------------------------------------------------------------------

// B が入ってくる向き。
enum class SlideDirection : std::uint8_t {
  FromLeft = 0,
  FromRight,
  FromTop,
  FromBottom,
};

inline constexpr std::size_t kSlideDirectionCount = 4;
[[nodiscard]] const char* toString(SlideDirection direction) noexcept;

struct SlideParams {
  SlideDirection direction{SlideDirection::FromLeft};
};

// ★Wipe の角度を自由角にしない。
//   斜めの境界は矩形転送では表せず、走査線ごとの転送（= 画面高さぶんの転送）に
//   なる。それは領域コピーではないので、Wipe を CompositeClass::RegionCopy と
//   分類した前提が崩れる。つまりこの制約は好みではなく**分類から来る制約**である。
//   45 度刻みにしたくなったら、先に compositeClassOf(Wipe) を見直すこと。
inline constexpr std::array<std::uint16_t, 4> kWipeAngles{0, 90, 180, 270};

struct WipeParams {
  // 境界が進む向き（度）。値は必ず kWipeAngles のいずれか。
  std::uint16_t angleDeg{0};
};

// スリット枚数の範囲。
// 下限 4: 2〜3 枚では Wipe との区別がつかない。
// 上限 10: 転送回数が枚数に比例する。720p で 10 枚なら 1 枚 72px で、
//          これ以上細かくすると Tab5 で転送回数が効いてくるうえ見た目も潰れる。
inline constexpr std::uint8_t kBlindsSlatsMin = 4;
inline constexpr std::uint8_t kBlindsSlatsMax = 10;

struct BlindsParams {
  // ★0 にならないこと（0 だと描くものが無くなり画面が固まる）。
  std::uint8_t slats{kBlindsSlatsMin};
  bool vertical{true};
};

// 演出ごとのパラメータをまとめて持つ。
//
// ★std::variant + std::visit ではなく、タグ (TransitionPlan::kind) + 素の struct に
//   している。RTTI が無い環境で型分岐に variant を使うのは正しい選択肢だが、
//   visit はコードサイズを押し上げる（docs/PORTABILITY.md）。ここは全体で
//   数バイトしかなく、判別タグが既に TransitionPlan::kind として存在するので、
//   素直に全部持つほうが安い。**kind に対応するメンバだけを読むこと。**
struct TransitionParams {
  SlideParams slide{};
  WipeParams wipe{};
  BlindsParams blinds{};
};

// 正規化矩形。写真（画面にフィット済み）に対する切り出し範囲。
//
// ★千分率の整数が正典。float は単一除算で導出した射影にすぎない。
//   理由はこのファイル冒頭【2A が決めたこと 4】（FMA でビットが食い違う）。
struct NormRect {
  std::uint16_t xMilli{0};
  std::uint16_t yMilli{0};
  std::uint16_t wMilli{1000};
  std::uint16_t hMilli{1000};

  [[nodiscard]] constexpr float x() const noexcept { return static_cast<float>(xMilli) / 1000.0F; }
  [[nodiscard]] constexpr float y() const noexcept { return static_cast<float>(yMilli) / 1000.0F; }
  [[nodiscard]] constexpr float w() const noexcept { return static_cast<float>(wMilli) / 1000.0F; }
  [[nodiscard]] constexpr float h() const noexcept { return static_cast<float>(hMilli) / 1000.0F; }

  // ★整数で判定する。x() + w() <= 1.0F と書くと浮動小数の加算が復活する。
  [[nodiscard]] constexpr bool insideUnitSquare() const noexcept {
    return wMilli > 0 && hMilli > 0 && (static_cast<int>(xMilli) + static_cast<int>(wMilli)) <= 1000 &&
           (static_cast<int>(yMilli) + static_cast<int>(hMilli)) <= 1000;
  }

  [[nodiscard]] constexpr bool operator==(const NormRect&) const noexcept = default;
};

// 寄り側の倍率の範囲（千分率）。
// 1000 に近すぎると動いて見えず、小さすぎると元画像の解像度が足りず眠くなる。
inline constexpr std::uint16_t kKenBurnsScaleMinMilli = 700;
inline constexpr std::uint16_t kKenBurnsScaleMaxMilli = 900;

struct KenBurnsParams {
  NormRect start{};
  NormRect end{};

  // ★start と end は必ず異なる（片方は必ず全画面、もう片方は倍率 < 1000）。
  //   同じだと 20 秒間まったく動かず、演出が消える。
  [[nodiscard]] constexpr bool moves() const noexcept { return !(start == end); }
};

// ----------------------------------------------------------------------------
// 乱数の予算 ― 根拠はこのファイル冒頭【2A が決めたこと 3】
// ----------------------------------------------------------------------------

// 演出パラメータの導出が引く回数。**演出によらず一定**。
// 現在の最大消費は Blinds の 2 回。1 回ぶん余裕を持たせてある。
// ★予算内であれば、後からパラメータを 1 つ増やしても乱数列はずれない。
inline constexpr std::uint64_t kTransitionParamDraws = 3;

// planTransition() が引く回数。抽選 1 + パラメータ 3。
// ★weights にも caps にも依存しない。
inline constexpr std::uint64_t kTransitionPlanDraws = 1 + kTransitionParamDraws;

// planDwell() / deriveKenBurns() が引く回数。使うのは 4 回、1 回は余り。
// ★caps に依存しない（KenBurns が使えなくても同じだけ引く）。
inline constexpr std::uint64_t kDwellPlanDraws = 5;

// ----------------------------------------------------------------------------
// 計画の生成
// ----------------------------------------------------------------------------

// 一度の切り替えぶんの計画。
// ★TransitionPlan（凍結済み）は変えず、外側で束ねている。
struct PlannedTransition {
  TransitionPlan plan{};
  TransitionParams params{};
};

struct PlannedDwell {
  DwellPlan plan{};
  KenBurnsParams kenBurns{};
};

// 演出パラメータを seed から決定的に導出する。
// ★乱数消費: 演出によらず常に kTransitionParamDraws 回。順序はヘッダ冒頭の表。
[[nodiscard]] TransitionParams deriveTransitionParams(TransitionKind kind,
                                                      TransitionRng& rng) noexcept;

// KenBurns の開始 / 終了矩形を seed から決定的に導出する。
// ★戻り値の矩形は必ず単位正方形に収まる（= 画面外を舐めない）。
// ★乱数消費: 常に kDwellPlanDraws 回。
[[nodiscard]] KenBurnsParams deriveKenBurns(TransitionRng& rng) noexcept;

// 次の切り替えの計画を立てる。
//
// - caps で描けない演出は候補から外す（重み 0 と同じ扱い）
// - 候補が 1 つも残らなければ fallbackTransition(caps) を使う
//   （重みが全て 0 のときも同じ経路。**caps を満たす種別が必ず返る**）
// - duration は durationOf(kind)
//
// ★乱数消費: weights / caps によらず常に kTransitionPlanDraws 回。
//   これが「重みを調整した」「blend を切った」だけで列がずれないことの根拠。
[[nodiscard]] PlannedTransition planTransition(const TransitionWeights& weights, RenderCaps caps,
                                               SteadyTime now, TransitionRng& rng) noexcept;

// 滞在効果の計画を立てる。
//
// KenBurns は blend 群なので、blend が使えなければ DwellEffect::None になる。
// ★それでも乱数は同じだけ引く（caps で消費回数を変えない）。
[[nodiscard]] PlannedDwell planDwell(RenderCaps caps, SteadyTime now,
                                     TransitionRng& rng) noexcept;

}  // namespace ambient::core
