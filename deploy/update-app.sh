#!/usr/bin/env bash
#
# update-app.sh - Mac のリポジトリから本番機へコードを配布する
#
# ============================================================================
# 【更新前の注意】スキーマ変更を伴う更新の前には、必ず手動でバックアップを取ること
#
#   ssh operator@geoframe.example.test \
#     'sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh'
#
#   catalog.db は Vertex AI 課金済み embedding・手動GPS修正・お気に入りを含む
#   再構築不可の一次データ。定時バックアップ（geoframe-backup.timer）は6時間ごと
#   なので、最悪6時間ぶんの作業が巻き戻る。マイグレーションを含む更新では
#   「直前の世代」を必ず自分の手で作ってから流すこと。
# ============================================================================
#
# このスクリプトの存在意義:
#   rsync の --exclude を手で打つと、いつか必ず打ち漏らす。特に var/ の
#   除外漏れは --delete と組み合わさって **本番データの全消し** に直結する
#   （catalog.db / photos/ / derivatives/ が消える）。
#   除外リストとパーミッション指定をスクリプト内の定数として固定し、
#   人間が毎回タイプしないようにするのが目的。
#
# 実行場所: Mac（このリポジトリ内）。コンテナ側では実行しない。
#
set -euo pipefail

# --- 配布先（--target または GEOFRAME_SSH_TARGET で必ず指定）---
SSH_TARGET="${GEOFRAME_SSH_TARGET:-}"
REMOTE_ROOT="${GEOFRAME_REMOTE_ROOT:-/opt/geoframe}"
SERVICE="geoframe-server"
HEALTH_URL="http://localhost:8080/api/admin/status"
HEALTH_TIMEOUT=30

# ============================================================================
# rsync の除外リスト（**この定数以外の場所に --exclude を書かないこと**）
# ============================================================================
# var/            … データルート（GEOFRAME_DATA_DIR の既定値 = app/var/）。
#                   本番の実データは /var/lib/geoframe にあるが、Mac側 app/var/ を
#                   配ってしまうと開発機のDBで本番を上書きしかねない。
#                   加えて --delete との組み合わせでの誤爆が最も怖いのがここ。
# .venv/          … Python 仮想環境。中身はプラットフォーム依存のバイナリで、
#                   macOS のものを Linux へ配ると import 時に即死する。
#                   コンテナ側 /opt/geoframe/app/.venv は消してはならない。
# __pycache__/    … .pyc は Python が再生成する。配る意味が無いうえ
#                   古いキャッシュを持ち込むと混乱の元。
# .env            … 秘密。本番の設定は /etc/default/geoframe が正（README §2.3）。
# drive_key.json  … Google Drive サービスアカウント鍵。本番は
#                   /var/lib/geoframe/drive_key.json が正（600）。
# .DS_Store       … macOS のゴミ。
RSYNC_EXCLUDES=(
    --exclude 'var/'
    --exclude '.venv/'
    --exclude '__pycache__/'
    --exclude '.env'
    --exclude 'drive_key.json'
    --exclude '.DS_Store'
)

# ============================================================================
# 転送後の権限正規化は必須（省略してはいけない）
# ============================================================================
# rsync -a はソース側のパーミッションをそのまま転送先へ保存する。
# Mac 側のリポジトリの app/ は 700（drwx------）になっており、これがそのまま
# コンテナへ渡ると geoframe ユーザーが WorkingDirectory=/opt/geoframe/app へ
# CHDIR できなくなる。結果、systemd が status=200/CHDIR で起動失敗し、
# Restart=always と相まって無限再起動ループに入る（実際に踏んだ事故）。
#
# rsync の --chmod で転送時に上書きするのが素直だが、**この経路は使えない**:
# macOS 標準の rsync は openrsync（protocol 29 / rsync 2.6.9 互換）であり
# --chmod を実装していないため、指定すると "invalid argument" で即座に落ちる。
# そこで転送後に SSH 側で chmod をかけて正規化する。
#
# .venv は rsync 対象外（除外リスト参照）なので prune して触らない。
# コンテナ内で構築した実行ビット付きのバイナリ・スクリプトを壊さないため。
normalize_perms() {
    local dir="$1"
    run_remote "find ${dir} -path ${dir}/.venv -prune -o -type d -exec chmod 755 {} +"
    run_remote "find ${dir} -path ${dir}/.venv -prune -o -type f -exec chmod 644 {} +"
    run_remote "chmod 755 ${dir}"
}

DRY_RUN=0

usage() {
    cat <<'USAGE'
使い方: update-app.sh [--dry-run] [--target user@host] [-h|--help]

  --dry-run          何も変更せず、rsync が転送する予定の差分だけを表示する。
                     サービスの停止・起動・権限修正も行わない。
  --target user@host 配布先のSSH先を指定。
                     環境変数 GEOFRAME_SSH_TARGET でも指定可。

配布対象は固定で、このリポジトリの app/ と deploy/ の2つ。
除外リストはスクリプト内の定数（RSYNC_EXCLUDES）で固定されている。
手で --exclude を足さないこと。権限は転送後に SSH 側の chmod で正規化する。
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --target)  [ $# -ge 2 ] || { echo "ERROR: --target には値が必要" >&2; exit 2; }
                   SSH_TARGET="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: 不明な引数: $1" >&2; usage >&2; exit 2 ;;
    esac
done

log() { echo "[update-app] $*" >&2; }
die() { echo "[update-app] ERROR: $*" >&2; exit 1; }

[ -n "$SSH_TARGET" ] || { usage >&2; die "--target user@host または GEOFRAME_SSH_TARGET の指定が必要。"; }

# --- 配布元の確認（リポジトリルートはこのスクリプトの位置から求める）---
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -d "${REPO_ROOT}/app" ]    || die "配布元が見つからない: ${REPO_ROOT}/app"
[ -d "${REPO_ROOT}/deploy" ] || die "配布元が見つからない: ${REPO_ROOT}/deploy"
# server.py の存在で「本当に app/ か」を確認する（別ディレクトリを指していた場合の保険）
[ -f "${REPO_ROOT}/app/server.py" ] || die "${REPO_ROOT}/app に server.py が無い。リポジトリの構成を確認すること。"

