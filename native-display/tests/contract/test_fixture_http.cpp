// test_fixture_http.cpp — transport 層の契約テスト（レーン 1D）
//
// ============================================================================
// このファイルが固定していること
// ============================================================================
//
//   1. ステータスコードで分岐してから parse すること。403 / 404 で HTML が
//      返っても壊れないこと（assets/fixtures/errors/ の **実物**を使う）
//   2. /api/photos が裸の JSON 配列であること（{items:[...]} ではない）
//   3. ok フィールドの有無がエンドポイントごとに **非対称**であること
//   4. roomenv の ok:false が 200 で返り、トランスポート層はこれを成功として
//      パーサへ渡すこと（＝障害として握り潰さないこと）
//   5. presence.night が true / false / null の 3 値をとること
//   6. タイムアウト / 接続失敗が HTTP エラーと **区別できる**こと
//   7. 呼び出し回数が数えられること
//
// ★実サーバにも本番 API にも一切繋がない。すべて fixture ファイルと
//   FakeHttpClient で再現している。
//
// ============================================================================
// JSON のパースについて — この層では「構造の検査」に留める
// ============================================================================
//
// third_party/ に vendor されているのは doctest だけで、JSON ライブラリは無い。
// 新たに vendor すると CMakeLists.txt の変更が要り、並行作業中の他レーンと
// 衝突する。よってここでは JSON を完全にはパースせず、
//
//   * ステータス / Content-Type / ボディの生文字列
//   * 先頭が '[' か '{' か（裸の配列かオブジェクトか）
//   * トップレベルのキーの出現「回数」
//
// のレベルで契約を固定する。回数で見ているのは、単なる部分文字列の存在より
// 強い主張になるため（0 件は偶然では起こらないし、件数が変われば検出できる）。
// 本格的なパーサの契約は、パーサを実装するレーンの責務である。

#include <doctest.h>

#include <cstddef>
#include <fstream>
#include <iterator>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/http.hpp"
#include "ambient/transport/fixture_http_client.hpp"

using ambient::core::HttpError;
using ambient::core::HttpRequest;
using ambient::core::HttpResponse;
using ambient::core::IHttpClient;
using ambient::core::successBody;
using ambient::transport::FakeHttpClient;
using ambient::transport::FixtureHttpClient;
using ambient::transport::FixtureMeta;

