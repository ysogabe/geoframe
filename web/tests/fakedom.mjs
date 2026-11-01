// vm で admin.js・photos.js・library.js・config.js を動かす描画試験のための、小さな DOM の代用品（依存なし）。
// 本物のブラウザの全部は真似ない。試験が必要とする範囲だけ:
//   - 要素の同一性（createElement したものが同じ参照のまま木に入る）、children 配列、親子の付け外し
//   - textContent（子の文字列をつなぐ getter と、子を置き換える setter）
//   - className / classList / dataset / setAttribute / getAttribute
//   - addEventListener（登録した関数を保持し、click() と dispatchEvent({type}) で呼ぶ。バブリングはしない。disabled なボタン等の click() は何も起こさない）
//   - disabled / value / checked / focus() と document.activeElement、getBoundingClientRect()（幅 100 の固定）、document.contains()
//   - querySelector / querySelectorAll: #id .class tag tag.class [data-x] [data-x=v] とその組み合わせ、
//     空白区切りの子孫（"nav .tab" など）
//   - document.createElement / createTextNode / getElementById / addEventListener / dispatchEvent
// buildAdminDom() は web/static/admin.html の骨格（id・class・data-*）を JS で組む。HTML は解析しない。
// 組んだ骨格が admin.html とずれていないかは htmlFacts() と domFacts() を突き合わせて試験で確かめる。

const kebabToCamel = s => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

class FakeText {
  constructor(data){ this.nodeType = 3; this.data = String(data); this.parentNode = null; }
  get textContent(){ return this.data; }
  set textContent(v){ this.data = String(v); }
}

class ClassList {
  constructor(owner){ this.owner = owner; }
  tokens(){ return this.owner.className.split(/\s+/).filter(Boolean); }
  set(tokens){ this.owner.className = tokens.join(' '); }
  add(...names){
    const t = this.tokens();
    names.forEach(n => { if(!t.includes(n)) t.push(n); });
    this.set(t);
  }
  remove(...names){ this.set(this.tokens().filter(n => !names.includes(n))); }
  contains(name){ return this.tokens().includes(name); }
  toggle(name, force){
    const on = force === undefined ? !this.contains(name) : !!force;
    if(on) this.add(name); else this.remove(name);
    return on;
  }
}

