const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const read = name => fs.readFileSync(`static/js/${name}`, 'utf8');

function makeTimers() {
  let next = 1;
  const timeouts = new Map();
  const intervals = new Map();
  return {
    timeouts, intervals,
    setTimeout(fn, delay) { const id = next++; timeouts.set(id, { fn, delay }); return id; },
    clearTimeout(id) { timeouts.delete(id); },
    setInterval(fn, delay) { const id = next++; intervals.set(id, { fn, delay }); return id; },
    clearInterval(id) { intervals.delete(id); },
    runTimeout(delay) {
      const entry = [...timeouts.entries()].find(([, task]) => task.delay === delay);
      assert.ok(entry, `timeout ${delay} が登録される`);
      timeouts.delete(entry[0]); entry[1].fn();
    },
    runInterval(delay) {
      const entry = [...intervals.entries()].find(([, task]) => task.delay === delay);
      assert.ok(entry, `interval ${delay} が登録される`);
      entry[1].fn();
    },
  };
}

function loadModes({ now = 1000 } = {}) {
  const clock = { now };
  const el = { textContent: '' };
  const listeners = {};
  const context = vm.createContext({
    console, Date: { now: () => clock.now },
    document: { getElementById: id => id === 'mode' ? el : null },
    window: { addEventListener: (name, fn) => { listeners[name] = fn; } },
  });
  vm.runInContext(read('modes.js'), context);
  return { context, el, listeners, clock };
}

const modes = loadModes();
const call = expression => vm.runInContext(expression, modes.context);

// Keep pickMode's five public branches covered through the real VM module.
for (const [state, expected] of [
  [{ sleep: true }, 'SLEEP'], [{ alert: true }, 'ALERT'],
  [{ pinned: true }, 'PINNED'], [{ ambient: true }, 'AMBIENT'], [{}, 'ROTATION'],
]) assert.strictEqual(call(`pickMode(${JSON.stringify(state)})`), expected);

// setMode('SLEEP') is only reachable from panels.js's HA presence path (see
// the real-path test below), so the compat shim must register it as an HA
// claim.  A real HA wake (presence-state sleeping:false) then clears it.
call("setMode('SLEEP')");
assert.strictEqual(call('resolveMode()'), 'SLEEP');
modes.listeners['geoframe:presence-state']({ detail: { sleeping: false } });
assert.strictEqual(call('resolveMode()'), 'ROTATION', 'HA復帰でsetMode(SLEEP)由来のha claimが解除される');

// A local sleep claim (future key-triggered local sleep, W3) is independent
// of HA and must survive presence-state wake events.
call("claimSleep('local')");
assert.strictEqual(call('resolveMode()'), 'SLEEP');
modes.listeners['geoframe:presence-state']({ detail: { sleeping: true } });
assert.strictEqual(call('resolveMode()'), 'SLEEP');
modes.listeners['geoframe:presence-state']({ detail: { sleeping: false } });
assert.strictEqual(call('resolveMode()'), 'SLEEP', 'ローカル就寝はHA復帰で解除されない');
call("releaseMode('sleep', 'local')");
assert.strictEqual(call('resolveMode()'), 'ROTATION');

// Load scheduler.js in the same VM as modes.js and exercise the actual goTo timeout.
{
  const timers = makeTimers();
  let shown = 0;
  const schedulerContext = vm.createContext({
    console, Date: { now: () => 1000 }, setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout, FLY_MS: 1000, LOW_POWER: false,
    PHOTOS: [{ lon: 139, lat: 35, title: 'one' }], playQueue: [], idById: {},
    markerEl: { style: {} }, activeLngLat: null, map: { flyTo: () => {} },
    document: { getElementById: () => ({ classList: { add: () => {} } }) },
    Image: function Image() {}, clearScatter: () => {}, clearHazard: () => {},
    setPrefVisible: () => {}, armBase: () => {}, preloadNext: () => {},
    setMode: vm.runInContext('setMode', modes.context),
    resolveMode: vm.runInContext('resolveMode', modes.context),
    modeClaimActive: vm.runInContext('modeClaimActive', modes.context),
    modeClaimIsTop: vm.runInContext('modeClaimIsTop', modes.context),
    showPhoto: () => { shown++; },
  });
  vm.runInContext(read('scheduler.js'), schedulerContext);
  vm.runInContext("setMode('PINNED'); goTo(0)", schedulerContext);
  timers.runTimeout(820);
  assert.strictEqual(shown, 0, 'ピン後のgoTo遅延でshowPhotoを呼ばない');
  assert.strictEqual(vm.runInContext('resolveMode()', schedulerContext), 'PINNED');
}

