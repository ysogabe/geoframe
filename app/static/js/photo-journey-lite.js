/* ============================================================
   photo-journey-lite.js — 3D PHOTO JOURNEY Lite の実行時コントローラ

   3D側のNavara viewはこのIIFEの中だけに保持する。MapLibre側のevents.js
   とはCustomEventの小さな境界で接続し、windowへviewを公開しない。

   重要な負荷制御:
     - 通常巡回は写真のflyToと30秒以上の滞在だけ
     - 可視タイルが落ち着いた後だけ、現在/次地点の小さなタイルリングを低速先読み
     - Navaraはanimation:false。タイル精細化は期限付きforceUpdateのみ
     - 航空機アラート中だけ、4〜5HzのsetIntervalで短時間デッドレコニング
     - requestAnimationFrame、常時描画、重いポストエフェクトは使わない
   ============================================================ */
(function(){
  if(!window.GEOFRAME_MODE_3D) return;

  const settings = window.GEOFRAME_PHOTO_JOURNEY_SETTINGS || {
    low: !!window.LOW_POWER,
    dwellMs: Math.max(typeof DWELL_MS !== 'undefined' ? DWELL_MS : 16000, 30000), flyMs: 5000,
    height: 1800, pitch: -50, headingStep: 47,
  };
  const AIRCRAFT_UPDATE_EVENT = window.GEOFRAME_AIRCRAFT_EVENT || 'geoframe:aircraft-update';
  const AIRCRAFT_ALERT_EVENT = window.GEOFRAME_AIRCRAFT_ALERT_EVENT || 'geoframe:aircraft-alert';
  const AIRCRAFT_ALERT_END_EVENT = window.GEOFRAME_AIRCRAFT_ALERT_END_EVENT ||
    'geoframe:aircraft-alert-end';

  let view = null;
  let mod = null;
  let layerState = null;
  let aircraftLayerState = null;
  let latestAircraft = [];
  let latestFeedNow = Date.now()/1000;
  let lastSuccessfulFeedWallTime = latestFeedNow;
  let latestAlert = null;
  let activeAlert = null;
  let sleeping = !!(window.GEOFRAME_EVENT_STATE &&
    window.GEOFRAME_EVENT_STATE.isSleeping && window.GEOFRAME_EVENT_STATE.isSleeping());
  let journeyTimer = null;
  let journeyGeneration = 0;
  let alertTimer = null;
  let alertGeneration = 0;
  let refinementTimer = null;
  let prefetchPlanTimer = null;
  let prefetchRateTimer = null;
  let prefetchInFlight = 0;
  const prefetchQueue = [];
  const prefetchKnown = new Set();
  const prefetchStarts = [];
  let disposed = false;
  let navaraFailed = false;
  const listeners = [];

  const nowMs = () => (typeof performance !== 'undefined' && performance.now)
    ? performance.now() : Date.now();

  function finite(value){
    const number = Number(value);
    return Number.isFinite(number) ? number : null;
  }

  function aircraftKey(a){
    return a && (a.hex || a.callsign) ? String(a.hex || a.callsign) : '';
  }

  function findAircraft(a){
    const key = aircraftKey(a);
    if(!key) return null;
    return latestAircraft.find(item => aircraftKey(item) === key) || null;
  }

  function validAircraft(a){
    const lat = finite(a && a.lat);
    const lon = finite(a && a.lon);
    return lat !== null && lon !== null && lat >= -90 && lat <= 90 &&
      lon >= -180 && lon <= 180;
  }

  function updateAircraftVisibility(){
    if(!aircraftLayerState || disposed) return;
    const hasFeatureEvents = aircraftLayerState.featureEventsAvailable === true;
    const renderedCount = finite(aircraftLayerState.renderedFeatureCount);
    // Navara layers expose featureCreated/featureRemoved.  Minimal test shims
    // and older adapters may not, so only those environments use visibleCount.
    const visible = hasFeatureEvents
      ? renderedCount !== null && renderedCount > 0
      : (finite(aircraftLayerState.visibleCount) || 0) > 0;
    document.body?.classList.toggle('aircraft-visible', visible);
    // Keep the async render proof inspectable from N100/CDP without exposing
    // the Navara view globally.
    if(document.body?.dataset){
      document.body.dataset.aircraftFeatureEvents = hasFeatureEvents ? 'true' : 'false';
      document.body.dataset.aircraftRenderedFeatureCount = String(
        hasFeatureEvents && renderedCount !== null ? renderedCount : 0);
      document.body.dataset.aircraftFeatureCreatedCount = String(
        finite(aircraftLayerState.featureCreatedCount) || 0);
    }
  }

  function eventState(){
    return window.GEOFRAME_EVENT_STATE || {};
  }

  function isSleeping(){
    const state = eventState();
    return sleeping || !!(state.isSleeping && state.isSleeping());
  }

  function eventUntil(){
    const value = eventState().getEventUntil;
    const until = typeof value === 'function' ? finite(value()) : null;
    return until || 0;
  }

  function isEventActive(){
    const state = eventState();
    return !!(state.isEventActive && state.isEventActive());
  }

  function isPaused(){
    return isSleeping() || !!activeAlert || isEventActive();
  }

  function listen(name, handler){
    if(typeof window.addEventListener !== 'function') return;
    window.addEventListener(name, handler);
    listeners.push([name, handler]);
  }

  /* speculative tile warm-up is deliberately a browser-side consumer of the
     existing /tiles/ proxy.  This keeps GSI attribution, persistent cache, and
     the server's shared upstream budget in one path; no bulk-download endpoint
     is added. */
  const PREFETCH_NORMAL_PER_MINUTE = 60;
  const PREFETCH_LOW_PER_MINUTE = 24;
  const PREFETCH_NORMAL_GAP_MS = 1000;
  const PREFETCH_LOW_GAP_MS = 2500;
  const PREFETCH_CONCURRENCY = 2;
  const PREFETCH_LOW_CONCURRENCY = 1;
  const PREFETCH_MAX_QUEUE = 128;
  const PREFETCH_KNOWN_MAX = 1024;

  function prefetchFetcher(){
    if(typeof window.fetch === 'function') return window.fetch.bind(window);
    if(typeof fetch === 'function') return fetch;
    return null;
  }

  function clearPrefetchWork(){
    if(prefetchPlanTimer){ clearTimeout(prefetchPlanTimer); prefetchPlanTimer=null; }
    if(prefetchRateTimer){ clearTimeout(prefetchRateTimer); prefetchRateTimer=null; }
    for(const url of prefetchQueue) prefetchKnown.delete(url);
    prefetchQueue.length=0;
  }

  function trimPrefetchStarts(now){
    while(prefetchStarts.length && now-prefetchStarts[0] >= 60000) prefetchStarts.shift();
  }

  function schedulePrefetchRatePump(delay){
    if(prefetchRateTimer || disposed) return;
    prefetchRateTimer=setTimeout(()=>{
      prefetchRateTimer=null;
      pumpPrefetch();
    }, Math.max(100, delay));
  }

  function pumpPrefetch(){
    const fetcher=prefetchFetcher();
    if(disposed || !fetcher) return;
    if(isPaused()){
      clearPrefetchWork();
      return;
    }
    const now=Date.now();
    trimPrefetchStarts(now);
    const limit=settings.low ? PREFETCH_LOW_PER_MINUTE : PREFETCH_NORMAL_PER_MINUTE;
    const gap=settings.low ? PREFETCH_LOW_GAP_MS : PREFETCH_NORMAL_GAP_MS;
    const concurrency=settings.low ? PREFETCH_LOW_CONCURRENCY : PREFETCH_CONCURRENCY;
    while(prefetchQueue.length && prefetchInFlight < concurrency){
      const current=Date.now();
      trimPrefetchStarts(current);
      if(prefetchStarts.length >= limit){
        schedulePrefetchRatePump(60000-(current-prefetchStarts[0])+50);
        return;
      }
      const last=prefetchStarts.at(-1);
      if(last !== undefined && current-last < gap){
        schedulePrefetchRatePump(gap-(current-last)+20);
        return;
      }
      const url=prefetchQueue.shift();
      prefetchInFlight++;
      prefetchStarts.push(current);
      Promise.resolve()
        .then(()=>fetcher(url,{cache:'force-cache'}))
        .then(response=>{
          const cacheStatus=response?.headers?.get?.('X-Tile-Cache');
          if(!response?.ok || cacheStatus === 'error' || cacheStatus === 'throttled')
            prefetchKnown.delete(url);
          // Consume the body so the server-side cache write is not abandoned by
          // the browser even though the decoded image is not displayed here.
          return response?.arrayBuffer?.();
        })
        .catch(()=>{ prefetchKnown.delete(url); })
        .finally(()=>{
          prefetchInFlight=Math.max(0,prefetchInFlight-1);
          pumpPrefetch();
        });
    }
  }

  function enqueuePrefetchUrls(urls){
    if(!prefetchFetcher() || !Array.isArray(urls)) return;
    for(const url of urls){
      if(typeof url !== 'string' || !url || prefetchKnown.has(url)) continue;
      if(prefetchQueue.length >= PREFETCH_MAX_QUEUE) break;
      prefetchKnown.add(url);
      prefetchQueue.push(url);
      // Keep the in-memory de-duplication bounded even during an all-day kiosk run.
      if(prefetchKnown.size > PREFETCH_KNOWN_MAX){
        const oldest=prefetchKnown.values().next().value;
        if(oldest) prefetchKnown.delete(oldest);
      }
    }
    pumpPrefetch();
  }

  function schedulePhotoPrefetch(targetMod, current, next){
    if(disposed || isPaused() || !prefetchFetcher() ||
       typeof targetMod?.getPhotoJourneyPrefetchTiles !== 'function') return;
    if(prefetchPlanTimer) clearTimeout(prefetchPlanTimer);
    // Let the visible Navara burst/refinement finish before spending the shared
    // upstream budget on speculative work.
    const delay=settings.low ? 5000 : 12500;
    prefetchPlanTimer=setTimeout(()=>{
      prefetchPlanTimer=null;
      if(disposed || isPaused()) return;
      const origin=typeof location !== 'undefined' ? location.origin : '';
      for(const point of [current,next]){
        if(!point) continue;
        try{
          enqueuePrefetchUrls(targetMod.getPhotoJourneyPrefetchTiles(point, {
            low:settings.low, origin,
          }));
        }catch(error){
          console.warn('[3d] speculative tile prefetch skipped',error);
        }
      }
    }, delay);
  }

  function clearRefinement(){
    if(refinementTimer){ clearInterval(refinementTimer); refinementTimer=null; }
  }

  /* animation:falseでは、非同期で親タイルが届いた後の次LOD選択が自動で進まない場合がある。
     常時描画には戻さず、カメラ配置・到着後だけ低頻度で更新して精細化する。 */
  function requestLodRefinement(targetView){
    clearRefinement();
    if(!targetView || disposed) return;
    const duration = settings.low ? 1200 : 12000;
    const interval = settings.low ? 400 : 200;
    const endAt = nowMs() + duration;
    try{ targetView.forceUpdate?.(); }catch(e){ return handleNavaraError(e); }
    refinementTimer = setInterval(()=>{
      if(disposed || activeAlert || isSleeping() || isEventActive()) return clearRefinement();
      try{ targetView.forceUpdate?.(); }
      catch(e){ clearRefinement(); handleNavaraError(e); return; }
      if(nowMs() >= endAt) clearRefinement();
    }, interval);
  }

  /* Navara/WASM/worker/WebGL の初期化失敗時の一度きりのフォールバック。 */
  function fallbackToMapLibre(err){
    console.warn('[3d] Navara初期化に失敗 → 通常(MapLibre)モードへフォールバック', err);
    try{ sessionStorage.setItem('geoframe.3d.fallback', '1'); }catch(e){}
    try{ location.reload(); }catch(e){}
  }

  function disposeController(){
    if(disposed) return;
    disposed=true;
    if(journeyTimer){ clearTimeout(journeyTimer); journeyTimer=null; }
    clearRefinement();
    if(alertTimer){ clearInterval(alertTimer); alertTimer=null; }
    clearPrefetchWork();
    journeyGeneration++;
    alertGeneration++;
    for(const [name, handler] of listeners){
      try{ window.removeEventListener?.(name, handler); }catch(e){}
    }
    listeners.length=0;
    try{ mod?.removePhotoJourneyAircraftLayer?.(aircraftLayerState); }catch(e){}
    document.body?.classList.remove('aircraft-visible');
  }

  function handleNavaraError(err){
    if(navaraFailed || disposed) return;
    navaraFailed=true;
    console.warn('[3d] Navara runtime error', err);
    disposeController();
    fallbackToMapLibre(err);
  }

  /* 初回地点だけをNavaraへ渡す。全地点をGeoJSON化すると航空写真LODを圧迫する。 */
  function toInitialPoint(index){
    const p = PHOTOS[index] || PHOTOS[0];
    return p ? [{ lon: p.lon, lat: p.lat, title: p.title || '' }] : [];
  }

  function updateAircraftLayer(){
    if(!mod || !aircraftLayerState || disposed) return;
    const target = activeAlert && activeAlert.aircraft;
    try{
      mod.updatePhotoJourneyAircraftLayer(aircraftLayerState, latestAircraft, {
        now: latestFeedNow,
        feedAgeSeconds: Math.max(0, Date.now()/1000 - lastSuccessfulFeedWallTime),
        maxAircraft: 20,
        maxAgeSeconds: 60,
        excludeHex: target ? aircraftKey(target) : undefined,
        // Keep the real altitude and world-space placement, but give an
        // 80–150m silhouette enough projected area at the fixed 2560x1080
        // camera height to survive pixelRatio=0.5 on N100.
        sizeMeters: settings.low ? 125 : 140,
      });
      updateAircraftVisibility();
    }catch(e){ handleNavaraError(e); }
  }

  function updateBasemapForAircraft(a){
    if(!a || !layerState || !mod?.updatePhotoJourneyBasemap) return;
    try{
      mod.updatePhotoJourneyBasemap(layerState.basemap, a);
      if(activeAlert) view.forceUpdate?.();
      else requestLodRefinement(view);
    }catch(e){ handleNavaraError(e); }
  }

  function cancelJourney(){
    journeyGeneration++;
    if(journeyTimer){ clearTimeout(journeyTimer); journeyTimer=null; }
  }

  function clearAlertAnimation(){
    alertGeneration++;
    if(alertTimer){ clearInterval(alertTimer); alertTimer=null; }
  }

  function scheduleJourney(delay=0){
    if(disposed || isSleeping() || activeAlert) return;
    if(journeyTimer) clearTimeout(journeyTimer);
    const until = eventUntil();
    if(isEventActive() || until > Date.now()){
      // A seismic event has no aircraft-specific callback. Poll the read-only
      // state at a single low-frequency deadline and resume afterwards.
      journeyTimer = setTimeout(()=>{ journeyTimer=null; scheduleJourney(0); },
        Math.max(250, Math.min(5000, until > Date.now() ? until-Date.now()+50 : 1000)));
      return;
    }
    journeyTimer = setTimeout(()=>{
      journeyTimer=null;
      if(!isPaused()) tick(view, mod, nextQueueIdx(), layerState);
    }, Math.max(0, delay));
  }

  function alertTarget(alert){
    return findAircraft(alert.aircraft) || alert.aircraft;
  }

  function applyAlertCamera(alert){
    if(!view || !mod || !activeAlert || activeAlert !== alert || isSleeping()) return;
    const source = alertTarget(alert);
    if(!source) return;
    const current = findAircraft(source);
    const target = current || source;
    const seen = finite(target.seen_pos) ?? 0;
    const feedAge = Math.max(0, Date.now()/1000 - latestFeedNow);
    // Do not extrapolate an aircraft that has disappeared from the feed.
    const seconds = alert.missing ? 0 : Math.min(8, seen + feedAge);
    const projected = mod.advanceAircraft ? mod.advanceAircraft(target, seconds) : target;
    const pose = alert.kind === 'match'
      ? (mod.aircraftCameraPoseAt
        ? mod.aircraftCameraPoseAt(target, seconds, {eyeOffsetM:8})
        : mod.aircraftCameraPose(projected, {eyeOffsetM:8}))
      : mod.aircraftChasePose
        ? mod.aircraftChasePose(projected)
        : mod.aircraftApproachPose(projected);
    view.setCamera(pose);
    view.forceUpdate?.();
  }

  function startAlertInterpolation(alert, token){
    clearAlertAnimation();
    const interval = settings.low ? 250 : 200; // 4Hz low-power / 5Hz normal
    const run = ()=>{
      if(disposed || !activeAlert || activeAlert !== alert ||
         activeAlert.token !== token || isSleeping()) return;
      if(Date.now() >= alert.until){ finishAlert(alert.until); return; }
      try{ applyAlertCamera(alert); }
      catch(e){ clearAlertAnimation(); handleNavaraError(e); }
    };
    run();
    if(activeAlert === alert && !disposed) alertTimer=setInterval(run, interval);
  }

  function finishAlert(until, reason='ended'){
    if(activeAlert && until && activeAlert.until !== until) return;
    const ended = activeAlert;
    if(ended) activeAlert=null;
    if(latestAlert && (!until || latestAlert.until === until)) latestAlert=null;
    clearAlertAnimation();
    clearRefinement();
    updateAircraftLayer(); // selected aircraft becomes visible again
    if(reason === 'target-disappeared' && !isSleeping() && !isEventActive())
      setMode('3D PHOTO JOURNEY LITE');
    if(!disposed && !isSleeping()) scheduleJourney(150);
    return !!ended;
  }

  function startAlert(alert, force=false){
    if(disposed || !view || !mod || !alert || !alert.aircraft) return;
    const until = finite(alert.until) || (Date.now()+20000);
    if(until <= Date.now()) return;
    const sameAlert = activeAlert && activeAlert.until === until &&
      aircraftKey(activeAlert.aircraft) === aircraftKey(alert.aircraft);
    if(sameAlert && !force) return;
    cancelJourney();
    clearAlertAnimation();
    clearRefinement();
    clearPrefetchWork();
    const target = alertTarget(alert);
    const next = {
      aircraft: target,
      kind: alert.kind || 'normal',
      until,
      missing: !findAircraft(target),
      stage: 'focus',
      startedAt: Date.now(),
      token: ++alertGeneration,
    };
    activeAlert=next;
    updateAircraftLayer(); // never draw the polygon through the camera
    updateBasemapForAircraft(target);
    try{
      const focus = next.kind === 'match'
        ? mod.aircraftApproachPose(target)
        : (mod.aircraftChasePose ? mod.aircraftChasePose(target) : mod.aircraftApproachPose(target));
      const flight = mod.flyPhotoJourneyCamera(view, focus, {
        duration: settings.low ? 1200 : 1800,
      });
      Promise.resolve(flight).then(ok=>{
        if(disposed || !activeAlert || activeAlert !== next ||
           activeAlert.token !== next.token || isSleeping()) return;
        if(ok !== false){
          applyAlertCamera(next); // MEMORY LINK becomes reconstructed aircraft POV here
          next.stage = next.kind === 'match' ? 'pov' : 'chase';
        }
        startAlertInterpolation(next, next.token);
      }).catch(err=>{
        if(activeAlert === next) handleNavaraError(err);
      });
    }catch(e){ handleNavaraError(e); }
  }

  function handleAircraftUpdate(event){
    const detail = event && event.detail || {};
    const feedAvailable = detail.feedAvailable !== false && detail.ok !== false;
    /* events.js deliberately sends the last good snapshot on a transport
       failure.  Keep both that snapshot and its success clock untouched so
       a frozen seen_pos can age out in the renderer. */
    if(feedAvailable){
      latestAircraft = Array.isArray(detail.aircraft) ? detail.aircraft.slice() : [];
      lastSuccessfulFeedWallTime = Date.now()/1000;
      const feedNow = finite(detail.now);
      latestFeedNow = feedNow !== null ? feedNow : lastSuccessfulFeedWallTime;
    }
    let disappeared = null;
    if(activeAlert){
      const current = findAircraft(activeAlert.aircraft);
      if(current) activeAlert.aircraft=current;
      if(feedAvailable && !current) disappeared = activeAlert.until;
      else activeAlert.missing=!current;
    }
    if(disappeared) finishAlert(disappeared, 'target-disappeared');
    else updateAircraftLayer();
  }

  function handleAircraftAlert(event){
    const detail = event && event.detail || {};
    if(!detail.aircraft || !validAircraft(detail.aircraft)) return;
    latestAlert={
      aircraft: detail.aircraft,
      kind: detail.kind || 'normal',
      until: finite(detail.until) || Date.now()+20000,
    };
    if(view && !isSleeping()) startAlert(latestAlert);
  }

  function handleAircraftAlertEnd(event){
    const until = finite(event && event.detail && event.detail.until);
    // Ignore an old timeout arriving after a replacement alert.
    if(activeAlert && until && activeAlert.until !== until) return;
    finishAlert(until || (activeAlert && activeAlert.until));
  }

  function handlePresence(event){
    const next = !!(event && event.detail && event.detail.sleeping);
    if(next === sleeping) return;
    sleeping=next;
    if(next){
      cancelJourney();
      clearRefinement();
      clearAlertAnimation();
      clearPrefetchWork();
      if(activeAlert) activeAlert.suspended=true;
      return;
    }
    if(activeAlert && activeAlert.until > Date.now()) startAlert(activeAlert, true);
    else scheduleJourney(150);
  }

  listen(AIRCRAFT_UPDATE_EVENT, handleAircraftUpdate);
  listen(AIRCRAFT_ALERT_EVENT, handleAircraftAlert);
  listen(AIRCRAFT_ALERT_END_EVENT, handleAircraftAlertEnd);
  listen('geoframe:presence-state', handlePresence);
  listen('pagehide', disposeController);

  function tick(targetView, targetMod, i, targetLayer, initial=false){
    if(disposed || isPaused()){
      scheduleJourney(250);
      return;
    }
    if(i < 0){ scheduleJourney(1000); return; }
    const token=journeyGeneration;
    markShown(i);
    const p=PHOTOS[i];
    const nextIndex=nextQueueIdx();
    schedulePhotoPrefetch(targetMod,p,nextIndex >= 0 ? PHOTOS[nextIndex] : null);
    showPhoto(i);
    setMode('3D PHOTO JOURNEY LITE');
    const height=settings.height+(settings.low?0:(i%3)*400);
    const heading=settings.headingStep ? (i*settings.headingStep)%360 : 0;
    if(targetLayer && targetMod.updatePhotoJourneyBasemap){
      try{
        targetMod.updatePhotoJourneyBasemap(targetLayer.basemap,p);
        requestLodRefinement(targetView);
      }catch(e){ handleNavaraError(e); return; }
    }
    if(initial){ scheduleJourney(settings.dwellMs); return; }
    let flight;
    try{
      flight=targetMod.flyPhotoJourneyCamera(targetView,
        {lng:p.lon,lat:p.lat,height,pitch:settings.pitch,heading},
        {duration:settings.flyMs});
    }catch(e){ handleNavaraError(e); return; }
    Promise.resolve(flight)
      .then(()=>{ if(!disposed && !activeAlert && !isSleeping() && !isEventActive()) requestLodRefinement(targetView); })
      .catch(err=>{ if(!activeAlert && !disposed) console.warn('[3d] photo fly interrupted',err); })
      .finally(()=>{
        if(token===journeyGeneration && !disposed && !activeAlert && !isSleeping())
          scheduleJourney(settings.dwellMs);
      });
  }

  async function boot(){
    try{ await manifestReady; }catch(e){ /* file://直開き等はモックで継続 */ }
    if(disposed) return;
    reindexPhotos();
    loadPlayState();
    syncSchedule();
    if(!playQueue.length) buildPlayOrder();
    buildTicker();
    startLivePanels();
    setMode('3D PHOTO JOURNEY LITE');

    try{ mod=await import('/static/navara/photo-journey-lite.js'); }
    catch(e){ return handleNavaraError(e); }
    const container=document.getElementById('modelayer');
    if(!container) return handleNavaraError(new Error('#modelayer が見つからない'));
    const first=nextQueueIdx();
    try{
      view=mod.createPhotoJourneyView(container,{low:settings.low});
      await view.init();
      // 地形(GSI DEM)を抑制したDEM由来シェーディングで照らす最初の試行値。
      // sunはapplyColor:trueで大気散乱の計算を経ずに指定色をそのまま使い、
      // castShadow:falseでN100向けにシャドウマップを一切生成しない。
      view.addLight({ambient:{intensity:0.78}});
      view.addLight({sun:{intensity:0.28,applyColor:true,castShadow:false}});
      const points=toInitialPoint(first);
      if(first>=0){
        const p=PHOTOS[first];
        mod.setPhotoJourneyCamera(view,{lng:p.lon,lat:p.lat,height:settings.height,
          pitch:settings.pitch,heading:settings.headingStep?(first*settings.headingStep)%360:0});
      }
      layerState=mod.addPhotoJourneyLayers(view,{points,route:[]});
      aircraftLayerState=mod.addPhotoJourneyAircraftLayer(view, {
        onFeatureDiagnostics: updateAircraftVisibility,
      });
      updateAircraftLayer();
      requestLodRefinement(view);
    }catch(e){ return handleNavaraError(e); }

    const pending=latestAlert && latestAlert.until > Date.now() ? latestAlert : null;
    if(pending) startAlert(pending);
    else if(!isPaused()){
      if(first>=0) tick(view,mod,first,layerState,true);
      else scheduleJourney(1000);
    }
  }

  boot();
})();
