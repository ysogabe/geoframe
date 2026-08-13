// ambient/core/care/care_model.hpp — お世話（hogo-meter）の表示モデルと claim
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★状態（Green/Yellow/Red）と点灯要否は **サーバが計算する**
// ============================================================================
//
// constants.hpp:140-142 と screens.hpp 冒頭が明言しているとおり、しきい値計算を
// こちら側で再実装してはいけない。正典は
//
//   dashboard/src/lib/schedule-status.ts
//     computeScheduleStatus()  → status (0=None/1=Green/2=Yellow/3=Red)
//     shouldLightScreen()      → shouldLight
//
// であり、`/api/agent/:id/config` の各 item にその **結論** が載って降りてくる
// (dashboard/src/lib/repositories/care-item-repository.ts:159-168)。
// このファイルがやってよいのは「結論 → 表示・claim」への写像だけである。
//
// ★firmware も Mac も一クライアントに過ぎない。契約の裁定者は dashboard API である。
//   詳細と file:line + SHA-256 は tests/contract/HOGO_CONTRACT.md。
//
// ============================================================================
// ★保留（2 秒の取り消し猶予）は作り直さない
// ============================================================================
//
// D-4 の不変条件は ambient/core/care/pending_record.hpp に **実装済み** である。
//
//   > 保留は「取り消しボタンへのヒット」でのみ破棄される。
//   > それ以外のすべての離脱経路は、離脱前に確定する。
//
// CareModel はその PendingCareRecord を **メンバとして持ち**、離脱 API を
// そのまま素通しする。ここに 2 個目の保留状態を作らないこと。
//
// ★CareModel の beginRecord/leave/tick は、確定した記録を返すと同時に
//   自分の「ローカル最終記録時刻」へも反映する（noteCommitted 相当を内部で呼ぶ）。
//   呼び出し側に残る責務は **SyncPolicy へ積むこと 1 つだけ** にしてある。
//   戻り値は [[nodiscard]] なので捨てられない。
//
// ============================================================================
// ★RecordedAwaitingServer ― 「記録したら即 Green」は成立しない
// ============================================================================
//
// 記録 POST が 204 で成功しても、状態色はサーバが計算するので
// **次の config poll（最大 30 秒後）まで変わらない**
// （docs/CORE_CONTRACTS.md §5.4 / display_state.hpp）。
//
// この間の表示は「ローカルの記録時刻を経過時間に反映しつつ、色はサーバ待ち」である。
// 経過時間に使う値は上流と同じく **max(ローカル, サーバ)** にする
// (firmware/variants/tab5/main.cpp:156-157 の std::max)。
//
// ★寿命を必ず付けること ← ここが罠
//
//   RecordedAwaitingServer は **最強の優先度** である（claim.hpp:105-113）。
//   記録 POST が総試行 5 回を使い切って破棄された場合、サーバは永久にその記録を
//   知らないので、「サーバが追いつくまで待つ」だけの実装だと claim が
//   **永久にラッチし、地震アラートまで抑止する**。
//
//   よって二重に止める:
//     1. collect() が kAwaitingServerMax を過ぎたら出すのをやめる（source 側の状態）
//     2. claim.expiresAt にも同じ期限を入れる（安全網）
//   claim.hpp:148-150 が「expiresAt は安全網であって source の状態管理の
//   代わりにはならない」と明記しているので、両方やる。
//
//   既定 90 秒 = poll 間隔 30 秒 × 3。通常の離脱経路は applyConfig() であり、
//   これはあくまでラッチ破りである。
//
// ★もう 1 つのラッチ経路（6→3 の縮小）も塞いである:
//   6 項目で slot5 を記録した直後に config が 3 項目へ縮むと、slot5 は以後
//   present にならないので「サーバが追いついた」判定を通る機会が二度と来ない。
//   これも最強 claim を（期限までとはいえ）画面に無いカードのために出させる。
//   対策も二重:
//     1. applyConfig() が present でない slot の待ちを解除する
//     2. collect() の走査を careLayout() の範囲に限る（findDue() と同じ境界）
//
// ============================================================================
// ★Yellow も CareDue である（Care ではない）
// ============================================================================
//
// display_state.hpp:36-37 が `CareDue` を「お世話の期限超過（**Red / Yellow**）」と
// 定義している。また claim.hpp:101-104 は Care を「ユーザが明示的に開いた画面で
// 自動遷移ではない」と定義している。したがって:
//
//   status が Red / Yellow  → CareDue の claim（自動遷移）
//   ユーザがお世話画面を開いた → Care の claim（openCareScreen()）
//
// **status から Care を出さないこと。** Yellow を Care に落とすと、
// 「Yellow のあいだ環境カットに横取りされる」（Env < Care だが Env の周期カットが
// Care と競合しない保証は無い）という別の意味になる。
//
// ============================================================================
// ★label の 18 バイト上限 ― 上流と挙動が違う唯一の点
// ============================================================================
//
// 上流は char[LABEL_MAX_LEN + 1] へ strlcpy する (server_sync.cpp:321)。
// strlcpy は **バイト単位** で切るので、日本語ラベルでは UTF-8 の途中で切れて
// 不正なバイト列になりうる（全角 7 文字 = 21 バイト → 18 バイト目が
// 3 バイト文字の 1 バイト目や 2 バイト目に当たる）。
//
// こちらは **コードポイント境界まで戻して切る**。理由:
//   * SDL_ttf (TTF_RenderUTF8_Blended) は不正な UTF-8 でグリフが化ける／失敗する
//   * Tab5 の efont も同様に化ける
//   * 「18 バイト以内」という契約は満たしたまま、壊れた表示だけを避けられる
//
// ★なお dashboard は書き込み時点で 18 バイトを検証している
//   (dashboard/src/lib/validation.ts:29-31, types.ts:3) ので、
//   正常系では 18 バイトを超える label は降ってこない。これは
//   **防御的な二段目**であり、境界割れは実運用では到達しない想定である。
//   到達するのは API を直接叩かれた場合や、上流仕様が変わった場合。

