#!/usr/bin/env bash
#
# rust-deploy-test.sh - rust-deploy.sh の検証環境側の本体を、モックの systemctl/curl で試験する
#
# ssh もしない・実際のサービスにも触らない。一時ディレクトリに「検証環境」を作り、
# `rust-deploy.sh --print-remote-script` で取り出した本体を一般ユーザーのまま走らせる。
# 見ること: 誤配布の拒否、成功、各工程（再起動・daemon-reload・enable・unit の導入・確認）の失敗からの
# 復元、復元の mv の失敗（手作業の手順が出て終了コード 3）、初回の配布の失敗、中断（SIGTERM）。
# [12] 以降は本番の指定（--production。ssh なし・その機体で直接）: 一時の git リポジトリにスクリプトと unit を
# 写して走らせ、ホスト名・印・Python 版の稼働・mask・sha256・commit・作業ツリー・件数の下限・--check-only・
# --rollback・unit の置き場所・古い置き場所の本体・数の引数の上限・環境変数で検査を外せないことを確かめる。
# 本番の経路は GEOFRAME_RS_* を読まない（あれば断る）ので、配置先・確認の URL・印・設定のファイル・root の UID は、
# 一時コピーの中の固定値（PROD_*）を書き換えて向ける（geoframe-admin-test.sh と同じ方式。root の UID は
# この試験を動かしているユーザー）。検証環境の経路（[1]〜[11]）だけが GEOFRAME_RS_TEST_* で差し替える。
#
# 実行: bash deploy/rust-deploy-test.sh
#
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE="$(bash "$HERE/rust-deploy.sh" --print-remote-script)"
T="$(mktemp -d /tmp/rust-deploy-test.XXXXXX)"
trap 'rm -rf "$T"' EXIT

pass=0 failn=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
ng()   { failn=$((failn + 1)); echo "  FAIL $*"; }
expect() { # expect <説明> <条件コマンド...>
    local d="$1"; shift
    if "$@"; then ok "$d"; else ng "$d"; fi
}

# --- モック ---
mkdir -p "$T/mock"
cat > "$T/mock/systemctl" <<'M'
#!/usr/bin/env bash
S="$MOCK_DIR/state"; C="$MOCK_DIR/ctl"
bump() { # bump <名前>: 制御ファイルの回数が残っていれば1減らして 0（=失敗させる）を返す
    local f="$C/$1" n; [ -f "$f" ] || return 1
    n=$(cat "$f"); [ "$n" -gt 0 ] || return 1
    echo $((n - 1)) > "$f"; return 0
}
# 状態の表示。rs_state / py_state に任意の ActiveState を置ける（active のファイルがあれば active）
rs_active() { if [ -f "$S/active" ]; then echo active; elif [ -f "$S/rs_state" ]; then cat "$S/rs_state"; else echo inactive; fi; }
py_active() { if [ -f "$S/py_active" ]; then echo active; elif [ -f "$S/py_state" ]; then cat "$S/py_state"; else echo inactive; fi; }
cmd="" unit=""
for a in "$@"; do case "$a" in -*) ;; *) if [ -z "$cmd" ]; then cmd="$a"; else unit="$a"; fi ;; esac; done
echo "$cmd $unit" >> "$MOCK_DIR/calls.log"
case "$cmd" in
is-active)
    if [ "$unit" = geoframe-server.service ]; then st="$(py_active)"; else st="$(rs_active)"; fi
    echo "$st"; [ "$st" = active ] ;;
show)
    # ctl/py_show, ctl/rs_show: fail（systemd へ繋がらない: 標準エラーだけ・終了コード1）| empty（空出力・終了コード0）|
    #   dup_conflict（ActiveState が inactive のあとに active）| dup_same（ActiveState が inactive で2行）|
    #   dup_load（LoadState が2行）
    if [ "$unit" = geoframe-server.service ]; then tag=py; else tag=rs; fi
    if [ -f "$C/${tag}_show" ]; then
        case "$(cat "$C/${tag}_show")" in
            fail)  echo "Failed to connect to bus: No such file or directory" >&2; exit 1 ;;
            empty) exit 0 ;;
            dup_conflict) printf 'LoadState=loaded\nActiveState=inactive\nActiveState=active\n'; exit 0 ;;
            dup_same)     printf 'LoadState=loaded\nActiveState=inactive\nActiveState=inactive\n'; exit 0 ;;
            dup_load)     printf 'LoadState=loaded\nLoadState=loaded\nActiveState=inactive\n'; exit 0 ;;
        esac
    fi
    if [ "$tag" = py ]; then
        load=loaded; [ -f "$S/py_load" ] && load="$(cat "$S/py_load")"
        act="$(py_active)"
    else
        if [ -f "$MOCK_UNIT" ]; then load=loaded; else load=not-found; fi
        act="$(rs_active)"
    fi
    printf 'ActiveState=%s\nLoadState=%s\n' "$act" "$load"; exit 0 ;;
is-enabled)
    if [ "$unit" != geoframe-server.service ] && [ -f "$S/masked" ]; then echo masked; exit 1; fi
    if [ -f "$S/enabled" ]; then echo enabled; exit 0; fi
    if [ -f "$MOCK_UNIT" ]; then echo disabled; exit 1; fi
    exit 1 ;;
enable)  bump fail_enable && exit 1; touch "$S/enabled"; exit 0 ;;
disable) bump fail_disable && exit 1; rm -f "$S/enabled"; exit 0 ;;
daemon-reload) bump fail_reload && exit 1; exit 0 ;;
reset-failed) exit 0 ;;
stop)
    bump fail_stop && exit 1
    if [ ! -f "$MOCK_UNIT" ]; then echo "Failed to stop $unit: Unit $unit not loaded." >&2; exit 5; fi
    rm -f "$S/active"; exit 0 ;;
restart)
    n=0; [ -f "$MOCK_DIR/restart_n" ] && n=$(cat "$MOCK_DIR/restart_n"); n=$((n + 1)); echo "$n" > "$MOCK_DIR/restart_n"
    # n 回目の restart で、呼び出し元（本体のシェル）へ SIGTERM を送る（復元の最中の中断を決まった時点で起こす）
    if [ -f "$C/term_on_restart" ] && grep -qw "$n" "$C/term_on_restart"; then echo "$n" >> "$MOCK_DIR/term_sent"; kill -TERM "$PPID"; fi
    [ -f "$C/hang" ] && sleep 30
    if [ -f "$C/poison_prev" ]; then rm -rf "$MOCK_BIN.prev"; mkdir "$MOCK_BIN.prev"; rm -f "$C/poison_prev"; fi
    if bump fail_restart; then rm -f "$S/active"; exit 1; fi
    if [ -f "$MOCK_BIN" ] && grep -q BADSTART "$MOCK_BIN"; then rm -f "$S/active"; exit 1; fi
    touch "$S/active"; exit 0 ;;
*) exit 0 ;;
esac
M
cat > "$T/mock/curl" <<'M'
#!/usr/bin/env bash
S="$MOCK_DIR/state"; url="" w=0 out=""
while [ $# -gt 0 ]; do
    case "$1" in
        http://*) url="$1" ;;
        -w) w=1 ;;
        -o) shift; out="$1" ;;
    esac
    shift
done
if [ ! -f "$S/active" ]; then [ "$w" = 1 ] && printf 000; exit 7; fi
case "$url" in
*/healthz) if grep -q BADHEALTH "$MOCK_BIN" 2>/dev/null; then exit 22; fi; exit 0 ;;
*/api/v1/photos)
    body='[]'; [ -f "$MOCK_DIR/ctl/photos_json" ] && body="$(cat "$MOCK_DIR/ctl/photos_json")"
    grep -q BADARRAY "$MOCK_BIN" 2>/dev/null && body='{"a":1}'
    if [ -n "$out" ] && [ "$out" != /dev/null ]; then printf '%s' "$body" > "$out"; else [ -z "$out" ] && printf '%s' "$body"; fi
    [ "$w" = 1 ] && printf 200
    exit 0 ;;
esac
exit 0
M
cat > "$T/mock/hostname" <<'M'
#!/usr/bin/env bash
echo "${MOCK_HOSTNAME:-prod-host-for-test}"
M
cat > "$T/mock/journalctl" <<'M'
#!/usr/bin/env bash
exit 0
M
# 手元のラッパー（rust-deploy.sh 本体）を通す試験用。ssh は接続先を捨てて、コマンドをこの機体でそのまま実行する
# （sudo は外す。標準入力はそのまま本体へ渡る）。scp はローカルのコピー。
cat > "$T/mock/ssh" <<'M'
#!/usr/bin/env bash
while [ $# -gt 0 ]; do case "$1" in --) shift; break ;; -*) shift ;; *) break ;; esac; done
shift   # 接続先
cmd="$*"
echo "ssh ${cmd%% *}" >> "$T_LOG"
cmd="${cmd#sudo }"
eval "$cmd"
M
cat > "$T/mock/scp" <<'M'
#!/usr/bin/env bash
args=(); for a in "$@"; do case "$a" in -q|--) ;; *) args+=("$a") ;; esac; done
echo "scp" >> "$T_LOG"
cp "${args[0]}" "${args[1]#*:}"
M
cat > "$T/mock/file" <<'M'
#!/usr/bin/env bash
echo "$1: ELF 64-bit LSB executable, statically linked"
M
chmod +x "$T/mock/"*
export T_LOG="$T/wrapper.log"

