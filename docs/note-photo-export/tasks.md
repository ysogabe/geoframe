# note 記事用の写真書き出し タスク

- PRD: prd.md / 設計: design.md
- ブランチ: t3/note-photo-export-impl
- PR は 1 本にまとめる（設計 11 章の ⓪①② はコミットの順として積む）。マージはしない（PR を作るところまで）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | 写真サービスの一覧 API `GET /api/admin/export/photos`（A′、6.4）と試験、書き出し側の fixture、docs/reference への追記 | AC1, AC3（写真サービス側） | - | full | 完了 | 7d689f0 | 787e90d..46ad259 | レビュー（Sol）必須 0、推奨 1 件（秒付き日時の試験）を反映 |
| T2 | `crates/geoframe-export` の骨組み、引数の手書き解釈、設定の検査（接頭辞・work-root・remote 名・専用 client_id）、一覧の取得と 4.4 の検査、日付の選び方、フォルダ名 | AC1, AC2, AC3, 上限 X
| T3 | 画像の取得（上限・区画の検査・O_EXCL/O_NOFOLLOW）、寸法・明るさ・シャープネス（4.3）、manifest の生成。旧 manifest との実測 | AC3, AC4, AC5b, 一部の失敗, 上限, 4.3 | T2 | full | 未着手 | | | |
| T4 | rclone の呼び出し、宛先の状態、管理フォルダの判定、incoming→検証→公開、退避と復旧の表 | AC5, AC6, AC10 | T3 | full | 未着手 | | | |
| T5 | 実行枠（flock）、ジョブの記録と公開の記録、`run`・`recover` のコマンドと終了コード | AC8, AC9（記録）, AC11 | T4 | full | 未着手 | | | |
| T6 | `serve`（bind の検査、トークン、送信元の許可、本文の検査、接続の上限）、ジョブの API、`request_id` | AC7, AC8, AC9, AC10 | T5 | full | 未着手 | | | |
| T7 | unit の雛形、deploy/README の手順（専用ユーザー・専用 client_id）、静的ビルドと CI への追加、PRD・設計の状態欄と 11 章 | 配置の準備 | T6 | full | 未着手 | | | |
