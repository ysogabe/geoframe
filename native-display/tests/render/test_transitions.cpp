// test_transitions.cpp — 遷移描画の構造アサーション（Wave 2B）
//
// ============================================================================
// ★ピクセル完全一致で「絵」を固定しないこと
// ============================================================================
//
// ソフトウェアレンダラと Metal で結果が differ し、SDL のバージョン差でも変わる。
// 期待画像を焼くと、実装が正しいまま環境差で赤くなる（= 誰も直せないテスト）。
//
// 代わりに「その演出であるために必ず成り立つ構造」を検査する。
//
//   Slide            : progress=0.5 で画面が半分ずつに分かれる（方向に応じて）
//   Wipe             : 境界の位置が progress に線形追従する
//   FadeThroughBlack : progress=0.5 で画面全体の輝度が最小になる
//   CrossFade        : progress の増加とともに新画像の寄与が単調増加する
//   Blinds           : 帯の本数が BlindsParams::slats と一致する
//
// 例外が 2 つある。どちらも「絵」ではなく「同一性」を見るのでピクセル比較でよい。
//
//   * 決定性    : 同じ seed で 2 回計画 → 2 回描いて完全一致
//   * 領域コピー: Slide / Wipe / Blinds の結果に **中間色が 1 画素も無い**
//                （= 実装が alpha blend を使っていないことの直接の証拠）

#include "ambient/render/transitions.hpp"

#include <doctest.h>

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <memory>
#include <string>
#include <vector>

#include "ambient/core/rng.hpp"
#include "ambient/core/transition.hpp"

namespace {

using ambient::core::Millis;
using ambient::core::Progress;
using ambient::core::TransitionRng;

// 480x240 を使う理由: 幅も高さも 4/5/6/8/10 で割り切れる。
// Blinds のスリット境界が整数になり、「丸めで 1px 消えた帯」と
// 「実装の誤り」を取り違えずに済む。
constexpr int kW = 480;
constexpr int kH = 240;

struct Rgb {
  int r = 0;
  int g = 0;
  int b = 0;
  [[nodiscard]] bool close_to(const Rgb& other, int tolerance = 6) const noexcept {
    return std::abs(r - other.r) <= tolerance && std::abs(g - other.g) <= tolerance &&
           std::abs(b - other.b) <= tolerance;
  }
};

constexpr Rgb kFromColor{220, 40, 40};  // 遷移元 = 赤
constexpr Rgb kToColor{40, 80, 220};    // 遷移先 = 青

// SDL 一式を 1 テストぶんだけ立てて壊す。静的初期化順の問題を作らないよう
// あえて共有しない（dummy ドライバなら十分に速い）。
class Harness {
 public:
  Harness() {
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
      error_ = std::string("SDL_Init 失敗: ") + SDL_GetError();
      return;
    }
    inited_ = true;
    // ★実ウィンドウを出さない。ctest は SDL_VIDEODRIVER=dummy を設定するが、
    //   手で直接叩かれても画面を奪わないようにする。
    window_ = SDL_CreateWindow("render_tests", SDL_WINDOWPOS_CENTERED,
                               SDL_WINDOWPOS_CENTERED, kW, kH, SDL_WINDOW_HIDDEN);
    if (window_ == nullptr) {
      error_ = std::string("SDL_CreateWindow 失敗: ") + SDL_GetError();
      return;
    }
    renderer_ = SDL_CreateRenderer(window_, -1, 0);
    if (renderer_ == nullptr) {
      error_ = std::string("SDL_CreateRenderer 失敗: ") + SDL_GetError();
      return;
    }
    // ★論理解像度は設定しない。設定するとビューポートが絡んで
    //   SDL_RenderReadPixels の読み出し範囲が直感と食い違う（display.cpp の落とし穴）。
    target_ = SDL_CreateTexture(renderer_, SDL_PIXELFORMAT_ARGB8888,
                                SDL_TEXTUREACCESS_TARGET, kW, kH);
    if (target_ == nullptr || SDL_SetRenderTarget(renderer_, target_) != 0) {
      error_ = std::string("レンダーターゲットを用意できません: ") + SDL_GetError();
      return;
    }
    from_ = make_solid(kFromColor);
    to_ = make_solid(kToColor);
    ok_ = (from_ != nullptr && to_ != nullptr);
  }

  ~Harness() {
    if (from_ != nullptr) {
      SDL_DestroyTexture(from_);
    }
    if (to_ != nullptr) {
      SDL_DestroyTexture(to_);
    }
    if (target_ != nullptr) {
      SDL_SetRenderTarget(renderer_, nullptr);
      SDL_DestroyTexture(target_);
    }
    if (renderer_ != nullptr) {
      SDL_DestroyRenderer(renderer_);
    }
    if (window_ != nullptr) {
      SDL_DestroyWindow(window_);
    }
    if (inited_) {
      SDL_Quit();
    }
  }

