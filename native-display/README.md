# native-display — GEOFRAMEアンビエント表示（macOS + SDL2）

## 位置づけ

これは[企画案（2026-07-30）](../docs/archive/native-display-concept.md)を出発点にした実装です。
実装契約と手順は [`docs/`](docs/) にあります。

**企画**: GeoFrame（写真・気象・地震・Home Assistant）と hogo-meter（保護猫のお世話記録）を
1台のアンビエント端末へ統合する。普段は家族の写真が流れ、必要な瞬間だけ生活の操作盤になる。

**重要**: 最終ターゲットは **M5Stack Tab5（ESP32-P4）** です。
Mac + SDL2 は完成品ではなく、**移植可能な C++ Core を検証するためのリファレンス実装**です。
画面体験・状態遷移・API 契約・演出性能をここで固め、Renderer だけ差し替えて Tab5 へ持っていきます。

### `mac-wkwebview/` とは別系統です

同じリポジトリに [`mac-wkwebview/`](../mac-wkwebview/) がありますが、**あちらとこちらは別の企画です**。

| | `native-display/`（ここ） | `mac-wkwebview/` |
|---|---|---|
| 企画 | MacSDL スマートディスプレイ | Cog/WPE + RPi400 スマートディスプレイ |
| 最終ターゲット | M5Stack Tab5 (ESP32-P4) | Raspberry Pi 400 |
| Mac 上での位置づけ | 移植可能な C++ Core の検証 | 既存 Web UI の先行検証 |
| 描画 | SDL2（将来 M5GFX） | WKWebView（将来 Cog/WPE） |
| 言語 | C++20 | Swift |

混同して片方の変更をもう片方へ持ち込まないこと。

---

## ドキュメント

- [`docs/CORE_CONTRACTS.md`](docs/CORE_CONTRACTS.md): Coreの状態・時刻・乱数・HTTP契約
- [`docs/PORTABILITY.md`](docs/PORTABILITY.md): ESP32-P4移植性ゲート
- [`docs/FIXTURES.md`](docs/FIXTURES.md): 公開可能な合成fixtureと再採取手順

---

## 層構造

**層分離を規約ではなくビルドエラーで強制**しています。

```
ambient_core        (STATIC)  依存: 標準ライブラリのみ
ambient_render_sdl  (STATIC)  依存: ambient_core + SDL2 / SDL2_image / SDL2_ttf
ambient_transport   (STATIC)  依存: ambient_core + libcurl
ambient_display     (EXE)     依存: 上記3つ
```

`ambient_core` は Tab5 へそのまま持っていく層です。この層の include path には
SDL のパスを**入れていない**ので、`#include <SDL.h>` と書いた瞬間にコンパイルが落ちます。
それが正しい状態です（`fatal error: 'SDL.h' file not found`）。

### `ambient_core` に書いてはいけないもの

| 禁止 | 理由 |
|---|---|
| `#include <SDL.h>` 等 | 描画は Renderer 層の責務。Tab5 では M5GFX に差し替わる |
| `std::filesystem` | ESP32 に存在しない。パス解決は app / render 層で |
| `std::thread` / `std::mutex` | スレッドは Renderer / Transport 層の責務。Core は単一スレッド前提 |
| `<format>` | 使えるがコードサイズを大きく増やす。Tab5 の flash 予算に効く |

依存を足したくなったら、それが本当に Core の責務か疑ってください。
大抵は render 層か transport 層に置くべきものです。

詳細は [`CMakeLists.txt`](CMakeLists.txt) の先頭コメントにも書いてあります。

---

## ビルド

### 必要なもの

```bash
brew install cmake ninja pkg-config sdl2 sdl2_image sdl2_ttf
```

libcurl は macOS 同梱のもの（8.7.1）を使うので追加インストール不要です。

### フォントと fixture の準備

フォント本体（9.5MB のバイナリ）と実写真はリポジトリにコミットしていません。

```bash
# 日本語フォント（Noto Sans JP, OFL 1.1）を取得。冪等なので何度実行してもよい
./native-display/tools/fetch-fonts.sh

# 合成テスト画像を生成（Pillow が必要）
source app/.venv/bin/activate
python3 native-display/tools/make-sample-fixture.py
```

