// environment_model.cpp — 室内環境と気象の解釈（Wave 3B）
//
// ★しきい値・STALE・カット周期の **根拠はヘッダ冒頭が正典**。
//   ここには実装だけを書く。値を変えるときはヘッダのコメントごと直すこと。
//
// ★ロケール非依存: snprintf("%.1f") も iostream も <format> も使わない。
//   %f は LC_NUMERIC の小数点記号を使うため、環境によって "22,2" になる。
//   （state_machine.hpp の traceLine / alert_engine.cpp の toDecimal と同じ規律）

#include "ambient/core/env/environment_model.hpp"

#include <algorithm>
#include <array>
#include <limits>

#include "ambient/core/model/parse.hpp"

namespace ambient::core {
namespace {

// 桁あふれの門。センサー値は高々 6 桁なので、これを超える入力は壊れている。
constexpr std::int64_t kParseLimit = 1'000'000'000'000LL;

[[nodiscard]] bool isDigit(char c) noexcept { return c >= '0' && c <= '9'; }

// scale（1 / 10 / 100 …）の 10 進桁数。scale=1 -> 0、10 -> 1、100 -> 2。
[[nodiscard]] int fractionDigitsFor(int scale) noexcept {
  int digits = 0;
  int rest = scale;
  while (rest >= 10) {
    rest /= 10;
    ++digits;
  }
  return digits;
}

// 符号なし 10 進文字列化。★<format> も to_string も使わない
//   （to_string は locale 非依存だが、桁詰めを自前でやりたいので統一する）。
[[nodiscard]] std::string decimalDigits(std::uint64_t value) {
  if (value == 0U) {
    return "0";
  }
  std::string out;
  while (value != 0U) {
    out.push_back(static_cast<char>('0' + static_cast<char>(value % 10U)));
    value /= 10U;
  }
  std::reverse(out.begin(), out.end());
  return out;
}

// 2 桁ゼロ詰め（時刻表示用）。
[[nodiscard]] std::string twoDigits(int value) {
  const int clamped = (value < 0 || value > 99) ? 0 : value;
  std::string out;
  out.push_back(static_cast<char>('0' + static_cast<char>(clamped / 10)));
  out.push_back(static_cast<char>('0' + static_cast<char>(clamped % 10)));
  return out;
}

// 先頭の 0 を落とす（"07" -> "7"）。全部 0 なら "0"。
[[nodiscard]] std::string stripLeadingZeros(std::string_view text) {
  std::size_t i = 0;
  while (i + 1 < text.size() && text[i] == '0') {
    ++i;
  }
  return std::string{text.substr(i)};
}

// EnvMetricKind の表示順（正準順）。
// ★HA が返す順ではなく、企画書 §5.3 の「初期表示候補」の順に並べ替える。
//   ENV 画面のタイルは先頭 3 件しか出ないので、ここの順序が
//   「何が大きく出るか」を決める（= 判断であり core の責務）。
[[nodiscard]] int displayRank(EnvMetricKind kind) noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature:
      return 0;
    case EnvMetricKind::Humidity:
      return 1;
    case EnvMetricKind::CarbonDioxide:
      return 2;
    case EnvMetricKind::VolatileOrganicCompounds:
      return 3;
    case EnvMetricKind::Pressure:
      return 4;
    case EnvMetricKind::Battery:
      return 5;
    case EnvMetricKind::Other:
      return 6;
  }
  return 6;
}

// 種類ごとの表示桁数（小数点以下）。
[[nodiscard]] int decimalsFor(EnvMetricKind kind) noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature:
    case EnvMetricKind::Humidity:
      return 1;
    case EnvMetricKind::CarbonDioxide:
    case EnvMetricKind::VolatileOrganicCompounds:
    case EnvMetricKind::Pressure:
    case EnvMetricKind::Battery:
    case EnvMetricKind::Other:
      return 0;
  }
  return 0;
}

