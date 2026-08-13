// ambient/core/clock.hpp — 時刻の抽象
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ============================================================================
// なぜ単調時刻と壁時計を両方持つのか
// ============================================================================
//
// 用途が違う。混ぜると必ず壊れる。
//
//   steadyNow()  単調時刻。経過時間の計測に使う。
//                トランジションの progress、クールダウン、トースト猶予、
//                ポーリング間隔 — すべてこちら。
//                NTP 補正やタイムゾーン変更で巻き戻らないことが必要だから。
//
//   wallNow()    壁時計（Unix epoch 秒, UTC）。「いつの出来事か」に使う。
//                地震の at、写真の撮影日時、お世話の記録時刻 — こちら。
//                サーバや他端末と突き合わせる値だから。
//
// ★ SteadyTime と WallTime は別の型にしてある。取り違えるとコンパイルエラーになる。
//
// ============================================================================
// スリープ復帰のジャンプ ― これが本命
// ============================================================================
//
// macOS の mach_absolute_time（= std::chrono::steady_clock の実体）は
// **システムスリープ中に停止する**。ノートを閉じて 8 時間後に開くと、
// 単調時刻は 8 時間ぶん進んでいない。
//
// この状態で「30 秒ごとにポーリング」「2 秒でトースト満了」「10 分クールダウン」を
// 単調時刻だけで判断すると、復帰直後に
//   - ポーリングが 8 時間ぶん遅れたまま再開しない
//   - クールダウンが明けたつもりで明けていない
// という実時間とのズレが残る。トーストに至っては 8 時間表示され続けうる。
//
// そこで **跳躍を検知して Core へ通知する**。検知は「壁時計の進みと
// 単調時刻の進みの差 (skew)」で行う。これは純粋な計算なので Core に置く。
// プラットフォーム実装 (IClock) は生の 2 つの時刻を返すだけでよい。
//
//   skew > 0  : 壁だけ進んだ = 単調時計が止まっていた（スリープ復帰）
//   skew < 0  : 壁が巻き戻った / 大きく飛んだ（NTP 初回同期、手動時刻変更）
//
// どちらの場合も Core は「予定を組み直せ」というシグナルとして扱う。
// 具体的な再スケジュール内容は各消費者（ポーリング / トースト / クールダウン）の責務。
//
// ★注意: ESP32 側では esp_timer が deep sleep 中も進むため skew は出にくいが、
//   NTP の初回同期で壁時計が 1970 年から現在へ飛ぶ。検知の仕組みは同じで足りる。

#pragma once

#include <compare>
#include <cstdint>
#include <optional>

namespace ambient::core {

// ----------------------------------------------------------------------------
// 時間の型（強い型。相互に暗黙変換しない）
// ----------------------------------------------------------------------------

// 期間。ミリ秒。
struct Millis {
  std::int64_t value{0};

  [[nodiscard]] friend constexpr auto operator<=>(const Millis&, const Millis&) noexcept = default;
};

[[nodiscard]] constexpr Millis operator+(Millis a, Millis b) noexcept {
  return Millis{a.value + b.value};
}
[[nodiscard]] constexpr Millis operator-(Millis a, Millis b) noexcept {
  return Millis{a.value - b.value};
}
[[nodiscard]] constexpr Millis operator-(Millis a) noexcept { return Millis{-a.value}; }

// 単調時刻。起点は任意（プロセス起動時など）。絶対値に意味はなく、差分だけに意味がある。
struct SteadyTime {
  std::int64_t ms{0};

  [[nodiscard]] friend constexpr auto operator<=>(const SteadyTime&,
                                                  const SteadyTime&) noexcept = default;
};

[[nodiscard]] constexpr SteadyTime operator+(SteadyTime t, Millis d) noexcept {
  return SteadyTime{t.ms + d.value};
}
[[nodiscard]] constexpr SteadyTime operator-(SteadyTime t, Millis d) noexcept {
  return SteadyTime{t.ms - d.value};
}
[[nodiscard]] constexpr Millis operator-(SteadyTime a, SteadyTime b) noexcept {
  return Millis{a.ms - b.ms};
}

// 壁時計。Unix epoch 秒 (UTC)。
// ★秒精度。ミリ秒精度が要るものは単調時刻で測ること。
struct WallTime {
  std::int64_t epochSeconds{0};

