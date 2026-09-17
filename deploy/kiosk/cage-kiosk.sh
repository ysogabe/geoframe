#!/usr/bin/env bash
#
# cage-kiosk.sh — GeoFrame キオスクを cage（単一アプリ用Waylandコンポジタ）+ Chromium で起動する
#
# 位置づけ:
#   Cog + DRM の第二候補。DRM直結より層が1つ増えるが、
#   ・Chromium の CDP（--remote-debugging-port）で描画FPSを実測できる
#   ・入力・出力・回転まわりの道具立てが揃っている
#   という利点がある。**負荷測定はまずこの構成で回すのが速い。**
#
# 前提:
#   - cage, chromium が入っている（Raspberry Pi リポジトリの trixie arm64 に存在）
#   - 実行ユーザーが video / render / input グループに所属していること
#   - 他のコンポジタ（Xorg, labwc, cog）を同時に起動しないこと
#
# 設定（/etc/geoframe/kiosk.env）:
#   GEOFRAME_URL, GEOFRAME_KIOSK_WAIT_TIMEOUT, GEOFRAME_KIOSK_OFFLINE_URL
#   KIOSK_PROFILE_DIR（永続パス固定。localStorageを消さないため必須）
#   KIOSK_DEBUG_PORT（既定9222。0 で無効）
#
set -euo pipefail

GEOFRAME_URL="${GEOFRAME_URL:-http://localhost:8080/}"
WAIT_TIMEOUT="${GEOFRAME_KIOSK_WAIT_TIMEOUT:-60}"
OFFLINE_URL="${GEOFRAME_KIOSK_OFFLINE_URL:-}"
PROFILE_DIR="${KIOSK_PROFILE_DIR:-$HOME/.config/geoframe-kiosk}"
DEBUG_PORT="${KIOSK_DEBUG_PORT:-9222}"

log() { echo "[cage-kiosk.sh] $*" >&2; }

for b in cage chromium; do
    command -v "$b" >/dev/null 2>&1 || { log "ERROR: $b が見つかりません"; exit 1; }
done

mkdir -p "$PROFILE_DIR"

elapsed=0
until curl --silent --fail --max-time 2 -o /dev/null "$GEOFRAME_URL"; do
    if [ "$elapsed" -ge "$WAIT_TIMEOUT" ]; then
        log "WARNING: ${WAIT_TIMEOUT}s待ってもサーバー応答なし（$GEOFRAME_URL）"
        if [ -n "$OFFLINE_URL" ]; then
            log "フォールバックURLへ切り替えます: $OFFLINE_URL"
            GEOFRAME_URL="$OFFLINE_URL"
        fi
        break
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done
log "server check done (waited ${elapsed}s). url=$GEOFRAME_URL"

CHROME_ARGS=(
    --kiosk
    --ozone-platform=wayland
    --noerrdialogs
    --disable-session-crashed-bubble
    --disable-translate
    --password-store=basic
    --user-data-dir="$PROFILE_DIR"
)
# 負荷測定用（127.0.0.1限定。LANへは開かない）
if [ "$DEBUG_PORT" != "0" ]; then
    CHROME_ARGS+=(--remote-debugging-address=127.0.0.1 "--remote-debugging-port=$DEBUG_PORT")
fi

log "starting: cage -s -- chromium ${CHROME_ARGS[*]} $GEOFRAME_URL"
exec cage -s -- chromium "${CHROME_ARGS[@]}" "$GEOFRAME_URL"
