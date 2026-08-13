#!/usr/bin/env bash
#
# screen-ctl.sh - GeoFrame キオスクのXディスプレイ電源制御スクリプト
#
# 目的:
#   app/geoframe/ha.py は Home Assistant の人感/在室センサーと連携し、
#   無人と判定すると GEOFRAME_SLEEP_CMD、復帰すると GEOFRAME_WAKE_CMD で
#   指定したシェルコマンドを実行する（server.py本体からの呼び出し。
#   `subprocess.Popen(cmd, shell=True)`）。このスクリプトはその2つの環境変数
#   から呼ばれる想定で、DISPLAY/DPMS制御の詳細をここに集約する
#   （/etc/default/geoframe に長いワンライナーを直書きしない）。
#
#     GEOFRAME_SLEEP_CMD=/opt/geoframe/deploy/kiosk/screen-ctl.sh off
#     GEOFRAME_WAKE_CMD=/opt/geoframe/deploy/kiosk/screen-ctl.sh on
#
#   設計意図: このスクリプトを手動実行して疎通確認するテスト
#   （例: ./screen-ctl.sh off && ./screen-ctl.sh status）が、そのまま
#   本番の実行経路（ha.py が呼ぶコマンド）の検証になる。テストの実行手段と
#   本番配線を同じ経路にすることで、「テストは通ったのに本番では効かない」を防ぐ。
#
# 前提:
#   - kiosk.sh が起動時にDPMS拡張を有効なまま保つよう設定していること
#     （`xset +dpms` + `xset dpms 0 0 0`。README §4.2参照。DPMS拡張自体が
#     無効化されていると、このスクリプトの `xset dpms force off/on` は
#     無視される）。
#   - このスクリプトの実行ユーザー（通常はサーバーを動かす geoframe ユーザー）
#     がXディスプレイへ接続できること。**XAUTHORITYはこのスクリプトでは
#     指定しない**。代わりに kiosk.sh 側で GEOFRAME_KIOSK_XHOST_ALLOW_USER に
#     このユーザー名を指定し、`xhost +SI:localuser:<user>` による許可を
#     事前に行っておく前提とする（README §4.2「push型」参照）。
#
set -euo pipefail

log() { echo "[screen-ctl.sh] $*" >&2; }

if ! command -v xset >/dev/null 2>&1; then
    log "ERROR: xset が見つかりません（x11-xserver-utils パッケージを確認）"
    exit 1
fi

# DISPLAYは:0固定が既定だが、環境変数 GEOFRAME_KIOSK_DISPLAY で上書き可能
# （README既存の環境変数命名規則 GEOFRAME_KIOSK_* に合わせる）。
DISPLAY_TARGET="${GEOFRAME_KIOSK_DISPLAY:-:0}"

usage() {
    cat <<'USAGE' >&2
Usage: screen-ctl.sh on|off|status
  on      DISPLAY=<target> xset dpms force on を実行し、画面を点灯させる。
  off     DISPLAY=<target> xset dpms force off を実行し、画面を消灯させる。
  status  xset q の "Monitor is On"/"Monitor is Off" を解釈し、標準出力に
          on/off/unknown のいずれかを出力する。
          終了コード: on=0, off=1, unknown=2
USAGE
}

cmd_on() {
    log "DISPLAY=${DISPLAY_TARGET} xset dpms force on"
    DISPLAY="$DISPLAY_TARGET" xset dpms force on
}

cmd_off() {
    log "DISPLAY=${DISPLAY_TARGET} xset dpms force off"
    DISPLAY="$DISPLAY_TARGET" xset dpms force off
}

cmd_status() {
    local output
    # pipefail下で `xset q | grep ...` を直接使うと、Xディスプレイに接続できない
    # 場合（xset q自体の失敗）にスクリプトごと落ちてしまう。先に出力を変数へ
    # 捕捉し、成否を明示的に判定してから解釈することでこれを避ける。
    if ! output="$(DISPLAY="$DISPLAY_TARGET" xset q 2>/dev/null)"; then
        log "xset q に失敗（Xディスプレイ ${DISPLAY_TARGET} に接続できない可能性）"
        echo "unknown"
        exit 2
    fi

    if echo "$output" | grep -q 'Monitor is On'; then
        echo "on"
        exit 0
    elif echo "$output" | grep -q 'Monitor is Off'; then
        echo "off"
        exit 1
    else
        log "xset q の出力から Monitor 状態を判定できなかった"
        echo "unknown"
        exit 2
    fi
}

case "${1:-}" in
    on)     cmd_on ;;
    off)    cmd_off ;;
    status) cmd_status ;;
    *)      usage; exit 2 ;;
esac
