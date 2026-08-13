/* ============================================================
   util.js — 依存を持たない汎用関数
   ※読み込み順が契約: config.js の次、data.js より前
   宣言する主なグローバル: seeded, countUp, fmtRel, _twTimers, typewrite, placeholder
   依存する他ファイルのグローバル: なし（placeholder は同ファイル内の seeded のみ使用）
   ※元 index.html では countUp/fmtRel/typewrite/placeholder は地図生成の後ろに
     書かれていたが、いずれも純粋な関数宣言でトップレベル実行を伴わないため
     ここへ前倒ししている（_twTimers の WeakMap 生成も依存なし）
   ============================================================ */

/* ==== 決定的な擬似乱数（演出用ポリゴン生成に使用） ==== */
function seeded(seed){ let s = seed % 2147483647; if(s<=0) s+=2147483646;
  return ()=> (s = s*16807 % 2147483647) / 2147483647; }

/* ==== 数値カウントアップ ==== */
function countUp(el, target, dur=1200, fmt=v=>Math.round(v).toLocaleString()){
  const t0=performance.now();
  (function step(t){ const p=Math.min((t-t0)/dur,1), e=1-Math.pow(1-p,3);
    el.textContent=fmt(target*e); if(p<1) requestAnimationFrame(step); })(t0);
}
/* ==== 相対時刻（「1年8ヶ月前」等） ==== */
function fmtRel(timeStr){
  if(!timeStr) return '';
  const t=new Date(timeStr.replace(' ','T'));
  if(isNaN(t)) return '';
  const d=(Date.now()-t.getTime())/86400000;
  if(d<0) return '';
  if(d<1) return '今日';
  if(d<30) return `${Math.floor(d)}日前`;
  if(d<365){ const m=Math.floor(d/30.44); return `${m}ヶ月前`; }
  const y=Math.floor(d/365.25), m=Math.floor((d-y*365.25)/30.44);
  return m>0?`${y}年${m}ヶ月前`:`${y}年前`;
}

/* ==== タイプライター ====
   同じ要素に対して二重起動すると前回のtickが生き残って文字列が混ざるため、
   要素ごとに実行中タイマーを覚えておき、開始時に必ず止める */
const _twTimers = new WeakMap();
function typewrite(el, text, speed=18){
  clearTimeout(_twTimers.get(el));
  el.textContent=''; let i=0;
  (function tick(){ if(i<=text.length){ el.textContent=text.slice(0,i++);
    _twTimers.set(el, setTimeout(tick,speed)); } })(); }

/* ==== 写真プレースホルダ（img読込失敗時のフォールバック） ==== */
function placeholder(p){
  const [h1,h2]=p.hue, rnd=seeded(p.lat*1e4|0);
  let lines='';
  for(let i=0;i<7;i++){ const y=12+i*13+rnd()*6;
    lines+=`<path d="M0 ${y} Q 25 ${y-8-rnd()*10} 50 ${y} T 100 ${y}" fill="none" stroke="rgba(255,255,255,.25)" stroke-width=".5"/>`; }
  return `<div class="ph" style="background:linear-gradient(135deg,hsl(${h1} 65% 22%),hsl(${h2} 70% 40%))">
    <svg viewBox="0 0 100 90" preserveAspectRatio="none">${lines}</svg>
    <span class="label">NO PHOTO — ${p.img}</span></div>`;
}
