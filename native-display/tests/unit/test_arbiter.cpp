// tests/unit/test_arbiter.cpp — 優先度アービターと状態機械（レーン 1A）
//
// ★次フェーズでここに書くもの:
//   - 7 段の優先順位を網羅する表駆動テスト
//   - ★レベルトリガであることの検証:
//     「Alert 中に Red になり、Alert が明けたら CareDue が出る」を
//     **再配達の仕組み無しで** 通ること。これがイベントキューを避けた理由そのもの。
//   - AlertEngine の群発クールダウン（グローバル 10 分）
//   - 各 IClaimSource の実装

#include <doctest.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>
#include <vector>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"
#include "ambient/core/env/environment_model.hpp"
#include "ambient/core/model/roomenv.hpp"
#include "ambient/core/rng.hpp"
#include "ambient/core/state/alert_engine.hpp"
#include "ambient/core/state/arbiter.hpp"
#include "ambient/core/state/claim.hpp"
#include "ambient/core/state/state_machine.hpp"

using ambient::core::Claim;
using ambient::core::ClaimPriority;
using ambient::core::decide;
using ambient::core::Decision;
using ambient::core::DisplayStateKind;
using ambient::core::IClaimSource;
using ambient::core::Millis;
using ambient::core::StateMachine;
using ambient::core::SteadyTime;

namespace {

Claim makeClaim(ClaimPriority priority, DisplayStateKind kind, std::uint32_t sourceId = 0,
                std::uint32_t payloadId = 0) {
  Claim c{};
  c.priority = priority;
  c.kind = kind;
  c.sourceId = sourceId;
  c.payloadId = payloadId;
  return c;
}

}  // namespace

TEST_CASE("claim が無ければ nullopt（既定値を返して異常を隠さない）") {
  const std::vector<Claim> none;
  CHECK_FALSE(decide(none, SteadyTime{0}).has_value());
}

TEST_CASE("最も優先度の高い claim が勝つ") {
  const std::vector<Claim> claims{
      makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo),
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert),
      makeClaim(ClaimPriority::Care, DisplayStateKind::Care),
  };
  const auto d = decide(claims, SteadyTime{0});
  REQUIRE(d.has_value());
  CHECK(d->kind == DisplayStateKind::Alert);
}

TEST_CASE("7 段の順序") {
  static_assert(ClaimPriority::Photo < ClaimPriority::Env);
  static_assert(ClaimPriority::Env < ClaimPriority::Degraded);
  static_assert(ClaimPriority::Degraded < ClaimPriority::Care);
  static_assert(ClaimPriority::Care < ClaimPriority::CareDue);
  static_assert(ClaimPriority::CareDue < ClaimPriority::Alert);

  // ############################################################################
  // # ★この 1 行を「Alert が最強の方が自然だ」で戻さないこと。
  // #
  // #   出典 1: 企画書 §2.3「表示優先順位」
  // #             rank 2 = 記録操作と取り消し猶予 / rank 3 = 緊急地震通知
  // #   出典 2: 企画書 §2.1 ALERT
  // #             「通常通知は操作中に割り込ませず、
  // #               **重要通知のみ操作完了後に表示する**」= 地震も操作完了を待つ
  // #   出典 3: この設計の要件そのもの
  // #             「記録保留中は地震で遷移させない、保留が明けたら地震を出す」
  // #
  // #   ユーザが今まさに押している操作を地震が横取りすると、押した記録が
  // #   消えたように見える。地震アラートは AlertEngine がレベルとして出し続けるので、
  // #   保留が明けた tick で自動的に前へ出る（抑止ロジックは書かない）。
  // #   下の「★操作中の保留」テストがこの挙動そのものを固定している。
  // ############################################################################
  static_assert(ClaimPriority::Alert < ClaimPriority::RecordedAwaitingServer);

  // Degraded > Env: データが陳腐化しているのに環境情報を出すのは害
  const std::vector<Claim> claims{
      makeClaim(ClaimPriority::Env, DisplayStateKind::Env),
      makeClaim(ClaimPriority::Degraded, DisplayStateKind::Degraded),
  };
  const auto d = decide(claims, SteadyTime{0});
  REQUIRE(d.has_value());
  CHECK(d->kind == DisplayStateKind::Degraded);

  // ★Degraded は最強ではない（企画書 rank 1「初期設定・OTA・致命的エラー」は
  //   この 7 段に対応する状態が無い）。データが古いだけで地震を抑止しないこと。
  const std::vector<Claim> staleDuringQuake{
      makeClaim(ClaimPriority::Degraded, DisplayStateKind::Degraded),
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert),
  };
  CHECK(decide(staleDuringQuake, SteadyTime{0})->kind == DisplayStateKind::Alert);
}

