// 5画面（PHOTO / CARE / CARE_DUE / ALERT / ENV）の描画インターフェース。
//
// ============================================================================
// ★この層は「判断」をしない
// ============================================================================
//
// ここにあるのは **描くための入力（View Model）** であって、生データではない。
//
//   * お世話の Green/Yellow/Red は **サーバが計算して返す**
//     （ambient/core/constants.hpp:140-142）。しきい値を Mac 側で
//     再実装してはいけないので、`CareCard::status` は「既に決まった結論」を
//     受け取るフィールドである。
//   * CO2 の 1000ppm 注意 / 1500ppm 警告のような判定も同様に
//     `EnvMetric::level` として **決まった結論** を受け取る。
//     この翻訳単位に「value >= 1500 なら赤」を書いてはいけない。
//   * 「Red なら CARE を出す」のような表示状態の選択は ambient_core の
//     decide() の責務。ここは `DisplayStateKind` を受け取って描くだけ。
//
// この層がやってよいのは「結論 → 色・字の大きさ・配置」への写像だけである。
//
// ============================================================================
// 型について
// ============================================================================
//
// あえて ambient_core の model 型（Quake / RoomEnvItem 等）を直接受け取らない。
// 描画に必要なのは「整形済みの文字列」であって、生の数値や単位ではないため、
// core 側のパース結果の形が変わってもこの層は影響を受けない方がよい。
// 変換（core の model → ここの View Model）は app 層の責務とする。
//
// ★震度（maxi）は **文字列のまま** 受け取ること。"5-" / "5+" のような
//   弱強の別があり、整数化すると静かに情報が消える
//   （ambient/core/constants.hpp:84-85）。

#pragma once

#include <SDL.h>

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "ambient/core/state/display_state.hpp"
#include "ambient/render/display.hpp"
#include "ambient/render/image.hpp"
#include "ambient/render/text.hpp"

namespace ambient::render {

// ---------------------------------------------------------------------------
// 共通
// ---------------------------------------------------------------------------

// 論理座標系の大きさ。既定は 1280x720（Display の論理解像度と一致させる）。
struct ScreenLayout {
  int width = 1280;
  int height = 720;
};

// 画面ごとに使うフォント一式。
//
// SDL_ttf は「開いたときの point size」でしか描けない（描画時にサイズを
// 指定する API が無い）ので、必要な字の大きさぶんだけ Font を開いて渡す。
// 数メートル離れて読めることが要件なので、想定サイズを名前に書いておく。
struct ScreenFonts {
  const Font* hero = nullptr;     // 約150pt: 震度など単発の巨大数値
  const Font* value = nullptr;    // 約108pt: 室温・CO2 などの計測値
  const Font* title = nullptr;    // 約56pt : 画面見出し・写真タイトル
  const Font* label = nullptr;    // 約40pt : カードのラベル・強調行
  const Font* body = nullptr;     // 約30pt : 本文
  const Font* caption = nullptr;  // 約26pt : 補足（これより小さくしないこと）