#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/state/claim.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// config poll の取り込み結果（transport のパーサがこれを埋める）
// ----------------------------------------------------------------------------
//
// ★この型が core にあるのは、消費者（CareModel）が core にいるからである。
//   transport の parseHogoConfigJson() が「生の本文 → この型」を担当する
//   （geoframe_client の parseEnvJson が core::EnvSnapshot を埋めるのと同じ形）。

struct CareSlotConfig {
  // この poll でこの slot が返ってきたか（上流 _hasLabel 相当。server_sync.h:162）。
  bool present{false};

  // ★18 バイトへ丸め済み（UTF-8 境界を割らない）。パーサが truncateLabelUtf8() を通す。
  std::string label;

  // サーバ側の最終記録時刻。0 = 未記録（上流 server_sync.h:70 の意味論）。
  WallTime lastEpoch{};

  // ★サーバが計算した結論。こちらで計算し直さないこと。
  ScheduleStatus status{ScheduleStatus::None};

  // ★未取得時は安全側で true（点灯）。上流 server_sync.h:171 と同じ既定
  //   （ここを false 既定にすると「画面が点灯しない」へ意味が反転する）。
  bool shouldLight{true};
};

// config の "firmware" オブジェクト。
// ★version / sha256 / sig の 3 つすべてが非空のときだけ present になる
//   (server_sync.cpp:356-364)。sig が空 = 未署名オファーは掴まない。
// ★キーが無い poll では present が false へ戻る（残留させない）。
struct HogoFirmwareOffer {
  bool present{false};
  std::string version;
  std::string sha256;
  std::string sig;
  std::uint32_t attempt{0};
};

struct HogoConfigSnapshot {
  std::array<CareSlotConfig, constants::kCareItemCount> slots{};

  // この poll が返した有効スロット数（= 見えた最大 slot + 1）。
  // ★上流 activeItemCount() は int だが、こちらは添字に使うので std::size_t。
  //   意味は同じ（poll ごとに数え直すので 6→3 の縮小に追従する。server_sync.cpp:336-338）。
  std::size_t activeSlots{0};

  HogoFirmwareOffer firmware{};

  // ★キー省略が正常系。キーが無い poll では必ず false へ戻る
  //   (server_sync.cpp:292 の `| false`。Web 側での取り消しに追従するため)。
  bool deregisterRequested{false};
};

