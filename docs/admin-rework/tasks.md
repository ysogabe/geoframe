# 管理画面の整え直し（A） タスク

- PRD: prd.md / 設計: design.md
- PR ①（骨組み・旅行ブログのフラグ・掃除）は geoframe の PR #70 で 2026-10-31 にマージ済み（T1〜T3）。
- この表は **PR ②**（写真タブへの AI 検索の統合とカードの統一）の分。ブランチ: t3/admin-rework-2-search（main cb0fc2f から）。
- PR ② が受け持つ受け入れ条件: AC2（`/ai-search` の分）、AC5、AC6（検索側の分と、PR ① で見送った位置モーダルの保存の試験）、AC7、AC11。AC8・AC9 は PR ③。
- PR ① から持ち越した小さな項目もここで直す: T2 の参考 2 件（位置なしの判定を DB の値で揃える、`hint` のコメント）、`loadFilterCounts` のエラー応答の試験、`innerHTML`（OSM 出典）の除去と grep 試験。

| ID | 内容 | AC | 依存 | 種類 | 状態 | 開始 SHA | コミット | メモ |
|---|---|---|---|---|---|---|---|---|
| T4 | サーバー: `admin.rs` の `time_anchors`・`nearest_hint`・`suggestions`・`infer_bucket` を補完関数 `decorate_locations` に切り出し、`GET /api/admin/photos` と `GET /api/ai/search` の両方から呼ぶ。検索は返すグループ・写真が確定した後に重複なしの ID で 1 回だけ呼び、位置なしの写真が無ければ `time_anchors` を読まない。検索の写真 JSON に `source`・`rotation`・`loc_source`・`infer_dt_s`・`infer_bucket`・`hint`・`suggest` を足す。位置なしの判定は DB の値（`lat IS NULL`）で揃え、`hint` のコメントを直す。試験: 検索 API の写真に項目が付く、同じ写真の `hint`・`suggest` が一覧と検索で一致、位置ありだけの結果では錨を読まない | AC6（検索側の API） | - | full | 完了 | 89b6244 | 6a5e6be | レビュー（Sol）必須なし。推奨 2 件は見送り（DB に非有限の実数が入ったときの `infer_bucket` の差: アプリは書かない。補完が別トランザクション: 表示用のヒントで許容。設計の判断どおり） |
| T5 | 画面: `photos.js` 新設（`normalizePhoto`・`cardFor`・一覧・検索・場面・ビューア・位置指定モーダルを `admin.js` から移す。`admin.js` は共通ヘルパ・タブ・状況・起動に縮める）。写真タブに検索バー（検索・「AI おすすめ」・「除外も検索」・`#search-note`）、`#groups`（場面ごと）、`#viewer`（`ai-search.html` のビューアを移植）。成功時だけ `view='search'`、失敗は設計の表の日本語（400 `bad_params` も）、「✕ 検索を閉じる」で直前の `filter`・`page` に戻る。`initialTab()` は `/ai-search` で写真タブ＋検索欄にフォーカス。`admin.css` に `.ps-` の部品を統合。OSM 出典の `innerHTML` を DOM で組み直す。描画試験: 検索の成功・失敗 4 種・閉じる・AI おすすめの URL、検索結果のカードからの ★/除外/回転/位置、`stepModal` の順序、位置モーダルの地図クリックと保存（地図スタブ）、`loadFilterCounts` のエラー応答。`ai-search.html` のリンクは残す（T6 で消す） | AC5, AC6（検索側の画面）, AC7, AC11 | T4 | full | 完了 | 6da8dfa | edb8a51, b4a4c68, c4892fc | レビュー（Sol）2 回。必須 1（検索中の条件切替）は修正済み。推奨「300 文字超のときも先行の検索を無効にする」は T6 で直す。設計と違えた点: 位置の保存後は地名でなく座標（応答に地名が無い）。目視は未実施 |
| T6 | 仕上げ: `ai-search.html`・`ai-search.css`・`js/ai-search.js` を削除し、`webpath::page_target` の `/ai-search` を `static/admin.html` に向ける（`webpath.rs`・`embedded.rs`・`static_files.rs` の試験を更新。`/ai-search` の本文が `/admin` と同じ、描画試験で写真タブ＋フォーカス）。写真タブの一時的な「AI 写真検索 ▸」リンクを外す。`web/static/js` に `innerHTML`・`outerHTML`・`insertAdjacentHTML`・`document.write` が無いことの grep 試験。`docs/architecture/photo-pipeline.md` の「API・UI」の `/ai-search` の記述を直す | AC2（`/ai-search`）, AC11 | T5 | quick | 完了 | 435efea | 790477d, 0736899 | レビュー（Opus）必須・推奨なし。参考 2 件（grep 試験は 1 階層のみ、300 文字超のあとの条件切替）は見送り |
