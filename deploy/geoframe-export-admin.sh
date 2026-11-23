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
#   （止めるとき: 起動時に標準エラーへ出る unit 名で sudo systemctl stop <unit 名>。SIGTERM を受けた run は、飛んでいる Drive の要求が終わるのを待ち、
#     アップロードのスレッドを全部止めてから終わる。上限は要求 1 つにつき 100 秒）
#
# 検証用の入口（--verify を最初の引数にする。本番の記録・宛先・作業用フォルダに触れず、別の状態ディレクトリと別の宛先・作業用フォルダで動かす）:
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify run --from 2026-01-10 --destination yamareco --dry-run
#   sudo /opt/geoframe/deploy/geoframe-export-admin.sh --verify recover
#   （検証用の状態ディレクトリは、このスクリプトの VERIFY_STATE_DIR の固定値。検証用の宛先の親フォルダ・作業用フォルダの Drive ID は、
#     /etc/default/geoframe-export の GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO・GEOFRAME_EXPORT_VERIFY_FOLDER_TRAVEL_JOURNAL・
#     GEOFRAME_EXPORT_VERIFY_WORK_FOLDER から読む。dry_run・書き出し・recover は同じ入口で行う。本番の export.lock には触れない。
#     資格情報のファイルは本番と同じもの（API 版はファイルを書き換えないので、検証用の写しは要らない）。手順は deploy/README.md §8.8 の 4）
#
# なぜ要るか: geoframe-export を root で直接動かすと、作られるファイル（jobs/・published/）が root の所有になり、サービス
# （User=geoframe-export）が読めなくなる。バイナリは root では動かない（終了コード 2）。
# このラッパーは systemd-run で一時的なサービス（geoframe-export-cli-<日時>-<番号>）を作り、User=geoframe-export・サービスと同じ設定の引数
# （deploy/geoframe-export.service の ExecStart と同じ --drive-credentials・--folder-*・--work-folder・--state-dir・--feed-base）・
# 同じ LoadCredential=・同じ状態ディレクトリ（StateDirectory=geoframe-export、0700）・同じ UMask と、同じハードニングで動かす。サービスと同じ
# 実行枠・記録・資格情報を相手にする。
# 一時サービスの名前は呼び出しごとに変える（固定名だと、前の一時サービスが残っているときに systemd-run が名前の衝突で失敗し、
# 終了コード 4 にならない）。同時の実行は、バイナリが export.lock で断る（終了コード 4）。
#
# run・recover だけ受け付ける。serve はサービス（systemctl）で動かす。設定の引数（--drive-credentials など、上の固定の引数）を足すと、
# バイナリが重複として断る（終了コード 2）。宛先の親フォルダ・作業用フォルダ（--folder-yamareco・--folder-travel-journal・--work-folder）と、
# 資格情報（--drive-credentials）・試験専用の通信先（--drive-endpoint）は、本番の記録と食い違う書き出しを作らないよう、このスクリプトでも
# 断る（検証は --verify を使う）。
# 動かす前に、ユーザーと状態ディレクトリ（パスだけ）と一時サービスの名前を標準エラー出力へ出す。標準出力は geoframe-export の JSON だけ。
# Drive の ID・資格情報の値は出さない。
# 終了コードは geoframe-export の終了コード（systemd-run --wait が返す。run: 0 成功・0 枚 / 1 失敗 / 2 引数・設定の誤り / 3 conflict / 4 実行中）。
#
set -euo pipefail

BIN="/usr/local/bin/geoframe-export"
RUN_USER="geoframe-export"
STATE_DIR="/var/lib/geoframe-export"
FEED_BASE="http://127.0.0.1:8080"
# Drive の資格情報（root 所有の 0600。`geoframe-export import-credentials` で作る）。サービスの LoadCredential= と同じ元のファイル
# （下の PROPS の LoadCredential=）。CRED_FILE は存在の確認だけに使う（試験が一時ディレクトリへ差し替える）
CRED_UNIT_PATH="/etc/geoframe-export/drive-credentials.json"
CRED_FILE="$CRED_UNIT_PATH"
# フォルダの ID の置き場所（サービスの EnvironmentFile= と同じ。root 所有の 0600）。source せず、決まったキーの行だけを取り出す
DEFAULTS_FILE="/etc/default/geoframe-export"

