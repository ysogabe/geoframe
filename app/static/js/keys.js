/* ============================================================
   keys.js — 表示機キーボードの正規化・操作・ローカルclaim
   ============================================================ */

const KEY_SLEEP_DOUBLE_PRESS_MS = 2000;
const KEY_SLEEP_DURATION_MS = 9 * 60 * 60 * 1000;
const KEY_CLAIM_MAX_MS = 12 * 60 * 60 * 1000;
const KEY_CLOCK_BACKWARD_GUARD_MS = 5 * 60 * 1000;
const KEY_TOAST_RESERVED_MS = 6000;
const KEYMAP_PIN_STORAGE_KEY = 'geoframe.local.pin.v1';
const KEYMAP_SLEEP_STORAGE_KEY = 'geoframe.local.sleep.v1';
const KEY_TOAST_ID = 'key-toast';

let keysInitialized = false;
let keyAssignments = null;
let sleepFirstPressedAt = null;
let sleepFirstPressTimer = null;
let keyToastTimer = null;
let keyToastGeneration = 0;
let localSleepExpiryTimer = null;
let localSleepClaimed = false;
let localPinClaimed = false;

function keysWindow() {
  return typeof window !== 'undefined' && window ? window : null;
}

function keysSearch() {
  const browserWindow = keysWindow();
  if (browserWindow && browserWindow.location) return String(browserWindow.location.search || '');
  if (typeof location !== 'undefined' && location) return String(location.search || '');
  return '';
}

function keysQueryValue(name) {
  const search = keysSearch();
  if (typeof URLSearchParams === 'function') {
    try { return new URLSearchParams(search).get(name); } catch (e) {}
  }
  const query = search.replace(/^\?/, '').split('&');
  for (const part of query) {
    const pair = part.split('=');
    if (pair[0] === name) return pair.length > 1 ? pair.slice(1).join('=') : '';
  }
  return null;
}

function keysAreEnabled() {
  return keysQueryValue('keys') === '1';
}

function keysThreeDIsEnabled() {
  const browserWindow = keysWindow();
  return keysQueryValue('mode') === '3d' || !!(browserWindow && browserWindow.GEOFRAME_MODE_3D);
}

function keysNow() {
  return typeof Date !== 'undefined' && Date && typeof Date.now === 'function'
    ? Date.now() : 0;
}

