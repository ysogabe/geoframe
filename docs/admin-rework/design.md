# 管理画面の整え直し（A） 設計

作成日: 2026-10-30。状態: レビュー 2 回（Sol ×2、安全面 Opus ×1）を反映済み。ユーザー承認待ち。PRD は [prd.md](prd.md)。行番号は 2026-10-30 の
`main`（`a869e71`。PR #69 のあとは `guard.rs`・`cli.rs`・`ingest.rs`・`no_python.rs` がずれる）のもの。

## 概要

3 ページ（`/admin`・`/ai-search`・`/blog-admin`）の管理画面を、`admin.html` 1 枚の「状況 / 写真 / 設定」の
3 タブに統合する。旅行ブログは環境変数 `GEOFRAME_BLOG`（既定 `0`）で無効にする。写真タブは一覧と AI 検索を
同居させ、カードを 1 種類にする。設定タブは新しい読み取り専用 API `GET /api/admin/config` で、起動時の設定を
出どころつきで表示する。サーバーは Rust（axum）、画面は素の JavaScript（classic script）を `build.rs` で埋め込む
方式を続ける。PR は 3 本（「実装の順番」）。

## 今の作り（調査結果）

### サーバー

- 設定: `Config`（`crates/geoframe/src/config.rs:158-210`）。`load()`（`config.rs:221-`）が環境変数を読む。空文字は未設定
  （`config.rs:225` の `get` クロージャ）。起動引数は `Overrides`（`config.rs:140-145`、`listen` と `data_dir` だけ）で、
  引数があるときは `GEOFRAME_HOST`・`PORT`・`DATA_DIR` を読まない（`config.rs:228-253`）。`GEOFRAME_ALLOWED_HOSTS` は
  `get` でなく `lookup` を直接使う（`config.rs:258`）。AI 設定は `AiConfig::load(&dyn Fn)`（`ai/settings.rs:971`）。
  **値の出どころ（引数 / 環境変数 / 既定）は記録していない。** `log_summary()`（`config.rs:503-527`）が起動ログ用の要約。
  `GEOFRAME_INGEST` の読み方（`config.rs:344-355`。`0`/`1` だけ受ける）が新しいフラグの手本。
- AI 設定 JSON: `RawConfig`（`ai/settings.rs:415-422`）→ 検査済みの `AiConfig`（`settings.rs:943`）、用途ごとの
  `TaskSettings`（`settings.rs:210`）、`CloudTarget`（`settings.rs:201`。鍵のパスだけ持ち、秘密は持たない）。
  `Debug` は手で実装して中身を隠す（`settings.rs:224-232`、`246-253`、`265-271`、`950`、`1214`）。
  用途が送れない理由は `Disabled`（`settings.rs:1161-1180`）で、`ops.rs:92` の `gate` が計算するが応答には出していない。
  周期の秒数は `config.ai.legacy().cycle(task).1`（`settings.rs:921`）。
- AppState: `config: Arc<Config>`（`server.rs:62`）。`AdminContext`（`admin.rs:45-75`）は `config.clone()` を持つ。
- 経路: `router()`（`server.rs:837-856`）→ `guard`（`569`。GET/HEAD/POST 以外は 501、Host 不許可は 403、
  **`post::route_of` に無い POST は 403**（`server.rs:600`））→ `site()`（`670`）。POST は `post::route_of`
  （`post.rs:81-92`）→ `post::handle`（CSRF は `csrf.rs:65-80`、`post.rs:145-170`）。GET は `/api/admin/status`
  （`server.rs:717`）・`/api/admin/photos`（`720`）が**前方一致**、`/api/v1/photos`（`714`）は完全一致。`/api/blog/`
  （`732-746`）は `blog_route`（`399-406`）の 4 本だけ 200、他は 404。ページは `webpath::page_target`
  （`webpath.rs:96-103`）→ `statics::serve_embedded_key`（`statics.rs:278-313`）。ラベルは `server.rs:779-783`。
  転送（`Location`）のヘルパは無い。
- `INGEST=0` の 503: `update::writer_or_unavailable`（`update.rs:85-87`）→ `post.rs:121-124`。`INGEST=0` は
  writer が無いだけで、**読み取りの接続は使える**（`db.rs:217`）。
- `GET /api/admin/status`（`admin.rs:186-241`、組み立ては `server.rs:230-274`）: `sync`・`db`（`total`・`no_loc`・
  `favorite`・`hidden`・`by_loc_source`・`infer_buckets`・`quality`・`ai`・`loc`）・`disk`・`ai_jobs`・`ai_pauses`・
  `ai_task_blocks`・`ai_waits`・`ai_config_invalid`・`ai_location_holds`・`ai_scenes`・`server_time`。
  **AI の設定値と「今日の送信数」は出ていない。** `budget::usage(conn, TaskKind, now_utc_unix, cycle_secs) -> rusqlite::Result<Usage{day, cycle}>`
  （`ai/budget.rs:154`）は worker（`ai/worker.rs:357`）と試験が使う。日の上限は `TaskSettings.daily_request_limit`
  （`settings.rs:210-222`）。
- `GET /api/admin/photos`（`admin.rs:938-`）: `filter`（`Filter`、`admin.rs:692-745`。`all`・`noloc`・`fav`・`hidden`・
  `inferred`・`manual`・`suggest`。`condition()` は `:728`、`count_photos` は `:174`）、`page`・`per`（上限 200）。
  `AdminPhoto`（`admin.rs:772-797`）は `fav`・`hidden`・`loc_source`・`infer_bucket`・`rotation`・`source`・`hint`・
  `suggest`・`scene` を持つ。**`hint` は `noloc`・`suggest` のフィルタのときだけ付く**（`with_hints`、`admin.rs:742`・`1019`）。
  `suggest` は候補のある写真に付く（`suggestions`、`admin.rs:888`）。
- `GET /api/ai/search`（`ai/query.rs:66-`、SQL は `search_view.rs:190`、写真の JSON は `:207-215`）: `mode`
  （`semantic`・`recommended`・`empty`）、`groups[].photos[]`。写真は `favorite`（一覧の `fav` と名前が違う）、
  `similarity`、`recommended`、`subject` 等、`visual_cluster`・`visual_representative`。**`source`・`rotation`・
  `loc_source`・`hint`・`suggest` は無い。** 503 は AI 無効（`query.rs:79-81`）・残り時間なし・クライアント無し。
  429 はプロセス内の毎分の上限（`query.rs:86-101`）。空の `q` は `recommended`（SQLite の推奨）。
- 埋め込み: `build.rs:25` の `ROOTS = ["static", "vendor"]` を再帰で表にする。ファイルの増減に `build.rs` の変更は不要。
  `embedded.rs:63-79` の試験が `static/blog-admin.html`・`static/ai-search.html` の名前を直接持つ。
  `tests/static_files.rs:375-417` が `/blog-admin`・`/ai-search` の 200 を確かめる。
