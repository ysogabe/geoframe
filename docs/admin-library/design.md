# 写真ライブラリと選別の確認（B） 設計

作成日: 2026-11-01。状態: 下書き（レビュー前）。PRD: [prd.md](prd.md)。

## 概要

[PRD](prd.md) の B（サムネイル、ライブラリ G3、選別の確認 G4）をどう作るか。A の設計書
（[../admin-rework/design.md](../admin-rework/design.md)。末尾の「実装との差」が実装の正）の上に足す。
行番号は 2026-11-01（main b9ebb6f）時点。

## 今の作り（調査結果）

### 品質解析と推奨

- 同期の段 `quality`（`crates/geoframe/src/quality.rs:1-19`）と CLI `quality-scan`（`quality.rs:546`）。未解析を
  `GEOFRAME_QUALITY_PER_CYCLE`（既定 25）件ずつ解析し、残りが 0 になると `rebuild_if_ready`（`quality.rs:345`）が
  **`DELETE FROM photo_quality`（`quality.rs:360`）の後に全行を入れ直す**。`group_id` は再構築のたびに振り直される
  （`quality/group.rs:344-353`）。
- グループ: 同じ `content_sha256`（`group.rs:252-261`）か、撮影時刻差 3 分以内かつ pHash 距離 12 以下
  （`group.rs:19-21`）の連結成分。2 枚以上だけが行になる（`group.rs:284-293`）。SHA が 1 種類なら `exact`、それ以外は
  `similar`（`group.rs:303`）。
- 推奨: 点数（鮮明さ 62%・露出 25%・コントラスト 13% − 白飛び・黒つぶれ、`group.rs:165-170`）の降順、同点は `id` 昇順
  （`group.rs:296-302`）。SHA ごとの最上位を代表にし、その代表の上位 2 枚が `recommended=1`（`group.rs:305-313`）。
  `reasons_json`・`issues_json` は日本語の文字列の JSON 配列（`group.rs:171-215`）。`algorithm` は
  `rust-phash-v1:t3:h12`（`quality.rs:50`）。
- 表（`schema.rs:66-84`）: `photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id, group_kind,
  rank_in_group, recommended, score, reasons_json, issues_json, algorithm, analyzed_at)`。読む側は `display`・`md5`・`mtime`
  が今の `photos` と合う行だけ使う（`photos.rs:41-44` の `QUALITY_JOIN`、`admin.rs:335` の `quality_summary`）。
- **人の判断を置ける列は無い。** 再構築で残る人手の列は `photos.favorite`・`photos.hidden` だけ。

### フィード

- `list_photos`（`photos.rs:57-82`）: `lat IS NOT NULL AND display IS NOT NULL AND hidden=0`、
  `QUALITY_VISIBLE = (q.group_kind IS NULL OR q.group_kind!='exact' OR q.recommended=1)`（`photos.rs:47`）、
  旧 AI 選別の `ai_visible()`（`selection.rs:52`）。`quality_recommended` は `in_group && recommended`、`quality_role` は
  `recommended`/`alternate`/空（`photos.rs:166-171`）。
- 旅: `assign_trips`（`photos.rs:335-352`）、`TRIP_GAP_SECONDS = 48*3600`（`photos.rs:54`）。`e - last > 48h` で新しい旅。
  時刻の読めない写真はその都度新しい旅。番号はフィードに出る写真の中だけで振る。
- 契約（`docs/architecture/photo-feed-contract.md`、`photo-feed.schema.json`）: `quality_recommended` は
  「品質解析でグループの上位として推奨された写真。グループに入らなければ `false`」。管理用 API は契約の対象外。

### 管理 API と防御

- `GET /api/admin/photos`（`server.rs:316-347`、`admin.rs:1046-1140`）: `page`・`per`（既定 50、上限 200）・`filter`。
  並びは `taken_at` 昇順（空は最後）。`thumb` は `thumb_for`（`admin.rs:827`）で `/<display>` か `/derivatives/<drive_id>.jpg`。
  **Python 版の出力との完全一致の試験**（`tests/admin_api.rs:28` の `python_shape` で Rust だけの項目を除いて比べる。
  フィクスチャは再生成できない）。`GET /api/admin/status` も同じ。
- 書き込み（`post.rs:57-91`）: `Route` に variant、`label`、`route_of`、`handle` の `match`。`guard`（`server.rs:573-612`）が
  Host を検査し、`route_of` に無い POST は 403。CSRF は `post.rs:33-46`・`csrf.rs:65`（JSON の Content-Type、Origin と Host の
  一致）。本文は `Content-Length` 必須・64 KiB 上限（`post.rs:29`）。`writer_or_unavailable`（`update.rs:86-88`）が
  `INGEST=0` で 503。`setflag` は `update.rs:121-169`。
- GET の登録: `server.rs:677` の `site` の中で、完全一致の判定（`/api/admin/config`、`server.rs:736`）を前方一致より前に
  置き、`labeled(...)` でログのラベルを付ける。JSON は `json_response`（`server.rs:501`）で `Cache-Control: no-store`。

### 画像の配信

- `/derivatives/`・`/photos/` は `statics::serve_data`（`statics.rs:312-360`）。領域とファイルを `canonicalize` し、外に出る
  リンクは 403、同時配信の枠が無ければ 503。`file_response`（`statics.rs:170`）が `Content-Type`・`Content-Length`・
  `Last-Modified`・`ETag` と 304 を扱う。**`Cache-Control` は付かない**（`server.rs:617`）。