// 1 つの複合セレクタ（"button.tab[data-tab=status]" など）を {tag, id, classes, attrs} にする。
function parseCompound(src){
  const out = {tag: null, id: null, classes: [], attrs: []};
  let rest = src;
  const m = /^[a-zA-Z][a-zA-Z0-9-]*/.exec(rest);
  if(m){ out.tag = m[0].toLowerCase(); rest = rest.slice(m[0].length); }
  while(rest){
    let r;
    if((r = /^#([\w-]+)/.exec(rest))) out.id = r[1];
    else if((r = /^\.([\w-]+)/.exec(rest))) out.classes.push(r[1]);
    else if((r = /^\[(data-[\w-]+)(?:=("[^"]*"|'[^']*'|[^\]]*))?\]/.exec(rest))){
      const v = r[2] === undefined ? null : r[2].replace(/^["']|["']$/g, '');
      out.attrs.push([kebabToCamel(r[1].slice(5)), v]);
    }
    else throw new Error('fakedom: 未対応のセレクタ: ' + src);
    rest = rest.slice(r[0].length);
  }
  return out;
}
function matchesCompound(el, c){
  if(c.tag && el.tag !== c.tag) return false;
  if(c.id !== null && el.id !== c.id) return false;
  if(!c.classes.every(k => el.classList.contains(k))) return false;
  return c.attrs.every(([k, v]) =>
    Object.prototype.hasOwnProperty.call(el.dataset, k) && (v === null || String(el.dataset[k]) === v));
}
function matchesChain(el, chain){
  if(!matchesCompound(el, chain[chain.length - 1])) return false;
  let i = chain.length - 2, p = el.parentNode;
  while(i >= 0 && p && p.nodeType === 1){
    if(matchesCompound(p, chain[i])) i--;
    p = p.parentNode;
  }
  return i < 0;
}

const FORM_CONTROLS = new Set(['button', 'input', 'select', 'textarea']);

class FakeElement {
  constructor(tag, doc){
    this.nodeType = 1;
    this.tag = String(tag).toLowerCase();
    this.tagName = this.tag.toUpperCase();
    this.ownerDocument = doc;
    this.id = '';
    this.className = '';
    this.children = [];
    this.parentNode = null;
    this.style = {};
    // 本物の dataset と同じく、入れた値は文字列になる（el.dataset.id = 5 は '5'）
    this.dataset = new Proxy({}, {set(t, k, v){ t[k] = String(v); return true; }});
    this.attributes = {};
    this.listeners = {};
    this.classList = new ClassList(this);
    this.disabled = false;
    this.value = '';
    this.checked = false;
    this.title = '';
  }
  getBoundingClientRect(){ return {left: 0, top: 0, width: 100, height: 100}; }
  get childNodes(){ return this.children; }
  get textContent(){ return this.children.map(c => c.textContent).join(''); }
  set textContent(v){
    this.children.forEach(c => { c.parentNode = null; });
    this.children = [];
    if(v !== '' && v !== null && v !== undefined) this.appendChild(new FakeText(v));
  }
  appendChild(child){
    if(child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.children.push(child);
    return child;
  }
  removeChild(child){
    const i = this.children.indexOf(child);
    if(i >= 0){ this.children.splice(i, 1); child.parentNode = null; }
    return child;
  }
  remove(){ if(this.parentNode) this.parentNode.removeChild(this); }
  replaceChildren(...nodes){
    this.children.forEach(c => { c.parentNode = null; });
    this.children = [];
    nodes.forEach(n => this.appendChild(typeof n === 'string' ? new FakeText(n) : n));
  }
  setAttribute(name, value){
    const v = String(value);
    this.attributes[name] = v;
    if(name === 'id') this.id = v;
    else if(name === 'class') this.className = v;
    else if(name.startsWith('data-')) this.dataset[kebabToCamel(name.slice(5))] = v;
  }
  getAttribute(name){
    if(name === 'id') return this.id || null;
    if(name === 'class') return this.className || null;
    if(name.startsWith('data-')){
      const k = kebabToCamel(name.slice(5));
      return Object.prototype.hasOwnProperty.call(this.dataset, k) ? String(this.dataset[k]) : null;
    }
    return Object.prototype.hasOwnProperty.call(this.attributes, name) ? this.attributes[name] : null;
  }
  addEventListener(type, fn){ (this.listeners[type] = this.listeners[type] || []).push(fn); }
  dispatchEvent(ev){
    ev.target = ev.target || this;
    ev.currentTarget = this;
    if(!ev.preventDefault) ev.preventDefault = () => { ev.defaultPrevented = true; };
    if(!ev.stopPropagation) ev.stopPropagation = () => { ev.propagationStopped = true; };
    (this.listeners[ev.type] || []).slice().forEach(fn => fn(ev));
    // 親へ伝える（バブリング）のは、試験が `bubbles: true` を付けたイベントだけ（既存の試験は子で止まる前提）
    if(ev.bubbles && !ev.propagationStopped && this.parentNode && this.parentNode.nodeType === 1){
      this.parentNode.dispatchEvent(Object.assign(ev, {target: ev.target}));
    }
    return !ev.defaultPrevented;
  }
  // ブラウザと同じく、disabled なフォームコントロールの click() はイベントを発火しない
  // （dispatchEvent() で直接送ったイベントは、本物のブラウザでも届くのでそのまま）
  click(){
    if(this.disabled && FORM_CONTROLS.has(this.tag)) return;
    return this.dispatchEvent({type: 'click'});
  }
  focus(){ this.ownerDocument.activeElement = this; }
  descendants(){
    const out = [];
    const walk = n => n.children.forEach(c => { if(c.nodeType === 1){ out.push(c); walk(c); } });
    walk(this);
    return out;
  }
  querySelectorAll(selector){
    const chain = String(selector).trim().split(/\s+/).map(parseCompound);
    return this.descendants().filter(e => matchesChain(e, chain));
  }
  querySelector(selector){ return this.querySelectorAll(selector)[0] || null; }
}

export function createDocument(){
  const doc = {
    listeners: {},
    activeElement: null,
    createElement: tag => new FakeElement(tag, doc),
    createTextNode: text => new FakeText(text),
    addEventListener(type, fn){ (doc.listeners[type] = doc.listeners[type] || []).push(fn); },
    dispatchEvent(ev){
      ev.target = ev.target || doc;
      if(!ev.preventDefault) ev.preventDefault = () => { ev.defaultPrevented = true; };
      (doc.listeners[ev.type] || []).slice().forEach(fn => fn(ev));
      return !ev.defaultPrevented;
    },
    contains: node => { for(let n = node; n; n = n.parentNode) if(n === doc.body) return true; return false; },
    getElementById: id => doc.body.descendants().find(e => e.id === id) || null,
    querySelector: sel => doc.body.querySelector(sel),
    querySelectorAll: sel => doc.body.querySelectorAll(sel),
  };
  doc.body = new FakeElement('body', doc);
  doc.activeElement = doc.body;
  return doc;
}

// ---- admin.html の骨格 ----
// この表は web/static/admin.html のチップと同じ並び・同じ表示名にしておく（htmlFacts との突き合わせで id・data-* は検査される）。
export const CHIPS = [
  ['all', 'すべて'], ['noloc', '位置なし'], ['suggest', 'AI 候補あり'], ['inferred', '推定'],
  ['manual', '手動'], ['fav', '★ お気に入り'], ['hidden', '🚫 除外'],
];

export function buildAdminDom(){
  const document = createDocument();
  const mk = (tag, o = {}, parent = null) => {
    const e = document.createElement(tag);
    if(o.id) e.id = o.id;
    if(o.cls) e.className = o.cls;
    if(o.text !== undefined) e.textContent = o.text;
    Object.entries(o.data || {}).forEach(([k, v]) => { e.dataset[k] = v; });
    if(parent) parent.appendChild(e);
    return e;
  };
  const body = document.body;

  const header = mk('header', {}, body);
  const top = mk('div', {cls: 'hd-top'}, header);
  mk('div', {cls: 'logo', text: 'GEOFRAME'}, top);
  mk('div', {cls: 'sub', text: 'ADMIN CONSOLE'}, top);
  mk('div', {id: 'servertime', text: '--'}, top);
  const nav = mk('nav', {}, header);
  mk('button', {cls: 'tab on', text: '▸ 状況', data: {tab: 'status'}}, nav);
  mk('button', {cls: 'tab', text: '▸ 写真', data: {tab: 'photos'}}, nav);
  mk('button', {cls: 'tab', text: '▸ ライブラリ', data: {tab: 'library'}}, nav);
  mk('button', {cls: 'tab', text: '▸ 設定', data: {tab: 'config'}}, nav);

  const main = mk('main', {}, body);
  const st = mk('section', {id: 'v-status'}, main);
  const panel = (title, ...ids) => {
    const p = mk('div', {cls: 'panel'}, st);
    mk('h2', {text: title}, p);
    return ids.map(id => mk('div', {id, cls: 'grid'}, p));
  };
  panel('▸ TODO — 要対応', 's-todo');
  panel('▸ SYNC — 同期状態', 's-sync');
  const catalog = panel('▸ CATALOG — DB件数内訳', 's-db')[0].parentNode;
  const kvs = mk('div', {cls: 'grid'}, catalog);
  ['s-locsrc', 's-buckets', 's-source'].forEach(id => {
    const c = mk('div', {cls: 'cell'}, kvs);
    mk('span', {cls: 'k'}, c);
    mk('table', {id, cls: 'kv'}, c);
  });
  panel('▸ DISK — ディスク使用量', 's-disk');

  const ph = mk('section', {id: 'v-photos', cls: 'hidden'}, main);
  const sbar = mk('div', {cls: 'searchbar'}, ph);
  mk('input', {id: 'q'}, sbar);
  mk('button', {id: 'search', cls: 'btn', text: '検索'}, sbar);
  mk('button', {id: 'recommend', cls: 'btn', text: 'AI おすすめ'}, sbar);
  const opt = mk('label', {cls: 'opt'}, sbar);
  mk('input', {id: 'include-hidden'}, opt);
  opt.appendChild(document.createTextNode(' 除外も検索'));
  mk('button', {id: 'close-search', cls: 'btn hidden', text: '✕ 検索を閉じる'}, sbar);
  mk('div', {id: 'search-note', cls: 'search-note dim'}, ph);
  const bar = mk('div', {cls: 'toolbar'}, ph);
  const chips = mk('div', {id: 'chips', cls: 'chips'}, bar);
  CHIPS.forEach(([f, label], i) => {
    const b = mk('button', {cls: i === 0 ? 'chip on' : 'chip', data: {filter: f}}, chips);
    b.appendChild(document.createTextNode(label + ' '));
    mk('span', {cls: 'n'}, b);
  });
  mk('span', {cls: 'spacer'}, bar);
  const pager = mk('div', {id: 'pager', cls: 'toolbar'}, ph);
  mk('button', {id: 'prev', cls: 'btn', text: '◀ PREV'}, pager);
  mk('span', {id: 'pageinfo', cls: 'count', text: '--'}, pager);
  mk('button', {id: 'next', cls: 'btn', text: 'NEXT ▶'}, pager);
  mk('div', {id: 'cards'}, ph);
  mk('div', {id: 'groups', cls: 'hidden'}, ph);
  mk('div', {id: 'empty', cls: 'empty hidden', text: '該当する写真がありません'}, ph);

  // ライブラリタブ（admin.html の #v-library と同じ id）
  const lib = mk('section', {id: 'v-library', cls: 'hidden'}, main);
  const lbar = mk('div', {cls: 'toolbar lib-bar'}, lib);
  mk('button', {id: 'lib-view-trips', cls: 'lib-seg on', text: '旅', data: {view: 'trips'}}, lbar);
  mk('button', {id: 'lib-view-calendar', cls: 'lib-seg', text: '日付', data: {view: 'calendar'}}, lbar);
  mk('button', {id: 'lib-view-places', cls: 'lib-seg', text: '場所', data: {view: 'places'}}, lbar);
  mk('button', {id: 'lib-view-map', cls: 'lib-seg', text: '地図', data: {view: 'map'}}, lbar);
  const lopt = mk('label', {cls: 'opt'}, lbar);
  mk('input', {id: 'lib-hidden'}, lopt);
  lopt.appendChild(document.createTextNode(' 除外も含める'));
  mk('span', {cls: 'spacer'}, lbar);
  mk('span', {id: 'lib-note', cls: 'count'}, lbar);
  mk('div', {id: 'lib-trips'}, lib);
  mk('div', {id: 'lib-calendar', cls: 'hidden'}, lib);
  mk('div', {id: 'lib-places', cls: 'hidden'}, lib);
  const lmapbox = mk('div', {id: 'lib-map-box', cls: 'hidden'}, lib);
  const lmbar = mk('div', {cls: 'toolbar'}, lmapbox);
  mk('button', {id: 'lib-bbox', cls: 'btn', text: 'この範囲の写真を見る'}, lmbar);
  const lperiod = mk('span', {id: 'lib-period', cls: 'lib-period hidden'}, lmbar);
  mk('span', {id: 'lib-period-text'}, lperiod);
  mk('button', {id: 'lib-period-clear', cls: 'btn', text: '期間を解除'}, lperiod);
  mk('span', {cls: 'spacer'}, lmbar);
  mk('span', {id: 'lib-map-note', cls: 'count'}, lmbar);
  mk('div', {id: 'lib-map'}, lmapbox);
  const lgrid = mk('div', {id: 'lib-grid-box', cls: 'hidden'}, lib);
  const lgbar = mk('div', {cls: 'toolbar'}, lgrid);
  mk('button', {id: 'lib-back', cls: 'btn', text: '◀ 戻る'}, lgbar);
  mk('span', {id: 'lib-sel', cls: 'lib-sel'}, lgbar);
  mk('span', {cls: 'spacer'}, lgbar);
  mk('button', {id: 'lib-prev', cls: 'btn', text: '◀ PREV'}, lgbar);
  mk('span', {id: 'lib-pageinfo', cls: 'count', text: '--'}, lgbar);
  mk('button', {id: 'lib-next', cls: 'btn', text: 'NEXT ▶'}, lgbar);
  mk('div', {id: 'lib-grid'}, lgrid);
  mk('div', {id: 'lib-empty', cls: 'empty hidden', text: '該当する写真がありません'}, lgrid);

  // 設定タブ（admin.html の #v-config と同じ id。.scroll の包みは id が無いので省く）
  const cf = mk('section', {id: 'v-config', cls: 'hidden'}, main);
  mk('div', {id: 'c-note', cls: 'cfg-note'}, cf);
  const rtp = mk('div', {cls: 'panel'}, cf);
  mk('div', {id: 'c-runtime', cls: 'grid'}, rtp);
  mk('table', {id: 'c-files', cls: 'cfg'}, rtp);
  mk('table', {id: 'c-ingest', cls: 'cfg'}, mk('div', {cls: 'panel'}, cf));
  mk('table', {id: 'c-geo', cls: 'cfg'}, mk('div', {cls: 'panel'}, cf));
  const aip = mk('div', {cls: 'panel'}, cf);
  mk('table', {id: 'c-ai', cls: 'cfg'}, aip);
  mk('div', {id: 'c-ai-state', cls: 'cfg-note'}, aip);
  mk('table', {id: 'c-ai-tasks', cls: 'cfg'}, aip);
  mk('table', {id: 'c-ai-space', cls: 'cfg'}, aip);
  const otp = mk('div', {id: 'c-other-panel', cls: 'panel hidden'}, cf);
  mk('table', {id: 'c-other', cls: 'cfg'}, otp);
  mk('div', {id: 'c-legend', cls: 'legend'}, mk('div', {cls: 'panel'}, cf));

  const modal = mk('div', {id: 'modal', cls: 'hidden'}, body);
  const box = mk('div', {cls: 'mbox'}, modal);
  const head = mk('div', {cls: 'mhead'}, box);
  mk('span', {id: 'm-title', cls: 's', text: '—'}, head);
  mk('button', {id: 'm-close', cls: 'btn x', text: 'CLOSE'}, head);
  const search = mk('div', {cls: 'msearch'}, box);
  mk('input', {id: 'm-q'}, search);
  mk('button', {id: 'm-search', cls: 'btn', text: '検索'}, search);
  mk('div', {id: 'm-results', cls: 'hidden'}, box);
  mk('div', {id: 'm-suggest', cls: 'hidden'}, box);
  mk('div', {id: 'minimap'}, box);
  const foot = mk('div', {cls: 'mfoot'}, box);
  mk('span', {id: 'm-hint', cls: 'hint'}, foot);
  mk('span', {id: 'm-pick', cls: 'pick'}, foot);
  const save = mk('button', {id: 'm-save', cls: 'btn', text: 'SAVE'}, foot);
  save.disabled = true;

  // ビューア（admin.html の #viewer と同じ id）
  const viewer = mk('div', {id: 'viewer', cls: 'ps-viewer hidden'}, body);
  const shell = mk('section', {cls: 'ps-viewer-shell'}, viewer);
  mk('button', {id: 'viewer-close', cls: 'ps-viewer-close', text: '×'}, shell);
  mk('button', {id: 'viewer-prev', cls: 'ps-viewer-nav ps-viewer-prev', text: '‹'}, shell);
  const media = mk('div', {id: 'viewer-media', cls: 'ps-viewer-media'}, shell);
  mk('img', {id: 'viewer-image'}, media);
  mk('div', {id: 'viewer-image-score', cls: 'ps-viewer-imgscore'}, media);
  const aside = mk('aside', {cls: 'ps-viewer-detail'}, shell);
  const over = mk('div', {cls: 'ps-viewer-overline'}, aside);
  mk('span', {id: 'viewer-counter'}, over);
  mk('span', {id: 'viewer-group-kind'}, over);
  mk('h2', {id: 'viewer-title'}, aside);
  mk('p', {id: 'viewer-meta', cls: 'ps-viewer-meta'}, aside);
  mk('div', {id: 'viewer-location', cls: 'ps-viewer-location'}, aside);
  mk('div', {id: 'viewer-badges', cls: 'tags ps-viewer-badges'}, aside);
  const vai = mk('div', {id: 'viewer-ai'}, aside);
  mk('div', {id: 'viewer-scores', cls: 'ps-viewer-scores'}, vai);
  mk('dl', {id: 'viewer-analysis', cls: 'ps-viewer-analysis'}, vai);
  mk('ul', {id: 'viewer-reasons'}, mk('section', {cls: 'ps-viewer-copy'}, vai));
  mk('ul', {id: 'viewer-concerns'}, mk('section', {cls: 'ps-viewer-copy ps-concern'}, vai));
  const vgroup = mk('section', {id: 'viewer-group', cls: 'ps-viewer-copy'}, aside);
  mk('p', {id: 'viewer-group-summary'}, vgroup);
  const vact = mk('div', {cls: 'ps-viewer-actions'}, aside);
  mk('button', {id: 'viewer-favorite', cls: 'ps-fav', text: '☆ favorite'}, vact);
  mk('button', {id: 'viewer-hidden', cls: 'ps-hid', text: '○ hidden'}, vact);
  mk('button', {id: 'viewer-next', cls: 'ps-viewer-nav ps-viewer-next', text: '›'}, shell);

  mk('div', {id: 'toast'}, body);
  return document;
}

// ---- admin.html とのずれの検査用 ----
// HTML のテキストから id・data-tab・data-filter を取り出す（コメントの中は除く）。
export function htmlFacts(html){
  const text = html.replace(/<!--[\s\S]*?-->/g, '');
  const all = re => [...text.matchAll(re)].map(m => m[1]);
  return {
    ids: all(/\sid="([^"]+)"/g).sort(),
    tabs: all(/\sdata-tab="([^"]+)"/g),
    filters: all(/\sdata-filter="([^"]+)"/g),
  };
}
// 組んだ DOM から同じ事実を取り出す。
export function domFacts(document){
  const els = document.body.descendants();
  return {
    ids: els.filter(e => e.id).map(e => e.id).sort(),
    tabs: els.filter(e => e.dataset.tab !== undefined).map(e => e.dataset.tab),
    filters: els.filter(e => e.dataset.filter !== undefined).map(e => e.dataset.filter),
  };
}
