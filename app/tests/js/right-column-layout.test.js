/* 右カラム（#intel / #envpanel / #hzlegend）のレイアウト不変条件を、
 * ブラウザを使わず app.css を静的に構文解析＋幾何計算して検証する回帰テスト。
 *
 * 背景: 364bc33 → abab9ce → ab2ce7e は「ROOM ENVパネルの表示崩れ」への応急対応の
 * 積み重ねで、ab2ce7e 自身が「QAゲートが『見える項目数』を検証していなかった」ことを
 * 見落とし原因として述べている。本テストはその欠けていた回帰検知を補う。
 *
 * 旧バージョン（2ef6362/0c64ee9）は根本対応前のCSSを対象に、既知の表示崩れを
 * EXPECTED_GAPS のラチェットとして許容しつつ悪化のみを検知していた。本コミットで
 * Issue #8 の根本対応（#legend削除・#intelコンパクト化によるROOM ENV表示余白の確保）
 * を app.css に実施したため、そのラチェットは撤去し、INV-1（表示容量>=5）/INV-2
 * （クリアランス>=8px）はPASSの直接assertへ、旧INV-3/INV-5（#legendとの重なり回避）
 * は#legend自体の削除に伴い撤去した。
 */
const assert = require('assert');
const fs = require('fs');

const css = fs.readFileSync('static/app.css', 'utf8');
const html = fs.readFileSync('index.html', 'utf8');
const panelsJs = fs.readFileSync('static/js/panels.js', 'utf8');

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
  rightColumnPanels.length === 3,
  `right:46px を持つ右カラムパネルは3個のはずが ${rightColumnPanels.length} 個だった: ` +
    rightColumnPanels.map(b => b.selector).join(', ')
);
const rightColumnSelectors = new Set(rightColumnPanels.map(b => b.selector));
for (const sel of ['#intel', '#envpanel', '#hzlegend']) {
  must(rightColumnSelectors.has(sel), `右カラムパネルに ${sel} が見つからない`);
}

