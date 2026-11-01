/* ============================================================
   curation.js — 管理ページの選別タブ（classic script / 依存なし）
   構成: 1) 状態 2) 組の読み出し 3) 段（1 組）の描画 4) 確定・取り消し 5) 読み直し・変更の反映 6) 一覧・切り替え・初期化
   admin.js（$ el toast postJSON）、photos.js（normalizePhoto openModalFor onPhotoChange）、library.js（libQuery putImage）の後に読み込む。
   定義だけを置き、起動は admin.js の boot() が initCuration() を呼ぶ形（読み込んだだけでは何にも触らない）。
   サーバーは GET /api/admin/curation/groups と POST confirm・reset（crates/geoframe/src/admin_curation.rs）。
   effective・selectable・warning・state はサーバーの値をそのまま使い、画面では再計算しない。token はそのまま返す。
   描画は textContent と DOM API だけ（HTML 文字列を流し込む API は使わない）。
   ============================================================ */
'use strict';

/* ==== 1) 状態 ==== */
/* state: 'unconfirmed'（再確認を含む）| 'confirmed' | 'all'。kind: 'all' | 'exact' | 'similar'。
   groups: 今のページの組（members は normalizePhoto を通した写真 + rank・score・… をそのまま持つ）。
   picks: 段ごとの選択（確定前）Map<groupKey, Set<photoId>>。busy: 確定・取り消しの送信中の段（多重送信を防ぐ）。
   nodes: 段の要素 Map<groupKey, section>。reloading: 1 組の読み直しが飛んでいる段。
   応答の順序は 1 つの規則で守る。ver（表示の版）は画面の段を変えるたび（一覧の反映・段の置き換え・段の削除・確定の直接反映）と、
   書き込み（確定・取り消しの POST）の成功のたび（直接反映しない古い応答でも。古さの判定は進める前の版で行う）に進める。
   要求を出しただけでは進めない。すべての要求（一覧の GET・1 組の GET・確定・取り消しの POST）は発行時の ver を控え、
   応答のときに ver が違えば古いものとして扱う（発行後に画面が変わっていなければ、そのまま反映する）:
     一覧の GET → 捨てて一覧を取り直す。1 組の GET → 捨てて、その段が今の一覧に残っていれば読み直す。
     POST → 段を置き換えず、成功のトーストだけ出して、段が残っていれば読み直す（成功で版を進めるので、成功より前に出した
     飛んでいる一覧・1 組の GET は、サーバーの確定前の値かもしれないとして捨てられ、取り直される）。
   取り直しは「変化のあと」に出すので、変化が止まれば収束する。
   seq: 一覧の要求番号（新しい一覧の要求に追い越された応答を捨てる）。gseq: 段ごとの要求番号（同じ段の新しい読み直し・確定に
   追い越された応答を捨てる。確定の応答は番号を進め、飛んでいる読み直し（reloading）も無効にする）。
   keep: 絞り込みに合わなくても残す段 Set<groupKey>。確定・取り消しの成功で、その段が今の一覧にあるときだけ入れ
   （直接の反映でも古い応答でも。ページ移動などで一覧から消えた段は入れない）、一覧の反映・段を外したときに消す。
   鍵が変わる・重複でまとめられるときは、残る段へ引き継ぐ。読み直しが★・除外の変更から出ても、keep の段は絞り込み不一致で消さない */
const cs = {
  state: 'unconfirmed', kind: 'all',
  page: 1, pages: 1, per: 20, total: 0, groups: [],
  picks: new Map(), busy: new Set(), nodes: new Map(), reloading: new Set(), keep: new Set(),
  seq: 0, gseq: new Map(), ver: 0,
};

const CUR_STATES = ['unconfirmed', 'confirmed', 'all'];
const CUR_KINDS = ['all', 'exact', 'similar'];
const CUR_STATE_TEXT = {unconfirmed: '未確認', confirmed: '確認済み', recheck: '再確認'};
const CUR_WARNING_TEXT = {representative_not_visible: '代表がフィードに出せません'};
const CUR_WARNING_HINT = {representative_not_visible:
  '有効な推奨の写真がすべて除外か位置なしです。代表を選び直すか、除外・位置を直してください'};

