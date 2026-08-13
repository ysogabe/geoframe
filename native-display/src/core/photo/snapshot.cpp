// snapshot.cpp — 写真スケジューラの永続化（レーン 1B）
//
// ★形式・上限・「上限を超えたときに何が壊れるか」の判断は
//   ambient/core/photo/snapshot.hpp の冒頭「1B の決定」に書いてある。
//   ここはその実装であり、判断の記録はヘッダが正典。
//
// 例外は投げない。壊れた入力は nullopt で返す（Core の規律）。
// std::stoul / std::stoi は使わない（例外を投げるため）。

#include "ambient/core/photo/snapshot.hpp"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace ambient::core {

namespace {

// ============================================================================
// 直列化形式（version 1）
// ============================================================================
//
//   snapshot  := "gfp" <version> "\n"
//                <roundNo> "\n"
//                <s0> " " <s1> " " <s2> " " <s3> " " <draws> " " <steps> "\n"
//                blob(<signature>) "\n"
//                <queueCount> "\n"
//                { blob(<photoId>) "\n" }        x queueCount
//                <shownCount> "\n"
//                { blob(<photoId>) ":" <round> "\n" }  x shownCount
//
//   blob      := <byteLength> ":" <bytes>
//
// ★blob が長さ前置きなのは、photoId が **認証の無い LAN 上のサーバ**から来る
//   文字列だからである。改行やコロンが混ざっても曖昧にならず、
//   エスケープ処理（実装ミスの温床）が要らない。
//
// ★数値は 10 進 ASCII。バイナリにしないのはエンディアン依存を持ち込まないため。

constexpr std::string_view kMagic = "gfp";

// 1 エントリの最小バイト数（"0:\n" = 3）。壊れた count で巨大な確保をしないための下限。
constexpr std::size_t kMinBytesPerEntry = 3;

void appendUint(std::string& out, std::uint64_t value) {
  char buf[20];
  std::size_t len = 0;
  if (value == 0) {
    buf[len++] = '0';
  }
  while (value != 0) {
    buf[len++] = static_cast<char>('0' + (value % 10U));
    value /= 10U;
  }
  while (len > 0) {
    out += buf[--len];
  }
}

void appendBlob(std::string& out, std::string_view bytes) {
  appendUint(out, bytes.size());
  out += ':';
  out += bytes;
}

// ----------------------------------------------------------------------------
// 読み取り側（カーソル式。例外は投げない）
// ----------------------------------------------------------------------------

class Cursor {
 public:
  explicit Cursor(std::string_view text) noexcept : text_(text) {}

  [[nodiscard]] std::size_t remaining() const noexcept { return text_.size() - pos_; }
  [[nodiscard]] bool atEnd() const noexcept { return pos_ >= text_.size(); }

  // 10 進整数を読み、続く delimiter を 1 文字消費する。
  [[nodiscard]] bool readUint(char delimiter, std::uint64_t& out) noexcept {
    std::uint64_t value = 0;
    std::size_t digits = 0;
    constexpr std::uint64_t kMax = std::numeric_limits<std::uint64_t>::max();
    while (pos_ < text_.size()) {
      const char c = text_[pos_];
      if (c < '0' || c > '9') {
        break;
      }
      const auto d = static_cast<std::uint64_t>(c - '0');
      if (value > (kMax - d) / 10U) {
        return false;  // 桁あふれ。静かに飽和させない。
      }
      value = value * 10U + d;
      ++digits;
      ++pos_;
    }
    if (digits == 0 || pos_ >= text_.size() || text_[pos_] != delimiter) {
      return false;
    }
    ++pos_;
    out = value;
    return true;
  }

  // "<len>:<bytes>" を読み、続く delimiter を 1 文字消費する。
  [[nodiscard]] bool readBlob(char delimiter, std::string& out) {
    std::uint64_t length = 0;
    if (!readUint(':', length)) {
      return false;
    }
    // ★残りバイト数を超える長さは壊れた入力。ここで弾かないと
    //   細工した短い入力で巨大な確保を誘発できる。
    if (length > remaining()) {
      return false;
    }
    const auto len = static_cast<std::size_t>(length);
    out.assign(text_.substr(pos_, len));
    pos_ += len;
    if (pos_ >= text_.size() || text_[pos_] != delimiter) {
      return false;
    }
    ++pos_;
    return true;
  }

  [[nodiscard]] bool consume(std::string_view literal) noexcept {
    if (remaining() < literal.size() || text_.substr(pos_, literal.size()) != literal) {
      return false;
    }
    pos_ += literal.size();
    return true;
  }

