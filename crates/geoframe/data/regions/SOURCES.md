# 境界データの出典

このディレクトリの `polygons.bin.gz` と `names.json.gz` は、[Natural Earth](https://www.naturalearthdata.com/) の
国と第1行政区画（州・県など）の境界を、そのまま（丸めも簡略化もせず）変換したものです。

## 出典

- 提供元: Natural Earth（<https://www.naturalearthdata.com/>）
- 版: 5.1.1（1:10m。Natural Earth の最新のデータの版。GitHub 上の配布コミットは `v5.1.2` のタグ）
- 国（Admin 0）: `ne_10m_admin_0_countries_jpn`（日本視点版。北方領土・竹島などは日本の領土として扱われる）
- 第1行政区画（Admin 1）: `ne_10m_admin_1_states_provinces`（既定版）
- 取得元: <https://github.com/nvkelso/natural-earth-vector> のコミット `f1890d9f152c896d250a77557a5751a93d494776`（`v5.1.2`）の
  `10m_cultural/`。公式の配布 zip と shp・dbf がバイト単位で一致することを確かめている
- 元ファイルの URL・サイズ・SHA256 は `manifest.json` に記録している

## 利用条件

Natural Earth のデータは**パブリックドメイン**で、帰属表示は不要です
（[利用条件](https://www.naturalearthdata.com/about/terms-of-use/)）。出典を明記するのが礼儀とされているので、ここに残します。

> Made with Natural Earth. Free vector and raster map data @ naturalearthdata.com.

## 注意

- Natural Earth の境界は測量用の精度ではなく、最新の行政区画も保証しません。画面は撮影位置からの分類であることを示します。
- 国境の見方が分かれる地域（北方領土・竹島など）は日本視点版の国境で国を判定します。第1行政区画は既定版のままなので、
  国の境界と第1行政区画が食い違う場所があります（一覧は `tools/regions/crossref-report.tsv`）。
- 名称の無い第1行政区画 7 件は、地域として見せられないので含めていません（`manifest.json` の `counts.excluded_admin1`）。

## 変換

`tools/regions/update_regions.py` が取得（SHA256 を検証）・検査・変換を行います。手順は `tools/regions/README.md`。
通常の `cargo build` は何も取得・生成せず、ここにコミットされた成果物を `include_bytes!` で埋め込むだけです。
manifest の SHA256 と成果物の一致は `cargo test`（`regions::tests`）が検査します。

## ファイル

|ファイル|内容|
|---|---|
|`manifest.json`|元データの URL・版・SHA256、変換器の版、成果物の SHA256・件数・サイズ上限|
|`polygons.bin.gz`|国・地域のポリゴン本体（形式は `crates/geoframe/src/regions/data.rs` の冒頭）|
|`names.json.gz`|ID→名称表（日本語・英語・原表記・別名、日本の都道府県の対応）|
|`SOURCES.md`|このファイル|
