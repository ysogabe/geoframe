/* ============================================================
   frontend/photo-journey-lite.js — 3D PHOTO JOURNEY Lite の Navara アダプタ
   ============================================================
   `npm run build:navara`（vite build）がこのファイルをエントリとして
   app/static/navara/ に自己完結の ES バンドルを生成する。

   N100（低スペック）向けの「軽量構成」をここで唯一の形に固定する:
     - 内部描画倍率は 0.5（固定GPU領域を抑え、航空写真LODへ予算を回す）
     - shadow 無効、MSAA 無効、halfFloat 無効
     - DefaultPlugin は addPlugin() で登録するのみ。addDefaultPhotorealScene() は
       呼ばない（SSR/SSAO/DOF/大気/雲/ボリュームを一切有効化しない）
     - 出典表示は view.attribution へ GSI / OSM を登録する

   依存する Navara API（@navaramap/three 0.1.1）:
     - new ThreeView({ container, pixelRatio, shadow, segments, maxSse, ... })
     - view.addPlugin(new DefaultPlugin()) → await view.init()
     - view.addSource({ type: "raster-tile" | "raster-dem" | "geojson", ... }) → Source ハンドル
     - view.addLayer({ type: "terrain" | "raster" | "vector", source, ... })
     - JAPAN_GSI_ELEVATION_DECODER() → raster-dem source の elevationDecoder
     - view.setCamera({ lng, lat, height, heading, pitch, roll })
     - view.flyTo({ lng, lat, height, pitch, heading }, { duration })
     - source.update({ type: "raster-tile", url, ... })
     - view.attribution?.add([{ attribution, attributionUrl }])
   ============================================================ */
import ThreeView, { JAPAN_GSI_ELEVATION_DECODER } from "@navaramap/three";
import { DefaultPlugin } from "@navaramap/three-default-plugin";

/* 軽量構成。N100相当の内蔵GPUでタイルの先読みと常時描画を抑える上限値。 */
const NORMAL_OPTIONS = Object.freeze({
  pixelRatio: 0.6,      // 2560x1080実機では1536x648。第一段階として描画密度を上げる
  shadow: false,        // 影は初期化時オプション。N100 では無効
  // view.globeのLOD品質。segmentsは初期地球面の分割数（DEMは別途適応分割）、
  // maxSseは許容screen-space-error（px、大きいほど粗く早く安定する）。
  // GSI DEM地形の初回トライアルはN100を優先し、通常表示でも控えめな値にする。
  segments: 32,
  maxSse: 4,
  multisampling: 0,     // MSAA 無効（0）
  halfFloat: false,     // ポスト処理は低精度で十分
  picking: false,       // フィーチャーピッキング無効（タップ対象が無い）
  animation: false,     // flyTo/forceUpdate時だけ描画（Navaraのon-demand mode）
  mobileOptimization: true, // 低画素比などのモバイル向け軽量化を有効化
  // 2560x1080/pixelRatio=1では固定領域だけで予算超過し、画像がZ3で停止した。
  // 0.5倍と512MiB上限の組合せでN95実測Z15まで到達する。
  cacheBytes: 512 * 1024 * 1024,
  // 通常表示は撮影地点を判別できる航空写真LODを優先する。
  // 低スペック向けの粗いLODはLOW_POWER_OPTIONSだけで有効にする。
  lodFog: { enabled: false, density: 0, sseFactor: 0 },
  dynamicSse: {
    enabled: false, density: 0, sseFactor: 0,
    heightFalloff: 0.25, minHeight: 0, maxHeight: 8000,
  },
  memoryBudget: {
    maxWorkerHeapBytes: 128 * 1024 * 1024,
    fontBudgetBytes: 8 * 1024 * 1024,
    maxPendingRequests: 4,
    sseMultiplierMin: 1,
    sseMultiplierMax: 8,
  },
});

const LOW_POWER_OPTIONS = Object.freeze({
  ...NORMAL_OPTIONS,
  // 省電力表示は従来の内部倍率を維持し、通常表示だけを段階的に精細化する。
  pixelRatio: 0.5,
  // 地形は?lowでも無効化しない（GOALの「low でも terrain を維持」）。
  // segments/maxSseだけ粗くしてメッシュ分割・タイル選択のコストを下げる。
  segments: 16,
  maxSse: 8,
  cacheBytes: 64 * 1024 * 1024,
  lodFog: { enabled: true, density: 1e-1, sseFactor: 12 },
  dynamicSse: {
    enabled: true, density: 5e-4, sseFactor: 48,
    heightFalloff: 0.15, minHeight: 0, maxHeight: 6000,
  },
  memoryBudget: {
    maxWorkerHeapBytes: 64 * 1024 * 1024,
    fontBudgetBytes: 4 * 1024 * 1024,
    maxPendingRequests: 2,
    sseMultiplierMin: 8,
    sseMultiplierMax: 48,
  },
});

export const LITE_OPTIONS = NORMAL_OPTIONS;
export const LOW_POWER_LITE_OPTIONS = LOW_POWER_OPTIONS;

export function getLiteOptions({ low = false } = {}) {
  return low ? LOW_POWER_OPTIONS : NORMAL_OPTIONS;
}

/* ThreeView を生成し DefaultPlugin を登録する。init() は呼び出し側で await する。
   ※ addDefaultPhotorealScene() は意図的に呼ばない（大気・雲・SSR/SSAO/DOF を排除）。 */
export function createPhotoJourneyView(container, opts = {}) {
  const defaults = getLiteOptions({ low: opts.low });
  const view = new ThreeView({
    container,
    pixelRatio: Math.min(opts.pixelRatio ?? defaults.pixelRatio, 1),
    shadow: opts.shadow ?? defaults.shadow,
    segments: opts.segments ?? defaults.segments,
    // 粗い写真を隠すhybridでは注視点の詳細写真へLODを進める。
    // 通常のphoto、省電力は従来のLOD設定を維持する。
    maxSse: opts.maxSse ?? (opts.terrainStyle === "hybrid" && !opts.low ? 3 : defaults.maxSse),
    multisampling: opts.multisampling ?? defaults.multisampling,
    halfFloat: opts.halfFloat ?? defaults.halfFloat,
    picking: opts.picking ?? defaults.picking,
    useNormal: true,
    animation: opts.animation ?? defaults.animation,
    mobileOptimization: opts.mobileOptimization ?? defaults.mobileOptimization,
    cacheBytes: opts.cacheBytes ?? defaults.cacheBytes,
    lodFog: opts.lodFog ?? defaults.lodFog,
    dynamicSse: opts.dynamicSse ?? defaults.dynamicSse,
    memoryBudget: opts.memoryBudget ?? defaults.memoryBudget,
  });
  view.addPlugin(new DefaultPlugin());
  return view;
}

