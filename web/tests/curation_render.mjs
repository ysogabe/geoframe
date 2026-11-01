// admin.js・photos.js・library.js・curation.js（この順に同じ context へ読む）の選別タブ — 段（種類・枚数・状態・メンバー・
// 点数・タグ）、チェックして「代表にする」、「このままで確認済み」、「取り消す」、409 の読み直し、ビューア、★・除外の反映、
// 絞り込み、ページング — を、fakedom.mjs の DOM の代用品と fetch の代用品で動かして確かめる。
// 使い方: node web/tests/curation_render.mjs <admin.js のパス> <photos.js のパス> <library.js のパス> <curation.js のパス>
// fetch は URL ごとの合成 JSON を返すだけで、ネットワークには出ない。地名・座標はすべて架空の値。
import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom} from './fakedom.mjs';

const [adminPath, photosPath, libraryPath, curationPath] = process.argv.slice(2);
assert.ok(adminPath && photosPath && libraryPath && curationPath,
  'admin.js・photos.js・library.js・curation.js のパスが要る');
const sources = [[adminPath, 'admin.js'], [photosPath, 'photos.js'], [libraryPath, 'library.js'], [curationPath, 'curation.js']]
  .map(([p, name]) => [fs.readFileSync(p, 'utf8'), name]);
const loadScripts = context => sources.forEach(([text, name]) => vm.runInContext(text, context, {filename: name}));

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const respond = (status, body) =>
  Promise.resolve({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body)});
const reply = body => respond(200, body);
const clone = v => JSON.parse(JSON.stringify(v));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return {promise, resolve}; };

// ---- 合成データ（架空の県・市・座標） ----
const statusBody = () => ({
  ok: true, server_time: '2030-01-01 10:00:00',
  sync: {ready: 10, pending: 0, total: 10},
  db: {no_loc: 0, no_display: 0, favorite: 0, hidden: 0, enrich_pending: 0, no_time: 0, by_loc_source: {exif: 10},
       infer_buckets: {}, by_source: {drive: 10}, filters: {all: 10, noloc: 0, suggest: 0, inferred: 0, manual: 0, fav: 0, hidden: 0}},
  disk: {used_pct: 10, free_bytes: 1e11, total_bytes: 2e11,
         dirs: {derivatives: {bytes: 1e6, files: 10}, tile_cache: {bytes: 1e3, files: 1}}},
});
const mem = (id, o = {}) => ({
  id, name: 'p' + id + '.jpg', thumb: '/derivatives/p' + id + '.jpg', thumb_url: '/thumb/' + id + '.jpg?v=0123456789ab',
  taken_at: '2030-05-03 09:12', place: '', pref: '架空県', city: '試験市', lat: 10.0, lon: 20.0,
  favorite: false, hidden: false, rotation: 0, source: 'drive',
  rank: 1, score: 80, reasons: [], issues: [], recommended: false, effective: false, curation: null, selectable: true, ...o,
});
const grp = (kind, group_id, token, state, members, o = {}) =>
  ({kind, group_id, token, state, taken_at: '2030-05-03 09:12', count: members.length, warning: null, members, ...o});
// 未確認の exact（記録なし。602 は除外で選べない）
const EX1 = () => grp('exact', 11, 'aaaaaaaaaaaaaaaa', 'unconfirmed', [
  mem(601, {rank: 1, score: 88.5, recommended: true, effective: true, reasons: ['ピントが合っている']}),
  mem(602, {rank: 2, score: 71.5, favorite: true, hidden: true, selectable: false, issues: ['ブレ']}),
]);
// 再確認の similar（611 が人の代表、613 は記録が無効、613 は画像なし）
const SM1 = () => grp('similar', 21, 'bbbbbbbbbbbbbbbb', 'recheck', [
  mem(611, {rank: 2, score: 70, curation: 'representative', effective: true}),
  mem(612, {rank: 1, score: 85, recommended: true}),
  mem(613, {rank: 3, score: null, curation: 'stale', thumb: '', thumb_url: ''}),
], {taken_at: '2030-05-02 08:00'});
// 確認済み（人の代表なし）。有効な推奨がフィードに出せない
const SM2 = () => grp('similar', 22, 'cccccccccccccccc', 'confirmed', [
  mem(621, {rank: 1, recommended: true, effective: true, hidden: true, selectable: false, curation: 'reviewed'}),
  mem(622, {rank: 2, curation: 'reviewed'}),
], {warning: 'representative_not_visible', taken_at: '2030-05-01 07:00'});
// 確認済み（人の代表あり）
const SM3 = () => grp('similar', 23, 'dddddddddddddddd', 'confirmed', [
  mem(631, {rank: 1, curation: 'reviewed'}),
  mem(632, {rank: 2, effective: true, curation: 'representative'}),
], {taken_at: '2030-04-30 06:00'});
const DEFAULT_GROUPS = () => [EX1(), SM1(), SM2(), SM3()];
const listBody = (groups, o = {}) =>
  ({ok: true, page: 1, per: 20, pages: 1, total: groups.length, groups, ...o});

