// admin.js・photos.js・library.js（この順に同じ context へ読む）のライブラリタブ — 旅・日付のカード（選ぶと下の詳細に写真の格子）・
// 年の月のチップ・除外の切り替え・古い応答の捨て方・ビューア・★の反映・場所（都道府県のカード・市区町村のチップ・地図（偽の MapLibre）・
// 地図の移動の理由・期間・「この範囲の写真を見る」「全体に戻す」）・吹き出し — を、fakedom.mjs の DOM の代用品と fetch・MapLibre の代用品で動かして確かめる。
// 使い方: node web/tests/library_render.mjs <admin.js のパス> <photos.js のパス> <library.js のパス>
// fetch は URL ごとの合成 JSON を返すだけで、ネットワークには出ない。座標・地名はすべて架空の値。
import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom} from './fakedom.mjs';
import {createFakeMaplibre} from './fake_maplibre.mjs';

const [adminPath, photosPath, libraryPath] = process.argv.slice(2);
assert.ok(adminPath && photosPath && libraryPath, 'admin.js・photos.js・library.js のパスが要る');
const sources = [[adminPath, 'admin.js'], [photosPath, 'photos.js'], [libraryPath, 'library.js']]
  .map(([p, name]) => [fs.readFileSync(p, 'utf8'), name]);
const loadScripts = context => sources.forEach(([text, name]) => vm.runInContext(text, context, {filename: name}));

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const respond = (status, body) =>
  Promise.resolve({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body)});
const reply = body => respond(200, body);

// ---- 合成データ（架空の県・市・座標） ----
const statusBody = () => ({
  ok: true, server_time: '2030-01-01 10:00:00',
  sync: {ready: 10, pending: 0, total: 10},
  db: {no_loc: 1, no_display: 0, favorite: 0, hidden: 0, enrich_pending: 0, no_time: 0, by_loc_source: {exif: 9},
       infer_buckets: {}, by_source: {drive: 10},
       filters: {all: 10, noloc: 1, suggest: 0, inferred: 0, manual: 0, fav: 0, hidden: 0}},
  disk: {used_pct: 10, free_bytes: 1e11, total_bytes: 2e11,
         dirs: {derivatives: {bytes: 1e6, files: 10}, tile_cache: {bytes: 1e3, files: 1}}},
});
const TRIP_A = {key: '2030-05-03 09:12', start: '2030-05-03 09:12', end: '2030-05-05 18:40', count: 212, noloc: 7,
  places: [{pref: '架空県', city: '試験市', count: 150}, {pref: '', city: '', count: 40}],
  cover: {id: 501, thumb_url: '/thumb/501.jpg?v=aaaaaaaaaaaa'}};
const TRIP_B = {key: '2029-01-01 00:00', start: '2029-01-01 00:00', end: '2029-01-01 00:00', count: 1, noloc: 1,
  places: [], cover: null};
const tripsBody = () => ({ok: true, trips: [TRIP_A, TRIP_B], untimed: 12});
const calendarBody = () => ({ok: true, untimed: 5, years: [
  {year: '2030', count: 30, cover: {id: 601, thumb_url: '/thumb/601.jpg?v=cccccccccccc'},
   months: [{month: '05', count: 20}, {month: '12', count: 10}]},
  {year: '2029', count: 4, cover: null, months: [{month: '01', count: 4}]}]});
const placesBody = () => ({ok: true, abroad: 9, prefs: [
  {pref: '架空県', count: 50, cover: {id: 701, thumb_url: '/thumb/701.jpg?v=dddddddddddd'}, bbox: [130, 30, 132, 32],
   cities: [{city: '見本町', count: 8, bbox: [130.5, 30.5, 130.6, 30.6]}, {city: '試験市', count: 40, bbox: [131, 31, 131.5, 31.5]},
            {city: '', count: 2, bbox: null}]},
  {pref: '試験県', count: 3, cover: null, bbox: null, cities: [{city: 'サンプル市', count: 3, bbox: null}]}]});

// 旧形式の合成応答（prefs・abroad だけ）に、新しい応答の国・地域・未判定を足す。実 API と同じ分類にする:
// 日本の地域になるのは対応表（PREF_TABLE）にある pref だけで、表にない旧地名（表記揺れ）は日本の region_unknown に入り、countries[].regions[] には現れない
// （prefs の行には残る。旧形式 pref=<名前> でだけ引ける）。abroad は「地域未判定」にする。新しい項目を最初から持つ応答（countries のあるもの）はそのまま
const PREF_TABLE = new Set(('北海道 青森県 岩手県 宮城県 秋田県 山形県 福島県 茨城県 栃木県 群馬県 埼玉県 千葉県 東京都 神奈川県 新潟県 富山県 石川県 福井県 山梨県 長野県 '
  + '岐阜県 静岡県 愛知県 三重県 滋賀県 京都府 大阪府 兵庫県 奈良県 和歌山県 鳥取県 島根県 岡山県 広島県 山口県 徳島県 香川県 愛媛県 高知県 福岡県 佐賀県 長崎県 熊本県 大分県 宮崎県 鹿児島県 沖縄県').split(' '));
// 試験用に表へ足した架空の県（地名・座標はすべて架空。実際の表にはない。これらを日本の地域として扱う試験が多いので、表にあるものとして置く）
['架空県', '試験県', '境界県', '多県', 'カード甲', 'カード乙', '候補だけ', '候補新', '新しい範囲', 'カード新'].forEach(n => PREF_TABLE.add(n));
const jpId = pref => 'ne:JPN-' + [...pref].map(ch => ch.codePointAt(0).toString(16)).join('');
const union = boxes => {
  const b = boxes.filter(Boolean);
  return b.length ? [Math.min(...b.map(x => x[0])), Math.min(...b.map(x => x[1])), Math.max(...b.map(x => x[2])), Math.max(...b.map(x => x[3]))] : null;
};
const noneItem = () => ({count: 0, cover: null, bbox: null, fit_bbox: null});
function withCountries(body){
  if(!body || body.ok !== true || body.countries) return body;
  const prefs = (body.prefs || []).filter(p => p.pref);
  const known = prefs.filter(p => PREF_TABLE.has(p.pref));
  const legacy = prefs.filter(p => !PREF_TABLE.has(p.pref));
  const regions = known.map(p => ({id: jpId(p.pref), name: p.pref, aliases: [p.pref], count: p.count, cover: p.cover,
    bbox: p.bbox, fit_bbox: p.bbox, pref: p.pref, boundary_only: 0}));
  const ruBox = union(legacy.map(p => p.bbox));
  const regionUnknown = legacy.length
    ? {count: legacy.reduce((a, p) => a + p.count, 0), cover: legacy.find(p => p.cover)?.cover || null, bbox: ruBox, fit_bbox: ruBox}
    : noneItem();
  const box = union(prefs.map(p => p.bbox));
  const all = regions.concat(legacy.map(p => ({count: p.count, cover: p.cover})));
  const countries = prefs.length ? [{id: 'ne:JPN', name: '日本', aliases: ['日本', 'Japan'], count: all.reduce((a, r) => a + r.count, 0),
    cover: all.find(r => r.cover)?.cover || null, bbox: box, fit_bbox: box, regions, region_unknown: regionUnknown}] : [];
  const abroad = body.abroad || 0;
  return {...body, countries, classification_available: true, classification_pending: 0,
    unknown: {unclassified: abroad ? {count: abroad, cover: null, bbox: null, fit_bbox: null} : noneItem(), no_location: noneItem()}};
}

const lp = o => ({name: 'lib.jpg', thumb: '/derivatives/lib.jpg', thumb_url: '', taken_at: '2030-05-03 09:12',
  lat: 10.0, lon: 20.0, loc_source: 'exif', place: '', pref: '架空県', city: '試験市',
  favorite: false, hidden: false, rotation: 0, source: 'drive', ...o});
const P501 = lp({id: 501, name: 'a.jpg', thumb_url: '/thumb/501.jpg?v=aaaaaaaaaaaa'});
const P502 = lp({id: 502, name: 'b.jpg', thumb_url: '/thumb/502.jpg?v=bbbbbbbbbbbb', lat: null, lon: null,
  loc_source: 'none', pref: '', city: '', taken_at: '2030-05-03 09:20'});
const P503 = lp({id: 503, name: 'c.jpg', thumb: '', thumb_url: '', taken_at: '2030-05-03 10:00', favorite: true});
const GRID = [P501, P502, P503];
const feature = (id, lon, lat) => ({type: 'Feature', geometry: {type: 'Point', coordinates: [lon, lat]}, properties: {id}});
const pointsBody = (extra = {}) => ({ok: true, truncated: false, total: 2, type: 'FeatureCollection',
  features: [feature(501, 20.0, 10.0), feature(503, 20.5, 10.5)], ...extra});
// 写真タブの一覧（ライブラリの写真とは別の id。ps.entries が変わらないことの確認用）
const TAB_PHOTOS = [1, 2].map(id => ({id, name: 't' + id + '.jpg', thumb: '/derivatives/t' + id + '.jpg',
  taken_at: '2030-01-0' + id + ' 12:00:00', lat: 11.0, lon: 21.0, loc_source: 'exif', infer_bucket: '', place: '',
  pref: '架空県', city: '試験市', fav: false, hidden: false, rotation: 0, source: 'drive'}));

function makeEnv({gridItems = GRID, pathname = '/admin'} = {}){
  // requestAnimationFrame は試験が flush するまで溜める（描画の確定後に呼ぶ処理）。matchMedia は幅 640px の境の変化を試験から起こす
  const rafQueue = [];
  const media = {matches: false, handlers: []};
  const document = buildAdminDom();
  const calls = [];
  const flags = {};
  const handlers = {photos: null, points: null, trips: null, calendar: null, places: null, when: null, tabList: null, search: null};   // (params, rec) => Promise | undefined
  const fake = createFakeMaplibre();
  const fetchFake = (url, opts = {}) => {
    const rec = {url, method: opts.method || 'GET', body: opts.body};
    calls.push(rec);
    if(calls.length > 5000) throw new Error('fetch が 5000 回を超えた（読み直しのループ）: ' + url);   // settle() は setImmediate で待つだけ
    const params = url.includes('?') ? new URLSearchParams(url.split('?')[1]) : new URLSearchParams();
    if(url === '/api/admin/status') return reply(statusBody());
    if(url === '/api/ai/status') return reply({ok: true, photos: 1, eligible: 1});
    if(url.startsWith('/api/ai/search?') && handlers.search) return handlers.search(params, rec);
    if(url.startsWith('/api/admin/photos?')){
      if(handlers.tabList) return handlers.tabList(params, rec);
      return reply({ok: true, page: 1, per: 50, pages: 1, total: TAB_PHOTOS.length, filter: 'all', items: TAB_PHOTOS});
    }
    if(url.startsWith('/api/admin/library/trips')){
      return (handlers.trips && handlers.trips(params, rec)) || reply(tripsBody());
    }
    if(url.startsWith('/api/admin/library/photos')){
      if(handlers.photos){ const r = handlers.photos(params, rec); if(r) return r; }
      const per = Number(params.get('per')) || 60;
      return reply({ok: true, page: Number(params.get('page')) || 1, per, pages: 1, total: gridItems.length,
                    items: gridItems});
    }
    if(url.startsWith('/api/admin/library/calendar')){
      return (handlers.calendar && handlers.calendar(params, rec)) || reply(calendarBody());
    }
    if(url.startsWith('/api/admin/library/when')){
      return (handlers.when && handlers.when(params, rec)) || reply({ok: false, error: 'unreadable'});
    }
    if(url.startsWith('/api/admin/library/places')){
      const r = (handlers.places && handlers.places(params, rec)) || reply(placesBody());
      return r.then(res => res.json().then(b => respond(res.status, withCountries(b))));
    }
    if(url === '/api/admin/setloc'){
      const b = JSON.parse(opts.body);
      return reply({ok: true, lat: b.lat, lon: b.lon});
    }
    if(url.startsWith('/api/admin/library/points')){
      return (handlers.points && handlers.points(params, rec)) || reply(pointsBody());
    }
    if(url === '/api/admin/setflag'){
      const b = JSON.parse(opts.body);
      const cur = flags[b.id] = flags[b.id] || {favorite: false, hidden: false};
      if(b.favorite !== undefined) cur.favorite = b.favorite;
      if(b.hidden !== undefined) cur.hidden = b.hidden;
      return reply({ok: true, favorite: cur.favorite, hidden: cur.hidden});
    }
    return reply({ok: false, error: 'unexpected ' + url});
  };
  const context = vm.createContext({
    document, fetch: fetchFake, location: {pathname, origin: 'http://localhost'},
    setInterval(){}, setTimeout(){ return 0; }, clearTimeout(){}, console, maplibregl: fake.maplibregl,
    requestAnimationFrame: fn => { rafQueue.push(fn); return rafQueue.length; },
    matchMedia: query => ({media: query, get matches(){ return media.matches; },
                           addEventListener: (type, fn) => { if(type === 'change') media.handlers.push(fn); }}),
  });
  loadScripts(context);
  const reqs = prefix => calls.filter(c => c.url.startsWith(prefix));
  const last = prefix => {
    const c = reqs(prefix).at(-1);
    return c ? new URLSearchParams(c.url.split('?')[1] || '') : null;
  };
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  const ev = code => JSON.parse(vm.runInContext('JSON.stringify(' + code + ')', context));
  const raf = () => { const fns = rafQueue.splice(0); fns.forEach(f => f()); return fns.length; };
  const narrow = on => { media.matches = on; media.handlers.forEach(f => f({matches: on})); };
  return {document, context, calls, handlers, fake, reqs, last, boot, ev, raf, narrow, rafQueue, $: s => document.querySelector(s),
          $$: s => document.querySelectorAll(s)};
}

const hidden = (env, sel) => env.$(sel).classList.contains('hidden');
const openLib = async env => { env.$('.tab[data-tab=library]').click(); await settle(); };
const cards = env => env.$$('#lib-trip-cards .lib-card');
const mainOf = card => card.querySelector('.lib-card-main');
const pressTrip = (env, i = 0) => mainOf(cards(env)[i]).click();
const tiles = env => env.$$('#lib-grid .lib-tile');
const marksOf = tile => tile.querySelectorAll('.lib-mark').map(m => m.textContent);
const activeTabs = env => env.$$('nav .tab').filter(t => t.classList.contains('on')).map(t => t.dataset.tab);
const mapOf = env => env.fake.maps[0];
// vm の中で作られた配列・object は別の realm なので、deepEqual の前に JSON を通して普通の値にする
const plain = v => JSON.parse(JSON.stringify(v));
// ノードの同一性は assert.ok で比べる。assert.equal / strictEqual にノードを渡すと、失敗時の差分作りが文書全体を inspect して
// 時間とメモリを大きく食う（fakedom.mjs の hidden の注記を参照）
const same = (a, b, msg) => assert.ok(a === b, msg || '同じノードのはず');
const notSame = (a, b, msg) => assert.ok(a !== b, msg || '別のノードのはず');

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('library.js を読み込んだだけでは location にも fetch にも触らず、HTML 文字列を組む API も使わない', () => {
  const touched = [];
  const sandbox = {document: buildAdminDom(), setInterval(){}, setTimeout(){}, clearTimeout(){}, console};
  Object.defineProperty(sandbox, 'location', {get(){ touched.push('location'); throw new Error('location'); }});
  Object.defineProperty(sandbox, 'fetch', {get(){ touched.push('fetch'); throw new Error('fetch'); }});
  Object.defineProperty(sandbox, 'maplibregl', {get(){ touched.push('maplibregl'); throw new Error('maplibregl'); }});
  loadScripts(vm.createContext(sandbox));
  assert.deepEqual(touched, []);
  for(const [text, name] of sources){
    assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write|setHTML|srcdoc|createContextualFragment|DOMParser/.test(text), name);
  }
});

await test('(a) 旅のカード: 表紙・期間・枚数（位置なし）・地名のタグ・主ボタン・「地図で見る」・日時なしのカード。カードは非対話の容器で、ボタンを入れ子にしない', async () => {
  const env = makeEnv();
  await env.boot();
  assert.equal(hidden(env, '#v-library'), true, '起動時は状況タブ');
  assert.deepEqual(env.reqs('/api/admin/library/'), [], 'タブを開くまで読まない');
  await openLib(env);
  assert.deepEqual(activeTabs(env), ['library']);
  assert.equal(hidden(env, '#v-library'), false);
  assert.equal(env.reqs('/api/admin/library/trips').length, 1);
  assert.equal(env.reqs('/api/admin/library/trips')[0].url, '/api/admin/library/trips', '既定は除外を含めない');
  const c = cards(env);
  assert.equal(c.length, 3, '旅 2 つ + 日時なし');
  assert.ok(c.every(x => x.tagName.toLowerCase() === 'div' && x.classList.contains('lib-card')));
  assert.equal(c[0].listeners.click, undefined, 'カードの容器は対話しない（click を聞くのは中の主ボタン）');
  const main = mainOf(c[0]);
  assert.equal(main.tagName.toLowerCase(), 'button');
  assert.equal(main.type, 'button');
  assert.equal(main.getAttribute('aria-pressed'), 'false');
  same(main.parentNode, c[0]);
  assert.equal(main.querySelectorAll('button').length, 0, 'ボタンを入れ子にしない');
  const mapBtn = c[0].querySelector('.lib-card-map');
  assert.equal(mapBtn.tagName.toLowerCase(), 'button');
  assert.equal(mapBtn.textContent, '地図で見る');
  same(mapBtn.parentNode, c[0], '「地図で見る」は主ボタンの兄弟');
  assert.equal(main.querySelector('.lib-card-name').textContent, '2030-05-03 09:12 〜 2030-05-05 18:40');
  assert.equal(main.querySelector('.lib-card-count').textContent, '212 枚（位置なし 7）');
  assert.deepEqual(main.querySelectorAll('.tag').map(t => t.textContent), ['架空県 試験市 150', '海外・不明 40']);
  const cover = main.querySelector('.lib-cover img');
  assert.equal(cover.src, '/thumb/501.jpg?v=aaaaaaaaaaaa');
  assert.equal(cover.loading, 'lazy');
  assert.equal(mainOf(c[1]).querySelector('.lib-card-name').textContent, '2029-01-01 00:00', '1 枚だけの旅は期間を 1 つだけ');
  assert.equal(mainOf(c[1]).querySelector('.lib-cover .img-broken').textContent, '画像なし', '表紙が無い旅');
  assert.equal(mainOf(c[2]).querySelector('.lib-card-name').textContent, '日時なし');
  assert.equal(mainOf(c[2]).querySelector('.lib-card-count').textContent, '12 枚');
  assert.equal(c[2].querySelector('.lib-card-map'), null, '日時なしに地図は無い');
  assert.equal(env.$('#lib-note').textContent, '旅 2 件');
  assert.equal(hidden(env, '#lib-trips'), false);
  assert.equal(hidden(env, '#lib-detail'), true, '選ぶまで詳細は出さない');
  assert.equal(hidden(env, '#lib-places'), true);
  assert.equal(env.$('#lib-back'), null, '戻るボタンは無い');
  assert.ok(env.$('#lib-trip-cards').classList.contains('lib-cards'));
  assert.ok(!env.$('#lib-trip-cards').classList.contains('is-strip'), '選択が無いうちは 1 段にしない');
  assert.ok(c.every(x => !x.classList.contains('is-selected') && mainOf(x).getAttribute('aria-pressed') === 'false'));
  // 表紙が読めないときも代わりの表示
  cover.dispatchEvent({type: 'error'});
  assert.equal(main.querySelector('.lib-cover img'), null);
  assert.equal(main.querySelector('.lib-cover .img-broken').textContent, '画像なし');
});
await test('(b) 「除外も含める」: 旅は hidden=1 で読み直し、日時なし 0 枚なら行を出さない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.trips = () => reply({ok: true, trips: [TRIP_B], untimed: 0});
  const box = env.$('#lib-hidden');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.reqs('/api/admin/library/trips').length, 2);
  assert.equal(env.last('/api/admin/library/trips').get('hidden'), '1');
  assert.equal(cards(env).length, 1);
  env.handlers.trips = () => reply({ok: true, trips: [], untimed: 0});
  box.checked = false;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/trips').get('hidden'), null);
  assert.equal(env.$('#lib-trips').textContent, '写真がありません');
});

await test('(c) AC3 旅のカードを押すと、一覧を残したまま #lib-detail に from/to 付きで写真の格子が出る。印・遅延読み込み・画像なしも出る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2030-05-03 09:12');
  assert.equal(q.get('to'), '2030-05-05 18:40');
  assert.equal(q.get('page'), '1');
  assert.equal(q.get('per'), '60');
  assert.equal(q.get('hidden'), null);
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-trips'), false, '一覧は残る');
  assert.equal(cards(env).length, 3);
  assert.ok(env.$('#lib-trip-cards').classList.contains('is-strip'), '選んでいるあいだは 1 段の横スクロール');
  assert.ok(cards(env)[0].classList.contains('is-selected'));
  assert.equal(mainOf(cards(env)[0]).getAttribute('aria-pressed'), 'true');
  assert.equal(env.$('#lib-back'), null);
  assert.equal(env.$('#lib-sel').textContent, '2030-05-03 09:12 〜 2030-05-05 18:40');
  assert.equal(env.$('#lib-pageinfo').textContent, '3件 / 1 of 1ページ');
  assert.equal(env.$('#lib-prev').disabled, true);
  assert.equal(env.$('#lib-next').disabled, true);
  assert.deepEqual(plain(cards(env)[0].scrollCalls), [{block: 'nearest', inline: 'nearest'}], '選んだカードを見える所へ');
  const t = tiles(env);
  assert.equal(t.length, 3);
  assert.deepEqual(t.map(x => x.dataset.id), ['501', '502', '503']);
  const img = t[0].querySelector('img');
  assert.equal(img.src, '/thumb/501.jpg?v=aaaaaaaaaaaa');
  assert.equal(img.loading, 'lazy');
  assert.deepEqual(marksOf(t[0]), []);
  assert.deepEqual(marksOf(t[1]), ['位置なし']);
  assert.deepEqual(marksOf(t[2]), ['★']);
  assert.equal(t[1].querySelector('img').src, '/thumb/502.jpg?v=bbbbbbbbbbbb');
  assert.equal(t[2].querySelector('img'), null, 'thumb_url も thumb も無い写真');
  assert.equal(t[2].querySelector('.img-broken').textContent, '画像なし');
  // 読めない画像
  img.dispatchEvent({type: 'error'});
  assert.equal(t[0].querySelector('img'), null);
  assert.equal(t[0].querySelector('.img-broken').textContent, '画像なし');
});

await test('(c1) 別のカードを押すと選択と格子が切り替わる。もう一度押すと選択を外し、詳細を閉じる。日時なしは untimed=1', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const photoReqs = () => env.reqs('/api/admin/library/photos').length;
  pressTrip(env, 0);
  await settle();
  assert.equal(photoReqs(), 1);
  env.handlers.photos = params => reply({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [params.get('from') === '2029-01-01 00:00' ? P503 : P501]});
  pressTrip(env, 1);
  await settle();
  assert.equal(photoReqs(), 2);
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2029-01-01 00:00');
  assert.equal(q.get('to'), '2029-01-01 00:00');
  assert.deepEqual(cards(env).map(x => x.classList.contains('is-selected')), [false, true, false]);
  assert.deepEqual(cards(env).map(x => mainOf(x).getAttribute('aria-pressed')), ['false', 'true', 'false']);
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['503']);
  assert.equal(env.$('#lib-sel').textContent, '2029-01-01 00:00');
  // AC5: 日時なしのカード
  mainOf(cards(env)[2]).click();
  await settle();
  const u = env.last('/api/admin/library/photos');
  assert.equal(u.get('untimed'), '1');
  assert.equal(u.get('from'), null);
  assert.equal(u.get('to'), null);
  assert.equal(env.$('#lib-sel').textContent, '日時なし');
  assert.deepEqual(cards(env).map(x => x.classList.contains('is-selected')), [false, false, true]);
  // 選んでいるカードをもう一度押すと外す（読み直さない）
  const before = photoReqs();
  mainOf(cards(env)[2]).click();
  await settle();
  assert.equal(photoReqs(), before);
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.ok(!env.$('#lib-trip-cards').classList.contains('is-strip'), '選択が無ければ元の並びに戻る');
  assert.ok(cards(env).every(x => !x.classList.contains('is-selected')));
  assert.equal(env.ev('ls.sel.trips'), null);
});

await test('(c3) 主ボタンはクリック・Enter・Space のどれでも 1 回だけ読む。「地図で見る」を押しても主の選択は起きない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const photoReqs = () => env.reqs('/api/admin/library/photos').length;
  assert.equal(photoReqs(), 0);
  mainOf(cards(env)[0]).click();
  await settle();
  assert.equal(photoReqs(), 1, 'クリック');
  // 本物のブラウザは button の Enter・Space から click を起こす（fakedom も同じ）。keydown でも処理していると二重に読む
  mainOf(cards(env)[1]).dispatchEvent({type: 'keydown', key: 'Enter'});
  await settle();
  assert.equal(photoReqs(), 2, 'Enter');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2029-01-01 00:00');
  mainOf(cards(env)[0]).dispatchEvent({type: 'keydown', key: ' '});
  await settle();
  assert.equal(photoReqs(), 3, 'Space');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2030-05-03 09:12');
  mainOf(cards(env)[1]).dispatchEvent({type: 'keydown', key: 'Tab'});
  await settle();
  assert.equal(photoReqs(), 3, 'Tab では読まない');
  assert.deepEqual(cards(env).map(x => x.classList.contains('is-selected')), [true, false, false]);
  // 「地図で見る」（バブリングするクリックでも）: 場所の画面をその旅の期間で開く。旅の主ボタンの選択は起きない（旅の選択はそのまま）。
  // 下には期間の写真全体（1 回だけ読む）
  const mapBtn = cards(env)[1].querySelector('.lib-card-map');
  mapBtn.dispatchEvent({type: 'click', bubbles: true});
  await settle();
  assert.equal(photoReqs(), 4, '期間の写真を 1 回だけ読む');
  assert.equal(env.ev('ls.sel.trips.key'), '2030-05-03 09:12', '旅の選択はそのまま');
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'period'});
  assert.equal(env.ev('ls.view'), 'places');
  assert.equal(env.last('/api/admin/library/points').get('from'), '2029-01-01 00:00');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2029-01-01 00:00');
});

await test('(c4) AC16a 詳細の応答を待つあいだに別のカードを選ぶと、遅れた応答は捨てる（gen）。view を替えた・除外を切り替えたときも', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const held = {};
  env.handlers.photos = params => {
    const key = params.get('from') + (params.get('hidden') ? '#h' : '');
    if(key === '2030-05-03 09:12') return new Promise(res => { held.A = body => res({ok: true, status: 200, json: () => Promise.resolve(body)}); });
    return reply({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [key.startsWith('2029') ? P503 : P502]});
  };
  const pageOf = body => ({ok: true, page: 1, per: 60, pages: 1, total: body.length, items: body});
  pressTrip(env, 0);                       // A（応答は保留）
  await settle();
  pressTrip(env, 1);                       // B（すぐ応答）
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['503']);
  held.A(pageOf([P501, P502]));            // 遅れて A の応答が届く
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['503'], '遅れた応答は捨てる');
  assert.equal(env.$('#lib-sel').textContent, '2029-01-01 00:00');
  assert.deepEqual(cards(env).map(x => x.classList.contains('is-selected')), [false, true, false]);
  // view を替えると、飛んでいる詳細の応答は捨てる
  pressTrip(env, 0);                       // A をまた保留
  await settle();
  env.$('#lib-view-calendar').click();
  await settle();
  held.A(pageOf([P501]));
  await settle();
  assert.equal(tiles(env).length, 0, '別の view の詳細に旅の写真を出さない');
  assert.equal(hidden(env, '#lib-detail'), true);
  // 戻ると、その view の選択（A）の詳細を読み直す
  env.handlers.photos = null;
  env.$('#lib-view-trips').click();
  await settle();
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502', '503']);
  assert.ok(cards(env)[0].classList.contains('is-selected'));
});
await test('(c2) 格子のページング: 次へ・前へで page を付けて読み直す。古い応答は捨てる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const pageBody = (page, items) => reply({ok: true, page, per: 60, pages: 3, total: 130, items});
  env.handlers.photos = params => pageBody(Number(params.get('page')), params.get('page') === '2' ? [P502] : GRID);
  pressTrip(env, 0);
  await settle();
  assert.equal(env.$('#lib-pageinfo').textContent, '130件 / 1 of 3ページ');
  assert.equal(env.$('#lib-next').disabled, false);
  assert.equal(env.$('#lib-prev').disabled, true);
  env.$('#lib-next').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('page'), '2');
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['502']);
  assert.equal(env.$('#lib-prev').disabled, false);
  // 遅い応答（page 3）が、速く続けて押した page 1 の結果のあとに届いても表示は変わらない
  let slow;
  env.handlers.photos = params => params.get('page') === '3'
    ? new Promise(res => { slow = () => res({ok: true, status: 200, json: () => Promise.resolve(
        {ok: true, page: 3, per: 60, pages: 3, total: 130, items: [P503]})}); })
    : pageBody(1, GRID);
  env.$('#lib-next').click();                    // page 3 を要求（保留）
  env.$('#lib-prev').click();                    // page 2 に戻る要求
  await settle();
  slow();
  await settle();
  assert.notDeepEqual(tiles(env).map(x => x.dataset.id), ['503'], '古い応答は捨てる');
});

