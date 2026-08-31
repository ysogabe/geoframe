/* 3D PHOTO JOURNEY Lite の新規フロント機能を、ブラウザ無しで実挙動ベースに検証する。
 *
 * 検証対象と方式:
 *  - bootstrap.js（起動分岐・一度きりのフォールバックガード）: vm 上で実実行
 *  - modes.js（#mode の唯一の writer）: vm 上で実実行
 *  - frontend/photo-journey-lite.js（軽量構成・manifest→ポイント/ルート変換・ThreeView
 *    実利用）: import 文と export 接頭辞を除去して vm 上で関数を実呼び出し
 *     （バンドル本体は WebGL/DOM を要するため navara-build-integrity.test.js が静的に検証）
 *  - static/js/photo-journey-lite.js（3dコントローラ）: 遅延ロード・フォールバック・
 *    ~16s周期・showPhoto/setMode 再利用の構造をソース上で検証
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
 * frontend/photo-journey-lite.js — 軽量構成・変換・ThreeView実利用
 * ============================================================ */
const frontendPath = '../frontend/photo-journey-lite.js';
const frontendSrc = fs.readFileSync(frontendPath, 'utf8');

// CDN（unpkg / jsdelivr / esm.sh / 一般CDN）由来の実行時コードを一切含まないこと。
// タイルは server.py の /tiles/{gsi,osm}/ プロキシを指す。
for (const bad of ['unpkg', 'jsdelivr', 'esm.sh', 'cdn.']) {
  assert.ok(!frontendSrc.toLowerCase().includes(bad), `frontendソースにCDN参照が混入: ${bad}`);
}
assert.ok(frontendSrc.includes('/tiles/gsi/{z}/{x}/{y}.png'), 'GSIタイルをプロキシから取得する');
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
const feContext = {
  console,
  location: { origin: 'http://localhost:8080' },
  ThreeView: FakeThreeView,
  DefaultPlugin: FakeDefaultPlugin,
};
runInContext(stripped, feContext);

const LITE_OPTIONS = vm.runInContext('LITE_OPTIONS', feContext);
assert.ok(Object.isFrozen(LITE_OPTIONS), 'LITE_OPTIONSは凍結されている');
assert.strictEqual(LITE_OPTIONS.pixelRatio, 1, 'devicePixelRatio上限は1');
assert.strictEqual(LITE_OPTIONS.shadow, false, '影は無効');
assert.strictEqual(LITE_OPTIONS.multisampling, 0, 'MSAA無効');
assert.strictEqual(LITE_OPTIONS.halfFloat, false, 'halfFloat無効');
assert.strictEqual(LITE_OPTIONS.animation, false, '常時60fpsではなく変更時描画');
assert.strictEqual(LITE_OPTIONS.cacheBytes, 128 * 1024 * 1024, '通常cacheBytesに上限を設定');
assert.ok(LITE_OPTIONS.lodFog.sseFactor > 2, '通常LODを粗くして遠景タイルを抑制');
const LOW_POWER_LITE_OPTIONS = vm.runInContext('LOW_POWER_LITE_OPTIONS', feContext);
assert.ok(LOW_POWER_LITE_OPTIONS.cacheBytes < LITE_OPTIONS.cacheBytes, 'LOW_POWERはcacheBytesをさらに下げる');
assert.ok(LOW_POWER_LITE_OPTIONS.memoryBudget.maxPendingRequests < LITE_OPTIONS.memoryBudget.maxPendingRequests,
  'LOW_POWERは同時リクエスト数をさらに下げる');

const createPhotoJourneyView = vm.runInContext('createPhotoJourneyView', feContext);
const lowView = createPhotoJourneyView({ id: 'modelayer' }, { low: true });
assert.strictEqual(lowView.options.animation, false, 'LOW_POWERでもon-demand描画');
assert.strictEqual(lowView.options.cacheBytes, LOW_POWER_LITE_OPTIONS.cacheBytes, 'LOW_POWER設定をThreeViewへ渡す');
assert.strictEqual(lowView.plugins.length, 1, 'DefaultPluginを1つだけ登録');

