// admin.js・photos.js・ops.js（この順に同じ context へ読む。boot() が initPhotos() を呼ぶので photos.js も要る）の運用タブ — 操作のボタン（取り込み有効 / 無効）、送信中・実行中・待ちの disabled、
// 終了した記録の段の表、大量削除の保留の 2 段階の確認（件数と範囲の文言・409 で閉じて読み直し・読み直しで印が変わると閉じる・
// 一覧が空のための保留は案内だけ）、定期サイクルの直近の結果と次の予定、履歴 10 件の順序、古い応答の破棄、タブを離れると
// 読み直しが止まること — を、fakedom.mjs の DOM の代用品と fetch の代用品で動かして確かめる。
// 使い方: node web/tests/ops_render.mjs <admin.js のパス> <photos.js のパス> <ops.js のパス>（cargo test --test admin_ui_ops 経由で回す）
// fetch は URL ごとの合成 JSON を返すだけで、ネットワークには出ない。setInterval・clearInterval は記録するだけ（タイマーは動かさない）。
import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom} from './fakedom.mjs';

const [adminPath, photosPath, opsPath] = process.argv.slice(2);
assert.ok(adminPath && photosPath && opsPath, 'admin.js・photos.js・ops.js のパスが要る');
const sources = [[adminPath, 'admin.js'], [photosPath, 'photos.js'], [opsPath, 'ops.js']].map(([p, name]) => [fs.readFileSync(p, 'utf8'), name]);
const loadScripts = context => sources.forEach(([text, name]) => vm.runInContext(text, context, {filename: name}));

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const respond = (status, body) =>
  Promise.resolve({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body)});
const reply = body => respond(200, body);
const clone = v => JSON.parse(JSON.stringify(v));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return {promise, resolve}; };
// ノードの同一性は assert.ok で比べる（assert.strictEqual にノードを渡すと、失敗時の差分作りが文書全体を inspect して重い）
const same = (a, b, msg) => assert.ok(a === b, msg || '同じノードのはず');
const notSame = (a, b, msg) => assert.ok(a !== b, msg || '別のノードのはず');

// ---- 合成データ ----
const statusBody = () => ({
  ok: true, server_time: '2030-01-01 10:00:00',
  sync: {ready: 10, pending: 0, total: 10},
  db: {no_loc: 0, no_display: 0, favorite: 0, hidden: 0, enrich_pending: 0, no_time: 0, by_loc_source: {exif: 10},
       infer_buckets: {}, by_source: {drive: 10}, filters: {all: 10, noloc: 0, suggest: 0, inferred: 0, manual: 0, fav: 0, hidden: 0}},
  disk: {used_pct: 10, free_bytes: 1e11, total_bytes: 2e11,
         dirs: {derivatives: {bytes: 1e6, files: 10}, tile_cache: {bytes: 1e3, files: 1}}},
});
const STAGES_OK = () => [
  {name: 'index_local', result: 'done', counts: {indexed: 3, unchanged: 7}},
  {name: 'index_drive', result: 'skipped', reason: 'Drive の鍵がありません'},
  {name: 'quality', result: 'failed'},
];
const CYCLE = () => ({
  id: 0, kind: 'cycle', started_at: '2030-01-01 09:50:00', finished_at: '2030-01-01 09:50:07', outcome: 'partial',
  stages: STAGES_OK(),
});
const rec = (id, o = {}) => ({
  id, kind: 'sync', requested_at: '2030-01-01 09:' + String(10 + id).padStart(2, '0') + ':00',
  started_at: '2030-01-01 09:' + String(10 + id).padStart(2, '0') + ':01',
  finished_at: '2030-01-01 09:' + String(10 + id).padStart(2, '0') + ':09', outcome: 'done',
  stages: [{name: 'index_local', result: 'done', counts: {indexed: id}}], ...o,
});
const HOLD = (o = {}) => ({
  held: 120, reason: 'ratio', drive_rows: 500, since: '2030-01-01 08:00:00', checked_at: '2030-01-01 09:50:00',
  token: '9f3a9f3a9f3a9f3a', releasable: true, ...o,
});
const opsBody = (o = {}) => ({
  ok: true, ingest: true, poll_minutes: 10,
  worker: {running: null, queued: null, next_cycle_at: '2030-01-01 10:00:00', last_cycle: CYCLE()},
  hold: null, ai: {retry_kinds: []}, history: [], ...o,
});
const withWorker = (w, o = {}) => opsBody({worker: {...opsBody().worker, ...w}, ...o});

