# ambient_coreの移植性ゲート（ESP32-P4 / M5Stack Tab5）

## このドキュメントとゲートが食い違ったら

**ゲートを信じて、このドキュメントを直すこと。**

ここに書かれているのは、ある時点で `tools/check-esp-portability.sh` を実際に走らせて得た測定結果です。
ツールチェーンや上流パッケージが更新されれば結果は変わります。
「ドキュメントにこう書いてあるから大丈夫」は根拠になりません。手を動かして確かめた結果が正典です。

---

## 目的

`native-display/` は macOS + SDL2 の実装ですが、本当の目的は
**M5Stack Tab5 (ESP32-P4) へ移植可能な C++ Core (`ambient_core`) を作ること**です。
Mac 版は完成品ではなく、移植可能な Core を検証するための足場にすぎません。

したがって評価軸は「Mac で動く」より「**Tab5 でコンパイルできる形になっている**」が上位です。

移植の可否を最終フェーズまで検証しないと、そこで設計をやり直す羽目になります。
そこで**実機を一切触らずに、`ambient_core` が ESP32-P4 の実ツールチェーンで
コンパイルできることを継続的に検証するゲート**を用意しました。

```bash
./native-display/tools/check-esp-portability.sh
```

- リポジトリ内のどこから実行してもよい (スクリプト自身の位置からパスを解決する)
- 終了コード 0 = 成功 / 1 = 失敗。CI や QA ゲートにそのまま組み込める
- **実機に接続しない。`pio` も `esptool` も実行しない。**

### ゲートの射程 — ここが重要

このゲートは **コンパイルのみ** で、**リンクも実行もしません**。

- **通ること** = 依存と C++ 言語機能が ESP32-P4 に適合している
- **通ること ≠ Tab5 で動く**

リンク時にしか出ない問題 (シンボル不足)、実行時にしか出ない問題
(スタック不足、ヒープ枯渇、VFS の挙動、コードサイズ超過) は**検出できません**。
後述の `std::filesystem` がその代表例です。

この穴を少しでも埋めるため、コンパイルとは別立てで
**include の静的検査 (規約チェック)** を同じスクリプト内で実行しています (後述)。

### 検証範囲は `src/core/` のみ — `src/transport/` を検証しない理由

ゲートが探索するのは `src/core/` 配下だけです。
`src/transport/` や `src/render_sdl/` はコンパイルしません。**これは意図的です。**

「transport も Tab5 に載るのだから検証すべきでは」と考えたくなりますが、**当てはまりません**。
`ambient_transport` は

- Mac では **libcurl**
- Tab5 では **HTTPClient / WiFiClientSecure**

という**別々の実装**になる層です。同じソースが両方でコンパイルされることは設計上ありません。
したがって transport を P4 でコンパイルしようとすること自体が的外れで、
「同一ソースの移植性」という概念がそもそも適用できません。
同じ理由で `render_sdl` (SDL2 / Mac 専用) も対象外です。

**ただし、ここには重要なシグナルがあります。**

> 移植したい**純粋なポリシー**（送信キュー管理・リトライ規律・優先制御など、
> **I/O を含まない判断ロジック**）が `ambient_transport` に入っていたら、
> **それは配置ミスです。`ambient_core` へ移してください。**

純粋ロジックが core に置かれていれば、ゲートの射程は自動的に正しくなります。
逆に「transport の中身も検証したい」と感じたときは、
ゲートの範囲を広げるのではなく、**その中身が core にあるべきではないかを疑ってください。**
ゲートの範囲を広げるのは誤った対処です (Mac 専用実装を P4 でコンパイルしようとして失敗するだけ)。

---

## ツールチェーン — 最大の落とし穴

`~/.platformio/packages/` に **riscv32 の g++ が 2 つ**あり、バージョンが全く違います。

