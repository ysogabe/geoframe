#include "ambient/transport/curl_http_client.hpp"

#include <curl/curl.h>

#include <cstdint>
#include <string>
#include <utility>

namespace ambient::transport {
namespace {

// 応答を受け取りながら上限を検査する。
//
// ★Content-Length を信じないこと。chunked 応答では存在しないし、嘘をつくことも
//   できる。実際に受け取ったバイト数で打ち切るのが唯一確実な防御である。
//   （CURLOPT_MAXFILESIZE_LARGE も併用するが、あれは Content-Length ベースなので
//     早期に弾けるときだけ効く「おまけ」である。）
struct WriteSink {
  std::string* body{nullptr};
  std::size_t limit{0};
  bool exceeded{false};
};

std::size_t writeCallback(char* data, std::size_t size, std::size_t nmemb,
                          void* userdata) {
  auto* sink = static_cast<WriteSink*>(userdata);
  const std::size_t chunk = size * nmemb;
  if (sink == nullptr || sink->body == nullptr) {
    return 0;
  }
  if (sink->body->size() + chunk > sink->limit) {
    sink->exceeded = true;
    // 0 を返すと libcurl は CURLE_WRITE_ERROR で転送を中断する。
    return 0;
  }
  sink->body->append(data, chunk);
  return chunk;
}

core::HttpError mapCurlError(CURLcode code) noexcept {
  switch (code) {
    case CURLE_OPERATION_TIMEDOUT:
      return core::HttpError::Timeout;
    case CURLE_COULDNT_CONNECT:
    case CURLE_COULDNT_RESOLVE_HOST:
    case CURLE_COULDNT_RESOLVE_PROXY:
    case CURLE_INTERFACE_FAILED:
      return core::HttpError::ConnectFailed;
    case CURLE_SSL_CONNECT_ERROR:
    case CURLE_PEER_FAILED_VERIFICATION:
    case CURLE_SSL_CERTPROBLEM:
    case CURLE_SSL_CIPHER:
    case CURLE_SSL_CACERT_BADFILE:
    case CURLE_SSL_ISSUER_ERROR:
      return core::HttpError::TlsFailed;
    case CURLE_FILESIZE_EXCEEDED:
      return core::HttpError::TooLarge;
    case CURLE_ABORTED_BY_CALLBACK:
      return core::HttpError::Cancelled;
    default:
      return core::HttpError::Unknown;
  }
}

long toCurlMillis(core::Millis value, core::Millis fallback) noexcept {
  const std::int64_t raw = (value.value > 0) ? value.value : fallback.value;
  return static_cast<long>(raw);
}

// curl_global_init は **スレッドセーフではない**。多重初期化を防ぐために
// 呼び出し回数を数えるだけの、極めて素朴な管理をする。
// ★このカウンタ自体もメインスレッドからしか触らない前提である
//   （ヘッダに「メインスレッドから、他のスレッドを起こす前に」と書いてある）。
int g_globalInitCount = 0;

}  // namespace

// ---------------------------------------------------------------------------
// Impl — ★CURL* はここから外に出さない
// ---------------------------------------------------------------------------

struct CurlHttpClient::Impl {
  CURL* handle{nullptr};
  char errorBuffer[CURL_ERROR_SIZE]{};

  Impl() = default;
  Impl(const Impl&) = delete;
  Impl& operator=(const Impl&) = delete;