function makeEnv({pathname = '/admin'} = {}){
  const document = buildAdminDom();
  const calls = [];
  const flags = {};
  const handlers = {groups: null, confirm: null, reset: null};   // (params | body, rec) => Promise | undefined
  const fetchFake = (url, opts = {}) => {
    const rec = {url, method: opts.method || 'GET', body: opts.body};
    calls.push(rec);
    const params = url.includes('?') ? new URLSearchParams(url.split('?')[1]) : new URLSearchParams();
    if(url === '/api/admin/status') return reply(statusBody());
    if(url.startsWith('/api/admin/curation/groups')){
      if(handlers.groups){ const r = handlers.groups(params, rec); if(r) return r; }
      return reply(listBody(DEFAULT_GROUPS()));
    }
    if(url === '/api/admin/curation/confirm' || url === '/api/admin/curation/reset'){
      const h = url.endsWith('confirm') ? handlers.confirm : handlers.reset;
      const r = h && h(JSON.parse(opts.body), rec);
      return r || reply({ok: false, error: 'unexpected ' + url});
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
    setInterval(){}, setTimeout(){ return 0; }, clearTimeout(){}, console,
  });
  loadScripts(context);
  const reqs = prefix => calls.filter(c => c.url.startsWith(prefix));
  const listReqs = () => reqs('/api/admin/curation/groups');
  const lastList = () => {
    const c = listReqs().at(-1);
    return c ? new URLSearchParams(c.url.split('?')[1] || '') : null;
  };
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  const ev = code => JSON.parse(vm.runInContext('JSON.stringify(' + code + ')', context));
  const run = code => vm.runInContext(code, context);       // const の状態（cs）は context のプロパティではないので、式で読む
  return {document, context, calls, handlers, reqs, listReqs, lastList, boot, ev, run, $: s => document.querySelector(s),
          $$: s => document.querySelectorAll(s)};
}

const hidden = (env, sel) => env.$(sel).classList.contains('hidden');
const openCur = async env => { env.$('.tab[data-tab=curation]').click(); await settle(); };
const sections = env => env.$$('#cur-groups .cur-group');
const sectionOf = (env, kind, id) => sections(env).find(s => s.dataset.key === kind + ':' + id);
const cardOf = (section, id) => section.querySelectorAll('.cur-member').find(l => l.dataset.id === String(id));
const tagsOf = card => card.querySelectorAll('.cur-tags .tag').map(t => t.textContent);
const body = rec => JSON.parse(rec.body);
const activeTabs = env => env.$$('nav .tab').filter(t => t.classList.contains('on')).map(t => t.dataset.tab);
const plain = v => JSON.parse(JSON.stringify(v));
const posts = (env, url) => env.reqs(url).filter(c => c.method === 'POST');
const toastText = env => env.$('#toast').textContent;
const openWith = async (env, groups) => {
  env.handlers.groups = () => reply(listBody(groups));
  await env.boot();
  await openCur(env);
};

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('curation.js を読み込んだだけでは location にも fetch にも触らず、HTML 文字列を組む API も使わない', () => {
  const touched = [];
  const sandbox = {document: buildAdminDom(), setInterval(){}, setTimeout(){}, clearTimeout(){}, console};
  Object.defineProperty(sandbox, 'location', {get(){ touched.push('location'); throw new Error('location'); }});
  Object.defineProperty(sandbox, 'fetch', {get(){ touched.push('fetch'); throw new Error('fetch'); }});
  loadScripts(vm.createContext(sandbox));
  assert.deepEqual(touched, []);
  for(const [text, name] of sources){
    assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(text), name);
  }
});

await test('(a) タブを開くまで読まず、開くと state=unconfirmed&kind=all で読む。段の見出し（種類・枚数・撮影日時・状態）と注意が描かれる', async () => {
  const env = makeEnv();
  await env.boot();
  assert.equal(hidden(env, '#v-curation'), true, '起動時は状況タブ');
  assert.deepEqual(env.listReqs(), []);
  await openCur(env);
  assert.deepEqual(activeTabs(env), ['curation']);
  assert.equal(hidden(env, '#v-curation'), false);
  assert.equal(hidden(env, '#v-status'), true);
  assert.equal(env.listReqs().length, 1);
  const q = env.lastList();
  assert.deepEqual([q.get('state'), q.get('kind'), q.get('page'), q.get('per')], ['unconfirmed', 'all', '1', '20']);
  const s = sections(env);
  assert.equal(s.length, 4);
  assert.deepEqual(s.map(x => x.dataset.key), ['exact:11', 'similar:21', 'similar:22', 'similar:23']);
  const head = x => ['.cur-kind', '.cur-count', '.cur-time', '.cur-state'].map(c => x.querySelector(c).textContent);
  assert.deepEqual(head(s[0]), ['exact', '2 枚', '2030-05-03 09:12', '状態: 未確認']);
  assert.deepEqual(head(s[1]), ['similar', '3 枚', '2030-05-02 08:00', '状態: 再確認']);
  assert.equal(head(s[2])[3], '状態: 確認済み', '人の代表が無い確認済み');
  assert.equal(head(s[3])[3], '状態: 確認済み（人の代表）');
  assert.equal(s[0].querySelector('.cur-warning'), null);
  assert.equal(s[2].querySelector('.cur-warning').textContent, '代表がフィードに出せません', 'warning はサーバーの値のまま');
  assert.equal(env.$('#cur-note').textContent, '4 組');
  assert.equal(env.$('#cur-pageinfo').textContent, '4組 / 1 of 1ページ');
  assert.equal(hidden(env, '#cur-empty'), true);
  assert.ok(env.$('#cur-state-unconfirmed').classList.contains('on'));
  assert.ok(env.$('#cur-kind-all').classList.contains('on'));
});

