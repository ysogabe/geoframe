/* ============================================================
   photocard.js — アクティブマーカー・近傍写真スキャッター・写真カード表示
   ※読み込み順が契約: map.js の後。ここでの map.on('move', syncMarker) は
      events.js の map.on('move', syncAircraft/syncQuakes) より必ず先に登録すること
      （'move' ハンドラの呼び出し順は登録順で決まる。元 index.html と同じ順序）
   宣言する主なグローバル: activeLngLat, markerEl, syncMarker,
                           SCATTER_MAX, SCATTER_KM, kmBetween, clearScatter,
                           showScatter, photoSeq, showPhoto, intelSeq, loadIntel
   依存する他ファイルのグローバル: LOW_POWER (config.js),
                                   countUp/fmtRel/typewrite/placeholder (util.js),
                                   PHOTOS/usingMock (data.js), map (map.js)
   ※kmBetween は汎用関数だが呼び出し元が showScatter だけなので、元の位置関係を
     優先してこのファイルに残した
   ============================================================ */

/* ==== アクティブマーカー追従（DOM／フィルタ非適用レイヤ） ==== */
let activeLngLat = [PHOTOS[0].lon, PHOTOS[0].lat];
const markerEl = document.getElementById('marker');
function syncMarker(){ const px = map.project(activeLngLat);
  markerEl.style.left=px.x+'px'; markerEl.style.top=px.y+'px'; }
map.on('move', syncMarker); map.on('render', syncMarker);

/* ==== 近傍写真スキャッター（同じ場所の写真を複数同時表示） ==== */
const SCATTER_MAX = LOW_POWER ? 2 : 4;
const SCATTER_KM = 3;
function kmBetween(a,b,c,d){
  const R=6371, r=Math.PI/180;
  const s=Math.sin((c-a)*r/2)**2 + Math.cos(a*r)*Math.cos(c*r)*Math.sin((d-b)*r/2)**2;
  return 2*R*Math.asin(Math.sqrt(s));
}
function clearScatter(){ document.getElementById('scatter').innerHTML=''; }
function showScatter(p, iMain){
  clearScatter();
  const near = [];
  PHOTOS.forEach((q,j)=>{
    if(j===iMain || !q.img) return;
    if(kmBetween(p.lat,p.lon,q.lat,q.lon) <= SCATTER_KM) near.push(q);
  });
  if(!near.length) return;
  for(let i=near.length-1;i>0;i--){ const k=Math.floor(Math.random()*(i+1));
    [near[i],near[k]]=[near[k],near[i]]; }
  const slots=[[46,13],[63,22],[50,58],[66,62],[57,38]];
  for(let i=slots.length-1;i>0;i--){ const k=Math.floor(Math.random()*(i+1));
    [slots[i],slots[k]]=[slots[k],slots[i]]; }
  const layer=document.getElementById('scatter');
  near.slice(0,SCATTER_MAX).forEach((q,k)=>{
    const [sx,sy]=slots[k%slots.length];
    const el=document.createElement('div'); el.className='scat';
    el.style.width=(230+Math.random()*130)+'px';
    el.style.left=(sx+Math.random()*8-4)+'%';
    el.style.top=(sy+Math.random()*8-4)+'%';
    el.style.setProperty('--rot',(Math.random()*12-6).toFixed(1)+'deg');
    el.style.zIndex=String(9-k);
    el.innerHTML=`<img src="${q.img}" onerror="this.parentNode.remove()">`+
      `<div class="cap">▸ ${q.time?q.time.slice(5,16):''}${q.inferred?' ≈':''}</div>`;
    layer.appendChild(el);
    setTimeout(()=>el.classList.add('show'), 350+k*450);
  });
}

