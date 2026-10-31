// admin.js・photos.js・config.js（この順に同じ context へ読む）の設定タブ — 読み取り専用の案内・RUNTIME・
// 設定ファイルの表・3 つのパネル・AI の用途の表・凡例・失敗・古い応答の破棄 — を、fakedom.mjs の DOM の代用品と
// fetch の代用品で動かして確かめる。
// 使い方: node web/tests/config_render.mjs <admin.js のパス> <photos.js のパス> <config.js のパス>
// 合成の応答（crates/geoframe/tests/admin_config_api.rs と同じ形・35 項目）だけで、ネットワークには出ない。
// 値は合成のみ（example.test・/srv/example/...・mark-…）。
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {buildAdminDom, htmlFacts, domFacts} from './fakedom.mjs';

const [jsPath, photosPath, configPath] = process.argv.slice(2);
assert.ok(jsPath && photosPath && configPath, 'admin.js・photos.js・config.js のパスが要る');
const htmlPath = path.join(path.dirname(jsPath), '..', 'admin.html');
const sources = [[jsPath, 'admin.js'], [photosPath, 'photos.js'], [configPath, 'config.js']]
  .map(([p, name]) => [fs.readFileSync(p, 'utf8'), name]);
const loadScripts = context => sources.forEach(([src, name]) => vm.runInContext(src, context, {filename: name}));

const settle = async () => { for(let i = 0; i < 10; i++) await new Promise(r => setImmediate(r)); };
const respond = (status, body) =>
  Promise.resolve({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(body)});

// ---- 合成の応答（EnvItem::ALL の 35 項目。順序も同じ） ----
const ALL_KEYS = [
  'GEOFRAME_DATA_DIR', 'GEOFRAME_HOST', 'GEOFRAME_PORT', 'GEOFRAME_ALLOWED_HOSTS', 'GEOFRAME_LOG', 'GEOFRAME_BLOG',
  'GEOFRAME_INGEST', 'GEOFRAME_DRIVE_POLL_MIN', 'GEOFRAME_PHOTOS_DIR', 'GEOFRAME_FETCH_PER_CYCLE',
  'GEOFRAME_QUALITY_PER_CYCLE', 'GEOFRAME_DRIVE_KEY', 'GEOFRAME_DRIVE_FOLDER', 'GEOFRAME_BLOG_PLANS_DIR',
  'GEOFRAME_INFER_MIN', 'GEOFRAME_INFER_OFFSET_MIN', 'GEOFRAME_TILE_UA', 'GEOFRAME_TILE_CACHE_MAX_MB',
  'GEOFRAME_TILE_FETCH_PER_MIN', 'GEOFRAME_AI_CONFIG', 'GEOFRAME_AI_ENABLED', 'GEOFRAME_AI_POLL_S',
  'GEOFRAME_AI_BACKLOG_POLL_S', 'GEOFRAME_AI_EMBED_PER_CYCLE', 'GEOFRAME_AI_JUDGE_PER_CYCLE',
  'GEOFRAME_AI_SEARCH_PER_MIN', 'GEOFRAME_LOC_SUGGEST', 'GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE',
  'GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE', 'GEOFRAME_EMBED_BACKEND', 'GEOFRAME_AI_TIME_WINDOW_MIN',
  'GEOFRAME_AI_SIMILARITY', 'GEOFRAME_AI_SHORTLIST_MAX', 'GEOFRAME_AI_VISUAL_SIMILARITY', 'GEOFRAME_RANK_MODEL',
];
assert.equal(ALL_KEYS.length, 35);
const VALUES = {
  GEOFRAME_DATA_DIR: '/srv/example/geoframe', GEOFRAME_HOST: '0.0.0.0', GEOFRAME_PORT: '8080',
  GEOFRAME_ALLOWED_HOSTS: 'localhost, mark-host.example.test', GEOFRAME_LOG: 'info', GEOFRAME_BLOG: '0',
  GEOFRAME_INGEST: '1', GEOFRAME_DRIVE_POLL_MIN: '10', GEOFRAME_PHOTOS_DIR: 'photos',
  GEOFRAME_FETCH_PER_CYCLE: '25', GEOFRAME_QUALITY_PER_CYCLE: '25',
  GEOFRAME_DRIVE_KEY: '/srv/example/mark-drive-key.json', GEOFRAME_DRIVE_FOLDER: 'mark-folder',
  GEOFRAME_BLOG_PLANS_DIR: null, GEOFRAME_INFER_MIN: '15', GEOFRAME_INFER_OFFSET_MIN: '-3',
  GEOFRAME_TILE_UA: 'mark@example.test', GEOFRAME_TILE_CACHE_MAX_MB: '16384', GEOFRAME_TILE_FETCH_PER_MIN: '240',
  GEOFRAME_AI_CONFIG: '/srv/example/mark-ai-config.json', GEOFRAME_AI_ENABLED: '1', GEOFRAME_AI_POLL_S: '60',
  GEOFRAME_AI_BACKLOG_POLL_S: '5', GEOFRAME_AI_EMBED_PER_CYCLE: '12', GEOFRAME_AI_JUDGE_PER_CYCLE: '2',
  GEOFRAME_AI_SEARCH_PER_MIN: '30', GEOFRAME_LOC_SUGGEST: '1', GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE: '10',
  GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE: '2', GEOFRAME_EMBED_BACKEND: 'gemini', GEOFRAME_AI_TIME_WINDOW_MIN: '15',
  GEOFRAME_AI_SIMILARITY: '0.82', GEOFRAME_AI_SHORTLIST_MAX: '6', GEOFRAME_AI_VISUAL_SIMILARITY: '0.9',
  GEOFRAME_RANK_MODEL: '/srv/example/geoframe/models/rank_model.json',
};
const SOURCES = {
  GEOFRAME_DATA_DIR: 'arg', GEOFRAME_HOST: 'default', GEOFRAME_PORT: 'default', GEOFRAME_ALLOWED_HOSTS: 'env',
  GEOFRAME_INGEST: 'env', GEOFRAME_DRIVE_KEY: 'env', GEOFRAME_DRIVE_FOLDER: 'env', GEOFRAME_TILE_UA: 'env',
  GEOFRAME_AI_CONFIG: 'env', GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE: 'env', GEOFRAME_AI_SIMILARITY: 'env',
};
const sourceOf = key => SOURCES[key] || 'default';
// ファイルの項目（Exposure::File）だけ state を持つ。modified_at は GEOFRAME_AI_CONFIG だけ
const FILE_STATE = {GEOFRAME_DRIVE_KEY: 'present', GEOFRAME_RANK_MODEL: 'absent', GEOFRAME_AI_CONFIG: 'loaded'};

