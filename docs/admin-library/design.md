# 写真ライブラリと選別の確認（B） 設計

作成日: 2026-11-01。状態: 設計レビュー 2 回（Sol）・安全面レビュー 2 回（Opus）を反映済み。ユーザー承認待ち。PRD: [prd.md](prd.md)。

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
  リンクは 403、同時配信の枠が無ければ 503。`file_response`（`statics.rs:170`、非公開）が `Content-Type`・`Content-Length`・
  `Last-Modified` を付け、`If-Modified-Since` の 304 は `serve_data` 側。データ画像には ETag を渡していない（`etag: None`）。**`Cache-Control` は付かない**（`server.rs:617`）。
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

1. **人の選択は写真ごとに、写真の世代（`display`・`md5`・`mtime`・内容の SHA）を鍵に持つ**（`photo_curation`）。
   `photo_quality` の `group_id` に紐付けない（再構築で消える・振り直される）。「グループ G の代表」ではなく「写真 X は、X が属するグループの代表」と記録すれば、
   再構築で組み替わっても意味が保たれる。確認済みも写真ごとの印（`reviewed`）にし、「今のメンバー全員に有効な印がある」
   ことを確認済みの定義にする。これで「新しい写真が加わる → 再確認」が表の設計から自然に出る（R15）。
   - 選ばなかった案: `photo_quality` に `human_pick` 列を足して再構築時に引き継ぐ。再構築が「特徴量から作り直す」純粋さを
     失い、`quality-scan` の変更が要る。グループの同一性（メンバー集合のハッシュ）を鍵にする案は、1 枚増えただけで記録が
     全部無効になり、「再確認」と「未確認」の区別がつかない。
2. **フィードは「有効な推奨」で判定する。** `QUALITY_JOIN` の隣に `photo_curation` を LEFT JOIN し、グループに有効な
   人の代表が 1 枚でもあればその写真だけ、無ければ解析の `recommended` を使う。契約の項目は変えず、値の由来を文書に
   足す（R17・R19）。表が無い DB では JOIN を省く SQL に切り替える（R23）。旧 AI 選別（`selection.rs`）の「表示できる
   推薦写真」の判定も有効な推奨で行い、人の代表は `favorite` と同じく旧 AI の抑止を免れる（人の判断を機械の判断より
   優先する。Sol の指摘 3）。
   - 選ばなかった案: 人の選択を管理画面だけに留めて miharashi に効かせない（ユーザーの決定で不採用）。選択を
     `photos.hidden` に落とす（除外は 1 枚ずつ、という決定に反する。exact の推奨外は `hidden` でなく推奨で落ちている）。
3. **サムネイルは要求時に作ってファイルに貯める**（`/thumb/<id>.jpg?v=<世代>`）。同期の段を足さず、バックログも持たない。
   世代は `display`・`md5`・`mtime` から作るので、回転・原本の更新で URL が変わり、ブラウザのキャッシュを長くできる。
   - 選ばなかった案: 同期の段で全枚数を前もって作る（C の運用コンソールと合わせて後から足せる。今回は要らない）。
     `/derivatives/` に別の大きさを足す（契約の対象で、`file` の形が固定）。
4. **新しい項目は新しい API に置く。** `/api/admin/photos`・`/status` は Python 版との一致の試験があるので触らない
   （R18 の件数だけ `db` に足し、`python_shape` で除く）。
5. **ビューアを「開いた側が渡した配列」で動かす**よう `photos.js` を小さく直し、ライブラリと選別から同じビューアを使う。
   `viewerIndex` の名前と `openModal(index)` の外部の挙動は変えず（描画試験が `viewerIndex` を直接見る）、参照する配列だけを
   `viewerEntries`（既定は `ps.entries`）に置き換える。カード（`cardFor`）はライブラリでは使わず、軽いタイル（サムネイルだけ）にする。
   - 選ばなかった案: ライブラリ用のビューアを別に作る（★・除外・前後・キー操作が二重になる）。
6. **旅はサーバーで区切る。** フィードの `assign_trips` と同じ規則（48 時間超）を、除外や位置なしを含む全写真に当てる。
   旅の識別子は先頭の写真の撮影日時（`YYYY-MM-DD HH:MM`）にする（番号は写真が増えるとずれる）。対象の写真が違うので、
   フィードの `trip` とは境界が違ってよい（位置なしの写真が間にあると、ライブラリでは 1 つの旅につながる）。意図した差。
7. 地図の点は **1 回の GET で GeoJSON を返し、クラスタは MapLibre に任せる**（6,500 点なら 1 点 20 バイト程度）。
   上限（20,000 点）を超えたら新しい順に切って `truncated: true`。点の中身は ID だけで、吹き出しに要る情報は押した時点で
   `library/photos?ids=` を引く。

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
| `crates/geoframe/src/thumb.rs`（新） | `/thumb/<id>.jpg` の経路。ID と版の検証、世代の計算、キャッシュの有無、ID ごとのロックと生成の枠、`spawn_blocking` での生成、失敗の記憶、ETag と 304 |
| `imaging/derivative.rs` | `pub fn make_thumbnail(src: &Path, long_edge: u32, limits: &DecodeLimits) -> Result<Vec<u8>, ImageError>` を足す（`render` は `MAX_INPUT_BYTES` を決め打ちしている（`derivative.rs:602`）ので、上限を引数で受ける形に分ける。全体を復号する前にファイルの大きさとヘッダの寸法で断る。`resize` を再利用。JPEG 品質 80、EXIF は持ち込まない。書き込みは呼び手が `write_atomic`） |
| `admin_library.rs`（新） | `GET /api/admin/library/trips`・`/photos`・`/points`（あると良い: `/calendar`・`/places`）。`AdminContext` を再利用 |
| `curation.rs`（新） | `photo_curation` の DDL（`TABLES`・`SCHEMA`）、`ensure_tables(conn)`（書き込み側の起動時。`ingest::start_impl` が AI の表の次に呼び、`plan`（`--dry-run`）も `pending` で数える）、`available(conn)`（`sqlite_master` を見る）、SQL 片（別名を受ける関数 `join`・`group_has_pick`・`effective_recommended`・`quality_visible`・`has_valid_representative`。設計書の `CURATION_JOIN`・`GROUP_HAS_PICK`・`EFFECTIVE_RECOMMENDED`・`QUALITY_VISIBLE_EFFECTIVE` に当たる）、`pending_groups(conn)`、グループの状態 `group_state`・`State`・`Record`、`Member`・`group_token`・`mtime_text` |
| `admin_curation.rs`（新） | `GET /api/admin/curation/groups`、`POST /api/admin/curation/confirm`・`/reset` の処理 |
| `photos.rs` | `list_photos` が `curation::available` で `feed_sql(curation)` を選ぶ（偽なら従来と同じ文字列。単体試験で固定）。`quality_recommended`・`quality_role`・`QUALITY_VISIBLE` を有効な推奨に置き換える。`QUALITY_JOIN`・`FEED_BASE_WHERE`（品質以外の述語）は `pub(crate)` で、`confirm` の `representative_not_visible` が共有する |
| `selection.rs` | `VALID_MEMBERS` 定数を関数 `valid_members(curation)` にし、`ai_visible(curation)` が引数を受ける（`admin_counts` と `ai/groups/repository.rs` の `legacy_in` は `curation::available` を自分で引く） |
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

今の入口は `openModal(index)`（`photos.js:576`）・`closeModal()`（`:585`）・`stepModal(delta)`（`:595`）・
`closePhotoOverlays()`（`:786`。`admin.js:70` が写真タブ以外へ移るとき呼ぶ）・`openPhotosTab()`（`:246`。`admin.js:76`）。
`renderModal`（`:508`）、前後の先読み（`:567-572`）、★・除外のハンドラ（`:835-842`）、画像クリック（`:845`）、`applyPhoto`
（`:602-611`）が `ps.entries` と `viewerIndex` を直接読む。

- 新しい変数 `let viewerEntries = null;`。`viewerEntries || ps.entries` を返す `vEntries()` を置き、上の**全部の参照**
  （`renderModal`・`stepModal`・先読み・★/除外のハンドラ・画像クリック・`applyPhoto` の「開いている写真」の判定）を
  `vEntries()` に置き換える。`viewerIndex` の名前・意味は変えない（描画試験 `photos_render.mjs:846, 918, 968, 973, 1256` が
  直接見る）。`openModal(index)` は `viewerEntries = null` にしてから開く（写真タブの挙動は不変）。`closeModal()` で
  `viewerEntries = null`。写真タブの再描画（一覧・検索の応答、チップの切り替え、検索を閉じる）で閉じるのは
  `ps.entries` を使っているビューア（`viewerEntries === null`）だけにする（`closeModalIfOwn()`）。遅れて届いた写真タブの応答で、
  ライブラリ・選別から開いたビューアを閉じない。`closePhotoOverlays()` は今までどおり全部閉じる。