function makeEnv(initial = opsBody()){
  const document = buildAdminDom();
  const calls = [];
  const state = {body: initial};
  const handlers = {ops: null, sync: null, quality: null, release: null};   // (rec) => Promise | undefined
  const intervals = [];
  const cleared = [];
  const fetchFake = (url, opts = {}) => {
    const r = {url, method: opts.method || 'GET', body: opts.body};
    calls.push(r);
    // 解決済みの応答で読み直しを繰り返すループがあると calls が膨らむ
    if(calls.length > 2000) throw new Error('fetch が 2000 回を超えた（読み直しのループ）: ' + url);
    if(url === '/api/admin/status') return reply(statusBody());
    if(url === '/api/admin/ops'){
      const h = handlers.ops && handlers.ops(r);
      return h || reply(clone(state.body));
    }
    const post = {'/api/admin/ops/sync': 'sync', '/api/admin/ops/quality-scan': 'quality',
                  '/api/admin/ops/drive-release': 'release'}[url];
    if(post){
      const h = handlers[post] && handlers[post](r);
      return h || reply({ok: true, request: {id: 1, kind: post, state: 'queued'}});
    }
    return reply({ok: false, error: 'unexpected ' + url});
  };
  const context = vm.createContext({
    document, fetch: fetchFake, location: {pathname: '/admin', origin: 'http://localhost'},
    setInterval(fn, ms){ intervals.push({fn, ms}); return intervals.length; },
    clearInterval(id){ cleared.push(id); },
    setTimeout(){ return 0; }, clearTimeout(){}, console,
  });
  loadScripts(context);
  const $ = s => document.querySelector(s);
  const $$ = s => document.querySelectorAll(s);
  return {
    document, context, calls, state, handlers, intervals, cleared, $, $$,
    boot: async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); },
    open: async () => { $('.tab[data-tab=ops]').click(); await settle(); },
    leave: async () => { $('.tab[data-tab=status]').click(); await settle(); },
    // 3 秒ごとの読み直し（直近に登録された ops の周期を 1 回進める）
    tick: async () => { const t = intervals.filter(i => i.ms === 3000).at(-1); t.fn(); await settle(); },
    gets: () => calls.filter(c => c.url === '/api/admin/ops' && c.method === 'GET'),
    posts: url => calls.filter(c => c.url === url && c.method === 'POST'),
    run: code => vm.runInContext(code, context),
    text: () => $('#v-ops').textContent,
    toastText: () => $('#toast').textContent,
    toastErr: () => $('#toast').className.includes('err'),
  };
}
const bootOpen = async (body) => { const env = makeEnv(body); await env.boot(); await env.open(); return env; };
const hidden = (env, sel) => env.$(sel).classList.contains('hidden');
const recIds = (env, sel) => env.$$(sel + ' .op-rec').map(r => Number(r.dataset.id));

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('ops.js を読み込んだだけでは fetch にも location にも触らず、HTML 文字列を組む API も使わない', () => {
  const ctx = vm.createContext({
    document: buildAdminDom(), console,
    fetch(){ throw new Error('fetch に触った'); },
    get location(){ throw new Error('location に触った'); },
  });
  loadScripts(ctx);
  const text = sources.map(([t]) => t).join('\n');
  for(const bad of ['innerHTML', 'outerHTML', 'insertAdjacentHTML', 'document.write', 'confirm(']) assert.ok(!text.includes(bad), bad);
});

