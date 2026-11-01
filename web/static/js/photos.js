/* ============================================================
   photos.js — 管理ページの写真タブ（classic script / 依存なし）
   構成: 1) 状態・正規化 2) カード 3) 一覧・チップ 4) 検索・場面 5) ビューア
        6) フラグ・回転・更新の反映 7) 位置指定モーダル（MapLibre・地名検索・AI候補） 8) 初期化
   admin.js（共通ヘルパ $ el fmtDur toast postJSON・タブ・状況・boot）の後に読み込む。
   定義だけを置き、起動は admin.js の boot() が initPhotos() を呼ぶ形（読み込んだだけでは何にも触らない）。
   描画は textContent と DOM API だけ（HTML 文字列を流し込む API は使わない）。
   ============================================================ */
'use strict';

/* ==== 1) 状態・正規化 ==== */
/* view: 'list'（チップ + 一覧）か 'search'（AI 検索の結果を場面ごと）。
   entries は今見えている写真の平らな配列（一覧は 1 枚 1 件、検索は場面の順）。ビューアはこれを前後に動く。
   cards は entries と同じ添字のカードの要素 */
const ps = {filter: 'all', page: 1, view: 'list', query: '', includeHidden: false,
            mode: 'recommended', entries: [], cards: [],
            pending: null};   // 応答待ちの検索 {query, seq}。query は成功した検索の語（ps.query）とは別

const SRC_LABEL = {exif: 'EXIF', drive_meta: 'DRIVE', inferred: '推定', manual: '手動'};
const SUBJECT_LABELS = {person: '人物', dog: '犬', cat: '猫', person_and_pet: '人物＋ペット',
  landscape: '風景', food: '食事', vehicle: '乗り物', other: 'その他'};
const PET_LABELS = {awake: '起きている', sleeping: '睡眠中', active: '活動中', uncertain: '不明', not_applicable: '対象外'};
const EYE_LABELS = {open: '開眼', closed: '閉眼', mixed: '一部閉眼', not_visible: '目は見えない',
  uncertain: '不明', not_applicable: '対象外'};
const CROP_LABELS = {good: '顔切れなし', intentional_closeup: '意図的なアップ', accidental_cut: '顔切れ注意',
  not_visible: '顔は見えない', uncertain: '不明', not_applicable: '対象外'};
const HORIZON_LABELS = {level: '水平良好', intentional_tilt: '意図的な傾き', accidental_tilt: '水平傾き注意',
  uncertain: '不明', not_applicable: '対象外'};
const GROUP_KIND_LABEL = {scene: 'SAME SCENE', pet: 'PET PHOTO', similar: 'SIMILAR BURST', single: 'SINGLE'};

function hasValue(v){ return v !== null && v !== undefined; }
function hasLoc(photo){ return hasValue(photo.lat); }

/* 一覧（GET /api/admin/photos。origin='list'）と検索（GET /api/ai/search。origin='search'）の写真を
   共通の形にする。お気に入りは一覧が fav、検索が favorite。AI の評価と構図の分類は検索だけ（一覧では null） */
function normalizePhoto(raw, origin){
  const search = origin === 'search';
  const photo = {
    id: raw.id, name: raw.name || '', thumb: raw.thumb || '', thumb_url: raw.thumb_url || '', taken_at: raw.taken_at || '',
    lat: hasValue(raw.lat) ? raw.lat : null, lon: hasValue(raw.lon) ? raw.lon : null,
    place: raw.place || '', pref: raw.pref || '', city: raw.city || '',
    favorite: !!(search ? raw.favorite : raw.fav), hidden: !!raw.hidden,
    loc_source: raw.loc_source || 'none', infer_bucket: raw.infer_bucket || '',
    rotation: raw.rotation || 0, source: raw.source || '',
    hint: raw.hint || null, suggest: raw.suggest || null,
    ai: null, visual: null,
  };
  if(search){
    photo.ai = {
      similarity: Number(raw.similarity) || 0, recommended: hasValue(raw.recommended) ? raw.recommended : null,
      subject: raw.subject || '', eyes: raw.eyes || '', pet_state: raw.pet_state || '',
      face_crop: raw.face_crop || '', horizon_status: raw.horizon_status || '',
      horizon_score: hasValue(raw.horizon_score) ? raw.horizon_score : null,
      subject_sharpness: hasValue(raw.subject_sharpness) ? raw.subject_sharpness : null,
      aesthetic_score: hasValue(raw.aesthetic_score) ? raw.aesthetic_score : null,
      keep_score: hasValue(raw.keep_score) ? raw.keep_score : null,
      reasons: raw.reasons || [], concerns: raw.concerns || [],
    };
    photo.visual = {
      cluster: raw.visual_cluster, representative: !!raw.visual_representative,
      similarity: hasValue(raw.visual_similarity) ? raw.visual_similarity : 1,
      size: raw.visual_cluster_size || 1,
    };
  }
  return photo;
}

/* 1 行目（.l1）がカードの主役。位置があれば地名（無ければ座標 4 桁）。位置が無ければ「次に何をするか」:
   AI候補 → 近い時刻の位置 → 手がかりなし。地名は位置がある行だけ表示する
   （推定を取り消された行には解決済みの地名が残っていることがあり、出すと誤解を生む） */
