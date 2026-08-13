/* Webスケジューラの品質優先度をNodeのvm上で検証する。DOM描画は対象外。 */
const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const math = Object.create(Math);
math.random = () => 0.5;
const storage = new Map();
const context = vm.createContext({
  console, Date, Math: math,
  PHOTOS: [], usingMock: false, idById: {},
  localStorage: {
    getItem: key => storage.get(key) || null,
    setItem: (key, value) => storage.set(key, value),
  },
});
vm.runInContext(fs.readFileSync('static/js/scheduler.js', 'utf8'), context);

const result = vm.runInContext(`(() => {
  const base = {id:'p', time:'', fetchedAt:'', fav:false};
  shown = {p: 9}; roundNo = 10;
  const neutral = photoPriority({...base});
  const recommended = photoPriority({...base, qualityKind:'similar', qualityRecommended:true});
  const alternate = photoPriority({...base, qualityKind:'similar', qualityRecommended:false});

  PHOTOS = [];
  for(let i=0;i<7;i++) PHOTOS.push({
    id:'p'+i, trip:0, time:'', fetchedAt:'', fav:false,
    qualityKind:i===0||i===6 ? 'similar' : '',
    qualityRecommended:i===0,
    qualityRole:i===0 ? 'recommended' : (i===6 ? 'alternate' : '')
  });
  idById = {}; PHOTOS.forEach((p,i)=>idById[p.id]=i);
  playQueue = ['p6']; shown = {}; roundNo = 0; knownIds = null;
  savedSig = photoSig(); savedQualitySig = '';
  const rebuilt = syncSchedule();
  return {neutral, recommended, alternate, rebuilt, queue:[...playQueue], qsig:qualitySig()};
})()`, context);

assert(result.recommended > result.neutral, '推奨は未評価より優先する');
assert(result.neutral > result.alternate, '類似alternateは未評価より減点する');
assert.strictEqual(result.rebuilt, true, '品質ロール初回適用時は待機キューを再評価する');
assert(!result.queue.includes('p6'), '7枚の同一tripではalternateが最初の6枚から外れる');
assert(result.queue.includes('p0'), 'recommendedは最初の6枚に含まれる');
assert.notStrictEqual(result.qsig, '0:0', '品質ロールをシグネチャへ含める');
console.log('scheduler quality: OK');
