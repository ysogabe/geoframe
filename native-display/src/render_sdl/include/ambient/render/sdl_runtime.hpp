// SDL2 ランタイムの初期化とバージョン診断。
//
// 【なぜバージョン診断がここまで手厚いのか】
// Homebrew の `sdl2` は sdl2-compat（SDL3 の上に載る SDL2 互換層）へ移行済みで、
// sdl2_ttf は `/opt/homebrew/opt/sdl2/lib/libSDL2-2.0.0.dylib` を、
// sdl2_image は `/opt/homebrew/opt/sdl2-compat/lib/libSDL2-2.0.0.dylib` を
// それぞれ参照している。現状は前者のシンボリックリンクが後者へ向いているため
// 同一 dylib に解決されて整合しているが、誰かが `brew link --overwrite sdl2` を
// 実行すると sdl2_ttf だけが実 sdl2 2.32.8 を掴み、1プロセスに libSDL2 が
// 2つロードされる。この状態は SDL の内部状態が二重化するため何が起きても
// おかしくない（典型的には TTF 描画時のクラッシュや無反応）。
//
// そこで起動時に次を全部出す:
//   * SDL_GetVersion() / SDL_GetRevision()
//   * SDL_image / SDL_ttf のコンパイル時バージョンとリンク時バージョンの両方
//   * (macOS のみ) プロセスにロードされた libSDL2 の実体を dyld から列挙

#pragma once

#include <string>

namespace ambient::render {

// SDL / SDL_image / SDL_ttf のバージョンを標準エラーへ出力する。
// コンパイル時とリンク時が食い違っていたら警告を出す。
// 1プロセスに libSDL2 が複数ロードされていたら強い警告を出す。
//
// SDL_Init より前に呼んでも後に呼んでもよいが、SDL_Init 後のほうが
// dyld の列挙結果が正確になる（遅延ロードを拾えるため）。
void log_runtime_versions();

// SDL 本体 + SDL_image + SDL_ttf をまとめて初期化する。
// 失敗時は false を返し、error に人間が読めるメッセージを入れる。
[[nodiscard]] bool init_runtime(std::string& error);

// init_runtime で確保したものをまとめて解放する。
void shutdown_runtime() noexcept;

}  // namespace ambient::render
