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
     - view.attribution?.add([{ attribution, attributionUrl }])
   ============================================================ */
import ThreeView from "@navaramap/three";
import { DefaultPlugin } from "@navaramap/three-default-plugin";

/* 軽量構成。N100 実機の 30fps / 24fps ターゲットを守るための上限値。 */
export const LITE_OPTIONS = Object.freeze({
  pixelRatio: 1,        // devicePixelRatio の上限（高DPIでも1に固定）
  shadow: false,        // 影は初期化時オプション。N100 では無効
  multisampling: 0,     // MSAA 無効（0）
  halfFloat: false,     // ポスト処理は低精度で十分
  picking: false,       // フィーチャーピッキング無効（タップ対象が無い）
  animation: true,      // 常時描画（カメラ遷移があるため）
  mobileOptimization: true, // 低画素比などのモバイル向け軽量化を有効化
});

/* ThreeView を生成し DefaultPlugin を登録する。init() は呼び出し側で await する。
   ※ addDefaultPhotorealScene() は意図的に呼ばない（大気・雲・SSR/SSAO/DOF を排除）。 */
export function createPhotoJourneyView(container, opts = {}) {
  const view = new ThreeView({
    container,
    pixelRatio: Math.min(opts.pixelRatio ?? LITE_OPTIONS.pixelRatio, 1),
    shadow: opts.shadow ?? LITE_OPTIONS.shadow,
    multisampling: opts.multisampling ?? LITE_OPTIONS.multisampling,
    halfFloat: opts.halfFloat ?? LITE_OPTIONS.halfFloat,
    picking: opts.picking ?? LITE_OPTIONS.picking,
    animation: opts.animation ?? LITE_OPTIONS.animation,
    mobileOptimization: opts.mobileOptimization ?? LITE_OPTIONS.mobileOptimization,
  });
  view.addPlugin(new DefaultPlugin());
  return view;
}

/* 写真ポイント・ライトルート線・背景タイルをレイヤとして追加する。
   tileUrls は server.py の /tiles/{gsi,osm}/ プロキシを指す（CDN は使わない）。 */
export function addPhotoJourneyLayers(view, { points = [], route = [], tileUrls = {} } = {}) {
  const origin = typeof location !== "undefined" ? location.origin : "";
  const gsiUrl = tileUrls.gsi || `${origin}/tiles/gsi/{z}/{x}/{y}.png`;
  const osmUrl = tileUrls.osm || `${origin}/tiles/osm/{z}/{x}/{y}.png`;

  const gsi = view.addSource({
    type: "raster-tile",
    url: gsiUrl,
    minZoom: 5,
    maxZoom: 18,
  });
  view.addLayer({ type: "raster", source: gsi });

  const osm = view.addSource({
    type: "raster-tile",
    url: osmUrl,
    maxZoom: 19,
  });
  view.addLayer({ type: "raster", source: osm });

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
        attribution: "地図データ 出典: 国土地理院（地理院タイル）",
        attributionUrl: "https://maps.gsi.go.jp/development/ichiran.html",
      },
      {
        attribution: "© OpenStreetMap contributors",
        attributionUrl: "https://www.openstreetmap.org/copyright",
      },
    ]);
  }
}

/* カメラを即時配置する。height はメートル（楕円体からの高度）。 */
export function setPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }) {
  view.setCamera({ lng, lat, height, heading, pitch, roll: 0 });
}

/* カメラを指定地点へ低頻度で飛ばす。完了時に true（別カメラ操作で割り込まれたら false）。 */
export function flyPhotoJourneyCamera(view, { lng, lat, height = 2000, pitch = -30, heading = 0 }, options = {}) {
  return view.flyTo({ lng, lat, height, pitch, heading }, { duration: options.duration || 4000 });
}
