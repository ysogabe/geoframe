// hogo_client.cpp — ambient/transport/hogo_client.hpp の実装
//
// ★nlohmann / std::filesystem / getenv はこの翻訳単位に閉じ込める。
//   公開ヘッダには std::string しか出さない（Tab5 で層ごと差し替えるため）。

#include "ambient/transport/hogo_client.hpp"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <cctype>
#include <cstdlib>
#include <fstream>
#include <ios>
#include <sstream>
#include <utility>

namespace ambient::transport {
namespace {

using Json = nlohmann::json;

// ---------------------------------------------------------------------------
// ★ネストの深さに上限を掛ける（geoframe_client.cpp と同じ理由・同じ手順）
// ---------------------------------------------------------------------------
//
// nlohmann/json には深さ上限のパースオプションが無い。したがってパーサへ渡す前に
// 生バイト列を 1 回走査して数える。文字列リテラル中の '{' '[' を数えないよう
// 文字列とエスケープを追う。これは JSON の検証ではない。
[[nodiscard]] bool jsonDepthWithinLimit(std::string_view text, int limit) noexcept {
  int depth = 0;
  bool inString = false;
  bool escaped = false;

  for (const char ch : text) {
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch == '\\') {
        escaped = true;
      } else if (ch == '"') {
        inString = false;
      }
      continue;
    }
    switch (ch) {
      case '"':
        inString = true;
        break;
      case '{':
      case '[':
        ++depth;
        if (depth > limit) {
          return false;
        }
        break;
      case '}':
      case ']':
        --depth;
        break;
      default:
        break;
    }
  }
  return true;
}

// ★唯一のパース入口。allow_exceptions = false + is_discarded() 検査
//   （例外を制御フローに使わないのが本プロジェクトの規律）。
[[nodiscard]] bool parseJsonSafely(std::string_view body, Json& out) {
  if (body.empty()) {
    return false;
  }
  if (!jsonDepthWithinLimit(body, kHogoMaxJsonDepth)) {
    return false;
  }
  out = Json::parse(body.begin(), body.end(), nullptr, /*allow_exceptions=*/false);
  return !out.is_discarded();
}

// 型に寛容な取り出し（型が違っても落ちない）。
[[nodiscard]] std::string readString(const Json& node, const char* key) {
  const auto it = node.find(key);
  if (it == node.end() || !it->is_string()) {
    return {};
  }
  return it->get<std::string>();
}

[[nodiscard]] std::int64_t readInt(const Json& node, const char* key, std::int64_t fallback) {
  const auto it = node.find(key);
  if (it == node.end() || !it->is_number_integer()) {
    return fallback;
  }
  return it->get<std::int64_t>();
}

// ★キーが無い / 型が違う場合は fallback。上流 ArduinoJson の `| default` と同じ意味。
[[nodiscard]] bool readBool(const Json& node, const char* key, bool fallback) {
  const auto it = node.find(key);
  if (it == node.end() || !it->is_boolean()) {
    return fallback;
  }
  return it->get<bool>();
}

[[nodiscard]] char lowerAscii(char ch) noexcept {
  return static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
}

[[nodiscard]] bool equalsIgnoreCase(std::string_view a, std::string_view b) noexcept {
  if (a.size() != b.size()) {
    return false;
  }
  for (std::size_t i = 0; i < a.size(); ++i) {
    if (lowerAscii(a[i]) != lowerAscii(b[i])) {
      return false;
    }
  }
  return true;
}

[[nodiscard]] bool startsWithIgnoreCase(std::string_view text, std::string_view prefix) noexcept {
  return text.size() >= prefix.size() && equalsIgnoreCase(text.substr(0, prefix.size()), prefix);
}

}  // namespace

// ---------------------------------------------------------------------------
// URL / ボディの組み立て
// ---------------------------------------------------------------------------

const char* toString(HogoStatus status) noexcept {
  // ★default 節を足さないこと（-Wswitch で新しい値に気づくため）。
  switch (status) {
    case HogoStatus::Ok:
      return "Ok";
    case HogoStatus::TransportFailed:
      return "TransportFailed";
    case HogoStatus::HttpFailed:
      return "HttpFailed";
    case HogoStatus::ParseFailed:
      return "ParseFailed";
  }
  return "?";
}

