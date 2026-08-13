// test_hogo_client.cpp — hogo-meter API クライアントの契約テスト（Wave 4 / Phase 3）
//
// ############################################################################
// # ★★ 実本番 API に一切繋がないこと（GET も含む） ★★
// #
// #   https://akebi.dam-lab.work/hogo-meter は **稼働中の機器のダッシュボード**
// #   である。異常系（401/403/404/409/5xx/タイムアウト）を本番へ撃つと
// #   Cloudflare の防御を誘発し、運用ログを汚染する。
// #   このファイルが使うのは FakeHttpClient（メモリ上）だけである。
// #   実ネットワークへ出る経路は 1 つも無い。
// ############################################################################
//
// ★契約の裁定者は firmware ではなく dashboard API である。
//   値 / firmware の file:line / dashboard の file:line / 基準コミット / SHA-256 は
//   tests/contract/HOGO_CONTRACT.md にまとめてある。ここはその実行可能な写しである。
//
// ★このファイルは表駆動で SyncPolicy を step する。スレッドを起こさないので決定的。
//
// 必ずカバーするもの（Wave 4 の受け入れ条件）:
//   1. キュー満杯時に epoch 最小が上書きされること
//   2. 総試行 5 回（初回 1 回 + 再試行 4 回）で諦めること。5 秒間隔
//   3. 1 回の update() で HTTP は最大 1 件であること
//   4. 未送信キューがあれば record が config poll より優先されること
//   5. 起動直後は即時 1 回 poll すること
//   6. 異常系: 401 / 403 / 404 / 409 / 5xx / タイムアウト
//   7. firmware / deregister のキー省略が false へのリセットとして扱われること
//   8. label の 18 バイト上限（マルチバイト文字の途中で切らない）
//   9. ログに secret が出ないこと

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstdio>
#include <fstream>
#include <string>
#include <vector>

#include "ambient/core/care/care_model.hpp"
#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/care/sync_policy.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/http.hpp"
#include "ambient/transport/fixture_http_client.hpp"
#include "ambient/transport/hogo_client.hpp"

namespace {

using ambient::core::CareItem;
using ambient::core::HogoConfigSnapshot;
using ambient::core::HttpError;
using ambient::core::HttpRequest;
using ambient::core::HttpResponse;
using ambient::core::ScheduleStatus;
using ambient::core::SteadyTime;
using ambient::core::SyncActionKind;
using ambient::core::SyncPolicy;
using ambient::core::SyncReadiness;
using ambient::core::WallTime;
using ambient::transport::FakeHttpClient;
using ambient::transport::HogoClient;
using ambient::transport::HogoClientConfig;
using ambient::transport::HogoStatus;
using ambient::transport::HogoSync;
using ambient::transport::HogoSyncOutcome;
namespace constants = ambient::core::constants;

// ★実運用値と同じ「パス接頭辞込み・末尾スラッシュ無し」の形。
//   ホストはテスト専用の非実在ドメインにしてある（誤って実通信しても解決しない）。
constexpr const char* kBaseUrl = "https://hogo.invalid/hogo-meter";
constexpr const char* kDeviceId = "aabbccddeeff";
constexpr const char* kSecret = "s3cr3t-do-not-log-me";

constexpr SyncReadiness kReady{true, true};

[[nodiscard]] SteadyTime at(std::int64_t ms) noexcept { return SteadyTime{ms}; }
[[nodiscard]] WallTime epoch(std::int64_t seconds) noexcept { return WallTime{seconds}; }

[[nodiscard]] HogoClientConfig makeConfig() {
  HogoClientConfig config;
  config.baseUrl = kBaseUrl;
  config.deviceId = kDeviceId;
  config.deviceSecret = kSecret;
  return config;
}

[[nodiscard]] std::string configUrl() {
  return ambient::transport::hogoConfigUrl(kBaseUrl, kDeviceId);
}
[[nodiscard]] std::string recordsUrl() {
  return ambient::transport::hogoRecordsUrl(kBaseUrl, kDeviceId);
}

// 3 スロットの正常な config 応答（dashboard の getAgentConfig が返す形）。
[[nodiscard]] std::string okConfigBody() {
  return R"({"items":[
    {"slot":0,"label":"投薬","lastEpoch":1700000000,"status":1,"shouldLight":false},
    {"slot":1,"label":"目薬","lastEpoch":1700000100,"status":2,"shouldLight":true},
    {"slot":2,"label":"ご飯","lastEpoch":0,"status":3,"shouldLight":true}
  ]})";
}

// 記録 POST の成功応答。★204 No Content・空ボディ
// (dashboard/src/app/api/agent/[deviceId]/records/route.ts:55)。
[[nodiscard]] HttpResponse noContent() {
  HttpResponse response;
  response.status = 204;
  return response;
}

[[nodiscard]] HttpResponse jsonError(int status, std::string body) {
  HttpResponse response;
  response.status = status;
  response.body = std::move(body);
  response.contentType = "application/json";
  return response;
}

// Cloudflare / リバースプロキシは HTML を返すことがある。
[[nodiscard]] HttpResponse htmlError(int status) {
  HttpResponse response;
  response.status = status;
  response.body = "<!doctype html><html><body><h1>" + std::to_string(status) + "</h1></body></html>";
  response.contentType = "text/html;charset=utf-8";
  return response;
}

}  // namespace

// ============================================================================
// URL の組み立て — ★パス接頭辞込み・正規化しない素朴な連結
// ============================================================================

TEST_CASE("hogo: URL は baseUrl + \"/api/agent/{id}/...\" の素朴な連結") {
  // 出典: server_sync.cpp:221 / 399 / 469
  CHECK(ambient::transport::hogoConfigUrl(kBaseUrl, kDeviceId) ==
        "https://hogo.invalid/hogo-meter/api/agent/aabbccddeeff/config");
  CHECK(ambient::transport::hogoRecordsUrl(kBaseUrl, kDeviceId) ==
        "https://hogo.invalid/hogo-meter/api/agent/aabbccddeeff/records");
  CHECK(ambient::transport::hogoDeregisterAckUrl(kBaseUrl, kDeviceId) ==
        "https://hogo.invalid/hogo-meter/api/agent/aabbccddeeff/deregister-ack");
}

