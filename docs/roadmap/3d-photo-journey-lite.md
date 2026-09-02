# 3D PHOTO JOURNEY Lite（Navara）実装計画と設計メモ

作成日: 2026-08-31
前提文書: [[0004-frontend-display-mode-arbitration]]（`docs/decisions/0004`）
対象ハード: N100 相当（非力な内蔵GPU・無風ファンレス小型機を想定）

## 1. 目的

既存の MapLibre（2Dタイル）表示を維持したまま、URL に `?mode=3d` を付けたときだけ
Navara（@navaramap/three）の ThreeView に切り替え、高解像度の航空・衛星写真上を
低頻度のカメラ遷移で流す「3D PHOTO JOURNEY Lite」起動モードを追加する。

N100 で 30fps / 24fps を維持するため、Navara の重い演出（SSR/SSAO/DOF/大気/雲/ボリューム/
3D Tiles/大規模メッシュ）は**一切有効化しない**。あくまで「Lite」である。

## 2. 起動分岐（bootstrap.js）

`app/static/js/bootstrap.js` が**必ず最初に**読まれ、`window.GEOFRAME_MODE_3D` を確定する。

1. 通常URL（`?mode=3d` 無し）: `GEOFRAME_MODE_3D = false`。既存の MapLibre 表示・
   スケジューラ・イベント・パネルがそのまま動く。**Navara の JS/WASM/worker は
   一切読み込まれない**（遅延ロードのため）。
2. `?mode=3d`: `GEOFRAME_MODE_3D = true` にし、`body.mode3d` を付与。
   - `sessionStorage['geoframe.3d.fallback'] === '1'` のとき（前回の初期化失敗）は
     `mode` パラメータを剥がして `location.replace()` で一度だけ通常モードへ戻す。
     フラグはその場で消すため、**リロードループは生じない**。

分岐は `app/static/js/map.js`（MapLibre の生成をスタブに差し替え）と
`app/static/js/main.js`（`start()` の早期 return）でも参照する。

## 3. 描画層（#modelayer）

`app/index.html` に `#modelayer`（`#map` の直後）を追加し、`body.mode3d` のときだけ
`display:block` にする。MapLibre の `#map` は `body.mode3d` で `display:none`。

- **全画面テイクオーバー層**であり、右カラム（`#intel`/`#envpanel`/`#hzlegend` の
  `right:46px`）とは独立。`inset:-3px` にして、焼き付き対策の drift（±2px 移動）で
  端が欠けないようにする。
- **焼き付き対策**: `#hud-root` と同じ `drift 240s linear infinite` を継承する。
  `body.low`（軽量モード）では `#hud-root` と同じく drift を止める。
- 右カラム回帰テスト `right-column-layout.test.js` の不変条件（`right:46px` は3箇所、
  `:root` は2ブロック）を壊さないよう、`#modelayer` は `right` も `:root` も使わない。

## 4. 軽量構成（N100ターゲット）

Navara アダプタ `frontend/photo-journey-lite.js` が唯一の構成点。`LITE_OPTIONS` は
`Object.freeze` で固定する。

| 項目 | 値 | 理由 |
|---|---|---|
| `pixelRatio` | 0.5 | 2560x1080実機の固定GPU領域を約1/4へ削減し、航空写真LODへ予算を回す |
| `shadow` | false | 影はN100に重い |
| `multisampling` | 0 | MSAA無効 |
| `halfFloat` | false | ポスト処理は低精度で十分 |
| `picking` | false | タップ対象が無い |
| `animation` | false | Navaraのon-demand render（flyTo/変更時のみ描画） |
| `mobileOptimization` | true | モバイル向け軽量化を有効化 |
| `cacheBytes` | 512 MiB | 予約量ではなく保護上限。N95実測でZ3停止を解消しZ15へ到達 |
| `lodFog` / `dynamicSse` | 通常: 無効、`?low`: 有効・粗め | 通常は地点判読、lowは負荷削減を優先 |
| `maxPendingRequests` | 4（`?low`: 2） | カメラ移動時のdecode/fetch burstを抑制 |

`?low` では cacheBytes を64 MiB、worker heapを64 MiB、最大同時リクエストを2、
LOD/SSEをさらに粗くし、カメラ高度を4,200m、飛行時間を1,200ms、地点の滞在を最低45秒
（通常は最低30秒）にする。方位変更も止める。高度を上げるのは近景を拡大するためではなく、
遠景LODを選ばせてN100のタイル数と更新回数を抑えるためである。

