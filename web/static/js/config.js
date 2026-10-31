/* ============================================================
   config.js — 管理ページの設定タブ（読み取り専用。classic script / 依存なし。定義だけ）
   admin.js → photos.js → config.js の順に読み込む。共有はグローバル関数: ここは admin.js の $ el cell fmtDur を使い、
   admin.js は ここの initConfig loadConfig を（typeof で守って）使う。
   データは GET /api/admin/config だけ（設定タブを開くたびに 1 回。周期的には読まない）。
   API は key・value・source（とファイルの state・modified_at）だけを返す。分類・意味・単位・並びはこのファイルの
   CONFIG_ITEMS が持つ（docs/reference/configuration.md の「Rust が読む環境変数」の短い版）。
   描画は textContent と DOM API だけ（HTML 文字列は組まない）。
   ============================================================ */
'use strict';

/* 分類 group: ingest（基本・取り込み）/ geo（位置推定・地図）/ ai / files（設定ファイル。専用の表）。
   API の items に無い key は出さない。ここに無い key は「その他」に出す（どちらも落とさない）。 */
const CONFIG_ITEMS = [
  {key: 'GEOFRAME_AI_CONFIG', group: 'files', label: 'AI 設定 JSON のパス。routing・モデル・Provider・上限・予算はこのファイルが決める', unit: ''},
  {key: 'GEOFRAME_DRIVE_KEY', group: 'files', label: 'Google Drive のサービスアカウント鍵 JSON のパス（中身は読まない・出さない）', unit: ''},
  {key: 'GEOFRAME_RANK_MODEL', group: 'files', label: '同一場面グループの順位に使う学習済みモデル JSON。無ければ固定の式で順位を付ける', unit: ''},

  {key: 'GEOFRAME_DATA_DIR', group: 'ingest', label: 'データ置き場（catalog.db・derivatives・photos・キャッシュ）。起動引数 --data-dir が優先', unit: ''},
  {key: 'GEOFRAME_HOST', group: 'ingest', label: '待ち受けのアドレス。起動引数 --listen は host と port の両方を置き換える', unit: ''},
  {key: 'GEOFRAME_PORT', group: 'ingest', label: 'HTTP の待ち受けポート', unit: ''},
  {key: 'GEOFRAME_ALLOWED_HOSTS', group: 'ingest', label: '許可する Host 名（localhost を含む）。IP リテラル宛は無条件に許可', unit: ''},
  {key: 'GEOFRAME_LOG', group: 'ingest', label: 'ログの詳しさ', unit: ''},
  {key: 'GEOFRAME_BLOG', group: 'ingest', label: '旅行ブログ。1 で有効、0 で無効', unit: ''},
  {key: 'GEOFRAME_INGEST', group: 'ingest', label: '取り込み（DB への書き込みと同期サイクル）。1 で有効、0 は書かないモード', unit: ''},
  {key: 'GEOFRAME_DRIVE_POLL_MIN', group: 'ingest', label: '同期サイクルの間隔', unit: '分'},
  {key: 'GEOFRAME_PHOTOS_DIR', group: 'ingest', label: 'ローカル写真の置き場（データ置き場からの相対）', unit: ''},
  {key: 'GEOFRAME_FETCH_PER_CYCLE', group: 'ingest', label: '同期 1 サイクルで derivative を作る枚数', unit: '枚'},
  {key: 'GEOFRAME_QUALITY_PER_CYCLE', group: 'ingest', label: '同期 1 サイクルで品質を解析する枚数', unit: '枚'},
  {key: 'GEOFRAME_DRIVE_FOLDER', group: 'ingest', label: '取り込む Drive のフォルダ（名前または ID）', unit: ''},
  {key: 'GEOFRAME_BLOG_PLANS_DIR', group: 'ingest', label: '旅程 JSON のディレクトリ。未設定ならバイナリに埋め込んだ旅程を使う', unit: ''},

  {key: 'GEOFRAME_INFER_MIN', group: 'geo', label: '位置推定の窓。この範囲内の位置つき写真から推定する', unit: '分'},
  {key: 'GEOFRAME_INFER_OFFSET_MIN', group: 'geo', label: 'カメラ時計のずれの補正（負でもよい）', unit: '分'},
  {key: 'GEOFRAME_TILE_UA', group: 'geo', label: '地図タイル・地名検索の上流へ送る User-Agent（連絡先入り）', unit: ''},
  {key: 'GEOFRAME_TILE_CACHE_MAX_MB', group: 'geo', label: 'タイルキャッシュの上限（全レイヤー共有）', unit: 'MB'},
  {key: 'GEOFRAME_TILE_FETCH_PER_MIN', group: 'geo', label: 'タイルの上流取得（キャッシュミスだけ）の毎分の上限。0 は上流へ行かない', unit: '回/分'},

  {key: 'GEOFRAME_AI_ENABLED', group: 'ai', label: 'AI 全体。1 以外は全用途を無効にする（AI 検索は 503）', unit: ''},
  {key: 'GEOFRAME_AI_POLL_S', group: 'ai', label: 'AI ワーカーの周期', unit: '秒'},
  {key: 'GEOFRAME_AI_BACKLOG_POLL_S', group: 'ai', label: '仕事が残っているときの次の周期までの待ち', unit: '秒'},
  {key: 'GEOFRAME_AI_EMBED_PER_CYCLE', group: 'ai', label: '画像 embedding の 1 周期の上限', unit: '枚'},
  {key: 'GEOFRAME_AI_JUDGE_PER_CYCLE', group: 'ai', label: '写真の比較（photo_judge）の 1 周期の上限', unit: 'グループ'},
  {key: 'GEOFRAME_AI_SEARCH_PER_MIN', group: 'ai', label: '検索語の embedding（AI 検索）の毎分の上限', unit: '回/分'},
  {key: 'GEOFRAME_LOC_SUGGEST', group: 'ai', label: '撮影地の手がかり・推定。1 以外は両方の用途を無効にする', unit: ''},
  {key: 'GEOFRAME_LOC_SUGGEST_CLUE_PER_CYCLE', group: 'ai', label: '撮影地の手がかり判定の 1 周期の上限', unit: '枚'},
  {key: 'GEOFRAME_LOC_SUGGEST_GEMINI_PER_CYCLE', group: 'ai', label: '撮影地の推定（クラウド）の 1 周期の上限', unit: '日'},
  {key: 'GEOFRAME_EMBED_BACKEND', group: 'ai', label: '管理画面の AI 件数が数える embedding の識別（gemini / local）。計算する場所は AI 設定 JSON が決める', unit: ''},
  {key: 'GEOFRAME_AI_TIME_WINDOW_MIN', group: 'ai', label: '同一場面候補とみなす撮影時刻の窓', unit: '分'},
  {key: 'GEOFRAME_AI_SIMILARITY', group: 'ai', label: '同一場面候補とみなす embedding の cosine 類似度のしきい値', unit: ''},
  {key: 'GEOFRAME_AI_SHORTLIST_MAX', group: 'ai', label: '1 グループで比較に回す最大枚数（2〜6）', unit: '枚'},
  {key: 'GEOFRAME_AI_VISUAL_SIMILARITY', group: 'ai', label: '検索結果で「同じ構図」に畳む cosine 類似度のしきい値', unit: ''}
];
const CONFIG_META = new Map(CONFIG_ITEMS.map((m, i) => [m.key, Object.assign({order: i}, m)]));