# 検証用の入口（--verify）の固定値。本番と別の状態ディレクトリ（StateDirectory=geoframe-export-verify。jobs/・published/・
# export.lock・作業領域が本番と別）。宛先の親フォルダ・作業用フォルダは、本番とは別の Drive のフォルダ（ID は DEFAULTS_FILE の
# VERIFY_ のキー）。3 つのフォルダは互いに入れ子にならない（geoframe-export が起動時に Drive へ問い合わせて検査する）。
VERIFY_STATE_NAME="geoframe-export-verify"
VERIFY_STATE_DIR="/var/lib/${VERIFY_STATE_NAME}"

# サービスの ExecStart と同じ設定の引数（geoframe-export.service と食い違わないこと。deploy/geoframe-export-admin-test.sh が検査する）。
# 資格情報のパスは、unit では `--drive-credentials %d/drive-credentials`（`%d` は unit のファイルの中で systemd が資格情報のディレクトリに
# 展開する）。systemd-run の一時サービスのコマンドでは `%` の指定子が展開されない（systemd 257 で確かめた）ので、包みは `/bin/sh -c` を挟み、
# 実行時に `$CREDENTIALS_DIRECTORY/drive-credentials`（LoadCredential= を付けたときに systemd が入れる環境変数。`--expand-environment=no` なので
# systemd は展開せず、sh が展開する）を `--drive-credentials` に渡す。同じファイル（drive-credentials）を指す。
# --state-dir は、入口ごとに下で決めた値（本番か検証用）。フォルダの ID は、入口ごとに DEFAULTS_FILE から読む。
CRED_NAME="drive-credentials"

# サービスと同じハードニング（geoframe-export.service の同名の設定。同じ値であることを試験が検査する）。
# StateDirectory=・StateDirectoryMode= は、下で入口ごとに足す。
PROPS=(
    "LoadCredential=${CRED_NAME}:${CRED_UNIT_PATH}"
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
    MemoryDenyWriteExecute=true
    MemoryMax=512M
    TasksMax=64
    # SIGTERM（systemctl stop <unit 名>）は本体だけに送る。止め方と順序は geoframe-export が決める（サービスと同じ）
    KillMode=mixed
    # SIGTERM を受けた run は、飛んでいる要求が終わるのを待ち、アップロードのスレッドを全部止めるまで終わらない（要求 1 つの上限は 100 秒）。
    # サービスの TimeoutStopSec と同じだけ待つ
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

# フォルダの ID を DEFAULTS_FILE から取り出す。source しない（シェルとして実行しない）。決まったキー 1 つの行 `KEY=値` を、その行ごとに
# 厳しく読む: ちょうど 1 行で、値は Drive の ID の文字（英数字・_・-）の 1〜128 文字だけ（引用符・空白・コメントは認めない）。
# 値を返すだけで、画面には出さない
read_id() { # read_id <KEY> → 値を標準出力へ。無い・複数・形が違うときは、キー名だけを示して終了コード 2
    local key="$1" lines value
    [ -f "$DEFAULTS_FILE" ] || die "フォルダの ID の置き場所（${DEFAULTS_FILE}）が無い（deploy/README.md の手順で作る）"
    lines="$(grep -E "^${key}=" "$DEFAULTS_FILE" || true)"
    [ -n "$lines" ] || die "${DEFAULTS_FILE} に ${key} が無い"
    [ "$(printf '%s\n' "$lines" | wc -l)" = 1 ] || die "${DEFAULTS_FILE} の ${key} が複数行ある"
    value="${lines#"${key}="}"
    printf '%s' "$value" | grep -Eqx '[A-Za-z0-9_-]{1,128}' || die "${DEFAULTS_FILE} の ${key} の値が Drive の ID の形（英数字・_・- の 1〜128 文字だけ）ではない"
    printf '%s' "$value"
}

# 入口ごとの値。検証用の入口は、状態ディレクトリ・宛先の親フォルダ・作業用フォルダのどれも本番と共有しない（資格情報のファイルは同じ）
if [ "$verify" = 1 ]; then
    state_dir="$VERIFY_STATE_DIR"
    state_name="$VERIFY_STATE_NAME"
    key_prefix="GEOFRAME_EXPORT_VERIFY_"
else
    state_dir="$STATE_DIR"
    state_name="geoframe-export"
    key_prefix="GEOFRAME_EXPORT_"
fi

# 宛先の親フォルダ・作業用フォルダ・資格情報・試験専用の通信先は、この入口では変えさせない。本番の状態ディレクトリ（published/・
# ジョブの記録）のまま宛先だけを変えると、本番の公開の記録を別の場所の ID で上書きし、以後の置き換えが not_managed になる。
# 検証は --verify で行う。`--drive-endpoint` は試験専用（偽の Drive へ向ける）で、包みからは指定させない
for a in "$@"; do
    case "$a" in
        --folder-yamareco|--folder-yamareco=*|--folder-travel-journal|--folder-travel-journal=*|--work-folder|--work-folder=*)
            die "宛先の親フォルダ・作業用フォルダ（--folder-yamareco・--folder-travel-journal・--work-folder）は指定できない。検証用の宛先は --verify を最初の引数にして動かす" ;;
        --drive-credentials|--drive-credentials=*|--drive-endpoint|--drive-endpoint=*)
            die "資格情報・通信先（--drive-credentials・--drive-endpoint）は指定できない（サービスと同じものを使う）" ;;
    esac
