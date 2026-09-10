/* 実アダプタと実コントローラを接続し、写真の有無・完了順・中断を検証する。 */
const assert = require('assert');
const fs = require('fs');
const vm = require('vm');
const adapterSource = fs.readFileSync('../frontend/photo-journey-lite.js','utf8')
  .replace(/^import\s+[^\n]*\n/gm,'').replace(/^export\s+/gm,'');
const location = { origin:'http://localhost', search:'' };
const settingsSource=fs.readFileSync('static/js/photo-journey-settings.js','utf8');
function actualSettings(search='',low=false){
 return vm.runInNewContext(settingsSource+';window.GEOFRAME_PHOTO_JOURNEY_SETTINGS',
  {window:{},DWELL_MS:16000,LOW_POWER:low,location:{search},URLSearchParams});
}
assert.strictEqual(actualSettings().terrainStyle,'hybrid');
assert.strictEqual(actualSettings('',true).terrainStyle,'cyber');
assert.strictEqual(actualSettings('?terrainStyle=cyber').terrainStyle,'cyber');
assert.strictEqual(actualSettings('?terrainStyle=photo',true).terrainStyle,'photo');
assert.strictEqual(actualSettings('?terrainStyle=unknown').terrainStyle,'hybrid');
assert.strictEqual(actualSettings().imageryMinDwellMs,55000,'暗い地形の精細化を待ってから写真を重ねる');
const api = vm.runInNewContext(adapterSource + `;({addPhotoJourneyLayers,
 updatePhotoJourneyBasemap,setPhotoJourneyImageryOpacity,getPhotoJourneyRevealTiles,
 getPhotoJourneyPrefetchTiles,selectPhotoJourneyBasemap,photoJourneyGroundLines,
 photoJourneyTerrainMesh,
 updatePhotoJourneyGroundLines,hidePhotoJourneyGroundLines,photoJourneyCameraPose,
 photoJourneyElevation,updatePhotoJourneyLighting})`, {
 console, location, JAPAN_GSI_ELEVATION_DECODER:()=>({}),
});
const point = {lon:139.7,lat:35.6,realElev:true,intel:{elev:50}};
function makeView(){
 const sources=[], layers=[];
 return { sources,layers,globe:{color:{clone(){return {setHex(){return this;}}}}},
  init:async()=>{},addLight(){},forceUpdate(){},setCamera(){},atmosphere:{},
  sampleTerrainHeight:()=>50,
  addSource(def){const handle={...def,updates:[],update(d){this.updates.push(d)},delete(){this.deleted=true}};
   sources.push(handle);return handle;},
  addLayer(def){const handle={...def,updates:[],update(d){this.updates.push(d)},delete(){this.deleted=true}};
   layers.push(handle);return handle;},attribution:{add(){}},
 };
}
const required=api.getPhotoJourneyRevealTiles(point,{origin:location.origin});
assert.strictEqual(required.length,9);
assert.strictEqual(new Set(required).size,9);
assert.ok(required.every(u=>u.includes('/gsi-photo/14/')&&u.endsWith('?detail=1')));
assert.strictEqual(api.getPhotoJourneyRevealTiles({lon:-74,lat:41}).length,0);
assert.strictEqual(api.getPhotoJourneyRevealTiles({lon:999,lat:35}).length,0);
assert.ok(api.getPhotoJourneyPrefetchTiles(point,{imagery:false}).every(u=>u.includes('/gsi-dem/')));
assert.ok(api.getPhotoJourneyPrefetchTiles(point,{detail:true}).filter(u=>u.includes('/gsi-photo/'))
 .every(u=>u.endsWith('?detail=1')));
const hybridView=makeView();
const hybrid=api.addPhotoJourneyLayers(hybridView,{points:[point],terrainStyle:'hybrid'});
assert.strictEqual(hybrid.basemap.source.minZoom,2,'親LODを飛ばして探索を増やさない');
assert.strictEqual(hybrid.basemap.source.maxZoom,18,'近景もGSIの詳細写真の提供範囲を維持する');
assert.ok(hybrid.basemap.source.url.endsWith('?detail=1'),'粗い写真は専用応答で除外する');
assert.strictEqual(hybrid.basemap.layer.raster.opacity,0);
const shade=hybridView.layers.find(l=>l.hillshade);
assert.strictEqual(shade.raster.show,true,'陰影スロットを合成対象に残す');
assert.strictEqual(shade.raster.opacity,0,'生のDEM色は出さない');
api.setPhotoJourneyImageryOpacity(hybrid.basemap,0.5);
assert.strictEqual(hybrid.basemap.layer.updates.at(-1).raster.opacity,0.5);
assert.strictEqual(hybrid.basemap.layer.updates.at(-1).source,hybrid.basemap.source,
 '設定を置換する更新でも写真ソースを保持する');
