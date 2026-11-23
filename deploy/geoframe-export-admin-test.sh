#!/usr/bin/env bash
#
# geoframe-export-admin-test.sh - geoframe-export-admin.sh を、モックの systemd-run・systemctl・id で試験する
#
# 実機の systemd にも /usr/local にも /var/lib にも /etc にも触れない。geoframe-export-admin.sh を一時ディレクトリへ写し、
# バイナリ・状態ディレクトリ（本番と検証用）・フォルダの ID の置き場所・資格情報のファイルの固定のパスだけを一時ディレクトリの中へ
# 差し替える。systemd-run は PATH のモックで、受け取った引数を 1 行に 1 つずつ記録する（systemctl のモックは、呼ばれたことを記録するだけ。
# 包みは呼ばない）。検証用の入口（--verify）が本番の実行枠 export.lock にも、本番のフォルダの ID・状態ディレクトリにも触れず（引数にも
# ReadWritePaths= にも出さず）、検証用のフォルダの ID だけを使うことを確かめる。フォルダの ID は、シェルで source せず、決まったキーの
# 行だけを厳しく取り出していること（変な値・重複・引用符・コマンドの実行をしないこと）も確かめる。
# あわせて、geoframe-export.service と包みの設定（--drive-credentials・LoadCredential=・フォルダの ID の変数名・ハードニング）が
# 食い違っていないこと、試験専用の --drive-endpoint と rclone の引数がどこにも残っていないことを検査する。
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

# 架空の Drive の ID（本番の値ではない）
ID_Y=IDyamareco0001 ID_T=IDtravel0002 ID_W=IDwork0003
ID_VY=IDvyamareco0011 ID_VT=IDvtravel0012 ID_VW=IDvwork0013

mkdir -p "$T/mock" "$T/bin" "$T/state" "$T/vstate" "$T/etc"
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
: > "$T/etc/drive-credentials.json"   # 資格情報のファイル（存在の確認だけ。中身は読まない）
write_defaults() { # write_defaults → フォルダの ID の置き場所（本番用と検証用のキー）を作る
    cat > "$T/etc/geoframe-export" <<D
# 試験用の環境ファイル（架空の値）
GEOFRAME_EXPORT_LISTEN=192.0.2.1:8090
GEOFRAME_EXPORT_FOLDER_YAMARECO=$ID_Y
GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL=$ID_T
GEOFRAME_EXPORT_WORK_FOLDER=$ID_W
GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO=$ID_VY
GEOFRAME_EXPORT_VERIFY_FOLDER_TRAVEL_JOURNAL=$ID_VT
GEOFRAME_EXPORT_VERIFY_WORK_FOLDER=$ID_VW
D
}
write_defaults
: > "$T/state/export.lock"   # 本番の実行枠のファイル（サービスが一度でも動くと、専用ユーザーの持ち物としてある）
export PATH="$T/mock:$PATH" MOCK_ARGS="$T/args.log" MOCK_LOCK="$T/state/export.lock" MOCK_LOCKSEEN="$T/lockseen" MOCK_SYSTEMCTL="$T/systemctl.log"
: > "$MOCK_SYSTEMCTL"

# 固定のパス 5 行（バイナリ・本番の状態ディレクトリ・検証用の状態ディレクトリ・フォルダの ID の置き場所・資格情報のファイル）だけを
# 一時ディレクトリの中へ差し替える。LoadCredential= に渡す元のパス（CRED_UNIT_PATH）は差し替えない（unit と同じ値のまま比べる）
sed -e "s#^BIN=.*#BIN=\"$T/bin/geoframe-export\"#" \
    -e "s#^STATE_DIR=.*#STATE_DIR=\"$T/state\"#" \
    -e "s#^VERIFY_STATE_DIR=.*#VERIFY_STATE_DIR=\"$T/vstate\"#" \
    -e "s#^DEFAULTS_FILE=.*#DEFAULTS_FILE=\"$T/etc/geoframe-export\"#" \
    -e "s#^CRED_FILE=.*#CRED_FILE=\"$T/etc/drive-credentials.json\"#" \
    "$SCRIPT" > "$T/admin.sh"
