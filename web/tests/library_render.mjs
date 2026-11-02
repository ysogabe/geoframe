// admin.js・photos.js・library.js（この順に同じ context へ読む）のライブラリタブ — 旅・日付のカード（選ぶと下の詳細に写真の格子）・
// 年の月のチップ・除外の切り替え・古い応答の捨て方・ビューア・★の反映・場所の行・地図（偽の MapLibre）・吹き出し・
// 「この範囲の写真を見る」— を、fakedom.mjs の DOM の代用品と fetch・MapLibre の代用品で動かして確かめる。
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
  {pref: '架空県', count: 50, cities: [{city: '試験市', count: 40}, {city: '見本町', count: 8}, {city: '', count: 2}]},
  {pref: '試験県', count: 3, cities: [{city: 'サンプル市', count: 3}]}]});

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
  const document = buildAdminDom();
  const calls = [];
  const flags = {};
  const handlers = {photos: null, points: null, trips: null, calendar: null, places: null, tabList: null, search: null};   // (params, rec) => Promise | undefined
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
    if(url.startsWith('/api/admin/library/places')){
      return (handlers.places && handlers.places(params, rec)) || reply(placesBody());
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
  });
  loadScripts(context);
  const reqs = prefix => calls.filter(c => c.url.startsWith(prefix));
  const last = prefix => {
    const c = reqs(prefix).at(-1);
    return c ? new URLSearchParams(c.url.split('?')[1] || '') : null;
  };
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  const ev = code => JSON.parse(vm.runInContext('JSON.stringify(' + code + ')', context));
  return {document, context, calls, handlers, fake, reqs, last, boot, ev, $: s => document.querySelector(s),
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
  assert.equal(hidden(env, '#lib-map-box'), true);
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
  // 「地図で見る」（バブリングするクリックでも）: 地図の画面へ移るだけで、主の選択は変わらない・写真も読まない
  const mapBtn = cards(env)[1].querySelector('.lib-card-map');
  mapBtn.dispatchEvent({type: 'click', bubbles: true});
  await settle();
  assert.equal(photoReqs(), 3);
  assert.equal(env.ev('ls.sel.trips.key'), '2030-05-03 09:12', '旅の選択はそのまま');
  assert.equal(env.ev('ls.sel.map'), null);
  assert.equal(env.ev('ls.view'), 'map');
  assert.equal(env.last('/api/admin/library/points').get('from'), '2029-01-01 00:00');
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
  env.$('#lib-view-map').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/points').length, 1);
  assert.equal(env.reqs('/api/admin/library/points')[0].url, '/api/admin/library/points', '期間なし・除外を含めない');
  assert.equal(hidden(env, '#lib-map-box'), false);
  assert.equal(hidden(env, '#lib-trips'), true);
  assert.deepEqual(activeTabs(env), ['library']);
  assert.ok(env.$('#lib-view-map').classList.contains('on'));
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
  assert.ok(map.calls.resize >= 1, '表示したとき大きさを合わせる');
  // 切り捨てのとき
  env.handlers.points = () => reply(pointsBody({truncated: true, total: 25000}));
  env.$('#lib-view-trips').click();
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
  await settle();
  const live = env.fake.markers.filter(m => !m.removed);
  assert.equal(live.length, 1);
  assert.equal(map.sources.photos.setDataCalls.length, 2);
});

