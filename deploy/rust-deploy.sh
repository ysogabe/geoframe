#!/usr/bin/env bash
#
# rust-deploy.sh - 手元で Rust 版 geoframe をビルドし、検証環境へ配布して入れ替える
#
# ============================================================================
# 対象は **検証環境（lxc-geoframe）だけ**。本番の切替は P5（docs/rust-migration/plan.md）で、
# ユーザーの立ち会いのもとで行う。本番機（Python 版 geoframe-server が動いている機体）へは
# 実行しないこと。接続先に既定値は無い（--target か GEOFRAME_SSH_TARGET で必ず指定）。
# 誤配布を止めるため、接続先に検証環境の印（/etc/geoframe/staging）が無いか、Python 版
# （geoframe-server.service）が非稼働と確認できなければ（稼働中・状態を取得できない場合も）、
# 何も変更せずに拒否する。
#
# deploy.sh（Python 版。本番機で git を取り込む薄いラッパー）とは別物で、こちらはバイナリを
# 手元でビルドして送る。やること:
#   1. cargo build --release --target x86_64-unknown-linux-musl -p geoframe（完全静的）
#   2. バイナリと unit を転送（sha256 で照合）
#   3. 配置先のバイナリを .prev に残して入れ替え、unit を入れて daemon-reload・enable・restart
#   4. /healthz と /api/v1/photos を確認。入れ替え以降のどこで失敗しても（再起動の失敗・途中の中断も）、
#      バイナリ・unit・enable の状態を配布前へ戻す。初回の配布なら disable して unit も撤去する
# ============================================================================
#
# 実行場所: 開発機（このリポジトリ内）。
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

TARGET_TRIPLE="x86_64-unknown-linux-musl"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCAL_BIN="${GEOFRAME_RS_LOCAL_BIN:-${REPO_ROOT}/target/${TARGET_TRIPLE}/release/geoframe}"
LOCAL_UNIT="${REPO_ROOT}/deploy/${UNIT_NAME}"

DRY_RUN=0
DO_BUILD=1
ROLLBACK=0