# --- 場面の用意 ---
export MOCK_DIR="$T/env" MOCK_BIN="$T/env/bin/geoframe" MOCK_UNIT="$T/env/units/geoframe-rs.service"
export GEOFRAME_RS_TEST_UNIT_DIR="$T/env/units" GEOFRAME_RS_TEST_MARKER="$T/env/etc/staging"
# 古い置き場所（/etc/systemd/system）と設定のファイルも一時ディレクトリへ。root の判定はこの試験のユーザーに
export GEOFRAME_RS_TEST_ETC_UNIT_DIR="$T/env/etc-units" GEOFRAME_RS_TEST_ENV_FILE="$T/env/etc/geoframe-rs"
export GEOFRAME_RS_TEST_ROOT_UID="$(id -u)"
REAL_GIT="$(command -v git)"
export PATH="$T/mock:$PATH"

# 検証環境を作り直す。引数: installed（バイナリ・unit・enable・稼働が済み）| fresh（何も無い）
setup() {
    rm -rf "$T/env" "$T/stage"
    mkdir -p "$T/env/bin" "$T/env/units" "$T/env/etc" "$T/env/etc-units" "$T/env/state" "$T/env/ctl" "$T/stage"
    touch "$T/env/etc/staging"
    printf 'GEOFRAME_SECRET_FOR_TEST=ENV-SECRET\n' > "$GEOFRAME_RS_TEST_ENV_FILE"; chmod 0600 "$GEOFRAME_RS_TEST_ENV_FILE"
    printf 'OLD-BIN\n' > "$T/stage/old.bin"
    printf 'NEW-BIN\n' > "$T/stage/geoframe"
    printf 'NEW-UNIT\n' > "$T/stage/geoframe-rs.service"
    if [ "${1:-installed}" = installed ]; then
        printf 'OLD-BIN\n' > "$MOCK_BIN"; chmod 0755 "$MOCK_BIN"
        printf 'OLD-UNIT\n' > "$MOCK_UNIT"
        touch "$T/env/state/enabled" "$T/env/state/active"
    fi
    : > "$T/env/calls.log"
}
run() { # run <mode> → 出力は $T/out、終了コードは $rc
    local stage=""; [ "$1" = deploy ] && stage="$T/stage"
    bash -c "$REMOTE" bash "$1" "$MOCK_BIN" http://127.0.0.1:1 2 "$stage" > "$T/out" 2>&1
    rc=$?
}
content() { cat "$1" 2>/dev/null; }
no_restored() { ! grep -q '戻した（' "$T/out"; }

echo "[1] 誤配布の防止"
setup; rm -f "$GEOFRAME_RS_TEST_MARKER"; run deploy
expect "印が無ければ拒否（終了コード4）" test "$rc" = 4
expect "拒否したとき何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a ! -e "$MOCK_BIN.new"
setup; touch "$T/env/state/py_active"; run deploy
expect "Python 版が稼働中なら拒否（終了コード4）" test "$rc" = 4
expect "拒否したとき何も変えていない（Python 版）" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev"
setup; ln -s /etc/hostname "$T/env/etc/staging2"; rm "$GEOFRAME_RS_TEST_MARKER"; ln -s "$T/env/etc/staging2" "$GEOFRAME_RS_TEST_MARKER"; run deploy
expect "印がシンボリックリンクなら拒否" test "$rc" = 4
setup; rm -f "$GEOFRAME_RS_TEST_MARKER"; run rollback
expect "rollback も印が無ければ拒否" test "$rc" = 4
setup; run preflight
expect "preflight は印があれば成功" test "$rc" = 0
# Python 版の状態を取得できないときは、非稼働とみなさず拒否する（inactive/failed か、unit が無いと確認できたときだけ通す）
setup; echo fail > "$T/env/ctl/py_show"; run deploy
expect "Python 版の状態取得が失敗（systemd に繋がらない）なら拒否（終了コード4）" test "$rc" = 4
expect "状態取得の失敗で拒否したとき何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a ! -e "$MOCK_BIN.new"
setup; echo empty > "$T/env/ctl/py_show"; run deploy
expect "Python 版の状態が空出力なら拒否（終了コード4）" test "$rc" = 4
for dup in dup_conflict dup_same dup_load; do
    setup; echo "$dup" > "$T/env/ctl/py_show"; run deploy
    expect "Python 版の状態行が重複（$dup）なら拒否（終了コード4）" test "$rc" = 4
    expect "重複（$dup）で拒否したとき何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a ! -e "$MOCK_BIN.new"
    setup; echo "$dup" > "$T/env/ctl/py_show"; run rollback
    expect "rollback も状態行が重複（$dup）なら拒否（終了コード4）" test "$rc" = 4
done
setup; echo maintenance > "$T/env/state/py_state"; run deploy
expect "Python 版の状態が未知の値なら拒否（終了コード4）" test "$rc" = 4
setup; echo activating > "$T/env/state/py_state"; run deploy
expect "Python 版が activating なら拒否（終了コード4）" test "$rc" = 4
setup; echo fail > "$T/env/ctl/py_show"; run rollback
expect "rollback も状態取得の失敗なら拒否（終了コード4）" test "$rc" = 4
setup; echo failed > "$T/env/state/py_state"; run preflight
expect "Python 版が failed なら通す" test "$rc" = 0
setup; echo not-found > "$T/env/state/py_load"; run preflight
expect "Python 版が未導入（not-found/inactive）なら通す" test "$rc" = 0
setup; echo not-found > "$T/env/state/py_load"; touch "$T/env/state/py_active"; run deploy
expect "Python 版の unit ファイルが無くても稼働中（not-found/active）なら拒否（終了コード4）" test "$rc" = 4
expect "not-found/active で拒否したとき何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a ! -e "$MOCK_BIN.new"
setup; echo not-found > "$T/env/state/py_load"; echo deactivating > "$T/env/state/py_state"; run preflight
expect "not-found/deactivating も拒否（終了コード4）" test "$rc" = 4
setup; echo not-found > "$T/env/state/py_load"; touch "$T/env/state/py_active"; run rollback
expect "rollback も not-found/active なら拒否（終了コード4）" test "$rc" = 4

echo "[2] 成功"
setup; run deploy
expect "成功は終了コード0" test "$rc" = 0
expect "新しいバイナリと unit が入り、.prev に旧版が残る" test "$(content "$MOCK_BIN")" = NEW-BIN -a "$(content "$MOCK_BIN.prev")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = NEW-UNIT -a "$(content "$MOCK_UNIT.prev")" = OLD-UNIT
expect "稼働・enable のまま" test -f "$T/env/state/active" -a -f "$T/env/state/enabled"

