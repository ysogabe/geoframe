// http.cpp — IHttpClient まわりの小さな純粋関数。
//
// クライアントの実装はここには無い。
//   Mac  : ambient_transport（libcurl）
//   Tab5 : ambient_transport の Tab5 版（HTTPClient / WiFiClientSecure）
//   テスト: fixture 実装（レーン 1D）
// 同一のクライアントコードが 3 者すべてを通ることが、この抽象の目的。

#include "ambient/core/http.hpp"

namespace ambient::core {

const char* toString(HttpError error) noexcept {
  switch (error) {
    case HttpError::None:
      return "None";
    case HttpError::Timeout:
      return "Timeout";
    case HttpError::ConnectFailed:
      return "ConnectFailed";
    case HttpError::TlsFailed:
      return "TlsFailed";
    case HttpError::TooLarge:
      return "TooLarge";
    case HttpError::Cancelled:
      return "Cancelled";
    case HttpError::Unknown:
      return "Unknown";
  }
  return "?";
}

std::optional<std::string_view> successBody(const HttpResponse& response) noexcept {
  if (response.error != HttpError::None) {
    return std::nullopt;
  }
  if (!isSuccessStatus(response.status)) {
    // ★403 / 404 は HTML のエラーページが返る。ここで止めるのが本質。
    return std::nullopt;
  }
  return std::string_view{response.body};
}

}  // namespace ambient::core
