/* ============================================================
   admin.js — 管理ページ /admin のフロント（classic script / 依存なし）
   構成: 1) 共通ヘルパ 2) タブ 3) 状況 4) 起動
   写真タブ（一覧・チップ・検索・カード・ビューア・位置指定）は photos.js（この後に読み込む）。
   共有はグローバル関数: photos.js は ここの $ el fmtDur toast postJSON showTab を使い、
   admin.js は photos.js の initPhotos openPhotosTab gotoFilter を（呼ぶ時点で解決して）使う。
   読み込んだだけでは location・fetch に触らない。起動は DOMContentLoaded の boot() だけ
   （描画試験が vm で読み込んで aiOpsCells などを直接呼ぶため。boot の外で photos.js の関数は呼ばない）。
   ============================================================ */
'use strict';

/* ==== 1) 共通ヘルパ ==== */
function $(sel){ return document.querySelector(sel); }
function el(tag, cls, text){
  const n = document.createElement(tag);
  if(cls) n.className = cls;
  if(text !== undefined && text !== null) n.textContent = text;
  return n;
}
function fmtBytes(b){
  if(b === null || b === undefined) return '--';
  const u = ['B','KB','MB','GB','TB'];
  let i = 0, v = b;
  while(v >= 1024 && i < u.length-1){ v /= 1024; i++; }
  return (v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)) + ' ' + u[i];
}
function fmtDur(s){
  if(s === null || s === undefined) return '--';
  if(s < 3600) return Math.round(s / 60) + '分';
  if(s < 86400) return (s / 3600).toFixed(1) + '時間';
  return (s / 86400).toFixed(1) + '日';
}
let toastTimer = null;
function toast(msg, isErr){
  const t = $('#toast');
  t.textContent = msg;
  t.className = 'show' + (isErr ? ' err' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.className = ''; }, isErr ? 5000 : 2200);
}
/* 更新系はすべてここを通す。Content-Type: application/json はサーバー側で必須
   （クロスオリジンからの単純リクエストを成立させないためのCSRF対策） */
function postJSON(path, body){
  return fetch(path, {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify(body)
  }).then(r => r.json().then(j => {
    if(!r.ok || !j.ok) throw new Error(j && j.error ? j.error : ('HTTP ' + r.status));
    return j;
  }, () => { throw new Error('HTTP ' + r.status); }));
}

/* ==== 2) タブ ==== */
/* 開いたときのタブ。/ と /admin（旅行ブログが無効のときの /blog-admin も同じ HTML）は状況。
   /ai-search を同じ HTML で返す版（写真タブへ統合したあと）では写真タブ。
   パスは先頭の連続する / を 1 つにし、末尾の / を除いて === で比べる */
function initialTab(){
  if(typeof location === 'undefined') return 'status';
  const path = String(location.pathname).replace(/^\/+/, '/').replace(/\/+$/, '');
  return path === '/ai-search' ? 'photos' : 'status';
}
function showTab(name){
  const tab = (name === 'photos') ? 'photos' : 'status';
  document.querySelectorAll('nav .tab').forEach(b =>
    b.classList.toggle('on', b.dataset.tab === tab));
  $('#v-status').classList.toggle('hidden', tab !== 'status');
  $('#v-photos').classList.toggle('hidden', tab !== 'photos');
  if(tab === 'status'){ loadStatus(); return; }
  openPhotosTab();            // photos.js: 件数・索引の枚数・一覧（検索結果を見ていたときはそのまま）
}

/* ==== 3) STATUS ==== */
function cell(k, v, unit, cls){
  const c = el('div', 'cell' + (cls ? ' ' + cls : ''));
  c.appendChild(el('span', 'k', k));
  const val = el('div', 'v');
  val.appendChild(document.createTextNode(v));
  if(unit) val.appendChild(el('small', null, unit));
  c.appendChild(val);
  return c;
}
function kvTable(node, obj){
  node.textContent = '';
  const keys = Object.keys(obj || {});
  if(!keys.length){ node.appendChild(el('tr')).appendChild(el('td', null, '—')); return; }
  keys.sort((a, b) => obj[b] - obj[a]);
  keys.forEach(k => {
    const tr = el('tr');
    tr.appendChild(el('td', null, k || '(なし)'));
    tr.appendChild(el('td', null, String(obj[k])));
    node.appendChild(tr);
  });
}
/* Driveの大量削除の保留（/api/admin/status の sync.mass_delete_hold。Rust版だけが返す）の注意表示。
   保留が無い・キーが無い（Python版・取り込み無効）ときは null を返し、何も出さない。
   解除は画面からはできない。ここには解除のコマンドを文字で示すだけ。 */