  Harness(const Harness&) = delete;
  Harness& operator=(const Harness&) = delete;
  Harness(Harness&&) = delete;
  Harness& operator=(Harness&&) = delete;

  [[nodiscard]] bool ok() const noexcept { return ok_; }
  [[nodiscard]] const std::string& error() const noexcept { return error_; }
  [[nodiscard]] SDL_Renderer* renderer() const noexcept { return renderer_; }
  [[nodiscard]] SDL_Texture* from() const noexcept { return from_; }
  [[nodiscard]] SDL_Texture* to() const noexcept { return to_; }

  // 単色のテクスチャを作る。中間色が 1 画素も無いことを検査したいので、
  // 元の 2 色は十分に離しておくこと。
  [[nodiscard]] SDL_Texture* make_solid(const Rgb& color) {
    SDL_Texture* texture = SDL_CreateTexture(renderer_, SDL_PIXELFORMAT_ARGB8888,
                                             SDL_TEXTUREACCESS_TARGET, kW, kH);
    if (texture == nullptr) {
      return nullptr;
    }
    SDL_Texture* const previous = SDL_GetRenderTarget(renderer_);
    SDL_SetRenderTarget(renderer_, texture);
    SDL_SetRenderDrawBlendMode(renderer_, SDL_BLENDMODE_NONE);
    SDL_SetRenderDrawColor(renderer_, static_cast<Uint8>(color.r),
                           static_cast<Uint8>(color.g), static_cast<Uint8>(color.b), 255);
    SDL_RenderClear(renderer_);
    SDL_SetRenderTarget(renderer_, previous);
    SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_NONE);
    return texture;
  }

  // 現在のレンダーターゲットを丸ごと読む。
  [[nodiscard]] std::vector<std::uint32_t> read() const {
    std::vector<std::uint32_t> pixels(static_cast<std::size_t>(kW) *
                                      static_cast<std::size_t>(kH));
    const int result = SDL_RenderReadPixels(renderer_, nullptr, SDL_PIXELFORMAT_ARGB8888,
                                            pixels.data(), kW * 4);
    if (result != 0) {
      pixels.clear();
    }
    return pixels;
  }

 private:
  bool inited_ = false;
  bool ok_ = false;
  std::string error_;
  SDL_Window* window_ = nullptr;
  SDL_Renderer* renderer_ = nullptr;
  SDL_Texture* target_ = nullptr;
  SDL_Texture* from_ = nullptr;
  SDL_Texture* to_ = nullptr;
};

[[nodiscard]] Rgb pixel_at(const std::vector<std::uint32_t>& pixels, int x, int y) {
  const std::size_t index = static_cast<std::size_t>(y) * static_cast<std::size_t>(kW) +
                            static_cast<std::size_t>(x);
  if (index >= pixels.size()) {
    return Rgb{-1, -1, -1};
  }
  const std::uint32_t value = pixels[index];
  return Rgb{static_cast<int>((value >> 16) & 0xFFU), static_cast<int>((value >> 8) & 0xFFU),
             static_cast<int>(value & 0xFFU)};
}

[[nodiscard]] double mean_luma(const std::vector<std::uint32_t>& pixels) {
  if (pixels.empty()) {
    return -1.0;
  }
  double sum = 0.0;
  for (const std::uint32_t value : pixels) {
    const double r = static_cast<double>((value >> 16) & 0xFFU);
    const double g = static_cast<double>((value >> 8) & 0xFFU);
    const double b = static_cast<double>(value & 0xFFU);
    sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
  }
  return sum / static_cast<double>(pixels.size());
}

// 遷移先（青）の平均寄与。CrossFade の単調性を測るのに使う。
[[nodiscard]] double mean_blue(const std::vector<std::uint32_t>& pixels) {
  if (pixels.empty()) {
    return -1.0;
  }
  double sum = 0.0;
  for (const std::uint32_t value : pixels) {
    sum += static_cast<double>(value & 0xFFU);
  }
  return sum / static_cast<double>(pixels.size());
}

// duration に対する任意の進捗を作る。
// ★Progress は経過時間からしか作れない。フレーム番号を詰め替えないこと。
[[nodiscard]] Progress progress_of(double fraction, Millis duration) {
  const auto elapsed =
      static_cast<std::int64_t>(fraction * static_cast<double>(duration.value) + 0.5);
  return Progress::fromElapsed(Millis{elapsed}, duration);
}