await test('(e2) クラスタを押すと展開できる zoom まで寄る。単独の点・クラスタの上ではカーソルが指になる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
  await settle();
  assert.equal(hidden(env, '#lib-detail'), true);
  env.fake.bounds = {w: 100.1234567, s: 10.5, e: 120.25, n: 30.75};
  env.$('#lib-bbox').click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '100.123457,10.5,120.25,30.75');
  assert.equal(q.get('from'), null);
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-map-box'), false, '地図は残る');
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
  env.$('#lib-view-map').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs + 1);
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '-180,-90,180,90');
  assert.equal(hidden(env, '#lib-detail'), false);
});
await test('(g2) 地図は周回させない（renderWorldCopies: false・maxBounds）。経度が ±180 を超える表示でも bbox は範囲内で w <= e', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
  await settle();
  assert.equal(pointReqs().length, 3);
  assert.equal(env.last('/api/admin/library/points').get('hidden'), null);
  assert.equal(env.ev('ls.pointsStale'), false);
  // 点の応答を待っている間に切り替えたら、古い設定の応答は捨てる
  const held = [];
  env.handlers.points = () => new Promise(res => held.push(() => res({ok: true, status: 200, json: () =>
    Promise.resolve(pointsBody({features: [feature(999, 1, 1)]}))})));
  env.$('#lib-view-trips').click();
  env.$('#lib-view-map').click();           // 点の要求が飛ぶ（まだ応答なし）
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
  const toggled = ['lib-trips', 'lib-calendar', 'lib-places', 'lib-map-box', 'lib-months', 'lib-detail', 'lib-period', 'lib-empty', 'viewer-noimage'];
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
  // 古い部品は無い
  assert.ok(!html.includes('lib-back') && !html.includes('lib-grid-box'));
});
await test('(h) 旅の「地図で見る」: その期間で points を読み、期間を表示し、解除で全期間に戻る。範囲の写真にも期間が付く', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  cards(env)[0].querySelector('.lib-card-map').click();
  await settle();
  assert.equal(hidden(env, '#lib-map-box'), false);
  assert.ok(env.$('#lib-view-map').classList.contains('on'));
  const q = env.last('/api/admin/library/points');
  assert.equal(q.get('from'), '2030-05-03 09:12');
  assert.equal(q.get('to'), '2030-05-05 18:40');
  assert.equal(hidden(env, '#lib-period'), false);
  assert.equal(env.$('#lib-period-text').textContent, '期間: 2030-05-03 09:12 〜 2030-05-05 18:40');
  env.$('#lib-bbox').click();
  await settle();
  const g = env.last('/api/admin/library/photos');
  assert.equal(g.get('bbox'), '100,10,120,30');
  assert.equal(g.get('from'), '2030-05-03 09:12');
  assert.equal(g.get('to'), '2030-05-05 18:40');
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲・2030-05-03 09:12 〜 2030-05-05 18:40');
  env.$('#lib-period-clear').click();
  await settle();
  assert.equal(hidden(env, '#lib-period'), true);
  const p = env.last('/api/admin/library/points');
  assert.equal(p.get('from'), null);
  assert.equal(p.get('to'), null);
});
await test('(h2) 旅の「地図で見る」は前の旅の範囲選択を持ち越さない: 旅 A → 地図で見る → この範囲の写真を見る → 旅へ戻る → 旅 B の地図で見る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const A = '2030-05-03 09:12', B = '2029-01-01 00:00';
  let releaseA;
  env.handlers.photos = params => {
    if(params.get('bbox') && params.get('from') === A){       // 旅 A の範囲の写真（応答は保留）
      return new Promise(res => { releaseA = () => res({ok: true, status: 200, json: () => Promise.resolve(
        {ok: true, page: 1, per: 60, pages: 1, total: 2, items: [P501, P502]})}); });
    }
    return null;
  };
  cards(env)[0].querySelector('.lib-card-map').click();      // 旅 A の地図
  await settle();
  env.$('#lib-bbox').click();                                  // この範囲の写真を見る（保留）
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('from'), A);
  env.$('#lib-view-trips').click();                            // 旅へ戻る
  await settle();
  const photoReqs = env.reqs('/api/admin/library/photos').length;
  cards(env)[1].querySelector('.lib-card-map').click();      // 旅 B の地図
  await settle();
  assert.equal(env.last('/api/admin/library/points').get('from'), B);
  assert.equal(env.$('#lib-period-text').textContent, '期間: 2029-01-01 00:00');
  assert.equal(env.reqs('/api/admin/library/photos').length, photoReqs, 'A の期間の写真を読み直さない');
  assert.equal(hidden(env, '#lib-detail'), true, 'A の範囲選択は解除されている');
  assert.equal(env.$('#lib-sel').textContent, '');
  assert.equal(tiles(env).length, 0);
  assert.equal(env.ev('ls.sel.map'), null);
  // 遅れて A の応答が届いても捨てる
  releaseA();
  await settle();
  assert.equal(tiles(env).length, 0, '遅れた A の詳細は出さない');
  assert.equal(hidden(env, '#lib-detail'), true);
  // B の範囲の写真は B の期間で読む
  env.handlers.photos = null;
  env.$('#lib-bbox').click();
  await settle();
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), B);
  assert.equal(q.get('to'), B);
});
await test('(i) 点の応答が遅れて入れ違っても、新しい要求の結果だけが source に渡る', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const release = [];
  env.handlers.points = () => new Promise(res => release.push(body => res({ok: true, status: 200, json: () => Promise.resolve(body)})));
  env.$('#lib-view-map').click();
  await settle();
  env.$('#lib-view-trips').click();
  env.$('#lib-view-map').click();                  // 2 つ目の要求
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
  await settle();
  const map = mapOf(env);
  const resizes = map.calls.resize, trips = env.reqs('/api/admin/library/trips').length;
  env.$('.tab[data-tab=status]').click();
  await settle();
  await openLib(env);
  assert.equal(env.reqs('/api/admin/library/trips').length, trips + 1);
  assert.ok(map.calls.resize > resizes);
  assert.equal(hidden(env, '#lib-map-box'), false);
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
  assert.deepEqual(plain(env.ev('ls.loaded')), {trips: true, calendar: false, places: false});
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
  assert.equal(env.ev('ls.loaded.places'), false);
  assert.equal(env.ev('ls.prefs.length'), 2, '飛んでいた空の応答で一覧を差し替えない');
});
const placeRows = env => env.$$('#lib-places .lib-grow');
const rowText = row => [row.querySelector('.lib-gname').textContent, row.querySelector('.lib-gcount').textContent];
const showBtn = row => row.querySelector('.lib-gphotos');
await test('(q) 場所（今の行の作り）: 都道府県の行を押すと市区町村の行が開き、どちらからも pref（と city）付きで下の詳細に出る。海外・不明は abroad=1', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.reqs('/api/admin/library/places')[0].url, '/api/admin/library/places');
  assert.ok(env.$('#lib-view-places').classList.contains('on'));
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(hidden(env, '#lib-calendar'), true);
  assert.equal(env.$('#lib-note').textContent, '2 都道府県');
  let r = placeRows(env);
  assert.deepEqual(r.map(rowText), [['架空県', '50 枚'], ['試験県', '3 枚']]);
  assert.equal(env.$('#lib-places .lib-summary .lib-summary-text').textContent, '海外・不明（9 枚）');
  // 開く
  r[0].querySelector('.lib-expand').click();
  r = placeRows(env);
  assert.deepEqual(r.map(rowText), [['架空県', '50 枚'], ['試験市', '40 枚'], ['見本町', '8 枚'], ['（市区町村なし）', '2 枚'],
                                    ['試験県', '3 枚']]);
  assert.equal(showBtn(r[3]), null, '市区町村が空の項目は city で絞れないので、写真を見るボタンを置かない');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '開いただけでは読まない');
  // 市区町村 → pref と city。下の詳細に出て、一覧は残る
  showBtn(r[2]).click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('pref'), '架空県');
  assert.equal(q.get('city'), '見本町');
  assert.equal(q.get('from'), null);
  assert.equal(q.get('abroad'), null);
  assert.equal(env.$('#lib-sel').textContent, '架空県 見本町');
  assert.equal(hidden(env, '#lib-detail'), false);
  assert.equal(hidden(env, '#lib-places'), false);
  assert.equal(tiles(env).length, 3);
  assert.deepEqual(placeRows(env).map(x => x.classList.contains('is-selected')), [true, false, true, false, false], '選んだ市区町村と、その県');
  // 都道府県 → pref だけ
  showBtn(placeRows(env)[4]).click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('pref'), '試験県');
  assert.equal(q.get('city'), null);
  assert.equal(env.$('#lib-sel').textContent, '試験県');
  // 海外・不明
  env.$('#lib-places .lib-summary .lib-summary-show').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('abroad'), '1');
  assert.equal(q.get('pref'), null);
  assert.equal(q.get('city'), null);
  assert.equal(q.get('untimed'), null);
  assert.equal(env.$('#lib-sel').textContent, '海外・不明');
  assert.ok(env.$('#lib-places .lib-summary').classList.contains('is-selected'));
  // 除外を含める → 詳細にも場所の一覧にも hidden=1（abroad も付いたまま）
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('abroad'), '1');
  assert.equal(env.last('/api/admin/library/places').get('hidden'), '1');
  // 空・失敗
  env.handlers.places = () => reply({ok: true, prefs: [], abroad: 0});
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  assert.equal(env.$('#lib-places').textContent, '写真がありません');
  env.handlers.places = () => respond(500, {ok: false, error: 'boom'});
  env.$('#lib-view-trips').click();
  env.$('#lib-view-places').click();
  await settle();
  assert.ok(env.$('#toast').textContent.includes('場所の一覧を読めませんでした: boom'));
});
await test('(q2) ライブラリタブを開き直すと、日付・場所のビューのままなら一覧を読み直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-places').click();
  await settle();
  env.$('.tab[data-tab=status]').click();
  await settle();
  await openLib(env);
  assert.equal(env.reqs('/api/admin/library/places').length, 2);
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
  env.$('#lib-view-map').click();
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