await test('(b) メンバー: サーバーの順のまま、順位・点数・タグ・画像（thumb_url。画像なしは代わりの表示）・理由と懸念の折りたたみ・無効なチェックボックス', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const [ex, sm1] = sections(env);
  const cards = ex.querySelectorAll('.cur-member');
  assert.deepEqual(cards.map(c => c.dataset.id), ['601', '602']);
  assert.deepEqual(cards.map(c => c.querySelector('.cur-rank').textContent), ['#1', '#2']);
  assert.deepEqual(cards.map(c => c.querySelector('.cur-score').textContent), ['点数 88.5', '点数 71.5']);
  assert.deepEqual(tagsOf(cards[0]), ['解析の推奨']);
  assert.deepEqual(tagsOf(cards[1]), ['★', '除外']);
  assert.ok(cards[0].classList.contains('is-effective'), 'effective はサーバーの値');
  assert.ok(!cards[1].classList.contains('is-effective'));
  assert.equal(cards[0].querySelector('img').src, '/thumb/601.jpg?v=0123456789ab');
  assert.equal(cards[0].querySelector('img').loading, 'lazy');
  // selectable でないメンバーはチェックボックスが無効（画面で計算しない）
  assert.equal(cards[0].querySelector('input').disabled, false);
  assert.equal(cards[1].querySelector('input').disabled, true);
  assert.ok(cards[1].classList.contains('is-unselectable'));
  // 理由・懸念
  assert.equal(cards[0].querySelector('.cur-why summary').textContent, '理由 1・懸念 0');
  assert.deepEqual(cards[0].querySelectorAll('.cur-reasons li').map(x => x.textContent), ['ピントが合っている']);
  assert.deepEqual(cards[1].querySelectorAll('.cur-issues li').map(x => x.textContent), ['ブレ']);
  assert.equal(cards[1].querySelector('.cur-why summary').textContent, '理由 0・懸念 1');
  // 人の代表・再確認のタグ、画像なし・点数なし
  const c2 = sm1.querySelectorAll('.cur-member');
  assert.deepEqual(c2.map(c => c.dataset.id), ['611', '612', '613'], '順位順ではなくサーバーが返した並び');
  assert.deepEqual(tagsOf(c2[0]), ['人の代表']);
  assert.deepEqual(tagsOf(c2[1]), ['解析の推奨']);
  assert.deepEqual(tagsOf(c2[2]), ['再確認']);
  assert.equal(c2[2].querySelector('img'), null);
  assert.equal(c2[2].querySelector('.img-broken').textContent, '画像なし');
  assert.equal(c2[2].querySelector('.cur-score').textContent, '点数 --');
  assert.equal(c2[0].querySelector('.cur-why'), null, '理由も懸念も無ければ折りたたみは出さない');
  // 画像が読めないときも代わりの表示
  const img = cards[0].querySelector('img');
  img.dispatchEvent({type: 'error'});
  assert.equal(cards[0].querySelector('img'), null);
  assert.equal(cards[0].querySelector('.img-broken').textContent, '画像なし');
});

await test('(c) 操作ボタン: 代表にする = チェックが 1 つ以上で有効、「取り消す」= 記録のあるメンバーがいる段だけ、確認済みの段は「このままで確認済み」が無効', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const [ex, sm1, sm2] = sections(env);
  const pick = s => s.querySelector('.cur-do-pick');
  assert.equal(pick(ex).disabled, true, '選ぶまでは無効');
  assert.equal(ex.querySelector('.cur-do-keep').disabled, false);
  assert.equal(ex.querySelector('.cur-do-reset'), null, '記録が無い段に取り消しは出ない');
  assert.ok(sm1.querySelector('.cur-do-reset'));
  assert.ok(sm2.querySelector('.cur-do-reset'), 'reviewed だけでも記録はある');
  assert.equal(sm2.querySelector('.cur-do-keep').disabled, true);
  const box = cardOf(ex, 601).querySelector('input');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  assert.equal(pick(ex).disabled, false);
  assert.equal(ex.querySelector('.cur-pick-note').textContent, '1 枚を選択中');
  box.checked = false;
  box.dispatchEvent({type: 'change'});
  assert.equal(pick(ex).disabled, true);
  assert.equal(ex.querySelector('.cur-pick-note').textContent, '');
});

await test('(d) チェックして「代表にする」: confirm に token 付きの本文が POST され、応答の組でその段だけが描き直される（他の段はそのまま）', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const before = sections(env);
  const [ex, sm1] = before;
  const box = cardOf(sm1, 612).querySelector('input');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  const done = grp('similar', 21, 'eeeeeeeeeeeeeeee', 'confirmed', [
    mem(611, {rank: 2, score: 70, curation: 'reviewed'}),
    mem(612, {rank: 1, score: 85, recommended: true, effective: true, curation: 'representative'}),
    mem(613, {rank: 3, score: null, curation: 'reviewed', thumb: '', thumb_url: ''}),
  ], {taken_at: '2030-05-02 08:00'});
  env.handlers.confirm = () => reply({ok: true, group: done});
  sm1.querySelector('.cur-do-pick').click();
  await settle();
  const sent = posts(env, '/api/admin/curation/confirm');
  assert.equal(sent.length, 1);
  assert.deepEqual(plain(body(sent[0])), {kind: 'similar', group_id: 21, token: 'bbbbbbbbbbbbbbbb', representatives: [612]});
  assert.equal(sent[0].method, 'POST');
  const after = sections(env);
  assert.equal(after.length, 4);
  assert.strictEqual(after[0], ex, '他の段は描き直さない');
  assert.strictEqual(after[2], before[2]);
  assert.notStrictEqual(after[1], sm1);
  assert.equal(after[1].querySelector('.cur-state').textContent, '状態: 確認済み（人の代表）');
  assert.deepEqual(tagsOf(cardOf(after[1], 612)), ['解析の推奨', '人の代表']);
  assert.equal(cardOf(after[1], 612).querySelector('input').checked, false, '選択はその段だけ消える');
  assert.equal(after[1].querySelector('.cur-do-keep').disabled, true, 'token も新しい組のもの');
  assert.equal(env.ev('cs.groups[1].token'), 'eeeeeeeeeeeeeeee');
  assert.ok(toastText(env).includes('代表を確定しました'));
  // 新しい段からの確定は新しい token を送る
  const box2 = cardOf(after[1], 611).querySelector('input');
  box2.checked = true;
  box2.dispatchEvent({type: 'change'});
  env.handlers.confirm = () => reply({ok: true, group: done});
  after[1].querySelector('.cur-do-pick').click();
  await settle();
  assert.equal(body(posts(env, '/api/admin/curation/confirm')[1]).token, 'eeeeeeeeeeeeeeee');
  assert.deepEqual(plain(body(posts(env, '/api/admin/curation/confirm')[1]).representatives), [611]);
});

