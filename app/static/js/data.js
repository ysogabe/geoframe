/* ============================================================
   data.js — 写真データ（モック / manifest）とその再索引
   ※読み込み順が契約: util.js の後（applyManifest が seeded を呼ぶ）、map.js より前
      （map.js のトップレベルが PHOTOS[0] を参照するため）
   宣言する主なグローバル: MOCK_PHOTOS, PHOTOS, usingMock, idById,
                           reindexPhotos, applyManifest, manifestReady
   依存する他ファイルのグローバル: seeded (util.js)
   ※末尾の const manifestReady = fetch(...) はトップレベル実行文。元の位置のまま
     （地図生成より前）に置いている
   ============================================================ */
const MOCK_PHOTOS = [
  { title:"東京タワーの夜景", lat:35.6586, lon:139.7454, time:"2026-05-03 19:42", img:"photos/p1.jpg",
    hue:[195,230], intel:{elev:18, youto:"商業地域", price:2850000, flood:"区域外", pop:4200, sta:112000} },
  { title:"渋谷スクランブル交差点", lat:35.6595, lon:139.7005, time:"2026-04-18 17:03", img:"photos/p2.jpg",
    hue:[280,320], intel:{elev:14, youto:"商業地域", price:12500000, flood:"0.5m未満", pop:9800, sta:680000} },
  { title:"江の島の夕暮れ", lat:35.2996, lon:139.4802, time:"2026-03-21 17:55", img:"photos/p3.jpg",
    hue:[15,45], intel:{elev:4, youto:"第一種住居地域", price:210000, flood:"津波想定 3m", pop:1200, sta:24000} },
  { title:"河口湖と富士山", lat:35.5171, lon:138.7511, time:"2026-01-11 08:12", img:"photos/p4.jpg",
    hue:[200,150], intel:{elev:833, youto:"用途地域外", price:38000, flood:"区域外", pop:310, sta:5200} },
  { title:"伏見稲荷の千本鳥居", lat:34.9671, lon:135.7727, time:"2025-11-24 09:30", img:"photos/p5.jpg",
    hue:[350,25], intel:{elev:38, youto:"第一種住居地域", price:185000, flood:"0.5〜3m", pop:2600, sta:31000} },
  { title:"道頓堀ネオン", lat:34.6687, lon:135.5013, time:"2025-10-02 20:15", img:"photos/p6.jpg",
    hue:[300,60], intel:{elev:2, youto:"商業地域", price:5100000, flood:"3〜5m", pop:7400, sta:250000} },
];
/* ==== manifest読込（server.py稼働時は実写真、失敗時はモックで継続） ==== */
let PHOTOS = MOCK_PHOTOS;
let usingMock = true;
let idById = {};        // 写真ID → PHOTOS の添字（再生キューはIDで持つのでこれで解決する）
function reindexPhotos(){
  idById = {};
  PHOTOS.forEach((p, i) => {
    // モック写真はIDを持たない → 合成して以降の機構をID基準で統一する
    if(p.id === undefined || p.id === null) p.id = 'm' + i;
    idById[p.id] = i;
  });
}
function applyManifest(list){
  if(!Array.isArray(list) || !list.length) return false;
  // 5分毎のmanifest再取得でPHOTOSを作り直すため、取得済みの実測intel(p.real)を引き継ぐ。
  // loadIntelは写真表示時にしか走らないので、引き継がないとティッカーから実測値が消える
  const prevReal = {};
  PHOTOS.forEach(p => { if(p.real) prevReal[p.id] = p.real; });
  PHOTOS = list.map(it => {
    const rnd = seeded((Math.abs(it.lat*1e4|0) + Math.abs(it.lon*1e4|0)) || 7);
    const youtos = ['商業地域','近隣商業地域','第一種住居地域','第二種住居地域','準工業地域','用途地域外'];
    const floods = ['区域外','区域外','0.5m未満','0.5〜3m','3〜5m'];
    // タイトルは地名ベース（例:「宮古島市 伊良部」）。地名未解決時のみファイル名
    const locTitle = it.city ? `${it.city}${it.place ? ' ' + it.place : ''}` : '';
    return {
      id: it.id, fetchedAt: it.fetched_at || '', fav: !!it.fav,
      qualityScore: it.quality_score == null ? null : +it.quality_score,
      qualityKind: it.quality_kind || '', qualityRank: it.quality_rank || null,
      qualityRecommended: !!it.quality_recommended, qualityRole: it.quality_role || '',
      title: locTitle || it.title, lat: it.lat, lon: it.lon,
      time: it.time || '', img: it.file, place: it.place || '',
      pref: it.pref || '', city: it.city || '', trip: it.trip,
      inferred: !!it.inferred, realElev: it.elev != null,
      hue: [Math.floor(rnd()*360), Math.floor(rnd()*360)],
      intel: {
        elev: it.elev != null ? it.elev : Math.round(rnd()*80),   // 実データ（地理院）
        youto: youtos[Math.floor(rnd()*youtos.length)],           // 以下モック
        price: Math.round(rnd()*900+30)*1000,
        flood: floods[Math.floor(rnd()*floods.length)],
        pop: Math.round(rnd()*8000),
        sta: Math.round(rnd()*300)*100,
      }
    };
  });
  PHOTOS.forEach(p => { if(prevReal[p.id]) p.real = prevReal[p.id]; });
  usingMock = false;
  reindexPhotos();
  return true;
}
const manifestReady = fetch('/api/photos')
  .then(r => r.ok ? r.json() : Promise.reject(r.status))
  .then(applyManifest)
  .catch(() => { /* file://直開き等 → モックで継続 */ });
