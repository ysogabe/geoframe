# ドキュメント

このディレクトリには、プロジェクト全体に関係する文書を置きます。ビルドや運用など、
特定コンポーネントだけに関係する説明は、そのコンポーネントの近くに配置します。

## はじめに読む文書

| 文書 | 状態 | 内容 |
|---|---|---|
| [写真パイプライン](architecture/photo-pipeline.md) | 現行 | 写真の取り込みから配信APIまでの流れ |
| [写真カタログ](architecture/photo-catalog.md) | 参照 | SQLiteとメタデータ分離の設計・設計履歴 |
| [写真フィードの契約](architecture/photo-feed-contract.md) | 現行 | 表示クライアント（miharashi）へ渡す写真一覧と画像 |
| [ADR 0008: 写真管理基盤への転換](decisions/0008-photo-platform-and-miharashi-display.md) | 現行 | 表示をmiharashiへ移し、GEOFRAMEを写真管理基盤にした経緯 |
| [設定リファレンス](reference/configuration.md) | 現行 | 環境変数の一覧（`config.py`から自動生成） |
| [デプロイ](../deploy/README.md) | 現行 | systemd、更新、バックアップ |

## ディレクトリ構成

### `architecture/` — 設計

コードを変更するときにも参照する、システム設計と実装上の契約です。

- [写真カタログとメタデータ分離](architecture/photo-catalog.md)
- [現行の写真パイプライン](architecture/photo-pipeline.md)
- [写真フィードの契約（表示クライアント向け）](architecture/photo-feed-contract.md)

### `decisions/` — 意思決定記録

アーキテクチャ上の意思決定記録（ADR）です。判断が変わった場合は過去の記録を黙って
書き換えず、置き換える新しいADRを追加します。

- [ADR 0001: 旅行ブログ生成](decisions/0001-travel-blog-generation.md)
- [ADR 0002: 画面電源の手動オーバーライド](decisions/0002-screen-power-override-via-ha.md)（廃止）
- [ADR 0003: 画面制御の状態機械](decisions/0003-screen-control-state-machine.md)（廃止）
- [ADR 0004: 表示モードのアービトレーション](decisions/0004-frontend-display-mode-arbitration.md)（廃止）
- [ADR 0005: エージェント稼働データのHAバス](decisions/0005-agent-status-via-ha-bus.md)（廃止）
- [ADR 0006: 実装順序（Wave構成）](decisions/0006-implementation-sequencing.md)（廃止）
- [ADR 0007: 管理画面のキーマップ保存](decisions/0007-admin-keymap-write-path.md)（廃止）
- [ADR 0008: GEOFRAMEを写真管理基盤にし、表示はmiharashiへ移す](decisions/0008-photo-platform-and-miharashi-display.md)

### `reference/` — リファレンス

コードから機械的に導ける一覧です。手で書き写さず、生成コマンドで更新します。生成結果が
古い場合はCI（`tools/gen_env_reference.py --check`）が失敗します。

- [設定リファレンス（環境変数）](reference/configuration.md) — `app/geoframe/config.py` から生成

### `archive/` — 履歴資料

設計の経緯を残すための過去資料です。現在と異なるパス、ハードウェア、前提条件を含む
可能性があるため、セットアップ手順としては使用しないでください。表示系（キオスク・3D・
外部連携）の資料はADR 0008で削除したコードを前提にしています。削除前のコードは
`display-final`タグで参照できます。

- [初期スマートディスプレイ構想](archive/smart-display-concept.md)
- [Raspberry Pi 400 / Cog-WPE構成案](archive/rpi-cog-display-design.md)
- [Raspberry Pi 400 キオスク移行の基準値](archive/rpi400-kiosk-migration-baseline.md)
- [ネイティブ表示の企画案（実装前の概要設計）](archive/native-display-concept.md)
- [3D PHOTO JOURNEY Lite（Navara）の実装契約](archive/3d-photo-journey-lite.md)
- [3D PHOTO JOURNEY の奥行きとサイバー地形](archive/3d-depth-and-cyber-display.md)
- [Home Assistant連携](archive/home-assistant.md)
- [国土交通省の地理空間MCP](archive/geospatial-mcp.md)
- [Google連携と追加表示コンテンツ](archive/content-integrations.md)
- [表示モード拡張の実装プラン（Wave構成）](archive/display-mode-expansion-plan.md)

## 文書作成ルール

- 本文と見出しは日本語を基本とします。API名、識別子、製品名などは原表記を維持します。
- ファイル名はリンクの安定性を優先して英小文字とハイフンを使用します。
- セットアップコマンドは操作対象の近く（`deploy/`など）に置き、
  `docs/`には横断的な文書を置きます。
- 実ホスト名、プライベートIP、エンティティID、デバイス名、個人パス、センサー実測値、
  認証情報、本番環境の台帳はコミットしません。
- 日付付き進捗メモや機体固有runbookは、Git管理外の`local-notes/`へ置きます。
- 過去資料には履歴資料であることを明記し、計画上の挙動を現行仕様として説明しません。
- 新しい文書を追加したら、この索引（`docs/README.md`）の該当節にも追加します。
- 索引に載せた文書の状態（現行・参照・計画）は、実装の進行に合わせて更新します。
- 相対リンクはCIの`tools/check_markdown_links.py`で検査されます。文書を移動したら、索引と
  参照元のリンクも同じ変更で更新します。
