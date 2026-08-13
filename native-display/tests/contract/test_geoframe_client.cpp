// test_geoframe_client.cpp — GeoFrame API クライアントの契約テスト
//
// ★実サーバに繋がないこと。FixtureHttpClient / FakeHttpClient を使う。
//   fixture は **実サーバから採取した本物の応答**（docs/FIXTURES.md）なので、
//   ここを通ることは「実応答の形に耐える」ことの証拠になる。
//
// 特に固定したい落とし穴（すべて実測で確認済みのもの）:
//   1. /api/photos は **裸の JSON 配列**。{"items":[...]} を期待すると 0 件で
//      「成功」してしまう ← エラーにならないのでテストで縛るしかない
//   2. `ok` の有無がエンドポイントごとに **非対称**
//   3. エラー応答は **HTML**。ステータスで分岐してから parse すること
//   4. roomenv の ok:false は **HA 未設定**であって障害ではない
//   5. 信頼できない入力（深いネスト・巨大・途中で切れた本文）で落ちないこと

#include <doctest.h>

#include <cstddef>
#include <string>

#include "ambient/core/http.hpp"
#include "ambient/core/model/parse.hpp"
#include "ambient/transport/fixture_http_client.hpp"
#include "ambient/transport/geoframe_client.hpp"

namespace {

using ambient::core::EnvSnapshot;
using ambient::core::HttpError;
using ambient::core::PhotoManifest;
using ambient::core::PhotoRelPath;
using ambient::core::PresenceSnapshot;
using ambient::core::RoomEnvAvailability;
using ambient::core::RoomEnvSnapshot;
using ambient::transport::FakeHttpClient;
using ambient::transport::FixtureHttpClient;
using ambient::transport::GeoFrameClient;
using ambient::transport::GeoFrameConfig;
using ambient::transport::GeoFrameResult;
using ambient::transport::GeoFrameStatus;
using ambient::transport::parseEnvJson;
using ambient::transport::parsePhotosJson;
using ambient::transport::parsePresenceJson;

std::string fixtureDir() { return std::string(AMBIENT_ASSET_DIR) + "/fixtures"; }

GeoFrameConfig testConfig() {
  GeoFrameConfig config;
  config.baseUrl = "http://127.0.0.1:8080";  // ★fixture なので実通信はしない
  return config;
}

}  // namespace

// ============================================================================
// /api/photos — ★裸の配列
// ============================================================================

TEST_CASE("photos: fixture（実応答）を裸の配列として読める") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient client{fixtures, testConfig()};

  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);

  REQUIRE(result.ok());
  CHECK(result.httpStatus == 200);
  CHECK_FALSE(result.degraded());
  // docs/FIXTURES.md: コミット版は 50 件に縮小してある。
  CHECK(manifest.items.size() == 50);

  // ★id は実測で **整数**。PhotoId は std::string なので 10 進表記になる。
  CHECK(manifest.items.front().id == "1");
  // ★キーは `file` で、先頭スラッシュ **無し**。
  CHECK(manifest.items.front().file.value == "derivatives/fixture-0001.jpg");
  CHECK(manifest.items.front().file.value.front() != '/');
  // ★キーは `time`（takenAt ではない）。"YYYY-MM-DD HH:MM"。
  CHECK(manifest.items.front().time == "2024-01-15 09:00");
  CHECK(ambient::core::parsePhotoTime(manifest.items.front().time).has_value());
}

TEST_CASE("photos: ★rebuildIndex() が呼ばれている（indexOf が引ける）") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient client{fixtures, testConfig()};

  PhotoManifest manifest;
  REQUIRE(client.fetchPhotos(manifest).ok());

  // 索引が無くても indexOf は動く（線形探索へ落ちるだけ）ので、
  // 索引そのものが作られていることを見る。
  CHECK(manifest.idOrder.size() == manifest.items.size());

  int mismatches = 0;
  for (std::size_t i = 0; i < manifest.items.size(); ++i) {
    const auto found = ambient::core::indexOf(manifest, manifest.items[i].id);
    if (!found || *found != i) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);
}

TEST_CASE("photos: ★{items:[...]} は失敗にする（0 件で静かに成功させない）") {
  // これが「エラーにならないので気づけない」典型。配列でなければ失敗にする。
  PhotoManifest manifest;
  CHECK_FALSE(parsePhotosJson(R"({"items": [{"id": 1, "file": "a.jpg"}]})", manifest));
  CHECK(manifest.items.empty());
}

