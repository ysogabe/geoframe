#!/usr/bin/env bash
#
# geoframe-export-admin-test.sh - geoframe-export-admin.sh を、モックの systemd-run・systemctl・id で試験する
#
# 実機の systemd にも /usr/local にも /var/lib にも触れない。geoframe-export-admin.sh を一時ディレクトリへ写し、
# バイナリ・rclone・設定ファイルの固定のパスだけを一時ディレクトリの中へ差し替える。systemd-run は PATH のモックで、
# 受け取った引数を 1 行に 1 つずつ記録する。あわせて、geoframe-export.service と包みの設定が食い違っていないことを検査する。
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

mkdir -p "$T/mock" "$T/bin" "$T/state/rclone"
cat > "$T/mock/systemd-run" <<'M'
#!/usr/bin/env bash
: > "$MOCK_ARGS"
for a in "$@"; do printf '%s\n' "$a" >> "$MOCK_ARGS"; done
echo "child-stdout"
exit "${MOCK_RC:-0}"
M
cat > "$T/mock/systemctl" <<'M'
#!/usr/bin/env bash
# systemctl is-active --quiet <unit> だけを受ける。MOCK_ACTIVE=1 なら「動いている」(0)
[ "${MOCK_ACTIVE:-0}" = 1 ]
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
export PATH="$T/mock:$PATH" MOCK_ARGS="$T/args.log"

sed -e "s#^BIN=.*#BIN=\"$T/bin/geoframe-export\"#" \
    -e "s#^RCLONE_BIN=.*#RCLONE_BIN=\"$T/bin/rclone\"#" \
    -e "s#^RCLONE_CONF=.*#RCLONE_CONF=\"$T/state/rclone/rclone.conf\"#" \
    "$SCRIPT" > "$T/admin.sh"
expect "試験の準備: 差し替えた写しが元と違うのは 3 行だけ" bash -c 'test "$(diff "$1" "$2" | grep -c "^>")" = 3' _ "$SCRIPT" "$T/admin.sh"
expect "元のスクリプトの固定のパスは設計どおり" bash -c 'grep -qx "BIN=\"/usr/local/bin/geoframe-export\"" "$1" && grep -qx "RUN_USER=\"geoframe-export\"" "$1" && grep -qx "STATE_DIR=\"/var/lib/geoframe-export\"" "$1" && grep -qx "RCLONE_BIN=\"/usr/local/bin/rclone\"" "$1" && grep -qx "RCLONE_CONF=\"\${STATE_DIR}/rclone/rclone.conf\"" "$1"' _ "$SCRIPT"

run() { # run <引数...> → 標準出力は $T/out、標準エラーは $T/err、終了コードは $rc
    : > "$MOCK_ARGS"
    bash "$T/admin.sh" "$@" > "$T/out" 2> "$T/err"
    rc=$?
}
called() { test -s "$MOCK_ARGS"; }

echo "[1] 動かす"
run run --from 2026-01-10 --destination yamareco --dry-run
expect "終了コード 0・子の標準出力がそのまま出る" bash -c 'test "$1" = 0 && grep -qx child-stdout "$2"' _ "$rc" "$T/out"
expect "systemd-run の最初の引数は固定の unit 名・専用ユーザー・--wait --pipe --collect" bash -c 'head -n 5 "$1" | diff - <(printf "%s\n" --unit=geoframe-export-cli --uid=geoframe-export --wait --pipe --collect) >/dev/null' _ "$MOCK_ARGS"
expect "コマンドは バイナリ → 引数 → サービスと同じ設定の引数 の順" bash -c 'tail -n 15 "$1" | diff - <(printf "%s\n" "$2/bin/geoframe-export" run --from 2026-01-10 --destination yamareco --dry-run --rclone "$2/bin/rclone" --rclone-config "$2/state/rclone/rclone.conf" --state-dir /var/lib/geoframe-export --feed-base http://127.0.0.1:8080) >/dev/null' _ "$MOCK_ARGS" "$T"
expect "動かす前にユーザーと状態ディレクトリ（パスだけ）を標準エラーに出す" grep -q 'ユーザー geoframe-export、状態ディレクトリ /var/lib/geoframe-export' "$T/err"
expect "標準出力には案内を混ぜない（CLI の出力だけ）" bash -c '! grep -q geoframe-export-admin "$1"' _ "$T/out"
run recover --resolve 0123456789abcdef0123456789abcdef
expect "recover --resolve も同じ形で渡す" bash -c 'grep -qx recover "$1" && grep -qx -e --resolve "$1" && grep -qx 0123456789abcdef0123456789abcdef "$1" && grep -qx -e --state-dir "$1"' _ "$MOCK_ARGS"
run run --from 2026-01-10 --destination yamareco --on-exists replace
expect "--on-exists replace も渡す" bash -c 'grep -qx -e --on-exists "$1" && grep -qx replace "$1"' _ "$MOCK_ARGS"
MOCK_RC=3 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（3 conflict）" test "$rc" = 3
MOCK_RC=4 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（4 実行中）" test "$rc" = 4
MOCK_RC=1 run run --from 2026-01-10 --destination yamareco
expect "geoframe-export の終了コードをそのまま返す（1 失敗）" test "$rc" = 1

echo "[2] 動かさない条件"
run serve --listen 192.0.2.1:8090 --token-file /x --allow 192.0.2.10
expect "serve は動かさない（終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run
expect "サブコマンドが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run --version
expect "知らないサブコマンド（--version を含む）は動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
MOCK_UID=1000 run recover
expect "root でなければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
MOCK_NOUSER=geoframe-export run recover
expect "専用ユーザーが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/state/rclone/rclone.conf" "$T/state/rclone/rclone.conf.off"; run recover
expect "rclone の設定のファイルが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/state/rclone/rclone.conf.off" "$T/state/rclone/rclone.conf"
rm -f "$T/bin/geoframe-export"; run recover
expect "バイナリが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
printf '#!/bin/sh\n' > "$T/bin/geoframe-export"; chmod +x "$T/bin/geoframe-export"
MOCK_ACTIVE=1 run recover
expect "一時サービスが動いていれば動かさない（終了コード 4）" bash -c 'test "$1" = 4 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
run -h
expect "-h は使い方を出して終了コード 0（systemd-run を呼ばない）" bash -c 'test "$1" = 0 && grep -q "geoframe-export-admin.sh" "$2" && ! test -s "$3"' _ "$rc" "$T/err" "$MOCK_ARGS"

echo "[3] サービス（geoframe-export.service）との整合"
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
for key in StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID TimeoutStopSec; do
    val="$(sed -n "s/^${key}=//p" "$UNIT")"
    expect "unit の ${key}=${val} を包みも同じ値で渡す" bash -c 'grep -qxF -e "$1=$2" "$3"' _ "$key" "$val" "$MOCK_ARGS"
done
expect "unit にあるハードニングの設定を包みが渡し忘れていない（コメントでない行の名前が全部、上の一覧にある）" bash -c '
    listed=" StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID TimeoutStopSec "
    skip=" Description Documentation After Wants StartLimitIntervalSec StartLimitBurst Type User EnvironmentFile ExecStart KillMode Restart RestartSec RestartPreventExitStatus StandardOutput StandardError SyslogIdentifier WantedBy "
    for k in $(grep -E "^[A-Za-z]+=" "$1" | sed "s/=.*//" | sort -u); do
        case "$listed$skip" in *" $k "*) ;; *) echo "未分類: $k" >&2; exit 1 ;; esac
    done' _ "$UNIT"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
