// ambient/transport/fixture_http_client.hpp — テスト用の IHttpClient 実装 2 種
//
// ============================================================================
// なぜ fixture を IHttpClient の「実装」にするのか
// ============================================================================
//
// fixture 駆動のテストと、後続フェーズの実通信（libcurl / esp_http_client）が
// **同一のクライアントコードを通る**ようにするため。fixture を別経路にすると
// 「fixture でだけ動く経路」が生まれ、実通信へ切り替えた瞬間に壊れる。
//
// ============================================================================
// なぜ core ではなく transport 層に置くのか
// ============================================================================
//
// ファイルを読むため std::filesystem が要る。ambient_core は std::filesystem を
// 使えない（ESP32 の VFS が POSIX セマンティクスを満たさない。
// docs/PORTABILITY.md 参照）。よって transport 層に置く。
//
// ★このヘッダに <filesystem> も curl の型も出さないこと。
//   Tab5 ではこの層ごと esp_http_client ベースの実装に差し替わる。
//   ディレクトリは std::string で受け、path 型は .cpp に閉じ込めてある。
//
// ============================================================================
// なぜ型を 2 つに分けたのか
// ============================================================================
//
//   FixtureHttpClient — ファイル駆動。読み取り専用。実サーバの応答を再現する。
//   FakeHttpClient    — メモリ上でプログラム可能。異常系を「注入」する。
//
// 役割が違う（前者は忠実さ、後者は自由さ）ので 1 つの型に押し込むと、
// 「この応答はどこから来たのか」がテストを読んでも分からなくなる。
//
// 2 つは合成できる:
//
//   FixtureHttpClient fixtures{dir};
//   FakeHttpClient    fake{&fixtures};        // 未設定 URL は fixture へ委譲
//   fake.failWith(url, HttpError::Timeout);   // この URL だけタイムアウトさせる
//
// これで「正常系は実応答のまま、1 本だけ壊す」が 1 行で書ける。
//
// ★どちらも実ネットワークに一切触れない。

#pragma once

#include <cstddef>
#include <functional>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/http.hpp"

namespace ambient::transport {

// ----------------------------------------------------------------------------
// FixtureMeta — assets/fixtures/index.json の 1 エントリに対応
// ----------------------------------------------------------------------------
//
// status / contentType は **採取時の実測値**であって、こちらで決めた値ではない。
// 特に errors/ の 3 件は 403 / 404 で Content-Type が text/html である。
//
// 文字列ビューはすべて静的記憶域の文字列リテラルを指す（.cpp の constexpr 表）。
// 生存期間の心配は要らない。
struct FixtureMeta {
  std::string_view key;          // index.json の fixtures のキー
  int status{0};                 // 実測ステータスコード
  std::string_view contentType;  // 実測 Content-Type（正規化しない）
  std::string_view bodyPath;     // fixture ディレクトリからの相対パス
};

// ----------------------------------------------------------------------------
// FixtureHttpClient — assets/fixtures/ 以下のファイルを応答として返す
// ----------------------------------------------------------------------------
//
// URL のパス部分を fixture キーへ割り当て、そのファイルを **生のボディ**として
// 返す。パースはしない（IHttpClient の契約どおり）。
//
// ★エラー応答も忠実に返す。403 / 404 では index.json の実測どおり
//   status = 403/404、Content-Type = text/html;charset=utf-8、本文は **HTML** に
//   なる。「どんな応答でも JSON parse する」実装はここで壊れるのが正しい。
//
// 既定のパス表（コンストラクタで構築。mapPath() で上書き・追加できる）:
//
//   /api/photos        -> photos
//   /api/env           -> env
//   /api/roomenv       -> roomenv
//   /api/presence      -> presence
//   /api/stats         -> stats
//   /api/admin/photos  -> admin-photos
//   /server.py         -> err-static-forbidden   （静的ホワイトリスト外の 403）
//
// 表に無いパスは **err-not-found（404・HTML）** になる。実サーバと同じ壊れ方を
// させるためで、テストで URL を打ち間違えたときに空の 200 で静かに通らない。
//
// HttpRequest::maxResponseBytes を超えるボディは status = 0 / error = TooLarge で
// 返す（Tab5 は応答をまるごと RAM に載せるため、この経路の検証が要る）。
class FixtureHttpClient final : public core::IHttpClient {
 public:
  // fixtureDir は assets/fixtures に相当するディレクトリ（実行時に指定できる）。
  // contract_tests には AMBIENT_ASSET_DIR がコンパイル時定義で入っているので、
  // 呼び出し側が AMBIENT_ASSET_DIR "/fixtures" を渡せばよい。
  //
  // ★このライブラリ側は AMBIENT_ASSET_DIR を参照しない。ambient_transport 自身は
  //   その定義を持たないため、参照すると「このターゲットでは絶対にコンパイル
  //   されない分岐」を抱えることになる。
  explicit FixtureHttpClient(std::string fixtureDir);