const KEY_PATH = '/srv/example/mark-key-1.json';
const TASKS = [
  {task: 'image_embedding', provider: 'local', model: 'mark-model-embed', service_id: 'mark-svc',
   source: 'ai_config', disabled: null},
  {task: 'query_embedding', provider: 'local', model: 'mark-model-embed', service_id: 'mark-svc',
   source: 'ai_config', disabled: null},
  {task: 'photo_judge', provider: 'cloud', model: 'mark-model-judge', allow_cloud: true, include_hidden: false,
   daily_limit: 40, today_utc: 12, credential_path: KEY_PATH, credential: 'present',
   project: 'mark-project', region: 'mark-region1', source: 'ai_config', disabled: null},
  {task: 'location_clue', provider: 'local', model: 'mark-model-decision', source: 'ai_config', disabled: null},
  {task: 'location_suggest', provider: 'cloud', model: 'mark-model-suggest', allow_cloud: false,
   include_hidden: true, daily_limit: null, today_utc: null, credential_path: '/srv/example/mark-key-2.json',
   credential: 'absent', project: 'mark-project-2', region: 'mark-region2', source: 'ai_config',
   disabled: 'cloud_not_allowed'},
];
const SPACE = {space_id: 'mark-space-id', model_id: 'mark-space-model', revision: 'mark-space-rev',
               image_preprocess: 'mark-space-img', text_preprocess: 'mark-space-txt', dimension: 768,
               normalization: 'l2'};

function configBody(over = {}){
  const items = ALL_KEYS.map(key => {
    const it = {key, value: VALUES[key], source: sourceOf(key)};
    if(FILE_STATE[key]) it.state = FILE_STATE[key];
    if(key === 'GEOFRAME_AI_CONFIG') it.modified_at = '2026-10-30 14:07:11';
    return it;
  });
  return {
    ok: true, version: '0.1.0', started_at: '2026-10-30 08:25:00', uptime_s: 47520, items,
    ai: {configured: true, invalid: null, enabled: true, version: 1,
         local_base_url: 'http://mark-host.invalid:8100', embedding_space: SPACE, tasks: TASKS},
    ...over,
  };
}

