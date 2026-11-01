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
  const handlers = {photos: null, points: null, trips: null};   // (params, rec) => Promise | undefined
  const fake = createFakeMaplibre();
  const fetchFake = (url, opts = {}) => {
    const rec = {url, method: opts.method || 'GET', body: opts.body};
    calls.push(rec);
    const params = url.includes('?') ? new URLSearchParams(url.split('?')[1]) : new URLSearchParams();
    if(url === '/api/admin/status') return reply(statusBody());
    if(url === '/api/ai/status') return reply({ok: true, photos: 1, eligible: 1});
    if(url.startsWith('/api/admin/photos?')){
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
  map.fire('render');
  const labels = () => env.fake.markers.filter(m => !m.removed);
  assert.deepEqual(labels().map(m => m.element.textContent), ['1.2k', '12']);
  assert.deepEqual(plain(labels().map(m => m.lngLat)), [[20, 10], [21, 11]]);
  map.fire('render');
  assert.equal(labels().length, 2, '同じクラスタを重ねて作らない');
  env.fake.features = [env.fake.features[2]];
  map.fire('render');
  assert.deepEqual(labels().map(m => m.element.textContent), ['12'], '見えなくなったクラスタの文字は外す');
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

console.log(`ok (${n})`);