  ~Impl() {
    if (handle != nullptr) {
      curl_easy_cleanup(handle);
    }
  }
};

// ---------------------------------------------------------------------------
// グローバル初期化
// ---------------------------------------------------------------------------

bool CurlHttpClient::globalInit(std::string& error) noexcept {
  if (g_globalInitCount > 0) {
    ++g_globalInitCount;
    return true;
  }
  const CURLcode code = curl_global_init(CURL_GLOBAL_DEFAULT);
  if (code != CURLE_OK) {
    error = std::string("curl_global_init に失敗: ") + curl_easy_strerror(code);
    return false;
  }
  g_globalInitCount = 1;
  return true;
}

void CurlHttpClient::globalShutdown() noexcept {
  if (g_globalInitCount <= 0) {
    return;
  }
  --g_globalInitCount;
  if (g_globalInitCount == 0) {
    curl_global_cleanup();
  }
}

// ---------------------------------------------------------------------------
// 生成 / 破棄
// ---------------------------------------------------------------------------

CurlHttpClient::CurlHttpClient(std::unique_ptr<Impl> impl, CurlHttpConfig config) noexcept
    : impl_(std::move(impl)), config_(std::move(config)) {}

CurlHttpClient::~CurlHttpClient() = default;

std::unique_ptr<CurlHttpClient> CurlHttpClient::create(CurlHttpConfig config,
                                                       std::string& error) {
  auto impl = std::make_unique<Impl>();
  impl->handle = curl_easy_init();
  if (impl->handle == nullptr) {
    error = "curl_easy_init に失敗しました（globalInit() を先に呼びましたか）。";
    return nullptr;
  }
  // ★private コンストラクタなので make_unique が使えない。
  return std::unique_ptr<CurlHttpClient>(
      new CurlHttpClient(std::move(impl), std::move(config)));
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

core::HttpResponse CurlHttpClient::fetch(const core::HttpRequest& request) {
  core::HttpResponse response;

  if (impl_ == nullptr || impl_->handle == nullptr) {
    response.error = core::HttpError::Unknown;
    response.body = "CurlHttpClient が初期化されていません。";
    return response;
  }
  if (request.url.empty()) {
    response.error = core::HttpError::Unknown;
    response.body = "URL が空です。";
    return response;
  }

  CURL* curl = impl_->handle;
  curl_easy_reset(curl);
  impl_->errorBuffer[0] = '\0';

  const std::size_t limit = (request.maxResponseBytes > 0)
                                ? request.maxResponseBytes
                                : config_.defaultMaxResponseBytes;

  WriteSink sink;
  sink.body = &response.body;
  sink.limit = limit;

  curl_easy_setopt(curl, CURLOPT_URL, request.url.c_str());
  curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, writeCallback);
  curl_easy_setopt(curl, CURLOPT_WRITEDATA, &sink);
  curl_easy_setopt(curl, CURLOPT_ERRORBUFFER, impl_->errorBuffer);

  // ★マルチスレッドで使うため必須。名前解決のタイムアウトに
  //   alarm()/シグナルを使わせない。
  curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

  // ★リダイレクトを追わない（SSRF 対策。ヘッダのコメントを参照）。
  curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 0L);

  // ★TLS の検証を **明示する**。
  //   curl_easy_reset() が既定（どちらも検証あり）へ戻すので現状も検証は効いて
  //   いるが、hogo-meter は HTTPS で長期の資格情報（X-Device-Secret）を運ぶ。
  //   「既定に依存している」状態だと、切り分け中に一時的に 0 を入れた行が
  //   残っても誰も気づけない。明示しておけば grep で 1 か所に見える。
  //   ★VERIFYHOST は 2（ホスト名照合あり）。1 は廃止されている値である。
  curl_easy_setopt(curl, CURLOPT_SSL_VERIFYPEER, 1L);
  curl_easy_setopt(curl, CURLOPT_SSL_VERIFYHOST, 2L);

  // ★http / https 以外へ行かせない。
#if defined(CURLOPT_PROTOCOLS_STR)
  curl_easy_setopt(curl, CURLOPT_PROTOCOLS_STR, "http,https");
#else
  curl_easy_setopt(curl, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS);
#endif

