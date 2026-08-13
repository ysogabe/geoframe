# Dependencies.cmake — 外部依存の探索と正規化
#
# ここで見つけた依存は、必ず以下の INTERFACE ターゲット経由でのみ使うこと。
# 生の SDL2::SDL2 などを直接 target_link_libraries に書かないこと。
#
#   ambient_dep_sdl2
#   ambient_dep_sdl2_image
#   ambient_dep_sdl2_ttf
#   ambient_dep_curl
#
# 【重要】SDL2 の探索経路（CONFIG か pkg-config か）と、実際にリンクされる dylib の
# 実体パスを必ず STATUS ログへ出す。Homebrew の sdl2 は sdl2-compat（SDL3 の上に載る
# SDL2 互換層）へ移行済みで、`brew link --overwrite sdl2` を誰かが実行すると
# sdl2_ttf だけが実 sdl2 を掴み「1プロセスに libSDL2 が2つ」という致命的状態になる。
# 実体パスをログに残しておくと、その事故が configure 時点で目視できる。

include_guard(GLOBAL)

# --- ヘルパ: imported ターゲットの実体パスを解決してログへ出す -----------------

function(_ambient_report_lib_location label tgt)
  if(NOT TARGET ${tgt})
    message(STATUS "  ${label}: (ターゲット ${tgt} なし)")
    return()
  endif()

  set(_loc "")
  foreach(_prop
      IMPORTED_LOCATION
      IMPORTED_LOCATION_RELEASE
      IMPORTED_LOCATION_RELWITHDEBINFO
      IMPORTED_LOCATION_NOCONFIG)
    get_target_property(_v ${tgt} ${_prop})
    if(_v AND NOT _v STREQUAL "_v-NOTFOUND")
      set(_loc "${_v}")
      break()
    endif()
  endforeach()

  if(NOT _loc)
    # pkg-config 由来の imported ターゲットは IMPORTED_LOCATION を持たない。
    get_target_property(_v ${tgt} INTERFACE_LINK_LIBRARIES)
    if(_v AND NOT _v STREQUAL "_v-NOTFOUND")
      message(STATUS "  ${label}: link=${_v}")
    else()
      message(STATUS "  ${label}: 実体パス不明（${tgt}）")
    endif()
    return()
  endif()

  # シンボリックリンクを剥がして「本当に何をリンクするのか」を出す。
  # 例: /opt/homebrew/opt/sdl2/lib/... -> /opt/homebrew/Cellar/sdl2-compat/2.32.70/lib/...
  file(REAL_PATH "${_loc}" _real)
  if(_real STREQUAL _loc)
    message(STATUS "  ${label}: ${_loc}")
  else()
    message(STATUS "  ${label}: ${_loc}")
    message(STATUS "  ${label}   -> 実体 ${_real}")
  endif()
endfunction()

# --- SDL2 -------------------------------------------------------------------

add_library(ambient_dep_sdl2 INTERFACE)
add_library(ambient_dep_sdl2_image INTERFACE)
add_library(ambient_dep_sdl2_ttf INTERFACE)

find_package(SDL2 CONFIG QUIET)

if(SDL2_FOUND AND TARGET SDL2::SDL2)
  set(AMBIENT_SDL2_DISCOVERY "find_package(SDL2 CONFIG)")
  target_link_libraries(ambient_dep_sdl2 INTERFACE SDL2::SDL2)
  set(_ambient_sdl2_report_target SDL2::SDL2)
else()
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(SDL2 REQUIRED IMPORTED_TARGET sdl2)
  set(AMBIENT_SDL2_DISCOVERY "pkg-config (PkgConfig::SDL2)")
  target_link_libraries(ambient_dep_sdl2 INTERFACE PkgConfig::SDL2)
  set(_ambient_sdl2_report_target PkgConfig::SDL2)
endif()