通常表示では航空写真から撮影地点を判別できることを優先し、距離・傾斜によるLOD緩和と
常時SSE倍率を無効化する。Navara公式の軽量GIS例と同じく、raster追加前に
`ambient: {intensity: 1}`を置く。`unlit`切替はこの構成でラスタ合成を失うため使用しない。
`animation:false`のon-demand描画で親タイル到着後のLOD更新が止まらないよう、カメラ配置・
到着後だけ12秒間、200ms間隔で`forceUpdate()`する。常時描画は行わず、精細化後は停止する。

**除外する効果（Lite では呼ばない）**: `DefaultPlugin.addDefaultPhotorealScene()`
（SSR / SSAO / DOF / 大気散乱 / 雲 / ボリューム / 水面テクスチャ）。これらが参照する
`@navaramap/three/dist/assets/`（大気・雲・ノイズ・水、計約38MB）はビルド成果物に
**同梱しない**（バンドル内に `assets/` 参照は0件であることをテストで保証）。

**タイル**: CDN（unpkg/jsdelivr/esm.sh）を使わず、既存の server.py プロキシを使う。
同一地点ではraster basemapを**常に1層だけ**生成する。日本の経緯度範囲ではGSIの航空・衛星写真
（Z2〜13はLandsat 8、Z14〜18は全国最新写真（シームレス））、国外ではOSMを選ぶ。巡回先が
国境を跨ぐときは同じraster sourceのURLを`Source.update()`して切り替える。GSI/OSMを同時に
全面fetchしないため、2560x1080での初期tile burstと240 tiles/min上流制限への到達を抑える。
出典（GSI・Landsat / OpenStreetMap）は `view.attribution.add()` で表示する。
全3,099地点のポイントと1本の全世界ルート線は、画面を覆って位置判読を妨げ、GPU予算も
消費したためNavaraへ渡さない。地点名・座標は既存PHOTO LOG / AREA INTELで表示する。

## 4.1 GSI DEM地形（試行第一弾）

`?mode=3d` の航空写真の下に、国土地理院の公式標高タイルから作った実地形を初めて敷く。
2Dの`#map`（MapLibre）表示と航空機の実高度/POV挙動は変更しない。

**出典・提供範囲**（公式APIまとめ・DEMタイル仕様）:
- https://maps.gsi.go.jp/development/ichiran.html
- https://maps.gsi.go.jp/development/demtile.html
- Z0-8: `demgm_png`（全球数値地図由来。世界規模だが粗い）
- Z9-14: `dem_png`（数値標高モデル・国内10mメッシュ相当。海外・海上は欠測）
- エンコード（両方共通）: `x = 65536*R + 256*G + B`。`x < 2^23` は`x*0.01m`、
  `x == 2^23`（=RGB 128,0,0）は欠測、`x > 2^23` は`(x-2^24)*0.01m`。

**プロキシ・キャッシュ**（`app/geoframe/tiles.py` / `config.py`）: 既存の`gsi-photo`と同型の
band方式で`/tiles/gsi-dem/{z}/{x}/{y}.png`を追加した。上流フェッチは全layer共有の
240 tiles/min窓をそのまま使う（DEM専用に上限を増やさない）。上流が404（提供範囲外）を
返した場合、他layerのような透明PNGではなく、GSI仕様のno-data値そのものである
RGB(128,0,0)で塗り潰した不透明PNGを生成してキャッシュする。raster-demデコーダは
透明ピクセルを標高0mの実データと区別できないため、欠測は欠測のまま伝える。

**フォールバック**: Z9-14で国外・海上など欠測の地点は、常にRGB(128,0,0)のno-dataタイルが
200で返るため（欠測=平坦な標高として解釈される）3D初期化を止めない。Z0-8の`demgm_png`は
世界規模でカバーするため、Z9-14が欠測でも地形そのものが消えることはない
（詳細地形が使えない地域ほど粗い全球DEMへ自然に収束する）。

**フロント（`frontend/photo-journey-lite.js`）**: `addPhotoJourneyLayers()`が
raster-dem source（`JAPAN_GSI_ELEVATION_DECODER()`でデコード、Z0-14）を`terrain` layer→
抑制した`hillshade` raster layer（同じsourceを再利用・`raster.show:false`で疑似カラーは
出さない）→航空写真`raster` layerの順で追加する。terrainをimageryより先に追加しないと
後続レイヤのドレープ先が無いため、この順序を保つ。

