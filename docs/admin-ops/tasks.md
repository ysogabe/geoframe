# 運用コンソール（C） タスク

- PRD: prd.md / 設計: design.md
- PR は 2 本（PRD の「実装の順番」）。① はこのブランチ、② は ① のマージ後に `main` から切るブランチ（同じ worktree）。
  各 PR の前にブランチ全体の確認（テスト・PRD 突き合わせ・`security-review`）を行う。
- ブランチ: ① `t3/admin-ops-console-design` / ② （① のマージ後に決める）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | PR ①: `ops.rs`（`OpKind`・`Request`・`Record`・`Report`・`Board`（1 つの Mutex、`enqueue`/`take_queued`/`finish`/`last_drive_read_finished`/時計の注入）・`token_of`・JSON 化）、`drive/index.rs` の `Allow { No, Yes, Only(HeldIds) }`・`HeldIds`（件数だけの `Debug`）・`MassDeleteHold.ids`・`plan()` の `Only` の分岐（空の一覧は常に保留）・`counts` の `approved`/`deleted`・`Only` のときの写し（`.part` → rename、空き容量、writer の中でトランザクションの外）と保留の記憶の順序（コミット後に消す）、単体試験 | AC4（plan・run・token の単体分）, AC6（Debug の単体分） | - | full | 完了 | f088bc6 | 6e426ae, d664ffa | Sol 2 回。採用 4（写しの権限を作成時から・途中停止の試験・前回の写しがある失敗の試験・WAL を含む空き容量）。見送り 1（umask を固定した別プロセスでの権限の試験。処理は正しく、試験の環境依存だけ） |
| T2 | PR ①: `sync.rs` の `stages(drive, Allow)`・`index_drive_stage_only`、`quality::Scan`（Stage）、`spawn_worker` の変更（`Board`・`MassDeleteBoard`、100 ms 刻みの待ち、`run(req)` と隔離、`outcome` の対応表、手動同期後の周期）、`cli.rs` の `serve` の配線、偽の Stage による単体試験（busy・直列・隔離・停止・`too_soon`・`hold_changed`） | AC2(b), AC4（`hold_changed`・`skipped`/`failed` の分） | T1 | full | 完了 | d664ffa | dcc984d, 3177130, df3e6ca | Sol 1 回。必須なし。推奨 2（写しの試験が更新中を確かめる・DriveRelease の NotRun/Failed/Panicked をワーカー経由で）を採用。見送りなし。推奨だけなので再レビューは省いた |
| T3 | PR ①: `admin_ops.rs`（`GET /api/admin/ops`、`sync`・`quality-scan`・`drive-release` の POST）、`post.rs`・`server.rs` の登録、`AppState.ops`、`tests/ops_api.rs`（子プロセスの `serve` と `spawn_state`）、`photo-pipeline.md` の管理 API の節 | AC2(a), AC3, AC4（API 分）, AC6, AC7（3 本分） | T2 | full | 完了 | df3e6ca | f47cc8a, c234c1a | Sol 2 回。採用 2（停止後の drive-release を保留の判定より先に 503・AC3 で特徴量の世代一致を数える）。見送りなし。tests/no_python.rs の許可リストに ops_api の BIN と kill を追加 |
| T4 | PR ①: `ops.js`・`admin.html`・`admin.css`・`admin.js`（タブ・`showTab`・`boot`・`build.rs`/`embedded.rs`）、状況タブの大量削除の文言、`ops_render.mjs`・`tests/admin_ui_ops.rs` | AC1（3 操作分）, AC2(c), AC4（描画分）, AC8, AC9（static_files 分） | T3 | full | 完了 | c234c1a | 69c8b8c | Sol 1 回。指摘なし。ブラウザでの目視は未実施（本番機のため） |
| T5 | PR ①: 文書の同期系（`deploy/README.md` の保留の解除と写しの戻し方、`geoframe-admin.sh` の冒頭、`server.md`、`drive/index.rs` の警告ログの文言、`docs/README.md`）、設計書の「実装との差」 | AC9（文書分） | T4 | quick | 実装中 | 69c8b8c | | |
| T6 | PR ②: `cli.rs` で再試行用の handlers（2 組目）・`ai_clock`・`ai_ops_lock` を `AppState` へ、`ai::ops::retry` を部分集計つきの誤り型に（CLI の出力・終了コードは不変）、`admin_ops.rs` の `ai-retry`・`ai-release-pause`（閉包の中で `try_lock`）、`tests/ops_api.rs` の AI 分、`scene_judge` の交差の試験 | AC5, AC7（AI 分） | T5（PR ① マージ） | full | 未着手 | | | |
| T7 | PR ②: `ops.js` の AI の区画と R7 の案内、状況タブの AI の文言と `admin_ai_ops_render.mjs` の期待値、R9（要対応の行）、描画試験、文書の AI 分（`deploy/README.md`、`ai/ops.rs` のコメント、`cli.rs` のコメント） | AC1（5 操作分）, AC9（AI 分） | T6 | full | 未着手 | | | |
