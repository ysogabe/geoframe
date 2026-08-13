// 5種のトランジション描画と滞在効果（Wave 2B）。
//
// ★進捗は必ず ambient::core::Progress を受け取ること。
//   Progress は fromElapsed(elapsed, duration) からしか作れず、フレーム番号を
//   渡す経路が型として存在しない。Tab5 は Mac より遥かに低い fps でしか
//   描けない可能性が高く、フレーム数駆動だと演出が間延びする。
//
// ============================================================================
// この層の立ち位置 ― 「何を描くか」は core が決めている
// ============================================================================
//
// 演出の抽選・パラメータ導出・duration はすべて ambient_core
// (ambient/core/transition.hpp) が決定済みである。ここは
// 「TransitionPlan + TransitionParams + Progress を受け取って SDL で描く」
// だけの層で、判断を持たない。
//
//   ★kind に対応する params のメンバだけを読むこと（TransitionParams は
//     variant ではなくタグ + 素の struct）。
//
// ============================================================================
// ★CompositeClass を嘘にしないこと ― 領域コピー群は本当に領域コピーで描く
// ============================================================================
//
// core は演出を 2 群に分類している（transition.hpp 冒頭）。
//
//   Blend      : CrossFade / FadeThroughBlack / KenBurns
//   RegionCopy : Slide / Wipe / Blinds
//
// この分類は「Tab5 の blend が重かったら重みだけ下げればよい」という運用の
// 土台である。**RegionCopy 群の実装が内部で alpha blend を使うと、その土台が
// 崩れる**（重みを下げても blend の負荷が消えない）。
//
// そこで本実装は RegionCopy 群を描く直前に
//   SDL_SetTextureBlendMode(tex, SDL_BLENDMODE_NONE)
//   SDL_SetTextureAlphaMod(tex, 255)
// を明示的に設定する。**blend を「使っていない」のではなく「使えない状態にして
// から描く」**ので、うっかり alpha を混ぜたらその場で見た目が壊れて気づける。
// 転送回数は Slide=2 / Wipe=2 / Blinds=1+N、いずれも SDL_RenderCopy のみ。
//
// ============================================================================
// ★イージングを入れていない理由
// ============================================================================
//
// 進捗はすべて線形に使う（smoothstep などを噛ませない）。理由は 3 つ。
//
//  1. Wipe の境界が progress に線形追従することが render_tests の構造
//     アサーションであり、契約として外に見えている。
//  2. 低 fps でこそイージングは害になる。15fps では 700ms が 10 サンプルしか
//     無く、ease-in-out は中央の 1 サンプルあたりの移動量をさらに増やす。
//     「動きではなく飛びに見える」（transition.hpp【2A が決めたこと 2】(b)）を
//     悪化させる方向である。
//  3. 「fps を変えても同じカーブを疎にサンプルするだけ」という性質を
//     目視で確かめるとき、カーブが単純なほど確認が容易い。
//
// 実機で見て必要になったらここに足すこと（core 側には持ち込まないこと。
// core は「いつ・どれを」だけを決める）。
//
// ============================================================================
// ★Wipe の角度の意味 ― Tab5 移植時はここに合わせること
// ============================================================================
//
// core が保証しているのは「値が kWipeAngles = {0, 90, 180, 270} のいずれか」
// までで、向きの意味は描画側の取り決めである。本実装は次のとおり。
//
//   0   : 境界が左 → 右へ進む（遷移先が左端から現れる）
//   90  : 境界が上 → 下へ進む
//   180 : 境界が右 → 左へ進む
//   270 : 境界が下 → 上へ進む
//
// 自由角にしないこと。斜めの境界は矩形転送で表せず走査線ごとの転送になり、
// Wipe を RegionCopy と分類した前提が崩れる（transition.hpp）。

#pragma once

#include <SDL.h>

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>

#include "ambient/core/transition.hpp"

