// ambient/transport/hogo_client.hpp — hogo-meter ダッシュボード API クライアント
//
// ★公開ヘッダに curl の型も nlohmann の型も出さないこと。
//   どちらも PRIVATE リンクで、Tab5 では HTTPClient / ArduinoJson へ差し替わる。
//   公開ヘッダに漏れていると差し替えられない。
//
// ★判断ロジックをここに書かないこと。core の SyncPolicy が持つ。
//   ここが持つのは「URL を組み立てて送り、ステータスと生ボディを返す」だけ。
//
// ============================================================================
// ★★ baseUrl はパス接頭辞込みで、正規化しない ★★
// ============================================================================
//
// 上流は素朴な文字列連結で URL を作る (server_sync.cpp:221, 399, 469):
//
//   String url = String(_baseUrl) + "/api/agent/" + _deviceId + "/config";
//
// 実運用値は `https://akebi.dam-lab.work/hogo-meter` であり **パス接頭辞を含む**。
// したがって実パスは `/hogo-meter/api/agent/{deviceId}/config` になる。
//
//   ★末尾スラッシュを付けないこと。付けると `//api/agent/...` になる。
//   ★geoframe_client のように末尾スラッシュを削る正規化を **入れないこと**。
//     上流が正規化しない以上、こちらだけ正規化すると
//     「Mac では通るが Tab5 では 404」という差が生まれる。設定ミスは
//     両方で同じように壊れるのが正しい。
//     （代わりに hogoValidateBaseUrl() で「壊れる設定」を **検出** できるようにしてある。
//       黙って直すのではなく、呼び出し側が起動時に気づけるようにするため。）
//
// ============================================================================
// ★契約の裁定者は firmware ではなく dashboard API
// ============================================================================
//
// firmware は一クライアントに過ぎない。エンドポイントの真の仕様は
// dashboard/src/app/api/agent/[deviceId]/*/route.ts にある。
// file:line と SHA-256 は tests/contract/HOGO_CONTRACT.md に記録してある。要点:
//
//   POST .../records  成功は **204 No Content**（空ボディ）
//                     ボディは **配列ではなく単発 1 件** {"slot":N,"recordedAt":E}
//                     検証順は 404(deviceId 不明) → 401(secret) → 400(body)
//   GET  .../config   {"items":[...]}。firmware / deregister は
//                     **非対象ならキー自体を省略**（省略 = false へリセットが契約）
//
// ============================================================================
// ★secret をログに出さないこと
// ============================================================================
//
// X-Device-Secret はデバイスの資格情報である。診断メッセージ・URL・ボディの
// どこにも出さない。ログへ出すときは redactForLog() を通すこと
// （contract_tests がこれを検査する）。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>

#include "ambient/core/care/care_model.hpp"
#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/care/sync_policy.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/http.hpp"

namespace ambient::transport {

// config 応答の受け入れ上限。
// 出典: server_sync.cpp:268 MAX_CONFIG_RESPONSE_BYTES = 2048
//
// ★上流との差: 上流は Content-Length が明示されたときだけ事前に拒否し、
//   chunked では時間（5 秒タイムアウト）で間接的に抑える (server_sync.cpp:255-274)。
//   こちらは **本文全体に対する硬い上限** である（CurlHttpClient が
//   maxResponseBytes を超えたら TooLarge で切る）。
//   余裕の見積もり: 上流コメントが「items 3 件 + firmware で 600-700 バイト」と
//   実測している。6 項目モードでも約 1100 バイトで、2048 に約 1.9 倍の余裕がある。
inline constexpr std::size_t kHogoMaxConfigResponseBytes = 2048;

// JSON のネスト上限。config 応答は最も深い items[].* でも 3 段しかない。
inline constexpr int kHogoMaxJsonDepth = 16;

// ----------------------------------------------------------------------------
// 結果
// ----------------------------------------------------------------------------

enum class HogoStatus : std::uint8_t {
  Ok = 0,
  // トランスポート失敗（接続不可・タイムアウト・上限超）。httpStatus は 0。
  TransportFailed,
  // 応答は得たが 2xx ではない。★401 / 403 / 404 / 409 / 5xx はすべてここ。
  //   区別は httpStatus を見ること（SyncPolicy は区別せず一律にリトライ減算する。
  //   上流がそうだから。sync_policy.cpp の applyResult を参照）。
  HttpFailed,
  // 2xx だが期待した形ではなかった（JSON でない / items が無い）。
  ParseFailed,
};

[[nodiscard]] const char* toString(HogoStatus status) noexcept;

struct HogoResult {
  HogoStatus status{HogoStatus::Ok};
  int httpStatus{0};
  core::HttpError error{core::HttpError::None};
  std::string contentType;
  std::size_t bodyBytes{0};

  // ★診断用。secret を絶対に入れないこと。ユーザ向けの文言でもない。
  std::string message;

