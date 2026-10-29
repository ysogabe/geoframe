// admin.js の aiOpsCells（AI運用の STATUS 描画）を、最小の DOM の代用品で動かして確かめる。
// 使い方: node app/tests/admin_ai_ops_render.mjs <admin.js のパス>
// 合成の status JSON だけを使い、ネットワークには出ない。
import fs from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const path = process.argv[2];
assert.ok(path, 'admin.js のパスが要る');

function node(tag){
  return {
    tag, className: '', textContent: '', children: [], style: {}, dataset: {},
    classList: {toggle(){}, add(){}, remove(){}},
    appendChild(c){ this.children.push(c); return c; },
    addEventListener(){}, querySelector(){ return node('x'); },
  };
}
const document = {
  querySelector(){ return node('x'); },
  querySelectorAll(){ return []; },
  createElement: node,
  createTextNode(text){ return {tag: '#text', textContent: String(text), children: []}; },
  addEventListener(){},
};
const context = vm.createContext({
  document, fetch: () => new Promise(() => {}), setInterval(){}, setTimeout(){}, clearTimeout(){},
  console,
});
vm.runInContext(fs.readFileSync(path, 'utf8'), context, {filename: 'admin.js'});

const text = n => [n.textContent, ...n.children.map(text)].join(' ');
const render = d => vm.runInContext('aiOpsCells', context)(d).map(text).join('\n');

// 待ちが無い・キーが無い応答（Python 版と同じ）には何も描かない
assert.equal(render({}), '');
assert.equal(render({ai_waits: {items: []}}), '');

// 有効な待ち: 用途・範囲・固定の理由・UTC の期限
const out = render({ai_waits: {items: [
  {task: 'location_clue', scope: 'entrance', reason: 'leased', until: '2026-10-27T01:23:45Z'},
  {task: 'image_embedding', scope: 'service', reason: 'not_ready', until: '2026-10-27T02:00:00Z'},
]}});
assert.match(out, /AI一時待機中/);
assert.match(out, /location_clue（入口全体）: GPUのlease中 \/ 期限 2026-10-27 01:23:45 UTC/);
assert.match(out, /image_embedding（サービス単体）: サービスが未準備 \/ 期限 2026-10-27 02:00:00 UTC/);
assert.match(out, /期限が来れば自動で再開/);

// 設定が使えず結び直せない待ちは件数だけ
const unidentified = render({ai_waits: {items: [], unidentified: 3}});
assert.match(unidentified, /用途を特定できない待ち 3件/);
assert.match(unidentified, /3 件|3\s*件/);

// 想定外の値は生のまま描かず、期限は '--'
const odd = render({ai_waits: {items: [{task: 't', scope: 'x', reason: 'y', until: 'bad'}]}});
assert.match(odd, /期限 --/);

console.log('ok');