echo "[3] 入れ替え以降の各工程の失敗から、配布前へ戻る"
back_to_old() { test "$(content "$MOCK_BIN")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = OLD-UNIT -a -f "$T/env/state/active" -a -f "$T/env/state/enabled"; }
setup; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "restart の失敗: 終了コード1" test "$rc" = 1
expect "restart の失敗: バイナリ・unit・稼働・enable が元どおり" back_to_old
expect "restart の失敗: 失敗した方が .failed に残る" test "$(content "$MOCK_BIN.failed")" = NEW-BIN
setup; printf 'BADSTART\n' > "$T/stage/geoframe"; run deploy
expect "起動できないバイナリ: 終了コード1・元どおり" test "$rc" = 1 -a "$(content "$MOCK_BIN")" = OLD-BIN
setup; printf 'BADHEALTH\n' > "$T/stage/geoframe"; run deploy
expect "/healthz が通らない: 終了コード1・元どおり" test "$rc" = 1 -a "$(content "$MOCK_BIN")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = OLD-UNIT
setup; echo 1 > "$T/env/ctl/fail_reload"; run deploy
expect "daemon-reload の失敗: 終了コード1・元どおり" test "$rc" = 1
expect "daemon-reload の失敗: 元どおり" back_to_old
setup; echo 1 > "$T/env/ctl/fail_enable"; run deploy
expect "enable の失敗: 終了コード1・元どおり" test "$rc" = 1
expect "enable の失敗: 元どおり" back_to_old
setup; rm "$T/stage/geoframe-rs.service"; run deploy
expect "unit の導入の失敗: 終了コード1" test "$rc" = 1
expect "unit の導入の失敗: 元どおり" back_to_old
setup; rm -f "$T/env/state/enabled"; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "もとが disabled のとき、失敗後も disabled に戻る" test "$rc" = 1 -a ! -f "$T/env/state/enabled" -a "$(content "$MOCK_UNIT")" = OLD-UNIT
setup; rm -f "$T/env/state/active"; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "もとが停止中のとき、失敗後も止めたまま" test "$rc" = 1 -a ! -f "$T/env/state/active" -a "$(content "$MOCK_BIN")" = OLD-BIN

echo "[4] 復元の mv の失敗（手作業の手順を出し、終了コード3。『戻した』と言わない）"
setup; echo 1 > "$T/env/ctl/fail_restart"; touch "$T/env/ctl/poison_prev"; run deploy
expect "mv の失敗: 終了コード3" test "$rc" = 3
expect "mv の失敗: 手作業の復旧手順が出る" grep -q '手で戻すこと' "$T/out"
expect "mv の失敗（検証環境）: 手順の文言は『検証環境で』で、本番の案内（cutover.md）は出ない" bash -c 'grep -q "検証環境で root として手で戻すこと" "$1" && ! grep -q "本番機で root" "$1" && ! grep -q "cutover.md" "$1"' _ "$T/out"
expect "mv の失敗: 失敗した操作が出る" grep -q "失敗した操作: mv " "$T/out"
expect "mv の失敗: 『戻した』と表示しない" no_restored
setup; run deploy; chmod a-w "$T/env/bin"; run rollback; chmod u+w "$T/env/bin"
expect "rollback の mv の失敗: 終了コード3・手順が出る" test "$rc" = 3
expect "rollback の mv の失敗: 『戻した』と表示しない" no_restored
setup; echo 1 > "$T/env/ctl/fail_restart"; echo 5 > "$T/env/ctl/fail_reload"; run deploy
expect "復元の daemon-reload も失敗: 終了コード3・手順が出る" test "$rc" = 3
expect "復元の daemon-reload も失敗: 『戻した』と表示しない" no_restored

# 復元は済んだが、戻したあとの確認が通らない（旧版も /healthz が通らない）: mv の手順は出さず、止める手順を先に出す
setup; printf 'BADHEALTH\n' > "$MOCK_BIN"; printf 'BADHEALTH\n' > "$T/stage/geoframe"; run deploy
expect "復元後の確認の失敗: 終了コード3" test "$rc" = 3
expect "復元後の確認の失敗: 先に止める手順が出る" grep -q 'systemctl stop geoframe-rs.service' "$T/out"
expect "復元後の確認の失敗: 使い済みの .prev への mv は案内しない" bash -c '! grep -q "直前のバイナリへ: mv" "$1"' _ "$T/out"
expect "復元後の確認の失敗: 『戻した』と表示しない" no_restored

echo "[5] 初回の配布の失敗（戻す先が無い）"
setup fresh; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "初回の失敗: 終了コード1" test "$rc" = 1
expect "初回の失敗: unit が撤去され、disable され、止まっている" test ! -e "$MOCK_UNIT" -a ! -f "$T/env/state/enabled" -a ! -f "$T/env/state/active"
expect "初回の失敗: バイナリは .failed へ退避し、配置先は空" test ! -e "$MOCK_BIN" -a "$(content "$MOCK_BIN.failed")" = NEW-BIN
setup fresh; run deploy
expect "初回の成功: 終了コード0・enable・稼働" test "$rc" = 0 -a -f "$T/env/state/enabled" -a -f "$T/env/state/active" -a "$(content "$MOCK_UNIT")" = NEW-UNIT
setup fresh; printf 'STALE-PREV\n' > "$MOCK_BIN.prev"; printf 'STALE-UNIT\n' > "$MOCK_UNIT.prev"; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "初回の失敗: 古い .prev を戻す先として使わない" test "$rc" = 1 -a ! -e "$MOCK_BIN" -a ! -e "$MOCK_UNIT"

echo "[6] 中断（SIGTERM）"
setup; touch "$T/env/ctl/hang"
setsid bash -c "$REMOTE" bash deploy "$MOCK_BIN" http://127.0.0.1:1 2 "$T/stage" > "$T/out" 2>&1 &
pid=$!
sleep 1.5; kill -TERM -- "-$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rc=$?
rm -f "$T/env/ctl/hang"
expect "restart 中の中断: 元どおりに戻る" back_to_old
expect "restart 中の中断: 中断の旨が出る" grep -q '変更の途中で終了した' "$T/out"

# 復元の最中に SIGTERM が来ても、復元をやり切ってから終了する（モックの restart が本体へ SIGTERM を送る）
setup; echo 1 > "$T/env/ctl/fail_restart"; echo 2 > "$T/env/ctl/term_on_restart"; run deploy
expect "復元中の SIGTERM（通常の復元）: 復元をやり切って終了コード1" test "$rc" = 1
expect "復元中の SIGTERM（通常の復元）: 元どおりで稼働している" back_to_old
expect "復元中の SIGTERM（通常の復元）: 戻したあとの確認まで行う" grep -q '戻した（直前のバイナリで確認 OK）' "$T/out"
setup; echo 1 2 > "$T/env/ctl/term_on_restart"; run deploy
expect "復元中の SIGTERM（EXIT trap 内の復元）: 復元をやり切って終了コード1" test "$rc" = 1
expect "復元中の SIGTERM（EXIT trap 内の復元）: 元どおりで稼働している" back_to_old
expect "復元中の SIGTERM（EXIT trap 内の復元）: 中断の旨と『戻した』が出る" bash -c 'grep -q "変更の途中で終了した" "$1" && grep -q "戻した（直前のバイナリで確認 OK）" "$1"' _ "$T/out"
setup; run deploy; echo 2 > "$T/env/ctl/term_on_restart"; run rollback
expect "復元中の SIGTERM（手動 rollback）: 最後まで戻して終了コード0" test "$rc" = 0
expect "復元中の SIGTERM（手動 rollback）: 旧版で稼働している" back_to_old
# 復元が途中で失敗するときは、中断が重なっても手作業の手順と終了コード3を出す
# 復元時（2回目）の restart の失敗と、その restart が送る SIGTERM を重ねる（SIGTERM を実際に送ったことも確かめる）
setup; echo 2 > "$T/env/ctl/fail_restart"; echo 2 > "$T/env/ctl/term_on_restart"; run deploy
expect "復元の失敗と SIGTERM が重なっても: SIGTERM を2回目の restart で実際に送った" test "$(content "$T/env/term_sent")" = 2
expect "復元の失敗と SIGTERM が重なっても: 終了コード3・手順が出る" bash -c 'test "$1" = 3 && grep -q "手で戻すこと" "$2" && grep -q "失敗した操作: systemctl restart" "$2"' _ "$rc" "$T/out"
expect "復元の失敗と SIGTERM が重なっても: 『戻した』と表示しない" no_restored

# 復元の中で致命的なシェルエラー（未定義変数の参照）が起きても、手作業の手順と終了コード3を出す。
# 一時コピーの restore() の先頭に未定義変数の参照を注入して試験する（set -u で、その場のシェルが終了する）
REMOTE_ORIG="$REMOTE"
REMOTE="$(printf '%s\n' "$REMOTE_ORIG" | sed 's/^restore() {$/restore() { : "$UNDEFINED_VAR_FOR_TEST"/')"
expect "試験の準備: 未定義変数の参照を注入できた" bash -c 'printf "%s" "$1" | grep -q "^restore() { : \"\$UNDEFINED_VAR_FOR_TEST\""' _ "$REMOTE"
setup; echo 1 > "$T/env/ctl/term_on_restart"; run deploy
expect "EXIT trap 内の復元が異常終了: 終了コード3（143にならない）" test "$rc" = 3
expect "EXIT trap 内の復元が異常終了: 手作業の復旧手順が出る" grep -q '手で戻すこと' "$T/out"
expect "EXIT trap 内の復元が異常終了: 想定外の終了が出る・『戻した』と表示しない" bash -c 'grep -q "復元の最中に想定外の終了" "$1" && ! grep -q "戻した（" "$1"' _ "$T/out"
expect "EXIT trap 内の復元が異常終了: SIGTERM を実際に送った" test "$(content "$T/env/term_sent")" = 1
setup; echo 1 > "$T/env/ctl/fail_restart"; run deploy
expect "通常の復元が異常終了: 終了コード3・手順が出る" bash -c 'test "$1" = 3 && grep -q "手で戻すこと" "$2"' _ "$rc" "$T/out"
setup; run deploy; run rollback
expect "手動 rollback の復元が異常終了: 終了コード3・手順が出る" bash -c 'test "$1" = 3 && grep -q "手で戻すこと" "$2"' _ "$rc" "$T/out"
REMOTE="$REMOTE_ORIG"

echo "[7] 手動の rollback"
setup; run deploy; run rollback
expect "rollback の成功: 終了コード0・旧版に戻る" test "$rc" = 0 -a "$(content "$MOCK_BIN")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = OLD-UNIT
setup; run rollback
expect ".prev が無い rollback: 終了コード1" test "$rc" = 1

echo "[8] stop の失敗・状態取得の失敗（止まったと確認できないなら、戻せていない = 終了コード3）"
# 配布前は停止中（r_service=stop）で、restart の失敗から復元に入る
stopped_fail() { setup; rm -f "$T/env/state/active"; echo 1 > "$T/env/ctl/fail_restart"; }
stopped_fail; echo 5 > "$T/env/ctl/fail_stop"; echo activating > "$T/env/state/rs_state"; run deploy
expect "stop の失敗＋activating: 終了コード3・手順が出る" bash -c 'test "$1" = 3 && grep -q "手で戻すこと" "$2" && grep -q "サービスを止められない" "$2"' _ "$rc" "$T/out"
expect "stop の失敗＋activating: 『戻した』と表示しない" no_restored
stopped_fail; echo activating > "$T/env/state/rs_state"; run deploy
expect "stop は成功でも activating のまま: 終了コード3" test "$rc" = 3
stopped_fail; echo 5 > "$T/env/ctl/fail_stop"; run deploy
expect "stop の失敗（状態は inactive）: stop の成功を必須にして終了コード3" test "$rc" = 3
stopped_fail; echo fail > "$T/env/ctl/rs_show"; run deploy
expect "停止後の状態取得の失敗: 終了コード3" test "$rc" = 3
stopped_fail; echo empty > "$T/env/ctl/rs_show"; run deploy
expect "停止後の状態が空出力: 終了コード3" test "$rc" = 3
stopped_fail; run deploy
expect "stop が成功して inactive なら戻せる（終了コード1）" test "$rc" = 1 -a ! -f "$T/env/state/active"
# unit が存在しない（stop が「not loaded」で失敗する）ときだけは例外: 初回の配布で unit の導入に失敗した場合
setup fresh; rm "$T/stage/geoframe-rs.service"; run deploy
expect "unit 未導入の初回: stop が not loaded でも戻せる（終了コード1）" test "$rc" = 1 -a ! -e "$MOCK_BIN" -a ! -e "$MOCK_UNIT"

echo "[9] 手元のラッパーの終了コード（ssh・scp はモック）"
export GEOFRAME_RS_LOCAL_BIN="$T/stage/geoframe" GEOFRAME_RS_BIN="$MOCK_BIN" GEOFRAME_RS_CHECK_BASE=http://127.0.0.1:1 GEOFRAME_RS_WAIT_SECS=2
wrap() { # wrap <引数...> → 出力は $T/out、終了コードは $rc。stage/geoframe は実行可能にして渡す
    chmod +x "$T/stage/geoframe"; : > "$T_LOG"
    bash "$HERE/rust-deploy.sh" --skip-build --target mock@test "$@" > "$T/out" 2>&1
    rc=$?
}
setup; rm -f "$GEOFRAME_RS_TEST_MARKER"; wrap
expect "ラッパー: 事前検査の拒否は終了コード4のまま" test "$rc" = 4
expect "ラッパー: 拒否されたら何も転送しない" bash -c '! grep -q scp "$1"' _ "$T_LOG"
expect "ラッパー: 何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev"
setup; touch "$T/env/state/py_active"; wrap
expect "ラッパー: Python 版が稼働中なら4" test "$rc" = 4
setup; echo fail > "$T/env/ctl/py_show"; wrap
expect "ラッパー: Python 版の状態を確認できないなら4" test "$rc" = 4
setup; wrap
expect "ラッパー: 成功は0" test "$rc" = 0 -a "$(content "$MOCK_BIN")" = NEW-BIN
setup; echo 1 > "$T/env/ctl/fail_restart"; wrap
expect "ラッパー: 戻せた失敗は1" test "$rc" = 1 -a "$(content "$MOCK_BIN")" = OLD-BIN
setup; echo 1 > "$T/env/ctl/fail_restart"; touch "$T/env/ctl/poison_prev"; wrap
expect "ラッパー: 戻せなかった失敗は3" test "$rc" = 3
setup; rm -f "$GEOFRAME_RS_TEST_MARKER"; wrap --rollback
expect "ラッパー: rollback の拒否も4" test "$rc" = 4
setup; wrap; wrap --rollback
expect "ラッパー: rollback の成功は0" test "$rc" = 0 -a "$(content "$MOCK_BIN")" = OLD-BIN
unset GEOFRAME_RS_LOCAL_BIN GEOFRAME_RS_BIN GEOFRAME_RS_CHECK_BASE GEOFRAME_RS_WAIT_SECS

echo "[10] 埋め込む旅程の検査（git 管理外のファイル・リンクがあればビルドの前に止める。ログに名前を出さない）"
# 一時リポジトリへスクリプトと unit だけ写して走らせる（REPO_ROOT はスクリプトの場所から決まる）。
# --dry-run --skip-build なので、ビルド・ssh はしない。検査の結果は出力の件数と終了コードで見る
R="$T/plans-repo"
mkdir -p "$R/deploy" "$R/web/travel_blog/plans" "$T/plans-bin"
cp "$HERE/rust-deploy.sh" "$R/deploy/"; cp "$HERE/geoframe-rs.service" "$R/deploy/"
printf '{}' > "$R/web/travel_blog/plans/tracked.json"
printf 'ig-name-*.json\n' > "$R/.gitignore"
git -C "$R" init -q
git -C "$R" add -A
git -C "$R" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m init
printf 'BIN' > "$T/plans-bin/geoframe"; chmod +x "$T/plans-bin/geoframe"
plans() { # plans → 出力は $T/out、終了コードは $rc
    GEOFRAME_RS_LOCAL_BIN="$T/plans-bin/geoframe" bash "$R/deploy/rust-deploy.sh" --dry-run --skip-build --target mock@test \
        > "$T/out" 2>&1
    rc=$?
}
plans_reached_build() { grep -q "ビルドを省略" "$T/out"; }
plans
expect "旅程の検査: 管理下だけなら通る（件数 0・ビルドの段へ進む）" bash -c 'grep -q "ファイル 0 件、リンク 0 件" "$1"' _ "$T/out"
expect "旅程の検査: 管理下だけなら止まらない" plans_reached_build
printf '{"secret":1}' > "$R/web/travel_blog/plans/untracked-name.json"
plans
expect "旅程の検査: 未追跡のファイルは止める（終了コード1・件数 1）" bash -c 'test "$1" = 1 && grep -q "ファイル 1 件、リンク 0 件" "$2"' _ "$rc" "$T/out"
expect "旅程の検査: 止めたらビルドの段へ進まない" bash -c '! grep -q "ビルドを省略" "$1"' _ "$T/out"
expect "旅程の検査: 名前も内容もログに出さない" bash -c '! grep -q -e untracked-name -e secret "$1"' _ "$T/out"
rm "$R/web/travel_blog/plans/untracked-name.json"
printf '{}' > "$R/web/travel_blog/plans/ig-name-1.json"
plans
expect "旅程の検査: .gitignore で無視されたファイルも止める（終了コード1）" bash -c 'test "$1" = 1 && grep -q "ファイル 1 件" "$2"' _ "$rc" "$T/out"
expect "旅程の検査: 無視されたファイルの名前もログに出さない" bash -c '! grep -q ig-name "$1"' _ "$T/out"
rm "$R/web/travel_blog/plans/ig-name-1.json"
printf '{}' > "$R/web/travel_blog/plans/.hidden.json"
plans
expect "旅程の検査: ドット始まりの未追跡も止める（終了コード1）" test "$rc" = 1
rm "$R/web/travel_blog/plans/.hidden.json"
mkdir "$R/web/travel_blog/plans/sub"; printf '{}' > "$R/web/travel_blog/plans/sub/nested.json"
plans
expect "旅程の検査: 下の階層の未追跡も止める（終了コード1）" test "$rc" = 1
rm -rf "$R/web/travel_blog/plans/sub"
ln -s "$T/plans-bin/geoframe" "$R/web/travel_blog/plans/link-name.json"
plans
expect "旅程の検査: リンクは止める（終了コード1・リンク 1 件）" bash -c 'test "$1" = 1 && grep -q "リンク 1 件" "$2"' _ "$rc" "$T/out"
expect "旅程の検査: リンクの名前もログに出さない" bash -c '! grep -q link-name "$1"' _ "$T/out"
rm "$R/web/travel_blog/plans/link-name.json"
plans
expect "旅程の検査: 取り除けば通る" plans_reached_build
# 管理下の旅程の変更は止めない（既存の「未コミットの変更あり」の表示だけ）
printf '{"changed":1}' > "$R/web/travel_blog/plans/tracked.json"
plans
expect "旅程の検査: 管理下の変更は止めず、未コミットの表示が出る" bash -c 'grep -q "未コミットの変更あり" "$1"' _ "$T/out"
git -C "$R" checkout -q -- web/travel_blog/plans/tracked.json
# 旅程のディレクトリ自体がリンク
mv "$R/web/travel_blog/plans" "$T/plans-moved"; ln -s "$T/plans-moved" "$R/web/travel_blog/plans"
plans
expect "旅程の検査: ディレクトリ自体のリンクも止める（終了コード1）" bash -c 'test "$1" = 1 && grep -q "リンク 1 件" "$2"' _ "$rc" "$T/out"
rm "$R/web/travel_blog/plans"; mv "$T/plans-moved" "$R/web/travel_blog/plans"
# 親（web/travel_blog・web）がリンク: リンク先の未追跡の旅程が、plans 自体の検査をすり抜けて埋め込まれる
printf '{"secret":1}' > "$R/web/travel_blog/plans/parent-link-name.json"
git -C "$R" add -A; git -C "$R" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m tracked-extra
mv "$R/web/travel_blog" "$T/tb-moved"; ln -s "$T/tb-moved" "$R/web/travel_blog"
printf '{"secret":2}' > "$T/tb-moved/plans/outside-name.json"
plans
expect "旅程の検査: 親（web/travel_blog）のリンクも止める（終了コード1・リンク 1 件）" bash -c 'test "$1" = 1 && grep -q "リンク 1 件" "$2"' _ "$rc" "$T/out"
expect "旅程の検査: 親のリンクで止めたらビルドの段へ進まない" bash -c '! grep -q "ビルドを省略" "$1"' _ "$T/out"
expect "旅程の検査: 親のリンクでも名前も内容もログに出さない" bash -c '! grep -q -e outside-name -e parent-link-name -e secret "$1"' _ "$T/out"
rm "$R/web/travel_blog"; mv "$T/tb-moved" "$R/web/travel_blog"; rm "$R/web/travel_blog/plans/outside-name.json"
mv "$R/web" "$T/web-moved"; ln -s "$T/web-moved" "$R/web"
plans
expect "旅程の検査: さらに上（web）のリンクも止める（終了コード1・リンク 1 件）" bash -c 'test "$1" = 1 && grep -q "リンク 1 件" "$2"' _ "$rc" "$T/out"
rm "$R/web"; mv "$T/web-moved" "$R/web"
git -C "$R" reset -q --hard HEAD~1
plans
expect "旅程の検査: リンクを戻せば通る" plans_reached_build
# 読めないディレクトリ（find が標準エラーに名前を出す）。root では権限が効かないので飛ばす
if [ "$(id -u)" -ne 0 ]; then
    mkdir "$R/web/travel_blog/plans/locked-dir-name"; chmod 000 "$R/web/travel_blog/plans/locked-dir-name"
    plans
    chmod 755 "$R/web/travel_blog/plans/locked-dir-name"
    expect "旅程の検査: 読めないディレクトリは止める（終了コード1・調べられない旨の定型文）" bash -c 'test "$1" = 1 && grep -q "調べられない" "$2"' _ "$rc" "$T/out"
    expect "旅程の検査: 読めないディレクトリの名前をログに出さない" bash -c '! grep -q -e locked-dir-name -e Permission "$1"' _ "$T/out"
    expect "旅程の検査: 読めないディレクトリで止めたらビルドの段へ進まない" bash -c '! grep -q "ビルドを省略" "$1"' _ "$T/out"
    rmdir "$R/web/travel_blog/plans/locked-dir-name"
fi
# UTF-8 でない名前（未追跡）: 止める。ログに名前も化けた名前も出さない
badname=$'bad-utf8-name-\xff.json'
printf '{}' > "$R/web/travel_blog/plans/$badname"
plans
expect "旅程の検査: UTF-8 でない名前の未追跡も止める（終了コード1・件数 1）" bash -c 'test "$1" = 1 && grep -q "ファイル 1 件、リンク 0 件" "$2"' _ "$rc" "$T/out"
expect "旅程の検査: UTF-8 でない名前もログに出さない" bash -c '! grep -q -e bad-utf8 -e xff "$1"' _ "$T/out"
rm "$R/web/travel_blog/plans/$badname"
plans
expect "旅程の検査: 取り除けば通る（読めない・UTF-8 でない名前のあと）" plans_reached_build
# rollback は旅程を使わないので検査しない
printf '{}' > "$R/web/travel_blog/plans/stray.json"
GEOFRAME_RS_LOCAL_BIN="$T/plans-bin/geoframe" bash "$R/deploy/rust-deploy.sh" --rollback --dry-run --target mock@test > "$T/out" 2>&1
expect "旅程の検査: --rollback では検査しない" bash -c '! grep -q "旅程の検査" "$1"' _ "$T/out"
rm "$R/web/travel_blog/plans/stray.json"

echo "[11] unit の書き込み許可（旅行ブログの保存・書き出しが書けること。T3.6）"
unit_ro="$(grep -E '^ReadOnlyPaths=' "$HERE/geoframe-rs.service")"
expect "unit: ReadOnlyPaths に photos が残る" bash -c 'grep -q "/var/lib/geoframe/photos" <<<"$1"' _ "$unit_ro"
expect "unit: ReadOnlyPaths から travel_blog を外した（下書きを書ける）" bash -c '! grep -q travel_blog <<<"$1"' _ "$unit_ro"
expect "unit: ReadOnlyPaths から blog_exports を外した（書き出しを書ける）" bash -c '! grep -q blog_exports <<<"$1"' _ "$unit_ro"
expect "unit: ReadOnlyPaths は 1 行だけ" test "$(grep -cE '^ReadOnlyPaths=' "$HERE/geoframe-rs.service")" = 1
expect "unit: 書き込みを広げた分、ProtectSystem=strict は残る" grep -q '^ProtectSystem=strict' "$HERE/geoframe-rs.service"

echo "[11b] unit の本番向けの中身（T5.1）"
U="$HERE/geoframe-rs.service"
expect "unit: 本体の置き場所は /usr/local/lib/systemd/system と書いてある" grep -q '/usr/local/lib/systemd/system/geoframe-rs.service' "$U"
expect "unit: 『検証環境専用』『本番機にはこの unit を入れない』と書いていない" bash -c '! grep -q -e 検証環境専用 -e "本番機にはこの unit を入れない" -e "印が無い" "$1"' _ "$U"
expect "unit: After= に geoframe-server.service を足し、Conflicts= も残す" bash -c 'grep -qE "^After=.*geoframe-server.service" "$1" && grep -q "^Conflicts=geoframe-server.service" "$1"' _ "$U"
expect "unit: Environment= を持たない（DATA_DIR・鍵・TZ は設定のファイルへ）" bash -c '! grep -qE "^Environment=" "$1"' _ "$U"
expect "unit: EnvironmentFile= は共通（- 付き）のあとに geoframe-rs（- 無し）" bash -c 'test "$(grep -E "^EnvironmentFile=" "$1")" = "EnvironmentFile=-/etc/default/geoframe
EnvironmentFile=/etc/default/geoframe-rs"' _ "$U"
expect "unit: StartLimitIntervalSec・StartLimitBurst は [Unit] 節" bash -c 'awk "/^\[Service\]/{exit} /^StartLimitIntervalSec=/{a=1} /^StartLimitBurst=/{b=1} END{exit !(a&&b)}" "$1"' _ "$U"
expect "unit: ReadOnlyPaths に鍵と models/ を足した（1 行のまま）" bash -c 'grep -E "^ReadOnlyPaths=" "$1" | grep -q -e "-/var/lib/geoframe/drive_key.json" && grep -E "^ReadOnlyPaths=" "$1" | grep -q -e "-/var/lib/geoframe/models"' _ "$U"
for line in 'CapabilityBoundingSet=' 'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6' ProtectKernelTunables=true ProtectKernelModules=true \
    ProtectKernelLogs=true ProtectControlGroups=true RestrictNamespaces=true LockPersonality=true RestrictSUIDSGID=true PrivateDevices=true \
    SystemCallArchitectures=native UMask=0027 MemoryDenyWriteExecute=true NoNewPrivileges=true; do
    expect "unit: $line" grep -qxF "$line" "$U"
done
expect "unit: 優先順位の説明が正しい（EnvironmentFile= が書いた順に関係なく Environment= に勝つ）" bash -c 'grep -q "書いた順に関係なく Environment= に勝つ" "$1" && ! grep -q "後勝ち" "$1"' _ "$U"

echo "[11c] 古い置き場所の本体と unit の置き場所（検証環境の経路）"
setup; printf 'ETC-BODY\n' > "$GEOFRAME_RS_TEST_ETC_UNIT_DIR/geoframe-rs.service"; run deploy
expect "古い置き場所に本体があれば拒否（終了コード4）" test "$rc" = 4
expect "古い置き場所の拒否で何も変えていない" test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a "$(content "$MOCK_UNIT")" = OLD-UNIT
expect "古い置き場所の拒否が退避を案内する" grep -q '退避' "$T/out"
setup; printf 'ETC-BODY\n' > "$GEOFRAME_RS_TEST_ETC_UNIT_DIR/geoframe-rs.service"; run rollback
expect "rollback も古い置き場所に本体があれば拒否（終了コード4）" test "$rc" = 4
setup; ln -s /dev/null "$GEOFRAME_RS_TEST_ETC_UNIT_DIR/geoframe-rs.service"; run deploy
expect "古い置き場所が mask のリンク（/dev/null）なら本体ではないので通る" test "$rc" = 0
setup; rm -rf "$T/env/units"; run deploy
expect "unit の置き場所が無ければ作って入れる" test "$rc" = 0 -a "$(content "$MOCK_UNIT")" = NEW-UNIT

echo "[11d] /api/v1/photos の数え方（jq・Python に頼らない）"
eval "$(sed -n '/^count_photos() {/,/^}/p' <<<"$REMOTE")"
count_of() { printf '%s' "$1" > "$T/photos.json"; count_photos "$T/photos.json"; }
expect "件数: 空の配列は 0" test "$(count_of '[]')" = 0
expect "件数: 要素 3（入れ子のオブジェクト・配列を含む）" test "$(count_of '[{"id":1,"a":{"b":[1,{"c":2}]}},{"id":2},{"id":3,"t":[]}]')" = 3
expect "件数: 文字列の中の括弧・引用符・バックスラッシュに惑わされない" test "$(count_of '[{"n":"a}{\"b\"[{"},{"n":"\\"},{"n":"x]"}]')" = 3
expect "件数: 改行・空白があっても数える" test "$(count_of $'[\n {"id":1},\n {"id":2}\n]')" = 2

