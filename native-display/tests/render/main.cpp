// render_tests のエントリポイント。
//
// ★SDL_VIDEODRIVER=dummy で走らせる（CMakeLists で設定済み）。実ウィンドウを開かない。
//
// ★ピクセル完全一致を採らないこと。ソフトウェアレンダラと Metal で結果が differ し、
//   SDL のバージョン差でも変わる。代わりに**構造アサーション**を書くこと:
//     Slide            : progress=0.5 で左半分が旧・右半分が新
//     Wipe             : 境界が progress に線形追従
//     FadeThroughBlack : progress=0.5 で輝度最小
//   ただし「同一 seed で2回描いて完全一致」はピクセル比較でよい。
#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN
#include <doctest.h>
