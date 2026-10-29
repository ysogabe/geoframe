#!/usr/bin/env bash
#
# rust-deploy.sh - Rust 版 geoframe を配布して入れ替える（検証環境へ ssh で / 本番ではその機体で直接）
#
# ============================================================================
# 2 つの使い方がある。
#
# (1) 検証環境（lxc-geoframe）へ: 手元でビルドして ssh で配る。接続先に既定値は無い（--target か
#     GEOFRAME_SSH_TARGET で必ず指定）。誤配布を止めるため、接続先に検証環境の印（/etc/geoframe/staging）が
#     無いか、Python 版（geoframe-server.service）が非稼働と確認できなければ（稼働中・状態を取得できない
#     場合も）、何も変更せずに拒否する。
#
# (2) 本番へ: --production=<期待するホスト名>。ssh を使わず、その機体で root として直接動く（ビルドもしない）。
#     本番であること・配るものが演習で使ったものそのものであることを確かめ、確かめられなければ何も変更せず
#     に拒否する（検証環境の印がある機体、ホスト名が違う機体、Python 版が稼働中の機体などには配らない）。
#     本番の手順は docs/rust-migration/cutover.md（T5.3）。
#
# deploy.sh（Python 版。本番機で git を取り込む薄いラッパー）とは別物で、こちらはバイナリを配る。やること:
#   0. （検証環境）埋め込む旅程（app/travel_blog/plans）に git 管理外のファイルかリンクがあれば、ビルド前に止める
#   1. （検証環境）cargo build --release --target x86_64-unknown-linux-musl -p geoframe（完全静的）
#   2. バイナリと unit を転送（sha256 で照合）。本番では --bin を root 所有の一時ディレクトリへ写してから照合
#   3. 配置先のバイナリを .prev に残して入れ替え、unit（/usr/local/lib/systemd/system/）を入れて
#      daemon-reload・enable・restart
#   4. /healthz と /api/v1/photos を確認（本番では件数の下限も）。入れ替え以降のどこで失敗しても（再起動の失敗・
#      途中の中断も）、バイナリ・unit・enable の状態を配布前へ戻す。初回の配布なら disable して unit も撤去する
# ============================================================================
#
# 実行場所: 検証環境へは開発機（このリポジトリ内）。本番へは本番機の /opt/geoframe（sudo を付けて）。
#
set -euo pipefail

SSH_TARGET="${GEOFRAME_SSH_TARGET:-}"
# 配置先。変えるときは unit の ExecStart も合わせる。
BIN_PATH="${GEOFRAME_RS_BIN:-/usr/local/bin/geoframe}"
UNIT_NAME="geoframe-rs.service"
# 確認に使う URL（検証環境の中から curl する。待ち受けを変えたときに合わせる）
CHECK_BASE="${GEOFRAME_RS_CHECK_BASE:-http://127.0.0.1:8080}"
# 再起動のあと、/healthz が通るまで待つ秒数
WAIT_SECS="${GEOFRAME_RS_WAIT_SECS:-30}"

# 本番の指定（--production）の固定値。環境変数では変えられない（本番の経路は GEOFRAME_RS_* を一切読まず、
# 1 つでも環境にあれば断る）。変えるのはこのファイルの編集だけ。試験は一時コピーのこの行を書き換える。
PROD_BIN_PATH="/usr/local/bin/geoframe"
PROD_CHECK_BASE="http://127.0.0.1:8080"
PROD_UNIT_DIR="/usr/local/lib/systemd/system"
PROD_ETC_UNIT_DIR="/etc/systemd/system"
PROD_MARKER="/etc/geoframe/staging"
PROD_ENV_FILE="/etc/default/geoframe-rs"
PROD_ROOT_UID="0"
PROD_WAIT_SECS_DEFAULT="30"

TARGET_TRIPLE="x86_64-unknown-linux-musl"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCAL_BIN="${GEOFRAME_RS_LOCAL_BIN:-${REPO_ROOT}/target/${TARGET_TRIPLE}/release/geoframe}"
LOCAL_UNIT="${REPO_ROOT}/deploy/${UNIT_NAME}"

DRY_RUN=0
DO_BUILD=1
ROLLBACK=0
# 本番の指定（--production）。空なら検証環境の経路
PROD_HOST=""
PROD_BIN="" PROD_SHA="" PROD_COMMIT="" PROD_MIN=""
CHECK_ONLY=0
WAIT_SET=0
TARGET_SET=0