await test('(c2b) ページ送りの応答が保留のまま別の旅を選んでも、遅れて届いた前の旅の 2 ページ目は捨てる（B の選択・写真・ページ表示・送りボタンが保たれる）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const pageOf = (page, pages, total, items) => ({ok: true, page, per: 60, pages, total, items});
  let releaseA2;
  env.handlers.photos = params => {
    if(params.get('from') === '2030-05-03 09:12'){            // 旅 A
      if(params.get('page') === '2'){
        return new Promise(res => { releaseA2 = () => res({ok: true, status: 200, json: () => Promise.resolve(pageOf(2, 3, 130, [P502]))}); });
      }
      return reply(pageOf(1, 3, 130, GRID));
    }
    return reply(pageOf(1, 1, 1, [P503]));                    // 旅 B
  };
  pressTrip(env, 0);
  await settle();
  assert.equal(env.$('#lib-pageinfo').textContent, '130件 / 1 of 3ページ');
  env.$('#lib-next').click();                                  // A の 2 ページ目（保留）
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('page'), '2');
  pressTrip(env, 1);                                           // B を選ぶ（1 ページ目がすぐ返る）
  await settle();
  const check = msg => {
    assert.equal(env.$('#lib-sel').textContent, '2029-01-01 00:00', msg);
    assert.deepEqual(tiles(env).map(x => x.dataset.id), ['503'], msg);
    assert.equal(env.$('#lib-pageinfo').textContent, '1件 / 1 of 1ページ', msg);
    assert.equal(env.$('#lib-next').disabled, true, msg);
    assert.equal(env.$('#lib-prev').disabled, true, msg);
    assert.deepEqual(cards(env).map(x => x.classList.contains('is-selected')), [false, true, false], msg);
  };
  check('B の 1 ページ目');
  releaseA2();                                                 // 遅れて A の 2 ページ目が届く
  await settle();
  check('遅れた A の応答は捨てる');
  // その後に B で次へを押せない（A の pages 3 を引きずらない）。押しても読まない
  const before = env.reqs('/api/admin/library/photos').length;
  env.$('#lib-next').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
});
await test('(d) タイルを押すとビューアが開き、★は setflag にその写真の id を送って印が変わり、閉じるとタイルへフォーカスが戻る', async () => {
  const env = makeEnv();
  await env.boot();
  // 写真タブの一覧（別の写真）を先に読んでおく
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const tabBefore = env.ev('ps.entries.map(e => [e.photo.id, e.photo.favorite, e.photo.hidden])');
  assert.deepEqual(tabBefore, [[1, false, false], [2, false, false]]);
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  const t = tiles(env);
  t[1].click();
  assert.equal(hidden(env, '#viewer'), false);
  assert.ok(env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), 1);
  assert.equal(env.$('#viewer-title').textContent, '#502  b.jpg');
  assert.equal(env.$('#viewer-counter').textContent, '2 / 3');
  assert.equal(hidden(env, '#viewer-ai'), true);
  assert.equal(hidden(env, '#viewer-group'), true);
  same(env.document.activeElement, env.$('#viewer-close'));
  // 前後は格子の並び
  env.$('#viewer-next').click();
  assert.equal(env.$('#viewer-title').textContent, '#503  c.jpg');
  env.$('#viewer-prev').click();
  assert.equal(env.$('#viewer-title').textContent, '#502  b.jpg');
  // ★ → setflag の id はこの写真。タイルの印、ビューアの表示が変わる。写真タブの ps.entries は変わらない
  const flagsBefore = env.reqs('/api/admin/setflag').length;
  env.$('#viewer-favorite').click();
  await settle();
  const posts = env.reqs('/api/admin/setflag');
  assert.equal(posts.length, flagsBefore + 1);
  assert.deepEqual(JSON.parse(posts.at(-1).body), {id: 502, favorite: true});
  assert.deepEqual(marksOf(t[1]), ['★', '位置なし']);
  assert.equal(env.$('#viewer-favorite').textContent, '★ favorite');
  assert.deepEqual(env.ev('ps.entries.map(e => [e.photo.id, e.photo.favorite, e.photo.hidden])'), tabBefore);
  // 除外 → 🚫 とタイルの薄い表示
  env.$('#viewer-hidden').click();
  await settle();
  assert.deepEqual(JSON.parse(env.reqs('/api/admin/setflag').at(-1).body), {id: 502, hidden: true});
  assert.deepEqual(marksOf(t[1]), ['★', '🚫', '位置なし']);
  assert.ok(t[1].classList.contains('is-hidden'));
  // 閉じると、開いたタイルにフォーカスが戻る
  env.$('#viewer-close').click();
  assert.equal(hidden(env, '#viewer'), true);
  same(env.document.activeElement, t[1]);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  // Enter でも開ける
  t[0].dispatchEvent({type: 'keydown', key: 'Enter'});
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.$('#viewer-title').textContent, '#501  a.jpg');
});

await test('(d2) 写真タブなど別の画面での変更も、格子のタイルの印に反映される（onPhotoChange）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  vm.runInContext('applyPhoto(501, {favorite: true, hidden: true})', env.context);
  const t = tiles(env);
  assert.deepEqual(marksOf(t[0]), ['★', '🚫']);
  assert.ok(t[0].classList.contains('is-hidden'));
  vm.runInContext('applyPhoto(501, {favorite: false, hidden: false, thumb_url: "/thumb/501.jpg?v=cccccccccccc"})', env.context);
  assert.deepEqual(marksOf(t[0]), []);
  assert.ok(!t[0].classList.contains('is-hidden'));
  assert.equal(t[0].querySelector('img').src, '/thumb/501.jpg?v=cccccccccccc');
  vm.runInContext('applyPhoto(999, {favorite: true})', env.context);        // 格子に無い写真は何も起こさない
  assert.deepEqual(marksOf(t[1]), ['位置なし']);
  assert.deepEqual(marksOf(t[2]), ['★']);
});

await test('(e) 地図ビュー: points を読み、偽 MapLibre の source にその GeoJSON が渡る。クラスタ・単独の点の層とクラスタの件数', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/points').length, 1);
  assert.equal(env.reqs('/api/admin/library/points')[0].url, '/api/admin/library/points', '期間なし・除外を含めない');
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(hidden(env, '#lib-trips'), true);
  assert.deepEqual(activeTabs(env), ['library']);
  assert.ok(env.$('#lib-view-places').classList.contains('on'));
  assert.equal(env.fake.maps.length, 1);
  const map = mapOf(env);
  assert.equal(map.options.container, 'lib-map');
  assert.equal(map.options.style.sources.osm.tiles[0], 'http://localhost/tiles/osm/{z}/{x}/{y}.png');
  assert.deepEqual(plain(map.options.style.layers.map(l => l.id)), ['bg', 'osm', 'gsi']);
  assert.equal(map.controls.length, 1);
  const src = map.sources.photos;
  assert.ok(src, 'photos source');
  assert.equal(src.options.type, 'geojson');
  assert.equal(src.options.cluster, true);
  assert.equal(src.options.clusterRadius, 40);
  assert.equal(src.options.clusterMaxZoom, 16);
  assert.deepEqual(plain(map.layers.map(l => l.id)), ['lib-clusters', 'lib-point']);
  assert.ok(map.layers.every(l => l.source === 'photos'));
  assert.equal(src.setDataCalls.length, 1);
  assert.deepEqual(plain(src.setDataCalls[0]),
    {type: 'FeatureCollection', features: [feature(501, 20.0, 10.0), feature(503, 20.5, 10.5)]});
  assert.equal(env.$('#lib-map-note').textContent, '2 点');
  assert.equal(map.calls.resize, 0, '大きさの合わせ直しは描画の確定後');
  env.raf();
  assert.equal(map.calls.resize, 1, '表示したとき大きさを合わせる');
  assert.deepEqual(plain(map.calls.resizeArgs), [{libMove: true}], 'resize は moveend を起こすので libMove を付ける');
  // 切り捨てのとき
  env.handlers.points = () => reply(pointsBody({truncated: true, total: 25000}));
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.fake.maps.length, 1, '地図は 1 つを使い回す');
  assert.equal(env.$('#lib-map-note').textContent, '2 点（全 25000 点のうち新しい順）');
  assert.equal(src.setDataCalls.length, 2);
  // クラスタの件数の文字: 見えているクラスタごとに 1 つのマーカー
  env.fake.features = [
    {properties: {cluster_id: 7, point_count: 1200, point_count_abbreviated: '1.2k'}, geometry: {coordinates: [20, 10]}},
    {properties: {cluster_id: 7, point_count: 1200, point_count_abbreviated: '1.2k'}, geometry: {coordinates: [20, 10]}},
    {properties: {cluster_id: 8, point_count: 12, point_count_abbreviated: 12}, geometry: {coordinates: [21, 11]}},
  ];
  map.fire('sourcedata', {sourceId: 'photos', isSourceLoaded: true});
  const labels = () => env.fake.markers.filter(m => !m.removed);
  assert.deepEqual(labels().map(m => m.element.textContent), ['1.2k', '12']);
  assert.deepEqual(plain(labels().map(m => m.lngLat)), [[20, 10], [21, 11]]);
  map.fire('moveend');
  assert.equal(labels().length, 2, '同じクラスタを重ねて作らない');
  env.fake.features = [env.fake.features[2]];
  map.fire('zoomend');
  assert.deepEqual(labels().map(m => m.element.textContent), ['12'], '見えなくなったクラスタの文字は外す');
});

await test('(e3) クラスタの件数の更新は render（毎フレーム）では呼ばず、moveend・zoomend・点の sourcedata・setData の直後だけ', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  const types = map.handlers.map(h => h.type);
  assert.ok(!types.includes('render'), 'render には登録しない');
  for(const t of ['moveend', 'zoomend', 'idle', 'sourcedata']) assert.ok(types.includes(t), t);
  const feat = id => ({properties: {cluster_id: id, point_count: 9, point_count_abbreviated: 9}, geometry: {coordinates: [20, 10]}});
  const labels = () => env.fake.markers.filter(m => !m.removed).length;
  env.fake.features = [feat(1)];
  map.fire('render');
  assert.equal(labels(), 0, 'render では更新しない');
  map.fire('sourcedata', {sourceId: 'other', isSourceLoaded: true});
  map.fire('sourcedata', {sourceId: 'photos', isSourceLoaded: false});
  assert.equal(labels(), 0, '別の source・読み込み途中の sourcedata では更新しない');
  map.fire('sourcedata', {sourceId: 'photos', isSourceLoaded: true});
  assert.equal(labels(), 1);
  env.fake.features = [feat(1), feat(2)];
  map.fire('moveend');
  assert.equal(labels(), 2);
  // moveend の時点では古いまま（sourcedata も出ない）→ idle で更新先のクラスタに合う
  env.fake.features = [feat(1), feat(2)];
  map.fire('moveend');
  env.fake.features = [feat(5)];
  assert.equal(labels(), 2, 'idle の前は古いまま');
  map.fire('idle');
  assert.equal(labels(), 1, 'idle で更新先のクラスタに合う');
  // setData の直後にも合わせる（読み直しで古い文字は外れ、見えているクラスタの文字が付く）
  env.fake.features = [feat(3)];
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  const live = env.fake.markers.filter(m => !m.removed);
  assert.equal(live.length, 1);
  assert.equal(map.sources.photos.setDataCalls.length, 2);
});

await test('(e2) クラスタを押すと展開できる zoom まで寄る。単独の点・クラスタの上ではカーソルが指になる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  map.fire('click', {features: [{properties: {cluster_id: 7}, geometry: {coordinates: [20.25, 10.5]}}]}, 'lib-clusters');
  await settle();
  assert.equal(map.sources.photos.expansionAsked, 7);
  assert.deepEqual(plain(map.calls.easeTo), [{center: [20.25, 10.5], zoom: 11}]);
  map.fire('mouseenter', {}, 'lib-point');
  assert.equal(map.canvas.style.cursor, 'pointer');
  map.fire('mouseleave', {}, 'lib-point');
  assert.equal(map.canvas.style.cursor, '');
});

await test('(f) 点を押すと ids= で 1 枚を引き、吹き出しは 読み込み中 → 内容。「開く」で同じビューアが開く', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  let release;
  env.handlers.photos = () => new Promise(res => { release = () => res({ok: true, status: 200, json: () =>
    Promise.resolve({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [P501]})}); });
  map.fire('click', {features: [{properties: {id: 501}, geometry: {coordinates: [20, 10]}}]}, 'lib-point');
  assert.equal(env.fake.popups.length, 1);
  const pop = env.fake.popups[0];
  assert.deepEqual(plain(pop.lngLat), [20, 10]);
  assert.equal(pop.map, map);
  assert.equal(pop.content.textContent, '読み込み中');
  const ask = env.reqs('/api/admin/library/photos').at(-1);
  assert.equal(ask.url, '/api/admin/library/photos?ids=501');
  release();
  await settle();
  assert.equal(pop.content.querySelector('img').src, '/thumb/501.jpg?v=aaaaaaaaaaaa');
  assert.equal(pop.content.querySelector('.lib-pop-dt').textContent, '2030-05-03 09:12');
  assert.equal(pop.content.querySelector('.lib-pop-place').textContent, '架空県 試験市');
  assert.equal(pop.removed, false);
  pop.content.querySelector('.lib-pop-open').click();
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.$('#viewer-title').textContent, '#501  a.jpg');
  assert.equal(env.$('#viewer-counter').textContent, '1 / 1');
  assert.equal(hidden(env, '#viewer-ai'), true);
  // 吹き出しで開いたビューアの★も、setflag にその id を送る（格子が無くても動く）
  env.$('#viewer-favorite').click();
  await settle();
  assert.deepEqual(JSON.parse(env.reqs('/api/admin/setflag').at(-1).body), {id: 501, favorite: true});
  assert.equal(env.$('#viewer-favorite').textContent, '★ favorite');
  env.$('#viewer-close').click();
});

await test('(f2) 点の要求は新しいものだけが吹き出しに出る（古い応答は捨てる）。失敗は「読めませんでした」。除外を含めるときは hidden=1', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  const releases = {};
  env.handlers.photos = params => new Promise(res => {
    const id = params.get('ids');
    releases[id] = (how = 'ok') => res(how === 'fail'
      ? {ok: false, status: 500, json: () => Promise.resolve({ok: false, error: 'boom'})}
      : {ok: true, status: 200, json: () => Promise.resolve(
          {ok: true, page: 1, per: 60, pages: 1, total: 1, items: [id === '501' ? P501 : P503]})});
  });
  const click = (id, lon) => map.fire('click', {features: [{properties: {id}, geometry: {coordinates: [lon, 10]}}]}, 'lib-point');
  click(501, 20);
  click(503, 20.5);
  assert.equal(env.fake.popups.length, 2);
  assert.equal(env.fake.popups[0].removed, true, '前の吹き出しは閉じる');
  releases['503']();
  await settle();
  releases['501']();                               // 古い要求の応答が後から届く
  await settle();
  const [p1, p2] = env.fake.popups;
  assert.equal(p2.content.querySelector('.lib-pop-dt').textContent, '2030-05-03 10:00');
  assert.equal(p1.content.textContent, '読み込み中', '古い吹き出しは書き換えない');
  // 失敗
  click(501, 20);
  releases['501']('fail');
  await settle();
  assert.equal(env.fake.popups[2].content.textContent, '読めませんでした');
  // 除外を含めるとき、ids の要求にも hidden=1 が付く
  const box = env.$('#lib-hidden');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/points').get('hidden'), '1');
  click(503, 20.5);
  assert.equal(env.last('/api/admin/library/photos').get('ids'), '503');
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1');
});

await test('(g) 「この範囲の写真を見る」: 表示範囲を ±180/±90 に丸めた bbox で写真を読み、地図の下の詳細に出す（地図は残る）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(hidden(env, '#lib-detail'), true);
  env.fake.bounds = {w: 100.1234567, s: 10.5, e: 120.25, n: 30.75};
  env.$('#lib-bbox').click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '100.123457,10.5,120.25,30.75');
  assert.equal(q.get('from'), null);
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-places'), false, '地図は残る');
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲');
  assert.equal(tiles(env).length, 3);
  assert.deepEqual(plain(env.$('#lib-detail').scrollCalls), [{block: 'nearest', inline: 'nearest'}]);
  // 世界を引いた表示（範囲外の値）は丸める
  env.fake.bounds = {w: -250.5, s: -95, e: 300, n: 91.5};
  env.$('#lib-bbox').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '-180,-90,180,90');
  // 別の view へ移ると詳細は隠れ、地図へ戻ると読み直す
  env.$('#lib-view-trips').click();
  assert.equal(hidden(env, '#lib-detail'), true);
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs + 1);
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '-180,-90,180,90');
  assert.equal(hidden(env, '#lib-detail'), false);
});
await test('(g2) 地図は周回させない（renderWorldCopies: false・maxBounds）。経度が ±180 を超える表示でも bbox は範囲内で w <= e', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  assert.equal(map.options.renderWorldCopies, false);
  assert.deepEqual(plain(map.options.maxBounds), [[-179.999, -85], [179.999, 85]]);
  // 回帰の見張り: 実際の MapLibre 4.7.1 は maxBounds の経度がちょうど ±180 だと初期カメラがズーム 22・経度 180 に潰れる。
  // 偽の MapLibre では検出できないので、値そのもので見張る。
  for(const lng of [plain(map.options.maxBounds)[0][0], plain(map.options.maxBounds)[1][0]]) assert.ok(Math.abs(lng) < 180, `maxBounds の経度は ±180 未満: ${lng}`);
  env.fake.bounds = {w: 190, s: 10, e: 210, n: 30};
  env.$('#lib-bbox').click();
  await settle();
  const bbox = env.last('/api/admin/library/photos').get('bbox').split(',').map(Number);
  assert.deepEqual(bbox, [-180, 10, 180, 30], '潰れた範囲は経度の全域にする');
  assert.ok(bbox[0] >= -180 && bbox[2] <= 180 && bbox[0] <= bbox[2]);
  env.fake.bounds = {w: 179, s: 10, e: 210, n: 30};      // 東だけ超える: 範囲内に丸めた w < e
  env.$('#lib-bbox').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '179,10,180,30');
});

await test('(m) 「除外も含める」の切り替え: 地図が見えていれば点も詳細も hidden=1 で読み直し、見えていなければ地図へ移るとき hidden を合わせて読む', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const pointReqs = () => env.reqs('/api/admin/library/points');
  assert.equal(pointReqs().length, 1);
  env.$('#lib-bbox').click();
  await settle();
  const trips = env.reqs('/api/admin/library/trips').length;
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1', '詳細は読み直す');
  assert.equal(env.last('/api/admin/library/photos').get('page'), '1');
  assert.equal(pointReqs().length, 2, '地図が見えているので点も読み直す');
  assert.equal(env.last('/api/admin/library/points').get('hidden'), '1');
  assert.equal(env.reqs('/api/admin/library/trips').length, trips, '見えていない旅の一覧は読まない（loaded を落とすだけ）');
  assert.equal(env.ev('ls.loaded.trips'), false);
  // 旅の画面へ移ると、落ちていた旅の一覧を hidden=1 で読む
  env.$('#lib-view-trips').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/trips').length, trips + 1);
  assert.equal(env.last('/api/admin/library/trips').get('hidden'), '1');
  // 旅の画面で切り替えると、地図の点は読まず、地図へ移るとき読む（hidden なし）
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(pointReqs().length, 2);
  assert.equal(env.ev('ls.pointsStale'), true);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(pointReqs().length, 3);
  assert.equal(env.last('/api/admin/library/points').get('hidden'), null);
  assert.equal(env.ev('ls.pointsStale'), false);
  // 点の応答を待っている間に切り替えたら、古い設定の応答は捨てる
  const held = [];
  env.handlers.points = () => new Promise(res => held.push(() => res({ok: true, status: 200, json: () =>
    Promise.resolve(pointsBody({features: [feature(999, 1, 1)]}))})));
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();           // 点の要求が飛ぶ（まだ応答なし）
  await settle();
  env.$('#lib-view-trips').click();
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const setData = mapOf(env).sources.photos.setDataCalls.length;
  held.forEach(f => f());
  await settle();
  assert.equal(mapOf(env).sources.photos.setDataCalls.length, setData, '古い応答では地図の点を差し替えない');
});
await test('(n) 写真タブの一覧・検索の応答が遅れて届いても、ライブラリから開いたビューアは閉じない（写真タブのビューアは閉じる）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  // 写真タブの一覧を要求（応答は保留）→ ライブラリのビューアを開く → 応答
  let releaseList;
  env.handlers.tabList = () => new Promise(res => { releaseList = () => res({ok: true, status: 200, json: () =>
    Promise.resolve({ok: true, page: 1, per: 50, pages: 1, total: 2, filter: 'all', items: TAB_PHOTOS})}); });
  vm.runInContext('loadList()', env.context);
  tiles(env)[1].click();
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.ev('viewerIndex'), 1);
  releaseList();
  await settle();
  assert.equal(env.ev('ps.entries.length'), 2, '一覧の応答は反映される');
  assert.equal(hidden(env, '#viewer'), false, '一覧の応答でライブラリのビューアは閉じない');
  assert.equal(env.ev('viewerIndex'), 1);
  assert.equal(env.ev('viewerEntries.length'), 3);
  // 検索の応答でも同じ
  let releaseSearch;
  env.handlers.search = () => new Promise(res => { releaseSearch = () => res({ok: true, status: 200, json: () =>
    Promise.resolve({ok: true, mode: 'search', query: 'x', searched_photos: 0, groups: []})}); });
  vm.runInContext("search('x')", env.context);
  releaseSearch();
  await settle();
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(hidden(env, '#viewer'), false, '検索の応答でも閉じない');
  assert.equal(env.ev('viewerIndex'), 1);
  // 検索を閉じる・チップの切り替えも同じ
  vm.runInContext('closeSearch()', env.context);
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.ev('viewerIndex'), 1);
  // closePhotoOverlays は今までどおり全部閉じる
  vm.runInContext('closePhotoOverlays()', env.context);
  assert.equal(hidden(env, '#viewer'), true);
  assert.equal(env.ev('viewerIndex'), -1);
  // 写真タブ自身のビューア（ps.entries）は、一覧の再描画で閉じる
  env.handlers.tabList = null;
  vm.runInContext('loadList()', env.context);
  await settle();
  vm.runInContext('openModal(0)', env.context);
  assert.equal(hidden(env, '#viewer'), false);
  vm.runInContext('loadList()', env.context);
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.equal(env.ev('viewerIndex'), -1);
});

await test('(o) .hidden で出し入れする要素の display は、ID・クラスの規則が .hidden に勝たないよう、対の .hidden 規則を持つ', () => {
  const css = fs.readFileSync(new URL('../static/admin.css', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
  const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(m => [m[1].trim(), m[2]]);
  assert.ok(rules.some(([sel, body]) => sel === '.hidden' && /display:\s*none/.test(body)));
  // library.js が .hidden を付け外しする要素（id）と、その class
  const html = fs.readFileSync(new URL('../static/admin.html', import.meta.url), 'utf8');
  const toggled = ['lib-trips', 'lib-calendar', 'lib-places', 'lib-months', 'lib-detail', 'lib-period', 'lib-empty', 'viewer-noimage',
                   'lib-search', 'lib-when', 'lib-when-note', 'lib-place-combo', 'lib-place-list'];
  const selectorsOf = id => {
    const tag = html.match(new RegExp('<[^>]*id="' + id + '"[^>]*>'));
    assert.ok(tag, id);
    const cls = (tag[0].match(/class="([^"]*)"/) || [, ''])[1].split(/\s+/).filter(c => c && c !== 'hidden');
    return ['#' + id, ...cls.map(c => '.' + c)];
  };
  for(const id of toggled){
    for(const sel of selectorsOf(id)){
      const displays = rules.filter(([rsel, body]) => rsel.split(',').map(x => x.trim()).includes(sel) && /(^|;)\s*display:/.test(body));
      if(!displays.length) continue;
      assert.ok(rules.some(([rsel, body]) => rsel.split(',').map(x => x.trim()).includes(sel + '.hidden') && /display:\s*none/.test(body)),
        sel + ' に display があるのに ' + sel + '.hidden{display:none} が無い');
    }
  }
  // 共通ビューアの画像（.ps-viewer-media img の display が .hidden に勝つ）にも対の規則
  assert.ok(rules.some(([sel, body]) => sel === '.ps-viewer-media img.hidden' && /display:\s*none/.test(body)));
  // 逆向きの確認: .lib-* / #lib-* の display を持つ規則のうち、.hidden を付ける要素のものは上の対を持つ
  const libDisplay = rules.filter(([sel, body]) => /^[#.]lib-[\w-]+$/.test(sel) && /(^|;)\s*display:/.test(body)).map(([sel]) => sel);
  const hasPair = sel => rules.some(([rsel]) => rsel === sel + '.hidden');
  for(const sel of libDisplay){
    const isToggled = toggled.some(id => selectorsOf(id).includes(sel));
    if(isToggled) assert.ok(hasPair(sel), sel);
  }
  // コンボボックスの包み（id なし）は display を持つので、対の規則が要る
  assert.ok(rules.some(([sel, body]) => sel === '.lib-combo' && /display:/.test(body)));
  assert.ok(rules.some(([sel, body]) => sel === '.lib-combo.hidden' && /display:\s*none/.test(body)));
  // 古い部品は無い
  assert.ok(!html.includes('lib-back') && !html.includes('lib-grid-box') && !html.includes('lib-map-box') && !html.includes('lib-view-map'));
});
const A = '2030-05-03 09:12', Z = '2030-05-05 18:40', B = '2029-01-01 00:00';
const qOf = c => new URLSearchParams(c.url.split('?')[1] || '');
const placeQs = env => env.reqs('/api/admin/library/places').map(qOf);
const pcards = env => env.$$('#lib-place-cards .lib-region');            // 展開している国の地域のカード（日本の都道府県）
const ccards = env => env.$$('#lib-place-cards .lib-country');          // 国のカード
const allCards = env => env.$$('#lib-place-cards .lib-card');
const cardByKey = (env, key) => allCards(env).find(c => c.dataset.key === key);
const JP = 'ne:JPN';
const cardName = c => mainOf(c).querySelector('.lib-card-name').textContent;
const cardCount = c => mainOf(c).querySelector('.lib-card-count').textContent;
const cityChips = card => card.querySelectorAll('.lib-cities .lib-city');
const openPlaces = async env => { env.$('#lib-view-places').click(); await settle(); };
const pointsInTrip = () => reply(pointsBody({features: [feature(501, 130.5, 30.5), feature(503, 131.5, 31.5), feature(502, 131, 31)]}));
const mapBtnOf = (env, i) => cards(env)[i].querySelector('.lib-card-map');

await test('(h) AC8 旅の「地図で見る」: 場所の画面をその期間で開き、/places・/points・/photos のすべてに同じ from・to。下に旅の写真（1 ページ目）。点の範囲へ fitBounds（libMove 付き）で、カードは絞らない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.points = pointsInTrip;
  mapBtnOf(env, 0).click();
  await settle();
  for(const name of ['places', 'points', 'photos']){
    const rs = env.reqs('/api/admin/library/' + name).map(qOf);
    assert.ok(rs.length >= 1, name);
    rs.forEach(q => { assert.equal(q.get('from'), A, name); assert.equal(q.get('to'), Z, name); });
  }
  assert.equal(env.reqs('/api/admin/library/places').length, 2, 'カード用と候補用');
  assert.equal(env.reqs('/api/admin/library/points').length, 1);
  assert.equal(env.reqs('/api/admin/library/photos').length, 1);
  const g = env.last('/api/admin/library/photos');
  assert.equal(g.get('page'), '1');
  assert.equal(g.get('pref'), null);
  assert.equal(g.get('bbox'), null);
  placeQs(env).forEach(q => assert.equal(q.get('bbox'), null, '地図の絞り込みは外れている'));
  assert.equal(hidden(env, '#lib-places'), false);
  assert.ok(env.$('#lib-view-places').classList.contains('on'));
  assert.equal(hidden(env, '#lib-detail'), false);
  same(env.$('#lib-detail').parentNode, env.$('#lib-place-main'), '詳細は右の列の地図の下');
  assert.equal(env.$('#lib-sel').textContent, '期間 ' + A + ' 〜 ' + Z);
  assert.equal(hidden(env, '#lib-period'), false);
  assert.equal(env.$('#lib-period-text').textContent, '期間: ' + A + ' 〜 ' + Z);
  assert.equal(tiles(env).length, 3, '下にその旅の写真');
  const map = mapOf(env);
  assert.deepEqual(plain(map.calls.fitBounds), [{bounds: [130.5, 30.5, 131.5, 31.5], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}}]);
  map.endMove();
  await settle();
  assert.equal(env.reqs('/api/admin/library/places').length, 2, 'fitBounds の moveend ではカードを絞らない');
  assert.equal(env.ev('ls.mapBbox'), null);
  // 範囲の写真を見る: 期間も付く
  env.$('#lib-bbox').click();
  await settle();
  const b = env.last('/api/admin/library/photos');
  assert.equal(b.get('bbox'), '130.5,30.5,131.5,31.5');
  assert.equal(b.get('from'), A);
  assert.equal(b.get('to'), Z);
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲・' + A + ' 〜 ' + Z);
  // 解除: カード・候補・点は期間なしで読み直し、範囲の選択はそのまま期間なしで読み直す（ページ 1）
  const since = env.calls.length;
  env.$('#lib-period-clear').click();
  await settle();
  const after = env.calls.slice(since).filter(c => c.url.startsWith('/api/admin/library/'));
  assert.deepEqual(after.map(c => c.url.split('?')[0].split('/').pop()).sort(), ['photos', 'places', 'places', 'points']);
  after.forEach(c => { const q = qOf(c); assert.equal(q.get('from'), null, c.url); assert.equal(q.get('to'), null, c.url); });
  const g2 = env.last('/api/admin/library/photos');
  assert.equal(g2.get('bbox'), '130.5,30.5,131.5,31.5');
  assert.equal(g2.get('page'), '1');
  assert.equal(hidden(env, '#lib-period'), true);
  assert.equal(env.ev('ls.period'), null);
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲');
  assert.equal(hidden(env, '#lib-detail'), false);
});

await test('(h3) 期間の写真全体の詳細は、解除すると隠れて、飛んでいる応答も捨てる。解除のあとカード・候補・点は期間なし', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  let release;
  env.handlers.photos = () => new Promise(res => { release = () => res({ok: true, status: 200, json: () =>
    Promise.resolve({ok: true, page: 1, per: 60, pages: 1, total: 3, items: GRID})}); });
  mapBtnOf(env, 0).click();                          // 期間の写真（応答は保留）
  await settle();
  assert.equal(tiles(env).length, 0);
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  env.$('#lib-period-clear').click();
  await settle();
  assert.equal(hidden(env, '#lib-detail'), true, '期間の写真全体の詳細は隠れる');
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs, '詳細は読み直さない');
  release();                                          // 飛んでいた応答が遅れて届く
  await settle();
  assert.equal(tiles(env).length, 0, '飛んでいた応答は捨てる');
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(placeQs(env).slice(-2).every(q => q.get('from') === null), true);
  assert.equal(env.last('/api/admin/library/points').get('from'), null);
  assert.equal(hidden(env, '#lib-period'), true);
});