usage() {
    cat <<'USAGE'
使い方: rust-deploy.sh [--dry-run] [--skip-build] [--rollback] [--print-remote-script]
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

環境変数（任意）:
  GEOFRAME_RS_BIN          配置先のバイナリ（既定 /usr/local/bin/geoframe）
  GEOFRAME_RS_CHECK_BASE   確認の URL（既定 http://127.0.0.1:8080。検証環境の中から叩く）
  GEOFRAME_RS_WAIT_SECS    再起動後に /healthz を待つ秒数（既定 30）
  GEOFRAME_RS_LOCAL_BIN    配るバイナリ（既定は musl ビルドの成果物。巻き戻しの試験用に差し替える。
                           --skip-build と合わせる。静的リンクの検査は通す）

配置先のバイナリは入れ替えの直前に <配置先>.prev へ残す（直前の1世代だけ）。
unit も同じく /etc/systemd/system/geoframe-rs.service.prev に残す。
入れ替え以降（unit の導入・daemon-reload・enable・restart・確認）のどこで失敗しても、自動で配布前の
状態へ戻す（バイナリ・unit・enable・稼働の状態）。初回の配布なら、止めて disable し、新しく入れた
unit とバイナリ（geoframe.failed へ退避）を撤去する。

復元の最中は中断（SIGHUP・SIGINT・SIGTERM。ssh の切断）を無視し、復元が終わってから終了する（終了コードは復元の結果）。
復元で止める操作は、stop の成功と、inactive/failed の確認の両方が要る（確認できなければ終了コード 3）。

終了コード: 0 成功 / 1 失敗したが配布前の状態へ戻した / 3 戻す途中で失敗した（手作業の復旧が要る。
            手順が表示される）/ 4 誤配布の防止のため拒否した（何も変更していない）/ 2 引数の誤り。

誤配布の防止（変更の前に検査。--rollback も同じ）:
  - 接続先に検証環境の印 /etc/geoframe/staging（通常ファイル）があること。
    検証環境で一度だけ作る:  sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging
  - geoframe-server.service（Python 版）の ActiveState が inactive/failed と確認できること
    （未導入は not-found/inactive で通る。LoadState は見ない: unit ファイルが無くても動いている
    サービスはありうるので、not-found/active も拒否。稼働中・状態を取得できない・空出力・未知の状態・状態の行の欠け／重複も拒否）。
  本番の切替は P5 で、専用の手順と明示的なフラグをそのとき作る（このスクリプトは本番に配れない）。

本番機には実行しないこと（本番の切替は P5）。
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
# 試験用（モック）に差し替えられる。検証環境では既定のまま。
unit_dir="${GEOFRAME_RS_TEST_UNIT_DIR:-/etc/systemd/system}"
marker="${GEOFRAME_RS_TEST_MARKER:-/etc/geoframe/staging}"

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

# 誤配布の防止。変更の前に必ず通す。
preflight() {
    if [ ! -f "$marker" ] || [ -L "$marker" ]; then
        say "拒否: 検証環境の印（${marker}）が無い。ここは検証環境ではない可能性がある。何も変更していない"
        say "      検証環境なら、一度だけ  install -d /etc/geoframe && touch ${marker}  を実行する"
        return 4
    fi
    # Python 版は『非稼働と確認できた』ときだけ通す。状態を取得できない（systemd に繋がらない・空出力）や
    # 未知の状態を非稼働とみなして通すと、本番機へ配れてしまう。
    if ! unit_state "$py_unit"; then
        say "拒否: Python 版（${py_unit}）の状態を取得できない。非稼働と確認できないので何も変更していない"
        return 4
    fi
    # LoadState は見ない。unit ファイルを消して daemon-reload しても、動いているサービスは動き続けうる
    # （not-found と active は両立する）。未導入の機体は not-found/inactive になり、そのまま通る。
    case "$u_active" in
        inactive|failed) return 0 ;;
        active|activating|reloading|deactivating)
            say "拒否: Python 版（${py_unit}）が稼働中（${u_active}）。本番機の可能性がある。何も変更していない"
            return 4 ;;
        *)
            say "拒否: Python 版（${py_unit}）の状態が不明（${u_active}）。非稼働と確認できないので何も変更していない"
            return 4 ;;
    esac
}

