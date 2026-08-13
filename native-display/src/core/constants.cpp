// constants.cpp — constants.hpp は inline constexpr だけなので実装は無い。
//
// このファイルの役割:
//   1. 移植性ゲート (tools/check-esp-portability.sh) に constants.hpp を
//      ESP32-P4 でコンパイルさせる（どの .cpp からも include されないヘッダは
//      検証対象にならない）。
//   2. 移植元の値との対応を static_assert で固定する。
//      値を「なんとなく」変えたらここで落ちる。

#include "ambient/core/constants.hpp"

namespace ambient::core::constants {
namespace {

// ---- 写真スケジューラ（出典 app/static/js/scheduler.js:25-34）--------
static_assert(kTripMax == 6, "TRIP_MAX");
static_assert(kPriNever == 45.0F, "PRI_NEVER");
static_assert(kPriNewMax == 12.0F, "PRI_NEW_MAX");
static_assert(kNewHalflifeDays == 4.0F, "NEW_HALFLIFE_D（新着ブーストの半減期）");
static_assert(kPriAnnivMax == 3.0F, "PRI_ANNIV_MAX");
static_assert(kAnnivWindowDays == 14.0F, "ANNIV_WINDOW_D（記念日の窓）");
static_assert(kPriFav == 2.0F, "PRI_FAV");
static_assert(kPriQualityRecommended == 6.0F, "PRI_QUALITY_RECOMMENDED");
static_assert(kPriQualityAlternate == -8.0F, "PRI_QUALITY_ALTERNATE");
static_assert(kPriJitter == 4.0F, "PRI_JITTER");
static_assert(kNewInsertWindow == 12, "NEW_INSERT_WINDOW");

// ★飢餓なし（全網羅）保証の根拠。
//   ブーストの合計は有界だが staleness は上限なく増えるので、
//   長く出ていない写真は必ずいつか追い越す。
//   未表示写真 (45pt) を最大ブースト付きの既表示写真が超えるには
//   staleness > 72 が必要。
static_assert(kPriNewMax + kPriAnnivMax + kPriFav + kPriQualityRecommended + kPriJitter == 27.0F,
              "ブーストの上限が 27pt であること（飢餓なし保証の前提）");
static_assert(kPriNever >
                  kPriNewMax + kPriAnnivMax + kPriFav + kPriQualityRecommended + kPriJitter,
              "未表示写真は常に最優先クラスにいること");

// ---- 地震アラート（出典 app/static/js/events.js:174-232）-------------
static_assert(kSeenQuakeMax == 500, "SEEN_EQ_MAX");
static_assert(kQuakeMinIntensity == 3, "震度3以上のみイベント化");
static_assert(kQuakeGlobalCooldown.value == 600000, "群発対策のグローバルクールダウン 10 分");

// ---- hogo-meter 同期（出典 hogo_common/server_sync.h ほか）------------------
static_assert(kPollInterval.value == 30000, "POLL_INTERVAL_MS");
static_assert(kHttpTimeout.value == 5000, "HTTP_TIMEOUT_MS");
static_assert(kRecordQueueSize == 8, "QUEUE_SIZE");
static_assert(kRecordMaxRetries == 5, "RECORD_MAX_RETRIES（初回1回 + 再試行4回 = 総試行5回）");
static_assert(kRecordRetryInterval.value == 5000, "RETRY_INTERVAL_MS");
static_assert(kToastTotal.value == 2000, "TOAST_TOTAL_SEC = 2");
static_assert(kLabelMaxBytes == 18, "LABEL_MAX_BYTES（バイト数であって文字数ではない）");
static_assert(kCareItemCount == 6, "CARE_ITEM_COUNT");
static_assert(kMaxHttpRequestsPerUpdate == 1, "1 回の update() で HTTP は最大 1 件");

// ---- 永続化の予算 ----------------------------------------------------------
static_assert(kTab5NvsPartitionBytes == 20480, "Tab5 の NVS パーティションは 20KB");
static_assert(kNvsMaxKeyBytes == 15, "NVS のキー長上限");

// ★容量問題の可視化。
//   shownが設計値2000件ぶん"id:round"で十数KBになり、
//   NVS 20KB（うち hogo-meter が 4 namespace 使用中）には入らない。
//   ここが「Mac では通り Tab5 で破綻する」の本体である。
//   詳細と選択肢は ambient/core/photo/snapshot.hpp を参照。
static_assert(kDesignPhotoCount * 7 > kTab5NvsPartitionBytes / 2,
              "shown を無制限に持つと NVS 予算を明らかに超えること（設計上の前提）");

}  // namespace
}  // namespace ambient::core::constants
