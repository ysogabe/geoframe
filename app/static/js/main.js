/* ============================================================
   main.js — 起動シーケンス（起動配線 / start() / 各種 setInterval / ウォッチドッグ）
   ※読み込み順が契約: 必ず最後。ここから上の全ファイルのグローバルを参照する
   宣言する主なグローバル: startOnce, start, MANIFEST_FAIL_MS, lastManifestOk,
                           bootAt, dailyReloadDone, startWatchdogs
   依存する他ファイルのグローバル: ほぼ全ファイル
     （DWELL_MS/config.js, manifestReady/data.js, map・tryAddLayers・mockPolys・
       ptsData・armBase/map.js, showPhoto・syncMarker/photocard.js,
       buildTicker・loadStats・showStats・showWeekly・startLivePanels/panels.js,
       nextQueueIdx・markShown・goTo・INTERLUDE_KINDS・showInterlude/scheduler.js）
   ※先頭の startOnce / map.on('load') / map.on('styledata') / setTimeout(4500) は
     元 index.html では map.js 相当の位置（519〜523行）にあったが、start() を参照する
     ため main.js へ移した。map.js に残すと 'load' が main.js 実行前に発火した場合に
     started=true のまま ReferenceError となり永久に起動しなくなる
   ============================================================ */
function startOnce(){ if(started) return; started=true;
  manifestReady.then(start, start); }
map.on('load', ()=>{ tryAddLayers(); startOnce(); });
map.on('styledata', tryAddLayers);
setTimeout(()=>{ tryAddLayers(); startOnce(); }, 4500);
function start(){
  // 3D PHOTO JOURNEY Lite（?mode=3d）では MapLibre の巡回・イベント・パネルを
  // 起動しない。初期化と巡回は static/js/photo-journey-lite.js（3dコントローラ）が行う。
  if(window.GEOFRAME_MODE_3D) return;
  // manifest反映後のデータでレイヤ・カメラ・ティッカーを整える
  if(!usingMock && layersAdded){
    try{ map.getSource('mock').setData(mockPolys()); map.getSource('pts').setData(ptsData()); }catch(e){}
  }
  // 再生状態を復元 → manifest差分を同期 → キュー先頭から再開（showPhoto(0)固定をやめる）
  reindexPhotos();
  loadPlayState();
  syncSchedule();
  if(!playQueue.length) buildPlayOrder();
  const first = nextQueueIdx();
  idx = first >= 0 ? first : 0;
  const p0 = PHOTOS[idx];
  activeLngLat=[p0.lon, p0.lat];
  armBase(p0.lon, p0.lat, map.getZoom());
  map.jumpTo({center:activeLngLat});
  buildTicker(); loadStats(); loadOnThisDay();
  markShown(idx);
  showPhoto(idx); syncMarker();
  let tick=0, interludeTurn=0;
  setInterval(()=>{ if(sleeping) return;                // 不在スリープ中は巡回停止
    if(Date.now()<eventUntil) return;                   // イベント演出中は巡回を一時停止
    tick++;
    if(tick % STATS_EVERY === 0){        // 定期的にインタールードを挟む（キューは消費しない）
      // 種別は INTERLUDE_KINDS を一巡（STATS → WEEKLY → OTD）。
      // ★添字を取ってから加算する（初回のインタールードは従来どおり統計カット）
      const kind = INTERLUDE_KINDS[interludeTurn % INTERLUDE_KINDS.length];
      interludeTurn++;
      if(showInterlude(kind)){
        eventUntil = Date.now() + DWELL_MS - 500;
        return;
      }
    }
    const nxt = nextQueueIdx();                         // 覗くだけ（ここでは消費しない）
    if(nxt < 0) return;
    const curT=PHOTOS[idx]&&PHOTOS[idx].trip, nxtT=PHOTOS[nxt]&&PHOTOS[nxt].trip;
    if(nxtT!==undefined && nxtT!==curT && lastIntroTrip!==nxtT && showTripIntro(nxtT)){
      lastIntroTrip=nxtT;                               // 旅の切り替わりにイントロを挟む
      eventUntil = Date.now() + DWELL_MS - 500;
      return;
    }
    idx=nxt; markShown(idx); goTo(idx); }, DWELL_MS);   // 表示するのでここで消費
  startLivePanels();
  // 稼働中に追加された写真を拾う（5分毎にmanifest再取得。Drive同期運用向け）
  // 併せてサーバー疎通のウォッチドッグも兼ねる（無人キオスク運用のため自力復帰させる）
  setInterval(async()=>{
    let ok=false;
    try{
      const r = await fetch('/api/photos');
      if(r.ok){
        ok=true;
        if(applyManifest(await r.json())){
          if(layersAdded){ try{ map.getSource('mock').setData(mockPolys());
            map.getSource('pts').setData(ptsData()); }catch(e){} }
          // 通常の写真差分では位置を保つ。品質ロールが変わったときだけ待機キューを再評価する。
          buildTicker(); syncSchedule();
        }
        loadStats();
        if(new Date().toDateString() !== otdDay) loadOnThisDay();   // 日付が変わったら取り直す
      }
    }catch(e){}
    if(ok){ lastManifestOk = Date.now(); }
    else if(Date.now() - lastManifestOk > MANIFEST_FAIL_MS){
      console.warn('[watchdog] /api/photos が10分以上失敗 → リロード');
      location.reload();
    }
  }, 300000);
  startWatchdogs();
}

/* ============================================================
   ウォッチドッグ（無人・長期稼働の自力復帰）
   - manifest取得が10分以上失敗 → リロード（上の5分間隔インターバル内で判定）
   - WebGLコンテキスト消失 → リロード（地図が白飛びしたまま固まるのを防ぐ）
   - 深夜4時台に1日1回リロード（Chromium長期稼働のメモリ肥大対策）
   ============================================================ */
const MANIFEST_FAIL_MS = 10*60e3;
let lastManifestOk = Date.now();
const bootAt = Date.now();
let dailyReloadDone = false;
function startWatchdogs(){
  lastManifestOk = Date.now();
  try{
    map.getCanvas().addEventListener('webglcontextlost', ()=>{
      console.warn('[watchdog] WebGLコンテキスト消失 → リロード');
      location.reload();
    });
  }catch(e){}
  setInterval(()=>{
    if(new Date().getHours() !== 4){ dailyReloadDone = false; return; }
    if(dailyReloadDone) return;
    // リロード直後も4時台なら再発火してループする → 稼働1時間未満は見送る
    if(Date.now() - bootAt < 3600e3) return;
    dailyReloadDone = true;
    console.info('[watchdog] 深夜の定期リロード');
    location.reload();
  }, 300000);
}
