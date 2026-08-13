#!/usr/bin/env bash
#
# deploy.sh - Mac から本番機へ「origin/main の取り込み」を指示する薄いラッパー
#
# ============================================================================
# このスクリプトは意図的に **ロジックを持たない**
# ============================================================================
#   実体は本番機上の /opt/geoframe/deploy/deploy-pull.sh で、こちらは
#   ssh + sudo でそれを呼ぶだけ。両側にロジックを分けて持つと、Mac 側だけ
#   更新されて本番側が古いまま、という食い違いが起きる。それは旧 rsync 方式
#   （update-app.sh）で問題だった「Mac の作業ツリーの状態が本番の状態を決める」
#   構図の再来になる。
#
#   本番へ入るコードは常に origin/main であり、この薄いラッパーが多少古くても
#   結果は変わらない、という性質を保つのが目的。実際の適用ロジックを変更したい
#   ときは deploy-pull.sh を直し、それを push して本番へ取り込ませること
#   （deploy-pull.sh 自身も git 管理下なので、次回の適用で更新される）。
# ============================================================================
#
# 実行場所: Mac（このリポジトリ内）。本番機側では実行しない。
#
set -euo pipefail

# --- 配布先（--target または GEOFRAME_SSH_TARGET で必ず指定）---
SSH_TARGET="${GEOFRAME_SSH_TARGET:-}"
REMOTE_SCRIPT="/opt/geoframe/deploy/deploy-pull.sh"

DRY_RUN=0

usage() {
    cat <<'USAGE'
使い方: deploy.sh [--dry-run] [--target user@host] [-h|--help]

  --dry-run          本番機側で差分の表示だけを行う。
                     サービスの停止・起動、git reset --hard、pip install は行わない。
  --target user@host 本番機のSSH先を指定。
                     環境変数 GEOFRAME_SSH_TARGET でも指定可。

このスクリプトはロジックを持たず、本番機上の
/opt/geoframe/deploy/deploy-pull.sh を sudo で呼ぶだけ。
本番へ入るコードは常に origin/main であり、Mac の作業ツリーの状態は影響しない。

必ず最初に --dry-run で何が入るかを確認すること。
DBスキーマを変更する更新の前には、必ず手動バックアップを取得すること。
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

log() { echo "[deploy] $*" >&2; }
die() { echo "[deploy] ERROR: $*" >&2; exit 1; }

[ -n "$SSH_TARGET" ] || { usage >&2; die "--target user@host または GEOFRAME_SSH_TARGET の指定が必要。"; }

# リモートへ渡す引数は、ローカルのシェルと ssh 先のシェルで2回解釈される。
# "$@" をそのまま ssh のコマンド文字列へ埋め込むと再分割・クォート崩れを起こす。
# 受け付けるフラグは --dry-run の1つだけなので、ここで判定してから固定文字列を
# 組み立てる（将来フラグが増えたら、同じ形でここに素直に足すこと）。
REMOTE_ARGS=""
[ "$DRY_RUN" -eq 1 ] && REMOTE_ARGS="--dry-run"

log "target : ${SSH_TARGET}"
log "remote : sudo ${REMOTE_SCRIPT} ${REMOTE_ARGS}"
[ "$DRY_RUN" -eq 1 ] && log "*** DRY-RUN: 本番機側で停止・適用・起動は行われない ***"

# -t（tty割り当て）は sudo のパスワードプロンプトを通すために必要。
# exec で置き換え、リモート側の終了コードをそのままこのスクリプトの
# 終了コードにする（CI やシェルの && 連結から使えるようにするため）。
exec ssh -t "$SSH_TARGET" "sudo ${REMOTE_SCRIPT} ${REMOTE_ARGS}"
