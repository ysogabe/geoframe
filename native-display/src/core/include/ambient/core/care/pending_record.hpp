// ambient/core/care/pending_record.hpp — お世話記録の保留（2 秒の取り消し猶予）
//
// 【この層の制約】docs/PORTABILITY.md と CMakeLists.txt 冒頭を必ず読むこと。
//
// ############################################################################
// # ★不変条件 ― このモジュールはこれが全てである
// #
// #   保留は「取り消しボタンへのヒット」でのみ破棄される。
// #   それ以外のすべての離脱経路は、離脱前に確定する。
// #
// ############################################################################
//
// ============================================================================
// なぜ「3 つの確定トリガ」として書いてはいけないのか
// ============================================================================
//
// 素朴に書くと「トースト満了で確定」「別カードのタップで確定」「終了時に確定」
// という **経路の列挙** になる。これは必ず破綻する。
//
// 経路は後から増えるからである。設定画面への遷移、無操作タイムアウト、
// 画面ロック、外部からの表示切替 ―― 増えるたびに「ここでも確定を呼ぶ」を
// 思い出さなければならない。**1 箇所忘れると、ユーザが押した記録が黙って消える。**
// しかも消えたことは画面に出ない（トーストはもう閉じている）ので、
// 「たまに記録されない」というバグになり再現条件が掴めない。
//
// そこで **1 つの不変条件** として実装する。
//
//   離脱の出口を leave(PendingExit) 1 箇所に絞る。
//   leave() の中身は「理由が UndoHit なら破棄、それ以外は確定」の 1 行である。
//
// ★新しい離脱経路を足すときは、PendingExit に値を足して leave() を呼ぶだけでよい。
//   確定を書き忘れる余地が無い。既定が「確定」だからである。
//
// 既知の離脱経路（★これで全部とは限らない。だから不変条件として書いている）:
//   - トースト満了
//   - 別カードのタップ
//   - 設定画面への遷移
//   - 無操作タイムアウト
//   - アプリ終了
//
// ============================================================================
// 確定した後どうなるか ― RecordedAwaitingServer
// ============================================================================
//
// 確定 = 「送信キューへ積む」であって「サーバに届いた」ではない。
// さらに、記録の状態色（Red / Yellow / Green）は **サーバが計算して返す** ため、
// 送信が成功しても次の config poll（最大 30 秒後）まで色は変わらない。
//
// ★「記録したら即 Green」は成立しない。
//   DisplayStateKind::RecordedAwaitingServer がこの間の状態である
//   （ambient/core/state/display_state.hpp）。

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>

#include "ambient/core/clock.hpp"
#include "ambient/core/constants.hpp"

namespace ambient::core {

// お世話項目。
// 出典: <path-to-hogo-meter>/firmware/lib/hogo_common/app_types.h:32-42
//       （★読み取り専用。値は配列添字として使えるよう 0 起点）
// slot3-5 は Tab5 の 6 項目モード専用の中立スロット。用途はサーバのラベルで決まる。
enum class CareItem : std::uint8_t {
  Toilet = 0,
  Eye = 1,
  Food = 2,
  Slot3 = 3,
  Slot4 = 4,
  Slot5 = 5,
};

// 記録の状態。
// 出典: app_types.h:53（★数値コードはサーバ API と一致させること）
// ★この値を Mac 側で計算してはいけない。サーバが返した値をそのまま使う。
enum class ScheduleStatus : std::uint8_t {
  None = 0,
  Green = 1,
  Yellow = 2,
  Red = 3,
};

// ----------------------------------------------------------------------------
// 離脱の理由
// ----------------------------------------------------------------------------
//
// ★新しい離脱経路はここに足す。足すだけで確定される（既定が確定だから）。
//   UndoHit を増やしてはいけない ―― 破棄する経路は永久に 1 つだけである。
enum class PendingExit : std::uint8_t {
  // ★唯一の破棄経路。取り消しボタンへの明示的なヒット。
  UndoHit = 0,

