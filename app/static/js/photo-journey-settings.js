/* 3D巡回の負荷設定。GPUなしの回帰テストからも実値を検証できるようにする。 */
(function(){
  const baseDwell = Number.isFinite(DWELL_MS) && DWELL_MS > 0 ? DWELL_MS : 16000;

  window.GEOFRAME_PHOTO_JOURNEY_SETTINGS = Object.freeze({
    low: !!LOW_POWER,
    dwellMs: LOW_POWER ? Math.max(baseDwell, 45000) : Math.max(baseDwell, 30000),
    flyMs: LOW_POWER ? 1200 : 5000,
    // 真下寄りの俯瞰（-50°）ではDEMの起伏が写真の傾きに埋もれるため、
    // 地平線と斜面が読める斜め視点に固定する。heightは楕円体からの実高度。
    height: LOW_POWER ? 4200 : 2400,
    pitch: LOW_POWER ? -28 : -32,
    headingStep: LOW_POWER ? 0 : 47,
    presencePollMs: LOW_POWER ? 30000 : 15000,
  });
})();