const CONFIG_SOURCE = {arg: '起動引数', env: '環境変数', default: '既定', ai_config: 'AI設定JSON'};
const CONFIG_SOURCE_NOTE = {
  arg: '起動引数（--listen・--data-dir）で指定した値',
  env: '環境変数（systemd の EnvironmentFile・Environment）で指定した値',
  default: '指定が無く、既定値のままの項目',
  ai_config: 'AI 設定 JSON が決めた値'
};
const CONFIG_FILE_STATE = {
  present: ['あり', 'good'], absent: ['なし', 'dim'], unreadable: ['読めない', 'bad'],
  loaded: ['読み込み済み', 'good'], invalid: ['不正', 'bad']
};
const CONFIG_TASK = {
  image_embedding: '画像の embedding', query_embedding: '検索語の embedding', photo_judge: '写真の比較',
  location_clue: '撮影地の手がかり', location_suggest: '撮影地の推定'
};
const CONFIG_NOTE =
  '読み取り専用です（この画面から設定は変えられません）。起動時に読み込んだ設定を、出どころつきで全項目そのまま出します。' +
  '鍵のパス・接続先・連絡先・project などが含まれるので、この画面を共有（スクリーンショットや画面共有）しないでください。' +
  '鍵ファイルの中身は出ません。' +
  '設定を変えるには、環境ファイル（systemd の EnvironmentFile。本番は /etc/default/geoframe-rs と /etc/default/geoframe）と' +
  ' AI 設定 JSON（GEOFRAME_AI_CONFIG）を編集して、サービスを再起動してください。';