await test('(h4) 期間のあいだの国・地域・市区町村・地域未判定は from・to を付けて読み、地域を選んでから解除すると、地域の詳細を期間なしで読み直す（ページ 1）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  mapBtnOf(env, 0).click();
  await settle();
  assert.equal(pcards(env).length, 0, '期間のあいだは日本を自動で展開しない');
  mainOf(ccards(env)[0]).click();                  // 日本（国全体）
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), null);
  assert.equal(q.get('from'), A);
  assert.equal(pcards(env).length, 2, '国を選ぶと地域を展開する');
  mainOf(pcards(env)[0]).click();                  // 架空県
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('pref'), null);
  assert.equal(q.get('from'), A);
  assert.equal(q.get('to'), Z);
  assert.equal(env.$('#lib-sel').textContent, '日本 架空県');
  cityChips(pcards(env)[0])[1].click();             // 見本町
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('city'), '見本町');
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('from'), A);
  mainOf(cardByKey(env, 'unclassified')).click();   // 地域未判定
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('unclassified'), '1');
  assert.equal(q.get('country'), null);
  assert.equal(q.get('from'), A);
  assert.equal(q.get('to'), Z);
  mainOf(pcards(env)[0]).click();                   // 架空県をまた選ぶ
  await settle();
  env.$('#lib-next').disabled = false;
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  env.$('#lib-period-clear').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs + 1);
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('from'), null);
  assert.equal(q.get('to'), null);
  assert.equal(q.get('page'), '1');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(tiles(env).length, 3);
  assert.ok(pcards(env)[0].classList.contains('is-selected'), '県の選択は残る');
});

await test('(h2) 旅の「地図で見る」は前の旅の範囲選択を持ち越さない: 旅 A → 地図で見る → この範囲の写真を見る（保留）→ 旅へ戻る → 旅 B の地図で見る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  let releaseA;
  env.handlers.photos = params => {
    if(params.get('bbox') && params.get('from') === A){       // 旅 A の範囲の写真（応答は保留）
      return new Promise(res => { releaseA = () => res({ok: true, status: 200, json: () => Promise.resolve(
        {ok: true, page: 1, per: 60, pages: 1, total: 1, items: [P503]})}); });
    }
    return null;
  };
  mapBtnOf(env, 0).click();                                    // 旅 A の地図
  await settle();
  env.$('#lib-bbox').click();                                  // この範囲の写真を見る（保留）
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('from'), A);
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '100,10,120,30');
  env.$('#lib-view-trips').click();                            // 旅へ戻る
  await settle();
  mapBtnOf(env, 1).click();                                    // 旅 B の地図
  await settle();
  assert.equal(env.last('/api/admin/library/points').get('from'), B);
  assert.equal(env.$('#lib-period-text').textContent, '期間: ' + B);
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), B, 'B の期間の写真を読む');
  assert.equal(q.get('to'), B);
  assert.equal(q.get('bbox'), null, 'A の範囲選択は持ち越さない');
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'period'});
  assert.equal(env.$('#lib-sel').textContent, '期間 ' + B);
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502', '503']);
  // 遅れて A の応答が届いても捨てる
  releaseA();
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502', '503'], '遅れた A の詳細は出さない');
  assert.equal(env.$('#lib-sel').textContent, '期間 ' + B);
});
await test('(i) 点の応答が遅れて入れ違っても、新しい要求の結果だけが source に渡る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const release = [];
  env.handlers.points = () => new Promise(res => release.push(body => res({ok: true, status: 200, json: () => Promise.resolve(body)})));
  env.$('#lib-view-places').click();
  await settle();
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();                  // 2 つ目の要求
  await settle();
  assert.equal(release.length, 2);
  release[1](pointsBody({features: [feature(503, 20.5, 10.5)], total: 1}));
  await settle();
  release[0](pointsBody({features: [feature(501, 20.0, 10.0)], total: 1}));
  await settle();
  const src = mapOf(env).sources.photos;
  assert.equal(src.setDataCalls.length, 1);
  assert.deepEqual(plain(src.setDataCalls[0].features.map(f => f.properties.id)), [503]);
});

await test('(j) 地図の style がまだのうちに届いた点は、load のあとの source に入る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  // load を遅らせる偽物にする: on('load') で自動では起こさない
  const Base = env.fake.maplibregl.Map;
  env.fake.maplibregl.Map = class extends Base {
    on(type, a, b){
      if(type === 'load'){ this.lateLoad = typeof a === 'function' ? a : b; return this; }
      return super.on(type, a, b);
    }
  };
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  assert.equal(map.sources.photos, undefined);
  map.lateLoad({type: 'load'});
  assert.deepEqual(plain(map.sources.photos.options.data.features.map(f => f.properties.id)), [501, 503]);
  assert.equal(map.sources.photos.setDataCalls.length, 0);
});

await test('(k) どのタブから離れてもビューアが閉じ、ライブラリへ戻ると旅を読み直して地図の大きさを合わせ直す。選択と詳細は残る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  tiles(env)[0].click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('.tab[data-tab=status]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), -1);
  assert.equal(hidden(env, '#v-library'), true);
  // 戻ると、選んでいた旅とその詳細（読み直し）
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  await openLib(env);
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs + 1);
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.ok(cards(env)[0].classList.contains('is-selected'));
  assert.equal(tiles(env).length, 3);
  // 設定タブへ移るときも
  tiles(env)[0].click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('.tab[data-tab=config]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  // 地図ビューのままタブを往復する
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  env.raf();
  const resizes = map.calls.resize, trips = env.reqs('/api/admin/library/trips').length;
  env.$('.tab[data-tab=status]').click();
  await settle();
  await openLib(env);
  env.raf();
  assert.equal(env.reqs('/api/admin/library/trips').length, trips + 1);
  assert.ok(map.calls.resize > resizes);
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(env.fake.maps.length, 1);
  // 写真タブから戻っても、写真タブが開いたビューアはライブラリに持ち越さない
  env.$('.tab[data-tab=photos]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
});
await test('(l) 旅の読み込み失敗はトーストで知らせ、一覧は前のまま', async () => {
  const env = makeEnv();
  await env.boot();
  env.handlers.trips = () => respond(500, {ok: false, error: 'boom'});
  await openLib(env);
  assert.ok(env.$('#toast').textContent.includes('旅の一覧を読めませんでした: boom'));
  assert.equal(cards(env).length, 0);
});


const monthChips = env => env.$$('#lib-months .lib-month');
const yearCards = env => env.$$('#lib-year-cards .lib-card');
await test('(p) AC4 日付: 年のカード（表紙・年・枚数）と日時なしのカード。年を押すと年の範囲で読み、月のチップ（写真の無い月は押せない）が出る。チップでその月の範囲', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  assert.equal(env.reqs('/api/admin/library/calendar').length, 0, '日付を開くまで読まない');
  env.$('#lib-view-calendar').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/calendar')[0].url, '/api/admin/library/calendar', '既定は除外を含めない');
  assert.ok(env.$('#lib-view-calendar').classList.contains('on'));
  assert.ok(!env.$('#lib-view-trips').classList.contains('on'));
  assert.equal(hidden(env, '#lib-calendar'), false);
  assert.equal(hidden(env, '#lib-trips'), true);
  assert.equal(hidden(env, '#lib-places'), true);
  assert.equal(env.$('#lib-note').textContent, '2 年');
  const c = yearCards(env);
  assert.equal(c.length, 3, '2 年 + 日時なし');
  assert.deepEqual(c.map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2030年', '2029年', '日時なし']);
  assert.deepEqual(c.map(x => mainOf(x).querySelector('.lib-card-count').textContent), ['30 枚', '4 枚', '5 枚']);
  assert.equal(mainOf(c[0]).querySelector('.lib-cover img').src, '/thumb/601.jpg?v=cccccccccccc', '年の表紙');
  assert.equal(mainOf(c[1]).querySelector('.lib-cover .img-broken').textContent, '画像なし', '表紙の無い年');
  assert.equal(env.$('#lib-year-cards').classList.contains('lib-cards'), true);
  assert.equal(hidden(env, '#lib-months'), true, '年を選ぶまで月は出さない');
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.reqs('/api/admin/library/photos').length, 0);
  // 年のカード → 年の範囲、月のチップ
  mainOf(c[0]).click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2030-00-00 00:00');
  assert.equal(q.get('to'), '2030-99-99 99:99');
  assert.equal(env.$('#lib-sel').textContent, '2030年');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-months'), false);
  assert.ok(env.$('#lib-year-cards').classList.contains('is-strip'));
  let chips = monthChips(env);
  assert.equal(chips.length, 12, '1〜12 月');
  assert.deepEqual(chips.map(x => x.dataset.key), ['01', '02', '03', '04', '05', '06', '07', '08', '09', '10', '11', '12'].map(m => '2030-' + m));
  assert.deepEqual(chips.map(x => x.disabled), [true, true, true, true, false, true, true, true, true, true, true, false], '応答に無い月は押せない');
  assert.equal(chips[4].textContent, '05月20');
  assert.equal(chips[11].textContent, '12月10');
  assert.equal(chips[0].textContent, '01月');
  assert.equal(tiles(env).length, 3);
  // 写真の無い月は押せない
  const before = env.reqs('/api/admin/library/photos').length;
  chips[0].click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  // 月のチップ → その月の範囲。年のカードも選ばれたまま
  chips[11].click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2030-12-00 00:00');
  assert.equal(q.get('to'), '2030-12-99 99:99');
  assert.equal(env.$('#lib-sel').textContent, '2030年12月');
  assert.equal(chips[11].getAttribute('aria-pressed'), 'true');
  assert.ok(chips[11].classList.contains('is-selected'));
  assert.equal(chips[4].getAttribute('aria-pressed'), 'false');
  assert.ok(c[0].classList.contains('is-selected'));
  assert.equal(mainOf(c[0]).getAttribute('aria-pressed'), 'false', '押された本体は月');
  same(monthChips(env)[11], chips[11], '選んでもチップは作り直さない（フォーカスを失わない）');
  // 別の月 → 切り替わる。同じ月をもう一度押すと外れる（年のカードは選ばれたまま消える）
  chips[4].click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2030-05-00 00:00');
  assert.equal(chips[11].getAttribute('aria-pressed'), 'false');
  chips[4].click();
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.ev('ls.sel.calendar'), null);
  assert.equal(hidden(env, '#lib-months'), true);
  // 別の年 → その年の月のチップに替わる
  mainOf(c[1]).click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2029-00-00 00:00');
  chips = monthChips(env);
  assert.equal(chips.length, 12);
  assert.deepEqual(chips.map(x => x.disabled), [false, ...Array(11).fill(true)]);
  assert.equal(chips[0].dataset.key, '2029-01');
  // 年のカードを選んだまま、別の年の月を… は無い。年のカードをもう一度押すと外れ、月も消える
  mainOf(c[1]).click();
  assert.equal(hidden(env, '#lib-months'), true);
  assert.equal(hidden(env, '#lib-detail'), true);
  // AC5: 日時なし
  mainOf(c[2]).click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('untimed'), '1');
  assert.equal(q.get('from'), null);
  assert.equal(env.$('#lib-sel').textContent, '日時なし');
  assert.equal(hidden(env, '#lib-months'), true, '日時なしに月は無い');
  // 旅へ戻すと、旅の一覧と旅の件数。旅の選択は別に持つ（まだ無い）
  env.$('#lib-view-trips').click();
  assert.equal(hidden(env, '#lib-trips'), false);
  assert.equal(hidden(env, '#lib-calendar'), true);
  assert.equal(hidden(env, '#lib-detail'), true, '旅の選択はまだ無い');
  assert.equal(env.$('#lib-note').textContent, '旅 2 件');
  // 日付へ戻ると、選んでいた日時なしの詳細を読み直す
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  env.$('#lib-view-calendar').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs + 1);
  assert.equal(env.last('/api/admin/library/photos').get('untimed'), '1');
  assert.ok(yearCards(env)[2].classList.contains('is-selected'));
});

await test('(p2) 日付: 暦に無い月（13）も形のとおり並べる。写真が無ければ空の表示。読み込み失敗はトースト', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.calendar = () => reply({ok: true, untimed: 0, years: [
    {year: '2031', count: 3, cover: {id: 7, thumb_url: ''}, months: [{month: '02', count: 1}, {month: '13', count: 2}]}]});
  env.$('#lib-view-calendar').click();
  await settle();
  assert.deepEqual(yearCards(env).map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2031年'], '日時なし 0 枚ならカードを出さない');
  assert.equal(mainOf(yearCards(env)[0]).querySelector('.lib-cover .img-broken').textContent, '画像なし', 'thumb_url が空なら画像なし');
  mainOf(yearCards(env)[0]).click();
  await settle();
  const chips = monthChips(env);
  assert.equal(chips.length, 13);
  assert.equal(chips[12].dataset.key, '2031-13');
  assert.equal(chips[12].disabled, false);
  chips[12].click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2031-13-00 00:00');
  assert.equal(env.last('/api/admin/library/photos').get('to'), '2031-13-99 99:99');
  // 応答から年が無くなっても選択は残り、詳細はそのまま（該当が無ければ「該当する写真がありません」）
  env.handlers.calendar = () => reply({ok: true, years: [], untimed: 0});
  env.$('#lib-view-places').click();
  env.$('#lib-view-calendar').click();
  await settle();
  assert.equal(env.$('#lib-year-cards').textContent, '写真がありません');
  assert.equal(hidden(env, '#lib-months'), true, '年が無ければ月も出さない');
  assert.notEqual(env.ev('ls.sel.calendar'), null, '選択は残す');
  // 失敗
  env.handlers.calendar = () => respond(500, {ok: false, error: 'boom'});
  env.$('#lib-view-places').click();
  env.$('#lib-view-calendar').click();
  await settle();
  assert.ok(env.$('#toast').textContent.includes('日付の一覧を読めませんでした: boom'));
});

await test('(p3) AC15 除外の切り替え: 見えている一覧・詳細（ページ 1）を hidden=1 で読み直し、見えていない view は loaded を落とすだけ。飛んでいる一覧の応答は捨てる（AC16a）', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.photos = params => reply({ok: true, page: Number(params.get('page')), per: 60, pages: 3, total: 130, items: GRID});
  pressTrip(env, 0);
  await settle();
  env.$('#lib-next').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('page'), '2');
  // 切り替え: 出る URL はすべて hidden=1。旅の一覧と詳細（ページ 1）だけ。日付・場所・点は読まない
  const since = env.calls.length;
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const libCalls = env.calls.slice(since).filter(c => c.url.startsWith('/api/admin/library/'));
  assert.deepEqual(libCalls.map(c => c.url.split('?')[0].split('/').pop()).sort(), ['photos', 'trips']);
  assert.ok(libCalls.every(c => new URLSearchParams(c.url.split('?')[1] || '').get('hidden') === '1'), 'すべて hidden=1');
  assert.equal(env.last('/api/admin/library/photos').get('page'), '1');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2030-05-03 09:12', '選択はそのまま');
  assert.deepEqual(plain(env.ev('ls.loaded')), {trips: true, calendar: false, placeCards: false, placeOptions: false});
  // 日付の画面へ移ると、落ちていた一覧を hidden=1 で読む
  env.$('#lib-view-calendar').click();
  await settle();
  assert.equal(env.last('/api/admin/library/calendar').get('hidden'), '1');
  // 日付の画面で選んでから戻す: 日付の一覧・詳細が hidden なしで読み直される
  mainOf(yearCards(env)[0]).click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1');
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/calendar').get('hidden'), null);
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), null);
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2030-00-00 00:00');
  // 切り替えの前に出した一覧の応答が後から届いても捨てる
  const held = [];
  const slow = body => new Promise(res => held.push(() => res({ok: true, status: 200, json: () => Promise.resolve(body)})));
  env.handlers.calendar = () => slow({ok: true, untimed: 0, years: [{year: '1999', count: 1, cover: null, months: []}]});
  env.$('#lib-view-places').click();
  env.$('#lib-view-calendar').click();          // 切り替えの前の一覧（保留）
  await settle();
  env.handlers.calendar = () => reply({ok: true, untimed: 0, years: [{year: '2031', count: 1, cover: null, months: []}]});
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.deepEqual(yearCards(env).map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2031年']);
  held.forEach(f => f());
  await settle();
  assert.deepEqual(yearCards(env).map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2031年'], '古い一覧の応答は捨てる');
  // 見えていない view の一覧が飛んでいても、切り替えのあとに loaded を立て直さない
  env.handlers.places = () => slow({ok: true, prefs: [], abroad: 0});
  env.$('#lib-view-places').click();
  await settle();
  env.$('#lib-view-calendar').click();
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  held.forEach(f => f());
  await settle();
  assert.equal(env.ev('ls.loaded.placeCards'), false);
  assert.equal(env.ev('ls.loaded.placeOptions'), false);
  assert.equal(env.ev('ls.prefs.length'), 2, '飛んでいた空の応答で一覧を差し替えない');
  assert.equal(env.ev('ls.placeOpts.prefs.length'), 2);
});
await test('(q) AC1・AC2・AC11 場所: 国のカード（日本）と、初回に展開した日本の都道府県のカード（表紙・名前・枚数・主ボタン）と地域未判定。県を押すと country・region・fitBounds・市区町村のチップ（件数順）。チップで city と fitBounds。もう一度押すと外れる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.$('#lib-view-map'), null, '地図の seg は無い');
  assert.deepEqual(env.$$('.lib-seg').map(b => b.textContent), ['旅', '日付', '場所']);
  const ps = env.reqs('/api/admin/library/places');
  assert.deepEqual(ps.map(c => c.url), ['/api/admin/library/places', '/api/admin/library/places'], 'カード用と候補用。既定は除外を含めない');
  assert.ok(env.$('#lib-view-places').classList.contains('on'));
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(hidden(env, '#lib-calendar'), true);
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.$('#lib-note').textContent, '1 か国');
  assert.ok(env.$('#lib-place-cards').classList.contains('lib-cards'));
  assert.ok(!env.$('#lib-place-cards').classList.contains('is-strip'), '場所のカードは 1 段にしない');
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県', '試験県', '地域未判定'], '国 → 展開した国の地域 → 国一覧の地域未判定');
  assert.deepEqual(allCards(env).map(cardCount), ['53 枚', '50 枚', '3 枚', '9 枚']);
  assert.equal(env.ev('ls.sel.places'), null, '日本を展開するだけで選ばない');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '写真は読まない');
  assert.equal(env.fake.maps[0].calls.fitBounds.length, 0, '地図は動かさない');
  assert.equal(mainOf(ccards(env)[0]).getAttribute('aria-expanded'), 'true');
  const c = pcards(env).concat([cardByKey(env, 'unclassified')]);
  assert.ok(allCards(env).every(x => x.tagName.toLowerCase() === 'div' && x.listeners.click === undefined), 'カードの容器は対話しない');
  assert.equal(mainOf(c[0]).querySelector('.lib-cover img').src, '/thumb/701.jpg?v=dddddddddddd', '県の表紙');
  assert.equal(mainOf(c[1]).querySelector('.lib-cover .img-broken').textContent, '画像なし');
  assert.equal(env.$$('#lib-place-cards .lib-cities').length, 0, '選ぶまでチップは出さない');
  assert.equal(env.fake.maps[0].calls.fitBounds.length, 0);
  // 県 → country・region と fitBounds
  mainOf(c[0]).click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('pref'), null, '旧形式は混ぜない');
  assert.equal(q.get('city'), null);
  assert.equal(env.$('#lib-sel').textContent, '日本 架空県');
  assert.equal(hidden(env, '#lib-detail'), false);
  same(env.$('#lib-detail').parentNode, env.$('#lib-place-main'));
  const map = mapOf(env);
  assert.deepEqual(plain(map.calls.fitBounds), [{bounds: [130, 30, 132, 32], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}}]);
  assert.ok(c[0].classList.contains('is-selected'));
  assert.equal(mainOf(c[0]).getAttribute('aria-pressed'), 'true');
  assert.ok(c[0].classList.contains('has-cities'));
  let chips = cityChips(c[0]);
  assert.deepEqual(chips.map(x => x.textContent), ['試験市40', '見本町8', '（市区町村なし）2'], '件数の多い順');
  assert.deepEqual(chips.map(x => x.disabled), [false, false, true], 'city が空のものは押せない');
  chips.forEach(x => { same(x.parentNode.parentNode, c[0], 'チップは主ボタンの兄弟（カードの子）'); });
  assert.equal(mainOf(c[0]).querySelectorAll('button').length, 0, 'ボタンを入れ子にしない');
  assert.equal(c[1].querySelector('.lib-cities'), null, '選んでいない県にチップは付かない');
  assert.equal(mainOf(c[1]).getAttribute('aria-pressed'), 'false');
  // 市区町村のチップ → city と fitBounds。チップは作り直さない
  chips[1].click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('city'), '見本町');
  assert.equal(q.get('pref'), null);
  assert.equal(env.$('#lib-sel').textContent, '日本 架空県 見本町');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1)), {bounds: [130.5, 30.5, 130.6, 30.6], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}});
  assert.equal(map.calls.fitBounds.length, 2);
  assert.equal(chips[1].getAttribute('aria-pressed'), 'true');
  assert.ok(chips[1].classList.contains('is-selected'));
  assert.equal(mainOf(c[0]).getAttribute('aria-pressed'), 'false', '押された本体は市区町村');
  assert.ok(c[0].classList.contains('is-selected'), 'その県のカードも選ばれたまま');
  same(cityChips(c[0])[1], chips[1], 'チップを作り直さない（フォーカスを失わない）');
  const before = env.reqs('/api/admin/library/photos').length;
  chips[2].click();                                          // city が空: 押せない
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  // bbox が null の県: 選べるが地図は動かさない。前の県のチップは消える
  mainOf(c[1]).click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('region'), jpId('試験県'));
  assert.equal(map.calls.fitBounds.length, 2, 'bbox が null なら地図は動かさない');
  assert.equal(env.$('#lib-map-hint').textContent, '', '位置のある写真が無いので案内も出さない');
  assert.equal(c[0].querySelector('.lib-cities'), null);
  assert.equal(cityChips(c[1]).length, 1);
  assert.ok(!c[0].classList.contains('is-selected'));
  // 地域未判定
  mainOf(c[2]).click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('unclassified'), '1');
  assert.equal(q.get('country'), null);
  assert.equal(q.get('pref'), null);
  assert.equal(env.$('#lib-sel').textContent, '地域未判定');
  assert.equal(map.calls.fitBounds.length, 2);
  assert.ok(c[2].classList.contains('is-selected'));
  // 同じカードをもう一度押すと外れ、詳細は隠れ、地図は動かさない
  mainOf(c[2]).click();
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(map.calls.fitBounds.length, 2);
  // 今の範囲と同じなら fitBounds を呼ばない
  env.fake.bounds = {w: 130, s: 30, e: 132, n: 32};
  mainOf(c[0]).click();
  await settle();
  assert.equal(map.calls.fitBounds.length, 2, '今の範囲と同じなら呼ばない');
  assert.equal(env.last('/api/admin/library/photos').get('region'), jpId('架空県'));
  // 除外を含める: カード用・候補用・点・詳細のすべてが hidden=1
  const since = env.calls.length;
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const libCalls = env.calls.slice(since).filter(x => x.url.startsWith('/api/admin/library/'));
  assert.deepEqual(libCalls.map(x => x.url.split('?')[0].split('/').pop()).sort(), ['photos', 'places', 'places', 'points']);
  assert.ok(libCalls.every(x => qOf(x).get('hidden') === '1'));
  assert.equal(env.last('/api/admin/library/photos').get('page'), '1');
  // 空・失敗
  env.handlers.places = () => reply({ok: true, prefs: [], abroad: 0});
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.$('#lib-place-cards').textContent, '写真がありません');
  env.handlers.places = () => respond(500, {ok: false, error: 'boom'});
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  assert.ok(env.$('#toast').textContent.includes('場所の一覧を読めませんでした: boom'));
});

await test('(q1) 市区町村のチップは最大 30（残りは選べない「ほか N」の行）。県のカードの幅が 480px 以上の地図では fitBounds の余白が 40', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const many = Array.from({length: 35}, (_, i) => ({city: '市' + String(i).padStart(2, '0'), count: 100 - i, bbox: [131, 31, 131.5, 31.5]}));
  env.handlers.places = () => reply({ok: true, abroad: 0, prefs: [{pref: '多県', count: 999, cover: null, bbox: [130, 30, 132, 32], cities: many}]});
  await openPlaces(env);
  env.$('#lib-map').rectWidth = 800;
  mainOf(pcards(env)[0]).click();
  await settle();
  const chips = cityChips(pcards(env)[0]);
  assert.equal(chips.length, 30);
  assert.equal(chips[0].textContent, '市00100');
  assert.equal(chips[29].textContent, '市2971');
  const more = pcards(env)[0].querySelector('.lib-cities .lib-more');
  assert.equal(more.textContent, 'ほか 5（上の欄で探せます）');
  assert.notEqual(more.tagName.toLowerCase(), 'button', '選べない行');
  assert.deepEqual(plain(mapOf(env).calls.fitBounds[0].options), {padding: 40, maxZoom: 12});
});

await test('(q2) ライブラリタブを開き直すと、場所のビューのままならカード用・候補用を読み直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/places').length, 2);
  env.$('.tab[data-tab=status]').click();
  await settle();
  await openLib(env);
  assert.equal(env.reqs('/api/admin/library/places').length, 4);
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(hidden(env, '#lib-trips'), true);
});
await test('(r) 格子のタイルの 📍 で位置の指定が開く（ビューアは開かない）。保存すると「位置なし」の印が消える', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  const t = tiles(env);
  assert.deepEqual(marksOf(t[1]), ['位置なし']);
  const btn = t[1].querySelector('.lib-loc');
  assert.equal(btn.tagName.toLowerCase(), 'button');
  assert.equal(btn.textContent, '📍');
  assert.equal(btn.title, '位置を指定');
  assert.equal(btn.type, 'button');
  assert.ok(t.every(x => x.querySelector('.lib-loc')), '位置のある写真にも置く（指定し直せる）');
  assert.equal(hidden(env, '#modal'), true);
  // 親へ伝わるクリック（バブリング）でも、ボタンで止まってタイルのビューアには届かない。別の場所のクリックなら届く
  btn.dispatchEvent({type: 'click', bubbles: true});
  assert.equal(hidden(env, '#modal'), false, '位置の指定が開く');
  assert.equal(hidden(env, '#viewer'), true, 'タイルのクリック（ビューア）とは混ざらない');
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('curPhoto.id'), 502);
  assert.equal(env.$('#m-title').textContent, '#502  b.jpg  /  2030-05-03 09:20');
  assert.equal(env.$('#m-save').disabled, true, '地図を押すまで保存できない');
  // キー操作（Enter・Space）もタイルのビューアに届かない。Escape は止めない（document まで届いて位置の指定を閉じられる）
  const kd = key => { const ev = {type: 'keydown', key, bubbles: true}; btn.dispatchEvent(ev); return ev; };
  assert.equal(kd('Enter').propagationStopped, true);
  assert.equal(kd(' ').propagationStopped, true);
  assert.equal(kd('Escape').propagationStopped, undefined, 'Escape は伝播を止めない');
  assert.equal(hidden(env, '#viewer'), true);
  // 地図（位置の指定の地図）を押して保存
  const mini = env.fake.maps.at(-1);
  mini.fire('click', {lngLat: {lat: 35.123456, lng: 139.654321}});
  assert.equal(env.$('#m-save').disabled, false);
  env.$('#m-save').click();
  await settle();
  const post = env.reqs('/api/admin/setloc').at(-1);
  assert.deepEqual(JSON.parse(post.body), {id: 502, lat: 35.123456, lon: 139.654321});
  assert.equal(hidden(env, '#modal'), true, '保存すると閉じる');
  assert.deepEqual(marksOf(t[1]), [], '位置なしの印が消える');
  assert.equal(env.ev('ls.items[1].lat'), 35.123456);
  assert.equal(env.ev('ls.items[1].loc_source'), 'manual');
  assert.equal(hidden(env, '#viewer'), true);
  // ★ など他の印は変わらない（c.jpg は ★ のまま）
  assert.deepEqual(marksOf(t[2]), ['★']);
  // 別の場所（タイルの子の画像）から送ったクリックなら、バブリングでタイルに届いてビューアが開く
  t[0].querySelector('img').dispatchEvent({type: 'click', bubbles: true});
  assert.equal(hidden(env, '#viewer'), false, '画像のクリックはビューアを開く');
});

await test('(s) 共通ビューアの「画像なし」: 格子から開いても地図の吹き出しの「開く」から開いても、thumb が空なら代わりの表示。次の写真で消える', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const shown = () => ({none: !hidden(env, '#viewer-noimage'), img: !hidden(env, '#viewer-image'),
                        text: env.$('#viewer-noimage').textContent});
  pressTrip(env, 0);
  await settle();
  // 格子: 503 は thumb が空
  tiles(env)[2].click();
  assert.equal(env.$('#viewer-title').textContent, '#503  c.jpg');
  assert.deepEqual(shown(), {none: true, img: false, text: '画像なし'});
  env.$('#viewer-prev').click();             // 502（thumb あり）へ
  assert.equal(env.$('#viewer-title').textContent, '#502  b.jpg');
  assert.deepEqual(shown(), {none: false, img: true, text: '画像なし'});
  // 読めないとき（error）も同じ。次の写真で戻る
  env.$('#viewer-image').dispatchEvent({type: 'error'});
  assert.deepEqual(shown(), {none: true, img: false, text: '画像なし'});
  env.$('#viewer-prev').click();
  assert.deepEqual(shown(), {none: false, img: true, text: '画像なし'});
  env.$('#viewer-close').click();
  // 地図の吹き出しの「開く」: 503 を ids= で引いた 1 枚
  env.$('#lib-view-places').click();
  await settle();
  env.handlers.photos = () => reply({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [P503]});
  mapOf(env).fire('click', {features: [{properties: {id: 503}, geometry: {coordinates: [20.5, 10.5]}}]}, 'lib-point');
  await settle();
  env.fake.popups.at(-1).content.querySelector('.lib-pop-open').click();
  assert.equal(env.$('#viewer-title').textContent, '#503  c.jpg');
  assert.deepEqual(shown(), {none: true, img: false, text: '画像なし'});
});

