/* ============================================================
   library.js — 管理ページのライブラリタブ（classic script / 依存なし）
   構成: 1) 状態 2) 画像・タイル 3) 選択と下の詳細 4) 旅のカード 5) 日付（年のカード・月のチップ）・場所（行）
        6) 写真の格子（詳細）7) 地図（MapLibre のクラスタ・吹き出し・範囲）8) 切り替え・初期化
   admin.js（$ el toast）と photos.js（normalizePhoto openModalFor onPhotoChange tileStyle）の後に読み込む。
   定義だけを置き、起動は admin.js の boot() が initLibrary() を呼ぶ形（読み込んだだけでは何にも触らない）。
   サーバーは GET /api/admin/library/{trips,photos,points,calendar,places}（crates/geoframe/src/admin_library.rs）。
   描画は textContent と DOM API だけ（HTML 文字列を流し込む API は使わない）。
   ============================================================ */
'use strict';

/* ==== 1) 状態 ==== */
/* view: 'trips'（旅）| 'calendar'（日付）| 'places'（場所）| 'map'（地図。場所との統合は後の段）。
   sel: view ごとの選択（今の選択は sel[view]。null なら一覧だけ）。選択 S は
        {kind:'trip', key, from, to} | {kind:'year', year, from, to} | {kind:'month', year, month, from, to} | {kind:'untimed'}
        | {kind:'pref', pref} | {kind:'city', pref, city} | {kind:'abroad'} | {kind:'bbox', bbox: [w, s, e, n], period}
   選ぶと一覧を残したまま #lib-detail に S の写真の格子を出す。同じカードをもう一度押すと選択を外す。
   gen: 選択・view・除外が変わるたびに増やす。読み取りを待つ間にこれが変わった応答は捨てる（seq と二重に守る）。
   anchor: Shift の範囲の起点（後の段で使う）。
   years / prefs: calendar・places の応答。openPrefs: 開いている都道府県。loaded: 読み込み済みの一覧
   period: 地図の点と範囲の絞り込みに掛ける期間 {from, to}（旅の「地図で見る」で入り、解除ボタンで外す）。
   pointsStale: 地図が見えていないあいだに設定（除外も含める）が変わったので、地図に戻るとき点を取り直す印。
   seq: 要求の種類ごとの番号。古い応答は捨てる（種類を分けるのは、吹き出しの要求が詳細の読み込みを捨てないため） */
const ls = {
  view: 'trips', includeHidden: false,
  trips: [], untimed: 0,
  years: [], calUntimed: 0, prefs: [], abroad: 0,
  openPrefs: new Set(),
  loaded: {trips: false, calendar: false, places: false},
  sel: {trips: null, calendar: null, places: null, map: null}, gen: 0, anchor: null,
  period: null, pointsStale: false,
  page: 1, pages: 1, per: 60, total: 0, items: [], tiles: [],
  map: null, pendingData: null, labels: new Map(), popup: null,
  seq: {trips: 0, detail: 0, points: 0, popup: 0, calendar: 0, places: 0, when: 0},
};

const LIB_POINTS_SOURCE = 'photos';

function libQuery(obj){
  return Object.keys(obj)
    .filter(k => obj[k] !== undefined && obj[k] !== null && obj[k] !== '' && obj[k] !== false)
    .map(k => encodeURIComponent(k) + '=' + encodeURIComponent(obj[k] === true ? '1' : obj[k]))
    .join('&');
}
/* 応答の写真（favorite を持つ）を、写真タブのビューアが読む形にする */
function libPhoto(raw){ return normalizePhoto(Object.assign({}, raw, {fav: raw.favorite}), 'list'); }
function libGet(path, params){
  const query = libQuery(params);
  return fetch('/api/admin/library/' + path + (query ? '?' + query : ''), {headers: {'Accept': 'application/json'}})
    .then(r => r.json().then(d => d, () => ({ok: false, error: 'HTTP ' + r.status})))
    .then(d => {
      if(!d || !d.ok) throw new Error(d && d.error ? d.error : 'library error');
      return d;
    });
}