usage() {
    cat <<'USAGE'
使い方（検証環境へ）:
  rust-deploy.sh [--dry-run] [--skip-build] [--rollback] [--print-remote-script]
                 --target user@host [-h|--help]

  --target user@host  検証環境のSSH先（ssh と sudo が使えること）。
                      環境変数 GEOFRAME_SSH_TARGET でも指定可。既定値は無い。
  --dry-run           ビルド（--skip-build でなければ）と、入れ替えの手順の表示だけ。
                      転送・配置・再起動は行わない。
  --skip-build        ビルド済みの成果物（target/x86_64-unknown-linux-musl/release/geoframe）を使う。
  --rollback          配布せず、配置先の .prev（直前のバイナリ）へ手で戻す。
                      戻したあとに /healthz と /api/v1/photos を確認する。
  --print-remote-script
                      検証環境で実行する本体を標準出力へ出して終わる（モックでの試験用。ssh しない）。

使い方（本番で。ssh を使わず、その機体で root として直接実行する。ビルドしない）:
  sudo rust-deploy.sh --production=<期待するホスト名> --bin=<パス> --expect-sha256=<値>
                      --expect-commit=<SHA> --expect-min-photos=<件数> [--wait-secs=<秒>] [--check-only]
  sudo rust-deploy.sh --rollback --production=<期待するホスト名>

  sudo は環境変数を消すので、本番の指定では値をすべて引数で渡す。このスクリプトのある checkout（本番では
  /opt/geoframe）の HEAD が --expect-commit（40 桁）と一致し、追跡下のファイルに変更が無いことが条件。
  --bin           配るバイナリ（絶対パス）。root 所有の一時ディレクトリへ写してから sha256 を取り、
                  --expect-sha256（64 桁）と違えば断る。入れるのはその写し（確かめた後で差し替えられない）。
  --expect-min-photos
                  配布のあと /api/v1/photos の件数がこれより少なければ失敗として戻す（1 以上 999999999 以下。
                  桁が多い値は断る）。
  --wait-secs     再起動のあと /healthz を待つ秒数（0〜3600。既定 30。演習で測った値を渡す）。
  --check-only    変更の前の検査だけを行って終わる（ホスト名・検証環境の印が無いこと・commit・作業ツリー・
                  バイナリの sha256・設定のファイル /etc/default/geoframe-rs の有無とモードと所有者・mask の状態・
                  古い置き場所の unit の本体）。Python 版の状態の検査は除く（Python を止める前に実行するため）。
  --rollback --production=…
                  配布せず、配置先の .prev（直前のバイナリと unit）へ戻す。Python 版が非稼働であること、
                  ホスト名などの検査は同じ。
  入れる unit は、その checkout の deploy/geoframe-rs.service。

環境変数（任意。検証環境の経路だけ。--production は GEOFRAME_RS_* を一切使わず、1 つでも環境にあれば
終了コード 4 で断る。配置先・確認の URL・unit の置き場所・設定のファイルなどは本番用の固定値）:
  GEOFRAME_RS_BIN          配置先のバイナリ（既定 /usr/local/bin/geoframe）
  GEOFRAME_RS_CHECK_BASE   確認の URL（既定 http://127.0.0.1:8080。その機体の中から叩く）
  GEOFRAME_RS_WAIT_SECS    再起動後に /healthz を待つ秒数（既定 30）
  GEOFRAME_RS_LOCAL_BIN    配るバイナリ（検証環境。既定は musl ビルドの成果物。巻き戻しの試験用に差し替える。
                           --skip-build と合わせる。静的リンクの検査は通す）

配置先のバイナリは入れ替えの直前に <配置先>.prev へ残す（直前の1世代だけ）。
unit の本体は /usr/local/lib/systemd/system/geoframe-rs.service に置き、直前を同じ場所の .prev に残す
（/etc/systemd/system/ には mask のリンクと drop-in だけを置く）。/etc/systemd/system/geoframe-rs.service に
古い本体のファイルがあると、systemd はそちらを優先するので、何も変更せずに断る（先に退避する）。
入れ替え以降（unit の導入・daemon-reload・enable・restart・確認）のどこで失敗しても、自動で配布前の
状態へ戻す（バイナリ・unit・enable・稼働の状態）。初回の配布なら、止めて disable し、新しく入れた
unit とバイナリ（geoframe.failed へ退避）を撤去する。

復元の最中は中断（SIGHUP・SIGINT・SIGTERM。ssh の切断）を無視し、復元が終わってから終了する（終了コードは復元の結果）。
復元で止める操作は、stop の成功と、inactive/failed の確認の両方が要る（確認できなければ終了コード 3）。

終了コード: 0 成功 / 1 失敗したが配布前の状態へ戻した / 3 戻す途中で失敗した（手作業の復旧が要る。
            手順が表示される）/ 4 誤配布の防止のため拒否した（何も変更していない）/ 2 引数の誤り。
            初回の本番の配布が 0 でなければ、すぐ Python 版へ戻す手順（cutover.md）を行うこと。

誤配布の防止（変更の前に検査。--rollback も同じ）:
  検証環境:
  - 接続先に検証環境の印 /etc/geoframe/staging（通常ファイル）があること。
    検証環境で一度だけ作る:  sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging
  - geoframe-server.service（Python 版）の ActiveState が inactive/failed と確認できること
    （未導入は not-found/inactive で通る。LoadState は見ない: unit ファイルが無くても動いている
    サービスはありうるので、not-found/active も拒否。稼働中・状態を取得できない・空出力・未知の状態・状態の行の欠け／重複も拒否）。
  本番（--production）:
  - hostname が期待どおり、検証環境の印が無い、geoframe-rs が mask されていない、Python 版が非稼働
    （--check-only では除く）、checkout の HEAD と作業ツリー、バイナリの sha256、設定のファイルのモード。

（検証環境）ビルドの前に、app/travel_blog/plans（バイナリへ埋め込まれる旅程）に git 管理外（未追跡・無視）のファイルか
リンクがあれば、終了コード 1 で止める（--dry-run・--skip-build でも検査する）。そこへ至るパスの途中（app・
app/travel_blog）がリンクでも止める。読めないディレクトリがあって調べられないときも止める。ログには件数と
定型文だけを出し、ファイル名は出さない。
USAGE
}

log() { echo "[rust-deploy] $*" >&2; }
die() { echo "[rust-deploy] ERROR: $*" >&2; exit 1; }

# リモート（検証環境）で実行する本体。引数で値を渡し、標準入力から bash -s で読ませる（クォートを1回で済ませる）。
#   $1: モード（preflight / deploy / rollback）  $2: 配置先  $3: 確認の URL  $4: 待つ秒数
#   $5: 一時ディレクトリ（deploy のみ。バイナリと unit が入っている）
#
# errexit（set -e）には頼らない。`if f` や `f && g` から呼ばれた関数の中では errexit が効かないので、
# 変更を加える行はすべて、その行の直後で成否を明示して判定する。全体を関数に包み、最後の行で呼ぶ
# （bash -s は標準入力から本体を読むので、途中のコマンドが標準入力を食べても本体が欠けない）。
read -r -d '' REMOTE_SCRIPT <<'REMOTE' || true
set -u -o pipefail

unit_name="geoframe-rs.service"
py_unit="geoframe-server.service"
# unit の本体の置き場所。/etc/systemd/system/ には mask のリンク（/dev/null）と drop-in だけを置くので、
# 本体はここへ入れる（mask は /etc/systemd/system/<unit> にリンクを作るため、そこに本体があると失敗する）。
# 試験用（モック）に差し替えられる。実機では既定のまま。
# 本番の指定（--production）のときは、ラッパー（rust-deploy.sh の PROD_*）が RD_* で渡す固定値だけを使い、
# GEOFRAME_RS_TEST_* は読まない。環境変数で検査の向き先を変えられない（sudo -E などで環境を引き継いでも効かない）。
# 検証環境（ssh）のときだけ、試験用（モック）に GEOFRAME_RS_TEST_* で差し替えられる。実機では既定のまま。
if [ -n "${RD_PROD_HOST:-}" ]; then
    unit_dir="${RD_UNIT_DIR:?}"
    etc_unit_dir="${RD_ETC_UNIT_DIR:?}"
    marker="${RD_MARKER:?}"
    env_file="${RD_ENV_FILE:?}"
    root_uid="${RD_ROOT_UID:?}"
else
    unit_dir="${GEOFRAME_RS_TEST_UNIT_DIR:-/usr/local/lib/systemd/system}"
    etc_unit_dir="${GEOFRAME_RS_TEST_ETC_UNIT_DIR:-/etc/systemd/system}"
    marker="${GEOFRAME_RS_TEST_MARKER:-/etc/geoframe/staging}"
    env_file="${GEOFRAME_RS_TEST_ENV_FILE:-/etc/default/geoframe-rs}"
    root_uid="${GEOFRAME_RS_TEST_ROOT_UID:-0}"
fi
# etc_unit_dir は古い置き場所。ここに本体（通常のファイル）があると、systemd は /usr/local/lib より優先して読むので断る。
# 本番の指定（--production）のとき、ラッパーが同じプロセスの環境変数で渡す（ssh は通らないので消えない）。
# 検証環境（ssh）では空: prod_host=期待するホスト名  min_photos=配布後の /api/v1/photos の件数の下限
# skip_py=1 で Python 版の状態の検査を除く（--check-only）
prod_host="${RD_PROD_HOST:-}" min_photos="${RD_MIN_PHOTOS:-}" skip_py="${RD_SKIP_PY:-0}"

mode="" bin="" base="" wait_secs="" stage="" unit_path=""
# 配布前の状態と、どこまで変更したか（失敗時の復元に使う）
had_bin=0 had_unit=0 was_enabled="" was_active=""
m_bin=0 m_unit=0 m_enable=0
phase="idle"       # idle → mutating（変更を始めた。完了か復元で idle へ戻る）→ restoring（復元の最中）
fail_step=""