TEST_CASE("hogo: ★baseUrl を正規化しない（末尾スラッシュは壊れたまま出る）") {
  // ★geoframe_client のように末尾スラッシュを削らない。上流が削らない以上、
  //   こちらだけ黙って直すと「Mac では通るが Tab5 では 404」という差が生まれる。
  CHECK(ambient::transport::hogoConfigUrl("https://h.invalid/hogo-meter/", kDeviceId) ==
        "https://h.invalid/hogo-meter//api/agent/aabbccddeeff/config");

  // 代わりに「壊れる設定」を **検出** できるようにしてある（黙って直すのではなく）。
  CHECK(ambient::transport::hogoValidateBaseUrl(kBaseUrl).empty());
  CHECK_FALSE(ambient::transport::hogoValidateBaseUrl("https://h.invalid/hogo-meter/").empty());
  CHECK_FALSE(ambient::transport::hogoValidateBaseUrl("").empty());
  CHECK_FALSE(ambient::transport::hogoValidateBaseUrl("ftp://h.invalid").empty());
}

TEST_CASE("hogo: 記録のボディは配列ではなく単発 1 件") {
  // 出典: server_sync.cpp:415-416 / dashboard validation.ts:101-104（agentRecordSchema）
  CHECK(ambient::transport::hogoRecordBody(CareItem::Toilet, epoch(1700000000)) ==
        R"({"slot":0,"recordedAt":1700000000})");
  CHECK(ambient::transport::hogoRecordBody(CareItem::Slot5, epoch(1700000123)) ==
        R"({"slot":5,"recordedAt":1700000123})");
}

// ============================================================================
// 9. ログに secret が出ないこと
// ============================================================================

TEST_CASE("hogo: ★secret が URL / ボディ / 診断メッセージ / ログに出ない") {
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  http.respondWith(recordsUrl(), noContent());

  HogoClientConfig config = makeConfig();
  config.cfAccessClientId = "cf-client-id-secret";
  config.cfAccessClientSecret = "cf-client-secret-value";
  config.fwVersion = "1.2.3";
  config.failedFwVersion = "1.2.2";
  config.failedFwAttempt = 7;
  HogoClient client{http, config};

  const HttpRequest configRequest = client.buildConfigRequest();
  const HttpRequest recordRequest = client.buildRecordRequest(CareItem::Eye, epoch(1700000000));

  // secret は **ヘッダの値としてだけ** 存在する。
  CHECK(configRequest.url.find(kSecret) == std::string::npos);
  CHECK(recordRequest.url.find(kSecret) == std::string::npos);
  CHECK(recordRequest.body.find(kSecret) == std::string::npos);

  bool secretHeaderPresent = false;
  for (const auto& header : configRequest.headers) {
    if (header.name == "X-Device-Secret" && header.value == kSecret) {
      secretHeaderPresent = true;
    }
  }
  CHECK(secretHeaderPresent);

  // ★ログ用の整形では伏せられる。
  const std::string configLog = ambient::transport::redactForLog(configRequest);
  const std::string recordLog = ambient::transport::redactForLog(recordRequest);
  CHECK(configLog.find(kSecret) == std::string::npos);
  CHECK(recordLog.find(kSecret) == std::string::npos);
  CHECK(configLog.find("cf-client-secret-value") == std::string::npos);
  CHECK(configLog.find("cf-client-id-secret") == std::string::npos);
  CHECK(configLog.find("***") != std::string::npos);
  // 伏せないもの（診断に要る）は残っている。
  CHECK(configLog.find("/api/agent/aabbccddeeff/config") != std::string::npos);
  CHECK(configLog.find("1.2.3") != std::string::npos);
  CHECK(configLog.find("1.2.2#7") != std::string::npos);
  CHECK(recordLog.find(R"({"slot":1,"recordedAt":1700000000})") != std::string::npos);

  // 診断メッセージにも出ない（成功・失敗の両方）。
  HogoConfigSnapshot snapshot;
  const auto okResult = client.pollConfig(snapshot);
  CHECK(okResult.message.find(kSecret) == std::string::npos);

  FakeHttpClient failing;
  failing.respondWith(configUrl(), htmlError(401));
  HogoClient failingClient{failing, makeConfig()};
  const auto failResult = failingClient.pollConfig(snapshot);
  CHECK_FALSE(failResult.ok());
  CHECK(failResult.message.find(kSecret) == std::string::npos);
  CHECK_FALSE(failResult.message.empty());
}

TEST_CASE("hogo: 平文 http では Cloudflare トークンを送らない") {
  // 出典: server_sync.cpp:241-244（インターネットで有効な資格情報を平文に流さない）
  FakeHttpClient http;
  HogoClientConfig config = makeConfig();
  config.baseUrl = "http://192.0.2.20:3000/hogo-meter";
  config.cfAccessClientId = "id";
  config.cfAccessClientSecret = "secret";
  HogoClient client{http, config};

  std::size_t cfHeaders = 0;
  for (const auto& header : client.buildConfigRequest().headers) {
    if (header.name == "CF-Access-Client-Id" || header.name == "CF-Access-Client-Secret") {
      ++cfHeaders;
    }
  }
  CHECK(cfHeaders == 0);

  config.baseUrl = kBaseUrl;  // https:// なら送る
  HogoClient secure{http, config};
  cfHeaders = 0;
  for (const auto& header : secure.buildConfigRequest().headers) {
    if (header.name == "CF-Access-Client-Id" || header.name == "CF-Access-Client-Secret") {
      ++cfHeaders;
    }
  }
  CHECK(cfHeaders == 2);
}

