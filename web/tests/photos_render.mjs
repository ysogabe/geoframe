// admin.js と photos.js（この順で同じ context に読む）の写真タブ — チップ・一覧・検索（AI 検索の場面）・
// 統一カード・ビューア・フラグ操作・位置指定モーダル・要対応 — を、fakedom.mjs の DOM の代用品と
// fetch の代用品で動かして確かめる。
// 使い方: node web/tests/photos_render.mjs <admin.js のパス> <photos.js のパス>
// fetch は URL ごとの合成 JSON を返すだけで、ネットワークには出ない。
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom, htmlFacts, domFacts, CHIPS} from './fakedom.mjs';

const jsPath = process.argv[2];
const photosPath = process.argv[3];
assert.ok(jsPath, 'admin.js のパスが要る');
assert.ok(photosPath, 'photos.js のパスが要る');
const htmlPath = path.join(path.dirname(jsPath), '..', 'admin.html');
const source = fs.readFileSync(jsPath, 'utf8');
const photosSource = fs.readFileSync(photosPath, 'utf8');
const loadScripts = context => {
  vm.runInContext(source, context, {filename: 'admin.js'});
  vm.runInContext(photosSource, context, {filename: 'photos.js'});
};

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const respond = (status, body) =>
  Promise.resolve({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body)});
const reply = body => respond(200, body);

const FILTERS = {all: 6540, noloc: 123, suggest: 12, inferred: 410, manual: 88, fav: 57, hidden: 1031};
const FILTERS_TEXT = {all: '6,540', noloc: '123', suggest: '12', inferred: '410', manual: '88', fav: '57', hidden: '1,031'};

function statusBody(filters){
  return {
    ok: true, server_time: '2026-10-31 10:00:00',
    sync: {ready: 6500, pending: 0, total: 6540},
    db: {no_loc: 123, no_display: 0, favorite: filters.fav, hidden: filters.hidden, enrich_pending: 0,
         no_time: 0, by_loc_source: {exif: 6000}, infer_buckets: {}, by_source: {drive: 6540}, filters},
    disk: {used_pct: 40, free_bytes: 1e11, total_bytes: 2e11,
           dirs: {derivatives: {bytes: 1e9, files: 100}, tile_cache: {bytes: 1e6, files: 10}}},
  };
}

// 位置あり+EXIF / 推定+bucket / 位置なし+候補(+ヒント) / 位置なし+ヒント / 位置なし+手がかりなし(除外済み)
const base = {name: 'x.jpg', thumb: '/derivatives/x.jpg', taken_at: '2026-10-01 12:00:00', lat: null, lon: null,
              loc_source: 'none', infer_bucket: '', place: '', pref: '', city: '', fav: false, hidden: false,
              rotation: 0, source: 'drive'};
const PATTERNS = [
  {...base, id: 1, name: 'a.jpg', lat: 35.6812, lon: 139.7671, loc_source: 'exif', infer_bucket: '60分以内',
   pref: '東京都', city: '千代田区', place: '丸の内'},
  {...base, id: 2, name: 'b.jpg', lat: 34.5, lon: 135.25, loc_source: 'inferred', infer_bucket: '60分以内',
   source: 'local'},
  {...base, id: 3, name: 'c.jpg', suggest: {place_name: '京都駅'},
   hint: {label: '大阪', dt_s: 5400, lat: 34.7, lon: 135.5}},
  {...base, id: 4, name: 'd.jpg', hint: {label: '奈良市', dt_s: 7200, lat: 34.7, lon: 135.8}},
  {...base, id: 5, name: 'e.jpg', taken_at: '', thumb: '', hint: null, hidden: true},
];

// ---- 検索結果の合成データ（T4 のあとの GET /api/ai/search の写真の形。一覧の fav は favorite） ----
const sp = o => ({name: 's.jpg', thumb: '/derivatives/s.jpg', taken_at: '2026-10-02 09:00:00', lat: null, lon: null,
  place: '', pref: '', city: '', favorite: false, hidden: false, source: 'drive', rotation: 0, loc_source: '',
  infer_dt_s: null, infer_bucket: '', similarity: 0.5, recommended: null, subject: '', eyes: '', pet_state: '',
  face_crop: '', horizon_status: '', horizon_score: null, subject_sharpness: null, aesthetic_score: null,
  keep_score: null, reasons: [], concerns: [], quality_group_kind: '', visual_cluster: 1, visual_similarity: 1,
  visual_representative: true, visual_cluster_size: 1, ...o});
const here = {lat: 35.6812, lon: 139.7671, loc_source: 'exif', pref: '東京都', city: '千代田区', place: '丸の内'};
const S = {
  rep: sp({id: 101, name: 'dog1.jpg', ...here, similarity: 0.91, recommended: true, subject: 'dog', keep_score: 80,
           eyes: 'open', reasons: ['表情が良い'], concerns: ['少し暗い'],
           visual_cluster: 1, visual_cluster_size: 2}),
  twin: sp({id: 102, name: 'dog2.jpg', ...here, similarity: 0.88, recommended: false, subject: 'dog', keep_score: 60,
            visual_cluster: 1, visual_similarity: 0.97, visual_representative: false, visual_cluster_size: 2}),
  sugg: sp({id: 103, name: 'view.jpg', similarity: 0.7, subject: 'landscape', visual_cluster: 2,
            suggest: {place_name: '京都駅', lat: 34.98, lon: 135.76, precision: 'landmark', scope: 'photo',
                      evidence: ['駅舎が写っている'], model: 'm'},
            hint: {label: '大阪', dt_s: 5400, lat: 34.7, lon: 135.5, taken_at: '2026-10-02 08:00:00'}}),
  hint: sp({id: 201, name: 'cat.jpg', source: 'local', similarity: 0.8, subject: 'cat', visual_cluster: 1,
            hint: {label: '奈良市', dt_s: 7200, lat: 34.7, lon: 135.8, taken_at: '2026-10-02 07:00:00'}}),
  inf: sp({id: 202, name: 'inf.jpg', lat: 34.5, lon: 135.25, loc_source: 'inferred', infer_bucket: '60分以内',
           infer_dt_s: 1200, similarity: 0.6, visual_cluster: 2}),
};
const G1 = {group_id: 'g1', kind: 'scene', summary: '丸の内の散歩', min_similarity: 0.7, max_similarity: 0.91,
            avg_similarity: 0.8, best_similarity: 0.91, visual_cluster_count: 2, collapsed_photo_count: 1,
            photos: [S.twin, S.rep, S.sugg]};
const G2 = {group_id: 'g2', kind: 'pet', summary: '', min_similarity: null, max_similarity: null,
            avg_similarity: null, best_similarity: 0.8, visual_cluster_count: 2, collapsed_photo_count: 0,
            photos: [S.hint, S.inf]};
const searchBody = (q, groups = [G1, G2], mode = 'semantic') =>
  JSON.parse(JSON.stringify({ok: true, query: q, mode, groups, searched_photos: 6500, include_hidden: false}));

// 1 回の試験の環境: DOM・fetch・vm の context。calls に fetch の記録が残る。
function makeEnv({pathname, photos = PATTERNS, hold = false, aiStatus = true}){
  const gate = {holdStatus: false};   // true の間の /api/admin/status は、返答を held の関数で手動にする
  const document = buildAdminDom();
  const filters = {...FILTERS};
  const calls = [];
  const held = [];
  const heldStatus = [];
  const heldSearch = [];
  const flags = {};
  const maplibre = {maps: [], markers: []};
  const timers = {immediate: false};  // true の間、短い setTimeout（モーダルの resize 等）はすぐ実行する
  const handlers = {search: null, rotateUrl: null};    // 検索の応答を差し替える: (url, rec) => Promise
  const fetchFake = (url, opts = {}) => {
    const rec = {url, method: opts.method || 'GET', headers: opts.headers, body: opts.body};
    calls.push(rec);
    if(calls.length > 5000) throw new Error('fetch が 5000 回を超えた（読み直しのループ）: ' + url);   // settle() は setImmediate で待つだけ
    if(url === '/api/admin/status'){
      const body = statusBody({...filters});     // 要求した時点のサーバーの状態
      if(!gate.holdStatus) return reply(body);
      // 手動で返す。how: 'ok'（既定）| 'reject'（通信失敗）| 'notok'（ok:false の応答）
      return new Promise((resolve, reject) => heldStatus.push((how = 'ok') => {
        if(how === 'reject') reject(new Error('net down'));
        else if(how === 'notok') resolve({ok: true, status: 200, json: () => Promise.resolve({ok: false, error: 'boom'})});
        else resolve({ok: true, status: 200, json: () => Promise.resolve(body)});
      }));
    }
    if(url === '/api/ai/status'){
      return aiStatus ? reply({ok: true, photos: 6500, eligible: 6540, judged: 100, groups: 20, pipeline: {}})
                      : respond(503, {ok: false, error: 'AI検索サービスへ接続できません。しばらくして再試行してください'});
    }
    if(url.startsWith('/api/ai/search?')){
      if(handlers.search) return handlers.search(url, rec);
      return reply(searchBody(new URLSearchParams(url.split('?')[1]).get('q')));
    }
    if(url.startsWith('/api/admin/photos?')){
      const q = new URLSearchParams(url.split('?')[1]);
      const body = {ok: true, page: Number(q.get('page')), per: 50, pages: 3, total: photos.length,
                    filter: q.get('filter'), items: photos};
      if(!hold) return reply(body);
      return new Promise(resolve => held.push(() => resolve({ok: true, status: 200, json: () => Promise.resolve(body)})));
    }
    if(url === '/api/admin/setflag'){
      const b = JSON.parse(opts.body);
      const cur = flags[b.id] = flags[b.id] || {favorite: false, hidden: false};
      if(b.favorite !== undefined){ cur.favorite = b.favorite; filters.fav += b.favorite ? 1 : -1; }
      if(b.hidden !== undefined) cur.hidden = b.hidden;
      return reply({ok: true, favorite: cur.favorite, hidden: cur.hidden});
    }
    if(url === '/api/admin/rotate'){
      const b = JSON.parse(opts.body);
      const rotation = ((flags['r' + b.id] || 0) + b.delta) % 360;
      flags['r' + b.id] = rotation;
      const body = {ok: true, rotation, thumb: '/derivatives/rot' + rotation + '.jpg'};
      if(handlers.rotateUrl) body.thumb_url = handlers.rotateUrl(b.id, rotation);   // 版付きの小さな画像（省略可）
      return reply(body);
    }
    if(url === '/api/admin/setloc'){
      const b = JSON.parse(opts.body);
      return reply({ok: true, id: b.id, lat: b.lat, lon: b.lon, loc_source: 'manual'});
    }
    if(url.startsWith('/api/admin/geocode?')){
      return reply({ok: true, items: [{title: 'Times Square', lat: 40.758, lon: -73.9855, source: 'osm'}]});
    }
    return reply({ok: false, error: 'unexpected ' + url});
  };
  // MapLibre の代用品: ensureMini() が落ちず、作られた地図・マーカーが数えられ、地図の click を起こせる程度
  const maplibregl = {
    Map: class { constructor(opts){ this.opts = opts; this.handlers = {}; maplibre.maps.push(this); }
                 addControl(){} on(type, fn){ this.handlers[type] = fn; } resize(){} jumpTo(){} flyTo(){} },
    NavigationControl: class {},
    Marker: class { constructor(){ maplibre.markers.push(this); }
                    setLngLat(){ return this; } addTo(){ return this; } remove(){}
                    getElement(){ return {}; } },
  };
  const context = vm.createContext({
    document, fetch: fetchFake, location: {pathname, origin: 'http://localhost'},
    setInterval(){}, setTimeout(fn, ms){ if(timers.immediate && ms <= 50) fn(); return 0; },
    clearTimeout(){}, console, maplibregl,
  });
  loadScripts(context);
  const photoCalls = () => calls.filter(c => c.url.startsWith('/api/admin/photos?'));
  const statusCalls = () => calls.filter(c => c.url === '/api/admin/status');
  const searchCalls = () => calls.filter(c => c.url.startsWith('/api/ai/search?'));
  const aiStatusCalls = () => calls.filter(c => c.url === '/api/ai/status');
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  // vm の中の値を JSON 経由で取り出す（別の realm の配列と deepEqual しないため）
  const ev = code => JSON.parse(vm.runInContext('JSON.stringify(' + code + ')', context));
  return {document, context, calls, held, heldStatus, heldSearch, gate, handlers, timers, maplibre, photoCalls,
          statusCalls, searchCalls, aiStatusCalls, boot, ev, $: s => document.querySelector(s)};
}
const query = url => new URLSearchParams(url.split('?')[1]);
const chipOf = (env, f) => env.$(`.chip[data-filter=${f}]`);
const onChips = env => env.document.querySelectorAll('.chip').filter(c => c.classList.contains('on'))
  .map(c => c.dataset.filter);
