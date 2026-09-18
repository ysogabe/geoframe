/* ============================================================
   modes.js — 表示モードラベルの一元化（#mode 要素の唯一の writer）
   ============================================================ */
const MODE_EL_ID = 'mode';
const MODE_PRIORITY = ['SLEEP', 'ALERT', 'PINNED', 'AGENTS', 'AMBIENT', 'ROTATION'];

/* Claim state is deliberately inert at load time.  Browser globals, storage,
   timers and event listeners are acquired only by initModes(). */
const MODE_CLAIMS = {
  pin: { active: false, label: 'PINNED', until: 0, source: 'local', canRender: true },
  alert: { active: false, label: 'ALERT', until: 0, source: 'event', canRender: true },
  sleep: {
    active: false, label: 'SLEEP', until: 0, source: 'local', canRender: true,
    claims: {
      local: { active: false, label: 'SLEEP', until: 0, canRender: true },
      ha: { active: false, label: 'SLEEP', until: 0, canRender: true },
    },
  },
  ambient: { active: false, label: 'AMBIENT', until: 0, source: 'local', canRender: true },
  agents: { active: false, label: 'AGENTS', until: 0, source: 'local', canRender: true },
};
let modesInitialized = false;
let lastModeEventKey = null;

function pickMode(state) {
  const s = state || {};
  if (s.sleep) return 'SLEEP';
  if (s.alert) return 'ALERT';
  if (s.pinned) return 'PINNED';
  if (s.agents) return 'AGENTS';
  if (s.ambient) return 'AMBIENT';
  return 'ROTATION';
}

function initModes() {
  if (modesInitialized) return true;
  modesInitialized = true;
  if (typeof window === 'undefined') return false;
  if (typeof window.addEventListener === 'function') {
    window.addEventListener('geoframe:presence-state', event => {
      const sleeping = !!(event && event.detail && event.detail.sleeping);
      if (sleeping) claimMode('sleep', { source: 'ha', label: 'SLEEP' });
      else releaseMode('sleep', 'ha');
      resolveMode();
    });
  }
  return true;
}

function claimMode(name, options, legacyOptions) {
  const claim = MODE_CLAIMS[name];
  if (!claim) return false;
  const opts = typeof options === 'string'
    ? Object.assign({ label: options }, legacyOptions || {})
    : (options || {});
  if (name === 'sleep') {
    const source = opts.source === 'ha' ? 'ha' : 'local';
    const owned = claim.claims[source];
    owned.active = true;
    if (Object.prototype.hasOwnProperty.call(opts, 'label')) owned.label = opts.label;
    if (Object.prototype.hasOwnProperty.call(opts, 'until')) owned.until = Number(opts.until) || 0;
    if (Object.prototype.hasOwnProperty.call(opts, 'canRender')) owned.canRender = opts.canRender;
    claim.source = source;
    return true;
  }
  claim.active = true;
  if (Object.prototype.hasOwnProperty.call(opts, 'label')) claim.label = opts.label;
  if (Object.prototype.hasOwnProperty.call(opts, 'until')) claim.until = Number(opts.until) || 0;
  if (Object.prototype.hasOwnProperty.call(opts, 'source')) claim.source = opts.source;
  if (Object.prototype.hasOwnProperty.call(opts, 'canRender')) claim.canRender = opts.canRender;
  return true;
}

function releaseMode(name, source) {
  const claim = MODE_CLAIMS[name];
  if (!claim) return false;
  if (name === 'sleep') {
    if (source === 'ha' || source === 'local') {
      claim.claims[source].active = false;
      claim.claims[source].until = 0;
    } else {
      Object.values(claim.claims).forEach(owned => { owned.active = false; owned.until = 0; });
    }
  } else {
    if (source && claim.source !== source) return false;
    claim.active = false;
    claim.until = 0;
  }
  resolveMode();
  return true;
}

function activeClaim(claim) {
  if (!claim.active) return false;
  if (claim.until > 0 && Date.now() >= claim.until) {
    claim.active = false;
    claim.until = 0;
    return false;
  }
  if (typeof claim.canRender === 'function' && !claim.canRender()) return false;
  return claim.canRender !== false;
}