const HOLD_REASON = {
  ratio: 'Driveの一覧に無い行が多すぎる（50件超かつ全体の20%超）',
  empty_listing: 'Driveの一覧が空だった'
};
function massDeleteNotice(hold){
  if(!hold) return null;
  const c = cell('Driveの削除を保留中', hold.held, '件', 'bad');
  c.style.gridColumn = '1 / -1';
  c.appendChild(el('span', 'k',
    (HOLD_REASON[hold.reason] || hold.reason) + ' / Driveの行 ' + hold.drive_rows + '件'));
  c.appendChild(el('span', 'k', '保留の開始 ' + hold.since + ' / 最後に確認 ' + hold.checked_at));
  c.appendChild(el('span', 'k',
    '意図した削除なら、サービスを止めて geoframe sync --allow-mass-delete を1回実行（この画面からは解除できません）'));
  return c;
}
/* AI運用の状態（/api/admin/status の ai_config_invalid・ai_jobs・ai_pauses・ai_waits。Rust版だけが返し、あるときだけ入る）。
   出すのは件数と固定の理由コードだけ（対象・接続先・モデルは返らない）。無ければ空の配列。
   Provider停止の解除・失敗ジョブの再試行は画面からはできない。コマンドを文字で示すだけ。 */
const AI_JOB_STATE = {
  pending: '待機', running: '実行中', deferred: '後回し', failed: '失敗', succeeded: '完了', obsolete: '無効'
};
const AI_REASON = {
  unavailable: 'Provider利用不可', rate_limited: '429（待機）', timeout: '時間切れ', server_error: '5xx',
  bad_response: '応答の不正', input_too_large: '入力が大きすぎる', input_invalid: '入力が不正',
  policy_disabled: '設定・許可で無効', shutdown: '停止のため戻した', interrupted: '中断',
  panic: '処理の異常終了', internal: '内部エラー', provider_paused: 'Provider停止中',
  budget_exhausted: '送信予算を使い切り', clock_rewound: '時計が巻き戻っている（送信予算を守るため送信しない）',
  other: 'その他'
};
const AI_PAUSE_REASON = {
  unauthorized: '認証エラー（401）', forbidden: '権限エラー（403）', model_not_found: 'モデルが無い（404）',
  contract_mismatch: '契約の不一致'
};
/* ai_waits: まだ有効な共有の一時待機（到達不能・GPU lease・429など。期限が来れば自然に消える）。
   items は {task, scope: entrance|service, reason, until}。untilはUTCのISO 8601（YYYY-MM-DDTHH:MM:SSZ）で、
   画面では「YYYY-MM-DD HH:MM:SS UTC」と書く。unidentified は設定が使えず用途に結び直せない件数。 */
