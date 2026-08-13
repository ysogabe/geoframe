// tests/unit/test_alert_engine.cpp — 地震アラート（レーン 1A）
//
// 移植元: app/static/js/events.js:174-232
//
// ★ここで固定している上流の挙動:
//   - eid による重複排除。初回ロード分はすべて既知
//   - 震度 3 以上のみイベント化。maxi は "5-" のような文字列
//   - 群発対策のグローバルクールダウン 10 分（eid ごとではない）
//   - 他イベント表示中は発火しない
//   - ★発火しなかった地震も既知になる（rememberEq は判定の **前** に呼ばれる）
//
// ★時刻は必ず FakeClock / SteadyTime を通す。実時刻を使わない（決定性）。
// ★時間の長さはリテラルではなく定数を参照する
//   （kAlertHoldDuration などを変えたときテストが自動で追従するように）。

#include <doctest.h>

#include <cstddef>
#include <cstdint>
#include <string>
#include <utility>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/model/env.hpp"
#include "ambient/core/state/alert_engine.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state_store.hpp"

using ambient::core::AlertEngine;
using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::FakeClock;
using ambient::core::kAlertHoldDuration;
using ambient::core::MemoryStateStore;
using ambient::core::Millis;
using ambient::core::QuakeItem;
using ambient::core::SteadyTime;
using ambient::core::StoreCapacity;
using ambient::core::StoreStatus;
using ambient::core::TimeJump;
using ambient::core::TimeJumpDetector;
using ambient::core::WallTime;

namespace constants = ambient::core::constants;

namespace {

QuakeItem quake(std::string eid, std::string maxi) {
  QuakeItem q{};
  q.eid = std::move(eid);
  q.maxi = std::move(maxi);
  q.mag = "4.2";
  q.place = "岩手県沖";
  q.time = "13:51";
  // ★日跨ぎの同定に time は使えない（"HH:MM" で日付が落ちている）。at を持たせる。
  q.at = "2026-07-30T13:51:00+09:00";
  return q;
}

// 発火しているか（claim が出るか）を collect() 経由で見る。
bool claimsAlert(const AlertEngine& engine, SteadyTime now) {
  std::vector<Claim> out;
  engine.collect(now, out);
  return out.size() == 1 && out.front().priority == ClaimPriority::Alert;
}

// テスト用の潤沢な予算。Tab5 の実予算は移植時に実測する（未決定）。
StoreCapacity roomyBudget() {
  StoreCapacity cap{};
  cap.totalBytes = 8 * 1024;
  cap.maxValueBytes = 4 * 1024;
  cap.maxKeyBytes = constants::kNvsMaxKeyBytes;
  return cap;
}

}  // namespace

// ============================================================================
// 発火の 4 条件
// ============================================================================

TEST_CASE("震度 3 以上の新規地震で発火する") {
  AlertEngine engine;
  const std::vector<QuakeItem> quakes{quake("20260730135103", "4")};

  engine.ingest(quakes, SteadyTime{0}, /*firstLoad=*/false, /*otherEventActive=*/false);

  CHECK(engine.alerting(SteadyTime{0}));
  CHECK(claimsAlert(engine, SteadyTime{0}));
  CHECK(engine.activeQuake().eid == "20260730135103");
}

TEST_CASE("★初回ロード分はすべて既知として扱う（起動直後の一斉発火を防ぐ）") {
  // /api/env は過去 24 時間ぶんを返す。起動時にこれで一斉に鳴ってはいけない。
  AlertEngine engine;
  const std::vector<QuakeItem> quakes{
      quake("a", "5-"),
      quake("b", "6+"),
      quake("c", "4"),
  };

  engine.ingest(quakes, SteadyTime{0}, /*firstLoad=*/true, /*otherEventActive=*/false);

  CHECK_FALSE(engine.alerting(SteadyTime{0}));
  CHECK(engine.seenCount() == 3);

  // 2 回目の poll で同じものが来ても、既知なので鳴らない。
  engine.ingest(quakes, SteadyTime{1000}, /*firstLoad=*/false, /*otherEventActive=*/false);
  CHECK_FALSE(engine.alerting(SteadyTime{1000}));
  CHECK(engine.seenCount() == 3);
}