- 表示用 derivative は長辺 `DERIV_PX = 2560`・品質 85（`imaging/derivative.rs:36-38`）。`make_derivative`（`:937`）→
  `render`（`:600`）→ `target_dims`（`:366`、公開）→ `resize`（`:392`、**非公開**、`fast_image_resize` CatmullRom）。
  ファイル名は `<drive_id>.jpg` / `<drive_id>.r<rotation>.jpg`（`:83-91`）。書き込みは `lock_derivatives()`（`:734`）で直列化。
- データディレクトリ（`datadir.rs:60-196`）: `catalog.db`、`derivatives/`、`photos/`、`blog_exports/`、`tmp/`、`tile_cache/`。

### 画面

- タブ: `admin.html:25-203` の `button[data-tab]` と `section.tab-panel#tab-*`。`admin.js:56-79` の `initialTab()`・
  `showTab(name)`（写真なら `loadPhotos()`、設定なら `loadConfig()`、離れるとき `photos.js` の `onTabLeave()`）。
  `boot()`（`admin.js:315-326`）。DOM は `el(tag, cls, text)`（`admin.js:14`）と `textContent` で組む。
  `innerHTML` 等は `tests/static_files.rs:479-491` が禁止。
- 写真タブ（`photos.js`）: 状態 `ps`（`filter`・`page`・`view`・`query`・`includeHidden`・`mode`・`cards`・`pending`・
  `entries`）。`cardFor(photo, index, groupUi)`（`:126`）。★・除外は `setFlag` → `POST /api/admin/setflag` →
  `applyPhoto(id, patch)`（`:602-611`）が `ps.entries` の同じ写真とカード、開いているビューアを更新。
- ビューア（`admin.html:163-203`、`photos.js:486-611`）: **`ps.entries` と `viewerIndex` を直接読む**（`renderModal`、
  `:508`。前後は `:595` で周回）。★・除外のボタンを持つ（`#viewer-favorite`・`#viewer-hidden`）。`#viewer-ai` は一覧から
  開いたとき隠す。
- 地図: 位置指定モーダルは **MapLibre GL 4.7.1**（`/vendor/` に同梱、`admin.html:21-23`）。`ensureMini()`（`photos.js:641`）が
  `/tiles/osm/{z}/{x}/{y}.png`・`/tiles/gsi/…` のラスタ source と出典の定数で style を組む。
- 描画試験: `web/tests/fakedom.mjs`（要素・属性・クラス・`querySelector`・イベント。`canvas`・`IntersectionObserver` は無い）、
  `photos_render.mjs`・`config_render.mjs`（`fetch` を URL ごとの合成 JSON に差し替え、`test('(…)', …)` の形）。
  Rust 側の `tests/admin_ui_photos.rs`・`admin_ui_config.rs` が Node で走らせる（CI は Node 必須）。

### 試験の道具

- `tests/common/mod.rs`: `TempDir::new`（:154）、`build_admin_catalog`（:380、`tests/fixtures/admin-api-catalog.sql` から
  合成 DB と空の `derivatives/*.jpg`）、`spawn_with_env`（:397）、`get`/`request`（:281, :307）。
- フィードの契約: `tests/photo_feed_contract.rs`・`photo_feed_endpoint.rs`・`photo_feed_miharashi.rs`。
- `schema.rs` の `SCHEMA` は固定の基準と比べる試験がある（`fresh_database_matches_the_python_one`）。AI の表は別の定数で
  作り（`ai.rs:80` の `missing_tables`、`ai/scene_judge.rs` の `SCHEMA`）、無くても読む側が動く（`db.rs:62` の
  `REQUIRED_SCHEMA` の考え方）。

## 方針

1. **人の選択は写真ごとに、内容のハッシュを鍵に持つ**（`photo_curation`）。`photo_quality` の `group_id` に紐付けない
   （再構築で消える・振り直される）。「グループ G の代表」ではなく「写真 X は、X が属するグループの代表」と記録すれば、
   再構築で組み替わっても意味が保たれる。確認済みも写真ごとの印（`reviewed`）にし、「今のメンバー全員に有効な印がある」
   ことを確認済みの定義にする。これで「新しい写真が加わる → 再確認」が表の設計から自然に出る（R15）。
   - 選ばなかった案: `photo_quality` に `human_pick` 列を足して再構築時に引き継ぐ。再構築が「特徴量から作り直す」純粋さを
     失い、`quality-scan` の変更が要る。グループの同一性（メンバー集合のハッシュ）を鍵にする案は、1 枚増えただけで記録が
     全部無効になり、「再確認」と「未確認」の区別がつかない。
2. **フィードは「有効な推奨」で判定する。** `QUALITY_JOIN` の隣に `photo_curation` を LEFT JOIN し、グループに有効な
   人の代表が 1 枚でもあればその写真だけ、無ければ解析の `recommended` を使う。契約の項目は変えず、値の由来を文書に
   足す（R17・R19）。表が無い DB では JOIN を省く SQL に切り替える（R23）。
   - 選ばなかった案: 人の選択を管理画面だけに留めて miharashi に効かせない（ユーザーの決定で不採用）。選択を
     `photos.hidden` に落とす（除外は 1 枚ずつ、という決定に反する。exact の推奨外は `hidden` でなく推奨で落ちている）。
