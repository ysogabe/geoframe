/* ============================================================
   frontend/photo-journey-lite.js — 3D PHOTO JOURNEY Lite の Navara アダプタ
   ============================================================
   `npm run build:navara`（vite build）がこのファイルをエントリとして
   app/static/navara/ に自己完結の ES バンドルを生成する。

   N100（低スペック）向けの「軽量構成」をここで唯一の形に固定する:
     - devicePixelRatio は 1 以下（画素を4倍にしない）
     - shadow 無効、MSAA 無効、halfFloat 無効
     - DefaultPlugin は addPlugin() で登録するのみ。addDefaultPhotorealScene() は
       呼ばない（SSR/SSAO/DOF/大気/雲/ボリュームを一切有効化しない）
     - 出典表示は view.attribution へ GSI / OSM を登録する

   依存する Navara API（@navaramap/three 0.1.1）:
     - new ThreeView({ container, pixelRatio, shadow, ... })
     - view.addPlugin(new DefaultPlugin()) → await view.init()
     - view.addSource({ type: "raster-tile" | "geojson", ... }) → Source ハンドル
     - view.addLayer({ type: "raster" | "vector", source, ... })
     - view.setCamera({ lng, lat, height, heading, pitch, roll })
     - view.flyTo({ lng, lat, height, pitch, heading }, { duration })
     - source.update({ type: "raster-tile", url, ... })
     - view.attribution?.add([{ attribution, attributionUrl }])
   ============================================================ */
import ThreeView from "@navaramap/three";
import { DefaultPlugin } from "@navaramap/three-default-plugin";

/* 軽量構成。N100相当の内蔵GPUでタイルの先読みと常時描画を抑える上限値。 */
const NORMAL_OPTIONS = Object.freeze({
  pixelRatio: 1,        // devicePixelRatio の上限（高DPIでも1に固定）
  shadow: false,        // 影は初期化時オプション。N100 では無効
  multisampling: 0,     // MSAA 無効（0）
  halfFloat: false,     // ポスト処理は低精度で十分
  picking: false,       // フィーチャーピッキング無効（タップ対象が無い）
  animation: false,     // flyTo/forceUpdate時だけ描画（Navaraのon-demand mode）
  mobileOptimization: true, // 低画素比などのモバイル向け軽量化を有効化
  cacheBytes: 128 * 1024 * 1024,
  lodFog: { enabled: true, density: 5e-4, sseFactor: 4 },
  dynamicSse: {
    enabled: true, density: 2e-4, sseFactor: 32,
    heightFalloff: 0.25, minHeight: 0, maxHeight: 8000,
  },
  memoryBudget: {
    maxWorkerHeapBytes: 128 * 1024 * 1024,
    fontBudgetBytes: 8 * 1024 * 1024,
    maxPendingRequests: 4,
    sseMultiplierMin: 2,
    sseMultiplierMax: 24,
  },
});

const LOW_POWER_OPTIONS = Object.freeze({
  ...NORMAL_OPTIONS,
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
    multisampling: opts.multisampling ?? defaults.multisampling,
    halfFloat: opts.halfFloat ?? defaults.halfFloat,
    picking: opts.picking ?? defaults.picking,
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

/* 写真ポイント・ライトルート線・背景タイルをレイヤとして追加する。
   tileUrls は server.py の /tiles/{gsi-photo,osm}/ プロキシを指す（CDN は使わない）。 */
function isDomesticPoint(point) {
  return !!point && point.lon >= 122.5 && point.lon <= 154.5 &&
    point.lat >= 20.0 && point.lat <= 46.5;
}

export function selectPhotoJourneyBasemap(point) {
  return isDomesticPoint(point) ? "gsi-photo" : "osm";
}

function rasterDefinition(kind, urls) {
  return {
    type: "raster-tile",
    url: urls[kind],
    minZoom: kind === "gsi-photo" ? 2 : 0,
    maxZoom: kind === "gsi-photo" ? 18 : 19,
  };
}

export function updatePhotoJourneyBasemap(state, point) {
  const next = selectPhotoJourneyBasemap(point);
  if (!state || state.kind === next) return state;
  state.source.update(rasterDefinition(next, state.urls));
  state.kind = next;
  return state;
}

export function addPhotoJourneyLayers(view, { points = [], route = [], tileUrls = {}, basemap } = {}) {
  const origin = typeof location !== "undefined" ? location.origin : "";
  const urls = {
    "gsi-photo": tileUrls.gsiPhoto || `${origin}/tiles/gsi-photo/{z}/{x}/{y}.png`,
    osm: tileUrls.osm || `${origin}/tiles/osm/{z}/{x}/{y}.png`,
  };
  const kind = basemap || selectPhotoJourneyBasemap(points[0] || route[0]);
  const raster = view.addSource(rasterDefinition(kind, urls));
  view.addLayer({ type: "raster", source: raster });

  const features = points.map((p) => ({
    type: "Feature",
    properties: { title: p.title || "" },
    geometry: { type: "Point", coordinates: [p.lon, p.lat] },
  }));
  if (route.length > 1) {
    features.push({
      type: "Feature",
      properties: {},
      geometry: { type: "LineString", coordinates: route.map((p) => [p.lon, p.lat]) },
    });
  }
  const geo = view.addSource({
    type: "geojson",
    data: { type: "FeatureCollection", features },
  });
  view.addLayer({ type: "vector", source: geo, point: { color: 0x00e5ff } });
  view.addLayer({ type: "vector", source: geo, polyline: { color: 0x00e5ff } });

  // 出典表示（地理院タイル / OSM）。CDN に頼らずサーバー由来のタイルでも出典は必須。
  if (view.attribution) {
    view.attribution.add([
      {
        attribution: "航空・衛星画像: 国土地理院 / データソース: Landsat8画像（GSI, TSIC, GEO Grid/AIST）、Landsat8画像（courtesy of the U.S. Geological Survey）、海底地形（GEBCO）",
        attributionUrl: "https://maps.gsi.go.jp/development/ichiran.html",
      },
      {
        attribution: "© OpenStreetMap contributors",
        attributionUrl: "https://www.openstreetmap.org/copyright",
      },
    ]);
  }

  return { basemap: { kind, source: raster, urls } };
}

/* カメラを即時配置する。height はメートル（楕円体からの高度）。 */
export function setPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }) {
  view.setCamera({ lng, lat, height, heading, pitch, roll: 0 });
}

/* カメラを指定地点へ低頻度で飛ばす。完了時に true（別カメラ操作で割り込まれたら false）。 */
export function flyPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }, options = {}) {
  return view.flyTo({ lng, lat, height, pitch, heading }, { duration: options.duration ?? 4000 });
}