say() { echo "[remote] $*"; }

# unit の LoadState と ActiveState を取る（u_load・u_active に入れる）。取得できない（systemd に繋がらない・
# 空出力・LoadState／ActiveState の行がちょうど1行でない（欠け・重複）・値が空）なら非ゼロ。unit が存在しないときは systemd が LoadState=not-found を返すので、それとは区別できる。
# LoadState と ActiveState は独立で、not-found でも active でありうる。
u_load="" u_active=""
unit_state() {
    local out
    u_load="" u_active=""
    out="$(systemctl show --property=LoadState,ActiveState "$1" 2>/dev/null)" || return 1
    # LoadState= と ActiveState= はそれぞれちょうど1行のときだけ受け入れる。先頭だけを使うと、矛盾する行
    # （ActiveState=inactive のあとに ActiveState=active）を『止まっている』と取り違える。同じ値の重複も拒否。
    [ "$(printf '%s\n' "$out" | grep -c '^LoadState=')" = 1 ] || return 1
    [ "$(printf '%s\n' "$out" | grep -c '^ActiveState=')" = 1 ] || return 1
    u_load="$(printf '%s\n' "$out" | sed -n 's/^LoadState=//p')"
    u_active="$(printf '%s\n' "$out" | sed -n 's/^ActiveState=//p')"
    [ -n "$u_load" ] && [ -n "$u_active" ]
}

# Python 版（geoframe-server.service）が非稼働と確認できること。
py_check() {
    # Python 版は『非稼働と確認できた』ときだけ通す。状態を取得できない（systemd に繋がらない・空出力）や
    # 未知の状態を非稼働とみなして通すと、稼働中の Python 版と二重に書いてしまう。
    if ! unit_state "$py_unit"; then
        say "拒否: Python 版（${py_unit}）の状態を取得できない。非稼働と確認できないので何も変更していない"
        return 4
    fi
    # LoadState は見ない。unit ファイルを消して daemon-reload しても、動いているサービスは動き続けうる
    # （not-found と active は両立する）。未導入の機体は not-found/inactive になり、そのまま通る。
    case "$u_active" in
        inactive|failed) return 0 ;;
        active|activating|reloading|deactivating)
            say "拒否: Python 版（${py_unit}）が稼働中（${u_active}）。何も変更していない"
            return 4 ;;
        *)
            say "拒否: Python 版（${py_unit}）の状態が不明（${u_active}）。非稼働と確認できないので何も変更していない"
            return 4 ;;
    esac
}

# 古い置き場所（/etc/systemd/system/geoframe-rs.service）に本体のファイルがあれば断る。systemd は /etc を
# /usr/local/lib より優先して読むので、新しい置き場所へ入れても効かない。mask のリンク（/dev/null）は本体ではない。
placement_check() {
    local old="${etc_unit_dir}/${unit_name}"
    if [ -e "$old" ] || [ -L "$old" ]; then
        if [ -L "$old" ] && [ "$(readlink "$old")" = /dev/null ]; then return 0; fi
        say "拒否: 古い置き場所に unit の本体がある（${old}）。新しい置き場所（${unit_dir}）より優先されるので、"
        say "      先に退避すること（systemctl stop / disable のあと、本体を /etc の外へ移して daemon-reload）。何も変更していない"
        return 4
    fi
    return 0
}

# 誤配布の防止。変更の前に必ず通す。
preflight() {
    if [ -n "$prod_host" ]; then preflight_production; else preflight_staging; fi
}

# 検証環境: 印があり、Python 版が非稼働。
preflight_staging() {
    if [ ! -f "$marker" ] || [ -L "$marker" ]; then
        say "拒否: 検証環境の印（${marker}）が無い。ここは検証環境ではない可能性がある。何も変更していない"
        say "      検証環境なら、一度だけ  install -d /etc/geoframe && touch ${marker}  を実行する"
        return 4
    fi
    py_check || return $?
    placement_check
}

# 本番（--production）: ホスト名が期待どおり、検証環境の印が無い、geoframe-rs が mask されていない、
# 古い置き場所に本体が無い、設定のファイル（/etc/default/geoframe-rs）がある（中身は読まない。モードと所有者だけ）、
# Python 版が非稼働（--check-only では除く）。
preflight_production() {
    local host mode_owner state
    host="$(hostname 2>/dev/null)" || host=""
    if [ -z "$host" ] || [ "$host" != "$prod_host" ]; then
        say "拒否: ホスト名が期待と違う。この機体は本番として指定された機体ではない。何も変更していない"
        return 4
    fi
    if [ -e "$marker" ] || [ -L "$marker" ]; then
        say "拒否: 検証環境の印（${marker}）がある。検証環境に本番の指定は使えない。何も変更していない"
        return 4
    fi
    state="$(systemctl is-enabled "$unit_name" 2>/dev/null)" || true
    case "$state" in
        masked|masked-runtime)
            say "拒否: ${unit_name} が mask されている。切り替え直すときは先に unmask すること。何も変更していない"
            return 4 ;;
    esac
    placement_check || return $?
    if [ ! -f "$env_file" ] || [ -L "$env_file" ]; then
        say "拒否: 設定のファイル（${env_file}）が通常のファイルとして無い。何も変更していない"
        return 4
    fi
    mode_owner="$(stat -c '%a %u' -- "$env_file" 2>/dev/null)" || mode_owner=""
    if [ "$mode_owner" != "600 ${root_uid}" ]; then
        say "拒否: 設定のファイル（${env_file}）のモードが 0600 でない、または所有者が root でない。何も変更していない"
        return 4
    fi
    if [ "$skip_py" != 1 ]; then py_check || return $?; fi
    return 0
}

# /api/v1/photos の JSON 配列の要素（最上位のオブジェクト）の数を、ファイル $1 から数える。jq や Python に頼らない:
# 文字列の中身（\" を含む）を消して括弧だけを残し、深さ 2 の { を数える。
count_photos() {
    tr -d '\n' < "$1" | sed -E 's/\\.//g; s/"[^"]*"//g' | tr -cd '{}[]' \
        | awk 'BEGIN { d = 0; n = 0 } { for (i = 1; i <= length($0); i++) { c = substr($0, i, 1); if (c == "{" || c == "[") { d++; if (c == "{" && d == 2) n++ } else d-- } } END { print n + 0 }'
}