- 版: `CARGO_PKG_VERSION`（`cli.rs:314-317`）。コミットハッシュと起動時刻は持っていない。
- 試験: `cargo test` が `tests/admin_ui_ai_waits.rs` から `node web/tests/admin_ai_ops_render.mjs` を起動する
  （ローカルで Node が無ければ省略、**CI では Node 必須**。`.github/workflows/test.yml:124-126`）。Node を使う試験は
  `tests/no_python.rs:79` の許可リストに要る。環境変数つきの起動は `tests/common/mod.rs:397` の `spawn_with_env`。

### 画面

- `admin.js`（560 行）: 共通ヘルパ（`$`・`el`・`toast`・`postJSON`）、タブ（`showTab`、`admin.js:43`）、STATUS
  （`loadStatus`、`aiOpsCells`、`massDeleteNotice`）、一覧（`setFilter`・`loadList`・`cardFor`、`admin.js:239-320`）、
  フラグ（`setFlag`・`rotatePhoto`）、位置指定モーダル（MapLibre、地名検索、AI 候補、`admin.js:345-520`）。
  `openLocModal(it)`（`admin.js:498`）は**渡された `it.suggest`・`it.hint` を使うだけ**で、API からは取らない。
  グローバル状態は `curFilter`・`curPage`・`curPhoto`・`picked`・`mini` 等。起動は末尾（`admin.js:525-`）で直接実行。
- `ai-search.js`（463 行、IIFE）: `photoCard`（`<template id="photo-template">`）、ビューア（`renderModal`・`openModal`・
  `stepModal`、スワイプ・キー操作）、場面の描画（`render`・`visualOrder`・`applyGroupVisibility`）、`search`、
  `setPhotoFlag`。空の検索語で `recommended` を表示。失敗は `#message` と toast。
- CSS: `admin.css` と `ai-search.css` は `:root` の変数名が同じで値が違う（`--bg`・`--panel`・`--ink`…）。
  クラスは `.card` と `.photo-card` で別だが、`.hidden`・`#toast`・`.badge`・`.tag` は衝突しうる。
- 古い記述: `admin.html:9-14`（Phase 4、`server.py`、`handlers.py`）、`admin.js:384`・`456`（`server.py`・`loc_suggest.py`）、
  `ai-search.html:13`（「表示画面」→ `/`）。
- 試験: `web/tests/admin_ai_ops_render.mjs:10-30` は `vm` に最小の `document`・`fetch`・タイマーを渡して `admin.js`
  全体を評価し、グローバルの `aiOpsCells` を直接呼ぶ。**`location` は無い。** `classList.toggle`・`dataset`・
  `querySelector` は空の代用品で、要素の同一性やイベントは保持しない。

## 方針

| 論点 | 選んだ案 | 選ばなかった案と理由 |
|---|---|---|
| ページの数 | `admin.html` 1 枚。`/`・`/admin`・`/ai-search`（PR ② から）・ブログ無効時の `/blog-admin` は同じ HTML。JS が `location.pathname` でタブを選ぶ | HTTP の転送（302）: ヘルパが無く、`static_files.rs` の試験と「ページは 200」の前提を変える。利点が無い |
| 旅行ブログ | `GEOFRAME_BLOG`（既定 `0`）。無効なら `/api/blog/*` の GET 4 本と POST 2 本（save・export）は 404、ページは管理画面 | 経路の削除: `blog_write`・`post.rs` の `Route` が未使用になり clippy `-D warnings` で落ちる。作り直す予定なのでコードは残す |
| 無効時の `/static/blog-admin.*` | 配り続ける（JS は API が 404 で動かない。中身は静的で無害） | 埋め込みの配信側にも判定を足す: 配信の経路に設定を持ち込む割に得るものが無い。文書に明記する |
| チップの件数 | `GET /api/admin/status` の `db.filters` に、`Filter::condition`（既存）と**同じ条件**で 7 件数を足す。`Filter::ALL`（**新設**の定数配列）を回す | 一覧 API の `total` を 7 回呼ぶ: 要求が増え、件数と条件がずれうる |
| 一覧と検索の同居 | 写真タブの上に検索欄。一覧（チップ 7 種）と検索結果（場面ごと）を切り替える。「AI おすすめ」は検索欄の横の**ボタン**（`q` 空の `recommended`）で、チップではない | 検索を別タブに残す: ユーザーの要望に反する。おすすめをチップにする: チップは一覧のフィルタで、おすすめは検索の一種なので混ぜない |
| 検索結果の写真の位置情報 | 検索 API の写真に `source`・`rotation`・`loc_source`・`infer_bucket`・`hint`・`suggest` を足す。一覧と同じ補完関数（`admin.rs` から切り出す）を使う。一覧側も**全フィルタ**で位置なし写真に `hint` を付ける | 検索結果だけ「位置なし」表示: R10 を満たさない。画面側で後から取る: API が増え、カードが 2 段階で描かれる |
| カード | 1 種類。入力を `normalizePhoto` で共通の形にして `el()` で組む（`<template>` は使わない。試験の代用品で動かすため） | 2 種類のまま: 操作の差が残る |
| JS の分割 | `admin.js`（共通ヘルパ・タブ・状況・設定・起動）と `photos.js`（写真タブ）。classic script 2 本をこの順で読む。定義だけを置き、初期化は `DOMContentLoaded` の `boot()` 1 か所で行う | 1 本: 1,000 行超で読みにくい。ES modules: 試験の `vm` の読み方を変える必要がある |
| CSS | `admin.css` 1 本に統合。カード・タグ・トースト・チップは共用の名前。場面のまとまりとビューアだけ `.ps-` を付ける | 2 本のまま: `:root` の変数が衝突する |
| 設定の出どころ | `load()` の流れとは別に、項目の列挙 `EnvItem`（**新設**の enum）と `Overrides` から `sources` を作る | `get` クロージャで記録: `AiConfig::load` が `&dyn Fn` を要求し、引数優先の経路と `ALLOWED_HOSTS` は `get` を通らず、AI 設定不正の早期 return で未読が残る |
| 設定 API の出す範囲 | **全項目を設定どおりの値で出す**（ユーザーの決定。鍵のパス・接続先・連絡先・project を含む）。出さないのは `Config` が持たないもの（鍵の中身・トークン）と内部の値（`embedding_contract`・`config_hash`）だけ。`EnvItem` の **ワイルドカード無しの `match`** で項目ごとに組み立て、`Config` を `Serialize` しない | 伏せる案（有無だけ・件数だけ・同一の印）: 安全面レビューの案だったが、ユーザーが「設定項目の全量が分かるようにしたい」と決めた。`Config` を丸ごと返す: 項目の追加が気づかれずに応答に乗る |
| 設定 API のメソッド | GET だけ（HEAD は既存の管理 API と同じく 403） | HEAD も 200: `site()` の管理 API は全て GET の分岐の中にあり（`server.rs:669` の説明）、例外を作る理由が無い |
| 今日の送信数 | `GET /api/admin/config` が読み取り接続で `budget::usage` を呼ぶ。取れなければ `null` にして 200 | 状況 API に足す: 設定タブと状況タブで二重に出る |
| 版・起動時刻 | `CARGO_PKG_VERSION` と、`serve()` で取った `started_at`（`AppState`） | コミットハッシュ: `build.rs` に git を読む処理を足すことになる。今回は見送り |