[[nodiscard]] ambient::core::TransitionPlan plan_of(ambient::core::TransitionKind kind) {
  ambient::core::TransitionPlan plan;
  plan.kind = kind;
  plan.duration = ambient::core::durationOf(kind);
  return plan;
}

// 走査線上で「遷移先の色」が連続する区間の本数を数える。
[[nodiscard]] int count_to_runs_horizontal(const std::vector<std::uint32_t>& pixels, int y) {
  int runs = 0;
  bool inside = false;
  for (int x = 0; x < kW; ++x) {
    const bool is_to = pixel_at(pixels, x, y).close_to(kToColor);
    if (is_to && !inside) {
      ++runs;
    }
    inside = is_to;
  }
  return runs;
}

[[nodiscard]] int count_to_runs_vertical(const std::vector<std::uint32_t>& pixels, int x) {
  int runs = 0;
  bool inside = false;
  for (int y = 0; y < kH; ++y) {
    const bool is_to = pixel_at(pixels, x, y).close_to(kToColor);
    if (is_to && !inside) {
      ++runs;
    }
    inside = is_to;
  }
  return runs;
}

// 走査線を左から見て、遷移先の色でなくなる最初の x（= 境界）。
[[nodiscard]] int boundary_from_left(const std::vector<std::uint32_t>& pixels, int y) {
  for (int x = 0; x < kW; ++x) {
    if (!pixel_at(pixels, x, y).close_to(kToColor)) {
      return x;
    }
  }
  return kW;
}

[[nodiscard]] int boundary_from_top(const std::vector<std::uint32_t>& pixels, int x) {
  for (int y = 0; y < kH; ++y) {
    if (!pixel_at(pixels, x, y).close_to(kToColor)) {
      return y;
    }
  }
  return kH;
}

}  // namespace

// ============================================================================
// 幾何（描画せずに計算だけを検査する）
// ============================================================================

TEST_CASE("幾何: Wipe の境界は progress に線形追従する") {
  const Millis duration = ambient::core::kWipeDuration;
  for (const std::uint16_t angle : ambient::core::kWipeAngles) {
    CAPTURE(angle);
    const bool horizontal = (angle == 0 || angle == 180);
    const int span = horizontal ? kW : kH;
    int previous = -1;
    for (int step = 0; step <= 10; ++step) {
      const double fraction = static_cast<double>(step) / 10.0;
      const Progress progress = progress_of(fraction, duration);
      const SDL_Rect rect = ambient::render::wipe_reveal_rect(angle, progress, kW, kH);
      const int band = horizontal ? rect.w : rect.h;
      const int expected = static_cast<int>(
          static_cast<double>(span) * static_cast<double>(progress.value()) + 0.5);
      // 線形追従: 進捗 x スパンの四捨五入と一致すること（丸めの 1px は許容）。
      CHECK(std::abs(band - expected) <= 1);
      // 単調増加であること。
      CHECK(band >= previous);
      previous = band;
    }
    CHECK(previous == span);  // 端点
  }
}

TEST_CASE("幾何: Wipe の帯は角度ごとに正しい辺から伸びる") {
  const Millis duration = ambient::core::kWipeDuration;
  const Progress half = progress_of(0.5, duration);

  const SDL_Rect right = ambient::render::wipe_reveal_rect(0, half, kW, kH);
  CHECK(right.x == 0);
  CHECK(right.w == kW / 2);
  CHECK(right.h == kH);

  const SDL_Rect down = ambient::render::wipe_reveal_rect(90, half, kW, kH);
  CHECK(down.y == 0);
  CHECK(down.h == kH / 2);
  CHECK(down.w == kW);

  const SDL_Rect left = ambient::render::wipe_reveal_rect(180, half, kW, kH);
  CHECK(left.x == kW / 2);
  CHECK(left.w == kW / 2);

  const SDL_Rect up = ambient::render::wipe_reveal_rect(270, half, kW, kH);
  CHECK(up.y == kH / 2);
  CHECK(up.h == kH / 2);
}

TEST_CASE("幾何: Slide の押し出し量は 0 -> 全幅へ線形に動く") {
  const Millis duration = ambient::core::kSlideDuration;
  using ambient::core::SlideDirection;
  const SlideDirection directions[] = {SlideDirection::FromLeft, SlideDirection::FromRight,
                                       SlideDirection::FromTop, SlideDirection::FromBottom};
  for (const SlideDirection direction : directions) {
    const bool horizontal =
        (direction == SlideDirection::FromLeft || direction == SlideDirection::FromRight);
    const int span = horizontal ? kW : kH;
    CHECK(ambient::render::slide_offset_px(direction, progress_of(0.0, duration), kW, kH) == 0);
    CHECK(ambient::render::slide_offset_px(direction, progress_of(0.5, duration), kW, kH) ==
          span / 2);
    CHECK(ambient::render::slide_offset_px(direction, progress_of(1.0, duration), kW, kH) ==
          span);
  }
}

