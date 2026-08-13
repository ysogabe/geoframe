// test_environment_model.cpp — 室内環境と気象の解釈（Wave 3B）
//
// ★このファイルが固定しているもの:
//   - 企画書由来の CO2 1000/1500 を **リテラルで** 検査する（速度制限）
//   - STALE のしきい値が /api/env のサーバ側キャッシュ 300 秒より長いこと
//     （短いと取得成功直後から常時 STALE になる。実装が壊れても画面は
//       「それらしく」見えるので、テストでしか捕まえられない）
//   - ok:false（HA 未設定）が STALE にも Degraded にもならないこと
//   - 環境カットが周期的にしか claim を出さないこと
//     （常時 claim すると Env > Photo なので写真が二度と出ない）

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/env/environment_model.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"
#include "ambient/core/state/claim.hpp"

using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::DisplayStateKind;
using ambient::core::EnvCutSchedule;
using ambient::core::EnvironmentModel;
using ambient::core::EnvLevel;
using ambient::core::EnvMetricKind;
using ambient::core::EnvSnapshot;
using ambient::core::EnvView;
using ambient::core::Millis;
using ambient::core::RoomEnvAvailability;
using ambient::core::RoomEnvItem;
using ambient::core::RoomEnvSnapshot;
using ambient::core::SteadyTime;
using ambient::core::WallTime;
using ambient::core::WeatherSummary;
using ambient::core::WeeklyDay;

namespace {

RoomEnvItem makeItem(std::string entity, std::string value, std::string unit,
                     std::string entityClass, std::string name) {
  RoomEnvItem item;
  item.entity = std::move(entity);
  item.value = std::move(value);
  item.unit = std::move(unit);
  item.entityClass = std::move(entityClass);
  item.name = std::move(name);
  return item;
}

// assets/fixtures/api/roomenv.json をそのまま写した合成値。
// ★HA が返す順（温度・湿度・CO2・VOC・気圧）で入れる。並べ替えるのは model の仕事。
RoomEnvSnapshot fixtureRoomEnv() {
  RoomEnvSnapshot snapshot;
  snapshot.availability = RoomEnvAvailability::Available;
  snapshot.items = {
      makeItem("sensor.fixture_temperature_01", "22.50", "°C", "temperature",
               "サンプルセンサー 温度"),
      makeItem("sensor.fixture_humidity_02", "48.00", "%", "humidity",
               "サンプルセンサー 湿度"),
      makeItem("sensor.fixture_carbon_dioxide_03", "650", "ppm",
               "carbon_dioxide", "サンプルセンサー 二酸化炭素"),
      makeItem("sensor.fixture_volatile_organic_compounds_04", "120",
               "μg/m³", "volatile_organic_compounds", "サンプルセンサー 揮発性有機化合物"),
      makeItem("sensor.fixture_pressure_05", "1013.20", "mbar", "pressure",
               "サンプルセンサー 気圧"),
  };
  return snapshot;
}

RoomEnvSnapshot singleMetric(std::string entityClass, std::string value) {
  RoomEnvSnapshot snapshot;
  snapshot.availability = RoomEnvAvailability::Available;
  snapshot.items = {makeItem("sensor.x", std::move(value), "", std::move(entityClass), "x")};
  return snapshot;
}

EnvLevel levelOfSingle(const std::string& entityClass, const std::string& value) {
  EnvironmentModel model;
  model.ingestRoomEnv(singleMetric(entityClass, value), SteadyTime{0}, WallTime{0});
  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.metrics.size() == 1);
  return view.metrics[0].level;
}

}  // namespace

// ---------------------------------------------------------------------------
// 値の変換（ロケール非依存であること）
// ---------------------------------------------------------------------------

TEST_CASE("parseDecimalScaled: 文字列の値を整数倍で読む（四捨五入）") {
  using ambient::core::parseDecimalScaled;
  CHECK(parseDecimalScaled("22.18", 10).value() == 222);   // 切り上げ側
  CHECK(parseDecimalScaled("23.67", 10).value() == 237);
  CHECK(parseDecimalScaled("52.69", 10).value() == 527);
  CHECK(parseDecimalScaled("22.14", 10).value() == 221);   // 切り捨て側
  CHECK(parseDecimalScaled("1167", 1).value() == 1167);
  CHECK(parseDecimalScaled("999.87", 1).value() == 1000);  // 桁上がり
  CHECK(parseDecimalScaled("-3.55", 10).value() == -36);
  CHECK(parseDecimalScaled(" 4 ", 1).value() == 4);
  CHECK(parseDecimalScaled(".5", 10).value() == 5);
}