# /healthz が 200 で、/api/v1/photos が 200 の JSON 配列（先頭が [）で返ること。$1 があれば、配列の要素の数が
# $1 以上であること（DATA_DIR の誤りで空の DB を作ったまま「成功」にしない）。本文は出力しない。
check() {
    local min="${1:-}" i=0 code first count body
    # 数でない・桁が多すぎる値は、整数の比較が異常終了して『下限を満たした』ように進むので、比較の前に断る
    [[ "$wait_secs" =~ ^[0-9]{1,4}$ ]] || { say "待つ秒数が不正"; return 1; }
    if [ -n "$min" ] && ! [[ "$min" =~ ^[1-9][0-9]{0,8}$ ]]; then say "件数の下限が不正（1 以上 9 桁まで）"; return 1; fi
    until curl -fsS --max-time 3 -o /dev/null "${base}/healthz" 2>/dev/null; do
        i=$((i + 1))
        [ "$i" -lt "$wait_secs" ] || { say "/healthz が ${wait_secs} 秒以内に通らない"; return 1; }
        sleep 1
    done
    body="$(mktemp /tmp/geoframe-rs-photos.XXXXXX)" || { say "確認用の一時ファイルを作れない"; return 1; }
    code="$(curl -sS --max-time 60 -o "$body" -w '%{http_code}' "${base}/api/v1/photos" 2>/dev/null)" || code=""
    if [ "$code" != "200" ]; then
        rm -f "$body"; say "/api/v1/photos が 200 でない（${code:-接続できない}）"; return 1
    fi
    first="$(head -c 1 "$body" 2>/dev/null)"
    if [ "$first" != "[" ]; then
        rm -f "$body"; say "/api/v1/photos が JSON 配列を返さない"; return 1
    fi
    if [ -n "$min" ]; then
        count="$(count_photos "$body")" || count=""
        rm -f "$body"
        [[ "$count" =~ ^[0-9]{1,9}$ ]] || { say "/api/v1/photos の件数を数えられない"; return 1; }
        # 『下限以上』が成り立つときだけ通す。比較が異常終了したとき（数でない値など）も、通さずに失敗にする
        if [ "$count" -ge "$min" ] 2>/dev/null; then
            say "確認 OK（/healthz 200、/api/v1/photos は配列で ${count} 件 >= 下限 ${min}）"
            return 0
        fi
        say "/api/v1/photos の件数 ${count} が下限 ${min} より少ない、または比較できない（DATA_DIR の誤りなど）"
        return 1
    fi
    rm -f "$body"
    say "確認 OK（/healthz 200、/api/v1/photos は配列）"
}

# reset-failed: RestartPreventExitStatus で止まった failed 状態を解いてから起動する
service_restart() {
    systemctl reset-failed "$unit_name" 2>/dev/null || true
    systemctl restart "$unit_name"
}

# 止まったと確認できること。stop の成功は必須で、失敗を許すのは unit が存在しない（LoadState=not-found）
# ときだけ。そのうえで状態が inactive か failed と確認できなければ失敗（activating・deactivating や、
# 状態を取得できない場合を『止まった』とみなさない）。
service_stop() {
    local stopped=1
    systemctl stop "$unit_name" >/dev/null 2>&1 || stopped=0
    unit_state "$unit_name" || return 1
    if [ "$stopped" = 0 ] && [ "$u_load" != not-found ]; then return 1; fi
    case "$u_active" in
        inactive|failed) return 0 ;;
        *) return 1 ;;
    esac
}

fail() { fail_step="$*"; say "失敗: $*"; }

# 手作業の復旧手順。自動で戻せなかったときに出す。restore_done=1 は、ファイルの復元は済んだが、
# 戻したあとの確認が通らないとき（.prev はもう使い済みなので、mv の手順は出さない）。
restore_done=0
manual_hint() {
    say "================================================================"
    say "自動で戻せなかった（失敗した操作: ${fail_step:-不明}）。検証環境で root として手で戻すこと:"
    say "  1. まず止める（Restart=on-failure で、失敗した状態が 5 秒おきに再起動されうる）:"
    say "       systemctl stop ${unit_name}"
    say "  2. 状態を見る:  ls -l ${bin} ${bin}.prev ${bin}.failed ${unit_path} ${unit_path}.prev"
    say "                 systemctl status ${unit_name}; systemctl is-enabled ${unit_name}"
    if [ "$restore_done" = 1 ]; then
        say "  （バイナリと unit は配布前の状態へ戻してある。戻したあとの確認が通らない）"
        say "  3. 原因を調べる: journalctl -u ${unit_name} -n 50 --no-pager"
        say "                 データ・設定（/etc/default/geoframe、DB の持ち主）に配布前から問題がなかったか"
        say "  4. 直ったら: systemctl reset-failed ${unit_name}; systemctl restart ${unit_name}; curl -fsS ${base}/healthz"
    elif [ "${first_deploy:-0}" = 1 ]; then
        say "  （初回の配布だった。戻す先は『何も無い』状態）"
        say "  3. systemctl disable ${unit_name}"
        say "  4. mv -f ${bin} ${bin}.failed; rm -f ${unit_path}"
        say "  5. systemctl daemon-reload"
    else
        say "  3. 直前のバイナリへ: mv -f ${bin}.prev ${bin}"
        say "  4. 直前の unit へ（${unit_path}.prev があれば）: mv -f ${unit_path}.prev ${unit_path}"
        say "  5. systemctl daemon-reload && systemctl reset-failed ${unit_name}; systemctl restart ${unit_name}"
        say "  6. curl -fsS ${base}/healthz"
    fi
    say "================================================================"
}

# 復元。方針は次の変数で受け、各操作の成否を明示して、最初の失敗で非ゼロを返す。
#   r_service: restart | stop | keep    r_disable: 0 | 1
#   r_bin: prev | remove | keep         r_unit: prev | remove | keep
# 失敗した方のバイナリは調べられるように ${bin}.failed に残す。
restore() {
    if [ "$r_service" = stop ]; then
        service_stop || { fail "サービスを止められない"; return 1; }
    fi
    if [ "$r_disable" = 1 ]; then
        systemctl disable "$unit_name" >/dev/null 2>&1 || { fail "systemctl disable"; return 1; }
    fi
    case "$r_bin" in
        prev)
            if [ -e "$bin" ]; then
                cp -p "$bin" "${bin}.failed" 2>/dev/null || say "警告: 失敗した方のバイナリを ${bin}.failed に残せなかった"
            fi
            mv -f "${bin}.prev" "$bin" || { fail "mv ${bin}.prev → ${bin}"; return 1; } ;;
        remove)
            if [ -e "$bin" ]; then
                mv -f "$bin" "${bin}.failed" || { fail "mv ${bin} → ${bin}.failed"; return 1; }
            fi ;;
    esac
    case "$r_unit" in
        prev)   mv -f "${unit_path}.prev" "$unit_path" || { fail "mv ${unit_path}.prev → ${unit_path}"; return 1; } ;;
        remove) rm -f "$unit_path" || { fail "rm ${unit_path}"; return 1; } ;;
    esac
    if [ "$r_unit" != keep ] || [ "$r_disable" = 1 ]; then
        systemctl daemon-reload || { fail "systemctl daemon-reload"; return 1; }
    fi
    if [ "$r_service" = restart ]; then
        service_restart || { fail "systemctl restart"; return 1; }
    fi
    return 0
}

# 復元の最中は中断（SIGHUP・SIGINT・SIGTERM。ssh の切断も）と SIGPIPE を無視する。復元の途中で終了すると、
# 新版のバイナリ・unit が残ったままサービスが止まり、復旧手順も出ない。無視は子プロセス（systemctl など）にも
# 引き継がれるので、プロセスグループへ送られた中断で復元中のコマンドが落ちることもない。復元が終わってから
# 終了する（終了コードは復元の結果。SIGKILL だけは防げない）。
protect_restore() {
    phase="restoring"
    trap '' HUP INT TERM PIPE
}

