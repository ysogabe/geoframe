#!/bin/bash
# ビルド方式（glibc動的・musl静的・同梱libheif）ごとの時間・最大RSSを測る（検証環境で実行）。
#   bench_builds.sh <作業ディレクトリ>   結果は <作業ディレクトリ>/bench_builds.log
# 作業ディレクトリに次のバイナリがある前提（無いものは飛ばす）:
#   target/release/geoframe-imgpoc          検証環境でビルド（glibc 2.39、システムのlibheif 1.17.6へ動的リンク）
#   imgpoc-n100-glibc                       開発機（glibc 2.41）でビルドしたもの（heic-zuneのみ）
#   imgpoc-musl                             開発機でmusl静的ビルド（heic-zuneのみ）
#   imgpoc-musl-libheif                     zig cc でmusl静的ビルド（同梱libheif 1.23.0＋静的libde265）
#   target-emb2/release/geoframe-imgpoc     glibc、同梱libheif＋静的libde265＋静的libstdc++
set -u
W=$1
export TMPDIR="$W/tmp"
LOG="$W/bench_builds.log"
: > "$LOG"
run() {  # run <見出し> <バイナリ> <入力種別> <オプション...>
  local label=$1 bin=$2 kind=$3
  shift 3
  [ -x "$bin" ] || { echo "## $label: バイナリなし" >> "$LOG"; return; }
  "$bin" "$@" "$W/data/$kind" > /dev/null 2>&1
  local r
  r=$(/usr/bin/time -f "wall=%es user=%Us sys=%Ss cpu=%P" "$bin" "$@" "$W/data/$kind" 2>&1 | grep -E "枚数|1枚|出力サイズ|wall=")
  { echo "## $label ($kind) $*"; echo "$r"; echo; } >> "$LOG"
}
J="--icc drop"
for jobs in 1 2; do
  run "JPEG glibc(検証環境ビルド) jobs=$jobs" "$W/target/release/geoframe-imgpoc" jpeg $J --jobs $jobs
  run "JPEG glibc(開発機ビルド) jobs=$jobs"   "$W/imgpoc-n100-glibc" jpeg $J --jobs $jobs
  run "JPEG musl静的 jobs=$jobs"              "$W/imgpoc-musl" jpeg $J --jobs $jobs
done
for jobs in 1 2; do
  run "HEIC zune glibc(検証環境ビルド) jobs=$jobs" "$W/target/release/geoframe-imgpoc" heic $J --heic zune --jobs $jobs
  run "HEIC zune glibc(開発機ビルド) jobs=$jobs"   "$W/imgpoc-n100-glibc" heic $J --heic zune --jobs $jobs
  run "HEIC zune musl静的 jobs=$jobs"              "$W/imgpoc-musl" heic $J --heic zune --jobs $jobs
  run "HEIC libheif 1.17.6(システム) glibc jobs=$jobs" "$W/target/release/geoframe-imgpoc" heic $J --heic libheif --jobs $jobs
  run "HEIC libheif 1.23.0(同梱,glibc) jobs=$jobs" "$W/target-emb2/release/geoframe-imgpoc" heic $J --heic libheif --jobs $jobs
  run "HEIC libheif 1.23.0(同梱,musl静的) jobs=$jobs" "$W/imgpoc-musl-libheif" heic $J --heic libheif --jobs $jobs
done
echo done >> "$LOG"