- 既存の `openModal(index)` のうち「ビューアを表示する処理」（`hidden` を外す、`body.ps-viewer-open`、`renderModal()`、
  閉じるボタンへのフォーカス、閉じたときのフォーカスの戻し先の記録）を内部関数 `showViewer(index)` に切り出し、
  `openModal` と下の `openModalFor` の両方から呼ぶ。
- 公開関数 `openModalFor(photos, index, opts)`: `photos` は `{id, name, thumb, thumb_url, taken_at, lat, lon, place, pref, city,
  favorite, hidden, rotation, source}` の配列。`viewerEntries = photos.map(photo => ({photo, group: null}))` にして
  `showViewer(index)`。`opts.ai=false` で `#viewer-ai`・`#viewer-group` を隠す（一覧から開いたときと同じ扱い）。
- `applyPhoto(id, patch)` は、`ps.entries`・`viewerEntries`・開いているビューアに加えて、購読者 `photoChangeListeners`
  （`library.js`・`curation.js` が `onPhotoChange(fn)` で登録）に `(id, patch)` を渡す。購読者は自分のタイルを描き直す。
- `closePhotoOverlays()` は今までどおり `admin.js` の `showTab` が写真タブ以外へ移るときに呼ぶが、ライブラリ・選別から
  開いたビューアも閉じる必要があるので、`showTab` は**どのタブから離れるときも**呼ぶ（`admin.js:70` の条件を外す）。
- 描画試験: 既存の (p)(q)(q2) をそのまま通す。追加で「`ps.entries` と違う配列から `openModalFor` で開くとビューアが表示され
  （`hidden` が外れ `body.ps-viewer-open`）、★を押すと `POST /api/admin/setflag` の `id` がその配列の写真になり、`ps.entries` の
  写真は変わらず、閉じると開いた要素にフォーカスが戻る」を足す（Sol の指摘 8）。

### ライブラリの状態（`library.js`）

```
ls = {
  view: 'trips' | 'map' | 'calendar' | 'places',   // 必須は trips と map。calendar・places は「あると良い」（PR ② で実装）
  grid: false,          // true の間は、どちらの view の上にも格子を出す（戻るで元の view へ）
  includeHidden: false,
  trips: [], untimed: 0,   // GET /api/admin/library/trips の結果
  years: [], calUntimed: 0, prefs: [], abroad: 0,   // calendar・places の結果。openYears・openPrefs は開いている行、loaded は読み込み済みの view
  sel: null,            // 格子の絞り込み {from, to} | {untimed} | {bbox} | {pref, city?} | {abroad} と表示用の label
  period: null,         // 地図の点と範囲に掛ける期間 {from, to}（旅の「地図で見る」で入り、解除で外す）
  pointsStale: false,   // 地図が見えていないあいだに「除外も含める」・写真の位置・除外が変わった印（地図へ戻るとき点を取り直す）
  page: 1, pages: 1, per: 60, total: 0, items: [], tiles: [],   // 格子
  map: null, pendingData: null, labels: Map, popup: null,       // maplibregl.Map（地図ビューを初めて開いたとき作る）
  seq: {trips, grid, points, popup, calendar, places},   // 要求の種類ごとの番号（古い応答を捨てる。種類を分けるのは、
                                       // 吹き出しの要求が格子の読み込みを捨てないため）
}
```

- 旅の一覧: 1 旅 1 行（表紙のサムネイル、`start〜end`、`N 枚（位置なし M）`、場所の上位 3 つ、「地図で見る」）。
  押すと `sel = {from: start, to: end}` で格子を読む。末尾に「日時なし（K 枚）」。
- 格子: `div.lib-tile > img[loading=lazy][src=thumb_url]` ＋ 右下に ★・🚫・位置なしの小さな印。押すと
  `openModalFor(ls.items, i, {ai: false})`。ページングは写真タブと同じ部品。
- 地図: `ensureLibMap()` が `ensureMini()` と同じ style の組み方（source・出典の定数を `photos.js` から共有関数
  `tileStyle()` に切り出す）で `#lib-map` に作る。`photos` という GeoJSON source（`cluster: true, clusterRadius: 40,
  clusterMaxZoom: 16`）と 3 層（クラスタの円、件数の文字、単独の点）。クラスタを押すと
  `getClusterExpansionZoom` で寄る。点を押すと、その時点で `GET /api/admin/library/photos?ids=<id>` を引き（要求番号を
  付け、別の点を押したら古い応答は捨てる）、届いたら `maplibregl.Popup`（DOM を `el()` で組んで `setDOMContent`）に
  サムネイル・日時・地名・「開く」を出す。取得中は「読み込み中」、失敗は「読めませんでした」の吹き出し。「開く」は届いた
  1 枚で `openModalFor([photo], 0, {ai: false})`。クラスタの件数の文字は、style に `glyphs`（文字の形のデータ）が無く
  symbol 層が使えないので、見えているクラスタへ合わせる DOM のマーカー（`pointer-events: none`）で出す。更新は毎フレームの
  `render` では呼ばず、`moveend`・`zoomend`・`idle`（MapLibre 4.7.1 では前 2 つが描画時の source 更新より先に発火し、更新先のタイルがキャッシュにあると `sourcedata` も出ないため、描画が落ち着いた後に 1 回合わせる）、点の source の `sourcedata`（`e.sourceId === 'photos' && e.isSourceLoaded`）、
  `setData` の直後に行う（「3 層」は円 2 つ + マーカー）。Map の生成オプションに `renderWorldCopies: false` と
  `maxBounds: [[-180, -85], [180, 85]]` を入れて世界を周回させない（経度が ±180 を超えると表示範囲を bbox にできない。日付変更線を
  またぐ範囲は扱わない）。`currentBbox` は `getBounds()` を ±180/±90 に丸め、丸めた後に `w >= e` なら経度は `[-180, …, 180, …]`
  にする。「除外も含める」の切り替えでは、旅の一覧・格子・地図の点のうち読み込み済みのものを全部読み直す。地図が見えていない
  （旅の一覧・格子の上）ときは飛んでいる点の応答を捨てて `pointsStale` の印だけ立て、地図へ戻ったとき（戻る・ビューの切り替え）に
  点を取り直す。ツールバーに「この範囲の写真を見る」（`map.getBounds()` を `bbox` にして
  格子を読む）と期間の表示（旅から来たときの `from`/`to`、解除ボタン）。タブに戻ったとき `map.resize()` を呼ぶ。
- `.hidden` で出し入れする要素（`#lib-trips`・`#lib-period` ほか）に ID・クラスで `display` を指定するときは、対になる
  `#lib-trips.hidden{display:none}` のような規則を置く（詳細度が `.hidden` に勝って消えなくなるため。描画試験 (o) が検査する）。
- 画像の読み込み失敗（`img` の `error`）は格子・表紙・選別のメンバーで共通のプレースホルダ（`.img-broken`、文字は
  「画像なし」）に差し替える（写真タブのカードと同じ扱い）。
- 描画試験では `window.maplibregl` を偽物（`Map`・`Popup`・`addSource`・`getSource().setData` を記録するだけ）に
  差し替え、source に渡った GeoJSON と `bbox` 付きの要求、点を押したときの `ids=` の要求と吹き出しの DOM を検証する。
  実際のクラスタの操作・タブ復帰後の地図の大きさは検証環境で目視。

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
  「取り消す」= 記録があるときだけ）。画像を押すと `openModalFor(group.members, i, {ai: false})`。
- 確定は `POST /api/admin/curation/confirm {kind, group_id, token, representatives: [...] | null}`（`token` は `groups` が
  返したメンバー集合の世代の印）。応答の `group` でその段を描き直す。409 `group_changed` なら「組が変わりました。
  読み直します」のトーストを出し、その段だけ `GET …/groups?state=all&kind=all&ids=<先頭の写真 ID>` で読み直す（既定の
  `state=unconfirmed` だと確認済みになった組を見失う）。届いた組が今の絞り込み（`cs.state`・`cs.kind`）に合えば描き直し、
  合わなければ段を外す。応答が空（組が消えた）でも段を外す。
- 状況タブの「要対応」に `未確認の選別 N 組` を足す（R18。`db.quality.curation_pending`）。

## インターフェース

### `GET /thumb/<id>.jpg?v=<gen>`（新。PR ①）

- 経路: 正規化した後のパス（`webpath` の判定と同じ段階。`/thumb/` の分岐は静的配信の白名簿より前）が 1 段だけで
  `^[1-9][0-9]{0,17}\.jpg$` に合うものだけを受ける（先頭の 0・`0`・末尾の `/`・2 段以上は 404）。`i64` に変換し、
  以後のファイル名には変換後の数値を使う。`v` は 12 桁の小文字 hex（それ以外は 404）。アクセスログのラベルは
  `labeled(…, "/thumb/{id}")`。