/* ==== 2) 画像・タイル ==== */
/* box に画像を入れる。src が空（表示用ファイルも derivative も無い）か読めないときは「画像なし」の代わりの表示 */
function placeholder(){ return el('div', 'img-broken', NO_IMAGE_TEXT); }
function putImage(box, src, alt){
  if(!src){ box.appendChild(placeholder()); return null; }
  const img = document.createElement('img');
  img.loading = 'lazy';
  img.decoding = 'async';
  img.alt = alt || '';
  img.src = src;
  img.addEventListener('error', () => {
    img.remove();
    box.appendChild(placeholder());
  });
  box.appendChild(img);
  return img;
}
/* 印: ★（お気に入り）・🚫（除外）・位置なし。photo の今の値で描き直す */
function paintTile(tile, photo){
  tile.classList.toggle('is-hidden', !!photo.hidden);
  const marks = tile.querySelector('.lib-marks');
  marks.textContent = '';
  if(photo.favorite) marks.appendChild(el('span', 'lib-mark fav', '★'));
  if(photo.hidden) marks.appendChild(el('span', 'lib-mark hid', '🚫'));
  if(!hasLoc(photo)) marks.appendChild(el('span', 'lib-mark noloc', '位置なし'));
  const img = tile.querySelector('img');
  const src = photo.thumb_url || photo.thumb;
  if(img && src && img.src !== src) img.src = src;
}
function tileFor(photo, index){
  const tile = el('div', 'lib-tile');
  tile.dataset.id = photo.id;
  tile.setAttribute('role', 'button');
  tile.setAttribute('tabindex', '0');
  tile.setAttribute('aria-label', photo.name + (photo.taken_at ? '（' + photo.taken_at + '）' : '') + 'を拡大');
  tile.title = photo.name + (photo.taken_at ? '  ' + photo.taken_at : '');
  putImage(tile, photo.thumb_url || photo.thumb, photo.name);
  tile.appendChild(el('div', 'lib-marks'));
  paintTile(tile, photo);
  const open = () => {
    tile.focus();           // 閉じたときにこのタイルへフォーカスを戻す（ブラウザによってはクリックでは移らない）
    openModalFor(ls.items, index, {ai: false});
  };
  tile.addEventListener('click', open);
  tile.addEventListener('keydown', e => {
    if(e.key === 'Enter' || e.key === ' '){ e.preventDefault(); open(); }
  });
  /* 位置を指定（写真タブと同じ位置の指定）。タイルのクリック・キー操作（ビューア）とは混ざらないよう、どちらも止める */
  const loc = el('button', 'lib-loc', '📍');
  loc.type = 'button';
  loc.title = '位置を指定';
  loc.setAttribute('aria-label', photo.name + 'の位置を指定');
  loc.addEventListener('click', e => { e.stopPropagation(); openLocModal(photo); });
  loc.addEventListener('keydown', e => { if(e.key === 'Enter' || e.key === ' ') e.stopPropagation(); });  // Escape は document まで届かせる（位置の指定を閉じる）
  tile.appendChild(loc);
  return tile;
}

/* ==== 3) 選択と下の詳細 ==== */
function placeLabel(p){
  const name = [p.pref, p.city].filter(Boolean).join(' ');
  return (name || '海外・不明') + ' ' + p.count;
}
function rangeLabel(from, to){ return from === to ? from : from + ' 〜 ' + to; }
/* calendar は形（桁）だけで数えるので、月 13 や 31 日 24 時台のような暦に無い値もその年・月に入る。範囲も 00〜99 で形いっぱいに取る（サーバーは from/to を形だけ見て文字列で比べる） */
function yearRange(year){ return {from: year + '-00-00 00:00', to: year + '-99-99 99:99'}; }
function monthRange(year, month){ return {from: year + '-' + month + '-00 00:00', to: year + '-' + month + '-99 99:99'}; }
function tripSel(trip){ return {kind: 'trip', key: trip.key, from: trip.start, to: trip.end}; }
function yearSel(year){ return Object.assign({kind: 'year', year}, yearRange(year)); }
function monthSel(year, month){ return Object.assign({kind: 'month', year, month}, monthRange(year, month)); }