await test('AC1: 取り込み有効なら操作のボタンが 2 つ（保留が無ければ解除は出ない）、定期サイクルと履歴の区画も出る', async () => {
  const env = await bootOpen();
  assert.ok(!hidden(env, '#v-ops'));
  assert.ok(hidden(env, '#v-status'));
  assert.ok(hidden(env, '#o-readonly'));
  assert.ok(!hidden(env, '#o-actions'));
  const buttons = env.$$('#v-ops button');
  assert.deepEqual(buttons.map(b => b.id), ['o-sync', 'o-quality']);
  assert.deepEqual(buttons.map(b => b.textContent), ['同期をいま回す', '品質解析を一括で回す']);
  assert.ok(buttons.every(b => !b.disabled));
  assert.ok(hidden(env, '#o-hold-panel'));
  assert.ok(!hidden(env, '#o-cycle-panel') && !hidden(env, '#o-history-panel'));
  assert.equal(env.gets().length, 1, '開いたときに 1 回読む');
  assert.deepEqual(env.intervals.filter(i => i.ms === 3000).length, 1);
});

await test('AC1: 保留があれば「解除する…」が 3 つ目のボタンとして出る', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  assert.ok(!hidden(env, '#o-hold-panel'));
  assert.deepEqual(env.$$('#v-ops button').map(b => b.id), ['o-sync', 'o-quality', 'o-hold-open']);
  assert.equal(env.$('#o-hold-open').textContent, '解除する…');
  assert.ok(env.text().includes('120'));
});

await test('AC1: ingest:false は案内だけで、ボタンは押せず、保留・サイクル・履歴の区画も出ない', async () => {
  const env = await bootOpen({ok: true, ingest: false, worker: null, hold: null, ai: null, history: []});
  assert.ok(!hidden(env, '#o-readonly'));
  assert.ok(env.$('#o-readonly').textContent.includes('取り込みが無効なので運用操作はできない（読み取り専用の起動）'));
  assert.ok(hidden(env, '#o-actions'));
  assert.ok(env.$('#o-sync').disabled && env.$('#o-quality').disabled);
  assert.ok(hidden(env, '#o-hold-panel') && hidden(env, '#o-cycle-panel') && hidden(env, '#o-history-panel'));
  env.$('#o-sync').click();
  await settle();
  assert.equal(env.posts('/api/admin/ops/sync').length, 0, 'disabled のボタンは POST を出さない');
});

await test('読み込み前（最初の応答が来るまで）は、ボタンを押せず「読み込み中」を出す', async () => {
  const env = makeEnv();
  const d = deferred();
  env.handlers.ops = () => d.promise;
  await env.boot();
  await env.open();
  assert.ok(env.$('#o-sync').disabled && env.$('#o-quality').disabled);
  assert.ok(env.text().includes('読み込み中'));
  d.resolve({ok: true, status: 200, json: () => Promise.resolve(opsBody())});
  await settle();
  assert.ok(!env.$('#o-sync').disabled);
  assert.ok(!env.text().includes('読み込み中'));
});

await test('AC2(c): sync の POST を出した直後に同じボタンが disabled になり、応答のあと読み直して外れる（品質解析は押せる）', async () => {
  const env = await bootOpen();
  const d = deferred();
  env.handlers.sync = () => d.promise;
  env.$('#o-sync').click();
  assert.ok(env.$('#o-sync').disabled, 'POST を出した直後（応答前）に無効');
  assert.ok(!env.$('#o-quality').disabled, '別の種類は押せる');
  const sent = env.posts('/api/admin/ops/sync');
  assert.equal(sent.length, 1);
  assert.deepEqual(JSON.parse(sent[0].body), {});
  // 連打: click イベントを直接送っても 2 本目は出ない
  env.$('#o-sync').dispatchEvent({type: 'click'});
  await settle();
  assert.equal(env.posts('/api/admin/ops/sync').length, 1);
  const before = env.gets().length;
  d.resolve({ok: true, status: 200, json: () => Promise.resolve({ok: true, request: {id: 5, kind: 'sync', state: 'queued'}})});
  await settle();
  assert.ok(env.toastText().includes('同期を受け付けました') && !env.toastErr());
  assert.equal(env.gets().length, before + 1, '成功したら即読み直す');
  assert.ok(!env.$('#o-sync').disabled, '読み直しが済めば（待ち・実行中が無ければ）外れる');
});