// 種類ごとの正準ラベル。Other は呼ばない（HA の name を使う）。
[[nodiscard]] const char* canonicalLabel(EnvMetricKind kind) noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature:
      return "室温";
    case EnvMetricKind::Humidity:
      return "湿度";
    case EnvMetricKind::CarbonDioxide:
      return "CO2";
    case EnvMetricKind::VolatileOrganicCompounds:
      return "VOC";
    case EnvMetricKind::Pressure:
      return "気圧";
    case EnvMetricKind::Battery:
      return "電池";
    case EnvMetricKind::Other:
      return "";
  }
  return "";
}

// 種類ごとの正準単位。空文字列なら HA が返した unit をそのまま使う。
// ★温度だけ "°C" を "℃" にする（日本語表示の慣習。値は変えない）。
[[nodiscard]] const char* canonicalUnit(EnvMetricKind kind) noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature:
      return "℃";
    case EnvMetricKind::Humidity:
      return "%";
    case EnvMetricKind::CarbonDioxide:
      return "ppm";
    case EnvMetricKind::Battery:
      return "%";
    case EnvMetricKind::VolatileOrganicCompounds:
    case EnvMetricKind::Pressure:
    case EnvMetricKind::Other:
      return "";
  }
  return "";
}

}  // namespace

// ----------------------------------------------------------------------------
// 小さな変換
// ----------------------------------------------------------------------------

const char* toString(EnvLevel level) noexcept {
  switch (level) {
    case EnvLevel::Normal:
      return "Normal";
    case EnvLevel::Caution:
      return "Caution";
    case EnvLevel::Warning:
      return "Warning";
  }
  return "?";
}

const char* toString(EnvMetricKind kind) noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature:
      return "Temperature";
    case EnvMetricKind::Humidity:
      return "Humidity";
    case EnvMetricKind::CarbonDioxide:
      return "CarbonDioxide";
    case EnvMetricKind::VolatileOrganicCompounds:
      return "VolatileOrganicCompounds";
    case EnvMetricKind::Pressure:
      return "Pressure";
    case EnvMetricKind::Battery:
      return "Battery";
    case EnvMetricKind::Other:
      return "Other";
  }
  return "?";
}

EnvMetricKind classifyDeviceClass(std::string_view deviceClass) noexcept {
  if (deviceClass == "temperature") {
    return EnvMetricKind::Temperature;
  }
  if (deviceClass == "humidity") {
    return EnvMetricKind::Humidity;
  }
  if (deviceClass == "carbon_dioxide") {
    return EnvMetricKind::CarbonDioxide;
  }
  if (deviceClass == "volatile_organic_compounds" ||
      deviceClass == "volatile_organic_compounds_parts") {
    return EnvMetricKind::VolatileOrganicCompounds;
  }
  if (deviceClass == "pressure" || deviceClass == "atmospheric_pressure") {
    return EnvMetricKind::Pressure;
  }
  if (deviceClass == "battery") {
    return EnvMetricKind::Battery;
  }
  // ★未知の class は捨てない。文字列として表示する（企画書 §5.3）。
  return EnvMetricKind::Other;
}

