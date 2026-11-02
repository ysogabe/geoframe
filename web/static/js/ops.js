/* ============================================================
   ops.js — 管理ページの運用タブ（classic script / 依存なし。定義だけ）
   admin.js → photos.js → library.js → curation.js → config.js → ops.js の順に読み込む。共有はグローバル関数: ここは admin.js の
   $ el cell toast postJSON と HOLD_REASON を使い、admin.js は ここの initOps openOpsTab stopOpsPolling を（typeof で守って）使う。
   データは GET /api/admin/ops だけ（運用タブを表示しているあいだ 3 秒ごとに読む。離れると止める）。DB は読まない API なので
   AI の件数などは持たない。操作は POST /api/admin/ops/sync・quality-scan・drive-release（docs/admin-ops/design.md）。
   大量削除の保留の解除は 2 段階（「解除する…」→ 確認の段 →「N 件の削除を許可」）。確認の段は開いた時点の token・件数を固定して持ち、
   読み直しで保留の token が変わる・保留が消えたら閉じる（見ている最中に承認の対象が入れ替わらない）。
   描画は textContent と DOM API だけ（HTML 文字列は組まない）。
   ============================================================ */
'use strict';

const OPS_POLL_MS = 3000;

/* 画面の状態。snap はサーバーの最後の応答、sending は POST を出して応答を待っている種類（連打の防止）。
   confirming は確認の段 {token, held}（開いた時点の値で固定）。seq は読み直しの要求番号（古い応答を捨てる） */
const os = {seq: 0, timer: null, snap: null, sending: new Set(), confirming: null, loadFailed: false, sig: {}};

const OPS_KIND = {
  cycle: '定期サイクル', sync: '同期', quality_scan: '品質解析（一括）', drive_release: '大量削除の保留の解除'
};
const OPS_STAGE_RESULT = {
  done: '完了', skipped: '省略', failed: '失敗', panicked: 'パニック', not_run: '未実行'
};
const OPS_OUTCOME = {
  done: '完了', partial: '一部の段が失敗', failed: '失敗', skipped: '省略',
  exceeded: '保留に戻した（承認した範囲を超えた）', hold_changed: '保留が変わったので実行せず'
};
const OPS_OUTCOME_CLS = {done: 'good', partial: 'warn', failed: 'bad', skipped: 'warn', exceeded: 'warn', hold_changed: 'warn'};
/* POST の失敗の符号 → 日本語。HTTP 503 は取り込みが無効か、停止の合図のあと */
const OPS_ERROR = {
  busy: 'すでに実行中か待ちがあります',
  too_soon: '前回の同期・解除から 60 秒は待ってください',
  hold_changed: '保留が変わったので読み直しました',
  no_hold: '保留が変わったので読み直しました',
  not_releasable: 'この保留は画面から解除できません',
  bad_params: '要求の形が正しくありません',
  'HTTP 503': '取り込みが無効か停止中です'
};

function opsKindLabel(kind){ return OPS_KIND[kind] || String(kind); }
function opsErrorText(e){
  const msg = e && e.message ? e.message : String(e);
  return OPS_ERROR[msg] || ('失敗しました: ' + msg);
}

/* ---- 状態の読み取り ---- */
function opsWorker(){ return os.snap && os.snap.worker ? os.snap.worker : null; }
/* 同じ種類が、送信中・実行中・待ちのどれかにあるか（ボタンの disabled） */
function opsBusy(kind){
  if(os.sending.has(kind)) return true;
  const w = opsWorker();
  return !!w && ((w.running && w.running.kind === kind) || (w.queued && w.queued.kind === kind));
}
function opsIngest(){ return !!os.snap && os.snap.ingest === true; }