function l1Of(photo){
  if(hasLoc(photo)){
    const loc = [photo.pref, photo.city, photo.place].filter(Boolean).join(' ');
    return {text: loc || (photo.lat.toFixed(4) + ', ' + photo.lon.toFixed(4)), cls: 'l1'};
  }
  if(photo.suggest) return {text: 'AI候補: ' + photo.suggest.place_name, cls: 'l1 act'};
  if(photo.hint && photo.hint.label){
    return {text: '近い時刻の位置: ' + photo.hint.label + '（' + fmtDur(photo.hint.dt_s) + '差）',
            cls: 'l1 act hint'};
  }
  return {text: '手がかりなし', cls: 'l1 act none'};
}
/* タグ: 由来（EXIF・DRIVE・推定 <bucket>・手動・位置なし）。検索結果の写真は AI PICK・FAVORITE・被写体を足す。
   full（ビューア）は目・水平のバッジも足す */
function tagSpecs(photo, full){
  const src = hasLoc(photo) ? (photo.loc_source || 'none') : 'none';
  const tags = [{text: (SRC_LABEL[src] || '位置なし') +
                       (src === 'inferred' && photo.infer_bucket ? ' ' + photo.infer_bucket : ''),
                 cls: 'tag t-' + src}];
  const ai = photo.ai;
  if(ai){
    if(ai.recommended === true) tags.push({text: 'AI PICK', cls: 'tag t-pick'});
    if(photo.favorite) tags.push({text: 'FAVORITE', cls: 'tag t-fav'});
    if(ai.subject){
      tags.push({text: SUBJECT_LABELS[ai.subject] || ai.subject,
                 cls: (ai.subject === 'dog' || ai.subject === 'cat') ? 'tag t-pet' : 'tag t-subject'});
    }
    if(full){
      if(ai.eyes === 'open') tags.push({text: 'EYES OPEN', cls: 'tag t-subject'});
      if(ai.horizon_status === 'level') tags.push({text: 'LEVEL', cls: 'tag t-pick'});
      if(ai.horizon_status === 'intentional_tilt') tags.push({text: 'INTENTIONAL TILT', cls: 'tag t-subject'});
      if(ai.horizon_status === 'accidental_tilt') tags.push({text: 'TILT', cls: 'tag t-pet'});
    }
  }
  return tags;
}
function scoreLabel(photo){
  const pct = Math.round((photo.ai ? photo.ai.similarity : 0) * 100);
  return ps.mode === 'semantic' ? pct + '% MATCH' : pct + ' SCORE';
}

/* ==== 2) カード（一覧・検索で同じ 1 種類） ==== */
function paintL1(node, photo){
  const r = l1Of(photo);
  node.className = r.cls;
  node.textContent = r.text;
}
function paintTags(box, photo, full){
  box.textContent = '';
  tagSpecs(photo, full).forEach(t => box.appendChild(el('span', t.cls, t.text)));
}
function rotateTitle(photo){
  return '時計回りに90°回して表示' + (photo.rotation ? '（今 ' + photo.rotation + '°）' : '');
}
/* 描く順: 画像（検索結果は右上に NN% MATCH、左下に構図）→ 1 行目 → 2 行目 → タグ → 操作。
   index は ps.entries の添字（画像クリックでビューアを開く）。groupUi は検索結果の場面の折りたたみ */
function cardFor(photo, index, groupUi){
  const c = el('div', 'card' + (photo.hidden ? ' is-hidden' : '') +
                      (photo.visual && photo.visual.representative ? ' ps-rep' : ''));
  c.dataset.id = photo.id;

  const ph = el('div', 'ph');
  if(photo.thumb){
    const img = document.createElement('img');
    img.loading = 'lazy';          // 50枚同時取得を避ける
    img.decoding = 'async';
    img.alt = photo.name;
    img.src = photo.thumb_url || photo.thumb;
    // 取得済みderivativeが消えている場合はプレースホルダに落とす
    img.addEventListener('error', () => {
      img.remove();
      ph.appendChild(el('div', 'noimg', 'NO IMAGE'));
    });
    ph.appendChild(img);
    ph.classList.add('ps-clickable');
    ph.setAttribute('role', 'button');
    ph.setAttribute('tabindex', '0');
    ph.setAttribute('aria-label', photo.name + 'の詳細を表示');
    ph.addEventListener('click', () => openModal(index));
    ph.addEventListener('keydown', e => {
      if(e.key === 'Enter' || e.key === ' '){ e.preventDefault(); openModal(index); }
    });
  }else{
    ph.appendChild(el('div', 'noimg', '画像未取得\n(位置未設定のため)'));
  }
  if(photo.ai) ph.appendChild(el('span', 'ps-score', scoreLabel(photo)));
  if(photo.visual){
    ph.appendChild(el('span', 'ps-visual', photo.visual.representative
      ? '構図 ' + photo.visual.cluster + ' 代表'
      : '代表と ' + Math.round(photo.visual.similarity * 100) + '%'));
  }
  c.appendChild(ph);

  const m = el('div', 'meta');
  const l1 = el('div');
  paintL1(l1, photo);
  m.appendChild(l1);
  const l2 = el('div', 'l2');
  l2.appendChild(el('span', 'dt', photo.taken_at || '撮影時刻なし'));
  l2.appendChild(el('span', 'nm', photo.name));
  m.appendChild(l2);
  const tags = el('div', 'tags');
  paintTags(tags, photo, false);
  m.appendChild(tags);
  if(photo.visual && groupUi){
    const more = el('button', 'ps-more hidden');
    more.addEventListener('click', () => groupUi.toggleCluster(photo.visual.cluster));
    m.appendChild(more);
  }
  c.appendChild(m);

  /* ★・🚫 は押した時点の見た目（.on）を基準に反転する。同じ写真のカードが複数あっても
     applyPhoto が全部に応答の値を反映するので、どれから押しても食い違わない */
  const acts = el('div', 'acts');
  const fav = el('button', 'fav' + (photo.favorite ? ' on' : ''), '★');
  fav.title = 'お気に入り';
  fav.addEventListener('click', () => setFlag(photo, {favorite: !fav.classList.contains('on')}));
  const hid = el('button', 'hid' + (photo.hidden ? ' on' : ''), '🚫');
  hid.title = '表示から除外';
  hid.addEventListener('click', () => setFlag(photo, {hidden: !hid.classList.contains('on')}));
  const loc = el('button', hasLoc(photo) ? 'setloc' : 'setloc strong', hasLoc(photo) ? '位置' : '位置を付ける');
  loc.title = '地図で位置を指定';
  loc.addEventListener('click', () => openLocModal(photo));
  acts.appendChild(fav); acts.appendChild(hid); acts.appendChild(loc);
  // 横倒し・逆さまで保存された写真を直す（画像があるDriveの写真だけ。一覧・検索で同じ条件）
  if(photo.thumb && photo.source === 'drive'){
    const rot = el('button', 'rotate', '↻');
    rot.title = rotateTitle(photo);
    rot.addEventListener('click', () => rotatePhoto(photo, rot));
    acts.appendChild(rot);
  }
  c.appendChild(acts);
  return c;
}
/* photo の今の値でカードを描き直す（★・除外・位置・回転の反映） */
function refreshCard(card, photo){
  if(!card) return;
  card.classList.toggle('is-hidden', photo.hidden);
  card.querySelector('.fav').classList.toggle('on', photo.favorite);
  card.querySelector('.hid').classList.toggle('on', photo.hidden);
  paintL1(card.querySelector('.l1'), photo);
  paintTags(card.querySelector('.tags'), photo, false);
  const loc = card.querySelector('.setloc');
  loc.textContent = hasLoc(photo) ? '位置' : '位置を付ける';
  loc.classList.toggle('strong', !hasLoc(photo));
  const img = card.querySelector('.ph img');
  const cardSrc = photo.thumb_url || photo.thumb;
  if(img && cardSrc && img.src !== cardSrc) img.src = cardSrc;
  const rot = card.querySelector('.rotate');
  if(rot) rot.title = rotateTitle(photo);
}

