#!/usr/bin/env bash
# `miharashi png --renderer cpu` を N 回くり返し、成功の数を数える（毎回、一覧の取得 → 写真1枚の取得・デコード・合成）。
#   tools/miharashi-feed-check/repeat-png.sh <miharashi のバイナリ> <設定ファイル> [回数=60]
# バイナリのビルド（miharashi のリポジトリは変更しない。出力先を外に逃がす）:
#   cargo build --release -p miharashi-app --manifest-path "$MIHARASHI_DIR/Cargo.toml" --target-dir /tmp/miharashi-target
set -uo pipefail
BIN="${1:?miharashi のバイナリ}"; CFG="${2:?設定ファイル}"; N="${3:-60}"
OUT="$(mktemp -d /tmp/miharashi-png.XXXXXX)"
trap 'rm -rf "$OUT"' EXIT
ok=0; ng=0
for i in $(seq 1 "$N"); do
    if "$BIN" png --config "$CFG" --renderer cpu --size 640x360 --out "$OUT" >"$OUT/log" 2>&1 && [ -s "$OUT/status.png" ]; then
        ok=$((ok + 1))
    else
        ng=$((ng + 1)); echo "失敗 #$i: $(head -c 200 "$OUT/log")" >&2
    fi
done
echo "png --renderer cpu: ${N} 回中 成功 ${ok}、失敗 ${ng}"
[ "$ng" -eq 0 ]
