'use strict';
const $ = s => document.querySelector(s);
const state = {tripId:'', plan:null, candidates:null, manifest:null, dirty:false};

function busy(on, text){ $('#busy').classList.toggle('hidden', !on); if(text) $('#busy span').textContent=text; }
let toastTimer;
function toast(text, error=false){ const n=$('#toast'); n.textContent=text; n.className='toast show'+(error?' error':''); clearTimeout(toastTimer); toastTimer=setTimeout(()=>n.className='toast',3500); }
async function json(url, options){ const r=await fetch(url, options); const j=await r.json().catch(()=>({})); if(!r.ok||!j.ok) throw new Error(j.error||`HTTP ${r.status}`); return j; }
function post(url, body){ return json(url,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}); }
function stat(value,label){ const n=document.createElement('div'); n.className='stat'; const b=document.createElement('b'); b.textContent=value; const s=document.createElement('span'); s.textContent=label; n.append(b,s); return n; }

async function boot(){
  try{
    const trips=await json('/api/blog/trips');
    const select=$('#trip-select'); select.textContent='';
    trips.items.forEach(t=>{ const o=document.createElement('option'); o.value=t.trip_id; o.textContent=t.title; select.appendChild(o); });
    if(!trips.items.length) throw new Error('旅程JSONがありません');
    select.addEventListener('change',()=>loadTrip(select.value));
    await loadTrip(trips.items[0].trip_id);
  }catch(e){ toast(e.message,true); busy(false); }
}

async function loadTrip(tripId){
  busy(true,'旅程と写真DBを照合しています…'); state.tripId=tripId;
  try{
    const [trip,candidates,draft]=await Promise.all([
      json('/api/blog/trip?trip_id='+encodeURIComponent(tripId)),
      json('/api/blog/candidates?trip_id='+encodeURIComponent(tripId)),
      json('/api/blog/draft?trip_id='+encodeURIComponent(tripId))
    ]);
    state.plan=trip.plan; state.candidates=candidates; state.manifest=draft.manifest; state.dirty=false;
    render(); toast(draft.source==='saved'?'保存済み下書きを開きました':'写真DBから下書きを生成しました');
  }catch(e){ toast('読み込み失敗: '+e.message,true); }
  finally{ busy(false); }
}

function chapterById(id){ return state.manifest.chapters.find(c=>c.id===id); }
function selectedMap(chapter){ return new Map((chapter.images||[]).map(i=>[String(i.photo_id),i])); }
function candidateImages(group){ return group.candidates; }

function render(){
  $('#title').textContent=state.manifest.title; $('#subtitle').textContent=state.manifest.subtitle||'';
  const selected=state.manifest.chapters.reduce((n,c)=>n+c.images.length,0);
  const stats=$('#stats'); stats.textContent=''; stats.append(stat(state.candidates.photo_count,'期間内の写真'),stat(selected,'初期選択'),stat(state.plan.segments.length,'日程'),stat('GPS非表示','プライバシー'));
  const nav=$('#chapter-nav'), box=$('#chapters'); nav.textContent=''; box.textContent='';
  state.candidates.chapters.forEach((group,index)=>{
    const chapter=chapterById(group.segment_id); if(!chapter) return;
    const a=document.createElement('a'); a.href='#chapter-'+group.segment_id; a.textContent=`DAY ${index+1} · ${group.date.slice(5)}`; nav.appendChild(a);
    const node=$('#chapter-template').content.firstElementChild.cloneNode(true); node.id='chapter-'+group.segment_id;
    node.querySelector('.chapter-day').textContent=`DAY ${index+1} · ${group.date} · ${group.timezone}`;
    node.querySelector('h2').textContent=chapter.title;
    node.querySelector('.chapter-meta').textContent=chapter.location_label;
    node.querySelector('.photo-count').textContent=`${group.photo_count} PHOTOS → ${group.candidates.length}候補`;
    const body=node.querySelector('.body'); body.value=chapter.body; body.addEventListener('input',()=>{chapter.body=body.value;dirty();});
    const selected=selectedMap(chapter); const grid=node.querySelector('.photo-grid');
    candidateImages(group).forEach(photo=>grid.appendChild(photoCard(photo,chapter,selected)));
    box.appendChild(node);
  });
}