function makeEnv({configHandler} = {}){
  const document = buildAdminDom();
  const calls = [];
  const fetchFake = (url, opts = {}) => {
    calls.push({url, method: opts.method || 'GET'});
    if(url === '/api/admin/status'){
      return respond(200, {ok: true, server_time: '2026-10-31 10:00:00', sync: {ready: 1, pending: 0, total: 1},
        db: {no_loc: 0, no_display: 0, favorite: 0, hidden: 0, enrich_pending: 0, no_time: 0, by_loc_source: {},
             infer_buckets: {}, by_source: {}, filters: {all: 1, noloc: 0, suggest: 0, inferred: 0, manual: 0, fav: 0, hidden: 0}},
        disk: {used_pct: 1, free_bytes: 1e11, total_bytes: 2e11, dirs: {derivatives: {bytes: 1, files: 1}, tile_cache: {bytes: 1, files: 1}}}});
    }
    if(url === '/api/admin/config') return configHandler ? configHandler(calls.length) : respond(200, configBody());
    return respond(404, {ok: false, error: 'unexpected ' + url});
  };
  const context = vm.createContext({
    document, fetch: fetchFake, location: {pathname: '/admin', origin: 'http://localhost'},
    setInterval(){ throw new Error('setInterval は状況の 30 秒更新だけ（boot の中）'); }, setTimeout(){ return 0; },
    clearTimeout(){}, console,
  });
  // boot() の setInterval（状況の更新）だけは許す。設定の周期読みは無いことを下の試験で確かめる
  const intervals = [];
  context.setInterval = (fn, ms) => { intervals.push({fn, ms}); return intervals.length; };
  loadScripts(context);
  const boot = async () => { document.dispatchEvent({type: 'DOMContentLoaded'}); await settle(); };
  return {document, context, calls, intervals, boot, $: s => document.querySelector(s),
          configCalls: () => calls.filter(c => c.url === '/api/admin/config'),
          openConfig: async () => { document.querySelector('.tab[data-tab=config]').click(); await settle(); }};
}

const text = env => env.$('#v-config').textContent;
const rows = (env, sel) => env.document.querySelectorAll(sel + ' tr').slice(1);   // 見出し行を除く
const rowOf = (env, key) => {
  const all = env.document.querySelectorAll('#v-config tr').filter(tr => tr.children[0] && tr.children[0].textContent === key);
  assert.equal(all.length, 1, key + ' の行は 1 つ');
  return all[0];
};

let n = 0;
async function test(name, fn){ await fn(); n++; console.log('ok - ' + name); }

await test('合成の応答は EnvItem::ALL の 35 項目', () => {
  assert.equal(configBody().items.length, 35);
  assert.equal(new Set(ALL_KEYS).size, 35);
});

await test('admin.html と fakedom の骨格（id・タブ・読み込み順）が一致している', () => {
  const html = fs.readFileSync(htmlPath, 'utf8');
  const want = htmlFacts(html);
  const have = domFacts(buildAdminDom());
  assert.deepEqual(have.ids, want.ids, 'id の集合');
  assert.deepEqual(want.tabs, ['status', 'photos', 'config']);
  assert.deepEqual(have.tabs, want.tabs);
  for(const id of ['v-config', 'c-note', 'c-runtime', 'c-files', 'c-ingest', 'c-geo', 'c-ai', 'c-ai-state',
                   'c-ai-tasks', 'c-ai-space', 'c-legend']){
    assert.ok(want.ids.includes(id), id);
  }
  assert.ok(html.includes('<section id="v-config" class="hidden">'), '設定タブは最初は隠れている');
  assert.ok(html.includes('data-tab="config">▸ 設定</button>'));
  const scripts = [...html.matchAll(/<script src="([^"]+)"/g)].map(m => m[1]).filter(u => u.startsWith('/static/'));
  assert.deepEqual(scripts, ['/static/js/admin.js', '/static/js/photos.js', '/static/js/config.js']);
});

await test('config.js は HTML 文字列を組まず、読み込んだだけでは fetch にも location にも触らない', () => {
  const text = fs.readFileSync(configPath, 'utf8');
  assert.ok(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(text));
  assert.ok(!text.includes('DOMContentLoaded') && !text.includes('setInterval'));
  const touched = [];
  const sandbox = {document: buildAdminDom(), setInterval(){}, setTimeout(){}, clearTimeout(){}, console};
  Object.defineProperty(sandbox, 'location', {get(){ touched.push('location'); throw new Error('location'); }});
  Object.defineProperty(sandbox, 'fetch', {get(){ touched.push('fetch'); throw new Error('fetch'); }});
  loadScripts(vm.createContext(sandbox));
  assert.deepEqual(touched, []);
});

