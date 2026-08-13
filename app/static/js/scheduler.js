/* ============================================================
   scheduler.js — 再生スケジューラ（写真IDベースの再生キュー）/ ON THIS DAY /
                  カメラ遷移 goTo
   ※読み込み順が契約: panels.js の後、main.js より前
   宣言する主なグローバル: TRIP_MAX, PRI_NEVER, PRI_NEW_MAX, PRI_ANNIV_MAX, PRI_FAV,
       PRI_QUALITY_RECOMMENDED, PRI_QUALITY_ALTERNATE,
       PRI_JITTER, PLAY_STATE_KEY, NEW_HALFLIFE_D, ANNIV_WINDOW_D, NEW_INSERT_WINDOW,
       playQueue, shown, roundNo, savedSig, knownIds, annivWeight, photoPriority,
       buildPlayOrder, nextQueueIdx, markShown, photoSig, qualitySig, savePlayState, loadPlayState,
       syncSchedule, ONTHISDAY, otdPos, otdDay, loadOnThisDay, showOnThisDay,
       INTERLUDE_KINDS, showInterlude, idx, preloadNext, goTo
   依存する他ファイルのグローバル: LOW_POWER/FLY_MS (config.js),
       PHOTOS/usingMock/idById (data.js), map/armBase/setPrefVisible (map.js),
       markerEl/clearScatter/showPhoto/activeLngLat (photocard.js),
       clearHazard/eventUntil (events.js),
       eventCard/showStats/showWeekly (panels.js)
   ============================================================ */

/* ============================================================
   再生スケジューラ（写真IDベースの再生キュー）
   全網羅 + シャッフル + 新着優先 + 記念日加味 + localStorage永続化。
   1ラウンド = 各トリップから最大TRIP_MAX枚を選出した1周分（≒51分）。
   キューは「写真ID」で持つ。manifest再取得でPHOTOSの添字が動いてもIDは動かないので、
   再生位置が巻き戻らない。
   ============================================================ */
const TRIP_MAX = 6;          // 1トリップから1ラウンドに出す最大枚数
const PRI_NEVER = 45;        // 一度も表示していない写真の基礎スコア
const PRI_NEW_MAX = 12;      // 新着ブースト最大（fetched_atから半減期4日で指数減衰）
const PRI_ANNIV_MAX = 3;     // 記念日ブースト最大（撮影月日と今日の環状距離、窓14日で線形減衰）
const PRI_FAV = 2;           // お気に入りブースト
const PRI_QUALITY_RECOMMENDED = 6; // 類似群の解析推奨（お気に入りより強く、新着より弱い）
const PRI_QUALITY_ALTERNATE = -8;  // 類似群の非推奨。有限減点なのでstalenessで必ず追い越す
const PRI_JITTER = 4;        // ランダムゆらぎ
const PLAY_STATE_KEY = 'geoframe.play.v1';
const NEW_HALFLIFE_D = 4;    // 新着ブーストの半減期（日）
const ANNIV_WINDOW_D = 14;   // 記念日ブーストの窓（日）
const NEW_INSERT_WINDOW = 12;// 新着をキュー先頭から何位置以内に差し込むか

let playQueue = [];   // 再生待ちの写真IDの配列（先頭が次に出る写真）
let shown = {};       // 写真ID → 最後に表示したラウンド番号
let roundNo = 0;
let savedSig = '';    // 最後に同期したPHOTOSのシグネチャ
let savedQualitySig = ''; // 品質ロールだけのシグネチャ。変化時は現在キューを再評価する
let knownIds = null;  // 前回同期時点で存在したID集合（メモリのみ。null=未初期化）