/* ---- 描画の小物 ---- */
/* 区画の中身が前回と同じなら作り直さない（3 秒ごとの読み直しで、押している最中のボタンを消さない） */
function opsRebuild(key, node, signature, build){
  const sig = JSON.stringify(signature);
  if(os.sig[key] === sig) return;
  os.sig[key] = sig;
  node.textContent = '';
  build(node);
}
function opsLine(cls, text){ return el('p', cls, text); }
function opsFormatCounts(counts){
  if(!counts) return '';
  return Object.keys(counts).map(k => k + ' ' + counts[k]).join(' / ');
}
/* 段の表（名前・結果・理由・件数） */
function opsStageTable(stages){
  const table = el('table', 'cfg ops-stages');
  const head = el('tr');
  ['段', '結果', '理由', '件数'].forEach(h => head.appendChild(el('th', null, h)));
  table.appendChild(head);
  if(!stages || !stages.length){
    const tr = el('tr');
    const td = el('td', 'dim', '段の記録はありません');
    td.colSpan = 4;
    tr.appendChild(td);
    table.appendChild(tr);
    return table;
  }
  stages.forEach(s => {
    const tr = el('tr');
    tr.dataset.stage = s.name;
    tr.appendChild(el('td', 'k', s.name));
    const res = el('td', 'v');
    res.appendChild(el('span', 'st ' + (s.result === 'done' ? 'good' : (s.result === 'failed' || s.result === 'panicked' ? 'bad' : 'warn')),
      OPS_STAGE_RESULT[s.result] || s.result));
    tr.appendChild(res);
    tr.appendChild(el('td', 'd', s.reason || ''));
    tr.appendChild(el('td', 'm', opsFormatCounts(s.counts)));
    table.appendChild(tr);
  });
  return table;
}
/* 保留の解除の記録なら、承認した件数と削除した件数（index_drive の counts）を 1 行にする */
function opsReleaseSummary(rec){
  if(rec.kind !== 'drive_release') return null;
  const st = (rec.stages || []).find(s => s.name === 'index_drive');
  const c = st && st.counts;
  if(!c || c.approved === undefined) return null;
  return '承認 ' + c.approved + ' 件・削除 ' + (c.deleted === undefined ? 0 : c.deleted) + ' 件';
}
/* 終わった実行の記録 1 件（履歴・直近の定期サイクル） */
function opsRecord(rec){
  const box = el('div', 'op-rec');
  box.dataset.id = rec.id;
  const head = el('div', 'op-rec-head');
  head.appendChild(el('span', 'op-kind', opsKindLabel(rec.kind) + (rec.kind === 'cycle' ? '' : ' #' + rec.id)));
  const outcome = rec.outcome ? (OPS_OUTCOME[rec.outcome] || rec.outcome) : '実行中';
  head.appendChild(el('span', 'st ' + (OPS_OUTCOME_CLS[rec.outcome] || ''), outcome));
  box.appendChild(head);
  const times = [];
  if(rec.requested_at) times.push('受付 ' + rec.requested_at);
  times.push('開始 ' + (rec.started_at || '—'));
  times.push('終了 ' + (rec.finished_at || '—'));
  box.appendChild(el('div', 'op-times', times.join(' / ')));
  const summary = opsReleaseSummary(rec);
  if(summary) box.appendChild(el('div', 'op-summary', summary));
  if(rec.report) box.appendChild(el('div', 'op-summary', opsFormatCounts(rec.report)));
  box.appendChild(opsStageTable(rec.stages));
  return box;
}

/* ---- 区画 1: 操作 ---- */
function renderOpsActions(){
  const ingest = opsIngest();
  const loaded = !!os.snap;
  $('#o-readonly').classList.toggle('hidden', !(loaded && !ingest));
  $('#o-actions').classList.toggle('hidden', !ingest);
  $('#o-sync').disabled = !ingest || opsBusy('sync');
  $('#o-quality').disabled = !ingest || opsBusy('quality_scan');
}

