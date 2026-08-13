// tests/unit/test_care_pending.cpp — お世話記録の保留（レーン 1A）
//
// ############################################################################
// # ★ここでテストしているのは 1 つの不変条件である
// #
// #   保留は「取り消しボタンへのヒット」でのみ破棄される。
// #   それ以外のすべての離脱経路は、離脱前に確定する。
// #
// # ★新しい離脱経路（PendingExit の値）を足したら、
// #   「離脱経路を全部試す」テストが自動的にそれを拾うようにしてある。
// #   経路ごとにテストを書き足す運用にすると、書き忘れが起きる。
// ############################################################################
//
// ★次フェーズでここに書くもの:
//   - 確定した記録が送信キューへ積まれること（QUEUE_SIZE=8、満杯なら epoch 最小を上書き）
//   - 送信成功後に RecordedAwaitingServer へ入り、次の config poll で抜けること
//   - スリープ復帰（時間跳躍）でトーストの残り秒が即座に再計算されること

#include <doctest.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <vector>

#include "ambient/core/care/pending_record.hpp"
#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/state/arbiter.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/state_machine.hpp"

using ambient::core::CareItem;
using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::DisplayStateKind;
using ambient::core::FakeClock;
using ambient::core::Millis;
using ambient::core::PendingCareRecord;
using ambient::core::PendingExit;
using ambient::core::PendingOutcome;
using ambient::core::StateMachine;
using ambient::core::SteadyTime;
using ambient::core::TimeJumpDetector;
using ambient::core::WallTime;

namespace {

Claim makeClaim(ClaimPriority priority, DisplayStateKind kind) {
  Claim c{};
  c.priority = priority;
  c.kind = kind;
  return c;
}

// ★離脱経路の全列挙。PendingExit に値を足したらここにも足すこと。
//
//   足し忘れは静かに通らない。すぐ下の static_assert が
//   kPendingExitCount との不一致でコンパイルを落とす。
//   （そして kPendingExitCount を上げ忘れたら、pending_record.cpp の
//     default 無し switch が -Wswitch を出す。ヘッダのコメント参照）
constexpr std::array<PendingExit, ambient::core::kPendingExitCount> kAllExits{
    PendingExit::UndoHit,       PendingExit::ToastExpired, PendingExit::OtherCardTapped,
    PendingExit::NavigatedAway, PendingExit::IdleTimeout,  PendingExit::AppShutdown,
};

// ★新しい離脱経路を足したのに、ここへ入れ忘れた場合の検出。
//   「最後の値」を基準に列挙すると、後ろに足された値を素通りしてしまう。
static_assert(kAllExits.back() == PendingExit::AppShutdown,
              "PendingExit に値を足したら kAllExits にも足し、この番兵も更新すること");

}  // namespace

TEST_CASE("★不変条件: UndoHit だけが破棄。それ以外はすべて確定") {
  for (const PendingExit exit : kAllExits) {
    PendingCareRecord pending;
    (void)pending.begin(CareItem::Toilet, SteadyTime{0}, WallTime{1700000000});
    REQUIRE(pending.active());

    const auto committed = pending.leave(exit);

    if (exit == PendingExit::UndoHit) {
      CHECK_FALSE(committed.has_value());
      CHECK(pending.lastOutcome() == PendingOutcome::Discarded);
    } else {
      // ★経路が何であれ確定する。これが不変条件。
      CHECK_MESSAGE(committed.has_value(), ambient::core::toString(exit));
      CHECK(pending.lastOutcome() == PendingOutcome::Committed);
      CHECK(committed->item == CareItem::Toilet);
      CHECK(committed->recordedAt.epochSeconds == 1700000000);
    }
    CHECK_FALSE(pending.active());
  }
}

TEST_CASE("★discardsPending は UndoHit だけを名指しする（既定は確定）") {
  static_assert(ambient::core::discardsPending(PendingExit::UndoHit));
  for (const PendingExit exit : kAllExits) {
    if (exit != PendingExit::UndoHit) {
      CHECK_FALSE(ambient::core::discardsPending(exit));
    }
  }
}

TEST_CASE("トースト満了（2 秒）で確定する") {
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Food, SteadyTime{0}, WallTime{1700000000});

  CHECK(pending.grace() == ambient::core::constants::kToastTotal);
  CHECK(pending.grace().value == 2000);

  CHECK_FALSE(pending.tick(SteadyTime{1999}).has_value());
  CHECK(pending.active());

  const auto committed = pending.tick(SteadyTime{2000});
  REQUIRE(committed.has_value());
  CHECK(committed->item == CareItem::Food);
  CHECK_FALSE(pending.active());
}

TEST_CASE("猶予内に UndoHit すれば破棄される") {
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Eye, SteadyTime{0}, WallTime{1700000000});
  CHECK_FALSE(pending.leave(PendingExit::UndoHit).has_value());
  CHECK_FALSE(pending.tick(SteadyTime{5000}).has_value());  // 満了しても何も出ない
}

