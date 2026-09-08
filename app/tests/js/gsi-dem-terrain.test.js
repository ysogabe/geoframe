/* GSI DEM terrain + 地形の立体感を優先したDEM由来シェーディングを、ブラウザ無しで
 * 実挙動ベースに検証する。
 *
 * 検証対象と方式:
 *  - GSI標高タイルのRGBエンコード/デコード式（knowledge snapshot由来）: 素のJSで
 *    再実装し、実際の標高値・no-data境界を独立に検証する（陽性/陰性対照）。
 *  - server.py側のgsi-demルーティング/no-data方針: config.py/tiles.py/handlers.py
 *    のソーステキストを読み、frontendが依存する契約（URL・zoom帯・no-data色）が
 *    実装と一致することを確認する（陽性/陰性対照）。
 *  - frontend/photo-journey-lite.js: import文とexport接頭辞を除去してvm上で実行し、
 *    raster-dem source・terrain/hillshade layerの追加順序・quality設定
 *    （segments/maxSse/pixelRatio）・重い演出が無いことを実行時に検証する。
 *  - static/js/photo-journey-lite.js（3dコントローラ）: boot()をvm上で実実行し、
 *    view.addLightへ渡る値（ambient/sun/applyColor/castShadow）を実際に捕捉する。
 *  - 航空機: DEM/terrain追加が既存のperPositionHeight/絶対高度描画を変えないことの
 *    回帰確認。
 */
const assert = require('assert');
const fs = require('fs');
const vm = require('vm');
const path = require('path');

function runInContext(source, context) {
  return vm.runInContext(source, vm.createContext(context));
}

/* ============================================================
 * 1. GSI標高エンコード式（knowledge snapshot）の独立再実装
 *    x = 65536*R + 256*G + B
 *    x <  2^23            => elevation = x * 0.01m
 *    x == 2^23 (=128,0,0) => no data
 *    x >  2^23            => elevation = (x - 2^24) * 0.01m
 * ============================================================ */
function gsiElevation(r, g, b) {
  const x = 65536 * r + 256 * g + b;
  const NO_DATA = 2 ** 23;
  if (x === NO_DATA) return null;
  if (x < NO_DATA) return x * 0.01;
  return (x - 2 ** 24) * 0.01;
}

// 陽性対照: 実在しうる標高値が正しくデコードされる。
assert.strictEqual(gsiElevation(0, 0, 0), 0, '(0,0,0)は標高0m');
assert.strictEqual(gsiElevation(0, 39, 16), 100, '(0,39,16) x=9994+16=10000 => 100.00m');
assert.ok(Math.abs(gsiElevation(5, 195, 0) - 3776) < 1e-9,
  '大きな正標高（富士山クラスの3776m）も0.01m精度で復元する');
// 陰性対照: RGB(128,0,0) はサーバーの欠測エンコードそのもの（no data）。
assert.strictEqual(gsiElevation(128, 0, 0), null, 'RGB(128,0,0)はno-data（2^23）と判定する');
// 陰性対照: 閾値のすぐ内側/外側で判定が切り替わることを確認する（境界値の対称テスト）。
assert.notStrictEqual(gsiElevation(127, 255, 255), null, '2^23-1（no-dataの直前）は実データ扱い');
assert.ok(gsiElevation(127, 255, 255) > 0, '2^23-1は正の標高として復元する');
assert.ok(gsiElevation(128, 0, 1) < 0, '2^23+1（no-dataの直後）は負標高側の式に切り替わる');

/* ============================================================
 * 2. バックエンド（config.py/tiles.py/handlers.py）のgsi-demルーティング契約
 * ============================================================ */
const configSrc = fs.readFileSync('geoframe/config.py', 'utf8');
const tilesSrc = fs.readFileSync('geoframe/tiles.py', 'utf8');
const handlersSrc = fs.readFileSync('geoframe/handlers.py', 'utf8');

// 陽性対照: gsi-demはband方式（gsi-photoと同型）で、demgm_png(z0-8)/dem_png(z9-14)を持つ。
// 上流同時接続数はN100試行のため2へ絞る（共有240枚/分の予算は据え置き）。
assert.ok(/"gsi-dem":\s*\("",\s*14,\s*2\)/.test(configSrc),
  'TILE_SOURCESのgsi-demはband選択方式（空URL・maxzoom14・上流並列2）で登録される');