api.updatePhotoJourneyBasemap(hybrid.basemap,point);
assert.strictEqual(hybrid.basemap.opacity,0,'同じ国内でも新しい地点は暗い地形から開始');
const old=hybrid.basemap.source;
api.updatePhotoJourneyBasemap(hybrid.basemap,{lon:-74,lat:41});
assert.strictEqual(old.deleted,true,'海外では隠れたOSM画像を取得しない');
assert.strictEqual(hybrid.basemap.source,null);
api.updatePhotoJourneyBasemap(hybrid.basemap,point);
assert.ok(hybrid.basemap.source && hybrid.basemap.source!==old,'国内へ戻ると写真経路を再作成');
for(const options of [{terrainStyle:'cyber'},{terrainStyle:'hybrid',low:true}]){
 const v=makeView();const s=api.addPhotoJourneyLayers(v,{points:[point],...options});
 assert.strictEqual(s.basemap.source,null);
 assert.ok(!v.sources.some(s=>s.type==='raster-tile'));
}
const photo=api.addPhotoJourneyLayers(makeView(),{points:[point],terrainStyle:'photo'});
assert.strictEqual(photo.basemap.opacity,1);
assert.ok(!photo.basemap.source.url.includes('detail=1'));
const grid=api.photoJourneyGroundLines(makeView(),point);
assert.ok(grid.features.length>0 && grid.features.length<=12);
let vertexCount=0;
for(const line of grid.features) for(const p of line.geometry.coordinates[0]){
 vertexCount++;
 assert.ok(p.every(Number.isFinite));
 assert.strictEqual(p[2],62);
 assert.ok(Math.abs(p[0]-point.lon)<0.015 && Math.abs(p[1]-point.lat)<0.012);
}
assert.ok(vertexCount<=720,'局所線の頂点数を固定上限に抑える');
assert.strictEqual(api.photoJourneyGroundLines({sampleTerrainHeight:()=>undefined},point).features.length,0);
assert.strictEqual(api.photoJourneyGroundLines(makeView(),null).features.length,0);
const mesh=api.photoJourneyTerrainMesh(makeView(),point,{low:true});
assert.strictEqual(mesh.features.length,16,'cyber/Lowの局所メッシュは4x4セルに抑える');
assert.ok(mesh.features.every(feature => feature.properties.terrainMesh === true),
  '局所メッシュは識別用プロパティを持つ');
assert.ok(mesh.features.every(feature => feature.geometry.coordinates[0].every(p => p.length === 3 &&
  p[2] === 66)), '局所メッシュも標高付き頂点を使う');
assert.strictEqual(api.photoJourneyTerrainMesh({sampleTerrainHeight:()=>undefined},point,{low:true}).features.length,0,
  'DEM未到着時は局所メッシュを作らない');
const lines=hybrid.groundLines;
api.updatePhotoJourneyGroundLines(lines,point);
assert.strictEqual(lines.layer.polygon.perPositionHeight,true,'頂点の地形標高を保持する');
assert.strictEqual(lines.layer.polygon.clampToGround,false,'祖先タイルで線幅が拡大する焼き付けを使わない');
api.hidePhotoJourneyGroundLines(lines);
assert.strictEqual(lines.layer.updates.at(-1).polygon.show,false);
api.updatePhotoJourneyGroundLines(lines,point);
assert.strictEqual(lines.layer.updates.at(-1).polygon.perPositionHeight,true,'再表示でも標高を保持する');
assert.strictEqual(lines.layer.updates.at(-1).source,lines.source);
const cyber=api.addPhotoJourneyLayers(makeView(),{points:[point],terrainStyle:'cyber',low:true});
assert.strictEqual(cyber.groundLines.mesh,true,'cyberだけ局所メッシュを有効にする');
api.updatePhotoJourneyGroundLines(cyber.groundLines,point);
assert.ok(cyber.groundLines.meshLayer && cyber.groundLines.meshLayer.polygon.wireframe,
  '局所メッシュはwireframe Polygonレイヤーで描画する');