// Exercise the real main.js interval with the same claim API.
{
  const timers = makeTimers();
  let shown = 0;
  const mainContext = vm.createContext({
    console, Date: { now: () => 1000 }, setTimeout: timers.setTimeout,
    clearTimeout: timers.clearTimeout, setInterval: timers.setInterval,
    clearInterval: timers.clearInterval, window: { GEOFRAME_MODE_3D: false },
    map: { on: () => {}, getZoom: () => 10, jumpTo: () => {}, getCanvas: () => ({ addEventListener: () => {} }) },
    tryAddLayers: () => {},
    manifestReady: { then: fn => fn() }, usingMock: true, layersAdded: false,
    PHOTOS: [{ lon: 139, lat: 35, trip: 1 }], playQueue: [0], idx: 0,
    DWELL_MS: 1000, STATS_EVERY: 99, MANIFEST_FAIL_MS: 999999,
    LOW_POWER: false, activeLngLat: null, INTERLUDE_KINDS: [],
    reindexPhotos: () => {}, loadPlayState: () => {}, syncSchedule: () => {},
    buildPlayOrder: () => {}, nextQueueIdx: () => -1, markShown: () => {},
    showPhoto: () => { shown++; }, syncMarker: () => {}, armBase: () => {},
    buildTicker: () => {}, loadStats: () => {}, loadOnThisDay: () => {},
    startLivePanels: () => {}, startWatchdogs: () => {}, showInterlude: () => false,
    showTripIntro: () => false, location: { reload: () => {} }, fetch: async () => ({ ok: false }),
    setMode: vm.runInContext('setMode', modes.context),
    resolveMode: vm.runInContext('resolveMode', modes.context),
    modeClaimActive: vm.runInContext('modeClaimActive', modes.context),
    modeClaimIsTop: vm.runInContext('modeClaimIsTop', modes.context),
  });
  vm.runInContext(read('main.js'), mainContext);
  vm.runInContext("setMode('PINNED'); start()", mainContext);
  shown = 0;
  timers.runInterval(1000);
  assert.strictEqual(shown, 0, 'ピン後のmain intervalでshowPhotoを呼ばない');
  assert.strictEqual(vm.runInContext('resolveMode()', mainContext), 'PINNED');
}

// Releasing ALERT through the real events.js path exposes an active pin.
{
  const bar = { style: {}, className: '', textContent: '' };
  const eventContext = vm.createContext({
    console, Date: { now: () => 1000 }, Number, Math,
    LOW_POWER: false, EVENT_MS: 20000,
    setTimeout: () => 1, clearTimeout: () => {},
    window: { GEOFRAME_MODE_3D: false, dispatchEvent: () => {}, CustomEvent: class {
      constructor(type, init) { this.type = type; this.detail = init.detail; }
    } },
    map: { on: () => {}, flyTo: () => {} },
    document: { getElementById: id => id === 'alertbar' ? bar : null },
    armBase: () => {}, clearScatter: () => {}, activeLngLat: null,
    eventCard: () => {}, setMode: vm.runInContext('setMode', modes.context),
    releaseMode: vm.runInContext('releaseMode', modes.context),
    resolveMode: vm.runInContext('resolveMode', modes.context),
    fetch: async () => ({ ok: false }),
  });
  vm.runInContext(read('events.js'), eventContext);
  call("setMode('PINNED')");
  assert.strictEqual(vm.runInContext("triggerEvent({ hex: 'alert', lat: 35, lon: 139, rare: true })", eventContext), true);
  assert.strictEqual(vm.runInContext("finishAircraftAlert('ended')", eventContext), true);
  assert.strictEqual(call('resolveMode()'), 'PINNED');
}

// A render-blocked pin resumes when allowed.
call("setMode('PHOTO JOURNEY'); claimMode('pin', { canRender: false })");
assert.strictEqual(call('resolveMode()'), 'PHOTO JOURNEY');
call("claimMode('pin', { canRender: true })");
assert.strictEqual(call('resolveMode()'), 'PINNED');

