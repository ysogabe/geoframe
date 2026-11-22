#!/usr/bin/env bash
#
# geoframe-export-admin-test.sh - geoframe-export-admin.sh を、モックの systemd-run・systemctl・id で試験する
#
# 実機の systemd にも /usr/local にも /var/lib にも触れない。geoframe-export-admin.sh を一時ディレクトリへ写し、
# バイナリ・rclone・状態ディレクトリ（本番と検証用）の固定のパスだけを一時ディレクトリの中へ差し替える。systemd-run は PATH のモックで、
# 受け取った引数を 1 行に 1 つずつ記録する（systemctl のモックは、呼ばれたことを記録するだけ。包みは呼ばない）。検証用の入口（--verify）が
# 本番の実行枠 export.lock にも、本番の rclone の設定・rclone/ にも触れず（引数にも ReadWritePaths= にも出さず）、検証用の状態ディレクトリの
# rclone の設定の写しだけを使うことを確かめる。
# あわせて、geoframe-export.service と包みの設定が食い違っていないことを検査する。
#
# 実行: bash deploy/geoframe-export-admin-test.sh
#
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$(mktemp -d /tmp/geoframe-export-admin-test.XXXXXX)"
trap 'rm -rf "$T"' EXIT

pass=0 failn=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
ng()   { failn=$((failn + 1)); echo "  FAIL $*"; }
expect() { local d="$1"; shift; if "$@"; then ok "$d"; else ng "$d"; fi; }

SCRIPT="$HERE/geoframe-export-admin.sh"
UNIT="$HERE/geoframe-export.service"

mkdir -p "$T/mock" "$T/bin" "$T/state/rclone" "$T/vstate/rclone"
cat > "$T/mock/systemd-run" <<'M'
#!/usr/bin/env bash
: > "$MOCK_ARGS"
for a in "$@"; do printf '%s\n' "$a" >> "$MOCK_ARGS"; done
# 呼び出し元（包み）が本番の実行枠（export.lock）を握ったまま exec しているか。握っていれば、ここで取れない（検証用の入口も握らない）
if [ -f "$MOCK_LOCK" ] && ! flock -n "$MOCK_LOCK" true 2>/dev/null; then echo held > "$MOCK_LOCKSEEN"; else echo free > "$MOCK_LOCKSEEN"; fi
echo "child-stdout"
exit "${MOCK_RC:-0}"
M
cat > "$T/mock/systemctl" <<'M'
#!/usr/bin/env bash
# 包みは systemctl を呼ばない（呼ばれたら記録して、試験が検出する）
echo "$*" >> "$MOCK_SYSTEMCTL"
M
cat > "$T/mock/id" <<'M'
#!/usr/bin/env bash
# id -u → MOCK_UID（呼んだ人）、id -u <ユーザー> → ユーザーが MOCK_NOUSER でなければ 1234
if [ "$#" = 2 ]; then
    [ "$2" = "${MOCK_NOUSER:-}" ] && exit 1
    echo 1234; exit 0
fi
echo "${MOCK_UID:-0}"
M
chmod +x "$T/mock/"*
printf '#!/bin/sh\n' > "$T/bin/geoframe-export"; chmod +x "$T/bin/geoframe-export"
printf '#!/bin/sh\n' > "$T/bin/rclone"; chmod +x "$T/bin/rclone"
: > "$T/state/rclone/rclone.conf"
: > "$T/vstate/rclone/rclone.conf"   # 検証の準備で本番の設定を写したもの
: > "$T/state/export.lock"   # 本番の実行枠のファイル（サービスが一度でも動くと、専用ユーザーの持ち物としてある）
export PATH="$T/mock:$PATH" MOCK_ARGS="$T/args.log" MOCK_LOCK="$T/state/export.lock" MOCK_LOCKSEEN="$T/lockseen" MOCK_SYSTEMCTL="$T/systemctl.log"
: > "$MOCK_SYSTEMCTL"

