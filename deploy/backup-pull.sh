#!/usr/bin/env bash
#
# backup-pull.sh - コンテナ内のバックアップを LXC ホスト側へ日次で回収する
#
# ============================================================================
# なぜ scp / rsync over SSH ではなく `lxc file pull` なのか
# ============================================================================
# 対象コンテナのネットワークは macvlan で構成されている。
# macvlan は「親インターフェースを共有する子IFどうしの通信」を、
# 親ホスト自身との間だけ遮断する（LAN上の他機からは到達できるのに、
# 親ホストからだけ届かない、という一見不可解な状態になる）。
# したがって親ホストからコンテナへの ssh / curl / rsync は成立しない。
#
# 一方 `lxc exec` / `lxc file pull` は LXD のUnixソケット経由でコンテナの
# 名前空間に直接入るため、ネットワーク経路を一切使わない = macvlan の
# 制約を受けない。ホスト⇔コンテナ間のファイル授受は必ずこちらを使うこと。
# ============================================================================
#
# 前提: このスクリプトを実行するユーザーが lxd グループに属していること
#       （属していない場合は sudo 付きで実行する。cron 例は末尾参照）
#
# 実行場所: LXCホスト。コンテナ内では実行しない。
#
set -euo pipefail

CONTAINER="${GEOFRAME_CONTAINER:-geoframe}"
# コンテナ内のソース。backup-catalog.sh が更新している「最新世代」。
# （latest.db.gz はハードリンクの実ファイル。lxc file pull はシンボリックリンクを
#   実体として取れないことがあるため、コンテナ側で実ファイルにしてある）
REMOTE_LATEST="/var/backups/geoframe/latest.db.gz"
DEST_DIR="${GEOFRAME_BACKUP_PULL_DIR:-/srv/backups/geoframe}"

# lxc は絶対パスで解決する。
# LXD は snap 版で実体は /usr/sbin/lxc（snap のラッパー）だが、
# **cron の既定 PATH（/usr/bin:/bin）には含まれない**。対話シェルでは
# 通るのに cron からだけ command not found で落ちる、という一番気付きにくい
# 壊れ方をするため、PATH に依存せず解決する。
LXC_BIN="${LXC_BIN:-}"
if [ -z "$LXC_BIN" ]; then
    for candidate in /usr/sbin/lxc /snap/bin/lxc /usr/bin/lxc; do
        [ -x "$candidate" ] && { LXC_BIN="$candidate"; break; }
    done
fi
[ -n "$LXC_BIN" ] && [ -x "$LXC_BIN" ] \
    || { echo "[backup-pull] ERROR: lxc コマンドが見つからない（LXC_BIN で明示指定できる）" >&2; exit 1; }

# 保持世代数。日次 × 2週間 = 14世代。
# 数えるのは catalog-*.db.gz のみ（他のファイルは対象外）。
KEEP_GENERATIONS="${GEOFRAME_BACKUP_PULL_KEEP:-14}"

log() { echo "[backup-pull] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

# lxc の実在確認は冒頭の LXC_BIN 解決で済んでいる（PATH に依存しない）。

# 回収先が無いと lxc file pull は分かりにくいエラーで落ちるので先に作る
mkdir -p -- "$DEST_DIR" || die "回収先を作成できない: ${DEST_DIR}"
[ -w "$DEST_DIR" ] || die "回収先へ書き込めない: ${DEST_DIR}"

# コンテナが動いていなければ pull できない。先に明示的に確認する。
if ! "$LXC_BIN" info "$CONTAINER" >/dev/null 2>&1; then
    die "コンテナが見つからない/参照できない: ${CONTAINER} （lxd グループ所属か、sudo 実行かを確認すること）"
fi

TS="$(date +%Y%m%d-%H%M%S)"
DEST="${DEST_DIR}/catalog-${TS}.db.gz"
# 一時ファイルは回収先と同一ファイルシステム上に置く（mv を原子的にするため）。
# 先頭 "." で catalog-*.db.gz のグロブから外し、世代カウント・prune の対象にしない。
TMP="${DEST_DIR}/.tmp-pull-${TS}.$$"

trap 'rm -f -- "$TMP"' EXIT

# --- 1) 回収 ---
log "lxc file pull ${CONTAINER}${REMOTE_LATEST} -> ${TMP}"
if ! "$LXC_BIN" file pull "${CONTAINER}${REMOTE_LATEST}" "$TMP"; then
    die "lxc file pull に失敗した。コンテナ側で backup-catalog.sh が動いているか確認すること（systemctl status geoframe-backup.timer）。"
fi
[ -s "$TMP" ] || die "回収したファイルが空: ${TMP}"

# --- 2) 健全性確認 ---
# gzip -t は gzip コンテナ（CRC含む）の健全性のみを検証する。
# 中の SQLite DB そのものの整合性は、**コンテナ側の backup-catalog.sh が
# 取得直後に PRAGMA integrity_check で既に検証済み**。
# したがってここで展開して sqlite3 を掛け直す必要はない
# （LXCホストに sqlite3 を要求しないためにも、この分担のまま維持すること）。
# ここで見たいのは「転送経路でファイルが壊れていないか」だけ。
if ! gzip -t -- "$TMP"; then
    die "gzip -t に失敗（転送中に壊れた可能性）。この世代は保存しない: ${TMP}"
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
# cron 設定例（LXCホスト側。lxd グループに属するユーザーの crontab -e に記述）
# ============================================================================
#   # GEOFRAME: catalog.db を日次で LXC ホスト側へ回収する
#   # 分を :00 ちょうどから外しているのは、logrotate や他の定時ジョブと
#   # 発火が重なるのを避けるため。
#   # 拾うのは常に「その時点の latest.db.gz」= コンテナ側の直近世代。
#   # なお cron はこのLXCホストのタイムゾーンで、コンテナ側の
#   # OnCalendar= はコンテナのシステムタイムゾーンで評価されるため、
#   # 両者が揃っていないと拾う世代が数時間前後する。日次1本を残すという
#   # 要件には影響しないので、揃っていなくても問題ない。
#   23 4 * * * /srv/scripts/backup-pull.sh >> /var/log/geoframe-backup-pull.log 2>&1
#
# root の crontab で動かす場合（lxd グループに入れたくない場合）は
#   23 4 * * * /srv/scripts/backup-pull.sh >> /var/log/geoframe-backup-pull.log 2>&1
# を /etc/cron.d/geoframe-backup-pull に置き、ユーザー欄に root を指定する:
#   23 4 * * * root /srv/scripts/backup-pull.sh >> /var/log/geoframe-backup-pull.log 2>&1
# ============================================================================
