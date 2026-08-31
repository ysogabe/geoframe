/* ============================================================
   photo-journey-lite.js — 3D PHOTO JOURNEY Lite の実行時コントローラ
   ※読み込み順が契約: scheduler.js の後、main.js より前
   （起動時は manifestReady を await するため、参照するグローバルは
     呼び出し時点で必ず定義済み。順序は緩いが自然順に置く）

   宣言する主なグローバル: なし（IIFE に閉じる。GEOFRAME_MODE_3D は bootstrap.js）
   依存する他ファイルのグローバル: GEOFRAME_MODE_3D (bootstrap.js),
       DWELL_MS (config.js), PHOTOS/usingMock/manifestReady/reindexPhotos (data.js),
       playQueue/loadPlayState/syncSchedule/buildPlayOrder/nextQueueIdx/markShown
       (scheduler-core.js), showPhoto (photocard.js), setMode (modes.js),
       buildTicker (panels.js)

   役割:
     - ?mode=3d のときだけ Navara ThreeView（app/static/navara/ の自己完結バンドル）を
       動的 import で遅延ロードする。通常URLではこの import 文に到達しないため
       Navara の JS/WASM/worker 資産は一切読み込まれない。
     - manifest（PHOTOS）→ 写真ポイント + ライトルート線へ変換し、約16秒周期の
       低頻度カメラ遷移を回す。写真カード・AREA INTEL・HUD は通常モードと同じ
       showPhoto / buildTicker / setMode を再利用する。
     - Navara / WASM / worker / WebGL の初期化に失敗したら sessionStorage に
       フラグを立てて一度だけ通常(MapLibre)モードへフォールバックする。
       （リロードは bootstrap.js が mode=3d を剥がして実行。ループしない）
   ============================================================ */
(function(){
  if(!window.GEOFRAME_MODE_3D) return;

  const DWELL_3D_MS = DWELL_MS || 16000;  // 1地点の滞在（低頻度遷移の基準）
  const FLY_3D_MS   = 5000;               // カメラ移動アニメーション時間

  /* Navara/WASM/worker/WebGL の初期化失敗時の一度きりのフォールバック。
     bootstrap.js は geoframe.3d.fallback==='1' を検知すると mode=3d を剥がして
     リロードし、その際にフラグを消すためリロードループは生じない。 */
  function fallbackToMapLibre(err){
    console.warn('[3d] Navara初期化に失敗 → 通常(MapLibre)モードへフォールバック', err);
    try{ sessionStorage.setItem('geoframe.3d.fallback', '1'); }catch(e){}
    location.reload();
  }

  /* manifest（PHOTOS）→ Navara レイヤ用の points / route（同一配列を順序のまま使う） */
  function toPoints(){
    const points = PHOTOS.map(p => ({ lon: p.lon, lat: p.lat, title: p.title || '' }));
    return { points, route: points };
  }

  async function boot(){
    // 通常モードと同じ初期化を流用（manifest反映 → 再生状態復元 → 差分同期 → キュー構築）
    try{ await manifestReady; }catch(e){ /* file://直開き等はモックで継続 */ }
    reindexPhotos();
    loadPlayState();
    syncSchedule();
    if(!playQueue.length) buildPlayOrder();
    buildTicker();   // HUDティッカーは通常モードと共通

    // Navara バンドルを遅延ロード（通常URLではここに到達しない）
    let mod;
    try{
      mod = await import('/static/navara/photo-journey-lite.js');
    }catch(e){ return fallbackToMapLibre(e); }

    const container = document.getElementById('modelayer');
    if(!container) return fallbackToMapLibre(new Error('#modelayer が見つからない'));

    let view;
    try{
      view = mod.createPhotoJourneyView(container);
      await view.init();               // WebGLコンテキスト生成（失敗時はthrow→フォールバック）
      const { points, route } = toPoints();
      mod.addPhotoJourneyLayers(view, { points, route });
    }catch(e){ return fallbackToMapLibre(e); }

    const first = nextQueueIdx();
    if(first >= 0) tick(view, mod, first);
    else setTimeout(() => tick(view, mod, nextQueueIdx()), 1000);
  }

  function tick(view, mod, i){
    if(i < 0){ setTimeout(() => tick(view, mod, nextQueueIdx()), 1000); return; }
    markShown(i);
    const p = PHOTOS[i];
    showPhoto(i);                 // 写真カード + AREA INTEL（通常モードと共通）
    setMode('PHOTO JOURNEY');
    // 撮影地点を中心に低高度・やや俯角で見る。heading は地点ごとに回して変化を付ける。
    const height  = 1600 + (i % 3) * 400;
    const heading = (i * 47) % 360;
    Promise.resolve(
      mod.flyPhotoJourneyCamera(
        view,
        { lng: p.lon, lat: p.lat, height, pitch: -32, heading },
        { duration: FLY_3D_MS }
      )
    ).catch(()=>{}).finally(()=>{
      setTimeout(() => tick(view, mod, nextQueueIdx()), DWELL_3D_MS);
    });
  }

  boot();
})();
