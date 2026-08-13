// ambient/transport/curl_http_client.hpp — libcurl による IHttpClient 実装
//
// ★公開ヘッダに curl の型も nlohmann の型も出さないこと。
//   どちらも PRIVATE リンクで、Tab5 では esp_http_client / ArduinoJson に
//   差し替わる。公開ヘッダに漏れていると差し替えられない。
//   → CURL* は pimpl の中に閉じ込めてある。
//
// ============================================================================
// ★★ libcurl の easy handle はスレッド間で共有できない ★★
// ============================================================================
//
// libcurl のドキュメントが明言している制約:
//
//   * 1 つの easy handle を複数スレッドから同時に使ってはいけない
//   * `curl_global_init()` はスレッドセーフではない。他の curl 関数より **前** に、
//     まだスレッドが 1 本しかない時点で呼ぶこと
//   * マルチスレッドで使うときは `CURLOPT_NOSIGNAL = 1` が必須
//     （さもないと名前解決のタイムアウトに alarm/シグナルが使われ、
//       メインスレッドのシグナルハンドラを踏み荒らす）
//
// したがって設計は次のとおり:
//
//   * **1 インスタンス = 1 easy handle = 1 スレッド専用**。
//     スレッドをまたいで同じインスタンスを使わないこと。
//     写真取得ワーカーと API ポーリングスレッドは **別のインスタンス** を持つ。
//   * `globalInit()` / `globalShutdown()` は **メインスレッドから、
//     他のスレッドを起こす前に** 呼ぶこと。
//
// ============================================================================
// 安全側に倒してある設定（変える前に理由を読むこと）
// ============================================================================
//
//   * リダイレクトを **追わない**（CURLOPT_FOLLOWLOCATION = 0）。
//     応答は認証の無い LAN 上のサーバから来る。Location ヘッダを信じて追うと
//     任意のホストへ接続させられる（SSRF）。
//   * プロトコルを http / https に限定する。file:// や gopher:// へ誘導されない。
//   * 応答サイズに上限を掛ける。★Tab5 は応答をまるごと RAM に載せるので、
//     無制限は「サーバ次第で OOM する端末」を意味する。
//   * 接続と全体の両方にタイムアウトを掛ける。全体だけだと、名前解決が
//     詰まったときに 1 tick の停止時間が読めなくなる。

#pragma once

#include <cstddef>
#include <memory>
#include <string>

#include "ambient/core/clock.hpp"
#include "ambient/core/http.hpp"

namespace ambient::transport {

struct CurlHttpConfig {
  // 接続（名前解決 + TCP + TLS ハンドシェイク）に許す時間。
  // HttpRequest::timeout（全体）より短くしておくと、サーバが落ちている間の
  // 待ち時間が短く済み、復帰検知が速くなる。
  core::Millis connectTimeout{2000};

  // HttpRequest::maxResponseBytes が 0（実装既定）のときに使う上限。
  std::size_t defaultMaxResponseBytes{8U * 1024 * 1024};

  // HttpRequest::timeout が 0 のときに使う全体タイムアウト。
  core::Millis defaultTimeout{5000};

  std::string userAgent{"ambient-display/0.1 (+native-display)"};
};

class CurlHttpClient final : public core::IHttpClient {
 public:
  // ★メインスレッドから、他のスレッドを起こす前に 1 回だけ呼ぶこと。
  //   成功したら必ず対で globalShutdown() を呼ぶ（プロセス終了直前）。
  [[nodiscard]] static bool globalInit(std::string& error) noexcept;
  static void globalShutdown() noexcept;

  // 失敗時は nullptr を返し error にメッセージを入れる（例外は投げない）。
  [[nodiscard]] static std::unique_ptr<CurlHttpClient> create(CurlHttpConfig config,
                                                              std::string& error);

  ~CurlHttpClient() override;

  // ★このオブジェクトを作ったスレッドからのみ呼ぶこと。
  [[nodiscard]] core::HttpResponse fetch(const core::HttpRequest& request) override;

  [[nodiscard]] const CurlHttpConfig& config() const noexcept { return config_; }

 private:
  struct Impl;

  CurlHttpClient(std::unique_ptr<Impl> impl, CurlHttpConfig config) noexcept;

  std::unique_ptr<Impl> impl_;  // ★CURL* をここへ閉じ込める
  CurlHttpConfig config_;
};

}  // namespace ambient::transport