log "repo   : ${REPO_ROOT}"
log "target : ${SSH_TARGET}:${REMOTE_ROOT}"
[ "$DRY_RUN" -eq 1 ] && log "*** DRY-RUN: 転送も停止/起動も行わない ***"

# ssh 経由でリモートコマンドを実行するヘルパ。
# sudo が要るものは -t（tty割り当て）を付けてパスワードプロンプトを通す。
run_remote()      { ssh    "$SSH_TARGET" "$@"; }
run_remote_sudo() { ssh -t "$SSH_TARGET" "$@"; }

# --- 0) 疎通確認 ---
run_remote 'true' >/dev/null 2>&1 || die "SSH で ${SSH_TARGET} に接続できない。"

# --- 1) サービス停止 ---
# 稼働中に .py を差し替えると、更新途中の中途半端な状態でモジュールが
# 読み込まれうる（Restart=always なのでクラッシュ→再起動で拾ってしまう）。
if [ "$DRY_RUN" -eq 0 ]; then
    log "stopping ${SERVICE} ..."
    run_remote_sudo "sudo systemctl stop ${SERVICE}"
else
    log "(dry-run) skip: sudo systemctl stop ${SERVICE}"
fi

# --- 2) rsync ---
# ソース・宛先の**両方**に末尾スラッシュを付けること。
# 付け忘れると /opt/geoframe/app/app/ のような入れ子が生まれる。
# app/ と deploy/ はそれぞれ別呼び出しにする（--delete の効果範囲を
# 各ディレクトリ内に閉じ込め、/opt/geoframe 直下へ波及させないため）。
RSYNC_OPTS=(-a --delete --human-readable --itemize-changes "${RSYNC_EXCLUDES[@]}")
[ "$DRY_RUN" -eq 1 ] && RSYNC_OPTS+=(--dry-run --verbose)

log "rsync: app/ -> ${REMOTE_ROOT}/app/"
rsync "${RSYNC_OPTS[@]}" "${REPO_ROOT}/app/"    "${SSH_TARGET}:${REMOTE_ROOT}/app/"

log "rsync: deploy/ -> ${REMOTE_ROOT}/deploy/"
rsync "${RSYNC_OPTS[@]}" "${REPO_ROOT}/deploy/" "${SSH_TARGET}:${REMOTE_ROOT}/deploy/"

# --- 3) 権限正規化 ---
# dir 755 / file 644 に揃えてから、シェルスクリプトへ実行ビットを付け直す。
# **順序が重要**: 先に +x を付けてから 644 で潰すと実行ビットが消える。
# backup-catalog.sh は geoframe-backup.service の ExecStart= から起動される
# ため、実行ビットが無いと 203/EXEC で失敗する。しかもタイマー起動なので
# 気付くのが6時間後になる。
if [ "$DRY_RUN" -eq 0 ]; then
    log "fixing permissions (dir 755 / file 644, then +x on *.sh) ..."
    normalize_perms "${REMOTE_ROOT}/app"
    normalize_perms "${REMOTE_ROOT}/deploy"
    run_remote "find ${REMOTE_ROOT}/deploy -type f -name '*.sh' -exec chmod +x {} +"
else
    log "(dry-run) skip: 権限正規化（dir 755 / file 644 / *.sh に +x）"
fi

# --- 4) サービス起動 ---
if [ "$DRY_RUN" -eq 1 ]; then
    log "(dry-run) skip: sudo systemctl start ${SERVICE} / 起動確認"
    log "dry-run 完了。実際に配布するには --dry-run を外して再実行すること。"
    exit 0
fi

log "starting ${SERVICE} ..."
run_remote_sudo "sudo systemctl start ${SERVICE}"

# --- 5) 起動確認 ---
# (a) systemd から見て active か
if ! run_remote "systemctl is-active --quiet ${SERVICE}"; then
    log "起動確認に失敗: ${SERVICE} が active ではない。"
    run_remote "systemctl status --no-pager -n 30 ${SERVICE}" || true
    die "サービスが起動していない。上のログを確認すること。"
fi
log "systemctl is-active: ok"

# (b) HTTP が実際に応答するか。
#     curl は **コンテナ内で** 実行する。localhost 宛にすることで
#     Host ヘッダ検証（README §6）に確実に通る。
#     systemctl start は Python がポートを bind し終える前に戻るため、
#     1回きりの確認ではなく最大 ${HEALTH_TIMEOUT} 秒ポーリングする。
log "waiting for HTTP 200 at ${HEALTH_URL} (timeout ${HEALTH_TIMEOUT}s) ..."
code=""
elapsed=0
while [ "$elapsed" -lt "$HEALTH_TIMEOUT" ]; do
    code="$(run_remote "curl -s -o /dev/null -w '%{http_code}' --max-time 3 ${HEALTH_URL}" 2>/dev/null || true)"
    [ "$code" = "200" ] && break
    sleep 2
    elapsed=$((elapsed + 2))
done

if [ "$code" != "200" ]; then
    log "HTTP 応答が 200 ではない（最後の値: '${code:-なし}'）"
    run_remote "journalctl -u ${SERVICE} -n 50 --no-pager" || true
    die "更新は転送済みだがサーバーが正常応答していない。上のログを確認すること。"
fi

log "health check: HTTP 200 (${elapsed}s)"
log "更新完了。"