  [[nodiscard]] friend constexpr auto operator<=>(const WallTime&,
                                                  const WallTime&) noexcept = default;
};

[[nodiscard]] constexpr Millis operator-(WallTime a, WallTime b) noexcept {
  return Millis{(a.epochSeconds - b.epochSeconds) * 1000};
}

// ----------------------------------------------------------------------------
// IClock — プラットフォームが実装する
// ----------------------------------------------------------------------------
//
// 実装は Mac 側が std::chrono、Tab5 側が esp_timer / time()。
// ★実装は「生の値を返す」だけにすること。跳躍判定などのポリシーを持ち込まない
//   （持ち込むと Mac と Tab5 でポリシーが分岐し、テストできなくなる）。
class IClock {
 public:
  IClock() = default;
  virtual ~IClock() = default;
  IClock(const IClock&) = delete;
  IClock& operator=(const IClock&) = delete;
  IClock(IClock&&) = delete;
  IClock& operator=(IClock&&) = delete;

  // 単調時刻。巻き戻らないこと。macOS ではスリープ中に停止してよい
  // （それを検知するのが TimeJumpDetector の役目）。
  [[nodiscard]] virtual SteadyTime steadyNow() const noexcept = 0;

  // 壁時計。未同期なら 0 に近い値を返してよい（NTP 前は 1970 年）。
  [[nodiscard]] virtual WallTime wallNow() const noexcept = 0;
};

// ----------------------------------------------------------------------------
// 跳躍検知 ― 純粋ロジックなので Core に置く
// ----------------------------------------------------------------------------

struct TimeJump {
  Millis steadyDelta{};  // 前回 update からの単調時刻の進み
  Millis wallDelta{};    // 前回 update からの壁時計の進み
  Millis skew{};         // wallDelta - steadyDelta。正 = 単調時計が止まっていた
};

// 既定のしきい値。
// ポーリング間隔 (30s) より十分小さく、通常の tick 揺らぎより十分大きい値。
// ここを小さくしすぎると通常運転で誤検知し、再スケジュールが暴発する。
inline constexpr Millis kDefaultTimeJumpThreshold{2000};

// 毎 tick 呼ぶ。副作用は自身の内部状態のみ（Core は単一スレッド前提）。
//
// 使い方:
//   auto jump = detector.update(clock.steadyNow(), clock.wallNow());
//   if (jump) { /* ポーリング予定・トースト・クールダウンを組み直す */ }
class TimeJumpDetector {
 public:
  explicit TimeJumpDetector(Millis threshold = kDefaultTimeJumpThreshold) noexcept;

  // |skew| がしきい値を超えたときだけ TimeJump を返す。
  // 初回呼び出しは基準点を取るだけで必ず nullopt。
  [[nodiscard]] std::optional<TimeJump> update(SteadyTime steady, WallTime wall) noexcept;

  // 基準点を捨てる。次回 update は初回扱いになる。
  void reset() noexcept;

  [[nodiscard]] Millis threshold() const noexcept { return threshold_; }