## 構成

```mermaid
flowchart LR
  subgraph browser[ブラウザ: admin.html]
    shell[admin.js 共通ヘルパ・タブ・状況・設定・boot]
    photos[photos.js 一覧・検索・カード・ビューア・位置指定]
    shell -- グローバル関数 --> photos
  end
  subgraph server[geoframe serve]
    site[server.rs site()]
    status[/api/admin/status + db.filters/]
    list[/api/admin/photos 全フィルタで hint/]
    search[/api/ai/search + 位置の項目/]
    config[/api/admin/config 新設/]
    blog[/api/blog/* GEOFRAME_BLOG で 404/]
    pages[webpath::page_target → admin.html]
  end
  shell --> status
  shell --> config
  photos --> list
  photos --> search
  photos -- setflag/rotate/setloc --> site
  browser --> pages
```

### 画面の構造（`admin.html`）

- ヘッダ: ロゴ、`#servertime`、ナビ `button.tab[data-tab=status|photos|config]`（設定タブは PR ③ で足す）。
  旅行ブログのリンクは無い。
- `#v-status`: 今のパネル（同期・DB 内訳・AI 運用・ディスク）＋「要対応」のセル（位置なし・AI 候補あり。押すと
  `showTab('photos')` と `setFilter(...)`）。
- `#v-photos`: 検索バー（`#q`、検索ボタン、「AI おすすめ」ボタン、「除外も検索」チェック、`#search-note`）、
  チップ列（`.chip[data-filter]`、件数 `.chip .n`）、ページング、`#cards`（一覧）、`#groups`（検索結果。場面ごとの
  `section.ps-group`）、`#empty`。PR ① では検索バーの代わりに `/ai-search` への一時的なリンクを置く。
- `#v-config`: 「安全面」の表の項目（描き方は「テスト計画」の `config_render.mjs` と同じ土台）。
- モーダル: `#modal`（位置指定。今のまま）、`#viewer`（ビューア。`ai-search.html` の `#photo-modal` を移植、`.ps-viewer`）。

### 読み込みと起動の順序

```html
<script src="/static/js/admin.js"></script>   <!-- 定義: $ el toast postJSON fmtDur showTab loadStatus aiOpsCells loadConfig boot -->
<script src="/static/js/photos.js"></script>  <!-- 定義: initPhotos setFilter search cardFor normalizePhoto openLocModal ... -->
```

`admin.js` の末尾は `document.addEventListener('DOMContentLoaded', boot)` だけにする。`boot()` は
`initPhotos()`（`photos.js` が定義。呼ぶ時点で解決される）→ `showTab(initialTab())` の順。`initialTab()` は
`typeof location === 'undefined'` なら `'status'`、`location.pathname` の先頭の連続する `/` を 1 つに・末尾の `/` を
取り除いた結果が `'/ai-search'` なら `'photos'`（比較は `===` で、オブジェクトの添字では引かない）（検索欄に
フォーカス）、それ以外は `'status'`。既存の試験は `document.addEventListener` が空の代用品なので `boot` は走らず、
`aiOpsCells` の呼び出しはそのまま通る。URL は書き換えない（`history` は使わない）。

### 写真タブの状態

```js
const ps = {filter: 'all', page: 1, view: 'list' /* 'list' | 'search' */,
            query: '', includeHidden: false, entries: [] /* ビューア用の平らな配列 */};
```

- `setFilter(f)`: `view='list'`、`page=1`、一覧を読む（`GET /api/admin/photos`）。チップは 7 種（`all`・`noloc`・`suggest`・
  `inferred`・`manual`・`fav`・`hidden`）。
- `search(q)` / 「AI おすすめ」: `GET /api/ai/search?q=<q>&groups=12[&include_hidden=1]` を呼び、**成功したときだけ**
  `view='search'` にして `#groups` に描き、`#cards`・ページングを隠す。失敗したときは `view`・`filter`・`page` を
  変えず、`#search-note` に理由を出す（下の表）。「✕ 検索を閉じる」で `view='list'` に戻し、直前の `filter`・`page`
  で一覧を読み直す。
- 検索の失敗の表示（HTTP の状態と応答の `error` から決める。文言は固定）:

| 応答 | `#search-note` |
|---|---|
| 503 で `error` に `AI` 無効の旨（`query.rs:79-81` の `Disabled`） | 「AI は無効です（`GEOFRAME_AI_ENABLED`）。一覧は使えます」 |
| 503（それ以外。索引が使えない・接続先なし） | 「検索の索引がまだ使えません。状況タブの AI の欄を見てください」 |
| 429 | 「検索が多すぎます。1 分待ってから試してください」 |
| 400（`error` は `bad_params`。`server.rs:817` の `ai_search_error`） | 「検索語は 300 文字以内で入力してください」（画面が固定の日本語に対応付ける。`groups` は画面が固定で送るので誤りにならない） |
| その他・通信失敗 | 「検索できませんでした: <error>」 |

### カード（`photos.js` の `cardFor(photo, index)`）

`normalizePhoto(raw, origin)` が一覧（`origin='list'`）と検索（`origin='search'`、**「`GET /api/ai/search` の変更」の項目追加の後**）の JSON を
共通の形にする:

| 共通の名前 | 一覧（`/api/admin/photos`） | 検索（`/api/ai/search`、追加後） |
|---|---|---|
| `favorite` | `fav` | `favorite` |
| `hidden`・`id`・`name`・`thumb`・`taken_at`・`lat`・`lon`・`place`・`pref`・`city` | そのまま | そのまま |
| `loc_source`・`infer_bucket`・`rotation`・`source`・`hint`・`suggest` | そのまま | そのまま（追加する項目） |
| `ai` | `null` | `{similarity, recommended, subject, eyes, pet_state, face_crop, horizon_status, reasons, concerns, quality…}` |
| `visual` | `null` | `{cluster, representative, similarity}` |

描く順（R10。一覧と検索で同じ）:

1. 画像（`loading=lazy`）。検索結果では右上に `NN% MATCH` / `NN SCORE`。クリックでビューア。
2. **1 行目（主役）**: 位置があれば地名（`pref city place`、無ければ座標 4 桁）。位置が無ければ、`suggest` があれば
   「AI 候補: <place_name>」、無ければ `hint` があれば「近い時刻の位置: <label>（<差>）」、どちらも無ければ「手がかりなし」。
3. 2 行目: `taken_at`（無ければ「撮影時刻なし」）と `name`。
4. タグ: 由来（`EXIF`・`DRIVE`・`推定 <bucket>`・`手動`・`位置なし`）。`infer_bucket` は `inferred` のときだけ。検索結果では
   `AI PICK`・`FAVORITE`・被写体のバッジを追加。
5. 操作: `★`・`🚫`・`位置`（常に）、`↻`（`thumb` があり `source === 'drive'` のとき。一覧・検索で同じ条件）。

`setFlag` は応答の `favorite`・`hidden` で同じ `id` の**全カードとビューア**を更新する（`ai-search.js` の
`updatePhotoState` の作りを引き継ぐ）。`openLocModal` は一覧・検索のどちらのカードからも開け、保存後は 1 行目を地名に、
タグを `手動` に描き直す。

### 場面ごとの表示（検索結果）

`ai-search.js` の `render`・`visualOrder`・`applyGroupVisibility`・`renderModal`・`openModal`・`stepModal` を `photos.js` に
移す。DOM の id は `ps-` の接頭辞に改名（`#photo-modal` → `#viewer` 等）。ビューアは `ps.entries` を見るので、一覧でも
検索でも同じ関数で前後に動く。AI が返す自由文（`summary`・`reasons`・`concerns`）は `textContent` で描く。

## インターフェース

### 環境変数 `GEOFRAME_BLOG`（新設。Rust だけ）

- 値: `0`（既定、無効）か `1`（有効）。それ以外は `INGEST` と同じく設定の誤り（起動しない）。
- 無効のとき:
  - `GET /api/blog/*`（`trips`・`trip`・`candidates`・`draft` と、その他の `/api/blog/` 配下）: `server.rs:732` の
    `blog_route` の前で 404（`statics::error_page(NOT_FOUND, "Not Found", "Nothing matches the given URI")`。本文に
    フラグのことは書かない。ラベル `/api/blog/{*path}`）。
  - `POST /api/blog/save`・`/api/blog/export`: `site()` の POST 分岐で `route_of` の直後（`server.rs:680`）に、
    `Route::BlogSave | Route::BlogExport` なら 404。本文は POST の他の誤りと同じ JSON（`error_response(NOT_FOUND,
    "Not Found")`、`server.rs:513`）で、`postJSON` が読める形にする。CSRF の検査（`post::handle`）と `INGEST=0` の
    503 より前。`Route` の値で判定するので、パーセント符号化などの書き方の違いで漏れない。
    **優先順位: 無効の 404 ＞ CSRF の 403 ＞ `INGEST=0` の 503。** それ以外の `/api/blog/` への POST は今までどおり
    `guard` で 403（`server.rs:600`。変えない）。
  - `/blog-admin`: `server.rs:777` で `page_target` の結果が `static/blog-admin.html` なら `static/admin.html` に差し替える。
    ラベルは `/blog-admin` のまま（アクセスログで `/admin` と区別できる。有効か無効かは起動ログの `blog=on/off` で分かる）。
    `security_headers` は `page_target` を自分で計算するので、HTML のヘッダ（`X-Frame-Options` 等）は今までどおり付く。
  - `/static/blog-admin.html`・`/static/js/blog-admin.js`・`/static/blog-admin.css` と、書き出し済みの `/blog_exports/`
    は配る（「方針」の表の判断。静的な内容で、API が 404 なら画面は何もできない。書き出しは利用者が自分で作ったもの）。
- 有効のとき: 今と同じ。
- `Config.blog: bool`、`log_summary()` に `blog=on/off`、`config.rs` 冒頭の表、`docs/reference/configuration.md` の
  「基本・取り込み」の節に 1 行、`deploy/README.md` に「旅行ブログは `GEOFRAME_BLOG=1` で戻す。**新しいバイナリへ
  更新した次回の起動から**、環境ファイルを変えなくても無効になる」。

### `GET /api/admin/status` の追加キー

```json
"db": { "...": "今のまま",
  "filters": {"all": 6540, "noloc": 123, "suggest": 12, "inferred": 410, "manual": 88, "fav": 57, "hidden": 31} }
```

`Filter::ALL`（新設。7 件の配列）を回して `count_photos(conn, f.condition())`（既存）で数える。`all` は `db.total` と同じ値。

### `GET /api/admin/photos` の変更

`hint` を**全フィルタ**で、位置の無い行に付ける（`with_hints` を廃し、ページ内に位置なしの行があるときだけ
`time_anchors` を読む）。`suggest` は今のまま。応答の形は変えない。

### `GET /api/ai/search` の変更（契約外の管理用 API。`photo-feed-contract.md` の対象外）

写真の JSON（`search_view.rs:207-215`）に `source`・`rotation`・`loc_source`・`infer_dt_s`・`infer_bucket`・`hint`・
`suggest` を足す。SQL（`search_view.rs:190`）に `p.source,p.rotation,p.loc_source,p.infer_dt_s` を足し、`hint`・`suggest`・
`infer_bucket` は `admin.rs` の `time_anchors`・`nearest_hint`・`suggestions`・`infer_bucket` を `pub(crate)` の補完関数
`admin::decorate_locations(conn, ctx_windows, &mut [LocFields])` にまとめ、一覧と検索の両方から呼ぶ。検索では
**返すグループと写真が確定した後**に、返す写真の ID を重複なしで集めて **1 回だけ**呼ぶ（候補の全件読み込みの段には
入れない）。位置の無い写真が返す結果に無ければ `time_anchors` は読まない。

### `GET /api/admin/config`（新設。完全一致、GET のみ。PR ③）