  [[nodiscard]] bool complete() const noexcept {
    return hero != nullptr && value != nullptr && title != nullptr &&
           label != nullptr && body != nullptr && caption != nullptr;
  }
};

// ---------------------------------------------------------------------------
// PHOTO
// ---------------------------------------------------------------------------

// 写真に添えるキャプション。空文字列の行は描かない。
struct PhotoCaption {
  std::string title;      // 例 "A町せせらぎ"
  std::string place;      // 例 "A県 A市 A町"
  std::string taken_at;   // 例 "2024-01-15 09:00"
  std::string clock;      // 右上の時計。例 "14:26"
};

// ---------------------------------------------------------------------------
// CARE / CARE_DUE
// ---------------------------------------------------------------------------

// お世話の状態。**サーバの ScheduleStatus（0=None/1=Green/2=Yellow/3=Red）を
// そのまま写したもの**。Mac 側で計算し直さないこと。
enum class CareStatus : std::uint8_t {
  None = 0,
  Green = 1,
  Yellow = 2,
  Red = 3,
};

struct CareCard {
  // hogo-meter の CareItem スロット番号（0=トイレ / 1=目薬 / 2=ご飯 / 3-5 は
  // Tab5 の 6 項目モード専用）。記録の宛先はこの番号で決まるので、
  // ヒットテストは **画面上の並び順ではなくこの値** を返す。
  std::size_t slot = 0;
  std::string label;      // 例 "トイレ"
  std::string elapsed;    // 例 "3時間12分 経過"
  std::string last_time;  // 例 "最終 11:20"
  CareStatus status = CareStatus::None;
};

struct CareScreen {
  std::string heading;             // 例 "お世話の記録"
  std::string hint;                // 右上の補足。例 "タップで記録"
  std::string footer;              // 下部の補足
  std::vector<CareCard> cards;     // 3 項目 or 6 項目（最大 6）
};

// 期限到来の通知。写真の上に重ねる。
struct CareDueOverlay {
  std::string headline;     // 例 "トイレ の時間です"
  std::string detail;       // 例 "前回から 12時間 経過しています"
  std::string elapsed;      // 右側に大きく出す経過時間。例 "12:04"
  std::string action_hint;  // 例 "画面をタップするとお世話画面へ"
  CareStatus status = CareStatus::Red;
};

// ---------------------------------------------------------------------------
// ALERT
// ---------------------------------------------------------------------------

struct AlertScreen {
  std::string title;      // 例 "地震速報"
  std::string intensity;  // ★生の maxi 文字列。"5-" / "5+" をそのまま渡すこと
  std::string magnitude;  // 例 "M6.1"
  std::string place;      // 例 "己県北部"
  std::string occurred;   // 例 "2026-07-30 14:00 発生"
  std::string note;       // 例 "落ち着いて身の安全を確保してください"
};

// ---------------------------------------------------------------------------
// ENV
// ---------------------------------------------------------------------------

// 値の水準。★しきい値判定の **結果** を受け取る。ここで判定しない。
enum class EnvLevel : std::uint8_t {
  Normal = 0,
  Caution,
  Warning,
};

struct EnvMetric {
  std::string label;  // 例 "室温"
  std::string value;  // 例 "23.7"（単位を含めない）
  std::string unit;   // 例 "℃"
  EnvLevel level = EnvLevel::Normal;
  bool stale = false;  // 一定時間更新されていない（数値は残して STALE を出す）
};

struct EnvWeeklyDay {
  std::string date;      // 例 "8/1"
  std::string temp_max;  // 例 "32"。不明なら "--"
  std::string temp_min;  // 例 "24"。不明なら "--"
};

struct EnvScreen {
  std::string heading;       // 例 "室内環境と天気"
  std::string hint;          // 右上の補足
  std::vector<EnvMetric> metrics;  // 先頭 3 件を大きなタイルで出す
  std::string weather_area;  // 例 "仮想北部"
  std::string weather_text;  // 例 "晴れ時々くもり..."
  std::string weather_temps; // 例 "最高 37℃ / 最低 26℃"
  std::string weather_pop;   // 例 "降水確率 20%"
  std::vector<EnvWeeklyDay> weekly;  // 7 件想定。空なら週間帯を描かない
};

// ---------------------------------------------------------------------------
// 5画面ぶんの描画入力をまとめたもの
// ---------------------------------------------------------------------------

struct ScreenData {
  PhotoCaption photo;
  CareScreen care;
  CareDueOverlay care_due;
  AlertScreen alert;
  EnvScreen env;
};

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------
//
// いずれも SDL_RenderClear からは始めない（呼び出し側が背景を用意する）
// ……のではなく、各画面が自分で全面を塗る。画面同士が重ならないようにするため。

void draw_photo_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                       const Texture* background, const PhotoCaption& caption,
                       const ScreenLayout& layout);

void draw_care_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                      const CareScreen& screen, const ScreenLayout& layout);

void draw_care_due_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                          const Texture* background, const PhotoCaption& caption,
                          const CareDueOverlay& overlay, const ScreenLayout& layout);

void draw_alert_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                       const Texture* background, const AlertScreen& screen,
                       const ScreenLayout& layout);

void draw_env_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                     const EnvScreen& screen, const ScreenLayout& layout);

// 表示状態から描画関数へ振り分けるだけの薄い写像。
//
// ★ここに「どの状態を出すか」の判断は入っていない。kind は core の decide() が
//   決めたものを受け取る。Degraded / RecordedAwaitingServer は後続フェーズの
//   担当なので、今は「未実装」と明示する画面を出す（黙って写真を出すと
//   実装漏れが見えなくなる）。
void draw_screen(SDL_Renderer* renderer, core::DisplayStateKind kind,
                 const ScreenFonts& fonts, const Texture* background,
                 const ScreenData& data, const ScreenLayout& layout);

// ---------------------------------------------------------------------------
// タッチ / クリックのヒットテスト
// ---------------------------------------------------------------------------
//
// ★座標は必ず Display::window_to_logical() を通した **論理座標** を渡すこと。
//   SDL のイベントが返す生のウィンドウ座標を渡すとレターボックスぶんズレる。
//   黒帯上のクリックは論理解像度の範囲外（負値や 1280 超）になるが、
//   その場合も矩形に入らないので自然に nullopt になる。

// カードの矩形を計算する。
//
// ★描画とヒットテストは **必ずこの 1 つの関数** を通すこと。
//   描画側が矩形をその場で計算し、ヒットテスト側が別に計算する形にすると、
//   片方だけ直したときに「見えている位置と反応する位置がずれる」という、
//   スクリーンショットでは絶対に見つからない不具合になる。
[[nodiscard]] std::vector<SDL_Rect> care_card_rects(std::size_t card_count,
                                                    const ScreenLayout& layout);

struct CareHit {
  std::size_t index = 0;  // CareScreen::cards の添字（描画順）
  std::size_t slot = 0;   // 記録の宛先スロット（CareCard::slot）
};

// 論理座標がどのカードに当たったかを返す。当たらなければ nullopt。
[[nodiscard]] std::optional<CareHit> hit_test_care(const CareScreen& screen,
                                                   LogicalPoint point,
                                                   const ScreenLayout& layout);

}  // namespace ambient::render