await test('AC2(c): 待ち・実行中の応答でも同じ種類のボタンだけが disabled、別の種類は押せる', async () => {
  const env = await bootOpen(withWorker({
    running: {id: 7, kind: 'sync', started_at: '2030-01-01 10:00:01'},
    queued: {id: 8, kind: 'quality_scan', requested_at: '2030-01-01 10:00:02'},
  }));
  assert.ok(env.$('#o-sync').disabled);
  assert.ok(env.$('#o-quality').disabled);
  assert.ok(env.text().includes('実行中: 同期 #7（開始 2030-01-01 10:00:01）'));
  assert.ok(env.text().includes('待ち: 品質解析（一括） #8（受付 2030-01-01 10:00:02）'));
  env.state.body = withWorker({running: {id: 8, kind: 'quality_scan', started_at: '2030-01-01 10:00:09'}});
  await env.tick();
  assert.ok(!env.$('#o-sync').disabled);
  assert.ok(env.$('#o-quality').disabled);
  // 定期サイクルの実行中は、どのボタンも止めない
  env.state.body = withWorker({running: {id: 0, kind: 'cycle', started_at: '2030-01-01 10:00:10'}, next_cycle_at: null});
  await env.tick();
  assert.ok(!env.$('#o-sync').disabled && !env.$('#o-quality').disabled);
  assert.ok(env.text().includes('実行中: 定期サイクル（開始 2030-01-01 10:00:10）'));
});

await test('AC2(c): 終了の応答（履歴）で、段の表（名前・結果・理由・件数）が描かれる', async () => {
  const env = await bootOpen(withWorker({running: {id: 3, kind: 'sync', started_at: '2030-01-01 10:00:01'}}));
  assert.equal(env.$$('#o-history .op-rec').length, 0);
  assert.ok(env.text().includes('まだ操作の履歴はありません'));
  env.state.body = opsBody({history: [rec(3, {kind: 'sync', outcome: 'partial', stages: STAGES_OK().concat([
    {name: 'fetch', result: 'panicked'}, {name: 'enrich', result: 'not_run'}])})]});
  await env.tick();
  const row = name => env.$$('#o-history tr').find(tr => tr.dataset.stage === name);
  assert.deepEqual(row('index_local').children.map(c => c.textContent), ['index_local', '完了', '', 'indexed 3 / unchanged 7']);
  assert.deepEqual(row('index_drive').children.map(c => c.textContent), ['index_drive', '省略', 'Drive の鍵がありません', '']);
  assert.equal(row('quality').children[1].textContent, '失敗');
  assert.equal(row('fetch').children[1].textContent, 'パニック');
  assert.equal(row('enrich').children[1].textContent, '未実行');
  const head = env.$('#o-history .op-rec-head').textContent;
  assert.ok(head.includes('同期 #3') && head.includes('一部の段が失敗'), head);
  assert.ok(!env.$('#o-sync').disabled);
});

await test('履歴の outcome は日本語になり、保留の解除の記録には承認・削除の件数が出る', async () => {
  const env = await bootOpen(opsBody({history: [
    rec(6, {kind: 'drive_release', outcome: 'done', stages: [{name: 'index_drive', result: 'done', counts: {approved: 120, deleted: 118, deletes: 118}}]}),
    rec(5, {kind: 'drive_release', outcome: 'exceeded', stages: [{name: 'index_drive', result: 'done', counts: {mass_delete_withheld: 1}}]}),
    rec(4, {kind: 'drive_release', outcome: 'hold_changed', stages: []}),
    rec(3, {kind: 'quality_scan', outcome: 'failed', stages: []}),
    rec(2, {kind: 'sync', outcome: 'skipped', stages: [{name: 'index_drive', result: 'not_run'}]}),
  ]}));
  const heads = env.$$('#o-history .op-rec-head').map(h => h.textContent);
  assert.ok(heads[0].includes('大量削除の保留の解除 #6') && heads[0].includes('完了'));
  assert.ok(heads[1].includes('保留に戻した'));
  assert.ok(heads[2].includes('保留が変わったので実行せず'));
  assert.ok(heads[3].includes('品質解析（一括） #3') && heads[3].includes('失敗'));
  assert.ok(heads[4].includes('省略'));
  const sums = env.$$('#o-history .op-summary').map(s => s.textContent);
  assert.ok(sums.includes('承認 120 件・削除 118 件'), sums.join('|'));
  assert.ok(env.$$('#o-history .op-rec').find(r => r.dataset.id === '4').textContent.includes('段の記録はありません'));
});

