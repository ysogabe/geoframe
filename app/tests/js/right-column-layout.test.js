/* 右カラム（#intel / #envpanel / #legend / #hzlegend）のレイアウト不変条件を、
 * ブラウザを使わず app.css を静的に構文解析＋幾何計算して検証する回帰テスト。
 *
 * 背景: 364bc33 → abab9ce → ab2ce7e は「ROOM ENVパネルの表示崩れ」への応急対応の
 * 積み重ねで、ab2ce7e 自身が「QAゲートが『見える項目数』を検証していなかった」ことを
 * 見落とし原因として述べている。本テストはその欠けていた回帰検知を補う。
 * 根本対応（#envpanelの動的レイアウト化）は Issue #8 に委ねてあり、本テストは
 * 現状の表示崩れを EXPECTED_GAPS のラチェットとして許容しつつ、悪化のみを検知する。
 * CSSの修正はスコープ外なのでこのテストが赤くならないよう app.css を書き換えてはならない。
 */
const assert = require('assert');
const fs = require('fs');

const css = fs.readFileSync('static/app.css', 'utf8');
const html = fs.readFileSync('index.html', 'utf8');
const panelsJs = fs.readFileSync('static/js/panels.js', 'utf8');

const structuralFailures = [];

function must(cond, message) {
  if (!cond) {
    throw new Error('[structural] ' + message);
  }
  return cond;
}

// ============================================================
// CSSミニパーサ（波かっこの対応を数えるだけの極小実装）
// app.cssには @keyframes が7個あり（35/52/74/109/122/222/274行）、
// セレクタ単位の正規表現スキャンではこれらの内部の "0%{...}" 等に
// 引っかかって破綻するため、ネストを厳密に追う必要がある。
// ============================================================

function stripComments(text) {
  // ブレース対応を崩さないよう、コメント本体は同じ長さの空白に置換する
  // （改行だけは維持して以降の処理に影響しないようにする）
  return text.replace(/\/\*[\s\S]*?\*\//g, m => m.replace(/[^\n]/g, ' '));
}

// テキスト先頭からトップレベルの { ... } ブロック列を取り出す。
// ネストしたブロック（@media / @keyframes の中身）はここでは展開しない。
function parseBlocks(text) {
  const blocks = [];
  let i = 0;
  while (i < text.length) {
    const brace = text.indexOf('{', i);
    if (brace === -1) break;
    const selector = text.slice(i, brace).trim();
    let depth = 1;
    let j = brace + 1;
    while (j < text.length && depth > 0) {
      if (text[j] === '{') depth++;
      else if (text[j] === '}') depth--;
      j++;
    }
    must(depth === 0, `波かっこの対応が取れていない: selector="${selector}" 付近`);
    const body = text.slice(brace + 1, j - 1);
    if (selector) blocks.push({ selector, body });
    i = j;
  }
  return blocks;
}

// @media の中身は再帰的に展開し、context:'media' を付ける。
// @keyframes の中身（0%{...}等）はレイアウト計算に不要なので展開しない。
function collectBlocks(text, context) {
  const out = [];
  for (const block of parseBlocks(text)) {
    if (block.selector.startsWith('@media')) {
      out.push({ ...block, context: 'media' });
      out.push(...collectBlocks(block.body, 'media'));
    } else if (block.selector.startsWith('@keyframes')) {
      out.push({ ...block, context });
    } else {
      out.push({ ...block, context });
    }
  }
  return out;
}

// 宣言ブロックの中身を [prop, value] の配列に分解する。
// calc()/var()/rgba() 等の中の ',' や ':' に引きずられないよう、
// 括弧の深さが0の ';' でのみ宣言を区切る。
function parseDeclarations(body) {
  const decls = [];
  let depth = 0;
  let start = 0;
  const push = raw => {
    const s = raw.trim();
    if (!s) return;
    const idx = s.indexOf(':');
    if (idx === -1) return;
    decls.push([s.slice(0, idx).trim(), s.slice(idx + 1).trim()]);
  };
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c === '(') depth++;
    else if (c === ')') depth--;
    else if (c === ';' && depth === 0) {
      push(body.slice(start, i));
      start = i + 1;
    }
  }
  push(body.slice(start));
  return decls;
}