TEST_CASE("parseDecimalScaled: 読めない入力は nullopt（静かに 0 にしない）") {
  using ambient::core::parseDecimalScaled;
  CHECK_FALSE(parseDecimalScaled("", 10).has_value());
  CHECK_FALSE(parseDecimalScaled("unknown", 10).has_value());
  CHECK_FALSE(parseDecimalScaled("unavailable", 1).has_value());  // HA が返しうる値
  CHECK_FALSE(parseDecimalScaled("22.18abc", 10).has_value());
  CHECK_FALSE(parseDecimalScaled("-", 10).has_value());
  CHECK_FALSE(parseDecimalScaled("99999999999999999999", 10).has_value());
}

TEST_CASE("formatScaled: 小数点は必ず '.'（ロケールに従わない）") {
  using ambient::core::formatScaled;
  CHECK(formatScaled(222, 1) == "22.2");
  CHECK(formatScaled(1167, 0) == "1167");
  CHECK(formatScaled(5, 1) == "0.5");
  CHECK(formatScaled(0, 1) == "0.0");
  CHECK(formatScaled(-36, 1) == "-3.6");
}

TEST_CASE("shortDate: 週間予報の日付を詰める") {
  using ambient::core::shortDate;
  CHECK(shortDate("2026-07-31") == "7/31");
  CHECK(shortDate("2026-08-01") == "8/1");
  CHECK(shortDate("") == "");  // 壊れた値はそのまま返す（隠さない）
  CHECK(shortDate("31/07") == "31/07");
}

// ---------------------------------------------------------------------------
// device_class → ラベル・単位・種類
// ---------------------------------------------------------------------------

TEST_CASE("classifyDeviceClass: 実測した class をすべて拾う") {
  using ambient::core::classifyDeviceClass;
  CHECK(classifyDeviceClass("temperature") == EnvMetricKind::Temperature);
  CHECK(classifyDeviceClass("humidity") == EnvMetricKind::Humidity);
  CHECK(classifyDeviceClass("carbon_dioxide") == EnvMetricKind::CarbonDioxide);
  CHECK(classifyDeviceClass("volatile_organic_compounds") ==
        EnvMetricKind::VolatileOrganicCompounds);
  CHECK(classifyDeviceClass("pressure") == EnvMetricKind::Pressure);
  // ★未知の class は捨てずに Other（企画書 §5.3）。
  CHECK(classifyDeviceClass("illuminance") == EnvMetricKind::Other);
  CHECK(classifyDeviceClass("") == EnvMetricKind::Other);
}

TEST_CASE("ラベルと単位は class から決まる（HA の name には依存しない）") {
  EnvironmentModel model;
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{0}, WallTime{0});
  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.metrics.size() == 5);

  CHECK(view.metrics[0].label == "室温");
  CHECK(view.metrics[0].unit == "℃");  // "°C" ではなく日本語表示の慣習へ
  CHECK(view.metrics[0].value == "22.5");
  CHECK(view.metrics[1].label == "湿度");
  CHECK(view.metrics[1].value == "48.0");
  CHECK(view.metrics[2].label == "CO2");
  CHECK(view.metrics[2].unit == "ppm");
  CHECK(view.metrics[2].value == "650");
  // 正準単位を持たない種類は HA の unit をそのまま通す。
  CHECK(view.metrics[3].unit == "μg/m³");
  CHECK(view.metrics[4].unit == "mbar");
  CHECK(view.metrics[4].value == "1013");  // 1013.20 -> 1013（0 桁表示）
}

TEST_CASE("未知の class は name をラベルにして値を素通しする") {
  EnvironmentModel model;
  RoomEnvSnapshot snapshot;
  snapshot.availability = RoomEnvAvailability::Available;
  snapshot.items = {makeItem("sensor.door", "on", "", "door", "玄関ドア")};
  model.ingestRoomEnv(snapshot, SteadyTime{0}, WallTime{0});

  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.metrics.size() == 1);
  CHECK(view.metrics[0].kind == EnvMetricKind::Other);
  CHECK(view.metrics[0].label == "玄関ドア");
  CHECK(view.metrics[0].value == "on");
  CHECK(view.metrics[0].level == EnvLevel::Normal);
}