- **生成するのは `v` が DB から計算した今の `gen` と一致するときだけ。** 一致しなければ、今の世代のキャッシュがあれば
  それを返し、無ければ 404（生成しない）。`gen` は 48 bit で、同一オリジンの管理 API（`thumb_url`）を読まないと分からない
  ので、外部のページの `<img>` が連番の ID で生成とディスク書き込みを起こすことはできず、`thumb_failed` も外から
  埋められない（平文 HTTP では `Sec-Fetch-Site` が付かない（Fetch Metadata は HTTPS と localhost だけ）ので、これが主な
  防御）。古い画面が持つ古い `v` は生成しないので「画像なし」かキャッシュになる（画面を読み直せば直る）。
- GET だけ生成する。HEAD はキャッシュがあれば本文なしで返し、無ければ 404（生成しない）。
- 重ねがけ（HTTPS で配るときだけ効く）: `Sec-Fetch-Site` が `cross-site`・`same-site` なら 403（`same-origin`・`none`・
  ヘッダ無しは通す）。応答に `Cross-Origin-Resource-Policy: same-origin` を付ける。
- 書き込み側（writer）が無い起動（`INGEST=0`）では生成しない。キャッシュがあれば返し、無ければ 5 の「書けない」と同じく
  元の表示用画像を返す（データディレクトリへの新しい書き込み経路を黙って増やさない）。
- 世代 `gen` = SHA-256（依存に既にある `ring::digest::SHA256`。`blake3`/`sha2` は足さない）の先頭 12 hex。入力は
  `display | md5 | mtime | THUMB_VERSION` を `|` で連結（`md5`・`mtime` が NULL なら空。`THUMB_VERSION` は `THUMB_PX`・品質を
  変えたときに上げる定数で、古いファイルと ETag が残らないようにする）。API の `thumb_url` は `/thumb/<id>.jpg?v=<gen>`。
- 処理:
  1. DB で `display`・`drive_id`・`md5`・`mtime` を引く（`thumb_for` と同じ規則で元ファイルを決める。無ければ 404。
     この 404 は失敗として覚えない）。`display` は `derivatives/` か `photos/` で始まり、`datadir::lexical` と同じ字面の
     検査（`..`・絶対パス・NUL・ドット始まりの要素を断る）を通るものだけを受け、それ以外は 404。`display` が NULL で `drive_id` から
     `derivatives/<drive_id>.jpg` を作るときは、`drive_id` を `datadir::check_drive_id`（`derivative_name` と同じ検査）に
     通す（DB の値が外を指していても返さない）。元ファイルはその**領域**の中でなければ
     ならない（`serve_data` と同じく領域のディレクトリとファイルの両方を `canonicalize` し、領域の外に出るリンクは 403、
     ディレクトリは 403、その他の特殊ファイルは 404。`statics.rs:312-345` の検査を関数に切り出して共有する）。
  2. `thumbs/<id>-<gen>.jpg` があれば返す（`thumbs/` も領域として同じ検査。`static_slots` の `try_acquire` を通り、空きが
     無ければ 503。`serve_data` と同じ）。
  3. 失敗の記憶 `thumb_failed`（下記）に `<id>-<gen>` があれば、ロックも枠も取らずに 404（5 の `decode`/`limit`）。
  4. **写真 ID ごとのロック**（`AppState.thumb_locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>`）を
     `lock_owned()` で取り（`OwnedMutexGuard`。生成の枠と一緒に `spawn_blocking` のクロージャへ move し、生成・失敗の記憶・
     古い世代の掃除が終わるまで持つ。要求が drop されても同じ ID の次の要求が先に生成を始めない）、取った後に 2 と 3 を
     もう一度見る（待っているあいだに他の要求が作った・失敗した）。表からの削除は `Arc::strong_count == 1`（待っている
     要求が無い）のときだけ（待機者のいるロックを消すと同じ ID に別のロックができる）。次に生成の枠
     `Semaphore(THUMB_CONCURRENCY)` を `acquire_owned()` で取る。**ID のロックと枠を合わせて、待ちは最長 10 秒・待つ数は
     最大 32**（待っている要求の数は `AtomicUsize`。超えたら 503 と `Retry-After: 5`。接続を握り続けない。
     `MAX_CONNECTIONS` は miharashi と共有）。切断で要求が drop されたときの待ち数の減算とロック表の掃除は Drop guard で
     行う。取った `OwnedSemaphorePermit` は
     `spawn_blocking` のクロージャに move し、生成が終わるまで持つ（クライアントが切っても復号が枠を超えて走らない）。
     （実装: ロックと枠は `tokio::spawn` した**所有タスク**が持ち、その中で `spawn_blocking(生成＋`write_atomic`)` →
     async 側で `catalog.read` による世代の引き直し → `spawn_blocking(古い世代の掃除)` の順に進め、掃除の終わりで離す。
     要求の future が切断されても所有タスクが続くので保証は同じ。）クロージャは `make_thumbnail(元, 320, 上限)` → `DataDir::thumb_dest(id, gen)`（`derivative_dest` と同じく `thumbs/` の
     実体がそのディレクトリ自身であることを確かめる。リンクなら書かない）→ `derivative.rs:951` の `write_atomic(dest, bytes)`
     （同じディレクトリに `.part` を `O_EXCL` で作り、`sync_all` して rename。既存）。成功したら古い世代を消す: `thumbs/` の実体が
     そのディレクトリ自身であること（`derivative_dest` と同じ検査）を確かめ、`symlink_metadata` で通常のファイルで、名前が
     `^<id>-[0-9a-f]{12}\.jpg$` に合い、今の `<gen>` でないものだけ。消す直前に DB の世代をもう一度引き、自分の世代が今の
     世代のときだけ消す（回転の直前に始めた生成が、回転後の世代を消さない）。起動時の掃除（`ingest.rs:260-320`）に
     `thumbs/` の `.part` を加える。
  5. 失敗の扱い（単純にする。Sol・Opus の 2 回目）:
     - **復号できない・上限を超える**（`decode`/`limit`）: 404（`Cache-Control: no-store`）。画面は「画像なし」のプレース
       ホルダ（壊れた原本や数十 MB の原本をそのまま返しても画面は救えず、負荷の高いときに転送を増やすだけ）。
       `thumb_failed: Mutex<(VecDeque<(i64,[u8;6])>, HashSet<…>)>` に **DB から計算した `<id>-<gen>`** を覚え（上限 10,000 件。
       超えたら古いものから捨てる）、起動中はロックも枠も取らずにすぐ 404 を返す。
     - **サムネイルはできたが書けない**（`write`。ディスクの満杯・権限・`thumbs/` がリンク）、または **writer の無い起動で
       キャッシュが無い**: 元の表示用画像を返す（拡張子が jpg/jpeg/png のときだけ。`serve_data` の領域の検査を切り出した
       関数で検査し、`static_slots` を通して別の応答を組む。`Content-Type` は拡張子から、`Cache-Control: no-store`、
       `Last-Modified`・304 は付けない）。覚えないが、60 秒のあいだは同じ ID の生成を試みない（`thumb_write_failed_until:
       Mutex<Option<Instant>>` で全体を止める）。
     - **生成タスクのパニック**: 元の表示用画像を返す。失敗の記憶にも 60 秒の停止にも入れない。ロックと枠は解放される。
     - **元ファイルが読めない通常の I/O エラー**: 404。記憶しない。
     - **DB の読み取りの失敗**（読み取り接続の待ち（既定 10 秒、`db.rs:57`。ロック・枠の待ちとは別）の期限切れを含む）:
       最初の写真の検索なら 503（`no-store`）。生成後の世代の引き直しなら古い世代の掃除を見送るだけ。
     - ログは失敗の初回に 1 行（WARN、ID と分類 `decode`/`limit`/`write` だけ。エラーの文面・パスは出さない）。
- 応答ヘッダ（キャッシュから返すとき）: `Content-Type: image/jpeg`、`Content-Length`、`Last-Modified`、
  `ETag: "<gen>"`（`thumb.rs` が付ける。`serve_data` のデータ画像は今までどおり ETag 無し）、`If-None-Match` が一致すれば 304、
  `Cache-Control: private, max-age=604800`（URL に世代があるので長くてよい）。`file_response`（`statics.rs:170`。非公開の
  応答の組み立て）は `pub(crate)` にして使う。
- パスは DB の `display` と `derivatives/<drive_id>.jpg`、`thumbs/<id>-<gen>.jpg` からだけ作る。クライアントの文字列は
  `<id>`（数字）以外使わない。
