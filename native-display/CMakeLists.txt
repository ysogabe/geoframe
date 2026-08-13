# GeoFrame Ambient Display — macOS + SDL2 リファレンス実装
#
# 最終ターゲットは M5Stack Tab5（ESP32-P4）。Mac + SDL2 は「移植可能な C++ Core」を
# 検証するためのリファレンス実装であって、完成品ではない。
#
# ------------------------------------------------------------------------------
# 層構造（これが設計の要。規約ではなくビルドエラーで層分離を強制する）
# ------------------------------------------------------------------------------
#
#   ambient_core        (STATIC)  依存: 標準ライブラリのみ
#   ambient_render_sdl  (STATIC)  依存: ambient_core + SDL2 / SDL2_image / SDL2_ttf
#   ambient_transport   (STATIC)  依存: ambient_core + libcurl
#   ambient_display     (EXE)     依存: 上記3つ
#
# ambient_core は Tab5（ESP32-P4 / ESP-IDF）へそのまま持っていく層である。
# したがって ambient_core に以下を書いてはいけない:
#
#   * #include <SDL.h> などの SDL ヘッダ
#       → ambient_core の include path に SDL のパスは入れていない。
#         書いた瞬間にコンパイルが落ちるのが正しい状態。
#   * std::filesystem
#       → ESP32 に存在しない。パス解決は app / render 層の責務。
#   * std::thread / std::mutex
#       → スレッドは Renderer / Transport 層の責務。Core は単一スレッド前提。
#   * <format>
#       → 使えるがコードサイズを大きく増やす。Tab5 の flash 予算に効くので避ける。
#
# 依存を足したくなったら、それは本当に Core の責務か疑うこと。
# 大抵は render 層か transport 層に置くべきものである。
# ------------------------------------------------------------------------------

cmake_minimum_required(VERSION 3.20...3.31)

project(AmbientDisplay
  VERSION 0.1.0
  DESCRIPTION "GeoFrame Ambient Switch — macOS/SDL2 reference implementation"
  LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE RelWithDebInfo CACHE STRING "Build type" FORCE)
endif()

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