await test('(t) 位置・除外が変わると地図の点を取り直す: 地図が見えていればすぐ。見えていない（旅の画面・別のタブ）なら印だけ立てて地図へ戻るとき。★だけなら読まない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const pointReqs = () => env.reqs('/api/admin/library/points');
  assert.equal(pointReqs().length, 1);
  // 地図が見えている間の位置の patch はすぐ取り直す
  env.context.libPhotoChanged(502, {lat: 35, lon: 139, loc_source: 'manual'});
  await settle();
  assert.equal(pointReqs().length, 2);
  // ★だけの patch では読まない
  env.context.libPhotoChanged(501, {favorite: true});
  await settle();
  assert.equal(pointReqs().length, 2);
  assert.equal(env.ev('ls.pointsStale'), false);
  // 地図の下に詳細があっても、地図は見えている
  env.$('#lib-bbox').click();
  await settle();
  env.context.libPhotoChanged(501, {hidden: true});
  await settle();
  assert.equal(pointReqs().length, 3);
  // 飛んでいた古い点の応答は、取り直したあとで届いても捨てる
  const held = [];
  env.handlers.points = () => new Promise(res => held.push(() => res({ok: true, status: 200, json: () =>
    Promise.resolve(pointsBody({features: [feature(999, 1, 1)]}))})));
  env.context.libPhotoChanged(501, {lat: 36, lon: 140});
  env.handlers.points = null;
  env.context.libPhotoChanged(501, {lat: 36.5, lon: 140});
  await settle();
  const setData = mapOf(env).sources.photos.setDataCalls.length;
  held.forEach(f => f());
  await settle();
  assert.equal(mapOf(env).sources.photos.setDataCalls.length, setData);
  // 旅の画面にいるときは読まず、地図へ移るとき読む
  env.$('#lib-view-trips').click();
  await settle();
  const n = pointReqs().length;
  env.context.libPhotoChanged(502, {lat: 37, lon: 141});
  assert.equal(pointReqs().length, n);
  assert.equal(env.ev('ls.pointsStale'), true);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(pointReqs().length, n + 1);
  assert.equal(env.ev('ls.pointsStale'), false);
  // 別のタブ（写真タブ）にいるときは読まず、ライブラリへ戻るとき読む
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const m = pointReqs().length;
  env.context.libPhotoChanged(501, {lat: 36, lon: 140});
  assert.equal(pointReqs().length, m);
  assert.equal(env.ev('ls.pointsStale'), true);
  await openLib(env);
  assert.equal(pointReqs().length, m + 1);
  assert.equal(env.ev('ls.pointsStale'), false);
});

await test('(u) 偽の MapLibre: fitBounds(bounds, options, eventData)・easeTo(options, eventData) は eventData を moveend のイベントに載せ、getBounds は差し替えられる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  const events = [];
  map.on('moveend', e => events.push(e));
  map.fitBounds([[130, 30], [140, 40]], {padding: 40, maxZoom: 12}, {libMove: true});
  map.easeTo({center: [1, 2], zoom: 5}, {from: 'cluster'});
  map.easeTo({center: [3, 4], zoom: 6});
  assert.equal(map.moves.length, 3);
  assert.deepEqual(plain(map.calls.fitBounds), [{bounds: [[130, 30], [140, 40]], options: {padding: 40, maxZoom: 12}, eventData: {libMove: true}}]);
  assert.deepEqual(plain(map.calls.easeTo.slice(-2)), [{center: [1, 2], zoom: 5}, {center: [3, 4], zoom: 6}], 'easeTo の記録は options だけ');
  map.endMove();
  assert.equal(events.at(-1).libMove, true, 'fitBounds の eventData が moveend に載る');
  assert.deepEqual(plain(env.fake.bounds), {w: 130, s: 30, e: 140, n: 40}, 'fitBounds を終えると表示範囲が移動先になる');
  map.endMove();
  assert.equal(events.at(-1).from, 'cluster');
  assert.equal(events.at(-1).libMove, undefined);
  map.interrupt();                              // 3 つ目は終えずに捨てる（moveend は起きない）
  assert.equal(map.moves.length, 0);
  assert.equal(events.length, 2);
  env.fake.getBounds = () => ({w: 1, s: 2, e: 3, n: 4});
  env.$('#lib-bbox').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '1,2,3,4', 'getBounds の差し替えが効く');
});

// ---- AC7: 地図の移動の理由 ----
// 利用者の移動の印は 2 つだけ: moveend のイベントの originalEvent（ドラッグ・ホイール・ダブルクリック・タッチ・キーボード・ズームボタン）と、
// 自分のコードが付けた eventData {libUser:true}（クラスタを押したときの easeTo）。カード・チップ・「地図で見る」・「全体に戻す」・自分の resize の
// 移動の moveend には libMove が付く。originalEvent も libUser も無い moveend（ResizeObserver 経由の resize とその _constrain の補正）は絞らない。
// 偽の MapLibre は fitBounds・easeTo を積むだけで、moveend は endMove() を呼んだときに起こす
// （interrupt() は起こさず捨てる。本物は中断された移動の moveend も起こすので、試験で明示して fire する）
const withBboxOnly = () => params => params.get('bbox') ? reply({ok: true, abroad: 0, prefs: [
  {pref: '架空県', count: 7, cover: {id: 701, thumb_url: '/thumb/701.jpg?v=dddddddddddd'}, bbox: [130.5, 30.5, 131, 31],
   cities: [{city: '見本町', count: 7, bbox: [130.5, 30.5, 130.6, 30.6]}]}]}) : null;
const bboxReqs = env => placeQs(env).filter(q => q.get('bbox'));
const userMove = (env, to) => {                  // 利用者のドラッグ相当: 中心・ズーム・表示範囲が変わり、originalEvent の付いた moveend
  const map = mapOf(env);
  if(to.bounds) env.fake.bounds = to.bounds;
  if(to.center) env.fake.center = to.center;
  if(to.zoom !== undefined) env.fake.zoom = to.zoom;
  map.fire('movestart', {originalEvent: {type: 'mousedown'}});
  map.fire('moveend', {originalEvent: {type: 'mouseup'}});
};

await test('(v) AC7 ドラッグ・ズーム・クラスタを押したあとの移動では /places?bbox=… を読み直し、カードが範囲の中の国・県・枚数になり、#lib-note に「地図の範囲: N か国」。候補用は読み直さない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.places = withBboxOnly();
  await openPlaces(env);
  env.raf();                                              // 場所を出したときの resize（libMove 付き）では読まない
  const map = mapOf(env);
  assert.equal(bboxReqs(env).length, 0);
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県', '試験県', '地域未判定']);
  // ドラッグ
  userMove(env, {bounds: {w: 130.25, s: 30.5, e: 131.75, n: 31.5}, center: {lng: 131, lat: 31}});
  await settle();
  assert.equal(bboxReqs(env).length, 1);
  let q = bboxReqs(env)[0];
  assert.equal(q.get('bbox'), '130.25,30.5,131.75,31.5');
  assert.equal(q.get('from'), null);
  assert.deepEqual(plain(env.ev('ls.mapBbox')), [130.25, 30.5, 131.75, 31.5]);
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県'], '範囲の中に写真のある県だけ（日本は展開したまま）');
  assert.deepEqual(allCards(env).map(cardCount), ['7 枚', '7 枚'], '範囲の中の枚数');
  assert.equal(env.$('#lib-note').textContent, '地図の範囲: 1 か国');
  assert.equal(placeQs(env).filter(x => !x.get('bbox')).length, 2, '候補用（bbox なし）は読み直さない');
  // ズーム
  userMove(env, {bounds: {w: 130.5, s: 30.75, e: 131.5, n: 31.25}, zoom: 8});
  await settle();
  assert.equal(bboxReqs(env).length, 2);
  assert.equal(bboxReqs(env)[1].get('bbox'), '130.5,30.75,131.5,31.25');
  // クラスタを押す: easeTo に libUser を付ける → 終わると moveend に libUser が載る
  mapOf(env).fire('click', {features: [{properties: {cluster_id: 7}, geometry: {coordinates: [131.25, 31.1]}}]}, 'lib-clusters');
  await settle();
  assert.equal(map.moves.length, 1);
  assert.deepEqual(plain(map.moves[0].eventData), {libUser: true}, 'クラスタの easeTo は libUser を付ける（libMove は付けない）');
  env.fake.bounds = {w: 131, s: 31, e: 131.5, n: 31.25};
  map.endMove();
  await settle();
  assert.equal(bboxReqs(env).length, 3);
  assert.equal(bboxReqs(env)[2].get('bbox'), '131,31,131.5,31.25');
  // originalEvent も libUser も無い moveend は利用者の移動ではない（ResizeObserver 経由の resize は eventData が配列で originalEvent が無い）。
  // 中心・ズームが変わっていなくても、変わっていても（resize の _constrain の補正）絞らない
  map.resize([]);
  map.fire('moveend', {});
  await settle();
  assert.equal(bboxReqs(env).length, 3, '大きさだけの変化ではカードを絞らない');
  const camBefore = {...env.fake.center};
  env.fake.constrain = () => { env.fake.center = {lng: 131.1, lat: 31.05}; env.fake.zoom = 3; env.fake.bounds = {w: 100, s: 10, e: 150, n: 50}; };
  map.resize([]);                                      // 偽の resize が中心・ズームを補正して moveend を起こす
  env.fake.constrain = null;
  await settle();
  assert.notDeepEqual(plain(env.fake.center), plain(camBefore), '補正で中心が動いた');
  assert.equal(bboxReqs(env).length, 3, 'resize の補正（中心・ズームが変わる）でもカードを絞らない');
  assert.deepEqual(plain(env.ev('ls.mapBbox')), [131, 31, 131.5, 31.25], '絞り込みは前のまま');
  env.fake.bounds = {w: 131, s: 31, e: 131.5, n: 31.25};
  // この範囲の写真を見る: 表示範囲の bbox
  env.$('#lib-bbox').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '131,31,131.5,31.25');
  assert.equal(q.get('from'), null);
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'bbox', bbox: [131, 31, 131.5, 31.25]});
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲');
  assert.equal(placeQs(env).at(-1).get('country'), null, 'カードの読み直しに場所の選択は載らない');
  // 全体に戻す: 日本全体へ（libMove 付き）、絞り込みを外して読み直す。moveend では読まない
  const reqs = placeQs(env).length;
  env.$('#lib-reset').click();
  await settle();
  assert.deepEqual(plain(map.calls.fitBounds.at(-1)), {bounds: [122, 24, 154, 46], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}});
  assert.equal(env.ev('ls.mapBbox'), null);
  assert.equal(placeQs(env).length, reqs + 1);
  q = placeQs(env).at(-1);
  assert.equal(q.get('bbox'), null, 'bbox が外れる');
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県', '試験県', '地域未判定']);
  assert.equal(env.$('#lib-note').textContent, '1 か国');
  map.endMove();
  await settle();
  assert.equal(placeQs(env).length, reqs + 1, '全体に戻すの moveend では読まない');
});

await test('(v2) AC7 カード・チップの選択の fitBounds のあとは読まない。カードを続けて 2 回選ぶ（中断された 1 回目の moveend も含め）読まない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.places = withBboxOnly();
  await openPlaces(env);
  env.raf();
  const map = mapOf(env);
  const base = placeQs(env).length;
  mainOf(pcards(env)[0]).click();                      // 県 → fitBounds #1
  await settle();
  assert.equal(map.moves.length, 1);
  map.endMove();                                        // libMove 付きの moveend
  await settle();
  assert.equal(placeQs(env).length, base, '選択の fitBounds のあとは読まない');
  // 続けて 2 回: チップを押す（#2）→ 終わる前に県を押す（#3）
  cityChips(pcards(env)[0])[0].click();                 // 見本町（#2）
  await settle();
  env.fake.bounds = {w: 100, s: 10, e: 120, n: 30};     // 移動の途中の表示範囲（県の範囲とは違う）
  mainOf(pcards(env)[0]).click();                       // 県を選ぶ（#3）。市区町村の選択から県の選択に替わる
  await settle();
  assert.equal(map.moves.length, 2, '#2 と #3 が積まれている');
  map.interrupt();                                      // 本物は #3 が #2 を中断し、#2 の moveend を（libMove 付きで）起こす
  map.fire('moveend', {libMove: true});
  map.endMove();
  await settle();
  assert.equal(placeQs(env).length, base, '中断された 1 回目の moveend も含め読まない');
  assert.equal(env.ev('ls.mapBbox'), null);
  // 自動の移動の途中で利用者がドラッグ: 移動の moveend（libMove）は読まず、ドラッグの moveend（libMove なし）で読む
  cityChips(pcards(env)[0])[0].click();                 // fitBounds（積む）
  await settle();
  map.endMove();                                        // 中断された移動の moveend（libMove）
  await settle();
  assert.equal(placeQs(env).length, base);
  userMove(env, {bounds: {w: 130.4, s: 30.4, e: 130.7, n: 30.7}, center: {lng: 130.55, lat: 30.55}});
  await settle();
  assert.equal(bboxReqs(env).length, 1, '自動の移動の途中のドラッグで読む');
  assert.equal(bboxReqs(env)[0].get('bbox'), '130.4,30.4,130.7,30.7');
  // 自動の移動の途中にクラスタを押す: 中断された移動は読まず、クラスタの easeTo の moveend で読む
  mainOf(pcards(env)[0]).click();                       // 県を選ぶ（市区町村の選択から替わる）→ fitBounds
  await settle();
  assert.equal(map.moves.length, 1);
  map.fire('click', {features: [{properties: {cluster_id: 9}, geometry: {coordinates: [130.55, 30.55]}}]}, 'lib-clusters');
  await settle();
  const before = bboxReqs(env).length;
  map.interrupt();                                      // fitBounds が中断される
  map.fire('moveend', {libMove: true});
  await settle();
  assert.equal(bboxReqs(env).length, before, '中断された自動の移動では読まない');
  env.fake.bounds = {w: 130.5, s: 30.5, e: 130.6, n: 30.6};
  map.endMove();                                        // クラスタの easeTo の moveend
  await settle();
  assert.equal(bboxReqs(env).length, before + 1);
  assert.equal(bboxReqs(env).at(-1).get('bbox'), '130.5,30.5,130.6,30.6');
});

await test('(v2b) AC7 利用者の移動の判定: originalEvent 付き・libUser 付きの moveend だけ読む。印の無い moveend（resize の補正）・libMove 付きでは読まない。originalEvent があっても libMove 付きなら読まない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.handlers.places = withBboxOnly();
  await openPlaces(env);
  env.raf();
  const map = mapOf(env);
  assert.equal(bboxReqs(env).length, 0);
  const fireEnd = async (event, bounds) => {
    env.fake.bounds = bounds;
    map.fire('moveend', event);
    await settle();
    return bboxReqs(env).length;
  };
  // 印の無い moveend: 範囲が変わっていても読まない（ResizeObserver 経由の resize とその補正）
  assert.equal(await fireEnd(undefined, {w: 1, s: 2, e: 3, n: 4}), 0, 'イベントの内容が無い');
  assert.equal(await fireEnd({}, {w: 1, s: 2, e: 3, n: 4}), 0, '印が無い');
  assert.equal(await fireEnd({0: {contentRect: {}}, length: 1}, {w: 1, s: 2, e: 3, n: 4}), 0, 'ResizeObserver の entries 相当');
  // libMove 付きは読まない
  assert.equal(await fireEnd({libMove: true}, {w: 5, s: 6, e: 7, n: 8}), 0, 'libMove 付き');
  assert.equal(await fireEnd({libMove: true, originalEvent: {type: 'mouseup'}}, {w: 5, s: 6, e: 7, n: 8}), 0, 'libMove が優先');
  assert.equal(env.ev('ls.mapBbox'), null);
  // originalEvent 付きは読む（ドラッグ・ホイール・ダブルクリック・キーボード・ズームボタン）
  assert.equal(await fireEnd({originalEvent: {type: 'mouseup'}}, {w: 130.25, s: 30.5, e: 131.75, n: 31.5}), 1, 'originalEvent 付き');
  assert.equal(bboxReqs(env)[0].get('bbox'), '130.25,30.5,131.75,31.5');
  // libUser 付き（クラスタを押したときの easeTo）は読む
  assert.equal(await fireEnd({libUser: true}, {w: 130.5, s: 30.5, e: 131, n: 31}), 2, 'libUser 付き');
  assert.equal(bboxReqs(env)[1].get('bbox'), '130.5,30.5,131,31');
  // 場所の画面でない（地図が見えない）ときは読まない
  env.$('#lib-view-trips').click();
  await settle();
  assert.equal(await fireEnd({originalEvent: {type: 'mouseup'}}, {w: 9, s: 9, e: 9.5, n: 9.5}), 2, '地図が見えていない');
});

await test('(v2c) 地図の回転・傾斜は無効: dragRotate・touchZoomRotate（回転）・touchPitch・keyboard（回転）・boxZoom を disable。NavigationControl はコンパスなし', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  for(const call of ['dragRotate.disable', 'touchZoomRotate.disableRotation', 'touchPitch.disable', 'keyboard.disableRotation', 'boxZoom.disable'])
    assert.ok(map.disabled.includes(call), call);
  assert.ok(!map.disabled.includes('touchZoomRotate.disable'), 'ピンチのズームは残す');
  assert.ok(!map.disabled.includes('keyboard.disable'), '矢印キーの移動・ズームは残す');
  assert.deepEqual(plain(map.controls.map(c => c.control.options)), [{showCompass: false}]);
});

await test('(v3) カード用と候補用の /places は保存先・seq・loaded を分け、逆の順で返してもそれぞれ最新だけを使う。期間・除外が変わると両方を読み直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const held = [];
  env.handlers.places = params => new Promise(res => held.push({bbox: params.get('bbox'), hidden: params.get('hidden'),
    send: body => res({ok: true, status: 200, json: () => Promise.resolve(body)})}));
  const body = names => ({ok: true, abroad: 0, prefs: names.map((pref, i) => ({pref, count: 10 - i, cover: null, bbox: null, cities: []}))});
  await openPlaces(env);
  assert.equal(held.length, 2, 'カード用と候補用');
  held[1].send(body(['候補だけ']));                    // 候補用が先に返る
  await settle();
  assert.equal(env.ev('ls.loaded.placeOptions'), true);
  assert.equal(env.ev('ls.loaded.placeCards'), false, 'カード用はまだ');
  held[0].send(body(['カード甲', 'カード乙']));
  await settle();
  assert.deepEqual(pcards(env).map(cardName), ['カード甲', 'カード乙']);
  assert.deepEqual(plain(env.ev('ls.placeOpts.prefs.map(p => p.pref)')), ['候補だけ'], '候補はカードに混ざらない');
  assert.deepEqual(plain(env.ev('ls.prefs.map(p => p.pref)')), ['カード甲', 'カード乙']);
  // 地図を 2 回動かす: 古いカードの応答（後から返る）は捨てる
  userMove(env, {bounds: {w: 1, s: 2, e: 3, n: 4}, center: {lng: 2, lat: 3}});
  userMove(env, {bounds: {w: 5, s: 6, e: 7, n: 8}, center: {lng: 6, lat: 7}});
  await settle();
  const moves = held.slice(2);
  assert.deepEqual(moves.map(h => h.bbox), ['1,2,3,4', '5,6,7,8']);
  moves[1].send(body(['新しい範囲']));
  await settle();
  moves[0].send(body(['古い範囲']));
  await settle();
  assert.deepEqual(pcards(env).map(cardName), ['新しい範囲']);
  assert.equal(env.ev('ls.placeOpts.prefs.length'), 1, '地図の移動は候補を読み直さない');
  // 除外の切り替え: 両方の seq が増え、飛んでいる応答は捨てる。両方を hidden=1 で読み直す
  userMove(env, {bounds: {w: 9, s: 10, e: 11, n: 12}, center: {lng: 10, lat: 11}});    // カード用（保留）
  await settle();
  const stale = held.at(-1);
  const n = held.length;
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const fresh = held.slice(n);
  assert.equal(fresh.length, 2, 'カード用と候補用を読み直す');
  assert.ok(fresh.every(h => h.hidden === '1'));
  fresh[1].send(body(['候補新']));
  fresh[0].send(body(['カード新']));
  await settle();
  stale.send(body(['飛んでいた応答']));
  await settle();
  assert.deepEqual(pcards(env).map(cardName), ['カード新']);
  assert.deepEqual(plain(env.ev('ls.placeOpts.prefs.map(p => p.pref)')), ['候補新']);
  // 見えていない view で期間が変わる（地図で見る）と、両方の seq が増えて loaded が落ちる
  env.$('#lib-view-trips').click();
  await settle();
  const k = held.length;
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(held.length, k, '見えていなければ読まない');
  assert.equal(env.ev('ls.loaded.placeCards'), false);
  assert.equal(env.ev('ls.loaded.placeOptions'), false);
});

await test('(v4) 地図の大きさ: 場所を出したとき・詳細の出し入れ・幅 640px の境をまたいだとき、描画の確定後に resize({libMove:true})。毎回の選択では予約しない。resize の moveend ではカードを絞らない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  assert.equal(env.rafQueue.length, 0);
  await openPlaces(env);
  const map = mapOf(env);
  assert.equal(map.calls.resize, 0, '描画の確定前は呼ばない');
  assert.equal(env.raf(), 1);
  assert.deepEqual(plain(map.calls.resizeArgs), [{libMove: true}]);
  const base = placeQs(env).length;
  assert.equal(placeQs(env).filter(q => q.get('bbox')).length, 0, 'resize の moveend で読まない');
  mainOf(pcards(env)[0]).click();                      // 詳細が出る
  await settle();
  assert.equal(env.raf(), 1, '詳細が出たとき');
  mainOf(pcards(env)[1]).click();                      // 詳細は出たまま切り替わる
  await settle();
  assert.equal(env.rafQueue.length, 0, '選択の切り替えでは予約しない');
  mainOf(pcards(env)[1]).click();                      // 外す → 詳細が隠れる
  assert.equal(env.raf(), 1, '詳細が隠れたとき');
  env.narrow(true);                                     // 幅 640px の境をまたいだ
  assert.equal(env.raf(), 1);
  env.narrow(false);
  assert.equal(env.raf(), 1);
  assert.equal(map.calls.resize, 5);
  assert.ok(map.calls.resizeArgs.every(a => a && a.libMove === true));
  assert.equal(placeQs(env).length, base, 'resize ではカードを読み直さない');
  // 別のタブにいるあいだは合わせない
  env.$('.tab[data-tab=status]').click();
  await settle();
  env.narrow(true);
  assert.equal(env.raf(), 0, 'タブが見えていなければ予約しない');
  assert.equal(map.calls.resize, 5, 'タブが見えていなければ resize しない');
  // 旅の画面では予約しない
  await openLib(env);
  env.raf();
  env.$('#lib-view-trips').click();
  await settle();
  env.narrow(false);
  assert.equal(env.rafQueue.length, 0);
});

await test('(v5) 詳細 #lib-detail は旅・日付では一覧の下（#v-library の末尾）、場所では右の列の地図の下。場所を出すだけでは詳細を移すだけで消えない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const detail = env.$('#lib-detail');
  same(detail.parentNode, env.$('#v-library'));
  pressTrip(env, 0);
  await settle();
  same(detail.parentNode, env.$('#v-library'));
  await openPlaces(env);
  same(detail.parentNode, env.$('#lib-place-main'));
  assert.equal(hidden(env, '#lib-detail'), true, '場所の選択は無い');
  const kids = env.$('#lib-place-main').children;
  same(kids.at(-1), detail, '地図のあと');
  assert.equal(env.$('#lib-map').parentNode, env.$('#lib-place-main'));
  env.$('#lib-view-trips').click();
  await settle();
  same(detail.parentNode, env.$('#v-library'));
  assert.equal(hidden(env, '#lib-detail'), false, '旅の選択の詳細');
  assert.equal(env.$$('#lib-grid .lib-tile').length, 3);
  assert.equal(env.ev("typeof ls.sel.map"), 'undefined', '地図の view の選択は無い');
});

// ---- T7: 旅の検索・日付の言葉とピッカーの範囲・場所のコンボボックス ----
const searchBox = env => env.$('#lib-search');
const whenBox = env => env.$('#lib-when');
const comboBox = env => env.$('#lib-place-combo');
const typeInto = (box, text) => { box.value = text; box.dispatchEvent({type: 'input'}); };
const keyOn = (box, key, extra = {}) => box.dispatchEvent({type: 'keydown', key, ...extra});
const photoQs = env => env.reqs('/api/admin/library/photos').map(qOf);
const whenReqs = env => env.reqs('/api/admin/library/when');
const optsOf = env => env.$$('#lib-place-list .lib-opt');
const jsonRes = body => ({ok: true, status: 200, json: () => Promise.resolve(body)});

await test('(w0) 探す欄は view ごとに 1 つだけ出る: 旅は #lib-search、日付は #lib-when と #lib-when-note、場所は #lib-place-combo', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const shown = () => ['#lib-search', '#lib-when', '#lib-when-note', '#lib-place-combo'].filter(s => !hidden(env, s));
  assert.deepEqual(shown(), ['#lib-search']);
  env.$('#lib-view-calendar').click();
  await settle();
  assert.deepEqual(shown(), ['#lib-when', '#lib-when-note']);
  env.$('#lib-view-places').click();
  await settle();
  assert.deepEqual(shown(), ['#lib-place-combo']);
  assert.equal(hidden(env, '#lib-place-list'), true, '候補は打つまで出ない');
  same(comboBox(env).parentNode.parentNode, env.$('.lib-bar'), '探す欄は .lib-bar の中（.lib-place-side の overflow に切られない）');
  assert.equal(comboBox(env).parentNode.classList.contains('hidden'), false);
  env.$('#lib-view-trips').click();
  await settle();
  assert.deepEqual(shown(), ['#lib-search']);
  assert.equal(comboBox(env).parentNode.classList.contains('hidden'), true, 'コンボボックスの包みも隠す');
});

await test('(w1) AC9 旅の検索: 打つと rangeLabel と placeLabel の文字（小文字・語のすべて）でカードが減り、読まない。Enter（変換中を除く）で /photos?q=…、見出し「“…” の検索」', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const input = searchBox(env);
  assert.equal(cards(env).length, 3);
  typeInto(input, '架空');
  assert.equal(cards(env).length, 1, '地名に「架空」を持つ旅だけ（日時なしのカードも出さない）');
  assert.equal(mainOf(cards(env)[0]).querySelector('.lib-card-name').textContent, '2030-05-03 09:12 〜 2030-05-05 18:40');
  assert.equal(env.$('#lib-note').textContent, '旅 1 / 2 件');
  typeInto(input, '2029 01');
  assert.deepEqual(cards(env).map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2029-01-01 00:00'], '語のすべてが合うもの');
  typeInto(input, '2029　架空');
  assert.equal(cards(env).length, 0, '全角の空白でも語に分ける。どちらの旅も 2 語を満たさない');
  assert.equal(env.$('#lib-trips').textContent, '一致する旅がありません');
  typeInto(input, '海外・不明');
  assert.equal(cards(env).length, 1, '地名の海外・不明（placeLabel）にも合う');
  typeInto(input, '  ');
  assert.equal(cards(env).length, 3, '空白だけなら絞らない');
  assert.equal(env.$('#lib-note').textContent, '旅 2 件');
  typeInto(input, 'ABC');
  assert.equal(env.$('#lib-trips').textContent, '一致する旅がありません');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '打っただけでは写真を読まない');
  assert.equal(env.reqs('/api/admin/library/trips').length, 1, '旅の一覧も読み直さない');
  // 変換中の Enter では読まない
  typeInto(input, '架空 試験');
  keyOn(input, 'Enter', {isComposing: true});
  keyOn(input, 'Enter', {keyCode: 229});
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, 0);
  // Enter で文字の合う写真
  keyOn(input, 'Enter');
  await settle();
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('q'), '架空 試験');
  assert.equal(q.get('page'), '1');
  assert.equal(q.get('from'), null);
  assert.equal(q.get('hidden'), null);
  assert.equal(env.$('#lib-sel').textContent, '“架空 試験” の検索');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.deepEqual(plain(env.ev('ls.sel.trips')), {kind: 'q', q: '架空 試験'});
  assert.ok(env.$('#lib-trip-cards').classList.contains('is-strip'), '選んだあいだは 1 段');
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502', '503']);
  // 前後の空白は除いて送る。別の語で Enter → 切り替わる
  typeInto(input, '  架空 ');
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('q'), '架空');
  // 旅のカードを押すと検索の選択は旅に替わる（検索欄の文字は残る）
  pressTrip(env, 0);
  await settle();
  assert.equal(env.ev('ls.sel.trips.kind'), 'trip');
  assert.equal(input.value, '  架空 ');
  // 空で Enter → 選択を外す（読まない）
  typeInto(input, '');
  const before = env.reqs('/api/admin/library/photos').length;
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  assert.equal(env.ev('ls.sel.trips'), null);
  assert.equal(hidden(env, '#lib-detail'), true);
  keyOn(input, 'Enter');                              // 選択が無いときの空の Enter は何も起きない
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
});