expect "試験の準備: 差し替えた写しが元と違うのは 5 行だけ" bash -c 'test "$(diff "$1" "$2" | grep -c "^>")" = 5' _ "$SCRIPT" "$T/admin.sh"
expect "元のスクリプトの固定のパスは設計どおり" bash -c 'grep -qx "BIN=\"/usr/local/bin/geoframe-export\"" "$1" && grep -qx "RUN_USER=\"geoframe-export\"" "$1" && grep -qx "STATE_DIR=\"/var/lib/geoframe-export\"" "$1" && grep -qx "CRED_UNIT_PATH=\"/etc/geoframe-export/drive-credentials.json\"" "$1" && grep -qx "DEFAULTS_FILE=\"/etc/default/geoframe-export\"" "$1" && grep -qx "VERIFY_STATE_NAME=\"geoframe-export-verify\"" "$1" && grep -qx "VERIFY_STATE_DIR=\"/var/lib/\${VERIFY_STATE_NAME}\"" "$1"' _ "$SCRIPT"

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
expect "コマンドは バイナリ → 引数 → サービスと同じ設定の引数（資格情報・本番のフォルダの ID・本番の --state-dir）の順" bash -c 'diff <(sed -n "/^\/tmp\/.*\/bin\/geoframe-export$/,\$p" "$1") <(printf "%s\n" "$2/bin/geoframe-export" run --from 2026-01-10 --destination yamareco --dry-run --drive-credentials %d/drive-credentials --folder-yamareco '"$ID_Y"' --folder-travel-journal '"$ID_T"' --work-folder '"$ID_W"' --feed-base http://127.0.0.1:8080 --state-dir "$2/state") >/dev/null' _ "$MOCK_ARGS" "$T"
expect "資格情報は LoadCredential=drive-credentials:<root 所有のファイル> で渡し、--drive-credentials は %d/drive-credentials（unit と同じ）" bash -c 'grep -qx "LoadCredential=drive-credentials:/etc/geoframe-export/drive-credentials.json" "$1" && grep -qx -e "%d/drive-credentials" "$1"' _ "$MOCK_ARGS"
expect "本番の入口は StateDirectory=geoframe-export・0700 で、ReadWritePaths は足さない" bash -c 'grep -qx "StateDirectory=geoframe-export" "$1" && grep -qx "StateDirectoryMode=0700" "$1" && ! grep -q "^ReadWritePaths=" "$1"' _ "$MOCK_ARGS"
expect "本番の入口は本番の実行枠を握らない（バイナリが取る）" bash -c 'test "$(cat "$1")" = free' _ "$MOCK_LOCKSEEN"
expect "動かす前にユーザーと状態ディレクトリ（パスだけ）を標準エラーに出す" grep -q "ユーザー geoframe-export、状態ディレクトリ $T/state" "$T/err"
expect "止めるときの unit 名（systemd-run に渡した名前と同じ）を標準エラーに出す" bash -c 'u=$(sed -n "s/^--unit=//p" "$1"); test -n "$u" && grep -qF "sudo systemctl stop $u" "$2"' _ "$MOCK_ARGS" "$T/err"
expect "標準エラーにも標準出力にも、Drive のフォルダの ID の値は出ない" bash -c '! grep -q -e "$3" -e "$4" -e "$5" "$1" "$2"' _ "$T/err" "$T/out" "$ID_Y" "$ID_T" "$ID_W"
expect "標準出力には案内を混ぜない（CLI の出力だけ）" bash -c '! grep -q geoframe-export-admin "$1"' _ "$T/out"
u1="$(unit_of)"; run run --from 2026-01-10 --destination yamareco --dry-run; u2="$(unit_of)"
expect "呼び出しごとに unit 名が変わる" bash -c 'test -n "$1" && test -n "$2" && test "$1" != "$2"' _ "$u1" "$u2"
expect "systemctl を呼ばない（実行中かどうかの判定は、バイナリの export.lock に任せる）" test ! -s "$MOCK_SYSTEMCTL"
run run --from '${HOME}' --destination '$X' --on-exists '$$'
expect "引数の中の \${HOME}・\$X・\$\$ は、展開されずそのまま渡る（リテラル）" bash -c 'grep -qxF -e "\${HOME}" "$1" && grep -qxF -e "\$X" "$1" && grep -qxF -e "\$\$" "$1"' _ "$MOCK_ARGS"
run recover --resolve 0123456789abcdef0123456789abcdef
expect "recover --resolve も同じ形で渡す" bash -c 'grep -qx recover "$1" && grep -qx -e --resolve "$1" && grep -qx 0123456789abcdef0123456789abcdef "$1" && grep -qx -e --state-dir "$1" && grep -qx -e --folder-yamareco "$1"' _ "$MOCK_ARGS"
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
for opt in "--folder-yamareco x" "--folder-yamareco=x" "--folder-travel-journal x" "--folder-travel-journal=x" "--work-folder x" "--work-folder=x"; do
    # shellcheck disable=SC2086
    run run --from 2026-01-10 --destination yamareco $opt
    expect "宛先の親フォルダ・作業用フォルダは指定させない（${opt%% *}。終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2" && grep -q -e "--verify" "$3"' _ "$rc" "$MOCK_ARGS" "$T/err"
    # shellcheck disable=SC2086
    run --verify run --from 2026-01-10 --destination yamareco $opt
    expect "--verify でも、宛先の親フォルダ・作業用フォルダは指定させない（${opt%% *}）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
