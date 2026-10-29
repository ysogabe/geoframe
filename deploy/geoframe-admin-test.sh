#!/usr/bin/env bash
#
# geoframe-admin-test.sh - geoframe-admin.sh を、モックの systemd-run・id で試験する
#
# 実機の systemd にも /etc/default にも触れない。geoframe-admin.sh を一時ディレクトリへ写し、設定のファイルと
# バイナリの固定のパスだけを一時ディレクトリの中へ差し替える。systemd-run は PATH のモックで、受け取った引数を
# 1 行に 1 つずつ記録する。
#
# 実行: bash deploy/geoframe-admin-test.sh
#
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$(mktemp -d /tmp/geoframe-admin-test.XXXXXX)"
trap 'rm -rf "$T"' EXIT

pass=0 failn=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
ng()   { failn=$((failn + 1)); echo "  FAIL $*"; }
expect() { local d="$1"; shift; if "$@"; then ok "$d"; else ng "$d"; fi; }

mkdir -p "$T/mock" "$T/etc" "$T/bin"
cat > "$T/mock/systemd-run" <<'M'
#!/usr/bin/env bash
: > "$MOCK_ARGS"
for a in "$@"; do printf '%s\n' "$a" >> "$MOCK_ARGS"; done
echo "child-stdout"
exit "${MOCK_RC:-0}"
M
cat > "$T/mock/id" <<'M'
#!/usr/bin/env bash
echo "${MOCK_UID:-0}"
M
chmod +x "$T/mock/"*
printf '#!/bin/sh\n' > "$T/bin/geoframe"; chmod +x "$T/bin/geoframe"
export PATH="$T/mock:$PATH" MOCK_ARGS="$T/args.log"

sed -e "s#^ENV_COMMON=.*#ENV_COMMON=\"$T/etc/geoframe\"#" \
    -e "s#^ENV_RS=.*#ENV_RS=\"$T/etc/geoframe-rs\"#" \
    -e "s#^BIN=.*#BIN=\"$T/bin/geoframe\"#" \
    "$HERE/geoframe-admin.sh" > "$T/admin.sh"
expect "試験の準備: 差し替えた写しが元と違うのは 3 行だけ" bash -c 'test "$(diff "$1" "$2" | grep -c "^>")" = 3' _ "$HERE/geoframe-admin.sh" "$T/admin.sh"
expect "元のスクリプトの固定のパスは設計どおり" bash -c 'grep -qx "ENV_COMMON=\"/etc/default/geoframe\"" "$1" && grep -qx "ENV_RS=\"/etc/default/geoframe-rs\"" "$1" && grep -qx "BIN=\"/usr/local/bin/geoframe\"" "$1"' _ "$HERE/geoframe-admin.sh"

run() { # run <引数...> → 標準出力は $T/out、標準エラーは $T/err、終了コードは $rc
    : > "$MOCK_ARGS"
    bash "$T/admin.sh" "$@" > "$T/out" 2> "$T/err"
    rc=$?
}
write_rs() { printf '%s\n' "$@" > "$T/etc/geoframe-rs"; chmod 0600 "$T/etc/geoframe-rs"; }
called() { test -s "$MOCK_ARGS"; }