3. **サムネイルは要求時に作ってファイルに貯める**（`/thumb/<id>.jpg?v=<世代>`）。同期の段を足さず、バックログも持たない。
   世代は `display`・`md5`・`mtime` から作るので、回転・原本の更新で URL が変わり、ブラウザのキャッシュを長くできる。
   - 選ばなかった案: 同期の段で全枚数を前もって作る（C の運用コンソールと合わせて後から足せる。今回は要らない）。
     `/derivatives/` に別の大きさを足す（契約の対象で、`file` の形が固定）。
4. **新しい項目は新しい API に置く。** `/api/admin/photos`・`/status` は Python 版との一致の試験があるので触らない
   （R18 の件数だけ `db` に足し、`python_shape` で除く）。
5. **ビューアを「開いた側が渡した配列」で動かす**よう `photos.js` を小さく直し、ライブラリと選別から同じビューアを使う。
   カード（`cardFor`）はライブラリでは使わず、軽いタイル（サムネイルだけ）にする。
   - 選ばなかった案: ライブラリ用のビューアを別に作る（★・除外・前後・キー操作が二重になる）。
6. **旅はサーバーで区切る。** フィードの `assign_trips` と同じ規則（48 時間超）を、除外や位置なしを含む全写真に当てる。
   旅の識別子は先頭の写真の撮影日時（`YYYY-MM-DD HH:MM`）にする（番号は写真が増えるとずれる）。対象の写真が違うので、
   フィードの `trip` とは境界が違ってよい（位置なしの写真が間にあると、ライブラリでは 1 つの旅につながる）。意図した差。
7. 地図の点は **1 回の GET で GeoJSON を返し、クラスタは MapLibre に任せる**（6,500 点なら 1 点 20 バイト程度）。
   上限（20,000 点）を超えたら新しい順に切って `truncated: true`。

## 構成

```mermaid
flowchart LR
  subgraph browser[管理画面（素の JS）]
    admin[admin.js タブ・el()]
    photos[photos.js 写真タブ・ビューア・位置モーダル]
    library[library.js ライブラリ（旅・地図）]
    curation[curation.js 選別]
  end
  subgraph server[geoframe serve]
    thumb[thumb.rs GET /thumb/<id>.jpg]
    libapi[admin_library.rs GET /api/admin/library/*]
    curapi[admin_curation.rs GET/POST /api/admin/curation/*]
    curmod[curation.rs 表・SQL 片・有効な推奨]
    feed[photos.rs list_photos]
  end
  library --> libapi
  library -. 開く .-> photos
  curation --> curapi
  curation -. 開く .-> photos
  library & curation & photos --> thumb
  curapi --> curmod
  feed --> curmod
  thumb --> deriv[(derivatives/ photos/)]
  thumb --> thumbs[(thumbs/)]
  curmod --> db[(catalog.db photo_curation)]
```

### モジュール（Rust）

| ファイル | 役割 |
|---|---|
| `crates/geoframe/src/thumb.rs`（新） | `/thumb/<id>.jpg` の経路。ID の検証、世代の計算、キャッシュの有無、生成（`spawn_blocking` ＋ `Semaphore(2)`）、`file_response` で返す |
| `imaging/derivative.rs` | `pub fn make_thumbnail(src: &Path, dst: &Path, long_edge: u32) -> Result<()>` を足す（既存の `render`/`resize` を使う。JPEG 品質 80、EXIF は持ち込まない。tmp に書いて rename） |
| `admin_library.rs`（新） | `GET /api/admin/library/trips`・`/photos`・`/points`（あると良い: `/calendar`・`/places`）。`AdminContext` を再利用 |
| `curation.rs`（新） | `photo_curation` の DDL（`TABLES` 定数）、`ensure_tables(conn)`（書き込み側の起動時）、`available(conn)`（`sqlite_master` を見る）、SQL 片 `CURATION_JOIN`・`EFFECTIVE_RECOMMENDED`・`QUALITY_VISIBLE_EFFECTIVE`、グループの状態の計算 |
| `admin_curation.rs`（新） | `GET /api/admin/curation/groups`、`POST /api/admin/curation/confirm`・`/reset` の処理 |
| `photos.rs` | `list_photos` が `curation::available` で SQL 片を選ぶ。`quality_recommended`・`quality_role`・`QUALITY_VISIBLE` を有効な推奨に置き換える |
| `post.rs` | `Route::CurationConfirm`・`Route::CurationReset` |
| `server.rs` | `/thumb/` の分岐（`/derivatives/` の隣）、`/api/admin/library/`・`/api/admin/curation/` の完全一致の分岐 |
| `admin.rs` | `status` の `db.quality` に `curation_pending`（あると良い R18） |

### モジュール（画面）

| ファイル | 役割 |
|---|---|
| `web/static/admin.html` | ナビに `ライブラリ`・`選別`、`#tab-library`（切替・旅の一覧 `#lib-trips`・格子 `#lib-grid`・地図 `#lib-map`・地図の吹き出し）、`#tab-curation`（絞り込み・段の一覧 `#cur-groups`・ページング）。`<script>` は `admin.js` → `photos.js` → `library.js` → `curation.js` → `config.js` |
| `web/static/js/photos.js` | ビューアの共通化（下記）。`applyPhoto` の変更通知 |
| `web/static/js/library.js`（新） | 状態 `ls`、旅の一覧、格子（ページ）、地図（MapLibre の cluster source）、「表示範囲の写真」 |
| `web/static/js/curation.js`（新） | 状態 `cs`、段の描画、選択と確定・取り消し、409 の読み直し |
| `web/static/admin.css` | `.lib-*`・`.cur-*`。既存の変数と `.tags`・`.chip` を使う |