 private:
  Millis threshold_;
  SteadyTime lastSteady_{};
  WallTime lastWall_{};
  bool primed_{false};
};

// ----------------------------------------------------------------------------
// 跳躍を検知したら何をするか ― ポリシー（レーン 1A が決定）
// ----------------------------------------------------------------------------
//
// clock.hpp は「検知」までしか決めていなかった。消費者ごとの対処を各所で
// 好きに書くと、Mac と Tab5 で、あるいはモジュール間で挙動が分岐する。
// そこで **変換を 1 つの純粋関数に閉じ込める**。
//
//   catchUp(jump)         跳躍のうち「実際に経過したのに単調時刻が数えなかった」量
//   rebaseForJump(anchor) 単調時刻の基準点を、その量だけ過去へずらす
//
// ★決めたこと（と、その理由）:
//
//   skew > 0（スリープ復帰 / NTP で壁が前へ飛んだ）
//     → **経過時間を測っている基準点を巻き戻す**。
//       つまり「その時間は実際に経過した」として扱う。
//       - 地震のグローバルクールダウン（10 分）: 実時間で 10 分経ったなら
//         明けていなければならない。明けないままだと、復帰後に本物の地震が
//         来ても抑止される。**抑止側に倒れるのが最も危険**なので巻き戻す。
//       - アラートの表示時間: 8 時間前の地震アラートが画面に残り続けるのを防ぐ。
//         基準点を巻き戻せば alerting() が自然に false になる。
//
//   skew < 0（壁時計が巻き戻った。NTP 補正 / 手動変更）
//     → **何もしない**。壁時計が信用できないという情報でしかなく、
//       実時間が経過した証拠にはならない。ここで基準点を未来へずらすと
//       クールダウンが延び、**本物の地震アラートを黙って握り潰す**。
//       単調時刻は巻き戻らないので、そのまま基準として使い続けるのが正しい。
//
//   PendingCareRecord（2 秒の取り消し猶予）
//     → ★意図的に何もしない。不変条件が既に安全側に倒れているため。
//       スリープ中に猶予が満了していたら、復帰後の最初の tick() が
//       ToastExpired で **確定** する（破棄ではない）。
//       スリープ中にユーザが取り消しボタンを押せた道理も無い。
//       跳躍補正を足しても「いつ確定するか」しか変わらず、
//       「確定するか破棄するか」は変わらない。触らないのが最も安全。
//       （UI 側は残り秒の表示を再計算すればよい。それは Renderer の責務）
//
//   ポーリング予定
//     → Core の責務ではない（transport / app 層）。同じ rebaseForJump() を
//       次回ポーリング予定時刻に当てれば「即時 1 回ポーリング」になる。

// 跳躍のうち「実時間は進んだのに単調時刻が数えなかった」量。
// 巻き戻し（skew < 0）は 0 を返す ― 上記のとおり握り潰しを避けるため。
[[nodiscard]] constexpr Millis catchUp(const TimeJump& jump) noexcept {
  return jump.skew.value > 0 ? jump.skew : Millis{0};
}

// 経過時間の基準点を、跳躍ぶんだけ過去へずらす。
// ★anchor - catchUp なので、以降の (now - anchor) がその量だけ大きくなる
//   = 「その時間は経過した」という扱いになる。
[[nodiscard]] constexpr SteadyTime rebaseForJump(SteadyTime anchor,
                                                 const TimeJump& jump) noexcept {
  return anchor - catchUp(jump);
}

// ----------------------------------------------------------------------------
// FakeClock — テスト用の差し替え実装
// ----------------------------------------------------------------------------
//
// Core に置く理由: unit_tests は ambient_core だけにリンクする（層分離の検証）。
// テストダブルが Core の外にあると、テストがプラットフォーム層へ依存してしまう。
class FakeClock final : public IClock {
 public:
  FakeClock() = default;
  explicit FakeClock(SteadyTime steady, WallTime wall) noexcept
      : steady_(steady), wall_(wall) {}

  [[nodiscard]] SteadyTime steadyNow() const noexcept override { return steady_; }
  [[nodiscard]] WallTime wallNow() const noexcept override { return wall_; }

  // 通常の時間経過。単調時刻と壁時計を同じだけ進める。
  void advance(Millis d) noexcept;

  // ★macOS のシステムスリープを模す。壁時計だけ進み、単調時刻は止まる。
  //   TimeJumpDetector がこれを検知できることをテストで固定すること。
  void simulateSleep(Millis wallElapsed) noexcept;

  // NTP 同期などによる壁時計の飛びを模す。単調時刻は動かさない。
  void setWall(WallTime w) noexcept { wall_ = w; }

 private:
  SteadyTime steady_{};
  WallTime wall_{};
};

}  // namespace ambient::core