const cleanCss = stripComments(css);
const allBlocks = collectBlocks(cleanCss, 'default');

function declMap(body) {
  const m = new Map();
  for (const [prop, value] of parseDeclarations(body)) m.set(prop, value);
  return m;
}

function findBlock(selector, context) {
  return allBlocks.find(b => b.selector === selector && (context === undefined || b.context === context));
}

// ============================================================
// calc() / min() / max() / var() の再帰下降評価器
// 未対応トークンは必ず throw する（既定値0を返すと「もっともらしい数値」に
// 化けて検証が黙って無意味化するため）。
// ============================================================

function tokenize(expr) {
  const tokens = [];
  let i = 0;
  while (i < expr.length) {
    const c = expr[i];
    if (/\s/.test(c)) { i++; continue; }
    if (c === '(' || c === ')' || c === ',') { tokens.push({ type: c }); i++; continue; }
    if (c === '+' || c === '*' || c === '/') { tokens.push({ type: 'op', op: c }); i++; continue; }
    if (c === '-') {
      if (expr[i + 1] === '-') {
        const m = /^--[a-zA-Z0-9-]+/.exec(expr.slice(i));
        must(m, `不正なCSS変数名: "${expr.slice(i, i + 20)}"`);
        tokens.push({ type: 'var', name: m[0] });
        i += m[0].length;
        continue;
      }
      tokens.push({ type: 'op', op: '-' });
      i++;
      continue;
    }
    if (/[0-9.]/.test(c)) {
      const m = /^[0-9.]+/.exec(expr.slice(i));
      let j = i + m[0].length;
      const unitM = /^[a-zA-Z%]*/.exec(expr.slice(j));
      const unit = unitM[0];
      tokens.push({ type: 'num', value: parseFloat(m[0]), unit });
      i = j + unit.length;
      continue;
    }
    if (/[a-zA-Z]/.test(c)) {
      const m = /^[a-zA-Z-]+/.exec(expr.slice(i));
      tokens.push({ type: 'ident', name: m[0] });
      i += m[0].length;
      continue;
    }
    throw new Error(`calc評価器: 未対応の文字 "${c}" (式: "${expr}")`);
  }
  return tokens;
}

function makeParser(tokens, ctx) {
  let pos = 0;
  const peek = () => tokens[pos];
  const next = () => tokens[pos++];

  function toPx(node) {
    if (node.unit === 'px' || node.unit === '') return node.value;
    if (node.unit === 'vh') return (node.value / 100) * ctx.viewport.height;
    if (node.unit === 'vw') return (node.value / 100) * ctx.viewport.width;
    throw new Error(`calc評価器: 未対応の単位 "${node.unit}"`);
  }

  function parseFactor() {
    const t = peek();
    if (!t) throw new Error('calc評価器: 式が途中で終わっている');
    if (t.type === 'num') { next(); return toPx(t); }
    if (t.type === 'var') {
      next();
      const raw = ctx.vars.get(t.name);
      must(raw !== undefined, `未定義のCSS変数を参照: ${t.name}`);
      return evaluate(raw, ctx);
    }
    if (t.type === '(') {
      next();
      const v = parseExpr();
      must(peek() && peek().type === ')', 'calc評価器: 閉じかっこが無い');
      next();
      return v;
    }
    if (t.type === 'ident') {
      const fn = t.name;
      next();
      must(peek() && peek().type === '(', `calc評価器: 関数呼び出しの ( が無い (${fn})`);
      next();
      const args = [parseExpr()];
      while (peek() && peek().type === ',') { next(); args.push(parseExpr()); }
      must(peek() && peek().type === ')', `calc評価器: 関数呼び出しの ) が無い (${fn})`);
      next();
      if (fn === 'calc') { must(args.length === 1, 'calc()は引数1つのみ対応'); return args[0]; }
      if (fn === 'min') return Math.min(...args);
      if (fn === 'max') return Math.max(...args);
      // var(--x) の "--x" 自体は既に専用の 'var' トークン型で解決済みなので、
      // ここに来るのは var(...) という関数呼び出しの外側の形だけであり、
      // 中身(args[0])をそのまま返せばよい
      if (fn === 'var') { must(args.length === 1, 'var()は引数1つのみ対応（フォールバックは本CSSに存在しない）'); return args[0]; }
      throw new Error(`calc評価器: 未対応の関数 "${fn}"`);
    }
    throw new Error(`calc評価器: 予期しないトークン ${JSON.stringify(t)}`);
  }

  function parseTerm() {
    let v = parseFactor();
    while (peek() && peek().type === 'op' && (peek().op === '*' || peek().op === '/')) {
      const op = next().op;
      const rhs = parseFactor();
      v = op === '*' ? v * rhs : v / rhs;
    }
    return v;
  }

  function parseExpr() {
    let v = parseTerm();
    while (peek() && peek().type === 'op' && (peek().op === '+' || peek().op === '-')) {
      const op = next().op;
      const rhs = parseTerm();
      v = op === '+' ? v + rhs : v - rhs;
    }
    return v;
  }

  return { parseExpr, atEnd: () => pos >= tokens.length };
}

