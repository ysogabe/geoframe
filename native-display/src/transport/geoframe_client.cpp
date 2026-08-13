#include "ambient/transport/geoframe_client.hpp"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <cstddef>
#include <string>
#include <utility>

#include "ambient/core/model/parse.hpp"

namespace ambient::transport {

// ---------------------------------------------------------------------------
// ★件数上限の不変条件を機械的に検査する（geoframe_client.hpp の該当節）
// ---------------------------------------------------------------------------
//
//   上限件数 × sizeof(要素) <= kDefaultMaxResponseBytes  … 増幅率 < 1
//
// 数字を覚え込ませないためのもの。**要素が太ったら / 上限を上げたらここで落ちる**
// のが正しい状態である。落ちたら上限を下げること。
//
// ★この assert が評価するのは **Mac のビルドの sizeof だけ**である。移植性ゲート
//   (tools/check-esp-portability.sh) は src/core/ しかコンパイルしないので、この
//   翻訳単位は riscv32 のツールチェーンを通らない。riscv32 側は手計算で確認した:
//   libstdc++ の std::string は 32 バイトなので PhotoItem は約 288 バイト、
//   8192 件で約 2.36MB。4MB の上限を下回る。
static_assert(kMaxPhotoItems * sizeof(core::PhotoItem) <= kDefaultMaxResponseBytes,
              "写真の件数上限 × 1 件あたりのサイズが応答上限を超えている");
static_assert(kMaxQuakeItems * sizeof(core::QuakeItem) <= kDefaultMaxResponseBytes,
              "地震の件数上限 × 1 件あたりのサイズが応答上限を超えている");
static_assert(kMaxWeeklyDays * sizeof(core::WeeklyDay) <= kDefaultMaxResponseBytes,
              "週間予報の件数上限 × 1 件あたりのサイズが応答上限を超えている");
static_assert(kMaxRoomEnvItems * sizeof(core::RoomEnvItem) <= kDefaultMaxResponseBytes,
              "室内環境の件数上限 × 1 件あたりのサイズが応答上限を超えている");

namespace {

using Json = nlohmann::json;

// ---------------------------------------------------------------------------
// ★ネストの深さに上限を掛ける
// ---------------------------------------------------------------------------
//
// nlohmann/json には「深さ上限」のパースオプションが無い（parser_callback は
// 深さを受け取るが、フィルタであって中断機構ではなく、しかも再帰は先に進む）。
// したがって **パーサへ渡す前に生バイト列を走査して** 深さを数える。
//
// 文字列リテラルの中の '{' '[' を数えないよう、文字列とエスケープを追う。
// これは JSON の検証ではない（それは nlohmann の仕事）。「深すぎる入力を
// パーサに渡さない」ためだけの、走査 1 回の前段である。
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

// ★唯一のパース入口。ここ以外で nlohmann::json::parse を呼ばないこと。
//   allow_exceptions = false で呼び、is_discarded() を検査する
//   （例外を制御フローに使わないのが本プロジェクトの規律）。
[[nodiscard]] bool parseJsonSafely(std::string_view body, Json& out) {
  if (body.empty()) {
    return false;
  }
  if (!jsonDepthWithinLimit(body, kMaxJsonDepth)) {
    return false;
  }
  out = Json::parse(body.begin(), body.end(), nullptr, /*allow_exceptions=*/false);
  return !out.is_discarded();
}

// ---------------------------------------------------------------------------
// 型に寛容な取り出し（型が違っても落ちない）
// ---------------------------------------------------------------------------

[[nodiscard]] std::string readString(const Json& node, const char* key) {
  const auto it = node.find(key);
  if (it == node.end()) {
    return {};
  }
  if (it->is_string()) {
    return it->get<std::string>();
  }
  // ★数値が文字列で来たり、その逆だったりする箇所がある
  //   （例: photos の id は数値、roomenv の value は文字列）。
  //   数値だった場合は 10 進表記へ落として拾う。
  if (it->is_number_integer()) {
    return std::to_string(it->get<std::int64_t>());
  }
  if (it->is_number_unsigned()) {
    return std::to_string(it->get<std::uint64_t>());
  }
  return {};
}

[[nodiscard]] double readDouble(const Json& node, const char* key, double fallback) {
  const auto it = node.find(key);
  if (it == node.end()) {
    return fallback;
  }
  if (it->is_number()) {
    return it->get<double>();
  }
  if (it->is_string()) {
    const std::optional<int> parsed = core::parseIntPrefix(it->get<std::string>());
    return parsed ? static_cast<double>(*parsed) : fallback;
  }
  return fallback;
}

[[nodiscard]] bool readBool(const Json& node, const char* key, bool fallback) {
  const auto it = node.find(key);
  if (it == node.end() || !it->is_boolean()) {
    return fallback;
  }
  return it->get<bool>();
}

[[nodiscard]] std::int32_t readInt32(const Json& node, const char* key,
                                     std::int32_t fallback) {
  const auto it = node.find(key);
  if (it == node.end()) {
    return fallback;
  }
  if (it->is_number_integer() || it->is_number_unsigned()) {
    return static_cast<std::int32_t>(it->get<std::int64_t>());
  }
  if (it->is_string()) {
    const std::optional<int> parsed = core::parseIntPrefix(it->get<std::string>());
    return parsed ? static_cast<std::int32_t>(*parsed) : fallback;
  }
  return fallback;
}

// null / 欠落 / 型違いはすべて nullopt。
// ★weekly の pop / temp_min / temp_max は **実測で null になる**（翌日分）。
//   0 に潰さないこと（「降水確率 0%」と「予報が無い」は別の情報）。
[[nodiscard]] std::optional<int> readOptionalInt(const Json& node, const char* key) {
  const auto it = node.find(key);
  if (it == node.end() || it->is_null()) {
    return std::nullopt;
  }
  if (it->is_number_integer() || it->is_number_unsigned()) {
    return static_cast<int>(it->get<std::int64_t>());
  }
  if (it->is_number_float()) {
    return static_cast<int>(it->get<double>());
  }
  if (it->is_string()) {
    return core::parseIntPrefix(it->get<std::string>());
  }
  return std::nullopt;
}

}  // namespace

// ---------------------------------------------------------------------------
// 診断
// ---------------------------------------------------------------------------

const char* toString(GeoFrameStatus status) noexcept {
  switch (status) {
    case GeoFrameStatus::Ok:
      return "Ok";
    case GeoFrameStatus::TransportFailed:
      return "TransportFailed";
    case GeoFrameStatus::HttpFailed:
      return "HttpFailed";
    case GeoFrameStatus::ParseFailed:
      return "ParseFailed";
    case GeoFrameStatus::NotConfigured:
      return "NotConfigured";
  }
  return "?";
}

// ---------------------------------------------------------------------------
// /api/photos ― ★裸の JSON 配列
// ---------------------------------------------------------------------------

bool parsePhotosJson(std::string_view body, core::PhotoManifest& out) {
  Json root;
  if (!parseJsonSafely(body, root)) {
    return false;
  }
  // ★ここが最大の落とし穴。`{"items": [...]}` を期待すると 0 件で「成功」する。
  //   配列でなければ **失敗として扱う**（静かに空の manifest を返さない）。
  if (!root.is_array()) {
    return false;
  }

  core::PhotoManifest manifest;
  // ★root.size() をそのまま渡さないこと（geoframe_client.hpp の「配列の件数に
  //   上限」節）。フィルタは下のループの中、つまり **この確保より後ろ**なので、
  //   1 件も採用しない入力でも root.size() ぶんの確保が先に走ってしまう。
  manifest.items.reserve(std::min(root.size(), kMaxPhotoItems));

  for (const Json& entry : root) {
    // ★採用件数にも上限を掛ける。reserve だけ抑えても push_back の繰り返しで
    //   結局増えるため。continue ではなく break にして走査そのものも打ち切る
    //   （CPU も入力サイズに比例させない）。
    if (manifest.items.size() >= kMaxPhotoItems) {
      break;
    }
    if (!entry.is_object()) {
      continue;  // 壊れた 1 件で全体を捨てない
    }

    core::PhotoItem item;

    // ★実測: id は **整数**。PhotoId は std::string なので 10 進表記へ落とす。
    item.id = readString(entry, "id");
    if (item.id.empty()) {
      continue;  // ID の無い写真は同定できないので捨てる
    }

    // ★キーは `file`。先頭スラッシュ **無し** の相対パス。
    item.file = core::PhotoRelPath{readString(entry, "file")};
    if (item.file.value.empty()) {
      continue;
    }

    // ★キーは `time`。`takenAt` ではない。"YYYY-MM-DD HH:MM"。
    item.time = readString(entry, "time");
    item.fetchedAt = readString(entry, "fetched_at");
    item.trip = readInt32(entry, "trip", 0);
    item.fav = readBool(entry, "fav", false);
    const std::string qualityRole = readString(entry, "quality_role");
    item.qualityRecommended = qualityRole == "recommended";
    item.qualityAlternate = qualityRole == "alternate";
    item.qualityScore = static_cast<float>(readDouble(entry, "quality_score", 0.0));
    item.lat = readDouble(entry, "lat", 0.0);
    item.lon = readDouble(entry, "lon", 0.0);
    item.title = readString(entry, "title");
    item.place = readString(entry, "place");
    item.pref = readString(entry, "pref");
    item.city = readString(entry, "city");
    item.inferred = readBool(entry, "inferred", false);

    manifest.items.push_back(std::move(item));
  }

  // ★呼び忘れても動くが indexOf が線形探索へ落ちる（photo.hpp）。必ず呼ぶ。
  manifest.rebuildIndex();
  out = std::move(manifest);
  return true;
}

// ---------------------------------------------------------------------------
// /api/env ― ★トップレベルに ok は無い
// ---------------------------------------------------------------------------

bool parseEnvJson(std::string_view body, core::EnvSnapshot& out) {
  Json root;
  if (!parseJsonSafely(body, root)) {
    return false;
  }
  if (!root.is_object()) {
    return false;
  }

  core::EnvSnapshot snapshot;

  if (const auto quakes = root.find("quakes"); quakes != root.end() && quakes->is_array()) {
    // ★上限つきの確保 + 採用件数の上限（photos と同じ理由。ヘッダの該当節）。
    snapshot.quakes.reserve(std::min(quakes->size(), kMaxQuakeItems));
    for (const Json& entry : *quakes) {
      if (snapshot.quakes.size() >= kMaxQuakeItems) {
        break;
      }
      if (!entry.is_object()) {
        continue;
      }
      core::QuakeItem quake;
      quake.eid = readString(entry, "eid");
      quake.lat = readDouble(entry, "lat", 0.0);
      quake.lon = readDouble(entry, "lon", 0.0);
      // ★mag も maxi も **文字列** で返る。数値化しないこと。
      //   maxi は "5-" / "5+"（震度 5 弱 / 5 強）をとる。
      quake.mag = readString(entry, "mag");
      quake.maxi = readString(entry, "maxi");
      quake.place = readString(entry, "place");
      // ★time は "HH:MM" で日付が落ちている。同定には at を使うこと。
      quake.time = readString(entry, "time");
      quake.at = readString(entry, "at");
      if (quake.eid.empty()) {
        continue;  // 同定できない地震は捨てる
      }
      snapshot.quakes.push_back(std::move(quake));
    }
  }

  // weather は null になりうる（気象庁の取得失敗時）。
  if (const auto weather = root.find("weather");
      weather != root.end() && weather->is_object()) {
    core::WeatherSummary summary;
    summary.area = readString(*weather, "area");
    summary.text = readString(*weather, "text");
    summary.pop = readOptionalInt(*weather, "pop");
    if (const auto temps = weather->find("temps");
        temps != weather->end() && temps->is_array()) {
      // ★実測: temps は **文字列の配列**（["27", "36"]）。
      // ★reserve は無いが上限は要る。無ければ push_back の繰り返しで結局増える。
      for (const Json& value : *temps) {
        if (summary.temps.size() >= kMaxWeatherTemps) {
          break;
        }
        if (value.is_number_integer() || value.is_number_unsigned()) {
          summary.temps.push_back(static_cast<int>(value.get<std::int64_t>()));
        } else if (value.is_number_float()) {
          summary.temps.push_back(static_cast<int>(value.get<double>()));
        } else if (value.is_string()) {
          if (const std::optional<int> parsed =
                  core::parseIntPrefix(value.get<std::string>())) {
            summary.temps.push_back(*parsed);
          }
        }
      }
    }
    snapshot.weather = std::move(summary);
  }

  if (const auto weekly = root.find("weekly"); weekly != root.end() && weekly->is_array()) {
    snapshot.weekly.reserve(std::min(weekly->size(), kMaxWeeklyDays));
    for (const Json& entry : *weekly) {
      if (snapshot.weekly.size() >= kMaxWeeklyDays) {
        break;
      }
      if (!entry.is_object()) {
        continue;
      }
      core::WeeklyDay day;
      day.date = readString(entry, "date");
      day.code = readString(entry, "code");
      // ★pop / temp_min / temp_max は実測で null をとる（翌日分）。
      day.pop = readOptionalInt(entry, "pop");
      day.tempMax = readOptionalInt(entry, "temp_max");
      day.tempMin = readOptionalInt(entry, "temp_min");
      snapshot.weekly.push_back(std::move(day));
    }
  }

  out = std::move(snapshot);
  return true;
}

// ---------------------------------------------------------------------------
// /api/roomenv ― ★ok:false は「HA 未設定」であって障害ではない
// ---------------------------------------------------------------------------

bool parseRoomEnvJson(std::string_view body, core::RoomEnvSnapshot& out) {
  Json root;
  if (!parseJsonSafely(body, root)) {
    return false;
  }
  if (!root.is_object()) {
    return false;
  }

  core::RoomEnvSnapshot snapshot;

  // ★ここだけ ok がある（env / stats / presence / photos には無い）。
  const bool ok = readBool(root, "ok", false);
  if (!ok) {
    snapshot.availability = core::RoomEnvAvailability::NotConfigured;
    out = std::move(snapshot);
    return true;  // JSON としては読めている。障害ではない。
  }

  snapshot.availability = core::RoomEnvAvailability::Available;

  if (const auto items = root.find("items"); items != root.end() && items->is_array()) {
    snapshot.items.reserve(std::min(items->size(), kMaxRoomEnvItems));
    for (const Json& entry : *items) {
      if (snapshot.items.size() >= kMaxRoomEnvItems) {
        break;
      }
      if (!entry.is_object()) {
        continue;
      }
      core::RoomEnvItem item;
      item.entity = readString(entry, "entity");
      // ★value は常に文字列。"23.83" も "9454" も文字列で来る。
      item.value = readString(entry, "value");
      item.unit = readString(entry, "unit");
      // JSON のキーは `class`（C++ の予約語なのでフィールド名を変えてある）。
      item.entityClass = readString(entry, "class");
      item.name = readString(entry, "name");
      if (item.entity.empty()) {
        continue;
      }
      snapshot.items.push_back(std::move(item));
    }
  }

  out = std::move(snapshot);
  return true;
}

// ---------------------------------------------------------------------------
// /api/presence ― ★night は 3 値
// ---------------------------------------------------------------------------

bool parsePresenceJson(std::string_view body, core::PresenceSnapshot& out) {
  Json root;
  if (!parseJsonSafely(body, root)) {
    return false;
  }
  if (!root.is_object()) {
    return false;
  }

  core::PresenceSnapshot snapshot;
  // ★true / false / null の 3 値。null は「判定できない」であって false ではない。
  //   欠落と型違いも nullopt に倒す（false に倒すと夜に画面が煌々と光る）。
  if (const auto night = root.find("night");
      night != root.end() && night->is_boolean()) {
    snapshot.night = night->get<bool>();
  }
  out = snapshot;
  return true;
}

// ---------------------------------------------------------------------------
// クライアント
// ---------------------------------------------------------------------------

GeoFrameClient::GeoFrameClient(core::IHttpClient& http, GeoFrameConfig config)
    : http_(&http), config_(std::move(config)) {
  // base の末尾スラッシュは joinUrl 側で正規化されるが、ログや診断で
  // "http://host:8080//api/..." が見えると紛らわしいのでここでも落としておく。
  while (config_.baseUrl.size() > 1 && config_.baseUrl.back() == '/') {
    config_.baseUrl.pop_back();
  }
}

GeoFrameResult GeoFrameClient::get(std::string_view path, std::string& body) {
  GeoFrameResult result;
  body.clear();

  core::HttpRequest request;
  request.method = core::HttpMethod::Get;
  request.url = config_.baseUrl;
  request.url.append(path);
  request.timeout = config_.timeout;
  request.maxResponseBytes = config_.maxResponseBytes;
  request.headers.push_back(core::HttpHeader{"Accept", "application/json"});

  const core::HttpResponse response = http_->fetch(request);

  result.httpStatus = response.status;
  result.error = response.error;
  result.contentType = response.contentType;
  result.bodyBytes = response.body.size();

  // ★★ ステータスで分岐してから parse する ★★
  //   403 / 404 の本文は HTML である。ここを通さずにパーサへ渡すと、
  //   最も診断が必要な場面で「JSON parse エラー」しか出なくなる。
  const std::optional<std::string_view> success = core::successBody(response);
  if (!success) {
    if (response.error != core::HttpError::None) {
      result.status = GeoFrameStatus::TransportFailed;
      result.message = std::string("通信に失敗: ") + core::toString(response.error);
      // ★実装が入れた診断を拾う（CurlHttpClient は CURLOPT_ERRORBUFFER をここへ
      //   入れる）。HttpError は 7 値しかなく、具体的な理由はここにしか残らない。
      //   ★error != None なので successBody() は nullopt。この body がパーサへ
      //     渡ることは無い（core/http.hpp の「パースの門」）。
      if (!response.body.empty()) {
        result.message += "（" + response.body + "）";
      }
      // 受信バイト数としては 0 が正しい（body に入っているのは診断文であって
      // サーバからの本文ではない）。
      result.bodyBytes = 0;
    } else {
      result.status = GeoFrameStatus::HttpFailed;
      result.message = "HTTP " + std::to_string(response.status) +
                       "（本文は " +
                       (response.contentType.empty() ? "不明" : response.contentType) +
                       "。JSON とは限らない）";
    }
    return result;
  }

  body.assign(*success);
  result.status = GeoFrameStatus::Ok;
  return result;
}

GeoFrameResult GeoFrameClient::fetchPhotos(core::PhotoManifest& out) {
  std::string body;
  GeoFrameResult result = get("/api/photos", body);
  if (!result.ok()) {
    return result;
  }
  if (!parsePhotosJson(body, out)) {
    result.status = GeoFrameStatus::ParseFailed;
    result.message = "/api/photos を配列として読めませんでした（裸の配列のはずです）";
  }
  return result;
}

GeoFrameResult GeoFrameClient::fetchEnv(core::EnvSnapshot& out) {
  std::string body;
  GeoFrameResult result = get("/api/env", body);
  if (!result.ok()) {
    return result;
  }
  if (!parseEnvJson(body, out)) {
    result.status = GeoFrameStatus::ParseFailed;
    result.message = "/api/env をオブジェクトとして読めませんでした";
  }
  return result;
}

GeoFrameResult GeoFrameClient::fetchRoomEnv(core::RoomEnvSnapshot& out) {
  std::string body;
  GeoFrameResult result = get("/api/roomenv", body);
  if (!result.ok()) {
    return result;
  }
  if (!parseRoomEnvJson(body, out)) {
    result.status = GeoFrameStatus::ParseFailed;
    result.message = "/api/roomenv をオブジェクトとして読めませんでした";
    return result;
  }
  if (out.availability == core::RoomEnvAvailability::NotConfigured) {
    // ★障害ではない。Degraded にも STALE にもしないための専用の値。
    result.status = GeoFrameStatus::NotConfigured;
    result.message = "Home Assistant が未設定です（機能なし。障害ではありません）";
  }
  return result;
}

GeoFrameResult GeoFrameClient::fetchPresence(core::PresenceSnapshot& out) {
  std::string body;
  GeoFrameResult result = get("/api/presence", body);
  if (!result.ok()) {
    return result;
  }
  if (!parsePresenceJson(body, out)) {
    result.status = GeoFrameStatus::ParseFailed;
    result.message = "/api/presence をオブジェクトとして読めませんでした";
  }
  return result;
}

std::string GeoFrameClient::photoUrl(const core::PhotoRelPath& file) const {
  // ★PhotoRelPath 用の joinUrl を使うこと（AdminAbsPath 用と非対称）。
  return core::joinUrl(config_.baseUrl, file);
}

}  // namespace ambient::transport
