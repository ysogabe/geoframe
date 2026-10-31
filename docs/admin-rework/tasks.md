# 管理画面の整え直し（A） タスク

- PRD: prd.md / 設計: design.md
- ブランチ: t3/geoframe-admin-screen-study
- この表は **PR ①**（骨組み・旅行ブログのフラグ・掃除）の分。PR ② と ③ は ① のマージ後に main から切り、別の tasks で進める。
- PR ① が受け持つ受け入れ条件: AC1、AC2（`/blog-admin` の分）、AC3、AC4、AC6（一覧側の分）、AC10、AC11。
  AC2 の `/ai-search`・AC5・AC7・AC6 の検索側は PR ②、AC8・AC9 は PR ③。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T1 | `GEOFRAME_BLOG`（既定 0）: `config.rs` のフィールド・読み込み・`log_summary`・冒頭の表、`server.rs` の 404（`/api/blog/` の GET、`POST save/export` は `route_of` 直後、`/blog-admin` は `admin.html` へ差し替え）、既存の blog 試験を `=1` で起動、無効時の試験（GET 4 本・POST 2 本の 404、CSRF 違反でも `INGEST=0` でも 404、本文にフラグの文字が無い）、`docs/reference/configuration.md`・`deploy/README.md` の追記 | AC2（blog）, AC10, AC11 | - | full | 完了 | 1d2c8f5 | 5f798b9 | レビュー（Sol）指摘なし。この機体では試験用スタブのポートへ外のプロセスが接続して長い試験が abort することがある（変更前から）。CI で確認 |
| T2 | 一覧 API の拡張: `GET /api/admin/status` の `db.filters`（`Filter::ALL` 新設、`count_photos(condition)` で 7 件数）、`GET /api/admin/photos` の `hint` を全フィルタで位置なしの行に付ける（ページ内に位置なしの行があるときだけ `time_anchors` を読む）、`tests/admin_api.rs` で件数の一致と `hint` の有無 | AC4（API） | - | quick | 完了 | d889d10 | ab2a43d | レビュー（Opus）必須なし。参考 2 件は見送り（位置なしの判定を JSON 化後の `lat.is_null()` でなく DB の値で揃える、`hint` のコメントの更新）。PR ② の `decorate_locations` の切り出しで直す |
| T3 | 画面の骨組み: `admin.html`/`admin.css`/`admin.js` を「状況 / 写真」の 2 タブに（旅行ブログのリンクを外し、`/ai-search` へのリンクは写真タブのツールバーに残す）、チップ 7 種＋件数、ページング、「要対応」のセル、一覧側のカードの見直し（R10 の 1 行目の規則・タグ・`位置を付ける` の強調・回転の条件）、古い記述の掃除、起動を `DOMContentLoaded` の `boot()` に、`web/tests/fakedom.mjs` 新設、`web/tests/photos_render.mjs`（チップ・タブ選択・カード 5 パターン・★クリック→POST→同じ id の更新）、`tests/admin_ui_photos.rs`、`no_python.rs` の許可リスト、`admin_ai_ops_render.mjs` が検証内容を変えずに通る、`web/static` に `server.py`・`handlers.py`・`loc_suggest.py`・「表示画面」が無い試験 | AC1, AC3, AC4（画面）, AC6（一覧側）, AC11 | T2 | full | 完了 | 07f43a6 | 14a875f, 6dac071, 348fe33, f3bfd5c | レビュー（Sol）2 回。必須 1（件数取得の競合）は修正済み。見送り: 位置モーダルの地図クリックと保存の試験、(e2) のエラー応答の試験（PR ② の photos.js 移動時に足す）。目視は未実施 |