// The real ALERT label (INTERCEPT etc., not the literal string 'ALERT') must
// still block goTo's delayed callback from calling showPhoto.  This is the
// exact repro from the round-2 review: modeClaimIsTop('ambient') used to
// compare the resolved label against the literal strings SLEEP/ALERT/PINNED,
// which never matches a real alert label.
{
  const modes2 = loadModes();
  const timers = makeTimers();
  let shown = 0;
  const bar = { style: {}, className: '', textContent: '' };
  const sharedContext = vm.createContext({
    console, Date: { now: () => 1000 }, Number, Math,
    setTimeout: timers.setTimeout, clearTimeout: timers.clearTimeout,
    FLY_MS: 1000, LOW_POWER: false, EVENT_MS: 20000,
    PHOTOS: [{ lon: 139, lat: 35, title: 'one' }], playQueue: [], idById: {},
    markerEl: { style: {} }, activeLngLat: null,
    map: { flyTo: () => {}, on: () => {} },
    document: { getElementById: id => id === 'alertbar' ? bar : { classList: { add: () => {} } } },
    Image: function Image() {}, clearScatter: () => {}, clearHazard: () => {},
    setPrefVisible: () => {}, armBase: () => {}, preloadNext: () => {},
    eventCard: () => {},
    window: { GEOFRAME_MODE_3D: false, dispatchEvent: () => {}, CustomEvent: class {
      constructor(type, init) { this.type = type; this.detail = init && init.detail; }
    } },
    fetch: async () => ({ ok: false }),
    setMode: vm.runInContext('setMode', modes2.context),
    resolveMode: vm.runInContext('resolveMode', modes2.context),
    modeClaimActive: vm.runInContext('modeClaimActive', modes2.context),
    modeClaimIsTop: vm.runInContext('modeClaimIsTop', modes2.context),
    releaseMode: vm.runInContext('releaseMode', modes2.context),
    showPhoto: () => { shown++; },
  });
  vm.runInContext(read('scheduler.js'), sharedContext);
  vm.runInContext(read('events.js'), sharedContext);

  vm.runInContext('goTo(0)', sharedContext);
  assert.strictEqual(
    vm.runInContext("triggerEvent({ hex: 'rt', lat: 35, lon: 139, rare: true })", sharedContext),
    true,
  );
  assert.strictEqual(vm.runInContext('resolveMode()', modes2.context), 'INTERCEPT',
    '実際のALERTラベルはINTERCEPT(文字列ALERTではない)');
  timers.runTimeout(820);
  assert.strictEqual(shown, 0, 'ALERT中(実ラベルINTERCEPT)はgoTo遅延でshowPhotoを呼ばない');
  assert.strictEqual(vm.runInContext('resolveMode()', modes2.context), 'INTERCEPT',
    'ALERT中はgoTo遅延の発火後もモードがINTERCEPTのまま');

  // No regression: once the alert really ends, the ordinary rotation resumes.
  vm.runInContext("finishAircraftAlert('ended')", sharedContext);
  vm.runInContext('goTo(0)', sharedContext);
  timers.runTimeout(820);
  assert.strictEqual(shown, 1, 'ALERT解除後は通常巡回でshowPhotoが走る(退行なし)');
}

// pin claim expiry: an active claim past its `until` releases itself and
// falls back to ordinary rotation.
{
  const m = loadModes({ now: 1000 });
  const c = expr => vm.runInContext(expr, m.context);
  c("claimMode('pin', { label: 'PINNED', until: 1500 })");
  assert.strictEqual(c('resolveMode()'), 'PINNED');
  m.clock.now = 1600;
  assert.strictEqual(c('resolveMode()'), 'ROTATION', 'pinのuntilが過ぎたら通常巡回へ戻る');
}