/* 選択の見出しと、同じ選択かどうかの印 */
function selLabel(S){
  switch(S.kind){
    case 'trip': return rangeLabel(S.from, S.to);
    case 'year': return S.year + '年';
    case 'month': return S.year + '年' + S.month + '月';
    case 'untimed': return '日時なし';
    case 'pref': return S.pref;
    case 'city': return S.pref + ' ' + S.city;
    case 'abroad': return '海外・不明';
    case 'bbox': return '地図の範囲' + (S.period ? '・' + rangeLabel(S.period.from, S.period.to) : '');
    default: return '';
  }
}
function selId(S){
  switch(S.kind){
    case 'trip': return 'trip:' + S.key;
    case 'year': return 'year:' + S.year;
    case 'month': return 'month:' + S.year + '-' + S.month;
    case 'bbox': return 'bbox:' + S.bbox.join(',') + (S.period ? '@' + S.period.from + '/' + S.period.to : '');
    case 'pref': return 'pref:' + S.pref;
    case 'city': return 'city:' + S.pref + '/' + S.city;
    default: return S.kind;
  }
}
/* カード・チップ・行の data-key が、選択のどれに当たるか。exact は選んだもの本体、ctx はそれを含む親（月を選んだときの年のカード） */
function selectedKeys(S){
  const exact = new Set(), ctx = new Set();
  if(!S) return {exact, ctx};
  switch(S.kind){
    case 'trip': exact.add(S.key); break;
    case 'year': exact.add(S.year); break;
    case 'month': exact.add(S.year + '-' + S.month); ctx.add(S.year); break;
    case 'untimed': exact.add('untimed'); break;
    case 'pref': exact.add(S.pref); break;
    case 'city': exact.add(S.pref + '/' + S.city); ctx.add(S.pref); break;
    case 'abroad': exact.add('abroad'); break;
    default: break;
  }
  return {exact, ctx};
}
const LIB_BOXES = {trips: '#lib-trips', calendar: '#lib-calendar', places: '#lib-places'};
const LIB_CARD_ROWS = {trips: '#lib-trip-cards', calendar: '#lib-year-cards'};
/* 選択の見た目だけを合わせる（一覧は作り直さない。押したボタンのフォーカスを失わないため） */
function paintBox(view){
  const S = ls.sel[view];
  const keys = selectedKeys(S);
  $(LIB_BOXES[view]).querySelectorAll('[data-key]').forEach(node => {
    const key = node.dataset.key;
    const exact = keys.exact.has(key);
    node.classList.toggle('is-selected', exact || keys.ctx.has(key));
    const press = node.classList.contains('lib-card') ? node.querySelector('.lib-card-main')
      : (node.tagName === 'BUTTON' ? node : null);
    if(press) press.setAttribute('aria-pressed', exact ? 'true' : 'false');
  });
  const row = LIB_CARD_ROWS[view];
  if(row) $(row).classList.toggle('is-strip', !!S);       // 選んだあいだは 1 段の横スクロール
}
function paintSelection(){
  renderMonths(false);
  Object.keys(LIB_BOXES).forEach(paintBox);
}
/* 選んだカードを見える所へ（1 段の横スクロールの中でも）。場所・地図の画面は詳細へ */
function scrollSelected(){
  const S = ls.sel[ls.view];
  if(!S) return;
  const row = LIB_CARD_ROWS[ls.view];
  const node = row ? $(row).querySelector('.lib-card.is-selected') : $('#lib-detail');
  if(node && node.scrollIntoView) node.scrollIntoView({block: 'nearest', inline: 'nearest'});
}
/* 選択を S にして（null なら外して）詳細を読む。古い詳細の応答は gen と seq で捨てる */
function applySel(S){
  ls.sel[ls.view] = S;
  ls.gen++;
  ls.seq.detail++;
  ls.page = 1;
  clearDetail();
  showLib();
  scrollSelected();
  if(S) loadDetail();
}
/* カード・チップは aria-pressed のボタン。同じものをもう一度押すと外す */
function toggleSel(S){
  const cur = ls.sel[ls.view];
  applySel(cur && selId(cur) === selId(S) ? null : S);
}

/* ==== 4) 旅のカード ==== */
/* カードは非対話の容器。主ボタン（表紙・名前・枚数）が選択を受け、「地図で見る」は兄弟のボタン（ボタンを入れ子にしない）。
   Enter・Space は button が click を起こすので、click だけを聞く */
function cardMain(card, onPress){
  const main = el('button', 'lib-card-main');
  main.type = 'button';
  main.setAttribute('aria-pressed', 'false');
  main.addEventListener('click', onPress);
  card.appendChild(main);
  return main;
}
function tripCard(trip){
  const card = el('div', 'lib-card');
  card.dataset.key = trip.key;
  const main = cardMain(card, () => toggleSel(tripSel(trip)));
  const cover = el('span', 'lib-cover');
  putImage(cover, trip.cover ? (trip.cover.thumb_url || '') : '', '');
  main.appendChild(cover);
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', rangeLabel(trip.start, trip.end)));
  body.appendChild(el('span', 'lib-card-count', trip.count + ' 枚（位置なし ' + trip.noloc + '）'));
  const tags = el('span', 'tags');
  (trip.places || []).forEach(p => tags.appendChild(el('span', 'tag t-subject', placeLabel(p))));
  body.appendChild(tags);
  main.appendChild(body);
  const map = el('button', 'btn lib-card-map', '地図で見る');
  map.type = 'button';
  map.addEventListener('click', () => mapForTrip(trip));
  card.appendChild(map);
  return card;
}
/* 表紙の無いカード（日時なし）。押すと untimed の写真 */
function textCard(key, name, count){
  const card = el('div', 'lib-card lib-card-text');
  card.dataset.key = key;
  const main = cardMain(card, () => toggleSel({kind: 'untimed'}));
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', name));
  body.appendChild(el('span', 'lib-card-count', count + ' 枚'));
  main.appendChild(body);
  return card;
}
/* 上の選択欄の右の件数（view ごと。読み込み前は空） */
function updateLibNote(){
  let text = '';
  if(ls.view === 'trips' && ls.loaded.trips) text = '旅 ' + ls.trips.length + ' 件';
  else if(ls.view === 'calendar' && ls.loaded.calendar) text = ls.years.length + ' 年';
  else if(ls.view === 'places' && ls.loaded.places) text = ls.prefs.length + ' 都道府県';
  $('#lib-note').textContent = text;
}
function renderTrips(){
  const box = $('#lib-trip-cards');
  box.textContent = '';
  ls.trips.forEach(t => box.appendChild(tripCard(t)));
  if(ls.untimed > 0) box.appendChild(textCard('untimed', '日時なし', ls.untimed));
  if(!ls.trips.length && !ls.untimed) box.appendChild(el('div', 'empty', '写真がありません'));
  paintBox('trips');
  updateLibNote();
}
function loadTrips(){
  const seq = ++ls.seq.trips;
  libGet('trips', {hidden: ls.includeHidden}).then(d => {
    if(seq !== ls.seq.trips) return;
    ls.trips = d.trips || [];
    ls.untimed = d.untimed || 0;
    ls.loaded.trips = true;
    renderTrips();
  }).catch(e => { if(seq === ls.seq.trips) toast('旅の一覧を読めませんでした: ' + e.message, true); });
}
/* 「地図で見る」は新しい期間で地図を見直す。以前の地図の範囲選択（別の旅の期間を持つ bbox）は持ち越さず解除し、
   飛んでいる詳細の応答も捨てる（setLibView が復元する sel.map を空にしてから切り替える） */
