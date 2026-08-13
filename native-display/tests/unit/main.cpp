// tests/unit/main.cpp — doctest のエントリポイント（このファイルにだけ実装を展開する）
//
// ############################################################################
// # このテストターゲットは ambient_core **だけ** にリンクしている。
// #
// #   CMakeLists.txt の unit_tests のリンク行を見ること。
// #   SDL も curl も無い。これは意図であり、層分離の継続的な検証である。
// #
// #   テストのために SDL や curl をリンクしたくなったら、リンク行を直す前に
// #   「そのロジックは本当に Core にあるべきか」「テストが Core の外を
// #   触ろうとしていないか」を疑うこと。
// ############################################################################
//
// ファイルの割り当て（次フェーズ）:
//
//   test_rng.cpp              1B  乱数。★消費順序の golden trace はここ
//   test_clock.cpp            1A  時刻・スリープ復帰の跳躍検知
//   test_state_store.cpp      1B  永続化・容量超過
//   test_http.cpp             1D  HTTP 応答の扱い（HTML エラーページを含む）
//   test_transition.cpp       1C  遷移／滞在・時間駆動の進捗・重み抽選
//   test_parse.cpp            1D  文字列 → 型（震度 "5-" など）
//   test_arbiter.cpp          1A  優先度アービター・状態機械
//   test_care_pending.cpp     1A  お世話記録の保留（不変条件）
//   test_photo_scheduler.cpp  1B  スケジューラ・スナップショット
//
// 現時点のテストは「ハーネスが動くこと」と「実装済みの契約」だけを見ている。
// スタブに対する意味のあるテストは書けないので、書く場所だけ用意してある。

#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest.h>

#include <string_view>

#include "ambient/core/version.hpp"

TEST_CASE("ハーネスが動く: ambient_core のバージョン文字列が取れる") {
  const char* version = ambient::core::version_string();
  REQUIRE(version != nullptr);
  CHECK(std::string_view{version} == "0.1.0");

  const char* note = ambient::core::target_note();
  REQUIRE(note != nullptr);
  CHECK(std::string_view{note}.find("Tab5") != std::string_view::npos);
}
