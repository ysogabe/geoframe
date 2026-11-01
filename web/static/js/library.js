/* ============================================================
   library.js — 管理ページのライブラリタブ（classic script / 依存なし）
   構成: 1) 状態 2) 画像・タイル 3) 旅の一覧 4) 日付・場所の眺め方 5) 格子（ページ） 6) 地図（MapLibre のクラスタ・吹き出し・範囲）
        7) 切り替え・初期化
   admin.js（$ el toast）と photos.js（normalizePhoto openModalFor onPhotoChange tileStyle）の後に読み込む。
   定義だけを置き、起動は admin.js の boot() が initLibrary() を呼ぶ形（読み込んだだけでは何にも触らない）。
   サーバーは GET /api/admin/library/{trips,photos,points,calendar,places}（crates/geoframe/src/admin_library.rs）。
   描画は textContent と DOM API だけ（HTML 文字列を流し込む API は使わない）。
   ============================================================ */
'use strict';

/* ==== 1) 状態 ==== */
/* view: 'trips'（旅の一覧）| 'map'（地図）| 'calendar'（年 → 月）| 'places'（都道府県 → 市区町村）。
   grid が true の間は、どれの上にも格子（#lib-grid-box）を出す（戻るで元の view へ）。
   sel: 格子に出す絞り込み {from, to} | {untimed: true} | {bbox: [w, s, e, n]} | {pref, city?} | {abroad: 1} と、表示用の label。
   years / prefs: calendar・places の応答。openYears / openPrefs: 開いている年・都道府県。loaded: 読み込み済みの view
   period: 地図の点と範囲の絞り込みに掛ける期間 {from, to}（旅の「地図で見る」で入り、解除ボタンで外す）。
   pointsStale: 地図が見えていないあいだに設定（除外も含める）が変わったので、地図に戻るとき点を取り直す印。
   seq: 要求の種類ごとの番号。古い応答は捨てる（種類を分けるのは、吹き出しの要求が格子の読み込みを捨てないため） */
