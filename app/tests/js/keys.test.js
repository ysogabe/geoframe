const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const read = name => fs.readFileSync(`static/js/${name}`, 'utf8');
const MODES_SOURCE = read('modes.js');
const KEYMAP_SOURCE = read('keymap.js');
const KEYS_SOURCE = read('keys.js');
const BOOTSTRAP_SOURCE = fs.readFileSync('static/js/bootstrap.js', 'utf8');

const HOUR = 60 * 60 * 1000;
const CACHE_KEY = 'geoframe.keymap.v1';
const PIN_KEY = 'geoframe.local.pin.v1';
const SLEEP_KEY = 'geoframe.local.sleep.v1';

function makeTimers(clock) {
  let nextId = 1;
  const tasks = new Map();
  return {
    tasks,
    setTimeout(fn, delay) {
      const id = nextId++;
      tasks.set(id, { fn, delay, at: clock.now + Math.max(0, delay) });
      return id;
    },
    clearTimeout(id) { tasks.delete(id); },
    advance(ms) {
      clock.now += ms;
      let progressed = true;
      while (progressed) {
        progressed = false;
        const due = [...tasks.entries()]
          .filter(([, task]) => task.at <= clock.now)
          .sort((a, b) => a[1].at - b[1].at || a[0] - b[0]);
        if (!due.length) continue;
        for (const [id, task] of due) {
          if (!tasks.has(id)) continue;
          tasks.delete(id);
          task.fn();
          progressed = true;
        }
      }
    },
    run(delay) {
      const entry = [...tasks.entries()].find(([, task]) => task.delay === delay);
      assert.ok(entry, `timeout ${delay} が登録される`);
      const delta = Math.max(0, entry[1].at - clock.now);
      this.advance(delta);
    },
  };
}

class FakeClassList {
  constructor() { this.values = new Set(); }
  add(...names) { names.forEach(name => this.values.add(name)); }
  remove(...names) { names.forEach(name => this.values.delete(name)); }
  contains(name) { return this.values.has(name); }
  toggle(name, force) {
    const next = force === undefined ? !this.values.has(name) : !!force;
    if (next) this.values.add(name); else this.values.delete(name);
    return next;
  }
}

class FakeElement {
  constructor(tagName = 'div') {
    this.tagName = tagName.toUpperCase();
    this.nodeName = this.tagName;
    this.textContent = '';
    this.style = {};
    this.classList = new FakeClassList();
    this.listeners = {};
    this.parentElement = null;
    this.parentNode = null;
    this.isContentEditable = false;
    this.contentEditable = 'inherit';
  }
  addEventListener(type, fn) {
    (this.listeners[type] ||= []).push(fn);
  }
  appendChild(child) {
    child.parentElement = this;
    child.parentNode = this;
    return child;
  }
}

function validKeys(overrides = {}) {
  const keys = {};
  for (let i = 0; i < 10; i++) keys[String(i)] = i === 0 ? 'sleep' : i === 1 ? 'rotation' : 'reserved';
  return Object.assign(keys, overrides);
}

function validDocument(overrides = {}) {
  return Object.assign({
    rev: 4,
    updated_at: '2026-09-17T00:00:00Z',
    keys: validKeys(),
    modes: [
      { id: 'rotation', label: '地図と写真' },
      { id: 'sleep', label: '就寝' },
      { id: 'reserved', label: '未割当' },
    ],
  }, overrides);
}

function responseFor(data, { ok = true } = {}) {
  return { ok, json: async () => data };
}