# 固定のパス 4 行（バイナリ・rclone・本番の状態ディレクトリ・検証用の状態ディレクトリ）だけを一時ディレクトリの中へ差し替える（rclone の設定のパスは状態ディレクトリの下）
sed -e "s#^BIN=.*#BIN=\"$T/bin/geoframe-export\"#" \
    -e "s#^RCLONE_BIN=.*#RCLONE_BIN=\"$T/bin/rclone\"#" \
    -e "s#^STATE_DIR=.*#STATE_DIR=\"$T/state\"#" \
    -e "s#^VERIFY_STATE_DIR=.*#VERIFY_STATE_DIR=\"$T/vstate\"#" \
    "$SCRIPT" > "$T/admin.sh"
expect "試験の準備: 差し替えた写しが元と違うのは 4 行だけ" bash -c 'test "$(diff "$1" "$2" | grep -c "^>")" = 4' _ "$SCRIPT" "$T/admin.sh"
expect "元のスクリプトの固定のパスは設計どおり" bash -c 'grep -qx "BIN=\"/usr/local/bin/geoframe-export\"" "$1" && grep -qx "RUN_USER=\"geoframe-export\"" "$1" && grep -qx "STATE_DIR=\"/var/lib/geoframe-export\"" "$1" && grep -qx "RCLONE_BIN=\"/usr/local/bin/rclone\"" "$1" && grep -qx "RCLONE_CONF=\"\${STATE_DIR}/rclone/rclone.conf\"" "$1" && grep -qx "VERIFY_STATE_NAME=\"geoframe-export-verify\"" "$1" && grep -qx "VERIFY_STATE_DIR=\"/var/lib/\${VERIFY_STATE_NAME}\"" "$1" && grep -qx "VERIFY_RCLONE_CONF=\"\${VERIFY_STATE_DIR}/rclone/rclone.conf\"" "$1"' _ "$SCRIPT"

run() { # run <引数...> → 標準出力は $T/out、標準エラーは $T/err、終了コードは $rc
    : > "$MOCK_ARGS"; rm -f "$MOCK_LOCKSEEN"
    bash "$T/admin.sh" "$@" > "$T/out" 2> "$T/err"
    rc=$?
}
called() { test -s "$MOCK_ARGS"; }
unit_of() { sed -n 's/^--unit=//p' "$MOCK_ARGS"; }

