/* ============================================================
   library.js — 管理ページのライブラリタブ（classic script / 依存なし）
   構成: 1) 状態 2) 画像・タイル 3) 選択と下の詳細 4) 旅のカード 5) 日付（年のカード・月のチップ）
        6) 場所（都道府県のカード・市区町村のチップ・期間）7) 写真の格子（詳細）8) 地図（MapLibre のクラスタ・吹き出し・範囲・移動の理由）9) 切り替え・初期化
   admin.js（$ el toast）と photos.js（normalizePhoto openModalFor onPhotoChange tileStyle）の後に読み込む。
   定義だけを置き、起動は admin.js の boot() が initLibrary() を呼ぶ形（読み込んだだけでは何にも触らない）。
   サーバーは GET /api/admin/library/{trips,photos,points,calendar,places}（crates/geoframe/src/admin_library.rs）。
   描画は textContent と DOM API だけ（HTML 文字列を流し込む API は使わない）。
   ============================================================ */
'use strict';

/* ==== 1) 状態 ==== */
/* view: 'trips'（旅）| 'calendar'（日付）| 'places'（場所。都道府県のカード・地図・写真の詳細を 1 つの画面に置く）。
   sel: view ごとの選択（今の選択は sel[view]。null なら一覧だけ）。選択 S は
        {kind:'trip', key, from, to} | {kind:'year', year, from, to} | {kind:'month', year, month, from, to} | {kind:'untimed'}
        | {kind:'pref', pref} | {kind:'city', pref, city} | {kind:'abroad'} | {kind:'bbox', bbox: [w, s, e, n]} | {kind:'period'}（場所の画面で、期間の写真全体）
        | {kind:'span', from, to}（Shift の範囲。形の範囲）| {kind:'words', from, to, source}（日付の言葉。暦の範囲）| {kind:'q', q}（旅の文字検索）
   選ぶと一覧を残したまま #lib-detail に S の写真の格子を出す。同じカードをもう一度押すと選択を外す。
   gen: 選択・view・除外が変わるたびに増やす。読み取りを待つ間にこれが変わった応答は捨てる（seq と二重に守る）。
   anchor: Shift の範囲の起点 {from, to, year}（直前に普通に選んだ年・月の形の範囲と、月のチップを出す年）。年・月・範囲の選択以外になると null。
   tripFilter: 旅のカードを絞る検索欄の文字。whenMsg: 日付の言葉の読み込み中・失敗の文 {gen, text}（その gen のあいだだけ出す）。
   combo: 場所のコンボボックスの状態 {open, items, more, active}（items は選べる候補、more は表に入りきらなかった件数、active は強調している添字）。
   years: calendar の応答。
   prefs / abroad: 都道府県のカード用の /places の応答（period・hidden・mapBbox を掛ける）。placeOpts: コンボボックスの候補用の /places の応答
   （period・hidden だけ。mapBbox は掛けない。候補の UI は後の段）。保存先・seq（placeCards・placeOptions）・loaded を別に持つ。
   loaded: 読み込み済みの一覧
   period: 場所の画面に掛かる期間 {from, to}（旅の「地図で見る」で入り、解除ボタンで外す）。カード・候補・点・詳細のすべてに掛かる。
   mapBbox: 利用者が地図を動かしたときのカードの絞り込み [w, s, e, n]。null なら絞らない。
   fitPoints: 「地図で見る」のあと、点を読めたら点の範囲へ地図を動かす印。
   onPlaces・detailShown・resizeQueued: 地図の大きさを合わせ直す時点を決める印（場所の view を出した・詳細の出し入れ・合わせ直しの予約中）。
   pointsStale: 地図が見えていないあいだに設定（除外も含める）が変わったので、地図に戻るとき点を取り直す印。
   seq: 要求の種類ごとの番号。古い応答は捨てる（種類を分けるのは、吹き出しの要求が詳細の読み込みを捨てないため）。
        cluster はクラスタを押したあとの展開ズームの応答待ち。応答のとき seq.cluster と gen の両方が押した時点のままでなければ地図を動かさない
        （カード・候補・全体に戻す・利用者の地図の移動・点の読み直しは seq.cluster を進め、選択・view・期間・除外の変更は gen を進める） */
