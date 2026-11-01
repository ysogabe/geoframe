// admin.js・photos.js・library.js（この順に同じ context へ読む）のライブラリタブ — 旅の一覧・格子・ビューア・
// ★の反映・地図（偽の MapLibre）・吹き出し・「この範囲の写真を見る」— を、fakedom.mjs の DOM の代用品と
// fetch・MapLibre の代用品で動かして確かめる。
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
  {year: '2030', count: 30, months: [{month: '05', count: 20}, {month: '12', count: 10}]},
  {year: '2029', count: 4, months: [{month: '01', count: 4}]}]});
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
const rows = env => env.$$('#lib-trips .lib-trip');
const tiles = env => env.$$('#lib-grid .lib-tile');
const marksOf = tile => tile.querySelectorAll('.lib-mark').map(m => m.textContent);
const activeTabs = env => env.$$('nav .tab').filter(t => t.classList.contains('on')).map(t => t.dataset.tab);
const mapOf = env => env.fake.maps[0];
// vm の中で作られた配列・object は別の realm なので、deepEqual の前に JSON を通して普通の値にする
const plain = v => JSON.parse(JSON.stringify(v));

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
    assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(text), name);
  }
});

await test('(a) 旅の一覧: 期間・枚数（位置なし）・場所の上位・表紙・日時なしの行が描かれる', async () => {
  const env = makeEnv();
  await env.boot();
  assert.equal(hidden(env, '#v-library'), true, '起動時は状況タブ');
  assert.deepEqual(env.reqs('/api/admin/library/'), [], 'タブを開くまで読まない');
  await openLib(env);
  assert.deepEqual(activeTabs(env), ['library']);
  assert.equal(hidden(env, '#v-library'), false);
  assert.equal(env.reqs('/api/admin/library/trips').length, 1);
  assert.equal(env.reqs('/api/admin/library/trips')[0].url, '/api/admin/library/trips', '既定は除外を含めない');
  const r = rows(env);
  assert.equal(r.length, 3, '旅 2 つ + 日時なし');
  assert.equal(r[0].querySelector('.lib-trip-range').textContent, '2030-05-03 09:12 〜 2030-05-05 18:40');
  assert.equal(r[0].querySelector('.lib-trip-count').textContent, '212 枚（位置なし 7）');
  assert.deepEqual(r[0].querySelectorAll('.tag').map(t => t.textContent), ['架空県 試験市 150', '海外・不明 40']);
  const cover = r[0].querySelector('.lib-cover img');
  assert.equal(cover.src, '/thumb/501.jpg?v=aaaaaaaaaaaa');
  assert.equal(cover.loading, 'lazy');
  assert.equal(r[1].querySelector('.lib-trip-range').textContent, '2029-01-01 00:00', '1 枚だけの旅は期間を 1 つだけ');
  assert.equal(r[1].querySelector('.lib-cover .img-broken').textContent, '画像なし', '表紙が無い旅');
  assert.equal(r[2].querySelector('.lib-trip-range').textContent, '日時なし（12 枚）');
  assert.equal(env.$('#lib-note').textContent, '旅 2 件');
  assert.equal(hidden(env, '#lib-trips'), false);
  assert.equal(hidden(env, '#lib-grid-box'), true);
  assert.equal(hidden(env, '#lib-map-box'), true);
  // 表紙が読めないときも代わりの表示
  cover.dispatchEvent({type: 'error'});
  assert.equal(r[0].querySelector('.lib-cover img'), null);
  assert.equal(r[0].querySelector('.lib-cover .img-broken').textContent, '画像なし');
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
  assert.equal(rows(env).length, 1);
  env.handlers.trips = () => reply({ok: true, trips: [], untimed: 0});
  box.checked = false;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/trips').get('hidden'), null);
  assert.equal(env.$('#lib-trips').textContent, '写真がありません');
});