/* ==== 3) 一覧・チップ ==== */
/* 表示の切り替え。list: チップ・ページング・#cards。search: #groups と「検索を閉じる」 */
function applyView(){
  const search = ps.view === 'search';
  $('#chips').classList.toggle('hidden', search);
  $('#pager').classList.toggle('hidden', search);
  $('#cards').classList.toggle('hidden', search);
  $('#groups').classList.toggle('hidden', !search);
  $('#close-search').classList.toggle('hidden', !search);
}
function setFilter(f){
  ps.filter = f; ps.page = 1; ps.view = 'list';
  searchSeq++;                       // 飛んでいる検索の応答で、選んだ一覧を上書きしない
  closeModal();
  document.querySelectorAll('.chip').forEach(b => b.classList.toggle('on', b.dataset.filter === f));
  applyView();
  loadList();
}
/* 状況タブの「要対応」から: そのフィルタの一覧へ（検索中でも一覧に戻す）。showTab が一覧を 1 回だけ読む */
function gotoFilter(f){
  ps.filter = f; ps.view = 'list';
  showTab('photos');
}
/* 写真タブを開いたとき（admin.js の showTab から）。検索結果を見ていたときは、その結果のまま */
function openPhotosTab(){
  loadFilterCounts();
  loadIndexNote();
  if(ps.view === 'list') setFilter(ps.filter);
}
/* チップの件数は GET /api/admin/status の db.filters（一覧 API の total と同じ条件で数えたもの） */
let countsSeq = 0;    // ★の直後などに続けて読んだとき、遅れて届いた古い件数でチップを上書きしない
function loadFilterCounts(){
  const seq = ++countsSeq;
  fetch('/api/admin/status').then(r => r.json()).then(d => {
    if(seq !== countsSeq) return;
    if(!d.ok) throw new Error(d.error || 'status error');
    const filters = (d.db && d.db.filters) || {};
    document.querySelectorAll('.chip').forEach(b => {
      const n = b.querySelector('.n');
      const v = filters[b.dataset.filter];
      if(n) n.textContent = (v === undefined || v === null) ? '' : Number(v).toLocaleString('ja-JP');
    });
  }).catch(e => { if(seq === countsSeq) toast('件数取得失敗: ' + e.message, true); });
}
let listSeq = 0;      // 速く切り替えたとき、遅れて届いた古い応答で一覧を上書きしない
function loadList(){
  const seq = ++listSeq;
  fetch('/api/admin/photos?filter=' + encodeURIComponent(ps.filter) +
        '&page=' + ps.page + '&per=50')
    .then(r => r.json()).then(d => {
      if(seq !== listSeq) return;
      if(!d.ok) throw new Error(d.error || 'list error');
      ps.page = d.page;
      $('#pageinfo').textContent = d.total + '件 / ' + d.page + ' of ' + d.pages + 'ページ';
      $('#prev').disabled = d.page <= 1;
      $('#next').disabled = d.page >= d.pages;
      $('#empty').classList.toggle('hidden', d.items.length > 0);
      closeModal();
      const box = $('#cards'); box.textContent = '';
      ps.entries = []; ps.cards = [];
      d.items.forEach((raw, i) => {
        const photo = normalizePhoto(raw, 'list');
        ps.entries.push({photo, group: null, groupIndex: 0, photoIndex: i});
        const card = cardFor(photo, i, null);
        ps.cards.push(card);
        box.appendChild(card);
      });
    }).catch(e => { if(seq === listSeq) toast('一覧取得失敗: ' + e.message, true); });
}

