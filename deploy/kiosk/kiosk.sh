#!/usr/bin/env bash
#
# kiosk.sh - GeoFrame Chromium キオスク起動スクリプト
#
# 重要: --user-data-dir は必ず永続パスに固定すること。
#   GeoFrame のフロントエンドはスライドショーの再生位置をブラウザ側の
#   永続ストレージ（localStorage等）に保存する設計。
#   incognito/--guest や、毎回破棄される一時プロファイルでの起動は
#   このストレージを消してしまい再生位置が保持できなくなるため禁止。
#   （このスクリプトは常に $KIOSK_PROFILE_DIR という固定パスを使う）
#
set -euo pipefail

GEOFRAME_URL="${GEOFRAME_URL:-http://localhost:8080/}"
KIOSK_PROFILE_DIR="${KIOSK_PROFILE_DIR:-$HOME/.config/geoframe-kiosk}"
WAIT_TIMEOUT="${GEOFRAME_KIOSK_WAIT_TIMEOUT:-60}"

log() { echo "[kiosk.sh] $*" >&2; }

# --- Chromiumバイナリの検出 ---
# Raspberry Pi OS 系は chromium-browser、Debian/一部環境は chromium という
# コマンド名になっていることが多いため、両方を探す。
CHROMIUM_BIN=""
for candidate in chromium chromium-browser; do
    if command -v "$candidate" >/dev/null 2>&1; then
        CHROMIUM_BIN="$(command -v "$candidate")"
        break
    fi
done

if [ -z "$CHROMIUM_BIN" ]; then
    log "ERROR: chromium / chromium-browser のどちらも見つかりません。"
    log "  Raspberry Pi OS: sudo apt install chromium-browser"
    log "  Debian/Ubuntu:   sudo apt install chromium"
    exit 1
fi
log "using chromium binary: $CHROMIUM_BIN"

mkdir -p "$KIOSK_PROFILE_DIR"

# --- サーバーが応答するまでポーリング（最大 WAIT_TIMEOUT 秒）---
log "waiting for GeoFrame server at $GEOFRAME_URL (timeout ${WAIT_TIMEOUT}s)..."
elapsed=0
until curl --silent --fail --max-time 2 -o /dev/null "$GEOFRAME_URL"; do
    if [ "$elapsed" -ge "$WAIT_TIMEOUT" ]; then
        log "WARNING: ${WAIT_TIMEOUT}s待ってもサーバー応答なし。とりあえず起動を続行する。"
        break
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done
log "server check done (waited ${elapsed}s)."

# --- 画面ブランク/スクリーンセーバー抑止（X11環境のみ。失敗しても続行）---
# 以前は `xset s off -dpms` でDPMS拡張自体を無効化していたが、これだと
# `xset q` が常に `DPMS is Disabled` を返すようになり、GEOFRAME_SLEEP_CMD/
# GEOFRAME_WAKE_CMD（README §4.2、screen-ctl.sh経由）が実行する
# `xset dpms force off/on` が一切効かなくなる（実機で `DPMS is Disabled` を
# 確認済み）。そこでDPMS拡張自体は有効のままにし、アイドルによる自動offだけを
# タイムアウト0（=無効化）で止める方式に変更した。これにより通常時は
# 自動で暗くならず、かつ `xset dpms force off/on` による明示制御には応答できる。
if command -v xset >/dev/null 2>&1; then
    xset s off 2>/dev/null || log "xset s off 失敗（Wayland環境等では想定内。無視して続行）"
    xset s noblank 2>/dev/null || true
    xset +dpms 2>/dev/null || log "xset +dpms 失敗（Wayland環境等では想定内。無視して続行）"
    xset dpms 0 0 0 2>/dev/null || log "xset dpms 0 0 0 失敗（Wayland環境等では想定内。無視して続行）"
else
    log "xset が見つからないためスクリーンブランク抑止をスキップ（Waylandの場合はREADMEのswayidle例を参照）"
fi

# --- xhost許可（GEOFRAME_SLEEP_CMD/WAKE_CMD からのXディスプレイ操作用。完全オプトイン）---
# GEOFRAME_KIOSK_XHOST_ALLOW_USER が設定されている場合のみ、そのUnixユーザーに対して
# `xhost +SI:localuser:<user>` でXディスプレイへの接続を許可する。これは
# Unixソケット越しのSO_PEERCREDによるUID単位の許可であり（`startx -- -nolisten tcp`
# 前提でTCPは開いていないためこの方式が成立する）、ネットワーク越しの接続を
# 開放するものではない。未設定（空文字含む）の場合は何もしない。既存の別機体
# 構成やSLEEP_CMD/WAKE_CMD未使用の環境に影響を与えないための完全オプトイン。
if [ -n "${GEOFRAME_KIOSK_XHOST_ALLOW_USER:-}" ]; then
    if command -v xhost >/dev/null 2>&1; then
        xhost "+SI:localuser:${GEOFRAME_KIOSK_XHOST_ALLOW_USER}" 2>/dev/null \
            || log "xhost +SI:localuser:${GEOFRAME_KIOSK_XHOST_ALLOW_USER} 失敗（無視して続行）"
    else
        log "xhost が見つからないため GEOFRAME_KIOSK_XHOST_ALLOW_USER の許可をスキップ（x11-xserver-utils パッケージを確認）"
    fi
fi

# --- Chromium起動 ---
# --user-data-dir は永続パス固定。incognito/--guest は使わないこと（冒頭コメント参照）。
# --password-store=basic: gnome-keyring/kwalletへの問い合わせを止める。
#   キオスクは保存パスワードやCookie暗号化にOS側キーリングを必要としないが、
#   デスクトップ環境にgnome-keyringが入っていると、自動ログイン経由のセッションでは
#   ログイン時にキーリングが解錠されない（自動ログインはlightdm-autologinという
#   別のPAMサービスを通り、通常ログイン用のpam_gnome_keyring.soフックを経由しない）。
#   その状態でChromiumがデフォルトのキーリング連携を試みると、起動のたびに
#   「キーリングを解錠」ダイアログが割り込み、キオスクの全画面表示を止めてしまう。
#   basicストア（平文ローカルファイル）を明示指定することで、この問い合わせ自体を
#   発生させない。
# --disable-translate: ページ言語を検知して出る「翻訳しますか？」ポップアップを止める。
#   キオスクにはキーボード・マウスが無く閉じる操作ができないため、
#   GeoFrameのHUDの一部を覆ったまま居座ってしまう。フラグに加えて
#   chromium-policy-disable-translate.json（Enterprise policy。プロファイルの
#   リセットに影響されず常に効く）でも二重に無効化している。
exec "$CHROMIUM_BIN" \
    --kiosk \
    --noerrdialogs \
    --disable-session-crashed-bubble \
    --password-store=basic \
    --disable-translate \
    --user-data-dir="$KIOSK_PROFILE_DIR" \
    "$GEOFRAME_URL"
