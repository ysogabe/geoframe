# 公開準備チェックリスト

リポジトリをPublicにしたときの確認の記録です。2026-10-30に全履歴を監査し、所有者が下の判断をしたうえで公開しました。

秘密の値、鍵の接頭辞、実在の個人情報は、この文書にも書きません。

## 全履歴の監査（2026-10-30）

部分cloneでは取得できていないオブジェクトがあったため、`git clone --mirror`で全refs（ブランチ、タグ`display-final`、
GitHubの`refs/pull/*` 53件）を取り直し、到達できるすべてのblobを走査しました。

| 項目 | 結果 |
|---|---|
| 範囲 | 477コミット、PRの参照53件、タグ1件。未取得のオブジェクトなし |
| 鍵・トークン（AWS、Google、GitHub、Slack、OpenAI・Anthropic、サービスアカウントのJSON、JWT） | 実際の鍵は無し。以前「鍵の接頭辞に一致」とした箇所は、旧`app/static/navara/`の縮小済みJSに埋め込まれたWASMのbase64の中の偶然の一致だった |
| 秘密鍵のマーカー | テストの固定データ（`jwt-test-pkcs*.b64`など、試験専用の鍵）と、不正入力の試験だけ |
| 秘密の値の代入 | 空の値の例（`.env`の雛形）と、テストの中の明らかな偽物だけ |
| プライベートIP | SSRF対策の試験の入力と例のほかに、旧版の`docs/rust-migration/plan.md`にLANのIPが3件（**履歴にだけ残る**。現在のファイルには無い） |
| 写真・画像 | 同梱の画像は合成の試験画像（EXIFなし）、合成のHEIC、透明タイル、紹介ページのイメージ図だけ。実在の写真は無い |
| 位置 | 座標は駅などの公開の地点、格子の試験値、公開データの一覧。自宅の座標は無い。旧Python版の既定の天気予報区（都道府県単位）が履歴に残る |
| コミットの作者 | GitHubのnoreplyアドレスのほか、所有者の個人のメールアドレスと勤務先のメールアドレス、機体名を含むアドレスが履歴に残る |
| Issue（14件）・PR（52件）の本文とコメント | 鍵・メールアドレス・自宅の情報は無し。機体名（`gpu-eval`など）と、例のIPの説明が1件 |
| Actions | 実行441回。Secrets・Variablesは未使用で、アーティファクトは0件 |
| Releases・Webhook | どちらも0件 |
| Deploy key | 本番機用の読み取り専用の鍵が1つ（公開後もそのまま使う） |

### 履歴を書き換えなかった理由

GitHubの`refs/pull/*`は所有者でも消せないため、履歴を書き換えても、PRの参照から古いコミットを読めます。
消すには別のリポジトリを作り直すしかなく、IssueやPRの記録とリンクを失います。上の走査で鍵は見つからず、
残るのは機体名やLANのIP、作者のメールアドレスなど外から届かない情報だったため、今の履歴のまま公開しました。

## 所有者の判断（2026-10-30）

- **履歴**: 書き換えずに、今の履歴のまま公開する。
- **旅程の例**: `web/travel_blog/plans/uzbekistan-silk-road-2025.json`（2025年の実際の旅の日程と訪問地。宿・便名・同行者は無い）は、そのまま公開する。
- **機体名・コンテナ名**: `docs/rust-migration/`や`deploy/`に残る機体名（`geoframe-n100`、`lxc-geoframe`、`gpu-eval`など）は、そのままにする。[文書の決まり](README.md)は実ホスト名をコミットしないとしているが、すでに書かれた移行の記録は例外として残す。新しい文書には書かない。IPアドレスは現在のファイルに無い。
- **撮影地の手がかりの検証データ**: `tools/loc_clue_eval/labels.json`（写真のID・撮影日時・ファイル名と手がかりの有無。画像と位置は無い）は、そのまま公開する。

## 以前に解消した事項

- 同梱のサードパーティ資産のライセンス本文（MapLibre GL JS v4.7.1のBSD-3-Clause、Share Tech MonoのSIL OFL 1.1）を
  `web/vendor/licenses/`に同梱した（出典・版・取得日は[`web/vendor/README.md`](../web/vendor/README.md)）。
- READMEの外部通信・課金・認証なしの説明と、文書の状態を現状に合わせた。
- 紹介ページ（`site/`）と、手動起動だけのGitHub Pages用workflowを追加した。
- リポジトリ名を`geoframe-ambient-display`から`geoframe`に変えた。

## 公開のときの確認

- [x] 未解消の事項に、所有者が結論を出した
- [x] 履歴を公開してよいか（書き換えるか、現在の履歴のまま公開するか）を決めた
- [x] 全refsとタグを対象に、鍵・個人情報・位置情報の走査を、未取得のオブジェクトを取得した状態でやり直した
- [x] Issue・PR・Actionsのログとアーティファクト・Releasesを確認した
- [x] 旅程のサンプルの扱いを決めた
- [x] README・使い方（`docs/USAGE.md`）・LPの説明（認証なし、外部通信と課金、写真とGPSの扱い）が実装と合っている
- [x] `python3 tools/check_markdown_links.py`と`python3 tools/check_site.py`が通る
- [ ] リポジトリをPublicにした
- [ ] Private vulnerability reporting、secret scanning、push protection を有効にした
- [ ] GitHub Pages（Source = GitHub Actions）を有効にし、`pages.yml`を手動起動して表示を確かめた

公開のあとに見直すこと: ブランチの保護（`main`）と、Actionsの権限・外部からのPRの承認の設定。
