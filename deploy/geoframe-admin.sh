#!/usr/bin/env bash
#
# geoframe-admin.sh - geoframe の CLI を、サービス（geoframe-rs.service）と同じユーザー・同じ設定で動かす
#
# 使い方（root で。sudo を付ける）:
#   sudo /opt/geoframe/deploy/geoframe-admin.sh ai-jobs status
#   sudo systemctl stop geoframe-rs && sudo /opt/geoframe/deploy/geoframe-admin.sh ai-jobs retry --kind photo_judge
#   （sync・organize・quality-scan は、通常は管理画面の運用タブから行う（sync・quality-scan・大量削除の保留の解除）。
#    CLI は serve が動いていないときだけ使う。書き込み系のコマンド ai-jobs retry|release-pause|deactivate-space は、
#    サービスを止めてから動かす。稼働中の serve が .ingest.lock を持っているので、動いていると断られる。
#    ai-jobs status は読むだけなので、サービスを止めなくてよい）
#
# なぜ要るか: geoframe を root で直接動かすと、作られるファイル（catalog.db-wal・.cache.json など）が root の
# 所有になり、サービス（User=geoframe）が書けなくなる。バイナリは root での書き込みを断る（終了コード 2）。
# このラッパーは systemd-run で一時的なサービスを作り、User=geoframe・サービスと同じ設定のファイル
# （/etc/default/geoframe → /etc/default/geoframe-rs の順。後のものが勝つ）・UMask=0027・photos/ を読み取り専用、
# で動かす。unit とすべての CLI が同じ設定のファイルを読むので、同じ DATA_DIR を相手にする。
#
# 動かす前に、対象の DATA_DIR（パスだけ。設定のほかの値は出さない）を標準エラー出力へ出す。
# 終了コードは geoframe の終了コード（systemd-run --wait が返す）。
#
set -euo pipefail

ENV_COMMON="/etc/default/geoframe"
ENV_RS="/etc/default/geoframe-rs"
BIN="/usr/local/bin/geoframe"
RUN_USER="geoframe"

say() { echo "[geoframe-admin] $*" >&2; }
die() { echo "[geoframe-admin] ERROR: $*" >&2; exit 2; }

case "${1:-}" in
    -h|--help)
        sed -n '3,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
        exit 0 ;;
esac

# 0600 root:root の設定のファイルを読めるのは root（と systemd）だけ。systemd-run --uid も root が要る
[ "$(id -u)" = 0 ] || die "root で実行すること（sudo を付ける）"
[ -f "$ENV_RS" ] && [ -r "$ENV_RS" ] || die "設定のファイル（${ENV_RS}）を読めない。Rust 版の設定が置かれていない機体では使えない"
[ -x "$BIN" ] || die "geoframe のバイナリが無い（${BIN}）"
command -v systemd-run >/dev/null 2>&1 || die "systemd-run が無い"

# DATA_DIR（パスだけ）。後に書いた行が勝つ（EnvironmentFile= と同じ）。引用符は外す。値の形が想定外なら出さない
raw="$(sed -n 's/^[[:space:]]*GEOFRAME_DATA_DIR=//p' "$ENV_RS" | tail -n 1)"
raw="${raw%"${raw##*[![:space:]]}"}"
case "$raw" in
    \"*\") raw="${raw#\"}"; raw="${raw%\"}" ;;
    \'*\') raw="${raw#\'}"; raw="${raw%\'}" ;;
esac
[ -n "$raw" ] || die "${ENV_RS} に GEOFRAME_DATA_DIR が無い。サービスと同じ DATA_DIR を相手にできないので動かさない"
case "$raw" in
    /*[!A-Za-z0-9_./-]*|"") die "${ENV_RS} の GEOFRAME_DATA_DIR が、絶対パス（英数字と _ . / - だけ）ではない。動かさない" ;;
    /*) ;;
    *) die "${ENV_RS} の GEOFRAME_DATA_DIR が絶対パスではない。動かさない" ;;
esac
say "対象の DATA_DIR: ${raw}（ユーザー ${RUN_USER}、設定 ${ENV_COMMON} → ${ENV_RS}）"
for a in "$@"; do
    case "$a" in
        --data-dir|--data-dir=*) say "注意: --data-dir が指定されている。上の DATA_DIR とは別の場所を相手にする" ;;
    esac
done

exec systemd-run --uid="$RUN_USER" --wait --pipe --collect \
    -p EnvironmentFile=-"$ENV_COMMON" \
    -p EnvironmentFile="$ENV_RS" \
    -p UMask=0027 \
    -p ReadOnlyPaths=-/var/lib/geoframe/photos \
    "$BIN" "$@"