TEST_CASE("数値として読めない値は生のまま出す（異常を隠さない）") {
  EnvironmentModel model;
  model.ingestRoomEnv(singleMetric("temperature", "unavailable"), SteadyTime{0},
                      WallTime{0});
  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.metrics.size() == 1);
  CHECK(view.metrics[0].value == "unavailable");
  CHECK(view.metrics[0].level == EnvLevel::Normal);
}

TEST_CASE("表示順は正準順（HA が返す順ではない）") {
  EnvironmentModel model;
  RoomEnvSnapshot snapshot;
  snapshot.availability = RoomEnvAvailability::Available;
  // わざと逆順で渡す。
  snapshot.items = {
      makeItem("sensor.p", "1000", "mbar", "pressure", "圧力"),
      makeItem("sensor.c", "500", "ppm", "carbon_dioxide", "CO2"),
      makeItem("sensor.h", "50", "%", "humidity", "湿度"),
      makeItem("sensor.t", "22", "°C", "temperature", "温度"),
  };
  model.ingestRoomEnv(snapshot, SteadyTime{0}, WallTime{0});

  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.metrics.size() == 4);
  // ENV 画面のタイルは先頭 3 件しか出ないので、この順序が「何が大きく出るか」。
  CHECK(view.metrics[0].kind == EnvMetricKind::Temperature);
  CHECK(view.metrics[1].kind == EnvMetricKind::Humidity);
  CHECK(view.metrics[2].kind == EnvMetricKind::CarbonDioxide);
  CHECK(view.metrics[3].kind == EnvMetricKind::Pressure);
}

// ---------------------------------------------------------------------------
// しきい値
// ---------------------------------------------------------------------------

TEST_CASE("★CO2 のしきい値は 1000 / 1500（企画書 §2.1 ENV）") {
  // ############################################################################
  // # ★リテラルで書いてある。企画書由来の値を「なんとなく」変えたときに
  // #   ここで手が止まるようにするための速度制限である
  // #   （test_alert_engine.cpp が kAlertHoldDuration=60000 でやっているのと同じ）。
  // #   変えるなら企画書 §2.1 と environment_model.hpp のコメントごと直すこと。
  // ############################################################################
  CHECK(ambient::core::kDefaultEnvThresholds.co2CautionPpm == 1000);
  CHECK(ambient::core::kDefaultEnvThresholds.co2WarningPpm == 1500);

  CHECK(levelOfSingle("carbon_dioxide", "999") == EnvLevel::Normal);
  CHECK(levelOfSingle("carbon_dioxide", "1000") == EnvLevel::Caution);  // 以上
  CHECK(levelOfSingle("carbon_dioxide", "1200") == EnvLevel::Caution);
  CHECK(levelOfSingle("carbon_dioxide", "1499") == EnvLevel::Caution);
  CHECK(levelOfSingle("carbon_dioxide", "1500") == EnvLevel::Warning);  // 以上
  CHECK(levelOfSingle("carbon_dioxide", "1800") == EnvLevel::Warning);
}

TEST_CASE("温度のしきい値（注意帯は事務所衛生基準規則 18〜28℃）") {
  CHECK(levelOfSingle("temperature", "22.18") == EnvLevel::Normal);
  CHECK(levelOfSingle("temperature", "18.0") == EnvLevel::Normal);
  CHECK(levelOfSingle("temperature", "28.0") == EnvLevel::Normal);
  CHECK(levelOfSingle("temperature", "17.9") == EnvLevel::Caution);
  CHECK(levelOfSingle("temperature", "28.1") == EnvLevel::Caution);
  // 警告帯は 3B の拡張（10℃未満 / 31℃以上）。
  CHECK(levelOfSingle("temperature", "30.9") == EnvLevel::Caution);
  CHECK(levelOfSingle("temperature", "31.0") == EnvLevel::Warning);
  CHECK(levelOfSingle("temperature", "9.9") == EnvLevel::Warning);
}

TEST_CASE("湿度のしきい値（注意帯は事務所衛生基準規則 40〜70%）") {
  CHECK(levelOfSingle("humidity", "52.69") == EnvLevel::Normal);
  CHECK(levelOfSingle("humidity", "40") == EnvLevel::Normal);
  CHECK(levelOfSingle("humidity", "70") == EnvLevel::Normal);
  CHECK(levelOfSingle("humidity", "39.9") == EnvLevel::Caution);
  CHECK(levelOfSingle("humidity", "70.1") == EnvLevel::Caution);
  CHECK(levelOfSingle("humidity", "29.9") == EnvLevel::Warning);
  CHECK(levelOfSingle("humidity", "80.1") == EnvLevel::Warning);
}