function evaluate(expr, ctx) {
  const tokens = tokenize(expr);
  const parser = makeParser(tokens, ctx);
  const v = parser.parseExpr();
  must(parser.atEnd(), `calc評価器: 式の末尾に余分なトークンがある: "${expr}"`);
  return v;
}

// ============================================================
// vacuous pass 防止 (2): 収集件数を明示的に固定して検証する
// ============================================================

const rootBlocks = allBlocks.filter(b => b.selector === ':root');
must(rootBlocks.length === 2, `:root ブロックは2個のはずが ${rootBlocks.length} 個だった`);

const rightColumnPanels = allBlocks.filter(b => {
  const decls = declMap(b.body);
  return decls.get('right') === '46px';
});
must(
  rightColumnPanels.length === 4,
  `right:46px を持つ右カラムパネルは4個のはずが ${rightColumnPanels.length} 個だった: ` +
    rightColumnPanels.map(b => b.selector).join(', ')
);
const rightColumnSelectors = new Set(rightColumnPanels.map(b => b.selector));
for (const sel of ['#intel', '#envpanel', '#legend', '#hzlegend']) {
  must(rightColumnSelectors.has(sel), `右カラムパネルに ${sel} が見つからない`);
}

const intelTileCount = (html.match(/class="tile"/g) || []).length;
must(intelTileCount === 6, `#intel の .tile は6個のはずが ${intelTileCount} 個だった`);

const legendRowCount = (html.match(/class="row"/g) || []).length;
must(legendRowCount === 4, `#legend の .row は4個のはずが ${legendRowCount} 個だった`);

const envLabelsMatch = /const ENV_LABELS\s*=\s*\{([^}]*)\}/.exec(panelsJs);
must(envLabelsMatch, 'panels.js に ENV_LABELS の定義が見つからない');
const envLabelsKeyCount = (envLabelsMatch[1].match(/[a-zA-Z_][a-zA-Z0-9_]*\s*:/g) || []).length;
must(envLabelsKeyCount === 5, `ENV_LABELS のキーは5個のはずが ${envLabelsKeyCount} 個だった`);
// センサー数5の第一の出典は app.css 281-283行のコメント
// 「HAセンサー5個中2個までしか表示できない」。ENV_LABELSのキー数(5)は
// それと一致するかを見る cross-check であり、第一の出典ではない
// （ENV_LABELSはit.class→表示名のフォールバック辞書であり台帳ではないため）。
must(
  /HAセンサー5個中2個までしか表示できない/.test(css),
  'app.css の応急対応コメント（HAセンサー5個中2個…）が見つからない。' +
    'センサー数5の出典が失われている可能性がある'
);
const SENSOR_COUNT = 5;

// INV-0e: panels.jsがclass="tile"を生成し、index.htmlにclass="in-head"がある
must(/class="tile"/.test(panelsJs), 'INV-0e: panels.js に class="tile" を生成する箇所が無い');
must(/class="in-head"/.test(html), 'INV-0e: index.html に class="in-head" が無い');

// ============================================================
// :root 変数の収集と INV-0f（var()参照の突き合わせ）
// ============================================================

const defaultRootDecls = declMap(rootBlocks[0].body);
const kioskRootDecls = declMap(rootBlocks[1].body);