const activeTab = env => env.document.querySelectorAll('nav .tab').filter(t => t.classList.contains('on'))
  .map(t => t.dataset.tab);
const cards = env => env.document.querySelectorAll('#cards .card');
const gcards = env => env.document.querySelectorAll('#groups .card');
const hidden = (env, sel) => env.$(sel).classList.contains('hidden');
// ノードの同一性は assert.ok で比べる。assert.equal / strictEqual にノードを渡すと、失敗時の差分作りが文書全体を inspect して
// 時間とメモリを大きく食う（fakedom.mjs の hidden の注記を参照）
const same = (a, b, msg) => assert.ok(a === b, msg || '同じノードのはず');
const notSame = (a, b, msg) => assert.ok(a !== b, msg || '別のノードのはず');

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('admin.js と photos.js を読み込んだだけでは location にも fetch にも触らない', async () => {
  const touched = [];
  const sandbox = {document: buildAdminDom(), setInterval(){}, setTimeout(){}, clearTimeout(){}, console};
  Object.defineProperty(sandbox, 'location', {get(){ touched.push('location'); throw new Error('location'); }});
  Object.defineProperty(sandbox, 'fetch', {get(){ touched.push('fetch'); throw new Error('fetch'); }});
  loadScripts(vm.createContext(sandbox));
  assert.deepEqual(touched, []);
});

await test('admin.html と fakedom の骨格（id・タブ・チップ）が一致している', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const want = htmlFacts(html);
  const have = domFacts(buildAdminDom());
  assert.deepEqual(have.ids, want.ids, 'id の集合');
  assert.deepEqual(have.tabs, ['status', 'photos', 'library', 'curation', 'config']);
  assert.deepEqual(want.tabs, have.tabs, 'data-tab');
  assert.deepEqual(want.filters, CHIPS.map(c => c[0]), 'data-filter');
  assert.deepEqual(have.filters, want.filters);
  // チップの表示名も admin.html と同じ（"すべて <span class="n">" の形）
  CHIPS.forEach(([f, label]) => {
    assert.ok(html.includes(`data-filter="${f}">${label} <span class="n"></span>`), `${f} の表示名 ${label}`);
  });
  assert.ok(!/href="[^"]*blog-admin/.test(html), '旅行ブログのリンクが無い');
  // 検索バー・ビューア・読み込み順
  assert.ok(/<input type="text" id="q"/.test(html) && !/id="q"[^>]*maxlength/.test(html),
    '#q に maxlength は付けない（UTF-16 単位で API のコードポイント数とずれる。長さは search() で数える）');
  assert.ok(html.includes('id="viewer"') && !html.includes('photo-modal'), 'ビューアは #viewer');
  assert.ok(!html.includes('href="/ai-search"') && !html.includes('AI 写真検索 ▸'), '旧ページへのリンクは無い');
  const scripts = [...html.matchAll(/<script src="([^"]+)"/g)].map(m => m[1]).filter(u => u.startsWith('/static/'));
  assert.deepEqual(scripts, ['/static/js/admin.js', '/static/js/photos.js', '/static/js/library.js', '/static/js/curation.js', '/static/js/config.js'],
    'admin.js → photos.js → library.js → curation.js → config.js の順');
});

await test('画面の JS に HTML 文字列を流し込む API が 1 つも無い', () => {
  for(const [name, text] of [['admin.js', source], ['photos.js', photosSource]]){
    assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(text), name);
  }
  // 末尾の起動は admin.js の DOMContentLoaded だけ（photos.js は定義だけ）
  assert.ok(/document\.addEventListener\('DOMContentLoaded', boot\);\s*$/.test(source));
  assert.ok(!photosSource.includes('DOMContentLoaded'));
});

await test('(a) boot() で /admin と / とパスの揺れでは状況タブが選ばれる', async () => {
  for(const pathname of ['/admin', '/', '//admin/', '/admin/', '/blog-admin']){
    const env = makeEnv({pathname});
    await env.boot();
    assert.deepEqual(activeTab(env), ['status'], pathname);
    assert.equal(hidden(env, '#v-status'), false, pathname);
    assert.equal(hidden(env, '#v-photos'), true, pathname);
    assert.equal(env.statusCalls().length, 1, pathname);
    assert.equal(env.photoCalls().length, 0, pathname);
    assert.equal(env.$('#servertime').textContent, 'SERVER 2026-10-31 10:00:00');
  }
  const ctx = makeEnv({pathname: '/admin'}).context;
  assert.equal(vm.runInContext('initialTab()', ctx), 'status');
  assert.equal(vm.runInContext("(() => { location.pathname = '///ai-search/'; return initialTab(); })()", ctx),
               'photos');
  assert.equal(vm.runInContext("(() => { location.pathname = '/ai-search/x'; return initialTab(); })()", ctx),
               'status');
  // location が無い環境（既存の描画試験）では状況
  const bare = vm.createContext({document: buildAdminDom(), setInterval(){}, console});
  loadScripts(bare);
  assert.equal(vm.runInContext('initialTab()', bare), 'status');
});

await test('(b) 写真タブを開くとチップ 7 種の件数が db.filters と一致する', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  assert.deepEqual(activeTab(env), ['photos']);
  assert.equal(hidden(env, '#v-photos'), false);
  assert.equal(hidden(env, '#v-status'), true);
  const chips = env.document.querySelectorAll('.chip');
  assert.deepEqual(chips.map(c => c.dataset.filter), CHIPS.map(c => c[0]));
  chips.forEach(c => {
    assert.equal(c.querySelector('.n').textContent, FILTERS_TEXT[c.dataset.filter], c.dataset.filter);
  });
  assert.deepEqual(onChips(env), ['all']);
  assert.equal(env.statusCalls().length, 2);                 // 起動時の状況 + 写真タブの件数
  assert.equal(query(env.photoCalls()[0].url).get('filter'), 'all');
  assert.equal(cards(env).length, PATTERNS.length);
  assert.equal(env.$('#pageinfo').textContent, '5件 / 1 of 3ページ');
  assert.equal(env.$('#prev').disabled, true);
  assert.equal(env.$('#next').disabled, false);
});

await test('(c) チップを押すと filter= が変わり page=1 になり、on が移る', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  env.$('#next').click();                                    // 2 ページ目へ
  await settle();
  assert.equal(query(env.photoCalls().at(-1).url).get('page'), '2');
  chipOf(env, 'noloc').click();
  await settle();
  let q = query(env.photoCalls().at(-1).url);
  assert.equal(q.get('filter'), 'noloc');
  assert.equal(q.get('page'), '1');
  assert.deepEqual(onChips(env), ['noloc']);
  chipOf(env, 'suggest').click();
  await settle();
  q = query(env.photoCalls().at(-1).url);
  assert.equal(q.get('filter'), 'suggest');
  assert.equal(q.get('page'), '1');
  assert.deepEqual(onChips(env), ['suggest']);
  // 状況タブへ戻って写真タブを開き直すと、直前のフィルタのまま
  env.$('.tab[data-tab=status]').click();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  assert.equal(query(env.photoCalls().at(-1).url).get('filter'), 'suggest');
  assert.deepEqual(onChips(env), ['suggest']);
});

await test('(c2) 速く切り替えたとき、遅れて届いた古い応答で一覧を上書きしない', async () => {
  const env = makeEnv({pathname: '/admin', hold: true});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  chipOf(env, 'noloc').click();
  chipOf(env, 'fav').click();
  await settle();
  assert.equal(env.held.length, 3);                          // all, noloc, fav の順に未返答
  env.held[2]();                                             // 最後の要求から先に返す
  await settle();
  assert.equal(cards(env).length, PATTERNS.length);
  const before = cards(env);
  env.held[0](); env.held[1]();
  await settle();
  assert.ok(cards(env).length === before.length && cards(env).every((c, i) => c === before[i]),
            '古い応答は捨てられ、カードの要素は同じまま');
});

