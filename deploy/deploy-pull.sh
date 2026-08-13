#!/usr/bin/env bash
#
# deploy-pull.sh - 本番機の /opt/geoframe で origin/main を取り込み、コードを更新する
#
# ============================================================================
# 【更新前の注意】スキーマ変更を伴う更新の前には、必ず手動でバックアップを取ること
#
#   sudo -u geoframe /opt/geoframe/deploy/backup-catalog.sh
#
#   catalog.db は Vertex AI 課金済み embedding・手動GPS修正・お気に入りを含む
#   再構築不可の一次データ。定時バックアップ（geoframe-backup.timer）は6時間ごと
#   なので、最悪6時間ぶんの作業が巻き戻る。マイグレーションを含む更新では
#   「直前の世代」を必ず自分の手で作ってから流すこと。
# ============================================================================
#
# このスクリプトの存在意義（rsync 方式 update-app.sh の後継）:
#   旧 update-app.sh は Mac の作業ツリーから rsync -a で直接配布していたが、
#   geoframe:geoframe 所有の既存ファイルに対し非所有者ユーザーから rsync -a を
#   かけると、rsync がソース側パーミッションの再現（chmod）を試みて
#   "failed to set permissions" で全滅する（未解決のまま運用を阻害していた）。
#   さらに配られるのは「Mac の作業ツリーの現在の中身」であり、コミットしていない
#   変更やブランチ違いがそのまま本番へ紛れ込む（サイレントドリフト）。
#   本番に何が入っているかを後から説明する手段が無いのが最大の問題だった。
#
#   本スクリプトは、本番機自身が origin/main を取り込む方式に置き換える。
#   本番の状態は常にコミットSHAで一意に説明でき、ロールバックも
#   git reset --hard <SHA> で完結する。
#
# ============================================================================
# ファイルへの一律 chmod は行わない（rsync 方式との最大の違い・重要）
# ============================================================================
#   update-app.sh は転送後に「dir 755 / file 644 → *.sh に +x 付け直し」を
#   掛けていた。macOS 標準の rsync（openrsync）が --chmod 非対応で、Mac 側の
#   700 のディレクトリがそのまま渡ると geoframe ユーザーが CHDIR できず
#   status=200/CHDIR で無限再起動する事故があったためである。
#
#   git 方式ではこの後処理は不要かつ有害。git は core.fileMode の下で
#   インデックスに記録されたモード（100644 / 100755）を checkout 時に正しく
#   復元する。実際 deploy/*.sh は index 上 100755 で記録されている
#   （git ls-files -s deploy/ で確認できる）。ここで「file 644」を掛けると
#   *.sh に記録済みの実行ビットを潰してしまい、geoframe-backup.service が
#   203/EXEC で起動失敗する。しかもタイマー起動なので気付くのが6時間後になる。
#
#   よってこのスクリプトが正規化するのは **ディレクトリの 755 のみ**。
#   ファイルのモードは git に任せ、絶対に触らないこと。
#
# ============================================================================
# git gc / git prune はこのスクリプトから実行しない
# ============================================================================
#   force-push で origin から消えたコミットも、ローカルに到達不能オブジェクト
#   として残っている限り git reset --hard <SHA> で戻せる。gc はそれを回収して
#   しまい、ロールバック手段を失う。fetch が暗黙に呼ぶ自動 gc まで止めるには、
#   初回セットアップで `git config --local gc.auto 0` を設定しておくこと
#   （README §7 参照）。
#
# 実行場所: 本番機（N100）上で root として実行する。Mac では実行しない。
#           Mac からは deploy/deploy.sh（ssh + sudo の薄いラッパー）経由で呼ぶ。
#
set -euo pipefail

# --- 対象は固定。引数で切り替えられないようにする ---
# デプロイ先を引数で受けると、いつか typo で無関係なディレクトリを
# reset --hard する。運用先は1台1パスなので定数で固定する。
REPO_DIR="/opt/geoframe"
VENV_PIP="/opt/geoframe/app/.venv/bin/pip"
REQUIREMENTS="/opt/geoframe/app/requirements.txt"
SERVICE="geoframe-server"
HEALTH_URL="http://localhost:8080/api/admin/status"
HEALTH_TIMEOUT=30