await test('AC8: 直近の定期サイクル（開始・終了・結果・段）と次の予定・間隔が出る', async () => {
  const env = await bootOpen();
  const cyc = env.$('#o-cycle').textContent;
  assert.ok(cyc.includes('次の定期サイクル: 2030-01-01 10:00:00（間隔 10 分）'), cyc);
  assert.ok(cyc.includes('定期サイクル'));
  assert.ok(cyc.includes('開始 2030-01-01 09:50:00') && cyc.includes('終了 2030-01-01 09:50:07'), cyc);
  assert.ok(cyc.includes('一部の段が失敗'));
  assert.deepEqual(env.$$('#o-cycle tr').filter(tr => tr.dataset.stage).map(tr => tr.dataset.stage),
    ['index_local', 'index_drive', 'quality']);
  // まだ終わった定期サイクルが無い
  env.state.body = withWorker({last_cycle: null, next_cycle_at: '2030-01-01 10:10:00'});
  await env.tick();
  assert.ok(env.$('#o-cycle').textContent.includes('まだ定期サイクルは終わっていません'));
  assert.ok(env.$('#o-cycle').textContent.includes('2030-01-01 10:10:00'));
});

await test('AC8: 履歴 10 件が新しい順に描かれる（サーバーの並びに関わらず番号の大きい順）', async () => {
  const ids = [4, 11, 2, 9, 13, 1, 7, 12, 3, 10];
  const env = await bootOpen(opsBody({history: ids.map(i => rec(i))}));
  assert.deepEqual(recIds(env, '#o-history'), [13, 12, 11, 10, 9, 7, 4, 3, 2, 1]);
  assert.equal(recIds(env, '#o-history').length, 10);
  assert.ok(env.$$('#o-history .op-times')[0].textContent.includes('受付 2030-01-01 09:23:00'));
  // 新しい順で来ても同じ
  env.state.body = opsBody({history: [14, 13, 12].map(i => rec(i))});
  await env.tick();
  assert.deepEqual(recIds(env, '#o-history'), [14, 13, 12]);
});

await test('AC4: 「解除する…」で確認の段が開き、件数と消えるものの範囲と写しの文言が出る', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  assert.ok(!env.$('#o-hold-confirm'));
  env.$('#o-hold-open').click();
  const box = env.$('#o-hold-confirm');
  assert.ok(box, '確認の段');
  const t = box.textContent;
  assert.ok(t.includes('承認した 120 件のうち引き続き一覧に無い行だけを削除します'), t);
  assert.ok(t.includes('承認していない行が 1 件でも消えようとすれば、削除せず再び保留します'), t);
  assert.ok(t.includes('位置（手で直したものを含む）・★・除外・回転・タイトルも一緒に消え'), t);
  assert.ok(t.includes('旅行ブログの下書きは画像が抜けます'), t);
  assert.ok(t.includes('原本（Drive 側）には触れません'), t);
  assert.ok(t.includes('解除前の DB の写しを DATA_DIR の下に 1 つ残します（次の解除で上書き）'), t);
  assert.equal(env.$('#o-hold-allow').textContent, '120 件の削除を許可');
  assert.equal(env.$('#o-hold-cancel').textContent, 'やめる');
  assert.ok(!env.$('#o-hold-open'), '確認の段が開いているあいだは「解除する…」は出ない');
  assert.equal(env.posts('/api/admin/ops/drive-release').length, 0, '開いただけでは送らない');
  env.$('#o-hold-cancel').click();
  assert.ok(!env.$('#o-hold-confirm') && env.$('#o-hold-open'));
  assert.equal(env.posts('/api/admin/ops/drive-release').length, 0);
});

