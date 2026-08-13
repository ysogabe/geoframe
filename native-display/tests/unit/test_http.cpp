// tests/unit/test_http.cpp — HTTP 応答の扱い（レーン 1D）
//
// ★次フェーズでここに書くもの:
//   - fixture 実装（IHttpClient）を差し込んで、実通信と同じクライアントコードが
//     動くこと
//   - ★403 で HTML が返ってきたときにパーサへ渡らないこと
//     （実際に GeoFrame の Host 検証で返る HTML を fixture に入れて確かめる）
//   - タイムアウト / 接続失敗のときの Degraded 遷移
//   - 「1 回の update() で fetch は最大 1 回」「未送信 record を config poll より優先」

#include <doctest.h>

#include <string_view>

#include "ambient/core/http.hpp"

using ambient::core::HttpError;
using ambient::core::HttpResponse;
using ambient::core::isSuccessStatus;
using ambient::core::successBody;

TEST_CASE("2xx かつトランスポート成功のときだけボディが取れる") {
  HttpResponse ok{};
  ok.status = 200;
  ok.body = "[]";
  const auto body = successBody(ok);
  REQUIRE(body.has_value());
  CHECK(*body == "[]");
}

TEST_CASE("★403 の HTML エラーページはパーサへ渡らない") {
  // GeoFrame の Host 検証で拒否されると HTML が返る。
  // 「どんな応答でも JSON parse する」実装は、最も診断が必要な場面で死ぬ。
  HttpResponse forbidden{};
  forbidden.status = 403;
  forbidden.contentType = "text/html; charset=utf-8";
  forbidden.body = "<!DOCTYPE html><html><body>Forbidden</body></html>";
  CHECK_FALSE(successBody(forbidden).has_value());
}

TEST_CASE("404 の HTML もパーサへ渡らない") {
  HttpResponse notFound{};
  notFound.status = 404;
  notFound.contentType = "text/html";
  notFound.body = "<html>404</html>";
  CHECK_FALSE(successBody(notFound).has_value());
}

TEST_CASE("トランスポート失敗のときはボディを見ない") {
  HttpResponse timeout{};
  timeout.status = 0;
  timeout.error = HttpError::Timeout;
  timeout.body = "";
  CHECK_FALSE(successBody(timeout).has_value());

  // ステータスが 200 に見えてもトランスポート失敗なら渡さない（防御）。
  HttpResponse weird{};
  weird.status = 200;
  weird.error = HttpError::ConnectFailed;
  weird.body = "[]";
  CHECK_FALSE(successBody(weird).has_value());
}

TEST_CASE("204 は 2xx なので通る（本文が空でも成功）") {
  HttpResponse noContent{};
  noContent.status = 204;
  const auto body = successBody(noContent);
  REQUIRE(body.has_value());
  CHECK(body->empty());
}

TEST_CASE("isSuccessStatus の境界") {
  static_assert(!isSuccessStatus(199));
  static_assert(isSuccessStatus(200));
  static_assert(isSuccessStatus(299));
  static_assert(!isSuccessStatus(300));
  static_assert(!isSuccessStatus(0));
  CHECK(true);
}

TEST_CASE("HttpError の文字列化") {
  CHECK(std::string_view{ambient::core::toString(HttpError::Timeout)} == "Timeout");
  CHECK(std::string_view{ambient::core::toString(HttpError::None)} == "None");
}