for opt in "--drive-credentials x" "--drive-credentials=x" "--drive-endpoint http://127.0.0.1:1" "--drive-endpoint=http://127.0.0.1:1"; do
    # shellcheck disable=SC2086
    run run --from 2026-01-10 --destination yamareco $opt
    expect "資格情報・試験専用の通信先は指定させない（${opt%% *}。終了コード 2、systemd-run を呼ばない）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
    # shellcheck disable=SC2086
    run --verify recover $opt
    expect "--verify でも、資格情報・試験専用の通信先は指定させない（${opt%% *}）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
MOCK_UID=1000 run recover
expect "root でなければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
MOCK_NOUSER=geoframe-export run recover
expect "専用ユーザーが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/etc/drive-credentials.json" "$T/etc/drive-credentials.json.off"
run recover
expect "資格情報のファイルが無ければ動かさない（終了コード 2）。import-credentials で作る案内を出す" bash -c 'test "$1" = 2 && ! test -s "$2" && grep -q "import-credentials" "$3"' _ "$rc" "$MOCK_ARGS" "$T/err"
run --verify recover
expect "--verify も、資格情報のファイル（本番と同じもの）が無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/etc/drive-credentials.json.off" "$T/etc/drive-credentials.json"
rm -f "$T/bin/geoframe-export"; run recover
expect "バイナリが無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
printf '#!/bin/sh\n' > "$T/bin/geoframe-export"; chmod +x "$T/bin/geoframe-export"
run -h
expect "-h は使い方を出して終了コード 0（systemd-run を呼ばない）" bash -c 'test "$1" = 0 && grep -q "geoframe-export-admin.sh" "$2" && grep -q -e "--verify" "$2" && ! grep -q "^set -euo" "$2" && ! test -s "$3"' _ "$rc" "$T/err" "$MOCK_ARGS"

echo "[2b] フォルダの ID の読み方（source しない。決まったキーの行だけを厳しく取り出す）"
mv "$T/etc/geoframe-export" "$T/etc/geoframe-export.off"
run recover
expect "フォルダの ID の置き場所が無ければ動かさない（終了コード 2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
mv "$T/etc/geoframe-export.off" "$T/etc/geoframe-export"
for key in GEOFRAME_EXPORT_FOLDER_YAMARECO GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL GEOFRAME_EXPORT_WORK_FOLDER; do
    write_defaults; grep -v "^${key}=" "$T/etc/geoframe-export" > "$T/etc/d.tmp"; mv "$T/etc/d.tmp" "$T/etc/geoframe-export"
    run recover
    expect "${key} が無ければ動かさない（終了コード 2。キー名を示す）" bash -c 'test "$1" = 2 && ! test -s "$2" && grep -q "$3" "$4"' _ "$rc" "$MOCK_ARGS" "$key" "$T/err"
    run --verify recover
    expect "${key} が無くても、検証用の入口は動かす（検証用のキーを読む）" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
write_defaults
for bad in "" "has space" "a;touch $T/pwned" 'a$(touch '"$T"'/pwned)' '"quoted"' "'quoted'" "a/b" "a.b" "日本語" "$(printf 'a%.0s' $(seq 1 129))"; do
    write_defaults
    grep -v "^GEOFRAME_EXPORT_WORK_FOLDER=" "$T/etc/geoframe-export" > "$T/etc/d.tmp"
    printf 'GEOFRAME_EXPORT_WORK_FOLDER=%s\n' "$bad" >> "$T/etc/d.tmp"; mv "$T/etc/d.tmp" "$T/etc/geoframe-export"
    run recover
    expect "Drive の ID の形でない値は断る（終了コード 2、systemd-run を呼ばない）: [${bad:0:24}]" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
