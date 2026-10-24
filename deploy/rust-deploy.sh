#!/usr/bin/env bash
#
# rust-deploy.sh - 手元で Rust 版 geoframe をビルドし、検証環境へ配布して入れ替える
#
# ============================================================================
# 対象は **検証環境（lxc-geoframe）だけ**。本番の切替は P5（docs/rust-migration/plan.md）で、
# ユーザーの立ち会いのもとで行う。本番機（Python 版 geoframe-server が動いている機体）へは
# 実行しないこと。接続先に既定値は無い（--target か GEOFRAME_SSH_TARGET で必ず指定）。
#
# deploy.sh（Python 版。本番機で git を取り込む薄いラッパー）とは別物で、こちらはバイナリを
# 手元でビルドして送る。やること:
#   1. cargo build --release --target x86_64-unknown-linux-musl -p geoframe（完全静的）
#   2. バイナリと unit を転送（sha256 で照合）
#   3. 配置先のバイナリを .prev に残して入れ替え、unit を入れて daemon-reload・enable・restart
#   4. /healthz と /api/v1/photos を確認。失敗したら .prev へ戻す
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
使い方: rust-deploy.sh [--dry-run] [--skip-build] [--rollback] --target user@host [-h|--help]

  --target user@host  検証環境のSSH先（ssh と sudo が使えること）。
                      環境変数 GEOFRAME_SSH_TARGET でも指定可。既定値は無い。
  --dry-run           ビルド（--skip-build でなければ）と、入れ替えの手順の表示だけ。
                      転送・配置・再起動は行わない。
  --skip-build        ビルド済みの成果物（target/x86_64-unknown-linux-musl/release/geoframe）を使う。
  --rollback          配布せず、配置先の .prev（直前のバイナリ）へ手で戻す。
                      戻したあとに /healthz と /api/v1/photos を確認する。

環境変数（任意）:
  GEOFRAME_RS_BIN          配置先のバイナリ（既定 /usr/local/bin/geoframe）
  GEOFRAME_RS_CHECK_BASE   確認の URL（既定 http://127.0.0.1:8080。検証環境の中から叩く）
  GEOFRAME_RS_WAIT_SECS    再起動後に /healthz を待つ秒数（既定 30）
  GEOFRAME_RS_LOCAL_BIN    配るバイナリ（既定は musl ビルドの成果物。巻き戻しの試験用に差し替える。
                           --skip-build と合わせる。静的リンクの検査は通す）

配置先のバイナリは入れ替えの直前に <配置先>.prev へ残す（直前の1世代だけ）。
unit も同じく /etc/systemd/system/geoframe-rs.service.prev に残す。
確認に失敗したら自動で .prev へ戻して再起動し、終了コード 1 で終わる。

本番機には実行しないこと（本番の切替は P5）。
USAGE
}

log() { echo "[rust-deploy] $*" >&2; }
die() { echo "[rust-deploy] ERROR: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)    DRY_RUN=1; shift ;;
        --skip-build) DO_BUILD=0; shift ;;
        --rollback)   ROLLBACK=1; shift ;;
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

# リモートで実行する本体。引数で値を渡し、標準入力から bash -s で読ませる（クォートを1回で済ませる）。
#   $1: モード（deploy / rollback）  $2: 配置先  $3: 確認の URL  $4: 待つ秒数
#   $5: 一時ディレクトリ（deploy のみ。バイナリと unit が入っている）
read -r -d '' REMOTE_SCRIPT <<'REMOTE' || true
set -euo pipefail
mode="$1"; bin="$2"; base="$3"; wait_secs="$4"; stage="${5:-}"
unit_name="geoframe-rs.service"
unit_path="/etc/systemd/system/${unit_name}"

say() { echo "[remote] $*"; }

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

restart() {
    # reset-failed: RestartPreventExitStatus で止まった failed 状態を解いてから起動する
    systemctl reset-failed "$unit_name" 2>/dev/null || true
    systemctl restart "$unit_name"
}