// ----------------------------------------------------------------------------
// ★平文 http では **X-Device-Secret も** 送らない（上流と挙動を変えた点 #4）
// ----------------------------------------------------------------------------
//
// 上流は http:// を「開発・復旧用の平文フォールバック」として許可し、
// X-Device-Secret を平文で送る（server_sync.cpp:227, net_client.h）。
// Mac 側は送らない。差分の理由は tests/contract/HOGO_CONTRACT.md にある。

TEST_CASE("hogo: ★平文 http では X-Device-Secret を送らない") {
  FakeHttpClient http;
  HogoClientConfig config = makeConfig();
  config.baseUrl = "http://192.0.2.20:3000/hogo-meter";
  HogoClient client{http, config};

  for (const HttpRequest& request :
       {client.buildConfigRequest(),
        client.buildRecordRequest(CareItem::Eye, epoch(1700000000))}) {
    for (const auto& header : request.headers) {
      CHECK(header.name != "X-Device-Secret");
      CHECK(header.value.find(kSecret) == std::string::npos);
    }
    CHECK(request.url.find(kSecret) == std::string::npos);
    CHECK(request.body.find(kSecret) == std::string::npos);
    // ログ整形にも出ない（redactForLog は伏せるが、そもそも積まれていない）。
    CHECK(ambient::transport::redactForLog(request).find(kSecret) == std::string::npos);
  }

  // ★https なら従来どおり送る（この gate が sync を丸ごと壊していないことの確認）。
  config.baseUrl = kBaseUrl;
  HogoClient secure{http, config};
  bool present = false;
  for (const auto& header : secure.buildConfigRequest().headers) {
    if (header.name == "X-Device-Secret" && header.value == kSecret) {
      present = true;
    }
  }
  CHECK(present);
}

TEST_CASE("hogo: ★baseUrl の平文 http は検証エラーになる") {
  // 黙って 401 にするのではなく、設定時点で理由を出せるようにする。
  const std::string message =
      ambient::transport::hogoValidateBaseUrl("http://192.0.2.20:3000/hogo-meter");
  CHECK_FALSE(message.empty());
  CHECK(ambient::transport::hogoValidateBaseUrl(kBaseUrl).empty());
}

// ============================================================================
// 8. label の 18 バイト上限（マルチバイト文字の途中で切らない）
// ============================================================================

TEST_CASE("hogo: ★label は 18 バイトで丸められ、UTF-8 の途中で切れない") {
  // dashboard は書き込み時に 18 バイトを検証している (validation.ts:29-31, types.ts:3)。
  // これは **防御的な二段目** であり、API を直接叩かれた場合にだけ効く。
  const std::string body =
      R"({"items":[
        {"slot":0,"label":"あいうえおか","lastEpoch":0,"status":0},
        {"slot":1,"label":"あいうえおかき","lastEpoch":0,"status":0},
        {"slot":2,"label":"0123456789012345678901234567","lastEpoch":0,"status":0}
      ]})";

  HogoConfigSnapshot snapshot;
  REQUIRE(ambient::transport::parseHogoConfigJson(body, snapshot));

  CHECK(snapshot.slots[0].label == "あいうえおか");  // 18 バイトちょうど
  CHECK(snapshot.slots[0].label.size() == 18);

  // ★21 バイト → 18 バイトで切ると 3 バイト文字の途中。境界まで戻して 6 文字。
  CHECK(snapshot.slots[1].label == "あいうえおか");
  CHECK(snapshot.slots[1].label.size() == 18);

  CHECK(snapshot.slots[2].label.size() == 18);

  // 全 slot が上限を守っていること（表示に渡る前の保証）。
  std::size_t violations = 0;
  for (const auto& slot : snapshot.slots) {
    if (slot.label.size() > constants::kLabelMaxBytes) {
      ++violations;
    }
  }
  CHECK(violations == 0);
}

// ============================================================================
// 7. firmware / deregister のキー省略は false へのリセット
// ============================================================================

TEST_CASE("hogo: ★deregister はキー省略で false へ戻る（2 段階で確認する）") {
  // 出典: firmware server_sync.cpp:287-298 の `| false`
  //       dashboard config/route.ts:51-53「非対象はキー省略」
  // ★「最初から無ければ false」だけでは **リセット** を試験できない。
  //   Web 側で要求を取り消したときに端末が追従するかがこの契約の眼目である。
  HogoConfigSnapshot snapshot;

  const std::string requested =
      R"({"deregister":true,"items":[{"slot":0,"label":"a","lastEpoch":0,"status":0}]})";
  REQUIRE(ambient::transport::parseHogoConfigJson(requested, snapshot));
  REQUIRE(snapshot.deregisterRequested);

  // 同じ snapshot へ「キーが無い」応答を適用する。
  const std::string cancelled = R"({"items":[{"slot":0,"label":"a","lastEpoch":0,"status":0}]})";
  REQUIRE(ambient::transport::parseHogoConfigJson(cancelled, snapshot));
  CHECK_FALSE(snapshot.deregisterRequested);
}

TEST_CASE("hogo: ★firmware オファーもキー省略で消える（残留しない）") {
  // 出典: server_sync.cpp:345-365 / dashboard config/route.ts:49-50
  HogoConfigSnapshot snapshot;

  const std::string offered =
      R"({"items":[{"slot":0,"label":"a","lastEpoch":0,"status":0}],
          "firmware":{"version":"1.2.3","sha256":"ab12","sig":"BASE64SIG","attempt":3}})";
  REQUIRE(ambient::transport::parseHogoConfigJson(offered, snapshot));
  REQUIRE(snapshot.firmware.present);
  CHECK(snapshot.firmware.version == "1.2.3");
  CHECK(snapshot.firmware.attempt == 3);

  const std::string gone = R"({"items":[{"slot":0,"label":"a","lastEpoch":0,"status":0}]})";
  REQUIRE(ambient::transport::parseHogoConfigJson(gone, snapshot));
  CHECK_FALSE(snapshot.firmware.present);
  CHECK(snapshot.firmware.version.empty());
  CHECK(snapshot.firmware.attempt == 0);
}

