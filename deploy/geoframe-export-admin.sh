#!/usr/bin/env bash
#
# geoframe-export-admin.sh - geoframe-export の CLI（run・recover）を、サービス（geoframe-export.service）と同じ専用ユーザー・同じ設定で動かす
#
# 使い方（root で。sudo を付ける）:
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh run --from 2026-01-10 --destination yamareco --dry-run
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh run --from 2026-01-10 --to 2026-01-11 --destination travel-journal [--on-exists replace]
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh recover --resolve <job_id>
#   （run は、サービスが動いていても使える: 実行枠 export.lock をサービスと共有するので、書き出しの実行中なら終了コード 4）
#   （止めるとき: sudo systemctl stop geoframe-export-cli。SIGTERM を受けた run は、実行中の rclone を止めて wait し終えてから終わる）
#
# なぜ要るか: geoframe-export を root で直接動かすと、作られるファイル（jobs/・published.json・rclone の設定の更新）が
# root の所有になり、サービス（User=geoframe-export）が読めなくなる。バイナリは root では動かない（終了コード 2）。
# このラッパーは systemd-run で一時的なサービス geoframe-export-cli を作り、User=geoframe-export・サービスと同じ設定の引数
# （deploy/geoframe-export.service の ExecStart と同じ --rclone・--rclone-config・--state-dir・--feed-base）・同じ状態ディレクトリ
# （StateDirectory=geoframe-export、0700）・同じ UMask と、同じハードニングで動かす。サービスと同じ実行枠・記録・rclone の設定を相手にする。
#
# run・recover だけ受け付ける。serve はサービス（systemctl）で動かす。設定の引数（--rclone など、上の固定の引数）を足すと、
# バイナリが重複として断る（終了コード 2）。
# 動かす前に、ユーザーと状態ディレクトリ（パスだけ）を標準エラー出力へ出す。標準出力は geoframe-export の JSON だけ。
# 終了コードは geoframe-export の終了コード（systemd-run --wait が返す。run: 0 成功・0 枚 / 1 失敗 / 2 引数・設定の誤り / 3 conflict / 4 実行中）。
#
set -euo pipefail

BIN="/usr/local/bin/geoframe-export"
RUN_USER="geoframe-export"
UNIT="geoframe-export-cli"
STATE_DIR="/var/lib/geoframe-export"
RCLONE_BIN="/usr/local/bin/rclone"
RCLONE_CONF="${STATE_DIR}/rclone/rclone.conf"
FEED_BASE="http://127.0.0.1:8080"

# サービスの ExecStart と同じ設定の引数（geoframe-export.service と食い違わないこと。deploy/geoframe-export-admin-test.sh が検査する）
COMMON_ARGS=(--rclone "$RCLONE_BIN" --rclone-config "$RCLONE_CONF" --state-dir "$STATE_DIR" --feed-base "$FEED_BASE")

# サービスと同じハードニング（geoframe-export.service の同名の設定。同じ値であることを試験が検査する）
PROPS=(
    StateDirectory=geoframe-export
    StateDirectoryMode=0700
    UMask=0077
    NoNewPrivileges=true
    ProtectSystem=strict
    ProtectHome=yes
    PrivateTmp=true
    PrivateDevices=true
    CapabilityBoundingSet=
    "RestrictAddressFamilies=AF_INET AF_INET6"
    SystemCallFilter=@system-service
    SystemCallArchitectures=native
    ProtectKernelTunables=true
    ProtectKernelModules=true
    ProtectKernelLogs=true
    ProtectControlGroups=true
    RestrictNamespaces=true
    LockPersonality=true
    RestrictSUIDSGID=true
    # SIGTERM を受けた run は rclone を止めて wait し終えるまで終わらない。サービスの TimeoutStopSec と同じだけ待つ
    TimeoutStopSec=180s
)

say() { echo "[geoframe-export-admin] $*" >&2; }
die() { echo "[geoframe-export-admin] ERROR: $*" >&2; exit 2; }

case "${1:-}" in
    -h|--help)
        sed -n '3,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
        exit 0 ;;
    run|recover) ;;
    serve) die "serve はこのラッパーでは動かさない（サービスで動かす: systemctl start geoframe-export）" ;;
    "") die "サブコマンド（run・recover）を指定すること（使い方は --help）" ;;
    *) die "知らないサブコマンド（run・recover だけ）。使い方は --help" ;;
esac

# systemd-run --uid は root が要る（設定のファイルや状態ディレクトリは専用ユーザーの持ち物で、root が読める）
[ "$(id -u)" = 0 ] || die "root で実行すること（sudo を付ける）"
id -u "$RUN_USER" >/dev/null 2>&1 || die "ユーザー ${RUN_USER} が無い（deploy/README.md の手順で作る）"
[ -x "$BIN" ] || die "geoframe-export のバイナリが無い（${BIN}）"
[ -x "$RCLONE_BIN" ] || die "rclone が無い（${RCLONE_BIN}）"
[ -f "$RCLONE_CONF" ] || die "rclone の設定のファイルが無い（${RCLONE_CONF}）。専用の client_id での設定が済んでいない機体では使えない"
command -v systemd-run >/dev/null 2>&1 || die "systemd-run が無い"

# 一時サービスの名前を固定しているので、前のものが動いていると systemd-run が名前の衝突で失敗する（終了コードが紛らわしい）。先に断る
if systemctl is-active --quiet "${UNIT}.service" 2>/dev/null; then
    say "実行中の ${UNIT}.service がある（geoframe-export の別の run・recover）。終わるのを待つか、systemctl stop ${UNIT} で止める"
    exit 4
fi

say "ユーザー ${RUN_USER}、状態ディレクトリ ${STATE_DIR}（サービスと同じ設定。実行枠もサービスと共通）"

sd_args=()
for p in "${PROPS[@]}"; do sd_args+=(-p "$p"); done

exec systemd-run --unit="$UNIT" --uid="$RUN_USER" --wait --pipe --collect \
    "${sd_args[@]}" \
    "$BIN" "$@" "${COMMON_ARGS[@]}"