await test('(d2) 複数選ぶと representatives は段のメンバー順の ID。送信中は段のボタンが無効で、二重に送らない', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const sm1 = sections(env)[1];
  for(const id of [612, 611]){
    const box = cardOf(sm1, id).querySelector('input');
    box.checked = true;
    box.dispatchEvent({type: 'change'});
  }
  assert.equal(sm1.querySelector('.cur-pick-note').textContent, '2 枚を選択中');
  const d = deferred();
  env.handlers.confirm = () => d.promise;
  sm1.querySelector('.cur-do-pick').click();
  assert.equal(sm1.querySelector('.cur-do-pick').disabled, true);
  assert.equal(sm1.querySelector('.cur-do-keep').disabled, true);
  assert.equal(sm1.querySelector('.cur-do-reset').disabled, true);
  sm1.querySelector('.cur-do-pick').click();
  sm1.querySelector('.cur-do-keep').click();
  env.context.curConfirmKeep(env.run('cs.groups[1]'));        // ボタン以外から呼んでも二重に送らない
  await settle();
  assert.equal(posts(env, '/api/admin/curation/confirm').length, 1);
  assert.deepEqual(plain(body(posts(env, '/api/admin/curation/confirm')[0]).representatives), [611, 612]);
  d.resolve({ok: true, status: 200, json: () => Promise.resolve({ok: true, group: grp('similar', 21, '5555555555555555', 'confirmed',
    [mem(611, {curation: 'reviewed'}), mem(612, {curation: 'representative'}), mem(613, {curation: 'reviewed'})])})});
  await settle();
  assert.equal(sections(env).length, 4);
  assert.equal(sections(env)[1].dataset.key, 'similar:21');
  assert.equal(sections(env)[1].querySelector('.cur-state').textContent, '状態: 確認済み（人の代表）');
});

await test('(e) 「このままで確認済み」: 人の代表があればその ID（stale は含めない）、無ければ representatives は null', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const [ex, sm1] = sections(env);
  env.handlers.confirm = b => reply({ok: true, group: grp(b.kind, b.group_id, 'ffffffffffffffff', 'confirmed',
    [mem(601, {curation: 'reviewed'}), mem(602, {curation: 'reviewed', selectable: false})])});
  sm1.querySelector('.cur-do-keep').click();
  await settle();
  let sent = posts(env, '/api/admin/curation/confirm');
  assert.deepEqual(plain(body(sent[0])), {kind: 'similar', group_id: 21, token: 'bbbbbbbbbbbbbbbb', representatives: [611]});
  ex.querySelector('.cur-do-keep').click();
  await settle();
  sent = posts(env, '/api/admin/curation/confirm');
  assert.equal(sent.length, 2);
  assert.deepEqual(plain(body(sent[1])), {kind: 'exact', group_id: 11, token: 'aaaaaaaaaaaaaaaa', representatives: null});
  assert.ok('representatives' in body(sent[1]), 'キーを省かない（null は明示する）');
  assert.ok(toastText(env).includes('確認済みにしました'));
});

await test('(f) 「取り消す」: reset に kind・group_id・token だけを POST し、応答の組（未確認）で描き直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const sm3 = sections(env)[3];
  env.handlers.reset = () => reply({ok: true, group: grp('similar', 23, '1111111111111111', 'unconfirmed',
    [mem(631, {rank: 1, recommended: true, effective: true}), mem(632, {rank: 2})])});
  sm3.querySelector('.cur-do-reset').click();
  await settle();
  const sent = posts(env, '/api/admin/curation/reset');
  assert.equal(sent.length, 1);
  assert.deepEqual(plain(body(sent[0])), {kind: 'similar', group_id: 23, token: 'dddddddddddddddd'});
  const fresh = sections(env)[3];
  assert.equal(fresh.querySelector('.cur-state').textContent, '状態: 未確認');
  assert.equal(fresh.querySelector('.cur-do-reset'), null, '記録が無くなったので取り消しは消える');
  assert.ok(toastText(env).includes('取り消しました'));
});