function makeScenario({ search = '?keys=1', now = 1000000, api = validDocument(), fetchError = false,
                        storage = {} } = {}) {
  const clock = { now };
  const timers = makeTimers(clock);
  const storageValues = new Map(Object.entries(storage));
  const fetchCalls = [];
  const logs = { warn: [], error: [], info: [] };
  const elements = new Map();
  const modeEl = new FakeElement('span');
  const sleepEl = new FakeElement('div');
  const sleepLayer = new FakeElement('div');
  const toastEl = new FakeElement('div');
  elements.set('mode', modeEl);
  elements.set('sleep', sleepEl);
  elements.set('sleep-layer', sleepLayer);
  elements.set('key-toast', toastEl);
  const body = new FakeElement('body');
  const windowListeners = new Map();
  const window = {
    location: { search },
    GEOFRAME_MODE_3D: false,
    addEventListener(type, fn) {
      (windowListeners.get(type) || (windowListeners.set(type, []), windowListeners.get(type))).push(fn);
    },
    dispatchEvent(event) {
      for (const fn of windowListeners.get(event.type) || []) fn(event);
    },
  };
  const document = {
    readyState: 'complete',
    body,
    getElementById(id) { return elements.get(id) || null; },
    createElement(tag) { return new FakeElement(tag); },
  };
  const localStorage = {
    getItem(key) { return storageValues.has(key) ? storageValues.get(key) : null; },
    setItem(key, value) { storageValues.set(key, String(value)); },
    removeItem(key) { storageValues.delete(key); },
  };
  const sessionStorage = { getItem() { return null; }, removeItem() {} };
  const context = vm.createContext({
    console: {
      warn: (...args) => logs.warn.push(args.join(' ')),
      error: (...args) => logs.error.push(args.join(' ')),
      info: (...args) => logs.info.push(args.join(' ')),
    },
    Date: { now: () => clock.now },
    JSON,
    Number,
    Math,
    Object,
    Set,
    Map,
    Promise,
    URLSearchParams,
    window,
    location: window.location,
    document,
    localStorage,
    sessionStorage,
    fetch: (url, options) => {
      fetchCalls.push({ url, options });
      if (fetchError) return Promise.reject(new Error('offline'));
      return Promise.resolve(responseFor(api));
    },
    setTimeout: timers.setTimeout.bind(timers),
    clearTimeout: timers.clearTimeout.bind(timers),
  });
  vm.runInContext(BOOTSTRAP_SOURCE, context);
  vm.runInContext(MODES_SOURCE, context);
  vm.runInContext(KEYMAP_SOURCE, context);
  vm.runInContext(KEYS_SOURCE, context);

  function fireWindow(type, event) {
    for (const fn of windowListeners.get(type) || []) fn(event || { type });
  }
  function advance(ms) { timers.advance(ms); }
  function emit(code, extras = {}) {
    const event = Object.assign({
      code,
      repeat: false,
      ctrlKey: false,
      altKey: false,
      metaKey: false,
      shiftKey: false,
      isComposing: false,
      target: new FakeElement('div'),
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; },
    }, extras);
    for (const fn of windowListeners.get('keydown') || []) fn(event);
    return event;
  }
  async function start() {
    fireWindow('DOMContentLoaded');
    timers.advance(0);
    await Promise.resolve();
    await Promise.resolve();
    timers.advance(0);
    await Promise.resolve();
    return scenario;
  }
  const scenario = {
    context, clock, timers, storageValues, fetchCalls, logs, elements, windowListeners,
    window, document, modeEl, sleepEl, sleepLayer, toastEl,
    fireWindow, emit, advance, start,
    mode(expression) { return vm.runInContext(expression, context); },
  };
  return scenario;
}

function assertDefaultMap(scenario, message) {
  assert.strictEqual(JSON.stringify(scenario.mode('getKeymap().keys')),
    JSON.stringify(validKeys()), message);
}

async function sleepTwice(scenario, gap = 100) {
  scenario.emit('Digit0');
  scenario.advance(gap);
  scenario.emit('Digit0');
  await Promise.resolve();
}