TEST_CASE("幾何: Blinds のスリットは隙間も重なりも作らない") {
  const Millis duration = ambient::core::kBlindsDuration;
  const Progress full = progress_of(1.0, duration);

  for (std::uint8_t slats = ambient::core::kBlindsSlatsMin;
       slats <= ambient::core::kBlindsSlatsMax; ++slats) {
    CAPTURE(slats);
    ambient::core::BlindsParams params;
    params.slats = slats;

    // 縦スリット: progress=1 で x 方向を隙間なく覆う。
    params.vertical = true;
    int covered = 0;
    int expected_x = 0;
    for (std::size_t i = 0; i < slats; ++i) {
      const SDL_Rect rect = ambient::render::blinds_reveal_rect(params, i, full, kW, kH);
      CHECK(rect.x == expected_x);  // 前のスリットの右端とぴったり接する
      CHECK(rect.h == kH);
      covered += rect.w;
      expected_x = rect.x + rect.w;
    }
    CHECK(covered == kW);
    CHECK(expected_x == kW);

    // 横スリット。
    params.vertical = false;
    covered = 0;
    int expected_y = 0;
    for (std::size_t i = 0; i < slats; ++i) {
      const SDL_Rect rect = ambient::render::blinds_reveal_rect(params, i, full, kW, kH);
      CHECK(rect.y == expected_y);
      CHECK(rect.w == kW);
      covered += rect.h;
      expected_y = rect.y + rect.h;
    }
    CHECK(covered == kH);
  }
}

TEST_CASE("幾何: KenBurns は端点で start / end に一致し、単位正方形から出ない") {
  TransitionRng rng{20260730ULL};
  for (int i = 0; i < 32; ++i) {
    const ambient::core::KenBurnsParams params = ambient::core::deriveKenBurns(rng);
    REQUIRE(params.moves());

    const Progress start = progress_of(0.0, ambient::core::kDwellDurationDefault);
    const Progress end = progress_of(1.0, ambient::core::kDwellDurationDefault);
    CHECK(ambient::render::ken_burns_rect_at(params, start) == params.start);
    CHECK(ambient::render::ken_burns_rect_at(params, end) == params.end);

    for (int step = 0; step <= 10; ++step) {
      const Progress progress = progress_of(static_cast<double>(step) / 10.0,
                                            ambient::core::kDwellDurationDefault);
      const ambient::core::NormRect rect = ambient::render::ken_burns_rect_at(params, progress);
      // ★画面外を舐めないこと。core の deriveKenBurns がこれを保証しており、
      //   補間で破ってはならない。
      CHECK(rect.insideUnitSquare());
    }
  }
}

// ============================================================================
// 描画の構造アサーション
// ============================================================================

TEST_CASE("Slide: progress=0.5 で画面が方向どおりに半分ずつに分かれる") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::Slide);
  ambient::core::TransitionParams params;
  const Progress half = progress_of(0.5, plan.duration);

  const int left_x = kW / 4;
  const int right_x = (kW * 3) / 4;
  const int top_y = kH / 4;
  const int bottom_y = (kH * 3) / 4;

  SUBCASE("FromRight: 左半分が旧・右半分が新") {
    params.slide.direction = ambient::core::SlideDirection::FromRight;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, left_x, kH / 2).close_to(kFromColor));
    CHECK(pixel_at(pixels, right_x, kH / 2).close_to(kToColor));
  }

  SUBCASE("FromLeft: 左半分が新・右半分が旧（FromRight の鏡像）") {
    params.slide.direction = ambient::core::SlideDirection::FromLeft;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, left_x, kH / 2).close_to(kToColor));
    CHECK(pixel_at(pixels, right_x, kH / 2).close_to(kFromColor));
  }

  SUBCASE("FromTop: 上半分が新・下半分が旧") {
    params.slide.direction = ambient::core::SlideDirection::FromTop;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, kW / 2, top_y).close_to(kToColor));
    CHECK(pixel_at(pixels, kW / 2, bottom_y).close_to(kFromColor));
  }

  SUBCASE("FromBottom: 上半分が旧・下半分が新") {
    params.slide.direction = ambient::core::SlideDirection::FromBottom;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, kW / 2, top_y).close_to(kFromColor));
    CHECK(pixel_at(pixels, kW / 2, bottom_y).close_to(kToColor));
  }
}