  // 以下はすべて確定。
  ToastExpired,     // トースト満了
  OtherCardTapped,  // 別カードのタップ
  NavigatedAway,    // 設定画面などへの遷移
  IdleTimeout,      // 無操作タイムアウト
  AppShutdown,      // アプリ終了
};

// ★PendingExit の値の数。値を足したらここも +1 すること。
//
// 足し忘れの検出は 2 段構えになっている（どちらも「黙って通る」ことがない）:
//
//   1. pending_record.cpp の toString() は **default 無しの switch** である。
//      値を足すと -Wswitch が出る（ビルドは警告ゼロを維持する規律）。
//      ★default を足さないこと。足すとこの検出が死ぬ。
//   2. この定数を +1 すると、tests/unit/test_care_pending.cpp の
//      kAllExits に対する static_assert が落ち、新しい経路を
//      「離脱経路の全列挙」テストへ入れるまでコンパイルが通らない。
//
// 1 が「気づく」ための仕掛け、2 が「テストさせる」ための仕掛けである。
inline constexpr std::size_t kPendingExitCount = 6;

[[nodiscard]] const char* toString(PendingExit reason) noexcept;

// ★この関数が不変条件そのものである。
//   switch で経路を列挙しないこと。UndoHit だけを名指しし、残りは既定で確定。
[[nodiscard]] constexpr bool discardsPending(PendingExit reason) noexcept {
  return reason == PendingExit::UndoHit;
}

enum class PendingOutcome : std::uint8_t {
  Committed = 0,
  Discarded,
};

// 確定した記録。送信キューへ積む単位。
struct CommittedRecord {
  CareItem item{CareItem::Toilet};

  // ★記録時刻は **壁時計** で持つ（サーバへ送る絶対時刻だから）。
  //   保留の残り時間は単調時刻で測る（clock.hpp の使い分けを参照）。
  WallTime recordedAt{};
};

// ----------------------------------------------------------------------------
// PendingCareRecord
// ----------------------------------------------------------------------------
//
// 保留は同時に 1 件だけ。2 件目を begin() すると 1 件目は
// OtherCardTapped 相当で **確定** される（破棄されない）。
class PendingCareRecord {
 public:
  PendingCareRecord() = default;

  // 保留を開始する。既存の保留があれば、それを先に確定して返す。
  // ★戻り値を捨てないこと。捨てると 1 件目の記録が失われる。
  [[nodiscard]] std::optional<CommittedRecord> begin(CareItem item, SteadyTime now,
                                                     WallTime wallNow);

  [[nodiscard]] bool active() const noexcept { return active_; }
  [[nodiscard]] CareItem pendingItem() const noexcept { return item_; }

  // 残り猶予（表示用）。保留中でなければ Millis{0}。
  [[nodiscard]] Millis remaining(SteadyTime now) const noexcept;

  // ★唯一の離脱 API。すべての離脱経路はここを通す。
  //   保留中でなければ nullopt（呼んでも無害。冪等）。
  //   UndoHit なら Discarded で nullopt、それ以外は必ず Committed。
  [[nodiscard]] std::optional<CommittedRecord> leave(PendingExit reason);

  // トースト満了の検知。毎 tick 呼ぶ。
  // 猶予を過ぎていれば leave(ToastExpired) と同じことをする。
  //
  // ★時間跳躍（スリープ復帰）に注意:
  //   単調時刻が止まっている間にトーストが満了すべきだった場合、
  //   復帰後の最初の tick で満了として確定する（これは正しい）。
  //   TimeJumpDetector が跳躍を報告したら、UI 側は残り秒の表示を
  //   即座に再計算すること（clock.hpp を参照）。
  [[nodiscard]] std::optional<CommittedRecord> tick(SteadyTime now);

  // 猶予（既定 2 秒。出典 hogo-meter tab5/ui.h の TOAST_TOTAL_SEC）。
  [[nodiscard]] Millis grace() const noexcept { return grace_; }
  void setGrace(Millis grace) noexcept { grace_ = grace; }

  // 直近の離脱の結果（テスト・ログ用）。
  [[nodiscard]] std::optional<PendingOutcome> lastOutcome() const noexcept {
    return lastOutcome_;
  }

 private:
  bool active_{false};
  CareItem item_{CareItem::Toilet};
  SteadyTime startedAt_{};
  WallTime recordedAt_{};
  Millis grace_{constants::kToastTotal};
  std::optional<PendingOutcome> lastOutcome_{};
};

}  // namespace ambient::core