fixture は**合成画像**です。実写真は個人データなのでコミットしません。
2560x1707（実写真と同じ 3:2）にしてあるのは、後続フェーズでメモリ予算を
実物と同じピクセル数で検証するためです。

### ビルドと実行

```bash
cmake -S native-display -B native-display/build -G Ninja
cmake --build native-display/build
./native-display/build/ambient_display
```

- **ESC** またはウィンドウの閉じるボタンで終了します。
- クリックすると論理座標がログに出ます。

### 主なオプション

```
--assets <dir>       アセットのルート
--font <path>        フォントファイル（.ttf / .ttc）
--font-index <n>     フォントの face index（.ttc 用、既定 0）
--photo <path>       全画面 cover 表示する画像
--window-size <WxH>  初期ウィンドウサイズ（16:9 以外にするとレターボックスを確認できる）
--screenshot <path>  1枚だけ描画して保存し終了（.png / .bmp）
```

`--screenshot` は後続フェーズの目視確認・回帰確認に使えます。

```bash
# 16:9（レターボックスなし）
./native-display/build/ambient_display --screenshot /tmp/shot.png

# 16:9 以外（上下に黒帯が入ることを確認できる）
./native-display/build/ambient_display --window-size 1000x900 --screenshot /tmp/tall.png
```

---

## 描画の約束事

### 論理解像度 1280x720

`SDL_RenderSetLogicalSize(renderer, 1280, 720)` を使っています。
ウィンドウをどう拡縮しても中身のアスペクト比は保たれ、余りはレターボックス（黒帯）になります。
**描画コードは常に 1280x720 の論理座標だけを見てください。**

### 入力座標は必ず `window_to_logical()` を通すこと

`Display::window_to_logical(window_x, window_y)` が、レターボックスのオフセット・
論理解像度への拡縮・macOS Retina のポイント/ピクセル差をまとめて吸収します。

**SDL のイベントが返す座標をそのまま使わないでください。** 黒帯ぶんズレます。
黒帯上をクリックすると論理解像度の範囲外の値（負値や 1280 超）が返るので、
呼び出し側で範囲チェックしてください。

起動時に四隅の変換結果を自己確認としてログへ出しています。16:9 のウィンドウなら
恒等写像、それ以外なら黒帯ぶんのズレが現れます。

Tab5 移植時はこの関数の中身がタッチパネルのキャリブレーションに置き換わります。
呼び出し側のコードは変えなくてよい、というのがこの seam の狙いです。

### フォントのパスとフェイスインデックスは設定値

`FontSpec { path, point_size, face_index }` で指定し、`TTF_OpenFontIndex()` を使っています。
将来 `.ttc`（1ファイルに複数 face を含む形式）へ差し替える可能性があるためです。
**ハードコードした `TTF_OpenFont()` に戻さないでください。** Tab5 でフォントを
差し替えるための contract です。

なお `NotoSansJP[wght].ttf` は可変フォントですが、SDL_ttf 2.24 は可変軸（wght）を
指定する API を持たないため、既定インスタンスの **Thin** が選ばれます。写真の上では
細すぎて読めないので、`FontSpec::synthetic_bold` で FreeType の合成ボールドを
掛けています。SDL_ttf 3.x へ上げるか静的ウェイトのフォントに差し替えれば不要になります。

### cover 表示

`cover_source_rect()` が、アスペクト比を保ったまま画面を埋め、はみ出しをクロップする
srcrect を計算します（`SDL_RenderCopy` の srcrect に渡す）。
2560x1707 (3:2) → 1280x720 (16:9) では上下が 133px / 134px クロップされます。

合成 fixture はこの挙動が目視で分かるように作ってあります。
**PASS 条件**: 赤/橙の帯とマーカー 1〜4 が消え、青/緑の帯が画面左右端に残ること。

---

## ⚠ SDL2-compat の注意（1プロセスに libSDL2 が2つ問題）

Homebrew の `sdl2` は **`sdl2-compat`（SDL3 の上に載る SDL2 互換層）へ移行済み**です。

```
brew info sdl2       → sdl2-compat: stable 2.32.70 ... Aliases: sdl2
/opt/homebrew/opt/sdl2 → ../Cellar/sdl2-compat/2.32.70 へのシンボリックリンク
実体は SDL3 3.4.12 が背後で動く
```