await test('(d) カードの 1 行目・タグ・位置ボタン・回転ボタンが規則どおり', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const cs = cards(env);
  const part = (c, sel) => c.querySelector(sel);
  const classes = c => part(c, '.l1').className.split(/\s+/).sort().join(' ');
  const rows = cs.map(c => ({
    id: c.dataset.id,
    l1: part(c, '.l1').textContent, l1cls: classes(c),
    dt: part(c, '.l2').children[0].textContent, nm: part(c, '.l2').children[1].textContent,
    tag: part(c, '.tag').textContent, tagcls: part(c, '.tag').className,
    setloc: part(c, '.acts .setloc').textContent, strong: part(c, '.acts .setloc').classList.contains('strong'),
    rotate: part(c, '.acts .rotate') !== null,
    fav: part(c, '.fav').classList.contains('on'), hid: part(c, '.hid').classList.contains('on'),
    faded: c.classList.contains('is-hidden'),
  }));
  assert.deepEqual(rows, [
    // 位置あり+EXIF: 地名。infer_bucket が付いていてもタグには出さない。Drive で画像あり → 回転できる
    {id: '1', l1: '東京都 千代田区 丸の内', l1cls: 'l1', dt: '2026-10-01 12:00:00', nm: 'a.jpg',
     tag: 'EXIF', tagcls: 'tag t-exif', setloc: '位置', strong: false, rotate: true,
     fav: false, hid: false, faded: false},
    // 推定: 地名が無いので座標 4 桁。bucket 付き。source が drive でないので回転できない
    {id: '2', l1: '34.5000, 135.2500', l1cls: 'l1', dt: '2026-10-01 12:00:00', nm: 'b.jpg',
     tag: '推定 60分以内', tagcls: 'tag t-inferred', setloc: '位置', strong: false, rotate: false,
     fav: false, hid: false, faded: false},
    // 位置なし+候補(ヒントもある): 候補が優先
    {id: '3', l1: 'AI候補: 京都駅', l1cls: 'act l1', dt: '2026-10-01 12:00:00', nm: 'c.jpg',
     tag: '位置なし', tagcls: 'tag t-none', setloc: '位置を付ける', strong: true, rotate: true,
     fav: false, hid: false, faded: false},
    // 位置なし+ヒント
    {id: '4', l1: '近い時刻の位置: 奈良市（2.0時間差）', l1cls: 'act hint l1', dt: '2026-10-01 12:00:00',
     nm: 'd.jpg', tag: '位置なし', tagcls: 'tag t-none', setloc: '位置を付ける', strong: true, rotate: true,
     fav: false, hid: false, faded: false},
    // 位置なし+手がかりなし+撮影時刻なし+画像なし+除外済み
    {id: '5', l1: '手がかりなし', l1cls: 'act l1 none', dt: '撮影時刻なし', nm: 'e.jpg',
     tag: '位置なし', tagcls: 'tag t-none', setloc: '位置を付ける', strong: true, rotate: false,
     fav: false, hid: true, faded: true},
  ]);
  // 位置なしのカードには位置の行（地名）が出ない: 取り消し後に place が残っている行
  const stale = makeEnv({pathname: '/admin', photos: [{...base, id: 9, place: '残った地名', pref: '残県'}]});
  await stale.boot();
  stale.$('.tab[data-tab=photos]').click();
  await settle();
  assert.equal(cards(stale)[0].querySelector('.l1').textContent, '手がかりなし');
  assert.ok(!cards(stale)[0].textContent.includes('残った地名'));
});

await test('(e) 同じ id のカード 2 枚: ★ で POST し、応答で 2 枚とも更新され、件数が読み直される', async () => {
  const dup = {...base, id: 7, name: 'dup.jpg', lat: 35.0, lon: 135.0, loc_source: 'exif'};
  const env = makeEnv({pathname: '/admin', photos: [dup, {...dup}, {...base, id: 8, name: 'other.jpg'}]});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const [c1, c2, c3] = cards(env);
  assert.equal(c1.dataset.id, '7');
  assert.equal(c2.dataset.id, '7');
  const statusBefore = env.statusCalls().length;
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '57');

  c1.querySelector('.fav').click();
  await settle();
  const posts = env.calls.filter(c => c.method === 'POST');
  assert.equal(posts.length, 1);
  assert.equal(posts[0].url, '/api/admin/setflag');
  assert.equal(posts[0].body, '{"id":7,"favorite":true}');
  assert.equal(posts[0].headers['Content-Type'], 'application/json');
  assert.ok(c1.querySelector('.fav').classList.contains('on'));
  assert.ok(c2.querySelector('.fav').classList.contains('on'), '同じ id の 2 枚目も更新される');
  assert.ok(!c3.querySelector('.fav').classList.contains('on'), '別の写真は変わらない');
  assert.equal(env.statusCalls().length, statusBefore + 1, 'loadFilterCounts() が呼ばれる');
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58');

  // 2 枚目から押すと、画面の状態（on）を基準に外す要求になる
  c2.querySelector('.fav').click();
  await settle();
  assert.equal(env.calls.filter(c => c.method === 'POST').at(-1).body, '{"id":7,"favorite":false}');
  assert.ok(!c1.querySelector('.fav').classList.contains('on'));
  assert.ok(!c2.querySelector('.fav').classList.contains('on'));
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '57');

  // 除外: 2 枚とも薄く（is-hidden）なり、🚫 が on になる
  c2.querySelector('.hid').click();
  await settle();
  assert.equal(env.calls.filter(c => c.method === 'POST').at(-1).body, '{"id":7,"hidden":true}');
  [c1, c2].forEach(c => {
    assert.ok(c.classList.contains('is-hidden'));
    assert.ok(c.querySelector('.hid').classList.contains('on'));
  });
  assert.ok(!c3.classList.contains('is-hidden'));
});

await test('(e2) 件数取得が 2 回重なっても、最新の要求の件数だけがチップに出る', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.gate.holdStatus = true;
  env.$('.tab[data-tab=photos]').click();                    // 件数取得 A（fav=57 の時点）が未返答
  await settle();
  assert.equal(env.heldStatus.length, 1);
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '', 'A はまだ届いていない');
  cards(env)[0].querySelector('.fav').click();               // ★ → サーバーは fav=58 → 件数取得 B
  await settle();
  assert.equal(env.heldStatus.length, 2);
  env.heldStatus[1]();                                       // B が先に届く
  await settle();
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58');
  env.heldStatus[0]();                                       // 遅れて A（57）が届いても
  await settle();
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58', '古い件数で戻らない');
  assert.equal(env.$('#toast').className, 'show', 'エラーのトーストも出ない');
});

await test('(e3) 回転ボタン: POST /api/admin/rotate {id, delta:90} → 応答の thumb・rotation でカードが更新される', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const card = cards(env)[0];
  const rot = card.querySelector('.rotate');
  assert.equal(card.querySelector('.ph img').src, '/derivatives/x.jpg');
  rot.click();
  assert.equal(rot.disabled, true, '応答を待つあいだは押せない');
  rot.click();                                               // disabled なので 2 回目は届かない
  await settle();
  const posts = env.calls.filter(c => c.url === '/api/admin/rotate');
  assert.equal(posts.length, 1);
  assert.equal(posts[0].method, 'POST');
  assert.equal(posts[0].body, '{"id":1,"delta":90}');
  assert.equal(posts[0].headers['Content-Type'], 'application/json');
  assert.equal(card.querySelector('.ph img').src, '/derivatives/rot90.jpg');
  assert.equal(rot.title, '時計回りに90°回して表示（今 90°）');
  assert.equal(rot.disabled, false);
  assert.ok(env.$('#toast').textContent.includes('90°'));
  // 続けてもう一度
  rot.click();
  await settle();
  assert.equal(card.querySelector('.ph img').src, '/derivatives/rot180.jpg');
  assert.equal(rot.title, '時計回りに90°回して表示（今 180°）');
});

await test('(e4) カードの画像は thumb_url（無ければ thumb）。回転で thumb_url が変わると src も変わり、ビューアは thumb のまま', async () => {
  const withUrl = {...base, id: 1, name: 'a.jpg', thumb: '/derivatives/a.jpg', thumb_url: '/thumb/1.jpg?v=aaaaaaaaaaaa'};
  const noUrl = {...base, id: 2, name: 'b.jpg', thumb: '/derivatives/b.jpg', thumb_url: ''};
  const env = makeEnv({pathname: '/admin', photos: [withUrl, noUrl]});
  env.handlers.rotateUrl = (id, rotation) => '/thumb/' + id + '.jpg?v=rot' + String(rotation).padStart(9, '0');
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  const [c1, c2] = cards(env);
  assert.equal(c1.querySelector('.ph img').src, '/thumb/1.jpg?v=aaaaaaaaaaaa', '(a) thumb_url を使う');
  assert.equal(c2.querySelector('.ph img').src, '/derivatives/b.jpg', '(b) thumb_url が空なら thumb');
  // (d) ビューアの拡大画像は表示用画像（thumb）のまま
  c1.querySelector('.ph').click();
  assert.equal(env.$('#viewer-image').src, '/derivatives/a.jpg');
  env.$('#viewer-close').click();
  // (c) 回転の応答の thumb_url でカードの src が新しい値になる
  c1.querySelector('.rotate').click();
  await settle();
  assert.equal(c1.querySelector('.ph img').src, '/thumb/1.jpg?v=rot000000090');
  // thumb_url の無い応答（古いサーバー）なら thumb に戻る
  env.handlers.rotateUrl = null;
  c1.querySelector('.rotate').click();
  await settle();
  assert.equal(c1.querySelector('.ph img').src, '/derivatives/rot180.jpg');
});

await test('(g) 位置ボタン: モーダルが開き、#m-title に id と名前が入り、地図が 1 つだけ作られる', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  env.$('.tab[data-tab=photos]').click();
  await settle();
  assert.equal(hidden(env, '#modal'), true);
  cards(env)[2].querySelector('.setloc').click();            // id 3: 位置なし + 候補 + ヒント
  assert.equal(hidden(env, '#modal'), false);
  assert.equal(env.$('#m-title').textContent, '#3  c.jpg  /  2026-10-01 12:00:00');
  assert.equal(env.$('#m-save').disabled, true);
  assert.equal(hidden(env, '#m-suggest'), false);
  assert.ok(env.$('#m-hint').textContent.includes('大阪'));
  assert.equal(env.maplibre.maps.length, 1);
  assert.equal(env.maplibre.maps[0].opts.container, 'minimap');
  env.$('#m-close').click();
  assert.equal(hidden(env, '#modal'), true);
  cards(env)[0].querySelector('.setloc').click();            // id 1: 位置あり。地図は作り直さない
  assert.equal(env.$('#m-title').textContent, '#1  a.jpg  /  2026-10-01 12:00:00');
  assert.equal(hidden(env, '#m-suggest'), true);
  assert.equal(env.maplibre.maps.length, 1);
});

await test('(f) 要対応のセルを押すと写真タブとそのチップが選ばれ、一覧は 1 回だけ読まれる', async () => {
  for(const [idx, f, count] of [[0, 'noloc', '123'], [1, 'suggest', '12']]){
    const env = makeEnv({pathname: '/admin'});
    await env.boot();
    const cells = env.$('#s-todo').children;
    assert.equal(cells.length, 2);
    assert.ok(cells[idx].textContent.includes(count), cells[idx].textContent);
    cells[idx].click();
    await settle();
    assert.deepEqual(activeTab(env), ['photos']);
    assert.equal(hidden(env, '#v-photos'), false);
    assert.equal(hidden(env, '#v-status'), true);
    assert.deepEqual(onChips(env), [f]);
    assert.equal(env.photoCalls().length, 1);
    const q = query(env.photoCalls()[0].url);
    assert.equal(q.get('filter'), f);
    assert.equal(q.get('page'), '1');
    assert.equal(chipOf(env, f).querySelector('.n').textContent, FILTERS_TEXT[f]);
  }
  // db.filters が無い応答（古いサーバー）では要対応のセルを出さない
  const env = makeEnv({pathname: '/admin'});
  env.context.fetch = url => reply((() => { const b = statusBody(FILTERS); delete b.db.filters; return b; })());
  await env.boot();
  assert.equal(env.$('#s-todo').children.length, 0);
});