/* ==== 4) 検索・場面ごとの表示 ==== */
/* #search-note: 検索の失敗の理由（err）と、検索の状態・索引の枚数（薄い文字）。kind: busy|result|index|err */
let noteKind = '';
let indexNote = '';
let indexNoteAsked = false;
function setNote(text, kind){
  const n = $('#search-note');
  n.textContent = text;
  n.className = 'search-note ' + (kind === 'err' ? 'err' : 'dim');
  noteKind = text ? kind : '';
}
/* 写真タブを初めて開いたとき 1 回だけ、索引の枚数を読む（失敗したら何も出さない） */
function loadIndexNote(){
  if(indexNoteAsked) return;
  indexNoteAsked = true;
  fetch('/api/ai/status').then(r => r.json()).then(d => {
    if(!d || !d.ok || !hasValue(d.photos) || !hasValue(d.eligible)) return;
    indexNote = '索引 ' + d.photos + '/' + d.eligible + ' 枚';
    if(noteKind === '') setNote(indexNote, 'index');
  }).catch(() => {});
}
/* 検索 API の失敗を、固定の日本語にする（HTTP の状態と応答の error から決める） */
function searchErrorText(status, error){
  if(status === 503){
    if(String(error || '').includes('AI検索は無効')) return 'AI は無効です（GEOFRAME_AI_ENABLED）。一覧は使えます';
    return '検索の索引がまだ使えません。状況タブの AI の欄を見てください';
  }
  if(status === 429) return '検索が多すぎます。1 分待ってから試してください';
  if(status === 400) return '検索語は 300 文字以内で入力してください';
  return '検索できませんでした: ' + (error || ('HTTP ' + status));
}
/* 同じ構図の写真を、場面の中で代表を先頭にしてまとめる */
function visualOrder(group){
  const clusters = new Map();
  group.photos.forEach(photo => {
    if(!clusters.has(photo.visual.cluster)) clusters.set(photo.visual.cluster, []);
    clusters.get(photo.visual.cluster).push(photo);
  });
  const keep = p => (hasValue(p.ai.keep_score) ? p.ai.keep_score : -1);
  const out = [];
  clusters.forEach(photos => {
    photos.sort((a, b) =>
      Number(b.visual.representative) - Number(a.visual.representative) ||
      b.visual.similarity - a.visual.similarity ||
      keep(b) - keep(a));
    photos.forEach(p => out.push(p));
  });
  return out;
}
function applyGroupVisibility(group, section, state){
  state.cards.forEach((card, photo) => {
    const expanded = state.showAll || state.expanded.has(photo.visual.cluster);
    card.classList.toggle('ps-collapsed', !photo.visual.representative && !expanded);
    const more = card.querySelector('.ps-more');
    if(more && photo.visual.representative && photo.visual.size > 1){
      more.classList.toggle('hidden', state.showAll);
      more.textContent = expanded
        ? '類似' + (photo.visual.size - 1) + '枚を閉じる'
        : '＋' + (photo.visual.size - 1) + '枚の類似を表示';
    }
  });
  section.querySelector('.ps-expand').textContent = state.showAll
    ? '同じ構図 ' + group.collapsed_photo_count + '枚を畳む'
    : 'すべて見る（' + group.photos.length + '枚）';
}
/* 検索結果を #groups に描く。ps.entries は場面の順・場面の中は構図の順の平らな配列 */
function renderSearch(data){
  closeModal();
  ps.mode = data.mode;
  ps.query = data.query || '';
  const groups = (data.groups || []).map(g =>
    Object.assign({}, g, {photos: g.photos.map(p => normalizePhoto(p, 'search'))}));
  const ordered = groups.map(visualOrder);
  ps.entries = []; ps.cards = [];
  groups.forEach((group, gi) => ordered[gi].forEach((photo, pi) =>
    ps.entries.push({photo, group, groupIndex: gi, photoIndex: pi})));
  const entryOf = new Map(ps.entries.map((e, i) => [e.photo, i]));
  const box = $('#groups');
  box.textContent = '';
  $('#empty').classList.toggle('hidden', groups.length > 0);
  groups.forEach((group, gi) => {
    const section = el('section', 'ps-group');
    const head = el('div', 'ps-group-head');
    const left = el('div');
    left.appendChild(el('span', 'ps-kind', String(gi + 1).padStart(2, '0') + ' / ' +
                                           (GROUP_KIND_LABEL[group.kind] || group.kind)));
    left.appendChild(el('h3', 'ps-summary', group.summary ||
                        (group.photos.length > 1 ? group.photos.length + '枚の類似写真' : '検索候補')));
    head.appendChild(left);
    const tools = el('div', 'ps-group-tools');
    tools.appendChild(el('div', 'ps-match', data.mode === 'semantic'
      ? Math.round(group.best_similarity * 100) + '% MATCH · ' + group.photos.length + '枚'
      : group.photos.length + ' PHOTO' + (group.photos.length > 1 ? 'S' : '')));
    const stat = el('div', 'ps-cluster-stat', group.collapsed_photo_count
      ? group.visual_cluster_count + '構図 / 類似' + group.collapsed_photo_count + '枚を整理'
      : group.visual_cluster_count + '構図');
    if(hasValue(group.avg_similarity)){
      stat.title = '場面全体のembedding類似度: 平均' + Math.round(group.avg_similarity * 100) +
                   '%・範囲' + Math.round(group.min_similarity * 100) + '〜' +
                   Math.round(group.max_similarity * 100) + '%';
    }
    tools.appendChild(stat);
    const expand = el('button', 'ps-expand' + (group.collapsed_photo_count ? '' : ' hidden'));
    tools.appendChild(expand);
    head.appendChild(tools);
    section.appendChild(head);

    const grid = el('div', 'ps-grid');
    const state = {
      showAll: false, expanded: new Set(), cards: new Map(),
      toggleCluster(cluster){
        if(this.expanded.has(cluster)) this.expanded.delete(cluster); else this.expanded.add(cluster);
        applyGroupVisibility(group, section, this);
      },
    };
    ordered[gi].forEach(photo => {
      const index = entryOf.get(photo);
      const card = cardFor(photo, index, state);
      ps.cards[index] = card;
      state.cards.set(photo, card);
      grid.appendChild(card);
    });
    expand.addEventListener('click', () => {
      state.showAll = !state.showAll;
      applyGroupVisibility(group, section, state);
    });
    applyGroupVisibility(group, section, state);
    section.appendChild(grid);
    box.appendChild(section);
  });
}
/* 検索（検索ボタン・Enter）と「AI おすすめ」（q が空）。成功したときだけ view='search' にする。
   失敗は view・filter・page を変えず、#search-note に理由を出す。古い応答（searchSeq が違う）は捨てる */