function mapForTrip(trip){
  ls.period = {from: trip.start, to: trip.end};
  ls.sel.map = null;
  ls.gen++;
  ls.seq.detail++;
  setLibView('map');
}

/* ==== 5) 日付（年のカード・月のチップ）・場所（行） ==== */
function yearCard(y){
  const card = el('div', 'lib-card');
  card.dataset.key = y.year;
  const main = cardMain(card, () => toggleSel(yearSel(y.year)));
  const cover = el('span', 'lib-cover');
  putImage(cover, y.cover ? (y.cover.thumb_url || '') : '', '');
  main.appendChild(cover);
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', y.year + '年'));
  body.appendChild(el('span', 'lib-card-count', y.count + ' 枚'));
  main.appendChild(body);
  return card;
}
/* 選んだ年（年・月のどちらを選んでいても）の月のチップ。1〜12 月を並べ、応答に無い月は押せない。
   選択のたびには作り直さない（rebuild か、年が変わったときだけ）。フォーカスを失わないため */
function renderMonths(rebuild){
  const box = $('#lib-months');
  const S = ls.sel.calendar;
  const year = S && (S.kind === 'year' || S.kind === 'month') ? S.year : null;
  const y = year ? ls.years.find(x => x.year === year) : null;
  box.classList.toggle('hidden', !y);
  if(!y){ box.textContent = ''; delete box.dataset.year; return; }
  if(!rebuild && box.dataset.year === year) return;
  box.dataset.year = year;
  box.textContent = '';
  const counts = new Map((y.months || []).map(m => [m.month, m.count]));
  const names = [];
  for(let i = 1; i <= 12; i++) names.push(String(i).padStart(2, '0'));
  counts.forEach((_, m) => { if(!names.includes(m)) names.push(m); });   // 暦に無い月（13 など）も形だけで数えられている
  names.sort();
  names.forEach(m => {
    const chip = el('button', 'chip lib-month', m + '月');
    chip.type = 'button';
    chip.dataset.key = year + '-' + m;
    chip.setAttribute('aria-pressed', 'false');
    if(counts.has(m)) chip.appendChild(el('span', 'n', String(counts.get(m))));
    else chip.disabled = true;
    chip.addEventListener('click', () => toggleSel(monthSel(year, m)));
    box.appendChild(chip);
  });
}
function renderCalendar(){
  const box = $('#lib-year-cards');
  box.textContent = '';
  ls.years.forEach(y => box.appendChild(yearCard(y)));
  if(ls.calUntimed > 0) box.appendChild(textCard('untimed', '日時なし', ls.calUntimed));
  if(!ls.years.length && !ls.calUntimed) box.appendChild(el('div', 'empty', '写真がありません'));
  renderMonths(true);
  paintBox('calendar');
  updateLibNote();
}
function loadCalendar(){
  const seq = ++ls.seq.calendar;
  libGet('calendar', {hidden: ls.includeHidden}).then(d => {
    if(seq !== ls.seq.calendar) return;
    ls.years = d.years || [];
    ls.calUntimed = d.untimed || 0;
    ls.loaded.calendar = true;
    renderCalendar();
  }).catch(e => { if(seq === ls.seq.calendar) toast('日付の一覧を読めませんでした: ' + e.message, true); });
}
/* 場所は今の行の作り（後の段でカードと地図に統合する）。年・都道府県の行（押すと市区町村の行を開閉）と、「写真を見る」 */
function groupRow(title, count, opts){
  const row = el('div', 'lib-grow' + (opts.sub ? ' lib-sub' : ''));
  row.dataset.key = opts.key;
  if(opts.onToggle){
    const toggle = el('button', 'lib-expand');
    toggle.type = 'button';
    toggle.setAttribute('aria-expanded', opts.expanded ? 'true' : 'false');
    toggle.appendChild(el('span', 'lib-caret', opts.expanded ? '▾' : '▸'));
    toggle.appendChild(el('span', 'lib-gname', title));
    toggle.addEventListener('click', opts.onToggle);
    row.appendChild(toggle);
  }else{
    row.appendChild(el('span', 'lib-gname', title));
  }
  row.appendChild(el('span', 'lib-gcount', count + ' 枚'));
  if(opts.onShow){
    const btn = el('button', 'btn lib-gphotos', '写真を見る');
    btn.type = 'button';
    btn.addEventListener('click', opts.onShow);
    row.appendChild(btn);
  }
  return row;
}
function summaryRow(key, text, onShow){
  const row = el('div', 'lib-summary');
  row.dataset.key = key;
  row.appendChild(el('span', 'lib-summary-text', text));
  const btn = el('button', 'btn lib-summary-show', '写真を見る');
  btn.type = 'button';
  btn.addEventListener('click', onShow);
  row.appendChild(btn);
  return row;
}
function renderPlaces(){
  const box = $('#lib-places');
  box.textContent = '';
  ls.prefs.forEach(p => {
    const open = ls.openPrefs.has(p.pref);
    const group = el('div', 'lib-group');
    group.appendChild(groupRow(p.pref, p.count, {
      key: p.pref, expanded: open,
      onToggle: () => { if(open) ls.openPrefs.delete(p.pref); else ls.openPrefs.add(p.pref); renderPlaces(); },
      onShow: () => applySel({kind: 'pref', pref: p.pref}),
    }));
    if(open){
      (p.cities || []).forEach(c => {
        // 市区町村が空の項目は、サーバーが空の city を絞り込みに使えない（空の値は捨てられる）ので、件数だけを出す
        group.appendChild(groupRow(c.city || '（市区町村なし）', c.count, {
          key: p.pref + '/' + c.city, sub: true,
          onShow: c.city ? () => applySel({kind: 'city', pref: p.pref, city: c.city}) : null,
        }));
      });
    }
    box.appendChild(group);
  });
  if(ls.abroad > 0){
    box.appendChild(summaryRow('abroad', '海外・不明（' + ls.abroad + ' 枚）', () => applySel({kind: 'abroad'})));
  }
  if(!ls.prefs.length && !ls.abroad) box.appendChild(el('div', 'empty', '写真がありません'));
  paintBox('places');
  updateLibNote();
}
function loadPlaces(){
  const seq = ++ls.seq.places;
  libGet('places', {hidden: ls.includeHidden}).then(d => {
    if(seq !== ls.seq.places) return;
    ls.prefs = d.prefs || [];
    ls.abroad = d.abroad || 0;
    ls.loaded.places = true;
    renderPlaces();
  }).catch(e => { if(seq === ls.seq.places) toast('場所の一覧を読めませんでした: ' + e.message, true); });
}
/* view の一覧を読む（地図は点を別に読む） */
function loadLibList(view){
  if(view === 'trips') loadTrips();
  else if(view === 'calendar') loadCalendar();
  else if(view === 'places') loadPlaces();
}