  curl_easy_setopt(curl, CURLOPT_TIMEOUT_MS,
                   toCurlMillis(request.timeout, config_.defaultTimeout));
  curl_easy_setopt(curl, CURLOPT_CONNECTTIMEOUT_MS,
                   toCurlMillis(config_.connectTimeout, config_.connectTimeout));
  curl_easy_setopt(curl, CURLOPT_ACCEPT_ENCODING, "");  // 対応する圧縮を自動で申告
  curl_easy_setopt(curl, CURLOPT_USERAGENT, config_.userAgent.c_str());

  // Content-Length で早期に弾けるときだけ効く「おまけ」。本命は writeCallback。
  curl_easy_setopt(curl, CURLOPT_MAXFILESIZE_LARGE, static_cast<curl_off_t>(limit));

  struct curl_slist* headers = nullptr;
  for (const core::HttpHeader& header : request.headers) {
    const std::string line = header.name + ": " + header.value;
    struct curl_slist* appended = curl_slist_append(headers, line.c_str());
    if (appended == nullptr) {
      break;  // 確保に失敗。既に積んだぶんだけで進む（例外は投げない）
    }
    headers = appended;
  }

  if (request.method == core::HttpMethod::Post) {
    curl_easy_setopt(curl, CURLOPT_POST, 1L);
    curl_easy_setopt(curl, CURLOPT_POSTFIELDS, request.body.c_str());
    curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE_LARGE,
                     static_cast<curl_off_t>(request.body.size()));
    if (!request.contentType.empty()) {
      const std::string line = "Content-Type: " + request.contentType;
      struct curl_slist* appended = curl_slist_append(headers, line.c_str());
      if (appended != nullptr) {
        headers = appended;
      }
    }
  } else {
    curl_easy_setopt(curl, CURLOPT_HTTPGET, 1L);
  }

  if (headers != nullptr) {
    curl_easy_setopt(curl, CURLOPT_HTTPHEADER, headers);
  }

  const CURLcode code = curl_easy_perform(curl);

  if (headers != nullptr) {
    curl_slist_free_all(headers);
  }

  if (code != CURLE_OK) {
    // ★status は 0 のまま（応答を得られなかった）。error を見ること。
    response.status = 0;
    response.error = sink.exceeded ? core::HttpError::TooLarge : mapCurlError(code);
    response.contentType.clear();

    // ★★ CURLOPT_ERRORBUFFER の中身を捨てないこと ★★
    //   HttpError は 7 値しかないので、「どのホストへの接続がどう失敗したか」は
    //   ここにしか残らない。設定していながら一度も読まないと、最も診断が必要な
    //   場面で "ConnectFailed" としか出せなくなる。
    //   ★診断は body へ入れる（初期化失敗の経路と同じ流儀）。error != None の
    //     とき successBody() は nullopt を返すので、この文字列がパーサへ渡る
    //     経路は無い（core/http.hpp の「パースの門」）。
    response.body.clear();
    if (sink.exceeded) {
      // ★上限超のときの errorBuffer は "Failed writing body" 等になり、実際の
      //   理由（writeCallback が 0 を返して中断した）と食い違う。こちらを優先する。
      response.body = "応答が受け入れ上限 " + std::to_string(limit) + " バイトを超えました";
    } else {
      response.body = curl_easy_strerror(code);
      if (impl_->errorBuffer[0] != '\0') {
        response.body += ": ";
        response.body += impl_->errorBuffer;
      }
    }
    return response;
  }

  long status = 0;
  curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &status);
  response.status = static_cast<int>(status);
  response.error = core::HttpError::None;

  // ★Content-Type は正規化せずそのまま持つ（http.hpp の契約）。
  const char* contentType = nullptr;
  curl_easy_getinfo(curl, CURLINFO_CONTENT_TYPE, &contentType);
  if (contentType != nullptr) {
    response.contentType = contentType;
  }

  // ★ここで body を parse しないこと。403/404 は HTML が返る。
  //   呼び出し側は successBody() を通すこと（core/http.hpp）。
  return response;
}

}  // namespace ambient::transport
