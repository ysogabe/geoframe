/* 3D PHOTO JOURNEY Lite の新規フロント機能を、ブラウザ無しで実挙動ベースに検証する。
 *
 * 検証対象と方式:
 *  - bootstrap.js（起動分岐・一度きりのフォールバックガード）: vm 上で実実行
 *  - modes.js（#mode の唯一の writer）: vm 上で実実行
 *  - frontend/photo-journey-lite.js（軽量構成・単一raster basemap・ThreeView
 *    実利用）: import 文と export 接頭辞を除去して vm 上で関数を実呼び出し
 *     （バンドル本体は WebGL/DOM を要するため navara-build-integrity.test.js が静的に検証）
 *  - static/js/photo-journey-lite.js（3dコントローラ）: 遅延ロード・フォールバック・
 *    30s周期・showPhoto/setMode 再利用の構造をソース上で検証
 */
const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

function runInContext(source, context) {
  return vm.runInContext(source, vm.createContext(context));
}

/* ============================================================
 * bootstrap.js — 起動分岐（?mode=3d）と一度きりのフォールバック
 * ============================================================ */
const bootstrapSrc = fs.readFileSync('static/js/bootstrap.js', 'utf8');

function loadBootstrap({ search = '', fallback = null } = {}) {
  let replaced = null;
  const session = new Map();
  if (fallback !== null) session.set('geoframe.3d.fallback', fallback);
  const location = {
    search, pathname: '/', hash: '',
    replace: url => { replaced = url; },
  };
  const bodyClasses = new Set();
  const context = {
    URLSearchParams,
    console,
    location,
    sessionStorage: {
      getItem: k => (session.has(k) ? session.get(k) : null),
      setItem: (k, v) => session.set(k, String(v)),
      removeItem: k => session.delete(k),
    },
    document: { body: { classList: { add: c => bodyClasses.add(c) } } },
    window: {},
  };
  runInContext(bootstrapSrc, context);
  return { context, replaced, session, bodyClasses };
}

{
  // 通常URL（?mode=3d 無し）: Navara を一切読み込む分岐に入らない
  const r = loadBootstrap({ search: '' });
  assert.strictEqual(r.context.window.GEOFRAME_MODE_3D, false, '通常URLでは3Dにしない');
  assert.strictEqual(r.replaced, null, '通常URLではリロードしない');
  assert.deepStrictEqual([...r.bodyClasses], [], '通常URLではbody.mode3dを付けない');
}

{
  // ?mode=3d（未フォールバック）: 3D有効化してbody.mode3dを付ける
  const r = loadBootstrap({ search: '?mode=3d', fallback: null });
  assert.strictEqual(r.context.window.GEOFRAME_MODE_3D, true, '?mode=3dでは3Dにする');
  assert.ok(r.bodyClasses.has('mode3d'), '?mode=3dではbody.mode3dを付ける');
  assert.strictEqual(r.replaced, null, '?mode=3d（未失敗）ではリロードしない');
}

{
  // ?mode=3d かつ 既にフォールバック済みフラグ: modeを剥がして一度だけ通常モードへ
  const r = loadBootstrap({ search: '?mode=3d', fallback: '1' });
  assert.strictEqual(r.context.window.GEOFRAME_MODE_3D, false, 'フォールバック済みなら3Dにしない');
  assert.strictEqual(r.replaced, '/', 'mode=3dを剥がしてリロードする（?mode=が消える）');
  assert.strictEqual(r.session.has('geoframe.3d.fallback'), false, 'リロード時にフラグを消す（ループしない）');
  assert.deepStrictEqual([...r.bodyClasses], [], 'フォールバック時はmode3dを付けない');
}

{
  // ?mode=3d でフラグが '1' 以外（例: 壊れた値）: 未失敗として3Dへ進む
  const r = loadBootstrap({ search: '?mode=3d', fallback: '0' });
  assert.strictEqual(r.context.window.GEOFRAME_MODE_3D, true, 'フラグが1でなければ3Dへ進む');
  assert.strictEqual(r.replaced, null, 'フラグが1でなければリロードしない');
}

/* map.js — 3D起動時はMapLibreのrendererを生成しない */
{
  let mapCreated = false;
  const mapContext = {
    console,
    window: { GEOFRAME_MODE_3D: true },
    location: { protocol: 'http:', origin: 'http://localhost:8080' },
    document: { body: { classList: { add: () => {} } } },
    LOW_POWER: false,
    PHOTOS: [{ lon: 139.7, lat: 35.6 }],
    seeded: () => 0.5,
    maplibregl: { Map: class { constructor() { mapCreated = true; } } },
  };
  runInContext(fs.readFileSync('static/js/map.js', 'utf8'), mapContext);
  assert.strictEqual(mapCreated, false, '?mode=3dではMapLibre rendererを生成しない');
}

/* ============================================================
 * modes.js — #mode の唯一の writer / pickMode ラダー
 * ============================================================ */
{
  const el = { textContent: '' };
  const context = { document: { getElementById: id => (id === 'mode' ? el : null) }, console };
  runInContext(fs.readFileSync('static/js/modes.js', 'utf8'), context);

  const setMode = vm.runInContext('setMode', context);
  setMode('TRANSIT ▸ 東京タワー');
  assert.strictEqual(el.textContent, 'MODE: TRANSIT ▸ 東京タワー', 'setModeはMODE: 接頭辞を付ける');

  const pickMode = vm.runInContext('pickMode', context);
  assert.strictEqual(pickMode({ sleep: true }), 'SLEEP', 'SLEEPが最優先');
  assert.strictEqual(pickMode({ alert: true }), 'ALERT', 'ALERTがSLEEPに次ぐ');
  assert.strictEqual(pickMode({ pinned: true }), 'PINNED', 'PINNED');
  assert.strictEqual(pickMode({ ambient: true }), 'AMBIENT', 'AMBIENT');
  assert.strictEqual(pickMode({}), 'ROTATION', '既定はROTATION');
}

/* ============================================================
 * frontend/photo-journey-lite.js — 軽量構成・basemap変換・ThreeView実利用
 * ============================================================ */
const frontendPath = '../frontend/photo-journey-lite.js';
const frontendSrc = fs.readFileSync(frontendPath, 'utf8');

// CDN（unpkg / jsdelivr / esm.sh / 一般CDN）由来の実行時コードを一切含まないこと。
// タイルは server.py の /tiles/{gsi-photo,osm}/ プロキシを指す。
for (const bad of ['unpkg', 'jsdelivr', 'esm.sh', 'cdn.']) {
  assert.ok(!frontendSrc.toLowerCase().includes(bad), `frontendソースにCDN参照が混入: ${bad}`);
}
assert.ok(frontendSrc.includes('/tiles/gsi-photo/{z}/{x}/{y}.png'), 'GSI航空・衛星写真をプロキシから取得する');
assert.ok(frontendSrc.includes('/tiles/osm/{z}/{x}/{y}.png'), 'OSMタイルをプロキシから取得する');

// ThreeView を実際に new し、DefaultPlugin を登録する。ただし addDefaultPhotorealScene
// （SSR/SSAO/DOF/大気/雲/ボリューム）は「呼び出し」が存在しないこと。
assert.ok(/new ThreeView\s*\(/.test(frontendSrc), 'ThreeViewを実生成する');
assert.ok(/addPlugin\(\s*new DefaultPlugin\(\)\s*\)/.test(frontendSrc), 'DefaultPluginを登録する');
assert.ok(!/\.addDefaultPhotorealScene\s*\(/.test(frontendSrc), 'addDefaultPhotorealSceneを呼ばない');

// import/export を除去して vm 上で実行し、実挙動を検証する。
const stripped = frontendSrc
  .replace(/^import\s+[^\n]*\n/gm, '')
  .replace(/^export\s+/gm, '');
const constructedViews = [];
class FakeThreeView {
  constructor(options) { this.options = options; this.plugins = []; constructedViews.push(this); }
  addPlugin(plugin) { this.plugins.push(plugin); }
}
class FakeDefaultPlugin {}
const gsiDemDecoderCalls = [];
function FakeJapanGsiElevationDecoder() {
  const decoder = { kind: 'japanGSI' };
  gsiDemDecoderCalls.push(decoder);
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

const LITE_OPTIONS = vm.runInContext('LITE_OPTIONS', feContext);
assert.ok(Object.isFrozen(LITE_OPTIONS), 'LITE_OPTIONSは凍結されている');
assert.strictEqual(LITE_OPTIONS.pixelRatio, 0.65, '通常表示は第二段階として内部描画倍率0.65へ上げる');
assert.strictEqual(LITE_OPTIONS.shadow, false, '影は無効');
assert.strictEqual(LITE_OPTIONS.multisampling, 0, 'MSAA無効');
assert.strictEqual(LITE_OPTIONS.halfFloat, false, 'halfFloat無効');
assert.strictEqual(LITE_OPTIONS.animation, false, '常時60fpsではなく変更時描画');
assert.strictEqual(LITE_OPTIONS.cacheBytes, 512 * 1024 * 1024, '実測Z15へ到達する通常cacheBytes上限');
assert.strictEqual(LITE_OPTIONS.lodFog.enabled, false, '通常表示は航空写真の詳細LODを維持する');
assert.strictEqual(LITE_OPTIONS.dynamicSse.enabled, false, '通常表示では傾斜時も航空写真を粗くしない');
assert.strictEqual(LITE_OPTIONS.memoryBudget.sseMultiplierMin, 1, '通常表示の基礎SSEを劣化させない');
const LOW_POWER_LITE_OPTIONS = vm.runInContext('LOW_POWER_LITE_OPTIONS', feContext);
assert.ok(LOW_POWER_LITE_OPTIONS.pixelRatio < LITE_OPTIONS.pixelRatio,
  'LOW_POWERは内部画素倍率だけを下げる');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.segments, LITE_OPTIONS.segments,
  'LOW_POWERでも地形メッシュ分割を通常値で維持する');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.maxSse, LITE_OPTIONS.maxSse,
  'LOW_POWERでも地形LODを通常値で維持する');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.lodFog.enabled, false,
  'LOW_POWERでも地形LODを過度に粗くするfogを使わない');
