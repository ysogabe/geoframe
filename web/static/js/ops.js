/* ============================================================
   ops.js — 管理ページの運用タブ（classic script / 依存なし。定義だけ）
   admin.js → photos.js → library.js → curation.js → config.js → ops.js の順に読み込む。共有はグローバル関数: ここは admin.js の
   $ el cell toast postJSON と HOLD_REASON を使い、admin.js は ここの initOps openOpsTab stopOpsPolling を（typeof で守って）使う。
   データは GET /api/admin/ops（運用タブを表示しているあいだ 3 秒ごとに読む。離れると止める）。DB は読まない API なので AI の件数・
   失敗の理由・停止・待ちは持たず、GET /api/admin/status を、運用タブを開いたときと AI の操作のあとに 1 回ずつ併読して出す
   （3 秒ごとには読まない）。操作は POST /api/admin/ops/sync・quality-scan・drive-release・ai-retry・ai-release-pause
   （docs/admin-ops/design.md）。AI の 2 操作はサーバーで同じ排他なので、画面でもどちらかが送信中なら両方押せない。
   大量削除の保留の解除は 2 段階（「解除する…」→ 確認の段 →「N 件の削除を許可」）。確認の段は開いた時点の token・件数を固定して持ち、
   読み直しで保留の token が変わる・保留が消えたら閉じる（見ている最中に承認の対象が入れ替わらない）。
   admin.js の AI_REASON・AI_PAUSE_REASON・AI_WAIT_REASON・AI_WAIT_SCOPE・fmtUtc も使う。
   描画は textContent と DOM API だけ（HTML 文字列は組まない）。
   ============================================================ */
'use strict';

const OPS_POLL_MS = 3000;

/* 画面の状態。snap はサーバーの最後の応答、sending は POST を出して応答を待っている種類（連打の防止）。
   confirming は確認の段 {token, held}（開いた時点の値で固定）。seq は読み直しの要求番号（古い応答を捨てる）。
   status は /api/admin/status の最後の応答（AI の区画用）、statusSeq はその要求番号（seq と同じ規則） */
const os = {seq: 0, timer: null, snap: null, sending: new Set(), confirming: null, loadFailed: false, sig: {},
            status: null, statusSeq: 0, statusFailed: false};

const OPS_KIND = {
  cycle: '定期サイクル', sync: '同期', quality_scan: '品質解析（一括）', drive_release: '大量削除の保留の解除',
  ai_retry: 'AI ジョブの再試行', ai_release_pause: 'AI の停止の解除'
};
const OPS_STAGE_RESULT = {
  done: '完了', skipped: '省略', failed: '失敗', panicked: 'パニック', not_run: '未実行'
};
const OPS_OUTCOME = {
  done: '完了', partial: '一部の段が失敗', failed: '失敗', skipped: '省略',
  exceeded: '保留に戻した（承認した範囲を超えた）', hold_changed: '保留が変わったので実行せず',
  retried: '再試行した', released: '停止を解除した'
};
const OPS_OUTCOME_CLS = {done: 'good', partial: 'warn', failed: 'bad', skipped: 'warn', exceeded: 'warn', hold_changed: 'warn',
                         retried: 'good', released: 'good'};
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
/* AI の 2 操作（ai-retry・ai-release-pause）の失敗。busy・500 の意味が同期系と違うので、こちらを先に引く */
const OPS_AI_ERROR = {
  busy: 'AI の操作を実行中です',
  config_unavailable: 'AI 設定が無いので用途を指定した解除はできません（すべて解除は可能）',
  '内部エラーが発生しました': '失敗しました（済んだ分は履歴に残ります）'
};
/* サーバーの 503 の定型文（post.rs の Unavailable）。postJSON は応答の error をそのまま例外の文にするので、先頭で見分ける */
const OPS_UNAVAILABLE_PREFIX = '書き込みが無効なので更新できません';

