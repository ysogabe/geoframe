// ambient/core/care/sync_policy.hpp — hogo-meter 同期の「判断」だけを持つ純粋ポリシー
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★なぜ core に置くのか（transport ではなく）
// ============================================================================
//
// Tab5 では HTTP の実装が別物になる（Mac: libcurl / Tab5: HTTPClient +
// WiFiClientSecure）。しかし **キュー管理・リトライ規律・優先制御は同じ** である。
// これらを transport に置くと、そっくり同じ判断ロジックを 2 回書くことになり、
// しかも移植性ゲート（tools/check-esp-portability.sh は src/core/ しか見ない）の
// 射程外になるので、ズレても誰も気づけない。
//
// docs/PORTABILITY.md が明言しているとおり:
//
//   > 移植したい純粋なポリシー（送信キュー管理・リトライ規律・優先制御など、
//   > I/O を含まない判断ロジック）が ambient_transport に入っていたら、
//   > それは配置ミスです。ambient_core へ移してください。
//
// ============================================================================
// ★このクラスは IHttpClient に触らない ― 「決める」と「送る」を分けてある
// ============================================================================
//
// 素朴に書くと SyncPolicy::update(IHttpClient&) になるが、そうすると
// URL の組み立て（transport の責務）が core に染み出すか、あるいは
// 「HTTP を投げる抽象」をもう 1 枚被せる羽目になる。
//
// 代わりに、このクラスは **「この tick に何をすべきか」を 1 個だけ返す**。
//
//   SyncAction action = policy.nextAction(now, ready);   // 純粋。副作用なし
//   ... 呼び出し側（transport の HogoSync）が action を 1 件だけ実行する ...
//   policy.applyResult(action, success, completedAt);    // 結果を戻す
//
// この形の利点:
//
//   * **「1 回の update() で HTTP は最大 1 件」が構造的に成立する。**
//     nextAction() が 1 個しか返さないので、呼び出し側が 2 本投げる書き方に
//     ならない（constants::kMaxHttpRequestsPerUpdate = 1）。
//   * SyncPolicy が unit_tests（ambient_core だけにリンク）で完結して試験できる。
//     キュー満杯の追い出しも総試行 5 回も、HTTP を一切模さずに固定できる。
//   * Tab5 側は「action を実行する 30 行」だけを書き直せばよい。
//
// ============================================================================
// 移植元（★読み取り専用。値を変えないこと）
// ============================================================================
//
//   <path-to-hogo-meter>/firmware/lib/hogo_common/server_sync.h
//   <path-to-hogo-meter>/firmware/lib/hogo_common/server_sync.cpp
//
// 基準コミットと引用箇所の SHA-256 は tests/contract/HOGO_CONTRACT.md にある。
// 定数の実体は ambient/core/constants.hpp（出典つき）。
//
// 移植した規律:
//
//   * POLL_INTERVAL_MS = 30000。**起動直後は間隔を待たずに即時 1 回**
//     (server_sync.cpp:73 `if (!_polledOnce || ...)`)
//   * HTTP_TIMEOUT_MS = 5000（このクラスは値を持つだけ。効かせるのは transport）
//   * QUEUE_SIZE = 8。**満杯なら epoch 最小（最も古い）を上書き**
//     (server_sync.cpp:97-112)
//   * RECORD_MAX_RETRIES = 5 は **初回 1 回 + 再試行 4 回 = 総試行 5 回**
//     (server_sync.cpp:432-442。失敗のたび 1 減らし、0 になった時点で破棄)
//   * RETRY_INTERVAL_MS = 5000 (server_sync.cpp:441)
//   * **1 回の update() で HTTP は最大 1 件。未送信キューがあれば record 送信を
//     config poll より優先** (server_sync.cpp:60-67)
//
// ★送信キューは RAM のみで永続化しない（上流も同じ）。上流はローカル NVS を
//   正としており、キューは「ダッシュボードへの非同期な複製」でしかない
//   (server_sync.cpp:20-24)。永続化するのは slot ごとの最終 epoch だけである。
//
// ============================================================================
// ★上流の挙動をそのまま移した「意外な」点 ― 直さないこと
// ============================================================================
//
// **record がバックオフ中でも config poll は走らない。**
//
// 上流 update() は「キューに active な要素が 1 つでもあれば drainOneRecord() を
// 呼んで return」する (server_sync.cpp:62-67)。drainOneRecord() は送信期限
// (nextAttemptMs) が来ていなければ何もせず false を返す (server_sync.cpp:387-390)。
// つまり **5 秒のバックオフ中は config poll が飢える**。
//
// 直したくなるが、直さないこと。理由:
//
//   1. これは「1 tick の停止時間を 1 リクエストぶんに抑える」規律の帰結であり、
//      バグではない。バックオフ中に poll を入れると、リトライ 4 回ぶんの
//      20 秒の間に poll が最大 1 回混ざり、Tab5 と Mac で HTTP の発生順序が
//      変わる。順序が変わると「Mac では再現しない」不具合の温床になる。
//   2. 記録が滞留している状況で config の新しさを優先する理由が無い
//      （上流ヘッダ:55-57 が「お世話の記録が届く方が config の新しさより重要」と
//        明記している）。
//
// この性質は tests/contract/HOGO_CONTRACT.md に「意図的な挙動」として記録してある。