echo "[1] 動かす"
run run --from 2026-01-10 --destination yamareco --dry-run
expect "終了コード 0・子の標準出力がそのまま出る" bash -c 'test "$1" = 0 && grep -qx child-stdout "$2"' _ "$rc" "$T/out"
expect "一時サービスの名前は呼び出しごとの一意な形（geoframe-export-cli-<日時>-<番号>-<乱数>）" bash -c 'grep -Eqx -e "--unit=geoframe-export-cli-[0-9]{14}-[0-9]+-[0-9]+" "$1"' _ "$MOCK_ARGS"
expect "systemd-run の引数は --uid=専用ユーザー・--wait・--pipe・--collect・--expand-environment=no" bash -c 'for a in --uid=geoframe-export --wait --pipe --collect --expand-environment=no; do grep -qx -e "$a" "$1" || exit 1; done' _ "$MOCK_ARGS"
expect "--expand-environment=no はコマンド（バイナリ）より前にある（後ろだと geoframe-export の引数になる）" bash -c 'a=$(grep -nx -e "--expand-environment=no" "$1" | cut -d: -f1); b=$(grep -nxF "$2" "$1" | cut -d: -f1); test -n "$a" && test -n "$b" && test "$a" -lt "$b"' _ "$MOCK_ARGS" "$T/bin/geoframe-export"
expect "コマンドは バイナリ → 引数 → サービスと同じ設定の引数（本番の --state-dir）の順" bash -c 'diff <(sed -n "/^\/tmp\/.*\/bin\/geoframe-export$/,\$p" "$1") <(printf "%s\n" "$2/bin/geoframe-export" run --from 2026-01-10 --destination yamareco --dry-run --rclone "$2/bin/rclone" --rclone-config "$2/state/rclone/rclone.conf" --feed-base http://127.0.0.1:8080 --state-dir "$2/state") >/dev/null' _ "$MOCK_ARGS" "$T"
expect "本番の入口は StateDirectory=geoframe-export・0700 で、ReadWritePaths は足さない" bash -c 'grep -qx "StateDirectory=geoframe-export" "$1" && grep -qx "StateDirectoryMode=0700" "$1" && ! grep -q "^ReadWritePaths=" "$1"' _ "$MOCK_ARGS"
expect "本番の入口は本番の実行枠を握らない（バイナリが取る）" bash -c 'test "$(cat "$1")" = free' _ "$MOCK_LOCKSEEN"
expect "動かす前にユーザーと状態ディレクトリ（パスだけ）を標準エラーに出す" grep -q "ユーザー geoframe-export、状態ディレクトリ $T/state" "$T/err"
expect "止めるときの unit 名（systemd-run に渡した名前と同じ）を標準エラーに出す" bash -c 'u=$(sed -n "s/^--unit=//p" "$1"); test -n "$u" && grep -qF "sudo systemctl stop $u" "$2"' _ "$MOCK_ARGS" "$T/err"
expect "標準出力には案内を混ぜない（CLI の出力だけ）" bash -c '! grep -q geoframe-export-admin "$1"' _ "$T/out"
u1="$(unit_of)"; run run --from 2026-01-10 --destination yamareco --dry-run; u2="$(unit_of)"
expect "呼び出しごとに unit 名が変わる" bash -c 'test -n "$1" && test -n "$2" && test "$1" != "$2"' _ "$u1" "$u2"
expect "systemctl を呼ばない（実行中かどうかの判定は、バイナリの export.lock に任せる）" test ! -s "$MOCK_SYSTEMCTL"
run run --from '${HOME}' --destination '$X' --on-exists '$$'
expect "引数の中の \${HOME}・\$X・\$\$ は、展開されずそのまま渡る（リテラル）" bash -c 'grep -qxF -e "\${HOME}" "$1" && grep -qxF -e "\$X" "$1" && grep -qxF -e "\$\$" "$1"' _ "$MOCK_ARGS"
run recover --resolve 0123456789abcdef0123456789abcdef
expect "recover --resolve も同じ形で渡す" bash -c 'grep -qx recover "$1" && grep -qx -e --resolve "$1" && grep -qx 0123456789abcdef0123456789abcdef "$1" && grep -qx -e --state-dir "$1"' _ "$MOCK_ARGS"
run run --from 2026-01-10 --destination yamareco --on-exists replace
expect "--on-exists replace も渡す" bash -c 'grep -qx -e --on-exists "$1" && grep -qx replace "$1"' _ "$MOCK_ARGS"
MOCK_RC=3 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（3 conflict）" test "$rc" = 3
MOCK_RC=4 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（4 実行中。実行枠の競合はバイナリが決める）" test "$rc" = 4
MOCK_RC=1 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（1 失敗）" test "$rc" = 1

echo "[2] 動かさない条件"
run serve --listen 192.0.2.1:8090 --token-file /x --allow 192.0.2.10
expect "serve は動かさない（終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run
expect "サブコマンドが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run --version
expect "知らないサブコマンド（--version を含む）は動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run --verify
expect "--verify だけ（サブコマンドが無い）なら動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run --verify serve --listen 192.0.2.1:8090
expect "--verify でも serve は動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
for opt in "--remote-yamareco gdrive:x/y" "--remote-yamareco=gdrive:x/y" "--remote-travel-journal gdrive:x/y" "--remote-travel-journal=gdrive:x/y" "--work-root gdrive:x" "--work-root=gdrive:x"; do
    # shellcheck disable=SC2086
    run run --from 2026-01-10 --destination yamareco $opt
    expect "宛先の接頭辞・作業用フォルダは指定させない（${opt%% *}。終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2" && grep -q -e "--verify" "$3"' _ "$rc" "$MOCK_ARGS" "$T/err"
    # shellcheck disable=SC2086
    run --verify run --from 2026-01-10 --destination yamareco $opt
    expect "--verify でも、宛先の接頭辞・作業用フォルダは指定させない（${opt%% *}）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
