/* ============================================================
   panels.js — 時計 / ティッカー / イベントカード / 室内環境 / 在不在 /
               TRIP REPLAY / TRAVEL STATS の各パネル
   ※読み込み順が契約: events.js の後（ENV を参照する buildTicker を含むため）
   宣言する主なグローバル: DAYS, buildTicker, eventCard, ENV_LABELS, fmtEnvVal,
                           pollRoomEnv, sleeping, nightMode, isNightByClock,
                           applyNight, pollPresence, startLivePanels, lastIntroTrip,
                           showTripIntro, STATS_EVERY, STATS, loadStats,
                           statsCamera, showStats, WK_DAYS, WX_ICON, WX_TEXT,
                           wxIcon, wxText, showWeekly
   依存する他ファイルのグローバル: LOW_POWER/NIGHT_FROM_H/NIGHT_TO_H (config.js),
       countUp/typewrite (util.js),
       PHOTOS/usingMock (data.js), map/armBase/fitZoom/ensurePrefLayer/setPrefVisible (map.js),
       markerEl/clearScatter (photocard.js), ENV (events.js)
   ※時計の setInterval は元 index.html では地図直後にあったが、
     どのグローバルにも依存しない独立した実行文なのでこの位置で問題ない
   ※eventCard は元 index.html では ADS-B ブロック内（renderAirPanel と
     triggerEvent の間）にあるが、パネル系として指定どおりここへ移した。
     呼び出しは全て実行時なので読み込み順の問題は起きない
   ============================================================ */

/* ==== 時計 ==== */
const DAYS=['SUN','MON','TUE','WED','THU','FRI','SAT'];
setInterval(()=>{ const d=new Date(), z=n=>String(n).padStart(2,'0');
  document.getElementById('clock').textContent=`${z(d.getHours())}:${z(d.getMinutes())}:${z(d.getSeconds())}`;
  document.getElementById('dateline').textContent=`${d.getFullYear()}.${z(d.getMonth()+1)}.${z(d.getDate())} ${DAYS[d.getDay()]}`;
},250);


/* ==== ティッカー ====
   実写真では擬似値（p.intel の乱数生成分）を一切流さない。
   不動産情報ライブラリの実測値は loadIntel が p.real に載せた分だけを使い、
   未取得の写真は旅名・撮影地・撮影日など実データ由来の文言に差し替える */
function buildTicker(){
  const tripLabel = t => {
    const tr = STATS && STATS.trips && STATS.trips.find(x=>x.id===t);
    return tr ? tr.label : '';
  };
  const parts=PHOTOS.slice(0,30).map(p=>{
    const head=`<b>▸ ${p.title}</b> ${p.lat.toFixed(3)}N ${p.lon.toFixed(3)}E`;
    if(usingMock)   // デモ用モックデータはこれまでどおり
      return `${head} — 用途:${p.intel.youto} / 地価:${p.intel.price.toLocaleString()}円/㎡ / 浸水:${p.intel.flood}`;
    const bits=[], r=p.real;
    if(r){
      if(r.youto) bits.push(`用途:${r.youto}`);
      if(r.price) bits.push(`地価:${r.price}`);
      if(r.flood) bits.push(`浸水:${r.flood}`);
    }
    if(!bits.length){          // 実測値がまだ無い写真は実データだけで組む
      const trip=(p.trip!==undefined)?tripLabel(p.trip):'';
      if(trip) bits.push(trip);
      const loc=[p.pref,p.city,p.place].filter(Boolean).join(' ');
      if(loc) bits.push(loc);
      if(p.realElev) bits.push(`標高:${p.intel.elev}m`);
      if(p.time) bits.push(`撮影:${p.time}`);
    }
    return bits.length ? `${head} — ${bits.join(' / ')}` : head;
  });
  const note = usingMock
    ? '[ALL VALUES = MOCK DATA — server.py起動で実写真＋地理院実データに切替]'
    : '[標高・地名 = 地理院実データ / 用途地域・地価・浸水 = 不動産情報ライブラリ実データ（取得済み地点のみ）]';
  let env='';
  if(ENV&&ENV.weather) env += ` /// <b>▸ 天気(${ENV.weather.area})</b> ${ENV.weather.text}${ENV.weather.pop!=null?' ☂'+ENV.weather.pop+'%':''}`;
  if(ENV&&ENV.quakes&&ENV.quakes.length){ const q=ENV.quakes[0];
    env += ` /// <b>▸ 直近地震</b> ${q.time} M${q.mag}${q.maxi?' 震度'+q.maxi:''} ${q.place}`; }
  document.getElementById('ticker-inner').innerHTML =
    parts.join('&nbsp;&nbsp;&nbsp;///&nbsp;&nbsp;&nbsp;')+'&nbsp;&nbsp;&nbsp;///&nbsp;&nbsp;&nbsp;'+note+env;
}