### ビューアの共通化（`photos.js`）

- 新しい内部状態 `viewer = {entries: [], index: -1, opts: {ai: false, onChange: null}}`。`renderModal`・前後移動・キー操作・
  `closeViewer` は `viewer.entries` を読む。写真タブは今までどおり `openViewer(index)` を呼び、その中で
  `viewer.entries = ps.entries` を設定する（挙動は不変。既存の描画試験 (p) が担保）。
- 公開関数 `openViewerFor(photos, index, opts)`: `photos` は `{id, name, thumb, thumb_url, taken_at, lat, lon, place, pref,
  city, favorite, hidden, rotation, source}` の配列。`entries` は `{photo, group: null}` に包む。`opts.ai=false` で
  `#viewer-ai`・`#viewer-group` を隠す。
- `applyPhoto(id, patch)` は、`ps.entries`・開いているビューアに加えて、登録された購読者 `photoChangeListeners`
  （`library.js`・`curation.js` が `onPhotoChange(fn)` で登録）に `(id, patch)` を渡す。購読者は自分のタイルを描き直す。
- `onTabLeave()` はタブに依らず呼ばれるようにし（`showTab` が「前のタブ」に対して呼ぶ）、ビューア・位置モーダルを閉じる。

### ライブラリの状態（`library.js`）

```
ls = {
  view: 'trips' | 'map' | 'calendar' | 'places',   // 必須は trips と map
  includeHidden: false,
  trips: [],            // GET /api/admin/library/trips の結果
  sel: null,            // 選んだ絞り込み {from, to, bbox, pref, city, label}
  page: 1, per: 60, total: 0, items: [], tiles: [],   // 格子
  map: null,            // maplibregl.Map（地図ビューを初めて開いたとき作る）
  pending: 0,           // 要求番号（古い応答を捨てる。photos.js と同じ流儀）
}
```

- 旅の一覧: 1 旅 1 行（表紙のサムネイル、`start〜end`、`N 枚（位置なし M）`、場所の上位 3 つ、「地図で見る」）。
  押すと `sel = {from: start, to: end}` で格子を読む。末尾に「日時なし（K 枚）」。
- 格子: `div.lib-tile > img[loading=lazy][src=thumb_url]` ＋ 右下に ★・🚫・位置なしの小さな印。押すと
  `openViewerFor(ls.items, i, {ai: false})`。ページングは写真タブと同じ部品。
- 地図: `ensureLibMap()` が `ensureMini()` と同じ style の組み方（source・出典の定数を `photos.js` から共有関数
  `tileStyle()` に切り出す）で `#lib-map` に作る。`photos` という GeoJSON source（`cluster: true, clusterRadius: 40,
  clusterMaxZoom: 16`）と 3 層（クラスタの円、件数の文字、単独の点）。クラスタを押すと
  `getClusterExpansionZoom` で寄る。点を押すと `maplibregl.Popup`（DOM を `el()` で組んで `setDOMContent`）に
  サムネイル・日時・地名・「開く」。「開く」は `GET /api/admin/library/photos?ids=<id>` で 1 枚を取り
  `openViewerFor`。ツールバーに「この範囲の写真を見る」（`map.getBounds()` を `bbox` にして格子を読む）と期間の表示
  （旅から来たときの `from`/`to`、解除ボタン）。
- 描画試験では `window.maplibregl` を偽物（`Map`・`Popup`・`addSource`・`getSource().setData` を記録するだけ）に
  差し替え、source に渡った GeoJSON と `bbox` 付きの要求を検証する。

### 選別の状態（`curation.js`）

```
cs = {
  state: 'unconfirmed' | 'confirmed' | 'all',   // 既定 unconfirmed（再確認を含む）
  kind: 'all' | 'exact' | 'similar',
  page: 1, per: 20, total: 0, groups: [],
  picks: Map<groupKey, Set<photoId>>,            // 段ごとの選択（確定前）
  pending: 0,
}
```

- 1 段 = `section.cur-group`: 見出し（`exact 3 枚 2024-05-03 09:12 ／ 状態: 未確認 | 確認済み（人の代表）| 再確認`）、
  メンバーの横並び（`label.cur-member > input[type=checkbox] + img + 点数 + タグ（解析の推奨・人の代表・★・除外）+
  理由/懸念の折りたたみ`）、操作（「選んだ写真を代表にする」= チェックが 1 つ以上で有効、「このままで確認済み」、
  「取り消す」= 記録があるときだけ）。画像を押すと `openViewerFor(group.members, i, {ai: false})`。
- 確定は `POST /api/admin/curation/confirm {photo_ids: [...全メンバー], representatives: [...] | null}`。応答の `group` で
  その段を描き直す。409 `group_changed` なら「組が変わりました。読み直します」のトーストを出し、その段だけ
  `GET …/groups?ids=` で読み直す。
- 状況タブの「要対応」に `未確認の選別 N 組` を足す（R18。`db.quality.curation_pending`）。

## インターフェース

### `GET /thumb/<id>.jpg?v=<gen>`（新。PR ①）