await test('(w2) AC12 日付の言葉: Enter（変換中を除く）で when?q=、読み込み中は「読んでいます…」、成功で期間・読み方を #lib-when-note に出して /photos?from=&to=。規則と LLM', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const input = whenBox(env), note = env.$('#lib-when-note');
  let resolve;
  env.handlers.when = () => new Promise(res => { resolve = body => res(jsonRes(body)); });
  input.value = '去年の夏';
  keyOn(input, 'Enter', {isComposing: true});
  await settle();
  assert.equal(whenReqs(env).length, 0, '変換中の Enter では読まない');
  keyOn(input, 'Enter');
  await settle();
  assert.equal(whenReqs(env).length, 1);
  assert.equal(whenReqs(env)[0].url, '/api/admin/library/when?q=' + encodeURIComponent('去年の夏'));
  assert.equal(note.textContent, '読んでいます…');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '応答が来るまで写真は読まない');
  resolve({ok: true, from: '2025-06-01 00:00', to: '2025-08-31 23:59', source: 'rule'});
  await settle();
  assert.equal(note.textContent, '2025-06-01〜2025-08-31 として検索（規則）');
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2025-06-01 00:00');
  assert.equal(q.get('to'), '2025-08-31 23:59');
  assert.equal(q.get('page'), '1');
  assert.equal(env.$('#lib-sel').textContent, '2025-06-01〜2025-08-31');
  assert.deepEqual(plain(env.ev('ls.sel.calendar')), {kind: 'words', from: '2025-06-01 00:00', to: '2025-08-31 23:59', source: 'rule'});
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-months'), true, '言葉の選択に月のチップは出さない');
  assert.ok(env.$('#lib-year-cards').classList.contains('is-strip'));
  // LLM の読み。同じ日は 1 つだけ
  input.value = '娘が生まれた頃';
  keyOn(input, 'Enter');
  await settle();
  resolve({ok: true, from: '2025-08-15 00:00', to: '2025-08-15 23:59', source: 'llm'});
  await settle();
  assert.equal(note.textContent, '2025-08-15 として検索（LLM）');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2025-08-15 00:00');
  // 別の画面（旅→日付）へ往復しても、選択が words のままなら言葉の文が出る
  env.$('#lib-view-trips').click();
  await settle();
  env.$('#lib-view-calendar').click();
  await settle();
  assert.equal(note.textContent, '2025-08-15 として検索（LLM）');
  // 空で Enter → 言葉の選択を外す
  input.value = '  ';
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.ev('ls.sel.calendar'), null);
  assert.equal(note.textContent, '');
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(whenReqs(env).length, 2, '空では読まない');
});

await test('(w2b) AC12 日付の言葉の失敗: unreadable・400・403・通信の失敗は「読めませんでした。ピッカーで選んでください」、unavailable は「（LLM が使えません）」を添える。選択は変えない', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const input = whenBox(env), note = env.$('#lib-when-note');
  mainOf(yearCards(env)[0]).click();                  // 2030 年を選んでおく
  await settle();
  const photos = env.reqs('/api/admin/library/photos').length;
  const MSG = '読めませんでした。ピッカーで選んでください';
  const ask = async (text, handler) => {
    env.handlers.when = handler;
    input.value = text;
    keyOn(input, 'Enter');
    await settle();
  };
  await ask('あいまい', () => reply({ok: false, error: 'unreadable'}));
  assert.equal(note.textContent, MSG);
  await ask('あいまい2', () => reply({ok: false, error: 'unavailable'}));
  assert.equal(note.textContent, MSG + '（LLM が使えません）');
  await ask('あいまい3', () => respond(400, {ok: false, error: 'bad request'}));
  assert.equal(note.textContent, MSG, '400 も読めない扱い（LLM の文は付けない）');
  await ask('あいまい4', () => respond(403, {ok: false, error: 'forbidden'}));
  assert.equal(note.textContent, MSG);
  await ask('あいまい5', () => Promise.reject(new Error('network')));
  assert.equal(note.textContent, MSG);
  await ask('あいまい6', () => reply({ok: true, from: 5, to: null, source: 'rule'}));
  assert.equal(note.textContent, MSG, '形の違う成功の応答も読めない扱い');
  assert.equal(env.reqs('/api/admin/library/photos').length, photos, '失敗では写真を読み直さない');
  assert.equal(env.ev('ls.sel.calendar.kind'), 'year', '選択は変えない');
  assert.equal(hidden(env, '#lib-detail'), false);
  // 失敗の文は、別の選択をすると消える
  mainOf(yearCards(env)[1]).click();
  await settle();
  assert.equal(note.textContent, '');
});

await test('(w3) AC16a 言葉の応答を待つあいだに月を選ぶと、遅れて来た応答は選択を変えない。続けて 2 回 Enter したときは新しい方だけ。除外の切り替えでも捨てる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const input = whenBox(env), note = env.$('#lib-when-note');
  const held = [];
  env.handlers.when = () => new Promise(res => held.push(body => res(jsonRes(body))));
  const words = (from, to) => ({ok: true, from: from + ' 00:00', to: to + ' 23:59', source: 'rule'});
  mainOf(yearCards(env)[0]).click();
  await settle();
  input.value = '去年の夏';
  keyOn(input, 'Enter');
  await settle();
  assert.equal(note.textContent, '読んでいます…');
  monthChips(env)[4].click();                          // 待つあいだに 5 月を選ぶ
  await settle();
  assert.equal(note.textContent, '', '読んでいますの表示も消える');
  const photos = env.reqs('/api/admin/library/photos').length;
  held[0](words('2025-06-01', '2025-08-31'));
  await settle();
  assert.equal(env.ev('ls.sel.calendar.kind'), 'month', '遅れた応答は選択を変えない');
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2030-05-00 00:00');
  assert.equal(env.reqs('/api/admin/library/photos').length, photos, '写真も読み直さない');
  assert.equal(note.textContent, '');
  // 続けて 2 回 Enter: 古い方の応答は捨て、新しい方を使う
  input.value = 'A';
  keyOn(input, 'Enter');
  input.value = 'B';
  keyOn(input, 'Enter');
  await settle();
  held[2](words('2024-01-01', '2024-01-31'));
  await settle();
  assert.equal(env.ev('ls.sel.calendar.from'), '2024-01-01 00:00');
  held[1](words('2023-01-01', '2023-01-31'));
  await settle();
  assert.equal(env.ev('ls.sel.calendar.from'), '2024-01-01 00:00', '古い Enter の応答は捨てる');
  // 除外を切り替えると飛んでいる応答は捨てる
  input.value = 'C';
  keyOn(input, 'Enter');
  await settle();
  env.$('#lib-hidden').checked = true;
  assert.equal(note.textContent, '読んでいます…');
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(note.textContent, '2024-01-01〜2024-01-31 として検索（規則）', '切り替え直後に読んでいますは解け、選んでいる期間の文に戻る');
  held[3](words('2022-01-01', '2022-01-31'));
  await settle();
  assert.equal(env.ev('ls.sel.calendar.from'), '2024-01-01 00:00');
  assert.equal(note.textContent, '2024-01-01〜2024-01-31 として検索（規則）', '遅れた応答が来ても変わらない');
  // view を替えて戻したあとでも、替える前に出した応答は捨てる
  input.value = 'D';
  keyOn(input, 'Enter');
  await settle();
  env.$('#lib-view-trips').click();
  env.$('#lib-view-calendar').click();
  await settle();
  held[4](words('2021-01-01', '2021-01-31'));
  await settle();
  assert.equal(env.ev('ls.sel.calendar.from'), '2024-01-01 00:00');
  assert.equal(note.textContent, '2024-01-01〜2024-01-31 として検索（規則）', '読んでいますは残らない');
});

await test('(w4) AC13 ピッカー: 写真の無い月は押せない。Shift で anchor と押したものの形の範囲の min(from)〜max(to)（逆向き・年またぎ・年と月の混在）。anchor が無い Shift は普通の選択', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const shiftClick = node => node.dispatchEvent({type: 'click', shiftKey: true});
  const lastQ = () => env.last('/api/admin/library/photos');
  const sel = () => plain(env.ev('ls.sel.calendar'));
  // anchor が無い Shift は普通の選択
  assert.equal(env.ev('ls.anchor'), null);
  shiftClick(mainOf(yearCards(env)[0]));
  await settle();
  assert.equal(sel().kind, 'year');
  assert.equal(lastQ().get('from'), '2030-00-00 00:00');
  assert.deepEqual(plain(env.ev('ls.anchor')), {from: '2030-00-00 00:00', to: '2030-99-99 99:99', year: '2030'});
  // 写真の無い月は押せない（Shift でも）。anchor は変わらない
  const before = env.reqs('/api/admin/library/photos').length;
  monthChips(env)[0].click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  // 月 12 を普通に押す → anchor は 12 月。Shift で 5 月（逆向き）
  monthChips(env)[11].click();
  await settle();
  assert.deepEqual(plain(env.ev('ls.anchor')), {from: '2030-12-00 00:00', to: '2030-12-99 99:99', year: '2030'});
  shiftClick(monthChips(env)[4]);
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2030-05-00 00:00', to: '2030-12-99 99:99'});
  assert.equal(lastQ().get('from'), '2030-05-00 00:00');
  assert.equal(lastQ().get('to'), '2030-12-99 99:99');
  assert.equal(lastQ().get('page'), '1');
  assert.equal(env.$('#lib-sel').textContent, '2030年5月〜2030年12月');
  assert.equal(hidden(env, '#lib-months'), false, '範囲を選んだあとも、起点の年の月は出したまま');
  assert.equal(monthChips(env).length, 12);
  assert.ok(!monthChips(env)[11].classList.contains('is-selected'), '範囲を選んだ月のチップ自体は押した状態にならない（exact は範囲だけ）');
  // 順向き: 5 月を普通に押して Shift で 12 月
  monthChips(env)[4].click();
  await settle();
  assert.equal(sel().kind, 'month');
  shiftClick(monthChips(env)[11]);
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2030-05-00 00:00', to: '2030-12-99 99:99'});
  // 年またぎ・混在: 月 5（anchor）に Shift で 2029 年のカード → 年の範囲との min〜max
  shiftClick(mainOf(yearCards(env)[1]));
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2029-00-00 00:00', to: '2030-05-99 99:99'});
  assert.equal(env.$('#lib-sel').textContent, '2029年〜2030年5月');
  assert.equal(lastQ().get('from'), '2029-00-00 00:00');
  assert.equal(lastQ().get('to'), '2030-05-99 99:99');
  // anchor は変わらないので、続けて別の Shift は同じ起点から広げる
  shiftClick(monthChips(env)[11]);
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2030-05-00 00:00', to: '2030-12-99 99:99'});
  // 年→年: 2029 年を普通に押し、Shift で 2030 年
  mainOf(yearCards(env)[1]).click();
  await settle();
  assert.equal(sel().kind, 'year');
  shiftClick(mainOf(yearCards(env)[0]));
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2029-00-00 00:00', to: '2030-99-99 99:99'});
  assert.equal(env.$('#lib-sel').textContent, '2029年〜2030年');
  // 月（2029-01）→ Shift で 2030 年のカード（月と年の混在・年またぎ）
  mainOf(yearCards(env)[1]).click();                  // 範囲から 2029 年の選択に替わる
  await settle();
  assert.equal(sel().kind, 'year');
  mainOf(yearCards(env)[1]).click();                  // 選択中をもう一度 → 外す
  await settle();
  assert.equal(env.ev('ls.sel.calendar'), null);
  assert.equal(env.ev('ls.anchor'), null, '選択を外すと anchor も外れる');
  mainOf(yearCards(env)[1]).click();
  await settle();
  monthChips(env)[0].click();
  await settle();
  assert.equal(sel().kind, 'month');
  shiftClick(mainOf(yearCards(env)[0]));
  await settle();
  assert.deepEqual(sel(), {kind: 'span', from: '2029-01-00 00:00', to: '2030-99-99 99:99'});
  assert.equal(env.$('#lib-sel').textContent, '2029年1月〜2030年');
  // 日時なしを選ぶと anchor が外れ、そのあとの Shift は普通の選択
  mainOf(yearCards(env)[2]).click();
  await settle();
  assert.equal(env.ev('ls.anchor'), null);
  shiftClick(mainOf(yearCards(env)[0]));
  await settle();
  assert.equal(sel().kind, 'year');
  // 言葉を選んでも anchor は外れる
  env.handlers.when = () => reply({ok: true, from: '2025-06-01 00:00', to: '2025-08-31 23:59', source: 'rule'});
  whenBox(env).value = '去年の夏';
  keyOn(whenBox(env), 'Enter');
  await settle();
  assert.equal(env.ev('ls.anchor'), null);
  shiftClick(mainOf(yearCards(env)[1]));
  await settle();
  assert.equal(sel().kind, 'year', 'anchor が無いので普通の選択');
});

// 長野県（423）・愛媛県・千葉県・東京都と、市区町村。北海道は市区町村が空だけ
const comboBody = () => ({ok: true, abroad: 0, prefs: [
  {pref: '長野県', count: 423, cover: null, bbox: [137.3, 35.2, 138.7, 37.0], cities: [
    {city: '長野市', count: 120, bbox: [138.0, 36.5, 138.3, 36.8]}, {city: '松本市', count: 80, bbox: [137.8, 36.1, 138.1, 36.4]},
    {city: '軽井沢町', count: 40, bbox: null}]},
  {pref: '東京都', count: 300, cover: null, bbox: [139.0, 35.5, 139.9, 35.9], cities: [{city: '世田谷区', count: 50, bbox: [139.5, 35.6, 139.7, 35.7]}]},
  {pref: '千葉県', count: 200, cover: null, bbox: [139.7, 35.0, 140.9, 36.0], cities: [
    {city: '千葉市', count: 100, bbox: [140.0, 35.5, 140.3, 35.7]}, {city: '松戸市', count: 70, bbox: [139.8, 35.7, 139.95, 35.9]}]},
  {pref: '愛媛県', count: 90, cover: null, bbox: [132.0, 33.0, 133.5, 34.2], cities: [
    {city: '松山市', count: 60, bbox: [132.7, 33.7, 132.9, 33.9]}, {city: '今治市', count: 20, bbox: [132.9, 33.9, 133.1, 34.1]},
    {city: '<b>町', count: 5, bbox: null}]},
  {pref: '北海道', count: 10, cover: null, bbox: null, cities: [{city: '', count: 10, bbox: null}]}]});
const labelsOf = env => optsOf(env).map(o => o.textContent);
// コンボボックスで決めた日本の地域・市区町村の選択（国 ID・地域 ID を持つ。名前は見出しの label と、市区町村を引く pref だけ）
const rkey = pref => JP + '/' + jpId(pref);
const selRegion = pref => ({kind: 'region', country: JP, region: jpId(pref), pref, label: '日本 ' + pref});
const selCity = (pref, city) => ({kind: 'city', country: JP, region: jpId(pref), pref, city, label: '日本 ' + pref + ' ' + city});

await test('(x1) AC14 コンボボックス: 「松」で「松」を含む候補だけ（件数の多い順）、「ながの」「nagano」「ナガノ」で長野県。読みは県・府・都の付いた形も。ARIA の属性と lib-opt-<添字>', async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(comboBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  const input = comboBox(env);
  assert.equal(input.getAttribute('role'), 'combobox');
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(input.getAttribute('aria-controls'), 'lib-place-list');
  assert.equal(input.getAttribute('aria-autocomplete'), 'list');
  assert.equal(input.getAttribute('aria-activedescendant'), null);
  assert.equal(env.$('#lib-place-list').getAttribute('role'), 'listbox');
  typeInto(input, '松');
  assert.deepEqual(labelsOf(env), ['長野県 松本市（80）', '千葉県 松戸市（70）', '愛媛県 松山市（60）']);
  assert.deepEqual(optsOf(env).map(o => o.id), ['lib-opt-0', 'lib-opt-1', 'lib-opt-2']);
  assert.ok(optsOf(env).every(o => o.getAttribute('role') === 'option'));
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  assert.equal(hidden(env, '#lib-place-list'), false);
  assert.equal(input.getAttribute('aria-activedescendant'), null, '打っただけでは強調しない');
  assert.ok(optsOf(env).every(o => o.getAttribute('aria-selected') === 'false'));
  // 県は件数の多い順で、市区町村より先。都道府県の名前の部分一致
  typeInto(input, '県');
  assert.deepEqual(labelsOf(env), ['長野県（423）', '千葉県（200）', '愛媛県（90）']);
  typeInto(input, '長野');
  assert.deepEqual(labelsOf(env), ['長野県（423）', '長野県 長野市（120）'], '都道府県 → 市区町村');
  // 読み: ひらがな・ローマ字・カタカナ・大文字
  for(const t of ['ながの', 'nagano', 'ナガノ', 'NAGANO', 'ながのけん', 'naganoken', 'ｎａｇａｎｏ']){
    typeInto(input, t);
    assert.deepEqual(labelsOf(env), ['長野県（423）'], t);
  }
  typeInto(input, 'ken');
  assert.deepEqual(labelsOf(env), [], 'ken だけで県の全部が出ない');
  assert.equal(env.$('#lib-place-list').textContent, '候補がありません');
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  typeInto(input, 'tokyo');
  assert.deepEqual(labelsOf(env), ['東京都（300）']);
  typeInto(input, 'とうきょうと');
  assert.deepEqual(labelsOf(env), ['東京都（300）']);
  typeInto(input, 'きょう');
  assert.deepEqual(labelsOf(env), ['東京都（300）'], '京都府は「きょうと」で、「きょう」は東京都の読みにも合う');
  typeInto(input, 'kyoto');
  assert.deepEqual(labelsOf(env), ['東京都（300）'], '部分一致なので「tokyoto」の中に kyoto がある。京都府は写真が無いので候補にない');
  // 市区町村の読みは持たない
  typeInto(input, 'matsumoto');
  assert.deepEqual(labelsOf(env), []);
  // 地名は textContent にだけ入る
  typeInto(input, '<b>');
  assert.deepEqual(labelsOf(env), ['愛媛県 <b>町（5）']);
  assert.equal(optsOf(env)[0].children.filter(c => c.nodeType === 1).length, 0, '要素は作らない（文字のノードだけ）');
  // 空にすると閉じる
  typeInto(input, '');
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(hidden(env, '#lib-place-list'), true);
  // city が空だけの北海道に市区町村の候補は無い。北海道は県として出る
  typeInto(input, 'hokkaido');
  assert.deepEqual(labelsOf(env), ['北海道（10）']);
  typeInto(input, '北');
  assert.deepEqual(labelsOf(env), ['北海道（10）']);
});

await test('(x2) AC14 20 件を超えたら、最後に選べない「ほか N 件（文字を足して絞ってください）」の行。↓・↑ はその行を飛ばす', async () => {
  const env = makeEnv();
  const cities = Array.from({length: 25}, (_, i) => ({city: '試験' + String(i).padStart(2, '0') + '市', count: 100 - i, bbox: null}));
  env.handlers.places = () => reply({ok: true, abroad: 0, prefs: [{pref: '長野県', count: 999, cover: null, bbox: null, cities}]});
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  const input = comboBox(env);
  typeInto(input, '試験');
  const rows = optsOf(env);
  assert.equal(rows.length, 21, '候補 20 件 + ほかの行');
  assert.equal(rows[0].textContent, '長野県 試験00市（100）');
  assert.equal(rows[19].textContent, '長野県 試験19市（81）');
  assert.equal(rows[20].textContent, 'ほか 5 件（文字を足して絞ってください）');
  assert.equal(rows[20].id, 'lib-opt-20');
  assert.equal(rows[20].getAttribute('aria-disabled'), 'true');
  assert.ok(rows[20].classList.contains('lib-opt-more'));
  // 選べない: クリックしても読まない。↓ で最後の候補から先頭へ回る
  rows[20].click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, 0);
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  keyOn(input, 'ArrowUp');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-19', '↑ は最後の候補（ほかの行ではない）');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  // 20 件ちょうどなら行は出ない
  typeInto(input, '試験0');
  assert.equal(optsOf(env).length, 10);
  assert.ok(optsOf(env).every(o => !o.classList.contains('lib-opt-more')));
});

await test('(x3) AC14 ↓↑ で aria-activedescendant を動かし、Enter で決める: 市区町村は pref・city、県は pref だけ。fitBounds（libMove 付き）。決めたあとは閉じる。クリックでも決める', async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(comboBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  const input = comboBox(env);
  const map = mapOf(env);
  typeInto(input, '松');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  assert.deepEqual(optsOf(env).map(o => o.getAttribute('aria-selected')), ['true', 'false', 'false']);
  assert.deepEqual(optsOf(env).map(o => o.classList.contains('is-active')), [true, false, false]);
  keyOn(input, 'ArrowDown');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-2');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0', '回り込む');
  keyOn(input, 'ArrowUp');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-2');
  assert.equal(optsOf(env)[2].scrollCalls.length > 0, true, '強調した候補を見える所へ');
  keyOn(input, 'ArrowUp');
  keyOn(input, 'ArrowUp');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  // 変換中のキーは無視する（↓ も Enter も）
  keyOn(input, 'ArrowDown', {isComposing: true});
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  keyOn(input, 'Enter', {isComposing: true});
  keyOn(input, 'Enter', {keyCode: 229});
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '変換中の Enter では決めない');
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  // Enter → 市区町村（長野県 松本市）
  const enter = {type: 'keydown', key: 'Enter'};
  input.dispatchEvent(enter);
  await settle();
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('region'), jpId('長野県'));
  assert.equal(q.get('city'), '松本市');
  assert.deepEqual(plain(env.ev('ls.sel.places')), selCity('長野県', '松本市'));
  assert.equal(env.$('#lib-sel').textContent, '日本 長野県 松本市');
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('pref'), null, '旧形式は混ぜない');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1)), {bounds: [137.8, 36.1, 138.1, 36.4], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}});
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(input.getAttribute('aria-activedescendant'), null);
  assert.equal(hidden(env, '#lib-place-list'), true);
  assert.equal(input.value, '松本市');
  const pref = pcards(env).find(c => c.dataset.key === rkey('長野県'));
  assert.ok(pref, '県のカード');
  assert.ok(pref.classList.contains('is-selected'), 'その県のカードが選ばれ');
  assert.ok(pref.classList.contains('has-cities'));
  assert.equal(cityChips(pref).find(c => c.dataset.key === rkey('長野県') + '/松本市').getAttribute('aria-pressed'), 'true');
  assert.equal(map.moves.length, 1, '地図は 1 回だけ動かす');
  // Enter で決めたあとの Enter（候補が閉じている）は何もしない
  const reqs = env.reqs('/api/admin/library/photos').length;
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, reqs);
  // 県だけ: pref だけ
  typeInto(input, 'ながの');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
  const p = env.last('/api/admin/library/photos');
  assert.equal(p.get('region'), jpId('長野県'));
  assert.equal(p.get('city'), null);
  assert.deepEqual(plain(env.ev('ls.sel.places')), selRegion('長野県'));
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [137.3, 35.2, 138.7, 37.0]);
  // 今選んでいるものをもう一度選んでも外れない（読み直さない）。地図は同じ範囲なら動かさない
  const n = env.reqs('/api/admin/library/photos').length;
  map.endMove();                                        // 地図が県の範囲に着いた
  env.fake.bounds = {w: 137.3, s: 35.2, e: 138.7, n: 37.0};
  typeInto(input, 'nagano');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
  assert.deepEqual(plain(env.ev('ls.sel.places')), selRegion('長野県'), '外れない');
  assert.equal(env.reqs('/api/admin/library/photos').length, n);
  // クリックでも決める。強調は要らない。bbox が null の市区町村は地図を動かさない
  const moves = map.calls.fitBounds.length;
  typeInto(input, '軽井沢');
  optsOf(env)[0].click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('city'), '軽井沢町');
  assert.equal(map.calls.fitBounds.length, moves);
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  // 候補の mousedown は入力欄のフォーカスを奪わない（blur で閉じない）
  typeInto(input, '松');
  const md = {type: 'mousedown'};
  optsOf(env)[0].dispatchEvent(md);
  assert.equal(md.defaultPrevented, true);
  // フォーカスが外れたら閉じる
  input.dispatchEvent({type: 'blur'});
  assert.equal(input.getAttribute('aria-expanded'), 'false');
});

await test('(x4) AC14 Esc で閉じる（閉じているときの Esc は何もしない）。閉じたあと ↓ で全部の候補を開き直せる。場所を離れると閉じる', async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(comboBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  const input = comboBox(env);
  typeInto(input, '松');
  keyOn(input, 'ArrowDown');
  const esc = {type: 'keydown', key: 'Escape'};
  input.dispatchEvent(esc);
  assert.equal(esc.defaultPrevented, true);
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(input.getAttribute('aria-activedescendant'), null);
  assert.equal(hidden(env, '#lib-place-list'), true);
  assert.equal(input.value, '松', '文字は残す');
  const esc2 = {type: 'keydown', key: 'Escape'};
  input.dispatchEvent(esc2);
  assert.notEqual(esc2.defaultPrevented, true, '閉じているときの Esc は他の処理に任せる');
  // Esc のあと ↓ で開き直す（文字に合う候補で、先頭を強調）
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  assert.equal(labelsOf(env).length, 3);
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  // 空の欄で ↓ → 全部の候補（県 4 + 市区町村）
  typeInto(input, '');
  keyOn(input, 'ArrowDown');
  assert.equal(optsOf(env).length, 15, '国 1（日本）+ 県 5（北海道を含む）+ 市区町村 9');
  assert.ok(labelsOf(env)[0].startsWith('日本（'), '国が先');
  assert.equal(labelsOf(env)[1], '長野県（423）');
  // 場所を離れると閉じる
  env.$('#lib-view-trips').click();
  await settle();
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(hidden(env, '#lib-place-list'), true);
});

await test('(x5) AC14 地図の範囲で隠れている県も選べる: mapBbox を外してカードを読み直してから選ぶ。市区町村のチップはそのあとに出る。読み直しの間に別の操作があれば選ばない', async () => {
  const env = makeEnv();
  const held = [];
  env.handlers.places = params => {
    if(!params.get('bbox')){                                               // 候補を決めたあとの読み直し（地図の絞り込みを外した要求）だけを保留できる
      const all = comboBody();
      return held.hold ? new Promise(res => held.push(() => res(jsonRes(all)))) : reply(all);
    }
    return reply({ok: true, abroad: 0, prefs: [comboBody().prefs[3]]});    // 地図の範囲には愛媛県だけ
  };
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  env.raf();
  userMove(env, {bounds: {w: 132, s: 33, e: 134, n: 34.5}});
  await settle();
  assert.deepEqual(pcards(env).map(cardName), ['愛媛県'], '地図の範囲の中は愛媛県だけ');
  const input = comboBox(env);
  typeInto(input, '松本');                              // 候補用は地図の絞り込みを掛けない: 長野県 松本市が出る
  assert.deepEqual(labelsOf(env), ['長野県 松本市（80）']);
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.ev('ls.mapBbox'), null, '地図の範囲による絞り込みは外れる');
  assert.equal(env.last('/api/admin/library/places').get('bbox'), null, 'カードは bbox なしで読み直した');
  assert.deepEqual(pcards(env).map(cardName), ['長野県', '東京都', '千葉県', '愛媛県', '北海道']);
  assert.deepEqual(plain(env.ev('ls.sel.places')), selCity('長野県', '松本市'));
  assert.equal(env.last('/api/admin/library/photos').get('region'), jpId('長野県'));
  assert.equal(env.last('/api/admin/library/photos').get('city'), '松本市');
  const pref = pcards(env).find(c => c.dataset.key === rkey('長野県'));
  assert.ok(pref.classList.contains('has-cities'), 'カードを読み直したあとにチップが出る');
  assert.equal(cityChips(pref).find(c => c.dataset.key === rkey('長野県') + '/松本市').getAttribute('aria-pressed'), 'true');
  assert.equal(env.$('#lib-note').textContent, '1 か国');
  assert.deepEqual(plain(mapOf(env).calls.fitBounds.at(-1).bounds), [137.8, 36.1, 138.1, 36.4]);
  // 読み直しの間に別の操作（view を替える）があれば、読み直したあとに選ばない
  mapOf(env).endMove();
  userMove(env, {bounds: {w: 132, s: 33, e: 134, n: 34.5}});
  await settle();
  assert.equal(env.ev('ls.mapBbox').length, 4);
  held.hold = true;
  typeInto(input, '東京');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
  assert.equal(env.ev('ls.mapBbox'), null);
  assert.equal(held.length, 1, 'bbox なしの読み直しが 1 件、保留されている');
  env.$('#lib-view-trips').click();
  await settle();
  const sel = JSON.stringify(plain(env.ev('ls.sel.places')));
  held.forEach(f => f());
  await settle();
  assert.equal(JSON.stringify(plain(env.ev('ls.sel.places'))), sel, '別の画面にいるあいだに来た読み直しでは選ばない');
});

await test('(x5b) AC14 R14 候補を決めたあとの読み直しの間に、候補の再選択・カードのクリックがあれば、あとから届いた応答は選択を変えない', async () => {
  const env = makeEnv();
  const held = [];
  env.handlers.places = params => {
    if(!params.get('bbox')){
      const all = comboBody();
      return held.hold ? new Promise(res => held.push(() => res(jsonRes(all)))) : reply(all);
    }
    return reply({ok: true, abroad: 0, prefs: [comboBody().prefs[3]]});    // 地図の範囲には愛媛県だけ
  };
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  env.raf();
  userMove(env, {bounds: {w: 132, s: 33, e: 134, n: 34.5}});
  await settle();
  assert.deepEqual(pcards(env).map(cardName), ['愛媛県']);
  mainOf(pcards(env)[0]).click();                      // 愛媛県を選ぶ（地図の範囲の中なのでカードがある）
  await settle();
  const input = comboBox(env);
  const choose = text => { typeInto(input, text); keyOn(input, 'ArrowDown'); keyOn(input, 'Enter'); };
  const prefSel = selRegion('愛媛県');
  // 隠れている長野県 松本市を候補から決める（読み直しを保留）→ 保留中に愛媛県を候補から再選択
  held.hold = true;
  choose('松本');
  await settle();
  assert.equal(held.length, 1, '読み直しが 1 件保留されている');
  assert.equal(env.ev('ls.mapBbox'), null);
  choose('愛媛');
  await settle();
  assert.deepEqual(plain(env.ev('ls.sel.places')), prefSel, '同じ県の再選択では選択を外さない');
  held.forEach(f => f());
  await settle();
  assert.deepEqual(plain(env.ev('ls.sel.places')), prefSel, '遅れて届いた応答で松本市に変わらない');
  assert.equal(env.last('/api/admin/library/photos').get('city'), null);
  // 保留中にカードをクリックしても、あとから届いた応答は選択を戻さない
  held.hold = false;
  held.length = 0;
  mapOf(env).endMove();
  userMove(env, {bounds: {w: 132, s: 33, e: 134, n: 34.5}});
  await settle();
  assert.equal(env.ev('ls.mapBbox').length, 4);
  held.hold = true;
  choose('松本');
  await settle();
  assert.equal(held.length, 1);
  mainOf(pcards(env)[0]).click();                      // 愛媛県のカード（読み直し前の一覧）をもう一度押して外す
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  held.forEach(f => f());
  await settle();
  assert.equal(env.ev('ls.sel.places'), null, '遅れて届いた応答で松本市が選ばれない');
});

