# ドキュメント

このディレクトリには、プロジェクト全体に関係する文書を置きます。ビルドや運用など、
特定コンポーネントだけに関係する説明は、そのコンポーネントの近くに配置します。

## はじめに読む文書

| 文書 | 状態 | 内容 |
|---|---|---|
| [写真パイプライン](architecture/photo-pipeline.md) | 現行 | 写真の取り込みからUI表示までの流れ |
| [写真カタログ](architecture/photo-catalog.md) | 参照 | SQLiteとメタデータ分離の設計・設計履歴 |
| [ネイティブ表示設計](architecture/native-display.md) | 参照 | C++/SDL2クライアントの設計 |
| [Home Assistant連携](integrations/home-assistant.md) | 現行 | 安全な連携設定 |
| [地理空間MCP](integrations/geospatial-mcp.md) | 参照 | 国土交通省APIとMCPの調査記録 |
| [デプロイ](../deploy/README.md) | 現行 | systemd、キオスク、更新、バックアップ |

## ディレクトリ構成

### `architecture/` — 設計

コードを変更するときにも参照する、システム設計と実装上の契約です。

- [写真カタログとメタデータ分離](architecture/photo-catalog.md)
- [現行の写真パイプライン](architecture/photo-pipeline.md)
- [ネイティブ表示設計](architecture/native-display.md)

ネイティブクライアント固有の詳細な契約は
[`native-display/docs/`](../native-display/docs/)にあります。

### `decisions/` — 意思決定記録

アーキテクチャ上の意思決定記録（ADR）です。判断が変わった場合は過去の記録を黙って
書き換えず、置き換える新しいADRを追加します。

- [ADR 0001: 旅行ブログ生成](decisions/0001-travel-blog-generation.md)

### `integrations/` — 外部連携

アプリケーション境界をまたぐ設定や調査です。

- [Home Assistant連携](integrations/home-assistant.md)
- [国土交通省の地理空間MCP](integrations/geospatial-mcp.md)

### `roadmap/` — 検討事項

実装を約束するものではない、将来案や検討メモです。

- [Google連携と追加表示コンテンツ](roadmap/content-integrations.md)
- [3Dの奥行き改善とサイバー表示の比較](roadmap/3d-depth-and-cyber-display.md)

### `archive/` — 履歴資料

設計の経緯を残すための過去資料です。現在と異なるパス、ハードウェア、前提条件を含む
可能性があるため、セットアップ手順としては使用しないでください。

- [初期スマートディスプレイ構想](archive/smart-display-concept.md)
- [Raspberry Pi 400 / Cog-WPE構成案](archive/rpi-cog-display-design.md)

## 文書作成ルール

- 本文と見出しは日本語を基本とします。API名、識別子、製品名などは原表記を維持します。
- ファイル名はリンクの安定性を優先して英小文字とハイフンを使用します。
- セットアップコマンドは操作対象の近く（`deploy/`、`native-display/`など）に置き、
  `docs/`には横断的な文書を置きます。
- 実ホスト名、プライベートIP、エンティティID、デバイス名、個人パス、センサー実測値、
  認証情報、本番環境の台帳はコミットしません。
- 日付付き進捗メモや機体固有runbookは、Git管理外の`local-notes/`へ置きます。
- 過去資料には履歴資料であることを明記し、計画上の挙動を現行仕様として説明しません。