 private:
  std::string_view text_;
  std::size_t pos_{0};
};

[[nodiscard]] bool fitsUint32(std::uint64_t value) noexcept {
  return value <= std::numeric_limits<std::uint32_t>::max();
}

// round が古い順（同点は photoId 昇順）に dropCount 件捨てる。
// ★同点の順序を決めておくこと。決めないと Mac と Tab5 で捨てる写真が食い違う。
void dropOldestShown(std::vector<ShownEntry>& shown, std::size_t dropCount) {
  if (dropCount == 0) {
    return;
  }
  if (dropCount >= shown.size()) {
    shown.clear();
    return;
  }

  std::vector<std::uint32_t> order(shown.size());
  for (std::size_t i = 0; i < order.size(); ++i) {
    order[i] = static_cast<std::uint32_t>(i);
  }
  const std::vector<ShownEntry>& entries = shown;
  std::sort(order.begin(), order.end(), [&entries](std::uint32_t a, std::uint32_t b) noexcept {
    if (entries[a].round != entries[b].round) {
      return entries[a].round < entries[b].round;
    }
    return entries[a].photoId < entries[b].photoId;
  });

  std::vector<char> doomed(shown.size(), 0);
  for (std::size_t i = 0; i < dropCount; ++i) {
    doomed[order[i]] = 1;
  }

  // photoId 昇順という不変条件を保ったまま詰める。
  std::size_t write = 0;
  for (std::size_t read = 0; read < shown.size(); ++read) {
    if (doomed[read] == 0) {
      // ★自己 move 代入を避ける。std::string の自己 move は「有効だが未規定の値」
      //   になり、実際に中身が空になる（このガードが無いと先頭要素が消える）。
      if (write != read) {
        shown[write] = std::move(shown[read]);
      }
      ++write;
    }
  }
  shown.resize(write);
}

}  // namespace

SchedulerSnapshot truncateForStore(SchedulerSnapshot snapshot, const SnapshotLimits& limits) {
  // 1. 件数の上限。
  if (limits.maxShownEntries != 0 && snapshot.shown.size() > limits.maxShownEntries) {
    dropOldestShown(snapshot.shown, snapshot.shown.size() - limits.maxShownEntries);
  }
  if (limits.maxQueueEntries != 0 && snapshot.playQueue.size() > limits.maxQueueEntries) {
    // ★末尾から捨てる。先頭は「次に出る写真」なので捨てると再生位置が飛ぶ。
    snapshot.playQueue.resize(limits.maxQueueEntries);
  }

  // 2. バイト数の上限。encodeSnapshot の結果でしか測れないので反復する。
  //    ★反復回数に上限を設けること（Tab5 で無限ループさせない）。
  if (limits.maxTotalBytes == 0) {
    return snapshot;
  }
  constexpr int kMaxIterations = 64;
  for (int iteration = 0; iteration < kMaxIterations; ++iteration) {
    const std::size_t size = encodeSnapshot(snapshot).size();
    if (size <= limits.maxTotalBytes) {
      break;
    }
    if (!snapshot.shown.empty()) {
      // 1回に1件ずつ削ると数千回符号化する羽目になる。1割ずつ削る。
      const std::size_t step = std::max<std::size_t>(1, snapshot.shown.size() / 10);
      dropOldestShown(snapshot.shown, step);
      continue;
    }
    if (!snapshot.playQueue.empty()) {
      const std::size_t step = std::max<std::size_t>(1, snapshot.playQueue.size() / 10);
      snapshot.playQueue.resize(snapshot.playQueue.size() - step);
      continue;
    }
    // shown も queue も空でまだ超える = シグネチャ等の固定部だけで超過している。
    // これ以上は捨てるものが無い。呼び出し側が write() の戻り値で気づく。
    break;
  }
  return snapshot;
}

std::string encodeSnapshot(const SchedulerSnapshot& snapshot) {
  std::string out;
  // 見積もり: 固定部 + queue 1 件 8B + shown 1 件 10B。
  out.reserve(96 + snapshot.playQueue.size() * 8 + snapshot.shown.size() * 10);

  out += kMagic;
  appendUint(out, snapshot.version);
  out += '\n';

  appendUint(out, snapshot.roundNo);
  out += '\n';

  appendUint(out, snapshot.schedulerRng.s0);
  out += ' ';
  appendUint(out, snapshot.schedulerRng.s1);
  out += ' ';
  appendUint(out, snapshot.schedulerRng.s2);
  out += ' ';
  appendUint(out, snapshot.schedulerRng.s3);
  out += ' ';
  appendUint(out, snapshot.schedulerRng.draws);
  out += ' ';
  appendUint(out, snapshot.schedulerRng.steps);
  out += '\n';

  appendBlob(out, snapshot.photosSignature);
  out += '\n';

  appendUint(out, snapshot.playQueue.size());
  out += '\n';
  for (const PhotoId& id : snapshot.playQueue) {
    appendBlob(out, id);
    out += '\n';
  }

  appendUint(out, snapshot.shown.size());
  out += '\n';
  for (const ShownEntry& entry : snapshot.shown) {
    appendBlob(out, entry.photoId);
    out += ':';
    appendUint(out, entry.round);
    out += '\n';
  }

  // ★knownIdHashes / knownIdsPrimed は書かない（実行時のみの状態）。
  //   復元後の初回 syncSchedule は「現 manifest を既知集合として記録するだけ」に
  //   なり、出典 scheduler.js の knownIds === null と同じ挙動になる。
  return out;
}