/* static/js/photo-journey-settings.js — 低頻度巡回設定を実値で検証 */
const settingsSrc = fs.readFileSync('static/js/photo-journey-settings.js', 'utf8');
function loadSettings(low) {
  const context = { console, LOW_POWER: low, DWELL_MS: 16000, window: {} };
  runInContext(settingsSrc, context);
  return context.window.GEOFRAME_PHOTO_JOURNEY_SETTINGS;
}
const normalSettings = loadSettings(false);
const lowSettings = loadSettings(true);
assert.strictEqual(normalSettings.dwellMs, 16000, '通常の巡回周期は既定値を維持');
assert.ok(lowSettings.dwellMs > normalSettings.dwellMs, 'LOW_POWERは巡回更新頻度を下げる');
assert.ok(lowSettings.flyMs < normalSettings.flyMs, 'LOW_POWERはflyTo時間を短縮');
assert.ok(lowSettings.height > normalSettings.height, 'LOW_POWERは高高度で遠景LODにする');
assert.strictEqual(lowSettings.headingStep, 0, 'LOW_POWERは方位更新を止める');

// manifest（points/route）→ Navara レイヤ変換の実呼び出し。
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
  tileUrls: { gsi: 'http://h/tiles/gsi/{z}/{x}/{y}.png', osm: 'http://h/tiles/osm/{z}/{x}/{y}.png' },
});

const raster = sources.filter(s => s.type === 'raster-tile');
assert.strictEqual(raster.length, 1, '同時に描画するraster-tile basemapは1つだけ');
assert.strictEqual(raster[0].url, 'http://h/tiles/gsi/{z}/{x}/{y}.png', '国内地点ではGSIを選ぶ');
assert.strictEqual(layers.filter(l => l.type === 'raster').length, 1, 'raster layerも1つだけ');
assert.strictEqual(layerState.basemap.kind, 'gsi', '初期basemapの選択状態を返す');

const updatePhotoJourneyBasemap = vm.runInContext('updatePhotoJourneyBasemap', feContext);
updatePhotoJourneyBasemap(layerState.basemap, { lon: -73.9857, lat: 40.7484 });
assert.strictEqual(sourceUpdates.length, 1, '国外へ移動したときだけ同一sourceを更新');
assert.strictEqual(sourceUpdates[0].url, 'http://h/tiles/osm/{z}/{x}/{y}.png', '国外ではOSMへ切り替える');
assert.strictEqual(layerState.basemap.kind, 'osm', 'basemap状態を更新する');

const geo = sources.find(s => s.type === 'geojson');
assert.ok(geo, '写真ポイント/ルートのgeojsonソースを足す');
assert.strictEqual(geo.data.type, 'FeatureCollection');
const features = geo.data.features;
assert.strictEqual(features.filter(f => f.geometry.type === 'Point').length, 2, '写真ポイントを2点入れる');
const line = features.find(f => f.geometry.type === 'LineString');
assert.ok(line, 'ライトルート線（LineString）を入れる');
// vm レルムで生成された配列はNode側のArrayとプロトタイプが異なるため、JSONで比較する
assert.strictEqual(JSON.stringify(line.geometry.coordinates), JSON.stringify([[139.7454, 35.6586], [135.5013, 34.6687]]), 'ルートは地点順');
assert.strictEqual(JSON.stringify(features[0].geometry.coordinates), JSON.stringify([139.7454, 35.6586]), 'ポイント座標は[lon,lat]');

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
// 一度きりのフォールバックガード（フラグを立ててリロード。bootstrap.jsがmodeを剥がす）
assert.ok(controllerSrc.includes("sessionStorage.setItem('geoframe.3d.fallback', '1')"), 'フォールバックフラグを立てる');
assert.ok(/location\.reload\(\)/.test(controllerSrc), '失敗時はリロードする');
// 低頻度カメラ遷移と、既存HUD/写真カードの再利用
assert.ok(controllerSrc.includes('settings.dwellMs'), '設定値で低頻度の滞在時間を制御する');
assert.ok(controllerSrc.includes('showPhoto(i)'), '既存の写真カードを再利用する');
assert.ok(controllerSrc.includes("setMode('3D PHOTO JOURNEY LITE')"), '3D表示ラベルを明示する');
assert.ok(!controllerSrc.includes("setMode('PHOTO JOURNEY')"), '3Dで旧ラベルを使わない');

console.log('photo-journey-lite: OK');
