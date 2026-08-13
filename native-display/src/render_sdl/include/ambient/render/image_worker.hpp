// ambient/render/image_worker.hpp — 写真の取得・デコード・縮小を行うワーカー
//
// ★なぜ core ではなくここに置くのか:
//   **スレッドを持つから**。ambient_core は単一スレッド前提で <thread> を
//   使えない（docs/PORTABILITY.md）。並行性は Renderer / Transport の責務。
//
// ★この層は transport を include しない:
//   受け取るのは core::IHttpClient の参照だけ。curl も GeoFrameClient も
//   知らない。fixture 実装を渡せばネットワーク非依存で動く。
//
// ============================================================================
// ★★ デコード前に寸法だけ読んで予算判定すること ★★
// ============================================================================
//
// 実測:
//   * derivative は 2560x1707。RGBA 展開で **1 枚 17.5MB**。
//     先読み 2 枚 + 現在で約 50MB。
//   * さらに **ローカル原本はサーバ側でリサイズされずに来る**。
//     24MP なら 1 枚 96MB になりえる。
//   * ★リサイズ用のクエリパラメータも専用エンドポイントも **存在しない**
//     （実測で確認済み）。つまりサーバ側で小さくしてもらう手段が無い。
//
// したがって処理順は必ず:
//
//   DL → probe（ヘッダから寸法だけ）→ 予算判定 → decode → cover crop
//      → box filter 縮小 → 1280x720 RGBA (3.5MB)
//
// 「decode してから大きさを見る」実装は、96MB を確保し終えてから
// 「大きすぎた」と気づくことになる。予算超過は **一時除外**（Excluded）へ
// 落とすこと。**クラッシュさせない**。
//
// ============================================================================
// キャッシュと寿命 — ★shared_ptr であることが安全機構である
// ============================================================================
//
// get() が返すのは `std::shared_ptr<const RgbaImage>` である。
// LRU からの追い出しは **キャッシュ側の参照を落とすだけ**なので、
// トランジション中に「前の写真」を握っている呼び出し側があれば、
// その画像は生き続ける。追い出しが use-after-free になる経路が無い。
//
// setPinned() は安全機構ではなく **再ダウンロードを避けるための最適化**である
// （現在と直前の写真を追い出さないようにする）。
//
// ============================================================================
// スレッドの規律
// ============================================================================
//
//   * IHttpClient は **ワーカースレッド専用**。呼び出し側は同じインスタンスを
//     他のスレッドから使わないこと（libcurl の easy handle は共有できない）。
//   * SDL_Texture の生成は **メインスレッド**の仕事。ここは RGBA バイト列まで
//     しか作らない（SDL_image のデコードはレンダラに触らないので安全）。

#pragma once

#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/http.hpp"
#include "ambient/core/image/downsample.hpp"
#include "ambient/core/model/photo.hpp"

namespace ambient::render {

// 縮小済みの RGBA8888 画像。★これが唯一「ピクセルを持つ」型で、core には無い。
struct RgbaImage {
  int width{0};
  int height{0};
  std::vector<std::uint8_t> pixels;  // width * height * 4

  // 診断用。縮小前（= サーバから来た画像）の寸法。
  std::uint32_t sourceWidth{0};
  std::uint32_t sourceHeight{0};

  [[nodiscard]] std::size_t byteSize() const noexcept { return pixels.size(); }
};

enum class ImageState : std::uint8_t {
  Unknown = 0,  // 一度も要求していない
  Pending,      // キューに載っている / 処理中
  Ready,        // キャッシュにある
  Failed,       // 取得・デコードに失敗（サーバ復帰後に再試行しうる）
  Excluded,     // ★予算超過による一時除外。落とさずに次の写真へ進む
};

[[nodiscard]] const char* toString(ImageState state) noexcept;

struct ImageWorkerConfig {
  // ★IP リテラルを使うこと（Host 検証。ホスト名だと 403 になりうる）。
  std::string baseUrl{"http://127.0.0.1:8080"};

  int targetWidth{1280};
  int targetHeight{720};