TEST_CASE("Slide: 遷移が終われば画面全体が遷移先になる") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::Slide);
  ambient::core::TransitionParams params;
  params.slide.direction = ambient::core::SlideDirection::FromRight;
  ambient::render::draw_transition(harness.renderer(), plan, params,
                                   progress_of(1.0, plan.duration), harness.from(),
                                   harness.to(), kW, kH);
  const auto pixels = harness.read();
  REQUIRE(!pixels.empty());
  CHECK(pixel_at(pixels, 1, 1).close_to(kToColor));
  CHECK(pixel_at(pixels, kW - 2, kH - 2).close_to(kToColor));
  CHECK(pixel_at(pixels, kW / 2, kH / 2).close_to(kToColor));
}

TEST_CASE("Wipe: 描いた絵の境界が progress に線形追従する") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::Wipe);
  ambient::core::TransitionParams params;

  SUBCASE("角度 0（左 -> 右）") {
    params.wipe.angleDeg = 0;
    int previous = -1;
    for (int step = 1; step < 10; ++step) {
      const double fraction = static_cast<double>(step) / 10.0;
      const Progress progress = progress_of(fraction, plan.duration);
      ambient::render::draw_transition(harness.renderer(), plan, params, progress,
                                       harness.from(), harness.to(), kW, kH);
      const auto pixels = harness.read();
      REQUIRE(!pixels.empty());
      const int boundary = boundary_from_left(pixels, kH / 2);
      const int expected = static_cast<int>(
          static_cast<double>(kW) * static_cast<double>(progress.value()) + 0.5);
      CHECK(std::abs(boundary - expected) <= 1);
      CHECK(boundary > previous);
      previous = boundary;
    }
  }

  SUBCASE("角度 90（上 -> 下）") {
    params.wipe.angleDeg = 90;
    int previous = -1;
    for (int step = 1; step < 10; ++step) {
      const double fraction = static_cast<double>(step) / 10.0;
      const Progress progress = progress_of(fraction, plan.duration);
      ambient::render::draw_transition(harness.renderer(), plan, params, progress,
                                       harness.from(), harness.to(), kW, kH);
      const auto pixels = harness.read();
      REQUIRE(!pixels.empty());
      const int boundary = boundary_from_top(pixels, kW / 2);
      const int expected = static_cast<int>(
          static_cast<double>(kH) * static_cast<double>(progress.value()) + 0.5);
      CHECK(std::abs(boundary - expected) <= 1);
      CHECK(boundary > previous);
      previous = boundary;
    }
  }

  SUBCASE("角度 180 / 270 は反対の辺から進む") {
    const Progress quarter = progress_of(0.25, plan.duration);

    params.wipe.angleDeg = 180;
    ambient::render::draw_transition(harness.renderer(), plan, params, quarter, harness.from(),
                                     harness.to(), kW, kH);
    auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, kW - 2, kH / 2).close_to(kToColor));  // 右端が新
    CHECK(pixel_at(pixels, 2, kH / 2).close_to(kFromColor));     // 左端は旧のまま

    params.wipe.angleDeg = 270;
    ambient::render::draw_transition(harness.renderer(), plan, params, quarter, harness.from(),
                                     harness.to(), kW, kH);
    pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(pixel_at(pixels, kW / 2, kH - 2).close_to(kToColor));  // 下端が新
    CHECK(pixel_at(pixels, kW / 2, 2).close_to(kFromColor));     // 上端は旧のまま
  }
}

TEST_CASE("FadeThroughBlack: progress=0.5 で画面全体の輝度が最小になる") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::FadeThroughBlack);
  const ambient::core::TransitionParams params;

  double min_luma = 1e9;
  double luma_at_half = -1.0;
  double best_fraction = -1.0;
  for (int step = 0; step <= 20; ++step) {
    const double fraction = static_cast<double>(step) / 20.0;
    ambient::render::draw_transition(harness.renderer(), plan, params,
                                     progress_of(fraction, plan.duration), harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    const double luma = mean_luma(pixels);
    if (luma < min_luma) {
      min_luma = luma;
      best_fraction = fraction;
    }
    if (step == 10) {
      luma_at_half = luma;
    }
  }
  CHECK(best_fraction == doctest::Approx(0.5));
  CHECK(luma_at_half == doctest::Approx(min_luma));
  // 0.5 では黒で覆い切っていること（2 枚が同時に見えない = 企画の意図）。
  CHECK(luma_at_half < 1.0);
}

