/* ============================================================
   photo-journey-lite.js — 3D PHOTO JOURNEY Lite の実行時コントローラ
   ※読み込み順が契約: scheduler.js の後、main.js より前
   （起動時は manifestReady を await するため、参照するグローバルは
     呼び出し時点で必ず定義済み。順序は緩いが自然順に置く）

   宣言する主なグローバル: なし（IIFE に閉じる。GEOFRAME_MODE_3D は bootstrap.js）
   依存する他ファイルのグローバル: GEOFRAME_MODE_3D (bootstrap.js),
       DWELL_MS/LOW_POWER (config.js), PHOTOS/usingMock/manifestReady/reindexPhotos (data.js),
       playQueue/loadPlayState/syncSchedule/buildPlayOrder/nextQueueIdx/markShown
       (scheduler-core.js), showPhoto (photocard.js), setMode (modes.js),
       buildTicker (panels.js)

   役割:
     - ?mode=3d のときだけ Navara ThreeView（app/static/navara/ の自己完結バンドル）を
       動的 import で遅延ロードする。通常URLではこの import 文に到達しないため
       Navara の JS/WASM/worker 資産は一切読み込まれない。
     - manifest（PHOTOS）の初回地点だけで航空写真basemapを初期化し、最低30秒周期の
       低頻度カメラ遷移を回す。写真カード・AREA INTEL・HUD は通常モードと同じ
       showPhoto / buildTicker / setMode を再利用する。
     - Navara / WASM / worker / WebGL の初期化に失敗したら sessionStorage に
       フラグを立てて一度だけ通常(MapLibre)モードへフォールバックする。
       （リロードは bootstrap.js が mode=3d を剥がして実行。ループしない）
   ============================================================ */
(function(){
  if(!window.GEOFRAME_MODE_3D) return;

  const settings = window.GEOFRAME_PHOTO_JOURNEY_SETTINGS || {
    low: !!window.LOW_POWER, dwellMs: Math.max(DWELL_MS || 16000, 30000), flyMs: 5000,
    height: 1800, pitch: -50, headingStep: 47,
  };
  let refinementTimer = null;

  /* animation:falseでは、非同期で親タイルが届いた後の次LOD選択が自動で進まない場合がある。
     常時描画には戻さず、カメラ配置・到着後だけ低頻度で更新してZ14以上まで精細化する。 */
  function requestLodRefinement(view){
    if(refinementTimer) clearInterval(refinementTimer);
    const duration = settings.low ? 1200 : 12000;
    const interval = settings.low ? 400 : 200;
    const endAt = performance.now() + duration;
    view.forceUpdate?.();
    refinementTimer = setInterval(()=>{
      view.forceUpdate?.();
      if(performance.now() >= endAt){
        clearInterval(refinementTimer);
        refinementTimer = null;
      }
    }, interval);
  }

  /* Navara/WASM/worker/WebGL の初期化失敗時の一度きりのフォールバック。
     bootstrap.js は geoframe.3d.fallback==='1' を検知すると mode=3d を剥がして
     リロードし、その際にフラグを消すためリロードループは生じない。 */
  function fallbackToMapLibre(err){
    console.warn('[3d] Navara初期化に失敗 → 通常(MapLibre)モードへフォールバック', err);
    try{ sessionStorage.setItem('geoframe.3d.fallback', '1'); }catch(e){}
    location.reload();
  }

  /* 初回地点だけをNavaraへ渡す。全3,099地点のGeoJSON化は航空写真LODを圧迫する。 */
  function toInitialPoint(index){
    const p = PHOTOS[index] || PHOTOS[0];
    return p ? [{ lon: p.lon, lat: p.lat, title: p.title || '' }] : [];
  }

  async function boot(){
    // 通常モードと同じ初期化を流用（manifest反映 → 再生状態復元 → 差分同期 → キュー構築）
    try{ await manifestReady; }catch(e){ /* file://直開き等はモックで継続 */ }
    reindexPhotos();
    loadPlayState();
    syncSchedule();
    if(!playQueue.length) buildPlayOrder();
    buildTicker();   // HUDティッカーは通常モードと共通
    setMode('3D PHOTO JOURNEY LITE');

    // Navara バンドルを遅延ロード（通常URLではここに到達しない）
    let mod;
    try{
      mod = await import('/static/navara/photo-journey-lite.js');
    }catch(e){ return fallbackToMapLibre(e); }

    const container = document.getElementById('modelayer');
    if(!container) return fallbackToMapLibre(new Error('#modelayer が見つからない'));

    let view;
    let layerState;
    const first = nextQueueIdx();
    try{
      view = mod.createPhotoJourneyView(container, { low: settings.low });
      await view.init();               // WebGLコンテキスト生成（失敗時はthrow→フォールバック）
      // Navara公式の軽量GIS例と同じく、ラスタ追加前にアンビエント光を置く。
      // unlitへの切替はラスタ合成を失わせるため使用しない。
      view.addLight({ ambient: { intensity: 1 } });
      const points = toInitialPoint(first);

      /* raster layer は追加時点のカメラから可視タイルを選ぶ。ThreeView の既定カメラの
         まま先に layer を追加すると、animation:false の on-demand mode では最初の
         flyTo 後も白い楕円体だけが残ることがある。初回地点へ即時配置してから layer
         を追加し、初期タイル選択を実際の表示地点に固定する。 */
      if(first >= 0){
        const p = PHOTOS[first];
        mod.setPhotoJourneyCamera(view, {
          lng: p.lon,
          lat: p.lat,
          height: settings.height,
          pitch: settings.pitch,
          heading: settings.headingStep ? (first * settings.headingStep) % 360 : 0,
        });
      }
      layerState = mod.addPhotoJourneyLayers(view, { points, route: [] });
      requestLodRefinement(view);
    }catch(e){ return fallbackToMapLibre(e); }

    // The initial setCamera above is intentionally the first frame. Calling
    // flyTo synchronously in the same turn can replace the not-yet-processed
    // camera change and restart from Navara's default globe camera.
    if(first >= 0) tick(view, mod, first, layerState, true);
    else setTimeout(() => tick(view, mod, nextQueueIdx(), layerState), 1000);
  }

  function tick(view, mod, i, layerState, initial = false){
    if(i < 0){ setTimeout(() => tick(view, mod, nextQueueIdx(), layerState), 1000); return; }
    markShown(i);
    const p = PHOTOS[i];
    showPhoto(i);                 // 写真カード + AREA INTEL（通常モードと共通）
    setMode('3D PHOTO JOURNEY LITE');
    // LOW_POWERは高めの高度・固定方位・長い滞在でLOD選択と更新回数を抑える。
    const height  = settings.height + (settings.low ? 0 : (i % 3) * 400);
    const heading = settings.headingStep ? (i * settings.headingStep) % 360 : 0;
    if(layerState && mod.updatePhotoJourneyBasemap){
      mod.updatePhotoJourneyBasemap(layerState.basemap, p);
      requestLodRefinement(view);
    }
    if(initial){
      // Let the camera placed before layer registration own the first view.
      // Subsequent points use flyTo as usual.
      setTimeout(() => tick(view, mod, nextQueueIdx(), layerState), settings.dwellMs);
      return;
    }
    Promise.resolve(
      mod.flyPhotoJourneyCamera(
        view,
        { lng: p.lon, lat: p.lat, height, pitch: settings.pitch, heading },
        { duration: settings.flyMs }
      )
    ).then(()=> requestLodRefinement(view)).catch(()=>{}).finally(()=>{
      setTimeout(() => tick(view, mod, nextQueueIdx(), layerState), settings.dwellMs);
    });
  }

  boot();
})();
