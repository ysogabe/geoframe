// test_care_model.cpp — お世話の表示モデルと claim（Wave 4 / Phase 3）
//
// ############################################################################
// # ★ここで固定するもの
// #
// #   1. formatElapsed()      上流 ui_model.cpp からの純粋関数の移植（挙動不変）
// #   2. truncateLabelUtf8()  18 バイト上限。★上流と唯一挙動が違う点
// #   3. careLayout()         3 / 6 バケット化（上流が「呼び出し側の責務」と明記）
// #   4. RecordedAwaitingServer  「記録したら即 Green」は成立しない
// #   5. Red / Yellow はどちらも CareDue（display_state.hpp:36-37）
// #
// # ★状態（0-3）と shouldLight を **こちらで計算していない**ことも固定する。
// #   計算はサーバの責務（constants.hpp:140-142）。
// ############################################################################

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "ambient/core/care/care_model.hpp"
#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/state/arbiter.hpp"
#include "ambient/core/state/claim.hpp"

namespace {

using ambient::core::CareItem;
using ambient::core::CareLayout;
using ambient::core::CareModel;
using ambient::core::CareView;
using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::DisplayStateKind;
using ambient::core::HogoConfigSnapshot;
using ambient::core::Millis;
using ambient::core::PendingExit;
using ambient::core::ScheduleStatus;
using ambient::core::SteadyTime;
using ambient::core::WallTime;
namespace constants = ambient::core::constants;

[[nodiscard]] SteadyTime at(std::int64_t ms) noexcept { return SteadyTime{ms}; }
[[nodiscard]] WallTime epoch(std::int64_t seconds) noexcept { return WallTime{seconds}; }

// slotCount 件の slot を埋めた config を作る。status は Green。
[[nodiscard]] HogoConfigSnapshot makeConfig(std::size_t slotCount) {
  HogoConfigSnapshot snapshot;
  for (std::size_t i = 0; i < slotCount && i < constants::kCareItemCount; ++i) {
    snapshot.slots[i].present = true;
    snapshot.slots[i].label = "項目" + std::to_string(i);
    snapshot.slots[i].status = ScheduleStatus::Green;
    snapshot.slots[i].shouldLight = false;
  }
  snapshot.activeSlots = slotCount;
  return snapshot;
}

[[nodiscard]] std::vector<Claim> collectFrom(const CareModel& model, SteadyTime now) {
  std::vector<Claim> claims;
  model.collect(now, claims);
  return claims;
}

}  // namespace

// ============================================================================
// formatElapsed — 上流 ui_model.cpp:13-36 の移植（挙動を変えないこと）
// ============================================================================

