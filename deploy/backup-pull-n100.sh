#!/usr/bin/env bash
#
# backup-pull-n100.sh - リモートホストのcatalog.dbバックアップを
#   日次で回収する（SSH版、旧ファイル名を維持）
#
# ============================================================================
# backup-pull.sh（LXC専用、`lxc file pull`版）の後継。
#
# pull型にしているのは、push型だとリモートホストが万一侵害された
# 場合にバックアップ先ごと汚染されうるため。pull型ならリモート側は
# 回収先への書き込み手段を持たない。
#
# 認証は「latest.db.gzの読み取りだけ」に制限した専用鍵
# （例: ~/.ssh/geoframe_backup_pull）を使う。リモート側のauthorized_keysで
#   command="cat /var/backups/geoframe/latest.db.gz",no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty
# を指定してあるため、この鍵で接続すると常にこの1コマンドだけが実行され、
# 任意コマンド実行はできない（鍵が漏れてもバックアップ閲覧以上のことはできない）。
#
# リモート側の /var/backups/geoframe は geoframe:geoframe 0750 を想定する。
# 接続ユーザーには、必要最小限の読み取り権限だけを与える。
# ============================================================================
#
# 実行場所: バックアップ回収先ホスト。リモート側では実行しない。
#
set -euo pipefail

SSH_TARGET="${GEOFRAME_BACKUP_SSH_TARGET:-${GEOFRAME_N100_SSH_TARGET:-}}"
SSH_KEY="${GEOFRAME_BACKUP_SSH_KEY:-${GEOFRAME_N100_SSH_KEY:-$HOME/.ssh/geoframe_backup_pull}}"
DEST_DIR="${GEOFRAME_BACKUP_PULL_DIR:-$HOME/backups/geoframe}"

# 保持世代数。日次 × 2週間 = 14世代。backup-pull.sh(LXC版)と揃えてある。
KEEP_GENERATIONS="${GEOFRAME_BACKUP_PULL_KEEP:-14}"

log() { echo "[backup-pull-n100] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

[ -r "$SSH_KEY" ] || die "SSH鍵が見つからない: ${SSH_KEY}"
[ -n "$SSH_TARGET" ] || die "GEOFRAME_BACKUP_SSH_TARGET=user@host の指定が必要"

mkdir -p -- "$DEST_DIR" || die "回収先を作成できない: ${DEST_DIR}"
[ -w "$DEST_DIR" ] || die "回収先へ書き込めない: ${DEST_DIR}"

TS="$(date +%Y%m%d-%H%M%S)"
DEST="${DEST_DIR}/catalog-${TS}.db.gz"
# 一時ファイルは回収先と同一ファイルシステム上に置く（mv を原子的にするため）。
# 先頭 "." で catalog-*.db.gz のグロブから外し、世代カウント・prune の対象にしない。
TMP="${DEST_DIR}/.tmp-pull-${TS}.$$"

trap 'rm -f -- "$TMP"' EXIT

# --- 1) 回収 ---
# 接続先コマンドはリモート側のauthorized_keysで固定されているため、ここで
# 何を指定しても無視される（command=制限）。catの標準出力をそのままリダイレクトする。
log "リモートホストから取得: ${SSH_TARGET} -> ${TMP}"
if ! ssh -o BatchMode=yes -o ConnectTimeout=10 -i "$SSH_KEY" "$SSH_TARGET" > "$TMP"; then
    die "取得に失敗した。リモート側でgeoframe-backup.timerが動いているか確認すること。"
fi
[ -s "$TMP" ] || die "回収したファイルが空: ${TMP}"

# --- 2) 健全性確認 ---
# gzipコンテナ(CRC含む)の健全性のみを検証する。中のSQLite DB自体の整合性は、
# リモート側のbackup-catalog.shが取得直後にPRAGMA integrity_checkで既に検証済み
# （このスクリプトでsqlite3を要求しないためにも、この分担のまま維持すること）。
if ! gzip -t -- "$TMP"; then
    die "gzip -tに失敗（転送中に壊れた可能性）。この世代は保存しない: ${TMP}"
fi
log "gzip -t: ok"

# --- 3) 確定 ---
mv -f -- "$TMP" "$DEST"
chmod 0640 -- "$DEST"
trap - EXIT
log "saved: ${DEST} ($(stat -c %s -- "$DEST" 2>/dev/null || echo '?') bytes)"

# --- 4) 古い世代の削除 ---
old_generations=()
if compgen -G "${DEST_DIR}/catalog-*.db.gz" >/dev/null; then
    mapfile -t old_generations < <(
        ls -1t -- "${DEST_DIR}"/catalog-*.db.gz | tail -n +$((KEEP_GENERATIONS + 1))
    )
fi
if [ "${#old_generations[@]}" -gt 0 ]; then
    for f in "${old_generations[@]}"; do
        rm -f -- "$f"
        log "prune: $f"
    done
    log "prune: ${#old_generations[@]} 世代を削除（保持=${KEEP_GENERATIONS}）"
else
    log "prune: 削除対象なし（保持=${KEEP_GENERATIONS}）"
fi

log "done."

# ============================================================================
# セットアップ手順（回収先側。1回だけ）
# ============================================================================
#   1. 専用鍵を作成:
#        ssh-keygen -t ed25519 -N '' -C 'geoframe-backup-pull' \
#            -f ~/.ssh/geoframe_backup_pull
#   2. リモート側 ~/.ssh/authorized_keys に、公開鍵を command制限付きで追加:
#        command="cat /var/backups/geoframe/latest.db.gz",no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty <公開鍵>
#   3. 接続ユーザーにバックアップの読み取り権限を与える。
#   4. 回収先のknown_hostsへリモートのホスト鍵を登録:
#        ssh-keyscan -H geoframe.example.test >> ~/.ssh/known_hosts
#
# ============================================================================
# cron 設定例（回収先側の crontab -e に記述）
# ============================================================================
#   # GEOFRAME: リモートからcatalog.dbを日次で回収する
#   37 3 * * * GEOFRAME_BACKUP_SSH_TARGET=operator@geoframe.example.test /srv/scripts/backup-pull-n100.sh >> /var/log/geoframe-backup-pull.log 2>&1
# ============================================================================