TEST_CASE("気圧はしきい値を持たない（無根拠な色を付けない）") {
  CHECK(levelOfSingle("pressure", "999.87") == EnvLevel::Normal);
  CHECK(levelOfSingle("pressure", "1050") == EnvLevel::Normal);
}

// ---------------------------------------------------------------------------
// ★ok:false（Home Assistant 未設定）
// ---------------------------------------------------------------------------

TEST_CASE("★ok:false は capability-gate。STALE にも Degraded にもしない") {
  EnvironmentModel model;
  RoomEnvSnapshot notConfigured;
  notConfigured.availability = RoomEnvAvailability::NotConfigured;

  model.ingestRoomEnv(notConfigured, SteadyTime{0}, WallTime{0});
  CHECK(model.roomEnvNotConfigured());

  // どれだけ時間が経っても STALE にならない（「古い値」が存在しないため）。
  CHECK_FALSE(model.roomStale(SteadyTime{0}));
  CHECK_FALSE(model.roomStale(SteadyTime{24 * 60 * 60 * 1000}));

  // 室内のカット自体が無い。天気も無ければ claim も出さない（機能なし）。
  const EnvView view = model.view(SteadyTime{0});
  CHECK(view.metrics.empty());
  CHECK_FALSE(view.roomAvailable);
  CHECK(view.empty());
  CHECK_FALSE(model.hasAnyData());

  std::vector<Claim> claims;
  model.collect(SteadyTime{0}, claims);
  CHECK(claims.empty());
}

TEST_CASE("HA 未設定でも天気があれば環境カットは出る") {
  EnvironmentModel model;
  RoomEnvSnapshot notConfigured;
  notConfigured.availability = RoomEnvAvailability::NotConfigured;
  model.ingestRoomEnv(notConfigured, SteadyTime{0}, WallTime{0});

  EnvSnapshot env;
  WeatherSummary weather;
  weather.area = "北西部";
  weather.text = "晴れ";
  weather.pop = 20;
  weather.temps = {27, 36};
  env.weather = weather;
  model.ingestEnv(env, SteadyTime{0}, WallTime{0});

  const EnvView view = model.view(SteadyTime{0});
  CHECK(view.hasWeather);
  CHECK(view.metrics.empty());
  CHECK(view.weatherPop == "降水確率 20%");
  CHECK(view.weatherTemps == "気温 27 / 36 ℃");
  CHECK_FALSE(view.empty());

  std::vector<Claim> claims;
  model.collect(SteadyTime{0}, claims);
  CHECK(claims.size() == 1);
}

// ---------------------------------------------------------------------------
// ★STALE
// ---------------------------------------------------------------------------

TEST_CASE("★STALE のしきい値は /api/env のサーバ側キャッシュより長いこと") {
  // ############################################################################
  // # /api/env はサーバが 300 秒キャッシュする（app/geoframe/jma.py:88、
  // # 企画書 §5.4）。取得に成功した瞬間の値でも最大 300 秒古い。
  // # したがってしきい値を 300 秒以下にすると **常時 STALE** になり、
  // # 画面は「それらしく」見えたまま警告が出っぱなしになる。
  // # 実装を壊しても目視では気づけないので、ここで固定する。
  // ############################################################################
  CHECK(ambient::core::kDefaultEnvStaleness.weather.value >
        ambient::core::kEnvServerCachePeriod.value);
  // ポーリング間隔（30 秒）を足した最悪値 330 秒よりさらに余裕があること。
  CHECK(ambient::core::kDefaultEnvStaleness.weather.value >= 330 * 1000);
  // 室内は HA 更新 55 秒 + ポーリング 30 秒 = 85 秒の 2 倍以上あること。
  CHECK(ambient::core::kDefaultEnvStaleness.room.value >= 85 * 1000 * 2);
}