**N100設定（最初の抑制値）**:

| 項目 | 通常 | `?low` | 理由 |
|---|---|---|---|
| terrain segments | 32 | 16 | メッシュ分割コスト。`?low`でも地形自体は無効化しない |
| terrain maxSse | 4 | 8 | 許容screen-space-error（px）。粗いほど早く安定する |
| 標高スケール | 1.0（設定項目なし） | 同左 | `@navaramap/three@0.1.1`にterrain用の乗算スケールAPIが無いことを型定義で確認済み。デコード結果の実測メートルをそのまま使う＝素の1.0 |
| hillshade exaggeration | 0.65 | 同左 | `HillshadeMaterial.exaggeration`（既定1.0）を検証済みの最も近い等価パラメータとして採用し、抑制した値にする |
| ambient intensity | 0.78 | 同左 | 従来の1.0から地形の陰影が見える程度まで落とす |
| sun intensity / applyColor / castShadow | 0.28 / true / false | 同左 | `applyColor:true`で大気散乱計算を経ず指定色をそのまま使う。`castShadow:false`でシャドウマップを一切生成しない（N100向けの既存`shadow:false`初期化オプションと合わせて二重に無効化） |
| pixelRatio | 0.5（既存のまま） | 0.5 | 変更しない |

`addDefaultPhotorealScene()` / SSR / SSAO / DOF / 雲 / 大気散乱エフェクト / 常時描画
（`animation:true`）は今回も一切使わない。terrain layerも`castShadow:false`・
`receiveShadow:false`を明示する。

**航空機との関係**: 航空機Polygonは`clampToGround:false`・`perPositionHeight:true`のまま
変更しない。新しい地形の上に絶対高度でそのまま浮かぶ（地形へクランプしない）。
MEMORY LINKの機上視点・アラートカメラも変更しない。

**出典表示**: 既存の`view.attribution.add()`（国土地理院・ichiran.htmlへのリンク）の
attribution文言へ「標高タイル（基盤地図情報数値標高モデル・全球数値地図）」を追記した。
DEM専用の別エントリは追加せず、同一発行元（国土地理院）の1エントリへ統合している。

**このトライアルで確認しないこと（実機QA・orchestrator側）**:
- N100実機でのfps/GPUメモリへの実測影響（本ロードマップの§9「実機検証」と同様、後日の
  長時間運用確認に含める）。
- 見た目の最終的な良し悪し（陰影の強さ・地形の誇張度など）の主観評価。今回の数値
  （hillshade 0.65・ambient 0.78・sun 0.28）は「抑制した最初の試行値」であり、
  実機での見え方を見て調整する前提の暫定値。
- 山岳地・崖などextreme reliefでのterrain skirt/tile境界の視覚的な破綻有無。

## 5. 巡回（3dコントローラ）

`app/static/js/photo-journey-lite.js` が `?mode=3d` のときだけ動き、通常モードと同じ
初期化を流用する。

1. `manifestReady` を await → `reindexPhotos()` → `loadPlayState()` →
   `syncSchedule()` → `playQueue` が空なら `buildPlayOrder()`。
   （再生キュー・全網羅・記念日加味・localStorage永続化は 2D と完全に共用）
2. `import('/static/navara/photo-journey-lite.js')` で Navara バンドルを遅延ロード。
3. `createPhotoJourneyView(#modelayer)` → `await view.init()`。初回キュー地点へ
   `setCamera()` を先に適用してから初回地点1件だけで`addPhotoJourneyLayers()`を呼び、
   `forceUpdate()` で最初の可視タイル選択・描画を要求する。初回は同一ターンの
   `flyTo()` を省略し、Navara既定カメラへの上書きを防ぐ。
4. 巡回ループ: `nextQueueIdx()` → `markShown(i)` → `showPhoto(i)`（既存の写真カード +
   AREA INTEL を再利用）→ `setMode('3D PHOTO JOURNEY LITE')` →
   `flyPhotoJourneyCamera(...)`。通常滞在は最低30秒、カメラ移動は5秒。低電力時は最低45秒滞在、
   1.2秒移動、4,200mの高高度・固定方位とし、LODと更新頻度を下げる。

**失敗時のフォールバック**: Navara / WASM / worker / WebGL の初期化に失敗したら
`sessionStorage['geoframe.3d.fallback'] = '1'` を立てて `location.reload()` する。
bootstrap.js が `mode=3d` を剥がして一度だけ通常モードへ戻す（ループしない）。

