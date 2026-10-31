/* ============================================================
   admin.js — 管理ページ /admin のフロント（classic script / 依存なし）
   構成: 1) 共通ヘルパ 2) タブ 3) 状況 4) 写真（チップ・一覧・カード） 5) フラグ操作
        6) 位置指定モーダル（MapLibre・地名検索・AI撮影地候補） 9) 起動
   読み込んだだけでは location・fetch に触らない。起動は DOMContentLoaded の boot() だけ
   （描画試験が vm で読み込んで aiOpsCells などを直接呼ぶため）。
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
let curFilter = 'all', curPage = 1;
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
  loadFilterCounts();
  setFilter(curFilter);
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
/* 要対応: 位置なし・AI候補あり。押すと写真タブのそのフィルタへ移る。
   curFilter を先に変えて showTab を呼ぶ（showTab が一覧を 1 回だけ読む） */
const TODO_ITEMS = [['noloc', '位置なし（要位置指定）'], ['suggest', 'AI候補あり（要確認）']];
function todoCells(filters){
  if(!filters) return [];
  return TODO_ITEMS.map(([f, label]) => {
    const n = filters[f] || 0;
    const c = cell(label, n.toLocaleString('ja-JP'), '枚', n ? 'warn' : 'good');
    const go = () => { curFilter = f; showTab('photos'); };
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

/* ==== 4) 写真（チップ・一覧・カード） ==== */
const SRC_LABEL = {exif: 'EXIF', drive_meta: 'DRIVE', inferred: '推定', manual: '手動'};
function setFilter(f){
  curFilter = f; curPage = 1;
  document.querySelectorAll('.chip').forEach(b => b.classList.toggle('on', b.dataset.filter === f));
  loadList();
}
/* チップの件数は GET /api/admin/status の db.filters（一覧 API の total と同じ条件で数えたもの） */
function loadFilterCounts(){
  fetch('/api/admin/status').then(r => r.json()).then(d => {
    if(!d.ok) throw new Error(d.error || 'status error');
    const filters = (d.db && d.db.filters) || {};
    document.querySelectorAll('.chip').forEach(b => {
      const n = b.querySelector('.n');
      const v = filters[b.dataset.filter];
      if(n) n.textContent = (v === undefined || v === null) ? '' : Number(v).toLocaleString('ja-JP');
    });
  }).catch(e => toast('件数取得失敗: ' + e.message, true));
}
let listSeq = 0;      // 速く切り替えたとき、遅れて届いた古い応答で一覧を上書きしない
function loadList(){
  const seq = ++listSeq;
  fetch('/api/admin/photos?filter=' + encodeURIComponent(curFilter) +
        '&page=' + curPage + '&per=50')
    .then(r => r.json()).then(d => {
      if(seq !== listSeq) return;
      if(!d.ok) throw new Error(d.error || 'list error');
      curPage = d.page;
      $('#pageinfo').textContent = d.total + '件 / ' + d.page + ' of ' + d.pages + 'ページ';
      $('#prev').disabled = d.page <= 1;
      $('#next').disabled = d.page >= d.pages;
      $('#empty').classList.toggle('hidden', d.items.length > 0);
      const box = $('#cards'); box.textContent = '';
      d.items.forEach(it => box.appendChild(cardFor(it)));
    }).catch(e => { if(seq === listSeq) toast('一覧取得失敗: ' + e.message, true); });
}
/* 1 行目（.l1）がカードの主役。位置があれば地名。位置が無ければ「次に何をするか」:
   AI候補 → 近い時刻の位置 → 手がかりなし。地名は位置がある行だけ表示する
   （推定を取り消された行には解決済みの地名が残っていることがあり、出すと誤解を生む） */
function cardFor(it){
  const c = el('div', 'card' + (it.hidden ? ' is-hidden' : ''));
  c.dataset.id = it.id;

  const ph = el('div', 'ph');
  if(it.thumb){
    const img = document.createElement('img');
    img.loading = 'lazy';          // 50枚同時取得を避ける
    img.decoding = 'async';
    img.alt = it.name;
    img.src = it.thumb;
    // 取得済みderivativeが消えている場合はプレースホルダに落とす
    img.addEventListener('error', () => {
      ph.textContent = '';
      ph.appendChild(el('div', 'noimg', 'NO IMAGE'));
    });
    ph.appendChild(img);
  }else{
    ph.appendChild(el('div', 'noimg', '画像未取得\n(位置未設定のため)'));
  }
  c.appendChild(ph);

  const hasLoc = (it.lat !== null && it.lat !== undefined);
  const m = el('div', 'meta');
  if(hasLoc){
    const loc = [it.pref, it.city, it.place].filter(Boolean).join(' ');
    m.appendChild(el('div', 'l1', loc || (it.lat.toFixed(4) + ', ' + it.lon.toFixed(4))));
  }else if(it.suggest){
    m.appendChild(el('div', 'l1 act', 'AI候補: ' + it.suggest.place_name));
  }else if(it.hint && it.hint.label){
    m.appendChild(el('div', 'l1 act hint', '近い時刻の位置: ' + it.hint.label +
                                           '（' + fmtDur(it.hint.dt_s) + '差）'));
  }else{
    m.appendChild(el('div', 'l1 act none', '手がかりなし'));
  }
  const l2 = el('div', 'l2');
  l2.appendChild(el('span', 'dt', it.taken_at || '撮影時刻なし'));
  l2.appendChild(el('span', 'nm', it.name));
  m.appendChild(l2);
  const src = hasLoc ? (it.loc_source || 'none') : 'none';
  m.appendChild(el('span', 'tag t-' + src, (SRC_LABEL[src] || '位置なし') +
                   (src === 'inferred' && it.infer_bucket ? ' ' + it.infer_bucket : '')));
  c.appendChild(m);

  /* ★・🚫 は押した時点の見た目（.on）を基準に反転する。同じ写真のカードが複数あっても
     setFlag が全部に応答の値を反映するので、どれから押しても食い違わない */
  const acts = el('div', 'acts');
  const fav = el('button', 'fav' + (it.fav ? ' on' : ''), '★');
  fav.title = 'お気に入り';
  fav.addEventListener('click', () => setFlag(it, {favorite: !fav.classList.contains('on')}));
  const hid = el('button', 'hid' + (it.hidden ? ' on' : ''), '🚫');
  hid.title = '表示から除外';
  hid.addEventListener('click', () => setFlag(it, {hidden: !hid.classList.contains('on')}));
  const loc2 = el('button', hasLoc ? 'setloc' : 'setloc strong', hasLoc ? '位置' : '位置を付ける');
  loc2.title = '地図で位置を指定';
  loc2.addEventListener('click', () => openLocModal(it));
  acts.appendChild(fav); acts.appendChild(hid); acts.appendChild(loc2);
  // 横倒し・逆さまで保存された写真を直す（画像があるDriveの写真だけ）
  if(it.thumb && it.source === 'drive'){
    const rot = el('button', 'rotate', '↻');
    rot.title = '時計回りに90°回して表示' + (it.rotation ? '（今 ' + it.rotation + '°）' : '');
    rot.addEventListener('click', () => rotatePhoto(c, it, rot));
    acts.appendChild(rot);
  }
  c.appendChild(acts);
  return c;
}

/* ==== 5) フラグ操作 ==== */
function setFlag(it, patch){
  postJSON('/api/admin/setflag', Object.assign({id: it.id}, patch)).then(j => {
    it.fav = j.favorite; it.hidden = j.hidden;
    document.querySelectorAll('#cards .card').forEach(card => {
      if(String(card.dataset.id) !== String(it.id)) return;
      card.querySelector('.fav').classList.toggle('on', j.favorite);
      card.querySelector('.hid').classList.toggle('on', j.hidden);
      card.classList.toggle('is-hidden', j.hidden);
    });
    toast('#' + it.id + ' 更新: ' + (j.favorite ? '★' : '☆') + (j.hidden ? ' 🚫除外' : ''));
    loadFilterCounts();           // ★・除外の件数（チップ）が変わる
  }).catch(e => toast('更新失敗: ' + e.message, true));
}

function rotatePhoto(card, it, btn){
  btn.disabled = true;
  postJSON('/api/admin/rotate', {id: it.id, delta: 90}).then(j => {
    it.rotation = j.rotation; it.thumb = j.thumb;
    const img = card.querySelector('.ph img');
    if(img) img.src = j.thumb;
    btn.title = '時計回りに90°回して表示' + (j.rotation ? '（今 ' + j.rotation + '°）' : '');
    toast('#' + it.id + ' を回しました（' + j.rotation + '°）');
  }).catch(e => toast('回転失敗: ' + e.message, true))
    .finally(() => { btn.disabled = false; });
}

/* ==== 6) 位置指定モーダル（MapLibre） ==== */
/* 背景タイルは本体と同じ Rust のプロキシ（tiles.rs）経由（ローカルキャッシュを共有できる）。
   出典表示は地理院タイル・OSMいずれも利用条件で必須なので style に持たせる */
const TILE_ATTRIB_OSM = "© <a href='https://www.openstreetmap.org/copyright'>OpenStreetMap</a> contributors";
const TILE_ATTRIB_GSI = "出典: <a href='https://maps.gsi.go.jp/development/ichiran.html'>国土地理院（地理院タイル）</a>";
let mini = null, miniMarker = null, curPhoto = null, picked = null;
let suggestMarker = null;     // AI候補の代表点（確定用の miniMarker とは色を分ける）

function ensureMini(){
  if(mini) return mini;
  mini = new maplibregl.Map({
    container: 'minimap',
    style: {version: 8, sources: {
        osm: {type: 'raster', tiles: [location.origin + '/tiles/osm/{z}/{x}/{y}.png'],
              tileSize: 256, maxzoom: 19, attribution: TILE_ATTRIB_OSM},
        gsi: {type: 'raster', tiles: [location.origin + '/tiles/gsi/{z}/{x}/{y}.png'],
              tileSize: 256, minzoom: 5, maxzoom: 18,
              bounds: [122.5, 20.2, 154.5, 46.5], attribution: TILE_ATTRIB_GSI}},
      layers: [{id: 'bg', type: 'background', paint: {'background-color': '#e8eef4'}},
               {id: 'osm', type: 'raster', source: 'osm'},
               {id: 'gsi', type: 'raster', source: 'gsi'}]},
    center: [139.7, 35.68], zoom: 9,
    attributionControl: {compact: true}
  });
  mini.addControl(new maplibregl.NavigationControl({showCompass: false}), 'top-right');
  mini.on('click', e => {
    picked = {lat: +e.lngLat.lat.toFixed(6), lon: +e.lngLat.lng.toFixed(6)};
    if(!miniMarker) miniMarker = new maplibregl.Marker({color: '#00e5ff'});
    miniMarker.setLngLat([picked.lon, picked.lat]).addTo(mini);
    $('#m-pick').textContent = picked.lat.toFixed(5) + ', ' + picked.lon.toFixed(5);
    $('#m-save').disabled = false;
  });
  return mini;
}
/* ==== 6b) 地名検索（地図を近くまで移動するショートカット。位置はここでは確定しない） ====
   国内は地理院、0件なら海外を含めてOpenStreetMap（Nominatim）で引く（サーバー側で切替）。
   候補クリック後も既存の「地図クリック→SAVE」フローは維持する。検索結果の代表点は
   住所の代表座標であり実際の撮影地点とは限らないため、自動確定はしない */
function clearGeocodeResults(){
  const box = $('#m-results');
  box.textContent = '';
  box.classList.add('hidden');
}
function doGeocodeSearch(){
  const q = $('#m-q').value.trim();
  clearGeocodeResults();
  if(!q) return;
  fetch('/api/admin/geocode?q=' + encodeURIComponent(q))
    .then(r => r.json()).then(d => {
      const box = $('#m-results');
      box.classList.remove('hidden');
      if(!d.ok || !d.items.length){
        box.appendChild(el('div', 'm-noresult',
          '見つかりませんでした（海外は「Times Square」のように地名だけで検索してください）'));
        return;
      }
      d.items.forEach(item => {
        const row = el('div', 'm-result', item.title);
        row.addEventListener('click', () => {
          ensureMini().flyTo({center: [item.lon, item.lat], zoom: 14});
          clearGeocodeResults();
        });
        box.appendChild(row);
      });
      // 海外の候補はOpenStreetMap（Nominatim）由来。ODbLの出典表示が要る
      if(d.items.some(item => item.source === 'osm')){
        const credit = el('div', 'm-noresult');
        credit.innerHTML = '地名データ ' + TILE_ATTRIB_OSM;
        box.appendChild(credit);
      }
    }).catch(e => toast('検索失敗: ' + e.message, true));
}

/* ==== 6c) AIの撮影地候補（ai/location_suggest.rs）。候補の場所へ地図を動かすだけで、確定はしない ====
   地名・根拠はモデルの出力なので必ず textContent で入れる（innerHTMLに渡さない） */
const PRECISION_LABEL = {landmark: '建物・施設', district: '地区', city: '市区町村',
                         region: '都道府県・州', country: '国', unknown: '不明'};
const PRECISION_ZOOM = {landmark: 16, district: 14, city: 11, region: 7, country: 5};
function suggestCenter(sg){
  if(!sg || sg.lat === null || sg.lat === undefined || !PRECISION_ZOOM[sg.precision]) return null;
  return {center: [sg.lon, sg.lat], zoom: PRECISION_ZOOM[sg.precision]};
}
function renderSuggest(it){
  const box = $('#m-suggest');
  box.textContent = '';
  const sg = it.suggest;
  const hasLoc = (it.lat !== null && it.lat !== undefined);
  box.classList.toggle('hidden', !sg || hasLoc);
  if(!sg || hasLoc) return;
  const row = el('div', 'row');
  row.appendChild(el('span', 'lbl', 'AI候補'));
  row.appendChild(el('span', 'place', sg.place_name));
  row.appendChild(el('span', 'meta',
    '細かさ: ' + (PRECISION_LABEL[sg.precision] || sg.precision) +
    (sg.scope === 'day' ? '（同じ日の写真から推定）' : '（この写真から推定）') +
    (sg.scope === 'photo' && sg.day_place_name && sg.day_place_name !== sg.place_name
       ? ' / この日: ' + sg.day_place_name : '')));
  const go = suggestCenter(sg);
  if(go){
    const btn = el('button', 'btn', '候補へ移動');
    btn.addEventListener('click', () => ensureMini().flyTo(go));
    row.appendChild(btn);
  }
  box.appendChild(row);
  if(sg.evidence && sg.evidence.length){
    box.appendChild(el('div', 'note', '根拠（同じ日の写真をAIに見せたときの説明。「写真N」はそのとき送った写真の番号）'));
    const ul = el('ul');
    sg.evidence.slice(0, 6).forEach(e => ul.appendChild(el('li', null, e)));
    box.appendChild(ul);
  }
  box.appendChild(el('div', 'note',
    (go ? '地図上の点は地名の代表点で、撮影地点ではありません。' : '地図上の位置は特定できませんでした。') +
    ' 確認して地図をクリックし、SAVEで確定してください（' + sg.model + '）'));
}

function openLocModal(it){
  curPhoto = it; picked = null;
  $('#m-q').value = '';
  clearGeocodeResults();
  renderSuggest(it);
  $('#m-title').textContent = '#' + it.id + '  ' + it.name +
                              (it.taken_at ? '  /  ' + it.taken_at : '');
  $('#m-pick').textContent = '';
  $('#m-save').disabled = true;
  $('#m-hint').textContent = it.hint && it.hint.label
    ? ('参考: 近い時刻の写真は「' + it.hint.label + '」付近（' +
       fmtDur(it.hint.dt_s) + '差）')
    : '地図をクリックして位置を指定してください';
  $('#modal').classList.remove('hidden');
  const m = ensureMini();
  // 非表示状態で生成されると寸法が0になるため、表示直後にresizeする
  setTimeout(() => {
    m.resize();
    if(miniMarker){ miniMarker.remove(); miniMarker = null; }
    const c = (it.lat !== null && it.lat !== undefined) ? [it.lon, it.lat]
            : (it.hint ? [it.hint.lon, it.hint.lat] : null);
    const sgo = suggestCenter(it.suggest);
    if(suggestMarker){ suggestMarker.remove(); suggestMarker = null; }
    if(sgo && !(it.lat !== null && it.lat !== undefined)){
      suggestMarker = new maplibregl.Marker({color: '#ffb300'}).setLngLat(sgo.center).addTo(m);
      suggestMarker.getElement().title = 'AI候補: ' + it.suggest.place_name + '（代表点）';
    }
    if(c) m.jumpTo({center: c, zoom: 11});
    else if(sgo) m.jumpTo(sgo);       // 近い時刻の写真が無ければAI候補の場所から始める
  }, 30);
}
function closeLocModal(){ $('#modal').classList.add('hidden'); curPhoto = null; }
function saveLoc(){
  if(!curPhoto || !picked) return;
  $('#m-save').disabled = true;
  postJSON('/api/admin/setloc', {id: curPhoto.id, lat: picked.lat, lon: picked.lon})
    .then(() => {
      toast('#' + curPhoto.id + ' の位置を設定しました');
      closeLocModal();
      loadList();
      loadFilterCounts();         // 位置なしの件数（チップ）が変わる
    })
    .catch(e => { toast('位置の保存に失敗: ' + e.message, true); $('#m-save').disabled = false; });
}

/* ==== 9) 起動 ==== */
function boot(){
  document.querySelectorAll('nav .tab').forEach(b =>
    b.addEventListener('click', () => showTab(b.dataset.tab)));
  document.querySelectorAll('.chip').forEach(b =>
    b.addEventListener('click', () => setFilter(b.dataset.filter)));
  $('#prev').addEventListener('click', () => { if(curPage > 1){ curPage--; loadList(); } });
  $('#next').addEventListener('click', () => { curPage++; loadList(); });
  $('#m-close').addEventListener('click', closeLocModal);
  $('#m-save').addEventListener('click', saveLoc);
  $('#m-search').addEventListener('click', doGeocodeSearch);
  $('#m-q').addEventListener('keydown', e => {
    if(e.key === 'Enter'){ e.preventDefault(); doGeocodeSearch(); }
  });
  $('#modal').addEventListener('click', e => { if(e.target.id === 'modal') closeLocModal(); });
  document.addEventListener('keydown', e => {
    if(e.key === 'Escape' && !$('#modal').classList.contains('hidden')) closeLocModal();
  });
  showTab(initialTab());
  setInterval(() => { if(!$('#v-status').classList.contains('hidden')) loadStatus(); }, 30000);
}
document.addEventListener('DOMContentLoaded', boot);