TEST_CASE("hogo: ★sig が空の firmware オファーは掴まない（未署名を受け付けない）") {
  // 出典: server_sync.cpp:353-364「sig が空 = 旧サーバーや異常応答は安全側で false」
  struct Row {
    const char* json;
    bool expectPresent;
  };
  const Row rows[] = {
      {R"({"items":[],"firmware":{"version":"1.0","sha256":"ab","sig":"S"}})", true},
      {R"({"items":[],"firmware":{"version":"1.0","sha256":"ab","sig":""}})", false},
      {R"({"items":[],"firmware":{"version":"1.0","sha256":"ab"}})", false},
      {R"({"items":[],"firmware":{"version":"","sha256":"ab","sig":"S"}})", false},
      {R"({"items":[],"firmware":{"version":"1.0","sha256":"","sig":"S"}})", false},
      {R"({"items":[],"firmware":{}})", false},
  };

  std::size_t violations = 0;
  for (const Row& row : rows) {
    HogoConfigSnapshot snapshot;
    if (!ambient::transport::parseHogoConfigJson(row.json, snapshot)) {
      ++violations;
      continue;
    }
    if (snapshot.firmware.present != row.expectPresent) {
      ++violations;
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("hogo: shouldLight / status の欠落と範囲外は安全側へ倒れる") {
  // 出典: server_sync.cpp:325-333
  const std::string body =
      R"({"items":[
        {"slot":0,"label":"a","lastEpoch":5},
        {"slot":1,"label":"b","lastEpoch":6,"status":99,"shouldLight":false},
        {"slot":2,"label":"c","lastEpoch":7,"status":-1}
      ]})";
  HogoConfigSnapshot snapshot;
  REQUIRE(ambient::transport::parseHogoConfigJson(body, snapshot));

  // shouldLight の欠落 → true（★false 既定にすると「点灯しない」へ意味が反転する）
  CHECK(snapshot.slots[0].shouldLight);
  CHECK(snapshot.slots[0].status == ScheduleStatus::None);  // status 欠落 → 0
  CHECK(snapshot.slots[0].lastEpoch == epoch(5));

  // 範囲外の status → None（クラッシュさせない）
  CHECK(snapshot.slots[1].status == ScheduleStatus::None);
  CHECK_FALSE(snapshot.slots[1].shouldLight);
  CHECK(snapshot.slots[2].status == ScheduleStatus::None);

  CHECK(snapshot.activeSlots == 3);
}

TEST_CASE("hogo: 想定外の slot は無視され、activeSlots は最大 slot + 1") {
  // 出典: server_sync.cpp:313-315, 336-338
  const std::string body =
      R"({"items":[
        {"slot":0,"label":"a"},{"slot":5,"label":"f"},
        {"slot":6,"label":"x"},{"slot":-1,"label":"y"}
      ]})";
  HogoConfigSnapshot snapshot;
  REQUIRE(ambient::transport::parseHogoConfigJson(body, snapshot));
  CHECK(snapshot.activeSlots == 6);
  CHECK(snapshot.slots[0].present);
  CHECK(snapshot.slots[5].present);
  CHECK_FALSE(snapshot.slots[1].present);
}

TEST_CASE("hogo: items が無い / JSON でない応答は ParseFailed（落ちない）") {
  struct Row {
    const char* body;
    bool expectOk;
  };
  const Row rows[] = {
      {R"({"items":[]})", true},
      {R"({"deregister":true})", false},  // items が無い（server_sync.cpp:300-304）
      {"<!doctype html><html>403</html>", false},
      {"", false},
      {R"({"items":[)", false},              // 途中で切れている
      {R"([{"slot":0}])", false},            // 裸の配列（/api/photos の形）
      {"[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]", false},  // 深すぎる
  };

  std::size_t violations = 0;
  for (const Row& row : rows) {
    HogoConfigSnapshot snapshot;
    if (ambient::transport::parseHogoConfigJson(row.body, snapshot) != row.expectOk) {
      ++violations;
    }
  }
  CHECK(violations == 0);
}

// ============================================================================
// 5. 起動直後は即時 1 回 poll する
// ============================================================================

