#!/usr/bin/env bash
#
# deploy-pull-test.sh - deploy-pull.sh の「Rust 版へ切り替えたあとは断る」検査を、モックの systemctl で試験する
#
# 実機のサービスにも /opt/geoframe にも触れない。deploy-pull.sh を一時ディレクトリへ写し、固定の定数
# （checkout の固定の印・デプロイ先）だけを一時ディレクトリの中へ差し替えて動かす。systemctl は PATH の
# モックで、呼ばれた引数を記録する。この検査を通った場合は、その先（root の確認・デプロイ先の確認）で別の
# 理由（終了コード 1）で止まる: 試験は root でも非 root でも、サービスの停止・git・pip まで進まない。
#
# 実行: bash deploy/deploy-pull-test.sh
#
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$(mktemp -d /tmp/deploy-pull-test.XXXXXX)"
trap 'rm -rf "$T"' EXIT

pass=0 failn=0
ok()   { pass=$((pass + 1)); echo "  ok   $*"; }
ng()   { failn=$((failn + 1)); echo "  FAIL $*"; }
expect() { local d="$1"; shift; if "$@"; then ok "$d"; else ng "$d"; fi; }

mkdir -p "$T/mock" "$T/etc"
cat > "$T/mock/systemctl" <<'M'
#!/usr/bin/env bash
echo "$*" >> "$MOCK_CALLS"
if [ "${1:-}" = show ]; then
    case "${MOCK_SHOW:-}" in
        fail)  echo "Failed to connect to bus" >&2; exit 1 ;;
        empty) exit 0 ;;
        *)     printf '%s\n' "$MOCK_SHOW"; exit 0 ;;
    esac
fi
exit 0
M
chmod +x "$T/mock/systemctl"
export PATH="$T/mock:$PATH" MOCK_CALLS="$T/calls.log"

# 試験用の写し: 固定の印とデプロイ先だけを差し替える（本物の /opt/geoframe は指さない）
sed -e "s#^PIN_FILE=.*#PIN_FILE=\"$T/etc/checkout-pinned\"#" \
    -e "s#^REPO_DIR=.*#REPO_DIR=\"$T/no-such-checkout\"#" \
    "$HERE/deploy-pull.sh" > "$T/deploy-pull.sh"
expect "試験の準備: 差し替えた写しが元と違うのは 2 行だけ" bash -c 'test "$(diff <(cat "$1") <(cat "$2") | grep -c "^>")" = 2' _ "$HERE/deploy-pull.sh" "$T/deploy-pull.sh"

run() { # run <引数...> → 出力は $T/out、終了コードは $rc
    : > "$MOCK_CALLS"
    bash "$T/deploy-pull.sh" "$@" > "$T/out" 2>&1
    rc=$?
}
state() { # state <LoadState> <UnitFileState> <ActiveState> → MOCK_SHOW
    MOCK_SHOW="$(printf 'ActiveState=%s\nLoadState=%s\nUnitFileState=%s' "$3" "$1" "$2")"; export MOCK_SHOW
}
reached_next_check() { test "$rc" = 1 && grep -q -e 'root で実行すること' -e 'デプロイ先が存在しない' "$T/out"; }
only_show_calls() { ! grep -qv '^show ' "$MOCK_CALLS"; }

echo "[1] 断る"
rm -f "$T/etc/checkout-pinned"
touch "$T/etc/checkout-pinned"; state not-found '' inactive; run
expect "固定の印があれば断る（終了コード3）" test "$rc" = 3
expect "固定の印の理由を表示する" grep -q '固定の印' "$T/out"
expect "固定の印で断るとき systemctl に触れない" test ! -s "$MOCK_CALLS"
run --dry-run
expect "固定の印は --dry-run でも断る（終了コード3）" test "$rc" = 3
rm -f "$T/etc/checkout-pinned"; ln -s "$T/etc/no-such-target" "$T/etc/checkout-pinned"; run
expect "固定の印が壊れたリンクでも断る（終了コード3）" test "$rc" = 3
rm -f "$T/etc/checkout-pinned"
for spec in "loaded enabled inactive" "loaded enabled-runtime inactive" "loaded disabled active" "loaded disabled activating" \
            "loaded disabled reloading" "loaded disabled deactivating" "loaded disabled maintenance" "masked masked inactive" \
            "masked '' inactive" "loaded masked-runtime inactive" "loaded enabled active"; do
    eval "set -- $spec"; state "$1" "$2" "$3"; run
    expect "geoframe-rs が ${1}/${2:-なし}/${3} なら断る（終了コード3）" test "$rc" = 3
    expect "  ${1}/${2:-なし}/${3}: 断る理由を表示し、systemctl は show だけ" bash -c 'grep -q "拒否" "$1" && ! grep -qv "^show " "$2"' _ "$T/out" "$MOCK_CALLS"