```json
{
  "ok": true,
  "version": "0.1.0",
  "started_at": "2026-10-30 08:25",
  "uptime_s": 47520,
  "items": [
    {"key": "GEOFRAME_DATA_DIR",      "value": "/srv/example/geoframe",            "source": "arg"},
    {"key": "GEOFRAME_HOST",          "value": "0.0.0.0",                          "source": "default"},
    {"key": "GEOFRAME_ALLOWED_HOSTS", "value": "localhost, geoframe.example.test", "source": "env"},
    {"key": "GEOFRAME_INGEST",        "value": "1",                                "source": "env"},
    {"key": "GEOFRAME_BLOG",          "value": "0",                                "source": "default"},
    {"key": "GEOFRAME_DRIVE_KEY",     "value": "/etc/example/drive_key.json",      "state": "present", "source": "env"},
    {"key": "GEOFRAME_DRIVE_FOLDER",  "value": "ExampleFolder",                    "source": "env"},
    {"key": "GEOFRAME_BLOG_PLANS_DIR","value": null,                               "source": "default"},
    {"key": "GEOFRAME_TILE_UA",       "value": "GeoFrame/0.1 (admin@example.test)", "source": "env"},
    {"key": "GEOFRAME_RANK_MODEL",    "value": "/srv/example/geoframe/models/rank_model.json", "state": "absent", "source": "default"},
    {"key": "GEOFRAME_AI_CONFIG",     "value": "/etc/example/ai-config.json",      "state": "loaded", "source": "env",
                                      "modified_at": "2026-10-30 14:07"}
  ],
  "ai": {
    "configured": true, "invalid": null, "enabled": true, "version": 1,
    "local_base_url": "http://192.0.2.10:8100",
    "embedding_space": {"space_id": "example-space-1", "model_id": "example-embed", "revision": "r1",
                        "image_preprocess": "img-v1", "text_preprocess": "txt-v1", "dimension": 768, "normalization": "l2"},
    "tasks": [
      {"task": "image_embedding", "provider": "local", "model": "example-embed", "service_id": "example-embed-svc", "source": "ai_config"},
      {"task": "photo_judge", "provider": "cloud", "model": "example-judge", "source": "ai_config",
       "allow_cloud": true, "include_hidden": false, "daily_limit": 40, "today_utc": 12,
       "credential_path": "/etc/example/credentials.json", "credential": "present",
       "project": "example-project", "region": "example-region1", "disabled": null}
    ]
  }
}
```

- `items` は平らな 1 列で、**`EnvItem::ALL`（「`Config` の変更」）の全項目**を必ず含む（値が既定でも出す。未設定で既定も
  無い項目は `value: null`）。分類・説明文・単位・並びは画面側（`admin.js` の表）が持つ。API は `key`・`value`・`source` と、
  ファイルの項目だけ `state`・`modified_at`。
- `value` は `EnvItem` の**ワイルドカード無しの `match`** で `Config` の型つきの値から作る（`Display` や `Debug` の文字列化に
  頼らず、項目ごとに書く）。項目を足して組み立ての枝を書き忘れるとコンパイルで落ちる。**全項目を設定どおりの値で出す**
  （ユーザーの決定、2026-10-30。「安全面」の表）。
- `source` は `arg`・`env`・`default`。AI 設定 JSON 由来の値は `ai_config`。
- `state`（`exposure()` が `File` の項目）: `present`/`absent`/`unreadable`（要求のたびに `fs::metadata` で見る。エラー文は
  出さない）。`GEOFRAME_AI_CONFIG` は `loaded`/`invalid`/`absent` と `modified_at`。
- `ai.tasks[].disabled` は `Disabled`（`settings.rs:1161-1180`）の固定コード（`allow_cloud_false`・`no_daily_limit` 等）か `null`。
- `today_utc` は `budget::usage(conn, task, now_utc_unix, config.ai.legacy().cycle(task).1)?.day`（クラウドの用途だけ）。
  **UTC 日の件数**で、画面も「今日（UTC）」と書く。読めない（DB が無い・表が無い・エラー）ときは `null` にして
  **応答は 200 のまま**。設定の表示は DB の状態に依存しない。
- `ai.invalid` は `AiConfig::invalid` のコード（設定不正のとき。`tasks` は空）。`configured=false`（`GEOFRAME_AI_CONFIG`
  未設定）なら `tasks` は空。
- `items[].modified_at`（`GEOFRAME_AI_CONFIG` だけ）は起動時に `fs::metadata` で控えた `mtime`（`Config.ai_config_modified`。
  パスは `Config` に持たない）。読めなければ省く。
- 失敗の応答は作らない（設定は起動時に読んだ値で常に返せる）。HEAD は既存の管理 API と同じく 403。`/api/admin/configX`
  や末尾 `/` は完全一致に当たらず静的配信の判定に回って 403。
- `json_response`（`server.rs:497`）で返す（`Cache-Control: no-store` が付く）。
- 画面は設定タブを開いたときだけ読み、周期的には読み直さない。

### `Config` の変更（`config.rs`。PR ③）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source { Arg, Env, Default }

/// 設定 API での出し方（「安全面」の表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure { Value, File /* 値に加えてファイルの有無 */ }

/// Rust が読む環境変数（1 つ 1 つを列挙。`docs/reference/configuration.md` と `tests/env_reference.rs` が名前を照合する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvItem { DataDir, Host, Port, AllowedHosts, Log, Ingest, Blog, DrivePollMin, /* … */ AiVisualSimilarity }
impl EnvItem {
    pub const ALL: &[EnvItem] = &[EnvItem::DataDir, EnvItem::Host, /* … */];
    pub fn name(self) -> &'static str { match self { EnvItem::DataDir => "GEOFRAME_DATA_DIR", /* ワイルドカード無し */ } }
    pub fn exposure(self) -> Exposure { match self { EnvItem::DriveKey => Exposure::File, /* ワイルドカード無し */ } }
}

pub struct Config { /* 既存 */ pub blog: bool, pub sources: BTreeMap<&'static str, Source>,
                    pub ai_config_modified: Option<SystemTime> }