/* 撮影月日と今日の環状距離を 0〜1 の重みに（近いほど1）。年跨ぎ考慮（12/30と1/2は距離3日） */
function annivWeight(timeStr){
  if(!timeStr || timeStr.length < 10) return 0;
  const m = +timeStr.slice(5,7), d = +timeStr.slice(8,10);
  if(!m || !d) return 0;
  const doy = (mm, dd) => Math.round((Date.UTC(2001, mm-1, dd) - Date.UTC(2001,0,1)) / 86400000);
  const now = new Date();
  let diff = Math.abs(doy(m, d) - doy(now.getMonth()+1, now.getDate()));
  if(diff > 182) diff = 365 - diff;                 // 環状（年跨ぎ）
  return diff >= ANNIV_WINDOW_D ? 0 : (1 - diff / ANNIV_WINDOW_D);
}

/* 優先度スコア。
   ★飢餓なし（全網羅）保証:
     ブーストの合計は有界で、最大
     PRI_NEW_MAX+PRI_ANNIV_MAX+PRI_FAV+PRI_QUALITY_RECOMMENDED+PRI_JITTER = 27pt にしかならない。
     一方 staleness（最終表示からの経過ラウンド数）は上限なく増え続ける。
     したがって長く出ていない写真は必ずいつか全ブースト付きの写真を追い越し、選出される。
     未表示写真は PRI_NEVER=45 から始まるので、既表示写真がこれを超えるには staleness>72 が必要
     ＝未表示写真は常に最優先クラスにいる。
     実際の全網羅周期を決めるのは主に TRIP_MAX の方で、N枚のトリップは1ラウンドに TRIP_MAX 枚出るため
     ceil(N/TRIP_MAX) ラウンドで必ず一巡する（最大トリップの枚数が律速）。 */
function photoPriority(p){
  const last = shown[p.id];
  let s = (last === undefined) ? PRI_NEVER : (roundNo - last);
  if(p.fetchedAt){                                   // 新着ブースト（指数減衰）
    const t = Date.parse(p.fetchedAt);
    if(!isNaN(t)){
      const days = (Date.now() - t) / 86400000;
      if(days >= 0) s += PRI_NEW_MAX * Math.pow(0.5, days / NEW_HALFLIFE_D);
    }
  }
  s += PRI_ANNIV_MAX * annivWeight(p.time);          // 記念日ブースト
  if(p.fav) s += PRI_FAV;                            // お気に入りブースト
  // 完全重複の非代表はserver側でmanifestから除外済み。ここでは類似群だけ順位へ反映する。
  if(p.qualityKind === 'similar'){
    s += p.qualityRecommended ? PRI_QUALITY_RECOMMENDED : PRI_QUALITY_ALTERNATE;
  }
  return s + Math.random() * PRI_JITTER;             // ランダムゆらぎ
}

/* 1ラウンド分をキュー末尾に積む。
   トリップ順はシャッフルするが、トリップ内は撮影時刻順（=PHOTOS配列順）を保ち旅のストーリー性を維持。 */
function buildPlayOrder(){
  if(!PHOTOS.length) return 0;
  roundNo++;
  // トリップ単位にグループ化（PHOTOSは時系列順なので連続runで検出できる）
  const groups=[]; let cur=null;
  PHOTOS.forEach((p,i)=>{
    const t = p.trip===undefined ? 0 : p.trip;
    if(!cur || cur.t!==t){ cur={t, idxs:[]}; groups.push(cur); }
    cur.idxs.push(i);
  });
  // トリップ順をFisher-Yatesでシャッフル（毎ラウンド違う旅順で流れる）
  for(let i=groups.length-1;i>0;i--){
    const j=Math.floor(Math.random()*(i+1));
    const tmp=groups[i]; groups[i]=groups[j]; groups[j]=tmp;
  }
  const out=[];
  for(const g of groups){
    const take = Math.min(TRIP_MAX, g.idxs.length);
    // スコアは先に確定させる（比較のたびにMath.random()を呼ぶと比較関数が非一貫になり順序が壊れる）
    const scored = g.idxs.map(i => ({i:i, s:photoPriority(PHOTOS[i])}));
    scored.sort((a,b)=> b.s - a.s);
    const pick = scored.slice(0, take).map(o=>o.i).sort((a,b)=> a-b);   // 窓内は時刻順へ戻す
    for(const i of pick) out.push(PHOTOS[i].id);
  }
  playQueue = playQueue.concat(out);
  savePlayState();
  return out.length;
}