const ls = {
  view: 'trips', includeHidden: false, grid: false,
  trips: [], untimed: 0,
  years: [], calUntimed: 0, prefs: [], abroad: 0,
  openYears: new Set(), openPrefs: new Set(),
  loaded: {trips: false, calendar: false, places: false},
  sel: null, period: null, pointsStale: false,
  page: 1, pages: 1, per: 60, total: 0, items: [], tiles: [],
  map: null, pendingData: null, labels: new Map(), popup: null,
  seq: {trips: 0, grid: 0, points: 0, popup: 0, calendar: 0, places: 0},
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

/* ==== 3) 旅の一覧 ==== */
function placeLabel(p){
  const name = [p.pref, p.city].filter(Boolean).join(' ');
  return (name || '海外・不明') + ' ' + p.count;
}
function rangeLabel(from, to){ return from === to ? from : from + ' 〜 ' + to; }
function tripRow(trip){
  const row = el('div', 'lib-trip');
  row.dataset.key = trip.key;
  const cover = el('div', 'lib-cover');
  putImage(cover, trip.cover ? (trip.cover.thumb_url || '') : '', '');
  cover.classList.add('lib-clickable');
  cover.setAttribute('role', 'button');
  cover.setAttribute('tabindex', '0');
  cover.setAttribute('aria-label', rangeLabel(trip.start, trip.end) + 'の写真を見る');
  const show = () => selectTrip(trip);
  cover.addEventListener('click', show);
  cover.addEventListener('keydown', e => {
    if(e.key === 'Enter' || e.key === ' '){ e.preventDefault(); show(); }
  });
  row.appendChild(cover);

  const body = el('div', 'lib-trip-body');
  body.appendChild(el('div', 'lib-trip-range', rangeLabel(trip.start, trip.end)));
  body.appendChild(el('div', 'lib-trip-count',
    trip.count + ' 枚（位置なし ' + trip.noloc + '）'));
  const tags = el('div', 'tags');
  (trip.places || []).forEach(p => tags.appendChild(el('span', 'tag t-subject', placeLabel(p))));
  body.appendChild(tags);
  const acts = el('div', 'lib-trip-acts');
  const photos = el('button', 'btn lib-trip-photos', '写真を見る');
  photos.type = 'button';
  photos.addEventListener('click', show);
  const map = el('button', 'btn lib-trip-map', '地図で見る');
  map.type = 'button';
  map.addEventListener('click', () => mapForTrip(trip));
  acts.appendChild(photos);
  acts.appendChild(map);
  body.appendChild(acts);
  row.appendChild(body);
  return row;
}
/* 末尾の 1 行（日時なし・海外・不明）。押すと格子 */
function summaryRow(text, onShow){
  const row = el('div', 'lib-trip lib-untimed');
  const body = el('div', 'lib-trip-body');
  body.appendChild(el('div', 'lib-trip-range', text));
  const acts = el('div', 'lib-trip-acts');
  const btn = el('button', 'btn lib-trip-photos', '写真を見る');
  btn.type = 'button';
  btn.addEventListener('click', onShow);
  acts.appendChild(btn);
  body.appendChild(acts);
  row.appendChild(body);
  return row;
}
function untimedRow(count){
  return summaryRow('日時なし（' + count + ' 枚）', () => openGrid({untimed: true, label: '日時なし'}));
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
  const box = $('#lib-trips');
  box.textContent = '';
  ls.trips.forEach(t => box.appendChild(tripRow(t)));
  if(ls.untimed > 0) box.appendChild(untimedRow(ls.untimed));
  if(!ls.trips.length && !ls.untimed) box.appendChild(el('div', 'empty', '写真がありません'));
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
function selectTrip(trip){
  openGrid({from: trip.start, to: trip.end, label: rangeLabel(trip.start, trip.end)});
}
function mapForTrip(trip){
  ls.period = {from: trip.start, to: trip.end};
  ls.view = 'map';
  ls.grid = false;
  showLib();
  loadPoints();
}

/* ==== 4) 日付・場所の眺め方 ==== */
/* 年・都道府県の行（押すと下の月・市区町村の行を開閉）と、その下の行。どちらも「写真を見る」で格子 */
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
function toggleIn(set, key, render){
  if(set.has(key)) set.delete(key); else set.add(key);
  render();
}
/* calendar は形（桁）だけで数えるので、月 13 や 31 日 24 時台のような暦に無い値もその年・月に入る。格子の範囲も 00〜99 で形いっぱいに取る（サーバーは from/to を形だけ見て文字列で比べる） */
function yearRange(year){ return {from: year + '-00-00 00:00', to: year + '-99-99 99:99'}; }
function monthRange(year, month){ return {from: year + '-' + month + '-00 00:00', to: year + '-' + month + '-99 99:99'}; }
function renderCalendar(){
  const box = $('#lib-calendar');
  box.textContent = '';
  ls.years.forEach(y => {
    const open = ls.openYears.has(y.year);
    const group = el('div', 'lib-group');
    group.appendChild(groupRow(y.year + '年', y.count, {
      key: y.year, expanded: open,
      onToggle: () => toggleIn(ls.openYears, y.year, renderCalendar),
      onShow: () => openGrid(Object.assign(yearRange(y.year), {label: y.year + '年'})),
    }));
    if(open){
      (y.months || []).forEach(m => group.appendChild(groupRow(m.month + '月', m.count, {
        key: y.year + '-' + m.month, sub: true,
        onShow: () => openGrid(Object.assign(monthRange(y.year, m.month), {label: y.year + '年' + m.month + '月'})),
      })));
    }
    box.appendChild(group);
  });
  if(ls.calUntimed > 0) box.appendChild(untimedRow(ls.calUntimed));
  if(!ls.years.length && !ls.calUntimed) box.appendChild(el('div', 'empty', '写真がありません'));
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
function renderPlaces(){
  const box = $('#lib-places');
  box.textContent = '';
  ls.prefs.forEach(p => {
    const open = ls.openPrefs.has(p.pref);
    const group = el('div', 'lib-group');
    group.appendChild(groupRow(p.pref, p.count, {
      key: p.pref, expanded: open,
      onToggle: () => toggleIn(ls.openPrefs, p.pref, renderPlaces),
      onShow: () => openGrid({pref: p.pref, label: p.pref}),
    }));
    if(open){
      (p.cities || []).forEach(c => {
        // 市区町村が空の項目は、サーバーが空の city を絞り込みに使えない（空の値は捨てられる）ので、件数だけを出す
        group.appendChild(groupRow(c.city || '（市区町村なし）', c.count, {
          key: p.pref + '/' + c.city, sub: true,
          onShow: c.city ? () => openGrid({pref: p.pref, city: c.city, label: p.pref + ' ' + c.city}) : null,
        }));
      });
    }
    box.appendChild(group);
  });
  if(ls.abroad > 0){
    box.appendChild(summaryRow('海外・不明（' + ls.abroad + ' 枚）', () => openGrid({abroad: 1, label: '海外・不明'})));
  }
  if(!ls.prefs.length && !ls.abroad) box.appendChild(el('div', 'empty', '写真がありません'));
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
/* いまの view に要る一覧を読む（trips は旅のタブを開くとき・設定の変更で別に読む） */
function loadViewList(){
  if(ls.view === 'calendar') loadCalendar();
  else if(ls.view === 'places') loadPlaces();
}

/* ==== 5) 格子（ページ） ==== */
function openGrid(sel){
  ls.sel = sel;
  ls.page = 1;
  ls.grid = true;
  showLib();
  loadGrid();
}
function gridParams(){
  const s = ls.sel || {};
  const p = {page: ls.page, per: ls.per, hidden: ls.includeHidden};
  if(s.untimed){ p.untimed = true; }
  else if(s.abroad){ p.abroad = true; }
  else if(s.pref){ p.pref = s.pref; if(s.city) p.city = s.city; }
  else if(s.bbox){
    p.bbox = s.bbox.join(',');
    if(ls.period){ p.from = ls.period.from; p.to = ls.period.to; }
  }else{ p.from = s.from; p.to = s.to; }
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
  $('#lib-sel').textContent = ls.sel ? ls.sel.label : '';
}
function loadGrid(){
  const seq = ++ls.seq.grid;
  libGet('photos', gridParams()).then(d => {
    if(seq !== ls.seq.grid) return;
    ls.page = d.page; ls.pages = d.pages; ls.total = d.total;
    ls.items = (d.items || []).map(libPhoto);
    renderGrid();
  }).catch(e => { if(seq === ls.seq.grid) toast('写真を読めませんでした: ' + e.message, true); });
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
  // 見えていない（旅の一覧・範囲の格子の上、別のタブ）なら印だけ立てて、地図に戻るとき（closeGrid・setLibView・openLibraryTab）に読む
  if(patch && ('lat' in patch || 'lon' in patch || 'hidden' in patch)){
    if(ls.view === 'map' && !ls.grid && !$('#v-library').classList.contains('hidden')) loadPoints();
    else{ ls.seq.points++; ls.pointsStale = true; }
  }
}

/* ==== 6) 地図 ==== */
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
  const bbox = currentBbox();
  openGrid({bbox, label: '地図の範囲' + (ls.period ? '・' + rangeLabel(ls.period.from, ls.period.to) : '')});
}

/* ==== 7) 切り替え・初期化 ==== */
function showLib(){
  const trips = ls.view === 'trips', map = ls.view === 'map';
  const calendar = ls.view === 'calendar', places = ls.view === 'places';
  $('#lib-trips').classList.toggle('hidden', ls.grid || !trips);
  $('#lib-calendar').classList.toggle('hidden', ls.grid || !calendar);
  $('#lib-places').classList.toggle('hidden', ls.grid || !places);
  $('#lib-map-box').classList.toggle('hidden', ls.grid || !map);
  $('#lib-grid-box').classList.toggle('hidden', !ls.grid);
  $('#lib-view-trips').classList.toggle('on', trips);
  $('#lib-view-calendar').classList.toggle('on', calendar);
  $('#lib-view-places').classList.toggle('on', places);
  $('#lib-view-map').classList.toggle('on', map);
  updateLibNote();
  $('#lib-period').classList.toggle('hidden', !ls.period);
  $('#lib-period-text').textContent = ls.period ? '期間: ' + rangeLabel(ls.period.from, ls.period.to) : '';
  if(map && !ls.grid){
    const m = ensureLibMap();
    if(m) m.resize();           // 隠れているあいだに寸法が変わった（タブ・ビューの切り替え）ので合わせ直す
  }else{
    closePopup();
  }
}
function setLibView(view){
  ls.view = view;
  ls.grid = false;
  showLib();
  if(view === 'map') loadPoints();
  else loadViewList();
}
/* ライブラリタブを開いたとき（admin.js の showTab から）。旅は毎回読み直す。地図ビューなら点も */
function closeGrid(){
  ls.grid = false;
  showLib();
  if(ls.view === 'map' && ls.pointsStale) loadPoints();
}
function openLibraryTab(){
  loadTrips();
  loadViewList();
  showLib();
  if(ls.view === 'map' && !ls.grid) loadPoints();
}
function onLibHiddenChange(){
  ls.includeHidden = !!$('#lib-hidden').checked;
  loadTrips();
  // 日付・場所は読み込み済みのもの（いま見ている view を含む）を読み直す
  if(ls.loaded.calendar || ls.view === 'calendar') loadCalendar();
  if(ls.loaded.places || ls.view === 'places') loadPlaces();
  if(ls.grid){ ls.page = 1; loadGrid(); }
  // 地図が見えていれば点を読み直す。見えていない（旅の一覧・格子の上）なら、飛んでいる応答を捨てて印だけ立て、地図に戻るとき読む
  if(ls.view === 'map' && !ls.grid) loadPoints();
  else{ ls.seq.points++; ls.pointsStale = true; }
}
function initLibrary(){
  $('#lib-view-trips').addEventListener('click', () => setLibView('trips'));
  $('#lib-view-calendar').addEventListener('click', () => setLibView('calendar'));
  $('#lib-view-places').addEventListener('click', () => setLibView('places'));
  $('#lib-view-map').addEventListener('click', () => setLibView('map'));
  $('#lib-hidden').addEventListener('change', onLibHiddenChange);
  $('#lib-back').addEventListener('click', closeGrid);
  $('#lib-prev').addEventListener('click', () => { if(ls.page > 1){ ls.page--; loadGrid(); } });
  $('#lib-next').addEventListener('click', () => { if(ls.page < ls.pages){ ls.page++; loadGrid(); } });
  $('#lib-bbox').addEventListener('click', viewBbox);
  $('#lib-period-clear').addEventListener('click', () => { ls.period = null; showLib(); loadPoints(); });
  onPhotoChange(libPhotoChanged);
}
