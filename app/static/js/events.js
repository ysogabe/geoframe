/* ============================================================
   events.js — ADS-B（航空機）/ 防災レイヤ / 気象庁（天気・地震）のイベント演出
   ※読み込み順が契約: photocard.js の後（'move' ハンドラの登録順を保つため）
   宣言する主なグローバル:
     [ADS-B] ADSB_POLL, EVENT_MS, aircraft, eventUntil, cooldown, acEls,
             COOLDOWN_MAX, pruneCooldown, syncAircraft, renderAirPanel,
             triggerEvent, pollAircraft
     [防災]  EQ_EVENT_MS, HZ_COLORS, hzActive, ensureHazardLayers, showHazard, clearHazard
     [気象庁] ENV, eqEls, seenEq, envFirst, lastEqEvent, SEEN_EQ_MAX,
              rememberEq, syncQuakes, loadEnv
   依存する他ファイルのグローバル: LOW_POWER (config.js), map/armBase (map.js),
       clearScatter/activeLngLat (photocard.js),
       eventCard/buildTicker (panels.js: 後続ファイルだが実行時にしか呼ばないので可)
   ※先頭にある「巡回シーケンス」の見出しコメントは、元 index.html で ADS-B ブロックの
     直前に置かれていたものなので、元の位置関係を優先してこのファイルに残した
   ============================================================ */

/* ==== 巡回シーケンス ==== */
/* ============================================================
   ADS-B（tar1090連携）: 航空機のリアルタイム表示とイベント演出
   - 通常: 緑の機影＋コールサイン/高度、AIR TRAFFICパネル
   - レア機体（軍用・特殊型式）: マゼンタ警報＋機体へフライト
   - 行き先マッチ: 行き先空港が自分の写真の場所 → 琥珀色でその写真を表示
   ============================================================ */
const ADSB_POLL = LOW_POWER ? 9000 : 5000;
const EVENT_MS = 20000;
let aircraft = [], eventUntil = 0;
const cooldown = {};   // hex/callsign → 次回演出可能時刻
const acEls = {};      // hex → DOM要素
/* 常時稼働で機体キーが溜まり続けないよう、期限切れを掃除しつつ上限を切る */
const COOLDOWN_MAX = 300;
function pruneCooldown(){
  let keys = Object.keys(cooldown);
  if(keys.length <= COOLDOWN_MAX) return;
  const now = Date.now();
  keys.forEach(k=>{ if(cooldown[k] < now) delete cooldown[k]; });
  keys = Object.keys(cooldown);
  if(keys.length > COOLDOWN_MAX)
    keys.sort((a,b)=>cooldown[a]-cooldown[b])
        .slice(0, keys.length-COOLDOWN_MAX).forEach(k=>delete cooldown[k]);
}

function syncAircraft(){
  // 3DではMapLibreの投影座標が存在しない。AIR TRAFFICカードは表示するが、
  // 機影DOMを(0,0)へ重ねて誤表示しないよう地図上マーカーだけを止める。
  if(window.GEOFRAME_MODE_3D) return;
  const layer = document.getElementById('aclayer');
  const seen = new Set();
  aircraft.forEach(a=>{
    seen.add(a.hex);
    let el = acEls[a.hex];
    if(!el){
      el = document.createElement('div'); el.className='ac';
      el.innerHTML = '<span class="tri">▲</span><div class="lbl"></div>';
      layer.appendChild(el); acEls[a.hex]=el;
    }
    el.classList.toggle('rare', a.rare||a.emergency);
    el.classList.toggle('match', !!a.match && !a.rare);
    el.querySelector('.tri').style.transform = `rotate(${(a.track||0) - map.getBearing()}deg)`;
    const alt = a.alt==null?'':(typeof a.alt==='number'? Math.round(a.alt/100)+'FL':'GND');
    el.querySelector('.lbl').textContent = `${a.callsign||a.hex} ${alt}`;
    const px = map.project([a.lon, a.lat]);
    el.style.left = px.x+'px'; el.style.top = px.y+'px';
    el.style.display = (px.x<-60||px.y<-60||px.x>innerWidth+60||px.y>innerHeight+60)?'none':'';
  });
  Object.keys(acEls).forEach(h=>{ if(!seen.has(h)){ acEls[h].remove(); delete acEls[h]; } });
}
map.on('move', syncAircraft);

function renderAirPanel(){
  const panel = document.getElementById('airpanel');
  if(!aircraft.length){ panel.style.display='none'; return; }
  panel.style.display='block';
  document.getElementById('ac-count').textContent = aircraft.length+' TRACKED';
  document.getElementById('ac-list').innerHTML = aircraft.slice(0,5).map(a=>{
    const rt = a.route ? `${a.route.from.iata||'—'}→${a.route.to.iata||'—'}` : '';
    const cls = (a.rare||a.emergency)?'rare':(a.match?'match':'');
    return `<div class="ac-row ${cls}"><span class="cs">${a.callsign||a.hex}</span>`+
      `<span>${a.type||''}</span><span class="rt">${rt}</span>`+
      `<span>${a.dist_km!=null?a.dist_km+'km':''}</span></div>`;
  }).join('');
}