TEST_CASE("hogo: ★起動直後は即時 1 回 poll する（FakeHttpClient で観測）") {
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  // t=0 の 1 回目の update() で既に 1 件飛ぶ。
  const HogoSyncOutcome first = sync.update(at(0), kReady);
  CHECK(first.action.kind == SyncActionKind::PollConfig);
  CHECK(first.executed);
  CHECK(first.result.ok());
  CHECK(first.haveConfig);
  CHECK(http.callCount() == 1);
  CHECK(http.callCountFor(configUrl()) == 1);

  // 取り込んだ config が CareModel へ渡せる形になっている。
  CHECK(first.config.activeSlots == 3);
  CHECK(first.config.slots[0].label == "投薬");
  CHECK(first.config.slots[0].status == ScheduleStatus::Green);
  CHECK_FALSE(first.config.slots[0].shouldLight);
  CHECK(first.config.slots[1].status == ScheduleStatus::Yellow);
  CHECK(first.config.slots[2].status == ScheduleStatus::Red);

  // 30 秒経つまで 1 件も増えない。
  const HogoSyncOutcome idle = sync.update(at(29'999), kReady);
  CHECK(idle.action.kind == SyncActionKind::None);
  CHECK_FALSE(idle.executed);
  CHECK(http.callCount() == 1);

  const HogoSyncOutcome second = sync.update(at(30'000), kReady);
  CHECK(second.action.kind == SyncActionKind::PollConfig);
  CHECK(http.callCount() == 2);
}

TEST_CASE("hogo: ★圏外 / NTP 未同期では 1 件も投げず、即時 poll の権利も残る") {
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  const HogoSyncOutcome offline = sync.update(at(0), SyncReadiness{false, true});
  CHECK(offline.action.kind == SyncActionKind::None);
  CHECK_FALSE(offline.executed);
  CHECK(http.callCount() == 0);

  const HogoSyncOutcome noTime = sync.update(at(1'000), SyncReadiness{true, false});
  CHECK_FALSE(noTime.executed);
  CHECK(http.callCount() == 0);

  // 100 秒後に上がったら、30 秒待たずに即座に 1 回目が飛ぶ。
  const HogoSyncOutcome ready = sync.update(at(100'000), kReady);
  CHECK(ready.action.kind == SyncActionKind::PollConfig);
  CHECK(http.callCount() == 1);
}

// ============================================================================
// 3. 1 回の update() で HTTP は最大 1 件
// ============================================================================

TEST_CASE("hogo: ★1 回の update() で HTTP は最大 1 件（callCount の差分で確認）") {
  // ★nextAction が 1 個返したことを見るだけでは、駆動側の二度撃ちを検出できない。
  //   FakeHttpClient の呼び出し回数の **差分** で観測する。
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  http.respondWith(recordsUrl(), noContent());

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  // record も poll も両方やりたい状態を作る（キューに 3 件 + 未 poll）。
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'001), at(0));
  policy.enqueueRecord(CareItem::Eye, epoch(1'700'000'002), at(0));
  policy.enqueueRecord(CareItem::Food, epoch(1'700'000'003), at(0));

  std::size_t violations = 0;
  std::size_t previous = 0;
  std::size_t executedCount = 0;
  for (std::int64_t ms = 0; ms <= 120'000; ms += 1'000) {
    const HogoSyncOutcome outcome = sync.update(at(ms), kReady);
    const std::size_t delta = http.callCount() - previous;
    previous = http.callCount();

    // ★1 tick あたりの HTTP は 0 件か 1 件。2 件以上は絶対に起きない。
    if (delta > constants::kMaxHttpRequestsPerUpdate) {
      ++violations;
    }
    // executed と実際の呼び出しが一致すること（数え漏れの検出）。
    if (outcome.executed != (delta == 1)) {
      ++violations;
    }
    if (outcome.executed) {
      ++executedCount;
    }
    // action が None なら 1 件も投げない。
    if (outcome.action.none() && delta != 0) {
      ++violations;
    }
  }
  CHECK(violations == 0);
  CHECK(executedCount > 0);  // 被覆: 実際に何度も投げている
}

// ============================================================================
// 4. 未送信キューがあれば record が config poll より優先される
// ============================================================================

TEST_CASE("hogo: ★record が config poll より優先される") {
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  http.respondWith(recordsUrl(), noContent());

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  // 未 poll（= poll したい）状態で記録が入る。
  policy.enqueueRecord(CareItem::Eye, epoch(1'700'000'000), at(0));

  const HogoSyncOutcome first = sync.update(at(0), kReady);
  CHECK(first.action.kind == SyncActionKind::SendRecord);
  CHECK(http.callCountFor(recordsUrl()) == 1);
  CHECK(http.callCountFor(configUrl()) == 0);  // ★poll はまだ 1 度も飛んでいない

  // キューが空になって初めて poll へ回る。
  const HogoSyncOutcome second = sync.update(at(1), kReady);
  CHECK(second.action.kind == SyncActionKind::PollConfig);
  CHECK(http.callCountFor(configUrl()) == 1);

  // 送られたボディが単発 1 件の形であること。
  REQUIRE(http.requests().size() == 2);
  CHECK(http.requests()[0].method == ambient::core::HttpMethod::Post);
  CHECK(http.requests()[0].body == R"({"slot":1,"recordedAt":1700000000})");
  CHECK(http.requests()[1].method == ambient::core::HttpMethod::Get);
  CHECK(http.requests()[1].body.empty());
}

TEST_CASE("hogo: ★バックオフ中は config poll も走らない（上流の意図的な挙動）") {
  // 上流 update() は active な要素があれば drain して return する
  // (server_sync.cpp:62-67)。期限前なら何もしない (server_sync.cpp:387-390)。
  // ★これは移植の忠実さであってバグではない。HOGO_CONTRACT.md に記載。
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  http.respondWith(recordsUrl(), jsonError(500, R"({"error":"boom"})"));

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  const HogoSyncOutcome attempt = sync.update(at(0), kReady);
  REQUIRE(attempt.action.kind == SyncActionKind::SendRecord);
  CHECK_FALSE(attempt.result.ok());

  // バックオフ中: 未 poll なのに config が 1 件も飛ばない。
  std::size_t violations = 0;
  for (std::int64_t ms = 1'000; ms <= 4'000; ms += 1'000) {
    const HogoSyncOutcome outcome = sync.update(at(ms), kReady);
    if (!outcome.action.none() || outcome.executed) {
      ++violations;
    }
  }
  CHECK(violations == 0);
  CHECK(http.callCountFor(configUrl()) == 0);

  // 5 秒でリトライが起きる。
  const HogoSyncOutcome retry = sync.update(at(5'000), kReady);
  CHECK(retry.action.kind == SyncActionKind::SendRecord);
  CHECK(http.callCountFor(recordsUrl()) == 2);
}

// ============================================================================
// 2. 総試行 5 回（初回 1 回 + 再試行 4 回）で諦める。5 秒間隔
// ============================================================================

TEST_CASE("hogo: ★総試行 5 回で諦める（HTTP の呼び出し回数で確認）") {
  // ★RECORD_MAX_RETRIES = 5 は「5 回再試行」ではない（constants.hpp:116-118）。
  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  http.respondWith(recordsUrl(), jsonError(500, R"({"error":"server"})"));

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};
  policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

  std::vector<std::int64_t> attemptTimes;
  std::size_t violations = 0;
  for (std::int64_t ms = 0; ms <= 60'000; ms += 1'000) {
    const std::size_t before = http.callCountFor(recordsUrl());
    const HogoSyncOutcome outcome = sync.update(at(ms), kReady);
    if (http.callCountFor(recordsUrl()) > before) {
      attemptTimes.push_back(ms);
      if (outcome.result.status != HogoStatus::HttpFailed || outcome.result.httpStatus != 500) {
        ++violations;  // ★ループ内で CHECK を撃たない。集計する。
      }
    }
  }
  CHECK(violations == 0);

  REQUIRE(attemptTimes.size() == 5);  // 総試行 5 回
  CHECK(attemptTimes[0] == 0);
  CHECK(attemptTimes[1] == 5'000);   // 5 秒間隔
  CHECK(attemptTimes[2] == 10'000);
  CHECK(attemptTimes[3] == 15'000);
  CHECK(attemptTimes[4] == 20'000);

  CHECK(policy.droppedRecords() == 1);
  CHECK(policy.activeCount() == 0);
  // ★破棄されたあとは config poll へ戻る（永久リトライしない）。
  CHECK(http.callCountFor(configUrl()) > 0);
}

TEST_CASE("hogo: 4 回失敗してから 204 が返れば破棄されない") {
  FakeHttpClient http;
  // ★FakeHttpClient は同じ URL への仕込みをキューとして消費する
  //   （最後の 1 件は以降ずっと返る）。「4 回失敗してから成功」がこれで書ける。
  http.respondWith(recordsUrl(), jsonError(503, "{}"));
  http.respondWith(recordsUrl(), jsonError(503, "{}"));
  http.respondWith(recordsUrl(), jsonError(503, "{}"));
  http.respondWith(recordsUrl(), jsonError(503, "{}"));
  http.respondWith(recordsUrl(), noContent());
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};
  policy.enqueueRecord(CareItem::Food, epoch(1'700'000'000), at(0));

  for (std::int64_t ms = 0; ms <= 25'000; ms += 1'000) {
    const HogoSyncOutcome outcome = sync.update(at(ms), kReady);
    static_cast<void>(outcome);
  }

  CHECK(policy.droppedRecords() == 0);
  CHECK(policy.sentRecords() == 1);
  CHECK(http.callCountFor(recordsUrl()) == 5);
}

// ============================================================================
// 1. キュー満杯時に epoch 最小が上書きされる
// ============================================================================

TEST_CASE("hogo: ★キュー満杯で epoch 最小が上書きされ、その 1 件は送られない") {
  FakeHttpClient http;
  http.respondWith(recordsUrl(), noContent());
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");

  SyncPolicy policy;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  // 9 件を、添字順 = 古い順にならない epoch で積む。最小は 1'000'050。
  const std::int64_t epochs[] = {
      1'000'400, 1'000'200, 1'000'900, 1'000'050, 1'000'700,
      1'000'300, 1'000'600, 1'000'800, 2'000'000,
  };
  for (const std::int64_t value : epochs) {
    policy.enqueueRecord(CareItem::Toilet, epoch(value), at(0));
  }
  CHECK(policy.evictedRecords() == 1);
  CHECK(policy.activeCount() == SyncPolicy::kQueueSize);

  // キューを空になるまで駆動する。
  for (std::int64_t ms = 0; ms <= 20'000; ms += 1'000) {
    const HogoSyncOutcome outcome = sync.update(at(ms), kReady);
    static_cast<void>(outcome);
    if (policy.activeCount() == 0) {
      break;
    }
  }
  CHECK(policy.activeCount() == 0);

  // 送られたボディを集めて、追い出された epoch が **含まれない** ことを確認する。
  std::vector<std::string> bodies;
  for (const auto& request : http.requests()) {
    if (request.method == ambient::core::HttpMethod::Post) {
      bodies.push_back(request.body);
    }
  }
  CHECK(bodies.size() == SyncPolicy::kQueueSize);  // 8 件だけ送られた（9 件目ではない）

  std::size_t evictedSeen = 0;
  std::size_t newestSeen = 0;
  for (const std::string& body : bodies) {
    if (body.find("1000050") != std::string::npos) {
      ++evictedSeen;
    }
    if (body.find("2000000") != std::string::npos) {
      ++newestSeen;
    }
  }
  CHECK(evictedSeen == 0);  // ★最も古い 1 件は捨てられた
  CHECK(newestSeen == 1);   // ★直近の 1 件は生き残った
}

// ============================================================================
// 6. 異常系 — ★すべて Fake でのみ再現する（本番 API へ撃たない）
// ============================================================================

TEST_CASE("hogo: ★異常系（401/403/404/409/5xx/タイムアウト）を表で網羅する") {
  struct Row {
    const char* name;
    int status;             // 0 = トランスポート失敗
    HttpError error;
    bool html;              // 本文が HTML か
    HogoStatus expected;
  };

  // 出典（dashboard 側の意味）:
  //   401 = secret 不一致 / 404 = deviceId 不明（device-auth.ts:86-89、検証順は
  //         404 → 401 → 400。records/route.ts:14）
  //   403 = Cloudflare Access 等の手前で拒否（本文は HTML）
  //   409 = deregister-ack が取り消された契約 (deregister-ack/route.ts:40)
  //   5xx = サーバ側障害
  const Row rows[] = {
      {"401 unauthorized", 401, HttpError::None, false, HogoStatus::HttpFailed},
      {"403 forbidden(HTML)", 403, HttpError::None, true, HogoStatus::HttpFailed},
      {"404 device not found", 404, HttpError::None, false, HogoStatus::HttpFailed},
      {"409 conflict", 409, HttpError::None, false, HogoStatus::HttpFailed},
      {"400 bad request", 400, HttpError::None, false, HogoStatus::HttpFailed},
      {"500 internal", 500, HttpError::None, true, HogoStatus::HttpFailed},
      {"502 bad gateway(HTML)", 502, HttpError::None, true, HogoStatus::HttpFailed},
      {"503 unavailable", 503, HttpError::None, false, HogoStatus::HttpFailed},
      {"timeout", 0, HttpError::Timeout, false, HogoStatus::TransportFailed},
      {"connect failed", 0, HttpError::ConnectFailed, false, HogoStatus::TransportFailed},
      {"tls failed", 0, HttpError::TlsFailed, false, HogoStatus::TransportFailed},
      {"too large", 0, HttpError::TooLarge, false, HogoStatus::TransportFailed},
  };

  std::size_t violations = 0;
  std::string firstFailure;
  for (const Row& row : rows) {
    // --- config poll 側 ---
    {
      FakeHttpClient http;
      if (row.status == 0) {
        http.failWith(configUrl(), row.error);
      } else {
        http.respondWith(configUrl(),
                         row.html ? htmlError(row.status)
                                  : jsonError(row.status, R"({"error":"x"})"));
      }
      HogoClient client{http, makeConfig()};
      HogoConfigSnapshot snapshot;
      const auto result = client.pollConfig(snapshot);

      if (result.status != row.expected || result.ok()) {
        ++violations;
        if (firstFailure.empty()) {
          firstFailure = std::string{row.name} + " (config) -> " +
                         ambient::transport::toString(result.status);
        }
      }
      // ★HTML を受け取っても JSON パーサへ渡らない（ステータスで分岐してから parse）。
      if (snapshot.activeSlots != 0) {
        ++violations;
      }
      // 診断メッセージが空でなく、secret を含まない。
      if (result.message.empty() || result.message.find(kSecret) != std::string::npos) {
        ++violations;
      }
    }

    // --- record POST 側: どの異常でも「1 回の失敗」として同じ扱いになる ---
    {
      FakeHttpClient http;
      if (row.status == 0) {
        http.failWith(recordsUrl(), row.error);
      } else {
        http.respondWith(recordsUrl(),
                         row.html ? htmlError(row.status)
                                  : jsonError(row.status, R"({"error":"x"})"));
      }
      SyncPolicy policy;
      HogoClient client{http, makeConfig()};
      HogoSync sync{policy, client};
      policy.enqueueRecord(CareItem::Toilet, epoch(1'700'000'000), at(0));

      const HogoSyncOutcome outcome = sync.update(at(0), kReady);
      if (!outcome.executed || outcome.result.ok()) {
        ++violations;
      }
      // ★上流はステータスで分岐しない。401 も 5xx もタイムアウトも同じ減算経路
      //   (server_sync.cpp:418-443)。「401 は即諦める」を足すと、secret ローテーション
      //   直後の記録が黙って消える。
      if (policy.queue()[0].retriesLeft != constants::kRecordMaxRetries - 1) {
        ++violations;
      }
      if (policy.activeCount() != 1) {
        ++violations;
      }
    }
  }

  INFO(firstFailure);
  CHECK(violations == 0);
}

TEST_CASE("hogo: 200 で HTML が返ってきても JSON パーサが壊れない") {
  // Cloudflare のインタースティシャルなど、2xx なのに HTML というケース。
  FakeHttpClient http;
  HttpResponse response;
  response.status = 200;
  response.body = "<!doctype html><html><body>Just a moment...</body></html>";
  response.contentType = "text/html;charset=utf-8";
  http.respondWith(configUrl(), response);

  HogoClient client{http, makeConfig()};
  HogoConfigSnapshot snapshot;
  const auto result = client.pollConfig(snapshot);
  CHECK(result.status == HogoStatus::ParseFailed);
  CHECK(result.httpStatus == 200);
  CHECK(snapshot.activeSlots == 0);
}

TEST_CASE("hogo: baseUrl / deviceId が未設定なら I/O しない") {
  // 上流 server_sync.cpp:213, 372 の先頭ガード（自己登録が完了する前）。
  FakeHttpClient http;
  HogoClientConfig config;
  config.deviceSecret = kSecret;
  HogoClient client{http, config};

  CHECK_FALSE(client.configured());
  HogoConfigSnapshot snapshot;
  const auto pollResult = client.pollConfig(snapshot);
  const auto postResult = client.postRecord(CareItem::Toilet, epoch(1'700'000'000));
  CHECK_FALSE(pollResult.ok());
  CHECK_FALSE(postResult.ok());
  CHECK(http.callCount() == 0);  // ★1 件も投げない
  CHECK(pollResult.message.find(kSecret) == std::string::npos);
}

TEST_CASE("hogo: 応答の受け入れ上限が上流の値と一致する") {
  // 出典: server_sync.cpp:268 MAX_CONFIG_RESPONSE_BYTES = 2048
  CHECK(ambient::transport::kHogoMaxConfigResponseBytes == 2048);

  FakeHttpClient http;
  http.respondWith(configUrl(), 200, okConfigBody(), "application/json");
  HogoClient client{http, makeConfig()};
  const HttpRequest request = client.buildConfigRequest();
  CHECK(request.maxResponseBytes == 2048);
  CHECK(request.timeout == constants::kHttpTimeout);
}

// ============================================================================
// CareModel との結線（記録 → 204 → 次の poll で色が確定する一連の流れ）
// ============================================================================

TEST_CASE("hogo: 記録 204 のあと、色が変わるのは次の poll の後") {
  // ★「記録したら即 Green」は成立しない（CORE_CONTRACTS §5.4）。
  FakeHttpClient http;
  http.respondWith(recordsUrl(), noContent());
  // 1 回目の poll: Red。2 回目: 記録を取り込んで Green。
  http.respondWith(configUrl(), 200,
                   R"({"items":[{"slot":0,"label":"投薬","lastEpoch":0,"status":3,"shouldLight":true}]})",
                   "application/json");
  http.respondWith(
      configUrl(), 200,
      R"({"items":[{"slot":0,"label":"投薬","lastEpoch":1700000000,"status":1,"shouldLight":false}]})",
      "application/json");

  SyncPolicy policy;
  ambient::core::CareModel model;
  HogoClient client{http, makeConfig()};
  HogoSync sync{policy, client};

  // 起動 → 即時 poll（Red）
  const HogoSyncOutcome firstPoll = sync.update(at(0), kReady);
  REQUIRE(firstPoll.haveConfig);
  model.applyConfig(firstPoll.config);
  CHECK(model.view(at(0), epoch(1'700'000'000)).cards[0].status == ScheduleStatus::Red);

  // 記録 → 保留 → 確定 → キューへ
  const auto ignored = model.beginRecord(CareItem::Toilet, at(1'000), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(3'000));
  REQUIRE(committed.has_value());
  policy.enqueue(*committed, at(3'000));

  // 送信は 204 で成功する。
  const HogoSyncOutcome post = sync.update(at(3'000), kReady);
  REQUIRE(post.action.kind == SyncActionKind::SendRecord);
  CHECK(post.result.ok());
  CHECK(post.result.httpStatus == 204);

  // ★送信成功しても色はまだ Red（サーバが計算するので次の poll まで変わらない）。
  const auto midView = model.view(at(3'100), epoch(1'700'000'060));
  CHECK(midView.cards[0].status == ScheduleStatus::Red);
  CHECK(midView.cards[0].awaitingServer);
  CHECK(midView.cards[0].elapsed == "1分前");  // 経過時間はローカル値で即座に戻る
  CHECK(model.awaitingServer(at(3'100)));

  // 次の poll（30 秒後）で Green が降りてくる。
  const HogoSyncOutcome secondPoll = sync.update(at(33'000), kReady);
  REQUIRE(secondPoll.action.kind == SyncActionKind::PollConfig);
  REQUIRE(secondPoll.haveConfig);
  model.applyConfig(secondPoll.config);

  const auto finalView = model.view(at(33'100), epoch(1'700'000'120));
  CHECK(finalView.cards[0].status == ScheduleStatus::Green);
  CHECK_FALSE(finalView.cards[0].awaitingServer);
  CHECK_FALSE(model.awaitingServer(at(33'100)));
}

// ============================================================================
// CA バンドル（★経路のみ。curl_http_client.cpp への配線は未実施 = 申し送り）
// ============================================================================

TEST_CASE("hogo: HOGO_CA_BUNDLE は PEM ファイルへのパスとして読める") {
  // ★上流 ca_certs.h の ROOT_CA_BUNDLE は純 PEM で Arduino 非依存なので、
  //   Mac 側で同じ CA を使えば **Tab5 の CA ピン切れを前倒しで検出** できる。
  //   ここで作るのは読み込み経路だけ。実際の検証は疎通が可能になってから。
  const char* kEnvName = "AMBIENT_TEST_HOGO_CA_BUNDLE";

  // 未設定 → present=false（正常系。システムのトラストストアを使う）。
  ::unsetenv(kEnvName);
  {
    const auto load = ambient::transport::loadCaBundleFromEnv(kEnvName);
    CHECK_FALSE(load.present);
    CHECK_FALSE(load.ok);
    CHECK(load.pem.empty());
  }

  // 存在しないパス → present=true / ok=false（黙って無視しない）。
  ::setenv(kEnvName, "/nonexistent/path/to/ca.pem", 1);
  {
    const auto load = ambient::transport::loadCaBundleFromEnv(kEnvName);
    CHECK(load.present);
    CHECK_FALSE(load.ok);
    CHECK_FALSE(load.message.empty());
  }

  // ★一時ファイルはビルドディレクトリ名に依存させない（`cmake -B out` でも動くこと）。
  const char* tmpDir = std::getenv("TMPDIR");
  const std::string tmpBase = (tmpDir != nullptr && tmpDir[0] != '\0') ? std::string{tmpDir}
                                                                      : std::string{"/tmp/"};

  // PEM に見えないファイル → 拒否する（TLS 時に原因不明の失敗を出さないため）。
  const std::string badPath = tmpBase + "ambient-test-ca-not-pem.txt";
  {
    std::ofstream file(badPath);
    REQUIRE(file.good());  // 書けなければテストの前提が崩れている（黙って通さない）
    file << "this is not a certificate\n";
  }
  ::setenv(kEnvName, badPath.c_str(), 1);
  {
    const auto load = ambient::transport::loadCaBundleFromEnv(kEnvName);
    CHECK(load.present);
    CHECK_FALSE(load.ok);
    CHECK(load.message.find("PEM") != std::string::npos);
  }

  // PEM に見えるファイル → 読める。
  const std::string goodPath = tmpBase + "ambient-test-ca.pem";
  {
    std::ofstream file(goodPath);
    REQUIRE(file.good());
    file << "-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n";
  }
  ::setenv(kEnvName, goodPath.c_str(), 1);
  {
    const auto load = ambient::transport::loadCaBundleFromEnv(kEnvName);
    CHECK(load.present);
    CHECK(load.ok);
    CHECK(load.pem.find("BEGIN CERTIFICATE") != std::string::npos);
    CHECK(load.message.empty());
  }

  ::unsetenv(kEnvName);
  std::remove(badPath.c_str());
  std::remove(goodPath.c_str());
}

// ============================================================================
// toString（doctest の失敗メッセージが "{?}" にならないこと）
// ============================================================================

TEST_CASE("hogo: HogoStatus の toString") {
  CHECK(std::string{ambient::transport::toString(HogoStatus::Ok)} == "Ok");
  CHECK(std::string{ambient::transport::toString(HogoStatus::TransportFailed)} ==
        "TransportFailed");
  CHECK(std::string{ambient::transport::toString(HogoStatus::HttpFailed)} == "HttpFailed");
  CHECK(std::string{ambient::transport::toString(HogoStatus::ParseFailed)} == "ParseFailed");
}
