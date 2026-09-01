/* ============================================================
   bootstrap.js — 起動分岐（?mode=3d の検出と一度きりのフォールバック）
   ※読み込み順が契約: 必ず最初（他の全ファイルが GEOFRAME_MODE_3D を参照する）
   宣言する主なグローバル: GEOFRAME_MODE_3D
   依存する他ファイルのグローバル: なし
   ============================================================ */

/* 通常URL（?mode=3d 無し）では既存の MapLibre 表示を維持し、Navara の
   JS/WASM/worker を一切読み込まない。?mode=3d のときだけ Navara を遅延ロードする。 */
window.GEOFRAME_MODE_3D = false;

(function () {
  let params;
  try { params = new URLSearchParams(location.search); }
  catch (e) { return; }
  if (params.get('mode') !== '3d') return;

  /* Navara/WASM/worker/WebGL の初期化に失敗したら通常モードへ戻すための
     セッション内フラグ。すでに失敗済みなら mode=3d を剥がして通常モードで
     リロードする（一度きり。リロードループは生じない）。 */
  let failed = false;
  try { failed = sessionStorage.getItem('geoframe.3d.fallback') === '1'; } catch (e) {}

  if (failed) {
    params.delete('mode');
    const qs = params.toString();
    const url = location.pathname + (qs ? '?' + qs : '') + location.hash;
    try { sessionStorage.removeItem('geoframe.3d.fallback'); } catch (e) {}
    location.replace(url);
    return;
  }

  window.GEOFRAME_MODE_3D = true;
  if (document.body) document.body.classList.add('mode3d');
})();
