// 5画面（PHOTO / CARE / CARE_DUE / ALERT / ENV）の描画。
//
// 方針: このファイルは「core が決めた表示状態を SDL で描く」だけに留める。
// 何を表示するかの判断は ambient_core の decide() の責務であり、ここに
// 「Red なら CARE を出す」「CO2 が 1500 を超えたら赤」のような分岐を
// 書いてはいけない。結論は View Model（screens.hpp）として渡ってくる。
//
// 数メートル離れて眺めるアンビエント端末なので、字は大きく、
// 背景とのコントラストを確保する。写真の上に文字を置くときは必ず
// 暗い帯かグラデーションを敷く。

#include "ambient/render/screens.hpp"

#include <SDL_ttf.h>

#include <algorithm>

namespace ambient::render {
namespace {

// ---------------------------------------------------------------------------
// 配色
// ---------------------------------------------------------------------------
// 数メートル離れて読むため、彩度より「地と字の明度差」を優先する。

constexpr SDL_Color kBgDark{0x0E, 0x11, 0x16, 0xFF};      // 画面の地
constexpr SDL_Color kPanel{0x1B, 0x21, 0x2B, 0xFF};       // カード・パネル
constexpr SDL_Color kPanelEdge{0x2E, 0x37, 0x45, 0xFF};   // パネルの縁
constexpr SDL_Color kInk{0xF2, 0xF5, 0xF9, 0xFF};         // 主要な文字
constexpr SDL_Color kInkMuted{0xA8, 0xB4, 0xC4, 0xFF};    // 補足の文字

constexpr SDL_Color kStatusGreen{0x3F, 0xBF, 0x6F, 0xFF};
constexpr SDL_Color kStatusYellow{0xE8, 0xB3, 0x3A, 0xFF};
constexpr SDL_Color kStatusRed{0xE5, 0x48, 0x4D, 0xFF};
constexpr SDL_Color kStatusNone{0x6C, 0x78, 0x88, 0xFF};

constexpr SDL_Color kAlertBand{0xC8, 0x1E, 0x28, 0xFF};   // 地震速報の警報色
constexpr SDL_Color kAlertPanel{0x24, 0x0B, 0x0E, 0xFF};

constexpr SDL_Color kBlack{0x00, 0x00, 0x00, 0xFF};

// ---------------------------------------------------------------------------
// 小さな描画ヘルパ
// ---------------------------------------------------------------------------

void fill_rect(SDL_Renderer* renderer, const SDL_Rect& rect, SDL_Color color,
               Uint8 alpha = 0xFF) {
  SDL_SetRenderDrawColor(renderer, color.r, color.g, color.b, alpha);
  SDL_RenderFillRect(renderer, &rect);
}

void draw_outline(SDL_Renderer* renderer, const SDL_Rect& rect, SDL_Color color,
                  int thickness) {
  const SDL_Rect top{rect.x, rect.y, rect.w, thickness};
  const SDL_Rect bottom{rect.x, rect.y + rect.h - thickness, rect.w, thickness};
  const SDL_Rect left{rect.x, rect.y, thickness, rect.h};
  const SDL_Rect right{rect.x + rect.w - thickness, rect.y, thickness, rect.h};
  fill_rect(renderer, top, color);
  fill_rect(renderer, bottom, color);
  fill_rect(renderer, left, color);
  fill_rect(renderer, right, color);
}

// 縦方向の擬似グラデーション。SDL2 にグラデーション API は無いので
// 帯を重ねて近似する。写真の上に文字を置くときの下敷きに使う。
void fill_vertical_scrim(SDL_Renderer* renderer, const SDL_Rect& area, SDL_Color color,
                         int top_alpha, int bottom_alpha) {
  constexpr int kBands = 28;
  for (int i = 0; i < kBands; ++i) {
    const int band_top = area.y + (area.h * i) / kBands;
    const int band_bottom = area.y + (area.h * (i + 1)) / kBands;
    const int alpha = top_alpha + ((bottom_alpha - top_alpha) * i) / (kBands - 1);
    const SDL_Rect band{area.x, band_top, area.w, band_bottom - band_top};
    fill_rect(renderer, band, color, static_cast<Uint8>(std::clamp(alpha, 0, 255)));
  }
}

// 写真の上に文字を置くための下敷き。
//
// ★純粋なグラデーションだけでは足りない。文字が乗る帯の不透明度が
//   0.7 程度に届かないと、明るい写真や被写体に文字が負ける（実測: 合成
//   fixture に焼き込まれた白文字がキャプションを貫通して読めなくなった）。
//   そこで「文字が乗る範囲は一定の濃さ + その外側だけ短くぼかす」形にする。
//   帯の境界が線に見えないよう、ぼかし幅は十分に取ること。
constexpr int kScrimFeather = 64;
constexpr int kScrimAlpha = 205;

void draw_bottom_scrim(SDL_Renderer* renderer, const ScreenLayout& layout,
                       int band_height) {
  const SDL_Rect feather{0, layout.height - band_height - kScrimFeather, layout.width,
                         kScrimFeather};
  fill_vertical_scrim(renderer, feather, kBlack, 0, kScrimAlpha);
  const SDL_Rect band{0, layout.height - band_height, layout.width, band_height};
  fill_rect(renderer, band, kBlack, static_cast<Uint8>(kScrimAlpha));
}

void draw_top_scrim(SDL_Renderer* renderer, const ScreenLayout& layout,
                    int band_height) {
  const SDL_Rect band{0, 0, layout.width, band_height};
  fill_rect(renderer, band, kBlack, static_cast<Uint8>(kScrimAlpha));
  const SDL_Rect feather{0, band_height, layout.width, kScrimFeather};
  fill_vertical_scrim(renderer, feather, kBlack, kScrimAlpha, 0);
}

// TTF_SizeUTF8 の薄いラッパ。text.hpp は中央揃えも計測 API も公開していないので
// ここで直接測る（text.hpp を書き換えるのは他レーンとの共有面を広げるため避ける）。
int text_width(const Font& font, const std::string& utf8_text) {
  if (utf8_text.empty()) {
    return 0;
  }
  int width = 0;
  int height = 0;
  if (TTF_SizeUTF8(font.raw(), utf8_text.c_str(), &width, &height) != 0) {
    return 0;
  }
  return width;
}

TextStyle plain_style(SDL_Color color) {
  TextStyle style;
  style.color = color;
  // パネルの上（地が単色）では影は不要。大きな字がにじむので切る。
  style.drop_shadow = false;
  return style;
}

TextStyle over_photo_style(SDL_Color color) {
  TextStyle style;
  style.color = color;
  style.drop_shadow = true;
  style.shadow_offset = 3;
  style.shadow_color = SDL_Color{0, 0, 0, 200};
  return style;
}

// center_x を中心にして描く。
int draw_text_centered(SDL_Renderer* renderer, const Font& font,
                       const std::string& utf8_text, int center_x, int y,
                       const TextStyle& style) {
  const int width = text_width(font, utf8_text);
  return draw_text(renderer, font, utf8_text, center_x - width / 2, y, style);
}

SDL_Color status_color(CareStatus status) noexcept {
  switch (status) {
    case CareStatus::Green:
      return kStatusGreen;
    case CareStatus::Yellow:
      return kStatusYellow;
    case CareStatus::Red:
      return kStatusRed;
    case CareStatus::None:
      break;
  }
  return kStatusNone;
}

// 状態 → 表示ラベル。**判定ではなく、決まった結論の日本語化**である。
const char* status_label(CareStatus status) noexcept {
  switch (status) {
    case CareStatus::Green:
      return "記録済み";
    case CareStatus::Yellow:
      return "そろそろ";
    case CareStatus::Red:
      return "要対応";
    case CareStatus::None:
      break;
  }
  return "未設定";
}

SDL_Color level_color(EnvLevel level) noexcept {
  switch (level) {
    case EnvLevel::Caution:
      return kStatusYellow;
    case EnvLevel::Warning:
      return kStatusRed;
    case EnvLevel::Normal:
      break;
  }
  return kStatusGreen;
}

// 背景写真。テクスチャが無い場合は暗い地で埋める（黒画面で黙って動かない）。
void draw_background_photo(SDL_Renderer* renderer, const Texture* background,
                           const ScreenLayout& layout) {
  const SDL_Rect full{0, 0, layout.width, layout.height};
  fill_rect(renderer, full, kBgDark);
  if (background != nullptr) {
    draw_cover(renderer, *background, layout.width, layout.height);
  }
}

// 画面の見出し行（左に大見出し、右に補足）。CARE / ENV で共通。
void draw_heading(SDL_Renderer* renderer, const ScreenFonts& fonts,
                  const std::string& heading, const std::string& hint,
                  const ScreenLayout& layout) {
  draw_text(renderer, *fonts.title, heading, 48, 32, plain_style(kInk));
  if (!hint.empty()) {
    // 右上は FPS HUD（app 層）が y=8 付近を使うので、こちらは y=54 から。
    draw_text(renderer, *fonts.caption, hint, layout.width - 48, 54,
              plain_style(kInkMuted), TextAlign::kRight);
  }
  const SDL_Rect rule{48, 116, layout.width - 96, 2};
  fill_rect(renderer, rule, kPanelEdge);
}

// お世話カードのグリッド定義。care_card_rects() と共有する。
struct CareGrid {
  int columns = 3;
  int rows = 1;
  int origin_x = 48;
  int origin_y = 140;
  int card_w = 0;
  int card_h = 0;
  int gap = 20;
};

CareGrid care_grid(std::size_t card_count, const ScreenLayout& layout) {
  CareGrid grid;
  const int count = static_cast<int>(std::min<std::size_t>(card_count, 6));
  grid.columns = (count <= 3) ? std::max(count, 1) : 3;
  grid.rows = (count <= 3) ? 1 : 2;

  const int available_w = layout.width - grid.origin_x * 2;
  grid.card_w = (available_w - grid.gap * (grid.columns - 1)) / grid.columns;

  // 下部にフッタ 1 行ぶんを空ける。
  const int available_h = layout.height - grid.origin_y - 96;
  if (grid.rows == 1) {
    // 1 行のときは間延びさせず、縦中央に置く。
    // 上限は 2 行時よりやや大きい程度に留める（タップ対象としては大きい方が
    // よいが、余白ばかりのカードは情報が薄く見える）。
    grid.card_h = std::min(available_h, 264);
    grid.origin_y += (available_h - grid.card_h) / 2;
  } else {
    grid.card_h = (available_h - grid.gap) / grid.rows;
  }
  return grid;
}

void draw_care_card(SDL_Renderer* renderer, const ScreenFonts& fonts,
                    const CareCard& card, const SDL_Rect& rect) {
  const SDL_Color accent = status_color(card.status);

  fill_rect(renderer, rect, kPanel);
  draw_outline(renderer, rect, kPanelEdge, 2);

  // 左端の状態バー。色だけで判別できるようにしつつ、文字でも書く
  // （色覚特性や暗所での視認性のため、色を唯一の情報源にしない）。
  const SDL_Rect accent_bar{rect.x, rect.y, 14, rect.h};
  fill_rect(renderer, accent_bar, accent);

  const int text_x = rect.x + 40;

  draw_text(renderer, *fonts.label, card.label, text_x, rect.y + 22,
            plain_style(kInk));
  draw_text(renderer, *fonts.label, card.elapsed, text_x, rect.y + 92,
            plain_style(accent));
  draw_text(renderer, *fonts.caption, card.last_time, text_x, rect.y + 150,
            plain_style(kInkMuted));
  draw_text(renderer, *fonts.caption, status_label(card.status), text_x,
            rect.y + 186, plain_style(accent));
}

void draw_env_metric_tile(SDL_Renderer* renderer, const ScreenFonts& fonts,
                          const EnvMetric& metric, const SDL_Rect& rect) {
  const SDL_Color accent = level_color(metric.level);

  fill_rect(renderer, rect, kPanel);
  draw_outline(renderer, rect, kPanelEdge, 2);
  const SDL_Rect accent_bar{rect.x, rect.y, rect.w, 8};
  fill_rect(renderer, accent_bar, accent);

  const int text_x = rect.x + 28;
  draw_text(renderer, *fonts.caption, metric.label, text_x, rect.y + 24,
            plain_style(kInkMuted));

  const int value_y = rect.y + 66;
  const int value_w =
      draw_text(renderer, *fonts.value, metric.value, text_x, value_y,
                plain_style(accent));
  if (!metric.unit.empty()) {
    // 単位は値のベースライン寄りに小さく添える。
    draw_text(renderer, *fonts.label, metric.unit, text_x + value_w + 10,
              value_y + 74, plain_style(kInkMuted));
  }

  if (metric.stale) {
    // 値は残したまま STALE を出す（企画書 §2.1 ENV）。
    draw_text(renderer, *fonts.caption, "STALE", rect.x + rect.w - 24, rect.y + 24,
              plain_style(kStatusYellow), TextAlign::kRight);
  }
}

// 後続フェーズ担当の表示状態を、黙って写真で代替せずに明示する。
void draw_unimplemented_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                               const char* state_name, const ScreenLayout& layout) {
  const SDL_Rect full{0, 0, layout.width, layout.height};
  fill_rect(renderer, full, kBgDark);
  draw_text_centered(renderer, *fonts.title, state_name, layout.width / 2, 280,
                     plain_style(kInk));
  draw_text_centered(renderer, *fonts.body, "この表示状態の画面は後続フェーズで実装します",
                     layout.width / 2, 380, plain_style(kInkMuted));
}

}  // namespace