echo "[12] 本番の指定（--production。ssh なし・その機体で直接。一時の git リポジトリで実行）"
PR="$T/prod-repo"
mkdir -p "$PR/deploy"
cp "$HERE/geoframe-rs.service" "$PR/deploy/"
# 本番の経路は GEOFRAME_RS_* の環境変数を読まない（あれば断る）。配置先・確認の URL・unit の置き場所・印・設定のファイル・
# root の UID は、スクリプトの中の固定値（PROD_*）で、この写しだけを一時ディレクトリへ向けて書き換える
# （geoframe-admin-test.sh と同じ方式）。本物のスクリプトは書き換えない
write_prod_copy() { # write_prod_copy <出力> <root とみなす UID>
    sed -e "s#^PROD_BIN_PATH=.*#PROD_BIN_PATH=\"$MOCK_BIN\"#" \
        -e "s#^PROD_CHECK_BASE=.*#PROD_CHECK_BASE=\"http://127.0.0.1:1\"#" \
        -e "s#^PROD_UNIT_DIR=.*#PROD_UNIT_DIR=\"$T/env/units\"#" \
        -e "s#^PROD_ETC_UNIT_DIR=.*#PROD_ETC_UNIT_DIR=\"$T/env/etc-units\"#" \
        -e "s#^PROD_MARKER=.*#PROD_MARKER=\"$T/env/etc/staging\"#" \
        -e "s#^PROD_ENV_FILE=.*#PROD_ENV_FILE=\"$T/env/etc/geoframe-rs\"#" \
        -e "s#^PROD_ROOT_UID=.*#PROD_ROOT_UID=\"$2\"#" \
        "$HERE/rust-deploy.sh" > "$1"
}
write_prod_copy "$PR/deploy/rust-deploy.sh" "$(id -u)"
write_prod_copy "$PR/deploy/rust-deploy-notroot.sh" "$(( $(id -u) + 1 ))"
expect "本番の試験の準備: 書き換えた写しが元と違うのは固定値の 7 行だけ" bash -c 'test "$(diff "$1" "$2" | grep -c "^>")" = 7' _ "$HERE/rust-deploy.sh" "$PR/deploy/rust-deploy.sh"
expect "本番の固定値は設計どおり（配置先・確認の URL・unit の置き場所・古い置き場所・印・設定のファイル・root は 0）" bash -c '
    for l in "PROD_BIN_PATH=\"/usr/local/bin/geoframe\"" "PROD_CHECK_BASE=\"http://127.0.0.1:8080\"" \
             "PROD_UNIT_DIR=\"/usr/local/lib/systemd/system\"" "PROD_ETC_UNIT_DIR=\"/etc/systemd/system\"" \
             "PROD_MARKER=\"/etc/geoframe/staging\"" "PROD_ENV_FILE=\"/etc/default/geoframe-rs\"" "PROD_ROOT_UID=\"0\""; do
        grep -qxF "$l" "$1" || exit 1
    done' _ "$HERE/rust-deploy.sh"