TEST_CASE("photos: 壊れた 1 件は捨てるが、残りは読む") {
  PhotoManifest manifest;
  const char* body = R"([
    {"id": 1, "file": "a.jpg", "time": "2024-01-02 03:04", "trip": 0},
    {"id": 2},
    "文字列がまざっている",
    {"file": "id-none.jpg"},
    {"id": 3, "file": "c.jpg", "trip": "7", "fav": true, "lat": "12", "elev": null,
     "quality_role": "recommended", "quality_score": 91.5}
  ])";
  REQUIRE(parsePhotosJson(body, manifest));
  CHECK(manifest.items.size() == 2);
  CHECK(manifest.items[0].id == "1");
  CHECK(manifest.items[1].id == "3");
  // 型が違っても落ちず、読めるものは読む。
  CHECK(manifest.items[1].trip == 7);
  CHECK(manifest.items[1].fav);
  CHECK(manifest.items[1].lat == doctest::Approx(12.0));
  CHECK(manifest.items[1].qualityRecommended);
  CHECK_FALSE(manifest.items[1].qualityAlternate);
  CHECK(manifest.items[1].qualityScore == doctest::Approx(91.5));
}

TEST_CASE("photos: 空配列は「0 件で成功」（サーバ起動直後に実際に起きる）") {
  PhotoManifest manifest;
  REQUIRE(parsePhotosJson("[]", manifest));
  CHECK(manifest.items.empty());
}

// ============================================================================
// ★エラー応答は JSON とは限らない — ステータスで分岐してから parse する
// ============================================================================

TEST_CASE("errors: 404 の HTML 本文でクラッシュせず HttpFailed になる") {
  FixtureHttpClient fixtures{fixtureDir()};
  // 既定のパス表に無いパスは err-not-found（404・HTML）になる。
  fixtures.mapPath("/api/photos", "err-not-found");

  GeoFrameClient client{fixtures, testConfig()};
  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);

  CHECK(result.status == GeoFrameStatus::HttpFailed);
  CHECK(result.httpStatus == 404);
  CHECK(result.error == HttpError::None);  // 通信自体は成功している
  CHECK(result.contentType == "text/html;charset=utf-8");
  CHECK(result.degraded());
  // ★HTML をパーサへ渡していないこと（渡していれば ParseFailed になる）。
  CHECK(result.status != GeoFrameStatus::ParseFailed);
}

TEST_CASE("errors: Host 検証の 403（HTML）も同様に扱える") {
  FixtureHttpClient fixtures{fixtureDir()};
  fixtures.mapPath("/api/env", "err-host-forbidden");

  GeoFrameClient client{fixtures, testConfig()};
  EnvSnapshot env;
  const GeoFrameResult result = client.fetchEnv(env);

  CHECK(result.status == GeoFrameStatus::HttpFailed);
  CHECK(result.httpStatus == 403);
  CHECK(result.bodyBytes > 0);  // 本文は届いている（HTML）
}

TEST_CASE("errors: トランスポート失敗は status=0 で HTTP エラーと区別できる") {
  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake{&fixtures};
  fake.failWith("http://127.0.0.1:8080/api/photos", HttpError::ConnectFailed);

  GeoFrameClient client{fake, testConfig()};
  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);

  CHECK(result.status == GeoFrameStatus::TransportFailed);
  CHECK(result.httpStatus == 0);
  CHECK(result.error == HttpError::ConnectFailed);
  CHECK(result.degraded());
}

TEST_CASE("errors: 実装が入れた診断（curl の ERRORBUFFER 相当）を捨てない") {
  // ★HttpError は 7 値しかない。「どのホストへの接続がどう失敗したか」は
  //   実装が body へ入れる診断にしか残らない（CurlHttpClient は
  //   CURLOPT_ERRORBUFFER をそこへ入れる）。ここではそれを FakeHttpClient で
  //   模して、message まで伝わることを固定する。
  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake{&fixtures};

  ambient::core::HttpResponse failure;
  failure.status = 0;
  failure.error = HttpError::ConnectFailed;
  failure.body = "Failed to connect to 127.0.0.1 port 8080: Connection refused";
  fake.respondWith("http://127.0.0.1:8080/api/photos", std::move(failure));

  GeoFrameClient client{fake, testConfig()};
  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);

  CHECK(result.status == GeoFrameStatus::TransportFailed);
  CHECK(result.message.find("Connection refused") != std::string::npos);
  // ★診断文はサーバからの本文ではないので受信バイト数には数えない。
  CHECK(result.bodyBytes == 0);
}

