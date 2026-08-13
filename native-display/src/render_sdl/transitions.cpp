// transitions.cpp — 5 種の遷移と滞在効果の SDL 実装。
//
// 設計判断（イージング無し / RegionCopy 群で blend を封じる / Wipe の角度の意味 /
// RenderCaps の判定方法）は transitions.hpp 冒頭が正典。ここには書かない。

#include "ambient/render/transitions.hpp"

#include <algorithm>

#include "ambient/render/image.hpp"

namespace ambient::render {
namespace {

using core::CompositeClass;
using core::Progress;

// ---------------------------------------------------------------------------
// 状態の保存と復元（RAII）
// ---------------------------------------------------------------------------
//
// ★★ ここが漏れると「別の不具合」に化ける ★★
//
// CrossFade は SDL_SetTextureAlphaMod() で **テクスチャそのもの** を書き換える。
// そのテクスチャは LivePhotoTextures が通常描画にも使い回している同じ実体である。
// 遷移が途中で打ち切られた（新しい切り替えが割り込んだ / 早期 return した）とき
// alpha=128 のまま残ると、以降ずっと写真が半透明で描かれ、下地の暗色が透けて
// 「写真が妙に暗い」という**まったく違う症状**になる。原因追跡はほぼ不可能。
//
// したがって「関数の最後で戻す」ではなくスコープ脱出で必ず戻す。
class TextureStateGuard {
 public:
  explicit TextureStateGuard(SDL_Texture* texture) noexcept : texture_(texture) {
    if (texture_ == nullptr) {
      return;
    }
    SDL_GetTextureBlendMode(texture_, &blend_);
    SDL_GetTextureAlphaMod(texture_, &alpha_);
    SDL_GetTextureColorMod(texture_, &red_, &green_, &blue_);
  }

  ~TextureStateGuard() {
    if (texture_ == nullptr) {
      return;
    }
    SDL_SetTextureBlendMode(texture_, blend_);
    SDL_SetTextureAlphaMod(texture_, alpha_);
    SDL_SetTextureColorMod(texture_, red_, green_, blue_);
  }

  TextureStateGuard(const TextureStateGuard&) = delete;
  TextureStateGuard& operator=(const TextureStateGuard&) = delete;
  TextureStateGuard(TextureStateGuard&&) = delete;
  TextureStateGuard& operator=(TextureStateGuard&&) = delete;

 private:
  SDL_Texture* texture_;
  SDL_BlendMode blend_ = SDL_BLENDMODE_NONE;
  Uint8 alpha_ = 255;
  Uint8 red_ = 255;
  Uint8 green_ = 255;
  Uint8 blue_ = 255;
};

// レンダラの描画色とブレンドモードも同様に戻す。
// （screens.cpp が「前のフレームで設定された描画色」を前提にしていなくても、
//   呼び出し前後で状態が変わらないほうが後から足す人が事故らない。）
class RendererStateGuard {
 public:
  explicit RendererStateGuard(SDL_Renderer* renderer) noexcept : renderer_(renderer) {
    if (renderer_ == nullptr) {
      return;
    }
    SDL_GetRenderDrawColor(renderer_, &red_, &green_, &blue_, &alpha_);
    SDL_GetRenderDrawBlendMode(renderer_, &blend_);
  }

  ~RendererStateGuard() {
    if (renderer_ == nullptr) {
      return;
    }
    SDL_SetRenderDrawColor(renderer_, red_, green_, blue_, alpha_);
    SDL_SetRenderDrawBlendMode(renderer_, blend_);
  }

  RendererStateGuard(const RendererStateGuard&) = delete;
  RendererStateGuard& operator=(const RendererStateGuard&) = delete;
  RendererStateGuard(RendererStateGuard&&) = delete;
  RendererStateGuard& operator=(RendererStateGuard&&) = delete;