# 本番の経路を動かすシェル。GEOFRAME_RS_*（この試験の他の節が、検証環境の経路のために export している）を外して動かす
prodsh() { local u=() v; for v in $(compgen -e | grep '^GEOFRAME_RS_'); do u+=(-u "$v"); done; env ${u[@]+"${u[@]}"} bash "$@"; }
printf 'tracked\n' > "$PR/tracked.txt"
git -C "$PR" init -q
git -C "$PR" add -A
git -C "$PR" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m init
HEAD_SHA="$(git -C "$PR" rev-parse HEAD)"
PROD_HOST_NAME=prod-host-for-test
pbin() { chmod +x "$T/stage/geoframe"; sha256sum "$T/stage/geoframe" | cut -d' ' -f1; }
SHA_OK=""
prod() { # prod <追加の引数...> → 出力は $T/out、終了コードは $rc。既定は全部正しい指定
    SHA_OK="$(pbin)"; : > "$T_LOG"
    [ -f "$T/env/ctl/photos_json" ] || printf '[{"id":1}]' > "$T/env/ctl/photos_json"
    prodsh "${PROD_SCRIPT:-$PR/deploy/rust-deploy.sh}" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$SHA_OK" \
        --expect-commit="$HEAD_SHA" --expect-min-photos=1 --wait-secs=2 "$@" > "$T/out" 2>&1
    rc=$?
}
nothing_changed() { test "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev" -a ! -e "$MOCK_BIN.new" -a "$(content "$MOCK_UNIT")" = OLD-UNIT -a ! -e "$MOCK_UNIT.prev"; }
no_ssh() { ! grep -q -e ssh -e scp "$T_LOG"; }
no_restart() { ! grep -q '^restart ' "$T/env/calls.log"; }
no_prod_marker() { rm -f "$GEOFRAME_RS_TEST_MARKER"; }