TEST_CASE("★eid で重複排除する（同じ地震が毎 poll 来ても 1 回だけ）") {
  AlertEngine engine;
  const std::vector<QuakeItem> quakes{quake("dup", "5+")};

  engine.ingest(quakes, SteadyTime{0}, false, false);
  REQUIRE(engine.alerting(SteadyTime{0}));

  std::vector<Claim> first;
  engine.collect(SteadyTime{0}, first);
  REQUIRE(first.size() == 1);
  CHECK(first.front().payloadId != 0);  // 0 は「区別なし」なので使わない

  // 表示が明けたあと、同じ eid が再び来ても発火しない。
  const SteadyTime later = SteadyTime{0} + kAlertHoldDuration + constants::kQuakeGlobalCooldown;
  engine.ingest(quakes, later, false, false);
  CHECK_FALSE(engine.alerting(later));
  CHECK(engine.seenCount() == 1);
}

TEST_CASE("★震度 3 未満は発火しない。ただし既知にはなる（events.js と同じ）") {
  AlertEngine engine;
  const std::vector<QuakeItem> weak{quake("weak", "2")};

  engine.ingest(weak, SteadyTime{0}, false, false);
  CHECK_FALSE(engine.alerting(SteadyTime{0}));

  // ★ここが重要。rememberEq は震度判定の **前** に呼ばれるので既知になる。
  //   「発火したものだけ既知にする」へ変えるとこの CHECK が落ちる。
  //   （変えると、クールダウン明けに古い弱震が掘り起こされて鳴る）
  CHECK(engine.seenCount() == 1);
  CHECK(engine.hasSeen("weak"));
}

TEST_CASE("★震度 \"5-\" は 5 として扱う（std::stoi では弱強が静かに消える）") {
  // maxi は気象庁の震度階級で "5-"（5 弱） "5+"（5 強）がありうる文字列。
  // parseIntPrefix() を通していることの確認。
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("j5minus", "5-")}, SteadyTime{0}, false, false);
  REQUIRE(engine.alerting(SteadyTime{0}));
  CHECK(engine.activeQuake().maxi == "5-");
}

TEST_CASE("maxi が数値として読めない場合は発火しない（既定で鳴らさない）") {
  AlertEngine engine;
  const std::vector<QuakeItem> quakes{
      quake("empty", ""),
      quake("junk", "不明"),
  };
  engine.ingest(quakes, SteadyTime{0}, false, false);
  CHECK_FALSE(engine.alerting(SteadyTime{0}));
  CHECK(engine.seenCount() == 2);
}

TEST_CASE("eid が空の item は捨てる（同定できないので既知にもできない）") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("", "6-")}, SteadyTime{0}, false, false);
  CHECK_FALSE(engine.alerting(SteadyTime{0}));
  CHECK(engine.seenCount() == 0);
}

TEST_CASE("他イベント表示中は発火しない") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("busy", "5+")}, SteadyTime{0}, false,
                /*otherEventActive=*/true);
  CHECK_FALSE(engine.alerting(SteadyTime{0}));
  // ★既知にはなる（events.js の rememberEq が判定前にあるため）。
  CHECK(engine.seenCount() == 1);
}

// ============================================================================
// グローバルクールダウン（群発地震対策）
// ============================================================================

TEST_CASE("★群発対策: グローバル 10 分のクールダウン") {
  AlertEngine engine;

  CHECK(engine.cooldownElapsed(SteadyTime{0}));  // 1 発目は抑止しない

  engine.ingest(std::vector<QuakeItem>{quake("swarm1", "5-")}, SteadyTime{0}, false, false);
  REQUIRE(engine.alerting(SteadyTime{0}));

  // 5 分後、群発の 2 発目。★eid が違っても発火しない（グローバルだから）。
  const SteadyTime fiveMin{5 * 60 * 1000};
  CHECK_FALSE(engine.cooldownElapsed(fiveMin));
  engine.ingest(std::vector<QuakeItem>{quake("swarm2", "6-")}, fiveMin, false, false);
  CHECK_FALSE(claimsAlert(engine, fiveMin));    // 表示時間も既に切れている
  CHECK(engine.activeQuake().eid == "swarm1");  // 2 発目に差し替わっていない

  // クールダウンが明けたら発火する。
  const SteadyTime after = SteadyTime{0} + constants::kQuakeGlobalCooldown;
  CHECK(engine.cooldownElapsed(after));
  engine.ingest(std::vector<QuakeItem>{quake("swarm3", "4")}, after, false, false);
  CHECK(engine.alerting(after));
  CHECK(engine.activeQuake().eid == "swarm3");
}