- `<id>` は 1〜18 桁の数字だけ。他は 404。`v` は無視してよい（キャッシュの破棄のためだけに URL に含める）。
- 世代 `gen` = `blake3`/`sha256` の先頭 12 hex（`display | md5 | mtime` を `|` で連結。`md5`・`mtime` が NULL なら空）。
  API の `thumb_url` は `/thumb/<id>.jpg?v=<gen>`。
- 処理: (1) DB で `display`・`drive_id`・`md5`・`mtime` を引く（`thumb_for` と同じ規則で元ファイルを決める。無ければ 404）。
  (2) `thumbs/<id>-<gen>.jpg` があれば `file_response`。(3) 無ければ `Semaphore(2)` を取り（待つ）、取った後にもう一度 (2)。
  `spawn_blocking` で `make_thumbnail(元, tmp/thumb-<id>-<gen>.jpg, 320)` → rename。同じ `<id>-` で始まる古いファイルを消す。
  (4) 生成に失敗（読めない画像・`thumbs/` に書けない）したら、元の表示用画像を `file_response` で返す（`Content-Type` は
  拡張子から。`Cache-Control` は `no-store`）。失敗した `<id>-<gen>` は `AppState` の `thumb_failed: Mutex<HashSet<String>>`
  に覚え、起動中は Semaphore を取らずにすぐ代わりを返す（壊れた画像が 2 つの枠を塞ぎ続けないため）。ログは失敗の初回に
  1 行（WARN、ID と理由だけ。パスは出さない）。
- 応答ヘッダ: `Content-Type: image/jpeg`、`ETag`・`Last-Modified`（`file_response` のまま）、
  `Cache-Control: private, max-age=604800`（URL に世代があるので長くてよい）。
- 元ファイルの検査は `serve_data` と同じ（`canonicalize` してデータディレクトリの中であること）。パスは DB の `display` と
  `derivatives/<drive_id>.jpg` からだけ作る。
- 定数: `THUMB_PX = 320`、`THUMB_QUALITY = 80`、`THUMB_CONCURRENCY = 2`。

### `GET /api/admin/library/trips`（新。PR ②）

- 引数: `hidden=1` で除外を含む（既定は含まない）。
- 応答:

```json
{"ok": true,
 "trips": [{"key": "2024-05-03 09:12", "start": "2024-05-03 09:12", "end": "2024-05-05 18:40",
            "count": 212, "noloc": 7,
            "places": [{"pref": "京都府", "city": "京都市", "count": 150}, {"pref": "滋賀県", "city": "大津市", "count": 40}],
            "cover": {"id": 1234, "thumb_url": "/thumb/1234.jpg?v=0f3a…"}}],
 "untimed": 12}
```

- 旅は `start` の降順。`cover` は旅の中で位置があり除外でない最初の写真（無ければ先頭）。`places` は `pref`・`city` の
  組の件数の上位 3 つ（`pref` が空なら `{"pref": "", "city": "", …}` で「海外・不明」）。
- 実装: `SELECT id, taken_at, lat, lon, pref, city, hidden, display, drive_id, md5, mtime FROM photos WHERE taken_at IS NOT NULL
  AND taken_at!='' [AND hidden=0] ORDER BY taken_at, id` を 1 回読み、Rust で 48 時間超の空きで区切る（`photos.rs` の
  `parse_epoch`・`TRIP_GAP_SECONDS` を `pub(crate)` にして共有）。

### `GET /api/admin/library/photos`（新。PR ②）

- 引数: `from`・`to`（`YYYY-MM-DD HH:MM`。両端を含む）、`bbox=minlon,minlat,maxlon,maxlat`、`ids=1,2,3`（最大 200）、
  `pref`・`city`（R10）、`untimed=1`（日時なし）、`hidden=1`、`page`（既定 1）、`per`（既定 60、1〜200）。
  絞り込みが 1 つも無ければ 400 `bad_params`。日時・数値の形が違えば 400。
- 応答: `{ok, page, per, pages, total, items: [LibraryPhoto]}`。
  `LibraryPhoto = {id, name, thumb, thumb_url, taken_at, lat, lon, loc_source, place, pref, city, favorite, hidden, rotation, source}`
  （`thumb` は `thumb_for` と同じ表示用画像。ビューアが使う）。並びは `taken_at` 昇順（空は最後）、`id` 昇順。

### `GET /api/admin/library/points`（新。PR ②）

- 引数: `from`・`to`、`hidden=1`。
- 応答: `{ok, truncated, total, type: "FeatureCollection", features: [{type: "Feature", geometry: {type: "Point",
  coordinates: [lon, lat]}, properties: {id}}]}`。`lat`・`lon` の両方がある写真だけ。上限 `POINTS_MAX = 20000`
  （`taken_at` の新しい順に切る）。

### あると良い（PR ② の時間があれば）

- `GET /api/admin/library/calendar` → `{ok, years: [{year, count, months: [{month, count}]}], untimed}`
  （`substr(taken_at,1,4)`・`substr(taken_at,1,7)` で GROUP BY）。
- `GET /api/admin/library/places` → `{ok, prefs: [{pref, count, cities: [{city, count}]}], abroad: n, noloc: n}`。

### `photo_curation` 表（新。PR ③。`curation::TABLES`。`SCHEMA` 定数には足さない）

```sql
CREATE TABLE IF NOT EXISTS photo_curation(
  photo_id INTEGER PRIMARY KEY,
  content_sha256 TEXT NOT NULL,                 -- 記録したときの写真の内容（photo_quality.content_sha256）
  role TEXT NOT NULL CHECK(role IN ('representative','reviewed')),
  decided_at TEXT NOT NULL                      -- 'YYYY-MM-DD HH:MM:SS'（UTC）
);
```