// ---------------------------------------------------------------------------
// PHOTO
// ---------------------------------------------------------------------------

void draw_photo_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                       const Texture* background, const PhotoCaption& caption,
                       const ScreenLayout& layout) {
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  draw_background_photo(renderer, background, layout);

  // 上部: 時計の下敷き。
  if (!caption.clock.empty()) {
    draw_top_scrim(renderer, layout, 110);
    draw_text(renderer, *fonts.title, caption.clock, layout.width - 48, 24,
              over_photo_style(kInk), TextAlign::kRight);
  }

  // 下部: キャプションの下敷き。
  const bool has_caption = !caption.title.empty() || !caption.place.empty() ||
                           !caption.taken_at.empty();
  if (has_caption) {
    draw_bottom_scrim(renderer, layout, 210);
  }

  int y = layout.height - 196;
  if (!caption.title.empty()) {
    draw_text(renderer, *fonts.title, caption.title, 56, y, over_photo_style(kInk));
    y += 78;
  }
  if (!caption.place.empty()) {
    draw_text(renderer, *fonts.body, caption.place, 56, y, over_photo_style(kInk));
    y += 46;
  }
  if (!caption.taken_at.empty()) {
    draw_text(renderer, *fonts.caption, caption.taken_at, 56, y,
              over_photo_style(kInkMuted));
  }
}