/* 背景タイルをレイヤとして追加する。
   3,099地点を1本のGeoJSONルートにすると全世界の線が画面を覆い、GPU予算も
   航空写真から奪うため、地点情報は既存PHOTO LOG/HUDへ任せる。
   tileUrls は server.py の /tiles/{gsi-photo,osm}/ プロキシを指す（CDN は使わない）。 */
function isDomesticPoint(point) {
  return !!point && point.lon >= 122.5 && point.lon <= 154.5 &&
    point.lat >= 20.0 && point.lat <= 46.5;
}

export function selectPhotoJourneyBasemap(point) {
  return isDomesticPoint(point) ? "gsi-photo" : "osm";
}

/*
 * 巡回中の投機取得用タイル列。Navara本体の可視タイル選択とは分離し、
 * 地形の親タイルと「現在地点の周囲」「次の巡回地点」の画像3x3リングを返す。
 * 通常時は国内のz15/z16（z16は一段先の精細化）、国外はOSMのz15、
 * ?lowでは全地域z14に抑える。実際の取得レートと停止条件はcontroller側で管理する。
 */
const PHOTO_PREFETCH_RADIUS = 1;
const PHOTO_PREFETCH_NORMAL_ZOOMS = Object.freeze([15, 16]);
const PHOTO_PREFETCH_LOW_ZOOMS = Object.freeze([14]);

function photoJourneyTileXY(point, zoom) {
  const lon = Number(point?.lon ?? point?.lng);
  const lat = Number(point?.lat);
  if (!Number.isFinite(lon) || !Number.isFinite(lat) ||
      lon < -180 || lon > 180 || lat < -90 || lat > 90) return null;
  const clippedLat = Math.max(-85.05112878, Math.min(85.05112878, lat));
  const n = 2 ** zoom;
  const x = Math.floor(n * ((lon + 180) / 360));
  const radians = clippedLat * Math.PI / 180;
  const y = Math.floor(n * (1 - Math.log(Math.tan(radians) + 1 / Math.cos(radians)) / Math.PI) / 2);
  return { x: Math.max(0, Math.min(n - 1, x)), y: Math.max(0, Math.min(n - 1, y)), n };
}

export function getPhotoJourneyPrefetchTiles(point, { low = false, origin = "", imagery = true, detail = false } = {}) {
  const tile = photoJourneyTileXY(point, PHOTO_PREFETCH_NORMAL_ZOOMS[0]);
  if (!tile) return [];
  const layer = selectPhotoJourneyBasemap(point);
  const zooms = low || detail ? PHOTO_PREFETCH_LOW_ZOOMS :
    (layer === "gsi-photo" ? PHOTO_PREFETCH_NORMAL_ZOOMS : [15]);
  const base = String(origin || "").replace(/\/+$/, "");
  const urls = [];
  // 地形の親タイルも温める。DEMを先に用意してから写真へ予算を回す。
  // 国内はZ14、海外は全球DEMの上限Z8まで。既存の取得レート/キュー上限内で処理する。
  const demZooms = layer === "gsi-photo" ? (low ? [8, 10, 12] : [8, 10, 12, 14]) : [6, 8];
  for (const zoom of demZooms) {
    const center = photoJourneyTileXY(point, zoom);
    urls.push(`${base}/tiles/gsi-dem/${zoom}/${center.x}/${center.y}.png`);
  }
  for (const zoom of imagery ? zooms : []) {
    const center = photoJourneyTileXY(point, zoom);
    if (!center) continue;
    for (let dy = -PHOTO_PREFETCH_RADIUS; dy <= PHOTO_PREFETCH_RADIUS; dy++) {
      for (let dx = -PHOTO_PREFETCH_RADIUS; dx <= PHOTO_PREFETCH_RADIUS; dx++) {
        const x = (center.x + dx + center.n) % center.n;
        const y = center.y + dy;
        if (y < 0 || y >= center.n) continue;
        urls.push(`${base}/tiles/${layer}/${zoom}/${x}/${y}.png${detail && layer === "gsi-photo" ? "?detail=1" : ""}`);
      }
    }
  }
  return urls;
}

function rasterDefinition(kind, urls, style = "photo") {
  return {
    type: "raster-tile",
    url: style === "hybrid" && kind === "gsi-photo"
      ? `${urls[kind]}${urls[kind].includes("?") ? "&" : "?"}detail=1` : urls[kind],
    minZoom: kind === "gsi-photo" ? 2 : 0,
    maxZoom: kind === "gsi-photo" ? 18 : 19,
  };
}

/* ============================================================
   GSI DEM terrain（raster-dem）+ 地形の立体感を優先したDEM由来シェーディング。
   server.py の /tiles/gsi-dem/ プロキシ（z0-8: demgm_png、z9-14: dem_png）を
   JAPAN_GSI_ELEVATION_DECODER() でデコードし、実地形として立ち上げる。
   terrain layer を imagery layer（rasterDefinition）より先に addLayer することで、
   Navara が後続レイヤをこの地形へドレープできる（順序が逆だとドレープ先が無い）。
   ============================================================ */
// server.py 側 GSI_DEM_TILE_BANDS と一致させること（z0-8 demgm_png / z9-14 dem_png）。
const DEM_MIN_ZOOM = 0;
const DEM_MAX_ZOOM = 14;
// TerrainMaterial/RasterDemSourceDescriptionには標高への乗算スケールが存在しない
// （@navaramap/three 0.1.1 の型定義で確認済み）。デコード結果の実測メートルを
// そのまま使うことが「実高度スケール1.0」であり、追加設定は不要。
// HillshadeMaterial.exaggeration（既定1.0）だけが検証済みの陰影強調パラメータ。
// 遠方や読み込み中の粗い地形にも陰影を補うため、同じraster-demソースを
// 参照する薄いhillshade raster layerで、地形の面が読める（1.35倍）陰影を追加する。
// Hillshadeの係数変更はメッシュ数を増やさず、N100でも地形の立体感だけを改善する。
const DEM_HILLSHADE_EXAGGERATION = 1.35;