  [[nodiscard]] bool ok() const noexcept { return status == HogoStatus::Ok; }
};

// ----------------------------------------------------------------------------
// 設定
// ----------------------------------------------------------------------------

struct HogoClientConfig {
  // ★パス接頭辞込み・**末尾スラッシュ無し**（例 "https://host/hogo-meter"）。
  //   正規化しない（冒頭の節）。
  std::string baseUrl;

  std::string deviceId;

  // ★ログに出さないこと。
  // ★baseUrl が https:// のときだけ送る。平文 http:// では **送らない**
  //   （上流は送る。差分の理由は hogoValidateBaseUrl() の実装コメント）。
  std::string deviceSecret;

  // Cloudflare Access Service Token。
  // ★両方非空 **かつ baseUrl が https://** のときだけ送る (server_sync.cpp:241-244)。
  //   平文 http では送らない（インターネットで有効な資格情報を平文に流さない）。
  std::string cfAccessClientId;
  std::string cfAccessClientSecret;

  // 実行中のファームウェアバージョン。非空なら config poll に X-FW-Version を付ける。
  std::string fwVersion;

  // セルフ OTA の失敗申告。非空なら config poll に X-FW-Failed: <ver>#<att> を付ける
  // (server_sync.cpp:235-237)。
  std::string failedFwVersion;
  std::uint32_t failedFwAttempt{0};

  core::Millis timeout{core::constants::kHttpTimeout};
  std::size_t maxResponseBytes{kHogoMaxConfigResponseBytes};

  // ---- CA バンドル（★現時点では未配線。下の節を読むこと）------------------
  //
  // Mac では既定でシステムのトラストストアに任せる（CURLOPT_CAINFO を設定しない）。
  // 上流の ca_certs.h は ESP32 用だが、中身は **純 PEM で Arduino 非依存** なので、
  // これを Mac 側でも検証に使えば **Tab5 での CA ピン切れを前倒しで検出できる**。
  //
  // ★★ 未配線の申し送り ★★
  //   この値を CURLOPT_CAINFO_BLOB へ渡すのは src/transport/curl_http_client.cpp の
  //   仕事であり、Wave 4 の所有ファイルではない。**現時点で誰もこの値を読まない。**
  //   loadCaBundleFromEnv() で読み込む経路と検査だけを用意してある。
  //   配線する Wave は curl_http_client.cpp を直すこと（HOGO_CONTRACT.md に記載）。
  std::string caBundlePem;
};

// ----------------------------------------------------------------------------
// URL とボディの組み立て（純粋。HTTP を介さず contract_tests が直接検証する）
// ----------------------------------------------------------------------------
//
// ★素朴な文字列連結。正規化しない（冒頭の節）。

[[nodiscard]] std::string hogoConfigUrl(std::string_view baseUrl, std::string_view deviceId);
[[nodiscard]] std::string hogoRecordsUrl(std::string_view baseUrl, std::string_view deviceId);
[[nodiscard]] std::string hogoDeregisterAckUrl(std::string_view baseUrl,
                                               std::string_view deviceId);

// {"slot":N,"recordedAt":E}
// ★配列ではない。1 リクエスト 1 件（dashboard の agentRecordSchema は
//   単一オブジェクトのみを受け付ける。validation.ts:101-104）。
[[nodiscard]] std::string hogoRecordBody(core::CareItem item, core::WallTime recordedAt);

// baseUrl の設定ミスを **検出** する（黙って直さない。冒頭の節）。
// 問題があれば理由を返し、無ければ空文字列。
//
// ★平文 http:// は **エラーとして返す**（上流は許可する。差分は
//   tests/contract/HOGO_CONTRACT.md の「上流と挙動を変えた点」の表）。
[[nodiscard]] std::string hogoValidateBaseUrl(std::string_view baseUrl);

// baseUrl が https:// か。★Cloudflare トークンだけでなく **X-Device-Secret の
// 付与判定にも使う**（server_sync.cpp の netclient::isHttpsUrl 相当）。
[[nodiscard]] bool hogoIsHttpsUrl(std::string_view baseUrl) noexcept;

// ----------------------------------------------------------------------------
// パーサ（HTTP を介さず単体で叩ける）
// ----------------------------------------------------------------------------
//
// ★渡してよいのは core::successBody() を通った本文だけ。
//   とはいえ HTML / 空 / 途中で切れた入力で落ちてはいけないので、
//   どの経路も false を返すだけにしてある。
//
// ★キー省略の契約: firmware / deregister はキーが無ければ present/false になる。
//   out は毎回まっさらに初期化されるので、「前回の値が残る」ことがない。
[[nodiscard]] bool parseHogoConfigJson(std::string_view body, core::HogoConfigSnapshot& out);

// ----------------------------------------------------------------------------
// ログの秘匿
// ----------------------------------------------------------------------------

inline constexpr std::string_view kRedactedValue = "***";

// 値を伏せるべきヘッダか（大小文字を区別しない）。
// X-Device-Secret / CF-Access-Client-Secret / CF-Access-Client-Id。
[[nodiscard]] bool isSensitiveHeader(std::string_view name) noexcept;

// リクエストを 1 行でログへ出す形にする。★機微ヘッダの値は kRedactedValue に置換。
// POST のボディは記録に slot と epoch しか含まないので、そのまま出す。
[[nodiscard]] std::string redactForLog(const core::HttpRequest& request);

// ----------------------------------------------------------------------------
// CA バンドルの読み込み（★経路のみ。現時点で誰も消費しない）
// ----------------------------------------------------------------------------

struct CaBundleLoad {
  bool present{false};  // 環境変数が設定されていたか
  bool ok{false};       // 読めて PEM に見えたか
  std::string pem;
  std::string message;  // 失敗理由（診断用）
};

// HOGO_CA_BUNDLE を **PEM ファイルへのパス** として読む。
// ★インラインの PEM は受け付けない（環境変数に改行を入れる運用は事故のもとで、
//   「パスにも PEM にも見えない値」を静かに無視する分岐が生まれる）。
// ★PEM に見えない中身は拒否して message に理由を入れる。黙って通すと
//   TLS 時に「証明書検証失敗」としか出ず、原因の切り分けができない。
[[nodiscard]] CaBundleLoad loadCaBundleFromEnv(const char* envName = "HOGO_CA_BUNDLE");

// ----------------------------------------------------------------------------
// HogoClient — 「送る」だけ
// ----------------------------------------------------------------------------

class HogoClient {
 public:
  // http は所有しない。呼び出し側が寿命を保証すること。
  // ★http は **このクライアントを使うスレッド専用** のインスタンスであること
  //   （CurlHttpClient の easy handle はスレッド間で共有できない）。
  HogoClient(core::IHttpClient& http, HogoClientConfig config);