function eventCard(head, imgHtml, title, exif){
  clearScatter();   // イベント表示中は散らし写真を片付ける
  const card=document.getElementById('photocard');
  card.classList.remove('hidden','pc-glitch'); void card.offsetWidth; card.classList.add('pc-glitch');
  card.querySelector('.pc-head span').textContent = head;
  document.getElementById('pc-idx').textContent='LIVE';
  document.getElementById('pc-img').innerHTML = imgHtml;
  document.getElementById('pc-title').textContent = title;
  typewrite(document.getElementById('pc-exif'), exif);
}

/* ==== ROOM ENV（HAの室内環境センサー） ==== */
const ENV_LABELS = {temperature:'室温', humidity:'湿度', carbon_dioxide:'CO2',
                    pressure:'気圧', volatile_organic_compounds:'VOC'};
function fmtEnvVal(it){
  const v = parseFloat(it.value);
  if(isNaN(v)) return it.value;
  if(it.class==='temperature') return v.toFixed(1);
  if(it.class==='pressure') return v.toFixed(1);
  if(it.class==='humidity') return Math.round(v);
  return Math.round(v).toLocaleString();
}
async function pollRoomEnv(){
  try{
    const r = await fetch('/api/roomenv'); if(!r.ok) return;
    const d = await r.json();
    const panel = document.getElementById('envpanel');
    if(!d.ok || !d.items.length){ panel.style.display='none'; return; }
    panel.style.display='block';
    document.getElementById('env-rows').innerHTML = d.items.map(it=>{
      const label = ENV_LABELS[it.class] || it.name;
      let cls='';
      if(it.class==='carbon_dioxide'){
        const v=parseFloat(it.value);
        if(v>=1500) cls='warn'; else if(v>=1000) cls='warn2';   // 換気目安
      }
      return `<div class="tile"><span class="k">${label}</span>`+
        `<span class="v ${cls}">${fmtEnvVal(it)}<small> ${it.unit}</small></span></div>`;
    }).join('');
  }catch(e){}
}

/* ==== 在/不在スリープ（HA人感センサー）＋ナイトモード（減光） ==== */
let sleeping=false;
let nightMode=null;     // null=未判定。body.night の現在状態と一致させる
/* HAのsun.sunが取れないとき（/api/presence の night が null）の時刻フォールバック */
function isNightByClock(){
  const h = new Date().getHours();
  return NIGHT_FROM_H > NIGHT_TO_H ? (h>=NIGHT_FROM_H || h<NIGHT_TO_H)
                                   : (h>=NIGHT_FROM_H && h<NIGHT_TO_H);
}
function applyNight(on){
  on = !!on;
  if(on===nightMode) return;
  nightMode = on;
  document.body.classList.toggle('night', on);
}
async function pollPresence(){
  let d = null;
  try{
    const r=await fetch('/api/presence');
    if(r.ok) d = await r.json();
  }catch(e){}
  // 減光は通信できなくても続ける必要がある（寝室のキオスクが眩しいままにならないように）。
  // night が真偽値で来ていればそれを、null/キー無し/取得失敗なら時刻で判定する
  applyNight(d && typeof d.night==='boolean' ? d.night : isNightByClock());
  if(!d) return;
  const off = d.screen==='off';
  if(off!==sleeping){
    sleeping=off;
    document.getElementById('sleep').classList.toggle('on', off);
    setMode(off?'SLEEP':'PHOTO JOURNEY');
  }
}

/* ==== 常時更新するライブHUD ====
   MapLibreと3Dのどちらでも、ADS-B・天気・在不在・HA室内環境は同じDOMカードを使う。
   起動経路が複数あってもintervalを二重登録しないよう、この入口で一度だけ開始する。 */