await test('(f2) 状況タブの「未確認の選別」（db.quality.curation_pending）: 要対応と品質の欄に出て、押すと選別タブへ移る。無ければ出さない', async () => {
  const withQuality = pending => {
    const b = statusBody(FILTERS);
    b.db.quality = {features: 5, pending: 0, analyzed: 4, recommended: 2, exact_suppressed: 1, similar_alternates: 1};
    if(pending !== undefined) b.db.quality.curation_pending = pending;
    return b;
  };
  const textOf = n => n.textContent;
  // 件数があるとき: 要対応（押せる）と品質の欄（warn）に出る。既存の要対応 2 つはそのまま先に並ぶ
  const env = makeEnv({pathname: '/admin'});
  const orig = env.context.fetch;
  env.context.fetch = (url, opts) => url === '/api/admin/status' ? reply(withQuality(12)) : orig(url, opts);
  await env.boot();
  const todo = env.$('#s-todo').children;
  assert.equal(todo.length, 3);
  assert.ok(textOf(todo[0]).includes('位置なし') && textOf(todo[1]).includes('AI候補あり'));
  assert.equal(textOf(todo[2]), '未確認の選別12組');
  assert.ok(todo[2].classList.contains('warn') && todo[2].classList.contains('todo'));
  assert.equal(todo[2].getAttribute('role'), 'button');
  const dbCell = env.$('#s-db').children.find(c => textOf(c).includes('未確認の選別'));
  assert.equal(textOf(dbCell), '未確認の選別12組');
  assert.ok(dbCell.classList.contains('warn'));
  // キーボード（Enter）でも移れる。選別タブの JS（curation.js）が無くても、タブは切り替わる
  const enter = {type: 'keydown', key: 'Enter'};
  todo[2].dispatchEvent(enter);
  await settle();
  assert.deepEqual(activeTab(env), ['curation']);
  assert.equal(hidden(env, '#v-curation'), false);
  assert.equal(hidden(env, '#v-status'), true);
  assert.equal(enter.defaultPrevented, true);
  // 0 件は good。押しても移れる
  const env0 = makeEnv({pathname: '/admin'});
  const orig0 = env0.context.fetch;
  env0.context.fetch = (url, opts) => url === '/api/admin/status' ? reply(withQuality(0)) : orig0(url, opts);
  await env0.boot();
  const zero = env0.$('#s-todo').children[2];
  assert.equal(textOf(zero), '未確認の選別0組');
  assert.ok(zero.classList.contains('good') && !zero.classList.contains('warn'));
  zero.click();
  await settle();
  assert.deepEqual(activeTab(env0), ['curation']);
  // curation_pending が無い応答（Python 版・古いサーバー・quality なし）では出さない
  for(const body of [withQuality(undefined), statusBody(FILTERS)]){
    const e = makeEnv({pathname: '/admin'});
    const o = e.context.fetch;
    e.context.fetch = (url, opts) => url === '/api/admin/status' ? reply(body) : o(url, opts);
    await e.boot();
    assert.equal(e.$('#s-todo').children.length, 2);
    assert.ok(!e.$('#s-db').children.some(c => textOf(c).includes('未確認の選別')));
  }
});

// ============ ここから: 検索・統一カード・ビューア・位置の保存 ============
const openPhotos = async env => { env.$('.tab[data-tab=photos]').click(); await settle(); };
const searchFor = async (env, q) => { env.$('#q').value = q; env.$('#search').click(); await settle(); };
const note = env => env.$('#search-note').textContent;

await test('(h) /ai-search で boot() すると写真タブが選ばれ #q にフォーカスがあり、索引の枚数が薄く出る', async () => {
  const env = makeEnv({pathname: '/ai-search'});
  await env.boot();
  assert.deepEqual(activeTab(env), ['photos']);
  assert.equal(hidden(env, '#v-photos'), false);
  assert.equal(hidden(env, '#v-status'), true);
  same(env.document.activeElement, env.$('#q'));
  assert.equal(env.photoCalls().length, 1);
  assert.equal(note(env), '索引 6500/6540 枚');
  assert.ok(env.$('#search-note').classList.contains('dim'));
  // /admin では状況タブで、フォーカスは #q にない。写真タブを開いたとき 1 回だけ /api/ai/status を読む
  const adm = makeEnv({pathname: '/admin'});
  await adm.boot();
  assert.deepEqual(activeTab(adm), ['status']);
  notSame(adm.document.activeElement, adm.$('#q'));
  assert.equal(adm.aiStatusCalls().length, 0);
  await openPhotos(adm);
  assert.equal(note(adm), '索引 6500/6540 枚');
  adm.$('.tab[data-tab=status]').click();
  await openPhotos(adm);
  assert.equal(adm.aiStatusCalls().length, 1, '開き直しでは読まない');
  // 索引の状態が読めなければ何も出さない
  const bad = makeEnv({pathname: '/admin', aiStatus: false});
  await bad.boot();
  await openPhotos(bad);
  assert.equal(note(bad), '');
});

await test('(i) 検索の成功: view=search になり、場面ごとのセクションと統一カード（1 行目・タグ・MATCH・AI PICK）が規則どおり', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  chipOf(env, 'noloc').click();                              // 検索の前の filter・page を覚えておく
  await settle();
  await searchFor(env, '海辺を走っている犬');
  const url = env.searchCalls().at(-1).url;
  assert.equal(url, '/api/ai/search?q=' + encodeURIComponent('海辺を走っている犬') + '&groups=12');
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(env.ev('ps.filter'), 'noloc');
  assert.equal(env.ev('ps.query'), '海辺を走っている犬');
  assert.equal(hidden(env, '#groups'), false);
  assert.equal(hidden(env, '#cards'), true);
  assert.equal(hidden(env, '#chips'), true);
  assert.equal(hidden(env, '#pager'), true);
  assert.equal(hidden(env, '#close-search'), false);
  assert.equal(hidden(env, '#empty'), true);
  assert.ok(note(env).includes('6500枚を検索 / 5枚 → 4構図'), note(env));
  assert.ok(note(env).includes('索引 6500/6540 枚'));

  const groups = env.document.querySelectorAll('#groups section.ps-group');
  assert.equal(groups.length, 2);
  assert.equal(groups[0].querySelector('.ps-kind').textContent, '01 / SAME SCENE');
  assert.equal(groups[0].querySelector('.ps-summary').textContent, '丸の内の散歩');
  assert.equal(groups[0].querySelector('.ps-match').textContent, '91% MATCH · 3枚');
  assert.equal(groups[0].querySelector('.ps-cluster-stat').textContent, '2構図 / 類似1枚を整理');
  assert.equal(groups[1].querySelector('.ps-kind').textContent, '02 / PET PHOTO');
  assert.equal(groups[1].querySelector('.ps-summary').textContent, '2枚の類似写真');
  assert.equal(groups[1].querySelector('.ps-match').textContent, '80% MATCH · 2枚');
  // 場面の中は構図の順（代表が先頭）に並び、ps.entries はその平らな配列
  assert.deepEqual(env.ev('ps.entries.map(e => e.photo.id)'), [101, 102, 103, 201, 202]);
  assert.deepEqual(env.ev('ps.entries.map(e => e.groupIndex + ":" + e.photoIndex)'),
                   ['0:0', '0:1', '0:2', '1:0', '1:1']);

  const part = (c, sel) => c.querySelector(sel);
  const rows = gcards(env).map(c => ({
    id: c.dataset.id,
    l1: part(c, '.l1').textContent, l1cls: part(c, '.l1').className.split(/\s+/).sort().join(' '),
    tags: c.querySelectorAll('.tag').map(t => t.textContent),
    tag0: part(c, '.tag').className,
    score: part(c, '.ps-score').textContent, visual: part(c, '.ps-visual').textContent,
    setloc: part(c, '.setloc').textContent, strong: part(c, '.setloc').classList.contains('strong'),
    rotate: part(c, '.rotate') !== null, collapsed: c.classList.contains('ps-collapsed'),
  }));
  assert.deepEqual(rows, [
    // 位置あり + AI PICK + 犬
    {id: '101', l1: '東京都 千代田区 丸の内', l1cls: 'l1', tags: ['EXIF', 'AI PICK', '犬'], tag0: 'tag t-exif',
     score: '91% MATCH', visual: '構図 1 代表', setloc: '位置', strong: false, rotate: true, collapsed: false},
    // 同じ構図の 2 枚目: 畳まれる。AI PICK なし（非推奨）
    {id: '102', l1: '東京都 千代田区 丸の内', l1cls: 'l1', tags: ['EXIF', '犬'], tag0: 'tag t-exif',
     score: '88% MATCH', visual: '代表と 97%', setloc: '位置', strong: false, rotate: true, collapsed: true},
    // 位置なし + 候補（ヒントもある）: 候補が優先
    {id: '103', l1: 'AI候補: 京都駅', l1cls: 'act l1', tags: ['位置なし', '風景'], tag0: 'tag t-none',
     score: '70% MATCH', visual: '構図 2 代表', setloc: '位置を付ける', strong: true, rotate: true, collapsed: false},
    // 位置なし + ヒント。source が local なので回転できない
    {id: '201', l1: '近い時刻の位置: 奈良市（2.0時間差）', l1cls: 'act hint l1', tags: ['位置なし', '猫'],
     tag0: 'tag t-none', score: '80% MATCH', visual: '構図 1 代表', setloc: '位置を付ける', strong: true,
     rotate: false, collapsed: false},
    // 推定: 座標 4 桁 + bucket
    {id: '202', l1: '34.5000, 135.2500', l1cls: 'l1', tags: ['推定 60分以内'], tag0: 'tag t-inferred',
     score: '60% MATCH', visual: '構図 2 代表', setloc: '位置', strong: false, rotate: true, collapsed: false},
  ]);
  assert.equal(gcards(env)[3].querySelectorAll('.tag')[1].className, 'tag t-pet');
  assert.equal(gcards(env)[0].querySelectorAll('.tag')[1].className, 'tag t-pick');

  // 畳み: 「すべて見る」で畳まれた写真が出て、もう一度で畳まれる。構図の代表の「＋1枚」でその構図だけ出る
  const expand = groups[0].querySelector('.ps-expand');
  assert.equal(expand.classList.contains('hidden'), false);
  assert.equal(expand.textContent, 'すべて見る（3枚）');
  assert.equal(groups[1].querySelector('.ps-expand').classList.contains('hidden'), true, '畳む写真が無い場面');
  const more = gcards(env)[0].querySelector('.ps-more');
  assert.equal(more.classList.contains('hidden'), false);
  assert.equal(more.textContent, '＋1枚の類似を表示');
  assert.equal(gcards(env)[2].querySelector('.ps-more').classList.contains('hidden'), true, '単独の構図には出ない');
  more.click();
  assert.equal(gcards(env)[1].classList.contains('ps-collapsed'), false);
  assert.equal(more.textContent, '類似1枚を閉じる');
  more.click();
  assert.equal(gcards(env)[1].classList.contains('ps-collapsed'), true);
  expand.click();
  assert.equal(gcards(env)[1].classList.contains('ps-collapsed'), false);
  assert.equal(expand.textContent, '同じ構図 1枚を畳む');
  assert.equal(more.classList.contains('hidden'), true, 'すべて表示中は個別の開閉を隠す');
  expand.click();
  assert.equal(gcards(env)[1].classList.contains('ps-collapsed'), true);
  assert.equal(expand.textContent, 'すべて見る（3枚）');
});

