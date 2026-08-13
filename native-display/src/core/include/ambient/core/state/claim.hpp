// ambient/core/state/claim.hpp — 優先度アービターの入力
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜイベントキューではなく「レベルトリガの claim 評価」なのか ← 設計の要
// ============================================================================
//
// 表示すべきものの優先順位は 7 段ある。そのうち **6 つは状態（レベル）条件** である。
//
//   「記録が Red である」「記録が保留中である」「通信が切れている」——
//   これらは一度起きて終わるイベントではなく、**真であり続ける**条件である。
//
// これをイベントキューで扱うと、次の 2 つを手で書く羽目になる。
//
//   1. レベル条件をエッジ（変化した瞬間）に変換する
//   2. **上位に抑止されたイベントを、抑止が解けたときに再配達する仕組み**
//
// 2 がまさに防ぎたいバグの温床である。「地震アラート中に Red になった。
// アラートが明けたら Red を出す」を再配達キューで正しく書くのは難しく、
// 取りこぼしても普段は表に出ない（次の変化で復帰してしまうので、
// 再現条件を掴めないまま「たまに出ない」になる）。
//
// レベルトリガなら再配達は要らない。**毎 tick 全員に「今どうしたい？」と聞き、
// 一番強いものを選ぶ**だけである。抑止が解けた tick で自動的に選ばれる。
//
// 真にエッジなのは地震だけである（新しい地震 ID が現れる瞬間）。
// これは AlertEngine が **エッジ→レベルの変換器** として吸収する
// （ambient/core/state/alert_engine.hpp）。境界を 1 箇所に閉じ込める。

#pragma once

#include <cstdint>
#include <optional>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/state/display_state.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// 優先度（7 段）
// ----------------------------------------------------------------------------
//
// 数値が大きいほど強い。
//
// ★このうちレベル条件は 6 つ（Photo / Env / Degraded / Care / CareDue /
//   RecordedAwaitingServer）。Alert だけがエッジ由来で、AlertEngine が
//   レベルへ変換して供給する。
//
// ★順序の根拠:
//   - Photo が最弱: 既定のアンビエント表示。常に claim されている前提。
//   - Degraded > Env: データが陳腐化しているのに環境情報を出すのは害。
//     「古い天気」より「取れていない」を出すべき。
//   - CareDue > Care: 期限超過は通常のお世話カードより強い。
//   - Alert > CareDue: 生命に関わる情報。
//   - RecordedAwaitingServer が最強: ★下の「企画書との突合」を参照。
//
// ============================================================================
// 企画書 §2.3「表示優先順位」との突合（レーン 1A が実施。決着済み）
// ============================================================================
//
// 出典: docs/企画概要設計_MacSDLスマートディスプレイ.md §2.3
// （1 が最強）:
//
//   1. 初期設定・OTA・致命的エラー   ← ★この 7 段には対応する状態が無い（後述）
//   2. 記録操作と取り消し猶予        -> RecordedAwaitingServer
//   3. 緊急地震通知                  -> Alert
//   4. hogo-meter Red 状態           -> CareDue
//   5. 通常の GeoFrame 通知          -> Env（天気・週間予報のカット）
//   6. Home Assistant 環境カード     -> Env（室内環境のカット）
//   7. 写真表示                      -> Photo
//
// ★突合の結果、**1 箇所だけ**を直した: Alert と RecordedAwaitingServer の入れ替え。
//   当初 Alert を最強にしていたが、企画書は「記録操作と取り消し猶予」を
//   地震通知より上に置いている。独立した根拠が 3 つある:
//     - 企画書 §2.3 の rank 2 > rank 3
//     - 企画書 §2.1 ALERT:「通常通知は操作中に割り込ませず、
//       **重要通知のみ操作完了後に表示する**」= 地震も操作完了を待つ
//     - この設計の要件そのもの:「記録保留中は地震で遷移させない、
//       保留が明けたら地震を出す」。RecordedAwaitingServer < Alert では成立しない。
//   ★これを「Alert が最強の方が自然だ」で戻さないこと。ユーザが今まさに
//     押している操作を横取りする端末は、押した記録が消えたように見える。
//     地震アラートは AlertEngine がレベルとして出し続けるので、
//     保留が明けた tick で自動的に前へ出る（抑止ロジックは書かない）。
//
// ★Degraded は動かしていない（企画書 rank 1 は Degraded ではない）:
//   rank 1 は「初期設定・OTA・**致命的**エラー」であり、この 7 段のどれでもない
//   （Boot / Setup / OTA に相当する状態が DisplayStateKind に存在しない）。
//   Degraded は display_state.hpp が「通信断・データ陳腐化の **告知**」と定義し、
//   企画書の状態遷移図でも `Boot --> Degraded: **一部** API に接続できない` /
//   `Degraded --> Photo: 必要データが復旧` と、部分的かつ復帰可能な状態である。
//   Degraded を最強にすると、データが古いだけで地震アラートを抑止してしまい、
//   「生命に関わる情報」という Alert の根拠を反転させる。
//   ★rank 1 に相当する状態が要る（初期設定 / OTA / 致命的エラー）と分かったら、
//     DisplayStateKind と ClaimPriority に **新しい段を足す**こと。
//     Degraded を流用しないこと。意味が違う。
//
// ★ladder は 7 段だが状態と 1 対 1 ではない。rank 5 と rank 6 はどちらも Env
//   （display_state.hpp が Env を「天気・週間予報・室内環境」と定義している）に
//   潰れており、代わりに Care（お世話カード通常）が ladder に現れない。
//   Care は「ユーザが明示的に開いた画面」で自動遷移ではないため ladder の
//   対象外であり、Env < Care < CareDue に置いた（環境カットに横取りさせない）。
enum class ClaimPriority : std::uint8_t {
  Photo = 0,
  Env = 1,
  Degraded = 2,
  Care = 3,
  CareDue = 4,
  Alert = 5,
  RecordedAwaitingServer = 6,
};

