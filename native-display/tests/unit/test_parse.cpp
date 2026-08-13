// tests/unit/test_parse.cpp — 文字列 → 型
//
// parseIntPrefix / URL 組み立て / モデル型は先行実装。
// 日時パーサ（parseIso8601Fixed / parsePhotoTime / toWallTime / toCivilTime /
// dayOfYearNoLeap）はレーン 1B が実装し、このファイル後半でテストしている。
//
// ★異常系を重点的に見ていること。入力は **認証の無い LAN 上のサーバ**から来る。
//   ヘッダが「固定幅」と書いているせいで長さ検査を省きたくなるが、短い文字列で
//   範囲外アクセスを起こしてはいけない。

#include <doctest.h>

#include <string>
#include <string_view>

#include "ambient/core/constants.hpp"

#include "ambient/core/model/env.hpp"
#include "ambient/core/model/parse.hpp"
#include "ambient/core/model/photo.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"

using ambient::core::CivilTime;
using ambient::core::dayOfYearNoLeap;
using ambient::core::parseIntPrefix;
using ambient::core::parseIso8601Fixed;
using ambient::core::parsePhotoTime;
using ambient::core::toCivilTime;
using ambient::core::toWallTime;
using ambient::core::WallTime;

TEST_CASE("★震度の文字列を読む: std::stoi が使えないケース") {
  // /api/env の maxi は文字列で、"5-" (5 弱) / "5+" (5 強) がありうる。
  CHECK(parseIntPrefix("5-") == 5);
  CHECK(parseIntPrefix("5+") == 5);
  CHECK(parseIntPrefix("6-") == 6);
  CHECK(parseIntPrefix("3") == 3);
  CHECK(parseIntPrefix("2") == 2);
}

TEST_CASE("parseIntPrefix: 数字が無ければ nullopt（0 にしない）") {
  CHECK_FALSE(parseIntPrefix("").has_value());
  CHECK_FALSE(parseIntPrefix("abc").has_value());
  CHECK_FALSE(parseIntPrefix("-").has_value());
  CHECK_FALSE(parseIntPrefix("+").has_value());
  CHECK_FALSE(parseIntPrefix(" ").has_value());
}

TEST_CASE("parseIntPrefix: 符号と先頭空白") {
  CHECK(parseIntPrefix("-12") == -12);
  CHECK(parseIntPrefix("+7") == 7);
  CHECK(parseIntPrefix("  4") == 4);
  CHECK(parseIntPrefix("0") == 0);
}

TEST_CASE("parseIntPrefix: 範囲外は nullopt（静かに飽和させない）") {
  CHECK_FALSE(parseIntPrefix("99999999999999999999").has_value());
  CHECK_FALSE(parseIntPrefix("-99999999999999999999").has_value());
  CHECK(parseIntPrefix("2147483647") == 2147483647);
}

TEST_CASE("★震度しきい値の判定に使える（3 以上のみイベント化）") {
  const auto intensity = parseIntPrefix("5-");
  REQUIRE(intensity.has_value());
  CHECK(*intensity >= ambient::core::constants::kQuakeMinIntensity);

  const auto weak = parseIntPrefix("2");
  REQUIRE(weak.has_value());
  CHECK(*weak < ambient::core::constants::kQuakeMinIntensity);
}

TEST_CASE("★URL 組み立ての非対称: /api/photos と /api/admin/photos") {
  using ambient::core::AdminAbsPath;
  using ambient::core::joinUrl;
  using ambient::core::PhotoRelPath;

  const auto thumb = AdminAbsPath::make("/thumbs/a.jpg");
  REQUIRE(thumb.has_value());

  // /api/photos の file は先頭スラッシュ「無し」→ 区切りを補う
  CHECK(joinUrl("http://host:8080", PhotoRelPath{"photos/a.jpg"}) ==
        "http://host:8080/photos/a.jpg");

  // /api/admin/photos の thumb は先頭スラッシュ「あり」→ 補わない
  CHECK(joinUrl("http://host:8080", *thumb) == "http://host:8080/thumbs/a.jpg");

  // base の末尾スラッシュがあっても "//" にならない
  CHECK(joinUrl("http://host:8080/", PhotoRelPath{"photos/a.jpg"}) ==
        "http://host:8080/photos/a.jpg");
  CHECK(joinUrl("http://host:8080/", *thumb) == "http://host:8080/thumbs/a.jpg");
}