ここに落とし穴があります。`otool -L` で見ると:

| ライブラリ | 参照している libSDL2 | current version |
|---|---|---|
| `libSDL2_image` | `/opt/homebrew/opt/sdl2-compat/lib/libSDL2-2.0.0.dylib` | 3201.70.0（= 2.32.70 compat） |
| `libSDL2_ttf` | `/opt/homebrew/opt/sdl2/lib/libSDL2-2.0.0.dylib` | 3201.10.0（= 実 sdl2 2.32.8 でビルド） |

つまり **sdl2_ttf は実 sdl2 2.32.8 に対してビルドされている**のに、
`/opt/homebrew/opt/sdl2` のシンボリックリンクが sdl2-compat を指しているおかげで、
現状は全部が同一の dylib へ解決されて整合しています。

**誰かが `brew link --overwrite sdl2` を実行すると、この均衡が壊れます。**
`/opt/homebrew/opt/sdl2` が実 sdl2 2.32.8（`/opt/homebrew/Cellar/sdl2/2.32.8`、
現在は unlink 済み）を指すようになり、sdl2_ttf だけが実 sdl2 を、
sdl2_image が sdl2-compat を掴む → **1プロセスに libSDL2 が2つロードされます**。
SDL の内部状態が二重化するため、TTF 描画時のクラッシュなど理由不明の不具合が出ます。

### 検出方法

このアプリは起動時に次を全部ログへ出します。

- `SDL_GetVersion()` / `SDL_GetRevision()`
- SDL_image / SDL_ttf の**コンパイル時**バージョン（`SDL_IMAGE_VERSION` / `SDL_TTF_VERSION`）と
  **リンク時**バージョン（`IMG_Linked_Version()` / `TTF_Linked_Version()`）— 食い違ったら警告
- **dyld にロードされている `libSDL2` の実体を列挙**し、2つ以上なら強い警告

最後の dyld 列挙が**唯一この事故を検出できる検査**です。
コンパイル時 vs リンク時の比較は、二重ロード状態でも各ライブラリが自分の掴んでいる
SDL について矛盾のない値を返すため、素通りしてしまいます。

正常時のログ:

```
[sdl] SDL       compile=2.32.70 link=2.32.70
[sdl] SDL       revision=SDL-2.32.70-no-vcs
[sdl] SDL_image compile=2.8.12 link=2.8.12
[sdl] SDL_ttf   compile=2.24.0 link=2.24.0
[sdl] ロード済み libSDL2 #1: /opt/homebrew/Cellar/sdl2-compat/2.32.70/lib/libSDL2-2.0.0.dylib
```

`#2` が出たら壊れています。`brew unlink sdl2` で実 sdl2 を外し、sdl2-compat に統一してください。

CMake の configure 時にも、SDL2 / SDL2_image / SDL2_ttf の探索経路（CONFIG か pkg-config か）と
**シンボリックリンクを剥がした実体パス**を表示します。3つが別々の libSDL2 を指していないか
目視で確認してください。

### libcurl は pkg-config を使わないこと

`find_package(CURL REQUIRED)` を使っています。**pkg-config を使ってはいけません** —
macOS SDK に古い `.pc` があり `7.64.1` という嘘の値を返します（実体はシステムの 8.7.1）。
CMake 側で 8.0 未満なら `FATAL_ERROR` にして自己強制しています。

---

## セキュリティ運用前提

**接続先の GeoFrame server は認証がなく `0.0.0.0` にバインドしています。**

- **家庭内 LAN 限定の運用を前提**とします。
- **インターネットに露出させないこと。** ポート開放・DMZ・UPnP・トンネル公開はしないでください。
- hogo-meter の deviceId / secret は `native-display/.env`（`.gitignore` 済み）に置き、
  コミットしないでください。

---

## コミットしないもの

`.gitignore` で除外済みです。

- `native-display/build/`, `native-display/build-*/` — ビルド生成物
- `native-display/assets/fonts/*.ttf` 等 — フォント本体（`tools/fetch-fonts.sh` で取得）
- `native-display/.env` — hogo-meter の秘密情報
- `native-display/tests/data/` — 位置メタデータを含むフル fixture

`assets/fixtures/sample-photo.jpg` は**合成画像なのでコミットします**（プライバシー問題なし）。