// ---------------------------------------------------------------------------
// CARE
// ---------------------------------------------------------------------------

std::vector<SDL_Rect> care_card_rects(std::size_t card_count,
                                      const ScreenLayout& layout) {
  std::vector<SDL_Rect> rects;
  const std::size_t count = std::min<std::size_t>(card_count, 6);
  if (count == 0) {
    return rects;
  }
  const CareGrid grid = care_grid(count, layout);
  rects.reserve(count);
  for (std::size_t i = 0; i < count; ++i) {
    const int index = static_cast<int>(i);
    const int column = index % grid.columns;
    const int row = index / grid.columns;
    rects.push_back(SDL_Rect{grid.origin_x + column * (grid.card_w + grid.gap),
                             grid.origin_y + row * (grid.card_h + grid.gap),
                             grid.card_w, grid.card_h});
  }
  return rects;
}

std::optional<CareHit> hit_test_care(const CareScreen& screen, LogicalPoint point,
                                     const ScreenLayout& layout) {
  const std::vector<SDL_Rect> rects = care_card_rects(screen.cards.size(), layout);
  for (std::size_t i = 0; i < rects.size(); ++i) {
    const SDL_Rect& rect = rects[i];
    const float left = static_cast<float>(rect.x);
    const float top = static_cast<float>(rect.y);
    const float right = static_cast<float>(rect.x + rect.w);
    const float bottom = static_cast<float>(rect.y + rect.h);
    if (point.x >= left && point.x < right && point.y >= top && point.y < bottom) {
      return CareHit{i, screen.cards[i].slot};
    }
  }
  return std::nullopt;
}