/* ==== 6) 詳細（写真の格子・ページ） ==== */
function detailParams(S){
  const p = {page: ls.page, per: ls.per, hidden: ls.includeHidden};
  switch(S.kind){
    case 'untimed': p.untimed = true; break;
    case 'abroad': p.abroad = true; break;
    case 'pref': p.pref = S.pref; break;
    case 'city': p.pref = S.pref; p.city = S.city; break;
    case 'bbox':
      p.bbox = S.bbox.join(',');
      if(S.period){ p.from = S.period.from; p.to = S.period.to; }
      break;
    default: p.from = S.from; p.to = S.to;      // trip・year・month
  }
  return p;
}
function renderGrid(){
  const box = $('#lib-grid');
  box.textContent = '';
  ls.tiles = ls.items.map((photo, i) => tileFor(photo, i));
  ls.tiles.forEach(t => box.appendChild(t));
  $('#lib-empty').classList.toggle('hidden', ls.items.length > 0);
  $('#lib-pageinfo').textContent = ls.total + '件 / ' + ls.page + ' of ' + ls.pages + 'ページ';
  $('#lib-prev').disabled = ls.page <= 1;
  $('#lib-next').disabled = ls.page >= ls.pages;
}
/* 選択が変わったとき、前の選択の写真を残さない */
function clearDetail(){
  ls.items = [];
  ls.tiles = [];
  ls.total = 0;
  ls.pages = 1;
  $('#lib-grid').textContent = '';
  $('#lib-empty').classList.add('hidden');
  $('#lib-pageinfo').textContent = '--';
  $('#lib-prev').disabled = true;
  $('#lib-next').disabled = true;
}
function loadDetail(){
  const S = ls.sel[ls.view];
  if(!S) return;
  const seq = ++ls.seq.detail, gen = ls.gen;
  libGet('photos', detailParams(S)).then(d => {
    if(seq !== ls.seq.detail || gen !== ls.gen) return;
    ls.page = d.page; ls.pages = d.pages; ls.total = d.total;
    ls.items = (d.items || []).map(libPhoto);
    renderGrid();
  }).catch(e => { if(seq === ls.seq.detail && gen === ls.gen) toast('写真を読めませんでした: ' + e.message, true); });
}
/* ★・除外・位置などの変更（photos.js の applyPhoto から）。渡された配列とは別の object（吹き出しの 1 枚など）で
   変えたときも、自分の格子の同じ写真に同じ値を入れてタイルを描き直す */
