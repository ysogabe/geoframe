#!/usr/bin/env bash
# miharashi のチェックアウトへのリンクと Cargo.lock を用意する。
#   MIHARASHI_DIR=~/work/miharashi tools/miharashi-feed-check/setup.sh
# miharashi 本体は読むだけで、何も書き込まない。
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="${1:-${MIHARASHI_DIR:-}}"
[ -n "$SRC" ] || { echo "使い方: MIHARASHI_DIR=<miharashi のチェックアウト> $0   （または第1引数）" >&2; exit 2; }
SRC="$(cd "$SRC" && pwd)"
[ -f "$SRC/crates/miharashi-app/src/photos.rs" ] || { echo "miharashi のチェックアウトではない: $SRC" >&2; exit 2; }
mkdir -p "$HERE/vendor"
ln -sfn "$SRC" "$HERE/vendor/miharashi"
# 依存の版を miharashi と同じにする（cargo は使わない分を自動で外す）
cp -f "$SRC/Cargo.lock" "$HERE/Cargo.lock"
echo "miharashi: $SRC（$(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo 'git なし')）"
