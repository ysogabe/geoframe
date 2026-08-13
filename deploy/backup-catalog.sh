#!/usr/bin/env bash
#
# backup-catalog.sh - catalog.db の世代バックアップ（LXCコンテナ内で実行する）
#
# なぜこのスクリプトが要るか:
#   catalog.db は「再構築可能なキャッシュ」ではない一次データ。
#     - Vertex AI で課金して生成した embedding（6,290件）
#     - 管理画面から行った手動のGPS修正
#     - お気に入りフラグ
#   これらは失うと復元できない（README §2.1-1 参照）。
#
# なぜ cp ではなく sqlite3 .backup なのか:
#   catalog.db は WAL モードで運用している。`cp catalog.db` は
#   catalog.db-wal（まだ本体へチェックポイントされていないコミット分）を
#   取りこぼすため、「一見コピーできたが中身が古い/壊れている」バックアップに
#   なる。sqlite3 の .backup は WAL を含めた一貫スナップショットを作る。
#
# なぜ取得後に integrity_check するのか:
#   世代数を28で打ち切る設計上、壊れた世代を無検査で積むと
#   「壊れたバックアップが正常な世代を押し出す」事故が起きる。
#   整合性NGの世代は保存せずに破棄し、非ゼロ終了して systemd 側に失敗を残す。
#
# 実行ユーザー: geoframe（geoframe-backup.service 経由。手動実行時も
#   `sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh` を推奨）
#
set -euo pipefail

# --- 設定（環境変数で上書き可。既定値は本番構成に合わせてある）---
DATA_DIR="${GEOFRAME_DATA_DIR:-/var/lib/geoframe}"
DB="${GEOFRAME_CATALOG_DB:-${DATA_DIR}/catalog.db}"
BACKUP_DIR="${GEOFRAME_BACKUP_DIR:-/var/backups/geoframe}"

# 保持世代数。6時間ごと × 7日 = 28世代。
# ここで数えるのは catalog-*.db.gz のみで、latest.db.gz は対象外
# （latest は「最新世代へのポインタ」であって世代そのものではないため）。
KEEP_GENERATIONS="${GEOFRAME_BACKUP_KEEP:-28}"