TEST_CASE("STALE: 値は残したまま stale フラグが立つ") {
  EnvironmentModel model;
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{0}, WallTime{0});
  const Millis threshold = ambient::core::kDefaultEnvStaleness.room;

  CHECK_FALSE(model.roomStale(SteadyTime{threshold.value - 1}));
  CHECK(model.roomStale(SteadyTime{threshold.value}));

  const EnvView fresh = model.view(SteadyTime{0});
  REQUIRE(fresh.metrics.size() == 5);
  CHECK_FALSE(fresh.metrics[0].stale);

  const EnvView stale = model.view(SteadyTime{threshold.value});
  REQUIRE(stale.metrics.size() == 5);
  CHECK(stale.metrics[0].stale);
  // ★数値は消さない（企画書 §2.1「数値を残しつつ STALE を表示する」）。
  CHECK(stale.metrics[0].value == "22.5");

  // 取り直せば戻る（レベル条件なので特別な復帰処理は要らない）。
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{threshold.value}, WallTime{0});
  CHECK_FALSE(model.roomStale(SteadyTime{threshold.value}));
}

TEST_CASE("STALE: weather:null が返り続けても「新しい」ことにはしない") {
  EnvironmentModel model;
  EnvSnapshot withWeather;
  WeatherSummary weather;
  weather.area = "北西部";
  withWeather.weather = weather;
  model.ingestEnv(withWeather, SteadyTime{0}, WallTime{0});

  // 気象庁の解析失敗（weather:null / weekly:[]）。HTTP は 200 が返っている。
  const EnvSnapshot empty;
  const Millis threshold = ambient::core::kDefaultEnvStaleness.weather;
  model.ingestEnv(empty, SteadyTime{threshold.value}, WallTime{0});

  // ★「取得に成功した時刻」を基準にすると、ここで STALE が消えてしまう。
  //   値を受け取った時刻を基準にしているので STALE のままになる。
  CHECK(model.weatherStale(SteadyTime{threshold.value}));
  CHECK(model.view(SteadyTime{threshold.value}).weatherStale);
}

TEST_CASE("★スリープ復帰: 単調時刻が止まっていた間も「古くなった」と数える") {
  EnvironmentModel model;
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{1000}, WallTime{0});
  CHECK_FALSE(model.roomStale(SteadyTime{1000}));

  // ノートを閉じて 8 時間。単調時刻はほとんど進んでいない。
  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{5};
  jump.wallDelta = Millis{8 * 60 * 60 * 1000};
  jump.skew = Millis{8 * 60 * 60 * 1000 - 5};
  model.onTimeJump(jump);

  // ★補正が無いと「さっき取った値」に見えたままになる。
  CHECK(model.roomStale(SteadyTime{1005}));
}

// ---------------------------------------------------------------------------
// 週間予報
// ---------------------------------------------------------------------------

TEST_CASE("週間予報: pop / temp_min / temp_max の null を握り潰さない") {
  EnvironmentModel model;
  EnvSnapshot env;
  WeeklyDay today;  // 実測: 翌日分は 3 つとも null
  today.date = "2026-07-31";
  today.code = "101";
  WeeklyDay tomorrow;
  tomorrow.date = "2026-08-01";
  tomorrow.code = "200";
  tomorrow.pop = 20;
  tomorrow.tempMin = 24;
  tomorrow.tempMax = 32;
  env.weekly = {today, tomorrow};
  model.ingestEnv(env, SteadyTime{0}, WallTime{0});

  const EnvView view = model.view(SteadyTime{0});
  REQUIRE(view.weekly.size() == 2);
  CHECK(view.weekly[0].date == "7/31");
  CHECK(view.weekly[0].tempMax == "--");
  CHECK(view.weekly[0].tempMin == "--");
  CHECK(view.weekly[1].date == "8/1");
  CHECK(view.weekly[1].tempMax == "32");
  CHECK(view.weekly[1].tempMin == "24");
}

TEST_CASE("weekly が空でも落ちない（気象庁の解析失敗）") {
  EnvironmentModel model;
  const EnvSnapshot env;  // quakes-inject.json と同じ形（weather:null / weekly:[]）
  model.ingestEnv(env, SteadyTime{0}, WallTime{0});
  const EnvView view = model.view(SteadyTime{0});
  CHECK(view.weekly.empty());
  CHECK_FALSE(view.hasWeather);
  CHECK_FALSE(model.hasAnyData());
}

// ---------------------------------------------------------------------------
// 環境カット（claim）
// ---------------------------------------------------------------------------

