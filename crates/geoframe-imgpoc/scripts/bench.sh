#!/bin/bash
# 候補ごとの時間・最大RSSを測る（検証環境で実行する）。結果は <作業ディレクトリ>/bench.log
#   bench.sh <imgpocのバイナリ> <作業ディレクトリ>
# 作業ディレクトリに data/jpeg data/heic がある前提。各設定は2回走らせ、2回目（ページキャッシュが温まった状態）を記録する。
# 出力JPEGは <作業ディレクトリ>/out/<設定名>/ に残す（画質の比較用）。
set -u
BIN=$1
WORK=$2
export TMPDIR="$WORK/tmp"
mkdir -p "$TMPDIR" "$WORK/out"
LOG="$WORK/bench.log"
: > "$LOG"

run() {  # run <設定名> <入力種別 jpeg|heic> <オプション...>
  local name=$1 kind=$2
  shift 2
  rm -rf "$WORK/out/$name"
  "$BIN" "$@" --out "$WORK/out/$name" --report "$WORK/out/$name.jsonl" "$WORK/data/$kind" > /dev/null 2>&1
  rm -rf "$WORK/out/$name"
  local result
  result=$("$BIN" "$@" --out "$WORK/out/$name" --report "$WORK/out/$name.jsonl" "$WORK/data/$kind" 2>&1)
  { echo "## $name ($kind) $*"; echo "$result"; echo; } >> "$LOG"
}

# JPEG（ICCは捨てる＝現行Pythonと同じ。比較したい部品だけを変える）
run j-base        jpeg --icc drop
run j-zune        jpeg --icc drop --decoder zune
run j-img-catmull jpeg --icc drop --resize image:catmullrom
run j-img-lanczos jpeg --icc drop --resize image:lanczos3
run j-img-tri     jpeg --icc drop --resize image:triangle
run j-fir-lanczos jpeg --icc drop --resize fir:lanczos3
run j-fir-bilin   jpeg --icc drop --resize fir:bilinear
run j-fir-box     jpeg --icc drop --resize fir:box
run j-enc-image   jpeg --icc drop --encoder image
run j-enc-420opt  jpeg --icc drop --encoder je420opt
run j-enc-444     jpeg --icc drop --encoder je444
run j-resize1st   jpeg --icc drop --order resize-first
run j-kamadak     jpeg --icc drop --orientation kamadak
run j-icc-keep    jpeg --icc keep
run j-icc-srgb    jpeg --icc srgb
run j-jobs2       jpeg --icc drop --jobs 2
run j-jobs2-zune  jpeg --icc drop --jobs 2 --decoder zune

# HEIC
run h-libheif       heic --icc drop
run h-libheif-srgb  heic --icc srgb
run h-libheif-keep  heic --icc keep
run h-libheif-jobs2 heic --icc drop --jobs 2
if command -v heif-convert > /dev/null; then
  run h-cmd       heic --icc drop --heic heif-convert
  run h-cmd-jobs2 heic --icc drop --heic heif-convert --jobs 2
fi
# 純RustのHEICデコーダ（バイナリを heic-zune,heic-rs,heic-oxide 付きでビルドしておく。
# 付けていないと該当の行は全件エラーになり、下の一致比較も0枚になる）
run h-zune          heic --icc drop --heic zune
run h-heic-rs       heic --icc drop --heic heic-rs
run h-oxide         heic --icc drop --heic oxide

# 出力の比較: zuneとlibheifの出力が、ファイルごとにバイト単位で同じか（どちらも --icc drop）。
# heic-rs・oxideはlibheifと色が合わないので、一致する枚数が少ないことの確認になる。
same_bytes() {  # same_bytes <基準の設定名> <比較する設定名>
  local a=$1 b=$2 total=0 same=0 f
  for f in "$WORK/out/$a"/*.jpg; do
    [ -e "$f" ] || continue
    total=$((total + 1))
    cmp -s "$f" "$WORK/out/$b/$(basename "$f")" && same=$((same + 1))
  done
  echo "## 出力のバイト一致 $a と $b: $same/$total 枚" >> "$LOG"
}
same_bytes h-libheif h-zune
same_bytes h-libheif h-heic-rs
same_bytes h-libheif h-oxide

# ICCの有無（--reportのicc_in）が、libheifとzuneで同じ枚数か。値の一致までは見ていない
icc_count() { grep -c '"icc_in":true' "$WORK/out/$1.jsonl"; }
echo "## icc_in=true の枚数: libheif=$(icc_count h-libheif) zune=$(icc_count h-zune)" >> "$LOG"
echo done >> "$LOG"