MOCK_UID=1000 run recover
expect "root でなければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
MOCK_NOUSER=geoframe-export run recover
expect "専用ユーザーが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/state/rclone/rclone.conf" "$T/state/rclone/rclone.conf.off"; run recover
expect "rclone の設定のファイルが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run --verify recover
expect "--verify は、本番の rclone の設定が無くても（見ない）動かす" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/state/rclone/rclone.conf.off" "$T/state/rclone/rclone.conf"
mv "$T/vstate/rclone/rclone.conf" "$T/vstate/rclone/rclone.conf.off"
run --verify recover
expect "--verify は、検証用の写しが無ければ動かさない（本番の設定があっても。終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
expect "写しが無いときのメッセージは、写しのパスと README の準備の手順を示す（本番の設定のパスは出さない）" bash -c 'grep -qF "$1/vstate/rclone/rclone.conf" "$3" && grep -q "検証用の rclone の設定の写しが無い" "$3" && grep -q "§8.8 の 4" "$3" && ! grep -qF "$1/state/" "$3"' _ "$T" x "$T/err"
run recover
expect "写しが無くても、本番の入口は動かす（写しは検証用だけ）" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/vstate/rclone/rclone.conf.off" "$T/vstate/rclone/rclone.conf"
rm -f "$T/bin/geoframe-export"; run recover
expect "バイナリが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
printf '#!/bin/sh\n' > "$T/bin/geoframe-export"; chmod +x "$T/bin/geoframe-export"
run -h
expect "-h は使い方を出して終了コード 0（systemd-run を呼ばない）" bash -c 'test "$1" = 0 && grep -q "geoframe-export-admin.sh" "$2" && grep -q -e "--verify" "$2" && ! grep -q "^set -euo" "$2" && ! test -s "$3"' _ "$rc" "$T/err" "$MOCK_ARGS"

echo "[3] 検証用の入口（--verify。本番のロックにも rclone の設定にも触れない）"
run --verify run --from 2026-01-10 --destination yamareco --dry-run
expect "終了コード 0・子の標準出力がそのまま出る" bash -c 'test "$1" = 0 && grep -qx child-stdout "$2"' _ "$rc" "$T/out"
expect "コマンドは バイナリ → 引数 → 共通の設定の引数（rclone の設定は検証用の写し）→ 検証用の状態ディレクトリ → 検証用の宛先・作業用フォルダ の順" bash -c 'diff <(sed -n "/^\/tmp\/.*\/bin\/geoframe-export$/,\$p" "$1") <(printf "%s\n" "$2/bin/geoframe-export" run --from 2026-01-10 --destination yamareco --dry-run --rclone "$2/bin/rclone" --rclone-config "$2/vstate/rclone/rclone.conf" --feed-base http://127.0.0.1:8080 --state-dir "$2/vstate" --remote-yamareco gdrive:geoframe-export-verify/yamareco --remote-travel-journal gdrive:geoframe-export-verify/travel-journal --work-root gdrive:geoframe-export-verify-work) >/dev/null' _ "$MOCK_ARGS" "$T"
expect "本番の状態ディレクトリ・本番の接頭辞・本番の作業用フォルダは渡さない" bash -c '! grep -qxF "$2/state" "$1" && ! grep -qxF gdrive:yamareco "$1" && ! grep -qxF gdrive:travel-journal "$1" && ! grep -qxF gdrive:geoframe-export-work "$1"' _ "$MOCK_ARGS" "$T"
expect "検証用の状態ディレクトリは StateDirectory=geoframe-export-verify・0700（本番の StateDirectory は渡さない）" bash -c 'grep -qx "StateDirectory=geoframe-export-verify" "$1" && grep -qx "StateDirectoryMode=0700" "$1" && ! grep -qx "StateDirectory=geoframe-export" "$1"' _ "$MOCK_ARGS"
expect "本番の rclone の設定・rclone/・ロックなど、本番の状態ディレクトリのパスは、引数のどこにも出ない（--rclone-config にも ReadWritePaths= にも）" bash -c '! grep -qF "$2/state" "$1" && ! grep -q "^ReadWritePaths=" "$1"' _ "$MOCK_ARGS" "$T"
expect "systemd-run の引数は本番と同じ（一意な unit 名・--uid・--wait・--pipe・--collect・--expand-environment=no）" bash -c 'grep -Eqx -e "--unit=geoframe-export-cli-[0-9]{14}-[0-9]+-[0-9]+" "$1" && for a in --uid=geoframe-export --wait --pipe --collect --expand-environment=no; do grep -qx -e "$a" "$1" || exit 1; done' _ "$MOCK_ARGS"
expect "標準エラーに、検証用の入口であること・検証用の状態ディレクトリ・止めるときの unit 名を出す" bash -c 'u=$(sed -n "s/^--unit=//p" "$1"); grep -q "検証用の入口" "$2" && grep -q "状態ディレクトリ $3/vstate" "$2" && grep -qF "写し $3/vstate/rclone/rclone.conf" "$2" && ! grep -qF "$3/state/" "$2" && grep -qF "sudo systemctl stop $u" "$2"' _ "$MOCK_ARGS" "$T/err" "$T"
expect "検証用の 3 つの場所は互いに入れ子でなく、本番の既定の場所とも別（最初の成分が違う）" bash -c '
    a=$(grep -x "gdrive:.*" "$1"); first() { v="${1#gdrive:}"; echo "${v%%/*}"; }
    set -- $(printf "%s\n" "$a" | while read -r x; do first "$x"; done)
    y="$1"; t="$2"; w="$3"
    # 検証用の親フォルダ 1 つの下に yamareco・travel-journal（入れ子にならない 2 つ）、作業用フォルダは別の最上位
    test "$y" = "$t" && test "$y" != "$w" && test "$y" != yamareco && test "$y" != travel-journal && test "$w" != geoframe-export-work' _ "$MOCK_ARGS"