function libPhotoChanged(id, patch){
  ls.items.forEach((photo, i) => {
    if(String(photo.id) !== String(id)) return;
    Object.assign(photo, patch);
    if(ls.tiles[i]) paintTile(ls.tiles[i], photo);
  });

  // 位置・除外が変わると地図の点が変わる（★は点に出ない）。飛んでいる点の応答は捨て、地図が見えていれば取り直し、
  // 見えていない（地図以外の view、別のタブ）なら印だけ立てて、地図に戻るとき（setLibView・openLibraryTab）に読む
  if(patch && ('lat' in patch || 'lon' in patch || 'hidden' in patch)){
    if(mapVisible()) loadPoints();
    else{ ls.seq.points++; ls.pointsStale = true; }
  }
}
/* 地図が見えているか: 地図の view で、ライブラリのタブが見えている */
function mapVisible(){
  return ls.view === 'map' && !$('#v-library').classList.contains('hidden');
}

/* ==== 7) 地図 ==== */
const LIB_CLUSTER_COLOR = ['step', ['get', 'point_count'], '#00b8d4', 100, '#ffb300', 1000, '#ff2d95'];
const LIB_CLUSTER_RADIUS = ['step', ['get', 'point_count'], 14, 100, 19, 1000, 25];
const EMPTY_POINTS = {type: 'FeatureCollection', features: []};

function addPointLayers(map){
  map.addSource(LIB_POINTS_SOURCE, {
    type: 'geojson', data: ls.pendingData || EMPTY_POINTS,
    cluster: true, clusterRadius: 40, clusterMaxZoom: 16,
  });
  map.addLayer({id: 'lib-clusters', type: 'circle', source: LIB_POINTS_SOURCE, filter: ['has', 'point_count'],
    paint: {'circle-color': LIB_CLUSTER_COLOR, 'circle-radius': LIB_CLUSTER_RADIUS, 'circle-opacity': 0.85,
            'circle-stroke-width': 2, 'circle-stroke-color': '#05080f'}});
  map.addLayer({id: 'lib-point', type: 'circle', source: LIB_POINTS_SOURCE, filter: ['!', ['has', 'point_count']],
    paint: {'circle-color': '#00e5ff', 'circle-radius': 6, 'circle-stroke-width': 2,
            'circle-stroke-color': '#05080f'}});
}
/* クラスタの件数の文字。style に glyphs（文字の形のデータ）が無く symbol 層は使えないので、クラスタごとの DOM の
   マーカー（クリックは通す）を今見えているクラスタに合わせる。毎フレームの render では呼ばず、
   動き・ズームの終わり（moveend・zoomend）、描画が落ち着いたとき（idle）、点の source の読み込み完了（sourcedata）、setData の直後に呼ぶ */