expect "値の中のコマンドは実行されない（source していない）" test ! -e "$T/pwned"
write_defaults; echo "GEOFRAME_EXPORT_WORK_FOLDER=IDsecond0004" >> "$T/etc/geoframe-export"
run recover
expect "同じキーが 2 行あれば断る（終了コード 2。後勝ちにしない）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
write_defaults; { echo "#GEOFRAME_EXPORT_WORK_FOLDER=IDcommented"; echo " GEOFRAME_EXPORT_WORK_FOLDER=IDindented"; echo "export GEOFRAME_EXPORT_WORK_FOLDER=IDexported"; cat "$T/etc/geoframe-export"; } > "$T/etc/d.tmp"; mv "$T/etc/d.tmp" "$T/etc/geoframe-export"
run recover
expect "コメント・字下げ・export つきの行は読まない（キーの行だけ。本物の行を使う）" bash -c 'test "$1" = 0 && grep -qx '"$ID_W"' "$2" && ! grep -q -e IDcommented -e IDindented -e IDexported "$2"' _ "$rc" "$MOCK_ARGS"
write_defaults
run recover
expect "フォルダの ID を戻せば、また動かす" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"

echo "[3] 検証用の入口（--verify。本番のロックにも本番のフォルダにも触れない）"
run --verify run --from 2026-01-10 --destination yamareco --dry-run
expect "終了コード 0・子の標準出力がそのまま出る" bash -c 'test "$1" = 0 && grep -qx child-stdout "$2"' _ "$rc" "$T/out"
expect "コマンドは バイナリ → 引数 → 資格情報（本番と同じ）→ 検証用のフォルダの ID → 検証用の状態ディレクトリ の順" bash -c 'diff <(sed -n "/^\/tmp\/.*\/bin\/geoframe-export$/,\$p" "$1") <(printf "%s\n" "$2/bin/geoframe-export" run --from 2026-01-10 --destination yamareco --dry-run --drive-credentials %d/drive-credentials --folder-yamareco '"$ID_VY"' --folder-travel-journal '"$ID_VT"' --work-folder '"$ID_VW"' --feed-base http://127.0.0.1:8080 --state-dir "$2/vstate") >/dev/null' _ "$MOCK_ARGS" "$T"
expect "本番の状態ディレクトリ・本番のフォルダの ID は渡さない" bash -c '! grep -qxF "$2/state" "$1" && ! grep -qxF "$3" "$1" && ! grep -qxF "$4" "$1" && ! grep -qxF "$5" "$1"' _ "$MOCK_ARGS" "$T" "$ID_Y" "$ID_T" "$ID_W"
expect "検証用の状態ディレクトリは StateDirectory=geoframe-export-verify・0700（本番の StateDirectory は渡さない）" bash -c 'grep -qx "StateDirectory=geoframe-export-verify" "$1" && grep -qx "StateDirectoryMode=0700" "$1" && ! grep -qx "StateDirectory=geoframe-export" "$1"' _ "$MOCK_ARGS"
expect "資格情報は本番と同じ元のファイルを同じ形で渡す（検証用の写しは作らない）" bash -c 'grep -qx "LoadCredential=drive-credentials:/etc/geoframe-export/drive-credentials.json" "$1"' _ "$MOCK_ARGS"
expect "本番の状態ディレクトリ・ロックなどのパスは、引数のどこにも出ない（--state-dir にも ReadWritePaths= にも）" bash -c '! grep -qF "$2/state" "$1" && ! grep -q "^ReadWritePaths=" "$1"' _ "$MOCK_ARGS" "$T"
expect "systemd-run の引数は本番と同じ（一意な unit 名・--uid・--wait・--pipe・--collect・--expand-environment=no）" bash -c 'grep -Eqx -e "--unit=geoframe-export-cli-[0-9]{14}-[0-9]+-[0-9]+" "$1" && for a in --uid=geoframe-export --wait --pipe --collect --expand-environment=no; do grep -qx -e "$a" "$1" || exit 1; done' _ "$MOCK_ARGS"
expect "標準エラーに、検証用の入口であること・検証用の状態ディレクトリ・止めるときの unit 名を出す（Drive の ID の値は出さない）" bash -c 'u=$(sed -n "s/^--unit=//p" "$1"); grep -q "検証用の入口" "$2" && grep -q "状態ディレクトリ $3/vstate" "$2" && ! grep -qF "$3/state/" "$2" && grep -qF "sudo systemctl stop $u" "$2" && ! grep -q -e "$4" -e "$5" -e "$6" "$2"' _ "$MOCK_ARGS" "$T/err" "$T" "$ID_VY" "$ID_VT" "$ID_VW"
expect "検証用の入口は、本番の実行枠（export.lock）を取らない（exec した systemd-run から見て、ロックは空いている）" bash -c 'test "$(cat "$1")" = free' _ "$MOCK_LOCKSEEN"
run --verify recover --resolve 0123456789abcdef0123456789abcdef
expect "--verify recover も同じ形（検証用の状態ディレクトリ）で渡す" bash -c 'grep -qx recover "$1" && grep -qx -e --resolve "$1" && grep -qx "$2/vstate" "$1" && grep -qx -e --work-folder "$1"' _ "$MOCK_ARGS" "$T"
u1="$(unit_of)"; run --verify recover; u2="$(unit_of)"
expect "--verify でも呼び出しごとに unit 名が変わる" bash -c 'test -n "$1" && test "$1" != "$2"' _ "$u1" "$u2"
run --verify run --from '${HOME}' --destination yamareco
expect "--verify でも、引数の \${HOME} はリテラルのまま渡る" bash -c 'grep -qxF -e "\${HOME}" "$1"' _ "$MOCK_ARGS"
MOCK_RC=4 run --verify run --from 2026-01-10 --destination yamareco
expect "--verify でも geoframe-export の終了コードをそのまま返す（4 実行中。検証用の実行枠の競合）" test "$rc" = 4
for key in GEOFRAME_EXPORT_VERIFY_FOLDER_YAMARECO GEOFRAME_EXPORT_VERIFY_FOLDER_TRAVEL_JOURNAL GEOFRAME_EXPORT_VERIFY_WORK_FOLDER; do
    write_defaults; grep -v "^${key}=" "$T/etc/geoframe-export" > "$T/etc/d.tmp"; mv "$T/etc/d.tmp" "$T/etc/geoframe-export"
    run --verify recover
    expect "${key} が無ければ、検証用の入口は動かさない（終了コード 2。キー名を示す。本番のフォルダへ落とさない）" bash -c 'test "$1" = 2 && ! test -s "$2" && grep -q "$3" "$4"' _ "$rc" "$MOCK_ARGS" "$key" "$T/err"
    run recover
    expect "${key} が無くても、本番の入口は動かす（検証用のキーは本番では読まない）" bash -c 'test "$1" = 0 && test -s "$2"' _ "$rc" "$MOCK_ARGS"