TEST_CASE("★7 段すべてが、それだけが最上位のとき選ばれる（全段の網羅）") {
  // 弱い順に並べた表。i 段目までを claim として渡すと、必ず i 段目が勝つ。
  struct Row {
    ClaimPriority priority;
    DisplayStateKind kind;
  };
  constexpr std::array<Row, ambient::core::kClaimPriorityCount> kLadder{{
      {ClaimPriority::Photo, DisplayStateKind::Photo},
      {ClaimPriority::Env, DisplayStateKind::Env},
      {ClaimPriority::Degraded, DisplayStateKind::Degraded},
      {ClaimPriority::Care, DisplayStateKind::Care},
      {ClaimPriority::CareDue, DisplayStateKind::CareDue},
      {ClaimPriority::Alert, DisplayStateKind::Alert},
      {ClaimPriority::RecordedAwaitingServer, DisplayStateKind::RecordedAwaitingServer},
  }};

  // ★ループの中で CHECK を撃たない（assertion が積み上がる）。集計して 1 回だけ。
  std::size_t mismatches = 0;
  for (std::size_t top = 0; top < kLadder.size(); ++top) {
    std::vector<Claim> claims;
    for (std::size_t i = 0; i <= top; ++i) {
      claims.push_back(makeClaim(kLadder[i].priority, kLadder[i].kind));
    }
    const auto d = decide(claims, SteadyTime{0});
    if (!d.has_value() || d->kind != kLadder[top].kind) {
      ++mismatches;
    }
  }
  CHECK(mismatches == 0);

  // 表が 7 段の enum と 1 対 1 であること（段を足したら表も足す）。
  static_assert(kLadder.size() == ambient::core::kClaimPriorityCount);
}

TEST_CASE("★操作中の保留: 記録の取り消し猶予中は地震でも遷移しない") {
  // ############################################################################
  // # ここが ClaimPriority の並びを直した理由そのものである。
  // #
  // #   「記録保留中は地震で遷移させない、保留が明けたら地震を出す」を、
  // #   **抑止ロジックを一切書かずに** 成立させる。
  // #   レベルトリガなので、地震の claim は保留中もずっと出続けている。
  // #   保留が消えた tick で自動的に地震が選ばれる。再配達キューは無い。
  // ############################################################################
  const Claim photo = makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo);
  const Claim quake =
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert, /*sourceId=*/9, /*payloadId=*/7);

  // 取り消し猶予（2 秒）のあいだ出続ける claim。expiresAt で寿命を表す。
  Claim recording = makeClaim(ClaimPriority::RecordedAwaitingServer,
                              DisplayStateKind::RecordedAwaitingServer, /*sourceId=*/5);
  recording.expiresAt = SteadyTime{2000};

  const std::vector<Claim> claims{photo, quake, recording};

  // 猶予中: 地震が来ていても記録操作が勝つ。
  CHECK(decide(claims, SteadyTime{0})->kind == DisplayStateKind::RecordedAwaitingServer);
  CHECK(decide(claims, SteadyTime{1999})->kind == DisplayStateKind::RecordedAwaitingServer);

  // 猶予が明けた瞬間に地震が出る。★抑止解除のコードはどこにも無い。
  const auto after = decide(claims, SteadyTime{2000});
  REQUIRE(after.has_value());
  CHECK(after->kind == DisplayStateKind::Alert);
  CHECK(after->payloadId == 7);
}