std::optional<std::int64_t> parseDecimalScaled(std::string_view text, int scale) noexcept {
  if (scale < 1) {
    return std::nullopt;
  }
  std::size_t i = 0;
  while (i < text.size() && (text[i] == ' ' || text[i] == '\t')) {
    ++i;
  }
  bool negative = false;
  if (i < text.size() && (text[i] == '+' || text[i] == '-')) {
    negative = text[i] == '-';
    ++i;
  }

  bool sawDigit = false;
  std::int64_t acc = 0;
  while (i < text.size() && isDigit(text[i])) {
    sawDigit = true;
    if (acc > kParseLimit) {
      return std::nullopt;  // 静かに飽和させない
    }
    acc = acc * 10 + static_cast<std::int64_t>(text[i] - '0');
    ++i;
  }

  // 小数部。scale の桁数ぶん取り込み、その次の 1 桁で四捨五入する。
  const int wanted = fractionDigitsFor(scale);
  std::size_t frac = (i < text.size() && text[i] == '.') ? i + 1 : text.size();
  const bool hasPoint = frac != text.size();
  if (hasPoint) {
    i = frac;
  }
  for (int d = 0; d < wanted; ++d) {
    char digit = '0';
    if (hasPoint && frac < text.size() && isDigit(text[frac])) {
      digit = text[frac];
      ++frac;
      sawDigit = true;
    }
    if (acc > kParseLimit) {
      return std::nullopt;
    }
    acc = acc * 10 + static_cast<std::int64_t>(digit - '0');
  }
  char roundDigit = '0';
  if (hasPoint && frac < text.size() && isDigit(text[frac])) {
    roundDigit = text[frac];
    ++frac;
    sawDigit = true;
  }
  if (roundDigit >= '5') {
    ++acc;
  }
  // 残りの小数桁は読み飛ばす（既に丸めた）。
  if (hasPoint) {
    while (frac < text.size() && isDigit(text[frac])) {
      ++frac;
    }
    i = frac;
  }

  if (!sawDigit) {
    return std::nullopt;
  }
  // 末尾の空白だけ許す。ゴミが付いた入力は受け付けない
  // （認証の無い LAN サーバ由来なので、静かに切り捨てない）。
  while (i < text.size() && (text[i] == ' ' || text[i] == '\t')) {
    ++i;
  }
  if (i != text.size()) {
    return std::nullopt;
  }
  return negative ? -acc : acc;
}

std::string formatScaled(std::int64_t scaled, int decimals) {
  const bool negative = scaled < 0;
  auto magnitude = static_cast<std::uint64_t>(scaled);
  if (negative) {
    magnitude = ~magnitude + 1U;  // INT64_MIN でも UB にならない絶対値
  }
  std::string digits = decimalDigits(magnitude);
  if (decimals > 0) {
    const auto want = static_cast<std::size_t>(decimals) + 1U;
    while (digits.size() < want) {
      digits.insert(digits.begin(), '0');
    }
    digits.insert(digits.size() - static_cast<std::size_t>(decimals), 1, '.');
  }
  if (negative) {
    digits.insert(digits.begin(), '-');
  }
  return digits;
}

std::string shortDate(std::string_view isoDate) {
  if (isoDate.size() < 10 || isoDate[4] != '-' || isoDate[7] != '-') {
    return std::string{isoDate};
  }
  const std::string month = stripLeadingZeros(isoDate.substr(5, 2));
  const std::string day = stripLeadingZeros(isoDate.substr(8, 2));
  return month + "/" + day;
}

// ----------------------------------------------------------------------------
// EnvironmentModel
// ----------------------------------------------------------------------------

void EnvironmentModel::setCutSchedule(const EnvCutSchedule& schedule) noexcept {
  cut_ = schedule;
  if (cut_.interval.value <= 0) {
    cut_.interval = kDefaultEnvCutSchedule.interval;
  }
  if (cut_.duration.value <= 0) {
    cut_.duration = kDefaultEnvCutSchedule.duration;
  }
  // ★duration >= interval だと claim が途切れず、写真が二度と出なくなる。
  //   設定ミスで「写真の出ない端末」になるのは事故なので、ここで潰す。
  if (cut_.duration.value >= cut_.interval.value) {
    cut_.duration = Millis{cut_.interval.value / 2};
    if (cut_.duration.value <= 0) {
      cut_.duration = Millis{1};
    }
  }
}

void EnvironmentModel::anchorIfNeeded(SteadyTime now) noexcept {
  if (anchored_) {
    return;
  }
  const bool hasRoom = !roomNotConfigured_ && !roomItems_.empty();
  const bool hasWeather = weather_.has_value() || !weekly_.empty();
  if (!hasRoom && !hasWeather) {
    return;
  }
  // ★最初のカットは「最初に出せる内容が揃った瞬間」から始まる。
  //   10 分待たないと環境が一度も見られない、という体験にしないため。
  anchor_ = now;
  anchored_ = true;
}