std::optional<SchedulerSnapshot> decodeSnapshot(std::string_view encoded) {
  Cursor cursor{encoded};

  if (!cursor.consume(kMagic)) {
    return std::nullopt;
  }
  std::uint64_t version = 0;
  if (!cursor.readUint('\n', version)) {
    return std::nullopt;
  }
  // ★未知バージョンは黙って読まない。読むと壊れたデータを正常として使ってしまう。
  if (version != kSnapshotVersion) {
    return std::nullopt;
  }

  SchedulerSnapshot out{};
  out.version = static_cast<std::uint32_t>(version);

  std::uint64_t roundNo = 0;
  if (!cursor.readUint('\n', roundNo) || !fitsUint32(roundNo)) {
    return std::nullopt;
  }
  out.roundNo = static_cast<std::uint32_t>(roundNo);

  std::uint64_t rng[4] = {0, 0, 0, 0};
  if (!cursor.readUint(' ', rng[0]) || !cursor.readUint(' ', rng[1]) ||
      !cursor.readUint(' ', rng[2]) || !cursor.readUint(' ', rng[3])) {
    return std::nullopt;
  }
  for (const std::uint64_t v : rng) {
    if (!fitsUint32(v)) {
      return std::nullopt;
    }
  }
  std::uint64_t draws = 0;
  std::uint64_t steps = 0;
  if (!cursor.readUint(' ', draws) || !cursor.readUint('\n', steps)) {
    return std::nullopt;
  }
  out.schedulerRng.s0 = static_cast<std::uint32_t>(rng[0]);
  out.schedulerRng.s1 = static_cast<std::uint32_t>(rng[1]);
  out.schedulerRng.s2 = static_cast<std::uint32_t>(rng[2]);
  out.schedulerRng.s3 = static_cast<std::uint32_t>(rng[3]);
  out.schedulerRng.draws = draws;
  out.schedulerRng.steps = steps;

  if (!cursor.readBlob('\n', out.photosSignature)) {
    return std::nullopt;
  }

  std::uint64_t queueCount = 0;
  if (!cursor.readUint('\n', queueCount)) {
    return std::nullopt;
  }
  if (queueCount > cursor.remaining() / kMinBytesPerEntry) {
    return std::nullopt;  // 残りバイト数に対して件数が多すぎる = 壊れている。
  }
  out.playQueue.resize(static_cast<std::size_t>(queueCount));
  for (PhotoId& id : out.playQueue) {
    if (!cursor.readBlob('\n', id)) {
      return std::nullopt;
    }
  }

  std::uint64_t shownCount = 0;
  if (!cursor.readUint('\n', shownCount)) {
    return std::nullopt;
  }
  if (shownCount > cursor.remaining() / kMinBytesPerEntry) {
    return std::nullopt;
  }
  out.shown.resize(static_cast<std::size_t>(shownCount));
  for (ShownEntry& entry : out.shown) {
    std::uint64_t round = 0;
    if (!cursor.readBlob(':', entry.photoId) || !cursor.readUint('\n', round) ||
        !fitsUint32(round)) {
      return std::nullopt;
    }
    entry.round = static_cast<std::uint32_t>(round);
  }

  if (!cursor.atEnd()) {
    return std::nullopt;  // 余りがある = 想定外の形式。黙って無視しない。
  }

  // ★shown の不変条件（photoId 昇順）をここで必ず立て直す。
  //   保存側が守っていても、ファイルは外から書き換えられうる。
  std::sort(out.shown.begin(), out.shown.end(),
            [](const ShownEntry& a, const ShownEntry& b) noexcept {
              return a.photoId < b.photoId;
            });
  // 同一 ID の重複は後勝ちで潰す（重複があると upsert の探索が壊れる）。
  const auto duplicate = std::unique(
      out.shown.begin(), out.shown.end(),
      [](const ShownEntry& a, const ShownEntry& b) noexcept { return a.photoId == b.photoId; });
  out.shown.erase(duplicate, out.shown.end());

  return out;
}

}  // namespace ambient::core
