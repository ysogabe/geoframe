(() => {
  'use strict';
  const $ = selector => document.querySelector(selector);
  const groupsNode = $('#groups');
  const messageNode = $('#message');
  let busy = false;
  let modalEntries = [];
  let modalIndex = -1;
  let modalMode = 'recommended';
  let modalLastFocus = null;
  let swipeStart = null;
  let suppressMediaClick = false;

  const SUBJECT_LABELS = {person:'人物',dog:'犬',cat:'猫',person_and_pet:'人物＋ペット',
    landscape:'風景',food:'食事',vehicle:'乗り物',other:'その他'};
  const PET_LABELS = {awake:'起きている',sleeping:'睡眠中',active:'活動中',uncertain:'不明',not_applicable:'対象外'};
  const EYE_LABELS = {open:'開眼',closed:'閉眼',mixed:'一部閉眼',not_visible:'目は見えない',uncertain:'不明',not_applicable:'対象外'};
  const CROP_LABELS = {good:'顔切れなし',intentional_closeup:'意図的なアップ',accidental_cut:'顔切れ注意',not_visible:'顔は見えない',uncertain:'不明',not_applicable:'対象外'};
  const HORIZON_LABELS = {level:'水平良好',intentional_tilt:'意図的な傾き',accidental_tilt:'水平傾き注意',uncertain:'不明',not_applicable:'対象外'};

  function toast(text, error=false){
    const node = $('#toast');
    node.textContent = text;
    node.className = 'show' + (error ? ' error' : '');
    clearTimeout(node._timer);
    node._timer = setTimeout(() => { node.className = ''; }, 2600);
  }

  async function getJSON(url){
    const response = await fetch(url, {headers:{'Accept':'application/json'}});
    const data = await response.json().catch(() => ({}));
    if(!response.ok || !data.ok) throw new Error(data.error || `HTTP ${response.status}`);
    return data;
  }

  async function postJSON(url, payload){
    const response = await fetch(url, {
      method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(payload)
    });
    const data = await response.json().catch(() => ({}));
    if(!response.ok || !data.ok) throw new Error(data.error || `HTTP ${response.status}`);
    return data;
  }

  function badge(text, className=''){
    const node = document.createElement('span');
    node.className = `badge ${className}`.trim();
    node.textContent = text;
    return node;
  }

  function renderBadges(container, photo){
    container.replaceChildren();
    if(photo.recommended === true) container.appendChild(badge('AI PICK', 'rec'));
    if(photo.favorite){
      const favoriteBadge = badge('FAVORITE', 'rec');
      favoriteBadge.classList.add('favorite-state-badge');
      container.appendChild(favoriteBadge);
    }
    if(photo.subject) container.appendChild(badge(
      SUBJECT_LABELS[photo.subject] || photo.subject,
      ['dog','cat'].includes(photo.subject) ? 'pet' : ''));
    if(photo.eyes === 'open') container.appendChild(badge('EYES OPEN'));
    if(photo.horizon_status === 'level') container.appendChild(badge('LEVEL', 'rec'));
    if(photo.horizon_status === 'intentional_tilt') container.appendChild(badge('INTENTIONAL TILT'));
    if(photo.horizon_status === 'accidental_tilt') container.appendChild(badge('TILT', 'pet'));
  }

  function updatePhotoState(id, state){
    modalEntries.forEach(entry => {
      if(entry.photo.id === id){
        entry.photo.favorite = state.favorite;
        entry.photo.hidden = state.hidden;
      }
    });
    document.querySelectorAll(`.photo-card[data-photo-id="${id}"]`).forEach(card => {
      card.dataset.favorite = state.favorite ? '1' : '0';
      card.dataset.hidden = state.hidden ? '1' : '0';
      card.classList.toggle('is-hidden', state.hidden);
      const fav = card.querySelector('.favorite');
      const hid = card.querySelector('.hidden-button');
      fav.classList.toggle('on', state.favorite);
      fav.textContent = state.favorite ? '★ favorite' : '☆ favorite';
      hid.classList.toggle('on', state.hidden);
      hid.textContent = state.hidden ? '● hidden' : '○ hidden';
      const entry = modalEntries.find(item => item.photo.id === id);
      if(entry) renderBadges(card.querySelector('.badges'), entry.photo);
    });
    if(modalIndex >= 0 && modalEntries[modalIndex]?.photo.id === id) renderModal();
  }

  async function setPhotoFlag(photo, patch){
    const data = await postJSON('/api/admin/setflag', Object.assign({id:photo.id}, patch));
    updatePhotoState(photo.id, data);
    toast(`#${photo.id} ${Object.keys(patch)[0]}を更新しました`);
    return data;
  }

  function photoCard(photo, mode, entryIndex, groupUi){
    const card = $('#photo-template').content.firstElementChild.cloneNode(true);
    card.dataset.photoId = photo.id;
    card.dataset.favorite = photo.favorite ? '1' : '0';
    card.dataset.hidden = photo.hidden ? '1' : '0';
    card.dataset.visualCluster = photo.visual_cluster;
    card.classList.toggle('is-hidden', photo.hidden);
    card.classList.toggle('visual-representative', photo.visual_representative);
    const image = card.querySelector('img');
    image.src = photo.thumb;
    image.alt = photo.name;
    image.onerror = () => { image.alt = '画像を読み込めません'; };
    const imageWrap = card.querySelector('.image-wrap');
    imageWrap.tabIndex = 0;
    imageWrap.setAttribute('role', 'button');
    imageWrap.setAttribute('aria-label', `${photo.name}の詳細を表示`);
    imageWrap.addEventListener('click', () => openModal(entryIndex));
    imageWrap.addEventListener('keydown', event => {
      if(event.key === 'Enter' || event.key === ' '){
        event.preventDefault(); openModal(entryIndex);
      }
    });
    const pct = Math.round(photo.similarity * 100);
    card.querySelector('.score').textContent = mode === 'semantic' ? `${pct}% MATCH` : `${pct} SCORE`;
    const visualPct = Math.round((photo.visual_similarity ?? 1) * 100);
    card.querySelector('.visual-similarity').textContent = photo.visual_representative
      ? `構図 ${photo.visual_cluster} 代表` : `代表と ${visualPct}%`;

    renderBadges(card.querySelector('.badges'), photo);
    card.querySelector('.photo-name').textContent = `#${photo.id}  ${photo.name}`;
    const location = photo.place || [photo.pref, photo.city].filter(Boolean).join(' ');
    card.querySelector('.photo-meta').textContent = [photo.taken_at, location].filter(Boolean).join(' / ') || '日時・場所なし';
    card.querySelector('.reason').textContent = (photo.reasons || [])[0] ||
      (photo.keep_score != null ? `keep ${photo.keep_score} / aesthetic ${photo.aesthetic_score ?? '—'}` +
        (photo.horizon_score != null ? ` / level ${photo.horizon_score}` : '') : 'AI詳細評価なし');

    const clusterMore = card.querySelector('.cluster-more');
    if(photo.visual_representative && photo.visual_cluster_size > 1){
      clusterMore.classList.remove('hidden');
      clusterMore.addEventListener('click', event => {
        event.stopPropagation();
        groupUi.toggleCluster(photo.visual_cluster);
      });
    }

    const fav = card.querySelector('.favorite');
    const hid = card.querySelector('.hidden-button');
    updatePhotoStateLater(card, photo);
    fav.addEventListener('click', async event => {
      event.stopPropagation();
      try{
        await setPhotoFlag(photo, {favorite:card.dataset.favorite !== '1'});
      }catch(error){ toast(`更新失敗: ${error.message}`, true); }
    });
    hid.addEventListener('click', async event => {
      event.stopPropagation();
      try{
        await setPhotoFlag(photo, {hidden:card.dataset.hidden !== '1'});
      }catch(error){ toast(`更新失敗: ${error.message}`, true); }
    });
    return card;
  }

  function updatePhotoStateLater(card, photo){
    card.classList.toggle('is-hidden', photo.hidden);
    const fav = card.querySelector('.favorite');
    const hid = card.querySelector('.hidden-button');
    fav.classList.toggle('on', photo.favorite);
    fav.textContent = photo.favorite ? '★ favorite' : '☆ favorite';
    hid.classList.toggle('on', photo.hidden);
    hid.textContent = photo.hidden ? '● hidden' : '○ hidden';
  }

  function appendScore(label, value){
    if(value === null || value === undefined || value === '') return;
    const box = document.createElement('div');
    box.className = 'modal-score';
    const number = document.createElement('b');
    number.textContent = String(value);
    const caption = document.createElement('span');
    caption.textContent = label;
    box.append(number, caption);
    $('#modal-scores').appendChild(box);
  }

  function appendAnalysis(label, value){
    if(!value) return;
    const term = document.createElement('dt');
    const detail = document.createElement('dd');
    term.textContent = label; detail.textContent = value;
    $('#modal-analysis').append(term, detail);
  }

  function fillList(node, values, fallback){
    node.replaceChildren();
    const items = values && values.length ? values : (fallback ? [fallback] : []);
    items.forEach(value => {
      const item = document.createElement('li'); item.textContent = value; node.appendChild(item);
    });
  }

  function renderModal(){
    const entry = modalEntries[modalIndex];
    if(!entry) return;
    const photo = entry.photo;
    const labels = {scene:'SAME SCENE',pet:'PET PHOTO',similar:'SIMILAR BURST',single:'SINGLE'};
    $('#modal-image').src = photo.thumb;
    $('#modal-image').alt = photo.name;
    $('#modal-photo-title').textContent = `#${photo.id}  ${photo.name}`;
    $('#modal-photo-meta').textContent = photo.taken_at || '撮影時刻なし';
    $('#modal-counter').textContent = `${modalIndex + 1} / ${modalEntries.length}`;
    $('#modal-group-kind').textContent = `${labels[entry.group.kind] || entry.group.kind} · GROUP ${entry.groupIndex + 1}` +
      ` · ${entry.photoIndex + 1}/${entry.group.photos.length}`;
    $('#modal-group-summary').textContent = entry.group.summary ||
      (entry.group.photos.length > 1 ? `${entry.group.photos.length}枚の類似写真` : '単独の検索候補');

    const pct = Math.round(photo.similarity * 100);
    $('#modal-image-score').textContent = modalMode === 'semantic' ? `${pct}% MATCH` : `${pct} SCORE`;
    renderBadges($('#modal-badges'), photo);

    const location = photo.place || [photo.pref, photo.city].filter(Boolean).join(' ');
    const locationNode = $('#modal-location');
    locationNode.replaceChildren();
    locationNode.appendChild(document.createTextNode(location || '撮影地情報なし'));
    if(photo.lat !== null && photo.lat !== undefined && photo.lon !== null && photo.lon !== undefined){
      const coords = document.createElement('span');
      coords.className = 'coords';
      coords.textContent = `${Number(photo.lat).toFixed(5)}, ${Number(photo.lon).toFixed(5)}`;
      locationNode.appendChild(coords);
    }

    $('#modal-scores').replaceChildren();
    appendScore(modalMode === 'semantic' ? 'MATCH' : 'SEARCH SCORE', pct);
    appendScore('採用', photo.keep_score);
    appendScore('美的', photo.aesthetic_score);
    appendScore('主被写体', photo.subject_sharpness);
    appendScore('水平', photo.horizon_score);
    appendScore('構図類似', Math.round((photo.visual_similarity ?? 1) * 100));

    $('#modal-analysis').replaceChildren();
    appendAnalysis('主題', SUBJECT_LABELS[photo.subject] || photo.subject);
    appendAnalysis('犬猫の状態', PET_LABELS[photo.pet_state] || photo.pet_state);
    appendAnalysis('開眼', EYE_LABELS[photo.eyes] || photo.eyes);
    appendAnalysis('顔切れ', CROP_LABELS[photo.face_crop] || photo.face_crop);
    appendAnalysis('水平', HORIZON_LABELS[photo.horizon_status] || photo.horizon_status);
    appendAnalysis('AI採用', photo.recommended === true ? '推奨' :
      (photo.recommended === false ? '非推奨' : '未評価'));
    appendAnalysis('類似セット', photo.visual_representative
      ? `構図代表・全${photo.visual_cluster_size || 1}枚`
      : `代表と${Math.round((photo.visual_similarity ?? 1) * 100)}%・全${photo.visual_cluster_size || 1}枚`);

    fillList($('#modal-reasons'), photo.reasons, 'AI詳細評価なし');
    fillList($('#modal-concerns'), photo.concerns, 'なし');
    const fav = $('#modal-favorite');
    const hid = $('#modal-hidden');
    fav.classList.toggle('on', photo.favorite);
    fav.textContent = photo.favorite ? '★ favorite' : '☆ favorite';
    hid.classList.toggle('on', photo.hidden);
    hid.textContent = photo.hidden ? '● hidden' : '○ hidden';

    const multiple = modalEntries.length > 1;
    $('#modal-prev').disabled = !multiple;
    $('#modal-next').disabled = !multiple;
    if(multiple){
      const previous = modalEntries[(modalIndex - 1 + modalEntries.length) % modalEntries.length].photo;
      const next = modalEntries[(modalIndex + 1) % modalEntries.length].photo;
      [previous.thumb, next.thumb].forEach(src => { const preload = new Image(); preload.src = src; });
    }
  }

  function openModal(index){
    if(index < 0 || index >= modalEntries.length) return;
    modalIndex = index;
    modalLastFocus = document.activeElement;
    renderModal();
    $('#photo-modal').classList.remove('hidden');
    document.body.classList.add('modal-open');
    $('#modal-close').focus();
  }

  function closeModal(){
    if(modalIndex < 0) return;
    $('#photo-modal').classList.add('hidden');
    document.body.classList.remove('modal-open');
    modalIndex = -1;
    if(modalLastFocus && document.contains(modalLastFocus)) modalLastFocus.focus();
    modalLastFocus = null;
  }

  function stepModal(delta){
    if(modalIndex < 0 || modalEntries.length < 2) return;
    modalIndex = (modalIndex + delta + modalEntries.length) % modalEntries.length;
    renderModal();
  }

  function visualOrder(group){
    const clusters = new Map();
    group.photos.forEach(photo => {
      if(!clusters.has(photo.visual_cluster)) clusters.set(photo.visual_cluster, []);
      clusters.get(photo.visual_cluster).push(photo);
    });
    return Array.from(clusters.values()).flatMap(photos => photos.sort((left, right) =>
      Number(right.visual_representative) - Number(left.visual_representative) ||
      (right.visual_similarity ?? 0) - (left.visual_similarity ?? 0) ||
      (right.keep_score ?? -1) - (left.keep_score ?? -1)));
  }

  function applyGroupVisibility(group, section, state){
    state.cards.forEach((card, photo) => {
      const expanded = state.showAll || state.expanded.has(photo.visual_cluster);
      card.classList.toggle('cluster-collapsed', !photo.visual_representative && !expanded);
      const more = card.querySelector('.cluster-more');
      if(photo.visual_representative && photo.visual_cluster_size > 1){
        more.classList.toggle('hidden', state.showAll);
        more.textContent = expanded
          ? `類似${photo.visual_cluster_size - 1}枚を閉じる`
          : `＋${photo.visual_cluster_size - 1}枚の類似を表示`;
      }
    });
    const toggle = section.querySelector('.group-expand');
    toggle.textContent = state.showAll
      ? `代表だけ表示（${group.visual_cluster_count}構図）`
      : `すべて表示（${group.photos.length}枚）`;
  }

  function render(data){
    closeModal();
    modalMode = data.mode;
    modalEntries = [];
    const orderedGroups = data.groups.map(group => visualOrder(group));
    data.groups.forEach((group, groupIndex) => {
      orderedGroups[groupIndex].forEach((photo, photoIndex) => {
        modalEntries.push({photo, group, groupIndex, photoIndex});
      });
    });
    const entryIndexByPhoto = new Map(modalEntries.map((entry, index) => [entry.photo, index]));
    groupsNode.replaceChildren();
    messageNode.className = 'message hidden';
    $('#result-title').textContent = data.query ? `「${data.query}」` : 'おすすめ写真';
    const returnedPhotos = data.groups.reduce((sum, group) => sum + group.photos.length, 0);
    const visualClusters = data.groups.reduce((sum, group) => sum + group.visual_cluster_count, 0);
    $('#result-meta').textContent = `${data.searched_photos} SEARCHED / ${returnedPhotos}枚 → ${visualClusters}構図`;
    if(!data.groups.length){
      messageNode.textContent = '検索できる写真がありません。PoC結果のSQLite取り込みを確認してください。';
      messageNode.className = 'message'; return;
    }
    data.groups.forEach((group, index) => {
      const section = $('#group-template').content.firstElementChild.cloneNode(true);
      const labels = {scene:'SAME SCENE', pet:'PET PHOTO', similar:'SIMILAR BURST', single:'SINGLE'};
      section.querySelector('.kind').textContent = `${String(index+1).padStart(2,'0')} / ${labels[group.kind] || group.kind}`;
      section.querySelector('.summary').textContent = group.summary ||
        (group.photos.length > 1 ? `${group.photos.length}枚の類似写真` : '検索候補');
      section.querySelector('.match').textContent = data.mode === 'semantic'
        ? `${Math.round(group.best_similarity * 100)}% MATCH · ${group.photos.length}枚`
        : `${group.photos.length} PHOTO${group.photos.length > 1 ? 'S' : ''}`;
      const clusterStat = section.querySelector('.cluster-stat');
      clusterStat.textContent = group.collapsed_photo_count
        ? `${group.visual_cluster_count}構図 / 類似${group.collapsed_photo_count}枚を整理`
        : `${group.visual_cluster_count}構図`;
      if(group.avg_similarity != null){
        clusterStat.title = `場面全体のembedding類似度: 平均${Math.round(group.avg_similarity * 100)}%` +
          `・範囲${Math.round(group.min_similarity * 100)}〜${Math.round(group.max_similarity * 100)}%`;
      }
      const grid = section.querySelector('.photo-grid');
      const state = {
        showAll:false, expanded:new Set(), cards:new Map(),
        toggleCluster(cluster){
          if(this.expanded.has(cluster)) this.expanded.delete(cluster);
          else this.expanded.add(cluster);
          applyGroupVisibility(group, section, this);
        },
      };
      orderedGroups[index].forEach(photo => {
        const entryIndex = entryIndexByPhoto.get(photo);
        const card = photoCard(photo, data.mode, entryIndex, state);
        state.cards.set(photo, card);
        grid.appendChild(card);
      });
      const expand = section.querySelector('.group-expand');
      expand.classList.toggle('hidden', !group.collapsed_photo_count);
      expand.addEventListener('click', () => {
        state.showAll = !state.showAll;
        applyGroupVisibility(group, section, state);
      });
      applyGroupVisibility(group, section, state);
      groupsNode.appendChild(section);
    });
  }

  async function search(){
    if(busy) return;
    closeModal();
    busy = true; $('#search-button').disabled = true; $('#search-button').textContent = '検索中…';
    messageNode.textContent = $('#query').value.trim() ? 'Geminiで検索文を解析しています…' : 'SQLiteからおすすめを読み込んでいます…';
    messageNode.className = 'message'; groupsNode.replaceChildren();
    const params = new URLSearchParams({q:$('#query').value.trim(), groups:'12'});
    if($('#include-hidden').checked) params.set('include_hidden','1');
    try{ render(await getJSON(`/api/ai/search?${params}`)); }
    catch(error){
      messageNode.textContent = `検索できませんでした: ${error.message}`;
      messageNode.className = 'message error'; toast(error.message, true);
    }finally{ busy = false; $('#search-button').disabled = false; $('#search-button').textContent = '検索'; }
  }

  $('#search-form').addEventListener('submit', event => { event.preventDefault(); search(); });
  document.querySelectorAll('[data-query]').forEach(button => button.addEventListener('click', () => {
    $('#query').value = button.dataset.query; search();
  }));
  $('#include-hidden').addEventListener('change', search);

  $('#modal-close').addEventListener('click', closeModal);
  $('#modal-prev').addEventListener('click', () => stepModal(-1));
  $('#modal-next').addEventListener('click', () => stepModal(1));
  $('#photo-modal').addEventListener('click', event => {
    if(event.target === $('#photo-modal')) closeModal();
  });
  $('#modal-favorite').addEventListener('click', async () => {
    const photo = modalEntries[modalIndex]?.photo;
    if(!photo) return;
    try{ await setPhotoFlag(photo, {favorite:!photo.favorite}); }
    catch(error){ toast(`更新失敗: ${error.message}`, true); }
  });
  $('#modal-hidden').addEventListener('click', async () => {
    const photo = modalEntries[modalIndex]?.photo;
    if(!photo) return;
    try{ await setPhotoFlag(photo, {hidden:!photo.hidden}); }
    catch(error){ toast(`更新失敗: ${error.message}`, true); }
  });

  const modalMedia = $('#modal-media');
  modalMedia.addEventListener('click', event => {
    if(suppressMediaClick || modalEntries.length < 2) return;
    const bounds = modalMedia.getBoundingClientRect();
    const position = (event.clientX - bounds.left) / bounds.width;
    if(position < .34) stepModal(-1);
    else if(position > .66) stepModal(1);
  });
  modalMedia.addEventListener('pointerdown', event => {
    if(!event.isPrimary) return;
    swipeStart = {id:event.pointerId, x:event.clientX, y:event.clientY};
  });
  modalMedia.addEventListener('pointerup', event => {
    if(!swipeStart || swipeStart.id !== event.pointerId) return;
    const dx = event.clientX - swipeStart.x;
    const dy = event.clientY - swipeStart.y;
    swipeStart = null;
    if(Math.abs(dx) < 55 || Math.abs(dx) < Math.abs(dy) * 1.2) return;
    suppressMediaClick = true;
    stepModal(dx < 0 ? 1 : -1);
    window.setTimeout(() => { suppressMediaClick = false; }, 180);
  });
  modalMedia.addEventListener('pointercancel', () => { swipeStart = null; });
  document.addEventListener('keydown', event => {
    if(modalIndex < 0) return;
    if(event.key === 'Escape'){ event.preventDefault(); closeModal(); }
    else if(event.key === 'ArrowLeft'){ event.preventDefault(); stepModal(-1); }
    else if(event.key === 'ArrowRight'){ event.preventDefault(); stepModal(1); }
  });

  getJSON('/api/ai/status').then(data => {
    $('#coverage').textContent = `${data.photos}/${data.eligible}枚 embedding / ` +
      `${data.judged}枚 詳細評価 / ${data.groups}グループ`;
  }).catch(error => { $('#coverage').textContent = `DB確認失敗: ${error.message}`; });
  search();
})();