| パス | バージョン | 用途 |
|---|---|---|
| `.../toolchain-riscv32-esp/bin/riscv32-esp-elf-g++` | **GCC 8.4.0** (crosstool-NG esp-2021r2-patch5) | 古い espressif32@6.7.0 用。**C++20 が使えず誤った結論に至る** |
| `.../toolchain-riscv32-esp@src-775c29a2c405d3cb696522542d944d7c/bin/riscv32-esp-elf-g++` | **GCC 14.2.0** (crosstool-NG esp-14.2.0_20251107) | **ESP32-P4 / Tab5 用。これが正しい** |

`PATH` 解決に任せると前者を引く可能性があります。
そのためスクリプトは**絶対パスで pin** し、起動時に `-dumpversion` が
`14.2.0` と厳密一致するかを検査し、違えば理由を明示して即 `exit 1` します。

実測した誤検出時の出力:

```
[FATAL] 間違ったツールチェーンを掴んでいます。
    検出バージョン: '8.4.0'
    バナー        : riscv32-esp-elf-g++ (crosstool-NG esp-2021r2-patch5) 8.4.0
    期待バージョン: 14.2.0
```

---

## コンパイルフラグ

Tab5 のボード定義 `m5stack-tab5-p4.json` は `"chip_variant": "esp32p4_es"` を指定します。
したがって実ビルドで適用されるフラグの出典は:

```
~/.platformio/packages/framework-arduinoespressif32-libs/esp32p4_es/flags/cpp_flags
```

**スクリプトはこのファイルを毎回読み込みます。ハードコードしていません。**
上流パッケージが更新されればフラグも追従すべきだからです。
ファイルが存在しない場合は、**緩いフラグでフォールバックせず失敗させます**
(フォールバックするとゲートが嘘をつくため)。

実測した内容 (1 行、27 フラグ。スクリプトはここに `-Os` を足すので計 28 になり、
ゲートの出力は「フラグ数 : 28」と表示されます):

```
-march=rv32imafc_zicsr_zifencei_xesploop_xespv -mabi=ilp32f -ffunction-sections -fdata-sections
-Wno-error=unused-function -Wno-error=unused-variable -Wno-error=unused-but-set-variable
-Wno-error=deprecated-declarations -Wno-error=extra -Wno-unused-parameter -Wno-sign-compare
-Wno-enum-conversion -gdwarf-4 -ggdb -nostartfiles -freorder-blocks -Wwrite-strings
-fstack-protector -fstrict-volatile-bitfields -fno-jump-tables -fno-tree-switch-conversion
-march=rv32imafc_zicsr_zifencei_xesppie -std=gnu++2b -fexceptions -fno-rtti -fuse-cxa-atexit
-std=gnu++2a
```

読み取れる重要な事実:

- **`-std=` が 2 回現れ、後勝ちで `-std=gnu++2a` = 実効 C++20**
- **`-fexceptions`** (例外 有効) / **`-fno-rtti`** (RTTI 無効)
- `-march` も 2 回現れ、後勝ちで `-march=rv32imafc_zicsr_zifencei_xesppie`
- `-mabi=ilp32f`

`-Os` は `cpp_flags` に含まれないため、スクリプト側で明示的に追加しています。
出典は `esp32p4_es/sdkconfig` の `CONFIG_COMPILER_OPTIMIZATION_SIZE=y` です。

**`-Os` はファイル由来のフラグより後ろに置くこと (現在の実装もそうなっています)。**
GCC は `-O` 系も後勝ちのため、将来上流が `cpp_flags` に `-Og` や `-O2` を足しても
こちらの `-Os` が勝ち、実機の最適化レベルと一致し続けます。
`-std=` や `-march=` が同じファイル内で既に後勝ちで解決されているのと同じ理屈です。
善意で先頭へ移動しないでください。

**`-Wall` / `-Wextra` / `-Werror` は意図的に足していません。**
実ビルドに `-Werror` が無い以上、実ビルドより厳しいゲートは偽の失敗を生み、
やがて誰にも信用されなくなるからです。

### 実測したプリプロセッサマクロ