#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// この tick にすべきこと
// ----------------------------------------------------------------------------

enum class SyncActionKind : std::uint8_t {
  // 何もしない。★「HTTP を 1 件も投げない」であって「異常」ではない。
  None = 0,
  // 未送信 record を 1 件 POST する。
  SendRecord,
  // config を GET する。
  PollConfig,
};

inline constexpr std::size_t kSyncActionKindCount = 3;

[[nodiscard]] const char* toString(SyncActionKind kind) noexcept;

// 上流 update() の `wifiConnected && timeValid` ゲートに対応する。
//
// ★このゲートは「初回即時 poll」の判定より **前** にある (server_sync.cpp:56-58)。
//   したがって起動直後にネットワークが上がっていなくても即時 poll の権利は
//   消費されない。上がった瞬間に 30 秒待たず 1 回目が飛ぶ。
struct SyncReadiness {
  bool networkUp{true};
  bool timeValid{true};

  [[nodiscard]] constexpr bool ok() const noexcept { return networkUp && timeValid; }
};

struct SyncAction {
  SyncActionKind kind{SyncActionKind::None};

  // SendRecord のときだけ意味を持つ。applyResult() へそのまま返すこと。
  std::size_t queueIndex{0};
  CareItem item{CareItem::Toilet};
  WallTime recordedAt{};

  [[nodiscard]] constexpr bool none() const noexcept {
    return kind == SyncActionKind::None;
  }
};

// ----------------------------------------------------------------------------
// 固定長キューの 1 要素（server_sync.h:215-221 の PendingRecord に対応）
// ----------------------------------------------------------------------------

struct QueuedRecord {
  bool active{false};
  CareItem item{CareItem::Toilet};

  // ★壁時計。サーバへ送る絶対時刻であり、追い出しの比較キーでもある。
  WallTime recordedAt{};

  // 残り試行回数。enqueue 時に maxAttempts（= 5）。0 で破棄。
  std::uint8_t retriesLeft{0};

  // ★単調時刻。バックオフの期限。
  SteadyTime nextAttemptAt{};
};

// ----------------------------------------------------------------------------
// 設定 ― 既定は constants.hpp の移植値そのもの
// ----------------------------------------------------------------------------
//
// ★テストのために既定値を短くしないこと。FakeClock を進めれば実値のまま試験できる。
//   値を変えると「Mac では通るが Tab5 の実挙動と違う」テストになる。
struct SyncPolicyConfig {
  Millis pollInterval{constants::kPollInterval};
  Millis retryInterval{constants::kRecordRetryInterval};

  // ★総試行回数。「再試行回数」ではない（初回 1 回 + 再試行 4 回）。
  std::uint8_t maxAttempts{constants::kRecordMaxRetries};
};

// ----------------------------------------------------------------------------
// 追い出しの記録（診断用。捨てたことが画面に出ないので観測点を残す）
// ----------------------------------------------------------------------------

struct EnqueueOutcome {
  std::size_t index{0};   // 積んだ位置
  bool evicted{false};    // 満杯で 1 件追い出したか
  CareItem evictedItem{CareItem::Toilet};
  WallTime evictedAt{};
};

// ----------------------------------------------------------------------------
// SyncPolicy
// ----------------------------------------------------------------------------
//
// ★単一スレッド前提（Core の規律）。
class SyncPolicy {
 public:
  static constexpr std::size_t kQueueSize = constants::kRecordQueueSize;

  SyncPolicy() = default;
  explicit SyncPolicy(SyncPolicyConfig config) noexcept : config_(config) {}