function curKey(group){ return group.kind + ':' + group.group_id; }
/* 今の絞り込みに合う組か（unconfirmed は recheck を含む。サーバーの state をそのまま比べるだけ） */
function curMatches(group){
  const stateOk = cs.state === 'all' ||
    (cs.state === 'confirmed' ? group.state === 'confirmed' : group.state !== 'confirmed');
  return stateOk && (cs.kind === 'all' || cs.kind === group.kind);
}

/* ==== 2) 組の読み出し ==== */
function curGet(params){
  const query = libQuery(params);
  return fetch('/api/admin/curation/groups' + (query ? '?' + query : ''), {headers: {'Accept': 'application/json'}})
    .then(r => r.json().then(d => d, () => ({ok: false, error: 'HTTP ' + r.status})))
    .then(d => {
      if(!d || !d.ok) throw new Error(d && d.error ? d.error : 'curation error');
      return d;
    });
}
/* 応答のメンバー 1 枚を、ビューア（photos.js）が読む形にする。この object をそのまま openModalFor に渡すので、
   ★・除外の反映（applyPhoto）が同じ object に入る。baseline は読み込んだ時点の除外・位置（変わったら selectable が古い） */
function curMember(raw){
  const member = Object.assign({}, raw, normalizePhoto(Object.assign({}, raw, {fav: raw.favorite}), 'list'));
  member.baseline = {hidden: member.hidden, lat: member.lat, lon: member.lon};
  return member;
}
function curGroup(raw){
  return Object.assign({}, raw, {members: (raw.members || []).map(curMember)});
}