assert.strictEqual(LOW_POWER_LITE_OPTIONS.dynamicSse.enabled, false,
  'LOW_POWERでも地形の動的SSEを無効にして斜面を保持する');

const createPhotoJourneyView = vm.runInContext('createPhotoJourneyView', feContext);
const lowView = createPhotoJourneyView({ id: 'modelayer' }, { low: true });
assert.strictEqual(lowView.options.animation, false, 'LOW_POWERでもon-demand描画');
assert.strictEqual(lowView.options.cacheBytes, LOW_POWER_LITE_OPTIONS.cacheBytes, 'LOW_POWER設定をThreeViewへ渡す');
assert.strictEqual(lowView.plugins.length, 1, 'DefaultPluginを1つだけ登録');

// Controller VMではNavaraの代わりに、同じfrontend純関数を実際に接続する。
const photoJourneyCameraPose = vm.runInContext('photoJourneyCameraPose', feContext);
const photoJourneyElevation = vm.runInContext('photoJourneyElevation', feContext);
const updatePhotoJourneyLighting = vm.runInContext('updatePhotoJourneyLighting', feContext);

/* ADS-B純関数 — Navara/DOMに依存しない高度・姿勢・機影生成 */
const feetToMeters = vm.runInContext('feetToMeters', feContext);
const aircraftAltitudeMeters = vm.runInContext('aircraftAltitudeMeters', feContext);
const aircraftHeading = vm.runInContext('aircraftHeading', feContext);
const aircraftPitch = vm.runInContext('aircraftPitch', feContext);
const aircraftRoll = vm.runInContext('aircraftRoll', feContext);
const aircraftFreshnessAge = vm.runInContext('aircraftFreshnessAge', feContext);
const isFreshAircraft = vm.runInContext('isFreshAircraft', feContext);
const aircraftPolygon = vm.runInContext('aircraftPolygon', feContext);
const advanceAircraft = vm.runInContext('advanceAircraft', feContext);
const aircraftGeoJSON = vm.runInContext('aircraftGeoJSON', feContext);
const aircraftChasePose = vm.runInContext('aircraftChasePose', feContext);
assert.strictEqual(feetToMeters(1000), 304.8, 'ftをmへ変換する');
assert.strictEqual(aircraftAltitudeMeters({ alt_geom: 1000, alt_baro: 1500, alt: 2000 }), 304.8,
  'alt_geomをalt_baro/alt fallbackより優先する');
assert.ok(Math.abs(aircraftAltitudeMeters({ alt_baro: 1500, alt: 2000 }) - 457.2) < 1e-9,
  'alt_geom欠損時はalt_baroへfallbackする');
assert.strictEqual(aircraftAltitudeMeters({ alt: 'ground' }), 0,
  'groundは地表高度0として安全に扱う');
assert.strictEqual(aircraftAltitudeMeters({}), 0, '欠損高度は0として安全に扱う');
assert.strictEqual(aircraftAltitudeMeters({ alt_geom: 'NaN', alt_baro: Infinity }), 0,
  '非有限高度は0として安全に扱う');
assert.strictEqual(aircraftHeading({ true_heading: 12, track: 34, mag_heading: 56 }), 12,
  'headingはtrue_headingを最優先する');
assert.strictEqual(aircraftHeading({ track: 34, mag_heading: 56 }), 34,
  'true_heading欠損時はtrackへfallbackする');
assert.strictEqual(aircraftHeading({ mag_heading: -30 }), 330,
  'true/track欠損時はmag_headingを正規化して使う');
assert.ok(Math.abs(aircraftPitch({ geom_rate: 600, gs: 120 })) <= 25,
  '推定pitchをclampする');
assert.strictEqual(aircraftPitch({ geom_rate: 600, gs: 0 }), 0,
  '速度欠損・地上ではpitchを0にする');
assert.strictEqual(aircraftRoll({ roll: 999 }), 60, 'roll上限をclampする');
assert.strictEqual(aircraftRoll({ roll: -999 }), -60, 'roll下限をclampする');
assert.strictEqual(isFreshAircraft({ seen_pos: 10 }, 1700000000), true,
  'seen_posが新鮮な機体を通す');
assert.strictEqual(isFreshAircraft({ seen_pos: 61 }, 1700000000), false,
  'seen_posが古い機体を除外する');
assert.strictEqual(aircraftFreshnessAge({ seen_pos: 50 }, 1700000000, 11), 61,
  'readsb seen_posへ最終成功feedからの経過時間を加算する');
assert.strictEqual(isFreshAircraft({ seen_pos: 50 }, 1700000000, 60, 9), true,
  'combined ageが鮮度上限以内なら機体を残す');
assert.strictEqual(isFreshAircraft({ seen_pos: 50 }, 1700000000, 60, 11), false,
  'combined ageが90秒を超えたら機体を除外する');
assert.strictEqual(aircraftGeoJSON([{ hex: 'outage', lat: 35, lon: 139, seen_pos: 50,
  alt_geom: 1000 }], { now: 1700000000, feedAgeSeconds: 11 }).features.length, 0,
  'transport outage中も凍結seen_posの機影を90秒後に消す');
const aircraft = Array.from({ length: 25 }, (_, i) => ({
  hex: `h${i}`, lat: 35 + i / 1000, lon: 139 + i / 1000,
  seen_pos: 1, alt_geom: 1000 + i, track: i,
}));
aircraft[24].seen_pos = 99;
const aircraftGJ = aircraftGeoJSON(aircraft, { now: 1700000000 });
assert.strictEqual(aircraftGJ.features.length, 20, '新鮮な機体を最大20機に制限する');
assert.strictEqual(aircraftGJ.features[0].geometry.type, 'Polygon', '機影をPolygonで作る');
assert.strictEqual(Object.prototype.hasOwnProperty.call(aircraftGJ.features[0], 'id'), false,
  'Navara互換のためFeature.idへ文字列を渡さない');
const ring = aircraftGJ.features[0].geometry.coordinates[0];
assert.ok(ring.length >= 8 && ring.length <= 12, '機影Polygonは8〜12頂点');
assert.ok(ring.every(coord => coord.length === 3 && Number.isFinite(coord[2])),
  '全座標に高度を含める');
const ringArea = ring.slice(0, -1).reduce((sum, point, i, points) => {
  const next = points[(i + 1) % points.length];
  return sum + point[0] * next[1] - next[0] * point[1];
}, 0);
assert.ok(ringArea > 0, 'GeoJSON外周はNavaraのFrontSide向けに反時計回り');
assert.strictEqual(aircraftGeoJSON(aircraft, { now: 1700000000, maxAircraft: 0 }).features.length, 0,
  'maxAircraft=0では機影を生成しない');
const rotated = aircraftGeoJSON([{ hex: 'rotation', lat: 35, lon: 139,
  seen_pos: 1, alt_geom: 1000, true_heading: 90 }], { now: 1700000000 });
assert.notStrictEqual(JSON.stringify(rotated.features[0].geometry.coordinates),
  JSON.stringify(aircraftGJ.features[0].geometry.coordinates), 'headingで機影を回転する');
const northFacing = aircraftPolygon({ lat: 35, lon: 139, alt_geom: 1000, track: 0 }, { sizeMeters: 80 });
const eastFacing = aircraftPolygon({ lat: 35, lon: 139, alt_geom: 1000, track: 90 }, { sizeMeters: 150 });
assert.ok(northFacing && eastFacing && northFacing[0][2] === 304.8,
  'Polygonの高度はalt_geomをmへ変換する');
assert.notStrictEqual(northFacing[0][0], eastFacing[0][0], 'Polygonはheadingで東西へ回転する');
const advanced = advanceAircraft({ lat: 35, lon: 139, alt_geom: 1000,
  true_heading: 90, gs: 100, geom_rate: 600 }, 2);
assert.ok(advanced.lon > 139 && advanced.alt_geom > 1000,
  'アラート中だけ短時間の速度補間を適用できる');
const aircraftCameraPose = vm.runInContext('aircraftCameraPose', feContext);
const aircraftPose = aircraftCameraPose({ lat: 35, lon: 139, alt_geom: 1000,
  true_heading: 45, geom_rate: 800, gs: 180, roll: 90 });
assert.strictEqual(aircraftPose.height, 304.8 + 8, '機上視点はalt_geom由来の高度へ置く');
assert.strictEqual(aircraftPose.heading, 45, '機上視点の方位はtrue_headingを使う');
assert.ok(aircraftPose.pitch > 0, '上昇中の機上視点はNavaraでもnose-upになる');
assert.strictEqual(aircraftPose.roll, 60, '機上視点のrollもclampする');
assert.ok(aircraftChasePose({ lat: 35, lon: 139, alt_geom: 1000 }).height > aircraftPose.height,
  'rare/emergencyは機上視点ではなく上空chase/focusへ置ける');
const chaseNorth = aircraftChasePose({ lat: 35, lon: 139, alt_geom: 1000, true_heading: 0 });
const chaseEast = aircraftChasePose({ lat: 35, lon: 139, alt_geom: 1000, true_heading: 90 });
assert.ok(chaseNorth.lat < 35 && Math.abs(chaseNorth.lng - 139) < 1e-6,
  'heading 0のchaseは機体の南側へ地理座標で移動する');
assert.ok(chaseEast.lng < 139 && Math.abs(chaseEast.lat - 35) < 1e-6,
  'heading 90のchaseは機体の西側へ地理座標で移動する');
assert.strictEqual(chaseNorth.heading, 0, 'chase視点は機体の進行方向を向く');
assert.strictEqual(aircraftChasePose({ lat: 35, lon: 139, alt_geom: 1000,
  true_heading: 0 }, { distanceM: 1, aboveM: 1, pitch: 999 }).pitch, 25,
  'chase距離・上空オフセット・pitchの上限をclampする');
assert.strictEqual(aircraftChasePose({ lat: 35, lon: 139, alt_geom: 1000,
  true_heading: 0 }, { distanceM: 99999, aboveM: 99999, pitch: -999 }).pitch, -25,
  'chase pitchの下限をclampする');

