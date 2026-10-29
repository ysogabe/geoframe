# 現行実装の棚卸し（Rust移行用）

- 状態: 2026-10-24 時点の `main` 相当（ブランチ `t3/issue-54-implementation-plan`）のPython実装をコードから調べた記録
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54) / 計画: [plan.md](plan.md) / タスク: [tasks.md](tasks.md)（T0.2、AC2）
- 目的: Rustへ移すときに機能・暗黙の挙動・一次データの扱いが漏れないよう、移すべき対象を一覧にする。

この文書は**リポジトリ内のコードだけ**から作った。本番DB・本番機・実写真は見ていない。値の例は合成したものか、記号で書く（実際の座標・ファイル名・IDは載せない）。

## 0. 読み方

- 実装箇所は `パス:関数名` のバッククォート表記（リンクにしない）。`h:` は `app/geoframe/handlers.py`、`a:` は `app/geoframe/admin.py` の略として表に使う。
- 移行フェーズは [plan.md](plan.md) の割当に従う。P1=読み取りAPI・静的配信、P2=取り込み、P3=管理機能、P4=AI・ジョブ、P5=切替・運用。1つの行が複数フェーズにまたがるときは「P1（配信）/P3（機能）」のように書く。
- 外部通信の略: **Gemini**=Vertex AI（Agent Platform）、**Drive**=Google Drive API、**GSI**=国土地理院、**Nom**=Nominatim、**タイル**=OSM/地理院の地図タイル、**Decision**=local-model-labのd3-lite（`GEOFRAME_DECISION_URL`）。

---

## 1. HTTPエンドポイント

サーバーは標準ライブラリの `ThreadingHTTPServer` + `SimpleHTTPRequestHandler`（`app/server.py`、`app/geoframe/handlers.py:Handler`）。**認証は無い**（家庭内LAN前提）。アクセスログは無効（`log_message` が何もしない）。

### 1.1 一覧

ルーティングの一致のさせ方が行ごとに違う。「一致」列は、Rustで同じ挙動にするか整理するかを P1 で決めるための記録。

| # | メソッド・パス | クエリ／本文 | 応答の要点 | 状態変更 | 外部通信 | 実装箇所 | 呼び出し元 | 一致 | フェーズ |
|---|---|---|---|---|---|---|---|---|---|
| E1 | GET `/api/v1/photos` | なし | 写真オブジェクトのJSON配列（契約。`id file time lat lon pref city title fav fetched_at quality_* trip` ほか内部値）。位置あり・display有り・hidden除外・品質/AIの抑制除外。撮影日時昇順、時刻不明は最後。`trip` は48時間超の空きで0から採番 | なし | なし | `photos.py:api_photos` / `photos.py:assign_trips` / `handlers.py:Handler.do_GET` | miharashi（表示） | クエリ除去後の完全一致 | P1 |
| E2 | GET `/api/admin/status` | なし | `sync{ready,pending,total}`（**Rustは保留があるときだけ`mass_delete_hold`を足す**。T3.7⑤）、`db{件数内訳、by_loc_source、infer_buckets、quality、ai、loc}`、`disk`（60秒キャッシュ）、`server_time` | なし | なし | `admin.py:api_admin_status`、`loc_suggest.py:loc_status`、`admin.py:disk_info` | `admin.js`（STATUSタブ）、`deploy/loc_decision_morning.py`（`db.loc.clue_runnable`、無ければ `clue_pending` を読む） | **生パスの前方一致**（`/api/admin/status?x` も `/api/admin/statusX` も通る） | P1（AI・位置候補の列は読み取りのみ。テーブルが空でも動くこと） |
| E3 | GET `/api/admin/photos` | `page`(1)、`per`(50、上限200)、`filter`(all/noloc/fav/hidden/inferred/manual/suggest。未知はall) | `items[]`（id name thumb taken_at lat lon loc_source infer_dt_s infer_bucket place pref city fav hidden rotation source、位置なしでは `hint`・`suggest`）、`page pages total filter`。並びは時刻（NULL・空は最後）→id | なし | なし | `admin.py:api_admin_photos` / `admin.py:_thumb_for` / `admin.py:_nearest_hint` / `admin.py:_suggestions` | `admin.js`（一覧） | 生パスの前方一致。`int()` 失敗は400 `bad_params` | P1 |
| E4 | GET `/api/admin/geocode` | `q`（2文字未満は空配列） | `{ok, items[{title,lat,lon,source:'gsi'|'osm'}]}`。GSIが0件のときだけNom。失敗は空配列で握りつぶす | なし（`.cache.json` のメモリキャッシュを更新） | GSI、Nom（1秒1件の全体ロック） | `geo.py:geocode_search` / `geo.py:_gsi_geocode` / `geo.py:nominatim_search` | `admin.js`（地名検索） | 生パスの前方一致 | P3 |
| E5 | GET `/api/ai/status` | なし | embedding件数、グループ、パイプライン状態（`photo_ai_status` 設定）、モデル切替直後の他モデル行数 | なし | なし | `ai_search.py:api_ai_status` | `ai-search.js` | 完全一致 | P4（E5/E6は[T4.4b実装](ai-search.md)） |
| E6 | GET `/api/ai/search` | `q`（300字以内、空なら「推薦順」）、`groups`(12、1〜30)、`include_hidden`(1/true) | `groups[{group_id kind summary 類似度 photos[]}]`、`mode` は `semantic`/`recommended`/`empty`。分あたり30件を超えると429、`GEOFRAME_AI_ENABLED=0` は503、上流障害は503（定型文）。**不正な `groups`（数値でない等）・`q` の不正は400**（現行は例外文字列 `str(e)` をそのまま返す。Rustでは定型文にする） | なし（クエリのembeddingをメモリLRU 64件にキャッシュ） | Gemini（または local embedding）で検索語を1回embedding | `ai_search.py:api_ai_search` / `ai_search.py:_embed_query` / `ai_search.py:_search_rate_limit_allowed` | `ai-search.js` | 完全一致 | P4（E5/E6は[T4.4b実装](ai-search.md)） |
| E7 | GET `/api/blog/trips` | なし | 旅程JSON（`app/travel_blog/plans/*.json`）の一覧と、下書き・書き出しの有無 | なし | なし | `travel_blog.py:api_blog_trips` / `travel_blog.py:list_plans` | `blog-admin.js` | 完全一致 | P3 |
| E8 | GET `/api/blog/trip` | `trip_id`（`^[a-z0-9][a-z0-9-]{0,79}$`） | 旅程JSONそのもの。不正は400、無ければ404 | なし | なし | `travel_blog.py:api_blog_trip` / `travel_blog.py:load_plan` | `blog-admin.js` | 完全一致 | P3 |
| E9 | GET `/api/blog/candidates` | `trip_id` | 日ごとの写真候補（`hidden` でない写真を日付範囲で選び、粗い地名だけ返す。正確なGPSは返さない） | なし | なし | `travel_blog.py:api_blog_candidates` / `travel_blog.py:_segment_candidates` | `blog-admin.js` | 完全一致 | P3 |
| E10 | GET `/api/blog/draft` | `trip_id` | 保存済み下書き（`source:saved`）か、候補から生成した初期案（`source:generated`） | なし | なし | `travel_blog.py:api_blog_draft` / `travel_blog.py:generate_manifest` | `blog-admin.js` | 完全一致 | P3 |
| E11 | GET/HEAD `/tiles/{osm|gsi}/{z}/{x}/{y}.png` | 正規表現 `^/tiles/(osm|gsi)/(\d{1,2})/(\d+)/(\d+)\.png$` | 常に200（取得失敗・流量超過も透明PNG）。`X-Tile-Cache: hit/miss/error/throttled`。`error`/`throttled` は `no-store`、他は `public, max-age=86400`。座標範囲外・ズーム超過・パス不一致は404 | `DATA_DIR/tile_cache/` にファイル作成 | タイル（OSM maxzoom19・同時2、GSI maxzoom18・同時4。上流取得は全体で毎分240枚まで） | `handlers.py:Handler._serve_tile` / `tiles.py:fetch_tile` | `admin.js`（MapLibreのraster source） | 前方一致 `/tiles/` | P3 |
| E12 | POST `/api/admin/setloc` | JSON `{id:int>0, lat:-90〜90, lon:-180〜180}`（数値のみ、boolと文字列は拒否） | `{ok,id,lat,lon,loc_source:'manual'}`。対象なしは404 | `photos` の lat/lon/loc_source='manual'、`inferred_from`/`infer_dt_s`/地名列をNULLへ | なし | `admin.py:admin_setloc` | `admin.js`（SAVE） | `_norm_path` 後の完全一致 | P3 |
| E13 | POST `/api/admin/setflag` | JSON `{id, favorite?:bool, hidden?:bool}`（どちらも無ければ400） | `{ok,id,favorite,hidden}` | `photos.favorite`/`hidden` | なし | `admin.py:admin_setflag` | `admin.js`、`ai-search.js` | 同上 | P3 |
| E14 | POST `/api/admin/rotate` | JSON `{id, delta:90/180/270/-90}` | `{ok,id,rotation,thumb}`。Drive由来で derivative がある写真だけ（それ以外は400） | 回転した derivative を別名で作成→`photos.display`/`rotation` 更新→旧ファイル削除 | なし | `admin.py:admin_rotate` / `util.py:derivative_name` | `admin.js`（回転ボタン。常に+90） | 同上 | P3 |
| E15 | POST `/api/blog/save` | JSON `{trip_id, manifest}`（検証あり。章40・章内画像100・合計200枚、各文字列に長さ上限） | `{ok,trip_id,saved}`。`published` は `draft` に落として保存 | `DATA_DIR/travel_blog/drafts/<trip_id>.json`（tmp→`os.replace`） | なし | `travel_blog.py:save_blog_draft` / `travel_blog.py:_validate_manifest` | `blog-admin.js` | 同上 | P3 |
| E16 | POST `/api/blog/export` | JSON `{trip_id, manifest, dry_run?}` | `{ok,images,path,url}`（**Rustは使えない写真を飛ばしたときだけ`skipped`を足す**。T3.7④）。実行中の別エクスポートがあれば409 | `DATA_DIR/blog_exports/<trip_id>/`（staging→置換、`.previous` を経由）。画像は1800pxで再エンコードしEXIF/GPSを落とす | なし | `travel_blog.py:export_blog` | `blog-admin.js` | 同上 | P3 |
| E17 | GET/HEAD `/`、`/admin`（`/admin/` も） | なし | `static/admin.html` を返す（内部書き換え）。`no-cache`、`X-Frame-Options: DENY`、CSP `frame-ancestors 'none'` | なし | なし | `handlers.py:Handler._admin_page_rewrite` | ブラウザ（管理画面） | `_norm_path` 後の完全一致（`/admin/../.env` は `/.env` になり403） | P1 |
| E18 | GET/HEAD `/blog-admin` | なし | `static/blog-admin.html`（E17と同じヘッダ） | なし | なし | `handlers.py:Handler._blog_page_rewrite` | `admin.html` のTRAVEL BLOGタブ | 同上 | P1（配信）/P3（機能） |
| E19 | GET/HEAD `/ai-search` | なし | `static/ai-search.html`（同上） | なし | なし | `handlers.py:Handler._ai_search_page_rewrite` | `admin.html` のAI写真検索タブ | 同上 | P1（配信）/P4（機能） |
| E20 | GET/HEAD `/static/*` | - | `app/static/` の `admin.html/css`、`ai-search.*`、`blog-admin.*`、`js/*.js`。`no-cache` | なし | なし | `handlers.py:Handler.do_GET` / `_static_allowed` | 各HTML | prefix許可 | P1（バイナリへ埋め込む） |
| E21 | GET/HEAD `/vendor/*` | - | `maplibre-gl-4.7.1.min.js/css`、`fonts.css`、`fonts/`。`no-cache` | なし | なし | 同上 | `admin.html`、`ai-search.html` | prefix許可 | P1（埋め込み） |
| E22 | GET/HEAD `/favicon.ico` | - | 現リポジトリに実体なし（`ROOT/favicon.ico` を探して無ければ404） | なし | なし | `STATIC_ALLOW_EXACT` | ブラウザ | 完全一致 | P1（実体が無いことを確認） |
| E23 | GET/HEAD `/derivatives/*` | - | 表示用JPEG（`DATA_DIR/derivatives/`）。Cache-Controlを付けない（ブラウザのヒューリスティックキャッシュ）。`Last-Modified`/If-Modified-Since→304 | なし | なし | `handlers.py:Handler.translate_path` | miharashi、管理画面、`ai-search.js` | prefix許可、基準ディレクトリ=DATA_DIR | P1 |
| E24 | GET/HEAD `/photos/*` | - | `DATA_DIR/photos/` のローカル原本。**ローカルのJPG/PNGは `display` が原本パスそのもの**なので、E1の `file` が `photos/...` になりうる | なし | なし | 同上 | miharashi（`file` 経由） | prefix許可 | P1 |
| E25 | GET/HEAD `/blog_exports/*` | - | 旅行ブログの書き出し成果物（`index.html` ほか）。作業領域 `.<trip>-<temp>/`・`.<trip>.previous` はドット始まりのため非公開 | なし | なし | 同上 | `blog-admin.js` が `url` を返し利用者が開く | prefix許可 | P1（配信）/P3 |