await test('(y) AC15 除外の切り替えで、検索・言葉・候補の読み込みはすべて hidden=1（詳細はページ 1）。候補用の /places も読み直し、開いているリストは新しい候補になる', async () => {
  const env = makeEnv();
  env.handlers.places = params => reply(params.get('hidden') === '1'
    ? {ok: true, abroad: 0, prefs: [{pref: '長野県', count: 5, cover: null, bbox: null, cities: [{city: '松本市', count: 5, bbox: null}]}]}
    : comboBody());
  await env.boot();
  await openLib(env);
  const toggle = on => { env.$('#lib-hidden').checked = on; env.$('#lib-hidden').dispatchEvent({type: 'change'}); };
  // 旅の検索
  typeInto(searchBox(env), '松本');
  keyOn(searchBox(env), 'Enter');
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), null);
  env.handlers.photos = params => reply({ok: true, page: Number(params.get('page')), per: 60, pages: 3, total: 130, items: GRID});
  env.$('#lib-next').click();
  await settle();
  toggle(true);
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('q'), '松本');
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('page'), '1');
  // 日付の言葉
  env.$('#lib-view-calendar').click();
  await settle();
  env.handlers.when = () => reply({ok: true, from: '2025-06-01 00:00', to: '2025-08-31 23:59', source: 'rule'});
  whenBox(env).value = '去年の夏';
  keyOn(whenBox(env), 'Enter');
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('from'), '2025-06-01 00:00');
  assert.equal(whenReqs(env).length, 1);
  toggle(false);
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), null);
  assert.equal(env.last('/api/admin/library/photos').get('from'), '2025-06-01 00:00', '言葉の選択はそのまま');
  assert.equal(whenReqs(env).length, 1, '言葉を読み直さない（期間はそのまま）');
  // 場所: カード用と候補用の両方を hidden=1 で読み直す。開いているリストは新しい候補に替わる
  await openPlaces(env);
  typeInto(comboBox(env), '松');
  assert.deepEqual(labelsOf(env), ['長野県 松本市（80）', '千葉県 松戸市（70）', '愛媛県 松山市（60）']);
  const since = env.calls.length;
  toggle(true);
  await settle();
  const placeCalls = env.calls.slice(since).filter(c => c.url.startsWith('/api/admin/library/places'));
  assert.equal(placeCalls.length, 2, 'カード用と候補用');
  assert.ok(placeCalls.every(c => new URLSearchParams(c.url.split('?')[1]).get('hidden') === '1'));
  assert.deepEqual(labelsOf(env), ['長野県 松本市（5）'], '候補は除外を含めた件数で作り直す');
  // 候補を決めると、詳細も hidden=1
  keyOn(comboBox(env), 'ArrowDown');
  keyOn(comboBox(env), 'Enter');
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1');
  assert.equal(env.last('/api/admin/library/photos').get('city'), '松本市');
});

// ---- R14: クラスタの展開ズームの応答が、あとの操作より遅れて届く ----
// 偽の MapLibre は holdExpansion で応答を保留できる。保留中に別の操作をしてから解決しても、古いクラスタへ寄らず（easeTo しない）、
// libUser の moveend による /places?bbox=… の読み直しも起きない
await test('(y1) R14 クラスタの展開ズームの応答を待つあいだに、県カード・候補（同じ候補の選び直しも）・全体に戻す・期間の変更・除外・利用者の移動があると、古い応答では地図を動かさない。保留しない操作と、続けて押した 2 回目は動く', async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(comboBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  env.raf();
  const map = mapOf(env);
  const fake = env.fake;
  const pendingFor = async action => {                 // クラスタを押し、保留のまま action をして、解決する
    fake.holdExpansion = true;
    const before = fake.pendingExpansions.length;
    map.fire('click', {features: [{properties: {cluster_id: 9}, geometry: {coordinates: [137.5, 36.5]}}]}, 'lib-clusters');
    assert.equal(fake.pendingExpansions.length, before + 1, '展開ズームの応答は保留された');
    await action();
    await settle();
    fake.pendingExpansions[before].resolve();
    await settle();
    fake.holdExpansion = false;
  };
  const flushMoves = () => { while(map.moves.length) map.endMove(); };
  const assertQuiet = msg => {
    flushMoves();
    assert.equal(map.calls.easeTo.length, 0, msg + ': easeTo しない');
    assert.equal(bboxReqs(env).length, 0, msg + ': /places?bbox=… を読み直さない');
  };
  // 県のカード
  await pendingFor(() => { mainOf(pcards(env)[0]).click(); });
  assert.equal(env.ev('ls.sel.places.pref'), '長野県');
  assertQuiet('県カード');
  // 候補（市区町村）
  await pendingFor(() => {
    typeInto(comboBox(env), '松本');
    keyOn(comboBox(env), 'ArrowDown');
    keyOn(comboBox(env), 'Enter');
  });
  assert.equal(env.ev('ls.sel.places.city'), '松本市');
  assertQuiet('候補');
  // 同じ候補の選び直し: 選択（gen）は変わらず、地図を動かすだけ
  const gen = env.ev('ls.gen');
  await pendingFor(() => {
    typeInto(comboBox(env), '松本');
    keyOn(comboBox(env), 'ArrowDown');
    keyOn(comboBox(env), 'Enter');
  });
  assert.equal(env.ev('ls.gen'), gen, '同じ候補の選び直しは選択を変えない（seq.cluster だけで捨てる）');
  assert.equal(env.ev('ls.sel.places.city'), '松本市');
  assertQuiet('同じ候補の選び直し');
  // 全体に戻す（選択は変わらない）
  const gen2 = env.ev('ls.gen');
  await pendingFor(() => { env.$('#lib-reset').click(); });
  assert.equal(env.ev('ls.gen'), gen2);
  assertQuiet('全体に戻す');
  // 利用者の地図の移動（この移動の bbox の読み直しは 1 回だけ。古い応答はそれを増やさない）
  fake.holdExpansion = true;
  map.fire('click', {features: [{properties: {cluster_id: 9}, geometry: {coordinates: [137.5, 36.5]}}]}, 'lib-clusters');
  userMove(env, {bounds: {w: 137, s: 36, e: 138, n: 37}, center: {lng: 137.5, lat: 36.5}});
  await settle();
  assert.equal(bboxReqs(env).length, 1);
  fake.pendingExpansions.at(-1).resolve();
  await settle();
  fake.holdExpansion = false;
  flushMoves();
  assert.equal(map.calls.easeTo.length, 0, '利用者の移動: easeTo しない');
  assert.equal(bboxReqs(env).length, 1, '利用者の移動: 古い応答で読み直しが増えない');
  // 期間の変更（旅の「地図で見る」で入り、解除ボタンで外す）
  const periodsBefore = bboxReqs(env).length;
  await pendingFor(() => { mapBtnOf(env, 0).click(); });
  assert.deepEqual(plain(env.ev('ls.period')), {from: '2030-05-03 09:12', to: '2030-05-05 18:40'});
  flushMoves();
  assert.equal(map.calls.easeTo.length, 0, '期間に入る: easeTo しない');
  await pendingFor(() => { env.$('#lib-period-clear').click(); });
  assert.equal(env.ev('ls.period'), null);
  flushMoves();
  assert.equal(map.calls.easeTo.length, 0, '期間の解除: easeTo しない');
  assert.equal(bboxReqs(env).length, periodsBefore, '期間の変更: 古い応答で読み直しが増えない');
  // 除外を含める
  await pendingFor(() => { env.$('#lib-hidden').checked = true; env.$('#lib-hidden').dispatchEvent({type: 'change'}); });
  flushMoves();
  assert.equal(map.calls.easeTo.length, 0, '除外: easeTo しない');
  assert.equal(bboxReqs(env).length, periodsBefore, '除外: 古い応答で読み直しが増えない');
  // 点を読み直した（setData でクラスタの番号が振り直される）あとの古い応答。選択（gen）は変わらない
  env.$('#lib-hidden').checked = false; env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const gen3 = env.ev('ls.gen');
  const setData = map.sources.photos.setDataCalls.length;
  await pendingFor(() => { env.context.libPhotoChanged(501, {lat: 36, lon: 140}); });
  assert.equal(env.ev('ls.gen'), gen3);
  assert.equal(map.sources.photos.setDataCalls.length, setData + 1, '点を取り直した');
  flushMoves();
  assert.equal(map.calls.easeTo.length, 0, '点の読み直し: easeTo しない');
  // 続けて 2 回押す: 先の応答は捨て、あとのクラスタへだけ寄る
  fake.holdExpansion = true;
  const at = fake.pendingExpansions.length;
  map.fire('click', {features: [{properties: {cluster_id: 1}, geometry: {coordinates: [131, 31]}}]}, 'lib-clusters');
  map.fire('click', {features: [{properties: {cluster_id: 2}, geometry: {coordinates: [132, 32]}}]}, 'lib-clusters');
  fake.pendingExpansions[at + 1].resolve(12);
  await settle();
  fake.pendingExpansions[at].resolve(9);
  await settle();
  fake.holdExpansion = false;
  assert.deepEqual(plain(map.calls.easeTo), [{center: [132, 32], zoom: 12}], 'あとのクラスタだけ');
  flushMoves();
  // 保留しない普通の押し方（何も起きなければ寄る）
  const eased = map.calls.easeTo.length;
  map.fire('click', {features: [{properties: {cluster_id: 3}, geometry: {coordinates: [133, 33]}}]}, 'lib-clusters');
  await settle();
  assert.equal(map.calls.easeTo.length, eased + 1);
  assert.deepEqual(plain(map.calls.easeTo.at(-1)), {center: [133, 33], zoom: 11});
});

// 無効にするのは利用者の移動の「開始」。過去の展開の moveend（libUser）や、移動の終わりが応答のあとに来る順序で取りこぼさない
await test('(y2) R14 クラスタの展開の easeTo の途中で別のクラスタを押しても、前の展開の moveend であとの応答は捨てない。応答待ちにドラッグを始めたら、moveend より先に応答が返っても古いクラスタへは寄らない。初めての単発のホイールでも寄らない', async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(comboBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  env.raf();
  const map = mapOf(env);
  const fake = env.fake;
  const click = (id, at) => map.fire('click', {features: [{properties: {cluster_id: id}, geometry: {coordinates: at}}]}, 'lib-clusters');
  // (a) A の応答 → A の easeTo の途中で B を押す → A の moveend → B の応答
  fake.holdExpansion = true;
  click(1, [131, 31]);
  fake.pendingExpansions.at(-1).resolve(9);
  await settle();
  assert.deepEqual(plain(map.calls.easeTo), [{center: [131, 31], zoom: 9}], 'A へ寄り始めた');
  click(2, [132, 32]);
  const pendingB = fake.pendingExpansions.at(-1);
  map.endMove();                                           // A の moveend（libUser）。B の番号を進めない
  await settle();
  pendingB.resolve(12);
  await settle();
  fake.holdExpansion = false;
  assert.deepEqual(plain(map.calls.easeTo), [{center: [131, 31], zoom: 9}, {center: [132, 32], zoom: 12}], 'B の応答は捨てられず B へ寄る');
  while(map.moves.length) map.endMove();
  await settle();
  // (b) 押す → ドラッグ開始 → 応答（ドラッグの moveend より先）→ ドラッグ終了
  const eased = map.calls.easeTo.length;
  fake.holdExpansion = true;
  click(3, [133, 33]);
  map.fire('movestart', {originalEvent: {type: 'mousedown'}});
  fake.pendingExpansions.at(-1).resolve(10);
  await settle();
  fake.holdExpansion = false;
  assert.equal(map.calls.easeTo.length, eased, 'ドラッグ中に返った古い応答では寄らない');
  map.fire('moveend', {originalEvent: {type: 'mouseup'}});
  await settle();
  assert.equal(map.calls.easeTo.length, eased, 'ドラッグが終わっても寄らない');
  // libMove 付きの自動の移動の開始では捨てない
  fake.holdExpansion = true;
  click(4, [134, 34]);
  map.fire('movestart', {libMove: true});
  fake.pendingExpansions.at(-1).resolve(11);
  await settle();
  fake.holdExpansion = false;
  assert.equal(map.calls.easeTo.length, eased + 1, '自動の移動の開始では応答を捨てない');
  assert.deepEqual(plain(map.calls.easeTo.at(-1)), {center: [134, 34], zoom: 11});
  while(map.moves.length) map.endMove();
  await settle();
  // (c) 初めての単発のホイール: MapLibre は判別を待ってから originalEvent の無い movestart を起こす。ホイールそのもので捨てる
  const eased2 = map.calls.easeTo.length;
  fake.holdExpansion = true;
  click(5, [135, 35]);
  map.fire('wheel', {originalEvent: {type: 'wheel', deltaY: 100}});
  map.fire('movestart', {});
  fake.pendingExpansions.at(-1).resolve(12);
  await settle();
  fake.holdExpansion = false;
  assert.equal(map.calls.easeTo.length, eased2, 'ホイールのあとに返った古い応答では寄らない');
});

// ---- R10: 年をまたぐ「月から月」 ----
// 月のチップ列の端の「‹ 前の年」「次の年 ›」で、出す年だけを替える（選択も起点 anchor も変えない）
const crossYearCalendar = () => reply({ok: true, untimed: 0, years: [
  {year: '2025', count: 5, cover: null, months: [{month: '02', count: 3}, {month: '06', count: 2}]},
  {year: '2024', count: 7, cover: null, months: [{month: '12', count: 7}]}]});
const navOf = (env, dir) => env.$('#lib-months .lib-month-nav[data-dir=' + dir + ']');

await test('(w5) AC13 年をまたぐ月から月: 月を起点にして年を替え（選択と起点は変わらない）、Shift で別の年の月を押すと from=2024-12…〜to=2025-02… の範囲。逆向きも', async () => {
  const env = makeEnv();
  env.handlers.calendar = crossYearCalendar;
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const shiftClick = node => node.dispatchEvent({type: 'click', shiftKey: true});
  const lastQ = () => env.last('/api/admin/library/photos');
  const sel = () => plain(env.ev('ls.sel.calendar'));
  const chipYear = () => env.$('#lib-months').dataset.year;
  // 2024 年を選ぶ → 月のチップは 2024 年。次の年だけの切り替え（前の年は無い）
  mainOf(yearCards(env)[1]).click();
  await settle();
  assert.equal(chipYear(), '2024');
  assert.equal(navOf(env, 'prev'), null, '2024 より前の年は無い');
  assert.equal(navOf(env, 'next').textContent, '2025年 ›');
  assert.equal(navOf(env, 'next').getAttribute('aria-label'), '2025年の月を出す');
  assert.equal(monthChips(env).length, 12, '月のチップは 12 個のまま（切り替えのボタンは数えない）');
  // 12 月を起点にする
  monthChips(env)[11].click();
  await settle();
  assert.deepEqual(sel(), {kind: 'month', year: '2024', month: '12', from: '2024-12-00 00:00', to: '2024-12-99 99:99'});
  const reqs = env.reqs('/api/admin/library/photos').length;
  const anchor = plain(env.ev('ls.anchor'));
  // 年を替える: 写真は読み直さず、選択も起点も変わらない。月のチップだけ 2025 年になる
  navOf(env, 'next').click();
  await settle();
  assert.equal(chipYear(), '2025');
  assert.equal(env.reqs('/api/admin/library/photos').length, reqs, '年の切り替えでは写真を読まない');
  assert.deepEqual(sel(), {kind: 'month', year: '2024', month: '12', from: '2024-12-00 00:00', to: '2024-12-99 99:99'});
  assert.deepEqual(plain(env.ev('ls.anchor')), anchor);
  assert.equal(navOf(env, 'next'), null);
  assert.equal(navOf(env, 'prev').textContent, '‹ 2024年');
  assert.equal(monthChips(env)[1].dataset.key, '2025-02');
  assert.ok(monthChips(env).every(c => !c.classList.contains('is-selected')), '2025 年には選択中の月が無い');
  assert.ok(monthChips(env).every(c => c.getAttribute('aria-pressed') === 'false'), '2025 年のチップは押された表示ではない');
  same(env.document.activeElement, navOf(env, 'prev'), '押したボタンの代わり（残っている側の切り替え）へフォーカスが移る');
  // 選んだ月の年へ戻っても（Shift で範囲にする前）、選択の表示と aria-pressed が残る
  navOf(env, 'prev').click();
  await settle();
  assert.equal(chipYear(), '2024');
  assert.ok(monthChips(env)[11].classList.contains('is-selected'), '戻ったときも 12 月は選択表示');
  assert.equal(monthChips(env)[11].getAttribute('aria-pressed'), 'true', '戻ったときも 12 月は aria-pressed=true');
  assert.ok(monthChips(env).filter(c => c !== monthChips(env)[11]).every(c => !c.classList.contains('is-selected') && c.getAttribute('aria-pressed') === 'false'), '他の月は未選択');
  navOf(env, 'next').click();
  await settle();
  assert.equal(chipYear(), '2025');
  // 2025 年 2 月を Shift で押す → 2024 年 12 月から 2025 年 2 月
  shiftClick(monthChips(env)[1]);
  await settle();
  const want = {kind: 'span', from: '2024-12-00 00:00', to: '2025-02-99 99:99'};
  assert.deepEqual(sel(), want);
  assert.equal(lastQ().get('from'), '2024-12-00 00:00');
  assert.equal(lastQ().get('to'), '2025-02-99 99:99');
  assert.equal(lastQ().get('page'), '1');
  assert.equal(env.$('#lib-sel').textContent, '2024年12月〜2025年2月');
  assert.equal(chipYear(), '2025', '範囲を選んだあとも、切り替えた年の月を出したまま');
  assert.equal(env.ev('ls.anchor.year'), '2024', '起点は 2024 年 12 月のまま');
  // 続けて 2024 年に戻って Shift で別の月 → 同じ起点から
  navOf(env, 'prev').click();
  await settle();
  assert.equal(chipYear(), '2024');
  assert.deepEqual(sel(), want, '切り替えても範囲は変わらない');
  // 範囲の選択中は月のチップを塗らない（paintBox の規則どおり。月を選んでいたときの表示が年の切り替えで残らない）
  assert.ok(monthChips(env).every(c => !c.classList.contains('is-selected') && c.getAttribute('aria-pressed') === 'false'), '範囲の選択中は月のチップは未選択表示');
  // 普通に月を押すと範囲を捨て、その月が新しい起点。切り替えた年の記憶も消える
  monthChips(env)[11].click();
  await settle();
  assert.equal(sel().kind, 'month');
  assert.equal(env.ev('ls.monthYear'), null);
  // 逆向き: 2025 年 2 月を起点にして、2024 年 12 月を Shift
  mainOf(yearCards(env)[0]).click();
  await settle();
  assert.equal(sel().kind, 'year');
  assert.equal(chipYear(), '2025');
  monthChips(env)[1].click();
  await settle();
  assert.deepEqual(plain(env.ev('ls.anchor')), {from: '2025-02-00 00:00', to: '2025-02-99 99:99', year: '2025'});
  navOf(env, 'prev').click();
  await settle();
  assert.equal(chipYear(), '2024');
  assert.equal(sel().kind, 'month', '年を替えただけでは選択は変わらない');
  shiftClick(monthChips(env)[11]);
  await settle();
  assert.deepEqual(sel(), want, '逆向きでも同じ範囲');
  assert.equal(lastQ().get('from'), '2024-12-00 00:00');
  assert.equal(lastQ().get('to'), '2025-02-99 99:99');
  // 選択を外す（もう一度範囲を押す）と月は隠れ、年の記憶も消える
  {
    env.$$('#lib-year-cards .lib-card')[0].querySelector('.lib-card-main').click();      // 範囲 → 2025 年
    await settle();
    assert.equal(sel().kind, 'year');
    assert.equal(env.ev('ls.monthYear'), null);
    assert.equal(chipYear(), '2025');
  }
});

// ---- 国・地域のカード（段③ T6）。合成の応答: 日本（都道府県・地域不明・境界データだけの県）・フランス・ドイツ（同名の地域）・フィジー（日付変更線をまたぐ） ----
const cvr = n => ({id: n, thumb_url: '/thumb/' + n + '.jpg?v=' + String(n).padStart(12, 'e')});
const item = (count, o = {}) => ({count, cover: cvr(900 + count), bbox: [10, 10, 20, 20], fit_bbox: [10, 10, 20, 20], ...o});
const noItem = () => ({count: 0, cover: null, bbox: null, fit_bbox: null});
const BAD_NAME = '<img src=x onerror=alert(1)>';
const intlBody = (over = {}) => ({ok: true, abroad: 0, classification_available: true, classification_pending: 0,
  prefs: [
    {pref: '架空県', count: 46, cover: cvr(701), bbox: [130, 30, 132, 32],
     cities: [{city: '見本町', count: 8, bbox: [130.5, 30.5, 130.6, 30.6]}, {city: '試験市', count: 38, bbox: [131, 31, 131.5, 31.5]}]},
    {pref: '試験県', count: 3, cover: null, bbox: null, cities: []}],
  countries: [
    {id: JP, name: '日本', aliases: ['日本', 'Japan'], count: 60, cover: cvr(701), bbox: [130, 30, 132, 32], fit_bbox: [130, 30, 132, 32], regions: [
      {id: jpId('架空県'), name: '架空県', aliases: [], count: 50, cover: cvr(701), bbox: [130, 30, 132, 32], fit_bbox: [130, 30, 132, 32], pref: '架空県', boundary_only: 4},
      {id: jpId('境界県'), name: '境界県', aliases: [], count: 5, cover: cvr(702), bbox: [140, 40, 141, 41], fit_bbox: [140, 40, 141, 41], pref: '境界県', boundary_only: 5},
      {id: jpId('試験県'), name: '試験県', aliases: [], count: 3, cover: null, bbox: null, fit_bbox: null, pref: '試験県', boundary_only: 0}],
     region_unknown: item(2)},
    {id: 'ne:FRA', name: 'フランス', aliases: ['France'], count: 30, cover: cvr(710), bbox: [-4, 43, 7, 50], fit_bbox: [-4, 43, 7, 50], regions: [
      {id: 'ne:FRA-r1', name: '中央', aliases: [], count: 20, cover: cvr(711), bbox: [1, 46, 3, 48], fit_bbox: [1, 46, 3, 48]},
      {id: 'ne:FRA-r2', name: BAD_NAME, aliases: [], count: 10, cover: cvr(712), bbox: [4, 44, 5, 45], fit_bbox: [4, 44, 5, 45]}],
     region_unknown: noItem()},
    {id: 'ne:DEU', name: 'ドイツ', aliases: ['Germany'], count: 10, cover: cvr(720), bbox: [6, 47, 14, 54], fit_bbox: [6, 47, 14, 54], regions: [
      {id: 'ne:DEU-r1', name: '中央', aliases: [], count: 10, cover: cvr(721), bbox: [9, 50, 10, 51], fit_bbox: [9, 50, 10, 51]}],
     region_unknown: noItem()},
    {id: 'ne:FJI', name: 'フィジー', aliases: ['Fiji'], count: 4, cover: cvr(730), bbox: [-180, -20, 180, -15], fit_bbox: null, regions: [
      {id: 'ne:FJI-r1', name: '西部', aliases: [], count: 2, cover: cvr(731), bbox: [177, -18, 180, -16], fit_bbox: [177, -18, 180, -16]},
      {id: 'ne:FJI-r2', name: '日付変更線の地域', aliases: [], count: 1, cover: cvr(732), bbox: [-180, -18, 180, -16], fit_bbox: null},
      {id: 'ne:FJI-r3', name: '範囲外の地域', aliases: [], count: 1, cover: cvr(733), bbox: [200, -17, 210, -16], fit_bbox: [200, -17, 210, -16]}],
     region_unknown: item(1, {bbox: [-180, -19, 180, -15], fit_bbox: null})}],
  unknown: {unclassified: item(6, {fit_bbox: null}), no_location: {count: 4, cover: cvr(740), bbox: null, fit_bbox: null}},
  ...over});
const useIntl = (env, over) => { env.handlers.places = () => reply(intlBody(over)); };
const intlEnv = async over => {
  const env = makeEnv();
  useIntl(env, over);
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  return env;
};
const photoQ = env => env.last('/api/admin/library/photos');
const hintOf = env => env.$('#lib-map-hint').textContent;
const press = async (env, card) => { mainOf(card).click(); await settle(); };
const HINT = 'この地域は地図を移動して確認できます';

await test('(t6a) AC1 国のカード: API の並び（件数降順）・表紙・枚数。初回は日本の地域（地域不明も）だけ展開し、写真・地図・選択は動かさない。地域未判定・場所不明は国の後ろ。名前は textContent だけ', async () => {
  const env = await intlEnv();
  assert.deepEqual(ccards(env).map(cardName), ['日本', 'フランス', 'ドイツ', 'フィジー']);
  assert.deepEqual(ccards(env).map(cardCount), ['60 枚', '30 枚', '10 枚', '4 枚']);
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県', '境界県', '試験県', '地域不明', 'フランス', 'ドイツ', 'フィジー', '地域未判定', '場所不明']);
  assert.deepEqual(allCards(env).map(c => c.dataset.key), [JP, rkey('架空県'), rkey('境界県'), rkey('試験県'), JP + '/!unknown', 'ne:FRA', 'ne:DEU', 'ne:FJI', 'unclassified', 'no_location'],
    'キーは国 ID・地域 ID（名前ではない）');
  assert.equal(mainOf(ccards(env)[1]).querySelector('.lib-cover img').src, '/thumb/710.jpg?v=eeeeeeeee710');
  assert.equal(mainOf(cardByKey(env, 'no_location')).querySelector('.lib-cover img').src, '/thumb/740.jpg?v=eeeeeeeee740');
  assert.deepEqual(ccards(env).map(c => mainOf(c).getAttribute('aria-expanded')), ['true', 'false', 'false', 'false']);
  assert.equal(env.ev('ls.openCountry'), JP);
  assert.equal(env.ev('ls.sel.places'), null, '展開だけで選ばない');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '写真を読まない');
  assert.equal(mapOf(env).calls.fitBounds.length, 0, '地図を動かさない');
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.$('#lib-note').textContent, '4 か国');
  assert.ok(allCards(env).every(c => mainOf(c).tagName === 'BUTTON' && mainOf(c).getAttribute('aria-pressed') === 'false'), '主ボタンは button（Enter・Space は click になる）');
  // 名前は textContent で入る（HTML として解釈しない）
  await press(env, ccards(env)[1]);
  const evil = cardByKey(env, 'ne:FRA/ne:FRA-r2');
  assert.equal(cardName(evil), BAD_NAME);
  assert.equal(evil.querySelectorAll('img').length, 1, '表紙の img だけ（名前から img は生えない）');
  assert.equal(evil.querySelectorAll('img')[0].src.startsWith('/thumb/'), true);
});

await test('(t6b) AC1・AC11 他国を押す: その国の写真（country）と地域の一覧へ切り替え（日本の展開は閉じる）。フォーカスは押したボタンに残る。地域で country&region。同じ項目をもう一度押すと選択解除（展開は残る）。同名の地域は ID で区別する', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  await press(env, ccards(env)[1]);                      // フランス
  let q = photoQ(env);
  assert.equal(q.get('country'), 'ne:FRA');
  ['region', 'region_unknown', 'pref', 'city', 'abroad', 'unclassified', 'no_location', 'bbox'].forEach(k => assert.equal(q.get(k), null, k));
  assert.equal(env.$('#lib-sel').textContent, 'フランス');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(tiles(env).length, 3);
  assert.deepEqual(allCards(env).map(cardName), ['日本', 'フランス', '中央', BAD_NAME, 'ドイツ', 'フィジー', '地域未判定', '場所不明'], '日本は閉じ、フランスの地域が続く（地域不明は 0 件なので出ない）');
  assert.equal(env.ev('ls.openCountry'), 'ne:FRA');
  const fra = cardByKey(env, 'ne:FRA');
  assert.ok(fra.classList.contains('is-selected'));
  assert.equal(mainOf(fra).getAttribute('aria-pressed'), 'true');
  assert.equal(mainOf(fra).getAttribute('aria-expanded'), 'true');
  same(env.document.activeElement, mainOf(fra), '一覧を作り直しても押したボタンにフォーカスが残る');
  assert.deepEqual(plain(map.calls.fitBounds), [{bounds: [-4, 43, 7, 50], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}}], 'fit_bbox に合わせる');
  // 地域
  await press(env, cardByKey(env, 'ne:FRA/ne:FRA-r1'));
  q = photoQ(env);
  assert.equal(q.get('country'), 'ne:FRA');
  assert.equal(q.get('region'), 'ne:FRA-r1');
  assert.equal(env.$('#lib-sel').textContent, 'フランス 中央');
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'region', country: 'ne:FRA', region: 'ne:FRA-r1', pref: '', label: 'フランス 中央'});
  assert.ok(cardByKey(env, 'ne:FRA/ne:FRA-r1').classList.contains('is-selected'));
  assert.ok(!fra.classList.contains('is-selected'), '国のカードの選択は外れる');
  assert.equal(cardByKey(env, 'ne:FRA/ne:FRA-r1').querySelector('.lib-cities'), null, '日本以外の地域に市区町村のチップは無い');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [1, 46, 3, 48]);
  // 国のカードをまた押すと（選択は地域なので）国全体を選ぶ。地域をもう一度押すと外れる
  await press(env, cardByKey(env, 'ne:FRA'));
  assert.equal(photoQ(env).get('region'), null);
  assert.equal(env.ev('ls.sel.places.kind'), 'country');
  await press(env, cardByKey(env, 'ne:FRA'));
  assert.equal(env.ev('ls.sel.places'), null, '同じ国をもう一度押すと選択解除');
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.ev('ls.openCountry'), 'ne:FRA', '解除しても地域の展開は残る');
  assert.equal(pcards(env).length, 2);
  // ドイツ: 同名の「中央」でも国・地域 ID が別なので、別の選択になる（外れずに切り替わる）
  await press(env, cardByKey(env, 'ne:DEU'));
  assert.equal(photoQ(env).get('country'), 'ne:DEU');
  await press(env, cardByKey(env, 'ne:DEU/ne:DEU-r1'));
  assert.equal(photoQ(env).get('region'), 'ne:DEU-r1');
  assert.equal(photoQ(env).get('country'), 'ne:DEU');
  await press(env, cardByKey(env, 'ne:DEU/ne:DEU-r1'));
  assert.equal(env.ev('ls.sel.places'), null);
  await press(env, ccards(env)[0]);                      // 日本へ戻す: 日本の地域に切り替わる
  assert.equal(photoQ(env).get('country'), JP);
  assert.deepEqual(allCards(env).slice(0, 5).map(cardName), ['日本', '架空県', '境界県', '試験県', '地域不明']);
});