TEST_CASE("Degraded の分岐: 通信が生きていれば Env、切れていれば Degraded") {
  const Claim photo = makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo);
  const Claim env = makeClaim(ClaimPriority::Env, DisplayStateKind::Env);
  const Claim degraded = makeClaim(ClaimPriority::Degraded, DisplayStateKind::Degraded);

  // 通信が生きている = Degraded の source が claim を出さない。
  const std::vector<Claim> healthy{photo, env};
  CHECK(decide(healthy, SteadyTime{0})->kind == DisplayStateKind::Env);

  // 通信断 = Degraded が claim を出す。「古い天気」より「取れていない」。
  const std::vector<Claim> broken{photo, env, degraded};
  CHECK(decide(broken, SteadyTime{0})->kind == DisplayStateKind::Degraded);

  // 復旧すると、レベルなので何もせず Env へ戻る。
  CHECK(decide(healthy, SteadyTime{1000})->kind == DisplayStateKind::Env);

  // ★Degraded 中でも写真より上、Care より下。
  const std::vector<Claim> withCare{photo, degraded,
                                    makeClaim(ClaimPriority::Care, DisplayStateKind::Care)};
  CHECK(decide(withCare, SteadyTime{0})->kind == DisplayStateKind::Care);
}

TEST_CASE("★同一優先度は span 内で先に現れたものが勝つ（安定選択の契約）") {
  const std::vector<Claim> claims{
      makeClaim(ClaimPriority::Care, DisplayStateKind::Care, /*sourceId=*/1),
      makeClaim(ClaimPriority::Care, DisplayStateKind::Care, /*sourceId=*/2),
      makeClaim(ClaimPriority::Care, DisplayStateKind::Care, /*sourceId=*/3),
  };
  const auto d = decide(claims, SteadyTime{0});
  REQUIRE(d.has_value());
  CHECK(d->sourceId == 1);
}

TEST_CASE("notBefore 前の claim は選ばれない（クールダウン）") {
  Claim cooling = makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert);
  cooling.notBefore = SteadyTime{5000};
  const std::vector<Claim> claims{cooling,
                                  makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};

  const auto before = decide(claims, SteadyTime{4999});
  REQUIRE(before.has_value());
  CHECK(before->kind == DisplayStateKind::Photo);

  const auto after = decide(claims, SteadyTime{5000});
  REQUIRE(after.has_value());
  CHECK(after->kind == DisplayStateKind::Alert);
}

TEST_CASE("expiresAt 以降の claim は選ばれない（レベルの寿命）") {
  Claim expiring = makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert);
  expiring.expiresAt = SteadyTime{3000};
  const std::vector<Claim> claims{expiring,
                                  makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};

  const auto during = decide(claims, SteadyTime{2999});
  REQUIRE(during.has_value());
  CHECK(during->kind == DisplayStateKind::Alert);

  const auto after = decide(claims, SteadyTime{3000});
  REQUIRE(after.has_value());
  CHECK(after->kind == DisplayStateKind::Photo);
}

TEST_CASE("★レベルトリガ: 抑止が解けたら再配達の仕組み無しで復帰する") {
  // Alert 表示中に CareDue が真になった。イベントキューなら
  // 「Alert に抑止された CareDue イベント」を後で再配達する仕組みが要る。
  // レベルなら CareDue の claim が出続けているだけなので、
  // Alert が消えた tick で自動的に選ばれる。
  Claim alert = makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert);
  alert.expiresAt = SteadyTime{20000};
  const Claim careDue = makeClaim(ClaimPriority::CareDue, DisplayStateKind::CareDue);
  const Claim photo = makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo);

  const std::vector<Claim> claims{photo, careDue, alert};

  CHECK(decide(claims, SteadyTime{1000})->kind == DisplayStateKind::Alert);
  CHECK(decide(claims, SteadyTime{19999})->kind == DisplayStateKind::Alert);
  // Alert の寿命が切れた瞬間に CareDue へ。再配達キューは登場しない。
  CHECK(decide(claims, SteadyTime{20000})->kind == DisplayStateKind::CareDue);
}