  ~FixtureHttpClient() override = default;

  [[nodiscard]] core::HttpResponse fetch(const core::HttpRequest& request) override;

  // fixture キーを直接引く（fetch を通さない補助）。
  // 未知のキー・読めないファイルは status = 0 / error = Unknown を返し、body に
  // 診断メッセージが入る。例外は投げない。
  [[nodiscard]] core::HttpResponse load(std::string_view fixtureKey) const;

  // URL のパス（例 "/api/presence"）を fixture キーへ割り当てる。既存は上書き。
  void mapPath(std::string path, std::string fixtureKey);

  // --- 呼び出し回数（「1 回の update() で HTTP は最大 1 件」の検証用）--------
  [[nodiscard]] std::size_t callCount() const noexcept { return callCount_; }
  [[nodiscard]] const std::vector<std::string>& requestedUrls() const noexcept {
    return requestedUrls_;
  }
  // 記録だけ消す。パス表は残る。
  void resetCounters() noexcept;

  [[nodiscard]] const std::string& fixtureDir() const noexcept { return fixtureDir_; }

  // --- index.json の写し ------------------------------------------------------
  // 表そのものは .cpp の constexpr 配列。contract_tests の同期テストが
  // index.json との食い違いを検出する。
  [[nodiscard]] static std::vector<FixtureMeta> knownFixtures();
  [[nodiscard]] static std::optional<FixtureMeta> metaFor(std::string_view key) noexcept;

 private:
  std::string fixtureDir_;
  std::map<std::string, std::string, std::less<>> pathToKey_;
  std::vector<std::string> requestedUrls_;
  std::size_t callCount_{0};
};

// ----------------------------------------------------------------------------
// FakeHttpClient — 応答を仕込めるクライアント（異常系の再現用）
// ----------------------------------------------------------------------------
//
// URL 完全一致で応答を仕込む。**呼び出しごとに別の応答**を仕込めるよう、
// 同じ URL への respondWith() / failWith() はキューに積まれる。
//
//   ★消費の規則: 呼ばれるたびに次の要素へ進み、**最後の 1 件は以降ずっと返る**。
//     つまり 1 件だけ仕込めば毎回同じ応答、3 件仕込めば 1→2→3→3→3… となる。
//     「2 回失敗してから成功する」といったリトライ規律の検証がこれで書ける。
//
// どの URL にも一致しなかったとき:
//   1. fallback が設定されていればそちらへ委譲する
//   2. setDefaultResponse() 済みならそれを返す
//   3. どちらも無ければ status = 0 / error = Unknown（body に診断メッセージ）
//      ★空の 200 を返さない。仕込み忘れが静かに通ると最悪なので、大きく失敗する。
class FakeHttpClient final : public core::IHttpClient {
 public:
  FakeHttpClient() = default;

  // fallback は所有しない。呼び出し側が寿命を保証すること
  // （典型的には同じスコープに置いた FixtureHttpClient）。
  explicit FakeHttpClient(core::IHttpClient* fallback) noexcept : fallback_(fallback) {}

  ~FakeHttpClient() override = default;

  [[nodiscard]] core::HttpResponse fetch(const core::HttpRequest& request) override;

  // 任意のステータス・ボディ・Content-Type を仕込む。
  void respondWith(std::string url, core::HttpResponse response);
  void respondWith(std::string url, int status, std::string body, std::string contentType);

  // ★トランスポート失敗（HTTP ステータスが存在しない状態）を仕込む。
  //   status = 0 / error = 指定値。404 や 403 とは **別物**であり、
  //   呼び出し側は error と status の両方で区別できる。
  void failWith(std::string url, core::HttpError error);

  void setDefaultResponse(core::HttpResponse response);

  // --- 呼び出し回数と記録 ---------------------------------------------------
  [[nodiscard]] std::size_t callCount() const noexcept { return requests_.size(); }
  [[nodiscard]] std::size_t callCountFor(std::string_view url) const noexcept;
  [[nodiscard]] const std::vector<core::HttpRequest>& requests() const noexcept {
    return requests_;
  }

  // 記録（呼び出し履歴とキューの読み位置）だけ消す。仕込んだ応答は残る。
  void resetCounters() noexcept;
  // 仕込んだ応答と既定応答を消す。呼び出し履歴は残る。
  void clearResponses() noexcept;

 private:
  core::IHttpClient* fallback_{nullptr};
  std::map<std::string, std::vector<core::HttpResponse>, std::less<>> queued_;
  std::map<std::string, std::size_t, std::less<>> cursor_;
  std::optional<core::HttpResponse> defaultResponse_;
  std::vector<core::HttpRequest> requests_;
};

}  // namespace ambient::transport