expect "検証用の入口は、本番の実行枠（export.lock）を取らない（exec した systemd-run から見て、ロックは空いている）" bash -c 'test "$(cat "$1")" = free' _ "$MOCK_LOCKSEEN"
run --verify recover --resolve 0123456789abcdef0123456789abcdef
expect "--verify recover も同じ形（検証用の状態ディレクトリ）で渡す" bash -c 'grep -qx recover "$1" && grep -qx -e --resolve "$1" && grep -qx "$2/vstate" "$1" && grep -qx -e --work-root "$1"' _ "$MOCK_ARGS" "$T"
u1="$(unit_of)"; run --verify recover; u2="$(unit_of)"
expect "--verify でも呼び出しごとに unit 名が変わる" bash -c 'test -n "$1" && test "$1" != "$2"' _ "$u1" "$u2"
run --verify run --from '${HOME}' --destination yamareco
expect "--verify でも、引数の \${HOME} はリテラルのまま渡る" bash -c 'grep -qxF -e "\${HOME}" "$1"' _ "$MOCK_ARGS"
MOCK_RC=4 run --verify run --from 2026-01-10 --destination yamareco
expect "--verify でも geoframe-export の終了コードをそのまま返す（4 実行中。検証用の実行枠の競合）" test "$rc" = 4
# 本番の実行枠を、別のプロセスが持っている（本番の書き出しの実行中）。検証用の入口は、本番と何も共有しないので、待たず断らず動く
( exec 9<"$T/state/export.lock"; flock -x 9; exec sleep 30 ) >/dev/null 2>&1 &
holder=$!
for _ in $(seq 1 50); do flock -n "$T/state/export.lock" true 2>/dev/null || break; sleep 0.1; done
run --verify run --from 2026-01-10 --destination yamareco --dry-run
expect "本番の書き出しの実行中でも、検証用の入口は動かす（終了コード 0、systemd-run を呼ぶ。本番のロックを取らず、見にも行かない）" bash -c 'test "$1" = 0 && test -s "$2" && ! grep -q "本番の書き出し" "$3"' _ "$rc" "$MOCK_ARGS" "$T/err"
run run --from 2026-01-10 --destination yamareco --dry-run
expect "本番の入口は、実行枠を持つ別の書き出しがあっても systemd-run を呼ぶ（競合はバイナリが終了コード 4 で断る）" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
kill "$holder" 2>/dev/null; wait "$holder" 2>/dev/null
rm -f "$T/state/export.lock"
run --verify run --from 2026-01-10 --destination yamareco --dry-run
expect "本番の実行枠のファイルが無くても（サービスが一度も動いていない）、検証用の入口は動かす" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
: > "$T/state/export.lock"