void EnvironmentModel::ingestRoomEnv(const RoomEnvSnapshot& snapshot, SteadyTime now,
                                     WallTime wallNow) {
  if (snapshot.availability == RoomEnvAvailability::NotConfigured) {
    // ★ok:false = Home Assistant 未設定。**障害ではない**。
    //   古い値を STALE として出し続けるのも違うので、カットから外す。
    roomNotConfigured_ = true;
    roomItems_.clear();
    return;
  }
  roomNotConfigured_ = false;
  roomItems_ = snapshot.items;
  if (!roomItems_.empty()) {
    roomEverReceived_ = true;
    roomAt_ = now;
    roomWall_ = wallNow;
  }
  anchorIfNeeded(now);
}

void EnvironmentModel::ingestEnv(const EnvSnapshot& snapshot, SteadyTime now,
                                 WallTime wallNow) {
  // ★quakes はここで見ない。地震は AlertEngine（エッジ→レベル変換器）の担当で、
  //   2 か所で解釈すると「震度 3 以上」の判定が二重管理になる。
  const bool gotWeather = snapshot.weather.has_value() || !snapshot.weekly.empty();
  if (snapshot.weather.has_value()) {
    weather_ = snapshot.weather;
  }
  if (!snapshot.weekly.empty()) {
    weekly_ = snapshot.weekly;
  }
  if (gotWeather) {
    // ★「取得に成功した時刻」ではなく「値を受け取った時刻」を基準にする。
    //   気象庁の解析失敗で weather:null / weekly:[] が返り続けるとき、
    //   200 が返っているからといって値が新しいわけではない。
    weatherEverReceived_ = true;
    weatherAt_ = now;
    weatherWall_ = wallNow;
  }
  anchorIfNeeded(now);
}

void EnvironmentModel::ingestPresence(const PresenceSnapshot& snapshot, SteadyTime now) {
  // ★night は true / false / null の 3 値。value_or(false) で潰さないこと。
  night_ = snapshot.night;
  (void)now;
}

void EnvironmentModel::onTimeJump(const TimeJump& jump) noexcept {
  // ★clock.hpp のポリシーどおり skew > 0 のときだけ基準点を巻き戻す。
  //   これが無いと、8 時間スリープして復帰した直後に「8 時間前の値」が
  //   STALE にならない（単調時刻が進んでいないため）。
  roomAt_ = rebaseForJump(roomAt_, jump);
  weatherAt_ = rebaseForJump(weatherAt_, jump);
  anchor_ = rebaseForJump(anchor_, jump);
}

bool EnvironmentModel::roomStale(SteadyTime now) const noexcept {
  if (roomNotConfigured_ || !roomEverReceived_) {
    return false;  // ★未設定は「古い」ではない
  }
  return (now - roomAt_) >= staleness_.room;
}

bool EnvironmentModel::weatherStale(SteadyTime now) const noexcept {
  if (!weatherEverReceived_) {
    return false;
  }
  return (now - weatherAt_) >= staleness_.weather;
}

std::optional<EnvCutWindow> EnvironmentModel::cutWindowAt(SteadyTime now) const noexcept {
  if (!anchored_ || now < anchor_) {
    return std::nullopt;
  }
  const std::int64_t elapsed = (now - anchor_).value;
  const std::int64_t interval = cut_.interval.value;
  if (interval <= 0) {
    return std::nullopt;
  }
  const std::int64_t cycle = elapsed / interval;
  const std::int64_t phase = elapsed - cycle * interval;
  if (phase >= cut_.duration.value) {
    return std::nullopt;
  }
  EnvCutWindow window;
  // payloadId は 0 が「区別なし」なので必ず 1 以上にする。
  const auto raw = static_cast<std::uint64_t>(cycle + 1) & 0xFFFF'FFFFULL;
  window.index = (raw == 0U) ? 1U : static_cast<std::uint32_t>(raw);
  window.begin = anchor_ + Millis{cycle * interval};
  window.end = window.begin + cut_.duration;
  return window;
}