const AI_WAIT_REASON = {
  unreachable: '到達できない', leased: 'GPUのlease中', not_ready: 'サービスが未準備',
  rate_limited: '429（レート制限）', busy: '混雑中'
};
const AI_WAIT_SCOPE = {entrance: '入口全体', service: 'サービス単体'};
function fmtUtc(iso){
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})Z$/.exec(String(iso));
  return m ? m[1] + ' ' + m[2] + ' UTC' : '--';
}
function aiOpsCells(d){
  const cells = [];
  if(d.ai_config_invalid){
    const c = cell('AI設定が不正', d.ai_config_invalid.code, '', 'bad');
    c.appendChild(el('span', 'k',
      '新しいAI呼び出しだけ止まっています（写真の表示・管理は継続）。設定を直して再起動してください'));
    cells.push(c);
  }
  const jobs = d.ai_jobs;
  if(jobs){
    const by = jobs.by_state || {};
    Object.keys(AI_JOB_STATE).forEach(st => {
      if(!by[st]) return;
      const bad = st === 'failed';
      const c = cell('AIジョブ ' + AI_JOB_STATE[st], by[st], '件',
                     bad ? 'bad' : (st === 'deferred' ? 'warn' : ''));
      const reasons = (jobs.reasons || {})[st] || {};
      Object.keys(reasons).forEach(code => {
        c.appendChild(el('span', 'k', (AI_REASON[code] || code) + ' ' + reasons[code] + '件'));
      });
      if(bad) c.appendChild(el('span', 'k',
        'サービスを止めて geoframe ai-jobs retry --all で再検査（この画面からは操作できません）'));
      cells.push(c);
    });
  }
  const waits = d.ai_waits;
  if(waits && ((waits.items && waits.items.length) || waits.unidentified)){
    const items = waits.items || [];
    const c = cell('AI一時待機中', items.length + (waits.unidentified || 0), '件', 'warn');
    items.forEach(w => {
      c.appendChild(el('span', 'k',
        w.task + '（' + (AI_WAIT_SCOPE[w.scope] || w.scope) + '）: ' +
        (AI_WAIT_REASON[w.reason] || w.reason) + ' / 期限 ' + fmtUtc(w.until)));
    });
    if(waits.unidentified) c.appendChild(el('span', 'k',
      '用途を特定できない待ち ' + waits.unidentified + '件（AI設定が使えないため）'));
    c.appendChild(el('span', 'k', '期限が来れば自動で再開します（操作は不要）'));
    cells.push(c);
  }
  const pauses = d.ai_pauses;
  if(pauses && pauses.length){
    const c = cell('AI停止中（Provider・用途）', pauses.length, '件', 'bad');
    pauses.forEach(p => {
      c.appendChild(el('span', 'k',
        p.task + '（' + (p.scope === 'provider' ? 'Provider全体' : '用途') + '）: ' +
        (AI_PAUSE_REASON[p.reason] || p.reason)));
    });
    c.appendChild(el('span', 'k',
      '設定を直して再起動するか、サービスを止めて geoframe ai-jobs release-pause --task <用途> で解除'));
    cells.push(c);
  }
  /* ai_task_blocks: 停止にかかって送られていない用途（停止を起こした用途と、同じ認証・接続先を使う用途）。
     {task, scope: provider|task, reason, waiting}。waiting はその用途の待ち（pending+deferred）の件数。集計できなかったときは null（件数不明）。 */
  const blocks = d.ai_task_blocks;
  if(blocks && blocks.length){
    const c = cell('AI送信が止まっている用途', blocks.length, '件', 'bad');
    blocks.forEach(b => {
      const waiting = b.waiting === null || b.waiting === undefined ? '待ちは件数不明' : (Number(b.waiting) || 0) + ' 件が待ち';
      c.appendChild(el('span', 'k',
        b.task + ': ' + (b.scope === 'provider' ? 'Provider全体' : '用途') + 'の停止（' +
        (AI_PAUSE_REASON[b.reason] || b.reason) + '）で ' + waiting));
    });
    c.appendChild(el('span', 'k', '停止中は待ちのジョブを送りません。解除すると再開します'));
    cells.push(c);
  }
  return cells;
}
/* 要対応: 位置なし・AI候補あり。押すと写真タブのそのフィルタへ移る（photos.js の gotoFilter が
   フィルタを変えて showTab を呼ぶので、一覧は 1 回だけ読まれる） */
