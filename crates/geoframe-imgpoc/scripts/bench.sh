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
echo done >> "$LOG"