done

# systemd-run --uid は root が要る（資格情報・フォルダの ID の置き場所は root 所有。状態ディレクトリは専用ユーザーの持ち物で、root が読める）
[ "$(id -u)" = 0 ] || die "root で実行すること（sudo を付ける）"
id -u "$RUN_USER" >/dev/null 2>&1 || die "ユーザー ${RUN_USER} が無い（deploy/README.md の手順で作る）"
[ -x "$BIN" ] || die "geoframe-export のバイナリが無い（${BIN}）"
[ -f "$CRED_FILE" ] || die "Drive の資格情報のファイルが無い（${CRED_FILE}）。geoframe-export import-credentials で作る（deploy/README.md）"
folder_yamareco="$(read_id "${key_prefix}FOLDER_YAMARECO")"
folder_travel_journal="$(read_id "${key_prefix}FOLDER_TRAVEL_JOURNAL")"
work_folder="$(read_id "${key_prefix}WORK_FOLDER")"
COMMON_ARGS=(
    --folder-yamareco "$folder_yamareco"
    --folder-travel-journal "$folder_travel_journal"
    --work-folder "$work_folder"
    --feed-base "$FEED_BASE"
)
command -v systemd-run >/dev/null 2>&1 || die "systemd-run が無い"

# 一時サービスの名前は呼び出しごとに一意にする（日時と、このプロセスの番号と乱数）。同じ名前を使い回すと、前の一時サービスが
# 残っているときに systemd-run が名前の衝突で失敗する（終了コード 4 にならない）。同時の実行かどうかは、export.lock を取る
# バイナリが決める（終了コード 4）
UNIT="geoframe-export-cli-$(date +%Y%m%d%H%M%S)-$$-${RANDOM}"

sd_args=()
sd_args+=(-p "StateDirectory=${state_name}" -p "StateDirectoryMode=0700")
for p in "${PROPS[@]}"; do sd_args+=(-p "$p"); done

if [ "$verify" = 1 ]; then
    say "検証用の入口: ユーザー ${RUN_USER}、状態ディレクトリ ${state_dir}（本番の記録・ロックとは別）、宛先の親フォルダと作業用フォルダは検証用（${DEFAULTS_FILE} の VERIFY_ のキー）、資格情報は本番と同じファイル"
else
    say "ユーザー ${RUN_USER}、状態ディレクトリ ${state_dir}（サービスと同じ設定。実行枠もサービスと共通）"
fi
say "一時サービス ${UNIT}（途中で止めるとき: sudo systemctl stop ${UNIT}）"

# --expand-environment=no: systemd は一時サービスのコマンドの引数の中の $NAME・${NAME} を既定で展開する。引数はそのまま
# geoframe-export へ渡したい（systemd 254 以降のオプション。古い systemd では systemd-run が断る）
# コマンドは /bin/sh -c '…' で、sh が最後に資格情報のパスを足して geoframe-export を exec する（$0 がバイナリ、残りがその引数）。
# 引数は sh の位置引数として渡すので、値の中の $・空白・引用符は解釈されない
exec systemd-run --unit="$UNIT" --uid="$RUN_USER" --wait --pipe --collect --expand-environment=no \
    "${sd_args[@]}" \
    /bin/sh -c 'exec "$0" "$@" --drive-credentials "${CREDENTIALS_DIRECTORY}/drive-credentials"' \
    "$BIN" "$@" "${COMMON_ARGS[@]}" --state-dir "$state_dir"