TEST_CASE("★別カードのタップ: 1 件目は確定される（破棄されない）") {
  PendingCareRecord pending;
  CHECK_FALSE(pending.begin(CareItem::Toilet, SteadyTime{0}, WallTime{1700000000}).has_value());

  // 2 件目を開始 → 1 件目が確定して返る。★戻り値を捨てると記録が消える。
  const auto first = pending.begin(CareItem::Food, SteadyTime{500}, WallTime{1700000001});
  REQUIRE(first.has_value());
  CHECK(first->item == CareItem::Toilet);

  CHECK(pending.active());
  CHECK(pending.pendingItem() == CareItem::Food);
}

TEST_CASE("leave は冪等（保留が無いときに呼んでも無害）") {
  PendingCareRecord pending;
  CHECK_FALSE(pending.leave(PendingExit::AppShutdown).has_value());
  CHECK_FALSE(pending.leave(PendingExit::UndoHit).has_value());
}

TEST_CASE("残り猶予の表示") {
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Toilet, SteadyTime{1000}, WallTime{1700000000});
  CHECK(pending.remaining(SteadyTime{1000}).value == 2000);
  CHECK(pending.remaining(SteadyTime{1500}).value == 1500);
  CHECK(pending.remaining(SteadyTime{3000}).value == 0);
  CHECK(pending.remaining(SteadyTime{9999}).value == 0);  // 負にならない
}

TEST_CASE("★スリープ復帰: 猶予を大きく過ぎていても、確定であって破棄ではない") {
  // 単調時計が止まっている間にトーストが満了すべきだったケース。
  // 復帰後の最初の tick で確定する（記録が消えてはいけない）。
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Slot3, SteadyTime{0}, WallTime{1700000000});

  const auto committed = pending.tick(SteadyTime{8 * 60 * 60 * 1000});
  REQUIRE(committed.has_value());
  CHECK(committed->item == CareItem::Slot3);
  CHECK(pending.lastOutcome() == PendingOutcome::Committed);
}

TEST_CASE("記録時刻は壁時計で持つ（サーバへ送る絶対時刻）") {
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Toilet, SteadyTime{123}, WallTime{1764500000});
  const auto committed = pending.leave(PendingExit::ToastExpired);
  REQUIRE(committed.has_value());
  // ★単調時刻 (123) ではなく壁時計が記録されている
  CHECK(committed->recordedAt.epochSeconds == 1764500000);
}

TEST_CASE("★PendingExit に値を足しても既定が確定側であること") {
  // ############################################################################
  // # 不変条件を守る仕掛けは「既定が確定」であることそのものである。
  // # 新しい離脱経路（設定画面 / 画面ロック / 外部からの表示切替 ……）を
  // # 足したときに、確定を書き忘れる余地が無いことをここで固定する。
  // #
  // # ★このテスト自身は「今ある値」しか走らない。値の追加を検出するのは
  // #   このテストではなく、次の 2 つである（どちらも黙って通らない）:
  // #     1. pending_record.cpp の default 無し switch → -Wswitch
  // #     2. kAllExits のサイズ／番兵に対する static_assert（上）
  // #   ここはその先で「足された値も確定側に落ちる」ことを保証する部分。
  // ############################################################################
  std::size_t discardCount = 0;
  std::size_t commitFailures = 0;

  // ★ループの中で CHECK を撃たない。集計してから 1 回だけ検査する。
  for (const PendingExit exit : kAllExits) {
    PendingCareRecord pending;
    (void)pending.begin(CareItem::Toilet, SteadyTime{0}, WallTime{1700000000});
    const auto committed = pending.leave(exit);

    if (ambient::core::discardsPending(exit)) {
      ++discardCount;
      if (committed.has_value()) {
        ++commitFailures;  // 破棄経路なのに確定した
      }
    } else if (!committed.has_value()) {
      ++commitFailures;  // 確定経路なのに記録が消えた
    }
  }

  // ★破棄する経路は永久に 1 つだけ（UndoHit）。
  //   discardsPending() に条件を足すとここが落ちる。
  CHECK(discardCount == 1);
  CHECK(commitFailures == 0);
  CHECK(kAllExits.size() == ambient::core::kPendingExitCount);
}

TEST_CASE("★列挙の外側の値を渡しても確定側に倒れる（既定が確定であることの証明）") {
  // 「新しい離脱経路を足す」を、まだ名前の無い値で先取りして再現する。
  // ★discardsPending() は UndoHit だけを名指しするので、
  //   未知の値は自動的に確定側になる。これが「書き忘れる余地が無い」の実体。
  const auto futureExit = static_cast<PendingExit>(ambient::core::kPendingExitCount);
  CHECK_FALSE(ambient::core::discardsPending(futureExit));

  PendingCareRecord pending;
  (void)pending.begin(CareItem::Food, SteadyTime{0}, WallTime{1700000000});
  const auto committed = pending.leave(futureExit);
  REQUIRE(committed.has_value());  // ★記録は確定する。黙って消えない。
  CHECK(committed->item == CareItem::Food);
  CHECK(pending.lastOutcome() == PendingOutcome::Committed);
}

// ============================================================================
// RecordedAwaitingServer ― 「記録したら即 Green」は成立しない
// ============================================================================