/* ==== 3) 段（1 組）の描画 ==== */
function curStateText(group){
  if(group.state === 'confirmed'){
    return group.members.some(m => m.curation === 'representative') ? '確認済み（人の代表）' : '確認済み';
  }
  return CUR_STATE_TEXT[group.state] || group.state;
}
function curScoreText(member){
  return typeof member.score === 'number' ? member.score.toFixed(1) : '--';
}
/* メンバーのタグ: 解析の推奨・人の代表・再確認（記録が写真の変更で無効）・★・除外 */
function curTagSpecs(member){
  const tags = [];
  if(member.recommended) tags.push({text: '解析の推奨', cls: 'tag t-pick'});
  if(member.curation === 'representative') tags.push({text: '人の代表', cls: 'tag t-manual'});
  if(member.curation === 'stale') tags.push({text: '再確認', cls: 'tag t-pet'});
  if(member.favorite) tags.push({text: '★', cls: 'tag t-fav'});
  if(member.hidden) tags.push({text: '除外', cls: 'tag t-none'});
  return tags;
}
function curPaintMember(label, member){
  label.classList.toggle('is-effective', !!member.effective);
  label.classList.toggle('is-unselectable', !member.selectable);
  label.classList.toggle('is-hidden', !!member.hidden);
  const tags = label.querySelector('.cur-tags');
  tags.textContent = '';
  curTagSpecs(member).forEach(t => tags.appendChild(el('span', t.cls, t.text)));
  const img = label.querySelector('img');
  const src = member.thumb_url || member.thumb;
  if(img && src && img.src !== src) img.src = src;
}
function curPlaceText(member){
  return [member.pref, member.city, member.place].filter(Boolean).join(' ') || '位置なし';
}
function curReasons(member){
  const reasons = member.reasons || [], issues = member.issues || [];
  if(!reasons.length && !issues.length) return null;
  const why = el('details', 'cur-why');
  why.appendChild(el('summary', null, '理由 ' + reasons.length + '・懸念 ' + issues.length));
  if(reasons.length){
    why.appendChild(el('div', 'cur-why-h', '理由'));
    const ul = el('ul', 'cur-reasons');
    reasons.forEach(r => ul.appendChild(el('li', null, String(r))));
    why.appendChild(ul);
  }
  if(issues.length){
    why.appendChild(el('div', 'cur-why-h', '懸念'));
    const ul = el('ul', 'cur-issues');
    issues.forEach(r => ul.appendChild(el('li', null, String(r))));
    why.appendChild(ul);
  }
  return why;
}
function curMemberCard(group, member, index, section){
  const label = el('label', 'cur-member');
  label.dataset.id = member.id;
  const pick = el('input', 'cur-pick');
  pick.type = 'checkbox';
  pick.disabled = !member.selectable;
  pick.checked = !!(cs.picks.get(curKey(group)) || new Set()).has(member.id);
  pick.title = member.selectable ? '代表にする写真として選ぶ' : 'フィードに出せない写真（除外か位置なし）は代表にできません';
  pick.addEventListener('change', () => curSetPick(group, member.id, pick.checked, section));
  label.appendChild(pick);

  /* 画像を押すとビューア。label の中なので、そのままではチェックボックスが切り替わる → button にして preventDefault */
  const thumb = el('button', 'cur-thumb');
  thumb.type = 'button';
  thumb.setAttribute('aria-label', member.name + 'を拡大');
  putImage(thumb, member.thumb_url || member.thumb, member.name);
  thumb.addEventListener('click', e => {
    e.preventDefault();
    thumb.focus();             // 閉じたときにここへフォーカスを戻す
    openModalFor(group.members, index, {ai: false});
  });
  label.appendChild(thumb);

  const line = el('div', 'cur-line');
  line.appendChild(el('span', 'cur-rank', '#' + member.rank));
  line.appendChild(el('span', 'cur-score', '点数 ' + curScoreText(member)));
  label.appendChild(line);
  const name = el('div', 'cur-name', member.name);
  name.title = member.name + (member.taken_at ? '  ' + member.taken_at : '');
  label.appendChild(name);
  label.appendChild(el('div', 'cur-place', curPlaceText(member)));
  label.appendChild(el('div', 'tags cur-tags'));
  const why = curReasons(member);
  if(why) label.appendChild(why);
  curPaintMember(label, member);
  return label;
}
function curButton(cls, text, onClick){
  const b = el('button', 'btn ' + cls, text);
  b.type = 'button';
  b.addEventListener('click', onClick);
  return b;
}
/* 操作ボタンの有効・無効（選択の数・確認済みか・送信中で決まる） */
function curPaintActions(section, group){
  const key = curKey(group);
  const busy = cs.busy.has(key);
  const n = (cs.picks.get(key) || new Set()).size;
  section.querySelector('.cur-do-pick').disabled = busy || n < 1;
  const keep = section.querySelector('.cur-do-keep');
  keep.disabled = busy || group.state === 'confirmed';
  keep.title = group.state === 'confirmed' ? 'すでに確認済みです' : '';
  const reset = section.querySelector('.cur-do-reset');
  if(reset) reset.disabled = busy;
  section.querySelector('.cur-pick-note').textContent = n ? n + ' 枚を選択中' : '';
  section.classList.toggle('is-busy', busy);
}
function curSection(group){
  const section = el('section', 'cur-group');
  section.dataset.key = curKey(group);
  const head = el('div', 'cur-head');
  head.appendChild(el('span', 'cur-kind', group.kind));
  head.appendChild(el('span', 'cur-count', group.count + ' 枚'));
  head.appendChild(el('span', 'cur-time', group.taken_at || '撮影日時なし'));
  head.appendChild(el('span', 'cur-state st-' + group.state, '状態: ' + curStateText(group)));
  section.appendChild(head);
  if(group.warning){
    const warn = el('div', 'cur-warning', CUR_WARNING_TEXT[group.warning] || '注意が必要な組です');
    if(CUR_WARNING_HINT[group.warning]) warn.title = CUR_WARNING_HINT[group.warning];
    section.appendChild(warn);
  }
  const members = el('div', 'cur-members');
  group.members.forEach((m, i) => members.appendChild(curMemberCard(group, m, i, section)));
  section.appendChild(members);

  const acts = el('div', 'cur-acts');
  acts.appendChild(curButton('cur-do-pick', '選んだ写真を代表にする', () => curConfirmPicked(group)));
  acts.appendChild(curButton('cur-do-keep', 'このままで確認済み', () => curConfirmKeep(group)));
  if(group.members.some(m => m.curation !== null && m.curation !== undefined)){
    acts.appendChild(curButton('cur-do-reset', '取り消す', () => curReset(group)));
  }
  acts.appendChild(el('span', 'cur-pick-note'));
  section.appendChild(acts);
  curPaintActions(section, group);
  return section;
}
function curSetPick(group, id, on, section){
  const key = curKey(group);
  const set = cs.picks.get(key) || new Set();
  if(on) set.add(id); else set.delete(id);
  if(set.size) cs.picks.set(key, set); else cs.picks.delete(key);
  curPaintActions(section, group);
}