let searchSeq = 0;
const MAX_QUERY_CHARS = 300;      // API（crates/geoframe/src/ai/query.rs）はコードポイント数で数える
function search(q){
  q = String(q === undefined ? $('#q').value : q).trim();
  if([...q].length > MAX_QUERY_CHARS){
    searchSeq++;                       // 先行の検索の応答で注意書きを上書きさせない
    ps.pending = null;
    setNote('検索語は ' + MAX_QUERY_CHARS + ' 文字以内で入力してください', 'err');
    return;
  }
  const seq = ++searchSeq;
  ps.pending = {query: q, seq};
  ps.includeHidden = !!$('#include-hidden').checked;
  setNote(q ? '検索しています…' : 'おすすめを読み込んでいます…', 'busy');
  const url = '/api/ai/search?q=' + encodeURIComponent(q) + '&groups=12' +
              (ps.includeHidden ? '&include_hidden=1' : '');
  fetch(url, {headers: {'Accept': 'application/json'}})
    .then(r => r.json().then(d => ({status: r.status, ok: r.ok, data: d || {}}),
                             () => ({status: r.status, ok: r.ok, data: {}})))
    .then(res => {
      if(seq !== searchSeq) return;
      ps.pending = null;
      if(!res.ok || !res.data.ok){
        setNote(searchErrorText(res.status, res.data.error), 'err');
        return;
      }
      ps.view = 'search';
      listSeq++;                       // 読み込み中の一覧の応答は捨てる
      renderSearch(res.data);
      applyView();
      const groups = res.data.groups || [];
      const returned = groups.reduce((s, g) => s + g.photos.length, 0);
      const clusters = groups.reduce((s, g) => s + (g.visual_cluster_count || 0), 0);
      setNote([(q ? '「' + q + '」' : 'おすすめ') + ' ' + (res.data.searched_photos || 0) + '枚を検索 / ' +
               returned + '枚 → ' + clusters + '構図', indexNote].filter(Boolean).join(' / '), 'result');
    })
    .catch(e => { if(seq === searchSeq){ ps.pending = null; setNote(searchErrorText(0, e.message), 'err'); } });
}
/* 「除外も検索」を切り替えたとき: 応答待ちの検索があればその語で、無ければ表示中の検索結果の語で検索し直す。
   一覧を見ていて応答待ちも無ければ何もしない。応答待ちの印は searchSeq を進める操作（一覧へ戻す等）で無効になる */
function researchWithToggle(){
  ps.includeHidden = !!$('#include-hidden').checked;
  const pending = ps.pending && ps.pending.seq === searchSeq ? ps.pending : null;
  if(pending) search(pending.query);
  else if(ps.view === 'search') search(ps.query);
}
/* 「✕ 検索を閉じる」: 一覧へ戻し、直前の filter・page で読み直す */
function closeSearch(){
  searchSeq++;
  ps.view = 'list';
  closeModal();
  ps.entries = []; ps.cards = [];
  $('#cards').textContent = '';
  $('#groups').textContent = '';
  $('#empty').classList.add('hidden');
  setNote(indexNote, 'index');
  applyView();
  loadList();
}

/* ==== 5) ビューア（拡大表示・前後移動・スワイプ・キー操作） ==== */
let viewerIndex = -1;
let viewerLastFocus = null;
let swipeStart = null;
let suppressMediaClick = false;

