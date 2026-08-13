/* ============================================================
   map.js — 背景タイル定義・MapLibre地図の生成・演出用GISレイヤ
   ※読み込み順が契約: data.js の後（トップレベルで PHOTOS[0] を読む）、
      他の全ての map.on(...) 登録より前（const map をここで生成するため）
   宣言する主なグローバル: ptsData, TILE_MOCK, OSM_TILES, GSI_TILES, JP_BOUNDS,
                           GSI_MINZOOM, inJapan, wantOsm, osmVisible, map,
                           setOsm, armBase, fitZoom, mockPolys,
                           layersAdded, started, tryAddLayers,
                           PREF_GEOJSON, PREF_VISITED_C, PREF_PLAIN_C, prefData,
                           prefVisited, prefAdded, prefLoading, prefBusy,
                           prefVisible, paintPref, tryAddPrefLayer,
                           ensurePrefLayer, setPrefVisible
   依存する他ファイルのグローバル: LOW_POWER (config.js), seeded (util.js),
                                   PHOTOS (data.js)
   ※トップレベル実行文（osmVisible の初期化 / new maplibregl.Map / body.low 付与 /
     map.on('moveend')）は元の相対順序のまま
   ※起動配線（startOnce / map.on('load') / map.on('styledata') / setTimeout(4500)）は
     start() を参照するため main.js へ移した（元 index.html の 519〜523 行）
   ============================================================ */
function ptsData(){
  return {type:'FeatureCollection',features:PHOTOS.map(p=>({type:'Feature',properties:{},
    geometry:{type:'Point',coordinates:[p.lon,p.lat]}}))};
}

/* ==== 地図 ==== */
/* 背景タイルは通常 server.py のプロキシ経由（ローカルキャッシュされ、回線断でも既訪問地域は出る）。
   file:// で開くモックモードだけはプロキシが居ないので上流へ直接アクセスする */
const TILE_MOCK = location.protocol === 'file:';
const OSM_TILES = TILE_MOCK ? 'https://tile.openstreetmap.org/{z}/{x}/{y}.png'
                            : location.origin + '/tiles/osm/{z}/{x}/{y}.png';
const GSI_TILES = TILE_MOCK ? 'https://cyberjapandata.gsi.go.jp/xyz/pale/{z}/{x}/{y}.png'
                            : location.origin + '/tiles/gsi/{z}/{x}/{y}.png';
/* 地理院タイル(pale)の実質提供範囲。域外・低ズームを要求しない（毎回404になるため） */
const JP_BOUNDS = [122.5, 20.2, 154.5, 46.5];
const GSI_MINZOOM = 5;
function inJapan(lon, lat){
  return lon>=JP_BOUNDS[0] && lon<=JP_BOUNDS[2] && lat>=JP_BOUNDS[1] && lat<=JP_BOUNDS[3];
}
/* 国内かつ地理院タイルが出るズームなら、OSMは重なって見えないので取得ごと止める（二重取得の解消）。
   国外・広域ズームでは地理院が出ないのでOSMが必要 */
function wantOsm(lon, lat, zoom){ return !(inJapan(lon, lat) && zoom>=GSI_MINZOOM+0.5); }
/* 初期表示分もここで決める（起動直後に国内OSMを1画面分取りに行くのを防ぐ） */
let osmVisible = wantOsm(PHOTOS[0].lon, PHOTOS[0].lat, 14.2);

const map = new maplibregl.Map({
  container:'map',
  style:{ version:8, sources:{
      osm:{ type:'raster', tiles:[OSM_TILES],
            tileSize:256, maxzoom:19,
            attribution:"© <a href='https://www.openstreetmap.org/copyright'>OpenStreetMap</a> contributors" },
      gsi:{ type:'raster', tiles:[GSI_TILES],
            tileSize:256, minzoom:GSI_MINZOOM, maxzoom:18, bounds:JP_BOUNDS,
            attribution:"地図データ 出典: <a href='https://maps.gsi.go.jp/development/ichiran.html'>国土地理院（地理院タイル）</a> | 地図上の色面は演出用の装飾" } },
    layers:[ {id:'bg', type:'background', paint:{'background-color':'#e8eef4'}},
             {id:'osm', type:'raster', source:'osm',   // 世界地図（海外・広域用ベース）
              layout:{visibility: osmVisible?'visible':'none'}},
             {id:'gsi', type:'raster', source:'gsi'} ] },   // 国内は地理院タイルが上に重なる
  center:[PHOTOS[0].lon, PHOTOS[0].lat], zoom:14.2,
  pitch:LOW_POWER?0:48, bearing:LOW_POWER?0:-12,
  refreshExpiredTiles:false,      // タイルは不変。期限切れ再取得で回線を使わない
  attributionControl:{compact:false}, interactive:true
});
if(LOW_POWER) document.body.classList.add('low');