 private:
  SDL_Renderer* renderer_;
  Uint8 red_ = 0;
  Uint8 green_ = 0;
  Uint8 blue_ = 0;
  Uint8 alpha_ = 255;
  SDL_BlendMode blend_ = SDL_BLENDMODE_NONE;
};

// ---------------------------------------------------------------------------
// 小さな算術ヘルパ
// ---------------------------------------------------------------------------

// [0, span] の範囲へ、進捗に比例した長さを返す。
// ★丸めは四捨五入。切り捨てだと progress=0.999 でも 1px 足りない帯が残り、
//   遷移の最後の 1 フレームだけ細い線が見える。
[[nodiscard]] int scaled(int span, float progress) noexcept {
  if (span <= 0) {
    return 0;
  }
  const float raw = static_cast<float>(span) * progress + 0.5F;
  const int value = static_cast<int>(raw);
  return std::clamp(value, 0, span);
}

// テクスチャの大きさ。取得に失敗したら 0 を返す。
struct TexSize {
  int w = 0;
  int h = 0;
};

[[nodiscard]] TexSize size_of(SDL_Texture* texture) noexcept {
  TexSize size;
  if (texture == nullptr) {
    return size;
  }
  if (SDL_QueryTexture(texture, nullptr, nullptr, &size.w, &size.h) != 0) {
    return TexSize{};
  }
  return size;
}

// 「画面全体」に対応するテクスチャ内の矩形（cover クロップ済み）。
// テクスチャが画面と同じ大きさなら全面がそのまま返る。
[[nodiscard]] SDL_Rect cover_base_rect(const TexSize& tex, int width, int height) noexcept {
  if (tex.w <= 0 || tex.h <= 0) {
    return SDL_Rect{0, 0, 0, 0};
  }
  return cover_source_rect(tex.w, tex.h, width, height);
}

// 論理座標の矩形 → テクスチャ座標の矩形。
// cover_base の内側へ線形に写す。整数演算のまま行い、丸め誤差で 1px 抜けないよう
// 端は base の端へ吸着させる。
[[nodiscard]] SDL_Rect to_source(const SDL_Rect& logical, const SDL_Rect& base, int width,
                                 int height) noexcept {
  if (width <= 0 || height <= 0 || base.w <= 0 || base.h <= 0) {
    return SDL_Rect{0, 0, 0, 0};
  }
  const auto map_x = [&](int value) {
    const long long scaled_value =
        (static_cast<long long>(value) * base.w + width / 2) / width;
    return base.x + static_cast<int>(scaled_value);
  };
  const auto map_y = [&](int value) {
    const long long scaled_value =
        (static_cast<long long>(value) * base.h + height / 2) / height;
    return base.y + static_cast<int>(scaled_value);
  };

  const int x0 = map_x(logical.x);
  const int x1 = map_x(logical.x + logical.w);
  const int y0 = map_y(logical.y);
  const int y1 = map_y(logical.y + logical.h);
  return SDL_Rect{x0, y0, x1 - x0, y1 - y0};
}

// 領域コピー: 論理矩形 dst へ、テクスチャの対応部分 src_logical を転送する。
//
// ★blend を明示的に切ってから転送する。RegionCopy 群がうっかり blend を
//   使っていないことを、コードの見た目ではなく **設定** で保証する。
void copy_region(SDL_Renderer* renderer, SDL_Texture* texture, const SDL_Rect& base,
                 const SDL_Rect& src_logical, const SDL_Rect& dst, int width,
                 int height) {
  if (dst.w <= 0 || dst.h <= 0) {
    return;
  }
  if (texture == nullptr) {
    // 遷移元が無い場合は黒で埋める（下地が透けないよう不透明で塗る）。
    SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
    SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
    SDL_RenderFillRect(renderer, &dst);
    return;
  }
  SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_NONE);
  SDL_SetTextureAlphaMod(texture, 255);
  SDL_SetTextureColorMod(texture, 255, 255, 255);
  const SDL_Rect src = to_source(src_logical, base, width, height);
  if (src.w <= 0 || src.h <= 0) {
    return;
  }
  SDL_RenderCopy(renderer, texture, &src, &dst);
}

// 画面全体を 1 枚で埋める（領域コピー）。
void copy_full(SDL_Renderer* renderer, SDL_Texture* texture, int width, int height) {
  const SDL_Rect full{0, 0, width, height};
  const SDL_Rect base = cover_base_rect(size_of(texture), width, height);
  copy_region(renderer, texture, base, full, full, width, height);
}

// 画面全体を、指定 alpha の blend で重ねる（CrossFade 用）。
void blend_full(SDL_Renderer* renderer, SDL_Texture* texture, Uint8 alpha, int width,
                int height) {
  if (texture == nullptr || alpha == 0) {
    return;
  }
  const TexSize tex = size_of(texture);
  const SDL_Rect base = cover_base_rect(tex, width, height);
  if (base.w <= 0 || base.h <= 0) {
    return;
  }
  SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_BLEND);
  SDL_SetTextureAlphaMod(texture, alpha);
  SDL_SetTextureColorMod(texture, 255, 255, 255);
  const SDL_Rect dst{0, 0, width, height};
  SDL_RenderCopy(renderer, texture, &base, &dst);
}