function appendScore(label, value){
  if(!hasValue(value) || value === '') return;
  const box = el('div', 'ps-viewer-score-cell');
  box.appendChild(el('b', null, String(value)));
  box.appendChild(el('span', null, label));
  $('#viewer-scores').appendChild(box);
}
function appendAnalysis(label, value){
  if(!value) return;
  $('#viewer-analysis').appendChild(el('dt', null, label));
  $('#viewer-analysis').appendChild(el('dd', null, value));
}
function fillList(node, values, fallback){
  node.textContent = '';
  const items = values && values.length ? values : (fallback ? [fallback] : []);
  items.forEach(v => node.appendChild(el('li', null, v)));
}
function renderModal(){
  const entry = ps.entries[viewerIndex];
  if(!entry) return;
  const photo = entry.photo, group = entry.group, ai = photo.ai, visual = photo.visual;
  const image = $('#viewer-image');
  image.src = photo.thumb;
  image.alt = photo.name;
  $('#viewer-title').textContent = '#' + photo.id + '  ' + photo.name;
  $('#viewer-meta').textContent = photo.taken_at || '撮影時刻なし';
  $('#viewer-counter').textContent = (viewerIndex + 1) + ' / ' + ps.entries.length;
  $('#viewer-group-kind').textContent = group
    ? (GROUP_KIND_LABEL[group.kind] || group.kind) + ' · GROUP ' + (entry.groupIndex + 1) +
      ' · ' + (entry.photoIndex + 1) + '/' + group.photos.length
    : '';
  $('#viewer-group').classList.toggle('hidden', !group);
  if(group){
    $('#viewer-group-summary').textContent = group.summary ||
      (group.photos.length > 1 ? group.photos.length + '枚の類似写真' : '単独の検索候補');
  }
  $('#viewer-image-score').textContent = ai ? scoreLabel(photo) : '';
  $('#viewer-image-score').classList.toggle('hidden', !ai);

  const locNode = $('#viewer-location');
  locNode.textContent = '';
  locNode.appendChild(document.createTextNode(l1Of(photo).text));
  if(hasLoc(photo) && hasValue(photo.lon)){
    locNode.appendChild(el('span', 'coords', Number(photo.lat).toFixed(5) + ', ' + Number(photo.lon).toFixed(5)));
  }
  paintTags($('#viewer-badges'), photo, true);

  $('#viewer-ai').classList.toggle('hidden', !ai);
  $('#viewer-scores').textContent = '';
  $('#viewer-analysis').textContent = '';
  if(ai){
    const pct = Math.round(ai.similarity * 100);
    appendScore(ps.mode === 'semantic' ? 'MATCH' : 'SEARCH SCORE', pct);
    appendScore('採用', ai.keep_score);
    appendScore('美的', ai.aesthetic_score);
    appendScore('主被写体', ai.subject_sharpness);
    appendScore('水平', ai.horizon_score);
    appendScore('構図類似', Math.round(visual.similarity * 100));
    appendAnalysis('主題', SUBJECT_LABELS[ai.subject] || ai.subject);
    appendAnalysis('犬猫の状態', PET_LABELS[ai.pet_state] || ai.pet_state);
    appendAnalysis('開眼', EYE_LABELS[ai.eyes] || ai.eyes);
    appendAnalysis('顔切れ', CROP_LABELS[ai.face_crop] || ai.face_crop);
    appendAnalysis('水平', HORIZON_LABELS[ai.horizon_status] || ai.horizon_status);
    appendAnalysis('AI採用', ai.recommended === true ? '推奨' : (ai.recommended === false ? '非推奨' : '未評価'));
    appendAnalysis('類似セット', visual.representative
      ? '構図代表・全' + visual.size + '枚'
      : '代表と' + Math.round(visual.similarity * 100) + '%・全' + visual.size + '枚');
    fillList($('#viewer-reasons'), ai.reasons, 'AI詳細評価なし');
    fillList($('#viewer-concerns'), ai.concerns, 'なし');
  }
  const fav = $('#viewer-favorite'), hid = $('#viewer-hidden');
  fav.classList.toggle('on', photo.favorite);
  fav.textContent = photo.favorite ? '★ favorite' : '☆ favorite';
  hid.classList.toggle('on', photo.hidden);
  hid.textContent = photo.hidden ? '● hidden' : '○ hidden';

  const multiple = ps.entries.length > 1;
  $('#viewer-prev').disabled = !multiple;
  $('#viewer-next').disabled = !multiple;
  if(multiple && typeof Image === 'function'){
    const n = ps.entries.length;
    [ps.entries[(viewerIndex - 1 + n) % n].photo.thumb, ps.entries[(viewerIndex + 1) % n].photo.thumb]
      .forEach(src => { if(src){ const pre = new Image(); pre.src = src; } });
  }
}
function openModal(index){
  if(index < 0 || index >= ps.entries.length) return;
  viewerIndex = index;
  viewerLastFocus = document.activeElement;
  renderModal();
  $('#viewer').classList.remove('hidden');
  document.body.classList.add('ps-viewer-open');
  $('#viewer-close').focus();
}
function closeModal(){
  if(viewerIndex < 0) return;
  $('#viewer').classList.add('hidden');
  document.body.classList.remove('ps-viewer-open');
  viewerIndex = -1;
  if(viewerLastFocus && document.contains(viewerLastFocus)) viewerLastFocus.focus();
  viewerLastFocus = null;
}
/* 末尾の次は先頭、先頭の前は末尾（回る） */
function stepModal(delta){
  if(viewerIndex < 0 || ps.entries.length < 2) return;
  viewerIndex = (viewerIndex + delta + ps.entries.length) % ps.entries.length;
  renderModal();
}