TEST_CASE("★AdminAbsPath は authority を乗っ取る値を構築段階で弾く（SSRF 対策）") {
  using ambient::core::AdminAbsPath;

  // thumb は認証の無い LAN 上のサーバから来る値で、攻撃者が偽装しうる。
  // joinUrl(base, AdminAbsPath) は区切りを補わずそのまま連結するため、
  // 先頭が '/' でない値を通すと接続先ホストがすり替わる。

  // "http://192.0.2.5:8080" + "@evil.example/x.jpg"
  //   -> RFC 3986 の authority は最後の '@' で分割されるので evil.example へ行く
  CHECK_FALSE(AdminAbsPath::make("@evil.example/x.jpg").has_value());

  // "http://geoframe.local" + ".evil.example/x.jpg"
  //   -> ホスト名が evil.example の DNS 配下になる
  CHECK_FALSE(AdminAbsPath::make(".evil.example/x.jpg").has_value());

  // プロトコル相対 URL。joinUrl は base の末尾スラッシュを全部落とすので、
  // base が "http://" なら "http:" まで縮み、"http://evil.example/x.jpg" になる。
  // 先頭が '/' であることだけを見ていては防げない（だから 2 つ目の条件が要る）。
  CHECK_FALSE(AdminAbsPath::make("//evil.example/x.jpg").has_value());

  // 空文字列も弾く（区切りが無く base と地続きになる）。
  CHECK_FALSE(AdminAbsPath::make("").has_value());

  // 正常な値は通り、従来どおり連結できること。
  const auto ok = AdminAbsPath::make("/api/admin/photos/thumb/IMG_0001.jpg");
  REQUIRE(ok.has_value());
  CHECK(ambient::core::joinUrl("http://192.0.2.5:8080", *ok) ==
        "http://192.0.2.5:8080/api/admin/photos/thumb/IMG_0001.jpg");

  // 先頭スラッシュ 1 個だけのパス（ルート直下）も正当。
  CHECK(AdminAbsPath::make("/thumbs/a.jpg").has_value());
  CHECK(AdminAbsPath::make("/").has_value());
}

TEST_CASE("★/api/roomenv の ok:false は「機能なし」であって障害ではない") {
  ambient::core::RoomEnvSnapshot snapshot{};
  snapshot.availability = ambient::core::RoomEnvAvailability::NotConfigured;

  // 表示しない。ただし Degraded にもしない（Home Assistant 未設定なだけ）。
  CHECK_FALSE(snapshot.displayable());
}

TEST_CASE("★/api/presence の night は 3 値（bool に潰さない）") {
  ambient::core::PresenceSnapshot unknown{};
  CHECK_FALSE(unknown.night.has_value());  // null

  ambient::core::PresenceSnapshot day{};
  day.night = false;
  REQUIRE(day.night.has_value());
  CHECK_FALSE(*day.night);

  // ★null と false は別物。value_or(false) で潰すと夜に画面が光る。
  CHECK(unknown.night.has_value() != day.night.has_value());
}

// ============================================================================
// 日時パーサ（レーン 1B）
// ============================================================================

TEST_CASE("parseIso8601Fixed: /api/env の quake の `at`（25 文字, +09:00）") {
  const auto at = parseIso8601Fixed("2026-07-29T05:40:28+09:00");
  REQUIRE(at.has_value());
  CHECK(at->year == 2026);
  CHECK(at->month == 7);
  CHECK(at->day == 29);
  CHECK(at->hour == 5);
  CHECK(at->minute == 40);
  CHECK(at->second == 28);
  CHECK(at->utcOffsetMinutes == 540);
  CHECK(toWallTime(*at).epochSeconds == 1785271228);
}

TEST_CASE("parseIso8601Fixed: Z 終端と、負のオフセット") {
  const auto zulu = parseIso8601Fixed("2026-07-29T05:40:28Z");
  REQUIRE(zulu.has_value());
  CHECK(zulu->utcOffsetMinutes == 0);
  CHECK(toWallTime(*zulu).epochSeconds == 1785303628);

  const auto minus = parseIso8601Fixed("2026-07-29T05:40:28-05:30");
  REQUIRE(minus.has_value());
  CHECK(minus->utcOffsetMinutes == -330);
  CHECK(toWallTime(*minus).epochSeconds == 1785303628 + 330 * 60);
}

