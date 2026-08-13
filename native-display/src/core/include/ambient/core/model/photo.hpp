// ambient/core/model/photo.hpp — /api/photos のデータ型
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
// ★写真の**ピクセルデータを Core に持たせないこと**。
//   Core が扱うのは識別子とメタデータだけ。画像バッファは Renderer 層。
//
// ============================================================================
// ★実測が正典 ― 企画書ではなくこちらに合わせること
// ============================================================================
//
// /api/photos は **裸の JSON 配列** である。`{items: [...]}` ではない。
//
//   [ {"file": "...", "time": "...", "trip": 0, ...}, ... ]
//
// パーサが`{items:...}`を期待していると、多数の写真があっても0件として動く。
// しかもエラーにはならないので気づきにくい。

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace ambient::core {

// ----------------------------------------------------------------------------
// ★URL 組み立ての非対称 ― ここを共通化すると 404 になる
// ----------------------------------------------------------------------------
//
//   /api/photos       の `file`  → 先頭スラッシュ **無し** の相対パス
//                                   例: "photos/2024/IMG_0001.jpg"
//   /api/admin/photos の `thumb` → 先頭スラッシュ **あり**
//                                   例: "/thumbs/IMG_0001.jpg"
//
// 素朴に base + path で連結すると、前者は "http://host" + "photos/..." になって
// パスが繋がらず、後者は "http://host/" + "/thumbs/..." で "//" になる。
// どちらも 404 になり、しかも原因が見えにくい。
//
// 型で分ける。混ぜて渡すとコンパイルエラーになる。
// 共通の joinUrl(base, std::string) を作らないこと。

// /api/photos の `file`。先頭スラッシュ無しの相対パス。
struct PhotoRelPath {
  std::string value;

  [[nodiscard]] bool operator==(const PhotoRelPath&) const noexcept = default;
};

// /api/admin/photos の `thumb`。先頭スラッシュ **あり** の絶対パス。
//
// ----------------------------------------------------------------------------
// ★★ なぜこの型だけ構築時に検証するのか（SSRF 対策）★★
// ----------------------------------------------------------------------------
//
// この値の出どころは **認証の無い LAN 上の GeoFrame server** である
// （README「セキュリティ運用前提」を参照）。信頼できない入力源であり、
// 攻撃者が値を偽装しうる前提で扱わなければならない。
//
// joinUrl(base, AdminAbsPath) は「先頭スラッシュあり」を前提に区切りを補わず
// そのまま連結する。したがって先頭が '/' でない値が入ると、連結結果の
// **authority 部（接続先ホスト）が乗っ取られる**:
//
//   "@evil.example/x.jpg"   -> "http://192.0.2.5:8080@evil.example/x.jpg"
//                              RFC 3986 では authority は最後の '@' で分割される
//                              ため、実際の接続先は evil.example
//   ".evil.example/x.jpg"   -> "http://geoframe.local.evil.example/x.jpg"
//                              （base がポート無しの場合。evil.example の DNS 配下）
//   "//evil.example/x.jpg"  -> プロトコル相対 URL。joinUrl は base の末尾スラッシュを
//                              **全部** 落とすので、base が "http://" なら "http:" まで
//                              縮み、連結結果は "http://evil.example/x.jpg" になる
//
// libcurl も esp_http_client もこの URL を素直に解釈する。
//
// 対になる PhotoRelPath 側は検証不要である。あちらは out += '/' を先に入れてから
// 連結するので、value が何であっても authority 部には食い込めない。
// 検証が要るのはこちらだけ、というのがこの非対称の理由。
//
// 検証は joinUrl の入口ではなく **構築時** に置く。入口で弾く方式だと、
// 別の使い方（ログ出力、別の URL 組み立て）をされたときに漏れるため。
// 「不正な値を持つ AdminAbsPath は存在しえない」を型の不変条件にする。
//
// ★検証失敗を例外で表現しないこと（docs/PORTABILITY.md: Core の制御フローに
//   例外を使わない。ESP32 は緊急用プールが 0 で、ヒープ枯渇時の throw が
//   abort() になる）。make() が std::optional を返す。
class AdminAbsPath {
 public:
  // 唯一の構築手段。検証を通らなければ nullopt。
  //
  // 弾く条件:
  //   * 空文字列       （連結しても区切りが無く、base と地続きになる）
  //   * 先頭が '/' でない（authority 部を乗っ取られる）
  //   * 先頭が "//"     （プロトコル相対 URL になり別ホストへ行く）
  [[nodiscard]] static std::optional<AdminAbsPath> make(std::string_view raw);

  [[nodiscard]] const std::string& value() const noexcept { return value_; }

  [[nodiscard]] bool operator==(const AdminAbsPath&) const noexcept = default;

 private:
  // private なので make() を通らずに構築できない（不変条件の担保）。
  explicit AdminAbsPath(std::string_view raw) : value_(raw) {}

  std::string value_;
};

// base は "http://host:port"（末尾スラッシュ無しを想定。あっても正規化する）。
[[nodiscard]] std::string joinUrl(std::string_view base, const PhotoRelPath& path);
[[nodiscard]] std::string joinUrl(std::string_view base, const AdminAbsPath& path);

// ----------------------------------------------------------------------------
// 写真
// ----------------------------------------------------------------------------

// 写真 ID。
// ★スケジューラのキューは **ID** で持つ（配列添字ではない）。
//   manifest を取り直して配列の添字が動いても ID は動かないので、
//   再生位置が巻き戻らない（出典: scheduler.js の設計コメント）。
using PhotoId = std::string;

struct PhotoItem {
  PhotoId id;

  // ★キーは `file`。先頭スラッシュ無し（上記の非対称を参照）。
  PhotoRelPath file;

  // ★キーは `time`。**`takenAt` ではない**。
  //   形式は "YYYY-MM-DD HH:MM"（区切りは半角スペース、秒もタイムゾーンも無い）。
  //   生文字列のまま持つ。解釈は parsePhotoTime() を通すこと。
  std::string time;

  // トリップ番号。1 ラウンドにトリップあたり最大 kTripMax 枚出す単位。
  std::int32_t trip{0};

  // 新着ブーストの基準時刻。JSON のキーは `fetched_at`（スネークケース）。
  // 空文字列 = 不明（ブーストなし）。
  std::string fetchedAt;

  bool fav{false};

  // 写真品質解析。完全重複の非代表はserverが除外するので、ここへ来るalternateは
  // 類似・連写グループの別カットであり、有限減点で表示機会を残す。
  bool qualityRecommended{false};
  bool qualityAlternate{false};
  float qualityScore{0.0F};

  // 位置と表示用メタデータ（出典: app/static/js/data.js:44-60）。
  // ★地図表示用途のみ。Core で三角関数などの重い計算はしないこと。
  double lat{0.0};
  double lon{0.0};
  std::string title;
  std::string place;
  std::string pref;
  std::string city;

  // 位置が EXIF ではなく推定であることを示す。
  bool inferred{false};
};

// /api/photos 全体（裸の配列）。
// ★配列順は撮影時刻順である（scheduler.js がトリップの連続 run 検出に依存している）。
//   並べ替えて保持しないこと。
struct PhotoManifest {
  std::vector<PhotoItem> items;

  // id -> 添字の逆引きを O(log n) にするための索引。
  // items の添字を **id の昇順**に並べたもの。
  //
  // ★なぜ std::map ではなくこれなのか:
  //   indexOf() は noexcept である。noexcept の中でノードを確保する map を
  //   組み立てることはできない（確保失敗が throw になる）。索引の構築は
  //   例外を投げてよいここ（rebuildIndex）に寄せ、参照側は確保しない。
  //   設計値2000件 x 4バイトで8KB程度。写真本体のメタデータより遥かに小さい。
  //
  // ★空のままでも **正しく動く**（indexOf が線形探索へ落ちるだけ）。
  //   1D のパーサが rebuildIndex() を呼び忘れても壊れず、遅くなるだけにしてある。
  std::vector<std::uint32_t> idOrder;

  // idOrder を作り直す。items を差し替えたら呼ぶこと。
  void rebuildIndex();

  // 同期の要否を判定するためのシグネチャ（出典: scheduler.js の photoSig）。
  //
  // 形式は "<件数>:<ハッシュを 36 進で>"。ハッシュは全 ID を連結した
  // 32bit の h = h*31 + byte（出典の JS が (h*31 + code)|0 で回しているものと
  // 同一。JS の |0 は 2^32 の剰余なので uint32 演算と厳密に一致する）。
  //
  // ★暗号学的強度は不要。「manifest が変わったこと」の検出にしか使わない。
  //   偽陰性（違う集合が同じ sig）が起きても、新着は shown に無いので
  //   PRI_NEVER で次ラウンドに選出される（劣化のしかたが安全側）。
  [[nodiscard]] std::string signature() const;
};

// id -> 添字の逆引き。見つからなければ nullopt。
// ★見つからない ID は「捨てて次へ」進むこと（manifest から消えた写真）。
//   例外を投げたり停止したりしないこと。
//
// idOrder があれば二分探索、無ければ線形探索へ落ちる。
// ★二分探索が外したときも線形探索で確認する。索引が古い（items を差し替えて
//   rebuildIndex() を呼び忘れた）場合に **誤って nullopt を返さない** ため。
//   古い索引の代償は速度だけであり、正しさは失われない。
[[nodiscard]] std::optional<std::size_t> indexOf(const PhotoManifest& manifest,
                                                 std::string_view id) noexcept;

}  // namespace ambient::core