上記フラグ + `-Os` で `-dM -E` した結果:

```
#define __cplusplus 202002L    <- C++20
#define __EXCEPTIONS 1         <- 例外 有効
#define __riscv_xlen 32
#define __riscv_flen 32
```

`__GXX_RTTI` は **定義されない** (= RTTI 無効)。

対応する `sdkconfig` の実測値:

- `CONFIG_COMPILER_CXX_EXCEPTIONS=y`
- `# CONFIG_COMPILER_CXX_RTTI is not set`
- `CONFIG_COMPILER_OPTIMIZATION_SIZE=y`
- `CONFIG_COMPILER_CXX_EXCEPTIONS_EMG_POOL_SIZE=0`

---

## `ambient_core` に書いてよいもの / いけないもの

以下の判定は、実際にゲートを通した**測定結果**が根拠です。
`検証` 列は「ゲートが自動で検出できるか」を表します。

| 項目 | 可否 | コンパイルゲートの実測結果 | 検証 |
|---|---|---|---|
| 標準ライブラリ (`<string>` `<vector>` `<map>` `<memory>` `<optional>` `<algorithm>`) | **可** | **PASS** | 自動 (コンパイル) |
| C++20 (`<span>` / `<ranges>` / concepts) | **可** | **PASS** | 自動 (コンパイル) |
| 例外 (`try` / `catch` / `throw`) | コンパイルは通る (下記の注意を読むこと) | **PASS** | 自動 (コンパイル) |
| `std::filesystem` | **使用禁止** | **PASS してしまう** | **include は自動検出 (規約チェック) / 実使用はレビュー** |
| `std::thread` / `std::mutex` / `<future>` / `<condition_variable>` | **Core に置かない** | PASS してしまう | **include は自動検出 (規約チェック) / 実使用はレビュー** |
| `<format>` | 可。ただし避ける | PASS | 自動 (規約チェックで**警告のみ**。失敗にはしない) |
| RTTI (`typeid` / `dynamic_cast`) | **使用不可** | **FAIL** | 自動 (コンパイル) |
| SDL / Mac 固有 API | **使用不可** | **FAIL** | 自動 (コンパイル) |
| 大きな静的バッファ / 深い再帰 | 避ける | 検出不可 | レビューで担保 |
| `std::vector` 等の動的確保 | 可 (PSRAM あり) | PASS | — |
| 写真のピクセルデータを Core に持たせる | **禁止** | 検出不可 | レビューで担保 |

「include は自動検出 / 実使用はレビュー」の意味: `#include <filesystem>` のように
**ヘッダを取り込んだ事実**は機械的に拾えますが、
別ヘッダ経由で間接的に使う等の**実使用**までは追えません。そこはレビューの担保です。

### 詳細

#### `std::filesystem` — 使用禁止だが、ゲートは検出できない

**測定結果: コンパイルは PASS する。** `#include` だけでなく
`std::filesystem::path` / `exists()` / `directory_iterator` を実際に使っても通りました。

理由を追跡した結果:

- ヘッダは実在する: `.../riscv32-esp-elf/include/c++/14.2.0/filesystem`
- `libstdc++.a` (rv32imafc_zicsr_zifencei/ilp32f 版) に **filesystem 関連シンボルが 633 個定義済み**
  (`_ZNKSt10filesystem18directory_iteratordeEv` 等)

つまり**リンクすら通ってしまう**可能性が高く、問題が出るのは実行時
(ESP-IDF の VFS 層が POSIX セマンティクスを完全には満たさない) とコードサイズです。

**コンパイルゲートを嘘つきにしないため、無理に失敗させる細工 (ヘッダ差し替え等) は入れていません。**
コンパイル結果は測定したままを報告します。

そのうえで、**別立ての規約チェック**が `#include <filesystem>` という記述自体を検出し、
違反として `exit 1` にします (後述)。
コンパイル結果を偽装するのではなく、独立したセクションとして
「コンパイルは通るが規約違反」と報告する形にしてあります。
ただし間接的な実使用までは追えないため、最終的な担保はレビューです。