# 変更を始めたあとの失敗（確認の失敗・各操作の失敗・中断）から、配布前の状態へ戻す。
# 戻せたら 1、戻す途中で失敗したら 3 を返す。
recover_deploy() {
    local rc
    protect_restore
    recover_body && rc=0 || rc=$?
    phase="idle"
    return "$rc"
}
recover_body() {
    say "配布前の状態へ戻す"
    journalctl -u "$unit_name" -n 15 --no-pager -o cat 2>/dev/null | sed 's/^/[remote:journal] /' || true
    r_bin=keep r_unit=keep r_disable=0 r_service=keep
    if [ "$m_bin" = 1 ]; then
        if [ "$had_bin" = 1 ]; then r_bin=prev; else r_bin=remove; fi
    fi
    if [ "$m_unit" = 1 ]; then
        if [ "$had_unit" = 1 ]; then r_unit=prev; else r_unit=remove; fi
    fi
    if [ "$m_enable" = 1 ] && [ "$was_enabled" != "enabled" ]; then r_disable=1; fi
    if [ "$had_bin" = 1 ] && [ "$was_active" = "active" ]; then r_service=restart; else r_service=stop; fi
    first_deploy=$((1 - had_bin))
    if ! restore; then
        manual_hint
        return 3
    fi
    if [ "$r_service" = restart ]; then
        if check; then
            say "戻した（直前のバイナリで確認 OK）"
        else
            fail "戻したあとの確認"
            restore_done=1
            manual_hint
            return 3
        fi
    else
        say "戻した（配布前は稼働していなかったので、止めたまま）"
    fi
    return 1
}

# 中断（ssh の切断・SIGTERM など）や想定外の終了で、変更の途中のまま抜けるときの後始末。
on_exit() {
    local rc=$? ph="$phase"
    # 変更の途中なら、復元に入る前に中断を無視する（この関数の先頭で次の中断に当たると復元が飛ぶ）
    if [ "$ph" = mutating ]; then protect_restore; fi
    trap - EXIT
    if [ "$mode" = deploy ]; then rm -f "${bin}.new" "${bin}.prev.tmp" "${unit_path}.prev.tmp" 2>/dev/null || true; fi
    if [ "$ph" = mutating ]; then
        say "変更の途中で終了した（rc=${rc}）"
        # EXIT trap は解除済みなので、復元の中で致命的なシェルエラー（未定義変数の参照など）が起きると、
        # そのままでは復旧手順を出さずに終わる。専用のサブシェルで復元し、異常終了しても親が手順と 3 を保証する。
        # 無視している中断（protect_restore）はサブシェルにも引き継がれる。復元の結果は 100 を足して返し、
        # 正常に終えたこと（101=戻せた・103=戻せず手順は表示済み）と異常終了（それ以外）を見分ける。
        local sub
        ( set -u; recover_deploy; exit $(($? + 100)) )
        sub=$?
        case "$sub" in
            101) rc=1 ;;
            103) rc=3 ;;
            *)
                fail "復元の最中に想定外の終了（サブシェルの終了コード ${sub}）"
                manual_hint
                rc=3 ;;
        esac
    elif [ "$ph" = restoring ]; then
        # 復元の最中に想定外の終了（exit・未定義変数の参照など）。復元を入れ子にせず、手作業の手順を出す
        fail "復元の最中に想定外の終了（rc=${rc}）"
        manual_hint
        rc=3
    fi
    exit "$rc"
}

# 入れ替え以降の変更。各操作を明示して、最初の失敗で非ゼロを返す（復元は呼び出し側）。
apply_deploy() {
    m_bin=1
    mv -f "${bin}.new" "$bin" || { fail "mv ${bin}.new → ${bin}"; return 1; }
    m_unit=1
    install -m 0644 "${stage}/${unit_name}" "$unit_path" || { fail "unit の導入（${unit_path}）"; return 1; }
    systemctl daemon-reload || { fail "systemctl daemon-reload"; return 1; }
    m_enable=1
    systemctl enable "$unit_name" >/dev/null 2>&1 || { fail "systemctl enable"; return 1; }
    service_restart || { fail "systemctl restart"; return 1; }
    check "$min_photos" || { fail "再起動後の確認"; return 1; }
    return 0
}

do_deploy() {
    [ -d "$stage" ] || { say "一時ディレクトリが無い"; return 1; }
    preflight || return $?

    # 配布前の状態を覚える
    had_bin=0;  [ -f "$bin" ] && had_bin=1
    had_unit=0; [ -f "$unit_path" ] && had_unit=1
    was_enabled="$(systemctl is-enabled "$unit_name" 2>/dev/null)" || true
    was_active="$(systemctl is-active "$unit_name" 2>/dev/null)" || true
    first_deploy=$((1 - had_bin))

    # unit の置き場所（/usr/local/lib/systemd/system は無い機体がある）。作れなければ、まだ何も変えていない。
    install -d -m 0755 "$unit_dir" || { say "unit の置き場所（${unit_dir}）を作れない。何も変更していない"; return 1; }

    # 新しいバイナリを置き、直前の状態を .prev に残す。ここで失敗したら、まだ何も変えていない。
    install -m 0755 "${stage}/geoframe" "${bin}.new" || { say "バイナリを置けない（${bin}.new）。何も変更していない"; return 1; }
    if [ "$had_bin" = 1 ]; then
        { cp -p "$bin" "${bin}.prev.tmp" && mv -f "${bin}.prev.tmp" "${bin}.prev"; } \
            || { say "直前のバイナリを ${bin}.prev に残せない。何も変更していない"; return 1; }
        say "直前のバイナリを ${bin}.prev に残した"
    else
        rm -f "${bin}.prev"   # 初回の配布。古い世代の .prev が残っていても、戻す先ではない
        say "配置先にバイナリが無い（初回の配布）"
    fi
    if [ "$had_unit" = 1 ]; then
        { cp -p "$unit_path" "${unit_path}.prev.tmp" && mv -f "${unit_path}.prev.tmp" "${unit_path}.prev"; } \
            || { say "直前の unit を ${unit_path}.prev に残せない。何も変更していない"; return 1; }
    else
        rm -f "${unit_path}.prev"
    fi

    phase="mutating"
    if ! apply_deploy; then
        say "入れ替え以降の工程で失敗した。直前の状態へ戻す"
        recover_deploy
        return $?
    fi
    phase="idle"
    say "完了: $("$bin" --version 2>/dev/null || true)"
    return 0
}

do_rollback() {
    preflight || return $?
    [ -f "${bin}.prev" ] || { say "${bin}.prev が無い"; return 1; }
    local rc
    protect_restore
    rollback_body && rc=0 || rc=$?
    phase="idle"
    return "$rc"
}
rollback_body() {
    r_bin=prev r_disable=0 r_service=restart
    if [ -f "${unit_path}.prev" ]; then r_unit=prev; else r_unit=keep; fi
    first_deploy=0
    if ! restore; then
        manual_hint
        return 3
    fi
    if ! check; then
        fail "戻したあとの確認"
        restore_done=1
        manual_hint
        return 3
    fi
    say "戻した: $("$bin" --version 2>/dev/null || true)"
    return 0
}