// 画面全体を黒で覆う（FadeThroughBlack 用）。
void veil_black(SDL_Renderer* renderer, Uint8 alpha, int width, int height) {
  if (alpha == 0) {
    return;
  }
  SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
  SDL_SetRenderDrawColor(renderer, 0, 0, 0, alpha);
  const SDL_Rect full{0, 0, width, height};
  SDL_RenderFillRect(renderer, &full);
}

[[nodiscard]] Uint8 to_alpha(float unit_value) noexcept {
  const float clamped = std::clamp(unit_value, 0.0F, 1.0F);
  const int value = static_cast<int>(clamped * 255.0F + 0.5F);
  return static_cast<Uint8>(std::clamp(value, 0, 255));
}

// ---------------------------------------------------------------------------
// 各演出
// ---------------------------------------------------------------------------

// CrossFade（Blend 群）。A を不透明で敷き、B を alpha=progress で重ねる。
void draw_cross_fade(SDL_Renderer* renderer, Progress progress, SDL_Texture* from,
                     SDL_Texture* to, int width, int height) {
  copy_full(renderer, from, width, height);
  blend_full(renderer, to, to_alpha(progress.value()), width, height);
}

// FadeThroughBlack（Blend 群）。
//
// 前半 [0, 0.5] は A の上へ黒を濃くしていき、後半 [0.5, 1] は B の上から黒を薄める。
// ★progress=0.5 でちょうど黒 alpha=255 になる = 画面の輝度が最小。
//   これが render_tests の構造アサーションになっている。
void draw_fade_through_black(SDL_Renderer* renderer, Progress progress, SDL_Texture* from,
                             SDL_Texture* to, int width, int height) {
  const float p = progress.value();
  if (p < 0.5F) {
    copy_full(renderer, from, width, height);
    veil_black(renderer, to_alpha(p * 2.0F), width, height);
  } else {
    copy_full(renderer, to, width, height);
    veil_black(renderer, to_alpha((1.0F - p) * 2.0F), width, height);
  }
}