  // GET /api/agent/:id/config。2xx かつ parse 成功で Ok。
  [[nodiscard]] HogoResult pollConfig(core::HogoConfigSnapshot& out);

  // POST /api/agent/:id/records。★成功は 204（2xx をすべて成功とみなす）。
  [[nodiscard]] HogoResult postRecord(core::CareItem item, core::WallTime recordedAt);

  // ---- リクエストの組み立て（送らない。テストとログのために公開）-----------
  [[nodiscard]] core::HttpRequest buildConfigRequest() const;
  [[nodiscard]] core::HttpRequest buildRecordRequest(core::CareItem item,
                                                     core::WallTime recordedAt) const;

  // deviceId / baseUrl が空なら sync を無効化する（上流 server_sync.cpp:213, 372）。
  [[nodiscard]] bool configured() const noexcept;

  [[nodiscard]] const HogoClientConfig& config() const noexcept { return config_; }

 private:
  void addCommonHeaders(core::HttpRequest& request) const;

  core::IHttpClient* http_;
  HogoClientConfig config_;
};

// ----------------------------------------------------------------------------
// HogoSync — SyncPolicy の判断を 1 件だけ実行する薄い駆動
// ----------------------------------------------------------------------------
//
// ★ここに判断を書かないこと。すべて SyncPolicy が決める。
//   このクラスがやるのは
//     1. policy.nextAction() を 1 回呼ぶ
//     2. 返ってきた action を **1 件だけ** 実行する
//     3. policy.applyResult() へ結果を戻す
//   の 3 つだけである。
//
// ★「1 回の update() で HTTP は最大 1 件」はここで初めて観測できる性質である。
//   contract_tests は FakeHttpClient::callCount() の差分で検査すること
//   （nextAction が 1 個返したことを見るだけでは、駆動側の二度撃ちを検出できない）。

struct HogoSyncOutcome {
  core::SyncAction action{};
  bool executed{false};  // HTTP を 1 件実行したか
  HogoResult result{};

  // PollConfig が成功したときだけ true。config は呼び出し側が
  // CareModel::applyConfig() へ渡す（HogoSync は CareModel を知らない）。
  bool haveConfig{false};
  core::HogoConfigSnapshot config{};
};

class HogoSync {
 public:
  HogoSync(core::SyncPolicy& policy, HogoClient& client) noexcept
      : policy_(&policy), client_(&client) {}

  // ★requestDuration は「HTTP に要した時間」。完了時刻 = now + requestDuration を
  //   applyResult() へ渡す。上流が間隔の起点をリクエスト **完了後** の millis() で
  //   取っているため (server_sync.cpp:75, 441)、ここを now にすると
  //   5 秒タイムアウトが効いた場合に次回間隔が詰まる。
  [[nodiscard]] HogoSyncOutcome update(core::SteadyTime now,
                                       core::SyncReadiness ready = {},
                                       core::Millis requestDuration = core::Millis{0});

 private:
  core::SyncPolicy* policy_;
  HogoClient* client_;
};

}  // namespace ambient::transport