- 有効な記録 = `photo_quality q`（世代の合う行）と `photo_id` と `content_sha256` が一致する行。
- グループ G（`q.group_kind, q.group_id`）について:
  - `picks(G)` = 有効な `representative` のメンバー。
  - 有効な推奨 `eff(p)` = `picks(G)` が空でなければ `p ∈ picks(G)`、空なら `q.recommended=1`。
  - 状態: 全メンバーに有効な記録がある → `confirmed`。記録が 1 つも無い → `unconfirmed`。一部にだけある（新しい写真が
    加わった、世代が変わった）→ `recheck`。
- SQL 片（`curation.rs`）:

```sql
-- CURATION_JOIN（QUALITY_JOIN の後ろに付ける）
LEFT JOIN photo_curation c ON c.photo_id=q.photo_id AND c.content_sha256=q.content_sha256
LEFT JOIN (SELECT q2.group_kind, q2.group_id, COUNT(*) AS picks
             FROM photo_quality q2 JOIN photos p2 ON p2.id=q2.photo_id AND p2.display=q2.display
              AND p2.md5 IS q2.source_md5 AND p2.mtime IS q2.source_mtime
             JOIN photo_curation c2 ON c2.photo_id=q2.photo_id AND c2.content_sha256=q2.content_sha256
            WHERE c2.role='representative'
            GROUP BY q2.group_kind, q2.group_id) gp
       ON gp.group_kind=q.group_kind AND gp.group_id=q.group_id
-- EFFECTIVE_RECOMMENDED
CASE WHEN q.group_id IS NULL THEN 0
     WHEN COALESCE(gp.picks,0)>0 THEN COALESCE(c.role='representative',0)
     ELSE q.recommended END
-- QUALITY_VISIBLE_EFFECTIVE
(q.group_kind IS NULL OR q.group_kind!='exact' OR <EFFECTIVE_RECOMMENDED>=1)
```

- 表が無いとき（`available(conn)=false`）は従来の SQL 片をそのまま使う。`Catalog::read` の接続ごとに `available` を
  1 回引く（`sqlite_master` の 1 行）。書き込み側は起動時に `ensure_tables`（`ai.rs` の AI 表と同じ場所）。

### `GET /api/admin/curation/groups`（新。PR ③）

- 引数: `state=unconfirmed|confirmed|all`（既定 `unconfirmed`。`unconfirmed` は `recheck` を含む）、
  `kind=all|exact|similar`、`ids=<photo_id,…>`（そのグループだけ。段の読み直し用）、`page`、`per`（既定 20、1〜50）。
- 応答:

```json
{"ok": true, "page": 1, "per": 20, "pages": 3, "total": 41,
 "groups": [{"kind": "similar", "group_id": 17, "state": "recheck", "taken_at": "2024-05-03 09:12", "count": 3,
   "members": [{"id": 101, "name": "IMG_0001.jpg", "thumb": "/derivatives/…", "thumb_url": "/thumb/101.jpg?v=…",
                "taken_at": "2024-05-03 09:12", "place": "…", "pref": "…", "city": "…", "lat": 35.0, "lon": 135.7,
                "favorite": false, "hidden": false, "rotation": 0, "source": "drive",
                "rank": 1, "score": 78.5, "reasons": ["…"], "issues": [],
                "recommended": true, "effective": true, "curation": "representative" | "reviewed" | "stale" | null}]}]}
```

- グループの並び: `taken_at`（グループの最小）の降順、`group_kind`、`group_id`。世代の合わない `photo_quality` の行と
  1 枚しか有効でないグループは出さない。`curation: "stale"` は記録はあるが `content_sha256` が合わないメンバー。

### `POST /api/admin/curation/confirm`（新。PR ③）

- 本文: `{"photo_ids": [int…], "representatives": [int…] | null}`。`photo_ids` は 2〜200 件、重複なし。`representatives` は
  `photo_ids` の部分集合で 1 件以上、または `null`（人の代表を持たず、解析の推奨のまま確認済み）。
  画面の「このままで確認済み」は、段に有効な人の代表があればそれを `representatives` に入れて送る（黙って解析の推奨に
  戻さない）。
- 代表の検査: `representatives` の各写真は `hidden=0` かつ `lat`・`lon` がある（フィードに出せる）こと。違えば 400
  `{"ok": false, "error": "representative_not_visible"}`。画面は除外・位置なしのメンバーのチェックボックスを無効にする。
- 検査（1 トランザクション、`writer`）: `photo_ids` の全部が世代の合う `photo_quality` に居て、同じ `(group_kind, group_id)`
  で、そのグループのメンバーが `photo_ids` と過不足なく一致する。違えば 409 `{"ok": false, "error": "group_changed"}`。
- 書き込み: `DELETE FROM photo_curation WHERE photo_id IN (…)` → 各メンバーに `role`（代表なら `representative`、他は
  `reviewed`）と `content_sha256`・`decided_at` を INSERT。
- 応答: `{ok: true, group: <groups と同じ 1 組>}`。
- 失敗: 本文の形が違えば 400 `bad_params`、`INGEST=0` は 503、CSRF は 403（既存）。

### `POST /api/admin/curation/reset`（新。PR ③）

