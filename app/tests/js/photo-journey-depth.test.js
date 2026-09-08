/* PHOTO JOURNEY Lite の写真地点カメラ・地形高度・斜光を、ブラウザ無しで
 * 実ソースの純粋関数へ入力して検証する。import/exportだけを除去してvmで実行し、
 * Navara/DOMのモック実装には依存しない。 */
const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

function runInContext(source, context) {
  return vm.runInContext(source, vm.createContext(context));
}

const frontendSrc = fs.readFileSync('../frontend/photo-journey-lite.js', 'utf8');
const stripped = frontendSrc
  .replace(/^import\s+[^\n]*\n/gm, '')
  .replace(/^export\s+/gm, '');

class FakeThreeView {}
class FakeDefaultPlugin {}
function fakeJapanGsiElevationDecoder() {
  return { kind: 'japanGSI' };
}
const context = {
  console,
  Date,
  Math,
  Number,
  ThreeView: FakeThreeView,
  DefaultPlugin: FakeDefaultPlugin,
  JAPAN_GSI_ELEVATION_DECODER: fakeJapanGsiElevationDecoder,
};
runInContext(stripped, context);

const photoJourneyCameraPose = vm.runInContext('photoJourneyCameraPose', context);
const photoJourneyElevation = vm.runInContext('photoJourneyElevation', context);
const updatePhotoJourneyLighting = vm.runInContext('updatePhotoJourneyLighting', context);

const DEG = Math.PI / 180;
const EARTH_RADIUS = 6378137;

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

function angularDifference(a, b) {
  return Math.abs((((a - b) + 180) % 360 + 360) % 360 - 180);
}

function greatCircleDistanceMeters(a, b) {
  const lat1 = a.lat * DEG;
  const lat2 = b.lat * DEG;
  const dLat = (b.lat - a.lat) * DEG;
  const dLng = (b.lng - a.lng) * DEG;
  const h = Math.sin(dLat / 2) ** 2 +
    Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLng / 2) ** 2;
  return EARTH_RADIUS * 2 * Math.atan2(Math.sqrt(h), Math.sqrt(Math.max(0, 1 - h)));
}

function initialBearingDegrees(from, to) {
  const lat1 = from.lat * DEG;
  const lat2 = to.lat * DEG;
  const dLng = (to.lng - from.lng) * DEG;
  const y = Math.sin(dLng) * Math.cos(lat2);
  const x = Math.cos(lat1) * Math.sin(lat2) -
    Math.sin(lat1) * Math.cos(lat2) * Math.cos(dLng);
  return ((Math.atan2(y, x) / DEG) % 360 + 360) % 360;
}

function assertTargetIsCentered(point, options, label) {
  const pose = photoJourneyCameraPose(point, options);
  assert.ok(pose, `${label}: 有効な姿勢を返す`);
  assert.ok(Number.isFinite(pose.lng) && Number.isFinite(pose.lat) &&
    Number.isFinite(pose.height) && Number.isFinite(pose.heading),
  `${label}: カメラ姿勢の全値が有限`);

  const target = { lng: point.lon ?? point.lng, lat: point.lat };
  const from = { lng: pose.lng, lat: pose.lat };
  const horizontalDistance = greatCircleDistanceMeters(from, target);
  const clearance = pose.height - (options.elevation ?? 0);
  const expectedDistance = clearance / Math.tan(-pose.pitch * DEG);
  assert.ok(Math.abs(horizontalDistance - expectedDistance) < 1e-5,
    `${label}: 地面相対高度と俯角から決まる後退距離`);

  const targetBearing = initialBearingDegrees(from, target);
  assert.ok(angularDifference(pose.heading, targetBearing) < 1e-8,
    `${label}: カメラ方位が撮影点を向く`);
  return pose;
}

/* 初期方位と周回方位の全周で、カメラは撮影点の後方へ退き、撮影点を視野中心に置く。 */
for (const heading of [0, 90, 180, 270]) {
  assertTargetIsCentered(
    { lon: 139.7454, lat: 35.6586 },
    { height: 2100, pitch: -32, heading, elevation: 37 },
    `東京・heading=${heading}`,
  );
}

