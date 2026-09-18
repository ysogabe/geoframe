const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const source = fs.readFileSync('static/js/agents-mode.js', 'utf8');

class FakeElement {
  constructor(tag) {
    this.tag = tag;
    this.className = '';
    this.textContent = '';
    this.children = [];
    this.style = {};
    this.hidden = false;
  }
  appendChild(child) { this.children.push(child); return child; }
  get firstChild() { return this.children[0] || null; }
  removeChild(child) {
    this.children = this.children.filter(c => c !== child);
    return child;
  }
}

function cardFields(card) {
  const obj = {};
  for (const child of card.children) obj[child.className] = child.textContent;
  return obj;
}

function makeContext() {
  let visible = false;
  let nextTimer = 1;
  const intervals = new Map();
  const timeouts = new Map();
  const intervalDelays = new Map();
  const timeoutDelays = new Map();
  const clearedIntervals = [];
  const clearedTimeouts = [];
  const listeners = {};
  const layer = new FakeElement('div');
  const document = {
    getElementById(id) { return id === 'agentslayer' ? layer : null; },
    createElement(tag) { return new FakeElement(tag); },
  };
  const window = {
    addEventListener(type, fn) { (listeners[type] ||= []).push(fn); },
    dispatchEvent(event) { for (const fn of listeners[event.type] || []) fn(event); },
  };
  let fetchCalls = 0;
  let fetchImpl = () => Promise.resolve({ json: async () => ({ agents: [], rotation_allowed: true }) });
  const releaseCalls = [];
  let agentsActive = false;
  const context = vm.createContext({
    window, document, Date, Number, Promise,
    modeClaimIsTop() { return visible; },
    modeClaimActive(name) { return name === 'agents' && agentsActive; },
    keysReleaseMode(name, source) { releaseCalls.push({ name, source }); agentsActive = false; return true; },
    keysResolveMode() { return null; },
    fetch(...args) { fetchCalls++; return fetchImpl(...args); },
    setInterval(fn, delay) { const id = nextTimer++; intervals.set(id, fn); intervalDelays.set(id, delay); return id; },
    clearInterval(id) { intervals.delete(id); intervalDelays.delete(id); clearedIntervals.push(id); },
    setTimeout(fn, delay) { const id = nextTimer++; timeouts.set(id, fn); timeoutDelays.set(id, delay); return id; },
    clearTimeout(id) { timeouts.delete(id); timeoutDelays.delete(id); clearedTimeouts.push(id); }
  });
  return {
    context, layer, listeners,
    intervals, timeouts, intervalDelays, timeoutDelays, clearedIntervals, clearedTimeouts,
    setVisible(v) { visible = v; },
    setAgentsActive(v) { agentsActive = v; },
    releaseCalls,
    setFetchImpl(fn) { fetchImpl = fn; },
    getFetchCalls() { return fetchCalls; },
    fireModeChange() { window.dispatchEvent({ type: 'geoframe:mode-change' }); },
  };
}

async function flush() {
  await new Promise(resolve => setTimeout(resolve, 0));
  await new Promise(resolve => setTimeout(resolve, 0));
}

