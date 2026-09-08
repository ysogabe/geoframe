/* 3D巡回の負荷設定。GPUなしの回帰テストからも実値を検証できるようにする。 */
(function(){
  const baseDwell = Number.isFinite(DWELL_MS) && DWELL_MS > 0 ? DWELL_MS : 16000;

  const requestedStyle = typeof location !== 'undefined'
    ? new URLSearchParams(location.search).get('terrainStyle') : null;
  const terrainStyle = ['hybrid', 'cyber', 'photo'].includes(requestedStyle)
    ? requestedStyle : (LOW_POWER ? 'cyber' : 'hybrid');

  window.GEOFRAME_PHOTO_JOURNEY_SETTINGS = Object.freeze({
    low: !!LOW_POWER,
    terrainStyle,
    imageryFadeMs: 1800,
    imageryMinDwellMs: 55000,
    // 冷えた地形が細かくなった後にも鑑賞時間を確保する。
    dwellMs: Math.max(baseDwell, 90000),
    flyMs: LOW_POWER ? 1200 : 5000,
    // heightは撮影地点の地面からの距離。カメラ位置へ変換するときに標高を加える。
    height: LOW_POWER ? 3200 : 1400,
    pitch: LOW_POWER ? -28 : -24,
    headingStep: LOW_POWER ? 0 : 47,
    // タイルが揃ってから短い周回で視差を見せ、その後は静止する。
    orbitDelayMs: 45000,
    orbitMs: LOW_POWER ? 0 : 8000,
    orbitDegrees: 22,
    presencePollMs: LOW_POWER ? 30000 : 15000,
  });
})();