function keysFiniteNumber(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

function keysStorage() {
  if (typeof localStorage === 'undefined' || !localStorage ||
      typeof localStorage.getItem !== 'function') return null;
  return localStorage;
}

function keysConsoleWarn(message) {
  if (typeof console !== 'undefined' && console && typeof console.warn === 'function') {
    console.warn(message);
  }
}

function keyClaimRecordIsValid(record, expectedMode, now) {
  if (!record || typeof record !== 'object' || Array.isArray(record) ||
      record.source !== 'local') return { valid: false, reason: 'format' };
  const mode = String(record.mode || '').toLowerCase();
  const expected = expectedMode === 'pin' ? ['pin', 'pinned'] : ['sleep'];
  if (!expected.includes(mode) || !keysFiniteNumber(record.since) ||
      !keysFiniteNumber(record.until) || record.since < 0 || record.until < 0) {
    return { valid: false, reason: 'clock' };
  }
  if (record.since > now) return { valid: false, reason: 'clock' };
  if (record.until <= 0 || record.until < record.since ||
      record.until - record.since > KEY_CLAIM_MAX_MS) {
    return { valid: false, reason: 'clock' };
  }
  if (Object.prototype.hasOwnProperty.call(record, 'savedAt')) {
    if (!keysFiniteNumber(record.savedAt) || record.savedAt < 0 ||
        now - record.savedAt < -KEY_CLOCK_BACKWARD_GUARD_MS) {
      return { valid: false, reason: 'clock' };
    }
  }
  if (record.until > 0 && record.until <= now) return { valid: false, reason: 'expired' };
  return { valid: true };
}

function discardLocalClaim(storageKey, reason, mode) {
  const storage = keysStorage();
  if (storage && typeof storage.removeItem === 'function') {
    try { storage.removeItem(storageKey); } catch (e) {}
  }
  if (mode === 'pin') {
    localPinClaimed = false;
    keysReleaseMode('pin', 'local');
  } else {
    localSleepClaimed = false;
    keysReleaseMode('sleep', 'local');
  }
  if (reason !== 'expired') keysConsoleWarn('[keys] local claim discarded: clock guard');
}

function readLocalClaim(storageKey, expectedMode) {
  const storage = keysStorage();
  if (!storage) return null;
  let raw;
  try { raw = storage.getItem(storageKey); } catch (e) { return null; }
  if (raw === null || raw === '') return null;
  let record;
  try { record = JSON.parse(raw); } catch (e) {
    discardLocalClaim(storageKey, 'format', expectedMode);
    return null;
  }
  const checked = keyClaimRecordIsValid(record, expectedMode, keysNow());
  if (!checked.valid) {
    discardLocalClaim(storageKey, checked.reason, expectedMode);
    return null;
  }
  return record;
}

function saveLocalClaim(storageKey, record, expectedMode) {
  const checked = keyClaimRecordIsValid(record, expectedMode, keysNow());
  if (!checked.valid) {
    discardLocalClaim(storageKey, checked.reason, expectedMode);
    return false;
  }
  const storage = keysStorage();
  if (storage && typeof storage.setItem === 'function') {
    try { storage.setItem(storageKey, JSON.stringify(record)); } catch (e) {}
  }
  return true;
}

function keysClaimMode(name, options) {
  if (typeof claimMode !== 'function') return false;
  try { return !!claimMode(name, options); } catch (e) { return false; }
}

function keysReleaseMode(name, source) {
  if (typeof releaseMode !== 'function') return false;
  try { return !!releaseMode(name, source); } catch (e) { return false; }
}

function keysResolveMode() {
  if (typeof resolveMode !== 'function') return null;
  try { return resolveMode(); } catch (e) { return null; }
}

function keyClaimIsActive(record) {
  if (!record || !record.active) return false;
  if (record.until > 0 && record.until <= keysNow()) {
    record.active = false;
    record.until = 0;
    return false;
  }
  return record.canRender !== false;
}

function localSleepIsActive() {
  if (typeof MODE_CLAIMS !== 'undefined' && MODE_CLAIMS.sleep && MODE_CLAIMS.sleep.claims) {
    const active = keyClaimIsActive(MODE_CLAIMS.sleep.claims.local);
    if (!active && localSleepClaimed) {
      localSleepClaimed = false;
      const storage = keysStorage();
      if (storage && typeof storage.removeItem === 'function') {
        try { storage.removeItem(KEYMAP_SLEEP_STORAGE_KEY); } catch (e) {}
      }
      return false;
    }
    if (active) localSleepClaimed = true;
    return active;
  }
  return localSleepClaimed;
}

function haSleepIsActive() {
  if (typeof MODE_CLAIMS !== 'undefined' && MODE_CLAIMS.sleep && MODE_CLAIMS.sleep.claims &&
      keyClaimIsActive(MODE_CLAIMS.sleep.claims.ha)) return true;
  const browserWindow = keysWindow();
  const state = browserWindow && browserWindow.GEOFRAME_EVENT_STATE;
  if (state && typeof state.isSleeping === 'function') {
    try { return !!state.isSleeping(); } catch (e) {}
  }
  return false;
}

function keyToastElement() {
  if (typeof document === 'undefined' || !document ||
      typeof document.getElementById !== 'function') return null;
  let element = document.getElementById(KEY_TOAST_ID);
  if (element) return element;
  if (typeof document.createElement !== 'function') return null;
  const body = document.body;
  if (!body || typeof body.appendChild !== 'function') return null;
  element = document.createElement('div');
  element.id = KEY_TOAST_ID;
  element.setAttribute?.('role', 'status');
  element.setAttribute?.('aria-live', 'polite');
  body.appendChild(element);
  return element;
}

function keyToastClear(element) {
  if (!element) return;
  element.textContent = '';
  if (element.classList && typeof element.classList.remove === 'function') {
    element.classList.remove('on');
  }
}

function showKeyToast(message, duration) {
  const element = keyToastElement();
  if (!element) return;
  keyToastGeneration++;
  const generation = keyToastGeneration;
  if (typeof clearTimeout === 'function' && keyToastTimer !== null) {
    try { clearTimeout(keyToastTimer); } catch (e) {}
  }
  element.textContent = String(message);
  if (element.classList && typeof element.classList.add === 'function') element.classList.add('on');
  if (typeof setTimeout !== 'function') return;
  keyToastTimer = setTimeout(() => {
    if (generation !== keyToastGeneration) return;
    keyToastClear(element);
    keyToastTimer = null;
  }, duration || 3000);
}

function clearSleepDoublePress() {
  sleepFirstPressedAt = null;
  if (typeof clearTimeout === 'function' && sleepFirstPressTimer !== null) {
    try { clearTimeout(sleepFirstPressTimer); } catch (e) {}
  }
  sleepFirstPressTimer = null;
}

function armSleepDoublePress(now) {
  clearSleepDoublePress();
  sleepFirstPressedAt = now;
  if (typeof setTimeout !== 'function') return;
  sleepFirstPressTimer = setTimeout(() => {
    if (sleepFirstPressedAt === null || keysNow() - sleepFirstPressedAt > KEY_SLEEP_DOUBLE_PRESS_MS) {
      sleepFirstPressedAt = null;
      sleepFirstPressTimer = null;
    }
  }, KEY_SLEEP_DOUBLE_PRESS_MS);
}

function removeStoredClaim(storageKey) {
  const storage = keysStorage();
  if (storage && typeof storage.removeItem === 'function') {
    try { storage.removeItem(storageKey); } catch (e) {}
  }
}

function clearLocalSleepExpiryTimer() {
  if (typeof clearTimeout === 'function' && localSleepExpiryTimer !== null) {
    try { clearTimeout(localSleepExpiryTimer); } catch (e) {}
  }
  localSleepExpiryTimer = null;
}

function armLocalSleepExpiry(until) {
  clearLocalSleepExpiryTimer();
  if (typeof setTimeout !== 'function') return;
  const delay = until - keysNow();
  if (delay <= 0 || delay > KEY_CLAIM_MAX_MS) return;
  localSleepExpiryTimer = setTimeout(() => {
    localSleepExpiryTimer = null;
    releaseLocalSleep();
  }, delay);
}

function releaseLocalSleep() {
  clearLocalSleepExpiryTimer();
  localSleepClaimed = false;
  removeStoredClaim(KEYMAP_SLEEP_STORAGE_KEY);
  keysReleaseMode('sleep', 'local');
  keysResolveMode();
}

function releaseLocalPin() {
  localPinClaimed = false;
  removeStoredClaim(KEYMAP_PIN_STORAGE_KEY);
  keysReleaseMode('pin', 'local');
  keysResolveMode();
}

function localSleepRecord(now) {
  return {
    mode: 'SLEEP', source: 'local', since: now,
    until: now + KEY_SLEEP_DURATION_MS, savedAt: now,
  };
}

function claimLocalSleep() {
  const now = keysNow();
  const record = localSleepRecord(now);
  localSleepClaimed = true;
  keysClaimMode('sleep', { label: 'SLEEP', source: 'local', until: record.until });
  if (!saveLocalClaim(KEYMAP_SLEEP_STORAGE_KEY, record, 'sleep')) {
    localSleepClaimed = false;
    keysReleaseMode('sleep', 'local');
    keysResolveMode();
    return false;
  }
  armLocalSleepExpiry(record.until);
  keysResolveMode();
  return true;
}

function restoreLocalClaims() {
  if (typeof initModes === 'function') {
    try { initModes(); } catch (e) {}
  }
  const pin = readLocalClaim(KEYMAP_PIN_STORAGE_KEY, 'pin');
  if (pin) {
    localPinClaimed = true;
    keysClaimMode('pin', { label: 'PINNED', source: 'local', until: pin.until });
  }
  const sleep = readLocalClaim(KEYMAP_SLEEP_STORAGE_KEY, 'sleep');
  if (sleep) {
    localSleepClaimed = true;
    keysClaimMode('sleep', { label: 'SLEEP', source: 'local', until: sleep.until });
    armLocalSleepExpiry(sleep.until);
  }
  if (pin || sleep) keysResolveMode();
}

function normalizeKeyCode(code) {
  if (typeof code !== 'string') return null;
  if (/^Digit[0-9]$/.test(code)) return code.slice(-1);
  if (/^Numpad[0-9]$/.test(code)) return code.slice(-1);
  return null;
}

function keyTargetIsEditable(target) {
  let current = target;
  while (current) {
    const tag = String(current.tagName || current.nodeName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select' ||
        current.isContentEditable === true || current.contentEditable === true ||
        String(current.contentEditable || '').toLowerCase() === 'true' ||
        current.contenteditable === true ||
        String(current.contenteditable || '').toLowerCase() === 'true') return true;
    current = current.parentElement || current.parentNode || null;
  }
  return false;
}

function keyEventIsIgnored(event) {
  return !event || event.repeat || event.ctrlKey || event.altKey || event.metaKey ||
    event.shiftKey || event.isComposing || keyTargetIsEditable(event.target);
}

function consumeKeyEvent(event) {
  if (event && typeof event.preventDefault === 'function') event.preventDefault();
}

function currentKeyAssignment(digit) {
  const assignments = keyAssignments ||
    (typeof getKeyAssignments === 'function' ? getKeyAssignments() : null) || {};
  return assignments[digit];
}

function onSleepAssignment() {
  const now = keysNow();
  if (sleepFirstPressedAt !== null) {
    const elapsed = now - sleepFirstPressedAt;
    if (elapsed >= 0 && elapsed <= KEY_SLEEP_DOUBLE_PRESS_MS) {
      clearSleepDoublePress();
      claimLocalSleep();
      return;
    }
  }
  armSleepDoublePress(now);
  showKeyToast('もう一度押すと就寝します', KEY_SLEEP_DOUBLE_PRESS_MS);
}

function handleKeydown(event) {
  if (keysThreeDIsEnabled() || keyEventIsIgnored(event)) return;
  const localSleeping = localSleepIsActive();
  const escape = event.code === 'Escape';
  if (localSleeping) {
    consumeKeyEvent(event);
    clearSleepDoublePress();
    releaseLocalSleep();
    return;
  }
  const digit = normalizeKeyCode(event.code);
  if (digit === null && !escape) return;

  const haSleeping = haSleepIsActive();
  consumeKeyEvent(event);
  if (haSleeping) {
    clearSleepDoublePress();
    showKeyToast('HA側で消灯中です', KEY_TOAST_RESERVED_MS);
    return;
  }
  if (escape) {
    clearSleepDoublePress();
    releaseLocalPin();
    if (typeof modeClaimActive === 'function' && modeClaimActive('agents')) {
      keysReleaseMode('agents', 'local');
      keysResolveMode();
    }
    return;
  }

  const assignment = currentKeyAssignment(digit);
  if (assignment === 'rotation') {
    clearSleepDoublePress();
    releaseLocalPin();
    keysReleaseMode('agents', 'local');
  } else if (assignment === 'agents') {
    clearSleepDoublePress();
    releaseLocalPin();
    if (typeof modeClaimActive === 'function' && modeClaimActive('agents')) {
      keysReleaseMode('agents', 'local');
    } else {
      keysClaimMode('agents', { label: 'AGENTS', source: 'local', until: keysNow() + KEY_CLAIM_MAX_MS });
    }
    keysResolveMode();
  } else if (assignment === 'sleep') {
    onSleepAssignment();
  } else if (assignment === 'reserved') {
    showKeyToast('このキーは未割当です', KEY_TOAST_RESERVED_MS);
  } else {
    showKeyToast('未割当のキーです', KEY_TOAST_RESERVED_MS);
  }
}

function initKeys() {
  if (keysInitialized) return true;
  if (!keysAreEnabled() || keysThreeDIsEnabled()) return false;
  keysInitialized = true;
  keyAssignments = typeof getKeyAssignments === 'function' ? getKeyAssignments() : null;
  restoreLocalClaims();
  const browserWindow = keysWindow();
  if (browserWindow && typeof browserWindow.addEventListener === 'function') {
    browserWindow.addEventListener('keydown', handleKeydown, false);
  }
  if (typeof loadKeymap === 'function') {
    loadKeymap(documentValue => {
      keyAssignments = documentValue && documentValue.keys
        ? Object.assign({}, documentValue.keys) : keyAssignments;
    });
  }
  return true;
}

/* Initialization is requested by bootstrap.js after all scripts are loaded. */