(async () => {
  // 1. 正常payloadでカードが2枚追加され、各フィールドがDOMに入る。
  {
    const env = makeContext();
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.context.agentsRender({
      rotation_allowed: true,
      agents: [
        { id: 'hermes', label: 'みなも (Hermes)', state: 'live', task_kind: 'coding', age_s: 3, metrics: [{ name: 'queue', value: 2 }] },
        { id: 'miach', label: 'miach-runtime', state: 'stale', task_kind: 'idle', age_s: 90, metrics: [{ name: 'active', value: 1 }] },
      ],
    });
    assert.strictEqual(env.layer.children.length, 3, 'title 1枚 + agent-card 2枚がlayerに追加される');
    assert.strictEqual(env.layer.children[0].className, 'agents-title');
    const cards = env.layer.children.slice(1);
    assert.deepStrictEqual(cards.map(c => c.className), ['agent-card', 'agent-card']);
    const first = cardFields(cards[0]);
    assert.strictEqual(first['agent-id'], 'ID: hermes');
    assert.strictEqual(first['agent-label'], 'みなも (Hermes)');
    assert.strictEqual(first['agent-state'], 'LIVE');
    assert.strictEqual(first['agent-task'], 'TASK: coding');
    assert.strictEqual(first['agent-age'], 'AGE: 3s');
    assert.strictEqual(first['agent-metrics'], 'METRICS: queue=2');
    const second = cardFields(cards[1]);
    assert.strictEqual(second['agent-id'], 'ID: miach');
    assert.strictEqual(second['agent-label'], 'miach-runtime');
    assert.strictEqual(second['agent-state'], 'STALE');
    assert.strictEqual(second['agent-task'], 'TASK: idle');
    assert.strictEqual(second['agent-age'], 'AGE: 90s');
    assert.strictEqual(second['agent-metrics'], 'METRICS: active=1');
    console.log('agents-mode: renders one card per agent with field text: OK');
  }

  // 2. 未知のstate/task_kind/labelは閉じた語彙の既定表示へ丸められる。
  {
    const env = makeContext();
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.context.agentsRender({
      rotation_allowed: true,
      agents: [{ id: 'rogue', label: 'HOST=/secret', state: 'rogue', task_kind: 'plotting', age_s: -1, metrics: [{ name: 'secret', value: 'token' }] }],
    });
    const card = cardFields(env.layer.children[1]);
    assert.strictEqual(card['agent-id'], 'ID: unknown', '未知idはunknownに丸める');
    assert.strictEqual(card['agent-label'], '不明', '未知文字列labelは不明に丸める');
    assert.strictEqual(card['agent-state'], 'UNKNOWN', '未知stateはunknownに丸める');
    assert.strictEqual(card['agent-task'], 'TASK: unknown', '未知task_kindはunknownに丸める');
    assert.strictEqual(card['agent-age'], 'AGE: ?s', '不正age_sは?に丸める');
    assert.strictEqual(card['agent-metrics'], 'METRICS: 不明', '未知metricsは不明に丸める');
    console.log('agents-mode: rounds unknown vocabulary to closed defaults: OK');
  }

  // 3. 表示中は5秒pollが継続し、interval callbackの実行のたびに描画が更新される。
  {
    const env = makeContext();
    let call = 0;
    env.setFetchImpl(() => {
      call++;
      const label = call === 1 ? 'みなも (Hermes)' : 'miach-runtime';
      return Promise.resolve({ json: async () => ({ agents: [{ id: 'hermes', label, state: 'live', task_kind: 'idle', age_s: 1 }], rotation_allowed: true }) });
    });
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.fireModeChange();
    await flush();
    assert.strictEqual(env.getFetchCalls(), 1, '表示直後に1回目のpollを行う');
    assert.strictEqual(cardFields(env.layer.children[1])['agent-label'], 'みなも (Hermes)');
    assert.strictEqual(env.intervals.size, 1, 'poll intervalを1つ登録する');
    assert.deepStrictEqual([...env.intervalDelays.values()], [5000], 'poll delayは5秒');
    const [pollFn] = env.intervals.values();
    pollFn();
    await flush();
    assert.strictEqual(env.getFetchCalls(), 2, 'interval callbackで再度pollする');
    assert.strictEqual(cardFields(env.layer.children[1])['agent-label'], 'miach-runtime', 'poll継続で描画が更新される');
    console.log('agents-mode: 5s poll continues and re-renders while visible: OK');
  }

  // 4. 非表示化でclearInterval/clearTimeoutされ、再表示で再登録される。
  {
    const env = makeContext();
    vm.runInContext(source, env.context);
    assert.strictEqual(env.intervals.size, 0, '非表示の初期状態ではpoll timerを登録しない');
    assert.strictEqual(env.timeouts.size, 0, '非表示の初期状態ではnudge timerを登録しない');
    env.setVisible(true);
    env.fireModeChange();
    await flush();
    assert.strictEqual(env.intervals.size, 1, '表示時にpoll intervalを登録する');
    assert.strictEqual(env.timeouts.size, 1, '表示時にnudge timeoutを登録する');
    assert.deepStrictEqual([...env.timeoutDelays.values()], [60000], 'nudge delayは60秒');
    env.setVisible(false);
    env.fireModeChange();
    assert.strictEqual(env.intervals.size, 0, '非表示化でpoll intervalを停止する');
    assert.strictEqual(env.timeouts.size, 0, '非表示化でnudge timeoutを停止する');
    assert.ok(env.clearedIntervals.length >= 1, 'clearIntervalを呼ぶ');
    assert.ok(env.clearedTimeouts.length >= 1, 'clearTimeoutを呼ぶ');
    env.setVisible(true);
    env.fireModeChange();
    await flush();
    assert.strictEqual(env.intervals.size, 1, '再表示でpoll intervalを再登録する');
    assert.strictEqual(env.timeouts.size, 1, '再表示でnudge timeoutを再登録する');
    console.log('agents-mode: timer lifecycle on visibility toggle: OK');
  }

  // 5. 片系欠落時は架空カードを作らず、実在する系だけを部分表示する。
  {
    const env = makeContext();
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.context.agentsRender({
      rotation_allowed: false,
      agents: [{ id: 'hermes', label: 'みなも (Hermes)', state: 'stale', task_kind: 'idle', age_s: 60 }],
    });
    assert.strictEqual(env.layer.children.length, 2, '片系のみでも欠落系の架空カードを作らず1枚だけ表示する');
    assert.strictEqual(cardFields(env.layer.children[1])['agent-state'], 'STALE');
    console.log('agents-mode: partial-source rendering (single agent) works: OK');
  }

  // 6. rotation_allowed:false（両系stale/unknown）で明示ピンを5分猶予後に解除する。
  {
    const env = makeContext();
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.setAgentsActive(true);
    env.context.agentsRender({
      rotation_allowed: false,
      agents: [
        { id: 'hermes', label: 'みなも (Hermes)', state: 'stale', task_kind: 'idle', age_s: 60 },
        { id: 'miach', label: 'miach-runtime', state: 'unknown', task_kind: 'idle', age_s: 121 },
      ],
    });
    assert.strictEqual(env.layer.children.length, 3, '両系STALE/unknownでも実在する2カードを表示する');
    assert.strictEqual(cardFields(env.layer.children[1])['agent-state'], 'STALE');
    assert.strictEqual(cardFields(env.layer.children[2])['agent-state'], 'UNKNOWN');
    assert.strictEqual(env.timeouts.size, 1, '両系STALE/unknownで5分猶予timeoutを登録する');
    assert.ok([...env.timeoutDelays.values()].includes(300000), '5分猶予delayは300秒');
    const graceId = [...env.timeoutDelays.entries()].find(([, delay]) => delay === 300000)[0];
    env.timeouts.get(graceId)();
    assert.deepStrictEqual(env.releaseCalls, [{ name: 'agents', source: 'local' }], '猶予経過後にagents claimを解除する');
    console.log('agents-mode: 5-minute grace releases explicit pin when both sources are stale: OK');
  }

  // 7. API失敗（fetch reject）でも未知/両系stale相当として5分猶予を開始する（rotation_allowedを握り潰さない）。
  {
    const env = makeContext();
    env.setFetchImpl(() => Promise.reject(new Error('network down')));
    vm.runInContext(source, env.context);
    env.setVisible(true);
    env.setAgentsActive(true);
    env.fireModeChange();
    await flush();
    // 表示中は60秒nudge timeoutが常に1本動く。ここでの2本目がAPI失敗経路で組まれた5分猶予timeout。
    assert.strictEqual(env.timeouts.size, 2, 'API失敗時もnudgeとは別に5分猶予timeoutを登録する（既存timerを消して終わりにしない）');
    console.log('agents-mode: fetch failure still arms the 5-minute grace instead of clearing it: OK');
  }

  assert.ok(!source.includes('innerHTML'), 'innerHTMLを使わない');
  console.log('agents-mode: all checks passed');
})().catch(err => {
  console.error(err);
  process.exit(1);
});