await test('(g) 409 group_changed: 「組が変わりました」のトースト → state=all&kind=all&ids=<先頭の写真> でその段だけ読み直し、今の絞り込みに合えば描き直す', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const before = sections(env);
  const sm1 = before[1];
  const box = cardOf(sm1, 612).querySelector('input');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  env.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  const rebuilt = grp('similar', 31, '2222222222222222', 'recheck', [mem(611, {rank: 1}), mem(612, {rank: 2}), mem(614, {rank: 3})]);
  env.handlers.groups = params => params.get('ids')
    ? reply(listBody([grp('exact', 99, 'x'.repeat(16), 'unconfirmed', [mem(611), mem(615)]), rebuilt])) : null;
  sm1.querySelector('.cur-do-pick').click();
  await settle();
  assert.ok(toastText(env).includes('組が変わりました。読み直します'), toastText(env));
  assert.ok(env.$('#toast').className.includes('err'));
  const q = env.lastList();
  assert.deepEqual([q.get('ids'), q.get('state'), q.get('kind')], ['611', 'all', 'all']);
  assert.equal(env.listReqs().length, 2, '全体は読み直さない（その段だけ）');
  const after = sections(env);
  assert.equal(after.length, 4);
  assert.strictEqual(after[0], before[0]);
  assert.equal(after[1].dataset.key, 'similar:31', '同じ kind の組を取る（group_id が変わっても拾える）');
  assert.deepEqual(after[1].querySelectorAll('.cur-member').map(c => c.dataset.id), ['611', '612', '614']);
  assert.equal(cardOf(after[1], 612).querySelector('input').checked, false, 'token が変わったので選択は持ち越さない');
  assert.equal(env.ev('cs.groups[1].token'), '2222222222222222');
  assert.equal(after[1].querySelector('.cur-do-pick').disabled, true);
  // ボタンはまた使える（busy が残っていない）
  assert.equal(after[1].querySelector('.cur-do-keep').disabled, false);
});

await test('(g2) 読み直した組が今の絞り込み（未確認）に合わなければ段を外す。応答が空でも外す。件数も減る', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  env.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  // 確認済みになって届いた → 未確認の絞り込みには合わない
  env.handlers.groups = params => params.get('ids')
    ? reply(listBody([grp('exact', 11, '3333333333333333', 'confirmed', [mem(601), mem(602)])])) : null;
  sections(env)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['similar:21', 'similar:22', 'similar:23']);
  assert.equal(env.$('#cur-pageinfo').textContent, '3組 / 1 of 1ページ');
  assert.equal(env.ev('cs.nodes.size'), 3);
  // 組が消えた（空の応答）
  env.handlers.groups = params => params.get('ids') ? reply(listBody([])) : null;
  sections(env)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['similar:22', 'similar:23']);
  // 同じ kind の組が無い（別の kind だけ）も外す
  env.handlers.groups = params => params.get('ids') ? reply(listBody([EX1()])) : null;
  env.handlers.reset = () => respond(409, {ok: false, error: 'group_changed'});
  sections(env)[0].querySelector('.cur-do-reset').click();       // similar:22（確認済みは keep が無効なので取り消しで）
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['similar:23']);
  // 絞り込みが「すべて」なら確認済みになった組も描き直す
  env.$('#cur-state-all').click();
  await settle();
  env.handlers.groups = params => params.get('ids')
    ? reply(listBody([grp('exact', 11, '4444444444444444', 'confirmed', [mem(601), mem(602)])])) : null;
  sections(env)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.equal(sections(env)[0].querySelector('.cur-state').textContent, '状態: 確認済み');
});

await test('(g3) 段を外してページが空になり、まだ件数が残っているなら一覧を読み直す。全部無くなれば「該当する組がありません」', async () => {
  const env = makeEnv();
  await openWith(env, [EX1()]);
  env.handlers.groups = params => params.get('ids') ? reply(listBody([])) : reply(listBody([EX1()], {total: 21, pages: 2, page: 2}));
  env.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  // total 1 のまま全部消える → 空の表示
  sections(env)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.equal(sections(env).length, 0);
  assert.equal(hidden(env, '#cur-empty'), false);
  assert.equal(env.listReqs().length, 2, '件数が残っていないので全体は読み直さない');
  // 総数が多いとき: ページが空になったらサーバーに合わせて読み直す
  const env2 = makeEnv();
  env2.handlers.groups = () => reply(listBody([EX1()], {total: 21, pages: 2, page: 2}));
  await env2.boot();
  await openCur(env2);
  env2.handlers.groups = params => params.get('ids') ? reply(listBody([])) : reply(listBody([SM1()], {total: 20, pages: 1, page: 1}));
  env2.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  sections(env2)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.equal(env2.listReqs().length, 3);
  assert.equal(env2.lastList().get('ids'), null);
  assert.deepEqual(sections(env2).map(s => s.dataset.key), ['similar:21']);
  assert.equal(env2.$('#cur-pageinfo').textContent, '20組 / 1 of 1ページ');
});

await test('(h) 400 representative_not_visible は「代表に選べない写真があります」のトースト（段は残り、読み直して selectable を合わせる）。その他の失敗は失敗のトースト', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const sm1 = sections(env)[1];
  const box = cardOf(sm1, 612).querySelector('input');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  env.handlers.confirm = () => respond(400, {ok: false, error: 'representative_not_visible'});
  env.handlers.groups = params => params.get('ids') ? reply(listBody([grp('similar', 21, 'bbbbbbbbbbbbbbbb', 'recheck', [
    mem(611, {rank: 2, curation: 'representative', effective: true}), mem(612, {rank: 1, selectable: false, hidden: true}),
    mem(613, {rank: 3, curation: 'stale'})], {taken_at: '2030-05-02 08:00'})])) : null;
  sm1.querySelector('.cur-do-pick').click();
  await settle();
  assert.equal(toastText(env), '代表に選べない写真があります');
  const fresh = sections(env)[1];
  assert.equal(fresh.dataset.key, 'similar:21');
  assert.equal(cardOf(fresh, 612).querySelector('input').disabled, true, '読み直しで selectable がサーバーの値になる');
  assert.equal(cardOf(fresh, 612).querySelector('input').checked, false, 'もう選べない写真の選択は外れる');
  assert.equal(fresh.querySelector('.cur-do-pick').disabled, true);
  // その他の失敗
  env.handlers.confirm = () => respond(500, {ok: false, error: 'boom'});
  fresh.querySelector('.cur-do-keep').click();
  await settle();
  assert.equal(toastText(env), '選別の更新に失敗: boom');
  assert.equal(sections(env)[1], fresh, '段は描き直さない');
  assert.equal(fresh.querySelector('.cur-do-keep').disabled, false, 'もう一度押せる');
  // 通信エラー（JSON でない）
  env.handlers.reset = () => respond(503, undefined);
  sections(env)[1].querySelector('.cur-do-reset').click();
  await settle();
  assert.ok(toastText(env).startsWith('選別の更新に失敗:'), toastText(env));
});