main() {
    mode="${1:-}"; bin="${2:-}"; base="${3:-}"; wait_secs="${4:-}"; stage="${5:-}"
    unit_path="${unit_dir}/${unit_name}"
    # 数の引数は、桁を絞った形だけを受ける（整数の比較が異常終了して検査を素通りするのを防ぐ）
    [[ "$wait_secs" =~ ^[0-9]{1,4}$ ]] || { say "待つ秒数が不正（数字 4 桁まで）。何も変更していない"; exit 2; }
    if [ -n "$min_photos" ] && ! [[ "$min_photos" =~ ^[1-9][0-9]{0,8}$ ]]; then
        say "件数の下限が不正（1 以上 9 桁まで）。何も変更していない"; exit 2
    fi
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap on_exit EXIT
    case "$mode" in
        preflight) preflight; exit $? ;;
        deploy)    do_deploy; exit $? ;;
        rollback)  do_rollback; exit $? ;;
        *) say "不明なモード"; exit 2 ;;
    esac
}
main "$@"
REMOTE

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)    DRY_RUN=1; shift ;;
        --skip-build) DO_BUILD=0; shift ;;
        --rollback)   ROLLBACK=1; shift ;;
        --print-remote-script) printf '%s\n' "$REMOTE_SCRIPT"; exit 0 ;;
        --target)     [ $# -ge 2 ] || { echo "ERROR: --target には値が必要" >&2; exit 2; }
                      SSH_TARGET="$2"; TARGET_SET=1; shift 2 ;;
        --production=*)       PROD_HOST="${1#--production=}"; [ -n "$PROD_HOST" ] || { echo "ERROR: --production には期待するホスト名が必要" >&2; exit 2; }; shift ;;
        --bin=*)              PROD_BIN="${1#--bin=}"; shift ;;
        --expect-sha256=*)    PROD_SHA="${1#--expect-sha256=}"; shift ;;
        --expect-commit=*)    PROD_COMMIT="${1#--expect-commit=}"; shift ;;
        --expect-min-photos=*) PROD_MIN="${1#--expect-min-photos=}"; shift ;;
        --wait-secs=*)        WAIT_SECS="${1#--wait-secs=}"; WAIT_SET=1; shift ;;
        --check-only)         CHECK_ONLY=1; shift ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "ERROR: 不明な引数: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# 本番の指定とそれ以外の引数の組み合わせ（ここで形を確かめる。値を使うのは production_main）