let livePanelsStarted = false;
function startLivePanels(){
  if(livePanelsStarted) return;
  livePanelsStarted = true;
  pollAircraft(); setInterval(pollAircraft, ADSB_POLL);
  loadEnv(); setInterval(loadEnv, 300000);
  pollPresence(); setInterval(pollPresence, 15000);
  pollRoomEnv(); setInterval(pollRoomEnv, 60000);
}

/* ==== TRIP REPLAY（旅行クラスタのイントロ） ==== */
let lastIntroTrip=-1;
function showTripIntro(tid){
  const tr=STATS&&STATS.trips&&STATS.trips.find(t=>t.id===tid);
  if(!tr) return false;
  markerEl.style.display='none';
  document.getElementById('statspanel').classList.add('hidden');
  setPrefVisible(false);     // 制覇マップは統計カットの間だけ出す
  setMode('TRIP REPLAY');
  const b=tr.bounds;
  armBase((b[0]+b[2])/2, (b[1]+b[3])/2, fitZoom(b, 11));
  map.fitBounds([[b[0],b[1]],[b[2],b[3]]],
    {padding:140, pitch:0, bearing:0, duration:LOW_POWER?2500:4500, maxZoom:11});
  eventCard('▸ TRIP REPLAY',
    `<div class="ph" style="background:linear-gradient(135deg,hsl(${(tid*67)%360} 60% 20%),hsl(${(tid*67+50)%360} 65% 38%))"><span class="label">TRIP ${String(tid+1).padStart(2,'0')}</span></div>`,
    tr.label, `${tr.count} PHOTOS\n${tr.start}${tr.end&&tr.end!==tr.start?' 〜 '+tr.end:''}`);
  document.getElementById('pc-idx').textContent='TRIP';
  return true;
}

/* ==== TRAVEL STATS（自分統計インタールード） ==== */
const STATS_EVERY = 10;   // 写真Nカットごとに統計画面を1カット挟む
let STATS = null;
async function loadStats(){
  try{ const r = await fetch('/api/stats');
    if(r.ok){ STATS = await r.json(); buildTicker(); }   // 旅名をティッカーに反映する
  }catch(e){}
}
/* 統計カット／週間予報カット共通のカメラ演出（日本全体をz4.2で俯瞰する）。
   z4.2は地理院タイルの提供外 → OSMを出す */
function statsCamera(){
  clearScatter();
  document.getElementById('photocard').classList.add('hidden');
  markerEl.style.display='none';
  armBase(136.5, 37.0, 4.2);
  map.flyTo({center:[136.5,37.0], zoom:4.2, pitch:0, bearing:0,
             duration:LOW_POWER?2500:5000, essential:true});
}

function showStats(){
  if(!STATS || !STATS.total) return false;
  statsCamera();
  setMode('TRAVEL STATS');
  const sp=document.getElementById('statspanel');
  sp.classList.remove('hidden');
  // ★週間予報カットと同じパネルを使い回しているので、必ず .wk を外して見出しも戻す
  //   （戻し忘れると2回目以降の統計カットが空パネルになる）
  sp.classList.remove('wk');
  document.getElementById('sp-head').textContent='▸ TRAVEL STATS';
  // 制覇マップ（訪問済み県の塗り分け）はこのカットの間だけ出す
  ensurePrefLayer(STATS.pref_list||[]);
  setPrefVisible(true);
  const pc=STATS.pref_count||0, pct=Math.round(pc/47*100);
  document.getElementById('st-conquer').innerHTML =
    `<div class="cq-lab">▸ <b>${pc}/47</b> 県 制覇<span>${pct}%</span></div>`+
    `<div class="cq-bar"><i style="width:${pct}%"></i></div>`;
  countUp(document.getElementById('st-pref'), STATS.pref_count, 1400);
  countUp(document.getElementById('st-total'), STATS.total, 1200);
  countUp(document.getElementById('st-dist'), STATS.dist_km, 1600);
  const sp0=STATS.span[0]?STATS.span[0].slice(0,7):'', sp1=STATS.span[1]?STATS.span[1].slice(0,7):'';
  document.getElementById('st-span').textContent = sp0&&sp1?`${sp0}〜${sp1}`:'--';
  const mx=Math.max(1,...STATS.months);
  document.getElementById('st-bars').innerHTML =
    STATS.months.map(v=>`<div style="height:${Math.round(v/mx*100)}%" title="${v}"></div>`).join('');
  document.getElementById('st-top').innerHTML =
    'TOP: '+(STATS.top_cities||[]).slice(0,3).map(c=>`<b>${c[0]}</b>×${c[1]}`).join('　')+
    (STATS.inferred?`<br>位置推定 ${STATS.inferred}枚 / 取得待ち ${STATS.pending}枚`:'');
  return true;
}