find_package(SDL2_image CONFIG QUIET)
if(SDL2_image_FOUND AND TARGET SDL2_image::SDL2_image)
  set(AMBIENT_SDL2_IMAGE_DISCOVERY "find_package(SDL2_image CONFIG)")
  target_link_libraries(ambient_dep_sdl2_image INTERFACE SDL2_image::SDL2_image)
  set(_ambient_sdl2_image_report_target SDL2_image::SDL2_image)
else()
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(SDL2_IMAGE REQUIRED IMPORTED_TARGET SDL2_image)
  set(AMBIENT_SDL2_IMAGE_DISCOVERY "pkg-config (PkgConfig::SDL2_IMAGE)")
  target_link_libraries(ambient_dep_sdl2_image INTERFACE PkgConfig::SDL2_IMAGE)
  set(_ambient_sdl2_image_report_target PkgConfig::SDL2_IMAGE)
endif()

find_package(SDL2_ttf CONFIG QUIET)
if(SDL2_ttf_FOUND AND TARGET SDL2_ttf::SDL2_ttf)
  set(AMBIENT_SDL2_TTF_DISCOVERY "find_package(SDL2_ttf CONFIG)")
  target_link_libraries(ambient_dep_sdl2_ttf INTERFACE SDL2_ttf::SDL2_ttf)
  set(_ambient_sdl2_ttf_report_target SDL2_ttf::SDL2_ttf)
else()
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(SDL2_TTF REQUIRED IMPORTED_TARGET SDL2_ttf)
  set(AMBIENT_SDL2_TTF_DISCOVERY "pkg-config (PkgConfig::SDL2_TTF)")
  target_link_libraries(ambient_dep_sdl2_ttf INTERFACE PkgConfig::SDL2_TTF)
  set(_ambient_sdl2_ttf_report_target PkgConfig::SDL2_TTF)
endif()

message(STATUS "----------------------------------------------------------------")
message(STATUS "SDL2 探索経路       : ${AMBIENT_SDL2_DISCOVERY}")
_ambient_report_lib_location("SDL2      " ${_ambient_sdl2_report_target})
message(STATUS "SDL2_image 探索経路 : ${AMBIENT_SDL2_IMAGE_DISCOVERY}")
_ambient_report_lib_location("SDL2_image" ${_ambient_sdl2_image_report_target})
message(STATUS "SDL2_ttf 探索経路   : ${AMBIENT_SDL2_TTF_DISCOVERY}")
_ambient_report_lib_location("SDL2_ttf  " ${_ambient_sdl2_ttf_report_target})
message(STATUS "  ※ 3つの実体が別々の libSDL2 を指していないか目視すること。")
message(STATUS "  ※ 分かれている場合は 1プロセスに libSDL2 が2つ載る。README 参照。")
message(STATUS "----------------------------------------------------------------")

# --- libcurl ----------------------------------------------------------------
#
# 【重要】curl は pkg-config で探してはいけない。macOS SDK に古い .pc があり
# 7.64.1 という嘘のバージョンを返す（実体はシステムの 8.7.1）。
# find_package(CURL) は CMake 同梱の FindCURL が curl-config / ヘッダから
# 実際のバージョンを取るので正しい値になる。

find_package(CURL REQUIRED)

if(CURL_VERSION_STRING VERSION_LESS 8.0)
  message(FATAL_ERROR
    "libcurl ${CURL_VERSION_STRING} が見つかった。8.0 未満は想定外。\n"
    "pkg-config 経由で macOS SDK の古い .pc（7.64.1 と嘘をつく）を掴んでいる可能性が高い。\n"
    "find_package(CURL) の探索結果を確認すること。")
endif()

add_library(ambient_dep_curl INTERFACE)
target_link_libraries(ambient_dep_curl INTERFACE CURL::libcurl)

message(STATUS "libcurl             : ${CURL_VERSION_STRING}")
_ambient_report_lib_location("libcurl   " CURL::libcurl)