namespace ambient::render {

// ---------------------------------------------------------------------------
// RenderCaps の実測
// ---------------------------------------------------------------------------
//
// core::RenderCaps は「そもそも描けるか」というプラットフォームの事実であり、
// **描画側が申告する**（重みテーブルとは由来が違う。transition.hpp 参照）。
// ここが SDL の実能力を調べて埋める。
//
// 判定は 3 段階で、上から順に試す。
//
//  1. 実測 (Measured): レンダーターゲットが使えるなら 4x4 のターゲットを作り、
//     ①不透明な赤で塗る → 半透明の青を FillRect → 結果が赤でも青でもない
//       中間色になること（**描画色の blend**。FadeThroughBlack が使う経路）
//     ②不透明な緑のテクスチャを alpha=128 で RenderCopy → 赤と緑の中間色に
//       なること（**テクスチャの alpha mod**。CrossFade が使う経路）
//     の両方を満たしたときだけ blend=true とする。
//     ★API が成功を返すことと実際に混ざることは別なので、混色を目で確かめる
//       に等しい検査をここでやる。
//
//  2. API 応答 (ApiOnly): ターゲットが使えない環境では
//     SDL_SetTextureBlendMode / SDL_SetTextureAlphaMod / SDL_SetRenderDrawBlendMode が
//     成功し、かつ設定値が読み戻せることを見る。
//
//  3. 判定不能 (Undetermined): テクスチャすら作れないなど、何も試せなかった場合。
//     **既定は true**（transition.hpp の RenderCaps::blend の既定に合わせる）。
//
// ★3 の既定を true にする理由（非対称なので後から必ず疑われる。ここに残す）:
//   false を返すと CrossFade / FadeThroughBlack / KenBurns が抽選から**静かに
//   消え**、フォールバックまで Wipe に変わる。6 演出のうち 3 つが失われる大きな
//   挙動変化が「測れなかった」から導かれることになる。
//   true を返して実は blend が壊れていた場合は、**画面が目に見えておかしくなる**。
//   静かに縮退するより、うるさく壊れるほうを選ぶ。
enum class CapsProbeMethod : std::uint8_t {
  Measured = 0,  // 実際に描いて読み戻した
  ApiOnly,       // API の成否と設定値の読み戻しだけ
  Undetermined,  // 何も試せなかった（既定値を使った）
};

[[nodiscard]] const char* toString(CapsProbeMethod method) noexcept;

struct RenderCapsReport {
  core::RenderCaps caps{};
  CapsProbeMethod method = CapsProbeMethod::Undetermined;
  // Measured のときだけ意味がある。混色の実測値（不透明な赤 + 半透明の青）。
  int drawMixR = -1;
  int drawMixB = -1;
  // Measured のときだけ意味がある。混色の実測値（不透明な赤 + alpha=128 の緑）。
  int texMixR = -1;
  int texMixG = -1;
  std::string note;  // 人間向けの説明（ログへ出す）
};

// SDL レンダラの実能力を調べて core::RenderCaps を作る。
// ★呼び出し後、レンダラのターゲット・描画色・ブレンドモードは元に戻る。
[[nodiscard]] RenderCapsReport probe_render_caps(SDL_Renderer* renderer);

// ---------------------------------------------------------------------------
// Canvas — 合成用のオフスクリーン面
// ---------------------------------------------------------------------------
//
// ★なぜ必要か: 遷移させたいのは「写真」ではなく「画面 1 枚ぶん」である。
//   キャプションや時計を含んだ完成フレームどうしを混ぜないと、遷移中だけ
//   文字が消えたり前の写真の文字が残ったりする。
//
// ★実測（sdl2-compat 3.4.12 / metal・software 双方）:
//   SDL_SetRenderTarget でテクスチャへ切り替えると論理解像度は 0 になり
//   scale=1・viewport=テクスチャ全面になる。したがって **キャンバスを論理解像度と
//   同じ大きさで作れば、キャンバス内の座標は論理座標とそのまま 1:1 で一致する**。
//   別途 SDL_RenderSetLogicalSize を張り直す必要は無い。
class Canvas {
 public:
  // 失敗時は nullptr を返し、error にメッセージを入れる。
  // ★レンダーターゲット非対応の環境でも失敗する（呼び出し側は縮退すること）。
  [[nodiscard]] static std::unique_ptr<Canvas> create(SDL_Renderer* renderer, int width,
                                                      int height, std::string& error);

  ~Canvas();

  Canvas(const Canvas&) = delete;
  Canvas& operator=(const Canvas&) = delete;
  Canvas(Canvas&&) = delete;
  Canvas& operator=(Canvas&&) = delete;

  [[nodiscard]] SDL_Texture* raw() const noexcept { return texture_; }
  [[nodiscard]] int width() const noexcept { return width_; }
  [[nodiscard]] int height() const noexcept { return height_; }