(async () => {
  // 1. ?keys=1 が無い場合は起動取得もイベント登録も行わない。
  {
    const scenario = makeScenario({ search: '' });
    await scenario.start();
    assert.strictEqual(scenario.fetchCalls.length, 0, '?keys=1なしではキーマップを取得しない');
    assert.strictEqual((scenario.windowListeners.get('keydown') || []).length, 0,
      '?keys=1なしではkeydownを登録しない');
  }

  // 2a. URL gate: ?mode=3dだけでは完全に無効。
  {
    const scenario = makeScenario({ search: '?keys=1&mode=3d' });
    scenario.window.GEOFRAME_MODE_3D = false;
    await scenario.start();
    assert.strictEqual(scenario.fetchCalls.length, 0, 'mode=3dではキーマップを取得しない');
    assert.strictEqual((scenario.windowListeners.get('keydown') || []).length, 0, 'mode=3dではkeydownを登録しない');
  }
  // 2b. GEOFRAME_MODE_3Dだけでも完全に無効。
  {
    const scenario = makeScenario({ search: '?keys=1' });
    scenario.window.GEOFRAME_MODE_3D = true;
    await scenario.start();
    assert.strictEqual(scenario.fetchCalls.length, 0, 'GEOFRAME_MODE_3Dではキーマップを取得しない');
    assert.strictEqual((scenario.windowListeners.get('keydown') || []).length, 0, 'GEOFRAME_MODE_3Dではkeydownを登録しない');
  }

  // 3. Digit1はpin解除、Digit0の二度押しはlocal sleep、Digit2は6秒toast。
  {
    const scenario = makeScenario();
    await scenario.start();
    scenario.mode("claimMode('pin', {source:'local'})");
    const rotation = scenario.emit('Digit1');
    assert.strictEqual(rotation.defaultPrevented, true, 'rotation操作を消費する');
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), false, 'rotationでlocal pinを解除する');
    await sleepTwice(scenario);
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), true,
      'Digit0二度押しでlocal sleepをclaimする');
    assert.strictEqual(scenario.sleepLayer.classList.contains('on'), true, 'local sleep overlayを表示する');
    scenario.emit('Escape');
    const reserved = scenario.emit('Digit2');
    assert.strictEqual(reserved.defaultPrevented, true, 'reserved操作を消費する');
    assert.strictEqual(scenario.toastEl.textContent, 'このキーは未割当です', 'reservedの固定toast');
    scenario.advance(6000);
    assert.strictEqual(scenario.toastEl.textContent, '', 'reserved toastは6秒で消える');
  }

  // 4. Digit/Numpadのcodeだけを見て同じ操作として扱う。
  {
    const scenario = makeScenario();
    await scenario.start();
    scenario.mode("claimMode('pin', {source:'local'})");
    scenario.emit('Numpad1');
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), false, 'Numpad1はDigit1と同じ');
    scenario.emit('Numpad0');
    scenario.advance(100);
    scenario.emit('Numpad0');
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), true, 'Numpad0はDigit0と同じ');
  }

  // 5. repeat/修飾キー/編集要素/contenteditable/IMEを無視する。
  {
    const scenario = makeScenario();
    await scenario.start();
    const editable = new FakeElement('input');
    const contentEditable = new FakeElement('div');
    contentEditable.isContentEditable = true;
    const cases = [
      { repeat: true }, { ctrlKey: true }, { altKey: true }, { metaKey: true }, { shiftKey: true },
      { isComposing: true }, { target: editable }, { target: new FakeElement('textarea') },
      { target: new FakeElement('select') }, { target: contentEditable },
    ];
    for (const extra of cases) {
      const event = scenario.emit('Digit0', extra);
      assert.strictEqual(event.defaultPrevented, false, '無視対象のイベントを消費しない');
    }
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), false,
      '無視対象ではsleep stateを変えない');
  }

  // 6. double pressの境界: 1999ms成立、2001ms不成立。
  {
    const accepted = makeScenario();
    await accepted.start();
    accepted.emit('Digit0'); accepted.advance(1999); accepted.emit('Digit0');
    assert.strictEqual(accepted.mode("MODE_CLAIMS.sleep.claims.local.active"), true, '1999msは成立');
    assert.ok([...accepted.timers.tasks.values()].some(task => task.delay === 9 * HOUR),
      'local sleepの期限タイマーを9時間で登録する');
    accepted.advance(9 * HOUR + 1);
    assert.strictEqual(accepted.mode("MODE_CLAIMS.sleep.claims.local.active"), false,
      '期限タイマーでlocal sleepを自動解除する');

    const rejected = makeScenario();
    await rejected.start();
    rejected.emit('Digit0'); rejected.advance(2001); rejected.emit('Digit0');
    assert.strictEqual(rejected.mode("MODE_CLAIMS.sleep.claims.local.active"), false, '2001msは不成立');
  }

  // 7. local sleep中は任意のキー/Escapeで覚醒し、そのキーをモード選択に使わない。
  {
    const scenario = makeScenario();
    await scenario.start();
    await sleepTwice(scenario);
    scenario.mode("claimMode('pin', {source:'local'})");
    const wake = scenario.emit('Digit1');
    assert.strictEqual(wake.defaultPrevented, true, 'local sleep中の任意キーを消費する');
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), false, 'Digit1でlocal sleepを解除する');
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), true, '覚醒に使ったDigit1でpin選択しない');
    await sleepTwice(scenario);
    const arbitraryWake = scenario.emit('KeyA');
    assert.strictEqual(arbitraryWake.defaultPrevented, true, '数字以外の任意キーもlocal sleep中は消費する');
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), false, 'KeyAでlocal sleepを解除する');
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), true, 'KeyA覚醒でpin選択しない');
    await sleepTwice(scenario);
    scenario.emit('Escape');
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.local.active"), false, 'Escapeでもlocal sleepを解除する');
  }

  // 8. HA由来のsleepはキーで解除せずHA側sleep toastだけを出す。
  {
    const scenario = makeScenario();
    await scenario.start();
    scenario.mode("claimMode('sleep', {source:'ha', label:'SLEEP'})");
    const digit = scenario.emit('Digit1');
    const escape = scenario.emit('Escape');
    assert.strictEqual(digit.defaultPrevented, true, 'HA sleep中の数字キーを消費する');
    assert.strictEqual(escape.defaultPrevented, true, 'HA sleep中のEscapeを消費する');
    assert.strictEqual(scenario.mode("MODE_CLAIMS.sleep.claims.ha.active"), true, 'HA sleepを解除しない');
    assert.strictEqual(scenario.toastEl.textContent, 'HA側で消灯中です', 'HA sleepの固定toast');
  }

  // 9. APIの壊れた5種は丸ごと捨て、既定へfail closedする。
  for (const [name, api] of [
    ['キー欠け', (() => { const keys = validKeys(); delete keys['9']; return validDocument({ keys }); })()],
    ['未知ID', validDocument({ keys: validKeys({ '2': 'future-mode' }) })],
    ['非文字列', validDocument({ keys: validKeys({ '2': 3 }) })],
    ['rotation 0', validDocument({ keys: validKeys({ '1': 'reserved' }) })],
    ['sleep 2', validDocument({ keys: validKeys({ '2': 'sleep' }) })],
  ]) {
    const scenario = makeScenario({ api });
    await scenario.start();
    assertDefaultMap(scenario, `API ${name} は既定へ戻す`);
  }

  // 10. 壊れたJSON/未知ID/12時間超のcacheは破棄して既定へ戻す。
  {
    const cases = [
      ['壊れたJSON', '{'],
      ['未知ID', JSON.stringify({ cachedAt: 1000000, data: validDocument({ keys: validKeys({ '2': 'future' }) }) })],
      ['12時間超', JSON.stringify({ cachedAt: 1000000 - 12 * HOUR - 1, data: validDocument() })],
    ];
    for (const [name, raw] of cases) {
      const scenario = makeScenario({ api: null, fetchError: true, storage: { [CACHE_KEY]: raw } });
      await scenario.start();
      assertDefaultMap(scenario, `cache ${name} は既定へ戻す`);
      assert.strictEqual(scenario.storageValues.has(CACHE_KEY), false, `cache ${name} を削除する`);
    }
    const cached = validDocument({ rev: 8, keys: validKeys({ '0': 'reserved', '2': 'rotation' }) });
    const scenario = makeScenario({ api: null, fetchError: true, storage: {
      [CACHE_KEY]: JSON.stringify({ cachedAt: 1000000, data: cached }),
    } });
    await scenario.start();
    assert.strictEqual(JSON.stringify(scenario.mode('getKeymap().keys')),
      JSON.stringify(cached.keys), 'valid cacheはAPI失敗時に使う');
    scenario.mode("claimMode('pin', {source:'local'})");
    scenario.emit('Digit2');
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), false,
      'valid cacheのDigit2=rotationを実際のpin解除へ反映する');
  }

  // 11. clock skew guard: future since / actual NaN / until:0 / 巨大期間 / savedAtの大幅な戻りを破棄。
  {
    const scenario = makeScenario();
    await scenario.start();
    assert.strictEqual(scenario.mode("keyClaimRecordIsValid({mode:'PINNED', source:'local', since:999000, until:NaN, savedAt:999000}, 'pin', 1000000).valid"), false,
      '実際のNaN untilを破棄する');
    assert.strictEqual(scenario.mode("keyClaimRecordIsValid({mode:'PINNED', source:'local', since:999000, until:0, savedAt:999000}, 'pin', 1000000).valid"), false,
      'until:0を無期限として受理しない');
  }
  for (const [name, record] of [
    ['未来since', { mode: 'PINNED', source: 'local', since: 1000001, until: 1001000, savedAt: 1000000 }],
    ['NaN', { mode: 'PINNED', source: 'local', since: null, until: 1001000, savedAt: 1000000 }],
    ['巨大期間', { mode: 'PINNED', source: 'local', since: 1000000, until: 1000000 + 12 * HOUR + 1, savedAt: 1000000 }],
    ['savedAt逆行', { mode: 'PINNED', source: 'local', since: 999000, until: 1001000, savedAt: 1000000 + 5 * 60 * 1000 + 1 }],
  ]) {
    const scenario = makeScenario({ storage: { [PIN_KEY]: JSON.stringify(record) } });
    await scenario.start();
    assert.strictEqual(scenario.mode("modeClaimActive('pin')"), false, `時計スキュー ${name} でpinを破棄する`);
    assert.strictEqual(scenario.storageValues.has(PIN_KEY), false, `時計スキュー ${name} の保存値を削除する`);
    assert.strictEqual(scenario.logs.warn.length, 1, `時計スキュー ${name} はconsoleを1行だけ残す`);
  }

  // 12. keys.js評価時はwindow/location/GEOFRAME_MODE_3D/タイマーへ触れず、bootstrapのload後だけ初期化する。
  {
    const touches = { window: 0, timer: 0 };
    const watchedWindow = new Proxy({}, { get(target, property) {
      touches.window++;
      throw new Error(`top-level window access: ${String(property)}`);
    } });
    const topContext = vm.createContext({
      window: watchedWindow,
      setTimeout() { touches.timer++; throw new Error('top-level timer access'); },
      clearTimeout() { touches.timer++; throw new Error('top-level timer access'); },
    });
    vm.runInContext(KEYS_SOURCE, topContext);
    assert.deepStrictEqual(touches, { window: 0, timer: 0 },
      'keys.jsトップレベルはwindow/location/3D/タイマーへ触れない');
  }

  // 13. 実ソースにinnerHTMLを持ち込まない。
  assert.strictEqual(KEYMAP_SOURCE.includes('innerHTML'), false, 'keymap.jsはinnerHTML禁止');
  assert.strictEqual(KEYS_SOURCE.includes('innerHTML'), false, 'keys.jsはinnerHTML禁止');

  console.log('keys: 12 required cases: OK');
})().catch(error => {
  console.error(error && error.stack ? error.stack : error);
  process.exitCode = 1;
});
