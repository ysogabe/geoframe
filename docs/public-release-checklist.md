# 公開準備チェックリスト

リポジトリをPublicにする前に確認する事項です。**2026-10-30時点では、Publicにしてよいという結論は出ていません。**
この文書の「確認済み」は、ローカルで行った部分的な検査の結果で、全履歴の監査ではありません。
リポジトリの公開とGitHub Pagesの有効化は、所有者が判断して行います（紹介ページは[LPの管理](LP.md)）。

秘密の値、鍵の接頭辞、実在の個人情報は、この文書にも書きません。

## 2026-10-30の準備状況

### 確認した範囲

| 項目 | 結果 |
|---|---|
| リポジトリの公開範囲 | Private（変更していない） |
| 追跡ファイル | 323ファイル（この変更の前） |
| 履歴の取得状況 | 浅いcloneではない。ただし**promisor付きのpartial clone**で、全履歴が手元にあるわけではない |
| 履歴の走査 | 全refsで1,340コミット。取得済みのオブジェクト6,511件、うちblob 2,433件を走査。**未取得のオブジェクトが5件あり、全履歴の完全な監査ではない** |
| 現在の追跡ファイル（テキスト） | GitHub・Google・AWSの鍵の既知パターンは検出なし |
| 秘密鍵のマーカー | 見つかった箇所は、テストの固定データの参照と不正入力の試験。実際の鍵ではない |
| プライベートIPのアドレス | SSRF対策の試験の入力。`/home/user`は合成の例 |
| テスト用の鍵 | `jwt-test-pkcs*.b64`は**試験専用**の鍵で、本番では使わない。内容は鍵の形式の試験にだけ使う |

### 履歴の鍵の接頭辞の一致

履歴のbundleで、鍵の接頭辞に一致する箇所がありました。base64のdata URI（画像など）の中の文字列が
たまたま一致した**疑陽性の可能性が高い**ですが、確認できていません。実際の鍵の漏えいとは断定できず、
安全とも断定できません。公開の前に、所有者が一致箇所を個別に確かめてください。

### この変更で解消した事項

- 同梱のサードパーティ資産のライセンス本文が足りなかった。MapLibre GL JS v4.7.1のBSD-3-Clause本文と、
  Share Tech MonoのSIL Open Font License 1.1の本文を、公式の配布元から取得して`web/vendor/licenses/`に同梱した
  （出典・版・取得日は[`web/vendor/README.md`](../web/vendor/README.md)。取得したままで改変なし）。
- READMEの外部通信・課金・認証なしの説明、文書の状態（Rust案が「計画」のまま、`web/vendor/README.md`が旧Python実装を指すなど）を現状に合わせた。
- 紹介ページ（`site/`）と、手動起動だけのGitHub Pages用workflowを追加した（公開はしていない）。

## 未解消の事項（公開の前に所有者が決める）

### 履歴に残る内部環境の情報

履歴には、すでに削除した旧`deploy/kiosk`、`deploy/agents`、`docs/rust-migration/plan`の旧版などの内部環境
（ホスト名、構成、運用の手順）が残っています。**ファイルを削除するだけでは、Publicにしたとき履歴から読めます。**

- 履歴の書き換え（`git filter-repo`など）はこの変更では行いません。書き換える場合は、全refs・タグ（`display-final`を含む）・
  既存のcloneやフォークへの影響を踏まえて所有者が判断します。
- 現在の追跡ファイルにも、検証環境のコンテナ名（`lxc-geoframe`など）や、機体名・内部の常駐サービス名
  （`geoframe-n100`、`gpu-eval`）が、`docs/rust-migration/`と`deploy/`、`docs/architecture/photo-pipeline.md`などに残っています。
  公開してよい粒度か、伏せるか、所有者が決めます。
- 履歴だけでなく、Issue、Pull Requestとその説明・コメント、Actionsのログとアーティファクト、Releases、タグ、
  Wiki・Projectsは、Public化とともに読めるようになります。これらにも内部情報や写真・位置情報が無いかを確認してください。
- `deploy/README.md`の「リポジトリはPrivateのため」（Deploy Keyの節）は、Public化後の運用に合わせて見直す。

### 同梱データ

- `web/travel_blog/plans/uzbekistan-silk-road-2025.json`は、**日付つきの実在の旅程**を含みます。公開してよいという
  所有者の同意を得るか、架空のサンプルに差し替えるか、決めてください。**この変更では、内容の転載も削除もしていません。**
- ほかに、実在の写真、位置情報、`catalog.db`、書き出した旅行ブログが追跡されていないことを、公開の直前に再確認する。

### GitHubの設定（未操作・未確認）

次は、この変更では操作も確認もしていません。公開の前後に所有者が設定します。

- リポジトリのPublic化
- GitHub Pagesの有効化（Source = GitHub Actions）と、[`pages.yml`](../.github/workflows/pages.yml)の手動起動
- Private vulnerability reporting（[SECURITY.md](../SECURITY.md)が前提にしている）
- Secret scanning と push protection
- ブランチの保護（`main`）と、Actionsの権限・承認の設定

## 公開直前の確認

- [ ] 上の未解消の事項に、所有者が結論を出した
- [ ] 履歴を公開してよいか（書き換えるか、現在の履歴のまま公開するか）を決めた
- [ ] 全refsとタグを対象に、鍵・個人情報・位置情報の走査を、未取得のオブジェクトを取得した状態でやり直した
- [ ] Issue・PR・Actionsのログとアーティファクト・Releasesを確認した
- [ ] 旅程のサンプルの扱いを決めた
- [ ] Private vulnerability reporting と secret scanning を有効にした
- [ ] README・LPの説明（認証なし、外部通信と課金、写真とGPSの扱い）が実装と合っている
- [ ] `python3 tools/check_markdown_links.py`と`python3 tools/check_site.py`が通る