/* OSMベースの表示切替。現在の状態はMapLibreに問い合わせず自前で持つ
   （getLayoutPropertyは未設定時undefinedを返すので比較に使えない） */
function setOsm(visible){
  if(visible===osmVisible) return;      // 変化なしなら何もしない
  osmVisible = visible;
  try{
    if(map.getLayer('osm'))
      map.setLayoutProperty('osm','visibility', visible?'visible':'none');
  }catch(e){}
}
/* カメラ移動の「行き先」から背景を先に決める。移動を始める前に呼ぶこと。
   これで国内に居る間はOSMタイルのリクエストが1本も出ない */
function armBase(lon, lat, zoom){ setOsm(wantOsm(lon, lat, zoom)); }
/* fitBounds用のズーム概算（実際より低めに見積もる＝OSMを消しすぎない安全側） */
function fitZoom(b, maxZoom){
  const w=Math.max(1e-6, b[2]-b[0]), h=Math.max(1e-6, b[3]-b[1]);
  return Math.min(maxZoom, Math.log2(360/w)-1, Math.log2(170/h)-1);
}
/* 保険: 手動操作やfitBounds着地後の実際のcenter/zoomでもう一度判定する */
map.on('moveend', ()=>{
  const c=map.getCenter(); armBase(c.lng, c.lat, map.getZoom());
});

/* 演出用モックGISポリゴン（撮影地点周辺に生成）
   ※canvasはinvert+hue-rotateフィルタ下で色相はおおむね維持される */
function mockPolys(){
  const feats=[];
  // 同一地点（約1km格子）に写真が集中してもポリゴンは1セットだけ生成する
  const uniq=[], seen=new Set();
  for(const p of PHOTOS){
    const k=`${Math.round(p.lat*100)},${Math.round(p.lon*100)}`;
    if(!seen.has(k)){ seen.add(k); uniq.push(p); }
    if(uniq.length>=40) break;
  }
  uniq.forEach((p,i)=>{
    const rnd = seeded(1000+i*77);
    const mk=(cls,n,scale)=>{ for(let k=0;k<n;k++){
      const cx=p.lon+(rnd()-.5)*.012, cy=p.lat+(rnd()-.5)*.009, r=.0012+rnd()*scale, pts=[];
      for(let a=0;a<=8;a++){ const th=a/8*Math.PI*2, rr=r*(0.7+rnd()*.6);
        pts.push([cx+Math.cos(th)*rr, cy+Math.sin(th)*rr*0.8]); }
      feats.push({type:'Feature', properties:{cls}, geometry:{type:'Polygon', coordinates:[pts]}}); } };
    mk('urban',3,.002); mk('hazard',2,.0015); mk('econ',1,.001);
  });
  return {type:'FeatureCollection', features:feats};
}
/* 初期化はイベント非依存で冪等に行う:
   タイル取得に失敗する環境（オフライン・不安定回線）では 'load' が発火しない
   ことがあるため、load / styledata / タイムアウトの三系統から起動する */
let layersAdded=false, started=false;
function tryAddLayers(){
  if(layersAdded) return;
  try{
    map.addSource('mock', {type:'geojson', data:mockPolys()});
    const colors={urban:'#00e5ff', hazard:'#ff2d95', econ:'#ffb300'};
    Object.entries(colors).forEach(([cls,c])=>{
      map.addLayer({id:'f-'+cls, type:'fill', source:'mock', filter:['==',['get','cls'],cls],
        paint:{'fill-color':c,'fill-opacity':0.10}});
      map.addLayer({id:'l-'+cls, type:'line', source:'mock', filter:['==',['get','cls'],cls],
        paint:{'line-color':c,'line-opacity':0.55,'line-width':1.2}});
    });
    map.addSource('pts',{type:'geojson',data:ptsData()});
    map.addLayer({id:'pts',type:'circle',source:'pts',
      paint:{'circle-radius':4,'circle-color':'#00e5ff','circle-opacity':.8,
             'circle-stroke-width':6,'circle-stroke-color':'#00e5ff','circle-stroke-opacity':.15}});
    layersAdded=true;
  }catch(e){ /* styleが未ロードなら次のイベントで再試行 */ }
}