const NONE_TEXT = '（未設定）';
let configSeq = 0;      // 要求番号。古い応答は捨てる

function initConfig(){
  setConfigNote(null);
  const lg = $('#c-legend');
  lg.textContent = '';
  Object.keys(CONFIG_SOURCE).forEach(k => {
    const row = el('div', 'lg-row');
    row.appendChild(cfgSource(k));
    row.appendChild(el('span', 'dim', ' ' + CONFIG_SOURCE_NOTE[k]));
    lg.appendChild(row);
  });
}

function setConfigNote(err){
  const n = $('#c-note');
  n.textContent = '';
  n.appendChild(el('p', null, CONFIG_NOTE));
  if(err) n.appendChild(el('p', 'err', '設定を読めませんでした: ' + err));
}

/* 設定タブを開くたびに 1 回読む（admin.js の showTab('config') から）。応答待ちの間に開き直したら、古い応答は捨てる */
function loadConfig(){
  const mine = ++configSeq;
  fetch('/api/admin/config').then(r => r.json().then(j => {
    if(!r.ok || !j.ok) throw new Error(j && j.error ? j.error : ('HTTP ' + r.status));
    return j;
  }, () => { throw new Error('HTTP ' + r.status); })).then(d => {
    if(mine === configSeq) renderConfig(d);
  }).catch(e => {
    if(mine === configSeq) setConfigNote(e && e.message ? e.message : String(e));
  });
}

/* ==== 描画の部品 ==== */
function cfgSource(src){
  const known = Object.prototype.hasOwnProperty.call(CONFIG_SOURCE, src);
  return el('span', 'src ' + (known ? src : 'default'), known ? CONFIG_SOURCE[src] : String(src));
}
function cfgHead(table, names){
  const tr = el('tr');
  names.forEach(n => tr.appendChild(el('th', null, n)));
  table.appendChild(tr);
}
function cfgEmptyRow(table, cols, text){
  const tr = el('tr');
  const td = el('td', 'dim', text);
  td.setAttribute('colspan', String(cols));
  tr.appendChild(td);
  table.appendChild(tr);
}
function cfgNone(){ return el('span', 'dim', NONE_TEXT); }
function cfgValue(td, value, unit){
  if(value === null || value === undefined){ td.appendChild(cfgNone()); return; }
  td.appendChild(document.createTextNode(String(value)));
  if(unit) td.appendChild(el('small', null, unit));
}
function cfgDash(){ return el('span', 'dim', '—'); }
function cfgState(state){
  const s = CONFIG_FILE_STATE[state];
  return el('span', 'st ' + (s ? s[1] : 'dim'), s ? s[0] : String(state));
}

/* items を分類ごとに、CONFIG_ITEMS の順（表に無い key は API の順で末尾）に分ける。state を持つ項目はファイルの表へ */
function groupConfigItems(items){
  const groups = {files: [], ingest: [], geo: [], ai: [], other: []};
  items.forEach((it, i) => {
    const m = CONFIG_META.get(it.key);
    const g = it.state !== undefined ? 'files' : (m && m.group !== 'files' ? m.group : 'other');
    groups[g].push({it, m, at: m ? m.order : CONFIG_ITEMS.length + i});
  });
  Object.keys(groups).forEach(g => groups[g].sort((a, b) => a.at - b.at));
  return groups;
}

