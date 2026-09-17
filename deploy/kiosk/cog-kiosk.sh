#!/usr/bin/env bash
#
# cog-kiosk.sh — GeoFrame キオスクを Cog + WPE WebKit（DRM/KMS直結）で起動する
#
# 位置づけ:
#   表示専用ノード（Raspberry Pi 400 等）の第一候補ランタイム。
#   デスクトップ環境もXもWaylandコンポジタも挟まず、Cog が DRM を直接掴む。
#
# 前提:
#   - サーバーは別機体でもよい（GEOFRAME_URL で指定）
#   - Cog は Debian の配布パッケージ（drm モジュール必須。実機で
#     `ls /usr/lib/aarch64-linux-gnu/cog/modules/` に libcogplatform-drm.so を確認）
#   - 実行ユーザーが video / render / input グループに所属していること
#   - 他のコンポジタ（Xorg, labwc, cage）を同時に起動しないこと（DRM master競合）
#
# 設定（/etc/geoframe/kiosk.env などから環境変数で渡す）:
#   GEOFRAME_URL                 表示するURL（既定 http://localhost:8080/ ）
#   COG_PLATFORM                 既定 drm
#   COG_PLATFORM_DRM_MODE_MAX    例 1920x1080@60（開発FHD） / 2560x1080@60（最終21:9）
#   COG_PLATFORM_DRM_VIDEO_MODE  native modeが選ばれない場合のみ指定（例 2560x1080）
#   COG_BG_COLOR                 既定 #05080fff
#   COG_EXTRA_ARGS               追加のCLI引数
#   GEOFRAME_KIOSK_WAIT_TIMEOUT  サーバー到達待ちの上限秒（既定60）
#   GEOFRAME_KIOSK_OFFLINE_URL   サーバー不達時に代わりに開くURL（未指定なら空表示のまま起動）
#
set -euo pipefail

GEOFRAME_URL="${GEOFRAME_URL:-http://localhost:8080/}"
COG_PLATFORM="${COG_PLATFORM:-drm}"
COG_PLATFORM_DRM_MODE_MAX="${COG_PLATFORM_DRM_MODE_MAX:-1920x1080@60}"
COG_BG_COLOR="${COG_BG_COLOR:-#05080fff}"
COG_EXTRA_ARGS="${COG_EXTRA_ARGS:-}"
WAIT_TIMEOUT="${GEOFRAME_KIOSK_WAIT_TIMEOUT:-60}"
OFFLINE_URL="${GEOFRAME_KIOSK_OFFLINE_URL:-}"

log() { echo "[cog-kiosk.sh] $*" >&2; }

COG_BIN="$(command -v cog || true)"
if [ -z "$COG_BIN" ]; then
    log "ERROR: cog が見つかりません。配布パッケージを確認してください（apt-cache policy cog）"
    exit 1
fi

# --- サーバーの到達待ち（kiosk.sh と同じ考え方: 起動直後の失敗で諦めない） ---
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

export COG_PLATFORM_DRM_MODE_MAX
[ -n "${COG_PLATFORM_DRM_VIDEO_MODE:-}" ] && export COG_PLATFORM_DRM_VIDEO_MODE

# pointer/cursor は production では無効のまま（設計書§9.1）。検証時のみ COG_EXTRA_ARGS で上書きする。
log "starting: cog --platform=$COG_PLATFORM renderer=modeset mode_max=$COG_PLATFORM_DRM_MODE_MAX"
exec "$COG_BIN" \
    --platform="$COG_PLATFORM" \
    --platform-params=renderer=modeset \
    --webprocess-failure=restart \
    --bg-color="$COG_BG_COLOR" \
    ${COG_EXTRA_ARGS} \
    "$GEOFRAME_URL"