await test('(c) 旅を押すと from/to 付きで写真を読み、サムネイルの格子（印・遅延読み込み・画像なし）が出る。戻るで一覧へ', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  rows(env)[0].querySelector('.lib-trip-photos').click();
  await settle();
  const q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2030-05-03 09:12');
  assert.equal(q.get('to'), '2030-05-05 18:40');
  assert.equal(q.get('page'), '1');
  assert.equal(q.get('per'), '60');
  assert.equal(q.get('hidden'), null);
  assert.equal(hidden(env, '#lib-grid-box'), false);
  assert.equal(hidden(env, '#lib-trips'), true);
  assert.equal(env.$('#lib-sel').textContent, '2030-05-03 09:12 〜 2030-05-05 18:40');
  assert.equal(env.$('#lib-pageinfo').textContent, '3件 / 1 of 1ページ');
  assert.equal(env.$('#lib-prev').disabled, true);
  assert.equal(env.$('#lib-next').disabled, true);
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
  // 戻る
  env.$('#lib-back').click();
  assert.equal(hidden(env, '#lib-grid-box'), true);
  assert.equal(hidden(env, '#lib-trips'), false);
  // 日時なし
  rows(env)[2].querySelector('.lib-trip-photos').click();
  await settle();
  const u = env.last('/api/admin/library/photos');
  assert.equal(u.get('untimed'), '1');
  assert.equal(u.get('from'), null);
});

await test('(c2) 格子のページング: 次へ・前へで page を付けて読み直す。古い応答は捨てる', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  const pageBody = (page, items) => reply({ok: true, page, per: 60, pages: 3, total: 130, items});
  env.handlers.photos = params => pageBody(Number(params.get('page')), params.get('page') === '2' ? [P502] : GRID);
  rows(env)[0].querySelector('.lib-trip-photos').click();
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

await test('(d) タイルを押すとビューアが開き、★は setflag にその写真の id を送って印が変わり、閉じるとタイルへフォーカスが戻る', async () => {
  const env = makeEnv();
  await env.boot();
  // 写真タブの一覧（別の写真）を先に読んでおく
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const tabBefore = env.ev('ps.entries.map(e => [e.photo.id, e.photo.favorite, e.photo.hidden])');
  assert.deepEqual(tabBefore, [[1, false, false], [2, false, false]]);
  await openLib(env);
  rows(env)[0].querySelector('.lib-trip-photos').click();
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
  assert.equal(env.document.activeElement, env.$('#viewer-close'));
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
  assert.equal(env.document.activeElement, t[1]);
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
  rows(env)[0].querySelector('.lib-trip-photos').click();
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

await test('(g) 「この範囲の写真を見る」: 表示範囲を ±180/±90 に丸めた bbox で写真を読み、格子を出す。戻ると地図の大きさを合わせ直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-map').click();
  await settle();
  const map = mapOf(env);
  env.fake.bounds = {w: 100.1234567, s: 10.5, e: 120.25, n: 30.75};
  env.$('#lib-bbox').click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '100.123457,10.5,120.25,30.75');
  assert.equal(q.get('from'), null);
  assert.equal(hidden(env, '#lib-grid-box'), false);
  assert.equal(hidden(env, '#lib-map-box'), true);
  assert.equal(env.$('#lib-sel').textContent, '地図の範囲');
  assert.equal(tiles(env).length, 3);
  const resizes = map.calls.resize;
  env.$('#lib-back').click();
  assert.equal(hidden(env, '#lib-map-box'), false);
  assert.ok(map.calls.resize > resizes, '戻ったとき地図の大きさを合わせ直す');
  // 世界を引いた表示（範囲外の値）は丸める
  env.fake.bounds = {w: -250.5, s: -95, e: 300, n: 91.5};
  env.$('#lib-bbox').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('bbox'), '-180,-90,180,90');
});

await test('(g2) 地図は周回させない（renderWorldCopies: false・maxBounds）。経度が ±180 を超える表示でも bbox は範囲内で w <= e', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-map').click();
  await settle();
  const map = mapOf(env);
  assert.equal(map.options.renderWorldCopies, false);
  assert.deepEqual(plain(map.options.maxBounds), [[-180, -85], [180, 85]]);
  env.fake.bounds = {w: 190, s: 10, e: 210, n: 30};
  env.$('#lib-bbox').click();
  await settle();
  const bbox = env.last('/api/admin/library/photos').get('bbox').split(',').map(Number);
  assert.deepEqual(bbox, [-180, 10, 180, 30], '潰れた範囲は経度の全域にする');
  assert.ok(bbox[0] >= -180 && bbox[2] <= 180 && bbox[0] <= bbox[2]);
  env.$('#lib-back').click();
  env.fake.bounds = {w: 179, s: 10, e: 210, n: 30};      // 東だけ超える: 範囲内に丸めた w < e
  env.$('#lib-bbox').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('bbox'), '179,10,180,30');
});

