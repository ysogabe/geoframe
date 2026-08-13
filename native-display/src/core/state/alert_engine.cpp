// alert_engine.cpp — 地震アラート（エッジ→レベル変換器）
//
// 移植元: app/static/js/events.js:174-232（読み取り専用。挙動を変えない）
//
// ★移植で特に注意した点:
//   - maxi は文字列。parseIntPrefix() を通す（"5-" を std::stoi に渡すと
//     例外を投げずに 5 を返し、弱強の区別が **静かに** 消える）。
//   - クールダウンは **グローバル 10 分**。eid ごとではない（群発で連鎖しない）。
//   - seen_ の上限は kSeenQuakeMax (= 500)。挿入順 = 古い順に捨てる。
//   - firstLoad では claim を立てず seen 登録だけ。
//   - ★rememberEq は「発火するかどうかを判定する **前**」に呼ばれる。
//     震度 3 未満の地震も、クールダウンで抑止された地震も、既知になって
//     二度と発火しない。これは上流の挙動であり、意図的に保存している。
//     （後から「発火したものだけ既知にする」へ変えると、クールダウン明けに
//       10 分前の古い地震が掘り起こされて鳴る）
//   - collect() は const。ここで状態を書き換えない（claim.hpp の契約）。

#include "ambient/core/state/alert_engine.hpp"

#include <algorithm>
#include <limits>
#include <optional>
#include <string>
#include <utility>

#include "ambient/core/model/parse.hpp"

namespace ambient::core {
namespace {

// 直列化フォーマットのバージョン行。形式を変えたら上げること
// （読めない版は捨てて起動を止めない）。
constexpr std::string_view kFormatVersion = "1";

// クールダウン基準が「一度も発火していない」ことを表す番兵。
constexpr std::string_view kNeverFired = "-";

// std::int64_t を 10 進文字列へ。★<format> は使わない（コードサイズ）。
[[nodiscard]] std::string toDecimal(std::int64_t value) {
  if (value == 0) {
    return "0";
  }
  const bool negative = value < 0;
  // ★負数を -value するのは INT64_MIN で UB。unsigned で絶対値を作る。
  auto magnitude = static_cast<std::uint64_t>(value);
  if (negative) {
    magnitude = ~magnitude + 1U;
  }
  std::string digits;
  while (magnitude != 0U) {
    digits.push_back(static_cast<char>('0' + static_cast<char>(magnitude % 10U)));
    magnitude /= 10U;
  }
  if (negative) {
    digits.push_back('-');
  }
  std::reverse(digits.begin(), digits.end());
  return digits;
}

// 10 進文字列 → std::int64_t。
// ★parseIntPrefix() は int 幅なので epoch 秒には使えない（2038 年で溢れる）。
[[nodiscard]] std::optional<std::int64_t> fromDecimal(std::string_view text) noexcept {
  if (text.empty()) {
    return std::nullopt;
  }
  std::size_t i = 0;
  const bool negative = text[0] == '-';
  if (negative) {
    i = 1;
  }
  if (i >= text.size()) {
    return std::nullopt;
  }
  std::uint64_t acc = 0;
  for (; i < text.size(); ++i) {
    if (text[i] < '0' || text[i] > '9') {
      return std::nullopt;
    }
    if (acc > (std::numeric_limits<std::uint64_t>::max() - 9U) / 10U) {
      return std::nullopt;  // 静かに飽和させない
    }
    acc = acc * 10U + static_cast<std::uint64_t>(text[i] - '0');
  }
  constexpr auto kMax = static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max());
  if (negative) {
    if (acc > kMax + 1U) {
      return std::nullopt;
    }
    return static_cast<std::int64_t>(~acc + 1U);
  }
  if (acc > kMax) {
    return std::nullopt;
  }
  return static_cast<std::int64_t>(acc);
}

// '\n' 区切りの次の行を切り出す。cursor は次行の先頭へ進む。
[[nodiscard]] std::string_view nextLine(std::string_view text, std::size_t& cursor) noexcept {
  if (cursor >= text.size()) {
    return {};
  }
  const std::size_t end = text.find('\n', cursor);
  if (end == std::string_view::npos) {
    const std::string_view line = text.substr(cursor);
    cursor = text.size();
    return line;
  }
  const std::string_view line = text.substr(cursor, end - cursor);
  cursor = end + 1;
  return line;
}

}  // namespace

// ---- 入力（エッジ） --------------------------------------------------------