/* ============================================================
   都道府県 制覇マップ（統計カット専用レイヤ）
   vendor/pref.geojson（Natural Earth 10m admin-1 を簡略化。出典は vendor/README.md）を
   遅延ロードし、訪問済み/未訪問を match 式で塗り分ける。
   ★通常の写真カットでは出さない。表示は showStats() の間だけ（setPrefVisible で切替）
   ============================================================ */
const PREF_GEOJSON = 'vendor/pref.geojson';   // index.html と同じ相対パス（file://でも壊れない）
const PREF_VISITED_C = '#00e5ff';             // 訪問済み（UI基調のシアン）
const PREF_PLAIN_C   = '#7d93ab';             // 未訪問（くすませる）
let prefData=null;        // fetch済みのGeoJSON（取得は1回だけ）
let prefVisited=[];       // 最後に受け取った訪問済み県名
let prefAdded=false, prefLoading=false, prefBusy=false, prefVisible=false;

/* 訪問済み県名の配列から fill-color / fill-opacity の match 式を作って流し込む。
   ★match は [input, ラベル配列, 出力, …, 既定値] の形。ラベル配列が空だと式として
     不正なので、訪問0県のときは定数に落とす */
function paintPref(visited){
  if(!prefAdded) return;
  const names = (visited||[]).filter(n=>typeof n==='string' && n);
  const color = names.length ? ['match',['get','name'], names, PREF_VISITED_C, PREF_PLAIN_C]
                             : PREF_PLAIN_C;
  const op    = names.length ? ['match',['get','name'], names, 0.30, 0.05] : 0.05;
  try{
    map.setPaintProperty('pref-fill','fill-color', color);
    map.setPaintProperty('pref-fill','fill-opacity', op);
    map.setPaintProperty('pref-line','line-color', color);
  }catch(e){}
}

/* GeoJSONが手元にあればレイヤを足す。冪等。
   ★tryAddLayers と同じ理由で「イベント非依存＋何度でも呼べる」形にしてある:
     styleのロードが終わっていない間 addSource は例外（Style is not done loading）を投げるので、
     ここで握り潰して styledata のたびに再試行する。回線が遅い/タブが非表示で
     rAFが止まっている等でstyleのロードが遅れても、後から必ず追い付く */
function tryAddPrefLayer(){
  if(prefAdded || prefBusy || !prefData) return;
  prefBusy = true;             // addSource が同期でstyledataを再発火しても二重登録しない
  try{
    // 装飾ポリゴンより下に敷く。tryAddLayers がまだ通っていない場合もあるので存在確認する
    const below = map.getLayer('f-urban') ? 'f-urban' : undefined;
    map.addSource('pref', {type:'geojson', data:prefData});
    map.addLayer({id:'pref-fill', type:'fill', source:'pref',
      layout:{visibility:'none'},
      paint:{'fill-color':PREF_PLAIN_C,'fill-opacity':0.05}}, below);
    map.addLayer({id:'pref-line', type:'line', source:'pref',
      layout:{visibility:'none'},
      paint:{'line-color':PREF_PLAIN_C,'line-opacity':0.5,'line-width':0.7}}, below);
    prefAdded = true;
  }catch(e){ prefBusy = false; return; }   // styleが未ロード → 次のstyledataで再試行
  prefBusy = false;
  paintPref(prefVisited);
  setPrefVisible(prefVisible);   // ロード待ちの間に来ていた表示要求を反映する
}
map.on('styledata', tryAddPrefLayer);

/* 統計カットから呼ぶ入口。GeoJSONの取得は1回だけ行い、以降は塗りの更新のみ。
   file:// のモックモードでは fetch が失敗するので握り潰す（コンソールを汚さない） */
function ensurePrefLayer(visited){
  prefVisited = visited || [];
  if(prefAdded){ paintPref(prefVisited); return; }
  if(prefData || prefLoading){ tryAddPrefLayer(); return; }
  prefLoading = true;
  fetch(PREF_GEOJSON).then(r=> r.ok ? r.json() : Promise.reject(0)).then(gj=>{
    prefData = gj; prefLoading = false;
    tryAddPrefLayer();
  }).catch(e=>{ prefLoading = false; });
}

function setPrefVisible(on){
  prefVisible = !!on;
  if(!prefAdded) return;
  const v = on ? 'visible' : 'none';
  try{
    map.setLayoutProperty('pref-fill','visibility', v);
    map.setLayoutProperty('pref-line','visibility', v);
  }catch(e){}
}