/* 経度±180度の折り返しと高緯度でも、球面移動後の逆方位が注視点へ戻る。 */
for (const [point, heading, label] of [
  [{ lon: 179.999, lat: 10.25 }, 90, '日付変更線・東側'],
  [{ lon: -179.999, lat: -10.25 }, 270, '日付変更線・西側'],
  [{ lon: 179.7, lat: 89.5 }, 0, '高緯度・北向き'],
  [{ lon: -179.7, lat: 89.5 }, 180, '高緯度・南向き'],
]) {
  const pose = assertTargetIsCentered(point, { height: 1400, pitch: -24, heading, elevation: 321 }, label);
  assert.ok(pose.lng >= -180 && pose.lng <= 180, `${label}: 経度を[-180,180]へ正規化する`);
  assert.ok(pose.lat >= -90 && pose.lat <= 90, `${label}: 緯度を有効範囲に保つ`);
}

/* 高度は地面相対であり、撮影地点標高を足した値になる。上下限と俯角上下限も固定する。 */
const clampedHeight = photoJourneyCameraPose(
  { lon: 0, lat: 0 },
  { height: 100, pitch: 0, elevation: 10000 },
);
assert.strictEqual(clampedHeight.height, 9400, 'カメラ高度は標高9000m + 最低clearance 400m');
assert.strictEqual(clampedHeight.pitch, -18, 'pitch上限（水平側）を-18度へclampする');
const clampedLow = photoJourneyCameraPose(
  { lon: 0, lat: 0 },
  { height: 90000, pitch: -90, elevation: -1000 },
);
assert.strictEqual(clampedLow.height, 7500, 'カメラ高度は標高-500m + 最大clearance 8000m');
assert.strictEqual(clampedLow.pitch, -70, 'pitch下限（俯角側）を-70度へclampする');
const aliasPose = photoJourneyCameraPose({ lng: -73.9857, lat: 40.7484 }, {
  height: 1400, pitch: -24, heading: -90, elevation: 12,
});
assert.ok(angularDifference(aliasPose.heading, 270) < 0.1,
  'lng aliasと負のheadingを正規化する（球面収束分は許容）');
for (const invalid of [
  null,
  {},
  { lon: NaN, lat: 35 },
  { lon: 139, lat: NaN },
  { lon: 181, lat: 35 },
  { lon: -181, lat: 35 },
  { lon: 139, lat: 91 },
  { lon: 139, lat: -91 },
  { lng: 'invalid', lat: 35 },
]) {
  assert.strictEqual(photoJourneyCameraPose(invalid), null, 'NaN/無効座標はカメラ姿勢を作らない');
}
const defaultsForInvalidOptions = photoJourneyCameraPose(
  { lon: 139, lat: 35 },
  { height: NaN, pitch: NaN, heading: NaN, elevation: NaN },
);
assert.strictEqual(defaultsForInvalidOptions.height, 1400, 'NaN高度は既定clearanceへfallbackする');
assert.strictEqual(defaultsForInvalidOptions.pitch, -24, 'NaN pitchは既定俯角へfallbackする');

/* 標高の優先順位は realElev=true の実標高→ DEM（有限値）→海面0m。
 * intel.elevだけを持つapplyManifest由来の疑似値は採用しない。 */
let sampledArgs = null;
const demView = {
  sampleTerrainHeight: args => {
    sampledArgs = args;
    return 123.5;
  },
};
assert.strictEqual(photoJourneyElevation(demView, { lng: 139.7, lat: 35.6, realElev: true, intel: { elev: 456 } }),
  456, '既知の実標高を粗い親DEMの低い推定値で上書きしない');
assert.strictEqual(sampledArgs, null, '実標高があるときはDEMサンプリングを省く');
assert.strictEqual(photoJourneyElevation(demView, { lng: 139.7, lat: 35.6 }),
  123.5, '既知の実標高がなければDEMから求める');