std::string hogoConfigUrl(std::string_view baseUrl, std::string_view deviceId) {
  // 出典: server_sync.cpp:221。★正規化しない素朴な連結。
  std::string url;
  url.reserve(baseUrl.size() + deviceId.size() + 20);
  url.append(baseUrl);
  url.append("/api/agent/");
  url.append(deviceId);
  url.append("/config");
  return url;
}

std::string hogoRecordsUrl(std::string_view baseUrl, std::string_view deviceId) {
  // 出典: server_sync.cpp:399。
  std::string url;
  url.reserve(baseUrl.size() + deviceId.size() + 21);
  url.append(baseUrl);
  url.append("/api/agent/");
  url.append(deviceId);
  url.append("/records");
  return url;
}

std::string hogoDeregisterAckUrl(std::string_view baseUrl, std::string_view deviceId) {
  // 出典: server_sync.cpp:469。
  std::string url;
  url.reserve(baseUrl.size() + deviceId.size() + 28);
  url.append(baseUrl);
  url.append("/api/agent/");
  url.append(deviceId);
  url.append("/deregister-ack");
  return url;
}

std::string hogoRecordBody(core::CareItem item, core::WallTime recordedAt) {
  // 出典: server_sync.cpp:415-416。★配列ではなく単発 1 件。
  std::string body = "{\"slot\":";
  body += std::to_string(static_cast<int>(item));
  body += ",\"recordedAt\":";
  body += std::to_string(recordedAt.epochSeconds);
  body += "}";
  return body;
}

bool hogoIsHttpsUrl(std::string_view baseUrl) noexcept {
  return startsWithIgnoreCase(baseUrl, "https://");
}

std::string hogoValidateBaseUrl(std::string_view baseUrl) {
  if (baseUrl.empty()) {
    return "baseUrl が空（sync は無効になる）";
  }
  if (!startsWithIgnoreCase(baseUrl, "http://") && !hogoIsHttpsUrl(baseUrl)) {
    return "baseUrl のスキームが http:// でも https:// でもない";
  }
  // ★★ 上流と挙動を変えている点（HOGO_CONTRACT.md の該当表を参照）★★
  //   上流は http:// を「開発・復旧用の平文フォールバック」として許可し、
  //   X-Device-Secret を平文で送る（server_sync.cpp:227 / net_client.h）。
  //   こちらは許可しない。理由:
  //     * X-Device-Secret は失効させる手段がデバイス再登録しか無い長期資格情報で、
  //       LAN 上の盗聴者に渡ると記録の偽装（ダッシュボードへの書き込み）ができる
  //     * libcurl は CURLOPT_PROXY を設定していないため http_proxy 環境変数を
  //       尊重する。平文だとプロキシにもそのまま見える
  //   ★黙って 401 にせず、設定時点で理由を出せるようにここで弾く
  //     （addCommonHeaders 側の gate は、この検証を通さない配線に対する二段目）。
  if (!hogoIsHttpsUrl(baseUrl)) {
    return "baseUrl が平文 http://（X-Device-Secret を平文で流さない。https:// を使うこと）";
  }
  if (baseUrl.back() == '/') {
    // ★黙って直さない。素朴連結なので "//api/agent/..." になり 404 になる。
    return "baseUrl の末尾がスラッシュ（連結すると //api/agent/... になる）";
  }
  return {};
}

// ---------------------------------------------------------------------------
// パーサ
// ---------------------------------------------------------------------------