tmp_dirs() { ls -d /tmp/geoframe-rs-deploy.* 2>/dev/null | wc -l; }
TMP_BEFORE="$(tmp_dirs)"
setup; no_prod_marker; prod
expect "本番: 成功は終了コード0" test "$rc" = 0
expect "本番: 新しいバイナリが入り、.prev に旧版が残る" test "$(content "$MOCK_BIN")" = NEW-BIN -a "$(content "$MOCK_BIN.prev")" = OLD-BIN
expect "本番: unit は checkout の deploy/geoframe-rs.service で、置き場所に入る（.prev は旧版）" bash -c 'cmp -s "$1" "$2" && test "$(cat "$3")" = OLD-UNIT' _ "$MOCK_UNIT" "$HERE/geoframe-rs.service" "$MOCK_UNIT.prev"
expect "本番: 稼働・enable のまま" test -f "$T/env/state/active" -a -f "$T/env/state/enabled"
expect "本番: ssh も scp も使わない" no_ssh
expect "本番: root 所有の一時ディレクトリ（バイナリの写し）を残さない" test "$(tmp_dirs)" = "$TMP_BEFORE"
# 引数の誤り（何も変更しない・終了コード2）
setup; no_prod_marker; prod --target mock@test
expect "本番: --target と同時は終了コード2" test "$rc" = 2
nothing_changed_args() { nothing_changed && no_restart; }
setup; no_prod_marker; : > "$T_LOG"
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" > "$T/out" 2>&1; rc=$?
expect "本番: --expect-* が欠けていれば終了コード2" test "$rc" = 2
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256=zz --expect-commit="$HEAD_SHA" --expect-min-photos=1 > "$T/out" 2>&1; rc=$?
expect "本番: sha256 の形が違えば終了コード2" test "$rc" = 2
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$SHA_OK" --expect-commit="${HEAD_SHA:0:12}" --expect-min-photos=1 > "$T/out" 2>&1; rc=$?
expect "本番: commit が完全な SHA でなければ終了コード2" test "$rc" = 2
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$SHA_OK" --expect-commit="$HEAD_SHA" --expect-min-photos=0 > "$T/out" 2>&1; rc=$?
expect "本番: --expect-min-photos=0 は終了コード2" test "$rc" = 2
prodsh "$PR/deploy/rust-deploy.sh" --check-only --target mock@test > "$T/out" 2>&1; rc=$?
expect "本番でないとき --check-only は終了コード2" test "$rc" = 2
prodsh "$PR/deploy/rust-deploy.sh" --rollback --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" > "$T/out" 2>&1; rc=$?
expect "本番: --rollback に --bin は終了コード2" test "$rc" = 2