- 本文: `{"photo_ids": [int…]}`（同じ検査）。`DELETE` だけ。応答は `confirm` と同じ。

### `GET /api/admin/status` の変更（PR ③）

- `db.quality.recommended`・`exact_suppressed`・`similar_alternates`（`quality_summary`、`admin.rs:335`）を
  `EFFECTIVE_RECOMMENDED` で数える（R19）。人の記録が無い Python のフィクスチャでは値が変わらないので `admin_api.rs` は通る。
- `db.quality.curation_pending`（あると良い R18）: 状態が `unconfirmed`・`recheck` のグループ数。`python_shape` で除く。

### `GET /api/v1/photos`（PR ③。項目は不変）

- `quality_recommended` と `quality_role`、exact の除外条件を `EFFECTIVE_RECOMMENDED` で計算する。人の記録が無い DB では
  値も不変。契約の文書と `photo-feed.schema.json`（`description` だけ）の `quality_recommended` に「管理画面で人が代表に
  選んだ写真があるグループでは、その写真だけが `true`（解析の推奨は使わない）」を足す。型・`required` は変えない。
- 人の代表は `confirm` で「出せる写真」に限っているが、確定後に除外・位置の削除で出せなくなることはある。そのときは
  解析の推奨と同じく、exact グループが一覧から消える（今の挙動と同じ穴。選別タブで `recheck` にはならないので、
  リスクの表に書く）。

## データ

- `thumbs/`（データディレクトリ直下。`datadir.rs` に項目を足す）。1 枚 15〜30 KB、6,500 枚で 200 MB 弱。古い世代は
  再生成時に消す。消しても次の要求で作り直せる（バックアップの対象外。`backup-catalog.sh` は触らない）。
- `photo_curation`: 書き込み側（`serve` の writer）が起動時に作る。移行は無い。`INGEST=0` の読み取り専用の DB では
  作らず、読む側が無いものとして動く。
- 旅・集計は毎回 SQL から計算する（6,500 行で 10 ms 台。キャッシュは作らない）。

## 安全面

- 入力の検証: `/thumb/` の ID は数字だけ。ライブラリの `from`/`to` は 16 文字の固定形式、`bbox` は有限の数 4 つで範囲内、
  `ids` は整数 200 件まで、`pref`/`city` は 64 文字まで（SQL はすべてバインド）。`confirm` の本文は整数の配列だけ。
- 書き込みは既存の `post.rs` の経路（Host → メソッド → `route_of` → CSRF → 64 KiB → JSON → `writer_or_unavailable`）。
  新しい POST は 2 本だけ、どちらも `photos`・`photo_quality` の存在を検査して 1 トランザクションで書く。
- 権限: 認証は無い（LAN 内、ADR 0008）。新しい API が出す情報（撮影地の座標・地名・サムネイル）は既存の
  `/api/admin/photos` と `/derivatives/` が既に出しているものと同じ範囲。`points` は全写真の座標を 1 回で返すが、
  `/api/admin/photos?per=200` を回せば同じものが取れるので、露出の範囲は変わらない。
- ファイル: サムネイルの読み元はデータディレクトリの中に限り（`canonicalize`）、書き先は `thumbs/` と `tmp/` だけ。
  ファイル名は `<id>-<gen>.jpg` で固定。
- 外部への通信: 無し（地図タイルは既存のプロキシ）。秘密情報は扱わない。ログにパス・座標を出さない。
- 資源: サムネイルの生成は同時 2 つ、1 枚 50〜150 ms 程度（2,560px の JPEG の復号が主）。初回の旅の一覧で 60 枚なら
  数秒で埋まる。`spawn_blocking` なので待ち受けは止まらない。

## テスト計画

| AC | 試験 | 置き場 |
|---|---|---|
| AC1 | 合成 JPEG（`image` crate で 1200×800 を作る）を `derivatives/` に置き、`/thumb/<id>.jpg` の 200・寸法・2 回目の ETag・`mtime` 変更後の再生成・404/403 の各ケース。`/derivatives/` のヘッダ不変は既存試験 | `tests/thumb.rs`（新） |
| AC2 | `admin-api-catalog.sql` の写真で `trips` の境界がフィードの `trip` と一致。描画試験で旅 → 格子 → ビューア → ★ → タイル更新 | `tests/library_api.rs`（新）、`web/tests/library_render.mjs`（新）＋ `tests/admin_ui_library.rs` |
| AC3 | `points` の位置なし除外・`from`/`to`・`truncated`（`POINTS_MAX` を試験用に小さくできる `#[cfg(test)]` の定数か、合成 DB に上限超の行）。`bbox` の絞り込み。描画試験で偽 MapLibre の `setData` と `bbox` 要求 | 同上 |
| AC4 | exact 1 組・similar 2 組（`photo_quality`・`photo_quality_features` を SQL で投入）に `confirm` → `groups`・`/api/v1/photos`・`photo_quality` の入れ直し後も保持。`reviewed` のみ・`reset` | `tests/curation_api.rs`（新） |
| AC5 | 確認済みの組に 1 枚足す → `recheck`。`mtime` 変更 → `recheck`・`stale`。集合違いの `confirm` → 409 で表が不変 | 同上 |
| AC6 | 既存のフィードの試験（契約・endpoint・miharashi）が無変更で通る。`photo_curation` 無しの DB で `list_photos` の SQL が従来と同じ結果（単体） | 既存 ＋ `photos.rs` の単体試験 |
| AC7 | CSRF 403・`INGEST=0` 503・表の無い DB で各 GET が 200・新しい GET への POST が 403 | `tests/curation_api.rs`・`tests/library_api.rs` |
| AC8 | `cargo fmt --check`・clippy・`cargo test`。`static_files.rs` の文字列 HTML の禁止が `library.js`・`curation.js` にも効く（`web/static/js` 直下に置く） | CI |
| AC9 | 年 → 月、都道府県 → 市区町村の描画。`curation_pending` と `groups?state=unconfirmed&per=1` の `total` の一致 | `library_render.mjs`・`tests/curation_api.rs` |

