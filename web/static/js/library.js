/* ============================================================
   library.js — 管理ページのライブラリタブ（classic script / 依存なし）
   構成: 1) 状態 2) 画像・タイル 3) 選択と下の詳細 4) 旅のカード 5) 日付（年のカード・月のチップ）
        6) 場所（国・地域のカード・日本の市区町村のチップ・期間）7) 写真の格子（詳細）8) 地図（MapLibre のクラスタ・吹き出し・範囲・移動の理由）9) 切り替え・初期化
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
        | {kind:'country', country, label} | {kind:'region', country, region, pref, label} | {kind:'city', country, region, pref, city, label}（日本の地域の市区町村）
        | {kind:'region_unknown', country, label} | {kind:'unclassified', label} | {kind:'no_location', label}
        | {kind:'legacy_pref', pref, city, label}（旧地名: 日本の 47 都道府県の対応表にない pref〔表記揺れ〕。API では日本の region_unknown に入りカードには出ないので、
          コンボボックスの候補（legacy 印付き）から旧形式 pref=<名前>（city があれば &city=<名前>）で選ぶ。名前をキーにするのはこの種類だけ。新形式の country/region とは混ぜない）
          （country・region は /places の国 ID・地域 ID。名前は label（見出し）と日本の pref（旧 prefs の市区町村を引くキー）だけで、選択の同一性は ID で決める）
        | {kind:'bbox', bbox: [w, s, e, n]} | {kind:'period'}（場所の画面で、期間の写真全体）
        | {kind:'span', from, to}（Shift の範囲。形の範囲）| {kind:'words', from, to, source}（日付の言葉。暦の範囲）| {kind:'q', q}（旅の文字検索）
   選ぶと一覧を残したまま #lib-detail に S の写真の格子を出す。同じカードをもう一度押すと選択を外す。
   gen: 選択・view・除外が変わるたびに増やす。読み取りを待つ間にこれが変わった応答は捨てる（seq と二重に守る）。
   anchor: Shift の範囲の起点 {from, to, year}（直前に普通に選んだ年・月の形の範囲と、月のチップを出す年）。年・月・範囲の選択以外になると null。
   monthYear: 範囲の選択中に月のチップを出す年（月のチップ列の「‹ 前の年」「次の年 ›」で切り替える。起点 anchor は変えない）。
        null なら選択の年（範囲を選んでいるあいだは anchor.year）。範囲（span）以外の選択に替わると null に戻る。
   tripFilter: 旅のカードを絞る検索欄の文字。whenMsg: 日付の言葉の読み込み中・失敗の文 {gen, text}（その gen のあいだだけ出す）。
   combo: 場所のコンボボックスの状態 {open, items, more, active}（items は選べる候補、more は表に入りきらなかった件数、active は強調している添字）。
   years: calendar の応答。
   countries / unknown / classif / prefs: 国・地域のカード用の /places の応答（period・hidden・mapBbox を掛ける。countries は国の配列（regions を持つ）、
   unknown は {unclassified, no_location}、classif は {available, pending}、prefs は日本の市区町村を引く旧形式の行）。
   openCountry: 地域を展開している国の ID（undefined は未決定。初回に期間・bbox・選択がなければ日本。選択すると選んだ国になる）。mapHint: 地図の案内 {sel, text}（sel は出した選択の selId。その選択のあいだだけ出す）。
   placeOpts: コンボボックスの候補用の /places の応答（prefs と、旧地名の見分けに使う countries）
   （period・hidden だけ。mapBbox は掛けない）。保存先・seq（placeCards・placeOptions）・loaded を別に持つ。
   loaded: 読み込み済みの一覧
   period: 場所の画面に掛かる期間 {from, to}（旅の「地図で見る」で入り、解除ボタンで外す）。カード・候補・点・詳細のすべてに掛かる。
   mapBbox: 利用者が地図を動かしたときのカードの絞り込み [w, s, e, n]。null なら絞らない。
   fitPoints: 「地図で見る」のあと、点を読めたら点の範囲へ地図を動かす印。
   onPlaces・detailShown・resizeQueued: 地図の大きさを合わせ直す時点を決める印（場所の view を出した・詳細の出し入れ・合わせ直しの予約中）。
   pointsStale: 地図が見えていないあいだに設定（除外も含める）が変わったので、地図に戻るとき点を取り直す印。
   seq: 要求の種類ごとの番号。古い応答は捨てる（種類を分けるのは、吹き出しの要求が詳細の読み込みを捨てないため）。
        cluster はクラスタを押したあとの展開ズームの応答待ち。応答のとき seq.cluster と gen の両方が押した時点のままでなければ地図を動かさない
        （カード・候補・全体に戻す・利用者の地図の移動の開始（movestart）・点の読み直しは seq.cluster を進め、選択・view・期間・除外の変更は gen を進める） */