TEST_CASE("errors: 200 でも JSON でなければ ParseFailed（HTML/空/途中切れ）") {
  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake{&fixtures};

  SUBCASE("HTML が 200 で返る") {
    fake.respondWith("http://127.0.0.1:8080/api/photos", 200,
                     "<!DOCTYPE HTML><html><body>oops</body></html>", "text/html");
  }
  SUBCASE("空") {
    fake.respondWith("http://127.0.0.1:8080/api/photos", 200, "", "application/json");
  }
  SUBCASE("途中で切れている") {
    fake.respondWith("http://127.0.0.1:8080/api/photos", 200,
                     R"([{"id": 1, "file": "a.jpg")", "application/json");
  }

  GeoFrameClient client{fake, testConfig()};
  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);
  CHECK(result.status == GeoFrameStatus::ParseFailed);
  CHECK(result.httpStatus == 200);
}

TEST_CASE("errors: 応答サイズ上限を超えたら TooLarge（Tab5 は RAM に全部載る）") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameConfig config = testConfig();
  config.maxResponseBytes = 64;  // fixture より遥かに小さい

  GeoFrameClient client{fixtures, config};
  PhotoManifest manifest;
  const GeoFrameResult result = client.fetchPhotos(manifest);

  CHECK(result.status == GeoFrameStatus::TransportFailed);
  CHECK(result.error == HttpError::TooLarge);
}

// ============================================================================
// ★信頼できない入力（認証の無い LAN 上のサーバ）
// ============================================================================

TEST_CASE("security: 深すぎるネストは受け付けない（スタック枯渇攻撃）") {
  std::string deep;
  const int depth = 2000;
  deep.reserve(static_cast<std::size_t>(depth) * 2);
  for (int i = 0; i < depth; ++i) {
    deep.push_back('[');
  }
  for (int i = 0; i < depth; ++i) {
    deep.push_back(']');
  }

  PhotoManifest manifest;
  CHECK_FALSE(parsePhotosJson(deep, manifest));

  EnvSnapshot env;
  CHECK_FALSE(parseEnvJson(deep, env));
}

