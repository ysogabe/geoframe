// ambient_transport の実装（現時点ではバージョン報告のみ）。
//
// このファイルは「空のライブラリを作らない」ためにも必要である。
// macOS の libtool はシンボルを1つも持たない静的ライブラリに対して
// "has no symbols" 警告を出すため、警告ゼロを維持するには実体が要る。

#include "ambient/transport/version.hpp"

#include <curl/curl.h>

namespace ambient::transport {

const char* version_string() noexcept {
  return "0.1.0";
}

const char* curl_runtime_version() noexcept {
  const curl_version_info_data* info = curl_version_info(CURLVERSION_NOW);
  return (info != nullptr && info->version != nullptr) ? info->version : "unknown";
}

}  // namespace ambient::transport