# 拒否（終了コード4・何も変更しない）
setup; no_prod_marker; MOCK_HOSTNAME=another-host prod
expect "本番: ホスト名が違えば拒否（終了コード4）" test "$rc" = 4
expect "本番: ホスト名の違いで何も変えていない" nothing_changed_args
setup; prod
expect "本番: 検証環境の印があれば拒否（終了コード4）" test "$rc" = 4
expect "本番: 印があるとき何も変えていない" nothing_changed_args
setup; no_prod_marker; touch "$T/env/state/py_active"; prod
expect "本番: Python 版が稼働中なら拒否（終了コード4）" test "$rc" = 4
expect "本番: Python 稼働中で何も変えていない" nothing_changed_args
setup; no_prod_marker; echo fail > "$T/env/ctl/py_show"; prod
expect "本番: Python 版の状態を取得できなければ拒否（終了コード4）" test "$rc" = 4
expect "本番: 状態不明で何も変えていない" nothing_changed_args
setup; no_prod_marker; echo maintenance > "$T/env/state/py_state"; prod
expect "本番: Python 版の状態が未知なら拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; touch "$T/env/state/masked"; prod
expect "本番: geoframe-rs が mask されていれば拒否（終了コード4）" test "$rc" = 4
expect "本番: mask で何も変えていない" nothing_changed_args
expect "本番: mask の拒否が unmask を案内する" grep -q 'unmask' "$T/out"
setup; no_prod_marker; : > "$T_LOG"
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$(printf '%064d' 0)" --expect-commit="$HEAD_SHA" --expect-min-photos=1 > "$T/out" 2>&1; rc=$?
expect "本番: sha256 が違えば拒否（終了コード4）" test "$rc" = 4
expect "本番: sha256 の違いで何も変えていない" nothing_changed_args
setup; no_prod_marker; : > "$T_LOG"
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$SHA_OK" --expect-commit="$(printf '%040d' 0)" --expect-min-photos=1 > "$T/out" 2>&1; rc=$?
expect "本番: commit が違えば拒否（終了コード4）" test "$rc" = 4
expect "本番: commit の違いで何も変えていない" nothing_changed_args
setup; no_prod_marker; printf 'edited\n' > "$PR/tracked.txt"; prod
expect "本番: 追跡下のファイルが汚れていれば拒否（終了コード4）" test "$rc" = 4
expect "本番: 汚れた作業ツリーで何も変えていない" nothing_changed_args
git -C "$PR" checkout -q -- tracked.txt
setup; no_prod_marker; printf 'untracked\n' > "$PR/untracked.txt"; printf 'edited\n' >> "$PR/deploy/geoframe-rs.service"; prod
expect "本番: unit の汚れも拒否する（追跡下の変更）" test "$rc" = 4
git -C "$PR" checkout -q -- deploy/geoframe-rs.service
setup; no_prod_marker; prod
expect "本番: 未追跡のファイルがあっても通る（app/.venv などは汚れではない）" test "$rc" = 0
rm -f "$PR/untracked.txt"
setup; no_prod_marker; rm -f "$GEOFRAME_RS_TEST_ENV_FILE"; prod
expect "本番: /etc/default/geoframe-rs が無ければ拒否（終了コード4）" test "$rc" = 4
expect "本番: 設定のファイルが無くて何も変えていない" nothing_changed_args
setup; no_prod_marker; chmod 0644 "$GEOFRAME_RS_TEST_ENV_FILE"; prod
expect "本番: 設定のファイルのモードが 0600 でなければ拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; rm -f "$GEOFRAME_RS_TEST_ENV_FILE"; ln -s /etc/hostname "$GEOFRAME_RS_TEST_ENV_FILE"; prod
expect "本番: 設定のファイルがシンボリックリンクなら拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; printf 'ETC-BODY\n' > "$GEOFRAME_RS_TEST_ETC_UNIT_DIR/geoframe-rs.service"; prod
expect "本番: 古い置き場所に本体があれば拒否（終了コード4）" test "$rc" = 4
expect "本番: 古い置き場所の拒否で何も変えていない" nothing_changed_args
setup; no_prod_marker; PROD_SCRIPT="$PR/deploy/rust-deploy-notroot.sh" prod
expect "本番: root（期待する UID）でなければ拒否（終了コード4）" test "$rc" = 4
expect "本番: root でなくて何も変えていない" nothing_changed_args
expect "本番: 設定のファイルの中身を読まない（ログに出ない）" bash -c '! grep -q "ENV-SECRET" "$1"' _ "$T/out"
setup; no_prod_marker; prod
expect "本番: 成功した配布のログにも設定のファイルの中身が出ない（ファイルには値がある）" bash -c 'grep -q ENV-SECRET "$2" && ! grep -q "ENV-SECRET" "$1"' _ "$T/out" "$GEOFRAME_RS_TEST_ENV_FILE"

# 安全に使う git
setup; no_prod_marker
mkdir -p "$T/gitmock"
cat > "$T/gitmock/git" <<M
#!/usr/bin/env bash
echo "\$*" >> "$T/git.log"
exec "$REAL_GIT" "\$@"
M
chmod +x "$T/gitmock/git"; : > "$T/git.log"
PATH="$T/gitmock:$PATH" prod
expect "本番: git に safe.directory（checkout だけ）と fsmonitor 無効を付ける" bash -c 'grep -q "safe.directory=$(cd "$1" && pwd -P)" "$2" && grep -q "core.fsmonitor=false" "$2"' _ "$PR" "$T/git.log"
expect "本番: git の検査を付けても成功する" test "$rc" = 0

# 件数の下限
setup; no_prod_marker; printf '[{"id":1},{"id":2},{"id":3}]' > "$T/env/ctl/photos_json"; prod --expect-min-photos=3
expect "本番: 件数が下限ちょうどなら成功" test "$rc" = 0
setup; no_prod_marker; printf '[{"id":1},{"id":2}]' > "$T/env/ctl/photos_json"; prod --expect-min-photos=3
expect "本番: 件数が下限より少なければ失敗して戻す（終了コード1）" test "$rc" = 1
expect "本番: 件数不足で、バイナリ・unit・稼働・enable が元どおり" back_to_old
expect "本番: 件数不足の理由が出る" grep -q '下限 3' "$T/out"
setup; no_prod_marker; printf 'BADARRAY\n' > "$T/stage/geoframe"; prod
expect "本番: 配列でない応答は失敗して戻す（終了コード1）" test "$rc" = 1
expect "本番: 配列でない応答で元どおり" back_to_old
# 失敗時の戻し
setup; no_prod_marker; echo 1 > "$T/env/ctl/fail_restart"; prod
expect "本番: restart の失敗は戻して終了コード1" test "$rc" = 1
expect "本番: restart の失敗で元どおり" back_to_old
setup; no_prod_marker; printf 'BADHEALTH\n' > "$T/stage/geoframe"; prod
expect "本番: /healthz が通らなければ戻して終了コード1" test "$rc" = 1
setup fresh; no_prod_marker; echo 1 > "$T/env/ctl/fail_restart"; prod
expect "本番: 初回の配布の失敗は止めて unit を外す（終了コード1）" test "$rc" = 1 -a ! -e "$MOCK_UNIT" -a ! -f "$T/env/state/enabled" -a ! -f "$T/env/state/active" -a ! -e "$MOCK_BIN"
expect "本番: 初回の失敗が Python 版へ戻す手順を案内する" grep -q 'cutover.md' "$T/out"
setup; no_prod_marker; echo 1 > "$T/env/ctl/fail_restart"; touch "$T/env/ctl/poison_prev"; prod
expect "本番: 戻せなかった失敗は終了コード3" test "$rc" = 3
expect "本番: 戻せなかった失敗の手順は『本番機で』で、検証環境とは言わず、Python 版へ戻すのは cutover.md §8 と案内する" bash -c 'grep -q "本番機で root として手で戻すこと" "$1" && ! grep -q "検証環境で root" "$1" && grep -q "cutover.md §8" "$1"' _ "$T/out"
# unit の置き場所
setup; no_prod_marker; rm -rf "$T/env/units"; prod
expect "本番: unit の置き場所が無くても作って入れる" test "$rc" = 0 -a -f "$MOCK_UNIT"
setup; no_prod_marker; ln -s /dev/null "$GEOFRAME_RS_TEST_ETC_UNIT_DIR/geoframe-rs.service"; prod
expect "本番: 古い置き場所が mask のリンクなら本体ではないので通る" test "$rc" = 0

# --check-only
setup; no_prod_marker; touch "$T/env/state/py_active"; prod --check-only
expect "本番: --check-only は Python 版が稼働中でも検査だけ通る（終了コード0）" test "$rc" = 0
expect "本番: --check-only は何も変えず、再起動もしない" nothing_changed_args
expect "本番: --check-only は一時の写しだけ作り、配置先に触れない" bash -c '! ls "$1"/geoframe.new "$1"/geoframe.prev 2>/dev/null' _ "$T/env/bin"
setup; no_prod_marker; MOCK_HOSTNAME=another-host prod --check-only
expect "本番: --check-only もホスト名が違えば拒否（終了コード4）" test "$rc" = 4
setup; prod --check-only
expect "本番: --check-only も印があれば拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; touch "$T/env/state/masked"; prod --check-only
expect "本番: --check-only も mask されていれば拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; chmod 0644 "$GEOFRAME_RS_TEST_ENV_FILE"; prod --check-only
expect "本番: --check-only も設定のファイルのモードが違えば拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; : > "$T_LOG"
prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$(printf '%064d' 0)" --expect-commit="$HEAD_SHA" --expect-min-photos=1 --check-only > "$T/out" 2>&1; rc=$?
expect "本番: --check-only も sha256 が違えば拒否（終了コード4）" test "$rc" = 4

