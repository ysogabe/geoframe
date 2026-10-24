# Rust単一バイナリ化 タスク

- 要求: [Issue #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 設計: [設計案](../architecture/rust-api-and-ai-providers-proposal.md) / 計画: [plan.md](plan.md)
- ブランチ: ローカルはT3の`t3/issue-54-implementation-plan`のまま、リモートはフェーズごとに分ける（P0は同名で PR #57、squash merge 済み。P1は`feat/rust-migration-p1`へpush）
- PRはフェーズごとに1つ。P2以降のタスクは、そのフェーズの開始時に細かく分け直す。
- 種類: `ops`は自分で行う運用作業、`quick`/`full`はビルドのスキルに任せる実装。

AC番号はIssue #54の受入条件の順（AC1 公開契約・永続データ、AC2 E2E、AC3 停止・lease中の動作と再試行、AC4 ベクトル空間、AC5 単一バイナリ・CI・systemd・依存、AC6 契約型・互換テスト、AC7 秘密・位置・写真を載せない）。

## P0: 棚卸しと技術PoC（PR #57、2026-10-24 マージ）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T0.1 | 検証環境の用意: lxc-geoframeの旧版退避、SSH、本番データのコピー、比較用Python（`:8090`、AI・Drive無効） | AC1 | - | ops | 完了 | - | - | 比較用Python（main e0f15fe、`geoframe-pyref.service`）の`/api/v1/photos`は本番と同じ3,065件 |
| T0.2 | 棚卸し`docs/rust-migration/inventory.md`: エンドポイント・DBテーブルと書き込み元・バックグラウンド処理・設定・E2Eシナリオ、各フェーズへの割当 | AC2 | - | quick | 完了 | 66ce058 | a0c2fca..fcea60b | レビュー（Opus）: 必須0、推奨4・参考3を反映（「ピアソン」は誤記でなく指摘不要） |
| T0.3 | Cargo workspaceの骨組み（`crates/geoframe`）、CIにfmt・clippy・test、musl静的ビルドの可否と検証環境での起動確認 | AC5 | - | full | 完了 | 8809e31 | 06df8ad | musl静的（static-pie, 約4MB）で検証環境動作。レビュー（Sol）: 必須0。推奨「WAL DBの読み取りテスト」はT1.1へ回す |
| T0.4 | 画像PoC: 実写真約100枚でデコード・EXIF向き・2560px・JPEG出力・HEICの計測、依存ライブラリの一覧。結果を文書へ（数値のみ） | AC5, AC7 | T0.3 | full | 完了 | 0f97acb | 912bb60..14152cf | 結論: HEICは純Rustのzune-heicでmusl静的のまま扱える。レビュー（Solが利用上限のためOpusで代替）: 必須0、推奨7・参考7を反映。CIのYAML構文エラー（ジョブ名のコロン）は管理者が修正。ユーザー指示（2026-10-24）でDriveは使わずローカルのファイルを使う: 検証環境の本番コピーのderivatives（JPEG）と、それを変換したHEIC。実機HEIC固有の問題（グリッド・HDR等）はP2で確認 |
| T0.5 | `/api/v1/photos`のserde型・JSON Schema・合成DBの契約テスト | AC6, AC7 | T0.3 | full | 完了 | 79d45d9 | e8598d0..6badbde | レビュー（Sol）2回: 1回目 必須2（実在地名、Pythonの`$`）・推奨2・参考1をすべて反映、2回目 指摘なし。本番コピーに`taken_at` NULLの行は無い |
| T0.6 | 全体レビューの指摘の反映: miharashiとの共有の整理、設計案レビューの記録、未決事項の更新、PoC crateの扱い | AC5, AC6 | T0.1〜T0.5 | quick | 完了 | 51fc0b6 | 51fc0b6..d3f0ac4＋管理者の文書修正 | P0全体レビュー（要求との突き合わせ、Solが利用上限のためOpusで代替）2回: 1回目 必須1・推奨6、2回目 必須0（推奨2・参考2を管理者が反映、P1受入に実クライアント確認を明記）。security-review: High/Medium 0、AC7でIPの記載を指摘→51fc0b6で削除 |

## P1: Rust API基盤

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1.1 | 設定（`GEOFRAME_*`）、SQLite読み取り層、ログ方針、ヘルス確認 | AC5, AC7 | P0 | full | 完了 | c41c4a9 | dc13801..52c7d8f | 検証環境の本番コピーでスキーマ検査を通過・/healthz 200（管理者確認）。レビュー（Solが利用上限のためOpusで代替）: 必須0、推奨4・参考を反映（パニック文言の固定、開きっぱなしのトランザクションのROLLBACK、/healthz専用接続、順番待ち10秒上限） |
| T1.2 | `GET /api/v1/photos`（除外・並び・タイトル・旅番号） | AC1, AC6 | T1.1 | full | 完了 | fae0fe6 | 9ae7fd0..5d8acc9 | 検証環境の本番コピー3,065件でPythonとJSON完全一致（管理者確認）。レビュー（Sol）: 必須0。推奨「全角数字の時刻」は文書に既知の差として記録しP2のインデクサで解消、参考「iterdumpの書式依存」は見送り |
| T1.3 | 静的配信（ホワイトリスト・ドットファイル・トラバーサル・HEAD）、Host検査、管理画面ページ、静的ファイルの埋め込み | AC1, AC5 | T1.1 | full | 実装中 | 48c9aaa | | T1.1レビュー: データ置き場がシンボリックリンクのとき、Pythonは`resolve()`で辿る。データ置き場も起動時に`canonicalize`してから比べる |
| T1.4 | 管理画面の読み取りAPI（`/api/admin/status`・`/api/admin/photos`ほか） | AC1 | T1.1 | full | 未着手 | | | |
| T1.5 | systemd unit、検証環境への配布スクリプト、Pythonとの全件比較スクリプト、miharashiの実プロセスでの取得確認 | AC1, AC5, AC6 | T1.2, T1.3 | full | 未着手 | | | 実クライアントでの確認はP0の設計案レビューで先送りしたもの。unitの注意はserver.md §6（`RestartPreventExitStatus=2`、`--listen`、`User=geoframe`） |

## P2〜P5（開始時に分け直す）

| ID | 内容 | AC | 種類 | 状態 |
|---|---|---|---|---|
| P2 | 写真取り込み（ローカル・Drive索引、derivative、EXIF、位置推定、逆ジオコーダ、簡易品質） | AC2, AC5 | full | 未着手 |
| P3 | 管理機能（手動GPS・お気に入り・除外・回転・地名検索・タイル・旅行ブログ・更新APIのCSRF対策） | AC1, AC2 | full | 未着手 |
| P4 | AI Provider・永続ジョブ・Embeddingインデックス・位置候補・同一場面・AI検索 | AC2, AC3, AC4, AC7 | full | 未着手 |
| P5 | バックアップ・復元演習、シャドー比較、本番切替、ロールバック演習、Python依存の撤去 | AC1, AC2, AC5 | full | 未着手 |