if [ -n "$PROD_HOST" ]; then
    # 本番の経路は GEOFRAME_RS_*（試験用の差し替え・配置先・確認の URL・待ち秒数・配るバイナリ）を一切使わない。
    # 検査の向き先を環境で変えられると、sudo -E などで環境を引き継いだ実行が検査を外せる。ある時点で断る
    env_rs="$(compgen -e | grep -E '^GEOFRAME_RS_' | tr '\n' ' ')" || env_rs=""
    if [ -n "$env_rs" ]; then
        echo "ERROR: 環境に GEOFRAME_RS_* がある（${env_rs% }）。--production はこれらを使わないので、外して実行する（何も変更していない）" >&2
        exit 4
    fi
    BIN_PATH="$PROD_BIN_PATH" CHECK_BASE="$PROD_CHECK_BASE"
    [ "$WAIT_SET" -eq 1 ] || WAIT_SECS="$PROD_WAIT_SECS_DEFAULT"
    [ "$TARGET_SET" -eq 0 ] || { echo "ERROR: --production と --target は同時に使えない（本番は ssh を使わない）" >&2; exit 2; }
    [ "$DRY_RUN" -eq 0 ] || { echo "ERROR: --production に --dry-run は無い（変更の前の検査だけなら --check-only）" >&2; exit 2; }
    [[ "$PROD_HOST" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "ERROR: --production のホスト名は英数字と . _ - だけ" >&2; exit 2; }
    if [ "$ROLLBACK" -eq 1 ]; then
        [ -z "$PROD_BIN$PROD_SHA$PROD_COMMIT$PROD_MIN" ] && [ "$CHECK_ONLY" -eq 0 ] \
            || { echo "ERROR: --rollback --production には、ホスト名のほかの指定（--bin・--expect-*・--check-only）は要らない" >&2; exit 2; }
    else
        [ -n "$PROD_BIN" ] && [ -n "$PROD_SHA" ] && [ -n "$PROD_COMMIT" ] && [ -n "$PROD_MIN" ] \
            || { usage >&2; echo "ERROR: --production には --bin・--expect-sha256・--expect-commit・--expect-min-photos がすべて要る" >&2; exit 2; }
        case "$PROD_BIN" in /*) ;; *) echo "ERROR: --bin は絶対パス" >&2; exit 2 ;; esac
        [[ "$PROD_SHA" =~ ^[0-9a-f]{64}$ ]] || { echo "ERROR: --expect-sha256 は小文字 16 進 64 桁" >&2; exit 2; }
        [[ "$PROD_COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "ERROR: --expect-commit は小文字 16 進 40 桁の完全な SHA" >&2; exit 2; }
        [[ "$PROD_MIN" =~ ^[1-9][0-9]{0,8}$ ]] || { echo "ERROR: --expect-min-photos は 1 以上 999999999 以下の整数（9 桁まで）" >&2; exit 2; }
    fi
else
    if [ -n "$PROD_BIN$PROD_SHA$PROD_COMMIT$PROD_MIN" ] || [ "$CHECK_ONLY" -eq 1 ] || [ "$WAIT_SET" -eq 1 ]; then
        echo "ERROR: --bin・--expect-*・--wait-secs・--check-only は --production と一緒に使う" >&2; exit 2
    fi
    [ -n "$SSH_TARGET" ] || { usage >&2; die "--target user@host または GEOFRAME_SSH_TARGET の指定が必要。"; }
fi

# リモートのシェルへ値を埋め込むので、パスと URL は使える文字を絞る（引用符の崩れ・注入を防ぐ）。
case "$BIN_PATH" in
    /*[!A-Za-z0-9_./-]*|"") die "GEOFRAME_RS_BIN は絶対パスで、英数字と _ . / - だけ。" ;;
    /*) ;;
    *) die "GEOFRAME_RS_BIN は絶対パスで指定する。" ;;
esac
[[ "$CHECK_BASE" =~ ^http://[A-Za-z0-9._:-]+$ ]] || die "GEOFRAME_RS_CHECK_BASE は http://host:port の形（パスなし）。"
[[ "$WAIT_SECS" =~ ^[0-9]{1,4}$ ]] && [ "$WAIT_SECS" -le 3600 ] \
    || { echo "ERROR: 待つ秒数（--wait-secs・GEOFRAME_RS_WAIT_SECS）は 0〜3600 の整数（4 桁まで）" >&2; exit 2; }

# ssh の接続先。'-' 始まりの値が ssh のオプション（-oProxyCommand=…）として解釈されると、
# 手元で任意コマンドが実行される。deploy.sh と同じく -- で打ち切る。
ssh_run() { ssh -- "$SSH_TARGET" "$@"; }

# リモートの本体を、モードを指定して実行する（-t は付けない。検証環境は NOPASSWD sudo。
# 標準入力を本体の読み込みに使うので tty は割り当てない）。
remote_run() {
    local mode="$1"; shift
    ssh -- "$SSH_TARGET" "sudo bash -s -- ${mode} '${BIN_PATH}' '${CHECK_BASE}' '${WAIT_SECS}' $*" <<<"$REMOTE_SCRIPT"
}

# --- 本番（--production）: ssh を使わず、その機体で直接 ---
# リモートの本体（REMOTE_SCRIPT）を、この機体で bash -c で動かす。本番用の値は、同じプロセスの環境変数で渡す
# （sudo は環境変数を消すが、ここはもう sudo の中なので消えない）。
run_local() {
    local mode="$1"; shift
    RD_PROD_HOST="$PROD_HOST" RD_MIN_PHOTOS="$PROD_MIN" RD_SKIP_PY="$CHECK_ONLY" \
        RD_UNIT_DIR="$PROD_UNIT_DIR" RD_ETC_UNIT_DIR="$PROD_ETC_UNIT_DIR" RD_MARKER="$PROD_MARKER" \
        RD_ENV_FILE="$PROD_ENV_FILE" RD_ROOT_UID="$PROD_ROOT_UID" \
        bash -c "$REMOTE_SCRIPT" bash "$mode" "$BIN_PATH" "$CHECK_BASE" "$WAIT_SECS" "$@"
}

production_main() {
    local root_uid="$PROD_ROOT_UID" rc=0 head real_root sum

    refuse() { log "拒否: $*"; log "何も変更していない（終了コード 4）"; exit 4; }
    [ "$(id -u)" = "$root_uid" ] || refuse "root で実行すること（sudo を付ける）"

    if [ "$ROLLBACK" -eq 1 ]; then
        log "モード  : production rollback（${BIN_PATH}.prev へ戻す。ホスト名 ${PROD_HOST} を確認する）"
        run_local rollback "" || rc=$?
        case "$rc" in
            3) log "戻す途中で失敗した。上に出た手作業の復旧手順で戻すこと（終了コード 3）" ;;
            4) log "誤配布の防止で拒否された。何も変更していない（終了コード 4）" ;;
        esac
        exit "$rc"
    fi

    # checkout の検査。root で動くので、checkout の所有者が違っても git が「dubious ownership」で落ちないよう、
    # この checkout だけを safe.directory にする。fsmonitor（リポジトリの設定が起動するコマンド）は使わず、
    # index も書き換えない。追跡下のファイルの変更だけを見る（未追跡の app/.venv・.env・鍵は汚れとして数えない）
    real_root="$(cd "$REPO_ROOT" && pwd -P)"
    git_c() { GIT_OPTIONAL_LOCKS=0 git -c "safe.directory=${real_root}" -c core.fsmonitor=false -C "$real_root" "$@"; }
    head="$(git_c rev-parse HEAD 2>/dev/null)" || refuse "checkout の HEAD を調べられない（git の作業ツリーで実行すること）"
    [ "$head" = "$PROD_COMMIT" ] || refuse "checkout の HEAD が --expect-commit と違う（HEAD ${head:0:12}、期待 ${PROD_COMMIT:0:12}）"
    if ! git_c diff --quiet HEAD -- 2>/dev/null; then
        refuse "checkout の作業ツリーに追跡下のファイルの変更がある（git status で確認。変更したまま配らない）"
    fi
    [ -f "$LOCAL_UNIT" ] && [ ! -L "$LOCAL_UNIT" ] || refuse "unit が無い: ${LOCAL_UNIT#"$REPO_ROOT"/}"

    # バイナリを root 所有の一時ディレクトリへ写してから sha256 を取る（確かめた後、使う前に差し替えられないように）
    [ -f "$PROD_BIN" ] || refuse "--bin のバイナリが無い（通常のファイルではない）"
    STAGE_DIR="$(mktemp -d /tmp/geoframe-rs-deploy.XXXXXX)" || refuse "一時ディレクトリを作れない"
    trap 'rm -rf "$STAGE_DIR"' EXIT
    install -m 0755 -- "$PROD_BIN" "${STAGE_DIR}/geoframe" || refuse "バイナリを一時ディレクトリへ写せない"
    install -m 0644 -- "$LOCAL_UNIT" "${STAGE_DIR}/${UNIT_NAME}" || refuse "unit を一時ディレクトリへ写せない"
    sum="$(sha256sum "${STAGE_DIR}/geoframe" | cut -d' ' -f1)"
    [ "$sum" = "$PROD_SHA" ] || refuse "バイナリの sha256 が --expect-sha256 と違う（写し ${sum:0:16}…）"
    if command -v file >/dev/null 2>&1; then
        file "${STAGE_DIR}/geoframe" | grep -Eq 'statically linked|static-pie linked' \
            || refuse "バイナリが静的リンクではない"
    fi
    log "commit  : ${head:0:12}（作業ツリーに変更なし）"
    log "binary  : sha256 ${sum:0:16}…（一致）, $(stat -c %s "${STAGE_DIR}/geoframe") bytes"
    log "配置先  : ${BIN_PATH}（直前を ${BIN_PATH}.prev へ）、unit: ${PROD_UNIT_DIR}/${UNIT_NAME}"
    log "確認    : ${CHECK_BASE}/healthz, ${CHECK_BASE}/api/v1/photos（件数 >= ${PROD_MIN}。${WAIT_SECS} 秒まで待つ）"

    if [ "$CHECK_ONLY" -eq 1 ]; then
        log "検査    : ホスト名・検証環境の印・mask・設定のファイル・古い置き場所の unit（--check-only: Python 版の状態は除く）"
    else
        log "検査    : ホスト名・検証環境の印・mask・設定のファイル・古い置き場所の unit・Python 版の非稼働"
    fi
    run_local preflight "" || rc=$?
    case "$rc" in
        0) ;;
        4) log "誤配布の防止で拒否された。何も変更していない（終了コード 4。上の理由を見ること）"; exit 4 ;;
        *) die "検査を完了できない（終了コード ${rc}）。何も変更していない" ;;
    esac
    if [ "$CHECK_ONLY" -eq 1 ]; then
        log "--check-only: 検査はすべて通った。変更はしていない"
        exit 0
    fi

    rc=0
    run_local deploy "$STAGE_DIR" || rc=$?
    case "$rc" in
        0) ;;
        1) log "配布に失敗したが、配布前の状態へ戻した（終了コード 1）。初回の配布なら、cutover.md の手順で Python 版へ戻すこと" ;;
        3) log "配布に失敗し、戻す途中でも失敗した。上に出た手作業の復旧手順で戻すこと（終了コード 3）" ;;
        4) log "誤配布の防止で拒否された（終了コード 4）" ;;
    esac
    exit "$rc"
}

if [ -n "$PROD_HOST" ]; then
    # 本番の経路は、旅程の検査・ビルド・ssh を使わない。ここで終わる（production_main は必ず exit する）
    cd "$REPO_ROOT"
    production_main
fi

if [ "$ROLLBACK" -eq 1 ]; then
    log "モード  : rollback（${BIN_PATH}.prev へ戻す）"
    if [ "$DRY_RUN" -eq 1 ]; then
        log "*** DRY-RUN: ここで止める（ssh もしない）***"
        exit 0
    fi
    rc=0
    remote_run rollback || rc=$?
    case "$rc" in
        3) log "戻す途中で失敗した。上に出た手作業の復旧手順で戻すこと（終了コード 3）" ;;
        4) log "誤配布の防止で拒否された。何も変更していない（終了コード 4）" ;;
    esac
    exit "$rc"
fi

# --- 1. 埋め込む旅程の検査 ---
# crates/geoframe/build.rs は app/travel_blog/plans の *.json をバイナリへ埋め込む。git の管理外（未追跡・
# .gitignore で無視）のファイルやリンクがあると、同じコミットの表示でも手元だけの旅程が混ざって配られる
# （下の DIRTY は管理下の変更しか見ない）。名前や内容はログに出さず、件数と真偽だけ。
check_embedded_plans() {
    local dir="app/travel_blog/plans" path="" part found links=0 untracked=0 status
    # リポジトリの根から旅程までの各要素（app・app/travel_blog・plans）がリンクでないこと。親がリンクだと、
    # リンク先の（git 管理外の）ファイルが、下の検査も build.rs も通って埋め込まれる
    for part in app travel_blog plans; do
        path="${path:+${path}/}${part}"
        if [ -L "$path" ]; then
            links=$((links + 1))
        elif [ ! -e "$path" ]; then
            break
        fi
    done
    if [ "$links" -eq 0 ] && [ -e "$dir" ]; then
        # find の標準エラー（読めない子ディレクトリの名前）は捨てる。失敗は定型文で止める
        found="$(find "$dir" -type l -printf x 2>/dev/null)" \
            || die "埋め込む旅程のディレクトリを調べられない（読めないディレクトリがある。名前はここに出さない）"
        links=${#found}
        if [ "$links" -eq 0 ]; then
            status="$(git status --porcelain --untracked-files=all --ignored -- "$dir" 2>/dev/null)" \
                || die "埋め込む旅程の git の状態を調べられない（git の作業ツリーで実行すること）"
            untracked="$(printf '%s' "$status" | grep -c -E '^(\?\?|!!) ' || true)"
        fi
    fi
    log "旅程の検査: ${dir} の git 管理外のファイル ${untracked} 件、リンク ${links} 件"
    if [ "$links" -ne 0 ] || [ "$untracked" -ne 0 ]; then
        die "埋め込む旅程に git 管理外のファイルかリンクがある。取り除くかコミットしてから配ること（名前はここに出さない。git status --ignored で確認）"
    fi
}

# --- 2. ビルド ---
cd "$REPO_ROOT"
check_embedded_plans
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
DIRTY=""
git diff --quiet HEAD -- crates Cargo.toml Cargo.lock app/static app/vendor app/travel_blog/plans 2>/dev/null || DIRTY="（未コミットの変更あり）"
if [ "$DO_BUILD" -eq 1 ]; then
    log "ビルド : cargo build --release --target ${TARGET_TRIPLE} -p geoframe（commit ${COMMIT}${DIRTY}）"
    cargo build --release --target "$TARGET_TRIPLE" -p geoframe >&2
else
    log "ビルドを省略（--skip-build）"
fi
[ -x "$LOCAL_BIN" ] || die "成果物が無い: ${LOCAL_BIN}"
# 完全静的であること（動的リンクだと配布先のライブラリに依存する。build.md §2）
if command -v file >/dev/null 2>&1; then
    file "$LOCAL_BIN" | grep -Eq 'statically linked|static-pie linked' \
        || die "成果物が静的リンクではない（musl でビルドしたか確認）"
fi
[ -f "$LOCAL_UNIT" ] || die "unit が無い: ${LOCAL_UNIT}"
SUM="$(sha256sum "$LOCAL_BIN" | cut -d' ' -f1)"

log "commit  : ${COMMIT}${DIRTY}"
log "binary  : ${LOCAL_BIN#"$REPO_ROOT"/}（sha256 ${SUM:0:16}…, $(stat -c %s "$LOCAL_BIN") bytes）"
log "配置先  : ${BIN_PATH}（直前を ${BIN_PATH}.prev へ）、unit: /usr/local/lib/systemd/system/${UNIT_NAME}"
log "確認    : ${CHECK_BASE}/healthz, ${CHECK_BASE}/api/v1/photos（${WAIT_SECS} 秒まで待つ）"
log "拒否条件: 検証環境の印 /etc/geoframe/staging が無い、geoframe-server.service（Python 版）が非稼働（ActiveState が inactive／failed）と確認できない、古い置き場所（/etc/systemd/system）に unit の本体がある"
if [ "$DRY_RUN" -eq 1 ]; then
    log "*** DRY-RUN: 転送・配置・再起動は行わない ***"
    exit 0
fi

# --- 3. 誤配布の検査（何も転送・変更しないうちに。リモートの本体でも変更の前に再検査する）---
log "検査    : 接続先が検証環境か（印と Python 版の稼働）"
rc=0
remote_run preflight "''" || rc=$?
case "$rc" in
    0) ;;
    4) log "誤配布の防止で拒否された。何も変更していない（終了コード 4。上の理由を見ること）"; exit 4 ;;
    *) die "接続先の検査を完了できない（終了コード ${rc}）。何も変更していない" ;;
esac

# --- 4. 転送（リモートの一時ディレクトリへ。sha256 を照合）---
log "転送    : 検証環境の一時ディレクトリへ（sha256 で照合）"
STAGE="$(ssh_run 'mktemp -d /tmp/geoframe-rs-deploy.XXXXXX')"
case "$STAGE" in /tmp/geoframe-rs-deploy.*) ;; *) die "リモートの一時ディレクトリが想定外: ${STAGE}" ;; esac
cleanup() { ssh_run "rm -rf '${STAGE}'" >/dev/null 2>&1 || true; }
trap cleanup EXIT
scp -q -- "$LOCAL_BIN" "${SSH_TARGET}:${STAGE}/geoframe"
scp -q -- "$LOCAL_UNIT" "${SSH_TARGET}:${STAGE}/${UNIT_NAME}"
REMOTE_SUM="$(ssh_run "sha256sum '${STAGE}/geoframe'" | cut -d' ' -f1)"
[ "$REMOTE_SUM" = "$SUM" ] || die "転送後の sha256 が一致しない"

# --- 5〜6. 入れ替え・確認・失敗時の復元（リモート）---
rc=0
remote_run deploy "'${STAGE}'" || rc=$?
case "$rc" in
    0) ;;
    1) log "配布に失敗したが、配布前の状態へ戻した（終了コード 1）" ;;
    3) log "配布に失敗し、戻す途中でも失敗した。上に出た手作業の復旧手順で戻すこと（終了コード 3）" ;;
    4) log "誤配布の防止で拒否された（終了コード 4）" ;;
esac
exit "$rc"