const ls = {
  view: 'trips', includeHidden: false,
  trips: [], untimed: 0,
  years: [], calUntimed: 0, prefs: [], abroad: 0, placeOpts: {prefs: [], abroad: 0},
  loaded: {trips: false, calendar: false, placeCards: false, placeOptions: false},
  sel: {trips: null, calendar: null, places: null}, gen: 0, anchor: null,
  tripFilter: '', whenMsg: null, combo: {open: false, items: [], more: 0, active: -1},
  period: null, mapBbox: null, fitPoints: false, pointsStale: false,
  onPlaces: false, detailShown: false, resizeQueued: false,
  page: 1, pages: 1, per: 60, total: 0, items: [], tiles: [],
  map: null, pendingData: null, labels: new Map(), popup: null,
  seq: {trips: 0, detail: 0, points: 0, popup: 0, calendar: 0, placeCards: 0, placeOptions: 0, when: 0, choose: 0, cluster: 0},
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
/* 形の範囲（00〜99）の端の表示: 「2024年8月」「2024年」。from は始まり側、to は終わり側（月が 99 なら年全体） */
function shapeLabel(text, isEnd){
  const year = text.slice(0, 4), month = text.slice(5, 7);
  const none = isEnd ? '99' : '00';
  return month === none ? year + '年' : year + '年' + Number(month) + '月';
}
function spanLabel(from, to){
  const a = shapeLabel(from, false), b = shapeLabel(to, true);
  return a === b ? a : a + '〜' + b;
}
/* 暦の範囲の日付の部分（「2025-06-01〜2025-08-31」。同じ日なら 1 つ） */
function daySpan(from, to){
  const a = from.slice(0, 10), b = to.slice(0, 10);
  return a === b ? a : a + '〜' + b;
}

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
    case 'bbox': return '地図の範囲' + (ls.period ? '・' + rangeLabel(ls.period.from, ls.period.to) : '');
    case 'period': return ls.period ? '期間 ' + rangeLabel(ls.period.from, ls.period.to) : '';
    case 'span': return spanLabel(S.from, S.to);
    case 'words': return daySpan(S.from, S.to);
    case 'q': return '“' + S.q + '” の検索';
    default: return '';
  }
}
function selId(S){
  switch(S.kind){
    case 'trip': return 'trip:' + S.key;
    case 'year': return 'year:' + S.year;
    case 'month': return 'month:' + S.year + '-' + S.month;
    case 'bbox': return 'bbox:' + S.bbox.join(',');
    case 'span': return 'span:' + S.from + '~' + S.to;
    case 'words': return 'words:' + S.from + '~' + S.to;
    case 'q': return 'q:' + S.q;
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
  paintCities();
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
  if(ls.view === 'calendar' && !(S && (S.kind === 'year' || S.kind === 'month' || S.kind === 'span'))) ls.anchor = null;
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
/* 表紙の無いカード（日時なし・海外不明）。押すと sel の写真 */
function textCard(key, name, count, onPress){
  const card = el('div', 'lib-card lib-card-text');
  card.dataset.key = key;
  const main = cardMain(card, onPress);
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', name));
  body.appendChild(el('span', 'lib-card-count', count + ' 枚'));
  main.appendChild(body);
  return card;
}
/* 上の選択欄の右の件数（view ごと。読み込み前は空） */
function updateLibNote(){
  let text = '';
  if(ls.view === 'trips' && ls.loaded.trips){
    const shown = visibleTrips().length;
    text = ls.tripFilter.trim() ? '旅 ' + shown + ' / ' + ls.trips.length + ' 件' : '旅 ' + ls.trips.length + ' 件';
  }
  else if(ls.view === 'calendar' && ls.loaded.calendar) text = ls.years.length + ' 年';
  else if(ls.view === 'places' && ls.loaded.placeCards) text = (ls.mapBbox ? '地図の範囲: ' : '') + ls.prefs.length + ' 都道府県';
  $('#lib-note').textContent = text;
}
/* 空白（全角も）で分けた小文字の語 */
function libWords(text){ return text.toLowerCase().split(/[\s\u3000]+/).filter(Boolean); }
/* 検索欄の文字で絞った旅（rangeLabel と placeLabel の文字に、語のすべてが部分一致するもの） */
function visibleTrips(){
  const words = libWords(ls.tripFilter);
  if(!words.length) return ls.trips;
  return ls.trips.filter(t => {
    const text = (rangeLabel(t.start, t.end) + ' ' + (t.places || []).map(placeLabel).join(' ')).toLowerCase();
    return words.every(w => text.includes(w));
  });
}
function renderTrips(){
  const box = $('#lib-trip-cards');
  box.textContent = '';
  const filtering = libWords(ls.tripFilter).length > 0;
  const trips = visibleTrips();
  trips.forEach(t => box.appendChild(tripCard(t)));
  // 日時なしのカードには日時も地名も無いので、検索欄に文字があるあいだは出さない
  if(ls.untimed > 0 && !filtering) box.appendChild(textCard('untimed', '日時なし', ls.untimed, () => toggleSel({kind: 'untimed'})));
  if(!trips.length && (filtering || !ls.untimed)){
    box.appendChild(el('div', 'empty', filtering && ls.trips.length ? '一致する旅がありません' : '写真がありません'));
  }
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
/* 検索欄の Enter: 文字の合う写真（/photos?q=）を下に出す。空なら選択を外す（検索でない旅の選択も外れる） */
function submitTripSearch(){
  const q = $('#lib-search').value.trim();
  if(!q){
    if(ls.sel.trips) applySel(null);
    return;
  }
  applySel({kind: 'q', q});
}
/* 「地図で見る」: 場所の画面をその旅の期間で開く。期間は都道府県のカード・候補・地図の点・写真の格子のすべてに掛かり、
   下には期間の写真全体を出す（sel.places = period）。以前の選択（別の旅の期間で選んだ県・範囲など）は持ち越さず、
   地図の絞り込み（mapBbox）も外す。飛んでいる詳細・カード・候補の応答は捨てる。地図は点を読んだあと点の範囲へ動く */
function mapForTrip(trip){
  ls.period = {from: trip.start, to: trip.end};
  ls.sel.places = {kind: 'period'};
  ls.mapBbox = null;
  ls.fitPoints = true;
  ls.gen++;
  ls.seq.detail++;
  ls.seq.placeCards++;
  ls.seq.placeOptions++;
  ls.loaded.placeCards = false;
  ls.loaded.placeOptions = false;
  ls.page = 1;
  clearDetail();
  setLibView('places', true);
}

/* ==== 5) 日付（年のカード・月のチップ）・場所（行） ==== */
/* 年のカード・月のチップを押す。普通に押すとその年・月を選び（もう一度押すと外す）、それを anchor（Shift の範囲の起点）にする。
   Shift を押しながら押すと、anchor と押したものの形の範囲の min(from)〜max(to) を {kind:'span'} で選ぶ（逆向き・年またぎ・年と月の混在も同じ）。
   anchor が無いときの Shift は普通の選択 */
function pickCalendar(e, S){
  if(e && e.shiftKey && ls.anchor){
    const from = ls.anchor.from < S.from ? ls.anchor.from : S.from;
    const to = ls.anchor.to > S.to ? ls.anchor.to : S.to;
    toggleSel({kind: 'span', from, to});
    return;
  }
  const cur = ls.sel.calendar;
  ls.anchor = cur && selId(cur) === selId(S) ? null : {from: S.from, to: S.to, year: S.year};
  toggleSel(S);
}
function yearCard(y){
  const card = el('div', 'lib-card');
  card.dataset.key = y.year;
  const main = cardMain(card, e => pickCalendar(e, yearSel(y.year)));
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
  // 範囲を選んでいるあいだは、起点の年の月を出し続ける（続けて Shift で別の月を押せるように）
  const year = S && (S.kind === 'year' || S.kind === 'month') ? S.year : (S && S.kind === 'span' && ls.anchor ? ls.anchor.year : null);
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
    chip.addEventListener('click', e => pickCalendar(e, monthSel(year, m)));
    box.appendChild(chip);
  });
}
function renderCalendar(){
  const box = $('#lib-year-cards');
  box.textContent = '';
  ls.years.forEach(y => box.appendChild(yearCard(y)));
  if(ls.calUntimed > 0) box.appendChild(textCard('untimed', '日時なし', ls.calUntimed, () => toggleSel({kind: 'untimed'})));
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
/* 日付の言葉（#lib-when）: Enter（変換中を除く）で GET when?q=。読み込み中は「読んでいます…」、成功で {kind:'words'} を選んで
   「<from の日付>〜<to の日付> として検索（規則 / LLM）」を #lib-when-note に出し、失敗（unreadable・unavailable・400・403・通信の失敗）は R8d の文を出す。
   応答は送った時点の gen と今の gen が同じときだけ使う（待つあいだに月を選べば、遅れた応答は選択を変えない） */
const LIB_WHEN_FAIL = '読めませんでした。ピッカーで選んでください';
function wordsNote(S){ return daySpan(S.from, S.to) + ' として検索（' + (S.source === 'llm' ? 'LLM' : '規則') + '）'; }
function paintWhenNote(){
  const S = ls.sel.calendar;
  let text = '';
  if(ls.whenMsg && ls.whenMsg.gen === ls.gen) text = ls.whenMsg.text;
  else if(S && S.kind === 'words') text = wordsNote(S);
  $('#lib-when-note').textContent = text;
}
function submitWhen(){
  const q = $('#lib-when').value.trim();
  const seq = ++ls.seq.when, gen = ls.gen;
  if(!q){
    ls.whenMsg = null;
    const S = ls.sel.calendar;
    if(S && S.kind === 'words') applySel(null);
    else paintWhenNote();
    return;
  }
  ls.whenMsg = {gen, text: '読んでいます…'};
  paintWhenNote();
  libGet('when', {q}).then(d => {
    if(typeof d.from !== 'string' || typeof d.to !== 'string') throw new Error('unreadable');
    return d;
  }).then(d => {
    if(seq !== ls.seq.when || gen !== ls.gen) return;
    applySel({kind: 'words', from: d.from, to: d.to, source: d.source});
  }, e => {
    if(seq !== ls.seq.when || gen !== ls.gen) return;
    ls.whenMsg = {gen, text: LIB_WHEN_FAIL + (e.message === 'unavailable' ? '（LLM が使えません）' : '')};
    paintWhenNote();
  });
}

/* ==== 6) 場所（都道府県のカード・市区町村のチップ・期間・地図の絞り込み） ==== */
const LIB_CITY_MAX = 30;
/* 都道府県のカード。主ボタン（表紙・名前・枚数）が県を選び、地図をその県の範囲へ動かす。選んだ県のカードには、主ボタンの兄弟として
   市区町村のチップの容器（.lib-cities）が付く（paintCities。ボタンを入れ子にしない） */
function placeCard(p){
  const card = el('div', 'lib-card lib-pref');
  card.dataset.key = p.pref;
  const main = cardMain(card, () => selectPlace({kind: 'pref', pref: p.pref}, p.bbox));
  const cover = el('span', 'lib-cover');
  putImage(cover, p.cover ? (p.cover.thumb_url || '') : '', '');
  main.appendChild(cover);
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', p.pref));
  body.appendChild(el('span', 'lib-card-count', p.count + ' 枚'));
  main.appendChild(body);
  return card;
}
/* 市区町村のチップ（件数の多い順に最大 30。残りは選べない「ほか N」の行）。city が空のものは件数だけを出して押せない（サーバーは空の city で絞れない） */
function buildCities(p){
  const box = el('div', 'lib-cities');
  box.setAttribute('role', 'group');
  box.setAttribute('aria-label', p.pref + 'の市区町村');
  const cities = (p.cities || []).slice().sort((a, b) => b.count - a.count);
  cities.slice(0, LIB_CITY_MAX).forEach(c => {
    const chip = el('button', 'chip lib-city', c.city || '（市区町村なし）');
    chip.type = 'button';
    chip.appendChild(el('span', 'n', String(c.count)));
    if(c.city){
      chip.dataset.key = p.pref + '/' + c.city;
      chip.setAttribute('aria-pressed', 'false');
      chip.addEventListener('click', () => selectPlace({kind: 'city', pref: p.pref, city: c.city}, c.bbox));
    }else{
      chip.disabled = true;
    }
    box.appendChild(chip);
  });
  if(cities.length > LIB_CITY_MAX){
    box.appendChild(el('span', 'lib-more', 'ほか ' + (cities.length - LIB_CITY_MAX) + '（上の欄で探せます）'));
  }
  return box;
}
/* 選んでいる県（県・市区町村のどちらを選んでいても）のカードにだけチップを付ける。選択のたびには作り直さない（押したチップのフォーカスを失わないため） */
function paintCities(){
  const S = ls.sel.places;
  const open = S && (S.kind === 'pref' || S.kind === 'city') ? S.pref : null;
  $('#lib-place-cards').querySelectorAll('.lib-pref').forEach(card => {
    const box = card.querySelector('.lib-cities');
    if(open !== null && card.dataset.key === open){
      card.classList.add('has-cities');
      const data = box ? null : ls.prefs.find(x => x.pref === open);
      if(data) card.appendChild(buildCities(data));
    }else{
      card.classList.remove('has-cities');
      if(box) box.remove();
    }
  });
}
function renderPlaces(){
  const box = $('#lib-place-cards');
  box.textContent = '';
  ls.prefs.forEach(p => box.appendChild(placeCard(p)));
  if(ls.abroad > 0) box.appendChild(textCard('abroad', '海外・不明', ls.abroad, () => selectPlace({kind: 'abroad'}, null)));
  if(!ls.prefs.length && !ls.abroad){
    box.appendChild(el('div', 'empty', ls.mapBbox ? '地図の範囲に写真がありません' : '写真がありません'));
  }
  paintCities();
  paintBox('places');
  updateLibNote();
}
/* カード・チップを押す: 選択（もう一度押すと外す）と、選んだものの範囲への地図の移動。bbox が null（位置のある写真が無い）なら地図は動かさない */
function selectPlace(S, bbox){
  const cur = ls.sel.places;
  const off = !!cur && selId(cur) === selId(S);
  ls.fitPoints = false;
  applySel(off ? null : S);
  if(!off) fitMapTo(bbox);
}
function placeParams(withBbox){
  const p = {hidden: ls.includeHidden};
  if(ls.period){ p.from = ls.period.from; p.to = ls.period.to; }
  if(withBbox && ls.mapBbox) p.bbox = ls.mapBbox.join(',');
  return p;
}
/* 都道府県のカード用の /places（period・hidden・mapBbox を掛ける）。afterRender は、この読み込みが最新のまま描画できたあとに呼ぶ */
function loadPlaceCards(afterRender){
  const seq = ++ls.seq.placeCards;
  libGet('places', placeParams(true)).then(d => {
    if(seq !== ls.seq.placeCards) return;
    ls.prefs = d.prefs || [];
    ls.abroad = d.abroad || 0;
    ls.loaded.placeCards = true;
    renderPlaces();
    if(afterRender) afterRender();
  }).catch(e => { if(seq === ls.seq.placeCards) toast('場所の一覧を読めませんでした: ' + e.message, true); });
}
/* コンボボックスの候補用の /places（period・hidden だけ。地図の絞り込みは掛けない）。開いているリストは新しい候補で作り直す */
function loadPlaceOptions(){
  const seq = ++ls.seq.placeOptions;
  libGet('places', placeParams(false)).then(d => {
    if(seq !== ls.seq.placeOptions) return;
    ls.placeOpts = {prefs: d.prefs || [], abroad: d.abroad || 0};
    ls.loaded.placeOptions = true;
    if(ls.combo.open) refreshCombo();
  }).catch(e => { if(seq === ls.seq.placeOptions) toast('場所の候補を読めませんでした: ' + e.message, true); });
}
/* ---- 場所のコンボボックス（#lib-place-combo。WAI-ARIA の combobox・リストの自動補完） ----
   候補は ls.placeOpts（期間・除外だけを掛けた /places）の都道府県と市区町村（city が空は除く）。入力と名前を NFKC・小文字・カタカナ→ひらがなに直して
   名前の部分一致か、都道府県の読み（ひらがな・ローマ字。県・府・都を除いた形も）の部分一致。並びは都道府県 → 市区町村、それぞれ件数の多い順で上位 20 件、
   超えたら最後に選べない「ほか N 件」の行。地名は textContent にだけ入れ、候補の id は添字から作る（lib-opt-<添字>） */
const LIB_OPT_MAX = 20;
/* 都道府県の読み [ひらがな, ローマ字]（県・府・都の前まで。北海道だけは全体） */
const LIB_PREF_READINGS = {
  '北海道': ['ほっかいどう', 'hokkaido'],
  '青森県': ['あおもり', 'aomori'], '岩手県': ['いわて', 'iwate'], '宮城県': ['みやぎ', 'miyagi'], '秋田県': ['あきた', 'akita'],
  '山形県': ['やまがた', 'yamagata'], '福島県': ['ふくしま', 'fukushima'],
  '茨城県': ['いばらき', 'ibaraki'], '栃木県': ['とちぎ', 'tochigi'], '群馬県': ['ぐんま', 'gunma'], '埼玉県': ['さいたま', 'saitama'],
  '千葉県': ['ちば', 'chiba'], '東京都': ['とうきょう', 'tokyo'], '神奈川県': ['かながわ', 'kanagawa'],
  '新潟県': ['にいがた', 'niigata'], '富山県': ['とやま', 'toyama'], '石川県': ['いしかわ', 'ishikawa'], '福井県': ['ふくい', 'fukui'],
  '山梨県': ['やまなし', 'yamanashi'], '長野県': ['ながの', 'nagano'], '岐阜県': ['ぎふ', 'gifu'], '静岡県': ['しずおか', 'shizuoka'],
  '愛知県': ['あいち', 'aichi'],
  '三重県': ['みえ', 'mie'], '滋賀県': ['しが', 'shiga'], '京都府': ['きょうと', 'kyoto'], '大阪府': ['おおさか', 'osaka'],
  '兵庫県': ['ひょうご', 'hyogo'], '奈良県': ['なら', 'nara'], '和歌山県': ['わかやま', 'wakayama'],
  '鳥取県': ['とっとり', 'tottori'], '島根県': ['しまね', 'shimane'], '岡山県': ['おかやま', 'okayama'], '広島県': ['ひろしま', 'hiroshima'],
  '山口県': ['やまぐち', 'yamaguchi'],
  '徳島県': ['とくしま', 'tokushima'], '香川県': ['かがわ', 'kagawa'], '愛媛県': ['えひめ', 'ehime'], '高知県': ['こうち', 'kochi'],
  '福岡県': ['ふくおか', 'fukuoka'], '佐賀県': ['さが', 'saga'], '長崎県': ['ながさき', 'nagasaki'], '熊本県': ['くまもと', 'kumamoto'],
  '大分県': ['おおいた', 'oita'], '宮崎県': ['みやざき', 'miyazaki'], '鹿児島県': ['かごしま', 'kagoshima'], '沖縄県': ['おきなわ', 'okinawa'],
};
const LIB_PREF_SUFFIX = {'県': ['けん', 'ken'], '府': ['ふ', 'fu'], '都': ['と', 'to']};
/* NFKC・小文字・カタカナ→ひらがな（ァ〜ヶ を 0x60 引く） */
function foldText(text){
  return String(text).normalize('NFKC').toLowerCase().replace(/[ァ-ヶ]/g, c => String.fromCharCode(c.charCodeAt(0) - 0x60));
}
/* 都道府県の読みの一致。県・府・都を付けた形（ながのけん・naganoken）も対象にするが、一致の始まりが付けた部分の中なら数えない
   （「ken」を打っただけで 県 の全部が出ないように） */
function prefReadingMatch(pref, f){
  const r = LIB_PREF_READINGS[pref];
  if(!r) return false;
  const suffix = LIB_PREF_SUFFIX[pref.slice(-1)] || ['', ''];
  return [0, 1].some(i => {
    const at = (r[i] + suffix[i]).indexOf(f);
    return at >= 0 && at < r[i].length;
  });
}
/* 入力 text に合う候補の表 {items, more}。text が空なら全部（↓ で開いたとき） */
function comboRows(text){
  const f = foldText(text.trim());
  const byCount = (a, b) => b.count - a.count || (a.pref + a.city < b.pref + b.city ? -1 : 1);
  const prefs = [], cities = [];
  (ls.placeOpts.prefs || []).forEach(p => {
    if(!p.pref) return;
    if(!f || foldText(p.pref).includes(f) || prefReadingMatch(p.pref, f)){
      prefs.push({kind: 'pref', pref: p.pref, city: '', count: p.count, bbox: p.bbox || null,
                  label: p.pref + '（' + p.count + '）'});
    }
    (p.cities || []).forEach(c => {
      if(!c.city || (f && !foldText(c.city).includes(f))) return;
      cities.push({kind: 'city', pref: p.pref, city: c.city, count: c.count, bbox: c.bbox || null,
                   label: p.pref + ' ' + c.city + '（' + c.count + '）'});
    });
  });
  const all = prefs.sort(byCount).concat(cities.sort(byCount));
  return {items: all.slice(0, LIB_OPT_MAX), more: Math.max(0, all.length - LIB_OPT_MAX)};
}
function optId(i){ return 'lib-opt-' + i; }
/* 強調している候補と aria 属性を合わせる（リストは作り直さない） */
function paintCombo(){
  const c = ls.combo, input = $('#lib-place-combo');
  input.setAttribute('aria-expanded', c.open ? 'true' : 'false');
  if(c.open && c.active >= 0) input.setAttribute('aria-activedescendant', optId(c.active));
  else input.removeAttribute('aria-activedescendant');
  $('#lib-place-list').classList.toggle('hidden', !c.open);
  $('#lib-place-list').querySelectorAll('.lib-opt').forEach(row => {
    const on = c.open && row.id === optId(c.active);
    row.classList.toggle('is-active', on);
    row.setAttribute('aria-selected', on ? 'true' : 'false');
    if(on && row.scrollIntoView) row.scrollIntoView({block: 'nearest'});
  });
}
/* 入力の文字で候補を作り直して開く */
function refreshCombo(){
  const c = ls.combo;
  const rows = comboRows($('#lib-place-combo').value);
  c.items = rows.items;
  c.more = rows.more;
  c.active = -1;
  c.open = true;
  const list = $('#lib-place-list');
  list.textContent = '';
  c.items.forEach((it, i) => {
    const row = el('div', 'lib-opt', it.label);
    row.id = optId(i);
    row.setAttribute('role', 'option');
    row.addEventListener('mousedown', e => e.preventDefault());       // 入力欄のフォーカスを保つ（blur で閉じてしまわないように）
    row.addEventListener('click', () => chooseComboItem(it));
    list.appendChild(row);
  });
  if(c.more > 0){
    const row = el('div', 'lib-opt lib-opt-more', 'ほか ' + c.more + ' 件（文字を足して絞ってください）');
    row.id = optId(c.items.length);
    row.setAttribute('role', 'option');
    row.setAttribute('aria-disabled', 'true');
    list.appendChild(row);
  }
  if(!c.items.length) list.appendChild(el('div', 'lib-opt-none', '候補がありません'));
  paintCombo();
}
function closeCombo(){
  ls.combo.open = false;
  ls.combo.active = -1;
  paintCombo();
}
function moveCombo(delta){
  const c = ls.combo, n = c.items.length;
  if(!n) return;
  c.active = c.active < 0 ? (delta > 0 ? 0 : n - 1) : (c.active + delta + n) % n;
  paintCombo();
}
/* 候補を決める。県・市区町村のカードを選んだのと同じ動き（selectPlace）。ただし同じものを選んでいるときは外さず、地図を動かすだけ */
function chooseComboItem(it){
  closeCombo();
  $('#lib-place-combo').value = it.kind === 'city' ? it.city : it.pref;
  choosePlace(it.kind === 'city' ? {kind: 'city', pref: it.pref, city: it.city} : {kind: 'pref', pref: it.pref}, it.bbox);
}
/* カードに（city なら県のカードの市区町村にも）出ていない候補は、地図の範囲による絞り込みを外してカードを読み直してから選ぶ
   （市区町村のチップはカード用の ls.prefs から作るので、読み直したあとに出る）。読み直しの間に別の操作があれば選ばない
   （カード・月などの選択は gen を進め、候補を決め直すと seq.choose が進む。どちらが変わっても保留中の選択は捨てる） */
function choosePlace(S, bbox){
  const cseq = ++ls.seq.choose;
  ls.seq.cluster++;            // 同じ候補を選び直して選択が変わらないとき（地図を動かすだけ）も、待っているクラスタの展開は捨てる
  const done = () => {
    const cur = ls.sel.places;
    if(cur && selId(cur) === selId(S)) fitMapTo(bbox);
    else selectPlace(S, bbox);
  };
  const card = ls.prefs.find(p => p.pref === S.pref);
  const shown = !!card && (S.kind !== 'city' || (card.cities || []).some(c => c.city === S.city));
  if(!ls.mapBbox || shown){ done(); return; }
  const gen = ls.gen;
  ls.mapBbox = null;
  loadPlaceCards(() => { if(cseq === ls.seq.choose && gen === ls.gen && ls.view === 'places') done(); });
}
function onComboKey(e){
  if(e.isComposing || e.keyCode === 229) return;        // 日本語入力の変換中は変換のためのキー
  const c = ls.combo;
  if(e.key === 'ArrowDown' || e.key === 'ArrowUp'){
    e.preventDefault();
    if(!c.open) refreshCombo();
    moveCombo(e.key === 'ArrowDown' ? 1 : -1);
  }else if(e.key === 'Enter'){
    if(c.open && c.active >= 0 && c.items[c.active]){
      e.preventDefault();
      chooseComboItem(c.items[c.active]);
    }
  }else if(e.key === 'Escape'){
    if(c.open){ e.preventDefault(); e.stopPropagation(); closeCombo(); }
  }
}
/* 期間（「期間: … ✕」）の解除: カード・候補・点を期間なしで読み直す。期間の写真全体の詳細は隠して飛んでいる応答も捨て、
   県・市区町村・範囲などを選んでいれば、その選択のまま期間なしで詳細を読み直す（ページ 1） */
function clearPeriod(){
  if(!ls.period) return;
  ls.period = null;
  ls.fitPoints = false;
  ls.gen++;
  ls.seq.detail++;
  ls.seq.placeCards++;
  ls.seq.placeOptions++;
  ls.page = 1;
  if(ls.sel.places && ls.sel.places.kind === 'period') ls.sel.places = null;
  clearDetail();
  if(ls.view === 'places'){
    showLib();
    loadPlaceCards();
    loadPlaceOptions();
    loadPoints();
    if(ls.sel.places) loadDetail();
  }else{
    ls.loaded.placeCards = false;
    ls.loaded.placeOptions = false;
    ls.pointsStale = true;
  }
}
/* view の一覧を読む（場所は都道府県のカードを毎回、候補は force か未読み込みのとき。地図の点は別に読む） */
function loadLibList(view, force){
  if(view === 'trips') loadTrips();
  else if(view === 'calendar') loadCalendar();
  else if(view === 'places'){
    loadPlaceCards();
    if(force || !ls.loaded.placeOptions) loadPlaceOptions();
  }
}

/* ==== 7) 詳細（写真の格子・ページ） ==== */
function detailParams(S){
  const p = {page: ls.page, per: ls.per, hidden: ls.includeHidden};
  switch(S.kind){
    case 'untimed': p.untimed = true; break;
    case 'abroad': p.abroad = true; break;
    case 'pref': p.pref = S.pref; break;
    case 'city': p.pref = S.pref; p.city = S.city; break;
    case 'bbox': p.bbox = S.bbox.join(','); break;
    case 'period': break;                          // 期間（下で掛ける）の写真全体
    case 'q': p.q = S.q; break;                    // 旅の文字検索
    default: p.from = S.from; p.to = S.to;      // trip・year・month・span・words
  }
  // 場所の画面の期間は、県・市区町村・海外不明・範囲・期間のすべてに掛かる（旅の「地図で見る」）
  if(ls.view === 'places' && ls.period){ p.from = ls.period.from; p.to = ls.period.to; }
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
  // 見えていない（場所以外の view、別のタブ）なら印だけ立てて、地図に戻るとき（setLibView・openLibraryTab）に読む
  if(patch && ('lat' in patch || 'lon' in patch || 'hidden' in patch)){
    if(mapVisible()) loadPoints();
    else{ ls.seq.points++; ls.pointsStale = true; }
  }
}
/* 地図が見えているか: 場所の view で、ライブラリのタブが見えている */
function mapVisible(){
  return ls.view === 'places' && !$('#v-library').classList.contains('hidden');
}

/* ==== 8) 地図 ==== */
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
/* 地図の移動の理由: カード・チップ・「地図で見る」・「全体に戻す」・自分の resize が地図を動かすときは、この eventData を渡す。
   MapLibre は fitBounds(bounds, options, eventData)・easeTo(options, eventData)・resize(eventData) の eventData を、その移動の
   movestart・move・moveend のイベントに載せる。moveend にこれが付いていれば「利用者が動かした」ではないので、カードを絞らない。
   印を状態に持たないので、移動が途中で別の移動に置き換わっても印が残らない */
const LIB_MOVE = {libMove: true};
const LIB_JAPAN = [122, 24, 154, 46];                  // 「全体に戻す」: 日本全体（沖縄から北方領土まで）
const LIB_NARROW_MAP = 480;                              // 地図の幅がこれ未満なら fitBounds の余白を小さくする
function mapWidth(){ return $('#lib-map').getBoundingClientRect().width; }
function fitOptions(){ return {padding: mapWidth() < LIB_NARROW_MAP ? 16 : 40, maxZoom: 12}; }
/* 今の範囲と同じなら動かさない（bbox が null＝位置のある写真が無いときも動かさない） */
function fitMapTo(bbox){
  if(!bbox) return;
  const map = ensureLibMap();
  if(!map) return;
  const cur = currentBbox();
  if(cur.every((v, i) => Math.abs(v - bbox[i]) < 1e-6)) return;
  map.fitBounds(bbox, fitOptions(), LIB_MOVE);
}
/* 利用者が地図を動かしたか。印は 2 つだけ（カメラの差からは推測しない）:
   - moveend のイベントに originalEvent がある。MapLibre 4.7.1 は、ドラッグ（慣性の終わりも）・ホイールのズーム・ダブルクリック・タッチ（ピンチ・
     ダブルタップ）・キーボードの移動、NavigationControl のズームボタンの moveend に originalEvent（元の DOM イベント）を載せる。
   - 自分のコードが付けた eventData {libUser: true}（クラスタを押したときの easeTo）。
   それ以外は絞らない: libMove 付きの自動の移動、resize（ResizeObserver 経由は eventData が無く、resize の _constrain が maxBounds の端や
   最小ズームの付近で中心・ズームを自動で補正して moveend を起こすが、それは利用者の移動ではない）。
   自動の移動の途中に利用者がドラッグしたとき、中断された移動の moveend は libMove 付き（絞らない）、そのあとのドラッグの moveend は originalEvent 付き（絞る）。 */
const LIB_USER = {libUser: true};
function isUserMove(e){
  if(!e || e.libMove) return false;
  return !!(e.originalEvent || e.libUser);
}
function onMapMoveEnd(e){
  if(!isUserMove(e)) return;
  ls.seq.cluster++;            // 利用者が動かした: 待っているクラスタの展開は古い（自分の展開の moveend もここへ来るが、応答は済んでいる）
  if(!mapVisible()) return;
  ls.fitPoints = false;
  ls.mapBbox = currentBbox();
  loadPlaceCards();
}
/* 描画の確定後に地図の大きさを合わせ直す（場所の view を出したとき・詳細の出し入れ・幅 640px の境をまたいだとき・タブに戻ったとき） */
function scheduleMapResize(){
  if(!ls.map || ls.resizeQueued) return;
  const run = () => {
    ls.resizeQueued = false;
    if(ls.map && mapVisible()) ls.map.resize(LIB_MOVE);
  };
  ls.resizeQueued = true;
  if(typeof requestAnimationFrame === 'function') requestAnimationFrame(run);
  else run();
}
/* 点の座標の範囲 [w, s, e, n]（点が無ければ null） */
function featuresBbox(features){
  let box = null;
  (features || []).forEach(f => {
    const c = f.geometry && f.geometry.coordinates;
    if(!c) return;
    if(!box) box = [c[0], c[1], c[0], c[1]];
    else{
      box[0] = Math.min(box[0], c[0]); box[1] = Math.min(box[1], c[1]);
      box[2] = Math.max(box[2], c[0]); box[3] = Math.max(box[3], c[1]);
    }
  });
  return box;
}
/* 「全体に戻す」: 日本全体へ戻し、地図の範囲による絞り込みを外して読み直す */
function resetMap(){
  ls.seq.cluster++;
  ls.fitPoints = false;
  ls.mapBbox = null;
  fitMapTo(LIB_JAPAN);
  loadPlaceCards();
}
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
  // 回転・傾斜は使わない: 回転・傾斜した地図の表示範囲は bbox（東西南北の矩形）で表せず、カードの絞り込み（currentBbox）と合わなくなる。
  // 管理画面の地図に回転・傾斜の用途は無い。右ドラッグ・Ctrl ドラッグ（回転・傾斜）、2 本指の回転、2 本指の傾斜、キーボードの回転・傾斜を無効にする。
  // 範囲選択のズーム（Shift+ドラッグ）も無効にする: MapLibre はこの移動に eventData を渡さず、moveend に originalEvent が載らないので利用者の移動と見分けられない。
  map.dragRotate.disable();
  map.touchZoomRotate.disableRotation();
  map.touchPitch.disable();
  map.keyboard.disableRotation();
  map.boxZoom.disable();
  map.addControl(new maplibregl.NavigationControl({showCompass: false}), 'top-right');
  map.on('load', () => addPointLayers(map));
  map.on('moveend', syncClusterLabels);
  map.on('moveend', onMapMoveEnd);
  map.on('zoomend', syncClusterLabels);
  // moveend・zoomend は描画での source の更新より先に発火し、更新先のタイルがキャッシュにあると sourcedata も出ないので、描画が落ち着いた後にも 1 回合わせる
  map.on('idle', syncClusterLabels);
  map.on('sourcedata', e => {
    if(e.sourceId === LIB_POINTS_SOURCE && e.isSourceLoaded) syncClusterLabels();
  });
  map.on('click', 'lib-clusters', e => {
    const f = e.features && e.features[0];
    if(!f) return;
    // 応答を待つあいだに別の場所を選んだ・全体に戻した・期間を変えた・利用者が地図を動かした・点を読み直したなら、古いクラスタへは寄らない
    const cseq = ++ls.seq.cluster, gen = ls.gen;
    map.getSource(LIB_POINTS_SOURCE).getClusterExpansionZoom(f.properties.cluster_id).then(zoom => {
      if(cseq !== ls.seq.cluster || gen !== ls.gen) return;
      map.easeTo({center: f.geometry.coordinates, zoom}, LIB_USER);
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
  ls.seq.cluster++;            // クラスタの番号が振り直される: 待っている展開は古い点のもの
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
    if(ls.fitPoints){            // 「地図で見る」: 点を読めたら点の範囲へ（点が無ければ動かさない）
      ls.fitPoints = false;
      fitMapTo(featuresBbox(d.features));
    }
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
  ls.fitPoints = false;
  applySel({kind: 'bbox', bbox: currentBbox()});
}

/* ==== 9) 切り替え・初期化 ==== */
function showLib(){
  const view = ls.view;
  const trips = view === 'trips', calendar = view === 'calendar', places = view === 'places';
  $('#lib-trips').classList.toggle('hidden', !trips);
  $('#lib-calendar').classList.toggle('hidden', !calendar);
  $('#lib-places').classList.toggle('hidden', !places);
  const S = ls.sel[view];
  // 詳細は 1 つだけ。旅・日付では一覧の下、場所では地図の下（右の列）に置く
  const detail = $('#lib-detail');
  const host = places ? $('#lib-place-main') : $('#v-library');
  if(detail.parentNode !== host) host.appendChild(detail);
  detail.classList.toggle('hidden', !S);
  $('#lib-sel').textContent = S ? selLabel(S) : '';
  $('#lib-view-trips').classList.toggle('on', trips);
  $('#lib-view-calendar').classList.toggle('on', calendar);
  $('#lib-view-places').classList.toggle('on', places);
  updateLibNote();
  // 探す欄は view ごと（旅: 文字検索 / 日付: 言葉 / 場所: コンボボックス）。出ていない欄のリストは閉じる
  $('#lib-search').classList.toggle('hidden', !trips);
  $('#lib-when').classList.toggle('hidden', !calendar);
  $('#lib-when-note').classList.toggle('hidden', !calendar);
  const combo = $('#lib-place-combo');
  combo.classList.toggle('hidden', !places);
  combo.parentNode.classList.toggle('hidden', !places);
  if(!places && ls.combo.open) closeCombo();
  paintWhenNote();
  $('#lib-period').classList.toggle('hidden', !ls.period);
  $('#lib-period-text').textContent = ls.period ? '期間: ' + rangeLabel(ls.period.from, ls.period.to) : '';
  paintSelection();
  const shown = places && !!S;
  if(places){
    // 場所を出したとき・詳細の出し入れ（スクロールバーで幅が変わる）で、描画の確定後に地図の大きさを合わせる
    if(ensureLibMap() && (!ls.onPlaces || shown !== ls.detailShown)) scheduleMapResize();
  }else{
    closePopup();
  }
  ls.onPlaces = places;
  ls.detailShown = shown;
}
/* view を切り替える。view ごとの選択は残り、その view の詳細を読み直す（ページ 1）。飛んでいる詳細の応答は捨てる。
   force は同じ view のままでも切り替えと同じにする（選択を差し替えたあと） */
function setLibView(view, force){
  const changed = force || view !== ls.view;
  if(changed){
    ls.view = view;
    ls.gen++;
    ls.seq.detail++;
    ls.page = 1;
    clearDetail();
  }
  showLib();
  if(view === 'places') loadPoints();
  if(view !== 'trips' || !ls.loaded.trips) loadLibList(view);
  if(changed && ls.sel[view]) loadDetail();
}
/* ライブラリタブを開いたとき（admin.js の showTab から）。旅は毎回読み直す。選んでいる view の一覧・詳細・地図の点も */
function openLibraryTab(){
  loadTrips();
  if(ls.view !== 'trips') loadLibList(ls.view, true);
  showLib();
  if(ls.view === 'places'){
    loadPoints();
    scheduleMapResize();        // 隠れているあいだに寸法が変わった（タブの切り替え）ので合わせ直す
  }
  if(ls.sel[ls.view]){ ls.page = 1; clearDetail(); loadDetail(); }
}
/* 除外も含める: すべての種類の飛んでいる応答を捨て、見えている view の一覧・地図の点・詳細を hidden を変えて読み直す（詳細はページ 1）。
   見えていない view の一覧は loaded を落とすだけ（開くときに読む）。選んでいたものが応答に無くなっても選択は残す */
function onLibHiddenChange(){
  ls.includeHidden = !!$('#lib-hidden').checked;
  ls.gen++;
  Object.keys(ls.seq).forEach(k => { ls.seq[k]++; });
  ls.whenMsg = null;           // 読み取り中の日付の言葉の応答は上で捨てた。「読んでいます…」は解き、words の選択があればその期間の文に戻す
  paintWhenNote();
  closePopup();
  Object.keys(ls.loaded).forEach(k => { ls.loaded[k] = false; });
  loadLibList(ls.view, true);
  if(ls.view === 'places') loadPoints();
  else ls.pointsStale = true;
  if(ls.sel[ls.view]){ ls.page = 1; loadDetail(); }
}
function initLibrary(){
  $('#lib-view-trips').addEventListener('click', () => setLibView('trips'));
  $('#lib-view-calendar').addEventListener('click', () => setLibView('calendar'));
  $('#lib-view-places').addEventListener('click', () => setLibView('places'));
  $('#lib-hidden').addEventListener('change', onLibHiddenChange);
  $('#lib-prev').addEventListener('click', () => { if(ls.page > 1){ ls.page--; loadDetail(); } });
  $('#lib-next').addEventListener('click', () => { if(ls.page < ls.pages){ ls.page++; loadDetail(); } });
  $('#lib-bbox').addEventListener('click', viewBbox);
  $('#lib-reset').addEventListener('click', resetMap);
  $('#lib-period-clear').addEventListener('click', clearPeriod);
  // 旅の検索: 打つとカードを絞り、Enter（変換中を除く）で文字の合う写真を下に出す。空で Enter なら選択を外す
  $('#lib-search').addEventListener('input', () => {
    ls.tripFilter = $('#lib-search').value;
    renderTrips();
  });
  $('#lib-search').addEventListener('keydown', e => {
    if(e.key !== 'Enter' || e.isComposing || e.keyCode === 229) return;
    e.preventDefault();
    submitTripSearch();
  });
  $('#lib-when').addEventListener('keydown', e => {
    if(e.key !== 'Enter' || e.isComposing || e.keyCode === 229) return;
    e.preventDefault();
    submitWhen();
  });
  const combo = $('#lib-place-combo');
  combo.addEventListener('input', () => {
    if(combo.value.trim()) refreshCombo();
    else closeCombo();
  });
  combo.addEventListener('keydown', onComboKey);
  combo.addEventListener('blur', closeCombo);
  // 幅 640px の境をまたぐと地図の大きさが変わる（CSS の @media と同じ境）
  if(typeof matchMedia === 'function'){
    const narrow = matchMedia('(max-width: 640px)');
    const onChange = () => { if(mapVisible()) scheduleMapResize(); };
    if(narrow.addEventListener) narrow.addEventListener('change', onChange);
    else if(narrow.addListener) narrow.addListener(onChange);
  }
  onPhotoChange(libPhotoChanged);
}