await test('(a) boot() は /admin で状況タブ。設定タブを開くたびに GET /api/admin/config を 1 回読み、周期では読まない', async () => {
  const env = makeEnv();
  await env.boot();
  assert.deepEqual(env.configCalls(), [], '状況タブのうちは読まない');
  assert.equal(env.$('#v-status').classList.contains('hidden'), false);
  assert.equal(env.$('#v-config').classList.contains('hidden'), true);
  await env.openConfig();
  assert.equal(env.configCalls().length, 1);
  assert.equal(env.configCalls()[0].method, 'GET');
  assert.equal(env.$('#v-config').classList.contains('hidden'), false);
  assert.equal(env.$('#v-status').classList.contains('hidden'), true);
  assert.deepEqual(env.document.querySelectorAll('nav .tab').filter(t => t.classList.contains('on')).map(t => t.dataset.tab),
                   ['config']);
  // 状況へ戻って、もう一度開くと読み直す
  env.$('.tab[data-tab=status]').click(); await settle();
  assert.equal(env.configCalls().length, 1);
  await env.openConfig();
  assert.equal(env.configCalls().length, 2);
  // 設定タブのまま開き直しても毎回読む
  await env.openConfig();
  assert.equal(env.configCalls().length, 3);
  // 周期の更新は状況だけ（30 秒の 1 本）。それを回しても設定は読まない（設定タブを見ているので状況も読まない）
  assert.equal(env.intervals.length, 1);
  env.intervals[0].fn(); await settle();
  assert.equal(env.configCalls().length, 3);
  assert.equal(env.calls.filter(c => c.url === '/api/admin/status').length, 2, '状況へ戻した 1 回と起動の 1 回だけ');
  assert.equal(vm.runInContext('initialTab()', env.context), 'status');
});

await test('(b) 案内文は読み取り専用と、画面を共有しないことを言う（読む前から出ている）', async () => {
  const env = makeEnv();
  await env.boot();
  const before = env.$('#c-note').textContent;
  assert.ok(before.includes('読み取り専用') && before.includes('共有'), before);
  await env.openConfig();
  const note = env.$('#c-note').textContent;
  assert.ok(note.includes('読み取り専用'));
  assert.ok(note.includes('画面を共有'), note);
  assert.ok(note.includes('鍵のパス') && note.includes('接続先') && note.includes('連絡先'));
  assert.ok(!note.includes('設定を読めませんでした'));
  // 凡例: 出どころ 4 種
  const legend = env.document.querySelectorAll('#c-legend span.src');
  assert.deepEqual(legend.map(b => b.className), ['src arg', 'src env', 'src default', 'src ai_config']);
});

await test('(c) items の 35 項目すべての key と value が画面に出る。null は（未設定）', async () => {
  const env = makeEnv();
  await env.boot(); await env.openConfig();
  const body = text(env);
  for(const key of ALL_KEYS){
    assert.ok(body.includes(key), key);
    const v = VALUES[key];
    if(v !== null) assert.ok(rowOf(env, key).textContent.includes(v), key + ' の値 ' + v);
  }
  const plans = rowOf(env, 'GEOFRAME_BLOG_PLANS_DIR');
  assert.ok(plans.textContent.includes('（未設定）'));
  assert.ok(!rowOf(env, 'GEOFRAME_DATA_DIR').textContent.includes('（未設定）'));
  // 分類: ingest 13・geo 5・ai 14（ファイルの 3 項目は専用の表）。表は 1 項目 1 行
  assert.equal(rows(env, '#c-ingest').length, 13);
  assert.equal(rows(env, '#c-geo').length, 5);
  assert.equal(rows(env, '#c-ai').length, 14);
  assert.equal(rows(env, '#c-files').length, 3);
  assert.deepEqual(rows(env, '#c-files').map(r => r.children[0].textContent),
                   ['GEOFRAME_AI_CONFIG', 'GEOFRAME_DRIVE_KEY', 'GEOFRAME_RANK_MODEL']);
  assert.equal(rows(env, '#c-ingest')[0].children[0].textContent, 'GEOFRAME_DATA_DIR');
  assert.equal(env.$('#c-other-panel').classList.contains('hidden'), true, 'その他は空なら隠す');
  // 単位と意味
  assert.ok(rowOf(env, 'GEOFRAME_DRIVE_POLL_MIN').textContent.includes('10分'));
  assert.ok(rowOf(env, 'GEOFRAME_AI_POLL_S').textContent.includes('60秒'));
  assert.ok(rowOf(env, 'GEOFRAME_INGEST').textContent.includes('取り込み'));
  // 全項目の行に意味がある（表に無い項目の代用文が出ていない）
  assert.ok(!body.includes('この画面に説明が無い'));
});

