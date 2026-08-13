// ambient/core/state/alert_engine.hpp — 地震アラート（エッジ→レベル変換器）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// この型が存在する理由
// ============================================================================
//
// 7 段の優先順位のうち、**真にエッジなのは地震だけ**である
// （新しい地震 ID が現れる「瞬間」）。他の 6 つは元からレベル条件である。
//
// エッジとレベルを 1 つのアービターで扱おうとすると、アービター全体が
// イベントキュー化して再配達問題を抱える（claim.hpp の解説を参照）。
//
// そこで **エッジ性をこの型 1 つに閉じ込める**。
//   入力: ingest() で新着地震を受け取る（エッジ）
//   出力: collect() が一定時間 claim を出し続ける（レベル）
//
// アービターから見れば AlertEngine は他の source と何も変わらない。
// エッジ／レベルの境界がコード上の 1 箇所に固定される。

#pragma once

#include <cstddef>
#include <cstdint>
#include <deque>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state_store.hpp"

namespace ambient::core {

// アラートを表示し続ける時間。
//
// ★実測して確定させた（レーン 1A）。出典は app/static/js/events.js:130
//     const EQ_EVENT_MS = 60000;   // 地震イベントは長め（防災レイヤをじっくり見せる）
//   docs/CORE_CONTRACTS.md は「config.js にある」と書いているが、実際は
//   events.js にある。暫定値だった 20000 は同じ events.js の EVENT_MS（=20000、
//   ADS-B 航空機イベント用）であって地震用ではない。取り違えを解消した。
//
// ★注意: 上流の 60 秒は「防災レイヤ（ハザードマップ）をじっくり見せる」ための
//   長さである。Mac 版は地図表示が対象外（企画書 §1.3）なので、この **値は
//   移植したが根拠は移植できていない**。演出を実装する段で再検討してよい。
//
//   ★test_alert_engine.cpp は、この値を **リテラル 60000 で** 検査している。
//     わざとである。実測して決めた値を「なんとなく」書き換えたときに
//     テストが落ちて手が止まる、という速度制限になる。
//     変えるなら、events.js の実測値との関係をこのコメントごと更新すること。
//     （表示時間の **長さ** に依存するテストは、リテラルではなく
//       この定数を参照している。値を変えても追従する）
inline constexpr Millis kAlertHoldDuration{60000};

// ----------------------------------------------------------------------------
// 既知地震 ID の永続化 ― ★上流（Web 版）に対する改善点
// ----------------------------------------------------------------------------
//
// events.js の seenEq は **メモリのみ** で、リロードで消える。
// C++ 版は IStateStore の背後へ永続化する。
//
// ★「firstLoad が既に一斉発火を防いでいるのに、なぜ永続化するのか」
//   ——これが最初に来る疑問なので先に答えておく。
//
//   seen の再現だけなら firstLoad でほぼ足りる。永続化が本当に効くのは
//   **グローバルクールダウンの基準時刻**である。
//   地震が発火 → 30 秒後にアプリが再起動、というとき、firstLoad はその地震を
//   既知にするが、everFired_ が false に戻るため cooldownElapsed() が true になり、
//   **群発の 2 発目が即座に発火する**。10 分クールダウンが存在する理由
//   （群発でイベントを連鎖させない）が、再起動のたびに無効化される。
//   しかもこれは画面に出ないので気づけない。だから両方を保存する。
//
// ★SteadyTime は再起動をまたぐと意味を失う（起点が任意）。
//   よってクールダウンの基準は **WallTime** で保存し、復元時に
//   「今の単調時刻の座標系」へ引き直す。save()/restore() が WallTime を
//   受け取るのはこのためである。
//
// ★何をどれだけ保存するか（決定と理由）:
//   - 直近 kSeenQuakePersistMax (= 64) 件だけ。全 500 件は保存しない。
//     500 x 15B ≒ 7.5KB は Tab5 の NVS パーティション 20KB の 37% を占め、
//     しかもその 20KB は hogo-meter が 4 namespace で先に使っている。
//     64 件なら約 1KB（5%）で収まる。
//   - 64 件で足りる根拠: 復元後に必要なのは「次回起動時点で API の
//     24 時間窓にまだ入っている地震」を既知にすることだけである。
//     プロセス実行中の重複排除は従来どおりメモリ上の 500 件が担う。
//   - ★「at の最大値（高水位標）を 1 つ保存する」案は採らなかった。
//     JMA は続報で古い時刻のイベントを後から出しうるため、高水位標だと
//     後から来た未知の地震を黙って飲み込む。失敗が「アラートが出ない」
//     という観測できない形になるので、実 ID を保持する方式を選んだ。
inline constexpr std::size_t kSeenQuakePersistMax = 64;

// IStateStore のキー。★NVS のキー長上限は 15 文字（constants::kNvsMaxKeyBytes）。
inline constexpr const char* kSeenQuakeStoreKey = "gf.eq.v1";

class AlertEngine final : public IClaimSource {
 public:
  // この source の識別子。他の source と衝突しない値にすること。
  static constexpr std::uint32_t kSourceId = 0x41'4C'52'54;  // 'ALRT'

  AlertEngine() = default;

  // ---- 入力（エッジ） ------------------------------------------------------