await test('(t6c) AC2 日本の地域: 市区町村のチップ（旧 prefs の同名の行・件数順）と「市区町村未取得 N」（絞り込みは無い）。市区町村は country&region&city。チップが無くても未取得だけ出す', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  await press(env, pcards(env)[0]);                      // 架空県
  let q = photoQ(env);
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('city'), null);
  assert.equal(q.get('pref'), null);
  const card = cardByKey(env, rkey('架空県'));
  assert.ok(card.classList.contains('has-cities'));
  assert.deepEqual(cityChips(card).map(x => x.textContent), ['試験市38', '見本町8']);
  const missing = card.querySelector('.lib-cities .lib-missing');
  assert.equal(missing.textContent, '市区町村未取得 4');
  assert.notEqual(missing.tagName, 'BUTTON', '絞り込みは無い（押せない）');
  const before = env.reqs('/api/admin/library/photos').length;
  missing.click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  const chips = cityChips(card);
  chips[1].click();                                      // 見本町
  await settle();
  q = photoQ(env);
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('city'), '見本町');
  assert.equal(env.$('#lib-sel').textContent, '日本 架空県 見本町');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [130.5, 30.5, 130.6, 30.6]);
  same(cityChips(card)[1], chips[1], 'チップを作り直さない');
  assert.equal(chips[1].getAttribute('aria-pressed'), 'true');
  assert.ok(card.classList.contains('is-selected'));
  chips[1].click();                                      // 同じチップ → 解除
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(card.querySelector('.lib-cities'), null, '選択が無くなると地域のチップは閉じる（従来どおり）');
  // 旧 prefs に行が無い県（境界データだけ）は未取得の数だけ。取得済みで未取得 0 の県は何も付かない
  await press(env, pcards(env)[1]);                      // 境界県
  const edge = cardByKey(env, rkey('境界県'));
  assert.deepEqual(cityChips(edge).map(x => x.textContent), []);
  assert.equal(edge.querySelector('.lib-cities').textContent, '市区町村未取得 5');
  assert.equal(card.querySelector('.lib-cities'), null, '前の県のチップは消える');
  await press(env, pcards(env)[2]);                      // 試験県（市区町村 0・未取得 0）
  assert.equal(cardByKey(env, rkey('試験県')).querySelector('.lib-cities'), null);
  assert.ok(!cardByKey(env, rkey('試験県')).classList.contains('has-cities'));
  // 日本の地域不明
  await press(env, cardByKey(env, JP + '/!unknown'));
  q = photoQ(env);
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region_unknown'), '1');
  assert.equal(q.get('region'), null);
  assert.equal(env.$('#lib-sel').textContent, '日本 地域不明');
});

await test('(t6d) AC1 地域不明（各国）・地域未判定・場所不明: 選ぶと region_unknown／unclassified／no_location。0 件なら出さない。場所不明は地図を動かさず案内も出さない', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  await press(env, ccards(env)[3]);                      // フィジー（日付変更線）
  await press(env, cardByKey(env, 'ne:FJI/!unknown'));
  let q = photoQ(env);
  assert.equal(q.get('country'), 'ne:FJI');
  assert.equal(q.get('region_unknown'), '1');
  assert.equal(env.$('#lib-sel').textContent, 'フィジー 地域不明');
  await press(env, cardByKey(env, 'unclassified'));
  q = photoQ(env);
  assert.equal(q.get('unclassified'), '1');
  ['country', 'region', 'region_unknown', 'no_location', 'pref', 'abroad'].forEach(k => assert.equal(q.get(k), null, k));
  assert.equal(env.$('#lib-sel').textContent, '地域未判定');
  assert.equal(env.ev('ls.openCountry'), 'ne:FJI', '国の外の項目では展開を替えない');
  const fits = map.calls.fitBounds.length;
  await press(env, cardByKey(env, 'no_location'));
  q = photoQ(env);
  assert.equal(q.get('no_location'), '1');
  assert.equal(q.get('unclassified'), null);
  assert.equal(env.$('#lib-sel').textContent, '場所不明');
  assert.equal(map.calls.fitBounds.length, fits, '位置が無いので地図は動かさない');
  assert.equal(hintOf(env), '', '案内も出さない');
  await press(env, cardByKey(env, 'no_location'));
  assert.equal(env.ev('ls.sel.places'), null, '同じ項目をもう一度押すと解除');
  // 0 件なら出さない（日本の地域不明・地域未判定・場所不明。展開中のフィジーの地域不明は 1 件なので残る）
  const bodyNone = intlBody({unknown: {unclassified: noItem(), no_location: noItem()}});
  bodyNone.countries[0].region_unknown = noItem();
  const calm = makeEnv();
  calm.handlers.places = () => reply(bodyNone);
  await calm.boot();
  await openLib(calm);
  await openPlaces(calm);
  assert.deepEqual(allCards(calm).map(cardName), ['日本', '架空県', '境界県', '試験県', 'フランス', 'ドイツ', 'フィジー']);
  await press(calm, ccards(calm)[3]);
  assert.deepEqual(allCards(calm).map(cardName).slice(3), ['フィジー', '西部', '日付変更線の地域', '範囲外の地域', '地域不明']);
});

await test('(t6e) 処理待ちと旧 DB: classification_pending があれば「処理待ち N 枚」、0 なら出さない。classification_available:false（旧 DB・日本だけ）でも国・地域・市区町村を選べ、分類がまだ始まっていないと短く示す', async () => {
  const env = await intlEnv({classification_pending: 3});
  assert.equal(env.$('#lib-place-status').textContent, '処理待ち 3 枚');
  const calm = makeEnv();
  useIntl(calm);
  await calm.boot();
  await openLib(calm);
  await openPlaces(calm);
  assert.equal(calm.$('#lib-place-status').textContent, '', '0 件なら出さない');
  // 旧 DB: 日本だけ。地域未判定は旧の未分類
  const body = intlBody({classification_available: false, classification_pending: 0});
  body.countries = body.countries.slice(0, 1);
  body.unknown = {unclassified: item(9, {fit_bbox: null}), no_location: noItem()};
  const old = makeEnv();
  old.handlers.places = () => reply(body);
  await old.boot();
  await openLib(old);
  await openPlaces(old);
  assert.equal(old.$('#lib-place-status').textContent, '国・地域の分類はまだ始まっていません');
  assert.deepEqual(allCards(old).map(cardName), ['日本', '架空県', '境界県', '試験県', '地域不明', '地域未判定']);
  await press(old, pcards(old)[0]);
  assert.equal(photoQ(old).get('region'), jpId('架空県'));
  cityChips(cardByKey(old, rkey('架空県')))[0].click();
  await settle();
  assert.equal(photoQ(old).get('city'), '試験市');
  await press(old, cardByKey(old, 'unclassified'));
  assert.equal(photoQ(old).get('unclassified'), '1');
  assert.equal(hintOf(old), HINT, '範囲があっても fit_bbox が無ければ案内を出す');
  // countries の無い古い応答でも壊れない（空の一覧）
  const bare = makeEnv();
  bare.handlers.places = () => reply({ok: true, countries: 'x', prefs: [], abroad: 0});
  await bare.boot();
  await openLib(bare);
  await openPlaces(bare);
  assert.equal(bare.$('#lib-place-cards').textContent, '写真がありません');
});

await test('(t6f) AC11 古い応答の破棄: 国を押した直後に別の国・地域・解除をしても、遅れて届いた前の写真は出さない（gen/seq）', async () => {
  const env = await intlEnv();
  const held = {};
  const sent = {};            // 保留した要求のすべて（キーごとに送った順）。resolver をリクエストごとに残す（新しい応答のあとで古い応答を解くため）
  env.handlers.photos = params => {
    const key = params.get('country') + '/' + (params.get('region') || '');
    if(key === 'ne:FRA/' || key === 'ne:FRA/ne:FRA-r1'){
      return new Promise(res => {
        const send = items => res(jsonRes({ok: true, page: 1, per: 60, pages: 1, total: items.length, items}));
        held[key] = () => send([P503]);
        (sent[key] = sent[key] || []).push({hidden: params.get('hidden'), send});
      });
    }
    return null;
  };
  await press(env, cardByKey(env, 'ne:FRA'));            // フランス（保留）
  assert.equal(tiles(env).length, 0);
  await press(env, cardByKey(env, 'ne:DEU'));            // ドイツ（すぐ届く: GRID の 3 枚）
  assert.equal(tiles(env).length, 3);
  held['ne:FRA/']();
  await settle();
  assert.equal(tiles(env).length, 3, '遅れたフランスの 1 枚は捨てる');
  assert.equal(env.$('#lib-sel').textContent, 'ドイツ');
  // 保留中に解除
  await press(env, cardByKey(env, 'ne:FRA'));            // フランス（保留）
  assert.equal(held['ne:FRA/'] !== undefined, true);
  await press(env, cardByKey(env, 'ne:FRA/ne:FRA-r1'));  // フランス 中央（保留）
  await press(env, cardByKey(env, 'ne:FRA/ne:FRA-r1'));  // 外す
  held['ne:FRA/ne:FRA-r1']();
  held['ne:FRA/']();
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(tiles(env).length, 0, '解除したあとに届いた応答は出さない');
  assert.equal(hidden(env, '#lib-detail'), true);
  // 除外の切り替えでも、飛んでいる国の応答は捨てる
  await press(env, cardByKey(env, 'ne:FRA'));
  const before = sent['ne:FRA/'].length;
  const stale = sent['ne:FRA/'][before - 1];             // 除外を含める前に送った要求（まだ保留）
  assert.equal(stale.hidden, null);
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const reqs = env.reqs('/api/admin/library/photos').length;
  assert.equal(sent['ne:FRA/'].length, before + 1, '除外を含めて読み直す');
  const fresh = sent['ne:FRA/'][before];
  assert.equal(fresh.hidden, '1');
  fresh.send([P501, P502]);                              // 新しい応答が先に届いて表示される
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502']);
  stale.send([P503]);                                    // そのあとに古い応答が届く
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501', '502'], '古い応答では内容が戻らない');
  assert.equal(env.reqs('/api/admin/library/photos').length, reqs);
  assert.equal(photoQ(env).get('hidden'), '1');
  assert.equal(photoQ(env).get('country'), 'ne:FRA', '選択は残り、除外を含めて読み直す');
});

// 実 API どおりの分類: 対応表にある架空県だけが日本の地域になり、表にない旧地名（表記揺れ）は日本の region_unknown に入る（prefs の行には残る）
const legacyBody = () => ({ok: true, abroad: 0, prefs: [
  {pref: '架空県', count: 50, cover: null, bbox: [130, 30, 132, 32], cities: [{city: '試験市', count: 40, bbox: [131, 31, 131.5, 31.5]}]},
  {pref: '旧表記県', count: 4, cover: null, bbox: [135, 34, 136, 35], cities: [
    {city: '旧市', count: 3, bbox: [135.2, 34.2, 135.4, 34.4]}, {city: '', count: 1, bbox: null}]},
  {pref: '歪み県', count: 2, cover: null, bbox: [10, 20, 5, 30], cities: [{city: '歪み町', count: 2, bbox: null}]}]});
const legacyEnv = async () => {
  const env = makeEnv();
  env.handlers.places = () => reply(legacyBody());
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  return env;
};
const chooseOpt = async (env, text) => {
  const input = comboBox(env);
  typeInto(input, text);
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
};
const noNewForm = q => ['country', 'region', 'region_unknown', 'unclassified', 'no_location'].forEach(k => assert.equal(q.get(k), null, k + ' は混ぜない'));

await test('(t6j) 対応表にない旧地名（日本の region_unknown に入る pref）: カードには出ないがコンボボックスの候補に残り、選ぶと旧形式 pref=（市区町村は pref&city）で写真を取り、見出し・地図の fit を出す。新形式とは混ぜない', async () => {
  const env = await legacyEnv();
  // API どおり: 地域は対応表の県だけ。旧地名は「地域不明」に入り、地域のカードには出ない
  assert.deepEqual(allCards(env).map(cardName), ['日本', '架空県', '地域不明']);
  assert.deepEqual(plain(env.ev('ls.countries[0].regions.map(r => r.pref)')), ['架空県']);
  assert.equal(cardCount(cardByKey(env, JP + '/!unknown')), '6 枚');
  assert.deepEqual(plain(env.ev('ls.prefs.map(p => p.pref)')), ['架空県', '旧表記県', '歪み県'], '旧形式の行は旧地名も持つ');
  // 候補: 旧地名も出る。旧地名には legacy の印、表にある県には付かない
  const input = comboBox(env);
  typeInto(input, '旧');
  assert.deepEqual(labelsOf(env), ['旧表記県（4）', '旧表記県 旧市（3）']);
  assert.deepEqual(plain(env.ev('ls.combo.items.map(i => i.legacy)')), [true, true]);
  typeInto(input, '架空');
  assert.deepEqual(plain(env.ev('ls.combo.items.map(i => [i.label, i.legacy])')), [['架空県（50）', false]]);
  typeInto(input, '試験市');
  assert.deepEqual(plain(env.ev('ls.combo.items.map(i => [i.label, i.legacy])')), [['架空県 試験市（40）', false]]);
  // 旧地名の県を選ぶ: pref= だけ。選択は legacy_pref。見出し・地図の fit
  const photos0 = env.reqs('/api/admin/library/photos').length;
  const map = mapOf(env);
  await chooseOpt(env, '旧表記');
  let q = photoQ(env);
  assert.equal(env.reqs('/api/admin/library/photos').length, photos0 + 1);
  assert.equal(q.get('pref'), '旧表記県');
  assert.equal(q.get('city'), null);
  noNewForm(q);
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'legacy_pref', pref: '旧表記県', city: '', label: '旧表記県'});
  assert.equal(env.$('#lib-sel').textContent, '旧表記県');
  assert.equal(tiles(env).length, 3, '写真が出る（無反応にならない）');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1)), {bounds: [135, 34, 136, 35], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}});
  assert.equal(comboBox(env).value, '旧表記県');
  assert.equal(hintOf(env), '');
  // カードに選択の印は付かず、新形式の地域のカードも選ばれない
  assert.ok(allCards(env).every(c => !c.classList.contains('is-selected')));
  // 市区町村: pref と city の両方
  await chooseOpt(env, '旧市');
  q = photoQ(env);
  assert.equal(q.get('pref'), '旧表記県');
  assert.equal(q.get('city'), '旧市');
  noNewForm(q);
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'legacy_pref', pref: '旧表記県', city: '旧市', label: '旧表記県 旧市'});
  assert.equal(env.$('#lib-sel').textContent, '旧表記県 旧市');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [135.2, 34.2, 135.4, 34.4]);
  // 同じ候補を選び直しても外れず、読み直さない
  const n = env.reqs('/api/admin/library/photos').length;
  await chooseOpt(env, '旧市');
  assert.equal(env.reqs('/api/admin/library/photos').length, n);
  assert.equal(env.ev('ls.sel.places.city'), '旧市');
  // 除外を含める・期間のあいだも旧形式のまま読み直す
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  q = photoQ(env);
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('pref'), '旧表記県');
  assert.equal(q.get('city'), '旧市');
  noNewForm(q);
  // 表にある県は従来どおり新形式（pref は送らない）
  await chooseOpt(env, '架空');
  q = photoQ(env);
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('pref'), null, '新形式に旧形式を混ぜない');
  assert.equal(env.ev('ls.sel.places.kind'), 'region');
  // カードを押すと旧地名の選択は替わる（新形式のみ）
  await chooseOpt(env, '旧表記');
  assert.equal(env.ev('ls.sel.places.kind'), 'legacy_pref');
  await press(env, cardByKey(env, JP + '/!unknown'));
  q = photoQ(env);
  assert.equal(q.get('region_unknown'), '1');
  assert.equal(q.get('pref'), null);
  assert.equal(q.get('city'), null);
  assert.equal(env.ev('ls.sel.places.kind'), 'region_unknown');
});

await test('(t6k) 旧地名の選択も gen/seq で守る: 遅れて届いた旧地名の写真は、別のカードを選んだあとに出さない。bbox が不正なら動かさず案内、次の選択で案内は消える。地図を動かして絞っていても選べる', async () => {
  const env = await legacyEnv();
  const map = mapOf(env);
  const sentL = [];
  env.handlers.photos = params => {
    if(params.get('pref') !== '旧表記県') return null;
    return new Promise(res => { sentL.push(items => res(jsonRes({ok: true, page: 1, per: 60, pages: 1, total: items.length, items}))); });
  };
  await chooseOpt(env, '旧表記');                          // 旧地名（保留）
  assert.equal(tiles(env).length, 0);
  await press(env, cardByKey(env, JP + '/' + jpId('架空県')));   // 別のカード（すぐ届く）
  assert.equal(tiles(env).length, 3);
  sentL[0]([P503]);
  await settle();
  assert.equal(tiles(env).length, 3, '遅れた旧地名の 1 枚は捨てる');
  assert.equal(env.$('#lib-sel').textContent, '日本 架空県');
  // 逆: 旧地名を選んだあとに除外を切り替えると、古い要求は捨てる
  await chooseOpt(env, '旧表記');
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  sentL.at(-1)([P501]);                                   // 新しい応答（hidden=1）
  await settle();
  sentL.at(-2)([P503, P502]);                             // 古い応答
  await settle();
  assert.deepEqual(tiles(env).map(x => x.dataset.id), ['501'], '古い応答では内容が戻らない');
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  env.handlers.photos = null;
  // bbox が不正（最小 > 最大）: 地図は動かさず案内。選択はできる
  const fits = map.calls.fitBounds.length;
  await chooseOpt(env, '歪み県');
  assert.equal(photoQ(env).get('pref'), '歪み県');
  assert.equal(map.calls.fitBounds.length, fits);
  assert.equal(hintOf(env), HINT);
  // bbox が無い旧地名の市区町村: 動かさず、案内も出さない
  await chooseOpt(env, '歪み町');
  assert.equal(photoQ(env).get('city'), '歪み町');
  assert.equal(map.calls.fitBounds.length, fits);
  assert.equal(hintOf(env), '', '前の選択の案内は残さない');
  assert.equal(env.ev('ls.mapHint'), null);
  // 案内を出した選択を外して選び直しても、前の案内は復活しない（旧地名 → 案内 → 別のカード → 解除）
  await chooseOpt(env, '歪み県');
  assert.equal(hintOf(env), HINT);
  await press(env, cardByKey(env, JP + '/' + jpId('架空県')));
  assert.equal(hintOf(env), '');
  assert.equal(env.ev('ls.mapHint'), null, '選択が替わると案内は捨てる');
  // 地図を動かしてカードを絞っていても、旧地名は選べる（カードの読み直しは要らない）
  userMove(env, {bounds: {w: 130, s: 30, e: 132, n: 32}, center: {lng: 131, lat: 31}});
  await settle();
  assert.deepEqual(plain(env.ev('ls.mapBbox')), [130, 30, 132, 32]);
  const places = env.reqs('/api/admin/library/places').length;
  await chooseOpt(env, '旧表記');
  assert.equal(photoQ(env).get('pref'), '旧表記県');
  assert.equal(env.reqs('/api/admin/library/places').length, places, 'カードを読み直さない');
  assert.deepEqual(plain(env.ev('ls.mapBbox')), [130, 30, 132, 32], '絞り込みはそのまま');
});

await test('(t6g) AC3・AC11 地図: fit_bbox に合わせる（libMove 付き・bbox の絞り込みを足さない）。fit_bbox が null（日付変更線をまたぐ）か不正なら動かさず、世界全体へも飛ばさず、案内を出す。次の選択・利用者の移動で案内は消える', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  const camera = () => JSON.stringify([env.fake.center, env.fake.zoom, env.fake.bounds]);
  await press(env, cardByKey(env, 'ne:FRA'));
  assert.deepEqual(plain(map.calls.fitBounds.map(f => f.bounds)), [[-4, 43, 7, 50]]);
  assert.equal(hintOf(env), '');
  map.endMove();                                         // 着いた（libMove 付きの moveend）
  await settle();
  assert.equal(env.ev('ls.mapBbox'), null, 'カードによる移動で絞り込みを足さない');
  placeQs(env).forEach(q => assert.equal(q.get('bbox'), null));
  // フィジー: bbox は経度の全域だが fit_bbox は null → 動かさない
  const cam = camera();
  const fits = map.calls.fitBounds.length;
  await press(env, cardByKey(env, 'ne:FJI'));
  assert.equal(photoQ(env).get('country'), 'ne:FJI', '写真は国 ID で絞る');
  assert.equal(map.calls.fitBounds.length, fits, '地図を動かさない');
  assert.equal(map.calls.easeTo.length + map.calls.flyTo.length, 0);
  assert.equal(camera(), cam, '現在位置のまま（世界全体へ飛ばない）');
  assert.equal(hintOf(env), HINT);
  assert.equal(env.$('#lib-map-hint').getAttribute('role'), 'status');
  env.$('#lib-hidden').checked = true;                   // 除外の切り替えでは選択も位置も変わらないので、案内は残る
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(hintOf(env), HINT);
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  // 日付変更線の手前の地域は合わせる。案内は選択が替わると消える
  await press(env, cardByKey(env, 'ne:FJI/ne:FJI-r1'));
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [177, -18, 180, -16]);
  assert.equal(hintOf(env), '');
  // 日付変更線をまたぐ地域・範囲外の fit_bbox・地域不明（fit_bbox null）
  for(const key of ['ne:FJI/ne:FJI-r2', 'ne:FJI/ne:FJI-r3', 'ne:FJI/!unknown']){
    const n = map.calls.fitBounds.length;
    await press(env, cardByKey(env, key));
    assert.equal(map.calls.fitBounds.length, n, key + ': 動かさない');
    assert.equal(hintOf(env), HINT, key + ': 案内');
    assert.equal(photoQ(env).get('country'), 'ne:FJI', key);
  }
  assert.equal(photoQ(env).get('region_unknown'), '1');
  assert.equal(camera(), cam);
  assert.equal(env.ev('ls.mapBbox'), null);
  // 地域未判定: bbox はあるが fit_bbox が null → 案内
  await press(env, cardByKey(env, 'unclassified'));
  assert.equal(hintOf(env), HINT);
  // 解除すると案内も消える。利用者が地図を動かしても消える
  await press(env, cardByKey(env, 'unclassified'));
  assert.equal(hintOf(env), '');
  assert.equal(env.ev('ls.mapHint'), null, '解除で案内そのものを捨てる（同じ選択を選び直しても前の案内が復活しない）');
  await press(env, cardByKey(env, 'ne:FJI'));
  assert.equal(hintOf(env), HINT);
  userMove(env, {bounds: {w: 170, s: -20, e: 179, n: -15}, center: {lng: 175, lat: -17}});
  await settle();
  assert.equal(hintOf(env), '', '利用者が動かしたら案内は役目を終える');
  assert.deepEqual(plain(env.ev('ls.mapBbox')), [170, -20, 179, -15], '絞り込みは利用者の移動のときだけ');
  // 日本の fit_bbox が無い地域（試験県: 位置のある写真なし）は案内も出さない
  env.handlers.places = () => reply(intlBody());
  await press(env, cardByKey(env, 'ne:FRA'));
  await press(env, ccards(env)[0]);
  const n = map.calls.fitBounds.length;
  await press(env, cardByKey(env, rkey('試験県')));
  assert.equal(map.calls.fitBounds.length, n);
  assert.equal(hintOf(env), '');
});

await test('(t6h) AC3 地図の点は場所の選択で消えず読み直さない（points に場所の条件を付けない）。国・地域の選択は places のカード読み込みも増やさない', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  const pointReqs = () => env.reqs('/api/admin/library/points');
  const n = pointReqs().length;
  assert.equal(n, 1);
  const sets = () => (map.sources.photos ? map.sources.photos.setDataCalls.length : 0);
  const s0 = sets();
  const places0 = env.reqs('/api/admin/library/places').length;
  for(const key of ['ne:FRA', 'ne:FRA/ne:FRA-r1', 'unclassified', 'no_location', 'ne:FJI', 'ne:FJI/ne:FJI-r2']){
    await press(env, cardByKey(env, key));
  }
  assert.equal(pointReqs().length, n, '点は読み直さない');
  assert.equal(sets(), s0, '点は差し替えない（消えない）');
  assert.equal(env.reqs('/api/admin/library/places').length, places0, '選択ではカードも読み直さない');
  pointReqs().forEach(c => {
    const q = qOf(c);
    ['country', 'region', 'region_unknown', 'unclassified', 'no_location', 'pref', 'city', 'bbox'].forEach(k => assert.equal(q.get(k), null, k));
  });
  assert.equal(env.ev('ls.pointsStale'), false);
  map.endMove();
  await settle();
  assert.equal(env.ev('ls.mapBbox'), null);
  assert.equal(env.reqs('/api/admin/library/places').length, places0, 'カードの移動の moveend ではカードを絞らない');
});

await test('(t6i) 選択は国 ID・地域 ID が同一性: 同じ名前の地域・市区町村が別の国・県にあっても混ざらない。期間・除外のあいだも country/region を送る。カードの読み直しで展開を失わない', async () => {
  const env = await intlEnv();
  await press(env, cardByKey(env, 'ne:FRA'));
  await press(env, cardByKey(env, 'ne:FRA/ne:FRA-r1'));
  const sel = plain(env.ev('ls.sel.places'));
  assert.equal(sel.region, 'ne:FRA-r1');
  // 地図を動かすとカードが読み直される: 展開も選択も残り、押したカードの見た目が戻る
  userMove(env, {bounds: {w: 0, s: 45, e: 4, n: 49}, center: {lng: 2, lat: 47}});
  await settle();
  assert.equal(env.ev('ls.openCountry'), 'ne:FRA');
  assert.ok(cardByKey(env, 'ne:FRA/ne:FRA-r1').classList.contains('is-selected'));
  assert.equal(mainOf(cardByKey(env, 'ne:FRA/ne:FRA-r1')).getAttribute('aria-pressed'), 'true');
  assert.deepEqual(plain(env.ev('ls.sel.places')), sel);
  // 除外を含める: 同じ選択で読み直す
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  const q = photoQ(env);
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('country'), 'ne:FRA');
  assert.equal(q.get('region'), 'ne:FRA-r1');
  // 期間: 旅の「地図で見る」でも、国の選択に from/to が付く
  env.$('#lib-view-trips').click();
  await settle();
  mapBtnOf(env, 0).click();
  await settle();
  await press(env, cardByKey(env, 'ne:DEU') || ccards(env)[2]);
  const p = photoQ(env);
  assert.equal(p.get('country'), 'ne:DEU');
  assert.equal(p.get('from'), A);
  assert.equal(p.get('to'), Z);
  // 期間を解除すると、選択はそのまま期間なしで読み直す
  env.$('#lib-period-clear').click();
  await settle();
  const r = photoQ(env);
  assert.equal(r.get('country'), 'ne:DEU');
  assert.equal(r.get('from'), null);
});

// ---- 国・地域の候補・選択の解除・写真詳細の国・地域・旅の地名・位置変更通知（段③ T7） ----

await test('(t7a) AC4 候補は 国 → 地域 → 日本の市区町村 の順（それぞれ件数降順）。同名の地域（フランス・ドイツの「中央」）には国名を付ける。名前は textContent だけ', async () => {
  const env = await intlEnv();
  const input = comboBox(env);
  keyOn(input, 'ArrowDown');
  assert.deepEqual(labelsOf(env), [
    '日本（60）', 'フランス（30）', 'ドイツ（10）', 'フィジー（4）',
    '架空県（50）', 'フランス 中央（20）', BAD_NAME + '（10）', 'ドイツ 中央（10）', '境界県（5）', '試験県（3）', '西部（2）', '日付変更線の地域（1）', '範囲外の地域（1）',
    '架空県 試験市（38）', '架空県 見本町（8）']);
  assert.deepEqual(plain(env.ev('ls.combo.items.map(i => i.kind)')),
    ['country', 'country', 'country', 'country', 'region', 'region', 'region', 'region', 'region', 'region', 'region', 'region', 'region', 'city', 'city']);
  assert.ok(optsOf(env).every(o => o.children.every(k => k.nodeType !== 1)), '名前から要素を作らない（文字のノードだけ）');
  // 同名でない地域は名前だけ。入力で絞っても、同名の地域は国名つきのまま
  typeInto(input, '中央');
  assert.deepEqual(labelsOf(env), ['フランス 中央（20）', 'ドイツ 中央（10）']);
  typeInto(input, '西部');
  assert.deepEqual(labelsOf(env), ['西部（2）']);
  // 地域不明は候補にしない（カードの「地域不明」から選ぶ）
  typeInto(input, '不明');
  assert.deepEqual(labelsOf(env), []);
});