// canRender=false skip/recovery, driven through the real goTo delayed
// callback (not a direct resolveMode() call).  A render-blocked pin defers
// to ambient; once it can render again, it reclaims the display on the next
// delayed callback.
{
  const modes3 = loadModes();
  const timers3 = makeTimers();
  let shown3 = 0;
  const schedulerContext3 = vm.createContext({
    console, Date: { now: () => 1000 }, setTimeout: timers3.setTimeout,
    clearTimeout: timers3.clearTimeout, FLY_MS: 1000, LOW_POWER: false,
    PHOTOS: [{ lon: 139, lat: 35, title: 'one' }], playQueue: [], idById: {},
    markerEl: { style: {} }, activeLngLat: null, map: { flyTo: () => {} },
    document: { getElementById: () => ({ classList: { add: () => {} } }) },
    Image: function Image() {}, clearScatter: () => {}, clearHazard: () => {},
    setPrefVisible: () => {}, armBase: () => {}, preloadNext: () => {},
    setMode: vm.runInContext('setMode', modes3.context),
    resolveMode: vm.runInContext('resolveMode', modes3.context),
    modeClaimActive: vm.runInContext('modeClaimActive', modes3.context),
    modeClaimIsTop: vm.runInContext('modeClaimIsTop', modes3.context),
    showPhoto: () => { shown3++; },
  });
  vm.runInContext(read('scheduler.js'), schedulerContext3);

  vm.runInContext("claimMode('pin', { canRender: false })", modes3.context);
  vm.runInContext('goTo(0)', schedulerContext3);
  timers3.runTimeout(820);
  assert.strictEqual(shown3, 1, 'canRender=falseのpinはambientを妨げずgoTo遅延で通常表示する');
  assert.strictEqual(vm.runInContext('resolveMode()', modes3.context), 'PHOTO JOURNEY');

  vm.runInContext("claimMode('pin', { canRender: true })", modes3.context);
  vm.runInContext('goTo(0)', schedulerContext3);
  timers3.runTimeout(820);
  assert.strictEqual(shown3, 1, 'canRenderが戻ったpinはgoTo遅延でPHOTO JOURNEYへの復帰をスキップする');
  assert.strictEqual(vm.runInContext('resolveMode()', modes3.context), 'PINNED');
}

// panels.js's real presence path (pollPresence, fetch-stubbed) drives the
// setMode('SLEEP') call that issue 1 is about.  This is the round-2 review's
// exact repro: screen off then on twice through the real function.
async function runPresenceRegression() {
  const listeners = {};
  const sharedWindow = {
    addEventListener: (name, fn) => { listeners[name] = fn; },
    dispatchEvent: (ev) => { if (listeners[ev.type]) listeners[ev.type](ev); },
    CustomEvent: class { constructor(type, init) { this.type = type; this.detail = init && init.detail; } },
    GEOFRAME_MODE_3D: false,
  };
  const modeEl = { textContent: '' };
  const sleepEl = { classList: { toggle: () => {} } };
  const generic = () => ({ classList: { add(){}, remove(){}, toggle(){} }, style: {}, textContent: '', innerHTML: '' });
  let fetchPayload = { screen: 'off' };
  const presenceContext = vm.createContext({
    console, Date,
    document: {
      getElementById: id => id === 'mode' ? modeEl : id === 'sleep' ? sleepEl : generic(),
      body: { classList: { toggle: () => {} } },
    },
    window: sharedWindow,
    setInterval: () => 1, setTimeout: () => 1, clearTimeout: () => {},
    NIGHT_FROM_H: 22, NIGHT_TO_H: 6,
    fetch: async () => ({ ok: true, json: async () => fetchPayload }),
  });
  vm.runInContext(read('modes.js'), presenceContext);
  vm.runInContext(read('panels.js'), presenceContext);

  await vm.runInContext('pollPresence()', presenceContext);
  assert.strictEqual(modeEl.textContent, 'MODE: SLEEP', 'HA screen=offの実経路でSLEEPになる');
  fetchPayload = { screen: 'on' };
  await vm.runInContext('pollPresence()', presenceContext);
  assert.strictEqual(modeEl.textContent, 'MODE: PHOTO JOURNEY',
    'HA screen=onの実経路でPHOTO JOURNEYへ戻る(HA由来の就寝が永久に残らない)');

  // Regression guarded by the previous round: a local sleep claim survives
  // even this real presence-path wake.
  vm.runInContext("claimSleep('local')", presenceContext);
  fetchPayload = { screen: 'off' };
  await vm.runInContext('pollPresence()', presenceContext);
  fetchPayload = { screen: 'on' };
  await vm.runInContext('pollPresence()', presenceContext);
  assert.strictEqual(modeEl.textContent, 'MODE: SLEEP',
    'ローカル就寝が有効な間はpollPresenceの実経路でもHA復帰でSLEEPのまま');
  vm.runInContext("releaseMode('sleep', 'local')", presenceContext);
}

runPresenceRegression()
  .then(() => console.log('modes-arbitration: OK'))
  .catch(error => {
    console.error(error && error.stack ? error.stack : error);
    process.exitCode = 1;
  });