assert.strictEqual(sampledArgs.lng, 139.7, 'DEMへ撮影地点の経度を渡す');
assert.strictEqual(sampledArgs.lat, 35.6, 'DEMへ撮影地点の緯度を渡す');
assert.strictEqual(photoJourneyElevation({ sampleTerrainHeight: () => NaN },
  { lon: 139, lat: 35, realElev: true, intel: { elev: 456 } }),
  456, 'DEMが欠損時はrealElev=trueの実標高へfallbackする');
assert.strictEqual(photoJourneyElevation({ sampleTerrainHeight: () => { throw new Error('DEM unavailable'); } },
  { lon: 139, lat: 35, realElev: true, intel: { elev: 456 } }),
  456, 'DEMサンプリングが例外時もrealElev=trueの実標高へfallbackする');
assert.strictEqual(photoJourneyElevation({},
  { lon: 139, lat: 35, intel: { elev: 456 } }),
  0, 'realElev無しのintel.elev（疑似標高）は採用しない');
assert.strictEqual(photoJourneyElevation({},
  { lon: 139, lat: 35, realElev: false, intel: { elev: 456 } }),
  0, 'realElev=falseのintel.elev（疑似標高）は採用しない');
assert.strictEqual(photoJourneyElevation({},
  { lon: 139, lat: 35, realElev: true, intel: { elev: 'bad' } }),
  0, '実標高が非有限なら海面0mへfallbackする');
assert.strictEqual(photoJourneyElevation({ sampleTerrainHeight: () => 20000 },
  { lon: 139, lat: 35 }), 9000, 'DEM標高上限をclampする');
assert.strictEqual(photoJourneyElevation({ sampleTerrainHeight: () => -1000 },
  { lon: 139, lat: 35 }), -500, 'DEM標高下限をclampする');

/* 斜光は地点の日付を無視し、経度4分/度の補正で地方太陽時15:00へ固定する。 */
function localSolarMinutes(date, longitude) {
  const utcMinutes = date.getUTCHours() * 60 + date.getUTCMinutes() +
    date.getUTCSeconds() / 60 + date.getUTCMilliseconds() / 60000;
  return ((utcMinutes + longitude * 4) % 1440 + 1440) % 1440;
}
const longitudes = [0, 139.7454, -73.9857, 180, -180, 179.999];
for (const longitude of longitudes) {
  const view = { atmosphere: { date: new Date(0) } };
  updatePhotoJourneyLighting(view, {
    lon: longitude,
    lat: longitude === 180 ? -65 : 35,
    date: '1999-01-01T00:00:00Z',
  });
  assert.strictEqual(localSolarMinutes(view.atmosphere.date, longitude), 15 * 60,
    `経度${longitude}度でも地方太陽時を15:00に揃える`);
  const expectedMs = Date.UTC(2026, 2, 20, 15) - longitude * 240000;
  assert.strictEqual(view.atmosphere.date.getTime(), expectedMs,
    `経度${longitude}度の太陽日時を春分15時から計算する`);
}
const sameLongitudeA = { atmosphere: { date: null } };
const sameLongitudeB = { atmosphere: { date: null } };
updatePhotoJourneyLighting(sameLongitudeA, { lon: 139, date: '2020-01-01' });
updatePhotoJourneyLighting(sameLongitudeB, { lon: 139, date: '2035-12-31' });
assert.strictEqual(sameLongitudeA.atmosphere.date.getTime(), sameLongitudeB.atmosphere.date.getTime(),
  '地点の日付によらず陰影用太陽日時を固定する');
const untouched = { atmosphere: { date: new Date(12345) } };
updatePhotoJourneyLighting(untouched, { lon: NaN, lat: 35 });
assert.strictEqual(untouched.atmosphere.date.getTime(), 12345, '無効経度では陰影用日時を変更しない');
updatePhotoJourneyLighting(untouched, { lon: 139, lat: 35 });
assert.ok(Number.isFinite(untouched.atmosphere.date.getTime()), '有効地点では陰影用日時を設定する');

console.log('photo-journey-depth.test.js: ok');
