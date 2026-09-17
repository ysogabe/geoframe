#!/usr/bin/env node
/**
 * measure-display-client.mjs — 表示クライアント側の描画性能をCDP経由で実測する
 *
 * 用途:
 *   「サーバーは別機体、表示のみ」構成に切り替えたとき、表示機（Pi400等）の
 *   描画性能とGPU経路が実用品質に達しているかを、同一の手順で測るための道具。
 *   Chromiumを --remote-debugging-port 付きで動かしている機体で使う。
 *
 * 使い方:
 *   node measure-display-client.mjs [--port 9222] [--seconds 10] [--url-substr 8080]
 *   例) node measure-display-client.mjs --seconds 20
 *
 * 前提:
 *   Node 22+（グローバル WebSocket）。npm依存なし。
 *
 * 出力:
 *   JSON 1件を標準出力へ。--out <file> で追記保存（JSONL）も可能。
 *   ※このスクリプトは読み取り専用（Runtime.evaluateで観測コードを流すだけ）。
 *     ページの状態は書き換えない。
 */
import fs from "node:fs";

const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : def;
};
const PORT = Number(opt("port", "9222"));
const SECONDS = Number(opt("seconds", "10"));
const URL_SUBSTR = opt("url-substr", "8080");
const OUT = opt("out", "");
const LABEL = opt("label", "");

async function httpJson(path) {
  const r = await fetch(`http://127.0.0.1:${PORT}${path}`);
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
  return r.json();
}

function connect(wsUrl) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    const pending = new Map();
    let id = 0;
    ws.onopen = () => resolve({
      send(method, params = {}) {
        return new Promise((res, rej) => {
          const mid = ++id;
          pending.set(mid, { res, rej });
          ws.send(JSON.stringify({ id: mid, method, params }));
        });
      },
      close: () => ws.close(),
    });
    ws.onerror = (e) => reject(new Error(`WS error: ${e.message ?? e}`));
    ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && pending.has(msg.id)) {
        const { res, rej } = pending.get(msg.id);
        pending.delete(msg.id);
        msg.error ? rej(new Error(JSON.stringify(msg.error))) : res(msg.result);
      }
    };
  });
}

// ページ内で走らせる観測コード（副作用なし・戻り値はJSON）
const PROBE = (seconds) => `
(async () => {
  const out = {};
  out.url = location.href;
  out.title = document.title;
  out.viewport = { w: innerWidth, h: innerHeight, dpr: devicePixelRatio };
  out.visibility = document.visibilityState;
  out.domNodes = document.querySelectorAll('*').length;
  out.mapLibre = typeof window.maplibregl !== 'undefined';
  try {
    const c = document.createElement('canvas');
    const gl = c.getContext('webgl2') || c.getContext('webgl');
    const dbg = gl && gl.getExtension('WEBGL_debug_renderer_info');
    out.webgl = gl ? {
      renderer: dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER),
      vendor: dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR),
      version: gl.getParameter(gl.VERSION),
    } : null;
    // 実画面のcanvasがどれで描かれているか
    const cvs = [...document.querySelectorAll('canvas')].map((el) => ({
      w: el.width, h: el.height, cls: el.className || el.id || '',
    }));
    out.canvases = cvs.slice(0, 8);
  } catch (e) { out.webglError = String(e); }

  const longTasks = [];
  let po = null;
  try {
    po = new PerformanceObserver((l) => {
      for (const e of l.getEntries()) longTasks.push(Math.round(e.duration));
    });
    po.observe({ entryTypes: ['longtask'] });
  } catch {}

  const frames = [];
  const t0 = performance.now();
  // rAFが止まっているページ（非表示・DPMS off・occluded）では永久にresolveしないため、
  // 必ず秒数で打ち切る。'how' で「回っていたか/止まっていたか」を判別できるようにする。
  const how = await new Promise((res) => {
    let last = performance.now();
    const hard = setTimeout(() => res(frames.length ? 'partial' : 'raf-stalled'), ${seconds} * 1000 + 500);
    function tick(t) {
      frames.push(t - last); last = t;
      if (t - t0 < ${seconds} * 1000) requestAnimationFrame(tick);
      else { clearTimeout(hard); res('ok'); }
    }
    requestAnimationFrame(tick);
  });
  const elapsed = performance.now() - t0;
  if (po) po.disconnect();
  out.how = how;
  out.visibility = document.visibilityState;
  out.fps = { rafStalled: how === 'raf-stalled', frames: frames.length, seconds: +(elapsed / 1000).toFixed(2) };
  if (frames.length > 1) {
    const d = frames.slice(1).sort((a, b) => a - b);
    const pct = (p) => +d[Math.min(d.length - 1, Math.floor(d.length * p))].toFixed(2);
    out.fps = { ...out.fps,
      fpsMean: +(d.length / (elapsed / 1000)).toFixed(1),
      frameMsP50: pct(0.5), frameMsP95: pct(0.95), frameMsP99: pct(0.99),
      over33ms: d.filter((x) => x > 33.4).length,
      over50ms: d.filter((x) => x > 50).length,
    };
  }
  out.longTasks = { count: longTasks.length, totalMs: longTasks.reduce((a, b) => a + b, 0), maxMs: Math.max(0, ...longTasks) };
  if (performance.memory) out.jsHeapMB = +(performance.memory.usedJSHeapSize / 1048576).toFixed(1);
  return JSON.stringify(out);
})()`;

(async () => {
  const targets = await httpJson("/json/list");
  const page = targets.find((t) => t.type === "page" && (t.url || "").includes(URL_SUBSTR))
    || targets.find((t) => t.type === "page");
  if (!page) { console.error("page target not found"); process.exit(2); }
  const c = await connect(page.webSocketDebuggerUrl);
  const r = await c.send("Runtime.evaluate", {
    expression: PROBE(SECONDS), awaitPromise: true, returnByValue: true,
  });
  c.close();
  if (r.exceptionDetails) { console.error(JSON.stringify(r.exceptionDetails)); process.exit(3); }
  const res = JSON.parse(r.result.value);
  const record = {
    label: LABEL || page.title,
    measuredAt: new Date().toISOString(),
    targetUrl: page.url,
    ...res,
  };
  const text = JSON.stringify(record, null, 2);
  console.log(text);
  if (OUT) fs.appendFileSync(OUT, JSON.stringify(record) + "\n");
})().catch((e) => { console.error(String(e)); process.exit(1); });