TEST_CASE("decide は純粋（何度呼んでも同じ答え）") {
  const std::vector<Claim> claims{makeClaim(ClaimPriority::Env, DisplayStateKind::Env)};
  const auto a = decide(claims, SteadyTime{1234});
  const auto b = decide(claims, SteadyTime{1234});
  CHECK(a == b);
}

TEST_CASE("StateMachine: 変化したときだけ遷移を返す") {
  StateMachine machine;
  const std::vector<Claim> photo{makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};

  const auto first = machine.update(photo, SteadyTime{0});
  REQUIRE(first.has_value());
  CHECK(first->to == DisplayStateKind::Photo);
  CHECK(machine.current() == DisplayStateKind::Photo);

  CHECK_FALSE(machine.update(photo, SteadyTime{1000}).has_value());
}

TEST_CASE("★StateMachine: kind が同じでも payloadId が変われば遷移する") {
  // 地震 A を表示中に地震 B が来た。kind は Alert のままだが内容は別物。
  StateMachine machine;
  const std::vector<Claim> quakeA{
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert, 1, /*payloadId=*/100)};
  const std::vector<Claim> quakeB{
      makeClaim(ClaimPriority::Alert, DisplayStateKind::Alert, 1, /*payloadId=*/200)};

  (void)machine.update(quakeA, SteadyTime{0});
  const auto t = machine.update(quakeB, SteadyTime{1000});
  REQUIRE(t.has_value());
  CHECK(t->from == DisplayStateKind::Alert);
  CHECK(t->to == DisplayStateKind::Alert);
  CHECK(t->fromPayloadId == 100);
  CHECK(t->toPayloadId == 200);
}

TEST_CASE("StateMachine: claim が無ければ fallback へ落ちる") {
  StateMachine machine{DisplayStateKind::Degraded};
  const std::vector<Claim> photo{makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};
  (void)machine.update(photo, SteadyTime{0});
  CHECK(machine.current() == DisplayStateKind::Photo);

  const std::vector<Claim> none;
  const auto t = machine.update(none, SteadyTime{1000});
  REQUIRE(t.has_value());
  CHECK(t->to == DisplayStateKind::Degraded);
}

TEST_CASE("StateMachine: 履歴は上限付き（Tab5 の RAM 予算）") {
  StateMachine machine;
  const std::vector<Claim> a{makeClaim(ClaimPriority::Photo, DisplayStateKind::Photo)};
  const std::vector<Claim> b{makeClaim(ClaimPriority::Env, DisplayStateKind::Env)};

  for (int i = 0; i < 100; ++i) {
    (void)machine.update((i % 2 == 0) ? a : b, SteadyTime{i});
  }
  CHECK(machine.history().size() <= StateMachine::kHistoryMax);
}

TEST_CASE("AlertEngine: 一度も発火していなければクールダウンは明けている") {
  const ambient::core::AlertEngine engine;
  CHECK(engine.cooldownElapsed(SteadyTime{0}));
  CHECK(engine.seenCount() == 0);
  CHECK_FALSE(engine.alerting(SteadyTime{0}));
}

TEST_CASE("AlertEngine: 発火していなければ claim を出さない") {
  const ambient::core::AlertEngine engine;
  std::vector<Claim> out;
  engine.collect(SteadyTime{0}, out);
  CHECK(out.empty());
}

// ============================================================================
// ★3 イベント源同時のランダム soak（Wave 3B）
// ============================================================================
//
// 目的: 写真・地震・環境（＋お世話・記録）が **同時に** claim を出す状況を
//       長時間ランダムに回し、優先度の違反が 1 件も起きないことを示す。
//
// ★決定的であること:
//   - 実時間ではなく FakeClock（100ms 刻みの tick）で進める
//   - 乱数は seed 固定の core::Rng（テスト専用の phantom domain）
//     ★標準の分布クラスは使わない。規格が実装を規定しておらず、
//       Mac と Tab5 で列が食い違う（docs/CORE_CONTRACTS.md §2.1）
//   - 実 HTTP も実時計も使わない
//
// ★「違反」の定義（3 つ独立に数える。1 つにまとめない）:
//   (a) 選ばれたものより **高い優先度の claim が有効なのに選ばれなかった**
//   (b) 選ばれた claim が activeAt(now) でない（期限切れ / notBefore 前）
//   (c) 同一優先度の中で **span 内の先頭でないもの** が選ばれた
//       （arbiter.hpp §3 の安定選択の契約）
//   加えて StateMachine が decide() と別の状態に落ち着いていないことも見る。
//
// ★網羅していないと 0 違反は無意味なので、被覆カウンタを取って
//   「その状況を実際に踏んだ」ことを最後に検査する。
//   特に **Alert と RecordedAwaitingServer が同時に有効な tick**。
//   Alert は最大値ではない（RecordedAwaitingServer が上）ので、
//   ここを踏まない soak はこの設計の要を一度も試していない。

