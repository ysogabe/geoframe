// ambient/core/model/env.hpp — /api/env のデータ型（気象庁: 地震・天気・週間予報）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★実測が正典
// ============================================================================
//
// /api/env のトップレベルは `quakes` / `weather` / `weekly` の 3 キー。
// **`ok` フィールドは無い。** 「ok を見て成否を判定する」実装を書かないこと。
// 成否は HTTP ステータスで判定する（ambient/core/http.hpp の successBody() を通す）。

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace ambient::core {

// ----------------------------------------------------------------------------
// 地震
// ----------------------------------------------------------------------------
//
// 実測した item（8 キー）:
//   {"eid":"20260730135103","lat":39.3,"lon":142.1,"mag":"3.6","maxi":"2",
//    "place":"岩手県沖","time":"13:51","at":"2026-07-30T13:51:00+09:00"}
struct QuakeItem {
  // イベント ID。既知判定（seen セット）のキー。
  std::string eid;

  double lat{0.0};
  double lon{0.0};

  // ★mag は **文字列** で返る。数値ではない。
  //   表示用にそのまま使う。数値比較が要るなら parseIntPrefix / 自前の実数変換を通す。
  std::string mag;

  // ★maxi（最大震度）も **文字列**。しかも "5-" "5+" のような値がありうる
  //   （気象庁の震度階級 5 弱 / 5 強）。
  //   ★std::stoi は使えない。parseIntPrefix()（model/parse.hpp）で整数部を取ること。
  //   イベント化のしきい値は kQuakeMinIntensity = 3。
  std::string maxi;

  std::string place;

  // ★time は "HH:MM" の **5 文字**。日付が落ちている。
  //   24 時間窓なので「昨日 23:50」と「今日 23:50」が区別できない。
  //   ★日跨ぎの同定にこれを使ってはいけない。表示専用。
  std::string time;

  // ★同定にはこちらを使う。ISO 8601 固定幅 "YYYY-MM-DDTHH:MM:SS+09:00"。
  //   parseIso8601Fixed()（model/parse.hpp）で読む。
  std::string at;
};

// ----------------------------------------------------------------------------
// 天気（今日）
// ----------------------------------------------------------------------------
//
// 出典: app/static/js/events.js（w.text / w.pop / w.temps）と
//       app/static/js/panels.js（ENV.weather.area）。
struct WeatherSummary {
  std::string area;  // 予報区名
  std::string text;  // 気象庁の予報文そのまま
  std::optional<int> pop;  // 降水確率 %。欠けることがある
  std::vector<int> temps;  // 気温。表示は "/" 区切り
};

// ----------------------------------------------------------------------------
// 週間予報
// ----------------------------------------------------------------------------
//
// 出典: app/static/js/panels.js:234-260（d.date / d.code / d.pop /
//       d.temp_max / d.temp_min を参照している）。
//
// ★ここは **サーバ応答を直接実測していない**（旧実装の消費側コードからの復元）。
//   レーン 1D が fixture を採取したときに確定させること。
//   ずれていたらこの struct を直し、docs/CORE_CONTRACTS.md に記録する。
struct WeeklyDay {
  std::string date;  // "YYYY-MM-DD"
  // 気象庁の天気コード。先頭 1 桁が 1=晴 / 2=曇 / 3=雨 / 4=雪。
  // ★文字列で来る前提（panels.js が String(code).charAt(0) している）。
  std::string code;
  std::optional<int> pop;
  std::optional<int> tempMax;  // JSON キーは temp_max
  std::optional<int> tempMin;  // JSON キーは temp_min
};

// ----------------------------------------------------------------------------
// /api/env 全体
// ----------------------------------------------------------------------------

struct EnvSnapshot {
  std::vector<QuakeItem> quakes;
  std::optional<WeatherSummary> weather;  // 欠けることがある
  std::vector<WeeklyDay> weekly;

  // ★`ok` は存在しない。ここに bool ok を足さないこと。
};

}  // namespace ambient::core
