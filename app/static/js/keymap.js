/* ============================================================
   keymap.js — 表示機用キーマップの取得・検証・キャッシュ
   ============================================================ */

const KEYMAP_CACHE_KEY = 'geoframe.keymap.v1';
const KEYMAP_CACHE_TTL_MS = 12 * 60 * 60 * 1000;
const KEYMAP_ALLOWED_IDS = Object.freeze(['rotation', 'sleep', 'reserved']);
const KEYMAP_DIGITS = Object.freeze(['0', '1', '2', '3', '4', '5', '6', '7', '8', '9']);
const KEYMAP_DEFAULT_KEYS = Object.freeze({
  '0': 'sleep', '1': 'rotation', '2': 'reserved', '3': 'reserved',
  '4': 'reserved', '5': 'reserved', '6': 'reserved', '7': 'reserved',
  '8': 'reserved', '9': 'reserved',
});
const KEYMAP_DEFAULT_MODES = Object.freeze([
  Object.freeze({ id: 'rotation', label: '地図と写真' }),
  Object.freeze({ id: 'sleep', label: '就寝' }),
  Object.freeze({ id: 'reserved', label: '未割当' }),
]);

let keymapCurrent = null;
let keymapLoadPromise = null;
const keymapListeners = [];

function keymapIsRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function keymapHasOwn(value, key) {
  return Object.prototype.hasOwnProperty.call(value, key);
}

function keymapFiniteNumber(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

function keymapNonNegativeInteger(value) {
  return keymapFiniteNumber(value) && Number.isInteger(value) && value >= 0;
}

function keymapKeysAreValid(keys) {
  if (!keymapIsRecord(keys)) return false;
  const names = Object.keys(keys);
  if (names.length !== KEYMAP_DIGITS.length ||
      names.some(name => !KEYMAP_DIGITS.includes(name)) ||
      (typeof Object.getOwnPropertySymbols === 'function' &&
       Object.getOwnPropertySymbols(keys).length)) return false;
  let rotations = 0;
  let sleeps = 0;
  for (const digit of KEYMAP_DIGITS) {
    if (!keymapHasOwn(keys, digit) || typeof keys[digit] !== 'string' ||
        !KEYMAP_ALLOWED_IDS.includes(keys[digit])) return false;
    if (keys[digit] === 'rotation') rotations++;
    if (keys[digit] === 'sleep') sleeps++;
  }
  return rotations >= 1 && sleeps <= 1;
}

function keymapDocumentIsValid(documentValue) {
  return keymapIsRecord(documentValue) &&
    keymapHasOwn(documentValue, 'rev') &&
    keymapNonNegativeInteger(documentValue.rev) &&
    keymapKeysAreValid(documentValue.keys);
}

function keymapClone(value) {
  if (!keymapIsRecord(value)) return value;
  const clone = {};
  for (const key of Object.keys(value)) {
    if (key === 'keys' && keymapIsRecord(value.keys)) clone.keys = Object.assign({}, value.keys);
    else if (key === 'modes' && Array.isArray(value.modes)) {
      clone.modes = value.modes.map(mode => keymapIsRecord(mode) ? Object.assign({}, mode) : mode);
    } else clone[key] = value[key];
  }
  return clone;
}

function validateKeymap(documentValue) {
  return keymapDocumentIsValid(documentValue) ? keymapClone(documentValue) : null;
}

function keymapDefaultDocument() {
  return {
    rev: 0,
    updated_at: null,
    keys: Object.assign({}, KEYMAP_DEFAULT_KEYS),
    modes: KEYMAP_DEFAULT_MODES.map(mode => Object.assign({}, mode)),
  };
}

function getKeymap() {
  return keymapClone(keymapCurrent || keymapDefaultDocument());
}

function getKeyAssignments() {
  return Object.assign({}, getKeymap().keys);
}

function keymapStorage() {
  if (typeof localStorage === 'undefined' || !localStorage ||
      typeof localStorage.getItem !== 'function') return null;
  return localStorage;
}

function keymapNow() {
  return typeof Date !== 'undefined' && Date && typeof Date.now === 'function'
    ? Date.now() : 0;
}

function discardKeymapCache() {
  const storage = keymapStorage();
  if (!storage || typeof storage.removeItem !== 'function') return;
  try { storage.removeItem(KEYMAP_CACHE_KEY); } catch (e) {}
}

function keymapCacheParts(parsed) {
  if (!keymapIsRecord(parsed)) return null;
  let documentValue = parsed;
  if (keymapIsRecord(parsed.data)) documentValue = parsed.data;
  else if (keymapIsRecord(parsed.payload)) documentValue = parsed.payload;
  else if (keymapIsRecord(parsed.value)) documentValue = parsed.value;

  let cachedAt;
  for (const field of ['cachedAt', 'cached_at', 'storedAt', 'savedAt']) {
    if (keymapHasOwn(parsed, field)) { cachedAt = parsed[field]; break; }
  }
  return { documentValue, cachedAt };
}

function readKeymapCache() {
  const storage = keymapStorage();
  if (!storage) return null;
  let raw;
  try { raw = storage.getItem(KEYMAP_CACHE_KEY); } catch (e) { return null; }
  if (raw === null || raw === '') return null;

  let parsed;
  try { parsed = JSON.parse(raw); } catch (e) {
    discardKeymapCache();
    return null;
  }
  const parts = keymapCacheParts(parsed);
  const now = keymapNow();
  if (!parts || !keymapFiniteNumber(parts.cachedAt) || parts.cachedAt < 0 ||
      parts.cachedAt > now || now - parts.cachedAt > KEYMAP_CACHE_TTL_MS) {
    discardKeymapCache();
    return null;
  }
  const valid = validateKeymap(parts.documentValue);
  if (!valid) {
    discardKeymapCache();
    return null;
  }
  return valid;
}

function writeKeymapCache(documentValue) {
  const storage = keymapStorage();
  if (!storage || typeof storage.setItem !== 'function') return;
  try {
    storage.setItem(KEYMAP_CACHE_KEY, JSON.stringify({
      cachedAt: keymapNow(),
      data: documentValue,
    }));
  } catch (e) {}
}

function notifyKeymapListeners(documentValue) {
  for (const listener of keymapListeners.slice()) {
    try { listener(getKeymap()); } catch (e) {}
  }
}

function onKeymapChange(listener) {
  if (typeof listener === 'function') keymapListeners.push(listener);
  return getKeymap();
}

function loadKeymap(listener) {
  if (typeof listener === 'function') keymapListeners.push(listener);
  if (keymapLoadPromise) return keymapLoadPromise;

  const cached = readKeymapCache();
  keymapCurrent = cached || keymapDefaultDocument();
  if (cached) notifyKeymapListeners(cached);
  if (typeof fetch !== 'function') return Promise.resolve(getKeymap());

  let request;
  try {
    request = fetch('/api/keymap', { method: 'GET', cache: 'no-store' });
  } catch (e) {
    return Promise.resolve(getKeymap());
  }
  keymapLoadPromise = Promise.resolve(request)
    .then(response => {
      if (!response || response.ok === false || typeof response.json !== 'function') {
        throw new Error('keymap request failed');
      }
      return response.json();
    })
    .then(documentValue => {
      const valid = validateKeymap(documentValue);
      if (valid) {
        keymapCurrent = valid;
        writeKeymapCache(valid);
        notifyKeymapListeners(valid);
      }
      return getKeymap();
    })
    .catch(() => getKeymap());
  return keymapLoadPromise;
}

function initKeymap(listener) {
  return loadKeymap(listener);
}
