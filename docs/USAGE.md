# 使い方

GEOFRAMEのビルド、起動、外部との通信、設定とデータの扱い、試験の回し方をまとめます。
概要は[README](../README.md)、本番の配布は[デプロイガイド](../deploy/README.md)を見てください。

> [!IMPORTANT]
> GEOFRAMEには利用者認証がありません。信頼できるローカルネットワークで使うか、
> 認証付きのリバースプロキシの内側に置いてください。インターネットへ直接公開しないでください。

## ビルドと起動

GEOFRAMEはRustの単一バイナリです。Rustの安定版ツールチェーン
（[rustup](https://rustup.rs/)）でビルドします。取り込み付きの起動例は、systemdが利用可能なLinuxで、
非rootユーザーとして実行してください。起動時に`systemctl show`で旧サービスの非稼働を確認するため、
systemdへ接続できないコンテナなどでは起動に失敗します。

```sh
cargo build --release -p geoframe
GEOFRAME_INGEST=1 ./target/release/geoframe serve --listen 127.0.0.1:8080 --data-dir ./var
```

ブラウザで <http://127.0.0.1:8080/> を開くと管理画面が表示されます。画面のファイル
（`web/static`・`web/vendor`）と旅行ブログの旅程（`web/travel_blog/plans`）はバイナリに埋め込まれます。

- `GEOFRAME_INGEST=1`のときだけ、写真の取り込み、表示用画像の生成、品質解析が順に進みます。
  無い（`0`の）あいだは**カタログ（`catalog.db`）に書かない**モードで、取り込みは止まり、位置の修正・お気に入り・
  旅行ブログの保存と書き出しなどの書き込みAPIは503を返します（読む機能は動きます。地図タイルのキャッシュだけはデータディレクトリへ書きます）。
- データディレクトリは`./var`（`--data-dir`か`GEOFRAME_DATA_DIR`で変更）で、Gitの追跡対象外です。
  最初は`./var/photos/`に写真を置くと取り込まれます。
- `serve`はrootでは起動しません。Google DriveとAIの連携は任意です。
- 本番の配布は[デプロイガイド](../deploy/README.md)を参照してください。

## 運用上の注意と外部との通信

- **認証がありません。** 複数人で使う用途、インターネットへ直接出す用途には向きません。
- **取り込み中や管理画面の操作時には、外部通信が発生します。** 写真自体のクラウドAI送信は任意ですが、座標の自動送信にも注意してください。
  - Google Driveの取り込み: `GEOFRAME_DRIVE_KEY`にサービスアカウントの鍵を置いたときだけ、Driveの一覧と原本を読みます。
  - 自動の地名・標高補完: `GEOFRAME_INGEST=1`で取り込むと、国内の未処理座標を地理院の逆ジオコーダ・標高APIへ送ります。Drive・AIの設定や管理画面の操作は不要です。
  - 地図と地名検索: 管理画面の位置修正で、OpenStreetMapと地理院のタイル、地理院とNominatimの地名検索へ通信します（座標・検索語が上流へ渡ります）。
  - AI: `GEOFRAME_AI_CONFIG`を設定しない限り推論は始まりません。クラウドを許した用途では、縮小してEXIFを除いた写真が外部のAPI（Google CloudのGemini）へ送られ、**課金が発生します**。用途ごとの1周期あたりの上限は設定できます（[設定リファレンス](reference/configuration.md)）。
- カタログ（`catalog.db`）には、手動で直したGPSやお気に入りが入ります。**公開するときは写真・`catalog.db`・生成した書き出しを含めないでください。**
- 旅行ブログの旅程の例（`web/travel_blog/plans/`）には、実際の旅程が入っています。公開前に扱いを決める予定です（[公開準備チェックリスト](public-release-checklist.md)）。
- 動作の確認は、x86_64のLinux（`x86_64-unknown-linux-musl`の静的ビルド）で行っています。他の環境は確かめていません。

## 構成

```text
写真（ローカル / Google Drive）
        │ 取り込み（GEOFRAME_INGEST=1）
        ▼
GEOFRAME（Rust単一バイナリ）── SQLite（catalog.db）
  ├─ 表示用画像・品質解析・位置の推定と修正
  ├─ 管理画面 / 旅行ブログ / AI（任意）
  └─ 写真フィード  GET /api/v1/photos ＋ /derivatives/
        │ HTTP
        ▼
miharashi（表示）
```

| パス | 内容 |
|---|---|
| [`crates/`](../crates/) | Rustのサーバー（`geoframe`。単体・結合試験を含む） |
| [`web/`](../web/) | 管理画面・旅行ブログの画面・旅程（バイナリに埋め込む）と、画面の試験 |
| [`site/`](../site/) | 紹介ページ（LP）。素のHTMLとCSSの静的サイト |
| [`docs/`](./) | アーキテクチャ、意思決定、リファレンス、履歴資料 |
| [`deploy/`](../deploy/) | systemd、配布、バックアップの構成例と、その試験 |
| [`tools/`](../tools/) | 文書とサイトの検査、計測・検証用の道具 |

## 設定とデータ

- 設定は起動引数と環境変数（`GEOFRAME_*`）で行います（`.env`は読みません）。全項目は
  [設定リファレンス](reference/configuration.md)にあります。
- 変更されるデータをソースツリー外へ置く場合は`--data-dir`か`GEOFRAME_DATA_DIR`を設定します。
- `.env`、`drive_key.json`、`catalog.db`、写真、表示用画像、生成レポートはGitへ
  コミットしないでください。標準的な配置先は`.gitignore`で除外済みです。
- `catalog.db`には、手動GPS修正、お気に入り、有料生成したAI埋め込みなど、再生成できない
  データが含まれます。稼働中のWALモードDBを単純な`cp`でコピーせず、SQLiteの`.backup`
  を使用してください。

## テスト

リポジトリルートで次を実行します。

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test`には、管理画面の描画をNodeで確かめる試験（Nodeが無ければ省略）と、コードが読む環境変数が
[設定リファレンス](reference/configuration.md)に載っているかの検査が含まれます。
文書・サイト・配布の道具は、システムの`python3`（標準ライブラリだけ）とbashで検査します。

```sh
python3 tools/check_markdown_links.py                  # 文書の相対リンク
python3 tools/check_site.py                            # 紹介ページ（site/）のリンク・資産・外部参照
python3 -m unittest discover -s deploy/tests           # 毎朝のDecision起動（deploy/loc_decision_morning.py）
python3 -m unittest discover -s tools/tests            # check_site.py の試験
node web/tests/admin_ai_ops_render.mjs web/static/js/admin.js
bash deploy/rust-deploy-test.sh                        # 配布スクリプト（モックの試験）
bash deploy/geoframe-admin-test.sh
```