namespace {

// ★テスト専用の phantom domain。既存の Scheduler / Transition と
//   共有しないための 1 行（rng.hpp の「用途ごとに別の型」の作法どおり）。
struct SoakDomain {};
using SoakRng = ambient::core::Rng<SoakDomain>;

// 常に claim を出す source（写真）。★これが無いと decide() が nullopt になる。
class PhotoSoakSource final : public IClaimSource {
 public:
  static constexpr std::uint32_t kSourceId = 0x50'48'4F'54;  // 'PHOT'

  void collect(SteadyTime /*now*/, std::vector<Claim>& out) const override {
    Claim claim;
    claim.priority = ClaimPriority::Photo;
    claim.kind = DisplayStateKind::Photo;
    claim.sourceId = kSourceId;
    claim.payloadId = payloadId_;
    out.push_back(claim);
  }

  void nextPhoto() noexcept { ++payloadId_; }

 private:
  std::uint32_t payloadId_{1};
};

// 一定時間だけ claim を出す source（地震 / お世話期限 / 記録の取り消し猶予）。
//
// ★collect() は const で now の純粋な関数。「1 回出して消える」実装にしない
//   （claim.hpp の契約。抑止中に claim が消えると再配達問題が戻ってくる）。
class BurstSoakSource final : public IClaimSource {
 public:
  BurstSoakSource(ClaimPriority priority, DisplayStateKind kind, std::uint32_t sourceId)
      : priority_(priority), kind_(kind), sourceId_(sourceId) {}

  // 入力（エッジ）。★collect() ではなくこちらで状態を変える。
  void fire(SteadyTime now, Millis hold) noexcept {
    end_ = now + hold;
    ++payloadId_;
  }

  void collect(SteadyTime now, std::vector<Claim>& out) const override {
    if (!activeAt(now)) {
      return;
    }
    Claim claim;
    claim.priority = priority_;
    claim.kind = kind_;
    claim.sourceId = sourceId_;
    claim.payloadId = payloadId_;
    claim.expiresAt = end_;
    out.push_back(claim);
  }

  [[nodiscard]] bool activeAt(SteadyTime now) const noexcept {
    return payloadId_ != 0 && now < end_;
  }

 private:
  ClaimPriority priority_;
  DisplayStateKind kind_;
  std::uint32_t sourceId_;
  SteadyTime end_{};
  std::uint32_t payloadId_{0};
};

// decide() とは独立に書いた参照実装。
// ★decide() の実装を写さないこと（同じ間違いを 2 回書くと検出力が消える）。
//   仕様を素直に書く: 有効なものだけ見て、優先度が最大のものを取り、
//   同点は先に現れたものを残す（strict > なので自然に先勝ちになる）。
[[nodiscard]] std::optional<Decision> referenceWinner(const std::vector<Claim>& claims,
                                                      SteadyTime now) {
  const Claim* best = nullptr;
  for (const Claim& claim : claims) {
    if (!claim.activeAt(now)) {
      continue;
    }
    if (best == nullptr ||
        static_cast<int>(claim.priority) > static_cast<int>(best->priority)) {
      best = &claim;
    }
  }
  if (best == nullptr) {
    return std::nullopt;
  }
  return Decision{best->kind, best->priority, best->sourceId, best->payloadId};
}

ambient::core::RoomEnvSnapshot soakRoomEnv(bool configured, int co2Ppm) {
  ambient::core::RoomEnvSnapshot snapshot;
  if (!configured) {
    snapshot.availability = ambient::core::RoomEnvAvailability::NotConfigured;
    return snapshot;
  }
  snapshot.availability = ambient::core::RoomEnvAvailability::Available;
  ambient::core::RoomEnvItem co2;
  co2.entity = "sensor.co2";
  co2.value = ambient::core::formatScaled(co2Ppm, 0);
  co2.unit = "ppm";
  co2.entityClass = "carbon_dioxide";
  co2.name = "CO2";
  snapshot.items.push_back(co2);
  return snapshot;
}

}  // namespace