- 復号の上限はサムネイル専用に厳しくする: 入力 `THUMB_MAX_INPUT_BYTES = 64 MiB`、`THUMB_MAX_INPUT_PIXELS = 50,000,000`
  （既存の derivative 用は 512 MiB・約 1.79 億画素（`derivative.rs:43-45`）で、ローカルの原本は 2,560px でなく原寸のまま
  `display` になる（`local.rs:702`）ため、そのままでは 2 枚同時で 1〜2 GB になり得る）。超えたら作らずに 404 にし、
  `limit` として覚える。Drive の写真は 2,560px の derivative から作るので上限に当たらない。縮小復号（DCT の 1/2〜1/8）は zune-jpeg に
  API が無いので使わず、上限内の画像を全体復号してから縮小する（ヘッダの寸法での拒否は復号の前）。
- 定数: `THUMB_PX = 320`、`THUMB_QUALITY = 80`、`THUMB_CONCURRENCY = 2`、`THUMB_WAIT_MAX = 10 s`、`THUMB_QUEUE_MAX = 32`、
  `THUMB_FAILED_MAX = 10_000`。

### `GET /api/admin/library/trips`（新。PR ②）

- 引数: `hidden=1` で除外を含む（既定は含まない）。
- 応答:

```json
{"ok": true,
 "trips": [{"key": "2024-05-03 09:12", "start": "2024-05-03 09:12", "end": "2024-05-05 18:40",
            "count": 212, "noloc": 7,
            "places": [{"pref": "架空県", "city": "架空市", "count": 150}, {"pref": "試験県", "city": "試験町", "count": 40}],
            "cover": {"id": 1234, "thumb_url": "/thumb/1234.jpg?v=0f3a…"}}],
 "untimed": 12}
```

- 旅は `start` の降順。`cover` は旅の中で位置があり除外でない最初の写真（無ければ先頭）。`places` は `pref`・`city` の
  組の件数の上位 3 つ（`pref` が空なら `{"pref": "", "city": "", …}` で「海外・不明」）。
- 実装: `SELECT id, taken_at, lat, lon, pref, city, hidden, display, drive_id, md5, mtime FROM photos WHERE <日時ありの形の判定>
  [AND hidden=0] ORDER BY taken_at, id` を 1 回読み、Rust で 48 時間超の空きで区切る（`photos.rs` の
  `parse_epoch`・`TRIP_GAP_SECONDS` を `pub(crate)` にして共有）。日時ありの判定は `photos`・`points` と同じ形の判定
  （`taken_at GLOB` の 16 桁の形。`untimed` はその否定で、NULL・空・`x` のような形の違う値を数える）。暦として読めない日時
  （`2025-02-30 10:00` など）は同じ文字列ごとに 1 つの旅（通常は 1 枚。`key`・`start`・`end` はその文字列そのまま。
  前後の読める写真とは別の旅）にする。
  このため旅の `count` の合計 + `untimed` は対象の写真の数に常に等しい。

### `GET /api/admin/library/photos`（新。PR ②）

- 引数: `from`・`to`（`YYYY-MM-DD HH:MM`。16 桁の形であること（暦の正しさは見ない。`taken_at` 自体も暦を検査していないため、形で揃える）、`from <= to`。両端を含む。`taken_at` は
  分までの 16 文字（`imaging::norm_time`）なので `to` を分まで含めれば旅の最後の写真は欠けない）、
  `bbox=minlon,minlat,maxlon,maxlat`（有限、lon は ±180、lat は ±90、`min <= max`。日付変更線をまたぐ範囲は扱わず、画面が
  `getBounds()` の値を範囲内に丸めてから送る）、`ids=1,2,3`（整数。重複を除く**前**の個数が 200 を超えても 400）、`pref`・`city`（R10。文字数で
  64 まで、比較は `=`）、`untimed=1`（日時なし）、`hidden=1`（`1` 以外は既定）、`page`（既定 1。既存の `admin::photos` と
  同じく `clamp(1, pages)`）、`per`（既定 60、1〜200）。絞り込みが 1 つも無ければ 400 `bad_params`。形が違えば 400。
  SQL の条件は固定の候補の文字列だけから組み、値はすべてバインドする（`set_flag` の流儀）。
- 応答: `{ok, page, per, pages, total, items: [LibraryPhoto]}`。
  `LibraryPhoto = {id, name, thumb, thumb_url, taken_at, lat, lon, loc_source, place, pref, city, favorite, hidden, rotation, source}`
  （`thumb` は `thumb_for` と同じ表示用画像。ビューアが使う）。並びは `taken_at` 昇順（空は最後）、`id` 昇順。

### `GET /api/admin/library/points`（新。PR ②）

- 引数: `from`・`to`（`photos` と同じ検査）、`hidden=1`。
- 応答: `{ok, truncated, total, type: "FeatureCollection", features: [{type: "Feature", geometry: {type: "Point",
  coordinates: [lon, lat]}, properties: {id}}]}`。`lat`・`lon` の両方がある写真だけ。上限 `POINTS_MAX = 20000`
  （`taken_at` の新しい順に切る）。

### あると良い（PR ② の時間があれば）

どちらも `trips` と同じ流儀（`GET` の完全一致、`no-store`、DB の失敗は 503、`hidden=1` で除外を含む、`photos` 表だけを読む）。

- `GET /api/admin/library/calendar` → `{ok, years: [{year, count, months: [{month, count}]}], untimed}`。
  日時あり（`trips` と同じ 16 桁の形の判定）の行を `substr(taken_at,1,4)`・`substr(taken_at,1,7)` で数える。`year` は
  `"2025"` のような 4 桁、`month` は `"06"` のような 2 桁の**文字列**（暦として読めない値も形が合えばそのまま出る。
  `2025-02-30 10:00` は 2025 年 02 月）。年は新しい順、月は古い順。年の `count` は月の `count` の合計。`untimed` は
  `trips` と同じ日時なしの件数。年の合計 + `untimed` は対象の写真の数に等しい。
- `GET /api/admin/library/places` → `{ok, prefs: [{pref, count, cities: [{city, count}]}], abroad: n}`。
  `pref` が空でない行を `pref`・`city` で数える（`city` が NULL・空なら `city: ""` の 1 項目）。`abroad` は `pref` が NULL か
  空の件数（位置なしも含む。`trips.places` の「海外・不明」と同じ定義）。並びは都道府県・市区町村とも件数の多い順、
  同数は名前の順。以前の案にあった `noloc` は返さない（`abroad` と意味が重なるため）。
- 画面からの格子: 年は `from=YYYY-00-00 00:00&to=YYYY-99-99 99:99`、月は `from=YYYY-MM-00 00:00&to=YYYY-MM-99 99:99`
  （`photos` の `from`・`to` は形（桁が数字か）だけを見て文字列で比べる。`calendar` も形だけで数えるので、月 `13` や
  `31 日 24 時台` のような暦に無い値もその年・月に入る。範囲は `00`〜`99` で形いっぱいに取り、それらも漏らさない）。都道府県・市区町村は
  `pref`・`city`、日時なしは `untimed=1`。
- `photos` に `abroad=1`（`pref IS NULL OR pref=''`）を足す。`parse_qs` が空の値（`pref=`）を捨てるので、「海外・不明」は
  `pref=` では絞れないため。`pref`・`city` と同時に受けない（どちらかがあれば 400）。`1` 以外の値は絞り込みにならない
  （`untimed` と同じ）。
- 画面（R9〜R11）: 切替に「日付」「場所」を足す（`ls.view` は `calendar` | `places`）。「日付」は年の行（件数つき）を押すと
  月の行が開き、年・月のどちらからも「写真を見る」で格子。末尾に「日時なし（K 枚）」。「場所」は都道府県の行を押すと
  市区町村の行が開き、都道府県の行からは格子へ進む。市区町村の行も格子へ進むが、**市区町村が空の項目**（「（市区町村なし）」）は
  件数だけを出し、押しても格子へは進まない。その写真はその都道府県の格子から見る（`parse_qs` が空の値を捨てるため
  `city=` では絞れず、`abroad` のような専用の引数は足さない）。末尾に「海外・不明（N 枚）」（`abroad=1`）。格子のタイルに「位置を指定」（📍）の
  ボタンを置き、押すと写真タブと同じ位置の指定（`openLocModal`）を開く（タイルを押したときのビューアとは混ざらない）。
  保存すると `applyPhoto` → `libPhotoChanged` でタイルの「位置なし」の印が消える。位置（`lat`・`lon`）か除外（`hidden`）が変わると
  地図の点も変わるので、`libPhotoChanged` は飛んでいる `points` の応答を捨て（`seq.points` を進め）、地図が見えていれば点を取り直し、
  見えていない（旅の一覧・範囲の格子の上、別のタブ）なら `pointsStale` を立てて、地図へ戻るとき取り直す。patch に `lat`・`lon`・`hidden` のどれも無ければ（★だけの patch）何もしない。ただし `photos.js` の `setFlag` は応答の `{favorite, hidden}` を常に渡すので、画面の★操作では保守的に 1 回取り直す（表示は正しく、要求が 1 つ増えるだけ）。
