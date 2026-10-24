#!/usr/bin/env bash
#
# rust-deploy-test.sh - rust-deploy.sh の検証環境側の本体を、モックの systemctl/curl で試験する
#
# ssh もしない・実際のサービスにも触らない。一時ディレクトリに「検証環境」を作り、
# `rust-deploy.sh --print-remote-script` で取り出した本体を一般ユーザーのまま走らせる。
# 見ること: 誤配布の拒否、成功、各工程（再起動・daemon-reload・enable・unit の導入・確認）の失敗からの
# 復元、復元の mv の失敗（手作業の手順が出て終了コード 3）、初回の配布の失敗、中断（SIGTERM）。
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
    # ctl/py_show, ctl/rs_show: fail（systemd へ繋がらない: 標準エラーだけ・終了コード1）| empty（空出力・終了コード0）
    if [ "$unit" = geoframe-server.service ]; then tag=py; else tag=rs; fi
    if [ -f "$C/${tag}_show" ]; then
        case "$(cat "$C/${tag}_show")" in
            fail)  echo "Failed to connect to bus: No such file or directory" >&2; exit 1 ;;
            empty) exit 0 ;;
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
    if [ -f "$C/term_on_restart" ] && grep -qw "$n" "$C/term_on_restart"; then kill -TERM "$PPID"; fi
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
S="$MOCK_DIR/state"; url="" w=0
for a in "$@"; do case "$a" in http://*) url="$a" ;; -w) w=1 ;; esac; done
if [ ! -f "$S/active" ]; then [ "$w" = 1 ] && printf 000; exit 7; fi
case "$url" in
*/healthz) if grep -q BADHEALTH "$MOCK_BIN" 2>/dev/null; then exit 22; fi; exit 0 ;;
*/api/v1/photos) [ "$w" = 1 ] && { printf 200; exit 0; }; printf '[]'; exit 0 ;;
esac
exit 0
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
export PATH="$T/mock:$PATH"

# 検証環境を作り直す。引数: installed（バイナリ・unit・enable・稼働が済み）| fresh（何も無い）
setup() {
    rm -rf "$T/env" "$T/stage"
    mkdir -p "$T/env/bin" "$T/env/units" "$T/env/etc" "$T/env/state" "$T/env/ctl" "$T/stage"
    touch "$T/env/etc/staging"
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
setup; echo maintenance > "$T/env/state/py_state"; run deploy
expect "Python 版の状態が未知の値なら拒否（終了コード4）" test "$rc" = 4
setup; echo activating > "$T/env/state/py_state"; run deploy
expect "Python 版が activating なら拒否（終了コード4）" test "$rc" = 4
setup; echo fail > "$T/env/ctl/py_show"; run rollback
expect "rollback も状態取得の失敗なら拒否（終了コード4）" test "$rc" = 4
setup; echo failed > "$T/env/state/py_state"; run preflight
expect "Python 版が failed なら通す" test "$rc" = 0
setup; echo not-found > "$T/env/state/py_load"; run preflight
expect "Python 版の unit が存在しないと確認できれば通す" test "$rc" = 0

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
setup; echo 1 > "$T/env/ctl/fail_restart"; touch "$T/env/ctl/poison_prev"; echo 2 > "$T/env/ctl/term_on_restart"; run deploy
expect "復元の失敗と SIGTERM が重なっても: 終了コード3・手順が出る" bash -c 'test "$1" = 3 && grep -q "手で戻すこと" "$2"' _ "$rc" "$T/out"

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

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