echo "[4] サービス（geoframe-export.service）との整合"
# unit の ExecStart の引数（\ の続きを 1 行にして、${…}・$… の展開を含む語は除く）
exec_line="$(awk '/^ExecStart=/{f=1} f{printf "%s", $0; if ($0 !~ /\\$/) exit} ' "$UNIT" | sed -e 's/\\//g' -e 's/^ExecStart=//' | tr -s ' ')"
unit_has() { case " $exec_line " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
expect "unit の ExecStart は serve を起動する" unit_has "serve"
for pair in "--rclone /usr/local/bin/rclone" "--rclone-config /var/lib/geoframe-export/rclone/rclone.conf" "--state-dir /var/lib/geoframe-export" "--feed-base http://127.0.0.1:8080"; do
    expect "unit と包みで同じ引数（${pair}）" bash -c 'case " $1 " in *" $2 "*) ;; *) exit 1 ;; esac; grep -q -e "^COMMON_ARGS=(.*" "$3" && case "$2" in --rclone\ *) grep -q "RCLONE_BIN=\"${2#--rclone }\"" "$3" ;; --rclone-config\ *) grep -q "RCLONE_CONF=\"\${STATE_DIR}/rclone/rclone.conf\"" "$3" && [ "${2#--rclone-config }" = "/var/lib/geoframe-export/rclone/rclone.conf" ] ;; --state-dir\ *) grep -q "STATE_DIR=\"${2#--state-dir }\"" "$3" ;; --feed-base\ *) grep -q "FEED_BASE=\"${2#--feed-base }\"" "$3" ;; esac' _ "$exec_line" "$pair" "$SCRIPT"
done
expect "unit の User= と包みの RUN_USER が同じ" bash -c 'u=$(sed -n "s/^User=//p" "$1"); grep -qx "RUN_USER=\"$u\"" "$2"' _ "$UNIT" "$SCRIPT"
expect "unit の StateDirectory= と包みの状態ディレクトリが同じ（/var/lib/ の下）" bash -c 'd=$(sed -n "s/^StateDirectory=//p" "$1"); grep -qx "STATE_DIR=\"/var/lib/$d\"" "$2"' _ "$UNIT" "$SCRIPT"
# ハードニングの値: unit の設定（コメントでない行）を、包みが systemd-run の -p で同じ値のまま渡している
run recover
for key in StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec; do
    val="$(sed -n "s/^${key}=//p" "$UNIT")"
    expect "unit の ${key}=${val} を包みも同じ値で渡す" bash -c 'grep -qxF -e "$1=$2" "$3"' _ "$key" "$val" "$MOCK_ARGS"
done
# 検証用の入口も、StateDirectory= 以外のハードニングは本番と同じ値（StateDirectory は検証用の名前）
run --verify recover
for key in StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec; do
    val="$(sed -n "s/^${key}=//p" "$UNIT")"
    expect "検証用の入口も unit の ${key}=${val} を同じ値で渡す" bash -c 'grep -qxF -e "$1=$2" "$3"' _ "$key" "$val" "$MOCK_ARGS"
done
expect "unit にあるハードニングの設定を包みが渡し忘れていない（コメントでない行の名前が全部、上の一覧にある）" bash -c '
    listed=" StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec "
    skip=" Description Documentation After Wants StartLimitIntervalSec StartLimitBurst Type User EnvironmentFile ExecStart Restart RestartSec RestartPreventExitStatus StandardOutput StandardError SyslogIdentifier WantedBy "
    for k in $(grep -E "^[A-Za-z]+=" "$1" | sed "s/=.*//" | sort -u); do
        case "$listed$skip" in *" $k "*) ;; *) echo "未分類: $k" >&2; exit 1 ;; esac
    done' _ "$UNIT"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