**規約: ファイル I/O は `IStateStore` 抽象の背後に隔離すること。**
Core はインターフェース越しにしか永続化に触れない。Mac 側は `std::filesystem` で実装してよいが、
それは Core の外 (Platform 層) に置く。

#### RTTI — 使用不可 (ゲートが自動で弾く)

`-fno-rtti` のため、実測で両方とも FAIL しました:

```
error: cannot use 'typeid' with '-fno-rtti'
error: 'dynamic_cast' not permitted with '-fno-rtti'
```

型で分岐したくなったら、仮想関数、`std::variant` + `std::visit`、
あるいは自前のタグ enum を使うこと。

#### 例外 — 測定結果と、それとは別の運用判断

2 つを混ぜないよう、分けて記載します。

**測定結果 (機械的事実):**
`try` / `catch` / `throw` を含むコードは **PASS** しました。
`-fexceptions` が有効で `__EXCEPTIONS 1` が定義され、`CONFIG_COMPILER_CXX_EXCEPTIONS=y` です。
**つまり例外はコンパイルが通ります。ゲートは例外を弾きません。**

**運用上の助言 (測定結果ではなく判断):**
`CONFIG_COMPILER_CXX_EXCEPTIONS_EMG_POOL_SIZE=0` すなわち**緊急用プールが 0** です。
ヒープ枯渇時に `throw` すると例外オブジェクトを確保できず `abort()` に至ります。
加えて例外は組込みではコードサイズを押し上げます。
よって **Core の制御フローに例外を使わないこと**を推奨します。
戻り値、`std::optional`、`std::expected` 相当の自前型で表現するほうが安全です。
(これは禁止ではなく設計判断です。ゲートは通します。)

#### `std::thread` / `std::mutex` — Core に置かない

スレッドは Renderer / Transport 層の責務です。
Core は「呼ばれたら計算して返す」純粋なロジックに保ち、並行性を持ち込まないこと。
Tab5 側では FreeRTOS のタスクが対応物になるため、Core が `std::thread` を前提にすると移植で破綻します。

#### `<format>` — 使えるが避ける

禁止ではありませんが、コードサイズを大きく増やします。
組込み側の余裕を考えると、Core では素朴な文字列連結や `snprintf` 相当で足りることが多いはずです。

#### 大きな静的バッファ・深い再帰 — 避ける

Tab5 のスタックは Mac より遥かに小さい (FreeRTOS タスクごとに数 KB 程度) です。
`char buf[64 * 1024]` のようなローカル配列や、入力サイズに比例する再帰は避けること。
**ゲートはこれを検出できません** (コンパイルは通る)。レビューで担保します。

#### 動的確保 — 可。ただし写真データは Core に持たせない

Tab5 には PSRAM があるため `std::vector` 等の動的確保は使えます。
ただし**写真のピクセルデータを Core に持たせないこと**。
画像バッファはサイズが桁違いで、確保戦略もプラットフォーム依存 (PSRAM / DMA 対応領域) です。
Core は画像の**識別子とメタデータ**だけを扱い、実体は Renderer 層に置くこと。

---

## 規約チェック (include の静的検査)

コンパイルゲートとは**別立て**で、`src/core/` 配下の `*.cpp` / `*.h` / `*.hpp` を対象に
禁止 include を grep で検出します。同じスクリプトが実行し、**出力は別セクション**になります。

レビューは必ず漏れるので、機械的に拾えるものは拾う、という趣旨です。
コンパイル結果には一切手を触れません。

| include | 扱い | 終了コード |
|---|---|---|
| `<filesystem>` | **違反** | `exit 1` |
| `<thread>` `<mutex>` `<future>` `<condition_variable>` | **違反** | `exit 1` |
| `<format>` | **警告のみ** (禁止ではない) | `exit 0` のまま |

`.cpp` が 0 本でもこのチェックは走ります (ヘッダだけで違反しうるため)。

