// ambient/core/image/downsample.hpp — 画像の前処理（寸法 probe / 予算判定 / box 縮小）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// ★なぜ core に置くのか
// ============================================================================
//
// ここにあるのは全部 **純粋関数** であり、ピクセル完全一致でテストできる数少ない
// 処理だから。unit_tests は ambient_core だけをリンクするので、ここに置かないと
// テストできない（render 層に置くと SDL を必要とするテストになる）。
//
// ★「写真のピクセルデータを Core に持たせない」規律との関係:
//   これらの関数は呼び出し側が用意したバッファを **読む / 書く** だけで、
//   **何も保持しません**。規律が禁じているのは Core が画像バッファを
//   所有・保持することです。
//
// ============================================================================
// ★ファイル名について（downsample.hpp に probe と予算判定が同居している理由）
// ============================================================================
//
// Wave 2C では native-display/CMakeLists.txt が凍結されており（2レーンが同時に
// 作業中のため）、**新しい .cpp を追加できません**。どの .cpp からも include
// されないヘッダは移植性ゲート（tools/check-esp-portability.sh）の対象外になり
// 「通ったつもり」になるので、probe / 予算判定を別ヘッダに切り出すことはせず、
// 既に CMakeLists にある src/core/image/downsample.cpp に同居させています。
// CMakeLists が解凍されたら probe.{hpp,cpp} へ分割してよい。
//
// ============================================================================
// ★なぜ nearest ではなく box filter なのか
// ============================================================================
//
// derivative は 2560x1707。これを 1280x720 へ cover 縮小すると約 2 倍縮小に
// なり、nearest では画素を間引くだけなのでモアレ（縞）が出ます。
// box filter は縮小後 1 画素に対応する元の矩形を平均するので、間引きで消える
// 高周波が平均へ畳み込まれ、モアレが出ません。

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>

namespace ambient::core {

// ----------------------------------------------------------------------------
// 寸法
// ----------------------------------------------------------------------------

struct ImageSize {
  std::uint32_t width{0};
  std::uint32_t height{0};

  [[nodiscard]] bool operator==(const ImageSize&) const noexcept = default;
};

// 受け入れる 1 辺の最大値。
//
// ★これは「大きい画像を弾くための予算」ではなく **オーバーフロー対策** である。
//   PNG の IHDR は幅・高さとも 2^31-1 まで表現できるので、素朴に w*h*4 を
//   計算すると 64bit でも溢れる。probe の段階で 1 辺を 65535（JPEG の上限）へ
//   制限しておけば 65535*65535*4 = 1.7e10 で uint64 に収まる。
//
//   ★寸法は **認証の無い LAN 上のサーバから来る攻撃者制御下の値** である。
//     「実際にはそんな画像は来ない」は根拠にならない。
inline constexpr std::uint32_t kMaxImageDimension = 65535;

// 圧縮画像の **ヘッダだけ** を読んで寸法を返す。デコードはしない。
//
// ★これが「デコード前に予算判定する」ための入口である。
//   デコードしてから大きさを知る実装は、24MP の原本 1 枚で 96MB を確保して
//   しまってから「大きすぎた」と気づくことになる。
//
// 対応形式: JPEG（SOFn マーカー） / PNG（IHDR）。
// それ以外・壊れている・途中で切れている・寸法が 0 or kMaxImageDimension 超は
// nullopt。**例外は投げない**（Core の規律）。
[[nodiscard]] std::optional<ImageSize> probeImageSize(
    std::span<const std::uint8_t> bytes) noexcept;

// ----------------------------------------------------------------------------
// デコード予算
// ----------------------------------------------------------------------------
//
// ★実測（docs/CORE_CONTRACTS.md / 企画の実測メモ）:
//     derivative 2560x1707 -> RGBA 展開で 17,479,680 バイト（約 16.7MiB）
//     ローカル原本はサーバ側でリサイズされずに来る。24MP なら約 96MB
//     （リサイズ用のクエリパラメータも専用エンドポイントも **存在しない**）
//
//   先読み 2 枚 + 現在の 3 枚を同時に展開すると約 50MB。原本が混ざると破綻する。
//   よって「デコード後の RGBA バイト数」に上限を掛け、超えるものは
//   **一時除外**（クラッシュさせず、次の写真へ進む）にする。

// 既定の上限。derivative（約 16.7MiB）は通し、24MP 原本（約 92MiB）は弾く。
inline constexpr std::uint64_t kDefaultMaxDecodedBytes = 32ULL * 1024 * 1024;

struct DecodeBudget {
  std::uint64_t maxDecodedBytes{kDefaultMaxDecodedBytes};
};

// RGBA8888 で展開したときのバイト数。0 = 寸法が不正（幅か高さが 0、または上限超）。
[[nodiscard]] std::uint64_t decodedBytesRGBA(ImageSize size) noexcept;

// 予算に収まるか。寸法が不正なら false。
[[nodiscard]] bool fitsDecodeBudget(ImageSize size, const DecodeBudget& budget) noexcept;

// ----------------------------------------------------------------------------
// box filter による縮小
// ----------------------------------------------------------------------------
//
// RGBA8888（1 画素 4 バイト、チャンネル順は問わない ― 各チャンネルを独立に
// 平均するだけなので RGBA でも BGRA でも同じ結果になる）。
//
// 出力画素 (dx, dy) に対応する入力の矩形は
//     x0 = dx*srcW/dstW, x1 = (dx+1)*srcW/dstW   （整数除算、x1<=x0 なら x0+1）
// で、その矩形内の全画素を単純平均する。丸めは四捨五入（+area/2 してから除算）。
//
// stride を受け取るのは **cover クロップ後の部分矩形をそのまま渡せるように**
// するため。呼び出し側は src に矩形の左上画素のアドレスを渡し、srcStride に
// 元画像の 1 行のバイト数を渡す（コピーが 1 回減る）。
//
// 契約:
//   * 縮小専用。dst のどちらかの辺が src より **大きい** 場合は false を返す
//     （等倍は許可。その場合は矩形が 1x1 になるので単純コピーと同じ結果）
//   * src / dst が null、幅か高さが 0、stride が幅*4 未満なら false
//   * 領域が重なっていないことは呼び出し側の責任（検査しない）
//   * ★例外を投げない。確保もしない。
[[nodiscard]] bool downsampleBoxRGBA(const std::uint8_t* src, ImageSize srcSize,
                                     std::size_t srcStride, std::uint8_t* dst,
                                     ImageSize dstSize, std::size_t dstStride) noexcept;

}  // namespace ambient::core