```

`sources` は `load()` の最後に **`load()` の流れとは独立に**作る: `EnvItem::ALL` を回し、`overrides.listen` があれば
`GEOFRAME_HOST`・`GEOFRAME_PORT` を `Arg`、`overrides.data_dir` があれば `GEOFRAME_DATA_DIR` を `Arg`、それ以外は
`lookup(name)` が空でない文字列なら `Env`、さもなくば `Default`。AI 設定不正の早期 return の経路でも作る。
`Config` の `Debug`（手実装、`log_summary` だけ）と `log_summary()` は変えない（`blog` の on/off だけ足す）。
試験: 既定だけで全項目 `Default`、環境変数 1 つで `Env`、引数ありで `Arg`（`HOST` も `PORT` も）、空文字は `Default`、
`sources` のキーが `EnvItem::ALL` と一致、`EnvItem::ALL` の名前が `tests/env_reference.rs` の名前の集合と一致（`ALL` への
追加漏れを捕まえる。`name()`・`exposure()`・応答の組み立ての漏れはコンパイルで落ちる）。

### `AppState` の変更（PR ③）

`started_at: SystemTime`（`serve()` で取る）。`config` の API は `Arc<Config>`・`started_at`・`catalog`（今日の送信数）を読む。

### 変わらないもの

`GET /api/v1/photos`・`/derivatives/`（契約）、`POST /api/admin/setflag|rotate|setloc`、`GET /api/admin/geocode`、
`GET /api/ai/status`、CSRF・Host 検査・`INGEST=0` の 503、`guard`。

## データ

- DB のスキーマは変えない。`db.filters`・`hint`・`suggest`・`today_utc` は読むだけ。
- 設定ファイルは読むだけ。移行なし。

## 安全面

- **認証なし**の前提は変えない。新しい API は GET のみで、書き込みは今の 5 本の POST（CSRF・`INGEST` ガード）のまま。
  CORS のヘッダは出さず、Host 検査があるので、LAN 内の別のページからブラウザ経由で `fetch` しても応答は読めない
  （`Origin` 違いでレスポンスが遮られる）。別の端末から直接開けば読めるのは今の `/api/admin/status` と同じ。
- **設定 API の項目ごとの扱い（唯一の仕様。PRD の R13・R14・AC8 はこの表を指す）**。2026-10-30 にユーザーが
  「設定項目の全量が分かるようにしたい。鍵のパスも出す」と決めたので、**設定ファイルの写し**として全項目を値で出す。
  安全面レビュー（Opus、2026-10-30）が挙げた「既存の『出さない』決まり」は**ログ・`ai-jobs status`・文書・PR** に対するもので、
  そこは変えない。`/api/admin/config` だけを意図した例外にし、PR ③ で ai-config.md（`:10`）と `config.rs`（`:176`・`:206`・
  `:209`・`:213`）のコメントに「`/api/admin/config` は出す」を書き足す。根拠: 認証の無い LAN サービスで、同じ LAN の端末は
  今でも POST で書き込めるため、読める範囲が増えても危険はほとんど増えない（レビューの評価）。残る危険は**画面の共有**
  （スクリーンショット）なので、設定タブの先頭の案内に「鍵のパス・接続先・連絡先が含まれる。画面を共有しない」と書く。

| 項目 | 出し方（`Exposure`） | 備考 |
|---|---|---|
| `GEOFRAME_DATA_DIR`・`PHOTOS_DIR`・`HOST`・`PORT`・`ALLOWED_HOSTS`・`LOG`・`INGEST`・`BLOG`・`DRIVE_POLL_MIN`・`FETCH_PER_CYCLE`・`QUALITY_PER_CYCLE`・`DRIVE_FOLDER`・`BLOG_PLANS_DIR`・`INFER_MIN`・`INFER_OFFSET_MIN`・`TILE_UA`・`TILE_CACHE_MAX_MB`・`TILE_FETCH_PER_MIN`・`AI_ENABLED`・`AI_POLL_S`・`AI_BACKLOG_POLL_S`・`AI_EMBED_PER_CYCLE`・`AI_JUDGE_PER_CYCLE`・`AI_SEARCH_PER_MIN`・`LOC_SUGGEST`・`LOC_SUGGEST_CLUE_PER_CYCLE`・`LOC_SUGGEST_GEMINI_PER_CYCLE`・`EMBED_BACKEND`・`AI_TIME_WINDOW_MIN`・`AI_SIMILARITY`・`AI_SHORTLIST_MAX`・`AI_VISUAL_SIMILARITY` | **値**（`Value`） | `ALLOWED_HOSTS` は `localhost` を含めた並び。`TILE_UA` は連絡先入り。`BLOG_PLANS_DIR` 未設定は `null`（埋め込み） |
| `GEOFRAME_DRIVE_KEY`・`RANK_MODEL`・`AI_CONFIG` | **値（パス）＋ファイルの有無**（`File`。`state`。`AI_CONFIG` は `modified_at` も） | パスは起動時の値、有無は要求のたびに見る。鍵の**中身**は `Config` が持たないので出ない |
| AI 用途の `provider`・`model`・`service_id`・`allow_cloud`・`include_hidden`・`daily_limit`・`today_utc`・`credential_path`（＋有無）・`project`・`region`・`disabled` | **値** | 認証ファイルの中身は持たないので出ない |
| `local_base_url`・`embedding_space` の全項目（`space_id`・`model_id`・`revision`・`image_preprocess`・`text_preprocess`・`dimension`・`normalization`） | **値** | — |
| `embedding_contract`・`config_hash`・クラウドの固定宛先・トークン・鍵の中身 | **出さない** | 内部の値か、`Config` が持たないもの |

- `Config` の `Debug`（手実装）と `log_summary()`、`ai-jobs status`、`/api/admin/status` の `ai_task_blocks` は**変えない**
  （ログと CLI は今までどおり伏せる）。
- 単体試験（AC8）: 合成の `Config`・`AiConfig` に、**検査に通る形の** ASCII の目印を入れる（鍵のパス `/tmp/mark-key-1.json`、
  `local_base_url` の `http://mark-host.invalid:8100`、`space_id` 等の `mark-space-…`、`service_id` の `mark-svc`、`TILE_UA` の
  `mark@example.test`、`project` の `mark-project`、`DRIVE_FOLDER` の `mark-folder`、`RANK_MODEL`・`BLOG_PLANS_DIR`・`AI_CONFIG` の
  パスの `mark-dir`、`ALLOWED_HOSTS` の `mark-host.example.test`）。鍵のパスが指す合成ファイルの中身は `SECRET-mark`。
  応答を作る前に AI 設定が `ready` で用途が 5 つあることを確かめ、検査対象ごとに分けて確かめる: (a) 応答の JSON に
  `items` の全項目があり、**上の目印がすべて値として含まれ**、`SECRET-mark`（鍵の中身）が含まれない。(b) `format!("{:?}", config)`
  と `log_summary()` に目印も `SECRET-mark` も、モデル名・region も含まれない（変えないことの確認）。(c) `router` を通した
  結合試験で本文に `SECRET-mark` が無い。
- 404 の応答（GET は `statics::error_page`、POST は `error_response`）の本文に、無効の理由（`GEOFRAME_BLOG`・「無効」）を
  書かない。試験で本文を確かめる。
- 画面は `textContent` で描く。今ある唯一の `innerHTML`（`admin.js:450`。地名検索の OSM の出典。PR ② で `photos.js` へ
  移る）は、出典のリンクを `el('a')` と `textContent` で組み直して無くす。そのうえで `web/static/js` に
  `innerHTML`・`outerHTML`・`insertAdjacentHTML`・`document.write` が**1 つも無い**ことを grep する試験
  （`tests/no_python.rs` と同じ流儀）を足す。MapLibre の `attribution`（定数）は MapLibre が描くので対象外。AI が返す自由文（`summary`・`reasons`・`concerns`）も `textContent`。
  検索語は API 側で 300 字の検査があり、アクセスログにクエリは出ない（今のまま）。
- 外部への通信は増えない（地図タイル・地名検索・AI は今のまま）。
- 見送り（別の機会に）: `X-Content-Type-Options: nosniff` と HTML の `script-src 'self'` の追加。MapLibre が `blob:` の
  worker と inline の style を要るかの確認が要るので、この PRD の範囲に入れない。