TEST_CASE("1 回の ingest に複数の新規地震があっても 1 件しか発火しない") {
  AlertEngine engine;
  const std::vector<QuakeItem> burst{
      quake("burst1", "5+"),
      quake("burst2", "6-"),
      quake("burst3", "4"),
  };
  engine.ingest(burst, SteadyTime{0}, false, false);

  CHECK(engine.activeQuake().eid == "burst1");  // 先頭が勝つ
  CHECK(engine.seenCount() == 3);               // 残りも既知になる

  std::vector<Claim> out;
  engine.collect(SteadyTime{0}, out);
  CHECK(out.size() == 1);
}

// ============================================================================
// エッジ→レベル変換（この型が存在する理由）
// ============================================================================

TEST_CASE("★エッジ→レベル: 発火後は表示時間のあいだ claim を出し続ける") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("level", "5-")}, SteadyTime{0}, false, false);

  // 表示時間中はずっとレベルとして出続ける（1 回出して消えるのではない）。
  CHECK(claimsAlert(engine, SteadyTime{0}));
  CHECK(claimsAlert(engine, SteadyTime{0} + kAlertHoldDuration - Millis{1}));
  // 満了したら出なくなる。
  CHECK_FALSE(claimsAlert(engine, SteadyTime{0} + kAlertHoldDuration));
}

TEST_CASE("claim の expiresAt は表示時間の満了時刻になっている") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("exp", "5-")}, SteadyTime{1000}, false, false);

  std::vector<Claim> out;
  engine.collect(SteadyTime{1000}, out);
  REQUIRE(out.size() == 1);
  REQUIRE(out.front().expiresAt.has_value());
  CHECK(*out.front().expiresAt == SteadyTime{1000} + kAlertHoldDuration);
  CHECK(out.front().sourceId == AlertEngine::kSourceId);
}

TEST_CASE("★アラート表示時間は上流の EQ_EVENT_MS（events.js:130）と一致する") {
  // 暫定値 20000 は同じ events.js の EVENT_MS（ADS-B 用）であって地震用ではなかった。
  CHECK(kAlertHoldDuration.value == 60000);
}

TEST_CASE("★別の地震に切り替わると payloadId が変わる（画面を更新させるため）") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("A", "5-")}, SteadyTime{0}, false, false);
  std::vector<Claim> first;
  engine.collect(SteadyTime{0}, first);
  REQUIRE(first.size() == 1);

  const SteadyTime later = SteadyTime{0} + constants::kQuakeGlobalCooldown;
  engine.ingest(std::vector<QuakeItem>{quake("B", "6-")}, later, false, false);
  std::vector<Claim> second;
  engine.collect(later, second);
  REQUIRE(second.size() == 1);

  CHECK(first.front().kind == second.front().kind);  // どちらも Alert
  CHECK(first.front().payloadId != second.front().payloadId);
}

TEST_CASE("既知地震 ID は上限 500 で頭打ちになる（長期稼働で増え続けない）") {
  AlertEngine engine;
  std::vector<QuakeItem> many;
  many.reserve(constants::kSeenQuakeMax + 50);
  for (std::size_t i = 0; i < constants::kSeenQuakeMax + 50; ++i) {
    many.push_back(quake("eid-" + std::to_string(i), "1"));
  }
  engine.ingest(many, SteadyTime{0}, /*firstLoad=*/true, false);

  CHECK(engine.seenCount() == constants::kSeenQuakeMax);
  CHECK_FALSE(engine.hasSeen("eid-0"));  // 最古が捨てられている
  CHECK(engine.hasSeen("eid-549"));      // 最新は残っている
}

// ============================================================================
// 時間跳躍（スリープ復帰）
// ============================================================================

