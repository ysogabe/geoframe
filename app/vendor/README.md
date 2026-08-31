# vendor/ — サードパーティ資産（セルフホスト）

キオスク運用ではネットワークが不安定・不通になりうるため、CDN 参照をやめて
ここに実体を置いている。取得日はいずれも **2026-07-29**。

| ファイル | 取得元 | バージョン | ライセンス |
|---|---|---|---|
| `maplibre-gl-4.7.1.min.js` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.js | 4.7.1 | BSD-3-Clause |
| `maplibre-gl-4.7.1.min.css` | https://cdnjs.cloudflare.com/ajax/libs/maplibre-gl/4.7.1/maplibre-gl.min.css | 4.7.1 | BSD-3-Clause |
| `fonts/share-tech-mono-latin.woff2` | https://fonts.gstatic.com/ (Google Fonts 経由) | v16 (latin subset) | SIL Open Font License 1.1 |
| `fonts.css` | 手書き（Google Fonts の `css2?family=Share+Tech+Mono&display=swap` を再現） | — | — |
| `pref.geojson` | https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_admin_1_states_provinces.geojson | Natural Earth 10m admin-1（取得 2026-07-30） | Public Domain |

- MapLibre GL JS: https://github.com/maplibre/maplibre-gl-js （BSD-3-Clause）
- Share Tech Mono: https://fonts.google.com/specimen/Share+Tech+Mono （SIL OFL 1.1）
- Natural Earth: https://www.naturalearthdata.com/about/terms-of-use/ （パブリックドメイン。
  「no permission needed」だが慣例として出典を記す）

## pref.geojson — 都道府県ポリゴン（制覇マップ用）

統計カットで訪問済みの都道府県を塗り分けるためのポリゴン。**108,909 バイト / 47 feature**。

- **取得元**: Natural Earth 1:10m Admin 1 – States, Provinces の GeoJSON 版
  （`nvkelso/natural-earth-vector` リポジトリの `geojson/` 配下。上表のURL、約 40MB）から
  `properties.admin == "Japan"` の 47 feature だけを抜き出した。
- **ライセンス**: パブリックドメイン（Natural Earth）。出典明記も義務ではないが上記のとおり記載する。
  国土数値情報 N03 は出典明記で利用できるが、全国版 ZIP が 632MB あり
  「同梱物を数百KBに収める」用途には重すぎるため採らなかった。
- **属性**: `name` = 都道府県名（Natural Earth の `name_ja`。`北海道` `千葉県` のように
  都/道/府/県 まで入った表記で、`/api/stats` の `pref_list`（= catalog.db の `photos.pref`）と
  そのまま突き合わせられる）、`code` = ISO 3166-2:JP の番号部（`01`〜`47`）。
- **簡略化手法**（stdlib のみ。新規 pip 依存なし。使い捨てスクリプトで一度だけ実行）:
  1. **Douglas-Peucker**（許容誤差 0.005 度 ≒ 550m）で頂点を間引く。再帰ではなくスタック実装。
  2. **座標を小数4桁に丸める**（≒11m）。丸めた結果リングの先頭と末尾がずれるので
     必ず閉じ直し、4点未満になったリングは捨てる。
  3. **微小な島を間引く**。外周リングの面積が 0.005 度²（≒55km²）未満のポリゴンを落とすが、
     **各県で最大のポリゴンは面積によらず必ず残す**（沖縄県・東京都のように主島が
     小さい県が丸ごと消えるのを防ぐ）。結果として沖縄本島・佐渡島・淡路島・対馬・
     伊豆諸島などは残り、無数の岩礁が落ちる。
  - 元の 47 feature / 約 20 万頂点 → 5,500 頂点。統計カットは z4.2（日本全体の俯瞰）
    なので、この精度で見た目の差は出ない。
- **更新するとき**: 上記URLから GeoJSON を取り直し、同じ 3 手順を通す。境界の
  正確さが要る用途（面積計算・行政境界の厳密判定）にこのファイルを使わないこと。

## 更新手順

バージョンを上げるときはファイル名の版数も合わせて変え、`index.html` の
`<link>` / `<script>` の参照を書き換える。`index.html` からは**相対パス**で
参照すること（`file://` で開くモックモードでも動かすため、`/vendor/...` の
絶対パスにしない）。

## 地図タイルについて

背景地図タイル（OpenStreetMap / 地理院タイル）はここには置かない。
`server.py` の `/tiles/{osm,gsi}/{z}/{x}/{y}.png` プロキシが取得し、
`GEOFRAME_DATA_DIR/tile_cache/`（開発時の既定は `app/var/tile_cache/`） にファイルキャッシュする（版管理対象外）。

## Navara（3D PHOTO JOURNEY Lite）の生成物について

`?mode=3d` 用の Navara（@navaramap/three）は **vendor/ には置かず**、
npm 依存（ルート `package.json`、全バージョン完全固定）から `npm run build:navara`
（vite）で `app/static/navara/` へ**決定的に生成しコミットする**（計9ファイル・約48MB）。

| 依存 | バージョン | 備考 |
|---|---|---|
| `@navaramap/three` | 0.1.1 | ThreeView（Rust/WASMコア） |
| `@navaramap/three-default-plugin` | 0.1.1 | DefaultPlugin。`addDefaultPhotorealScene()` は呼ばない |
| `three` | 0.185.1 | three 本体 |
| `postprocessing` | 6.39.4 | ポスト処理（LiteではhalfFloat無効で使用） |
| `vite` | 6.4.3 | build専用。Vite dev-server file read highを解消 |

- タイルは CDN を使わず、上記 `/tiles/{gsi,osm}/` プロキシを指す（出典は
  `view.attribution` に GSI / OSM を登録）。
- `@navaramap/three/dist/assets/`（大気・雲・ノイズ・水のテクスチャ）は参照されない
  ため**同梱しない**（バンドル内に `assets/` 参照0件をテストで保証）。
- 詳細は `docs/roadmap/3d-photo-journey-lite.md` を参照。
- `navara-worker.js` / `navara-font-worker.js` は、Navara 0.1.1に内蔵されたdata: workerを
  実ファイル化したもの。worker内のrelative WASM URLを壊さず、tile/polyline生成時の
  `Invalid URL`を防ぐ。
- 1地点ではGSIまたはOSMのraster basemapを1層だけfetchする。国内はGSI、国外はOSMとし、
  巡回先が変わった場合も同じsourceを更新する。二重fetchを避けるためのN100対策であり、
  上流の240 tiles/min制限を超えないことを保証するものではない。
