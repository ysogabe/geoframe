# Rust単一バイナリ化 タスク

- 要求: [Issue #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 設計: [設計案](../architecture/rust-api-and-ai-providers-proposal.md) / 計画: [plan.md](plan.md)
- ブランチ: フェーズごとに作る（P0は`t3/issue-54-implementation-plan`）
- PRはフェーズごとに1つ。P2以降のタスクは、そのフェーズの開始時に細かく分け直す。
- 種類: `ops`は自分で行う運用作業、`quick`/`full`はビルドのスキルに任せる実装。

AC番号はIssue #54の受入条件の順（AC1 公開契約・永続データ、AC2 E2E、AC3 停止・lease中の動作と再試行、AC4 ベクトル空間、AC5 単一バイナリ・CI・systemd・依存、AC6 契約型・互換テスト、AC7 秘密・位置・写真を載せない）。

## P0: 棚卸しと技術PoC

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T0.1 | 検証環境の用意: lxc-geoframeの旧版退避、SSH、本番データのコピー、比較用Python（`:8090`、AI・Drive無効） | AC1 | - | ops | 実装中 | | | 退避・SSH・DBとderivativesのコピーまで済み |
| T0.2 | 棚卸し`docs/rust-migration/inventory.md`: エンドポイント・DBテーブルと書き込み元・バックグラウンド処理・設定・E2Eシナリオ、各フェーズへの割当 | AC2 | - | quick | 未着手 | | | |
| T0.3 | Cargo workspaceの骨組み（`crates/geoframe`）、CIにfmt・clippy・test、musl静的ビルドの可否と検証環境での起動確認 | AC5 | - | full | 未着手 | | | |
| T0.4 | 画像PoC: 実写真約100枚でデコード・EXIF向き・2560px・JPEG出力・HEICの計測、依存ライブラリの一覧。結果を文書へ（数値のみ） | AC5, AC7 | T0.3 | full | 未着手 | | | 原本の取得にDrive鍵を使う前にユーザー確認 |
| T0.5 | `/api/v1/photos`のserde型・JSON Schema・合成DBの契約テスト | AC6, AC7 | T0.3 | full | 未着手 | | | |

## P1: Rust API基盤

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1.1 | 設定（`GEOFRAME_*`）、SQLite読み取り層、ログ方針、ヘルス確認 | AC5, AC7 | P0 | full | 未着手 | | | |
| T1.2 | `GET /api/v1/photos`（除外・並び・タイトル・旅番号） | AC1, AC6 | T1.1 | full | 未着手 | | | |
| T1.3 | 静的配信（ホワイトリスト・ドットファイル・トラバーサル・HEAD）、Host検査、管理画面ページ、静的ファイルの埋め込み | AC1, AC5 | T1.1 | full | 未着手 | | | |
| T1.4 | 管理画面の読み取りAPI（`/api/admin/status`・`/api/admin/photos`ほか） | AC1 | T1.1 | full | 未着手 | | | |
| T1.5 | systemd unit、検証環境への配布スクリプト、Pythonとの全件比較スクリプト | AC1, AC5 | T1.2, T1.3 | full | 未着手 | | | |

## P2〜P5（開始時に分け直す）

| ID | 内容 | AC | 種類 | 状態 |
|---|---|---|---|---|
| P2 | 写真取り込み（ローカル・Drive索引、derivative、EXIF、位置推定、逆ジオコーダ、簡易品質） | AC2, AC5 | full | 未着手 |
| P3 | 管理機能（手動GPS・お気に入り・除外・回転・地名検索・タイル・旅行ブログ・更新APIのCSRF対策） | AC1, AC2 | full | 未着手 |
| P4 | AI Provider・永続ジョブ・Embeddingインデックス・位置候補・同一場面・AI検索 | AC2, AC3, AC4, AC7 | full | 未着手 |
| P5 | バックアップ・復元演習、シャドー比較、本番切替、ロールバック演習、Python依存の撤去 | AC1, AC2, AC5 | full | 未着手 |