const ls = {
  view: 'trips', includeHidden: false,
  trips: [], untimed: 0,
  years: [], calUntimed: 0, prefs: [], countries: [], unknown: {}, classif: {available: true, pending: 0},
  openCountry: undefined, mapHint: null, placeOpts: {prefs: [], countries: []},
  loaded: {trips: false, calendar: false, placeCards: false, placeOptions: false},
  sel: {trips: null, calendar: null, places: null}, gen: 0, anchor: null, monthYear: null,
  tripFilter: '', whenMsg: null, combo: {open: false, items: [], more: 0, active: -1},
  period: null, mapBbox: null, fitPoints: false, pointsStale: false,
  onPlaces: false, detailShown: false, resizeQueued: false,
  page: 1, pages: 1, per: 60, total: 0, items: [], tiles: [],
  map: null, pendingData: null, labels: new Map(), popup: null, popupInfo: null,
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
/* 旅のカードの地名のタグ [{text, count}]（件数の多い順。同数は places → regions の順）。
   regions[]（国・地域の上位。海外の旅はここで国・地域名が出る）がある応答では、日本の pref のある写真は今までどおり places[] の「都道府県 市区町村」を使い
   （市区町村まで分かるため）、regions[] は日本以外の国・地域と、places[] に pref が無い日本の分類（境界データで入った写真）だけを足す。
   places[] の pref が空の行（「海外・不明」）は出さない。regions[] が無い旧応答は places[] をそのまま使う */
function tripPlaceTags(trip){
  const places = Array.isArray(trip.places) ? trip.places : [];
  if(!Array.isArray(trip.regions)) return places.map(p => ({text: placeLabel(p), count: p.count}));
  const inPlaces = new Set(places.filter(p => p.pref).map(p => p.pref));
  const tags = places.filter(p => p.pref).map(p => ({text: placeLabel(p), count: p.count}));
  trip.regions.forEach(r => {
    if(!r || typeof r.label !== 'string' || !r.label) return;
    // 日本の pref のある写真は places[] の行で出している: 同じ都道府県（regions[].region は pref と同じ名前。名称表の 47 件で一致を確認済み）と、
    // 地域が決まらない日本（対応表にない旧地名の pref はここに入る）は、places[] に pref の行があれば足さない
    if(r.country_id === LIB_JAPAN_ID && inPlaces.size && (!r.region_id || inPlaces.has(r.region))) return;
    tags.push({text: r.label + ' ' + r.count, count: Number(r.count) || 0});
  });
  return tags.map((t, i) => ({t, i})).sort((a, b) => (b.t.count - a.t.count) || (a.i - b.i)).map(x => x.t);
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
    case 'country': case 'region': case 'city': case 'region_unknown': case 'legacy_pref': case 'unclassified': case 'no_location': return S.label;
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
    case 'country': return 'country:' + S.country;
    case 'region': return 'region:' + S.country + '/' + S.region;
    case 'city': return 'city:' + S.country + '/' + S.region + '/' + S.city;
    case 'region_unknown': return 'region_unknown:' + S.country;
    case 'legacy_pref': return 'legacy_pref:' + encodeURIComponent(S.pref) + '/' + encodeURIComponent(S.city || '');
    default: return S.kind;          // untimed・unclassified・no_location・period
  }
}
/* 地域のカードの data-key（国 ID/地域 ID）。地域不明は固定の印 */
const LIB_REGION_UNKNOWN = '!unknown';
function regionKey(countryId, regionId){ return countryId + '/' + regionId; }
/* カード・チップ・行の data-key が、選択のどれに当たるか。exact は選んだもの本体、ctx はそれを含む親（月を選んだときの年のカード） */
function selectedKeys(S){
  const exact = new Set(), ctx = new Set();
  if(!S) return {exact, ctx};
  switch(S.kind){
    case 'trip': exact.add(S.key); break;
    case 'year': exact.add(S.year); break;
    case 'month': exact.add(S.year + '-' + S.month); ctx.add(S.year); break;
    case 'untimed': exact.add('untimed'); break;
    case 'country': exact.add(S.country); break;
    case 'region': exact.add(regionKey(S.country, S.region)); break;
    case 'city': exact.add(regionKey(S.country, S.region) + '/' + S.city); ctx.add(regionKey(S.country, S.region)); break;
    case 'region_unknown': exact.add(regionKey(S.country, LIB_REGION_UNKNOWN)); break;
    case 'unclassified': exact.add('unclassified'); break;
    case 'no_location': exact.add('no_location'); break;
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
  if(!(S && S.kind === 'span')) ls.monthYear = null;
  ls.gen++;
  ls.seq.detail++;
  ls.page = 1;
  setMapHint(null);            // 地図の案内は選択が変わる（外れる）と捨てる。同じ選択のまま読み直すだけの操作（期間・除外）は applySel を通らず、案内を保つ
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
  tripPlaceTags(trip).forEach(t => tags.appendChild(el('span', 'tag t-subject', t.text)));
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
  else if(ls.view === 'places' && ls.loaded.placeCards) text = (ls.mapBbox ? '地図の範囲: ' : '') + ls.countries.length + ' か国';
  $('#lib-note').textContent = text;
}
/* 空白（全角も）で分けた小文字の語 */
function libWords(text){ return text.toLowerCase().split(/[\s\u3000]+/).filter(Boolean); }
/* 検索欄の文字で絞った旅（rangeLabel と placeLabel の文字に、語のすべてが部分一致するもの） */
function visibleTrips(){
  const words = libWords(ls.tripFilter);
  if(!words.length) return ls.trips;
  return ls.trips.filter(t => {
    const text = (rangeLabel(t.start, t.end) + ' ' + tripPlaceTags(t).map(x => x.text).join(' ')).toLowerCase();
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
  ls.mapHint = null;               // 選択が替わるので、前の選択の案内は捨てる（表示はこのあと setLibView が合わせる）
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
/* 月のチップを出す年: 切り替えた年（monthYear）が一覧にあればそれ。無ければ、年・月を選んでいればその年、範囲を選んでいれば起点の年 */
function monthsYear(){
  const S = ls.sel.calendar;
  const base = S && (S.kind === 'year' || S.kind === 'month') ? S.year : (S && S.kind === 'span' && ls.anchor ? ls.anchor.year : null);
  if(!base) return null;
  return ls.monthYear && ls.years.some(x => x.year === ls.monthYear) ? ls.monthYear : base;
}
/* 月のチップ列の端の「‹ 前の年」「次の年 ›」。出す年だけを替え、選択も起点（anchor）も変えない
   （年をまたぐ「月から月」の範囲は、起点の月を押したあとに年を替えて、Shift で終わりの月を押す）。作り直されるので、押した向きの目印へフォーカスを戻す */
function stepMonthYear(year, dir){
  ls.monthYear = year;
  renderMonths(true);
  paintBox('calendar');            // 作り直したチップに選択の表示（is-selected・aria-pressed）を戻す
  const box = $('#lib-months');
  const nav = box.querySelector('.lib-month-nav[data-dir=' + dir + ']') || box.querySelector('.lib-month-nav');
  if(nav) nav.focus();
}
function monthNav(year, dir){
  const btn = el('button', 'chip lib-month-nav', dir === 'prev' ? '‹ ' + year + '年' : year + '年 ›');
  btn.type = 'button';
  btn.dataset.dir = dir;
  btn.setAttribute('aria-label', year + '年の月を出す');
  btn.addEventListener('click', () => stepMonthYear(year, dir));
  return btn;
}
/* 選んだ年（年・月のどちらを選んでいても）の月のチップ。1〜12 月を並べ、応答に無い月は押せない。
   選択のたびには作り直さない（rebuild か、年が変わったときだけ）。フォーカスを失わないため */
function renderMonths(rebuild){
  const box = $('#lib-months');
  // 範囲を選んでいるあいだは、起点の年（か切り替えた年）の月を出し続ける（続けて Shift で別の月を押せるように）
  const year = monthsYear();
  const y = year ? ls.years.find(x => x.year === year) : null;
  box.classList.toggle('hidden', !y);
  if(!y){ box.textContent = ''; delete box.dataset.year; return; }
  if(!rebuild && box.dataset.year === year) return;
  box.dataset.year = year;
  box.textContent = '';
  const all = ls.years.map(x => x.year).sort();
  const at = all.indexOf(year);
  if(at > 0) box.appendChild(monthNav(all[at - 1], 'prev'));
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
  if(at < all.length - 1) box.appendChild(monthNav(all[at + 1], 'next'));
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

/* ==== 6) 場所（国・地域のカード・日本の市区町村のチップ・期間・地図の絞り込み） ==== */
const LIB_CITY_MAX = 30;
const LIB_JAPAN_ID = 'ne:JPN';
const LIB_FIT_HINT = 'この地域は地図を移動して確認できます';
/* カードの載せ方: 国のカード（lib-country）の下に、展開している国の地域のカード（lib-region）が続き、最後に国一覧の「地域未判定」「場所不明」。
   どれも 1 つの div.lib-card（非対話の容器）で、主ボタン（表紙・名前・枚数）が選択を受ける。名前は textContent だけで入れる。
   国と地域は件数の多い順・同数は表示名順（サーバーの並びのまま）。地域不明は地域の最後、日本の市区町村のチップは選んだ地域のカードの中 */
function placeItemCard(cls, key, name, item, onPress){
  const card = el('div', 'lib-card ' + cls);
  card.dataset.key = key;
  const main = cardMain(card, onPress);
  const cover = el('span', 'lib-cover');
  putImage(cover, item.cover ? (item.cover.thumb_url || '') : '', '');
  main.appendChild(cover);
  const body = el('span', 'lib-card-body');
  body.appendChild(el('span', 'lib-card-name', name));
  body.appendChild(el('span', 'lib-card-count', item.count + ' 枚'));
  main.appendChild(body);
  return card;
}
function countrySel(c){ return {kind: 'country', country: c.id, label: c.name}; }
function regionSel(c, r){ return {kind: 'region', country: c.id, region: r.id, pref: r.pref || '', label: c.name + ' ' + r.name}; }
function citySel(c, r, city){ return {kind: 'city', country: c.id, region: r.id, pref: r.pref || '', city, label: c.name + ' ' + r.name + ' ' + city}; }
function regionUnknownSel(c){ return {kind: 'region_unknown', country: c.id, label: c.name + ' 地域不明'}; }
function legacyPrefSel(pref, city){ return {kind: 'legacy_pref', pref, city: city || '', label: city ? pref + ' ' + city : pref}; }
function countryCard(c){
  const open = ls.openCountry === c.id;
  const card = placeItemCard('lib-country' + (open ? ' has-regions' : ''), c.id, c.name, c, () => {
    selectPlace(countrySel(c), c);
    focusPlaceCard(c.id);          // 地域の展開で一覧を作り直すので、押したボタンへフォーカスを戻す
  });
  mainOf(card).setAttribute('aria-expanded', open ? 'true' : 'false');
  return card;
}
function mainOf(card){ return card.querySelector('.lib-card-main'); }
function regionCard(c, r){
  return placeItemCard('lib-region', regionKey(c.id, r.id), r.name, r, () => selectPlace(regionSel(c, r), r));
}
function focusPlaceCard(key){
  const node = Array.from($('#lib-place-cards').querySelectorAll('[data-key]')).find(n => n.dataset.key === key);
  const main = node && mainOf(node);
  if(main && main.focus) main.focus();
}
/* 日本の地域（pref を持つ）の市区町村のチップ（旧 prefs の同名の行の cities。件数の多い順に最大 30。残りは選べない「ほか N」の行）。
   city が空のものは件数だけを出して押せない（サーバーは空の city で絞れない）。boundary_only が 1 以上なら「市区町村未取得 N」（絞り込みはない）。
   出すものが無ければ null */
function buildCities(c, r){
  if(!r.pref) return null;
  const row = ls.prefs.find(x => x.pref === r.pref);
  const cities = ((row && row.cities) || []).slice().sort((a, b) => b.count - a.count);
  const missing = Number(r.boundary_only) || 0;
  if(!cities.length && missing <= 0) return null;
  const box = el('div', 'lib-cities');
  box.setAttribute('role', 'group');
  box.setAttribute('aria-label', r.pref + 'の市区町村');
  cities.slice(0, LIB_CITY_MAX).forEach(city => {
    const chip = el('button', 'chip lib-city', city.city || '（市区町村なし）');
    chip.type = 'button';
    chip.appendChild(el('span', 'n', String(city.count)));
    if(city.city){
      chip.dataset.key = regionKey(c.id, r.id) + '/' + city.city;
      chip.setAttribute('aria-pressed', 'false');
      chip.addEventListener('click', () => selectPlace(citySel(c, r, city.city), {bbox: city.bbox, fit_bbox: city.bbox}));
    }else{
      chip.disabled = true;
    }
    box.appendChild(chip);
  });
  if(cities.length > LIB_CITY_MAX){
    box.appendChild(el('span', 'lib-more', 'ほか ' + (cities.length - LIB_CITY_MAX) + '（上の欄で探せます）'));
  }
  if(missing > 0) box.appendChild(el('span', 'lib-more lib-missing', '市区町村未取得 ' + missing));
  return box;
}
function findRegion(countryId, regionId){
  const c = ls.countries.find(x => x.id === countryId);
  const r = c && (c.regions || []).find(x => x.id === regionId);
  return r ? {c, r} : null;
}
/* 選んでいる地域（地域・市区町村のどちらを選んでいても）のカードにだけチップを付ける。選択のたびには作り直さない（押したチップのフォーカスを失わないため） */
function paintCities(){
  const S = ls.sel.places;
  const open = S && (S.kind === 'region' || S.kind === 'city') ? regionKey(S.country, S.region) : null;
  $('#lib-place-cards').querySelectorAll('.lib-region').forEach(card => {
    const box = card.querySelector('.lib-cities');
    if(open !== null && card.dataset.key === open){
      const found = box ? null : findRegion(S.country, S.region);
      const built = found ? buildCities(found.c, found.r) : null;
      if(built) card.appendChild(built);
      card.classList.toggle('has-cities', !!box || !!built);
    }else{
      card.classList.remove('has-cities');
      if(box) box.remove();
    }
  });
}
/* 左の列の上の短い案内: 分類の表がない旧 DB／処理待ちの枚数（期間・範囲は掛けない） */
function paintPlaceStatus(){
  let text = '';
  if(ls.loaded.placeCards){
    if(!ls.classif.available) text = '国・地域の分類はまだ始まっていません';
    else if(ls.classif.pending > 0) text = '処理待ち ' + ls.classif.pending + ' 枚';
  }
  $('#lib-place-status').textContent = text;
}
function renderPlaces(){
  const box = $('#lib-place-cards');
  box.textContent = '';
  ls.countries.forEach(c => {
    box.appendChild(countryCard(c));
    if(ls.openCountry !== c.id) return;
    (c.regions || []).forEach(r => box.appendChild(regionCard(c, r)));
    const ru = c.region_unknown;
    if(ru && ru.count > 0){
      box.appendChild(placeItemCard('lib-region', regionKey(c.id, LIB_REGION_UNKNOWN), '地域不明', ru, () => selectPlace(regionUnknownSel(c), ru)));
    }
  });
  [['unclassified', '地域未判定'], ['no_location', '場所不明']].forEach(([kind, name]) => {
    const u = ls.unknown[kind];
    if(u && u.count > 0) box.appendChild(placeItemCard('lib-unknown', kind, name, u, () => selectPlace({kind, label: name}, u)));
  });
  if(!box.children.length){
    box.appendChild(el('div', 'empty', ls.mapBbox ? '地図の範囲に写真がありません' : '写真がありません'));
  }
  paintCities();
  paintBox('places');
  paintPlaceStatus();
  updateLibNote();
}
/* カード・チップを押す: 選択（もう一度押すと外す）と、選んだものの範囲への地図の移動。国・地域を選ぶと、その国の地域を展開する（外しても閉じない）。
   item は {bbox, fit_bbox}（カードの応答の項目）。fit_bbox に合わせて動かし、fit_bbox が無い（日付変更線をまたぐなど）が位置のある写真はあるときは動かさず案内を出す */
function selectPlace(S, item){
  const cur = ls.sel.places;
  const off = !!cur && selId(cur) === selId(S);
  ls.fitPoints = false;
  const expand = !off && !!S.country && ls.openCountry !== S.country;
  if(!off && S.country) ls.openCountry = S.country;
  applySel(off ? null : S);
  if(expand) renderPlaces();
  if(!off) fitSelected(S, item);
}
/* 地図の合わせ先として使える範囲か（4 つの数が有限・緯度経度の範囲内・最小 ≤ 最大・経度の幅が 180 度以内）。使えなければ世界全体へ飛ばさない */
function validFitBox(b){
  return Array.isArray(b) && b.length === 4 && b.every(v => typeof v === 'number' && Number.isFinite(v))
    && b[0] >= -180 && b[2] <= 180 && b[1] >= -90 && b[3] <= 90 && b[0] <= b[2] && b[1] <= b[3] && b[2] - b[0] <= 180;
}
function fitSelected(S, item){
  setMapHint(null);              // 前の選択の案内は残さない。合わせられない（下）ときだけ出し直す
  if(S.kind === 'no_location' || !item) return;
  if(validFitBox(item.fit_bbox)){ fitMapTo(item.fit_bbox); return; }
  if(item.bbox || item.fit_bbox) setMapHint(LIB_FIT_HINT, S);     // 位置のある写真はあるが、自動では合わせられない。地図は今の位置のまま
}
/* 案内は、それを出した選択（selId）が今の選択のあいだだけ出す（除外の切り替えなど、選択も地図の位置も変わらない再読み込みでは消さない） */
function setMapHint(text, S){
  ls.mapHint = text && S ? {sel: selId(S), text} : null;
  paintMapHint();
}
function paintMapHint(){
  const S = ls.sel.places;
  $('#lib-map-hint').textContent = ls.mapHint && ls.view === 'places' && S && selId(S) === ls.mapHint.sel ? ls.mapHint.text : '';
}
function placeParams(withBbox){
  const p = {hidden: ls.includeHidden};
  if(ls.period){ p.from = ls.period.from; p.to = ls.period.to; }
  if(withBbox && ls.mapBbox) p.bbox = ls.mapBbox.join(',');
  return p;
}
/* 応答の国の配列（id と名前が文字列のものだけ。regions は配列に直す） */
function readCountries(list){
  return (Array.isArray(list) ? list : []).filter(c => c && typeof c.id === 'string' && typeof c.name === 'string')
    .map(c => Object.assign({}, c, {regions: (Array.isArray(c.regions) ? c.regions : []).filter(r => r && typeof r.id === 'string' && typeof r.name === 'string')}));
}
/* 国・地域のカード用の /places（period・hidden・mapBbox を掛ける）。afterRender は、この読み込みが最新のまま描画できたあとに呼ぶ */
function loadPlaceCards(afterRender){
  const seq = ++ls.seq.placeCards;
  libGet('places', placeParams(true)).then(d => {
    if(seq !== ls.seq.placeCards) return;
    ls.prefs = d.prefs || [];
    ls.countries = readCountries(d.countries);
    ls.unknown = d.unknown || {};
    ls.classif = {available: d.classification_available !== false, pending: Number(d.classification_pending) || 0};
    ls.loaded.placeCards = true;
    // 初回（期間・地図の絞り込み・選択がなければ）は日本の地域を展開する。展開するだけで、写真や地図は選ばない
    if(ls.openCountry === undefined && !ls.period && !ls.mapBbox && !ls.sel.places && ls.countries.some(c => c.id === LIB_JAPAN_ID)){
      ls.openCountry = LIB_JAPAN_ID;
    }
    renderPlaces();
    if(afterRender) afterRender();
  }).catch(e => { if(seq === ls.seq.placeCards) toast('場所の一覧を読めませんでした: ' + e.message, true); });
}
/* コンボボックスの候補用の /places（period・hidden だけ。地図の絞り込みは掛けない）。開いているリストは新しい候補で作り直す */
function loadPlaceOptions(){
  const seq = ++ls.seq.placeOptions;
  libGet('places', placeParams(false)).then(d => {
    if(seq !== ls.seq.placeOptions) return;
    ls.placeOpts = {prefs: d.prefs || [], countries: readCountries(d.countries)};
    ls.loaded.placeOptions = true;
    if(ls.combo.open) refreshCombo();
  }).catch(e => { if(seq === ls.seq.placeOptions) toast('場所の候補を読めませんでした: ' + e.message, true); });
}
/* ---- 場所のコンボボックス（#lib-place-combo。WAI-ARIA の combobox・リストの自動補完） ----
   候補は ls.placeOpts（期間・除外だけを掛けた /places）の国・地域（countries）と日本の市区町村（旧 prefs の city。空は除く）、対応表にない旧地名。
   入力と名前を NFKC・小文字・カタカナ→ひらがなに直して、国・地域の表示名と aliases（日本語・英語・原表記）の部分一致か、日本の都道府県の読み
   （ひらがな・ローマ字。県・府・都を除いた形も）の部分一致。並びは 国 → 地域 → 市区町村、それぞれ件数の多い順で上位 20 件、
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
/* 入力 text に合う候補の表 {items, more}。text が空なら全部（↓ で開いたとき）。並びは 国 → 地域 → 市区町村（日本の旧 prefs の city）で、
   それぞれ件数の多い順（同数は名前順）、全体で上位 LIB_OPT_MAX 件。
   国・地域は表示名と aliases（日本語・英語・原表記）のどれかに、日本の地域はさらに都道府県の読み（ひらがな・ローマ字）に当たる。入力と名前は foldText で比べる。
   候補 {kind, label, text, count, …}: country は {country}、region は {country, region, pref}、city は {pref, city, bbox, legacy}、
   対応表にない旧地名は {kind:'pref'|'city', legacy:true, pref, city, bbox}（国・地域の候補は旧 prefs に出ない pref を持たない）。text は決めたあと入力欄に入れる名前。
   同じ名前が別の国の地域・国にもある地域は、国名を付けて見分ける（label に「<国名> <地域名>」。名前の重なりは入力に関係なく候補全体で決める） */
function comboRows(text){
  const f = foldText(text.trim());
  const byCount = (a, b) => b.count - a.count || (a.sort < b.sort ? -1 : a.sort > b.sort ? 1 : 0);
  const hit = names => !f || names.some(n => typeof n === 'string' && n && foldText(n).includes(f));
  const countries = [], regions = [], cities = [];
  // 日本の地域になる pref（対応表にある都道府県）。これに無い pref は旧地名（legacy）で、地域のカードに出ず、旧形式の pref= で選ぶ
  const known = new Set();
  const uses = new Map();
  const use = name => { const k = foldText(name); uses.set(k, (uses.get(k) || 0) + 1); };
  const list = ls.placeOpts.countries || [];
  list.forEach(c => { use(c.name); (c.regions || []).forEach(r => { use(r.name); if(r.pref) known.add(r.pref); }); });
  list.forEach(c => {
    if(hit([c.name].concat(c.aliases || []))){
      countries.push({kind: 'country', country: c.id, count: c.count, legacy: false, sort: c.name, text: c.name,
                      label: c.name + '（' + c.count + '）'});
    }
    (c.regions || []).forEach(r => {
      if(!(hit([r.name].concat(r.aliases || [])) || (r.pref && prefReadingMatch(r.pref, f)))) return;
      const name = uses.get(foldText(r.name)) > 1 ? c.name + ' ' + r.name : r.name;
      regions.push({kind: 'region', country: c.id, region: r.id, pref: r.pref || '', count: r.count, legacy: false, sort: name, text: r.name,
                    label: name + '（' + r.count + '）'});
    });
  });
  (ls.placeOpts.prefs || []).forEach(p => {
    if(!p.pref) return;
    const legacy = !known.has(p.pref);
    if(legacy && (!f || foldText(p.pref).includes(f) || prefReadingMatch(p.pref, f))){
      regions.push({kind: 'pref', pref: p.pref, city: '', count: p.count, bbox: p.bbox || null, legacy, sort: p.pref, text: p.pref,
                    label: p.pref + '（' + p.count + '）'});
    }
    (p.cities || []).forEach(c => {
      if(!c.city || (f && !foldText(c.city).includes(f))) return;
      cities.push({kind: 'city', pref: p.pref, city: c.city, count: c.count, bbox: c.bbox || null, legacy, sort: p.pref + ' ' + c.city, text: c.city,
                   label: p.pref + ' ' + c.city + '（' + c.count + '）'});
    });
  });
  const all = countries.sort(byCount).concat(regions.sort(byCount), cities.sort(byCount));
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
/* 候補を決める。国・地域・市区町村のカードを選んだのと同じ動き（selectPlace）。ただし同じものを選んでいるときは外さず、地図を動かすだけ。
   対応表にある都道府県は日本の国 ID・地域 ID の選択（カードと同じ）、対応表にない旧地名（legacy 印）は変更前と同じ旧形式の選択（legacy_pref）にする */
function chooseComboItem(it){
  closeCombo();
  $('#lib-place-combo').value = it.text;
  choosePlace(it);
}
/* 候補に当たるカードの選択 {S, item}（カード用の応答 ls.countries から。ID で引く）。市区町村は日本の地域の pref で引く。見つからなければ null */
function cardChoice(it){
  if(it.kind === 'country'){
    const c = ls.countries.find(x => x.id === it.country);
    return c ? {S: countrySel(c), item: c} : null;
  }
  if(it.kind === 'region'){
    const found = findRegion(it.country, it.region);
    return found ? {S: regionSel(found.c, found.r), item: found.r} : null;
  }
  for(const c of ls.countries){
    const r = (c.regions || []).find(x => x.pref === it.pref);
    if(!r) continue;
    const row = ls.prefs.find(x => x.pref === it.pref);
    const city = row && (row.cities || []).find(x => x.city === it.city);
    return city ? {S: citySel(c, r, it.city), item: {bbox: city.bbox, fit_bbox: city.bbox}} : null;
  }
  return null;
}
/* 旧地名の候補の選択と地図の合わせ先（候補が持つ旧 prefs の bbox。カードの応答には無いので、読み直しは要らない） */
function legacyChoice(it){
  return {S: legacyPrefSel(it.pref, it.kind === 'city' ? it.city : ''), item: {bbox: it.bbox, fit_bbox: it.bbox}};
}
/* カードに（city なら地域のカードの市区町村にも）出ていない候補は、地図の範囲による絞り込みを外してカードを読み直してから選ぶ
   （市区町村のチップはカード用の ls.prefs から作るので、読み直したあとに出る）。読み直しの間に別の操作があれば選ばない
   （カード・月などの選択は gen を進め、候補を決め直すと seq.choose が進む。どちらが変わっても保留中の選択は捨てる） */
function applyChoice(found){
  const cur = ls.sel.places;
  if(cur && selId(cur) === selId(found.S)){
    if(validFitBox(found.item.fit_bbox)){ setMapHint(null); fitMapTo(found.item.fit_bbox); }     // 選び直しで合わせられるようになったら、前の案内は消す
  }else selectPlace(found.S, found.item);
}
function choosePlace(it){
  const cseq = ++ls.seq.choose;
  ls.seq.cluster++;            // 同じ候補を選び直して選択が変わらないとき（地図を動かすだけ）も、待っているクラスタの展開は捨てる
  if(it.legacy){ applyChoice(legacyChoice(it)); return; }       // 旧地名はカードに出ない。カードの読み直しも待たない
  const done = () => {
    const found = cardChoice(it);
    if(found) applyChoice(found);
  };
  const shown = !!cardChoice(it);
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
/* 詳細の見出しの「選択を解除」: 今の view の選択を外して詳細を閉じる（どの種類の選択でも）。旧地名の選択（legacy_pref）はカードが無く、
   同じ候補を選び直しても外れないので、これで外す。場所では入力欄の文字も空にする。詳細が消えるので、フォーカスは今の view の切り替えボタンへ */
function clearSelection(){
  if(!ls.sel[ls.view]) return;
  ls.fitPoints = false;
  if(ls.view === 'places'){
    $('#lib-place-combo').value = '';
    closeCombo();
    ls.seq.choose++;           // 読み直しを待っている候補の選択も捨てる
    ls.seq.cluster++;
  }
  applySel(null);
  const seg = $('#lib-view-' + ls.view);
  if(seg && seg.focus) seg.focus();
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
  if(ls.sel.places && ls.sel.places.kind === 'period'){ ls.sel.places = null; ls.mapHint = null; }
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
    case 'country': p.country = S.country; break;
    case 'region': p.country = S.country; p.region = S.region; break;
    case 'city': p.country = S.country; p.region = S.region; p.city = S.city; break;      // 日本の地域の市区町村だけ（pref = 地域の都道府県 AND city）
    case 'region_unknown': p.country = S.country; p.region_unknown = true; break;
    case 'legacy_pref': p.pref = S.pref; if(S.city) p.city = S.city; break;        // 旧形式（変更前と同じ）。country・region とは混ぜない
    case 'unclassified': p.unclassified = true; break;
    case 'no_location': p.no_location = true; break;
    case 'bbox': p.bbox = S.bbox.join(','); break;
    case 'period': break;                          // 期間（下で掛ける）の写真全体
    case 'q': p.q = S.q; break;                    // 旅の文字検索
    default: p.from = S.from; p.to = S.to;      // trip・year・month・span・words
  }
  // 場所の画面の期間は、国・地域・市区町村・地域未判定・場所不明・範囲・期間のすべてに掛かる（旅の「地図で見る」）
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
   変えたときも、自分の格子の同じ写真に同じ値を入れてタイルを描き直す。開いている吹き出しの 1 枚も同じ（位置が変わったら
   地名・国と地域の行を描き直す。patch の location_region: null が古い国・地域を消す） */
function libPhotoChanged(id, patch){
  ls.items.forEach((photo, i) => {
    if(String(photo.id) !== String(id)) return;
    Object.assign(photo, patch);
    if(ls.tiles[i]) paintTile(ls.tiles[i], photo);
  });
  if(ls.popupInfo && String(ls.popupInfo.photo.id) === String(id)){
    Object.assign(ls.popupInfo.photo, patch);
    paintPopupInfo();
  }

  // 位置・除外が変わると地図の点が変わる（★は点に出ない）。飛んでいる点の応答は捨て、地図が見えていれば取り直し、
  // 見えていない（場所以外の view、別のタブ）なら印だけ立てて、地図に戻るとき（setLibView・openLibraryTab）に読む
  if(patch && ('lat' in patch || 'lon' in patch || 'hidden' in patch)){
    if(mapVisible()) loadPoints();
    else{ ls.seq.points++; ls.pointsStale = true; }
  }
  // 位置が変わると国・地域の件数（カード・候補・旅の地名）も変わる（その写真は次の分類まで「地域未判定」）。
  // ライブラリが見えていれば今の view の一覧を読み直し、見えていなければ読み込み済みの印を落とす（開くときに読む。飛んでいる応答は捨てる）
  if(patch && ('lat' in patch || 'lon' in patch)) refreshRegionLists();
}
/* 位置の変更のあと、国・地域に由来する一覧（旅の地名・場所のカードと候補）を新しくする */
function refreshRegionLists(){
  const visible = !$('#v-library').classList.contains('hidden');
  ls.seq.trips++; ls.seq.placeCards++; ls.seq.placeOptions++;
  ls.loaded.trips = false; ls.loaded.placeCards = false; ls.loaded.placeOptions = false;
  if(!visible) return;
  if(ls.view === 'trips') loadTrips();
  else if(ls.view === 'places'){ loadPlaceCards(); loadPlaceOptions(); }
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
/* 今の範囲と同じなら動かさない（bbox が null＝位置のある写真が無いときも、緯度経度の範囲外・数でない・最小が最大を超える範囲も動かさない） */
function fitMapTo(bbox){
  if(!Array.isArray(bbox) || bbox.length !== 4 || !bbox.every(v => typeof v === 'number' && Number.isFinite(v))
     || bbox[0] < -180 || bbox[2] > 180 || bbox[1] < -90 || bbox[3] > 90 || bbox[0] > bbox[2] || bbox[1] > bbox[3]) return;
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
/* 利用者が地図を動かし始めた（ドラッグ・ホイール・ピンチ・キーボード・ズームボタン。movestart に originalEvent が載る）: 待っているクラスタの展開は古い。
   クラスタの展開自身の移動（libUser 付き）の開始では捨てない。過去の展開の moveend も捨てない（あとから押した別のクラスタの応答を消さないため） */
function onMapMoveStart(e){
  if(e && !e.libMove && e.originalEvent) ls.seq.cluster++;
}
function onMapMoveEnd(e){
  if(!isUserMove(e)) return;
  if(!mapVisible()) return;
  ls.fitPoints = false;
  setMapHint('');              // 「地図を移動して確認できます」は、利用者が動かしたら役目を終える
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
  map.on('movestart', onMapMoveStart);
  // 初めての単発のホイールは、MapLibre 4.7.1 が 40ms の判別を待ってから originalEvent の無い movestart を起こすので、ホイールそのものでも待っているクラスタの展開を捨てる
  map.on('wheel', () => { ls.seq.cluster++; });
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
    // 応答を待つあいだに別の場所を選んだ・全体に戻した・期間を変えた・利用者が地図を動かし始めた・点を読み直したなら、古いクラスタへは寄らない
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
/* 吹き出しの中身のうち、位置が変わると古くなる 2 行（地名・国と地域）。ls.popupInfo は開いている吹き出しの {photo, place, region}（無ければ null） */
function paintPopupInfo(){
  const info = ls.popupInfo;
  if(!info) return;
  info.place.textContent = l1Of(info.photo).text;
  info.region.textContent = '';
  info.region.classList.toggle('hidden', !info.photo.location_region);
  const r = info.photo.location_region;
  if(!r) return;
  info.region.appendChild(el('span', 'region-label', r.label));
  if(r.method === 'inside') info.region.appendChild(el('span', 'region-note', REGION_INSIDE_NOTE));
}
function closePopup(){
  if(ls.popup){ ls.popup.remove(); ls.popup = null; }
  ls.popupInfo = null;
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
    ls.popupInfo = {photo, place: el('div', 'lib-pop-place'), region: el('div', 'lib-pop-region hidden')};
    body.appendChild(ls.popupInfo.place);
    body.appendChild(ls.popupInfo.region);
    paintPopupInfo();
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
  paintMapHint();
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
  $('#lib-sel-clear').addEventListener('click', clearSelection);
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