## テスト計画

Node は CI で必須（runner に同梱。`package.json` は置かない）。Node の試験を足すときは `tests/no_python.rs:79` の
許可リストと、`tests/admin_ui_ai_waits.rs` と同じ起動の殻（新設 `tests/admin_ui_photos.rs`）が要る。`document` の
代用品は **要素の同一性・子の配列・`classList`・`dataset`・`addEventListener`（登録した関数を保持して `click()` で
呼べる）・`focus()` と `document.activeElement`** を持つ小さな実装に作り直す（`web/tests/fakedom.mjs`、全部の描画試験が
読む）。AC2 のフォーカスは `boot()` の後の `document.activeElement` で確かめる。

| AC | 確かめ方 |
|---|---|
| AC1 | `tests/static_files.rs`: `/`・`/admin` の HTML に `data-tab="photos"` が含まれ `blog-admin` のリンクが無い。`tests/no_python.rs` 風に `web/static` を grep する試験で `server.py`・`handlers.py`・「表示画面」が無い |
| AC2 | `tests/static_files.rs`: `GEOFRAME_BLOG` 未設定で `/blog-admin` の本文が `/admin` と同じ、`=1` で `blog-admin.html`。`/ai-search` は `/admin` と同じ（PR ②）。`web/tests/photos_render.mjs`: `location.pathname='/ai-search'` で `boot()` が `photos` タブを選び検索欄にフォーカス、`/admin` で `status` |
| AC3 | `web/tests/admin_ai_ops_render.mjs` の検証内容（空・欠落・期限・未知コード・停止ブロック）を変えずに、`admin.js` の新しい構成と `fakedom.mjs` で通す（`aiOpsCells` はグローバルのまま、`boot` は走らない） |
| AC4 | `tests/admin_api.rs`: 合成 DB で `db.filters` の各値が `GET /api/admin/photos?filter=<f>` の `total` と一致。`photos_render.mjs`: チップを `click()` すると `ps.filter`・`ps.page=1` と `fetch` の URL が変わる |
| AC5 | `photos_render.mjs`: `fetch` の代用品で 503（AI 無効 / 索引なし）・429・400 を返し、`#search-note` の文言が表どおりで `ps.view==='list'` のまま。成功で `ps.view==='search'`、閉じると直前の `filter`・`page` で一覧を読む。空の検索語で「AI おすすめ」と同じ URL |
| AC6 | `photos_render.mjs`: `normalizePhoto`・`cardFor` に合成の 7 パターン（位置あり / 候補あり / ヒントあり / 手がかりなし / 推定 / 検索結果の位置あり / 検索結果の候補あり）を与え、1 行目とタグの文字列、回転ボタンの有無を確認。`★` を `click()` すると `POST /api/admin/setflag` が呼ばれ、応答で同じ `id` の 2 枚のカードとビューアが更新される。`tests/update_api.rs`・`rotate_api.rs` は検証内容を変えない（旅行ブログの契約を見る試験には `GEOFRAME_BLOG=1` を足す）。`tests/ai_search_api.rs`（既存があれば）に `source`・`hint`・`suggest` の有無 |
| AC7 | 手動（検証環境）。`stepModal` の順序は `photos_render.mjs` で `ps.entries` の並びを確認 |
| AC8 | `admin_config.rs` の単体試験: 応答に全項目があり目印が値として含まれ、鍵の中身が無い。`Debug`・`log_summary` には目印も鍵の中身もモデル名・region も無い。`sources` の `arg/env/default` と `EnvItem::ALL` の一致。`EnvItem::ALL` の名前と `tests/env_reference.rs` の名前の一致。結合試験で本文全体に目印が無い |
| AC9 | `tests/admin_api.rs`: `GET /api/admin/config` が 200 で `items`・`ai` を持ち `Cache-Control: no-store`。HEAD・`POST`・`/api/admin/configX`・末尾 `/` は 403。`INGEST=0` でも 200 で `today_utc` が `null` か数。`photos_render.mjs` 相当の `config_render.mjs`: 設定タブに読み取り専用の案内の文言と、`items` の全項目が描かれる |
| AC10 | `tests/blog_api.rs`・`blog_write_api.rs`: 既存の試験を `spawn_with_env(GEOFRAME_BLOG=1)` に変え、未設定で GET 4 本と POST 2 本が 404 の試験を足す。無効時に CSRF の違反（`Content-Type` 違い）でも 404、`INGEST=0` でも 404（優先順位）。`tests/env_reference.rs` が通る |
| AC11 | `cargo test`・`cargo clippy --all-targets -- -D warnings`・`cargo fmt --check`。`photo_feed` の契約試験は触らない |

## リスクと対策

| リスク | 対策 |
|---|---|
| `GEOFRAME_BLOG` の既定が `0` なので、**新しいバイナリへ更新した次回の起動から**、環境ファイルを触らなくてもブログが消える | 意図どおり（ユーザーの決定）。設定リファレンスと `deploy/README.md` に「戻すには `=1`」を書く。本番の更新はユーザーの立ち会いで行う（猶予期間は 2026-10-30 に終了） |
| 検索 API の写真に項目を足すと `time_anchors` の読み（位置つき写真の全件）が検索ごとに増える | 返す結果の確定後に 1 回だけ、位置の無い写真が含まれるときだけ読む。一覧の `noloc` ページと同じ負荷（6,500 行で数 ms）。合成 DB の試験で、位置ありだけの結果では錨の問い合わせが無いこと（`time_anchors` の呼び出し回数を数える）と、同じ写真の `hint`・`suggest` が一覧と検索で一致することを確かめる |
| 一覧の全フィルタで `hint` を付けると `all` のページでも `time_anchors` を読む | ページ内に位置なしの行があるときだけ読む |
| CSS 統合で `.hidden`・`#toast` が衝突 | 場面・ビューアだけ `.ps-`。`#toast` は `admin.css` の 1 つにする |
| `aiOpsCells` の試験が `admin.js` の分割で壊れる | `aiOpsCells` と依存するヘルパ（`el`・`fmtUtc`）は `admin.js` に残す。起動は `DOMContentLoaded` の `boot()` だけにし、評価時に `location` を触らない |
| `db.filters` で `GET /api/admin/status` が重くなる（7 つの `COUNT`） | 6,500 行の SQLite で `COUNT` は数 ms。`suggest` のサブクエリは `photo_loc_suggestions` の索引を使う。計測して 50 ms を超えるなら 60 秒のキャッシュ（`disk` と同じ） |
| `today_utc` の読みが失敗する（表が無い旧 DB、ロック） | `null` にして 200。設定の表示は DB に依存しない |
| `sources` の記録漏れ | `EnvItem::ALL` と `tests/env_reference.rs` の名前の集合を単体試験で突き合わせる |
| PR ② と ③ は PR ① に依存する | ① を戻すには ②③ を先に戻す。各 PR の本文に依存を書く。① 単独は ②③ のマージ前なら戻せる |
| 本番反映はマージと別 | PR のマージと本番の更新を分ける（PRD の制約）。更新は `deploy/README.md` §5 の手順で |