TEST_CASE("★スリープ復帰: 実時間で 10 分経っていればクールダウンは明ける") {
  // ############################################################################
  // # 決めたポリシー: skew > 0 なら「その時間は実際に経過した」として
  // #   基準点を巻き戻す。明けない側に倒れると、復帰後に本物の地震が来ても
  // #   抑止される（= 観測できない失敗）ので、明ける側に倒す。
  // ############################################################################
  FakeClock clock{SteadyTime{0}, WallTime{1700000000}};
  TimeJumpDetector detector;
  AlertEngine engine;
  (void)detector.update(clock.steadyNow(), clock.wallNow());

  engine.ingest(std::vector<QuakeItem>{quake("before-sleep", "5-")}, clock.steadyNow(), false,
                false);
  REQUIRE(engine.alerting(clock.steadyNow()));

  // 8 時間スリープ。単調時計は止まっているので steadyNow() は動かない。
  clock.simulateSleep(Millis{8 * 60 * 60 * 1000});
  const auto jump = detector.update(clock.steadyNow(), clock.wallNow());
  REQUIRE(jump.has_value());

  // 補正前は「まだ 10 分経っていない」ように見えてしまう。
  CHECK_FALSE(engine.cooldownElapsed(clock.steadyNow()));

  engine.onTimeJump(*jump);

  // 補正後: クールダウンは明け、8 時間前のアラートも画面から消える。
  CHECK(engine.cooldownElapsed(clock.steadyNow()));
  CHECK_FALSE(engine.alerting(clock.steadyNow()));
  CHECK_FALSE(claimsAlert(engine, clock.steadyNow()));

  // 復帰後に来た新しい地震はちゃんと鳴る。
  engine.ingest(std::vector<QuakeItem>{quake("after-sleep", "5+")}, clock.steadyNow(), false,
                false);
  CHECK(engine.alerting(clock.steadyNow()));
}

TEST_CASE("★壁時計の巻き戻しではクールダウンを延ばさない（アラートを握り潰さない）") {
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("q", "5-")}, SteadyTime{0}, false, false);

  // skew < 0（NTP 補正で壁時計が 1 時間巻き戻った）。
  TimeJump backwards{};
  backwards.steadyDelta = Millis{1000};
  backwards.wallDelta = Millis{-3600 * 1000};
  backwards.skew = backwards.wallDelta - backwards.steadyDelta;
  REQUIRE(backwards.skew.value < 0);

  engine.onTimeJump(backwards);

  // ★no-op であること。ここで基準点を未来へずらすとクールダウンが延び、
  //   本物の地震アラートが黙って消える。
  const SteadyTime justAfter = SteadyTime{0} + constants::kQuakeGlobalCooldown;
  CHECK(engine.cooldownElapsed(justAfter));
  CHECK(engine.alerting(SteadyTime{1000}));  // 表示も途中で切られていない
}

// ============================================================================
// 永続化（★上流の Web 版に対する改善点）
// ============================================================================