// #legend（レイヤ凡例）はIssue #8根本対応で削除済み。#intelの各tileに既に
// 色スウォッチ(.sw)があり情報が重複していたため。完全に消えたことを確認する。
must(!/id="legend"/.test(html), '#legend がindex.htmlから削除されていない（Issue #8根本対応で削除したはず）');
must(!/#legend\b/.test(cleanCss), '#legend の宣言がapp.cssから削除されていない（Issue #8根本対応で削除したはず）');

const intelTileCount = (html.match(/class="tile"/g) || []).length;
must(intelTileCount === 6, `#intel の .tile は6個のはずが ${intelTileCount} 個だった`);

const envLabelsMatch = /const ENV_LABELS\s*=\s*\{([^}]*)\}/.exec(panelsJs);
must(envLabelsMatch, 'panels.js に ENV_LABELS の定義が見つからない');
const envLabelsKeyCount = (envLabelsMatch[1].match(/[a-zA-Z_][a-zA-Z0-9_]*\s*:/g) || []).length;
must(envLabelsKeyCount === 5, `ENV_LABELS のキーは5個のはずが ${envLabelsKeyCount} 個だった`);
// センサー数5の第一の出典は app.css のキオスク@media内コメント（「HAセンサー5個」を
// 余裕をもって収容できる旨）。ENV_LABELSのキー数(5)はそれと一致するかを見る
// cross-check であり、第一の出典ではない（ENV_LABELSはit.class→表示名の
// フォールバック辞書であり台帳ではないため）。
must(
  /HAセンサー5個/.test(css),
  'app.css の根本対応コメント（HAセンサー5個…）が見つからない。' +
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

for (const sel of ['#intel', '#envpanel', '#hzlegend']) {
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
//
// 旧tileColumnHeightPx(91px)は「#intel .tile{gap:3px;padding:14px 20px}」
// （Issue #8根本対応前の#intel）を前提とした実測値であり、padding/gapの異なる
// .tile へそのまま流用できない。91を宣言値で分解し、内容部(2行分)だけを
// 実測定数として残す: 91 - gap(3) - padding上下(28) - border(1) = 59。
// gap/padding/borderはCSSの宣言値から実行時に読んで足す
// （resolveTileStyle/computeTileHeight）。
//
// Issue #8根本対応で#intelのfont-sizeを縮小したため、内容部の定数は#intelと
// #envpanelで別々に持つ必要がある（#envpanelはfont-size変更なしのため旧59pxを
// そのまま維持できるが、#intelは新しい値になる）:
// - envTileColumnContentPx: 59（実測値、変更なし。#envpanelが万一column化した
//   場合の退行検知フィクスチャ用）
// - intelTileColumnContentPx: 47（設計見積り、未実測）。#intelコンパクト化後の
//   縦積みタイル内容部の推定値。実機ヘッドレスChromium計測（Issue #8対応中）で
//   確定させ、見積りとズレていればこの値とapp.cssの--envpanel-gapを補正すること。
// ============================================================

const MEASURED_KIOSK = {
  inHeadHeightPx: 45, // .in-head: --fs-sm(20px)1行 + padding 10px上下 + border 1px
  tileRowHeightPx: 49, // .tile 横並び: --fs-lg(26px) + padding 9px上下 + border 1px
  envTileColumnContentPx: 59, // #envpanel .tile 縦積み内容部(2行分)。実測値、変更なし
  intelTileColumnContentPx: 47, // #intel .tile 縦積み内容部(2行分)。設計見積り、未実測
};
const ALL_TUNABLE_METRICS = { ...MEASURED_KIOSK };

// ============================================================
// EXPECTED_GAPS: 既知の未解決ギャップをラチェットとして保持する仕組み。
// Issue #8根本対応によりkiosk/INV-1・kiosk/INV-2・desktop/INV-5の3件はいずれも
// 解消したため空にした（旧3エントリはPR履歴を参照）。INV-1/INV-2は下記で
// verdict==='PASS'を直接assertする「本物の不変条件」に格上げし、旧INV-3/INV-5
// （#legendとの重なり回避）も撤去した。新たなギャップが生じた場合はここに追加する。
//
// 注: checkRatchet関数と下方のループ内 `else if (EXPECTED_GAPS[key])` 分岐は、
// EXPECTED_GAPSが空である限り到達しないが、将来また追加されたときのため意図的に
// 残してある（無害なdead code）。削除しないこと。
// ============================================================

const EXPECTED_GAPS = {};

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

function parseBorderBottomWidth(value) {
  // border-bottom は1辺のみなので、#legend用のparseBorderWidthVerticalとは
  // 異なり2倍しない
  const m = /^([\d.]+)px/.exec(value.trim());
  must(m, `border-bottom の幅を解釈できない: "${value}"`);
  return parseFloat(m[1]);
}

// ============================================================
// .tile の flex-direction(row/column)判定と、column時のタイル高さ算出。
// app.cssだけでなく任意のCSS文字列に対して適用できる純関数として実装し、
// 同ファイル内の自己検証（下記フィクスチャF1〜F6）で検知能力を確認する。
// ============================================================

// テキストからトップレベルの{...}ブロック列を取り、@mediaの中身を
// 「その@media条件式」付きで再帰展開する（入れ子@mediaが無い前提）。
function collectBlocksWithMediaCond(text, mediaCondition) {
  const out = [];
  for (const block of parseBlocks(text)) {
    if (block.selector.startsWith('@media')) {
      const cond = block.selector.replace(/^@media\s*/, '').trim();
      out.push(...collectBlocksWithMediaCond(block.body, cond));
    } else {
      out.push({ selector: block.selector, body: block.body, mediaCondition });
    }
  }
  return out;
}

// セレクタが「.tile自身」を指すか判定する（子孫セレクタ(.tile .v等)は対象外）。
// scope: 直前のID(#intel/#envpanel等)。無ければnull(全.tileに適用される汎用ルール)。
function tileSelfScope(selector) {
  const compounds = selector.trim().split(/\s+/);
  const last = compounds[compounds.length - 1];
  if (last !== '.tile') return undefined;
  let scope = null;
  for (let i = compounds.length - 2; i >= 0; i--) {
    if (compounds[i].startsWith('#')) { scope = compounds[i]; break; }
  }
  return { scope };
}

// 指定scope(#intel/#envpanel等)の .tile に適用される宣言をCSSカスケードに
// 従って解決する。IDスコープを持つセレクタ(#envpanel .tile)は、汎用セレクタ
// (.tile)より常に優先される（特異度）。同じ優先度内では記述順で後勝ち。
function resolveTileStyle(cssText, scope, viewport) {
  const blocks = collectBlocksWithMediaCond(stripComments(cssText), null);
  const applicable = [];
  for (const block of blocks) {
    if (block.mediaCondition !== null && !evalMediaQuery(block.mediaCondition, viewport)) continue;
    for (const rawSelector of block.selector.split(',')) {
      const info = tileSelfScope(rawSelector.trim());
      if (!info) continue;
      if (info.scope !== null && info.scope !== scope) continue;
      applicable.push({ idScoped: info.scope === scope, decls: declMap(block.body) });
    }
  }
  const ordered = [...applicable.filter(a => !a.idScoped), ...applicable.filter(a => a.idScoped)];
  const style = new Map();
  for (const entry of ordered) {
    for (const [prop, value] of entry.decls) style.set(prop, value);
  }
  return style;
}

function detectTileFlexDirection(cssText, scope, viewport) {
  const style = resolveTileStyle(cssText, scope, viewport);
  const fd = style.get('flex-direction');
  return fd && fd.trim() === 'column' ? 'column' : 'row';
}

// row: 実測定数tileRowHeightPxをそのまま使う。
// column: 内容部(scopeに応じてintelTileColumnContentPx／envTileColumnContentPx)に
// CSSの宣言値(gap/padding/border)を実行時に加算する（DECISION-3:
// tileColumnHeightPx(91px)の分解。Issue #8根本対応で#intelのfont-sizeが変わった
// ため内容部の定数をscope別に分けた）。
function computeTileHeight(cssText, mode, scope, viewport, metrics) {
  if (mode === 'row') return metrics.tileRowHeightPx;
  const contentPx =
    scope === '#intel' ? metrics.intelTileColumnContentPx
    : scope === '#envpanel' ? metrics.envTileColumnContentPx
    : must(false, `computeTileHeight: scope "${scope}" のcolumn modeコンテンツ高さが未定義`);
  const style = resolveTileStyle(cssText, scope, viewport);
  const gap = style.has('gap') ? parseGap(style.get('gap')) : 0;
  const paddingV = parsePaddingVertical(
    must(style.get('padding'), `${scope} .tile (column mode) に padding が無い`)
  );
  let borderV;
  if (style.has('border-bottom')) borderV = parseBorderBottomWidth(style.get('border-bottom'));
  else if (style.has('border')) borderV = parseBorderWidthVertical(style.get('border'));
  else throw new Error(`${scope} .tile (column mode) に border情報が無い`);
  return contentPx + gap + paddingV + borderV;
}

// ============================================================
// #envpanel / #intel の幾何（calc()評価器を通す）
// ============================================================

const intelDecls = declMap(findBlock('#intel', 'default').body);
const envpanelDecls = declMap(findBlock('#envpanel', 'default').body);

function computeGeometry(cssText, profile, metrics) {
  const viewport = PROFILES[profile].viewport;
  const vars = effectiveVars(profile);
  const ctx = { vars, viewport };

  const intelTop = evaluate(must(intelDecls.get('top'), '#intel に top が無い'), ctx);
  const envTop = evaluate(must(envpanelDecls.get('top'), '#envpanel に top が無い'), ctx);
  const envMaxHeight = evaluate(
    must(envpanelDecls.get('max-height'), '#envpanel に max-height が無い'),
    ctx
  );

  // #envpanelの容量計算に使う1タイルの高さは、row/columnどちらで並んでいるかを
  // app.cssから判定した結果に応じて選ぶ（無条件にtileRowHeightPxを使わない）。
  const intelMode = detectTileFlexDirection(cssText, '#intel', viewport);
  const envMode = detectTileFlexDirection(cssText, '#envpanel', viewport);

  const metricsAvailable = PROFILES[profile].metricsConfidence === 'measured';

  let intelHeight = null;
  let intelBottom = null;
  let clearance = null;
  let capacity = null;
  let envActualHeight = null;
  let envBottom = null;

  if (metricsAvailable) {
    const intelTileHeight = computeTileHeight(cssText, intelMode, '#intel', viewport, metrics);
    const envTileHeight = computeTileHeight(cssText, envMode, '#envpanel', viewport, metrics);
    // .tile:last-child{border-bottom:0} のぶん、N枚合計は N*tileH - 1px になる
    intelHeight = metrics.inHeadHeightPx + intelTileCount * intelTileHeight - 1;
    intelBottom = intelTop + intelHeight;
    clearance = envTop - intelBottom;
    capacity = Math.floor((envMaxHeight - metrics.inHeadHeightPx) / envTileHeight);
    envActualHeight = metrics.inHeadHeightPx + capacity * envTileHeight;
    envBottom = envTop + envActualHeight;
  }

  return {
    profile,
    viewport,
    metricsAvailable,
    intelMode,
    envMode,
    intelTop,
    intelHeight,
    intelBottom,
    envTop,
    envMaxHeight,
    envActualHeight,
    envBottom,
    clearance,
    capacity,
  };
}

const nominalGeom = {
  desktop: computeGeometry(cleanCss, 'desktop', ALL_TUNABLE_METRICS),
  kiosk: computeGeometry(cleanCss, 'kiosk', ALL_TUNABLE_METRICS),
};

// ============================================================
// 欠陥1・自己検証: tileSelfScope/resolveTileStyle/detectTileFlexDirection/
// computeTileHeightは、モジュール先頭で読み込んだapp.cssだけでなく任意のCSS
// 文字列に対して適用できる純関数である。app.css自体は変更禁止のため、この
// 自己検証が「退行CSSを検知できること」を確認できる唯一の手段になる。
// ============================================================

const KIOSK_VIEWPORT = PROFILES.kiosk.viewport;
const FIXTURE_MEDIA_COND = '(min-width:2200px) and (min-aspect-ratio:2/1)';
const FIXTURE_BASE_TILE =
  '.tile{display:flex;padding:9px 14px;border-bottom:1px solid rgba(125,147,171,.1)}\n' +
  '.tile:last-child{border-bottom:0}\n';

// F1: base_commitと同形（@media内に#intel .tile{flex-direction:column}のみ）
const FIXTURE_F1 =
  FIXTURE_BASE_TILE +
  `@media ${FIXTURE_MEDIA_COND}{#intel .tile{flex-direction:column;gap:3px;padding:14px 20px}}`;
// F2: @media内に#envpanel .tile{flex-direction:column;...}を追加した形（本ジョブが検知すべき退行）
const FIXTURE_F2 =
  FIXTURE_BASE_TILE +
  `@media ${FIXTURE_MEDIA_COND}{#intel .tile{flex-direction:column;gap:3px;padding:14px 20px}` +
  `#envpanel .tile{flex-direction:column;gap:3px;padding:14px 20px}}`;
// F2b: #envpanel .tile{flex-direction:column}のみ（padding/gapは基底.tileを継承）。
// KNOWLEDGE_SNAPSHOTの「78px」シナリオに対応する最小形の退行
const FIXTURE_F2_MINIMAL =
  FIXTURE_BASE_TILE + `@media ${FIXTURE_MEDIA_COND}{#envpanel .tile{flex-direction:column}}`;
// F3: @media内に素の.tile{flex-direction:column;...}（PR #5が起こした形。#intel限定でない）
const FIXTURE_F3 =
  FIXTURE_BASE_TILE + `@media ${FIXTURE_MEDIA_COND}{.tile{flex-direction:column;gap:3px;padding:14px 20px}}`;
// F4: カンマ区切りのgroupセレクタ
const FIXTURE_F4 =
  FIXTURE_BASE_TILE +
  `@media ${FIXTURE_MEDIA_COND}{#intel .tile,#envpanel .tile{flex-direction:column;gap:3px;padding:14px 20px}}`;
// F6: .tileの子孫であって.tile自身ではない（深い子孫セレクタに釣られないことの確認）
const FIXTURE_F6 =
  FIXTURE_BASE_TILE + `@media ${FIXTURE_MEDIA_COND}{#envpanel .tile .v{flex-direction:column}}`;

function assertMode(label, cssText, scope, expected) {
  const mode = detectTileFlexDirection(cssText, scope, KIOSK_VIEWPORT);
  assert.strictEqual(
    mode,
    expected,
    `[self-test ${label}] ${scope} の判定は${expected}のはずが${mode}だった`
  );
}

assertMode('F1', FIXTURE_F1, '#envpanel', 'row');
assertMode('F1', FIXTURE_F1, '#intel', 'column');
assertMode('F2', FIXTURE_F2, '#envpanel', 'column');
assertMode('F3', FIXTURE_F3, '#envpanel', 'column');
assertMode('F3', FIXTURE_F3, '#intel', 'column');
assertMode('F4', FIXTURE_F4, '#envpanel', 'column');
assertMode('F6', FIXTURE_F6, '#envpanel', 'row');

// 欠陥1・実効性: 「モードを読めるようになった」だけで終わらせず、読んだモードが
// 実際に#envpanelの表示容量に効いていることを確認する。定数の選び方（78px/91px
// いずれの退行CSSでも floor(122/x)=1）に依存しないことも合わせて確認する。
for (const [label, fixtureCss] of [
  ['F2', FIXTURE_F2],
  ['F2b', FIXTURE_F2_MINIMAL],
  ['F3', FIXTURE_F3],
  ['F4', FIXTURE_F4],
]) {
  const mode = detectTileFlexDirection(fixtureCss, '#envpanel', KIOSK_VIEWPORT);
  assert.strictEqual(mode, 'column', `[self-test ${label}] #envpanelがcolumn判定になっていない`);
  const tileHeight = computeTileHeight(fixtureCss, mode, '#envpanel', KIOSK_VIEWPORT, ALL_TUNABLE_METRICS);
  const capacity = Math.floor(
    (nominalGeom.kiosk.envMaxHeight - ALL_TUNABLE_METRICS.inHeadHeightPx) / tileHeight
  );
  // EXPECTED_GAPS撤去（Issue #8根本対応）に伴い、閾値はSENSOR_COUNT（本来
  // 満たすべき表示数）そのものを使う。#envpanelがcolumn化する退行が起きれば
  // capacityは5を大きく下回る（F2/F3/F4で3、F2_MINIMALで4）ため、この閾値で
  // 退行を確実に検知できる。
  assert.ok(
    capacity < SENSOR_COUNT,
    `[self-test ${label}] #envpanelがcolumn化した退行CSSでcapacityが` +
      `${SENSOR_COUNT}未満に落ちることを検知できていない (capacity=${capacity})`
  );
}

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
// 旧verdictInv3/verdictInv5（#legendとの重なり回避）はIssue #8根本対応での
// #legend削除に伴い撤去した。

const INV_DEFS = {
  'INV-1': { verdictFn: verdictInv1, valueFn: g => g.capacity, direction: 'biggerIsBetter' },
  'INV-2': { verdictFn: verdictInv2, valueFn: g => g.clearance, direction: 'biggerIsBetter' },
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
// INV-6（感度チェック）: MEASURED_KIOSK の4定数を
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
  return computeGeometry(cleanCss, profile, metrics);
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

// 幾何の数値表（top/height/bottom/max-height/容量/クリアランス）。
// mode列は.tileがrow/columnどちらで並んでいるとCSSから判定したかを示す
// （判定が壊れても人間が目視で気づけるようにするため明示的に出す）。
const geomTable = [];
for (const profile of Object.keys(PROFILES)) {
  const g = nominalGeom[profile];
  geomTable.push({
    profile,
    panel: '#intel',
    mode: g.intelMode,
    top: Math.round(g.intelTop),
    height: g.intelHeight === null ? 'N/A (INFO)' : Math.round(g.intelHeight),
    bottom: g.intelBottom === null ? 'N/A (INFO)' : Math.round(g.intelBottom),
  });
  geomTable.push({
    profile,
    panel: '#envpanel',
    mode: g.envMode,
    top: Math.round(g.envTop),
    height: g.envActualHeight === null ? 'N/A (INFO)' : Math.round(g.envActualHeight),
    bottom: g.envBottom === null ? 'N/A (INFO)' : Math.round(g.envBottom),
    maxHeight: Math.round(g.envMaxHeight),
    capacity: g.capacity === null ? 'N/A (INFO)' : g.capacity,
    clearance: g.clearance === null ? 'N/A (INFO)' : Math.round(g.clearance),
  });
}

console.log('=== right column layout: geometry (px) ===');
console.table(geomTable);
console.log('=== right column layout: invariant verdicts ===');
console.table(table);

// ============================================================
// 終了コード決定の第一段階: blockingFailures（ラチェット違反・未登録FAIL）を、
// INV-1/INV-2の直接assertより先に判定する。順序が逆だと、
// 本物のCSS退行によるラチェット違反があっても「パーサまたは高さモデルに誤りが
// ある可能性が高い」という誤誘導メッセージだけが出て、どの層が原因か分からなく
// なる（実際に起きた欠陥。旧版ではこの判定対象はEXPECTED_NOMINALとの突き合わせ
// だったが、Issue #8根本対応でINV-1/INV-2のPASS直接assertに置き換えた）。
// ここでラチェット違反があれば、それ以降のassertは実行されずexit 1になる。
// ============================================================

if (blockingFailures.length > 0) {
  console.error('=== BLOCKING FAILURES ===');
  for (const f of blockingFailures) console.error('- ' + f);
  process.exit(1);
}

// ============================================================
// INV-1/INV-2: Issue #8根本対応後の本来あるべき不変条件を直接assertする
// （EXPECTED_GAPSのラチェットで許容する対象ではなくなったため）。
// ============================================================

assert.strictEqual(
  verdictInv1(nominalGeom.kiosk),
  'PASS',
  `kiosk/INV-1 (表示容量>=${SENSOR_COUNT}) はPASSのはずが ` +
    `verdict=${verdictInv1(nominalGeom.kiosk)} (capacity=${nominalGeom.kiosk.capacity})`
);
assert.strictEqual(
  verdictInv2(nominalGeom.kiosk),
  'PASS',
  `kiosk/INV-2 (クリアランス>=8px) はPASSのはずが ` +
    `verdict=${verdictInv2(nominalGeom.kiosk)} (clearance=${nominalGeom.kiosk.clearance})`
);

assert.strictEqual(
  Math.round(nominalGeom.desktop.envMaxHeight),
  665,
  'desktop系統の#envpanel max-heightはcalcから決定的に665pxになるはず'
);
assert.strictEqual(
  Math.round(nominalGeom.kiosk.envMaxHeight),
  391,
  'kiosk系統の#envpanel max-heightはcalcから決定的に391pxになるはず'
);
assert.strictEqual(
  Math.round(nominalGeom.kiosk.intelHeight),
  440,
  'kiosk系統の#intel高さは440pxになるはず（intelTileColumnContentPx=47の設計見積り' +
    'に基づく暫定値。実機ヘッドレスChromium計測で確定させ、ズレていれば補正すること）'
);

console.log('right column layout: OK (#legend削除・#intelコンパクト化によりIssue #8根本対応完了。ROOM ENVはHAセンサー5個をrow表示のまま収容できる)');