bool parseHogoConfigJson(std::string_view body, core::HogoConfigSnapshot& out) {
  // ★毎回まっさらにする。ここが「キー省略 = false へリセット」の土台である。
  //   前回の値を残す実装にすると、Web 側で要求を取り消しても端末が追従しない。
  out = core::HogoConfigSnapshot{};

  Json doc;
  if (!parseJsonSafely(body, doc) || !doc.is_object()) {
    return false;
  }

  // リモート初期化（deregister）。★items の検証より前に取り込む。
  //   items が欠けた応答でも要求を取りこぼさないため（server_sync.cpp:287-298）。
  out.deregisterRequested = readBool(doc, "deregister", false);

  const auto items = doc.find("items");
  if (items == doc.end() || !items->is_array()) {
    // 上流も items が無ければ失敗扱い（server_sync.cpp:300-304）。
    return false;
  }

  bool sawSlot = false;
  std::size_t maxSlot = 0;
  for (const Json& item : *items) {
    if (!item.is_object()) {
      continue;
    }
    const std::int64_t slot = readInt(item, "slot", -1);
    if (slot < 0 || slot >= static_cast<std::int64_t>(core::constants::kCareItemCount)) {
      continue;  // 想定外スロットは無視（server_sync.cpp:313-315）。
    }
    const auto index = static_cast<std::size_t>(slot);
    sawSlot = true;
    maxSlot = std::max(maxSlot, index);

    core::CareSlotConfig& target = out.slots[index];
    target.present = true;
    // ★18 バイトへ丸める。UTF-8 の境界を割らない（care_model.hpp の該当節）。
    target.label = core::truncateLabelUtf8(readString(item, "label"));
    target.lastEpoch = core::WallTime{readInt(item, "lastEpoch", 0)};

    // status は 0-3。★範囲外はクラッシュさせず None へ倒す（server_sync.cpp:325-328）。
    const std::int64_t statusRaw = readInt(item, "status", 0);
    target.status = (statusRaw >= 0 && statusRaw <= 3)
                        ? static_cast<core::ScheduleStatus>(statusRaw)
                        : core::ScheduleStatus::None;

    // ★フィールドが無い旧サーバーでは安全側の true（server_sync.cpp:330-333）。
    target.shouldLight = readBool(item, "shouldLight", true);
  }

  // 有効スロット数 = 見えた最大 slot + 1（server_sync.cpp:336-338）。
  out.activeSlots = sawSlot ? maxSlot + 1 : 0;

  // ファームウェアオファー。★キーが無ければ present は false のまま
  //   （out を初期化済みなので残留しない。server_sync.cpp:345-365）。
  const auto firmware = doc.find("firmware");
  if (firmware != doc.end() && firmware->is_object()) {
    const std::string version = readString(*firmware, "version");
    const std::string sha256 = readString(*firmware, "sha256");
    const std::string sig = readString(*firmware, "sig");
    // ★3 つすべて非空のときだけ掴む。sig が空 = 未署名オファーは受け付けない。
    if (!version.empty() && !sha256.empty() && !sig.empty()) {
      out.firmware.present = true;
      out.firmware.version = version;
      out.firmware.sha256 = sha256;
      out.firmware.sig = sig;
      const std::int64_t attempt = readInt(*firmware, "attempt", 0);
      out.firmware.attempt =
          (attempt > 0 && attempt <= 0xFFFFFFFF) ? static_cast<std::uint32_t>(attempt) : 0U;
    }
  }

  return true;
}

// ---------------------------------------------------------------------------
// ログの秘匿
// ---------------------------------------------------------------------------

bool isSensitiveHeader(std::string_view name) noexcept {
  return equalsIgnoreCase(name, "X-Device-Secret") ||
         equalsIgnoreCase(name, "CF-Access-Client-Secret") ||
         equalsIgnoreCase(name, "CF-Access-Client-Id");
}

std::string redactForLog(const core::HttpRequest& request) {
  std::string out;
  out += (request.method == core::HttpMethod::Post) ? "POST " : "GET ";
  out += request.url;
  for (const core::HttpHeader& header : request.headers) {
    out += " [";
    out += header.name;
    out += ": ";
    out += isSensitiveHeader(header.name) ? std::string{kRedactedValue} : header.value;
    out += "]";
  }
  if (!request.body.empty()) {
    // ★記録のボディは slot と epoch しか含まない。資格情報は入らない。
    out += " body=";
    out += request.body;
  }
  return out;
}

// ---------------------------------------------------------------------------
// CA バンドル（★経路のみ。現時点で誰も消費しない。ヘッダの申し送りを読むこと）
// ---------------------------------------------------------------------------

