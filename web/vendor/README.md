# vendor/ — サードパーティ資産（セルフホスト）

管理ページを外部CDNに依存させないため、ここに実体を置いている。取得日はいずれも **2026-07-29**。

| ファイル | 取得元 | バージョン | ライセンス |
|---|---|---|---|
| `maplibre-gl-4.7.1.min.js` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.js | 4.7.1 | BSD-3-Clause |
| `maplibre-gl-4.7.1.min.css` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.css | 4.7.1 | BSD-3-Clause |
| `fonts/share-tech-mono-latin.woff2` | https://fonts.gstatic.com/ (Google Fonts 経由) | v16 (latin subset) | SIL Open Font License 1.1 |
| `fonts.css` | 手書き（Google Fonts の `css2?family=Share+Tech+Mono&display=swap` を再現） | — | — |

- MapLibre GL JS: https://github.com/maplibre/maplibre-gl-js （BSD-3-Clause）
- Share Tech Mono: https://fonts.google.com/specimen/Share+Tech+Mono （SIL OFL 1.1）


## 更新手順

バージョンを上げるときはファイル名の版数も合わせて変え、`static/admin.html` の
`<link>` / `<script>` の参照を書き換える。

## 地図タイルについて

管理ページの位置修正に使う背景地図タイル（OpenStreetMap / 地理院タイル）はここには置かない。
`server.py` の `/tiles/{osm,gsi}/{z}/{x}/{y}.png` プロキシが取得し、
`GEOFRAME_DATA_DIR/tile_cache/`（開発時の既定は `app/var/tile_cache/`） にファイルキャッシュする（版管理対象外）。