  // このキャンバスを描画対象にする RAII。スコープを抜けると元の対象へ戻る。
  //
  //   {
  //     Canvas::Scope scope(renderer, *canvas);
  //     draw_screen(...);           // キャンバスへ描かれる
  //   }                             // ここで元の対象（通常は画面）へ戻る
  //
  // ★スクリーンショットを撮る前に必ずスコープを閉じること
  //   （Display::save_screenshot は「現在のレンダーターゲット」を読む）。
  class Scope {
   public:
    Scope(SDL_Renderer* renderer, const Canvas& canvas) noexcept;
    ~Scope();
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;
    Scope(Scope&&) = delete;
    Scope& operator=(Scope&&) = delete;

    [[nodiscard]] bool ok() const noexcept { return ok_; }

   private:
    SDL_Renderer* renderer_;
    SDL_Texture* previous_;
    bool ok_;
  };

 private:
  Canvas() = default;

  SDL_Texture* texture_ = nullptr;
  int width_ = 0;
  int height_ = 0;
};

// ---------------------------------------------------------------------------
// 幾何 ― Tab5 移植時に同じ絵を出すための計算そのもの
// ---------------------------------------------------------------------------
//
// 描画本体から切り出して公開してある。M5GFX 版は転送 API だけを差し替えて
// この計算をそのまま使えばよい（= 見た目が食い違わない）。
// render_tests もここを直接検査する。

// Slide の押し出し量（ピクセル）。横方向の演出なら width、縦なら height に対する量。
[[nodiscard]] int slide_offset_px(core::SlideDirection direction, core::Progress progress,
                                  int width, int height) noexcept;

// Wipe で「新しく現れた」領域（= 遷移先を転送する矩形）。論理座標。
// progress=0 で幅または高さが 0、progress=1 で全画面になる。
[[nodiscard]] SDL_Rect wipe_reveal_rect(std::uint16_t angleDeg, core::Progress progress,
                                        int width, int height) noexcept;

// Blinds の index 番目のスリットで遷移先を転送する矩形。論理座標。
// スリットの境界は (i * 幅) / N の整数演算で決めるので、隙間も重なりも生じない。
[[nodiscard]] SDL_Rect blinds_reveal_rect(const core::BlindsParams& params, std::size_t index,
                                          core::Progress progress, int width,
                                          int height) noexcept;

// KenBurns の、その瞬間の切り出し矩形（正規化）。
// ★千分率の整数で補間する。理由は transition.hpp【2A が決めたこと 4】と同じで、
//   float の積和は FMA の対象になりプラットフォームでビットが食い違うため。
[[nodiscard]] core::NormRect ken_burns_rect_at(const core::KenBurnsParams& params,
                                               core::Progress progress) noexcept;

// ---------------------------------------------------------------------------
// 描画
// ---------------------------------------------------------------------------
//
// from / to は「画面 1 枚ぶん」の内容を持つテクスチャ。Canvas でも、写真の
// テクスチャでも構わない（大きさが違えば cover クロップして使う）。
// from == nullptr は「遷移元が無い」= 黒として扱う。

// 遷移を 1 フレーム描く。plan.kind に対応する params のメンバだけを読む。
//
// ★呼び出し後、from / to のテクスチャ状態（blend mode / alpha mod / color mod）と
//   レンダラの描画色・ブレンドモードは呼び出し前へ復元される。
//   復元は RAII で行っており、途中 return しても漏れない。
//   （漏らすと LivePhotoTextures と同じテクスチャを共有している通常描画が
//     半透明のまま描かれ続け、まったく別の不具合に見える。）
void draw_transition(SDL_Renderer* renderer, const core::TransitionPlan& plan,
                     const core::TransitionParams& params, core::Progress progress,
                     SDL_Texture* from, SDL_Texture* to, int width, int height);

// 滞在効果を 1 フレーム描く。DwellEffect::None なら等倍で描くだけ。
void draw_dwell(SDL_Renderer* renderer, const core::DwellPlan& plan,
                const core::KenBurnsParams& kenBurns, core::Progress progress,
                SDL_Texture* photo, int width, int height);

// KenBurns 単体（plan を持たずに描きたいとき。demo と render_tests が使う）。
void draw_ken_burns(SDL_Renderer* renderer, const core::KenBurnsParams& params,
                    core::Progress progress, SDL_Texture* photo, int width, int height);

// テクスチャを画面全体へ cover 表示する（遷移も滞在も無いときの素の 1 枚）。
// ★image.hpp の draw_cover は Texture 型を要求するので、SDL_Texture* 版をここに置く。
void draw_full(SDL_Renderer* renderer, SDL_Texture* texture, int width, int height);

}  // namespace ambient::render