void AlertEngine::ingest(std::span<const QuakeItem> quakes, SteadyTime now, bool firstLoad,
                         bool otherEventActive) {
  for (const QuakeItem& quake : quakes) {
    // eid が無い item は同定できない。既知にもできないので黙って捨てる
    // （既知にできないまま発火させると、毎ポーリングで鳴り続ける）。
    if (quake.eid.empty()) {
      continue;
    }

    // 1. 初回ロード分はすべて既知として扱う（過去 24 時間ぶんの一斉発火を防ぐ）。
    if (firstLoad) {
      remember(quake.eid);
      continue;
    }

    // 2. 既知なら何もしない。
    if (hasSeen(quake.eid)) {
      continue;
    }

    // ★ここで既知にする。以降の判定で弾かれても既知のまま（events.js と同じ）。
    remember(quake.eid);

    // 3. 震度 3 以上のみイベント化。
    //    ★maxi は "5-" "5+" がありうる文字列。parseIntPrefix で整数部を取る。
    const std::optional<int> intensity = parseIntPrefix(quake.maxi);
    if (!intensity.has_value() || *intensity < constants::kQuakeMinIntensity) {
      continue;
    }

    // 4. 他イベント表示中は発火しない（events.js の Date.now() >= eventUntil）。
    if (otherEventActive) {
      continue;
    }

    // 5. 群発対策のグローバルクールダウン（10 分）。eid ごとではない。
    if (!cooldownElapsed(now)) {
      continue;
    }

    active_ = quake;
    alertStartedAt_ = now;
    alertActive_ = true;
    activePayloadId_ = nextPayloadId();

    lastFiredAt_ = now;
    everFired_ = true;

    // ★1 回の ingest で複数発火させない。クールダウンを更新済みなので、
    //   同じ応答に含まれる後続の地震は 5 で自動的に弾かれる（既知にはなる）。
  }
}

void AlertEngine::remember(const std::string& eid) {
  seen_.push_back(eid);
  while (seen_.size() > constants::kSeenQuakeMax) {
    seen_.pop_front();  // 挿入順 = 古い順に捨てる
  }
}

bool AlertEngine::hasSeen(std::string_view eid) const noexcept {
  return std::find(seen_.begin(), seen_.end(), eid) != seen_.end();
}

bool AlertEngine::cooldownElapsed(SteadyTime now) const noexcept {
  if (!everFired_) {
    return true;  // まだ一度も発火していない。1 発目は抑止しない。
  }
  // ★グローバル。eid ごとのクールダウンに読み替えないこと（群発で連鎖する）。
  return (now - lastFiredAt_) >= constants::kQuakeGlobalCooldown;
}

std::uint32_t AlertEngine::nextPayloadId() noexcept {
  ++payloadCounter_;
  if (payloadCounter_ == 0) {
    payloadCounter_ = 1;  // 0 は「区別なし」の意味なので飛ばす
  }
  return payloadCounter_;
}

bool AlertEngine::alerting(SteadyTime now) const noexcept {
  if (!alertActive_) {
    return false;
  }
  return (now - alertStartedAt_) < kAlertHoldDuration;
}

// ---- 出力（レベル） --------------------------------------------------------

void AlertEngine::collect(SteadyTime now, std::vector<Claim>& out) const {
  if (!alerting(now)) {
    return;
  }

  Claim claim{};
  claim.priority = ClaimPriority::Alert;
  claim.kind = DisplayStateKind::Alert;
  claim.sourceId = kSourceId;
  claim.payloadId = activePayloadId_;
  claim.expiresAt = alertStartedAt_ + kAlertHoldDuration;
  out.push_back(claim);
}

// ---- 時間跳躍 --------------------------------------------------------------

void AlertEngine::onTimeJump(const TimeJump& jump) noexcept {
  // ポリシーは clock.hpp の rebaseForJump() に集約してある。
  // skew < 0 では catchUp() が 0 を返すので、ここは自動的に no-op になる。
  lastFiredAt_ = rebaseForJump(lastFiredAt_, jump);
  alertStartedAt_ = rebaseForJump(alertStartedAt_, jump);
}

// ---- 永続化 ----------------------------------------------------------------