/* ==== 4) 確定・取り消し ==== */
function curPost(path, body){ return postJSON('/api/admin/curation/' + path, body); }
/* 段を応答の組で描き直す（picks はその段だけ消す。他の段は触らない） */
function curReplace(oldKey, fresh, keepPicks){
  const i = cs.groups.findIndex(g => curKey(g) === oldKey);
  if(i < 0) return;
  const picks = cs.picks.get(oldKey);
  cs.picks.delete(oldKey);
  if(keepPicks && picks){
    const kept = new Set(fresh.members.filter(m => m.selectable && picks.has(m.id)).map(m => m.id));
    if(kept.size) cs.picks.set(curKey(fresh), kept);
  }
  cs.ver++;
  const old = cs.nodes.get(oldKey);
  cs.nodes.delete(oldKey);
  if(cs.keep.delete(oldKey)) cs.keep.add(curKey(fresh));   // 組の鍵が変わっても、重複でまとめられても、残す方針は残る段へ引き継ぐ
  const dup = cs.groups.findIndex((g, j) => j !== i && curKey(g) === curKey(fresh));
  if(dup >= 0){ curRemoveAt(i, old); return; }      // 読み直したら別の段と同じ組だった（組み替わり）→ 重複させない
  cs.groups[i] = fresh;
  const section = curSection(fresh);
  cs.nodes.set(curKey(fresh), section);
  if(old) old.replaceWith(section);
}
function curRemoveAt(i, node){
  cs.ver++;
  const group = cs.groups[i];
  if(group){ cs.picks.delete(curKey(group)); cs.nodes.delete(curKey(group)); cs.keep.delete(curKey(group)); }
  cs.groups.splice(i, 1);
  if(node) node.remove();
  cs.total = Math.max(0, cs.total - 1);
  curPaintList();
  if(!cs.groups.length && cs.total > 0) loadCuration();   // このページが空になった → サーバーに合わせたページを読む
}
function curRemove(key){
  const i = cs.groups.findIndex(g => curKey(g) === key);
  if(i >= 0) curRemoveAt(i, cs.nodes.get(key));
}
function curRepaintActions(group){
  const key = curKey(group);
  const section = cs.nodes.get(key);
  const current = cs.groups.find(g => curKey(g) === key) || group;   // 一覧が読み直されていたら今の組で塗る
  if(section) curPaintActions(section, current);
}
function curSubmit(group, path, body, doneText){
  const key = curKey(group);
  if(cs.busy.has(key)) return;
  cs.busy.add(key);
  curRepaintActions(group);
  const ver = cs.ver, gseq = cs.gseq.get(key) || 0;   // 送信時の表示の版・この段の要求番号
  curPost(path, body).then(d => {
    cs.busy.delete(key);
    /* 送ってから画面が変わっていたり（別の確定・一覧・読み直しの反映）、この段が読み直されていたら、応答の組は古いかもしれない
       → 置き換えない（古さは進める前の版で判定する） */
    const stale = ver !== cs.ver || gseq !== (cs.gseq.get(key) || 0);
    cs.gseq.set(key, (cs.gseq.get(key) || 0) + 1);       // 飛んでいる読み直しの応答は、この確定より古い
    cs.reloading.delete(key);                             // その読み直しは無効になった（古い応答では後始末されない）
    const current = cs.groups.find(g => curKey(g) === key);
    if(current) cs.keep.add(key);                         // 確定した段は絞り込みに合わなくても残す。一覧から消えた段は入れない
    if(d.group && !stale){
      curReplace(key, curGroup(d.group), false);          // 版は curReplace が進める（鍵が変われば keep も引き継ぐ）
      toast(doneText);
    }else{
      cs.ver++;                                           // 書き込みは成功した。飛んでいる一覧・1 組の GET はこの前の値かもしれない
      if(d.group) toast(doneText);
      curRepaintActions(group);
      if(current) curReload(current, false);              // 今の一覧に段が残っているときだけ読み直す
    }
  }).catch(e => {
    cs.busy.delete(key);
    curRepaintActions(group);
    if(e.message === 'group_changed'){
      toast('組が変わりました。読み直します', true);
      curReload(group, false);
    }else if(e.message === 'representative_not_visible'){
      toast('代表に選べない写真があります', true);
      curReload(group, true);                            // 除外・位置が変わった（selectable が古い）かもしれない
    }else{
      toast('選別の更新に失敗: ' + e.message, true);
    }
  });
}
function curConfirmPicked(group){
  const picks = cs.picks.get(curKey(group));
  const ids = group.members.filter(m => picks && picks.has(m.id)).map(m => m.id);
  if(!ids.length) return;
  curSubmit(group, 'confirm', {kind: group.kind, group_id: group.group_id, token: group.token, representatives: ids},
            '代表を確定しました（' + ids.length + ' 枚）');
}
/* 「このままで確認済み」: 人の代表があればそのまま入れて送る（黙って解析の推奨に戻さない）。無ければ null */
function curConfirmKeep(group){
  const ids = group.members.filter(m => m.curation === 'representative').map(m => m.id);
  curSubmit(group, 'confirm',
            {kind: group.kind, group_id: group.group_id, token: group.token, representatives: ids.length ? ids : null},
            '確認済みにしました');
}
function curReset(group){
  curSubmit(group, 'reset', {kind: group.kind, group_id: group.group_id, token: group.token}, '取り消しました');
}

