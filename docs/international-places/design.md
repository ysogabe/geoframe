# 国・地域別の場所表示 設計

作成日: 2026-11-04。状態: 承認済み。段①（境界データと分類段）実装済み。段②（管理API）は `places` の集計まで実装済み（`photos` の絞り込み・検索・旅・`location_region` は未）。段③（画面）は未実装。[PRD](prd.md)に対応。

## 概要

`pref` が空の写真の座標を、同梱の国・第1行政区画ポリゴンと照合し、独立した `photo_regions` 表へ分類を保存する。
日本で `pref` が入っている写真は、今までどおり地理院の `pref/city` を正とし、境界データで分類し直さない。
読み取りでは座標と分類版の一致を必ず確認する。
画面は国→地域、日本ではさらに市区町村という階層にする。既存データも同期段で順次分類する。

## 今の作り

調査時点の主な位置。行番号は後の変更で動くので関数名も併記する。

|場所|現在の役割・制約|
|---|---|
|`crates/geoframe/src/schema.rs:44`|`photos`に座標と日本向け地名。移行は書き込み起動時のみ|
|`crates/geoframe/src/schema.rs:598`|`fresh_database_matches_the_python_one`。`SCHEMA` 定数は Python の fixture と比べるので新しい表を足さない|
|`crates/geoframe/src/curation.rs:9`・`:32-58`|`SCHEMA` に足さない派生表の型（`available`・`pending`・`ensure_tables`）。今回もこれに合わせる|
|`crates/geoframe/src/geo/enrich.rs:513`|`place IS NULL`だけが対象。国外は空文字で処理済みになる|
|`crates/geoframe/src/geo/enrich.rs:975`|分類計画時の座標との一致を条件に適用する例|
|`crates/geoframe/src/geo/infer.rs:19`|座標再推定時に地名列を残す（意図した範囲外。server.md §15.2 の 5）|
|`crates/geoframe/src/local.rs:449-455`・`drive/index.rs:550-556`|再索引で座標が変わると `place/elev` を NULL にし、enrich が `pref/city` を書き直す。GPS が消えた場合は enrich の対象外（`enrich.rs:511` の `USABLE`）なので `pref/city` が残る|
|`crates/geoframe/src/admin_library.rs:275-290`・`web/static/js/library.js:812`|旧形式の絞り込み（`pref`・`pref&city`・`city` だけ・`abroad`）と、それを送る今の画面|
|`crates/geoframe/src/update.rs:91`|手動位置更新は地名を無効化する|
|`crates/geoframe/src/sync.rs:51`・`:335`|`STAGE_NAMES`（いまは 7 段）と同期段を組み立てる入口|
|`crates/geoframe/src/writer.rs`|書き込みの直列化窓口|
|`crates/geoframe/src/admin_library.rs:10`|モジュール説明「どれも `photos` 表だけを読む」。今回変わるので更新する|
|`crates/geoframe/src/admin_library.rs:48`|`Q_WORD_PARAMS`（`q` の 1 語あたりの検索列の数）|
|`crates/geoframe/src/admin_library.rs:68`|`ABROAD`は国ではなくpref空欄の判定|
|`crates/geoframe/src/admin_library.rs:854`|`build_places`はpref/cityで件数・表紙・bboxを集計|
|`crates/geoframe/src/admin_library.rs:653`|地図の点は期間・除外で絞る。場所選択後も全体の点を残す|
|`web/static/js/library.js:13`|選択状態・応答世代・地図移動の理由を管理|
|`web/static/js/library.js:514`|都道府県カード・市区町村チップ|
|`web/static/js/library.js:994`|地図の周回と日付変更線をまたぐbboxは扱わない|
|`web/static/js/photos.js:36`|共有ビューア向けの写真正規化|
|`docs/architecture/photo-feed-contract.md:94`|miharashi は `pref` 空の写真の国名を同梱の Natural Earth から引く|
|miharashi `crates/miharashi-render/src/geo.rs:135`|依存なしの `country_at`（NE の国境で点の内外判定）。日本語名の言い換え表 `NAME_OVERRIDES` を持つ|

既存の[ライブラリ設計](../admin-library-browse/design.md)の期間・bbox・除外・古い応答の破棄を維持する。

## 方針と代替案

|検討事項|採用案|理由|
|---|---|---|
|日本の扱い|`pref` が空でなければ「日本・その都道府県」。境界データは `pref` 空の写真だけ|NE の 10m 海岸線から外れる港・浜・小島、県境の食い違いで今の表示より悪くしない|
|保存|写真の派生表（`curation.rs` と同じ型）|元座標・日本向け地名・`place IS NULL`の意味を変えず、版と入力を管理できる|
|境界|Natural Earth 1:10m Admin0/Admin1（版は 5.1.1 に確定。段①の T1）|国と地域の重なりを統一でき、配布・利用条件も単純。miharashi と同じ出典|
|処理|独立した同期段|取り込み元ごとの実装漏れを避け、既存写真にも同じ処理を使える|
|既存分類の採用|現在の同梱分類版だけ|複数版を併読する切替機構を追加せず、再分類中は未判定として明示する|
|近隣推定|初回は入れない|規則・列・表示が増える。初回の未判定件数を見てから別の目的で検討する|
|起動・配布|正規化・圧縮データを同梱し、索引は分類段が初めて動くときに作る|実行時と通常ビルドにダウンロードやPythonを要求せず、読み取り専用起動に負担をかけない|