// Slide（RegionCopy 群）。
//
// A と B の両方を動かす「押し出し」。転送は 2 回だけで、画面外へ出る部分は
// 転送元矩形を縮めて表現する（SDL のクリップに任せず自分で切る）。
// ★どちらの転送も blend 無効。alpha は 1 度も触らない。
void draw_slide(SDL_Renderer* renderer, const core::SlideParams& params, Progress progress,
                SDL_Texture* from, SDL_Texture* to, int width, int height) {
  const SDL_Rect from_base = cover_base_rect(size_of(from), width, height);
  const SDL_Rect to_base = cover_base_rect(size_of(to), width, height);
  const int offset = slide_offset_px(params.direction, progress, width, height);

  switch (params.direction) {
    case core::SlideDirection::FromLeft: {
      // B が左から入り、A が右へ押し出される。
      const int rest = width - offset;
      copy_region(renderer, from, from_base, SDL_Rect{0, 0, rest, height},
                  SDL_Rect{offset, 0, rest, height}, width, height);
      copy_region(renderer, to, to_base, SDL_Rect{width - offset, 0, offset, height},
                  SDL_Rect{0, 0, offset, height}, width, height);
      return;
    }
    case core::SlideDirection::FromRight: {
      const int rest = width - offset;
      copy_region(renderer, from, from_base, SDL_Rect{offset, 0, rest, height},
                  SDL_Rect{0, 0, rest, height}, width, height);
      copy_region(renderer, to, to_base, SDL_Rect{0, 0, offset, height},
                  SDL_Rect{rest, 0, offset, height}, width, height);
      return;
    }
    case core::SlideDirection::FromTop: {
      const int rest = height - offset;
      copy_region(renderer, from, from_base, SDL_Rect{0, 0, width, rest},
                  SDL_Rect{0, offset, width, rest}, width, height);
      copy_region(renderer, to, to_base, SDL_Rect{0, height - offset, width, offset},
                  SDL_Rect{0, 0, width, offset}, width, height);
      return;
    }
    case core::SlideDirection::FromBottom: {
      const int rest = height - offset;
      copy_region(renderer, from, from_base, SDL_Rect{0, offset, width, rest},
                  SDL_Rect{0, 0, width, rest}, width, height);
      copy_region(renderer, to, to_base, SDL_Rect{0, 0, width, offset},
                  SDL_Rect{0, rest, width, offset}, width, height);
      return;
    }
  }
}

// Wipe（RegionCopy 群）。A は動かさず、新しく現れた軸平行の帯だけ B を転送する。
// 転送は 2 回（A 全面 + B の帯）。
void draw_wipe(SDL_Renderer* renderer, const core::WipeParams& params, Progress progress,
               SDL_Texture* from, SDL_Texture* to, int width, int height) {
  copy_full(renderer, from, width, height);
  const SDL_Rect reveal = wipe_reveal_rect(params.angleDeg, progress, width, height);
  const SDL_Rect to_base = cover_base_rect(size_of(to), width, height);
  copy_region(renderer, to, to_base, reveal, reveal, width, height);
}

// Blinds（RegionCopy 群）。A 全面 + スリット N 枚 = 1 + N 回の転送。
void draw_blinds(SDL_Renderer* renderer, const core::BlindsParams& params, Progress progress,
                 SDL_Texture* from, SDL_Texture* to, int width, int height) {
  copy_full(renderer, from, width, height);
  const SDL_Rect to_base = cover_base_rect(size_of(to), width, height);
  // ★0 枚は描くものが無くなり画面が固まる。core が下限 4 を保証しているが、
  //   ここでも 1 未満は 1 に丸めて「何も起きない」を作らない。
  const std::size_t slats = std::max<std::size_t>(params.slats, 1U);
  for (std::size_t i = 0; i < slats; ++i) {
    const SDL_Rect reveal = blinds_reveal_rect(params, i, progress, width, height);
    copy_region(renderer, to, to_base, reveal, reveal, width, height);
  }
}

}  // namespace

// ---------------------------------------------------------------------------
// 幾何
// ---------------------------------------------------------------------------

int slide_offset_px(core::SlideDirection direction, Progress progress, int width,
                    int height) noexcept {
  switch (direction) {
    case core::SlideDirection::FromLeft:
    case core::SlideDirection::FromRight:
      return scaled(width, progress.value());
    case core::SlideDirection::FromTop:
    case core::SlideDirection::FromBottom:
      return scaled(height, progress.value());
  }
  return 0;
}

