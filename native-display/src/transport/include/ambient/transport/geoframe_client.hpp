// ambient/transport/geoframe_client.hpp — GeoFrame server の API クライアント
//
// ★公開ヘッダに curl の型も nlohmann の型も出さないこと。
//   どちらも PRIVATE リンクで、Tab5 では esp_http_client / ArduinoJson に
//   差し替わる。公開ヘッダに漏れていると差し替えられない。
//   → パーサは「生の文字列を受け取り、core の model 型を埋める」形にしてある。
//
// ============================================================================
// ★実測が正典 ― 素朴な実装が静かに壊れる 4 か所
// ============================================================================
//
// 1. **/api/photos は裸の JSON 配列**。`{"items": [...]}` ではない。
//    `{items:...}` を期待するパーサは 0 件として動き、**エラーにもならない**。
//
// 2. **`ok` フィールドの有無がエンドポイントごとに非対称**:
//        env / stats / presence / photos … **無し**
//        roomenv / admin-photos      … **あり**
//    共通の「ok を見て成否を判定する」ヘルパを書くと env / stats で必ず落ちる。
//    成否は **HTTP ステータス** で判定する（core/http.hpp の successBody()）。
//
// 3. **エラー応答は JSON とは限らない**。Host 検証の 403、静的ホワイトリスト外の
//    403、404 は **HTML** が返る。ステータスで分岐してから parse すること。
//    このクライアントは successBody() を通った本文しかパーサへ渡さない。
//
// 4. **roomenv の `ok:false` は Home Assistant 未設定を意味する**。
//    ★障害ではない。Degraded にも STALE にもしない。
//    GeoFrameStatus::NotConfigured という別の値で表現してある
//    （`if (!result.ok()) degrade();` と書けないようにするため）。
//
// ============================================================================
// ★パースの安全（nlohmann/README.md の「セキュリティ上の前提」）
// ============================================================================
//
// パース対象は **認証の無い LAN 上のサーバから来る信頼できない入力** である。
//
//   * 例外を有効にしないパース経路を使う
//     （`parse(..., nullptr, false)` + `is_discarded()` 検査）
//   * **入力サイズに上限**（GeoFrameConfig::maxResponseBytes → HttpRequest へ）
//   * **ネストの深さに上限**（kMaxJsonDepth。深い入れ子でスタックを枯らす攻撃）
//   * 型が期待と違っても落ちない。写真 1 件が壊れていてもその 1 件を捨てて続ける
//     （1 件のゴミで 1800 件の manifest が丸ごと消えるほうが被害が大きい）

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/http.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/model/photo.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"

namespace ambient::transport {

// JSON のネスト上限。GeoFrame の応答は最も深い /api/env でも 4 段しかない。
// 16 は十分な余裕を見た値で、これを超える入力は攻撃か壊れた応答である。
inline constexpr int kMaxJsonDepth = 16;

// 応答の受け入れ上限（GeoFrameConfig::maxResponseBytes の既定値）。
// ★下の件数上限の妥当性がこの値からの逆算なので、名前付き定数にしてある。
inline constexpr std::size_t kDefaultMaxResponseBytes = 4U * 1024 * 1024;

// ----------------------------------------------------------------------------
// ★配列の件数に上限を掛ける ― reserve によるメモリ増幅の遮断
// ----------------------------------------------------------------------------
//
// 【何が起きるか】
// 偽サーバが `/api/photos` に `[0,0,0,...]` を **maxResponseBytes ちょうど**
// (4MB) 返すと、要素は "0," の 2 バイト刻みなので `root.size()` は約 210 万になる。
// 「オブジェクトでない要素は捨てる」フィルタは **reserve より後ろ**なので、
// 1 件も採用しないまま `reserve(2100000)` が走り、単一の確保で数百 MB になる。
//
// Mac では仮想アドレスの確保だけなので気づけない。**顕在化するのは Tab5
// (ESP32-P4)** で、オーバーコミットが無いため `std::bad_alloc` が飛ぶ。
//
// 【上限の決め方 ― 満たすべき不変条件】
//   上限件数 × sizeof(要素) <= kDefaultMaxResponseBytes
//
// すなわち **確保量が入力サイズを超えない**（増幅率 < 1）。数字そのものではなく
// この不変条件が根拠であり、.cpp 側で static_assert として機械的に検査している
// （sizeof は riscv32 の libstdc++ では Mac と違う。数字を覚え込ませないこと）。
//
// 各値の設計根拠:
//   写真     : 約2000件を想定し、8192件まで余裕を持たせる。
//   地震     : /api/env のquakesは気象庁の24時間窓。256件まで許容する。
//   週間予報 : 通常7日ぶんだが、32件まで許容する。
//   気温     : 通常は最高/最低の2件。★reserveが無くpush_backのみだが、
//              上限が無ければ結局同じだけ増える。
//   室内環境 : Home Assistantのエンティティ。128件まで許容する。
//
// ★上限に達したら **その先を読まずに打ち切る**（採用件数にも上限を掛ける）。
//   reserve だけ抑えても push_back の繰り返しで結局増えるため。
inline constexpr std::size_t kMaxPhotoItems = 8192;
inline constexpr std::size_t kMaxQuakeItems = 256;
inline constexpr std::size_t kMaxWeeklyDays = 32;
inline constexpr std::size_t kMaxWeatherTemps = 16;
inline constexpr std::size_t kMaxRoomEnvItems = 128;

// ----------------------------------------------------------------------------
// 結果
// ----------------------------------------------------------------------------

enum class GeoFrameStatus : std::uint8_t {
  Ok = 0,
  // トランスポート失敗（接続不可・タイムアウト・上限超）。status は 0。
  TransportFailed,
  // 応答は得たが 2xx ではない。★本文は HTML のことがある。
  HttpFailed,
  // 2xx だが期待した形ではなかった（JSON でない / 型が違う）。
  ParseFailed,
  // ★roomenv の ok:false = Home Assistant 未設定。**障害ではない**。
  NotConfigured,
};

[[nodiscard]] const char* toString(GeoFrameStatus status) noexcept;

struct GeoFrameResult {
  GeoFrameStatus status{GeoFrameStatus::Ok};
  int httpStatus{0};
  core::HttpError error{core::HttpError::None};
  std::string contentType;   // サーバが返したまま（正規化しない）
  std::size_t bodyBytes{0};
  std::string message;       // 診断用。ユーザ向けの文言ではない

