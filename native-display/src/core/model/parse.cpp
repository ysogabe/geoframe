// parse.cpp
//
// parseIntPrefix() は先行実装。理由:
//   震度 "5-" / "5+" を std::stoi に通すと死ぬ、というのが実測済みの落とし穴で、
//   ここが空だと各レーンがそれぞれ独自の（そしてたいてい壊れた）変換を書く。
//
// 日時パーサ（parseIso8601Fixed / parsePhotoTime / toWallTime / toCivilTime /
// dayOfYearNoLeap）はレーン 1B が実装した。規律:
//   * std::get_time を使わない（重く、ESP32 のロケール対応が薄い）
//   * std::mktime を使わない（TZ 環境変数に依存する）
//   * ★固定幅でも長さ検査を省かない。入力は認証の無い LAN 上のサーバから来る

#include "ambient/core/model/parse.hpp"

#include <cstddef>
#include <cstdint>
#include <limits>

namespace ambient::core {

std::optional<int> parseIntPrefix(std::string_view text) noexcept {
  std::size_t i = 0;
  while (i < text.size() && (text[i] == ' ' || text[i] == '\t')) {
    ++i;
  }

  bool negative = false;
  if (i < text.size() && (text[i] == '+' || text[i] == '-')) {
    negative = (text[i] == '-');
    ++i;
  }

  if (i >= text.size() || text[i] < '0' || text[i] > '9') {
    return std::nullopt;  // 数字が 1 桁も無い。"" / "abc" / "-" がここ。
  }

  // long long で積んでから範囲検査する（int での符号付きオーバーフロー = UB を避ける）。
  constexpr long long kIntMax = static_cast<long long>(std::numeric_limits<int>::max());
  constexpr long long kIntMin = static_cast<long long>(std::numeric_limits<int>::min());
  constexpr long long kGuard = kIntMax / 10 + 1;

  long long acc = 0;
  while (i < text.size() && text[i] >= '0' && text[i] <= '9') {
    if (acc > kGuard) {
      return std::nullopt;  // 明らかに範囲外。静かに飽和させない。
    }
    acc = acc * 10 + static_cast<long long>(text[i] - '0');
    if (acc > kIntMax + 1) {
      return std::nullopt;
    }
    ++i;
  }

  const long long signedValue = negative ? -acc : acc;
  if (signedValue > kIntMax || signedValue < kIntMin) {
    return std::nullopt;
  }
  return static_cast<int>(signedValue);
}

namespace {

// ----------------------------------------------------------------------------
// 固定幅パーサの部品
// ----------------------------------------------------------------------------
//
// ★どの関数も添字アクセスの前に呼び出し側が長さを検査済みであること。
//   ヘッダが「固定幅」と書いているせいで長さ検査を省きたくなるが、
//   入力は認証の無い LAN 上のサーバから来る。

[[nodiscard]] constexpr bool isDigit(char c) noexcept { return c >= '0' && c <= '9'; }

// text[pos] から count 桁の 10 進数を読む。数字以外が混ざれば nullopt。
// count は最大 4（年の 4 桁）なのでオーバーフローしない。
[[nodiscard]] std::optional<int> fixedDigits(std::string_view text, std::size_t pos,
                                             std::size_t count) noexcept {
  if (pos + count > text.size()) {
    return std::nullopt;  // 短い入力。ここで止めないと範囲外アクセスになる。
  }
  int value = 0;
  for (std::size_t i = 0; i < count; ++i) {
    const char c = text[pos + i];
    if (!isDigit(c)) {
      return std::nullopt;
    }
    value = value * 10 + (c - '0');
  }
  return value;
}

[[nodiscard]] constexpr bool isLeapYear(int year) noexcept {
  return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
}

// 実在する日付か（うるう年を考慮する）。
// ★dayOfYearNoLeap の「繰り上げ」とは目的が違う。あちらは記念日ブーストの
//   通日で、こちらは「サーバから来た日時が壊れていないか」の検査である。
[[nodiscard]] constexpr bool isValidDate(int year, int month, int day) noexcept {
  if (month < 1 || month > 12 || day < 1) {
    return false;
  }
  constexpr int kDaysInMonth[12] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
  int limit = kDaysInMonth[month - 1];
  if (month == 2 && isLeapYear(year)) {
    limit = 29;
  }
  return day <= limit;
}

// "YYYY-MM-DD" の 10 文字と、続く時刻部の区切り文字を検査して読む。
// 日付と時分までを埋め、秒とオフセットは呼び出し側が続きを読む。
[[nodiscard]] std::optional<CivilTime> parseDateAndHourMinute(std::string_view text,
                                                              char dateTimeSeparator) noexcept {
  // 最低 16 文字 ("YYYY-MM-DD HH:MM") 必要。
  if (text.size() < 16) {
    return std::nullopt;
  }
  if (text[4] != '-' || text[7] != '-' || text[10] != dateTimeSeparator || text[13] != ':') {
    return std::nullopt;
  }

  const auto year = fixedDigits(text, 0, 4);
  const auto month = fixedDigits(text, 5, 2);
  const auto day = fixedDigits(text, 8, 2);
  const auto hour = fixedDigits(text, 11, 2);
  const auto minute = fixedDigits(text, 14, 2);
  if (!year || !month || !day || !hour || !minute) {
    return std::nullopt;
  }
  if (!isValidDate(*year, *month, *day)) {
    return std::nullopt;
  }
  if (*hour > 23 || *minute > 59) {
    return std::nullopt;
  }

  CivilTime civil{};
  civil.year = *year;
  civil.month = *month;
  civil.day = *day;
  civil.hour = *hour;
  civil.minute = *minute;
  return civil;
}

// days-from-civil (Howard Hinnant, public domain).
// 1970-01-01 を 0 とする proleptic Gregorian の通日。
// ★std::mktime を使わない理由: TZ 環境変数に依存し、ESP32 で挙動が変わる。
[[nodiscard]] constexpr std::int64_t daysFromCivil(std::int64_t year, int month, int day) noexcept {
  year -= month <= 2 ? 1 : 0;
  const std::int64_t era = (year >= 0 ? year : year - 399) / 400;
  const std::int64_t yoe = year - era * 400;                                     // [0, 399]
  const std::int64_t doy =
      (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1;                  // [0, 365]
  const std::int64_t doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;                // [0, 146096]
  return era * 146097 + doe - 719468;
}

// civil_from_days (Howard Hinnant, public domain)。daysFromCivil の逆。
struct CivilDate {
  std::int64_t year{0};
  int month{0};
  int day{0};
};

[[nodiscard]] constexpr CivilDate civilFromDays(std::int64_t days) noexcept {
  days += 719468;
  const std::int64_t era = (days >= 0 ? days : days - 146096) / 146097;
  const std::int64_t doe = days - era * 146097;                                  // [0, 146096]
  const std::int64_t yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;  // [0, 399]
  const std::int64_t y = yoe + era * 400;
  const std::int64_t doy = doe - (365 * yoe + yoe / 4 - yoe / 100);               // [0, 365]
  const std::int64_t mp = (5 * doy + 2) / 153;                                    // [0, 11]
  const std::int64_t d = doy - (153 * mp + 2) / 5 + 1;                            // [1, 31]
  const std::int64_t m = mp + (mp < 10 ? 3 : -9);                                 // [1, 12]
  CivilDate out{};
  out.year = y + (m <= 2 ? 1 : 0);
  out.month = static_cast<int>(m);
  out.day = static_cast<int>(d);
  return out;
}

}  // namespace

std::optional<CivilTime> parseIso8601Fixed(std::string_view text) noexcept {
  // 受け付ける長さは 19 / 20 / 25 のみ（ヘッダのコメント参照）。
  // ★先に長さで振り分けることで、以降の添字アクセスが安全になる。
  if (text.size() != 19 && text.size() != 20 && text.size() != 25) {
    return std::nullopt;
  }

  auto civil = parseDateAndHourMinute(text, 'T');
  if (!civil) {
    return std::nullopt;
  }

  if (text[16] != ':') {
    return std::nullopt;
  }
  const auto second = fixedDigits(text, 17, 2);
  if (!second) {
    return std::nullopt;
  }
  // うるう秒 60 は受け付ける（気象庁由来の値が持ちうる）。61 以上は異常。
  if (*second > 60) {
    return std::nullopt;
  }
  civil->second = *second;

  if (text.size() == 19) {
    civil->utcOffsetMinutes = 0;  // オフセット無し。UTC とみなす（ヘッダの注記）。
    return civil;
  }

  if (text.size() == 20) {
    if (text[19] != 'Z' && text[19] != 'z') {
      return std::nullopt;
    }
    civil->utcOffsetMinutes = 0;
    return civil;
  }

  // 25 文字: "±HH:MM"
  const char sign = text[19];
  if (sign != '+' && sign != '-') {
    return std::nullopt;
  }
  if (text[22] != ':') {
    return std::nullopt;
  }
  const auto offsetHour = fixedDigits(text, 20, 2);
  const auto offsetMinute = fixedDigits(text, 23, 2);
  if (!offsetHour || !offsetMinute) {
    return std::nullopt;
  }
  // UTC±14:00 が現実の上限。それを超える値は壊れた応答とみなす。
  if (*offsetHour > 14 || *offsetMinute > 59) {
    return std::nullopt;
  }
  const int magnitude = *offsetHour * 60 + *offsetMinute;
  civil->utcOffsetMinutes = sign == '-' ? -magnitude : magnitude;
  return civil;
}

std::optional<CivilTime> parsePhotoTime(std::string_view text) noexcept {
  // ★厳密に 16 文字。短い入力を弾かないと添字アクセスが範囲外になり、
  //   長い入力を通すと壊れた応答を正常として扱ってしまう。
  if (text.size() != 16) {
    return std::nullopt;
  }
  auto civil = parseDateAndHourMinute(text, ' ');
  if (!civil) {
    return std::nullopt;
  }
  // 秒もタイムゾーンも無い。これは撮影地のローカル時刻であって絶対時刻ではない。
  civil->second = 0;
  civil->utcOffsetMinutes = 0;
  return civil;
}

WallTime toWallTime(const CivilTime& civil) noexcept {
  const std::int64_t days = daysFromCivil(civil.year, civil.month, civil.day);
  const std::int64_t seconds = days * 86400 + static_cast<std::int64_t>(civil.hour) * 3600 +
                               static_cast<std::int64_t>(civil.minute) * 60 +
                               static_cast<std::int64_t>(civil.second) -
                               static_cast<std::int64_t>(civil.utcOffsetMinutes) * 60;
  return WallTime{seconds};
}

CivilTime toCivilTime(WallTime wall, int utcOffsetMinutes) noexcept {
  const std::int64_t local = wall.epochSeconds + static_cast<std::int64_t>(utcOffsetMinutes) * 60;
  // 負の epoch（1970 年より前）でも floor 除算になるようにする。
  // C++ の / は 0 方向へ丸めるので、そのままだと 1969 年が 1 日ずれる。
  std::int64_t days = local / 86400;
  std::int64_t rem = local % 86400;
  if (rem < 0) {
    rem += 86400;
    --days;
  }

  const CivilDate date = civilFromDays(days);
  CivilTime out{};
  out.year = static_cast<int>(date.year);
  out.month = date.month;
  out.day = date.day;
  out.hour = static_cast<int>(rem / 3600);
  out.minute = static_cast<int>((rem % 3600) / 60);
  out.second = static_cast<int>(rem % 60);
  out.utcOffsetMinutes = utcOffsetMinutes;
  return out;
}

std::optional<int> dayOfYearNoLeap(int month, int day) noexcept {
  if (month < 1 || month > 12 || day < 1 || day > 31) {
    return std::nullopt;
  }
  // 平年の累積日数。1/1 = 0。
  // ★2 月を 28 日で持つので 2/29 は 3/1 と同じ 59 になる。
  //   出典 scheduler.js の Date.UTC(2001, m-1, d) と同じ繰り上がり挙動。
  constexpr int kCumulative[12] = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334};
  return kCumulative[month - 1] + (day - 1);
}

}  // namespace ambient::core