TEST_CASE("CrossFade: progress の増加とともに新画像の寄与が単調増加する") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::CrossFade);
  const ambient::core::TransitionParams params;

  double previous = -1.0;
  for (int step = 0; step <= 10; ++step) {
    const double fraction = static_cast<double>(step) / 10.0;
    ambient::render::draw_transition(harness.renderer(), plan, params,
                                     progress_of(fraction, plan.duration), harness.from(),
                                     harness.to(), kW, kH);
    const auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    const double blue = mean_blue(pixels);
    CHECK(blue > previous);  // 厳密な単調増加
    previous = blue;
  }

  // 端点: 0 は遷移元そのもの、1 は遷移先そのもの。
  ambient::render::draw_transition(harness.renderer(), plan, params,
                                   progress_of(0.0, plan.duration), harness.from(),
                                   harness.to(), kW, kH);
  CHECK(pixel_at(harness.read(), kW / 2, kH / 2).close_to(kFromColor));
  ambient::render::draw_transition(harness.renderer(), plan, params,
                                   progress_of(1.0, plan.duration), harness.from(),
                                   harness.to(), kW, kH);
  CHECK(pixel_at(harness.read(), kW / 2, kH / 2).close_to(kToColor));
}

TEST_CASE("Blinds: 帯の本数が BlindsParams::slats と一致する") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const auto plan = plan_of(ambient::core::TransitionKind::Blinds);
  ambient::core::TransitionParams params;
  const Progress half = progress_of(0.5, plan.duration);

  for (std::uint8_t slats = ambient::core::kBlindsSlatsMin;
       slats <= ambient::core::kBlindsSlatsMax; ++slats) {
    CAPTURE(slats);
    params.blinds.slats = slats;

    params.blinds.vertical = true;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(count_to_runs_horizontal(pixels, kH / 2) == static_cast<int>(slats));
    // 縦スリットなので、どの走査線でも同じ本数になる。
    CHECK(count_to_runs_horizontal(pixels, 2) == static_cast<int>(slats));
    CHECK(count_to_runs_horizontal(pixels, kH - 3) == static_cast<int>(slats));

    params.blinds.vertical = false;
    ambient::render::draw_transition(harness.renderer(), plan, params, half, harness.from(),
                                     harness.to(), kW, kH);
    pixels = harness.read();
    REQUIRE(!pixels.empty());
    CHECK(count_to_runs_vertical(pixels, kW / 2) == static_cast<int>(slats));
    CHECK(count_to_runs_vertical(pixels, 2) == static_cast<int>(slats));
  }
}

// ============================================================================
// ★CompositeClass::RegionCopy を嘘にしていないことの直接の証拠
// ============================================================================
//
// 領域コピー群が内部で alpha blend を使っていたら、境界に必ず中間色の画素が出る
// （単色 2 枚を混ぜているので、混ざれば一目で分かる）。
// 1 画素も中間色が無いことは「本当に矩形転送だけで描いた」ことの直接の証拠になる。
//
// この検査を外すと、うっかり blend を混ぜても絵は綺麗なまま通ってしまい、
// Tab5 で「重みを下げたのに blend の負荷が減らない」という形で初めて露見する。

TEST_CASE("領域コピー群は中間色を 1 画素も作らない（blend を使っていない証拠）") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  struct Case {
    ambient::core::TransitionKind kind;
    const char* name;
  };
  const Case cases[] = {
      {ambient::core::TransitionKind::Slide, "Slide"},
      {ambient::core::TransitionKind::Wipe, "Wipe"},
      {ambient::core::TransitionKind::Blinds, "Blinds"},
  };

  for (const Case& entry : cases) {
    CAPTURE(entry.name);
    // ★分類は core が持っている。ここでその分類を前提にしていることを明示する。
    REQUIRE(ambient::core::compositeClassOf(entry.kind) ==
            ambient::core::CompositeClass::RegionCopy);

    const auto plan = plan_of(entry.kind);
    ambient::core::TransitionParams params;
    params.slide.direction = ambient::core::SlideDirection::FromRight;
    params.wipe.angleDeg = 90;
    params.blinds.slats = 6;
    params.blinds.vertical = true;

    for (int step = 0; step <= 10; ++step) {
      const double fraction = static_cast<double>(step) / 10.0;
      CAPTURE(fraction);
      ambient::render::draw_transition(harness.renderer(), plan, params,
                                       progress_of(fraction, plan.duration), harness.from(),
                                       harness.to(), kW, kH);
      const auto pixels = harness.read();
      REQUIRE(!pixels.empty());

      int foreign = 0;
      for (const std::uint32_t value : pixels) {
        const Rgb rgb{static_cast<int>((value >> 16) & 0xFFU),
                      static_cast<int>((value >> 8) & 0xFFU),
                      static_cast<int>(value & 0xFFU)};
        if (!rgb.close_to(kFromColor, 2) && !rgb.close_to(kToColor, 2)) {
          ++foreign;
        }
      }
      CHECK(foreign == 0);
    }
  }
}