`photos`へ列を追加する案は、地名列と再分類状態・入力座標・版が混在するので採らない。
公開ジオコーダの常用は、位置送信とレート制限が生じるので採らない。
日本も NE で分類し直す案は、上の理由で採らない。geoBoundariesの国別詳細データは将来の差し替え候補。
Natural Earthも測量用の精度や最新の行政区画を保証しないため、画面は撮影位置からの分類であることを示す。

## モジュールとデータの流れ

追加する `regions` モジュールを、データ読込・判定・表（`available`/`pending`/`ensure_tables`）・読み取りヘルパーに分ける。

1. 分類段は、候補が 1 件以上あるサイクルでだけ同梱データを検証・展開し、不変の `Arc<RegionIndex>` を作る。
   サイクルの終わりに手放し、常駐させない（初回移行の後はほぼ毎回 0 件なので、展開の費用より常駐メモリを避ける）。
   読み取り API は、ポリゴンとは別に埋め込む小さな ID→名称表（国・地域の名前と検索別名）と `classifier_version` の定数だけを使う。
   実行時に全データをハッシュしない。
2. `index_local → index_drive → infer → enrich → regions → ...` の `regions` が現在の写真を読む。
3. `pref` が空（NULL または空文字）で座標が有効（enrich の `USABLE` と同じ型検査＋判定手順 1 の範囲検査を SQL で掛ける）で、
   未処理・座標変更・分類版不一致の行を抽出し、同一座標をまとめて判定する。無効な座標の行は候補に入れない。
4. writer経由で短いバッチを適用する。現座標を再確認する。写真が無くなった分類行もここで消す。
5. 管理APIは下の「場所の区分の規則」を共通の SQL 片 `regions::PlaceRules`（`regions/rules.rs`）にまとめ、集計・絞り込み・検索・表示で使う。
   `from`（`photos p` に都道府県の対応表 `pm` と有効な分類 `r` を結合）、`country`・`region`（ID の式）、`has_pref`・`no_location`・`unclassified`（真偽の式）、`search`（検索用文字列の式）を持つ。
   表の無い DB では `r` を同じ列の空の派生表にして同じ式を使う（表は作らない）。

### 場所の区分の規則

写真 1 枚は、上から順に最初に当てはまった 1 つの区分にだけ入る。

1. `pref` が空でない → 国=日本。`pref` が対応表の 47 件にあればその地域、無い名前（表記揺れなど）なら日本の `region_unknown`。
   座標の有無は見ない（今の `prefs` と同じく、GPS が消えて `pref` だけ残った写真もここに入る）。
2. `pref` が空で座標が無効 → `no_location`。
3. `pref` が空で有効な分類があり、国が `inside` → その国。地域が `inside` ならその地域、それ以外は `region_unknown`。
   日本の地域は対応表で都道府県へ寄せる。
4. それ以外（国が `outside`／`ambiguous`、有効な分類が無い＝処理待ち・判定できなかった `failed` の座標）→ `unclassified`。

集計の式: 国ごとの件数 = Σ地域 + `region_unknown`。Σ国 + `unclassified` + `no_location` = 全体（期間・bbox・除外を掛けた後）。

分類を `enrich` の結果待ちにはしない。地理院の障害で `enrich` が失敗しても、他の段と同じく独立して実行する。
`place IS NULL`（enrich 待ち）で `pref` も空の日本の写真は、一時的に境界データの都道府県に入り、enrich が `pref` を書くと地理院の結果へ移る。

## 境界データと分類器

### 配布と更新

- `crates/geoframe/data/regions/`に正規化・圧縮したデータ（`polygons.bin.gz`）、ID→名称表（`names.json.gz`）、
  `manifest.json`、出典説明（`SOURCES.md`）を置く。ポリゴンの形式は `crates/geoframe/src/regions/data.rs` の冒頭に定義する。
  座標は倍精度のビット列の差分＋可変長整数＋gzip で、丸めも簡略化もしない（元の shapefile とビット単位で一致する）。
  採用した版は Natural Earth 5.1.1（GitHub の `v5.1.2` タグも同じデータ）。取得元は公式 zip ではなく固定コミットの URL。
  サイズは、ポリゴン 約13.5MB・名称表 約0.15MB（合計 約13.7MB。頂点 1,839,483。展開後 約23MB）で、AC10 の目標 15MB に収まる。
- manifestは元データURL・版・SHA256・変換器版・正規化データのSHA256・件数・サイズ上限を含む。
- `tools/regions/`に明示的な更新コマンドと固定した開発用依存を置く。メンテナンス時だけ公開データを取得する。
  通常のCargoビルドでは取得・生成せず、コミットされた成果物を検証して埋め込む。
