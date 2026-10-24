#!/bin/bash
# bench.shが残した出力と、現行Pythonの出力・正解との画質差を集計する（検証環境で実行）。
#   quality.sh <作業ディレクトリ>   結果は <作業ディレクトリ>/quality.log
set -u
W=$1
PY="$W/venv/bin/python"
LOG="$W/quality.log"
: > "$LOG"
cmp_() {  # cmp_ <見出し> <基準> <比較> [その他の引数]
  local label=$1 base=$2 out=$3
  shift 3
  { echo "## $label"; "$PY" -I "$W/scripts/compare.py" "$base" "$out" "$@"; echo; } >> "$LOG" 2>&1
}
for c in j-base j-zune j-img-catmull j-img-lanczos j-img-tri j-fir-lanczos j-fir-bilin j-fir-box j-enc-image j-enc-420opt j-enc-444 j-resize1st j-kamadak; do
  cmp_ "$c（基準=現行Python、ICCなしの入力）" "$W/ref/py_jpeg" "$W/out/$c" "$W/data/gt" jpeg --step 2 --only noicc
done
# ICC付きの入力: 現行Python（ICCを捨てる）と、Rustのkeep/srgb/dropを、正解と比べる
for c in j-base j-icc-keep j-icc-srgb; do
  cmp_ "$c（基準=現行Python、ICC付きの入力）" "$W/ref/py_jpeg" "$W/out/$c" "$W/data/gt" jpeg --step 1 --only icc
done
for c in h-libheif h-libheif-srgb h-libheif-keep; do
  cmp_ "$c（基準=現行Python、HEIC、ICC付き）" "$W/ref/py_heic" "$W/out/$c" "$W/data/gt" heic --step 1 --only icc
done
cmp_ "h-libheif（基準=現行Python、HEIC、ICCなし）" "$W/ref/py_heic" "$W/out/h-libheif" "$W/data/gt" heic --step 2 --only noicc
cmp_ "h-cmd（heif-convert。基準=現行Python、HEIC、ICCなし）" "$W/ref/py_heic" "$W/out/h-cmd" "$W/data/gt" heic --step 2 --only noicc
echo done >> "$LOG"