const aircraftSources = [];
const aircraftLayers = [];
const aircraftLayerHandles = [];
let aircraftForceUpdates = 0;
const aircraftView = {
  addSource: source => {
    const handle = { ...source, updates: [], deleted: false,
      update: update => handle.updates.push(update),
      delete: () => { handle.deleted = true; return true; } };
    aircraftSources.push(handle);
    return handle;
  },
  addLayer: layer => {
    aircraftLayers.push(layer);
    const handlers = new Map();
    const handle = {
      deleted: false,
      delete: () => { handle.deleted = true; return true; },
      on: (name, callback) => {
        if (!handlers.has(name)) handlers.set(name, new Set());
        handlers.get(name).add(callback);
      },
      off: (name, callback) => handlers.get(name)?.delete(callback),
      emit: (name, params) => {
        for (const callback of [...(handlers.get(name) || [])]) callback(params);
      },
      handlerCount: name => (handlers.get(name) || new Set()).size,
    };
    aircraftLayerHandles.push(handle);
    return handle;
  },
  forceUpdate: () => { aircraftForceUpdates++; },
};
const addPhotoJourneyAircraftLayer = vm.runInContext('addPhotoJourneyAircraftLayer', feContext);
const updatePhotoJourneyAircraftLayer = vm.runInContext('updatePhotoJourneyAircraftLayer', feContext);
const removePhotoJourneyAircraftLayer = vm.runInContext('removePhotoJourneyAircraftLayer', feContext);
const aircraftDiagnostics = [];
const aircraftLayerState = addPhotoJourneyAircraftLayer(aircraftView, {
  onFeatureDiagnostics: snapshot => aircraftDiagnostics.push(snapshot),
});
assert.strictEqual(aircraftSources.length, 4, '通常/マッチ/rare/emergencyの固定GeoJSON sourceを作る');
assert.strictEqual(aircraftLayers.length, 4, '航空機vector polygon layerを4層作る');
assert.ok(aircraftSources.every(s => s.type === 'geojson' && s.tiled === false),
  '全航空機GeoJSON sourceでtiled:falseを明示する');
assert.ok(aircraftLayers.every(l => l.type === 'vector' && l.polygon.perPositionHeight),
  '全航空機layerをperPositionHeightのvector polygonにする');
assert.ok(aircraftLayers.every(l => l.polygon.clampToGround === false),
  '航空機layerを地形クランプせず実高度で描画する');
assert.ok(aircraftLayers.every(l => l.polygon.tiled === false),
  '全航空機polygon materialでtiled:falseを明示する');
assert.ok(aircraftLayers.every(l => l.polygon.lit === false &&
  l.polygon.transparent === false && l.polygon.opacity === 1),
  '航空機layerを照明非依存・不透明で描画する');
assert.ok(aircraftLayers.every(l => l.polygon.outline === true &&
  l.polygon.outlineShow === true && l.polygon.outlineWidth >= 2),
  'GeoJSON専用の初期アウトラインを有効化する');
assert.strictEqual(new Set(aircraftLayers.map(l => l.polygon.color)).size, 4,
  'normal/match/rare/emergencyを別色で表示する');
const initialAircraftSources = aircraftSources.slice();
const initialAircraftLayerHandles = aircraftLayerHandles.slice();
const aircraftRows = [{ hex: 'normal', lat: 35, lon: 139,
  seen_pos: 1, alt_geom: 1000 }, { hex: 'match', lat: 35, lon: 139.1,
  seen_pos: 1, alt_geom: 2000, match: {} }, { hex: 'rare', lat: 35, lon: 139.2,
  seen_pos: 1, alt_geom: 3000, rare: true }, { hex: 'emergency', lat: 35, lon: 139.3,
  seen_pos: 1, alt_geom: 4000, emergency: true }];
updatePhotoJourneyAircraftLayer(aircraftLayerState, [], { now: 1700000000 });
assert.ok(initialAircraftSources.every(s => s.updates.length === 1 &&
  s.updates[0].data.features.length === 0),
  '初期空sourceも空FeatureCollectionで同期する');
assert.strictEqual(aircraftLayerState.needsInitialRebuild, true,
  '空source更新後も最初の実データで再構築できる状態を保持する');
assert.strictEqual(aircraftForceUpdates, 1, '空source更新後にforceUpdateを1回だけ要求する');
updatePhotoJourneyAircraftLayer(aircraftLayerState, aircraftRows, { now: 1700000000 });
assert.strictEqual(aircraftSources.length, 8, '初回の実データで4 sourceを安全に再生成する');
assert.strictEqual(aircraftLayers.length, 8, '初回の実データで4 layerを安全に再生成する');
assert.ok(initialAircraftSources.every(s => s.deleted), '再構築時に初期空sourceを削除する');
assert.ok(initialAircraftLayerHandles.every(l => l.deleted &&
  l.handlerCount('featureCreated') === 0 && l.handlerCount('featureRemoved') === 0),
  '再構築時に初期layerのfeature listenerを解除する');
const rebuiltAircraftSources = aircraftSources.slice(4);
const rebuiltAircraftLayerHandles = aircraftLayerHandles.slice(4);
assert.ok(rebuiltAircraftSources.every(s => s.type === 'geojson' && s.tiled === false &&
  s.data.type === 'FeatureCollection' && s.data.features.length === 1),
  '再生成sourceへstatus別の実GeoJSONを直接渡す');
assert.ok(rebuiltAircraftLayerHandles.every(l => l.handlerCount('featureCreated') === 1 &&
  l.handlerCount('featureRemoved') === 1),
  '再生成layerへfeature listenerを再接続する');
assert.strictEqual(aircraftLayerState.needsInitialRebuild, false,
  '実データsourceへ切り替えた後は通常更新へ移行する');
assert.strictEqual(aircraftForceUpdates, 2, '初回source再構築後にもforceUpdateを要求する');
updatePhotoJourneyAircraftLayer(aircraftLayerState, aircraftRows, { now: 1700000000 });
assert.ok(rebuiltAircraftSources.every(s => s.updates.length === 1), 'source.updateで各statusを同期する');
assert.strictEqual(aircraftForceUpdates, 3, 'source更新後にforceUpdateを1回だけ要求する');
assert.ok(rebuiltAircraftSources.every(s => s.updates[0].type === 'geojson'),
  'source.updateへGeoJSON型を明示する');
assert.ok(rebuiltAircraftSources.every(s => s.updates[0].tiled === false),
  'source.updateでもtiled:falseを維持する');
assert.strictEqual(aircraftLayerState.visibleCount, 4,
  'source更新後の鮮度・座標・最大数フィルタ済み表示数を保持する');
assert.strictEqual(aircraftLayerState.featureEventsAvailable, true,
  '4 layerすべてでfeature eventを購読できる');
rebuiltAircraftLayerHandles[0].emit('featureCreated', { featureSetId: 1n });
assert.strictEqual(aircraftLayerState.renderedFeatureCount, 1,
  'featureCreatedで実際に生成済みのfeature数を保持する');
assert.strictEqual(aircraftLayerState.featureCreatedCount, 1,
  'featureCreated診断イベント数を保持する');
assert.strictEqual(aircraftForceUpdates, 4,
  '非同期featureCreated後にも追加forceUpdateを要求する');
rebuiltAircraftLayerHandles[0].emit('featureRemoved', { featureSetId: 1n });
assert.strictEqual(aircraftLayerState.renderedFeatureCount, 0,
  'featureRemovedで描画済みfeature数を減らす');
assert.strictEqual(aircraftLayerState.featureRemovedCount, 1,
  'featureRemoved診断イベント数を保持する');
updatePhotoJourneyAircraftLayer(aircraftLayerState, [], { now: 1700000000 });
assert.strictEqual(aircraftLayerState.visibleCount, 0, '空feedでstale機影を全statusから除去する');
assert.strictEqual(aircraftDiagnostics.at(-1).renderedFeatureCount, 0,
  'feature event診断snapshotをcontroller向けcallbackへ渡す');
assert.strictEqual(removePhotoJourneyAircraftLayer(aircraftLayerState), true, 'layer/sourceを削除できる');
assert.strictEqual(aircraftLayerState.featureEventsAvailable, false,
  'disposeでfeature event監視を無効化する');
assert.ok(aircraftLayerHandles.every(l => l.handlerCount('featureCreated') === 0 &&
  l.handlerCount('featureRemoved') === 0), 'disposeでfeature listenerを解除する');
assert.strictEqual(removePhotoJourneyAircraftLayer(aircraftLayerState), false, '削除は冪等に扱う');

const setPhotoJourneyCamera = vm.runInContext('setPhotoJourneyCamera', feContext);
let initialCamera = null;
setPhotoJourneyCamera({ setCamera: cam => { initialCamera = cam; } }, {
  lng: 139.7454, lat: 35.6586, height: 4200, pitch: -24, heading: 0,
});
assert.strictEqual(JSON.stringify(initialCamera), JSON.stringify({
  lng: 139.7454, lat: 35.6586, height: 4200, heading: 0, pitch: -24, roll: 0,
}), '初回地点へ即時配置できる');

/* static/js/photo-journey-settings.js — 低頻度巡回設定を実値で検証 */
const settingsSrc = fs.readFileSync('static/js/photo-journey-settings.js', 'utf8');
function loadSettings(low) {
  const context = { console, LOW_POWER: low, DWELL_MS: 16000, window: {} };
  runInContext(settingsSrc, context);
  return context.window.GEOFRAME_PHOTO_JOURNEY_SETTINGS;
}
const normalSettings = loadSettings(false);
const lowSettings = loadSettings(true);
const NORMAL_DWELL_MS = 90000;
const LOW_DWELL_MS = 90000;
const PHOTO_ORBIT_DELAY_MS = 45000;
assert.strictEqual(normalSettings.dwellMs, NORMAL_DWELL_MS, '冷えたDEMを待てるよう通常も90秒滞在する');
assert.strictEqual(lowSettings.dwellMs, LOW_DWELL_MS, 'LOW_POWERは巡回を90秒滞在へ下げる');
assert.strictEqual(normalSettings.orbitDelayMs, PHOTO_ORBIT_DELAY_MS, '通常周回はDEM待ち後の45秒で開始する');
assert.strictEqual(lowSettings.orbitDelayMs, PHOTO_ORBIT_DELAY_MS, 'LOW_POWERもDEM待ち後の45秒で周回判定する');
assert.ok(lowSettings.flyMs < normalSettings.flyMs, 'LOW_POWERはflyTo時間を短縮');
assert.strictEqual(lowSettings.height, normalSettings.height,
  'LOW_POWERでも地形を読めるカメラ高度を維持する');