void EnvironmentModel::collect(SteadyTime now, std::vector<Claim>& out) const {
  const bool hasRoom = !roomNotConfigured_ && !roomItems_.empty();
  const bool hasWeather = weather_.has_value() || !weekly_.empty();
  if (!hasRoom && !hasWeather) {
    return;  // ★出すものが無い（HA 未設定 かつ 天気も無い）= 機能なし
  }
  const std::optional<EnvCutWindow> window = cutWindowAt(now);
  if (!window.has_value()) {
    return;
  }
  Claim claim;
  claim.priority = ClaimPriority::Env;
  claim.kind = DisplayStateKind::Env;
  claim.sourceId = kSourceId;
  // ★カットごとに payloadId が変わる = StateMachine が毎回「別の内容」として
  //   遷移を報告する。同じ画面を出し直したことがログに残る。
  claim.payloadId = window->index;
  claim.expiresAt = window->end;
  out.push_back(claim);
}

EnvLevel EnvironmentModel::levelOf(EnvMetricKind kind, std::string_view value) const noexcept {
  switch (kind) {
    case EnvMetricKind::Temperature: {
      const std::optional<std::int64_t> deci = parseDecimalScaled(value, 10);
      if (!deci.has_value()) {
        return EnvLevel::Normal;
      }
      if (*deci >= thresholds_.tempWarningHighDeci ||
          *deci < thresholds_.tempWarningLowDeci) {
        return EnvLevel::Warning;
      }
      if (*deci > thresholds_.tempCautionHighDeci ||
          *deci < thresholds_.tempCautionLowDeci) {
        return EnvLevel::Caution;
      }
      return EnvLevel::Normal;
    }
    case EnvMetricKind::Humidity: {
      const std::optional<std::int64_t> deci = parseDecimalScaled(value, 10);
      if (!deci.has_value()) {
        return EnvLevel::Normal;
      }
      if (*deci > thresholds_.humidityWarningHighPercent * 10 ||
          *deci < thresholds_.humidityWarningLowPercent * 10) {
        return EnvLevel::Warning;
      }
      if (*deci > thresholds_.humidityCautionHighPercent * 10 ||
          *deci < thresholds_.humidityCautionLowPercent * 10) {
        return EnvLevel::Caution;
      }
      return EnvLevel::Normal;
    }
    case EnvMetricKind::CarbonDioxide: {
      const std::optional<std::int64_t> ppm = parseDecimalScaled(value, 1);
      if (!ppm.has_value()) {
        return EnvLevel::Normal;
      }
      // ★企画書 §2.1: 1000ppm 以上を注意 / 1500ppm 以上を警告。
      if (*ppm >= thresholds_.co2WarningPpm) {
        return EnvLevel::Warning;
      }
      if (*ppm >= thresholds_.co2CautionPpm) {
        return EnvLevel::Caution;
      }
      return EnvLevel::Normal;
    }
    case EnvMetricKind::VolatileOrganicCompounds: {
      const std::optional<std::int64_t> amount = parseDecimalScaled(value, 1);
      if (!amount.has_value()) {
        return EnvLevel::Normal;
      }
      if (*amount >= thresholds_.vocWarning) {
        return EnvLevel::Warning;
      }
      if (*amount >= thresholds_.vocCaution) {
        return EnvLevel::Caution;
      }
      return EnvLevel::Normal;
    }
    case EnvMetricKind::Pressure:
    case EnvMetricKind::Battery:
    case EnvMetricKind::Other:
      // 健康影響のしきい値が無いものに無根拠な色を付けない。
      return EnvLevel::Normal;
  }
  return EnvLevel::Normal;
}

