# 境界データの更新（`tools/regions/`）

「国・地域別の場所表示」（[PRD](../../docs/international-places/prd.md)・[設計](../../docs/international-places/design.md)）が
使う境界データを、Natural Earth から取得・検査・変換して `crates/geoframe/data/regions/` に置く道具です。

**通常のビルド・実行・`cargo test` では使いません。** `cargo build` は何も取得・生成せず、コミットされた成果物を埋め込むだけです。
境界データを更新するとき（版を上げる、対応表を直す）だけ、ここの手順を明示的に実行します。

## 採用しているデータ

|項目|内容|
|---|---|
|提供元|[Natural Earth](https://www.naturalearthdata.com/)（パブリックドメイン）|
|版|5.1.1（1:10m。2026-11-04 時点で公式の最新。GitHub の `v5.1.2` タグも同じデータで、`VERSION.txt` は 5.1.1）|
|国（Admin 0）|`ne_10m_admin_0_countries_jpn`（日本視点版）|
|第1行政区画（Admin 1）|`ne_10m_admin_1_states_provinces`（既定版）|
|取得元|<https://github.com/nvkelso/natural-earth-vector> のコミット `f1890d9f…`（`v5.1.2`）の `10m_cultural/`|

取得元は、更新で中身が変わりうる公式の zip の URL ではなく、**コミットで固定した GitHub の URL**です。
公式 zip（`naturalearth.s3.amazonaws.com/10m_cultural/<名前>.zip`）の shp・dbf と、固定コミットのファイルは
バイト単位で一致することを確かめています。取得元の URL・サイズ・SHA256 は [`sources.json`](sources.json) に固定し、
取得のたびに検証します（一致しない取得物は捨てます）。

## 手順

```bash
# 初回だけ。システムの Python を汚さないよう venv を使う（Python 依存は pyshp だけ。ハッシュ固定）
python3 -m venv .venv
.venv/bin/pip install --require-hashes -r tools/regions/requirements.txt

# 取得（tools/regions/.cache/<版>/ へ。git に入れない）→ 変換 → 成果物の書き出し
.venv/bin/python -I tools/regions/update_regions.py all

# 個別に
.venv/bin/python -I tools/regions/update_regions.py fetch      # 取得と SHA256 検証だけ
.venv/bin/python -I tools/regions/update_regions.py convert    # 変換して成果物と manifest を書く
.venv/bin/python -I tools/regions/update_regions.py check      # 変換し直して、コミット済みの成果物と一致するか確かめる（書かない）
```

`--source-dir <ディレクトリ>` で取得済みの元データを使えます（検証はします）。`--work` で取得物の置き場、`--out` で出力先を変えられます。

変換が書くもの:

|ファイル|内容|
|---|---|
|`crates/geoframe/data/regions/polygons.bin.gz`|ポリゴン本体（形式は [`data.rs`](../../crates/geoframe/src/regions/data.rs) の冒頭）|
|`crates/geoframe/data/regions/names.json.gz`|ID→名称表（日本語・英語・原表記・別名、日本の都道府県の対応）|
|`crates/geoframe/data/regions/manifest.json`|元データの URL・版・SHA256、変換器の版、成果物の SHA256・件数・サイズ上限|
|`tools/regions/crossref-report.tsv`|Admin0（日本視点版）と Admin1（既定版）で国参照が食い違う地域の一覧|

成果物の出典と利用条件は [`SOURCES.md`](../../crates/geoframe/data/regions/SOURCES.md)。

### 再現性

`check` はまず、コミット済みの成果物そのもの（圧縮後のサイズ・SHA256、展開後のサイズ・SHA256・上限）が manifest の記録と一致するかを
検証します（壊れていればここで止まります）。そのあと変換し直して、展開後の内容の SHA256 を manifest と比べます。
gzip は mtime とファイル名を持たせず、圧縮の水準を固定していますが、出力は zlib の版で変わりうるので、gzip のバイト列が違っても、
**展開後の内容が一致したときだけ**「zlib の版の違い」として許します。再現性の基準は `raw_sha256`（展開後）です。
`convert` も、展開後の内容が同じなら既存の gzip をそのまま残します（形状が変わらないのに blob が履歴に増えないように）。

### 版を上げるとき

1. `sources.json` の `data_version`・`git`（タグとコミット）・`base_url` を新しい版に直す。
2. `update_regions.py fetch --print-hashes` で各ファイルのサイズと SHA256 を表示し、`sources.json` の `layers` に写す
   （この段階では検証が通らないのは正しい。内容を目で確かめてから固定する）。
3. `update_regions.py all` を実行する。拒否されたら下の「変換が拒否するもの」を直す。
4. `crossref-report.tsv` の差分と、`git diff` のサイズ（`manifest.json` の `artifacts`）を確かめ、`cargo test` と
   `python3 -m unittest discover -s tools/tests` を通す。

## 変換が行う検査（拒否するもの）

- 形状: 輪が閉じていない・4 点未満・面積 0、座標が有限でない・経度 ±180／緯度 ±90 の範囲外、
  **経度 ±180 をまたぐ辺**（東西が反対側の点を直接つなぐ辺。両端がちょうど ±180 の南極の縁は図の端に沿うだけなので許す）。
- 多角形ぜんたいの辺どうしの関係（格子で絞って調べるので、頂点が百万単位でも数秒）:
  - 輪の自己交差、穴と外周・穴どうしの辺の交差（辺の内側どうしの X 字の交差）、一直線上で長さを持つ重なり（辺の重複・折り返し・
    外周と辺を共有する穴）は拒否。端点での接触（くびれ・外周に頂点で接する穴）は許すが、接点で輪が互いに突き抜ける横断（頂点を通る X 字。接点のまわりで 2 本の通り道の向きが交互に並ぶ）は拒否。
  - 穴は外側の輪の内側にあること（辺は交差も横断もしないので、頂点と辺の中点が外に出ないことで決まる）、穴どうしが入れ子・重なりでないこと。
  - 向きの判定は厳密（浮動小数点で怪しいときだけ有理数で計算し直す）。
  - 不正の例外表は持たない（2026-11-04 時点で、採用した版の元データに不正形状は 0 件）。将来の版で見つかったときは、勝手に直さず、
    ID と件数を報告して例外の扱いを決める。
- ID: 文字種（ASCII 英数字と `:_-`、64 文字以内）、Admin0・Admin1 の重複、Admin1 の国参照が Admin0 に無い。
- 国の付け替え（`id_resolution.json` の `admin1_parent`）: 付け替えた地域は、すべての片の代表点が付け替え先の国に入ること。
  既知の食い違い（`crossref-report.tsv`）とは別に、入らなければ止める。
- 日本: Admin1 が 47 件で、47 都道府県の対応表（[`tables/japan_prefectures.json`](tables/japan_prefectures.json)）に
  過不足なく対応すること。
- 対応表の余り: [`tables/id_resolution.json`](tables/id_resolution.json) と
  [`tables/country_names_ja.json`](tables/country_names_ja.json) に書いたのに使われない項目。
- サイズ: 成果物の合計が 15,000,000 B（PRD AC10 の目標）を超えるとき。**簡略化はせずに止めて**数値を報告する。

座標は IEEE 754 倍精度のまま丸めも簡略化もしません（復号した値は元の shapefile とビット単位で一致します）。

## 対応表（`tables/`）

|ファイル|内容|
|---|---|
|`id_resolution.json`|`admin1_id`: ID に使えない文字（`+`・`?`）を含む `adm1_code` の置き換え先。`admin1_parent`: 日本視点版の Admin0 に無い国（`SOL`・`CYN`・`KAS`・`KAB`）を指す Admin1 の付け替え先。`admin1_exclude`: 名称が全く無い Admin1（7 件。地域として見せられないので含めない）|
|`japan_prefectures.json`|日本の Admin1 の `adm1_code` → 写真の `pref` 列の表記（47 都道府県。JIS X 0401 の順）|
|`country_names_ja.json`|国の日本語名の言い換え。miharashi の `tools/world_geojson_to_txt.py` の `NAME_OVERRIDES` と同じ（PRD R13）|

内部 ID は `ne:<ADM0_A3>`（国）と `ne:<adm1_code>`（地域）です。ISO コードや名称は主キーにせず、補助属性（`iso2`・`iso`）として持ちます。
地域の表示名は日本語→英語→原表記の順に補い、検索用の別名（日本語・英語・原表記・NE の別名）を `search` に持ちます。
海外の読み仮名は作りません。

## 国参照の食い違い

国は日本視点版（`_jpn`）、地域は既定版で、両者の国境の見方が違います。変換は Admin1 の各片の代表点を日本視点版の国に
当てて食い違いを `crossref-report.tsv` に書きます。2026-11-04 時点（5.1.1）の要旨:

- **別の国に入る片がある 6 件**: サハリン州（北方領土の 4 片は日本）、ホルモズガーン州（1 片は UAE）、
  モロッコの 2 地方（西サハラの 1 片ずつ）、クリミア自治共和国とセヴァストポリ（ウクライナ）。
- **海岸線の違いだけ 14 件**: Admin1 の沖合の小島が Admin0 の海岸線の外にある（アブダビ、コーラル・シー諸島、沖縄県の 6 片など）。

国が決まった後に、その国の Admin1 に当たらない点は『地域不明』になります（設計「境界データと分類器」）。

## 試験

```bash
python3 -m unittest discover -s tools/tests -v        # 標準ライブラリだけ。形状の検査・往復・対応表・成果物と manifest の一致
REGIONS_SOURCE_DIR=<取得済みのディレクトリ> .venv/bin/python -m unittest tools.tests.test_regions   # 元の shapefile とのビット一致も
cargo test -p geoframe --lib regions::                # manifest の SHA256・名称表・47 都道府県・復号と形式の不正の拒否
```
