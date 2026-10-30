# vendor/ — サードパーティ資産（セルフホスト）

管理ページを外部CDNに依存させないため、ここに実体を置いている。取得日はいずれも **2026-07-29**。
GEOFRAMEのバイナリには、このディレクトリのファイル（下のライセンス本文を含む）が埋め込まれる。

| ファイル | 取得元 | バージョン | ライセンス |
|---|---|---|---|
| `maplibre-gl-4.7.1.min.js` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.js | 4.7.1 | BSD-3-Clause |
| `maplibre-gl-4.7.1.min.css` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.css | 4.7.1 | BSD-3-Clause |
| `fonts/share-tech-mono-latin.woff2` | https://fonts.gstatic.com/ (Google Fonts 経由) | v16 (latin subset) | SIL Open Font License 1.1 |
| `fonts.css` | 手書き（Google Fonts の `css2?family=Share+Tech+Mono&display=swap` を再現） | — | — |

- MapLibre GL JS: https://github.com/maplibre/maplibre-gl-js （BSD-3-Clause）
- Share Tech Mono: https://fonts.google.com/specimen/Share+Tech+Mono （SIL OFL 1.1）

## ライセンス本文

再配布するときに本文の同梱が求められるため、公式の配布元から取得した本文を`licenses/`に置いている。
どちらも**取得したままで、改変していない**（取得日 2026-10-30）。配布物（上の`.js`・`.css`・`.woff2`）の側は、
取得後に手を入れていない。

| ファイル | 取得元 | 版 | 備考 |
|---|---|---|---|
| `licenses/maplibre-gl-js-LICENSE.txt` | https://github.com/maplibre/maplibre-gl-js の`LICENSE.txt`（タグ`v4.7.1`） | 4.7.1 | MapLibreのBSD-3-Clause本文。同じ文書に、MapLibreが取り込んでいるmapbox-gl-js v1.13以前（BSD-3-Clause）などの本文も含まれる |
| `licenses/share-tech-mono-OFL.txt` | https://github.com/google/fonts の`ofl/sharetechmono/OFL.txt`（`main`。このファイルの最終変更は google/fonts のコミット`90abd17b4f97671435798b6147b698aa9087612f`、2015-03-06） | — | SIL Open Font License 1.1。著作権表示は Carrois Type Design, Ralph du Carrois。Reserved Font Nameは「Share」 |

`share-tech-mono-latin.woff2`は、Google Fontsが配信するlatinサブセット（v16）をそのまま置いたものである。
フォントを改変・再サブセット化する場合は、OFLのReserved Font Nameの条件を確認すること。


## 更新手順

バージョンを上げるときはファイル名の版数も合わせて変え、`web/static/admin.html` の
`<link>` / `<script>` の参照と、`crates/geoframe/src/embedded.rs`・`crates/geoframe/tests/static_files.rs`の
ファイル名、上の表とライセンス本文（新しい版のもの）を更新する。

## 地図タイルについて

管理ページの位置修正に使う背景地図タイル（OpenStreetMap / 地理院タイル）はここには置かない。
GEOFRAMEのRustサーバーの`/tiles/{osm,gsi}/{z}/{x}/{y}.png`プロキシが上流から取得し、
`GEOFRAME_DATA_DIR/tile_cache/`（既定は`./var/tile_cache/`）にファイルキャッシュする（版管理対象外）。
上流はOpenStreetMapの利用ポリシーと地理院タイルの利用規約に従うこと
（設定は[設定リファレンス](../../docs/reference/configuration.md)の`GEOFRAME_TILE_*`）。