// ----------------------------------------------------------------------------
// 3 / 6 バケット化 ― 「呼び出し側の責務」と上流が明記している部分
// ----------------------------------------------------------------------------
//
// 上流 server_sync.h:96-98:
//   > 3 or 6 へのバケット化・未 poll 時のフォールバック(既定 3)は
//   > 呼び出し側 tab5 main の責務とし、ここは生の数だけ返して機種ポリシーを持たない
//
// 実装は firmware/variants/tab5/main.cpp:140-142 の 1 行:
//   return (serverSync.activeItemCount() >= 4) ? 6 : 3;
//
// ★0（未 poll）も 3 に落ちる。この境界を変えないこと。
//
// ★自己登録経路では slotCount=3 に固定される
//   (dashboard/src/lib/repositories/device-repository.ts:37-41 MODEL_SLOT_COUNT)。
//   したがって **6 項目レイアウトの実 API 試験は原理的に不可能** であり、
//   6 項目は fixture / 合成 snapshot で担保する。

struct CareLayout {
  std::size_t itemCount{3};
  std::size_t rows{1};
  std::size_t columns{3};
};

[[nodiscard]] CareLayout careLayout(std::size_t activeSlots) noexcept;

// ----------------------------------------------------------------------------
// 純粋関数（上流からの移植）
// ----------------------------------------------------------------------------

// 経過時間の日本語相対表記。
// 出典: firmware/lib/hogo_common/ui_model.cpp:13-36（ロジックを変えないこと）
//
//   lastEpoch == 0        -> "未記録"
//   diff < 60             -> "たった今"     （now == last も含む）
//   diff < 3600           -> "N分前"
//   diff < 86400          -> "H時間前" / "H時間M分前"（分が 0 なら時間だけ）
//   それ以上              -> "D日前"
//
// ★巻き戻り（now < last。NTP 再同期など）は 0 にクランプする（上流と同じ）。
// ★上流は uint32_t だが WallTime は int64。narrowing しない。
[[nodiscard]] std::string formatElapsed(WallTime lastEpoch, WallTime now);

// UTF-8 の境界を割らずに maxBytes まで丸める。
// ★冒頭の節「label の 18 バイト上限」を読むこと（上流との唯一の挙動差）。
[[nodiscard]] std::string truncateLabelUtf8(std::string_view label,
                                            std::size_t maxBytes = constants::kLabelMaxBytes);

// ----------------------------------------------------------------------------
// 表示モデル（描画層へ写すための「結論」）
// ----------------------------------------------------------------------------

struct CareCardView {
  std::size_t slot{0};
  std::string label;
  std::string elapsed;      // formatElapsed の結果
  WallTime lastEpoch{};     // ★max(ローカル, サーバ)
  ScheduleStatus status{ScheduleStatus::None};
  bool shouldLight{true};
  bool awaitingServer{false};  // ローカル記録がまだサーバに反映されていない
  bool pending{false};         // 取り消し猶予中（このカードを今押した）
};

struct CareView {
  CareLayout layout{};
  std::vector<CareCardView> cards;  // layout.itemCount 件
  bool configReceived{false};       // config poll が一度でも成功したか

  // 期限到来（Red / Yellow）のうち最優先の 1 件。無ければ nullopt。
  std::optional<std::size_t> dueSlot{};
  ScheduleStatus dueStatus{ScheduleStatus::None};

  // 取り消し猶予の残り（表示用）。保留中でなければ 0。
  Millis undoRemaining{};
  std::optional<CareItem> pendingItem{};
};

// ----------------------------------------------------------------------------
// CareModel
// ----------------------------------------------------------------------------
//
// ★単一スレッド前提（Core の規律）。
class CareModel final : public IClaimSource {
 public:
  // この source の識別子。EnvironmentModel::kSourceId / AlertEngine::kSourceId と
  // 衝突しない値（'CARE' の ASCII）。
  static constexpr std::uint32_t kSourceId = 0x43'41'52'45;

  // RecordedAwaitingServer のラッチ破り。既定 = poll 間隔 × 3。
  // ★通常の離脱は applyConfig()。ここは「記録が破棄されてサーバが永久に
  //   追いつかない」場合に最強 claim が固着するのを防ぐためだけにある。
  static constexpr Millis kAwaitingServerMax{constants::kPollInterval.value * 3};

  CareModel() = default;

  // ---- 記録操作（PendingCareRecord の素通し） ------------------------------
  //
  // ★戻り値は「確定した記録」。SyncPolicy::enqueue() へ渡すこと。
  //   捨てるとユーザが押した記録がサーバへ届かない（[[nodiscard]]）。

  // 保留を開始する。既存の保留があれば OtherCardTapped 相当で **確定** して返す。
  [[nodiscard]] std::optional<CommittedRecord> beginRecord(CareItem item, SteadyTime now,
                                                           WallTime wallNow);