await test('(i2) 検索の結果が 0 件なら #empty を出し、スコア表示は mode に従う（recommended は NN SCORE）', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.handlers.search = () => reply(searchBody('x', [], 'empty'));
  await searchFor(env, 'x');
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(env.document.querySelectorAll('#groups section.ps-group').length, 0);
  assert.equal(hidden(env, '#empty'), false);
  env.handlers.search = () => reply(searchBody('', [G2], 'recommended'));
  await searchFor(env, '');
  assert.equal(gcards(env)[0].querySelector('.ps-score').textContent, '80 SCORE');
  assert.equal(env.document.querySelectorAll('#groups section.ps-group')[0].querySelector('.ps-match').textContent,
               '2 PHOTOS');
  assert.equal(hidden(env, '#empty'), true);
});

await test('(j) 検索の失敗 4 種: #search-note が表どおりで、view=list のまま、一覧・filter・page が残る', async () => {
  const cases = [
    [503, {ok: false, error: 'AI検索は無効化されています'}, 'AI は無効です（GEOFRAME_AI_ENABLED）。一覧は使えます'],
    [503, {ok: false, error: 'AI検索サービスへ接続できません。しばらくして再試行してください'},
     '検索の索引がまだ使えません。状況タブの AI の欄を見てください'],
    [429, {ok: false, error: '検索リクエストが多すぎます。しばらくしてから再試行してください'},
     '検索が多すぎます。1 分待ってから試してください'],
    [400, {ok: false, error: 'bad_params'}, '検索語は 300 文字以内で入力してください'],
    [500, {ok: false, error: 'boom'}, '検索できませんでした: boom'],
    [502, null, '検索できませんでした: HTTP 502'],
  ];
  for(const [status, body, want] of cases){
    const env = makeEnv({pathname: '/admin'});
    await env.boot();
    await openPhotos(env);
    chipOf(env, 'fav').click();
    await settle();
    env.$('#next').click();
    await settle();
    const before = cards(env);
    const listCalls = env.photoCalls().length;
    env.handlers.search = () => status === 502
      ? Promise.resolve({ok: false, status, json: () => Promise.reject(new Error('not json'))})
      : respond(status, body);
    await searchFor(env, '犬');
    assert.equal(note(env), want, String(status));
    assert.ok(env.$('#search-note').classList.contains('err'));
    assert.equal(env.ev('ps.view'), 'list');
    assert.equal(env.ev('ps.filter'), 'fav');
    assert.equal(env.ev('ps.page'), 2);
    assert.equal(hidden(env, '#cards'), false);
    assert.equal(hidden(env, '#groups'), true);
    assert.equal(hidden(env, '#chips'), false);
    assert.equal(hidden(env, '#close-search'), true);
    assert.ok(cards(env).length === before.length && cards(env).every((c, i) => c === before[i]), '一覧はそのまま');
    assert.equal(env.photoCalls().length, listCalls, '一覧を読み直さない');
  }
  // 通信失敗
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.handlers.search = () => Promise.reject(new Error('net down'));
  await searchFor(env, '犬');
  assert.equal(note(env), '検索できませんでした: net down');
  assert.equal(env.ev('ps.view'), 'list');
  // 検索が成功したあとの失敗は、前の結果を残して理由だけ出す。次に成功すれば理由は消える
  env.handlers.search = null;
  await searchFor(env, '犬');
  assert.equal(env.ev('ps.view'), 'search');
  env.handlers.search = () => respond(429, {ok: false, error: 'x'});
  await searchFor(env, '犬');
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(gcards(env).length, 5);
  assert.equal(note(env), '検索が多すぎます。1 分待ってから試してください');
  env.handlers.search = null;
  await searchFor(env, '猫');
  assert.ok(!env.$('#search-note').classList.contains('err'));
});

await test('(k) 「✕ 検索を閉じる」で view=list に戻り、直前の filter・page で一覧を読み直す', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  chipOf(env, 'noloc').click();
  await settle();
  env.$('#next').click();
  await settle();
  assert.equal(query(env.photoCalls().at(-1).url).get('page'), '2');
  await searchFor(env, '犬');
  assert.equal(env.ev('ps.view'), 'search');
  assert.deepEqual(onChips(env), ['noloc'], 'チップの印は変えない');
  const listCalls = env.photoCalls().length;
  env.$('#close-search').click();
  await settle();
  assert.equal(env.ev('ps.view'), 'list');
  assert.equal(env.photoCalls().length, listCalls + 1);
  const q = query(env.photoCalls().at(-1).url);
  assert.equal(q.get('filter'), 'noloc');
  assert.equal(q.get('page'), '2');
  assert.equal(hidden(env, '#cards'), false);
  assert.equal(hidden(env, '#groups'), true);
  assert.equal(hidden(env, '#chips'), false);
  assert.equal(hidden(env, '#pager'), false);
  assert.equal(hidden(env, '#close-search'), true);
  assert.equal(cards(env).length, PATTERNS.length);
  assert.equal(gcards(env).length, 0, '#groups の中身は空');
  assert.deepEqual(env.ev('ps.entries.map(e => e.photo.id)'), [1, 2, 3, 4, 5], 'ビューアは一覧の並びを見る');
  // 検索を見ているあいだに写真タブを離れて戻っても、検索の結果のまま（一覧を読み直さない）
  await searchFor(env, '犬');
  const n = env.photoCalls().length;
  env.$('.tab[data-tab=status]').click();
  await openPhotos(env);
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(env.photoCalls().length, n);
  // 要対応のセルから来たときは、検索中でも一覧へ
  env.$('.tab[data-tab=status]').click();
  await settle();
  env.$('#s-todo').children[0].click();
  await settle();
  assert.equal(env.ev('ps.view'), 'list');
  assert.equal(env.photoCalls().length, n + 1);
  assert.equal(query(env.photoCalls().at(-1).url).get('filter'), 'noloc');
});

await test('(l) 「AI おすすめ」は q 空、「除外も検索」は include_hidden=1。Enter で検索（変換中の Enter は無視）', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.$('#q').value = '前に入れた語';
  env.$('#recommend').click();
  await settle();
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=&groups=12');
  assert.equal(env.$('#q').value, '', 'おすすめは検索語を空にする');
  assert.equal(env.ev('ps.view'), 'search');
  env.$('#include-hidden').checked = true;
  env.$('#q').value = '海 犬';
  env.$('#search').click();
  await settle();
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('海 犬') + '&groups=12&include_hidden=1');
  assert.equal(env.ev('ps.includeHidden'), true);
  // チェックを切り替えると、検索中なら同じ語で検索し直す
  const n = env.searchCalls().length;
  env.$('#include-hidden').checked = false;
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.searchCalls().length, n + 1);
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('海 犬') + '&groups=12');
  // Enter
  env.$('#q').value = '夕焼け';
  env.$('#q').dispatchEvent({type: 'keydown', key: 'Enter', isComposing: true, keyCode: 229});
  await settle();
  assert.equal(env.searchCalls().length, n + 1, '変換を確定する Enter では検索しない');
  env.$('#q').dispatchEvent({type: 'keydown', key: 'Enter'});
  await settle();
  assert.equal(env.searchCalls().length, n + 2);
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('夕焼け') + '&groups=12');
});

await test('(m) 検索の古い応答（要求番号が古い）は捨てる。成功・失敗とも', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.handlers.search = (url) => new Promise(resolve => env.heldSearch.push(how => {
    const q = new URLSearchParams(url.split('?')[1]).get('q');
    if(how === 'fail') resolve({ok: false, status: 429, json: () => Promise.resolve({ok: false, error: 'x'})});
    else resolve({ok: true, status: 200, json: () => Promise.resolve(searchBody(q, q === 'first' ? [G2] : [G1, G2]))});
  }));
  await searchFor(env, 'first');
  await searchFor(env, 'second');
  assert.equal(env.heldSearch.length, 2);
  env.heldSearch[1]();                                       // 新しい要求が先に届く
  await settle();
  assert.equal(env.document.querySelectorAll('#groups section.ps-group').length, 2);
  const before = gcards(env);
  env.heldSearch[0]();                                       // 古い要求（1 場面）が遅れて届いても
  await settle();
  assert.equal(env.document.querySelectorAll('#groups section.ps-group').length, 2);
  assert.ok(gcards(env).length === before.length && gcards(env).every((c, i) => c === before[i]));
  assert.equal(env.ev('ps.query'), 'second');
  // 古い失敗も、今の表示を変えない
  await searchFor(env, 'third');
  await searchFor(env, 'fourth');
  env.heldSearch[3]();
  await settle();
  const okNote = note(env);
  env.heldSearch[2]('fail');
  await settle();
  assert.equal(note(env), okNote);
  assert.ok(!env.$('#search-note').classList.contains('err'));
  // 検索を飛ばしたまま一覧のチップを押すと、あとから届いた検索の結果で一覧を上書きしない
  await searchFor(env, 'fifth');
  env.$('.tab[data-tab=status]').click();
  await openPhotos(env);                                     // view は search のまま。もう一度チップを選ぶ
  env.$('#close-search').click();
  await settle();
  chipOf(env, 'fav').click();
  env.heldSearch[4]();
  await settle();
  assert.equal(env.ev('ps.view'), 'list');
});

