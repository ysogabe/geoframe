// ambient/core/constants.hpp — 既存実装から移植した定数
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★このファイルの値を変えないこと
// ============================================================================
//
// ここにある値はすべて **稼働中の実装から移植したもの** であり、
// 実運用で挙動が確認されている。「なんとなくこちらの方が良さそう」で
// 変えてよいものは 1 つも無い。
//
// 各定数に出典（ファイルと行）を書いてある。挙動を変えたくなったら、
// まず出典側の実装と、なぜその値なのかを読むこと。
// 変えるなら出典側と同時に変え、docs/CORE_CONTRACTS.md に記録すること。

#pragma once

#include <cstddef>
#include <cstdint>

#include "ambient/core/clock.hpp"

namespace ambient::core::constants {

// ============================================================================
// 写真スケジューラ
// 出典: app/static/js/scheduler.js:25-34
// ============================================================================

// TRIP_MAX — 1 トリップから 1 ラウンドに出す最大枚数。
// ★全網羅の周期を決めるのは主にこの値。N 枚のトリップは 1 ラウンドに
//   TRIP_MAX 枚出るため ceil(N / TRIP_MAX) ラウンドで必ず一巡する。
inline constexpr std::uint32_t kTripMax = 6;

// PRI_NEVER — 一度も表示していない写真の基礎スコア。
// ★飢餓なし（全網羅）保証の要。ブーストの合計は
//   PRI_NEW_MAX + PRI_ANNIV_MAX + PRI_FAV + PRI_QUALITY_RECOMMENDED + PRI_JITTER
//   = 27pt で有界だが、
//   staleness（最終表示からの経過ラウンド数）は上限なく増える。
//   よって長く出ていない写真は必ずいつか追い越して選出される。
//   既表示写真が 45 を超えるには staleness > 66 が必要 =
//   未表示写真は常に最優先クラスにいる。
inline constexpr float kPriNever = 45.0F;

// PRI_NEW_MAX — 新着ブースト最大。fetched_at から半減期 4 日で指数減衰。
inline constexpr float kPriNewMax = 12.0F;

// NEW_HALFLIFE_D — 新着ブーストの半減期（日）。
inline constexpr float kNewHalflifeDays = 4.0F;

// PRI_ANNIV_MAX — 記念日ブースト最大。
// 撮影月日と今日の**環状**距離（年跨ぎ考慮。12/30 と 1/2 は距離 3 日）、
// 窓 14 日で線形減衰。
inline constexpr float kPriAnnivMax = 3.0F;

// ANNIV_WINDOW_D — 記念日ブーストの窓（日）。
inline constexpr float kAnnivWindowDays = 14.0F;

// PRI_FAV — お気に入りブースト。
inline constexpr float kPriFav = 2.0F;

// 品質PoCの類似グループ内順位。完全重複の非代表はserver側でmanifestから除外する。
// 非推奨も有限減点に留めるため、stalenessが増えれば必ず再生対象へ戻る。
inline constexpr float kPriQualityRecommended = 6.0F;
inline constexpr float kPriQualityAlternate = -8.0F;

// PRI_JITTER — ランダムゆらぎ。[0, PRI_JITTER) を加算する。
// ★ここが SchedulerRng.unit() の消費点。写真 1 枚につき 1 回。
inline constexpr float kPriJitter = 4.0F;

// NEW_INSERT_WINDOW — 新着をキュー先頭から何位置以内に差し込むか。
// ★ここが SchedulerRng.belowN() の消費点。新着 1 枚につき 1 回。
inline constexpr std::uint32_t kNewInsertWindow = 12;

// 容量設計で想定する写真件数。合成した規模値であり、上限値ではない。
inline constexpr std::size_t kDesignPhotoCount = 2000;

// ============================================================================
// 地震アラート
// 出典: app/static/js/events.js:174-232
// ============================================================================

// SEEN_EQ_MAX — 既知地震 ID の保持上限。
// 長期稼働で際限なく増えないようにするためのリング（挿入順 = 古い順に捨てる）。
inline constexpr std::size_t kSeenQuakeMax = 500;

// イベント化する最小震度。
// ★maxi は文字列で "5-" "5+" のような値がありうる。std::stoi は使えない。
//   parseIntPrefix()（ambient/core/model/parse.hpp）で整数部だけ取ること。
inline constexpr int kQuakeMinIntensity = 3;

// 群発地震対策のグローバルクールダウン。
// ★同一イベント ID のクールダウンではない。**全体で 10 分**。
//   群発でイベントが連鎖しないようにするためのもの。
//   ID ごとのクールダウンに読み替えると、群発時に連続発火して意味を失う。
inline constexpr Millis kQuakeGlobalCooldown{10 * 60 * 1000};

// 他イベント表示中は発火しない（events.js の `Date.now() >= eventUntil` に対応）。
// これは定数ではなく規律。AlertEngine のコメントに書いてある。

// ============================================================================
// hogo-meter 同期
// 出典: <path-to-hogo-meter>/firmware/lib/hogo_common/server_sync.h
//       （★読み取り専用。変更禁止）
// ============================================================================

// POLL_INTERVAL_MS (server_sync.h:202) — config poll の間隔。
// ★起動直後は間隔を待たずに即時 1 回ポーリングする。
inline constexpr Millis kPollInterval{30000};

// HTTP_TIMEOUT_MS (server_sync.h:207) — 1 リクエストのタイムアウト。
// 接続・TLS ハンドシェイク・読取の各フェーズに効くため、最悪積算は約 15 秒。
inline constexpr Millis kHttpTimeout{5000};

// QUEUE_SIZE (server_sync.h:222) — 未送信 record キューの固定長。
// ★満杯時は epoch が最小（= 最も古い）要素を上書きする。
//   直近の記録の方が重要だという判断（出典側 cpp に根拠が詳述されている）。
inline constexpr std::size_t kRecordQueueSize = 8;

// RECORD_MAX_RETRIES (server_sync.h:223) — 再試行回数。
// ★初回 1 回 + 再試行 4 回 = **総試行 5 回**。「5 回再試行」ではない。
inline constexpr std::uint8_t kRecordMaxRetries = 5;

// RETRY_INTERVAL_MS (server_sync.h:225) — 再試行間隔。
inline constexpr Millis kRecordRetryInterval{5000};

// TOAST_TOTAL_SEC (firmware/variants/tab5/ui.h:26) — 遅延確定トーストの猶予秒数。
// 取り消し猶予がこの時間。詳細は ambient/core/care/pending_record.hpp。
inline constexpr Millis kToastTotal{2 * 1000};

// LABEL_MAX_BYTES / LABEL_MAX_LEN (server_sync.h:21) — 表示ラベルの上限。
// ★UTF-8 の **バイト数** 上限であって文字数ではない（全角 6 文字 = 18 バイト）。
inline constexpr std::size_t kLabelMaxBytes = 18;

// お世話項目のスロット数上限 (app_types.h:20 CARE_ITEM_COUNT)。
inline constexpr std::size_t kCareItemCount = 6;

// ★1 回の update() で HTTP は最大 1 件。
//   未送信 record キューがあれば record 送信を config poll より優先する。
//   （出典 server_sync.h の update() コメント。1 tick の停止時間を
//     1 リクエストぶんに抑えるための規律であり、性能上の要請ではない）
inline constexpr std::size_t kMaxHttpRequestsPerUpdate = 1;

// ★記録の状態（0-3）と点灯要否は **サーバが計算して返す**。
//   Mac 側でしきい値計算を再実装してはいけない。二重管理になり必ずずれる。
//   （ScheduleStatus: 0=None / 1=Green / 2=Yellow / 3=Red, app_types.h:53）

// ============================================================================
// 永続化の予算
// ============================================================================

// Tab5 の NVS パーティションサイズ。
// ★これは「この端末が使える総量」であり、**うち hogo-meter が既に
//   4 つの namespace を使っている**。ambient が使えるのはその残りでしかない。
//   実際に使ってよい量は Tab5 移植時に実測して決めること（未決定）。
inline constexpr std::size_t kTab5NvsPartitionBytes = 20 * 1024;

// NVS のキー長上限（NUL を除く）。
// ★"geoframe.play.v1" のような長いキーは Tab5 で通らない。
inline constexpr std::size_t kNvsMaxKeyBytes = 15;

}  // namespace ambient::core::constants