  // ★唯一の離脱 API。UndoHit だけが破棄、それ以外はすべて確定。
  //   新しい離脱経路は PendingExit に値を足すだけでよい（pending_record.hpp）。
  //
  // ★now を取るのは PendingCareRecord::leave() との差分。確定した瞬間が
  //   RecordedAwaitingServer の起点になるため、単調時刻がここで要る。
  //   （内部に「最後に見た now」を溜め込むと状態が増え、呼ばれ方に依存する）。
  [[nodiscard]] std::optional<CommittedRecord> leave(PendingExit reason, SteadyTime now);

  // トースト満了の検知。毎 tick 呼ぶ。
  [[nodiscard]] std::optional<CommittedRecord> tick(SteadyTime now);

  // 確定済みの記録をローカルへ反映する（RecordedAwaitingServer の起点）。
  // ★通常は上の 3 つが内部で呼ぶので、直接呼ぶ必要はない。
  //   永続化からの復元など、保留を経ない経路のために公開してある。
  void noteCommitted(const CommittedRecord& record, SteadyTime now) noexcept;

  // ---- config poll の取り込み ----------------------------------------------

  // ★成功した poll の結果だけを渡すこと（失敗時は前回値を温存する。上流と同じ）。
  //   サーバの lastEpoch がローカルに追いついた slot は awaiting を解除する。
  //
  // ★now を取らないのは意図。解除の判定は epoch の比較だけで決まり、
  //   単調時刻を混ぜると「poll した時刻で待ち時間が延びる」という別の意味になる。
  void applyConfig(const HogoConfigSnapshot& snapshot);

  // ---- お世話画面（ユーザ操作由来） ----------------------------------------
  //
  // ★status からは Care を出さない（冒頭の節）。Care はユーザが明示的に
  //   開いたときだけ。duration を過ぎたら自動的に消える。
  void openCareScreen(SteadyTime now, Millis duration = Millis{20000}) noexcept;
  void closeCareScreen() noexcept;

  // ---- 時間跳躍（スリープ復帰 / NTP 同期） ---------------------------------
  void onTimeJump(const TimeJump& jump) noexcept;

  // ---- 出力 ----------------------------------------------------------------

  // ★const。now の純粋な関数であること（レベルトリガ。claim.hpp §5.3）。
  //   出すのは 0 個か 1 個。強い順に
  //     RecordedAwaitingServer > CareDue(Red/Yellow) > Care(画面を開いている間)
  void collect(SteadyTime now, std::vector<Claim>& out) const override;

  [[nodiscard]] CareView view(SteadyTime now, WallTime wallNow) const;

  // ---- 観測 ----------------------------------------------------------------

  [[nodiscard]] bool configReceived() const noexcept { return configReceived_; }
  [[nodiscard]] const HogoConfigSnapshot& snapshot() const noexcept { return snapshot_; }
  [[nodiscard]] CareLayout layout() const noexcept { return careLayout(snapshot_.activeSlots); }
  [[nodiscard]] bool awaitingServer(SteadyTime now) const noexcept;
  [[nodiscard]] WallTime lastEpochFor(CareItem item) const noexcept;
  [[nodiscard]] const PendingCareRecord& pending() const noexcept { return pending_; }
  [[nodiscard]] PendingCareRecord& pending() noexcept { return pending_; }

  void setAwaitingServerMax(Millis limit) noexcept { awaitingMax_ = limit; }

 private:
  // 期限到来のうち最優先の 1 件（Red 優先、次に Yellow。どちらも slot 昇順）。
  struct DueSlot {
    std::optional<std::size_t> slot{};
    ScheduleStatus status{ScheduleStatus::None};
  };

  [[nodiscard]] static std::size_t indexOf(CareItem item) noexcept;
  [[nodiscard]] DueSlot findDue() const noexcept;

  PendingCareRecord pending_{};
  HogoConfigSnapshot snapshot_{};
  bool configReceived_{false};

  // ローカルで確定した最終記録時刻（slot ごと）。サーバが追いつくまでの拠り所。
  std::array<WallTime, constants::kCareItemCount> localLastEpoch_{};

  // RecordedAwaitingServer の起点（slot ごと）。nullopt = 待っていない。
  std::array<std::optional<SteadyTime>, constants::kCareItemCount> awaitingSince_{};

  std::optional<SteadyTime> careScreenUntil_{};
  Millis awaitingMax_{kAwaitingServerMax};
};

}  // namespace ambient::core