  // /api/env の quakes を投入する。
  //
  // 発火条件（すべて満たしたときだけ claim を立てる。出典 events.js:174-232）:
  //   1. eid が既知でない（seen セットに無い）
  //   2. maxi の整数部が kQuakeMinIntensity (= 3) 以上
  //      ★maxi は "5-" のような文字列。parseIntPrefix() で読むこと。
  //   3. 前回発火から kQuakeGlobalCooldown (= 10 分) 経過している
  //      ★これは **グローバル** クールダウンである。同一イベント ID ごとの
  //        クールダウンではない。群発地震でイベントが連鎖しないためのもの。
  //        ID ごとに読み替えると群発時に連続発火して意味を失う。
  //   4. 他イベントを表示中でない（otherEventActive == false）
  //
  // firstLoad == true のときは **claim を立てず、seen に登録するだけ**。
  // 起動直後に過去 24 時間ぶんの地震で一斉発火するのを防ぐ
  // （events.js の envFirst と同じ扱い）。
  void ingest(std::span<const QuakeItem> quakes, SteadyTime now, bool firstLoad,
              bool otherEventActive);

  // ---- 出力（レベル） ------------------------------------------------------

  // 発火中なら Alert の claim を 1 つ出す。発火していなければ何も出さない。
  void collect(SteadyTime now, std::vector<Claim>& out) const override;

  // ---- 時間跳躍（スリープ復帰 / NTP 同期） ---------------------------------

  // TimeJumpDetector が跳躍を報告したら呼ぶ。
  //
  // ★何をするかは clock.hpp の rebaseForJump() に集約したポリシーに従う:
  //   skew > 0 なら「その時間は実際に経過した」として基準点を巻き戻し、
  //   skew < 0 なら何もしない（クールダウンを延ばして本物のアラートを
  //   握り潰さないため）。
  //
  //   - クールダウンの基準 lastFiredAt_ を巻き戻す
  //     → 実時間で 10 分経っていれば復帰後に明けている
  //   - アラート表示の基準 alertStartedAt_ を巻き戻す
  //     → 8 時間前の地震アラートが画面に残り続けない
  void onTimeJump(const TimeJump& jump) noexcept;

  // ---- 永続化 --------------------------------------------------------------

  // 既知地震 ID（直近 kSeenQuakePersistMax 件）とクールダウン基準を書き出す。
  //
  // ★flush() は呼ばない。まとめてから呼ぶのは呼び出し側の責務
  //   （write() ごとに commit すると NVS のフラッシュ寿命を削る）。
  // ★領域不足なら件数を半減して再試行し、それでも入らなければ
  //   OutOfSpace を返す。★戻り値を握り潰さないこと。
  [[nodiscard]] StoreStatus save(IStateStore& store, SteadyTime now, WallTime wallNow) const;

  // 復元する。キーが無ければ NotFound を返し、状態は変更しない
  // （初回起動は異常ではないので、呼び出し側は NotFound を無視してよい）。
  //
  // ★now / wallNow は「クールダウンの残りを今の単調時刻の座標系へ引き直す」
  //   ために要る。電源が落ちていた時間もクールダウンの経過として数える。
  [[nodiscard]] StoreStatus restore(const IStateStore& store, SteadyTime now, WallTime wallNow);

  // ---- 検査用 --------------------------------------------------------------

  [[nodiscard]] std::size_t seenCount() const noexcept { return seen_.size(); }

  // eid が既知か。★発火しなかった地震も既知になる（events.js と同じ）。
  [[nodiscard]] bool hasSeen(std::string_view eid) const noexcept;
  [[nodiscard]] bool alerting(SteadyTime now) const noexcept;
  [[nodiscard]] const QuakeItem& activeQuake() const noexcept { return active_; }

  // ★グローバルクールダウンが明けているか。
  //   一度も発火していなければ常に true（起動直後の 1 発目を抑止しない）。
  //   ★eid ごとではなく **全体で** 1 つのクールダウンであることに注意。
  //   単体でテストできるよう public にしてある（群発地震の抑止はここが要）。
  [[nodiscard]] bool cooldownElapsed(SteadyTime now) const noexcept;

 private:
  // 表示内容が変わったことをアービターへ伝えるための ID を採番する。
  // 0 は「内容の区別なし」なので、必ず 1 以上を返す。
  [[nodiscard]] std::uint32_t nextPayloadId() noexcept;

  // eid を既知として登録する（上限を超えたら最古を捨てる）。
  // ★events.js の rememberEq に対応。発火の可否に **関係なく** 呼ぶ。
  void remember(const std::string& eid);

  // 直近 count 件を直列化する（save() の内部。件数を変えて再試行するため分けてある）。
  [[nodiscard]] std::string encode(std::size_t count, SteadyTime now, WallTime wallNow) const;

  // 既知地震 ID のリング。
  // ★上限 kSeenQuakeMax (= 500)。長期稼働で際限なく増えないようにするため。
  //   捨てるのは挿入順で最も古いもの。std::deque を使うのは、
  //   Tab5 で 500 件の連続確保を避けるため（大きな静的バッファを作らない）。
  std::deque<std::string> seen_;

  QuakeItem active_{};
  SteadyTime alertStartedAt_{};
  bool alertActive_{false};

  // グローバルクールダウンの基準時刻。
  SteadyTime lastFiredAt_{};
  bool everFired_{false};

  std::uint32_t payloadCounter_{0};
  std::uint32_t activePayloadId_{0};
};

}  // namespace ambient::core