# /healthz が 200 で、/api/v1/photos が JSON 配列（先頭が [）で返ること。
check() {
    local i=0
    until curl -fsS --max-time 3 -o /dev/null "${base}/healthz" 2>/dev/null; do
        i=$((i + 1))
        [ "$i" -lt "$wait_secs" ] || { say "/healthz が ${wait_secs} 秒以内に通らない"; return 1; }
        sleep 1
    done
    local code head
    code="$(curl -sS --max-time 30 -o /dev/null -w '%{http_code}' "${base}/api/v1/photos" 2>/dev/null)" || code=""
    [ "$code" = "200" ] || { say "/api/v1/photos が 200 でない（${code:-接続できない}）"; return 1; }
    # 先頭の 1 バイトだけ読む（head が先に閉じて curl が書き込みエラーになっても無視する）
    head="$( { curl -fsS --max-time 30 "${base}/api/v1/photos" 2>/dev/null || true; } | head -c 1)"
    [ "$head" = "[" ] || { say "/api/v1/photos が JSON 配列を返さない"; return 1; }
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
    check || { fail "再起動後の確認"; return 1; }
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
                      SSH_TARGET="$2"; shift 2 ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "ERROR: 不明な引数: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[ -n "$SSH_TARGET" ] || { usage >&2; die "--target user@host または GEOFRAME_SSH_TARGET の指定が必要。"; }

# リモートのシェルへ値を埋め込むので、パスと URL は使える文字を絞る（引用符の崩れ・注入を防ぐ）。
case "$BIN_PATH" in
    /*[!A-Za-z0-9_./-]*|"") die "GEOFRAME_RS_BIN は絶対パスで、英数字と _ . / - だけ。" ;;
    /*) ;;
    *) die "GEOFRAME_RS_BIN は絶対パスで指定する。" ;;
esac
[[ "$CHECK_BASE" =~ ^http://[A-Za-z0-9._:-]+$ ]] || die "GEOFRAME_RS_CHECK_BASE は http://host:port の形（パスなし）。"
case "$WAIT_SECS" in
    ''|*[!0-9]*) die "GEOFRAME_RS_WAIT_SECS は整数。" ;;
esac

# ssh の接続先。'-' 始まりの値が ssh のオプション（-oProxyCommand=…）として解釈されると、
# 手元で任意コマンドが実行される。deploy.sh と同じく -- で打ち切る。
ssh_run() { ssh -- "$SSH_TARGET" "$@"; }

# リモートの本体を、モードを指定して実行する（-t は付けない。検証環境は NOPASSWD sudo。
# 標準入力を本体の読み込みに使うので tty は割り当てない）。
remote_run() {
    local mode="$1"; shift
    ssh -- "$SSH_TARGET" "sudo bash -s -- ${mode} '${BIN_PATH}' '${CHECK_BASE}' '${WAIT_SECS}' $*" <<<"$REMOTE_SCRIPT"
}

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

# --- 1. ビルド ---
cd "$REPO_ROOT"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
DIRTY=""
git diff --quiet HEAD -- crates Cargo.toml Cargo.lock app/static app/vendor 2>/dev/null || DIRTY="（未コミットの変更あり）"
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
log "配置先  : ${BIN_PATH}（直前を ${BIN_PATH}.prev へ）、unit: /etc/systemd/system/${UNIT_NAME}"
log "確認    : ${CHECK_BASE}/healthz, ${CHECK_BASE}/api/v1/photos（${WAIT_SECS} 秒まで待つ）"
log "拒否条件: 検証環境の印 /etc/geoframe/staging が無い、または geoframe-server.service（Python 版）が非稼働（ActiveState が inactive／failed）と確認できない"
if [ "$DRY_RUN" -eq 1 ]; then
    log "*** DRY-RUN: 転送・配置・再起動は行わない ***"
    exit 0
fi

# --- 2. 誤配布の検査（何も転送・変更しないうちに。リモートの本体でも変更の前に再検査する）---
log "検査    : 接続先が検証環境か（印と Python 版の稼働）"
rc=0
remote_run preflight "''" || rc=$?
case "$rc" in
    0) ;;
    4) log "誤配布の防止で拒否された。何も変更していない（終了コード 4。上の理由を見ること）"; exit 4 ;;
    *) die "接続先の検査を完了できない（終了コード ${rc}）。何も変更していない" ;;
esac

# --- 3. 転送（リモートの一時ディレクトリへ。sha256 を照合）---
log "転送    : 検証環境の一時ディレクトリへ（sha256 で照合）"
STAGE="$(ssh_run 'mktemp -d /tmp/geoframe-rs-deploy.XXXXXX')"
case "$STAGE" in /tmp/geoframe-rs-deploy.*) ;; *) die "リモートの一時ディレクトリが想定外: ${STAGE}" ;; esac
cleanup() { ssh_run "rm -rf '${STAGE}'" >/dev/null 2>&1 || true; }
trap cleanup EXIT
scp -q -- "$LOCAL_BIN" "${SSH_TARGET}:${STAGE}/geoframe"
scp -q -- "$LOCAL_UNIT" "${SSH_TARGET}:${STAGE}/${UNIT_NAME}"
REMOTE_SUM="$(ssh_run "sha256sum '${STAGE}/geoframe'" | cut -d' ' -f1)"
[ "$REMOTE_SUM" = "$SUM" ] || die "転送後の sha256 が一致しない"

# --- 4〜5. 入れ替え・確認・失敗時の復元（リモート）---
rc=0
remote_run deploy "'${STAGE}'" || rc=$?
case "$rc" in
    0) ;;
    1) log "配布に失敗したが、配布前の状態へ戻した（終了コード 1）" ;;
    3) log "配布に失敗し、戻す途中でも失敗した。上に出た手作業の復旧手順で戻すこと（終了コード 3）" ;;
    4) log "誤配布の防止で拒否された（終了コード 4）" ;;
esac
exit "$rc"
