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
#   （止めるとき: 起動時に標準エラーへ出る unit 名で sudo systemctl stop <unit 名>。SIGTERM を受けた run は、実行中の rclone を止めて wait し終えてから終わる）
#
# 検証用の入口（--verify を最初の引数にする。本番の記録・宛先・作業用フォルダに触れず、別の状態ディレクトリと別の宛先の接頭辞で動かす）:
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify run --from 2026-01-10 --destination yamareco --dry-run
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify recover
#   （検証用の状態ディレクトリ・宛先・作業用フォルダは、このスクリプトの VERIFY_* の固定値。dry_run・書き出し・recover は同じ入口で行う。
#     本番の export.lock も取るので、本番の書き出しの実行中は終了コード 4）
#
# なぜ要るか: geoframe-export を root で直接動かすと、作られるファイル（jobs/・published.json・rclone の設定の更新）が
# root の所有になり、サービス（User=geoframe-export）が読めなくなる。バイナリは root では動かない（終了コード 2）。
# このラッパーは systemd-run で一時的なサービス（geoframe-export-cli-<日時>-<番号>）を作り、User=geoframe-export・サービスと同じ設定の引数
# （deploy/geoframe-export.service の ExecStart と同じ --rclone・--rclone-config・--state-dir・--feed-base）・同じ状態ディレクトリ
# （StateDirectory=geoframe-export、0700）・同じ UMask と、同じハードニングで動かす。サービスと同じ実行枠・記録・rclone の設定を相手にする。
# 一時サービスの名前は呼び出しごとに変える（固定名だと、前の一時サービスが残っているときに systemd-run が名前の衝突で失敗し、
# 終了コード 4 にならない）。同時の実行は、バイナリが export.lock で断る（終了コード 4）。
#
# run・recover だけ受け付ける。serve はサービス（systemctl）で動かす。設定の引数（--rclone など、上の固定の引数）を足すと、
# バイナリが重複として断る（終了コード 2）。宛先の接頭辞と作業用フォルダ（--remote-yamareco・--remote-travel-journal・--work-root）は、
# 本番の記録と食い違う書き出しを作らないよう、このスクリプトでも断る（検証は --verify を使う）。
# 動かす前に、ユーザーと状態ディレクトリ（パスだけ）と一時サービスの名前を標準エラー出力へ出す。標準出力は geoframe-export の JSON だけ。
# 終了コードは geoframe-export の終了コード（systemd-run --wait が返す。run: 0 成功・0 枚 / 1 失敗 / 2 引数・設定の誤り / 3 conflict / 4 実行中）。
#
set -euo pipefail

BIN="/usr/local/bin/geoframe-export"
RUN_USER="geoframe-export"
STATE_DIR="/var/lib/geoframe-export"
RCLONE_BIN="/usr/local/bin/rclone"
RCLONE_CONF="${STATE_DIR}/rclone/rclone.conf"
FEED_BASE="http://127.0.0.1:8080"

# 検証用の入口（--verify）の固定値。本番と別の状態ディレクトリ（StateDirectory=geoframe-export-verify。jobs/・published.json・
# export.lock・作業領域が本番と別）、別の宛先の接頭辞、別の作業用フォルダ。remote 名（gdrive）は本番と同じ
# （rclone の設定は本番の 1 つを参照する。写さない）。3 つの場所は互いに入れ子にならない（geoframe-export が検査する）。
VERIFY_STATE_NAME="geoframe-export-verify"
VERIFY_STATE_DIR="/var/lib/${VERIFY_STATE_NAME}"
VERIFY_ARGS=(
    --remote-yamareco gdrive:geoframe-export-verify/yamareco
    --remote-travel-journal gdrive:geoframe-export-verify/travel-journal
    --work-root gdrive:geoframe-export-verify-work
)

# サービスの ExecStart と同じ設定の引数（geoframe-export.service と食い違わないこと。deploy/geoframe-export-admin-test.sh が検査する）。
# --state-dir は本番のもの。検証用の入口では、下で検証用のものに替える。
COMMON_ARGS=(--rclone "$RCLONE_BIN" --rclone-config "$RCLONE_CONF" --feed-base "$FEED_BASE")

# サービスと同じハードニング（geoframe-export.service の同名の設定。同じ値であることを試験が検査する）。
# StateDirectory=・StateDirectoryMode= は、下で入口ごとに足す。
PROPS=(
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
    # SIGTERM（systemctl stop <unit 名>）は本体だけに送る。rclone の止め方と順序は geoframe-export が決める（サービスと同じ）
    KillMode=mixed
    # SIGTERM を受けた run は rclone を止めて wait し終えるまで終わらない。サービスの TimeoutStopSec と同じだけ待つ
    TimeoutStopSec=180s
)

say() { echo "[geoframe-export-admin] $*" >&2; }
die() { echo "[geoframe-export-admin] ERROR: $*" >&2; exit 2; }

verify=0
if [ "${1:-}" = "--verify" ]; then
    verify=1
    shift
fi

