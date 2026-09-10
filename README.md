# GEOFRAME

GEOFRAMEは、個人の写真を地図・気象・地震・航空機などの地理情報と組み合わせて表示する、
セルフホスト型のアンビエントフォトディスプレイです。

メインアプリケーションは、ローカルフォルダまたはGoogle Drive上の写真をSQLiteへ索引し、
表示用画像を生成して、ブラウザ向け画面とAPIを提供します。常時稼働のミニPC（N100クラス）と
壁掛けモニターでの据え置き利用を想定しています。専用キオスクやネイティブ表示の実験的な
クライアントも含みます。

> [!IMPORTANT]
> GEOFRAMEは信頼できるローカルネットワークでの利用を想定しています。写真APIと管理APIには
> 利用者認証がありません。インターネットへ直接公開しないでください。

## 主な機能

- SQLiteによる写真カタログと、ローカルフォルダ・Google Driveからの取り込み
- 地図上のフォトジャーニーと国土交通省の地理空間データ表示
- 気象庁の天気予報・地震情報と、地震時の防災レイヤ表示
- Navara（`@navaramap/three`）による3Dフォトジャーニー（`?mode=3d`、省電力構成）
- 写真カタログからの旅行ブログ下書き生成と、静的HTMLの書き出し
- Home Assistantによる在宅判定、室内環境表示、画面ON/OFFの手動オーバーライド（任意）
- ADS-B航空機連携と、AIによる写真検索・選定・同一場面の整理（任意）
- ブラウザキオスク、macOS WKWebView、実験的なSDL2クライアント

## クイックスタート

Python 3.10以降を使用します（CIは3.12で検証しています）。

```sh
cd app
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
cp .env.example .env
GEOFRAME_HOST=127.0.0.1 .venv/bin/python server.py
```

ブラウザで <http://127.0.0.1:8080/> を開きます。起動すると写真の取り込み、表示用画像の生成、
品質解析が順に進みます。データディレクトリの既定値は`app/var/`で、Gitの追跡対象外です。
Google Drive、Home Assistant、国土交通省API、ADS-B、AIの各連携は任意で、未設定でも写真の
表示は動作します。

3D表示はURLに`?mode=3d`を付けたときだけNavaraを読み込みます。WebGL/WASMの初期化に失敗した
場合は、そのセッションでは通常表示へ自動的に戻ります。

## リポジトリ構成

| パス | 内容 |
|---|---|
| [`app/`](app/) | Pythonサーバー、Web UI、テスト |
| [`docs/`](docs/) | アーキテクチャ、意思決定、リファレンス、外部連携、ロードマップ、履歴資料 |
| [`frontend/`](frontend/) | 3Dフォトジャーニー（Navara）アダプタのソース |
| [`deploy/`](deploy/) | systemd、キオスク、更新、バックアップの構成例 |
| [`native-display/`](native-display/) | 実験的なC++/SDL2表示クライアント |
| [`mac-wkwebview/`](mac-wkwebview/) | macOS向けの薄いWKWebViewキオスククライアント |
| [`tools/`](tools/) | リポジトリ横断の検査スクリプト |

3D表示のWASMバンドル（`app/static/navara/`）は生成物をコミットしており、ルートで
`npm ci && npm run build:navara`を実行すると決定的に再生成できます。

## ドキュメント

- [ドキュメント索引](docs/README.md) — 最初に読む文書の案内
- [写真パイプライン](docs/architecture/photo-pipeline.md) / [写真カタログ](docs/architecture/photo-catalog.md)
- [3Dフォトジャーニーの実装契約](docs/architecture/3d-photo-journey-lite.md)
- [設定リファレンス](docs/reference/configuration.md) — 環境変数の全項目と既定値
- [デプロイガイド](deploy/README.md) — systemd、キオスク、更新、バックアップ
- [ネイティブ表示](native-display/README.md) / [macOS WKWebView](mac-wkwebview/README.md)

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
for t in tests/js/*.test.js; do node "$t" || exit 1; done
```

`app/tests/js/navara-build-integrity.test.js`は`app/static/navara/`の生成物を検査するため、
生成物を含むチェックアウト（または`npm run build:navara`の実行後）で動かしてください。
文書の相対リンクと設定リファレンスの同期は、リポジトリルートで次を実行して検査します。

```sh
python3 tools/check_markdown_links.py
python3 tools/gen_env_reference.py --check
```

ネイティブクライアントのビルドとテストは[`native-display/README.md`](native-display/README.md)、
macOSクライアントは[`mac-wkwebview/README.md`](mac-wkwebview/README.md)を参照してください。

## コントリビューションとセキュリティ

変更を送る場合は[コントリビューションガイド](CONTRIBUTING.md)を参照してください。
脆弱性に関する報告は、公開Issueではなく[セキュリティ方針](SECURITY.md)に従ってください。

## ライセンス

このプロジェクトは[MIT License](LICENSE)で公開します。
