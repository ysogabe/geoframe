# GEOFRAME

GEOFRAMEは、個人の写真を管理するセルフホスト型の写真管理基盤です。

ローカルフォルダまたはGoogle Drive上の写真をSQLiteへ索引し、表示用画像の生成、品質解析、
AIによる検索と同一場面の整理、位置情報の修正、旅行ブログの下書き作成を行います。
写真の表示は別プロジェクトの[miharashi](https://github.com/ysogabe/miharashi)が担い、
GEOFRAMEは[写真フィード](docs/architecture/photo-feed-contract.md)を提供します。

> [!IMPORTANT]
> GEOFRAMEには利用者認証がありません。信頼できるローカルネットワークで使うか、
> 認証付きのリバースプロキシの内側に置いてください。インターネットへ直接公開しないでください。

## 主な機能

- SQLiteによる写真カタログと、ローカルフォルダ・Google Driveからの取り込み
- 撮影位置の推定と逆ジオコーディング、管理画面での位置修正・お気に入り・除外
- 表示用画像（長辺2560px JPEG）の生成と、表示クライアント向けの写真フィード
- 写真の品質解析と、AIによる写真検索・同一場面の整理（任意）
- 写真カタログからの旅行ブログ下書き生成と、静的HTMLの書き出し

2026年10月に表示機能（Webキオスク、地図・気象・地震・航空機の表示、Home Assistant連携、
3D表示、ネイティブ表示）をmiharashiへ移し、このリポジトリから削除しました
（[ADR 0008](docs/decisions/0008-photo-platform-and-miharashi-display.md)）。
削除前のコードは`display-final`タグで参照できます。

## クイックスタート

Python 3.10以降を使用します（CIは3.12で検証しています）。

```sh
cd app
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
cp .env.example .env
GEOFRAME_HOST=127.0.0.1 .venv/bin/python server.py
```

ブラウザで <http://127.0.0.1:8080/> を開くと管理画面が表示されます。起動すると写真の
取り込み、表示用画像の生成、品質解析が順に進みます。データディレクトリの既定値は
`app/var/`で、Gitの追跡対象外です。Google DriveとAIの連携は任意です。

## リポジトリ構成

| パス | 内容 |
|---|---|
| [`app/`](app/) | Pythonサーバー、管理画面、テスト |
| [`docs/`](docs/) | アーキテクチャ、意思決定、リファレンス、履歴資料 |
| [`deploy/`](deploy/) | systemd、更新、バックアップの構成例 |
| [`tools/`](tools/) | リポジトリ横断の検査スクリプト |

## ドキュメント

- [ドキュメント索引](docs/README.md) — 最初に読む文書の案内
- [写真パイプライン](docs/architecture/photo-pipeline.md) / [写真カタログ](docs/architecture/photo-catalog.md)
- [写真フィードの契約](docs/architecture/photo-feed-contract.md) — 表示クライアントとの接点
- [設定リファレンス](docs/reference/configuration.md) — 環境変数の全項目と既定値
- [デプロイガイド](deploy/README.md) — systemd、更新、バックアップ

## 設定とデータ

- ローカル設定は`app/.env.example`を`app/.env`へコピーして編集します。全項目は
  [設定リファレンス](docs/reference/configuration.md)にあります。
- 変更されるデータをソースツリー外へ置く場合は`GEOFRAME_DATA_DIR`を設定します。
- `.env`、`drive_key.json`、`catalog.db`、写真、表示用画像、生成レポートはGitへ
  コミットしないでください。標準的な配置先は`.gitignore`で除外済みです。
- `catalog.db`には、手動GPS修正、お気に入り、有料生成したAI埋め込みなど、再生成できない
  データが含まれます。稼働中のWALモードDBを単純な`cp`でコピーせず、SQLiteの`.backup`
  を使用してください。

## テスト

```sh
cd app
.venv/bin/python -m unittest discover -s tests
```

文書の相対リンクと設定リファレンスの同期は、リポジトリルートで次を実行して検査します。

```sh
python3 tools/check_markdown_links.py
python3 tools/gen_env_reference.py --check
```

## コントリビューションとセキュリティ

変更を送る場合は[コントリビューションガイド](CONTRIBUTING.md)を参照してください。
脆弱性に関する報告は、公開Issueではなく[セキュリティ方針](SECURITY.md)に従ってください。

## ライセンス

このプロジェクトは[MIT License](LICENSE)で公開します。
