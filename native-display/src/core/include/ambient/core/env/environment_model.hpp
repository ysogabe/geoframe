// ambient/core/env/environment_model.hpp — 室内環境と気象の解釈（Wave 3B）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★ここが担うのは「生の値 → 人間が読む結論」の写像だけ
// ============================================================================
//
// 描画層はしきい値を知らない（ambient/render/screens.hpp 冒頭が
// 「この翻訳単位に value >= 1500 なら赤 を書いてはいけない」と明言している）。
// したがって、
//
//   * どの device_class をどのラベル・単位で出すか
//   * どの値が「注意」「警告」か
//   * いつ STALE と呼ぶか
//   * 環境カットをいつ画面に出すか（= Env の claim を出す条件）
//
// はすべてここが決める。app 層は EnvView を View Model へ**写すだけ**にすること。
//
// ============================================================================
// ★しきい値の出典（決めた理由もここに書く。この節が正典）
// ============================================================================
//
// CO2 ― **企画書由来**。
//   企画書 §2.1 ENV:「CO2は既存GeoFrameと同じく、1000ppm以上を注意、
//   1500ppm以上を警告として色分けする。」→ kDefaultEnvThresholds の
//   co2CautionPpm / co2WarningPpm はこの値であり、**変えないこと**。
//
// 温度・湿度 ― **企画書に記載が無いのでレーン 3B が決めた**。二段構えにしてある。
//
//   (a) 注意帯は法令の推奨範囲をそのまま使う（値の出典がある）。
//       事務所衛生基準規則（昭和47年労働省令第43号）第5条第3項:
//         室温 18℃以上 28℃以下 / 相対湿度 40%以上 70%以下
//       → この範囲を外れたら Caution。
//   (b) 警告帯は **3B の拡張**（法令には無い）。
//       温度 10℃未満 / 31℃以上、湿度 30%未満 / 80%超。
//       根拠は「注意で済まない健康影響が始まる」ライン:
//         - 31℃以上: 室内での熱中症搬送が増える領域。エアコンの目安として
//           28℃を超えたら注意、30℃台は危険という一般的な啓発に合わせた。
//         - 10℃未満: ヒートショック・低体温の注意領域。
//         - 湿度 30%未満: 乾燥（気道・粘膜）。80%超: カビ・ダニの繁殖域。
//       ★これらは体感の調整対象であり、**企画書由来の CO2 と同格ではない**。
//         変えるならこのコメントごと直すこと（値だけ変えると出典が嘘になる）。
//
// VOC ― 厚生労働省の TVOC 暫定目標値 400 μg/m³ を Caution に採った。
//   Warning 1000 は 3B の拡張（目標値の 2.5 倍 = 換気が明確に必要）。
//   ただし ENV 画面のタイルは先頭 3 件（温度・湿度・CO2）しか描かないので、
//     VOC / 気圧の level は現時点では画面に出ない。精度を詰めても表に出ない。
//
// 気圧・電池・未知の class ― しきい値を置かない（常に Normal）。
//   企画書 §5.3「未知の種類も文字列として表示できるようにする」。
//   健康影響のしきい値が存在しないものに無根拠な色を付けない。
//
// ============================================================================
// ★STALE の設計 ― サーバ側キャッシュより短くすると「常時 STALE」になる
// ============================================================================
//
// 何を測っているか: **「最後に成功した取得からの経過時間」**であって、
// センサーが値を測った時刻からの経過ではない。理由は 2 つ:
//
//   1. /api/env にはタイムスタンプが無い。
//   2. /api/roomenv はトップレベルに `ts`（HA から取った時刻）を持つが、
//      現在の parseRoomEnvJson がそれを捨てている（RoomEnvSnapshot に
//      フィールドが無い）。★roomenv.hpp の担当が ts を通したら、
//      ここを「センサー時刻からの経過」へ差し替えられる（申し送り）。
//
// したがって「値の実年齢 ≦ しきい値 + サーバ側キャッシュ」になる。
// しきい値をキャッシュ周期より短くすると **取得成功直後から STALE** になる。
//
//   /api/env    : サーバが **300 秒** キャッシュする
//                 （app/geoframe/jma.py:88、企画書 §5.4 も明記）。
//                 取得成功時点で既に最大 300 秒古い。ポーリング間隔 30 秒を
//                 足して最悪 330 秒。→ しきい値 **900 秒（15 分）**。
//                 キャッシュ世代 2 つぶんの余裕がある。天気は分単位で変わる
//                 情報ではないので 15 分でも誤解を生まない。
//                 ★600 秒だと余裕が 1.8 倍しかなく、ポーリングが 1 回
//                   落ちただけで STALE が点滅する。
//   /api/roomenv: サーバは HA の値が 55 秒より古いときだけ取り直す
//                 （app/geoframe/ha.py:96）。ポーリング間隔 30 秒を
//                 足して最悪 85 秒。→ しきい値 **180 秒（3 分）**。
//                 最悪値の 2 倍強。HA やサーバが落ちたら 3 分で気づける。
//
// ★STALE は「数値を残したまま」出す（企画書 §2.1 ENV /
//   §10「Home Assistant不達 → 最終値にSTALE表示。写真表示は継続」）。
//   ★Degraded にはしない。ここが Degraded を立てると、通信が切れただけで
//     写真が「未実装」画面に置き換わる（screens.cpp の draw_screen が
//     Degraded を draw_unimplemented_screen へ回している）。
//
// ============================================================================
// ★ok:false（Home Assistant 未設定）は capability-gate であって障害ではない
// ============================================================================
//
// roomenv.hpp が明言しているとおり、ok:false は「機能なし」である。
//   * Degraded にしない
//   * STALE にしない（そもそも「古い値」が存在しない）
//   * カット自体を出さない（room の tile が 0 枚になるだけ）
// HA を使っていない家庭では常にこの状態で、そこで永続的に「通信異常」を
// 出す端末は壊れて見える。roomEnvNotConfigured() で観測できるようにしてある。
//
// ============================================================================
// ★環境カットのスケジュール ― これも「判断」なので core にある
// ============================================================================
//
// Env は写真より強い（ClaimPriority::Env > Photo）ので、**常時 claim すると
// 写真が二度と出ない**。環境カットは「写真の間に挟む全画面情報カード」
// （企画書 §2.1 ENV）なので、周期的に一定時間だけ claim する。
//
//   既定: interval 10 分（企画書 §9 の ENV_CARD_INTERVAL_MIN=10）
//         duration 20 秒（同 PHOTO_DURATION_SEC=20 = 写真 1 枠ぶん）
//
// ★最初のカットは「最初に取得できた瞬間」から始まる（anchor = 初回 ingest）。
//   起動して 10 分待たないと環境が一度も見られない、という体験を避けるため。
//   これは product の振る舞いであって、テストのための細工ではない。
//
// ★collect() は const で、now の純粋な関数である（レベルトリガ）。
//   「1 回出して消える」実装にしないこと（claim.hpp の契約）。

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/model/presence.hpp"
#include "ambient/core/model/roomenv.hpp"
#include "ambient/core/state/claim.hpp"

