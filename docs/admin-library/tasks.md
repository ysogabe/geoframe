# 写真ライブラリと選別の確認（B） タスク

- PRD: prd.md / 設計: design.md
- PR は 3 本（PRD の「実装の順番」）。① はこのブランチ、② ③ は ① のマージ後に `main` から切るブランチ（同じ worktree）。
  各 PR の前にブランチ全体の確認（テスト・PRD 突き合わせ・`security-review`）を行う。
- ブランチ: ① `t3/admin-photo-library-g3-g4` / ② `t3/admin-library-2-library`（予定） / ③ `t3/admin-library-3-curation`（予定）

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | PR ①: `GET /thumb/<id>.jpg?v=`（`make_thumbnail` と専用の上限、`thumb.rs`、`Area::Thumbs`・`thumb_dest`、ID ごとのロック・生成の枠・待ちの上限・失敗の記憶、ETag/304、`v` と世代の一致、`INGEST=0` では生成しない、起動時の `.part` 掃除）、`tests/thumb.rs`、`photo-pipeline.md` の節 | AC1, AC7（thumb 分）, AC8 | - | full | 完了 | 2cac23b | 1e7ae10, b13d161 | Sol 2 回。採用 2（停止の再確認・block_on 廃止）。見送り 1（試験の同期の厳密化。偽陽性の可能性だけで挙動の問題ではない） |
| T2 | PR ①: R4 — `AdminPhoto`・検索 API・`rotate` の応答に `thumb_url`、`python_shape` で除く、カードの `img.src` を `thumb_url || thumb`、回転後の描画試験 | AC8 | T1 | quick | 完了 | b13d161 | e5a0601 | Opus 1 回。必須なし。参考 4（回転で名前が変わらない行の世代、`/` 始まりの display、先読みの検証、報告の書き方）は見送り |
| T3 | PR ②: `admin_library.rs` の `trips`・`photos`・`points`（引数の検証、`truncate_points` の単体試験）、`parse_epoch`・`TRIP_GAP_SECONDS` の共有、フィクスチャ `library-catalog.sql`、`tests/library_api.rs` | AC2（API 分）, AC3（API 分）, AC7（library 分） | T1 | full | 未着手 | | | |
| T4 | PR ②: `photos.js` のビューア共通化（`viewerEntries`・`showViewer`・`openModalFor`・`onPhotoChange`・`tileStyle()`）、`admin.js` の `showTab`、`library.js`（旅・格子・地図・吹き出し・プレースホルダ）、`admin.html`/`admin.css`、`fake_maplibre.mjs`・`library_render.mjs`・`tests/admin_ui_library.rs` | AC2（画面分）, AC3（画面分）, AC8 | T3 | full | 未着手 | | | |
| T5 | PR ②（あると良い）: `calendar`・`places` API と日付・場所の眺め方、格子からの位置指定（R9〜R11） | AC9（一部） | T4 | quick | 未着手 | | | 時間があれば |
| T6 | PR ③: `curation.rs`（`TABLES`・`ensure_tables`・`available`・SQL 片・`group_token`）、`photos.rs`・`selection.rs`・`admin.rs` の `quality_summary` の置き換え、フィクスチャ `curation-catalog.sql`、`legacy_selection.rs` の合成ケース、契約の文書と Schema の説明文、性能の測定 | AC4（フィード分）, AC6 | T1 | full | 未着手 | | | |
| T7 | PR ③: `admin_curation.rs`（`groups`・`confirm`・`reset`）、`post.rs`・`server.rs` の登録、`tests/curation_api.rs` | AC4, AC5（API 分）, AC7（curation 分） | T6 | full | 未着手 | | | |
| T8 | PR ③: `curation.js`・`admin.html`/`admin.css`・状況タブの `curation_pending`（R18）、`curation_render.mjs`・`tests/admin_ui_curation.rs`、`photo-pipeline.md` の管理 API、設計書の「実装との差」 | AC5（画面分）, AC8, AC9（R18） | T7 | full | 未着手 | | | |
