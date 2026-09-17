/* ============================================================
   scheduler.js — 再生スケジューラの表示側（ON THIS DAY / インタールード / カメラ遷移 goTo）
   ※読み込み順が契約: scheduler-core.js（純ロジック）と panels.js の後、main.js より前
   宣言する主なグローバル: ONTHISDAY, otdPos, otdDay, loadOnThisDay, showOnThisDay,
       INTERLUDE_KINDS, showInterlude, idx, preloadNext, goTo
   依存する他ファイルのグローバル: LOW_POWER/FLY_MS (config.js),
       PHOTOS/usingMock/idById (data.js), playQueue (scheduler-core.js),
       map/armBase/setPrefVisible (map.js),
       markerEl/clearScatter/showPhoto/activeLngLat (photocard.js),
       clearHazard/eventUntil (events.js),
       eventCard/showStats/showWeekly (panels.js), setMode (modes.js)

   ※純ロジック（TRIP_MAX / PRI_* / buildPlayOrder / nextQueueIdx / syncSchedule 等）は
     scheduler-core.js へ分離した。DOM と map に触れない core 側だけを Node/vm で検証する。
   ※#mode への書き込みは modes.js の setMode() 経由（本ファイルは writer を持たない）。
   ============================================================ */

/* ==== ON THIS DAY（◯年前の今日） ==== */
let ONTHISDAY = [], otdPos = 0, otdDay = '';
async function loadOnThisDay(){
  if(usingMock) return;
  otdDay = new Date().toDateString();
  try{
    const r = await fetch('/api/onthisday'); if(!r.ok) return;
    const d = await r.json();
    ONTHISDAY = (d && d.ok && Array.isArray(d.items)) ? d.items : [];
    otdPos = 0;
  }catch(e){ ONTHISDAY = []; }
}
function showOnThisDay(){
  if(!ONTHISDAY.length) return false;
  const it = ONTHISDAY[otdPos % ONTHISDAY.length]; otdPos++;
  clearScatter(); clearHazard();
  document.getElementById('statspanel').classList.add('hidden');
  setPrefVisible(false);     // 制覇マップは統計カットの間だけ出す
  markerEl.style.display='none';
  setMode('ON THIS DAY');
  armBase(it.lon, it.lat, 11.5);
  map.flyTo({center:[it.lon,it.lat], zoom:11.5, pitch:0, bearing:0,
             duration:LOW_POWER?2500:4500, essential:true});
  activeLngLat=[it.lon,it.lat];
  const loc=[it.pref,it.city,it.place].filter(Boolean).join(' ');
  eventCard(`▸ ${it.years_ago}年前の今日`,
    `<img src="${it.file}" onerror="this.remove()">`, it.title,
    `${it.years_ago}年前の今日 — ${it.taken_at||''}\n${loc?loc+'\n':''}`+
    `LAT ${it.lat.toFixed(4)} N / LON ${it.lon.toFixed(4)} E`);
  return true;
}

/* ==== インタールードのローテーション ====
   写真 STATS_EVERY カットごとに1カット挟む差し込み画面の順番。
   配列を一巡させるので「◯年前の今日」の出番は従来どおり3回に1回のまま、
   間に週間天気予報が入る（STATS → WEEKLY → OTD → …）。
   選んだ種別にデータが無い（各 show* が false）ときは残りを順に試すので、
   該当写真が無い日や気象庁が落ちている間も統計カットが出続ける */
const INTERLUDE_KINDS = ['STATS', 'WEEKLY', 'OTD'];
function showInterlude(kind){
  const fn = {STATS: showStats, WEEKLY: showWeekly, OTD: showOnThisDay};
  const order = [kind].concat(INTERLUDE_KINDS.filter(k => k !== kind));
  for(const k of order){ if(fn[k] && fn[k]()) return true; }
  return false;
}

let idx=0;
/* 次に出る写真を先読みしておく（キューは覗くだけで消費しない）。
   goTo時点で markShown が済んでいるので playQueue[0] が次の写真 */
function preloadNext(){
  if(!playQueue.length) return;
  const j = idById[playQueue[0]];
  const q = (j !== undefined) ? PHOTOS[j] : null;
  if(q && q.img) new Image().src = q.img;
}
function goTo(i){
  const p=PHOTOS[i];
  preloadNext();
  activeLngLat=[p.lon,p.lat];
  markerEl.style.display='';
  clearScatter();
  clearHazard();   // 地震イベントで表示した防災レイヤを片付ける
  document.getElementById('statspanel').classList.add('hidden');
  setPrefVisible(false);     // 制覇マップは統計カットの間だけ出す
  document.getElementById('photocard').classList.add('hidden');
  setMode('TRANSIT ▸ ' + p.title);
  const gz = 14.2+(i%2?0.6:0);
  armBase(p.lon, p.lat, gz);
  map.flyTo({center:[p.lon,p.lat], zoom:gz,
             pitch:LOW_POWER?0:45+(i%3)*6, bearing:LOW_POWER?0:-30+(i*23)%60,
             duration:FLY_MS, curve:LOW_POWER?1.2:1.6, essential:true});
  setTimeout(()=>{
    if(typeof modeClaimIsTop === 'function' && !modeClaimIsTop('ambient')) return;
    setMode('PHOTO JOURNEY');
    showPhoto(i);
  }, FLY_MS*0.82);
}