await test('(c2) 表に無い key は「その他」に出し、API に無い key は行を作らない', async () => {
  const body = configBody();
  body.items = body.items.filter(i => i.key !== 'GEOFRAME_LOG' && i.key !== 'GEOFRAME_AI_VISUAL_SIMILARITY');
  body.items.push({key: 'GEOFRAME_MARK_FUTURE', value: 'mark-future-value', source: 'env'});
  body.items.push({key: 'GEOFRAME_MARK_NULL', value: null, source: 'default'});
  const env = makeEnv({configHandler: () => respond(200, body)});
  await env.boot(); await env.openConfig();
  assert.equal(env.$('#c-other-panel').classList.contains('hidden'), false);
  assert.deepEqual(rows(env, '#c-other').map(r => r.children[0].textContent), ['GEOFRAME_MARK_FUTURE', 'GEOFRAME_MARK_NULL']);
  assert.ok(rowOf(env, 'GEOFRAME_MARK_FUTURE').textContent.includes('mark-future-value'));
  assert.ok(rowOf(env, 'GEOFRAME_MARK_NULL').textContent.includes('（未設定）'));
  const tableKeys = env.document.querySelectorAll('#v-config tr').map(r => r.children[0].textContent);
  assert.ok(!tableKeys.includes('GEOFRAME_LOG') && !tableKeys.includes('GEOFRAME_AI_VISUAL_SIMILARITY'),
    'API に無い key は表に行を作らない');
  assert.ok(env.$('#c-runtime').textContent.includes('--'), 'RUNTIME のログは値が無ければ --');
  assert.equal(rows(env, '#c-ingest').length, 12);
  assert.equal(rows(env, '#c-ai').length, 13);
});

await test('(d) 出どころのバッジの色と文言が source と一致する', async () => {
  const env = makeEnv();
  await env.boot(); await env.openConfig();
  const LABEL = {arg: '起動引数', env: '環境変数', default: '既定', ai_config: 'AI設定JSON'};
  for(const key of ALL_KEYS){
    const badges = rowOf(env, key).querySelectorAll('span.src');
    assert.equal(badges.length, 1, key);
    assert.equal(badges[0].className, 'src ' + sourceOf(key), key);
    assert.equal(badges[0].textContent, LABEL[sourceOf(key)], key);
  }
  // 用途の表は ai_config（緑）
  const taskBadges = env.document.querySelectorAll('#c-ai-tasks span.src');
  assert.equal(taskBadges.length, 5);
  assert.ok(taskBadges.every(b => b.className === 'src ai_config' && b.textContent === 'AI設定JSON'));
  // 未知の source は既定の色で、文言はそのまま
  const unknown = configBody();
  unknown.items[0].source = 'mark-new-source';
  const env2 = makeEnv({configHandler: () => respond(200, unknown)});
  await env2.boot(); await env2.openConfig();
  const b = rowOf(env2, 'GEOFRAME_DATA_DIR').querySelectorAll('span.src')[0];
  assert.equal(b.className, 'src default');
  assert.equal(b.textContent, 'mark-new-source');
});

await test('(e) ファイルの state（あり/なし/読めない/読み込み済み/不正）と modified_at', async () => {
  const env = makeEnv();
  await env.boot(); await env.openConfig();
  const cfg = rowOf(env, 'GEOFRAME_AI_CONFIG').textContent;
  assert.ok(cfg.includes('読み込み済み') && cfg.includes('2026-10-30 14:07:11') && cfg.includes('/srv/example/mark-ai-config.json'));
  assert.ok(rowOf(env, 'GEOFRAME_DRIVE_KEY').textContent.includes('あり'));
  assert.ok(rowOf(env, 'GEOFRAME_RANK_MODEL').textContent.includes('なし'));
  assert.ok(!rowOf(env, 'GEOFRAME_DRIVE_KEY').textContent.includes('2026-'), 'modified_at の無い行は更新日時を出さない');
  assert.equal(rowOf(env, 'GEOFRAME_AI_CONFIG').querySelectorAll('span.st')[0].classList.contains('good'), true);
  // 残りの state
  const odd = configBody();
  odd.items.find(i => i.key === 'GEOFRAME_DRIVE_KEY').state = 'unreadable';
  odd.items.find(i => i.key === 'GEOFRAME_AI_CONFIG').state = 'invalid';
  const env2 = makeEnv({configHandler: () => respond(200, odd)});
  await env2.boot(); await env2.openConfig();
  assert.ok(rowOf(env2, 'GEOFRAME_DRIVE_KEY').textContent.includes('読めない'));
  assert.ok(rowOf(env2, 'GEOFRAME_AI_CONFIG').textContent.includes('不正'));
  assert.equal(rowOf(env2, 'GEOFRAME_AI_CONFIG').querySelectorAll('span.st')[0].classList.contains('bad'), true);
  const absent = configBody();
  absent.items.find(i => i.key === 'GEOFRAME_AI_CONFIG').state = 'absent';
  absent.items.find(i => i.key === 'GEOFRAME_AI_CONFIG').value = null;
  delete absent.items.find(i => i.key === 'GEOFRAME_AI_CONFIG').modified_at;
  const env3 = makeEnv({configHandler: () => respond(200, absent)});
  await env3.boot(); await env3.openConfig();
  const row = rowOf(env3, 'GEOFRAME_AI_CONFIG').textContent;
  assert.ok(row.includes('なし') && row.includes('（未設定）'));
});