void draw_care_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                      const CareScreen& screen, const ScreenLayout& layout) {
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  const SDL_Rect full{0, 0, layout.width, layout.height};
  fill_rect(renderer, full, kBgDark);

  draw_heading(renderer, fonts, screen.heading, screen.hint, layout);

  const std::vector<SDL_Rect> rects = care_card_rects(screen.cards.size(), layout);
  for (std::size_t i = 0; i < rects.size(); ++i) {
    draw_care_card(renderer, fonts, screen.cards[i], rects[i]);
  }

  if (!screen.footer.empty()) {
    draw_text(renderer, *fonts.caption, screen.footer, 48, layout.height - 56,
              plain_style(kInkMuted));
  }
}

// ---------------------------------------------------------------------------
// CARE_DUE
// ---------------------------------------------------------------------------

void draw_care_due_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                          const Texture* background, const PhotoCaption& caption,
                          const CareDueOverlay& overlay, const ScreenLayout& layout) {
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  // 写真表示は継続したまま、その上に重ねる（企画書 §2.1 CARE_DUE）。
  draw_photo_screen(renderer, fonts, background, PhotoCaption{}, layout);

  // 写真全体をわずかに落として、パネルの外側でも文字が読めるようにする。
  const SDL_Rect full{0, 0, layout.width, layout.height};
  fill_rect(renderer, full, kBlack, 90);

  const SDL_Color accent = status_color(overlay.status);

  const SDL_Rect panel{0, layout.height - 300, layout.width, 300};
  fill_rect(renderer, panel, kBgDark, 238);
  const SDL_Rect accent_bar{panel.x, panel.y, panel.w, 12};
  fill_rect(renderer, accent_bar, accent);

  draw_text(renderer, *fonts.caption, status_label(overlay.status), 56, panel.y + 34,
            plain_style(accent));
  draw_text(renderer, *fonts.title, overlay.headline, 56, panel.y + 72,
            plain_style(kInk));
  draw_text(renderer, *fonts.body, overlay.detail, 56, panel.y + 156,
            plain_style(kInk));
  draw_text(renderer, *fonts.caption, overlay.action_hint, 56, panel.y + 210,
            plain_style(kInkMuted));

  if (!overlay.elapsed.empty()) {
    draw_text(renderer, *fonts.value, overlay.elapsed, layout.width - 56,
              panel.y + 96, plain_style(accent), TextAlign::kRight);
  }

  // 写真のタイトルは上部へ退避させて、通知と競合させない。
  if (!caption.title.empty() || !caption.clock.empty()) {
    draw_top_scrim(renderer, layout, 96);
    draw_text(renderer, *fonts.body, caption.title, 56, 26, over_photo_style(kInk));
    draw_text(renderer, *fonts.title, caption.clock, layout.width - 48, 12,
              over_photo_style(kInk), TextAlign::kRight);
  }
}

