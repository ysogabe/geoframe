#!/usr/bin/env bash
#
# screen-ctl-pi.sh — 表示ノード（Raspberry Pi / DRM・Wayland キオスク）の画面電源制御
#
# なぜ既存の screen-ctl.sh が使えないか:
#   既存の screen-ctl.sh は `xset dpms force off/on` を使う **X11専用** の実装。
#   表示専用ノードは Cog（DRM直結）または cage（Wayland）で動くため X が無く、
#   そのままでは画面を消せない。ここではXを必要としない経路だけを使う。
#
# 方式:
#   sysfs   : /sys/class/drm/<connector>/dpms に On/Off を書く（既定・要root）
#             XもWaylandも不要。測定用スクリプトが読むのと同じ値なので、
#             「意図」と「実機」の照合がそのまま成立する。
#   cage    : wlr-randr --output <name> --off / --on（Waylandコンポジタ経由）
#
# 使い方:
#   ./screen-ctl-pi.sh on|off|status [--method sysfs|cage] [--connector card0-HDMI-A-1]
#
# 終了コード（既存 screen-ctl.sh と同じ契約にして、将来の screen-agent から
# 同じ呼び方で使えるようにしている）:
#   on=0 / off=1 / unknown=2（コマンド自体の失敗はそれ以外）
#
set -uo pipefail

METHOD="${SCREEN_CTL_METHOD:-sysfs}"
CONNECTOR="${SCREEN_CTL_CONNECTOR:-}"

args=()
while [ $# -gt 0 ]; do
    case "$1" in
        on|off|status) args+=("$1"); shift ;;
        --method) METHOD="$2"; shift 2 ;;
        --connector) CONNECTOR="$2"; shift 2 ;;
        *) echo "usage: $0 on|off|status [--method sysfs|cage] [--connector card0-HDMI-A-1]" >&2; exit 2 ;;
    esac
done
ACTION="${args[0]:-}"

detect_connector() {
    [ -n "$CONNECTOR" ] && { echo "$CONNECTOR"; return; }
    local c
    for c in /sys/class/drm/card*-*/; do
        if [ "$(cat "$c/status" 2>/dev/null)" = "connected" ]; then basename "$c"; return; fi
    done
}

case "$ACTION" in
    on|off)
        if [ "$METHOD" = "cage" ]; then
            command -v wlr-randr >/dev/null 2>&1 || { echo "wlr-randr が見つかりません" >&2; exit 3; }
            OUT="$(wlr-randr 2>/dev/null | awk 'NR==1{print $1}')"
            [ -z "$OUT" ] && { echo "cage出力を特定できません" >&2; exit 3; }
            if [ "$ACTION" = "on" ]; then exec wlr-randr --output "$OUT" --on; else exec wlr-randr --output "$OUT" --off; fi
        fi
        CONN="$(detect_connector)"
        [ -z "$CONN" ] && { echo "connected なDRMコネクタが見つかりません" >&2; exit 3; }
        F="/sys/class/drm/$CONN/dpms"
        [ -w "$F" ] || { echo "$F に書けません（rootで実行してください）" >&2; exit 3; }
        [ "$ACTION" = "on" ] && echo On > "$F" || echo Off > "$F"
        echo "[screen-ctl-pi.sh] $CONN dpms=$(cat "$F")" >&2
        ;;
    status)
        if [ "$METHOD" = "cage" ] && command -v wlr-randr >/dev/null 2>&1; then
            # wlr-randr は "Enabled: yes/no" を出す
            if wlr-randr 2>/dev/null | grep -qE '^\s*Enabled: yes'; then echo on; exit 0; else echo off; exit 1; fi
        fi
        CONN="$(detect_connector)"
        [ -z "$CONN" ] && { echo unknown; exit 2; }
        case "$(cat /sys/class/drm/$CONN/dpms 2>/dev/null)" in
            On)  echo on;  exit 0 ;;
            Off) echo off; exit 1 ;;
            *)   echo unknown; exit 2 ;;
        esac
        ;;
    *)
        echo "usage: $0 on|off|status [--method sysfs|cage] [--connector card0-HDMI-A-1]" >&2
        exit 2
        ;;
esac