SDL_Rect wipe_reveal_rect(std::uint16_t angleDeg, Progress progress, int width,
                          int height) noexcept {
  const float p = progress.value();
  switch (angleDeg) {
    case 90: {  // 上 → 下
      const int band = scaled(height, p);
      return SDL_Rect{0, 0, width, band};
    }
    case 180: {  // 右 → 左
      const int band = scaled(width, p);
      return SDL_Rect{width - band, 0, band, height};
    }
    case 270: {  // 下 → 上
      const int band = scaled(height, p);
      return SDL_Rect{0, height - band, width, band};
    }
    case 0:
    default: {  // 左 → 右。★core は {0,90,180,270} を保証するが、
                //   万一の値でも「何も描かない」より既定の向きへ倒す。
      const int band = scaled(width, p);
      return SDL_Rect{0, 0, band, height};
    }
  }
}

SDL_Rect blinds_reveal_rect(const core::BlindsParams& params, std::size_t index,
                            Progress progress, int width, int height) noexcept {
  const int slats = static_cast<int>(std::max<std::size_t>(params.slats, 1U));
  const int i = static_cast<int>(index);
  if (i < 0 || i >= slats) {
    return SDL_Rect{0, 0, 0, 0};
  }
  const float p = progress.value();

  if (params.vertical) {
    // 縦のスリット。x 方向に N 分割し、各帯の左端から右へ開く。
    const int x0 = (i * width) / slats;
    const int x1 = ((i + 1) * width) / slats;
    const int open = scaled(x1 - x0, p);
    return SDL_Rect{x0, 0, open, height};
  }
  const int y0 = (i * height) / slats;
  const int y1 = ((i + 1) * height) / slats;
  const int open = scaled(y1 - y0, p);
  return SDL_Rect{0, y0, width, open};
}

core::NormRect ken_burns_rect_at(const core::KenBurnsParams& params,
                                 Progress progress) noexcept {
  const float p = progress.value();
  // ★整数（千分率）で補間する。start + (end - start) * p を float でやると
  //   FMA の対象になり、コンパイラによって最下位ビットが食い違う。
  const auto lerp = [p](std::uint16_t a, std::uint16_t b) {
    const int from = static_cast<int>(a);
    const int to = static_cast<int>(b);
    const int delta = to - from;
    const float step = static_cast<float>(delta) * p;
    const int moved = static_cast<int>(step + (step >= 0.0F ? 0.5F : -0.5F));
    return static_cast<std::uint16_t>(std::clamp(from + moved, 0, 1000));
  };

  core::NormRect out{};
  out.xMilli = lerp(params.start.xMilli, params.end.xMilli);
  out.yMilli = lerp(params.start.yMilli, params.end.yMilli);
  out.wMilli = lerp(params.start.wMilli, params.end.wMilli);
  out.hMilli = lerp(params.start.hMilli, params.end.hMilli);
  return out;
}

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------

void draw_full(SDL_Renderer* renderer, SDL_Texture* texture, int width, int height) {
  if (renderer == nullptr || width <= 0 || height <= 0) {
    return;
  }
  const RendererStateGuard renderer_guard(renderer);
  const TextureStateGuard texture_guard(texture);
  copy_full(renderer, texture, width, height);
}

void draw_transition(SDL_Renderer* renderer, const core::TransitionPlan& plan,
                     const core::TransitionParams& params, Progress progress,
                     SDL_Texture* from, SDL_Texture* to, int width, int height) {
  if (renderer == nullptr || width <= 0 || height <= 0) {
    return;
  }
  // ★ここで確保した guard が、どの経路で抜けても状態を戻す。
  const RendererStateGuard renderer_guard(renderer);
  const TextureStateGuard from_guard(from);
  const TextureStateGuard to_guard(to);

  // ★kind に対応する params のメンバだけを読む（TransitionParams は variant ではない）。
  switch (plan.kind) {
    case core::TransitionKind::CrossFade:
      draw_cross_fade(renderer, progress, from, to, width, height);
      return;
    case core::TransitionKind::FadeThroughBlack:
      draw_fade_through_black(renderer, progress, from, to, width, height);
      return;
    case core::TransitionKind::Slide:
      draw_slide(renderer, params.slide, progress, from, to, width, height);
      return;
    case core::TransitionKind::Wipe:
      draw_wipe(renderer, params.wipe, progress, from, to, width, height);
      return;
    case core::TransitionKind::Blinds:
      draw_blinds(renderer, params.blinds, progress, from, to, width, height);
      return;
  }
  // 到達しない。念のため遷移先を出す（画面を固めない）。
  copy_full(renderer, to, width, height);
}

