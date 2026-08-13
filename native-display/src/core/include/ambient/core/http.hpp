// ambient/core/http.hpp — HTTP クライアントの抽象
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜ IHttpClient が core にあるのか（transport ではなく）
// ============================================================================
//
// 移植したい **純粋なポリシー** ―― 送信キュー管理、リトライ規律、
// 「1 回の update() で HTTP は最大 1 件」「未送信 record を config poll より優先」
// といった優先制御 ―― は I/O を含まない判断ロジックであり、core にあるべきである。
// docs/PORTABILITY.md が明言しているとおり、純粋ロジックが transport にあったら
// それは配置ミスである。
//
// そのポリシーは「HTTP を 1 件投げる」という操作に依存する。
// 依存する側が core にいる以上、依存される抽象も core になければならない。
// curl (Mac) / HTTPClient (Tab5) という具体は transport 層の実装詳細であり、
// この抽象の**背後**に隠れる。
//
// 副次的だが大きな効果: fixture 駆動のテストと実通信が **同一のクライアントコード**を
// 通る。IHttpClient の fixture 実装（レーン 1D）と curl 実装を差し替えるだけなので、
// 「fixture でだけ動く経路」が原理的に生まれない。
//
// ============================================================================
// なぜ「パース済み JSON」を返してはいけないのか ← 最重要
// ============================================================================
//
// GeoFrame server のエラー応答は **JSON とは限らない**。
//   - Host 検証で拒否された 403        → HTML のエラーページ
//   - 静的ファイルのホワイトリスト外 403 → HTML
//   - 404                              → HTML
//
// 「どんな応答でも JSON parse する」実装は、**最も診断が必要な場面でクラッシュする**。
// 403 が返っているのに「JSON parse エラー」しか出ない、という最悪の体験になる。
//
// そこで IHttpClient の戻り値は **ステータスコード + 生のボディ** に固定する。
// パースはこの層の責務ではない。
//
// ★パーサへ渡してよいのは successBody() の戻り値だけ、という規律にすること。
//   successBody() は「トランスポート成功 かつ 2xx」のときだけボディを返す。
//   response.body に直接 parser を掛けるコードを書いたら、それはレビューで落とす。

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/clock.hpp"

namespace ambient::core {

enum class HttpMethod : std::uint8_t {
  Get = 0,
  Post,
};

struct HttpHeader {
  std::string name;
  std::string value;
};

// トランスポート層で起きた失敗。HTTP ステータスとは別物。
// ★status != 0 のとき error は None である（サーバが応答を返している）。
//   404 も 403 も「通信は成功」であり、error は None、status がその値になる。
enum class HttpError : std::uint8_t {
  None = 0,
  Timeout,        // 接続 / 読み取りのタイムアウト
  ConnectFailed,  // 名前解決失敗、接続拒否、経路なし
  TlsFailed,      // 証明書検証失敗など
  TooLarge,       // 応答が受け入れ上限を超えた
  Cancelled,      // 呼び出し側の都合で中断
  Unknown,
};

[[nodiscard]] const char* toString(HttpError error) noexcept;

struct HttpRequest {
  HttpMethod method{HttpMethod::Get};
  std::string url;
  std::string body;         // POST のときだけ意味を持つ
  std::string contentType;  // POST のときだけ意味を持つ
  std::vector<HttpHeader> headers;

  // 1 リクエストあたりの上限。hogo-meter の実測値は 5000ms
  // （constants.hpp の kHttpTimeout。出典もそこに書いてある）。
  Millis timeout{};

  // 受け入れるボディの最大バイト数。0 = 実装既定。
  // ★Tab5 では応答をまるごと RAM に載せる。無制限にしてはいけない。
  std::size_t maxResponseBytes{0};
};

struct HttpResponse {
  // HTTP ステータス。0 = 応答を得られなかった（error を見ること）。
  int status{0};

  HttpError error{HttpError::None};

  // ★生のバイト列。JSON とは限らない（HTML のエラーページのことがある）。
  //   ここに直接パーサを掛けないこと。successBody() を通すこと。
  std::string body;

  // サーバが返した Content-Type（そのまま。正規化しない）。
  std::string contentType;
};

// ----------------------------------------------------------------------------
// IHttpClient
// ----------------------------------------------------------------------------
//
// 同期・ブロッキング。Core は単一スレッド前提なので非同期にはしない。
// 呼び出し側の規律（hogo-meter の実装から移植する契約。constants.hpp 参照）:
//
//   ★1 回の update() で fetch() を呼ぶのは最大 1 回。
//     未送信の record キューがあれば、record 送信を config poll より優先する。
//
// この規律により 1 tick の停止時間が 1 リクエストぶんに抑えられる。
// 「まとめて 3 本投げる」は絶対にしないこと（描画が止まる）。
class IHttpClient {
 public:
  IHttpClient() = default;
  virtual ~IHttpClient() = default;
  IHttpClient(const IHttpClient&) = delete;
  IHttpClient& operator=(const IHttpClient&) = delete;
  IHttpClient(IHttpClient&&) = delete;
  IHttpClient& operator=(IHttpClient&&) = delete;

  [[nodiscard]] virtual HttpResponse fetch(const HttpRequest& request) = 0;
};

// ----------------------------------------------------------------------------
// パースの門 ― ここを通さずにパーサへ渡さないこと
// ----------------------------------------------------------------------------

// トランスポート成功 (error == None) かつ 2xx のときだけボディを返す。
// それ以外は nullopt。
//
// 返るのは string_view なので、response の生存期間を超えて使わないこと。
[[nodiscard]] std::optional<std::string_view> successBody(const HttpResponse& response) noexcept;

// 2xx か。
[[nodiscard]] constexpr bool isSuccessStatus(int status) noexcept {
  return status >= 200 && status < 300;
}

}  // namespace ambient::core