function triggerEvent(a){
  const key = a.match ? 'm:'+(a.callsign||a.hex) : 'r:'+a.hex;
  const now = Date.now();
  if(now < (cooldown[key]||0)) return false;
  cooldown[key] = now + (a.match ? 2*3600e3 : 30*60e3);
  pruneCooldown();
  eventUntil = now + EVENT_MS;
  const bar = document.getElementById('alertbar');
  armBase(a.lon, a.lat, 9.5);
  map.flyTo({center:[a.lon,a.lat], zoom:9.5, pitch:0, bearing:0,
             duration:LOW_POWER?2500:4000, essential:true});
  activeLngLat=[a.lon,a.lat];
  if(a.emergency || a.rare){
    bar.className=''; bar.style.display='block';
    bar.textContent = a.emergency ? `⚠ EMERGENCY SQUAWK ${a.squawk}` : '⚠ RARE AIRCRAFT DETECTED';
    const img = a.photo_thumb
      ? `<img src="${a.photo_thumb}" onerror="this.remove()">`
      : `<div class="ph" style="background:linear-gradient(135deg,#3a0a22,#12060f)"><span class="label">NO IMAGE</span></div>`;
    eventCard('▸ RARE AIRCRAFT', img, `${a.callsign||a.hex} ${a.type?'['+a.type+']':''}`,
      `REG ${a.reg||'?'}  ALT ${a.alt??'?'}ft  GS ${a.gs??'?'}kt\n${a.owner||'OPERATOR UNKNOWN'}`);
    setMode('INTERCEPT');
  }else if(a.match){
    bar.className='match'; bar.style.display='block';
    bar.textContent = `MEMORY LINK: ${a.callsign} → ${a.route.to.iata} ${a.route.to.city}`;
    const m=a.match;
    eventCard('▸ MEMORY LINK', `<img src="${m.file}" onerror="this.remove()">`,
      m.title,
      `${a.callsign} は ${a.route.to.city} 行き\nあなたの写真: ${m.place||m.title}（${m.time||'—'}）  空港から${m.km}km`);
    setMode('MEMORY LINK');
  }
  setTimeout(()=>{ bar.style.display='none';
    setMode('PHOTO JOURNEY'); }, EVENT_MS);
  return true;
}

async function pollAircraft(){
  try{
    const r = await fetch('/api/aircraft'); if(!r.ok) throw 0;
    const d = await r.json(); if(!d.ok) throw 0;
    aircraft = d.aircraft;
  }catch(e){ aircraft = []; }
  syncAircraft(); renderAirPanel();
  if(Date.now() >= eventUntil){
    const cand = aircraft.find(a=>a.emergency) || aircraft.find(a=>a.rare) || aircraft.find(a=>a.match);
    if(cand) triggerEvent(cand);
  }
}

/* ==== 防災レイヤ（不動産情報ライブラリ / 地震イベント時に表示） ==== */
const EQ_EVENT_MS = 60000;   // 地震イベントは長め（防災レイヤをじっくり見せる）
const HZ_COLORS = {26:'#38bdf8', 28:'#a78bfa', 29:'#ff2d95', 22:'#ff7043', 21:'#ffb300', 25:'#d4e157'};
let hzActive = false;
function ensureHazardLayers(){
  Object.entries(HZ_COLORS).forEach(([code,c])=>{
    if(map.getSource('hz-'+code)) return;
    map.addSource('hz-'+code, {type:'geojson', data:{type:'FeatureCollection',features:[]}});
    map.addLayer({id:`hzf-${code}`, type:'fill', source:'hz-'+code,
      paint:{'fill-color':c,'fill-opacity':0.28}});
    map.addLayer({id:`hzl-${code}`, type:'line', source:'hz-'+code,
      paint:{'line-color':c,'line-opacity':0.85,'line-width':1.2}});
  });
}
async function showHazard(lat, lon){
  try{
    const r = await fetch(`/api/hazard?lat=${lat}&lon=${lon}`);
    if(!r.ok) return;
    const d = await r.json();
    if(!d.ok){ if(d.reason==='no_key') console.log('[hazard] APIキー未設定のためスキップ'); return; }
    ensureHazardLayers();
    hzActive = true;
    let rows='';
    for(const l of d.layers){
      const src = map.getSource('hz-'+l.code);
      if(src) src.setData(l.geojson);
      if(l.count>0)
        rows += `<div class="row"><span class="sw" style="background:${HZ_COLORS[l.code]}"></span>${l.name} <span class="n">${l.count}</span></div>`;
    }
    const lg = document.getElementById('hzlegend');
    lg.innerHTML = '<div class="h">▸ HAZARD LAYERS — 不動産情報ライブラリ</div>'+
      (rows || '<div class="row">周辺40km四方に該当区域なし</div>');
    lg.style.display='block';
  }catch(e){}
}
function clearHazard(){
  if(!hzActive) return;
  hzActive = false;
  Object.keys(HZ_COLORS).forEach(code=>{
    const src = map.getSource('hz-'+code);
    if(src) src.setData({type:'FeatureCollection',features:[]});
  });
  document.getElementById('hzlegend').style.display='none';
}