- shapefile変換は既存ライブラリを使う。独自のshapefileパーサは作らない。Python依存はデータ更新時だけとする。
  miharashi の `tools/world_geojson_to_txt.py` を参考にする。
- ポリゴンの穴と島を保持する。サイズが PRD AC10 の目標値を超える場合だけ、変換時の簡略化を検討しユーザーに諮る。
- NE の元データは日付変更線で既に分割されている（フィジー、チュクチなど）。変換時に、経度 ±180 をまたぐ辺が無いことを検査する。
- 不正形状、有限でない座標、識別子重複、国参照の不整合を変換時に拒否する。実行時にも形式版と展開サイズを確認する。
- 内部IDは `ne:<ADM0_A3>` と `ne:<adm1_code>` を基本にし、欠損・衝突は明示的な対応表で解決する。
  `adm1_code` が ID の文字種（ASCII 英数字と `:_-`）に合わない 33 件（`GAZ+00?` など）は `ne:GAZ-X00` のように置き換える。
  日本視点版の Admin0 に無い国（`SOL`・`CYN`・`KAS`・`KAB`）を指す Admin1 は、付け替え先の国（`SOM`・`CYP`・`IND`・`KAZ`）に属させる。
  名称が全く無い Admin1 は 7 件あり（`+99?` の穴埋め）、地域として見せられないので含めない（その点は国の『地域不明』になる）。
  ISOコードや名称を主キーにしない。ISOは得られる場合の補助属性とする。
- 国と国に準じる地域の表示区分は同じ世界データの区分で統一する。ソブリン国へ機械的に統合しない。
- 国（Admin0）は日本視点版 `ne_10m_admin_0_countries_jpn`、地域（Admin1）は既定版を使う。両者の食い違いで
  国の中に Admin1 が当たらない点（北方領土など）は、判定手順 3 の「0地域」として当該国の `region_unknown` に入る。
  Admin1 の国参照は国の ID（`ADM0_A3`）で照合し、変換時に食い違う地域の一覧を出力して確かめる
  （各片の代表点を日本視点版の国に当てる。`tools/regions/crossref-report.tsv`。別の国に入る片があるのは 6 件、海岸線の違いだけが 14 件）。
- 日本の Admin1 は 47 都道府県の `pref` 名への対応表を持ち、変換時に 47 件すべてが対応することを検査する。
  `pref` 空の写真が境界データで日本の地域に入ったとき、この対応で同じ都道府県カードにまとめる。
- 国の日本語名は miharashi の `NAME_OVERRIDES` と揃える（PRD R13）。地域の日本語名は日本語→英語→原表記の順に補完する。
  検索用には利用可能な各表記を保持する。海外地域の読み仮名自動生成は行わない。

点の内外判定は miharashi と同じく依存なしの自作（偶奇判定＋包絡矩形での絞り込み）で足りた（段①の T2。新しい crate は無し）。
実装は `regions/data.rs`（形式・復号。座標は 1 本の平らな配列とオフセット表 `Layer`。gzip を展開しながら小さな窓で復号し、
展開後の全体を持たない）、`regions/index.rs`（`RegionIndex`・1 度の格子・判定手順）、`regions/search.rs`
（`classifier_version`・`search_text`）に分ける。`geo` / `rstar` は使わない。
境界上の判定は座標の厳密な一致（外積がちょうど 0）で、頂点・軸に平行な辺・倍精度で厳密に載る辺の上が境界上になる。
gzip展開は既に依存にあるflate2を使う。実行時のネイティブGIS依存は追加しない。

### 判定手順

1. 緯度・経度は有限の数で、緯度±90・経度±180以内のみ受ける。無効座標にポリゴン判定しない。
2. 世界の国ポリゴンで、点が内部または境界上にある国候補を求める。同一国の複数片は1候補へまとめる。
   0 国なら国は `outside`。
3. 1国なら、その国のADM1だけ検索する。1地域なら `inside`、0地域なら国のみ判定（地域 `outside`）、複数なら地域は `ambiguous`。
4. 複数国なら国から `ambiguous`。配列の先頭や名前順で決めない。国境上の共有境界もこの扱い。

## 保存データと有効性

追加表 `photo_regions` の論理項目:

|項目|用途|
|---|---|
|photo_id（主キー）|photosとの1対1の関連|
|input_lat / input_lon|分類に使った座標。SQLiteの同値比較用に元の数値を保持|
|classifier_version|境界データ・分類アルゴリズム・名称表・`search_text` の正規化手順を含む固定ハッシュ（名前を直しても版が変わる）|
|country_id / region_id|地域マスタに対応する内部ID。未判定ならNULL|
|search_text|分類版に含まれる国・地域の日本語/英語/原表記を検索用に展開した文字列|
|country_method / region_method|`inside / outside / ambiguous`。判定できなかった座標の印は `failed`（国・地域・`search_text` は空。下の「有効な分類」）|
|classified_at|処理した時刻|