void draw_ken_burns(SDL_Renderer* renderer, const core::KenBurnsParams& params,
                    Progress progress, SDL_Texture* photo, int width, int height) {
  if (renderer == nullptr || width <= 0 || height <= 0) {
    return;
  }
  const RendererStateGuard renderer_guard(renderer);
  const TextureStateGuard texture_guard(photo);

  if (photo == nullptr) {
    SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
    SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
    const SDL_Rect full{0, 0, width, height};
    SDL_RenderFillRect(renderer, &full);
    return;
  }

  const SDL_Rect base = cover_base_rect(size_of(photo), width, height);
  if (base.w <= 0 || base.h <= 0) {
    return;
  }
  const core::NormRect rect = ken_burns_rect_at(params, progress);

  // 正規化矩形を cover 済みの領域内へ写す。
  // ★NormRect は w == h（千分率）だが base.w != base.h なので、
  //   画面と同じアスペクトの矩形になる = 写真の縦横比が保たれる。
  SDL_Rect src{};
  src.x = base.x + static_cast<int>((static_cast<long long>(rect.xMilli) * base.w) / 1000);
  src.y = base.y + static_cast<int>((static_cast<long long>(rect.yMilli) * base.h) / 1000);
  src.w = static_cast<int>((static_cast<long long>(rect.wMilli) * base.w) / 1000);
  src.h = static_cast<int>((static_cast<long long>(rect.hMilli) * base.h) / 1000);
  src.w = std::max(src.w, 1);
  src.h = std::max(src.h, 1);
  // 端が base をはみ出さないよう吸着させる（丸めで 1px はみ出すと縁が伸びる）。
  src.x = std::clamp(src.x, base.x, base.x + base.w - src.w);
  src.y = std::clamp(src.y, base.y, base.y + base.h - src.h);

  SDL_SetTextureBlendMode(photo, SDL_BLENDMODE_NONE);
  SDL_SetTextureAlphaMod(photo, 255);
  SDL_SetTextureColorMod(photo, 255, 255, 255);
  const SDL_Rect dst{0, 0, width, height};
  SDL_RenderCopy(renderer, photo, &src, &dst);
}

void draw_dwell(SDL_Renderer* renderer, const core::DwellPlan& plan,
                const core::KenBurnsParams& kenBurns, Progress progress, SDL_Texture* photo,
                int width, int height) {
  switch (plan.effect) {
    case core::DwellEffect::KenBurns:
      draw_ken_burns(renderer, kenBurns, progress, photo, width, height);
      return;
    case core::DwellEffect::None:
      draw_full(renderer, photo, width, height);
      return;
  }
  draw_full(renderer, photo, width, height);
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

std::unique_ptr<Canvas> Canvas::create(SDL_Renderer* renderer, int width, int height,
                                       std::string& error) {
  if (renderer == nullptr || width <= 0 || height <= 0) {
    error = "Canvas::create の引数が不正です";
    return nullptr;
  }
  if (SDL_RenderTargetSupported(renderer) == SDL_FALSE) {
    error = "このレンダラはレンダーターゲットに対応していません";
    return nullptr;
  }
  SDL_Texture* texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGBA8888,
                                           SDL_TEXTUREACCESS_TARGET, width, height);
  if (texture == nullptr) {
    error = std::string("キャンバス用テクスチャを作れませんでした: ") + SDL_GetError();
    return nullptr;
  }
  // 既定は不透明合成。キャンバスを画面へ出すときに alpha を持ち込まない。
  SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_NONE);

  std::unique_ptr<Canvas> canvas(new Canvas());
  canvas->texture_ = texture;
  canvas->width_ = width;
  canvas->height_ = height;
  return canvas;
}