function demSourceDefinition(url) {
  return {
    type: "raster-dem",
    url,
    minZoom: DEM_MIN_ZOOM,
    maxZoom: DEM_MAX_ZOOM,
    tileSize: 256,
    elevationDecoder: JAPAN_GSI_ELEVATION_DECODER(),
  };
}

/* raster-dem sourceを1つ登録し、terrain layer→hillshade layerの順で追加する。
   影は初期化オプション（LITE_OPTIONS.shadow=false）に加え、terrain/hillshadeの
   どちらでも castShadow/receiveShadow を明示的に false のままにする。 */
function addTerrain(view, demUrl) {
  const source = view.addSource(demSourceDefinition(demUrl));
  view.addLayer({
    type: "terrain",
    source,
    terrain: { lit: true, skirt: true, castShadow: false, receiveShadow: false },
  });
  // Navara 0.1.1はshow:falseのスロットをhillshadeも含めて合成から除外する。
  // show:trueで陰影を有効にし、opacity:0で生のDEM色を隠す。陰影の法線は
  // opacityから独立して合成されるため、写真なしでも斜面が読める。
  view.addLayer({
    type: "raster",
    source,
    raster: { show: true, opacity: 0 },
    hillshade: { exaggeration: DEM_HILLSHADE_EXAGGERATION },
  });
  return source;
}

export function updatePhotoJourneyBasemap(state, point) {
  if (!state) return state;
  const next = selectPhotoJourneyBasemap(point);
  if (state.kind !== next) {
    if (state.style === "hybrid" && next !== "gsi-photo") {
      // 海外の通常地図を衛星写真扱いにしない。見えない画像も取得しない。
      state.layer?.delete();
      state.source?.delete();
      state.layer = state.source = null;
    } else if (state.style === "hybrid" && !state.source && state.imageryEnabled) {
      state.source = state.view.addSource(rasterDefinition(next, state.urls, state.style));
      state.layer = state.view.addLayer({ type: "raster", source: state.source,
        raster: { show: true, opacity: 0 } });
      state.opacity = 0;
    } else state.source?.update(rasterDefinition(next, state.urls, state.style));
    state.kind = next;
  }
  if (state.style === "hybrid") setPhotoJourneyImageryOpacity(state, 0);
  return state;
}

export function setPhotoJourneyImageryOpacity(state, value) {
  if (!state?.layer) return;
  const opacity = Math.max(0, Math.min(1, Number(value) || 0));
  if (state.opacity === opacity) return;
  state.opacity = opacity;
  state.layer.update({ type: "raster", source: state.source, raster: { show: true, opacity } });
  state.view?.forceUpdate?.();
}

/* 写真判定は高精細写真へ切り替わるZ14の、注視点を囲む3×3のみ。
   全画面の読み込み完了を表すものではない。遠方のZ13以下はhybridでは描かない。 */
export function getPhotoJourneyRevealTiles(point, { origin = "" } = {}) {
  if (selectPhotoJourneyBasemap(point) !== "gsi-photo") return [];
  const zoom = 14;
  const center = photoJourneyTileXY(point, zoom);
  if (!center) return [];
  const base = String(origin || "").replace(/\/+$/, "");
  const urls = [];
  for (let dy = -1; dy <= 1; dy++) {
    for (let dx = -1; dx <= 1; dx++) {
      const x = (center.x + dx + center.n) % center.n;
      const y = center.y + dy;
      if (y >= 0 && y < center.n) urls.push(`${base}/tiles/gsi-photo/${zoom}/${x}/${y}.png?detail=1`);
    }
  }
  return urls;
}

/* 撮影地点の周りだけに置く途切れた距離リング。等高線ではない。
   地形標高を持つ細い帯にする。テクスチャへ焼く線は祖先LODで幅が変わり、
   通常のpolylineは頂点の標高を保持しないため、perPositionHeightのpolygonを使う。 */
export function photoJourneyGroundLines(view, point, { low = false } = {}) {
  if (!photoJourneyTileXY(point, 14)) return emptyAircraftGeoJSON();
  const features = [];
  const origin = { lon: point.lon ?? point.lng, lat: point.lat };
  for (const radius of low ? [500, 1000] : [400, 800, 1200]) {
    for (let quadrant = 0; quadrant < 4; quadrant++) {
      const inner = [], outer = [];
      for (let degrees = 10; degrees <= 80; degrees += 2.5) {
        const angle = (quadrant * 90 + degrees) * Math.PI / 180;
        const pos = offsetGeodetic(origin, Math.sin(angle) * radius, Math.cos(angle) * radius);
        const elevation = finiteNumber(view?.sampleTerrainHeight?.({ lng: pos.lon, lat: pos.lat }));
        if (elevation === null) { inner.length = 0; break; }
        for (const [edge, distance] of [[inner, radius - 4], [outer, radius + 4]]) {
          const p = offsetGeodetic(origin, Math.sin(angle) * distance, Math.cos(angle) * distance);
          edge.push([p.lon, p.lat, elevation + 12]);
        }
      }
      if (!inner.length) continue;
      const ring = [...inner, ...outer.reverse(), inner[0]];
      features.push({ type: "Feature", properties: {},
        geometry: { type: "Polygon", coordinates: [ring] } });
    }
  }
  return { type: "FeatureCollection", features };
}

function groundLinesLayer(source, show = true) {
  // Layer.updateは設定を置換する。表示切替時も標高・描画設定を含めて渡す。
  return { type: "vector", source,
    polygon: { show, clampToGround: false, perPositionHeight: true, tiled: false, color: 0x39cad8,
      opacity: 0.55, transparent: true, depthWrite: false,
      lit: false, castShadow: false, receiveShadow: false } };
}