function modeClaimActive(name) {
  const claim = MODE_CLAIMS[name];
  if (!claim) return false;
  if (name === 'sleep') return Object.values(claim.claims).some(activeClaim);
  return activeClaim(claim);
}

function collectClaimState() {
  const state = {};
  for (const name of ['sleep', 'alert', 'pin', 'agents', 'ambient']) {
    state[name === 'pin' ? 'pinned' : name] = modeClaimActive(name);
  }
  return state;
}

function syncLocalSleepLayer() {
  const layer = typeof document !== 'undefined' && document &&
    typeof document.getElementById === 'function'
    ? document.getElementById('sleep-layer') : null;
  if (!layer || !layer.classList || typeof layer.classList.toggle !== 'function') return;
  const local = MODE_CLAIMS.sleep && MODE_CLAIMS.sleep.claims &&
    MODE_CLAIMS.sleep.claims.local;
  layer.classList.toggle('on', !!local && activeClaim(local));
}

/* One arbitration API for delayed work: judge ownership by claim priority,
   not by the resolved label.  A real ALERT claim's label varies (INTERCEPT /
   MEMORY LINK / SEISMIC WATCH) and is never the literal string 'ALERT'. */
function modeClaimIsTop(name) {
  resolveMode();
  const priority = ['sleep', 'alert', 'pin', 'agents', 'ambient'];
  const top = priority.find(claimName => modeClaimActive(claimName));
  return top === name;
}

function resolveMode() {
  const state = collectClaimState();
  let label;
  if (state.sleep) label = MODE_CLAIMS.sleep.label;
  else if (state.alert) label = MODE_CLAIMS.alert.label;
  else if (state.pinned) label = MODE_CLAIMS.pin.label;
  else if (state.agents) label = MODE_CLAIMS.agents.label;
  else if (state.ambient) label = MODE_CLAIMS.ambient.label;
  else label = pickMode(state);
  const el = typeof document !== 'undefined' && document &&
    typeof document.getElementById === 'function'
    ? document.getElementById(MODE_EL_ID) : null;
  if (el) el.textContent = 'MODE: ' + label;
  syncLocalSleepLayer();
  const eventKey = JSON.stringify(state);
  if (eventKey !== lastModeEventKey && typeof window !== 'undefined' &&
      window && typeof window.dispatchEvent === 'function') {
    lastModeEventKey = eventKey;
    const EventCtor = window.CustomEvent || window.Event;
    try {
      window.dispatchEvent(EventCtor
        ? new EventCtor('geoframe:mode-change', { detail: state })
        : { type: 'geoframe:mode-change', detail: state });
    } catch (e) {}
  }
  return label;
}

/* Explicit local sleep registration (source:'local').  Not wired to any key
   handler yet -- key-triggered local sleep is W3's responsibility -- but the
   API exists now so setMode('SLEEP') can safely mean the HA-sourced claim. */
function claimSleep(source, until) {
  const opts = { label: 'SLEEP', source: source === 'ha' ? 'ha' : 'local' };
  if (until !== undefined) opts.until = until;
  claimMode('sleep', opts);
  resolveMode();
  return true;
}

/* Compatibility shim: callers keep their return value and API, but every
   visible result is now selected by resolveMode(). */
function setMode(label) {
  initModes();
  /* Only panels.js's HA presence poll calls setMode('SLEEP'); a local claim would outlive its HA wake release. */
  if (label === 'SLEEP') claimMode('sleep', { label, source: 'ha' });
  else if (label === 'PINNED') claimMode('pin', { label, source: 'local' });
  else if (label === 'ALERT' || label === 'INTERCEPT' || label === 'MEMORY LINK' ||
           label === 'SEISMIC WATCH') claimMode('alert', { label, source: 'event' });
  else if (label === 'AGENTS') claimMode('agents', { label, source: 'local' });
  else claimMode('ambient', { label, source: 'local' });
  resolveMode();
  return label;
}