await test('AC4: 「N 件の削除を許可」は開いた時点の token だけを送り、成功で確認を閉じて読み直す', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  env.$('#o-hold-open').click();
  const d = deferred();
  env.handlers.release = () => d.promise;
  env.$('#o-hold-allow').click();
  assert.ok(env.$('#o-hold-allow').disabled, '送信中は押せない');
  assert.ok(env.$('#o-hold-confirm'), '応答が返るまで確認の段は残る');
  env.$('#o-hold-allow').dispatchEvent({type: 'click'});
  await settle();
  const sent = env.posts('/api/admin/ops/drive-release');
  assert.equal(sent.length, 1);
  assert.deepEqual(JSON.parse(sent[0].body), {token: '9f3a9f3a9f3a9f3a'});
  const before = env.gets().length;
  env.state.body = withWorker({queued: {id: 9, kind: 'drive_release', requested_at: '2030-01-01 10:00:00'}}, {hold: HOLD()});
  d.resolve({ok: true, status: 200, json: () => Promise.resolve({ok: true, request: {id: 9, kind: 'drive_release', state: 'queued'}, approved: 120})});
  await settle();
  assert.ok(env.toastText().includes('削除の許可を受け付けました（承認 120 件）') && !env.toastErr(), env.toastText());
  assert.ok(!env.$('#o-hold-confirm'), '許可を出したら確認の段は閉じる');
  assert.equal(env.gets().length, before + 1);
  assert.ok(env.$('#o-hold-open').disabled, '待ちに drive_release があるあいだは解除を始められない');
});

await test('AC4: 409 hold_changed で確認の段が閉じて読み直す（no_hold も同じ）', async () => {
  for(const code of ['hold_changed', 'no_hold']){
    const env = await bootOpen(opsBody({hold: HOLD()}));
    env.$('#o-hold-open').click();
    env.handlers.release = () => respond(409, {ok: false, error: code});
    env.state.body = opsBody({hold: HOLD({held: 130, token: 'bbbbbbbbbbbbbbbb'})});
    const before = env.gets().length;
    env.$('#o-hold-allow').click();
    await settle();
    assert.ok(env.toastErr() && env.toastText().includes('保留が変わったので読み直しました'), env.toastText());
    assert.ok(!env.$('#o-hold-confirm'), code + ': 確認は閉じる');
    assert.equal(env.gets().length, before + 1, code + ': 読み直す');
    assert.ok(env.text().includes('130'), '新しい件数が出る');
    assert.equal(env.$('#o-hold-open').textContent, '解除する…');
    assert.ok(!env.$('#o-hold-open').disabled, '送信中の印は外れている');
    assert.equal(env.run('os.confirming'), null);
  }
});

await test('AC4: 確認中の読み直しで token が変わる・保留が消えると確認が閉じて知らせる。同じ token なら開いたまま（作り直さない）', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  env.$('#o-hold-open').click();
  const box = env.$('#o-hold-confirm');
  await env.tick();                                  // 同じ応答
  same(env.$('#o-hold-confirm'), box, '同じ印なら確認の段は作り直さない');
  assert.equal(env.toastText(), '');
  env.state.body = opsBody({hold: HOLD({token: 'cccccccccccccccc', held: 121})});
  await env.tick();
  assert.ok(!env.$('#o-hold-confirm'));
  assert.ok(env.toastErr() && env.toastText().includes('保留が変わったので確認を閉じました'), env.toastText());
  assert.ok(env.text().includes('121'));
  // 閉じたあとの「解除する…」から開き直すと、新しい token と件数を持つ
  env.$('#o-hold-open').click();
  assert.equal(env.$('#o-hold-allow').textContent, '121 件の削除を許可');
  // （タブを離れずに）保留が消える
  env.state.body = opsBody({hold: null});
  await env.tick();
  assert.ok(!env.$('#o-hold-confirm') && hidden(env, '#o-hold-panel'));
  assert.ok(env.toastText().includes('保留が変わったので確認を閉じました'));
});