done
write_defaults
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
# unit の ExecStart の引数（\ の続きを 1 行にする。${…}・$… の展開を含む語は、展開する前の形のまま比べる）
exec_line="$(awk '/^ExecStart=/{f=1} f{printf "%s", $0; if ($0 !~ /\\$/) exit} ' "$UNIT" | sed -e 's/\\//g' -e 's/^ExecStart=//' | tr -s ' ')"
unit_has() { case " $exec_line " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
expect "unit の ExecStart は serve を起動する" unit_has "serve"
for pair in "--drive-credentials %d/drive-credentials" "--state-dir /var/lib/geoframe-export" "--feed-base http://127.0.0.1:8080"; do
    expect "unit と包みで同じ引数（${pair}）" bash -c 'case " $1 " in *" $2 "*) ;; *) exit 1 ;; esac; case "$2" in --drive-credentials\ *) grep -qF -e "--drive-credentials \"${2#--drive-credentials }\"" "$3" ;; --state-dir\ *) grep -q "^STATE_DIR=\"${2#--state-dir }\"" "$3" ;; --feed-base\ *) grep -q "FEED_BASE=\"${2#--feed-base }\"" "$3" ;; esac' _ "$exec_line" "$pair" "$SCRIPT"
done
for pair in "--folder-yamareco GEOFRAME_EXPORT_FOLDER_YAMARECO FOLDER_YAMARECO" "--folder-travel-journal GEOFRAME_EXPORT_FOLDER_TRAVEL_JOURNAL FOLDER_TRAVEL_JOURNAL" "--work-folder GEOFRAME_EXPORT_WORK_FOLDER WORK_FOLDER"; do
    set -- $pair
    expect "unit は ${1} に /etc/default/geoframe-export の \${$2} を渡し、包みは同じ名前のキー（検証用は VERIFY_ 付き）を読む" bash -c 'case " $1 " in *" $2 \${$3} "*) ;; *) exit 1 ;; esac; grep -qF "read_id \"\${key_prefix}$4\"" "$5" && grep -qx "    key_prefix=\"GEOFRAME_EXPORT_\"" "$5" && grep -qx "    key_prefix=\"GEOFRAME_EXPORT_VERIFY_\"" "$5" && test "$3" = "GEOFRAME_EXPORT_$4"' _ "$exec_line" "$1" "$2" "$3" "$SCRIPT"
done
expect "unit の LoadCredential= と包みの LoadCredential= の元のファイルが同じ" bash -c 'u=$(sed -n "s/^LoadCredential=//p" "$1"); grep -qxF "CRED_UNIT_PATH=\"${u#drive-credentials:}\"" "$2" && grep -qF "\"LoadCredential=drive-credentials:\${CRED_UNIT_PATH}\"" "$2"' _ "$UNIT" "$SCRIPT"
expect "試験専用の --drive-endpoint は、unit の ExecStart にも、包みが systemd-run に渡す引数にも出ない" bash -c '! case "$1" in *--drive-endpoint*) true ;; *) false ;; esac && ! grep -q -e "--drive-endpoint" "$2"' _ "$exec_line" "$MOCK_ARGS"
expect "rclone の引数（--rclone・--rclone-config・--remote-*・--work-root・--rclone-*-timeout）は、unit の ExecStart にも、包みの引数・設定にも残っていない" bash -c '! case "$1" in *--rclone*|*--remote-*|*--work-root*) true ;; *) false ;; esac && ! grep -q -e "--rclone" -e "--remote-" -e "--work-root" -e "RCLONE" -e "rclone_conf" "$2" && ! grep -q -e "--rclone" -e "--remote-" -e "--work-root" "$3"' _ "$exec_line" "$SCRIPT" "$MOCK_ARGS"
expect "unit の User= と包みの RUN_USER が同じ" bash -c 'u=$(sed -n "s/^User=//p" "$1"); grep -qx "RUN_USER=\"$u\"" "$2"' _ "$UNIT" "$SCRIPT"
expect "unit の StateDirectory= と包みの状態ディレクトリが同じ（/var/lib/ の下）" bash -c 'd=$(sed -n "s/^StateDirectory=//p" "$1"); grep -qx "STATE_DIR=\"/var/lib/$d\"" "$2"' _ "$UNIT" "$SCRIPT"
# ハードニングの値: unit の設定（コメントでない行）を、包みが systemd-run の -p で同じ値のまま渡している
run recover
for key in LoadCredential StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec; do
    val="$(sed -n "s/^${key}=//p" "$UNIT")"
    expect "unit の ${key}=${val} を包みも同じ値で渡す" bash -c 'grep -qxF -e "$1=$2" "$3"' _ "$key" "$val" "$MOCK_ARGS"
done
# 検証用の入口も、StateDirectory= 以外のハードニングは本番と同じ値（StateDirectory は検証用の名前）
run --verify recover
for key in LoadCredential StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec; do
    val="$(sed -n "s/^${key}=//p" "$UNIT")"
    expect "検証用の入口も unit の ${key}=${val} を同じ値で渡す" bash -c 'grep -qxF -e "$1=$2" "$3"' _ "$key" "$val" "$MOCK_ARGS"
done
expect "MemoryMax・TasksMax の値は、この段では変えていない（本番の関門で測り直す）" bash -c 'grep -qx "MemoryMax=512M" "$1" && grep -qx "TasksMax=64" "$1"' _ "$UNIT"
expect "unit にあるハードニングの設定を包みが渡し忘れていない（コメントでない行の名前が全部、上の一覧にある）" bash -c '
    listed=" LoadCredential StateDirectory StateDirectoryMode UMask NoNewPrivileges ProtectSystem ProtectHome PrivateTmp PrivateDevices CapabilityBoundingSet RestrictAddressFamilies SystemCallFilter SystemCallArchitectures ProtectKernelTunables ProtectKernelModules ProtectKernelLogs ProtectControlGroups RestrictNamespaces LockPersonality RestrictSUIDSGID MemoryDenyWriteExecute MemoryMax TasksMax KillMode TimeoutStopSec "
    skip=" Description Documentation After Wants StartLimitIntervalSec StartLimitBurst Type User EnvironmentFile ExecStart Restart RestartSec RestartPreventExitStatus StandardOutput StandardError SyslogIdentifier WantedBy "
    for k in $(grep -E "^[A-Za-z]+=" "$1" | sed "s/=.*//" | sort -u); do
        case "$listed$skip" in *" $k "*) ;; *) echo "未分類: $k" >&2; exit 1 ;; esac
    done' _ "$UNIT"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