表には写真名・鍵・画像・応答本文を保存しない。表示名は同梱のID→名称表から解決する。
検索用の表記だけは `search_text` に展開し、既存の文字検索のSQLから同じ有効分類JOINを通して参照する。
これによりSQLからメモリ上の表を引く独自関数や、語ごとの大量ID展開を避ける。`Q_WORD_PARAMS` は列の追加に合わせて増やす。
`pref` のある写真（分類行が無い）には、ビルド時に埋め込む「日本＋47 都道府県の別名（日本語・英語・原表記）」の
固定の `VALUES` 表を `pref` で JOIN し、同じ検索列として扱う。検索列は `CASE WHEN pref<>'' THEN 固定表.text ELSE r.search_text END`。
固定表は利用者の入力を含まない定数なので、SQL に埋め込んでよい。
分類結果に国IDがない場合、地域IDもNULL。地域IDと国IDの所属関係をwriter適用前に検証する。

有効な分類は、現写真と座標が一致し、`classifier_version`が現在の同梱版に一致する行だけ。
分類は入力座標だけに依存するので、写真の世代（md5/mtime）は照合に入れない（再索引で座標が同じなら再分類しない）。
この条件を `regions` の SQL 片に集約し、一覧・検索・件数・表紙・旅で個別に再実装しない。
座標にNULL・文字列・無限値を含む写真は有効な分類としてJOINしない。

判定そのものができない座標（国と地域の所属が名称表と合わない、同梱データの不整合）は、書かずに捨てると毎回候補へ戻る。
候補は ID 順に選ぶので、先頭の 1 万枚が恒久的に失敗すると後ろへ進めず、少数でも毎周期索引を作り続ける。
そこで失敗も `country_method`/`region_method` = `failed` の行（入力座標・分類版つき）として保存し、同じ座標・同じ版では選び直さない。
座標が動くか分類版が上がれば選び直す。**`failed` の行は有効な分類ではない**: `regions::join` は `country_method<>'failed'` を条件に含むので
`failed` の行には結合せず、読み取り側（段②）には「分類行が無い写真」と同じに見える。つまり区分の規則では国を持たない写真として
`unclassified` に入る（`failed` を別の区分や別の分岐にしない。T4 は `r.photo_id IS NULL` と `r.country_id` の扱いだけを書けばよい）。
`classification_pending`（`pending_photos`）は処理済みの `failed` を数えない（待っていれば直る状態ではないため）。

適用は、同一のwriterトランザクションで現在写真を照合し、存在・座標の条件を満たす場合だけUPSERTする。
削除・位置修正で条件が変わった場合は捨て、次回の候補選択に任せる。
同じ写真・同じ座標へ戻った場合に同じ結果を再利用することは問題ない。
写真が無くなった分類行は、`regions` 段の各サイクルで `photo_id NOT IN photos` を短いバッチで消す（読み取りでは JOIN で既に除かれる）。

`infer` が座標を変えたときに `pref/city` が残る制約（`infer.rs:19`）は、今回は直さない（PRD「やらないこと」）。
日本は `pref` を正とするので、この制約の影響は今と同じに留まる。直すときは地理院への問い合わせが増えることも含めて別の目的で扱う。

## 同期・初回移行・読み取り専用

- 表は `curation.rs` と同じく `schema::SCHEMA` に足さず、書き込み起動時に `regions::ensure_tables` で作る。
  `photos`全件の座標・地名を初期化しない。
- `regions`段は `STAGE_NAMES` に加える（段の個数の試験、運用タブ・同期結果の表示を合わせて直す）。
  同梱データの準備後、未処理・入力変更・版不一致を優先して選ぶ。
  `outside/ambiguous`も処理済みとして保存し、同じ版と入力では再実行しない。
- 1サイクル最大10,000枚・経過時間5秒、writer適用は最大100枚の短いバッチを初期値とする（`regions::stage` の定数 `PER_CYCLE`・`BUDGET`・`BATCH`。設定項目にはしない）。
  一致する入力をその場でまとめ、待たせる長いトランザクションを作らない。数値は合成大量データで検証し調整する。
- 時間上限・停止はバッチの前に見る（進めるため、1バッチ目は必ず試す）。適用済みの結果を残し、残りは次回の通常同期へ回す。
  **5秒は「バッチの境目で見る目安」**であって、厳密な締め切りではない。1バッチ（100枚）の判定・適用の途中では中断しないので、
  実際の所要は5秒に1バッチ分（数ms〜数十ms）が足されることがある。