await test('AC4: 許可の POST の途中で token が変わると、確認の段は閉じる（古い token は送った分だけ）', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  env.$('#o-hold-open').click();
  const d = deferred();
  env.handlers.release = () => d.promise;
  env.$('#o-hold-allow').click();
  env.state.body = opsBody({hold: HOLD({token: 'dddddddddddddddd'})});
  await env.tick();                                  // 送信中に読み直し: 保留の印が違うので確認を閉じる
  assert.ok(!env.$('#o-hold-confirm'));
  assert.equal(JSON.parse(env.posts('/api/admin/ops/drive-release')[0].body).token, '9f3a9f3a9f3a9f3a');
  d.resolve({ok: true, status: 200, json: () => Promise.resolve({ok: false, error: 'hold_changed'})});
  await settle();
  assert.ok(env.toastText().includes('保留が変わったので読み直しました'));
});

await test('AC4: too_soon・busy・HTTP 503 は日本語で知らせ、too_soon では確認を開いたままにする', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  env.$('#o-hold-open').click();
  env.handlers.release = () => respond(409, {ok: false, error: 'too_soon'});
  env.$('#o-hold-allow').click();
  await settle();
  assert.ok(env.toastErr() && env.toastText().includes('前回の同期・解除から 60 秒は待ってください'), env.toastText());
  assert.ok(env.$('#o-hold-confirm'), '待ってやり直せる');
  assert.ok(!env.$('#o-hold-allow').disabled);
  env.handlers.sync = () => respond(409, {ok: false, error: 'busy'});
  env.$('#o-sync').click();
  await settle();
  assert.ok(env.toastText().includes('すでに実行中か待ちがあります'));
  assert.ok(!env.$('#o-sync').disabled);
  env.handlers.quality = () => respond(503, {ok: false});
  env.$('#o-quality').click();
  await settle();
  assert.ok(env.toastErr() && env.toastText().includes('取り込みが無効か停止中です'), env.toastText());
  env.handlers.sync = () => respond(400, {ok: false, error: 'bad_params'});
  env.$('#o-sync').click();
  await settle();
  assert.ok(env.toastText().includes('要求の形が正しくありません'));
  env.handlers.sync = () => respond(500, {ok: false});
  env.$('#o-sync').click();
  await settle();
  assert.ok(env.toastErr() && env.toastText().includes('HTTP 500'), env.toastText());
});

await test('AC4: not_releasable の応答で確認が閉じ、一覧が空のための保留（releasable:false）は案内だけでボタンが出ない', async () => {
  const env = await bootOpen(opsBody({hold: HOLD({reason: 'empty_listing', held: 500, releasable: false, token: 'eeeeeeeeeeeeeeee'})}));
  assert.deepEqual(env.$$('#v-ops button').map(b => b.id), ['o-sync', 'o-quality'], '解除のボタンが無い');
  const t = env.$('#o-hold').textContent;
  assert.ok(t.includes('一覧が空のための保留は、この画面からは解除できません'), t);
  assert.ok(t.includes('serve を止めて CLI（geoframe sync --allow-mass-delete）'), t);
  assert.ok(t.includes('Driveの一覧が空だった'), '保留の理由は出る');
  assert.ok(!env.$('#o-hold-open') && !env.$('#o-hold-allow'));
  // ratio から empty_listing に変わったら、開いていた確認は閉じる
  const env2 = await bootOpen(opsBody({hold: HOLD()}));
  env2.$('#o-hold-open').click();
  env2.state.body = opsBody({hold: HOLD({reason: 'empty_listing', releasable: false, token: 'eeeeeeeeeeeeeeee'})});
  await env2.tick();
  assert.ok(!env2.$('#o-hold-confirm') && !env2.$('#o-hold-allow'));
  assert.ok(env2.toastText().includes('保留が変わったので確認を閉じました'));
  // 念のため直接 POST が not_releasable を返したときの文言
  const env3 = await bootOpen(opsBody({hold: HOLD()}));
  env3.$('#o-hold-open').click();
  env3.handlers.release = () => respond(409, {ok: false, error: 'not_releasable'});
  env3.$('#o-hold-allow').click();
  await settle();
  assert.ok(env3.toastText().includes('この保留は画面から解除できません'));
  assert.ok(!env3.$('#o-hold-confirm'));
});