assert.strictEqual(normalSettings.height, 1400, '通常表示は地形を見渡せる実高度にする');
assert.strictEqual(normalSettings.pitch, -24, '通常表示は地平線と斜面が読める斜め視点にする');
assert.strictEqual(lowSettings.pitch, -28, '?lowでも地形が読める斜め視点を維持する');
assert.strictEqual(lowSettings.headingStep, 0, 'LOW_POWERは方位更新を止める');

// 初回地点→単一Navara raster basemap変換の実呼び出し。
const addPhotoJourneyLayers = vm.runInContext('addPhotoJourneyLayers', feContext);
const sources = [];
const layers = [];
let attributions = null;
const sourceUpdates = [];
const mockView = {
  addSource: s => {
    sources.push(s);
    return { ...s, update: update => sourceUpdates.push(update) };
  },
  addLayer: l => { layers.push(l); },
  attribution: { add: a => { attributions = a; } },
};
const points = [
  { lon: 139.7454, lat: 35.6586, title: '東京タワーの夜景' },
  { lon: 135.5013, lat: 34.6687, title: '道頓堀ネオン' },
];
const route = points;
const layerState = addPhotoJourneyLayers(mockView, {
  points,
  route,
  tileUrls: { gsiPhoto: 'http://h/tiles/gsi-photo/{z}/{x}/{y}.png', osm: 'http://h/tiles/osm/{z}/{x}/{y}.png' },
});

const raster = sources.filter(s => s.type === 'raster-tile');
assert.strictEqual(raster.length, 1, '同時に描画するraster-tile basemapは1つだけ');
assert.strictEqual(raster[0].url, 'http://h/tiles/gsi-photo/{z}/{x}/{y}.png', '国内地点ではGSI航空・衛星写真を選ぶ');
assert.strictEqual(raster[0].minZoom, 2, 'GSIシームレス写真の提供下限Z2を使う');
// GSI DEM terrain trial: raster layerはimagery(1)とDEM hillshade overlay(1)の計2つになる。
// 個々の内訳・順序・decoder呼び出しはgsi-dem-terrain.test.jsで詳しく検証する。
const imageryRasterLayers = layers.filter(l => l.type === 'raster' && !l.hillshade);
assert.strictEqual(imageryRasterLayers.length, 1, 'imagery raster layerも1つだけ');
assert.strictEqual(layerState.basemap.kind, 'gsi-photo', '初期basemapの選択状態を返す');
assert.strictEqual(layers.filter(l => l.type === 'terrain').length, 1,
  'GSI DEM地形layerを1つ追加する');
assert.ok(gsiDemDecoderCalls.length >= 1, 'raster-dem sourceでJAPAN_GSI_ELEVATION_DECODERを使う');

const updatePhotoJourneyBasemap = vm.runInContext('updatePhotoJourneyBasemap', feContext);
updatePhotoJourneyBasemap(layerState.basemap, { lon: -73.9857, lat: 40.7484 });
assert.strictEqual(sourceUpdates.length, 1, '国外へ移動したときだけ同一sourceを更新');
assert.strictEqual(sourceUpdates[0].url, 'http://h/tiles/osm/{z}/{x}/{y}.png', '国外ではOSMへ切り替える');
assert.strictEqual(layerState.basemap.kind, 'osm', 'basemap状態を更新する');