await test('RUNTIME: 取り込み・待ち受け・データ置き場・経過・旅行ブログ・ログ・版', async () => {
  const env = makeEnv();
  await env.boot(); await env.openConfig();
  const cells = env.document.querySelectorAll('#c-runtime .cell');
  assert.equal(cells.length, 7);
  const t = cells.map(c => c.textContent).join('\n');
  assert.ok(cells[0].textContent.includes('取り込み INGEST') && cells[0].textContent.includes('有効'));
  assert.ok(cells[0].classList.contains('good'));
  assert.ok(t.includes('0.0.0.0:8080'));
  assert.ok(t.includes('/srv/example/geoframe'));
  assert.ok(t.includes('13.2時間'), '47520 秒は fmtDur で 13.2時間');
  assert.ok(t.includes('起動 2026-10-30 08:25:00'));
  assert.ok(cells[4].textContent.includes('旅行ブログ') && cells[4].textContent.includes('無効'));
  assert.ok(cells[5].textContent.includes('info'));
  assert.ok(cells[6].textContent.includes('0.1.0'));
  // INGEST=0 は無効（warn）
  const off = configBody();
  off.items.find(i => i.key === 'GEOFRAME_INGEST').value = '0';
  off.items.find(i => i.key === 'GEOFRAME_BLOG').value = '1';
  const env2 = makeEnv({configHandler: () => respond(200, off)});
  await env2.boot(); await env2.openConfig();
  const c2 = env2.document.querySelectorAll('#c-runtime .cell');
  assert.ok(c2[0].textContent.includes('無効') && c2[0].classList.contains('warn'));
  assert.ok(c2[4].textContent.includes('有効') && c2[4].classList.contains('good'));
});