CaBundleLoad loadCaBundleFromEnv(const char* envName) {
  CaBundleLoad load;
  if (envName == nullptr) {
    load.message = "環境変数名が null";
    return load;
  }

  // NOLINTNEXTLINE(concurrency-mt-unsafe) — 単一スレッドの初期化時にしか呼ばない。
  const char* value = std::getenv(envName);
  if (value == nullptr || value[0] == '\0') {
    return load;  // present = false。未設定は正常系（システムのトラストストアを使う）。
  }
  load.present = true;

  const std::string path{value};
  // ★インラインの PEM は受け付けない（ヘッダの節）。パスとしてだけ扱う。
  std::ifstream file(path, std::ios::binary);
  if (!file) {
    load.message = "CA バンドルを開けない: " + path;
    return load;
  }

  std::ostringstream buffer;
  buffer << file.rdbuf();
  std::string pem = buffer.str();
  if (pem.find("-----BEGIN CERTIFICATE-----") == std::string::npos) {
    // ★黙って通さない。通すと TLS 時に「証明書検証失敗」としか出ず切り分けできない。
    load.message = "PEM に見えない（BEGIN CERTIFICATE が無い）: " + path;
    return load;
  }

  load.ok = true;
  load.pem = std::move(pem);
  return load;
}

// ---------------------------------------------------------------------------
// HogoClient
// ---------------------------------------------------------------------------

HogoClient::HogoClient(core::IHttpClient& http, HogoClientConfig config)
    : http_(&http), config_(std::move(config)) {
  // ★baseUrl を正規化しないこと（ヘッダ冒頭の節）。検出は hogoValidateBaseUrl()。
}

bool HogoClient::configured() const noexcept {
  // 上流 pollConfig / drainOneRecord の先頭ガード（server_sync.cpp:213, 372）。
  return !config_.baseUrl.empty() && !config_.deviceId.empty();
}

void HogoClient::addCommonHeaders(core::HttpRequest& request) const {
  // ★★ 平文 http では **デバイスの資格情報も** 送らない ★★
  //   上流は送る（server_sync.cpp:227）。こちらが変えている点であり、理由は
  //   hogoValidateBaseUrl() のコメントと HOGO_CONTRACT.md に書いてある。
  //
  //   ★これは hogoValidateBaseUrl() の **二段目**である。検証は自由関数であって
  //     コンストラクタが呼ぶわけではないので、検証を忘れた配線でも secret が
  //     平文で出ないよう、送信の直前でも掛けておく。
  //   ★http:// で使うと当然サーバは 401 を返す。それが正しい（黙って通すより、
  //     設定の誤りとして表に出るほうがよい）。
  if (hogoIsHttpsUrl(config_.baseUrl)) {
    request.headers.push_back(core::HttpHeader{"X-Device-Secret", config_.deviceSecret});
  }

  // ★平文 http では Cloudflare トークンを送らない（server_sync.cpp:241-244）。
  if (!config_.cfAccessClientId.empty() && !config_.cfAccessClientSecret.empty() &&
      hogoIsHttpsUrl(config_.baseUrl)) {
    request.headers.push_back(
        core::HttpHeader{"CF-Access-Client-Id", config_.cfAccessClientId});
    request.headers.push_back(
        core::HttpHeader{"CF-Access-Client-Secret", config_.cfAccessClientSecret});
  }
}

core::HttpRequest HogoClient::buildConfigRequest() const {
  core::HttpRequest request;
  request.method = core::HttpMethod::Get;
  request.url = hogoConfigUrl(config_.baseUrl, config_.deviceId);
  request.timeout = config_.timeout;
  request.maxResponseBytes = config_.maxResponseBytes;
  addCommonHeaders(request);

  if (!config_.fwVersion.empty()) {
    request.headers.push_back(core::HttpHeader{"X-FW-Version", config_.fwVersion});
  }
  if (!config_.failedFwVersion.empty()) {
    // 形式は "<version>#<attempt>"（server_sync.cpp:236）。
    request.headers.push_back(core::HttpHeader{
        "X-FW-Failed",
        config_.failedFwVersion + "#" + std::to_string(config_.failedFwAttempt)});
  }
  request.headers.push_back(core::HttpHeader{"Accept", "application/json"});
  return request;
}

core::HttpRequest HogoClient::buildRecordRequest(core::CareItem item,
                                                 core::WallTime recordedAt) const {
  core::HttpRequest request;
  request.method = core::HttpMethod::Post;
  request.url = hogoRecordsUrl(config_.baseUrl, config_.deviceId);
  request.timeout = config_.timeout;
  request.maxResponseBytes = config_.maxResponseBytes;
  request.body = hogoRecordBody(item, recordedAt);
  request.contentType = "application/json";
  addCommonHeaders(request);
  request.headers.push_back(core::HttpHeader{"Content-Type", "application/json"});
  return request;
}