TEST_CASE("draw_transition はテクスチャの状態を書き換えたまま返さない") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  // ★呼び出し側（LivePhotoTextures）が設定した状態が壊れないこと。
  //   CrossFade は alpha mod を書き換えるので、ここが漏れると以降ずっと
  //   写真が半透明で描かれる（まったく別の症状に見える）。
  SDL_SetTextureBlendMode(harness.to(), SDL_BLENDMODE_BLEND);
  SDL_SetTextureAlphaMod(harness.to(), 200);
  SDL_SetTextureColorMod(harness.to(), 10, 20, 30);

  const auto plan = plan_of(ambient::core::TransitionKind::CrossFade);
  const ambient::core::TransitionParams params;
  ambient::render::draw_transition(harness.renderer(), plan, params,
                                   progress_of(0.4, plan.duration), harness.from(),
                                   harness.to(), kW, kH);

  SDL_BlendMode blend = SDL_BLENDMODE_NONE;
  Uint8 alpha = 0;
  Uint8 red = 0;
  Uint8 green = 0;
  Uint8 blue = 0;
  SDL_GetTextureBlendMode(harness.to(), &blend);
  SDL_GetTextureAlphaMod(harness.to(), &alpha);
  SDL_GetTextureColorMod(harness.to(), &red, &green, &blue);
  CHECK(blend == SDL_BLENDMODE_BLEND);
  CHECK(alpha == 200);
  CHECK(red == 10);
  CHECK(green == 20);
  CHECK(blue == 30);
}

TEST_CASE("KenBurns: 進捗とともに切り出しが動き、画面は必ず埋まる") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  TransitionRng rng{4242ULL};
  const ambient::core::KenBurnsParams params = ambient::core::deriveKenBurns(rng);
  REQUIRE(params.moves());

  // ★単色でも「一部だけ塗り分けた」画像でも駄目。切り出しがその一部を通らない
  //   ときに絵が変化せず、実装が正しくてもテストが落ちる。
  //   画像全域に位置が分かる模様（格子）を置き、**どこを切り出しても
  //   位置が変われば絵が変わる**状態にする。
  SDL_Texture* photo = harness.make_solid(kFromColor);
  REQUIRE(photo != nullptr);
  {
    SDL_Texture* const previous = SDL_GetRenderTarget(harness.renderer());
    SDL_SetRenderTarget(harness.renderer(), photo);
    SDL_SetRenderDrawBlendMode(harness.renderer(), SDL_BLENDMODE_NONE);
    constexpr int kCols = 16;
    constexpr int kRows = 8;
    for (int row = 0; row < kRows; ++row) {
      for (int col = 0; col < kCols; ++col) {
        const SDL_Rect cell{(col * kW) / kCols, (row * kH) / kRows, kW / kCols, kH / kRows};
        SDL_SetRenderDrawColor(harness.renderer(), static_cast<Uint8>(col * 16),
                               static_cast<Uint8>(row * 32),
                               static_cast<Uint8>(255 - col * 15), 255);
        SDL_RenderFillRect(harness.renderer(), &cell);
      }
    }
    SDL_SetRenderTarget(harness.renderer(), previous);
  }

  std::vector<std::vector<std::uint32_t>> frames;
  for (int step = 0; step <= 10; ++step) {
    const Progress progress =
        progress_of(static_cast<double>(step) / 10.0, ambient::core::kDwellDurationDefault);
    ambient::render::draw_ken_burns(harness.renderer(), params, progress, photo, kW, kH);
    auto pixels = harness.read();
    REQUIRE(!pixels.empty());
    // 画面全体が写真で埋まる（未描画の領域が残らない）。
    for (const std::uint32_t value : pixels) {
      CHECK(static_cast<int>((value >> 24) & 0xFFU) == 255);
    }
    frames.push_back(std::move(pixels));
  }

  // 20 秒間まったく動かない KenBurns は演出が消えているのと同じ。
  // ★毎ステップ絵が変わること（1 か所でも止まっていたら補間が壊れている）。
  for (std::size_t i = 1; i < frames.size(); ++i) {
    CAPTURE(i);
    CHECK(frames[i] != frames[i - 1]);
  }
  CHECK(frames.front() != frames.back());

  SDL_DestroyTexture(photo);
}

// ============================================================================
// 決定性（ここだけはピクセル完全一致でよい）
// ============================================================================