## 5.1 ADS-B航空機Polygonとアラート視点

`/api/aircraft` は従来の `alt` / `track` を維持しつつ、readsbの `alt_geom`（WGS84楕円体
基準の幾何高度）、`alt_baro`、`true_heading`、`mag_heading`、`track_rate`、`roll`、
`geom_rate`、`baro_rate`、`seen_pos` とフィード時刻 `now` を返す。3D表示では
`alt_geom` → `alt_baro` → `alt` の順に採用し、ftをmへ変換する。`ground`、欠損値、NaN/∞は
地表高度0または安全な既定値へ正規化する。

航空機はGeoJSONの8〜12頂点Polygonとして作り、全頂点に同じ実高度を持たせる。
Navaraのvector layerで `polygon.perPositionHeight: true` を指定するため、地表へクランプせず
上空の平面機影として描画できる。通常は緑、行き先マッチは琥珀、rareはマゼンタ、emergencyは
赤橙の4層に分け、位置の鮮度60秒以内・最大20機に制限する。

MEMORY LINKではCustomEvent経由で3D controllerが巡回を止め、対象のbasemapへ切り替え、
まず上空のfocus/chaseへ `flyTo` した後、幾何高度・true heading優先・上昇率/速度から推定した
pitch・roll clampを使う再構成機上視点へ移る。これはコックピット映像や実際の機内視点ではなく、
ADS-Bの位置・速度・姿勢から作る近似である。地形・建物・雲は取得せず、機影の高度も楕円体基準
なので、山岳地や高度基準の差による見え方は残る。rare/emergencyは少なくともchase/focusを
表示し、対象Polygonはカメラが機体内へ入らないよう一時的に除外する。

フィード更新の間だけ4〜5Hz（通常5Hz、LOW_POWER 4Hz）で最大8秒の短いデッドレコニングを行う。
常時 `requestAnimationFrame` は使わず、アラート終了、対象消失、別アラートへの置換、スリープ、
Navaraエラー時にタイマーとリスナーを破棄して巡回へ戻す。N100では航空機Polygonの数、
更新周波数、タイル同時取得数を増やさず、SSR/SSAO/DOF、影、雲、完全な地形再構成をLiteの
対象外とする。

対象消失は、成功したfeed（`feedAvailable: true`）で対象キーが無くなった場合だけ即時終了と
判定する。HTTP/JSON取得失敗（`feedAvailable: false`）は直前のfeedと最終成功時刻を保持し、
誤ってアラートを終了させない。一方、通常機影の鮮度は `seen_pos` に最終成功feedからの
wall-clock経過時間を加算して判定するため、transport outageが続けばcombined ageが60秒を
超えた時点で凍結Polygonも消える。終了時は航空機Polygonを再表示し、アラート用interval・
dwell timerを破棄して巡回を再開する。

## 6. ビルド

```
npm ci                 # package-lock.json から正確なバージョンで再現
npm run build:navara   # vite build → app/static/navara/ を決定的に生成
```

- 依存はすべて**完全固定**（caret/tilde 無し）: `@navaramap/three@0.1.1`、
  `@navaramap/three-default-plugin@0.1.1`、`three@0.185.1`、`postprocessing@6.39.4`、
  `vite@6.4.3`。
- 生成物はコミット済み（`app/static/navara/`、計9ファイル・約48MB）:
  - `photo-journey-lite.js`（エントリ/facade。Liteの公開APIを再輸出）
  - `photo-journey-lite2.js`（メインバンドル。エンジンWASMをbase64内包）
  - `spark.module-*.js`（workerチャンク。動的importでロード）
  - `navara-worker.js` / `navara-font-worker.js`（relative WASM URLを解決する実ファイルworker）
  - `navara_wasm_bg.wasm` / `navara_wasm_api_bg.wasm` / `navara_wasm_worker_bg.wasm` /
    `navara_wasm_font_worker_bg.wasm`
    （`\0asm` マジックを持つ実体。実行時の安全網を兼ねる）
- ハッシュ付きファイル名を排し（`chunkFileNames: "[name].js"`）、Navara 0.1.1に
  内蔵されたdata: workerを`navara-worker.js`/`navara-font-worker.js`へ外出しする。
  worker内のrelative WASM URLを実ファイルの`self.location.href`から解決できるようにする。
  2回ビルドしてmd5が一致することを確認する（決定的）。