namespace {

// ステータスで分岐してから parse する（core/http.hpp §8.2）。
// ★secret をここへ入れないこと。
[[nodiscard]] bool classify(const core::HttpResponse& response, HogoResult& result,
                            std::string& body) {
  result.httpStatus = response.status;
  result.error = response.error;
  result.contentType = response.contentType;
  result.bodyBytes = response.body.size();

  const std::optional<std::string_view> success = core::successBody(response);
  if (success) {
    body.assign(*success);
    result.status = HogoStatus::Ok;
    return true;
  }

  if (response.error != core::HttpError::None) {
    result.status = HogoStatus::TransportFailed;
    result.message = std::string("通信に失敗: ") + core::toString(response.error);
    // ★トランスポート失敗時の body は **サーバからの本文ではなく実装が入れた
    //   診断文**である（CurlHttpClient は CURLOPT_ERRORBUFFER をそこへ入れる）。
    //   受信バイト数としては 0 が正しい。geoframe_client.cpp の get() と同じ扱い。
    //   ★ここでは message へ連結しない。curl の診断は URL を含むことがあり、
    //     この層の message は「secret を含めない」ことを契約テストが検査している。
    //     URL に secret は入らないが、診断文の中身は libcurl 任せなので広げない。
    result.bodyBytes = 0;
  } else {
    result.status = HogoStatus::HttpFailed;
    result.message = "HTTP " + std::to_string(response.status) + "（本文は " +
                     (response.contentType.empty() ? "不明" : response.contentType) +
                     "。JSON とは限らない）";
  }
  return false;
}

}  // namespace

HogoResult HogoClient::pollConfig(core::HogoConfigSnapshot& out) {
  HogoResult result;
  if (!configured()) {
    result.status = HogoStatus::TransportFailed;
    result.error = core::HttpError::Cancelled;
    result.message = "baseUrl / deviceId が未設定（sync 無効）";
    return result;
  }

  const core::HttpResponse response = http_->fetch(buildConfigRequest());

  std::string body;
  if (!classify(response, result, body)) {
    return result;
  }

  if (!parseHogoConfigJson(body, out)) {
    result.status = HogoStatus::ParseFailed;
    result.message = "config の JSON を解釈できない（items が無い / JSON でない）";
  }
  return result;
}

HogoResult HogoClient::postRecord(core::CareItem item, core::WallTime recordedAt) {
  HogoResult result;
  if (!configured()) {
    result.status = HogoStatus::TransportFailed;
    result.error = core::HttpError::Cancelled;
    result.message = "baseUrl / deviceId が未設定（sync 無効）";
    return result;
  }

  const core::HttpResponse response = http_->fetch(buildRecordRequest(item, recordedAt));

  // ★成功は 204 No Content（空ボディ）。上流は「念のため 2xx を成功とみなす」
  //   としているのでそれに合わせる（server_sync.cpp:421-425）。
  std::string body;
  static_cast<void>(classify(response, result, body));
  return result;
}

// ---------------------------------------------------------------------------
// HogoSync
// ---------------------------------------------------------------------------

HogoSyncOutcome HogoSync::update(core::SteadyTime now, core::SyncReadiness ready,
                                 core::Millis requestDuration) {
  HogoSyncOutcome outcome;
  outcome.action = policy_->nextAction(now, ready);

  // ★None なら HTTP を一切投げない。ここで「ついでに poll する」を足さないこと。
  if (outcome.action.none()) {
    return outcome;
  }

  const core::SteadyTime completedAt = now + requestDuration;

  // ★default 節を足さないこと（-Wswitch）。
  switch (outcome.action.kind) {
    case core::SyncActionKind::None:
      return outcome;

    case core::SyncActionKind::PollConfig: {
      outcome.result = client_->pollConfig(outcome.config);
      outcome.executed = true;
      outcome.haveConfig = outcome.result.ok();
      policy_->applyResult(outcome.action, outcome.result.ok(), completedAt);
      return outcome;
    }

    case core::SyncActionKind::SendRecord: {
      outcome.result = client_->postRecord(outcome.action.item, outcome.action.recordedAt);
      outcome.executed = true;
      policy_->applyResult(outcome.action, outcome.result.ok(), completedAt);
      return outcome;
    }
  }
  return outcome;
}

}  // namespace ambient::transport