### 誤検出の逃げ道

行末に `// portability-ok: <理由>` を書くと、その行は除外されます。

```cpp
#include <mutex>  // portability-ok: このヘッダはテスト用フィクスチャからのみ include される
```

**理由の記述は必須です。** `// portability-ok:` だけの空の抑止コメントは無効で、
違反のまま扱われ、「理由が書かれていないため無効」と明示されます。
空白のみの理由も無効です。抑止を無言で積み上げられないようにするためです。

判定は素直な正規表現で、行頭の空白・`#  include`・`#include<tab><...>` といった
書式のゆらぎは吸収します。コメント内や文字列内の偽陽性はある程度許容する方針で、
凝った parser は持ち込んでいません (誤検出は上記の逃げ道で処理できるため)。

## ゲートの実装で意図的に選んだこと

| 選択 | 理由 |
|---|---|
| `-c` を使う (`-fsyntax-only` ではない) | コード生成まで走るため検出力が高い |
| 出力は `/dev/null` へ。リンクしない | 目的はコンパイル適合性の確認のみ |
| ESP-IDF / Arduino の include パスを渡さない | `ambient_core` は標準ライブラリのみに依存する設計。フレームワークのヘッダが必要になったら**それ自体が設計違反のシグナル**なので、そのまま失敗させる |
| include パスは実在するものだけ動的に追加 | Core のディレクトリ構成は別担当が決めるため。存在しないパスで「ヘッダが無い」と誤検出すると、フレームワークの include を足したくなり設計が壊れる |
| ソース 0 本でも `exit 0`。ただし警告を大きく出す | 未着手を失敗にはしないが、黙って緑にすると「ゲートが動いていないのに通っている」ように見えるため |
| フラグファイル不在時にフォールバックしない | ゲートが嘘をつくのを防ぐ |
| 1 本失敗しても全ソースを最後までコンパイルする | 失敗を一度にまとめて把握できるようにするため |

### 環境変数 (通常は不要)

- `ESP_CXX` — コンパイラの絶対パスを上書き
- `ESP_CPP_FLAGS_FILE` — `cpp_flags` の絶対パスを上書き

上書きしても**バージョン検査は必ず走る**ため、ゲートを緩めることはできません。
別マシンでパスが異なる場合の逃げ道として用意しています。

---

## ゲート自体の自己検証について

このゲートが「緑なのに壊れている」状態にならないよう、
スクリプト本体 (探索・集約・終了コード) を一時的な足場でテストしています。

検証済みの挙動 (コンパイルゲート):

- 正常ソースのみ → `exit 0`
- 正常 + 異常が混在 → `exit 1`。**成功したファイルも列挙したうえで**失敗ファイル名とエラー全文を表示
- `src/core/` のサブディレクトリも再帰的に探索する
- `src/core/` が空、または不在 → 警告を大きく出して `exit 0`
- 誤ツールチェーン (GCC 8.4.0) / `cpp_flags` 不在 / コンパイラ不在 → それぞれ理由を明示して `exit 1`
- リポジトリ外の任意ディレクトリから実行しても正しく解決する

検証済みの挙動 (規約チェック):

- `<filesystem>` / `<thread>` / `<mutex>` / `<future>` / `<condition_variable>` を検出 → `exit 1`
- 書式のゆらぎ (行頭インデント、`#  include`、`#include<tab><...>`) も検出する
- `<format>` のみ → 警告を出すが `exit 0` のまま
- `// portability-ok: <理由>` 付きの行は除外され、抑止件数として表示される
- 理由が空 / 空白のみの抑止コメントは**無効**で、違反のまま `exit 1`
- `.cpp` が 0 本でヘッダだけの場合も違反を検出して `exit 1`

検証用のフィクスチャは `native-display/` の外 (一時領域) に置き、検証後に削除しています。
`src/core/` にテスト用ファイルを置くことはしません (Core の実装担当と衝突するため)。