- 候補の走査も同じ時間・停止で見る。SQLite の進捗ハンドラ（`PROGRESS_OPS` = 1,000命令ごと）が、停止の合図があるか、
  時間の上限を過ぎたとき走査を中断し、そこまでに集めた候補（ID順の先頭側）で続ける。ただし1バッチ目の分の候補が揃うまでは中断しない
  （候補があるのに走査だけで時間切れになり続け、一度も進めなくなるのを避ける）。中断したときの `pending` は、数えた分までの下限になる
  （固定の文面と件数だけをログに出し、同期結果・運用履歴の件数にも `scan_incomplete=1` を載せて、`pending` が下限だと分かるようにする）。候補は `ORDER BY p.id` の主キー順で、最初の `PER_CYCLE` 枚を集めたあとも総数のために走査を続ける。
  ハンドラは走査のあいだだけ付け、終わると外す（共有の読み取り接続に残さない）。`rusqlite` の `hooks` feature を足す（依存は増えない）。
  合成データでの計測（release、`photos` に索引は足さない、`pref` 空・座標有効の写真）: 20万枚で全件が候補（初回移行）27ms、
  15万枚が分類済みで5万枚が候補 47ms、全件が分類済み（定常状態）48ms、100万枚で全件が分類済み 245ms。候補走査を含む1サイクル
  （索引は合成）も最大で20万枚 108ms、100万枚 376ms で、5秒の目安には遠い。進捗ハンドラは異常に遅い記憶装置への備えである。
  `pending, inside, unresolved, stale_input, removed, failed, deferred, scan_incomplete`を件数だけ返し、現行の同期結果・運用履歴へ載せる
  （`pending` はサイクル開始時の候補の総数、`inside` は国が決まって保存した枚数、`unresolved` は `outside`/`ambiguous` で保存した枚数、
  `stale_input` は適用の照合で捨てた枚数、`removed` は掃除した行、`failed` は判定できず、`failed` の印だけを保存した枚数（印は次の版まで選び直さない）、`deferred` は今回処理しなかった残り、`scan_incomplete` は候補の走査を時間の上限か停止で打ち切ったら 1・そうでなければ 0。1 のとき `pending` と `deferred` は下限で、実際の残りはもっと多い）。
  dry-run は `stale_input` を出さない（`scan_incomplete` は出す）。
- 段の順は 候補の選択 → 判定と適用（索引はここだけ） → 掃除。索引を手放してから掃除するので、掃除の削除が索引の寿命に重ならない。
  掃除も1回100行の短いバッチで、1サイクル最大 `PER_CYCLE` 行まで（最後の1回は残りの数だけ消す）。
  dry-run の `removed` も同じ `PER_CYCLE` で数える（孤児が上限より多くても、実行が消す数を超えない）。
- 適用の `INSERT … SELECT … FROM photos WHERE id=? AND lat=? AND lon=? AND (pref が空) AND (座標が有効) ON CONFLICT DO UPDATE` が照合を兼ねる。
  書いた行数が0なら `stale_input`。適用が失敗した（DBのエラー）ときは、確定済みのバッチを残して段を失敗にし、残りは次回の候補に戻る。
- 表が無い DB で書く実行（Apply）は、黙って表を作らず段を失敗にする（表は書き込みの起動時の `ensure_tables` が作る）。
- 新しい専用ワーカー、強制再分類HTTPエンドポイント、専用CLIは初回には作らない。
  未処理は定期同期または既存「今すぐ同期」で進める。データ版更新も同じ経路を使う。
- dry-runは、表があれば今の行と比べて候補を数え、オフライン分類・集計まで実行する。
  表が無ければ、メモリ上の写しにも表は作らず、`pref` が空で座標が有効な全件を処理待ちとして数え、それを分類した件数を返す。
  実DB・永続キャッシュ・画像は更新しない。他の同期段の通信有無は既存syncの仕様どおりである。
- 分類表の存在検査（`regions::available`）は読み取り接続で行い、旧DBではJOINを行わない分岐にする。
  読み取り側（段②）は有効な分類を `regions::join(p, r)`（`LEFT JOIN photo_regions r ON …`。座標の一致・今の分類版・座標の有効性を含む）で引く。
  `regions::valid_coordinates(p)` は座標の有効性（数値型・範囲）だけの条件。
  `regions::pending_photos(conn)` は分類待ちの写真数（`classification_pending` 用。段の `pending` と同じ条件。表が無ければ `pref` 空で座標が有効な全件）。
  APIから移行・再分類を実行しない。
- 表ありだが再分類中は、無効な旧版の分類を表示せず「地域未判定」へ含め、件数として処理待ちを表示する。
  旧版と新版の並行保持や全件処理完了時の原子的な版切替は初回に実装しない。
- 表なし旧DBの読み取り専用モードは、従来のpref/cityを日本として表示し、残りを旧未分類として表示する。
  `classification_available:false`を返し、再分類待ちとの違いを示す。書き込み有効化後の初回起動で新方式へ移る。

## 管理API

既存の管理APIへ追加する。公開 `/api/v1/photos` の既存フィールド・JSON契約は変更しない。
管理画面の分類情報をそのまま公開フィードへ流すことも行わない。

### 共通の分類表現

`location_region`として、国ID/名前、地域ID/名前、判定方法（`pref`・`inside`など）、表示用のラベルを返す。
`pref` が入った写真は国=日本、地域=その都道府県（地域IDは対応表の ID）、判定方法 `pref` とする。
分類版は管理画面の通常操作に露出させない。API内では有効性を検証済みとする。
座標変更通知で共有ビューアの分類情報を即座に消し、次の読み取りで未判定または新結果へ更新する。

### GET /api/admin/library/places