TEST_CASE("★parseIso8601Fixed: /api/photos の fetched_at はオフセットが無い 19 文字") {
  // 実測値。ここが nullopt になると新着ブーストが丸ごと効かなくなる。
  const auto fetched = parseIso8601Fixed("2026-07-29T05:40:28");
  REQUIRE(fetched.has_value());
  CHECK(fetched->utcOffsetMinutes == 0);  // UTC とみなす（ヘッダに理由を明記）
  CHECK(toWallTime(*fetched).epochSeconds == 1785303628);
}

TEST_CASE("★parseIso8601Fixed: 短い文字列で落ちない（長さ検査）") {
  // 認証の無い LAN 上のサーバから来る値なので、途中で切れた入力がありうる。
  CHECK_FALSE(parseIso8601Fixed("").has_value());
  CHECK_FALSE(parseIso8601Fixed("2").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:2").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28+09:0").has_value());
  // 長すぎるものも通さない（黙って切り捨てると壊れた応答を正常扱いする）。
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28+09:00 ").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28+09:00+09:00").has_value());
}

TEST_CASE("parseIso8601Fixed: 区切り位置と非数字を弾く") {
  CHECK_FALSE(parseIso8601Fixed("2026/07/29T05:40:28").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29 05:40:28").has_value());  // 区切りは 'T'
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05-40:28").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40-28").has_value());
  CHECK_FALSE(parseIso8601Fixed("20x6-07-29T05:40:28").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-0a-29T05:40:28").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28X").has_value());  // Z 以外の 20 文字目
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28*09:00").has_value());
}

TEST_CASE("★parseIso8601Fixed: 境界の日付とうるう年") {
  CHECK(parseIso8601Fixed("2024-02-29T12:00:00Z").has_value());        // うるう年
  CHECK_FALSE(parseIso8601Fixed("2025-02-29T12:00:00Z").has_value());  // 平年
  CHECK(parseIso8601Fixed("2000-02-29T12:00:00Z").has_value());        // 400 年則
  CHECK_FALSE(parseIso8601Fixed("1900-02-29T12:00:00Z").has_value());  // 100 年則
  CHECK(parseIso8601Fixed("2026-01-01T00:00:00Z").has_value());
  CHECK(parseIso8601Fixed("2026-12-31T23:59:59Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-00-10T00:00:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-13-10T00:00:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-00T00:00:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-32T00:00:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-04-31T00:00:00Z").has_value());  // 4 月は 30 日まで
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T24:00:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T00:60:00Z").has_value());
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T00:00:61Z").has_value());
  CHECK(parseIso8601Fixed("2026-07-29T00:00:60Z").has_value());  // うるう秒は通す
  CHECK_FALSE(parseIso8601Fixed("2026-07-29T05:40:28+15:00").has_value());  // ±14:00 が上限
}

TEST_CASE("★parsePhotoTime: /api/photos の `time` は 16 文字・区切りは半角スペース") {
  const auto taken = parsePhotoTime("2024-01-15 09:00");
  REQUIRE(taken.has_value());
  CHECK(taken->year == 2024);
  CHECK(taken->month == 1);
  CHECK(taken->day == 15);
  CHECK(taken->hour == 9);
  CHECK(taken->minute == 0);
  CHECK(taken->second == 0);
  CHECK(taken->utcOffsetMinutes == 0);

  // ★区切りは 'T' ではない。ISO 形式を渡しても通さない。
  CHECK_FALSE(parsePhotoTime("2024-01-15T09:00").has_value());
  // 長さは厳密に 16。
  CHECK_FALSE(parsePhotoTime("").has_value());
  CHECK_FALSE(parsePhotoTime("2024-01-15 09:0").has_value());
  CHECK_FALSE(parsePhotoTime("2024-01-15 09:00:00").has_value());
  CHECK_FALSE(parsePhotoTime("2024-01-15 09:00 ").has_value());
  // 値の範囲。
  CHECK_FALSE(parsePhotoTime("2021-02-30 05:29").has_value());
  CHECK_FALSE(parsePhotoTime("2024-01-15 25:00").has_value());
  CHECK(parsePhotoTime("2020-02-29 00:00").has_value());
}