api.hidePhotoJourneyGroundLines(cyber.groundLines);
assert.strictEqual(cyber.groundLines.meshLayer.updates.at(-1).polygon.show,false,
  '局所メッシュも巡回停止時に隠す');

const controller=fs.readFileSync('static/js/photo-journey-lite.js','utf8')
 .replace("import('/static/navara/photo-journey-lite.js')",'Promise.resolve(fakeMod)');
async function scenario({content='imagery',reduced=false,low=false,style='hybrid',brokenBody=false,held=false}={}){
 let now=0,id=0,nextPhoto=0,releaseBody;
 const timers=new Map(),listeners=new Map(),requests=[],alphas=[],phases=[];
 const set=(fn,delay,repeat=false)=>{const key=++id;timers.set(key,{fn,at:now+delay,delay,repeat});return key;};
 const clear=key=>timers.delete(key);
 const body={classList:{toggle(){},remove(){}},dataset:new Proxy({}, {set(t,k,v){t[k]=v;if(k==='terrainPhase')phases.push(v);return true;}})};
 const v=makeView();
 const sourceAddLayer=v.addLayer.bind(v);
 v.addLayer=def=>{const l=sourceAddLayer(def), update=l.update.bind(l);
  l.update=d=>{update(d);if(l.source.type==='raster-tile' && d.raster)alphas.push(d.raster.opacity)};return l;};
 const fakeMod={...api,createPhotoJourneyView:()=>v,setPhotoJourneyCamera:(v,p)=>v.setCamera(p),
  flyPhotoJourneyCamera:async()=>true,addPhotoJourneyAircraftLayer:()=>({}),
  updatePhotoJourneyAircraftLayer(){},removePhotoJourneyAircraftLayer(){},
  advanceAircraft:a=>a,aircraftChasePose:a=>({lng:a.lon,lat:a.lat,height:2000,pitch:-20,heading:0}),
  aircraftApproachPose:a=>({lng:a.lon,lat:a.lat,height:2000,pitch:-20,heading:0})};
 const win={GEOFRAME_MODE_3D:true,LOW_POWER:low,
  GEOFRAME_PHOTO_JOURNEY_SETTINGS:{low,terrainStyle:style,dwellMs:90000,flyMs:1,
   height:1400,pitch:-24,headingStep:0,orbitDelayMs:45000,orbitMs:0,
   imageryFadeMs:1800,imageryMinDwellMs:30000},
  GEOFRAME_EVENT_STATE:{isSleeping:()=>false,isEventActive:()=>false,getEventUntil:()=>0},
  matchMedia:()=>({matches:reduced}),
  addEventListener:(name,fn)=>listeners.set(name,fn),removeEventListener:name=>listeners.delete(name),
  fetch:async(url,opts)=>{requests.push({url,opts,at:now});return {ok:true,
   headers:{get:key=>key==='X-Tile-Content'?(typeof content==='function'?content(url):content):'hit'},
   arrayBuffer:()=>{if(brokenBody)return Promise.reject(Error('truncated'));if(held && url.includes('/gsi-photo/'))
    return new Promise(resolve=>{releaseBody=()=>resolve(new ArrayBuffer(256));});return Promise.resolve(new ArrayBuffer(256));}};},
 };
 class ClockDate extends Date {static now(){return now;}}
 const ctx={console,window:win,document:{body,getElementById:()=>({})},Date:ClockDate,
  performance:{now:()=>now},location,fakeMod,manifestReady:Promise.resolve(),
  setTimeout:(fn,d)=>set(fn,d),clearTimeout:clear,setInterval:(fn,d)=>set(fn,d,true),clearInterval:clear,
  PHOTOS:[point,{...point,lon:135.5}],playQueue:[0],DWELL_MS:90000,LOW_POWER:low,
  reindexPhotos(){},loadPlayState(){},syncSchedule(){},buildPlayOrder(){},buildTicker(){},
  startLivePanels(){},nextQueueIdx:()=>nextPhoto,markShown(){},showPhoto(){},setMode(){}};
 vm.runInNewContext(controller,ctx);
 const flush=async()=>{for(let i=0;i<15;i++)await Promise.resolve();};
 await flush();
 async function advance(ms){const end=now+ms;let loops=0;
  while(true){const first=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];
   if(!first)break;assert.ok(++loops<10000,'timerが発散しない');const[key,t]=first;now=t.at;
   if(t.repeat)t.at+=t.delay;else timers.delete(key);t.fn();await flush();
  }now=end;await flush();}
 const dispatch=(name,detail)=>listeners.get(name)?.({detail});
 return {advance,flush,dispatch,requests,alphas,phases,timers,body,v,
  setNext:i=>{nextPhoto=i},release:()=>releaseBody?.()};
}
(async()=>{
 const normal=await scenario();
 await normal.advance(29999);assert.strictEqual(normal.body.dataset.terrainPhase,'cyber');
 await normal.advance(1001);assert.strictEqual(normal.body.dataset.terrainPhase,'revealing');
 assert.ok(normal.alphas.some(a=>a>0&&a<1),'実画像受信後に中間不透明度を描く');
 await normal.advance(1000);assert.strictEqual(normal.body.dataset.terrainPhase,'photo');
 assert.strictEqual(normal.alphas.at(-1),1);
 assert.ok(normal.requests.filter(r=>r.url.includes('/14/')&&r.url.includes('gsi-photo'))
  .every(r=>r.opts.cache==='no-cache'),'判定用タイルは古いヘッダを再検証する');
 normal.dispatch('pagehide');assert.strictEqual(normal.timers.size,0,'終了時にfade/guide/先読みを全停止');
 for(const opts of [{content:'empty'},{content:null},{brokenBody:true},{content:u=>u===required[0]?'empty':'imagery'}]){
  const s=await scenario(opts);await s.advance(85000);
  assert.strictEqual(s.body.dataset.terrainPhase,'cyber','欠損・旧サーバー・受信失敗・一部欠損では写真にしない');
  assert.ok(s.requests.length<=60,'取得上限を守る');s.dispatch('pagehide');
 }
 const reduced=await scenario({reduced:true});await reduced.advance(32000);
 assert.strictEqual(reduced.body.dataset.terrainPhase,'photo');
 assert.ok(!reduced.alphas.some(a=>a>0&&a<1),'動きを減らす設定はフェードなし');reduced.dispatch('pagehide');
 const paused=await scenario();await paused.advance(30800);
 paused.dispatch('geoframe:presence-state',{sleeping:true});
 assert.strictEqual(paused.alphas.at(-1),0);const n=paused.alphas.length;
 await paused.advance(5000);assert.strictEqual(paused.alphas.length,n,'スリープ後に古いfadeが走らない');
 paused.dispatch('pagehide');assert.strictEqual(paused.timers.size,0);
 const alert=await scenario();await alert.advance(30800);
 alert.dispatch('geoframe:aircraft-alert',{aircraft:{hex:'test',lon:140,lat:36},until:60000,kind:'rare'});
 assert.strictEqual(alert.alphas.at(-1),0);await alert.advance(1000);
 assert.ok(!alert.phases.slice(alert.phases.lastIndexOf('cyber')+1).includes('photo'));
 alert.dispatch('pagehide');
 const stale=await scenario({held:true});await stale.advance(16000);
 stale.dispatch('geoframe:presence-state',{sleeping:true});stale.setNext(1);stale.release();await stale.flush();
 stale.dispatch('geoframe:presence-state',{sleeping:false});await stale.advance(32000);
 assert.strictEqual(stale.body.dataset.terrainPhase,'cyber','前地点の遅い応答で次地点の写真を表示しない');
 stale.dispatch('pagehide');
 const low=await scenario({low:true,style:'cyber'});await low.advance(85000);
 assert.ok(low.requests.every(r=>r.url.includes('/gsi-dem/')),'省電力サイバーは写真を取得しない');
 low.dispatch('pagehide');
 console.log('photo-journey-reveal: OK');
})().catch(e=>{console.error(e);process.exitCode=1});