  [[nodiscard]] bool ok() const noexcept { return status == GeoFrameStatus::Ok; }

  // ★「通信が壊れている」と言ってよいか。
  //   NotConfigured を **含めない** のがこの述語の存在理由である。
  //   HA を使っていない家庭で永続的に通信異常を出す端末は壊れて見える。
  [[nodiscard]] bool degraded() const noexcept {
    return status == GeoFrameStatus::TransportFailed ||
           status == GeoFrameStatus::HttpFailed || status == GeoFrameStatus::ParseFailed;
  }
};

// ----------------------------------------------------------------------------
// パーサ（HTTP を介さず単体で叩ける ― contract_tests がこれを直接検証する）
// ----------------------------------------------------------------------------
//
// ★渡してよいのは successBody() を通った本文だけ。
//   とはいえ「HTML を渡されたら落ちる」実装であってはならないので、
//   どのパーサも HTML / 空 / 途中で切れた入力に対して false を返すだけにしてある。

// /api/photos。★裸の配列。成功したら rebuildIndex() 済みの manifest が入る。
[[nodiscard]] bool parsePhotosJson(std::string_view body, core::PhotoManifest& out);

// /api/env。★トップレベルに ok は無い。
[[nodiscard]] bool parseEnvJson(std::string_view body, core::EnvSnapshot& out);

// /api/roomenv。★ok:false は NotConfigured（障害ではない）として out に入る。
//   JSON として読めた場合に true を返す（ok の真偽とは別）。
[[nodiscard]] bool parseRoomEnvJson(std::string_view body, core::RoomEnvSnapshot& out);

// /api/presence。★night は true / false / null の 3 値。
[[nodiscard]] bool parsePresenceJson(std::string_view body, core::PresenceSnapshot& out);

// ----------------------------------------------------------------------------
// クライアント
// ----------------------------------------------------------------------------

struct GeoFrameConfig {
  // ★IP リテラルを使うこと。
  //   GeoFrame server は Host ヘッダを検証しており、IP リテラルは無条件で通る。
  //   ホスト名だと Host 検証で 403（しかも本文は HTML）になりうる。
  std::string baseUrl{"http://127.0.0.1:8080"};

  core::Millis timeout{core::constants::kHttpTimeout};

  // 応答の受け入れ上限。約2000件規模の/api/photosを安全に扱える余裕を持たせる。
  // ★Tab5 は応答をまるごと RAM に載せる。無制限にしないこと。
  std::size_t maxResponseBytes{kDefaultMaxResponseBytes};
};

class GeoFrameClient {
 public:
  // http は所有しない。呼び出し側が寿命を保証すること。
  // ★http は **このクライアントを使うスレッド専用** のインスタンスであること
  //   （CurlHttpClient の easy handle はスレッド間で共有できない）。
  GeoFrameClient(core::IHttpClient& http, GeoFrameConfig config);

  [[nodiscard]] GeoFrameResult fetchPhotos(core::PhotoManifest& out);
  [[nodiscard]] GeoFrameResult fetchEnv(core::EnvSnapshot& out);
  [[nodiscard]] GeoFrameResult fetchRoomEnv(core::RoomEnvSnapshot& out);
  [[nodiscard]] GeoFrameResult fetchPresence(core::PresenceSnapshot& out);

  // 写真の絶対 URL。★PhotoRelPath は先頭スラッシュ無しなので joinUrl が補う。
  [[nodiscard]] std::string photoUrl(const core::PhotoRelPath& file) const;

  [[nodiscard]] const std::string& baseUrl() const noexcept { return config_.baseUrl; }
  [[nodiscard]] const GeoFrameConfig& config() const noexcept { return config_; }

 private:
  // GET して、ステータスで分岐して、成功時だけ本文を out へ渡す。
  [[nodiscard]] GeoFrameResult get(std::string_view path, std::string& body);

  core::IHttpClient* http_;
  GeoFrameConfig config_;
};

}  // namespace ambient::transport