# 適用前の HEAD。ロールバック手順の提示に使うため、失敗ハンドラからも
# 参照できるようトップレベルで初期化しておく（set -u 対策）。
OLD_HEAD=""
NEW_HEAD=""

log() { echo "[deploy-pull] $*" >&2; }
die() { echo "[deploy-pull] ERROR: $*" >&2; exit 1; }

# サービス停止後に失敗した場合、サービスは停止したままになる。
# 「何をすれば元に戻るか」をその場に出しておかないと、
# 暗い画面の前で手が止まる。復旧手順を必ず添えて落とす。
die_rollback() {
    log "ERROR: $*"
    log "------------------------------------------------------------------"
    log "サービスは停止したままの可能性がある。元に戻すには:"
    log "  cd ${REPO_DIR} && sudo git reset --hard ${OLD_HEAD:-<適用前のSHA>}"
    log "  sudo systemctl start ${SERVICE}"
    log "------------------------------------------------------------------"
    exit 1
}

usage() {
    cat <<'USAGE'
使い方: deploy-pull.sh [--dry-run] [-h|--help]

  --dry-run   何も変更せず、origin/main との差分だけを表示する。
              サービスの停止・起動、git reset --hard、pip install は行わない。

対象は固定で /opt/geoframe（引数では変えられない）。root で実行すること。
Mac からは deploy/deploy.sh --target user@host を使うこと。
USAGE
}

# 起動確認。
# systemctl start は Python がポートを bind し終える前に戻るため、
# 1回きりの確認ではなく最大 ${HEALTH_TIMEOUT} 秒ポーリングする。
# curl は本番機上で localhost 宛に実行するので、Host ヘッダ検証
# （GEOFRAME_ALLOWED_HOSTS / README §3）に確実に通る。
wait_for_health() {
    local code="" elapsed=0
    while [ "$elapsed" -lt "$HEALTH_TIMEOUT" ]; do
        code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "$HEALTH_URL" 2>/dev/null || true)"
        if [ "$code" = "200" ]; then
            log "health check: HTTP 200 (${elapsed}s)"
            return 0
        fi
        sleep 2
        elapsed=$((elapsed + 2))
    done
    log "HTTP 応答が 200 ではない（最後の値: '${code:-なし}'）"
    return 1
}