/* ---- 区画 2: Drive の大量削除の保留 ---- */
function openOpsConfirm(hold){
  os.confirming = {token: hold.token, held: hold.held};
  renderOps();
}
function closeOpsConfirm(){
  os.confirming = null;
  renderOps();
}
function renderOpsHold(){
  const hold = opsIngest() && os.snap.hold ? os.snap.hold : null;
  $('#o-hold-panel').classList.toggle('hidden', !hold);
  if(!hold){
    opsRebuild('hold', $('#o-hold'), null, () => {});
    return;
  }
  const confirming = os.confirming && os.confirming.token === hold.token ? os.confirming : null;
  const busy = opsBusy('drive_release');
  opsRebuild('hold', $('#o-hold'), {hold, confirming, busy}, node => {
    const grid = el('div', 'grid');
    const c = cell('Driveの削除を保留中', hold.held, '件', 'bad');
    c.appendChild(el('span', 'k', (HOLD_REASON[hold.reason] || hold.reason) + ' / Driveの行 ' + hold.drive_rows + '件'));
    c.appendChild(el('span', 'k', '保留の開始 ' + hold.since + ' / 最後に確認 ' + hold.checked_at));
    c.style.gridColumn = '1 / -1';
    grid.appendChild(c);
    node.appendChild(grid);
    if(!hold.releasable){
      node.appendChild(opsLine('op-note warn',
        '一覧が空のための保留は、この画面からは解除できません。Drive の共有やフォルダの設定を確かめたうえで、' +
        'serve を止めて CLI（geoframe sync --allow-mass-delete）で解除してください。'));
      return;
    }
    if(!confirming){
      const row = el('div', 'op-row');
      const open = el('button', 'btn', '解除する…');
      open.id = 'o-hold-open';
      open.disabled = busy;
      open.addEventListener('click', () => openOpsConfirm(hold));
      row.appendChild(open);
      row.appendChild(el('span', 'op-hint', '件数と範囲を確かめてから、削除を許可します（次の画面で確認）'));
      node.appendChild(row);
      return;
    }
    const box = el('div', 'op-confirm');
    box.id = 'o-hold-confirm';
    box.appendChild(el('h3', null, '削除の確認'));
    box.appendChild(opsLine('op-confirm-scope',
      'Drive の一覧を取り直し、承認した ' + confirming.held + ' 件のうち引き続き一覧に無い行だけを削除します。' +
      '承認していない行が 1 件でも消えようとすれば、削除せず再び保留します。'));
    box.appendChild(opsLine('op-confirm-loss',
      '消えるのは Drive の写真のカタログの行と表示用画像です。その写真に付けた位置（手で直したものを含む）・★・除外・回転・' +
      'タイトルも一緒に消え、これらの写真を使った旅行ブログの下書きは画像が抜けます。原本（Drive 側）には触れません。'));
    box.appendChild(opsLine('op-confirm-copy',
      '消す前に、解除前の DB の写しを DATA_DIR の下に 1 つ残します（次の解除で上書き）。'));
    const acts = el('div', 'op-row');
    const allow = el('button', 'btn danger', confirming.held + ' 件の削除を許可');
    allow.id = 'o-hold-allow';
    allow.disabled = busy;
    allow.addEventListener('click', () => releaseOpsHold());
    const cancel = el('button', 'btn', 'やめる');
    cancel.id = 'o-hold-cancel';
    cancel.addEventListener('click', closeOpsConfirm);
    acts.appendChild(allow);
    acts.appendChild(cancel);
    box.appendChild(acts);
    node.appendChild(box);
  });
}

/* ---- 区画 3: 定期サイクル ---- */
function renderOpsCycle(){
  const w = opsWorker();
  const snap = os.snap;
  const node = $('#o-cycle');
  const poll = snap && snap.poll_minutes ? snap.poll_minutes : null;
  opsRebuild('cycle', node, {ingest: opsIngest(), loaded: !!snap, w, poll}, box => {
    if(!snap){ box.appendChild(opsLine('dim', '読み込み中…')); return; }
    if(!opsIngest()) return;
    const state = el('div', 'op-state');
    if(w.running){
      state.appendChild(el('p', 'op-running',
        '実行中: ' + opsKindLabel(w.running.kind) + (w.running.kind === 'cycle' ? '' : ' #' + w.running.id) +
        '（開始 ' + w.running.started_at + '）'));
    }
    if(w.queued){
      state.appendChild(el('p', 'op-queued',
        '待ち: ' + opsKindLabel(w.queued.kind) + ' #' + w.queued.id + '（受付 ' + w.queued.requested_at + '）'));
    }
    state.appendChild(el('p', 'op-next',
      '次の定期サイクル: ' + (w.next_cycle_at || (w.running && w.running.kind === 'cycle' ? '実行中' : '—')) +
      (poll ? '（間隔 ' + poll + ' 分）' : '')));
    box.appendChild(state);
    if(w.last_cycle) box.appendChild(opsRecord(w.last_cycle));
    else box.appendChild(opsLine('dim', 'まだ定期サイクルは終わっていません'));
  });
}