/* ==== 表示更新 ==== */
let photoSeq=0;          // 画像ロードの世代番号（loadIntelのintelSeqと同方式）
function showPhoto(i){
  const seq=++photoSeq;
  const p=PHOTOS[i], card=document.getElementById('photocard');
  card.classList.remove('hidden'); card.classList.remove('pc-glitch');
  void card.offsetWidth; card.classList.add('pc-glitch');
  card.querySelector('.pc-head span').textContent='▸ PHOTO LOG';
  document.getElementById('pc-idx').textContent=`${String(i+1).padStart(2,'0')}/${String(PHOTOS.length).padStart(2,'0')}`;
  document.getElementById('pc-title').textContent=p.title;
  const imgBox=document.getElementById('pc-img');
  imgBox.innerHTML=placeholder(p);
  const img=new Image();
  // 遅れて完了した前の写真のロードが、今の表示を上書きしないようにする
  img.onload=()=>{ if(seq!==photoSeq) return;
    imgBox.innerHTML=''; imgBox.appendChild(img); };
  img.src=p.img;
  const latS=p.lat.toFixed(4), lonS=p.lon.toFixed(4);
  const inf=p.inferred?'≈':'';
  const full=[p.pref,p.city,p.place].filter(Boolean).join(' ');
  const locLine=full?`LOC ${inf}${full}${p.inferred?'（時刻から推定）':''}\n`:(p.inferred?'LOC ≈時刻から推定\n':'');
  const rel=fmtRel(p.time);
  typewrite(document.getElementById('pc-exif'),
    `${locLine}CAPTURED ${p.time||'— (EXIF時刻なし)'}${rel?`（${rel}）`:''}\n`+
    `LAT ${inf}${latS} N / LON ${inf}${lonS} E  ALT ${p.intel.elev}m`);
  // AREA INTEL: 不動産情報ライブラリの実データを取得（キー未設定時はモック）
  countUp(document.getElementById('v-elev'), p.intel.elev, 900);
  loadIntel(p, i);
  showScatter(p, i);   // 近くで撮った写真を散らして同時表示
}

let intelSeq=0;
async function loadIntel(p, i){
  const seq=++intelSeq;
  const set=(id,v)=>{ document.getElementById(id).textContent = v; };
  const badge=document.getElementById('intel-badge');
  try{
    ['v-youto','v-ratio','v-price','v-flood','v-pop'].forEach(id=>set(id,'…'));
    const r = await fetch(`/api/intel?lat=${p.lat}&lon=${p.lon}`);
    const d = await r.json();
    if(seq!==intelSeq) return;            // すでに次の写真へ進んでいたら破棄
    if(!d.ok) throw 0;
    badge.textContent='REAL DATA'; badge.style.background='var(--green)';
    const priceTxt = d.price
      ? `${Number(String(d.price).replace(/[^\d]/g,''))?Number(String(d.price).replace(/[^\d]/g,'')).toLocaleString():d.price}円/㎡ (${d.price_m}m)` : '';
    set('v-youto', d.youto || '—');
    set('v-ratio', d.ratio || '—');
    set('v-price', priceTxt || '—');
    const fl=document.getElementById('v-flood');
    fl.textContent = d.flood || '—';
    fl.classList.toggle('warn', !!d.flood && d.flood!=='区域外');
    set('v-pop', d.pop2050!=null ? d.pop2050.toLocaleString() : '—');
    // 実測値を写真オブジェクトに載せる（ティッカーはここに載った値だけを流す）
    p.real = {youto: d.youto||'', ratio: d.ratio||'', price: priceTxt,
              flood: d.flood||'', pop2050: d.pop2050};
  }catch(e){
    if(seq!==intelSeq) return;
    if(usingMock){        // モック写真のデモ表示のみ、従来どおり擬似値を出す
      badge.textContent='MOCK'; badge.style.background='var(--amber)';
      set('v-youto', p.intel.youto); set('v-ratio','—');
      set('v-price', p.intel.price.toLocaleString()+'円/㎡');
      const fl=document.getElementById('v-flood');
      fl.textContent=p.intel.flood;
      fl.classList.toggle('warn', p.intel.flood!=='区域外');
      set('v-pop', p.intel.pop.toLocaleString());
    }else{                // 実写真では擬似値を出さない（未取得は素直に空欄）
      badge.textContent='NO DATA'; badge.style.background='var(--amber)';
      ['v-youto','v-ratio','v-price','v-pop'].forEach(id=>set(id,'—'));
      const fl=document.getElementById('v-flood');
      fl.textContent='—'; fl.classList.remove('warn');
    }
  }
}