/* ==== 6) フラグ・回転・更新の反映 ==== */
/* 同じ id の写真を全部（entries・カード・開いているビューア）、patch の値で更新して描き直す */
function applyPhoto(id, patch){
  ps.entries.forEach((entry, i) => {
    if(String(entry.photo.id) !== String(id)) return;
    Object.assign(entry.photo, patch);
    refreshCard(ps.cards[i], entry.photo);
  });
  const open = ps.entries[viewerIndex];
  if(open && String(open.photo.id) === String(id)) renderModal();
}
function setFlag(photo, patch){
  postJSON('/api/admin/setflag', Object.assign({id: photo.id}, patch)).then(j => {
    applyPhoto(photo.id, {favorite: !!j.favorite, hidden: !!j.hidden});
    toast('#' + photo.id + ' 更新: ' + (j.favorite ? '★' : '☆') + (j.hidden ? ' 🚫除外' : ''));
    loadFilterCounts();           // ★・除外の件数（チップ）が変わる
  }).catch(e => toast('更新失敗: ' + e.message, true));
}
function rotatePhoto(photo, btn){
  btn.disabled = true;
  postJSON('/api/admin/rotate', {id: photo.id, delta: 90}).then(j => {
    applyPhoto(photo.id, {rotation: j.rotation, thumb: j.thumb, thumb_url: j.thumb_url || ''});
    toast('#' + photo.id + ' を回しました（' + j.rotation + '°）');
  }).catch(e => toast('回転失敗: ' + e.message, true))
    .finally(() => { btn.disabled = false; });
}

/* ==== 7) 位置指定モーダル（MapLibre） ==== */
/* 背景タイルは本体と同じ Rust のプロキシ（tiles.rs）経由（ローカルキャッシュを共有できる）。
   出典表示は地理院タイル・OSMいずれも利用条件で必須なので style に持たせる。
   MapLibre の attribution は HTML の文字列を受けるので、リンク先とテキストの定数から組む（定数のみ）。
   地名検索の OSM の出典は DOM で組む（osmCredit） */
const OSM_COPYRIGHT_URL = 'https://www.openstreetmap.org/copyright';
const OSM_NAME = 'OpenStreetMap';
const GSI_URL = 'https://maps.gsi.go.jp/development/ichiran.html';
const GSI_NAME = '国土地理院（地理院タイル）';
const TILE_ATTRIB_OSM = "© <a href='" + OSM_COPYRIGHT_URL + "'>" + OSM_NAME + '</a> contributors';
const TILE_ATTRIB_GSI = '出典: ' + "<a href='" + GSI_URL + "'>" + GSI_NAME + '</a>';
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
/* ---- 地名検索（地図を近くまで移動するショートカット。位置はここでは確定しない） ----
   国内は地理院、0件なら海外を含めてOpenStreetMap（Nominatim）で引く（サーバー側で切替）。
   候補クリック後も既存の「地図クリック→SAVE」フローは維持する。検索結果の代表点は
   住所の代表座標であり実際の撮影地点とは限らないため、自動確定はしない */
function clearGeocodeResults(){
  const box = $('#m-results');
  box.textContent = '';
  box.classList.add('hidden');
}
/* 海外の候補はOpenStreetMap（Nominatim）由来。ODbLの出典表示が要る（リンクは DOM で組む） */
function osmCredit(){
  const credit = el('div', 'm-noresult');
  credit.appendChild(document.createTextNode('地名データ © '));
  const a = el('a', null, OSM_NAME);
  a.setAttribute('href', OSM_COPYRIGHT_URL);
  a.setAttribute('target', '_blank');
  a.setAttribute('rel', 'noopener noreferrer');
  credit.appendChild(a);
  credit.appendChild(document.createTextNode(' contributors'));
  return credit;
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
      if(d.items.some(item => item.source === 'osm')) box.appendChild(osmCredit());
    }).catch(e => toast('検索失敗: ' + e.message, true));
}

/* ---- AIの撮影地候補（ai/location_suggest.rs）。候補の場所へ地図を動かすだけで、確定はしない ----
   地名・根拠はモデルの出力なので必ず textContent で入れる */
const PRECISION_LABEL = {landmark: '建物・施設', district: '地区', city: '市区町村',
                         region: '都道府県・州', country: '国', unknown: '不明'};