既存の `prefs` と `abroad` は旧形式として意味を維持し、新画面は次の追加フィールドを読む。

- `countries[]`: ID、表示名、検索別名、件数、表紙、bbox、fit_bbox、`regions[]`。
- `regions[]`: ID、表示名、検索別名、件数、表紙、bbox、fit_bbox。
  日本のみ、pref名と `cities[]`、`boundary_only` も持つ。市区町村は今の `pref/city` の集計をそのまま使う。
  `boundary_only` は境界データで入った `pref` 空の写真（enrich 待ちを含む）の件数で、画面は「市区町村未取得 N」と出す。
  日本の地域の件数 − `boundary_only` が、旧 `prefs` のその都道府県の件数と一致する。
- 各国に `region_unknown`: 当該国で地域が分からない写真の件数と範囲。
- `unknown`: `unclassified`（座標あり国未判定）と `no_location`（座標なし・無効）の件数。
- `classification_available`（表があるか）、`classification_pending`（`regions::pending_photos`。期間・範囲は掛けない）。

実装した応答の形（`places`）:

```json
{"ok": true, "prefs": [...], "abroad": 25,
 "classification_available": true, "classification_pending": 3,
 "countries": [{
   "id": "ne:FRA", "name": "フランス", "aliases": ["フランス", "France", "French Republic"],
   "count": 4, "cover": {"id": 14, "thumb_url": "/thumb/14.jpg?v=…"}, "bbox": [w, s, e, n], "fit_bbox": [w, s, e, n],
   "regions": [{"id": "ne:FRA-…", "name": "…", "aliases": [...], "count": 2, "cover": {...}, "bbox": [...], "fit_bbox": [...]}],
   "region_unknown": {"count": 1, "cover": {...}, "bbox": [...], "fit_bbox": [...]}}],
 "unknown": {"unclassified": {"count": 6, "cover": {...}, "bbox": [...], "fit_bbox": null},
             "no_location": {"count": 4, "cover": {...}, "bbox": null, "fit_bbox": null}}}
```

- 日本の地域だけ `pref`・`cities[]`（`prefs` の `cities` と同じ）・`boundary_only` を持つ。`region_unknown` は常にあり、無ければ `count: 0`・`cover`/`bbox`/`fit_bbox` が `null`。
- `fit_bbox` は、通常の範囲の経度の幅が 180 度以内なら `bbox` と同じ（空きが戻る間が必ず最大のため）。それより広いときだけ、経度を全部使って最小の区間を求め、日付変更線をまたぐなら `null`。
  `pref` のある写真を含む組（日本）は経度をそろえず、幅が 180 度を超えたら確かめずに `null`（日本の `pref` の写真が半周以上に広がるのは座標の誤りのときだけ）。
- 件数の多い順、同数は表示名、それも同じなら ID。国・地域・区分の集計は 1 つの走査（`pref` のある写真と、無い写真の 2 本の `GROUP BY`）から取り、
  旧 `prefs`・`abroad` も同じ結果から作る。代表の写真は、集計と同じ走査で組ごとに `MAX(COVER_KEY)`（`COVER_ORDER` と同じ順位を文字列にした鍵）を取り、
  国・地域は組の鍵の最大から選ぶ（窓関数の並べ替えをしない。`COVER_ORDER` との同値を単体試験で確かめる）。

新しい国別件数＋国未判定＋場所不明の合計を一致させる。旧 `abroad` と新しい国別件数は足し合わせない。
代表写真の選択は既存coversの規則を再利用し、JOINにより1枚が複数回現れないようにする。
期間・bbox・除外は全ての新項目へ先に適用する。候補用の要求には従来どおりbboxを付けない。

### GET /api/admin/library/photos

追加クエリ:

- `country=<内部ID>`: 国全体。
- `country=<内部ID>&region=<内部ID>`: 当該国の地域。日本の地域は `pref` 一致または境界データの対応で選ぶ。
- `country=<内部ID>&region_unknown=1`: 国のみ判定。
- `unclassified=1`: 座標あり・国未判定（競合・区域外・処理待ちを含む）。
- `no_location=1`: 有効な座標がない。

クエリは旧形式と新形式の 2 つに分け、混ぜると400。

- 旧形式: `pref`、`pref&city`、`city` だけ、`abroad`。今の意味のまま受ける（段③の前の画面と、段③を戻したときのため）。
- 新形式: `country`／`region`／`region_unknown`／`unclassified`／`no_location`。相互排他。`region`単独、別国の地域は400。
  新形式で `city` を受けるのは `country=ne:JPN` かつ日本の `region` を指定したときだけ。
  意味は「`pref` = 対応表の都道府県名 AND `city` = 指定値」（同名の市区町村が他県で混ざらない）。
  `city`＋`country` で `region` なし、日本以外の `region`＋`city` は400。
IDは最大64文字、許可文字はASCII英数字と `:_-`。SQLは常にパラメータ化する。
文法上正しい未知IDは0件を返し、画面は選択を外せる。古い選択を別地域へ暗黙変換しない。