// 投機取得はDEM親タイルを先に置き、国内gsi-photo／国外OSMの小さな3x3リングを続ける。
const getPhotoJourneyPrefetchTiles = vm.runInContext('getPhotoJourneyPrefetchTiles', feContext);
const domesticWarm = getPhotoJourneyPrefetchTiles({ lon: 139.7454, lat: 35.6586 }, { origin: 'http://h' });
assert.strictEqual(domesticWarm.length, 22, '国内通常時はDEM親4枚とz15/z16の3x3リングを先読みする');
assert.strictEqual(JSON.stringify(domesticWarm.slice(0, 4).map(url => url.match(/\/gsi-dem\/(\d+)\//)[1])),
  JSON.stringify(['8', '10', '12', '14']), '国内通常時はDEMをz8/z10/z12/z14の順で先読みする');
assert.ok(domesticWarm.slice(0, 4).every(url => url.startsWith('http://h/tiles/gsi-dem/') && url.endsWith('.png')),
  '国内DEM先読みURLは同一プロキシのPNGを指す');
assert.strictEqual(domesticWarm.filter(url => url.includes('/gsi-photo/15/')).length, 9,
  '国内先読みの第一段はGSI写真z15');
assert.strictEqual(domesticWarm.filter(url => url.includes('/gsi-photo/16/')).length, 9,
  '国内先読みの精細化段はGSI写真z16');
const foreignWarm = getPhotoJourneyPrefetchTiles({ lon: -73.9857, lat: 40.7484 }, { origin: 'http://h' });
assert.strictEqual(foreignWarm.length, 11, '国外通常時は全球DEM2枚とOSMのz15 3x3を先読みする');
assert.strictEqual(JSON.stringify(foreignWarm.slice(0, 2).map(url => url.match(/\/gsi-dem\/(\d+)\//)[1])),
  JSON.stringify(['6', '8']), '国外DEM先読みは全球DEMのz6/z8に上限を置く');
assert.ok(foreignWarm.slice(0, 2).every(url => url.startsWith('http://h/tiles/gsi-dem/') && url.endsWith('.png')),
  '国外DEM先読みURLもgsi-demプロキシのPNGを指す');
assert.ok(foreignWarm.filter(url => url.includes('/gsi-dem/')).every(url => {
  const zoom = Number(url.match(/\/gsi-dem\/(\d+)\//)[1]);
  return zoom <= 8;
}), '国外DEM先読みでz8を超える標高タイルを要求しない');
assert.ok(foreignWarm.slice(2).every(url => url.includes('/tiles/osm/15/')), '国外先読みでGSI写真を要求しない');
const lowWarm = getPhotoJourneyPrefetchTiles({ lon: 139.7454, lat: 35.6586 }, { low: true, origin: 'http://h' });
assert.strictEqual(lowWarm.length, 12, 'LOW_POWERはDEM親3枚とz14の3x3を先読みする');
assert.strictEqual(JSON.stringify(lowWarm.slice(0, 3).map(url => url.match(/\/gsi-dem\/(\d+)\//)[1])),
  JSON.stringify(['8', '10', '12']), 'LOW_POWERはDEMをz8/z10/z12へ抑える');
assert.ok(lowWarm.slice(3).every(url => url.includes('/gsi-photo/14/')), 'LOW_POWERの画像先読みズームを抑える');
assert.strictEqual(getPhotoJourneyPrefetchTiles({ lon: 999, lat: 35 }, { origin: 'http://h' }).length, 0,
  '不正座標は先読みしない');

assert.strictEqual(sources.filter(s => s.type === 'geojson').length, 0,
  '全地点ルートは描かず航空写真へGPU予算を集中する');
assert.strictEqual(layers.filter(l => l.type === 'vector').length, 0,
  '全世界を覆う地点・経路線を追加しない');

assert.ok(Array.isArray(attributions) && attributions.length === 2, '選択可能なGSI/OSMの出典を登録する');
assert.ok(attributions.some(a => a.attributionUrl.includes('maps.gsi.go.jp')), 'GSI出典を表示する');
assert.ok(attributions.some(a => a.attributionUrl.includes('openstreetmap.org')), 'OSM出典を表示する');

/* ============================================================
 * static/js/photo-journey-lite.js — 3dコントローラの構造
 * ============================================================ */
const controllerSrc = fs.readFileSync('static/js/photo-journey-lite.js', 'utf8');

// 通常URLはコントローラを即時returnし、動的importにも到達しない。
runInContext(controllerSrc, { window: { GEOFRAME_MODE_3D: false } });

// 通常URLでは分岐に入らない（Navaraを読み込まない）前提のガード
assert.ok(/if\(!window\.GEOFRAME_MODE_3D\) return;/.test(controllerSrc), '3Dでなければ即returnする');
// Navara バンドルを遅延ロード（通常URLではこの行に到達しない）
assert.ok(controllerSrc.includes("import('/static/navara/photo-journey-lite.js')"), 'Navaraを遅延ロードする');
assert.ok(controllerSrc.includes("const cyberLighting = settings.low || settings.terrainStyle === 'cyber'"),
  'サイバー地形だけ照明を明るくする分岐を持つ');
assert.ok(controllerSrc.includes('{ ambient: 0.92, sun: 1.50 }') &&
  controllerSrc.includes('{ ambient: 0.48, sun: 1.15 }'),
  'サイバー/写真表示の照明値をそれぞれ固定する');
assert.ok(/view\.addLight\(\{ambient:\{intensity:lighting\.ambient\}\}\)/.test(controllerSrc),
  'ambient照明を選択した値で追加する');
assert.ok(/view\.addLight\(\{sun:\{intensity:lighting\.sun,applyColor:true,castShadow:false\}\}\)/
  .test(controllerSrc), 'GSI DEM地形を選択した太陽光で照らし、シャドウマップは作らない');
assert.ok(!controllerSrc.includes('view.lit = false'), 'unlit切替でラスタ合成を失わせない');
assert.ok(/requestLodRefinement\((view|targetView)\)/.test(controllerSrc), 'on-demand描画でも航空写真LODを精細化する');
assert.ok(controllerSrc.includes('setInterval'), '常時描画ではなく期限付きの精細化バーストを使う');
assert.ok(/const duration = settings\.low \? 60000 : \(settings\.terrainStyle === 'hybrid' \? 75000 : 55000\)/.test(controllerSrc),
  '冷えたDEMを待ち、hybridは写真フェード後も75秒まで精細化する');
assert.ok(/const interval = settings\.low \? 800 : 400/.test(controllerSrc),
  '精細化更新を通常400ms/LOW_POWER800msへ抑える');
assert.ok(/const delay=settings\.low \? 60000 : 15000/.test(controllerSrc),
  '通常15秒/LOW_POWER60秒から上限付き先読みを開始する');
assert.ok(controllerSrc.includes('getPhotoJourneyPrefetchTiles'), 'Navara側の国内/国外判定済み先読み計画を使う');
assert.ok(controllerSrc.includes("'force-cache'") && controllerSrc.includes("response?.arrayBuffer?.()"), '先読み画像の本文を取得してサーバーキャッシュへ流す');
assert.ok(controllerSrc.includes('PREFETCH_NORMAL_PER_MINUTE = 60'), '先読みを毎分60枚へ制限する');
assert.ok(controllerSrc.includes('PREFETCH_LOW_PER_MINUTE = 24'), 'LOW_POWERの先読みを毎分24枚へ制限する');
assert.ok(controllerSrc.includes("X-Tile-Cache"), 'サーバーのthrottled/errorを検知して先読み重複を解除する');
assert.ok(controllerSrc.includes('clearPrefetchWork'), 'アラート/スリープ/破棄時に投機取得を停止する');
// 一度きりのフォールバックガード（フラグを立ててリロード。bootstrap.jsがmodeを剥がす）
assert.ok(controllerSrc.includes("sessionStorage.setItem('geoframe.3d.fallback', '1')"), 'フォールバックフラグを立てる');
assert.ok(/location\.reload\(\)/.test(controllerSrc), '失敗時はリロードする');
// 低頻度カメラ遷移と、既存HUD/写真カードの再利用
assert.ok(controllerSrc.includes('settings.dwellMs'), '設定値で低頻度の滞在時間を制御する');
assert.ok(controllerSrc.includes('showPhoto(i)'), '既存の写真カードを再利用する');
assert.ok(controllerSrc.includes('startLivePanels()'),
  '3DでもHA室内環境・ADS-B・天気・在不在のライブHUDを開始する');
assert.ok(controllerSrc.includes("setMode('3D PHOTO JOURNEY LITE')"), '3D表示ラベルを明示する');
assert.ok(!controllerSrc.includes("setMode('PHOTO JOURNEY')"), '3Dで旧ラベルを使わない');
assert.ok(controllerSrc.indexOf('mod.setPhotoJourneyCamera(view') < controllerSrc.indexOf('mod.addPhotoJourneyLayers(view'),
  '初期カメラを配置してからraster layerを追加する');
assert.ok(controllerSrc.includes('forceUpdate?.()'), 'on-demand描画で初期レイヤ追加後に再描画する');
assert.ok(controllerSrc.includes('toInitialPoint(first)'), 'Navaraへ全地点ではなく初回地点だけを渡す');
assert.ok(/tick\(view,?\s*mod,?\s*first,?\s*layerState,?\s*true\)/.test(controllerSrc),
  '初回はsetCameraを維持し、同一ターンのflyToで上書きしない');
assert.ok(/function tick\([^)]*initial=false\)/.test(controllerSrc),
  '初回カメラと通常のflyToを区別する');
assert.ok(controllerSrc.includes('if(initial){'), '初回はflyToをスキップしてタイル選択を安定させる');
assert.ok(controllerSrc.includes('addPhotoJourneyAircraftLayer'), '3D起動時に航空機Polygonレイヤーを登録する');
assert.ok(controllerSrc.includes('updatePhotoJourneyAircraftLayer'), 'ADS-B更新を3Dレイヤーへ渡す');
assert.ok(controllerSrc.includes('renderedFeatureCount'),
  'aircraft-visible判定にNavaraのrenderedFeatureCountを使う');
assert.ok(controllerSrc.includes('featureEventsAvailable') && controllerSrc.includes('visibleCount'),
  'feature event非対応環境だけvisibleCountへ明示fallbackする');
assert.ok(controllerSrc.includes('onFeatureDiagnostics'),
  '非同期feature eventでcontrollerの可視状態を再同期する');
assert.ok(controllerSrc.includes('aircraftCameraPoseAt'), 'MEMORY LINKで実高度の機上視点を使う');
assert.ok(controllerSrc.includes('aircraftChasePose'), 'rare/emergencyでchase/focus視点を使う');
assert.ok(controllerSrc.includes('geoframe:presence-state'), '在席スリープイベントで3D巡回を停止・再開する');
assert.ok(controllerSrc.includes('getEventUntil'), 'eventUntilを読み取り3D巡回を停止する');
assert.ok(controllerSrc.includes('isEventActive()'), '航空機以外のイベント中も3D巡回を停止する');
assert.ok(controllerSrc.includes('clearAlertAnimation'), 'アラート終了時に補間タイマーを片付ける');
assert.ok(controllerSrc.includes('excludeHex'), '機上視点中に自機Polygonを除外する');
assert.ok(controllerSrc.includes('lastSuccessfulFeedWallTime'),
  '失敗feedで最終成功時刻を進めず鮮度を経過時間で評価する');
assert.ok(controllerSrc.includes('feedAgeSeconds'), '機影layerへfeed outage経過時間を渡す');
assert.ok(controllerSrc.includes('disposeController();\n    fallbackToMapLibre'), 'Navaraエラー時にリスナーとタイマーを破棄する');
assert.ok(controllerSrc.includes('validAircraft'), '外部alert bridgeの不正座標を3Dへ渡さない');

// MapLibreを持たない3Dで機影・地震リングを左上(0,0)へ誤投影しない。
const eventsSrc = fs.readFileSync('static/js/events.js', 'utf8');
assert.ok(/function syncAircraft\(\)\{\s*\/\/[^]*?if\(window\.GEOFRAME_MODE_3D\) return;/.test(eventsSrc),
  '3DではMapLibre用の航空機マーカーを描画しない');
assert.ok(/function syncQuakes\(\)\{\s*\/\/[^]*?if\(window\.GEOFRAME_MODE_3D\) return;/.test(eventsSrc),
  '3DではMapLibre用の地震リングを描画しない');
assert.ok(eventsSrc.includes('new EventCtor(name, {detail})'), 'events.jsと3D controllerをCustomEventで疎結合に接続する');
assert.ok(eventsSrc.includes('let eventEndTimer = null'), '航空機と地震でイベント終了タイマーを共有する');
assert.ok(eventsSrc.includes('eventEndToken'), '置換イベントで古い終了タイマーを無効化する');
assert.ok(eventsSrc.includes('AIRCRAFT_ALERT_END_EVENT'), '航空機アラート終了を明示的に通知する');
assert.ok(eventsSrc.includes("finishAircraftAlert('target-disappeared')"),
  '成功したfeedで対象が消えたとき通常モードの終了処理へ渡す');
assert.ok(controllerSrc.includes('GEOFRAME_AIRCRAFT_ALERT_END_EVENT'), 'alert-endイベント名もbridge経由で共有する');
assert.ok(!controllerSrc.includes('window.view'), 'Navara viewをmutableなwindow globalへ公開しない');

/* events.jsのCustomEvent境界を実行して検証する。古いアラートの終了callbackを
 * 手動で遅れて実行しても、新しいアラートを終了させないことを確認する。 */
{
  const emitted = [];
  const timers = new Map();
  let timerId = 0;
  const FakeEvent = class {
    constructor(type, init) { this.type = type; this.detail = init && init.detail; }
  };
  const bar = { className: '', style: { display: '' }, textContent: '' };
  const timerSet = (fn, delay) => { const id = ++timerId; timers.set(id, { fn, delay }); return id; };
  const timerClear = id => { timers.delete(id); };
  const eventContext = {
    console, Date, Number, Math, setTimeout: timerSet, clearTimeout: timerClear,
    CustomEvent: FakeEvent,
    window: {
      GEOFRAME_MODE_3D: true, CustomEvent: FakeEvent,
      dispatchEvent: event => emitted.push(event),
      GEOFRAME_EVENT_STATE: {},
    },
    map: { on: () => {}, flyTo: () => {} },
    document: { getElementById: id => id === 'alertbar' ? bar : null },
    armBase: () => {}, clearScatter: () => {}, eventCard: () => {}, setMode: () => {},
    fetch: async () => ({ ok: false }), LOW_POWER: false,
  };
  runInContext(eventsSrc, eventContext);
  const first = vm.runInContext("triggerEvent({hex:'first',lat:35,lon:139,rare:true})", eventContext);
  const firstUntil = vm.runInContext('eventUntil', eventContext);
  const firstTimer = [...timers.keys()][0];
  const firstCallback = timers.get(firstTimer).fn;
  const second = vm.runInContext("triggerEvent({hex:'second',lat:35,lon:139,rare:true})", eventContext);
  const secondUntil = vm.runInContext('eventUntil', eventContext);
  const secondTimer = [...timers.keys()][0];
  assert.strictEqual(first, true, '3Dでも航空機アラートを受け付ける');
  assert.strictEqual(second, true, '後続アラートで対象を置き換えられる');
  assert.ok(!timers.has(firstTimer), '置換時に古い終了タイマーをcancelする');
  assert.ok(timers.has(secondTimer), '最新アラートの終了タイマーだけを保持する');
  const beforeLateOldEnd = emitted.length;
  // first callback is no longer scheduled, but invoking its captured callback
  // models a delayed task from an already queued browser turn.
  firstCallback();
  assert.ok(beforeLateOldEnd >= 3, '旧alert-endと新alertの通知を発行する');
  assert.ok(timers.has(secondTimer), '遅延した旧callbackでも最新タイマーの参照を壊さない');
  assert.strictEqual(emitted.at(-1).type, 'geoframe:aircraft-alert',
    '遅れて到着した旧終了callbackで新アラートを終了しない');
  assert.strictEqual(bar.style.display, 'block',
    '遅れた旧callbackで新アラートのalertbarを隠さない');
  assert.strictEqual(vm.runInContext('window.GEOFRAME_EVENT_STATE.isEventActive()', eventContext), true,
    'eventUntilをread-only stateとして公開する');
  timers.get(secondTimer).fn();
  assert.strictEqual(emitted.at(-1).type, 'geoframe:aircraft-alert-end',
    '最新アラートの終了だけを通知する');
}

// 通常(MapLibre)モードでも、既存のflyToとイベント演出は維持する。
{
  let flyCalls = 0;
  let normalMode = '';
  const normalBar = { className: '', style: { display: '' }, textContent: '' };
  const normalEvents = [];
  const normalContext = {
    console, Date, Number, Math, setTimeout: () => 1, clearTimeout: () => {},
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init.detail; } },
    window: {
      GEOFRAME_MODE_3D: false,
      dispatchEvent: event => normalEvents.push(event),
      GEOFRAME_EVENT_STATE: {},
    },
    map: { on: () => {}, flyTo: () => { flyCalls++; } },
    document: { getElementById: id => id === 'alertbar' ? normalBar :
      ({ className: '', style: {}, textContent: '' }) },
    armBase: () => {}, clearScatter: () => {}, eventCard: () => {},
    setMode: mode => { normalMode = mode; },
    fetch: async () => ({ ok: false }), LOW_POWER: false,
  };
  runInContext(eventsSrc, normalContext);
  assert.strictEqual(vm.runInContext("triggerEvent({hex:'normal',lat:35,lon:139,rare:true})", normalContext), true,
    '通常モードの航空機イベントも成功する');
  assert.strictEqual(flyCalls, 1, '通常モードのMapLibre flyToを維持する');
  assert.strictEqual(normalBar.style.display, 'block', '通常モードのalertbarを表示する');
  assert.strictEqual(normalMode, 'INTERCEPT', '通常モードのイベントmodeを設定する');
  assert.strictEqual(vm.runInContext("finishAircraftAlert('target-disappeared')", normalContext), true,
    '通常モードでも対象消失をアラート終了として処理する');
  assert.strictEqual(normalBar.style.display, 'none', '対象消失時に通常モードのalertbarを隠す');
  assert.strictEqual(normalMode, 'PHOTO JOURNEY', '対象消失時に通常モードへ戻す');
  assert.strictEqual(vm.runInContext('window.GEOFRAME_EVENT_STATE.isEventActive()', normalContext), false,
    '対象消失時に通常モードのイベント状態を解除する');
  assert.strictEqual(normalEvents.at(-1).detail.reason, 'target-disappeared',
    '対象消失理由をbridgeへ通知する');
}

// 通常/3Dの両起動経路が同じ一度きりのライブHUD入口を使う。
const panelsSrc = fs.readFileSync('static/js/panels.js', 'utf8');
const mainSrc = fs.readFileSync('static/js/main.js', 'utf8');
assert.ok(/let livePanelsStarted = false;/.test(panelsSrc), 'ライブHUD開始処理に二重起動ガードがある');
for(const call of ['pollAircraft()', 'loadEnv()', 'pollPresence()', 'pollRoomEnv()'])
  assert.ok(panelsSrc.includes(call), `ライブHUD入口が${call}を開始する`);
assert.ok(mainSrc.includes('startLivePanels()'), '通常モードも共通のライブHUD入口を使う');
assert.ok(panelsSrc.includes("else if(!window.GEOFRAME_MODE_3D) setMode('PHOTO JOURNEY')"),
  '3Dのwakeで通常モードのラベルを上書きしない');

/* 3D controllerのbridgeを実際に起動するVM回帰テスト。
 * alert中はjourney/refinementを止め、feed失敗では消失扱いにせず、成功feedで対象が
 * 消えた場合だけalert intervalを即時破棄してjourneyを再開することを確認する。 */
async function flushControllerMicrotasks(){
  for(let i=0; i<12; i++) await Promise.resolve();
}

async function runControllerBridgeRuntimeRegression(){
  const clockSeconds = { value: 1700000000 };
  const listeners = new Map();
  const timeouts = new Map();
  const intervals = new Map();
  let timerId = 0;
  const timerSet = (fn, delay) => {
    const id = ++timerId;
    timeouts.set(id, { fn, delay });
    return id;
  };
  const timerClear = id => { timeouts.delete(id); };
  const intervalSet = (fn, delay) => {
    const id = ++timerId;
    intervals.set(id, { fn, delay });
    return id;
  };
  const intervalClear = id => { intervals.delete(id); };
  const fakeWindow = {
    GEOFRAME_MODE_3D: true,
    LOW_POWER: true,
    GEOFRAME_PHOTO_JOURNEY_SETTINGS: {
      low: true, dwellMs: LOW_DWELL_MS, flyMs: 1, height: 1400, pitch: -28,
      headingStep: 0, orbitDelayMs: PHOTO_ORBIT_DELAY_MS,
    },
    GEOFRAME_EVENT_STATE: {
      isSleeping: () => false,
      isEventActive: () => false,
      getEventUntil: () => 0,
    },
    addEventListener: (name, handler) => {
      if(!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(handler);
    },
    removeEventListener: (name, handler) => listeners.get(name)?.delete(handler),
    dispatchEvent: event => {
      for(const handler of [...(listeners.get(event.type) || [])]) handler(event);
      return true;
    },
  };
  const cameraCalls = [];
  let terrainSamples = 0;
  const fakeView = {
    init: () => Promise.resolve(true),
    addLight: () => {},
    setCamera: pose => cameraCalls.push(pose),
    forceUpdate: () => {},
    sampleTerrainHeight: () => { terrainSamples++; return 123.5; },
    atmosphere: {},
  };
  const target = {
    hex: 'runtime-target', lat: 35, lon: 139, seen_pos: 10,
    alt_geom: 1000, true_heading: 0, gs: 100, geom_rate: 0,
  };
  let removed = false;
  let layerUpdates = 0;
  const layerFeedAges = [];
  const layerRowCounts = [];
  const layerVisibleCounts = [];
  const fakeMod = {
    createPhotoJourneyView: () => fakeView,
    setPhotoJourneyCamera: (view, pose) => view.setCamera(pose),
    photoJourneyCameraPose,
    photoJourneyElevation,
    updatePhotoJourneyLighting,
    addPhotoJourneyLayers: () => ({ basemap: {} }),
    addPhotoJourneyAircraftLayer: () => ({ id: 'aircraft-layer' }),
    updatePhotoJourneyAircraftLayer: (state, rows, options = {}) => {
      layerUpdates++;
      layerFeedAges.push(options.feedAgeSeconds);
      layerRowCounts.push(Array.isArray(rows) ? rows.length : 0);
      layerVisibleCounts.push(aircraftGeoJSON(rows, {
        now: options.now, maxAgeSeconds: options.maxAgeSeconds,
        maxAircraft: options.maxAircraft, feedAgeSeconds: options.feedAgeSeconds,
        excludeHex: options.excludeHex,
      }).features.length);
    },
    removePhotoJourneyAircraftLayer: () => { removed = true; },
    updatePhotoJourneyBasemap: () => {},
    flyPhotoJourneyCamera: () => Promise.resolve(true),
    advanceAircraft: aircraft => ({ ...aircraft }),
    aircraftApproachPose: aircraft => ({ lng: aircraft.lon, lat: aircraft.lat,
      height: 700, heading: 0, pitch: -25, roll: 0 }),
    aircraftCameraPoseAt: aircraft => ({ lng: aircraft.lon, lat: aircraft.lat,
      height: 312.8, heading: 0, pitch: 0, roll: 0 }),
    aircraftChasePose: aircraft => ({ lng: aircraft.lon - 0.01, lat: aircraft.lat,
      height: 500, heading: 0, pitch: -12, roll: 0 }),
  };
  const playQueue = [];
  const runtimeContext = {
    console, Date: { now: () => clockSeconds.value * 1000 }, Number, Math, Promise,
    performance: { now: () => clockSeconds.value * 1000 },
    setTimeout: timerSet, clearTimeout: timerClear,
    setInterval: intervalSet, clearInterval: intervalClear,
    window: fakeWindow,
    document: { getElementById: id => id === 'modelayer' ? { id } : null },
    manifestReady: Promise.resolve(),
    PHOTOS: [{ lon: 139, lat: 35, title: 'Runtime point' }],
    playQueue,
    DWELL_MS: LOW_DWELL_MS,
    LOW_POWER: true,
    fakeMod,
    sessionStorage: { setItem: () => {} },
    location: { reload: () => {} },
    reindexPhotos: () => {},
    loadPlayState: () => {},
    syncSchedule: () => {},
    buildPlayOrder: () => { if(!playQueue.length) playQueue.push(0); },
    nextQueueIdx: () => 0,
    markShown: () => {},
    showPhoto: () => {},
    setMode: () => {},
    buildTicker: () => {},
    startLivePanels: () => {},
  };
  const runtimeVm = vm.createContext(runtimeContext);
  const runtimeSource = controllerSrc.replace(
    "import('/static/navara/photo-journey-lite.js')",
    'Promise.resolve(fakeMod)',
  );
  vm.runInContext(runtimeSource, runtimeVm);
  await flushControllerMicrotasks();
  assert.ok(cameraCalls.length >= 1, 'controller VMが初期カメラを設定する');
  assert.strictEqual(intervals.size, 1, '初期LOD精細化だけがintervalを持つ');
  assert.strictEqual(timeouts.size, 2, '通常巡回はdwell timerと遅延標高再評価timerを持つ');
  assert.ok([...timeouts.values()].some(timer => timer.delay === LOW_DWELL_MS),
    '通常巡回のdwell timerを設定する');
  assert.ok([...timeouts.values()].some(timer => timer.delay === PHOTO_ORBIT_DELAY_MS),
    '通常巡回の標高再評価を45秒後へ遅延する');
  const lowOrbitDelayEntry = [...timeouts.entries()].find(([, timer]) => timer.delay === PHOTO_ORBIT_DELAY_MS);
  timeouts.delete(lowOrbitDelayEntry[0]);
  lowOrbitDelayEntry[1].fn();
  await flushControllerMicrotasks();
  assert.ok(terrainSamples >= 2, '初期配置後にDEM標高を再評価する');
  assert.strictEqual([...intervals.values()].filter(timer => timer.delay === 50).length, 0,
    'LOW_POWERでは遅延標高再評価後も周回intervalを作らない');

  const dispatch = (type, detail) => fakeWindow.dispatchEvent({ type, detail });
  dispatch('geoframe:aircraft-update', {
    aircraft: [target], now: clockSeconds.value, ok: true, feedAvailable: true,
  });
  clockSeconds.value += 55;
  dispatch('geoframe:aircraft-update', {
    aircraft: [], now: clockSeconds.value, ok: false, feedAvailable: false,
  });
  assert.strictEqual(layerRowCounts.at(-1), 1,
    '失敗feedでは最終成功snapshotをcontrollerが保持する');
  assert.ok(layerFeedAges.at(-1) >= 54.99, '失敗feedでは最終成功feedからの経過時間を渡す');
  assert.strictEqual(layerVisibleCounts.at(-1), 0,
    'seen_posが凍結してもcombined age超過で通常機影を消す');

  const firstUntil = clockSeconds.value * 1000 + 20000;
  dispatch('geoframe:aircraft-alert', { aircraft: target, kind: 'match', until: firstUntil });
  assert.strictEqual(timeouts.size, 0, 'alert開始時にjourney dwellをcancelする');
  assert.strictEqual(intervals.size, 0, 'alert開始時にLOD refinementもcancelする');
  await flushControllerMicrotasks();
  assert.strictEqual(intervals.size, 1, 'alert中は補間intervalを1本だけ動かす');
  assert.ok(cameraCalls.length >= 2, 'alertのfocus後に再構成視点を設定する');

  // Transport failure must not be interpreted as an aircraft disappearance.
  clockSeconds.value += 1;
  dispatch('geoframe:aircraft-update', {
    aircraft: [], now: clockSeconds.value, ok: false, feedAvailable: false,
  });
  assert.strictEqual(intervals.size, 1, 'feed失敗ではalert補間を継続する');
  assert.strictEqual(layerRowCounts.at(-1), 1, 'alert中も失敗feedでsnapshotを置き換えない');
  assert.ok(layerFeedAges.at(-1) >= 55.99, 'alert中もfeed outage ageを積算する');

  dispatch('geoframe:aircraft-alert-end', { until: firstUntil, reason: 'ended' });
  assert.strictEqual(intervals.size, 0, 'alert終了で補間intervalを破棄する');
  assert.strictEqual(timeouts.size, 1, 'alert終了でjourney resume timerを1本だけ作る');
  const resumeTask = [...timeouts.entries()][0];
  timeouts.delete(resumeTask[0]);
  resumeTask[1].fn();
  await flushControllerMicrotasks();

  // A successful empty feed is a deterministic target disappearance.
  dispatch('geoframe:aircraft-update', {
    aircraft: [target], now: clockSeconds.value, ok: true, feedAvailable: true,
  });
  const secondUntil = clockSeconds.value * 1000 + 20000;
  dispatch('geoframe:aircraft-alert', { aircraft: target, kind: 'rare', until: secondUntil });
  await flushControllerMicrotasks();
  assert.strictEqual(intervals.size, 1, 'replacement alertも補間intervalを1本だけ動かす');
  dispatch('geoframe:aircraft-update', {
    aircraft: [], now: clockSeconds.value, ok: true, feedAvailable: true,
  });
  assert.strictEqual(intervals.size, 0, '対象消失時は4〜5Hz intervalを即時破棄する');
  assert.strictEqual(timeouts.size, 1, '対象消失時はjourney resume timerをリークなく作る');
  assert.ok(layerUpdates > 0, 'alert終了時に自機除外後の機影layerを再同期する');

  fakeWindow.dispatchEvent({ type: 'pagehide', detail: null });
  assert.strictEqual(timeouts.size, 0, 'disposeでjourney timerを破棄する');
  assert.strictEqual(intervals.size, 0, 'disposeでintervalをリークしない');
  assert.strictEqual(removed, true, 'disposeで航空機layerを安全に削除する');
}

/* 写真滞在のライフサイクルを実タイマー相当のVMで検証する。単なる文字列検査ではなく、
 * 通常時の20Hz/8秒周回、LOW_POWER・reduced-motionの周回抑止、途中pause、dispose後の
 * 遅延callback無効化までを、実際のphotoJourneyCameraPose/Elevationへ接続して確認する。 */
async function runPhotoDwellLifecycleRegression(){
  async function createScenario({ low = false, reduced = false, deferredFly = false, knownElevation = false } = {}){
    const clock = { ms: 1700000000000 };
    const listeners = new Map();
    const timeouts = new Map();
    const intervals = new Map();
    const state = { eventActive: false };
    const pendingFlights = [];
    let timerId = 0;
    const timerSet = (fn, delay) => {
      const id = ++timerId;
      timeouts.set(id, { fn, delay });
      return id;
    };
    const timerClear = id => { timeouts.delete(id); };
    const intervalSet = (fn, delay) => {
      const id = ++timerId;
      intervals.set(id, { fn, delay });
      return id;
    };
    const intervalClear = id => { intervals.delete(id); };
    const body = {
      classList: { toggle: () => {}, remove: () => {} },
      dataset: {},
    };
    const fakeWindow = {
      GEOFRAME_MODE_3D: true,
      LOW_POWER: low,
      GEOFRAME_PHOTO_JOURNEY_SETTINGS: {
        low, dwellMs: low ? LOW_DWELL_MS : NORMAL_DWELL_MS, flyMs: 1, height: 1400,
        pitch: low ? -28 : -24, headingStep: 0,
        orbitDelayMs: PHOTO_ORBIT_DELAY_MS, orbitMs: 8000, orbitDegrees: 22,
      },
      GEOFRAME_EVENT_STATE: {
        isSleeping: () => false,
        isEventActive: () => state.eventActive,
        getEventUntil: () => 0,
      },
      matchMedia: () => ({ matches: reduced }),
      addEventListener: (name, handler) => {
        if (!listeners.has(name)) listeners.set(name, new Set());
        listeners.get(name).add(handler);
      },
      removeEventListener: (name, handler) => listeners.get(name)?.delete(handler),
      dispatchEvent: event => {
        for (const handler of [...(listeners.get(event.type) || [])]) handler(event);
        return true;
      },
    };
    const cameraCalls = [];
    let terrainSamples = 0;
    let removed = false;
    const fakeView = {
      init: () => Promise.resolve(true),
      addLight: () => {},
      setCamera: pose => cameraCalls.push(pose),
      forceUpdate: () => {},
      sampleTerrainHeight: () => { terrainSamples++; return 111; },
      atmosphere: {},
    };
    const fakeMod = {
      createPhotoJourneyView: () => fakeView,
      setPhotoJourneyCamera: (view, pose) => view.setCamera(pose),
      photoJourneyCameraPose,
      photoJourneyElevation,
      updatePhotoJourneyLighting,
      addPhotoJourneyLayers: () => ({ basemap: {}, terrain: {} }),
      addPhotoJourneyAircraftLayer: () => ({ id: 'aircraft-layer' }),
      updatePhotoJourneyAircraftLayer: () => {},
      removePhotoJourneyAircraftLayer: () => { removed = true; },
      updatePhotoJourneyBasemap: () => {},
      flyPhotoJourneyCamera: () => deferredFly
        ? new Promise(resolve => pendingFlights.push(resolve))
        : Promise.resolve(true),
      advanceAircraft: aircraft => ({ ...aircraft }),
      aircraftApproachPose: aircraft => ({ lng: aircraft.lon, lat: aircraft.lat,
        height: 700, heading: 0, pitch: -25, roll: 0 }),
      aircraftChasePose: aircraft => ({ lng: aircraft.lon, lat: aircraft.lat,
        height: 500, heading: 0, pitch: -12, roll: 0 }),
    };
    const playQueue = [];
    const runtimeContext = {
      console,
      Date: { now: () => clock.ms },
      Number, Math, Promise,
      performance: { now: () => clock.ms },
      setTimeout: timerSet, clearTimeout: timerClear,
      setInterval: intervalSet, clearInterval: intervalClear,
      window: fakeWindow,
      document: {
        body,
        getElementById: id => id === 'modelayer' ? { id } : null,
      },
      manifestReady: Promise.resolve(),
      PHOTOS: [{ lon: 139, lat: 35, title: 'Dwell point', realElev: knownElevation, intel: { elev: 88 } }],
      playQueue,
      DWELL_MS: low ? LOW_DWELL_MS : NORMAL_DWELL_MS,
      LOW_POWER: low,
      fakeMod,
      reindexPhotos: () => {},
      loadPlayState: () => {},
      syncSchedule: () => {},
      buildPlayOrder: () => { if (!playQueue.length) playQueue.push(0); },
      nextQueueIdx: () => 0,
      markShown: () => {},
      showPhoto: () => {},
      setMode: () => {},
      buildTicker: () => {},
      startLivePanels: () => {},
    };
    const runtimeVm = vm.createContext(runtimeContext);
    const runtimeSource = controllerSrc.replace(
      "import('/static/navara/photo-journey-lite.js')",
      'Promise.resolve(fakeMod)',
    );
    vm.runInContext(runtimeSource, runtimeVm);
    await flushControllerMicrotasks();

    const invokeTimeout = predicate => {
      const entry = [...timeouts.entries()].find(([, timer]) => predicate(timer));
      assert.ok(entry, '期待する遅延timerを登録する');
      timeouts.delete(entry[0]);
      entry[1].fn();
      return entry[1];
    };
    const invokeInterval = predicate => {
      const entry = [...intervals.entries()].find(([, timer]) => predicate(timer));
      assert.ok(entry, '期待するintervalを登録する');
      entry[1].fn();
      return entry[1];
    };
    const dispatch = (type, detail) => fakeWindow.dispatchEvent({ type, detail });
    const resolveNextFlight = (value = true) => {
      const resolve = pendingFlights.shift();
      assert.ok(resolve, '未完了flyToのresolve待ちを保持する');
      resolve(value);
    };
    return {
      clock, timeouts, intervals, cameraCalls, terrainSamples: () => terrainSamples,
      removed: () => removed, invokeTimeout, invokeInterval, dispatch,
      setEventActive: value => { state.eventActive = value; },
      pendingFlights, resolveNextFlight,
    };
  }

  const known = await createScenario({ knownElevation: true });
  known.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  assert.strictEqual(known.terrainSamples(), 0,
    '実標高のある写真は初期配置・周回とも粗いDEMで高度を上書きしない');
  assert.strictEqual(known.cameraCalls.at(-1).height, 1488,
    '標高88mの撮影地点から1400m上空を保つ');
  known.dispatch('pagehide', null);

  const low = await createScenario({ low: true });
  assert.strictEqual(low.timeouts.size, 2, 'LOW_POWERでもdwellと標高再評価の2 timerを持つ');
  low.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  assert.ok(low.terrainSamples() >= 2, 'LOW_POWERでも遅延DEM標高再評価を実行する');
  assert.strictEqual([...low.intervals.values()].filter(timer => timer.delay === 50).length, 0,
    'LOW_POWERでは遅延処理後も周回を開始しない');
  low.dispatch('pagehide', null);
  assert.strictEqual(low.timeouts.size, 0, 'LOW_POWERのdisposeで残りのdwell timerを破棄する');
  assert.strictEqual(low.intervals.size, 0, 'LOW_POWERのdisposeでLOD intervalを破棄する');

  const normal = await createScenario();
  const normalBeforeOrbit = normal.cameraCalls.length;
  normal.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  assert.ok(normal.terrainSamples() >= 2, '通常時も遅延DEM標高再評価を実行する');
  const orbit = normal.invokeInterval(timer => timer.delay === 50);
  assert.strictEqual(orbit.delay, 50, '通常周回は20Hz（50ms）で更新する');
  assert.ok(normal.cameraCalls.length > normalBeforeOrbit, '周回開始時に再計算したカメラ姿勢を設定する');
  normal.clock.ms += 4000;
  normal.invokeInterval(timer => timer.delay === 50);
  assert.ok(normal.intervals.size >= 2, '8秒周回の途中ではLODと周回intervalを維持する');
  normal.clock.ms += 4000;
  normal.invokeInterval(timer => timer.delay === 50);
  assert.strictEqual([...normal.intervals.values()].filter(timer => timer.delay === 50).length, 0,
    '通常周回は8秒で終了し、周回intervalを残さない');
  normal.dispatch('pagehide', null);
  assert.strictEqual(normal.timeouts.size, 0, '通常時のdisposeでdwell timerを破棄する');
  assert.strictEqual(normal.intervals.size, 0, '通常時のdisposeでLOD intervalを破棄する');
  assert.strictEqual(normal.removed(), true, '通常時のdisposeで航空機layerを削除する');

  const reduced = await createScenario({ reduced: true });
  reduced.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  assert.strictEqual([...reduced.intervals.values()].filter(timer => timer.delay === 50).length, 0,
    'prefers-reduced-motionでは通常設定でも周回intervalを開始しない');
  reduced.dispatch('pagehide', null);

  const paused = await createScenario();
  paused.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  assert.ok([...paused.intervals.values()].some(timer => timer.delay === 50),
    'pause前は通常周回intervalが稼働している');
  const staleOrbitTick = [...paused.intervals.values()].find(timer => timer.delay === 50).fn;
  paused.dispatch('geoframe:presence-state', { sleeping: true });
  assert.strictEqual(paused.timeouts.size, 0, '途中pauseで残りのdwell timerを破棄する');
  assert.strictEqual(paused.intervals.size, 0, '途中pauseで周回とLOD intervalを停止する');
  const pausedCameraCount = paused.cameraCalls.length;
  staleOrbitTick();
  assert.strictEqual(paused.cameraCalls.length, pausedCameraCount,
    'pause後に遅れて到着した周回tickはカメラを動かさない');
  paused.dispatch('geoframe:presence-state', { sleeping: false });
  assert.strictEqual(paused.timeouts.size, 1, 'wake後はjourney再開deadlineだけを作る');
  assert.strictEqual([...paused.timeouts.values()][0].delay, 150, 'wake後の再開deadlineを150msへ抑える');
  paused.dispatch('pagehide', null);
  assert.strictEqual(paused.timeouts.size, 0, 'wake後disposeでも再開deadlineを破棄する');

  const disposedDuringOrbit = await createScenario();
  disposedDuringOrbit.invokeTimeout(timer => timer.delay === PHOTO_ORBIT_DELAY_MS);
  const staleDisposedTick = [...disposedDuringOrbit.intervals.values()]
    .find(timer => timer.delay === 50).fn;
  const disposedCameraCount = disposedDuringOrbit.cameraCalls.length;
  disposedDuringOrbit.dispatch('pagehide', null);
  assert.strictEqual(disposedDuringOrbit.timeouts.size, 0,
    '周回中のdisposeでdwell timerを破棄する');
  assert.strictEqual(disposedDuringOrbit.intervals.size, 0,
    '周回中のdisposeで周回とLOD intervalを停止する');
  assert.strictEqual(disposedDuringOrbit.removed(), true,
    '周回中のdisposeで航空機layerを削除する');
  staleDisposedTick();
  assert.strictEqual(disposedDuringOrbit.cameraCalls.length, disposedCameraCount,
    'dispose後に遅れて到着した周回tickはカメラを動かさない');

  /* 通常flyTo完了時のstartPhotoDwellを、未完了Promiseでイベント境界ごとに検証する。 */
  const genericEvent = await createScenario({ deferredFly: true });
  genericEvent.invokeTimeout(timer => timer.delay === NORMAL_DWELL_MS);
  assert.strictEqual(genericEvent.pendingFlights.length, 1,
    '通常巡回の非初回flyToを未完了Promiseとして保持する');
  genericEvent.setEventActive(true);
  genericEvent.resolveNextFlight();
  await flushControllerMicrotasks();
  assert.strictEqual(genericEvent.timeouts.size, 1,
    'generic event中のfly完了でも巡回再開用journey timerを保持する');
  assert.strictEqual([...genericEvent.timeouts.values()][0].delay, 1000,
    'generic event中は周回を始めず、1秒後のevent再確認deadlineを残す');
  assert.strictEqual([...genericEvent.timeouts.values()].filter(timer => timer.delay === PHOTO_ORBIT_DELAY_MS).length, 0,
    'generic event中に遅延orbit timerを作らない');
  genericEvent.setEventActive(false);
  genericEvent.invokeTimeout(timer => timer.delay === 1000);
  genericEvent.invokeTimeout(timer => timer.delay === 0);
  assert.strictEqual(genericEvent.pendingFlights.length, 1,
    'generic event解除後に保留したjourney timerから新しいflyToを開始する');
  genericEvent.resolveNextFlight();
  await flushControllerMicrotasks();
  assert.strictEqual([...genericEvent.timeouts.values()].filter(timer => timer.delay === PHOTO_ORBIT_DELAY_MS).length, 1,
    'generic event解除後の新しいfly完了で周回準備を再開する');
  genericEvent.dispatch('pagehide', null);

  const alertDuringFlight = await createScenario({ deferredFly: true });
  alertDuringFlight.invokeTimeout(timer => timer.delay === NORMAL_DWELL_MS);
  alertDuringFlight.dispatch('geoframe:aircraft-alert', {
    aircraft: { hex: 'deferred-alert', lat: 35, lon: 139, seen_pos: 1 },
    kind: 'rare', until: 1700000030000,
  });
  assert.strictEqual(alertDuringFlight.pendingFlights.length, 2,
    'alert開始時に旧flyToとalert focus flyToを別々に保持する');
  alertDuringFlight.resolveNextFlight();
  await flushControllerMicrotasks();
  assert.strictEqual(alertDuringFlight.timeouts.size, 0,
    'alert開始後に遅れて完了した旧flyToがdwell/orbitを再起動しない');
  alertDuringFlight.resolveNextFlight();
  await flushControllerMicrotasks();
  assert.ok([...alertDuringFlight.intervals.values()].some(timer => timer.delay === 200),
    '新しいalertのfocus flyTo完了後だけalert補間を開始する');
  alertDuringFlight.dispatch('geoframe:aircraft-alert-end', { until: 1700000030000 });
  assert.strictEqual([...alertDuringFlight.timeouts.values()].filter(timer => timer.delay === NORMAL_DWELL_MS).length, 0,
    'alert終了後に旧flyTo由来のdwell timerを残さない');
  alertDuringFlight.dispatch('pagehide', null);

  const sleepDuringFlight = await createScenario({ deferredFly: true });
  sleepDuringFlight.invokeTimeout(timer => timer.delay === NORMAL_DWELL_MS);
  sleepDuringFlight.dispatch('geoframe:presence-state', { sleeping: true });
  sleepDuringFlight.resolveNextFlight();
  await flushControllerMicrotasks();
  assert.strictEqual(sleepDuringFlight.timeouts.size, 0,
    'sleep中に遅れて完了した旧flyToがdwell/orbitを再起動しない');
  assert.strictEqual(sleepDuringFlight.intervals.size, 0,
    'sleep中は旧flyTo完了後もLOD intervalを再起動しない');
  sleepDuringFlight.dispatch('geoframe:presence-state', { sleeping: false });
  assert.strictEqual(sleepDuringFlight.timeouts.size, 1,
    'wake後は新規journey再開deadlineだけを作る');
  assert.strictEqual([...sleepDuringFlight.timeouts.values()][0].delay, 150,
    'wake後のjourney再開deadlineを150msへ設定する');
  sleepDuringFlight.dispatch('pagehide', null);
}

runControllerBridgeRuntimeRegression()
  .then(() => runPhotoDwellLifecycleRegression())
  .then(() => console.log('photo-journey-lite: OK'))
  .catch(error => {
    console.error(error && error.stack ? error.stack : error);
    process.exitCode = 1;
  });
