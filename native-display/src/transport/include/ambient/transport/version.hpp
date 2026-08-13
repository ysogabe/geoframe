// ambient_transport — HTTP 取得層。
//
// 現時点では中身が無い（バージョン文字列のみ）。後続フェーズで
// GeoFrame server / hogo-meter への HTTP クライアントがここに入る。
//
// 【契約】このヘッダに curl の型を出さないこと。Tab5 では libcurl ではなく
// esp_http_client に差し替えるため、curl は実装ファイル内に閉じ込める
// （CMake 側でも ambient_dep_curl は PRIVATE リンクにしてある）。

#pragma once

namespace ambient::transport {

[[nodiscard]] const char* version_string() noexcept;

// リンク時に解決された libcurl のバージョン文字列。
// 起動ログへ出して、macOS SDK の古い .pc（7.64.1 と嘘をつく）を掴んでいないか確認する。
[[nodiscard]] const char* curl_runtime_version() noexcept;

}  // namespace ambient::transport