// ---------------------------------------------------------------------------
// ALERT
// ---------------------------------------------------------------------------

void draw_alert_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                       const Texture* background, const AlertScreen& screen,
                       const ScreenLayout& layout) {
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  // 背景写真は強く暗転させる。最も緊急度が高い表示なので、写真の情報量を残さない。
  draw_background_photo(renderer, background, layout);
  const SDL_Rect full{0, 0, layout.width, layout.height};
  // ★写真の情報量をほぼ消す。ここは最も緊急度の高い表示で、
  //   背景に読めるものが残っていると震度から目が逸れる。
  fill_rect(renderer, full, kBlack, 242);
  fill_rect(renderer, full, SDL_Color{0x60, 0x08, 0x0C, 0xFF}, 120);

  // 上部の警報バンド。
  const SDL_Rect band{0, 0, layout.width, 104};
  fill_rect(renderer, band, kAlertBand);
  draw_text(renderer, *fonts.title, screen.title, 48, 20, plain_style(kInk));
  if (!screen.occurred.empty()) {
    draw_text(renderer, *fonts.body, screen.occurred, layout.width - 48, 36,
              plain_style(kInk), TextAlign::kRight);
  }

  // 左: 最大震度。★intensity は "5-" / "5+" を含む生の文字列をそのまま描く。
  const SDL_Rect intensity_panel{48, 148, 400, 372};
  fill_rect(renderer, intensity_panel, kAlertPanel);
  draw_outline(renderer, intensity_panel, kAlertBand, 5);

  const int intensity_center = intensity_panel.x + intensity_panel.w / 2;
  draw_text_centered(renderer, *fonts.label, "最大震度", intensity_center,
                     intensity_panel.y + 26, plain_style(kInkMuted));
  draw_text_centered(renderer, *fonts.hero, screen.intensity, intensity_center,
                     intensity_panel.y + 96, plain_style(kInk));

  // 右: 震源地・規模。
  const int right_x = 496;
  draw_text(renderer, *fonts.body, "震源地", right_x, 160, plain_style(kInkMuted));
  draw_text(renderer, *fonts.title, screen.place, right_x, 200, plain_style(kInk));

  draw_text(renderer, *fonts.body, "マグニチュード", right_x, 316,
            plain_style(kInkMuted));
  draw_text(renderer, *fonts.value, screen.magnitude, right_x, 352,
            plain_style(kInk));

  if (!screen.note.empty()) {
    const SDL_Rect note_panel{48, 588, layout.width - 96, 84};
    fill_rect(renderer, note_panel, kAlertPanel);
    draw_outline(renderer, note_panel, kAlertBand, 3);
    draw_text(renderer, *fonts.body, screen.note, note_panel.x + 28,
              note_panel.y + 22, plain_style(kInk));
  }
}