export function updatePhotoJourneyGroundLines(state, point) {
  if (!state) return;
  const data = photoJourneyGroundLines(state.view, point, { low: state.low });
  if (!data.features.length) return;
  // 空GeoJSONからの初回更新を取りこぼすNavaraの経路を避け、有効な頂点で初めて作る。
  if (!state.source) {
    state.source = state.view.addSource({ type: "geojson", data, tiled: false });
    state.layer = state.view.addLayer(groundLinesLayer(state.source));
  } else {
    state.source.update({ type: "geojson", data, tiled: false });
    state.layer.update(groundLinesLayer(state.source));
  }
  state.view.forceUpdate?.();
}

export function hidePhotoJourneyGroundLines(state) {
  state?.layer?.update(groundLinesLayer(state.source, false));
}

/* ============================================================
   ADS-B 3D aircraft overlay

   The functions in this section are deliberately pure until the three small
   Navara layer helpers at the bottom.  The feed uses feet, knots and feet per
   minute, while GeoJSON positions use metres.  Keeping the conversion and
   geometry here makes the browser-side bridge testable without WebGL.
   ============================================================ */
const FEET_TO_METERS = 0.3048;
const KNOT_TO_METERS_PER_SECOND = 0.514444;
const VERTICAL_FEET_PER_MINUTE_TO_METERS_PER_SECOND = 0.00508;
const EARTH_LAT_METERS = 111320;
const AIRCRAFT_MAX_AGE_SECONDS = 60;
const AIRCRAFT_MAX_COUNT = 20;
const AIRCRAFT_PITCH_LIMIT = 25;
const AIRCRAFT_ROLL_LIMIT = 60;