await test('(i) 画像を押すとビューアが開く（チェックボックスは切り替えない・AI 評価は出さない）。メンバーの順に前後へ動ける', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const [ex] = sections(env);
  const thumb = cardOf(ex, 602).querySelector('.cur-thumb');
  const notCancelled = thumb.dispatchEvent({type: 'click'});
  assert.equal(notCancelled, false, 'label の既定動作（チェックの切り替え）を止める');
  assert.equal(hidden(env, '#viewer'), false);
  assert.ok(env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), 1);
  assert.equal(env.$('#viewer-title').textContent, '#602  p602.jpg');
  assert.equal(env.$('#viewer-counter').textContent, '2 / 2');
  assert.equal(hidden(env, '#viewer-ai'), true);
  assert.equal(hidden(env, '#viewer-group'), true);
  assert.equal(env.ev('viewerEntries.length'), 2);
  env.$('#viewer-next').click();
  assert.equal(env.$('#viewer-title').textContent, '#601  p601.jpg');
  // 閉じるとフォーカスが画像のボタンへ戻る
  env.$('#viewer-close').click();
  assert.equal(hidden(env, '#viewer'), true);
  assert.strictEqual(env.document.activeElement, thumb);
  assert.equal(cardOf(ex, 602).querySelector('input').checked, false);
});

await test('(j) ビューアの★: setflag にその写真の id が送られ、段のメンバーのタグだけが変わる（selectable・effective・warning はサーバーの値のまま、読み直さない）', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const ex = sections(env)[0];
  cardOf(ex, 601).querySelector('.cur-thumb').click();
  env.$('#viewer-favorite').click();
  await settle();
  const flagReq = env.reqs('/api/admin/setflag');
  assert.equal(flagReq.length, 1);
  assert.equal(body(flagReq[0]).id, 601);
  assert.deepEqual(tagsOf(cardOf(ex, 601)), ['解析の推奨', '★']);
  assert.deepEqual(tagsOf(cardOf(ex, 602)), ['★', '除外'], '他のメンバーは変わらない');
  assert.equal(cardOf(ex, 601).querySelector('input').disabled, false);
  assert.ok(cardOf(ex, 601).classList.contains('is-effective'));
  assert.equal(env.listReqs().length, 1, '★だけなら読み直さない');
  // もう一度押すと外れる
  env.$('#viewer-favorite').click();
  await settle();
  assert.deepEqual(tagsOf(cardOf(ex, 601)), ['解析の推奨']);
  assert.equal(env.listReqs().length, 1);
  assert.strictEqual(sections(env)[0], ex);
});

await test('(j2) 除外が変わると、その段だけを ids= で読み直して selectable・warning をサーバーの値に合わせる。選択のうち選べなくなった写真だけが外れる', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const before = sections(env);
  const sm1 = before[1];
  for(const id of [611, 612]){
    const box = cardOf(sm1, id).querySelector('input');
    box.checked = true;
    box.dispatchEvent({type: 'change'});
  }
  cardOf(sm1, 612).querySelector('.cur-thumb').click();
  env.handlers.groups = params => params.get('ids') ? reply(listBody([grp('similar', 21, 'bbbbbbbbbbbbbbbb', 'recheck', [
    mem(611, {rank: 2, score: 70, curation: 'representative', effective: true}),
    mem(612, {rank: 1, score: 85, recommended: true, hidden: true, selectable: false}),
    mem(613, {rank: 3, score: null, curation: 'stale', thumb: '', thumb_url: ''})],
    {taken_at: '2030-05-02 08:00', warning: 'representative_not_visible'})])) : null;
  env.$('#viewer-hidden').click();
  await settle();
  assert.equal(env.listReqs().length, 2);
  const q = env.lastList();
  assert.deepEqual([q.get('ids'), q.get('state'), q.get('kind')], ['611', 'all', 'all']);
  const after = sections(env);
  assert.strictEqual(after[0], before[0]);
  assert.notStrictEqual(after[1], sm1);
  assert.equal(after[1].querySelector('.cur-warning').textContent, '代表がフィードに出せません');
  assert.equal(cardOf(after[1], 612).querySelector('input').disabled, true);
  assert.deepEqual(tagsOf(cardOf(after[1], 612)), ['解析の推奨', '除外']);
  assert.equal(cardOf(after[1], 611).querySelector('input').checked, true, '選べる写真の選択は残る');
  assert.equal(cardOf(after[1], 612).querySelector('input').checked, false);
  assert.equal(after[1].querySelector('.cur-do-pick').disabled, false);
  assert.equal(after[1].querySelector('.cur-pick-note').textContent, '1 枚を選択中');
  // 読み直したあとの★は、新しい段のメンバーにも反映される（開いているビューアの写真は古い object のまま）
  env.context.applyPhoto(611, {favorite: true, hidden: false});
  assert.deepEqual(tagsOf(cardOf(sections(env)[1], 611)), ['人の代表', '★']);
});