EnvView EnvironmentModel::view(SteadyTime now) const {
  EnvView out;
  out.heading = "室内環境と天気";

  const bool stale = roomStale(now);
  out.roomStale = stale;
  out.roomAvailable = !roomNotConfigured_ && !roomItems_.empty();

  if (out.roomAvailable) {
    out.metrics.reserve(roomItems_.size());
    for (const RoomEnvItem& item : roomItems_) {
      EnvMetricView metric;
      metric.kind = classifyDeviceClass(item.entityClass);
      metric.level = levelOf(metric.kind, item.value);
      metric.stale = stale;

      const char* label = canonicalLabel(metric.kind);
      if (label[0] != '\0') {
        metric.label = label;
      } else {
        metric.label = item.name.empty() ? item.entity : item.name;
      }

      const char* unit = canonicalUnit(metric.kind);
      metric.unit = (unit[0] != '\0') ? std::string{unit} : item.unit;

      // ★value は常に文字列で来る。数値として読めたら整形し、
      //   読めなければ **生の文字列をそのまま出す**（異常を隠さない）。
      const int decimals = decimalsFor(metric.kind);
      int scale = 1;
      for (int d = 0; d < decimals; ++d) {
        scale *= 10;
      }
      if (metric.kind == EnvMetricKind::Other) {
        metric.value = item.value;
      } else if (const std::optional<std::int64_t> scaled =
                     parseDecimalScaled(item.value, scale)) {
        metric.value = formatScaled(*scaled, decimals);
      } else {
        metric.value = item.value;
      }
      out.metrics.push_back(std::move(metric));
    }

    // 正準順へ並べ替える（同じ種類の中は入力順のまま = 安定ソート）。
    std::stable_sort(out.metrics.begin(), out.metrics.end(),
                     [](const EnvMetricView& a, const EnvMetricView& b) {
                       return displayRank(a.kind) < displayRank(b.kind);
                     });
  }

  if (weather_.has_value()) {
    out.hasWeather = true;
    out.weatherArea = weather_->area;
    out.weatherText = weather_->text;
    if (weather_->pop.has_value()) {
      out.weatherPop = "降水確率 " + formatScaled(*weather_->pop, 0) + "%";
    }
    // ★temps に min/max の意味づけはしない（出典 events.js:204 も
    //   temps.join('/') としか書いていない）。順序をそのまま出す。
    if (!weather_->temps.empty()) {
      std::string text = "気温 ";
      for (std::size_t i = 0; i < weather_->temps.size(); ++i) {
        if (i != 0) {
          text += " / ";
        }
        text += formatScaled(weather_->temps[i], 0);
      }
      text += " ℃";
      out.weatherTemps = text;
    }
  }
  if (!weekly_.empty()) {
    out.hasWeather = true;
    out.weekly.reserve(weekly_.size());
    for (const WeeklyDay& day : weekly_) {
      EnvWeeklyView view;
      view.date = shortDate(day.date);
      // ★pop / temp_min / temp_max は null をとる（実測: 翌日分が全部 null）。
      view.tempMax = day.tempMax.has_value() ? formatScaled(*day.tempMax, 0) : "--";
      view.tempMin = day.tempMin.has_value() ? formatScaled(*day.tempMin, 0) : "--";
      out.weekly.push_back(std::move(view));
    }
  }
  out.weatherStale = out.hasWeather && weatherStale(now);

  // ヒント行。★「いつ取れた値か」を出す。STALE の判断材料になる。
  std::string hint;
  if (roomNotConfigured_) {
    hint = "室内センサーなし（Home Assistant 未設定）";
  } else if (roomEverReceived_) {
    const CivilTime civil = toCivilTime(roomWall_, utcOffsetMinutes_);
    hint = "室内 " + twoDigits(civil.hour) + ":" + twoDigits(civil.minute) + " 取得";
    if (out.roomStale) {
      hint += "（STALE）";
    }
  }
  if (weatherEverReceived_) {
    const CivilTime civil = toCivilTime(weatherWall_, utcOffsetMinutes_);
    if (!hint.empty()) {
      hint += " / ";
    }
    hint += "天気 " + twoDigits(civil.hour) + ":" + twoDigits(civil.minute) + " 取得";
    if (out.weatherStale) {
      hint += "（STALE）";
    }
  }
  if (night_.has_value()) {
    if (!hint.empty()) {
      hint += " / ";
    }
    hint += *night_ ? "夜間" : "日中";
  }
  out.hint = hint;
  return out;
}

}  // namespace ambient::core