  // 縮小後の画像を何枚まで保持するか。1 枚 3.5MB（1280x720）。
  //
  // ★★ 実測で判明: 「現在 + 先読み 2 枚 = 3」で見積もると **足りない** ★★
  //   呼び出し側は同時に
  //     現在 (pin) + 直前 (pin) + 先読み 3 枚（次・その次・そのまた次）
  //   を要求するので、実際には **5 枚** が同時に生きている。
  //   容量 4 で回すと、格納するたびに先読み分が追い出され、追い出された
  //   キーは状態が消えるので次のフレームで再要求され、**永久にダウンロードし
  //   続ける**（12 秒で 874 回 GET したのを実測した）。しかも表示は正常に
  //   見えるので、ログを読まないと気づけない。
  //   既定 6 = 約 21MB（5 枚 + 余裕 1 枚）。
  std::size_t cacheCapacity{6};

  // ★ダウンロードの受け入れ上限。圧縮された JPEG のバイト数に対する上限で、
  //   デコード後のバイト数（decodeBudget）とは別物。
  std::size_t maxDownloadBytes{24U * 1024 * 1024};

  // ★デコード後の RGBA バイト数の上限。超えたら Excluded。
  core::DecodeBudget decodeBudget{};

  core::Millis requestTimeout{8000};
};

struct ImageWorkerStats {
  std::size_t requested{0};
  std::size_t completed{0};
  std::size_t failed{0};
  std::size_t excluded{0};
  std::size_t evicted{0};
  std::size_t cacheHits{0};
  std::size_t queueDepth{0};
  std::size_t cachedImages{0};
  std::size_t cachedBytes{0};
  std::size_t cacheCapacity{0};
  // 1 枚のデコードで確保した RGBA の最大バイト数（予算の効きを見るため）。
  std::uint64_t peakDecodedBytes{0};
};

class ImageWorker {
 public:
  // http は所有しない。★ワーカースレッド専用のインスタンスを渡すこと。
  //   コンストラクタでスレッドが起動する。
  ImageWorker(core::IHttpClient& http, ImageWorkerConfig config);
  ~ImageWorker();

  ImageWorker(const ImageWorker&) = delete;
  ImageWorker& operator=(const ImageWorker&) = delete;
  ImageWorker(ImageWorker&&) = delete;
  ImageWorker& operator=(ImageWorker&&) = delete;

  // 取得を予約する。既に Pending / Ready / Excluded なら何もしない。
  // urgent = true でキューの先頭へ入れる（今すぐ出したい 1 枚）。
  void request(const std::string& key, const core::PhotoRelPath& file,
               bool urgent = false);

  // キャッシュにあれば返す（無ければ nullptr）。★ノンブロッキング。
  [[nodiscard]] std::shared_ptr<const RgbaImage> get(const std::string& key);

  [[nodiscard]] ImageState stateOf(const std::string& key) const;

  // LRU から追い出さないキーの集合を置き換える（最適化。安全機構ではない）。
  void setPinned(std::vector<std::string> keys);

  // Failed / Excluded の記録を忘れる。★サーバが復帰したときに呼ぶ。
  // Excluded も忘れるのは「一時」除外だからで、恒久的な排除ではない。
  void clearFailures();

  [[nodiscard]] ImageWorkerStats stats() const;

 private:
  struct Job {
    std::string key;
    std::string url;
  };

  void threadMain();
  // 1 件処理する。戻り値は結果の状態。
  [[nodiscard]] ImageState process(const Job& job, std::shared_ptr<RgbaImage>& out,
                                   std::string& diagnostic);

  void store(const std::string& key, std::shared_ptr<const RgbaImage> image);
  void touch(const std::string& key);  // 呼び出し側は lock を保持していること

  core::IHttpClient* http_;
  ImageWorkerConfig config_;

  mutable std::mutex mutex_;
  std::condition_variable cv_;
  std::deque<Job> queue_;
  std::map<std::string, ImageState, std::less<>> states_;
  std::map<std::string, std::shared_ptr<const RgbaImage>, std::less<>> cache_;
  std::vector<std::string> lru_;  // 先頭が最近使ったもの
  std::vector<std::string> pinned_;
  ImageWorkerStats stats_;

  // ★mutex_ の保護下にある（std::atomic にしないこと）。
  //   理由は image_worker.cpp のデストラクタのコメント（通知の取りこぼし）。
  //   ManifestPoller / EnvPoller も同じ形で書かれている。
  bool stop_ = false;
  std::thread thread_;
};

}  // namespace ambient::render
