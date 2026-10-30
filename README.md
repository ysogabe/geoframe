# GEOFRAME

**撮った写真を、手元で整理して、きれいに見せる準備をする。** GEOFRAMEは、個人の写真を管理するセルフホスト型の写真管理基盤です。

ローカルフォルダやGoogle Drive上の写真をSQLiteのカタログへ索引し、表示用画像の生成、品質解析、
撮影地の推定と修正、旅行ブログの下書き作成までを1つのRust製の単一バイナリで行います。
任意でAIを使い、言葉で写真を探したり、似た場面の写真から良いカットを選んだりもできます。
写真の表示は別プロジェクトの[miharashi](https://github.com/ysogabe/miharashi)が担い、
GEOFRAMEは[写真フィード](docs/architecture/photo-feed-contract.md)を提供します。

**紹介ページ（LP）**: <https://ysogabe.github.io/geoframe-ambient-display/>
（公開予定のURLです。リポジトリの公開とGitHub Pagesの有効化が済むまでは開けません。ソースは[`site/`](site/)、手順は[LPの管理](docs/LP.md)）

> [!IMPORTANT]
> GEOFRAMEには利用者認証がありません。信頼できるローカルネットワークで使うか、
> 認証付きのリバースプロキシの内側に置いてください。インターネットへ直接公開しないでください。

## できること

- **カタログ**: ローカルフォルダとGoogle Driveの写真をSQLiteへ索引します。HEICにも対応します。
- **表示用画像**: 原本は変えず、長辺2560pxのJPEGを別に作ります。作った画像にはEXIF（GPSを含む）を残しません。
- **品質解析**: 鮮明度・露出・クリッピングなどを解析し、似た写真の中から勧める写真を決めます。
- **撮影地**: 位置の無い写真の撮影地を前後の写真から推定し、逆ジオコーディングで地名を付けます。管理画面の地図で位置を直し、お気に入りや除外も付けられます。
- **旅行ブログ**: 旅程と写真カタログを照合し、日別の下書きを作ります。EXIFを除いた画像つきの静的HTMLとして書き出せます。
- **AI（任意）**: 言葉による写真検索、同一場面の整理、撮影地の手がかりの判定と推定を、LAN内のモデルサービスまたはクラウド（Gemini）で行います。設定するまで動きません。
- **写真フィード**: 表示クライアント向けに`GET /api/v1/photos`と`/derivatives/`を提供します。miharashiがこれを読んで描画します。

2026年10月に、表示機能（Webキオスク、地図・気象・地震・航空機の表示、Home Assistant連携、
3D表示、ネイティブ表示）をmiharashiへ移し、このリポジトリから削除しました
（[ADR 0008](docs/decisions/0008-photo-platform-and-miharashi-display.md)）。
削除前のコードは`display-final`タグで参照できます。

## いまの状態

Rustの単一バイナリへの移行（[実装記録](docs/rust-migration/plan.md)）を終え、Python版は撤去しました。
一人の利用者が自宅で運用している段階で、リポジトリは公開の準備中です
（[公開準備チェックリスト](docs/public-release-checklist.md)）。次のことに注意してください。

- **認証がありません。** 複数人で使う用途、インターネットへ直接出す用途には向きません。
- **取り込み中や管理画面の操作時には、外部通信が発生します。** 写真自体のクラウドAI送信は任意ですが、座標の自動送信にも注意してください。
  - Google Driveの取り込み: `GEOFRAME_DRIVE_KEY`にサービスアカウントの鍵を置いたときだけ、Driveの一覧と原本を読みます。
  - 自動の地名・標高補完: `GEOFRAME_INGEST=1`で取り込むと、国内の未処理座標を地理院の逆ジオコーダ・標高APIへ送ります。Drive・AIの設定や管理画面の操作は不要です。
  - 地図と地名検索: 管理画面の位置修正で、OpenStreetMapと地理院のタイル、地理院とNominatimの地名検索へ通信します（座標・検索語が上流へ渡ります）。
  - AI: `GEOFRAME_AI_CONFIG`を設定しない限り推論は始まりません。クラウドを許した用途では、縮小してEXIFを除いた写真が外部のAPI（Google CloudのGemini）へ送られ、**課金が発生します**。用途ごとの1周期あたりの上限は設定できます（[設定リファレンス](docs/reference/configuration.md)）。
- カタログ（`catalog.db`）には、手動で直したGPSやお気に入りが入ります。**公開するときは写真・`catalog.db`・生成した書き出しを含めないでください。**
- 旅行ブログの旅程の例（`web/travel_blog/plans/`）には、実際の旅程が入っています。公開前に扱いを決める予定です（チェックリスト参照）。
- 動作の確認は、x86_64のLinux（`x86_64-unknown-linux-musl`の静的ビルド）で行っています。他の環境は確かめていません。

## 導入

GEOFRAMEはRustの単一バイナリです（Pythonは要りません）。Rustの安定版ツールチェーン
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
- 本番の配布は[デプロイガイド](deploy/README.md)を参照してください。

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
| [`crates/`](crates/) | Rustのサーバー（`geoframe`。単体・結合試験を含む） |
| [`web/`](web/) | 管理画面・旅行ブログの画面・旅程（バイナリに埋め込む）と、画面の試験 |
| [`site/`](site/) | 紹介ページ（LP）。素のHTMLとCSSの静的サイト |
| [`docs/`](docs/) | アーキテクチャ、意思決定、リファレンス、履歴資料 |
| [`deploy/`](deploy/) | systemd、配布、バックアップの構成例と、その試験 |
| [`tools/`](tools/) | 文書とサイトの検査、計測・検証用の道具 |

## ドキュメント

- [ドキュメント索引](docs/README.md) — 最初に読む文書の案内
- [写真パイプライン](docs/architecture/photo-pipeline.md) / [写真カタログ](docs/architecture/photo-catalog.md)
- [写真フィードの契約](docs/architecture/photo-feed-contract.md) — 表示クライアントとの接点
- [設定リファレンス](docs/reference/configuration.md) — 環境変数の全項目と既定値
- [デプロイガイド](deploy/README.md) — systemd、更新、バックアップ
- [Rust移行の実装記録](docs/rust-migration/plan.md) — 履歴。現行の仕様書ではありません
- [LPの管理](docs/LP.md) / [公開準備チェックリスト](docs/public-release-checklist.md)

## 設定とデータ

- 設定は起動引数と環境変数（`GEOFRAME_*`）で行います（`.env`は読みません）。全項目は
  [設定リファレンス](docs/reference/configuration.md)にあります。
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
[設定リファレンス](docs/reference/configuration.md)に載っているかの検査が含まれます。
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

## コントリビューションとセキュリティ

変更を送る場合は[コントリビューションガイド](CONTRIBUTING.md)を参照してください。
脆弱性に関する報告は、公開Issueではなく[セキュリティ方針](SECURITY.md)に従ってください。

## ライセンス

このプロジェクトは[MIT License](LICENSE)で公開します。同梱するサードパーティ資産
（MapLibre GL JS、Share Tech Mono）のライセンス本文は[`web/vendor/`](web/vendor/README.md)にあります。
