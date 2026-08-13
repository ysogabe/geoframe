#!/usr/bin/env bash
#
# check-esp-portability.sh -- ambient_core の ESP32-P4 (M5Stack Tab5) 移植性ゲート
#
# 目的:
#   native-display/src/core/ 配下の ambient_core ソースを、Tab5 実機で実際に使われる
#   ツールチェーンとコンパイルフラグでコンパイルできるか検証する。
#   実機接続もリンクも実行もしない。コンパイルが通れば「依存」と「C++ 言語機能」の
#   適合は取れる、という割り切ったゲート。
#
#   詳細と、ambient_core に書いてよいもの/いけないものは docs/PORTABILITY.md を参照。
#
# 使い方:
#   ./native-display/tools/check-esp-portability.sh
#   (リポジトリ内のどこから実行してもよい。スクリプト自身の位置からパスを解決する)
#
# 終了コード:
#   0 = 全ソースがコンパイル成功 (または検証対象 0 本)
#   1 = コンパイル失敗、または環境の健全性チェック失敗
#
# 環境変数 (通常は指定不要):
#   ESP_CXX             コンパイラの絶対パスを上書き
#   ESP_CPP_FLAGS_FILE  cpp_flags ファイルの絶対パスを上書き
#   上書きしてもバージョン検査は必ず実行されるため、ゲートを緩めることはできない。
#
set -euo pipefail

# ---------------------------------------------------------------------------
# ピン留めしたツールチェーン
#
# 注意: ~/.platformio/packages/ には riscv32 の g++ が 2 つある。
#   toolchain-riscv32-esp/                       -> GCC 8.4.0  (古い。C++20 が使えない)
#   toolchain-riscv32-esp@src-775c29a2.../       -> GCC 14.2.0 (ESP32-P4 / Tab5 用。正しい)
# PATH 解決に任せると前者を引いて誤った結論に至るので、絶対パスで固定する。
#
# パスは $HOME 起点にする (特定ユーザーのホームを焼き込まない)。
# ただし末尾の @src-775c29a2... はパッケージのバージョンピンなので残すこと。
# 別マシンでレイアウトが違う場合は ESP_CXX / ESP_CPP_FLAGS_FILE で上書きする。
# ---------------------------------------------------------------------------
DEFAULT_CXX="$HOME/.platformio/packages/toolchain-riscv32-esp@src-775c29a2c405d3cb696522542d944d7c/bin/riscv32-esp-elf-g++"

# Tab5 のボード定義 m5stack-tab5-p4.json は "chip_variant": "esp32p4_es" を指定するため、
# 実ビルドで適用されるフラグはこのファイルにある。ハードコードせず必ず読み込む。
DEFAULT_FLAGS_FILE="$HOME/.platformio/packages/framework-arduinoespressif32-libs/esp32p4_es/flags/cpp_flags"

REQUIRED_GCC_VERSION="14.2.0"

CXX="${ESP_CXX:-$DEFAULT_CXX}"
FLAGS_FILE="${ESP_CPP_FLAGS_FILE:-$DEFAULT_FLAGS_FILE}"

# ---------------------------------------------------------------------------
# 表示ヘルパ
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
  C_RED=$'\033[0;31m'; C_GRN=$'\033[0;32m'; C_YEL=$'\033[0;33m'
  C_CYN=$'\033[0;36m'; C_BLD=$'\033[1m';    C_RST=$'\033[0m'
else
  C_RED=''; C_GRN=''; C_YEL=''; C_CYN=''; C_BLD=''; C_RST=''
fi