// ---------------------------------------------------------------------------
// ENV
// ---------------------------------------------------------------------------

void draw_env_screen(SDL_Renderer* renderer, const ScreenFonts& fonts,
                     const EnvScreen& screen, const ScreenLayout& layout) {
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  const SDL_Rect full{0, 0, layout.width, layout.height};
  fill_rect(renderer, full, kBgDark);

  draw_heading(renderer, fonts, screen.heading, screen.hint, layout);

  // 上段: 計測値タイル（最大 3 枚）。
  const int tile_count =
      static_cast<int>(std::min<std::size_t>(screen.metrics.size(), 3));
  if (tile_count > 0) {
    constexpr int kGap = 20;
    const int margin = 48;
    const int available = layout.width - margin * 2;
    const int tile_w = (available - kGap * (tile_count - 1)) / tile_count;
    for (int i = 0; i < tile_count; ++i) {
      const SDL_Rect rect{margin + i * (tile_w + kGap), 140, tile_w, 244};
      draw_env_metric_tile(renderer, fonts, screen.metrics[static_cast<std::size_t>(i)],
                           rect);
    }
  }

  // 中段: 天気。
  const SDL_Rect weather{48, 404, layout.width - 96, 156};
  fill_rect(renderer, weather, kPanel);
  draw_outline(renderer, weather, kPanelEdge, 2);
  draw_text(renderer, *fonts.label, screen.weather_area, weather.x + 28,
            weather.y + 20, plain_style(kInk));
  draw_text(renderer, *fonts.body, screen.weather_text, weather.x + 28,
            weather.y + 84, plain_style(kInkMuted));
  draw_text(renderer, *fonts.label, screen.weather_temps,
            weather.x + weather.w - 28, weather.y + 20, plain_style(kInk),
            TextAlign::kRight);
  draw_text(renderer, *fonts.body, screen.weather_pop, weather.x + weather.w - 28,
            weather.y + 84, plain_style(kInkMuted), TextAlign::kRight);

  // 下段: 週間予報。7 件想定。
  if (!screen.weekly.empty()) {
    const int days = static_cast<int>(std::min<std::size_t>(screen.weekly.size(), 7));
    const int margin = 48;
    const int strip_w = layout.width - margin * 2;
    const int column_w = strip_w / days;
    for (int i = 0; i < days; ++i) {
      const EnvWeeklyDay& day = screen.weekly[static_cast<std::size_t>(i)];
      const int center = margin + column_w * i + column_w / 2;
      draw_text_centered(renderer, *fonts.caption, day.date, center, 586,
                         plain_style(kInkMuted));
      draw_text_centered(renderer, *fonts.caption,
                         day.temp_max + " / " + day.temp_min, center, 626,
                         plain_style(kInk));
      if (i > 0) {
        const SDL_Rect divider{margin + column_w * i, 586, 2, 74};
        fill_rect(renderer, divider, kPanelEdge);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// 振り分け
// ---------------------------------------------------------------------------

void draw_screen(SDL_Renderer* renderer, core::DisplayStateKind kind,
                 const ScreenFonts& fonts, const Texture* background,
                 const ScreenData& data, const ScreenLayout& layout) {
  // ★各 draw_*_screen も同じ検査を持っているが、ここでも見る。
  //   draw_unimplemented_screen() は private ヘルパで検査を持たないため、
  //   この 1 行が無いと Degraded / RecordedAwaitingServer 経路だけが
  //   null フォントを参照しうる（= 通常運転では通らない経路だけが落ちる）。
  if (renderer == nullptr || !fonts.complete()) {
    return;
  }
  // ★ここは「kind → 描画関数」の写像だけ。kind を選ぶ判断は core の decide()。
  switch (kind) {
    case core::DisplayStateKind::Photo:
      draw_photo_screen(renderer, fonts, background, data.photo, layout);
      return;
    case core::DisplayStateKind::Care:
      draw_care_screen(renderer, fonts, data.care, layout);
      return;
    case core::DisplayStateKind::CareDue:
      draw_care_due_screen(renderer, fonts, background, data.photo, data.care_due,
                           layout);
      return;
    case core::DisplayStateKind::Alert:
      draw_alert_screen(renderer, fonts, background, data.alert, layout);
      return;
    case core::DisplayStateKind::Env:
      draw_env_screen(renderer, fonts, data.env, layout);
      return;
    case core::DisplayStateKind::Degraded:
      draw_unimplemented_screen(renderer, fonts, "DEGRADED（通信断の告知）", layout);
      return;
    case core::DisplayStateKind::RecordedAwaitingServer:
      draw_unimplemented_screen(renderer, fonts,
                                "RECORDED（記録送信済み・サーバ待ち）", layout);
      return;
  }
}

}  // namespace ambient::render