/* ==== 気象庁: 天気チップ・地震リング・新規地震イベント ==== */
let ENV=null; const eqEls={}; const seenEq=new Set(); let envFirst=true; let lastEqEvent=0;
/* 長期稼働で既知地震IDが際限なく増えないよう上限を設ける（Setは挿入順＝古い順） */
const SEEN_EQ_MAX = 500;
function rememberEq(eid){
  seenEq.add(eid);
  for(const k of seenEq){ if(seenEq.size <= SEEN_EQ_MAX) break; seenEq.delete(k); }
}
function syncQuakes(){
  // 3Dの背景はNavaraなのでMapLibre用の画面座標へ投影できない。
  // 天気・地震イベントのデータ更新は継続し、地図上リングだけを省く。
  if(window.GEOFRAME_MODE_3D) return;
  const layer=document.getElementById('eqlayer');
  const qs=(ENV&&ENV.quakes)||[]; const keep=new Set();
  qs.forEach(q=>{
    if(Math.abs(q.lat)>90 || Math.abs(q.lon)>180) return;  // 防御
    keep.add(q.eid);
    let el=eqEls[q.eid];
    if(!el){ el=document.createElement('div'); el.className='eq';
      el.innerHTML=`<div class="ering"></div><div class="ering"></div>`+
        `<div class="elbl">M${q.mag}${q.maxi?' 震度'+q.maxi:''} ${q.time}</div>`;
      layer.appendChild(el); eqEls[q.eid]=el; }
    const px=map.project([q.lon,q.lat]);
    el.style.left=px.x+'px'; el.style.top=px.y+'px';
    el.style.display=(px.x<-80||px.y<-80||px.x>innerWidth+80||px.y>innerHeight+80)?'none':'';
  });
  Object.keys(eqEls).forEach(k=>{ if(!keep.has(k)){ eqEls[k].remove(); delete eqEls[k]; } });
}
map.on('move', syncQuakes);
async function loadEnv(){
  try{ const r=await fetch('/api/env'); if(!r.ok) return; ENV=await r.json(); }catch(e){ return; }
  const w=ENV.weather;
  document.getElementById('weather').textContent = w
    ? `${w.text}${w.pop!=null?'  ☂'+w.pop+'%':''}${w.temps?'  '+w.temps.join('/')+'°C':''}` : '';
  syncQuakes();
  // 新規地震（震度3以上）は演出イベント。初回ロード分は既知として扱う
  for(const q of (ENV.quakes||[])){
    if(envFirst){ rememberEq(q.eid); continue; }
    if(seenEq.has(q.eid)) continue;
    rememberEq(q.eid);
    if(parseInt(q.maxi)>=3 && Date.now()>=eventUntil
       && Date.now()-lastEqEvent > 10*60e3){   // 群発地震でイベントが連鎖しないよう10分空ける
      lastEqEvent=Date.now();
      eventUntil=Date.now()+EQ_EVENT_MS;
      const bar=document.getElementById('alertbar');
      bar.className=''; bar.style.display='block';
      bar.textContent=`⚠ 地震発生 M${q.mag} 最大震度${q.maxi} ${q.place}`;
      armBase(q.lon, q.lat, 9.5);
      map.flyTo({center:[q.lon,q.lat], zoom:9.5, pitch:0, bearing:0,
                 duration:LOW_POWER?2500:4000, essential:true});
      eventCard('▸ SEISMIC EVENT',
        `<div class="ph" style="background:linear-gradient(135deg,#3a0a22,#12060f)"><span class="label">JMA QUAKE FEED</span></div>`,
        `${q.place}`, `M${q.mag}  最大震度${q.maxi}  ${q.time}\n出典: 気象庁`);
      setMode('SEISMIC WATCH');
      showHazard(q.lat, q.lon);   // 防災レイヤ（洪水・津波・土砂・液状化等）を重畳
      setTimeout(()=>{ bar.style.display='none';
        setMode('PHOTO JOURNEY'); }, EQ_EVENT_MS);
    }
  }
  envFirst=false;
  buildTicker();
}