namespace ambient::core {

// ----------------------------------------------------------------------------
// 値の水準（しきい値判定の **結果**）
// ----------------------------------------------------------------------------

enum class EnvLevel : std::uint8_t {
  Normal = 0,
  Caution,
  Warning,
};

[[nodiscard]] const char* toString(EnvLevel level) noexcept;

// device_class を、こちらが意味を知っている種類へ落としたもの。
// ★Other は「未知だが表示はする」。捨てないこと（企画書 §5.3）。
enum class EnvMetricKind : std::uint8_t {
  Temperature = 0,
  Humidity,
  CarbonDioxide,
  VolatileOrganicCompounds,
  Pressure,
  Battery,
  Other,
};

[[nodiscard]] const char* toString(EnvMetricKind kind) noexcept;

// Home Assistant の device_class 文字列 → EnvMetricKind。
// ★実測した class: temperature / humidity / carbon_dioxide /
//   volatile_organic_compounds / pressure（assets/fixtures/api/roomenv.json）。
[[nodiscard]] EnvMetricKind classifyDeviceClass(std::string_view deviceClass) noexcept;

// ----------------------------------------------------------------------------
// しきい値（出典は冒頭の節）
// ----------------------------------------------------------------------------
//
// ★温度は 0.1℃ 刻みの整数（deci = 10 倍値）で持つ。浮動小数の比較を
//   境界に持ち込まないため（Mac と Tab5 で最下位ビットが食い違いうる）。
struct EnvThresholds {
  // ★企画書由来。変えないこと。
  int co2CautionPpm{1000};
  int co2WarningPpm{1500};

  // 事務所衛生基準規則 第5条第3項（18〜28℃）
  int tempCautionLowDeci{180};
  int tempCautionHighDeci{280};
  // 3B の拡張
  int tempWarningLowDeci{100};
  int tempWarningHighDeci{310};

  // 事務所衛生基準規則 第5条第3項（40〜70%）
  int humidityCautionLowPercent{40};
  int humidityCautionHighPercent{70};
  // 3B の拡張
  int humidityWarningLowPercent{30};
  int humidityWarningHighPercent{80};

