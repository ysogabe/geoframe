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
cmd="" unit=""
for a in "$@"; do case "$a" in -*) ;; *) if [ -z "$cmd" ]; then cmd="$a"; else unit="$a"; fi ;; esac; done
echo "$cmd $unit" >> "$MOCK_DIR/calls.log"
case "$cmd" in
is-active)
    f="$S/active"; [ "$unit" = geoframe-server.service ] && f="$S/py_active"
    if [ -f "$f" ]; then echo active; exit 0; else echo inactive; exit 3; fi ;;
is-enabled)
    if [ -f "$S/enabled" ]; then echo enabled; exit 0; fi
    if [ -f "$MOCK_UNIT" ]; then echo disabled; exit 1; fi
    exit 1 ;;
enable)  bump fail_enable && exit 1; touch "$S/enabled"; exit 0 ;;
disable) bump fail_disable && exit 1; rm -f "$S/enabled"; exit 0 ;;
daemon-reload) bump fail_reload && exit 1; exit 0 ;;
reset-failed) exit 0 ;;
stop) rm -f "$S/active"; exit 0 ;;
restart)
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
chmod +x "$T/mock/"*

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

echo "[7] 手動の rollback"
setup; run deploy; run rollback
expect "rollback の成功: 終了コード0・旧版に戻る" test "$rc" = 0 -a "$(content "$MOCK_BIN")" = OLD-BIN -a "$(content "$MOCK_UNIT")" = OLD-UNIT
setup; run rollback
expect ".prev が無い rollback: 終了コード1" test "$rc" = 1

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
