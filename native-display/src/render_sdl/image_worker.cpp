#include "ambient/render/image_worker.hpp"

#include <SDL.h>
#include <SDL_image.h>

#include <algorithm>
#include <cstdio>
#include <utility>

#include "ambient/render/image.hpp"  // cover_source_rect（読み取り専用で使う）

namespace ambient::render {
namespace {

// SDL_Surface の RAII。デコードは複数の早期 return を持つので、
// 解放漏れを型で潰しておく。
struct SurfaceDeleter {
  void operator()(SDL_Surface* surface) const noexcept {
    if (surface != nullptr) {
      SDL_FreeSurface(surface);
    }
  }
};
using SurfacePtr = std::unique_ptr<SDL_Surface, SurfaceDeleter>;

struct RwDeleter {
  void operator()(SDL_RWops* ops) const noexcept {
    if (ops != nullptr) {
      SDL_RWclose(ops);
    }
  }
};
using RwPtr = std::unique_ptr<SDL_RWops, RwDeleter>;

}  // namespace

const char* toString(ImageState state) noexcept {
  switch (state) {
    case ImageState::Unknown:
      return "Unknown";
    case ImageState::Pending:
      return "Pending";
    case ImageState::Ready:
      return "Ready";
    case ImageState::Failed:
      return "Failed";
    case ImageState::Excluded:
      return "Excluded";
  }
  return "?";
}

// ---------------------------------------------------------------------------
// 生成 / 破棄
// ---------------------------------------------------------------------------

ImageWorker::ImageWorker(core::IHttpClient& http, ImageWorkerConfig config)
    : http_(&http), config_(std::move(config)) {
  if (config_.targetWidth <= 0) {
    config_.targetWidth = 1280;
  }
  if (config_.targetHeight <= 0) {
    config_.targetHeight = 720;
  }
  if (config_.cacheCapacity == 0) {
    config_.cacheCapacity = 1;
  }
  thread_ = std::thread(&ImageWorker::threadMain, this);
}

ImageWorker::~ImageWorker() {
  {
    // ★★ stop_ は必ず mutex_ を保持したまま変更すること ★★
    //
    //   条件変数の要件そのものである（述語が参照する状態はミューテックス保持下で
    //   変更する）。破ると **通知が消える**:
    //
    //     1. ワーカーが mutex_ を保持して述語 `stop_ || !queue_.empty()` を
    //        評価し false を得る。まだ mutex_ を持っており、cv_.wait() 内部の
    //        「アンロックしてブロック」にはまだ到達していない。
    //     2. この瞬間にこちらが stop_ = true と notify_all() を実行する
    //        （mutex_ を取らないので割り込める）。**誰も待っていないので通知は消える。**
    //     3. 直後にワーカーが mutex_ を解放してブロックする。二度と起きない。
    //        → 下の join() が永久にハングし、アプリが終了しなくなる。
    //
    //   ★stop_ を std::atomic に戻さないこと。アトミックにしても上の競合は
    //     一切防げず、「アトミックだからロック不要」という誤解を招くだけである。
    //     ManifestPoller / EnvPoller（app/main_sdl.cpp）も同じ形（素の bool +
    //     lock_guard）で書かれている。3 つを揃えてある。
    const std::lock_guard<std::mutex> lock(mutex_);
    stop_ = true;
  }
  cv_.notify_all();
  if (thread_.joinable()) {
    thread_.join();
  }
}

// ---------------------------------------------------------------------------
// 呼び出し側 API（メインスレッドから呼ばれる）
// ---------------------------------------------------------------------------

void ImageWorker::request(const std::string& key, const core::PhotoRelPath& file,
                          bool urgent) {
  if (key.empty() || file.value.empty()) {
    return;
  }
  // ★PhotoRelPath 専用の joinUrl を使うこと（AdminAbsPath 用と非対称）。
  Job job{key, core::joinUrl(config_.baseUrl, file)};

  {
    const std::lock_guard<std::mutex> lock(mutex_);
    const auto found = states_.find(key);
    if (found != states_.end() && found->second != ImageState::Unknown) {
      // Pending / Ready / Failed / Excluded はいずれも再投入しない。
      // Failed / Excluded を再試行したいときは clearFailures() を呼ぶこと。
      return;
    }
    states_[key] = ImageState::Pending;
    if (urgent) {
      queue_.push_front(std::move(job));
    } else {
      queue_.push_back(std::move(job));
    }
    ++stats_.requested;
  }
  cv_.notify_one();
}

std::shared_ptr<const RgbaImage> ImageWorker::get(const std::string& key) {
  const std::lock_guard<std::mutex> lock(mutex_);
  const auto found = cache_.find(key);
  if (found == cache_.end()) {
    return nullptr;
  }
  ++stats_.cacheHits;
  touch(key);
  return found->second;
}

ImageState ImageWorker::stateOf(const std::string& key) const {
  const std::lock_guard<std::mutex> lock(mutex_);
  const auto found = states_.find(key);
  return (found == states_.end()) ? ImageState::Unknown : found->second;
}

void ImageWorker::setPinned(std::vector<std::string> keys) {
  const std::lock_guard<std::mutex> lock(mutex_);
  pinned_ = std::move(keys);
}

void ImageWorker::clearFailures() {
  const std::lock_guard<std::mutex> lock(mutex_);
  for (auto it = states_.begin(); it != states_.end();) {
    if (it->second == ImageState::Failed || it->second == ImageState::Excluded) {
      it = states_.erase(it);
    } else {
      ++it;
    }
  }
}

ImageWorkerStats ImageWorker::stats() const {
  const std::lock_guard<std::mutex> lock(mutex_);
  ImageWorkerStats snapshot = stats_;
  snapshot.queueDepth = queue_.size();
  snapshot.cachedImages = cache_.size();
  snapshot.cacheCapacity = config_.cacheCapacity;
  std::size_t bytes = 0;
  for (const auto& entry : cache_) {
    if (entry.second != nullptr) {
      bytes += entry.second->byteSize();
    }
  }
  snapshot.cachedBytes = bytes;
  return snapshot;
}

// ---------------------------------------------------------------------------
// LRU（呼び出し側が mutex_ を保持していること）
// ---------------------------------------------------------------------------

void ImageWorker::touch(const std::string& key) {
  const auto it = std::find(lru_.begin(), lru_.end(), key);
  if (it != lru_.end()) {
    lru_.erase(it);
  }
  lru_.insert(lru_.begin(), key);
}

void ImageWorker::store(const std::string& key, std::shared_ptr<const RgbaImage> image) {
  const std::lock_guard<std::mutex> lock(mutex_);
  cache_[key] = std::move(image);
  states_[key] = ImageState::Ready;
  touch(key);

  // 容量を超えたら **後ろから** 追い出す。pinned は飛ばす。
  // ★追い出しはキャッシュの shared_ptr を落とすだけなので、呼び出し側が
  //   握っている画像は生き続ける（use-after-free にならない）。
  while (cache_.size() > config_.cacheCapacity) {
    bool evicted = false;
    for (auto it = lru_.rbegin(); it != lru_.rend(); ++it) {
      const std::string& candidate = *it;
      if (std::find(pinned_.begin(), pinned_.end(), candidate) != pinned_.end()) {
        continue;
      }
      if (cache_.erase(candidate) > 0) {
        states_.erase(candidate);  // 消したので再取得できるようにする
        ++stats_.evicted;
      }
      lru_.erase(std::next(it).base());
      evicted = true;
      break;
    }
    if (!evicted) {
      // 全部 pinned。これ以上は追い出せない（pinned の数は呼び出し側が
      // cacheCapacity 未満に保つこと）。
      break;
    }
  }
}

// ---------------------------------------------------------------------------
// ワーカースレッド
// ---------------------------------------------------------------------------

void ImageWorker::threadMain() {
  for (;;) {
    Job job;
    {
      // ★stop_ / queue_ はどちらも mutex_ の保護下にある（デストラクタの節）。
      std::unique_lock<std::mutex> lock(mutex_);
      cv_.wait(lock, [this] { return stop_ || !queue_.empty(); });
      if (stop_) {
        return;
      }
      job = std::move(queue_.front());
      queue_.pop_front();
    }

    std::shared_ptr<RgbaImage> image;
    std::string diagnostic;
    const ImageState state = process(job, image, diagnostic);

    if (state == ImageState::Ready && image != nullptr) {
      store(job.key, std::move(image));
      continue;
    }

    {
      const std::lock_guard<std::mutex> lock(mutex_);
      states_[job.key] = state;
      if (state == ImageState::Excluded) {
        ++stats_.excluded;
      } else {
        ++stats_.failed;
      }
    }
    std::fprintf(stderr, "[image-worker] %s id=%s: %s\n", toString(state), job.key.c_str(),
                 diagnostic.c_str());
  }
}

ImageState ImageWorker::process(const Job& job, std::shared_ptr<RgbaImage>& out,
                                std::string& diagnostic) {
  // --- 1. ダウンロード -------------------------------------------------------
  core::HttpRequest request;
  request.method = core::HttpMethod::Get;
  request.url = job.url;
  request.timeout = config_.requestTimeout;
  request.maxResponseBytes = config_.maxDownloadBytes;

  const core::HttpResponse response = http_->fetch(request);

  // ★ステータスで分岐してから中身を見る。403/404 は HTML が返る。
  const std::optional<std::string_view> body = core::successBody(response);
  if (!body) {
    if (response.error != core::HttpError::None) {
      diagnostic = std::string("通信に失敗 (") + core::toString(response.error) + ") " +
                   job.url;
      // 上限超は「この画像は重すぎる」なので一時除外扱いにする。
      return (response.error == core::HttpError::TooLarge) ? ImageState::Excluded
                                                           : ImageState::Failed;
    }
    diagnostic = "HTTP " + std::to_string(response.status) + " " + job.url;
    return ImageState::Failed;
  }

  const auto* bytes = reinterpret_cast<const std::uint8_t*>(body->data());
  const std::span<const std::uint8_t> raw(bytes, body->size());

  // --- 2. ★probe（寸法だけ読む。デコードしない）------------------------------
  const std::optional<core::ImageSize> probed = core::probeImageSize(raw);
  if (!probed) {
    diagnostic = "寸法を読めませんでした（JPEG/PNG ではないか、途中で切れています。" +
                 std::to_string(body->size()) + " バイト, Content-Type=" +
                 response.contentType + "）";
    return ImageState::Failed;
  }

  // --- 3. ★予算判定（デコードの前）-------------------------------------------
  if (!core::fitsDecodeBudget(*probed, config_.decodeBudget)) {
    diagnostic = "予算超過のため一時除外: " + std::to_string(probed->width) + "x" +
                 std::to_string(probed->height) + " = " +
                 std::to_string(core::decodedBytesRGBA(*probed) / (1024 * 1024)) +
                 "MB (上限 " +
                 std::to_string(config_.decodeBudget.maxDecodedBytes / (1024 * 1024)) +
                 "MB)";
    return ImageState::Excluded;
  }

  {
    const std::lock_guard<std::mutex> lock(mutex_);
    stats_.peakDecodedBytes =
        std::max(stats_.peakDecodedBytes, core::decodedBytesRGBA(*probed));
  }

  // --- 4. デコード -----------------------------------------------------------
  RwPtr rw(SDL_RWFromConstMem(body->data(), static_cast<int>(body->size())));
  if (rw == nullptr) {
    diagnostic = std::string("SDL_RWFromConstMem に失敗: ") + SDL_GetError();
    return ImageState::Failed;
  }
  SurfacePtr decoded(IMG_Load_RW(rw.get(), /*freesrc=*/0));
  if (decoded == nullptr) {
    diagnostic = std::string("デコードに失敗: ") + IMG_GetError();
    return ImageState::Failed;
  }

  // ★probe の値とデコード結果が食い違ったら、以降の予算前提が崩れている。
  //   デコードは済んでしまっているので落としはしないが、実寸で判定し直す。
  const core::ImageSize actual{static_cast<std::uint32_t>(decoded->w),
                               static_cast<std::uint32_t>(decoded->h)};
  if (!core::fitsDecodeBudget(actual, config_.decodeBudget)) {
    diagnostic = "デコード後の実寸が予算超過（probe と食い違い）: " +
                 std::to_string(actual.width) + "x" + std::to_string(actual.height);
    return ImageState::Excluded;
  }

  SurfacePtr rgba(SDL_ConvertSurfaceFormat(decoded.get(), SDL_PIXELFORMAT_RGBA32, 0));
  decoded.reset();  // ★元のサーフェスはここで手放す（同時に 2 枚持たない）
  if (rgba == nullptr) {
    diagnostic = std::string("RGBA への変換に失敗: ") + SDL_GetError();
    return ImageState::Failed;
  }

  // --- 5. cover クロップ ------------------------------------------------------
  const SDL_Rect crop =
      cover_source_rect(rgba->w, rgba->h, config_.targetWidth, config_.targetHeight);
  if (crop.w <= 0 || crop.h <= 0) {
    diagnostic = "cover クロップの矩形が空になりました";
    return ImageState::Failed;
  }

  auto image = std::make_shared<RgbaImage>();
  image->width = config_.targetWidth;
  image->height = config_.targetHeight;
  image->sourceWidth = actual.width;
  image->sourceHeight = actual.height;
  image->pixels.assign(static_cast<std::size_t>(config_.targetWidth) *
                           static_cast<std::size_t>(config_.targetHeight) * 4U,
                       0);

  const std::size_t dstStride = static_cast<std::size_t>(config_.targetWidth) * 4U;

  if (crop.w >= config_.targetWidth && crop.h >= config_.targetHeight) {
    // --- 6. ★box filter 縮小（本命の経路）-----------------------------------
    //   クロップ矩形の左上を指すポインタ + 元画像の stride を渡すので、
    //   クロップのための中間コピーが要らない。
    if (SDL_MUSTLOCK(rgba.get()) != 0 && SDL_LockSurface(rgba.get()) != 0) {
      diagnostic = std::string("SDL_LockSurface に失敗: ") + SDL_GetError();
      return ImageState::Failed;
    }
    const auto stride = static_cast<std::size_t>(rgba->pitch);
    const auto* base = static_cast<const std::uint8_t*>(rgba->pixels);
    const std::uint8_t* origin =
        base + static_cast<std::size_t>(crop.y) * stride + static_cast<std::size_t>(crop.x) * 4U;

    const bool ok = core::downsampleBoxRGBA(
        origin, core::ImageSize{static_cast<std::uint32_t>(crop.w),
                                static_cast<std::uint32_t>(crop.h)},
        stride, image->pixels.data(),
        core::ImageSize{static_cast<std::uint32_t>(config_.targetWidth),
                        static_cast<std::uint32_t>(config_.targetHeight)},
        dstStride);

    if (SDL_MUSTLOCK(rgba.get()) != 0) {
      SDL_UnlockSurface(rgba.get());
    }
    if (!ok) {
      diagnostic = "box filter 縮小に失敗しました";
      return ImageState::Failed;
    }
  } else {
    // --- 6'. 拡大が要る場合（元画像が 1280x720 より小さい）--------------------
    //   box filter は縮小の定義しか持たないので、ここだけ SDL の拡大に頼る。
    //   derivative は 2560 幅なので通常この経路は通らない。
    SurfacePtr target(SDL_CreateRGBSurfaceWithFormat(
        0, config_.targetWidth, config_.targetHeight, 32, SDL_PIXELFORMAT_RGBA32));
    if (target == nullptr) {
      diagnostic = std::string("拡大用サーフェスを作れませんでした: ") + SDL_GetError();
      return ImageState::Failed;
    }
    SDL_Rect src = crop;
    SDL_Rect dst{0, 0, config_.targetWidth, config_.targetHeight};
    SDL_SetSurfaceBlendMode(rgba.get(), SDL_BLENDMODE_NONE);
    if (SDL_BlitScaled(rgba.get(), &src, target.get(), &dst) != 0) {
      diagnostic = std::string("SDL_BlitScaled に失敗: ") + SDL_GetError();
      return ImageState::Failed;
    }
    const auto* srcPixels = static_cast<const std::uint8_t*>(target->pixels);
    const auto srcStride = static_cast<std::size_t>(target->pitch);
    for (int y = 0; y < config_.targetHeight; ++y) {
      const std::uint8_t* row = srcPixels + static_cast<std::size_t>(y) * srcStride;
      std::copy(row, row + dstStride, image->pixels.data() + static_cast<std::size_t>(y) * dstStride);
    }
  }

  {
    const std::lock_guard<std::mutex> lock(mutex_);
    ++stats_.completed;
  }
  out = std::move(image);
  return ImageState::Ready;
}

}  // namespace ambient::render