function opsKindLabel(kind){ return OPS_KIND[kind] || String(kind); }
function opsErrorText(e, extra){
  const msg = e && e.message ? e.message : String(e);
  if(extra && extra[msg]) return extra[msg];
  if(msg.startsWith(OPS_UNAVAILABLE_PREFIX)) return OPS_ERROR['HTTP 503'];
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
/* AI の 2 操作は同じ排他: どちらかが送信中なら両方押せない */
function opsAiBusy(){ return os.sending.has('ai_retry') || os.sending.has('ai_release_pause'); }
const OPS_AI_KINDS = ['ai_retry', 'ai_release_pause'];

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
/* AI の操作の結果（report）を 1 行にする。再試行は 5 つの件数、停止の解除は解除した件数 */
function opsReportText(report){
  if(!report) return '';
  if(report.requeued !== undefined){
    return '再登録 ' + report.requeued + '・不要 ' + report.obsolete + '・済み ' + report.satisfied +
      '・検査不可 ' + report.unsupported + '・無効 ' + report.policy_disabled;
  }
  if(report.released !== undefined) return '停止を ' + report.released + ' 件解除';
  return opsFormatCounts(report);
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
  if(rec.report) box.appendChild(el('div', 'op-summary', opsReportText(rec.report)));
  /* AI の操作に段は無い（件数は上の 1 行） */
  if(OPS_AI_KINDS.indexOf(rec.kind) < 0) box.appendChild(opsStageTable(rec.stages));
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

/* ---- 区画 1b: AI ジョブ ---- */
/* R7 の案内（解除の横に常に出す） */
const OPS_AI_RELEASE_NOTE =
  '解除すると、認証が直っていれば背景ワーカーが次に取り出すときからクラウドへ送り直す（料金がかかる。日の上限はそのまま効く）。' +
  '設定ファイルを直した場合は再起動するまで新しい設定にならず、直っていなければ同じ理由で再び止まる。' +
  '一時待機（ai_waits）と各ジョブの再試行の予定時刻は残るので、その期限までは送らない。';
const OPS_AI_RETRY_NOTE =
  '失敗した AI ジョブを現在の入力で再検査し、再登録・不要・済みに振り分ける（外部へは送らない。送るのは背景ワーカー）。' +
  '検査できない種類・設定で使えない用途は戻さず failed のまま残す。';
/* /api/admin/status から AI の区画に要る分だけ取り出す（署名にも使う）。status が無ければ null */
function opsAiView(){
  const st = os.status;
  if(!st) return null;
  return {
    jobs: st.ai_jobs || null, pauses: Array.isArray(st.ai_pauses) ? st.ai_pauses : [],
    blocks: Array.isArray(st.ai_task_blocks) ? st.ai_task_blocks : [], waits: st.ai_waits || null,
    invalid: st.ai_config_invalid || null
  };
}
function opsFailedOf(view, kind){
  const byKind = view && view.jobs && view.jobs.by_kind ? view.jobs.by_kind[kind] : null;
  return byKind && byKind.failed ? Number(byKind.failed) || 0 : 0;
}
/* 状況の要点（失敗の理由・停止・止まっている用途・一時待機）。無ければ「問題なし」の 1 行 */
function opsAiSummary(box, view){
  const lines = [];
  const jobs = view.jobs;
  const by = jobs && jobs.by_state ? jobs.by_state : {};
  if(by.failed){
    const reasons = (jobs.reasons || {}).failed || {};
    const why = Object.keys(reasons).map(code => (AI_REASON[code] || code) + ' ' + reasons[code] + ' 件').join(' / ');
    lines.push(['bad', '失敗した AI ジョブ ' + by.failed + ' 件' + (why ? '（' + why + '）' : '')]);
  }
  const rest = ['pending', 'running', 'deferred'].filter(st => by[st])
    .map(st => (AI_JOB_STATE[st] || st) + ' ' + by[st] + ' 件');
  if(rest.length) lines.push(['', 'AI ジョブ: ' + rest.join(' / ')]);
  if(view.invalid) lines.push(['bad', 'AI 設定が不正（' + view.invalid.code + '）: 新しい AI 呼び出しだけ止まっています']);
  view.pauses.forEach(p => lines.push(['bad',
    'AI 停止中: ' + p.task + '（' + (p.scope === 'provider' ? 'Provider全体' : '用途') + '）: ' + (AI_PAUSE_REASON[p.reason] || p.reason)]));
  view.blocks.forEach(b => {
    const waiting = b.waiting === null || b.waiting === undefined ? '待ちは件数不明' : (Number(b.waiting) || 0) + ' 件が待ち';
    lines.push(['warn', '送信が止まっている用途: ' + b.task + '（' + (AI_PAUSE_REASON[b.reason] || b.reason) + '）で ' + waiting]);
  });
  const waits = view.waits;
  if(waits){
    (waits.items || []).forEach(w => lines.push(['warn',
      'AI 一時待機: ' + w.task + '（' + (AI_WAIT_SCOPE[w.scope] || w.scope) + '）: ' + (AI_WAIT_REASON[w.reason] || w.reason) +
      ' / 期限 ' + fmtUtc(w.until)]));
    if(waits.unidentified) lines.push(['warn', '用途を特定できない待ち ' + waits.unidentified + ' 件（AI設定が使えないため）']);
  }
  if(!lines.length) lines.push(['good', '失敗・停止・一時待機はありません']);
  lines.forEach(([cls, text]) => box.appendChild(el('p', 'op-ai-line ' + cls, text)));
}
function renderOpsAi(){
  const snap = os.snap;
  const ingest = opsIngest();
  $('#o-ai-panel').classList.toggle('hidden', !!snap && !ingest);
  const view = ingest ? opsAiView() : null;
  const kinds = ingest && snap.ai && Array.isArray(snap.ai.retry_kinds) ? snap.ai.retry_kinds : [];
  const busy = opsAiBusy();
  opsRebuild('ai', $('#o-ai'), {loaded: !!snap, ingest, kinds, view, statusFailed: os.statusFailed, busy}, box => {
    if(!snap){ box.appendChild(opsLine('dim', '読み込み中…')); return; }
    if(!ingest) return;
    box.appendChild(el('h3', 'op-ai-h', '状況'));
    if(view) opsAiSummary(box, view);
    else box.appendChild(opsLine('op-ai-line', os.statusFailed ? 'AI の状況を読めません' : '読み込み中…'));

    box.appendChild(el('h3', 'op-ai-h', '再試行'));
    const all = el('div', 'op-row');
    const allBtn = el('button', 'btn', 'すべて再試行');
    allBtn.id = 'o-ai-retry-all';
    allBtn.disabled = busy;
    allBtn.addEventListener('click', () => retryOpsAi('all'));
    all.appendChild(allBtn);
    all.appendChild(el('span', 'op-hint', OPS_AI_RETRY_NOTE));
    box.appendChild(all);
    kinds.forEach(kind => {
      const row = el('div', 'op-row');
      const btn = el('button', 'btn', kind + ' を再試行');
      btn.id = 'o-ai-retry-' + kind;
      btn.disabled = busy;
      btn.addEventListener('click', () => retryOpsAi({kind}));
      row.appendChild(btn);
      if(view) row.appendChild(el('span', 'op-hint', '失敗 ' + opsFailedOf(view, kind) + ' 件'));
      box.appendChild(row);
    });
    /* 再試行できない種類（再試行用の処理が未登録）に失敗があれば、戻せないことを添える */
    const byKind = view && view.jobs && view.jobs.by_kind ? view.jobs.by_kind : {};
    Object.keys(byKind).filter(k => kinds.indexOf(k) < 0 && opsFailedOf(view, k) > 0).forEach(k => {
      box.appendChild(opsLine('op-note warn',
        k + '（失敗 ' + opsFailedOf(view, k) + ' 件）: この版では再検査できない（failed のまま保持）'));
    });

    box.appendChild(el('h3', 'op-ai-h', '停止の解除'));
    const tasks = [];
    if(view) view.pauses.forEach(p => { if(tasks.indexOf(p.task) < 0) tasks.push(p.task); });
    const rel = el('div', 'op-row');
    const relAll = el('button', 'btn', 'すべて解除');
    relAll.id = 'o-ai-release-all';
    relAll.disabled = busy;
    relAll.addEventListener('click', () => releaseOpsAiPause('all'));
    rel.appendChild(relAll);
    tasks.forEach(task => {
      const btn = el('button', 'btn', task + ' の停止を解除');
      btn.id = 'o-ai-release-' + task;
      btn.disabled = busy;
      btn.addEventListener('click', () => releaseOpsAiPause({task}));
      rel.appendChild(btn);
    });
    box.appendChild(rel);
    const note = opsLine('op-note warn', OPS_AI_RELEASE_NOTE);
    note.id = 'o-ai-release-note';
    box.appendChild(note);
  });
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
  renderOpsAi();
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

/* AI の区画用に /api/admin/status を読む（運用タブを開いたときと AI の操作のあと。3 秒ごとには読まない）。
   loadOps と同じく、もっと新しい要求を出したあとに届いた古い応答は捨てる */
function loadOpsStatus(){
  const seq = ++os.statusSeq;
  return fetch('/api/admin/status').then(r => r.json()).then(d => {
    if(seq !== os.statusSeq) return;
    if(!d || !d.ok) throw new Error(d && d.error ? d.error : 'status error');
    os.status = d;
    os.statusFailed = false;
    renderOps();
  }).catch(e => {
    if(seq !== os.statusSeq) return;
    if(!os.statusFailed) toast('AI の状況を読めません: ' + (e && e.message ? e.message : e), true);
    os.statusFailed = true;
    os.status = null;                                       // 最新の取得に失敗: 古い件数・停止状態は残さない
    renderOps();
  });
}

/* ---- 操作 ---- */
/* POST を出して、応答（成功・失敗）が返り、読み直しが済むまで同じ種類のボタンを押せなくする */
/* opts.errors: 失敗の符号 → 日本語の上書き。opts.refreshStatus: 済んだら /api/admin/status も読み直す（AI の操作） */
function postOps(kind, path, body, okText, opts = {}){
  if(os.sending.has(kind)) return Promise.resolve();
  os.sending.add(kind);
  renderOps();
  let closeConfirm = false;
  return postJSON(path, body).then(j => {
    toast(typeof okText === 'function' ? okText(j) : okText);
    closeConfirm = kind === 'drive_release';                 // 許可を出した: 確認の段は閉じる
  }, e => {
    const msg = e && e.message ? e.message : '';
    toast(opsErrorText(e, opts.errors), true);
    /* 保留が変わった・無い・解除できない: 確認の段は閉じて読み直す（busy・too_soon は開いたまま、待ってやり直せる） */
    closeConfirm = msg === 'hold_changed' || msg === 'no_hold' || msg === 'not_releasable';
  }).then(() => {
    if(closeConfirm) os.confirming = null;
    return Promise.all([loadOps(), opts.refreshStatus ? loadOpsStatus() : null]);
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

/* AI ジョブの再試行。target は 'all' か {kind}。応答の report（5 つの件数）を toast に出し、履歴と AI の状況を読み直す */
function retryOpsAi(target){
  if(opsAiBusy()) return Promise.resolve();                 // AI の 2 操作は同じ排他（disabled を外れた click も通さない）
  return postOps('ai_retry', '/api/admin/ops/ai-retry', {target}, j => {
    const r = j && j.report ? j.report : null;
    return '再試行しました（' + (r ? opsReportText(r) : '件数不明') + '）';
  }, {errors: OPS_AI_ERROR, refreshStatus: true});
}
/* AI の停止の解除。target は 'all' か {task}。応答の released を toast に出す */
function releaseOpsAiPause(target){
  if(opsAiBusy()) return Promise.resolve();
  return postOps('ai_release_pause', '/api/admin/ops/ai-release-pause', {target},
    j => '停止を ' + (j && j.released !== undefined ? j.released : '?') + ' 件解除しました',
    {errors: OPS_AI_ERROR, refreshStatus: true});
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
  os.status = null;                                         // 前に見た AI の状況は捨てる（開くたびに読み直す）
  os.statusFailed = false;
  loadOps();
  loadOpsStatus();
  os.timer = setInterval(loadOps, OPS_POLL_MS);
}
/* 運用タブを離れるとき（どのタブへ移るときも呼んでよい） */
function stopOpsPolling(){
  if(os.timer === null) return;
  clearInterval(os.timer);
  os.timer = null;
}