await test('(f) AI の用途の表: 今日（UTC）/ 上限・バー・—・認証ファイル・project・region・disabled、接続先と空間', async () => {
  const env = makeEnv();
  await env.boot(); await env.openConfig();
  const trs = rows(env, '#c-ai-tasks');
  assert.equal(trs.length, 5);
  const byTask = Object.fromEntries(trs.map(r => [r.children[0].textContent.split(/[^a-z_]/)[0], r]));
  const judge = byTask.photo_judge;
  assert.ok(judge.textContent.includes('12 / 40'), judge.textContent);
  assert.ok(judge.textContent.includes('mark-model-judge') && judge.textContent.includes('cloud'));
  assert.ok(judge.textContent.includes(KEY_PATH) && judge.textContent.includes('あり'));
  assert.ok(judge.textContent.includes('mark-project') && judge.textContent.includes('mark-region1'));
  assert.ok(judge.textContent.includes('クラウド許可') && judge.textContent.includes('hidden は送らない'));
  // バー: 12/40 = 30%（warn なし）
  let fill = judge.querySelectorAll('.bar i');
  assert.equal(fill.length, 1);
  assert.equal(fill[0].style.width, '30%');
  assert.equal(fill[0].classList.contains('warn'), false);
  assert.ok(judge.textContent.includes('有効'), 'disabled が null なら有効');
  // today_utc が null: 「—」。上限が無ければ上限なし。バーは無い
  const suggest = byTask.location_suggest;
  assert.ok(suggest.textContent.includes('—'));
  assert.ok(!suggest.textContent.includes('/ '), '上限が無いときは「今日 / 上限」の形にしない');
  assert.ok(suggest.textContent.includes('上限なし'));
  assert.equal(suggest.querySelectorAll('.bar').length, 0);
  assert.ok(suggest.textContent.includes('クラウド不許可') && suggest.textContent.includes('hidden も送る'));
  assert.ok(suggest.textContent.includes('cloud_not_allowed'), 'disabled の固定コード');
  assert.ok(suggest.textContent.includes('なし'), '認証ファイルの state（absent）');
  assert.ok(suggest.textContent.includes('mark-project-2') && suggest.textContent.includes('mark-region2'));
  // local の用途は service_id を出し、クラウドの欄は「—」
  const clip = byTask.image_embedding;
  assert.ok(clip.textContent.includes('mark-svc') && clip.textContent.includes('local'));
  assert.equal(clip.querySelectorAll('.bar').length, 0);
  assert.ok(!clip.textContent.includes('project'));
  // 接続先と空間
  const space = env.$('#c-ai-space').textContent;
  for(const s of ['local_base_url', 'http://mark-host.invalid:8100', 'space_id', 'mark-space-id', 'mark-space-model',
                  'mark-space-rev', 'mark-space-img', 'mark-space-txt', '768', 'l2'])
    assert.ok(space.includes(s), s);
  assert.equal(env.$('#c-ai-state').textContent, '');

  // 上限の 90% 以上で warn、0 を超えて上限に達しても幅は 100% まで
  for(const [today, limit, width, warn] of [[36, 40, '90%', true], [35, 40, '87.5%', false], [55, 40, '100%', true],
                                            [0, 40, '0%', false]]){
    const b = configBody();
    const t = b.ai.tasks.find(x => x.task === 'photo_judge');
    t.today_utc = today; t.daily_limit = limit;
    const e = makeEnv({configHandler: () => respond(200, b)});
    await e.boot(); await e.openConfig();
    const row = rows(e, '#c-ai-tasks').find(r => r.children[0].textContent.startsWith('photo_judge'));
    assert.ok(row.textContent.includes(`${today} / ${limit}`), `${today} / ${limit}`);
    const f = row.querySelectorAll('.bar i')[0];
    assert.equal(f.style.width, width, `${today}/${limit}`);
    assert.equal(f.classList.contains('warn'), warn, `${today}/${limit}`);
  }
  // 上限があって today_utc が null: 「— / 40」でバーなし
  const b2 = configBody();
  Object.assign(b2.ai.tasks.find(x => x.task === 'photo_judge'), {today_utc: null});
  const e2 = makeEnv({configHandler: () => respond(200, b2)});
  await e2.boot(); await e2.openConfig();
  const r2 = rows(e2, '#c-ai-tasks').find(r => r.children[0].textContent.startsWith('photo_judge'));
  assert.ok(r2.textContent.includes('— / 40'));
  assert.equal(r2.querySelectorAll('.bar').length, 0);
});

await test('(g) ai.configured=false と ai.invalid の表示', async () => {
  const none = configBody({ai: {configured: false, invalid: null, enabled: true, version: null,
                                local_base_url: null, embedding_space: null, tasks: []}});
  const env = makeEnv({configHandler: () => respond(200, none)});
  await env.boot(); await env.openConfig();
  assert.ok(env.$('#c-ai-state').textContent.includes('AI 設定 JSON が未設定'));
  assert.ok(!env.$('#c-ai-state').textContent.includes('AI 設定が不正'));
  assert.equal(rows(env, '#c-ai-tasks').length, 1, '用途が無いことを 1 行で示す');
  assert.ok(env.$('#c-ai-tasks').textContent.includes('用途の設定がありません'));
  assert.ok(env.$('#c-ai-space').textContent.includes('（未設定）'));
  // 項目の表はそのまま出る
  assert.ok(text(env).includes('GEOFRAME_AI_POLL_S'));

  const bad = configBody({ai: {configured: true, invalid: 'config_invalid', enabled: true, version: null,
                               local_base_url: null, embedding_space: null, tasks: []}});
  const env2 = makeEnv({configHandler: () => respond(200, bad)});
  await env2.boot(); await env2.openConfig();
  assert.ok(env2.$('#c-ai-state').textContent.includes('AI 設定が不正: config_invalid'));
  assert.ok(!env2.$('#c-ai-state').textContent.includes('AI 設定 JSON が未設定'));
  assert.equal(env2.$('#c-ai-state p').classList.contains('err'), true);
  // 設定が直って読み直すと表示が消える
  const env3 = makeEnv({configHandler: n => respond(200, n > 2 ? configBody() : bad)});
  await env3.boot(); await env3.openConfig();
  assert.ok(env3.$('#c-ai-state').textContent.includes('不正'));
  await env3.openConfig();
  assert.equal(env3.$('#c-ai-state').textContent, '');
  assert.equal(rows(env3, '#c-ai-tasks').length, 5);
});