TEST_CASE("★既知地震 ID が再起動をまたいで復元される") {
  MemoryStateStore store{roomyBudget()};

  {
    AlertEngine engine;
    engine.ingest(std::vector<QuakeItem>{quake("p1", "1"), quake("p2", "5-")}, SteadyTime{0},
                  /*firstLoad=*/true, false);
    CHECK(engine.save(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  }

  AlertEngine restored;
  CHECK(restored.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  CHECK(restored.seenCount() == 2);
  CHECK(restored.hasSeen("p1"));
  CHECK(restored.hasSeen("p2"));
}

TEST_CASE("★再起動でクールダウンが失効しない（群発の 2 発目が即座に鳴らない）") {
  // ############################################################################
  // # 永続化が本当に効くのはここ。
  // #   seen だけなら firstLoad がほぼ肩代わりするが、everFired_ が false に
  // #   戻ると cooldownElapsed() が true になり、10 分クールダウンが
  // #   再起動のたびに無効化される。しかも画面に出ないので気づけない。
  // ############################################################################
  MemoryStateStore store{roomyBudget()};

  {
    AlertEngine engine;
    engine.ingest(std::vector<QuakeItem>{quake("boom", "6-")}, SteadyTime{0}, false, false);
    REQUIRE(engine.alerting(SteadyTime{0}));
    // 発火から 30 秒後にアプリが落ちた。
    CHECK(engine.save(store, SteadyTime{30000}, WallTime{1700000030}) == StoreStatus::Ok);
  }

  // 再起動。★単調時刻の起点は変わっている（SteadyTime は再起動をまたげない）。
  AlertEngine restarted;
  CHECK(restarted.restore(store, SteadyTime{0}, WallTime{1700000031}) == StoreStatus::Ok);

  // 発火から 31 秒しか経っていないので、まだクールダウン中でなければならない。
  CHECK_FALSE(restarted.cooldownElapsed(SteadyTime{0}));

  // 群発の 2 発目は鳴らない。
  restarted.ingest(std::vector<QuakeItem>{quake("boom2", "5+")}, SteadyTime{0}, false, false);
  CHECK_FALSE(restarted.alerting(SteadyTime{0}));

  // 10 分経てば明ける。
  CHECK(restarted.cooldownElapsed(SteadyTime{0} + constants::kQuakeGlobalCooldown));
}

TEST_CASE("★電源が落ちていた時間もクールダウンの経過として数える") {
  MemoryStateStore store{roomyBudget()};
  {
    AlertEngine engine;
    engine.ingest(std::vector<QuakeItem>{quake("old", "5-")}, SteadyTime{0}, false, false);
    CHECK(engine.save(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  }

  // 1 時間後に起動した。実時間で 10 分をとうに超えているので明けている。
  AlertEngine restarted;
  CHECK(restarted.restore(store, SteadyTime{0}, WallTime{1700000000 + 3600}) == StoreStatus::Ok);
  CHECK(restarted.cooldownElapsed(SteadyTime{0}));
}

TEST_CASE("★保存するのは直近 64 件だけ（Tab5 の NVS は 20KB しかない）") {
  MemoryStateStore store{roomyBudget()};
  {
    AlertEngine engine;
    std::vector<QuakeItem> many;
    for (std::size_t i = 0; i < 200; ++i) {
      many.push_back(quake("k" + std::to_string(i), "1"));
    }
    engine.ingest(many, SteadyTime{0}, /*firstLoad=*/true, false);
    REQUIRE(engine.seenCount() == 200);
    CHECK(engine.save(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  }

  AlertEngine restored;
  CHECK(restored.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  CHECK(restored.seenCount() == ambient::core::kSeenQuakePersistMax);
  CHECK(restored.hasSeen("k199"));      // 直近が残る
  CHECK_FALSE(restored.hasSeen("k0"));  // 古いものは落ちる

  // 予算の見積もり: 1KB 台に収まっていること（20KB の NVS を食い潰さない）。
  CHECK(store.capacity().usedBytes < 1500);
}

TEST_CASE("★領域が足りなければ件数を減らして書く（OutOfSpace を握り潰さない）") {
  StoreCapacity tight{};
  tight.totalBytes = 200;
  tight.maxValueBytes = 200;
  tight.maxKeyBytes = constants::kNvsMaxKeyBytes;
  MemoryStateStore store{tight};

  AlertEngine engine;
  std::vector<QuakeItem> many;
  for (std::size_t i = 0; i < 64; ++i) {
    many.push_back(quake("eid-" + std::to_string(i), "1"));
  }
  engine.ingest(many, SteadyTime{0}, /*firstLoad=*/true, false);

  // 全件は入らないが、切り詰めて Ok になる。
  CHECK(engine.save(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);

  AlertEngine restored;
  CHECK(restored.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  CHECK(restored.seenCount() > 0);
  CHECK(restored.seenCount() < 64);
  CHECK(restored.hasSeen("eid-63"));  // 残ったのは直近側
}

TEST_CASE("初回起動は NotFound（異常ではない。状態を壊さない）") {
  const MemoryStateStore store{roomyBudget()};
  AlertEngine engine;
  engine.ingest(std::vector<QuakeItem>{quake("live", "5-")}, SteadyTime{0}, false, false);

  CHECK(engine.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::NotFound);
  CHECK(engine.seenCount() == 1);  // 既存の状態は変更されない
}

TEST_CASE("読めない形式は捨てて起動を止めない") {
  MemoryStateStore store{roomyBudget()};
  CHECK(store.write(ambient::core::kSeenQuakeStoreKey, "99\ngarbage\n") == StoreStatus::Ok);

  AlertEngine engine;
  CHECK(engine.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::BackendError);
  CHECK(engine.seenCount() == 0);
}

TEST_CASE("壊れたクールダウン基準は「明けている」側に倒す") {
  // ★「明けていない」に倒すと、本物の地震アラートを黙って握り潰す。
  MemoryStateStore store{roomyBudget()};
  CHECK(store.write(ambient::core::kSeenQuakeStoreKey, "1\nNOT-A-NUMBER\nx1\n") ==
        StoreStatus::Ok);

  AlertEngine engine;
  CHECK(engine.restore(store, SteadyTime{0}, WallTime{1700000000}) == StoreStatus::Ok);
  CHECK(engine.hasSeen("x1"));
  CHECK(engine.cooldownElapsed(SteadyTime{0}));
}

TEST_CASE("保存キーは NVS のキー長上限（15 文字）に収まる") {
  CHECK(std::string{ambient::core::kSeenQuakeStoreKey}.size() <= constants::kNvsMaxKeyBytes);
}
