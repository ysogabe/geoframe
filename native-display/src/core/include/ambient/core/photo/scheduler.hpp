// ambient/core/photo/scheduler.hpp — 写真の再生スケジューラ
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
// ★写真の**ピクセルデータを Core に持たせないこと**。ここが扱うのは ID だけ。
//
// 移植元: app/static/js/scheduler.js
//
// ############################################################################
// # ★★★ 乱数消費順序の契約 ― これを破ると Tab5 で再生が発散する ★★★
// #
// # 「Params を core 側で導出する」だけでは **不十分** である。
// # 全消費者が **同じ順序で・同じ回数** 引かなければ、同じ seed から
// # 同じ再生シーケンスにはならない。
// #
// # そしてこの違反は、Mac 上のテストでは **絶対に検出されない**。
// # Mac 側は常に自己整合するからである。発覚するのは Tab5 に載せた
// # 最終フェーズであり、そこで原因を特定するのは極めて難しい。
// #
// # 対策は 1 つ: Rng::draws() / Rng::steps() を golden trace テストで固定する。
// ############################################################################
//
// ============================================================================
// buildPlayOrder() 1 回あたりの SchedulerRng 消費（出典 scheduler.js:78-105）
// ============================================================================
//
//   1. トリップ順の Fisher-Yates シャッフル
//      i = groups.size()-1 から 1 まで降順に belowN(i+1) を 1 回ずつ。
//      → **groups.size() - 1 回**（groups が 0 or 1 個なら 0 回）
//
//   2. 各写真の優先度ゆらぎ
//      photoPriority() の末尾で unit() を 1 回。
//      → **そのラウンドで評価する写真 1 枚につき 1 回**
//      ★呼ぶ順序: シャッフル**後**のグループ順。グループ内は PHOTOS 配列順。
//        （scheduler.js は g.idxs.map(...) で評価しており、この順序である）
//      ★グループ内の全写真を評価する。TRIP_MAX で絞るのは評価の**後**。
//        「上位 6 枚だけ評価」に変えると消費回数が変わり、列がずれる。
//
//   合計: (groups - 1) + (そのラウンドで評価した写真数)
//
// ============================================================================
// syncSchedule()（新着の差し込み）の消費（出典 scheduler.js の NEW_INSERT_WINDOW）
// ============================================================================
//
//   新着 1 枚につき belowN(...) を 1 回。
//
//   ★1B が実装時に精密化した点（出典 scheduler.js:199 の .slice(0, ...)）:
//     差し込むのは **先頭 kNewInsertWindow 枚まで** である。したがって
//
//         draws の増分 = min(新着枚数, kNewInsertWindow)
//
//     であって「新着枚数そのもの」ではない。100 枚増えても引くのは 12 回。
//     残りは shown に無いので PRI_NEVER で次ラウンドに最優先選出される。
//
//   ★引数 n も入力依存である（belowN(min(kNewInsertWindow, queue.size()+1))）。
//     n が変わっても draws は 1 回のまま（belowN の仕様）。steps は棄却で
//     変わりうるので、golden trace で固定するのは draws の方。
//
//   ★旧 ID と 1 枚も重ならない場合（データセットごと入れ替わった）だけは、
//     差し込みではなくキューの作り直しになり buildPlayOrder() 1 回ぶんを消費する。
//     初回同期（起動直後 / スナップショット復元直後）は 0 回。
//
// 実測値は tests/unit/test_golden_trace.cpp が固定している。
//
// ============================================================================
// TransitionRng は別インスタンス（型が違う）
// ============================================================================
//
//   pickTransition() が belowN() を 1 回。写真 1 枚の表示につき 1 回。
//   ★SchedulerRng とは **別の型** なので、取り違えるとコンパイルエラーになる。
//     共有していたら、演出を 1 種類足すだけで再生順が変わっていた。

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/model/photo.hpp"
#include "ambient/core/photo/snapshot.hpp"
#include "ambient/core/rng.hpp"

namespace ambient::core {

// 優先度計算の入力。
// ★now を引数で受け取ること。内部で時計を引かないこと
//   （引くとテストが決定的でなくなる）。
struct PriorityInput {
  // 最終表示ラウンド。nullopt = 一度も表示していない（PRI_NEVER 適用）。
  std::optional<std::uint32_t> lastShownRound;
  std::uint32_t currentRound{0};

  // 新着ブースト用。fetched_at からの経過日数。nullopt = 不明（ブーストなし）。
  std::optional<float> ageDaysSinceFetch;

  // 記念日ブースト用。撮影月日と今日の環状距離を 0..1 に写した重み。
  // 0 = 窓の外、1 = 今日が記念日。
  float annivWeight{0.0F};