await test('(h) 失敗（503・通信失敗・本文が JSON でない）で #c-note に理由。案内も残る', async () => {
  const env = makeEnv({configHandler: () => respond(503, {ok: false, error: 'mark-unavailable'})});
  await env.boot(); await env.openConfig();
  const note = env.$('#c-note').textContent;
  assert.ok(note.includes('設定を読めませんでした: mark-unavailable'), note);
  assert.ok(note.includes('読み取り専用'), '失敗でも案内は残る');
  assert.equal(env.$('#c-note p.err').textContent, '設定を読めませんでした: mark-unavailable');

  const env2 = makeEnv({configHandler: () => Promise.reject(new Error('mark-network-down'))});
  await env2.boot(); await env2.openConfig();
  assert.ok(env2.$('#c-note').textContent.includes('設定を読めませんでした: mark-network-down'));

  const env3 = makeEnv({configHandler: () => Promise.resolve({ok: false, status: 500, json: () => Promise.reject(new Error('x'))})});
  await env3.boot(); await env3.openConfig();
  assert.ok(env3.$('#c-note').textContent.includes('設定を読めませんでした: HTTP 500'));

  // 失敗のあとの再読み込みが成功すれば、理由は消える
  const env4 = makeEnv({configHandler: n => n > 2 ? respond(200, configBody()) : respond(503, {ok: false, error: 'mark-unavailable'})});
  await env4.boot(); await env4.openConfig();
  assert.ok(env4.$('#c-note').textContent.includes('mark-unavailable'));
  await env4.openConfig();
  assert.ok(!env4.$('#c-note').textContent.includes('設定を読めませんでした'));
  assert.ok(text(env4).includes('GEOFRAME_DATA_DIR'));
});

await test('(i) 古い応答は捨てる（開き直したあとに先の応答が届いても、後の応答を上書きしない）', async () => {
  const held = [];
  const env = makeEnv({configHandler: () => new Promise(resolve => held.push(resolve))});
  await env.boot();
  await env.openConfig();
  await env.openConfig();
  assert.equal(held.length, 2);
  const first = configBody();
  first.items.find(i => i.key === 'GEOFRAME_DRIVE_FOLDER').value = 'mark-old-folder';
  const second = configBody();
  second.items.find(i => i.key === 'GEOFRAME_DRIVE_FOLDER').value = 'mark-new-folder';
  // 後の応答が先に届き、先の応答があとから届く
  held[1]({ok: true, status: 200, json: () => Promise.resolve(second)}); await settle();
  assert.ok(text(env).includes('mark-new-folder'));
  held[0]({ok: true, status: 200, json: () => Promise.resolve(first)}); await settle();
  assert.ok(text(env).includes('mark-new-folder'));
  assert.ok(!text(env).includes('mark-old-folder'));

  // 古い失敗も、新しい表示を消さない
  const held2 = [];
  const env2 = makeEnv({configHandler: () => new Promise((resolve, reject) => held2.push({resolve, reject}))});
  await env2.boot(); await env2.openConfig(); await env2.openConfig();
  held2[1].resolve({ok: true, status: 200, json: () => Promise.resolve(second)}); await settle();
  held2[0].reject(new Error('mark-stale-failure')); await settle();
  assert.ok(!env2.$('#c-note').textContent.includes('mark-stale-failure'));
  assert.ok(text(env2).includes('mark-new-folder'));
  // 応答待ちの間に別のタブへ移っても、届いた応答は設定の表に入るだけで、表示中のタブは変わらない
  const held3 = [];
  const env3 = makeEnv({configHandler: () => new Promise(resolve => held3.push(resolve))});
  await env3.boot(); await env3.openConfig();
  env3.$('.tab[data-tab=status]').click(); await settle();
  held3[0]({ok: true, status: 200, json: () => Promise.resolve(second)}); await settle();
  assert.equal(env3.$('#v-config').classList.contains('hidden'), true);
  assert.equal(env3.$('#v-status').classList.contains('hidden'), false);
});

await test('状況・写真タブへの切り替えで設定タブは隠れ、設定タブへ行ってもほかのタブの読み込みは起きない', async () => {
  const env = makeEnv();
  await env.boot();
  env.$('.tab[data-tab=photos]').click(); await settle();
  assert.equal(env.$('#v-config').classList.contains('hidden'), true);
  assert.equal(env.$('#v-photos').classList.contains('hidden'), false);
  const before = env.calls.length;
  await env.openConfig();
  assert.equal(env.calls.length, before + 1, '設定タブを開くと設定だけを読む');
  assert.equal(env.$('#v-photos').classList.contains('hidden'), true);
  assert.equal(vm.runInContext("showTab('unknown'), 1", env.context), 1);
  assert.equal(env.$('#v-status').classList.contains('hidden'), false, '未知のタブ名は状況');
});

console.log('ok (' + n + ')');
