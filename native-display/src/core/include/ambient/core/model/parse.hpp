// ambient/core/model/parse.hpp — サーバ応答の値を読むための小さなパーサ群
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ここにあるのは「JSON パーサ」ではない。JSON の構造解析は別の担当
// （fixture 実装 = レーン 1D、および transport 層）。
// ここにあるのは **文字列で返ってくる値を型に落とす** ための関数だけ。

#pragma once

#include <optional>
#include <string_view>

#include "ambient/core/clock.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// parseIntPrefix ― 震度のために必要
// ----------------------------------------------------------------------------
//
// /api/env の quake item は mag も maxi も **文字列** で返る。
// しかも maxi は震度なので "5-" "5+" のような値がありうる（気象庁の震度階級）。
//
//   ★std::stoi は使えない。"5-" で 5 は取れるが、例外を投げる入力がある。
//     例外を制御フローに使わないのが Core の規律（ESP32 は緊急用プールが 0）。
//   ★std::atoi も使わない。エラーと 0 が区別できない。
//
// この関数は先頭の（省略可能な符号付き）整数部だけを読む。
//   "5-"   -> 5
//   "5+"   -> 5
//   "3"    -> 3
//   "-1"   -> -1
//   " 4"   -> 4     （先頭の空白は読み飛ばす）
//   ""     -> nullopt
//   "abc"  -> nullopt
//   "-"    -> nullopt
// オーバーフローする入力は nullopt を返す（飽和させない。異常値を静かに通さない）。
[[nodiscard]] std::optional<int> parseIntPrefix(std::string_view text) noexcept;

// ----------------------------------------------------------------------------
// 日時
// ----------------------------------------------------------------------------

// 暦上の日時。タイムゾーンオフセット付き。
struct CivilTime {
  int year{0};
  int month{0};   // 1-12
  int day{0};     // 1-31
  int hour{0};    // 0-23
  int minute{0};  // 0-59
  int second{0};  // 0-59
  int utcOffsetMinutes{0};

  [[nodiscard]] bool operator==(const CivilTime&) const noexcept = default;
};

// ISO 8601 固定幅 "YYYY-MM-DDTHH:MM:SS+09:00"（25 文字）を読む。
// /api/env の quake item の `at` がこの形式。
//
// ★std::get_time を使わないこと。
//   重く（ロケール機構を引きずる）、ESP32 のロケール対応が薄い。
//   サーバ側で解析可能性が保証済みの固定幅なので、substring -> int の
//   素朴なパーサで十分である。
//
// ★ただし「固定幅だから長さ検査を省いてよい」ではない（1B 実装時の判断）。
//   この文字列は **認証の無い LAN 上のサーバ**から来る。攻撃者・バグ・
//   途中で切れた応答のいずれでも短い文字列が入りうるので、
//   添字アクセスの前に必ず長さを検査する。
//
// 受け付ける形（それ以外は nullopt）:
//   "YYYY-MM-DDTHH:MM:SS"        19 文字。オフセット無し → 0 とみなす
//   "YYYY-MM-DDTHH:MM:SSZ"       20 文字。オフセット 0
//   "YYYY-MM-DDTHH:MM:SS±HH:MM"  25 文字
//
// ★19 文字の形は /api/photos の `fetched_at`（実測 "2026-07-29T05:40:28"）が
//   これである。オフセットが無い値を UTC とみなすのは **こちらの決めごと**で、
//   実際にはサーバのローカル時刻（JST）である。新着ブーストは半減期 4 日
//   （= 5760 分）なので 540 分のずれは減衰率にして 6% 未満であり、
//   選出順に意味のある差を生まない。絶対時刻として他所で使うときは
//   このずれを思い出すこと。
//
// 日付の妥当性も検査する（うるう年を含む月末日、時分秒の範囲）。
// 区切り文字位置が違うもの、数字でない桁があるものは nullopt。
[[nodiscard]] std::optional<CivilTime> parseIso8601Fixed(std::string_view text) noexcept;

// 写真の `time` "YYYY-MM-DD HH:MM"（16 文字）を読む。
// ★キーは `takenAt` ではなく `time`。区切りは 'T' ではなく **半角スペース**。
//   秒もタイムゾーンも無いので、utcOffsetMinutes は 0、second は 0 になる。
//   タイムゾーンが無い以上、これは「撮影地のローカル時刻」であって
//   絶対時刻ではない。絶対時刻として扱わないこと（記念日判定は月日しか使わない）。
//
// ★長さは厳密に 16 文字。短い入力で範囲外アクセスを起こさないこと。
//   末尾に余分な文字が付いた 17 文字以上も受け付けない（静かに切り捨てると
//   壊れた応答を正常として通してしまう）。
[[nodiscard]] std::optional<CivilTime> parsePhotoTime(std::string_view text) noexcept;

// CivilTime を Unix epoch 秒へ。utcOffsetMinutes を考慮する。
// 実装は days-from-civil（Howard Hinnant の proleptic Gregorian 変換）。
// ★std::mktime を使わないこと（TZ 環境変数に依存し、ESP32 で挙動が変わる）。
[[nodiscard]] WallTime toWallTime(const CivilTime& civil) noexcept;

// toWallTime の逆。epoch 秒を、指定オフセットのローカル暦へ戻す。
// （civil_from_days。days-from-civil と対になる Howard Hinnant のアルゴリズム）
//
// ★用途: 記念日ブーストの「今日の月日」を出すこと。
//   写真の `time` は撮影地のローカル時刻なので、比較する「今日」も
//   ローカル暦でなければ日付が 1 日ずれる（JST の 0:00〜9:00 が前日になる）。
//   オフセットは呼び出し側が持つ（Core は時計もタイムゾーンも引かない）。
[[nodiscard]] CivilTime toCivilTime(WallTime wall, int utcOffsetMinutes) noexcept;

// 年内通日（1/1 = 0）。うるう年は考慮しない固定表を使う。
// 記念日ブーストの環状距離計算に使う（年をまたぐ距離を出すため）。
//
// ★うるう年の扱い（1B が決めたこと）:
//   固定表（2 月は 28 日）で cum[month-1] + (day-1) を返す。したがって
//   **2/29 は 3/1 と同じ通日 59 になる**。捨てずに丸めるのは、
//   出典 scheduler.js が Date.UTC(2001, m-1, d)（2001 年は平年）で
//   同じ繰り上がりをしているためで、移植としてはこれが忠実である。
//   影響は「2/29 撮影の写真の記念日ブーストが 1 日ずれる」だけで、
//   ブーストは最大 3pt、窓は 14 日なので選出順への影響は無視できる。
//   同じ理由で 4/31 のような存在しない日も繰り上げる（= 5/1）。
//   月が 1..12、日が 1..31 の範囲外なら nullopt。
[[nodiscard]] std::optional<int> dayOfYearNoLeap(int month, int day) noexcept;

}  // namespace ambient::core