  bool fav{false};
  bool qualityRecommended{false};
  bool qualityAlternate{false};
};

// 優先度スコアを返す。
//
// ★乱数消費: unit() を **ちょうど 1 回**（PRI_JITTER のゆらぎ）。
//   入力によらず必ず 1 回引くこと。条件分岐で引かない経路を作ると、
//   同じ写真集合でも列がずれる。
[[nodiscard]] float photoPriority(const PriorityInput& input, SchedulerRng& rng) noexcept;

// 撮影月日と今日の**環状**距離を 0..1 の重みへ（近いほど 1）。
// 年跨ぎを考慮する（12/30 と 1/2 の距離は 3 日）。出典 scheduler.js:43-52。
//
// 引数はどちらも dayOfYearNoLeap() の戻り値（1/1 = 0、うるう年なし）。
// 距離が kAnnivWindowDays 以上なら 0。乱数を引かない。
[[nodiscard]] float annivWeight(int photoDayOfYear, int todayDayOfYear) noexcept;

// ----------------------------------------------------------------------------
// 「今」の受け渡し ― ★Core は時計を引かない
// ----------------------------------------------------------------------------
//
// 新着ブーストには「fetched_at からの経過日数」が、記念日ブーストには
// 「今日の月日」が要る。どちらも時計に依存するので、**呼び出し側が渡す**。
// buildPlayOrder() の中で clock を引くと、テストが決定的でなくなる。
//
// ★既定（何も指定しない）= 両ブーストを掛けない。
//   このとき優先度は staleness と fav と乱数ゆらぎだけで決まる。
//   ★乱数の消費回数は now の中身によらず一定である（photoPriority は
//     入力によらず必ず unit() を 1 回引く）。したがって now を変えても
//     golden trace の draws は動かない。動くのは選ばれる写真の方だけ。
struct SchedulerNow {
  // 新着ブースト用。nullopt = ブーストを掛けない。
  // fetched_at ("YYYY-MM-DDTHH:MM:SS") との差から経過日数を出す。
  std::optional<WallTime> wall{};

  // 記念日ブースト用の「今日の通日」（dayOfYearNoLeap の戻り値）。
  // nullopt = ブーストを掛けない。
  //
  // ★写真の `time` は撮影地のローカル時刻なので、ここに渡す「今日」も
  //   ローカル暦で求めること（parse.hpp の toCivilTime にオフセットを渡す）。
  //   UTC で求めると JST の 0:00〜9:00 に日付が 1 日ずれる。
  std::optional<int> todayDayOfYear{};
};

// トリップのグループ。PHOTOS が撮影時刻順なので連続 run で検出できる。
struct TripGroup {
  std::int32_t trip{0};
  std::size_t begin{0};  // manifest.items 内の開始添字
  std::size_t count{0};
};

// manifest をトリップ単位にグループ化する（乱数を引かない）。
[[nodiscard]] std::vector<TripGroup> groupByTrip(const PhotoManifest& manifest);

// トリップ順を Fisher-Yates でシャッフルする。
//
// ★乱数消費: belowN() を **groups.size() - 1 回**。
//   i = n-1 から 1 まで降順に belowN(i+1)。
//   標準の std::shuffle を使わないこと（分布実装が処理系依存で移植性が無い。
//   rng.hpp の解説を参照）。
void shuffleTrips(std::span<TripGroup> groups, SchedulerRng& rng) noexcept;

// 1 ラウンドぶんの再生順を作る。
//
// ★乱数消費: shuffleTrips (groups-1 回) + 評価した写真数ぶんの unit()。
//   詳細はこのヘッダ冒頭の契約を参照。manifest が空なら **0 回**（何もしない）。
//
// state.roundNo をインクリメントし、state.playQueue の末尾へ積む。
// 戻り値は積んだ枚数。
//
// ★now は既定引数にしてある（追加であって変更ではない。3 引数の呼び出しは
//   そのままコンパイルできる）。渡さなければ新着・記念日ブーストが無効になるだけで、
//   乱数の消費回数は変わらない。
[[nodiscard]] std::size_t buildPlayOrder(const PhotoManifest& manifest, SchedulerSnapshot& state,
                                         SchedulerRng& rng, const SchedulerNow& now = {});

// 次に出す写真を「覗く」。キューは消費しない。
//
// ★キューを消費しない理由: 統計カット・旅イントロ・イベント割り込みで
//   捨てられることがあるため。実際に表示したときだけ markShown() を呼ぶ。
// ★解決できない ID（manifest から消えた写真）は捨てて次へ進むこと。
//   絶対に無限ループしないこと（出典 scheduler.js は guard 64 回で打ち切っている）。
//
// ★乱数消費: キューが空でなければ **0 回**。空なら buildPlayOrder() 1 回ぶん。
//   「覗くだけ」なのに乱数を引くことがあるのはここだけなので、
//   golden trace ではキューを空にした状態からの peekNext も固定すること。
[[nodiscard]] std::optional<std::size_t> peekNext(const PhotoManifest& manifest,
                                                  SchedulerSnapshot& state, SchedulerRng& rng,
                                                  const SchedulerNow& now = {});

// 実際に表示したときだけ呼ぶ。キューを 1 つ消費し、shown を更新する。
// ★乱数を引かない。
// ★キュー先頭が当該写真の ID のときだけ消費する（別の写真を割り込みで出した場合、
//   キューは動かさず shown だけ更新する。出典 scheduler.js:127）。
void markShown(const PhotoManifest& manifest, std::size_t index, SchedulerSnapshot& state);

// manifest の差し替えを反映する。
//
// ★乱数消費: 差し込む新着 1 枚につき belowN(...) を 1 回
//   = min(新着枚数, kNewInsertWindow) 回。新着が 0 枚なら 0 回。
//   旧 ID と 1 枚も重ならない場合だけは buildPlayOrder() 1 回ぶんを消費する。
//
// ★シグネチャが変わっていなければ **何もしない**（巻き戻しバグの根治点）。
void syncSchedule(const PhotoManifest& manifest, SchedulerSnapshot& state, SchedulerRng& rng,
                  const SchedulerNow& now = {});

}  // namespace ambient::core