補足:

- **現行にヘルスエンドポイントは無い**（`/health` は Decision 側のもの）。`plan.md` のP1で追加するヘルス確認は新規。当面は `GET /api/admin/status` が代用されている（`deploy/loc_decision_morning.py`）。
- HEAD は `/tiles/` と静的配信だけ対応。**`HEAD /api/...` は静的ホワイトリストに落ちて403**になる。
- `OPTIONS` は未実装で `501`。これは**CSRF対策の一部**（クロスオリジンの `application/json` POSTが先に出すプリフライトを失敗させる）。Rustでも**CORSヘッダを一切返さず、`OPTIONS` を許可しない**こと。
- 未知の静的パス・未知のPOSTパスは **404ではなく403**。`/api/blog/` 配下の未知パスだけ404。
- 応答のJSONは `ensure_ascii=False`、区切りに空白あり（`", "`・`": "`）。値の比較は構文解析後に行う（バイト一致は求めない）。Pythonの浮動小数の文字表現（`1e-05` など）はRust（`1e-5`）と異なりうるので、比較は数値として行う。

### 1.2 横断的な仕組み

| 項目 | 現行の挙動 | 実装箇所 | フェーズ |
|---|---|---|---|
| Host検査（DNSリバインディング対策） | GET/HEAD/POSTのルーティングより前に検査。Hostヘッダ無し・空は許可。ポートは `GEOFRAME_PORT` か省略のみ。IPリテラル（IPv6の `[::1]:8080`、ゾーンID `%en0` 付きも）は常に許可。ホスト名は `localhost` と `GEOFRAME_ALLOWED_HOSTS` のみ。拒否は403＋ログ1行（Host値を出す）。起動時に許可内容を1行ログ | `handlers.py:Handler._host_ok` / `_split_host_port` / `_deny_host`、`config.py:_parse_allowed_hosts` / `allowed_hosts_note` | P1 |
| パス正規化 | クエリ・フラグメントを除去 → URLデコード → `\` を `/` へ → NUL文字は拒否（空文字） → `posixpath.normpath`（`..` を解決、末尾スラッシュ除去）。判定は必ずデコード後 | `handlers.py:Handler._norm_path` | P1 |
| 静的ホワイトリスト | 許可は `/favicon.ico` と prefix `/static/ /vendor/ /derivatives/ /photos/ /blog_exports/` のみ。それ以外は403。**パスのどの要素もドット始まりなら403**（実在しないファイルも同じ403で、存在有無を漏らさない）。ディレクトリ自体は403（一覧を出さない） | `handlers.py:Handler._static_allowed` | P1 |
| 配信基準ディレクトリ | `/derivatives/ /photos/ /blog_exports/` だけ `DATA_DIR` 基準、`/static/ /vendor/` は `app/` 基準（Rustでは埋め込み）。切替は正規化済みパスで判定し、実体解決は親クラスの `..` 除去に任せている | `handlers.py:Handler.translate_path` | P1 |
| Cache-Control | HTML（`/`・`*.html`）・`/static/`・`/vendor/` は `no-cache`、`/api/` は `no-store`、`/derivatives/ /photos/ /blog_exports/` は付けない、`/tiles/` は個別。HTMLには `X-Frame-Options: DENY` と `Content-Security-Policy: frame-ancestors 'none'` | `handlers.py:Handler.end_headers` / `_send_json` | P1 |
| CSRF対策（POST） | (1) `Content-Type: application/json` 必須（違えば403）、(2) `Origin` があれば `Host` と同一（違えば403。Originなしの curl 等は通す）、(3) `Content-Length` 必須（無ければ400）、(4) 上限 64KiB（超過は本文を読まず413＋接続を閉じる）、(5) JSON解析失敗は400、(6) 本文はオブジェクトのみ。順序は Host → ルート存在(403) → (1)(2) → (3)(4)(5)(6) | `handlers.py:Handler._csrf_ok` / `do_POST` | P3 |
| エラー応答 | `BadRequest`→400、`NotFound`→404、ブログの `BadRequest`→400、`NotFound`→404、エクスポート中→409、その他の例外→500 `内部エラーが発生しました`（**例外文字列は返さない**。詳細はログのみ） | `handlers.py:Handler.do_POST` | P3 |
| 読み取りAPIにCSRF対策なし | GETは状態を変えない前提。ただし E4 は外部（GSI/Nom）を叩き、E6 は外部（Gemini）を叩く。そのため E6 には分あたり30件、E11 には上流取得の毎分240枚の流量上限がある | `ai_search.py:_search_rate_limit_allowed`、`tiles.py:_tile_fetch_allowed` | P3 / P4 |

### 1.3 画面（HTML/JS）とAPIの対応

| 画面 | ファイル | 使うAPI・資源 |
|---|---|---|
| 管理画面（STATUS・一覧・地図でのGPS修正） | `app/static/admin.html`、`js/admin.js` | E2、E3、E4、E12、E13、E14、E11（MapLibre）、E23（サムネ）、`/vendor/` |
| AI写真検索 | `ai-search.html`、`js/ai-search.js` | E5、E6、E13、E23 |
| 旅行ブログ | `blog-admin.html`、`js/blog-admin.js` | E7〜E10、E15、E16、E23 |
| 表示（kiosk） | このリポジトリには無い（miharashi） | E1、E23、E24 |

---

## 2. DBテーブル（`catalog.db`）

SQLite、WAL。接続は**プロセスで1本**（`check_same_thread=False`）で、全アクセスをグローバルロック `_db_lock` で直列化（`store.py:db` / `dbx` / `dbq` / `dbm`）。起動時に `store.py:db` が `SCHEMA` を実行し（`CREATE TABLE IF NOT EXISTS`）、続けて下記のALTER・バックフィルを行う。

分類は3つ。

- **一次**: 利用者が与えた情報で、再構築できない。
- **派生（無償）**: 原本・他の列から計算で作り直せる。
- **派生（有償）**: 作り直せるが、クラウド課金・GPU時間・時間のかかる外部通信が要る（Gemini判定、embedding、位置候補の回答）。サーバーのdocstring（`app/server.py:13,27`）と更新前の注意（`deploy/deploy-pull.sh:11`）は embedding を「再構築不可」と扱っている。

### 2.1 テーブル一覧

| テーブル | 主な列 | 分類 | 書く関数 | 読む関数 | 孤児行の掃除 |
|---|---|---|---|---|---|
| `photos` | 下の2.2 | 列ごとに異なる | `drive.py:index_drive` / `index_local` / `fetch_derivatives` / `local_derivatives` / `organize_photos`、`geo.py:infer_locations` / `enrich_rows`、`admin.py:admin_setloc` / `admin_setflag` / `admin_rotate` | 全モジュール（`photos.py:api_photos`、`admin.py`、`quality.py`、`ai_pipeline.py`、`ai_search.py`、`loc_suggest.py`、`travel_blog.py`） | 行の削除は `index_drive`（Drive側で消えた行）と `index_local`（ローカルから消えた行）、`organize_photos`（`--organize`） |
| `settings` | `key` PK, `value` | 派生（無償） | `store.py:setting_set`（`quality.py`、`ai_pipeline.py`、`loc_suggest.py` から） | `store.py:setting_get`、`loc_suggest.py:loc_status`、`ai_search.py:api_ai_status` | - |
| `photo_quality` | `photo_id` PK、`display`、`source_md5`、`source_mtime`、`content_sha256`、`group_id`、`group_kind`(`exact`/`similar`)、`rank_in_group`、`recommended`、`score`、`reasons_json`、`issues_json`、`algorithm`、`analyzed_at` | 派生（無償）。再生成に画像再読込 | `store.py:replace_photo_quality`（全削除→全挿入を `BEGIN IMMEDIATE` で1トランザクション） | `photos.py:api_photos`（JOIN）、`admin.py:api_admin_status`、`ai_search.py:_valid_rows` | 毎回全差し替え |
| `photo_quality_features` | `photo_id` PK、`display`、`source_md5`、`source_mtime`、`content_sha256`、`phash`、`sharpness`、`luminance`、`exposure_score`、`black_clip`、`white_clip`、`contrast`、`error`、`analyzed_at` | 派生（無償） | `quality.py:_analyze_pending`（upsert）、`quality.py:_remove_orphans`（削除） | `quality.py`、`ai_pipeline.py:_load_photos`、`admin.py`、`ai_search.py:api_ai_status` | `quality.py:_remove_orphans` |
| `photo_ai_features` | `photo_id` PK、`display`、`source_md5`、`source_mtime`、`content_sha256`、`embedding_model`、`embedding_dim`、`embedding`(float32 LE BLOB)、`subject`/`pet_state`/`eyes`/`face_crop`/`horizon_*`、`subject_sharpness`、`aesthetic_score`、`keep_score`、`recommended`、`reasons_json`、`concerns_json`、`judge_model`、`analyzed_at`、`error` | 派生（有償） | `ai_pipeline.py:_embed_pending`（upsert）、`store.py:complete_photo_ai_group`（判定列のUPDATE）、`store.py:replace_photo_ai_catalog`（PoC取込用の全差し替え）、`ai_pipeline.py:_remove_noncanonical_embeddings`（削除） | `ai_pipeline.py:_load_photos`、`ai_search.py:_valid_rows` / `api_ai_status`、`admin.py:api_admin_status` | `ai_pipeline.py:_remove_noncanonical_embeddings`（更新済み・削除済み・SHA完全重複の非代表） |
| `photo_embeddings` | `(content_sha256, embedding_model)` PK、`embedding_dim`、`embedding`、`analyzed_at` | 派生（有償）。モデル切替で戻したときの再課金を避けるキャッシュ | `ai_pipeline.py:_embed_pending`（`INSERT OR REPLACE`）、`store.py:db`（起動時のバックフィル `INSERT OR IGNORE`、**毎起動**）、`prefill_embeddings.py:main` | `ai_pipeline.py:_embed_pending` | `ai_pipeline.py:_remove_noncanonical_embeddings`（どの `photo_quality_features` にも無いSHAを削除） |
| `photo_ai_groups` | `group_id` PK、`kind`(`scene`/`pet`)、`summary`、`min/max/avg_similarity`、`algorithm`、`analyzed_at`、`status`(既定`judged`)、`same_scene`、`judge_version`、`judge_model`、`shortlist_json`、`error`、`attempts`、`judge_order`、`presented_json` | 派生（有償：Gemini比較の結果を含む） | `store.py:replace_photo_ai_groups`（全削除→全挿入。judged かつ `judge_version` 一致のものは旧行の判定を引き継いで挿入し直す）、`store.py:complete_photo_ai_group`、`ai_pipeline.py:_judge_pending`（失敗時 `status='error'`・`attempts+1`） | `photos.py:api_photos`（JOIN）、`ai_search.py`、`admin.py:api_admin_status` | 毎サイクル全差し替え |
| `photo_ai_group_members` | `(group_id, photo_id)` PK、`rank_in_group`、`recommended`、`shortlisted`、`local_score` | 派生（有償：`recommended` は判定結果） | 上と同じ（`replace_photo_ai_groups` / `complete_photo_ai_group`） | `photos.py:api_photos`（JOIN）、`ai_search.py:_valid_rows` | 毎サイクル全差し替え |
| `photo_loc_clues` | `photo_id` PK、`display`、`source_md5`、`source_mtime`、`clue`、`kind`、`model`、`analyzed_at`、`error` | 派生（無償だが、Decision（GPU）が必要） | `loc_suggest.py:_classify`（upsert） | `loc_suggest.py:_load_targets` | **掃除する処理なし**（写真が消えても行が残る。読む側が `display`/md5/mtime の一致で無効化する） |
| `photo_loc_runs` | `run_id` PK、`day`、`model`、`prompt_version`、`member_ids_json`、`sent_ids_json`、`status`(`pending`/`done`/`error`)、`attempts`、`answer_json`、`error`、`analyzed_at` | 派生（有償：Geminiの回答） | `loc_suggest.py:loc_suggest_cycle`（`INSERT OR REPLACE`） | `loc_suggest.py:loc_suggest_cycle` / `loc_status` | **掃除する処理なし** |
| `photo_loc_suggestions` | `photo_id` PK、`run_id`、`scope`(`photo`/`day`)、`place_name`、`day_place_name`、`geocode_query`、`precision`、`confidence`、`evidence_json`、`lat`、`lon`、`geocode_title`、`model`、`analyzed_at` | 派生（有償）。**`photos.lat/lon` には書かない**。人が管理画面で確認してSAVEしたとき（E12）だけ `manual` として `photos` に入る | `loc_suggest.py:_apply_run`（`INSERT OR REPLACE`） | `admin.py:_suggestions`、`admin.py:FILTERS['suggest']`、`loc_suggest.py:loc_suggest_cycle` / `loc_status` | **Python側に掃除する処理なし**（位置が確定した写真の行も残る。読む側は `lat IS NULL` で絞る）。ただしRustの`location_suggest`は、自分が書いた投影の行だけを周期で`DELETE`する（ファイルの差し替え・SAVE・hidden・日の構成の変化・履歴の破損。Pythonが書いた行には触れない。[ai-location-suggest.md](ai-location-suggest.md)） |

### 2.2 `photos` の列

| 列 | 内容 | 分類 | 備考（Rustで守ること） |
|---|---|---|---|
| `id` | 整数PK | **一次**（写真ID。契約。miharashiの表示履歴の鍵） | 行を消すと欠番になり、再追加されたら**別のid**になる。移行で再採番しない |
| `source` | `drive` / `local` | 派生（索引時に決まる） | |
| `drive_id` | Drive ファイルID（UNIQUE） | 派生。ただし**行の同一性を決める** | derivative のファイル名にも使う（`<drive_id>.jpg`、回転後は `<drive_id>.r<角度>.jpg`） |
| `path` | ローカル原本のDATA_DIRからの相対パス（UNIQUE、driveはNULL） | 派生。行の同一性を決める | |
| `display` | 表示用ファイルの相対パス。`derivatives/...` か（ローカルJPG/PNGでは）`photos/...` | 派生。ただし `rotate` で変わる | 値は**配信URLと実ファイルパスの両方**を兼ねる（`/` + display が配信URL、`DATA_DIR/display` が実体）。ディレクトリ名を変えない |
| `name` | 元のファイル名 | 派生 | ログに出さない（§6） |
| `md5` | Drive の `md5Checksum` | 派生 | 変更検知。品質・AI行の有効性判定にも使う |
| `mtime` | ローカルの更新時刻（REAL） | 派生 | Drive行ではNULL。判定は `IS` 比較（NULL同士が一致） |
| `taken_at` | `YYYY-MM-DD HH:MM`（ローカル時刻、タイムゾーン無し） | 派生（EXIF／Driveメタ。手動で直す機能は無い） | 未取得は**NULLではなく空文字**で入る |
| `lat`, `lon` | 緯度・経度 | `loc_source` により異なる: `exif`/`drive_meta`=派生（原本から）、`inferred`=派生（推定）、**`manual`=一次** | §2.3の「消える経路」を参照 |
| `loc_source` | `exif` / `drive_meta` / `inferred` / `manual` / NULL | `manual` の印が一次。他は派生 | `'inferred'` というリテラルを複数箇所が見ている（`geo.py:infer_locations` の注記）。段階は別列で表す |
| `inferred_from` | 推定の錨にした写真のid | 派生 | |
| `infer_dt_s` | 錨との時間差（秒） | 派生 | 窓ラベル（±15分／±2時間／±12時間）は表示時に計算（`geo.py:infer_bucket`） |
| `place`, `elev`, `muni_cd`, `pref`, `city` | 地名・標高・自治体 | 派生（GSIで再取得。外部通信が必要） | `place IS NULL` が「未enrich」の印。`setloc` が NULL に戻すと次サイクルで再enrich |
| `title` | 手動／AIが付けるタイトル | **一次扱い**（E1の `title` の最優先） | **現行コードに書き込み元が無い**（`photos.py`・`travel_blog.py` が読むだけ）。外部から直接書かれている可能性があり、扱いは未確認（§7） |
| `ai_caption` | AIのキャプション | **一次扱い** | 同上。ブログ候補の `alt`/`caption` に使う |
| `favorite` | 0/1 | **一次** | `ALTER` 追加列。既定0 |
| `hidden` | 0/1 | **一次**（「除外」） | 同上 |
| `rotation` | 時計回り 0/90/180/270 | **一次**（手動回転） | 同上。Driveのmd5変更時に0へ戻る（下記） |
| `indexed_at` | 索引した日時（naiveな `isoformat`） | 派生 | |
| `fetched_at` | derivative を作った日時（naiveな `isoformat`） | 派生。ただしE1の `fetched_at` として**新着ブーストに使われる** | 再構築すると全部が新着扱いになる |

### 2.3 一次データが消える・戻る経路（現行の挙動）

Rustでも意識して残すか、直すかを決める必要がある。

| 経路 | 現行の挙動 | 実装箇所 |
|---|---|---|
| Driveのmd5が変わった | `name md5 taken_at lat lon loc_source` を更新し、`place elev display fetched_at` をNULL、**`rotation=0`**。`loc_source` は `drive_meta`（GPS有）かNULLに**上書きされ、手動GPSは失われる**。`favorite`/`hidden` はSET句に無いので残る。**Rust（T3.7、2026-10-25決定）: `loc_source='manual'` の行は `lat lon loc_source place elev` を書かない**（`taken_at`・`name`・`md5`・`display`・`fetched_at`・`rotation=0`・`indexed_at` は今までどおり更新。判定は `UPDATE` の中で書く時点の行について行う）。`rotation=0` は③のとおり今のまま | `drive.py:index_drive` |
| ローカルのmtimeが変わった | `mtime taken_at lat lon loc_source` を上書き（**手動GPSは失われる**）。`place elev display` をNULL。**Rust（T3.7）: Driveと同じく、`loc_source='manual'` の行は `lat lon loc_source place elev` を書かない**（`mtime`・`taken_at`・`display`・`indexed_at` は更新） | `drive.py:index_local` |
| Drive側／ローカルから消えた | 行を削除（derivativeも削除）。`favorite`/`hidden`/`manual` も一緒に消える。**Rustも今のまま**（T3.7の決定: ②行の削除は変えない。消した行を旅行ブログが参照していても書き出しが止まらないよう、下の「写真の行が消える／idが変わる」で対応する） | `drive.py:index_drive` / `index_local` |
| Driveのmd5変更時 `inferred_from`/`infer_dt_s` | UPDATEに含まれず、`loc_source` が `drive_meta`／NULL に変わっても**古い値が残る**（読む側は `loc_source` を見るため実害は小さい） | `drive.py:index_drive` |
| 推定の錨が消えた | `inferred` の位置を取り消し、`lat lon loc_source inferred_from infer_dt_s display` をNULLへ。**derivativeのファイルは消さない**（`_thumb_for`・`loc_suggest._image_rel` が `derivatives/<drive_id>.jpg` を拾い直す） | `geo.py:infer_locations` |
| `--organize` | 位置の無いローカル写真（`source='local' AND lat IS NULL'`）を `_excluded/no_gps/` へ移し、**行を削除**する。`favorite`/`hidden` も消える。起動引数 `--organize` の手動操作。**Rustも今のまま**（T3.7の決定: ②） | `drive.py:organize_photos` |
| 推定の錨が消えた後のDrive写真 | `display=NULL` になったDrive写真は `fetch_derivatives`（`source='drive' AND display IS NULL`）が取り直し、`fetched_at` が新しくなる。表示側（E1の新着ブースト）で**新着扱いになり直す** | `geo.py:infer_locations`、`drive.py:fetch_derivatives` |
| 写真の行が消える／idが変わる | 旅行ブログの下書きは画像を `photo_id` で参照する（`travel_blog.py:319` 付近）。行が消えるか再追加で別idになると、下書きの参照が切れる（書き出し時に `_photo_source` が400）。**Rust（T3.7、④）: 書き出しは、行が無い（`missing`）・`hidden`・表示用の画像が使えない（`no_image`）写真だけを飛ばして残りを書き出し、応答の `skipped` で知らせる**（飛ばした写真は manifest・Markdown・HTML から取り除く。全部飛んだら本文だけ。`dry_run` も数える。下書きの保存E15は写真を見ない）。idが変わること自体は今のまま | `travel_blog.py` |
| `manual` | 再推定の対象外（`loc_source IS NULL OR 'inferred'` のみ再評価）。ほかの未解決写真の錨になる | `geo.py:infer_locations` |
| 大量削除の保留（Rustで足した保護。T2.4。**T3.7の⑤で管理画面に表示**） | 消す行が50件超かつDriveの行の20%超、または一覧が空のときは、削除を保留する。**意図した削除も配信に残る**（`geoframe sync --allow-mass-delete`の1回だけで外れ、`serve`の定期実行では外せない） | `drive/`（Rust） |

