/* ============================================================
   admin.js — 管理ページ /admin のフロント（classic script / 依存なし）
   ※本体画面（static/js/*.js）とは完全に独立。グローバルを共有しない
   構成: 1) 共通ヘルパ 2) タブ 3) STATUS 4) 写真一覧 5) フラグ操作
        6) 位置指定モーダル（MapLibre） 7) 自宅設定 8) 起動
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
function showTab(name){
  document.querySelectorAll('nav .tab').forEach(b =>
    b.classList.toggle('on', b.dataset.tab === name));
  const view = (name === 'status') ? 'status' : (name === 'home') ? 'home' : 'list';
  $('#v-status').classList.toggle('hidden', view !== 'status');
  $('#v-list').classList.toggle('hidden', view !== 'list');
  $('#v-home').classList.toggle('hidden', view !== 'home');
  if(view === 'status'){ loadStatus(); return; }
  if(view === 'home'){ loadHome(); return; }
  setFilter(name === 'noloc' ? 'noloc' : 'all');
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
function loadStatus(){
  fetch('/api/admin/status').then(r => r.json()).then(d => {
    if(!d.ok) throw new Error(d.error || 'status error');
    $('#servertime').textContent = 'SERVER ' + d.server_time;

    const sy = $('#s-sync'); sy.textContent = '';
    sy.appendChild(cell('表示可能', d.sync.ready, '枚', 'good'));
    sy.appendChild(cell('取得待ち', d.sync.pending, '枚', d.sync.pending ? 'warn' : ''));
    sy.appendChild(cell('全件数', d.sync.total, '枚'));

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
    const names = {derivatives: 'derivatives/', tile_cache: 'tile_cache/', hazard_cache: 'hazard_cache/'};
    Object.keys(names).forEach(k => {
      const u = d.disk.dirs[k] || {};
      const c = cell(names[k], fmtBytes(u.bytes), '');
      c.appendChild(el('span', 'k', (u.files || 0) + ' ファイル'));
      dk.appendChild(c);
    });

    const ha = $('#s-ha'); ha.textContent = '';
    if(!d.ha.enabled){
      ha.appendChild(cell('連携', 'OFF', '', 'warn'));
      ha.appendChild(cell('理由', 'トークン未設定', ''));
    }else{
      const ok = d.ha.ok;
      ha.appendChild(cell('接続', ok === true ? 'OK' : (ok === null ? '未確認' : 'NG'),
                          '', ok === true ? 'good' : (ok === null ? 'warn' : 'bad')));
      ha.appendChild(cell('在室判定', d.ha.present ? '在室' : '不在', ''));
      ha.appendChild(cell('画面', String(d.ha.screen || '--').toUpperCase(), ''));
      ha.appendChild(cell('最終検知からの経過', Math.floor(d.ha.idle_s / 60), '分'));
      ha.appendChild(cell('人感エンティティ', d.ha.motion_entity || '未設定', ''));
      ha.appendChild(cell('ROOM ENV', (d.ha.room_env_ok ? 'OK ' : 'NG ') + d.ha.room_env_count,
                          '件', d.ha.room_env_ok ? 'good' : 'warn'));
    }
  }).catch(e => toast('STATUS取得失敗: ' + e.message, true));
}

/* ==== 4) 写真一覧 ==== */
const SRC_LABEL = {exif: 'EXIF', drive_meta: 'DRIVE', inferred: '推定', manual: '手動'};
function setFilter(f){
  curFilter = f; curPage = 1;
  document.querySelectorAll('.flt').forEach(b => b.classList.toggle('on', b.dataset.filter === f));
  loadList();
}
function loadList(){
  fetch('/api/admin/photos?filter=' + encodeURIComponent(curFilter) +
        '&page=' + curPage + '&per=50')
    .then(r => r.json()).then(d => {
      if(!d.ok) throw new Error(d.error || 'list error');
      curPage = d.page;
      $('#pageinfo').textContent = d.total + '件 / ' + d.page + ' of ' + d.pages + 'ページ';
      $('#prev').disabled = d.page <= 1;
      $('#next').disabled = d.page >= d.pages;
      $('#empty').classList.toggle('hidden', d.items.length > 0);
      const box = $('#cards'); box.textContent = '';
      d.items.forEach(it => box.appendChild(cardFor(it)));
    }).catch(e => toast('一覧取得失敗: ' + e.message, true));
}
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

  const m = el('div', 'meta');
  m.appendChild(el('div', 'dt', it.taken_at || '撮影時刻なし'));
  m.appendChild(el('div', 'nm', it.name));
  // 地名は位置がある行だけ表示する。推定を取り消された行には解決済みの地名が
  // 残っていることがあり（取り消し時にplaceを消していないため）、
  // それを出すと「位置なしなのに地名がある」という誤解を生む
  const hasLoc = (it.lat !== null && it.lat !== undefined);
  if(hasLoc){
    const loc = [it.pref, it.city, it.place].filter(Boolean).join(' ');
    m.appendChild(el('div', 'loc', loc || (it.lat.toFixed(4) + ', ' + it.lon.toFixed(4))));
  }
  if(it.hint && it.hint.label)
    m.appendChild(el('div', 'loc', '近い時刻の位置: ' + it.hint.label +
                                   '（' + fmtDur(it.hint.dt_s) + '差）'));
  const src = it.loc_source || 'none';
  const tag = el('span', 'tag t-' + src,
                 (SRC_LABEL[src] || '位置なし') + (it.infer_bucket ? ' ' + it.infer_bucket : ''));
  m.appendChild(tag);
  c.appendChild(m);

  const acts = el('div', 'acts');
  const fav = el('button', 'fav' + (it.fav ? ' on' : ''), '★');
  fav.title = 'お気に入り';
  fav.addEventListener('click', () => setFlag(c, it, {favorite: !it.fav}));
  const hid = el('button', 'hid' + (it.hidden ? ' on' : ''), '🚫');
  hid.title = '表示から除外';
  hid.addEventListener('click', () => setFlag(c, it, {hidden: !it.hidden}));
  const loc2 = el('button', 'setloc', '位置');
  loc2.title = '地図で位置を指定';
  loc2.addEventListener('click', () => openLocModal(it));
  acts.appendChild(fav); acts.appendChild(hid); acts.appendChild(loc2);
  c.appendChild(acts);
  return c;
}