TEST_CASE("★確定 → RecordedAwaitingServer → 次の poll で Care/CareDue へ落ちる") {
  // ############################################################################
  // # 記録の状態色（Red/Yellow/Green）は **サーバが計算して返す**。
  // # POST が 200 で返っても、次の config poll（最大 30 秒）まで色は Red のまま。
  // # この状態は「ローカルの記録時刻を経過時間表示に反映しつつ、色はサーバ待ち」
  // # を表す。Mac 側でしきい値を再実装しない（二重管理は必ずずれる）。
  // ############################################################################
  StateMachine machine;
  std::vector<Claim> claims;

  const Claim photo = makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo);
  // サーバはまだ Red を返している（記録が反映されていない）。
  const Claim careDueRed = makeClaim(ClaimPriority::CareDue, DisplayStateKind::CareDue);

  claims = {photo, careDueRed};
  (void)machine.update(claims, SteadyTime{0});
  REQUIRE(machine.current() == DisplayStateKind::CareDue);

  // ユーザがカードを押し、2 秒後に確定して送信キューへ積まれた。
  PendingCareRecord pending;
  (void)pending.begin(CareItem::Toilet, SteadyTime{1000}, WallTime{1700000000});
  const auto committed = pending.tick(SteadyTime{1000} + ambient::core::constants::kToastTotal);
  REQUIRE(committed.has_value());

  // 送信成功。★サーバはまだ Red のまま（次の poll まで最大 30 秒）。
  Claim awaiting = makeClaim(ClaimPriority::RecordedAwaitingServer,
                             DisplayStateKind::RecordedAwaitingServer);
  claims = {photo, careDueRed, awaiting};
  const auto entered = machine.update(claims, SteadyTime{3000});
  REQUIRE(entered.has_value());
  CHECK(entered->from == DisplayStateKind::CareDue);
  CHECK(entered->to == DisplayStateKind::RecordedAwaitingServer);

  // poll が来るまでは Red が出続けていても表示は変わらない。
  CHECK_FALSE(machine.update(claims, SteadyTime{20000}).has_value());

  // 次の config poll が Green を返した。
  // → RecordedAwaitingServer の source が claim をやめ、CareDue も消える。
  claims = {photo, makeClaim(ClaimPriority::Care, DisplayStateKind::Care)};
  const auto settled = machine.update(claims, SteadyTime{33000});
  REQUIRE(settled.has_value());
  CHECK(settled->from == DisplayStateKind::RecordedAwaitingServer);
  CHECK(settled->to == DisplayStateKind::Care);
}

TEST_CASE("★RecordedAwaitingServer は地震アラートより強い（操作中の保留）") {
  // 押した手応えを地震に横取りさせない。企画書 §2.3 rank 2 > rank 3。
  const std::vector<Claim> claims{
      makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo),
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert),
      makeClaim(ClaimPriority::RecordedAwaitingServer,
                DisplayStateKind::RecordedAwaitingServer),
  };
  const auto d = ambient::core::decide(claims, SteadyTime{0});
  REQUIRE(d.has_value());
  CHECK(d->kind == DisplayStateKind::RecordedAwaitingServer);
}

TEST_CASE("★時間跳躍: 保留には跳躍補正を掛けない（掛ける必要が無い）") {
  // ############################################################################
  // # 決めたこと: PendingCareRecord は TimeJump を受け取らない。
  // #
  // #   理由: 不変条件が既に安全側に倒れている。
  // #   スリープ中に猶予が満了していたら、復帰後の最初の tick() が
  // #   ToastExpired で **確定** する（破棄ではない）。
  // #   スリープ中にユーザが取り消しボタンを押せた道理も無い。
  // #   跳躍補正を足しても「いつ確定するか」しか変わらず、
  // #   「確定するか破棄するか」は変わらない。API を増やす方が危険である。
  // #   （UI の残り秒表示の再計算は Renderer の責務）
  // ############################################################################
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  PendingCareRecord pending;
  (void)pending.begin(CareItem::Eye, clock.steadyNow(), clock.wallNow());

  // 猶予のさなかに 8 時間スリープした。単調時計は止まっている。
  clock.simulateSleep(Millis{8 * 60 * 60 * 1000});
  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());  // 跳躍は検知されている

  // ★補正を掛けなくても、単調時刻が止まっていたぶんは
  //   「猶予が経過していない」= まだ保留中、として一貫している。
  CHECK(pending.active());
  CHECK(pending.remaining(clock.steadyNow()).value ==
        ambient::core::constants::kToastTotal.value);

  // 復帰後、時間が進めば次の tick で **確定** する。破棄されない。
  clock.advance(ambient::core::constants::kToastTotal);
  const auto committed = pending.tick(clock.steadyNow());
  REQUIRE(committed.has_value());
  CHECK(committed->item == CareItem::Eye);
  CHECK(pending.lastOutcome() == PendingOutcome::Committed);
  // ★記録時刻は押した瞬間の壁時計であって、復帰後の時刻ではない。
  CHECK(committed->recordedAt.epochSeconds == 1700000000);
}