function cfgItemRow(entry, withState){
  const {it, m} = entry;
  const tr = el('tr');
  tr.appendChild(el('td', 'k', it.key));
  const v = el('td', 'v');
  cfgValue(v, it.value, m ? m.unit : '');
  tr.appendChild(v);
  const s = el('td', 's');
  s.appendChild(cfgSource(it.source));
  tr.appendChild(s);
  if(withState){
    const st = el('td', 'st');
    st.appendChild(it.state === undefined ? cfgDash() : cfgState(it.state));
    tr.appendChild(st);
    const mt = el('td', 'm');
    if(it.modified_at) mt.appendChild(document.createTextNode(String(it.modified_at))); else mt.appendChild(cfgDash());
    tr.appendChild(mt);
  }
  tr.appendChild(el('td', 'd', m ? m.label : '（この画面に説明が無い項目）'));
  return tr;
}
function renderItemTable(table, entries){
  table.textContent = '';
  cfgHead(table, ['項目', '値', '出どころ', '意味']);
  if(!entries.length){ cfgEmptyRow(table, 4, '項目がありません'); return; }
  entries.forEach(e => table.appendChild(cfgItemRow(e, false)));
}
function renderFilesTable(table, entries){
  table.textContent = '';
  cfgHead(table, ['項目', 'パス', '出どころ', '状態', '更新日時', '意味']);
  if(!entries.length){ cfgEmptyRow(table, 6, '項目がありません'); return; }
  entries.forEach(e => table.appendChild(cfgItemRow(e, true)));
}

/* RUNTIME のセル: 取り込み・待ち受け・データ置き場・起動からの経過・旅行ブログ・ログ・版 */
function runtimeCells(d, byKey){
  const val = k => { const it = byKey.get(k); return it && it.value !== undefined ? it.value : null; };
  const flag = (label, k) => {
    const v = val(k);
    const c = cell(label, v === null ? '--' : (v === '1' ? '有効' : '無効'), '',
                   v === '1' ? 'good' : (v === null ? '' : 'warn'));
    c.appendChild(el('span', 'k', k));
    return c;
  };
  const text = (label, v, sub) => {
    const c = cell(label, v === null || v === undefined ? '--' : String(v), '', 'sm');
    if(sub) c.appendChild(el('span', 'k', sub));
    return c;
  };
  const host = val('GEOFRAME_HOST'), port = val('GEOFRAME_PORT');
  const up = cell('起動からの経過', fmtDur(d.uptime_s));
  if(d.started_at) up.appendChild(el('span', 'k', '起動 ' + d.started_at));
  return [
    flag('取り込み INGEST', 'GEOFRAME_INGEST'),
    text('待ち受け', host !== null && port !== null ? host + ':' + port : null, 'GEOFRAME_HOST : GEOFRAME_PORT'),
    text('データ置き場', val('GEOFRAME_DATA_DIR'), 'GEOFRAME_DATA_DIR'),
    up,
    flag('旅行ブログ', 'GEOFRAME_BLOG'),
    text('ログ', val('GEOFRAME_LOG'), 'GEOFRAME_LOG'),
    text('版', d.version, 'geoframe --version')
  ];
}

/* 用途の表の「今日（UTC）/ 上限」。today_utc が null なら「—」、daily_limit があれば「今日 / 上限」とバー（90% 以上で warn） */
function taskUsageCell(t){
  const td = el('td', 'v');
  if(t.provider !== 'cloud'){ td.appendChild(cfgDash()); return td; }
  const today = t.today_utc === null || t.today_utc === undefined ? null : t.today_utc;
  const limit = t.daily_limit === null || t.daily_limit === undefined ? null : t.daily_limit;
  const nums = (today === null ? '—' : String(today)) + (limit === null ? '' : ' / ' + limit);
  td.appendChild(document.createTextNode(nums));
  if(limit === null) td.appendChild(el('small', null, '上限なし'));
  if(limit !== null && today !== null){
    const pct = limit > 0 ? Math.min(100, today / limit * 100) : 100;
    const bar = el('div', 'bar');
    const fill = el('i', limit <= 0 || today >= limit * 0.9 ? 'warn' : '');
    fill.style.width = pct + '%';
    bar.appendChild(fill);
    td.appendChild(bar);
  }
  return td;
}
function taskRow(t){
  const tr = el('tr');
  const k = el('td', 'k', String(t.task));
  if(CONFIG_TASK[t.task]) k.appendChild(el('small', 'dim', CONFIG_TASK[t.task]));
  tr.appendChild(k);
  const plain = (v, cls) => {
    const td = el('td', cls || 'v');
    if(v === null || v === undefined) td.appendChild(cfgDash()); else td.appendChild(document.createTextNode(String(v)));
    return td;
  };
  tr.appendChild(plain(t.provider));
  tr.appendChild(plain(t.model));
  tr.appendChild(plain(t.service_id));
  const cloud = el('td', 'v');
  if(t.provider === 'cloud'){
    cloud.appendChild(el('span', t.allow_cloud ? 'st good' : 'st bad', t.allow_cloud ? 'クラウド許可' : 'クラウド不許可'));
    cloud.appendChild(el('small', null, t.include_hidden ? 'hidden も送る' : 'hidden は送らない'));
  } else cloud.appendChild(cfgDash());
  tr.appendChild(cloud);
  tr.appendChild(taskUsageCell(t));
  const cred = el('td', 'v');
  if(t.credential_path === undefined || t.credential_path === null) cred.appendChild(cfgDash());
  else {
    cred.appendChild(document.createTextNode(String(t.credential_path)));
    if(t.credential) cred.appendChild(cfgState(t.credential));
  }
  tr.appendChild(cred);
  tr.appendChild(plain(t.project));
  tr.appendChild(plain(t.region));
  const dis = el('td', 'v');
  if(t.disabled) dis.appendChild(el('span', 'st bad', String(t.disabled)));
  else dis.appendChild(el('span', 'st good', '有効'));
  tr.appendChild(dis);
  const src = el('td', 's');
  src.appendChild(cfgSource(t.source));
  tr.appendChild(src);
  return tr;
}
const TASK_COLUMNS = ['用途', 'provider', 'モデル', 'サービス ID', 'クラウドと hidden', '今日（UTC）/ 日の上限',
                      '認証ファイル', 'project', 'region', '無効の理由', '出どころ'];