await test('(n) 検索結果のカード: ★ で POST し、同じ id の全カードとビューアが更新され、件数が読み直される', async () => {
  const dup = sp({id: 301, name: 'dup.jpg', ...here, similarity: 0.5, visual_cluster: 5});
  const gdup = {...G2, photos: [dup, {...dup, visual_cluster: 6}]};
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.handlers.search = () => reply(searchBody('x', [G1, gdup]));
  await searchFor(env, 'x');
  const cs = gcards(env);
  const [rep, , , d1, d2] = cs;
  assert.deepEqual([d1.dataset.id, d2.dataset.id], ['301', '301']);
  // 開いているビューアも同じ写真なら更新される
  d1.querySelector('.ph').click();
  assert.equal(env.ev('viewerIndex'), 3);
  assert.equal(env.$('#viewer-favorite').textContent, '☆ favorite');
  const statusBefore = env.statusCalls().length;
  d2.querySelector('.fav').click();
  await settle();
  const post = env.calls.filter(c => c.method === 'POST').at(-1);
  assert.equal(post.url, '/api/admin/setflag');
  assert.equal(post.body, '{"id":301,"favorite":true}');
  assert.equal(post.headers['Content-Type'], 'application/json');
  [d1, d2].forEach(c => assert.ok(c.querySelector('.fav').classList.contains('on')));
  assert.ok(!rep.querySelector('.fav').classList.contains('on'), '別の写真は変わらない');
  assert.deepEqual(d1.querySelectorAll('.tag').map(t => t.textContent), ['EXIF', 'FAVORITE']);
  assert.equal(env.$('#viewer-favorite').textContent, '★ favorite');
  assert.ok(env.$('#viewer-favorite').classList.contains('on'));
  assert.equal(env.statusCalls().length, statusBefore + 1, 'loadFilterCounts() が呼ばれる');
  assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58');
  // 除外もビューアから: 全カードが薄くなり、ビューアの表示も変わる
  env.$('#viewer-hidden').click();
  await settle();
  assert.equal(env.calls.filter(c => c.method === 'POST').at(-1).body, '{"id":301,"hidden":true}');
  [d1, d2].forEach(c => assert.ok(c.classList.contains('is-hidden') && c.querySelector('.hid').classList.contains('on')));
  assert.equal(env.$('#viewer-hidden').textContent, '● hidden');
  // ビューアの ★ で外す: 基準は写真の今の値
  env.$('#viewer-favorite').click();
  await settle();
  assert.equal(env.calls.filter(c => c.method === 'POST').at(-1).body, '{"id":301,"favorite":false}');
  assert.ok(!d2.querySelector('.fav').classList.contains('on'));
  assert.equal(d1.querySelectorAll('.tag').map(t => t.textContent).join(), 'EXIF');
  // 失敗はトースト
  env.context.fetch = () => respond(500, {ok: false, error: 'nope'});
  d1.querySelector('.fav').click();
  await settle();
  assert.equal(env.$('#toast').textContent, '更新失敗: nope');
  assert.equal(env.$('#toast').className, 'show err');
});

await test('(o) 検索結果のカード: 回転は source===drive で thumb があるときだけ。押すと thumb・title が更新される', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');
  const [c101, , , c201] = gcards(env);
  assert.equal(c201.querySelector('.rotate'), null, 'local は回せない');
  const rot = c101.querySelector('.rotate');
  assert.equal(c101.querySelector('.ph img').src, '/derivatives/s.jpg');
  rot.click();
  assert.equal(rot.disabled, true);
  await settle();
  const post = env.calls.filter(c => c.url === '/api/admin/rotate');
  assert.equal(post.length, 1);
  assert.equal(post[0].body, '{"id":101,"delta":90}');
  assert.equal(c101.querySelector('.ph img').src, '/derivatives/rot90.jpg');
  assert.equal(rot.title, '時計回りに90°回して表示（今 90°）');
  assert.equal(rot.disabled, false);
  // thumb が無い検索結果の写真は回せない
  const env2 = makeEnv({pathname: '/admin'});
  await env2.boot();
  await openPhotos(env2);
  env2.handlers.search = () => reply(searchBody('x', [{...G2, photos: [{...S.rep, thumb: ''}]}]));
  await searchFor(env2, 'x');
  assert.equal(gcards(env2)[0].querySelector('.rotate'), null);
  assert.equal(gcards(env2)[0].querySelector('.ph').classList.contains('ps-clickable'), false, '画像が無ければ開けない');
});

await test('(p) ビューア: 画像クリックで開き、stepModal が ps.entries の順に進んで末尾で先頭へ回り、Escape で閉じる', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');
  assert.equal(hidden(env, '#viewer'), true);
  gcards(env)[1].querySelector('.ph').click();               // 畳まれていても entries の添字で開ける（id 102）
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.ev('viewerIndex'), 1);
  same(env.document.activeElement, env.$('#viewer-close'));
  assert.ok(env.document.body.classList.contains('ps-viewer-open'));
  const shown = () => ({
    title: env.$('#viewer-title').textContent, counter: env.$('#viewer-counter').textContent,
    kind: env.$('#viewer-group-kind').textContent, img: env.$('#viewer-image').src,
  });
  assert.deepEqual(shown(), {title: '#102  dog2.jpg', counter: '2 / 5', kind: 'SAME SCENE · GROUP 1 · 2/3',
                             img: '/derivatives/s.jpg'});
  assert.equal(env.$('#viewer-image-score').textContent, '88% MATCH');
  assert.equal(env.$('#viewer-group-summary').textContent, '丸の内の散歩');
  assert.equal(hidden(env, '#viewer-ai'), false);
  assert.equal(hidden(env, '#viewer-group'), false);
  assert.ok(env.$('#viewer-scores').textContent.includes('88'));
  assert.ok(env.$('#viewer-analysis').textContent.includes('犬'));
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(shown().title, '#103  view.jpg');
  assert.equal(env.$('#viewer-location').textContent, 'AI候補: 京都駅', 'ビューアにも 1 行目と同じ手がかり');
  vm.runInContext('stepModal(1)', env.context);
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(shown().title, '#202  inf.jpg');
  assert.equal(shown().counter, '5 / 5');
  assert.equal(env.$('#viewer-group-kind').textContent, 'PET PHOTO · GROUP 2 · 2/2');
  assert.ok(env.$('#viewer-location').textContent.includes('34.50000, 135.25000'));
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(shown().title, '#101  dog1.jpg', '末尾の次は先頭（回る）');
  assert.equal(env.$('#viewer-reasons').textContent, '表情が良い');
  assert.equal(env.$('#viewer-concerns').textContent, '少し暗い');
  vm.runInContext('stepModal(-1)', env.context);
  assert.equal(shown().title, '#202  inf.jpg', '先頭の前は末尾');
  assert.equal(env.$('#viewer-reasons').textContent, 'AI詳細評価なし');
  // ボタン・キー・端のクリック・スワイプ
  env.$('#viewer-prev').click();
  assert.equal(shown().counter, '4 / 5');
  env.document.dispatchEvent({type: 'keydown', key: 'ArrowRight'});
  assert.equal(shown().counter, '5 / 5');
  env.document.dispatchEvent({type: 'keydown', key: 'ArrowLeft'});
  assert.equal(shown().counter, '4 / 5');
  env.$('#viewer-media').dispatchEvent({type: 'click', clientX: 90});     // 幅 100 の右端 → 次
  assert.equal(shown().counter, '5 / 5');
  env.$('#viewer-media').dispatchEvent({type: 'click', clientX: 50});     // 中央は動かない
  assert.equal(shown().counter, '5 / 5');
  env.$('#viewer-media').dispatchEvent({type: 'pointerdown', isPrimary: true, pointerId: 1, clientX: 200, clientY: 0});
  env.$('#viewer-media').dispatchEvent({type: 'pointerup', pointerId: 1, clientX: 100, clientY: 5});   // 左へスワイプ → 次
  assert.equal(shown().counter, '1 / 5');
  env.$('#viewer-media').dispatchEvent({type: 'click', clientX: 90});     // スワイプ直後のクリックは無視
  assert.equal(shown().counter, '1 / 5');
  // Escape で閉じ、フォーカスを戻す
  env.document.dispatchEvent({type: 'keydown', key: 'Escape'});
  assert.equal(hidden(env, '#viewer'), true);
  assert.equal(env.ev('viewerIndex'), -1);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  notSame(env.document.activeElement, env.$('#viewer-close'));
  // 閉じたあとの矢印は何もしない。背景のクリックでも閉じる
  env.document.dispatchEvent({type: 'keydown', key: 'ArrowRight'});
  assert.equal(env.ev('viewerIndex'), -1);
  gcards(env)[0].querySelector('.ph').click();
  env.$('#viewer').dispatchEvent({type: 'click'});
  assert.equal(hidden(env, '#viewer'), true);
  gcards(env)[0].querySelector('.ph').click();
  env.$('#viewer-close').click();
  assert.equal(hidden(env, '#viewer'), true);
});

await test('(p3) ビューアの画像が読めない（error）と「画像なし」が出て、次の写真に移ると消える。thumb が空なら最初から出る', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');
  const shown = () => ({none: !hidden(env, '#viewer-noimage'), img: !hidden(env, '#viewer-image'),
                        text: env.$('#viewer-noimage').textContent});
  gcards(env)[1].querySelector('.ph').click();
  assert.deepEqual(shown(), {none: false, img: true, text: '画像なし'});
  env.$('#viewer-image').dispatchEvent({type: 'error'});
  assert.deepEqual(shown(), {none: true, img: false, text: '画像なし'});
  vm.runInContext('stepModal(1)', env.context);
  assert.deepEqual(shown(), {none: false, img: true, text: '画像なし'}, '次の写真では画像の表示に戻る');
  // thumb が空の写真は、error を待たずに代わりの表示
  vm.runInContext("vEntries()[viewerIndex].photo.thumb = ''; renderModal()", env.context);
  assert.deepEqual(shown(), {none: true, img: false, text: '画像なし'});
  env.$('#viewer-close').click();
});

await test('(p2) 一覧のカードも同じビューアで開ける（AI の欄は出さず、一覧の並びで前後へ）。検索を閉じるとビューアも閉じる', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  cards(env)[2].querySelector('.ph').click();                // id 3
  assert.equal(hidden(env, '#viewer'), false);
  assert.equal(env.$('#viewer-title').textContent, '#3  c.jpg');
  assert.equal(env.$('#viewer-counter').textContent, '3 / 5');
  assert.equal(env.$('#viewer-group-kind').textContent, '');
  assert.equal(hidden(env, '#viewer-ai'), true);
  assert.equal(hidden(env, '#viewer-group'), true);
  assert.equal(hidden(env, '#viewer-image-score'), true);
  assert.equal(env.$('#viewer-location').textContent, 'AI候補: 京都駅');
  assert.equal(env.$('#viewer-badges').textContent, '位置なし');
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(env.$('#viewer-title').textContent, '#4  d.jpg');
  assert.equal(env.$('#viewer-location').textContent, '近い時刻の位置: 奈良市（2.0時間差）');
  // 一覧のカードからの ★ はビューアにも反映
  cards(env)[3].querySelector('.fav').click();
  await settle();
  assert.equal(env.$('#viewer-favorite').textContent, '★ favorite');
  // 画像が無い写真（id 5）のカードは開けない
  assert.equal(cards(env)[4].querySelector('.ph').classList.contains('ps-clickable'), false);
  // 検索結果を作ってビューアを開いたまま「検索を閉じる」
  await searchFor(env, '犬');
  gcards(env)[0].querySelector('.ph').click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('#close-search').click();
  assert.equal(hidden(env, '#viewer'), true);
});