die() {
  printf '%s\n' "${C_RED}${C_BLD}[FATAL]${C_RST} $*" >&2
  printf '%s\n' "${C_RED}移植性ゲートを実行できません。詳細は native-display/docs/PORTABILITY.md${C_RST}" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# パス解決 (スクリプト自身の位置から)
#   <root>/tools/check-esp-portability.sh  ->  <root> = native-display/
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_DIR="$ROOT_DIR/src"
CORE_DIR="$SRC_DIR/core"

printf '%s\n' "${C_BLD}=== ambient_core ESP32-P4 移植性チェック ===${C_RST}"
printf '%s\n' "対象ルート: $ROOT_DIR"
printf '\n'

# ---------------------------------------------------------------------------
# 健全性チェック (どれか失敗したら即 exit 1)
# ---------------------------------------------------------------------------

# 1. コンパイラが存在し実行可能か
if [ ! -x "$CXX" ]; then
  die "ESP32-P4 用コンパイラが見つからない、または実行可能でない:
    $CXX
  PlatformIO の esp32p4 ツールチェーンが未インストールの可能性があります。
  (このスクリプトは pio コマンドを実行しません。手動でご確認ください)"
fi

# 2. バージョンが 14.2.0 か
#    --version のバナー文字列 grep ではなく -dumpversion で厳密一致させる。
#    パイプは使わない (set -o pipefail 下で SIGPIPE により誤検知するため)。
actual_version="$("$CXX" -dumpversion 2>/dev/null || true)"
if [ "$actual_version" != "$REQUIRED_GCC_VERSION" ]; then
  version_banner_full="$("$CXX" --version 2>&1 || true)"
  version_banner="${version_banner_full%%$'\n'*}"
  die "間違ったツールチェーンを掴んでいます。
    使用パス      : $CXX
    検出バージョン: '${actual_version:-(取得失敗)}'
    バナー        : ${version_banner:-(取得失敗)}
    期待バージョン: $REQUIRED_GCC_VERSION

  GCC 8.4.0 は古い espressif32@6.7.0 用のツールチェーンで、C++20 が使えません。
  これで検証すると『Tab5 で動かない』という誤った結論に至ります。
  ESP32-P4 / Tab5 用は次のパスです:
    $DEFAULT_CXX"
fi

# 3. cpp_flags ファイルが存在するか
#    存在しない場合に緩いフラグへフォールバックしてはいけない。
#    ゲートが嘘をつくことになるため、必ず落とす。
if [ ! -f "$FLAGS_FILE" ]; then
  die "コンパイルフラグのファイルが見つかりません:
    $FLAGS_FILE
  Tab5 実機と同じフラグで検証できないため中断します。
  (安全側に倒すため、緩いフラグでのフォールバックは意図的に実装していません)"
fi

# ---------------------------------------------------------------------------
# フラグの読み込み
#   実ビルドと同じフラグを必ずファイルから読む (ハードコードしない)。
#   set -f で glob 展開を止めたうえで単語分割させる。
#   ファイルが将来複数行になっても IFS に改行が含まれるため正しく分割される。
# ---------------------------------------------------------------------------
flags_raw="$(cat "$FLAGS_FILE")"
set -f
# shellcheck disable=SC2206
CPP_FLAGS=( $flags_raw )
set +f

if [ ${#CPP_FLAGS[@]} -eq 0 ]; then
  die "cpp_flags ファイルが空です: $FLAGS_FILE"
fi

# -Os は cpp_flags には含まれない。sdkconfig の CONFIG_COMPILER_OPTIMIZATION_SIZE=y 由来
# なので、ここで明示的に足す。
CPP_FLAGS[${#CPP_FLAGS[@]}]="-Os"

# 注意: これ以外のフラグ (-Wall / -Wextra / -Werror 等) は足さないこと。
# 実ビルドより厳しいゲートは偽の失敗を生み、やがて無視されるようになる。

printf '%s\n' "${C_GRN}[OK]${C_RST}    コンパイラ  : $CXX"
printf '%s\n' "        バージョン  : $actual_version"
printf '%s\n' "${C_GRN}[OK]${C_RST}    フラグ出典  : $FLAGS_FILE"
printf '%s\n' "        フラグ数    : ${#CPP_FLAGS[@]} (-Os を含む)"

# ---------------------------------------------------------------------------
# include パス
#   ESP-IDF / Arduino のフレームワーク include パスは意図的に一切渡さない。
#   ambient_core は設計上、標準ライブラリのみに依存する。
#   フレームワークのヘッダが必要になったらそれ自体が設計違反のシグナルなので、
#   そのまま失敗させる。
#
#   ambient_core のディレクトリ構成は別担当が決めるため、実在するものだけを渡す。
# ---------------------------------------------------------------------------
INCLUDE_ARGS=()
for candidate in "$SRC_DIR" "$CORE_DIR" "$CORE_DIR/include" "$SRC_DIR/include" "$ROOT_DIR/include"; do
  if [ -d "$candidate" ]; then
    INCLUDE_ARGS[${#INCLUDE_ARGS[@]}]="-I$candidate"
  fi
done

# ---------------------------------------------------------------------------
# 検証対象ソースの探索
# ---------------------------------------------------------------------------
SOURCES=()
if [ -d "$CORE_DIR" ]; then
  while IFS= read -r -d '' f; do
    SOURCES[${#SOURCES[@]}]="$f"
  done < <(find "$CORE_DIR" -type f -name '*.cpp' -print0 | LC_ALL=C sort -z)
fi

# 検証対象 0 本: 成功扱い (exit 0) だが、黙って緑にすると
# 「ゲートが動いていないのに通っている」ように見えるので目立たせる。
if [ ${#SOURCES[@]} -eq 0 ]; then
  printf '\n'
  printf '%s\n' "${C_YEL}${C_BLD}+--------------------------------------------------------------+${C_RST}"
  printf '%s\n' "${C_YEL}${C_BLD}|  警告: 検証対象 0 本 -- ゲートは何も検証していません          |${C_RST}"
  printf '%s\n' "${C_YEL}${C_BLD}+--------------------------------------------------------------+${C_RST}"
  printf '%s\n' "${C_YEL}  $CORE_DIR/**/*.cpp が 1 本も見つかりませんでした。${C_RST}"
  if [ ! -d "$CORE_DIR" ]; then
    printf '%s\n' "${C_YEL}  (ディレクトリ自体が存在しません。ambient_core は未着手のようです)${C_RST}"
  fi
  printf '%s\n' "${C_YEL}  この緑は『移植可能』を意味しません。単に検証対象が無いだけです。${C_RST}"
  printf '%s\n' "${C_YEL}  (ヘッダのみでも規約チェックは下で実行します)${C_RST}"
else
  printf '%s\n' "${C_GRN}[OK]${C_RST}    検証対象    : ${#SOURCES[@]} 本"
  if [ ${#INCLUDE_ARGS[@]} -gt 0 ]; then
    printf '%s\n' "        include     : ${INCLUDE_ARGS[*]}"
  fi
fi

# ---------------------------------------------------------------------------
# 1 本ずつコンパイル
#   -fsyntax-only ではなく -c を使う (コード生成まで走るので検出力が高い)。
#   出力は捨てる。リンクも実行もしない。
# ---------------------------------------------------------------------------
pass_count=0
FAILED_FILES=()
FAILED_LOGS=()

if [ ${#SOURCES[@]} -gt 0 ]; then
printf '\n'
printf '%s\n' "${C_CYN}--- コンパイル (リンクはしない) ---${C_RST}"

for src in "${SOURCES[@]}"; do
  rel="${src#"$ROOT_DIR"/}"

  # set -e 下でも継続できるよう if で包む (包まないと最初の失敗で
  # サマリを出さずに落ちてしまう)。
  if compile_log="$("$CXX" "${CPP_FLAGS[@]}" ${INCLUDE_ARGS[@]+"${INCLUDE_ARGS[@]}"} \
                          -c -o /dev/null "$src" 2>&1)"; then
    printf '%s\n' "  ${C_GRN}PASS${C_RST}  $rel"
    if [ -n "$compile_log" ]; then
      # 警告のみ (実ビルドに -Werror が無いため失敗にはしない)
      printf '%s\n' "$compile_log" | sed 's/^/          /'
    fi
    pass_count=$(( pass_count + 1 ))
  else
    printf '%s\n' "  ${C_RED}FAIL${C_RST}  $rel"
    FAILED_FILES[${#FAILED_FILES[@]}]="$rel"
    FAILED_LOGS[${#FAILED_LOGS[@]}]="$compile_log"
  fi
done
fi   # ${#SOURCES[@]} -gt 0

fail_count=${#FAILED_FILES[@]}

# ---------------------------------------------------------------------------
# 規約チェック (grep ベース) -- コンパイルゲートとは別物
#
#   コンパイルは通ってしまうが ESP32 では問題になるものを、include の有無だけで
#   機械的に拾う。実使用の検出まではしない (そこはレビューの担保)。
#
#   これはコンパイル結果を偽装するものではない。コンパイル結果には手を触れず、
#   「コンパイルは通るが規約違反」を独立したセクションとして報告する。
#
#   誤検出の逃げ道: 行末に `// portability-ok: <理由>` を書くとその行は除外する。
#   理由が空の抑止コメントは無効 (違反のまま扱う)。
#
#   ヘッダも対象にするため、.cpp が 0 本でもこのチェックは実行される。
# ---------------------------------------------------------------------------
CONV_DENY_RE='filesystem|thread|mutex|future|condition_variable'
CONV_WARN_RE='format'
CONV_INC_RE='^[[:space:]]*#[[:space:]]*include[[:space:]]*<'
CONV_OK_RE='//[[:space:]]*portability-ok:[[:space:]]*[^[:space:]]'
CONV_OK_BARE_RE='//[[:space:]]*portability-ok:'

CONV_FILES=()
if [ -d "$CORE_DIR" ]; then
  while IFS= read -r -d '' f; do
    CONV_FILES[${#CONV_FILES[@]}]="$f"
  done < <(find "$CORE_DIR" -type f \
             \( -name '*.cpp' -o -name '*.h' -o -name '*.hpp' \) -print0 \
           | LC_ALL=C sort -z)
fi

VIOLATIONS=()
CONV_WARNINGS=()
suppressed_count=0

for f in ${CONV_FILES[@]+"${CONV_FILES[@]}"}; do
  conv_rel="${f#"$ROOT_DIR"/}"

  # -- 禁止 include --
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    hit_line="${hit%%:*}"
    hit_text="${hit#*:}"
    hit_trim="${hit_text#"${hit_text%%[![:space:]]*}"}"
    if [[ "$hit_text" =~ $CONV_OK_RE ]]; then
      suppressed_count=$(( suppressed_count + 1 ))
    elif [[ "$hit_text" =~ $CONV_OK_BARE_RE ]]; then
      VIOLATIONS[${#VIOLATIONS[@]}]="$conv_rel:$hit_line: $hit_trim
        -> 抑止コメントに理由が書かれていないため無効です。'// portability-ok: <理由>' の形で理由を書いてください。"
    else
      VIOLATIONS[${#VIOLATIONS[@]}]="$conv_rel:$hit_line: $hit_trim"
    fi
  done < <(grep -nE "${CONV_INC_RE}(${CONV_DENY_RE})>" "$f" 2>/dev/null || true)

  # -- 警告のみ (禁止ではない) --
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    hit_line="${hit%%:*}"
    hit_text="${hit#*:}"
    hit_trim="${hit_text#"${hit_text%%[![:space:]]*}"}"
    if [[ "$hit_text" =~ $CONV_OK_RE ]]; then
      suppressed_count=$(( suppressed_count + 1 ))
    else
      CONV_WARNINGS[${#CONV_WARNINGS[@]}]="$conv_rel:$hit_line: $hit_trim"
    fi
  done < <(grep -nE "${CONV_INC_RE}(${CONV_WARN_RE})>" "$f" 2>/dev/null || true)
done

violation_count=${#VIOLATIONS[@]}
warn_count=${#CONV_WARNINGS[@]}

printf '\n'
printf '%s\n' "${C_CYN}--- 規約チェック (コンパイルとは別。include の静的検査) ---${C_RST}"
printf '%s\n' "  対象   : ${#CONV_FILES[@]} ファイル (*.cpp / *.h / *.hpp)"
if [ "$suppressed_count" -gt 0 ]; then
  printf '%s\n' "  抑止済 : ${suppressed_count} 件 (// portability-ok: 付き)"
fi

if [ "$violation_count" -eq 0 ] && [ "$warn_count" -eq 0 ]; then
  printf '%s\n' "  ${C_GRN}規約違反なし${C_RST}"
fi

if [ "$warn_count" -gt 0 ]; then
  printf '\n'
  printf '%s\n' "${C_YEL}${C_BLD}[WARN] 使用可能だが推奨しない include (失敗にはしません):${C_RST}"
  for w in ${CONV_WARNINGS[@]+"${CONV_WARNINGS[@]}"}; do
    printf '%s\n' "${C_YEL}  $w${C_RST}"
  done
  printf '%s\n' "${C_YEL}  <format> は使えますがコードサイズを増やします。組込み側の余裕を考えて判断してください。${C_RST}"
fi

if [ "$violation_count" -gt 0 ]; then
  printf '\n'
  printf '%s\n' "${C_RED}${C_BLD}[規約違反] コンパイルは通りますが、ambient_core に置いてはいけない依存です:${C_RST}"
  for v in ${VIOLATIONS[@]+"${VIOLATIONS[@]}"}; do
    printf '%s\n' "${C_RED}  $v${C_RST}"
  done
  printf '\n'
  printf '%s\n' "${C_YEL}  なぜコンパイルが通るのに違反なのか:${C_RST}"
  printf '%s\n' "${C_YEL}    <filesystem>          -> ESP32 の VFS が POSIX セマンティクスを満たさない。"
  printf '%s\n' "                             ファイル I/O は IStateStore 抽象の背後へ隔離すること。"
  printf '%s\n' "    <thread> <mutex>      -> 並行性は Renderer / Transport 層の責務。Core は"
  printf '%s\n' "    <future> <condition_variable>  『呼ばれたら計算して返す』純粋なロジックに保つこと。"
  printf '%s\n' "                             Tab5 側は FreeRTOS タスクが対応物になるため前提が崩れる。${C_RST}"
  printf '\n'
  printf '%s\n' "${C_YEL}  誤検出の場合は行末に '// portability-ok: <理由>' を付けてください (理由は必須)。${C_RST}"
fi

# ---------------------------------------------------------------------------
# 総合サマリ
# ---------------------------------------------------------------------------
printf '\n'
printf '%s\n' "${C_CYN}--- 結果 ---${C_RST}"
printf '%s\n' "  コンパイル : 合計 ${#SOURCES[@]} 本 / 成功 ${C_GRN}${pass_count}${C_RST} 本 / 失敗 ${C_RED}${fail_count}${C_RST} 本"
printf '%s\n' "  規約       : 違反 ${C_RED}${violation_count}${C_RST} 件 / 警告 ${C_YEL}${warn_count}${C_RST} 件"

if [ "$fail_count" -gt 0 ]; then
  printf '\n'
  printf '%s\n' "${C_RED}${C_BLD}[FAIL] ESP32-P4 でコンパイルできないファイルがあります:${C_RST}"
  i=0
  while [ "$i" -lt "$fail_count" ]; do
    printf '\n'
    printf '%s\n' "${C_RED}${C_BLD}### ${FAILED_FILES[$i]}${C_RST}"
    printf '%s\n' "${FAILED_LOGS[$i]}" | sed 's/^/    /'
    i=$(( i + 1 ))
  done

  printf '\n'
  printf '%s\n' "${C_YEL}ヒント: ambient_core は標準ライブラリのみに依存する設計です。"
  printf '%s\n' "  - ヘッダが見つからない  -> ESP32 に存在しないヘッダを使っていないか、"
  printf '%s\n' "                             Core の外へ隔離すべき依存でないか確認"
  printf '%s\n' "  - SDL / Mac 固有 API    -> Core ではなく Renderer / Platform 層へ"
  printf '%s\n' "  - typeid / dynamic_cast -> -fno-rtti のため使用不可"
  printf '%s\n' "  詳細は native-display/docs/PORTABILITY.md${C_RST}"
fi

if [ "$fail_count" -eq 0 ] && [ "$violation_count" -eq 0 ]; then
  printf '\n'
  if [ ${#SOURCES[@]} -gt 0 ]; then
    printf '%s\n' "${C_GRN}${C_BLD}[PASS] ambient_core は ESP32-P4 (Tab5) のツールチェーンでコンパイルでき、規約違反もありません。${C_RST}"
  else
    printf '%s\n' "${C_YEL}${C_BLD}[PASS(空)] 規約違反はありませんが、コンパイル検証は 0 本です。${C_RST}"
  fi
  exit 0
fi

printf '\n'
if [ "$fail_count" -gt 0 ] && [ "$violation_count" -gt 0 ]; then
  printf '%s\n' "${C_RED}${C_BLD}[FAIL] コンパイル失敗 ${fail_count} 本、規約違反 ${violation_count} 件。${C_RST}"
elif [ "$fail_count" -gt 0 ]; then
  printf '%s\n' "${C_RED}${C_BLD}[FAIL] コンパイル失敗 ${fail_count} 本。${C_RST}"
else
  printf '%s\n' "${C_RED}${C_BLD}[FAIL] コンパイルは全て通りましたが、規約違反 ${violation_count} 件があります。${C_RST}"
fi

exit 1