/* 次に出す写真を「覗く」。キューは消費しない（統計/旅イントロ/イベント割込みで捨てられるため）。
   解決できないIDは捨てて次へ。setInterval内から呼ばれるので絶対に無限ループしない。 */
function nextQueueIdx(){
  if(!PHOTOS.length) return -1;
  for(let guard=0; guard<64; guard++){
    if(!playQueue.length && !buildPlayOrder()) return -1;
    const i = idById[playQueue[0]];
    if(i !== undefined) return i;
    playQueue.shift();          // manifestから消えたID → 捨てて次へ
  }
  playQueue = [];               // 異常時のフォールバック
  return buildPlayOrder() ? (idById[playQueue[0]] !== undefined ? idById[playQueue[0]] : 0) : 0;
}

/* 実際に表示したときだけ呼ぶ: キューを1つ消費し、表示ラウンドを記録して永続化 */
function markShown(i){
  const p = PHOTOS[i];
  if(!p) return;
  if(playQueue.length && playQueue[0] === p.id) playQueue.shift();
  shown[p.id] = roundNo;
  savePlayState();
}

/* ==== 再生状態の永続化 ====
   ★運用注意:
     - Chromium は --incognito や使い捨てプロファイルで起動しないこと（永続 --user-data-dir が必須）。
       localStorage が毎回消えると再生位置と表示履歴がリセットされ、全網羅が最初からやり直しになる。
     - GEOFRAME_PORT を変えるとオリジン（localhost:ポート）が変わるため別ストレージ扱いになり、
       同じ端末でも再生状態がリセットされる。
     - file:// のモックモードでは保存も復元も行わない。 */
function photoSig(){
  let h = 0;
  for(const p of PHOTOS){
    const s = String(p.id);
    for(let k=0;k<s.length;k++) h = (h*31 + s.charCodeAt(k)) | 0;
  }
  return PHOTOS.length + ':' + (h>>>0).toString(36);
}
function qualitySig(){
  let h = 0, n = 0;
  for(const p of PHOTOS){
    if(!p.qualityRole) continue;
    n++;
    const s = String(p.id) + ':' + p.qualityRole;
    for(let k=0;k<s.length;k++) h = (h*31 + s.charCodeAt(k)) | 0;
  }
  return n + ':' + (h>>>0).toString(36);
}
function savePlayState(){
  if(usingMock) return;
  try{
    localStorage.setItem(PLAY_STATE_KEY, JSON.stringify({
      v:1, sig:photoSig(), qualitySig:qualitySig(), queue:playQueue,
      round:roundNo, shown:shown, savedAt:Date.now()
    }));
  }catch(e){}
}
function loadPlayState(){
  if(usingMock) return false;
  let st = null;
  try{ st = JSON.parse(localStorage.getItem(PLAY_STATE_KEY) || 'null'); }catch(e){ st = null; }
  if(!st || st.v !== 1) return false;                 // 壊れている/版違い → 破棄して作り直す
  roundNo = st.round || 0;
  shown = {};
  const sh = st.shown || {};
  for(const k in sh) if(idById[k] !== undefined) shown[k] = sh[k];   // 現manifestに無いIDは掃除
  playQueue = (st.queue || []).filter(id => idById[id] !== undefined);
  savedSig = st.sig || '';
  savedQualitySig = st.qualitySig || '';
  return true;
}

/* manifest更新時の差分同期。
   ★巻き戻しバグの根治点: シグネチャが変わっていなければ「何もしない」。
     写真集合だけが変わったときは消えたIDの除去と新規IDの差し込みだけを行う。
     品質ロール変更時だけは、shown履歴を残して待機キューを再評価する。 */