描画試験の偽 MapLibre は `web/tests/fake_maplibre.mjs` に置き、`Map`（`on`・`addSource`・`addLayer`・`getSource`・
`getBounds`・`getClusterExpansionZoom` は固定値）と `Popup`・`NavigationControl` だけを持つ。

## リスクと対策

| リスク | 対策 |
|---|---|
| 人の代表が `similar` グループで 1 枚だけになり、解析の「上位 2 枚」より miharashi に出る枚数が減る | 意図した挙動として文書に書く。複数選べるので必要なら 2 枚選ぶ |
| 確定後に代表を除外した・位置を外したため、exact グループがフィードから消える | 解析の推奨と同じ既存の穴。選別タブの段に「代表が出せない」の警告を出す（`groups` の応答で `effective` かつ `hidden`/位置なしのメンバーがあれば `warning: "representative_not_visible"`） |
| 回転・原本の更新で記録が無効になり、確認し直しになる | `recheck` として目に見える形で戻す（黙って消さない）。`stale` の印で理由が分かる |
| `quality-scan` の再構築中（`DELETE` と INSERT の間）に `groups` を読む | 再構築は 1 トランザクションなので途中は見えない（`quality.rs:345-`）。`confirm` は writer のトランザクションで検査するので競合しても 409 になるだけ |
| サムネイルの初回生成で一覧が遅い | `loading=lazy` と同時 2 つの生成。60 枚/ページ。必要なら後で同期の段に前もって作る処理を足す（C） |
| `thumbs/` がディスクを圧迫する | 200 MB 弱の見積もりを文書に書く。古い世代は再生成時に消す |
| `list_photos` の SQL が重くなる（サブクエリの JOIN） | `photo_quality` に `(group_kind, group_id)` の索引が既にある。6,500 行で測って 20 ms を超えるなら `gp` を一時表にする |
| ビューアの共通化で写真タブの挙動が変わる | 既存の `photos_render.mjs` の (p)(q)(q2) をそのまま通す。`openViewer(index)` の外部の挙動は変えない |
| 偽 MapLibre では地図の見た目を検証できない | A と同じく検証環境で目視。描画試験は source に渡るデータと要求の URL を検証する |
| `tests/admin_api.rs` の完全一致が落ちる | `status` に足す項目は `python_shape` で除く。`/api/admin/photos` は触らない |

## 実装の順番

### PR ①: サムネイル（R1〜R4、R21）

1. `imaging/derivative.rs` に `make_thumbnail`（`render`/`resize` の再利用。単体試験: 寸法と向き）。
2. `thumb.rs`: 経路・世代・キャッシュ・Semaphore・フォールバック。`server.rs` に `/thumb/` の分岐。`datadir.rs` に `thumbs/`。
3. `tests/thumb.rs`（AC1）。`docs/architecture/photo-pipeline.md` にサムネイルの節（配信の一覧がある文書）。
4. （R4）`AdminPhoto`（`admin.rs`）と検索 API の写真に `thumb_url` を足し、`tests/admin_api.rs` の `python_shape` で
   `hint` と同じく除く。カードの `img.src` を `thumb_url`（無ければ `thumb`）にする。描画試験の合成データに `thumb_url` を
   足すだけで、カードの DOM と検証内容は変えない。

### PR ②: ライブラリ（R5〜R8。R9〜R11 は時間があれば）

1. `photos.rs` の `parse_epoch`・`TRIP_GAP_SECONDS` を `pub(crate)`。`admin_library.rs` の 3 本（＋あると良い 2 本）と結合試験。
2. `photos.js` のビューアの共通化（`openViewerFor`・`onPhotoChange`・`tileStyle()`）。既存の描画試験が通ることを先に確認。
3. `admin.html`・`admin.css`・`library.js`（旅 → 格子 → ビューア、地図）。`admin.js` のタブ登録。
4. `library_render.mjs` ＋ `fake_maplibre.mjs` ＋ `tests/admin_ui_library.rs`。
5. 検証環境で地図の目視（クラスタ・吹き出し・範囲の絞り込み）。

### PR ③: 選別の確認（R12〜R19）

1. `curation.rs`（表・`ensure_tables`・`available`・SQL 片）と `photos.rs` の置き換え。フィードの既存試験が通ることを確認。
2. `admin_curation.rs`（`groups`・`confirm`・`reset`）、`post.rs`・`server.rs` の登録、`tests/curation_api.rs`（AC4・AC5・AC7）。
3. `curation.js`・`admin.html`・`admin.css`、状況タブの件数（R18）、`curation_render.mjs` ＋ `tests/admin_ui_curation.rs`。
4. 契約の文書（R19）、`docs/architecture/photo-pipeline.md` の管理 API の一覧、`docs/admin-library/` の「実装との差」。