TEST_CASE("★環境カットは周期的にしか claim を出さない（常時だと写真が出ない）") {
  EnvironmentModel model;
  EnvCutSchedule schedule;
  schedule.interval = Millis{60000};
  schedule.duration = Millis{10000};
  model.setCutSchedule(schedule);
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{5000}, WallTime{0});

  const auto claimCountAt = [&model](std::int64_t ms) {
    std::vector<Claim> out;
    model.collect(SteadyTime{ms}, out);
    return out.size();
  };

  // ★最初のカットは「最初に取得できた瞬間」から始まる。
  CHECK(claimCountAt(5000) == 1);
  CHECK(claimCountAt(14999) == 1);
  CHECK(claimCountAt(15000) == 0);  // duration 10 秒で終わる
  CHECK(claimCountAt(64999) == 0);
  CHECK(claimCountAt(65000) == 1);  // 次の周期
  CHECK(claimCountAt(74999) == 1);
  CHECK(claimCountAt(75000) == 0);

  // 取得前は claim を出さない。
  CHECK(claimCountAt(0) == 0);
}

TEST_CASE("環境カットの claim は Env 優先度で、カットごとに payloadId が変わる") {
  EnvironmentModel model;
  EnvCutSchedule schedule;
  schedule.interval = Millis{60000};
  schedule.duration = Millis{10000};
  model.setCutSchedule(schedule);
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{0}, WallTime{0});

  std::vector<Claim> first;
  model.collect(SteadyTime{100}, first);
  REQUIRE(first.size() == 1);
  CHECK(first[0].priority == ClaimPriority::Env);
  CHECK(first[0].kind == DisplayStateKind::Env);
  CHECK(first[0].sourceId == EnvironmentModel::kSourceId);
  CHECK(first[0].payloadId == 1);
  REQUIRE(first[0].expiresAt.has_value());
  CHECK(first[0].expiresAt->ms == 10000);
  CHECK(first[0].activeAt(SteadyTime{100}));

  std::vector<Claim> second;
  model.collect(SteadyTime{60100}, second);
  REQUIRE(second.size() == 1);
  CHECK(second[0].payloadId == 2);  // 別の内容として遷移させる
}

TEST_CASE("★duration >= interval の設定ミスは潰す（写真が二度と出なくなるため）") {
  EnvironmentModel model;
  EnvCutSchedule broken;
  broken.interval = Millis{60000};
  broken.duration = Millis{60000};  // 隙間が無い
  model.setCutSchedule(broken);
  CHECK(model.cutSchedule().duration.value < model.cutSchedule().interval.value);

  EnvCutSchedule zero;
  zero.interval = Millis{0};
  zero.duration = Millis{0};
  model.setCutSchedule(zero);
  CHECK(model.cutSchedule().interval.value > 0);
  CHECK(model.cutSchedule().duration.value > 0);
}

TEST_CASE("collect は const で、何度呼んでも同じ答えを返す（レベルトリガ）") {
  EnvironmentModel model;
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{0}, WallTime{0});
  const EnvironmentModel& frozen = model;

  std::vector<Claim> a;
  std::vector<Claim> b;
  frozen.collect(SteadyTime{1000}, a);
  frozen.collect(SteadyTime{1000}, b);
  REQUIRE(a.size() == 1);
  REQUIRE(b.size() == 1);
  CHECK(a[0].payloadId == b[0].payloadId);
  REQUIRE(a[0].expiresAt.has_value());
  REQUIRE(b[0].expiresAt.has_value());
  CHECK(a[0].expiresAt->ms == b[0].expiresAt->ms);
}

TEST_CASE("presence の night は 3 値のまま保つ") {
  EnvironmentModel model;
  CHECK_FALSE(model.night().has_value());

  const ambient::core::PresenceSnapshot unknown;  // null
  model.ingestPresence(unknown, SteadyTime{0});
  CHECK_FALSE(model.night().has_value());

  ambient::core::PresenceSnapshot day;
  day.night = false;
  model.ingestPresence(day, SteadyTime{0});
  REQUIRE(model.night().has_value());
  CHECK_FALSE(*model.night());

  ambient::core::PresenceSnapshot night;
  night.night = true;
  model.ingestPresence(night, SteadyTime{0});
  REQUIRE(model.night().has_value());
  CHECK(*model.night());
}

TEST_CASE("hint に取得時刻が入る（UTC オフセットは app が渡す）") {
  EnvironmentModel model;
  model.setUtcOffsetMinutes(540);  // JST
  // 2026-07-30T14:26:00+09:00 = 2026-07-30T05:26:00Z = epoch 1785389160
  model.ingestRoomEnv(fixtureRoomEnv(), SteadyTime{0}, WallTime{1785389160});
  const EnvView view = model.view(SteadyTime{0});
  CHECK(view.hint == "室内 14:26 取得");
  CHECK(view.heading == "室内環境と天気");
}