await test('(m) 範囲の格子を見ている間に「除外も含める」を切り替えても、地図に戻ると点を hidden=1 で読み直す（設定が食い違わない）', async () => {
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
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1', '格子は読み直す');
  assert.equal(env.reqs('/api/admin/library/trips').length, trips + 1, '旅の一覧も読み直す');
  assert.equal(pointReqs().length, 1, '地図が見えていないあいだは点を読まない');
  env.$('#lib-back').click();
  await settle();
  assert.equal(pointReqs().length, 2, '地図に戻ったとき読み直す');
  assert.equal(env.last('/api/admin/library/points').get('hidden'), '1');
  assert.equal(hidden(env, '#lib-map-box'), false);
  env.$('#lib-back').click();      // 戻るを重ねても、点を取り直すのは 1 回だけ
  assert.equal(pointReqs().length, 2);
  // 旅の一覧の上で切り替えたときも、地図ビューへ移るときに読む（setLibView）。除外を外すと hidden なし
  env.$('#lib-view-trips').click();
  env.$('#lib-hidden').checked = false;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(pointReqs().length, 2);
  env.$('#lib-view-map').click();
  await settle();
  assert.equal(pointReqs().length, 3);
  assert.equal(env.last('/api/admin/library/points').get('hidden'), null);
  // 点の応答を待っている間に切り替えたら、古い設定の応答は捨てる
  const held = [];
  env.handlers.points = () => new Promise(res => held.push(() => res({ok: true, status: 200, json: () =>
    Promise.resolve(pointsBody({features: [feature(999, 1, 1)]}))})));
  env.$('#lib-view-trips').click();
  env.$('#lib-view-map').click();           // 点の要求が飛ぶ（まだ応答なし）
  env.$('#lib-bbox').click();               // 格子へ
  await settle();
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
  rows(env)[0].querySelector('.lib-trip-photos').click();
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
  const toggled = ['lib-trips', 'lib-calendar', 'lib-places', 'lib-map-box', 'lib-grid-box', 'lib-period', 'lib-empty'];
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
  // 逆向きの確認: .lib-* / #lib-* の display を持つ規則のうち、.hidden を付ける要素のものは上の対を持つ
  const libDisplay = rules.filter(([sel, body]) => /^[#.]lib-[\w-]+$/.test(sel) && /(^|;)\s*display:/.test(body)).map(([sel]) => sel);
  const hasPair = sel => rules.some(([rsel]) => rsel === sel + '.hidden');
  for(const sel of libDisplay){
    const isToggled = toggled.some(id => selectorsOf(id).includes(sel));
    if(isToggled) assert.ok(hasPair(sel), sel);
  }
});

await test('(h) 旅の「地図で見る」: その期間で points を読み、期間を表示し、解除で全期間に戻る。範囲の写真にも期間が付く', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  rows(env)[0].querySelector('.lib-trip-map').click();
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
  env.$('#lib-back').click();
  env.$('#lib-period-clear').click();
  await settle();
  assert.equal(hidden(env, '#lib-period'), true);
  const p = env.last('/api/admin/library/points');
  assert.equal(p.get('from'), null);
  assert.equal(p.get('to'), null);
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

await test('(k) どのタブから離れてもビューアが閉じ、ライブラリへ戻ると旅を読み直して地図の大きさを合わせ直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  rows(env)[0].querySelector('.lib-trip-photos').click();
  await settle();
  tiles(env)[0].click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('.tab[data-tab=status]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), -1);
  assert.equal(hidden(env, '#v-library'), true);
  // 設定タブへ移るときも
  await openLib(env);
  tiles(env)[0].click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('.tab[data-tab=config]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  // 地図ビューのままタブを往復する
  await openLib(env);
  env.$('#lib-back').click();
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
  assert.equal(rows(env).length, 0);
});

const groupRows = (env, box) => env.$$('#' + box + ' .lib-grow');
const rowText = row => [row.querySelector('.lib-gname').textContent, row.querySelector('.lib-gcount').textContent];
const showBtn = row => row.querySelector('.lib-gphotos');

await test('(p) 日付: 年の行（件数つき）を押すと月の行が開き、年・月のどちらからも from/to 付きで格子が出る。日時なしは untimed=1', async () => {
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
  // 年の行だけ（月は閉じている）。新しい年が先
  let r = groupRows(env, 'lib-calendar');
  assert.deepEqual(r.map(rowText), [['2030年', '30 枚'], ['2029年', '4 枚']]);
  assert.equal(r[0].querySelector('.lib-expand').getAttribute('aria-expanded'), 'false');
  // 末尾に日時なし
  assert.equal(env.$('#lib-calendar .lib-untimed .lib-trip-range').textContent, '日時なし（5 枚）');
  // 年を押すと月の行が開く（件数つき）。ほかの年は閉じたまま
  r[0].querySelector('.lib-expand').click();
  r = groupRows(env, 'lib-calendar');
  assert.deepEqual(r.map(rowText), [['2030年', '30 枚'], ['05月', '20 枚'], ['12月', '10 枚'], ['2029年', '4 枚']]);
  assert.equal(r[0].querySelector('.lib-expand').getAttribute('aria-expanded'), 'true');
  assert.equal(r[1].querySelector('.lib-expand'), null, '月の行は開閉しない');
  assert.equal(env.reqs('/api/admin/library/photos').length, 0, '開いただけでは格子を読まない');
  // 月の「写真を見る」
  showBtn(r[2]).click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2030-12-01 00:00');
  assert.equal(q.get('to'), '2030-12-31 23:59');
  assert.equal(q.get('pref'), null);
  assert.equal(hidden(env, '#lib-grid-box'), false);
  assert.equal(hidden(env, '#lib-calendar'), true);
  assert.equal(env.$('#lib-sel').textContent, '2030年12月');
  assert.equal(tiles(env).length, 3);
  // 戻ると日付の一覧（開いた年は開いたまま）
  env.$('#lib-back').click();
  assert.equal(hidden(env, '#lib-calendar'), false);
  assert.equal(hidden(env, '#lib-grid-box'), true);
  assert.equal(groupRows(env, 'lib-calendar').length, 4);
  // 年の「写真を見る」
  showBtn(groupRows(env, 'lib-calendar')[3]).click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('from'), '2029-01-01 00:00');
  assert.equal(q.get('to'), '2029-12-31 23:59');
  assert.equal(env.$('#lib-sel').textContent, '2029年');
  env.$('#lib-back').click();
  // 年の行をもう一度押すと閉じる
  groupRows(env, 'lib-calendar')[0].querySelector('.lib-expand').click();
  assert.deepEqual(groupRows(env, 'lib-calendar').map(rowText), [['2030年', '30 枚'], ['2029年', '4 枚']]);
  // 日時なし
  env.$('#lib-calendar .lib-untimed .lib-trip-photos').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('untimed'), '1');
  assert.equal(q.get('from'), null);
  assert.equal(env.$('#lib-sel').textContent, '日時なし');
  // 旅へ戻すと、旅の一覧と旅の件数
  env.$('#lib-back').click();
  env.$('#lib-view-trips').click();
  assert.equal(hidden(env, '#lib-trips'), false);
  assert.equal(hidden(env, '#lib-calendar'), true);
  assert.equal(env.$('#lib-note').textContent, '旅 2 件');
});

await test('(p2) 日付: 「除外も含める」で読み直し（hidden=1）、写真が無ければ空の表示。読み込み失敗はトースト', async () => {
  const env = makeEnv();
  await env.boot();
  await openLib(env);
  env.$('#lib-view-calendar').click();
  await settle();
  const before = env.reqs('/api/admin/library/calendar').length;
  env.handlers.calendar = () => reply({ok: true, years: [{year: '2031', count: 1, months: [{month: '02', count: 1}]}], untimed: 0});
  const box = env.$('#lib-hidden');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.reqs('/api/admin/library/calendar').length, before + 1);
  assert.equal(env.last('/api/admin/library/calendar').get('hidden'), '1');
  assert.deepEqual(groupRows(env, 'lib-calendar').map(rowText), [['2031年', '1 枚']]);
  assert.equal(env.$$('#lib-calendar .lib-untimed').length, 0, '日時なし 0 枚なら行を出さない');
  // 格子を見ているときの切り替えでも、日付の一覧は読み直し、格子には hidden=1 が付く
  groupRows(env, 'lib-calendar')[0].querySelector('.lib-gphotos').click();
  await settle();
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), '1');
  env.handlers.calendar = () => reply({ok: true, years: [], untimed: 0});
  box.checked = false;
  box.dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.last('/api/admin/library/calendar').get('hidden'), null);
  assert.equal(env.last('/api/admin/library/photos').get('hidden'), null);
  env.$('#lib-back').click();
  assert.equal(env.$('#lib-calendar').textContent, '写真がありません');
  // 失敗
  env.handlers.calendar = () => respond(500, {ok: false, error: 'boom'});
  env.$('#lib-view-places').click();
  env.$('#lib-view-calendar').click();
  await settle();
  assert.ok(env.$('#toast').textContent.includes('日付の一覧を読めませんでした: boom'));
});