- 共通ビューアの画像が無い（`thumb` が空）・読めない（`error`）ときは、格子・表紙・吹き出しと同じ「画像なし」の代わりの表示
  （`#viewer-noimage`）を出し、次の写真へ移ると画像の表示に戻す（R8）。

### `photo_curation` 表（新。PR ③。`curation::TABLES`。`SCHEMA` 定数には足さない）

```sql
CREATE TABLE IF NOT EXISTS photo_curation(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,                        -- 記録したときの写真の世代（photo_quality の同名の列と同じ意味）
  source_md5 TEXT,
  source_mtime REAL,
  content_sha256 TEXT NOT NULL,                 -- 記録したときの表示用画像の内容（photo_quality.content_sha256）
  role TEXT NOT NULL CHECK(role IN ('representative','reviewed')),
  decided_at TEXT NOT NULL                      -- 'YYYY-MM-DD HH:MM:SS'（UTC）
);
```

- 世代の鍵は 4 つ全部（Sol の指摘 1。`content_sha256` は表示用画像のバイト列の SHA なので、原本の `md5`・`mtime` だけが
  変わっても同じになり得る）。**有効な記録** = `photo_quality q`（`photos` と世代の合う行）と `photo_id`・`display`・
  `source_md5`・`source_mtime`・`content_sha256` がすべて一致する行。それ以外の記録は **stale**。
- グループ G（`q.group_kind, q.group_id`。`photos` と世代の合う `photo_quality` の行で、2 枚以上のもの）について:
  - `picks(G)` = 有効な `representative` のメンバー。
  - 有効な推奨 `eff(p)` = `picks(G)` が空でなければ `p ∈ picks(G)`、空なら `q.recommended=1`。
  - 状態（Sol の指摘 2。「記録がある」は stale を含む）:

    | メンバーの記録 | 状態 |
    |---|---|
    | 1 枚も記録が無い | `unconfirmed` |
    | 全メンバーに有効な記録がある | `confirmed` |
    | それ以外（記録の無いメンバーがいる＝写真が加わった、stale のメンバーがいる＝写真が変わった、全員 stale＝再構築で内容が変わった） | `recheck` |

  - 過渡期: 回転・原本の更新の直後は、その写真の `photo_quality` 行が世代違いになり、再解析（`quality` の段）と再構築まで
    グループから外れる。残りが 1 枚ならグループはいったん消え、再構築後に新しい SHA で戻ってきて、古い記録が stale なので
    `recheck` になる。残りが 2 枚以上なら、そのあいだ残りだけで `confirmed` に見えることがある（数分〜次の同期まで）。
    設計上の限界として文書に書く。
- SQL 片（`curation.rs`）。`gp` は有効な代表の有無だけが要るので `EXISTS` で書く:

```sql
-- CURATION_JOIN（QUALITY_JOIN の後ろに付ける。c は有効な記録だけ）
LEFT JOIN photo_curation c ON c.photo_id=q.photo_id AND c.display=q.display
  AND c.source_md5 IS q.source_md5 AND c.source_mtime IS q.source_mtime AND c.content_sha256=q.content_sha256
-- GROUP_HAS_PICK（有効な代表がグループにあるか）
EXISTS (SELECT 1 FROM photo_quality q2
          JOIN photos p2 ON p2.id=q2.photo_id AND p2.display=q2.display
           AND p2.md5 IS q2.source_md5 AND p2.mtime IS q2.source_mtime
          JOIN photo_curation c2 ON c2.photo_id=q2.photo_id AND c2.display=q2.display
           AND c2.source_md5 IS q2.source_md5 AND c2.source_mtime IS q2.source_mtime
           AND c2.content_sha256=q2.content_sha256 AND c2.role='representative'
         WHERE q2.group_kind=q.group_kind AND q2.group_id=q.group_id)
-- EFFECTIVE_RECOMMENDED
CASE WHEN q.group_id IS NULL THEN 0
     WHEN <GROUP_HAS_PICK> THEN COALESCE(c.role='representative',0)
     ELSE q.recommended END
-- QUALITY_VISIBLE_EFFECTIVE
(q.group_kind IS NULL OR q.group_kind!='exact' OR <EFFECTIVE_RECOMMENDED>=1)
```

- 境界の期待値（試験で固定する。Sol の指摘 16）:

  | 場面 | `effective` | 状態 |
  |---|---|---|
  | similar の中に同じ SHA の 2 枚があり、片方だけ代表に選んだ | 選んだ写真だけ `true`（写真 ID 単位） | `confirmed` |
  | 完全重複（exact）で 2 枚を代表に選んだ | 2 枚とも `true`（フィードに 2 枚出る） | `confirmed` |
  | 代表が stale（回転した）、他は `reviewed` のまま | 有効な代表が無いので解析の推奨 | `recheck` |
  | 古い世代の `representative` 行だけ残り、再構築で同じ組に戻った | 解析の推奨 | `recheck` |
  | `reviewed` だけ（`representatives: null` で確認） | 解析の推奨 | `confirmed` |

- 性能（測定済み）: 合成 6,500 行（うちグループ入り 1,000 行、代表の記録 100 件。`tests/curation_feed.rs` の
  `measure_the_feed_with_6500_rows`、`--ignored --nocapture`）で、`list_photos` は表が無い従来の SQL で約 52 ms、
  表がある SQL で約 54 ms（debug ビルド、20 回の平均。差は 5% 前後）。`EXPLAIN QUERY PLAN` は `photos` を 1 回走査し、
  `q`・`c`・`q2`（`idx_photo_quality_group`）・`p2`・`c2` はすべて主キーか `idx_photo_quality_group` の検索で、全表走査の
  副問い合わせは無い。グループ外の写真（6,500 行の大半）は `CASE WHEN q.group_id IS NULL` で `EXISTS` に入らない。
  よって代替（`GROUP_HAS_PICK` の集合を先に 1 回引いて Rust 側で判定）は採らず、`EXISTS` のままにした。
  読み取り接続は `PRAGMA query_only`（`db.rs:228`）なので一時表は使えない（代替を採るなら Rust 側の集合になる）。
- 表が無いとき（`available(conn)=false`）は従来の SQL 片をそのまま使う。`Catalog::read` の接続ごとに `available` を
  1 回引く（`sqlite_master` の 1 行）。書き込み側は起動時に `ensure_tables`（`ai.rs` の AI 表と同じ場所）。
- 旧 AI 選別（`selection.rs`）: `VALID_MEMBERS` の `shown`（`selection.rs:27-40`）が `mq.recommended=1` を見ているので、
  `EFFECTIVE_RECOMMENDED` に置き換える（同じ `curation.rs` の SQL 片を `mq` に当てる）。`ai_visible()` は、写真に有効な
  `representative` の記録があれば `favorite` と同じく抑止しない（人の代表が旧 AI の非推薦でも消えない）。
  合成ケース「品質の組と旧 AI の組が重なり、人の代表が旧 AI の非推薦」を `tests/legacy_selection.rs` に足す。

### `GET /api/admin/curation/groups`（新。PR ③）

- 引数: `state=unconfirmed|confirmed|all`（既定 `unconfirmed`。`unconfirmed` は `recheck` を含む。知らない値は 400）、
  `kind=all|exact|similar`（同じ）、`ids=<photo_id,…>`（そのグループだけ。段の読み直し用。整数、重複を除く前の個数で
  最大 200）、
  `page`（`clamp(1, pages)`）、`per`（既定 20、1〜50）。
- 応答:

```json
{"ok": true, "page": 1, "per": 20, "pages": 3, "total": 41,
 "groups": [{"kind": "similar", "group_id": 17, "token": "3f9c…", "state": "recheck", "taken_at": "2024-05-03 09:12",
   "count": 3, "warning": null,
   "members": [{"id": 101, "name": "IMG_0001.jpg", "thumb": "/derivatives/…", "thumb_url": "/thumb/101.jpg?v=…",
                "taken_at": "2024-05-03 09:12", "place": "…", "pref": "架空県", "city": "架空市", "lat": 10.0, "lon": 20.0,
                "favorite": false, "hidden": false, "rotation": 0, "source": "drive",
                "rank": 1, "score": 78.5, "reasons": ["…"], "issues": [],
                "recommended": true, "effective": true, "curation": "representative" | "reviewed" | "stale" | null}]}]}
```