await test('(t7b) AC4 日本語・英語・原表記の aliases で当たる（大小文字・全角・かなの違いは無視）。日本の読み仮名検索（ながの・nagano）と市区町村の検索も従来どおり', async () => {
  const body = intlBody();
  const de = body.countries.find(c => c.id === 'ne:DEU');
  de.aliases = ['Germany', 'Deutschland', 'Federal Republic of Germany'];
  de.regions[0].name = 'バイエルン';
  de.regions[0].aliases = ['Bayern', 'Bavaria', 'バイエルン'];
  const env = makeEnv();
  env.handlers.places = () => reply(body);
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  const input = comboBox(env);
  for(const t of ['ドイツ', 'germany', 'GERMANY', 'Deutschland', 'ｄｅｕｔｓｃｈ', 'federal republic']) {
    typeInto(input, t);
    assert.deepEqual(labelsOf(env), ['ドイツ（10）'], t);
  }
  for(const t of ['バイエルン', 'ばいえるん', 'bayern', 'BAVARIA']) {
    typeInto(input, t);
    assert.deepEqual(labelsOf(env), ['バイエルン（10）'], t);
  }
  typeInto(input, 'japan');
  assert.deepEqual(labelsOf(env), ['日本（60）']);
  typeInto(input, 'france');
  assert.deepEqual(labelsOf(env), ['フランス（30）']);
  typeInto(input, '試験市');
  assert.deepEqual(labelsOf(env), ['架空県 試験市（38）'], '市区町村の検索');
  // 読み仮名（日本の地域）
  const env2 = makeEnv();
  env2.handlers.places = () => reply(comboBody());
  await env2.boot();
  await openLib(env2);
  await openPlaces(env2);
  const input2 = comboBox(env2);
  for(const t of ['ながの', 'nagano', 'ナガノ', 'naganoken']){
    typeInto(input2, t);
    assert.deepEqual(labelsOf(env2), ['長野県（423）'], t);
  }
  typeInto(input2, 'ken');
  assert.deepEqual(labelsOf(env2), []);
  typeInto(input2, 'japan');
  assert.deepEqual(labelsOf(env2).length, 1);
  assert.ok(labelsOf(env2)[0].startsWith('日本（'));
});

await test('(t7c) 候補の選択: 国は country、地域は country&region（同名の地域も ID で区別）、日本の市区町村は country&region&city。地図は fit_bbox（libMove 付き）。入力欄は名前。同じ候補を選び直しても外れない', async () => {
  const env = await intlEnv();
  const map = mapOf(env);
  await chooseOpt(env, 'germany');
  let q = photoQ(env);
  assert.equal(q.get('country'), 'ne:DEU');
  assert.equal(q.get('region'), null);
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'country', country: 'ne:DEU', label: 'ドイツ'});
  assert.equal(env.$('#lib-sel').textContent, 'ドイツ');
  assert.equal(comboBox(env).value, 'ドイツ');
  assert.equal(env.ev('ls.openCountry'), 'ne:DEU');
  assert.deepEqual(plain(map.calls.fitBounds.at(-1)), {bounds: [6, 47, 14, 54], options: {padding: 16, maxZoom: 12}, eventData: {libMove: true}});
  // 同名の地域: 2 番目（ドイツ 中央）を ↓↓ Enter で
  const input = comboBox(env);
  typeInto(input, '中央');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'Enter');
  await settle();
  q = photoQ(env);
  assert.equal(q.get('country'), 'ne:DEU');
  assert.equal(q.get('region'), 'ne:DEU-r1');
  assert.deepEqual(plain(env.ev('ls.sel.places')), {kind: 'region', country: 'ne:DEU', region: 'ne:DEU-r1', pref: '', label: 'ドイツ 中央'});
  assert.equal(input.value, '中央');
  await chooseOpt(env, 'フランス 中');                    // 名前の「フランス 中央」ではなく、入力に合う候補は無い（国名つきは表示だけ）
  assert.equal(env.ev('ls.sel.places.region'), 'ne:DEU-r1', '合う候補が無ければ選択は変わらない');
  typeInto(input, '中央');
  keyOn(input, 'ArrowUp');                                  // 最後（ドイツ 中央）ではなく、↑ は末尾から → ドイツ。先頭へ戻るのは ↓↑ の確認で別に見る
  keyOn(input, 'ArrowUp');
  keyOn(input, 'Enter');
  await settle();
  q = photoQ(env);
  assert.equal(q.get('country'), 'ne:FRA');
  assert.equal(q.get('region'), 'ne:FRA-r1');
  // 日本の地域と市区町村
  await chooseOpt(env, '架空県');
  q = photoQ(env);
  assert.equal(q.get('country'), JP);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.deepEqual(plain(env.ev('ls.sel.places')), selRegion('架空県'));
  await chooseOpt(env, '見本町');
  q = photoQ(env);
  assert.equal(q.get('region'), jpId('架空県'));
  assert.equal(q.get('city'), '見本町');
  assert.deepEqual(plain(env.ev('ls.sel.places')), selCity('架空県', '見本町'));
  assert.equal(comboBox(env).value, '見本町');
  // 同じ候補を選び直しても外れず、読み直さない
  const n = env.reqs('/api/admin/library/photos').length;
  await chooseOpt(env, '見本町');
  assert.equal(env.reqs('/api/admin/library/photos').length, n);
  assert.equal(env.ev('ls.sel.places.city'), '見本町');
  // 境界データだけの県（旧 prefs に行が無い）も地域として選べる
  await chooseOpt(env, '境界県');
  assert.equal(photoQ(env).get('region'), jpId('境界県'));
  assert.deepEqual(plain(map.calls.fitBounds.at(-1).bounds), [140, 40, 141, 41]);
});

await test('(t7d) AC11 候補のキー操作: ↓↑ は端で回る、Enter は強調した候補だけを決める（強調なしの Enter は何もしない）、Esc は閉じる（閉じているときは他へ任せる）、↓ で開き直せる。変換中は無視', async () => {
  const env = await intlEnv();
  const input = comboBox(env);
  typeInto(input, '中央');
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  assert.equal(input.getAttribute('aria-activedescendant'), null);
  const before = env.reqs('/api/admin/library/photos').length;
  const enter = {type: 'keydown', key: 'Enter'};
  input.dispatchEvent(enter);
  await settle();
  assert.notEqual(enter.defaultPrevented, true, '強調していない Enter は他の処理に任せる');
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  keyOn(input, 'ArrowUp');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-1', '↑ は末尾から');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0', '末尾の次は先頭');
  keyOn(input, 'ArrowDown');
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  assert.deepEqual(optsOf(env).map(o => o.getAttribute('aria-selected')), ['true', 'false']);
  keyOn(input, 'ArrowDown', {isComposing: true});
  keyOn(input, 'Enter', {keyCode: 229});
  await settle();
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0', '変換中のキーは無視');
  assert.equal(env.reqs('/api/admin/library/photos').length, before);
  const esc = {type: 'keydown', key: 'Escape'};
  input.dispatchEvent(esc);
  assert.equal(esc.defaultPrevented, true);
  assert.equal(input.getAttribute('aria-expanded'), 'false');
  assert.equal(input.getAttribute('aria-activedescendant'), null);
  assert.equal(input.value, '中央');
  const esc2 = {type: 'keydown', key: 'Escape'};
  input.dispatchEvent(esc2);
  assert.notEqual(esc2.defaultPrevented, true);
  keyOn(input, 'ArrowDown');
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  assert.equal(input.getAttribute('aria-activedescendant'), 'lib-opt-0');
  keyOn(input, 'Enter');
  await settle();
  assert.equal(photoQ(env).get('region'), 'ne:FRA-r1', '先頭（フランス 中央）');
  assert.equal(input.getAttribute('aria-expanded'), 'false');
});

await test('(t7e) 旧地名（対応表にない pref）は国・地域の候補の並びでも「地域」の後ろに出て、legacy の印が付き、旧形式で選ぶ。国・表にある県には付かない', async () => {
  const env = await legacyEnv();
  const input = comboBox(env);
  keyOn(input, 'ArrowDown');
  assert.deepEqual(labelsOf(env), ['日本（56）', '架空県（50）', '旧表記県（4）', '歪み県（2）', '架空県 試験市（40）', '旧表記県 旧市（3）', '歪み県 歪み町（2）']);
  assert.deepEqual(plain(env.ev('ls.combo.items.map(i => i.legacy)')), [false, false, true, true, false, true, true]);
  await chooseOpt(env, '旧表記');
  assert.equal(env.ev('ls.sel.places.kind'), 'legacy_pref');
  assert.equal(photoQ(env).get('pref'), '旧表記県');
  assert.equal(photoQ(env).get('country'), null);
});

await test('(t7f) 候補の読み込みは古い応答を捨てる: 除外の切り替えを続けて 2 回して応答が逆順に届いても、開いている候補の一覧は新しい方で作る', async () => {
  const env = await intlEnv();
  const input = comboBox(env);
  typeInto(input, 'ドイツ');
  assert.deepEqual(labelsOf(env), ['ドイツ（10）']);
  const pending = [];
  env.handlers.places = () => new Promise(resolve => pending.push(resolve));
  for(const on of [true, false]){
    env.$('#lib-hidden').checked = on;
    env.$('#lib-hidden').dispatchEvent({type: 'change'});
  }
  await settle();
  assert.equal(pending.length, 4, '2 回 × （カード用・候補用）');
  const named = name => { const b = intlBody(); b.countries.find(c => c.id === 'ne:DEU').name = name; return b; };
  // 新しい 2 本（hidden=0）を先に、古い 2 本（hidden=1）をあとに返す
  const res = body => ({ok: true, status: 200, json: () => Promise.resolve(withCountries(body))});
  pending[2](res(named('ドイツ連邦'))); pending[3](res(named('ドイツ連邦')));
  await settle();
  assert.deepEqual(labelsOf(env), ['ドイツ連邦（10）']);
  pending[0](res(named('古いドイツ'))); pending[1](res(named('古いドイツ')));
  await settle();
  assert.deepEqual(labelsOf(env), ['ドイツ連邦（10）'], '遅れて届いた古い応答は捨てる');
  assert.equal(cardName(cardByKey(env, 'ne:DEU')), 'ドイツ連邦');
});

await test('(t7g) 地図の範囲で隠れている国も候補から選べる: 範囲を外してカードを読み直してから選ぶ。読み直しの間に別のカードを押せば選ばない（古い応答は捨てる）', async () => {
  const env = makeEnv();
  const held = [];
  env.handlers.places = params => {
    const body = intlBody();
    if(params.get('bbox')) body.countries = body.countries.filter(c => c.id === 'ne:FRA');       // 範囲の中はフランスだけ
    return params.get('bbox') || !held.hold ? reply(body) : new Promise(resolve => held.push(() => resolve(reply(body))));
  };
  await env.boot();
  await openLib(env);
  await openPlaces(env);
  userMove(env, {bounds: {w: 0, s: 45, e: 4, n: 49}, center: {lng: 2, lat: 47}});
  await settle();
  assert.deepEqual(allCards(env).map(cardName).slice(0, 1), ['フランス'], '範囲の中はフランスだけ');
  assert.ok(env.ev('ls.mapBbox'));
  // 隠れているドイツを候補から選ぶ → 範囲を外して読み直し、ドイツを選ぶ
  await chooseOpt(env, 'germany');
  assert.equal(env.ev('ls.mapBbox'), null);
  assert.equal(photoQ(env).get('country'), 'ne:DEU');
  assert.equal(env.ev('ls.sel.places.country'), 'ne:DEU');
  // 読み直しを保留にして、その間に別のカードを押す
  await press(env, cardByKey(env, 'ne:FRA'));                 // フランスを選んでおく（ドイツの候補を決めたときに変わったことが分かる）
  userMove(env, {bounds: {w: 0, s: 45, e: 4, n: 49}, center: {lng: 2, lat: 47}});
  await settle();
  assert.ok(env.ev('ls.mapBbox'));
  held.hold = true;
  const asked = () => env.reqs('/api/admin/library/photos').filter(c => c.url.includes('country=ne%3ADEU')).length;
  const before = asked();
  await chooseOpt(env, 'germany');
  assert.equal(held.length, 1, '範囲を外したカードの読み直しが保留になっている');
  assert.equal(env.ev('ls.mapBbox'), null);
  assert.equal(env.ev('ls.sel.places.country'), 'ne:FRA', 'まだ選んでいない（直前の選択のまま）');
  await press(env, cardByKey(env, 'ne:FRA/ne:FRA-r1'));
  assert.equal(env.ev('ls.sel.places.region'), 'ne:FRA-r1');
  held.splice(0).forEach(f => f());
  await settle();
  assert.equal(env.ev('ls.sel.places.country'), 'ne:FRA', '遅れて届いた読み直しはドイツを選ばない');
  assert.equal(env.ev('ls.sel.places.region'), 'ne:FRA-r1');
  assert.equal(asked(), before, '保留の間・あとにドイツの写真を要求していない');
});

await test('(t7h) 「選択を解除」: 旧地名の選択（カードが無く、選び直しても外れない）も、国・地域・範囲の選択も外せる。詳細を閉じ、入力欄を空にし、飛んでいる応答は捨て、フォーカスは view の切り替えへ', async () => {
  const env = await legacyEnv();
  const clear = () => env.$('#lib-sel-clear');
  assert.equal(clear().tagName.toLowerCase(), 'button');
  assert.equal(clear().textContent, '選択を解除');
  assert.equal(hidden(env, '#lib-detail'), true);
  await chooseOpt(env, '旧表記');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(env.ev('ls.sel.places.kind'), 'legacy_pref');
  // 同じ候補を選び直しても外れない
  await chooseOpt(env, '旧表記');
  assert.equal(env.ev('ls.sel.places.kind'), 'legacy_pref');
  clear().click();
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(hidden(env, '#lib-detail'), true);
  assert.equal(env.$('#lib-sel').textContent, '');
  assert.equal(comboBox(env).value, '', '入力欄の文字も空にする');
  assert.equal(tiles(env).length, 0);
  same(env.document.activeElement, env.$('#lib-view-places'));
  // 外したあと、同じ候補をもう一度選べる
  await chooseOpt(env, '旧表記');
  assert.equal(env.ev('ls.sel.places.kind'), 'legacy_pref');
  assert.equal(tiles(env).length > 0, true);
  // 読み込みを待っているあいだに外す: 遅れて届いた写真は出さない
  let release;
  env.handlers.photos = () => new Promise(res => { release = () => res({ok: true, status: 200, json: () => Promise.resolve({ok: true, page: 1, per: 60, pages: 1, total: 3, items: GRID})}); });
  await chooseOpt(env, '旧市');
  assert.equal(env.ev('ls.sel.places.city'), '旧市');
  clear().click();
  await settle();
  release();
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(tiles(env).length, 0, '外したあとに届いた応答は捨てる');
  assert.equal(hidden(env, '#lib-detail'), true);
  // 国のカードの選択も外せる（展開は残る）。カードの印も外れる
  env.handlers.photos = null;
  await press(env, cardByKey(env, 'ne:JPN'));
  assert.equal(mainOf(cardByKey(env, 'ne:JPN')).getAttribute('aria-pressed'), 'true');
  clear().click();
  await settle();
  assert.equal(env.ev('ls.sel.places'), null);
  assert.equal(mainOf(cardByKey(env, 'ne:JPN')).getAttribute('aria-pressed'), 'false');
  assert.equal(env.ev('ls.openCountry'), 'ne:JPN');
  // 旅の選択でも使える
  env.$('#lib-view-trips').click();
  await settle();
  pressTrip(env, 0);
  await settle();
  assert.equal(hidden(env, '#lib-detail'), false);
  clear().click();
  await settle();
  assert.equal(env.ev('ls.sel.trips'), null);
  assert.equal(hidden(env, '#lib-detail'), true);
  same(env.document.activeElement, env.$('#lib-view-trips'));
  clear().click();                       // 選択が無いときは何も起きない
  await settle();
  assert.equal(env.ev('ls.sel.trips'), null);
});

// ---- 写真詳細の国・地域（ライブラリのビューア・地図の吹き出し） ----
const REG_JP = {country_id: JP, country: '日本', region_id: jpId('架空県'), region: '架空県', method: 'pref', label: '日本・架空県'};
const REG_FR = {country_id: 'ne:FRA', country: 'フランス', region_id: 'ne:FRA-r1', region: '中央', method: 'inside', label: 'フランス・中央'};
const REG_FR_ONLY = {country_id: 'ne:FRA', country: 'フランス', region_id: null, region: null, method: 'inside', label: 'フランス'};
const R701 = lp({id: 701, name: 'jp.jpg', thumb_url: '/thumb/701.jpg?v=aaaaaaaaaaaa', location_region: REG_JP});
const R702 = lp({id: 702, name: 'fr.jpg', thumb_url: '/thumb/702.jpg?v=bbbbbbbbbbbb', pref: '', city: '', lat: 47.0, lon: 2.0, loc_source: 'exif', location_region: REG_FR});
const R703 = lp({id: 703, name: 'frc.jpg', thumb_url: '/thumb/703.jpg?v=cccccccccccc', pref: '', city: '', lat: 46.0, lon: 2.0, location_region: REG_FR_ONLY});
const R704 = lp({id: 704, name: 'old.jpg', thumb_url: '/thumb/704.jpg?v=dddddddddddd'});                       // キーなし（旧応答・国が決まらない写真）
const R705 = lp({id: 705, name: 'bad.jpg', thumb_url: '/thumb/705.jpg?v=eeeeeeeeeeee', pref: '', city: '', location_region: {...REG_FR, label: '<b>x</b>・<img src=x onerror=alert(1)>'}});
const REG_ITEMS = [R701, R702, R703, R704, R705];
const regEnv = async () => {
  const env = makeEnv({gridItems: REG_ITEMS});
  await env.boot();
  await openLib(env);
  pressTrip(env, 0);
  await settle();
  return env;
};
const regionRow = env => ({hidden: hidden(env, '#viewer-region'), parts: env.$('#viewer-region').children.map(c => c.textContent)});

await test('(t7i) AC4 ライブラリのビューアに国・地域: label を出し、境界データの分類（inside）だけ「撮影位置からの分類」。国だけ・キーなし・名前の中の HTML も扱える。GPS の由来のタグ（loc_source）とは別', async () => {
  const env = await regEnv();
  tiles(env)[0].click();
  assert.deepEqual(regionRow(env), {hidden: false, parts: ['日本・架空県']}, 'pref の写真に注記は付けない');
  assert.ok(env.$('#viewer-location').textContent.startsWith('架空県 試験市'), '地名の行は今までどおり');
  assert.ok(env.document.querySelectorAll('#viewer-badges .tag').some(t => t.textContent === 'EXIF'), 'GPS の由来のタグはそのまま');
  env.$('#viewer-next').click();
  assert.deepEqual(regionRow(env), {hidden: false, parts: ['フランス・中央', '撮影位置からの分類']});
  env.$('#viewer-next').click();
  assert.deepEqual(regionRow(env), {hidden: false, parts: ['フランス', '撮影位置からの分類']}, '地域が決まらなければ国だけ');
  env.$('#viewer-next').click();
  assert.equal(regionRow(env).hidden, true, 'キーが無い写真（旧 API・国が決まらない）は行ごと隠す');
  assert.equal(env.$('#viewer-region').textContent, '');
  env.$('#viewer-next').click();
  assert.deepEqual(regionRow(env).parts, ['<b>x</b>・<img src=x onerror=alert(1)>', '撮影位置からの分類']);
  assert.ok(env.$('#viewer-region').children.every(c => c.children.every(k => k.nodeType !== 1)), '名前から要素を作らない');
  env.$('#viewer-close').click();
});

await test('(t7j) AC4 地図の吹き出しにも国・地域（photos?ids= の location_region）: 注記つき・キーなしは行が隠れる。名前は textContent', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  const map = mapOf(env);
  const popupFor = async photo => {
    env.handlers.photos = () => reply({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [photo]});
    map.fire('click', {features: [{properties: {id: photo.id}, geometry: {coordinates: [20, 10]}}]}, 'lib-point');
    await settle();
    return env.fake.popups.at(-1);
  };
  let pop = await popupFor(R702);
  let row = pop.content.querySelector('.lib-pop-region');
  assert.equal(row.classList.contains('hidden'), false);
  assert.deepEqual(row.children.map(c => c.textContent), ['フランス・中央', '撮影位置からの分類']);
  pop = await popupFor(R701);
  assert.deepEqual(pop.content.querySelector('.lib-pop-region').children.map(c => c.textContent), ['日本・架空県']);
  pop = await popupFor(R704);
  row = pop.content.querySelector('.lib-pop-region');
  assert.equal(row.classList.contains('hidden'), true);
  assert.equal(row.textContent, '');
  pop = await popupFor(R705);
  assert.equal(pop.content.querySelector('.lib-pop-region').children[0].textContent, '<b>x</b>・<img src=x onerror=alert(1)>');
  assert.ok(pop.content.querySelector('.lib-pop-region').children.every(c => c.children.every(k => k.nodeType !== 1)));
});

await test('(t7k) AC7 位置修正の直後に、格子のその写真・開いているビューア・地図の吹き出しの古い国・地域が消える（地名の行は座標に変わる）。別の写真は変わらない', async () => {
  const env = await regEnv();
  const t = tiles(env);
  // 位置の指定（📍）から保存まで通した経路
  tiles(env)[1].click();                                // 702（フランス・中央）のビューア
  assert.deepEqual(regionRow(env).parts, ['フランス・中央', '撮影位置からの分類']);
  env.$('#viewer-close').click();
  t[1].querySelector('.lib-loc').click();
  const mini = env.fake.maps.at(-1);
  mini.fire('click', {lngLat: {lat: 35.5, lng: 139.5}});
  env.$('#m-save').click();
  await settle();
  assert.equal(env.ev('ls.items[1].location_region'), null, '格子の写真から消える');
  assert.equal(env.ev('ls.items[0].location_region.label'), '日本・架空県', '別の写真は変わらない');
  tiles(env)[1].click();
  assert.equal(regionRow(env).hidden, true, '開き直したビューアにも古い国・地域が出ない');
  env.$('#viewer-close').click();
  // ビューアを開いたまま通知が来ても、その場で消える
  tiles(env)[2].click();
  assert.deepEqual(regionRow(env).parts, ['フランス', '撮影位置からの分類']);
  vm.runInContext("applyPhoto(703, {lat: 1.5, lon: 2.5, loc_source: 'manual', infer_bucket: '', place: '', pref: '', city: ''})", env.context);
  assert.equal(regionRow(env).hidden, true, '位置の修正の直後に消える');
  assert.equal(env.ev('ls.items[2].location_region'), null);
  env.$('#viewer-close').click();
  // 地図の吹き出し: 別の object の 1 枚でも消え、地名の行も座標に変わる
  env.$('#lib-view-places').click();
  await settle();
  const map = env.fake.maps.find(m => m !== mini);            // 位置の指定の地図ではなく、ライブラリの地図
  env.handlers.photos = () => reply({ok: true, page: 1, per: 60, pages: 1, total: 1, items: [R702]});
  map.fire('click', {features: [{properties: {id: 702}, geometry: {coordinates: [2, 47]}}]}, 'lib-point');
  await settle();
  const pop = env.fake.popups.at(-1);
  assert.deepEqual(pop.content.querySelector('.lib-pop-region').children.map(c => c.textContent), ['フランス・中央', '撮影位置からの分類']);
  vm.runInContext("applyPhoto(702, {lat: 12.34567, lon: 23.45678, loc_source: 'manual', infer_bucket: '', place: '', pref: '', city: ''})", env.context);
  assert.equal(pop.content.querySelector('.lib-pop-region').classList.contains('hidden'), true);
  assert.equal(pop.content.querySelector('.lib-pop-region').textContent, '');
  assert.equal(pop.content.querySelector('.lib-pop-place').textContent, '12.3457, 23.4568');
  // 「開く」で開いたビューアにも古い国・地域は出ない
  pop.content.querySelector('.lib-pop-open').click();
  assert.equal(regionRow(env).hidden, true);
  env.$('#viewer-close').click();
});

await test('(t7l) AC7 位置が変わると、国・地域に由来する一覧も読み直す: 場所の画面ならカードと候補、旅の画面なら旅の地名。見えていない view・タブなら印だけ立てて、開くときに読む。飛んでいた古い応答は捨てる。★だけでは読まない', async () => {
  const env = await regEnv();
  const count = prefix => env.reqs(prefix).length;
  // 旅の画面（今ここ）: 旅を読み直す。場所は読まない
  const t0 = count('/api/admin/library/trips'), p0 = count('/api/admin/library/places');
  vm.runInContext("applyPhoto(701, {favorite: true})", env.context);
  await settle();
  assert.equal(count('/api/admin/library/trips'), t0, '★だけでは読まない');
  vm.runInContext("applyPhoto(701, {lat: 1, lon: 2, loc_source: 'manual'})", env.context);
  await settle();
  assert.equal(count('/api/admin/library/trips'), t0 + 1);
  assert.equal(count('/api/admin/library/places'), p0);
  assert.equal(env.ev('ls.loaded.placeCards'), false, '見えていない場所の一覧は開くときに読む');
  // 場所の画面へ移ると読む（カードは毎回、候補は loaded が落ちているので）
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(count('/api/admin/library/places'), p0 + 2, 'カード用と候補用');
  // 場所の画面: 位置が変わるとカードと候補をすぐ読み直す
  vm.runInContext("applyPhoto(702, {lat: 3, lon: 4, loc_source: 'manual'})", env.context);
  await settle();
  assert.equal(count('/api/admin/library/places'), p0 + 4);
  // ライブラリを離れていれば（写真タブの位置の修正など）読まず、印だけ
  env.$('.tab[data-tab=status]').click();
  await settle();
  const p1 = count('/api/admin/library/places'), t1 = count('/api/admin/library/trips');
  vm.runInContext("applyPhoto(703, {lat: 5, lon: 6, loc_source: 'manual'})", env.context);
  await settle();
  assert.equal(count('/api/admin/library/places'), p1);
  assert.equal(count('/api/admin/library/trips'), t1);
  assert.equal(env.ev('ls.loaded.placeCards || ls.loaded.placeOptions || ls.loaded.trips'), false);
  await openLib(env);
  assert.ok(count('/api/admin/library/places') >= p1 + 2, 'タブを開くと読む');
  assert.ok(count('/api/admin/library/trips') >= t1 + 1);
});

// ---- 旅のカードの地名（regions[] の上位。海外の旅で国・地域名が出る） ----
const tagsOf = card => card.querySelectorAll('.tag').map(t => t.textContent);
const TRIP_ABROAD = {key: '2030-08-01 08:00', start: '2030-08-01 08:00', end: '2030-08-09 20:00', count: 90, noloc: 2,
  places: [{pref: '', city: '', count: 88}],
  regions: [{country_id: 'ne:FRA', country: 'フランス', region_id: 'ne:FRA-r1', region: '中央', label: 'フランス・中央', count: 60},
            {country_id: 'ne:DEU', country: 'ドイツ', region_id: 'ne:DEU-r1', region: 'バイエルン', label: 'ドイツ・バイエルン', count: 20},
            {country_id: 'ne:DEU', country: 'ドイツ', region_id: null, region: null, label: 'ドイツ', count: 8}],
  cover: null};
const TRIP_HOME = {key: '2030-05-03 09:12', start: '2030-05-03 09:12', end: '2030-05-05 18:40', count: 212, noloc: 7,
  places: [{pref: '架空県', city: '試験市', count: 150}, {pref: '架空県', city: '見本町', count: 20}, {pref: '', city: '', count: 3}],
  regions: [{country_id: JP, country: '日本', region_id: jpId('架空県'), region: '架空県', label: '日本・架空県', count: 170},
            {country_id: JP, country: '日本', region_id: jpId('境界県'), region: '境界県', label: '日本・境界県', count: 5},
            {country_id: JP, country: '日本', region_id: null, region: null, label: '日本', count: 3}],
  cover: null};
const TRIP_OLD = {key: '2029-01-01 00:00', start: '2029-01-01 00:00', end: '2029-01-01 00:00', count: 1, noloc: 0,
  places: [{pref: '', city: '', count: 1}], cover: null};

await test('(t7m) AC4 旅のカードの地名: 海外の旅は regions[] の国・地域（件数順）で「海外・不明」を出さない。国内は places[] の「都道府県 市区町村」を保ち、places[] にない日本の分類（境界データ）だけ足す。regions[] の無い旧応答は今までどおり', async () => {
  const env = makeEnv();
  env.handlers.trips = () => reply({ok: true, trips: [TRIP_ABROAD, TRIP_HOME, TRIP_OLD], untimed: 0});
  await env.boot();
  await openLib(env);
  const c = cards(env);
  assert.deepEqual(tagsOf(c[0]), ['フランス・中央 60', 'ドイツ・バイエルン 20', 'ドイツ 8']);
  assert.deepEqual(tagsOf(c[1]), ['架空県 試験市 150', '架空県 見本町 20', '日本・境界県 5'], '同じ都道府県と、地域の決まらない日本（旧地名など）は places[] の行があれば足さない');
  assert.deepEqual(tagsOf(c[2]), ['海外・不明 1'], '旧応答（regions なし）は places[] のまま');
  // 旅の文字検索も同じ地名で当たる
  const input = searchBox(env);
  typeInto(input, 'フランス');
  assert.deepEqual(cards(env).map(x => mainOf(x).querySelector('.lib-card-name').textContent), ['2030-08-01 08:00 〜 2030-08-09 20:00']);
  typeInto(input, 'バイエルン');
  assert.equal(cards(env).length, 1);
  typeInto(input, '境界県');
  assert.equal(cards(env).length, 1);
  typeInto(input, '海外');
  assert.deepEqual(cards(env).map(x => tagsOf(x)[0]), ['海外・不明 1'], '海外の旅の「海外・不明」は出さないので、旧応答の旅だけ');
  // 名前は textContent
  const evil = {...TRIP_ABROAD, regions: [{...TRIP_ABROAD.regions[0], label: '<img src=x onerror=alert(1)>・<b>x</b>'}]};
  env.handlers.trips = () => reply({ok: true, trips: [evil], untimed: 0});
  env.$('.tab[data-tab=status]').click();
  await openLib(env);
  typeInto(searchBox(env), '');
  const tag = cards(env)[0].querySelector('.tag');
  assert.equal(tag.textContent, '<img src=x onerror=alert(1)>・<b>x</b> 60');
  assert.ok(tag.children.every(k => k.nodeType !== 1));
});