echo "[1] 動かす"
write_rs 'TZ=Asia/Tokyo' 'GEOFRAME_DATA_DIR=/srv/geoframe-data' 'GEOFRAME_SECRET_FOR_TEST=decoy-value' 'GEOFRAME_INGEST=1'
run ai-jobs status
expect "終了コード0・子の標準出力がそのまま出る" bash -c 'test "$1" = 0 && grep -qx child-stdout "$2"' _ "$rc" "$T/out"
expect "systemd-run の引数は設計どおり（順序も）" bash -c 'diff <(cat "$1") <(printf "%s\n" --uid=geoframe --wait --pipe --collect "-p" "EnvironmentFile=-$2/etc/geoframe" "-p" "EnvironmentFile=$2/etc/geoframe-rs" "-p" "UMask=0027" "-p" "ReadOnlyPaths=-/var/lib/geoframe/photos" "$2/bin/geoframe" ai-jobs status) >/dev/null' _ "$MOCK_ARGS" "$T"
expect "動かす前に DATA_DIR（パス）を標準エラーに出す" grep -q 'DATA_DIR: /srv/geoframe-data' "$T/err"
expect "設定のほかの値は出さない（標準出力・標準エラーのどちらにも）" bash -c '! grep -q -e decoy-value -e GEOFRAME_SECRET -e Asia/Tokyo "$1" "$2"' _ "$T/out" "$T/err"
expect "標準出力には DATA_DIR の表示を混ぜない（CLI の出力だけ）" bash -c '! grep -q geoframe-data "$1"' _ "$T/out"
run ai-jobs retry --kind photo_judge --all 'a b'
expect "引数をそのまま渡す（空白を含むものも 1 つのまま）" bash -c 'tail -n 5 "$1" | diff - <(printf "%s\n" ai-jobs retry --kind photo_judge --all "a b" | tail -n 5) >/dev/null && grep -qx "a b" "$1"' _ "$MOCK_ARGS"
MOCK_RC=2 run sync
expect "geoframe の終了コードをそのまま返す（2）" test "$rc" = 2
MOCK_RC=1 run sync
expect "geoframe の終了コードをそのまま返す（1）" test "$rc" = 1

echo "[2] DATA_DIR の読み取り"
write_rs '# GEOFRAME_DATA_DIR=/commented/out' 'GEOFRAME_DATA_DIR=/first' 'GEOFRAME_DATA_DIR=/second/dir'
run status
expect "同じ名前が複数あれば後のものが勝つ（EnvironmentFile= と同じ）。コメント行は無視" grep -q 'DATA_DIR: /second/dir' "$T/err"
write_rs 'GEOFRAME_DATA_DIR="/quoted/dir"'
run status
expect "二重引用符は外して表示する" grep -q 'DATA_DIR: /quoted/dir（' "$T/err"
write_rs "GEOFRAME_DATA_DIR='/single/dir'"
run status
expect "一重引用符も外して表示する" grep -q 'DATA_DIR: /single/dir（' "$T/err"
write_rs 'TZ=Asia/Tokyo'
run status
expect "GEOFRAME_DATA_DIR が無ければ動かさない（終了コード2）" test "$rc" = 2 && expect "  systemd-run を呼ばない" bash -c '! test -s "$1"' _ "$MOCK_ARGS"
write_rs 'GEOFRAME_DATA_DIR=relative/dir'
run status
expect "相対パスなら動かさない（終了コード2）" test "$rc" = 2
write_rs 'GEOFRAME_DATA_DIR=/data;rm -rf /'
run status
expect "想定外の文字を含むなら動かさない（終了コード2）。値は出さない" bash -c 'test "$1" = 2 && ! grep -q "rm -rf" "$2"' _ "$rc" "$T/err"
write_rs 'GEOFRAME_DATA_DIR='
run status
expect "値が空なら動かさない（終了コード2）" test "$rc" = 2

echo "[3] 動かさない条件"
write_rs 'GEOFRAME_DATA_DIR=/srv/geoframe-data'
MOCK_UID=1000 run status
expect "root でなければ動かさない（終了コード2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
rm -f "$T/etc/geoframe-rs"; run status
expect "geoframe-rs の設定のファイルが無ければ動かさない（終了コード2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
write_rs 'GEOFRAME_DATA_DIR=/srv/geoframe-data'
rm -f "$T/bin/geoframe"; run status
expect "バイナリが無ければ動かさない（終了コード2）" bash -c 'test "$1" = 2 && ! test -s "$2"' _ "$rc" "$MOCK_ARGS"
printf '#!/bin/sh\n' > "$T/bin/geoframe"; chmod +x "$T/bin/geoframe"
run --data-dir /elsewhere ai-jobs status
expect "--data-dir を渡すと注意を出す（それでも渡す）" bash -c 'grep -q "注意" "$1" && grep -qx -e "--data-dir" "$2"' _ "$T/err" "$MOCK_ARGS"
run -h
expect "-h は使い方を出して終了コード0（systemd-run を呼ばない）" bash -c 'test "$1" = 0 && grep -q "geoframe-admin.sh" "$2" && ! test -s "$3"' _ "$rc" "$T/err" "$MOCK_ARGS"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