文字検索 `q` は国・地域の日本語/英語/原表記も対象にする。JOIN・WHEREの生成を共通化し、
国名と地域名が同じ語に一致しても写真を二重に返さない（試験で確かめる）。語数・文字数上限は既存を維持する。

### trips・points・写真詳細

- tripsは時間での区切りを維持し、地名上位候補に国・地域ID/名称を追加する。旧pref/cityは互換項目として残す。
- 地図のpointsは従来の期間・除外での表示を維持する。地域選択で点の集合を狭めない。
  地図吹き出しは同じ写真取得経路から有効な `location_region` を読む。
- ライブラリのphotos応答と共有ビューアの正規化に任意の `location_region` を追加する。
  フィールドなしの旧API応答でも動く。写真タブ全体の検索仕様変更は行わない。

## 画面

- 左の一覧を国カードにし、選択国の下へ地域カードを展開する。国・地域は件数降順、同数は表示名順。
- 初回、期間・bbox・選択がなければ日本が存在するとき日本の地域一覧を展開し、国内を探す手数を抑える。
  展開だけでは写真や地図を自動選択しない。他国を押すとその国の写真と地域一覧へ切り替える。
- 地域を選ぶと写真と地図を切り替える。日本の市区町村チップはその下へ展開する。
- `地域不明`は各国に、`地域未判定`と`場所不明`は国一覧に置く。数が0なら出さない。
- 写真詳細には国・地域を出し、境界データによる分類のときは「撮影位置からの分類」と分かる短い注記を付ける。
- 場所コンボボックスは国→地域→日本の市区町村の順で候補を作る。同名地域には国名を付ける。
  NFKC・大小文字など既存の文字正規化とキーボード操作を維持する。
- 状態は国ID/地域IDを持つ選択に拡張し、名前を状態のキーとして使わない。
  既存の `gen/seq` と地図移動理由の扱いを全ての新選択へ適用する。

## 日付変更線と地図範囲

分類用の形状は日付変更線で分割済みのものを判定する。APIの検索用bboxは既存の通常bboxのまま。
カードの `bbox` は既存互換の通常範囲、`fit_bbox`は写真座標の円周上の最小経度区間を別に計算する。
最小区間が日付変更線にまたがる場合、初回実装では `fit_bbox:null` とし自動fitを省略する。
写真は国・地域IDで正しく絞り、地図は現在位置を保ち「この地域は地図を移動して確認できます」と短く示す。
不正bboxや世界全体へのズームに置き換えない。検索用bboxの仕様拡張は別目的の変更とする。

## 安全面

- 地域判定は同梱データと写真座標だけ。実行時の外部送信・鍵・新規ホスト設定はない。
- データ更新時は固定した公開出典から明示的に取得し、SHA256を検証する。通常のビルドで任意URLを読まない。
- 管理APIは現行のHost/Origin/入力上限を維持する。新しい書き込みエンドポイントを追加しない。
- 名前はDOMのtextContentで描画する。検索語・IDをSQLやHTMLへ直接連結しない。
- 分類結果のログは件数・固定理由だけ。写真座標・名前・写真IDをログに出さない（PRD AC12）。
- スキーマ・分類の書き込みは取り込み有効時だけwriter経由。読み取り専用動作を壊さない。

## テスト計画

|受け入れ条件|検証|
|---|---|
|AC1,AC3|API結合試験。複数国・同名地域・表紙・期間/bbox/除外・合計一致・ページング|
|AC2|変更前後の `places` の日本部分の比較（海岸・小島・県境・GPS が消えて `pref` だけ残った合成写真を含む。地域の件数 − `boundary_only` と `cities[]`）、市区町村・読み仮名・選択解除。旧形式のクエリが変更前と同じ結果を返す|
|AC3|区分の規則 1〜4 の各分岐と、集計の式（国 = Σ地域 + `region_unknown`、Σ国 + `unclassified` + `no_location` = 全体）|
|AC4|日本語/英語/原表記の検索、`pref` のある写真が「日本」「Japan」「Tokyo」で当たる、国名と地域名の両方に一致する語で二重にならない、旅の地名・文字検索|
|AC5,AC6|合成ポリゴンの内部・境界・穴・島・国境の共有辺・地域の共有辺・国のみ・海上・無効座標（判定手順 1〜4 の各分岐）|
|AC7|計画とwriter適用の間の座標変更・削除・データ版変更。読み取りでも旧結果を排除。削除後の残り行の掃除|
|AC8|途中停止・バッチ境界・件数/時間上限（候補の走査を含む）・版更新・未解決行と判定失敗行（`failed`）の再選択抑制と、先頭の恒久的な失敗が後ろを塞がないこと・dry-run（掃除の件数の上限を含む）・無通信・`STAGE_NAMES` の個数|
|AC9|分類表なしの旧DB、取り込み無効、分類表ありの未処理/旧版の混在|
|AC10|manifest検査、データ変換の再現性、muslビルド、公開フィード契約、バイナリ増加・RSS・索引作成時間の計測、読み取り専用起動で索引を作らない|
|AC11|既存fake DOM/MapLibre試験へ新選択・古い応答・位置更新・日付変更線のfit省略を追加|
|AC12|分類段のログ出力に座標・ID・名前が含まれないことを試験で確かめる|

