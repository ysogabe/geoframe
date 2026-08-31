# 3D PHOTO JOURNEY Lite（Navara）実装計画と設計メモ

作成日: 2026-08-31
前提文書: [[0004-frontend-display-mode-arbitration]]（`docs/decisions/0004`）
対象ハード: N100 相当（非力な内蔵GPU・無風ファンレス小型機を想定）

## 1. 目的

既存の MapLibre（2Dタイル）表示を維持したまま、URL に `?mode=3d` を付けたときだけ
Navara（@navaramap/three）の ThreeView に切り替え、写真ポイントとライトルート線を
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
| `pixelRatio` | 1（上限） | 高DPIで画素を4倍にしない |
| `shadow` | false | 影はN100に重い |
| `multisampling` | 0 | MSAA無効 |
| `halfFloat` | false | ポスト処理は低精度で十分 |
| `picking` | false | タップ対象が無い |
| `animation` | false | Navaraのon-demand render（flyTo/変更時のみ描画） |
| `mobileOptimization` | true | モバイル向け軽量化を有効化 |
| `cacheBytes` | 128 MiB | N100のタイルキャッシュ上限 |
| `lodFog` / `dynamicSse` | 有効・粗め | 遠景を低LODにして可視タイルを抑制 |
| `maxPendingRequests` | 4（`?low`: 2） | カメラ移動時のdecode/fetch burstを抑制 |

`?low` では cacheBytes を64 MiB、worker heapを64 MiB、最大同時リクエストを2、
LOD/SSEをさらに粗くし、カメラ高度を4,200m、飛行時間を1,200ms、地点の滞在を最低30秒
（通常は16秒）にする。方位変更も止める。高度を上げるのは近景を拡大するためではなく、
遠景LODを選ばせてN100のタイル数と更新回数を抑えるためである。

**除外する効果（Lite では呼ばない）**: `DefaultPlugin.addDefaultPhotorealScene()`
（SSR / SSAO / DOF / 大気散乱 / 雲 / ボリューム / 水面テクスチャ）。これらが参照する
`@navaramap/three/dist/assets/`（大気・雲・ノイズ・水、計約38MB）はビルド成果物に
**同梱しない**（バンドル内に `assets/` 参照は0件であることをテストで保証）。

**タイル**: CDN（unpkg/jsdelivr/esm.sh）を使わず、既存の server.py プロキシを使う。
同一地点ではraster basemapを**常に1層だけ**生成する。日本の経緯度範囲ならGSI、国外なら
OSMを選び、巡回先が国境を跨ぐときは同じraster sourceのURLを`Source.update()`して切り替える。
GSI/OSMを同時に全面fetchしないため、2560x1080での初期tile burstと240 tiles/min上流制限への
到達を抑える。出典（GSI / OpenStreetMap）は `view.attribution.add()` で表示する。

## 5. 巡回（3dコントローラ）

`app/static/js/photo-journey-lite.js` が `?mode=3d` のときだけ動き、通常モードと同じ
初期化を流用する。

1. `manifestReady` を await → `reindexPhotos()` → `loadPlayState()` →
   `syncSchedule()` → `playQueue` が空なら `buildPlayOrder()`。
   （再生キュー・全網羅・記念日加味・localStorage永続化は 2D と完全に共用）
2. `import('/static/navara/photo-journey-lite.js')` で Navara バンドルを遅延ロード。
3. `createPhotoJourneyView(#modelayer)` → `await view.init()` →
   `addPhotoJourneyLayers(view, {points, route})`。
4. 巡回ループ: `nextQueueIdx()` → `markShown(i)` → `showPhoto(i)`（既存の写真カード +
   AREA INTEL を再利用）→ `setMode('3D PHOTO JOURNEY LITE')` →
   `flyPhotoJourneyCamera(...)`。通常滞在は約16秒、カメラ移動は5秒。低電力時は最低30秒滞在、
   1.2秒移動、4,200mの高高度・固定方位とし、LODと更新頻度を下げる。

**失敗時のフォールバック**: Navara / WASM / worker / WebGL の初期化に失敗したら
`sessionStorage['geoframe.3d.fallback'] = '1'` を立てて `location.reload()` する。
bootstrap.js が `mode=3d` を剥がして一度だけ通常モードへ戻す（ループしない）。

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
  フォールバックガード・軽量構成（LITE_OPTIONS）・manifest→ポイント/ルート変換・
  ThreeView 実利用（`new ThreeView` + `DefaultPlugin`、`addDefaultPhotorealScene` を
  呼ばない）を vm 上で実実行。
- `app/tests/js/navara-build-integrity.test.js`（新規）— `app/static/navara/` の
  生成物整合性（公開API・WASM内包・`assets/` デッドウェイト排除・`\0asm` マジック・
  workerチャンク1個）。
- 既存 `app/tests/js/right-column-layout.test.js` は無変更で成功を維持。

## 8. rollback

- `?mode=3d` を付けなければ従来どおり MapLibre 表示。新規ファイル
  （bootstrap.js / modes.js / scheduler-core.js / photo-journey-lite.js / navara 資産）
  はフィーチャーブランチ内で完結するため、問題があればブランチ全体を revert して
  `deploy/deploy-pull.sh` 経由でデプロイし直す。
- `npm run build:navara` は決定的なので、`app/static/navara/` はいつでも再生成できる。

## 9. 残る実機検証（スコープ外）

以下は N100 実機での目視・計測が必要で、本実装のスコープ外（自動テストでは検出不能）。

1. 実機 N100 で 30fps / 24fps が維持されること（フレーム計測）。
2. 長時間（数時間〜数日）表示後の焼き付き痕（drift の実効性を目視確認）。
3. Navara 初期化失敗時のフォールバックが実機ブラウザで一度だけ発動すること
   （WebGL 無効化ブラウザ等での確認）。
4. `?mode=3d` と `?low` の併用時の挙動（drift 停止を含む）。
5. 実データ（server.py 稼働・写真多数）での再生キュー引継ぎ（2D↔3D で再生位置が
   共有されることの確認）。
