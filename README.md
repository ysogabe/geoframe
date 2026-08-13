# GEOFRAME

GEOFRAMEは、個人の写真を地図・気象・地震・航空機などの地理情報と組み合わせて表示する、
セルフホスト型のアンビエントフォトディスプレイです。

メインアプリケーションは、ローカルフォルダまたはGoogle Drive上の写真をSQLiteへ索引し、
表示用画像を生成して、ブラウザ向け画面とAPIを提供します。専用キオスクやネイティブ表示の
実験的なクライアントも含みます。

> [!IMPORTANT]
> GEOFRAMEは信頼できるローカルネットワークでの利用を想定しています。写真APIと管理APIには
> 利用者認証がありません。インターネットへ直接公開しないでください。

## 主な機能

- SQLiteによる写真カタログと、ローカルフォルダ・Google Driveからの取り込み
- 地図上のフォトジャーニーと国土交通省の地理空間データ表示
- 気象庁の天気予報・地震情報
- Home Assistantによる在宅判定と室内環境表示（任意）
- ADS-B航空機連携とAIによる写真検索・選定（任意）
- ブラウザキオスク、macOS WKWebView、実験的なSDL2クライアント

## クイックスタート

Python 3.10以降を使用します。

```sh
cd app
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
cp .env.example .env
GEOFRAME_HOST=127.0.0.1 .venv/bin/python server.py
```

ブラウザで <http://127.0.0.1:8080/> を開きます。データディレクトリの既定値は
`app/var/`で、Gitの追跡対象外です。Google Drive、Home Assistant、国土交通省API、
ADS-B、AIの各連携は任意です。

## リポジトリ構成

| パス | 内容 |
|---|---|
| [`app/`](app/) | Pythonサーバー、Web UI、テスト |
| [`docs/`](docs/) | アーキテクチャ、意思決定、外部連携、ロードマップ、履歴資料 |
| [`deploy/`](deploy/) | systemd、キオスク、更新、バックアップの構成例 |
| [`native-display/`](native-display/) | 実験的なC++/SDL2表示クライアント |
| [`mac-wkwebview/`](mac-wkwebview/) | macOS向けの薄いWKWebViewキオスククライアント |

最初に[ドキュメント索引](docs/README.md)を参照してください。本番環境の構築方法は
[デプロイガイド](deploy/README.md)にあります。

## 設定とデータ

- ローカル設定は`app/.env.example`を`app/.env`へコピーして編集します。
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

ネイティブクライアントのビルドとテストは
[`native-display/README.md`](native-display/README.md)を参照してください。

## コントリビューションとセキュリティ

変更を送る場合は[コントリビューションガイド](CONTRIBUTING.md)を参照してください。
脆弱性に関する報告は、公開Issueではなく[セキュリティ方針](SECURITY.md)に従ってください。

## ライセンス

このプロジェクトは[MIT License](LICENSE)で公開します。
