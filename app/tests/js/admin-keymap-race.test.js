const fs = require('fs');
const vm = require('vm');
const assert = require('assert');

const nodes = new Map();
class Node {
  constructor(tag = 'div') {
    this.tagName = tag; this.children = []; this.value = 'reserved';
    this.disabled = false; this.textContent = ''; this.className = '';
    this.listeners = {}; this.options = [];
  }
  set id(value) { this._id = value; nodes.set('#' + value, this); }
  get id() { return this._id; }
  appendChild(node) { this.children.push(node); if (this.tagName === 'select') this.options.push(node); return node; }
  addEventListener(type, fn) { this.listeners[type] = fn; }
  remove() { if (this._id) nodes.delete('#' + this._id); }
  classList = {toggle() {}, contains() { return false; }};
}
function node(selector) {
  if (!nodes.has(selector)) { const n = new Node(); n.id = selector.slice(1); }
  return nodes.get(selector);
}
for (let i = 0; i < 10; i++) { const n = node('#key-' + i); n.value = i === 0 ? 'sleep' : i === 1 ? 'rotation' : 'reserved'; }
const pending = [];
const context = {
  console, JSON, Promise,
  document: {querySelector: node, querySelectorAll() { return []; }, createElement: tag => new Node(tag),
    createTextNode: text => ({textContent: text}), addEventListener() {}},
  fetch: (url, opts) => new Promise(resolve => pending.push({url, opts, resolve})),
  setTimeout: () => 0, clearTimeout() {}, setInterval() {}, confirm: () => true
};
vm.createContext(context);
let source = fs.readFileSync('static/js/admin.js', 'utf8').replace('\nloadStatus();\n', '\n');
source += `
globalThis.probe = {
  seed(d) { keymapDoc = d; keymapDirty = false; renderKeymapFields(d); },
  load: loadKeymap, save: saveKeymap,
  edit() { keymapDirty = true; keymapEditGeneration++; document.querySelector('#key-2').value = 'rotation'; },
  state() { return {rev: keymapDoc && keymapDoc.rev, dirty: keymapDirty,
    generation: keymapLoadGeneration, key2: document.querySelector('#key-2').value}; }
};`;
vm.runInContext(source, context);
const modes = [{id: 'rotation', label: 'r'}, {id: 'sleep', label: 's'}, {id: 'reserved', label: 'x'}];
const keys = {'0': 'sleep', '1': 'rotation', '2': 'reserved', '3': 'reserved', '4': 'reserved', '5': 'reserved', '6': 'reserved', '7': 'reserved', '8': 'reserved', '9': 'reserved'};
const response = (status, data) => ({ok: status >= 200 && status < 300, status, json: () => Promise.resolve(data)});
const flush = () => new Promise(resolve => setImmediate(resolve));

(async () => {
  context.probe.seed({rev: 1, updated_at: 'old', keys, modes});
  context.probe.load(); context.probe.edit(); context.probe.save();
  const get = pending.find(item => item.url === '/api/keymap');
  const post = pending.find(item => item.url === '/api/admin/setkeymap');
  post.resolve(response(200, {rev: 2, updated_at: 'new', keys: {...keys, '2': 'rotation'}, modes}));
  await flush(); await flush();
  const saved = context.probe.state();
  assert.strictEqual(saved.rev, 2); assert.strictEqual(saved.dirty, false);
  get.resolve(response(200, {rev: 1, updated_at: 'old', keys, modes}));
  await flush(); await flush();
  assert.deepStrictEqual(context.probe.state(), saved);

  pending.splice(0);
  context.probe.seed({rev: 1, updated_at: 'old', keys, modes});
  context.probe.save();
  const save = pending.find(item => item.url === '/api/admin/setkeymap');
  context.probe.edit();
  save.resolve(response(200, {rev: 2, updated_at: 'new', keys, modes}));
  await flush(); await flush();
  const edited = context.probe.state();
  assert.strictEqual(edited.dirty, true);
  assert.strictEqual(edited.key2, 'rotation');
  assert.strictEqual(node('#toast').textContent,
    '保存しました（送信後の編集は未保存のまま保持しています）');
  console.log('admin keymap race regressions: OK');
})().catch(error => { console.error(error); process.exitCode = 1; });