Canvas::~Canvas() {
  if (texture_ != nullptr) {
    SDL_DestroyTexture(texture_);
  }
}

Canvas::Scope::Scope(SDL_Renderer* renderer, const Canvas& canvas) noexcept
    : renderer_(renderer), previous_(nullptr), ok_(false) {
  if (renderer_ == nullptr) {
    return;
  }
  previous_ = SDL_GetRenderTarget(renderer_);
  ok_ = (SDL_SetRenderTarget(renderer_, canvas.raw()) == 0);
  if (!ok_) {
    previous_ = nullptr;
  }
}

Canvas::Scope::~Scope() {
  if (renderer_ != nullptr && ok_) {
    SDL_SetRenderTarget(renderer_, previous_);
  }
}

// ---------------------------------------------------------------------------
// RenderCaps の実測
// ---------------------------------------------------------------------------

const char* toString(CapsProbeMethod method) noexcept {
  switch (method) {
    case CapsProbeMethod::Measured:
      return "Measured";
    case CapsProbeMethod::ApiOnly:
      return "ApiOnly";
    case CapsProbeMethod::Undetermined:
      return "Undetermined";
  }
  return "?";
}

namespace {

// 「中間色になっているか」の判定。両端に張り付いていたら混ざっていない。
// 余裕を大きめ (16..239) に取るのは、レンダラごとに丸めが 1〜2 違うため。
[[nodiscard]] bool is_mixed(int value) noexcept { return value > 16 && value < 239; }

struct ApiProbe {
  bool determined = false;  // false = 何も試せなかった
  bool blend = true;
  const char* note = "";
};

// API の応答だけで判定する（レンダーターゲットが使えない環境）。
[[nodiscard]] ApiProbe probe_api_only(SDL_Renderer* renderer) {
  SDL_Texture* probe = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGBA8888,
                                         SDL_TEXTUREACCESS_STATIC, 4, 4);
  if (probe == nullptr) {
    // ★判定不能。既定 true へ倒す（理由は transitions.hpp の RenderCaps の節）。
    return ApiProbe{false, true, "テクスチャを作れず判定できませんでした（既定 true）"};
  }

  bool ok = true;
  SDL_BlendMode blend = SDL_BLENDMODE_NONE;
  Uint8 alpha = 0;
  ok = ok && (SDL_SetTextureBlendMode(probe, SDL_BLENDMODE_BLEND) == 0);
  ok = ok && (SDL_GetTextureBlendMode(probe, &blend) == 0) && (blend == SDL_BLENDMODE_BLEND);
  ok = ok && (SDL_SetTextureAlphaMod(probe, 128) == 0);
  ok = ok && (SDL_GetTextureAlphaMod(probe, &alpha) == 0) && (alpha == 128);
  SDL_DestroyTexture(probe);

  ok = ok && (SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND) == 0);

  return ApiProbe{true, ok,
                  ok ? "API は blend を受け付けました（実測はしていません）"
                     : "API が blend の設定を拒否しました"};
}

}  // namespace