/* ==== 5) 読み直し・変更の反映 ==== */
/* 1 組だけ読み直す。state=all&kind=all で引く（既定の unconfirmed だと確認済みになった組を見失う）。
   ids= は写真 ID なので先頭のメンバーで引き、同じ kind の組を取る（再構築で group_id が変わっても拾える）。
   届いた組が今の絞り込みに合えば描き直し、合わなければ・空なら段を外す（cs.keep の段は、合わなくても描き直して残す）。
   応答は、新しい一覧の要求に追い越されたとき・同じ段の新しい読み直しや確定に追い越されたときは捨てる（後者は reloading を触らない）。
   発行後に画面が変わっていた（ver が違う）ときも捨てて、その段が今の一覧に残っていれば同じ条件で読み直す */
function curReload(group, keepPicks){
  const key = curKey(group);
  const first = group.members[0];
  if(!first) return;
  const gseq = (cs.gseq.get(key) || 0) + 1;
  cs.gseq.set(key, gseq);
  const seq = cs.seq, ver = cs.ver;
  cs.reloading.add(key);
  const latest = () => gseq === cs.gseq.get(key);
  curGet({state: 'all', kind: 'all', ids: first.id}).then(d => {
    if(!latest()) return;                                // 後続の要求（読み直し・確定）の状態は消さない
    cs.reloading.delete(key);
    if(seq !== cs.seq) return;                           // 新しい一覧の要求が出ている。一覧の応答が段を入れ替える
    if(ver !== cs.ver){                                  // 発行後に画面が変わった（一覧・別の段・確定の反映）→ この応答は古い
      const current = cs.groups.find(g => curKey(g) === key);
      if(current) curReload(current, keepPicks);
      return;
    }
    const raw = (d.groups || []).find(g => g.kind === group.kind);
    if(!raw){ curRemove(key); return; }
    const fresh = curGroup(raw);
    if(!cs.keep.has(key) && !curMatches(fresh)){ curRemove(key); return; }
    curReplace(key, fresh, keepPicks);
  }).catch(e => {
    if(!latest()) return;
    cs.reloading.delete(key);
    if(seq !== cs.seq) return;
    toast('組を読み直せませんでした: ' + e.message, true);
  });
}
/* ★・除外・位置などの変更（photos.js の applyPhoto から）。メンバーのタグを描き直す。
   除外・位置が読み込んだ時点から変わったら selectable・warning が古くなるので、その段を読み直す */