/* ==== WEEKLY FORECAST（週間天気予報カット） ====
   データは /api/env の weekly（気象庁）。統計カットと同じパネル・同じカメラ演出を使う。

   気象庁の週間予報は天気コードだけでテキストを持たないので、コードから表示を組む。
   ★あえて「晴時々曇」のような細かい天気予報文（telop）は持たない:
     気象庁はコード→telopの機械可読な対応表を安定URLで公開していない
     （bosai配下に telops.json 相当は存在しない）。うろ覚えの対応表を焼き込むと
     公式の予報文だと誤解される文言を出しうるため、確実に正しい大分類だけを出す。
   採用する規則: コードの上1桁が大分類（1=晴 / 2=曇 / 3=雨 / 4=雪）。
   ※この規則は全国56予報官署の実データから集めた (コード, 予報文) 91組で検証済み
     （1xx は必ず「晴れ…」、2xx は「くもり…」、3xx は「雨…」で始まり違反0件）。
   細かい今日の予報文が見たい場合は上部バーの天気チップ（ENV.weather.text＝
   気象庁の予報文そのまま）に出ている */
const WK_DAYS = ['日','月','火','水','木','金','土'];
const WX_ICON = {'1':'☀','2':'☁','3':'☂','4':'❄'};
const WX_TEXT = {'1':'晴','2':'曇','3':'雨','4':'雪'};
function wxIcon(code){ return WX_ICON[String(code).charAt(0)] || '·'; }
function wxText(code){ return WX_TEXT[String(code).charAt(0)] || '—'; }
function showWeekly(){
  const wk = (ENV && Array.isArray(ENV.weekly)) ? ENV.weekly : [];
  if(!wk.length) return false;
  statsCamera();
  setPrefVisible(false);      // 制覇マップは統計カット専用
  setMode('WEEKLY FORECAST');
  const sp=document.getElementById('statspanel');
  sp.classList.remove('hidden');
  sp.classList.add('wk');
  const area = (ENV && ENV.weather && ENV.weather.area) ? ENV.weather.area : '';
  document.getElementById('sp-head').textContent =
    '▸ WEEKLY FORECAST' + (area?'  '+area:'');
  // 上流の文字列は一切そのまま埋め込まない（コードは表引き、他は数値化して出す）
  const rows = wk.slice(0,7).map(d=>{
    const t = new Date((d.date||'')+'T00:00:00');
    const lab = isNaN(t) ? '--' : `${t.getMonth()+1}/${t.getDate()}`;
    const dow = isNaN(t) ? '' : WK_DAYS[t.getDay()];
    const cls = dow==='日'?' sun':(dow==='土'?' sat':'');
    const pop = (typeof d.pop==='number') ? d.pop+'%' : '—';
    const tmx = (typeof d.temp_max==='number') ? d.temp_max : '—';
    const tmn = (typeof d.temp_min==='number') ? d.temp_min : '—';
    return `<div class="wk-row${cls}"><span class="d">${lab}<small>${dow}</small></span>`+
      `<span class="ic">${wxIcon(d.code)}</span><span class="wt">${wxText(d.code)}</span>`+
      `<span class="pp">${pop}</span>`+
      `<span class="tt"><b>${tmx}</b>/<i>${tmn}</i></span></div>`;
  }).join('');
  document.getElementById('wk-body').innerHTML =
    '<div class="wk-hdr"><span class="d">DATE</span><span class="ic"></span>'+
    '<span class="wt">天気</span><span class="pp">降水</span>'+
    '<span class="tt">最高/最低</span></div>'+rows+
    '<div class="wk-note">出典: 気象庁 週間天気予報</div>';
  return true;
}