function syncClusterLabels(){
  const map = ls.map;
  if(!map || !map.getSource(LIB_POINTS_SOURCE) || !map.isSourceLoaded(LIB_POINTS_SOURCE)) return;
  const seen = new Set();
  map.querySourceFeatures(LIB_POINTS_SOURCE, {filter: ['has', 'point_count']}).forEach(f => {
    const id = f.properties.cluster_id;
    if(seen.has(id)) return;
    seen.add(id);
    if(ls.labels.has(id)) return;
    const node = el('div', 'lib-cluster-n', String(f.properties.point_count_abbreviated || f.properties.point_count));
    ls.labels.set(id, new maplibregl.Marker({element: node}).setLngLat(f.geometry.coordinates).addTo(map));
  });
  ls.labels.forEach((marker, id) => {
    if(!seen.has(id)){ marker.remove(); ls.labels.delete(id); }
  });
}
const LIB_MAX_LNG = 179.999;
function ensureLibMap(){
  if(ls.map) return ls.map;
  if(typeof maplibregl === 'undefined'){ toast('地図を読み込めませんでした', true); return null; }
  const map = new maplibregl.Map({
    container: 'lib-map', style: tileStyle(),
    center: [137, 36], zoom: 4, attributionControl: {compact: true},
    // 世界を横に周回させない（経度が ±180 を超えると表示範囲を bbox にできない。日付変更線をまたぐ範囲は扱わない）。
    // MapLibre 4.7.1 は maxBounds の経度がちょうど ±180 だと初期カメラがズーム 22・経度 180 に潰れるので、わずかに内側にする。
    // bbox は currentBbox が ±180 に丸める。
    renderWorldCopies: false, maxBounds: [[-LIB_MAX_LNG, -85], [LIB_MAX_LNG, 85]],
  });
  ls.map = map;
  map.addControl(new maplibregl.NavigationControl({showCompass: false}), 'top-right');
  map.on('load', () => addPointLayers(map));
  map.on('moveend', syncClusterLabels);
  map.on('zoomend', syncClusterLabels);
  // moveend・zoomend は描画での source の更新より先に発火し、更新先のタイルがキャッシュにあると sourcedata も出ないので、描画が落ち着いた後にも 1 回合わせる
  map.on('idle', syncClusterLabels);
  map.on('sourcedata', e => {
    if(e.sourceId === LIB_POINTS_SOURCE && e.isSourceLoaded) syncClusterLabels();
  });
  map.on('click', 'lib-clusters', e => {
    const f = e.features && e.features[0];
    if(!f) return;
    map.getSource(LIB_POINTS_SOURCE).getClusterExpansionZoom(f.properties.cluster_id).then(zoom => {
      map.easeTo({center: f.geometry.coordinates, zoom});
    }).catch(() => {});
  });
  map.on('click', 'lib-point', e => {
    const f = e.features && e.features[0];
    if(f) openPointPopup(f.properties.id, f.geometry.coordinates.slice());
  });
  ['lib-clusters', 'lib-point'].forEach(layer => {
    map.on('mouseenter', layer, () => { map.getCanvas().style.cursor = 'pointer'; });
    map.on('mouseleave', layer, () => { map.getCanvas().style.cursor = ''; });
  });
  return map;
}
function pointsParams(){
  const p = {hidden: ls.includeHidden};
  if(ls.period){ p.from = ls.period.from; p.to = ls.period.to; }
  return p;
}
function clearClusterLabels(){
  ls.labels.forEach(marker => marker.remove());
  ls.labels.clear();
}
function setPoints(collection){
  clearClusterLabels();        // クラスタの番号は読み直すと振り直されるので、古い文字は全部外す
  const src = ls.map && ls.map.getSource(LIB_POINTS_SOURCE);
  if(src){ src.setData(collection); syncClusterLabels(); }
  else ls.pendingData = collection;      // 地図の style がまだ → load で source を作るときに入れる
}
function loadPoints(){
  ensureLibMap();
  ls.pointsStale = false;
  const seq = ++ls.seq.points;
  libGet('points', pointsParams()).then(d => {
    if(seq !== ls.seq.points) return;
    setPoints({type: 'FeatureCollection', features: d.features || []});
    const n = (d.features || []).length;
    $('#lib-map-note').textContent = d.truncated
      ? n + ' 点（全 ' + d.total + ' 点のうち新しい順）'
      : n + ' 点';
  }).catch(e => { if(seq === ls.seq.points) toast('地図の点を読めませんでした: ' + e.message, true); });
}
function closePopup(){
  if(ls.popup){ ls.popup.remove(); ls.popup = null; }
}
/* 点を押した時点で 1 枚を引く（点の中身は id だけ）。別の点を押したら古い応答は捨てる */
function openPointPopup(id, lngLat){
  const seq = ++ls.seq.popup;
  closePopup();
  const body = el('div', 'lib-pop', '読み込み中');
  ls.popup = new maplibregl.Popup({closeButton: true, maxWidth: '260px'})
    .setLngLat(lngLat).setDOMContent(body).addTo(ls.map);
  libGet('photos', {ids: id, hidden: ls.includeHidden}).then(d => {
    if(seq !== ls.seq.popup) return;
    const raw = (d.items || [])[0];
    if(!raw) throw new Error('not found');
    const photo = libPhoto(raw);
    body.textContent = '';
    const thumb = el('div', 'lib-pop-thumb');
    putImage(thumb, photo.thumb_url || photo.thumb, photo.name);
    body.appendChild(thumb);
    body.appendChild(el('div', 'lib-pop-dt', photo.taken_at || '撮影時刻なし'));
    body.appendChild(el('div', 'lib-pop-place', l1Of(photo).text));
    const open = el('button', 'btn lib-pop-open', '開く');
    open.type = 'button';
    open.addEventListener('click', () => openModalFor([photo], 0, {ai: false}));
    body.appendChild(open);
  }).catch(() => { if(seq === ls.seq.popup) body.textContent = '読めませんでした'; });
}
/* 今の表示範囲を [w, s, e, n] に（lon は ±180・lat は ±90 に丸める。サーバーは範囲外を 400 にする）。
   地図は周回しない設定だが、丸めた後に w >= e（190〜210 が 180〜180 に潰れる等。日付変更線をまたぐ範囲は扱わない）なら経度は全域にする */
