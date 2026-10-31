// admin.js の写真タブ（チップ・一覧・カード・フラグ操作・要対応）を、fakedom.mjs の DOM の代用品と
// fetch の代用品で動かして確かめる。
// 使い方: node web/tests/photos_render.mjs <admin.js のパス>
// fetch は URL ごとの合成 JSON を返すだけで、ネットワークには出ない。
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom, htmlFacts, domFacts, CHIPS} from './fakedom.mjs';

const jsPath = process.argv[2];
assert.ok(jsPath, 'admin.js のパスが要る');
const htmlPath = path.join(path.dirname(jsPath), '..', 'admin.html');
const source = fs.readFileSync(jsPath, 'utf8');

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const reply = body => Promise.resolve({ok: true, status: 200, json: () => Promise.resolve(body)});

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

// 1 回の試験の環境: DOM・fetch・vm の context。calls に fetch の記録が残る。
function makeEnv({pathname, photos = PATTERNS, hold = false}){
  const document = buildAdminDom();
  const filters = {...FILTERS};
  const calls = [];
  const held = [];
  const flags = {};
  const fetchFake = (url, opts = {}) => {
    const rec = {url, method: opts.method || 'GET', headers: opts.headers, body: opts.body};
    calls.push(rec);
    if(url === '/api/admin/status') return reply(statusBody(filters));
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
    return reply({ok: false, error: 'unexpected ' + url});
  };
  const context = vm.createContext({
    document, fetch: fetchFake, location: {pathname, origin: 'http://localhost'},
    setInterval(){}, setTimeout(){}, clearTimeout(){}, console,
  });
  vm.runInContext(source, context, {filename: 'admin.js'});
  const photoCalls = () => calls.filter(c => c.url.startsWith('/api/admin/photos?'));
  const statusCalls = () => calls.filter(c => c.url === '/api/admin/status');
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  return {document, context, calls, held, photoCalls, statusCalls, boot, $: s => document.querySelector(s)};
}
const query = url => new URLSearchParams(url.split('?')[1]);
const chipOf = (env, f) => env.$(`.chip[data-filter=${f}]`);
const onChips = env => env.document.querySelectorAll('.chip').filter(c => c.classList.contains('on'))
  .map(c => c.dataset.filter);
const activeTab = env => env.document.querySelectorAll('nav .tab').filter(t => t.classList.contains('on'))
  .map(t => t.dataset.tab);
const cards = env => env.document.querySelectorAll('#cards .card');
const hidden = (env, sel) => env.$(sel).classList.contains('hidden');

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('admin.js を読み込んだだけでは location にも fetch にも触らない', async () => {
  const touched = [];
  const sandbox = {document: buildAdminDom(), setInterval(){}, setTimeout(){}, clearTimeout(){}, console};
  Object.defineProperty(sandbox, 'location', {get(){ touched.push('location'); throw new Error('location'); }});
  Object.defineProperty(sandbox, 'fetch', {get(){ touched.push('fetch'); throw new Error('fetch'); }});
  vm.runInContext(source, vm.createContext(sandbox), {filename: 'admin.js'});
  assert.deepEqual(touched, []);
});

await test('admin.html と fakedom の骨格（id・タブ・チップ）が一致している', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const want = htmlFacts(html);
  const have = domFacts(buildAdminDom());
  assert.deepEqual(have.ids, want.ids, 'id の集合');
  assert.deepEqual(have.tabs, ['status', 'photos']);
  assert.deepEqual(want.tabs, have.tabs, 'data-tab');
  assert.deepEqual(want.filters, CHIPS.map(c => c[0]), 'data-filter');
  assert.deepEqual(have.filters, want.filters);
  // チップの表示名も admin.html と同じ（"すべて <span class="n">" の形）
  CHIPS.forEach(([f, label]) => {
    assert.ok(html.includes(`data-filter="${f}">${label} <span class="n"></span>`), `${f} の表示名 ${label}`);
  });
  assert.ok(!/href="[^"]*blog-admin/.test(html), '旅行ブログのリンクが無い');
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
  vm.runInContext(source, bare);
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

console.log(`ok (${n})`);