**P3で決めたこと**（2026-10-25にユーザーが決定。実装は[tasks.md](tasks.md) T3.7、Rustの挙動は[server.md §0](server.md)・§13.7・§14.8・§10.6・§21.9）:

| 番号 | 決定 | Rustの挙動 |
|---|---|---|
| ① | **手で直した位置を残す** | 原本が更新されたとき（Driveの`md5`の変更＝`index_drive`、ローカルの`mtime`の変更＝`index_local`）、`loc_source='manual'`の行は`lat lon loc_source place elev`を書かない（`muni_cd pref city inferred_from infer_dt_s`は元から`SET`句に無く、そのまま）。`taken_at`・`name`・`md5`・`mtime`・`display`・`fetched_at`・`rotation`（③）・`indexed_at`は今までどおり更新。手動でない行は今のまま（原本のGPSかNULLで上書き、`place elev`をNULLにして付け直し）。判定は`UPDATE`の中（`CASE WHEN loc_source = 'manual'`）で**書く時点の行**について行うので、同期サイクルが計画を読んだあとに更新API（E12）が入った行も守られる。`--dry-run`の写しにも同じ書き込み関数を当て、件数に`location_kept`（計画の時点で手動だった更新行の数）を足す。**範囲外（今のまま）**: 推定の取り消し・変更で位置が動いたときに地名を付け直すか（`infer`は地名の列に触らない。[server.md §15.2](server.md#152-infer)の5）は、この決定に含まれず、Pythonと同じ |
| ② | **写真の行の削除は今のまま**（`index_drive`・`index_local`の削除、`--organize`）。お気に入り・除外・手動の位置は行と一緒に消える | 変更なし（Pythonと同じ）。旅行ブログ側の対応は④ |
| ③ | **回転を0に戻すのは今のまま**（Driveの`md5`の変更で`rotation=0`、回転済みのderivativeのファイルは残る） | **決定どおり今のまま。変更なし**（Pythonと同じ） |
| ④ | **旅行ブログの書き出しは、使えない写真を知らせて残りを書き出す**（削除→再追加でidが変わること自体は今のまま） | E16は、manifestの`photo_id`が指す写真の行が無い（`missing`）・除外（`hidden`）・表示用の画像が使えない（`no_image`）とき、全体を400で止めずにその写真だけを飛ばす。応答に`skipped: [{photo_id, reason}]`を足し、飛ばした画像は`blog-manifest.json`・`index.md`・`index.html`から取り除く。全部飛んだら本文だけを書き出す。`dry_run`も数える。`blog-admin.js`が「N枚の写真を書き出せませんでした」を理由ごとの件数付きで出す。下書きの保存（E15）は写真の存在を見ない（[server.md §21.9](server.md)） |
| ⑤ | **大量削除の保留を管理画面に表示する**（画面からは解除しない） | `GET /api/admin/status`の`sync`に、保留があるときだけ`mass_delete_hold: {held, reason: "ratio"\|"empty_listing", drive_rows, since, checked_at}`を足す（無いときはキーを出さない。取り込み無効の`serve`・最初のサイクルの前も同じ）。`admin.js`のSTATUSタブに注意（件数・理由・解除は`geoframe sync --allow-mass-delete`を1回実行、と文字で示す）。保留の記憶はプロセスの中だけ（[server.md §10.6](server.md#106-pythonとの差まとめ)） |

### 2.3b DB外のデータ（`DATA_DIR` ほか）

| パス | 分類 | 備考 |
|---|---|---|
| `DATA_DIR/travel_blog/drafts/<trip_id>.json` | **一次**（利用者が編集した下書き。`travel_blog.py:27` 付近の `DRAFT_DIR`） | 再構築できない。バックアップ対象に含める。`photo_id` で写真を参照する（§2.3） |
| `DATA_DIR/blog_exports/` | 派生（書き出し結果） | 下書きから作り直せる。ディレクトリ名は変えない |
| `DATA_DIR/derivatives/` | 派生（Drive原本から再取得できる） | 再取得すると `fetched_at` が新しくなる（§2.2） |
| `DATA_DIR/tile_cache/` | 派生（地図タイルのキャッシュ） | |
| `DATA_DIR/.cache.json` | 派生（地理院ジオコーダ結果のキャッシュ） | §3.2 |
| `DATA_DIR/models/rank_model.json`（`GEOFRAME_RANK_MODEL` で変更可） | **要判断**（学習済みの順位モデル。無ければ固定式にフォールバック） | 再生成の手段をこのリポジトリで確認できていない。Rustへ引き継ぐか決める |
| `drive_key.json`（`ROOT` 直下。`GEOFRAME_DRIVE_KEY`） | **秘密** | リポジトリ・ログ・文書に出さない |

### 2.4 スキーマ移行（`store.py:db`）

起動のたびに実行され、失敗（既に列がある）は握りつぶす。

| # | 内容 | 補足 |
|---|---|---|
| M0 | `PRAGMA journal_mode=WAL`、`SCHEMA` の `CREATE TABLE/INDEX IF NOT EXISTS` | `SCHEMA` の `photos` には `muni_cd pref city favorite hidden rotation` が**無い**（下のM1・M2でALTER追加される）。Rustが空DBから作るなら、これらを含めた定義が必要 |
| M1 | `photos` に `muni_cd TEXT, pref TEXT, city TEXT` を追加。**追加できたときだけ** `UPDATE photos SET place=NULL`（全行を再enrichさせる） | 既存DBではすでに列があるので何も起きない。Rustが同じ処理を入れると、新規列追加時にGSIを大量に叩く |
| M2 | `photos` に `favorite INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0, rotation INTEGER DEFAULT 0` | M1とは別ループ（`place=NULL` を誘発させないため） |
| M3 | `photos.infer_dt_s INTEGER`（`SCHEMA` にも入っている） | M1とは別（同上） |
| M4 | `photo_ai_features` に `horizon_status TEXT NOT NULL DEFAULT ''`、`horizon_score INTEGER` | 判定規則v3の追加列。過去評価は空欄のまま |
| M5 | `photo_ai_groups` に `status` `same_scene` `judge_version` `judge_model` `shortlist_json` `error` `attempts` `judge_order` `presented_json` | 既存PoCグループは `judged` として扱う（既定値が `'judged'`） |
| M6 | `photo_ai_group_members` に `shortlisted`、`local_score` | |
| M7 | `INSERT OR IGNORE INTO photo_embeddings ... SELECT ... FROM photo_ai_features WHERE error=''` | **毎起動**実行 |

### 2.5 `settings` のキー

| キー | 値 | 書く | 読む |
|---|---|---|---|
| `photo_quality_dirty` | `"1"` / `"0"`。特徴量が更新され品質の再構築が要る印 | `quality.py:_analyze_pending` / `_remove_orphans` / `rebuild_quality_if_ready` | `quality.py:rebuild_quality_if_ready` |
| `photo_ai_status` | AIサイクルの結果（JSON） | `ai_pipeline.py:ai_cycle` | `ai_search.py:api_ai_status` |
| `photo_ai_last_error` | 直近のAIパイプライン失敗（500字以内）。成功でキー削除 | `ai_pipeline.py:ai_cycle` / `ai_worker` | `ai_search.py:api_ai_status` |
| `photo_loc_status` | 位置候補サイクルの結果（JSON） | `loc_suggest.py:loc_suggest_cycle` | `loc_suggest.py:loc_status` |

### 2.6 AC4（ベクトル空間）に関わる現状

P4で直す対象を明確にするため、現行の「ベクトル空間の鍵」を記録する。

- `photo_ai_features` は**写真ごとに今のモデルの1行だけ**を持つ（`photo_id` が主キー）。`embedding_model` で絞った行だけを検索・グループ化に使う（`ai_search.py:_valid_rows`、`api_admin_status`）。切替直後の他モデル行は使われず、順に作り直される。
- `photo_embeddings` の主キーは `(content_sha256, embedding_model)` で、**`embedding_dim` は鍵に入っていない**。次元は `EMBED_DIM=768` にコード固定。
- 前処理版・ベクトル空間IDに当たる概念は無い（画像は768pxのJPEGで送る固定の前処理）。モデル名＝空間とみなしている。

---

## 3. バックグラウンド処理

すべて同一プロセス内のスレッドと、プロセス外のsystemdタイマー。例外は握りつぶしてログを出し、次の周期で再試行する形が基本。

### 3.1 一覧

| # | 処理 | 周期 | 内容 | 外部通信 | DB書き込み | 設定項目 | 実装箇所 | フェーズ |
|---|---|---|---|---|---|---|---|---|
| B0 | 起動時の掃除・準備 | 起動時に1回（この順） | `load_cache`（`.cache.json` 読込）→ `purge_cache_legacy`（現役でないキーを破棄し、あれば即保存）→ `db()`（スキーマ・移行）→ `tile_cache_prune` → `prepare_tmp_dir`（`DATA_DIR/tmp/` を用意し、`tmp/` と `derivatives/` の `.dl_*` を削除）。その後に各スレッドを起動 | なし | M0〜M7、キャッシュ保存 | `GEOFRAME_DATA_DIR` | `server.py`、`store.py:load_cache` / `purge_cache_legacy`、`drive.py:prepare_tmp_dir` | P1（準備）/P2（`.dl_*` 掃除） |
| B1 | 同期サイクル `worker` | 起動直後に1回、以降 `GEOFRAME_DRIVE_POLL_MIN`（10分）ごと | `cycle()`: `index_local` → Drive認証→`index_drive`（一覧取得・追加・更新・削除）→ `infer_locations` → `enrich_rows` → `local_derivatives` → `fetch_derivatives` → `quality_cycle` → 件数ログ。Drive鍵が無ければローカルのみ。各段は個別に例外を握る | Drive（`drive.readonly`、一覧とダウンロード）、GSI（逆ジオ・標高・`muni.js`） | `photos`（追加・更新・削除、位置推定、地名、display）、derivative ファイル、`.cache.json` | `GEOFRAME_DRIVE_*`、`GEOFRAME_FETCH_PER_CYCLE`(25)、`GEOFRAME_INFER_*`、`GEOFRAME_PHOTOS_DIR` | `catalog.py:worker` / `cycle`、`drive.py`、`geo.py` | P2 |
| B2 | 地名のenrich | B1の中 | 位置あり・`place IS NULL` を**1サイクル40件まで**。日本国内（おおよその矩形）ならGSIの逆ジオと標高（0.2秒待つ）、国外は任意のオフライン逆ジオコーダ（無ければ空で確定）。通信失敗はキャッシュせず次サイクル再試行、応答が空なら空で確定（`LIMIT 40` を占有させないため） | GSI | `photos.place elev muni_cd pref city`、`.cache.json`（`geo:`） | - | `geo.py:enrich_rows` / `muni_table` | P2 |
| B3 | 品質解析 | B1の中 | 孤児の特徴量削除 → 未解析を `GEOFRAME_QUALITY_PER_CYCLE`（既定=FETCH_PER_CYCLE）まで解析（SHA-256・32pxのpHash・鮮明度・露出など）→ 未解析が0かつ `photo_quality_dirty=1` のときだけ全写真を再グルーピング（完全重複=SHA一致、類似=3分以内かつpHash距離12以下）して `photo_quality` を全差し替え。失敗しても同期は止めず、前回結果を維持 | なし | `photo_quality_features`、`photo_quality`、`settings` | `GEOFRAME_QUALITY_PER_CYCLE` | `quality.py:quality_cycle` | P2（旧スコアとの一致は不要） |
| B4 | タイルキャッシュの掃除 | 起動時に1回＋B1の各周期の後に「前回から1時間超なら」実行 | 合計が `GEOFRAME_TILE_CACHE_MAX_MB`（16384）を超えたら mtime の古い順に消して90%まで縮める | なし | `tile_cache/` のファイル削除 | `GEOFRAME_TILE_CACHE_MAX_MB` | `tiles.py:tile_cache_prune`、`catalog.py:worker` | P3 |
| B5 | AIワーカー `ai_worker` | 起動10秒後に開始、以降 `GEOFRAME_AI_POLL_S`（60秒、30秒未満は30秒）ごと。`GEOFRAME_AI_ENABLED=0` ならスレッドを作らない | `ai_cycle`: 非代表embedding削除 → 品質特徴量のある写真を読み込み → 未embeddingを `GEOFRAME_AI_EMBED_PER_CYCLE`(12) 枚まで（キャッシュ命中分は数えない）→ 同一場面グループを再構築（時刻窓15分・cosine 0.82以上、1グループのGemini比較対象は最大6枚、グループ内順位は `rank_model.json` か固定式）→ 未判定グループを `GEOFRAME_AI_JUDGE_PER_CYCLE`(2) 件までGeminiで比較（最大5回失敗で `error` のまま停止）。**同一ループで続けて B6 を実行**（片方の失敗でもう片方を止めない） | Gemini（embedding：768px JPEGをinline送信、比較：`gemini-3.1-flash-lite` の `generateContent`で最大6枚の画像を送信）。`EMBED_BACKEND=local` では**embedding用の画像は送らないが、比較用の画像は常にGeminiへ送る**（`ai_pipeline.py:ai_cycle`） | `photo_ai_features`、`photo_embeddings`、`photo_ai_groups`、`photo_ai_group_members`、`settings` | `GEOFRAME_AI_*`、`GEOFRAME_EMBED_BACKEND`、`GEOFRAME_LOCAL_EMBED_*`、`GEOFRAME_RANK_MODEL`、`GOOGLE_*` | `ai_pipeline.py:ai_worker` / `ai_cycle` | P4 |
| B6 | 位置候補 | B5と同じループ内（`GEOFRAME_LOC_SUGGEST=1` かつ AI有効のとき） | (1) 位置なし・撮影時刻ありの写真を対象にする (2) `Decision` の `/health` が `ready` かつ画像対応なら、未判定を `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE`(60) 枚まで判定（1536px JPEGをbase64で送る。ローカルLAN）(3) 撮影時刻順に12時間超の空きか日付の変わり目で「日」にまとめる。**まとまり全体が判定済みのときだけ**、手がかり有り（`clue>=0.2` か `landmark`）の写真を最大6枚選んでGemini（`GEOFRAME_LOC_SUGGEST_MODEL`、既定 `gemini-3.8-flash`）へ送る。1サイクル `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE`(2) 日まで、最大5回失敗で停止 (4) 答えの地名を**Nominatimのみ**で座標化（見つからなければ最初の区切りを外して1回だけ引き直し、当たったら精度を1段粗くする。`loc_suggest.py:_geocode`）して `photo_loc_suggestions` に保存。**Decisionが応答しない間はGeminiに写真を送らない**（家の中の写真を外へ出さない関門） | Decision（LAN、`X-Client-Name: geoframe`）、Gemini、Nom | `photo_loc_clues`、`photo_loc_runs`、`photo_loc_suggestions`、`settings`、`.cache.json`（`geocode_osm:`） | `GEOFRAME_LOC_SUGGEST*`、`GEOFRAME_DECISION_URL` | `loc_suggest.py:loc_suggest_cycle` | P4 |
| B7 | キャッシュ保存 `cache_saver` | 10秒ごとに起床。変更があり、前回保存から60秒以上のときだけ書く | `.cache.json` を `tmp` に書いて `os.replace` | なし | `DATA_DIR/.cache.json` | - | `store.py:cache_saver` / `save_cache` | P2（逆ジオ・地名検索のキャッシュを引き継ぐ場合） |
| B8 | 流量制限・メモリキャッシュ（スレッドではない） | リクエスト時 | タイル上流取得: 直近60秒に240枚（`time.time()`）。AI検索: 直近60秒に30件（`time.monotonic()`。コードのコメントで意図的に使い分け）。Nominatim: 全体で1秒1件。検索語embeddingのLRU 64件。ディスク使用量の60秒キャッシュ（E2）。位置候補の逆ジオ再試行集合 | - | なし | `GEOFRAME_TILE_FETCH_PER_MIN`、`GEOFRAME_AI_SEARCH_PER_MIN` | `tiles.py`、`ai_search.py`、`geo.py`、`admin.py` | P3 / P4 |
| B9 | CLIモード | 手動 | `python server.py --organize`（位置の無いローカル写真を `_excluded/no_gps/` へ移動し、行も削除）、`--quality-scan`（品質を一括解析）。`app/geoframe/prefill_embeddings.py`（localバックエンド切替前に embedding を先に作る保守ツール） | `prefill`はlocal embeddingのみ | `photos`削除、`photo_quality*`、`photo_embeddings` | - | `server.py`、`drive.py:organize_photos`、`quality.py:quality_backfill`、`prefill_embeddings.py:main` | P2 / P4 |
| B10 | DBバックアップ | 6時間ごと（`geoframe-backup.timer`、`00,06,12,18:17`、`Persistent=true`）。別機体からの引き取りスクリプトあり | `sqlite3 .backup` → integrity_check → gzip → 28世代保持＋`latest.db.gz`。サーバーとは独立 | なし | `/var/backups/geoframe/` | `GEOFRAME_BACKUP_DIR` `GEOFRAME_BACKUP_KEEP` `GEOFRAME_CATALOG_DB` | `deploy/backup-catalog.sh`、`deploy/backup-pull*.sh`、`deploy/geoframe-backup.*` | P5（復元演習。Rust本体は関与しない） |
| B11 | 位置判定用Decisionの起動管理 | 毎朝5:00（ユーザーunit、`Persistent=false`） | `/api/admin/status` の `db.loc.clue_pending` を見て、判定待ちがあり、GPUが空いていれば Decision を起動して0になるまで見て止める。**現状は Decision が常駐のため実質何もしない** | Decision、llm-manager（LAN） | なし | `LOC_MORNING_*` | `deploy/loc_decision_morning.py`、`deploy/geoframe-loc-decision.*` | P4（`/api/admin/status` の形を壊さない／廃止判断） |

### 3.2 `.cache.json`（`DATA_DIR/.cache.json`）

store.py の永続キャッシュ。JSONオブジェクト。読み書きは `cache_get` / `cache_put` を通し、スレッド間はロックで保護。**座標を3桁に丸めた値がキーに入るため、ファイルは位置情報を含む**（配信対象外。リポジトリに入れない）。

| キー | 値 | 用途 | 書く |
|---|---|---|---|
| `geo:<緯度3桁>,<経度3桁>` | `{place, elev, muni_cd, pref, city}`（空でも確定として入る） | 逆ジオ・標高の結果。同じ座標をGSIへ二度問い合わせない | `geo.py:enrich_rows` |
| `geocode_osm:[<国コード>:]<検索語>` | 候補の配列（0件も含む。通信失敗は入れない） | Nominatim の結果。公開サーバーへの再問い合わせを避ける | `geo.py:nominatim_search` |
| `muni_table` | 市区町村コード→`[都道府県, 市区町村]` | GSIの `muni.js` から1回構築 | `geo.py:muni_table` |

- 起動時の `purge_cache_legacy` は、上の `geo:` `geocode_osm:` `muni_table` 以外のキー（旧 `exif:` `exif2:` `adsbdb:` `airport:` `intel2:` など）を捨てる。
- 再生成可能な派生データなので、Rust側が引き継がず空から始めても機能は壊れない（GSI・Nomへ再問い合わせが走るだけ）。引き継ぐ価値があるのは `geo:`（件数が多い）。

---

## 4. 設定項目

### 4.1 `GEOFRAME_*`（`app/geoframe/config.py`。`docs/reference/configuration.md` と照合済み）

`docs/reference/configuration.md` は `tools/gen_env_reference.py` が `config.py` から生成するため、config.py の代入と一致している（本文書は両方を見て、`config.py` 以外が読むものを4.3に分けた）。

| 変数 | 既定 | Rust側 | 備考 | フェーズ |
|---|---|---|---|---|
| `GEOFRAME_DATA_DIR` | `app/var` | **引き継ぐ**（本番 `/var/lib/geoframe`） | `.env` は `DATA_DIR` ではなく `app/` から読む。バイナリ配布なら `.env` の置き場を再定義 | P1 |
| `GEOFRAME_HOST` | `0.0.0.0` | 引き継ぐ | | P1 |
| `GEOFRAME_PORT` | `8080` | 引き継ぐ | Host検査のポート判定にも使う | P1 |
| `GEOFRAME_ALLOWED_HOSTS` | 空 | 引き継ぐ | カンマ区切り。`name:port` のポートは無視 | P1 |
| `GEOFRAME_PHOTOS_DIR` | `photos` | 引き継ぐ | `DATA_DIR` 配下であること | P2 |
| `GEOFRAME_DRIVE_KEY` | `drive_key.json` | 引き継ぐ | `app/` 相対（絶対パスなら絶対）。本番は `/var/lib/geoframe/drive_key.json` | P2 |
| `GEOFRAME_DRIVE_FOLDER` | `GeoFrame` | 引き継ぐ | 20字以上の英数字なら生のフォルダIDとして扱う | P2 |
| `GEOFRAME_DRIVE_POLL_MIN` | `10` | 引き継ぐ | 分（小数可）。**T2.1で実装済み**。0以下・非数は起動を断る（[server.md §11.2](server.md)） | P2 |
| `GEOFRAME_INGEST` | （Pythonに無い。Rustの既定`0`） | **Rustだけの項目** | 取り込み（DBへの書き込みと同期サイクル）を有効にする。P5の切替まで本番の書き手はPythonなので、明示（`1`）しない限りRustは書かない。**T2.1で実装済み**（[server.md §11.1](server.md)） | P2 |
| `GEOFRAME_FETCH_PER_CYCLE` | `25` | 引き継ぐ | | P2 |
| `GEOFRAME_QUALITY_PER_CYCLE` | `FETCH_PER_CYCLE` | 引き継ぐ | | P2 |
| `GEOFRAME_INFER_MIN` | `15` | 引き継ぐ | 段階窓は `{INFER_MIN, 2時間, 12時間}` | P2 |
| `GEOFRAME_INFER_OFFSET_MIN` | `0` | 引き継ぐ | カメラ時計のずれ | P2 |
| `GEOFRAME_TILE_UA` | `GeoFrame/0.1 (personal photo frame)` | 引き継ぐ | 本番では連絡先入りを設定する運用。検証環境でのOSMのタイルの実取得は、設定のあとに行う持ち越し（P5の本番切替の前。[plan.md](plan.md) P3・P5）。地名検索（地理院・Nominatim）にも同じ値を使う（T3.10。Pythonは地名検索に固定の`GeoFrame/0.2`） | P3 |
| `GEOFRAME_TILE_CACHE_MAX_MB` | `16384` | 引き継ぐ | | P3 |
| `GEOFRAME_TILE_FETCH_PER_MIN` | `240` | 引き継ぐ | | P3 |
| `GEOFRAME_AI_ENABLED` | `1` | 引き継ぐ（意味は「AIジョブ全体の有効化」） | `0` で E6 が503 | P4 |
| `GEOFRAME_AI_EMBED_PER_CYCLE` | `12` | 引き継ぐ（永続ジョブの上限に読み替え） | | P4 |
| `GEOFRAME_AI_JUDGE_PER_CYCLE` | `2` | 引き継ぐ | | P4 |
| `GEOFRAME_AI_POLL_S` | `60` | 引き継ぐ（ジョブの起床間隔） | 30秒未満は30へ | P4 |
| `GEOFRAME_AI_TIME_WINDOW_MIN` | `15` | 引き継ぐ | 同一場面の時刻窓 | P4 |
| `GEOFRAME_AI_SIMILARITY` | `0.82`（localは`0.83`） | モデルごとの設定に変える | ベクトル空間ごとのしきい値になる（AC4） | P4 |
| `GEOFRAME_AI_VISUAL_SIMILARITY` | `0.90`（localは`0.91`） | 同上 | | P4 |
| `GEOFRAME_AI_SHORTLIST_MAX` | `6` | 引き継ぐ | 2〜6にクランプ | P4 |
| `GEOFRAME_AI_JUDGE_SHUFFLE_RATE` | `0.2` | 引き継ぐ（または廃止検討） | 判定の偏りを見る実験用 | P4 |
| `GEOFRAME_AI_SEARCH_PER_MIN` | `30` | 引き継ぐ | | P4 |
| `GEOFRAME_EMBED_BACKEND` | `gemini` | **廃止候補** | plan.md ではembeddingをlocal-model-lab（またはProvider設定）へ移す。`local`（このサーバーのCPU）は引き継がない想定 | P4 |
| `GEOFRAME_LOCAL_EMBED_MODEL_DIR` | `DATA_DIR/models/embeddinggemma-2` | **廃止候補** | 同上 | P4 |
| `GEOFRAME_LOCAL_EMBED_THREADS` | `4` | **廃止候補** | 同上 | P4 |
| `GEOFRAME_RANK_MODEL` | `DATA_DIR/models/rank_model.json` | 要判断（P4の開始前） | 家庭の写真の評価から作る重み。無ければ固定式。リポジトリに入れない | P4 |
| `GEOFRAME_LOC_SUGGEST` | `1` | 引き継ぐ | AI有効のときだけ動く | P4 |
| `GEOFRAME_DECISION_URL` | `http://127.0.0.1:8100/llm-decision` | 引き継ぐ（Provider設定の一部になる） | 末尾スラッシュ除去。`/health` と `/v1/systemone` を叩く | P4 |
| `GEOFRAME_LOC_SUGGEST_MODEL` | `gemini-3.8-flash` | 引き継ぐ（タスクごとのモデル設定に移す） | | P4 |
| `GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE` | `60` | 引き継ぐ | | P4 |
| `GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE` | `2` | 引き継ぐ | | P4 |

`GEOFRAME_` の接頭辞でも `config.py` が読まないもの（deploy側）は4.3。

### 4.2 `config.py` が環境以外で決める定数

Rustでも固定値として再現が必要なもの: `DERIV_PX=2560`（derivativeの長辺）、`EXTS`（`.jpg .jpeg .png .tif .tiff`、HEIC対応時は `.heic .heif`）、`DRIVE_MIMES`（`image/jpeg png heic heif tiff`）、`INFER_WINDOWS_S`、`JP_BBOX=(20.0,122.0,46.5,154.5)`、`TILE_SOURCES`（URL・maxzoom・同時接続）、`PREF_REGIONS`（47都道府県。現行の画面では未使用に見えるが `JP_PREFS` として定義）、GSI・Nominatimの各URL、derivativeのJPEG品質85、Gemini比較用の画像768px・品質82。

### 4.3 その他の環境変数

| 変数 | 読む場所 | 意味 | Rust側 | フェーズ |
|---|---|---|---|---|
| `GOOGLE_APPLICATION_CREDENTIALS` | `ai_search.py` | Vertex AI のサービスアカウント鍵（未設定ならADC） | 引き継ぐ（Provider設定のクラウド側） | P4 |
| `GOOGLE_CLOUD_PROJECT` / `GOOGLE_CLOUD_PROJECT_ID` | `ai_search.py` | プロジェクトID（無ければ認証情報から検出） | 引き継ぐ | P4 |
| `GOOGLE_CLOUD_LOCATION` | `ai_search.py` | 既定 `us` | 引き継ぐ | P4 |
| `HF_HUB_OFFLINE` `TRANSFORMERS_OFFLINE` `HF_HOME` | `local_embedding.py`（`setdefault`） | localバックエンド専用 | 廃止候補（localバックエンドと運命を共にする） | P4 |
| `TZ` | systemd unit | `Asia/Tokyo`。naiveな時刻の解釈に効く（§6） | **引き継ぐ**（unitに明記）。Rustはnaiveな時刻を環境のTZに依存しない形で扱うのが望ましい | P1 |
| `PYTHONUNBUFFERED` | systemd unit | stdoutのバッファ無効化 | 不要 | P5 |
| `GEOFRAME_BACKUP_DIR` `GEOFRAME_BACKUP_KEEP` `GEOFRAME_CATALOG_DB` | `deploy/backup-catalog.sh` | バックアップ先・世代数・DB | 引き継ぐ（スクリプトはPython非依存） | P5 |
| `GEOFRAME_BACKUP_PULL_DIR` `GEOFRAME_BACKUP_PULL_KEEP` `GEOFRAME_BACKUP_SSH_KEY` `GEOFRAME_BACKUP_SSH_TARGET` `GEOFRAME_N100_SSH_KEY` `GEOFRAME_N100_SSH_TARGET` | `deploy/backup-pull*.sh` | 別機体からのバックアップ取得 | 引き継ぐ | P5 |
| `GEOFRAME_CONTAINER` | `deploy/backup-pull.sh` | LXCコンテナ名（既定 `geoframe`） | 引き継ぐ | P5 |
| `GEOFRAME_SSH_TARGET` | `deploy/deploy.sh` | 旧rsync配布先 | 廃止候補（Rust配布に置き換え） | P5 |
| `LOC_MORNING_GEOFRAME_STATUS_URL` `LOC_MORNING_LLM_MANAGER_URL` `LOC_MORNING_OLLAMA_URL` `LOC_MORNING_DECISION_URL` `LOC_MORNING_DECISION_UNIT`、`STATE_DIRECTORY` | `deploy/loc_decision_morning.py` | 朝のDecision起動管理 | Decisionが常駐のため実質不要。判断はP4 | P4 |

systemd unit（`deploy/geoframe-server.service`）が与えるもの: `User=geoframe`、`StateDirectory=geoframe`（`0750`）、`GEOFRAME_DATA_DIR=/var/lib/geoframe`、`GEOFRAME_DRIVE_KEY=/var/lib/geoframe/drive_key.json`、`TZ=Asia/Tokyo`、`EnvironmentFile=-/etc/default/geoframe`（後勝ち）、`Restart=always`、ハードニング（`NoNewPrivileges` `ProtectSystem=strict` `ProtectHome` `PrivateTmp` `SystemCallFilter=@system-service`）。`MemoryDenyWriteExecute` は付けない（Pillowのため。Rust化後は付けられる可能性があるが、画像ライブラリ次第）。

---

## 5. 代表的なE2Eシナリオ

Issue #54 の受入条件「写真追加→位置補完→表示→選別→検索→管理画面操作」を、現行の画面とAPIの手順にした。結果の数値一致は求めず、**DBの列・APIの応答で確かめる**。検証環境では Drive とクラウドAIを既定で無効にして行う（[plan.md](plan.md) の「守ること」）。写真は合成したものを使う。

| # | シナリオ | 手順（現行の画面・API） | 確認方法 | フェーズ |
|---|---|---|---|---|
| S1 | ローカル写真の追加→表示 | GPSとDateTimeOriginal入りのJPEGを `DATA_DIR/photos/` に置く → 次の同期サイクル（`GEOFRAME_DRIVE_POLL_MIN` 待ち）→ E1 を呼ぶ | `photos` に `source='local'`・`loc_source='exif'`・`display='photos/...'`。E1に1件増え、`file` が `photos/...`、`GET /photos/...` が200 | P2（取り込み）、P1（表示） |
| S2 | Driveの写真（GPS有）の追加 | Driveの `GeoFrame` フォルダへJPEGを追加 → サイクル → E1 | `loc_source='drive_meta'`。`display='derivatives/<drive_id>.jpg'`（長辺≦2560）。`fetched_at` が入る。E23が200。**検証環境の準備**: Driveは無効のままにし、Drive APIのスタブ（`files.list` が `imageMediaMetadata` 付きの合成ファイルを返し、`files.get` が合成JPEGを返す）を差す。または `source='drive'` の行と `derivatives/<drive_id>.jpg` を入れた検証用DBで索引後の状態から始める | P2 |
| S3 | GPSなし写真の位置補完 | GPSなし・撮影時刻が既存の位置あり写真の±15分／±2時間／±12時間以内の写真を追加 → サイクル → E3 `filter=inferred` | `loc_source='inferred'`、`inferred_from`＝錨のid、`infer_dt_s` が窓に合う。E3の `infer_bucket` が `±15分` など。E2の `db.infer_buckets` に集計。E1に含まれ `inferred=true` | P2 |
| S4 | 手動GPSと再推定の波及 | `filter=noloc` の写真を選び、地名検索（E4）で近くへ飛び、地図で位置を指定して SAVE（E12） → 次サイクル | `loc_source='manual'`、地名列が一旦NULL→サイクル後に埋まる。近い時刻の未解決写真が新しい `manual` を錨に推定される。**次のサイクルで `manual` が上書き・取り消しされない** | P3（更新）、P2（推定） |
| S5 | 選別（お気に入り・除外） | E13 で `hidden:true` → E1 から消える。`favorite:true` → E1の `fav=true`。AIの「同一場面の推奨外」に当たる写真をお気に入りにする | E1の件数が減る／`fav` が真。AIが抑制していても `favorite` は残る（`photos.py:AI_VISIBLE`）。E3 `filter=hidden/fav` に出る | P3 |
| S6 | 回転 | Drive由来の写真で E14 `delta:90` | `rotation=90`、`display` が `derivatives/<drive_id>.r90.jpg` に変わり旧ファイルが消える。E23 が新URLで200。E1の `file` も新しい名前（miharashi側のキャッシュ回避）。**検証環境の準備**: Driveを無効にしたまま、`source='drive'` の行と `derivatives/<drive_id>.jpg` を入れた検証用DBを使う（現行の回転は既存derivativeを回すだけで、Drive通信は要らない。`display` のファイルが実在することが前提）。Driveを使うなら上のスタブ | P3 |
| S7 | 重複の選別（品質解析） | 同じバイト列の写真を2枚入れる／3分以内のほぼ同じ写真を入れる → サイクル → E1・E2 | `photo_quality` に `exact`/`similar` の行ができ、完全重複は推奨外の1枚がE1から消える（`quality_kind='exact'` かつ非推奨）。E2の `db.quality` に `exact_suppressed` | P2 |
| S8 | AI検索（クラウド有効時） | E5 でパイプライン状態を確認 → E6 `q=<自然文>`、`q=` 空 | `mode='semantic'`／`'recommended'`、`groups[].photos[]` に類似度。`photo_ai_features` の `embedding_model` が今のモデルの行だけを検索対象にする。Geminiを無効にすると E6 は503（`AI検索は無効化されています`）で、E1〜E3 は影響を受けない。**検証環境の準備**: クラウドAIは無効のまま、embedding を返すスタブProvider（固定ベクトルを返す）か local-model-lab を使う。`photo_ai_features` に検証用のembedding行を入れたDBでもよい | P4 |
| S9 | 位置候補（Decisionが止まっている場合） | 位置なし写真がある状態で Decision を止めて待つ | E2の `db.loc.decision='unavailable'`、`clue_pending` が減らず、`photo_loc_runs` に行が増えない（**写真はGeminiに送られない**）。再開後に `filter=suggest` に候補が出る。候補は確定するまで `photos.lat` に書かれない | P4 |
| S10 | 旅行ブログの下書き→書き出し | `/blog-admin` で旅程を選ぶ（E7〜E10）→ 編集 → E15 保存 → E16 実行 | `DATA_DIR/travel_blog/drafts/<id>.json` ができる。`blog_exports/<id>/index.html` と `images/photo-<id>.jpg` ができ、画像のEXIF（GPS）が無い。`GET /blog_exports/<id>/index.html` が200。存在しなくても `GET /blog_exports/.<id>.previous/` や `/blog_exports/.<id>-x/` が404ではなく403（ドット始まりの要素は途中でも403） | P3 |

**同期サイクルの手動実行**: 現行には1回だけ動かす公式手段が無い（`catalog.py:cycle` はワーカーが `GEOFRAME_DRIVE_POLL_MIN` ごとに呼ぶ。検証では `python -c "from geoframe.catalog import cycle; cycle()"` を `app/` で動かして代用できる）。**Rust側で、同期サイクルを1回動かす手動トリガー（CLIサブコマンド等）を用意する**。S1〜S4、S7 の「サイクル待ち」はこれで置き換える。

→ **T2.1で用意した**: `geoframe sync`（1回だけ実行。`GEOFRAME_INGEST=1`が要る）と`geoframe sync --dry-run`（書かずに件数だけ）。[server.md §11.3](server.md)。

S5〜S6 は管理画面の操作のE2E、S8〜S9 は「local-model-lab停止／lease中でも閲覧・管理できる」の確認（AC3）にも使う。

---

## 6. 移行時の注意点

### 6.1 時刻・順序

- **時刻の形式が列ごとに違い、契約もそれに依存している**。
  - `taken_at`: `YYYY-MM-DD HH:MM`（ローカル、タイムゾーン無し）。
  - `indexed_at` / `fetched_at`: `datetime.now().isoformat(timespec='seconds')`＝**オフセット無し**の `YYYY-MM-DDTHH:MM:SS`。契約（[写真フィードの契約](../architecture/photo-feed-contract.md)）もこの形。RFC3339（`+09:00` 付き）で出さないこと。
  - `analyzed_at`（`photo_quality*`、`photo_ai_*`、`photo_loc_*`）: `datetime.now().astimezone().isoformat(timespec='seconds')`＝`+09:00` 付き。
  - `admin.py` の `server_time` / `disk.measured_at`: `time.strftime('%Y-%m-%d %H:%M:%S')`。
- **naiveな時刻をそのまま比較・差分する**: `util.py:t_epoch` は `datetime.strptime(...).timestamp()`（環境のTZで解釈）。位置推定・旅の区切り（48時間）・品質グルーピング・ブログ候補の日付範囲・`loc_suggest` の「日」まとめ（`taken_at[:10]`）がすべてこれに乗る。ユニットは `TZ=Asia/Tokyo` を明示している（未設定のUTCだと「今日の写真」等が9時間ずれた実害がある）。日本にはDSTが無いので、Rustは**TZに依存しないnaiveな日時（例: `chrono::NaiveDateTime`）で差分を取る**のが安全。`Local` を使うなら `TZ` を維持する。
- **並び順**: E1 の `ORDER BY` は `CASE WHEN taken_at='' THEN 1 ELSE 0 END, taken_at` だけで**`id` の同順位規則が無い**。同じ `taken_at` の写真の順序はSQLiteの実行計画次第。E3 は `..., taken_at, id`。T1.2 の「全件比較」では、現行Python実装の E1 の出力が実際にid順になっているかを先に確かめてから `, id` を足すか決める。`taken_at` は両インデクサが空文字で入れるのでNULLは来ないはずだが、NULLがあれば先頭に来る（`CASE` が0になるため）。
- `trip` の採番は E1 の**取得結果の並び**に依存する（`assign_trips`）。並びが変わると番号が変わる。写真が増えると番号がずれる（契約に明記）。

### 6.2 パス・ファイルの安全

- パス正規化の手順を**同じ順序で**再現する: デコード → `\`→`/` → NUL拒否 → `normpath`。判定はデコード後（`%2e%2e` 対策）。
- **ドット始まりの要素は途中のディレクトリでも403**。`DATA_DIR/tmp/.dl_<drive_id>`（Drive原本の一時ファイル。フル解像度・EXIF完全保持）を `derivatives/` の外に置いているのもこのため。起動時に `.dl_*` を削除する処理も維持する。
- ディレクトリは配信しない（一覧を出さない）。`/derivatives` 単体もDATA_DIR側へ寄せて403にする挙動。
- アプリ内の画像読み出しは `DATA_DIR` の外へ出ないことを確認する（`path.resolve().relative_to(DATA_DIR)`）。`quality.py`・`ai_pipeline.py:_safe_path`・`loc_suggest.py:_image_bytes`・`travel_blog.py:_photo_source` が同じ検査をしている。DBの `display` 列は信用しない前提で書かれている。
- `/photos/` はローカルのJPG/PNG原本そのものを配信しうる（EXIFのGPSを含む）。Rustでも配信範囲を勝手に広げ・狭めない（E24）。

### 6.3 ログに出してはいけない情報

Pythonの現行ログには、Rustでは出さないほうがよいものが含まれる。アクセスログは無効（`log_message`）。

- ファイル名: `[fetch] <name> → derivative生成`、`[local] <path>: 読み込み失敗`。
- 位置・日付に基づく推定: `[loc] 撮影地の候補: <日付> → <推定地名>（精度、送信枚数）`。
- 利用者の入力: `[geo] 地名検索失敗（... q=<検索語>）`。
- 座標: `[tile] <layer> z/x/y`（タイル座標は粗いが、連続すると行動を示しうる）。
- 秘密: Drive鍵・Google認証トークン・`Authorization` ヘッダ・サービスアカウントのパス内容。例外文字列に含まれうる（`Agent Platform HTTP <code>: <本文300字>` はそのままログに出ている）。
- 例外文字列をHTTP応答に返さない（500は定型文）。SQLite・OSErrorはローカル絶対パスやスキーマ片を含むため。

出してよい例: 件数・サイクルの所要時間・状態名・HTTPステータス・IDのみ（写真IDは可）。

### 6.4 暗黙の挙動で、Rustでも再現が必要なもの

| 挙動 | 内容 | 実装箇所 |
|---|---|---|
| 起動時の掃除 | `.dl_*` の削除、キャッシュの旧キー破棄、`tile_cache_prune` | `drive.py:prepare_tmp_dir`、`store.py:purge_cache_legacy`、`tiles.py:tile_cache_prune` |
| Drive側で消えた写真の削除 | 一覧に無い行を、derivative ごと削除（`favorite`/`hidden`/`manual` も消える） | `drive.py:index_drive` |
| ローカルで消えた写真の削除 | 同上（`display` が `derivatives/` で始まる場合だけファイルを消す。`photos/` の原本は消さない） | `drive.py:index_local` |
| 旧ミラー `photos/gdrive/` の除外 | パスのどこかに `gdrive` があれば索引しない | `drive.py:index_local` |
| 位置推定の取り消し | 錨が消えたら `inferred` を取り消し、`display=NULL` にする（ファイルは残す） | `geo.py:infer_locations` |
| 位置なし写真にもderivativeを作る | 位置が無くても管理画面で見て指定できるように、Drive写真は位置の有無で絞らず取得する。ローカルは位置ありだけ | `drive.py:fetch_derivatives` / `local_derivatives` |
| 失敗してもEXIF付き原本を残さない | `finally` で一時ファイルを削除 | `drive.py:fetch_derivatives` |
| 逆ジオの「空で確定」 | 応答が空でも確定扱い。通信失敗だけ再試行（`LIMIT 40` の占有防止） | `geo.py:enrich_rows` |
| 品質・AI行の有効性判定 | `display` / `md5` / `mtime`（`IS` 比較）が現在の `photos` と一致する行だけ採用。画像が変わると自動で無効になる | `photos.py:QUALITY_JOIN`、`ai_search.py:_valid_rows` ほか |
| 品質の「全員分そろってから再構築」 | 未解析が残る間は前回の `photo_quality` を維持 | `quality.py:rebuild_quality_if_ready` |
| AIグループの判定引き継ぎ | `judge_version` が同じ `group_id` の判定結果は再構築しても保持 | `ai_pipeline.py:_rebuild_groups` |
| 配信の抑制（E1） | 完全重複の非推奨を除外、AIが「同一場面で推奨外」としたもの（お気に入りを除く）を除外。**Rust（P4 / T4.3）は、メンバーの世代が今の写真と合い、同じグループに有効で表示可能な推奨写真があるときだけ抑止する**（[ai-index.md §6](ai-index.md)。意図した変更） | `photos.py:QUALITY_VISIBLE` / `AI_VISIBLE` |
| クラウド送信の関門 | 位置候補は Decision の判定が済んだ「日」だけをGeminiへ送る | `loc_suggest.py:loc_suggest_cycle` |
| ブログ書き出しの個人情報除去 | 再エンコード時にEXIFを渡さない、候補APIは粗い地名だけを返す | `travel_blog.py:_export_image` / `_location_label` |
| 書き込みの原子性 | 品質・AIグループは `BEGIN IMMEDIATE` で全削除→全挿入。derivative・キャッシュ・下書きは tmp→`os.replace`/`rename`。ブログ書き出しは staging→`.previous` を経由して置換 | `store.py`、`util.py:make_derivative`、`travel_blog.py:export_blog` |
| 同時実行 | DBは1接続＋グローバルロック（WAL）。ブログ書き出しは非ブロッキングのロック（取れなければ409）。Nominatim・タイルの上流取得の制限 | `store.py`、`travel_blog.py`、`geo.py`、`tiles.py` |
| 排他の外側にI/Oを出す | レート制限・embeddingキャッシュのロックは窓の更新だけに使い、外部通信はロックの外で行う | `tiles.py`、`ai_search.py` |

### 6.5 画像処理

- derivative は EXIFの向きを適用 → 回転（手動）→ 長辺2560pxに縮小（拡大しない）→ RGB/L以外はRGBへ → JPEG品質85。書き込みは `.part` を経由。回転後はファイル名を変える（`/derivatives/` をブラウザにキャッシュさせているため）。
- HEIC対応は `pillow_heif` が入っているときだけ（`EXTS` が変わる）。Rust側の方式は T0.4 の計測後に決まる。
- 品質解析のpHash・鮮明度は旧値との一致は不要（Issue の非目標）が、**「同じ入力で同じ結果」の再現性は保つ**（派生行の再計算判定が `algorithm` 文字列に依存）。

---

## 7. 調べても分からなかった点

- **`photos.title` と `photos.ai_caption` の書き込み元**: `app/` 内にUPDATE/INSERTが無い。読むだけ（E1の `title`、ブログ候補）。本番DBに値があるか、誰が書いたかは、コードからは不明。P1で値の有無を集計（件数だけ）で確かめ、**一次データとして扱うこと**。
- **`/favicon.ico` の実体**: リポジトリに無い。本番で別途置かれているかは不明。
- **`photo_loc_*` 三表の掃除**: 写真が消えても行が残る。長期の肥大の程度は本番DBを見ないと分からない。
- **`PREF_REGIONS` / `JP_PREFS`**: 定義はあるが、現行コードでの使用箇所は確認できなかった（過去の表示機能の名残の可能性）。
- **オフライン逆ジオコーダ（`reverse_geocode`）**: 任意の依存で、本番に入っているかは不明。入っていないと国外の写真は `place` が空で確定する。
- **本番の `/etc/default/geoframe` の内容**: リポジトリ外。どの `GEOFRAME_*` が実際に上書きされているか（特に `GEOFRAME_AI_ENABLED`、`GEOFRAME_EMBED_BACKEND`、`GEOFRAME_TILE_UA`、`GEOFRAME_ALLOWED_HOSTS`）は不明。P1の開始前に、**値は伏せて、設定されているキーの名前だけ**を確認する。
- **Decision（local-model-lab）の `/health` の応答形**: コードが読むのは `status=='ready'` と `modalities` に `image` が含まれることだけ。lab側の契約（local-model-lab #93 など）は未確認。

### E5/E6 Rust接続（T4.4b）

[HTTP/query契約と合成検証](ai-search.md)を参照。E6の新規送信は確定したLAN BF16契約だけ。旧Cloud/localのベクトルは空検索の同モデル構図比較だけに使い、新queryと混ぜない。Cloud queryは未対応で無送信503。非空queryの新active無しは候補0件でも503、空queryの保存済み推薦はProvider無しで200。新scene/rankはT4.5。