  // 厚労省 TVOC 暫定目標値 400 μg/m³ / Warning は 3B の拡張
  int vocCaution{400};
  int vocWarning{1000};
};

inline constexpr EnvThresholds kDefaultEnvThresholds{};

// ----------------------------------------------------------------------------
// STALE のしきい値（出典と根拠は冒頭の節）
// ----------------------------------------------------------------------------
//
// ★/api/env のサーバ側キャッシュ 300 秒より **必ず長くすること**。
//   短くすると取得成功直後から STALE になり、表示が常に警告状態になる。
inline constexpr Millis kEnvServerCachePeriod{300 * 1000};

struct EnvStaleness {
  Millis room{180 * 1000};      // 3 分（HA 更新 55s + ポーリング 30s の 2 倍強）
  Millis weather{900 * 1000};   // 15 分（サーバキャッシュ 300s の 3 倍）
};

inline constexpr EnvStaleness kDefaultEnvStaleness{};

// ----------------------------------------------------------------------------
// 環境カットのスケジュール
// ----------------------------------------------------------------------------

struct EnvCutSchedule {
  Millis interval{10 * 60 * 1000};  // 企画書 §9 ENV_CARD_INTERVAL_MIN=10
  Millis duration{20 * 1000};       // 企画書 §9 PHOTO_DURATION_SEC=20（写真 1 枠）
};

inline constexpr EnvCutSchedule kDefaultEnvCutSchedule{};

// 表示中のカット 1 回ぶん。
struct EnvCutWindow {
  std::uint32_t index{0};  // 1 始まり。claim の payloadId になる
  SteadyTime begin{};
  SteadyTime end{};
};

// ----------------------------------------------------------------------------
// View Model（描画層へそのまま写せる「結論」）
// ----------------------------------------------------------------------------

struct EnvMetricView {
  EnvMetricKind kind{EnvMetricKind::Other};
  std::string label;  // 例 "室温"
  std::string value;  // 例 "22.2"（単位を含めない）
  std::string unit;   // 例 "℃"
  EnvLevel level{EnvLevel::Normal};
  bool stale{false};
};

struct EnvWeeklyView {
  std::string date;     // 例 "7/31"
  std::string tempMax;  // 例 "32"。不明なら "--"
  std::string tempMin;
};

struct EnvView {
  std::vector<EnvMetricView> metrics;  // 正準順（温度→湿度→CO2→VOC→気圧→電池→その他）
  bool roomAvailable{false};           // HA が設定されていて値がある
  bool roomStale{false};

  bool hasWeather{false};
  bool weatherStale{false};
  std::string weatherArea;
  std::string weatherText;
  std::string weatherTemps;  // 例 "気温 27 / 36 ℃"
  std::string weatherPop;    // 例 "降水確率 20%"
  std::vector<EnvWeeklyView> weekly;

  std::string heading;  // 例 "室内環境と天気"
  std::string hint;     // 例 "室内 14:26 取得 / 天気 14:20 取得"

  // 出すものが何も無い（HA 未設定 かつ 天気も取れていない）。
  [[nodiscard]] bool empty() const noexcept { return metrics.empty() && !hasWeather; }
};

// ----------------------------------------------------------------------------
// EnvironmentModel
// ----------------------------------------------------------------------------
//
// ★単一スレッド前提（Core の規律）。ingest は必ずメインスレッドから呼ぶこと。
//   ポーリングは app / transport 層のスレッドで行い、結果だけを渡す。
class EnvironmentModel final : public IClaimSource {
 public:
  // この source の識別子。AlertEngine::kSourceId と衝突しない値。
  static constexpr std::uint32_t kSourceId = 0x45'4E'56'20;  // 'ENV '

  EnvironmentModel() = default;

  // ---- 設定 ----------------------------------------------------------------

  void setThresholds(const EnvThresholds& thresholds) noexcept { thresholds_ = thresholds; }
  void setStaleness(const EnvStaleness& staleness) noexcept { staleness_ = staleness; }

  // ★duration > interval のような矛盾した設定は黙って直す（clamp）。
  //   0 以下も既定へ戻す。設定ミスで claim が永続化するのを防ぐため。
  void setCutSchedule(const EnvCutSchedule& schedule) noexcept;

  // 「取得時刻」を人が読む形（HH:MM）にするために要る。
  // ★Core は時計もタイムゾーンも引かない。app 層が渡すこと（JST なら 540）。
  void setUtcOffsetMinutes(int minutes) noexcept { utcOffsetMinutes_ = minutes; }

  [[nodiscard]] const EnvThresholds& thresholds() const noexcept { return thresholds_; }
  [[nodiscard]] const EnvStaleness& staleness() const noexcept { return staleness_; }
  [[nodiscard]] const EnvCutSchedule& cutSchedule() const noexcept { return cut_; }