const TODO_ITEMS = [['noloc', '位置なし（要位置指定）'], ['suggest', 'AI候補あり（要確認）']];
function todoCells(filters){
  if(!filters) return [];
  return TODO_ITEMS.map(([f, label]) => {
    const n = filters[f] || 0;
    const c = cell(label, n.toLocaleString('ja-JP'), '枚', n ? 'warn' : 'good');
    const go = () => gotoFilter(f);
    c.classList.add('todo');
    c.setAttribute('role', 'button');
    c.setAttribute('tabindex', '0');
    c.addEventListener('click', go);
    c.addEventListener('keydown', e => {
      if(e.key === 'Enter' || e.key === ' '){ e.preventDefault(); go(); }
    });
    return c;
  });
}
function loadStatus(){
  fetch('/api/admin/status').then(r => r.json()).then(d => {
    if(!d.ok) throw new Error(d.error || 'status error');
    $('#servertime').textContent = 'SERVER ' + d.server_time;

    const td = $('#s-todo'); td.textContent = '';
    todoCells(d.db.filters).forEach(c => td.appendChild(c));

    const sy = $('#s-sync'); sy.textContent = '';
    sy.appendChild(cell('表示可能', d.sync.ready, '枚', 'good'));
    sy.appendChild(cell('取得待ち', d.sync.pending, '枚', d.sync.pending ? 'warn' : ''));
    sy.appendChild(cell('全件数', d.sync.total, '枚'));
    const hold = massDeleteNotice(d.sync.mass_delete_hold);
    if(hold) sy.appendChild(hold);

    const db = $('#s-db'); db.textContent = '';
    db.appendChild(cell('位置なし（lat NULL）', d.db.no_loc, '枚', d.db.no_loc ? 'warn' : 'good'));
    db.appendChild(cell('表示ファイル未生成', d.db.no_display, '枚'));
    db.appendChild(cell('★ お気に入り', d.db.favorite, '枚'));
    db.appendChild(cell('🚫 除外（hidden）', d.db.hidden, '枚'));
    db.appendChild(cell('地名の解決待ち', d.db.enrich_pending, '枚'));
    db.appendChild(cell('撮影時刻なし', d.db.no_time, '枚'));
    if(d.db.quality){
      db.appendChild(cell('品質特徴量を解析済み', d.db.quality.features, '枚', 'good'));
      db.appendChild(cell('品質解析待ち', d.db.quality.pending, '枚', d.db.quality.pending ? 'warn' : 'good'));
      db.appendChild(cell('重複・類似グループ対象', d.db.quality.analyzed, '枚'));
      db.appendChild(cell('品質推奨', d.db.quality.recommended, '枚'));
      db.appendChild(cell('完全重複の表示抑止', d.db.quality.exact_suppressed, '枚'));
      db.appendChild(cell('類似alternate', d.db.quality.similar_alternates, '枚'));
    }
    if(d.db.ai){
      db.appendChild(cell('AI embedding', d.db.ai.embedded + ' / ' + d.db.ai.eligible, '枚',
                          d.db.ai.pending ? 'warn' : 'good'));
      db.appendChild(cell('AI embedding待ち', d.db.ai.pending, '枚', d.db.ai.pending ? 'warn' : 'good'));
      db.appendChild(cell('AI場面グループ', d.db.ai.groups, '組'));
      db.appendChild(cell('Gemini比較待ち', d.db.ai.group_pending, '組',
                          d.db.ai.group_pending ? 'warn' : 'good'));
      db.appendChild(cell('AIによる表示抑止', d.db.ai.suppressed, '枚'));
    }
    if(d.db.loc){
      const lc = d.db.loc;
      db.appendChild(cell('Decision（前回の処理時）',
                          lc.decision === 'ready' ? '接続中' : (lc.decision ? '未接続' : '未実行'), '',
                          lc.decision === 'ready' ? 'good' : (lc.clue_pending ? 'warn' : '')));
      db.appendChild(cell('手がかり判定待ち', lc.clue_pending, '枚', lc.clue_pending ? 'warn' : 'good'));
      db.appendChild(cell('手がかりあり', lc.clue_passed + ' / ' + lc.targets, '枚'));
      db.appendChild(cell('撮影地を推定する日', lc.runs_done + ' / ' + lc.days_with_clue, '日',
                          lc.runs_done < lc.days_with_clue ? 'warn' : 'good'));
      db.appendChild(cell('AI撮影地候補のある写真', lc.suggested, '枚'));
      if(lc.runs_error) db.appendChild(cell('撮影地の推定に失敗', lc.runs_error, '日', 'bad'));
    }
    aiOpsCells(d).forEach(c => db.appendChild(c));
    kvTable($('#s-locsrc'), d.db.by_loc_source);
    kvTable($('#s-buckets'), d.db.infer_buckets);
    kvTable($('#s-source'), d.db.by_source);

    const dk = $('#s-disk'); dk.textContent = '';
    const pct = d.disk.used_pct;
    const lvl = pct >= 90 ? 'bad' : (pct >= 75 ? 'warn' : '');
    const c0 = cell('ディスク空き', fmtBytes(d.disk.free_bytes), '', lvl);
    const bar = el('div', 'bar');
    const fill = el('i', lvl); fill.style.width = Math.min(100, pct) + '%';
    bar.appendChild(fill);
    c0.appendChild(bar);
    c0.appendChild(el('span', 'k', '使用 ' + pct + '% / 全体 ' + fmtBytes(d.disk.total_bytes)));
    dk.appendChild(c0);
    const names = {derivatives: 'derivatives/', tile_cache: 'tile_cache/'};
    Object.keys(names).forEach(k => {
      const u = d.disk.dirs[k] || {};
      const c = cell(names[k], fmtBytes(u.bytes), '');
      c.appendChild(el('span', 'k', (u.files || 0) + ' ファイル'));
      dk.appendChild(c);
    });

  }).catch(e => toast('STATUS取得失敗: ' + e.message, true));
}

/* ==== 4) 起動 ==== */
/* 写真タブ（photos.js）は initPhotos() でイベントを結ぶ。photos.js は admin.js の後に読み込まれるが、
   boot() が呼ばれる時点（DOMContentLoaded）では両方とも定義済み */
function boot(){
  document.querySelectorAll('nav .tab').forEach(b =>
    b.addEventListener('click', () => showTab(b.dataset.tab)));
  initPhotos();
  const tab = initialTab();
  showTab(tab);
  setInterval(() => { if(!$('#v-status').classList.contains('hidden')) loadStatus(); }, 30000);
  if(tab === 'photos') $('#q').focus();     // /ai-search で開いたときは検索欄から始める
}
document.addEventListener('DOMContentLoaded', boot);