RenderCapsReport probe_render_caps(SDL_Renderer* renderer) {
  RenderCapsReport report;
  report.caps.blend = true;  // 既定（理由は transitions.hpp）
  if (renderer == nullptr) {
    report.method = CapsProbeMethod::Undetermined;
    report.note = "renderer が null のため判定できませんでした（既定 true）";
    return report;
  }

  const RendererStateGuard renderer_guard(renderer);

  // --- 1. 実測 ---------------------------------------------------------------
  if (SDL_RenderTargetSupported(renderer) == SDL_TRUE) {
    SDL_Texture* target = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGBA8888,
                                            SDL_TEXTUREACCESS_TARGET, 4, 4);
    SDL_Texture* source = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGBA8888,
                                            SDL_TEXTUREACCESS_TARGET, 4, 4);
    if (target != nullptr && source != nullptr) {
      SDL_Texture* const previous = SDL_GetRenderTarget(renderer);
      bool read_ok = true;
      Uint32 pixels[16] = {};
      const SDL_Rect all{0, 0, 4, 4};

      // (a) 不透明な緑のテクスチャを用意する。
      if (SDL_SetRenderTarget(renderer, source) == 0) {
        SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
        SDL_SetRenderDrawColor(renderer, 0, 255, 0, 255);
        SDL_RenderClear(renderer);
      } else {
        read_ok = false;
      }

      // (b) 描画色の blend: 赤 → 半透明の青。
      int draw_r = -1;
      int draw_b = -1;
      if (read_ok && SDL_SetRenderTarget(renderer, target) == 0) {
        SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
        SDL_SetRenderDrawColor(renderer, 255, 0, 0, 255);
        SDL_RenderClear(renderer);
        SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_BLEND);
        SDL_SetRenderDrawColor(renderer, 0, 0, 255, 128);
        SDL_RenderFillRect(renderer, &all);
        if (SDL_RenderReadPixels(renderer, nullptr, SDL_PIXELFORMAT_ARGB8888, pixels,
                                 4 * 4) == 0) {
          draw_r = static_cast<int>((pixels[0] >> 16) & 0xFFU);
          draw_b = static_cast<int>(pixels[0] & 0xFFU);
        } else {
          read_ok = false;
        }
      } else {
        read_ok = false;
      }

      // (c) テクスチャの alpha mod: 赤の上へ alpha=128 の緑。
      //     ★CrossFade が実際に使う経路はこちら。(b) だけでは検査にならない。
      int tex_r = -1;
      int tex_g = -1;
      if (read_ok) {
        SDL_SetRenderDrawBlendMode(renderer, SDL_BLENDMODE_NONE);
        SDL_SetRenderDrawColor(renderer, 255, 0, 0, 255);
        SDL_RenderClear(renderer);
        SDL_SetTextureBlendMode(source, SDL_BLENDMODE_BLEND);
        SDL_SetTextureAlphaMod(source, 128);
        SDL_RenderCopy(renderer, source, nullptr, &all);
        if (SDL_RenderReadPixels(renderer, nullptr, SDL_PIXELFORMAT_ARGB8888, pixels,
                                 4 * 4) == 0) {
          tex_r = static_cast<int>((pixels[0] >> 16) & 0xFFU);
          tex_g = static_cast<int>((pixels[0] >> 8) & 0xFFU);
        } else {
          read_ok = false;
        }
      }

      SDL_SetRenderTarget(renderer, previous);
      SDL_DestroyTexture(source);
      SDL_DestroyTexture(target);

      if (read_ok) {
        report.method = CapsProbeMethod::Measured;
        report.drawMixR = draw_r;
        report.drawMixB = draw_b;
        report.texMixR = tex_r;
        report.texMixG = tex_g;
        const bool draw_blend_ok = is_mixed(draw_r) && is_mixed(draw_b);
        const bool tex_blend_ok = is_mixed(tex_r) && is_mixed(tex_g);
        report.caps.blend = draw_blend_ok && tex_blend_ok;
        report.note = report.caps.blend
                          ? "実測: 描画色・テクスチャ alpha ともに混色を確認"
                          : "実測: 混色が確認できませんでした（blend 群を除外します）";
        return report;
      }
    } else {
      if (source != nullptr) {
        SDL_DestroyTexture(source);
      }
      if (target != nullptr) {
        SDL_DestroyTexture(target);
      }
    }
  }

  // --- 2. API 応答 / 3. 判定不能 ----------------------------------------------
  const ApiProbe api = probe_api_only(renderer);
  report.caps.blend = api.blend;
  report.note = api.note;
  report.method = api.determined ? CapsProbeMethod::ApiOnly : CapsProbeMethod::Undetermined;
  return report;
}

}  // namespace ambient::render