TEST_CASE("toWallTime / toCivilTime: std::mktime を使わない往復") {
  const CivilTime epoch{1970, 1, 1, 0, 0, 0, 0};
  CHECK(toWallTime(epoch).epochSeconds == 0);

  const CivilTime jst{2026, 7, 29, 5, 40, 28, 540};
  CHECK(toWallTime(jst).epochSeconds == 1785271228);

  // epoch -> ローカル暦 -> epoch の往復。
  const CivilTime back = toCivilTime(WallTime{1785271228}, 540);
  CHECK(back.year == 2026);
  CHECK(back.month == 7);
  CHECK(back.day == 29);
  CHECK(back.hour == 5);
  CHECK(back.minute == 40);
  CHECK(back.second == 28);
  CHECK(toWallTime(back).epochSeconds == 1785271228);

  // ★TZ 環境変数に依存しないこと（UTC で見れば前日の 20:40）。
  const CivilTime utc = toCivilTime(WallTime{1785271228}, 0);
  CHECK(utc.day == 28);
  CHECK(utc.hour == 20);
}

TEST_CASE("toCivilTime: 1970 年より前でも 1 日ずれない（floor 除算）") {
  // NTP 同期前の端末は 1970 年付近の壁時計を返す。
  const CivilTime justBefore = toCivilTime(WallTime{-1}, 0);
  CHECK(justBefore.year == 1969);
  CHECK(justBefore.month == 12);
  CHECK(justBefore.day == 31);
  CHECK(justBefore.hour == 23);
  CHECK(justBefore.minute == 59);
  CHECK(justBefore.second == 59);

  const CivilTime zero = toCivilTime(WallTime{0}, 0);
  CHECK(zero.year == 1970);
  CHECK(zero.month == 1);
  CHECK(zero.day == 1);
}

TEST_CASE("★dayOfYearNoLeap: 記念日ブーストの通日（うるう年は持たない）") {
  CHECK(dayOfYearNoLeap(1, 1) == 0);
  CHECK(dayOfYearNoLeap(1, 31) == 30);
  CHECK(dayOfYearNoLeap(2, 1) == 31);
  CHECK(dayOfYearNoLeap(3, 1) == 59);
  CHECK(dayOfYearNoLeap(12, 31) == 364);

  // ★2/29 は 3/1 と同じ 59 になる（固定表なので繰り上がる）。
  //   出典 scheduler.js が Date.UTC(2001, 1, 29)（2001 年は平年）で
  //   同じ繰り上がりをしているので、移植としてはこれが忠実。
  CHECK(dayOfYearNoLeap(2, 29) == dayOfYearNoLeap(3, 1));
  // 同じ理由で存在しない 4/31 は 5/1 と同じになる。
  CHECK(dayOfYearNoLeap(4, 31) == dayOfYearNoLeap(5, 1));

  CHECK_FALSE(dayOfYearNoLeap(0, 1).has_value());
  CHECK_FALSE(dayOfYearNoLeap(13, 1).has_value());
  CHECK_FALSE(dayOfYearNoLeap(1, 0).has_value());
  CHECK_FALSE(dayOfYearNoLeap(1, 32).has_value());
}

TEST_CASE("★日跨ぎの地震同定は `time`(HH:MM) ではなく `at`(ISO 8601) で行う") {
  // /api/env の quake の `time` は "HH:MM" の 5 文字で日付が落ちている。
  // 24 時間窓なので「昨日 23:50」と「今日 23:50」が区別できない。
  const auto yesterday = parseIso8601Fixed("2026-07-28T23:50:00+09:00");
  const auto today = parseIso8601Fixed("2026-07-29T23:50:00+09:00");
  REQUIRE(yesterday.has_value());
  REQUIRE(today.has_value());

  CHECK(toWallTime(*yesterday).epochSeconds == 1785250200);
  CHECK(toWallTime(*today).epochSeconds == 1785336600);
  // ★`time` だけなら同一に見える 2 件が、`at` なら 24 時間離れていると分かる。
  CHECK(toWallTime(*today).epochSeconds - toWallTime(*yesterday).epochSeconds == 86400);
}