function syncSchedule(){
  const sig = photoSig();
  const qsig = qualitySig();
  if(knownIds === null){
    // 起動直後の初回同期。現manifestを既知集合とする。
    // 停止中に増えていた写真は shown に無いので PRI_NEVER で次ラウンドに自然と上位選出される。
    knownIds = new Set(PHOTOS.map(p=>p.id));
    savedSig = sig;
    // 解析の初回適用・再解析後は、shown履歴を保ったまま待機キューだけ組み直す。
    // これが無いと永続queueが尽きるまで（約1ラウンド）品質順位が反映されない。
    if(qsig !== savedQualitySig){
      savedQualitySig = qsig;
      playQueue = [];
      buildPlayOrder();
      return true;
    }
    savedQualitySig = qsig;
    return false;
  }
  if(qsig !== savedQualitySig){
    playQueue = [];
    knownIds = new Set(PHOTOS.map(p=>p.id));
    savedSig = sig;
    savedQualitySig = qsig;
    buildPlayOrder();
    return true;
  }
  if(sig === savedSig) return false;         // 変化なし → キューを温存（作り直さない）
  playQueue = playQueue.filter(id => idById[id] !== undefined);      // 消えたIDを除去
  for(const k in shown) if(idById[k] === undefined) delete shown[k];
  // 旧IDと1枚も重ならない = データセットごと入れ替わった場合（起動時にserver.py未応答で
  // モックで始まり、5分後のリフレッシュで実写真に切り替わる経路など）。
  // 差し込み処理は「少数の新着」前提なので、ここで全件を先頭に詰め込まず正規手順で作り直す。
  let overlap = false;
  for(const p of PHOTOS){ if(knownIds.has(p.id)){ overlap = true; break; } }
  if(!overlap){
    playQueue = [];
    knownIds = new Set(PHOTOS.map(p=>p.id));
    savedSig = sig;
    savedQualitySig = qsig;
    buildPlayOrder();                        // 1ラウンド分をトリップ構成込みで生成
    return true;
  }
  // 新規IDはキュー先頭から NEW_INSERT_WINDOW 位置以内のランダム位置へ → 数分以内に画面に出る。
  // 一度に大量追加された場合に全件を先頭へ詰め込むと旅の構成が壊れるので差し込みは
  // NEW_INSERT_WINDOW 件までとし、残りは PRI_NEVER により次ラウンドで最優先選出させる。
  const fresh = PHOTOS.filter(p => !knownIds.has(p.id)).map(p=>p.id).slice(0, NEW_INSERT_WINDOW);
  for(const id of fresh){
    const pos = Math.floor(Math.random() * Math.min(NEW_INSERT_WINDOW, playQueue.length + 1));
    playQueue.splice(pos, 0, id);
  }
  knownIds = new Set(PHOTOS.map(p=>p.id));
  savedSig = sig;
  savedQualitySig = qsig;
  savePlayState();
  return true;
}

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
  document.getElementById('mode').textContent='MODE: ON THIS DAY';
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
  document.getElementById('mode').textContent='MODE: TRANSIT ▸ '+p.title;
  const gz = 14.2+(i%2?0.6:0);
  armBase(p.lon, p.lat, gz);
  map.flyTo({center:[p.lon,p.lat], zoom:gz,
             pitch:LOW_POWER?0:45+(i%3)*6, bearing:LOW_POWER?0:-30+(i*23)%60,
             duration:FLY_MS, curve:LOW_POWER?1.2:1.6, essential:true});
  setTimeout(()=>{ if(Date.now()<eventUntil) return;   // イベント演出中は写真カードを奪わない
    document.getElementById('mode').textContent='MODE: PHOTO JOURNEY';
    showPhoto(i); }, FLY_MS*0.82);
}
