#!/usr/bin/env bash
#
# 日本語フォント（Noto Sans JP、静的 Medium ウェイト）を取得する。
#
# フォント本体はバイナリなのでリポジトリにコミットしない（.gitignore 済み）。
# 代わりにこのスクリプトで取得し直せるようにしてある。
#
# ライセンス: SIL Open Font License 1.1
#   https://github.com/google/fonts/blob/main/ofl/notosansjp/OFL.txt
#   （google/fonts リポジトリの ofl/ 以下＝Open Font License 区画にある family。
#     下記で取得する静的インスタンスも同じ family の同じライセンスのデータで、
#     Google Fonts 側でフォーマット/ウェイトを変換して配信しているだけ）
#
# --- なぜ google/fonts の raw ファイルを直接取らないか -----------------------
# google/fonts リポジトリの ofl/notosansjp/ には可変フォント
# （NotoSansJP[wght].ttf、wght 軸あり）しか置いていない。SDL_ttf 2.24 は
# 可変軸を指定する API を持たないため、これをそのまま開くと既定インスタンスの
# Thin（最も細いウェイト）になってしまう（実機で確認済み、ログに style=Thin
# と出る）。数メートル離れて読むアンビエント端末には細すぎる。
#
# そこで Google Fonts の CSS2 配信 API に特定ウェイトを指定してリクエストし、
# Google 側でインスタンス化済みの「本物の静的フォント」（wght 軸を持つ fvar
# テーブルが無い、単一ウェイトの通常の TrueType）を取得する。取得したファイルを
# 実際に検査して fvar が無いことを確認済み（本スクリプトを書いた時点、
# v56, NotoSansJP-Medium.ttf, 約 5.3MB）。
#
# 【重要】User-Agent をブラウザっぽい文字列に変えないこと。
# Google Fonts の CSS2 API はリクエストの User-Agent でレスポンス形式を
# 出し分けている。ブラウザ UA だと WOFF2 を Unicode 範囲ごとに分割した
# 十数個の @font-face が返る（Web では正しい挙動だが、単一ファイルを
# TTF_OpenFontIndex() で開きたいこちらの用途には合わない）。curl の既定 UA
# （ブラウザとして認識されない UA）だと、全グリフを含む単一の .ttf への
# url() が1つだけ返ってくる。これを意図して使っているので、-A/--user-agent
# でブラウザ UA を渡さないこと。
#
# 冪等: 既にファイルがあれば何もしない。強制的に取り直したい場合は --force。

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
FONT_DIR="${SCRIPT_DIR}/../assets/fonts"
FONT_PATH="${FONT_DIR}/NotoSansJP.ttf"

# Google Fonts CSS2 API。wght@500 = Medium 相当の静的インスタンスを要求する。
# ここで返る src: url(...) は Google 側で都度採番される gstatic 上の
# バージョン付き URL なので、直接ハードコードせず毎回このエンドポイントから
# 解決する（フォントが更新されても追従できるように）。
CSS_URL="https://fonts.googleapis.com/css2?family=Noto+Sans+JP:wght@500&display=swap"

# 期待される最小サイズ。これを下回ったら取得失敗（HTML エラーページ等）とみなす。
MIN_BYTES=1000000

force=0
if [[ "${1:-}" == "--force" ]]; then
  force=1
fi

if [[ -f "${FONT_PATH}" && "${force}" -eq 0 ]]; then
  size=$(wc -c < "${FONT_PATH}" | tr -d ' ')
  echo "フォントは取得済みです: ${FONT_PATH} (${size} バイト)"
  echo "取り直す場合は: $0 --force"
  exit 0
fi

mkdir -p "${FONT_DIR}"

echo "Noto Sans JP (静的 Medium) を取得します..."
echo "  css : ${CSS_URL}"
echo "  to  : ${FONT_PATH}"

# 1. CSS2 API から @font-face 定義を取得し、実フォントの URL を抜き出す。
#    ブラウザ UA を送らないこと（スクリプト冒頭のコメント参照）。
css="$(curl --fail --location --show-error --silent \
            --connect-timeout 20 --max-time 60 \
            "${CSS_URL}")"

font_url="$(printf '%s' "${css}" | grep -oE 'https://fonts\.gstatic\.com/[^)]+\.ttf' | head -n 1 || true)"

if [[ -z "${font_url}" ]]; then
  echo "エラー: CSS2 レスポンスから .ttf の URL を抽出できませんでした。" >&2
  echo "  Google Fonts 側の応答形式が変わった可能性があります。" >&2
  echo "  レスポンス先頭:" >&2
  printf '%s\n' "${css}" | head -c 500 >&2
  echo >&2
  exit 1
fi

echo "  font: ${font_url}"

tmp_file="$(mktemp "${FONT_DIR}/.NotoSansJP.XXXXXX")"
trap 'rm -f "${tmp_file}"' EXIT

# --location でリダイレクトを追う。--fail で HTTP エラー時に非ゼロ終了。
curl --fail --location --show-error --silent \
     --connect-timeout 20 --max-time 300 \
     --output "${tmp_file}" \
     "${font_url}"

actual_bytes=$(wc -c < "${tmp_file}" | tr -d ' ')
if [[ "${actual_bytes}" -lt "${MIN_BYTES}" ]]; then
  echo "エラー: 取得したファイルが小さすぎます (${actual_bytes} バイト)。" >&2
  echo "  URL が変わったか、エラーページを掴んでいる可能性があります。" >&2
  exit 1
fi

# TrueType/OpenType のマジックナンバーを確認する。
magic="$(head -c 4 "${tmp_file}" | od -An -tx1 | tr -d ' \n')"
case "${magic}" in
  00010000|74727565|4f54544f|74746366) ;;  # ttf / true / OTTO / ttcf
  *)
    echo "エラー: フォントファイルに見えません (先頭4バイト: ${magic})。" >&2
    exit 1
    ;;
esac

mv "${tmp_file}" "${FONT_PATH}"
trap - EXIT

echo "完了: ${FONT_PATH} (${actual_bytes} バイト)"