  // ---- 入力 ----------------------------------------------------------------

  // 記録を送信キューへ積む。I/O は一切しない（上流 requestRecord と同じ）。
  //
  // ★満杯なら epoch が最小（= 最も古い）要素を上書きする。
  //   直近の記録の方が重要という判断で、ローカルの正データは別に永続化される前提
  //   (server_sync.cpp:93-96)。同値のときは添字の小さい方が残る
  //   （上流が strict `<` で走査するため。server_sync.cpp:99）。
  EnqueueOutcome enqueueRecord(CareItem item, WallTime recordedAt, SteadyTime now) noexcept;

  // PendingCareRecord が確定した記録をそのまま積む便宜版。
  EnqueueOutcome enqueue(const CommittedRecord& record, SteadyTime now) noexcept {
    return enqueueRecord(record.item, record.recordedAt, now);
  }

  // ---- 判断（純粋） --------------------------------------------------------

  // この tick にすべきことを 1 個だけ返す。副作用なし。何度呼んでも同じ。
  //
  // 決定順（上流 update() の構造をそのまま移した）:
  //   1. ready でなければ None（★即時 poll の権利は消費しない）
  //   2. active な record が 1 件でもあれば
  //        - 送信期限が来ているものがあれば SendRecord
  //        - 全件バックオフ中なら **None**（config poll はしない。冒頭の節を参照）
  //   3. 未 poll、または前回 poll から pollInterval 経過していれば PollConfig
  //   4. それ以外は None
  [[nodiscard]] SyncAction nextAction(SteadyTime now, SyncReadiness ready = {}) const noexcept;

  // ---- 結果の反映 ----------------------------------------------------------

  // nextAction() が返した action をそのまま渡すこと（queueIndex の取り違え防止）。
  //
  // ★completedAt は **リクエスト完了時刻** を渡すこと。上流は
  //   `_lastPollMs = millis()` / `nextAttemptMs = millis() + RETRY_INTERVAL_MS` を
  //   HTTP 呼び出しの **後** で取っている (server_sync.cpp:75, 441)。
  //   送信前の時刻を渡すと、5 秒タイムアウトが効いた場合に間隔が詰まる。
  //
  // ★呼び忘れると retriesLeft が減らず、同じ record を永久に送り続ける。
  //   HogoSync（transport）を通せば構造的に呼ばれる。
  void applyResult(const SyncAction& action, bool success, SteadyTime completedAt) noexcept;

  // ---- 時間跳躍（スリープ復帰 / NTP 同期） ---------------------------------
  //
  // clock.hpp のポリシーどおり、経過時間の基準点を跳躍ぶんだけ過去へずらす。
  // 「8 時間スリープしたのに次の poll が 30 秒後のまま」を避けるため
  //   （実際には 8 時間経っているので即時 poll が正しい）。
  // 巻き戻し（skew < 0）では何もしない（catchUp() が 0 を返す）。
  void onTimeJump(const TimeJump& jump) noexcept;

  // ---- 観測 ----------------------------------------------------------------

  [[nodiscard]] std::size_t activeCount() const noexcept;
  [[nodiscard]] bool hasPendingRecord(CareItem item) const noexcept;

  // 起動後に config poll が一度でも成功したか（上流 hasConfigPollSucceeded 相当）。
  // ★RAM のみ。再起動でリセットされる。
  [[nodiscard]] bool configPollSucceededOnce() const noexcept { return configPollOk_; }
  [[nodiscard]] bool polledOnce() const noexcept { return polledOnce_; }

  // 送信できずに諦めた件数（診断用）。★画面には出ないので、ここでしか観測できない。
  [[nodiscard]] std::size_t droppedRecords() const noexcept { return dropped_; }
  [[nodiscard]] std::size_t sentRecords() const noexcept { return sent_; }
  [[nodiscard]] std::size_t evictedRecords() const noexcept { return evicted_; }

  [[nodiscard]] const std::array<QueuedRecord, kQueueSize>& queue() const noexcept {
    return queue_;
  }
  [[nodiscard]] const SyncPolicyConfig& config() const noexcept { return config_; }

 private:
  SyncPolicyConfig config_{};
  std::array<QueuedRecord, kQueueSize> queue_{};
  SteadyTime lastPollAt_{};
  bool polledOnce_{false};
  bool configPollOk_{false};
  std::size_t dropped_{0};
  std::size_t sent_{0};
  std::size_t evicted_{0};
};

}  // namespace ambient::core