function curPhotoChanged(id, patch){
  cs.groups.slice().forEach(group => {
    const member = group.members.find(m => String(m.id) === String(id));
    if(!member) return;
    Object.assign(member, patch);
    const section = cs.nodes.get(curKey(group));
    if(section){
      const label = Array.from(section.querySelectorAll('.cur-member')).find(l => l.dataset.id === String(member.id));
      if(label) curPaintMember(label, member);
    }
    const base = member.baseline;
    const moved = ('hidden' in patch && !!patch.hidden !== base.hidden) ||
      ('lat' in patch && patch.lat !== base.lat) || ('lon' in patch && patch.lon !== base.lon);
    if(moved || cs.reloading.has(curKey(group))) curReload(group, true);
  });
}

/* ==== 6) 一覧・切り替え・初期化 ==== */
function curPaintList(){
  $('#cur-empty').classList.toggle('hidden', cs.groups.length > 0);
  $('#cur-pageinfo').textContent = cs.total + '組 / ' + cs.page + ' of ' + cs.pages + 'ページ';
  $('#cur-prev').disabled = cs.page <= 1;
  $('#cur-next').disabled = cs.page >= cs.pages;
  $('#cur-note').textContent = cs.total + ' 組';
}
function curPaintSegs(){
  CUR_STATES.forEach(s => $('#cur-state-' + s).classList.toggle('on', cs.state === s));
  CUR_KINDS.forEach(k => $('#cur-kind-' + k).classList.toggle('on', cs.kind === k));
}
function renderCuration(){
  const box = $('#cur-groups');
  box.textContent = '';
  cs.nodes.clear();
  cs.groups.forEach(group => {
    const section = curSection(group);
    cs.nodes.set(curKey(group), section);
    box.appendChild(section);
  });
  curPaintList();
}
function loadCuration(){
  const seq = ++cs.seq, ver = cs.ver;
  curGet({state: cs.state, kind: cs.kind, page: cs.page, per: cs.per}).then(d => {
    if(seq !== cs.seq) return;
    if(ver !== cs.ver){ loadCuration(); return; }     // 待つ間に画面が変わった（確定・段の読み直しの反映）→ この応答は古い。取り直す
    cs.page = d.page; cs.pages = d.pages; cs.total = d.total;
    cs.groups = (d.groups || []).map(curGroup);
    cs.picks.clear();                 // token が変わりうるので、前の選択は持ち越さない
    cs.reloading.clear();
    cs.keep.clear();
    cs.ver++;
    renderCuration();
  }).catch(e => {
    if(seq !== cs.seq) return;
    if(ver !== cs.ver){ loadCuration(); return; }
    toast('選別の組を読めませんでした: ' + e.message, true);
  });
}
function setCurFilter(state, kind){
  cs.state = state;
  cs.kind = kind;
  cs.page = 1;
  curPaintSegs();
  loadCuration();
}
/* 選別タブを開いたとき（admin.js の showTab から）。開くたびに読み直す */
function openCurationTab(){
  curPaintSegs();
  loadCuration();
}
function initCuration(){
  CUR_STATES.forEach(s => $('#cur-state-' + s).addEventListener('click', () => setCurFilter(s, cs.kind)));
  CUR_KINDS.forEach(k => $('#cur-kind-' + k).addEventListener('click', () => setCurFilter(cs.state, k)));
  $('#cur-prev').addEventListener('click', () => { if(cs.page > 1){ cs.page--; loadCuration(); } });
  $('#cur-next').addEventListener('click', () => { if(cs.page < cs.pages){ cs.page++; loadCuration(); } });
  onPhotoChange(curPhotoChanged);
}