case "${1:-}" in
    -h|--help)
        awk 'NR >= 3 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}" >&2
        exit 0 ;;
    run|recover) ;;
    serve) die "serve はこのラッパーでは動かさない（サービスで動かす: systemctl start geoframe-export）" ;;
    "") die "サブコマンド（run・recover）を指定すること（使い方は --help）" ;;
    *) die "知らないサブコマンド（run・recover だけ）。使い方は --help" ;;
esac

# 宛先の接頭辞・作業用フォルダは、この入口では変えさせない。本番の状態ディレクトリ（published.json・ジョブの記録）のまま
# 宛先だけを変えると、本番の公開の記録を別の場所の ID で上書きし、以後の置き換えが not_managed になる。検証は --verify で行う
for a in "$@"; do
    case "$a" in
        --remote-yamareco|--remote-yamareco=*|--remote-travel-journal|--remote-travel-journal=*|--work-root|--work-root=*)
            die "宛先の接頭辞・作業用フォルダ（--remote-yamareco・--remote-travel-journal・--work-root）は指定できない。検証用の宛先は --verify を最初の引数にして動かす" ;;
    esac
done

# systemd-run --uid は root が要る（設定のファイルや状態ディレクトリは専用ユーザーの持ち物で、root が読める）
[ "$(id -u)" = 0 ] || die "root で実行すること（sudo を付ける）"
id -u "$RUN_USER" >/dev/null 2>&1 || die "ユーザー ${RUN_USER} が無い（deploy/README.md の手順で作る）"
[ -x "$BIN" ] || die "geoframe-export のバイナリが無い（${BIN}）"
[ -x "$RCLONE_BIN" ] || die "rclone が無い（${RCLONE_BIN}）"
[ -f "$RCLONE_CONF" ] || die "rclone の設定のファイルが無い（${RCLONE_CONF}）。専用の client_id での設定が済んでいない機体では使えない"
command -v systemd-run >/dev/null 2>&1 || die "systemd-run が無い"

# 一時サービスの名前は呼び出しごとに一意にする（日時と、このプロセスの番号と乱数）。同じ名前を使い回すと、前の一時サービスが
# 残っているときに systemd-run が名前の衝突で失敗する（終了コード 4 にならない）。同時の実行かどうかは、export.lock を取る
# バイナリが決める（終了コード 4）
UNIT="geoframe-export-cli-$(date +%Y%m%d%H%M%S)-$$-${RANDOM}"

sd_args=()
if [ "$verify" = 1 ]; then
    state_dir="$VERIFY_STATE_DIR"
    state_name="$VERIFY_STATE_NAME"
    # 本番の rclone の設定への書き戻し（アクセストークンの更新）だけは、本番の rclone/ に許す。本番の jobs/・published.json・
    # export.lock は読み取り専用のまま（ProtectSystem=strict）
    sd_args+=(-p "ReadWritePaths=${STATE_DIR}/rclone")
    extra_args=("${VERIFY_ARGS[@]}")

    # 検証用の実行枠は本番と別なので、本番の書き出しと同時に動かさないように、本番の実行枠（export.lock）を、このスクリプトの
    # 終わりまで（exec した systemd-run が終わるまで）持つ。rclone の設定を 2 つの run が同時に書き戻すと、書き戻しの 2 回の
    # rename の間に設定ファイルが一瞬無くなる。取れなければ本番の書き出しの実行中
    prod_lock="${STATE_DIR}/export.lock"
    if [ -f "$prod_lock" ] && [ ! -L "$prod_lock" ]; then
        command -v flock >/dev/null 2>&1 || die "flock が無い（util-linux）"
        # 読み取り専用で開いて flock するだけ。専用ユーザーが export.lock をすり替えても、root は何も書かない
        exec {lock_fd}<"$prod_lock"
        if ! flock -n "$lock_fd"; then
            say "本番の書き出し（サービスか CLI）が実行中。終わるのを待つ（検証用の入口は、本番と同時に動かさない）"
            exit 4
        fi
    fi
else
    state_dir="$STATE_DIR"
    state_name="geoframe-export"
    extra_args=()
fi
sd_args+=(-p "StateDirectory=${state_name}" -p "StateDirectoryMode=0700")
for p in "${PROPS[@]}"; do sd_args+=(-p "$p"); done

if [ "$verify" = 1 ]; then
    say "検証用の入口: ユーザー ${RUN_USER}、状態ディレクトリ ${state_dir}（本番の記録とは別）、宛先と作業用フォルダは検証用（rclone の設定だけ本番の ${RCLONE_CONF}）"
else
    say "ユーザー ${RUN_USER}、状態ディレクトリ ${state_dir}（サービスと同じ設定。実行枠もサービスと共通）"
fi
say "一時サービス ${UNIT}（途中で止めるとき: sudo systemctl stop ${UNIT}）"

# --expand-environment=no: systemd は一時サービスのコマンドの引数の中の $NAME・${NAME} を既定で展開する。引数はそのまま
# geoframe-export へ渡したい（systemd 254 以降のオプション。古い systemd では systemd-run が断る）
exec systemd-run --unit="$UNIT" --uid="$RUN_USER" --wait --pipe --collect --expand-environment=no \
    "${sd_args[@]}" \
    "$BIN" "$@" "${COMMON_ARGS[@]}" --state-dir "$state_dir" ${extra_args[@]+"${extra_args[@]}"}