assert.ok(/GSI_DEM_TILE_BANDS\s*=\s*\(\s*\(\s*0,\s*8,\s*"[^"]*demgm_png[^"]*"\s*\)/.test(configSrc),
  'z0-8は全球DEM(demgm_png)を使う');
assert.ok(/\(\s*9,\s*14,\s*"[^"]*\/dem_png\/[^"]*"\s*\)/.test(configSrc),
  'z9-14は数値標高モデル(dem_png)を使う');
assert.ok(/RGB\s*\(?128,\s*0,\s*0\)?/.test(configSrc), 'config.pyがRGB(128,0,0) no-dataを文書化する');
// 陰性対照: gsi-demは他の固定URL層（osm/gsi）と違い、テーブルの第1要素が空文字（=band方式）。
assert.ok(!/"gsi-dem":\s*\("https/.test(configSrc),
  'gsi-demは固定URLテンプレートを持たない（zoomごとの帯選択に一本化する）');

assert.ok(tilesSrc.includes('DEM_NO_DATA_RGB = (128, 0, 0)'),
  'tiles.pyの欠測色定数がRGB(128,0,0)と一致する');
assert.ok(tilesSrc.includes('_dem_no_data_tile_bytes'), 'tiles.pyがDEM専用の欠測タイル生成関数を持つ');
assert.ok(/_placeholder_tile\(layer\)/.test(tilesSrc),
  'fetch_tileの代替タイル選択がlayerごとに分岐する（gsi-demだけno-data色）');
assert.ok(!tilesSrc.includes('_dem_no_data_tile_bytes() if layer == "gsi-photo"'),
  '陰性対照: 欠測色の分岐がgsi-photoへ誤って適用されていない');

assert.ok(/TILE_RE = re\.compile\(r"\^\/tiles\/\(osm\|gsi\|gsi-photo\|gsi-dem\)/.test(handlersSrc),
  'handlersのtileルートがgsi-demを認識する');

// 陽性対照: ズーム範囲外404の判定からgsi-demを除外する。z>14はfetch_tileへ委譲して
// RGB(128,0,0)欠測タイルを200で返す（404だとNavara raster-dem loaderがbodyを
// デコードせず初期化を失敗させるため）。
assert.ok(/if\s+z\s*>\s*TILE_SOURCES\[layer\]\[1\]\s+and\s+layer\s*!=\s*["']gsi-dem["']/
  .test(handlersSrc),
  'handlersはズーム範囲外404の判定からgsi-demを除外する（z15+もfetch_tileへ委譲）');

// 陽性対照: fetch_tileは上流URL解決（提供ズーム外のValueError）を分/枚の共有予算チェック
// より先に行う。上流へ行かない要求が240枚/分のスライディング窓を消費しない。
const urlResolveIdx = tilesSrc.indexOf('url = _upstream_tile_url(layer, z, x, y)');
const budgetCheckIdx = tilesSrc.indexOf('if not _tile_fetch_allowed():');
assert.ok(urlResolveIdx !== -1 && budgetCheckIdx !== -1 && urlResolveIdx < budgetCheckIdx,
  'fetch_tileは上流URL解決を共有予算チェックより先に行う（提供ズーム外が予算を消費しない）');

/* ============================================================
 * 3. frontend/photo-journey-lite.js — raster-dem登録・terrain順序・quality設定
 * ============================================================ */
const frontendPath = '../frontend/photo-journey-lite.js';
const frontendSrc = fs.readFileSync(frontendPath, 'utf8');

assert.ok(/import ThreeView,\s*\{\s*JAPAN_GSI_ELEVATION_DECODER\s*\}\s*from\s*"@navaramap\/three"/
  .test(frontendSrc), '@navaramap/threeからJAPAN_GSI_ELEVATION_DECODERを取得する');
assert.ok(frontendSrc.includes('/tiles/gsi-dem/{z}/{x}/{y}.png'),
  'GSI DEM標高タイルもserver.pyのプロキシ経由で取得する（CDN不使用）');
for (const bad of ['unpkg', 'jsdelivr', 'esm.sh', 'cdn.']) {
  assert.ok(!frontendSrc.toLowerCase().includes(bad), `DEM関連コードにもCDN参照が無い: ${bad}`);
}
// 陰性対照: 重い演出（DefaultPhotorealScene・postprocessing effect・実shadow有効化）を呼ばない。
assert.ok(!/\.addDefaultPhotorealScene\s*\(/.test(frontendSrc), 'addDefaultPhotorealSceneを呼ばない');
assert.ok(!/\.addEffect\s*\(/.test(frontendSrc), 'SSR/SSAO/DOF/雲などのeffectを一切追加しない');
assert.ok(!/shadow:\s*true/.test(frontendSrc), 'shadow:trueをどこにも書かない');
assert.ok(/castShadow:\s*false/.test(frontendSrc) && /receiveShadow:\s*false/.test(frontendSrc),
  'terrain layerでもcastShadow/receiveShadowを明示的にfalseにする');

const stripped = frontendSrc
  .replace(/^import\s+[^\n]*\n/gm, '')
  .replace(/^export\s+/gm, '');

const constructedViews = [];
class FakeThreeView {
  constructor(options) { this.options = options; this.plugins = []; constructedViews.push(this); }
  addPlugin(plugin) { this.plugins.push(plugin); }
}
class FakeDefaultPlugin {}
const decoderCalls = [];
function FakeJapanGsiElevationDecoder() {
  const decoder = { kind: 'japanGSI', id: decoderCalls.length };
  decoderCalls.push(decoder);
  return decoder;
}
const feContext = {
  console,
  location: { origin: 'http://localhost:8080' },
  ThreeView: FakeThreeView,
  DefaultPlugin: FakeDefaultPlugin,
  JAPAN_GSI_ELEVATION_DECODER: FakeJapanGsiElevationDecoder,
};
runInContext(stripped, feContext);

/* ---- quality設定: normal 32/4, low 16/8, pixelRatio 0.5固定 ---- */
const LITE_OPTIONS = vm.runInContext('LITE_OPTIONS', feContext);
const LOW_POWER_LITE_OPTIONS = vm.runInContext('LOW_POWER_LITE_OPTIONS', feContext);
assert.strictEqual(LITE_OPTIONS.segments, 32, '通常表示のterrain segmentsは32');
assert.strictEqual(LITE_OPTIONS.maxSse, 4, '通常表示のterrain maxSseは4');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.segments, 16, '?lowのterrain segmentsは16');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.maxSse, 8, '?lowのterrain maxSseは8');
assert.ok(Object.isFrozen(LITE_OPTIONS) && Object.isFrozen(LOW_POWER_LITE_OPTIONS),
  'quality設定を含むLITE_OPTIONSは凍結されたまま');

const createPhotoJourneyView = vm.runInContext('createPhotoJourneyView', feContext);
const normalView = createPhotoJourneyView({ id: 'modelayer' }, {});
assert.strictEqual(normalView.options.pixelRatio, 0.5, '通常表示のpixelRatioは0.5');
assert.strictEqual(normalView.options.segments, 32, '通常表示はThreeViewへsegments=32を渡す');
assert.strictEqual(normalView.options.maxSse, 4, '通常表示はThreeViewへmaxSse=4を渡す');
assert.strictEqual(normalView.options.shadow, false, '通常表示でもshadowは無効のまま');
assert.strictEqual(normalView.options.useNormal, true, '初期化時に地形法線を有効化する');
const hybridView = createPhotoJourneyView({ id: 'modelayer' }, { terrainStyle: 'hybrid' });
assert.strictEqual(hybridView.options.maxSse, 3, 'hybridは注視点の詳細写真LODを優先する');
const lowHybridView = createPhotoJourneyView({ id: 'modelayer' }, { terrainStyle: 'hybrid', low: true });
assert.strictEqual(lowHybridView.options.maxSse, 8, '省電力のLOD上限を維持する');

const lowView = createPhotoJourneyView({ id: 'modelayer' }, { low: true });
assert.strictEqual(lowView.options.pixelRatio, 0.5, '?lowでもpixelRatioは0.5のまま（変更しない）');
assert.strictEqual(lowView.options.segments, 16, '?lowはThreeViewへsegments=16を渡す');
assert.strictEqual(lowView.options.maxSse, 8, '?lowはThreeViewへmaxSse=8を渡す');

/* ---- raster-dem source・terrain/hillshade layerの追加順序（normal） ---- */
function mockLayerView() {
  const sources = [];
  const layers = [];
  return {
    sources,
    layers,
    view: {
      addSource: s => { const handle = { ...s, update: () => {} }; sources.push(handle); return handle; },
      addLayer: l => { layers.push(l); },
      attribution: { add: () => {} },
    },
  };
}

const addPhotoJourneyLayers = vm.runInContext('addPhotoJourneyLayers', feContext);
const normalMock = mockLayerView();
const normalLayerState = addPhotoJourneyLayers(normalMock.view, {
  points: [{ lon: 139.7454, lat: 35.6586, title: 'Tokyo Tower' }],
  route: [],
});

const demSources = normalMock.sources.filter(s => s.type === 'raster-dem');
assert.strictEqual(demSources.length, 1, 'raster-dem sourceは1つだけ登録する');
assert.strictEqual(demSources[0].url, 'http://localhost:8080/tiles/gsi-dem/{z}/{x}/{y}.png',
  'DEM URLもserver.pyのgsi-demプロキシを指す');
assert.strictEqual(demSources[0].minZoom, 0, 'DEM sourceのminZoomは0（demgm全球）');
assert.strictEqual(demSources[0].maxZoom, 14, 'DEM sourceのmaxZoomは14（server.pyのGSI_DEM_TILE_BANDSと一致）');
assert.ok(demSources[0].elevationDecoder && decoderCalls.includes(demSources[0].elevationDecoder),
  'raster-dem sourceがJAPAN_GSI_ELEVATION_DECODER()の戻り値を使う');

const terrainLayers = normalMock.layers.filter(l => l.type === 'terrain');
assert.strictEqual(terrainLayers.length, 1, 'terrain layerは1つだけ');
assert.strictEqual(terrainLayers[0].source, demSources[0], 'terrain layerがDEM sourceを参照する');
assert.strictEqual(terrainLayers[0].terrain.lit, true, '実lightingでterrainを照らす（DEM由来のシェーディング）');
assert.strictEqual(terrainLayers[0].terrain.skirt, true, 'タイル境界の隙間をskirtで隠す');
assert.strictEqual(terrainLayers[0].terrain.castShadow, false, 'terrainはシャドウマップを投影しない');
assert.strictEqual(terrainLayers[0].terrain.receiveShadow, false, 'terrainはシャドウマップを受けない');

const hillshadeLayers = normalMock.layers.filter(l => l.type === 'raster' && l.hillshade);
assert.strictEqual(hillshadeLayers.length, 1, 'hillshade overlayは1つだけ');
assert.strictEqual(hillshadeLayers[0].source, demSources[0], 'hillshade layerも同じDEM sourceを参照する（二重フェッチさせない）');
assert.strictEqual(hillshadeLayers[0].hillshade.exaggeration, 1.35,
  'DEM由来シェーディングの強調係数は地形を読める1.35（メッシュ量は増やさない）');
assert.strictEqual(hillshadeLayers[0].raster.show, true,
  'hillshadeのスロットを非表示にして陰影まで除外しない');
assert.strictEqual(hillshadeLayers[0].raster.opacity, 0,
  '生の標高色だけを隠し、hillshadeの法線は維持する');

const imageryLayers = normalMock.layers.filter(l => l.type === 'raster' && !l.hillshade);
assert.strictEqual(imageryLayers.length, 1, '航空写真imagery layerは従来どおり1つだけ');

const terrainIndex = normalMock.layers.indexOf(terrainLayers[0]);
const hillshadeIndex = normalMock.layers.indexOf(hillshadeLayers[0]);
const imageryIndex = normalMock.layers.indexOf(imageryLayers[0]);
assert.ok(terrainIndex < imageryIndex, 'terrain layerをimagery layerより先に追加する（drape対象を確保）');
assert.ok(hillshadeIndex < imageryIndex, 'hillshade layerもimageryより先に追加する');
assert.ok(terrainIndex < hillshadeIndex, 'terrain layerをhillshade layerより先に追加する');

assert.strictEqual(normalLayerState.terrain.source, demSources[0],
  'addPhotoJourneyLayersの戻り値からDEM sourceハンドルを参照できる');

/* ---- terrainは?low相当の呼び出しでも無条件に追加される（qualityだけがcreatePhotoJourneyView側で変わる） ---- */
const lowMock = mockLayerView();
addPhotoJourneyLayers(lowMock.view, { points: [{ lon: 139.7, lat: 35.6 }], route: [] });
assert.strictEqual(lowMock.layers.filter(l => l.type === 'terrain').length, 1,
  'terrainは?low相当でも無効化されない（GSI地形は常時有効）');
assert.strictEqual(lowMock.layers.filter(l => l.type === 'raster' && l.hillshade).length, 1,
  '?low相当でも抑制hillshadeを維持する');

/* ============================================================
 * 4. static/js/photo-journey-lite.js — ambient/sunの実値をboot()実行で捕捉する
 * ============================================================ */
const controllerSrc = fs.readFileSync('static/js/photo-journey-lite.js', 'utf8');

async function flushMicrotasks() {
  for (let i = 0; i < 8; i++) await Promise.resolve();
}

async function runLightingRegression() {
  const lightCalls = [];
  const fakeView = {
    init: () => Promise.resolve(true),
    addLight: config => lightCalls.push(config),
    setCamera: () => {},
    forceUpdate: () => {},
  };
  const fakeMod = {
    createPhotoJourneyView: () => fakeView,
    setPhotoJourneyCamera: () => {},
    addPhotoJourneyLayers: () => ({ basemap: {}, terrain: {} }),
    addPhotoJourneyAircraftLayer: () => ({ id: 'aircraft-layer' }),
    updatePhotoJourneyAircraftLayer: () => {},
  };
  const playQueue = [];
  const context = {
    console, Date, Number, Math, Promise,
    performance: { now: () => 0 },
    setTimeout: () => 1, clearTimeout: () => {},
    setInterval: () => 1, clearInterval: () => {},
    window: {
      GEOFRAME_MODE_3D: true,
      GEOFRAME_PHOTO_JOURNEY_SETTINGS: {
        low: false, dwellMs: 60000, flyMs: 1, height: 1400, pitch: -24, headingStep: 0,
        orbitDelayMs: 45000, orbitMs: 8000, orbitDegrees: 22,
      },
      GEOFRAME_EVENT_STATE: { isSleeping: () => false, isEventActive: () => false, getEventUntil: () => 0 },
      addEventListener: () => {}, removeEventListener: () => {}, dispatchEvent: () => true,
    },
    document: { getElementById: id => (id === 'modelayer' ? { id } : null) },
    manifestReady: Promise.resolve(),
    PHOTOS: [{ lon: 139, lat: 35, title: 'Lighting regression point' }],
    playQueue,
    DWELL_MS: 60000,
    LOW_POWER: false,
    sessionStorage: { setItem: () => {} },
    location: { reload: () => {} },
    reindexPhotos: () => {}, loadPlayState: () => {}, syncSchedule: () => {},
    buildPlayOrder: () => { if (!playQueue.length) playQueue.push(0); },
    nextQueueIdx: () => 0, markShown: () => {}, showPhoto: () => {}, setMode: () => {},
    buildTicker: () => {}, startLivePanels: () => {},
    fakeMod,
  };
  const runtimeSource = controllerSrc.replace(
    "import('/static/navara/photo-journey-lite.js')",
    'Promise.resolve(fakeMod)',
  );
  runInContext(runtimeSource, context);
  await flushMicrotasks();

  assert.strictEqual(lightCalls.length, 2, 'ambientとsunの2回だけaddLightを呼ぶ（他の光源は追加しない）');
  // vmサンドボックス越しのオブジェクトはOutward Objectと[[Prototype]]が異なるため、
  // deepStrictEqualではなくJSON表現で比較する（既存のsetPhotoJourneyCameraテストと同じ手法）。
  assert.strictEqual(JSON.stringify(lightCalls[0]), JSON.stringify({ ambient: { intensity: 0.48 } }),
    '地形の陰影を残すambient光0.48を最初に置く');
  assert.strictEqual(JSON.stringify(lightCalls[1]),
    JSON.stringify({ sun: { intensity: 1.15, applyColor: true, castShadow: false } }),
    '地形の面を照らす太陽光1.15・applyColor有効・シャドウマップ無効を実際にaddLightへ渡す');
}

/* ============================================================
 * 5. 航空機: DEM/terrain追加が既存の絶対高度描画を変えないことの回帰確認
 * ============================================================ */
const aircraftAltitudeMeters = vm.runInContext('aircraftAltitudeMeters', feContext);
const aircraftCameraPose = vm.runInContext('aircraftCameraPose', feContext);
assert.strictEqual(aircraftAltitudeMeters({ alt_geom: 1000 }), 304.8,
  '陰性対照: DEM追加後もalt_geom(ft)→m変換はterrain非依存のまま');
const pose = aircraftCameraPose({ lat: 35, lon: 139, alt_geom: 1000, true_heading: 45 });
assert.strictEqual(pose.height, 304.8 + 8, 'MEMORY LINKの機上視点高度は引き続き実高度(alt_geom)基準');

const addPhotoJourneyAircraftLayer = vm.runInContext('addPhotoJourneyAircraftLayer', feContext);
const aircraftMock = mockLayerView();
addPhotoJourneyAircraftLayer(aircraftMock.view, {});
assert.ok(aircraftMock.layers.every(l => l.polygon.perPositionHeight === true),
  '航空機layerは引き続きperPositionHeight（頂点ごとの実高度）で描画する');
assert.ok(aircraftMock.layers.every(l => l.polygon.clampToGround === false),
  '航空機layerは新しいterrainへクランプされない（絶対高度のまま浮遊表示を維持）');

runLightingRegression()
  .then(() => console.log('gsi-dem-terrain: OK'))
  .catch(error => {
    console.error(error && error.stack ? error.stack : error);
    process.exitCode = 1;
  });