TEST_CASE("★soak: 3 イベント源同時のランダム列で優先度違反が 1 件も起きない") {
  // 100ms 刻みで 50000 tick = 実時間 5000 秒ぶん。ctest 内で数百 ms で終わる。
  constexpr int kTicks = 50000;
  constexpr std::int64_t kTickMs = 100;

  SoakRng rng{0xC0FFEEULL};
  ambient::core::FakeClock clock{SteadyTime{0}, ambient::core::WallTime{1785389160}};

  PhotoSoakSource photo;
  ambient::core::EnvironmentModel env;
  {
    // ★カットを短くする。既定（10 分間隔）だと 5000 秒で 8 回しか出ず、
    //   「同時に有効」な状況をほとんど踏まない。
    ambient::core::EnvCutSchedule schedule;
    schedule.interval = Millis{60000};
    schedule.duration = Millis{20000};
    env.setCutSchedule(schedule);
  }
  BurstSoakSource quake{ClaimPriority::Alert, DisplayStateKind::Alert, 0x51'55'41'4B};
  BurstSoakSource careDueA{ClaimPriority::CareDue, DisplayStateKind::CareDue, 0xCA'11'00'01};
  BurstSoakSource careDueB{ClaimPriority::CareDue, DisplayStateKind::CareDue, 0xCA'11'00'02};
  BurstSoakSource recording{ClaimPriority::RecordedAwaitingServer,
                            DisplayStateKind::RecordedAwaitingServer, 0x52'45'43'21};

  // ★登録順は固定する。同一優先度は「span 内で先に現れたもの」が勝つ契約なので、
  //   順序が揺れると結果が揺れる（arbiter.hpp §3）。
  const std::array<IClaimSource*, 6> sources{{&photo, &env, &careDueA, &careDueB, &quake,
                                              &recording}};

  StateMachine machine;
  std::vector<Claim> claims;

  // 違反カウンタ（3 種 + 状態機械）
  std::size_t violationsHigherActive = 0;
  std::size_t violationsWinnerInactive = 0;
  std::size_t violationsNotFirst = 0;
  std::size_t violationsReference = 0;
  std::size_t violationsMachine = 0;
  std::size_t violationsNullopt = 0;

  // 被覆カウンタ（★これが 0 のままだと「0 違反」に意味が無い）
  std::size_t coverAllThree = 0;      // 写真 + 環境 + 地震 が同時に有効
  std::size_t coverAlertVsRecord = 0; // 地震 + 記録保留 が同時に有効（Alert は最大ではない）
  std::size_t coverTie = 0;           // 同一優先度が 2 つ以上有効（安定選択の検査）
  std::array<std::size_t, ambient::core::kClaimPriorityCount> wins{};

  for (int tick = 0; tick < kTicks; ++tick) {
    const SteadyTime now = clock.steadyNow();

    // ---- ランダムなイベント列（引く回数は tick ごとに固定） ----------------
    const std::uint32_t rollPhoto = rng.belowN(80);
    const std::uint32_t rollQuake = rng.belowN(2000);
    const std::uint32_t rollCareA = rng.belowN(600);
    const std::uint32_t rollCareB = rng.belowN(700);
    const std::uint32_t rollRecord = rng.belowN(250);
    const std::uint32_t rollRoom = rng.belowN(300);
    const std::uint32_t rollEnvApi = rng.belowN(500);
    const std::uint32_t co2 = 400 + rng.belowN(1400);
    const std::uint32_t careHold = 5000 + rng.belowN(35000);
    const std::uint32_t roomConfigured = rng.belowN(20);

    if (rollPhoto == 0) {
      photo.nextPhoto();
    }
    if (rollQuake == 0) {
      // 地震の表示時間は実際の値を使う（60 秒）。
      quake.fire(now, ambient::core::kAlertHoldDuration);
    }
    if (rollCareA == 0) {
      careDueA.fire(now, Millis{static_cast<std::int64_t>(careHold)});
    }
    if (rollCareB == 0) {
      careDueB.fire(now, Millis{static_cast<std::int64_t>(careHold)});
    }
    if (rollRecord == 0) {
      // 取り消し猶予は 2 秒（constants::kToastTotal）。
      recording.fire(now, ambient::core::constants::kToastTotal);
    }
    if (rollRoom == 0) {
      // 20 回に 1 回は HA 未設定（ok:false）。★capability-gate であって障害ではない。
      env.ingestRoomEnv(soakRoomEnv(roomConfigured != 0, static_cast<int>(co2)), now,
                        clock.wallNow());
    }
    if (rollEnvApi == 0) {
      ambient::core::EnvSnapshot snapshot;
      ambient::core::WeatherSummary weather;
      weather.area = "soak";
      weather.text = "晴れ";
      snapshot.weather = weather;
      env.ingestEnv(snapshot, now, clock.wallNow());
    }

    // ---- 収集して決める --------------------------------------------------
    ambient::core::collectClaims(sources, now, claims);
    const std::optional<Decision> decision = decide(claims, now);

    // 写真の source が常に claim を出すので nullopt にはならない。
    if (!decision.has_value()) {
      ++violationsNullopt;
      clock.advance(Millis{kTickMs});
      continue;
    }

    // (a) より高い優先度の claim が有効なのに選ばれていない
    // (b) 選ばれた claim が有効でない
    // (c) 同一優先度の先頭でないものが選ばれた
    bool sawWinner = false;
    bool winnerIsFirstOfPriority = true;
    for (const Claim& claim : claims) {
      if (!claim.activeAt(now)) {
        continue;
      }
      if (static_cast<int>(claim.priority) > static_cast<int>(decision->priority)) {
        ++violationsHigherActive;
        break;
      }
      if (claim.priority == decision->priority && !sawWinner) {
        // 同一優先度で最初に現れた有効な claim が勝者であること。
        if (claim.sourceId != decision->sourceId || claim.payloadId != decision->payloadId ||
            claim.kind != decision->kind) {
          winnerIsFirstOfPriority = false;
        }
        sawWinner = true;
      }
    }
    if (!sawWinner) {
      ++violationsWinnerInactive;
    }
    if (!winnerIsFirstOfPriority) {
      ++violationsNotFirst;
    }
    if (referenceWinner(claims, now) != decision) {
      ++violationsReference;
    }

    // 状態機械が decide() と別の状態に落ち着いていないこと。
    (void)machine.update(claims, now);
    if (machine.current() != decision->kind ||
        machine.currentPayloadId() != decision->payloadId) {
      ++violationsMachine;
    }

    // ---- 被覆 ------------------------------------------------------------
    std::array<std::size_t, ambient::core::kClaimPriorityCount> activeByPriority{};
    for (const Claim& claim : claims) {
      if (claim.activeAt(now)) {
        ++activeByPriority[static_cast<std::size_t>(claim.priority)];
      }
    }
    const bool photoActive = activeByPriority[static_cast<std::size_t>(ClaimPriority::Photo)] > 0;
    const bool envActive = activeByPriority[static_cast<std::size_t>(ClaimPriority::Env)] > 0;
    const bool alertActive = activeByPriority[static_cast<std::size_t>(ClaimPriority::Alert)] > 0;
    const bool recordActive =
        activeByPriority[static_cast<std::size_t>(ClaimPriority::RecordedAwaitingServer)] > 0;
    if (photoActive && envActive && alertActive) {
      ++coverAllThree;
    }
    if (alertActive && recordActive) {
      ++coverAlertVsRecord;
    }
    for (const std::size_t count : activeByPriority) {
      if (count >= 2) {
        ++coverTie;
        break;
      }
    }
    ++wins[static_cast<std::size_t>(decision->priority)];

    clock.advance(Millis{kTickMs});
  }

  // ★ループの中で CHECK を撃たない。集計してから 1 回だけ検査する。
  CHECK(violationsHigherActive == 0);
  CHECK(violationsWinnerInactive == 0);
  CHECK(violationsNotFirst == 0);
  CHECK(violationsReference == 0);
  CHECK(violationsMachine == 0);
  CHECK(violationsNullopt == 0);

  // ★被覆の検査。ここが 0 なら「0 違反」は何も証明していない。
  CHECK(coverAllThree > 0);
  CHECK(coverAlertVsRecord > 0);
  CHECK(coverTie > 0);
  CHECK(wins[static_cast<std::size_t>(ClaimPriority::Photo)] > 0);
  CHECK(wins[static_cast<std::size_t>(ClaimPriority::Env)] > 0);
  CHECK(wins[static_cast<std::size_t>(ClaimPriority::CareDue)] > 0);
  CHECK(wins[static_cast<std::size_t>(ClaimPriority::Alert)] > 0);
  CHECK(wins[static_cast<std::size_t>(ClaimPriority::RecordedAwaitingServer)] > 0);

  // ★Alert は最大ではない。Alert と記録保留が同時に有効な tick では
  //   **必ず記録保留が勝っている**（この soak の要）。
  MESSAGE("soak: ticks=" << kTicks << " 写真+環境+地震 同時=" << coverAllThree
                         << " 地震+記録保留 同時=" << coverAlertVsRecord
                         << " 同一優先度の競合=" << coverTie);
  MESSAGE("soak: 勝者の内訳 Photo=" << wins[0] << " Env=" << wins[1] << " Degraded=" << wins[2]
                                    << " Care=" << wins[3] << " CareDue=" << wins[4]
                                    << " Alert=" << wins[5] << " Recorded=" << wins[6]);
}

TEST_CASE("★soak: 同じ seed なら同じ遷移列になる（決定性）") {
  // ★決定的でない soak は「たまたま通った」を検出できない。
  //   同じ seed で 2 回回して、状態遷移のトレースが完全一致することを見る。
  const auto run = [](std::uint64_t seed) {
    SoakRng rng{seed};
    PhotoSoakSource photo;
    BurstSoakSource quake{ClaimPriority::Alert, DisplayStateKind::Alert, 1};
    BurstSoakSource recording{ClaimPriority::RecordedAwaitingServer,
                              DisplayStateKind::RecordedAwaitingServer, 2};
    ambient::core::EnvironmentModel env;
    ambient::core::EnvCutSchedule schedule;
    schedule.interval = Millis{30000};
    schedule.duration = Millis{8000};
    env.setCutSchedule(schedule);

    const std::array<IClaimSource*, 4> sources{{&photo, &env, &quake, &recording}};
    StateMachine machine;
    std::vector<Claim> claims;
    std::vector<ambient::core::Transition> transitions;

    ambient::core::FakeClock clock{SteadyTime{0}, ambient::core::WallTime{0}};
    for (int tick = 0; tick < 5000; ++tick) {
      const SteadyTime now = clock.steadyNow();
      const std::uint32_t rollQuake = rng.belowN(300);
      const std::uint32_t rollRecord = rng.belowN(150);
      const std::uint32_t rollRoom = rng.belowN(200);
      if (rollQuake == 0) {
        quake.fire(now, ambient::core::kAlertHoldDuration);
      }
      if (rollRecord == 0) {
        recording.fire(now, ambient::core::constants::kToastTotal);
      }
      if (rollRoom == 0) {
        env.ingestRoomEnv(soakRoomEnv(true, 800), now, clock.wallNow());
      }
      ambient::core::collectClaims(sources, now, claims);
      if (const auto transition = machine.update(claims, now)) {
        transitions.push_back(*transition);
      }
      clock.advance(Millis{100});
    }
    return ambient::core::formatTrace(transitions);
  };

  const std::string first = run(0xABCDEFULL);
  const std::string second = run(0xABCDEFULL);
  CHECK(first == second);
  CHECK_FALSE(first.empty());
  // seed が違えば列も違う（乱数が効いていることの確認）。
  CHECK(run(0x123456ULL) != first);
}