function finiteNumber(value) {
  if (value === null || value === undefined || value === "") return null;
  if (typeof value === "string" && value.trim().toLowerCase() === "ground") return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

export function feetToMeters(value) {
  const feet = finiteNumber(value);
  return feet === null ? 0 : feet * FEET_TO_METERS;
}

function altitudeFeet(value) {
  const feet = finiteNumber(value);
  return feet === null || feet < 0 ? null : feet;
}

/* alt_geom is the WGS84 geometric altitude.  alt_baro/alt are compatibility
   fallbacks; a literal "ground" and malformed values become sea-level 0m. */
export function aircraftAltitudeMeters(aircraft) {
  const a = aircraft || {};
  const geometric = altitudeFeet(a.alt_geom);
  if (geometric !== null) return feetToMeters(geometric);
  const fallback = altitudeFeet(a.alt_baro) ?? altitudeFeet(a.alt);
  return fallback === null ? 0 : feetToMeters(fallback);
}

function normalizedHeading(value) {
  const heading = finiteNumber(value);
  if (heading === null) return null;
  return ((heading % 360) + 360) % 360;
}

/* true heading is the best camera orientation; track is the motion fallback,
   then magnetic heading for old feeds that omit both. */
export function aircraftHeading(aircraft) {
  const a = aircraft || {};
  return normalizedHeading(a.true_heading) ??
    normalizedHeading(a.track) ??
    normalizedHeading(a.mag_heading) ?? 0;
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

/* Estimate flight-path pitch from vertical rate and ground speed.  Both
   conversions are intentionally explicit so a missing/ground speed cannot
   create Infinity or NaN camera values. */
export function aircraftPitch(aircraft) {
  const a = aircraft || {};
  const verticalRate = finiteNumber(a.geom_rate) ?? finiteNumber(a.baro_rate);
  const groundSpeed = finiteNumber(a.gs);
  if (verticalRate === null || groundSpeed === null || groundSpeed <= 0) return 0;
  const verticalMps = verticalRate * VERTICAL_FEET_PER_MINUTE_TO_METERS_PER_SECOND;
  const horizontalMps = groundSpeed * KNOT_TO_METERS_PER_SECOND;
  return clamp(Math.atan2(verticalMps, horizontalMps) * 180 / Math.PI,
    -AIRCRAFT_PITCH_LIMIT, AIRCRAFT_PITCH_LIMIT);
}

export function aircraftRoll(aircraft) {
  return clamp(finiteNumber((aircraft || {}).roll) ?? 0,
    -AIRCRAFT_ROLL_LIMIT, AIRCRAFT_ROLL_LIMIT);
}

/* readsb's seen_pos is seconds since the last position message.  Accept an
   epoch timestamp too, which keeps the helper compatible with alternate
   gateways that already materialise the feed timestamp. */
function seenPositionAgeSeconds(aircraft, now) {
  const seen = finiteNumber((aircraft || {}).seen_pos);
  if (seen === null || seen < 0) return null;
  if (seen <= 100000000) return seen;
  const feedNow = finiteNumber(now);
  return feedNow !== null && feedNow >= seen ? feedNow - seen : null;
}

/* A failed transport leaves the last good readsb object in memory.  Add the
   wall-clock outage age separately so a frozen seen_pos cannot stay visible
   forever.  This remains pure and is therefore easy to exercise without WebGL. */
export function aircraftFreshnessAge(aircraft, now = Date.now() / 1000,
  feedAgeSeconds = 0) {
  const seenAge = seenPositionAgeSeconds(aircraft, now);
  if (seenAge === null) return null;
  const outageAge = Math.max(0, finiteNumber(feedAgeSeconds) ?? 0);
  return seenAge + outageAge;
}

export function isFreshAircraft(aircraft, now = Date.now() / 1000,
  maxAgeSeconds = AIRCRAFT_MAX_AGE_SECONDS, feedAgeSeconds = 0) {
  const maxAge = finiteNumber(maxAgeSeconds);
  const age = aircraftFreshnessAge(aircraft, now, feedAgeSeconds);
  return maxAge !== null && maxAge >= 0 && age !== null && age <= maxAge;
}

function validCoordinate(aircraft) {
  const lat = finiteNumber((aircraft || {}).lat);
  const lon = finiteNumber((aircraft || {}).lon);
  return lat !== null && lon !== null && lat >= -90 && lat <= 90 &&
    lon >= -180 && lon <= 180 ? { lat, lon } : null;
}

export function aircraftVisualStatus(aircraft) {
  const a = aircraft || {};
  if (a.emergency) return "emergency";
  if (a.rare) return "rare";
  if (a.match) return "match";
  return "normal";
}

/* 10 unique vertices + the closing vertex.  The local x axis is along the
   nose (metres north at heading 0) and y is the right wing. */
const AIRCRAFT_SILHOUETTE = [
  [58, 0], [17, 10], [11, 32], [-7, 21], [-37, 8],
  [-55, 0], [-37, -8], [-7, -21], [11, -32], [17, -10],
];

export function aircraftPolygon(aircraft, options = {}) {
  const position = validCoordinate(aircraft);
  if (!position) return null;
  const heading = aircraftHeading(aircraft);
  const radians = heading * Math.PI / 180;
  const forward = Math.sin(radians);
  const north = Math.cos(radians);
  const east = Math.cos(radians);
  const rightNorth = -Math.sin(radians);
  // The default silhouette is 113m nose-to-tail.  sizeMeters is the desired
  // exaggerated display length, not a multiplier, and is bounded to keep one
  // bad caller from producing a world-sized polygon.
  const displayLengthM = clamp(finiteNumber(options.sizeMeters) ?? 113, 80, 150);
  const size = displayLengthM / 113;
  const heightScale = finiteNumber(options.heightScale) ?? 1;
  const heightM = aircraftAltitudeMeters(aircraft) * Math.max(0, heightScale);
  const cosLat = Math.max(0.05, Math.abs(Math.cos(position.lat * Math.PI / 180)));

  const ring = AIRCRAFT_SILHOUETTE.map(([along, lateral]) => {
    const northM = (along * north + lateral * rightNorth) * size;
    const eastM = (along * forward + lateral * east) * size;
    return [
      position.lon + eastM / (EARTH_LAT_METERS * cosLat),
      position.lat + northM / EARTH_LAT_METERS,
      heightM,
    ];
  });
  // GeoJSON exterior rings are counter-clockwise in lon/lat space.  The
  // silhouette is authored nose -> right wing -> tail -> left wing in the
  // local north/east frame, but that order becomes clockwise after the
  // north/east values are written as [lon, lat].  Navara's worker keeps the
  // resulting triangle indices and PolygonMesh uses front-face culling, so a
  // clockwise ring makes the entire aircraft disappear from an overhead POV.
  // Reverse only the ring order; positions, altitude and attitude metadata
  // remain unchanged.
  ring.reverse();
  ring.push(ring[0].slice());
  return ring;
}

function aircraftFeature(aircraft, options = {}) {
  const ring = aircraftPolygon(aircraft, options);
  if (!ring) return null;
  const status = aircraftVisualStatus(aircraft);
  const heightM = aircraftAltitudeMeters(aircraft);
  return {
    type: "Feature",
    properties: {
      hex: aircraft.hex || "",
      callsign: aircraft.callsign || "",
      status,
      layerStatus: status,
      heightM,
      heading: aircraftHeading(aircraft),
      pitch: aircraftPitch(aircraft),
      roll: aircraftRoll(aircraft),
    },
    geometry: { type: "Polygon", coordinates: [ring] },
  };
}

/* Build at most twenty fresh polygons.  excludeHex is used during the
   first-person alert so the selected aircraft is not drawn through the camera. */
export function aircraftGeoJSON(aircraft = [], options = {}) {
  const now = options.now === undefined ? Date.now() / 1000 : options.now;
  const maxAge = options.maxAgeSeconds ?? AIRCRAFT_MAX_AGE_SECONDS;
  const requestedCount = finiteNumber(options.maxAircraft) ?? AIRCRAFT_MAX_COUNT;
  const maxCount = Math.max(0, Math.min(Math.floor(requestedCount), AIRCRAFT_MAX_COUNT));
  const exclude = options.excludeHex;
  const wantedStatus = options.status;
  if (maxCount === 0) return { type: "FeatureCollection", features: [] };
  const seenKeys = new Set();
  const features = [];
  const rows = (Array.isArray(aircraft) ? aircraft : []).slice().sort((a, b) => {
    const rank = { emergency: 0, rare: 1, match: 2, normal: 3 };
    return (rank[aircraftVisualStatus(a)] ?? 3) - (rank[aircraftVisualStatus(b)] ?? 3);
  });
  for (const a of rows) {
    const key = a && (a.hex || a.callsign);
    if (exclude && key === exclude) continue;
    if (key && seenKeys.has(key)) continue;
    if (!isFreshAircraft(a, now, maxAge, options.feedAgeSeconds) || !validCoordinate(a)) continue;
    if (wantedStatus && aircraftVisualStatus(a) !== wantedStatus &&
      !(wantedStatus === "alert" &&
        (aircraftVisualStatus(a) === "rare" || aircraftVisualStatus(a) === "emergency"))) continue;
    const feature = aircraftFeature(a, options);
    if (!feature) continue;
    if (key) seenKeys.add(key);
    features.push(feature);
    if (features.length >= maxCount) break;
  }
  return { type: "FeatureCollection", features };
}

export function aircraftGeoJSONByStatus(aircraft, status, options = {}) {
  const all = aircraftGeoJSON(aircraft, { ...options, status });
  return {
    type: "FeatureCollection",
    features: all.features,
  };
}

/* Camera coordinates are geodetic: height is metres above the ellipsoid.  Navara
   uses the same sign as the aircraft attitude convention: positive pitch is
   nose-up and negative pitch looks toward the surface.  The default 8m eye
   offset leaves the camera above the flat aircraft polygon; the polygon is
   intentionally a display silhouette, not a solid aircraft. */
export function aircraftCameraPose(aircraft, options = {}) {
  const position = validCoordinate(aircraft) || { lon: 0, lat: 0 };
  const eyeOffset = Math.max(1, finiteNumber(options.eyeOffsetM) ?? 8);
  return {
    lng: position.lon,
    lat: position.lat,
    height: aircraftAltitudeMeters(aircraft) + eyeOffset,
    heading: aircraftHeading(aircraft),
    pitch: clamp(aircraftPitch(aircraft), -AIRCRAFT_PITCH_LIMIT, AIRCRAFT_PITCH_LIMIT),
    roll: aircraftRoll(aircraft),
  };
}

function offsetGeodetic(position, eastM, northM) {
  const cosLat = Math.max(0.05, Math.abs(Math.cos(position.lat * Math.PI / 180)));
  const lat = clamp(position.lat + northM / EARTH_LAT_METERS, -89.9, 89.9);
  const rawLon = position.lon + eastM / (EARTH_LAT_METERS * cosLat);
  return {
    lat,
    lon: ((rawLon + 180) % 360 + 360) % 360 - 180,
  };
}

function aircraftVelocity(aircraft) {
  const speed = finiteNumber((aircraft || {}).gs);
  if (speed === null || speed < 0) return null;
  const heading = aircraftHeading(aircraft) * Math.PI / 180;
  const verticalRate = finiteNumber((aircraft || {}).geom_rate) ??
    finiteNumber((aircraft || {}).baro_rate) ?? 0;
  return {
    eastMps: Math.sin(heading) * speed * KNOT_TO_METERS_PER_SECOND,
    northMps: Math.cos(heading) * speed * KNOT_TO_METERS_PER_SECOND,
    upMps: verticalRate * VERTICAL_FEET_PER_MINUTE_TO_METERS_PER_SECOND,
  };
}

/* Dead-reckoning is deliberately short-lived and only used during an alert.
   It keeps the camera moving between the feed's roughly five-second updates;
   it is never used to keep a permanent animation loop alive. */
export function advanceAircraft(aircraft, seconds = 0) {
  const a = { ...(aircraft || {}) };
  const position = validCoordinate(a);
  const velocity = aircraftVelocity(a);
  const duration = clamp(finiteNumber(seconds) ?? 0, -8, 8);
  if (!position || !velocity || duration === 0) return a;
  const cosLat = Math.max(0.05, Math.abs(Math.cos(position.lat * Math.PI / 180)));
  a.lat = position.lat + velocity.northMps * duration / EARTH_LAT_METERS;
  a.lon = position.lon + velocity.eastMps * duration / (EARTH_LAT_METERS * cosLat);
  const altitudeM = Math.max(0, aircraftAltitudeMeters(a) + velocity.upMps * duration);
  // Keep the original altitude source visible to callers while making the
  // derived height usable by aircraftCameraPose without introducing NaN.
  a.alt_geom = altitudeM / FEET_TO_METERS;
  return a;
}

export function aircraftCameraPoseAt(aircraft, seconds = 0, options = {}) {
  return aircraftCameraPose(advanceAircraft(aircraft, seconds), options);
}

export function aircraftChasePose(aircraft, options = {}) {
  const position = validCoordinate(aircraft) || { lon: 0, lat: 0 };
  const heading = aircraftHeading(aircraft);
  const radians = heading * Math.PI / 180;
  const chaseDistance = clamp(finiteNumber(options.distanceM) ?? 700, 300, 2500);
  const aboveM = clamp(finiteNumber(options.aboveM) ?? 180, 80, 600);
  // Place the camera behind the aircraft along the reciprocal flight vector.
  // Heading remains the aircraft heading so the view looks forward through the
  // flight path, while the modest vertical offset keeps the chase readable.
  const chase = offsetGeodetic(position,
    -Math.sin(radians) * chaseDistance,
    -Math.cos(radians) * chaseDistance);
  return {
    lng: chase.lon,
    lat: chase.lat,
    height: aircraftAltitudeMeters(aircraft) + aboveM,
    heading,
    pitch: clamp(finiteNumber(options.pitch) ?? -12, -AIRCRAFT_PITCH_LIMIT, AIRCRAFT_PITCH_LIMIT),
    roll: 0,
  };
}

export function aircraftApproachPose(aircraft) {
  const eye = aircraftCameraPose(aircraft, { eyeOffsetM: 8 });
  return {
    ...eye,
    height: Math.max(eye.height + 400, 1200),
    pitch: -28,
    roll: 0,
  };
}

const AIRCRAFT_LAYER_DEFS = Object.freeze({
  normal: Object.freeze({ color: 0x21f39a }),
  match: Object.freeze({ color: 0xffb000 }),
  rare: Object.freeze({ color: 0xff2d95 }),
  emergency: Object.freeze({ color: 0xff5a36 }),
});

function emptyAircraftGeoJSON() {
  return { type: "FeatureCollection", features: [] };
}

function aircraftLayerDefinition(source, color) {
  return {
    type: "vector",
    source,
    polygon: {
      color,
      perPositionHeight: true,
      clampToGround: false,
      // These are small, dynamic world-space polygons.  GeoJSON-VT tiling can
      // quantize/drop their Z coordinates and also disables polygon outlines.
      tiled: false,
      // Keep the aircraft legible against both bright imagery and dark HUD
      // edges.  The unlit path is supported by PolygonMaterial and avoids a
      // directional-light/normal dependency on the N100; it still writes the
      // normal/depth buffers in Navara's deferred renderer.
      lit: false,
      transparent: false,
      opacity: 1,
      outline: true,
      outlineShow: true,
      outlineColor: color,
      outlineWidth: 2.5,
    },
  };
}

function aircraftFeatureSetKey(status, params) {
  const featureSetId = params && params.featureSetId;
  if (featureSetId === undefined || featureSetId === null) return null;
  return `${status}:${String(featureSetId)}`;
}

function notifyAircraftFeatureDiagnostics(state) {
  if (typeof state.onFeatureDiagnostics !== "function") return;
  try {
    state.onFeatureDiagnostics({
      renderedFeatureCount: state.renderedFeatureCount,
      featureCreatedCount: state.featureCreatedCount,
      featureRemovedCount: state.featureRemovedCount,
      featureEventsAvailable: state.featureEventsAvailable,
    });
  } catch (error) {
    // A diagnostics/UI callback must never interrupt Navara's feature event.
  }
}

function bindAircraftFeatureEvents(state, status, layer) {
  if (!layer || typeof layer.on !== "function" || typeof layer.off !== "function") {
    return false;
  }
  const onCreated = params => {
    const key = aircraftFeatureSetKey(status, params);
    if (key !== null) state.renderedFeatureKeys.add(key);
    state.featureCreatedCount += 1;
    state.renderedFeatureCount = state.renderedFeatureKeys.size;
    // Feature creation is asynchronous after source.update().  Request a
    // frame at the point the worker has actually produced renderable geometry.
    try { state.view?.forceUpdate?.(); } catch (error) { /* best effort */ }
    notifyAircraftFeatureDiagnostics(state);
  };
  const onRemoved = params => {
    const key = aircraftFeatureSetKey(status, params);
    if (key !== null) state.renderedFeatureKeys.delete(key);
    state.featureRemovedCount += 1;
    state.renderedFeatureCount = state.renderedFeatureKeys.size;
    try { state.view?.forceUpdate?.(); } catch (error) { /* best effort */ }
    notifyAircraftFeatureDiagnostics(state);
  };
  try {
    layer.on("featureCreated", onCreated);
    layer.on("featureRemoved", onRemoved);
  } catch (error) {
    try { layer.off("featureCreated", onCreated); } catch (e) { /* best effort */ }
    try { layer.off("featureRemoved", onRemoved); } catch (e) { /* best effort */ }
    return false;
  }
  state.featureListeners.push({ layer, onCreated, onRemoved });
  return true;
}

function disposeAircraftFeatureEvents(state) {
  for (const listener of state.featureListeners || []) {
    try { listener.layer?.off?.("featureCreated", listener.onCreated); } catch (e) { /* best effort */ }
    try { listener.layer?.off?.("featureRemoved", listener.onRemoved); } catch (e) { /* best effort */ }
  }
  if (state.featureListeners) state.featureListeners.length = 0;
  state.renderedFeatureKeys?.clear?.();
  state.renderedFeatureCount = 0;
  state.featureEventsAvailable = false;
  notifyAircraftFeatureDiagnostics(state);
}

function deleteAircraftLayerHandles(state) {
  // Remove event handlers before deleting the layer.  Navara may synchronously
  // emit featureRemoved while tearing down a source.
  disposeAircraftFeatureEvents(state);
  for (const status of Object.keys(state.layers || {})) {
    try { state.layers[status]?.delete?.(); } catch (e) { /* best effort */ }
    delete state.layers[status];
  }
  for (const status of Object.keys(state.sources || {})) {
    try { state.sources[status]?.delete?.(); } catch (e) { /* best effort */ }
    delete state.sources[status];
  }
}

function createAircraftLayerHandles(state, dataByStatus = {}) {
  const createdSources = [];
  const createdLayers = [];
  try {
    for (const [status, definition] of Object.entries(AIRCRAFT_LAYER_DEFS)) {
      const source = state.view.addSource({
        type: "geojson",
        data: dataByStatus[status] || emptyAircraftGeoJSON(),
        // Keep the small dynamic collection in world coordinates.  The
        // GeoJSON-VT path is intended for large datasets and can lose Z.
        tiled: false,
      });
      state.sources[status] = source;
      createdSources.push({ status, source });
      const layer = state.view.addLayer(aircraftLayerDefinition(source, definition.color));
      state.layers[status] = layer;
      createdLayers.push({ status, layer });
      bindAircraftFeatureEvents(state, status, layer);
    }
    state.featureEventsAvailable = state.featureListeners.length ===
      Object.keys(AIRCRAFT_LAYER_DEFS).length;
  } catch (error) {
    disposeAircraftFeatureEvents(state);
    for (const { status, layer } of createdLayers) {
      try { layer?.delete?.(); } catch (e) { /* best effort */ }
      delete state.layers[status];
    }
    for (const { status, source } of createdSources) {
      try { source?.delete?.(); } catch (e) { /* best effort */ }
      delete state.sources[status];
    }
    throw error;
  }
}

export function addPhotoJourneyAircraftLayer(view, options = {}) {
  const sources = {};
  const layers = {};
  const state = {
    view,
    sources,
    layers,
    disposed: false,
    visibleCount: 0,
    renderedFeatureCount: 0,
    featureCreatedCount: 0,
    featureRemovedCount: 0,
    featureEventsAvailable: false,
    featureListeners: [],
    renderedFeatureKeys: new Set(),
    needsInitialRebuild: true,
    onFeatureDiagnostics: typeof options?.onFeatureDiagnostics === "function"
      ? options.onFeatureDiagnostics
      : null,
  };
  createAircraftLayerHandles(state);
  notifyAircraftFeatureDiagnostics(state);
  return state;
}

export function updatePhotoJourneyAircraftLayer(state, aircraft, options = {}) {
  if (!state || state.disposed) return state;
  const common = {
    now: options.now,
    maxAgeSeconds: options.maxAgeSeconds,
    feedAgeSeconds: options.feedAgeSeconds,
    maxAircraft: options.maxAircraft,
    sizeMeters: options.sizeMeters,
    heightScale: options.heightScale,
    excludeHex: options.excludeHex,
  };
  const all = aircraftGeoJSON(aircraft, common);
  // Expose the post-filter count to the controller so an opaque HTML photo
  // card can yield a little transparency while real 3D aircraft are present.
  // This is only a HUD occlusion mitigation; aircraft remain world-space
  // polygons at their feed altitude.
  state.visibleCount = all.features.length;
  const grouped = {
    normal: emptyAircraftGeoJSON(),
    match: emptyAircraftGeoJSON(),
    rare: emptyAircraftGeoJSON(),
    emergency: emptyAircraftGeoJSON(),
  };
  for (const feature of all.features) {
    grouped[feature.properties.layerStatus]?.features.push(feature);
  }
  let rebuiltInitial = false;
  if (state.needsInitialRebuild && all.features.length > 0) {
    // Navara 0.1.1 can retain the empty inline GeoJSON traversal created at
    // addSource/addLayer time.  Recreate once with real data so the first
    // aircraft snapshot enters the feature pipeline; later updates are cheap
    // in-place source updates.
    deleteAircraftLayerHandles(state);
    createAircraftLayerHandles(state, grouped);
    state.needsInitialRebuild = false;
    rebuiltInitial = true;
  }
  for (const status of Object.keys(AIRCRAFT_LAYER_DEFS)) {
    const data = grouped[status] || emptyAircraftGeoJSON();
    // The initial rebuild already supplied inline data to addSource.  Avoid
    // immediately replacing those new sources before Navara can traverse them.
    if (!rebuiltInitial) {
      state.sources[status]?.update?.({ type: "geojson", data, tiled: false });
    }
  }
  // animation:false means a source mutation must explicitly request a frame.
  state.view?.forceUpdate?.();
  return state;
}

export function removePhotoJourneyAircraftLayer(state) {
  if (!state || state.disposed) return false;
  state.disposed = true;
  deleteAircraftLayerHandles(state);
  return true;
}

export function addPhotoJourneyLayers(view, { points = [], route = [], tileUrls = {}, basemap,
  terrainStyle = "photo", low = false } = {}) {
  const origin = typeof location !== "undefined" ? location.origin : "";
  const urls = {
    "gsi-photo": tileUrls.gsiPhoto || `${origin}/tiles/gsi-photo/{z}/{x}/{y}.png`,
    osm: tileUrls.osm || `${origin}/tiles/osm/{z}/{x}/{y}.png`,
    "gsi-dem": tileUrls.gsiDem || `${origin}/tiles/gsi-dem/{z}/{x}/{y}.png`,
  };
  // terrainをimageryより先に追加する（drape対象を確保してから航空写真を載せる）。
  const style = ["hybrid", "cyber"].includes(terrainStyle) ? terrainStyle : "photo";
  if (style !== "photo" && view.globe?.color)
    view.globe.color = view.globe.color.clone().setHex(0x304c64);
  const demSource = addTerrain(view, urls["gsi-dem"]);
  const kind = basemap || selectPhotoJourneyBasemap(points[0] || route[0]);
  const photoEnabled = style === "photo" || (style === "hybrid" && !low);
  const raster = photoEnabled && (style === "photo" || kind === "gsi-photo")
    ? view.addSource(rasterDefinition(kind, urls, style)) : null;
  const opacity = style === "photo" ? 1 : 0;
  const layer = raster ? view.addLayer({ type: "raster", source: raster,
    raster: { show: true, opacity } }) : null;

  // 出典表示（地理院タイル / OSM）。CDN に頼らずサーバー由来のタイルでも出典は必須。
  if (view.attribution) {
    view.attribution.add([
      {
        attribution: "航空・衛星画像・地形: 国土地理院 / データソース: Landsat8画像（GSI, TSIC, GEO Grid/AIST）、Landsat8画像（courtesy of the U.S. Geological Survey）、海底地形（GEBCO）、標高タイル（基盤地図情報数値標高モデル・全球数値地図）",
        attributionUrl: "https://maps.gsi.go.jp/development/ichiran.html",
      },
      {
        attribution: "© OpenStreetMap contributors",
        attributionUrl: "https://www.openstreetmap.org/copyright",
      },
    ]);
  }

  return { basemap: { kind, source: raster, urls, layer, opacity, style, view, imageryEnabled: photoEnabled },
    terrain: { source: demSource },
    groundLines: style === "photo" ? null : { view, low, source: null, layer: null } };
}

/* 写真の座標はカメラ位置ではなく注視点。地面からの高さと俯角で後退距離を
   決める。日付変更線・極付近でも破綻しない球面移動でカメラ座標を求める。
   航空機の絶対高度カメラはこの変換を通さない。 */
export function photoJourneyCameraPose(point, {
  height = 1400, pitch = -24, heading = 0, elevation = 0,
} = {}) {
  const lng = finiteNumber(point?.lon ?? point?.lng);
  const lat = finiteNumber(point?.lat);
  if (lng === null || lat === null || Math.abs(lng) > 180 || Math.abs(lat) > 90)
    return null;
  const rad = Math.PI / 180;
  const clearance = clamp(finiteNumber(height) ?? 1400, 400, 8000);
  const tilt = clamp(finiteNumber(pitch) ?? -24, -70, -18);
  const bearing = (normalizedHeading(heading) ?? 0) * rad;
  const distance = clearance / Math.tan(-tilt * rad);
  const arc = distance / 6378137;
  const phi = lat * rad;
  const back = bearing + Math.PI;
  const cameraLat = Math.asin(clamp(Math.sin(phi) * Math.cos(arc) +
    Math.cos(phi) * Math.sin(arc) * Math.cos(back), -1, 1));
  const deltaLng = Math.atan2(Math.sin(back) * Math.sin(arc) * Math.cos(phi),
    Math.cos(arc) - Math.sin(phi) * Math.sin(cameraLat));
  const cameraLng = ((lng + deltaLng / rad + 540) % 360) - 180;
  // 移動先から注視点への方位（高緯度では出発時の方位と一致しない）。
  const toTarget = Math.atan2(Math.sin(-deltaLng) * Math.cos(phi),
    Math.cos(cameraLat) * Math.sin(phi) -
    Math.sin(cameraLat) * Math.cos(phi) * Math.cos(deltaLng)) / rad;
  return {
    lng: cameraLng, lat: cameraLat / rad,
    height: clamp(finiteNumber(elevation) ?? 0, -500, 9000) + clearance,
    pitch: tilt, heading: ((toTarget % 360) + 360) % 360, roll: 0,
  };
}

export function photoJourneyElevation(view, point) {
  // applyManifestの擬似標高は除外。実標高があれば、粗い親DEMによる低い推定値で
  // 上書きしない（読み込み中の山でカメラが地面へ沈むことを防ぐ）。
  const known = point.realElev === true ? finiteNumber(point.intel?.elev) : null;
  if (known !== null) return clamp(known, -500, 9000);
  let sampled = null;
  try {
    sampled = finiteNumber(view.sampleTerrainHeight?.({
      lng: point.lon ?? point.lng, lat: point.lat,
    }));
  } catch (error) { /* DEM未準備でも巡回を続ける。 */ }
  return clamp(sampled ?? 0, -500, 9000);
}

/* 鑑賞用の斜光。実時刻の太陽は夜側では地面の下に入り、斜面の陰影が消える。
   巡回地点の地方太陽時を春分の15時へ揃える。大気テクスチャは読み込まない。 */
export function updatePhotoJourneyLighting(view, point) {
  const lng = finiteNumber(point?.lon ?? point?.lng);
  if (!view.atmosphere || lng === null) return;
  view.atmosphere.date = new Date(Date.UTC(2026, 2, 20, 15) - lng * 240000);
}

/* カメラを即時配置する。height はメートル（楕円体からの高度）。 */
export function setPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }) {
  view.setCamera({ lng, lat, height, heading, pitch, roll: 0 });
}

/* カメラを指定地点へ低頻度で飛ばす。完了時に true（別カメラ操作で割り込まれたら false）。 */
export function flyPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }, options = {}) {
  return view.flyTo({ lng, lat, height, pitch, heading }, { duration: options.duration ?? 4000 });
}