const PRECISION_ZOOM = {landmark: 16, district: 14, city: 11, region: 7, country: 5};
function suggestCenter(sg){
  if(!sg || sg.lat === null || sg.lat === undefined || !PRECISION_ZOOM[sg.precision]) return null;
  return {center: [sg.lon, sg.lat], zoom: PRECISION_ZOOM[sg.precision]};
}
function renderSuggest(photo){
  const box = $('#m-suggest');
  box.textContent = '';
  const sg = photo.suggest;
  box.classList.toggle('hidden', !sg || hasLoc(photo));
  if(!sg || hasLoc(photo)) return;
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

function openLocModal(photo){
  curPhoto = photo; picked = null;
  $('#m-q').value = '';
  clearGeocodeResults();
  renderSuggest(photo);
  $('#m-title').textContent = '#' + photo.id + '  ' + photo.name +
                              (photo.taken_at ? '  /  ' + photo.taken_at : '');
  $('#m-pick').textContent = '';
  $('#m-save').disabled = true;
  $('#m-hint').textContent = photo.hint && photo.hint.label
    ? ('参考: 近い時刻の写真は「' + photo.hint.label + '」付近（' +
       fmtDur(photo.hint.dt_s) + '差）')
    : '地図をクリックして位置を指定してください';
  $('#modal').classList.remove('hidden');
  const m = ensureMini();
  // 非表示状態で生成されると寸法が0になるため、表示直後にresizeする
  setTimeout(() => {
    m.resize();
    if(miniMarker){ miniMarker.remove(); miniMarker = null; }
    const c = hasLoc(photo) ? [photo.lon, photo.lat] : (photo.hint ? [photo.hint.lon, photo.hint.lat] : null);
    const sgo = suggestCenter(photo.suggest);
    if(suggestMarker){ suggestMarker.remove(); suggestMarker = null; }
    if(sgo && !hasLoc(photo)){
      suggestMarker = new maplibregl.Marker({color: '#ffb300'}).setLngLat(sgo.center).addTo(m);
      suggestMarker.getElement().title = 'AI候補: ' + photo.suggest.place_name + '（代表点）';
    }
    if(c) m.jumpTo({center: c, zoom: 11});
    else if(sgo) m.jumpTo(sgo);       // 近い時刻の写真が無ければAI候補の場所から始める
  }, 30);
}
function closeLocModal(){ $('#modal').classList.add('hidden'); curPhoto = null; }
/* 写真タブを離れるとき（admin.js の showTab から）: ビューアと位置モーダルを閉じる */
function closePhotoOverlays(){ closeModal(); closeLocModal(); }
/* 保存すると、同じ id の全カード（一覧・検索結果）の 1 行目が座標（サーバーは地名を空に戻す）、
   タグが「手動」になる。一覧のときは読み直す（位置なしのフィルタから外れるため） */
function saveLoc(){
  if(!curPhoto || !picked) return;
  const photo = curPhoto, pos = picked;
  $('#m-save').disabled = true;
  postJSON('/api/admin/setloc', {id: photo.id, lat: pos.lat, lon: pos.lon})
    .then(j => {
      applyPhoto(photo.id, {
        lat: hasValue(j.lat) ? j.lat : pos.lat, lon: hasValue(j.lon) ? j.lon : pos.lon,
        loc_source: 'manual', infer_bucket: '', place: '', pref: '', city: '',
      });
      toast('#' + photo.id + ' の位置を設定しました');
      closeLocModal();
      if(ps.view === 'list') loadList();
      loadFilterCounts();         // 位置なしの件数（チップ）が変わる
    })
    .catch(e => { toast('位置の保存に失敗: ' + e.message, true); $('#m-save').disabled = false; });
}

/* ==== 8) 初期化（admin.js の boot() から 1 回だけ） ==== */
function initPhotos(){
  document.querySelectorAll('.chip').forEach(b =>
    b.addEventListener('click', () => setFilter(b.dataset.filter)));
  $('#prev').addEventListener('click', () => { if(ps.page > 1){ ps.page--; loadList(); } });
  $('#next').addEventListener('click', () => { ps.page++; loadList(); });

  $('#search').addEventListener('click', () => search($('#q').value));
  $('#recommend').addEventListener('click', () => { $('#q').value = ''; search(''); });
  $('#q').addEventListener('keydown', e => {
    // 日本語入力の変換を確定する Enter では検索しない
    if(e.key === 'Enter' && !e.isComposing && e.keyCode !== 229){ e.preventDefault(); search($('#q').value); }
  });
  $('#include-hidden').addEventListener('change', researchWithToggle);
  $('#close-search').addEventListener('click', closeSearch);

  $('#m-close').addEventListener('click', closeLocModal);
  $('#m-save').addEventListener('click', saveLoc);
  $('#m-search').addEventListener('click', doGeocodeSearch);
  $('#m-q').addEventListener('keydown', e => {
    if(e.key === 'Enter'){ e.preventDefault(); doGeocodeSearch(); }
  });
  $('#modal').addEventListener('click', e => { if(e.target.id === 'modal') closeLocModal(); });

  $('#viewer-close').addEventListener('click', closeModal);
  $('#viewer-prev').addEventListener('click', () => stepModal(-1));
  $('#viewer-next').addEventListener('click', () => stepModal(1));
  $('#viewer').addEventListener('click', e => { if(e.target === $('#viewer')) closeModal(); });
  $('#viewer-favorite').addEventListener('click', () => {
    const entry = ps.entries[viewerIndex];
    if(entry) setFlag(entry.photo, {favorite: !entry.photo.favorite});
  });
  $('#viewer-hidden').addEventListener('click', () => {
    const entry = ps.entries[viewerIndex];
    if(entry) setFlag(entry.photo, {hidden: !entry.photo.hidden});
  });
  const media = $('#viewer-media');
  media.addEventListener('click', e => {
    if(suppressMediaClick || ps.entries.length < 2) return;
    const bounds = media.getBoundingClientRect();
    const position = (e.clientX - bounds.left) / bounds.width;
    if(position < 0.34) stepModal(-1);
    else if(position > 0.66) stepModal(1);
  });
  media.addEventListener('pointerdown', e => {
    if(!e.isPrimary) return;
    swipeStart = {id: e.pointerId, x: e.clientX, y: e.clientY};
  });
  media.addEventListener('pointerup', e => {
    if(!swipeStart || swipeStart.id !== e.pointerId) return;
    const dx = e.clientX - swipeStart.x;
    const dy = e.clientY - swipeStart.y;
    swipeStart = null;
    if(Math.abs(dx) < 55 || Math.abs(dx) < Math.abs(dy) * 1.2) return;
    suppressMediaClick = true;
    stepModal(dx < 0 ? 1 : -1);
    setTimeout(() => { suppressMediaClick = false; }, 180);
  });
  media.addEventListener('pointercancel', () => { swipeStart = null; });

  /* キー操作: ビューアが開いていれば Escape・矢印。開いていなければ位置指定モーダルの Escape */
  document.addEventListener('keydown', e => {
    if(viewerIndex >= 0){
      if(e.key === 'Escape'){ e.preventDefault(); closeModal(); }
      else if(e.key === 'ArrowLeft'){ e.preventDefault(); stepModal(-1); }
      else if(e.key === 'ArrowRight'){ e.preventDefault(); stepModal(1); }
      return;
    }
    if(e.key === 'Escape' && !$('#modal').classList.contains('hidden')) closeLocModal();
  });
  applyView();
}
