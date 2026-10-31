# 管理画面の整え直し（A） タスク

- PRD: prd.md / 設計: design.md（末尾の「実装との差」が PR ①② の実装の正）
- PR ①（#70）と PR ②（#71）は 2026-10-31 にマージし、本番へ配布済み（main 961db5e）。
- この表は **PR ③**（設定タブ）の分。ブランチ: t3/admin-rework-3-config（main 961db5e から）。
- PR ③ が受け持つ受け入れ条件: AC1（設定タブの分）、AC8、AC9、AC11。
- ユーザーの決定（2026-10-30）: 設定タブは鍵のパスを含め**全項目を設定どおりの値で出す**（design.md「安全面」の表が唯一の仕様）。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T7 | サーバー: `config.rs` に `Source`・`Exposure`・`EnvItem`（`ALL`・`name()`・`exposure()`、ワイルドカード無しの `match`）・`sources`・`ai_config_modified`・`blog` を含む全項目。`AppState.started_at`。新設 `admin_config.rs` で `GET /api/admin/config` の応答（`version`・`started_at`・`uptime_s`・`items`（全項目、`value`・`source`、ファイルは `state`・`modified_at`）・`ai`（`configured`・`invalid`・`enabled`・`version`・`local_base_url`・`embedding_space` の全項目・`tasks[]` の `provider`・`model`・`service_id`・`allow_cloud`・`include_hidden`・`daily_limit`・`today_utc`・`credential_path`・`credential`・`project`・`region`・`disabled`）。経路は GET の完全一致、`json_response`（`no-store`）、HEAD/POST/似た名前は 403。`today_utc` は `budget::usage` で、読めなければ `null`（200 のまま）。試験: 目印（検査に通る形）が応答に全部含まれ鍵の中身（`SECRET-mark`）が無い、`Debug`・`log_summary` に目印もモデル名・region も無い、`EnvItem::ALL` の名前が `env_reference` の名前と一致、`sources` の arg/env/default、結合（200・`no-store`・HEAD/POST/configX/末尾 `/` は 403・`INGEST=0` でも 200）。文書: ai-config.md と `config.rs` のコメントに「`/api/admin/config` だけは出す」の例外、photo-pipeline.md の API 行 | AC8, AC9（API）, AC11 | - | full | 完了 | 6accce0 | 2f041cd, e9a4a5a, 7a15f51 | レビュー（Sol）指摘なし。設計と違えた点（`Config.ai_config_path` を持つ、`started_at` は bind 直後、`disabled` は全用途、時刻は秒まで、数値は文字列）は許容。設計書の「実装との差」に追記する |
| T8 | 画面: 「設定」タブ（ナビに 3 つ目）。先頭に読み取り専用の案内と「鍵のパス・接続先・連絡先が含まれるので画面を共有しない」。RUNTIME のセル（取り込み・待ち受け・データ置き場・起動からの経過・旅行ブログ・ログ・版）、設定ファイルの一覧（AI 設定 JSON のパス・状態・更新日時、Drive の鍵のパスと有無、順位モデルのパスと有無）、項目の表（分類・説明文・単位・並びは画面側が持つ。出どころのバッジ 4 色）、AI の用途の表（送り先・モデル・クラウド許可・hidden・「今日（UTC）の送信 / 日の上限」のバー・認証・project・region・無効の理由）と AI の数値の表。設定タブを開いたときだけ読む。`web/tests/config_render.mjs`（案内の文言、全項目の描画、出どころ、`today_utc` null の表示、`ai.configured=false`・`invalid` の表示）と `tests/admin_ui_config.rs`、`no_python.rs` の許可。描画は `textContent` のみ | AC1（設定タブ）, AC9（画面）, AC11 | T7 | full | 完了 | 2c3c657 | b57172f, 5b3d61f, 6f7d074, 5c41b3c | レビュー（Sol）2 回。必須 1（案内文に変更方法と再起動）は修正済み。実ブラウザの見た目は未確認 |