# .prev から戻す。バイナリと（あれば）unit を戻して再起動する。
# 失敗した方のバイナリは調べられるように ${bin}.failed に残す。
restore_prev() {
    if [ ! -f "${bin}.prev" ]; then
        say "${bin}.prev が無いので戻せない（初回の配布）。サービスを止める"
        systemctl stop "$unit_name" 2>/dev/null || true
        return 1
    fi
    mv -f "$bin" "${bin}.failed"
    mv -f "${bin}.prev" "$bin"
    if [ -f "${unit_path}.prev" ]; then
        mv -f "${unit_path}.prev" "$unit_path"
        systemctl daemon-reload
    fi
    restart
}

case "$mode" in
deploy)
    [ -d "$stage" ] || { say "一時ディレクトリが無い"; exit 1; }
    install -m 0755 "${stage}/geoframe" "${bin}.new"
    if [ -f "$bin" ]; then
        cp -p "$bin" "${bin}.prev"
        say "直前のバイナリを ${bin}.prev に残した"
    else
        say "配置先にバイナリが無い（初回の配布）"
    fi
    mv -f "${bin}.new" "$bin"
    if [ -f "$unit_path" ]; then cp -p "$unit_path" "${unit_path}.prev"; fi
    install -m 0644 "${stage}/${unit_name}" "$unit_path"
    systemctl daemon-reload
    systemctl enable "$unit_name" >/dev/null 2>&1
    restart
    if check; then
        say "完了: $("$bin" --version)"
        exit 0
    fi
    say "確認に失敗した。直前の状態へ戻す"
    journalctl -u "$unit_name" -n 15 --no-pager -o cat 2>/dev/null | sed 's/^/[remote:journal] /' || true
    if restore_prev && check; then
        say "戻した（直前のバイナリで確認 OK）"
    else
        say "戻したあとの確認も失敗した。手で調べること"
    fi
    exit 1
    ;;
rollback)
    [ -f "${bin}.prev" ] || { say "${bin}.prev が無い"; exit 1; }
    restore_prev
    check
    say "戻した: $("$bin" --version)"
    ;;
*) say "不明なモード"; exit 2 ;;
esac
REMOTE

if [ "$ROLLBACK" -eq 1 ]; then
    log "モード  : rollback（${BIN_PATH}.prev へ戻す）"
    if [ "$DRY_RUN" -eq 1 ]; then
        log "*** DRY-RUN: ここで止める（ssh もしない）***"
        exit 0
    fi
    exec ssh -- "$SSH_TARGET" "sudo bash -s -- rollback '${BIN_PATH}' '${CHECK_BASE}' '${WAIT_SECS}'" <<<"$REMOTE_SCRIPT"
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
if [ "$DRY_RUN" -eq 1 ]; then
    log "*** DRY-RUN: 転送・配置・再起動は行わない ***"
    exit 0
fi

# --- 2. 転送（リモートの一時ディレクトリへ。sha256 を照合）---
log "転送    : 検証環境の一時ディレクトリへ（sha256 で照合）"
STAGE="$(ssh_run 'mktemp -d /tmp/geoframe-rs-deploy.XXXXXX')"
case "$STAGE" in /tmp/geoframe-rs-deploy.*) ;; *) die "リモートの一時ディレクトリが想定外: ${STAGE}" ;; esac
cleanup() { ssh_run "rm -rf '${STAGE}'" >/dev/null 2>&1 || true; }
trap cleanup EXIT
scp -q -- "$LOCAL_BIN" "${SSH_TARGET}:${STAGE}/geoframe"
scp -q -- "$LOCAL_UNIT" "${SSH_TARGET}:${STAGE}/${UNIT_NAME}"
REMOTE_SUM="$(ssh_run "sha256sum '${STAGE}/geoframe'" | cut -d' ' -f1)"
[ "$REMOTE_SUM" = "$SUM" ] || die "転送後の sha256 が一致しない"

# --- 3〜4. 入れ替え・確認・失敗時の巻き戻し（リモート）---
# -t は付けない（検証環境は NOPASSWD sudo。標準入力をスクリプトに使うため tty は割り当てない）。
rc=0
ssh -- "$SSH_TARGET" "sudo bash -s -- deploy '${BIN_PATH}' '${CHECK_BASE}' '${WAIT_SECS}' '${STAGE}'" <<<"$REMOTE_SCRIPT" || rc=$?
exit "$rc"