function clampTo(v, limit){ return Number(Math.max(-limit, Math.min(limit, v)).toFixed(6)); }
function currentBbox(){
  const b = ls.map.getBounds();
  const w = clampTo(b.getWest(), 180), s = clampTo(b.getSouth(), 90);
  const e = clampTo(b.getEast(), 180), n = clampTo(b.getNorth(), 90);
  return w >= e ? [-180, s, 180, n] : [w, s, e, n];
}
function viewBbox(){
  if(!ls.map) return;
  applySel({kind: 'bbox', bbox: currentBbox(), period: ls.period ? {from: ls.period.from, to: ls.period.to} : null});
}

/* ==== 8) 切り替え・初期化 ==== */
function showLib(){
  const view = ls.view;
  const trips = view === 'trips', map = view === 'map';
  const calendar = view === 'calendar', places = view === 'places';
  $('#lib-trips').classList.toggle('hidden', !trips);
  $('#lib-calendar').classList.toggle('hidden', !calendar);
  $('#lib-places').classList.toggle('hidden', !places);
  $('#lib-map-box').classList.toggle('hidden', !map);
  const S = ls.sel[view];
  $('#lib-detail').classList.toggle('hidden', !S);
  $('#lib-sel').textContent = S ? selLabel(S) : '';
  $('#lib-view-trips').classList.toggle('on', trips);
  $('#lib-view-calendar').classList.toggle('on', calendar);
  $('#lib-view-places').classList.toggle('on', places);
  $('#lib-view-map').classList.toggle('on', map);
  updateLibNote();
  $('#lib-period').classList.toggle('hidden', !ls.period);
  $('#lib-period-text').textContent = ls.period ? '期間: ' + rangeLabel(ls.period.from, ls.period.to) : '';
  paintSelection();
  if(map){
    const m = ensureLibMap();
    if(m) m.resize();           // 隠れているあいだに寸法が変わった（タブ・ビューの切り替え）ので合わせ直す
  }else{
    closePopup();
  }
}
/* view を切り替える。view ごとの選択は残り、その view の詳細を読み直す（ページ 1）。飛んでいる詳細の応答は捨てる */
function setLibView(view){
  const changed = view !== ls.view;
  if(changed){
    ls.view = view;
    ls.gen++;
    ls.seq.detail++;
    ls.page = 1;
    clearDetail();
  }
  showLib();
  if(view === 'map') loadPoints();
  else if(view !== 'trips' || !ls.loaded.trips) loadLibList(view);
  if(changed && ls.sel[view]) loadDetail();
}
/* ライブラリタブを開いたとき（admin.js の showTab から）。旅は毎回読み直す。選んでいる view の一覧・詳細・地図の点も */
function openLibraryTab(){
  loadTrips();
  if(ls.view !== 'trips') loadLibList(ls.view);
  showLib();
  if(ls.view === 'map') loadPoints();
  if(ls.sel[ls.view]){ ls.page = 1; clearDetail(); loadDetail(); }
}
/* 除外も含める: すべての種類の飛んでいる応答を捨て、見えている view の一覧・地図の点・詳細を hidden を変えて読み直す（詳細はページ 1）。
   見えていない view の一覧は loaded を落とすだけ（開くときに読む）。選んでいたものが応答に無くなっても選択は残す */
function onLibHiddenChange(){
  ls.includeHidden = !!$('#lib-hidden').checked;
  ls.gen++;
  Object.keys(ls.seq).forEach(k => { ls.seq[k]++; });
  closePopup();
  Object.keys(ls.loaded).forEach(k => { ls.loaded[k] = false; });
  loadLibList(ls.view);
  if(ls.view === 'map') loadPoints();
  else ls.pointsStale = true;
  if(ls.sel[ls.view]){ ls.page = 1; loadDetail(); }
}
function initLibrary(){
  $('#lib-view-trips').addEventListener('click', () => setLibView('trips'));
  $('#lib-view-calendar').addEventListener('click', () => setLibView('calendar'));
  $('#lib-view-places').addEventListener('click', () => setLibView('places'));
  $('#lib-view-map').addEventListener('click', () => setLibView('map'));
  $('#lib-hidden').addEventListener('change', onLibHiddenChange);
  $('#lib-prev').addEventListener('click', () => { if(ls.page > 1){ ls.page--; loadDetail(); } });
  $('#lib-next').addEventListener('click', () => { if(ls.page < ls.pages){ ls.page++; loadDetail(); } });
  $('#lib-bbox').addEventListener('click', viewBbox);
  $('#lib-period-clear').addEventListener('click', () => { ls.period = null; showLib(); loadPoints(); });
  onPhotoChange(libPhotoChanged);
}