await test('(k) 絞り込み: 状態・種類の切り替えで state/kind 付きで page=1 から読み直し、選択中の表示が切り替わる。選択は持ち越さない', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const box = cardOf(sections(env)[0], 601).querySelector('input');
  box.checked = true;
  box.dispatchEvent({type: 'change'});
  env.$('#cur-state-confirmed').click();
  await settle();
  let q = env.lastList();
  assert.deepEqual([q.get('state'), q.get('kind'), q.get('page')], ['confirmed', 'all', '1']);
  assert.ok(env.$('#cur-state-confirmed').classList.contains('on'));
  assert.ok(!env.$('#cur-state-unconfirmed').classList.contains('on'));
  env.$('#cur-kind-exact').click();
  await settle();
  q = env.lastList();
  assert.deepEqual([q.get('state'), q.get('kind')], ['confirmed', 'exact']);
  assert.ok(env.$('#cur-kind-exact').classList.contains('on'));
  assert.ok(!env.$('#cur-kind-all').classList.contains('on'));
  env.$('#cur-kind-similar').click();
  await settle();
  assert.equal(env.lastList().get('kind'), 'similar');
  env.$('#cur-state-all').click();
  env.$('#cur-kind-all').click();
  await settle();
  q = env.lastList();
  assert.deepEqual([q.get('state'), q.get('kind')], ['all', 'all']);
  assert.equal(env.ev('cs.picks.size'), 0);
  assert.equal(cardOf(sections(env)[0], 601).querySelector('input').checked, false);
  // 絞り込みに合う組が無い → 空の表示
  env.handlers.groups = () => reply(listBody([]));
  env.$('#cur-state-confirmed').click();
  await settle();
  assert.equal(sections(env).length, 0);
  assert.equal(hidden(env, '#cur-empty'), false);
  assert.equal(env.$('#cur-empty').textContent, '該当する組がありません');
  assert.equal(env.$('#cur-note').textContent, '0 組');
});

await test('(l) ページング: 次へ・前へで page を付けて読み直す。端では無効。絞り込みを変えると 1 ページ目へ', async () => {
  const env = makeEnv();
  env.handlers.groups = params => {
    const page = Number(params.get('page')) || 1;
    return reply(listBody([EX1()], {page, pages: 3, total: 41}));
  };
  await env.boot();
  await openCur(env);
  assert.equal(env.$('#cur-pageinfo').textContent, '41組 / 1 of 3ページ');
  assert.equal(env.$('#cur-prev').disabled, true);
  assert.equal(env.$('#cur-next').disabled, false);
  env.$('#cur-next').click();
  await settle();
  assert.equal(env.lastList().get('page'), '2');
  assert.equal(env.$('#cur-pageinfo').textContent, '41組 / 2 of 3ページ');
  env.$('#cur-next').click();
  await settle();
  assert.equal(env.lastList().get('page'), '3');
  assert.equal(env.$('#cur-next').disabled, true);
  env.$('#cur-next').click();
  await settle();
  assert.equal(env.listReqs().length, 3, '最後のページでは読まない');
  env.$('#cur-prev').click();
  await settle();
  assert.equal(env.lastList().get('page'), '2');
  env.$('#cur-kind-exact').click();
  await settle();
  assert.equal(env.lastList().get('page'), '1');
});

await test('(m) 古い応答は捨てる: 絞り込みを続けて変えたとき、遅れて届いた前の応答で一覧を上書きしない', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const first = deferred(), second = deferred();
  const queue = [first, second];
  env.handlers.groups = () => queue.shift().promise;
  env.$('#cur-state-confirmed').click();
  env.$('#cur-kind-exact').click();
  second.resolve({ok: true, status: 200, json: () => Promise.resolve(listBody([EX1()]))});
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['exact:11']);
  first.resolve({ok: true, status: 200, json: () => Promise.resolve(listBody([SM3(), SM2()]))});
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['exact:11'], '先の応答は捨てる');
  assert.ok(env.$('#cur-kind-exact').classList.contains('on'));
});

await test('(m2) 一覧が読み直されたら、飛んでいた 1 組の読み直しの応答は捨てる（別のページ・絞り込みへ段を戻さない）', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const d = deferred();
  env.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  env.handlers.groups = params => params.get('ids') ? d.promise : null;
  sections(env)[1].querySelector('.cur-do-keep').click();
  await settle();
  env.handlers.groups = () => reply(listBody([EX1()]));
  env.$('#cur-kind-exact').click();
  await settle();
  d.resolve({ok: true, status: 200, json: () => Promise.resolve(listBody([SM1()]))});
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['exact:11']);
});

await test('(n) 確定の応答が一覧の読み直しのあとに届いても、そこに無い段は触らない', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  const d = deferred();
  env.handlers.confirm = () => d.promise;
  sections(env)[1].querySelector('.cur-do-keep').click();
  env.handlers.groups = () => reply(listBody([EX1()]));
  env.$('#cur-kind-exact').click();
  await settle();
  d.resolve({ok: true, status: 200, json: () => Promise.resolve({ok: true, group: SM3()})});
  await settle();
  assert.deepEqual(sections(env).map(s => s.dataset.key), ['exact:11']);
  assert.equal(env.ev('cs.busy.size'), 0);
});