done
MOCK_SHOW=fail; run
expect "状態を取得できなければ断る（終了コード3）" test "$rc" = 3
MOCK_SHOW=empty; run
expect "状態の出力が空なら断る（終了コード3）" test "$rc" = 3
MOCK_SHOW='UnitFileState=disabled'; run
expect "LoadState・ActiveState の行が無ければ断る（終了コード3）" test "$rc" = 3
expect "どの理由でも値（パス以外）を出さず、終了コード3の旨を表示する" grep -q '終了コード 3' "$T/out"
# UnitFileState の欠け・空・未知の値（導入済みの unit では、有効でないと確認できなければ断る）
MOCK_SHOW="$(printf 'LoadState=loaded\nActiveState=inactive')"; run
expect "導入済みで UnitFileState の行が無ければ断る（終了コード3）" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=not-found\nActiveState=inactive')"; run
expect "未導入でも UnitFileState の行が無ければ断る（3 行そろわない）" test "$rc" = 3
for fs in '' weird bad linked alias enabled-runtime; do
    state loaded "$fs" inactive; run
    expect "導入済みで UnitFileState が '${fs:-空}' なら断る（終了コード3）" bash -c 'test "$1" = 3 && grep -q "UnitFileState\|enabled" "$2"' _ "$rc" "$T/out"
done
state not-found enabled inactive; run
expect "未導入（not-found）なのに UnitFileState が空でなければ断る（終了コード3）" test "$rc" = 3
# 重複した行（最後の行で上書きして『止まっている』と取り違えない）
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=disabled\nActiveState=inactive\nActiveState=active')"; run
expect "ActiveState が inactive のあとに active（矛盾する重複）なら断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=disabled\nActiveState=active\nActiveState=inactive')"; run
expect "ActiveState が active のあとに inactive（矛盾する重複）なら断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=disabled\nActiveState=inactive\nActiveState=inactive')"; run
expect "ActiveState が同じ値で 2 行でも断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=enabled\nUnitFileState=disabled\nActiveState=inactive')"; run
expect "UnitFileState が enabled のあとに disabled（矛盾する重複）なら断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=disabled\nUnitFileState=disabled\nActiveState=inactive')"; run
expect "UnitFileState が同じ値で 2 行でも断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=masked\nLoadState=loaded\nUnitFileState=disabled\nActiveState=inactive')"; run
expect "LoadState が masked のあとに loaded（矛盾する重複）なら断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nLoadState=loaded\nUnitFileState=disabled\nActiveState=inactive')"; run
expect "LoadState が同じ値で 2 行でも断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=loaded\nUnitFileState=disabled\nActiveState=')"; run
expect "ActiveState の値が空なら断る" test "$rc" = 3
MOCK_SHOW="$(printf 'LoadState=\nUnitFileState=disabled\nActiveState=inactive')"; run
expect "LoadState の値が空なら断る" test "$rc" = 3

echo "[2] 通す（その先の確認へ進む。サービスには触れない）"
state not-found '' inactive; run
expect "geoframe-rs が未導入（not-found/inactive）なら、この検査は通す" reached_next_check
expect "  通ったとき systemctl は show だけ" only_show_calls
state loaded disabled inactive; run
expect "geoframe-rs が disabled/inactive（unit はあるが使っていない）なら通す" reached_next_check
state loaded disabled failed; run
expect "geoframe-rs が disabled/failed なら通す" reached_next_check
state loaded static inactive; run
expect "geoframe-rs が static/inactive なら通す" reached_next_check
for fs in indirect generated transient; do
    state loaded "$fs" inactive; run
    expect "geoframe-rs が ${fs}/inactive なら通す" reached_next_check
done

echo "[3] ヘルプは検査より前"
touch "$T/etc/checkout-pinned"; run -h
expect "-h は固定の印があっても使い方を出して終了コード0" bash -c 'test "$1" = 0 && grep -q "使い方" "$2" && grep -q "終了コード 3" "$2"' _ "$rc" "$T/out"
expect "不明な引数は終了コード2" bash -c 'bash "$1" --bogus >/dev/null 2>&1; test $? = 2' _ "$T/deploy-pull.sh"

echo
echo "結果: ok ${pass} / FAIL ${failn}"
[ "$failn" -eq 0 ]