- `token` は秘密ではない（同一オリジンの GET で読める。CSRF の対策は従来どおり Origin・Content-Type）。競合の検出にだけ
  使うので 64 bit の衝突は問題にならない。`token` = メンバーを `photo_id` 昇順に並べ、1 枚につき `"<photo_id>\t<display>\t<source_md5|->\t<source_mtime|->\t
  <content_sha256>\t<解析の recommended 0/1>\t<人の記録: representative|reviewed|stale|none>"`（NULL は `-`、空文字は
  そのまま。区切りはタブ）を改行でつないだ文字列の SHA-256 の先頭 16 hex。読むとき（`groups`）と書くとき（`confirm`・
  `reset` の検査）で同じ関数 `curation::group_token(members)` を使う。メンバーの集合・各写真の世代（4 列）・解析の推奨・
  人の記録のどれが変わっても一致しなくなるので、再構築・再解析・別のタブの確定・`reset` のどれとの競合も 409 になる
  （全 ID を送らないので枚数の上限が無い。Sol の指摘 4・2 回目の 1・2）。
- `warning`: 有効な推奨（`effective`）の写真がすべて除外か位置なし（フィードの品質以外の述語を満たさない）なら
  `"representative_not_visible"`（exact の組ならフィードから消えている）。それ以外は `null`。この定義がリスクの表からも
  参照される唯一の定義。
- グループの並び: `taken_at`（グループの最小）の降順、`group_kind`、`group_id`。世代の合わない `photo_quality` の行と
  1 枚しか有効でないグループは出さない。`curation: "stale"` は記録はあるが世代が合わないメンバー。

### `POST /api/admin/curation/confirm`（新。PR ③）

- 本文: `{"kind": "exact"|"similar", "group_id": int, "token": "<16 hex>", "representatives": [int…] | null}`。
  `representatives` は 1 件以上（上限は本文の 64 KiB だけ。独自の上限は置かない）、重複なし、または `null`（人の代表を
  持たず、解析の推奨のまま確認済み）。
  画面の「このままで確認済み」は、段に有効な人の代表があればそれを `representatives` に入れて送る（黙って解析の推奨に
  戻さない）。
- 本文の型: `kind` は 2 値のどちらか、`group_id` は `as_i64()` で取れて 1 以上、`token` は 16 桁の小文字 hex、
  `representatives` の各要素は `as_i64()` で取れて 1 以上（`1.0`・負数・0・真偽値・`u64` を超える数は 400）。
  **`representatives` のキーが無ければ 400**（`null` は明示したときだけ。壊れたクライアントが人の代表を黙って消さない）。
- 判定の順序は `set_flag` と同じ: 本文の形 400 → `writer_or_unavailable` 503 → トランザクションの中で 409 `group_changed` →
  400 `representative_not_visible`。AC7 の 503 の試験は正しい形の本文で行う。
- 検査（`transaction_with_behavior(Immediate)`。`Writer::call` はトランザクションを張らない（`writer.rs:239-241`）ので
  自分で張る。`quality-scan` の再構築は別の接続から `BEGIN IMMEDIATE` で全行を入れ替えるため、遅延開始で読んでから書くと
  `SQLITE_BUSY_SNAPSHOT` になる）: `(kind, group_id)` の世代の合うメンバーを引き、2 枚以上あり、その `token` が本文と
  一致すること。違えば 409 `{"ok": false, "error": "group_changed"}`（組が消えた・組み替わった・メンバーの内容が変わった、
  をまとめて扱う）。`representatives` はメンバーの部分集合であること（違えば 400 `bad_params`）。待ち切れなかった BUSY は
  503（500 にしない）。
- 代表の検査（同じトランザクション）: `representatives` の各写真が、フィードの品質以外の条件を満たすこと。SQL は
  `list_photos` の WHERE そのもの（`p.lat IS NOT NULL AND p.display IS NOT NULL AND COALESCE(p.hidden,0)=0`、
  `photos.rs:68-69`。定数を共有して変えない）、加えて `lon IS NULL` の行をフィードが Rust 側で落とす（`photos.rs:88-91`）のと
  同じ判定。違えば 400 `{"ok": false, "error": "representative_not_visible"}`。`groups` は各メンバーに同じ判定の
  `selectable: bool` を返し、画面は自分で計算せずそれでチェックボックスを無効にする。旧 AI 選別は人の代表を抑止しない（上の節）ので条件に入れない。
- 書き込み（同じトランザクション）: `DELETE FROM photo_curation WHERE photo_id IN (<メンバー>)` → 各メンバーに `role`
  （代表なら `representative`、他は `reviewed`）と世代の 4 列・`decided_at` を INSERT。
- 応答: `{ok: true, group: <groups と同じ 1 組>}`。
- 失敗: 本文の形が違えば 400 `bad_params`、`INGEST=0` は 503、CSRF は 403（既存）。エラーの本文は固定の符号だけで、
  ID の一覧・SQL・パスを含めない。

### `POST /api/admin/curation/reset`（新。PR ③）

- 本文: `{"kind", "group_id", "token"}`（同じ検査。`token` が合わなければ 409）。メンバーの `photo_curation` 行を `DELETE`
  するだけ（stale の行も消す）。応答は `confirm` と同じ。

### 実装との対応（`admin_curation.rs`。T7）

設計どおりに作った。実装で決めたこと・設計から細かくずれたことだけを書く。

- **共有の定義**: `groups` の一覧・件数、`confirm`/`reset` のメンバーの読み出し、状況タブの `curation::pending_groups` は
  `curation::members_from(curation)`（世代の合う `photo_quality` と `photos`、表があれば有効な記録 `c`）、
  `curation::member_record(curation)`（`representative`/`reviewed`/`stale`/`none` の式）、
  `curation::not_confirmed`/`all_confirmed`（`HAVING` の条件）から作る。`curation_pending` と
  `groups?state=unconfirmed` の `total` が一致するのは、同じ `FROM`・`GROUP BY`・`HAVING` から数えるため（AC9）。
  表が無いとき（`curation=false`）はこれらの片が `photo_curation` を一切書かない。
- **`ids=` は写真 ID**（その写真が属する、世代の合うグループを返す）。グループ ID ではない。
- **有効な推奨 `effective` は Rust で求める**（メンバーに有効な `representative` があればその写真だけ、無ければ
  `recommended`）。SQL の `effective_recommended` と同じ定義で、1 組ぶんのメンバーを読んだ後に付けるので副問い合わせは要らない。
  `warning` と `selectable` もこの 1 回の読み出しから出す。
- **グループの `taken_at`** は空でない `taken_at` の最小（無ければ `""`）。並びも同じ値
  （`COALESCE(MIN(NULLIF(p.taken_at,'')),'') DESC, group_kind, group_id`）。
- **`confirm` の `representatives: null`** は、人の代表がある組でも**人の代表を外して**全員を `reviewed` にする
  （解析の推奨に戻って確認済み）。画面の「このままで確認済み」は、有効な人の代表があればそれを入れて送る。
- **`reset` は `representatives` を見ない**（あっても無視）。`confirm` と `reset` の本文の検査は `kind`・`group_id`・
  `token` が共通で、エラーはどれも 400 `bad_params`（値は応答に出さない）。
- **`decided_at`** は `strftime('%Y-%m-%d %H:%M:%S','now')`（SQLite の UTC）。世代の 4 列は `INSERT … SELECT … FROM photo_quality`
  で `photo_quality` の行の値をそのまま入れる。
- **失敗の符号**: 本文の形 400 `bad_params` → writer が無ければ 503 → `BEGIN IMMEDIATE` の中で 409 `group_changed`
  （組が消えた・1 枚になった・`token` 違い）→ 400 `bad_params`（部分集合でない）→ 400 `representative_not_visible`。
  `photo_curation` 表が無い DB に writer がある（起動時に作る前提で通常は起きない）ときは何も書かず 503。
  待ち切れなかった `SQLITE_BUSY`/`LOCKED` は 503（既存の `ApiError::Unavailable` を使うので、本文の文面は
  「書き込みが無効…」の既存の定型文。符号は変えていない）。
- **HEAD・POST の `groups`、GET の `confirm`/`reset`** は既存の決まりどおり 403。`groups` は `no-store`。

### `GET /api/admin/status` の変更（PR ③）

- `db.quality.recommended`・`exact_suppressed`・`similar_alternates`（`quality_summary`、`admin.rs:335`）を
  `EFFECTIVE_RECOMMENDED` で数える（R19）。人の記録が無い Python のフィクスチャでは値が変わらないので `admin_api.rs` は通る。
- `db.quality.curation_pending`（あると良い R18）: 状態が `unconfirmed`・`recheck` のグループ数（`curation::pending_groups`。世代の合う `photo_quality` の行が 2 枚以上のグループのうち、有効な記録の無いメンバーがいるもの。表が無ければすべてのグループ）。`python_shape` で除く。