/* ---- 区画 4: 履歴（新しい順） ---- */
function renderOpsHistory(){
  const history = opsIngest() && Array.isArray(os.snap.history) ? os.snap.history.slice() : [];
  /* サーバーは新しい順で返すが、画面でも番号の大きい順にそろえる（番号は受け付けた順） */
  history.sort((a, b) => b.id - a.id);
  opsRebuild('history', $('#o-history'), {loaded: !!os.snap, history}, box => {
    if(!os.snap){ box.appendChild(opsLine('dim', '読み込み中…')); return; }
    if(!history.length){ box.appendChild(opsLine('dim', 'まだ操作の履歴はありません（再起動で消えます）')); return; }
    history.forEach(rec => box.appendChild(opsRecord(rec)));
  });
}

function renderOps(){
  renderOpsActions();
  renderOpsHold();
  renderOpsCycle();
  const ingest = opsIngest();
  $('#o-cycle-panel').classList.toggle('hidden', !!os.snap && !ingest);
  $('#o-history-panel').classList.toggle('hidden', !!os.snap && !ingest);
  renderOpsHistory();
}

/* ---- 読み直し ---- */
/* 読み直しで確認の段が古くなっていたら（保留が無い・token が違う）閉じて知らせる */
function reconcileOpsConfirm(){
  if(!os.confirming) return;
  const hold = opsIngest() && os.snap.hold ? os.snap.hold : null;
  if(hold && hold.token === os.confirming.token) return;
  os.confirming = null;
  toast('保留が変わったので確認を閉じました。件数を確かめ直してください', true);
}
/* 応答を反映するか捨てるかを決めたあとで解決する Promise を返す */
function loadOps(){
  const seq = ++os.seq;
  return fetch('/api/admin/ops').then(r => r.json()).then(d => {
    if(seq !== os.seq) return;              // もっと新しい要求を出したので、古い応答は捨てる
    if(!d || !d.ok) throw new Error(d && d.error ? d.error : 'ops error');
    os.snap = d;
    os.loadFailed = false;
    reconcileOpsConfirm();
    renderOps();
  }).catch(e => {
    if(seq !== os.seq) return;
    if(!os.loadFailed) toast('運用の状態を読めません: ' + (e && e.message ? e.message : e), true);
    os.loadFailed = true;
  });
}

/* ---- 操作 ---- */
/* POST を出して、応答（成功・失敗）が返り、読み直しが済むまで同じ種類のボタンを押せなくする */
function postOps(kind, path, body, okText){
  if(os.sending.has(kind)) return Promise.resolve();
  os.sending.add(kind);
  renderOps();
  let closeConfirm = false;
  return postJSON(path, body).then(j => {
    toast(typeof okText === 'function' ? okText(j) : okText);
    closeConfirm = kind === 'drive_release';                 // 許可を出した: 確認の段は閉じる
  }, e => {
    const msg = e && e.message ? e.message : '';
    toast(opsErrorText(e), true);
    /* 保留が変わった・無い・解除できない: 確認の段は閉じて読み直す（busy・too_soon は開いたまま、待ってやり直せる） */
    closeConfirm = msg === 'hold_changed' || msg === 'no_hold' || msg === 'not_releasable';
  }).then(() => {
    if(closeConfirm) os.confirming = null;
    return loadOps();
  }).then(() => {
    os.sending.delete(kind);
    renderOps();
  });
}
function releaseOpsHold(){
  if(!os.confirming) return Promise.resolve();
  return postOps('drive_release', '/api/admin/ops/drive-release', {token: os.confirming.token},
    j => '削除の許可を受け付けました（承認 ' + (j && j.approved !== undefined ? j.approved : os.confirming ? os.confirming.held : '?') + ' 件）');
}

/* ---- 入口 ---- */
/* ボタンのイベントを結ぶ（読み込みはしない）。保留の区画のボタンは描画のたびに作る */
function initOps(){
  $('#o-sync').addEventListener('click', () =>
    postOps('sync', '/api/admin/ops/sync', {}, '同期を受け付けました'));
  $('#o-quality').addEventListener('click', () =>
    postOps('quality_scan', '/api/admin/ops/quality-scan', {}, '品質解析を受け付けました'));
  renderOps();
}
/* 運用タブに入るとき: 確認の段は閉じて、読み直しと 3 秒ごとの読み直しを始める */
function openOpsTab(){
  stopOpsPolling();
  os.confirming = null;
  loadOps();
  os.timer = setInterval(loadOps, OPS_POLL_MS);
}
/* 運用タブを離れるとき（どのタブへ移るときも呼んでよい） */
function stopOpsPolling(){
  if(os.timer === null) return;
  clearInterval(os.timer);
  os.timer = null;
}