function effectiveVars(profile) {
  const m = new Map(defaultRootDecls);
  if (profile === 'kiosk') for (const [k, v] of kioskRootDecls) m.set(k, v);
  return m;
}

// --rot は :root には無い。photocard.js が要素ごとに
// el.style.setProperty('--rot', ...) でインラインに設定する「ローカルなカスタム
// プロパティ」であり、:root定義漏れのバグではなく意図的な設計。
// 素通しにするとvar()突き合わせの意味が薄れるため、例外は名指しし、かつ
// その根拠（photocard.jsが実際に設定している）自体をここで検証する。
const KNOWN_JS_SET_VARS = new Set(['--rot']);
const photocardJs = fs.readFileSync('static/js/photocard.js', 'utf8');
must(
  /setProperty\(\s*'--rot'/.test(photocardJs),
  '--rot はJS側で設定される想定だが static/js/photocard.js に setProperty(\'--rot\'...) が見つからない。' +
    'KNOWN_JS_SET_VARS の例外根拠が失われている'
);

const referencedVars = new Set();
for (const m of cleanCss.matchAll(/var\(\s*(--[a-zA-Z0-9-]+)/g)) referencedVars.add(m[1]);
must(referencedVars.size > 0, 'var() 参照が1件も見つからない（パーサが壊れている疑い）');
const definedVarNames = new Set([...defaultRootDecls.keys(), ...kioskRootDecls.keys()]);
const undefinedVars = [...referencedVars].filter(v => !definedVarNames.has(v) && !KNOWN_JS_SET_VARS.has(v));
must(
  undefinedVars.length === 0,
  `INV-0f: :root定義の無い var() 参照がある: ${undefinedVars.join(', ')}`
);

// ============================================================
// INV-0a/0b: @media の条件式を各プロファイルのビューポートで実評価する
// ブロックの存在確認だけでは、条件式(min-width:2200px等)が将来書き換わって
// 実機がキオスク系統から外れても検知できないため、条件式そのものを評価する。
// ============================================================

const mediaBlock = parseBlocks(cleanCss).find(b => b.selector.startsWith('@media'));
must(mediaBlock, '@media ブロックが見つからない');
const mediaCond = mediaBlock.selector;

function evalMediaQuery(query, viewport) {
  const minWidth = /min-width:\s*([\d.]+)px/.exec(query);
  const minAspect = /min-aspect-ratio:\s*([\d.]+)\s*\/\s*([\d.]+)/.exec(query);
  must(minWidth, `@media 条件式から min-width を抽出できない: "${query}"`);
  must(minAspect, `@media 条件式から min-aspect-ratio を抽出できない: "${query}"`);
  const widthOk = viewport.width >= parseFloat(minWidth[1]);
  const aspectOk = viewport.width / viewport.height >= parseFloat(minAspect[1]) / parseFloat(minAspect[2]);
  return widthOk && aspectOk;
}

const PROFILES = {
  desktop: { viewport: { width: 1920, height: 1080 }, metricsConfidence: 'derived' },
  kiosk: { viewport: { width: 2560, height: 1080 }, metricsConfidence: 'measured' },
};

const kioskMatches = evalMediaQuery(mediaCond, PROFILES.kiosk.viewport);
const desktopMatches = evalMediaQuery(mediaCond, PROFILES.desktop.viewport);
must(kioskMatches === true, `INV-0a: @media条件がkiosk(2560x1080)でtrueにならない: "${mediaCond}"`);
must(desktopMatches === false, `INV-0b: @media条件がdesktop(1920x1080)でfalseにならない: "${mediaCond}"`);

// ============================================================
// レイアウト方式の変更検知: position:absolute が消えたら幾何がCSS単体から
// 決まらなくなるため、黙って通さず明示的に失敗させる
// ============================================================

for (const sel of ['#intel', '#envpanel', '#legend', '#hzlegend']) {
  const block = allBlocks.find(b => b.selector === sel && b.context === 'default');
  must(block, `${sel} の宣言ブロックが見つからない`);
  const decls = declMap(block.body);
  if (decls.get('position') !== 'absolute') {
    throw new Error(
      `[layout-method] ${sel} から position:absolute が消えている。` +
        'Issue #8 の flexbox 化を進めているなら、このテスト(right-column-layout.test.js)の' +
        '該当部分も同じPRで更新すること。'
    );
  }
}

// ============================================================
// フォントメトリクス仮定（本テスト唯一の未検証の仮定。ここに集約する）
//
// 出典: 2026-08-14 / macOS + Chromium / 実測（Issue #8 対応中のadvisorレビューで
// 実データにより発覚した表示崩れの調査時に測定）。
// 懸念: 実機（デプロイ先）はLinuxであり、--jp フォントスタック
// ('Zen Kaku Gothic New' 等)が解決できずフォールバックする可能性がある。
// その場合これらの数値はズレる（Issue #7 で実機再測予定）。
//
// LINE_HEIGHT_FACTOR方式（font-size×行高係数+padding+borderから導出）は
// 不成立と確認済み（LH=1.2で導出するとtileColumnHeightPxが 87.2px vs 実測91pxで
// 3.8pxも外れる。.tileの縦積みは.k側に.sw(9px)を含むflex行があり、
// 単純な係数では説明できない）。よってこの4値はそのまま実測定数として持つ。
// ============================================================

const MEASURED_KIOSK = {
  inHeadHeightPx: 45, // .in-head: --fs-sm(20px)1行 + padding 10px上下 + border 1px
  tileRowHeightPx: 49, // .tile 横並び: --fs-lg(26px) + padding 9px上下 + border 1px
  tileColumnHeightPx: 91, // #intel .tile 縦積み(@media限定): 2行 + gap 3px + padding 14px上下 + border 1px
};
// legendRowHeightPx だけは他の3つと性質が異なる: #legend の font-size は10pxの
// リテラルで@mediaに上書きされないため、この値は両プロファイル共通
// (profile-independent)。実測ではなく font-size(10px) × 行高係数1.2 の推定値。
const PROFILE_INDEPENDENT_METRICS = {
  legendRowHeightPx: 12, // 推定値（実測なし）: 10px × 1.2
};
const ALL_TUNABLE_METRICS = { ...MEASURED_KIOSK, ...PROFILE_INDEPENDENT_METRICS };

// ============================================================
// #legend の高さ（フォントメトリクス非依存: font-sizeはリテラルなので
// legendRowHeightPxはprofile-independentとして扱ってよい）
// ============================================================

const legendDecls = declMap(findBlock('#legend', 'default').body);

function parsePaddingVertical(value) {
  const parts = value.trim().split(/\s+/).map(p => {
    const m = /^([\d.]+)px$/.exec(p);
    must(m, `padding の値を解釈できない: "${p}" (元: "${value}")`);
    return parseFloat(m[1]);
  });
  if (parts.length === 1) return parts[0] * 2;
  if (parts.length === 2) return parts[0] * 2;
  if (parts.length === 3) return parts[0] + parts[2];
  if (parts.length === 4) return parts[0] + parts[2];
  throw new Error(`padding の値の分割数が想定外: "${value}"`);
}

function parseBorderWidthVertical(value) {
  const m = /^([\d.]+)px/.exec(value.trim());
  must(m, `border の幅を解釈できない: "${value}"`);
  return parseFloat(m[1]) * 2; // 上下2辺分
}

function parseGap(value) {
  const m = /^([\d.]+)px$/.exec(value.trim());
  must(m, `gap の値を解釈できない: "${value}"`);
  return parseFloat(m[1]);
}

function computeLegendGeometry(metrics, viewport) {
  const paddingV = parsePaddingVertical(must(legendDecls.get('padding'), '#legend に padding が無い'));
  const borderV = parseBorderWidthVertical(must(legendDecls.get('border'), '#legend に border が無い'));
  const gap = parseGap(must(legendDecls.get('gap'), '#legend に gap が無い'));
  const height =
    legendRowCount * metrics.legendRowHeightPx + (legendRowCount - 1) * gap + paddingV + borderV;
  const bottom = 64; // #legend { bottom:64px } はリテラル
  const top = viewport.height - bottom - height;
  return { height, top, bottom };
}

// ============================================================
// #envpanel / #intel の幾何（calc()評価器を通す）
// ============================================================

const intelDecls = declMap(findBlock('#intel', 'default').body);
const envpanelDecls = declMap(findBlock('#envpanel', 'default').body);

function computeGeometry(profile, metrics) {
  const viewport = PROFILES[profile].viewport;
  const vars = effectiveVars(profile);
  const ctx = { vars, viewport };

  const intelTop = evaluate(must(intelDecls.get('top'), '#intel に top が無い'), ctx);
  const envTop = evaluate(must(envpanelDecls.get('top'), '#envpanel に top が無い'), ctx);
  const envMaxHeight = evaluate(
    must(envpanelDecls.get('max-height'), '#envpanel に max-height が無い'),
    ctx
  );
  const legend = computeLegendGeometry(metrics, viewport);

  const metricsAvailable = PROFILES[profile].metricsConfidence === 'measured';

  let intelHeight = null;
  let intelBottom = null;
  let clearance = null;
  let capacity = null;
  let envActualHeight = null;
  let envBottom = null;

  if (metricsAvailable) {
    // .tile:last-child{border-bottom:0} のぶん、N枚合計は N*tileH - 1px になる
    intelHeight = metrics.inHeadHeightPx + intelTileCount * metrics.tileColumnHeightPx - 1;
    intelBottom = intelTop + intelHeight;
    clearance = envTop - intelBottom;
    capacity = Math.floor((envMaxHeight - metrics.inHeadHeightPx) / metrics.tileRowHeightPx);
    envActualHeight = metrics.inHeadHeightPx + capacity * metrics.tileRowHeightPx;
    envBottom = envTop + envActualHeight;
  }

  const available = legend.top - envTop;
  const inv5Excess = envMaxHeight - available;

  return {
    profile,
    viewport,
    metricsAvailable,
    intelTop,
    intelHeight,
    intelBottom,
    envTop,
    envMaxHeight,
    envActualHeight,
    envBottom,
    clearance,
    capacity,
    legendTop: legend.top,
    legendHeight: legend.height,
    inv5Excess,
  };
}

const nominalGeom = {
  desktop: computeGeometry('desktop', ALL_TUNABLE_METRICS),
  kiosk: computeGeometry('kiosk', ALL_TUNABLE_METRICS),
};

// ============================================================
// 不変条件の判定（PASS/FAIL/INFO）
// ============================================================

function verdictInv1(g) {
  if (!g.metricsAvailable) return 'INFO';
  return g.capacity >= SENSOR_COUNT ? 'PASS' : 'FAIL';
}
function verdictInv2(g) {
  if (!g.metricsAvailable) return 'INFO';
  return g.clearance >= 8 ? 'PASS' : 'FAIL';
}
function verdictInv3(g) {
  if (!g.metricsAvailable) return 'INFO';
  return g.envBottom <= g.legendTop ? 'PASS' : 'FAIL';
}
function verdictInv5(g) {
  return g.inv5Excess <= 0 ? 'PASS' : 'FAIL';
}

const INV_DEFS = {
  'INV-1': { verdictFn: verdictInv1, valueFn: g => g.capacity, direction: 'biggerIsBetter' },
  'INV-2': { verdictFn: verdictInv2, valueFn: g => g.clearance, direction: 'biggerIsBetter' },
  'INV-3': { verdictFn: verdictInv3, valueFn: g => (g.metricsAvailable ? g.envBottom : null) },
  'INV-5': { verdictFn: verdictInv5, valueFn: g => g.inv5Excess, direction: 'smallerIsBetter' },
};

// ============================================================
// EXPECTED_GAPS: 既知の未解決ギャップ3件をラチェットとして保持する。
// 到達値を下回ったら失敗（悪化検知）、targetを満たしても失敗（エントリ削除を促す）。
// INV-1/INV-2は大きいほど良い、INV-5は超過量なので小さいほど良い（target:0）。
// ============================================================

const EXPECTED_GAPS = {
  'kiosk/INV-1': { target: 5, ratchetedTo: 2, direction: 'biggerIsBetter', unit: 'センサー表示数', closesWith: '#8' },
  'kiosk/INV-2': { target: 8, ratchetedTo: -2, direction: 'biggerIsBetter', unit: 'px クリアランス', closesWith: '#8' },
  'desktop/INV-5': { target: 0, ratchetedTo: 132, direction: 'smallerIsBetter', unit: 'px キャップ超過', closesWith: '#8' },
};

function checkRatchet(key, currentValue) {
  const g = EXPECTED_GAPS[key];
  if (g.direction === 'biggerIsBetter') {
    if (currentValue < g.ratchetedTo) {
      return { ok: false, reason: `悪化検知: ${key} が到達値 ${g.ratchetedTo}${g.unit} を下回った (現在: ${currentValue}${g.unit})` };
    }
    if (currentValue >= g.target) {
      return { ok: false, reason: `${key} が target ${g.target}${g.unit} を満たした。EXPECTED_GAPS のエントリを削除すること` };
    }
    return { ok: true };
  }
  // smallerIsBetter
  if (currentValue > g.ratchetedTo) {
    return { ok: false, reason: `悪化検知: ${key} が到達値 ${g.ratchetedTo}${g.unit} を上回った (現在: ${currentValue}${g.unit})` };
  }
  if (currentValue <= g.target) {
    return { ok: false, reason: `${key} が target ${g.target}${g.unit} を満たした。EXPECTED_GAPS のエントリを削除すること` };
  }
  return { ok: true };
}

// ============================================================
// INV-6（感度チェック）: MEASURED_KIOSK/PROFILE_INDEPENDENT_METRICS の4定数を
// それぞれ±2px振ってPASS/FAILが反転するかを見る。反転したらSENSITIVITY-LIMITEDと
// 分類するだけで、それ自体では失敗にしない。ラチェット比較には必ずnominal値を使う。
// ============================================================

const METRIC_KEYS = Object.keys(ALL_TUNABLE_METRICS);
const PERTURBATIONS = [];
for (const key of METRIC_KEYS) {
  for (const delta of [-2, 2]) {
    PERTURBATIONS.push({ key, delta });
  }
}

function perturbedGeometry(profile, key, delta) {
  const metrics = { ...ALL_TUNABLE_METRICS, [key]: ALL_TUNABLE_METRICS[key] + delta };
  return computeGeometry(profile, metrics);
}

const perturbedGeomCache = {};
for (const profile of Object.keys(PROFILES)) {
  perturbedGeomCache[profile] = PERTURBATIONS.map(p => ({
    ...p,
    geom: perturbedGeometry(profile, p.key, p.delta),
  }));
}

// ============================================================
// 判定の実行と数値表の出力
// ============================================================

const blockingFailures = [];
const table = [];

for (const profile of Object.keys(PROFILES)) {
  const g = nominalGeom[profile];
  for (const invId of Object.keys(INV_DEFS)) {
    const def = INV_DEFS[invId];
    const nominalVerdict = def.verdictFn(g);
    const key = `${profile}/${invId}`;
    const value = def.valueFn(g);

    let sensitivityLimited = false;
    if (nominalVerdict !== 'INFO') {
      for (const p of perturbedGeomCache[profile]) {
        const v = def.verdictFn(p.geom);
        if (v !== 'INFO' && v !== nominalVerdict) sensitivityLimited = true;
      }
    }

    let note = '';
    if (nominalVerdict === 'INFO') {
      note = 'INFO (no measured metrics for this profile)';
    } else if (EXPECTED_GAPS[key]) {
      const ratchet = checkRatchet(key, value);
      note = ratchet.ok
        ? `EXPECTED (ratcheted to ${EXPECTED_GAPS[key].ratchetedTo}${EXPECTED_GAPS[key].unit}, target ${EXPECTED_GAPS[key].target})`
        : `RATCHET VIOLATION: ${ratchet.reason}`;
      if (!ratchet.ok) blockingFailures.push(ratchet.reason);
    } else if (nominalVerdict === 'FAIL') {
      const allPerturbedFail = perturbedGeomCache[profile]
        .map(p => def.verdictFn(p.geom))
        .every(v => v === 'FAIL');
      if (allPerturbedFail) {
        const reason = `${key} が未登録のまま FAIL (nominal値=${value}) であり、` +
          '定数を±2px振った全組み合わせでもFAILのままだった（明確な悪化）';
        note = 'BLOCKING FAILURE (unregistered, robust to ±2px)';
        blockingFailures.push(reason);
      } else {
        note = 'FAIL (unregistered, but not robust to ±2px — not blocking)';
      }
    } else {
      note = '';
    }
    if (sensitivityLimited) note = (note ? note + ' / ' : '') + 'SENSITIVITY-LIMITED';

    table.push({
      profile,
      inv: invId,
      value: value === null || value === undefined ? '-' : Math.round(value * 10) / 10,
      verdict: nominalVerdict,
      note,
    });
  }
}

// 幾何の数値表（top/height/bottom/max-height/容量/クリアランス）
const geomTable = [];
for (const profile of Object.keys(PROFILES)) {
  const g = nominalGeom[profile];
  geomTable.push({
    profile,
    panel: '#intel',
    top: Math.round(g.intelTop),
    height: g.intelHeight === null ? 'N/A (INFO)' : Math.round(g.intelHeight),
    bottom: g.intelBottom === null ? 'N/A (INFO)' : Math.round(g.intelBottom),
  });
  geomTable.push({
    profile,
    panel: '#envpanel',
    top: Math.round(g.envTop),
    height: g.envActualHeight === null ? 'N/A (INFO)' : Math.round(g.envActualHeight),
    bottom: g.envBottom === null ? 'N/A (INFO)' : Math.round(g.envBottom),
    maxHeight: Math.round(g.envMaxHeight),
    capacity: g.capacity === null ? 'N/A (INFO)' : g.capacity,
    clearance: g.clearance === null ? 'N/A (INFO)' : Math.round(g.clearance),
  });
  geomTable.push({
    profile,
    panel: '#legend',
    top: Math.round(g.legendTop),
    height: Math.round(g.legendHeight),
    bottom: g.legendTop + g.legendHeight,
  });
}

console.log('=== right column layout: geometry (px) ===');
console.table(geomTable);
console.log('=== right column layout: invariant verdicts ===');
console.table(table);

// ============================================================
// 期待される算出結果との突き合わせ（契約 knowledge_snapshot 記載値）。
// 一致しない場合は EXPECTED_GAPS や判定規則を辻褄合わせで動かさず、
// .hermes/decision.json とResult Manifestの summary に差異を記録すること。
// ============================================================

const EXPECTED_NOMINAL = {
  'kiosk/INV-1/value': 2,
  'kiosk/INV-2/value': -2,
  'desktop/INV-5/value': 132,
};
const actualNominal = {
  'kiosk/INV-1/value': nominalGeom.kiosk.capacity,
  'kiosk/INV-2/value': Math.round(nominalGeom.kiosk.clearance),
  'desktop/INV-5/value': Math.round(nominalGeom.desktop.inv5Excess),
};
for (const k of Object.keys(EXPECTED_NOMINAL)) {
  const expected = EXPECTED_NOMINAL[k];
  const actual = actualNominal[k];
  const tolerance = k.endsWith('INV-1/value') ? 0 : 5;
  if (!Number.isFinite(actual) || Math.abs(actual - expected) > tolerance) {
    throw new Error(
      `算出結果が契約の期待値と食い違っている: ${k} 期待=${expected} 実測=${actual}。` +
        'パーサまたは高さモデルに誤りがある可能性が高い。' +
        '.hermes/decision.json とResult Manifestのsummaryに記録すること。'
    );
  }
}
assert.strictEqual(
  Math.round(nominalGeom.desktop.envMaxHeight),
  695,
  'desktop系統の#envpanel max-heightはcalcから決定的に695pxになるはず'
);
assert.strictEqual(
  Math.round(nominalGeom.kiosk.envMaxHeight),
  167,
  'kiosk系統の#envpanel max-heightはcalcから決定的に167pxになるはず'
);
assert.strictEqual(
  Math.round(nominalGeom.kiosk.intelHeight),
  590,
  'kiosk系統の#intel高さは590pxになるはず'
);

// ============================================================
// 終了コードの決定（rule a/b/c 以外では落ちない）
// ============================================================

if (blockingFailures.length > 0) {
  console.error('=== BLOCKING FAILURES ===');
  for (const f of blockingFailures) console.error('- ' + f);
  process.exit(1);
}

console.log('right column layout: OK (既知ギャップ3件はEXPECTED_GAPSのラチェット内、Issue #8待ち)');