TEST_CASE("決定性: 同じ seed から計画して描くと 2 回とも完全に同じ絵になる") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  constexpr std::uint64_t kSeed = 0x5EED202607'30ULL;
  const ambient::core::RenderCaps caps{true};
  const ambient::core::SteadyTime now{0};

  std::vector<std::uint32_t> runs[2];

  for (int pass = 0; pass < 2; ++pass) {
    // ★毎回 seed から計画し直す。同じ params を 2 回描くだけでは
    //   SDL の再現性しか見ておらず、パイプラインの決定性を検査したことにならない。
    TransitionRng rng{kSeed};
    std::vector<std::uint32_t> accumulated;
    for (int i = 0; i < 8; ++i) {
      const ambient::core::PlannedTransition planned = ambient::core::planTransition(
          ambient::core::kDefaultTransitionWeights, caps, now, rng);
      const Progress progress = progress_of(0.37, planned.plan.duration);
      ambient::render::draw_transition(harness.renderer(), planned.plan, planned.params,
                                       progress, harness.from(), harness.to(), kW, kH);
      const auto pixels = harness.read();
      REQUIRE(!pixels.empty());
      accumulated.insert(accumulated.end(), pixels.begin(), pixels.end());
    }
    runs[pass] = std::move(accumulated);
  }

  REQUIRE(runs[0].size() == runs[1].size());
  CHECK(runs[0] == runs[1]);
}

// ============================================================================
// RenderCaps の実測
// ============================================================================

TEST_CASE("RenderCaps: SDL の実能力を実測して申告する") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  const ambient::render::RenderCapsReport report =
      ambient::render::probe_render_caps(harness.renderer());
  CAPTURE(report.note);
  CAPTURE(ambient::render::toString(report.method));

  // dummy ドライバのソフトウェアレンダラはレンダーターゲットに対応しているので、
  // ここは実測経路を通るはず。通らなくなったら SDL 側の変化なので気づきたい。
  CHECK(report.method == ambient::render::CapsProbeMethod::Measured);
  CHECK(report.caps.blend);
  // 実測値が「中間色」であること（赤にも青にも張り付いていない）。
  CHECK(report.drawMixR > 16);
  CHECK(report.drawMixR < 239);
  CHECK(report.drawMixB > 16);
  CHECK(report.drawMixB < 239);
  CHECK(report.texMixR > 16);
  CHECK(report.texMixG > 16);

  // caps が blend 群を許すこと（= 6 演出すべてが抽選に残る）。
  CHECK(report.caps.allows(ambient::core::TransitionKind::CrossFade));
  CHECK(report.caps.allows(ambient::core::TransitionKind::FadeThroughBlack));
  CHECK(report.caps.allows(ambient::core::DwellEffect::KenBurns));
  CHECK(ambient::core::allowedWeights(ambient::core::kDefaultTransitionWeights, report.caps)
            .total() == ambient::core::kDefaultTransitionWeights.total());

  // ★呼び出し後にレンダーターゲットが変わっていないこと
  //   （変わっていると以降の描画が別の面へ行き、原因が分からなくなる）。
  ambient::render::draw_full(harness.renderer(), harness.to(), kW, kH);
  CHECK(pixel_at(harness.read(), kW / 2, kH / 2).close_to(kToColor));
}

TEST_CASE("Canvas: 画面 1 枚ぶんを合成してから遷移に使える") {
  Harness harness;
  REQUIRE_MESSAGE(harness.ok(), harness.error());

  std::string error;
  const std::unique_ptr<ambient::render::Canvas> canvas =
      ambient::render::Canvas::create(harness.renderer(), kW, kH, error);
  REQUIRE_MESSAGE(canvas != nullptr, error);

  // キャンバスへ描く。★スコープを抜けたら元の対象（テスト用ターゲット）へ戻ること。
  {
    const ambient::render::Canvas::Scope scope(harness.renderer(), *canvas);
    REQUIRE(scope.ok());
    SDL_SetRenderDrawBlendMode(harness.renderer(), SDL_BLENDMODE_NONE);
    SDL_SetRenderDrawColor(harness.renderer(), static_cast<Uint8>(kToColor.r),
                           static_cast<Uint8>(kToColor.g), static_cast<Uint8>(kToColor.b),
                           255);
    SDL_RenderClear(harness.renderer());
  }

  // 戻ったことの確認: ここでの描画はキャンバスではなくターゲットへ行く。
  ambient::render::draw_full(harness.renderer(), harness.from(), kW, kH);
  CHECK(pixel_at(harness.read(), kW / 2, kH / 2).close_to(kFromColor));

  // キャンバスを遷移先として使えること。
  const auto plan = plan_of(ambient::core::TransitionKind::Wipe);
  ambient::core::TransitionParams params;
  params.wipe.angleDeg = 0;
  ambient::render::draw_transition(harness.renderer(), plan, params,
                                   progress_of(0.5, plan.duration), harness.from(),
                                   canvas->raw(), kW, kH);
  const auto pixels = harness.read();
  REQUIRE(!pixels.empty());
  CHECK(pixel_at(pixels, kW / 4, kH / 2).close_to(kToColor));
  CHECK(pixel_at(pixels, (kW * 3) / 4, kH / 2).close_to(kFromColor));
}
