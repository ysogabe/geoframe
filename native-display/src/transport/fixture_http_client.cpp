// fixture_http_client.cpp — FixtureHttpClient / FakeHttpClient の実装
//
// ★これを transport 層に置く理由:
//   ファイルを読むため std::filesystem が要る。ambient_core は std::filesystem を
//   使えない（ESP32 に存在しない）ので core には置けない。
//
// ★fixture を IHttpClient の実装にする理由:
//   Phase 0 の fixture 駆動と、Phase 1 以降の実通信が **同一のクライアントコードを
//   通る**ようになり、「fixture でだけ動く経路」が生まれない。
//
// ★<filesystem> はこの翻訳単位に閉じ込めてある。公開ヘッダには出さないこと
//   （Tab5 では esp_http_client ベースの別実装に差し替わるため）。
//
// ★例外を制御フローに使わない（docs/PORTABILITY.md の規律）。
//   ファイルが読めない・キーが無いといった失敗は、すべて戻り値の
//   status = 0 / error = Unknown で表現する。

#include "ambient/transport/fixture_http_client.hpp"

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <utility>

namespace ambient::transport {
namespace {

// ----------------------------------------------------------------------------
// assets/fixtures/index.json の写し
// ----------------------------------------------------------------------------
//
// ★なぜ index.json をパースしないのか
//
//   この層に JSON パーサを持ち込まないため。third_party/ に vendor されているのは
//   doctest だけで、JSON ライブラリは無い。新たに vendor すると CMakeLists.txt の
//   変更が要り、並行作業中の他レーンと衝突する。
//
//   代わりに index.json と **同等の対応表**をここに持ち、食い違いは
//   contract_tests の「index.json との同期」テストが検出する
//   （キーの存在・body パスの存在・エントリ数の一致を突き合わせている）。
//
// ★status / contentType は採取時の実測値。こちらで決めた値ではない。
//   errors/ の 3 件が JSON ではなく HTML であることがこの表の要点である。
//
// すべての string_view は文字列リテラル（静的記憶域）を指すので、
// FixtureMeta を値で返してもぶら下がらない。
constexpr FixtureMeta kFixtures[] = {
    {"photos", 200, "application/json; charset=utf-8", "api/photos.json"},
    {"env", 200, "application/json; charset=utf-8", "api/env.json"},
    {"roomenv", 200, "application/json; charset=utf-8", "api/roomenv.json"},
    {"presence", 200, "application/json; charset=utf-8", "api/presence.json"},
    {"presence-night-true", 200, "application/json; charset=utf-8",
     "api/presence-night-true.json"},
    {"presence-night-false", 200, "application/json; charset=utf-8",
     "api/presence-night-false.json"},
    {"presence-night-null", 200, "application/json; charset=utf-8",
     "api/presence-night-null.json"},
    {"stats", 200, "application/json; charset=utf-8", "api/stats.json"},
    {"admin-photos", 200, "application/json; charset=utf-8", "api/admin-photos.json"},
    // ★ここから下は JSON ではない。403 / 404 で HTML が返る実測経路。
    {"err-host-forbidden", 403, "text/html;charset=utf-8", "errors/host-forbidden-403.html"},
    {"err-static-forbidden", 403, "text/html;charset=utf-8", "errors/static-forbidden-403.html"},
    {"err-not-found", 404, "text/html;charset=utf-8", "errors/not-found-404.html"},
    {"quakes-inject", 200, "application/json; charset=utf-8", "quakes-inject.json"},
};

// 表に無いパスへのアクセスはこれになる（実サーバと同じ壊れ方）。
constexpr std::string_view kNotFoundKey = "err-not-found";

// URL からパス部分だけを取り出す。
//   "http://192.0.2.5:8080/api/admin/photos?page=1" -> "/api/admin/photos"
//   "/api/env"                                       -> "/api/env"
// 素朴でよい。fixture の引き当てに使うだけで、URL の正当性検証はしない。
[[nodiscard]] std::string_view pathOfUrl(std::string_view url) noexcept {
  const auto schemeEnd = url.find("://");
  if (schemeEnd != std::string_view::npos) {
    url.remove_prefix(schemeEnd + 3);
    const auto slash = url.find('/');
    if (slash == std::string_view::npos) {
      return std::string_view{"/"};
    }
    url.remove_prefix(slash);
  }
  const auto cut = url.find_first_of("?#");
  if (cut != std::string_view::npos) {
    url = url.substr(0, cut);
  }
  return url;
}

// 例外を投げずにファイル全体を読む。失敗は nullopt。
[[nodiscard]] std::optional<std::string> readWholeFile(const std::filesystem::path& path) {
  std::ifstream in(path, std::ios::binary);
  if (!in.is_open()) {
    return std::nullopt;
  }
  std::string content{std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
  if (in.bad()) {
    return std::nullopt;
  }
  return content;
}

[[nodiscard]] core::HttpResponse transportFailure(core::HttpError error, std::string diagnostic) {
  core::HttpResponse response{};
  response.status = 0;
  response.error = error;
  response.body = std::move(diagnostic);
  return response;
}

}  // namespace

// ============================================================================
// FixtureHttpClient
// ============================================================================

FixtureHttpClient::FixtureHttpClient(std::string fixtureDir)
    : fixtureDir_(std::move(fixtureDir)) {
  // 既定のパス表。実サーバのエンドポイントに対応する。
  pathToKey_.emplace("/api/photos", "photos");
  pathToKey_.emplace("/api/env", "env");
  pathToKey_.emplace("/api/roomenv", "roomenv");
  pathToKey_.emplace("/api/presence", "presence");
  pathToKey_.emplace("/api/stats", "stats");
  pathToKey_.emplace("/api/admin/photos", "admin-photos");
  // 静的ホワイトリスト外の 403（実測で採取した経路）。
  pathToKey_.emplace("/server.py", "err-static-forbidden");
}

std::vector<FixtureMeta> FixtureHttpClient::knownFixtures() {
  return std::vector<FixtureMeta>{std::begin(kFixtures), std::end(kFixtures)};
}

std::optional<FixtureMeta> FixtureHttpClient::metaFor(std::string_view key) noexcept {
  for (const auto& meta : kFixtures) {
    if (meta.key == key) {
      return meta;
    }
  }
  return std::nullopt;
}

core::HttpResponse FixtureHttpClient::load(std::string_view fixtureKey) const {
  const auto meta = metaFor(fixtureKey);
  if (!meta) {
    return transportFailure(core::HttpError::Unknown,
                            "fixture key not found: " + std::string{fixtureKey});
  }

  const std::filesystem::path full =
      std::filesystem::path{fixtureDir_} / std::filesystem::path{std::string{meta->bodyPath}};

  auto body = readWholeFile(full);
  if (!body) {
    return transportFailure(core::HttpError::Unknown, "fixture file unreadable: " + full.string());
  }

  core::HttpResponse response{};
  // ★status / contentType は index.json の実測値。403/404 では HTML が入る。
  response.status = meta->status;
  response.error = core::HttpError::None;
  response.contentType = std::string{meta->contentType};
  response.body = std::move(*body);
  return response;
}

core::HttpResponse FixtureHttpClient::fetch(const core::HttpRequest& request) {
  ++callCount_;
  requestedUrls_.push_back(request.url);

  const std::string_view path = pathOfUrl(request.url);
  const auto found = pathToKey_.find(path);
  const std::string_view key =
      (found != pathToKey_.end()) ? std::string_view{found->second} : kNotFoundKey;

  core::HttpResponse response = load(key);

  // ★Tab5 は応答をまるごと RAM に載せる。上限を超えたら本文を渡さない。
  //   HTTP ステータスが存在しない状態（トランスポート失敗）として返す。
  if (request.maxResponseBytes > 0 && response.body.size() > request.maxResponseBytes) {
    return transportFailure(core::HttpError::TooLarge,
                            "fixture body exceeds maxResponseBytes: " + std::string{key});
  }

  return response;
}

void FixtureHttpClient::mapPath(std::string path, std::string fixtureKey) {
  pathToKey_.insert_or_assign(std::move(path), std::move(fixtureKey));
}

void FixtureHttpClient::resetCounters() noexcept {
  callCount_ = 0;
  requestedUrls_.clear();
}

// ============================================================================
// FakeHttpClient
// ============================================================================

void FakeHttpClient::respondWith(std::string url, core::HttpResponse response) {
  queued_[std::move(url)].push_back(std::move(response));
}

void FakeHttpClient::respondWith(std::string url, int status, std::string body,
                                 std::string contentType) {
  core::HttpResponse response{};
  response.status = status;
  response.error = core::HttpError::None;
  response.body = std::move(body);
  response.contentType = std::move(contentType);
  respondWith(std::move(url), std::move(response));
}

void FakeHttpClient::failWith(std::string url, core::HttpError error) {
  // ★status を 0 のままにするのが要点。HTTP エラー（403/404）と区別できる。
  core::HttpResponse response{};
  response.status = 0;
  response.error = error;
  respondWith(std::move(url), std::move(response));
}

void FakeHttpClient::setDefaultResponse(core::HttpResponse response) {
  defaultResponse_ = std::move(response);
}

core::HttpResponse FakeHttpClient::fetch(const core::HttpRequest& request) {
  requests_.push_back(request);

  const auto found = queued_.find(request.url);
  if (found != queued_.end() && !found->second.empty()) {
    std::size_t& cursor = cursor_[request.url];
    // ★最後の 1 件は以降ずっと返る（ヘッダのコメント参照）。
    const std::size_t index = std::min(cursor, found->second.size() - 1);
    ++cursor;
    return found->second[index];
  }

  if (fallback_ != nullptr) {
    return fallback_->fetch(request);
  }
  if (defaultResponse_) {
    return *defaultResponse_;
  }
  // ★空の 200 を返さない。仕込み忘れが静かに通らないようにする。
  return transportFailure(core::HttpError::Unknown,
                          "FakeHttpClient: no response programmed for " + request.url);
}

std::size_t FakeHttpClient::callCountFor(std::string_view url) const noexcept {
  std::size_t count = 0;
  for (const auto& request : requests_) {
    if (request.url == url) {
      ++count;
    }
  }
  return count;
}

void FakeHttpClient::resetCounters() noexcept {
  requests_.clear();
  cursor_.clear();
}

void FakeHttpClient::clearResponses() noexcept {
  queued_.clear();
  cursor_.clear();
  defaultResponse_.reset();
}

}  // namespace ambient::transport