# ============================================================================
# 本体を main() に包んでいる理由（他の deploy/*.sh が平坦なのにここだけ関数化）
# ============================================================================
#   git reset --hard は、実行中のこのスクリプト自身
#   （/opt/geoframe/deploy/deploy-pull.sh）を差し替えうる。bash はスクリプトを
#   一度に全部読まず、必要になった時点で続きを読み進めるため、平坦に書くと
#   「途中で中身が入れ替わった別バージョンの続き」を実行しうる。
#   実際には git は新ファイルを作って rename するので実行中プロセスは古い inode を
#   掴んだままで済むが、サービス停止後の適用中に踏むと復旧が面倒なため、
#   main() 定義 + 末尾の main "$@" で「reset に到達する前に全文をパースし
#   終えている」ことを保証する。
# ============================================================================
main() {
    local dry_run=0
    local req_changed=0

    while [ $# -gt 0 ]; do
        case "$1" in
            --dry-run) dry_run=1; shift ;;
            -h|--help) usage; exit 0 ;;
            *) echo "ERROR: 不明な引数: $1" >&2; usage >&2; exit 2 ;;
        esac
    done

    # root 必須。systemctl stop/start と、root 所有の /opt/geoframe への
    # 書き込み（git reset --hard）に必要。dry-run でも git fetch が .git へ
    # 書き込むため同じく root で実行する。
    [ "$(id -u)" -eq 0 ] || die "root で実行すること（Mac からは deploy/deploy.sh を使う）。"

    [ -d "$REPO_DIR" ]      || die "デプロイ先が存在しない: ${REPO_DIR}"
    [ -d "$REPO_DIR/.git" ] || die "${REPO_DIR} が git リポジトリではない。README §1 の初回セットアップを実施すること。"

    # このスクリプトのセキュリティモデルは「/opt/geoframe が root:root 所有」に
    # 全面的に依存している。所有者がずれたまま root で git を叩くと、
    # .git/hooks/* や .git/config の core.sshCommand を非rootユーザーが
    # 書き換えておくだけで、その内容が root 権限で実行されうる。
    # 前提が崩れていないことを、サービスに触る前にここで実際に検証する。
    [ "$(stat -c '%u:%g' "$REPO_DIR/.git")" = "0:0" ] \
        || die "${REPO_DIR}/.git が root:root 所有ではない。sudo chown -R root:root ${REPO_DIR} を実行すること。"

    # venv の pip の存在もここで確認しておく。この確認が無いと、venv が欠けた機体で
    # requirements.txt に差分があった場合、サービスを停止して reset まで済ませた
    # 後に初めて pip install が落ちる。停止済みサービスの復旧がその分だけ遅れる。
    [ -x "$VENV_PIP" ] \
        || die "venv の pip が無い: ${VENV_PIP}（README §1 の初期セットアップを実施すること）"

    cd "$REPO_DIR" || die "cd できない: ${REPO_DIR}"

    [ "$dry_run" -eq 1 ] && log "*** DRY-RUN: 停止・適用・起動は一切行わない ***"

    # --- 1) 取得 ---
    log "git fetch origin ..."
    git fetch origin \
        || die "git fetch に失敗。Deploy Key と github.com への到達性を確認すること（README トラブルシュート）。"

    OLD_HEAD="$(git rev-parse HEAD)" \
        || die "HEAD を解決できない。初回セットアップが未完了の可能性がある（README §1.2）。"
    NEW_HEAD="$(git rev-parse origin/main)" \
        || die "origin/main を解決できない。remote の設定を確認すること（README §1.2）。"
    log "現在の HEAD : ${OLD_HEAD}"
    log "origin/main : ${NEW_HEAD}"

    # --- 2) 差分の表示 ---
    log "--- 取り込まれるコミット（git log HEAD..origin/main）---"
    git --no-pager log --oneline HEAD..origin/main || true
    log "--- 変更されるファイル（git diff --stat HEAD origin/main）---"
    git --no-pager diff --stat HEAD origin/main || true

    # --- 3) 変更なし判定 ---
    # 「git log HEAD..origin/main が空かどうか」では判定しない。
    # GitHub 側は squash merge / force-push 運用が続く前提なので、force-push で
    # コミットが落ちた場合 HEAD..origin/main が空でもツリーの中身は違いうる。
    # 「同じものが入っている」ことを証明できるのは SHA の一致だけ。
    if [ "$OLD_HEAD" = "$NEW_HEAD" ]; then
        log "変更なし（HEAD は既に origin/main と同一）。サービスには一切触れずに終了する。"
        exit 0
    fi

    # --- 4) dry-run はここまで ---
    if [ "$dry_run" -eq 1 ]; then
        log "(dry-run) skip: systemctl stop / git reset --hard / pip install / systemctl start"
        log "dry-run 完了。実際に適用するには --dry-run を外して再実行すること。"
        exit 0
    fi

    # --- 5) 依存の再インストールが要るかを reset の前に確定させる ---
    # reset 後は HEAD が origin/main と一致して比較の基準を失う。
    # （OLD_HEAD のオブジェクトは残るので後からでも比較できるが、
    #   判定を前に固めておくほうが失敗時に何が起きたかを読みやすい。）
    if [ -n "$(git diff --name-only "$OLD_HEAD" "$NEW_HEAD" -- app/requirements.txt)" ]; then
        req_changed=1
    fi

    # --- 6) サービス停止 ---
    # 稼働中に .py を差し替えると、更新途中の中途半端な状態でモジュールが
    # 読み込まれうる（Restart=always なのでクラッシュ→再起動で拾ってしまう）。
    log "stopping ${SERVICE} ..."
    systemctl stop "$SERVICE" || die "サービスを停止できない。適用は行っていない。"

    # ここから起動確認が終わるまでが「サービスが止まっている区間」。
    # trap が無いと、SSH 断（HUP）や操作者の Ctrl-C（INT）でスクリプトが即死し、
    # die_rollback が呼ばれないまま復旧コマンドも出ずにサービスが停止したまま
    # 放置される。しかもこの区間には pip install（Pillow の再ビルドで数分かかりうる）
    # が含まれるため、最も踏みやすい区間が最も長いという最悪の組み合わせになる。
    # die_rollback は自身が exit 1 するので、ハンドラは1回だけ走って終わる。
    trap 'die_rollback "シグナルにより中断された"' INT TERM HUP

    # --- 7) 適用 ---
    # pull --ff-only ではなく reset --hard を採用する理由:
    #   GitHub 側は squash merge / force-push 運用が続く前提で、履歴が線形に
    #   伸びるとは限らない。ff-only は force-push のたびに失敗して手作業を
    #   要求する。一方この機体はデプロイ専用でローカル編集を持たない
    #   （持ってはいけない）ため、「origin が常に正・手元の差分は捨てる」
    #   という運用と reset --hard の意味がそのまま一致する。
    #
    # origin/main ではなく fetch 時点で解決した SHA を渡す。上で表示した差分と
    # 実際に適用されるものが必ず一致することを保証するため。
    #
    # なお reset --hard が触るのは **追跡下のファイルだけ** で、未追跡の
    # app/.venv/・app/var/・.env・drive_key.json は残る（git clean は実行しない）。
    # 本番の実データは /var/lib/geoframe にあり、いずれも .gitignore 済みで
    # 追跡されていないことを確認済み。
    log "git reset --hard ${NEW_HEAD} ..."
    git reset --hard "$NEW_HEAD" || die_rollback "git reset --hard に失敗した。"

    # --- 8) ディレクトリ権限の正規化（ディレクトリのみ。冒頭の説明を参照）---
    # 新規追加されたディレクトリが umask 次第で 750 等になると、geoframe ユーザーが
    # WorkingDirectory=/opt/geoframe/app へ CHDIR できず status=200/CHDIR で
    # 無限再起動する。.git と app/.venv は prune して触らない
    # （.venv は本番機上で作った実行ビット付きバイナリを含む）。
    # REPO_DIR 自身も -type d に一致するので 755 になる。
    log "ディレクトリ権限を正規化（755 / .git・app/.venv は除外）..."
    find "$REPO_DIR" \
        -path "${REPO_DIR}/.git" -prune -o \
        -path "${REPO_DIR}/app/.venv" -prune -o \
        -type d -exec chmod 755 {} + \
        || die_rollback "ディレクトリ権限の正規化に失敗した。"

    # --- 9) 依存の再インストール（requirements.txt が変わったときだけ）---
    # .venv は未追跡なので reset --hard では作り直されない。
    # cd 先は /opt/geoframe で venv は app/ の下にあるため、相対パスで書かず
    # 必ず絶対パスで叩くこと。
    if [ "$req_changed" -eq 1 ]; then
        log "requirements.txt に変更あり。依存を再インストールする ..."
        "$VENV_PIP" install -r "$REQUIREMENTS" || die_rollback "pip install に失敗した。"
    else
        log "requirements.txt に変更なし。pip install はスキップする。"
    fi

    # --- 10) サービス起動 ---
    log "starting ${SERVICE} ..."
    systemctl start "$SERVICE" || die_rollback "サービスを起動できない。"

    # (a) systemd から見て active か
    if ! systemctl is-active --quiet "$SERVICE"; then
        log "起動確認に失敗: ${SERVICE} が active ではない。"
        systemctl status --no-pager -n 30 "$SERVICE" || true
        die_rollback "サービスが起動していない。上のログを確認すること。"
    fi
    log "systemctl is-active: ok"

    # (b) HTTP が実際に応答するか
    log "waiting for HTTP 200 at ${HEALTH_URL} (timeout ${HEALTH_TIMEOUT}s) ..."
    if ! wait_for_health; then
        journalctl -u "$SERVICE" -n 50 --no-pager || true
        die_rollback "適用は済んだがサーバーが正常応答していない。上のログを確認すること。"
    fi

    # ここまで来ればサービスは起動して正常応答している。
    # 以降でシグナルを受けても die_rollback（＝サービス停止中を前提とした復旧案内）は
    # もう妥当ではないので、既定の挙動に戻す。
    trap - INT TERM HUP

    log "更新完了: ${OLD_HEAD} -> ${NEW_HEAD}"
    log "この更新を取り消すには:"
    log "  cd ${REPO_DIR} && sudo git reset --hard ${OLD_HEAD} && sudo systemctl restart ${SERVICE}"
}

main "$@"