await test('(o) 一覧の読み込み失敗はトーストで知らせ、一覧は前のまま', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  env.handlers.groups = () => respond(500, {ok: false, error: 'boom'});
  env.$('#cur-kind-exact').click();
  await settle();
  assert.equal(toastText(env), '選別の組を読めませんでした: boom');
  assert.equal(sections(env).length, 4);
  // 1 組の読み直しの失敗
  env.handlers.confirm = () => respond(409, {ok: false, error: 'group_changed'});
  env.handlers.groups = () => respond(500, {ok: false, error: 'down'});
  sections(env)[0].querySelector('.cur-do-keep').click();
  await settle();
  assert.equal(toastText(env), '組を読み直せませんでした: down');
  assert.equal(sections(env).length, 4);
});

await test('(p) タブを離れるとビューアが閉じ、戻ると開き直して読み直す（絞り込みは前のまま）。状況・写真・ライブラリのタブも切り替わる', async () => {
  const env = makeEnv();
  await env.boot();
  await openCur(env);
  env.$('#cur-state-all').click();
  await settle();
  cardOf(sections(env)[0], 601).querySelector('.cur-thumb').click();
  assert.equal(hidden(env, '#viewer'), false);
  env.$('.tab[data-tab=status]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.ok(!env.document.body.classList.contains('ps-viewer-open'));
  assert.equal(env.ev('viewerIndex'), -1);
  assert.equal(hidden(env, '#v-curation'), true);
  const n0 = env.listReqs().length;
  await openCur(env);
  assert.equal(env.listReqs().length, n0 + 1);
  assert.equal(env.lastList().get('state'), 'all');
  assert.ok(env.$('#cur-state-all').classList.contains('on'));
  // 他のタブへ移るときも
  cardOf(sections(env)[0], 601).querySelector('.cur-thumb').click();
  env.$('.tab[data-tab=config]').click();
  await settle();
  assert.equal(hidden(env, '#viewer'), true);
  assert.deepEqual(activeTabs(env), ['config']);
  env.$('.tab[data-tab=library]').click();
  await settle();
  assert.equal(hidden(env, '#v-curation'), true);
  assert.equal(hidden(env, '#v-library'), false);
});

await test('(q) .hidden で出し入れする要素の display は、ID・クラスの規則が .hidden に勝たないよう、対の .hidden 規則を持つ', () => {
  const css = fs.readFileSync(new URL('../static/admin.css', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
  const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(m => [m[1].trim(), m[2]]);
  assert.ok(rules.some(([sel, body]) => sel === '.hidden' && /display:\s*none/.test(body)));
  const html = fs.readFileSync(new URL('../static/admin.html', import.meta.url), 'utf8');
  // curation.js が .hidden を付け外しする要素（id）と、その class
  const toggled = ['cur-empty', 'v-curation', 'viewer-noimage'];
  const selectorsOf = id => {
    const tag = html.match(new RegExp('<[^>]*id="' + id + '"[^>]*>'));
    assert.ok(tag, id);
    const cls = (tag[0].match(/class="([^"]*)"/) || [, ''])[1].split(/\s+/).filter(c => c && c !== 'hidden');
    return ['#' + id, ...cls.map(c => '.' + c)];
  };
  const has = (sel, re) => rules.some(([rsel, b]) => rsel.split(',').map(x => x.trim()).includes(sel) && re.test(b));
  for(const id of toggled){
    for(const sel of selectorsOf(id)){
      if(has(sel, /(^|;)\s*display:/)){
        assert.ok(has(sel + '.hidden', /display:\s*none/), sel + ' に display があるのに ' + sel + '.hidden{display:none} が無い');
      }
    }
  }
  // 逆向き: #cur-* / .cur-* の display を持つ規則のうち、.hidden を付ける要素のものは対を持つ
  const curDisplay = rules.filter(([sel, b]) => /^[#.]cur-[\w-]+$/.test(sel) && /(^|;)\s*display:/.test(b)).map(([sel]) => sel);
  for(const sel of curDisplay){
    if(toggled.some(id => selectorsOf(id).includes(sel))) assert.ok(has(sel + '.hidden', /display:\s*none/), sel);
  }
  // 段・一覧・メンバーは .hidden を付けない（display を持ってよい）。付ける要素だけが対象であることの確認
  assert.ok(curDisplay.includes('#cur-groups'));
  assert.ok(!toggled.includes('cur-groups'));
});

await test('(r) 状況タブの「未確認の選別」を押すと選別タブへ移る（curation_pending があるときだけ出る）', async () => {
  const env = makeEnv();
  const orig = env.context.fetch;
  env.context.fetch = (url, opts) => {
    if(url !== '/api/admin/status') return orig(url, opts);
    const b = statusBody();
    b.db.quality = {features: 1, pending: 0, analyzed: 4, recommended: 2, exact_suppressed: 1, similar_alternates: 1,
                    curation_pending: 7};
    return reply(b);
  };
  await env.boot();
  const todo = env.$('#s-todo').children.find(c => c.textContent.includes('未確認の選別'));
  assert.ok(todo, '要対応に出る');
  assert.equal(todo.textContent, '未確認の選別7組');
  assert.ok(todo.classList.contains('warn') && todo.classList.contains('todo'));
  const dbCell = env.$('#s-db').children.find(c => c.textContent.includes('未確認の選別'));
  assert.ok(dbCell && dbCell.classList.contains('warn'));
  todo.click();
  await settle();
  assert.deepEqual(activeTabs(env), ['curation']);
  assert.equal(hidden(env, '#v-curation'), false);
  assert.equal(env.lastList().get('state'), 'unconfirmed');
  assert.equal(sections(env).length, 4);
});

console.log(`ok (${n})`);
