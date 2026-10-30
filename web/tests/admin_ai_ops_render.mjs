// admin.js の aiOpsCells（AI運用の STATUS 描画）を、最小の DOM の代用品で動かして確かめる。
// 使い方: node web/tests/admin_ai_ops_render.mjs <admin.js のパス>
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

// 停止にかかる用途: 用途名・範囲・理由の日本語・待ちの件数だけ。無ければ描かない
const blocks = render({ai_task_blocks: [
  {task: 'photo_judge', scope: 'provider', reason: 'forbidden', waiting: 2},
  {task: 'location_suggest', scope: 'provider', reason: 'forbidden', waiting: 3},
  {task: 'location_clue', scope: 'task', reason: 'model_not_found', waiting: 0},
]});
assert.match(blocks, /AI送信が止まっている用途/);
assert.match(blocks, /photo_judge: Provider全体の停止（権限エラー（403））で 2 件が待ち/);
assert.match(blocks, /location_suggest: Provider全体の停止（権限エラー（403））で 3 件が待ち/);
assert.match(blocks, /location_clue: 用途の停止（モデルが無い（404））で 0 件が待ち/);
assert.equal(render({ai_task_blocks: []}), '');
const oddBlock = render({ai_task_blocks: [{task: 't', scope: 'x', reason: 'y', waiting: 'bad'}]});
assert.match(oddBlock, /t: 用途の停止（y）で 0 件が待ち/);

// 件数を集計できなかったとき（null）は 0 件と区別して「件数不明」。停止の情報は残る
const unknownBlock = render({ai_task_blocks: [{task: 'photo_judge', scope: 'provider', reason: 'forbidden', waiting: null}]});
assert.match(unknownBlock, /photo_judge: Provider全体の停止（権限エラー（403））で 待ちは件数不明/);
assert.doesNotMatch(unknownBlock, /0 件/);

console.log('ok');