/* ==== 5) フラグ操作 ==== */
function setFlag(card, it, patch){
  postJSON('/api/admin/setflag', Object.assign({id: it.id}, patch)).then(j => {
    it.fav = j.favorite; it.hidden = j.hidden;
    card.querySelector('.fav').classList.toggle('on', j.favorite);
    card.querySelector('.hid').classList.toggle('on', j.hidden);
    card.classList.toggle('is-hidden', j.hidden);
    toast('#' + it.id + ' 更新: ' + (j.favorite ? '★' : '☆') + (j.hidden ? ' 🚫除外' : ''));
  }).catch(e => toast('更新失敗: ' + e.message, true));
}

/* ==== 6) 位置指定モーダル（MapLibre） ==== */
/* 背景タイルは本体と同じ server.py のプロキシ経由（ローカルキャッシュを共有できる）。
   出典表示は地理院タイル・OSMいずれも利用条件で必須なので style に持たせる */
const TILE_ATTRIB_OSM = "© <a href='https://www.openstreetmap.org/copyright'>OpenStreetMap</a> contributors";
const TILE_ATTRIB_GSI = "出典: <a href='https://maps.gsi.go.jp/development/ichiran.html'>国土地理院（地理院タイル）</a>";
let mini = null, miniMarker = null, curPhoto = null, picked = null;

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
        box.appendChild(el('div', 'm-noresult', '見つかりませんでした（国内の地名のみ対応）'));
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
    }).catch(e => toast('検索失敗: ' + e.message, true));
}

function openLocModal(it){
  curPhoto = it; picked = null;
  $('#m-q').value = '';
  clearGeocodeResults();
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
    if(c) m.jumpTo({center: c, zoom: 11});
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
    })
    .catch(e => { toast('位置の保存に失敗: ' + e.message, true); $('#m-save').disabled = false; });
}

/* ==== 7) 自宅設定 ==== */
/* ドロップダウンの選択肢は必ずサーバー（/api/admin/home の prefs）から動的に読む。
   静的にハードコードすると config.PREF_TO_REGION のキー一覧とドリフトし得るため、
   正本はサーバー側の1箇所に固定する */
function loadHome(){
  fetch('/api/admin/home').then(r => r.json()).then(d => {
    if(!d.ok) throw new Error(d.error || 'home error');
    const sel = $('#home-pref');
    if(!sel.options.length){
      const opt0 = el('option', null, '未設定'); opt0.value = '';
      sel.appendChild(opt0);
      d.prefs.forEach(p => { const o = el('option', null, p); o.value = p; sel.appendChild(o); });
    }
    sel.value = d.home_pref || '';
    $('#home-region').textContent = d.region ? ('地方: ' + d.region) : '';
  }).catch(e => toast('自宅設定の取得失敗: ' + e.message, true));
}
function saveHome(){
  const pref = $('#home-pref').value || null;
  postJSON('/api/admin/sethome', {home_pref: pref}).then(d => {
    $('#home-region').textContent = d.region ? ('地方: ' + d.region) : '';
    toast(d.home_pref ? ('自宅を' + d.home_pref + 'に設定しました') : '自宅設定を解除しました');
  }).catch(e => toast('保存失敗: ' + e.message, true));
}

/* ==== 8) 起動 ==== */
document.querySelectorAll('nav .tab').forEach(b =>
  b.addEventListener('click', () => showTab(b.dataset.tab)));
document.querySelectorAll('.flt').forEach(b =>
  b.addEventListener('click', () => setFilter(b.dataset.filter)));
$('#prev').addEventListener('click', () => { if(curPage > 1){ curPage--; loadList(); } });
$('#next').addEventListener('click', () => { curPage++; loadList(); });
$('#m-close').addEventListener('click', closeLocModal);
$('#m-save').addEventListener('click', saveLoc);
$('#m-search').addEventListener('click', doGeocodeSearch);
$('#m-q').addEventListener('keydown', e => {
  if(e.key === 'Enter'){ e.preventDefault(); doGeocodeSearch(); }
});
$('#home-save').addEventListener('click', saveHome);
$('#modal').addEventListener('click', e => { if(e.target.id === 'modal') closeLocModal(); });
document.addEventListener('keydown', e => {
  if(e.key === 'Escape' && !$('#modal').classList.contains('hidden')) closeLocModal();
});
loadStatus();
setInterval(() => { if(!$('#v-status').classList.contains('hidden')) loadStatus(); }, 30000);
