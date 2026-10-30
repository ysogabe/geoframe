# 紹介ページ（LP）の管理

`site/`は、GEOFRAMEを紹介する静的なページです。素のHTMLとCSSだけで、JavaScript、外部のフォント、CDN、
アクセス解析は使いません。GitHub Pagesで公開しています（公開したときの確認は[公開準備チェックリスト](public-release-checklist.md)）。

- URL: <https://ysogabe.github.io/geoframe/>
- ソース: [`site/`](../site/)（`index.html`、`styles.css`、`favicon.svg`、`images/`）
- 公開用のworkflow: [`.github/workflows/pages.yml`](../.github/workflows/pages.yml)（手動起動だけ）

## 更新の手順

pushではページは更新されません。`site/`の変更を`main`へマージしたあと、所有者が手動で公開します。

1. Actions タブで「紹介ページ（GitHub Pages）」を選び、**Run workflow**で手動起動する。
   workflowは`site/`を検査（`tools/check_site.py`）してから、`site/`だけを公開します。
2. 完了後、上のURLを開いて表示を確かめる。

Pages の Source は「GitHub Actions」です。公開を止めたいときは、Settings → Pages で公開を取り消します。

## 公開するのは`site/`だけ

workflowが公開するのは`site/`の中の静的ファイルだけです。リポジトリの他の場所（`web/`の旅程JSON、`docs/`、
`deploy/`など）は公開されません。`site/`には次のものを**入れません**。

- 実在の写真、旅程、位置情報、`catalog.db`、書き出した旅行ブログ
- 私的なサーバー名・IPアドレス、本番のURL、APIやデータの参照、秘密情報
- 外部から読み込む資産（フォント、スクリプト、画像、アクセス解析のタグ）

ページ内の「説明図」は、機能の関係を表す図で、実際の画面ではありません。画面の画像を載せるときは、
合成した写真か、所有者が公開に同意した写真だけを使い、実際の画面であることを明記してください。

## ローカルでの確認と編集

ビルドは要りません。`site/`を静的サーバーで配ります。

```sh
python3 tools/check_site.py            # リンク・fragment・資産・外部ホスト・データファイルの検査
python3 -m unittest discover -s tools/tests   # 検査ツールの試験
python3 -m http.server 8000 --directory site  # 手元で表示（http://127.0.0.1:8000/）
```

`tools/check_site.py`は、`site/`の内部リンクの参照先と`#`のid、資産の存在を確かめ、外部のホストが
ナビゲーションの許可リスト（`github.com`・`ysogabe.github.io`・`rustup.rs`）だけであること、
画像やstylesheetなどの外部資産を読み込まないこと、`<script>`・`<iframe>`・`<form>`が無いこと、
`site/`にJSONやデータベースなどのファイルが無いことを確かめます。CIの「文書・道具」jobでも実行されます。
許可するホストを増やすときは、`ALLOWED_HOSTS`を変えて理由をPRに書いてください。

文言を変えるときは、READMEと使い方（`docs/USAGE.md`）と食い違わないようにしてください。特に、**認証が無いこと、外部へ通信する機能
（取り込み時の地名・標高補完、Google Drive・地図・クラウドAI）と課金、写真とGPSの扱い**は、実装に合わせて書きます
（[README](../README.md)、[使い方](USAGE.md)、[設定リファレンス](reference/configuration.md)）。