list(APPEND CMAKE_MODULE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/cmake")
include(Dependencies)

# --- 共通の警告設定 ---------------------------------------------------------
# 警告ゼロを維持すること。SDL のヘッダ由来の警告は imported ターゲットの
# include が SYSTEM 扱いになるため出ない。

add_library(ambient_warnings INTERFACE)
target_compile_options(ambient_warnings INTERFACE
  -Wall
  -Wextra
  -Wpedantic
  -Wshadow
  -Wconversion
  -Wsign-conversion)

# ==============================================================================
# ambient_core — プラットフォーム非依存層
# ==============================================================================
#
# ここに SDL も curl も JSON も入れない。target_link_libraries も
# ambient_warnings（コンパイルフラグのみ）以外は付けないこと。

#
# 【重要】ここに追加した .cpp は移植性ゲート
# (tools/check-esp-portability.sh) が ESP32-P4 の実ツールチェーンでコンパイルする。
# 逆に言うと、どの .cpp からも include されないヘッダは検証されない。
# ヘッダを足したら、それを include する .cpp が必ず 1 本あるようにすること。

add_library(ambient_core STATIC
  src/core/version.cpp
  src/core/rng.cpp
  src/core/clock.cpp
  src/core/state_store.cpp
  src/core/http.cpp
  src/core/transition.cpp
  src/core/constants.cpp
  src/core/model/parse.cpp
  src/core/model/photo.cpp
  src/core/state/arbiter.cpp
  src/core/state/state_machine.cpp
  src/core/state/alert_engine.cpp
  src/core/care/pending_record.cpp
  src/core/photo/scheduler.cpp
  src/core/photo/snapshot.cpp
  src/core/image/downsample.cpp
  src/core/env/environment_model.cpp
  src/core/care/sync_policy.cpp
  src/core/care/care_model.cpp)

target_include_directories(ambient_core
  PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/src/core/include>)

target_link_libraries(ambient_core PRIVATE ambient_warnings)

# ==============================================================================
# ambient_render_sdl — SDL2 バックエンド（Tab5 では M5GFX 版に差し替える層）
# ==============================================================================

add_library(ambient_render_sdl STATIC
  src/render_sdl/sdl_runtime.cpp
  src/render_sdl/display.cpp
  src/render_sdl/text.cpp
  src/render_sdl/image.cpp
  src/render_sdl/screens.cpp
  src/render_sdl/transitions.cpp
  src/render_sdl/image_worker.cpp)

target_include_directories(ambient_render_sdl
  PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/src/render_sdl/include>)

# SDL は PUBLIC。この層のヘッダが SDL の型（SDL_Renderer* 等）を公開しているため。
# ambient_core は PUBLIC（描画対象のデータ型を将来公開する）。
target_link_libraries(ambient_render_sdl
  PUBLIC
    ambient_core
    ambient_dep_sdl2
    ambient_dep_sdl2_image
    ambient_dep_sdl2_ttf
  PRIVATE
    ambient_warnings)

# ==============================================================================
# ambient_transport — HTTP 取得層（GeoFrame server / hogo-meter）
# ==============================================================================
#
# 今回は中身が空（バージョン文字列だけ）。後続フェーズで libcurl を使った
# クライアントがここに入る。curl は PRIVATE — この層のヘッダは curl の型を
# 公開しないこと（Tab5 では esp_http_client に差し替えるため）。

add_library(ambient_transport STATIC
  src/transport/version.cpp
  src/transport/fixture_http_client.cpp
  src/transport/curl_http_client.cpp
  src/transport/geoframe_client.cpp
  src/transport/hogo_client.cpp)

target_include_directories(ambient_transport
  PUBLIC $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/src/transport/include>)

# nlohmann/json は third_party/ に単一ヘッダで vendor 済み（FetchContent は使わない）。
# バージョン・ライセンス・SHA-256 は third_party/nlohmann/README.md を参照。
#
# ★SYSTEM を付ける理由: nlohmann 自身のヘッダが -Wconversion 等で警告を出しても
#   こちらのビルドを警告ゼロに保つため。
add_library(ambient_dep_nlohmann INTERFACE)
target_include_directories(ambient_dep_nlohmann SYSTEM
  INTERFACE $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/third_party>)

# ★curl も nlohmann も PRIVATE であること。
#   この層の公開ヘッダに curl の型や JSON 型が漏れたら、利用側でコンパイルエラーになる。
#   それが正しい状態である（Tab5 では esp_http_client / ArduinoJson に差し替わるため、
#   公開ヘッダに漏れていると差し替えられない）。
target_link_libraries(ambient_transport
  PUBLIC  ambient_core
  PRIVATE ambient_dep_curl ambient_dep_nlohmann ambient_warnings)

# ==============================================================================
# ambient_display — 実行ファイル
# ==============================================================================

add_executable(ambient_display
  src/app/main_sdl.cpp)

target_link_libraries(ambient_display
  PRIVATE
    ambient_core
    ambient_render_sdl
    ambient_transport
    ambient_warnings)

# アセットのデフォルト位置を configure 時に焼き込む。
# 相対パスにすると cwd 依存で壊れるため（リポジトリルートから
# ./native-display/build/ambient_display と叩けること）。
# 実行時は --assets <dir> で上書きできる。
target_compile_definitions(ambient_display PRIVATE
  AMBIENT_ASSET_DIR="${CMAKE_CURRENT_SOURCE_DIR}/assets")

# ==============================================================================
# unit_tests — ambient_core だけを検証するテスト
# ==============================================================================
#
# 【重要】このターゲットは ambient_core **だけ** にリンクする。
#
#   このリンク行が層分離の継続的な検証そのものである。
#   SDL / curl をここへ足したくなったら、それは「Core に置くべきでないものが
#   Core に入った」か「テストが Core の外を触ろうとしている」かのどちらかである。
#   リンク行を直すのではなく、設計側を疑うこと。
#
# doctest は third_party/ に単一ヘッダで vendor 済み（FetchContent は使わない）。
# 理由・バージョン・ライセンスは third_party/doctest/README.md を参照。

add_library(ambient_dep_doctest INTERFACE)

# SYSTEM を付ける理由: doctest 自身のヘッダが -Wall/-Wextra/-Wconversion で
# 警告を出しても、こちらのビルドを警告ゼロに保つため。
target_include_directories(ambient_dep_doctest SYSTEM
  INTERFACE $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/third_party/doctest>)

enable_testing()

add_executable(unit_tests
  tests/unit/main.cpp
  tests/unit/test_rng.cpp
  tests/unit/test_clock.cpp
  tests/unit/test_state_store.cpp
  tests/unit/test_http.cpp
  tests/unit/test_transition.cpp
  tests/unit/test_parse.cpp
  tests/unit/test_arbiter.cpp
  tests/unit/test_care_pending.cpp
  tests/unit/test_photo_scheduler.cpp
  tests/unit/test_state_machine.cpp
  tests/unit/test_alert_engine.cpp
  tests/unit/test_snapshot.cpp
  tests/unit/test_golden_trace.cpp
  tests/unit/test_downsample.cpp
  tests/unit/test_environment_model.cpp
  tests/unit/test_sync_policy.cpp
  tests/unit/test_care_model.cpp)

# ★SDL も curl も書かないこと。ambient_core と doctest だけ。
target_link_libraries(unit_tests
  PRIVATE
    ambient_core
    ambient_dep_doctest
    ambient_warnings)

# DOCTEST_CONFIG_DOUBLE_STRINGIFY が必要な理由（外すとコンパイルが落ちる）:
#
#   doctest は値を文字列化するとき **非修飾の** toString(x) を呼ぶ。
#   ambient::core にも toString(StoreStatus) 等があるため、ADL でこちらが選ばれ、
#   const char* が返る。doctest 側は String を期待しているので
#   「const char* + const char*」で invalid operands になる。
#
#   このマクロを定義すると toString(toString(x)) となり、内側で ambient::core の
#   const char* を得たあと、外側の doctest::toString(const char*) が String にする。
#   結果として **失敗メッセージに enum の名前がそのまま出る**（"{?}" にならない）ので、
#   回避策というより改善である。
#
#   ★enum を CHECK で比較したいレーンは、core 側に toString() を足せばよい。
target_compile_definitions(unit_tests PRIVATE DOCTEST_CONFIG_DOUBLE_STRINGIFY)

add_test(NAME unit_tests COMMAND unit_tests)

# ==============================================================================
# contract_tests — transport 層の契約テスト
# ==============================================================================
#
# unit_tests との違いは **リンクするもの** であり、それが層の境界の表明になっている。
#
#   unit_tests     : ambient_core だけ。SDL も curl も入らない
#   contract_tests : ambient_core + ambient_transport。実 HTTP は使わない
#
# ここに置くのは「fixture / Fake を差し込んで、実通信なしにクライアントの
# 振る舞いを固定する」テスト。**実サーバにも本番 API にも一切繋がないこと。**
# 異常系（401/403/404/409/5xx/タイムアウト/HTML が返る）はすべてここで再現する。

add_executable(contract_tests
  tests/contract/main.cpp
  tests/contract/test_fixture_http.cpp
  tests/contract/test_geoframe_client.cpp
  tests/contract/test_hogo_client.cpp)

target_link_libraries(contract_tests
  PRIVATE
    ambient_core
    ambient_transport
    ambient_dep_doctest
    ambient_warnings)

target_compile_definitions(contract_tests PRIVATE
  DOCTEST_CONFIG_DOUBLE_STRINGIFY
  AMBIENT_ASSET_DIR="${CMAKE_CURRENT_SOURCE_DIR}/assets")

add_test(NAME contract_tests COMMAND contract_tests)

# ==============================================================================
# render_tests — 描画層のテスト（実ウィンドウを開かない）
# ==============================================================================
#
# ★ピクセル完全一致を採らないこと。
#   ソフトウェアレンダラと Metal で結果が differ し、SDL のバージョン差でも変わる。
#   代わりに**構造アサーション**を書くこと:
#     Slide            : progress=0.5 で左半分が旧・右半分が新
#     Wipe             : 境界が progress に線形追従
#     FadeThroughBlack : progress=0.5 で輝度最小
#   ただし「同一 seed で2回描いて完全一致」はピクセル比較でよい。

add_executable(render_tests
  tests/render/main.cpp
  tests/render/test_transitions.cpp)

target_link_libraries(render_tests
  PRIVATE
    ambient_core
    ambient_render_sdl
    ambient_dep_doctest
    ambient_warnings)

target_compile_definitions(render_tests PRIVATE
  DOCTEST_CONFIG_DOUBLE_STRINGIFY
  AMBIENT_ASSET_DIR="${CMAKE_CURRENT_SOURCE_DIR}/assets")

add_test(NAME render_tests COMMAND render_tests)

# 実ウィンドウを開かずに走らせる。CI や他の作業と画面を奪い合わないため。
set_tests_properties(render_tests PROPERTIES
  ENVIRONMENT "SDL_VIDEODRIVER=dummy")

message(STATUS "----------------------------------------------------------------")
message(STATUS "アセット既定パス    : ${CMAKE_CURRENT_SOURCE_DIR}/assets")
message(STATUS "ビルドタイプ        : ${CMAKE_BUILD_TYPE}")
message(STATUS "C++ 標準            : ${CMAKE_CXX_STANDARD}")
message(STATUS "----------------------------------------------------------------")