await test('古い応答は捨てる: 先に出した読み直しの応答が後から返っても、新しい応答の表示を上書きしない', async () => {
  const env = makeEnv();
  const slow = deferred();
  let first = true;
  env.handlers.ops = () => { if(first){ first = false; return slow.promise; } return undefined; };
  await env.boot();
  await env.open();                                   // 1 本目: 遅い
  env.state.body = opsBody({history: [rec(21)]});
  await env.tick();                                   // 2 本目: すぐ返る
  assert.deepEqual(recIds(env, '#o-history'), [21]);
  slow.resolve({ok: true, status: 200, json: () => Promise.resolve(opsBody({history: [rec(1)]}))});
  await settle();
  assert.deepEqual(recIds(env, '#o-history'), [21], '遅れて来た古い応答は捨てる');
  assert.equal(env.run('os.snap.history[0].id'), 21);
});

await test('読み直しの失敗は 1 回だけ知らせ、直れば表示が戻る', async () => {
  const env = await bootOpen();
  env.handlers.ops = () => respond(500, {ok: false, error: 'boom'});
  await env.tick();
  assert.ok(env.toastErr() && env.toastText().includes('運用の状態を読めません: boom'), env.toastText());
  env.$('#toast').textContent = '';
  await env.tick();
  assert.equal(env.toastText(), '', '続けて失敗しても毎回は知らせない');
  assert.ok(!env.$('#o-sync').disabled, '前の表示は残る');
  env.handlers.ops = null;
  env.state.body = opsBody({history: [rec(2)]});
  await env.tick();
  assert.deepEqual(recIds(env, '#o-history'), [2]);
});

await test('タブを開くと 3 秒の読み直しが始まり、離れると clearInterval で止まる。開き直すと 1 本だけ', async () => {
  const env = makeEnv();
  await env.boot();
  assert.equal(env.intervals.filter(i => i.ms === 3000).length, 0, '運用タブを開くまで周期は無い');
  assert.equal(env.gets().length, 0, '起動しただけでは読まない');
  await env.open();
  const ops = env.intervals.map((t, i) => [t, i + 1]).filter(([t]) => t.ms === 3000);
  assert.equal(ops.length, 1);
  assert.deepEqual(env.cleared, []);
  await env.tick();
  assert.equal(env.gets().length, 2);
  await env.leave();
  assert.deepEqual(env.cleared, [ops[0][1]], '離れたら ops の周期だけを止める');
  assert.equal(env.run('os.timer'), null);
  await env.open();
  assert.equal(env.intervals.filter(i => i.ms === 3000).length, 2);
  // 別のタブ（設定）へ移るときも止まる
  await env.$('.tab[data-tab=config]').click();
  await settle();
  assert.equal(env.cleared.length, 2);
  // 運用タブを開いたまま同じタブを押し直しても、周期は重ならない
  await env.open();
  await env.open();
  assert.equal(env.cleared.length, 3, '押し直すたび、前の周期を止めてから始め直す');
  assert.equal(env.run('os.timer'), env.intervals.length);
});

await test('運用タブを開き直すと確認の段は閉じる', async () => {
  const env = await bootOpen(opsBody({hold: HOLD()}));
  env.$('#o-hold-open').click();
  assert.ok(env.$('#o-hold-confirm'));
  await env.leave();
  await env.open();
  assert.ok(!env.$('#o-hold-confirm') && env.$('#o-hold-open'));
});

await test('状況タブの大量削除の注意は「運用タブから解除」を案内する（古い「サービスを止めて」の文言は無い）', async () => {
  const env = makeEnv();
  const cell = env.run('massDeleteNotice')({held: 120, reason: 'ratio', drive_rows: 500, since: 's', checked_at: 'c'});
  const t = cell.textContent;
  assert.ok(t.includes('意図した削除なら運用タブから解除（確かめた件数を超える削除はしない。serve が動いていないときは geoframe sync --allow-mass-delete）'), t);
  assert.ok(!t.includes('この画面からは解除できません') && !t.includes('サービスを止めて'), t);
});

console.log(n + ' tests passed');