TEST_CASE("security: 文字列の中の括弧は深さに数えない（誤検出しない）") {
  PhotoManifest manifest;
  const char* body = R"([{"id": 1, "file": "a.jpg", "title": "[[[[{{{{ \" [[[["}])";
  REQUIRE(parsePhotosJson(body, manifest));
  REQUIRE(manifest.items.size() == 1);
  CHECK(manifest.items[0].title == R"([[[[{{{{ " [[[[)");
}

TEST_CASE("security: 上限ちょうどの深さは通す") {
  std::string body;
  for (int i = 0; i < 16; ++i) {
    body += "[";
  }
  for (int i = 0; i < 16; ++i) {
    body += "]";
  }
  PhotoManifest manifest;
  // 配列としては読めるが写真は 1 件も無い（要素がオブジェクトでないため）。
  CHECK(parsePhotosJson(body, manifest));
  CHECK(manifest.items.empty());
}

// ----------------------------------------------------------------------------
// ★reserve によるメモリ増幅（geoframe_client.hpp の「配列の件数に上限」節）
// ----------------------------------------------------------------------------
//
// ★入力は上限定数から作ること。定数を変えてもテストが追従し、かつ
//   4MB の文字列を組み立てずに **同じ経路**を通せる（テストを遅くしない）。
//
// ★Mac ではオーバーコミットのおかげで巨大 reserve が「成功」してしまうので、
//   「落ちなかった」ことは証拠にならない。capacity() / size() を直接見る。

TEST_CASE("security: 巨大な配列でも reserve が件数上限を超えない") {
  // 偽サーバが `[0,0,0,...]` を返す。要素は 2 バイト刻みなので、root.size() を
  // そのまま reserve すると **1 件も採用しないのに**巨大な確保が走る。
  const std::size_t count = ambient::transport::kMaxPhotoItems * 2;
  std::string body;
  body.reserve(count * 2 + 2);
  body.push_back('[');
  for (std::size_t i = 0; i < count; ++i) {
    if (i != 0) {
      body.push_back(',');
    }
    body.push_back('0');
  }
  body.push_back(']');

  PhotoManifest manifest;
  REQUIRE(parsePhotosJson(body, manifest));
  CHECK(manifest.items.empty());  // オブジェクトでないので 1 件も採用しない
  // ★これが本題。修正前は capacity == count（= 上限の 2 倍）になる。
  CHECK(manifest.items.capacity() <= ambient::transport::kMaxPhotoItems);
}

TEST_CASE("security: 採用件数そのものにも上限がある（push_back で膨らまない）") {
  // 全要素が **正当な写真オブジェクト**。reserve だけ抑えても push_back の
  // 繰り返しで結局増えるため、採用側にも上限が要る。
  const std::size_t count = ambient::transport::kMaxPhotoItems + 100;
  std::string body;
  body.reserve(count * 32);
  body.push_back('[');
  for (std::size_t i = 0; i < count; ++i) {
    if (i != 0) {
      body.push_back(',');
    }
    body += R"({"id":)";
    body += std::to_string(i);
    body += R"(,"file":"a.jpg"})";
  }
  body.push_back(']');

  PhotoManifest manifest;
  REQUIRE(parsePhotosJson(body, manifest));
  CHECK(manifest.items.size() == ambient::transport::kMaxPhotoItems);
  CHECK(manifest.items.capacity() <= ambient::transport::kMaxPhotoItems);
}

TEST_CASE("security: env / roomenv の配列にも件数上限がある") {
  // quakes / weekly も同じ経路（reserve(size()) + push_back）である。
  std::string env = R"({"quakes":[)";
  const std::size_t quakes = ambient::transport::kMaxQuakeItems + 10;
  for (std::size_t i = 0; i < quakes; ++i) {
    if (i != 0) {
      env.push_back(',');
    }
    env += R"({"eid":")";
    env += std::to_string(i);
    env += R"("})";
  }
  env += R"(],"weather":{"temps":[)";
  for (std::size_t i = 0; i < ambient::transport::kMaxWeatherTemps + 10; ++i) {
    if (i != 0) {
      env.push_back(',');
    }
    env += "20";
  }
  env += R"(]},"weekly":[)";
  for (std::size_t i = 0; i < ambient::transport::kMaxWeeklyDays + 10; ++i) {
    if (i != 0) {
      env.push_back(',');
    }
    env += R"({"date":"2026-01-01","code":"100"})";
  }
  env += "]}";

  EnvSnapshot snapshot;
  REQUIRE(parseEnvJson(env, snapshot));
  CHECK(snapshot.quakes.size() == ambient::transport::kMaxQuakeItems);
  CHECK(snapshot.quakes.capacity() <= ambient::transport::kMaxQuakeItems);
  CHECK(snapshot.weekly.size() == ambient::transport::kMaxWeeklyDays);
  CHECK(snapshot.weekly.capacity() <= ambient::transport::kMaxWeeklyDays);
  REQUIRE(snapshot.weather.has_value());
  CHECK(snapshot.weather->temps.size() == ambient::transport::kMaxWeatherTemps);

  std::string room = R"({"ok":true,"items":[)";
  for (std::size_t i = 0; i < ambient::transport::kMaxRoomEnvItems + 10; ++i) {
    if (i != 0) {
      room.push_back(',');
    }
    room += R"({"entity":"sensor.)";
    room += std::to_string(i);
    room += R"(","value":"1"})";
  }
  room += "]}";

  RoomEnvSnapshot rooms;
  REQUIRE(ambient::transport::parseRoomEnvJson(room, rooms));
  CHECK(rooms.items.size() == ambient::transport::kMaxRoomEnvItems);
  CHECK(rooms.items.capacity() <= ambient::transport::kMaxRoomEnvItems);
}

// ============================================================================
// /api/env — ★ok は無い / maxi は文字列 / weekly は null をとる
// ============================================================================

TEST_CASE("env: 合成fixtureを読める。★okフィールドは無い") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient client{fixtures, testConfig()};

  EnvSnapshot env;
  const GeoFrameResult result = client.fetchEnv(env);

  REQUIRE(result.ok());
  CHECK_FALSE(env.quakes.empty());
  REQUIRE(env.weather.has_value());
  CHECK_FALSE(env.weather->text.empty());
  // ★tempsはサーバースキーマどおり文字列の配列。整数へ落として持つ。
  CHECK(env.weather->temps.size() == 2);
  // ★weekly は 7 件。先頭日は pop / temp_min / temp_max が null。
  REQUIRE(env.weekly.size() == 7);
  CHECK_FALSE(env.weekly.front().pop.has_value());
  CHECK_FALSE(env.weekly.front().tempMax.has_value());
  CHECK(env.weekly[1].tempMax.has_value());
  CHECK_FALSE(env.weekly.front().date.empty());
  CHECK_FALSE(env.weekly.front().code.empty());
}

TEST_CASE("env: ★maxi は文字列のまま持つ（5弱 と 5強 を潰さない）") {
  FixtureHttpClient fixtures{fixtureDir()};
  const ambient::core::HttpResponse response = fixtures.load("quakes-inject");
  REQUIRE(response.status == 200);

  EnvSnapshot env;
  REQUIRE(parseEnvJson(response.body, env));
  REQUIRE(env.quakes.size() >= 9);

  // docs/FIXTURES.md §4 の表: FIXTURE-0005 = "5-", FIXTURE-0006 = "5+"。
  std::string weak;
  std::string strong;
  for (const auto& quake : env.quakes) {
    if (quake.eid == "FIXTURE-0005") weak = quake.maxi;
    if (quake.eid == "FIXTURE-0006") strong = quake.maxi;
  }
  CHECK(weak == "5-");
  CHECK(strong == "5+");
  // ★整数化すると弱強の別が静かに消える。parseIntPrefix はどちらも 5 を返す。
  CHECK(ambient::core::parseIntPrefix(weak).value_or(-1) == 5);
  CHECK(ambient::core::parseIntPrefix(strong).value_or(-1) == 5);
  CHECK(weak != strong);
}

TEST_CASE("env: ★日跨ぎの同定には at を使う（time は HH:MM で衝突する）") {
  FixtureHttpClient fixtures{fixtureDir()};
  const ambient::core::HttpResponse response = fixtures.load("quakes-inject");
  REQUIRE(response.status == 200);

  EnvSnapshot env;
  REQUIRE(parseEnvJson(response.body, env));

  std::string timeA;
  std::string timeB;
  std::string atA;
  std::string atB;
  for (const auto& quake : env.quakes) {
    if (quake.eid == "FIXTURE-0010") {
      timeA = quake.time;
      atA = quake.at;
    }
    if (quake.eid == "FIXTURE-0011") {
      timeB = quake.time;
      atB = quake.at;
    }
  }
  REQUIRE_FALSE(atA.empty());
  REQUIRE_FALSE(atB.empty());
  CHECK(timeA == timeB);  // ★"23:50" で衝突する
  CHECK(atA != atB);      // ★at なら区別できる
  CHECK(ambient::core::parseIso8601Fixed(atA).has_value());
}

TEST_CASE("env: weather が null でも weekly が空でも読める（気象庁の取得失敗）") {
  EnvSnapshot env;
  REQUIRE(parseEnvJson(R"({"quakes": [], "weather": null, "weekly": []})", env));
  CHECK_FALSE(env.weather.has_value());
  CHECK(env.weekly.empty());
  CHECK(env.quakes.empty());
}

// ============================================================================
// /api/roomenv — ★ok:false は障害ではない
// ============================================================================

TEST_CASE("roomenv: ok:true なら Available。value は文字列のまま") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient client{fixtures, testConfig()};

  RoomEnvSnapshot roomenv;
  const GeoFrameResult result = client.fetchRoomEnv(roomenv);

  REQUIRE(result.ok());
  CHECK(roomenv.availability == RoomEnvAvailability::Available);
  CHECK(roomenv.displayable());
  REQUIRE_FALSE(roomenv.items.empty());
  // ★value は常に文字列。"23.67" であって 23.67 ではない。
  CHECK(roomenv.items.front().value == "22.50");
  CHECK(roomenv.items.front().entityClass == "temperature");
}

TEST_CASE("roomenv: ★ok:false は NotConfigured。degraded() にしないこと") {
  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake{&fixtures};
  fake.respondWith("http://127.0.0.1:8080/api/roomenv", 200, R"({"ok": false})",
                   "application/json");

  GeoFrameClient client{fake, testConfig()};
  RoomEnvSnapshot roomenv;
  const GeoFrameResult result = client.fetchRoomEnv(roomenv);

  CHECK(result.status == GeoFrameStatus::NotConfigured);
  CHECK(roomenv.availability == RoomEnvAvailability::NotConfigured);
  CHECK_FALSE(roomenv.displayable());
  // ★ここが本質。HA 未設定の家庭で永続的に「通信異常」を出さないための境界。
  CHECK_FALSE(result.degraded());
  CHECK(result.httpStatus == 200);
}

// ============================================================================
// /api/presence — ★night は 3 値
// ============================================================================

TEST_CASE("presence: night は true / false / null の 3 値") {
  FixtureHttpClient fixtures{fixtureDir()};

  SUBCASE("true") {
    const auto response = fixtures.load("presence-night-true");
    REQUIRE(response.status == 200);
    PresenceSnapshot presence;
    REQUIRE(parsePresenceJson(response.body, presence));
    REQUIRE(presence.night.has_value());
    CHECK(*presence.night);
  }
  SUBCASE("false") {
    const auto response = fixtures.load("presence-night-false");
    REQUIRE(response.status == 200);
    PresenceSnapshot presence;
    REQUIRE(parsePresenceJson(response.body, presence));
    REQUIRE(presence.night.has_value());
    CHECK_FALSE(*presence.night);
  }
  SUBCASE("null は「判定できない」であって false ではない") {
    const auto response = fixtures.load("presence-night-null");
    REQUIRE(response.status == 200);
    PresenceSnapshot presence;
    REQUIRE(parsePresenceJson(response.body, presence));
    CHECK_FALSE(presence.night.has_value());
  }
  SUBCASE("キー自体が無い場合も nullopt へ倒す") {
    PresenceSnapshot presence;
    REQUIRE(parsePresenceJson(R"({"present": true})", presence));
    CHECK_FALSE(presence.night.has_value());
  }
}

// ============================================================================
// URL 組み立て — ★先頭スラッシュの非対称
// ============================================================================

TEST_CASE("photoUrl: file は先頭スラッシュ無しなので joinUrl が区切りを補う") {
  FixtureHttpClient fixtures{fixtureDir()};

  SUBCASE("base に末尾スラッシュが無い") {
    GeoFrameConfig config;
    config.baseUrl = "http://127.0.0.1:8080";
    GeoFrameClient client{fixtures, config};
    CHECK(client.photoUrl(PhotoRelPath{"derivatives/a.jpg"}) ==
          "http://127.0.0.1:8080/derivatives/a.jpg");
  }
  SUBCASE("base に末尾スラッシュがあっても // にならない") {
    GeoFrameConfig config;
    config.baseUrl = "http://127.0.0.1:8080/";
    GeoFrameClient client{fixtures, config};
    CHECK(client.baseUrl() == "http://127.0.0.1:8080");
    CHECK(client.photoUrl(PhotoRelPath{"derivatives/a.jpg"}) ==
          "http://127.0.0.1:8080/derivatives/a.jpg");
  }
}

TEST_CASE("client: 1 回の fetch で HTTP は 1 件だけ") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient client{fixtures, testConfig()};

  PhotoManifest manifest;
  REQUIRE(client.fetchPhotos(manifest).ok());
  CHECK(fixtures.callCount() == 1);

  EnvSnapshot env;
  REQUIRE(client.fetchEnv(env).ok());
  CHECK(fixtures.callCount() == 2);
}

TEST_CASE("client: 失敗しても out を壊さない（前回の値を保持できる）") {
  FixtureHttpClient fixtures{fixtureDir()};
  GeoFrameClient good{fixtures, testConfig()};

  PhotoManifest manifest;
  REQUIRE(good.fetchPhotos(manifest).ok());
  const std::size_t before = manifest.items.size();

  FakeHttpClient fake{&fixtures};
  fake.failWith("http://127.0.0.1:8080/api/photos", HttpError::Timeout);
  GeoFrameClient bad{fake, testConfig()};

  const GeoFrameResult result = bad.fetchPhotos(manifest);
  CHECK(result.status == GeoFrameStatus::TransportFailed);
  // ★通信に失敗したからといって手元の manifest を空にしない
  //   （サーバが落ちている間も表示を続けられること）。
  CHECK(manifest.items.size() == before);
}