inline constexpr std::size_t kClaimPriorityCount = 7;

[[nodiscard]] const char* toString(ClaimPriority priority) noexcept;

// ----------------------------------------------------------------------------
// Claim ― 「私はこれを表示したい」という表明
// ----------------------------------------------------------------------------
//
// ★レベルであること。条件が真である **間ずっと** 同じ claim を出し続ける。
//   「1 回出して消える」実装にすると、上位に抑止されている間に claim が消滅し、
//   結局イベントキューと同じ再配達問題を抱えることになる。
struct Claim {
  ClaimPriority priority{ClaimPriority::Photo};

  // この claim が勝ったときに表示する状態。
  DisplayStateKind kind{DisplayStateKind::Photo};

  // どの IClaimSource が出したか。デバッグと遷移判定に使う。
  // 値の割り当ては各 source が自分で決める（衝突しないよう定数で持つこと）。
  std::uint32_t sourceId{0};

  // 表示内容の識別子。
  // ★同じ kind でも payloadId が変われば「別の内容」として遷移させる。
  //   例: 地震 A のアラート中に地震 B が来た → kind は Alert のままだが
  //       payloadId が変わるので画面を更新する。
  //   0 は「内容の区別なし」を意味する。
  std::uint32_t payloadId{0};

  // この時刻より前は選ばれない（クールダウン明けの表現）。
  // 既定 = SteadyTime{} なので、指定しなければ常に有効。
  SteadyTime notBefore{};

  // この時刻以降は無効（レベルの寿命）。nullopt = 無期限。
  // ★寿命付きの claim は、寿命が切れたら source 側も出すのをやめること。
  //   expiresAt はあくまで安全網であり、source の状態管理の代わりにはならない。
  std::optional<SteadyTime> expiresAt{};

  // now の時点でこの claim が有効か。
  [[nodiscard]] constexpr bool activeAt(SteadyTime now) const noexcept {
    if (now < notBefore) {
      return false;
    }
    if (expiresAt.has_value() && now >= *expiresAt) {
      return false;
    }
    return true;
  }
};

// ----------------------------------------------------------------------------
// IClaimSource ― claim を出す側
// ----------------------------------------------------------------------------
//
// 実装例: 写真、地震アラート、お世話記録、環境情報、通信状態。
class IClaimSource {
 public:
  IClaimSource() = default;
  virtual ~IClaimSource() = default;
  IClaimSource(const IClaimSource&) = delete;
  IClaimSource& operator=(const IClaimSource&) = delete;
  IClaimSource(IClaimSource&&) = delete;
  IClaimSource& operator=(IClaimSource&&) = delete;

  // 現在の claim を out へ **追記** する（out をクリアしないこと）。
  // 出す数は 0 個か 1 個。2 つ出したくなったら source を分けること
  // （1 つの source が複数の関心を持っているというシグナルである）。
  //
  // ★const である。ここで状態を書き換えてはいけない。
  //   collect() は「今どうしたいか」を答えるだけの問い合わせであり、
  //   毎 tick 何度呼んでも同じ答えが返ること。
  //   入力の取り込み（例: AlertEngine の新着地震）は別のメソッドで行う。
  virtual void collect(SteadyTime now, std::vector<Claim>& out) const = 0;
};

}  // namespace ambient::core
