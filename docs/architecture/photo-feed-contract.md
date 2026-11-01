# 写真フィードの契約（表示クライアント向け）

GEOFRAMEが表示クライアントへ渡す写真一覧と画像の取得方法を定めます。現在の表示
クライアントは[miharashi](https://github.com/ysogabe/miharashi)（DRM/KMSへ直接描画する
キオスク）だけで、GEOFRAMEとの接点はこの文書の範囲に限ります。

応答の形は[JSON Schema](photo-feed.schema.json)に定めています。この契約は次のテストで検査します。
変更するときは、Schema・各テスト・表示クライアント側の読み取りを同時に直してください。

- Pythonで作った基準: フィクスチャ（`crates/geoframe/tests/fixtures/photo-feed-python.json`）と、同じ合成DBのSQL
  （`photo-feed-catalog.sql`。Pythonの本物のスキーマと行）は、旧Python版（P5aのマージのコミット`a804623`まで存在）に
  合成データだけを流して作りました。Python側の検査（`app/tests/test_photo_feed_contract.py`）と生成の道具
  （`tools/gen_photo_feed_fixture.py`）はP5bで削除したので、再生成はできません。フィクスチャは固定の基準として、
  下のRustのテストが読みます。
- Rust: `crates/geoframe/tests/photo_feed_contract.rs`が、型（`geoframe::feed::Photo`）の出力と上のフィクスチャを
  同じSchemaで検査し、フィクスチャを型で読み書きし直しても内容が変わらないことを確かめます。
- miharashiの読み方: `crates/geoframe/tests/photo_feed_miharashi.rs`が、miharashiが読むフィールドと型を再現して
  フィクスチャと型の出力、およびRustのエンドポイントの実出力が読めることを確かめます（miharashiのcrateには依存しません）。
- Rustのエンドポイント: `crates/geoframe/tests/photo_feed_endpoint.rs`が、フィクスチャと同じ合成DB（上のSQL）に対する
  `GET /api/v1/photos`の出力が、フィクスチャ（旧Python版の出力）と`serde_json::Value`として完全に一致することと、
  除外・並び・旅番号・タイトル・ヘッダ・503を確かめます。

## 写真一覧

```
GET /api/v1/photos
```

応答は写真オブジェクトのJSON配列です。表示クライアントが頼ってよいのは次のフィールドだけです。
ほかのフィールド（品質解析やAIの内部値など）は予告なく変わります。

| フィールド | 型 | 内容 |
|---|---|---|
| `id` | 整数 | 写真のID。一覧の中で一意で、同じ写真なら取り直しても変わらない（表示履歴の鍵に使える） |
| `file` | 文字列 | 表示用画像の相対パス（例: `derivatives/<id>.jpg`）。下の「画像」を参照 |
| `time` | 文字列 | 撮影日時（`YYYY-MM-DD HH:MM`形式のローカル時刻）。不明なら空文字 |
| `lat` / `lon` | 数値 | 撮影地点の緯度・経度（度）。一覧には位置のある写真だけが入るので必ずある |
| `pref` | 文字列 | 都道府県名。住所を引けなかった写真（海外を含む）は空文字 |
| `city` | 文字列 | 市区町村名。住所を引けなかった写真は空文字 |
| `title` | 文字列 | 表示名。手動・AIの設定 → 市区町村＋町名 → ファイル名の順に決まり、空にはならない |
| `fav` | 真偽値 | 利用者がお気に入りにした写真 |
| `fetched_at` | 文字列 | GEOFRAMEへ取り込んだ日時（`YYYY-MM-DDTHH:MM:SS`形式のローカル時刻）。不明なら空文字 |
| `quality_kind` | 文字列 | 品質解析のグループの種類。`exact`（完全に同じ写真）、`similar`（3分以内に撮った見た目のほぼ同じ写真）、グループに入らない・未解析なら空文字 |
| `quality_recommended` | 真偽値 | 品質解析でグループの上位として推奨された写真。管理画面で人が代表に選んだ写真があるグループでは、その写真だけが`true`（解析の推奨は使わない）。グループに入らない・未解析なら`false` |
| `trip` | 整数 | 撮影日時を48時間超の間隔で区切った旅の番号（0から）。**この一覧の中だけで意味を持つ**（写真が増えると番号がずれる） |

`quality_recommended`はグループに入った写真にしか付きません。「良い写真だけ」を選ぶ目的で
これだけを条件にすると、グループに入らない単独の写真がすべて外れます。似た写真を省きたい
ときは「`quality_kind`が`similar`でない、または`quality_recommended`、または`fav`」を条件にしてください。

一覧に含まれるのは、位置情報があり、表示用画像が生成済みで、非表示にされていない写真です。
品質解析で完全な重複と判定されたグループの推奨外と、AI整理で同じ場面と判定されたグループの
推奨外（お気に入りと、管理画面で人が代表に選んだ写真を除く）も除かれます。並びは撮影日時の昇順で、
撮影日時の不明な写真が最後に来ます。表示の順番や選び方はクライアント側で決めてかまいません。

### 表示クライアント（miharashi）の読み方と、この表との差

miharashiは`serde_json::Value`で読み、`file`が文字列でない要素を飛ばします（`similar`の推奨外も表示対象から外します）。ほかのフィールドが
欠けても失敗せず、既定値（文字列は空、数値は無し、真偽値は`false`）で続けます（`miharashi-app/src/photos.rs`の
`list_geoframe`と`album.rs`の`eligible`・`parse_time`）。この表と食い違う点は次のとおりです。契約は表のままで、
miharashiの読み方に合わせて表を緩めてはいません。

- `id`: miharashiは数値も文字列も受け、無ければ`file`で代用します。契約は整数で、必ずあります。
- `lat` / `lon`: miharashiは数値でなければ「位置なし」として扱います。契約は数値で、必ずあります。
- `fav` / `quality_recommended`: miharashiは`true`との一致で見ます。`1`や`"true"`は`false`扱いになるので、
  必ず真偽値で出してください（`quality_recommended`が`1`だと、`similar`の写真が推奨外として落ちます）。
- `time` / `fetched_at`: miharashiは先頭16文字を`YYYY-MM-DD HH:MM`か`YYYY-MM-DDTHH:MM`として読み、
  読めなければ先頭10文字の日付だけ（12:00扱い）で読みます。`fetched_at`の秒は使いません。
- miharashiが読む契約フィールドは`id file time lat lon pref city title fav fetched_at quality_kind
  quality_recommended trip`のすべてです。`quality_kind`と`quality_recommended`は`similar`の絞り込みだけに使います。
  `place`・`elev`・`inferred`と品質解析・AIの内部値は読みません。

### 現行実装との差（Rust移行で決めること）

- `time`: 契約は不明なら空文字ですが、現行のPython実装は`photos.taken_at`が`NULL`の写真で`null`を返します
  （`"time": r["taken_at"]`に`or ""`が無い）。しかも並びは`taken_at=''`だけを最後に回すので、`NULL`の写真は先頭に
  来て旅番号も別になります。インデクサ（`drive.py`・ローカル索引）は`norm_time`を通して常に文字列（読めなければ空文字）で保存するので、通常は起きません（手でDBを書き換えたときなどだけ）。
  Rust実装（T1.2）は意図した差として、`NULL`を空文字（時刻不明）として扱い、契約どおり空文字で出して、並びも
  最後にします（旅番号も時刻不明の写真と同じ。時刻不明の写真は1枚ごとに別の旅になる）。本番のコピーには該当する行が無いことを確認済みです。
- `lon`: `lat`はあって`lon`が`NULL`の行は、現行のPythonでは、同じ旅に他の写真があると`assign_trips`の`min(lon)`が
  `TypeError`になって一覧全体が500、その行だけの旅のときは`lon: null`になります。Rustはこの行を一覧から外します
  （契約では数値。miharashiは数値でない`lon`を「位置なし」として扱うので、`lon: null`と見え方は同じ。`setloc`は`lat`/`lon`を一緒に書くので通常は起きません）。
- 同じ撮影日時の写真の並び: `ORDER BY`は撮影日時だけで、同順位の並びはSQLiteの実行計画に依存します。RustはPythonと同じSQLなので通常は同じですが、保証はしていません（本番のコピーでの全件比較で確かめます）。
- 夏時間: 旅番号の48時間の差を、PythonはローカルのTZで数え、Rustは暦だけで数えます。本番の`TZ=Asia/Tokyo`には夏時間が無く、差は同じです。
- `file`: 契約の例は`derivatives/<id>.jpg`ですが、ローカルのJPEG/PNGは原本をそのまま`photos/...`として指す
  ことがあります。どちらも`/`で始まらない相対パスで、同じ`GET /<file>`で取れます。Schemaは先頭が`/`でない
  ことだけを要求します。

### 表示クライアント（miharashi）の使い方

- 上の条件で表示対象を選ぶ。
- Home Assistantの自宅から`home_radius_km`（既定50km）以上離れた写真を、撮影間隔48時間で「旅」に
  区切る。近い写真は季節ごとの「いつもの暮らし」にまとめる。自宅の位置が取れないときは`trip`を使う。
- `pref`が空の写真は、同梱の国境データ（Natural Earth）から国と近い都市を引いて、旅の名前と写真の説明に使う。
- `fetched_at`は新しく取り込んだ写真を優先する加点に、`id`は表示履歴の保存に使う。

## 画像

`file`の値をベースURLに`/`でつないだURLで、表示用のJPEG（長辺が最大`DERIV_PX`＝2560px）を取得できます。

```
GET /derivatives/<id>.jpg     → 200 image/jpeg
```

## 接続

- 表示クライアントは同じ機体から`http://127.0.0.1:8080`で接続します。
- IPアドレス宛てのアクセスは`Host`検査を通ります。ホスト名で接続する場合は、その名前を
  `GEOFRAME_ALLOWED_HOSTS`へ登録してください（[設定リファレンス](../reference/configuration.md)）。
- 認証はありません。