### `GET /api/v1/photos`（PR ③。項目は不変）

- `quality_recommended` と `quality_role`、exact の除外条件を `EFFECTIVE_RECOMMENDED` で計算する。`QUALITY_VISIBLE_EFFECTIVE`
  は既存の `hidden=0`・位置・`display`・`ai_visible()` の条件と AND でつなぐ（人の代表でも、確定後に除外した写真は出ない。
  AC4 で確かめる）。人の記録が無い DB では値も不変。契約の文書と `photo-feed.schema.json`（`description` だけ）の `quality_recommended` に「管理画面で人が代表に
  選んだ写真があるグループでは、その写真だけが `true`（解析の推奨は使わない）」を足す。型・`required` は変えない。
- 旧 AI 選別との関係は「`photo_curation` 表」の節のとおり（`shown` に有効な推奨、人の代表は抑止を免れる）。
- 人の代表は `confirm` で「出せる写真」に限っているが、確定後に除外・位置の削除で出せなくなることはある。そのときは
  解析の推奨と同じく、exact グループが一覧から消える（今の挙動と同じ穴。選別タブで `recheck` にはならないので、
  リスクの表に書く）。

## データ

- `thumbs/`（データディレクトリ直下。`datadir.rs` に項目を足し、`webpath.rs` の `Area` に `Thumbs` を足して `serve_data` と
  同じ領域の検査を受ける。`Area::Thumbs` は領域の検査を共有するための内部の区分で、`/thumbs/` という配信経路は無い
  （静的配信の白名簿に入れない）。`thumbs/` がリンクのときは書かず・消さず、起動時の `.part` の掃除も黙って見送る
  （`derivatives/` のような警告は出さない）。生成関数の差し替えと回数の観測のための API（`with_renderer`・`renders()`・
  `with_thumbs` など）は結合試験のために通常ビルドにもある）。1 枚 15〜30 KB、6,500 枚で 200 MB 弱。古い世代は再生成時に消す。消しても次の要求で
  作り直せる（バックアップの対象外。`backup-catalog.sh` は触らない）。writer の無い起動では作らない（上の `/thumb/` の節）。
- `photos.id` は `INTEGER PRIMARY KEY`（AUTOINCREMENT でない）ので ID は再利用され得るが、サムネイルは世代、`photo_curation`
  は世代の 4 列を鍵に含むので取り違えない。消した写真の `thumbs/<id>-*.jpg` と `photo_curation` の行は残る（URL からは
  引けない）。掃除は C（運用コンソール）の対象としてリスクの表に書く。
- `photo_curation`: 書き込み側（`serve` の writer）が起動時に作る。移行は無い。`INGEST=0` の読み取り専用の DB では
  作らず、読む側が無いものとして動く。
- 旅・集計は毎回 SQL から計算する（6,500 行で 10 ms 台。キャッシュは作らない）。

## 安全面

- 入力の検証: `/thumb/` の ID は数字だけ。ライブラリの `from`/`to` は 16 文字の固定形式、`bbox` は有限の数 4 つで範囲内、
  `ids` は整数 200 件まで、`pref`/`city` は 64 文字まで（SQL はすべてバインド）。`confirm`・`reset` の本文は
  `{kind, group_id, token, representatives}` で、型の検査は API の節のとおり。
- 書き込みは既存の `post.rs` の経路（Host → メソッド → `route_of` → CSRF → 64 KiB → JSON → `writer_or_unavailable`）。
  新しい POST は 2 本だけ、どちらも `photos`・`photo_quality` の存在を検査して 1 トランザクションで書く。
- 外部公開のとき: `/thumb/`・`/api/admin/library/*`・`/api/admin/curation/*` は管理用の経路で、Cloudflare Access の保護の
  外（miharashi 用の `/api/v1/photos`・`/derivatives/` の素通し）に入れてはいけない。`/thumb/` は連番の ID で除外した写真も
  引ける。
- 権限: 認証は無い（LAN 内、ADR 0008）。新しい API が出す情報（撮影地の座標・地名・サムネイル）は既存の
  `/api/admin/photos` と `/derivatives/` が既に出しているものと同じ範囲。`points` は全写真の座標を 1 回で返すが、
  `/api/admin/photos?per=200` を回せば同じものが取れるので、露出の範囲は変わらない。
- ファイル: サムネイルの読み元は `derivatives/`・`photos/` の領域の中に限り（領域とファイルを `canonicalize`、領域の外へ
  出るリンクは 403、通常のファイル以外は 404。`serve_data` と同じ）、書き先は `thumbs/` だけ（`write_atomic` が同じ
  ディレクトリに tmp を作って rename）。ファイル名は `<id>-<gen>.jpg` で固定。生成の失敗で元ファイルを返すのは、拡張子で
  画像と分かるときだけ（`catalog.db` のような画像でないファイルを返さない）。
- 外部への通信: 無し（地図タイルは既存のプロキシ）。秘密情報は扱わない。ログにパス・座標を出さない。
- 文書・試験データ・画面写真: 応答の例と新しいフィクスチャは既存の `admin-api-catalog.sql` と同じ流儀（架空の地名、明らかに
  合成と分かる座標）。試験の画像は `image` crate でコードから作り、本番のデータディレクトリから写さない。PR・文書に貼る
  画面写真は合成データだけ（本番のコピーで目視した画面は貼らない。写真の集まり方で自宅が分かる）。
- 資源: サムネイルの生成は同時 2 つ、同じ写真は ID ごとのロックで 1 つ。Drive の写真は 2,560px の derivative（復号で
  約 20 MB、50〜150 ms）、ローカルの原本は原寸（上限 5,000 万画素 × 3 ch ＝ 150 MB に変換のコピーが加わり 1 枚 300〜400 MB、
  2 つ同時で 1 GB 弱が最悪）。初回の旅の一覧で 60 枚なら数秒〜十数秒で埋まる。`spawn_blocking` なので待ち受けは止まらない。復号の上限・待ちの上限・失敗の
  記憶は `/thumb/` の節の定義が唯一の定義（ここでは繰り返さない）。`confirm`・`reset` は 1 トランザクションで検査と書き込みを行い、
  `token` で画面が見た組との一致を確かめるので、再構築や別のタブの確定と競合しても古い判断で上書きしない。

## テスト計画

| AC | 試験 | 置き場 |
|---|---|---|
| AC1 | 合成 JPEG（`image` crate で 1200×800 を作る）を `derivatives/` に置き、`/thumb/<id>.jpg` の 200・寸法・`ETag` の存在と `If-None-Match` の 304・`mtime` 変更後の再生成と古い世代の削除・404（数字でない、無い写真）・403（領域の外へのリンク）・壊れた JPEG は 404 で 2 回目は復号しない（失敗の記憶。生成関数の呼び出し回数を数える）・書けない（`thumbs/` を読み取り専用に）ときは元ファイルが返る・同じ写真への同時 8 要求で生成が 1 回（ID ごとのロック）・`display` が `catalog.db`・`../x`・`tmp/x` を指す行は 404 で本文が DB でない・寸法だけ大きい合成 PNG は 404（上限。全体を復号しない）・GPS 付きの合成 JPEG から作ったサムネイルに APP1（EXIF）が無い・`Sec-Fetch-Site: cross-site` は 403・HEAD は生成しない・`v` が今の世代と違うときは生成せず 404（キャッシュがあればそれ）・`INGEST=0` では生成せず `thumbs/` もできず元の画像が返る・待ち合わせ中に要求を drop しても同時の生成が 2 を超えない・同じ ID の生成途中で要求を drop しても次の要求が二重に生成しない（単体）。`/derivatives/` のヘッダ不変は既存試験 | `tests/thumb.rs`（新）＋ `thumb.rs` の単体試験 |
| AC2 | 新しいフィクスチャ `tests/fixtures/library-catalog.sql`（10 分・1 時間 30 分・8 時間・ちょうど 48 時間・9 日の差、位置なし、日時なし、除外を含む）で `trips` の境界・`noloc`・`untimed`・`hidden=1` の差を独立した期待値で検証。描画試験で旅 → 格子 → ビューア → ★ → `POST setflag` の `id` → タイルの印の更新（購読者）。写真タブのカードが変わらないことも見る | `tests/library_api.rs`（新）、`web/tests/library_render.mjs`（新）＋ `tests/admin_ui_library.rs` |
| AC3 | `points` の位置なし除外・`from`/`to`・`bbox` の絞り込み（結合）。`truncated` は切り詰めの関数（`fn truncate_points(rows, max)`）の単体試験と、実際の上限を使う結合試験（2024 年の 20,001 点を足して、位置のある写真 20,012 枚のうち 20,000 点で `truncated: true`・`total` が全体の数・古いものから落ちること、範囲で絞れば `truncated: false`）。描画試験で偽 MapLibre の `setData`、点を押したときの `ids=` 要求と吹き出し、`bbox` 要求 | 同上 |
| AC4 | 新しいフィクスチャ `tests/fixtures/curation-catalog.sql`（`admin-api-catalog.sql` は similar 1 組・exact 1 組なので流用しない）: exact 1 組・similar 2 組（1 組は同じ SHA の 2 枚を含む）。`confirm` → `groups`・`/api/v1/photos`・`status` の `db.quality.recommended`・`photo_quality` の入れ直し（`group_id` を変える）後も保持。`reviewed` のみ・`reset`・上の境界の表の 5 行・代表に記録した後で除外にした写真は `/api/v1/photos` に出ない・`photo_quality` を入れ替える書き込みと同時に `confirm` しても表が半端にならない | `tests/curation_api.rs`（新） |
| AC5 | 確認済みの組に 1 枚足す → `recheck`。メンバーの `mtime` と `photo_quality` の世代を変える（再解析後を模す）→ `recheck`・`stale`。`token` 違いの `confirm`・`reset` → 409 で表が不変（集合違い・SHA は同じで `mtime` だけ違う・別のタブが先に代表を変えた後の古い `token`・`reset` 後の古い `token` の 4 通り）。`representative_not_visible` → 400 で表が不変 | 同上 |
| AC6 | 既存のフィードの試験（契約・endpoint・miharashi）が無変更で通る。`photo_curation` 無しの DB で `list_photos` の SQL が従来と同じ結果（単体）。旧 AI の組と重なる合成ケース（人の代表が旧 AI の非推薦でも出る） | 既存 ＋ `photos.rs` の単体試験 ＋ `tests/legacy_selection.rs` |
| AC7 | CSRF 403・`INGEST=0` 503・表の無い DB で各 GET が 200・新しい GET への POST が 403 | `tests/curation_api.rs`・`tests/library_api.rs` |
| AC8 | `cargo fmt --check`・clippy・`cargo test`。`static_files.rs` の文字列 HTML の禁止が `library.js`・`curation.js` にも効く（`web/static/js` 直下に置く） | CI |
| AC9 | 年 → 月、都道府県 → 市区町村の描画。`curation_pending` と `groups?state=unconfirmed&per=1` の `total` の一致 | `library_render.mjs`・`tests/curation_api.rs` |