namespace {

// contract_tests には AMBIENT_ASSET_DIR がコンパイル時定義で入っている
// （CMakeLists.txt の contract_tests の節を参照）。
// ライブラリ側はこの定義を持たないので、場所を知っているのは呼び出し側である。
[[nodiscard]] std::string fixtureDir() { return std::string{AMBIENT_ASSET_DIR "/fixtures"}; }

[[nodiscard]] std::string baseUrl() { return "http://192.0.2.5:8080"; }

[[nodiscard]] HttpRequest getRequest(const std::string& url) {
  HttpRequest request{};
  request.url = url;
  return request;
}

[[nodiscard]] std::optional<std::string> readTextFile(const std::string& path) {
  std::ifstream in(path, std::ios::binary);
  if (!in.is_open()) {
    return std::nullopt;
  }
  return std::string{std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

[[nodiscard]] std::size_t countOccurrences(std::string_view haystack, std::string_view needle) {
  if (needle.empty()) {
    return 0;
  }
  std::size_t count = 0;
  for (std::size_t pos = haystack.find(needle); pos != std::string_view::npos;
       pos = haystack.find(needle, pos + needle.size())) {
    ++count;
  }
  return count;
}

[[nodiscard]] char firstNonSpace(std::string_view text) {
  for (const char c : text) {
    if (c != ' ' && c != '\t' && c != '\n' && c != '\r') {
      return c;
    }
  }
  return '\0';
}

[[nodiscard]] char lastNonSpace(std::string_view text) {
  for (std::size_t i = text.size(); i > 0; --i) {
    const char c = text[i - 1];
    if (c != ' ' && c != '\t' && c != '\n' && c != '\r') {
      return c;
    }
  }
  return '\0';
}

// "key": <token> の <token> を生のまま取り出す。JSON のパースではなく、
// 「null と false が別のトークンとして書かれている」ことを見るための最小の検査。
[[nodiscard]] std::string rawValueOf(std::string_view body, std::string_view key) {
  const std::string quoted = "\"" + std::string{key} + "\"";
  const auto keyPos = body.find(quoted);
  if (keyPos == std::string_view::npos) {
    return {};
  }
  const auto colon = body.find(':', keyPos + quoted.size());
  if (colon == std::string_view::npos) {
    return {};
  }
  std::size_t start = colon + 1;
  while (start < body.size() && (body[start] == ' ' || body[start] == '\t')) {
    ++start;
  }
  std::size_t end = start;
  while (end < body.size() && body[end] != ',' && body[end] != '\n' && body[end] != '}' &&
         body[end] != '\r') {
    ++end;
  }
  return std::string{body.substr(start, end - start)};
}

// --------------------------------------------------------------------------
// パーサの代役
// --------------------------------------------------------------------------
//
// ★呼び出し側が守るべき形をここに固定する:
//   successBody() を通ったものだけがパーサへ渡る。
//   response.body を直接パーサへ渡すコードはレビューで落とす（http.hpp の契約）。
struct ParserSpy {
  int calls{0};
  std::string lastSeen;

  void parse(std::string_view body) {
    ++calls;
    lastSeen = std::string{body};
  }
};

[[nodiscard]] bool consumeThroughGate(const HttpResponse& response, ParserSpy& parser) {
  if (const auto body = successBody(response)) {
    parser.parse(*body);
    return true;
  }
  return false;
}

// トランスポート失敗（HTTP ステータスが存在しない状態）か。
// これが HTTP エラー（403 / 404）との判別軸である。
[[nodiscard]] bool isTransportFailure(const HttpResponse& response) {
  return response.error != HttpError::None;
}

}  // namespace

// ============================================================================
// 0. 土台 — fixture ディレクトリと index.json との同期
// ============================================================================

TEST_CASE("fixture 表が index.json と同期している") {
  // FixtureHttpClient は index.json をパースせず、同等の対応表を持っている
  // （この層に JSON パーサを持ち込まないため）。表がずれたらここで落ちる。
  const auto index = readTextFile(fixtureDir() + "/index.json");
  REQUIRE(index.has_value());

  const auto fixtures = FixtureHttpClient::knownFixtures();
  REQUIRE_FALSE(fixtures.empty());

  std::string failures;
  for (const auto& meta : fixtures) {
    if (countOccurrences(*index, "\"" + std::string{meta.key} + "\"") == 0) {
      failures += "index.json にキーが無い: " + std::string{meta.key} + "\n";
    }
    if (countOccurrences(*index, std::string{meta.bodyPath}) == 0) {
      failures += "index.json に body パスが無い: " + std::string{meta.bodyPath} + "\n";
    }
    const auto body = readTextFile(fixtureDir() + "/" + std::string{meta.bodyPath});
    if (!body) {
      failures += "ファイルが読めない: " + std::string{meta.bodyPath} + "\n";
    } else if (body->empty()) {
      failures += "ファイルが空: " + std::string{meta.bodyPath} + "\n";
    }
  }
  INFO(failures);
  CHECK(failures.empty());

  // ★逆向きの検査。index.json にだけ追加された fixture を見逃さないため。
  //   index.json の各エントリは "body": を 1 つ持つので、その件数と表の件数が一致する。
  CHECK(countOccurrences(*index, "\"body\":") == fixtures.size());
}

TEST_CASE("知らない fixture キー・読めないファイルは例外ではなく戻り値で表現される") {
  const FixtureHttpClient client{fixtureDir()};

  const auto unknown = client.load("no-such-fixture");
  CHECK(unknown.status == 0);
  CHECK(unknown.error == HttpError::Unknown);
  CHECK_FALSE(successBody(unknown).has_value());

  const FixtureHttpClient broken{fixtureDir() + "/no-such-directory"};
  const auto missing = broken.load("photos");
  CHECK(missing.status == 0);
  CHECK(missing.error == HttpError::Unknown);
  CHECK_FALSE(successBody(missing).has_value());
}

// ============================================================================
// 1. ★ステータスコードで分岐してから parse する
// ============================================================================

TEST_CASE("★403 / 404 は HTML が返る — 実物の fixture でパーサへ渡らないことを固定する") {
  const FixtureHttpClient client{fixtureDir()};

  struct ErrorCase {
    std::string_view key;
    int status;
    std::string_view marker;  // 経路ごとに違う本文（同じ 403 でも別の理由）
  };
  const ErrorCase cases[] = {
      {"err-host-forbidden", 403, "Forbidden host"},
      {"err-static-forbidden", 403, "Error code: 403"},
      {"err-not-found", 404, "File not found"},
  };

  std::string failures;
  for (const auto& c : cases) {
    ParserSpy parser{};
    const auto response = client.load(c.key);

    if (response.status != c.status) {
      failures += std::string{c.key} + ": ステータスが違う\n";
    }
    // ★Content-Type は JSON ではない。実測どおり text/html。
    if (response.contentType != "text/html;charset=utf-8") {
      failures += std::string{c.key} + ": Content-Type が text/html ではない\n";
    }
    // ★本文は HTML。JSON パーサへ渡したら壊れる形。
    if (response.body.rfind("<!DOCTYPE HTML", 0) != 0) {
      failures += std::string{c.key} + ": 本文が HTML で始まっていない\n";
    }
    if (countOccurrences(response.body, c.marker) == 0) {
      failures += std::string{c.key} + ": 経路固有の文言が無い\n";
    }
    // ★トランスポートとしては成功している。error は None、status がその値。
    if (isTransportFailure(response)) {
      failures += std::string{c.key} + ": HTTP エラーなのにトランスポート失敗扱い\n";
    }
    // ★門を通らない = パーサが一度も呼ばれない。ここが本質。
    if (consumeThroughGate(response, parser)) {
      failures += std::string{c.key} + ": successBody() を通ってしまった\n";
    }
    if (parser.calls != 0) {
      failures += std::string{c.key} + ": パーサが呼ばれてしまった\n";
    }
  }
  INFO(failures);
  CHECK(failures.empty());
}

TEST_CASE("★同じ 403 でも Host 拒否と静的ホワイトリスト外は別の本文である") {
  const FixtureHttpClient client{fixtureDir()};
  const auto host = client.load("err-host-forbidden");
  const auto stat = client.load("err-static-forbidden");

  CHECK(host.status == 403);
  CHECK(stat.status == 403);
  // Host 拒否だけが "Forbidden host"。静的側は "Forbidden." で理由が違う。
  CHECK(countOccurrences(host.body, "Forbidden host") == std::size_t{1});
  CHECK(countOccurrences(stat.body, "Forbidden host") == std::size_t{0});
  CHECK(host.body != stat.body);
}

TEST_CASE("2xx のときだけパーサが呼ばれる") {
  const FixtureHttpClient client{fixtureDir()};
  ParserSpy parser{};

  const auto ok = client.load("env");
  REQUIRE(ok.status == 200);
  CHECK(consumeThroughGate(ok, parser));
  CHECK(parser.calls == 1);

  // 続けてエラー応答を食わせても呼び出し回数は増えない。
  CHECK_FALSE(consumeThroughGate(client.load("err-not-found"), parser));
  CHECK(parser.calls == 1);
}

TEST_CASE("表に無いパスは 404 の HTML になる（実サーバと同じ壊れ方）") {
  FixtureHttpClient client{fixtureDir()};
  const auto response = client.fetch(getRequest(baseUrl() + "/api/typo"));

  CHECK(response.status == 404);
  CHECK(response.contentType == "text/html;charset=utf-8");
  CHECK(firstNonSpace(response.body) == '<');
  CHECK_FALSE(successBody(response).has_value());
}

TEST_CASE("静的ホワイトリスト外のパスは 403 の HTML になる") {
  FixtureHttpClient client{fixtureDir()};
  const auto response = client.fetch(getRequest(baseUrl() + "/server.py"));

  CHECK(response.status == 403);
  CHECK(firstNonSpace(response.body) == '<');
  CHECK_FALSE(successBody(response).has_value());
}

// ============================================================================
// 2. ★/api/photos は裸の JSON 配列
// ============================================================================

TEST_CASE("★/api/photos は裸の JSON 配列（{items:[...]} ではない）") {
  FixtureHttpClient client{fixtureDir()};
  const auto response = client.fetch(getRequest(baseUrl() + "/api/photos"));
  REQUIRE(response.status == 200);

  const auto body = successBody(response);
  REQUIRE(body.has_value());

  // ★トップレベルがいきなり '['。
  CHECK(firstNonSpace(*body) == '[');
  CHECK(lastNonSpace(*body) == ']');
  CHECK(firstNonSpace(*body) != '{');

  // ★"items"を期待するパーサは多数の写真があっても0件として動き、しかもエラーに
  //   ならない。photos の応答に "items" は存在しないことを固定しておく。
  CHECK(countOccurrences(*body, "\"items\"") == std::size_t{0});

  // 対照: admin-photos はオブジェクトで items を持つ。同じ扱いにできない。
  const auto admin = client.fetch(getRequest(baseUrl() + "/api/admin/photos?page=1&per=50"));
  REQUIRE(admin.status == 200);
  CHECK(firstNonSpace(admin.body) == '{');
  CHECK(countOccurrences(admin.body, "\"items\"") == std::size_t{1});
}

TEST_CASE("file と thumb の先頭スラッシュは非対称のまま fixture に保存されている") {
  const FixtureHttpClient client{fixtureDir()};

  // /api/photos の file は先頭スラッシュ「無し」。
  CHECK(countOccurrences(client.load("photos").body, "\"file\": \"derivatives/") > std::size_t{0});
  // /api/admin/photos の thumb は先頭スラッシュ「あり」。
  CHECK(countOccurrences(client.load("admin-photos").body, "\"thumb\": \"/derivatives/") >
        std::size_t{0});
}

// ============================================================================
// 3. ★ok フィールドの有無がエンドポイントごとに非対称
// ============================================================================

TEST_CASE("★ok フィールドの有無はエンドポイントごとに非対称") {
  const FixtureHttpClient client{fixtureDir()};

  struct OkExpectation {
    std::string_view key;
    std::size_t okFields;  // トップレベルの "ok" の出現回数
  };
  // 出典: docs/FIXTURES.md §5-2（実測）
  const OkExpectation expectations[] = {
      // 無い
      {"env", 0},
      {"stats", 0},
      {"presence", 0},
      {"photos", 0},  // そもそも裸の配列
      // ある
      {"roomenv", 1},
      {"admin-photos", 1},
  };

  std::string failures;
  for (const auto& e : expectations) {
    const auto response = client.load(e.key);
    if (response.status != 200) {
      failures += std::string{e.key} + ": 200 ではない\n";
      continue;
    }
    const auto actual = countOccurrences(response.body, "\"ok\"");
    if (actual != e.okFields) {
      failures += std::string{e.key} + ": \"ok\" の出現回数が期待と違う\n";
    }
  }
  INFO(failures);
  CHECK(failures.empty());
}

TEST_CASE("★「ok を見て失敗判定する」共通ヘルパは env / stats / presence / photos で必ず落ちる") {
  const FixtureHttpClient client{fixtureDir()};

  // 素朴な共通ヘルパ。ok が true でなければ失敗とみなす。
  const auto naiveLooksSuccessful = [](std::string_view body) {
    return countOccurrences(body, "\"ok\": true") == std::size_t{1};
  };

  // ok を持つ 2 つでは正しく動く。
  CHECK(naiveLooksSuccessful(client.load("roomenv").body));
  CHECK(naiveLooksSuccessful(client.load("admin-photos").body));

  // ★ok を持たない 4 つでは、ステータス 200 の完全に正常な応答を「失敗」と
  //   誤判定する。だから ok での判定にはエンドポイントごとの分岐が要る。
  std::string failures;
  for (const std::string_view key : {"env", "stats", "presence", "photos"}) {
    const auto response = client.load(key);
    if (response.status != 200) {
      failures += std::string{key} + ": 200 ではない\n";
    }
    if (successBody(response).has_value() == false) {
      failures += std::string{key} + ": successBody() が通らない\n";
    }
    if (naiveLooksSuccessful(response.body)) {
      failures += std::string{key} + ": ok を持たないはずなのに素朴ヘルパが通った\n";
    }
  }
  INFO(failures);
  CHECK(failures.empty());
}

// ============================================================================
// 4. ★roomenv の ok:false は「機能なし」であって障害ではない
// ============================================================================

TEST_CASE("★roomenv の ok:false は 200 で返り、トランスポート層はパーサへ渡す") {
  // ★このテストが主張できるのは **トランスポート層の半分**だけである。
  //   「Degraded にも STALE にも落とさない」という判定は
  //   src/core/state/（レーン 1A）の責務であり、ここでは検証できない。
  //   ここで固定するのは「ok:false が障害として握り潰されず、
  //   ポリシー層まで確実に届く」こと。これが崩れると 1A は判断材料を失う。
  //
  //   ok:false の roomenv fixture は存在しない（実採取時は ok:true だった。
  //   assets/ は 1D の所有ではないので追加もしない）。よって FakeHttpClient で
  //   同じ形の本文を組み立てて注入する。
  const std::string url = baseUrl() + "/api/roomenv";
  FakeHttpClient fake;
  fake.respondWith(url, 200, R"({"ok": false, "items": []})", "application/json; charset=utf-8");

  ParserSpy parser{};
  const auto response = fake.fetch(getRequest(url));

  CHECK(response.status == 200);
  CHECK(response.error == HttpError::None);
  CHECK_FALSE(isTransportFailure(response));
  // ★門を通る。＝パーサ（＝ポリシー層）まで届く。
  CHECK(consumeThroughGate(response, parser));
  CHECK(parser.calls == 1);
  CHECK(countOccurrences(parser.lastSeen, "\"ok\": false") == std::size_t{1});
}

TEST_CASE("対照: 本物の障害（5xx）は門を通らない") {
  const std::string url = baseUrl() + "/api/roomenv";
  FakeHttpClient fake;
  fake.respondWith(url, 503, "<html>Service Unavailable</html>", "text/html;charset=utf-8");

  ParserSpy parser{};
  const auto response = fake.fetch(getRequest(url));

  CHECK(response.status == 503);
  CHECK_FALSE(consumeThroughGate(response, parser));
  CHECK(parser.calls == 0);
}

// ============================================================================
// 5. ★presence.night は true / false / null の 3 値
// ============================================================================

TEST_CASE("★presence.night は true / false / null の 3 値をとる") {
  const FixtureHttpClient client{fixtureDir()};

  struct NightCase {
    std::string_view key;
    std::string_view raw;
  };
  const NightCase cases[] = {
      {"presence-night-true", "true"},
      {"presence-night-false", "false"},
      {"presence-night-null", "null"},
  };

  std::string failures;
  for (const auto& c : cases) {
    const auto response = client.load(c.key);
    if (response.status != 200) {
      failures += std::string{c.key} + ": 200 ではない\n";
      continue;
    }
    if (rawValueOf(response.body, "night") != c.raw) {
      failures += std::string{c.key} + ": night の生の値が期待と違う\n";
    }
  }
  INFO(failures);
  CHECK(failures.empty());

  // ★null と false は生の段階で別のトークンである。
  //   value_or(false) で潰すと「判定不能」と「昼」の区別が消える。
  const auto nightNull = rawValueOf(client.load("presence-night-null").body, "night");
  const auto nightFalse = rawValueOf(client.load("presence-night-false").body, "night");
  CHECK(nightNull != nightFalse);
  CHECK(nightNull == "null");
}

TEST_CASE("mapPath で /api/presence の応答を night=null 版に差し替えられる") {
  FixtureHttpClient client{fixtureDir()};
  const std::string url = baseUrl() + "/api/presence";

  CHECK(rawValueOf(client.fetch(getRequest(url)).body, "night") == "false");

  client.mapPath("/api/presence", "presence-night-null");
  CHECK(rawValueOf(client.fetch(getRequest(url)).body, "night") == "null");
}

// ============================================================================
// 6. ★タイムアウト / 接続失敗は HTTP エラーと区別できる
// ============================================================================

TEST_CASE("★タイムアウト・接続失敗は HTTP エラーと区別できる") {
  const std::string url = baseUrl() + "/api/env";
  const FixtureHttpClient fixtures{fixtureDir()};

  // (a) トランスポート失敗: HTTP ステータスが存在しない。
  FakeHttpClient timedOut;
  timedOut.failWith(url, HttpError::Timeout);
  const auto timeout = timedOut.fetch(getRequest(url));

  FakeHttpClient refused;
  refused.failWith(url, HttpError::ConnectFailed);
  const auto connectFailed = refused.fetch(getRequest(url));

  // (b) HTTP エラー: 通信は成功していて、ステータスがその値。
  const auto notFound = fixtures.load("err-not-found");
  const auto forbidden = fixtures.load("err-host-forbidden");

  // どれもパーサへは渡らない。ここは同じ。
  CHECK_FALSE(successBody(timeout).has_value());
  CHECK_FALSE(successBody(connectFailed).has_value());
  CHECK_FALSE(successBody(notFound).has_value());
  CHECK_FALSE(successBody(forbidden).has_value());

  // ★しかし「なぜ失敗したのか」は区別できなければならない。
  //   判別軸 1: error が None かどうか
  CHECK(isTransportFailure(timeout));
  CHECK(isTransportFailure(connectFailed));
  CHECK_FALSE(isTransportFailure(notFound));
  CHECK_FALSE(isTransportFailure(forbidden));

  //   判別軸 2: status が 0 かどうか（http.hpp: 0 = 応答を得られなかった）
  CHECK(timeout.status == 0);
  CHECK(connectFailed.status == 0);
  CHECK(notFound.status == 404);
  CHECK(forbidden.status == 403);

  //   種別も保たれる（Degraded の理由表示に効く）
  CHECK(timeout.error == HttpError::Timeout);
  CHECK(connectFailed.error == HttpError::ConnectFailed);
  CHECK(notFound.error == HttpError::None);
}

TEST_CASE("maxResponseBytes を超える応答は TooLarge になり本文が渡らない") {
  // ★Tab5 は応答をまるごと RAM に載せる。上限を掛ける口があることを固定する。
  FixtureHttpClient client{fixtureDir()};
  auto request = getRequest(baseUrl() + "/api/photos");
  request.maxResponseBytes = 128;

  const auto response = client.fetch(request);
  CHECK(response.status == 0);
  CHECK(response.error == HttpError::TooLarge);
  CHECK(isTransportFailure(response));
  CHECK_FALSE(successBody(response).has_value());

  // 上限を外せば同じ URL が通る。
  const auto full = client.fetch(getRequest(baseUrl() + "/api/photos"));
  CHECK(full.status == 200);
  CHECK(successBody(full).has_value());
}

TEST_CASE("応答は呼び出しごとに差し替えられる（2 回失敗してから成功する）") {
  const std::string url = baseUrl() + "/api/env";
  FakeHttpClient fake;
  fake.failWith(url, HttpError::Timeout);
  fake.failWith(url, HttpError::ConnectFailed);
  fake.respondWith(url, 200, "{}", "application/json; charset=utf-8");

  CHECK(fake.fetch(getRequest(url)).error == HttpError::Timeout);
  CHECK(fake.fetch(getRequest(url)).error == HttpError::ConnectFailed);
  CHECK(fake.fetch(getRequest(url)).status == 200);
  // ★最後の 1 件は以降ずっと返る。
  CHECK(fake.fetch(getRequest(url)).status == 200);
  CHECK(fake.callCount() == std::size_t{4});
}

TEST_CASE("仕込み忘れは空の 200 ではなく大きな失敗になる") {
  FakeHttpClient fake;
  const auto response = fake.fetch(getRequest(baseUrl() + "/api/env"));
  CHECK(response.status == 0);
  CHECK(response.error == HttpError::Unknown);
  CHECK_FALSE(successBody(response).has_value());
}

// ============================================================================
// 7. ★呼び出し回数を数えられる
// ============================================================================

TEST_CASE("★呼び出し回数を数えられる（1 回の update() で HTTP は最大 1 件）") {
  FixtureHttpClient client{fixtureDir()};
  CHECK(client.callCount() == std::size_t{0});

  // 「1 tick につき fetch は 1 回まで」という規律を模した呼び出し側。
  const auto updateOnce = [&client](const std::string& path) {
    return client.fetch(getRequest(baseUrl() + path));
  };

  updateOnce("/api/env");
  updateOnce("/api/presence");
  updateOnce("/api/roomenv");

  CHECK(client.callCount() == std::size_t{3});
  REQUIRE(client.requestedUrls().size() == std::size_t{3});
  CHECK(client.requestedUrls()[0] == baseUrl() + "/api/env");
  CHECK(client.requestedUrls()[2] == baseUrl() + "/api/roomenv");

  client.resetCounters();
  CHECK(client.callCount() == std::size_t{0});
  CHECK(client.requestedUrls().empty());

  // パス表は resetCounters() では消えない。
  CHECK(updateOnce("/api/env").status == 200);
  CHECK(client.callCount() == std::size_t{1});
}

TEST_CASE("FakeHttpClient は URL ごとの呼び出し回数を数えられる") {
  const std::string envUrl = baseUrl() + "/api/env";
  const std::string photosUrl = baseUrl() + "/api/photos";

  FakeHttpClient fake;
  fake.respondWith(envUrl, 200, "{}", "application/json; charset=utf-8");
  fake.respondWith(photosUrl, 200, "[]", "application/json; charset=utf-8");

  static_cast<void>(fake.fetch(getRequest(envUrl)));
  static_cast<void>(fake.fetch(getRequest(envUrl)));
  static_cast<void>(fake.fetch(getRequest(photosUrl)));

  CHECK(fake.callCount() == std::size_t{3});
  CHECK(fake.callCountFor(envUrl) == std::size_t{2});
  CHECK(fake.callCountFor(photosUrl) == std::size_t{1});
  CHECK(fake.callCountFor(baseUrl() + "/api/stats") == std::size_t{0});
  REQUIRE(fake.requests().size() == std::size_t{3});
  CHECK(fake.requests()[0].url == envUrl);

  fake.resetCounters();
  CHECK(fake.callCount() == std::size_t{0});
  // 仕込みは残っている。
  CHECK(fake.fetch(getRequest(envUrl)).status == 200);
}

// ============================================================================
// 層の境界 — fixture と Fake が同じ抽象を通ること
// ============================================================================

TEST_CASE("fixture 実装も Fake も IHttpClient としてまったく同じに使える") {
  // ★これが fixture を IHttpClient の実装にした理由そのもの。
  //   呼び出し側は基底のポインタしか知らないので、fixture 駆動のテストと
  //   実通信（後続フェーズの curl 実装）が同一のコードを通る。
  const std::string url = baseUrl() + "/api/env";

  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake;
  fake.respondWith(url, 200, "{}", "application/json; charset=utf-8");

  const auto callThroughAbstraction = [&url](IHttpClient& client) {
    ParserSpy parser{};
    const bool parsed = consumeThroughGate(client.fetch(getRequest(url)), parser);
    return parsed && parser.calls == 1;
  };

  CHECK(callThroughAbstraction(fixtures));
  CHECK(callThroughAbstraction(fake));
}

TEST_CASE("Fake は fixture を土台にして 1 本だけ壊せる") {
  // 正常系は実応答のまま、対象の URL だけタイムアウトさせる。
  const std::string envUrl = baseUrl() + "/api/env";
  const std::string presenceUrl = baseUrl() + "/api/presence";

  FixtureHttpClient fixtures{fixtureDir()};
  FakeHttpClient fake{&fixtures};
  fake.failWith(envUrl, HttpError::Timeout);

  const auto broken = fake.fetch(getRequest(envUrl));
  CHECK(broken.error == HttpError::Timeout);
  CHECK(broken.status == 0);

  // 仕込んでいない URL は fixture の実応答がそのまま返る。
  const auto intact = fake.fetch(getRequest(presenceUrl));
  CHECK(intact.status == 200);
  CHECK(firstNonSpace(intact.body) == '{');
  CHECK(rawValueOf(intact.body, "night") == "false");

  // 委譲された分は fixture 側でも数えられている。
  CHECK(fake.callCount() == std::size_t{2});
  CHECK(fixtures.callCount() == std::size_t{1});
}