TEST_CASE("care_model: formatElapsed は上流の分岐をそのまま再現する") {
  using ambient::core::formatElapsed;

  struct Row {
    std::int64_t last;
    std::int64_t now;
    const char* expected;
  };

  // ★表駆動。境界（59/60、3599/3600、86399/86400）を必ず含める。
  const Row rows[] = {
      // lastEpoch == 0 は未記録（★クランプより前に判定する）
      {0, 1'700'000'000, "未記録"},
      {0, 0, "未記録"},
      // 60 秒未満
      {1'700'000'000, 1'700'000'000, "たった今"},  // now == last
      {1'700'000'000, 1'700'000'059, "たった今"},
      // 分
      {1'700'000'000, 1'700'000'060, "1分前"},
      {1'700'000'000, 1'700'000'119, "1分前"},
      {1'700'000'000, 1'700'003'599, "59分前"},
      // 時間（分が 0 なら時間だけ）
      {1'700'000'000, 1'700'003'600, "1時間前"},
      {1'700'000'000, 1'700'003'660, "1時間1分前"},
      {1'700'000'000, 1'700'086'399, "23時間59分前"},
      // 日
      {1'700'000'000, 1'700'086'400, "1日前"},
      {1'700'000'000, 1'700'172'799, "1日前"},
      {1'700'000'000, 1'700'172'800, "2日前"},
      // ★巻き戻り（NTP 再同期など）は 0 にクランプ = "たった今"
      {1'700'000'100, 1'700'000'000, "たった今"},
  };

  std::size_t violations = 0;
  std::string firstFailure;
  for (const Row& row : rows) {
    const std::string actual = formatElapsed(epoch(row.last), epoch(row.now));
    if (actual != row.expected) {
      ++violations;
      if (firstFailure.empty()) {
        firstFailure = "last=" + std::to_string(row.last) + " now=" + std::to_string(row.now) +
                       " expected=" + row.expected + " actual=" + actual;
      }
    }
  }
  INFO(firstFailure);
  CHECK(violations == 0);
}

TEST_CASE("care_model: formatElapsed は int64 を uint32 へ縮めない") {
  // ★上流は uint32_t だが WallTime は int64。大きな epoch で静かに折り返す
  //   実装になっていないことを固定する。
  using ambient::core::formatElapsed;
  const std::int64_t beyond32 = 5'000'000'000;  // uint32 の範囲を超える
  CHECK(formatElapsed(epoch(beyond32 - 60), epoch(beyond32)) == "1分前");
  CHECK(formatElapsed(epoch(beyond32 - 3600), epoch(beyond32)) == "1時間前");
}

// ============================================================================
// truncateLabelUtf8 — ★上流と挙動が違う唯一の点
// ============================================================================

TEST_CASE("care_model: label は 18 バイト上限（UTF-8 のバイト数であって文字数ではない）") {
  using ambient::core::truncateLabelUtf8;

  CHECK(constants::kLabelMaxBytes == 18);

  // 全角 6 文字 = 18 バイト（ちょうど）→ そのまま。
  const std::string six = "あいうえおか";
  REQUIRE(six.size() == 18);
  CHECK(truncateLabelUtf8(six) == six);

  // 全角 7 文字 = 21 バイト → 18 バイト目が 3 バイト文字の途中に当たる。
  // ★上流の strlcpy はここで切り、不正な UTF-8 を作る。こちらは境界まで戻す。
  const std::string seven = "あいうえおかき";
  REQUIRE(seven.size() == 21);
  const std::string cut = truncateLabelUtf8(seven);
  CHECK(cut == six);
  CHECK(cut.size() == 18);
  CHECK(cut.size() <= constants::kLabelMaxBytes);  // 契約（18 バイト以内）は満たす
}

TEST_CASE("care_model: 丸めた結果は必ず有効な UTF-8 の境界で終わる") {
  using ambient::core::truncateLabelUtf8;

  // 末尾のバイト列がコードポイントとして完結しているか。
  const auto endsMidCodepoint = [](const std::string& text) {
    if (text.empty()) {
      return false;
    }
    std::size_t index = text.size() - 1;
    std::size_t trailing = 0;
    while (index > 0 && (static_cast<unsigned char>(text[index]) & 0xC0U) == 0x80U) {
      ++trailing;
      --index;
    }
    const auto lead = static_cast<unsigned char>(text[index]);
    std::size_t expected = 0;
    if ((lead & 0x80U) == 0x00U) {
      expected = 0;
    } else if ((lead & 0xE0U) == 0xC0U) {
      expected = 1;
    } else if ((lead & 0xF0U) == 0xE0U) {
      expected = 2;
    } else if ((lead & 0xF8U) == 0xF0U) {
      expected = 3;
    } else {
      return true;  // 継続バイトが先頭に来た = 壊れている
    }
    return trailing != expected;
  };

  // 1〜4 バイト文字を混ぜた文字列を、あらゆる上限で切ってみる。
  const std::string mixed = "aあiいuうeえoお絵文字🐕テスト漢字ABC";
  std::size_t violations = 0;
  for (std::size_t limit = 0; limit <= mixed.size() + 4; ++limit) {
    const std::string cut = truncateLabelUtf8(mixed, limit);
    if (limit <= mixed.size() && cut.size() > limit) {
      ++violations;  // 上限を超えた
    }
    if (endsMidCodepoint(cut)) {
      ++violations;  // コードポイントの途中で切れた
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("care_model: ASCII の label は素直に切れる") {
  using ambient::core::truncateLabelUtf8;
  CHECK(truncateLabelUtf8("abc") == "abc");
  CHECK(truncateLabelUtf8("0123456789012345678901234567").size() == 18);
  CHECK(truncateLabelUtf8("").empty());
}

// ============================================================================
// 3 / 6 バケット化 — 上流 tab5/main.cpp:140-142
// ============================================================================

TEST_CASE("care_model: careLayout は 3 か 6 の 2 値だけを返す") {
  struct Row {
    std::size_t activeSlots;
    std::size_t expectedItems;
    std::size_t expectedRows;
  };
  // 出典: return (serverSync.activeItemCount() >= 4) ? 6 : 3;
  // ★0（未 poll）も 3 に落ちる。境界（3 / 4）を必ず含める。
  const Row rows[] = {
      {0, 3, 1}, {1, 3, 1}, {2, 3, 1}, {3, 3, 1},
      {4, 6, 2}, {5, 6, 2}, {6, 6, 2}, {99, 6, 2},
  };

  std::size_t violations = 0;
  for (const Row& row : rows) {
    const CareLayout layout = ambient::core::careLayout(row.activeSlots);
    if (layout.itemCount != row.expectedItems || layout.rows != row.expectedRows ||
        layout.columns != 3) {
      ++violations;
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("care_model: view のカード数がレイアウトに追従する（6→3 の縮小も）") {
  CareModel model;

  model.applyConfig(makeConfig(6));
  CHECK(model.layout().itemCount == 6);
  CHECK(model.view(at(0), epoch(1'700'000'000)).cards.size() == 6);

  // ★上流 activeSlots は poll ごとに数え直すので縮小に追従する
  //   (server_sync.cpp:336-338)。sticky な _hasLabel を数えていない証拠。
  model.applyConfig(makeConfig(3));
  CHECK(model.layout().itemCount == 3);
  CHECK(model.view(at(0), epoch(1'700'000'000)).cards.size() == 3);
}

// ============================================================================
// RecordedAwaitingServer — 「記録したら即 Green」は成立しない
// ============================================================================

TEST_CASE("care_model: 記録直後は RecordedAwaitingServer、色はサーバ待ち") {
  CareModel model;

  // サーバは Red と言っている。
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[0].status = ScheduleStatus::Red;
  config.slots[0].lastEpoch = epoch(0);
  model.applyConfig(config);

  // 記録 → 保留（2 秒猶予）。
  const auto previous = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(previous.has_value());

  {
    const std::vector<Claim> claims = collectFrom(model, at(100));
    REQUIRE(claims.size() == 1);
    CHECK(claims[0].priority == ClaimPriority::RecordedAwaitingServer);
    CHECK(claims[0].kind == DisplayStateKind::RecordedAwaitingServer);
    CHECK(claims[0].sourceId == CareModel::kSourceId);
  }

  // 猶予満了で確定。
  const auto committed = model.tick(at(constants::kToastTotal.value));
  REQUIRE(committed.has_value());
  CHECK(committed->item == CareItem::Toilet);
  CHECK(committed->recordedAt == epoch(1'700'000'000));

  // ★確定後も RecordedAwaitingServer のまま（サーバの status はまだ Red）。
  {
    const std::vector<Claim> claims = collectFrom(model, at(3'000));
    REQUIRE(claims.size() == 1);
    CHECK(claims[0].priority == ClaimPriority::RecordedAwaitingServer);
  }

  // ★経過時間だけは即座にローカル値を反映する（色はサーバ待ち、時刻はローカル）。
  const CareView view = model.view(at(3'000), epoch(1'700'000'030));
  REQUIRE(view.cards.size() == 3);
  CHECK(view.cards[0].elapsed == "たった今");
  CHECK(view.cards[0].lastEpoch == epoch(1'700'000'000));
  CHECK(view.cards[0].status == ScheduleStatus::Red);  // ★色は依然 Red
  CHECK(view.cards[0].awaitingServer);
}

TEST_CASE("care_model: 次の config poll がサーバ側に追いつくと待ちが明ける") {
  CareModel model;
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[0].status = ScheduleStatus::Red;
  model.applyConfig(config);

  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  REQUIRE(model.awaitingServer(at(2'000)));

  // サーバが記録を取り込んで Green になった config が降りてくる。
  HogoConfigSnapshot updated = makeConfig(3);
  updated.slots[0].status = ScheduleStatus::Green;
  updated.slots[0].lastEpoch = epoch(1'700'000'000);
  model.applyConfig(updated);

  CHECK_FALSE(model.awaitingServer(at(2'100)));
  CHECK(collectFrom(model, at(2'100)).empty());  // Green なので CareDue も出ない
}

TEST_CASE("care_model: サーバが追いつかなくても最強 claim はラッチしない") {
  // ★記録 POST が総試行 5 回を使い切って破棄されると、サーバは永久にその記録を
  //   知らない。ここで待ち続けると RecordedAwaitingServer（最強）が固着し、
  //   地震アラートまで抑止される。二重に止めていることを確認する。
  CareModel model;
  model.applyConfig(makeConfig(3));

  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());

  const std::int64_t limit = CareModel::kAwaitingServerMax.value;
  CHECK(limit == constants::kPollInterval.value * 3);

  // 期限内は出る。
  {
    const std::vector<Claim> claims = collectFrom(model, at(2'000 + limit - 1));
    REQUIRE(claims.size() == 1);
    CHECK(claims[0].priority == ClaimPriority::RecordedAwaitingServer);
    // ★1: expiresAt（安全網）
    REQUIRE(claims[0].expiresAt.has_value());
    CHECK(*claims[0].expiresAt == at(2'000 + limit));
  }

  // ★2: source 側でも出すのをやめる（claim.hpp:148-150 が expiresAt だけに
  //   頼るなと明記している）。
  CHECK(collectFrom(model, at(2'000 + limit)).empty());
  CHECK_FALSE(model.awaitingServer(at(2'000 + limit)));
}

TEST_CASE("care_model: RecordedAwaitingServer は Alert より強い（Alert は最大ではない）") {
  // ★claim.hpp の突合結果: 企画書 §2.3 rank 2「記録操作と取り消し猶予」>
  //   rank 3「緊急地震通知」。`priority == Alert` を「最上位」の意味で
  //   書いたコードは誤りになる。
  CareModel model;
  model.applyConfig(makeConfig(3));
  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());

  Claim alert;
  alert.priority = ClaimPriority::Alert;
  alert.kind = DisplayStateKind::Alert;
  alert.sourceId = 0xEE'00'00'01;

  std::vector<Claim> claims;
  claims.push_back(alert);  // 先に積んでも負けないこと（安定選択に紛れないため）
  model.collect(at(500), claims);

  const auto decision = ambient::core::decide(std::span<const Claim>{claims}, at(500));
  REQUIRE(decision.has_value());
  CHECK(decision->priority == ClaimPriority::RecordedAwaitingServer);
  CHECK(decision->kind == DisplayStateKind::RecordedAwaitingServer);

  // ★保留が明ければ地震が自動的に前へ出る（抑止ロジックを書かない設計の証拠）。
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  model.setAwaitingServerMax(Millis{0});  // サーバ待ちも明けた状況にする

  std::vector<Claim> later;
  later.push_back(alert);
  model.collect(at(2'001), later);
  const auto afterDecision = ambient::core::decide(std::span<const Claim>{later}, at(2'001));
  REQUIRE(afterDecision.has_value());
  CHECK(afterDecision->priority == ClaimPriority::Alert);
}

// ============================================================================
// Red / Yellow はどちらも CareDue（display_state.hpp:36-37）
// ============================================================================

TEST_CASE("care_model: Red も Yellow も CareDue（Care へ落とさない）") {
  struct Row {
    ScheduleStatus status;
    bool expectClaim;
  };
  const Row rows[] = {
      {ScheduleStatus::None, false},
      {ScheduleStatus::Green, false},
      {ScheduleStatus::Yellow, true},
      {ScheduleStatus::Red, true},
  };

  std::size_t violations = 0;
  for (const Row& row : rows) {
    CareModel model;
    HogoConfigSnapshot config = makeConfig(3);
    config.slots[0].status = row.status;
    model.applyConfig(config);

    const std::vector<Claim> claims = collectFrom(model, at(0));
    if (claims.empty() == row.expectClaim) {
      ++violations;
      continue;
    }
    if (row.expectClaim && (claims[0].priority != ClaimPriority::CareDue ||
                            claims[0].kind != DisplayStateKind::CareDue)) {
      ++violations;
    }
  }
  CHECK(violations == 0);
}

TEST_CASE("care_model: Red は Yellow より前の slot にあっても後ろにあっても勝つ") {
  {
    CareModel model;
    HogoConfigSnapshot config = makeConfig(3);
    config.slots[0].status = ScheduleStatus::Yellow;
    config.slots[2].status = ScheduleStatus::Red;
    model.applyConfig(config);

    const CareView view = model.view(at(0), epoch(1'700'000'000));
    REQUIRE(view.dueSlot.has_value());
    CHECK(*view.dueSlot == 2);
    CHECK(view.dueStatus == ScheduleStatus::Red);
  }
  {
    CareModel model;
    HogoConfigSnapshot config = makeConfig(3);
    config.slots[0].status = ScheduleStatus::Red;
    config.slots[2].status = ScheduleStatus::Yellow;
    model.applyConfig(config);

    const CareView view = model.view(at(0), epoch(1'700'000'000));
    REQUIRE(view.dueSlot.has_value());
    CHECK(*view.dueSlot == 0);
    CHECK(view.dueStatus == ScheduleStatus::Red);
  }
}

TEST_CASE("care_model: レイアウト外の slot に残った status を拾わない") {
  // 6 項目で Red だった slot5 が、3 項目へ縮小した config で表示対象外になる。
  CareModel model;
  HogoConfigSnapshot wide = makeConfig(6);
  wide.slots[5].status = ScheduleStatus::Red;
  model.applyConfig(wide);
  REQUIRE_FALSE(collectFrom(model, at(0)).empty());

  model.applyConfig(makeConfig(3));  // slot5 は present も status も落ちる
  CHECK(collectFrom(model, at(0)).empty());
}

TEST_CASE("care_model: view の dueSlot と claim は必ず一致する") {
  // ★別々に書くと「画面は Red なのに遷移は Yellow」が起きる。同じ関数を使っている証拠。
  CareModel model;
  HogoConfigSnapshot config = makeConfig(6);
  config.slots[1].status = ScheduleStatus::Yellow;
  config.slots[4].status = ScheduleStatus::Red;
  model.applyConfig(config);

  const CareView view = model.view(at(0), epoch(1'700'000'000));
  const std::vector<Claim> claims = collectFrom(model, at(0));
  REQUIRE(view.dueSlot.has_value());
  REQUIRE(claims.size() == 1);
  const std::uint32_t expected = (static_cast<std::uint32_t>(*view.dueSlot) + 1U) * 16U +
                                 static_cast<std::uint32_t>(view.dueStatus);
  CHECK(claims[0].payloadId == expected);
  CHECK(*view.dueSlot == 4);
  CHECK(view.dueStatus == ScheduleStatus::Red);
}

TEST_CASE("care_model: Yellow から Red へ変わると payloadId が変わる（画面を更新させる）") {
  CareModel model;
  HogoConfigSnapshot yellow = makeConfig(3);
  yellow.slots[0].status = ScheduleStatus::Yellow;
  model.applyConfig(yellow);
  const std::vector<Claim> before = collectFrom(model, at(0));
  REQUIRE(before.size() == 1);

  HogoConfigSnapshot red = makeConfig(3);
  red.slots[0].status = ScheduleStatus::Red;
  model.applyConfig(red);
  const std::vector<Claim> after = collectFrom(model, at(0));
  REQUIRE(after.size() == 1);

  CHECK(before[0].kind == after[0].kind);            // 同じ画面種別
  CHECK(before[0].payloadId != after[0].payloadId);  // ★でも別の内容
}

// ============================================================================
// D-4 の不変条件を作り直していないこと
// ============================================================================

TEST_CASE("care_model: 取り消しヒットだけが記録を破棄する") {
  // ★不変条件そのものは test_care_pending.cpp が全経路で網羅する。
  //   ここは CareModel が PendingCareRecord を素通ししていることの確認。
  {
    CareModel model;
    const auto ignored = model.beginRecord(CareItem::Eye, at(0), epoch(1'700'000'000));
    CHECK_FALSE(ignored.has_value());
    const auto discarded = model.leave(PendingExit::UndoHit, at(500));
    CHECK_FALSE(discarded.has_value());
    CHECK_FALSE(model.awaitingServer(at(500)));
    CHECK(model.lastEpochFor(CareItem::Eye) == epoch(0));  // ローカルにも残らない
  }
  {
    CareModel model;
    const auto ignored = model.beginRecord(CareItem::Eye, at(0), epoch(1'700'000'000));
    CHECK_FALSE(ignored.has_value());
    // ★UndoHit 以外はすべて確定（既定が確定）。
    const auto committed = model.leave(PendingExit::NavigatedAway, at(500));
    REQUIRE(committed.has_value());
    CHECK(committed->item == CareItem::Eye);
    CHECK(model.awaitingServer(at(500)));
    CHECK(model.lastEpochFor(CareItem::Eye) == epoch(1'700'000'000));
  }
}

TEST_CASE("care_model: 別カードをタップすると 1 件目が確定して返る") {
  CareModel model;
  const auto first = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(first.has_value());

  // ★戻り値を捨てると 1 件目の記録が失われる（[[nodiscard]]）。
  const auto previous = model.beginRecord(CareItem::Food, at(500), epoch(1'700'000'005));
  REQUIRE(previous.has_value());
  CHECK(previous->item == CareItem::Toilet);
  CHECK(previous->recordedAt == epoch(1'700'000'000));

  // 1 件目はローカルへ反映済み、2 件目は保留中。
  CHECK(model.lastEpochFor(CareItem::Toilet) == epoch(1'700'000'000));
  CHECK(model.pending().active());
  CHECK(model.pending().pendingItem() == CareItem::Food);
}

// ============================================================================
// max(ローカル, サーバ) — 上流 tab5/main.cpp:156-157
// ============================================================================

TEST_CASE("care_model: 経過時間は max(ローカル, サーバ) を使う") {
  CareModel model;

  // サーバの方が新しい（他の端末やダッシュボードから記録された）。
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[0].lastEpoch = epoch(1'700'000'500);
  model.applyConfig(config);
  CHECK(model.lastEpochFor(CareItem::Toilet) == epoch(1'700'000'500));

  // ローカルの方が新しい（今この端末で押した）。
  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'900));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  CHECK(model.lastEpochFor(CareItem::Toilet) == epoch(1'700'000'900));

  // ★古い config が降りてきてもローカルの新しい値を消さない。
  HogoConfigSnapshot stale = makeConfig(3);
  stale.slots[0].lastEpoch = epoch(1'700'000'500);
  model.applyConfig(stale);
  CHECK(model.lastEpochFor(CareItem::Toilet) == epoch(1'700'000'900));
}

// ============================================================================
// Care（ユーザが開いた画面）は status からは出さない
// ============================================================================

TEST_CASE("care_model: Care はユーザ操作でだけ出る") {
  CareModel model;
  model.applyConfig(makeConfig(3));  // 全部 Green
  CHECK(collectFrom(model, at(0)).empty());

  model.openCareScreen(at(0), Millis{20'000});
  {
    const std::vector<Claim> claims = collectFrom(model, at(1'000));
    REQUIRE(claims.size() == 1);
    CHECK(claims[0].priority == ClaimPriority::Care);
    CHECK(claims[0].kind == DisplayStateKind::Care);
  }

  CHECK(collectFrom(model, at(20'000)).empty());  // 期限で自動的に消える

  model.openCareScreen(at(30'000), Millis{20'000});
  model.closeCareScreen();
  CHECK(collectFrom(model, at(30'100)).empty());
}

TEST_CASE("care_model: CareDue は Care より強い") {
  CareModel model;
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[0].status = ScheduleStatus::Red;
  model.applyConfig(config);
  model.openCareScreen(at(0), Millis{20'000});

  const std::vector<Claim> claims = collectFrom(model, at(100));
  REQUIRE(claims.size() == 1);
  CHECK(claims[0].priority == ClaimPriority::CareDue);
}

// ============================================================================
// collect() はレベルトリガであること（claim.hpp §5.3）
// ============================================================================

TEST_CASE("care_model: collect は何度呼んでも同じ答えを返す") {
  CareModel model;
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[1].status = ScheduleStatus::Red;
  model.applyConfig(config);

  const std::vector<Claim> a = collectFrom(model, at(1'000));
  const std::vector<Claim> b = collectFrom(model, at(1'000));
  const std::vector<Claim> c = collectFrom(model, at(1'000));
  REQUIRE(a.size() == 1);
  REQUIRE(b.size() == 1);
  REQUIRE(c.size() == 1);
  CHECK(a[0].payloadId == b[0].payloadId);
  CHECK(b[0].payloadId == c[0].payloadId);
  CHECK(a[0].priority == c[0].priority);
}

TEST_CASE("care_model: collect は out をクリアしない（追記する）") {
  CareModel model;
  HogoConfigSnapshot config = makeConfig(3);
  config.slots[0].status = ScheduleStatus::Red;
  model.applyConfig(config);

  std::vector<Claim> claims;
  Claim photo;
  photo.priority = ClaimPriority::Photo;
  claims.push_back(photo);
  model.collect(at(0), claims);
  CHECK(claims.size() == 2);
}

// ============================================================================
// 時間跳躍
// ============================================================================

TEST_CASE("care_model: スリープ復帰でサーバ待ちが明ける") {
  CareModel model;
  model.applyConfig(makeConfig(3));
  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  REQUIRE(model.awaitingServer(at(2'000)));

  // 8 時間スリープ。実時間では 8 時間経っているので、サーバ待ちは打ち切ってよい。
  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{1'000};
  jump.wallDelta = Millis{8 * 60 * 60 * 1000};
  jump.skew = jump.wallDelta - jump.steadyDelta;
  model.onTimeJump(jump);

  CHECK_FALSE(model.awaitingServer(at(3'000)));
}

TEST_CASE("care_model: 保留（2 秒猶予）は時間跳躍で触らない") {
  // ★clock.hpp:199-205 が明記: 不変条件が既に安全側（確定）に倒れているので、
  //   跳躍補正を足しても「いつ確定するか」しか変わらない。触らないのが最も安全。
  CareModel model;
  const auto ignored = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());

  ambient::core::TimeJump jump;
  jump.steadyDelta = Millis{100};
  jump.wallDelta = Millis{8 * 60 * 60 * 1000};
  jump.skew = jump.wallDelta - jump.steadyDelta;
  model.onTimeJump(jump);

  // 単調時刻が進んでいないので保留はまだ生きている。
  CHECK(model.pending().active());
  // 復帰後の最初の tick で **確定** する（破棄ではない）。
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  CHECK(committed->recordedAt == epoch(1'700'000'000));
}

// ============================================================================
// config を受け取る前の既定（安全側）
// ============================================================================

TEST_CASE("care_model: config 未取得なら shouldLight は安全側で true、claim は出ない") {
  CareModel model;
  CHECK_FALSE(model.configReceived());
  CHECK(model.layout().itemCount == 3);  // 未 poll のフォールバック

  const CareView view = model.view(at(0), epoch(1'700'000'000));
  CHECK_FALSE(view.configReceived);
  REQUIRE(view.cards.size() == 3);

  std::size_t violations = 0;
  for (const auto& card : view.cards) {
    // ★false 既定にすると「画面が点灯しない」へ意味が反転する（server_sync.h:169-171）。
    if (!card.shouldLight) {
      ++violations;
    }
    if (card.status != ScheduleStatus::None) {
      ++violations;
    }
    if (card.elapsed != "未記録") {
      ++violations;
    }
  }
  CHECK(violations == 0);
  CHECK(collectFrom(model, at(0)).empty());
}

// ============================================================================
// ★レイアウト外へ落ちた slot が最強 claim をラッチしないこと（回帰テスト）
// ============================================================================

TEST_CASE("care_model: レイアウト外へ落ちた slot はサーバ待ちを出さない") {
  // 6 項目で slot5 を記録 → config が 3 項目へ縮小。
  // slot5 は present でなくなるので applyConfig の「サーバが追いついたら解除」経路を
  // 通らない。ここで RecordedAwaitingServer（★最強）を出し続けると、
  // 画面に無いカードのために地震アラートまで抑止される。
  CareModel model;
  model.applyConfig(makeConfig(6));

  const auto ignored = model.beginRecord(CareItem::Slot5, at(0), epoch(1'700'000'000));
  CHECK_FALSE(ignored.has_value());
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  REQUIRE_FALSE(collectFrom(model, at(2'000)).empty());  // 6 項目のうちは出てよい

  model.applyConfig(makeConfig(3));  // slot5 がレイアウト外へ落ちる

  CHECK(collectFrom(model, at(2'100)).empty());
  CHECK_FALSE(model.awaitingServer(at(2'100)));
}

TEST_CASE("care_model: 同じカードの再タップは重複記録にならない") {
  // 上流 tab5/main.cpp:305-307「同じカードの保留中に再タップ -> 無視(重複記録防止)」。
  // ★ガードが無いと leave(OtherCardTapped) 経由で 1 件目が **確定** され、
  //   1 タップのつもりが 2 件記録される（ダッシュボードを見るまで気づけない）。
  CareModel model;
  const auto first = model.beginRecord(CareItem::Toilet, at(0), epoch(1'700'000'000));
  CHECK_FALSE(first.has_value());

  const auto second = model.beginRecord(CareItem::Toilet, at(500), epoch(1'700'000'005));
  CHECK_FALSE(second.has_value());  // ★何も確定しない

  // 保留は 1 件目のまま生きており、猶予の起点も動かない（残り 1.5 秒）。
  CHECK(model.pending().active());
  CHECK(model.pending().pendingItem() == CareItem::Toilet);
  CHECK(model.pending().remaining(at(500)) == Millis{1'500});

  // 満了で確定するのは 1 件だけ、時刻は **1 回目のタップ**のもの。
  const auto committed = model.tick(at(2'000));
  REQUIRE(committed.has_value());
  CHECK(committed->recordedAt == epoch(1'700'000'000));
  CHECK_FALSE(model.tick(at(3'000)).has_value());  // 2 件目は存在しない
}