function renderAiPanel(ai){
  const state = $('#c-ai-state');
  state.textContent = '';
  if(ai && ai.configured === false)
    state.appendChild(el('p', 'warn', 'AI 設定 JSON が未設定です（GEOFRAME_AI_CONFIG）。AI の推論は動きません'));
  if(ai && ai.invalid)
    state.appendChild(el('p', 'err', 'AI 設定が不正: ' + ai.invalid + '（新しい AI 呼び出しだけ止まっています）'));

  const tasks = $('#c-ai-tasks');
  tasks.textContent = '';
  cfgHead(tasks, TASK_COLUMNS);
  const list = ai && Array.isArray(ai.tasks) ? ai.tasks : [];
  if(!list.length) cfgEmptyRow(tasks, TASK_COLUMNS.length, '用途の設定がありません');
  list.forEach(t => tasks.appendChild(taskRow(t)));

  const space = $('#c-ai-space');
  space.textContent = '';
  cfgHead(space, ['項目', '値', '意味']);
  const row = (k, v, label) => {
    const tr = el('tr');
    tr.appendChild(el('td', 'k', k));
    const td = el('td', 'v');
    cfgValue(td, v, '');
    tr.appendChild(td);
    tr.appendChild(el('td', 'd', label));
    space.appendChild(tr);
  };
  if(!ai){ cfgEmptyRow(space, 3, '—'); return; }
  row('enabled', ai.enabled === undefined || ai.enabled === null ? null : (ai.enabled ? '有効' : '無効'),
      'AI 全体の有効（GEOFRAME_AI_ENABLED）');
  row('version', ai.version, 'AI 設定 JSON の version');
  row('local_base_url', ai.local_base_url, 'local の用途の送り先（ベース URL）');
  const sp = ai.embedding_space;
  if(!sp) row('embedding_space', null, 'embedding の空間の識別（AI 設定 JSON）');
  else [['space_id', '空間の ID'], ['model_id', 'embedding のモデル'], ['revision', 'モデルの版'],
        ['image_preprocess', '画像の前処理'], ['text_preprocess', '文字の前処理'],
        ['dimension', '次元数'], ['normalization', '正規化']].forEach(([k, label]) =>
    row('embedding_space.' + k, sp[k], label));
}

function renderConfig(d){
  setConfigNote(null);
  const items = Array.isArray(d.items) ? d.items : [];
  const byKey = new Map(items.map(i => [i.key, i]));
  const groups = groupConfigItems(items);

  const rt = $('#c-runtime');
  rt.textContent = '';
  runtimeCells(d, byKey).forEach(c => rt.appendChild(c));
  renderFilesTable($('#c-files'), groups.files);
  renderItemTable($('#c-ingest'), groups.ingest);
  renderItemTable($('#c-geo'), groups.geo);
  renderItemTable($('#c-ai'), groups.ai);
  renderItemTable($('#c-other'), groups.other);
  $('#c-other-panel').classList.toggle('hidden', !groups.other.length);
  renderAiPanel(d.ai);
}