`app/package.json` は `{"type": "commonjs"}` を宣言する。ルート `package.json` が
`"type": "module"` を持つため、`app/tests/js/*.test.js`（CommonJS）が ESM 扱いされて
`require` が壊れるのを打ち消すための最小構成。

## 7. テスト

- `app/tests/js/scheduler-quality.test.js` — `scheduler-core.js` を読むよう変更。
  純ロジックを scheduler.js から分離したこと以外、アサーションは同一。
- `app/tests/js/photo-journey-lite.test.js`（新規）— 起動分岐（bootstrap.js）・
  フォールバックガード・軽量構成（LITE_OPTIONS）・単一raster basemap変換・
  ThreeView 実利用（`new ThreeView` + `DefaultPlugin`、`addDefaultPhotorealScene` を
  呼ばない）・ADS-B純関数・Polygon layer lifecycle・CustomEvent置換競合・3D controllerの
  alert pause/resumeとtransport outage agingをvm上で実実行。
- `app/tests/test_adsb.py` — `/api/aircraft` の旧フィールド互換、追加姿勢フィールド、
  feed `now`、ground/欠損/非有限値の正規化をモックフィードで検証。
- `app/tests/js/navara-build-integrity.test.js`（新規）— `app/static/navara/` の
  生成物整合性（公開API・WASM内包・`assets/` デッドウェイト排除・`\0asm` マジック・
  workerチャンク1個・`raster-dem`/`hillshade`がbundleへ含まれること）。
- 既存 `app/tests/js/right-column-layout.test.js` は無変更で成功を維持。
- `app/tests/js/gsi-dem-terrain.test.js`（新規）— §4.1のGSI DEM地形トライアルを検証する。
  GSI標高エンコード式の素のJS再実装（no-data閾値2^23の陽性/陰性対照）・
  config.py/tiles.py/handlers.pyのgsi-demルーティング契約のソーステキスト照合・
  `addPhotoJourneyLayers`のraster-dem source登録とterrain→hillshade→imageryの追加順序・
  normal(32/4)/low(16/8)のquality設定・3D controllerの`boot()`をvm実行して
  `view.addLight`へ渡るambient(0.78)/sun(0.28, applyColor, castShadow:false)の実値・
  重い演出（addDefaultPhotorealScene/addEffect/shadow:true）が無いこと・
  航空機のperPositionHeight/clampToGround:falseが地形追加後も変わらないことを実実行で検証。
- `app/tests/test_tiles.py` — gsi-photoの既存テストに加え、gsi-demのURL選択（z0-8=demgm_png/
  z9-14=dem_png）とzoom境界、no-dataタイルがRGB(128,0,0)・GSI仕様の欠測閾値2^23と一致する
  こと、`fetch_tile`が上流404/5xx/流量制限それぞれでno-dataタイルを返しつつ404だけを
  永続キャッシュすること（`urlopen`をモックし、実際のPillowで生成したPNGバイト列を検証）、
  標高PNGがJPEG写真と違い非可逆な再エンコードをしないことをカバーする。

## 8. rollback

- `?mode=3d` を付けなければ従来どおり MapLibre 表示。新規ファイル
  （bootstrap.js / modes.js / scheduler-core.js / photo-journey-lite.js / navara 資産）
  はフィーチャーブランチ内で完結するため、問題があればブランチ全体を revert して
  `deploy/deploy-pull.sh` 経由でデプロイし直す。
- `npm run build:navara` は決定的なので、`app/static/navara/` はいつでも再生成できる。

## 9. 実機検証

N95実機（`geoframe-n100.local`）で航空写真単独の隔離ページを計測した。

- 2560x1080 CSS表示、内部canvas 1280x540（pixelRatio 0.5）
- カメラ高度1,800m、俯角-50°、Navara算出zoom 16.32
- GSI航空写真131枚、最大Z15、GPU推定約440MiB
- 3秒計測60fps、p95 16.7ms、50ms超フレーム0

以下は長時間運用で継続確認する。

1. 長時間（数時間〜数日）表示後の焼き付き痕（drift の実効性を目視確認）。
2. Navara 初期化失敗時のフォールバックが実機ブラウザで一度だけ発動すること
   （WebGL 無効化ブラウザ等での確認）。
3. `?mode=3d` と `?low` の併用時の挙動（drift 停止を含む）。
4. 実データ（server.py 稼働・写真多数）での再生キュー引継ぎ（2D↔3D で再生位置が
   共有されることの確認）。