await test('(q) 場所: 都道府県の行を押すと市区町村の行が開き、どちらからも pref（と city）付きで格子が出る。海外・不明は abroad=1', async () => {
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
  let r = groupRows(env, 'lib-places');
  assert.deepEqual(r.map(rowText), [['架空県', '50 枚'], ['試験県', '3 枚']]);
  assert.equal(env.$('#lib-places .lib-untimed .lib-trip-range').textContent, '海外・不明（9 枚）');
  // 開く
  r[0].querySelector('.lib-expand').click();
  r = groupRows(env, 'lib-places');
  assert.deepEqual(r.map(rowText), [['架空県', '50 枚'], ['試験市', '40 枚'], ['見本町', '8 枚'], ['（市区町村なし）', '2 枚'],
                                    ['試験県', '3 枚']]);
  assert.equal(showBtn(r[3]), null, '市区町村が空の項目は city で絞れないので、写真を見るボタンを置かない');
  // 市区町村 → pref と city
  showBtn(r[2]).click();
  await settle();
  let q = env.last('/api/admin/library/photos');
  assert.equal(q.get('pref'), '架空県');
  assert.equal(q.get('city'), '見本町');
  assert.equal(q.get('from'), null);
  assert.equal(q.get('abroad'), null);
  assert.equal(env.$('#lib-sel').textContent, '架空県 見本町');
  assert.equal(hidden(env, '#lib-grid-box'), false);
  assert.equal(tiles(env).length, 3);
  env.$('#lib-back').click();
  assert.equal(hidden(env, '#lib-places'), false);
  // 都道府県 → pref だけ
  showBtn(groupRows(env, 'lib-places')[4]).click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('pref'), '試験県');
  assert.equal(q.get('city'), null);
  assert.equal(env.$('#lib-sel').textContent, '試験県');
  env.$('#lib-back').click();
  // 海外・不明
  env.$('#lib-places .lib-untimed .lib-trip-photos').click();
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('abroad'), '1');
  assert.equal(q.get('pref'), null);
  assert.equal(q.get('city'), null);
  assert.equal(q.get('untimed'), null);
  assert.equal(env.$('#lib-sel').textContent, '海外・不明');
  // 除外を含める → 格子にも場所の一覧にも hidden=1（abroad も付いたまま）
  env.$('#lib-hidden').checked = true;
  env.$('#lib-hidden').dispatchEvent({type: 'change'});
  await settle();
  q = env.last('/api/admin/library/photos');
  assert.equal(q.get('hidden'), '1');
  assert.equal(q.get('abroad'), '1');
  assert.equal(env.last('/api/admin/library/places').get('hidden'), '1');
  env.$('#lib-back').click();
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
  rows(env)[0].querySelector('.lib-trip-photos').click();
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
  // キー操作（Enter）もタイルのビューアに届かない
  btn.dispatchEvent({type: 'keydown', key: 'Enter', bubbles: true});
  btn.dispatchEvent({type: 'keydown', key: ' ', bubbles: true});
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
});

console.log(`ok (${n})`);