  // ---- 入力 ----------------------------------------------------------------

  // /api/roomenv の結果。
  // ★ok:false（NotConfigured）でも呼んでよい。障害ではないので
  //   「最後に取れた値」を捨てずに保持し、STALE にもしない。
  void ingestRoomEnv(const RoomEnvSnapshot& snapshot, SteadyTime now, WallTime wallNow);

  // /api/env の結果（天気・週間予報）。地震は AlertEngine の担当なので見ない。
  void ingestEnv(const EnvSnapshot& snapshot, SteadyTime now, WallTime wallNow);

  // /api/presence。night は 3 値。★value_or(false) で潰さないこと。
  void ingestPresence(const PresenceSnapshot& snapshot, SteadyTime now);

  // ---- 時間跳躍（スリープ復帰 / NTP 同期） ---------------------------------
  //
  // ★これが無いと、8 時間スリープして復帰した直後に
  //   「8 時間前の値」が STALE にならない（単調時刻が進んでいないため）。
  //   clock.hpp の rebaseForJump() のポリシーに従う。
  void onTimeJump(const TimeJump& jump) noexcept;

  // ---- 出力（レベル） ------------------------------------------------------

  // 環境カットの表示中なら Env の claim を 1 つ出す。
  // ★const。now の純粋な関数であること。
  void collect(SteadyTime now, std::vector<Claim>& out) const override;

  // 表示用の結論一式。
  [[nodiscard]] EnvView view(SteadyTime now) const;

  // ---- 検査用 --------------------------------------------------------------

  [[nodiscard]] bool hasAnyData() const noexcept { return anchored_; }
  [[nodiscard]] bool roomEnvNotConfigured() const noexcept { return roomNotConfigured_; }
  [[nodiscard]] bool roomStale(SteadyTime now) const noexcept;
  [[nodiscard]] bool weatherStale(SteadyTime now) const noexcept;
  [[nodiscard]] std::optional<bool> night() const noexcept { return night_; }

  // now がカットの表示中ならその窓を返す。★純粋関数。
  [[nodiscard]] std::optional<EnvCutWindow> cutWindowAt(SteadyTime now) const noexcept;

 private:
  void anchorIfNeeded(SteadyTime now) noexcept;
  [[nodiscard]] EnvLevel levelOf(EnvMetricKind kind, std::string_view value) const noexcept;

  EnvThresholds thresholds_{kDefaultEnvThresholds};
  EnvStaleness staleness_{kDefaultEnvStaleness};
  EnvCutSchedule cut_{kDefaultEnvCutSchedule};
  int utcOffsetMinutes_{0};

  std::vector<RoomEnvItem> roomItems_;
  bool roomNotConfigured_{true};
  bool roomEverReceived_{false};
  SteadyTime roomAt_{};
  WallTime roomWall_{};

  std::optional<WeatherSummary> weather_;
  std::vector<WeeklyDay> weekly_;
  bool weatherEverReceived_{false};
  SteadyTime weatherAt_{};
  WallTime weatherWall_{};

  std::optional<bool> night_;

  // 環境カットの起点。最初に「出せる内容」が揃った瞬間。
  SteadyTime anchor_{};
  bool anchored_{false};
};

// ----------------------------------------------------------------------------
// 小さな変換（テストから直接叩けるよう公開してある）
// ----------------------------------------------------------------------------

// 10 進小数の文字列を「scale 倍の整数」にする。
//   parseDecimalScaled("22.18", 10) -> 222   （四捨五入）
//   parseDecimalScaled("1167", 1)   -> 1167
//   parseDecimalScaled("", 10)      -> nullopt
//
// ★std::stod / std::strtod を使わない。前者は例外、後者はロケール依存
//   （LC_NUMERIC が "," の環境で小数点を読み損ねる）。
[[nodiscard]] std::optional<std::int64_t> parseDecimalScaled(std::string_view text,
                                                             int scale) noexcept;

// scale 倍の整数を、小数点以下 decimals 桁の文字列へ戻す。
//   formatScaled(222, 1) -> "22.2"
//   formatScaled(1167, 0) -> "1167"
//
// ★snprintf("%.1f") を使わない。%f はロケールの小数点記号を使うため、
//   環境によって "22,2" になる（state_machine.hpp / alert_engine.cpp と同じ理由）。
[[nodiscard]] std::string formatScaled(std::int64_t scaled, int decimals);

// "2026-07-31" -> "7/31"。読めない形式は入力をそのまま返す。
[[nodiscard]] std::string shortDate(std::string_view isoDate);

}  // namespace ambient::core