# --rollback --production
prod_rollback() { : > "$T_LOG"; prodsh "$PR/deploy/rust-deploy.sh" --rollback --production="$PROD_HOST_NAME" "$@" > "$T/out" 2>&1; rc=$?; }
setup; no_prod_marker; prod; prod_rollback
expect "本番: --rollback --production は旧版へ戻す（終了コード0）" test "$rc" = 0 -a "$(content "$MOCK_BIN")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = OLD-UNIT
expect "本番: --rollback は ssh を使わない" no_ssh
setup; no_prod_marker; prod; MOCK_HOSTNAME=another-host prod_rollback
expect "本番: --rollback もホスト名が違えば拒否（終了コード4・新版のまま）" test "$rc" = 4 -a "$(content "$MOCK_BIN")" = NEW-BIN
setup; no_prod_marker; prod; touch "$T/env/state/py_active"; prod_rollback
expect "本番: --rollback も Python 版が稼働中なら拒否（終了コード4）" test "$rc" = 4 -a "$(content "$MOCK_BIN")" = NEW-BIN
setup; prod_rollback
expect "本番: --rollback も検証環境の印があれば拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; touch "$T/env/state/masked"; prod_rollback
expect "本番: --rollback も mask されていれば拒否（終了コード4）" test "$rc" = 4
setup; no_prod_marker; prod_rollback
expect "本番: .prev が無い --rollback は終了コード1" test "$rc" = 1

# 環境変数で検査を外せない（--production は GEOFRAME_RS_* を使わず、あれば断る。sudo -E など環境を引き継ぐ実行を想定）
envrefuse() { # envrefuse <名前=値...>: 既にある GEOFRAME_RS_* を全部外し、指定の環境変数だけを付けて、全部正しい指定で動かす（prodsh を通さず、環境をそのまま渡す）
    SHA_OK="$(pbin)"; : > "$T_LOG"
    local unset_args=() name
    for name in $(compgen -e | grep '^GEOFRAME_RS_' || true); do unset_args+=(-u "$name"); done
    env "${unset_args[@]}" "$@" bash "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$SHA_OK" \
        --expect-commit="$HEAD_SHA" --expect-min-photos=1 --wait-secs=2 > "$T/out" 2>&1
    rc=$?
}
for kv in "GEOFRAME_RS_TEST_MARKER=$T/env/etc/other-marker" "GEOFRAME_RS_TEST_UNIT_DIR=$T/env/units" "GEOFRAME_RS_TEST_ETC_UNIT_DIR=$T/env/etc-units" \
          "GEOFRAME_RS_TEST_ENV_FILE=$T/env/etc/geoframe-rs" "GEOFRAME_RS_TEST_ROOT_UID=$(id -u)" "GEOFRAME_RS_CHECK_BASE=http://127.0.0.1:1" \
          "GEOFRAME_RS_BIN=$MOCK_BIN" "GEOFRAME_RS_WAIT_SECS=2" "GEOFRAME_RS_LOCAL_BIN=$T/stage/geoframe" \
          "GEOFRAME_RS_TEST_MARKER=" "GEOFRAME_RS_UNKNOWN_NAME=1"; do
    setup; no_prod_marker; : > "$T/env/calls.log"; envrefuse "$kv"
    expect "本番: 環境に ${kv%%=*} があれば断る（終了コード4・何も変えず、systemctl にも触れない）" test "$rc" = 4 -a ! -s "$T/env/calls.log" -a "$(content "$MOCK_BIN")" = OLD-BIN
done
expect "本番: 環境変数で断るとき、理由に GEOFRAME_RS_* が出る" grep -q 'GEOFRAME_RS_' "$T/out"
# 検査の向き先を環境で変えて印を外そうとしても、印の検査は固定の場所で行われる（印があるまま、印の場所を別のところに向けても断る）
setup; envrefuse "GEOFRAME_RS_TEST_MARKER=$T/env/etc/no-such-marker"
expect "本番: 印の検査先を環境で別の場所へ向けても、印があれば断る（終了コード4）" test "$rc" = 4 -a "$(content "$MOCK_BIN")" = OLD-BIN
# 書き換えていない本物のスクリプトは、固定の本物の場所を使う: 試験を動かすユーザー（root でない）では、root の検査で断る
if [ "$(id -u)" != 0 ]; then
    setup; no_prod_marker; : > "$T/env/calls.log"
    prodsh "$HERE/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$(printf '%064d' 0)" --expect-commit="$HEAD_SHA" --expect-min-photos=1 > "$T/out" 2>&1; rc=$?
    expect "本番: 書き換えていないスクリプトは root（UID 0）を要求して断る（終了コード4・何も変えない）" test "$rc" = 4 -a ! -s "$T/env/calls.log" -a "$(content "$MOCK_BIN")" = OLD-BIN
fi

# 数の引数: 桁が多い値や数でない値は引数の検査で断る（終了コード2・何も変えない）。Bash の整数比較は大きすぎる値で
# 異常終了し、if の中だと『下限を満たした』ように進むので、入口で上限を設ける
for bad in 999999999999999999999 1000000000 0 01 -1 1.5 abc ""; do
    setup; no_prod_marker; : > "$T/env/calls.log"
    prodsh "$PR/deploy/rust-deploy.sh" --production="$PROD_HOST_NAME" --bin="$T/stage/geoframe" --expect-sha256="$(pbin)" --expect-commit="$HEAD_SHA" --expect-min-photos="$bad" > "$T/out" 2>&1; rc=$?
    expect "本番: --expect-min-photos='${bad}' は終了コード2で断る（何も変えない）" test "$rc" = 2 -a ! -s "$T/env/calls.log" -a "$(content "$MOCK_BIN")" = OLD-BIN
done
setup; no_prod_marker; printf '[{"id":1}]' > "$T/env/ctl/photos_json"; prod --expect-min-photos=999999999
expect "本番: 9 桁の上限（999999999）は受け入れ、件数が足りなければ失敗して戻す（終了コード1）" test "$rc" = 1
expect "本番: 上限の値で件数が足りない失敗は元どおりに戻る" back_to_old
setup; no_prod_marker; printf '[]' > "$T/env/ctl/photos_json"; prod --expect-min-photos=999999999999999999999
expect "本番: 空の配信 × 21 桁の下限は、引数の検査で断る（終了コード2・確認 OK に進まない）" test "$rc" = 2 && ! grep -q '確認 OK' "$T/out"
for bad in 99999 -1 abc 1.5 ""; do
    setup; no_prod_marker; : > "$T/env/calls.log"; prod --wait-secs="$bad"
    expect "本番: --wait-secs='${bad}' は終了コード2で断る（何も変えない）" test "$rc" = 2 -a ! -s "$T/env/calls.log" -a "$(content "$MOCK_BIN")" = OLD-BIN
done
setup; no_prod_marker; prod --wait-secs=3601
expect "本番: --wait-secs が 3600 を超えれば終了コード2" test "$rc" = 2
# 本体（check）自体も、巨大な下限・数でない値で『確認 OK』に進まない（引数の検査をすり抜けた場合の二重の守り）
eval "$(sed -n '/^check() {/,/^}/p;/^count_photos() {/,/^}/p;/^say() {/p' <<<"$REMOTE")"
setup; touch "$T/env/state/active"; printf '[]' > "$T/env/ctl/photos_json"; base=http://127.0.0.1:1 wait_secs=2
check 999999999999999999999 > "$T/out" 2>&1; rc=$?
expect "check: 空の配信 × 21 桁の下限は失敗（確認 OK にならない）" test "$rc" = 1 && ! grep -q '確認 OK' "$T/out"
check abc > "$T/out" 2>&1; rc=$?
expect "check: 下限が数でなければ失敗" test "$rc" = 1 && ! grep -q '確認 OK' "$T/out"
printf '[{"id":1},{"id":2}]' > "$T/env/ctl/photos_json"
check 2 > "$T/out" 2>&1; rc=$?
expect "check: 件数が下限ちょうどなら成功" test "$rc" = 0 && grep -q '確認 OK' "$T/out"
check 3 > "$T/out" 2>&1; rc=$?
expect "check: 件数が下限より少なければ失敗" test "$rc" = 1
check "" > "$T/out" 2>&1; rc=$?
expect "check: 下限なしは配列であれば成功" test "$rc" = 0
wait_secs=99999999999; check 1 > "$T/out" 2>&1; rc=$?
expect "check: 待つ秒数が長すぎる値は失敗" test "$rc" = 1
wait_secs=2
# 本体の入口: 不正な数は何も変更せず終了コード2
setup; bash -c "$REMOTE" bash deploy "$MOCK_BIN" http://127.0.0.1:1 99999999999 "$T/stage" > "$T/out" 2>&1; rc=$?
expect "本体: 待つ秒数が不正なら終了コード2で何も変えない" test "$rc" = 2 -a "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev"
setup; RD_MIN_PHOTOS=999999999999999999999 bash -c "$REMOTE" bash deploy "$MOCK_BIN" http://127.0.0.1:1 2 "$T/stage" > "$T/out" 2>&1; rc=$?
expect "本体: 件数の下限が不正なら終了コード2で何も変えない" test "$rc" = 2 -a "$(content "$MOCK_BIN")" = OLD-BIN -a ! -e "$MOCK_BIN.prev"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
