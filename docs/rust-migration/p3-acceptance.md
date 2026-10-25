# P3の受入確認（検証環境での記録）

- 日時: 2026-10-25に実施
- 追跡: [GEOFRAME #54](https://github.com/ysogabe/geoframe-ambient-display/issues/54)（Rust単一バイナリ化）/ 計画: [plan.md](plan.md) §4 P3 / タスク: [tasks.md](tasks.md) T3.8 / 前フェーズの記録: [p1-acceptance.md](p1-acceptance.md)・[p2-acceptance.md](p2-acceptance.md)
- 対象のソース: コミット`83b7c5f`（T3.7完了時点。`crates/`・`app/`は無変更）
- バイナリ: `cargo build --release --target x86_64-unknown-linux-musl -p geoframe`（17,298,520 B、sha256 `5d044fdc84e27ea9…`）。`deploy/rust-deploy.sh`で配布（検査・転送・入れ替え・確認OK）。配布後の unit は`ReadOnlyPaths`が`photos`だけ（T3.6で`travel_blog`・`blog_exports`を外した版）であることを確認
- 検証環境（lxc-geoframe。Ubuntu 24.04）。本番機ではない。接続先の値はGit管理外のメモにあり、ここには書かない。本番機（geoframe-server）・本番の`/var/lib/geoframe`・本番の`/etc/default/geoframe`・本番のDrive鍵には触れていない
- **写真のid・ファイル名・位置・地名・撮影日時・検索語・旅程の識別子と中身は書かない。件数・状態コード・真偽・ハッシュの先頭だけ**
- 比較用Pythonは`geoframe-pyref`（`:8090`、AI・Drive無効）。**RustとはDATA_DIRが別**（Rustは`/var/lib/geoframe`、Pythonは`/var/lib/geoframe-pyref`）。旅行ブログの書き出しの排他（[server.md §21.6](server.md#216-競合と停止)）が効く前提を満たしている。両方の`catalog.db`は試験の前に同じ内容（`.dump`のSHA-256が`57f6dceff3b48bc5…`で一致、`photos`6,535行すべて`source='drive'`、`hidden`・`favorite`・`rotation`が0、`loc_source='manual'`が17行）

## 0. 結果の要約

**API による受入は通過。画面の目視と一部の実通信（OSMのタイル・Nominatim）、大量削除の保留の実機は、期限付きの持ち越し**（期限はP5の本番切替の前。P3のPRのマージはこれで止めない。§7）。

| 項目 | 結果 |
| --- | --- |
| S5 お気に入り・除外（E13） | **通過。** 除外で`/api/v1/photos`が1件減り`filter=hidden`に出る。お気に入りで`fav=true`・`filter=fav`に出る。元に戻すと両方戻る。RustとPythonで同じ |
| S10 旅行ブログ（E7〜E10・E15・E16） | **通過。** 保存した下書きは**Pythonとバイト列が一致**。書き出し一式（`blog-manifest.json`・`index.md`・`index.html`）は3つともPythonと**ハッシュが一致**、画像41枚は長辺1800px・RGB・EXIF/GPS/ICCなし。`GET /blog_exports/…`が200、ドット始まりは403。同時の書き出しは`[200, 409, 409]`。除外した写真・存在しない写真を含めると、Rustは**200＋`skipped`**で残りを書き出す（Pythonは400でそのまま止まる＝T3.7の決定による差） |
| S4 手動の位置（E12）＋地名検索（E4） | **通過。** `setloc`で`manual`・地名列がNULL → 次のサイクル（約15秒）で地名が埋まり、手動の位置は6サイクル（約95秒）上書き・取り消しされない。近い時刻の未解決の写真2枚が即座に推定され、全体で104行が新しい手動の位置を錨に推定された。**`photos`の全6,535行がPythonと一致**。地名検索は地理院で200・10件（RustもPythonも同じ） |
| S6 回転（E14） | **通過。** 90°を4回で、そのたびに新しいderivativeのURLが200・古いURLが404・フィードの`file`が新しい名前・DBの`rotation`が90→180→270→0。4回目で元の名前（`<drive_id>.jpg`）に戻る。RustとPythonで同じ |
| 地図タイル（E11） | **通過（地理院）。** 3枚が`miss`→2回目は`hit`、`image/png`・`public, max-age=86400`、キャッシュのファイルが3つできる。HEADは200、不正なパスは404。**OSMは持ち越し（ユーザーの設定待ち）**: 検証環境の`/etc/default/geoframe`に`GEOFRAME_TILE_UA`の行が0件（§7） |
| Nominatimの実通信 | **持ち越し。** 地名検索は地理院が結果を返したので呼ばれていない。User-Agent（連絡先なしの`GeoFrame/0.2`）の扱いが決まったあとに管理者が確かめる（§7） |
| 更新APIの入口（CSRF・エラー応答） | **通過。** 12通りの拒否・不正入力の状態コードがPythonと12/12一致（403・400・413・404）。`OPTIONS`は501でCORSヘッダなし。未知のPOSTは403、未知の`GET /api/blog/…`だけ404 |
| `GEOFRAME_INGEST=0` | **通過。** 更新API（`setloc`・`setflag`・`rotate`）・保存・`dry_run`でない書き出しは503（定型文）、入力の誤りは503より先の400。`dry_run`は200。DBの`.dump`・`travel_blog`/`blog_exports`/`derivatives`のファイル一覧・DATA_DIR直下の名前は**前後で一致**。地名検索・タイル・旅行ブログの読み取り・フィード・statusは200 |
| 全件比較（`tools/compare_with_python.py`） | 一覧3,097件がJSONで完全一致。想定外の差2項目はどちらも説明できた（§5）。**説明できなかった差は0** |
| miharashiの表示の継続 | **通過。** 取り込みを有効にしたRustで、定常200枚・停止と再開300枚とも終了コード0 |
| T3.7 ⑤ 大量削除の保留の表示 | **持ち越し（検証環境では出せない）。** 試験での確認に留める（§3）。実機は Drive の鍵が置かれてから管理者が行う（§7） |
| 画面の目視 | **持ち越し（ユーザー）。** HTML/JSの取得・構文・表示関数の動作確認まで（§4）。期限はP5の本番切替の前（§7） |
| 検証環境の最終状態 | `ingest=0`で常駐・`/healthz` 200・`/api/v1/photos` 3,065件・両方のDBは`.dump`のSHA-256が試験前と一致（§8） |

## 1. 準備

| 手順 | 結果 |
| --- | --- |
| バックアップ（検証環境の中、名前に`p3`） | `/var/tmp/geoframe-p3-backup/{geoframe,geoframe-pyref}/`に、`sqlite3 .backup`の`catalog.db`・`blog_exports`・`travel_blog`・`.cache.json`・DATA_DIR直下の名前の一覧・`derivatives/`のハードリンク（`rotate`は別名のファイルを作って古いものを消すだけなので、実体は残る）・`.dump`のSHA-256（両方`57f6dceff3b48bc5…`）。**RustとPythonの両方**（同じ要求をPythonにも送るので、Python側のDBも変わる） |
| 配布 | `deploy/rust-deploy.sh`（印`/etc/geoframe/staging`あり・`geoframe-server`は未導入で通過）。入れ替え後に`/healthz` 200・`/api/v1/photos`が配列 |
| 取り込みを有効に | 実行時のdrop-in（`/run/systemd/system/geoframe-rs.service.d/`）で`GEOFRAME_INGEST=1`・`GEOFRAME_DRIVE_POLL_MIN=0.2`（サイクルを約12秒に）。起動ログは`[guard] 検証環境の印を確認しました`のあと、7段が回る。Pythonにも同じ周期のdrop-in。ファイルの unit は変えていない |
| 起動時のサイクル | `index_local files=0`、`infer anchors=3125 targets=3409 inferred=0`、`enrich candidates=0`、`quality pending=0`。**変更0**（P2と同じ） |

## 2. 管理画面の操作（E2E）

Rust（`:8080`）とPython（`:8090`）に**同じ要求**を同じ順に送り、応答・DBの変化・ファイルを比べた。写真は、`/api/v1/photos`に出ていて品質のグループに入っていない2枚、位置のない写真1枚、フィードの写真1枚を選んだ（idは書かない）。

### 2.1 S5 選別（E13）

| 操作 | Rust | Python |
| --- | --- | --- |
| `hidden:true` | 200。フィード 3,065→3,064件。`filter=hidden`の`total`は1でその写真を含む。DBは`hidden=1` | 同じ |
| `favorite:true`（別の写真） | 200。フィードの`fav`が真。`filter=fav`の`total`は1でその写真を含む。DBは`favorite=1` | 同じ |
| 元に戻す（`hidden:false`・`favorite:false`） | 200・200。フィードが1件戻り（3,097→3,098。S4の推定の分を含む）、`fav`が真の写真は0、`filter=hidden`・`filter=fav`の`total`は0 | 同じ |

除外した写真は旅行ブログの候補（E9）にも出ない（Rust・Python とも）。

### 2.2 S10 旅行ブログ（E7〜E10・E15・E16）

旅程は埋め込みの1件（`GEOFRAME_BLOG_PLANS_DIR`は未設定）。7日の区間、`photo_count`712、候補は区間ごとに最大36。検証環境には、以前に保存した下書き（41枚）が両方にあった。

| 操作 | Rust | Python |
| --- | --- | --- |
| E7 一覧・E8 旅程・E9 候補・E10 下書き | 200。件数・区間ごとの候補数・`privacy.exact_gps_in_response=false`・下書きの章と画像の数が同じ | 同じ |
| E15 保存（`status:"published"`を送る） | 200。`DATA_DIR/travel_blog/drafts/<trip>.json`ができ、**ファイルのバイト列がPythonと一致**（sha256の先頭`35663c0f0062036b`、モード0644、`.part`の残り0）。保存後のE10は`source:"saved"`・`status:"draft"`（`published`は`draft`に落ちる）、E7の`has_draft`が真 | 同じ |
| E16 `dry_run`（普通の下書き） | 200・`images:41` | 同じ |
| E16 `dry_run`（除外した写真を1枚足す） | 200・`images:41`・`skipped:[{reason:"hidden"}]` | 200・`images:42`（`skipped`なし。Pythonは写真を見ない） |
| E16 `dry_run`（存在しない写真を1枚足す） | 200・`images:41`・`skipped:[{reason:"missing"}]` | 200・`images:42` |
| E16 実行（除外した写真を足す） | **200**・`images:41`・`skipped:[{reason:"hidden"}]`。約7秒。書き出し一式は41枚で書き換わる | **400**（「表示用の画像がありません」）。前の書き出しは残る |
| E16 実行（存在しない写真を足す） | 200・`images:41`・`skipped:[{reason:"missing"}]` | 400。前の書き出しは残る |
| E16 実行（普通の下書き） | 200・`images:41`。約5秒 | 200・`images:41`。約8秒 |
| 書き出した一式 | `blog-manifest.json`・`index.md`・`index.html`・`images/`（41枚）。作業領域・`.previous`の残り0。**3つのファイルのsha256の先頭が Python と一致**（`d10a144527d1972e`・`4b710ecc41805445`・`157943135bfa9ed9`） | 同じ |
| 画像41枚の検査 | デコード失敗0・RGB・長辺の最大1800px。JPEGのヘッダ部にAPP1（EXIF）・APP2（ICC）・その他のAPPセグメントが**0**、`Exif`・`GPS`の文字列も0（元はderivativeで、もともとEXIFは持たない。書き出しが付け足さないことの確認） | 同じ |
| `GET /blog_exports/<trip>/index.html`・`blog-manifest.json` | 200（`text/html`・`application/json`） | 同じ |
| `GET /blog_exports/.<trip>.previous/`・`/blog_exports/.<trip>-x/`（実在しない） | **403** | 403 |
| 同時の書き出し3本 | `[200, 409, 409]` | `[200, 409, 409]` |

画像のバイト列はデコーダ・縮小・エンコーダが違うので一致しない（寸法・成分数・EXIF/ICCの有無が一致するだけ。[server.md §21.4](server.md#214-画像htmlmarkdown)）。`no_image`の理由（表示用の画像が無い・読めない）は、検証環境では作らず、`blog_write`の試験（[server.md §21.10](server.md#2110-テスト)）での確認に留める。

### 2.3 S4 手動の位置（E12）と地名検索（E4）

| 操作 | Rust | Python |
| --- | --- | --- |
| E4 地名検索（一般的な地名。実際の地理院） | 200・10件・`source`はすべて`gsi`・各件のキーは`title/lat/lon/source` | 同じ |
| E4 1文字・`q`なし | 200・`items:[]`（通信しない） | 同じ |
| `filter=noloc`の写真に E12 `setloc`（公開の代表点） | 200・`loc_source:"manual"`。直後のDB: `lat`あり・`loc_source='manual'`・`place`/`city`/`elev`はNULL。10分以内の未解決の2枚はまだNULL | 同じ |
| 次のサイクル（約15秒後） | 手動の行: `place`・`city`・`elev`が**埋まる**（`enrich`のログは`candidates=40 cache_hit=39 queried=1`、実際の地理院への問い合わせは1座標）。近い時刻の未解決の2枚: `loc_source='inferred'`・`infer_dt_s`・`inferred_from`・地名が付く。ログは`infer … inferred=104`（錨が増えて、±12時間までの窓で104行が推定された） | 同じ（2枚の行は一致） |
| 以後6サイクル（約95秒） | 手動の行は`loc_source='manual'`のまま（上書き・取り消しされない）。推定の2枚も変わらない | 同じ |
| 全行の比較 | 変わった行（3行）の`lat lon loc_source place elev muni_cd pref city inferred_from infer_dt_s`が一致。**`photos`の6,535行×21列（`indexed_at`・`fetched_at`を除く）がRustとPythonで一致** | |

### 2.4 S6 回転（E14）

Drive由来でフィードに出ている1枚（`derivatives/<drive_id>.jpg`）に`delta:90`を4回。

| 回 | Rust | Python |
| --- | --- | --- |
| 1 | 200・`rotation:90`。新しいURL（`….r90.jpg`）200・古いURL 404・フィードの`file`が新しい名前・DBの`rotation=90` | 同じ |
| 2 | 200・180（`.r180`）。新200・旧404・`file`が新しい名前 | 同じ |
| 3 | 200・270（`.r270`）。新200・旧404・`file`が新しい名前 | 同じ |
| 4 | 200・`rotation:0`。新しいURL（`<drive_id>.jpg`に戻る）200・古いURL 404・`file`が元の名前・DBの`rotation=0` | 同じ |
| 入力の検査 | `delta:45`→400、`delta:"90"`→400、存在しない`id`→404 | 同じ |
| `delta:90.0`（小数） | **400**（Rustのみ試験。Pythonは通して`.r90.0.jpg`を作る既知の差。[server.md §0](server.md#0-pythonとの差の索引)・[§17.8](server.md#178-e14-rotatet32)） | （送っていない） |

`.part`の残り0、回転用の`.r*.jpg`の残り0。回転は再エンコードなので、4回戻した後もderivativeのバイト列は元と違う（§5の差1）。

### 2.5 地図タイル（E11）

| 操作 | Rust | Python |
| --- | --- | --- |
| 地理院のタイル3枚（実際の通信）を2回ずつ | 1回目: 3枚とも`X-Tile-Cache: miss`、2回目: `hit`。200・`image/png`・`public, max-age=86400`・本文はPNG。`tile_cache/gsi`にファイル3つ（`.tmp`の残り0） | 同じ（3つ） |
| `HEAD` | 200 | 200 |
| 形の合わない（`.jpg`）・最大ズーム超過・`x`が範囲外・未知の層・`..` | すべて404 | すべて404 |
| OpenStreetMapのタイル | **持ち越し**（`GEOFRAME_TILE_UA`の行が0件。件数だけ確認。ユーザーの設定待ち。§7） | |

上流への実際の通信は、地理院のタイル（Rust 4枚・Python 3枚）、地名検索（3回）、逆ジオコード・標高（1座標）だけ。OSM（`GEOFRAME_TILE_UA`が未設定）・Nominatim（地理院が結果を返したので呼ばれない）には送っていない。どちらも持ち越し（§7）。

### 2.6 更新APIの入口（CSRF・エラー応答）

Rust と Python に、`/api/admin/setflag`へ同じ要求を送り、状態コードを比べた（12通り）。

| 要求 | Rust | Python |
| --- | --- | --- |
| `Content-Type: text/plain` | 403 | 403 |
| `Origin`がHostと違う | 403 | 403 |
| `Content-Length`なし | 400 | 400 |
| 本文70,000バイト | 413 | 413 |
| 不正なJSON・配列のJSON | 400・400 | 400・400 |
| `id`が文字列・存在しない`id` | 400・404 | 400・404 |
| `Origin`なし（許される）で存在しない`id` | 404 | 404 |
| 未知のPOSTパス（`/api/admin/…`・`/api/blog/…`）・読み取り専用ルートへのPOST | 403・403・403 | 403・403・403 |
| `OPTIONS` | 501・CORSヘッダなし | 501・CORSヘッダなし |
| `GET /api/blog/<未知>`・`GET /api/admin/<未知>` | 404・403 | 404・403 |

**状態コード12/12一致。** 本文はJSONとして等しい（Rustは区切りに空白を入れない。[server.md §8.4](server.md)で決めた既知の差）。

### 2.7 `GEOFRAME_INGEST=0`

drop-inを外してRustを再起動（`取り込みは無効です（DB に書きません）`）。試験の前後で、`catalog.db`の`.dump`のSHA-256、`travel_blog`・`blog_exports`・`derivatives`の全ファイル（名前・大きさ・更新時刻）、DATA_DIR直下の名前の一覧を取り、**3つとも一致**。

| 要求 | 結果 |
| --- | --- |
| `setloc`・`setflag`・`rotate`・`save`・`export`（`dry_run`でない） | **すべて503**（`書き込みが無効なので更新できません…`） |
| `export`（`dry_run:true`） | 200 |
| `id`が不正な`setloc`・`delta`が不正な`rotate`・不正な`trip_id`の`save` | 400（503より先） |
| 地名検索（実際の地理院）・タイル（キャッシュの`hit`と、別のタイルの`miss`） | 200・200（`hit`）・200（`miss`）。タイルは`catalog.db`ではなく`tile_cache/`に書く |
| 旅行ブログの`trips`・`trip`・`candidates`・`draft`・`/blog_exports/…/index.html`・フィード・`status` | すべて200 |

## 3. T3.7 ⑤ 大量削除の保留の表示

`/api/admin/status`の`sync.mass_delete_hold`は、`index_drive`がDriveの一覧から削除を保留したときだけ出る。検証環境にはDriveの鍵が無く、本番のバイナリはDriveの接続先を変えられない（テスト用の入口は`cfg(test)`・ループバック限定）ので、**実機では出せない**。確かめたのは次まで:

- 取り込みを有効にしたRustの`status`には`mass_delete_hold`が**無い**（`sync`のキーは`pending`・`ready`・`total`）。Pythonと`status`の全体が一致（全件比較の項目2）。
- 試験での確認（`server.md §0`の索引にある、`drive`・`admin`・ブラウザ側の表示関数の試験）。**実機の確認は、鍵が置かれたあとに管理者が行う**（[p2-acceptance.md §6](p2-acceptance.md)と同じ）。

## 4. 管理画面の表示

ブラウザでの目視はこの作業ではしていない（**持ち越し。ユーザーに依頼。§7**）。代わりに:

| 確認 | 結果 |
| --- | --- |
| `GET /admin`・`/blog-admin`・`/ai-search`・各CSS・`/vendor/maplibre…` | Rustは200。Pythonとバイト列が一致（`/admin`は4,321 B、`/blog-admin`は2,168 B、`/ai-search`は5,000 B） |
| `GET /static/js/admin.js`・`/static/js/blog-admin.js` | 200。**リポジトリの`app/static/js/`の実ファイルとバイト一致**。Pythonと一致しないのは、比較用Pythonの`app/`がT3.7より前の版のため（§5の差2） |
| `node --check` | 両方とも構文エラーなし |
| 表示関数をNodeで実行（DOMの代役つき） | `massDeleteNotice`: 保留なし（キーなし）は`null`、`ratio`は「Driveの削除を保留中 / 件数 / 理由 / Driveの行 / 保留の開始 / 最後に確認 / 解除のコマンド（`geoframe sync --allow-mass-delete`）と「この画面からは解除できません」」を組み立てる、`empty_listing`は「一覧が空」。`exportSummary`: `skipped`なしは「N枚の画像とHTMLを書き出しました」・警告なし、`skipped`ありは「…M枚の写真を書き出せませんでした（除外 2枚・写真DBに無い 1枚）」・警告あり |

**画面でしか見えないこと（地図の位置指定・回転ボタン・STATUSタブの注意の見た目・ブログの書き出しの結果の文面）の目視は、持ち越しとしてユーザーに依頼する（§7）。**

## 5. 全件比較とmiharashi

`tools/compare_with_python.py`（Rust `:8080` と Python `:8090`、操作を終えた時点。両方とも同じ変更を受けている）:

| 項目 | 結果 |
| --- | --- |
| `GET /api/v1/photos` | 3,097件でJSON完全一致（型も区別、並びも一致）。ヘッダも一致 |
| `/api/admin/status` | 一致（`server_time`・`disk`を除く） |
| `/api/admin/photos`（filter×page×per＋不正なクエリ222通り） | 状態コードとJSONが222/222一致 |
| 表示用画像（`/derivatives/…`） | 3,096/3,097件がバイト一致。**差1件**（下の差1） |
| 管理画面ページ・拒否・メソッド・Host・`/healthz`の既知の差 | ページ7/7、拒否33/33、メソッド10/10、Host 10/10が一致。Rustの埋め込みファイルはリポジトリの実ファイルと14/14一致 |
| 埋め込みファイルのPython一致 | 12/14。**差2**（下） |

**想定外の差2項目は、どちらも説明できた。**

| 差 | 理由 | 分類 |
| --- | --- | --- |
| 1. 回転した写真1枚のderivativeのバイト列（寸法・名前は同じ） | 4回回して戻したが、そのたびに再エンコードしている（Rustはimage/zune-jpeg、PythonはPillow）。P2で既知の「画素はPythonと一致しない」（[server.md §0](server.md#0-pythonとの差の索引)） | 既知の差（想定内） |
| 2. `/static/js/admin.js`・`/static/js/blog-admin.js` | 比較用Pythonの`/opt/geoframe-pyref/app`は、T3.7で変えたこの2ファイルより前の版（`sha256`の先頭がコミット`201d2b4`の版と一致）。Rust は T3.7 の版（`mass_delete_hold`の表示・`skipped`の表示）。ほかの12ファイルは一致 | 版の違い（想定内） |

追加で、全部の操作を終えて（元に戻す操作を含む）、**RustとPythonの`catalog.db`の`.dump`のSHA-256が一致**した（`fa6dc15eda013fb2…`。試験前の`57f6dcef…`とは違う＝手動の位置と推定の104行が増えた分）。同じ要求を同じ順に送ると、DBの全体が同じになる。

**miharashi**（[`tools/miharashi-feed-check/`](../../tools/miharashi-feed-check/README.md)、miharashiのコミット`1bc73a6`。本番の`miharashi.service`には触れていない）。取り込みを有効にしたRust（サイクル約12秒）に向けた:

| 試験 | 結果 |
| --- | --- |
| 定常（200枚） | **終了コード0**。写真183枚＋旅・季節のカード17枚（計200）。`Retrying` 0回。11秒 |
| 停止と再開（300枚。10秒後に`systemctl stop`、8秒後に`start`） | **終了コード0**。写真275枚＋カード25枚（計300）。`Retrying` 4回・`Online` 1回で自動回復。最初の`Retrying`から次の写真まで15.1秒。33秒 |
| ログ | 操作の間のRustのERROR 0。WARNはホストの拒否の見本・未実装メソッドだけ |

## 6. タイルキャッシュの掃除の同時実行（T3.4の引き継ぎ）

RustとPythonの掃除（`tile_cache/`の上限）が**同じ`tile_cache/`に対して同時に走る**場面は、**比較用PythonのDATA_DIRが別なので起きない**（上の準備のとおり）。この確認は不要として扱う。本番の切替（P5）ではPythonを止めてからRustが掃除を持つので、同時には走らない。

## 7. 持ち越しと見つかったこと

**直すべき不具合: なし。**

### 7.1 持ち越し（期限はP5の本番切替の前。P3のPRのマージはこれで止めない）

[plan.md](plan.md)のP3・P5の節と同じ内容。管理者が決めた扱い。

| 持ち越し | 担当 | 確かめる内容 |
| --- | --- | --- |
| ①画面の目視（§4） | ユーザー | `/admin`の地図の位置指定・回転ボタン・STATUSタブの注意（大量削除の保留）の見た目、`/blog-admin`の書き出しの結果の文面 |
| ②OSMのタイルの実取得（§2.5） | ユーザーが検証環境の`/etc/default/geoframe`に連絡先入りの`GEOFRAME_TILE_UA`を設定したあと、管理者 | OSMのタイル数枚が`miss`→`hit`で200・`image/png`になり、キャッシュのファイルができる |
| ③Nominatimの実通信と User-Agent の決定 | 連絡先入りの User-Agent をどうするかを決めたあと、管理者 | 地理院が0件のときの Nominatim への問い合わせが成功し、1秒1件・結果のキャッシュが守られる。この受入では地理院が結果を返したので呼ばれていない（送る User-Agent は今は連絡先なしの`GeoFrame/0.2`） |
| ④T3.7 ⑤ 大量削除の保留の実機（§3） | 管理者（Drive の鍵が置かれてから。P2のT2.4の本物のDriveの確認、[p2-acceptance.md §6](p2-acceptance.md)と一緒） | 保留が`/api/admin/status`の`sync.mass_delete_hold`と STATUS タブに出て、`geoframe sync --allow-mass-delete`で解除できる |

### 7.2 そのほか

記録のみ（このタスクでは直していない）:

| 内容 | 扱い |
| --- | --- |
| 比較用Pythonの`app/`がT3.7より前の版 | §5の差2。想定内 |
| 検証環境のバックアップ`/var/tmp/geoframe-p3-backup/` | §8のとおり残してある |

## 8. 検証環境に加えた変更（戻した）と最終状態

| 変更 | 戻し方・結果 |
| --- | --- |
| `/usr/local/bin/geoframe`（`.prev`は直前の世代）と unit を`rust-deploy.sh`で配布し直した | そのまま（HEAD の版＝`83b7c5f`。unit の`ReadOnlyPaths`は`photos`だけ） |
| 実行時のdrop-in（Rustの`GEOFRAME_INGEST=1`、Pythonの周期） | **削除済み**（`/run/systemd/system/`に残り0、`daemon-reload`済み）。Rustは`ingest=0`で再起動 |
| 両方の`catalog.db`（手動の位置・推定・除外・お気に入り・回転が変わった） | **バックアップから戻した**。`.dump`のSHA-256が両方`57f6dceff3b48bc5…`（試験前と**一致**） |
| `blog_exports/`・`travel_blog/`（保存・書き出し） | バックアップから戻した。`diff -r`で差0（両方） |
| `derivatives/`（回転で作り直された1ファイル。名前は同じで実体が違う） | 名前とinodeをバックアップのハードリンクと一致させた。差0（両方。6,535ファイル） |
| Rustの`tile_cache/`・新しくできた`.cache.json`、Pythonの`tile_cache/`と変わった`.cache.json` | 前者は削除、Pythonの`.cache.json`はバックアップから戻した。DATA_DIR直下の名前の一覧は試験前と一致 |
| `/tmp`に作った補助スクリプト | 削除済み |
| バックアップ`/var/tmp/geoframe-p3-backup/` | **残してある**（受入の記録を確かめる間。不要になったら削除してよい） |

最終状態:

| 項目 | 結果 |
| --- | --- |
| `geoframe-rs` | active、起動ログは`取り込みは無効です（DB に書きません）`、環境に`GEOFRAME_INGEST`なし |
| `GET /healthz` | 200 |
| `GET /api/v1/photos` | 3,065件（試験前と同じ） |
| 比較用Python（`:8090`） | active、`/api/v1/photos` 3,065件（`/healthz`は Python に無く403） |

## 9. 結論

**P3の受入は、APIによる確認としては通過。画面の目視と一部の実通信は持ち越し**（§7.1。期限はP5の本番切替の前）。[plan.md](plan.md) P3の、検証環境で確かめられる部分は満たした。

- 管理画面の更新操作（手動の位置・お気に入り・除外・回転・旅行ブログの保存と書き出し）を取り込みを有効にしたRustで行い、変更がDBとファイルに残る。同じ要求を比較用Pythonにも送ると、`photos`の全行・下書きのバイト列・書き出しの3ファイルが一致する。差は T3.7 の決定（使えない写真を飛ばす）と、既知の差（画素・小数の`delta`・JSONの空白）だけ。
- `GEOFRAME_INGEST=0`では書かない（503、DBもファイルも不変）が、地名検索・タイル・旅行ブログの読み取りは動く。
- 全件比較の差は想定内の2つだけ、miharashiの定常・停止と再開は終了コード0。
- 持ち越し: 画面の目視（ユーザー）、OSMのタイル（`GEOFRAME_TILE_UA`の設定後に管理者）、Nominatimの実通信とUser-Agentの決定（決めたあとに管理者）、T3.7 ⑤の実機確認（Driveの鍵が置かれてから管理者）。