await test('(q) 位置モーダルの保存（検索結果のカード）: 地図の click → SAVE → POST setloc、全カードが座標・手動に、件数が読み直される', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');
  env.timers.immediate = true;                               // openLocModal の setTimeout(…, 30) をすぐ実行する
  const c103 = gcards(env)[2];
  assert.equal(hidden(env, '#modal'), true);
  c103.querySelector('.setloc').click();
  assert.equal(hidden(env, '#modal'), false);
  assert.equal(env.$('#m-title').textContent, '#103  view.jpg  /  2026-10-02 09:00:00');
  assert.equal(hidden(env, '#m-suggest'), false);
  assert.ok(env.$('#m-hint').textContent.includes('大阪'));
  assert.equal(env.maplibre.maps.length, 1);
  assert.equal(env.$('#m-save').disabled, true);
  env.$('#m-save').click();                                  // 位置を選ぶ前は何も送らない
  await settle();
  assert.equal(env.calls.filter(c => c.url === '/api/admin/setloc').length, 0);
  const map = env.maplibre.maps[0];
  map.handlers.click({lngLat: {lat: 35.123456789, lng: 139.987654321}});
  assert.equal(env.$('#m-pick').textContent, '35.12346, 139.98765');
  assert.equal(env.$('#m-save').disabled, false);
  const statusBefore = env.statusCalls().length, listBefore = env.photoCalls().length;
  env.$('#m-save').click();
  await settle();
  const posts = env.calls.filter(c => c.url === '/api/admin/setloc');
  assert.equal(posts.length, 1);
  assert.equal(posts[0].method, 'POST');
  assert.equal(posts[0].body, '{"id":103,"lat":35.123457,"lon":139.987654}');
  assert.equal(posts[0].headers['Content-Type'], 'application/json');
  assert.equal(c103.querySelector('.l1').textContent, '35.1235, 139.9877');
  assert.equal(c103.querySelector('.l1').className, 'l1', 'AI候補の強調は外れる');
  assert.deepEqual(c103.querySelectorAll('.tag').map(t => t.textContent), ['手動', '風景']);
  assert.equal(c103.querySelector('.tag').className, 'tag t-manual');
  assert.equal(c103.querySelector('.setloc').textContent, '位置');
  assert.equal(c103.querySelector('.setloc').classList.contains('strong'), false);
  assert.equal(hidden(env, '#modal'), true);
  assert.equal(env.$('#toast').textContent, '#103 の位置を設定しました');
  assert.equal(env.statusCalls().length, statusBefore + 1, 'loadFilterCounts() が呼ばれる');
  assert.equal(env.photoCalls().length, listBefore, '検索結果は読み直さない');
  assert.equal(env.ev('ps.view'), 'search');
  // 失敗: モーダルは開いたまま、SAVE は押し直せる
  const c202 = gcards(env)[4];
  c202.querySelector('.setloc').click();
  map.handlers.click({lngLat: {lat: 1, lng: 2}});
  env.context.fetch = () => respond(503, {ok: false, error: 'busy'});
  env.$('#m-save').click();
  await settle();
  assert.equal(env.$('#toast').textContent, '位置の保存に失敗: busy');
  assert.equal(hidden(env, '#modal'), false);
  assert.equal(env.$('#m-save').disabled, false);
  assert.equal(c202.querySelector('.l1').textContent, '34.5000, 135.2500', 'カードは変わらない');
  // Escape で閉じる（ビューアが閉じているとき）
  env.document.dispatchEvent({type: 'keydown', key: 'Escape'});
  assert.equal(hidden(env, '#modal'), true);
});

await test('(q2) 位置モーダルの保存（一覧のカード）: カードが座標・手動に描き直り、一覧と件数が読み直される', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.timers.immediate = true;
  const c4 = cards(env)[3];                                  // id 4: 位置なし + ヒント
  c4.querySelector('.setloc').click();
  env.maplibre.maps[0].handlers.click({lngLat: {lat: 34.6851, lng: 135.8048}});
  const statusBefore = env.statusCalls().length, listBefore = env.photoCalls().length;
  env.$('#m-save').click();
  await settle();
  assert.equal(env.calls.filter(c => c.url === '/api/admin/setloc').at(-1).body, '{"id":4,"lat":34.6851,"lon":135.8048}');
  assert.equal(env.statusCalls().length, statusBefore + 1);
  assert.equal(env.photoCalls().length, listBefore + 1, '位置なしのフィルタから外れるので読み直す');
  assert.equal(query(env.photoCalls().at(-1).url).get('filter'), 'all');
  // 読み直しの応答（サーバーの値）で置き換わる前のカード（c4）は、保存直後に座標・手動へ描き直されていた
  assert.equal(c4.querySelector('.l1').textContent, '34.6851, 135.8048');
  assert.equal(c4.querySelector('.tag').textContent, '手動');
  assert.equal(env.$('#toast').textContent, '#4 の位置を設定しました');
});

await test('(r) 地名検索の海外候補に OSM の出典が出る（リンクは DOM で組み、HTML 文字列を使わない）。地図の出典は定数から', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.timers.immediate = true;
  cards(env)[0].querySelector('.setloc').click();
  env.$('#m-q').value = 'Times Square';
  env.$('#m-search').click();
  await settle();
  const box = env.$('#m-results');
  assert.equal(box.classList.contains('hidden'), false);
  assert.equal(box.children.length, 2);
  assert.equal(box.children[0].textContent, 'Times Square');
  const credit = box.children[1];
  assert.equal(credit.textContent, '地名データ © OpenStreetMap contributors');
  const a = credit.querySelector('a');
  assert.equal(a.getAttribute('href'), 'https://www.openstreetmap.org/copyright');
  assert.equal(a.getAttribute('rel'), 'noopener noreferrer');
  assert.equal(a.textContent, 'OpenStreetMap');
  assert.equal(env.ev('TILE_ATTRIB_OSM'),
               "© <a href='https://www.openstreetmap.org/copyright'>OpenStreetMap</a> contributors");
  assert.equal(env.maplibre.maps[0].opts.style.sources.osm.attribution, env.ev('TILE_ATTRIB_OSM'));
  assert.ok(env.maplibre.maps[0].opts.style.sources.gsi.attribution.includes('国土地理院'));
});

await test('(s) loadFilterCounts: 古い応答が通信失敗・ok:false でも件数もトーストも変わらない。最新の失敗はトーストに出る', async () => {
  for(const how of ['reject', 'notok']){
    const env = makeEnv({pathname: '/admin'});
    await env.boot();
    await openPhotos(env);                                   // 件数 57
    env.gate.holdStatus = true;
    vm.runInContext('loadFilterCounts()', env.context);      // A（古い）
    cards(env)[0].querySelector('.fav').click();             // ★ → B（最新）
    await settle();
    assert.equal(env.heldStatus.length, 2);
    env.heldStatus[1]();
    await settle();
    assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58');
    const toastText = env.$('#toast').textContent, toastCls = env.$('#toast').className;
    env.heldStatus[0](how);                                  // 古い A が失敗で届く
    await settle();
    assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '58', how);
    assert.equal(env.$('#toast').textContent, toastText, how);
    assert.equal(env.$('#toast').className, toastCls, how);
  }
  // 最新の要求が失敗したときは、件数をそのままにトーストで知らせる
  for(const [how, text] of [['reject', '件数取得失敗: net down'], ['notok', '件数取得失敗: boom']]){
    const env = makeEnv({pathname: '/admin'});
    await env.boot();
    await openPhotos(env);
    env.gate.holdStatus = true;
    vm.runInContext('loadFilterCounts()', env.context);
    await settle();
    env.heldStatus[0](how);
    await settle();
    assert.equal(chipOf(env, 'fav').querySelector('.n').textContent, '57', '前の件数のまま');
    assert.equal(env.$('#toast').textContent, text);
    assert.equal(env.$('#toast').className, 'show err');
  }
});

await test('(m2) 飛んでいる検索と一覧が入れ違っても、あとから届いた古い応答で今の表示を壊さない', async () => {
  // 検索を飛ばしたままチップを押す → 届いた検索の結果で、選んだ一覧に戻さない
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  env.handlers.search = (url) => new Promise(resolve => env.heldSearch.push(() =>
    resolve({ok: true, status: 200, json: () => Promise.resolve(searchBody('x'))})));
  await searchFor(env, 'x');
  chipOf(env, 'fav').click();
  await settle();
  env.heldSearch[0]();
  await settle();
  assert.equal(env.ev('ps.view'), 'list');
  assert.equal(hidden(env, '#groups'), true);
  assert.equal(cards(env).length, PATTERNS.length);
  // 一覧の読み込み中に検索が成功する → 遅れて届いた一覧で ps.entries（ビューアが見る配列）を上書きしない
  const env2 = makeEnv({pathname: '/admin', hold: true});
  await env2.boot();
  env2.$('.tab[data-tab=photos]').click();
  await settle();
  assert.equal(env2.held.length, 1);
  await searchFor(env2, 'x');
  assert.equal(env2.ev('ps.view'), 'search');
  env2.held[0]();
  await settle();
  assert.deepEqual(env2.ev('ps.entries.map(e => e.photo.id)'), [101, 102, 103, 201, 202]);
  assert.equal(cards(env2).length, 0, '隠れた #cards にも描かない');
  assert.equal(hidden(env2, '#empty'), true);
});

await test('(v) 検索の応答待ちに「除外も検索」を切り替えると、要求中の語で include_hidden=1 を付けて検索し直す。古い応答は捨てる', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');                                // 「犬」の結果を表示中
  env.handlers.search = (url) => new Promise(resolve => env.heldSearch.push(() => {
    const q = new URLSearchParams(url.split('?')[1]).get('q');
    resolve({ok: true, status: 200, json: () => Promise.resolve(searchBody(q, [G1]))});
  }));
  await searchFor(env, '猫');                                // 応答前
  assert.equal(env.ev('ps.query'), '犬', '成功時にしか ps.query は変わらない');
  env.$('#include-hidden').checked = true;
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.heldSearch.length, 2);
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('猫') + '&groups=12&include_hidden=1');
  env.heldSearch[0]();                                       // 古い「猫」（除外なし）が遅れて届いても捨てる
  await settle();
  assert.equal(env.ev('ps.query'), '犬');
  env.heldSearch[1]();
  await settle();
  assert.equal(env.ev('ps.query'), '猫');
  assert.equal(env.ev('ps.pending'), null);
  // 応答が済んだら、表示中の語（ps.query）で検索し直す
  env.$('#include-hidden').checked = false;
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('猫') + '&groups=12');
});