公開の代表点による境界データのスモーク試験と、合成ポリゴンによる規則の単体試験を分ける。
私的な写真座標・行動履歴・本番DBの抜粋をfixtureにしない。
大量データでは1万件程度の合成写真で処理時間・writer待ち・照合回数を測り、バッチ上限を調整する。
地図を動かすたびに来る `places` の応答時間を、`photo_regions(country_id, region_id)` の索引あり・1万件の合成データで測る。
目標は変更前の `places` の応答時間の 1.5 倍以内（段②で変更前を先に測る）。
実測（`tests/library_places_perf.rs`、release、1 万件、変更前 → 変更後の中央値）は段②の PR の説明を参照。計測は `cargo test --release --test library_places_perf -- --ignored --nocapture`。

必要な確認: `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、
画面の DOM 試験は上限付きの `cargo test --test admin_ui_library`（共有ビューアは `admin_ui_photos`）経由で回す
（`node` を直接実行しない。メモリ事故の再発防止）。muslビルド、`python3 tools/check_markdown_links.py`。

## リスクと対策

- 境界の更新年・沿岸精度: 出典と版を固定する。海岸の取りこぼしは「地域未判定」で見せ、件数を見て近隣推定を検討する。
- 初回処理中に未判定が増える: 処理待ち件数を出し、短いバッチで通常同期を継続する。旧結果を正しいように表示しない。
- 実行ファイル・メモリ増加: 正規化圧縮し、索引は分類段で遅延して一度だけ作る。段①でサイズと RSS を測り、PRD AC10 の目標値と比べる。
  100MB級の国別詳細JSONをそのまま同梱しない。
  T2 の計測（release。座標 1,839,483 点 = 28.1MB と格子の表 1.4MB）: 索引作成後の RSS 増加は glibc で +43MB（うち確保したメモリ 30MB、
  残りは埋め込みデータを読んだ再読込できるページ）、musl + mimalloc で +51MB（確保 38MB）、索引作成 80〜130ms、
  1 点の判定 約 10µs（最悪 0.06〜0.26ms）、リリースバイナリ（musl）の増加 +13.9MB（実際に使うときの値）。mimalloc は手放した後の返却が
  次の確保まで遅れる。測り方は `regions/measure_tests.rs` の冒頭。
- 分類データ不正: データ生成と起動検査で失敗を検出。破損した分類を提供せず旧分類画面へ縮退し、状態に分類利用不可を出す。
- 日本の表示の後退: `pref` を正とすることで避ける。AC2 で変更前後の一致を確かめる。
- 帰属に争いのある地域: `pref` 空の写真だけが対象。国は日本視点版の国境で判定する（PRD「決まったこと」）。
- Admin1 の細かさの国差: 初回はそのまま使い、使ってみて細かすぎる国を `region` 列でまとめるかを決める。
- 管理画面と miharashi の国名のずれ: 日本語名の言い換え表を揃える。
- 表示名変更・ID変更: 名前をキーにせず版を管理。更新後の未知選択は空として解除できるようにする。

## 実装の順番

3 段（サブ Issue）に分け、段ごとに 1 本の PR にする。各段は単独でマージでき、単独で戻せる。

1. 段①（PR 1）: 境界データと分類段。画面・API の変更なし。本書と PRD もこの PR に入れる。
   データ変換・manifest・同梱・分類器と合成試験 → `photo_regions` 表（`ensure_tables`）・同期段・CAS・残り行の掃除。
   公開地点で基礎精度、サイズ・RSS・索引作成時間を計測する。dry-run の件数と運用タブの段の表示で確かめる。
   運用タブの段の表は汎用の作りなので、画面の変更は件数表示の確認だけ。戻すときは `photo_regions` 表が残っても無害（読む側が無い）。
2. 段②（PR 2）: 管理API。国/地域集計・絞り込み・検索（`q`）・旅・写真情報・旧DB分岐。旧形式のクエリと公開フィード互換を確認する。
3. 段③（PR 3）: 画面。国・地域カード、候補検索、国内市区町村、未分類表示、地図fit、写真詳細の注記。

段②は段①の表に、段③は段②の API に依存する。近隣推定と `infer` の地名の修正は、それぞれ別の目的として切り離す。

## 参照

- [Natural Earth Admin1](https://www.naturalearthdata.com/downloads/10m-cultural-vectors/10m-admin-1-states-provinces/)
- [Natural Earth利用条件](https://www.naturalearthdata.com/about/terms-of-use/)
- [rstar RTree](https://docs.rs/rstar/latest/rstar/struct.RTree.html)
- [ライブラリの既存要求](../admin-library-browse/prd.md)
- [公開写真フィードの契約](../architecture/photo-feed-contract.md)
- miharashi の `crates/miharashi-render/src/geo.rs`（`country_at`・`NAME_OVERRIDES`）と `tools/world_geojson_to_txt.py`