log() { echo "[backup-catalog] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

# --- 事前チェック（壊れた前提で走り出さないためのガード）---
command -v sqlite3 >/dev/null 2>&1 \
    || die "sqlite3 コマンドが見つからない。コンテナ内で 'sudo apt install sqlite3' を実行すること。"
[ -f "$DB" ] \
    || die "catalog.db が見つからない: $DB （GEOFRAME_DATA_DIR の設定を確認すること）"
[ -r "$DB" ] \
    || die "catalog.db を読めない: $DB （実行ユーザー=$(id -un) が geoframe グループに属しているか確認すること）"
[ -d "$BACKUP_DIR" ] \
    || die "バックアップ先が無い: $BACKUP_DIR （'sudo install -d -o geoframe -g geoframe -m 0750 $BACKUP_DIR'）"
[ -w "$BACKUP_DIR" ] \
    || die "バックアップ先へ書き込めない: $BACKUP_DIR （所有者・モード0750と実行ユーザーを確認すること）"

TS="$(date +%Y%m%d-%H%M%S)"
DEST="${BACKUP_DIR}/catalog-${TS}.db.gz"

# 一時ファイルは必ず $BACKUP_DIR 配下（= 最終出力と同一ファイルシステム）に作る。
# /tmp に作ると最後の mv がファイルシステムを跨いだコピーになり、
# 「途中まで書けた .db.gz」が最終ファイル名で見えてしまう瞬間が生まれる。
# 同一FS上なら mv は rename(2) で原子的に完了する。
# 先頭を "." にしてあるのは catalog-*.db.gz のグロブに引っかけないため
# （世代カウント・prune の対象から外す）。
TMP="${BACKUP_DIR}/.tmp-catalog-${TS}.$$"

# 途中で落ちても中間物を残さない（整合性NGで die する経路も含めて必ず消える）。
trap 'rm -f -- "$TMP" "$TMP.gz"' EXIT

# --- 1) スナップショット取得 ---
# mode=ro: バックアップ処理が一次データを絶対に書き換えないためのガード。
#   （SQLite側でも読み取り専用として開かれるので、誤操作でVACUUM等が走らない）
# 注意: WALモードのDBを ro で開くには catalog.db-shm を開ける必要がある。
#   サーバー稼働中は geoframe ユーザーが -shm を保持しているため問題ないが、
#   サーバー停止中かつ -shm が消えている状態だとここで失敗することがある。
#   その場合はサーバーを起動してから再実行すること（下のメッセージ参照）。
log "snapshot: $DB -> $TMP"
if ! sqlite3 "file:${DB}?mode=ro" ".backup '${TMP}'"; then
    die "sqlite3 .backup に失敗した。サーバー停止中にWAL(-shm)が無いと read-only では開けないことがある。\
 'systemctl status geoframe-server' を確認し、稼働中に再実行すること。"
fi
[ -s "$TMP" ] || die "スナップショットが空。取得に失敗している: $TMP"

# --- 2) 整合性チェック（スナップショット側に対して実行する）---
# 検査対象は取得したスナップショットであって、稼働中の本体ではない。
# 出力が厳密に "ok" 一行の場合のみ合格とする。破損時は複数行のエラーが出るため、
# grep での部分一致判定は使わない（エラー文字列中の "ok" を誤って拾う）。
integrity="$(sqlite3 "$TMP" 'PRAGMA integrity_check;' 2>&1)" \
    || integrity="sqlite3 の実行自体が失敗: ${integrity}"
if [ "$integrity" != "ok" ]; then
    die "integrity_check が ok ではない。この世代は保存せず破棄する（既存の正常世代は温存される）: ${integrity}"
fi
log "integrity_check: ok"

# --- 3) 圧縮して最終ファイル名へ（原子的に公開）---
gzip -9 -c -- "$TMP" > "${TMP}.gz" || die "gzip に失敗した: ${TMP}"
rm -f -- "$TMP"
mv -f -- "${TMP}.gz" "$DEST"
chmod 0640 -- "$DEST"
trap - EXIT
log "saved: $DEST ($(stat -c %s -- "$DEST" 2>/dev/null || echo '?') bytes)"

# --- 4) latest.db.gz の更新 ---
# シンボリックリンクではなくハードリンクにしている。
# 理由: 回収側（backup-pull.sh）が `lxc file pull` を使うが、
#   LXD の file pull はシンボリックリンクを「リンクとして」扱い、
#   実体を取得できないことがある。ハードリンクなら通常ファイルとして読める。
# 追加の利点: 世代 prune で catalog-*.db.gz 側の名前が消えても、
#   latest.db.gz が同じ inode を参照し続けるので中身は失われない。
LATEST_TMP="${BACKUP_DIR}/.latest-${TS}.tmp"
if ! ln -f -- "$DEST" "$LATEST_TMP" 2>/dev/null; then
    # 何らかの理由でハードリンクが張れない場合はコピーで代替する
    cp -f -- "$DEST" "$LATEST_TMP" || die "latest.db.gz の作成に失敗した"
fi
mv -f -- "$LATEST_TMP" "${BACKUP_DIR}/latest.db.gz"
log "updated: ${BACKUP_DIR}/latest.db.gz"

# --- 5) 古い世代の削除 ---
# グロブを catalog-*.db.gz に限定することで、latest.db.gz と
# .tmp-* / .latest-*.tmp を構造的に prune 対象外にしている
# （名前でのexclude判定を書かずに済ませるのが目的）。
old_generations=()
if compgen -G "${BACKUP_DIR}/catalog-*.db.gz" >/dev/null; then
    mapfile -t old_generations < <(
        ls -1t -- "${BACKUP_DIR}"/catalog-*.db.gz | tail -n +$((KEEP_GENERATIONS + 1))
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