await test('(v2) 初回検索（一覧を見ている）の応答前に切り替えても、チェックに合う要求の結果が表示される。要求が無ければ何もしない', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  const n0 = env.searchCalls().length;
  env.$('#include-hidden').checked = true;                   // 一覧で、要求中の検索も無い → 何もしない
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.searchCalls().length, n0);
  env.$('#include-hidden').checked = false;
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  env.handlers.search = (url) => new Promise(resolve => env.heldSearch.push(() => {
    const q = new URLSearchParams(url.split('?')[1]).get('q');
    const inc = new URLSearchParams(url.split('?')[1]).get('include_hidden') === '1';
    resolve({ok: true, status: 200, json: () => Promise.resolve({...searchBody(q, inc ? [G1, G2] : [G2]), include_hidden: inc})});
  }));
  await searchFor(env, '犬');                                // 初回検索（応答前）
  assert.equal(env.ev('ps.view'), 'list');
  env.$('#include-hidden').checked = true;
  env.$('#include-hidden').dispatchEvent({type: 'change'});
  await settle();
  assert.equal(env.searchCalls().length, n0 + 2);
  assert.equal(env.searchCalls().at(-1).url, '/api/ai/search?q=' + encodeURIComponent('犬') + '&groups=12&include_hidden=1');
  env.heldSearch[0]();                                       // 除外なしの古い応答は捨てる
  await settle();
  assert.equal(env.ev('ps.view'), 'list');
  env.heldSearch[1]();
  await settle();
  assert.equal(env.ev('ps.view'), 'search');
  assert.equal(env.document.querySelectorAll('#groups section.ps-group').length, 2, 'チェックどおり除外も含む結果');
});

await test('(w) 写真タブを離れると、ビューアと位置モーダルが閉じ、背景のスクロール禁止も外れる', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  await searchFor(env, '犬');
  gcards(env)[1].querySelector('.ph').click();
  assert.equal(hidden(env, '#viewer'), false);
  assert.ok(env.document.body.classList.contains('ps-viewer-open'));
  env.$('.tab[data-tab=status]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), -1);
  env.timers.immediate = true;
  await openPhotos(env);
  gcards(env)[2].querySelector('.setloc').click();
  assert.equal(hidden(env, '#modal'), false);
  env.$('.tab[data-tab=status]').click();
  await settle();
  assert.equal(hidden(env, '#modal'), true);
});

await test('(x) 検索語の長さはコードポイントで数える: 日本語 300 は送る、絵文字を含む 301 は送らない、UTF-16 で 301 でも 300 なら送る', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  const n0 = env.searchCalls().length;
  await searchFor(env, 'あ'.repeat(300));
  assert.equal(env.searchCalls().length, n0 + 1);
  await searchFor(env, 'あ'.repeat(299) + '😀' + 'い');       // コードポイント 301
  assert.equal(env.searchCalls().length, n0 + 1, '301 は送らない');
  assert.equal(note(env), '検索語は 300 文字以内で入力してください');
  assert.ok(env.$('#search-note').classList.contains('err'));
  const q300 = 'あ'.repeat(299) + '😀';                        // UTF-16 で 301、コードポイントで 300
  assert.equal(q300.length, 301);
  await searchFor(env, q300);
  assert.equal(env.searchCalls().length, n0 + 2);
  assert.equal(query(env.searchCalls().at(-1).url).get('q'), q300);
});

await test('(y) 301 文字を送らないとき、応答待ちの「犬」の検索は無効になり、後から成功・失敗が届いても注意書きと表示は変わらない', async () => {
  for(const outcome of ['ok', 'fail']){
    const env = makeEnv({pathname: '/admin'});
    await env.boot();
    await openPhotos(env);
    let release;
    env.handlers.search = () => new Promise(res => { release = res; });
    env.$('#q').value = '犬';
    env.$('#search').click();
    await settle();
    assert.equal(note(env), '検索しています…');
    await searchFor(env, 'あ'.repeat(301));
    const want = '検索語は 300 文字以内で入力してください';
    assert.equal(note(env), want);
    assert.equal(env.ev('ps.pending'), null);
    release(outcome === 'ok' ? await reply(searchBody('犬', [G2], 'search')) : await respond(500, {ok: false, error: 'boom'}));
    await settle();
    assert.equal(note(env), want, outcome);
    assert.ok(env.$('#search-note').classList.contains('err'), outcome);
    assert.equal(env.ev('ps.view'), 'list', outcome);
  }
});

await test('(z) ps.entries と違う配列から openModalFor で開く: ★は渡した写真の id に POST し、ps.entries の写真は変わらず、閉じると開いた要素にフォーカスが戻る', async () => {
  const env = makeEnv({pathname: '/admin'});
  await env.boot();
  await openPhotos(env);
  assert.equal(env.ev('ps.entries.map(e => e.photo.id)').length, 5);
  // 一覧とは別の写真（id 900・901。架空の座標）。id 1 は ps.entries にもある同じ写真の別の object
  const mine = [
    {id: 900, name: 'lib-a.jpg', thumb: '/derivatives/lib-a.jpg', thumb_url: '/thumb/900.jpg?v=aaaaaaaaaaaa',
     taken_at: '2030-01-02 03:04', lat: 10.0, lon: 20.0, loc_source: 'exif', place: '', pref: '架空県', city: '試験市',
     favorite: false, hidden: false, rotation: 0, source: 'drive', ai: null, visual: null},
    {id: 901, name: 'lib-b.jpg', thumb: '/derivatives/lib-b.jpg', thumb_url: '', taken_at: '2030-01-02 03:05',
     lat: null, lon: null, loc_source: 'none', place: '', pref: '', city: '', favorite: false, hidden: false,
     rotation: 0, source: 'local', ai: null, visual: null},
    {id: 1, name: 'dup.jpg', thumb: '/derivatives/dup.jpg', thumb_url: '', taken_at: '', lat: null, lon: null,
     loc_source: 'none', place: '', pref: '', city: '', favorite: false, hidden: false, rotation: 0, source: 'drive',
     ai: null, visual: null},
  ];
  vm.runInContext('globalThis.__mine = ' + JSON.stringify(mine), env.context);
  const opener = env.document.createElement('button');
  env.document.body.appendChild(opener);
  opener.focus();
  vm.runInContext('openModalFor(__mine, 1, {ai: false})', env.context);
  assert.equal(hidden(env, '#viewer'), false);
  assert.ok(env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), 1);
  assert.equal(env.$('#viewer-title').textContent, '#901  lib-b.jpg');
  assert.equal(env.$('#viewer-counter').textContent, '2 / 3', '渡した配列の長さ');
  assert.equal(hidden(env, '#viewer-ai'), true);
  assert.equal(hidden(env, '#viewer-group'), true);
  same(env.document.activeElement, env.$('#viewer-close'));
  // 前後は渡した配列の中で回る（AI の欄は出さないまま）
  vm.runInContext('stepModal(-1)', env.context);
  assert.equal(env.$('#viewer-title').textContent, '#900  lib-a.jpg');
  assert.equal(hidden(env, '#viewer-ai'), true);
  vm.runInContext('stepModal(-1)', env.context);
  assert.equal(env.$('#viewer-title').textContent, '#1  dup.jpg', '先頭の前は末尾');
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(env.$('#viewer-title').textContent, '#900  lib-a.jpg');
  // ★は渡した写真（id 900）の id で setflag に POST し、ビューアの表示が変わる。ps.entries の写真は変わらない
  const before = env.calls.filter(c => c.url === '/api/admin/setflag').length;
  env.$('#viewer-favorite').click();
  await settle();
  const posts = env.calls.filter(c => c.url === '/api/admin/setflag');
  assert.equal(posts.length, before + 1);
  assert.deepEqual(JSON.parse(posts.at(-1).body), {id: 900, favorite: true});
  assert.equal(env.$('#viewer-favorite').textContent, '★ favorite');
  assert.equal(env.ev('__mine[0].favorite'), true, '渡した配列の写真に反映');
  assert.deepEqual(env.ev('ps.entries.map(e => e.photo.favorite)'), [false, false, false, false, false]);
  // 同じ id の写真が ps.entries にもあるとき（id 1）は、両方が更新され、一覧のカードも描き直される
  vm.runInContext('stepModal(1)', env.context);
  vm.runInContext('stepModal(1)', env.context);
  assert.equal(env.$('#viewer-title').textContent, '#1  dup.jpg');
  env.$('#viewer-hidden').click();
  await settle();
  assert.deepEqual(env.ev('ps.entries.map(e => e.photo.hidden)'), [true, false, false, false, true]);
  assert.ok(cards(env)[0].classList.contains('is-hidden'));
  // 変更の購読: (id, patch) が渡る
  vm.runInContext('globalThis.__seen = []; onPhotoChange((id, patch) => __seen.push([id, patch]))', env.context);
  env.$('#viewer-hidden').click();
  await settle();
  assert.deepEqual(env.ev('__seen'), [[1, {favorite: false, hidden: false}]]);
  // 閉じると、開いたときにフォーカスのあった要素へ戻り、以後 ps.entries のビューアに戻る
  env.$('#viewer-close').click();
  assert.equal(hidden(env, '#viewer'), true);
  same(env.document.activeElement, opener);
  assert.equal(env.ev('viewerEntries'), null);
  cards(env)[1].querySelector('.ph').click();
  assert.equal(env.$('#viewer-counter').textContent, '2 / 5', '写真タブから開くと ps.entries');
  assert.equal(env.$('#viewer-title').textContent, '#2  b.jpg');
  // 範囲外・空の配列では開かない
  env.$('#viewer-close').click();
  vm.runInContext('openModalFor([], 0, {ai: false}); openModalFor(__mine, 3, {})', env.context);
  assert.equal(hidden(env, '#viewer'), true);
});

await test('(z2) tileStyle() は呼ぶたびに新しい style を返し、タイルは現在の origin から組む', async () => {
  const env = makeEnv({pathname: '/admin'});
  const a = env.ev('tileStyle()'), b = env.ev('tileStyle()');
  assert.deepEqual(a, b);
  assert.equal(a.sources.osm.tiles[0], 'http://localhost/tiles/osm/{z}/{x}/{y}.png');
  assert.equal(a.sources.gsi.tiles[0], 'http://localhost/tiles/gsi/{z}/{x}/{y}.png');
  assert.deepEqual(a.layers.map(l => l.id), ['bg', 'osm', 'gsi']);
  assert.ok(a.sources.osm.attribution.includes('OpenStreetMap'));
  assert.ok(vm.runInContext('tileStyle() !== tileStyle() && tileStyle().sources !== tileStyle().sources', env.context));
});

console.log(`ok (${n})`);