function photoCard(photo,chapter,selected){
  const options=[photo].concat((photo.alternates||[]).map(a=>Object.assign({},photo,a,{alternates:[]})));
  let optionIndex=options.findIndex(p=>selected.has(String(p.photo_id))); if(optionIndex<0) optionIndex=0;
  let current=options[optionIndex], id=String(current.photo_id), saved=selected.get(id);
  const card=document.createElement('div'); card.className='photo-card'+(saved?' selected':'');
  const img=document.createElement('img'); img.loading='lazy'; img.decoding='async'; card.appendChild(img);
  const pick=document.createElement('label'); pick.className='pick'; const check=document.createElement('input'); check.type='checkbox'; check.checked=!!saved; pick.appendChild(check); card.appendChild(pick);
  const score=document.createElement('span'); score.className='score'; score.textContent=Math.round((photo.score||0)*100); card.appendChild(score);
  const info=document.createElement('div'); info.className='photo-info';
  const time=document.createElement('div'); time.className='photo-time';
  const loc=document.createElement('div'); loc.className='photo-loc'; loc.textContent=photo.location_label||chapter.location_label;
  const cap=document.createElement('textarea'); cap.className='caption'; cap.rows=2; cap.placeholder='キャプション';
  const reasons=document.createElement('div'); reasons.className='reasons'; reasons.textContent=(photo.selection_reason||[]).join(' · ')+(photo.duplicate_count>1?` · 同時刻${photo.duplicate_count}枚`:'');
  const alternate=document.createElement('button'); alternate.className='button alternate'; alternate.type='button';
  if(options.length>1){ alternate.textContent=`別カット ${optionIndex+1}/${options.length}`; info.append(time,loc,cap,alternate,reasons); }
  else info.append(time,loc,cap,reasons);
  card.appendChild(info);
  function showCurrent(){
    id=String(current.photo_id); saved=selected.get(id);
    img.src=current.src; img.alt=current.alt||chapter.title;
    time.textContent=(current.captured_at||'').slice(11,16)||'--:--';
    cap.value=(saved&&saved.caption)||current.caption||`${chapter.title} [要確認]`;
    if(options.length>1) alternate.textContent=`別カット ${optionIndex+1}/${options.length}`;
  }
  function sync(){
    const ix=chapter.images.findIndex(i=>String(i.photo_id)===id);
    if(check.checked){
      // 同じ連写グループの旧代表が選択済みなら置き換える。
      options.forEach(p=>{ const j=chapter.images.findIndex(i=>String(i.photo_id)===String(p.photo_id)); if(j>=0&&String(p.photo_id)!==id) chapter.images.splice(j,1); });
      const now=chapter.images.findIndex(i=>String(i.photo_id)===id);
      const item={photo_id:id,src:current.src,alt:current.alt||chapter.title,caption:cap.value,score:current.score||0,selection_reason:current.selection_reason||[]};
      if(now<0) chapter.images.push(item); else chapter.images[now]=item;
    }else if(ix>=0) chapter.images.splice(ix,1);
    card.classList.toggle('selected',check.checked); dirty(); updateSelectedStat();
  }
  check.addEventListener('change',sync); cap.addEventListener('input',()=>{ if(check.checked) sync(); });
  alternate.addEventListener('click',()=>{
    const wasChecked=check.checked;
    if(wasChecked){ const old=chapter.images.findIndex(i=>String(i.photo_id)===id); if(old>=0) chapter.images.splice(old,1); }
    optionIndex=(optionIndex+1)%options.length; current=options[optionIndex]; showCurrent();
    if(wasChecked) sync(); else dirty();
  });
  showCurrent();
  return card;
}

function dirty(){ state.dirty=true; $('#save').textContent='下書き保存 ●'; }
function updateSelectedStat(){ const n=state.manifest.chapters.reduce((v,c)=>v+c.images.length,0); $('#stats .stat:nth-child(2) b').textContent=n; }
async function save(){
  try{ $('#save').disabled=true; await post('/api/blog/save',{trip_id:state.tripId,manifest:state.manifest}); state.dirty=false; $('#save').textContent='下書き保存'; toast('下書きを保存しました'); }
  catch(e){ toast('保存失敗: '+e.message,true); } finally{$('#save').disabled=false;}
}
async function exportBlog(){
  try{
    busy(true,'EXIFを除去して静的ブログを書き出しています…');
    const r=await post('/api/blog/export',{trip_id:state.tripId,manifest:state.manifest});
    toast(`${r.images}枚の画像とHTMLを書き出しました`);
    window.open(r.url,'_blank','noopener');
  }catch(e){ toast('書き出し失敗: '+e.message,true); } finally{busy(false);}
}
function reevaluate(){
  if(state.dirty&&!confirm('未保存の写真選択だけを初期候補へ戻します。本文は保持します。続けますか？')) return;
  state.candidates.chapters.forEach(group=>{
    const chapter=chapterById(group.segment_id); if(!chapter) return;
    chapter.images=group.candidates.filter(p=>p.selected).map(p=>({photo_id:p.photo_id,src:p.src,alt:p.alt,caption:p.caption||`${chapter.title} [要確認]`,score:p.score,selection_reason:p.selection_reason}));
  });
  dirty(); render(); toast('写真候補を再評価しました。本文は保持しています');
}

$('#save').addEventListener('click',save); $('#export').addEventListener('click',exportBlog); $('#reload').addEventListener('click',reevaluate);
window.addEventListener('beforeunload',e=>{if(state.dirty){e.preventDefault();e.returnValue='';}});
boot();