描画試験の偽 MapLibre は `web/tests/fake_maplibre.mjs` に置き、`Map`（`on`・`addSource`・`addLayer`・`getSource`・
`getBounds`・`getClusterExpansionZoom` は固定値）と `Popup`・`NavigationControl` だけを持つ。

## リスクと対策

| リスク | 対策 |
|---|---|
| 人の代表が `similar` グループで 1 枚だけになり、解析の「上位 2 枚」より miharashi に出る枚数が減る | 意図した挙動として文書に書く。複数選べるので必要なら 2 枚選ぶ |
| 確定後に代表を除外した・位置を外したため、exact グループがフィードから消える | 解析の推奨と同じ既存の穴。選別タブの段に「代表が出せない」の警告を出す（`groups` の `warning` の定義に従う） |
| 回転・原本の更新で記録が無効になり、確認し直しになる | `recheck` として目に見える形で戻す（黙って消さない）。`stale` の印で理由が分かる |
| `quality-scan` の再構築中（`DELETE` と INSERT の間）に `groups` を読む | 再構築は 1 トランザクションなので途中は見えない（`quality.rs:345-`）。`confirm` は writer のトランザクションで検査するので競合しても 409 になるだけ |
| サムネイルの初回生成で一覧が遅い | `loading=lazy` と同時 2 つの生成。60 枚/ページ。必要なら後で同期の段に前もって作る処理を足す（C） |
| `thumbs/` がディスクを圧迫する | 200 MB 弱の見積もりを文書に書く。古い世代は再生成時に消す |
| `list_photos` の SQL が重くなる（`EXISTS` のサブクエリ） | 実装時に合成 6,500 行で `EXPLAIN QUERY PLAN` と時間を測る。遅ければ代表のある組の集合を先に 1 回引いて Rust 側で判定する（読み取り接続は `query_only` なので一時表は使えない） |
| ビューアの共通化で写真タブの挙動が変わる | 既存の `photos_render.mjs` の (p)(q)(q2) をそのまま通す。`openModal(index)`・`viewerIndex` の外部の挙動は変えない。別の配列から開いて正しい ID に POST する試験を足す |
| 回転のあとカードが古いサムネイルを出す | `POST /api/admin/rotate` の応答に `thumb_url` を足し、`applyPhoto` の patch に含める（描画試験に回転後の `src` を足す） |
| 回転・原本の更新の直後、残りのメンバーだけで `confirmed` に見える | 設計上の限界として文書化。再解析・再構築で `recheck` に戻る |
| 消した写真の `thumbs/` のファイルと `photo_curation` の行が残る | 害は無い（URL から引けず、世代で取り違えない）。掃除は C（運用コンソール）で |
| 偽 MapLibre では地図の見た目を検証できない | A と同じく検証環境で目視。描画試験は source に渡るデータと要求の URL を検証する |
| `tests/admin_api.rs` の完全一致が落ちる | `status` に足す項目は `python_shape` で除く。`/api/admin/photos` は触らない |

## 実装の順番

PR の依存: ② と ③ は ① のサムネイルを使うが、無くても `thumb_url || thumb` で動く（重いだけ）。③ は ② のビューアの
共通化（`openModalFor`・`onPhotoChange`）に依存する。戻す順番は A と同じ: ① は ②③ のマージ前なら単独で戻せる。② を戻す
には ③ を先に戻す（Sol の指摘 9。厳密な独立は保てないので、順番を明記する）。

### PR ①: サムネイル（R1〜R4、R21）

1. `imaging/derivative.rs` に `make_thumbnail`（`render`/`resize` の再利用。単体試験: 寸法と向き）。`statics.rs` の領域の
   検査と `file_response` を `pub(crate)` に切り出す。`webpath.rs` の `Area` に `Thumbs`。
2. `thumb.rs`: 経路・世代・キャッシュ・ID ごとのロック・Semaphore・失敗の記憶・フォールバック・ETag と 304。`server.rs` に
   `/thumb/` の分岐。`datadir.rs` に `thumbs/`。
3. `tests/thumb.rs`（AC1）。`docs/architecture/photo-pipeline.md` にサムネイルの節（配信の一覧がある文書）。
4. （R4）`AdminPhoto`（`admin.rs`）・検索 API の写真・`POST /api/admin/rotate` の応答に `thumb_url` を足し、
   `tests/admin_api.rs` の `python_shape` で `hint` と同じく除く（方針 4「新しい項目は新しい API に」の意図した例外。
   `python_shape` で除ける項目に限る）。カードの `img.src` を `thumb_url`（無ければ `thumb`）にし、`rotatePhoto` の patch に
   `thumb_url` を含める。描画試験の合成データに `thumb_url` を足し、回転後の `src` の検証を 1 つ足す。

### PR ②: ライブラリ（R5〜R8。R9〜R11 は時間があれば）

1. `photos.rs` の `parse_epoch`・`TRIP_GAP_SECONDS` を `pub(crate)`。`admin_library.rs` の 3 本（＋あると良い 2 本）と結合試験
   （新しいフィクスチャ `library-catalog.sql`）。
2. `photos.js` のビューアの共通化（`viewerEntries`・`openModalFor`・`onPhotoChange`・`tileStyle()`）。既存の描画試験が通ることを
   先に確認し、別の配列から開く試験を足す。
3. `admin.html`・`admin.css`・`library.js`（旅 → 格子 → ビューア、地図）。`admin.js` のタブ登録。
4. `library_render.mjs` ＋ `fake_maplibre.mjs` ＋ `tests/admin_ui_library.rs`。
5. 検証環境で地図の目視（クラスタ・吹き出し・範囲の絞り込み）。

### PR ③: 選別の確認（R12〜R19）

1. `curation.rs`（表・`ensure_tables`・`available`・SQL 片・`token`）と `photos.rs`・`selection.rs`・`admin.rs`
   （`quality_summary`）の置き換え。フィードと旧選別の既存試験が通ることを確認し、重なる合成ケースを足す。性能の測定。
2. `admin_curation.rs`（`groups`・`confirm`・`reset`）、`post.rs`・`server.rs` の登録、`tests/curation_api.rs`
   （AC4・AC5・AC7。新しいフィクスチャ `curation-catalog.sql`）。
3. `curation.js`・`admin.html`・`admin.css`、状況タブの件数（R18）、`curation_render.mjs` ＋ `tests/admin_ui_curation.rs`。
4. 契約の文書（R19）、`docs/architecture/photo-pipeline.md` の管理 API の一覧、`docs/admin-library/` の「実装との差」。