std::string AlertEngine::encode(std::size_t count, SteadyTime now, WallTime wallNow) const {
  std::string out;
  out.append(kFormatVersion);
  out.push_back('\n');

  // クールダウンの基準を **壁時計** で書く。
  // ★SteadyTime は再起動をまたぐと意味を失う（起点が任意）ので保存できない。
  //   「今から見て何ミリ秒前に発火したか」を壁時計へ移して記録する。
  if (everFired_) {
    const Millis sinceFired = now - lastFiredAt_;
    out.append(toDecimal(wallNow.epochSeconds - (sinceFired.value / 1000)));
  } else {
    out.append(kNeverFired);
  }
  out.push_back('\n');

  const std::size_t keep = std::min(count, seen_.size());
  const std::size_t begin = seen_.size() - keep;  // 直近 keep 件（新しい方を残す）
  for (std::size_t i = begin; i < seen_.size(); ++i) {
    const std::string& eid = seen_[i];
    // ★区切り文字を含む eid は書かない。eid は認証の無い LAN 上のサーバから
    //   来るので、こちらの直列化を壊せる値が来うる（PROGRESS.md の SSRF 参照）。
    //   捨てても「既知でなくなる = 一度だけ余分に鳴りうる」だけで、
    //   壊れた復元よりはるかに軽い。
    if (eid.find('\n') != std::string::npos) {
      continue;
    }
    out.append(eid);
    out.push_back('\n');
  }
  return out;
}

StoreStatus AlertEngine::save(IStateStore& store, SteadyTime now, WallTime wallNow) const {
  // 領域不足なら件数を半減して再試行する。
  // ★OutOfSpace と ValueTooLarge の両方が「大きすぎる」を意味する。
  //   片方だけ見ると、1 値あたりの上限に当たったときに切り詰めずに諦めてしまう。
  std::size_t count = std::min(kSeenQuakePersistMax, seen_.size());
  for (;;) {
    const StoreStatus status = store.write(kSeenQuakeStoreKey, encode(count, now, wallNow));
    if (status != StoreStatus::OutOfSpace && status != StoreStatus::ValueTooLarge) {
      return status;  // Ok か、切り詰めても直らない種類の失敗
    }
    if (count == 0) {
      return status;  // eid 0 件でも入らない。呼び出し側が知るべき異常。
    }
    count /= 2;
  }
}

StoreStatus AlertEngine::restore(const IStateStore& store, SteadyTime now, WallTime wallNow) {
  std::string blob;
  const StoreStatus status = store.read(kSeenQuakeStoreKey, blob);
  if (status != StoreStatus::Ok) {
    return status;  // NotFound は初回起動。異常ではない。
  }

  std::size_t cursor = 0;
  if (nextLine(blob, cursor) != kFormatVersion) {
    // 読めない版。状態は変更せず捨てる（起動を止めない）。
    return StoreStatus::BackendError;
  }

  const std::string_view firedLine = nextLine(blob, cursor);

  std::deque<std::string> restored;
  while (cursor < blob.size()) {
    const std::string_view line = nextLine(blob, cursor);
    if (line.empty()) {
      continue;
    }
    restored.emplace_back(line);
    if (restored.size() > constants::kSeenQuakeMax) {
      restored.pop_front();
    }
  }
  seen_ = std::move(restored);

  // ★以下、クールダウン基準が復元できない経路はすべて「明けている」側に倒す。
  //   「明けていない」に倒れると、本物の地震アラートを黙って握り潰す。
  //   最悪ケースは群発の 2 発目が 1 回余分に鳴ることで、そちらの方が軽い。
  if (firedLine == kNeverFired) {
    everFired_ = false;
    lastFiredAt_ = SteadyTime{};
    return StoreStatus::Ok;
  }

  const std::optional<std::int64_t> firedEpoch = fromDecimal(firedLine);
  if (!firedEpoch.has_value()) {
    everFired_ = false;
    lastFiredAt_ = SteadyTime{};
    return StoreStatus::Ok;
  }

  // 壁時計で測った経過を、今の単調時刻の座標系へ引き直す。
  // ★電源が落ちていた時間もクールダウンの経過として数える（実時間だから）。
  const std::int64_t elapsedMs = (wallNow.epochSeconds - *firedEpoch) * 1000;
  if (elapsedMs < 0) {
    // 壁時計が巻き戻っている（NTP 未同期で起動した等）。経過量が信用できない。
    everFired_ = false;
    lastFiredAt_ = SteadyTime{};
    return StoreStatus::Ok;
  }

  everFired_ = true;
  lastFiredAt_ = now - Millis{elapsedMs};
  return StoreStatus::Ok;
}

}  // namespace ambient::core