## 実装の順番

### PR ①: 骨組み・旅行ブログのフラグ・掃除（R1 の 2 タブ分、R3〜R6、R18〜R20）

1. `config.rs`: `blog`。試験。`docs/reference/configuration.md`・`deploy/README.md`。
2. `server.rs`: ブログの 404（GET・POST・ページ）。`tests/blog_api.rs`・`blog_write_api.rs`・`static_files.rs` の更新と追加。
3. `admin.rs`: `db.filters`（`Filter::ALL`）、全フィルタの `hint`。`tests/admin_api.rs`。
4. `admin.html`・`admin.css`・`admin.js`: ナビを「状況 / 写真」にし、写真タブにチップ（件数つき）とページング、
   「要対応」のセル。古い記述の掃除。ブログのリンクを外す。**`/ai-search` ページとそこへのリンクは残す**（写真タブの
   ツールバー）。起動を `boot()` にまとめ、`aiOpsCells` の試験が通ることを確認。`web/tests/fakedom.mjs` と
   `photos_render.mjs`（チップ・タブ選択・R10 の 1 行目）、`tests/admin_ui_photos.rs`、`no_python.rs` の許可リスト。

### PR ②: 写真タブへの AI 検索の統合（R2 の `/ai-search`、R7〜R12）

1. `search_view.rs`・`admin.rs`: 写真に `source`・`rotation`・`loc_source`・`infer_bucket`・`hint`・`suggest`（補完関数の共有）。
2. `photos.js` 新設: `normalizePhoto`・`cardFor`・一覧・検索・場面・ビューア・位置指定モーダル（`admin.js` から移す）。
   `admin.js` は共通ヘルパ・タブ・状況・起動に縮める。
3. `admin.html`: 検索バー・`#groups`・`#viewer`。`ai-search.html`・`ai-search.css`・`ai-search.js` を削除し、
   `/ai-search` を `admin.html` に向ける（`webpath.rs`・`embedded.rs`・`static_files.rs` の試験）。
4. `admin.css`: `.ps-` の部品を統合。
5. `photos_render.mjs` に検索・カード・フラグ更新の試験を足す。

### PR ③: 設定タブ（R1 の設定タブ、R13〜R17）

1. `config.rs`: `Source`・`Exposure`・`EnvItem`・`sources`・`ai_config_modified`。試験。
2. `server.rs`: `started_at`。`admin_config.rs` 新設（応答の組み立て、`EnvItem` のワイルドカード無しの `match`、`today_utc`、ファイルの有無）。
   `GET /api/admin/config` の経路。
3. 単体試験（目印の不在、`sources`）と `tests/admin_api.rs`。`innerHTML` の grep の試験。
4. `admin.html`・`admin.js`: 「設定」タブ、項目の表（説明文・単位・並び）、読み取り専用の案内、「今日（UTC）」。`config_render.mjs`。
5. `docs/architecture/photo-pipeline.md` の「API・UI」に `GET /api/admin/config` を 1 行。`docs/rust-migration/ai-config.md` と
   `config.rs:213` のコメントに「`/api/admin/config` だけはモデル名・region を出す」の例外を書き足す。

② と ③ は ① の後なら順不同。各 PR の本文に依存関係（① が前提）を書く。

## 実装との差（PR ①・② の完了時に追記。2026-10-31）

実装のレビューで受け入れた、本文の記述と違う点。本文は設計時のまま残し、ここが実装の正。

- **カード**: `cardFor(photo, index, groupUi)` の第 3 引数で、場面の折りたたみ（構図ごと・場面全体）を扱う。正規化後は
  `visual.size` と AI の点数の項目も持つ。位置の保存後の 1 行目は**地名でなく座標 4 桁＋`手動` のタグ**（`setloc` の応答に
  地名が無く、サーバーが地名を空に戻して次の取り込みで付け直すため）。保存後は同じ写真の全カードと開いているビューアを
  更新し、一覧表示中は一覧も読み直す。
- **写真タブの状態**: `ps` は `filter`・`page`・`view`・`query`・`includeHidden` に加えて `mode`（`semantic`/`recommended`）、
  `cards`、`pending`（応答待ちの検索語と要求番号）を持つ。`entries` は `{photo, group, groupIndex, photoIndex}` の配列。
  検索・一覧・件数の要求には番号を付け、古い応答は捨てる。「除外も検索」の切替は、応答待ちがあればその語で、無ければ
  表示中の語で検索し直す（一覧で応答待ちも無ければ何もしない）。検索語が 300 文字（コードポイント）を超えるときは
  送らず、先行の検索も無効にする。
- **状態遷移**: 検索結果ではチップ・ページング・`#cards` を隠す。状況タブへ行って戻ると検索結果を保持し、「要対応」から
  来たときは一覧に戻す。写真タブを離れるとビューアと位置モーダルを閉じる。
- **読み込みと起動**: `boot()` はナビのイベント登録 → `initPhotos()` → 初期タブの表示 → 状況の 30 秒更新の登録 →
  `/ai-search` なら検索欄にフォーカス。`initialTab()` はタブ名を返すだけ。索引の枚数は写真タブを初めて開いたときだけ読む。
- **場面・ビューア**: DOM の id は `viewer-*`、クラスは `.ps-*`。ビューアは一覧のカードからも開け（AI の欄は隠す）、
  前後移動は周回する。
- **テスト**: 検索 API の位置の項目は `crates/geoframe/src/ai/query/tests.rs` の
  `search_photos_carry_the_admin_location_fields_and_match_the_admin_list`（`tests/` から AI の合成 DB を使えないため）。
  HTML 文字列を挿入する API の不在は `tests/static_files.rs` の `the_scripts_never_build_html_from_strings`（PR ② で
  追加済み。PR ③ ではやらない）。描画試験は `node web/tests/photos_render.mjs web/static/js/admin.js web/static/js/photos.js`
  （2 つの JS を同じ context に読む）。位置の保存は (q)(q2)、ビューアの順序・周回・`Escape` は (p)、折りたたみは (i)。
  検証環境での目視（AC3・AC7）は未実施。
- **見送った指摘**: DB に非有限の実数が入ったときの `infer_bucket` の差（アプリは書かない）、補完が別トランザクション
  （表示用のヒント）、`the_scripts_never_build_html_from_strings` は `web/static/js` の 1 階層だけ（サブディレクトリは無い）。
